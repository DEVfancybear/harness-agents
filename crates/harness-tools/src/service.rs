use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use harness_providers::CancellationToken;
use harness_store_sqlite::{
    PublishedArtifact, SourceWorkMarker, SqliteStore, StoreError, ToolApprovalBinding,
    ToolApprovalRecord, ToolApprovalState, ToolIntentCommit, ToolIntentRecord, ToolIntentStatus,
    ToolSettlementCommit, ToolTaskUpdateCommit,
};
use harness_types::{
    ContentHash, ErrorCode, EventEnvelope, EventId, HarnessError, NextActionProposal,
    P0_SCHEMA_VERSION, PendingToolCall, PendingToolState, ProducerIdentity, SourceAuthority,
    StorePort, TaskId, ToolExecutionId, ToolExecutionReceipt, ToolIntentState, ToolOutcomeState,
    WorkingState,
};
use serde_json::{Map, Value, json};

use crate::{
    ApprovalGrant, CaptureStream, CodingToolAction, Decision, GIT_LOG_DEFAULT_LIMIT,
    HISTORY_READ_MAX_BYTES, HISTORY_SEARCH_DEFAULT_LIMIT, IsolationMode,
    PROCESS_OUTPUT_PAGE_MAX_BYTES, PreparedToolRequest, TOOL_CONTRACT_VERSION, ToolCapabilities,
    ToolExecutionView, ToolOutput, ToolPolicy, ToolRequest,
    capture::{self, StreamTail},
    coding_tool_names,
    process::{self, ProcessResult, TreeCleanup},
    secrets::{HostEnvironmentSecrets, ProcessEnvironment, SecretResolver},
    workspace::{
        apply_text_patch, edit_text, glob_files, inspect_workspace, inspect_workspace_for_read,
        list_files, plan_edit_text, read_file_range, read_text, redact_text, resolve_relative,
        search_text, validate_glob, validate_search, write_text_checked,
    },
};
use harness_store_sqlite::HistoryScope;

fn hook_matches(matcher: Option<&str>, tool_name: &str) -> bool {
    matcher.is_none_or(|matcher| {
        matcher
            .split('|')
            .map(str::trim)
            .any(|pattern| pattern == "*" || pattern == tool_name)
    })
}

fn tool_hook_payload(prepared: &PreparedToolRequest, event: &str) -> Value {
    let mut args = prepared
        .final_action
        .canonical_value()
        .unwrap_or_else(|_| json!({"unavailable": true}));
    redact_hook_arguments(&mut args);
    let mut payload = json!({
        "event": event,
        "session_id": prepared.request.session_id.as_str(),
        "task_id": prepared.request.task_id.as_str(),
        "tool": {
            "name": prepared.final_action.kind().as_str(),
            "args_digest": prepared.action_hash.as_str(),
            "args": args,
        },
        "cwd": prepared.workspace_root_text,
    });
    if serde_json::to_vec(&payload).is_ok_and(|bytes| bytes.len() <= 8 * 1024) {
        return payload;
    }
    let args = payload["tool"]["args"].to_string();
    let mut preview = args.chars().take(5_000).collect::<String>();
    payload["tool"]["args"] = json!({"truncated": true, "preview": preview});
    while serde_json::to_vec(&payload).is_ok_and(|bytes| bytes.len() > 8 * 1024) {
        if preview.is_empty() {
            payload["tool"]["args"] = json!({"truncated": true});
            break;
        }
        preview = preview
            .chars()
            .take(preview.chars().count().saturating_sub(256))
            .collect();
        payload["tool"]["args"] = json!({"truncated": true, "preview": preview});
    }
    payload
}

fn redact_hook_arguments(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                let normalized = key.to_ascii_lowercase();
                if [
                    "secret",
                    "token",
                    "password",
                    "api_key",
                    "credential",
                    "authorization",
                ]
                .iter()
                .any(|needle| normalized.contains(needle))
                {
                    *child = Value::String("[REDACTED]".to_owned());
                } else {
                    redact_hook_arguments(child);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact_hook_arguments),
        _ => {}
    }
}

async fn run_configured_hook(
    hook: &ConfiguredToolHook,
    mut payload: Value,
    cancellation: CancellationToken,
) -> Result<(), String> {
    payload["event"] = Value::String(hook.event.clone());
    let input =
        serde_json::to_vec(&payload).map_err(|_| "hook input is invalid JSON".to_owned())?;
    let cwd = payload["cwd"]
        .as_str()
        .map_or_else(|| Path::new("."), Path::new);
    let result = process::run_hook_command(
        cwd,
        &hook.command,
        &hook.args,
        &input,
        hook.timeout_seconds.min(60).saturating_mul(1000),
        cancellation,
    )
    .await
    .map_err(|error| error.to_string())?;
    if result.status == process::HookProcessStatus::TimedOut {
        return Err("hook timed out".to_owned());
    }
    if result.status == process::HookProcessStatus::Canceled {
        return Err("hook was canceled".to_owned());
    }
    if result.exit_code == Some(0) {
        return Ok(());
    }
    let reason = if result.exit_code == Some(2) {
        result
            .stdout
            .trim()
            .lines()
            .next()
            .or_else(|| result.stderr.trim().lines().next())
            .filter(|line| !line.is_empty())
            .map_or_else(
                || "hook exited with status 2".to_owned(),
                |line| line.chars().take(512).collect::<String>(),
            )
    } else {
        format!("hook exited with status {:?}", result.exit_code)
    };
    Err(reason)
}

/// Read the current repository commit through the bounded Git process runner.
/// A missing or uncommitted HEAD is represented as `None`.
pub async fn git_head_commit(root: &Path) -> Result<Option<String>, HarnessError> {
    let output = process::run_structured(
        root,
        "git",
        &["rev-parse".to_owned(), "HEAD".to_owned()],
        15_000,
        CancellationToken::new(),
        &ProcessEnvironment::empty(),
        &crate::capture::ProcessSpoolConfig::default(),
    )
    .await?;
    if output.timed_out || output.canceled || output.exit_code != Some(0) {
        return Ok(None);
    }
    let commit = output.stdout.trim();
    let valid =
        matches!(commit.len(), 40 | 64) && commit.bytes().all(|byte| byte.is_ascii_hexdigit());
    Ok(valid.then(|| commit.to_owned()))
}

/// Show all tracked changes since a recorded commit through the built-in Git
/// process runner. Both staged and unstaged changes are included.
pub async fn git_diff_from(root: &Path, base_commit: &str) -> Result<String, HarnessError> {
    let valid = matches!(base_commit.len(), 40 | 64)
        && base_commit.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !valid {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "stored Git base is not a full commit id",
        ));
    }
    let output = process::run_structured(
        root,
        "git",
        &[
            "diff".to_owned(),
            "--no-ext-diff".to_owned(),
            base_commit.to_owned(),
            "--".to_owned(),
        ],
        15_000,
        CancellationToken::new(),
        &ProcessEnvironment::empty(),
        &crate::capture::ProcessSpoolConfig::default(),
    )
    .await?;
    if output.timed_out || output.canceled || output.exit_code != Some(0) {
        return Err(HarnessError::new(
            ErrorCode::ProcessOutcomeUnknown,
            "Git diff did not complete successfully",
        ));
    }
    Ok(output.stdout)
}

/// Dispatches an `ExternalTool` action after the gate has authorized it and the
/// durable intent is committed. A returned error is treated exactly like any
/// other dispatch failure: the outcome becomes uncertain, never a success.
pub trait ExternalToolDispatcher: Send + Sync {
    /// Validate the target and arguments without performing a remote call.
    /// Implementations must reject anything they can prove will fail before
    /// dispatch so an approval is never requested for a locally invalid call.
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a serde_json::Value,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), harness_types::HarnessError>> + Send + 'a>,
    >;

    fn dispatch_external<'a>(
        &'a self,
        authorization: &'a ToolDispatchAuthorization,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a serde_json::Value,
        timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<ToolOutput, harness_types::HarnessError>>
                + Send
                + 'a,
        >,
    >;
}

/// Unforgeable proof that the shared tool gate committed this invocation's
/// approval and intent before asking an external dispatcher to perform it.
/// Only `ToolExecutionService` can construct this value.
pub struct ToolDispatchAuthorization {
    _sealed: (),
}

impl ToolDispatchAuthorization {
    const fn after_gate_commit() -> Self {
        Self { _sealed: () }
    }
}

/// An optional presentation observer. It is deliberately invoked only after a
/// receipt commit; an error or panic can alter neither receipt nor outcome.
pub trait ToolObserver: Send + Sync {
    fn observe(&self, view: &ToolExecutionView) -> Result<(), String>;
}

/// A trusted, bounded host hook. `command` is an executable, never shell text;
/// `args` are passed as individual arguments to the existing process runner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfiguredToolHook {
    pub event: String,
    pub matcher: Option<String>,
    pub command: String,
    pub args: Vec<String>,
    pub timeout_seconds: u64,
    pub source: String,
}

/// The one P3 authority permitted to move a coding proposal across a side
/// effect. Direct helpers in sibling modules are crate-private.
#[derive(Clone)]
pub struct ToolExecutionService {
    store: Arc<SqliteStore>,
    policy: ToolPolicy,
    observer: Option<Arc<dyn ToolObserver>>,
    external: Option<Arc<dyn ExternalToolDispatcher>>,
    secrets: Arc<dyn SecretResolver>,
    spool: crate::capture::ProcessSpoolConfig,
    hooks: Vec<ConfiguredToolHook>,
    /// What this host has actually been measured to enforce (M12).
    ///
    /// It is `None` until a caller supplies a measured matrix, and `None` means
    /// "refuse": a strict request is never served against capabilities nobody
    /// measured.
    capabilities: Option<Arc<crate::CapabilityMatrix>>,
}

impl ToolExecutionService {
    #[must_use]
    pub fn new(store: Arc<SqliteStore>) -> Self {
        Self {
            store,
            policy: ToolPolicy::default(),
            observer: None,
            external: None,
            secrets: Arc::new(HostEnvironmentSecrets),
            spool: crate::capture::ProcessSpoolConfig::default(),
            hooks: Vec::new(),
            capabilities: None,
        }
    }

    /// Supply the capability matrix measured on this host (M12).
    ///
    /// The matrix is a measurement, so it is passed in rather than guessed: a
    /// service that was never handed one refuses every strict request, and a
    /// service handed a matrix refuses exactly the capabilities that matrix
    /// does not report as enforced.
    #[must_use]
    pub fn with_capability_matrix(mut self, matrix: Arc<crate::CapabilityMatrix>) -> Self {
        self.capabilities = Some(matrix);
        self
    }

    /// What this host has been measured to enforce, when it has been measured.
    #[must_use]
    pub fn capability_matrix(&self) -> Option<&Arc<crate::CapabilityMatrix>> {
        self.capabilities.as_ref()
    }

    /// The reason a strict action is refused here, in terms of what was
    /// measured rather than in terms of what was assumed.
    fn strict_refusal_reason(&self) -> String {
        match &self.capabilities {
            Some(matrix) => match crate::StrictProfile::Full.refusal(matrix) {
                Some(refusal) => refusal.message().to_owned(),
                // A matrix that claims full confinement still meets a revision
                // with no confinement adapter. Refusing is the only honest
                // answer; running it as containment would be a silent
                // downgrade wearing a green verdict.
                None => format!(
                    "the measured capability matrix for this host claims full confinement, but this revision implements no confinement adapter; refusing rather than executing the request as containment ({})",
                    matrix.summary()
                ),
            },
            None => "strict isolation requires confinement capabilities that have not been measured on this host; run `ha sandbox probe` to measure them. A strict request is never downgraded to the host runner."
                .to_owned(),
        }
    }

