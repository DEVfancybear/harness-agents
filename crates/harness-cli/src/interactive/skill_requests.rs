//! The host side of prime-agent's Python skills.
//!
//! prime-agent's skills are thin kernel wrappers over `rlm.host_request`; the host
//! owns the state they read and change (`AgentSession`'s `handleGoalHostRequest`,
//! `handleCompactHostRequest`, the `model.info` handler). This module answers the same
//! requests from `ha`'s state: `goal.*` drives `/goal`, `compact.*` schedules a
//! `/compact` for the end of the turn, `model.info` names the model. Requests from
//! several hosts are chained: the first that knows a type answers it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use super::events::SessionEvent;
use super::repl::{HostReply, HostRequests};

/// Several hosts' requests, answered by the first that knows the type.
pub struct ChainedRequests(pub Vec<Arc<dyn HostRequests>>);

/// The session's own kernel requests, answered between turns too: prime-agent's
/// `bash.completed` and `bash.consumed`. A background command usually finishes
/// after the turn that started it, when no turn's handlers are bound; without
/// these every completion was refused ("... follow-up ... was not accepted").
pub struct BashCompletions {
    pub sender: UnboundedSender<SessionEvent>,
}

impl BashCompletions {
    fn answer(&self, kind: &str, request: &Value) -> Result<Value, String> {
        let pid = request["pid"]
            .as_u64()
            .filter(|pid| *pid > 0)
            .ok_or_else(|| format!("{kind} pid must be a positive integer"))?;
        let command = request["command"]
            .as_str()
            .filter(|command| !command.is_empty())
            .ok_or_else(|| format!("{kind} command must be a non-empty string"))?
            .to_owned();
        let event = if kind == "bash.completed" {
            let exit_code = request["exitCode"]
                .as_i64()
                .ok_or("bash.completed exitCode must be an integer")?;
            SessionEvent::BashCompleted {
                pid,
                command,
                exit_code,
            }
        } else {
            SessionEvent::BashConsumed { pid, command }
        };
        self.sender
            .send(event)
            .map_err(|_| "the session is closing".to_owned())?;
        Ok(json!({}))
    }
}

impl HostRequests for BashCompletions {
    fn handle<'a>(&'a self, request: &'a Value) -> HostReply<'a> {
        Box::pin(async move {
            let kind = request["type"].as_str().unwrap_or_default();
            matches!(kind, "bash.completed" | "bash.consumed").then(|| self.answer(kind, request))
        })
    }
}

impl HostRequests for ChainedRequests {
    fn handle<'a>(&'a self, request: &'a Value) -> HostReply<'a> {
        Box::pin(async move {
            for host in &self.0 {
                if let Some(reply) = host.handle(request).await {
                    return Some(reply);
                }
            }
            None
        })
    }
}

/// prime-agent's generic MCP connections, served from ha's configured servers.
///
/// prime-agent does not give the model MCP tools as native tool namespaces: the
/// kernel's `mcp` object (`rlm.mcp`) opens a configured server itself and calls its
/// tools, asking the host only for the server's configuration (`mcp.config`) and the
/// user's connections (`mcp.list_connections`), as `McpManager.hostHandlers` answers.
/// The plugin listings are prime's service-catalog cards
/// ([`super::mcp_catalog`]); connecting stays the user's (`/plugins`).
pub struct McpRequests {
    servers: std::collections::BTreeMap<String, harness_types::McpServerConfigV2>,
    workspace: std::path::PathBuf,
    plugins: Vec<Value>,
}

impl McpRequests {
    #[must_use]
    pub fn new(
        servers: std::collections::BTreeMap<String, harness_types::McpServerConfigV2>,
        workspace: std::path::PathBuf,
    ) -> Self {
        Self {
            servers,
            workspace,
            plugins: Vec::new(),
        }
    }

