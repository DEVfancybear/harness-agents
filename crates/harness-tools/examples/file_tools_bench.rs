//! Time the file tools end to end, through the same gate a model call takes:
//! prepare, approval, durable intent, dispatch and receipt.
//!
//! ```text
//! cargo run --release -p harness-tools --example file_tools_bench -- [--files N] [--repo PATH] [--rounds N]
//! ```
//!
//! Without `--repo` a synthetic Git workspace of `--files` source files is
//! built in a temporary folder and every tool runs against it, edits and
//! writes included. With `--repo` only the read-only tools run, against that
//! folder, which is never changed.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use harness_providers::CancellationToken;
use harness_session::{AdmitInputRequest, SessionService};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_tools::{
    ApprovalMode, CodingToolAction, ToolExecutionService, ToolOutput, ToolRequest, TurnLimits,
    TurnObserver, TurnOptions, TurnProgress, execute_action_with_approval, observe_workspace,
};
use harness_types::{HostId, InputId, ProjectId, SessionId, SourceAuthority, TaskId};

struct Quiet;

impl TurnObserver for Quiet {
    fn observe(&self, _progress: TurnProgress) {}
}

struct Args {
    files: usize,
    repo: Option<PathBuf>,
    rounds: usize,
}

fn args() -> Args {
    let mut parsed = Args {
        files: 2000,
        repo: None,
        rounds: 20,
    };
    let mut words = std::env::args().skip(1);
    while let Some(word) = words.next() {
        match word.as_str() {
            "--files" => parsed.files = words.next().and_then(|n| n.parse().ok()).unwrap_or(2000),
            "--rounds" => parsed.rounds = words.next().and_then(|n| n.parse().ok()).unwrap_or(20),
            "--repo" => parsed.repo = words.next().map(PathBuf::from),
            other => eprintln!("ignored argument {other}"),
        }
    }
    parsed
}

/// A file of about 4 KiB that reads like Rust, with one editable marker line.
fn source_file(index: usize) -> String {
    let mut text = format!("//! Module {index}.\n\nuse std::collections::HashMap;\n\n");
    text.push_str(&format!("pub const VALUE_{index}: u64 = 0;\n\n"));
    for function in 0..12 {
        text.push_str(&format!(
            "/// Compute item {function} of module {index}.\npub fn compute_{index}_{function}(input: &HashMap<String, u64>) -> u64 {{\n    let total = input.values().copied().sum::<u64>();\n    total.wrapping_mul({function}).wrapping_add({index})\n}}\n\n"
        ));
    }
    if index % 20 == 0 {
        text.push_str("// NEEDLE_MARKER: a rare literal the search finds in one file in twenty.\n");
    }
    text
}

fn git(root: &Path, arguments: &[&str]) {
    let _ = std::process::Command::new("git")
        .args(arguments)
        .current_dir(root)
        .output();
}

fn synthetic_workspace(root: &Path, files: usize) {
    for index in 0..files {
        let directory = root
            .join("src")
            .join(format!("group_{}", index / 100))
            .join(format!("part_{}", (index / 10) % 10));
        std::fs::create_dir_all(&directory).expect("workspace directory");
        std::fs::write(
            directory.join(format!("module_{index}.rs")),
            source_file(index),
        )
        .expect("workspace file");
    }
    std::fs::write(root.join(".gitignore"), "target/\n").expect("gitignore");
    // A repository, so the tools pay for their Git calls as they would in a
    // real project. Nothing is committed: `git add` writes one object file per
    // source file, which antivirus scanning makes take minutes on Windows.
    git(root, &["init", "-q"]);
}

fn module_path(index: usize) -> String {
    format!(
        "src/group_{}/part_{}/module_{index}.rs",
        index / 100,
        (index / 10) % 10
    )
}

#[derive(Default)]
struct Samples {
    times: Vec<Duration>,
    failures: usize,
    first_failure: Option<String>,
}

