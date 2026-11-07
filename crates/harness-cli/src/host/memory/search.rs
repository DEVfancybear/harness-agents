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
use super::memory_validity::{self, Window};

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

/// What a search reads: `memory` (the default), `knowledge`, `history` (the
/// project's conversations as the journal keeps them), or several.
///
/// # Errors
/// `sources` is not a non-empty list of those.
pub fn sources(arguments: &Value) -> Result<Vec<&'static str>, String> {
    let Some(given) = arguments.get("sources").filter(|value| !value.is_null()) else {
        return Ok(vec!["memory"]);
    };
    let refusal = || "sources is a list of memory, knowledge and history".to_owned();
    let names = given.as_array().ok_or_else(refusal)?;
    let mut sources = Vec::new();
    for name in names {
        let source = match name.as_str() {
            Some("memory") => "memory",
            Some("knowledge") => "knowledge",
            Some("history") => "history",
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
            if sources(arguments)?
                .iter()
                .any(|source| matches!(*source, "knowledge" | "history"))
            {
                return Err(
                    "a readout reads memory entries; search knowledge and history with mode rank"
                        .to_owned(),
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
    for key in TIMES {
        match arguments.get(key) {
            None | Some(Value::Null) => {}
            Some(Value::String(time)) => {
                memory_store::instant(time)?;
            }
            Some(_) => return Err(format!("{key} is a time, such as 2026-10-26T09:30:00Z")),
        }
    }
    if !matches!(
        arguments.get("include_ended"),
        None | Some(Value::Null | Value::Bool(_))
    ) {
        return Err("include_ended is true or false".to_owned());
    }
    check_presentation(arguments)
}

/// The times a search may name beside the temporal filter: `as_of`, the
/// entries that held then; `since` and `until`, those that held at some time
/// between.
const TIMES: [&str; 3] = ["as_of", "since", "until"];

/// The time a search names, as a window: `as_of` a point, `since`/`until`
/// and the temporal filter's modes a window, all narrowed together. `None`
/// when it names none.
///
/// # Errors
/// A time or the temporal filter is malformed.
pub fn named_time(arguments: &Value) -> Result<Option<Window>, String> {
    let time = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .map(memory_store::instant)
            .transpose()
    };
    let mut window: Option<Window> = None;
    let mut narrow = |other: Window| {
        window = Some(match window.take() {
            Some(window) => window.and(&other),
            None => other,
        });
    };
    if let Some(at) = time("as_of")? {
        narrow(Window {
            from: Some(at.clone()),
            to: Some(at),
        });
    }
    let (since, until) = (time("since")?, time("until")?);
    if since.is_some() || until.is_some() {
        narrow(Window {
            from: since,
            to: until,
        });
    }
    if let Some(temporal) = arguments.get("temporal").filter(|value| !value.is_null()) {
        let (from, to) = Temporal::parse(temporal)?.window();
        narrow(Window { from, to });
    }
    Ok(window)
}

/// `ranked` with no entry that ended (its period over, or closed by a
/// correction or the expiry pass) right above a current one the words
/// matched as well or better, and none shown as unverified (a web page's, a
/// child's) above a verified one so matched: what is true now, and what the
/// user or the agent said, comes first among equals. Otherwise the order
/// stands.
#[must_use]
pub fn settled(entries: &[&Entry], mut ranked: Vec<(usize, f64)>) -> Vec<(usize, f64)> {
    let standing = |index: usize| {
        let entry = entries[index];
        (
            u8::from(entry.ended || entry.standing.closed()),
            u8::from(entry.standing.unverified()),
        )
    };
    for at in 1..ranked.len() {
        let mut place = at;
        while place > 0
            && standing(ranked[place].0) < standing(ranked[place - 1].0)
            && ranked[place].1 >= ranked[place - 1].1
        {
            ranked.swap(place, place - 1);
            place -= 1;
        }
    }
    ranked
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
        // reordered by recency and use. One query needs only its best
        // candidates; variants are fused over their whole rankings.
        let best_of = (queries.len() == 1).then_some(candidates);
        let rankings = queries
            .iter()
            .map(|query| bm25(&considered, query, best_of))
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
    // A search that names a time ranks by how well each entry's validity
    // matches it, then by the words; one that names none puts what holds
    // now, and what was not a page's or a child's, first among equals.
    let ranked = match named_time(arguments)? {
        Some(window) => {
            let now = chrono::Utc::now().timestamp_millis();
            let mut fitted = ranked
                .into_iter()
                .map(|(index, score)| {
                    let fit = memory_validity::fit(considered[index].doc, &window, now);
                    (index, score, fit)
                })
                .collect::<Vec<_>>();
            fitted.sort_by(|a, b| b.2.total_cmp(&a.2));
            fitted
                .into_iter()
                .map(|(index, score, _)| (index, score))
                .collect()
        }
        None if mode == "fulltext" => settled(
            &considered
                .iter()
                .map(|placed| placed.entry)
                .collect::<Vec<_>>(),
            ranked,
        ),
        None => ranked,
    };
    let ranked = named_first(&considered, &queries, ranked);
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
    let left_out = closed_left_out(placed, arguments, &queries)?;
    if left_out > 0 {
        result["closed"] = json!({
            "left_out": left_out,
            "note": format!("{left_out} closed entries (superseded by a correction, or expired unused) also match: as_of a time reads what held then, include_ended: true reads them all"),
        });
    }
    if hits_empty(&result) {
        result["note"] = json!(miss_note(&queries));
        missed_branches(&mut result, &considered);
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
        "No memory matched {searched}. Search matches words (stems, prefixes, accents folded), not meaning: search again with other words - a synonym, the other language the project uses, a shorter or broader term, a name or path from the code - or fewer filters, before saying it is not remembered. `branches` lists where the searched entries sit, named in the store's own words and language: search in that language, or browse a branch with `tree` and no query."
    )
}

/// The most branches a missed search lists.
const MISSED_BRANCHES: usize = 12;

/// A missed search's `branches`: the trees the searched entries sit in, with
/// how many each holds, the fullest first, so the model sees the words and
/// the language the store is written in. Nothing when nothing was searched.
pub fn missed_branches(result: &mut Value, considered: &[&Placed<'_>]) {
    let mut counts = BTreeMap::<&str, usize>::new();
    for placed in considered {
        *counts.entry(placed.entry.tree.as_str()).or_default() += 1;
    }
    if counts.is_empty() {
        return;
    }
    let mut branches = counts.into_iter().collect::<Vec<_>>();
    branches.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    result["branches"] = branches
        .into_iter()
        .take(MISSED_BRANCHES)
        .map(|(tree, entries)| json!({"tree": tree, "entries": entries}))
        .collect();
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
    let held = held(arguments)?;
    Ok(placed
        .iter()
        .filter(|placed| {
            let entry = placed.entry;
            let facets = Value::Object(entry.meta.clone());
            held(placed)
                && tree.as_ref().is_none_or(|tree| tree.matches(&entry.tree))
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

/// Which entries a search reads by when they held. An entry a correction or
/// the expiry pass closed is read only when the search names a time or
/// `include_ended`; one whose own period ended is read as before, ranked
/// below a current one ([`settled`]). With `as_of`, only the entries that
/// held then (and had been written by then); with `since`/`until`, those
/// that held at some time between.
///
/// # Errors
/// A time is malformed.
fn held(arguments: &Value) -> Result<impl Fn(&Placed<'_>) -> bool, String> {
    let as_of = arguments
        .get("as_of")
        .and_then(Value::as_str)
        .map(memory_store::instant)
        .transpose()?;
    let between = match (arguments.get("since"), arguments.get("until")) {
        (None | Some(Value::Null), None | Some(Value::Null)) => None,
        _ => named_time(&json!({"since": arguments["since"], "until": arguments["until"]}))?,
    };
    let closed_too = arguments["include_ended"] == true || named_time(arguments)?.is_some();
    Ok(move |placed: &Placed<'_>| {
        (closed_too || !placed.entry.standing.closed())
            && as_of
                .as_deref()
                .is_none_or(|at| memory_validity::valid_at(placed.doc, at))
            && between
                .as_ref()
                .is_none_or(|window| memory_validity::overlaps(placed.doc, window))
    })
}

/// How many closed entries a search that names no time left out although its
/// words match them: it says so, so a question about what held before is
/// asked again with `as_of`.
///
/// # Errors
/// A filter is malformed.
fn closed_left_out(
    placed: &[Placed<'_>],
    arguments: &Value,
    queries: &[String],
) -> Result<usize, String> {
    if queries.is_empty() || arguments["include_ended"] == true || named_time(arguments)?.is_some()
    {
        return Ok(0);
    }
    let mut wide = arguments.clone();
    wide["include_ended"] = json!(true);
    let closed = filtered(placed, &wide)?
        .into_iter()
        .filter(|placed| placed.entry.standing.closed())
        .collect::<Vec<_>>();
    if closed.is_empty() {
        return Ok(0);
    }
    Ok(queries
        .iter()
        .flat_map(|query| bm25(&closed, query, None))
        .map(|(index, _)| index)
        .collect::<std::collections::BTreeSet<_>>()
        .len())
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

/// The paths a search's queries name: words with a `/` in them, or a file's
/// name with its extension (`invoice.rs`), quotes and brackets aside.
fn named_paths(queries: &[String]) -> Vec<String> {
    let mut named = Vec::new();
    for word in queries.iter().flat_map(|query| query.split_whitespace()) {
        let word = word.trim_matches(|character: char| {
            matches!(
                character,
                '"' | '\'' | '`' | '(' | ')' | '[' | ']' | ',' | ';' | ':'
            )
        });
        let path_like = word.contains('/')
            || word.contains('\\')
            || word.rsplit_once('.').is_some_and(|(stem, extension)| {
                !stem.is_empty()
                    && !extension.is_empty()
                    && extension
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric())
                    && stem.chars().last().is_some_and(char::is_alphanumeric)
            });
        if path_like && !named.iter().any(|seen| seen == word) {
            named.push(word.to_owned());
        }
    }
    named
}

/// `ranked` with the entries kept for, or citing, a path the queries name
/// first, each group in its order: a search that names a file finds what
/// was kept for it.
fn named_first(
    considered: &[&Placed<'_>],
    queries: &[String],
    ranked: Vec<(usize, f64)>,
) -> Vec<(usize, f64)> {
    let named = named_paths(queries);
    if named.is_empty() {
        return ranked;
    }
    let (first, rest): (Vec<_>, Vec<_>) = ranked.into_iter().partition(|(index, _)| {
        let cited = &considered[*index].entry.cited;
        named.iter().any(|path| cited.names(path))
    });
    first.into_iter().chain(rest).collect()
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

/// A search of the project's conversations as the journal keeps them, for a
/// memory search with `sources` naming `history`: the hits of `query` (at
/// most `limit`) as values, and how many sessions the index had not caught
/// up yet. It is the `history_search` tool's search with scope `project`.
pub type HistorySearch =
    std::sync::Arc<dyn Fn(&str, usize) -> Result<(Vec<Value>, u64), String> + Send + Sync>;

/// How long a history search waits for the journal's index to catch up, as
/// the `history_search` tool does; what is left is caught up behind it.
const HISTORY_CATCH_UP: std::time::Duration = std::time::Duration::from_secs(3);

/// The characters of a history hit's text a memory search shows.
const HISTORY_PREVIEW_CHARS: usize = 400;

/// [`HistorySearch`] over the project store `store` (the turn's writer, held
/// weakly: the index is brought up to date before the search) for the conversations of
/// `project`, `task` the one asking. Called from a blocking thread: the
/// search runs on the runtime `handle` from a thread of its own.
#[must_use]
pub fn history_searcher(
    handle: tokio::runtime::Handle,
    store: std::sync::Weak<harness_store_sqlite::SqliteStore>,
    project: harness_types::ProjectId,
    task: harness_types::TaskId,
) -> HistorySearch {
    std::sync::Arc::new(move |query: &str, limit: usize| {
        // The store is the turn's: a search after it closed finds nothing.
        let store = store.upgrade().ok_or("the project store is closed")?;
        let (project, task, query) = (project.clone(), task.clone(), query.to_owned());
        let search = async move {
            let caught = store
                .catch_up_history(
                    None,
                    harness_store_sqlite::CatchUpPace::Until(
                        std::time::Instant::now() + HISTORY_CATCH_UP,
                    ),
                )
                .await
                .map_err(|error| error.to_string())?;
            if caught.behind > 0 {
                store.catch_up_history_in_background(None);
            }
            let scope = harness_store_sqlite::HistoryScope::new(project, task).across_project();
            let hits = store
                .history_search_filtered(
                    &scope,
                    &query,
                    limit,
                    &harness_store_sqlite::HistoryFilter::default(),
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok((hits.iter().map(history_hit).collect(), caught.behind))
        };
        let handle = handle.clone();
        std::thread::scope(|scope| {
            scope
                .spawn(move || handle.block_on(search))
                .join()
                .unwrap_or_else(|_| Err("the history search stopped".to_owned()))
        })
    })
}

/// One history hit as a memory search shows it: the source `history_read`
/// reads, its kind, the conversation and when its session began, and the
/// head of its text.
fn history_hit(hit: &harness_store_sqlite::HistoryHit) -> Value {
    json!({
        "source_id": hit.source_id,
        "kind": hit.kind,
        "conversation": hit.task_id,
        "started": hit.started,
        "matched_terms": format!("{}/{}", hit.matched_terms, hit.query_terms),
        "preview": hit.preview.chars().take(HISTORY_PREVIEW_CHARS).collect::<String>(),
    })
}

/// A field's words as BM25 matches them: each distinct word once, sorted,
/// with how often it occurs, and the words of each English stem. A term's
/// frequency is then a binary search for the words it begins and a lookup of
/// its stem's words, not a pass over every word of every field. How BM25
/// read an entry before the index ([`Corpus`]); the tests check the index
/// against it.
#[cfg(test)]
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

#[cfg(test)]
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
/// its content and where it sits, then its facets. With `limit`, only the
/// best `limit` of them, chosen without sorting the rest.
fn bm25(considered: &[&Placed<'_>], query: &str, limit: Option<usize>) -> Vec<(usize, f64)> {
    let entries = considered
        .iter()
        .map(|placed| placed.entry)
        .collect::<Vec<_>>();
    let terms = harness::query_terms(query)
        .into_iter()
        .map(|term| (term, 1.0))
        .collect::<Vec<_>>();
    best(bm25_scores(&entries, &terms), limit.unwrap_or(usize::MAX))
}

/// The texts of an entry's four fields, as BM25 reads them: its title, its
/// content, where it sits (its tree and its key), and its facets (`pinned`
/// left out), each weighing [`FIELD_WEIGHTS`].
fn field_texts(entry: &Entry) -> [String; 4] {
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
    // The files it is kept for and cites are words of its place: a search
    // for `billing` finds an entry kept for `src/billing/**`.
    let files = entry
        .cited
        .paths
        .iter()
        .map(String::as_str)
        .chain(
            entry
                .cited
                .citations
                .iter()
                .map(|citation| citation.path.as_str()),
        )
        .map(|path| path.replace(['/', '*', '.'], " "))
        .collect::<Vec<_>>()
        .join(" ");
    [
        entry.title.clone().unwrap_or_default(),
        entry.content.clone().unwrap_or_default(),
        format!("{} {} {files}", entry.tree.replace('/', " "), entry.id),
        facets,
    ]
}

/// What each of [`field_texts`] weighs.
const FIELD_WEIGHTS: [f64; 4] = [TITLE_WEIGHT, CONTENT_WEIGHT, PATH_WEIGHT, META_WEIGHT];

/// One entry's fields as BM25 read them before the index, its words as
/// written and folded: the reference the index is checked against.
#[cfg(test)]
struct Fields {
    written: [Field; 4],
    folded: [Field; 4],
}

#[cfg(test)]
impl Fields {
    fn new(entry: &Entry, stemmer: &rust_stemmers::Stemmer) -> Self {
        let texts = field_texts(entry);
        let fields = |fold: bool| {
            std::array::from_fn(|field| {
                Field::new(words(&texts[field], fold), FIELD_WEIGHTS[field], stemmer)
            })
        };
        Self {
            written: fields(false),
            folded: fields(true),
        }
    }

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

/// BM25 as it ran before the index: every entry tokenized, stemmed and
/// scored on every call. The tests check [`bm25_weighted`] against it.
#[cfg(test)]
#[allow(clippy::cast_precision_loss, reason = "counts are small")]
fn bm25_reference(entries: &[&Entry], terms: &[(String, f64)]) -> Vec<(usize, f64)> {
    if terms.is_empty() || entries.is_empty() {
        return Vec::new();
    }
    let stemmer = rust_stemmers::Stemmer::create(rust_stemmers::Algorithm::English);
    let terms = prepared(terms, &stemmer);
    let fields = entries
        .iter()
        .map(|entry| Fields::new(entry, &stemmer))
        .collect::<Vec<_>>();
    let lengths = fields.iter().map(Fields::length).collect::<Vec<_>>();
    let average = (lengths.iter().sum::<f64>() / lengths.len() as f64).max(1.0);
    let documents = entries.len() as u64;
    let frequencies = fields
        .iter()
        .map(|fields| {
            terms
                .iter()
                .map(|term| fields.frequency(&term.text, &term.stem, term.fold))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
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
            for ((tf, term), idf) in frequencies.iter().zip(&terms).zip(&idf) {
                if *tf > 0.0 {
                    score += term.weight * idf * tf * (K1 + 1.0)
                        / (tf + K1 * (1.0 - B + B * lengths[index] / average));
                }
            }
            (score > 0.0).then_some((index, score))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}

/// A query term as BM25 matches it: its text, its English stem, whether it
/// reads the entries without their accents, and what it weighs.
struct Term {
    text: String,
    stem: String,
    fold: bool,
    weight: f64,
}

fn prepared(terms: &[(String, f64)], stemmer: &rust_stemmers::Stemmer) -> Vec<Term> {
    terms
        .iter()
        .map(|(term, weight)| Term {
            fold: super::knowledge::store::fold(term) == *term,
            stem: stemmer.stem(term).into_owned(),
            text: term.clone(),
            weight: *weight,
        })
        .collect()
}

/// Where an entry was read from, so its words are indexed once and kept
/// until it changes: its store (the directory and the tree root it was read
/// under) and its `version_hash`, which every write that changes its title,
/// content, tree, name or meta changes.
#[derive(Clone, Debug)]
pub struct Indexed {
    pub store: std::sync::Arc<str>,
    pub hash: String,
}

/// One entry's words counted per field, written and folded, and its length:
/// what the index keeps of it.
struct Counted {
    /// `(word, field, times as written, times folded)`.
    words: Vec<(String, u8, u32, u32)>,
    length: f64,
}

#[allow(clippy::cast_precision_loss, reason = "counts are small")]
fn counted(entry: &Entry) -> Counted {
    let texts = field_texts(entry);
    let mut counts = BTreeMap::<(String, u8), (u32, u32)>::new();
    let mut totals = [0_usize; 4];
    for (field, text) in texts.iter().enumerate() {
        let written = words(text, false);
        totals[field] = written.len();
        #[allow(clippy::cast_possible_truncation, reason = "four fields")]
        let label = field as u8;
        for word in written {
            counts.entry((word, label)).or_default().0 += 1;
        }
        for word in words(text, true) {
            counts.entry((word, label)).or_default().1 += 1;
        }
    }
    Counted {
        words: counts
            .into_iter()
            .map(|((word, field), (written, folded))| (word, field, written, folded))
            .collect(),
        length: totals
            .iter()
            .zip(FIELD_WEIGHTS)
            .map(|(total, weight)| *total as f64 * weight)
            .sum(),
    }
}

/// One entry's use of one word in one field.
#[derive(Clone, Copy)]
struct Posting {
    slot: u32,
    field: u8,
    written: u32,
    folded: u32,
}

/// The words of a store's entries, indexed once: each entry a slot with its
/// length, each word the slots that hold it (an inverted index), and the
/// words of each English stem. A term's entries are then the postings of the
/// words it begins and of its stem's words, not a pass over every entry.
#[derive(Default)]
struct Corpus {
    /// Each entry's slot by `(kind, key)`, with the hash it was indexed at.
    slots: std::collections::HashMap<(String, String), (String, u32)>,
    lengths: Vec<f64>,
    /// Every word, in byte order: the words a term begins sit together.
    words: BTreeMap<String, u32>,
    /// Each word's text, by its id.
    names: Vec<String>,
    stems: std::collections::HashMap<String, Vec<u32>>,
    postings: Vec<Vec<Posting>>,
    /// The most entries one call read from this store since it was indexed:
    /// slots past twice that are entries changed or gone, and the corpus is
    /// indexed again.
    peak: usize,
    /// When it was last read, in calls: the least recent store goes first.
    used: u64,
}

impl Corpus {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a store's slots and words fit u32"
    )]
    fn add(&mut self, counted: Counted, stemmer: &rust_stemmers::Stemmer) -> u32 {
        let slot = self.lengths.len() as u32;
        self.lengths.push(counted.length);
        for (word, field, written, folded) in counted.words {
            let id = if let Some(id) = self.words.get(&word) {
                *id
            } else {
                let id = self.names.len() as u32;
                self.stems
                    .entry(stemmer.stem(&word).into_owned())
                    .or_default()
                    .push(id);
                self.names.push(word.clone());
                self.postings.push(Vec::new());
                self.words.insert(word, id);
                id
            };
            self.postings[id as usize].push(Posting {
                slot,
                field,
                written,
                folded,
            });
        }
        slot
    }

    /// The words a term matches: those it begins (`deploy` finds `deploys`),
    /// then the words of its stem it does not begin (`deploying` finds
    /// `deployed`); each word once.
    fn matching(&self, term: &str, stem: &str) -> Vec<u32> {
        let mut ids = self
            .words
            .range::<str, _>((std::ops::Bound::Included(term), std::ops::Bound::Unbounded))
            .take_while(|(word, _)| word.starts_with(term))
            .map(|(_, id)| *id)
            .collect::<Vec<_>>();
        if let Some(stemmed) = self.stems.get(stem) {
            ids.extend(
                stemmed
                    .iter()
                    .filter(|id| !self.names[**id as usize].starts_with(term)),
            );
        }
        ids
    }
}

/// The most stores whose index is kept: a long-lived process serves the
/// global store and the stores of the conversations it runs.
const INDEXED_STORES: usize = 32;
/// Slots a store's index may hold beyond twice its peak before it is indexed
/// again.
const INDEX_SLACK: usize = 1_024;

/// Every store's index, for the process: the digest and memory search share
/// it, so a turn re-reads only the entries that changed since the last one.
fn indexes() -> &'static std::sync::Mutex<std::collections::HashMap<std::sync::Arc<str>, Corpus>> {
    static INDEXES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::sync::Arc<str>, Corpus>>,
    > = std::sync::OnceLock::new();
    INDEXES.get_or_init(Default::default)
}

/// How many calls have read an index: a store's `used`.
static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// No entry in this slot.
const NO_ENTRY: u32 = u32::MAX;

/// [`bm25`] for a query whose terms weigh differently - the turn's digest
/// weighs a goal's words above an earlier message's - over plain entries.
/// The indexes of the entries holding a term, best first.
#[cfg(test)]
#[must_use]
pub fn bm25_weighted(entries: &[&Entry], terms: &[(String, f64)]) -> Vec<(usize, f64)> {
    best(bm25_scores(entries, terms), usize::MAX)
}

/// The best `limit` of `scores`, best first, ties by index: the order a
/// full sort gives, without sorting the rest.
#[must_use]
pub fn best(mut scores: Vec<(usize, f64)>, limit: usize) -> Vec<(usize, f64)> {
    let order =
        |a: &(usize, f64), b: &(usize, f64)| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0));
    if limit == 0 {
        return Vec::new();
    }
    if scores.len() > limit {
        scores.select_nth_unstable_by(limit - 1, order);
        scores.truncate(limit);
    }
    scores.sort_by(order);
    scores
}

/// Each entry's BM25 score, in the entries' order, for those holding a term
/// (a score above zero). The statistics are those of `entries`. An entry read
/// from a store ([`Entry::indexed`]) is looked up in that store's index and
/// indexed only when it is new or changed; any other is indexed for this call.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    reason = "counts are small; one pass: index, then score"
)]
#[must_use]
pub fn bm25_scores(entries: &[&Entry], terms: &[(String, f64)]) -> Vec<(usize, f64)> {
    if terms.is_empty() || entries.is_empty() {
        return Vec::new();
    }
    let stemmer = rust_stemmers::Stemmer::create(rust_stemmers::Algorithm::English);
    let terms = prepared(terms, &stemmer);
    // The stores' indexes are taken out for the call, and put back after: a
    // call on another thread meanwhile indexes its own, and either is right.
    let mut corpora: Vec<(Option<std::sync::Arc<str>>, Corpus)> = Vec::new();
    let mut taken = std::collections::HashMap::<std::sync::Arc<str>, usize>::new();
    let use_count = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    {
        let mut cache = indexes()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for entry in entries {
            if let Some(indexed) = &entry.indexed
                && !taken.contains_key(&indexed.store)
            {
                let corpus = cache.remove(&indexed.store).unwrap_or_default();
                taken.insert(indexed.store.clone(), corpora.len());
                corpora.push((Some(indexed.store.clone()), corpus));
            }
        }
    }
    let ephemeral = corpora.len();
    corpora.push((None, Corpus::default()));
    // Each entry's (corpus, slot); the ones to index first.
    let mut places = vec![(0_usize, NO_ENTRY); entries.len()];
    let mut assigned = corpora
        .iter()
        .map(|_| std::collections::HashSet::<u32>::new())
        .collect::<Vec<_>>();
    let mut pending = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let found = entry.indexed.as_ref().and_then(|indexed| {
            let at = taken[&indexed.store];
            let slot = corpora[at]
                .1
                .slots
                .get(&(entry.kind.clone(), entry.id.clone()))
                .filter(|(hash, _)| *hash == indexed.hash)
                .map(|(_, slot)| *slot)?;
            assigned[at].insert(slot).then_some((at, slot))
        });
        match found {
            Some(place) => places[index] = place,
            None => pending.push(index),
        }
    }
    // New and changed entries are tokenized, stemmed and counted, on several
    // threads for many: the part that grows with what changed.
    let counts = parallel_map(&pending, |index| counted(entries[*index]));
    for (index, counted) in pending.into_iter().zip(counts) {
        let entry = entries[index];
        // A second entry of one key in one call is indexed apart.
        let at = entry
            .indexed
            .as_ref()
            .map(|indexed| taken[&indexed.store])
            .filter(|at| {
                !corpora[*at]
                    .1
                    .slots
                    .get(&(entry.kind.clone(), entry.id.clone()))
                    .is_some_and(|(_, slot)| assigned[*at].contains(slot))
            })
            .unwrap_or(ephemeral);
        let corpus = &mut corpora[at].1;
        let slot = corpus.add(counted, &stemmer);
        if at != ephemeral
            && let Some(indexed) = &entry.indexed
        {
            corpus.slots.insert(
                (entry.kind.clone(), entry.id.clone()),
                (indexed.hash.clone(), slot),
            );
        }
        assigned[at].insert(slot);
        places[index] = (at, slot);
    }
    // Which entry each slot of each corpus is in this call.
    let mut entry_of = corpora
        .iter()
        .map(|(_, corpus)| vec![NO_ENTRY; corpus.lengths.len()])
        .collect::<Vec<_>>();
    for (index, (at, slot)) in places.iter().enumerate() {
        entry_of[*at][*slot as usize] = index as u32;
    }
    let lengths = places
        .iter()
        .map(|(at, slot)| corpora[*at].1.lengths[*slot as usize])
        .collect::<Vec<_>>();
    let average = (lengths.iter().sum::<f64>() / lengths.len() as f64).max(1.0);
    let documents = entries.len() as u64;
    let mut counts = vec![[0.0_f64; 4]; entries.len()];
    let mut counting = vec![false; entries.len()];
    let mut touched = Vec::new();
    let mut scores = vec![0.0_f64; entries.len()];
    let mut marked = vec![false; entries.len()];
    let mut holding = Vec::new();
    for term in &terms {
        for (at, (_, corpus)) in corpora.iter().enumerate() {
            for word in corpus.matching(&term.text, &term.stem) {
                for posting in &corpus.postings[word as usize] {
                    let index = entry_of[at][posting.slot as usize];
                    let times = if term.fold {
                        posting.folded
                    } else {
                        posting.written
                    };
                    if index == NO_ENTRY || times == 0 {
                        continue;
                    }
                    let index = index as usize;
                    if !counting[index] {
                        counting[index] = true;
                        touched.push(index);
                    }
                    counts[index][usize::from(posting.field)] += f64::from(times);
                }
            }
        }
        // A field's count times its weight, summed over the fields in their
        // order: the frequency a pass over the entry's fields gives.
        let frequencies = touched
            .iter()
            .map(|index| {
                let frequency = counts[*index]
                    .iter()
                    .zip(FIELD_WEIGHTS)
                    .map(|(count, weight)| count * weight)
                    .sum::<f64>();
                (*index, frequency)
            })
            .filter(|(_, frequency)| *frequency > 0.0)
            .collect::<Vec<_>>();
        for index in touched.drain(..) {
            counts[index] = [0.0; 4];
            counting[index] = false;
        }
        let idf = harness_types::search::bm25_idf(documents, frequencies.len() as u64);
        for (index, tf) in frequencies {
            if !marked[index] {
                marked[index] = true;
                holding.push(index);
            }
            scores[index] += term.weight * idf * tf * (K1 + 1.0)
                / (tf + K1 * (1.0 - B + B * lengths[index] / average));
        }
    }
    let mut cache = indexes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (at, (store, mut corpus)) in corpora.into_iter().enumerate() {
        let Some(store) = store else {
            continue;
        };
        corpus.peak = corpus.peak.max(assigned[at].len());
        corpus.used = use_count;
        if corpus.lengths.len() <= corpus.peak.saturating_mul(2) + INDEX_SLACK {
            cache.insert(store, corpus);
        }
    }
    while cache.len() > INDEXED_STORES {
        let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, corpus)| corpus.used)
            .map(|(store, _)| store.clone())
        else {
            break;
        };
        cache.remove(&oldest);
    }
    drop(cache);
    holding.sort_unstable();
    holding
        .into_iter()
        .map(|index| (index, scores[index]))
        .filter(|(_, score)| *score > 0.0)
        .collect()
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
            indexed: None,
            standing: super::memory_validity::Standing::default(),
            cited: super::memory_citations::Cited::default(),
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
    bm25(&placed.iter().collect::<Vec<_>>(), query, None)
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

    /// Among entries the words matched alike, what holds now comes before
    /// what ended, and what the user or the agent said before a fetched
    /// page's words; a better match keeps its place.
    #[test]
    fn among_equals_what_holds_now_and_was_not_a_pages_comes_first() {
        let state = harness::parse(
            &json!({"schema": 1, "entries": {"memory": {
                "ended": {"title": "t", "content": "c", "temporal": {"start": "2026-01-01", "end": "2026-02-01"}},
                "current": {"title": "t", "content": "c"},
                "page": {"title": "t", "content": "c", "origin": {"kind": "web"}},
                "weaker": {"title": "t", "content": "c"},
            }}}),
            "global",
            "/global",
        );
        let entries = state.entries["memory"].iter().collect::<Vec<_>>();
        let at = |id: &str| {
            entries
                .iter()
                .position(|entry| entry.id == id)
                .expect("entry")
        };
        let ranked = settled(
            &entries,
            vec![
                (at("ended"), 1.0),
                (at("page"), 1.0),
                (at("current"), 1.0),
                (at("weaker"), 0.5),
            ],
        );
        assert_eq!(
            ranked
                .iter()
                .map(|(index, _)| entries[*index].id.as_str())
                .collect::<Vec<_>>(),
            ["current", "page", "ended", "weaker"]
        );
    }

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
                indexed: None,
                standing: super::memory_validity::Standing::default(),
                cited: super::super::memory_citations::Cited::default(),
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

    /// An entry of generated prose, indexed as read from `store` at `hash`.
    fn prose_entry(prose: &mut Prose, index: usize, store: &str, hash: &str) -> Entry {
        let title_words = 2 + prose.below(6);
        let content_words = 5 + prose.below(30);
        let mut meta = serde_json::Map::new();
        if prose.below(3) == 0 {
            meta.insert("type".to_owned(), json!(prose.words(1)));
        }
        if prose.below(5) == 0 {
            meta.insert("pinned".to_owned(), json!(true));
        }
        Entry {
            id: format!("k{index}"),
            name: format!("k{index}"),
            memory: None,
            kind: if index.is_multiple_of(7) {
                "prompt"
            } else {
                "memory"
            }
            .to_owned(),
            title: Some(prose.words(title_words)),
            content: Some(prose.words(content_words)),
            path: "general".to_owned(),
            scope: "global".to_owned(),
            version: 1,
            reference: serde_json::Map::new(),
            arguments: serde_json::Map::new(),
            pinned: false,
            updated_at: None,
            project: None,
            aliases: Vec::new(),
            tree: format!("/global/area{}/sở_thích", prose.below(4)),
            meta,
            temporal: None,
            ended: false,
            source: None,
            usage: memory_store::Usage::default(),
            indexed: Some(Indexed {
                store: store.into(),
                hash: hash.to_owned(),
            }),
            standing: super::memory_validity::Standing::default(),
            cited: super::super::memory_citations::Cited::default(),
        }
    }

    /// The same order and the same scores, to the bit.
    fn same(index: &[(usize, f64)], reference: &[(usize, f64)], what: &str) {
        assert_eq!(index.len(), reference.len(), "{what}: how many matched");
        for (got, want) in index.iter().zip(reference) {
            assert_eq!(got.0, want.0, "{what}: the order");
            assert_eq!(got.1.to_bits(), want.1.to_bits(), "{what}: a score");
        }
    }

    /// The index ranks as the pass over every entry did, over random stores
    /// and queries: on its first call, on later calls that read it, after
    /// entries change, are added and removed, over a subset of a store (a
    /// filtered search), over two stores at once, and with the same entry
    /// twice.
    #[test]
    fn the_index_ranks_exactly_as_a_pass_over_every_entry() {
        for seed in 1..=3_u64 {
            let mut prose = Prose::new(seed);
            let store = format!("equivalence-{seed}-{}", std::process::id());
            let mut entries = (0..200)
                .map(|index| prose_entry(&mut prose, index, &store, "v1"))
                .collect::<Vec<_>>();
            let queries = (0..5)
                .map(|_| {
                    let count = 1 + prose.below(6);
                    let text = prose.words(count);
                    let mut terms = harness::query_terms(&text)
                        .into_iter()
                        .map(|term| (term, [1.0, 1.5, 2.0][prose.below(3)]))
                        .collect::<Vec<_>>();
                    // Prefixes and folded forms: `dep`, `may`.
                    terms.push(("dep".to_owned(), 1.0));
                    terms.push(("may".to_owned(), 0.7));
                    terms
                })
                .collect::<Vec<_>>();
            let check = |entries: &[Entry], what: &str| {
                let refs = entries.iter().collect::<Vec<_>>();
                for terms in &queries {
                    same(
                        &bm25_weighted(&refs, terms),
                        &bm25_reference(&refs, terms),
                        what,
                    );
                }
            };
            check(&entries, "first call");
            check(&entries, "a call that reads the index");
            // Some entries change (a new hash), some are new, some gone.
            for index in (0..entries.len()).step_by(17) {
                let changed = prose_entry(&mut prose, index, &store, "v2");
                entries[index] = changed;
            }
            entries.extend((200..240).map(|index| prose_entry(&mut prose, index, &store, "v1")));
            entries.retain(|entry| !entry.id.ends_with('3'));
            check(&entries, "after a change");
            let subset = entries
                .iter()
                .filter(|entry| entry.tree.contains("area1"))
                .cloned()
                .collect::<Vec<_>>();
            check(&subset, "a subset of the store");
            let other = format!("{store}-local");
            let mut both = entries.clone();
            both.extend((0..60).map(|index| prose_entry(&mut prose, index, &other, "v1")));
            check(&both, "two stores");
            let mut twice = subset.clone();
            twice.push(subset[0].clone());
            check(&twice, "an entry twice");
            let plain = entries
                .iter()
                .cloned()
                .map(|mut entry| {
                    entry.indexed = None;
                    entry
                })
                .collect::<Vec<_>>();
            check(&plain, "entries read from no store");
            check(&entries, "the store again");
        }
    }

    /// The digest and a search over a store read whole rank as before, and
    /// a write to one entry is seen on the next read.
    #[test]
    fn a_store_read_whole_ranks_as_before_and_a_write_is_seen() {
        let directory = tempfile::tempdir().expect("tempdir");
        let dir = directory.path().join("harness");
        generated_store(&dir, 400, 11);
        let terms = harness::weighted_terms(
            Some("fix the deploy of kalo build"),
            &["parser tokens", "máy in số"],
        );
        let ranked = |state: &harness::State| {
            let entries = state.entries["memory"].iter().collect::<Vec<_>>();
            (
                bm25_weighted(&entries, &terms),
                bm25_reference(&entries, &terms),
            )
        };
        let state = harness::load(&dir, "global");
        assert!(state.entries["memory"][0].indexed.is_some());
        let (index, reference) = ranked(&state);
        same(&index, &reference, "a store's first read");
        // A write: the entry the words reach least now holds them all.
        let last = index.last().expect("a match").0;
        let key = state.entries["memory"][last].id.clone();
        let mut document = memory_store::load_document(&dir).expect("document");
        document["entries"]["memory"][&key]["title"] = json!("fix the deploy of kalo build");
        memory_store::save_document(
            &dir,
            &document,
            &memory_store::Writer::new("model", &json!({"via": "test"}), "update"),
        )
        .expect("write");
        let state = harness::load(&dir, "global");
        let (index, reference) = ranked(&state);
        same(&index, &reference, "after a write");
        assert_eq!(
            state.entries["memory"][index[0].0].id, key,
            "the write is seen"
        );
    }

    /// A generator of words a store holds: several hundred pseudo-words,
    /// drawn with a skew so a few are common and most are rare, and English
    /// words whose stems match.
    pub(crate) struct Prose {
        state: u64,
        vocabulary: Vec<String>,
    }

    impl Prose {
        pub(crate) fn new(seed: u64) -> Self {
            const SYLLABLES: [&str; 24] = [
                "ka", "lo", "mi", "sen", "tor", "va", "rel", "dun", "pi", "gra", "shu", "ne", "bo",
                "qua", "zet", "ri", "mon", "fa", "lis", "cu", "dra", "ho", "wen", "ty",
            ];
            const ENGLISH: [&str; 24] = [
                "deploy",
                "deploys",
                "deployed",
                "deploying",
                "parser",
                "parsing",
                "build",
                "builds",
                "staging",
                "stage",
                "release",
                "released",
                "token",
                "tokens",
                "review",
                "reviewed",
                "máy",
                "in",
                "số",
                "lỗi",
                "cargo",
                "test",
                "tests",
                "fix",
            ];
            let mut vocabulary = ENGLISH.map(str::to_owned).to_vec();
            for first in SYLLABLES {
                for second in SYLLABLES {
                    vocabulary.push(format!("{first}{second}"));
                }
            }
            Self {
                state: seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1),
                vocabulary,
            }
        }

        pub(crate) fn next(&mut self) -> u64 {
            self.state = self
                .state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.state >> 33
        }

        /// A number below `bound`.
        pub(crate) fn below(&mut self, bound: usize) -> usize {
            usize::try_from(self.next()).unwrap_or_default() % bound.max(1)
        }

        /// `count` words, the first of the vocabulary drawn far more often.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_precision_loss,
            clippy::cast_sign_loss,
            reason = "a skewed draw"
        )]
        pub(crate) fn words(&mut self, count: usize) -> String {
            (0..count)
                .map(|_| {
                    let unit = self.below(1_000_000) as f64 / 1_000_000.0;
                    let index = (unit.powi(3) * self.vocabulary.len() as f64) as usize;
                    self.vocabulary[index.min(self.vocabulary.len() - 1)].clone()
                })
                .collect::<Vec<_>>()
                .join(" ")
        }
    }

    /// A store of `count` memory entries of generated prose, written in one
    /// save.
    pub(crate) fn generated_store(dir: &Path, count: usize, seed: u64) {
        let mut prose = Prose::new(seed);
        let mut records = serde_json::Map::new();
        for index in 0..count {
            let title_words = 3 + prose.below(5);
            let title = prose.words(title_words);
            let content_words = 15 + prose.below(40);
            let content = prose.words(content_words);
            let path = format!("area{}/topic{}", prose.below(7), prose.below(11));
            let mut entry = json!({"id": format!("e{index}"), "title": title, "content": content, "path": path, "version": 1});
            if index % 5 == 0 {
                entry["metadata"] = json!({"type": prose.words(1)});
            }
            records.insert(format!("e{index}"), entry);
        }
        let state = json!({"schema": 1, "entries": {"memory": records}, "refinements": []});
        memory_store::save_document(
            dir,
            &state,
            &memory_store::Writer::new("model", &json!({"via": "bench"}), "create"),
        )
        .expect("save the generated store");
    }

    /// What ranking costs a turn on a large store: the digest and a memory
    /// search, each run three times on one loaded state, over 10,000 and
    /// 50,000 entries. Run with `cargo test --release -p harness-cli --bin ha
    /// -- --ignored ranking_cost --nocapture`.
    #[test]
    #[ignore = "a timing, not a check"]
    fn ranking_cost_over_large_stores() {
        for count in [10_000, 50_000] {
            let directory = tempfile::tempdir().expect("tempdir");
            let dir = directory.path().join("harness");
            generated_store(&dir, count, 7);
            let started = std::time::Instant::now();
            let state = harness::load(&dir, "global");
            let load = started.elapsed();
            let terms = harness::weighted_terms(
                Some("fix the deploy of kalo build before staging"),
                &["the parser tokens fail in review", "máy in số bị lỗi"],
            );
            let options = harness::DigestOptions {
                repl: false,
                refine: false,
                memory_tool: true,
            };
            let mut digests = Vec::new();
            for _ in 0..3 {
                let started = std::time::Instant::now();
                let digest = harness::render_digest(&state, options, &terms);
                digests.push(started.elapsed());
                assert!(!digest.shown.is_empty());
            }
            let entries = state.entries.get("memory").cloned().unwrap_or_default();
            let doc = Value::Null;
            let placed = entries
                .iter()
                .map(|entry| Placed {
                    entry,
                    dir: Path::new(""),
                    doc: &doc,
                })
                .collect::<Vec<_>>();
            let mut searches = Vec::new();
            for _ in 0..3 {
                let started = std::time::Instant::now();
                let result = search(
                    &placed,
                    &json!({"query": "deploying the parser to staging"}),
                    &|entry| json!({"id": entry.id}),
                )
                .expect("search");
                searches.push(started.elapsed());
                assert!(result["total"].as_u64().unwrap_or_default() > 0);
            }
            eprintln!(
                "{count} entries, {} terms: load {load:?}; digest {digests:?}; search {searches:?}",
                terms.len()
            );
        }
    }
}