    #[must_use]
    pub fn with_policy(mut self, policy: ToolPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Attach hooks already filtered by the config trust layer.
    #[must_use]
    pub fn with_hooks(mut self, hooks: Vec<ConfiguredToolHook>) -> Self {
        self.hooks = hooks;
        self
    }

    /// Run observational hooks. Their failures are returned as notices for the
    /// caller and never alter a completed action or receipt.
    pub async fn run_event_hooks(
        &self,
        event: &str,
        payload: Value,
        cancellation: CancellationToken,
    ) -> Vec<String> {
        let tool_name = payload["tool"]["name"].as_str();
        let hooks = self
            .hooks
            .iter()
            .filter(|hook| {
                hook.event == event
                    && tool_name.is_none_or(|name| hook_matches(hook.matcher.as_deref(), name))
            })
            .collect::<Vec<_>>();
        let mut notices = Vec::new();
        for hook in hooks {
            if let Err(reason) =
                run_configured_hook(hook, payload.clone(), cancellation.clone()).await
            {
                notices.push(format!(
                    "{event} hook from {} failed: {reason}",
                    hook.source
                ));
            }
        }
        notices
    }

    pub(crate) async fn run_pre_tool_hooks(
        &self,
        prepared: &PreparedToolRequest,
        cancellation: &CancellationToken,
    ) -> Option<String> {
        let name = prepared.final_action.kind().as_str();
        let payload = tool_hook_payload(prepared, "pre_tool_use");
        for hook in self.hooks.iter().filter(|hook| {
            hook.event == "pre_tool_use" && hook_matches(hook.matcher.as_deref(), name)
        }) {
            match run_configured_hook(hook, payload.clone(), cancellation.clone()).await {
                Ok(()) => {}
                Err(reason) => return Some(format!("{} ({})", reason, hook.source)),
            }
        }
        None
    }

    /// Record a pre-hook refusal with its stable error code and ordinary receipt.
    pub async fn record_hook_block(
        &self,
        prepared: &PreparedToolRequest,
        reason: &str,
    ) -> Result<ToolExecutionView, HarnessError> {
        self.record_denied(
            prepared,
            ToolExecutionId::generate(),
            None,
            ErrorCode::BlockedByHook,
            reason,
        )
        .await
    }

    /// Explain whether an already prepared action is denied, allowed without a
    /// prompt, or still needs an approval answer.
    #[must_use]
    pub fn decision(&self, prepared: &PreparedToolRequest) -> Decision {
        if let Some(reason) = &prepared.policy_denial {
            Decision::Deny(reason.clone())
        } else {
            self.policy.decide(prepared.action())
        }
    }

    /// Replace how `secret://` references are resolved for this host.
    #[must_use]
    pub fn with_secrets(mut self, secrets: Arc<dyn SecretResolver>) -> Self {
        self.secrets = secrets;
        self
    }

    /// Where process output is spooled and how much of it is kept.
    #[must_use]
    pub fn with_spool(mut self, spool: crate::capture::ProcessSpoolConfig) -> Self {
        self.spool = spool;
        self
    }

    /// Attach the external tool dispatcher. Without one, an `ExternalTool`
    /// action is denied rather than silently ignored.
    #[must_use]
    pub fn with_external(mut self, external: Arc<dyn ExternalToolDispatcher>) -> Self {
        self.external = Some(external);
        self
    }

    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn ToolObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    #[must_use]
    pub const fn contract_version() -> u16 {
        TOOL_CONTRACT_VERSION
    }

    #[must_use]
    pub fn capabilities() -> ToolCapabilities {
        ToolCapabilities {
            schema_version: TOOL_CONTRACT_VERSION,
            #[cfg(windows)]
            process_tree_cleanup: "windows_job_object".to_owned(),
            #[cfg(unix)]
            process_tree_cleanup: "posix_process_session".to_owned(),
            #[cfg(not(any(windows, unix)))]
            process_tree_cleanup: "unsupported".to_owned(),
            strict_isolation: false,
            shell_requires_explicit_action: true,
            filesystem_network_sandbox: false,
        }
    }

    /// Build a bounded, redacted unified preview before an approval panel opens.
    pub(crate) fn approval_diff(
        prepared: &PreparedToolRequest,
    ) -> Result<Option<String>, HarnessError> {
        use similar::TextDiff;

        let root = prepared.workspace_root.as_path();
        let (path, before, after) = match prepared.action() {
            CodingToolAction::ApplyPatch {
                path, replacement, ..
            }
            | CodingToolAction::WriteFile {
                path,
                content: replacement,
                ..
            } => {
                let target = resolve_relative(root, path, false)?;
                let before = if target.exists() {
                    read_text(&target)?
                } else {
                    String::new()
                };
                (path.as_str(), before, replacement.clone())
            }
            CodingToolAction::EditFile {
                path,
                old_string,
                new_string,
                replace_all,
            } => {
                let target = resolve_relative(root, path, false)?;
                let before = read_text(&target)?;
                let after =
                    plan_edit_text(&before, old_string, new_string, *replace_all, path)?.content;
                (path.as_str(), before, after)
            }
            _ => return Ok(None),
        };
        let old_name = format!("a/{path}");
        let new_name = format!("b/{path}");
        let diff = TextDiff::from_lines(&before, &after)
            .unified_diff()
            .header(&old_name, &new_name)
            .to_string();
        let lines = diff.lines().collect::<Vec<_>>();
        let truncated = lines.len() > 39;
        let mut preview = lines
            .iter()
            .take(if truncated { 38 } else { 39 })
            .map(|line| redact_text(line).trim_end_matches('\n').to_owned())
            .collect::<Vec<_>>();
        if truncated {
            preview.push("... [diff truncated]".to_owned());
        }
        Ok(Some(format!("[diff]\n{}", preview.join("\n"))))
    }

    /// Validate identity and original arguments, apply deterministic policy
    /// transforms, validate again, then snapshot the exact final action and
    /// workspace state an approval will bind to.
    pub async fn prepare(&self, request: ToolRequest) -> Result<PreparedToolRequest, HarnessError> {
        if request.actor_id.trim().is_empty() {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "tool actor must not be empty",
            ));
        }
        // The deterministic checks run first. Inspecting the workspace hashes every
        // non-ignored file (measured: 962 ms for a `list_files` whose optional path was
        // blank), so an action that cannot be executed must not pay for it — and must
        // not register a project on its way to being refused either.
        let final_action = self.policy.transform_and_validate(request.action.clone())?;
        // The advertised descriptor registry is the authority for built-in
        // names: an unadvertised name is refused before any workspace work.
        // External tools are resolved by the host catalogue that advertised
        // them, so they are validated by their own dispatcher.
        if !matches!(final_action, CodingToolAction::ExternalTool { .. })
            && !coding_tool_names().contains(&final_action.kind().as_str())
        {
            return Err(HarnessError::new(
                ErrorCode::PolicyDenied,
                format!(
                    "tool {} is not in the advertised descriptor registry",
                    final_action.kind().as_str()
                ),
            ));
        }
        if let CodingToolAction::ExternalTool {
            plugin_id,
            tool_name,
            arguments,
            ..
        } = &final_action
        {
            let external = self.external.as_ref().ok_or_else(|| {
                HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "external tool requested without a configured dispatcher",
                )
            })?;
            external
                .validate_external(plugin_id, tool_name, arguments)
                .await?;
        }
        let state = self
            .current_state(&request.session_id, &request.task_id)
            .await?;
        // An artifact id is not a capability. A paged read is refused before a
        // proposal exists unless the artifact belongs to this exact project and
        // task, so a model cannot enumerate another task's evidence.
        if let CodingToolAction::ReadProcessOutput { artifact_id, .. } = &final_action {
            let artifact_id = harness_types::ArtifactId::parse(artifact_id.clone())?;
            let scoped = self
                .store
                .artifact_is_scoped_to(&artifact_id, &state.workspace.project_id, &request.task_id)
                .await
                .map_err(store_error)?;
            if !scoped {
                return Err(HarnessError::new(
                    ErrorCode::ScopeAuthorityDenied,
                    "captured artifact is not scoped to this task",
                ));
            }
        }
        let workspace = if is_read_only(&final_action) {
            inspect_workspace_for_read(&request.workspace_root, state.workspace.project_id.clone())?
        } else {
            inspect_workspace(&request.workspace_root, state.workspace.project_id.clone())?
        };
        self.store
            .register_project(workspace.registration())
            .await
            .map_err(store_error)?;
        Self::validate_workspace_action(&workspace.root, &request.action)?;
        Self::validate_workspace_action(&workspace.root, &final_action)?;
        let action_hash = final_action.canonical_hash()?;
        Ok(PreparedToolRequest {
            request,
            final_action: final_action.clone(),
            action_hash,
            workspace_root: workspace.root,
            workspace_root_text: workspace.root_text,
            workspace_fingerprint: workspace.fingerprint,
            workspace_identity_hash: workspace.identity_hash,
            project_id: workspace.project_id,
            policy_revision: self.policy.revision(),
            policy_denial: self.policy.denial_for(&final_action),
        })
    }

    /// Issue and persist a one-use approval for the exact prepared action.
    pub async fn approve(
        &self,
        prepared: &PreparedToolRequest,
    ) -> Result<ApprovalGrant, HarnessError> {
        self.approve_with_expiry(prepared, None).await
    }

    /// Same as [`Self::approve`], with an optional explicit expiry for UI/host
    /// integrations. The store independently enforces the expiry at consume.
    pub async fn approve_with_expiry(
        &self,
        prepared: &PreparedToolRequest,
        expires_at_unix_ms: Option<u64>,
    ) -> Result<ApprovalGrant, HarnessError> {
        if let Some(reason) = &prepared.policy_denial {
            return Err(HarnessError::new(ErrorCode::PolicyDenied, reason.clone()));
        }
        if prepared.policy_revision != self.policy.revision() {
            return Err(HarnessError::new(
                ErrorCode::ApprovalStale,
                "policy changed before approval could be issued",
            ));
        }
        if expires_at_unix_ms.is_some_and(|expiry| current_unix_ms().is_ok_and(|now| now > expiry))
        {
            return Err(HarnessError::new(
                ErrorCode::ApprovalStale,
                "cannot issue an already expired approval",
            ));
        }
        let approval_id = harness_types::ToolApprovalId::generate();
        let binding_json = json!({
            "schema_version": TOOL_CONTRACT_VERSION,
            "approval_id": approval_id,
            "session_id": prepared.request.session_id,
            "task_id": prepared.request.task_id,
            "invocation_id": prepared.request.invocation_id,
            "call_id": prepared.request.call_id,
            "actor_id": prepared.request.actor_id,
            "action_hash": prepared.action_hash,
            "workspace_root": prepared.workspace_root_text,
            "workspace_fingerprint": prepared.workspace_fingerprint,
            "policy_revision": prepared.policy_revision,
            "tool_revision": TOOL_CONTRACT_VERSION,
            "expires_at_unix_ms": expires_at_unix_ms,
        });
        let binding_hash = ContentHash::from_canonical_json(&binding_json)?;
        let grant = ApprovalGrant {
            approval_id: approval_id.clone(),
            session_id: prepared.request.session_id.clone(),
            task_id: prepared.request.task_id.clone(),
            invocation_id: prepared.request.invocation_id.clone(),
            call_id: prepared.request.call_id.clone(),
            actor_id: prepared.request.actor_id.clone(),
            action_hash: prepared.action_hash.clone(),
            workspace_root: prepared.workspace_root_text.clone(),
            workspace_fingerprint: prepared.workspace_fingerprint.clone(),
            policy_revision: prepared.policy_revision,
            tool_revision: u64::from(TOOL_CONTRACT_VERSION),
            expires_at_unix_ms,
        };
        self.store
            .issue_tool_approval(ToolApprovalRecord {
                approval_id,
                session_id: grant.session_id.clone(),
                task_id: grant.task_id.clone(),
                invocation_id: grant.invocation_id.as_str().to_owned(),
                call_id: grant.call_id.clone(),
                actor_id: grant.actor_id.clone(),
                binding_hash,
                action_hash: grant.action_hash.clone(),
                workspace_root: grant.workspace_root.clone(),
                workspace_fingerprint: grant.workspace_fingerprint.clone(),
                policy_revision: grant.policy_revision,
                tool_revision: grant.tool_revision,
                expires_at_unix_ms,
                state: ToolApprovalState::Active,
                approval_json: binding_json,
            })
            .await
            .map_err(store_error)?;
        Ok(grant)
    }

    pub async fn revoke_approval(
        &self,
        grant: &ApprovalGrant,
        reason: &str,
    ) -> Result<(), HarnessError> {
        self.store
            .revoke_tool_approval(&grant.approval_id, reason)
            .await
            .map_err(store_error)
    }

    /// Execute an approved P3 action without provider cancellation plumbing.
    pub async fn execute(
        &self,
        prepared: PreparedToolRequest,
        approval: Option<ApprovalGrant>,
    ) -> Result<ToolExecutionView, HarnessError> {
        self.execute_with_cancellation(prepared, approval, CancellationToken::new())
            .await
    }

    /// The only public P3 side-effect entrypoint. The ordered gate is:
    /// validate → transform → revalidate → approval → monotonic guards →
    /// durable intent/approval consumption → dispatch → durable receipt →
    /// non-authoritative observer.
    #[allow(clippy::too_many_lines)]
    pub async fn execute_with_cancellation(
        &self,
        prepared: PreparedToolRequest,
        approval: Option<ApprovalGrant>,
        cancellation: CancellationToken,
    ) -> Result<ToolExecutionView, HarnessError> {
        match self.begin(prepared, approval, &cancellation).await? {
            Begun::Finished(view) => Ok(view),
            Begun::Ready(call) => {
                let outcome = self.run_begun(&call, cancellation).await;
                self.finish_begun(*call, outcome).await
            }
        }
    }

    /// First of three phases: every gate check, then the durable intent.
    ///
    /// Intents are committed one at a time, in call order, because each takes the
    /// next journal sequence. What follows - [`Self::run_begun`], the side effect
    /// itself - holds no sequence, so several begun calls can run at once; their
    /// receipts are then committed in call order by [`Self::finish_begun`].
    #[allow(
        clippy::too_many_lines,
        reason = "every gate check before the intent, in the order the contract states them"
    )]
    pub(crate) async fn begin(
        &self,
        prepared: PreparedToolRequest,
        approval: Option<ApprovalGrant>,
        cancellation: &CancellationToken,
    ) -> Result<Begun, HarnessError> {
        let execution_id = ToolExecutionId::generate();
        let transformed = self
            .policy
            .transform_and_validate(prepared.request.action.clone())?;
        if prepared.policy_revision != self.policy.revision()
            || transformed != prepared.final_action
        {
            return self
                .record_denied_begun(
                    &prepared,
                    execution_id,
                    approval.as_ref(),
                    ErrorCode::ApprovalStale,
                    "policy transform or revision changed after preparation",
                )
                .await;
        }
        if let Some(reason) = self.policy.denial_for(&transformed) {
            return self
                .record_denied_begun(
                    &prepared,
                    execution_id,
                    approval.as_ref(),
                    ErrorCode::PolicyDenied,
                    &reason,
                )
                .await;
        }
        if action_requests_strict_isolation(&transformed) {
            let reason = self.strict_refusal_reason();
            return self
                .record_denied_begun(
                    &prepared,
                    execution_id,
                    approval.as_ref(),
                    ErrorCode::StrictIsolationUnavailable,
                    &reason,
                )
                .await;
        }
        if cancellation.is_cancelled() {
            return self
                .record_denied_begun(
                    &prepared,
                    execution_id,
                    approval.as_ref(),
                    ErrorCode::ProcessCanceled,
                    "tool execution was canceled before the durable intent",
                )
                .await;
        }
        let Some(approval) = approval else {
            return self
                .record_denied_begun(
                    &prepared,
                    execution_id,
                    None,
                    ErrorCode::ApprovalRequired,
                    "tool action requires an explicit approval",
                )
                .await;
        };
        if let Some((code, reason)) = approval_mismatch(&prepared, &approval) {
            return self
                .record_denied_begun(&prepared, execution_id, Some(&approval), code, &reason)
                .await;
        }
        Self::validate_workspace_action(&prepared.workspace_root, &transformed)?;
        // The dispatch must act on the workspace the approval named: the same root
        // and repository. Its file contents are not compared: an indexer, an
        // editor or the app's own export writes files while a turn runs, and a
        // whole-tree comparison refused good actions as stale. A write is bound to
        // the file it changes by that file's expected hash; the tree fingerprint is
        // still recorded before and after, for the receipt.
        let reobserved =
            inspect_workspace_for_read(&prepared.workspace_root, prepared.project_id.clone())?;
        if reobserved.identity_hash != prepared.workspace_identity_hash {
            return self
                .record_denied_begun(
                    &prepared,
                    execution_id,
                    Some(&approval),
                    ErrorCode::StaleWorkspace,
                    "workspace root or Git identity changed after approval",
                )
                .await;
        }
        self.store
            .register_project(reobserved.registration())
            .await
            .map_err(store_error)?;
        if let Err(error) = self
            .validate_dispatch_preconditions(&prepared, &transformed)
            .await
        {
            let code = error.code();
            return self
                .record_denied_begun(
                    &prepared,
                    execution_id,
                    Some(&approval),
                    code,
                    &error.to_string(),
                )
                .await;
        }
        if matches!(transformed, CodingToolAction::TaskUpdate { .. }) {
            return self
                .execute_task_update(prepared, approval, execution_id)
                .await
                .map(Begun::Finished);
        }
        let mut state = self
            .current_state(&prepared.request.session_id, &prepared.request.task_id)
            .await?;
        let intent_sequence = self
            .store
            .next_sequence(&prepared.request.session_id)
            .await
            .map_err(store_error)?;
        state.pending_tool_calls.push(PendingToolCall {
            execution_id: execution_id.clone(),
            state: PendingToolState::Pending,
            reconciliation_hint: "inspect the durable tool intent and workspace before any retry"
                .to_owned(),
        });
        state.revision = intent_sequence;
        state.through_event_seq = intent_sequence;
        let intent = ToolIntentRecord {
            tool_execution_id: execution_id.clone(),
            session_id: prepared.request.session_id.clone(),
            task_id: prepared.request.task_id.clone(),
            invocation_id: prepared.request.invocation_id.as_str().to_owned(),
            call_id: prepared.request.call_id.clone(),
            actor_id: prepared.request.actor_id.clone(),
            tool_name: transformed.kind().as_str().to_owned(),
            action_json: transformed.canonical_value()?,
            action_hash: prepared.action_hash.clone(),
            workspace_root: prepared.workspace_root_text.clone(),
            workspace_fingerprint: prepared.workspace_fingerprint.clone(),
            before_fingerprint: Some(prepared.workspace_fingerprint.clone()),
            policy_revision: prepared.policy_revision,
            tool_revision: u64::from(TOOL_CONTRACT_VERSION),
            approval: binding_from_grant(&approval),
            status: ToolIntentStatus::Recorded,
            intent_sequence,
        };
        let intent_event = event_for_intent(&intent)?;
        StorePort::admit_invocation(
            self.store.as_ref(),
            ToolIntentCommit {
                expected_sequence: intent_sequence,
                event: intent_event.clone(),
                intent,
                working_state: state,
                marker: SourceWorkMarker {
                    marker_id: format!("tool-intent:{}", execution_id.as_str()),
                    event_id: intent_event.event_id,
                    sequence: intent_sequence,
                    kind: "tool_intent".to_owned(),
                    status: "committed".to_owned(),
                },
            },
        )
        .await?;

        Ok(Begun::Ready(Box::new(BegunCall {
            prepared,
            approval,
            execution_id,
            transformed,
        })))
    }

    /// Second phase: the side effect, with no journal write, so begun calls can
    /// run side by side.
    pub(crate) async fn run_begun(
        &self,
        call: &BegunCall,
        cancellation: CancellationToken,
    ) -> Result<Dispatched, HarnessError> {
        let BegunCall {
            prepared,
            execution_id,
            transformed,
            ..
        } = call;
        match transformed {
            CodingToolAction::ExternalTool {
                plugin_id,
                tool_name,
                arguments,
                timeout_ms,
                ..
            } => match &self.external {
                Some(external) => external
                    .dispatch_external(
                        &ToolDispatchAuthorization::after_gate_commit(),
                        plugin_id,
                        tool_name,
                        arguments,
                        *timeout_ms,
                    )
                    .await
                    .map(Dispatched::plain),
                None => Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "no external tool dispatcher is configured for this host",
                )),
            },
            other => {
                // Secret values are resolved here, after the durable intent and
                // immediately before the spawn: they exist only for the length
                // of this dispatch.
                let environment = ProcessEnvironment::resolve(
                    ProcessEnvironment::bindings_of(other),
                    |reference| self.policy.allows_secret(reference),
                    self.secrets.as_ref(),
                );
                match environment {
                    Ok(environment) => {
                        self.dispatch(prepared, other, cancellation, &environment, execution_id)
                            .await
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }

    /// Third phase: the receipt, committed in call order.
    pub(crate) async fn finish_begun(
        &self,
        call: BegunCall,
        dispatched: Result<Dispatched, HarnessError>,
    ) -> Result<ToolExecutionView, HarnessError> {
        let BegunCall {
            prepared,
            approval,
            execution_id,
            ..
        } = call;
        match dispatched {
            Ok(dispatched) => {
                self.settle(
                    &prepared,
                    execution_id,
                    Some(approval.approval_id.as_str()),
                    dispatched.output,
                    dispatched.artifact,
                    ToolOutcomeState::Settled,
                    ToolIntentStatus::Settled,
                )
                .await
            }
            Err(error) => {
                let reason =
                    format!("side effect may be uncertain; do not rerun automatically: {error}");
                self.settle(
                    &prepared,
                    execution_id,
                    Some(approval.approval_id.as_str()),
                    ToolOutput::OutcomeUnknown { reason },
                    None,
                    ToolOutcomeState::OutcomeUnknown,
                    ToolIntentStatus::OutcomeUnknown,
                )
                .await
            }
        }
    }

    /// Inspect a pending intent after restart. It deliberately never dispatches
    /// the action again. Only an exact observed patch post-state is settled as
    /// applied; all other cases remain an explicit unknown outcome.
    pub async fn reconcile_pending(
        &self,
        session_id: &harness_types::SessionId,
        execution_id: &ToolExecutionId,
    ) -> Result<ToolExecutionView, HarnessError> {
        let intent = self
            .store
            .tool_intent(execution_id)
            .await
            .map_err(store_error)?
            .ok_or_else(|| {
                HarnessError::new(ErrorCode::InvalidPayload, "tool intent was not found")
            })?;
        if intent.session_id != *session_id || intent.status != ToolIntentStatus::Recorded {
            return Err(HarnessError::new(
                ErrorCode::ToolIntentConflict,
                "tool intent is not pending for this session",
            ));
        }
        let action: CodingToolAction =
            serde_json::from_value(intent.action_json.clone()).map_err(|_| {
                HarnessError::new(ErrorCode::InvalidPayload, "stored tool action is invalid")
            })?;
        let state = self
            .current_state(&intent.session_id, &intent.task_id)
            .await?;
        let root = Path::new(&intent.workspace_root);
        let output = match &action {
            CodingToolAction::ApplyPatch {
                path,
                expected_hash,
                replacement,
                ..
            } => {
                let path = resolve_relative(root, path, false)?;
                match read_text(&path) {
                    Ok(current) if current == *replacement => ToolOutput::ApplyPatch {
                        path: path.to_string_lossy().replace('\\', "/"),
                        before_hash: expected_hash.clone(),
                        after_hash: ContentHash::from_bytes(current.as_bytes()),
                    },
                    _ => ToolOutput::OutcomeUnknown {
                        reason: "pending patch has no exact observed post-state; no rerun was attempted"
                            .to_owned(),
                    },
                }
            }
            _ => ToolOutput::OutcomeUnknown {
                reason: "pending non-idempotent tool requires user-directed reconciliation; no rerun was attempted"
                    .to_owned(),
            },
        };
        let outcome = if matches!(output, ToolOutput::ApplyPatch { .. }) {
            ToolOutcomeState::Settled
        } else {
            ToolOutcomeState::OutcomeUnknown
        };
        let final_status = if outcome == ToolOutcomeState::Settled {
            ToolIntentStatus::Settled
        } else {
            ToolIntentStatus::OutcomeUnknown
        };
        let prepared = PreparedToolRequest {
            request: ToolRequest {
                session_id: intent.session_id.clone(),
                task_id: intent.task_id.clone(),
                actor_id: intent.actor_id.clone(),
                invocation_id: harness_types::ToolInvocationId::parse(
                    intent.invocation_id.clone(),
                )?,
                call_id: intent.call_id.clone(),
                workspace_root: root.to_owned(),
                action: action.clone(),
            },
            final_action: action,
            action_hash: intent.action_hash.clone(),
            workspace_root: root.to_owned(),
            workspace_root_text: intent.workspace_root.clone(),
            workspace_fingerprint: intent.workspace_fingerprint.clone(),
            workspace_identity_hash: ContentHash::from_bytes(intent.workspace_root.as_bytes()),
            project_id: state.workspace.project_id,
            policy_revision: intent.policy_revision,
            policy_denial: None,
        };
        self.settle(
            &prepared,
            execution_id.clone(),
            Some(intent.approval.approval_id.as_str()),
            output,
            None,
            outcome,
            final_status,
        )
        .await
    }

    pub async fn reassociate_workspace(
        &self,
        session_id: &harness_types::SessionId,
        task_id: &TaskId,
        old_canonical_root: &str,
        replacement_root: impl AsRef<Path>,
    ) -> Result<(), HarnessError> {
        let state = self.current_state(session_id, task_id).await?;
        let replacement = inspect_workspace(replacement_root.as_ref(), state.workspace.project_id)?;
        self.store
            .reassociate_project(
                &replacement.project_id,
                old_canonical_root,
                replacement.registration(),
            )
            .await
            .map_err(store_error)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the central workspace guard applies containment and typed bounds before approval"
    )]
    fn validate_workspace_action(
        root: &Path,
        action: &CodingToolAction,
    ) -> Result<(), HarnessError> {
        match action {
            CodingToolAction::ReadFile { path, .. }
            | CodingToolAction::ApplyPatch { path, .. }
            | CodingToolAction::WriteFile { path, .. }
            | CodingToolAction::EditFile { path, .. } => {
                let _ = resolve_relative(root, path, false)?;
            }
            CodingToolAction::ListFiles { path }
            | CodingToolAction::SearchText { path, .. }
            | CodingToolAction::GitDiff { path }
            | CodingToolAction::GitLog { path, .. } => {
                if let Some(path) = path {
                    let _ = resolve_relative(root, path, true)?;
                }
            }
            CodingToolAction::Glob { pattern, path } => {
                validate_glob(pattern)?;
                if let Some(path) = path {
                    let _ = resolve_relative(root, path, true)?;
                }
            }
            CodingToolAction::RunProcess { .. }
            | CodingToolAction::RunShell { .. }
            | CodingToolAction::ReadProcessOutput { .. }
            | CodingToolAction::HistorySearch { .. }
            | CodingToolAction::HistoryRead { .. }
            | CodingToolAction::GitStatus
            | CodingToolAction::TaskUpdate { .. }
            | CodingToolAction::ExternalTool { .. } => {}
        }
        match action {
            CodingToolAction::SearchText {
                query,
                regex,
                case_insensitive,
                glob,
                context_lines,
                ..
            } => {
                validate_search(query, *regex, *case_insensitive)?;
                if let Some(glob) = glob {
                    validate_glob(glob)?;
                }
                if context_lines
                    .is_some_and(|lines| lines > crate::contracts::SEARCH_CONTEXT_MAX_LINES)
                {
                    return Err(HarnessError::new(
                        ErrorCode::InvalidPayload,
                        "search context exceeds the configured line bound",
                    ));
                }
            }
            CodingToolAction::WriteFile {
                path,
                content,
                expected_hash,
            } => {
                if content.len() > crate::workspace::MAX_TEXT_FILE_BYTES {
                    return Err(HarnessError::new(
                        ErrorCode::OutputLimitExceeded,
                        "workspace write exceeds the 1 MiB text limit",
                    ));
                }
                let target = resolve_relative(root, path, false)?;
                if target.exists() {
                    let current = read_text(&target)?;
                    let current_hash = ContentHash::from_bytes(current.as_bytes());
                    if expected_hash.as_ref() != Some(&current_hash) {
                        return Err(HarnessError::new(
                            ErrorCode::StaleWorkspace,
                            "overwriting an existing file requires its matching expected_hash",
                        ));
                    }
                } else if expected_hash.is_some() {
                    return Err(HarnessError::new(
                        ErrorCode::StaleWorkspace,
                        "expected_hash was supplied but the target file does not exist",
                    ));
                }
                let parent = target.parent().ok_or_else(|| {
                    HarnessError::new(ErrorCode::WorkspaceEscape, "write path has no parent")
                })?;
                if !parent.is_dir() {
                    return Err(HarnessError::new(
                        ErrorCode::InvalidPayload,
                        "write_file parent directory does not exist",
                    ));
                }
            }
            CodingToolAction::EditFile {
                path,
                old_string,
                new_string,
                replace_all,
            } => {
                let target = resolve_relative(root, path, false)?;
                let current = read_text(&target)?;
                let _ = plan_edit_text(&current, old_string, new_string, *replace_all, path)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Validate operation-specific state before a durable intent claims a
    /// side effect may occur. The patch hash is checked here and again inside
    /// the atomic write helper to close the ordinary stale-edit case while
    /// still treating a race after intent conservatively. A paged capture read
    /// is checked here too: a page that cannot exist is a refusal, not an
    /// unknown outcome that implies a side effect might have happened.
    #[allow(
        clippy::too_many_lines,
        reason = "operation preconditions stay ordered before durable intent and dispatch"
    )]
    async fn validate_dispatch_preconditions(
        &self,
        prepared: &PreparedToolRequest,
        action: &CodingToolAction,
    ) -> Result<(), HarnessError> {
        let root = prepared.workspace_root.as_path();
        match action {
            CodingToolAction::ReadFile {
                path,
                offset,
                limit,
            } => {
                // Deterministic content-policy failures (binary or unsupported
                // encoding) are denied before an intent is created. A later
                // disappearance/race remains an ordinary settled/unknown
                // dispatch result, never an unsafe success.
                let target = resolve_relative(root, path, false)?;
                let _ = read_text(&target)?;
                let _ = read_file_range(
                    &target,
                    offset.unwrap_or(0),
                    limit.unwrap_or(crate::contracts::READ_FILE_DEFAULT_LINES),
                )?;
            }
            CodingToolAction::Glob { pattern, .. } => validate_glob(pattern)?,
            CodingToolAction::SearchText {
                query,
                regex,
                case_insensitive,
                glob,
                ..
            } => {
                validate_search(query, *regex, *case_insensitive)?;
                if let Some(glob) = glob {
                    validate_glob(glob)?;
                }
            }
            CodingToolAction::ApplyPatch {
                path,
                expected_hash,
                ..
            } => {
                let target = resolve_relative(root, path, false)?;
                let current = read_text(&target)?;
                if ContentHash::from_bytes(current.as_bytes()) != *expected_hash {
                    return Err(HarnessError::new(
                        ErrorCode::StaleWorkspace,
                        "patch expected hash does not match current file content",
                    ));
                }
            }
            CodingToolAction::WriteFile {
                path,
                expected_hash,
                ..
            } => {
                let target = resolve_relative(root, path, false)?;
                match read_text(&target) {
                    Ok(current) => {
                        let current_hash = ContentHash::from_bytes(current.as_bytes());
                        if expected_hash.as_ref() != Some(&current_hash) {
                            return Err(HarnessError::new(
                                ErrorCode::StaleWorkspace,
                                "write_file expected_hash does not match current file content",
                            ));
                        }
                    }
                    Err(error) if error.code() == ErrorCode::InvalidPayload && !target.exists() => {
                        if expected_hash.is_some() {
                            return Err(HarnessError::new(
                                ErrorCode::StaleWorkspace,
                                "write_file expected_hash was supplied for a missing file",
                            ));
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
            CodingToolAction::EditFile {
                path,
                old_string,
                new_string,
                replace_all,
            } => {
                let target = resolve_relative(root, path, false)?;
                let current = read_text(&target)?;
                let _ = plan_edit_text(&current, old_string, new_string, *replace_all, path)?;
            }
            CodingToolAction::ReadProcessOutput {
                artifact_id,
                stream,
                offset,
                ..
            } => {
                let header = self.capture_header(artifact_id).await?;
                let total = match stream {
                    CaptureStream::Stdout => header.stdout_bytes,
                    CaptureStream::Stderr => header.stderr_bytes,
                };
                if *offset > total {
                    return Err(HarnessError::new(
                        ErrorCode::InvalidPayload,
                        format!(
                            "requested offset {offset} is past the {total} captured {} bytes",
                            stream.as_str()
                        ),
                    ));
                }
            }
            CodingToolAction::HistorySearch { query, .. } => {
                if query.trim().is_empty() {
                    return Err(HarnessError::new(
                        ErrorCode::InvalidPayload,
                        "a history search needs at least one searchable term",
                    ));
                }
            }
            CodingToolAction::HistoryRead {
                source_id, offset, ..
            } => {
                // A foreign or expired reference is a refusal, not an unknown
                // outcome: the read can be decided before any intent exists.
                let scope = self.history_scope(prepared).await?;
                let page = self
                    .store
                    .history_read(&scope, source_id, *offset, 1)
                    .await
                    .map_err(store_error)?;
                let _ = page;
            }
            _ => {}
        }
        Ok(())
    }

    /// The capture header of a published process artifact.
    async fn capture_header(
        &self,
        artifact_id: &str,
    ) -> Result<capture::CaptureHeader, HarnessError> {
        let probe = self
            .store
            .read_artifact_page(artifact_id, 0, CAPTURE_HEADER_PROBE_BYTES)
            .await
            .map_err(store_error)?
            .ok_or_else(capture_not_found)?;
        capture::parse_capture_header(&probe.bytes)
    }

    async fn execute_task_update(
        &self,
        prepared: PreparedToolRequest,
        approval: ApprovalGrant,
        _execution_id: ToolExecutionId,
    ) -> Result<ToolExecutionView, HarnessError> {
        let CodingToolAction::TaskUpdate { note } = prepared.final_action else {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "non-task action reached task update path",
            ));
        };
        let mut state = self
            .current_state(&prepared.request.session_id, &prepared.request.task_id)
            .await?;
        let sequence = self
            .store
            .next_sequence(&prepared.request.session_id)
            .await
            .map_err(store_error)?;
        state.next_action_proposals.push(NextActionProposal {
            authority: SourceAuthority::RuntimeObserved,
            description: note.clone(),
        });
        state.revision = sequence;
        state.through_event_seq = sequence;
        let event = event_for_task_update(&prepared.request.session_id, sequence, &note)?;
        StorePort::commit_task_update(
            self.store.as_ref(),
            ToolTaskUpdateCommit {
                session_id: prepared.request.session_id,
                task_id: prepared.request.task_id,
                expected_sequence: sequence,
                event: event.clone(),
                working_state: state,
                marker: SourceWorkMarker {
                    marker_id: format!("tool-task-update:{sequence}"),
                    event_id: event.event_id,
                    sequence,
                    kind: "tool_task_update".to_owned(),
                    status: "committed".to_owned(),
                },
                approval: binding_from_grant(&approval),
            },
        )
        .await?;
        let mut view = ToolExecutionView {
            schema_version: TOOL_CONTRACT_VERSION,
            execution_id: None,
            receipt: None,
            output: ToolOutput::TaskUpdate { note },
            observer_failure: None,
        };
        self.notify(&mut view);
        Ok(view)
    }

    #[allow(clippy::too_many_lines)] // one dispatch table, one arm per tool
    async fn dispatch(
        &self,
        prepared: &PreparedToolRequest,
        action: &CodingToolAction,
        cancellation: CancellationToken,
        environment: &ProcessEnvironment,
        execution_id: &ToolExecutionId,
    ) -> Result<Dispatched, HarnessError> {
        let root = prepared.workspace_root.as_path();
        match action {
            CodingToolAction::ExternalTool { .. } => Err(HarnessError::new(
                ErrorCode::PolicyDenied,
                "external tool actions are dispatched by the configured dispatcher, not directly",
            )),
            CodingToolAction::ReadFile {
                path,
                offset,
                limit,
            } => {
                let target = resolve_relative(root, path, false)?;
                let output = read_file_range(
                    &target,
                    offset.unwrap_or(0),
                    limit.unwrap_or(crate::contracts::READ_FILE_DEFAULT_LINES),
                )?;
                Ok(Dispatched::plain(ToolOutput::ReadFile {
                    path: path.replace('\\', "/"),
                    content: output.text,
                    truncated: output.truncated,
                }))
            }
            CodingToolAction::ListFiles { path } => {
                let (paths, truncated) = list_files(root, path.as_deref())?;
                Ok(Dispatched::plain(ToolOutput::ListFiles {
                    paths,
                    truncated,
                }))
            }
            CodingToolAction::SearchText {
                query,
                path,
                regex,
                case_insensitive,
                glob,
                context_lines,
            } => {
                let output = search_text(
                    root,
                    query,
                    path.as_deref(),
                    *regex,
                    *case_insensitive,
                    glob.as_deref(),
                    context_lines.unwrap_or(0),
                )?;
                Ok(Dispatched::plain(ToolOutput::SearchText {
                    matches: output.matches,
                    truncated: output.truncated,
                }))
            }
            CodingToolAction::ApplyPatch {
                path,
                expected_hash,
                replacement,
            } => {
                let target = resolve_relative(root, path, false)?;
                let before = read_text(&target)?;
                let artifact = self.publish_before_content(Some(&before))?;
                let mutation = apply_text_patch(&target, expected_hash, replacement)?;
                let output = ToolOutput::ApplyPatch {
                    path: path.replace('\\', "/"),
                    before_hash: mutation.before_hash.clone(),
                    after_hash: mutation.after_hash.clone(),
                };
                Ok(Dispatched { output, artifact })
            }
            CodingToolAction::WriteFile {
                path,
                content,
                expected_hash,
            } => {
                let target = resolve_relative(root, path, false)?;
                let before = if target.exists() {
                    Some(read_text(&target)?)
                } else {
                    None
                };
                let artifact = self.publish_before_content(before.as_deref())?;
                let mutation = write_text_checked(&target, expected_hash.as_ref(), content)?;
                let output = ToolOutput::WriteFile {
                    path: path.replace('\\', "/"),
                    before_hash: mutation.before_hash.clone(),
                    after_hash: mutation.after_hash.clone(),
                };
                Ok(Dispatched { output, artifact })
            }
            CodingToolAction::EditFile {
                path,
                old_string,
                new_string,
                replace_all,
            } => {
                let target = resolve_relative(root, path, false)?;
                let before = read_text(&target)?;
                let artifact = self.publish_before_content(Some(&before))?;
                let (mutation, diff) =
                    edit_text(&target, path, old_string, new_string, *replace_all)?;
                let output = ToolOutput::EditFile {
                    path: path.replace('\\', "/"),
                    before_hash: mutation.before_hash.clone(),
                    after_hash: mutation.after_hash.clone(),
                    replacements: mutation.replacements,
                    diff: redact_text(&diff),
                };
                Ok(Dispatched { output, artifact })
            }
            CodingToolAction::Glob { pattern, path } => {
                let (paths, truncated) = glob_files(root, path.as_deref(), pattern)?;
                Ok(Dispatched::plain(ToolOutput::Glob { paths, truncated }))
            }
            CodingToolAction::ReadProcessOutput {
                artifact_id,
                stream,
                offset,
                length,
            } => {
                let page = self
                    .read_capture_page(artifact_id, *stream, *offset, *length)
                    .await?;
                Ok(Dispatched::plain(page))
            }
            CodingToolAction::HistorySearch { query, limit } => {
                let hits = self.history_search(prepared, query, *limit).await?;
                Ok(Dispatched::plain(hits))
            }
            CodingToolAction::HistoryRead {
                source_id,
                offset,
                length,
            } => {
                let page = self
                    .history_read(prepared, source_id, *offset, *length)
                    .await?;
                Ok(Dispatched::plain(page))
            }
            CodingToolAction::RunProcess {
                executable,
                args,
                timeout_ms,
                isolation,
                ..
            } => {
                let lease = self
                    .begin_backend_lease(prepared, action, *timeout_ms, *isolation, execution_id)
                    .await?;
                let output = process::run_structured(
                    root,
                    executable,
                    args,
                    *timeout_ms,
                    cancellation,
                    environment,
                    &self.spool,
                )
                .await;
                self.finish_backend_lease(lease, output).await
            }
            CodingToolAction::RunShell {
                command,
                timeout_ms,
                isolation,
                ..
            } => {
                let lease = self
                    .begin_backend_lease(prepared, action, *timeout_ms, *isolation, execution_id)
                    .await?;
                let output = process::run_shell(
                    root,
                    command,
                    *timeout_ms,
                    cancellation,
                    environment,
                    &self.spool,
                )
                .await;
                self.finish_backend_lease(lease, output).await
            }
            CodingToolAction::GitStatus => {
                let output = process::run_structured(
                    root,
                    "git",
                    &[
                        "status".to_owned(),
                        "--porcelain=v1".to_owned(),
                        "--branch".to_owned(),
                        "--untracked-files=all".to_owned(),
                    ],
                    15_000,
                    cancellation,
                    &ProcessEnvironment::empty(),
                    &self.spool,
                )
                .await?;
                Ok(Dispatched::plain(git_output("status", output)))
            }
            CodingToolAction::GitDiff { path } => {
                let mut args = vec!["diff".to_owned(), "--no-ext-diff".to_owned()];
                if let Some(path) = path {
                    args.push("--".to_owned());
                    args.push(path.clone());
                }
                let output = process::run_structured(
                    root,
                    "git",
                    &args,
                    15_000,
                    cancellation,
                    &ProcessEnvironment::empty(),
                    &self.spool,
                )
                .await?;
                Ok(Dispatched::plain(git_output("diff", output)))
            }
            CodingToolAction::GitLog { path, limit } => {
                let args = git_log_arguments(path.as_deref(), *limit);
                let output = process::run_structured(
                    root,
                    "git",
                    &args,
                    15_000,
                    cancellation,
                    &ProcessEnvironment::empty(),
                    &self.spool,
                )
                .await?;
                Ok(Dispatched::plain(git_output("log", output)))
            }
            CodingToolAction::TaskUpdate { .. } => Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "task update must use its atomic task projection path",
            )),
        }
    }

    /// The scope one history read runs under: this task, plus whatever lineage a
    /// fork was explicitly allowed to keep reading.
    pub async fn history_scope(
        &self,
        prepared: &PreparedToolRequest,
    ) -> Result<HistoryScope, HarnessError> {
        let grants = self
            .store
            .history_grants(&prepared.request.session_id)
            .await
            .map_err(store_error)?;
        Ok(HistoryScope::new(
            prepared.project_id.clone(),
            prepared.request.task_id.clone(),
        )
        .with_grants(grants))
    }

    /// Bring the journal index up to date and search it inside this task.
    async fn history_search(
        &self,
        prepared: &PreparedToolRequest,
        query: &str,
        limit: Option<u32>,
    ) -> Result<ToolOutput, HarnessError> {
        // The read scope is the task, so the index has to cover every session
        // that worked on it; otherwise a continuation session could not find
        // what its predecessor wrote.
        self.store
            .index_task_history(&prepared.request.task_id)
            .await
            .map_err(store_error)?;
        let scope = self.history_scope(prepared).await?;
        let wanted = limit.unwrap_or(HISTORY_SEARCH_DEFAULT_LIMIT);
        let hits = self
            .store
            .history_search(&scope, query, usize::try_from(wanted).unwrap_or(10))
            .await
            .map_err(store_error)?;
        let truncated = hits.len() >= usize::try_from(wanted).unwrap_or(10);
        Ok(ToolOutput::HistorySearch {
            hits: hits
                .into_iter()
                .map(|hit| crate::HistoryHitView {
                    source_id: hit.source_id,
                    sequence: hit.sequence,
                    kind: hit.kind,
                    availability: hit.availability.as_str().to_owned(),
                    matched_terms: hit.matched_terms,
                    preview: hit.preview,
                })
                .collect(),
            truncated,
        })
    }

    /// Read one exact page of an indexed source.
    async fn history_read(
        &self,
        prepared: &PreparedToolRequest,
        source_id: &str,
        offset: u64,
        length: u32,
    ) -> Result<ToolOutput, HarnessError> {
        let scope = self.history_scope(prepared).await?;
        let page = self
            .store
            .history_read(
                &scope,
                source_id,
                offset,
                usize::try_from(length.min(HISTORY_READ_MAX_BYTES)).unwrap_or(usize::MAX),
            )
            .await
            .map_err(store_error)?;
        Ok(ToolOutput::HistoryRead {
            source_id: page.source_id,
            sequence: page.sequence,
            source_kind: page.kind,
            offset: page.offset,
            length: u64::try_from(page.bytes.len()).unwrap_or(u64::MAX),
            total_bytes: page.total_bytes,
            text: String::from_utf8_lossy(&page.bytes).into_owned(),
        })
    }

    /// Persist a lease for a process execution before the process exists (M12-03).
    ///
    /// A lease that cannot be written stops the call: an execution whose
    /// lifecycle is not recorded is exactly the resource a later host cannot
    /// reconcile.
    async fn begin_backend_lease(
        &self,
        prepared: &PreparedToolRequest,
        action: &CodingToolAction,
        timeout_ms: u64,
        isolation: IsolationMode,
        execution_id: &ToolExecutionId,
    ) -> Result<crate::LeaseOwner, HarnessError> {
        let plan = crate::ExecutionPlan::for_process_action(
            action,
            &prepared.workspace_root,
            &self.measured_capabilities(),
            Self::strict_profile(isolation),
            timeout_ms,
            self.spool.limits().max_capture_bytes,
        );
        let request = crate::LeaseRequest::from_plan(
            &plan,
            prepared.request.session_id.as_str(),
            prepared.request.task_id.as_str(),
            execution_id.as_str(),
            // Lease locks live with the store, not with the capture staging
            // directory: they are durable lifecycle state, and the spool is
            // emptied as soon as a capture is published.
            self.store.paths().data_dir.join("leases"),
        );
        crate::LeaseOwner::open(&self.store, &request).await
    }

    /// Close the lease after the capture is published, and keep the artifact
    /// digest in the record: export is what happens next, and it must be able to
    /// check the digest the execution recorded.
    async fn finish_backend_lease(
        &self,
        lease: crate::LeaseOwner,
        output: Result<ProcessResult, HarnessError>,
    ) -> Result<Dispatched, HarnessError> {
        let output = output?;
        let dispatched = self.dispatched_process(output)?;
        let artifact = dispatched.artifact.as_ref().map(|artifact| {
            (
                artifact.artifact_id.as_str(),
                artifact.content_hash.as_str(),
            )
        });
        lease.release(&self.store, artifact).await?;
        Ok(dispatched)
    }

    /// The matrix this host was measured against, or the unmeasured one.
    ///
    /// A host nobody probed enforces nothing *as far as this process knows*, so
    /// the substitute matrix is complete and every verdict in it is
    /// `unsupported` with that reason attached. It is never a silent default: a
    /// plan built from it claims nothing, and a strict request against it is
    /// refused for want of a measurement.
    fn measured_capabilities(&self) -> crate::CapabilityMatrix {
        match &self.capabilities {
            Some(matrix) => (**matrix).clone(),
            None => crate::CapabilityMatrix::new(
                crate::HostIdentity::observed("unmeasured"),
                crate::Capability::ALL
                    .into_iter()
                    .map(|capability| {
                        crate::CapabilityFinding::unsupported(
                            capability,
                            crate::CapabilityEvidence::new(
                                "P-UNMEASURED",
                                "no probe has been run in this process",
                                "unsupported: this host has not been measured; run `ha sandbox probe`",
                            ),
                        )
                    })
                    .collect(),
            ),
        }
    }

    /// The profile a call runs under. `strict` is Full, and Full is refused
    /// before this point on a host that cannot enforce it.
    fn strict_profile(isolation: IsolationMode) -> crate::StrictProfile {
        match isolation {
            IsolationMode::Strict => crate::StrictProfile::Full,
            IsolationMode::BestEffort => crate::StrictProfile::Containment,
        }
    }

    /// Publish a finished process capture as the durable artifact the receipt
    /// will reference, and describe it in the model-facing output.
    ///
    /// The capture is published *before* the receipt exists, so a receipt can
    /// only ever point at bytes that are already flushed.
    fn dispatched_process(&self, output: ProcessResult) -> Result<Dispatched, HarnessError> {
        let (artifact, captured_bytes, capture_hash, capture_truncated, capture_tail) =
            match &output.capture {
                Some(capture) => {
                    let bytes = std::fs::read(&capture.path).map_err(|error| {
                        HarnessError::new(
                            ErrorCode::ArtifactWriteFailed,
                            format!("cannot read the finished capture: {error}"),
                        )
                    })?;
                    let published = self.store.publish_artifact(&bytes).map_err(store_error)?;
                    let _ = std::fs::remove_file(&capture.path);
                    (
                        Some(published),
                        capture.bytes,
                        Some(capture.hash.clone()),
                        capture.truncated,
                        capture.tail.clone(),
                    )
                }
                None => (None, 0, None, false, String::new()),
            };
        let output = process_output(
            output,
            artifact.as_ref(),
            capture_hash,
            capture_truncated,
            &capture_tail,
            captured_bytes,
        );
        Ok(Dispatched { output, artifact })
    }

    /// Read one page of a captured process output.
    ///
    /// The scope check already happened at preparation; this re-checks it, so a
    /// dispatch can never read an artifact the gate did not authorize.
    pub async fn read_capture_page(
        &self,
        artifact_id: &str,
        stream: CaptureStream,
        offset: u64,
        length: u32,
    ) -> Result<ToolOutput, HarnessError> {
        let page_length =
            usize::try_from(length.min(PROCESS_OUTPUT_PAGE_MAX_BYTES)).unwrap_or(usize::MAX);
        // The header is host framing at the very start of the artifact; it says
        // where each stream's bytes live so a page never has to guess. The
        // request was already validated before the intent; this re-reads it so
        // the dispatch itself cannot page outside the captured bytes.
        let header = self.capture_header(artifact_id).await?;
        let (start, total) = match stream {
            CaptureStream::Stdout => (header.stdout_offset(), header.stdout_bytes),
            CaptureStream::Stderr => (header.stderr_offset(), header.stderr_bytes),
        };
        if offset > total {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                format!(
                    "requested offset {offset} is past the {total} captured {} bytes",
                    stream.as_str()
                ),
            ));
        }
        // A page never runs past the end of its own stream: the next section of
        // the artifact is a different stream, not more of this one.
        let remaining = usize::try_from(total - offset).unwrap_or(usize::MAX);
        let page = self
            .store
            .read_artifact_page(artifact_id, start + offset, page_length.min(remaining))
            .await
            .map_err(store_error)?
            .ok_or_else(capture_not_found)?;
        Ok(ToolOutput::ProcessOutput {
            artifact_id: artifact_id.to_owned(),
            stream: stream.as_str().to_owned(),
            offset,
            length: u64::try_from(page.bytes.len()).unwrap_or(u64::MAX),
            total_bytes: total,
            text: String::from_utf8_lossy(&page.bytes).into_owned(),
        })
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "the receipt, artifact, event, and intent settle in one durable transaction"
    )]
    async fn settle(
        &self,
        prepared: &PreparedToolRequest,
        execution_id: ToolExecutionId,
        approval_id: Option<&str>,
        output: ToolOutput,
        artifact: Option<PublishedArtifact>,
        outcome_state: ToolOutcomeState,
        final_status: ToolIntentStatus,
    ) -> Result<ToolExecutionView, HarnessError> {
        let mut state = self
            .current_state(&prepared.request.session_id, &prepared.request.task_id)
            .await?;
        let sequence = self
            .store
            .next_sequence(&prepared.request.session_id)
            .await
            .map_err(store_error)?;
        state
            .pending_tool_calls
            .retain(|pending| pending.execution_id != execution_id);
        state.revision = sequence;
        state.through_event_seq = sequence;
        // A read changed nothing: its workspace after is the one it started from.
        let after_fingerprint = if is_read_only(&prepared.final_action) {
            prepared.workspace_fingerprint.clone()
        } else {
            inspect_workspace(&prepared.workspace_root, prepared.project_id.clone()).map_or_else(
                |_| prepared.workspace_fingerprint.clone(),
                |workspace| workspace.fingerprint,
            )
        };
        // A process capture is its own durable evidence: the receipt points at
        // the captured bytes rather than at a re-serialization of them. Every
        // other output keeps the long-standing behavior of publishing its own
        // model-facing view.
        let artifact = match artifact {
            Some(artifact) => Some(artifact),
            None => self.publish_output_artifact(&output)?,
        };
        let (before_hash, after_hash) = match &output {
            ToolOutput::ApplyPatch {
                before_hash,
                after_hash,
                ..
            }
            | ToolOutput::EditFile {
                before_hash,
                after_hash,
                ..
            }
            | ToolOutput::WriteFile {
                before_hash,
                after_hash,
                ..
            } => (Some(before_hash.clone()), Some(after_hash.clone())),
            _ => (None, None),
        };
        let receipt = ToolExecutionReceipt {
            schema_version: P0_SCHEMA_VERSION,
            tool_execution_id: execution_id.clone(),
            task_id: prepared.request.task_id.clone(),
            invocation_id: prepared.request.invocation_id.as_str().to_owned(),
            call_id: prepared.request.call_id.clone(),
            input_hash: prepared.action_hash.clone(),
            policy_revision: prepared.policy_revision,
            approval_id: approval_id.map(ToOwned::to_owned),
            intent_state: ToolIntentState::IntentRecorded,
            outcome_state,
            before_fingerprint: Some(prepared.workspace_fingerprint.clone()),
            after_fingerprint: Some(after_fingerprint),
            before_hash,
            after_hash,
            exit_code: match &output {
                ToolOutput::Process { exit_code, .. } => *exit_code,
                _ => None,
            },
            artifact_id: artifact
                .as_ref()
                .map(|artifact| artifact.artifact_id.clone()),
            observed_at_seq: sequence,
        };
        let model_view = json!({
            "call_id": prepared.request.call_id,
            "text": crate::turn_driver::render_tool_output(
                prepared.final_action.kind().as_str(),
                &output,
            ),
        });
        let event = event_for_receipt(
            &prepared.request.session_id,
            sequence,
            &receipt,
            "p3",
            Some(&model_view),
        )?;
        StorePort::settle_invocation(
            self.store.as_ref(),
            ToolSettlementCommit {
                expected_sequence: sequence,
                event: event.clone(),
                receipt: receipt.clone(),
                working_state: state,
                marker: SourceWorkMarker {
                    marker_id: format!("tool-receipt:{}", execution_id.as_str()),
                    event_id: event.event_id,
                    sequence,
                    kind: "tool_receipt".to_owned(),
                    status: match outcome_state {
                        ToolOutcomeState::Settled => "settled".to_owned(),
                        ToolOutcomeState::OutcomeUnknown => "outcome_unknown".to_owned(),
                        ToolOutcomeState::NotStarted | ToolOutcomeState::Denied => {
                            "invalid".to_owned()
                        }
                    },
                },
                artifact,
                final_status,
            },
        )
        .await?;
        let mut view = ToolExecutionView {
            schema_version: TOOL_CONTRACT_VERSION,
            execution_id: Some(execution_id),
            receipt: Some(receipt),
            output,
            observer_failure: None,
        };
        self.notify(&mut view);
        Ok(view)
    }

    async fn record_denied_begun(
        &self,
        prepared: &PreparedToolRequest,
        execution_id: ToolExecutionId,
        approval: Option<&ApprovalGrant>,
        code: ErrorCode,
        reason: &str,
    ) -> Result<Begun, HarnessError> {
        self.record_denied(prepared, execution_id, approval, code, reason)
            .await
            .map(Begun::Finished)
    }

    async fn record_denied(
        &self,
        prepared: &PreparedToolRequest,
        execution_id: ToolExecutionId,
        approval: Option<&ApprovalGrant>,
        code: ErrorCode,
        reason: &str,
    ) -> Result<ToolExecutionView, HarnessError> {
        let mut state = self
            .current_state(&prepared.request.session_id, &prepared.request.task_id)
            .await?;
        let sequence = self
            .store
            .next_sequence(&prepared.request.session_id)
            .await
            .map_err(store_error)?;
        state.revision = sequence;
        state.through_event_seq = sequence;
        let receipt = ToolExecutionReceipt {
            schema_version: P0_SCHEMA_VERSION,
            tool_execution_id: execution_id.clone(),
            task_id: prepared.request.task_id.clone(),
            invocation_id: prepared.request.invocation_id.as_str().to_owned(),
            call_id: prepared.request.call_id.clone(),
            input_hash: prepared.action_hash.clone(),
            policy_revision: prepared.policy_revision,
            approval_id: approval.map(|grant| grant.approval_id.as_str().to_owned()),
            intent_state: ToolIntentState::Denied,
            outcome_state: ToolOutcomeState::Denied,
            before_fingerprint: Some(prepared.workspace_fingerprint.clone()),
            after_fingerprint: None,
            before_hash: None,
            after_hash: None,
            exit_code: None,
            artifact_id: None,
            observed_at_seq: sequence,
        };
        let denied_view = (code == ErrorCode::BlockedByHook).then(|| {
            json!({
                "kind": "denied",
                "code": code.as_str(),
                "reason": reason,
            })
        });
        let event = event_for_receipt(
            &prepared.request.session_id,
            sequence,
            &receipt,
            "p3_denied",
            denied_view.as_ref(),
        )?;
        StorePort::record_synthetic_receipt(
            self.store.as_ref(),
            harness_types::ReceiptCommit {
                session_id: prepared.request.session_id.clone(),
                task_id: prepared.request.task_id.clone(),
                expected_sequence: sequence,
                event: event.clone(),
                receipt: receipt.clone(),
                working_state: state,
                marker: SourceWorkMarker {
                    marker_id: format!("tool-denied:{}", execution_id.as_str()),
                    event_id: event.event_id,
                    sequence,
                    kind: "tool_denied".to_owned(),
                    status: code.as_str().to_owned(),
                },
                artifact: None,
            },
        )
        .await?;
        let mut view = ToolExecutionView {
            schema_version: TOOL_CONTRACT_VERSION,
            execution_id: Some(execution_id),
            receipt: Some(receipt),
            output: ToolOutput::Denied {
                code: code.as_str().to_owned(),
                reason: reason.to_owned(),
            },
            observer_failure: None,
        };
        self.notify(&mut view);
        Ok(view)
    }

    fn publish_output_artifact(
        &self,
        output: &ToolOutput,
    ) -> Result<Option<PublishedArtifact>, HarnessError> {
        let bytes = serde_json::to_vec(output).map_err(|_| {
            HarnessError::new(
                ErrorCode::ArtifactWriteFailed,
                "tool output cannot be serialized",
            )
        })?;
        self.store
            .publish_artifact(&bytes)
            .map(Some)
            .map_err(store_error)
    }

    fn publish_before_content(
        &self,
        before: Option<&str>,
    ) -> Result<Option<PublishedArtifact>, HarnessError> {
        let Some(before) = before else {
            return Ok(None);
        };
        if before.len() > crate::workspace::MAX_TEXT_FILE_BYTES {
            return Err(HarnessError::new(
                ErrorCode::OutputLimitExceeded,
                "pre-edit artifact exceeds the 1 MiB limit",
            ));
        }
        self.store
            .publish_artifact(before.as_bytes())
            .map(Some)
            .map_err(store_error)
    }

    fn notify(&self, view: &mut ToolExecutionView) {
        let Some(observer) = &self.observer else {
            return;
        };
        let result = catch_unwind(AssertUnwindSafe(|| observer.observe(view)));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => view.observer_failure = Some(error),
            Err(_) => {
                view.observer_failure =
                    Some("tool observer panicked after receipt commit".to_owned());
            }
        }
    }

    async fn current_state(
        &self,
        session_id: &harness_types::SessionId,
        task_id: &TaskId,
    ) -> Result<WorkingState, HarnessError> {
        let state = self
            .store
            .current_projection(task_id)
            .await
            .map_err(store_error)?
            .ok_or_else(|| {
                HarnessError::new(ErrorCode::InvalidPayload, "task has no admitted state")
            })?;
        if state.session_id != *session_id {
            return Err(HarnessError::new(
                ErrorCode::TaskLeaseConflict,
                "requested session does not own the task projection",
            ));
        }
        Ok(state)
    }
}

