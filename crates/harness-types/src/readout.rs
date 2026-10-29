//! A query-conditioned memory readout, after MEMORILLA ("Latent semantic
//! memory for LLMs", arXiv 2610.21375).
//!
//! MEMORILLA encodes every document once into a store of embeddings, then for
//! each query lets `K` learned slots, conditioned on the query, attend over the
//! whole store through two cross-attention blocks, and hands a frozen LLM
//! exactly `K` soft tokens: a continuous read of everything rather than a
//! discrete top-k selection, at a budget that does not grow with the store.
//!
//! ha's models take text, and nothing here is trained, so this keeps what
//! carries over: every slot reads the whole store by softmax attention over
//! cosine similarity; each slot is conditioned on the query and refined by
//! what it read ([`ROUNDS`] blocks, as the paper's two); the slots keep
//! distinct identities by each reading what the slots before it left
//! unexplained (the query's residual after their reads), where MEMORILLA's
//! learned initial slots differ by training; and the output is a fixed number
//! of slots whatever the size of the store. A caller renders each slot as the
//! documents that carry most of its attention.

/// Cross-attention blocks per slot: MEMORILLA's `L_ref` is two.
pub const ROUNDS: usize = 2;
/// How sharply a slot attends: cosine similarities are divided by this before
/// the softmax, so a document 0.1 more similar weighs `e^2` (about 7) times
/// more. A fixed number, not a learned one.
pub const TEMPERATURE: f64 = 0.05;
/// A slot ends the readout once less than this share of the query is left
/// unexplained: the rest would read noise.
pub const RESIDUAL_FLOOR: f64 = 0.2;

/// One slot of a readout: how its attention spread over the documents, the
/// largest first, and its similarity to the query.
#[derive(Clone, Debug, PartialEq)]
pub struct Slot {
    /// `(document index, attention weight)`, weights summing to 1.
    pub attention: Vec<(usize, f64)>,
    /// Cosine similarity of the slot's read to the query.
    pub relevance: f64,
    /// What of the query the slot read for, of unit length: the whole query
    /// for the first slot, what the earlier slots left of it for a later one.
    pub condition: Vec<f64>,
    /// How much of the query the slot's read explains: the length of what it
    /// took off the residual, the query being of unit length.
    pub explained: f64,
}

impl Slot {
    /// The documents that carry `share` of the slot's attention, at most
    /// `most`, the largest first.
    #[must_use]
    pub fn carriers(&self, share: f64, most: usize) -> Vec<(usize, f64)> {
        let mut carried = 0.0;
        let mut out = Vec::new();
        for (document, weight) in &self.attention {
            if out.len() >= most || carried >= share {
                break;
            }
            carried += weight;
            out.push((*document, *weight));
        }
        out
    }
}

/// Read `documents` for `query` into at most `slots` slots. Vectors of any
/// dimension; one that differs from the query's, or is zero, is never read.
/// Returns no slot for an empty store or a zero query.
#[must_use]
pub fn readout(documents: &[Vec<f32>], query: &[f32], slots: usize) -> Vec<Slot> {
    let Some(query) = unit(
        &query
            .iter()
            .map(|value| f64::from(*value))
            .collect::<Vec<_>>(),
    ) else {
        return Vec::new();
    };
    let store = documents
        .iter()
        .enumerate()
        .filter(|(_, document)| document.len() == query.len())
        .filter_map(|(index, document)| {
            unit(
                &document
                    .iter()
                    .map(|value| f64::from(*value))
                    .collect::<Vec<_>>(),
            )
            .map(|vector| (index, vector))
        })
        .collect::<Vec<_>>();
    if store.is_empty() {
        return Vec::new();
    }
    let mut residual = query.clone();
    let mut out = Vec::new();
    while out.len() < slots {
        if norm(&residual) < RESIDUAL_FLOOR {
            break;
        }
        // The slot is conditioned on what of the query is left, then refined
        // by its own read, with the query as its residual connection.
        let Some(condition) = unit(&residual) else {
            break;
        };
        let mut slot = condition.clone();
        let mut attention = Vec::new();
        let mut read = vec![0.0; query.len()];
        for _ in 0..ROUNDS {
            attention = attend(&store, &slot);
            read = mix(&store, &attention);
            let Some(refined) = unit(&add(&condition, &read)) else {
                break;
            };
            slot = refined;
        }
        let Some(read_unit) = unit(&read) else {
            break;
        };
        let relevance = dot(&read_unit, &query);
        // What this slot read is explained: the next one reads the rest.
        let explained = dot(&residual, &read_unit);
        residual = residual
            .iter()
            .zip(&read_unit)
            .map(|(left, right)| left - explained * right)
            .collect();
        let mut ranked = attention
            .into_iter()
            .map(|(position, weight)| (store[position].0, weight))
            .collect::<Vec<_>>();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out.push(Slot {
            attention: ranked,
            relevance,
            condition,
            explained,
        });
        // A slot that explained nothing would repeat itself forever.
        if explained.abs() < 1e-9 {
            break;
        }
    }
    out
}

