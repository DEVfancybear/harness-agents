//! Autonomous mode, after prime-agent's `core/autonomous.ts`.
//!
//! With `/autonomous on` the session does not stop when the model does: every
//! turn that ends cleanly is followed by another, with prime-agent's
//! continuation prompt, until a budget runs out (continuations, turns, tokens,
//! time - checked in that order). Quality gates are shell commands the user
//! wrote: after each turn they run in the workspace; all passing ends the run,
//! a failure continues it with the command's output, and a gate that keeps
//! failing ends it. A gate that failed is not rerun while the worktree has not
//! changed since, as prime-agent snapshots `git status`, `git diff` and the
//! untracked files.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use sha2::{Digest, Sha256};

/// prime-agent's `DEFAULT_AUTONOMOUS_CONTINUATION_PROMPT`.
pub const CONTINUATION_PROMPT: &str = "No human input is available in autonomous mode. Continue working until the host evaluator, verifier, or configured autonomous limits stop the run. If you were asking the user a question, make a reasonable assumption and verify it. If you believe you are blocked, prove it with host-observable evidence, preserve that evidence, and keep looking for safe progress while budget remains. Do not end the session yourself; the verifier/evaluator decides completion when configured gates pass.";

/// prime-agent's usage line, without the subagent keep-alive flag `ha` lacks.
pub const USAGE: &str = "Usage: /autonomous [status|off] or /autonomous on [--max-continuations <n|unlimited>] [--max-turns <n|unlimited>] [--max-tokens <n|unlimited>] [--timeout-ms <n|unlimited>] [--gate <command>] [--gate-retries <n>] [--gate-timeout-ms <n>]";

/// prime-agent's `UNLIMITED_AUTONOMOUS_LIMIT` (`Number.MAX_SAFE_INTEGER`).
pub const UNLIMITED: u64 = 9_007_199_254_740_991;

const MAX_GATE_OUTPUT_CHARS: usize = 6000;

/// The budgets, prime-agent's `DEFAULT_AUTONOMOUS_LIMITS` by default.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_continuations: u64,
    pub max_turns: u64,
    pub max_tokens: u64,
    pub timeout_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_continuations: 3,
            max_turns: 12,
            max_tokens: 80_000,
            timeout_ms: 30 * 60 * 1000,
        }
    }
}

/// The quality gates, prime-agent's `DEFAULT_AUTONOMOUS_GATES` by default.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Gates {
    pub commands: Vec<String>,
    pub max_retries: u64,
    pub timeout_ms: u64,
}

impl Default for Gates {
    fn default() -> Self {
        Self {
            commands: Vec::new(),
            max_retries: 3,
            timeout_ms: 5 * 60 * 1000,
        }
    }
}

/// What `/autonomous on` asked for; unnamed fields keep their current value.
/// The `--autonomous*` flags of a headless run carry the same fields.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Options {
    pub max_continuations: Option<u64>,
    pub max_turns: Option<u64>,
    pub max_tokens: Option<u64>,
    pub timeout_ms: Option<u64>,
    pub gates: Option<Vec<String>>,
    pub gate_retries: Option<u64>,
    pub gate_timeout_ms: Option<u64>,
}

/// One `/autonomous` command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    Status,
    On(Options),
    Off,
}

