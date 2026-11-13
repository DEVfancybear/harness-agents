//! The one learned skill that fits a request, in the turn's context.
//!
//! LIFE-HARNESS (arXiv 2605.22166) retrieves skills for each task by BM25 and
//! inserts only the top one, so a skill that does not fit never crowds the
//! context; without the layer its agents lost up to 17% of their accuracy.
//! ha listed its learned skills by name and left it to the model to activate
//! one. Now the request is scored against every learned skill's name,
//! description and body, and the best goes into the turn - only when its
//! score stands out from the rest, judged by the spread of the others' scores
//! ([`best_of`]), so a request that fits none gets none. What the skill took
//! for granted in the turns it failed goes with it: those assumptions were
//! kept, and never read when the skill was used again.
//!
//! What is asked is a request ([`query`]): the active goal's objective and the
//! newest input the user typed. Not an input the host or a child wrote (a
//! goal's continuation, a report, a kernel notice), which shares words with
//! every skill and asks nothing. A skill is put whole into a conversation once
//! since its last compaction; later turns it fits get one line ([`again`]),
//! because its text is already in the conversation ([`given_in`] finds it in
//! the packets the conversation sent).
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
use harness_types::readout::words;
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
#[cfg(test)]
#[must_use]
pub fn recall(layers: &Layers, skills: &[LearnedSkill], request: &str) -> Option<Recalled> {
    consider(layers, skills, request).recalled
}

/// What recall made of a request: the skill that fits, if one does, and each
/// candidate's score, which orders the catalog when none fits.
#[derive(Debug)]
pub struct Considered {
    pub recalled: Option<Recalled>,
    pub scores: BTreeMap<String, f64>,
}

/// [`recall`], with the scores of the skills it weighed.
#[must_use]
pub fn consider(layers: &Layers, skills: &[LearnedSkill], request: &str) -> Considered {
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
    let query = words(request);
    let fits = scores_of(&documents, &query);
    let scores = skills
        .iter()
        .zip(&fits)
        .map(|(skill, fit)| (skill.name.clone(), fit.score))
        .collect::<BTreeMap<_, _>>();
    let Some(best) = standout(&fits) else {
        return Considered {
            recalled: None,
            scores,
        };
    };
    let score = fits[best].score;
    let skill = skills[best];
    let mut body = bodies[best].trim().to_owned();
    if body.chars().count() > MAX_BODY_CHARS {
        body = body.chars().take(MAX_BODY_CHARS).collect::<String>();
        body.push_str("\n…(cut; activate_skill reads it whole)");
    }
    let recalled = Recalled {
        name: skill.name.clone(),
        body,
        failures: skill.failures.clone(),
        cases: skill.cases.clone(),
        unproven: skill.usage.unproven(),
        scripts: skill.scripts.clone(),
        directory: layers.root(skill.layer).map(|root| root.join(&skill.name)),
        score,
    };
    Considered {
        recalled: Some(recalled),
        scores,
    }
}

/// The words of the request a skill must hold to fit: one shared word is not a
/// fit, whatever the library's size, and a word most skills hold is not one of
/// the two ([`scores_of`]).
const MIN_WORDS: usize = 2;

/// How many deviations of the OTHER skills' scores the best must lie above
/// their mean.
///
/// The best of n scores that all come from one distribution lies above its
/// mean by k deviations with a chance of about n times the tail beyond k: at
/// one deviation (the digest's `STANDOUT`, judged with the best in the spread)
/// a dozen skills that fit nothing still produce a "best" 84 times in 100; at
/// two, 27; at three, under 2. Scores of BM25 are skewed to the right, so the
/// real chance is higher than the normal tail says, and three is the least
/// that keeps a request that fits none from getting a skill. A skill that fits
/// scores several times the rest, which three deviations of them do not stop.
const OTHERS_DEVIATIONS: f64 = 3.0;

/// The least spread of the others' scores, as a share of their mean. Scores
/// that all agree (two skills that match the same words; a library of three)
/// show no noise because there are too few of them to show it, not because
/// there is none: the best then has to lead by half the mean times
/// [`OTHERS_DEVIATIONS`], not by a hair.
const FLAT_SPREAD: f64 = 0.5;

/// The share of the request's own weight the words that tell skills apart and
/// the best skill holds must carry ([`Fit::share`]).
///
/// The spread rule says the best skill stands out from the others; it cannot
/// say the skill is what the request is about. A party prompt that happened to
/// hold "staging" and one common word scored every other skill about zero, so a
/// migration skill stood out of nothing. A whole skill in a turn is a cost the
/// request has to earn, and a request earns it when the skill accounts for a
/// good part of what the request says. Measured on the seeded library of
/// seven: a request about the skill holds 0.6 to 0.8 of its weight ("run the
/// db migration on staging"), and 0.36 when a marker and "reply with the single
/// word OK and call no tools" ride with it; the party prompt holds 0.11 and a
/// long goal that mentions the migration in one sentence 0.16 (0.13 to 0.23 in
/// a library of twelve for requests that fit none). A quarter lies between
/// the last of those and the first two, with room on both sides; a third lay
/// on the edge of the marked request. A request that merely mentions a skill
/// among other things falls to the catalog, which carries the skill's name for
/// the model to read it if it judges it fits.
const MIN_SHARE: f64 = 0.25;