/// The share of a slot's attention whose documents are recalled, and at most
/// how many: a slot is rendered by the documents that carry most of it.
pub const CARRIED_SHARE: f64 = 0.8;
pub const CARRIERS: usize = 3;
/// A document a slot carries is recalled only when it stands out from the
/// store: its similarity to the query at least this many standard deviations
/// above the mean. A statistic of the store, not a fixed threshold, so it
/// holds whatever the encoder's scale.
pub const STANDOUT: f64 = 1.0;
/// Below this many documents the statistic says little, and every carrier
/// counts.
pub const STATISTIC_FROM: usize = 5;

/// One slot as recalled: its relevance to the query and the documents that
/// carry it, with their attention.
#[derive(Clone, Debug, PartialEq)]
pub struct Recalled {
    pub relevance: f64,
    pub documents: Vec<(usize, f64)>,
}

/// [`readout`], rendered: each slot by the documents that carry most of its
/// attention and stand out from the store for what the slot read for: the
/// whole query for the first slot, the part the earlier ones left for a later
/// one (the second part of a two-part request shares fewer of its words, and
/// the whole query's statistic would drown it). A later slot that explains
/// less than [`RESIDUAL_FLOOR`] of the query read what is left of it after
/// its parts, its common words, and recalls nothing. Each document is
/// recalled by one slot only; a slot left with none is dropped.
#[must_use]
pub fn recall(documents: &[Vec<f32>], query: &[f32], slots: usize) -> Vec<Recalled> {
    let mut taken = std::collections::BTreeSet::new();
    readout(documents, query, slots)
        .iter()
        .enumerate()
        .filter(|(index, slot)| *index == 0 || slot.explained >= RESIDUAL_FLOOR)
        .filter_map(|(_, slot)| {
            let standing = standing_out(documents, &slot.condition, STANDOUT);
            let carried = slot
                .carriers(CARRIED_SHARE, CARRIERS)
                .into_iter()
                .filter(|(index, _)| standing.contains(index) && taken.insert(*index))
                .collect::<Vec<_>>();
            (!carried.is_empty()).then_some(Recalled {
                relevance: slot.relevance,
                documents: carried,
            })
        })
        .collect()
}

/// The documents whose similarity to the query stands out from the store's.
#[must_use]
pub fn standouts(documents: &[Vec<f32>], query: &[f32]) -> std::collections::BTreeSet<usize> {
    standing_out(
        documents,
        &query
            .iter()
            .map(|value| f64::from(*value))
            .collect::<Vec<_>>(),
        STANDOUT,
    )
}

/// The documents at least `deviations` standard deviations above the store's
/// mean similarity to `query`, a query in `f64` as a slot keeps what it read
/// for.
fn standing_out(
    documents: &[Vec<f32>],
    query: &[f64],
    deviations: f64,
) -> std::collections::BTreeSet<usize> {
    let similarities = documents
        .iter()
        .map(|document| {
            let document = document
                .iter()
                .map(|value| f64::from(*value))
                .collect::<Vec<_>>();
            cosine64(&document, query)
        })
        .collect::<Vec<_>>();
    let known = similarities.iter().flatten().copied().collect::<Vec<_>>();
    if known.len() < STATISTIC_FROM {
        return (0..documents.len())
            .filter(|index| similarities[*index].is_some())
            .collect();
    }
    #[allow(clippy::cast_precision_loss, reason = "document counts are small")]
    let count = known.len() as f64;
    let mean = known.iter().sum::<f64>() / count;
    let deviation = (known
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / count)
        .sqrt();
    similarities
        .iter()
        .enumerate()
        .filter(|(_, similarity)| {
            similarity.is_some_and(|similarity| similarity >= mean + deviations * deviation)
        })
        .map(|(index, _)| index)
        .collect()
}