fn binding_from_grant(grant: &ApprovalGrant) -> ToolApprovalBinding {
    ToolApprovalBinding {
        approval_id: grant.approval_id.clone(),
        session_id: grant.session_id.clone(),
        task_id: grant.task_id.clone(),
        invocation_id: grant.invocation_id.as_str().to_owned(),
        call_id: grant.call_id.clone(),
        actor_id: grant.actor_id.clone(),
        action_hash: grant.action_hash.clone(),
        workspace_root: grant.workspace_root.clone(),
        workspace_fingerprint: grant.workspace_fingerprint.clone(),
        policy_revision: grant.policy_revision,
        tool_revision: grant.tool_revision,
    }
}

fn approval_mismatch(
    prepared: &PreparedToolRequest,
    approval: &ApprovalGrant,
) -> Option<(ErrorCode, String)> {
    if approval.actor_id != prepared.request.actor_id
        || approval.session_id != prepared.request.session_id
        || approval.task_id != prepared.request.task_id
        || approval.invocation_id != prepared.request.invocation_id
        || approval.call_id != prepared.request.call_id
        || approval.action_hash != prepared.action_hash
        || approval.workspace_root != prepared.workspace_root_text
        || approval.workspace_fingerprint != prepared.workspace_fingerprint
        || approval.policy_revision != prepared.policy_revision
        || approval.tool_revision != u64::from(TOOL_CONTRACT_VERSION)
    {
        return Some((
            ErrorCode::ApprovalStale,
            "approval does not bind the final session/task/invocation/actor/action/workspace/policy revision".to_owned(),
        ));
    }
    if approval
        .expires_at_unix_ms
        .is_some_and(|expiry| current_unix_ms().is_ok_and(|now| now > expiry))
    {
        return Some((ErrorCode::ApprovalStale, "approval has expired".to_owned()));
    }
    None
}