/// Split arguments as a shell would for quoting: `"a b"` and `'a b'` are one.
fn split_args(text: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for char in text.chars() {
        match quote {
            Some(open) if char == open => quote = None,
            Some(_) => current.push(char),
            None if char == '"' || char == '\'' => {
                quote = Some(char);
                started = true;
            }
            None if char.is_whitespace() => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => {
                current.push(char);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err(format!("Unterminated quote. {USAGE}"));
    }
    if started {
        tokens.push(current);
    }
    Ok(tokens)
}

/// prime-agent's `parseAutonomousBudgetInt`.
fn budget_int(flag: &str, value: &str, allow_unlimited: bool) -> Result<u64, String> {
    if allow_unlimited && value.eq_ignore_ascii_case("unlimited") {
        return Ok(UNLIMITED);
    }
    let digits = value.replace([',', '_'], "");
    let valid = digits
        .chars()
        .next()
        .is_some_and(|first| ('1'..='9').contains(&first))
        && digits.chars().all(|char| char.is_ascii_digit());
    match digits.parse::<u64>() {
        Ok(number) if valid => Ok(number),
        _ => Err(format!(
            "--{flag} must be a positive integer{}. {USAGE}",
            if allow_unlimited {
                " or \"unlimited\""
            } else {
                ""
            }
        )),
    }
}

/// prime-agent's `_parseAutonomousSlashCommand`.
pub fn parse(argument: &str) -> Result<Command, String> {
    let tokens = split_args(argument)?;
    let Some(first) = tokens.first() else {
        return Ok(Command::Status);
    };
    let subcommand = first.to_ascii_lowercase();
    match subcommand.as_str() {
        "status" | "off" | "disable" | "disabled" => {
            if let Some(extra) = tokens.get(1) {
                return Err(format!("Unexpected autonomous argument: {extra}. {USAGE}"));
            }
            Ok(if subcommand == "status" {
                Command::Status
            } else {
                Command::Off
            })
        }
        "on" | "enable" | "enabled" => parse_options(&tokens[1..]).map(Command::On),
        _ => Err(USAGE.to_owned()),
    }
}

/// prime-agent's `parseAutonomousBudgetOptions`.
fn parse_options(tokens: &[String]) -> Result<Options, String> {
    const FLAGS: [&str; 7] = [
        "max-continuations",
        "max-turns",
        "max-tokens",
        "timeout-ms",
        "gate",
        "gate-retries",
        "gate-timeout-ms",
    ];
    let mut options = Options::default();
    let mut gates = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        if !token.starts_with("--") {
            return Err(format!("Unexpected autonomous argument: {token}. {USAGE}"));
        }
        let (raw_flag, inline) = match token.split_once('=') {
            Some((flag, value)) => (flag, Some(value.to_owned())),
            None => (token.as_str(), None),
        };
        let flag = raw_flag
            .strip_prefix("--autonomous-")
            .unwrap_or(&raw_flag[2..]);
        if !FLAGS.contains(&flag) {
            return Err(format!(
                "Unknown autonomous budget flag: {raw_flag}. {USAGE}"
            ));
        }
        let value = if let Some(value) = inline {
            value
        } else {
            let next = tokens.get(index + 1).filter(|next| !next.starts_with("--"));
            let Some(next) = next else {
                return Err(format!("Missing value for {raw_flag}. {USAGE}"));
            };
            index += 1;
            next.clone()
        };
        if value.is_empty() {
            return Err(format!("Missing value for {raw_flag}. {USAGE}"));
        }
        match flag {
            "gate" => gates.push(value),
            "gate-retries" => options.gate_retries = Some(budget_int(flag, &value, false)?),
            "gate-timeout-ms" => options.gate_timeout_ms = Some(budget_int(flag, &value, false)?),
            "max-continuations" => {
                options.max_continuations = Some(budget_int(flag, &value, true)?);
            }
            "max-turns" => options.max_turns = Some(budget_int(flag, &value, true)?),
            "max-tokens" => options.max_tokens = Some(budget_int(flag, &value, true)?),
            _ => options.timeout_ms = Some(budget_int(flag, &value, true)?),
        }
        index += 1;
    }
    if !gates.is_empty() {
        options.gates = Some(gates);
    }
    // Named budget flags define the whole budget: a limit the user did not name
    // stops cutting the run short. With none named, the defaults still apply.
    if options.max_continuations.is_some()
        || options.max_turns.is_some()
        || options.max_tokens.is_some()
        || options.timeout_ms.is_some()
    {
        options.max_continuations.get_or_insert(UNLIMITED);
        options.max_turns.get_or_insert(UNLIMITED);
        options.max_tokens.get_or_insert(UNLIMITED);
        options.timeout_ms.get_or_insert(UNLIMITED);
    }
    Ok(options)
}

/// The last gate that failed, prime-agent's `AgentAutonomousGateFailure`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GateFailure {
    pub command: String,
    pub attempt: u64,
    pub exit_text: String,
    pub output: String,
}

/// The worktree as git sees it, so an unchanged one is not checked again.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    status: String,
    diff: String,
    untracked: String,
}