/// How one skill fits a request.
#[derive(Clone, Copy, Debug)]
struct Fit {
    /// BM25 score of the skill's name, description and body.
    score: f64,
    /// The distinct words of the request that tell skills apart which the skill
    /// holds.
    words: usize,
    /// The IDF-weighted share of the request's distinct words that those words
    /// make. A word of the request that no skill holds weighs the most a word
    /// can in this library (as if a single skill held it): it is as telling as a
    /// word can be, and a skill cannot match it.
    share: f64,
}

/// The index and BM25 score of the document that stands out for the query.
///
/// It must match at least [`MIN_WORDS`] of the query's words, beat the next
/// one, lie [`OTHERS_DEVIATIONS`] deviations above the mean of all the OTHER
/// scores (the best left out, so it cannot raise the bar it must clear), and
/// carry [`MIN_SHARE`] of the request's weight. A score of a skill the query
/// did not reach is `0.0` and counts in the spread; when every other score is
/// zero the two words and the share decide. No word list: how much a word
/// counts comes from the library.
#[cfg(test)]
fn best_of(documents: &[Vec<String>], query: &[String]) -> Option<(usize, f64)> {
    let fits = scores_of(documents, query);
    let best = standout(&fits)?;
    Some((best, fits[best].score))
}

/// Each document's [`Fit`] for the query. The words that count toward the two
/// and the share are those that tell skills apart: held by at most half of them
/// (a statistic of the library, not a word list; with one or two skills every
/// word one of them holds tells).
fn scores_of(documents: &[Vec<String>], query: &[String]) -> Vec<Fit> {
    let query = query.iter().collect::<BTreeSet<_>>();
    if documents.is_empty() || query.is_empty() {
        return Vec::new();
    }
    #[allow(clippy::cast_precision_loss, reason = "word counts are small")]
    let average = documents.iter().map(Vec::len).sum::<usize>() as f64 / documents.len() as f64;
    let count = documents.len() as u64;
    let holding = |term: &String| {
        documents
            .iter()
            .filter(|document| document.contains(term))
            .count() as u64
    };
    let idf = query
        .iter()
        .map(|term| (*term, bm25_idf(count, holding(term))))
        .collect::<BTreeMap<_, _>>();
    let telling = query
        .iter()
        .map(|term| (*term, holding(term) * 2 <= count.max(2)))
        .collect::<BTreeMap<_, _>>();
    // What the request weighs: a word some skill holds by its IDF, one none
    // holds as much as the rarest word a skill can hold.
    let rarest = bm25_idf(count, 1);
    let weight = query
        .iter()
        .map(|term| {
            if holding(term) == 0 {
                rarest
            } else {
                idf[term]
            }
        })
        .sum::<f64>();
    documents
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
            let mut held = 0.0;
            for term in &query {
                if let Some(frequency) = counts.get(term) {
                    // A word most skills hold tells none of them apart: it
                    // scores, by its small weight, but is not one of the
                    // two words a fit needs, and carries no share.
                    if telling[term] {
                        matched += 1;
                        held += idf[term];
                    }
                    score += idf[term] * frequency * (K1 + 1.0)
                        / (frequency + K1 * (1.0 - B + B * length / average.max(1.0)));
                }
            }
            Fit {
                score,
                words: matched,
                share: if weight > 0.0 { held / weight } else { 0.0 },
            }
        })
        .collect()
}

/// The index of the one [`Fit`] that stands out ([`best_of`]).
fn standout(fits: &[Fit]) -> Option<usize> {
    let mut order = (0..fits.len()).collect::<Vec<_>>();
    order.sort_by(|left, right| fits[*right].score.total_cmp(&fits[*left].score));
    let best = *order.first()?;
    let Fit {
        score,
        words,
        share,
    } = fits[best];
    if score <= 0.0 || words < MIN_WORDS || share < MIN_SHARE {
        return None;
    }
    let others = order[1..]
        .iter()
        .map(|index| fits[*index].score)
        .collect::<Vec<_>>();
    if others.first().is_some_and(|next| *next >= score) {
        return None;
    }
    if others.is_empty() {
        return Some(best);
    }
    #[allow(clippy::cast_precision_loss, reason = "skill counts are small")]
    let count = others.len() as f64;
    let mean = others.iter().sum::<f64>() / count;
    let deviation = (others
        .iter()
        .map(|other| (other - mean).powi(2))
        .sum::<f64>()
        / count)
        .sqrt();
    (score > mean + OTHERS_DEVIATIONS * deviation.max(FLAT_SPREAD * mean)).then_some(best)
}

// ---------------------------------------------------------------------------
// What is asked