    /// The service-catalog cards `mcp.list_plugins` and `mcp.search_plugins`
    /// answer from.
    #[must_use]
    pub fn with_plugins(mut self, plugins: Vec<Value>) -> Self {
        self.plugins = plugins;
        self
    }

    /// ha's server configuration in prime-agent's `McpServerConfig` shape. A
    /// `secret://NAME` value becomes prime-agent's `{"env": "NAME"}` reference; a
    /// configuration with literal values is passed the way prime-agent passes an ACP
    /// server's, with `credentialSource: "acp"`.
    #[must_use]
    pub fn prime_config(&self, name: &str) -> Value {
        let Some(server) = self.servers.get(name) else {
            return json!({});
        };
        let mut config = serde_json::Map::new();
        let transport = server.transport.as_deref().unwrap_or("stdio");
        if transport == "stdio" {
            config.insert("type".to_owned(), json!("stdio"));
            config.insert("command".to_owned(), json!(server.command));
            config.insert("args".to_owned(), json!(server.args));
            if let Some(cwd) = &server.cwd {
                let path = std::path::Path::new(cwd);
                let path = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    self.workspace.join(path)
                };
                config.insert("cwd".to_owned(), json!(path.display().to_string()));
            }
            let literal = server
                .env
                .values()
                .any(|value| !value.starts_with("secret://"));
            let env = server
                .env
                .iter()
                .map(|(key, value)| {
                    let entry = match value.strip_prefix("secret://") {
                        Some(variable) if literal => {
                            json!(std::env::var(variable).unwrap_or_default())
                        }
                        Some(variable) => json!({ "env": variable }),
                        None => json!(value),
                    };
                    (key.clone(), entry)
                })
                .collect::<serde_json::Map<_, _>>();
            config.insert("env".to_owned(), Value::Object(env));
            if literal {
                config.insert("credentialSource".to_owned(), json!("acp"));
            }
        } else {
            config.insert("type".to_owned(), json!("http"));
            config.insert("url".to_owned(), json!(server.url));
            if let Some(variable) = &server.bearer_token_env {
                config.insert("bearerTokenEnvVar".to_owned(), json!(variable));
            }
        }
        if !server.enabled_tools.is_empty() {
            config.insert("enabledTools".to_owned(), json!(server.enabled_tools));
        }
        if !server.disabled_tools.is_empty() {
            config.insert("disabledTools".to_owned(), json!(server.disabled_tools));
        }
        if let Some(seconds) = server.tool_timeout_seconds {
            config.insert(
                "callTimeoutMs".to_owned(),
                json!(seconds.saturating_mul(1000)),
            );
        }
        config.insert("enabled".to_owned(), json!(true));
        Value::Object(config)
    }
}

impl HostRequests for McpRequests {
    fn handle<'a>(&'a self, request: &'a Value) -> HostReply<'a> {
        Box::pin(async move {
            let kind = request["type"].as_str().unwrap_or_default();
            let server = request["server"].as_str().unwrap_or_default();
            Some(match kind {
                "mcp.config" if server.is_empty() => Err("mcp.config requires a server".to_owned()),
                "mcp.config" if !self.servers.contains_key(server) => Err(format!(
                    "no MCP server named {server:?} is configured in ha; configured: [{}]. Add it under [mcp_servers.{server}] in ha's config.toml (see /config)",
                    self.servers.keys().cloned().collect::<Vec<_>>().join(", ")
                )),
                "mcp.config" => Ok(self.prime_config(server)),
                "mcp.list_connections" => Ok(json!({
                    "connections": self.servers.iter().map(|(name, server)| json!({
                        "connectionId": name,
                        "label": name,
                        "service": name,
                        "transport": server.transport.as_deref().unwrap_or("stdio"),
                        "status": "configured",
                        "source": "ha config",
                    })).collect::<Vec<_>>(),
                })),
                "mcp.list_plugins" => super::mcp_catalog::page(
                    &self.plugins,
                    request["connectionStatus"].as_str(),
                    request["cursor"].as_str(),
                    request["limit"]
                        .as_u64()
                        .and_then(|limit| usize::try_from(limit).ok())
                        .unwrap_or(50),
                ),
                "mcp.search_plugins" => Ok(json!({
                    "plugins": super::mcp_catalog::search(
                        &self.plugins,
                        request["query"].as_str().unwrap_or_default(),
                        request["limit"]
                            .as_u64()
                            .and_then(|limit| usize::try_from(limit).ok())
                            .unwrap_or(10),
                    ),
                    "nextCursor": Value::Null,
                })),
                "mcp.refresh" | "mcp.begin_login" | "mcp.connect" => Err(format!(
                    "{kind} is not available to the agent: the user connects services with /plugins or /mcp login <service>"
                )),
                _ => return None,
            })
        })
    }
}

