//! Memory search as memory-engine has it (`docs/architecture/MEMORY_STORE.en.md`
//! §6): BM25 over the words, cosine similarity over embeddings when the user
//! configured them, the two fused by weighted Reciprocal Rank Fusion, every
//! filter composable with either, and browsing by filters alone.
//!
//! ha's stores are small, so ranking runs over the entries a search
//! considers, after the filters, with the statistics of exactly those: a
//! global and a conversation's store rank as one corpus.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::harness::{self, Entry};
use super::knowledge::embedding;
use super::memory_filters::{Predicate, Temporal, TreeFilter, contains};
use super::memory_store;

/// memory-engine's limits: 10 results by default, 200 at most; 30
/// candidates per arm, at most 1,000.
const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 200;
const DEFAULT_CANDIDATES: usize = 30;
const MAX_CANDIDATES: usize = 1_000;
/// How long a search waits for entries to be embedded before ranking.
const EMBED_WAIT: Duration = Duration::from_secs(20);
/// Entries embedded per request.
const EMBED_BATCH: usize = 16;
/// BM25's parameters, and how much each field of an entry weighs.
const K1: f64 = 1.2;
const B: f64 = 0.75;
const TITLE_WEIGHT: f64 = 3.0;
const CONTENT_WEIGHT: f64 = 1.0;
const PATH_WEIGHT: f64 = 1.0;
const META_WEIGHT: f64 = 0.5;

/// The most other wordings one search takes, as knowledge search does.
const MAX_VARIANTS: usize = 4;

/// What a search reads: `memory` (the default), `knowledge`, or both.
///
/// # Errors
/// `sources` is not a non-empty list of those.
pub fn sources(arguments: &Value) -> Result<Vec<&'static str>, String> {
    let Some(given) = arguments.get("sources").filter(|value| !value.is_null()) else {
        return Ok(vec!["memory"]);
    };
    let refusal = || "sources is a list of memory and knowledge".to_owned();
    let names = given.as_array().ok_or_else(refusal)?;
    let mut sources = Vec::new();
    for name in names {
        let source = match name.as_str() {
            Some("memory") => "memory",
            Some("knowledge") => "knowledge",
            _ => return Err(refusal()),
        };
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    if sources.is_empty() {
        return Err(refusal());
    }
    Ok(sources)
}

/// The arguments a search takes beside `query`/`queries`.
pub const FILTERS: [&str; 5] = ["tree", "meta", "meta_predicate", "temporal", "grep"];

/// Check a search's arguments: something to search for or filter by, and
/// each filter well formed. `grep` must come with a search or another
/// filter (not `meta_predicate` alone), as in memory-engine.
///
/// # Errors
/// What is wrong with them.
pub fn check(arguments: &Value) -> Result<(), String> {
    let text = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
    };
    let searches = text("query").is_some()
        || arguments
            .get("queries")
            .and_then(Value::as_array)
            .is_some_and(|queries| !queries.is_empty())
        || text("semantic").is_some();
    let given = |key: &str| arguments.get(key).is_some_and(|value| !value.is_null());
    sources(arguments)?;
    if arguments
        .get("queries")
        .and_then(Value::as_array)
        .is_some_and(|variants| variants.len() > MAX_VARIANTS)
    {
        return Err(format!(
            "queries takes at most {MAX_VARIANTS} variants beside query"
        ));
    }
    if !searches && !FILTERS.iter().any(|key| given(key)) {
        return Err("search needs a query (or queries, or semantic), or a filter: tree, meta, meta_predicate, temporal or grep".to_owned());
    }
    if given("grep") && !searches && !["tree", "meta", "temporal"].iter().any(|key| given(key)) {
        return Err(
            "grep narrows a search: give it with a query or a tree, meta or temporal filter"
                .to_owned(),
        );
    }
    if let Some(tree) = text("tree") {
        TreeFilter::parse(tree)?;
    }
    if given("meta") && !arguments["meta"].is_object() {
        return Err(
            "meta is an object of facets to match, such as {\"type\": \"decision\"}".to_owned(),
        );
    }
    if let Some(predicate) = text("meta_predicate") {
        Predicate::parse(predicate)?;
    }
    if given("temporal") {
        Temporal::parse(&arguments["temporal"])?;
    }
    if let Some(pattern) = text("grep") {
        grep(pattern)?;
    }
    if let Some(threshold) = arguments
        .get("semantic_threshold")
        .filter(|value| !value.is_null())
    {
        let threshold = threshold
            .as_f64()
            .ok_or("semantic_threshold is a number in [0, 1]")?;
        if !(0.0..=1.0).contains(&threshold) {
            return Err("semantic_threshold is a cosine similarity in [0, 1]; it is refused, not clamped, outside".to_owned());
        }
    }
    if let Some(weights) = arguments.get("weights").filter(|value| !value.is_null()) {
        for key in ["fulltext", "semantic"] {
            if let Some(weight) = weights.get(key).filter(|value| !value.is_null()) {
                let weight = weight.as_f64().ok_or("weights are numbers in [0, 1]")?;
                if !(0.0..=1.0).contains(&weight) {
                    return Err("weights are numbers in [0, 1]".to_owned());
                }
            }
        }
    }
    check_presentation(arguments)
}