/// Cosine similarity of two vectors of one dimension.
#[must_use]
pub fn cosine(left: &[f32], right: &[f32]) -> Option<f64> {
    let left = left
        .iter()
        .map(|value| f64::from(*value))
        .collect::<Vec<_>>();
    let right = right
        .iter()
        .map(|value| f64::from(*value))
        .collect::<Vec<_>>();
    cosine64(&left, &right)
}

fn cosine64(left: &[f64], right: &[f64]) -> Option<f64> {
    if left.len() != right.len() || left.is_empty() {
        return None;
    }
    let product = norm(left) * norm(right);
    (product > 1e-12).then(|| dot(left, right) / product)
}

/// A text's words for [`word_vectors`]: lower case, runs of letters and
/// digits. No word list: how much a word counts comes from the store.
#[must_use]
pub fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.chars().count() >= 2)
        .map(str::to_owned)
        .collect()
}

/// TF-IDF vectors of the documents' words and of the query's, over the words
/// the documents hold: each word weighed by how rare it is among them. The
/// encoder every readout reads with.
#[must_use]
pub fn word_vectors(documents: &[Vec<String>], query: &[String]) -> (Vec<Vec<f32>>, Vec<f32>) {
    let mut vocabulary = std::collections::BTreeMap::<&str, usize>::new();
    let mut holding = Vec::<usize>::new();
    for words in documents {
        let unique = words
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        for word in unique {
            let next = vocabulary.len();
            let index = *vocabulary.entry(word).or_insert(next);
            if index == holding.len() {
                holding.push(0);
            }
            holding[index] += 1;
        }
    }
    #[allow(clippy::cast_precision_loss, reason = "word counts are small")]
    let count = documents.len() as f64;
    let vectorize = |words: &[String]| {
        let mut vector = vec![0.0_f32; vocabulary.len()];
        for word in words {
            if let Some(index) = vocabulary.get(word.as_str()) {
                #[allow(clippy::cast_precision_loss, reason = "word counts are small")]
                let idf = ((count + 1.0) / (holding[*index] as f64 + 0.5)).ln();
                #[allow(clippy::cast_possible_truncation, reason = "a weight")]
                {
                    vector[*index] += idf as f32;
                }
            }
        }
        vector
    };
    let vectors = documents.iter().map(|words| vectorize(words)).collect();
    (vectors, vectorize(query))
}

/// Softmax attention of `slot` over every document, by store position.
fn attend(store: &[(usize, Vec<f64>)], slot: &[f64]) -> Vec<(usize, f64)> {
    let logits = store
        .iter()
        .map(|(_, document)| dot(document, slot) / TEMPERATURE)
        .collect::<Vec<_>>();
    let top = logits.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let exps = logits
        .iter()
        .map(|logit| (logit - top).exp())
        .collect::<Vec<_>>();
    let sum = exps.iter().sum::<f64>();
    exps.into_iter()
        .enumerate()
        .map(|(position, value)| (position, value / sum))
        .collect()
}

/// The attention-weighted mix of the documents.
fn mix(store: &[(usize, Vec<f64>)], attention: &[(usize, f64)]) -> Vec<f64> {
    let mut read = vec![0.0; store.first().map_or(0, |(_, document)| document.len())];
    for (position, weight) in attention {
        for (value, component) in read.iter_mut().zip(&store[*position].1) {
            *value += weight * component;
        }
    }
    read
}