fn action_requests_strict_isolation(action: &CodingToolAction) -> bool {
    matches!(
        action,
        CodingToolAction::RunProcess {
            isolation: IsolationMode::Strict,
            ..
        } | CodingToolAction::RunShell {
            isolation: IsolationMode::Strict,
            ..
        }
    )
}

/// How much of an artifact is read to parse the capture header that precedes
/// the captured bytes. The header is a short JSON line written by this host.
const CAPTURE_HEADER_PROBE_BYTES: usize = 8 * 1024;

fn process_output(
    output: ProcessResult,
    artifact: Option<&PublishedArtifact>,
    capture_hash: Option<ContentHash>,
    capture_truncated: bool,
    capture_tail: &str,
    captured_bytes: u64,
) -> ToolOutput {
    ToolOutput::Process {
        executable: output.executable,
        shell: output.shell,
        exit_code: output.exit_code,
        timed_out: output.timed_out,
        canceled: output.canceled,
        queued: output.queued,
        // Only a confirmed reap produces a `ProcessResult` at all: an unconfirmed
        // one is a `ProcessOutcomeUnknown` error that settles as an
        // outcome-unknown receipt, so this stays a property of the evidence
        // rather than an assumption about the kill request.
        tree_cleanup_confirmed: matches!(
            output.tree_cleanup,
            TreeCleanup::NothingToClean | TreeCleanup::KilledAndReaped
        ),
        tree_cleanup: output.tree_cleanup.as_str().to_owned(),
        stdout: redact_text(&output.stdout),
        stderr: redact_text(&output.stderr),
        stdout_truncated: output.stdout_truncated,
        stderr_truncated: output.stderr_truncated,
        artifact_id: artifact.map(|artifact| artifact.artifact_id.as_str().to_owned()),
        captured_bytes,
        capture_hash,
        capture_truncated,
        capture_tail: redact_text(capture_tail),
        stdout_tail: redact_tail(&output.stdout_tail),
        stderr_tail: redact_tail(&output.stderr_tail),
    }
}

