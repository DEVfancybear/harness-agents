//! Memory search as memory-engine has it (`docs/architecture/MEMORY_STORE.en.md`
//! §6): BM25 over the words, the best matches reordered by how recently and
//! how much each entry was used (weighted Reciprocal Rank Fusion), every
//! filter composable with it, and browsing by filters alone. Words, not
//! meaning: a miss is searched again with other words, which the model picks.
//!
//! ha's stores are small, so ranking runs over the entries a search
//! considers, after the filters, with the statistics of exactly those: a
//! global and a conversation's store rank as one corpus.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Value, json};

use super::harness::{self, Entry};
use super::memory_filters::{Predicate, Temporal, TreeFilter, contains};
use super::memory_store;

/// memory-engine's limits: 10 results by default, 200 at most; 30
/// candidates per arm, at most 1,000.
const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 200;
const DEFAULT_CANDIDATES: usize = 30;
const MAX_CANDIDATES: usize = 1_000;
/// What recency and use weigh beside the words (weight 1) when they reorder
/// the best word matches: lighter, so they settle what the words leave close.
pub const RECENCY_WEIGHT: f64 = 0.3;
pub const USAGE_WEIGHT: f64 = 0.3;
/// How fast reads stop counting: their weight halves every 30 days after the
/// last one.
const USAGE_HALF_LIFE_DAYS: f64 = 30.0;
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
            .is_some_and(|queries| !queries.is_empty());
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
    match arguments.get("mode").filter(|value| !value.is_null()) {
        None => {}
        Some(mode) if mode == "rank" => {}
        Some(mode) if mode == "readout" => {
            if !searches {
                return Err("a readout reads for a query: give query or queries".to_owned());
            }
            if sources(arguments)?.contains(&"knowledge") {
                return Err(
                    "a readout reads memory entries; search knowledge with mode rank".to_owned(),
                );
            }
        }
        Some(_) => return Err("mode is rank or readout".to_owned()),
    }
    if let Some(slots) = arguments.get("slots").filter(|value| !value.is_null()) {
        let slots = slots.as_u64().ok_or("slots is a whole number")?;
        if !(1..=MAX_SLOTS as u64).contains(&slots) {
            return Err(format!("slots is 1 to {MAX_SLOTS}"));
        }
    }
    if !searches && !FILTERS.iter().any(|key| given(key)) {
        return Err("search needs a query (or queries), or a filter: tree, meta, meta_predicate, temporal or grep".to_owned());
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
const SELECTABLE: [&str; 18] = [
    "id",
    "memory_id",
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

/// The fields a call's `select` names, when it names any.
#[must_use]
pub fn selected(arguments: &Value) -> Option<Vec<String>> {
    arguments
        .get("select")
        .and_then(Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
}

/// Only the fields `select` names of a result already made (a read, a
/// version in history): `meta.<key>` under `meta`, and a content slice with
/// the whole content's length. A field the result lacks is left out.
#[must_use]
pub fn select_fields(value: &Value, fields: &[String]) -> Value {
    let mut out = serde_json::Map::new();
    let content = value["content"].as_str().unwrap_or_default();
    for field in fields {
        if let Some(key) = field.strip_prefix("meta.") {
            if let Some(found) = value["meta"].get(key) {
                if !out.get("meta").is_some_and(Value::is_object) {
                    out.insert("meta".to_owned(), json!({}));
                }
                out["meta"][key] = found.clone();
            }
            continue;
        }
        if field.starts_with("content:") {
            if let Ok((start, end)) = slice(field) {
                out.insert(
                    "content".to_owned(),
                    json!(content_slice(content, start, end)),
                );
                out.insert("content_length".to_owned(), json!(content.chars().count()));
            }
            continue;
        }
        if field == "content_length" {
            out.insert(field.clone(), json!(content.chars().count()));
        } else if let Some(found) = value.get(field) {
            out.insert(field.clone(), found.clone());
        }
    }
    Value::Object(out)
}

/// A result as JSON, or as YAML text when `format` asks for it.
#[must_use]
pub fn formatted(value: Value, arguments: &Value) -> Value {
    if arguments.get("format").and_then(Value::as_str) == Some("yaml") {
        json!({"text": yaml(&value)})
    } else {
        value
    }
}

/// A single result (a read) with `select` and `format` applied.
#[must_use]
pub fn present(value: Value, arguments: &Value) -> Value {
    let value = match selected(arguments) {
        Some(fields) => select_fields(&value, &fields),
        None => value,
    };
    formatted(value, arguments)
}

fn content_slice(content: &str, start: usize, end: Option<usize>) -> String {
    content
        .chars()
        .skip(start)
        .take(end.map_or(usize::MAX, |end| end - start))
        .collect()
}

/// `grep` as a case-insensitive regular expression.
///
/// # Errors
/// It is not one.
pub fn grep(pattern: &str) -> Result<regex::Regex, String> {
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

/// Run a search over `placed`, the entries of the scopes and kinds asked;
/// `view` renders a hit as a search result. The hits it returns are counted
/// as a use of each ([`memory_store::Use::Hit`]).
///
/// # Errors
/// The arguments are malformed, or a store cannot be read.
#[allow(
    clippy::too_many_lines,
    reason = "one search: filter, rank the words, reorder, present"
)]
pub fn search(
    placed: &[Placed<'_>],
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
    let considered = filtered(placed, arguments)?;
    let limit = number(arguments, "limit", DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let candidates =
        number(arguments, "candidate_limit", DEFAULT_CANDIDATES).clamp(limit, MAX_CANDIDATES);
    let queries = queries(arguments);
    let (mode, ranked): (&str, Vec<(usize, f64)>) = if queries.is_empty() {
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
    } else {
        // Every variant ranked by BM25, the variants fused, then the best
        // reordered by recency and use.
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
        let entries = considered
            .iter()
            .map(|placed| placed.entry)
            .collect::<Vec<_>>();
        let ranked = ranked.into_iter().take(candidates).collect::<Vec<_>>();
        (
            "fulltext",
            reranked(
                &entries,
                ranked,
                limit.saturating_mul(2),
                chrono::Utc::now().timestamp(),
            ),
        )
    };
    let total = ranked.len();
    let select = arguments
        .get("select")
        .and_then(Value::as_array)
        .map(|fields| fields.iter().filter_map(Value::as_str).collect::<Vec<_>>());
    let shown = ranked.iter().take(limit).collect::<Vec<_>>();
    record_hits(
        &shown
            .iter()
            .map(|(index, _)| considered[*index])
            .collect::<Vec<_>>(),
    );
    let hits = shown
        .iter()
        .map(|(index, score)| {
            let placed = considered[*index];
            if let Some(fields) = &select {
                project(placed, *score, fields)
            } else {
                let mut hit = view(placed.entry);
                hit["score"] = json!(rounded(*score));
                hit
            }
        })
        .collect::<Vec<_>>();
    let mut result = json!({"hits": hits, "total": total, "limit": limit, "mode": mode, "searched": considered.len()});
    if hits_empty(&result) {
        result["note"] = json!(miss_note(&queries));
    }
    Ok(match text("format") {
        Some("yaml") => json!({"text": yaml(&result)}),
        _ => result,
    })
}

/// What an empty search says: what was searched, that search matches words
/// and not meaning, and to search again with other words before saying it
/// is not remembered.
#[must_use]
pub fn miss_note(queries: &[String]) -> String {
    let searched = if queries.is_empty() {
        "these filters".to_owned()
    } else {
        queries
            .iter()
            .map(|query| format!("{query:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "No memory matched {searched}. Search matches words (stems, prefixes, accents folded), not meaning: search again with other words - a synonym, the other language the project uses, a shorter or broader term, a name or path from the code - or fewer filters, before saying it is not remembered."
    )
}

/// Count the entries a search returned as a hit of each, in its store.
pub fn record_hits(placed: &[&Placed<'_>]) {
    let mut by_dir = BTreeMap::<&Path, Vec<String>>::new();
    for placed in placed {
        if let Some(memory) = &placed.entry.memory {
            by_dir.entry(placed.dir).or_default().push(memory.clone());
        }
    }
    for (dir, memories) in by_dir {
        if let Err(error) = memory_store::record_use(dir, memory_store::Use::Hit, &memories) {
            harness_types::diag::debug("memory", "usage_not_recorded")
                .field("dir", dir.display().to_string())
                .field("error", error)
                .emit();
        }
    }
}

/// Seconds since the epoch of a time as ha writes one (`@<seconds>` or
/// RFC 3339).
#[must_use]
pub fn epoch(time: &str) -> Option<i64> {
    match time.strip_prefix('@') {
        Some(seconds) => seconds.parse().ok(),
        None => chrono::DateTime::parse_from_rfc3339(time)
            .ok()
            .map(|time| time.timestamp()),
    }
}

/// When an entry was last written or read: its recency.
#[must_use]
pub fn last_touched(entry: &Entry) -> Option<i64> {
    let written = entry.updated_at.as_deref().and_then(epoch);
    let read = entry.usage.last_read_at.as_deref().and_then(epoch);
    written.max(read)
}

/// How much an entry is used: its reads, weighing half as much for every
/// [`USAGE_HALF_LIFE_DAYS`] since the last. Search hits and digest showings
/// are not counted: they say what was put in front of the model, not what it
/// went on to use, and ranking by them would feed itself.
#[must_use]
#[allow(clippy::cast_precision_loss, reason = "counts and ages are small")]
pub fn usage_score(entry: &Entry, now: i64) -> f64 {
    if entry.usage.reads <= 0 {
        return 0.0;
    }
    let age_days = entry
        .usage
        .last_read_at
        .as_deref()
        .and_then(epoch)
        .map_or(f64::INFINITY, |read| (now - read).max(0) as f64 / 86_400.0);
    entry.usage.reads as f64 * 0.5_f64.powf(age_days / USAGE_HALF_LIFE_DAYS)
}

/// `ranked` - indexes into `entries` with their scores, best first - with
/// its first `pool` fused by weighted RRF with two lighter arms: recency
/// ([`last_touched`], [`RECENCY_WEIGHT`]) and use ([`usage_score`],
/// [`USAGE_WEIGHT`]); an entry with no time or no reads is left out of that
/// arm. The rest keep their order after the pool, so an entry the words
/// barely reached never passes the best of them. Each keeps its own score.
/// Ties go to the earlier place in `ranked`.
#[must_use]
pub fn reranked(
    entries: &[&Entry],
    ranked: Vec<(usize, f64)>,
    pool: usize,
    now: i64,
) -> Vec<(usize, f64)> {
    let pool = pool.min(ranked.len());
    if pool < 2 {
        return ranked;
    }
    let mut rest = ranked;
    let head = rest.drain(..pool).collect::<Vec<_>>();
    // Keyed by place, so a tie keeps the words' order.
    let key = |place: usize| format!("{place:08}");
    let by_words = (0..head.len())
        .map(|place| (key(place), 1.0))
        .collect::<Vec<_>>();
    let mut recent = (0..head.len())
        .filter_map(|place| last_touched(entries[head[place].0]).map(|time| (place, time)))
        .collect::<Vec<_>>();
    recent.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let by_recency = recent
        .into_iter()
        .map(|(place, _)| (key(place), 1.0))
        .collect::<Vec<_>>();
    let mut used = (0..head.len())
        .map(|place| (place, usage_score(entries[head[place].0], now)))
        .filter(|(_, score)| *score > 0.0)
        .collect::<Vec<_>>();
    used.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let by_use = used
        .into_iter()
        .map(|(place, _)| (key(place), 1.0))
        .collect::<Vec<_>>();
    let mut fused = harness_types::search::weighted_rrf(&[
        (&by_words, 1.0),
        (&by_recency, RECENCY_WEIGHT),
        (&by_use, USAGE_WEIGHT),
    ])
    .into_iter()
    .filter_map(|(key, _)| key.parse::<usize>().ok())
    .map(|place| head[place])
    .collect::<Vec<_>>();
    fused.append(&mut rest);
    fused
}

/// The entries every filter a search gives - `tree`, `meta`,
/// `meta_predicate`, `temporal`, `grep` - lets through.
///
/// # Errors
/// A filter is malformed.
pub fn filtered<'a>(
    placed: &'a [Placed<'a>],
    arguments: &Value,
) -> Result<Vec<&'a Placed<'a>>, String> {
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
    Ok(placed
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
        .collect())
}

fn hits_empty(result: &Value) -> bool {
    result["hits"].as_array().is_none_or(Vec::is_empty)
}

/// A score as a result shows it: to four places.
fn rounded(score: f64) -> f64 {
    (score * 1e4).round() / 1e4
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

/// A field's words as BM25 matches them: each distinct word once, sorted,
/// with how often it occurs, and the words of each English stem. A term's
/// frequency is then a binary search for the words it begins and a lookup of
/// its stem's words, not a pass over every word of every field.
struct Field {
    /// Distinct words in byte order, each with its count: the words a term
    /// begins sit together, from where the term would sort.
    words: Vec<(String, f64)>,
    /// Indexes into `words` of the words of each stem.
    stems: std::collections::HashMap<String, Vec<usize>>,
    /// How many words the field holds, repeats counted: its length.
    total: usize,
    weight: f64,
}

impl Field {
    fn new(words: Vec<String>, weight: f64, stemmer: &rust_stemmers::Stemmer) -> Self {
        let total = words.len();
        let mut counts = BTreeMap::<String, f64>::new();
        for word in words {
            *counts.entry(word).or_default() += 1.0;
        }
        let words = counts.into_iter().collect::<Vec<_>>();
        let mut stems = std::collections::HashMap::<String, Vec<usize>>::new();
        for (index, (word, _)) in words.iter().enumerate() {
            stems
                .entry(stemmer.stem(word).into_owned())
                .or_default()
                .push(index);
        }
        Self {
            words,
            stems,
            total,
            weight,
        }
    }

    /// How often the field holds a term: a word it begins (`deploy` finds
    /// `deploys`), or a word of the same stem (`deploying` finds `deployed`),
    /// each word counted once.
    fn frequency(&self, term: &str, stem: &str) -> f64 {
        let start = self.words.partition_point(|(word, _)| word.as_str() < term);
        let mut count = self.words[start..]
            .iter()
            .take_while(|(word, _)| word.starts_with(term))
            .map(|(_, count)| count)
            .sum::<f64>();
        if let Some(indexes) = self.stems.get(stem) {
            count += indexes
                .iter()
                .map(|index| &self.words[*index])
                .filter(|(word, _)| !word.starts_with(term))
                .map(|(_, count)| count)
                .sum::<f64>();
        }
        count * self.weight
    }
}

/// From how many entries BM25 builds and scores them on several threads: below
/// it, starting the threads costs more than they save.
const PARALLEL_FROM: usize = 512;

/// `f` over `items`, in order, on as many threads as the machine has when
/// there are enough items to share; on this thread otherwise.
fn parallel_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    if items.len() < PARALLEL_FROM || threads < 2 {
        return items.iter().map(f).collect();
    }
    let chunk = items.len().div_ceil(threads);
    std::thread::scope(|scope| {
        let f = &f;
        let handles = items
            .chunks(chunk)
            .map(|part| scope.spawn(move || part.iter().map(f).collect::<Vec<_>>()))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .flat_map(|handle| {
                // A chunk that panicked is not dropped: its results would leave
                // every later index pointing at another entry.
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    })
}

/// BM25 over the considered entries, best first, only those holding at least
/// one of the query's words: the words it begins, and the words of its stem,
/// with Snowball's English stemmer as memory-engine's Postgres configuration
/// has it. Fields weigh by what they say of the entry: its title most, then
/// its content and where it sits, then its facets.
fn bm25(considered: &[&Placed<'_>], query: &str) -> Vec<(usize, f64)> {
    let entries = considered
        .iter()
        .map(|placed| placed.entry)
        .collect::<Vec<_>>();
    let terms = harness::query_terms(query)
        .into_iter()
        .map(|term| (term, 1.0))
        .collect::<Vec<_>>();
    bm25_weighted(&entries, &terms)
}

/// One entry's fields as BM25 reads them, its words as written and folded.
struct Fields {
    written: [Field; 4],
    folded: [Field; 4],
}

impl Fields {
    fn new(entry: &Entry, stemmer: &rust_stemmers::Stemmer) -> Self {
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
        let place = format!("{} {}", entry.tree.replace('/', " "), entry.id);
        let texts = [
            (entry.title.as_deref().unwrap_or_default(), TITLE_WEIGHT),
            (entry.content.as_deref().unwrap_or_default(), CONTENT_WEIGHT),
            (place.as_str(), PATH_WEIGHT),
            (facets.as_str(), META_WEIGHT),
        ];
        let fields =
            |fold: bool| texts.map(|(text, weight)| Field::new(words(text, fold), weight, stemmer));
        Self {
            written: fields(false),
            folded: fields(true),
        }
    }

    /// How often the entry holds a term, in the form the term is in: a term
    /// without accents reads the entry without its accents, so `may in` finds
    /// `máy in`; one with accents reads it as written.
    fn frequency(&self, term: &str, stem: &str, fold: bool) -> f64 {
        let fields = if fold { &self.folded } else { &self.written };
        fields.iter().map(|field| field.frequency(term, stem)).sum()
    }

    #[allow(clippy::cast_precision_loss, reason = "counts are small")]
    fn length(&self) -> f64 {
        self.written
            .iter()
            .map(|field| field.total as f64 * field.weight)
            .sum()
    }
}

/// [`bm25`] for a query whose terms weigh differently - the turn's digest
/// weighs a goal's words above an earlier message's - over plain entries.
/// The indexes of the entries holding a term, best first.
#[allow(clippy::cast_precision_loss, reason = "counts are small")]
pub fn bm25_weighted(entries: &[&Entry], terms: &[(String, f64)]) -> Vec<(usize, f64)> {
    if terms.is_empty() || entries.is_empty() {
        return Vec::new();
    }
    let stemmer = rust_stemmers::Stemmer::create(rust_stemmers::Algorithm::English);
    let terms = terms
        .iter()
        .map(|(term, weight)| {
            let fold = super::knowledge::store::fold(term) == *term;
            let stem = stemmer.stem(term).into_owned();
            (term.as_str(), stem, fold, *weight)
        })
        .collect::<Vec<_>>();
    // Each entry's fields are tokenized, stemmed and indexed on its own: the
    // part that grows with the store, shared across threads for a large one.
    let fields = parallel_map(entries, |entry| {
        Fields::new(
            entry,
            &rust_stemmers::Stemmer::create(rust_stemmers::Algorithm::English),
        )
    });
    let lengths = fields.iter().map(Fields::length).collect::<Vec<_>>();
    let average = (lengths.iter().sum::<f64>() / lengths.len() as f64).max(1.0);
    let documents = entries.len() as u64;
    // Each term's frequency in each entry, once: the document frequencies and
    // the scores both read it.
    let frequencies = parallel_map(&fields, |fields| {
        terms
            .iter()
            .map(|(term, stem, fold, _)| fields.frequency(term, stem, *fold))
            .collect::<Vec<_>>()
    });
    let idf = (0..terms.len())
        .map(|term| {
            let holding = frequencies
                .iter()
                .filter(|frequencies| frequencies[term] > 0.0)
                .count() as u64;
            harness_types::search::bm25_idf(documents, holding)
        })
        .collect::<Vec<_>>();
    let mut ranked = frequencies
        .iter()
        .enumerate()
        .filter_map(|(index, frequencies)| {
            let mut score = 0.0;
            for ((tf, (_, _, _, weight)), idf) in frequencies.iter().zip(&terms).zip(&idf) {
                if *tf > 0.0 {
                    score += weight * idf * tf * (K1 + 1.0)
                        / (tf + K1 * (1.0 - B + B * lengths[index] / average));
                }
            }
            (score > 0.0).then_some((index, score))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}

/// Memory search's BM25 over plain documents, each a title and a text: their
/// indexes, best first, only those holding one of the query's words. What a
/// search over files that are not memory entries - the session logs - ranks
/// by, so it weighs words as memory search does.
#[must_use]
pub fn rank_documents(documents: &[(&str, &str)], query: &str) -> Vec<(usize, f64)> {
    // No id and no tree: BM25 reads them as the entry's path, and a document's
    // index there would match a number in the query.
    let entries = documents
        .iter()
        .map(|(title, text)| Entry {
            id: String::new(),
            name: String::new(),
            memory: None,
            kind: String::new(),
            title: Some((*title).to_owned()),
            content: Some((*text).to_owned()),
            path: String::new(),
            scope: String::new(),
            version: 0,
            reference: serde_json::Map::new(),
            arguments: serde_json::Map::new(),
            pinned: false,
            updated_at: None,
            project: None,
            aliases: Vec::new(),
            tree: String::new(),
            meta: serde_json::Map::new(),
            temporal: None,
            ended: false,
            source: None,
            usage: memory_store::Usage::default(),
        })
        .collect::<Vec<_>>();
    let doc = Value::Null;
    let placed = entries
        .iter()
        .map(|entry| Placed {
            entry,
            dir: Path::new(""),
            doc: &doc,
        })
        .collect::<Vec<_>>();
    bm25(&placed.iter().collect::<Vec<_>>(), query)
}

/// The slots a `readout` search reads by default, and at most: MEMORILLA's
/// `K` is 16.
pub const DEFAULT_SLOTS: usize = 8;
pub const MAX_SLOTS: usize = 16;

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

/// A hit with only the fields `select` names, read from the entry as stored:
/// `meta.<key>` under `meta`, left out when the entry has no such facet, as
/// memory-engine projects it.
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
            if let Some(found) = entry.meta.get(key) {
                if !out.get("meta").is_some_and(Value::is_object) {
                    out.insert("meta".to_owned(), json!({}));
                }
                out["meta"][key] = found.clone();
            }
            continue;
        }
        if field.starts_with("content:") {
            if let Ok((start, end)) = slice(field) {
                out.insert(
                    "content".to_owned(),
                    json!(content_slice(content, start, end)),
                );
                out.insert("content_length".to_owned(), json!(content.chars().count()));
            }
            continue;
        }
        let value = match *field {
            "id" => json!(entry.id),
            "name" => json!(entry.name),
            "memory_id" => json!(entry.memory),
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

    /// The indexed field counts a term as a pass over its words would: the
    /// words it begins and the words of its stem, each occurrence once.
    #[test]
    fn an_indexed_field_counts_as_a_pass_over_its_words() {
        let stemmer = rust_stemmers::Stemmer::create(rust_stemmers::Algorithm::English);
        let words = "deploy deploys deploying deployed redeploy deploy staging stage"
            .split(' ')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let field = Field::new(words.clone(), 2.0, &stemmer);
        for term in ["deploy", "deployed", "stag", "stage", "x", "re"] {
            let stem = stemmer.stem(term).into_owned();
            #[allow(clippy::cast_precision_loss, reason = "a few words")]
            let expected = words
                .iter()
                .filter(|word| word.starts_with(term) || stemmer.stem(word) == stem)
                .count() as f64
                * 2.0;
            assert!(
                (field.frequency(term, &stem) - expected).abs() < 1e-9,
                "{term}: {} != {expected}",
                field.frequency(term, &stem)
            );
        }
        assert_eq!(field.total, words.len());
    }

    /// Many entries are built and scored on several threads, in the order
    /// one thread gives; run with `--ignored` to see the time it takes.
    #[test]
    #[ignore = "a timing, not a check"]
    fn bm25_over_ten_thousand_entries() {
        let entries = (0..10_000)
            .map(|index| Entry {
                id: format!("e{index}"),
                name: format!("e{index}"),
                memory: None,
                kind: "memory".to_owned(),
                title: Some(format!("Note {index} about the parser and deploys")),
                content: Some(format!(
                    "entry {index}: the build of service {} runs cargo test before a deploy to staging; máy in số {}",
                    index % 97,
                    index % 13
                )),
                path: "general".to_owned(),
                scope: "global".to_owned(),
                version: 1,
                reference: serde_json::Map::new(),
                arguments: serde_json::Map::new(),
                pinned: false,
                updated_at: None,
                project: None,
                aliases: Vec::new(),
                tree: "/global/general".to_owned(),
                meta: serde_json::Map::new(),
                temporal: None,
                ended: false,
                source: None,
                usage: memory_store::Usage::default(),
            })
            .collect::<Vec<_>>();
        let refs = entries.iter().collect::<Vec<_>>();
        let terms = harness::weighted_terms(
            Some("fix the deploy of service 42"),
            &["may in so 7 bi loi", "the parser fails on staging"],
        );
        let started = std::time::Instant::now();
        let ranked = bm25_weighted(&refs, &terms);
        eprintln!(
            "bm25 over {} entries, {} terms: {:?}, {} ranked",
            entries.len(),
            terms.len(),
            started.elapsed(),
            ranked.len()
        );
        assert!(!ranked.is_empty());
    }

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

    #[test]
    fn plain_documents_rank_by_their_words_and_never_by_their_place() {
        let mut documents = vec![("Parser", "the tokenizer"), ("Release", "notes for 2026")];
        documents.extend(std::iter::repeat_n(("Docs", "a pass"), 12));
        let ranked = super::rank_documents(&documents, "tokenizer");
        assert_eq!(
            ranked.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            [0]
        );
        // "11" is a document's place, not a word any document holds.
        assert!(super::rank_documents(&documents, "11").is_empty());
        assert_eq!(super::rank_documents(&documents, "2026")[0].0, 1);
    }
}
