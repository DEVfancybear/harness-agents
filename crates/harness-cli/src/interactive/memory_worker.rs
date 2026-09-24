//! Background fact extraction, off the turn's path.
//!
//! Extraction used to run at the end of every turn, before the turn released the
//! project store, so a turn could end up to the extraction timeout later than its
//! answer. deer-flow runs its memory updater from a debounced queue instead, and this
//! is the same shape:
//!
//! - the turn only **queues** what it said, while its store is still open, and leaves;
//! - the worker waits for the conversation to go quiet for [`DEBOUNCE`], then asks the
//!   model once for a whole batch of turns (at most [`MAX_BATCH`]);
//! - it writes the facts under the same writer gate turns take, so it never holds the
//!   store while a turn wants it;
//! - on exit the queue is flushed, bounded, so the last turn's facts are not lost.

use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use harness_providers::ModelProvider;
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_types::HostId;
use tokio::sync::{Notify, mpsc::UnboundedSender};

use super::events::SessionEvent;
use super::memory::{self, QueuedTurn};

/// How long the conversation must be quiet before a batch is extracted.
///
/// deer-flow waits 30 seconds. A terminal session is quicker than a web chat, and the
/// point is to coalesce a burst of turns, not to delay a single one.
pub const DEBOUNCE: Duration = Duration::from_secs(8);

/// The most turns one extraction reads.
pub const MAX_BATCH: usize = 5;

/// How long a flush on exit may hold the app open.
pub const EXIT_FLUSH: Duration = Duration::from_secs(12);

/// How many times the worker tries to open the writer before it gives up on a batch.
const WRITER_ATTEMPTS: u32 = 5;

/// The queue of turns waiting for extraction, and the task that drains it.
#[derive(Clone)]
pub struct ExtractionWorker {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<State>,
    /// Signalled when the queue becomes empty and nothing is in flight.
    idle: Condvar,
    wake: Notify,
    store_dir: PathBuf,
    writer_gate: Arc<tokio::sync::Mutex<()>>,
    sender: UnboundedSender<SessionEvent>,
    debounce: Duration,
}

#[derive(Default)]
struct State {
    pending: Vec<QueuedTurn>,
    provider: Option<Arc<dyn ModelProvider>>,
    last_enqueued: Option<Instant>,
    in_flight: bool,
    started: bool,
    flush: bool,
}

impl ExtractionWorker {
    /// A worker for one project store. Nothing runs until a turn is queued.
    #[must_use]
    pub fn new(
        store_dir: PathBuf,
        writer_gate: Arc<tokio::sync::Mutex<()>>,
        sender: UnboundedSender<SessionEvent>,
    ) -> Self {
        Self::with_debounce(store_dir, writer_gate, sender, DEBOUNCE)
    }

    /// The same worker with another quiet period; tests use a short one.
    #[must_use]
    pub fn with_debounce(
        store_dir: PathBuf,
        writer_gate: Arc<tokio::sync::Mutex<()>>,
        sender: UnboundedSender<SessionEvent>,
        debounce: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                idle: Condvar::new(),
                wake: Notify::new(),
                store_dir,
                writer_gate,
                sender,
                debounce,
            }),
        }
    }

    /// Queue one finished turn. The newest provider is the one the batch will use.
    ///
    /// A fixture provider never extracts, so its turns are not queued at all.
    pub fn enqueue(&self, turn: QueuedTurn, provider: Arc<dyn ModelProvider>) {
        if provider.capabilities().fixture {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let start = {
            let Ok(mut state) = self.inner.state.lock() else {
                return;
            };
            state.pending.push(turn);
            state.provider = Some(provider);
            state.last_enqueued = Some(Instant::now());
            !std::mem::replace(&mut state.started, true)
        };
        if start {
            let inner = Arc::clone(&self.inner);
            handle.spawn(async move { inner.run().await });
        }
        self.inner.wake.notify_one();
    }

    /// Whether any turn is queued or being extracted.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.inner
            .state
            .lock()
            .is_ok_and(|state| !state.pending.is_empty() || state.in_flight)
    }

    /// Extract what is queued now, and wait for it, at most `limit`.
    ///
    /// Called when the app exits. It blocks the calling thread, so it returns at once
    /// on a current-thread runtime, where blocking would stop the very task it waits
    /// for. Returns whether the queue was drained.
    pub fn flush_blocking(&self, limit: Duration) -> bool {
        if !self.busy() {
            return true;
        }
        if let Ok(handle) = tokio::runtime::Handle::try_current()
            && handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread
        {
            return false;
        }
        if let Ok(mut state) = self.inner.state.lock() {
            state.flush = true;
        }
        self.inner.wake.notify_one();
        let Ok(state) = self.inner.state.lock() else {
            return false;
        };
        self.inner
            .idle
            .wait_timeout_while(state, limit, |state| {
                !state.pending.is_empty() || state.in_flight
            })
            .is_ok_and(|(state, _)| state.pending.is_empty() && !state.in_flight)
    }
}

