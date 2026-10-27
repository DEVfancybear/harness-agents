//! The one learned skill that fits a request, in the turn's context.
//!
//! LIFE-HARNESS (arXiv 2605.22166) retrieves skills for each task by BM25 and
//! inserts only the top one, so a skill that does not fit never crowds the
//! context; without the layer its agents lost up to 17% of their accuracy.
//! ha listed its learned skills by name and left it to the model to activate
//! one. Now the request is scored against every learned skill's name,
//! description and body, and the best goes into the turn - only when its
//! score stands out from the rest, judged by the scores' own spread, so a
//! request that fits none gets none. What the skill took for granted in the
//! turns it failed goes with it: those assumptions were kept, and never read
//! when the skill was used again.

use std::collections::{BTreeMap, BTreeSet};

use harness_session::{ContextBlock, ContextBlockKind};
use harness_types::readout::{STANDOUT, STATISTIC_FROM, words};
use harness_types::search::bm25_idf;

use super::learned::{Layers, LearnedSkill};

/// BM25's term-frequency saturation and length normalization.
const K1: f64 = 1.2;
const B: f64 = 0.75;

/// The most of a skill's body a turn carries.
const MAX_BODY_CHARS: usize = 4_000;

/// A learned skill recalled for a request.
#[derive(Clone, Debug, PartialEq)]
pub struct Recalled {
    pub name: String,
    pub body: String,
    pub failures: Vec<String>,
    pub score: f64,
}