/// The model a turn runs on, as `model.info` reports it.
#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    pub images: bool,
}

/// `goal.*`, `compact.*` and `model.info` for one turn.
pub struct SkillRequests {
    /// A refinement the `refine` skill scheduled for the end of the turn.
    refine: Arc<std::sync::Mutex<Option<super::refine::RefineOptions>>>,
    sender: UnboundedSender<SessionEvent>,
    model: ModelInfo,
    context_window: u64,
    /// The goal the turn started with, if one was active.
    goal: Option<String>,
    goal_host: Option<super::goal::GoalHost>,
    /// A goal this turn created through `goal.create`.
    created: std::sync::Mutex<Option<String>>,
    completed: AtomicBool,
    compact_scheduled: AtomicBool,
    /// The session's usage, for what the context holds now.
    usage: Option<Arc<std::sync::Mutex<super::cost::CostTracker>>>,
    /// The token budget of a goal this turn created.
    token_budget: std::sync::Mutex<Option<u64>>,
}

impl SkillRequests {
    #[must_use]
    pub fn new(
        refine: Arc<std::sync::Mutex<Option<super::refine::RefineOptions>>>,
        sender: UnboundedSender<SessionEvent>,
        model: ModelInfo,
        context_window: u64,
        goal: Option<String>,
        goal_host: Option<super::goal::GoalHost>,
    ) -> Self {
        Self {
            refine,
            sender,
            model,
            context_window,
            goal,
            goal_host,
            created: std::sync::Mutex::new(None),
            completed: AtomicBool::new(false),
            compact_scheduled: AtomicBool::new(false),
            usage: None,
            token_budget: std::sync::Mutex::new(None),
        }
    }

    /// Read the context's size from the session's usage, for `compact.status`.
    #[must_use]
    pub fn with_usage(mut self, usage: Arc<std::sync::Mutex<super::cost::CostTracker>>) -> Self {
        self.usage = Some(usage);
        self
    }

    fn objective(&self) -> Option<String> {
        self.created
            .lock()
            .ok()
            .and_then(|created| created.clone())
            .or_else(|| self.goal.clone())
    }

    /// `goalHostResponse`: the goal serialized the way the `goal` skill reads it.
    /// The budget is the one this turn's `goal.create` set; what it has spent is
    /// the app's to count, so `tokens_used` stays null here.
    fn goal_response(&self) -> Value {
        let Some(objective) = self.objective() else {
            return json!({ "goal": null, "remaining_tokens": null, "completion_budget_report": null });
        };
        let status = if self.completed.load(Ordering::SeqCst) {
            "complete"
        } else {
            "active"
        };
        json!({
            "goal": {
                "goal_id": null,
                "objective": objective,
                "status": status,
                "token_budget": self.token_budget.lock().ok().and_then(|budget| *budget),
                "tokens_used": null,
                "time_used_seconds": null,
                "created_at": null,
                "updated_at": null,
            },
            "remaining_tokens": null,
            "completion_budget_report": null,
        })
    }