/// What the gate runs remember between turns.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GateState {
    pub attempts: BTreeMap<String, u64>,
    pub last_failure: Option<GateFailure>,
    pub last_snapshot: Option<Snapshot>,
}

/// prime-agent's `AutonomousGateResult`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateResult {
    Passed,
    Failed,
    RetryExhausted,
}

/// One run of the gates, handed to the service and back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GateJob {
    pub commands: Vec<String>,
    pub max_retries: u64,
    pub timeout_ms: u64,
    pub state: GateState,
}

/// Why autonomous mode stopped carrying the run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stop {
    /// Off, or the turn failed or was stopped: nothing to carry.
    NotNeeded,
    /// Every gate passed.
    GatesPassed,
    /// A gate failed more times than `--gate-retries` allows.
    GateRetriesExhausted,
    /// A budget ran out: prime-agent's `AutonomousLimitReason`.
    Limit(&'static str),
}

/// What happens after a turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Decision {
    Stop(Stop),
    Continue(String),
}

/// prime-agent's `AutonomousRuntimeState`.
#[derive(Clone, Debug, Default)]
pub struct Autonomous {
    pub enabled: bool,
    pub continuations_used: u64,
    pub turns_used: u64,
    pub tokens_used: u64,
    started_at: Option<Instant>,
    pub limits: Limits,
    pub gates: Gates,
    pub gate_state: GateState,
}

impl Autonomous {
    /// `setAutonomousEnabled(true)` then `setAutonomousLimits`.
    pub fn turn_on(&mut self, options: &Options, now: Instant) {
        self.enabled = true;
        self.continuations_used = 0;
        self.turns_used = 0;
        self.tokens_used = 0;
        self.started_at = Some(now);
        self.gate_state = GateState::default();
        let limit =
            |value: Option<u64>, current: u64| value.filter(|value| *value > 0).unwrap_or(current);
        self.limits.max_continuations =
            limit(options.max_continuations, self.limits.max_continuations);
        self.limits.max_turns = limit(options.max_turns, self.limits.max_turns);
        self.limits.max_tokens = limit(options.max_tokens, self.limits.max_tokens);
        self.limits.timeout_ms = limit(options.timeout_ms, self.limits.timeout_ms);
        if let Some(commands) = &options.gates {
            self.gates.commands.clone_from(commands);
        }
        self.gates.max_retries = limit(options.gate_retries, self.gates.max_retries);
        self.gates.timeout_ms = limit(options.gate_timeout_ms, self.gates.timeout_ms);
    }

    /// `setAutonomousEnabled(false)`.
    pub fn turn_off(&mut self) {
        self.enabled = false;
        self.started_at = None;
        self.gate_state = GateState::default();
    }

    /// `addAutonomousUsage`: one turn and its tokens.
    pub fn record_turn(&mut self, tokens: u64) {
        if self.enabled {
            self.turns_used += 1;
            self.tokens_used = self.tokens_used.saturating_add(tokens);
        }
    }

