//! The WAL checkpoint, moved off the writer's path.
//!
//! `SQLite` copies the write-ahead log back into the database - a checkpoint -
//! on the connection whose commit makes the log long enough, and again when the
//! last connection closes. On a Windows disk with antivirus scanning that copy
//! and its flushes cost 0.4 to 1.4 s: every eighteenth tool call or so paused
//! for it, and every store close (the end of each turn) waited for it.
//!
//! A writer store therefore keeps a second connection whose only job is a
//! `PASSIVE` checkpoint once the writer has gone quiet - in a turn, while the
//! model is thinking. A passive checkpoint never blocks the writer, so the copy
//! is done in time nobody is waiting on, and the one at close finds little
//! left. The writer's own automatic checkpoint is raised to 16 MiB of log and
//! stays as the safety net for a writer that is never quiet. Measured with a
//! 600 ms model over 20 tool calls: the host's share of a run went from 2.6-3.5
//! s to 1.7-1.9 s, and the slowest tool call from 1.5 s to 0.27 s.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use tokio::{sync::Notify, task::JoinHandle};

/// How long the writer must be quiet before the checkpointer copies, so the
/// copy does not compete with the next commit.
const CHECKPOINT_PAUSE: Duration = Duration::from_millis(250);

/// Commits the log gathers before a background checkpoint is worth its
/// flushes - about five pages each, so roughly `SQLite`'s own 1,000-page
/// threshold. Checkpointing at every pause instead cost a flush per tool call:
/// with a 1.5 s model each call took 150-500 ms instead of 10-30.
const CHECKPOINT_COMMITS: u64 = 200;

/// What the writer's connection tells its checkpointer: one more commit.
#[derive(Default)]
pub(crate) struct CommitSignal {
    commits: AtomicU64,
    wake: Notify,
}

impl CommitSignal {
    /// Called from the writer's commit hook: count it, and wake the
    /// checkpointer once enough have gathered.
    pub(crate) fn committed(&self) {
        if self.commits.fetch_add(1, Ordering::Relaxed) + 1 >= CHECKPOINT_COMMITS {
            self.wake.notify_one();
        }
    }
}

/// The background checkpoint of one writer store.
pub(crate) struct Checkpointer {
    pool: SqlitePool,
    task: Option<JoinHandle<()>>,
}

impl Checkpointer {
    /// Start checkpointing `database` as `signal` reports commits.
    ///
    /// The connection is opened on the first checkpoint, not here, so a store
    /// that is opened and closed without writing much pays nothing for it.
    pub(crate) fn start(
        database: &Path,
        busy_timeout: Duration,
        signal: Arc<CommitSignal>,
    ) -> Self {
        let options = SqliteConnectOptions::new()
            .filename(database)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(busy_timeout);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_lazy_with(options);
        let checkpoints = pool.clone();
        let task = tokio::spawn(async move {
            loop {
                signal.wake.notified().await;
                // Wait for the writer to go quiet - in a turn, that is while
                // the model is thinking - so the copy never competes with it.
                let mut seen = signal.commits.load(Ordering::Relaxed);
                loop {
                    tokio::time::sleep(CHECKPOINT_PAUSE).await;
                    let now = signal.commits.load(Ordering::Relaxed);
                    if now == seen {
                        break;
                    }
                    seen = now;
                }
                signal.commits.fetch_sub(seen, Ordering::Relaxed);
                // A failed or partial checkpoint is not an error: the log is
                // still durable, and the next one (or the writer's own) copies
                // what this one could not.
                let _ = sqlx::query("PRAGMA wal_checkpoint(PASSIVE)")
                    .execute(&checkpoints)
                    .await;
            }
        });
        Self {
            pool,
            task: Some(task),
        }
    }

    /// Stop checkpointing and close the connection, before the writer closes:
    /// the writer's connection must be the last one, so its close is the one
    /// that finishes the log.
    pub(crate) async fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
        self.pool.close().await;
    }
}

impl Drop for Checkpointer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CHECKPOINT_COMMITS, CHECKPOINT_PAUSE};
    use crate::{SqliteStore, WriterOpenOptions};
    use harness_types::HostId;

    /// Enough commits and then a pause: the log is copied back into the
    /// database by the checkpointer, long before the writer's own 4,000-page
    /// threshold would, and the store still closes and reopens whole.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_quiet_writer_has_its_log_copied_back() {
        let temp = tempfile::tempdir().expect("temp dir");
        let store = SqliteStore::open_writer(WriterOpenOptions::new(
            temp.path().to_owned(),
            HostId::generate(),
        ))
        .await
        .expect("writer");
        let database = store.paths.database_path.clone();
        sqlx::query("CREATE TABLE checkpoint_rows(body TEXT NOT NULL)")
            .execute(&store.pool)
            .await
            .expect("table");
        let body = "x".repeat(2048);
        for _ in 0..=CHECKPOINT_COMMITS {
            sqlx::query("INSERT INTO checkpoint_rows(body) VALUES (?)")
                .bind(&body)
                .execute(&store.pool)
                .await
                .expect("row");
        }
        let written = std::fs::metadata(&database).expect("database").len();
        let mut copied = written;
        for _ in 0..40 {
            tokio::time::sleep(CHECKPOINT_PAUSE).await;
            copied = std::fs::metadata(&database).expect("database").len();
            if copied > written {
                break;
            }
        }
        assert!(
            copied > written + 400 * 1024,
            "the rows reached the database file: {written} -> {copied} bytes"
        );
        store.close().await.expect("close");
        let reopened = SqliteStore::open_writer(WriterOpenOptions::new(
            temp.path().to_owned(),
            HostId::generate(),
        ))
        .await
        .expect("reopen");
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM checkpoint_rows")
            .fetch_one(&reopened.pool)
            .await
            .expect("count");
        assert_eq!(
            rows,
            i64::try_from(CHECKPOINT_COMMITS + 1).expect("count fits")
        );
        reopened.close().await.expect("close");
    }
}