    async fn goal(&self, kind: &str, request: &Value) -> Result<Value, String> {
        match kind {
            "goal.get" => Ok(self.goal_response()),
            "goal.create" => {
                let objective = request["objective"]
                    .as_str()
                    .map(str::trim)
                    .filter(|objective| !objective.is_empty())
                    .ok_or("goal.create objective must be a string")?;
                // prime-agent's `validate_goal_budget`: a positive integer.
                let token_budget = match &request["token_budget"] {
                    Value::Null => None,
                    value => Some(
                        value
                            .as_u64()
                            .filter(|budget| *budget > 0)
                            .ok_or("Goal token budget must be a positive integer.")?,
                    ),
                };
                if self.objective().is_some() && !self.completed.load(Ordering::SeqCst) {
                    return Err("cannot create a new goal because this thread already has an active goal; run `await goal.complete()` when it is achieved, or ask the user to clear it with /goal clear".to_owned());
                }
                if let Ok(mut created) = self.created.lock() {
                    *created = Some(objective.to_owned());
                }
                self.completed.store(false, Ordering::SeqCst);
                if let Ok(mut budget) = self.token_budget.lock() {
                    *budget = token_budget;
                }
                let _ = self.sender.send(SessionEvent::GoalCreated {
                    objective: objective.to_owned(),
                    token_budget,
                });
                Ok(self.goal_response())
            }
            "goal.complete" => {
                let Some(objective) = self.objective() else {
                    return Err("there is no goal to complete".to_owned());
                };
                if !self.completed.load(Ordering::SeqCst) {
                    let summary = format!("completed: {objective}");
                    match &self.goal_host {
                        // The harness verifies before it completes; what is left
                        // reaches the kernel as the call's error.
                        Some(host) => {
                            host.finish(&summary).await?;
                        }
                        None => {
                            let _ = self.sender.send(SessionEvent::GoalCompleted { summary });
                        }
                    }
                    self.completed.store(true, Ordering::SeqCst);
                }
                Ok(self.goal_response())
            }
            _ => Err(format!("unknown goal request type \"{kind}\"")),
        }
    }

    /// `handleRefineHostRequest`: refinement waits for the turn to end, so
    /// `refine.run` only schedules it.
    fn refine(&self, kind: &str, request: &Value) -> Result<Value, String> {
        match kind {
            "refine.status" => Ok(json!({
                "pending": self.refine.lock().is_ok_and(|pending| pending.is_some()),
                "in_flight": false,
            })),
            "refine.run" => {
                let instructions = match &request["instructions"] {
                    Value::Null => None,
                    Value::String(text) => Some(text.clone()),
                    _ => {
                        return Err(
                            "refine.run instructions must be a string when provided".to_owned()
                        );
                    }
                };
                let global = match &request["global"] {
                    Value::Null => false,
                    Value::Bool(flag) => *flag,
                    _ => return Err("refine.run global must be a boolean when provided".to_owned()),
                };
                if let Ok(mut pending) = self.refine.lock() {
                    let previous = pending.take().unwrap_or_default();
                    *pending = Some(super::refine::RefineOptions {
                        instructions: instructions.or(previous.instructions),
                        global: global || previous.global,
                        rollback: None,
                        curate: false,
                    });
                }
                Ok(json!({
                    "scheduled": true,
                    "note": "Refinement runs when the current turn ends; applied edits are reported and reach your context through the harness digest. Continue working normally.",
                }))
            }
            _ => Err(format!("unknown refine request type \"{kind}\"")),
        }
    }

