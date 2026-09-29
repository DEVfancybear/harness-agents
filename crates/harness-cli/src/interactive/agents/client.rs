//! The client side of the background workers: find or start a project's
//! worker, ask it things, and be an agent's terminal.

use std::io::BufReader;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use super::super::bootstrap::LaunchContext;
use super::super::controller::Effect;
use super::super::events::{HistoryItem, Key, UiState};
use super::super::frontend::Frontend;
use super::protocol::{self, AgentInfo, CreateAgent, Reply, Request};
use super::registry::{self, Descriptor};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a question to the worker may take; starting an agent is the slow one.
const CALL_TIMEOUT: Duration = Duration::from_secs(150);
/// How long a new worker has to write its descriptor.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a key waits for what it produced before the frame is drawn.
const KEY_TIMEOUT: Duration = Duration::from_secs(5);

/// A connection that presented the worker's token.
pub struct Connection {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Connection {
    /// Connect to the worker `descriptor` names.
    ///
    /// # Errors
    /// The worker does not answer, or refuses the token.
    pub fn open(descriptor: &Descriptor) -> Result<Self, String> {
        let address = SocketAddr::from(([127, 0, 0, 1], descriptor.port));
        let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
            .map_err(|error| format!("the worker at {address} does not answer: {error}"))?;
        let _ = stream.set_nodelay(true);
        stream
            .set_read_timeout(Some(CALL_TIMEOUT))
            .map_err(|error| error.to_string())?;
        let reader = BufReader::new(stream.try_clone().map_err(|error| error.to_string())?);
        let mut connection = Self {
            reader,
            writer: stream,
        };
        connection.call(&Request::Hello {
            token: descriptor.token.clone(),
        })?;
        Ok(connection)
    }

    /// Ask one question.
    ///
    /// # Errors
    /// The worker refused it or went away.
    pub fn call(&mut self, request: &Request) -> Result<serde_json::Value, String> {
        protocol::write_line(&mut self.writer, request)
            .map_err(|error| format!("the worker went away: {error}"))?;
        match protocol::read_line::<Reply>(&mut self.reader) {
            Ok(Some(Reply::Ok { value })) => Ok(value),
            Ok(Some(Reply::Error { message })) => Err(message),
            Ok(Some(_)) => Err("the worker answered out of turn".to_owned()),
            Ok(None) => Err("the worker closed the connection".to_owned()),
            Err(error) => Err(format!("the worker's answer could not be read: {error}")),
        }
    }
}

/// The running worker of a project, or `None` (a descriptor its process left
/// behind is removed).
#[must_use]
pub fn connect(directory: &Path, key: &str) -> Option<(Descriptor, Connection)> {
    let descriptor = registry::read(&registry::descriptor_path(directory, key))?;
    if let Ok(connection) = Connection::open(&descriptor) {
        return Some((descriptor, connection));
    }
    if !harness_cli::daemon::process_is_alive(descriptor.pid) {
        registry::remove_if_owned(directory, key, descriptor.pid);
    }
    None
}

/// The project's worker, started when there is none.
///
/// # Errors
/// No worker could be started or reached.
pub fn ensure(
    directory: &Path,
    store_dir: &Path,
    project_root: &Path,
) -> Result<(Descriptor, Connection), String> {
    let key = registry::project_key(store_dir);
    if let Some(found) = connect(directory, &key) {
        if found.0.build != registry::build_identity() {
            return Err(
                "the project's background worker runs another build of ha; `ha shutdown` replaces it"
                    .to_owned(),
            );
        }
        return Ok(found);
    }
    spawn_worker(directory, &key, store_dir, project_root)?;
    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(found) = connect(directory, &key) {
            return Ok(found);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!(
        "the background worker did not start; its log is {}",
        registry::log_path(directory, &key).display()
    ))
}

/// Start `ha worker` for a project, apart from this terminal: closing the
/// terminal must not end it.
fn spawn_worker(
    directory: &Path,
    key: &str,
    store_dir: &Path,
    project_root: &Path,
) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    // Appended to: two terminals may start a worker at once, and the one that
    // loses the project's lock must not wipe the winner's log.
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(registry::log_path(directory, key))
        .map_err(|error| error.to_string())?;
    let errors = log.try_clone().map_err(|error| error.to_string())?;
    let mut command = std::process::Command::new(executable);
    command
        .arg("worker")
        .arg("--registry")
        .arg(directory)
        .arg("--store")
        .arg(store_dir)
        .arg("--root")
        .arg(project_root)
        .current_dir(project_root)
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(errors);
    detach(&mut command)
}

