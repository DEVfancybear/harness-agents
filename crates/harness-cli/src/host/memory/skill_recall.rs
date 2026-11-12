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
use std::fmt::Write as _;

use harness_session::{ContextBlock, ContextBlockKind};
use harness_types::readout::{standout_of, words};
use harness_types::search::bm25_idf;

use super::learned::{Layers, LearnedSkill, SkillScript};

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
    /// The scripts the skill carries, with the day a run was seen to succeed.
    pub scripts: Vec<SkillScript>,
    /// The skill's folder, which the scripts' paths are relative to.
    pub directory: Option<std::path::PathBuf>,
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
        scripts: skill.scripts.clone(),
        directory: layers.root(skill.layer).map(|root| root.join(&skill.name)),
        score,
    })
}

/// The index and BM25 score of the document that stands out for the query.
///
/// With enough documents for a spread to mean something, the best must lie
/// [`STANDOUT`](harness_types::readout::STANDOUT) deviations above the mean
/// score ([`standout_of`] judges it); with fewer, it must match at
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
    let bm25 = scored.iter().map(|(score, _)| *score).collect::<Vec<_>>();
    let best = standout_of(&bm25, |index| scored[index].1 >= 2)?;
    Some((best, bm25[best]))
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
    text.push_str(&scripts_note(recalled));
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

/// The most scripts a recalled skill lists, and the most characters of the
/// list: the block is bounded ([`RECALLED_TOKENS`]) and its tail is what a
/// cut loses, so the list is short and sits before the case and the failures.
const SCRIPTS_LISTED: usize = 8;
const SCRIPTS_CHARS: usize = 1_200;

/// The skill's scripts as the turn reads them: each with the day a run of it
/// was seen to succeed, or that none was; for a tested one, run it first.
///
/// After Continual-ARC (arXiv 2611.14505): what paid most was a tested,
/// executable artifact per task, run first when the task returned (0.65 to
/// 0.81 of the cost saved); the same knowledge as a rule in words saved far
/// less, because the agent confirmed it again before using it.
fn scripts_note(recalled: &Recalled) -> String {
    if recalled.scripts.is_empty() {
        return String::new();
    }
    let place = |script: &SkillScript| {
        recalled.directory.as_ref().map_or_else(
            || script.path.clone(),
            |directory| {
                format!(
                    "{}/{}",
                    directory.display().to_string().replace('\\', "/"),
                    script.path
                )
            },
        )
    };
    let mut ordered = recalled.scripts.iter().collect::<Vec<_>>();
    // Tested first: a cut keeps the ones worth running.
    ordered.sort_by_key(|script| script.tested_on.is_none());
    let mut text = String::from("\nThis skill carries scripts:");
    for script in ordered.iter().take(SCRIPTS_LISTED) {
        let line = match &script.tested_on {
            Some(day) => format!("\n- {}: ran successfully on {day}", place(script)),
            None => format!("\n- {}: not run yet", place(script)),
        };
        if text.chars().count() + line.chars().count() > SCRIPTS_CHARS {
            break;
        }
        text.push_str(&line);
    }
    if let Some(tested) = ordered.iter().find(|script| script.tested_on.is_some()) {
        let _ = write!(
            text,
            "\nRun {} first, before redoing the work by hand; if it fails, fix the script and say so.",
            place(tested)
        );
    }
    text
}

/// The most tokens a recalled skill takes in a turn's context.
const RECALLED_TOKENS: u64 = 1_500;

#[cfg(test)]
mod tests {
    use super::{Recalled, SkillScript, best_of, block, recall, without_front_matter};
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
            scripts: Vec::new(),
            directory: None,
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

    fn with_scripts(scripts: &[(&str, Option<&str>)]) -> Recalled {
        Recalled {
            name: "recolor-grid".to_owned(),
            body: "Run the script.".to_owned(),
            failures: Vec::new(),
            cases: Vec::new(),
            unproven: false,
            scripts: scripts
                .iter()
                .map(|(path, day)| SkillScript {
                    path: (*path).to_owned(),
                    tested_on: day.map(str::to_owned),
                })
                .collect(),
            directory: Some(std::path::PathBuf::from("C:\\skills\\recolor-grid")),
            score: 1.0,
        }
    }

