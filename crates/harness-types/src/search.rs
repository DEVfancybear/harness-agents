//! What every search tool does with a query of several words.
//!
//! A model asks one search for several things at once (`world-scene
//! game-config world-canvas`) or puts one word differently from the text. A
//! search that needed every word answered "0 found" to both; `find_files`
//! then dropped one word at a time and still found nothing. Every search
//! (`find_files`, `search_text`, `list_skills`, `history_search`) follows one
//! rule instead:
//!
//! - when some items hold every word, those are the answer;
//! - else the items holding the most words come first, and the answer names
//!   the words no item holds, so the model knows which part found nothing.

use std::collections::{BTreeMap, BTreeSet};

/// How much of a query one item holds: the words it matches, then the
/// search's own score for them. Ordered the way an answer is: more words
/// first, then the higher score.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Coverage {
    pub matched: u32,
    pub score: u64,
}

/// A search's items for a query's words, under the rule above.
#[derive(Debug)]
pub struct WordMatches<T> {
    /// The items, unsorted: each search breaks ties its own way, after the
    /// coverage.
    pub hits: Vec<(Coverage, T)>,
    /// Whether the hits hold every word.
    pub complete: bool,
    /// The words no item holds.
    pub unmatched: Vec<String>,
}

impl<T> WordMatches<T> {
    /// The hits in answer order, for a search with no tie-break of its own:
    /// equal hits keep the order they came in.
    pub fn most_first(&mut self) {
        self.hits
            .sort_by_key(|(coverage, _)| std::cmp::Reverse(*coverage));
    }

    /// The note an incomplete answer carries, naming what `what` are
    /// (`files`, `lines`, `skills`); `None` when every word was found
    /// together or nothing was found.
    #[must_use]
    pub fn note(&self, what: &str, words: &[String]) -> Option<String> {
        (!self.complete && !self.hits.is_empty())
            .then(|| partial_note(what, words, &self.unmatched))
    }
}

/// Match `items` against `words`: `score` is one item's score for one word,
/// `None` where the item does not hold it.
pub fn match_words<T>(
    items: impl IntoIterator<Item = T>,
    words: &[String],
    score: impl Fn(&T, &str) -> Option<u64>,
) -> WordMatches<T> {
    let mut found = vec![false; words.len()];
    let mut hits = items
        .into_iter()
        .filter_map(|item| {
            let mut coverage = Coverage::default();
            for (index, word) in words.iter().enumerate() {
                if let Some(points) = score(&item, word) {
                    coverage.matched += 1;
                    coverage.score += points;
                    found[index] = true;
                }
            }
            (coverage.matched > 0).then_some((coverage, item))
        })
        .collect::<Vec<_>>();
    let every = u32::try_from(words.len()).unwrap_or(u32::MAX);
    let complete = hits.iter().any(|(coverage, _)| coverage.matched == every);
    if complete {
        hits.retain(|(coverage, _)| coverage.matched == every);
    }
    WordMatches {
        hits,
        complete,
        unmatched: words
            .iter()
            .zip(found)
            .filter(|(_, found)| !found)
            .map(|(word, _)| word.clone())
            .collect(),
    }
}

/// `0 files match all of 'a b c'; files matching the most of its words; no
/// files match 'c':`
#[must_use]
pub fn partial_note(what: &str, words: &[String], unmatched: &[String]) -> String {
    let missing = if unmatched.is_empty() {
        String::new()
    } else {
        format!("; no {what} match '{}'", unmatched.join("', '"))
    };
    format!(
        "0 {what} match all of '{}'; {what} matching the most of its words{missing}:",
        words.join(" ")
    )
}

/// Fuse two rankings of the same items - by words and by meaning - as
/// `CowAgent` does: each channel's ranks normalized, weighted, and averaged
/// over the channels that returned the item. Knowledge and history search
/// both rank this way.
#[allow(
    clippy::cast_precision_loss,
    reason = "Bounded ranks have at most 2048 entries"
)]
#[must_use]
pub fn hybrid_fuse(
    lexical: &[(String, f64)],
    vectors: &[(String, f64)],
    vector_weight: f64,
    keyword_weight: f64,
    min_score: Option<f64>,
) -> Vec<(String, f64)> {
    if !vector_weight.is_finite()
        || !keyword_weight.is_finite()
        || vector_weight < 0.0
        || keyword_weight < 0.0
        || vector_weight + keyword_weight <= 0.0
    {
        return Vec::new();
    }
    let mut scores = BTreeMap::<String, (f64, f64)>::new();
    for (channel, weight) in [(lexical, keyword_weight), (vectors, vector_weight)] {
        let mut seen = BTreeSet::new();
        let unique = channel
            .iter()
            .filter(|(path, score)| score.is_finite() && seen.insert(path))
            .collect::<Vec<_>>();
        let count = unique.len();
        for (rank, (path, _)) in unique.into_iter().enumerate() {
            let normalized = (count - rank) as f64 / count as f64;
            let entry = scores.entry(path.clone()).or_default();
            entry.0 += normalized * weight;
            entry.1 += weight;
        }
    }
    let minimum = min_score.filter(|score| score.is_finite()).unwrap_or(0.0);
    let mut hits = scores
        .into_iter()
        .filter_map(|(path, (score, weight))| {
            let score = if weight > 0.0 { score / weight } else { 0.0 };
            (weight > 0.0 && score >= minimum).then_some((path, score))
        })
        .collect::<Vec<_>>();
    hits.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    hits
}

/// BM25's inverse document frequency of a term that `matching` of
/// `documents` hold: a rare term weighs more than a common one.
#[must_use]
pub fn bm25_idf(documents: u64, matching: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)] // counts far below 2^52
    let (documents, matching) = (documents as f64, matching as f64);
    (1.0 + (documents - matching + 0.5) / (matching + 0.5)).ln()
}

#[cfg(test)]
mod tests {
    use super::{Coverage, bm25_idf, match_words};

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    fn holds(item: &&str, word: &str) -> Option<u64> {
        item.contains(word).then_some(1)
    }

    #[test]
    fn items_holding_every_word_are_the_answer() {
        let items = ["config loader", "config", "loader"];
        let found = match_words(items, &words("config loader"), holds);
        assert!(found.complete);
        assert_eq!(found.hits.len(), 1);
        assert_eq!(found.hits[0].1, "config loader");
        assert!(found.note("files", &words("config loader")).is_none());
    }

    #[test]
    fn else_the_items_holding_the_most_words_and_the_words_none_hold() {
        let items = ["world-scene.ts", "game-config.ts", "world.ts", "readme"];
        let query = words("world-scene game-config nginx");
        let mut found = match_words(items, &query, holds);
        assert!(!found.complete);
        found.most_first();
        let names = found.hits.iter().map(|(_, item)| *item).collect::<Vec<_>>();
        assert_eq!(names, ["world-scene.ts", "game-config.ts"]);
        assert_eq!(found.unmatched, ["nginx"]);
        assert_eq!(
            found.note("files", &query).as_deref(),
            Some(
                "0 files match all of 'world-scene game-config nginx'; files matching the most of its words; no files match 'nginx':"
            )
        );
    }

    #[test]
    fn more_words_outrank_a_higher_score() {
        let two = Coverage {
            matched: 2,
            score: 2,
        };
        let one = Coverage {
            matched: 1,
            score: 100,
        };
        assert!(two > one);
    }

    #[test]
    fn a_rare_term_weighs_more() {
        assert!(bm25_idf(1000, 2) > bm25_idf(1000, 300));
        assert!(bm25_idf(10, 10) > 0.0);
    }
}