#[cfg(windows)]
fn detach(command: &mut std::process::Command) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    // A hidden console of its own rather than none: the worker's children -
    // shells, the Python kernel - inherit it, so none of them opens a window.
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let flags = CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
    // Out of the terminal's job when the job allows it, so closing the
    // terminal does not end the worker with it.
    command.creation_flags(flags | CREATE_BREAKAWAY_FROM_JOB);
    if command.spawn().is_ok() {
        return Ok(());
    }
    command.creation_flags(flags);
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("the background worker could not be started: {error}"))
}

#[cfg(unix)]
fn detach(command: &mut std::process::Command) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("the background worker could not be started: {error}"))
}

#[cfg(not(any(windows, unix)))]
fn detach(command: &mut std::process::Command) -> Result<(), String> {
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("the background worker could not be started: {error}"))
}

/// Start an agent in the worker of `context`'s project.
///
/// # Errors
/// The worker could not be reached or refused the agent.
pub fn create_in(context: &LaunchContext, spec: CreateAgent) -> Result<AgentInfo, String> {
    create_with_worker(context, spec).map(|(_, info)| info)
}

fn create_with_worker(
    context: &LaunchContext,
    spec: CreateAgent,
) -> Result<(Descriptor, AgentInfo), String> {
    let directory = registry::directory(&context.paths.data_dir);
    let (descriptor, mut connection) = ensure(
        &directory,
        &context.project_store_dir(),
        &context.project.root,
    )?;
    let value = connection.call(&Request::Create(Box::new(spec)))?;
    let info = serde_json::from_value(value).map_err(|error| error.to_string())?;
    Ok((descriptor, info))
}

/// One agent and the worker it runs in.
#[derive(Clone, Debug)]
pub struct Listed {
    pub descriptor: Descriptor,
    pub agent: AgentInfo,
}

/// Every agent of every running worker.
#[must_use]
pub fn list_all(directory: &Path) -> Vec<Listed> {
    let mut listed = Vec::new();
    for (key, _) in registry::all(directory) {
        let Some((descriptor, mut connection)) = connect(directory, &key) else {
            continue;
        };
        let Ok(value) = connection.call(&Request::List) else {
            continue;
        };
        let agents: Vec<AgentInfo> = serde_json::from_value(value).unwrap_or_default();
        listed.extend(agents.into_iter().map(|agent| Listed {
            descriptor: descriptor.clone(),
            agent,
        }));
    }
    listed
}

/// The agent `selector` names, across every worker.
///
/// # Errors
/// No agent, or more than one, has that id, name or id prefix.
pub fn find(directory: &Path, selector: &str) -> Result<Listed, String> {
    let listed = list_all(directory);
    let agents = listed
        .iter()
        .map(|listed| listed.agent.clone())
        .collect::<Vec<_>>();
    let id = super::worker::resolve_selector(&agents, selector)?;
    listed
        .into_iter()
        .find(|listed| listed.agent.id == id)
        .ok_or_else(|| format!("no agent {selector:?}"))
}

/// A connection to the worker `listed` runs in.
///
/// # Errors
/// The worker went away.
pub fn open(listed: &Listed) -> Result<Connection, String> {
    Connection::open(&listed.descriptor)
}

/// Start an agent for this terminal's launch and attach to it.
///
/// # Errors
/// No worker could be started, or the agent could not.
pub fn start_attached(
    context: &LaunchContext,
    spec: CreateAgent,
    columns: u16,
) -> Result<RemoteFrontend, String> {
    let (descriptor, agent) = create_with_worker(context, spec)?;
    RemoteFrontend::attach(&descriptor, &agent.id, columns)
}