    /// `autonomousLimitReason`.
    #[must_use]
    pub fn limit_reason(&self, now: Instant) -> Option<&'static str> {
        if self.continuations_used >= self.limits.max_continuations {
            return Some("maxContinuations");
        }
        if self.turns_used >= self.limits.max_turns {
            return Some("maxTurns");
        }
        if self.tokens_used >= self.limits.max_tokens {
            return Some("maxTokens");
        }
        let elapsed = self.started_at.map_or(0, |started| {
            u64::try_from(now.saturating_duration_since(started).as_millis()).unwrap_or(u64::MAX)
        });
        if self.started_at.is_some() && elapsed >= self.limits.timeout_ms {
            return Some("timeoutMs");
        }
        None
    }

    /// The gates to run after this turn, when there are any.
    #[must_use]
    pub fn gate_job(&self) -> Option<GateJob> {
        (self.enabled && !self.gates.commands.is_empty()).then(|| GateJob {
            commands: self.gates.commands.clone(),
            max_retries: self.gates.max_retries,
            timeout_ms: self.gates.timeout_ms,
            state: self.gate_state.clone(),
        })
    }

    /// `shouldAutonomouslyContinue` and `nextAutonomousContinuation`, after the
    /// gates (if any) ran. `clean` is false for a turn that failed or was stopped.
    pub fn decide(
        &mut self,
        clean: bool,
        gates: Option<(GateResult, GateState)>,
        now: Instant,
        timestamp: &str,
    ) -> Decision {
        if !self.enabled || !clean {
            return Decision::Stop(Stop::NotNeeded);
        }
        if let Some((result, state)) = gates {
            self.gate_state = state;
            match result {
                GateResult::Passed => return Decision::Stop(Stop::GatesPassed),
                GateResult::RetryExhausted => return Decision::Stop(Stop::GateRetriesExhausted),
                GateResult::Failed => {
                    if let Some(reason) = self.limit_reason(now) {
                        return Decision::Stop(Stop::Limit(reason));
                    }
                    self.continuations_used += 1;
                    let text = self.gate_state.last_failure.as_ref().map_or_else(
                        || format!("[autonomous-continuation]\n\n{CONTINUATION_PROMPT}"),
                        |failure| {
                            gate_failure_continuation(failure, self.gates.max_retries, timestamp)
                        },
                    );
                    return Decision::Continue(text);
                }
            }
        }
        if let Some(reason) = self.limit_reason(now) {
            return Decision::Stop(Stop::Limit(reason));
        }
        self.continuations_used += 1;
        Decision::Continue(format!(
            "[autonomous-continuation]\n\n{CONTINUATION_PROMPT}"
        ))
    }

    /// prime-agent's `describe_autonomous_limit`.
    #[must_use]
    pub fn describe_limit(&self, reason: &str, now: Instant) -> String {
        match reason {
            "maxContinuations" => format!(
                "maxContinuations reached ({}/{})",
                self.continuations_used, self.limits.max_continuations
            ),
            "maxTurns" => format!(
                "maxTurns reached ({}/{})",
                self.turns_used, self.limits.max_turns
            ),
            "maxTokens" => format!(
                "maxTokens reached ({}/{})",
                self.tokens_used, self.limits.max_tokens
            ),
            _ => {
                let elapsed = self.started_at.map_or(0, |started| {
                    u64::try_from(now.saturating_duration_since(started).as_millis())
                        .unwrap_or(u64::MAX)
                });
                format!("timeoutMs reached ({elapsed}/{})", self.limits.timeout_ms)
            }
        }
    }

    /// prime-agent's headless exit contract (`HeadlessAutonomous::exit_stderr`):
    /// the stderr line of a run that must exit non-zero - a configured gate
    /// still failing, or a run without gates that a limit stopped.
    #[must_use]
    pub fn exit_stderr(&self, now: Instant) -> Option<String> {
        let limit = self.limit_reason(now);
        if self.enabled
            && !self.gates.commands.is_empty()
            && let Some(failure) = &self.gate_state.last_failure
        {
            let attempt = self
                .gate_state
                .attempts
                .values()
                .copied()
                .chain([failure.attempt])
                .max()
                .unwrap_or(0);
            let limit_text = limit
                .map(|reason| {
                    format!(
                        "; autonomous limit reached: {}",
                        self.describe_limit(reason, now)
                    )
                })
                .unwrap_or_default();
            return Some(format!(
                "Autonomous quality gate still failing after attempt {attempt}/{}: {}{limit_text}",
                self.gates.max_retries, failure.exit_text
            ));
        }
        if self.enabled
            && self.gates.commands.is_empty()
            && let Some(reason) = limit
        {
            return Some(format!(
                "Autonomous run stopped before terminal evidence; {}",
                self.describe_limit(reason, now)
            ));
        }
        None
    }

    /// prime-agent's `_formatAutonomousStatus`, without the keep-alive clause.
    #[must_use]
    pub fn status(&self, now: Instant) -> String {
        // `Math.round` of seconds, in whole milliseconds.
        let elapsed = self.started_at.map_or(0, |started| {
            let millis = u64::try_from(now.saturating_duration_since(started).as_millis())
                .unwrap_or(u64::MAX);
            millis.saturating_add(500) / 1000
        });
        let time_budget = if self.limits.timeout_ms >= UNLIMITED {
            "unlimited".to_owned()
        } else {
            format!(
                "{}s",
                count(self.limits.timeout_ms.saturating_add(500) / 1000)
            )
        };
        let gates = if self.gates.commands.is_empty() {
            "none".to_owned()
        } else {
            self.gates
                .commands
                .iter()
                .map(|command| format!("\"{command}\""))
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "[autonomous-status: {}]\n\nContinuations: {}/{}. Turns: {}/{}. Tokens: {}/{}. Time: {elapsed}s/{time_budget}. Gates: {gates}.",
            if self.enabled { "on" } else { "off" },
            count(self.continuations_used),
            count(self.limits.max_continuations),
            count(self.turns_used),
            count(self.limits.max_turns),
            count(self.tokens_used),
            count(self.limits.max_tokens),
        )
    }
}

