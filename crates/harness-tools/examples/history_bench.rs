//! Time the journal index behind `history_search` and `history_read`.
//!
//! ```text
//! cargo run --release -p harness-tools --example history_bench -- [--events N] [--sessions N] [--rounds N]
//! ```
//!
//! A task's journal is filled with `--events` notes of prose and identifiers,
//! spread over `--sessions` sessions the way a long task's turns are,
//! then the index is built the way the first `history_search` of a long task
//! builds it, searched while up to date, searched after a few new entries, and
//! read page by page.

use std::{
    fmt::Write as _,
    sync::Arc,
    time::{Duration, Instant},
};

use harness_session::{AdmitInputRequest, SessionService};
use harness_store_sqlite::{HistoryScope, SqliteStore, WriterOpenOptions};
use harness_tools::observe_workspace;
use harness_types::{HostId, InputId, ProjectId, SessionId, SourceAuthority, TaskId};

fn arg(name: &str, default: usize) -> usize {
    let mut words = std::env::args().skip(1);
    while let Some(word) = words.next() {
        if word == name {
            return words.next().and_then(|n| n.parse().ok()).unwrap_or(default);
        }
    }
    default
}

const WORDS: [&str; 24] = [
    "parser",
    "renderer",
    "budget",
    "receipt",
    "workspace",
    "compaction",
    "provider",
    "session",
    "approval",
    "journal",
    "snapshot",
    "kernel",
    "worker",
    "schedule",
    "heartbeat",
    "fixture",
    "checkpoint",
    "migration",
    "token",
    "window",
    "delegate",
    "verifier",
    "feature",
    "release",
];

fn note(index: usize) -> String {
    let mut text = format!("Step {index}: ");
    for word in 0..(40 + index % 120) {
        text.push_str(WORDS[(index * 7 + word * 13) % WORDS.len()]);
        text.push(' ');
    }
    if index.is_multiple_of(97) {
        let _ = write!(text, "the release marker is XYZ-{index}. ");
    }
    text
}

async fn append(store: &Arc<SqliteStore>, session: &SessionId, task: &TaskId, text: String) {
    let mut payload = serde_json::Map::new();
    payload.insert("text".to_owned(), serde_json::Value::String(text));
    SessionService::new(Arc::clone(store))
        .append_runtime_event(session, task, "ops.note", payload, false)
        .await
        .expect("note appended");
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn report(name: &str, mut times: Vec<Duration>) {
    times.sort();
    let median = times[times.len() / 2];
    let worst = times[times.len() - 1];
    println!(
        "{name:<34} median {:>8.2} ms   max {:>8.2} ms",
        ms(median),
        ms(worst)
    );
}

#[tokio::main]
#[allow(clippy::too_many_lines, reason = "one benchmark, read top to bottom")]
async fn main() {
    let events = arg("--events", 2000);
    let rounds = arg("--rounds", 20);
    let sessions = arg("--sessions", 1).max(1);
    let temp = std::env::temp_dir().join(format!("ha-history-bench-{}", InputId::generate()));
    let workspace = temp.join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(
            temp.join("data"),
            HostId::generate(),
        ))
        .await
        .expect("store opens"),
    );
    let project = ProjectId::generate();
    let task = TaskId::generate();
    let mut session = SessionId::generate();
    let started = Instant::now();
    for turn in 0..sessions {
        if turn > 0 {
            // A new turn's session takes the task over, as a continuation does.
            store
                .release_task_lease(&task)
                .await
                .expect("lease released");
        }
        session = SessionId::generate();
        SessionService::new(Arc::clone(&store))
            .admit_input(AdmitInputRequest {
                session_id: session.clone(),
                task_id: task.clone(),
                input_id: InputId::generate(),
                expected_sequence: 1,
                authority: SourceAuthority::User,
                raw_text: format!("benchmark the journal index, turn {turn}"),
                workspace: observe_workspace(project.clone(), &workspace).expect("observation"),
                initial_plan_items: Vec::new(),
            })
            .await
            .expect("admitted");
        for index in (turn * events / sessions)..((turn + 1) * events / sessions) {
            append(&store, &session, &task, note(index)).await;
        }
    }
    println!(
        "journal: {events} entries in {sessions} sessions written in {:.1}s",
        started.elapsed().as_secs_f64()
    );

    let started = Instant::now();
    let indexed = store.index_task_history(&task).await.expect("index");
    println!(
        "{:<34} {:>8.0} ms   ({indexed} sources)",
        "first index of the task",
        ms(started.elapsed())
    );

    let scope = HistoryScope::new(project, task.clone());
    let mut common = Vec::new();
    let mut rare = Vec::new();
    let mut both = Vec::new();
    for _ in 0..rounds {
        let started = Instant::now();
        store.index_task_history(&task).await.expect("index");
        let hits = store
            .history_search(&scope, "parser budget", 10)
            .await
            .expect("search");
        common.push(started.elapsed());
        assert!(!hits.is_empty());
        let started = Instant::now();
        store.index_task_history(&task).await.expect("index");
        let _ = store
            .history_search(&scope, "XYZ-97", 10)
            .await
            .expect("search");
        rare.push(started.elapsed());
    }
    report("search, common terms (up to date)", common);
    report("search, rare identifier", rare);
    for round in 0..rounds {
        for extra in 0..5 {
            append(&store, &session, &task, note(events + round * 5 + extra)).await;
        }
        let started = Instant::now();
        store.index_task_history(&task).await.expect("index");
        let _ = store
            .history_search(&scope, "release marker", 10)
            .await
            .expect("search");
        both.push(started.elapsed());
    }
    report("search after 5 new entries", both);

    let ids = store.history_source_ids(&session, 200).await.expect("ids");
    let mut reads = Vec::new();
    for (round, id) in ids.iter().enumerate().take(rounds * 3) {
        let started = Instant::now();
        let _ = store
            .history_read(&scope, id, (round as u64 % 3) * 64, 1024)
            .await
            .expect("read");
        reads.push(started.elapsed());
    }
    report("history_read one page", reads);

    if let Ok(store) = Arc::try_unwrap(store) {
        let _ = store.close().await;
    }
    let _ = std::fs::remove_dir_all(&temp);
}
