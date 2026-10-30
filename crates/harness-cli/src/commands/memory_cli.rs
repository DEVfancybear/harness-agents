//! `ha memory`: the memory stores from the command line
//! (`docs/architecture/MEMORY_STORE.en.md`), and a project's markdown memory
//! store (`docs/architecture/MARKDOWN_MEMORY.en.md`).

use std::path::PathBuf;

use clap::{Args, Subcommand};
use harness_types::{ErrorCode, HarnessError};

use crate::interactive::{cli_extras, harness, markdown_memory, memory_store};

#[derive(Debug, Args)]
pub struct MemoryCommand {
    #[command(subcommand)]
    command: MemorySubcommand,
}

#[derive(Debug, Subcommand)]
enum MemorySubcommand {
    /// Print a store's whole state as JSON: its entries and refinements, in
    /// `harness_state.json`'s shape.
    Export {
        /// A conversation's own store instead of the global one.
        #[arg(long)]
        conversation: Option<String>,
    },
    /// Forget a memory entry for good: the entry, every version the log
    /// kept of it, its use counts and its copies beside the store
    /// (refinements, the imported legacy files). One event saying it was
    /// purged, with no content, remains. With --secrets, every entry and
    /// version that holds a secret value. Lists what it would remove and
    /// changes nothing without --yes.
    Purge(PurgeArgs),
    /// Check a markdown memory store (MEMORY.md and memory/) as
    /// markdown-agent-memory's memory-lint does: boot-file caps, provenance tags
    /// on new fact lines, deleted history, edited archives, dead index paths.
    /// Exits 1 when a check fails, 2 on a bad config.
    Lint(LintArgs),
    /// Write an empty markdown memory store: MEMORY.md, memory-lint.json and the
    /// memory/ tiers. Files that exist are kept.
    Init {
        /// The project root; the working directory when left out.
        #[arg(long)]
        root: Option<PathBuf>,
        /// Also write a git pre-commit hook that runs `ha memory lint --staged`.
        #[arg(long)]
        hook: bool,
        /// Also write example files (a person, a project, a decision, a daily
        /// note) into memory/templates/, which is in no tier.
        #[arg(long)]
        examples: bool,
    },
    /// Turn a flat memory file into the markdown store: the file is archived
    /// whole in memory/archive/, and an agent run (`ha exec`) splits it into
    /// people, projects, decisions and daily notes, tags every fact and
    /// rebuilds MEMORY.md as an index; then the store is linted.
    Migrate(MigrateArgs),
}

#[derive(Debug, Args)]
struct MigrateArgs {
    /// The project root; the working directory when left out.
    #[arg(long)]
    root: Option<PathBuf>,
    /// The flat file; the root's MEMORY.md when left out.
    #[arg(long)]
    from: Option<PathBuf>,
    /// The model of the run, as `ha exec --model` takes it.
    #[arg(long)]
    model: Option<String>,
    /// The thinking level of the run.
    #[arg(long)]
    thinking: Option<String>,
    /// The run's approval mode; it only needs to edit files.
    #[arg(long, default_value = "auto-edit")]
    approval: String,
}

#[derive(Debug, Args)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one flag per memory-lint option"
)]
struct LintArgs {
    /// The store's root; the working directory when left out.
    #[arg(long)]
    root: Option<PathBuf>,
    /// The config file, relative to the root (memory-lint.json).
    #[arg(long)]
    config: Option<PathBuf>,
    /// Check what is staged for the next commit.
    #[arg(long, conflicts_with_all = ["base", "no_diff"])]
    staged: bool,
    /// Check the working tree against this ref.
    #[arg(long, value_name = "REF", conflicts_with = "no_diff")]
    base: Option<String>,
    /// Read no diff: only the caps and the index links.
    #[arg(long)]
    no_diff: bool,
    /// Print the report as JSON.
    #[arg(long)]
    json: bool,
    /// Print only the failures and the result line.
    #[arg(long)]
    quiet: bool,
    /// Print the memory-lint version these checks reproduce.
    #[arg(long)]
    version: bool,
}

fn working_directory() -> Result<PathBuf, HarnessError> {
    std::env::current_dir()
        .map_err(|error| HarnessError::new(ErrorCode::StorageOpenFailed, error.to_string()))
}