    /// Continual-ARC (arXiv 2611.14505): a tested program run first when the
    /// task returned saved 0.65 to 0.81 of the cost; a rule in words saved far
    /// less. The block lists the scripts, says which were run, and tells the
    /// agent to run a tested one before redoing the work by hand.
    #[test]
    fn the_block_lists_the_scripts_and_says_to_run_the_tested_one_first() {
        let recalled = with_scripts(&[
            ("scripts/draft.py", None),
            ("scripts/solve.py", Some("2026-11-12")),
        ]);
        let text = format!("{:?}", block(&recalled));
        assert!(
            text.contains(
                "C:/skills/recolor-grid/scripts/solve.py: ran successfully on 2026-11-12"
            ),
            "{text}"
        );
        assert!(
            text.contains("C:/skills/recolor-grid/scripts/draft.py: not run yet"),
            "{text}"
        );
        assert!(
            text.contains("Run C:/skills/recolor-grid/scripts/solve.py first, before redoing the work by hand; if it fails, fix the script and say so."),
            "{text}"
        );
        assert!(
            text.find("solve.py: ran").expect("tested")
                < text.find("draft.py: not").expect("draft"),
            "a tested script is listed first"
        );
        // Nothing run yet: nothing to run first.
        let untested = format!("{:?}", block(&with_scripts(&[("scripts/draft.py", None)])));
        assert!(untested.contains("not run yet"), "{untested}");
        assert!(!untested.contains("first, before redoing"), "{untested}");
        // A skill with no script says nothing of scripts.
        assert!(!format!("{:?}", block(&with_scripts(&[]))).contains("carries scripts"));
    }

    /// The block is bounded: a skill with many long script names lists a few,
    /// the tested ones among them.
    #[test]
    fn the_scripts_list_stays_within_its_bound() {
        let names = (0..16)
            .map(|number| format!("scripts/{}-{number}.py", "long-name-".repeat(8)))
            .collect::<Vec<_>>();
        let mut scripts = names
            .iter()
            .map(|name| (name.as_str(), None))
            .collect::<Vec<_>>();
        scripts.push(("scripts/tested.py", Some("2026-11-12")));
        let note = super::scripts_note(&with_scripts(&scripts));
        assert!(note.chars().count() < super::SCRIPTS_CHARS + 300, "{note}");
        assert!(note.contains("tested.py: ran successfully"), "{note}");
    }

    /// A script a run vouched for is carried into the recalled skill with its
    /// day, and a script changed after the run is not tested any more.
    #[test]
    fn a_recalled_skill_carries_its_scripts_and_a_changed_one_is_untested() {
        use crate::interactive::learned::record_script_runs;
        let directory = tempfile::tempdir().expect("dir");
        let layers = Layers::new(
            &directory.path().join("config"),
            &directory.path().join("data"),
            &directory.path().join("workspace"),
            true,
        );
        let verdict = promote(
            &layers,
            &Intent::from_value(&json!({
                "action": "create", "name": "recolor-grid", "level": "project",
                "body": "---\nname: recolor-grid\ndescription: Use it to recolor the zebra grid.\ncategory: ops\n---\nRecolor the zebra grid with scripts/solve.py.\n",
                "files": {"scripts/solve.py": "print('recolored')\n"},
                "reason": "the user showed it", "evidence": "[User]: do it this way",
            })),
            &PromoteContext::default(),
        );
        assert!(verdict.ok, "{:?}", verdict.findings);
        let request = "recolor the zebra grid";
        let before = recall(&layers, &library(&layers), request).expect("fits");
        assert_eq!(before.scripts.len(), 1);
        assert_eq!(before.scripts[0].tested_on, None);
        let marked = record_script_runs(
            &layers,
            "recolor-grid",
            &[("scripts/solve.py".to_owned(), "recolored".to_owned())],
        );
        assert_eq!(marked, 1);
        let tested = recall(&layers, &library(&layers), request).expect("fits");
        assert_eq!(
            tested.scripts[0].tested_on.as_deref().map(str::len),
            Some(10),
            "{:?}",
            tested.scripts
        );
        assert!(format!("{:?}", block(&tested)).contains("first, before redoing the work by hand"));
        // The file is written again: the run was of another file.
        let root = layers
            .root(crate::interactive::learned::Layer::Project)
            .expect("project");
        std::fs::write(
            root.join("recolor-grid/scripts/solve.py"),
            "print('other')\n",
        )
        .expect("rewritten");
        let changed = recall(&layers, &library(&layers), request).expect("fits");
        assert_eq!(changed.scripts[0].tested_on, None);
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
