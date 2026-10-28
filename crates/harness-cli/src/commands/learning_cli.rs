//! `ha learning`: the learning loop's lessons (`docs/architecture/LEARNING_LOOP.en.md`).

use clap::{Args, Subcommand};
use harness_types::{ErrorCode, HarnessError};

use crate::interactive::{cli_extras, harness, lessons, memory};

#[derive(Debug, Args)]
pub struct LearningCommand {
    #[command(subcommand)]
    command: LearningSubcommand,
}

#[derive(Debug, Subcommand)]
enum LearningSubcommand {
    /// The lessons: candidates and learned rules (retired ones with --all).
    List {
        /// Retired lessons too.
        #[arg(long)]
        all: bool,
        /// The lessons as JSON.
        #[arg(long)]
        json: bool,
    },
    /// One lesson: its signals, with their quotes, and what happened to it.
    Show { id: String },
    /// Run the gate now: promote what passes, retire what two conversations
    /// contradicted.
    Distill,
    /// Retire a lesson, undoing its rule: the memory entry is deleted, or the
    /// markdown lines are struck through.
    Retire {
        id: String,
        /// Why, kept with the lesson.
        #[arg(long, default_value = "retired by the user")]
        reason: String,
    },
    /// What recurs: rules still corrected, rules and whether they worked,
    /// candidates by how often they recur, and facts.
    Report {
        /// Retired lessons too.
        #[arg(long)]
        all: bool,
        /// Also write the report as a page to this file.
        #[arg(long, value_name = "FILE")]
        html: Option<std::path::PathBuf>,
    },
    /// The open lessons by area of work, each with its confidence.
    Status,
    /// Write the open lessons, with their quotes, as a document another
    /// machine imports.
    Export {
        /// Write to this file instead of the standard output.
        #[arg(short, long, value_name = "FILE")]
        output: Option<std::path::PathBuf>,
        /// Only lessons at least this sure (0 to 1).
        #[arg(long, default_value_t = 0.0)]
        min_confidence: f64,
        /// Which lessons: global, project or all.
        #[arg(long, default_value = "all")]
        scope: String,
    },
    /// Bring in lessons an export holds: a lesson already here gains the
    /// signals it lacks, a new one waits for the gate as a candidate.
    Import {
        file: std::path::PathBuf,
        /// Say what would change, and change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Only lessons at least this sure (0 to 1).
        #[arg(long, default_value_t = 0.0)]
        min_confidence: f64,
        /// Which lessons: global, project or all. A project lesson becomes
        /// the current directory's project's.
        #[arg(long, default_value = "all")]
        scope: String,
    },
    /// Make a project's lesson one for every project; the gate then writes
    /// its rule where a rule for every project goes.
    Promote { id: String },
}

fn scope_filter(scope: &str) -> Result<lessons::ScopeFilter, HarnessError> {
    match scope {
        "global" => Ok(lessons::ScopeFilter::Global),
        "project" => Ok(lessons::ScopeFilter::Project),
        "all" => Ok(lessons::ScopeFilter::All),
        other => Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            format!("scope must be global, project or all, not {other:?}"),
        )),
    }
}

/// The project the current directory is, for a project lesson brought in.
fn current_project() -> Option<lessons::ProjectRef> {
    let root = std::env::current_dir().ok()?;
    let tag = memory::ProjectTag::of(&root);
    Some(lessons::ProjectRef {
        key: tag.key,
        name: tag.name,
        root: root.display().to_string(),
    })
}

fn failed(error: String) -> HarnessError {
    HarnessError::new(ErrorCode::StorageWriteFailed, error)
}