/// A search's `order_by`, `format` and `select`.
fn check_presentation(arguments: &Value) -> Result<(), String> {
    let text = |key: &str| arguments.get(key).and_then(Value::as_str);
    if let Some(order) = text("order_by")
        && !matches!(order, "asc" | "desc")
    {
        return Err("order_by is asc or desc".to_owned());
    }
    if let Some(format) = text("format")
        && !matches!(format, "json" | "compact" | "yaml")
    {
        return Err("format is json, compact or yaml".to_owned());
    }
    if let Some(select) = arguments.get("select").filter(|value| !value.is_null()) {
        let fields = select.as_array().ok_or("select is a list of fields")?;
        if fields.is_empty() {
            return Err("select names at least one field".to_owned());
        }
        let mut slices = 0;
        for field in fields {
            let field = field.as_str().ok_or("select is a list of field names")?;
            if field.starts_with("content:") {
                slice(field)?;
                slices += 1;
            } else if !field.starts_with("meta.") && !SELECTABLE.contains(&field) {
                return Err(format!(
                    "select has no field {field:?}; fields are {}, meta.<key> and content:N, content:M:N or content:M:",
                    SELECTABLE.join(", ")
                ));
            }
        }
        if slices > 1 {
            return Err("select takes one content slice".to_owned());
        }
    }
    Ok(())
}

/// The fields `select` may name.
const SELECTABLE: [&str; 17] = [
    "id",
    "scope",
    "kind",
    "name",
    "title",
    "tree",
    "path",
    "meta",
    "temporal",
    "version",
    "version_hash",
    "content",
    "content_length",
    "score",
    "created_at",
    "updated_at",
    "pinned",
];

fn grep(pattern: &str) -> Result<regex::Regex, String> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(1 << 20)
        .build()
        .map_err(|error| format!("grep is not a regular expression: {error}"))
}

/// Where each entry is stored, for its id and its vector.
pub struct Placed<'a> {
    pub entry: &'a Entry,
    pub dir: &'a Path,
    /// The entry as stored, for `select`.
    pub doc: &'a Value,
}

