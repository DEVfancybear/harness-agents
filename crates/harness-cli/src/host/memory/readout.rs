//! The memory readout (`docs/architecture/MEMORY_READOUT.en.md`): MEMORILLA's
//! query-conditioned read over every memory entry ([`harness_types::readout`])
//! on the vectors the memory store keeps, for the digest every turn carries
//! and for the `memory` tool's `readout` mode.
//!
//! The digest ranked entries by the request's words alone, so an entry that
//! shares no word with the request - the user's anaemia, for a question about
//! what to eat before a long run - never reached the turn. With embeddings
//! configured, the readout recalls it by meaning, and the digest fuses the two
//! rankings, as the paper's analysis suggests (retrieval and the latent read
//! answer different questions; their union beats either).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harness_types::readout;

use super::harness::Entry;
use super::knowledge::embedding;
use super::memory_store;

/// How the entries were read: by the embeddings the user configured, or by
/// their words when none are.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Encoder {
    Embeddings,
    Words,
}

impl Encoder {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Embeddings => "embeddings",
            Self::Words => "words",
        }
    }
}

/// A readout of `entries` (each with the store directory that keeps it) for
/// `query`, in at most `slots` slots, each entry recalled by one slot only
/// ([`readout::recall`]). With embeddings configured it reads their vectors,
/// embedding the query; otherwise, when `words` allows it, TF-IDF vectors of
/// the entries' words. `None` when there is nothing to read with.
///
/// # Errors
/// The embedding configuration or a store cannot be read, or the query cannot
/// be embedded.
pub fn recall(
    entries: &[(&Entry, &Path)],
    embedding_root: &Path,
    query: &str,
    slots: usize,
    words: bool,
) -> Result<Option<(Encoder, Vec<readout::Recalled>)>, String> {
    if entries.is_empty() || query.trim().is_empty() {
        return Ok(None);
    }
    let config = embedding::load_config(embedding_root)?;
    let stored = if config.enabled {
        let namespace = embedding::namespace_of(&config)?;
        Some(stored_vectors(entries, &namespace)?)
            .filter(|stored| stored.iter().any(Option::is_some))
    } else {
        None
    };
    let (encoder, vectors, query_vector) = match stored {
        Some(stored) => {
            let query_vector = embedding::embed_sync(&config, &[query.to_owned()])?
                .into_iter()
                .next()
                .ok_or("the embedding endpoint returned no vector")?;
            let vectors = stored.into_iter().map(Option::unwrap_or_default).collect();
            (Encoder::Embeddings, vectors, query_vector)
        }
        None if words => {
            let terms =
                |text: &str| super::harness::query_terms(&super::knowledge::store::fold(text));
            let documents = entries
                .iter()
                .map(|(entry, _)| {
                    terms(&format!(
                        "{} {}",
                        entry.title.as_deref().unwrap_or_default(),
                        entry.content.as_deref().unwrap_or_default()
                    ))
                })
                .collect::<Vec<_>>();
            let (vectors, query_vector) = readout::word_vectors(&documents, &terms(query));
            (Encoder::Words, vectors, query_vector)
        }
        None => return Ok(None),
    };
    Ok(Some((
        encoder,
        readout::recall(&vectors, &query_vector, slots),
    )))
}

