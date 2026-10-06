//! The background worker: prime-agent's resident session worker, one per
//! project store.
//!
//! Each agent is the interactive app's own controller, run on a thread of the
//! worker instead of under a terminal. Everything that moves a conversation
//! forward on its own - automatic continuations, goals, the autonomous gates,
//! queued messages, heartbeats, schedules, delegated children - lives in that
//! controller, so it keeps going with no terminal attached. A terminal that
//! attaches (`ha`, `ha attach`) only sends keys and draws what comes back.
//!
//! Differences from prime-agent, where ha's design makes them:
//! - one worker per project store rather than per root session: the agents of
//!   a project share its store, which takes a single writer;
//! - the supervisor is per project too: `ha worker` starts the worker as its
//!   child and starts it again when it dies (prime-agent's 250 ms, 1 s, 5 s),
//!   and the worker's journal brings its agents back on their ids;
//! - several terminals may attach to one agent, as prime-agent's clients
//!   view one session: each key is answered to the terminal that typed it,
//!   what the agent draws reaches all of them, and `/quit` detaches only the
//!   terminal that typed it.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::json;

use super::super::bootstrap::{self, LaunchContext, LaunchRequest};
use super::super::config::ConfigOverrides;
use super::super::controller::{Effect, InteractiveController};
use super::super::events::{HistoryItem, Key};
use super::super::paths::{HostPlatform, LaunchEnvironment};
use super::protocol::{self, AgentInfo, CreateAgent, ExecSpec, Reply, Request, SendMode};
use super::registry::{self, Descriptor};
use super::{CreateSession, SessionHost};

/// How long an agent's loop waits for a command before it drains its events.
const POLL: Duration = Duration::from_millis(20);
/// The spinner and clocks, as the TUI ticks them.
const TICK: Duration = Duration::from_millis(100);
/// How often an agent refreshes what `ha agents` shows about it.
const STATUS_EVERY: Duration = Duration::from_secs(1);
/// How often the worker looks for idle agents and for its own descriptor.
const SWEEP_EVERY: Duration = Duration::from_secs(5);
/// How long a worker with no agent stays, for the client that started it.
const EMPTY_GRACE: Duration = Duration::from_secs(30);
/// How long a new connection has to present the token.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// How long an agent may take to start.
const CREATE_TIMEOUT: Duration = Duration::from_mins(2);
/// prime-agent's supervisor retries a worker that died after these delays
/// (`WORKER_RETRY_DELAYS_MS`).
const RESTART_DELAYS: [Duration; 3] = [
    Duration::from_millis(250),
    Duration::from_secs(1),
    Duration::from_secs(5),
];
/// A worker that served this long before it died is not a crash loop: its
/// retries start over.
const STABLE_AFTER: Duration = Duration::from_mins(1);
/// prime-agent's `idleEvictionMinutes` default.
pub const DEFAULT_IDLE_EVICTION_MINUTES: u64 = 90;
/// How many history entries an agent keeps for a terminal that attaches, as
/// many as the TUI keeps for ctrl+o.
const HISTORY_ENTRIES: usize = 4000;

/// The worker's launch arguments (`ha worker`, started by a client).
#[derive(Clone, Debug)]
pub struct WorkerArgs {
    pub registry: PathBuf,
    pub store_dir: PathBuf,
    pub project_root: PathBuf,
    /// Serve the project; without it the process is the worker's supervisor.
    pub serve: bool,
}

/// prime-agent's supervisor: run the worker as a child process and start it
/// again when it dies - after 250 ms, 1 s and 5 s - so the agents it ran come
/// back from its journal. A worker that ends on its own (no agent left,
/// `ha shutdown`) ends the supervisor too.
///
/// # Errors
/// The worker cannot be started, or keeps dying.
pub fn supervise_process(args: &WorkerArgs) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut failures = 0_usize;
    loop {
        let started = Instant::now();
        let status = std::process::Command::new(&executable)
            .arg("worker")
            .arg("--serve")
            .arg("--registry")
            .arg(&args.registry)
            .arg("--store")
            .arg(&args.store_dir)
            .arg("--root")
            .arg(&args.project_root)
            .current_dir(&args.project_root)
            .stdin(std::process::Stdio::null())
            .status()
            .map_err(|error| format!("the worker could not be started: {error}"))?;
        if status.success() {
            return Ok(());
        }
        if started.elapsed() >= STABLE_AFTER {
            failures = 0;
        }
        let Some(delay) = RESTART_DELAYS.get(failures) else {
            log("the worker keeps dying; the supervisor stops");
            return Err(format!("the worker died {failures} times in a row"));
        };
        log(&format!(
            "the worker ended ({status}); starting it again in {delay:?}"
        ));
        failures += 1;
        std::thread::sleep(*delay);
    }
}

enum Command {
    Attach {
        connection: u64,
        out: Sender<Reply>,
        columns: u16,
    },
    Detach {
        connection: u64,
    },
    Key {
        connection: u64,
        seq: u64,
        key: Key,
    },
    Columns {
        connection: u64,
        columns: u16,
    },
    Send {
        text: String,
        mode: SendMode,
        reply: Sender<&'static str>,
    },
    Abort {
        reply: Sender<bool>,
    },
    LastAnswer {
        reply: Sender<String>,
    },
    Schedule {
        argument: String,
        reply: Sender<Result<Vec<String>, String>>,
    },
    Stop,
}

struct Status {
    info: AgentInfo,
    last_activity: Instant,
}

struct AgentHandle {
    inbox: Sender<Command>,
    status: Arc<Mutex<Status>>,
    thread: Option<JoinHandle<()>>,
}