    /// `handleCompactHostRequest`: a compaction would end the turn running the
    /// requesting cell, so `compact.run` only schedules it for the turn's end.
    fn compact(&self, kind: &str, request: &Value) -> Result<Value, String> {
        match kind {
            // prime-agent's `context_usage`: the context the latest response
            // reported, against the window; unknown before a response.
            "compact.status" => {
                let tokens = self
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.lock().ok()?.context_tokens());
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "a percentage of token counts far below 2^52"
                )]
                let percent = tokens.filter(|_| self.context_window > 0).map(|tokens| {
                    (tokens as f64 * 1000.0 / self.context_window as f64).round() / 10.0
                });
                Ok(json!({
                    "tokens": tokens,
                    "context_window": self.context_window,
                    "percent": percent,
                    "scheduled": self.compact_scheduled.load(Ordering::SeqCst),
                }))
            }
            "compact.run" => {
                let instructions = match &request["instructions"] {
                    Value::Null => None,
                    Value::String(text) => Some(text.clone()),
                    _ => {
                        return Err(
                            "compact.run instructions must be a string when provided".to_owned()
                        );
                    }
                };
                self.compact_scheduled.store(true, Ordering::SeqCst);
                let _ = self
                    .sender
                    .send(SessionEvent::CompactRequested { instructions });
                Ok(json!({
                    "scheduled": true,
                    "note": "Compaction runs when the current turn ends; you resume automatically afterwards. Continue working normally.",
                }))
            }
            _ => Err(format!("unknown compact request type \"{kind}\"")),
        }
    }
}

