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
//! - there is no supervisor process: `ha agents` reads the workers'
//!   descriptors, and a worker that dies takes its agents with it (their
//!   conversations are in the store, and `ha --resume` continues them);
//! - one terminal at a time: a second `attach` takes the agent over.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
use super::protocol::{self, AgentInfo, CreateAgent, Reply, Request, SendMode};
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
    token: String,
    agents: Mutex<BTreeMap<String, AgentHandle>>,
    /// When the worker last had no agent.
    empty_since: Mutex<Option<Instant>>,
    stopping: AtomicBool,
    connections: AtomicU64,
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
        return Err("another worker serves this project".to_owned());
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
        token: new_token(),
        agents: Mutex::new(BTreeMap::new()),
        empty_since: Mutex::new(Some(Instant::now())),
        stopping: AtomicBool::new(false),
        connections: AtomicU64::new(0),
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
    supervise(&worker);
    worker.stop_all();
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
            if agents.is_empty() {
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
        let id = new_id();
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
        log(&format!("agent {} started", info.id));
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
        Ok(status.info.clone())
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
        if let Some(thread) = agent.thread.take() {
            std::thread::spawn(move || {
                let _ = thread.join();
            });
        }
        log(&format!("agent {id} stopped"));
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
    client: Option<Client>,
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
            client: None,
            last_status: Instant::now(),
        };
        if let Some(prompt) = spec.prompt {
            let mut effects = Vec::new();
            agent
                .controller
                .deliver_external(prompt, true, &mut effects);
            agent.forward(effects, None);
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
                if let Some(client) = self.client.take() {
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
                self.forward(effects, None);
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
                if let Some(previous) = self.client.take() {
                    let _ = previous.out.send(Reply::Detached {
                        reason: "another terminal attached to this agent".to_owned(),
                    });
                }
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
                    self.client = Some(Client {
                        connection,
                        out,
                        last_state,
                    });
                }
                self.refresh_status();
            }
            Command::Detach { connection } => {
                if self
                    .client
                    .as_ref()
                    .is_some_and(|client| client.connection == connection)
                {
                    self.client = None;
                    self.refresh_status();
                }
            }
            Command::Key {
                connection,
                seq,
                key,
            } => {
                let effects = self.controller.handle_key(key);
                if self
                    .client
                    .as_ref()
                    .is_some_and(|client| client.connection == connection)
                {
                    self.forward(effects, Some(seq));
                } else {
                    self.forward(effects, None);
                }
            }
            Command::Columns {
                connection,
                columns,
            } => {
                if self
                    .client
                    .as_ref()
                    .is_some_and(|client| client.connection == connection)
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
                self.forward(effects, None);
                let _ = reply.send(outcome);
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
            status.info.attached = self.client.is_some();
            status.info.last_request = state.last_request;
            status.info.conversation = conversation;
        }
    }

    /// Keep what the effects add to the conversation, and send them to the
    /// attached terminal: as the answer to key `ack`, or as a frame.
    fn forward(&mut self, effects: Vec<Effect>, ack: Option<u64>) {
        let mut exit = false;
        for effect in &effects {
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
        let Some(client) = &mut self.client else {
            return;
        };
        let state = self.controller.ui_state();
        let text = serde_json::to_string(&state).unwrap_or_default();
        let state = (text != client.last_state).then(|| {
            client.last_state = text;
            Box::new(state)
        });
        if ack.is_none() && effects.is_empty() && state.is_none() {
            return;
        }
        let reply = match ack {
            Some(seq) => Reply::Ack {
                seq,
                effects,
                state,
            },
            None => Reply::Frame { effects, state },
        };
        let sent = client.out.send(reply).is_ok();
        if !sent || exit {
            log(&format!(
                "terminal {} {}",
                client.connection,
                if sent { "detached (/quit)" } else { "is gone" }
            ));
            // `/quit` closes the terminal; the agent carries on.
            self.client = None;
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
            Request::Rename { agent, name } => reply_of(
                worker
                    .rename(&agent, &name)
                    .map(|info| serde_json::to_value(info).unwrap_or_default()),
            ),
            Request::Shutdown => {
                worker.stopping.store(true, Ordering::SeqCst);
                let _ = protocol::write_line(&mut writer, &Reply::Ok { value: json!({}) });
                return;
            }
            Request::Attach { agent, columns } => {
                attach(worker, &agent, columns, reader, writer);
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