pub struct Worker {
    id: String,
    key: String,
    registry: PathBuf,
    store_dir: PathBuf,
    project_root: PathBuf,
    token: String,
    agents: Mutex<BTreeMap<String, AgentHandle>>,
    /// `Restart`: stop, but keep the journal for the next worker.
    preserve: AtomicBool,
    /// When the worker last had no agent.
    empty_since: Mutex<Option<Instant>>,
    stopping: AtomicBool,
    connections: AtomicU64,
    /// `ha exec` runs in progress: a worker running one is not empty.
    execs: AtomicUsize,
    /// The user configuration of the agents, where `idleEvictionMinutes` is.
    config_file: Mutex<Option<PathBuf>>,
    runtime: tokio::runtime::Handle,
}

/// Run the worker until it has nothing left to do.
///
/// # Errors
/// The worker cannot take its project's lock, listen, or write its descriptor.
pub fn run(args: &WorkerArgs) -> Result<(), String> {
    std::fs::create_dir_all(&args.registry).map_err(|error| error.to_string())?;
    let key = registry::project_key(&args.store_dir);
    // One worker per project: the lock is held for the worker's lifetime and
    // the OS releases it when the process ends, however it ends.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(registry::lock_path(&args.registry, &key))
        .map_err(|error| error.to_string())?;
    if lock.try_lock().is_err() {
        // Not a failure the supervisor should retry: the project is served.
        log("another worker serves this project");
        return Ok(());
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| error.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|_| "the worker needs an async runtime".to_owned())?;
    let worker = Arc::new(Worker {
        id: new_id(),
        key: key.clone(),
        registry: args.registry.clone(),
        store_dir: args.store_dir.clone(),
        project_root: args.project_root.clone(),
        token: new_token(),
        agents: Mutex::new(BTreeMap::new()),
        preserve: AtomicBool::new(false),
        empty_since: Mutex::new(Some(Instant::now())),
        stopping: AtomicBool::new(false),
        connections: AtomicU64::new(0),
        execs: AtomicUsize::new(0),
        config_file: Mutex::new(None),
        runtime,
    });
    let descriptor = Descriptor {
        schema_version: registry::SCHEMA_VERSION,
        worker_id: worker.id.clone(),
        pid: std::process::id(),
        port,
        token: worker.token.clone(),
        store_dir: args.store_dir.clone(),
        project_root: args.project_root.clone(),
        started_at_unix_ms: chrono::Utc::now().timestamp_millis(),
        build: registry::build_identity(),
    };
    registry::write(&args.registry, &key, &descriptor).map_err(|error| error.to_string())?;
    log(&format!(
        "worker {} serving {} on 127.0.0.1:{port}",
        worker.id,
        args.project_root.display()
    ));
    let accepting = Arc::clone(&worker);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if accepting.stopping.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = stream else { continue };
            let serving = Arc::clone(&accepting);
            std::thread::spawn(move || serve(&serving, stream));
        }
    });
    // The agents the last worker ran - it died, or a newer build replaced it -
    // and the conversations with a job to run come back.
    let recovering = Arc::clone(&worker);
    std::thread::spawn(move || recovering.recover());
    supervise(&worker);
    if worker.preserve.load(Ordering::SeqCst) {
        worker.keep_journal();
        worker.stop_all();
    } else {
        worker.stop_all();
        worker.keep_journal();
    }
    registry::remove_if_owned(&args.registry, &key, std::process::id());
    log("worker stopped");
    drop(lock);
    Ok(())
}