impl HostRequests for SkillRequests {
    fn handle<'a>(&'a self, request: &'a Value) -> HostReply<'a> {
        Box::pin(async move {
            let kind = request["type"].as_str().unwrap_or_default();
            Some(match kind {
                "model.info" => Ok(json!({
                    "id": self.model.id,
                    "provider": self.model.provider,
                    "input": if self.model.images { json!(["text", "image"]) } else { json!(["text"]) },
                })),
                kind if kind.starts_with("goal.") => self.goal(kind, request).await,
                kind if kind.starts_with("compact.") => self.compact(kind, request),
                kind if kind.starts_with("refine.") => self.refine(kind, request),
                _ => return None,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ModelInfo, SkillRequests};
    use crate::interactive::events::SessionEvent;
    use crate::interactive::repl::HostRequests;
    use serde_json::json;

    fn requests(
        goal: Option<&str>,
    ) -> (
        SkillRequests,
        tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
    ) {
        let (sender, events) = tokio::sync::mpsc::unbounded_channel();
        (
            SkillRequests::new(
                std::sync::Arc::new(std::sync::Mutex::new(None)),
                sender,
                ModelInfo {
                    id: "deepseek-v4-flash".to_owned(),
                    provider: "deepseek".to_owned(),
                    images: false,
                },
                128_000,
                goal.map(str::to_owned),
                None,
            ),
            events,
        )
    }

    #[tokio::test]
    async fn the_goal_skill_reads_creates_and_completes_the_goal() {
        let (host, mut events) = requests(None);
        let reply = host
            .handle(&json!({"type": "goal.get"}))
            .await
            .expect("known")
            .expect("ok");
        assert!(reply["goal"].is_null());
        // prime-agent's `validate_goal_budget`: a positive integer, or none.
        assert!(
            host.handle(&json!({"type": "goal.create", "objective": "x", "token_budget": 0}))
                .await
                .expect("known")
                .is_err(),
            "a budget must be a positive integer"
        );
        let created = host
            .handle(&json!({"type": "goal.create", "objective": "ship it", "token_budget": 5000}))
            .await
            .expect("known")
            .expect("ok");
        assert_eq!(created["goal"]["status"], "active");
        assert!(matches!(
            events.try_recv(),
            Ok(SessionEvent::GoalCreated { objective, token_budget: Some(5000) }) if objective == "ship it"
        ));
        assert!(
            host.handle(&json!({"type": "goal.create", "objective": "another"}))
                .await
                .expect("known")
                .is_err(),
            "one active goal at a time"
        );
        let done = host
            .handle(&json!({"type": "goal.complete"}))
            .await
            .expect("known")
            .expect("ok");
        assert_eq!(done["goal"]["status"], "complete");
        assert!(matches!(
            events.try_recv(),
            Ok(SessionEvent::GoalCompleted { .. })
        ));
    }

    #[tokio::test]
    async fn mcp_servers_are_served_in_prime_agents_shape() {
        use super::McpRequests;
        let mut servers = std::collections::BTreeMap::new();
        servers.insert(
            "files".to_owned(),
            harness_types::McpServerConfigV2 {
                command: Some("node".to_owned()),
                args: vec!["server.js".to_owned()],
                env: [("TOKEN".to_owned(), "secret://FILES_TOKEN".to_owned())].into(),
                cwd: Some("tools".to_owned()),
                ..harness_types::McpServerConfigV2::default()
            },
        );
        servers.insert(
            "remote".to_owned(),
            harness_types::McpServerConfigV2 {
                transport: Some("streamable_http".to_owned()),
                url: Some("https://mcp.example.com/mcp".to_owned()),
                bearer_token_env: Some("REMOTE_TOKEN".to_owned()),
                ..harness_types::McpServerConfigV2::default()
            },
        );
        let host = McpRequests::new(servers, std::path::PathBuf::from("/work"));
        let files = host
            .handle(&json!({"type": "mcp.config", "server": "files"}))
            .await
            .expect("known")
            .expect("ok");
        assert_eq!(files["type"], "stdio");
        assert_eq!(files["env"]["TOKEN"], json!({"env": "FILES_TOKEN"}));
        assert!(files.get("credentialSource").is_none());
        assert!(
            files["cwd"]
                .as_str()
                .is_some_and(|cwd| cwd.ends_with("tools"))
        );
        let remote = host
            .handle(&json!({"type": "mcp.config", "server": "remote"}))
            .await
            .expect("known")
            .expect("ok");
        assert_eq!(remote["type"], "http");
        assert_eq!(remote["bearerTokenEnvVar"], "REMOTE_TOKEN");
        let unknown = host
            .handle(&json!({"type": "mcp.config", "server": "nope"}))
            .await
            .expect("known")
            .expect_err("an undeclared server is named as such");
        assert!(unknown.contains("files, remote"), "{unknown}");
        let connections = host
            .handle(&json!({"type": "mcp.list_connections"}))
            .await
            .expect("known")
            .expect("ok");
        assert_eq!(connections["connections"].as_array().map(Vec::len), Some(2));
        assert!(
            host.handle(&json!({"type": "mcp.refresh", "server": "remote"}))
                .await
                .expect("known")
                .is_err()
        );
    }

    #[tokio::test]
    async fn compaction_is_scheduled_for_the_end_of_the_turn() {
        let (host, mut events) = requests(Some("goal"));
        let reply = host
            .handle(&json!({"type": "compact.run", "instructions": "keep the plan"}))
            .await
            .expect("known")
            .expect("ok");
        assert_eq!(reply["scheduled"], true);
        assert!(matches!(
            events.try_recv(),
            Ok(SessionEvent::CompactRequested { instructions: Some(text) }) if text == "keep the plan"
        ));
        let status = host
            .handle(&json!({"type": "compact.status"}))
            .await
            .expect("known")
            .expect("ok");
        assert_eq!(status["scheduled"], true);
        assert_eq!(status["context_window"], 128_000);
        let model = host
            .handle(&json!({"type": "model.info"}))
            .await
            .expect("known")
            .expect("ok");
        assert_eq!(model["id"], "deepseek-v4-flash");
        assert!(
            host.handle(&json!({"type": "something.else"}))
                .await
                .is_none()
        );
        let scheduled = host
            .handle(&json!({"type": "refine.run", "instructions": "remember tabs", "global": true}))
            .await
            .expect("known")
            .expect("ok");
        assert_eq!(scheduled["scheduled"], true);
        let status = host
            .handle(&json!({"type": "refine.status"}))
            .await
            .expect("known")
            .expect("ok");
        assert_eq!(status["pending"], true);
    }
}