impl Inner {
    async fn run(self: Arc<Self>) {
        loop {
            // `None` means the queue's lock is poisoned: nothing more can be queued
            // or drained, so the worker stops instead of spinning.
            let Some((batch, provider)) = self.next_batch().await else {
                return;
            };
            let message = self.extract(&batch, provider).await;
            if let Some(message) = message {
                let _ = self.sender.send(SessionEvent::Notice { message });
            }
            if let Ok(mut state) = self.state.lock() {
                state.in_flight = false;
                if state.pending.is_empty() {
                    state.flush = false;
                    self.idle.notify_all();
                }
            }
        }
    }

    /// Wait until a batch is due, then take it.
    ///
    /// A batch is due when the conversation has been quiet for the debounce, when the
    /// queue holds a full batch, or when a flush asked for it.
    async fn next_batch(&self) -> Option<(Vec<QueuedTurn>, Arc<dyn ModelProvider>)> {
        loop {
            let wait = {
                let mut state = self.state.lock().ok()?;
                if state.pending.is_empty() {
                    None
                } else {
                    let quiet = state.last_enqueued.map_or(self.debounce, |at| at.elapsed());
                    if state.flush || state.pending.len() >= MAX_BATCH || quiet >= self.debounce {
                        let take = state.pending.len().min(MAX_BATCH);
                        let batch = state.pending.drain(..take).collect::<Vec<_>>();
                        let provider = state.provider.clone()?;
                        state.in_flight = true;
                        return Some((batch, provider));
                    }
                    Some(self.debounce.saturating_sub(quiet))
                }
            };
            match wait {
                // A new turn, or a flush, wakes the wait early; the loop re-reads the
                // state rather than trusting why it woke.
                Some(remaining) => {
                    tokio::select! {
                        () = tokio::time::sleep(remaining) => {}
                        () = self.wake.notified() => {}
                    }
                }
                None => self.wake.notified().await,
            }
        }
    }

    /// Extract one batch; the result is the transcript line, if there is one.
    async fn extract(
        &self,
        batch: &[QueuedTurn],
        provider: Arc<dyn ModelProvider>,
    ) -> Option<String> {
        // Reading known facts needs no writer: a reader sees committed memory and
        // never blocks the turn that may be running now.
        let known = match SqliteStore::open_read_only(self.store_dir.clone()).await {
            Ok(store) => {
                let store = Arc::new(store);
                let known = memory::known_facts_for(Arc::clone(&store), batch).await;
                if let Ok(store) = Arc::try_unwrap(store) {
                    let _ = store.close().await;
                }
                match known {
                    Ok(known) => known,
                    Err(error) => {
                        return Some(format!("memory: facts were not extracted ({error})"));
                    }
                }
            }
            Err(error) => return Some(format!("memory: facts were not extracted ({error})")),
        };
        // The model call is the slow part, and it holds nothing.
        let facts = match memory::ask_for_facts(provider.as_ref(), &known, batch).await {
            Ok(facts) => facts,
            // A skipped extraction is not news on every turn; the reason is only worth a
            // line when the model was asked and failed.
            Err(reason) => {
                return (reason != "fixture providers do not extract")
                    .then(|| format!("memory: facts were not extracted ({reason})"));
            }
        };
        if facts.is_empty() {
            return None;
        }
        // Writing takes the gate turns take, so the store is never held by both.
        let _gate = self.writer_gate.lock().await;
        let store = match self.open_writer().await {
            Ok(store) => Arc::new(store),
            Err(error) => return Some(format!("memory: facts were not saved ({error})")),
        };
        let report = memory::apply_facts(Arc::clone(&store), batch, &known, facts).await;
        if let Ok(store) = Arc::try_unwrap(store) {
            let _ = store.close().await;
        }
        match report {
            Ok(report) => report.message(),
            Err(error) => Some(format!("memory: facts were not saved ({error})")),
        }
    }