/// Run one `ha learning` command.
///
/// # Errors
/// The data directory cannot be resolved, the lessons cannot be read or
/// written, or there is no such lesson.
pub fn run(command: LearningCommand) -> Result<(), HarnessError> {
    let global = harness::global_dir(&cli_extras::current_data_dir()?);
    let places = lessons::Places {
        global: global.clone(),
    };
    let now = chrono::Utc::now();
    match command.command {
        LearningSubcommand::List { all, json } => {
            let kept = lessons::load(&global).map_err(failed)?;
            let shown = kept
                .lessons
                .iter()
                .filter(|lesson| all || lesson.state != lessons::LessonState::Retired)
                .collect::<Vec<_>>();
            if json {
                println!("{}", serde_json::to_string(&shown).unwrap_or_default());
            } else if shown.is_empty() {
                println!(
                    "no lessons yet: the automatic review records them as conversations show them"
                );
            } else {
                for lesson in shown {
                    println!("{}", lessons::line(lesson, now));
                }
            }
            Ok(())
        }
        LearningSubcommand::Show { id } => {
            let kept = lessons::load(&global).map_err(failed)?;
            let lesson = kept
                .find(&id)
                .ok_or_else(|| failed(format!("no lesson {id}")))?;
            println!("{}", lessons::line(lesson, now));
            if let Some(placement) = &lesson.placement {
                println!(
                    "rule: {}",
                    serde_json::to_string(placement).unwrap_or_default()
                );
            }
            println!("signals:");
            for signal in &lesson.signals {
                println!(
                    "  {} {} {}\"{}\"",
                    signal.at.get(..10).unwrap_or(&signal.at),
                    signal.conversation,
                    if signal.against { "against " } else { "" },
                    signal.evidence
                );
            }
            println!("history:");
            for event in &lesson.events {
                println!(
                    "  {} {} {}",
                    event.at.get(..19).unwrap_or(&event.at),
                    event.what,
                    event.why
                );
            }
            Ok(())
        }
        LearningSubcommand::Distill => {
            let distilled = lessons::distill(&places, now).map_err(failed)?;
            for line in distilled.lines() {
                println!("{line}");
            }
            for (id, why) in &distilled.held {
                println!("held {id}: {why}");
            }
            if distilled.is_empty() && distilled.held.is_empty() {
                println!("no lesson passed the gate");
            }
            Ok(())
        }
        LearningSubcommand::Retire { id, reason } => {
            let lesson = lessons::retire(&places, &id, &reason).map_err(failed)?;
            println!("retired {}: {}", lesson.id, lesson.text);
            Ok(())
        }
        LearningSubcommand::Report { all, html } => {
            let kept = lessons::load(&global).map_err(failed)?;
            print!("{}", lessons::report(&kept, now, all));
            if let Some(path) = html {
                std::fs::write(&path, lessons::report_html(&kept, now, all))
                    .map_err(|error| failed(format!("cannot write {}: {error}", path.display())))?;
                println!("wrote {}", path.display());
            }
            Ok(())
        }
        LearningSubcommand::Status => {
            let kept = lessons::load(&global).map_err(failed)?;
            print!("{}", lessons::status(&kept, now));
            Ok(())
        }
        LearningSubcommand::Export {
            output,
            min_confidence,
            scope,
        } => {
            let kept = lessons::load(&global).map_err(failed)?;
            let document = lessons::export(&kept, now, min_confidence, scope_filter(&scope)?);
            let text = serde_json::to_string_pretty(&document).unwrap_or_default();
            let count = document["lessons"].as_array().map_or(0, Vec::len);
            match output {
                Some(path) => {
                    std::fs::write(&path, text).map_err(|error| {
                        failed(format!("cannot write {}: {error}", path.display()))
                    })?;
                    println!("exported {count} lesson(s) to {}", path.display());
                }
                None => println!("{text}"),
            }
            Ok(())
        }
        LearningSubcommand::Import {
            file,
            dry_run,
            min_confidence,
            scope,
        } => {
            let text = std::fs::read_to_string(&file)
                .map_err(|error| failed(format!("cannot read {}: {error}", file.display())))?;
            let document = serde_json::from_str::<serde_json::Value>(&text)
                .map_err(|error| failed(format!("{} is not JSON: {error}", file.display())))?;
            let project = current_project();
            let imported = lessons::import(
                &global,
                &document,
                min_confidence,
                scope_filter(&scope)?,
                project.as_ref(),
                dry_run,
            )
            .map_err(failed)?;
            let verb = if dry_run { "would add" } else { "added" };
            for id in &imported.added {
                println!("{verb} {id}");
            }
            for id in &imported.merged {
                println!(
                    "{} signals to {id}",
                    if dry_run { "would add" } else { "added" }
                );
            }
            for (id, why) in &imported.skipped {
                println!("skipped {id}: {why}");
            }
            println!(
                "{} new, {} merged, {} skipped{}",
                imported.added.len(),
                imported.merged.len(),
                imported.skipped.len(),
                if dry_run {
                    " (dry run: nothing changed)"
                } else {
                    ""
                }
            );
            if !dry_run {
                for line in lessons::distill(&places, now).map_err(failed)?.lines() {
                    println!("{line}");
                }
            }
            Ok(())
        }
        LearningSubcommand::Promote { id } => {
            let lesson = lessons::promote_global(&places, &id).map_err(failed)?;
            println!(
                "{} is a lesson for every project now: {}",
                lesson.id, lesson.text
            );
            let distilled = lessons::distill(&places, now).map_err(failed)?;
            for line in distilled.lines() {
                println!("{line}");
            }
            for (held, why) in &distilled.held {
                println!("held {held}: {why}");
            }
            Ok(())
        }
    }
}
