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
//!
//! From Evolver (arXiv 2611.07595), whose reused strategies cut the tokens of
//! tasks that already succeeded by 17% and which warns that an outdated one
//! costs more than none:
//! - a skill that fails more than it works since it was last written is not
//!   recalled (its failure-based restriction), until a patch tries it afresh;
//! - one nothing has shown to work is said to be unproven, not handed over as
//!   if it were validated;
//! - the words of a turn it worked in go with it (its Capsule: a concrete
//!   case beside the strategy).

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
    /// The words of the turns it worked in, newest last.
    pub cases: Vec<String>,
    /// Nothing has shown it to work yet.
    pub unproven: bool,
    pub score: f64,
}

/// The learned skill that fits `request`, if one stands out.
#[must_use]
pub fn recall(layers: &Layers, skills: &[LearnedSkill], request: &str) -> Option<Recalled> {
    // A skill held back is not a candidate at all: the next best may fit.
    let skills = skills
        .iter()
        .filter(|skill| !skill.usage.restricted())
        .collect::<Vec<_>>();
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
    let skill = skills[best];
    let mut body = bodies[best].trim().to_owned();
    if body.chars().count() > MAX_BODY_CHARS {
        body = body.chars().take(MAX_BODY_CHARS).collect::<String>();
        body.push_str("\n…(cut; activate_skill reads it whole)");
    }
    Some(Recalled {
        name: skill.name.clone(),
        body,
        failures: skill.failures.clone(),
        cases: skill.cases.clone(),
        unproven: skill.usage.unproven(),
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
    if recalled.unproven {
        text.push_str(
            "\nNothing has shown it to work yet: it is a draft. Check each step's result as you go.",
        );
    }
    if let Some(case) = recalled.cases.last() {
        text.push_str("\nA turn it worked in showed: ");
        text.push_str(case);
    }
    if !recalled.failures.is_empty() {
        text.push_str("\nWhen it was followed before and failed, it had taken for granted:");
        for failure in &recalled.failures {
            text.push_str("\n- ");
            text.push_str(failure);
        }
        text.push_str("\nCheck those before relying on it.");
    }
    ContextBlock::mandatory("recalled-skill", ContextBlockKind::Instruction, text)
        .with_max_tokens(RECALLED_TOKENS)
        .with_rest_at(format!("the learned skill {}", recalled.name))
}

/// The most tokens a recalled skill takes in a turn's context.
const RECALLED_TOKENS: u64 = 1_500;

#[cfg(test)]
mod tests {
    use super::{Recalled, best_of, block, recall, without_front_matter};
    use crate::interactive::learned::{
        Intent, Layers, PromoteContext, library, promote, record_outcome,
    };
    use harness_types::readout::words;
    use serde_json::json;

    fn learned(root: &std::path::Path, skills: &[(&str, &str)]) -> Layers {
        let layers = Layers::new(
            &root.join("config"),
            &root.join("data"),
            &root.join("workspace"),
            true,
        );
        for (name, body) in skills {
            let verdict = promote(
                &layers,
                &Intent::from_value(&json!({
                    "action": "create", "name": name, "level": "project",
                    "body": format!("---\nname: {name}\ndescription: Use it for {name}.\ncategory: ops\n---\n{body}\n"),
                    "reason": "the user showed it", "evidence": "[User]: do it this way",
                })),
                &PromoteContext::default(),
            );
            assert!(verdict.ok, "{:?}", verdict.findings);
        }
        layers
    }

    /// Evolver's failure-based restriction: the skill that fits but keeps
    /// failing is not recalled, and a patch tries it again.
    #[test]
    fn a_skill_that_keeps_failing_is_not_recalled_until_it_is_rewritten() {
        let directory = tempfile::tempdir().expect("dir");
        let layers = learned(
            directory.path(),
            &[
                (
                    "db-migration",
                    "Write the database migration and run it against staging.",
                ),
                (
                    "release-notes",
                    "Write the release notes from the merged changes.",
                ),
            ],
        );
        let request = "run the database migration against staging";
        let first = recall(&layers, &library(&layers), request).expect("fits");
        assert_eq!(first.name, "db-migration");
        assert!(first.unproven && first.cases.is_empty());
        for _ in 0..2 {
            record_outcome(
                &layers,
                "db-migration",
                false,
                "staging was reachable",
                "timeout",
            );
        }
        assert!(
            recall(&layers, &library(&layers), request).is_none(),
            "held back, and the other skill does not fit"
        );
        let patched = promote(
            &layers,
            &Intent::from_value(&json!({
                "action": "patch", "name": "db-migration", "old_string": "and run it",
                "new_string": "check staging answers, and run it",
                "reason": "it timed out", "evidence": "timeout",
            })),
            &PromoteContext::default(),
        );
        assert!(patched.ok, "{:?}", patched.findings);
        let again = recall(&layers, &library(&layers), request).expect("tried afresh");
        assert_eq!(again.name, "db-migration");
        assert_eq!(
            again.failures,
            ["staging was reachable", "staging was reachable"]
        );
    }

    /// Evolver's unproven draft and Capsule, as the turn reads them.
    #[test]
    fn the_block_says_a_skill_is_unproven_and_carries_the_case_it_worked_in() {
        let mut recalled = Recalled {
            name: "deploy".to_owned(),
            body: "Run the deploy script.".to_owned(),
            failures: Vec::new(),
            cases: Vec::new(),
            unproven: true,
            score: 1.0,
        };
        let text = format!("{:?}", block(&recalled));
        assert!(text.contains("Nothing has shown it to work yet"), "{text}");
        assert!(!text.contains("A turn it worked in showed"), "{text}");
        recalled.unproven = false;
        recalled.cases = vec![
            "older".to_owned(),
            "deployed 3 services, smoke test green".to_owned(),
        ];
        let text = format!("{:?}", block(&recalled));
        assert!(!text.contains("Nothing has shown it to work yet"), "{text}");
        assert!(
            text.contains("A turn it worked in showed: deployed 3 services, smoke test green")
                && !text.contains("older"),
            "{text}"
        );
    }

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