fn lint(args: LintArgs) -> Result<(), HarnessError> {
    if args.version {
        println!("{}", markdown_memory::LINT_VERSION);
        return Ok(());
    }
    let mode = if args.staged {
        markdown_memory::DiffMode::Staged
    } else if let Some(base) = args.base {
        markdown_memory::DiffMode::Base(base)
    } else if args.no_diff {
        markdown_memory::DiffMode::None
    } else {
        markdown_memory::DiffMode::WorkingTree
    };
    let options = markdown_memory::LintOptions {
        root: args.root.map_or_else(working_directory, Ok)?,
        config: args.config,
        mode,
    };
    let report = match markdown_memory::lint(&options) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("memory-lint: {error}");
            std::process::exit(2);
        }
    };
    if args.json {
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        print!("{}", report.text(args.quiet));
    }
    // The verdict is the exit code, so a pre-commit hook can block on it.
    if !report.passed() {
        std::process::exit(1);
    }
    Ok(())
}

fn init(root: Option<PathBuf>, hook: bool, examples: bool) -> Result<(), HarnessError> {
    let root = root.map_or_else(working_directory, Ok)?;
    let mut outcome = markdown_memory::init(&root, hook)
        .map_err(|error| HarnessError::new(ErrorCode::StorageWriteFailed, error))?;
    if examples {
        let more = markdown_memory::init_examples(&root)
            .map_err(|error| HarnessError::new(ErrorCode::StorageWriteFailed, error))?;
        outcome.written.extend(more.written);
        outcome.kept.extend(more.kept);
    }
    for path in &outcome.written {
        println!("wrote {path}");
    }
    for path in &outcome.kept {
        println!("kept  {path} (already there)");
    }
    match &outcome.hook {
        None => {}
        Some(markdown_memory::HookOutcome::Written(path)) => {
            println!(
                "wrote {} (runs `ha memory lint --staged --quiet`)",
                path.display()
            );
        }
        Some(markdown_memory::HookOutcome::Exists(path)) => println!(
            "kept  {} (a pre-commit hook exists: have it run `ha memory lint --staged --quiet` and block the commit when that exits non-zero)",
            path.display()
        ),
        Some(markdown_memory::HookOutcome::NoRepository) => {
            println!("no hook: {} is not in a git repository", root.display());
        }
    }
    println!(
        "ha puts MEMORY.md and the memory rules in every turn of a project whose root is {}; `ha memory lint` checks the store",
        root.display()
    );
    Ok(())
}

fn migrate(args: MigrateArgs) -> Result<(), HarnessError> {
    let failed = |error: String| HarnessError::new(ErrorCode::StorageWriteFailed, error);
    let root = args.root.map_or_else(working_directory, Ok)?;
    if !root.join(markdown_memory::CONFIG_FILE).is_file() {
        // The store's rules reach the run only when the store is there.
        let outcome = markdown_memory::init(&root, false).map_err(failed)?;
        for path in &outcome.written {
            println!("wrote {path}");
        }
    }
    let source = args
        .from
        .map_or_else(|| root.join("MEMORY.md"), |from| root.join(from));
    let archived = markdown_memory::archive_for_migration(&root, &source).map_err(failed)?;
    println!("archived {} whole as {archived}", source.display());
    let mut command = std::process::Command::new(
        std::env::current_exe().map_err(|error| failed(error.to_string()))?,
    );
    // The lint below checks the migration. A self-evaluation of it reads only
    // the turn's last tool results: it sent a whole migration back for an
    // archive read it could not see, and the second pass reached for a tool
    // the run's approval refuses.
    command.current_dir(&root).env("HA_SELF_EVAL", "off").args([
        "exec",
        "--approval",
        args.approval.as_str(),
    ]);
    if let Some(model) = &args.model {
        command.args(["--model", model]);
    }
    if let Some(thinking) = &args.thinking {
        command.args(["--thinking", thinking]);
    }
    command.arg(markdown_memory::migration_prompt(&archived));
    let status = command
        .status()
        .map_err(|error| failed(format!("cannot start the migration run: {error}")))?;
    if !status.success() {
        eprintln!(
            "memory migrate: the run ended with {status}; the flat file is kept at {archived}"
        );
        std::process::exit(1);
    }
    let report = markdown_memory::lint(&markdown_memory::LintOptions {
        root: root.clone(),
        config: None,
        mode: markdown_memory::DiffMode::WorkingTree,
    })
    .map_err(failed)?;
    print!("{}", report.text(false));
    if !report.passed() {
        eprintln!(
            "memory migrate: fix the FAIL lines above (or run `ha memory migrate` again); the flat file is kept at {archived}"
        );
        std::process::exit(1);
    }
    Ok(())
}