/// An agent's controller, reached over its worker's socket: what the TUI
/// drives when the conversation runs in the background.
pub struct RemoteFrontend {
    writer: TcpStream,
    /// What the worker sent; an error is why the connection ended.
    replies: Receiver<Result<Reply, String>>,
    agent: AgentInfo,
    banner: Vec<String>,
    /// The conversation so far, drawn by the first pump.
    restore: Option<Vec<HistoryItem>>,
    state: UiState,
    seq: u64,
    /// Why the terminal lost the agent, once it has.
    ended: Option<Ended>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Ended {
    /// `/quit`: the agent carries on.
    Detached,
    /// Another terminal took the agent, or it was stopped.
    TakenAway(String),
    /// The worker went away.
    Lost,
}

impl RemoteFrontend {
    /// Attach to agent `id` of the worker `descriptor` names.
    ///
    /// # Errors
    /// The worker or the agent is gone.
    pub fn attach(descriptor: &Descriptor, id: &str, columns: u16) -> Result<Self, String> {
        let Connection { mut reader, writer } = Connection::open(descriptor)?;
        let mut writer = writer;
        protocol::write_line(
            &mut writer,
            &Request::Attach {
                agent: id.to_owned(),
                columns,
            },
        )
        .map_err(|error| format!("the worker went away: {error}"))?;
        let (agent, banner, history, state) = match protocol::read_line::<Reply>(&mut reader) {
            Ok(Some(Reply::Attached {
                agent,
                banner,
                history,
                state,
            })) => (*agent, banner, history, *state),
            Ok(Some(Reply::Error { message })) => return Err(message),
            Ok(_) => return Err("the worker did not attach the terminal".to_owned()),
            Err(error) => return Err(format!("the worker's answer could not be read: {error}")),
        };
        // The terminal waits on keys, not on the socket; frames arrive whenever
        // the agent produces them.
        // On the handle the reader reads (a duplicated socket handle keeps its
        // own timeout on Windows).
        let _ = reader.get_ref().set_read_timeout(None);
        let (sender, replies) = mpsc::channel();
        std::thread::spawn(move || {
            loop {
                let reply = match protocol::read_line::<Reply>(&mut reader) {
                    Ok(Some(reply)) => Ok(reply),
                    Ok(None) => Err("the worker closed the connection".to_owned()),
                    Err(error) => Err(format!("the worker's message could not be read: {error}")),
                };
                let ended = reply.is_err();
                if sender.send(reply).is_err() || ended {
                    return;
                }
            }
        });
        Ok(Self {
            writer,
            replies,
            agent,
            banner,
            restore: Some(history),
            state,
            seq: 0,
            ended: None,
        })
    }

    #[must_use]
    pub const fn agent(&self) -> &AgentInfo {
        &self.agent
    }

    /// How the terminal lost the agent.
    #[must_use]
    pub const fn ended(&self) -> Option<&Ended> {
        self.ended.as_ref()
    }

    fn send(&mut self, request: &Request) -> bool {
        protocol::write_line(&mut self.writer, request).is_ok()
    }

    /// The effects one reply carries, keeping its view state.
    fn absorb(&mut self, reply: Result<Reply, String>) -> Vec<Effect> {
        match reply {
            Ok(Reply::Frame { effects, state } | Reply::Ack { effects, state, .. }) => {
                if let Some(state) = state {
                    self.state = *state;
                }
                if effects
                    .iter()
                    .any(|effect| matches!(effect, Effect::Exit(_)))
                {
                    self.ended.get_or_insert(Ended::Detached);
                }
                effects
            }
            Ok(Reply::Detached { reason }) => {
                self.ended = Some(Ended::TakenAway(reason.clone()));
                vec![
                    Effect::History(HistoryItem::Notice { message: reason }),
                    Effect::Exit(0),
                ]
            }
            Ok(Reply::Attached { .. } | Reply::Ok { .. } | Reply::Error { .. }) => Vec::new(),
            Err(reason) => self.lost(&reason),
        }
    }

    fn lost(&mut self, reason: &str) -> Vec<Effect> {
        if self.ended.is_some() {
            return Vec::new();
        }
        self.ended = Some(Ended::Lost);
        vec![
            Effect::History(HistoryItem::Error {
                message: format!(
                    "lost the background agent ({reason}); `ha attach {}` reconnects while it runs",
                    self.agent.display_name()
                ),
            }),
            Effect::Exit(1),
        ]
    }
}

impl Frontend for RemoteFrontend {
    fn resume_source(&mut self, _session_id: &str) -> Result<(), String> {
        Err("a background agent resumes when it is started".to_owned())
    }