fn dot(left: &[f64], right: &[f64]) -> f64 {
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

fn norm(vector: &[f64]) -> f64 {
    dot(vector, vector).sqrt()
}

fn add(left: &[f64], right: &[f64]) -> Vec<f64> {
    left.iter().zip(right).map(|(a, b)| a + b).collect()
}

fn unit(vector: &[f64]) -> Option<Vec<f64>> {
    let length = norm(vector);
    (length.is_finite() && length > 1e-12)
        .then(|| vector.iter().map(|value| value / length).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector(values: &[f32]) -> Vec<f32> {
        values.to_vec()
    }

    /// Every slot reads the whole store; the first follows the query, and the
    /// next reads the part of the query the first left unexplained.
    #[test]
    fn slots_read_the_whole_store_and_split_the_query() {
        let documents = vec![
            vector(&[1.0, 0.0, 0.0]),
            vector(&[0.9, 0.1, 0.0]),
            vector(&[0.0, 1.0, 0.0]),
            vector(&[0.0, 0.0, 1.0]),
        ];
        // A query about two things at once, more of the first.
        let slots = readout(&documents, &[0.8, 0.6, 0.0], 4);
        assert!(slots.len() >= 2, "{slots:?}");
        for slot in &slots {
            assert_eq!(slot.attention.len(), 4, "a slot reads every document");
            let mass = slot.attention.iter().map(|(_, weight)| weight).sum::<f64>();
            assert!((mass - 1.0).abs() < 1e-9);
        }
        let first = slots[0].carriers(0.9, 3);
        assert!(
            first.iter().any(|(document, _)| *document <= 1),
            "{first:?}"
        );
        let second = slots[1].carriers(0.9, 3);
        assert!(
            second.iter().any(|(document, _)| *document == 2),
            "the second slot reads what the first left: {second:?}"
        );
        assert!(
            slots.iter().all(|slot| slot.carriers(0.5, 1)[0].0 != 3),
            "nothing reads the unrelated document first"
        );
    }

    #[test]
    fn a_fixed_budget_whatever_the_store() {
        let documents = (0..500)
            .map(|index| {
                #[allow(clippy::cast_precision_loss, reason = "a small test index")]
                let angle = index as f32 / 80.0;
                vector(&[angle.cos(), angle.sin(), 0.1])
            })
            .collect::<Vec<_>>();
        let slots = readout(&documents, &[1.0, 0.2, 0.0], 8);
        assert!(!slots.is_empty() && slots.len() <= 8);
    }

    /// A document stands out only above the store's similarities, whatever
    /// their scale; a slot recalls only those, each once.
    #[test]
    fn a_document_stands_out_only_above_the_store() {
        let documents = (0..8)
            .map(|index| {
                if index == 3 {
                    vector(&[1.0, 0.0])
                } else {
                    vector(&[0.0, 1.0])
                }
            })
            .collect::<Vec<_>>();
        let standing = standouts(&documents, &[1.0, 0.1]);
        assert_eq!(standing.into_iter().collect::<Vec<_>>(), vec![3]);
        let recalled = recall(&documents, &[1.0, 0.1], 4);
        assert_eq!(recalled.len(), 1, "{recalled:?}");
        assert_eq!(recalled[0].documents[0].0, 3);
    }

    /// A later slot's document stands out for the part of the request that
    /// slot read for, though not for the whole request: the second part of a
    /// two-part request shares fewer of its words.
    #[test]
    fn a_later_slot_judges_by_the_part_it_read_for() {
        let documents = [
            words("Deploy day Deploys go out on Fridays"),
            words("Spring sale SPRING10 takes ten percent off any order"),
            words("Queue The team moved to NATS JetStream"),
            words("JWT keys rotate every 14 days"),
            words("Security audit The yearly security audit is due mid November"),
            words(
                "Billing release The billing service was deployed on Friday by the payments team",
            ),
            words("Health The user has iron deficiency anaemia they avoid tea with meals"),
        ];
        let (vectors, query) = word_vectors(
            &documents,
            &words("the user health and anaemia and which day deploys go out"),
        );
        assert!(!standouts(&vectors, &query).contains(&6), "the premise");
        let recalled = recall(&vectors, &query, 4)
            .iter()
            .flat_map(|slot| slot.documents.iter().map(|(index, _)| *index))
            .collect::<Vec<_>>();
        assert!(
            recalled.contains(&0) && recalled.contains(&6),
            "{recalled:?}"
        );
        assert_eq!(recalled.len(), 2, "{recalled:?}");
    }

    #[test]
    fn words_weigh_by_rarity() {
        let documents = [
            words("Deploys go out on Fridays"),
            words("The queue runs on Redis"),
            words("Answer in Vietnamese"),
        ];
        let (vectors, query) = word_vectors(&documents, &words("when do deploys go out"));
        assert!(cosine(&vectors[0], &query).unwrap_or(0.0) > 0.5);
        assert!(cosine(&vectors[2], &query).is_none_or(|similarity| similarity < 0.1));
    }

    #[test]
    fn nothing_to_read_reads_nothing() {
        assert!(readout(&[], &[1.0, 0.0], 4).is_empty());
        assert!(readout(&[vector(&[1.0, 0.0])], &[0.0, 0.0], 4).is_empty());
        assert!(readout(&[vector(&[1.0, 0.0, 0.0])], &[1.0, 0.0], 4).is_empty());
    }
}