/// A stream tail with the same line redaction every other preview gets.
fn redact_tail(tail: &StreamTail) -> StreamTail {
    StreamTail {
        text: redact_text(&tail.text),
        ..tail.clone()
    }
}

/// A dispatched action: what the model sees, and the durable artifact that
/// belongs to the receipt when the action produced one of its own.
pub(crate) struct Dispatched {
    output: ToolOutput,
    artifact: Option<PublishedArtifact>,
}

/// A call past its gate: either already answered, or intent-committed and ready
/// to run.
#[allow(
    clippy::large_enum_variant,
    reason = "one value per tool call, moved once; boxing the view only adds an allocation"
)]
pub(crate) enum Begun {
    Finished(ToolExecutionView),
    Ready(Box<BegunCall>),
}

/// A call whose durable intent is committed and whose side effect has not run.
pub(crate) struct BegunCall {
    prepared: PreparedToolRequest,
    approval: ApprovalGrant,
    execution_id: ToolExecutionId,
    transformed: CodingToolAction,
}

impl Dispatched {
    const fn plain(output: ToolOutput) -> Self {
        Self {
            output,
            artifact: None,
        }
    }
}

fn capture_not_found() -> HarnessError {
    HarnessError::new(
        ErrorCode::InvalidPayload,
        "captured artifact was not found for this task",
    )
}

