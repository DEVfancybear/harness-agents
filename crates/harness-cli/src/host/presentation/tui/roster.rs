//! The other agents of the project, for herdr's sidebar: read on a thread of
//! its own every second (a roster read on the screen's thread stalled every
//! key in the agents view) and handed to the screen through a channel.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use super::sidebar::Status;
use crate::interactive::agents::client;
use crate::interactive::agents::protocol::AgentInfo;

/// How often the roster is read again.
const REFRESH: Duration = Duration::from_secs(1);

/// The agent this terminal draws and the project whose agents it lists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scope {
    pub agent_id: String,
    pub project_root: PathBuf,
}

/// Another agent, as the sidebar lists it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub status: Status,
    /// The folder it works in, when that is not this terminal's project
    /// (`web`): the agents view lists every agent, and one of another folder
    /// left out of the sidebar was an agent gone missing.
    pub folder: Option<String>,
}

/// The roster's thread and the newest list it read.
#[derive(Debug)]
pub struct Poller {
    receiver: Receiver<Vec<Entry>>,
    entries: Vec<Entry>,
}

impl Poller {
    /// Start reading the roster for `scope`. The thread ends when the poller
    /// is dropped.
    #[must_use]
    pub fn start(scope: Scope) -> Self {
        let (sender, receiver) = mpsc::channel();
        // Without a thread the sender is dropped with it, and the sidebar lists
        // this session's agents only.
        let _ = std::thread::Builder::new()
            .name("ha-roster".to_owned())
            .spawn(move || {
                loop {
                    let listed = client::user_registry()
                        .map(|directory| client::list_all(&directory))
                        .unwrap_or_default();
                    let entries = entries(
                        &scope,
                        listed.into_iter().map(|listed| listed.agent).collect(),
                    );
                    if sender.send(entries).is_err() {
                        return;
                    }
                    std::thread::sleep(REFRESH);
                }
            });
        Self {
            receiver,
            entries: Vec::new(),
        }
    }

    /// A roster that never changes: the preview frames and the tests.
    #[cfg(test)]
    #[must_use]
    pub fn fixed(entries: Vec<Entry>) -> Self {
        let (_, receiver) = mpsc::channel();
        Self { receiver, entries }
    }

    /// Take the newest list the thread read; true when it changed.
    pub fn poll(&mut self) -> bool {
        let mut newest = None;
        while let Ok(entries) = self.receiver.try_recv() {
            newest = Some(entries);
        }
        match newest {
            Some(entries) if entries != self.entries => {
                self.entries = entries;
                true
            }
            _ => false,
        }
    }

    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
}

/// Every agent but `scope`'s own, as the agents view lists them: the
/// project's first, then those of other folders (each named by its folder);
/// in each, running first, then by name.
#[must_use]
pub fn entries(scope: &Scope, agents: Vec<AgentInfo>) -> Vec<Entry> {
    let project = normalized(&scope.project_root);
    let mut entries = agents
        .into_iter()
        .filter(|agent| agent.id != scope.agent_id)
        .map(|agent| Entry {
            status: status(&agent),
            name: agent
                .name
                .clone()
                .or_else(|| agent.title.clone())
                .unwrap_or_else(|| agent.id.clone()),
            folder: (normalized(&agent.project_root) != project)
                .then(|| folder(&agent.project_root)),
            id: agent.id,
        })
        .collect::<Vec<_>>();
    // The folder and the id after the name: two agents named alike keep
    // their places from one read to the next.
    entries.sort_by(|left, right| {
        (
            left.folder.is_some(),
            left.status != Status::Working,
            &left.name,
            &left.folder,
            &left.id,
        )
            .cmp(&(
                right.folder.is_some(),
                right.status != Status::Working,
                &right.name,
                &right.folder,
                &right.id,
            ))
    });
    entries
}

/// A folder's last part, as the sidebar names it.
fn folder(path: &Path) -> String {
    let text = path.to_string_lossy();
    text.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(&text)
        .to_owned()
}

/// herdr's state for an agent the roster lists.
fn status(agent: &AgentInfo) -> Status {
    match agent.status.as_str() {
        "waiting_approval" | "waiting_input" | "waiting_mcp_input" => Status::Blocked,
        "running" | "canceling" => Status::Working,
        _ if agent.busy => Status::Working,
        _ => Status::Idle,
    }
}

/// A path as two spellings of the same folder compare equal on Windows.
fn normalized(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    let text = text.strip_prefix("\\\\?\\").unwrap_or(&text);
    text.trim_end_matches('\\').to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{Scope, Status, entries};
    use crate::interactive::agents::protocol::AgentInfo;
    use std::path::PathBuf;

    fn agent(id: &str, name: Option<&str>, root: &str, status: &str) -> AgentInfo {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": name,
            "status": status,
            "busy": status == "running",
            "scheduled": false,
            "attached": false,
            "project_root": root,
            "model": "m",
            "conversation": null,
            "last_request": null,
            "title": null,
            "idle_seconds": 0,
            "worker_pid": 1,
            "subagents": [],
            "heartbeats": 0
        }))
        .expect("an agent")
    }

    /// Seen in the ninja project: the agents view listed four idle agents
    /// (`hello` twice, `0e0dde4f`, and the one the terminal drew) while the
    /// sidebar listed `main` and `0e0dde4f`: both `hello` agents worked in
    /// other folders and were left out without a word.
    #[test]
    fn agents_of_other_folders_are_listed_after_the_projects_own() {
        let scope = Scope {
            agent_id: "me".to_owned(),
            project_root: PathBuf::from("F:\\work\\ninja"),
        };
        let listed = entries(
            &scope,
            vec![
                agent("me", Some("M4 fully closed"), "F:\\work\\ninja", "ready"),
                agent("h1", Some("hello"), "F:\\work\\ninja\\web", "ready"),
                agent(
                    "h2",
                    Some("hello"),
                    "C:\\Users\\d\\Downloads\\scratch\\",
                    "ready",
                ),
                agent("0e0dde4f", None, "f:/work/ninja", "ready"),
            ],
        );
        let shown = listed
            .iter()
            .map(|entry| (entry.name.as_str(), entry.folder.as_deref()))
            .collect::<Vec<_>>();
        assert_eq!(
            shown,
            vec![
                ("0e0dde4f", None),
                ("hello", Some("scratch")),
                ("hello", Some("web")),
            ],
            "the three others the agents view lists, the project's first"
        );
    }

    #[test]
    fn it_lists_the_projects_other_agents_running_first() {
        let scope = Scope {
            agent_id: "me".to_owned(),
            project_root: PathBuf::from("F:\\work\\shop"),
        };
        let listed = entries(
            &scope,
            vec![
                agent("me", Some("this one"), "F:\\work\\shop", "running"),
                agent("a1", Some("tests"), "f:/work/shop/", "ready"),
                agent("a2", None, "\\\\?\\F:\\work\\shop", "running"),
                agent("a3", Some("asks"), "F:\\work\\shop", "waiting_approval"),
                agent("b1", Some("elsewhere"), "F:\\work\\other", "running"),
            ],
        );
        let shown = listed
            .iter()
            .map(|entry| (entry.name.as_str(), entry.status))
            .collect::<Vec<_>>();
        assert_eq!(
            shown,
            vec![
                ("a2", Status::Working),
                ("asks", Status::Blocked),
                ("tests", Status::Idle),
                ("elsewhere", Status::Working),
            ],
            "another folder's agent comes after the project's"
        );
        assert_eq!(listed[3].folder.as_deref(), Some("other"));
    }
}