/// Which entries `ha memory purge` reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
enum PurgeScope {
    /// The global store's entries kept for every project.
    Global,
    /// The global store's entries kept for this project (`--cwd`).
    Project,
    /// A conversation's own store (`--conversation`, or every one with
    /// `--secrets`).
    Local,
}

#[derive(Debug, Args)]
struct PurgeArgs {
    /// The entry: its immutable memory id or its key.
    #[arg(required_unless_present = "secrets", conflicts_with = "secrets")]
    target: Option<String>,
    /// Every entry and event that holds a secret value (a key, a token, a
    /// private key), in the global store and every conversation's.
    #[arg(long)]
    secrets: bool,
    /// Only these entries, and only their own copies: with a scope,
    /// --secrets leaves other entries' reviews and the imported files alone.
    #[arg(long, value_enum)]
    scope: Option<PurgeScope>,
    /// A conversation's own store (with --scope local, or no scope).
    #[arg(long)]
    conversation: Option<String>,
    /// The project, for `--scope project`; the working directory when left out.
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Purge; without it, list what would go, reading the store read-only.
    #[arg(long)]
    yes: bool,
}

/// `ha memory purge`: forget an entry for good, or every one holding a
/// secret. Lists first; `--yes` purges.
#[allow(clippy::too_many_lines, reason = "one command: find, list, purge")]
fn purge(args: &PurgeArgs) -> Result<(), HarnessError> {
    let failed = |error: String| HarnessError::new(ErrorCode::StorageWriteFailed, error);
    let data = cli_extras::current_data_dir()?;
    if args.scope == Some(PurgeScope::Local) && args.conversation.is_none() && !args.secrets {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "--scope local needs --conversation <id>",
        ));
    }
    if args.conversation.is_some()
        && matches!(args.scope, Some(PurgeScope::Global | PurgeScope::Project))
    {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "--conversation names a conversation's own store: it goes with --scope local, or no scope",
        ));
    }
    // Every secret value of a store's files is redacted only when no scope
    // bounds the purge: a scope purges its own entries' copies and no others.
    let redact_all = args.secrets && args.scope.is_none();
    let project = match args.scope {
        Some(PurgeScope::Project) => {
            let cwd = match &args.cwd {
                Some(cwd) => cwd.clone(),
                None => working_directory()?,
            };
            Some(crate::interactive::bootstrap::project_key_of(&cwd))
        }
        _ => None,
    };
    // The stores read, each with how its entries are named in the listing.
    let mut stores = Vec::new();
    if args.scope != Some(PurgeScope::Local) {
        stores.push(("global".to_owned(), harness::global_dir(&data)));
    }
    match &args.conversation {
        Some(task) => stores.push((format!("local {task}"), harness::local_dir(&data, task))),
        None if args.secrets && args.scope.is_none_or(|scope| scope == PurgeScope::Local) => {
            let mut tasks = std::fs::read_dir(data.join("sessions"))
                .map(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .map(|entry| entry.file_name().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            tasks.sort();
            for task in tasks {
                let dir = harness::local_dir(&data, &task);
                if memory_store::db_path(&dir).is_file() {
                    stores.push((format!("local {task}"), dir));
                }
            }
        }
        None => {}
    }
    // Which of a store's entries the scope keeps: a global store's entry is
    // the project's when it says so.
    let in_scope = |label: &str, doc: &serde_json::Value| {
        if !label.starts_with("global") {
            return true;
        }
        let tag = harness::project_key(doc.get("project"));
        match args.scope {
            Some(PurgeScope::Global) => tag.is_none(),
            Some(PurgeScope::Project) => tag.is_some() && tag == project,
            _ => true,
        }
    };
    let mut found = Vec::new();
    for (label, dir) in &stores {
        let targets = if args.secrets {
            memory_store::secret_targets(dir)
        } else {
            memory_store::purge_targets(dir, args.target.as_deref().unwrap_or_default())
                .map(|targets| targets.into_iter().map(|target| (target, "")).collect())
        }
        .map_err(|error| HarnessError::new(ErrorCode::StorageOpenFailed, error))?;
        let targets = targets
            .into_iter()
            .filter(|(target, _)| in_scope(label, &target.doc))
            .collect::<Vec<_>>();
        let plain = targets
            .iter()
            .map(|(target, _)| target.clone())
            .collect::<Vec<_>>();
        let copies = memory_store::purge_plan(dir, &plain, redact_all)
            .map_err(|error| HarnessError::new(ErrorCode::StorageOpenFailed, error))?;
        for file in memory_store::pending_legacy(dir) {
            println!(
                "[{label}] {file} waits to be imported: it is not read here, and the next write to the store imports it"
            );
        }
        if !targets.is_empty() || copies.any() {
            found.push((label.clone(), dir.clone(), targets, copies));
        }
    }
    if found.is_empty() {
        if args.secrets {
            println!("No entry or event of the memory stores holds a secret value.");
            return Ok(());
        }
        return Err(HarnessError::new(
            ErrorCode::ResourceNotFound,
            format!(
                "no memory entry is kept under {:?} in the stores read",
                args.target.as_deref().unwrap_or_default()
            ),
        ));
    }
    let verb = if args.yes { "purge" } else { "would purge" };
    for (label, _, targets, plan) in &found {
        if plan.any() {
            println!(
                "{verb} [{label}] copies beside the store: {} refinement summary(ies), {} record(s) of refinements.jsonl, {} imported legacy line(s) or entr(ies)",
                plan.refinements, plan.history, plan.legacy
            );
        }
        for (target, shape) in targets {
            // A key may be made from a title that held the secret.
            let key = harness_tools::secret_values::redact(&target.key);
            let what = if target.whole {
                format!(
                    "the {} entry and its {} event(s)",
                    if target.live { "live" } else { "deleted" },
                    target.logged
                )
            } else {
                format!(
                    "{} of its {} version(s)",
                    target.events.len(),
                    target.logged
                )
            };
            let held = if shape.is_empty() {
                String::new()
            } else {
                format!(": holds a secret value ({shape})")
            };
            println!(
                "{verb} [{label}] {}:{key} ({}): {what}{held}",
                target.kind, target.memory
            );
        }
    }
    if !args.yes {
        println!("Nothing was changed. Run again with --yes to purge.");
        return Ok(());
    }
    let writer = memory_store::Writer::new(
        "cli",
        &serde_json::json!({"via": "ha memory purge"}),
        "purge",
    );
    for (label, dir, targets, _) in &found {
        let _lock = harness::lock_state(dir).map_err(failed)?;
        let targets = targets
            .iter()
            .map(|(target, _)| target.clone())
            .collect::<Vec<_>>();
        let purged = memory_store::purge(dir, &targets, &writer, redact_all).map_err(failed)?;
        println!(
            "[{label}] purged {} memory(ies), {} event(s) removed; one purge event each remains; {} refinement summary(ies), {} record(s) of refinements.jsonl and {} imported legacy line(s) or entr(ies) rewritten",
            targets.len(),
            purged.removed,
            purged.rewritten.refinements,
            purged.rewritten.history,
            purged.rewritten.legacy
        );
        if !purged.settled {
            println!(
                "[{label}] another process reads the store: its log is emptied when that process closes it"
            );
        }
    }
    Ok(())
}

/// Run one `ha memory` command.
///
/// # Errors
/// The data directory cannot be resolved, the store cannot be read, or the
/// markdown store cannot be written.
pub fn run(command: MemoryCommand) -> Result<(), HarnessError> {
    match command.command {
        MemorySubcommand::Lint(args) => lint(args),
        MemorySubcommand::Init {
            root,
            hook,
            examples,
        } => init(root, hook, examples),
        MemorySubcommand::Migrate(args) => migrate(args),
        MemorySubcommand::Purge(args) => purge(&args),
        MemorySubcommand::Export { conversation } => {
            let data = cli_extras::current_data_dir()?;
            let (scope, dir) = match &conversation {
                Some(task) => ("local", harness::local_dir(&data, task)),
                None => ("global", harness::global_dir(&data)),
            };
            let document = memory_store::load_document(&dir)
                .map_err(|error| HarnessError::new(ErrorCode::StorageOpenFailed, error))?;
            println!(
                "{}",
                serde_json::json!({
                    "schema_version": 1,
                    "scope": scope,
                    "store": memory_store::db_path(&dir).display().to_string(),
                    "document": document,
                })
            );
            Ok(())
        }
    }
}