/// A count as prime-agent prints it: `en-US` grouping, or `unlimited`.
fn count(value: u64) -> String {
    if value >= UNLIMITED {
        return "unlimited".to_owned();
    }
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// prime-agent's `buildAutonomousGateFailureContinuation`.
#[must_use]
pub fn gate_failure_continuation(
    failure: &GateFailure,
    max_retries: u64,
    timestamp: &str,
) -> String {
    let mut text = format!(
        "[autonomous-continuation: gate-failed]\n\nAutonomous quality gate failed (attempt {}/{max_retries}): `{}` {}.\n",
        failure.attempt, failure.command, failure.exit_text
    );
    if failure.output.is_empty() {
        text.push('\n');
    } else {
        let _ = write!(text, "\nOutput:\n{}\n", failure.output);
    }
    let _ = write!(
        text,
        "\nContinue working. Fix the failure, then produce terminal evidence. Timestamp: {timestamp}."
    );
    text
}

/// prime-agent's `truncateGateOutput`.
fn truncate_output(output: &str, already_truncated: bool) -> String {
    if output.chars().count() <= MAX_GATE_OUTPUT_CHARS && !already_truncated {
        return output.to_owned();
    }
    let kept = output
        .chars()
        .take(MAX_GATE_OUTPUT_CHARS)
        .collect::<String>();
    format!("{kept}\n... [truncated]")
}

/// The paths prime-agent leaves out of the snapshot: build output and the
/// files its own verifier writes.
const PATHSPEC: [&str; 8] = [
    "--",
    ".",
    ":(exclude)verification",
    ":(exclude)target",
    ":(exclude).vf-prime-agent",
    ":(exclude)Cargo.lock",
    ":(exclude)submission.tar.gz",
    ":(exclude)runner_args.log",
];

async fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new("git")
            .args(args)
            .args(PATHSPEC)
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// prime-agent's `captureGitWorktreeSnapshot`.
pub(crate) async fn snapshot(root: &Path) -> Option<Snapshot> {
    let status = git(
        root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "-uall",
            "--no-renames",
        ],
    )
    .await?;
    let diff = git(
        root,
        &[
            "--no-optional-locks",
            "diff",
            "--no-ext-diff",
            "--binary",
            "HEAD",
        ],
    )
    .await?;
    let mut untracked = status
        .split('\0')
        .filter_map(|entry| entry.strip_prefix("?? "))
        .collect::<Vec<_>>();
    untracked.sort_unstable();
    let mut aggregate = Sha256::new();
    for path in untracked {
        aggregate.update(path.as_bytes());
        aggregate.update([0]);
        let hash = match std::fs::read(root.join(path)) {
            Ok(bytes) => format!("file:{}", hex(&Sha256::digest(&bytes))),
            Err(error) => format!("error:{error}"),
        };
        aggregate.update(hash.as_bytes());
        aggregate.update([0]);
    }
    Some(Snapshot {
        status,
        diff,
        untracked: hex(&aggregate.finalize()),
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// prime-agent's `runAutonomousQualityGates`: each gate in order, stopping at
/// the first failure.
pub async fn run_gates(
    root: &Path,
    job: GateJob,
    cancellation: harness_providers::CancellationToken,
) -> (GateResult, GateState) {
    let mut state = job.state;
    let verdict = |attempt: u64| {
        if attempt > job.max_retries {
            GateResult::RetryExhausted
        } else {
            GateResult::Failed
        }
    };
    for command in &job.commands {
        let current = snapshot(root).await;
        if let Some(failure) = state.last_failure.clone()
            && failure.command == *command
            && current.is_some()
            && current == state.last_snapshot
        {
            let attempt = state
                .attempts
                .get(command)
                .copied()
                .unwrap_or(failure.attempt)
                + 1;
            state.attempts.insert(command.clone(), attempt);
            state.last_failure = Some(GateFailure {
                attempt,
                exit_text: "not rerun: workspace unchanged since previous failed gate".to_owned(),
                output: "The autonomous gate was not rerun because the workspace has not changed since this failure. Edit source files, tests, or a blocker artifact before attempting to finish again.".to_owned(),
                ..failure
            });
            return (verdict(attempt), state);
        }
        let result =
            harness_tools::run_user_command(root, command, job.timeout_ms, cancellation.clone())
                .await;
        let after = snapshot(root).await;
        let (exit_text, output) = match &result {
            Ok(done)
                if done.status == harness_tools::HookProcessStatus::Exited
                    && done.exit_code == Some(0) =>
            {
                state.attempts.insert(command.clone(), 0);
                if state
                    .last_failure
                    .as_ref()
                    .is_some_and(|failure| failure.command == *command)
                {
                    state.last_failure = None;
                    state.last_snapshot = None;
                }
                continue;
            }
            Ok(done) => (
                match done.status {
                    harness_tools::HookProcessStatus::TimedOut => "timed out".to_owned(),
                    harness_tools::HookProcessStatus::Canceled => "canceled".to_owned(),
                    harness_tools::HookProcessStatus::Exited => done.exit_code.map_or_else(
                        || "exited unknown".to_owned(),
                        |code| format!("exited {code}"),
                    ),
                },
                truncate_output(
                    [done.stdout.as_str(), done.stderr.as_str()]
                        .iter()
                        .filter(|part| !part.is_empty())
                        .copied()
                        .collect::<Vec<_>>()
                        .join("\n")
                        .trim(),
                    done.stdout_truncated || done.stderr_truncated,
                ),
            ),
            Err(error) => (error.to_string(), String::new()),
        };
        let attempt = state.attempts.get(command).copied().unwrap_or(0) + 1;
        state.attempts.insert(command.clone(), attempt);
        state.last_failure = Some(GateFailure {
            command: command.clone(),
            attempt,
            exit_text,
            output,
        });
        state.last_snapshot = after;
        return (verdict(attempt), state);
    }
    state.last_failure = None;
    state.last_snapshot = None;
    (GateResult::Passed, state)
}

#[cfg(test)]
mod tests {
    use super::{
        Autonomous, Command, Decision, GateFailure, GateResult, GateState, Options, Stop,
        UNLIMITED, gate_failure_continuation, parse,
    };
    use std::time::{Duration, Instant};

    fn on(options: &Options) -> (Autonomous, Instant) {
        let now = Instant::now();
        let mut state = Autonomous::default();
        state.turn_on(options, now);
        (state, now)
    }

    #[test]
    fn the_headless_exit_contract_reports_a_failing_gate_and_a_spent_budget() {
        let (mut state, now) = on(&Options {
            gates: Some(vec!["cargo test".into()]),
            ..Options::default()
        });
        assert_eq!(state.exit_stderr(now), None);
        state.gate_state.last_failure = Some(GateFailure {
            command: "cargo test".into(),
            attempt: 2,
            exit_text: "exited with code 101".into(),
            output: String::new(),
        });
        assert_eq!(
            state.exit_stderr(now).as_deref(),
            Some("Autonomous quality gate still failing after attempt 2/3: exited with code 101")
        );
        let (mut ungated, now) = on(&Options::default());
        ungated.turns_used = 12;
        assert_eq!(
            ungated.exit_stderr(now).as_deref(),
            Some("Autonomous run stopped before terminal evidence; maxTurns reached (12/12)")
        );
    }

    #[test]
    fn q14_the_command_parses_like_prime() {
        assert_eq!(parse("").unwrap(), Command::Status);
        assert_eq!(parse("status").unwrap(), Command::Status);
        assert_eq!(parse("disable").unwrap(), Command::Off);
        assert_eq!(parse("on").unwrap(), Command::On(Options::default()));
        let Command::On(options) =
            parse(r#"on --max-turns 5 --gate "cargo test -p x" --gate=true --gate-retries 2"#)
                .unwrap()
        else {
            panic!("on");
        };
        assert_eq!(options.max_turns, Some(5));
        // Naming one budget makes the others unlimited, as prime-agent does.
        assert_eq!(options.max_continuations, Some(UNLIMITED));
        assert_eq!(options.max_tokens, Some(UNLIMITED));
        assert_eq!(
            options.gates.as_deref(),
            Some(&["cargo test -p x".to_owned(), "true".to_owned()][..])
        );
        assert_eq!(options.gate_retries, Some(2));
        let Command::On(options) = parse("on --autonomous-max-tokens 100,000").unwrap() else {
            panic!("on");
        };
        assert_eq!(options.max_tokens, Some(100_000));
        assert!(
            parse("on --max-turns 0")
                .unwrap_err()
                .starts_with("--max-turns must be a positive integer or \"unlimited\"")
        );
        assert!(
            parse("on --bogus 1")
                .unwrap_err()
                .starts_with("Unknown autonomous budget flag: --bogus")
        );
        assert!(
            parse("on --gate")
                .unwrap_err()
                .starts_with("Missing value for --gate")
        );
        assert!(
            parse("status now")
                .unwrap_err()
                .starts_with("Unexpected autonomous argument: now")
        );
        assert!(
            parse("maybe")
                .unwrap_err()
                .starts_with("Usage: /autonomous")
        );
    }

    #[test]
    fn q14_decide_follows_prime_order() {
        let stamp = "2026-09-28T00:00:00.000Z";
        // Off, or a turn that failed: nothing continues.
        let mut off = Autonomous::default();
        assert_eq!(
            off.decide(true, None, Instant::now(), stamp),
            Decision::Stop(Stop::NotNeeded)
        );
        let (mut state, now) = on(&Options::default());
        assert_eq!(
            state.decide(false, None, now, stamp),
            Decision::Stop(Stop::NotNeeded)
        );
        // No gates: continue until a limit, continuations first.
        for _ in 0..3 {
            let Decision::Continue(text) = state.decide(true, None, now, stamp) else {
                panic!("continues");
            };
            assert!(text.starts_with("[autonomous-continuation]\n\nNo human input is available"));
        }
        assert_eq!(
            state.decide(true, None, now, stamp),
            Decision::Stop(Stop::Limit("maxContinuations"))
        );
        // Then turns, tokens, time.
        let (mut state, now) = on(&Options::default());
        state.turns_used = 12;
        assert_eq!(
            state.decide(true, None, now, stamp),
            Decision::Stop(Stop::Limit("maxTurns"))
        );
        let (mut state, now) = on(&Options::default());
        state.record_turn(80_000);
        assert_eq!(
            state.decide(true, None, now, stamp),
            Decision::Stop(Stop::Limit("maxTokens"))
        );
        let (mut state, now) = on(&Options::default());
        let later = now + Duration::from_mins(31);
        assert_eq!(
            state.decide(true, None, later, stamp),
            Decision::Stop(Stop::Limit("timeoutMs"))
        );
        // Gates: passing ends the run, failing continues with the failure,
        // too many failures end it, and a limit still wins over a failure.
        let (mut state, now) = on(&Options::default());
        assert_eq!(
            state.decide(
                true,
                Some((GateResult::Passed, GateState::default())),
                now,
                stamp
            ),
            Decision::Stop(Stop::GatesPassed)
        );
        let failed = GateState {
            last_failure: Some(GateFailure {
                command: "cargo test".to_owned(),
                attempt: 1,
                exit_text: "exited 101".to_owned(),
                output: "1 failed".to_owned(),
            }),
            ..GateState::default()
        };
        let Decision::Continue(text) =
            state.decide(true, Some((GateResult::Failed, failed.clone())), now, stamp)
        else {
            panic!("a failed gate continues");
        };
        assert!(text.starts_with("[autonomous-continuation: gate-failed]"));
        assert_eq!(
            state.decide(
                true,
                Some((GateResult::RetryExhausted, failed.clone())),
                now,
                stamp
            ),
            Decision::Stop(Stop::GateRetriesExhausted)
        );
        state.continuations_used = 3;
        assert_eq!(
            state.decide(true, Some((GateResult::Failed, failed)), now, stamp),
            Decision::Stop(Stop::Limit("maxContinuations"))
        );
    }

    #[test]
    fn q14_gate_failure_prompt_matches_prime() {
        let failure = GateFailure {
            command: "cargo test".to_owned(),
            attempt: 2,
            exit_text: "exited 101".to_owned(),
            output: "test a ... FAILED".to_owned(),
        };
        assert_eq!(
            gate_failure_continuation(&failure, 3, "2026-09-28T01:02:03.004Z"),
            "[autonomous-continuation: gate-failed]\n\nAutonomous quality gate failed (attempt 2/3): `cargo test` exited 101.\n\nOutput:\ntest a ... FAILED\n\nContinue working. Fix the failure, then produce terminal evidence. Timestamp: 2026-09-28T01:02:03.004Z."
        );
        let quiet = GateFailure {
            output: String::new(),
            ..failure
        };
        assert!(
            gate_failure_continuation(&quiet, 3, "t")
                .contains("exited 101.\n\n\nContinue working.")
        );
    }

    #[test]
    fn q14_status_reads_like_prime() {
        let (state, now) = on(&Options {
            gates: Some(vec!["cargo test".to_owned()]),
            ..Options::default()
        });
        assert_eq!(
            state.status(now),
            "[autonomous-status: on]\n\nContinuations: 0/3. Turns: 0/12. Tokens: 0/80,000. Time: 0s/1,800s. Gates: \"cargo test\"."
        );
        let (state, now) = on(&Options {
            max_turns: Some(4),
            max_continuations: Some(UNLIMITED),
            max_tokens: Some(UNLIMITED),
            timeout_ms: Some(UNLIMITED),
            ..Options::default()
        });
        assert!(state.status(now).contains("Continuations: 0/unlimited. Turns: 0/4. Tokens: 0/unlimited. Time: 0s/unlimited. Gates: none."));
    }

    #[tokio::test]
    async fn q14_unchanged_worktree_skips_the_gate() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .expect("git runs")
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "base",
        ]);
        // The gate counts its own runs, in a file git is told to ignore.
        std::fs::write(root.join(".gitignore"), "runs.txt\n").expect("ignore");
        git(&["add", ".gitignore"]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "ignore",
        ]);
        #[cfg(windows)]
        let gate = "Add-Content runs.txt x; if (Test-Path ok.txt) { exit 0 } else { exit 3 }";
        #[cfg(not(windows))]
        let gate = "echo x >> runs.txt; test -f ok.txt || exit 3";
        let runs = || {
            std::fs::read_to_string(root.join("runs.txt"))
                .unwrap_or_default()
                .lines()
                .count()
        };
        let job = |state: GateState| super::GateJob {
            commands: vec![gate.to_owned()],
            max_retries: 3,
            timeout_ms: 60_000,
            state,
        };
        let token = harness_providers::CancellationToken::new;
        let (result, state) = super::run_gates(root, job(GateState::default()), token()).await;
        assert_eq!(result, GateResult::Failed);
        assert_eq!(state.last_failure.as_ref().unwrap().exit_text, "exited 3");
        assert_eq!(runs(), 1);
        // Nothing changed: counted as an attempt, not run again.
        let (result, state) = super::run_gates(root, job(state), token()).await;
        assert_eq!(result, GateResult::Failed);
        let failure = state.last_failure.clone().unwrap();
        assert_eq!(failure.attempt, 2);
        assert_eq!(
            failure.exit_text,
            "not rerun: workspace unchanged since previous failed gate"
        );
        assert_eq!(runs(), 1, "the gate did not run");
        // The fix changes the worktree, so the gate runs and passes.
        std::fs::write(root.join("ok.txt"), "ok").expect("fix");
        let (result, state) = super::run_gates(root, job(state), token()).await;
        assert_eq!(result, GateResult::Passed);
        assert!(state.last_failure.is_none());
        assert_eq!(runs(), 2);
    }
}
