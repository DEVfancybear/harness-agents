//! The memory readout (`docs/architecture/MEMORY_READOUT.en.md`): MEMORILLA's
//! query-conditioned read over every memory entry ([`harness_types::readout`])
//! on TF-IDF vectors of the entries' words, for the `memory` tool's `readout`
//! mode. Each slot reads every entry; a later slot reads what the earlier ones
//! left of the request, so the slots together answer a request whose parts
//! sit in different entries.

use harness_types::readout;

use super::harness::Entry;

/// A readout of `entries` for `query`, in at most `slots` slots, each entry
/// recalled by one slot only ([`readout::recall`]), over TF-IDF vectors of
/// their words. Empty when there is nothing to read or nothing to read for.
#[must_use]
pub fn recall(entries: &[&Entry], query: &str, slots: usize) -> Vec<readout::Recalled> {
    if entries.is_empty() || query.trim().is_empty() {
        return Vec::new();
    }
    let terms = |text: &str| super::harness::query_terms(&super::knowledge::store::fold(text));
    let documents = entries
        .iter()
        .map(|entry| {
            terms(&format!(
                "{} {}",
                entry.title.as_deref().unwrap_or_default(),
                entry.content.as_deref().unwrap_or_default()
            ))
        })
        .collect::<Vec<_>>();
    let (vectors, query_vector) = readout::word_vectors(&documents, &terms(query));
    readout::recall(&vectors, &query_vector, slots)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, title: &str, content: &str) -> Entry {
        let state = super::super::harness::parse(
            &serde_json::json!({"schema": 1, "entries": {"memory": {id: {"title": title, "content": content}}}}),
            "global",
            "/global",
        );
        state.entries["memory"][0].clone()
    }

    /// The words read: TF-IDF vectors, so a rare shared word draws the slot
    /// more than a common one.
    #[test]
    fn a_readout_reads_by_words() {
        let entries = [
            entry(
                "a",
                "Deploy day",
                "Deploys go out on Fridays after the freeze.",
            ),
            entry("b", "Queue", "The order queue runs on Redis Streams."),
            entry("c", "Language", "Answer in Vietnamese."),
        ];
        let refs = entries.iter().collect::<Vec<_>>();
        let slots = recall(&refs, "which day do deploys go out", 4);
        assert_eq!(slots[0].documents[0].0, 0, "{slots:?}");
        assert!(recall(&refs, "  ", 4).is_empty());
    }
}