    /// Open the writer, waiting out another process that holds it for a moment.
    async fn open_writer(&self) -> Result<SqliteStore, String> {
        let mut last = String::new();
        for attempt in 0..WRITER_ATTEMPTS {
            match SqliteStore::open_writer(WriterOpenOptions::new(
                self.store_dir.clone(),
                HostId::generate(),
            ))
            .await
            {
                Ok(store) => return Ok(store),
                Err(error) => last = error.to_string(),
            }
            tokio::time::sleep(Duration::from_millis(400 * u64::from(attempt + 1))).await;
        }
        Err(last)
    }
}

#[cfg(test)]
mod tests {
    use super::ExtractionWorker;
    use crate::interactive::events::SessionEvent;
    use crate::interactive::memory::{self, principal};
    use crate::interactive::project::resolve_project_id;
    use harness_providers::{
        CancellationToken, ModelCapabilities, ModelProvider, ProviderFuture, ProviderRequest,
        ProviderStreamEvent,
    };
    use harness_session::{AdmitInputRequest, SessionService};
    use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
    use harness_tools::observe_workspace;
    use harness_types::{HostId, InputId, SessionId, SourceAuthority, TaskId};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// Answers every call with facts for each turn it was shown, and counts calls.
    struct CountingProvider {
        calls: AtomicUsize,
        prompts: std::sync::Mutex<Vec<String>>,
    }

    impl ModelProvider for CountingProvider {
        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities {
                fixture: false,
                ..ModelCapabilities::deepseek_fixture()
            }
        }