/// Run a search over `placed`, the entries of the scopes and kinds asked.
/// `embedding_root` is the knowledge library whose embedding configuration
/// memory uses; `view` renders a hit as a search result.
///
/// # Errors
/// The arguments are malformed, or a store cannot be read.
#[allow(
    clippy::too_many_lines,
    reason = "one search: filter, rank each arm, fuse, present"
)]
pub fn search(
    placed: &[Placed<'_>],
    embedding_root: &Path,
    arguments: &Value,
    view: &dyn Fn(&Entry) -> Value,
) -> Result<Value, String> {
    check(arguments)?;
    let text = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    let tree = text("tree").map(TreeFilter::parse).transpose()?;
    let meta = arguments.get("meta").filter(|meta| meta.is_object());
    let predicate = text("meta_predicate").map(Predicate::parse).transpose()?;
    let temporal = arguments
        .get("temporal")
        .filter(|value| !value.is_null())
        .map(Temporal::parse)
        .transpose()?;
    let pattern = text("grep").map(grep).transpose()?;
    let considered = placed
        .iter()
        .filter(|placed| {
            let entry = placed.entry;
            let facets = Value::Object(entry.meta.clone());
            tree.as_ref().is_none_or(|tree| tree.matches(&entry.tree))
                && meta.is_none_or(|meta| contains(&facets, meta))
                && predicate
                    .as_ref()
                    .is_none_or(|predicate| predicate.matches(&facets))
                && temporal.as_ref().is_none_or(|temporal| {
                    temporal.matches(
                        memory_store::temporal_bounds(
                            placed.doc.get("temporal").unwrap_or(&Value::Null),
                        )
                        .ok()
                        .flatten()
                        .as_ref(),
                    )
                })
                && pattern.as_ref().is_none_or(|pattern| {
                    pattern.is_match(entry.content.as_deref().unwrap_or_default())
                })
        })
        .collect::<Vec<_>>();
    let limit = number(arguments, "limit", DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let candidates =
        number(arguments, "candidate_limit", DEFAULT_CANDIDATES).clamp(limit, MAX_CANDIDATES);
    let queries = queries(arguments);
    let semantic_text = text("semantic").map(str::to_owned);
    let weights = arguments.get("weights");
    let weight = |key: &str| {
        weights
            .and_then(|weights| weights.get(key))
            .and_then(Value::as_f64)
            .unwrap_or(1.0)
    };
    // The words' arm: every variant ranked by BM25, the variants fused.
    let rank_words = |queries: &[String]| {
        let rankings = queries
            .iter()
            .map(|query| bm25(&considered, query))
            .collect::<Vec<_>>();
        let ranked = if let [only] = rankings.as_slice() {
            only.clone()
        } else {
            let keyed = rankings
                .iter()
                .map(|ranking| {
                    ranking
                        .iter()
                        .map(|(index, score)| (format!("{index:08}"), *score))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let arms = keyed
                .iter()
                .map(|ranking| (ranking.as_slice(), 1.0))
                .collect::<Vec<_>>();
            harness_types::search::weighted_rrf(&arms)
                .into_iter()
                .filter_map(|(key, score)| key.parse::<usize>().ok().map(|index| (index, score)))
                .collect()
        };
        ranked.into_iter().take(candidates).collect::<Vec<_>>()
    };
    let fulltext = (!queries.is_empty()).then(|| rank_words(&queries));
    // The meaning arm: with embeddings configured, the semantic text - or the
    // query and its variants when none is given - against every entry's vector.
    let mut note = None;
    let meaning_texts = match (&semantic_text, queries.is_empty()) {
        (Some(text), _) => vec![text.clone()],
        (None, false) => queries.clone(),
        (None, true) => Vec::new(),
    };
    let threshold = arguments.get("semantic_threshold").and_then(Value::as_f64);
    let semantic = if meaning_texts.is_empty() {
        None
    } else {
        match meaning(&considered, embedding_root, &meaning_texts) {
            Ok(Some(similar)) => Some(
                similar
                    .into_iter()
                    .filter(|(_, score)| threshold.is_none_or(|threshold| *score >= threshold))
                    .take(candidates)
                    .collect::<Vec<_>>(),
            ),
            Ok(None) => {
                if semantic_text.is_some() {
                    note = Some("semantic search is not configured (/knowledge semantic --global), so words alone ranked this".to_owned());
                }
                None
            }
            Err(error) => {
                note = Some(format!(
                    "semantic search failed ({error}), so words alone ranked this"
                ));
                None
            }
        }
    };
    // Meaning asked for but not to be had: its words still search.
    let fulltext = match (fulltext, &semantic, &semantic_text) {
        (None, None, Some(text)) => Some(rank_words(std::slice::from_ref(text))),
        (fulltext, _, _) => fulltext,
    };
    let (mode, ranked): (&str, Vec<(usize, f64)>) = match (fulltext, semantic) {
        (Some(words), Some(similar)) if semantic_text.is_some() || !similar.is_empty() => {
            let key = |ranking: &[(usize, f64)]| {
                ranking
                    .iter()
                    .map(|(index, score)| (format!("{index:08}"), *score))
                    .collect::<Vec<_>>()
            };
            let (words, similar) = (key(&words), key(&similar));
            let fused = harness_types::search::weighted_rrf(&[
                (&words, weight("fulltext")),
                (&similar, weight("semantic")),
            ]);
            (
                "hybrid",
                fused
                    .into_iter()
                    .filter_map(|(key, score)| key.parse().ok().map(|index| (index, score)))
                    .collect(),
            )
        }
        (Some(words), _) => ("fulltext", words),
        (None, Some(similar)) => ("semantic", similar),
        (None, None) => {
            // Filters alone browse, newest first unless order_by says.
            let ascending = text("order_by") == Some("asc");
            let mut order = (0..considered.len()).collect::<Vec<_>>();
            let created = |index: &usize| {
                considered[*index].doc["created_at"]
                    .as_str()
                    .and_then(|time| memory_store::instant(time).ok())
                    .unwrap_or_default()
            };
            order.sort_by_key(created);
            if !ascending {
                order.reverse();
            }
            (
                "filter",
                order.into_iter().map(|index| (index, -1.0)).collect(),
            )
        }
    };
    let total = ranked.len();
    let select = arguments
        .get("select")
        .and_then(Value::as_array)
        .map(|fields| fields.iter().filter_map(Value::as_str).collect::<Vec<_>>());
    let hits = ranked
        .iter()
        .take(limit)
        .map(|(index, score)| {
            let placed = considered[*index];
            if let Some(fields) = &select {
                project(placed, *score, fields)
            } else {
                let mut hit = view(placed.entry);
                hit["score"] = json!(rounded(*score, mode));
                hit
            }
        })
        .collect::<Vec<_>>();
    let mut result = json!({"hits": hits, "total": total, "limit": limit, "mode": mode, "searched": considered.len()});
    if hits_empty(&result) {
        result["note"] = json!(
            "No memory matched. Memory holds only what was written before; if the answer matters and you have not yet, search once more with variants in queries (synonyms, a translation) or fewer filters; otherwise say it is not remembered."
        );
    }
    if let Some(note) = note {
        result["semantic"] = json!(note);
    }
    Ok(match text("format") {
        Some("yaml") => json!({"text": yaml(&result)}),
        _ => result,
    })
}

fn hits_empty(result: &Value) -> bool {
    result["hits"].as_array().is_none_or(Vec::is_empty)
}

/// A score as a result shows it: BM25 and cosine to four places, RRF's small
/// fused values to six.
fn rounded(score: f64, mode: &str) -> f64 {
    let places = if mode == "hybrid" { 1e6 } else { 1e4 };
    (score * places).round() / places
}

fn number(arguments: &Value, key: &str, default: usize) -> usize {
    arguments
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default)
}

/// A search's query and its variants, without repeats.
fn queries(arguments: &Value) -> Vec<String> {
    let mut queries: Vec<String> = Vec::new();
    let candidates = arguments
        .get("query")
        .and_then(Value::as_str)
        .into_iter()
        .chain(
            arguments
                .get("queries")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        );
    for query in candidates {
        let query = query.trim();
        if !query.is_empty()
            && !queries
                .iter()
                .any(|seen| seen.to_lowercase() == query.to_lowercase())
        {
            queries.push(query.to_owned());
        }
    }
    queries
}

/// The words a field holds, for BM25. A query without accents reads the
/// entries without theirs, so `may in` finds `máy in`; one with accents
/// reads them as written.
fn words(text: &str, fold: bool) -> Vec<String> {
    if fold {
        harness::tokens(&super::knowledge::store::fold(text))
    } else {
        harness::tokens(text)
    }
}

/// BM25 over the considered entries, best first, only those holding at least
/// one of the query's words (a word matches the words it begins: `deploy`
/// finds `deploys`). Fields weigh by what they say of the entry: its title
/// most, then its content and where it sits, then its facets.
#[allow(clippy::cast_precision_loss, reason = "counts are small")]
fn bm25(considered: &[&Placed<'_>], query: &str) -> Vec<(usize, f64)> {
    let fold = super::knowledge::store::fold(query) == query;
    let terms = harness::query_terms(&if fold {
        super::knowledge::store::fold(query)
    } else {
        query.to_owned()
    });
    if terms.is_empty() || considered.is_empty() {
        return Vec::new();
    }
    let fields = considered
        .iter()
        .map(|placed| {
            let entry = placed.entry;
            let facets = entry
                .meta
                .iter()
                .filter(|(key, _)| key.as_str() != "pinned")
                .map(|(key, value)| {
                    format!(
                        "{key} {}",
                        value
                            .as_str()
                            .map_or_else(|| value.to_string(), str::to_owned)
                    )
                })
                .collect::<Vec<_>>()
                .join(" ");
            [
                (
                    words(entry.title.as_deref().unwrap_or_default(), fold),
                    TITLE_WEIGHT,
                ),
                (
                    words(entry.content.as_deref().unwrap_or_default(), fold),
                    CONTENT_WEIGHT,
                ),
                (
                    words(
                        &format!("{} {}", entry.tree.replace('/', " "), entry.id),
                        fold,
                    ),
                    PATH_WEIGHT,
                ),
                (words(&facets, fold), META_WEIGHT),
            ]
        })
        .collect::<Vec<_>>();
    let lengths = fields
        .iter()
        .map(|fields| {
            fields
                .iter()
                .map(|(words, weight)| words.len() as f64 * weight)
                .sum::<f64>()
        })
        .collect::<Vec<_>>();
    let average = (lengths.iter().sum::<f64>() / lengths.len() as f64).max(1.0);
    let frequency = |fields: &[(Vec<String>, f64); 4], term: &str| {
        fields
            .iter()
            .map(|(words, weight)| {
                words.iter().filter(|word| word.starts_with(term)).count() as f64 * weight
            })
            .sum::<f64>()
    };
    let documents = considered.len() as u64;
    let idf = terms
        .iter()
        .map(|term| {
            let holding = fields
                .iter()
                .filter(|fields| frequency(fields, term) > 0.0)
                .count() as u64;
            harness_types::search::bm25_idf(documents, holding)
        })
        .collect::<Vec<_>>();
    let mut ranked = fields
        .iter()
        .enumerate()
        .filter_map(|(index, fields)| {
            let mut score = 0.0;
            let mut matched = false;
            for (term, idf) in terms.iter().zip(&idf) {
                let tf = frequency(fields, term);
                if tf > 0.0 {
                    matched = true;
                    score += idf * tf * (K1 + 1.0)
                        / (tf + K1 * (1.0 - B + B * lengths[index] / average));
                }
            }
            (matched && score > 0.0).then_some((index, score))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}

/// The entries' cosine similarity to the nearest of `texts`, best first, or
/// `None` when embeddings are not configured. Entries not yet embedded are
/// embedded first, for up to [`EMBED_WAIT`].
fn meaning(
    considered: &[&Placed<'_>],
    embedding_root: &Path,
    texts: &[String],
) -> Result<Option<Vec<(usize, f64)>>, String> {
    let config = embedding::load_config(embedding_root)?;
    if !config.enabled {
        return Ok(None);
    }
    let namespace = embedding::namespace_of(&config)?;
    let mut dirs = considered
        .iter()
        .map(|placed| placed.dir.to_path_buf())
        .collect::<Vec<_>>();
    dirs.sort();
    dirs.dedup();
    embed_pending(&dirs, &config, &namespace, Instant::now() + EMBED_WAIT);
    let queries = embedding::embed_sync(&config, texts)?;
    let mut vectors = BTreeMap::new();
    let mut ids = BTreeMap::new();
    for dir in &dirs {
        vectors.insert(dir.clone(), memory_store::vectors(dir, &namespace)?);
        ids.insert(dir.clone(), memory_store::ids(dir)?);
    }
    let mut similar = considered
        .iter()
        .enumerate()
        .filter_map(|(index, placed)| {
            let id = ids[placed.dir].get(&(placed.entry.kind.clone(), placed.entry.id.clone()))?;
            let vector = vectors[placed.dir].get(id)?;
            let best = queries
                .iter()
                .filter_map(|query| embedding::similarity(query, vector).ok())
                .fold(f64::NEG_INFINITY, f64::max);
            best.is_finite().then_some((index, best))
        })
        .collect::<Vec<_>>();
    similar.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(Some(similar))
}

/// Embed the entries of `dirs` that have no vector for their content, until
/// `deadline`: memory-engine's queue, drained by whoever needs it. A failure
/// is recorded against the entries it was for; a rate limit waits without
/// spending an attempt.
pub fn embed_pending(
    dirs: &[PathBuf],
    config: &embedding::EmbeddingConfig,
    namespace: &str,
    deadline: Instant,
) {
    for dir in dirs {
        while Instant::now() < deadline {
            let Ok(batch) = memory_store::unembedded(dir, namespace, EMBED_BATCH) else {
                break;
            };
            if batch.is_empty() {
                break;
            }
            let texts = batch
                .iter()
                .map(|entry| entry.text.clone())
                .collect::<Vec<_>>();
            match embedding::embed_sync(config, &texts) {
                Ok(vectors) if vectors.len() == batch.len() => {
                    let made = batch.into_iter().zip(vectors).collect::<Vec<_>>();
                    if memory_store::save_vectors(dir, namespace, &made).is_err() {
                        break;
                    }
                }
                Ok(_) => {
                    let _ = memory_store::embedding_failed(
                        dir,
                        &batch,
                        "the provider returned the wrong number of vectors",
                        false,
                    );
                    break;
                }
                Err(error) => {
                    let _ = memory_store::embedding_failed(
                        dir,
                        &batch,
                        &error,
                        error.contains("HTTP 429"),
                    );
                    harness_types::diag::warn("memory", "embedding_failed")
                        .field("error", error)
                        .emit();
                    break;
                }
            }
        }
    }
}

/// Embed what `dirs` hold in the background, when embeddings are configured:
/// a write does not wait for its vector.
pub fn embed_soon(dirs: Vec<PathBuf>, embedding_root: &Path) {
    let Ok(config) = embedding::load_config(embedding_root) else {
        return;
    };
    if !config.enabled {
        return;
    }
    let Ok(namespace) = embedding::namespace_of(&config) else {
        return;
    };
    std::thread::spawn(move || {
        embed_pending(
            &dirs,
            &config,
            &namespace,
            Instant::now() + Duration::from_mins(1),
        );
    });
}

/// `content:N`, `content:M:N` or `content:M:` as a character range.
fn slice(field: &str) -> Result<(usize, Option<usize>), String> {
    let bad = || format!("{field:?} is content:N, content:M:N or content:M:");
    let parts = field
        .trim_start_matches("content:")
        .split(':')
        .collect::<Vec<_>>();
    let parse = |text: &str| text.parse::<usize>().map_err(|_| bad());
    match parts.as_slice() {
        [end] => Ok((0, Some(parse(end)?))),
        [start, ""] => Ok((parse(start)?, None)),
        [start, end] => {
            let (start, end) = (parse(start)?, parse(end)?);
            if end < start {
                return Err(bad());
            }
            Ok((start, Some(end)))
        }
        _ => Err(bad()),
    }
}

/// A hit with only the fields `select` names, read from the entry as stored.
fn project(placed: &Placed<'_>, score: f64, fields: &[&str]) -> Value {
    let entry = placed.entry;
    let doc = placed.doc;
    let content = entry.content.as_deref().unwrap_or_default();
    let mut out = serde_json::Map::new();
    let root = if entry.scope == "local" {
        "/local"
    } else {
        memory_store::root_of(placed.dir)
    };
    for field in fields {
        if let Some(key) = field.strip_prefix("meta.") {
            out.insert(
                (*field).to_owned(),
                entry.meta.get(key).cloned().unwrap_or(Value::Null),
            );
            continue;
        }
        if field.starts_with("content:") {
            if let Ok((start, end)) = slice(field) {
                let taken = content
                    .chars()
                    .skip(start)
                    .take(end.map_or(usize::MAX, |end| end - start))
                    .collect::<String>();
                out.insert("content".to_owned(), json!(taken));
                out.insert("content_length".to_owned(), json!(content.chars().count()));
            }
            continue;
        }
        let value = match *field {
            "id" | "name" => json!(entry.id),
            "scope" => json!(entry.scope),
            "kind" => json!(entry.kind),
            "title" => json!(entry.title),
            "tree" => json!(entry.tree),
            "path" => json!(entry.path),
            "meta" => Value::Object(entry.meta.clone()),
            "temporal" => entry.temporal.clone().unwrap_or(Value::Null),
            "version" => json!(entry.version),
            "version_hash" => json!(memory_store::version_hash(
                root,
                &entry.kind,
                &entry.id,
                doc
            )),
            "content" => json!(content),
            "content_length" => json!(content.chars().count()),
            "score" => json!(score),
            "created_at" => doc.get("created_at").cloned().unwrap_or(Value::Null),
            "updated_at" => doc.get("updated_at").cloned().unwrap_or(Value::Null),
            "pinned" => json!(entry.pinned),
            _ => continue,
        };
        out.insert((*field).to_owned(), value);
    }
    Value::Object(out)
}

/// A JSON value as YAML: block mappings and sequences, strings quoted as JSON
/// quotes them whenever YAML could read them as something else.
#[must_use]
pub fn yaml(value: &Value) -> String {
    let mut out = String::new();
    write_yaml(value, 0, &mut out);
    out
}

fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => {
            let plain = !text.is_empty()
                && text.trim() == text
                && !text.contains([
                    '\n', ':', '#', '"', '\'', '{', '}', '[', ']', ',', '&', '*', '!', '|', '>',
                    '%', '@', '`',
                ])
                && !matches!(
                    text.to_lowercase().as_str(),
                    "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~"
                )
                && text.parse::<f64>().is_err()
                && !text.starts_with(['-', '?']);
            if plain {
                text.clone()
            } else {
                serde_json::to_string(text).unwrap_or_default()
            }
        }
        other => other.to_string(),
    }
}

fn write_yaml(value: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match value {
        Value::Object(fields) if !fields.is_empty() => {
            for (key, value) in fields {
                let key = scalar(&json!(key));
                match value {
                    Value::Object(inner) if !inner.is_empty() => {
                        let _ = writeln!(out, "{pad}{key}:");
                        write_yaml(value, indent + 2, out);
                    }
                    Value::Array(items) if !items.is_empty() => {
                        let _ = writeln!(out, "{pad}{key}:");
                        write_yaml(value, indent, out);
                    }
                    other => {
                        let _ = writeln!(out, "{pad}{key}: {}", inline(other));
                    }
                }
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for item in items {
                match item {
                    Value::Object(fields) if !fields.is_empty() => {
                        let mut nested = String::new();
                        write_yaml(item, indent + 2, &mut nested);
                        let nested = nested.trim_start();
                        let _ = write!(out, "{pad}- {nested}");
                    }
                    Value::Array(inner) if !inner.is_empty() => {
                        let _ = writeln!(out, "{pad}-");
                        write_yaml(item, indent + 2, out);
                    }
                    other => {
                        let _ = writeln!(out, "{pad}- {}", inline(other));
                    }
                }
            }
        }
        other => {
            let _ = writeln!(out, "{pad}{}", inline(other));
        }
    }
}

fn inline(value: &Value) -> String {
    match value {
        Value::Object(_) => "{}".to_owned(),
        Value::Array(_) => "[]".to_owned(),
        other => scalar(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_quotes_what_would_read_as_something_else() {
        let text = yaml(
            &json!({"hits": [{"id": "a", "score": 1.5, "meta": {"type": "decision"}}, {"id": "true", "content": "x: y"}], "total": 2, "empty": []}),
        );
        assert_eq!(
            text,
            "empty: []\nhits:\n- id: a\n  meta:\n    type: decision\n  score: 1.5\n- content: \"x: y\"\n  id: \"true\"\ntotal: 2\n"
        );
    }

    #[test]
    fn content_slices() {
        assert_eq!(slice("content:5").expect("n"), (0, Some(5)));
        assert_eq!(slice("content:2:5").expect("m:n"), (2, Some(5)));
        assert_eq!(slice("content:2:").expect("m:"), (2, None));
        assert!(slice("content:5:2").is_err());
    }
}