/// The exact `git log` argv: no pager, no color, bounded, and tab-separated
/// fields (full hash, ISO 8601 author date, subject) so a caller can parse it
/// without a second command.
fn git_log_arguments(path: Option<&str>, limit: Option<u32>) -> Vec<String> {
    let count = limit.unwrap_or(GIT_LOG_DEFAULT_LIMIT);
    let mut args = vec![
        "log".to_owned(),
        "--no-color".to_owned(),
        format!("-n{count}"),
        "--format=%H%x09%aI%x09%s".to_owned(),
    ];
    if let Some(path) = path {
        args.push("--".to_owned());
        args.push(path.to_owned());
    }
    args
}

fn git_output(operation: &str, output: ProcessResult) -> ToolOutput {
    let combined = if output.stderr.is_empty() {
        output.stdout
    } else if output.stdout.is_empty() {
        output.stderr
    } else {
        format!("{}\n{}", output.stdout, output.stderr)
    };
    ToolOutput::Git {
        operation: operation.to_owned(),
        output: redact_text(&combined),
        truncated: output.stdout_truncated || output.stderr_truncated,
    }
}

fn event_for_intent(intent: &ToolIntentRecord) -> Result<EventEnvelope, HarnessError> {
    let mut payload = Map::new();
    payload.insert(
        "tool_execution_id".to_owned(),
        Value::String(intent.tool_execution_id.as_str().to_owned()),
    );
    payload.insert(
        "tool_name".to_owned(),
        Value::String(intent.tool_name.clone()),
    );
    payload.insert(
        "action_hash".to_owned(),
        Value::String(intent.action_hash.as_str().to_owned()),
    );
    payload.insert(
        "reconciliation_hint".to_owned(),
        Value::String("inspect before retry; never blindly replay a durable intent".to_owned()),
    );
    let payload_hash = ContentHash::from_canonical_json(&Value::Object(payload.clone()))?;
    Ok(EventEnvelope {
        schema_version: P0_SCHEMA_VERSION,
        event_id: EventId::generate(),
        session_id: intent.session_id.clone(),
        seq: intent.intent_sequence,
        event_type: "tool.intent.recorded".to_owned(),
        producer: producer(),
        authority: SourceAuthority::RuntimeObserved,
        correlation_id: None,
        causation_id: None,
        continuity_critical: true,
        payload,
        payload_hash,
    })
}