/// The request recall is asked of: the active goal's objective and the newest
/// input the user typed, joined; `None` when there is neither.
///
/// `typed` is what the user typed for this turn (empty for a turn the host
/// started); `earlier` the user's earlier inputs of the conversation, newest
/// first. An input the host or a child wrote is no request and adds nothing:
/// the goal's continuation repeats the objective, a child's report or a kernel
/// notice shares words with every skill. When this turn is the host's own, the
/// newest earlier input of the user stands for it.
#[must_use]
pub fn query(goal: Option<&str>, typed: &str, earlier: &[String]) -> Option<String> {
    let user = std::iter::once(typed)
        .chain(earlier.iter().map(String::as_str))
        .find_map(users_words);
    let parts = goal
        .map(str::trim)
        .filter(|goal| !goal.is_empty())
        .into_iter()
        .chain(user)
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// What of `text` the user wrote: nothing for a turn the host started (a
/// goal's continuation, a bracketed note such as `[python-state]` or
/// `[child-exited: ...]`), and not the reports of delegated agents the app
/// folds into the user's message after a Ctrl-C.
fn users_words(text: &str) -> Option<&str> {
    let own = text
        .split(super::controller::HELD_REPORTS_LEAD)
        .next()
        .unwrap_or_default()
        .trim();
    (!own.is_empty() && !super::goal::is_host_text(own) && !is_tagged_note(own)).then_some(own)
}

/// Whether `text` opens with a bracketed lowercase tag - `[python-state]`,
/// `[child-exited: ...]`, `[agent-message from child:x]` - the way a note the
/// host sent itself does. Turns recorded before the typed words were kept have
/// only this to tell them from the user's.
fn is_tagged_note(text: &str) -> bool {
    let first = text.lines().next().unwrap_or_default().trim();
    first.strip_prefix('[').is_some_and(|rest| {
        let tag = rest.split([']', ':']).next().unwrap_or_default();
        !tag.is_empty()
            && tag.chars().all(|character| {
                character.is_ascii_lowercase() || character == '-' || character == ' '
            })
    })
}

// ---------------------------------------------------------------------------
// Once per conversation

/// How a skill's text opens in a message: a recalled block and a `/skill:`
/// expansion both carry the skill in a tag of this form.
const GIVEN_MARKER: &str = "<skill name=\"";

/// The learned skills whose text `packet` (a message the conversation sent)
/// holds: those a recall put in, and those the user ran with `/skill:`.
#[must_use]
pub fn given_in(packet: &str) -> BTreeSet<String> {
    packet
        .split(GIVEN_MARKER)
        .skip(1)
        .map(|rest| {
            rest.chars()
                .take_while(|character| {
                    character.is_ascii_lowercase()
                        || character.is_ascii_digit()
                        || *character == '-'
                })
                .collect::<String>()
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// The one line a turn carries when the skill that fits was already given
/// whole to this conversation.
#[must_use]
pub fn again(name: &str) -> ContextBlock {
    ContextBlock::mandatory(
        "recalled-skill",
        ContextBlockKind::Instruction,
        format!(
            "The learned skill {name} given earlier in this conversation still fits; its text is above, and activate_skill reads it again."
        ),
    )
    .with_max_tokens(RECALLED_TOKENS)
    .with_rest_at(format!("the learned skill {name}"))
}

/// The line the user is shown when a skill is put whole into a turn.
#[must_use]
pub fn notice(recalled: &Recalled) -> String {
    format!(
        "learned skill {} offered for this request{}",
        recalled.name,
        if recalled.unproven { " (unproven)" } else { "" }
    )
}

// ---------------------------------------------------------------------------
// The catalog: when no skill fits, the names

/// The most skills one catalog lists, and the most characters of the list.
///
/// A line is `name: description` (a description is at most 200 characters), so
/// forty lines of the usual 80 to 120 characters fit in the character bound,
/// which is about 1,500 tokens: a catalog is a nudge, not a second system
/// prompt. A larger library lists the skills nearest the request first, then
/// the most recently used, and says how many it left out.
const CATALOG_LINES: usize = 40;
const CATALOG_CHARS: usize = 6_000;

/// The most tokens the catalog block takes in a turn's context.
const CATALOG_TOKENS: u64 = 2_000;

/// What opens the catalog's list in a message, so a later turn can tell the
/// conversation already holds it ([`catalog_given_in`]).
const CATALOG_MARKER: &str = "<learned_skills>";

/// The catalog of learned skills a turn carries when none was put whole.
#[derive(Debug)]
pub struct Catalog {
    pub block: ContextBlock,
    /// The skills it lists.
    pub listed: usize,
    /// The skills it leaves out for the bound.
    pub left_out: usize,
}

/// A catalog of the learned skills, for a turn where recall put none whole:
/// one `name: description` line each, unproven ones marked, those recall holds
/// back left out. `scores` (from [`consider`]) puts the skills nearest the
/// request first; the rest follow by the most recently activated, viewed or
/// worked. `None` when there is nothing to list.
///
/// Recall by words cannot cross a language (a request in Vietnamese, skills in
/// English), and a request that mentions a skill among many things does not
/// stand out: the model can judge a description in any language, so it is told
/// what exists and decides.
#[must_use]
pub fn catalog(skills: &[LearnedSkill], scores: &BTreeMap<String, f64>) -> Option<Catalog> {
    let mut candidates = skills
        .iter()
        .filter(|skill| !skill.usage.restricted())
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    let score = |skill: &LearnedSkill| scores.get(&skill.name).copied().unwrap_or(0.0);
    candidates.sort_by(|left, right| {
        score(right)
            .total_cmp(&score(left))
            .then_with(|| right.last_used.cmp(&left.last_used))
            .then_with(|| left.name.cmp(&right.name))
    });
    let mut lines = Vec::new();
    let mut chars = 0;
    for skill in &candidates {
        let description = skill
            .description
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let line = format!(
            "- {}: {description}{}",
            skill.name,
            if skill.usage.unproven() {
                " (unproven)"
            } else {
                ""
            }
        );
        if lines.len() == CATALOG_LINES || chars + line.chars().count() + 1 > CATALOG_CHARS {
            break;
        }
        chars += line.chars().count() + 1;
        lines.push(line);
    }
    let listed = lines.len();
    let left_out = candidates.len() - listed;
    let mut text = format!(
        "These are procedures earlier conversations learned. When one fits what you are about to do, read it with activate_skill before starting; when none fits, ignore the list.\n{CATALOG_MARKER}\n{}",
        lines.join("\n")
    );
    if left_out > 0 {
        let _ = write!(
            text,
            "\n({left_out} more not listed; list_skills shows all.)"
        );
    }
    text.push_str("\n</learned_skills>");
    let block = ContextBlock::mandatory("learned-skills", ContextBlockKind::Instruction, text)
        .with_max_tokens(CATALOG_TOKENS)
        .with_rest_at("list_skills");
    Some(Catalog {
        block,
        listed,
        left_out,
    })
}

/// Whether `packet` (a message the conversation sent) holds the catalog.
#[must_use]
pub fn catalog_given_in(packet: &str) -> bool {
    packet.contains(CATALOG_MARKER)
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

    /// A library of twelve, modelled on a real one: short "Use when ..."
    /// descriptions and short bodies.
    const LIBRARY: [(&str, &str, &str); 12] = [
        (
            "delegated-work-scope-audit",
            "Use when delegating code edits to a child agent or before committing/integrating its output.",
            "Before integrating a child's edits, run git diff in the child worktree and list every file it touched. Compare them with the files the brief named. Revert any edit outside that scope and tell the child.",
        ),
        (
            "release-cut-checklist",
            "Use when cutting a release or bumping the version of the workspace.",
            "Bump the workspace version, build the candidate, run the live suites on its binary, commit, tag, and check the published checksum.",
        ),
        (
            "flaky-test-triage",
            "Use when a test passes and fails on reruns without any code change.",
            "Rerun the failing test alone ten times, record the seed, bisect the shared state that leaks between tests, and fix the ordering.",
        ),
        (
            "powershell-quoting",
            "Use when a shell command with quotes or paths with spaces fails on Windows.",
            "Wrap paths in double quotes, use single quoted here strings for multi line text, and call executables with spaces through the call operator.",
        ),
        (
            "cargo-lock-contention",
            "Use when cargo blocks waiting for a file lock on the build directory.",
            "Another agent holds the build directory. Wait and retry, never kill its cargo process, and report it.",
        ),
        (
            "live-evidence-report",
            "Use when reporting the result of a live test run.",
            "Quote each suite's RESULT line, the SHA-256 of the binary it ran on, the commit, and where the logs are.",
        ),
        (
            "git-identity",
            "Use when making a git commit and the author identity matters.",
            "Commit as the maintainer's work identity, never the account email, and end the message with the attribution line.",
        ),
        (
            "bilingual-docs-sync",
            "Use when a doc changes that exists in English and Vietnamese.",
            "Edit the en file and the vi file in the same change, keep the headings in step, and do not translate code names.",
        ),
        (
            "clippy-warning-fixes",
            "Use when clippy reports warnings that must be denied before the gate.",
            "Run clippy with warnings denied across the workspace and all targets, fix the cause, and never allow a lint without a reason.",
        ),
        (
            "bug-report-reading",
            "Use when the maintainer sends a diagnostic bundle or a bug report to read.",
            "Open the diagnostics jsonl first, count the events by kind, then read the turns around the first error and trace the cause.",
        ),
        (
            "worktree-hygiene",
            "Use when starting work in a repository other people are using.",
            "Work in a worktree of your own, never edit a checkout someone else is using, and remove the worktree when done.",
        ),
        (
            "memory-review-queue",
            "Use when a memory entry waits for review or an entry should be superseded.",
            "List the queue, show each item with its evidence, accept or reject by the user's words, and never accept an entry a child proposed alone.",
        ),
    ];

    /// Fits that carry the whole of a request's weight.
    fn fits_of(scored: &[(f64, usize)]) -> Vec<super::Fit> {
        scored
            .iter()
            .map(|(score, words)| super::Fit {
                score: *score,
                words: *words,
                share: 1.0,
            })
            .collect()
    }

    fn library_documents() -> Vec<Vec<String>> {
        LIBRARY
            .iter()
            .map(|(name, description, body)| {
                words(&format!("{} {description} {body}", name.replace('-', " ")))
            })
            .collect()
    }

    /// The name of the library's skill that fits, by the rule recall uses.
    fn fits(request: &str) -> Option<&'static str> {
        best_of(&library_documents(), &words(request)).map(|(index, _)| LIBRARY[index].0)
    }

    /// What recall asked before: the best of the scores had to lie one
    /// deviation above the mean of ALL the scores, itself among them, and with
    /// five skills or more no word had to match.
    fn old_rule(request: &str) -> Option<&'static str> {
        let scored = super::scores_of(&library_documents(), &words(request));
        let bm25 = scored.iter().map(|fit| fit.score).collect::<Vec<_>>();
        // With five skills or more the words matched were not consulted.
        harness_types::readout::standout_of(&bm25, |_| true).map(|index| LIBRARY[index].0)
    }

    /// Fails without the change: the old rule put a skill into a request about
    /// something no skill covers.
    #[test]
    fn a_request_that_none_of_twelve_skills_covers_recalls_nothing() {
        let request = "sing me a poem about autumn and make it rhyme";
        assert!(
            old_rule(request).is_some(),
            "control: the old rule still recalled a skill for it"
        );
        assert_eq!(fits(request), None);
        for request in [
            "cook rice with saffron and a little butter",
            "paint the kitchen wall a warmer white",
            "summarize this novel for my book club",
        ] {
            assert_eq!(fits(request), None, "{request}");
        }
    }

    #[test]
    fn a_request_that_names_a_skills_situation_recalls_it() {
        for (request, skill) in [
            (
                "I am about to integrate what the child agent edited; audit its output before I commit",
                "delegated-work-scope-audit",
            ),
            (
                "this test is flaky: it fails on a rerun with no code change",
                "flaky-test-triage",
            ),
            (
                "cargo is blocked waiting for the file lock on the build directory",
                "cargo-lock-contention",
            ),
            (
                "update the Vietnamese doc as well as the English one",
                "bilingual-docs-sync",
            ),
            (
                "the maintainer sent a bug report bundle, read the diagnostics",
                "bug-report-reading",
            ),
        ] {
            assert_eq!(fits(request), Some(skill), "{request}");
        }
    }

    /// Fails without the change: a long text that shares a single word with
    /// each of many skills, and the small words every text shares, is not a
    /// request any of them fits.
    #[test]
    fn a_long_text_that_shares_scattered_words_with_many_skills_recalls_nothing() {
        let text = "The child finished and its report is in. It ran the checks in its worktree, \
            then looked at the docs and the version, waited for the cargo build, and said that the \
            test run and the review of the queue are done. Nothing was committed, the logs are \
            where they were, and the maintainer can read them when it is time to go on.";
        assert!(
            old_rule(text).is_some(),
            "control: the old rule recalled a skill for it"
        );
        assert_eq!(fits(text), None);
    }

    /// Whatever the library's size, one shared word is not a fit, however
    /// rare the word is.
    #[test]
    fn two_distinct_words_are_needed_in_a_library_of_any_size() {
        assert_eq!(fits("clippy"), None);
        assert_eq!(fits("what does clippy say about it"), None);
        assert_eq!(fits("clippy warnings"), Some("clippy-warning-fixes"));
        // And in a library of two.
        let two = library_documents().into_iter().take(2).collect::<Vec<_>>();
        assert!(best_of(&two, &words("version")).is_none());
        assert!(best_of(&two, &words("bump the version")).is_some());
    }

    /// The best is judged against the OTHER skills' spread: when every other
    /// score is zero the two words decide; when the others agree, a hair of a
    /// lead is not enough.
    #[test]
    fn the_bar_is_the_others_spread_and_a_flat_field_needs_a_clear_lead() {
        let alone = docs(&[
            "deploy release staging run the deploy script",
            "paint the wall",
            "bake bread",
            "water the plants",
            "walk the dog",
        ]);
        assert!(best_of(&alone, &words("run the deploy script")).is_some());
        // The same scores everywhere but one that leads by a hair.
        let flat = super::standout(&fits_of(&[
            (1.0, 2),
            (0.95, 2),
            (0.95, 2),
            (0.95, 2),
            (0.95, 2),
        ]));
        assert_eq!(flat, None);
        let clear = super::standout(&fits_of(&[
            (3.0, 2),
            (0.95, 2),
            (0.95, 2),
            (0.95, 2),
            (0.95, 2),
        ]));
        assert_eq!(clear, Some(0));
        // A tie for the lead is no lead.
        assert_eq!(
            super::standout(&fits_of(&[(2.0, 3), (2.0, 3), (0.0, 0)])),
            None
        );
    }

    /// The whole path, through the files of a library: a request no skill
    /// covers recalls nothing, one that names a situation recalls its skill.
    #[test]
    fn recall_reads_a_library_of_twelve_from_disk() {
        let directory = tempfile::tempdir().expect("dir");
        let layers = Layers::new(
            &directory.path().join("config"),
            &directory.path().join("data"),
            &directory.path().join("workspace"),
            true,
        );
        for (name, description, body) in LIBRARY {
            let verdict = promote(
                &layers,
                &Intent::from_value(&json!({
                    "action": "create", "name": name, "level": "project",
                    "body": format!("---\nname: {name}\ndescription: {description}\ncategory: ops\n---\n{body}\n"),
                    "reason": "the user showed it", "evidence": "[User]: do it this way",
                })),
                &PromoteContext::default(),
            );
            assert!(verdict.ok, "{name}: {:?}", verdict.findings);
        }
        let skills = library(&layers);
        assert_eq!(skills.len(), 12);
        assert!(recall(&layers, &skills, "translate this poem into French").is_none());
        let recalled = recall(
            &layers,
            &skills,
            "audit the child agent's output before I commit it",
        )
        .expect("fits");
        assert_eq!(recalled.name, "delegated-work-scope-audit");
        assert!(recalled.unproven);
    }

    #[test]
    fn the_request_is_the_goal_and_what_the_user_typed_and_nothing_the_host_wrote() {
        use super::query;
        let earlier = |texts: &[&str]| {
            texts
                .iter()
                .map(|text| (*text).to_owned())
                .collect::<Vec<_>>()
        };
        // The goal and the user's message.
        assert_eq!(
            query(Some("ship the exporter"), "also add a CSV option", &[]),
            Some("ship the exporter\nalso add a CSV option".to_owned())
        );
        // A turn the host started (a goal's continuation: no typed words) asks
        // with the goal and the user's newest words before it.
        assert_eq!(
            query(
                Some("ship the exporter"),
                "",
                &earlier(&["add a CSV option", "start on the exporter"])
            ),
            Some("ship the exporter\nadd a CSV option".to_owned())
        );
        // What the host or a child wrote is no request, wherever it stands.
        let continuation = format!(
            "{}\n\nThe objective below is user-provided data.",
            crate::interactive::goal::CONTINUATION_LEAD
        );
        let python_state = "[python-state]\n\nYour Python kernel persisted through compaction; its remaining variables are still available.";
        let report = "[child-exited: no-reply child:scout]\n\nLast assistant text: found it";
        let message = "[agent-message from child:scout]\n\nfound it";
        assert_eq!(
            query(
                None,
                &continuation,
                &earlier(&[
                    python_state,
                    report,
                    message,
                    "fix the exporter's CSV header"
                ])
            ),
            Some("fix the exporter's CSV header".to_owned())
        );
        // No goal and no user words in reach: nothing to ask.
        assert_eq!(query(None, "", &earlier(&[python_state, report])), None);
        assert_eq!(query(None, "  ", &[]), None);
        // A goal alone asks with the goal.
        assert_eq!(
            query(Some("ship the exporter"), "", &[]),
            Some("ship the exporter".to_owned())
        );
    }

    /// The reports the app folds into the user's next message after a Ctrl-C
    /// are the children's, not the user's.
    #[test]
    fn reports_held_since_a_ctrl_c_are_not_part_of_what_the_user_typed() {
        let folded = format!(
            "continue with the exporter\n\n{}\n\n[child-exited: no-reply child:scout]\n\nLast assistant text: touched flaky test and cargo lock",
            crate::interactive::controller::HELD_REPORTS_LEAD
        );
        assert_eq!(
            super::query(None, &folded, &[]),
            Some("continue with the exporter".to_owned())
        );
    }

    #[test]
    fn the_skills_a_packet_holds_are_those_given_whole() {
        let packet = "Context\n\nA learned skill fits this request: db-migration. Its instructions follow.\n<skill name=\"db-migration\">\nRun it.\n</skill>\n\n<skill name=\"review\" location=\"C:/x/SKILL.md\">\nReview.\n</skill>\n\nThe learned skill release-notes given earlier in this conversation still fits; its text is above.";
        let given = super::given_in(packet);
        assert_eq!(
            given.into_iter().collect::<Vec<_>>(),
            ["db-migration", "review"]
        );
        // The one line is not the skill's text.
        assert!(super::given_in("The learned skill release-notes given earlier").is_empty());
    }

    #[test]
    fn the_block_carries_a_skill_whole_and_the_line_carries_only_its_name() {
        let recalled = Recalled {
            name: "deploy".to_owned(),
            body: "Run the deploy script.".to_owned(),
            failures: Vec::new(),
            cases: Vec::new(),
            unproven: true,
            scripts: Vec::new(),
            directory: None,
            score: 1.0,
        };
        let whole = format!("{:?}", block(&recalled));
        assert!(whole.contains("Run the deploy script."), "{whole}");
        let line = format!("{:?}", super::again("deploy"));
        assert!(
            line.contains("The learned skill deploy given earlier in this conversation still fits; its text is above, and activate_skill reads it again."),
            "{line}"
        );
        assert!(!line.contains("Run the deploy script."), "{line}");
        assert_eq!(
            super::notice(&recalled),
            "learned skill deploy offered for this request (unproven)"
        );
        let proven = Recalled {
            unproven: false,
            ..recalled
        };
        assert_eq!(
            super::notice(&proven),
            "learned skill deploy offered for this request"
        );
    }

    /// The seven skills recall.sh seeds in a user's library.
    const SEVEN: [(&str, &str, &str); 7] = [
        (
            "db-migration",
            "Use when asked to run the database migration against staging.",
            "Write the database migration, check that staging answers its health page, then run the migration against staging with the flag --safe-order and report the rows it changed.",
        ),
        (
            "release-notes",
            "Use when asked to write release notes or a changelog entry.",
            "Add the entry at the top of the changelog, one bullet per change, in the words of the people who asked for it.",
        ),
        (
            "image-resize",
            "Use when asked to resize product images for the storefront.",
            "Resize every product image to 800 pixels wide, keep the aspect ratio, and save the thumbnails beside the originals.",
        ),
        (
            "invoice-export",
            "Use when asked to export invoices to the accounting system.",
            "Export the invoices of the closed month as a CSV file, check the totals against the ledger, then upload the file to the accounting system.",
        ),
        (
            "log-rotation",
            "Use when asked to rotate and compress the server logs.",
            "Rotate the server logs at midnight, compress the old files with gzip, and delete archives older than thirty days.",
        ),
        (
            "cert-renewal",
            "Use when asked to renew the TLS certificate for the proxy.",
            "Request a new TLS certificate, install it on the proxy, reload the proxy, and check the expiry date in the browser.",
        ),
        (
            "audit-requirements",
            "Use when asked to audit the current state against every requirement and report the remaining proof.",
            "Audit the state of the work against each requirement, list the proof for each one, and report the status and the remaining tokens of effort.",
        ),
    ];

    fn documents_of(library: &[(&str, &str, &str)]) -> Vec<Vec<String>> {
        library
            .iter()
            .map(|(name, description, body)| {
                words(&format!("{} {description} {body}", name.replace('-', " ")))
            })
            .collect()
    }

    /// The skill of `library` that fits `request`, and whether it would, were
    /// the request's share of weight not asked of it.
    fn fit_in(
        library: &[(&'static str, &str, &str)],
        request: &str,
    ) -> (Option<&'static str>, Option<&'static str>) {
        let documents = documents_of(library);
        let fits = super::scores_of(&documents, &words(request));
        let name = |index: Option<usize>| index.map(|index| library[index].0);
        let spread_alone = fits
            .iter()
            .map(|fit| super::Fit { share: 1.0, ..*fit })
            .collect::<Vec<_>>();
        (
            name(super::standout(&fits)),
            name(super::standout(&spread_alone)),
        )
    }

    const LONG_GOAL: &str = "Make the exporter reliable. Read the tracker, list every open bug, and decide which are in scope. Then run the database migration against staging, because the schema changed. Afterwards update the dashboards, write a short note for the team about what moved, and ask the reviewers which parts they want to see first. Keep the notes short and in plain words.";

    /// Fails without the change: two telling words that the party prompt shares
    /// with a skill ("staging", "asked") while every other skill scored about
    /// zero put the migration skill whole into the turn.
    #[test]
    fn a_prompt_that_shares_two_words_by_coincidence_does_not_put_a_skill_whole() {
        let party = "MARK-R3A. When asked to plan the party, use the staging area for the cake. Reply with the single word OK-R3A and call no tools.";
        let (now, without_the_share) = fit_in(&SEVEN, party);
        assert_eq!(without_the_share, Some("db-migration"), "control");
        assert_eq!(now, None);
        // The same in the library of twelve: two words of one skill, by chance.
        let people = "Please plan the party for the people next Friday, order a big cake with candles, and use the checkout area for the music and the balloons.";
        let (now, without_the_share) = fit_in(&LIBRARY, people);
        assert_eq!(without_the_share, Some("worktree-hygiene"), "control");
        assert_eq!(now, None);
    }

    /// A request about a skill's situation still puts it whole, with the
    /// boilerplate a test prompt carries or without.
    #[test]
    fn a_request_about_a_skills_situation_still_puts_it_whole() {
        for request in [
            "run the db migration on staging",
            "please run the database migration on staging",
            "Run the database migration against staging again",
            "MARK-R1. Run the database migration against staging. Reply with the single word OK-R1 and call no tools.",
            "MARK-R2. Run the database migration against staging again. Reply with the single word OK-R2 and call no tools.",
        ] {
            assert_eq!(fit_in(&SEVEN, request).0, Some("db-migration"), "{request}");
        }
        assert_eq!(
            fit_in(&SEVEN, "sing me the song of autumn").0,
            None,
            "a request that fits none"
        );
    }

    /// Intended: a long goal that names a skill's situation in one sentence
    /// among many is not a request for that skill. It falls to the catalog,
    /// which names the skill for the model to read if it judges it fits.
    #[test]
    fn a_long_goal_that_mentions_a_situation_in_one_sentence_falls_to_the_catalog() {
        let (now, without_the_share) = fit_in(&SEVEN, LONG_GOAL);
        assert_eq!(without_the_share, Some("db-migration"), "control");
        assert_eq!(now, None);
    }

    fn disk_library(root: &std::path::Path, skills: &[(&str, &str, &str)]) -> Layers {
        let layers = Layers::new(
            &root.join("config"),
            &root.join("data"),
            &root.join("workspace"),
            true,
        );
        for (name, description, body) in skills {
            let verdict = promote(
                &layers,
                &Intent::from_value(&json!({
                    "action": "create", "name": name, "level": "project",
                    "body": format!("---\nname: {name}\ndescription: {description}\ncategory: ops\n---\n{body}\n"),
                    "reason": "the user showed it", "evidence": "[User]: do it this way",
                })),
                &PromoteContext::default(),
            );
            assert!(verdict.ok, "{name}: {:?}", verdict.findings);
        }
        layers
    }

    fn text_of(catalog: &super::Catalog) -> String {
        format!("{:?}", catalog.block)
    }

    #[test]
    fn the_catalog_lists_each_skill_once_unproven_marked_and_held_back_ones_left_out() {
        let directory = tempfile::tempdir().expect("dir");
        let layers = disk_library(directory.path(), &SEVEN);
        // One skill worked, so it is no draft; one keeps failing, so it is held back.
        record_outcome(
            &layers,
            "release-notes",
            true,
            "",
            "the changelog opens with RN-7",
        );
        for _ in 0..2 {
            record_outcome(&layers, "log-rotation", false, "gzip exists", "no gzip");
        }
        let skills = library(&layers);
        let catalog =
            super::catalog(&skills, &std::collections::BTreeMap::new()).expect("a catalog");
        let text = text_of(&catalog);
        assert_eq!((catalog.listed, catalog.left_out), (6, 0), "{text}");
        assert!(
            text.contains("- db-migration: Use when asked to run the database migration against staging. (unproven)"),
            "{text}"
        );
        assert!(
            text.contains(
                "- release-notes: Use when asked to write release notes or a changelog entry."
            ) && !text.contains("release notes or a changelog entry. (unproven)"),
            "{text}"
        );
        assert!(!text.contains("log-rotation"), "held back: {text}");
        assert!(
            text.contains(
                "read it with activate_skill before starting; when none fits, ignore the list"
            ),
            "{text}"
        );
        assert!(super::catalog_given_in(&text));
        assert!(!super::catalog_given_in(
            "A learned skill fits this request: x."
        ));
        // No skill, or only held-back ones: nothing to list.
        assert!(super::catalog(&[], &std::collections::BTreeMap::new()).is_none());
    }

    /// The skills nearest the request come first, then the most recently used.
    #[test]
    fn the_catalog_puts_the_nearest_skills_first_and_then_the_most_recently_used() {
        let directory = tempfile::tempdir().expect("dir");
        let layers = disk_library(directory.path(), &SEVEN);
        assert!(crate::interactive::learned::record_use(
            &layers,
            "cert-renewal",
            crate::interactive::learned::UseKind::Use
        ));
        let skills = library(&layers);
        let scores = [("image-resize".to_owned(), 5.0)].into_iter().collect();
        let text = text_of(&super::catalog(&skills, &scores).expect("a catalog"));
        let position = |name: &str| text.find(&format!("- {name}:")).expect(name);
        assert!(
            position("image-resize") < position("cert-renewal"),
            "{text}"
        );
        assert!(
            position("cert-renewal") < position("db-migration"),
            "{text}"
        );
        // An offer is not a taking: it does not move a skill up.
        assert!(crate::interactive::learned::record_use(
            &layers,
            "invoice-export",
            crate::interactive::learned::UseKind::Offered
        ));
        let text = text_of(
            &super::catalog(&library(&layers), &std::collections::BTreeMap::new())
                .expect("a catalog"),
        );
        let position = |name: &str| text.find(&format!("- {name}:")).expect(name);
        assert!(
            position("cert-renewal") < position("invoice-export"),
            "{text}"
        );
    }

    #[test]
    fn the_catalog_is_bounded_and_says_what_it_left_out() {
        let directory = tempfile::tempdir().expect("dir");
        let many = (0..60)
            .map(|number| {
                (
                    format!("skill-number-{number:02}"),
                    format!(
                        "Use when asked to handle case {number} of the {} kind.",
                        "very long ".repeat(12)
                    ),
                    "Do it.".to_owned(),
                )
            })
            .collect::<Vec<_>>();
        let borrowed = many
            .iter()
            .map(|(name, description, body)| (name.as_str(), description.as_str(), body.as_str()))
            .collect::<Vec<_>>();
        let layers = disk_library(directory.path(), &borrowed);
        let catalog = super::catalog(&library(&layers), &std::collections::BTreeMap::new())
            .expect("a catalog");
        let text = text_of(&catalog);
        assert!(
            catalog.listed <= super::CATALOG_LINES && catalog.listed > 0,
            "{}",
            catalog.listed
        );
        assert_eq!(catalog.listed + catalog.left_out, 60);
        assert!(catalog.left_out > 0);
        assert!(
            text.contains(&format!(
                "({} more not listed; list_skills shows all.)",
                catalog.left_out
            )),
            "{text}"
        );
        // The list within its bound (the sentence above and the tags around it excepted).
        assert!(text.len() < super::CATALOG_CHARS + 1_200, "{}", text.len());
    }
}