/// Evict idle agents, notice a removed descriptor, and leave once empty.
fn supervise(worker: &Arc<Worker>) {
    loop {
        let sweep = Instant::now() + SWEEP_EVERY;
        while Instant::now() < sweep {
            if worker.stopping.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let path = registry::descriptor_path(&worker.registry, &worker.key);
        if registry::read(&path).is_none_or(|descriptor| descriptor.pid != std::process::id()) {
            log("the worker descriptor is gone; stopping");
            return;
        }
        worker.evict_idle();
        worker.reap();
        worker.keep_journal();
        let empty = worker
            .empty_since
            .lock()
            .ok()
            .and_then(|since| *since)
            .is_some_and(|since| since.elapsed() >= EMPTY_GRACE);
        if empty {
            log("no agent left; stopping");
            return;
        }
    }
}

impl Worker {
    /// Write the journal of the agents running now.
    fn keep_journal(&self) {
        let journal = registry::Journal {
            schema_version: registry::SCHEMA_VERSION,
            store_dir: self.store_dir.clone(),
            project_root: self.project_root.clone(),
            agents: self
                .list()
                .into_iter()
                .map(|agent| registry::JournalAgent {
                    id: agent.id,
                    name: agent.name,
                    conversation: agent.conversation,
                })
                .collect(),
        };
        if let Err(error) = registry::write_journal(&self.registry, &self.key, &journal) {
            log(&format!("the journal could not be written: {error}"));
        }
    }

    /// Start again what the last worker left, as prime-agent's supervisor
    /// restores a dead worker's sessions: the journal's agents on their ids
    /// and conversations. They run on this worker's environment - the one the
    /// terminal that started it had - since an agent's own is never written.
    ///
    /// prime-agent's no-auto-resume contract: a conversation that was not
    /// running stays down. Its scheduled jobs stay dormant until the user opens
    /// it; opening every conversation with an active job brought back old
    /// sessions nobody had running.
    fn recover(self: &Arc<Self>) {
        let environment = LaunchEnvironment::capture().pairs();
        let spec = |conversation: String, name: Option<String>, id: Option<String>| CreateAgent {
            caller_dir: self.project_root.clone(),
            cwd: Some(self.project_root.clone()),
            environment: environment.clone(),
            resume: Some(conversation),
            name,
            id,
            ..CreateAgent::default()
        };
        let journal = registry::read_journal(&self.registry, &self.key);
        let scheduled = self
            .registry
            .parent()
            .map(super::super::schedules::tasks_with_active_jobs)
            .unwrap_or_default();
        let wanted = journal
            .as_ref()
            .map(|journal| journal.agents.clone())
            .unwrap_or_default();
        if wanted.is_empty() {
            if !scheduled.is_empty() {
                log(&format!(
                    "{} conversation(s) with scheduled jobs stay dormant until opened",
                    scheduled.len()
                ));
            }
            return;
        }
        let newest = self.newest_sessions();
        let mut open = std::collections::BTreeSet::new();
        for agent in wanted {
            let Some(session) = agent
                .conversation
                .as_ref()
                .and_then(|task| newest.get(task))
            else {
                continue;
            };
            match self.create(spec(session.clone(), agent.name, Some(agent.id.clone()))) {
                Ok(info) => {
                    log(&format!("agent {} recovered", info.id));
                    open.extend(agent.conversation);
                }
                Err(error) => log(&format!(
                    "agent {} could not be recovered: {error}",
                    agent.id
                )),
            }
        }
        let dormant = scheduled
            .iter()
            .filter(|task| !open.contains(*task))
            .count();
        if dormant > 0 {
            log(&format!(
                "{dormant} conversation(s) with scheduled jobs stay dormant until opened"
            ));
        }
        self.keep_journal();
    }

    /// The newest session of every task in this worker's store.
    fn newest_sessions(&self) -> BTreeMap<String, String> {
        let shared = super::super::store_lease::SharedStore::for_dir(self.store_dir.clone());
        self.runtime.block_on(async move {
            let Ok(lease) = shared.lease().await else {
                return BTreeMap::new();
            };
            let Ok(mut sessions) = lease.store().list_sessions().await else {
                return BTreeMap::new();
            };
            sessions.sort_by(|left, right| {
                left.created_at
                    .cmp(&right.created_at)
                    .then_with(|| left.session_id.as_str().cmp(right.session_id.as_str()))
            });
            sessions
                .into_iter()
                .map(|summary| {
                    (
                        summary.task_id.as_str().to_owned(),
                        summary.session_id.as_str().to_owned(),
                    )
                })
                .collect()
        })
    }

    fn idle_eviction(&self) -> Option<Duration> {
        let config = self.config_file.lock().ok().and_then(|path| path.clone());
        let setting = config
            .as_deref()
            .and_then(|path| super::super::config::load_setting(path, "idleEvictionMinutes"));
        idle_eviction_from(setting.as_ref())
    }

    fn evict_idle(&self) {
        let Some(after) = self.idle_eviction() else {
            return;
        };
        let idle = self
            .agents
            .lock()
            .map(|agents| {
                agents
                    .iter()
                    .filter(|(_, agent)| {
                        agent.status.lock().is_ok_and(|status| {
                            !status.info.attached
                                && !status.info.busy
                                && !status.info.scheduled
                                && status.last_activity.elapsed() >= after
                        })
                    })
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for id in idle {
            log(&format!("agent {id} was idle for {after:?}; stopping it"));
            let _ = self.stop(&id);
        }
    }

    /// Forget agents whose thread ended on its own.
    fn reap(&self) {
        let Ok(mut agents) = self.agents.lock() else {
            return;
        };
        agents.retain(|_, agent| {
            agent
                .thread
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
        });
        self.note_empty(&agents);
    }

    fn note_empty(&self, agents: &BTreeMap<String, AgentHandle>) {
        if let Ok(mut since) = self.empty_since.lock() {
            if agents.is_empty() && self.execs.load(Ordering::SeqCst) == 0 {
                since.get_or_insert_with(Instant::now);
            } else {
                *since = None;
            }
        }
    }

    fn list(&self) -> Vec<AgentInfo> {
        self.agents
            .lock()
            .map(|agents| {
                agents
                    .values()
                    .map(|agent| info_of(&agent.status))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The id of the agent `selector` names: its id, its name, or a prefix of
    /// its id that no other agent shares.
    fn resolve(&self, selector: &str) -> Result<String, String> {
        let agents = self
            .agents
            .lock()
            .map_err(|_| "the agents are unavailable".to_owned())?;
        resolve_selector(
            agents
                .values()
                .map(|agent| info_of(&agent.status))
                .collect::<Vec<_>>()
                .as_slice(),
            selector,
        )
    }

    fn inbox(&self, id: &str) -> Result<Sender<Command>, String> {
        self.agents
            .lock()
            .map_err(|_| "the agents are unavailable".to_owned())?
            .get(id)
            .map(|agent| agent.inbox.clone())
            .ok_or_else(|| format!("no agent {id}"))
    }

    /// Start an agent from a client's launch.
    ///
    /// # Errors
    /// The name is taken, the launch is for another project, or the app could
    /// not start.
    fn create(self: &Arc<Self>, spec: CreateAgent) -> Result<AgentInfo, String> {
        if let Some(name) = &spec.name {
            let taken = self
                .list()
                .iter()
                .any(|agent| agent.name.as_deref() == Some(name.as_str()) || agent.id == *name);
            if taken {
                return Err(format!("an agent is already named {name:?}"));
            }
        }
        // A recovered agent keeps its id, unless another agent has it now.
        let id = spec
            .id
            .clone()
            .filter(|id| self.list().iter().all(|agent| agent.id != *id))
            .unwrap_or_else(new_id);
        let (inbox, commands) = mpsc::channel();
        let (ready, started) = mpsc::channel::<Result<Arc<Mutex<Status>>, String>>();
        let worker = Arc::downgrade(self);
        let runtime = self.runtime.clone();
        let agent_id = id.clone();
        let thread = std::thread::Builder::new()
            .name(format!("agent-{id}"))
            .spawn(move || {
                let _entered = runtime.enter();
                match Agent::start(agent_id, spec, &worker) {
                    Ok(mut agent) => {
                        let _ = ready.send(Ok(Arc::clone(&agent.status)));
                        agent.run(&commands);
                        agent.controller.shut_down();
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        let status = started
            .recv_timeout(CREATE_TIMEOUT)
            .map_err(|_| "the agent did not start in time".to_owned())??;
        let info = info_of(&status);
        let mut agents = self
            .agents
            .lock()
            .map_err(|_| "the agents are unavailable".to_owned())?;
        agents.insert(
            id,
            AgentHandle {
                inbox,
                status,
                thread: Some(thread),
            },
        );
        self.note_empty(&agents);
        drop(agents);
        log(&format!("agent {} started", info.id));
        self.keep_journal();
        Ok(info)
    }

    fn send(&self, selector: &str, text: String, mode: SendMode) -> Result<&'static str, String> {
        let id = self.resolve(selector)?;
        let (reply, answer) = mpsc::channel();
        self.inbox(&id)?
            .send(Command::Send { text, mode, reply })
            .map_err(|_| format!("agent {id} has stopped"))?;
        answer
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| format!("agent {id} did not answer"))
    }

    /// End the agent's running turn; `false` when it was not running one.
    fn abort(&self, selector: &str) -> Result<bool, String> {
        let id = self.resolve(selector)?;
        let (reply, answer) = mpsc::channel();
        self.inbox(&id)?
            .send(Command::Abort { reply })
            .map_err(|_| format!("agent {id} has stopped"))?;
        answer
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| format!("agent {id} did not answer"))
    }

    /// The agent's last answer.
    fn last_answer(&self, selector: &str) -> Result<String, String> {
        let id = self.resolve(selector)?;
        let (reply, answer) = mpsc::channel();
        self.inbox(&id)?
            .send(Command::LastAnswer { reply })
            .map_err(|_| format!("agent {id} has stopped"))?;
        answer
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| format!("agent {id} did not answer"))
    }

    /// Run `/schedule <argument>` in the agent's conversation.
    fn schedule(&self, selector: &str, argument: &str) -> Result<Vec<String>, String> {
        let id = self.resolve(selector)?;
        let (reply, answer) = mpsc::channel();
        self.inbox(&id)?
            .send(Command::Schedule {
                argument: argument.to_owned(),
                reply,
            })
            .map_err(|_| format!("agent {id} has stopped"))?;
        answer
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| format!("agent {id} did not answer"))?
    }

    fn rename(&self, selector: &str, name: &str) -> Result<AgentInfo, String> {
        let name = name.trim();
        if name.is_empty() || name.chars().any(char::is_whitespace) {
            return Err("an agent name is one word".to_owned());
        }
        let id = self.resolve(selector)?;
        if self.list().iter().any(|agent| {
            agent.id != id && (agent.name.as_deref() == Some(name) || agent.id == name)
        }) {
            return Err(format!("an agent is already named {name:?}"));
        }
        let agents = self
            .agents
            .lock()
            .map_err(|_| "the agents are unavailable".to_owned())?;
        let agent = agents.get(&id).ok_or_else(|| format!("no agent {id}"))?;
        let mut status = agent
            .status
            .lock()
            .map_err(|_| "the agent is unavailable".to_owned())?;
        status.info.name = Some(name.to_owned());
        let info = status.info.clone();
        drop(status);
        drop(agents);
        self.keep_journal();
        Ok(info)
    }

    fn stop(&self, selector: &str) -> Result<AgentInfo, String> {
        let id = self.resolve(selector)?;
        let mut agents = self
            .agents
            .lock()
            .map_err(|_| "the agents are unavailable".to_owned())?;
        let mut agent = agents.remove(&id).ok_or_else(|| format!("no agent {id}"))?;
        self.note_empty(&agents);
        drop(agents);
        let info = info_of(&agent.status);
        let _ = agent.inbox.send(Command::Stop);
        let thread = agent.thread.take();
        // prime-agent's stop cleanup: the conversation's scheduled jobs are
        // cancelled once the agent has let go of them, so the next worker does
        // not reopen what the user stopped.
        let data_dir = self.registry.parent().map(std::path::Path::to_path_buf);
        let conversation = info.conversation.clone();
        std::thread::spawn(move || {
            if let Some(thread) = thread {
                let _ = thread.join();
            }
            if let (Some(data_dir), Some(task)) = (data_dir, conversation) {
                let cancelled = super::super::schedules::cancel_conversation(
                    &data_dir,
                    &task,
                    chrono::Utc::now(),
                );
                if cancelled > 0 {
                    log(&format!(
                        "{cancelled} scheduled job(s) of the stopped agent were cancelled"
                    ));
                }
                if super::super::heartbeat::cancel_conversation(&data_dir, &task) {
                    log("the stopped agent's heartbeats were cancelled");
                }
            }
        });
        log(&format!("agent {id} stopped"));
        self.keep_journal();
        Ok(info)
    }

    fn stop_all(&self) {
        let agents = self
            .agents
            .lock()
            .map(|mut agents| std::mem::take(&mut *agents))
            .unwrap_or_default();
        let threads = agents
            .into_values()
            .filter_map(|mut agent| {
                let _ = agent.inbox.send(Command::Stop);
                agent.thread.take()
            })
            .collect::<Vec<_>>();
        let deadline = Instant::now() + Duration::from_secs(10);
        for thread in threads {
            while !thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn info_of(status: &Arc<Mutex<Status>>) -> AgentInfo {
    status.lock().map_or_else(
        |poisoned| poisoned.into_inner().info.clone(),
        |status| {
            let mut info = status.info.clone();
            info.idle_seconds = status.last_activity.elapsed().as_secs();
            info
        },
    )
}

/// prime-agent's `idleEvictionMinutes`: a number of minutes, or `"off"`.
fn idle_eviction_from(setting: Option<&serde_json::Value>) -> Option<Duration> {
    match setting {
        None => Some(Duration::from_secs(DEFAULT_IDLE_EVICTION_MINUTES * 60)),
        Some(serde_json::Value::Number(minutes)) => minutes
            .as_f64()
            .filter(|minutes| minutes.is_finite() && *minutes > 0.0)
            .map(|minutes| Duration::from_secs_f64(minutes * 60.0)),
        Some(_) => None,
    }
}

/// The id of the agent `selector` names among `agents`.
pub fn resolve_selector(agents: &[AgentInfo], selector: &str) -> Result<String, String> {
    if let Some(agent) = agents
        .iter()
        .find(|agent| agent.id == selector || agent.name.as_deref() == Some(selector))
    {
        return Ok(agent.id.clone());
    }
    let matches = agents
        .iter()
        .filter(|agent| !selector.is_empty() && agent.id.starts_with(selector))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [agent] => Ok(agent.id.clone()),
        [] => Err(format!("no agent {selector:?}; `ha agents` lists them")),
        _ => Err(format!("{selector:?} names more than one agent")),
    }
}

/// One agent: the app's controller, driven without a terminal.
struct Agent {
    controller: InteractiveController,
    status: Arc<Mutex<Status>>,
    banner: Vec<String>,
    history: VecDeque<HistoryItem>,
    /// The terminals attached to it.
    clients: Vec<Client>,
    last_status: Instant,
}

struct Client {
    connection: u64,
    out: Sender<Reply>,
    /// The view state it was last sent, so an unchanged one is not sent again.
    last_state: String,
}

impl Agent {
    fn start(id: String, spec: CreateAgent, worker: &Weak<Worker>) -> Result<Self, String> {
        let owner = worker.upgrade().ok_or("the worker is stopping")?;
        let environment = LaunchEnvironment::from_pairs(spec.environment.clone());
        let context = resolve_context(&spec, &environment)?;
        if context.project_store_dir() != owner.store_dir {
            return Err("this worker serves another project".to_owned());
        }
        if let Ok(mut config) = owner.config_file.lock() {
            config.get_or_insert_with(|| context.paths.config_file.clone());
        }
        let overrides = ConfigOverrides {
            model: spec.model.clone(),
            profile: spec.profile.clone(),
            approval: spec.approval.clone(),
            ..ConfigOverrides::default()
        };
        let mut controller = super::super::app::controller_for_with_overrides(
            &context,
            &environment,
            spec.fixture,
            false,
            overrides,
        );
        controller.set_detachable();
        controller.set_session_host(Arc::new(WorkerSessionHost {
            worker: Weak::clone(worker),
            base: CreateAgent {
                prompt: None,
                name: None,
                resume: None,
                thinking: None,
                id: None,
                cwd: Some(context.project.root.clone()),
                caller_dir: context.project.root.clone(),
                ..spec.clone()
            },
        }));
        if let Some(level) = &spec.thinking {
            controller.set_thinking(level)?;
        }
        if let Some(source) = &spec.resume {
            controller.resume_source(source)?;
        }
        controller.set_columns(100);
        let banner = controller.boot_lines();
        let mut history = VecDeque::from([HistoryItem::Banner {
            lines: banner.clone(),
        }]);
        if let Some(source) = &spec.resume {
            history.push_back(HistoryItem::Message {
                text: format!(
                    "Selected session {source}; the next request verifies and recovers its context."
                ),
            });
        }
        let status = Arc::new(Mutex::new(Status {
            info: AgentInfo {
                id,
                name: spec.name.clone(),
                status: controller.ui_state().phase.label().to_owned(),
                busy: false,
                scheduled: false,
                attached: false,
                project_root: context.project.root.clone(),
                model: controller.model_label(),
                conversation: controller.conversation_id(),
                last_request: None,
                idle_seconds: 0,
                worker_pid: std::process::id(),
            },
            last_activity: Instant::now(),
        }));
        let mut agent = Self {
            controller,
            status,
            banner,
            history,
            clients: Vec::new(),
            last_status: Instant::now(),
        };
        if let Some(prompt) = spec.prompt {
            let mut effects = Vec::new();
            agent
                .controller
                .deliver_external(prompt, true, &mut effects);
            agent.forward(&effects, None);
        }
        agent.refresh_status();
        Ok(agent)
    }

    fn run(&mut self, commands: &Receiver<Command>) {
        let mut last_tick = Instant::now();
        loop {
            let mut stop = match commands.recv_timeout(POLL) {
                Ok(command) => self.handle(command),
                Err(RecvTimeoutError::Timeout) => false,
                Err(RecvTimeoutError::Disconnected) => true,
            };
            while !stop && let Ok(command) = commands.try_recv() {
                stop = self.handle(command);
            }
            if stop {
                for client in self.clients.drain(..) {
                    let _ = client.out.send(Reply::Detached {
                        reason: "the agent was stopped".to_owned(),
                    });
                }
                return;
            }
            let mut effects = self.controller.pump_events();
            if last_tick.elapsed() >= TICK {
                last_tick = Instant::now();
                effects.extend(self.controller.tick());
            }
            if !effects.is_empty() {
                self.touch();
                self.forward(&effects, None);
            }
            if self.last_status.elapsed() >= STATUS_EVERY {
                self.refresh_status();
            }
        }
    }

    /// Carry out one command; true when the agent must stop.
    fn handle(&mut self, command: Command) -> bool {
        self.touch();
        match command {
            Command::Attach {
                connection,
                out,
                columns,
            } => {
                self.controller.set_columns(columns);
                let state = self.controller.ui_state();
                let last_state = serde_json::to_string(&state).unwrap_or_default();
                self.refresh_status();
                let mut agent = Box::new(info_of(&self.status));
                agent.attached = true;
                let attached = Reply::Attached {
                    agent,
                    banner: self.banner.clone(),
                    history: self.history.iter().cloned().collect(),
                    state: Box::new(state),
                };
                if out.send(attached).is_ok() {
                    self.clients.push(Client {
                        connection,
                        out,
                        last_state,
                    });
                }
                self.refresh_status();
            }
            Command::Detach { connection } => {
                let before = self.clients.len();
                self.clients
                    .retain(|client| client.connection != connection);
                if self.clients.len() != before {
                    self.refresh_status();
                }
            }
            Command::Key {
                connection,
                seq,
                key,
            } => {
                let effects = self.controller.handle_key(key);
                self.forward(&effects, Some((connection, seq)));
            }
            Command::Columns {
                connection,
                columns,
            } => {
                // The terminal that last changed its size decides the wrapping.
                if self
                    .clients
                    .iter()
                    .any(|client| client.connection == connection)
                {
                    self.controller.set_columns(columns);
                }
            }
            Command::Send { text, mode, reply } => {
                let mut effects = Vec::new();
                let outcome = self.controller.deliver_external(
                    text,
                    mode != SendMode::FollowUp,
                    &mut effects,
                );
                self.forward(&effects, None);
                let _ = reply.send(outcome);
            }
            Command::Abort { reply } => {
                let (aborted, effects) = self.controller.abort_run();
                self.forward(&effects, None);
                let _ = reply.send(aborted);
            }
            Command::LastAnswer { reply } => {
                let _ = reply.send(self.controller.last_answer().to_owned());
            }
            Command::Schedule { argument, reply } => {
                let _ = reply.send(self.controller.schedule(&argument));
            }
            Command::Stop => return true,
        }
        false
    }

    fn touch(&self) {
        if let Ok(mut status) = self.status.lock() {
            status.last_activity = Instant::now();
        }
    }

    fn refresh_status(&mut self) {
        self.last_status = Instant::now();
        let state = self.controller.ui_state();
        let busy = self.controller.is_busy();
        let scheduled = self.controller.has_scheduled_work();
        let conversation = self.controller.conversation_id();
        if let Ok(mut status) = self.status.lock() {
            state.phase.label().clone_into(&mut status.info.status);
            status.info.busy = busy;
            status.info.scheduled = scheduled;
            status.info.attached = !self.clients.is_empty();
            status.info.last_request = state.last_request;
            status.info.conversation = conversation;
        }
    }

    /// Keep what the effects add to the conversation, and send them to the
    /// attached terminals: to the one that typed key `ack` as its answer, and
    /// to the others as a frame. `/quit` detaches only the terminal that typed
    /// it; the agent carries on.
    fn forward(&mut self, effects: &[Effect], ack: Option<(u64, u64)>) {
        let mut exit = false;
        for effect in effects {
            let item = match effect {
                Effect::History(item) => Some(item.clone()),
                Effect::Stream(text) => Some(HistoryItem::Assistant { text: text.clone() }),
                Effect::Thinking(text) => Some(HistoryItem::Thinking { text: text.clone() }),
                Effect::Exit(_) => {
                    exit = true;
                    None
                }
                _ => None,
            };
            if let Some(item) = item {
                if self.history.len() == HISTORY_ENTRIES {
                    self.history.pop_front();
                }
                self.history.push_back(item);
            }
        }
        if self.clients.is_empty() {
            return;
        }
        let state = self.controller.ui_state();
        let text = serde_json::to_string(&state).unwrap_or_default();
        // What the other terminals draw: the effects without the exit.
        let shared = effects
            .iter()
            .filter(|effect| !matches!(effect, Effect::Exit(_)))
            .cloned()
            .collect::<Vec<_>>();
        let mut gone = Vec::new();
        for client in &mut self.clients {
            let changed = (text != client.last_state).then(|| {
                client.last_state.clone_from(&text);
                Box::new(state.clone())
            });
            let typed =
                ack.and_then(|(connection, seq)| (connection == client.connection).then_some(seq));
            let reply = match typed {
                Some(seq) => Reply::Ack {
                    seq,
                    effects: effects.to_vec(),
                    state: changed,
                },
                // Exits the agent itself produces reach every terminal.
                None if ack.is_none() => {
                    if effects.is_empty() && changed.is_none() {
                        continue;
                    }
                    Reply::Frame {
                        effects: effects.to_vec(),
                        state: changed,
                    }
                }
                None => {
                    if shared.is_empty() && changed.is_none() {
                        continue;
                    }
                    Reply::Frame {
                        effects: shared.clone(),
                        state: changed,
                    }
                }
            };
            let sent = client.out.send(reply).is_ok();
            let leaves = exit && (typed.is_some() || ack.is_none());
            if !sent || leaves {
                log(&format!(
                    "terminal {} {}",
                    client.connection,
                    if sent { "detached (/quit)" } else { "is gone" }
                ));
                gone.push(client.connection);
            }
        }
        if !gone.is_empty() {
            self.clients
                .retain(|client| !gone.contains(&client.connection));
            self.refresh_status();
        }
    }
}

fn resolve_context(
    spec: &CreateAgent,
    environment: &LaunchEnvironment,
) -> Result<LaunchContext, String> {
    bootstrap::resolve(LaunchRequest {
        cwd: spec.cwd.clone(),
        caller_dir: spec.caller_dir.clone(),
        platform: HostPlatform::current(),
        environment: environment.clone(),
        explicit_data_dir: None,
    })
    .map_err(|error| error.to_string())
}

/// One connection: the token, then requests, or a terminal once it attaches.
fn serve(worker: &Arc<Worker>, stream: TcpStream) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(HELLO_TIMEOUT));
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;
    let authorized = matches!(
        protocol::read_line::<Request>(&mut reader),
        Ok(Some(Request::Hello { token })) if same_token(&token, &worker.token)
    );
    if !authorized {
        let _ = protocol::write_line(
            &mut writer,
            &Reply::Error {
                message: "the worker token was not presented".to_owned(),
            },
        );
        return;
    }
    // The reader holds its own handle of the socket, and on Windows a
    // duplicated handle keeps its own timeout: clear it where it is read.
    let _ = reader.get_ref().set_read_timeout(None);
    if protocol::write_line(
        &mut writer,
        &Reply::Ok {
            value: json!({ "worker_id": worker.id, "pid": std::process::id() }),
        },
    )
    .is_err()
    {
        return;
    }
    loop {
        let Ok(Some(request)) = protocol::read_line::<Request>(&mut reader) else {
            return;
        };
        let reply = match request {
            Request::Create(spec) => reply_of(
                worker
                    .create(*spec)
                    .map(|info| serde_json::to_value(info).unwrap_or_default()),
            ),
            Request::List => Reply::Ok {
                value: serde_json::to_value(worker.list()).unwrap_or_default(),
            },
            Request::Send {
                agent,
                text,
                from,
                mode,
            } => {
                let text = match from {
                    Some(from) => format!("[message from {from}]\n\n{text}"),
                    None => text,
                };
                reply_of(
                    worker
                        .send(&agent, text, mode)
                        .map(|outcome| json!({ "status": outcome })),
                )
            }
            Request::Stop { agent } => reply_of(
                worker
                    .stop(&agent)
                    .map(|info| serde_json::to_value(info).unwrap_or_default()),
            ),
            Request::Abort { agent } => reply_of(
                worker
                    .abort(&agent)
                    .map(|aborted| json!({ "aborted": aborted })),
            ),
            Request::LastAnswer { agent } => reply_of(
                worker
                    .last_answer(&agent)
                    .map(|text| json!({ "text": text })),
            ),
            Request::Schedule { agent, argument } => reply_of(
                worker
                    .schedule(&agent, &argument)
                    .map(|lines| json!({ "lines": lines })),
            ),
            Request::Rename { agent, name } => reply_of(
                worker
                    .rename(&agent, &name)
                    .map(|info| serde_json::to_value(info).unwrap_or_default()),
            ),
            Request::Shutdown | Request::Restart => {
                if matches!(request, Request::Restart) {
                    log("a newer build replaces this worker; its agents are kept");
                    worker.preserve.store(true, Ordering::SeqCst);
                }
                worker.stopping.store(true, Ordering::SeqCst);
                let _ = protocol::write_line(&mut writer, &Reply::Ok { value: json!({}) });
                return;
            }
            Request::Attach { agent, columns } => {
                attach(worker, &agent, columns, reader, writer);
                return;
            }
            Request::Exec(spec) => {
                exec(worker, *spec, reader, writer);
                return;
            }
            Request::Hello { .. }
            | Request::Key { .. }
            | Request::Columns { .. }
            | Request::Detach => Reply::Error {
                message: "this request needs an attached terminal".to_owned(),
            },
        };
        if protocol::write_line(&mut writer, &reply).is_err() {
            return;
        }
    }
}

fn reply_of(result: Result<serde_json::Value, String>) -> Reply {
    match result {
        Ok(value) => Reply::Ok { value },
        Err(message) => Reply::Error { message },
    }
}

/// The connection becomes the agent's terminal until it detaches.
fn attach(
    worker: &Arc<Worker>,
    selector: &str,
    columns: u16,
    mut reader: BufReader<TcpStream>,
    mut writer: TcpStream,
) {
    let inbox = match worker.resolve(selector).and_then(|id| worker.inbox(&id)) {
        Ok(inbox) => inbox,
        Err(message) => {
            let _ = protocol::write_line(&mut writer, &Reply::Error { message });
            return;
        }
    };
    let connection = worker.connections.fetch_add(1, Ordering::SeqCst);
    let (out, replies) = mpsc::channel::<Reply>();
    let Ok(closing) = writer.try_clone() else {
        return;
    };
    std::thread::spawn(move || {
        for reply in replies {
            if let Err(error) = protocol::write_line(&mut writer, &reply) {
                log(&format!(
                    "terminal {connection} could not be written to: {error}"
                ));
                break;
            }
        }
        // The agent let this terminal go: closing the socket is what tells it.
        let _ = writer.flush();
        let _ = writer.shutdown(Shutdown::Both);
    });
    if inbox
        .send(Command::Attach {
            connection,
            out,
            columns,
        })
        .is_err()
    {
        let _ = closing.shutdown(Shutdown::Both);
        return;
    }
    loop {
        let command = match protocol::read_line::<Request>(&mut reader) {
            Ok(Some(Request::Key { seq, key })) => Command::Key {
                connection,
                seq,
                key,
            },
            Ok(Some(Request::Columns { columns })) => Command::Columns {
                connection,
                columns,
            },
            other => {
                if let Err(error) = other {
                    log(&format!(
                        "terminal {connection} sent an unreadable request: {error}"
                    ));
                }
                let _ = inbox.send(Command::Detach { connection });
                return;
            }
        };
        if inbox.send(command).is_err() {
            let _ = closing.shutdown(Shutdown::Both);
            return;
        }
    }
}

/// An `ha exec` run's lines, sent to the terminal that asked for it.
struct ExecOutput(Sender<Reply>);

impl super::super::headless::Output for ExecOutput {
    fn stdout(&self, line: &str) {
        let _ = self.0.send(Reply::Output {
            stderr: false,
            text: line.to_owned(),
        });
    }

    fn stderr(&self, line: &str) {
        let _ = self.0.send(Reply::Output {
            stderr: true,
            text: line.to_owned(),
        });
    }
}

/// Run one `ha exec` turn on the store the agents share, and send its lines
/// back as they come. The terminal going away cancels the turn.
fn exec(
    worker: &Arc<Worker>,
    spec: ExecSpec,
    mut reader: BufReader<TcpStream>,
    mut writer: TcpStream,
) {
    let environment = LaunchEnvironment::from_pairs(spec.environment);
    let context = match bootstrap::resolve(LaunchRequest {
        cwd: spec.request.cwd.clone(),
        caller_dir: spec.caller_dir,
        platform: HostPlatform::current(),
        environment: environment.clone(),
        explicit_data_dir: None,
    }) {
        Ok(context) if context.project_store_dir() == worker.store_dir => context,
        Ok(_) => {
            let _ = protocol::write_line(
                &mut writer,
                &Reply::Error {
                    message: "this worker serves another project".to_owned(),
                },
            );
            return;
        }
        Err(error) => {
            let _ = protocol::write_line(
                &mut writer,
                &Reply::Failed {
                    code: error.code(),
                    message: error.message().to_owned(),
                },
            );
            return;
        }
    };
    worker.execs.fetch_add(1, Ordering::SeqCst);
    if let Ok(agents) = worker.agents.lock() {
        worker.note_empty(&agents);
    }
    log("exec run started");
    let cancellation = harness_providers::CancellationToken::new();
    let canceled = cancellation.clone();
    std::thread::spawn(move || {
        // Nothing more is read: the end of the stream is the terminal leaving.
        let mut rest = Vec::new();
        let _ = std::io::Read::read_to_end(&mut reader, &mut rest);
        canceled.cancel();
    });
    let (sender, lines) = mpsc::channel::<Reply>();
    let done = sender.clone();
    let shared = super::super::store_lease::SharedStore::for_dir(worker.store_dir.clone());
    worker.runtime.spawn(async move {
        let result = super::super::headless::run_with(
            spec.request,
            &context,
            &environment,
            super::super::headless::RunStore::Shared(shared),
            Arc::new(ExecOutput(sender)),
            cancellation,
        )
        .await;
        let _ = done.send(match result {
            Ok(code) => Reply::Exited { code },
            Err(error) => Reply::Failed {
                code: error.code(),
                message: error.message().to_owned(),
            },
        });
    });
    for reply in lines {
        let last = matches!(reply, Reply::Exited { .. } | Reply::Failed { .. });
        if protocol::write_line(&mut writer, &reply).is_err() || last {
            break;
        }
    }
    let _ = writer.shutdown(Shutdown::Both);
    worker.execs.fetch_sub(1, Ordering::SeqCst);
    if let Ok(agents) = worker.agents.lock() {
        worker.note_empty(&agents);
    }
    log("exec run ended");
}

/// `rlm.create_session` from an agent of this worker: a new agent here, or in
/// the worker of the project its `cwd` belongs to.
struct WorkerSessionHost {
    worker: Weak<Worker>,
    /// The creating agent's launch: its environment and options.
    base: CreateAgent,
}

impl SessionHost for WorkerSessionHost {
    fn create_session(&self, request: CreateSession) -> Result<serde_json::Value, String> {
        let worker = self.worker.upgrade().ok_or("the worker is stopping")?;
        let spec = CreateAgent {
            cwd: request.cwd.or_else(|| self.base.cwd.clone()),
            prompt: Some(request.prompt),
            name: request.name,
            model: request.model.or_else(|| self.base.model.clone()),
            thinking: request.thinking,
            ..self.base.clone()
        };
        let environment = LaunchEnvironment::from_pairs(spec.environment.clone());
        let context = resolve_context(&spec, &environment)?;
        let store_dir = context.project_store_dir();
        let info = if store_dir == worker.store_dir {
            worker.create(spec)?
        } else {
            super::client::create_in(&context, spec)?
        };
        Ok(json!({
            "active_session_id": info.id,
            "session_id": info.conversation.clone().unwrap_or_else(|| info.id.clone()),
            "name": info.display_name(),
            "session_file": store_dir.display().to_string(),
            "model": info.model,
        }))
    }
}

/// Compare two tokens without stopping at the first difference.
fn same_token(presented: &str, expected: &str) -> bool {
    presented.len() == expected.len()
        && presented
            .bytes()
            .zip(expected.bytes())
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
}

/// A short id for an agent or a worker: the random tail of a v7 id.
fn new_id() -> String {
    let id = harness_types::EventId::generate();
    let hex = id.as_str().trim_start_matches("event_").replace('-', "");
    hex[hex.len().saturating_sub(8)..].to_owned()
}

fn new_token() -> String {
    format!(
        "{}{}",
        harness_types::EventId::generate()
            .as_str()
            .trim_start_matches("event_")
            .replace('-', ""),
        harness_types::EventId::generate()
            .as_str()
            .trim_start_matches("event_")
            .replace('-', "")
    )
}

/// A line in the worker's log (its stderr, which the client points at a file
/// beside its descriptor). Lifecycle only: never a request, never a token.
fn log(message: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(
        stderr,
        "{} {message}",
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ")
    );
}

#[cfg(test)]
mod tests {
    use super::{AgentInfo, idle_eviction_from, resolve_selector, same_token};
    use serde_json::json;
    use std::time::Duration;

    fn agent(id: &str, name: Option<&str>) -> AgentInfo {
        AgentInfo {
            id: id.to_owned(),
            name: name.map(str::to_owned),
            status: "ready".to_owned(),
            busy: false,
            scheduled: false,
            attached: false,
            project_root: ".".into(),
            model: "m".to_owned(),
            conversation: None,
            last_request: None,
            idle_seconds: 0,
            worker_pid: 1,
        }
    }

    #[test]
    fn an_agent_is_named_by_id_name_or_unique_prefix() {
        let agents = [agent("ab12cd34", Some("api")), agent("ab99ee00", None)];
        assert_eq!(resolve_selector(&agents, "api").as_deref(), Ok("ab12cd34"));
        assert_eq!(resolve_selector(&agents, "ab9").as_deref(), Ok("ab99ee00"));
        assert!(
            resolve_selector(&agents, "ab").is_err(),
            "two agents share it"
        );
        assert!(resolve_selector(&agents, "zz").is_err());
    }

    #[test]
    fn idle_eviction_is_prime_agents_setting() {
        assert_eq!(idle_eviction_from(None), Some(Duration::from_mins(90)));
        assert_eq!(
            idle_eviction_from(Some(&json!(2))),
            Some(Duration::from_mins(2))
        );
        assert_eq!(idle_eviction_from(Some(&json!("off"))), None);
        assert_eq!(idle_eviction_from(Some(&json!(0))), None);
    }

    #[test]
    fn a_token_must_match_exactly() {
        assert!(same_token("abc", "abc"));
        assert!(!same_token("abd", "abc"));
        assert!(!same_token("ab", "abc"));
    }
}