impl Samples {
    fn record(&mut self, elapsed: Duration, outcome: Result<&ToolOutput, String>) {
        match outcome {
            Ok(ToolOutput::Denied { code, reason }) => self.fail(format!("{code}: {reason}")),
            Ok(ToolOutput::OutcomeUnknown { reason }) => self.fail(reason.clone()),
            Ok(_) => self.times.push(elapsed),
            Err(error) => self.fail(error),
        }
    }

    fn fail(&mut self, reason: String) {
        self.failures += 1;
        self.first_failure.get_or_insert(reason);
    }

    fn report(&mut self, name: &str) {
        self.times.sort();
        let ms = |duration: Duration| duration.as_secs_f64() * 1000.0;
        if self.times.is_empty() {
            println!(
                "{name:<22} {:>9} {:>9} {:>9} {:>5}  {}",
                "-",
                "-",
                "-",
                self.failures,
                self.first_failure.as_deref().unwrap_or_default()
            );
            return;
        }
        let median = self.times[self.times.len() / 2];
        let p90 = self.times[(self.times.len() * 9 / 10).min(self.times.len() - 1)];
        let mean =
            self.times.iter().sum::<Duration>() / u32::try_from(self.times.len()).unwrap_or(1);
        println!(
            "{name:<22} {:>9.1} {:>9.1} {:>9.1} {:>5}  {}",
            ms(median),
            ms(p90),
            ms(mean),
            self.failures,
            self.first_failure.as_deref().unwrap_or_default()
        );
    }
}

struct Bench {
    tools: ToolExecutionService,
    options: TurnOptions,
    session: SessionId,
    task: TaskId,
    observer: Arc<dyn TurnObserver>,
    sequence: u32,
}

impl Bench {
    async fn run(&mut self, action: CodingToolAction) -> (Duration, Result<ToolOutput, String>) {
        self.sequence += 1;
        let request = ToolRequest::new(
            self.session.clone(),
            self.task.clone(),
            "bench.actor",
            self.options.workspace_root.clone(),
            action,
        );
        let started = Instant::now();
        let result = execute_action_with_approval(
            &self.tools,
            request,
            &self.options,
            self.sequence,
            &self.observer,
            &CancellationToken::new(),
        )
        .await;
        (
            started.elapsed(),
            result
                .map(|view| view.output)
                .map_err(|error| error.to_string()),
        )
    }

    async fn measure(&mut self, name: &str, actions: Vec<CodingToolAction>) {
        let mut samples = Samples::default();
        for action in actions {
            let (elapsed, outcome) = self.run(action).await;
            samples.record(elapsed, outcome.as_ref().map_err(Clone::clone));
        }
        samples.report(name);
    }
}