    fn set_columns(&mut self, columns: u16) {
        let _ = self.send(&Request::Columns { columns });
    }

    fn boot_lines(&mut self) -> Vec<String> {
        self.banner.clone()
    }

    fn ui_state(&self) -> UiState {
        self.state.clone()
    }

    fn handle_key(&mut self, key: Key) -> Vec<Effect> {
        if self.ended.is_some() {
            return Vec::new();
        }
        self.seq += 1;
        let seq = self.seq;
        if !self.send(&Request::Key { seq, key }) {
            return self.lost("the worker went away");
        }
        let deadline = Instant::now() + KEY_TIMEOUT;
        let mut effects = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.replies.recv_timeout(left) {
                Ok(Ok(Reply::Ack {
                    seq: answered,
                    effects: produced,
                    state,
                })) if answered == seq => {
                    effects.extend(self.absorb(Ok(Reply::Ack {
                        seq: answered,
                        effects: produced,
                        state,
                    })));
                    return effects;
                }
                Ok(reply) => {
                    let ended = reply.is_err();
                    effects.extend(self.absorb(reply));
                    if ended || self.ended.is_some() {
                        return effects;
                    }
                }
                Err(RecvTimeoutError::Timeout) => return effects,
                Err(RecvTimeoutError::Disconnected) => {
                    effects.extend(self.lost("the worker went away"));
                    return effects;
                }
            }
        }
    }

    fn pump_events(&mut self) -> Vec<Effect> {
        let mut effects = Vec::new();
        if let Some(history) = self.restore.take() {
            effects.push(Effect::Restore(history));
            effects.push(Effect::Redraw);
        }
        while self.ended.is_none() {
            match self.replies.try_recv() {
                Ok(reply) => effects.extend(self.absorb(reply)),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    effects.extend(self.lost("the worker went away"));
                }
            }
        }
        effects
    }

    fn tick(&mut self) -> Vec<Effect> {
        // The agent ticks itself and sends the frames it draws.
        Vec::new()
    }
}

impl Drop for RemoteFrontend {
    fn drop(&mut self) {
        let _ = self.send(&Request::Detach);
        let _ = self.writer.shutdown(Shutdown::Both);
    }
}

/// Where the worker descriptors of this user are.
///
/// # Errors
/// The data directory cannot be resolved.
pub fn user_registry() -> Result<PathBuf, String> {
    let environment = super::super::paths::LaunchEnvironment::capture();
    let paths = super::super::paths::resolve(&super::super::paths::PathRequest {
        platform: super::super::paths::HostPlatform::current(),
        environment: &environment,
        explicit_data_dir: None,
    })
    .map_err(|error| error.to_string())?;
    Ok(registry::directory(&paths.data_dir))
}

#[cfg(test)]
mod tests {
    use super::ensure;
    use crate::interactive::agents::registry::{self, Descriptor};
    use std::io::{BufRead, Write};

    /// A worker that answers the token and nothing else.
    fn fake_worker() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = listener.local_addr().expect("address").port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                let mut writer = stream;
                let _ = writer.write_all(b"{\"type\":\"ok\",\"value\":{}}\n");
            }
        });
        port
    }

    #[test]
    fn a_worker_of_another_build_is_not_used() {
        let directory = tempfile::tempdir().expect("temp dir");
        let store = directory.path().join("store");
        let key = registry::project_key(&store);
        let mut descriptor = Descriptor {
            schema_version: registry::SCHEMA_VERSION,
            worker_id: "w".to_owned(),
            pid: std::process::id(),
            port: fake_worker(),
            token: "t".to_owned(),
            store_dir: store.clone(),
            project_root: directory.path().to_path_buf(),
            started_at_unix_ms: 1,
            build: "0.0.0+another".to_owned(),
        };
        registry::write(directory.path(), &key, &descriptor).expect("written");
        let refused = ensure(directory.path(), &store, directory.path())
            .err()
            .expect("another build is refused");
        assert!(refused.contains("another build of ha"), "{refused}");
        descriptor.build = registry::build_identity();
        registry::write(directory.path(), &key, &descriptor).expect("written");
        assert!(ensure(directory.path(), &store, directory.path()).is_ok());
    }
}