fn event_for_receipt(
    session_id: &harness_types::SessionId,
    sequence: u64,
    receipt: &ToolExecutionReceipt,
    kind: &str,
    model_view: Option<&Value>,
) -> Result<EventEnvelope, HarnessError> {
    let mut payload = Map::new();
    payload.insert(
        "receipt".to_owned(),
        serde_json::to_value(receipt).map_err(|_| {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                "tool receipt cannot be serialized",
            )
        })?,
    );
    payload.insert("kind".to_owned(), Value::String(kind.to_owned()));
    // The bounded, redacted view the model was given travels with the receipt
    // event: a continuation after a crash replays it as a paired tool result
    // instead of rerunning the tool or parsing the artifact.
    if let Some(view) = model_view {
        payload.insert("model_view".to_owned(), view.clone());
    }
    let payload_hash = ContentHash::from_canonical_json(&Value::Object(payload.clone()))?;
    Ok(EventEnvelope {
        schema_version: P0_SCHEMA_VERSION,
        event_id: EventId::generate(),
        session_id: session_id.clone(),
        seq: sequence,
        event_type: "receipt.recorded".to_owned(),
        producer: producer(),
        authority: SourceAuthority::RuntimeObserved,
        correlation_id: None,
        causation_id: None,
        continuity_critical: true,
        payload,
        payload_hash,
    })
}

