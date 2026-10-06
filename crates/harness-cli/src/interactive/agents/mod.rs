//! Background agents: prime-agent's daemon, as `ha` runs it.
//!
//! An interactive session runs in a background worker process and the terminal
//! attaches to it. Closing the terminal (`/quit`, ctrl+d) detaches: the agent
//! keeps its running turn, its goal, its children and its schedules, and
//! `ha attach <agent>` brings the terminal back. `ha agents` lists what runs,
//! `ha send` messages an agent, `ha stop` stops one and `ha shutdown` stops all.
//! An agent with no terminal, no work and no schedule stops after prime-agent's
//! `idleEvictionMinutes` (90 by default, `"off"` to keep it).
//!
//! `HA_DAEMON=off` (or `"daemon": false` in settings.json) runs the session in
//! the terminal as before.

pub mod client;
pub mod protocol;
pub mod registry;
pub mod worker;

use std::path::PathBuf;
use std::process::ExitCode;

use harness_types::{ErrorCode, HarnessError};

use super::paths::LaunchEnvironment;

/// prime-agent's `rlm.create_session` arguments.
#[derive(Clone, Debug, Default)]
pub struct CreateSession {
    pub prompt: String,
    pub name: Option<String>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub cwd: Option<PathBuf>,
}

/// Where `rlm.create_session` starts a separate top-level agent. Only an agent
/// that runs in a background worker has one.
pub trait SessionHost: Send + Sync {
    /// Start the agent and send it the prompt; the answer is prime-agent's
    /// `RLMCreateSessionHandle` payload.
    ///
    /// # Errors
    /// The name is taken, the model or thinking level is unknown, or no
    /// worker could start the agent.
    fn create_session(&self, request: CreateSession) -> Result<serde_json::Value, String>;
}

/// Whether an interactive session runs as a background agent: yes, unless
/// `HA_DAEMON` or the `daemon` setting turns it off.
#[must_use]
pub fn enabled(environment: &LaunchEnvironment, config_file: &std::path::Path) -> bool {
    if let Some(value) = environment
        .value("HA_DAEMON")
        .and_then(|value| value.to_str())
    {
        return !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "off" | "false" | "no"
        );
    }
    super::config::load_setting(config_file, "daemon")
        .and_then(|value| value.as_bool())
        .unwrap_or(true)
}

/// The spec of the agent a terminal launch starts.
#[must_use]
pub fn spec_for_launch(
    context: &super::bootstrap::LaunchContext,
    environment: &LaunchEnvironment,
    cwd: Option<PathBuf>,
    resume: Option<String>,
    fixture: bool,
    overrides: &super::config::ConfigOverrides,
) -> protocol::CreateAgent {
    protocol::CreateAgent {
        caller_dir: context.caller_dir.clone(),
        cwd,
        environment: environment.pairs(),
        resume,
        fixture,
        model: overrides.model.clone(),
        profile: overrides.profile.clone(),
        approval: overrides.approval.clone(),
        name: None,
        prompt: overrides.initial_prompt.clone(),
        thinking: overrides.thinking.clone(),
        goal: overrides.goal.clone(),
        system_prompt: overrides.system_prompt.clone(),
        append_system_prompt: overrides.append_system_prompt.clone(),
        id: None,
    }
}

/// The line the terminal leaves when it detaches from an agent.
#[must_use]
pub fn detach_hint(remote: &client::RemoteFrontend) -> Option<String> {
    let agent = remote.agent();
    match remote.ended() {
        Some(client::Ended::Detached) | None => Some(format!(
            "ha: agent {} keeps running in the background; reattach with `ha attach {}`",
            agent.display_name(),
            agent.display_name()
        )),
        Some(client::Ended::TakenAway(_) | client::Ended::Lost) => None,
    }
}

fn failure(message: impl Into<String>) -> HarnessError {
    HarnessError::new(ErrorCode::RuntimeBlocked, message.into())
}

fn print_json(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}