/// Each entry's vector in `namespace` for what it says now, when it has one.
fn stored_vectors(
    entries: &[(&Entry, &Path)],
    namespace: &str,
) -> Result<Vec<Option<Vec<f32>>>, String> {
    let mut dirs = entries
        .iter()
        .map(|(_, dir)| dir.to_path_buf())
        .collect::<Vec<PathBuf>>();
    dirs.sort();
    dirs.dedup();
    let mut vectors = BTreeMap::new();
    let mut ids = BTreeMap::new();
    for dir in &dirs {
        vectors.insert(dir.clone(), memory_store::vectors(dir, namespace)?);
        ids.insert(dir.clone(), memory_store::ids(dir)?);
    }
    Ok(entries
        .iter()
        .map(|(entry, dir)| {
            let dir = dir.to_path_buf();
            let memory = entry.memory.clone().or_else(|| {
                ids[&dir]
                    .get(&(entry.kind.clone(), entry.id.clone()))
                    .cloned()
            })?;
            vectors[&dir].get(&memory).cloned()
        })
        .collect())
}
/// The entries the digest of one turn recalls for `query`: `(kind, id)` in the
/// order the readout recalled them, for memories and prompt notes kept in
/// `global` (this project's and every project's) and `local`. Nothing without
/// embeddings configured: the digest already ranks by words. The entries not
/// yet embedded are embedded in the background for the next turn.
#[must_use]
pub fn digest_recall(
    global: &Path,
    local: &Path,
    project: Option<&str>,
    embedding_root: &Path,
    query: &str,
    slots: usize,
) -> Vec<(String, String)> {
    let Ok(config) = embedding::load_config(embedding_root) else {
        return Vec::new();
    };
    if !config.enabled {
        return Vec::new();
    }
    super::memory_search::embed_soon(
        vec![global.to_path_buf(), local.to_path_buf()],
        embedding_root,
    );
    let global_state = super::harness::for_project(super::harness::load(global, "global"), project);
    let local_state = super::harness::load(local, "local");
    // The digest renames a local entry whose id a global one holds.
    let global_ids = global_state
        .entries
        .values()
        .flatten()
        .map(|entry| (entry.kind.clone(), entry.id.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    let mut entries = Vec::new();
    for (state, dir) in [(&global_state, global), (&local_state, local)] {
        for kind in ["memory", "prompt"] {
            for entry in state.entries.get(kind).into_iter().flatten() {
                if !entry.ended && !entry.pinned && entry.content.is_some() {
                    entries.push((entry.clone(), dir.to_path_buf()));
                }
            }
        }
    }
    let placed = entries
        .iter()
        .map(|(entry, dir)| (entry, dir.as_path()))
        .collect::<Vec<_>>();
    match recall(&placed, embedding_root, query, slots, false) {
        Ok(Some((_, slots))) => slots
            .iter()
            .flat_map(|slot| slot.documents.iter())
            .map(|(index, _)| {
                let (entry, dir) = &entries[*index];
                let id = if dir.as_path() == local
                    && global_ids.contains(&(entry.kind.clone(), entry.id.clone()))
                {
                    format!("local:{}", entry.id)
                } else {
                    entry.id.clone()
                };
                (entry.kind.clone(), id)
            })
            .collect(),
        Ok(None) => Vec::new(),
        Err(error) => {
            harness_types::diag::warn("memory", "readout_failed")
                .field("error", error)
                .emit();
            Vec::new()
        }
    }
}

/// The slots a turn's digest reads: MEMORILLA's `K`. The digest still shows
/// at most its six entries of each kind, so this bounds what it recalls.
pub const DIGEST_SLOTS: usize = 8;

/// [`digest_recall`] off the async runtime: embedding the request is a
/// network call.
pub async fn digest_recall_async(
    global: PathBuf,
    local: PathBuf,
    project: Option<String>,
    embedding_root: PathBuf,
    query: String,
) -> Vec<(String, String)> {
    tokio::task::spawn_blocking(move || {
        digest_recall(
            &global,
            &local,
            project.as_deref(),
            &embedding_root,
            &query,
            DIGEST_SLOTS,
        )
    })
    .await
    .unwrap_or_default()
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

    /// Without embeddings, the words still read: TF-IDF vectors, so a rare
    /// shared word draws the slot more than a common one.
    #[test]
    fn words_read_when_no_embeddings_are_configured() {
        let directory = tempfile::tempdir().expect("dir");
        let entries = [
            entry(
                "a",
                "Deploy day",
                "Deploys go out on Fridays after the freeze.",
            ),
            entry("b", "Queue", "The order queue runs on Redis Streams."),
            entry("c", "Language", "Answer in Vietnamese."),
        ];
        let placed = entries
            .iter()
            .map(|entry| (entry, directory.path()))
            .collect::<Vec<_>>();
        let (encoder, slots) = recall(
            &placed,
            directory.path(),
            "which day do deploys go out",
            4,
            true,
        )
        .expect("readout")
        .expect("words");
        assert_eq!(encoder, Encoder::Words);
        assert_eq!(slots[0].documents[0].0, 0, "{slots:?}");
        assert!(
            recall(&placed, directory.path(), "x", 4, false)
                .expect("readout")
                .is_none(),
            "no words fallback asked, no embeddings: nothing"
        );
    }
}