fn event_for_task_update(
    session_id: &harness_types::SessionId,
    sequence: u64,
    note: &str,
) -> Result<EventEnvelope, HarnessError> {
    let mut payload = Map::new();
    payload.insert("note".to_owned(), Value::String(note.to_owned()));
    let payload_hash = ContentHash::from_canonical_json(&Value::Object(payload.clone()))?;
    Ok(EventEnvelope {
        schema_version: P0_SCHEMA_VERSION,
        event_id: EventId::generate(),
        session_id: session_id.clone(),
        seq: sequence,
        event_type: "task.updated".to_owned(),
        producer: producer(),
        authority: SourceAuthority::RuntimeObserved,
        correlation_id: None,
        causation_id: None,
        continuity_critical: true,
        payload,
        payload_hash,
    })
}

fn producer() -> ProducerIdentity {
    ProducerIdentity {
        plugin_id: "p3.tools".to_owned(),
        implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

fn current_unix_ms() -> Result<u64, HarnessError> {
    let duration = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| {
        HarnessError::new(
            ErrorCode::ApprovalStale,
            "system clock is before Unix epoch",
        )
    })?;
    u64::try_from(duration.as_millis()).map_err(|_| {
        HarnessError::new(
            ErrorCode::ApprovalStale,
            "system clock milliseconds exceed range",
        )
    })
}

fn store_error(error: StoreError) -> HarnessError {
    error.into_harness_error()
}

/// Whether an action only reads, so the workspace need not be fingerprinted for it.
fn is_read_only(action: &CodingToolAction) -> bool {
    crate::contracts::effect_class_for(action.kind().as_str())
        == crate::contracts::EffectClass::ReadOnly
}

#[cfg(test)]
mod tests {
    use super::ToolExecutionService;
    use crate::{
        CodingToolAction, PolicyMode, PreparedToolRequest, ToolPatternRule, ToolPolicy,
        ToolRequest, observe_workspace,
    };
    use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
    use harness_types::{ContentHash, ErrorCode, HostId, InputId, ProjectId, SessionId, TaskId};
    use std::sync::Arc;

    #[test]
    fn g09_hook_payload_truncates_large_action_arguments_to_eight_kibibytes() {
        let root = std::env::temp_dir().join("ha-g09-hook-payload");
        let action = CodingToolAction::WriteFile {
            path: "src/generated.txt".to_owned(),
            content: "x".repeat(20_000),
            expected_hash: None,
        };
        let request = ToolRequest::new(
            SessionId::generate(),
            TaskId::generate(),
            "test.actor",
            root.clone(),
            action.clone(),
        );
        let prepared = PreparedToolRequest {
            request,
            action_hash: action.canonical_hash().expect("canonical action hash"),
            final_action: action,
            workspace_root: root.clone(),
            workspace_root_text: root.to_string_lossy().into_owned(),
            workspace_fingerprint: ContentHash::from_bytes(b"workspace"),
            workspace_identity_hash: ContentHash::from_bytes(b"identity"),
            project_id: ProjectId::generate(),
            policy_revision: 0,
            policy_denial: None,
        };

        let payload = super::tool_hook_payload(&prepared, "pre_tool_use");
        let bytes = serde_json::to_vec(&payload).expect("bounded payload serializes");
        assert!(bytes.len() <= 8 * 1024, "payload was {} bytes", bytes.len());
        assert_eq!(payload["tool"]["args"]["truncated"], true);
        assert!(payload["tool"]["args"]["preview"].as_str().is_some());
    }

    /// A malformed action is refused by the cheap check, not by the workspace walk.
    ///
    /// Measured in the field: a TUI turn showed `[tool] list_files {"path": ""} failed
    /// 962ms`, because the optional-path check ran *after* `inspect_workspace` had hashed
    /// the whole repository. The reason is the same either way; what this pins is that
    /// nothing expensive — and nothing durable — happens first.
    #[tokio::test]
    async fn a_malformed_action_is_refused_before_the_workspace_is_inspected() {
        let temp = std::env::temp_dir().join(format!(
            "harness-tools-prepare-{}",
            harness_types::InputId::generate()
        ));
        std::fs::create_dir_all(&temp).expect("temp store directory");
        let store = Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(temp.clone(), HostId::generate()))
                .await
                .expect("store opens"),
        );
        let service = ToolExecutionService::new(Arc::clone(&store));
        let error = service
            .prepare(ToolRequest::new(
                SessionId::generate(),
                TaskId::generate(),
                "test.actor",
                temp.clone(),
                CodingToolAction::ListFiles {
                    path: Some("   ".to_owned()),
                },
            ))
            .await
            .expect_err("a blank optional path is refused");
        assert_eq!(error.code(), ErrorCode::InvalidPayload);
        assert!(
            error
                .to_string()
                .contains("optional tool path must not be blank"),
            "the deterministic check must answer first, got: {error}"
        );

        let registration = crate::workspace_registration(ProjectId::generate(), &temp)
            .expect("registration record");
        assert!(
            store
                .registered_project(&registration.canonical_root)
                .await
                .expect("project read")
                .is_none(),
            "a refused action must not register the project"
        );
        drop(service);
        Arc::try_unwrap(store)
            .expect("single owner")
            .close()
            .await
            .expect("store closes");
        let _ = std::fs::remove_dir_all(temp);
    }

    /// The kinds a host may auto-approve, and the three that it never may.
    ///
    /// Written as an exhaustive list on purpose: this predicate is the whole
    /// definition of what "read-only" means to the approval gate, so a new
    /// `ToolKind` cannot be added without this test failing and someone deciding
    /// which side it belongs on.
    #[test]
    fn tool_kinds_classify_read_only_by_construction_not_by_name() {
        use crate::ToolKind;

        for kind in [
            ToolKind::ReadFile,
            ToolKind::ListFiles,
            ToolKind::SearchText,
            ToolKind::Glob,
            ToolKind::GitStatus,
            ToolKind::GitDiff,
            ToolKind::GitLog,
        ] {
            assert!(
                kind.is_read_only(),
                "{} only reads, so a host may stop asking for it",
                kind.as_str()
            );
        }
        for kind in [
            ToolKind::ApplyPatch,
            ToolKind::WriteFile,
            ToolKind::EditFile,
            ToolKind::RunProcess,
            ToolKind::RunShell,
            ToolKind::TaskUpdate,
            ToolKind::ExternalTool,
        ] {
            assert!(
                !kind.is_read_only(),
                "{} can change the world, so it must always be asked about",
                kind.as_str()
            );
        }
    }

    /// Auto-approving a read-only action skips the *question*, never the check.
    ///
    /// This is the test that keeps the wider grant from becoming a way to read a
    /// credential file: a read-only kind whose path is protected is refused in
    /// `prepare`, which runs before any proposal - and therefore before any gate -
    /// exists. There is nothing for an "allow reads" answer to cover.
    #[tokio::test]
    async fn a_read_only_kind_cannot_reach_a_protected_path_through_the_gate() {
        use crate::observe_workspace;
        use harness_session::{AdmitInputRequest, SessionService};
        use harness_types::SourceAuthority;

        let temp = std::env::temp_dir().join(format!(
            "harness-tools-sensitive-{}",
            harness_types::InputId::generate()
        ));
        // The store lives beside the workspace, not inside it: a workspace walk that
        // hashes the open database fails on a locked file, which is a property of the
        // fixture and would hide the refusal this test is about.
        let store_dir = temp.join("store");
        let workspace = temp.join("workspace");
        std::fs::create_dir_all(workspace.join("src")).expect("temp workspace");
        std::fs::create_dir_all(&store_dir).expect("temp store");
        std::fs::write(workspace.join(".env"), "API_KEY=not-a-real-secret\n").expect("env file");
        std::fs::write(workspace.join("src").join("main.rs"), "fn main() {}\n")
            .expect("source file");
        let store = Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(store_dir, HostId::generate()))
                .await
                .expect("store opens"),
        );
        // A real admitted task, so a refusal below can only come from the path: with
        // no admission every call fails with the same code and the test would prove
        // nothing.
        let session_id = SessionId::generate();
        let task_id = TaskId::generate();
        let project_id = ProjectId::generate();
        SessionService::new(Arc::clone(&store))
            .admit_input(AdmitInputRequest {
                session_id: session_id.clone(),
                task_id: task_id.clone(),
                input_id: harness_types::InputId::generate(),
                expected_sequence: 1,
                authority: SourceAuthority::User,
                raw_text: "read the workspace".to_owned(),
                workspace: observe_workspace(project_id, &workspace)
                    .expect("workspace observation"),
                initial_plan_items: Vec::new(),
            })
            .await
            .expect("fixture input is admitted");

        let service = ToolExecutionService::new(Arc::clone(&store));
        for (path, expected) in [
            (".env", ErrorCode::SensitivePathDenied),
            ("../outside.txt", ErrorCode::WorkspaceEscape),
            ("src/../../outside.txt", ErrorCode::WorkspaceEscape),
        ] {
            let error = service
                .prepare(ToolRequest::new(
                    session_id.clone(),
                    task_id.clone(),
                    "test.actor",
                    workspace.clone(),
                    CodingToolAction::ReadFile {
                        path: path.to_owned(),
                        offset: None,
                        limit: None,
                    },
                ))
                .await
                .expect_err("a protected or escaping path is refused, not offered");
            assert_eq!(
                error.code(),
                expected,
                "{path} must be refused before a proposal exists"
            );
        }

        // The same kind on an ordinary path is still prepared, which is what proves
        // the refusals above came from the path and not from the kind.
        service
            .prepare(ToolRequest::new(
                session_id,
                task_id,
                "test.actor",
                workspace.clone(),
                CodingToolAction::ReadFile {
                    path: "src/main.rs".to_owned(),
                    offset: None,
                    limit: None,
                },
            ))
            .await
            .expect("an ordinary read is prepared");

        drop(service);
        Arc::try_unwrap(store)
            .expect("single owner")
            .close()
            .await
            .expect("store closes");
        let _ = std::fs::remove_dir_all(temp);
    }

    #[tokio::test]
    async fn g05_protected_path_beats_every_allow_rule_and_mode() {
        use harness_session::{AdmitInputRequest, SessionService};
        use harness_types::SourceAuthority;

        let temp = std::env::temp_dir().join(format!(
            "harness-tools-g05-protected-{}",
            harness_types::InputId::generate()
        ));
        let workspace = temp.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(temp.join("store")).expect("store directory");
        std::fs::write(workspace.join(".env"), "TOKEN=not-a-secret\n").expect("protected fixture");
        let store = Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(
                temp.join("store"),
                HostId::generate(),
            ))
            .await
            .expect("store opens"),
        );
        let session_id = SessionId::generate();
        let task_id = TaskId::generate();
        SessionService::new(Arc::clone(&store))
            .admit_input(AdmitInputRequest {
                session_id: session_id.clone(),
                task_id: task_id.clone(),
                input_id: InputId::generate(),
                expected_sequence: 1,
                authority: SourceAuthority::User,
                raw_text: "inspect config".to_owned(),
                workspace: observe_workspace(ProjectId::generate(), &workspace)
                    .expect("workspace observation"),
                initial_plan_items: Vec::new(),
            })
            .await
            .expect("input is admitted");

        let tools = ToolExecutionService::new(Arc::clone(&store)).with_policy(
            ToolPolicy::default()
                .with_tool_rules(vec![ToolPatternRule::allow(
                    "read_file(.env)",
                    "test allow must not bypass protection",
                )])
                .with_mode(PolicyMode::FullAuto),
        );
        let error = tools
            .prepare(ToolRequest::new(
                session_id,
                task_id,
                "test.actor",
                workspace,
                CodingToolAction::ReadFile {
                    path: ".env".to_owned(),
                    offset: None,
                    limit: None,
                },
            ))
            .await
            .expect_err("protected path is rejected before a rule or mode can allow it");
        assert_eq!(error.code(), ErrorCode::SensitivePathDenied);
        drop(tools);
        Arc::try_unwrap(store)
            .expect("single store owner")
            .close()
            .await
            .expect("store closes");
        let _ = std::fs::remove_dir_all(temp);
    }
}