#[tokio::main]
async fn main() {
    let args = args();
    let temp = std::env::temp_dir().join(format!("ha-file-tools-bench-{}", InputId::generate()));
    let store_dir = temp.join("store");
    std::fs::create_dir_all(&store_dir).expect("store directory");
    let (workspace, mutate) = match &args.repo {
        Some(repo) => (std::fs::canonicalize(repo).expect("repo path"), false),
        None => {
            let workspace = temp.join("workspace");
            std::fs::create_dir_all(&workspace).expect("workspace");
            let started = Instant::now();
            synthetic_workspace(&workspace, args.files);
            println!(
                "synthetic workspace: {} files in {:.1}s",
                args.files,
                started.elapsed().as_secs_f64()
            );
            (std::fs::canonicalize(&workspace).expect("workspace"), true)
        }
    };
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(store_dir, HostId::generate()))
            .await
            .expect("store opens"),
    );
    let session = SessionId::generate();
    let task = TaskId::generate();
    SessionService::new(Arc::clone(&store))
        .admit_input(AdmitInputRequest {
            session_id: session.clone(),
            task_id: task.clone(),
            input_id: InputId::generate(),
            expected_sequence: 1,
            authority: SourceAuthority::User,
            raw_text: "benchmark the file tools".to_owned(),
            workspace: observe_workspace(ProjectId::generate(), &workspace)
                .expect("workspace observation"),
            initial_plan_items: Vec::new(),
        })
        .await
        .expect("input admitted");
    let mut bench = Bench {
        tools: ToolExecutionService::new(Arc::clone(&store)),
        options: TurnOptions {
            workspace_root: workspace.clone(),
            actor_id: "bench.actor".to_owned(),
            approvals: ApprovalMode::Auto,
            limits: TurnLimits::default(),
        },
        session,
        task,
        observer: Arc::new(Quiet),
        sequence: 0,
    };
    let rounds = args.rounds;
    let files = if mutate { args.files } else { 0 };
    println!(
        "{:<22} {:>9} {:>9} {:>9} {:>5}",
        "tool (ms)", "median", "p90", "mean", "fail"
    );
    // Warm the caches a long session would have warm.
    let _ = bench.run(CodingToolAction::ListFiles { path: None }).await;
    let read_paths: Vec<String> = if mutate {
        (0..rounds * 3)
            .map(|n| module_path((n * 37) % files))
            .collect()
    } else {
        Vec::new()
    };
    if !read_paths.is_empty() {
        bench
            .measure(
                "read_file",
                read_paths
                    .iter()
                    .map(|path| CodingToolAction::ReadFile {
                        path: path.clone(),
                        offset: None,
                        limit: None,
                    })
                    .collect(),
            )
            .await;
    }
    let literal = if mutate { "NEEDLE_MARKER" } else { "fn main" };
    bench
        .measure(
            "search_text literal",
            (0..rounds)
                .map(|_| CodingToolAction::SearchText {
                    query: literal.to_owned(),
                    path: None,
                    regex: false,
                    case_insensitive: false,
                    glob: None,
                    context_lines: None,
                })
                .collect(),
        )
        .await;
    bench
        .measure(
            "search_text regex",
            (0..rounds)
                .map(|_| CodingToolAction::SearchText {
                    query: r"fn compute_\d+_11\(".to_owned(),
                    path: None,
                    regex: true,
                    case_insensitive: false,
                    glob: None,
                    context_lines: Some(1),
                })
                .collect(),
        )
        .await;
    bench
        .measure(
            "search_text rare",
            (0..rounds)
                .map(|_| CodingToolAction::SearchText {
                    query: "ZZZ_NOT_ANYWHERE_ZZZ".to_owned(),
                    path: None,
                    regex: false,
                    case_insensitive: true,
                    glob: None,
                    context_lines: None,
                })
                .collect(),
        )
        .await;
    bench
        .measure(
            "glob **/*.rs",
            (0..rounds)
                .map(|_| CodingToolAction::Glob {
                    pattern: "**/*.rs".to_owned(),
                    path: None,
                })
                .collect(),
        )
        .await;
    bench
        .measure(
            "list_files",
            (0..rounds)
                .map(|_| CodingToolAction::ListFiles { path: None })
                .collect(),
        )
        .await;
    if mutate {
        let mut edit = Samples::default();
        for round in 0..rounds * 2 {
            let index = (round * 53) % files;
            let (elapsed, outcome) = bench
                .run(CodingToolAction::EditFile {
                    path: module_path(index),
                    old_string: format!("pub const VALUE_{index}: u64 = 0;"),
                    new_string: format!("pub const VALUE_{index}: u64 = 1;"),
                    replace_all: false,
                    edits: Vec::new(),
                })
                .await;
            edit.record(elapsed, outcome.as_ref().map_err(Clone::clone));
        }
        edit.report("edit_file");
        bench
            .measure(
                "write_file (new)",
                (0..rounds)
                    .map(|n| CodingToolAction::WriteFile {
                        path: format!("notes/new_{n}.md"),
                        content: format!("# Note {n}\n\nWritten by the benchmark.\n"),
                        expected_hash: None,
                    })
                    .collect(),
            )
            .await;
    }
    drop(bench);
    if let Ok(store) = Arc::try_unwrap(store) {
        let _ = store.close().await;
    }
    let _ = std::fs::remove_dir_all(&temp);
}