        fn stream(&self, request: ProviderRequest, _cancel: CancellationToken) -> ProviderFuture {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let prompt = request.messages[0].content.clone();
            self.prompts.lock().expect("prompts").push(prompt);
            let reply = "{\"facts\":[\
                {\"content\":\"The project uses pnpm.\",\"category\":\"preference\",\"confidence\":0.9,\"turn\":1},\
                {\"content\":\"CI runs on Windows.\",\"category\":\"context\",\"confidence\":0.95,\"turn\":2}]}";
            let events = vec![
                ProviderStreamEvent::Started {
                    request_id: request.request_id,
                },
                ProviderStreamEvent::text(reply),
                ProviderStreamEvent::completed("stop"),
            ];
            Box::pin(async move { Ok(events) })
        }
    }

    /// Turns queued in a burst are extracted together, once the conversation is quiet,
    /// and by a single model call - after the turns have released the store.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::too_many_lines)] // two admitted turns, one batch, and what it wrote
    async fn a_burst_of_turns_is_extracted_once_in_the_background() {
        let temp = tempfile::tempdir().expect("temp");
        let workspace = temp.path().join("project");
        std::fs::create_dir(&workspace).expect("workspace");
        let store_dir = temp.path().join("data");
        let store = Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(
                store_dir.clone(),
                HostId::generate(),
            ))
            .await
            .expect("store"),
        );
        let project_id = resolve_project_id(&store, &workspace)
            .await
            .expect("project");
        let mut turns = Vec::new();
        for (question, answer) in [
            ("we build with pnpm here", "Understood."),
            ("CI runs on Windows runners", "Noted."),
        ] {
            let session = SessionId::generate();
            let task = TaskId::generate();
            SessionService::new(Arc::clone(&store))
                .admit_input(AdmitInputRequest {
                    session_id: session.clone(),
                    task_id: task.clone(),
                    input_id: InputId::generate(),
                    expected_sequence: 1,
                    authority: SourceAuthority::User,
                    raw_text: question.to_owned(),
                    workspace: observe_workspace(project_id.clone(), &workspace)
                        .expect("observation"),
                    initial_plan_items: Vec::new(),
                })
                .await
                .expect("admitted");
            let owner = principal(project_id.clone(), task, session.clone());
            turns.push(
                memory::queued_turn(&store, &owner, &session, answer)
                    .await
                    .expect("read")
                    .expect("a turn to learn from"),
            );
        }
        // The turns are over: the store is released before anything is extracted.
        Arc::try_unwrap(store)
            .expect("sole owner")
            .close()
            .await
            .expect("close");

        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let worker = ExtractionWorker::with_debounce(
            store_dir.clone(),
            Arc::clone(&gate),
            sender,
            Duration::from_millis(150),
        );
        let provider = Arc::new(CountingProvider {
            calls: AtomicUsize::new(0),
            prompts: std::sync::Mutex::new(Vec::new()),
        });
        for turn in turns {
            worker.enqueue(turn, Arc::clone(&provider) as Arc<dyn ModelProvider>);
        }
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            0,
            "queueing a turn does not call the model"
        );

        let notice = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("the batch finishes")
            .expect("a notice");
        assert!(
            matches!(&notice, SessionEvent::Notice { message } if message.contains("learned 2 fact(s)")),
            "{notice:?}"
        );
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "both turns are read by one call"
        );
        let prompt = provider.prompts.lock().expect("prompts")[0].clone();
        assert!(
            prompt.contains("Turn 1:") && prompt.contains("Turn 2:"),
            "{prompt}"
        );
        assert!(!worker.busy());

        // The facts are in the store, sourced from their own turns.
        let reader = SqliteStore::open_read_only(store_dir)
            .await
            .expect("reader");
        let service = harness_memory::MemoryService::new(Arc::new(reader));
        let owner = principal(project_id, TaskId::generate(), SessionId::generate());
        // One query per fact: each shares two terms with its own fact only, which is
        // what the overlap floor asks of a hit.
        for (query, expected) in [
            ("does the project uses pnpm", "The project uses pnpm."),
            ("ci runs on which os, windows?", "CI runs on Windows."),
        ] {
            let found = service
                .search(&owner, query, 8, None)
                .await
                .expect("search");
            assert!(
                found
                    .hits
                    .iter()
                    .any(|hit| hit.current.content.contains(expected)),
                "{query:?} finds {expected:?}: {:?}",
                found.hits
            );
        }
    }

    /// Exit does not lose the last turn: a flush runs the queue without waiting out the
    /// debounce, and reports whether it drained.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_flush_on_exit_runs_the_queue_without_the_debounce() {
        let temp = tempfile::tempdir().expect("temp");
        let workspace = temp.path().join("project");
        std::fs::create_dir(&workspace).expect("workspace");
        let store_dir = temp.path().join("data");
        let store = Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(
                store_dir.clone(),
                HostId::generate(),
            ))
            .await
            .expect("store"),
        );
        let project_id = resolve_project_id(&store, &workspace)
            .await
            .expect("project");
        let session = SessionId::generate();
        let task = TaskId::generate();
        SessionService::new(Arc::clone(&store))
            .admit_input(AdmitInputRequest {
                session_id: session.clone(),
                task_id: task.clone(),
                input_id: InputId::generate(),
                expected_sequence: 1,
                authority: SourceAuthority::User,
                raw_text: "we build with pnpm here".to_owned(),
                workspace: observe_workspace(project_id.clone(), &workspace).expect("observation"),
                initial_plan_items: Vec::new(),
            })
            .await
            .expect("admitted");
        let turn = memory::queued_turn(
            &store,
            &principal(project_id, task, session.clone()),
            &session,
            "Understood.",
        )
        .await
        .expect("read")
        .expect("a turn");
        Arc::try_unwrap(store)
            .expect("sole owner")
            .close()
            .await
            .expect("close");

        let (sender, _events) = tokio::sync::mpsc::unbounded_channel();
        // A debounce far longer than the test: only the flush can make it run.
        let worker = ExtractionWorker::with_debounce(
            store_dir,
            Arc::new(tokio::sync::Mutex::new(())),
            sender,
            Duration::from_mins(10),
        );
        let provider = Arc::new(CountingProvider {
            calls: AtomicUsize::new(0),
            prompts: std::sync::Mutex::new(Vec::new()),
        });
        worker.enqueue(turn, Arc::clone(&provider) as Arc<dyn ModelProvider>);
        assert!(worker.busy());
        let flusher = worker.clone();
        let drained =
            tokio::task::spawn_blocking(move || flusher.flush_blocking(Duration::from_secs(20)))
                .await
                .expect("flush joins");
        assert!(drained, "the queue drains on exit");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }
}