/// The learned skill that fits `request`, if one stands out.
#[must_use]
pub fn recall(layers: &Layers, skills: &[LearnedSkill], request: &str) -> Option<Recalled> {
    let bodies = skills
        .iter()
        .map(|skill| {
            layers
                .root(skill.layer)
                .and_then(|root| {
                    std::fs::read_to_string(root.join(&skill.name).join("SKILL.md")).ok()
                })
                .map(|document| without_front_matter(&document).to_owned())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    let documents = skills
        .iter()
        .zip(&bodies)
        .map(|(skill, body)| {
            words(&format!(
                "{} {} {body}",
                skill.name.replace(['-', '_'], " "),
                skill.description
            ))
        })
        .collect::<Vec<_>>();
    let (best, score) = best_of(&documents, &words(request))?;
    let skill = &skills[best];
    let mut body = bodies[best].trim().to_owned();
    if body.chars().count() > MAX_BODY_CHARS {
        body = body.chars().take(MAX_BODY_CHARS).collect::<String>();
        body.push_str("\n…(cut; activate_skill reads it whole)");
    }
    Some(Recalled {
        name: skill.name.clone(),
        body,
        failures: skill.failures.clone(),
        score,
    })
}

/// The index and BM25 score of the document that stands out for the query.
///
/// With enough documents for a spread to mean something, the best must lie
/// [`STANDOUT`] deviations above the mean score; with fewer, it must match at
/// least two of the query's words, so one shared word is not a fit. Either
/// way it must beat the next one.
fn best_of(documents: &[Vec<String>], query: &[String]) -> Option<(usize, f64)> {
    let query = query.iter().collect::<BTreeSet<_>>();
    if documents.is_empty() || query.is_empty() {
        return None;
    }
    #[allow(clippy::cast_precision_loss, reason = "word counts are small")]
    let average = documents.iter().map(Vec::len).sum::<usize>() as f64 / documents.len() as f64;
    let holding = |term: &String| {
        documents
            .iter()
            .filter(|document| document.contains(term))
            .count() as u64
    };
    let idf = query
        .iter()
        .map(|term| (*term, bm25_idf(documents.len() as u64, holding(term))))
        .collect::<BTreeMap<_, _>>();
    let scored = documents
        .iter()
        .map(|document| {
            let mut counts = BTreeMap::<&String, f64>::new();
            for word in document {
                *counts.entry(word).or_default() += 1.0;
            }
            #[allow(clippy::cast_precision_loss, reason = "word counts are small")]
            let length = document.len() as f64;
            let mut score = 0.0;
            let mut matched = 0;
            for term in &query {
                if let Some(frequency) = counts.get(term) {
                    matched += 1;
                    score += idf[term] * frequency * (K1 + 1.0)
                        / (frequency + K1 * (1.0 - B + B * length / average.max(1.0)));
                }
            }
            (score, matched)
        })
        .collect::<Vec<_>>();
    let mut order = (0..scored.len()).collect::<Vec<_>>();
    order.sort_by(|left, right| scored[*right].0.total_cmp(&scored[*left].0));
    let best = order[0];
    let (score, matched) = scored[best];
    if score <= 0.0 {
        return None;
    }
    if order.len() > 1 && scored[order[1]].0 >= score {
        return None;
    }
    if scored.len() >= STATISTIC_FROM {
        #[allow(clippy::cast_precision_loss, reason = "document counts are small")]
        let count = scored.len() as f64;
        let mean = scored.iter().map(|(score, _)| score).sum::<f64>() / count;
        let deviation = (scored
            .iter()
            .map(|(score, _)| (score - mean).powi(2))
            .sum::<f64>()
            / count)
            .sqrt();
        (score >= mean + STANDOUT * deviation).then_some((best, score))
    } else {
        (matched >= 2).then_some((best, score))
    }
}

fn without_front_matter(document: &str) -> &str {
    let Some(rest) = document.strip_prefix("---") else {
        return document;
    };
    rest.find("\n---").map_or(document, |end| {
        rest[end + 4..].trim_start_matches(['\r', '\n'])
    })
}

/// The block that carries a recalled skill into the turn.
#[must_use]
pub fn block(recalled: &Recalled) -> ContextBlock {
    let mut text = format!(
        "A learned skill fits this request: {}. Its instructions follow; follow them where they fit, and say so when they do not.\n<skill name=\"{}\">\n{}\n</skill>",
        recalled.name, recalled.name, recalled.body
    );
    if !recalled.failures.is_empty() {
        text.push_str("\nWhen it was followed before and failed, it had taken for granted:");
        for failure in &recalled.failures {
            text.push_str("\n- ");
            text.push_str(failure);
        }
        text.push_str("\nCheck those before relying on it.");
    }
    ContextBlock::mandatory("recalled-skill", ContextBlockKind::Instruction, text)
}

#[cfg(test)]
mod tests {
    use super::{best_of, without_front_matter};
    use harness_types::readout::words;

    fn docs(texts: &[&str]) -> Vec<Vec<String>> {
        texts.iter().map(|text| words(text)).collect()
    }

    #[test]
    fn the_skill_that_fits_is_recalled_and_none_when_none_fits() {
        let documents = docs(&[
            "deploy release staging run the deploy script then check the release",
            "database migration write a migration and run it against the database",
            "changelog write the changelog entry for a release",
        ]);
        let (best, _) = best_of(&documents, &words("run the database migration")).expect("fits");
        assert_eq!(best, 1);
        assert!(best_of(&documents, &words("paint the kitchen wall")).is_none());
        // One shared word among few skills is not a fit.
        assert!(best_of(&documents, &words("check my spelling")).is_none());
    }

    #[test]
    fn among_many_skills_the_best_must_stand_out() {
        let mut texts = vec![
            "build check run the build and report",
            "lint check run the linter and report",
            "test check run the tests and report",
            "format check run the formatter and report",
            "audit check run the audit and report",
        ];
        // Every skill matches "check run report": none stands out.
        assert!(best_of(&docs(&texts), &words("check run report")).is_none());
        texts.push("flaky test triage rerun the failing test alone and bisect the seed");
        let (best, _) = best_of(&docs(&texts), &words("the test is flaky, bisect the seed"))
            .expect("stands out");
        assert_eq!(best, 5);
    }

    #[test]
    fn the_front_matter_is_not_the_body() {
        assert_eq!(
            without_front_matter("---\nname: x\ndescription: y\n---\n# Body\nstep"),
            "# Body\nstep"
        );
        assert_eq!(without_front_matter("# Body"), "# Body");
    }
}