/// `ha agents` / `ha list`: every agent of every running worker.
///
/// # Errors
/// The data directory cannot be resolved.
pub fn list_command(json: bool) -> Result<ExitCode, HarnessError> {
    let registry = client::user_registry().map_err(failure)?;
    // Agents a reboot or a dead worker left in a journal come back first.
    if client::recover_journals(&registry) > 0 {
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    let listed = client::list_all(&registry);
    if json {
        print_json(&serde_json::json!({
            "schema_version": 1,
            "agents": listed.iter().map(|listed| &listed.agent).collect::<Vec<_>>(),
        }));
        return Ok(ExitCode::SUCCESS);
    }
    if listed.is_empty() {
        println!("No background agents are running.");
        return Ok(ExitCode::SUCCESS);
    }
    for line in table(&listed) {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

fn table(listed: &[client::Listed]) -> Vec<String> {
    let mut lines = vec![format!(
        "{:<10} {:<16} {:<18} {:<9} {}",
        "AGENT", "NAME", "STATUS", "IDLE", "PROJECT"
    )];
    for listed in listed {
        let agent = &listed.agent;
        let mut status = agent.status.clone();
        if agent.attached {
            status.push_str(" *");
        }
        let idle = if agent.status == "running" || agent.busy {
            "-".to_owned()
        } else {
            idle_label(agent.idle_seconds)
        };
        lines.push(format!(
            "{:<10} {:<16} {:<18} {:<9} {}",
            agent.id,
            agent.name.as_deref().unwrap_or("-"),
            status,
            idle,
            agent.project_root.display()
        ));
        if let Some(request) = &agent.last_request {
            let request = request.lines().next().unwrap_or_default();
            let short: String = request.chars().take(72).collect();
            lines.push(format!("{:<10} {short}", ""));
        }
    }
    lines.push("* a terminal is attached".to_owned());
    lines
}

fn idle_label(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        _ => format!("{}h{}m", seconds / 3600, seconds % 3600 / 60),
    }
}

/// `ha send`: deliver a message to an agent.
///
/// # Errors
/// No such agent, or its worker refused the message.
pub fn send_command(
    selector: &str,
    text: String,
    from: Option<String>,
    mode: protocol::SendMode,
    json: bool,
    wait: bool,
) -> Result<ExitCode, HarnessError> {
    let registry = client::user_registry().map_err(failure)?;
    let (listed, status) = match client::find(&registry, selector) {
        Ok(listed) => {
            let mut connection = client::open(&listed).map_err(failure)?;
            let value = connection
                .call(&protocol::Request::Send {
                    agent: listed.agent.id.clone(),
                    text,
                    from,
                    mode,
                })
                .map_err(failure)?;
            let status = value["status"].as_str().unwrap_or("delivered").to_owned();
            (listed, status)
        }
        // prime-agent wakes a saved session a message is sent to: a
        // conversation of this project, named by its session id, starts again
        // as an agent with the message as its first prompt.
        Err(unknown) => match wake_saved(&registry, selector, text) {
            Some(woken) => (woken.map_err(failure)?, "delivered".to_owned()),
            None => return Err(failure(unknown)),
        },
    };
    if wait {
        // prime-agent's `prompt_and_wait`: the turn the message started runs
        // to its end, and its answer is the result.
        let answer = wait_for_answer(&registry, &listed.agent.id)?;
        if json {
            print_json(&serde_json::json!({
                "schema_version": 1,
                "agent": listed.agent.id,
                "deliveryStatus": status,
                "text": answer,
            }));
        } else {
            println!("{answer}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    if json {
        print_json(&serde_json::json!({
            "schema_version": 1,
            "agent": listed.agent.id,
            "deliveryStatus": status,
        }));
    } else {
        println!("{status} to {}", listed.agent.display_name());
    }
    Ok(ExitCode::SUCCESS)
}

/// prime-agent's `wake_saved_target`: a selector no running agent answers to
/// that is a saved conversation's session id resumes it as a new agent of this
/// directory's project, whose first prompt is `text`. `None` when the selector
/// is not a session id.
fn wake_saved(
    registry: &std::path::Path,
    selector: &str,
    text: String,
) -> Option<Result<client::Listed, String>> {
    harness_types::SessionId::parse(selector.to_owned()).ok()?;
    Some((|| {
        let environment = LaunchEnvironment::capture();
        let context = super::bootstrap::resolve(super::bootstrap::LaunchRequest {
            cwd: None,
            caller_dir: std::env::current_dir().map_err(|error| error.to_string())?,
            platform: super::paths::HostPlatform::current(),
            environment: environment.clone(),
            explicit_data_dir: None,
        })
        .map_err(|error| error.to_string())?;
        let overrides = super::config::ConfigOverrides {
            initial_prompt: Some(text),
            ..super::config::ConfigOverrides::default()
        };
        let spec = spec_for_launch(
            &context,
            &environment,
            None,
            Some(selector.to_owned()),
            false,
            &overrides,
        );
        let agent = client::create_in(&context, spec)?;
        client::find(registry, &agent.id)
    })())
}

/// Wait until agent `id` has started on what was sent and gone idle again,
/// then read its answer.
fn wait_for_answer(registry: &std::path::Path, id: &str) -> Result<String, HarnessError> {
    let started = std::time::Instant::now();
    let mut seen_working = false;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let listed = client::find(registry, id).map_err(failure)?;
        let working = listed.agent.status != "ready" || listed.agent.busy;
        seen_working |= working;
        // A message that started nothing visible within a few seconds has
        // been answered already, or was queued behind nothing.
        if !working && (seen_working || started.elapsed() > std::time::Duration::from_secs(5)) {
            let mut connection = client::open(&listed).map_err(failure)?;
            let value = connection
                .call(&protocol::Request::LastAnswer {
                    agent: id.to_owned(),
                })
                .map_err(failure)?;
            return Ok(value["text"].as_str().unwrap_or_default().to_owned());
        }
    }
}

/// Run `/schedule <argument>` in one agent's conversation.
fn schedule_call(listed: &client::Listed, argument: &str) -> Result<Vec<String>, String> {
    let mut connection = client::open(listed)?;
    let value = connection.call(&protocol::Request::Schedule {
        agent: listed.agent.id.clone(),
        argument: argument.to_owned(),
    })?;
    Ok(value["lines"]
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .filter_map(|line| line.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default())
}

/// A `/schedule list` line: `<job id> <status> ...`.
fn job_line(line: &str) -> Option<(&str, &str)> {
    let mut words = line.split_whitespace();
    let id = words.next().filter(|id| id.starts_with("job-"))?;
    Some((id, words.next()?))
}

/// prime-agent's `schedule list [--all] [agent] [--json]`: the scheduled
/// prompts of one agent or of every agent; without `--all` only the ones
/// still to run.
///
/// # Errors
/// No such agent.
pub fn schedule_list(
    selector: Option<&str>,
    all: bool,
    json: bool,
) -> Result<ExitCode, HarnessError> {
    let registry = client::user_registry().map_err(failure)?;
    let agents = match selector {
        Some(selector) => vec![client::find(&registry, selector).map_err(failure)?],
        None => client::list_all(&registry),
    };
    let mut jobs = Vec::new();
    for listed in &agents {
        let Ok(lines) = schedule_call(listed, "list") else {
            continue;
        };
        for line in lines {
            let Some((id, status)) = job_line(&line) else {
                continue;
            };
            if !all && !matches!(status, "active" | "paused") {
                continue;
            }
            jobs.push((listed.agent.display_name(), id.to_owned(), line.clone()));
        }
    }
    if json {
        print_json(&serde_json::json!({
            "schema_version": 1,
            "jobs": jobs
                .iter()
                .map(|(agent, id, line)| serde_json::json!({ "agent": agent, "id": id, "line": line }))
                .collect::<Vec<_>>(),
        }));
    } else if jobs.is_empty() {
        println!("No scheduled prompts.");
    } else {
        for (agent, _, line) in &jobs {
            println!("{agent} {line}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// prime-agent's `schedule add <agent> <schedule> -- <message>`.
///
/// # Errors
/// No such agent, or a schedule the agent does not take.
pub fn schedule_add(
    selector: &str,
    when: &str,
    message: &str,
    json: bool,
) -> Result<ExitCode, HarnessError> {
    let registry = client::user_registry().map_err(failure)?;
    let listed = client::find(&registry, selector).map_err(failure)?;
    let lines = schedule_call(&listed, &format!("add {when} -- {message}")).map_err(failure)?;
    let line = lines.first().cloned().unwrap_or_default();
    if json {
        print_json(&serde_json::json!({
            "schema_version": 1,
            "agent": listed.agent.id,
            "job": line.strip_prefix("scheduled ").unwrap_or(&line),
        }));
    } else {
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

/// prime-agent's `schedule cancel <job-id>`. ha's job ids are counted per
/// conversation, so an id two agents share needs the agent before it.
///
/// # Errors
/// No such job, or an id more than one agent has.
pub fn schedule_cancel(
    selector: Option<&str>,
    job: &str,
    json: bool,
) -> Result<ExitCode, HarnessError> {
    let registry = client::user_registry().map_err(failure)?;
    let listed = if let Some(selector) = selector {
        client::find(&registry, selector).map_err(failure)?
    } else {
        let mut owners = client::list_all(&registry)
            .into_iter()
            .filter(|listed| {
                schedule_call(listed, "list").is_ok_and(|lines| {
                    lines
                        .iter()
                        .any(|line| job_line(line).is_some_and(|(id, _)| id == job))
                })
            })
            .collect::<Vec<_>>();
        match owners.len() {
            0 => return Err(failure(format!("no scheduled job {job}"))),
            1 => owners.remove(0),
            _ => {
                return Err(failure(format!(
                    "more than one agent has {job}: ha schedule cancel <agent> {job}"
                )));
            }
        }
    };
    let lines = schedule_call(&listed, &format!("cancel {job}")).map_err(failure)?;
    if json {
        print_json(&serde_json::json!({
            "schema_version": 1,
            "agent": listed.agent.id,
            "job": lines.first(),
        }));
    } else {
        println!("Cancelled {job} of {}", listed.agent.display_name());
    }
    Ok(ExitCode::SUCCESS)
}

/// `ha abort`: end an agent's running turn; the agent stays.
///
/// # Errors
/// No such agent.
pub fn abort_command(selector: &str, json: bool) -> Result<ExitCode, HarnessError> {
    let registry = client::user_registry().map_err(failure)?;
    let listed = client::find(&registry, selector).map_err(failure)?;
    let mut connection = client::open(&listed).map_err(failure)?;
    let value = connection
        .call(&protocol::Request::Abort {
            agent: listed.agent.id.clone(),
        })
        .map_err(failure)?;
    let aborted = value["aborted"].as_bool().unwrap_or(false);
    if json {
        print_json(&serde_json::json!({
            "schema_version": 1,
            "agent": listed.agent.id,
            "aborted": aborted,
        }));
    } else if aborted {
        println!(
            "aborted the running turn of {}",
            listed.agent.display_name()
        );
    } else {
        println!("{} has no running turn", listed.agent.display_name());
    }
    Ok(ExitCode::SUCCESS)
}

/// `ha stop`: stop one agent.
///
/// # Errors
/// No such agent.
pub fn stop_command(selector: &str, json: bool) -> Result<ExitCode, HarnessError> {
    let registry = client::user_registry().map_err(failure)?;
    let listed = client::find(&registry, selector).map_err(failure)?;
    let mut connection = client::open(&listed).map_err(failure)?;
    connection
        .call(&protocol::Request::Stop {
            agent: listed.agent.id.clone(),
        })
        .map_err(failure)?;
    if json {
        print_json(&serde_json::json!({
            "schema_version": 1,
            "stopped": listed.agent.id,
        }));
    } else {
        println!("stopped {}", listed.agent.display_name());
    }
    Ok(ExitCode::SUCCESS)
}

/// `ha rename`: give an agent a name `ha attach` and `ha send` accept.
///
/// # Errors
/// No such agent, or the name is taken.
pub fn rename_command(selector: &str, name: &str, json: bool) -> Result<ExitCode, HarnessError> {
    let registry = client::user_registry().map_err(failure)?;
    let listed = client::find(&registry, selector).map_err(failure)?;
    if client::list_all(&registry).iter().any(|other| {
        other.agent.id != listed.agent.id && other.agent.name.as_deref() == Some(name.trim())
    }) {
        return Err(failure(format!(
            "an agent is already named {:?}",
            name.trim()
        )));
    }
    let mut connection = client::open(&listed).map_err(failure)?;
    connection
        .call(&protocol::Request::Rename {
            agent: listed.agent.id.clone(),
            name: name.to_owned(),
        })
        .map_err(failure)?;
    if json {
        print_json(&serde_json::json!({
            "schema_version": 1,
            "agent": listed.agent.id,
            "name": name.trim(),
        }));
    } else {
        println!("{} is now {}", listed.agent.id, name.trim());
    }
    Ok(ExitCode::SUCCESS)
}

/// `ha shutdown`: stop every agent and every worker.
///
/// # Errors
/// Without `--force`, the user did not confirm.
pub fn shutdown_command(force: bool, json: bool) -> Result<ExitCode, HarnessError> {
    let registry = client::user_registry().map_err(failure)?;
    let listed = client::list_all(&registry);
    if !force {
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            return Err(failure(
                "ha shutdown stops every background agent; confirm with --force",
            ));
        }
        println!(
            "Stop {} background agent(s) and their workers? [y/N]",
            listed.len()
        );
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("nothing was stopped");
            return Ok(ExitCode::SUCCESS);
        }
    }
    let mut stopped = 0_usize;
    for (key, descriptor) in registry::all(&registry) {
        if let Some((_, mut connection)) = client::connect(&registry, &key)
            && connection.call(&protocol::Request::Shutdown).is_ok()
        {
            stopped += 1;
        } else if force && registry::process_is_alive(descriptor.pid) {
            // An unresponsive worker: the descriptor goes, and the worker
            // stops when it next finds it gone.
            registry::remove_if_owned(&registry, &key, descriptor.pid);
        }
    }
    if json {
        print_json(&serde_json::json!({
            "schema_version": 1,
            "workers_stopped": stopped,
            "agents_stopped": listed.len(),
        }));
    } else {
        println!("stopped {} agent(s) in {stopped} worker(s)", listed.len());
    }
    Ok(ExitCode::SUCCESS)
}

/// `ha worker`: run one project's background worker (started by `ha`).
///
/// # Errors
/// The worker could not start.
pub fn worker_command(args: &worker::WorkerArgs) -> Result<ExitCode, HarnessError> {
    if args.serve {
        worker::run(args).map_err(failure)?;
    } else {
        worker::supervise_process(args).map_err(failure)?;
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::{enabled, idle_label};
    use crate::interactive::paths::LaunchEnvironment;

    #[test]
    fn the_background_is_on_unless_turned_off() {
        let home = tempfile::tempdir().expect("temp dir");
        let config = home.path().join("config.toml");
        assert!(enabled(
            &LaunchEnvironment::from_pairs::<_, &str, &str>([]),
            &config
        ));
        assert!(!enabled(
            &LaunchEnvironment::from_pairs([("HA_DAEMON", "off")]),
            &config
        ));
        std::fs::write(home.path().join("settings.json"), r#"{"daemon": false}"#)
            .expect("settings");
        assert!(!enabled(
            &LaunchEnvironment::from_pairs::<_, &str, &str>([]),
            &config
        ));
        assert!(enabled(
            &LaunchEnvironment::from_pairs([("HA_DAEMON", "1")]),
            &config
        ));
    }

    #[test]
    fn idle_time_reads_short() {
        assert_eq!(idle_label(5), "5s");
        assert_eq!(idle_label(125), "2m");
        assert_eq!(idle_label(3_725), "1h2m");
    }
}
