//! `ha learning`: the learning loop's lessons (`docs/architecture/LEARNING_LOOP.en.md`).

use clap::{Args, Subcommand};
use harness_types::{ErrorCode, HarnessError};

use crate::interactive::{cli_extras, harness, lessons};

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
    }
}
