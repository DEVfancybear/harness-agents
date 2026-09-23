//! P6 acceptance: trust contracts, bounded stdio transport, the external tool
//! and provider bridges, the MCP client adapter, skills composition and
//! unload/reload, exercised through real extension processes.
//!
//! Only external boundaries are absent: a paid model API and a remote MCP
//! endpoint are out of scope and are not exercised. The P3 gate, the store, the
//! kernel registries and every extension process are real.

#[path = "phase_p6/support.rs"]
mod support;

use std::sync::Arc;

use harness_extensions::{
    CallOutcome, DEFAULT_CALL_TIMEOUT_MS, EXTENSION_PROTOCOL_VERSION, ExtensionCapability,
    ExtensionError, ExtensionFrame, ExtensionProvider, ExtensionToolDispatcher, ExtensionTransport,
    HANDSHAKE_TIMEOUT_MS, InactiveReason, LoadOutcome, MAX_FRAME_BYTES, MAX_INFLIGHT_CALLS,
    MAX_STDERR_BYTES, McpClient, SkillSource, TrustGrant, compose_skills, discover_skills,
    is_allowlisted_host_method, supported_host_methods, supported_protocol_versions,
};
use harness_types::{ErrorCode, PluginInstanceId, ScopeId, ToolExecutionId, ToolInvocationId};
use serde_json::json;

use support::{
    FixtureMode, assert_code, environment, fixture_manifest, fixture_mcp_server, fixture_plugin,
    grant_for, manifest_for, runtime,
};

// ---------------------------------------------------------------------------
// P6-S01
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p6_s01_extension_trust_contracts_are_versioned() {
    let manifest = fixture_manifest();
    manifest.validate().expect("fixture manifest is valid");
    assert_eq!(manifest.schema_version, EXTENSION_PROTOCOL_VERSION);
    assert_eq!(supported_protocol_versions(), [EXTENSION_PROTOCOL_VERSION]);

    // Reading a manifest executes nothing and resolves no secret, so a
    // repository can never start a plugin by being opened.
    let directory = tempfile::tempdir().expect("tempdir");
    let path = support::write_manifest(directory.path(), &manifest);
    let parsed = harness_extensions::host::read_manifest(&path).expect("manifest reads");
    assert_eq!(parsed.plugin_id, manifest.plugin_id);

    // A manifest that asks for an ungranted secret is still only a request.
    let with_secret = manifest_for(
        "p6.fixture.tools",
        manifest.executable_digest.clone(),
        vec![ExtensionCapability::Tools],
        vec!["secret://p6/fixture".to_owned()],
        Vec::new(),
    );
    let grant = grant_for(&with_secret, Vec::new());
    let error = grant.verify_manifest(&with_secret).unwrap_err();
    assert_eq!(error.code(), ErrorCode::SecretNotGranted);
    let allowed = grant_for(&with_secret, vec!["secret://p6/fixture".to_owned()]);
    allowed
        .verify_manifest(&with_secret)
        .expect("an explicit secret grant is accepted");

    // A host method outside the allowlist is refused at manifest validation.
    let mut denied_method = manifest.clone();
    denied_method.requested_host_methods = vec!["host.dangerous.unrestricted".to_owned()];
    let error = denied_method.validate().unwrap_err();
    assert_eq!(error.code(), ErrorCode::HostMethodDenied);

    // An environment variable outside the allowlist is refused too.
    let mut denied_env = manifest.clone();
    denied_env.requested_environment = vec!["AWS_SECRET_ACCESS_KEY".to_owned()];
    let error = denied_env.validate().unwrap_err();
    assert_eq!(error.code(), ErrorCode::EnvironmentDenied);

    // The allowlist is the only authority for host methods.
    assert!(is_allowlisted_host_method("host.echo"));
    assert!(!is_allowlisted_host_method("host.database.query"));
    assert_eq!(supported_host_methods().len(), 4);
}

// ---------------------------------------------------------------------------
// P6-S02 / K12
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p6_s02_bounded_stdio_transport_enforces_handshake_and_limits() {
    let manifest = fixture_manifest();
    let grant = grant_for(&manifest, Vec::new());
    let scope = ScopeId::generate();
    let transport = ExtensionTransport::connect(
        fixture_plugin(),
        &manifest,
        &grant,
        scope.clone(),
        1,
        environment(FixtureMode::Normal),
    )
    .await
    .expect("handshake succeeds");
    let session = transport.session();
    assert_eq!(session.protocol_version, EXTENSION_PROTOCOL_VERSION);
    assert_eq!(session.plugin_id, manifest.plugin_id);
    assert!(session.provides(ExtensionCapability::Tools));
    assert_eq!(session.host_methods, vec!["host.echo".to_owned()]);
    assert_eq!(transport.generation(), 1);
    assert_eq!(transport.scope_id(), &scope);

    // A real call is answered and recorded through the transport.
    let outcome = transport
        .call(
            "tool.read_observation",
            json!({"arguments": {"subject": "src/lib.rs"}}),
        )
        .await
        .expect("call is sent");
    let payload = outcome.answered().expect("fixture answers");
    assert_eq!(payload["observation"], "fixture observation");

    // The digest a grant pins is the executable's real digest.
    let digest = harness_extensions::executable_digest(&fixture_plugin()).expect("digest");
    assert_eq!(digest, manifest.executable_digest);
    transport.shutdown().await;

    // A digest mismatch refuses to start anything at all.
    let mut wrong = manifest.clone();
    wrong.executable_digest = harness_types::ContentHash::from_bytes(b"not-the-plugin");
    let error = ExtensionTransport::connect(
        fixture_plugin(),
        &wrong,
        &grant_for(&wrong, Vec::new()),
        ScopeId::generate(),
        1,
        environment(FixtureMode::Normal),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ExtensionDigestMismatch);

    // An environment variable name outside the allowlist is refused before any
    // process starts, and no secret value is ever placed in one.
    let mut with_env = manifest.clone();
    with_env.requested_environment = vec!["P6_FIXTURE_SECRET".to_owned()];
    let error = with_env.validate().unwrap_err();
    assert_eq!(error.code(), ErrorCode::EnvironmentDenied);

    // The pinned numbers are the ones the transport actually applies.
    assert_eq!(MAX_FRAME_BYTES, 1024 * 1024);
    assert_eq!(MAX_INFLIGHT_CALLS, 4);
    assert_eq!(MAX_STDERR_BYTES, 64 * 1024);
    assert_eq!(HANDSHAKE_TIMEOUT_MS, 5_000);
    assert_eq!(DEFAULT_CALL_TIMEOUT_MS, 30_000);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One fault-window walk; splitting hides the order.
async fn p6_k12_malformed_oversized_and_duplicate_frames_fail_bounded() {
    let manifest = fixture_manifest();
    let grant = grant_for(&manifest, Vec::new());

    // A malformed handshake frame never yields a negotiated session.
    let error = ExtensionTransport::connect(
        fixture_plugin(),
        &manifest,
        &grant,
        ScopeId::generate(),
        1,
        environment(FixtureMode::MalformedFrame),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            error.code(),
            ErrorCode::ExtensionProtocolError | ErrorCode::ExtensionProtocolUnsupported
        ),
        "malformed frame must fail closed: {error}"
    );

    // An oversize frame is refused rather than buffered. The host drops the
    // frame, so the handshake never settles and the refusal is bounded.
    let error = ExtensionTransport::connect(
        fixture_plugin(),
        &manifest,
        &grant,
        ScopeId::generate(),
        1,
        environment(FixtureMode::OversizeFrame),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            error.code(),
            ErrorCode::ExtensionProtocolError
                | ErrorCode::ExtensionProtocolUnsupported
                | ErrorCode::FrameLimitExceeded
        ),
        "oversize frame must fail closed: {error}"
    );

    // The frame limit is enforced at encode time too.
    let huge = ExtensionFrame::response("x", json!({"fill": "y".repeat(MAX_FRAME_BYTES + 16)}));
    let error = huge.encode_line().unwrap_err();
    assert_eq!(error.code(), ErrorCode::FrameLimitExceeded);

    // An unsupported protocol revision is refused during negotiation. The
    // plugin answers with a frame the host cannot accept, which is a typed
    // protocol refusal rather than a hang.
    let error = ExtensionTransport::connect(
        fixture_plugin(),
        &manifest,
        &grant,
        ScopeId::generate(),
        1,
        environment(FixtureMode::BadProtocol),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            error.code(),
            ErrorCode::ExtensionProtocolUnsupported | ErrorCode::ExtensionProtocolError
        ),
        "an unsupported protocol must fail closed: {error}"
    );

    // A capability the manifest never promised is refused.
    let error = ExtensionTransport::connect(
        fixture_plugin(),
        &manifest,
        &grant,
        ScopeId::generate(),
        1,
        environment(FixtureMode::UnknownCapability),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            error.code(),
            ErrorCode::ExtensionProtocolError | ErrorCode::ExtensionProtocolUnsupported
        ),
        "a capability outside the manifest must fail closed: {error}"
    );

    // A host method outside the allowlist is refused.
    let error = ExtensionTransport::connect(
        fixture_plugin(),
        &manifest,
        &grant,
        ScopeId::generate(),
        1,
        environment(FixtureMode::DeniedHostMethod),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::HostMethodDenied);

    // A plugin that exits before the handshake is reported, not retried.
    let error = ExtensionTransport::connect(
        fixture_plugin(),
        &manifest,
        &grant,
        ScopeId::generate(),
        1,
        environment(FixtureMode::ExitBeforeHandshake),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            error.code(),
            ErrorCode::ExtensionProtocolError | ErrorCode::ExtensionProtocolUnsupported
        ),
        "an early exit must be reported: {error}"
    );

    // Duplicate response ids settle nothing: the first answer wins and the
    // second is ignored, so no call is settled twice.
    let transport = ExtensionTransport::connect(
        fixture_plugin(),
        &manifest,
        &grant,
        ScopeId::generate(),
        1,
        environment(FixtureMode::DuplicateId),
    )
    .await
    .expect("handshake succeeds");
    let first = transport
        .call("tool.read_observation", json!({}))
        .await
        .expect("call");
    assert!(first.answered().is_some());
    transport.shutdown().await;

    // An ignored cancel expires the deadline, terminates the process tree, and
    // reports an uncertain outcome instead of a synthesized success.
    let stalling = ExtensionTransport::connect(
        fixture_plugin(),
        &manifest,
        &grant,
        ScopeId::generate(),
        1,
        environment(FixtureMode::IgnoreCancel),
    )
    .await
    .expect("handshake succeeds");
    let outcome = stalling
        .call_with_deadline("tool.write_note", json!({}), 400)
        .await
        .expect("deadline is enforced");
    match outcome {
        CallOutcome::Uncertain { reason } => assert!(reason.contains("deadline")),
        other => panic!("a stalled call must be uncertain, got {other:?}"),
    }
    stalling.shutdown().await;

    // A stderr flood is bounded and never parsed as protocol.
    let noisy = ExtensionTransport::connect(
        fixture_plugin(),
        &manifest,
        &grant,
        ScopeId::generate(),
        1,
        environment(FixtureMode::FloodStderr),
    )
    .await
    .expect("handshake succeeds");
    let _ = noisy.call("tool.read_observation", json!({})).await;
    assert!(noisy.stderr_tail().len() <= MAX_STDERR_BYTES);
    noisy.shutdown().await;
}

// ---------------------------------------------------------------------------
// P6-S03 / K13
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // One end-to-end gate walk.
async fn p6_s03_external_tools_cross_the_same_policy_gate() {
    let repository = tempfile::tempdir().expect("tempdir");
    let workspace = repository.path().join("workspace");
    std::fs::create_dir_all(workspace.join("src")).expect("mkdir");
    std::fs::write(workspace.join("src/lib.rs"), "pub fn greet() {}\n").expect("write");
    for arguments in [
        vec!["init", "-q"],
        vec!["config", "user.email", "p6@localhost"],
        vec!["config", "user.name", "p6"],
        vec!["add", "-A"],
        vec!["commit", "-qm", "base"],
    ] {
        let output = std::process::Command::new("git")
            .args(&arguments)
            .current_dir(&workspace)
            .output()
            .expect("git runs");
        assert!(output.status.success());
    }

    let store = Arc::new(
        harness_store_sqlite::SqliteStore::open_writer(
            harness_store_sqlite::WriterOpenOptions::new(
                repository.path().join("data"),
                harness_types::HostId::generate(),
            ),
        )
        .await
        .expect("store opens"),
    );
    let session = harness_types::SessionId::generate();
    let task = harness_types::TaskId::generate();
    harness_session::SessionService::new(Arc::clone(&store))
        .admit_input(harness_session::AdmitInputRequest {
            session_id: session.clone(),
            task_id: task.clone(),
            input_id: harness_types::InputId::generate(),
            expected_sequence: 1,
            authority: harness_types::SourceAuthority::User,
            raw_text: "external tool gate".to_owned(),
            workspace: harness_types::WorkspaceObservation {
                project_id: harness_types::ProjectId::generate(),
                worktree_id: "p6".to_owned(),
                base_commit: "0".repeat(40),
                observed_fingerprint: harness_types::ContentHash::from_bytes(b"p6"),
            },
            initial_plan_items: Vec::new(),
        })
        .await
        .expect("admit input");

    // A real extension process, registered through the host runtime.
    let (extension_runtime, _scope) = runtime();
    let manifest = fixture_manifest();
    let grant = grant_for(&manifest, Vec::new());
    let loaded = extension_runtime
        .load(
            fixture_plugin(),
            manifest.clone(),
            Some(&grant),
            environment(FixtureMode::Normal),
        )
        .await;
    assert!(
        matches!(loaded, LoadOutcome::Active { .. }),
        "fixture extension activates: {loaded:?}"
    );

    // The external tool is reachable only through the P3 gate.
    let extension_host = Arc::new(extension_runtime);
    let service = harness_tools::ToolExecutionService::new(Arc::clone(&store)).with_external(
        Arc::new(ExtensionToolDispatcher::new(Arc::clone(&extension_host))),
    );
    let request = harness_tools::ToolRequest {
        session_id: session.clone(),
        task_id: task.clone(),
        actor_id: "p6-acceptance".to_owned(),
        invocation_id: ToolInvocationId::generate(),
        call_id: None,
        workspace_root: workspace.clone(),
        action: harness_tools::CodingToolAction::ExternalTool {
            plugin_id: manifest.plugin_id.clone(),
            tool_name: "tool.read_observation".to_owned(),
            arguments: json!({"subject": "src/lib.rs"}),
            parent_invocation_id: None,
            timeout_ms: 5_000,
        },
    };
    let prepared = service
        .prepare(request)
        .await
        .expect("gate prepares the call");

    // Without an approval the gate denies, and the denial is durable evidence.
    let denied = service
        .execute(prepared.clone(), None)
        .await
        .expect("denial is recorded, not thrown");
    let receipt = denied.receipt.expect("denial receipt");
    assert_eq!(
        receipt.outcome_state,
        harness_types::ToolOutcomeState::Denied
    );
    assert_eq!(receipt.intent_state, harness_types::ToolIntentState::Denied);
    assert!(matches!(
        denied.output,
        harness_tools::ToolOutput::Denied { .. }
    ));

    // With an approval the same prepared call crosses the gate and the plugin.
    let grant_approval = service.approve(&prepared).await.expect("approval");
    let executed = service
        .execute(prepared, Some(grant_approval))
        .await
        .expect("execution");
    let receipt = executed.receipt.expect("execution receipt");
    assert_eq!(
        receipt.outcome_state,
        harness_types::ToolOutcomeState::Settled
    );
    match executed.output {
        harness_tools::ToolOutput::ExternalTool {
            plugin_id,
            tool_name,
            payload,
            ..
        } => {
            assert_eq!(plugin_id, manifest.plugin_id);
            assert_eq!(tool_name, "tool.read_observation");
            assert_eq!(payload["observation"], "fixture observation");
        }
        other => panic!("expected an external tool output, got {other:?}"),
    }

    // A nested call retains the parent invocation for correlation and cannot
    // escalate: the same gate still requires its own approval.
    let nested_request = harness_tools::ToolRequest {
        session_id: session.clone(),
        task_id: task.clone(),
        actor_id: "p6-acceptance".to_owned(),
        invocation_id: ToolInvocationId::generate(),
        call_id: None,
        workspace_root: workspace.clone(),
        action: harness_tools::CodingToolAction::ExternalTool {
            plugin_id: manifest.plugin_id.clone(),
            tool_name: "tool.write_note".to_owned(),
            arguments: json!({"text": "nested"}),
            parent_invocation_id: Some(receipt.invocation_id.clone()),
            timeout_ms: 5_000,
        },
    };
    let nested = service
        .prepare(nested_request)
        .await
        .expect("nested prepares");
    let nested_denied = service
        .execute(nested, None)
        .await
        .expect("nested denial is recorded");
    assert_eq!(
        nested_denied.receipt.expect("nested receipt").outcome_state,
        harness_types::ToolOutcomeState::Denied
    );

    // If the extension unloads after approval, the committed dispatch intent is
    // uncertain; the gate never reports a silent success.
    let after_unload = harness_tools::ToolRequest {
        session_id: session.clone(),
        task_id: task.clone(),
        actor_id: "p6-acceptance".to_owned(),
        invocation_id: ToolInvocationId::generate(),
        call_id: None,
        workspace_root: workspace.clone(),
        action: harness_tools::CodingToolAction::ExternalTool {
            plugin_id: manifest.plugin_id.clone(),
            tool_name: "tool.read_observation".to_owned(),
            arguments: json!({}),
            parent_invocation_id: None,
            timeout_ms: 2_000,
        },
    };
    let prepared = service
        .prepare(after_unload)
        .await
        .expect("active extension validates before approval");
    let approval = service.approve(&prepared).await.expect("approval");
    extension_host.shutdown_all().await;
    let executed = service
        .execute(prepared, Some(approval))
        .await
        .expect("execution records unknown");
    assert_eq!(
        executed.receipt.expect("receipt").outcome_state,
        harness_types::ToolOutcomeState::OutcomeUnknown
    );
    assert_eq!(extension_host.live_process_count().await, 0);
    drop(service);
    Arc::try_unwrap(store)
        .expect("store released")
        .close()
        .await
        .expect("store closes");
}

#[tokio::test]
async fn p6_k13_nested_extension_call_cannot_escalate_authority() {
    // A trust grant can only ever narrow, so a nested call cannot widen what the
    // parent held.
    let manifest = fixture_manifest();
    let parent = grant_for(&manifest, Vec::new());
    let mut requested = parent.clone();
    requested.allowed_secrets = vec!["secret://p6/extra".to_owned()];
    let error = parent.intersect(&requested).unwrap_err();
    assert_eq!(error.code(), ErrorCode::SecretNotGranted);

    let mut widened = parent.clone();
    widened
        .allowed_capabilities
        .push(ExtensionCapability::ModelProvider);
    let error = parent.intersect(&widened).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ExtensionCapabilityMismatch);

    // A narrower grant is accepted.
    let mut narrower = parent.clone();
    narrower.allowed_capabilities.clear();
    let intersected = parent.intersect(&narrower).expect("narrowing is allowed");
    assert!(intersected.allowed_capabilities.is_empty());

    // A grant for another plugin cannot be reused.
    let mut foreign = parent.clone();
    foreign.plugin_id = "p6.other".to_owned();
    let error = parent.intersect(&foreign).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ExtensionUntrusted);

    // The tool-call correlation identity is a first-class type, so a nested call
    // always names its parent invocation.
    let invocation = ToolInvocationId::generate();
    let execution = ToolExecutionId::generate();
    assert_ne!(invocation.as_str(), execution.as_str());
    assert!(invocation.as_str().starts_with("tool_invocation_"));
    assert!(execution.as_str().starts_with("tool_execution_"));
}

// ---------------------------------------------------------------------------
// P6-S04
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p6_s04_mcp_client_registers_scoped_tools_and_renegotiates() {
    let scope = ScopeId::generate();
    let mut client = McpClient::connect_stdio(fixture_mcp_server(), Vec::new(), scope.clone(), 1)
        .await
        .expect("MCP handshake and discovery succeed");
    let names = client.tools().keys().cloned().collect::<Vec<_>>();
    assert_eq!(names, vec!["observe".to_owned(), "write_note".to_owned()]);
    assert_eq!(client.generation(), 1);
    assert_eq!(client.scope_id(), &scope);

    // The client exposes metadata and registration; executable calls stay
    // behind the shared ToolExecutionService gate. M6 tests the approved and
    // denied calls end to end with durable intents and receipts.

    // Discovery is scoped registration: the tools land in the MCP scope, and a
    // duplicate registration in the same layer is refused.
    let mut registry = harness_kernel::ScopedRegistry::default();
    registry.add_root(scope.clone()).expect("root scope");
    let instance = PluginInstanceId::generate();
    let registered = client
        .register_tools(&mut registry, &instance)
        .expect("tools register");
    assert_eq!(registered.len(), 2);
    let resolved = registry
        .lookup(&scope, "mcp.tool:observe")
        .expect("registered tool resolves");
    assert_eq!(resolved.token.scope_id, scope);
    assert!(
        registry
            .register(&scope, "mcp.tool:observe", PluginInstanceId::generate(), 1)
            .is_err(),
        "a duplicate registration in one layer is refused"
    );

    // Schema drift is detected rather than ignored: a pinned digest that differs
    // from the advertised schema requires renegotiation before use.
    let actual = client
        .tools()
        .get("observe")
        .expect("observe descriptor")
        .digest
        .clone();
    let mut pinned = std::collections::BTreeMap::new();
    pinned.insert("observe".to_owned(), actual.clone());
    pinned.insert(
        "write_note".to_owned(),
        harness_types::ContentHash::from_bytes(b"stale-schema"),
    );
    let drifted = client.schema_drift(&pinned);
    assert_eq!(drifted, vec!["write_note".to_owned()]);
    let mut matching = std::collections::BTreeMap::new();
    matching.insert("observe".to_owned(), actual);
    matching.insert(
        "write_note".to_owned(),
        client.tools()["write_note"].digest.clone(),
    );
    assert!(client.schema_drift(&matching).is_empty());

    client.close().await;
}

// ---------------------------------------------------------------------------
// P6-S05 / K14
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p6_s05_skills_are_versioned_data_and_compose_by_precedence() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../harness-extensions/fixtures/skills");
    let project = discover_skills(&root, SkillSource::TrustedProject).expect("discover");
    assert_eq!(project.len(), 2);
    let mut names = project
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, vec!["observe-source", "verify-revision"]);
    for skill in &project {
        assert!(!skill.version.is_empty(), "a skill declares a version");
        assert!(!skill.content.is_empty());
    }

    // A higher-precedence source with the same name replaces a lower one, and
    // the replacement is reported rather than silent.
    let mut profile = project.clone();
    for skill in &mut profile {
        skill.source = SkillSource::Profile;
        skill.version = "99".to_owned();
    }
    let (composed, replaced) = compose_skills(&[project.clone(), profile.clone()].concat());
    assert_eq!(composed.len(), 2);
    assert!(
        composed
            .iter()
            .all(|skill| skill.source == SkillSource::Profile),
        "the profile source wins"
    );
    assert_eq!(replaced.len(), 2);
    assert!(replaced.iter().all(|(_, loser, winner)| {
        *loser == SkillSource::TrustedProject && *winner == SkillSource::Profile
    }));

    // Skill text is data: it cannot grant a tool or a secret by asking.
    let asking = harness_extensions::SkillDescriptor {
        requested_tools: vec!["run_shell".to_owned()],
        requested_secrets: vec!["secret://p6/forbidden".to_owned()],
        ..project[0].clone()
    };
    let error = asking.authorize(&[], &[]).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ExtensionCapabilityMismatch);
    let error = asking
        .authorize(&["run_shell".to_owned()], &[])
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::SecretNotGranted);
    let granted = asking
        .authorize(
            &["run_shell".to_owned()],
            &["secret://p6/forbidden".to_owned()],
        )
        .expect("an explicit grant authorizes the request");
    assert_eq!(granted, vec!["run_shell".to_owned()]);

    // A version pin reports drift instead of silently using the new content.
    let pin = harness_extensions::SkillVersionPin {
        name: project[0].name.clone(),
        version: project[0].version.clone(),
        digest: project[0].digest.clone(),
    };
    pin.check(&project[0])
        .expect("an unchanged skill matches its pin");
    let mut edited = project[0].clone();
    edited.version = "2".to_owned();
    let error = pin.check(&edited).unwrap_err();
    assert_eq!(error.code(), ErrorCode::SchemaVersionMismatch);
    let update = harness_extensions::SkillUpdate::detect(&pin, &edited, 7)
        .expect("detect")
        .expect("an update is detected");
    assert_eq!(update.visible_at_step, 7);
    assert_eq!(update.from_version, pin.version);
    assert_eq!(update.to_version, "2".to_owned());
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One trust-boundary walk.
async fn p6_k14_untrusted_profile_cannot_load_executables_or_secrets() {
    let profile = manifest_for(
        "p6.untrusted.profile",
        harness_extensions::executable_digest(&fixture_plugin()).expect("digest"),
        vec![ExtensionCapability::Tools],
        vec!["secret://p6/required".to_owned()],
        Vec::new(),
    );

    // Opening a repository profile performs no execution and resolves no secret.
    let directory = tempfile::tempdir().expect("tempdir");
    let path = support::write_manifest(directory.path(), &profile);
    let inspection = harness_extensions::ConfigInspection::from_manifest(
        &harness_extensions::host::read_manifest(&path).expect("manifest reads"),
    );
    assert!(inspection.requires_user_trust);
    assert_eq!(inspection.plugin_id, "p6.untrusted.profile");
    assert_eq!(
        inspection.requested_secrets,
        vec!["secret://p6/required".to_owned()]
    );
    assert!(!inspection.notes.is_empty());

    // Without a grant the runtime reports exactly why nothing started.
    let manifest = fixture_manifest();
    let (host, _scope) = runtime();
    let outcome = host
        .load(
            fixture_plugin(),
            manifest.clone(),
            None,
            environment(FixtureMode::Normal),
        )
        .await;
    match outcome {
        LoadOutcome::Inactive {
            reason,
            plugin_id,
            detail,
        } => {
            assert_eq!(plugin_id, manifest.plugin_id);
            assert_eq!(reason, InactiveReason::NoTrustGrant);
            assert!(!detail.is_empty());
        }
        LoadOutcome::Active { .. } => panic!("an untrusted plugin must not start"),
    }
    assert_eq!(host.live_process_count().await, 0);

    // A grant that does not cover the requested secret refuses at the boundary,
    // before any process is started.
    let with_secret = manifest_for(
        &manifest.plugin_id,
        manifest.executable_digest.clone(),
        vec![ExtensionCapability::Tools],
        vec!["secret://p6/required".to_owned()],
        Vec::new(),
    );
    let insufficient = grant_for(&with_secret, Vec::new());
    let failed = host
        .load(
            fixture_plugin(),
            with_secret.clone(),
            Some(&insufficient),
            environment(FixtureMode::Normal),
        )
        .await;
    match failed {
        LoadOutcome::Inactive { reason, .. } => {
            assert_eq!(reason, InactiveReason::SecretNotGranted);
        }
        LoadOutcome::Active { .. } => panic!("an ungranted secret must not start the plugin"),
    }
    assert_eq!(host.live_process_count().await, 0);

    // A grant with a different digest cannot start the pinned executable. The
    // mismatch is refused before a process exists.
    let mut wrong_digest = grant_for(&manifest, Vec::new());
    wrong_digest.executable_digest = harness_types::ContentHash::from_bytes(b"other-build");
    let failed = host
        .load(
            fixture_plugin(),
            manifest.clone(),
            Some(&wrong_digest),
            environment(FixtureMode::Normal),
        )
        .await;
    match failed {
        LoadOutcome::Inactive { reason, .. } => {
            assert_eq!(reason, InactiveReason::DigestMismatch);
        }
        LoadOutcome::Active { .. } => panic!("a digest mismatch must not start the plugin"),
    }
    assert_eq!(host.live_process_count().await, 0);

    // A manifest whose plugin id differs from the running plugin is refused
    // during the handshake, which is also a typed, non-retrying refusal.
    let aliased = manifest_for(
        "p6.untrusted.profile",
        manifest.executable_digest.clone(),
        vec![ExtensionCapability::Tools],
        Vec::new(),
        Vec::new(),
    );
    let failed = host
        .load(
            fixture_plugin(),
            aliased.clone(),
            Some(&grant_for(&aliased, Vec::new())),
            environment(FixtureMode::Normal),
        )
        .await;
    match failed {
        LoadOutcome::Inactive {
            reason, plugin_id, ..
        } => {
            assert_eq!(plugin_id, "p6.untrusted.profile");
            assert_eq!(reason, InactiveReason::DigestMismatch);
        }
        LoadOutcome::Active { .. } => panic!("an aliased plugin identity must not activate"),
    }
    assert_eq!(host.live_process_count().await, 0);

    // Inventory reports inactive entries and their reasons honestly.
    let inventory = host.inventory().await;
    assert!(
        !inventory.is_empty(),
        "refusals are remembered: {inventory:?}"
    );
    assert!(inventory.iter().any(|entry| entry.state == "inactive"));
    assert!(
        inventory
            .iter()
            .any(|entry| entry.inactive_reason.is_some())
    );

    // The fixture never receives a real secret: the host places none, and the
    // plugin reports only whether such a variable exists.
    let loaded = host
        .load(
            fixture_plugin(),
            manifest.clone(),
            Some(&grant_for(&manifest, Vec::new())),
            environment(FixtureMode::Normal),
        )
        .await;
    assert!(matches!(loaded, LoadOutcome::Active { .. }), "{loaded:?}");
    let transport = host
        .transport(&manifest.plugin_id)
        .await
        .expect("active transport");
    let outcome = transport
        .call("tool.write_note", json!({"text": "no secret here"}))
        .await
        .expect("call");
    let payload = outcome.answered().expect("answer");
    assert_eq!(
        payload["secret_present"],
        json!(false),
        "no secret value may be placed in a fixture environment"
    );
    host.shutdown_all().await;
    assert_eq!(host.live_process_count().await, 0);
}

// ---------------------------------------------------------------------------
// P6-S06
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // One lifecycle walk over real processes.
async fn p6_s06_unload_keeps_evidence_and_reload_uses_a_new_generation() {
    let (host, scope) = runtime();
    let manifest = fixture_manifest();
    let grant = grant_for(&manifest, Vec::new());

    let first = host
        .load(
            fixture_plugin(),
            manifest.clone(),
            Some(&grant),
            environment(FixtureMode::Normal),
        )
        .await;
    let LoadOutcome::Active {
        generation: first_generation,
        ..
    } = first
    else {
        panic!("first load must activate: {first:?}");
    };
    assert_eq!(first_generation, 1);
    assert_eq!(host.live_process_count().await, 1);

    // The active extension is registered in the real scope.
    let transport = host
        .transport(&manifest.plugin_id)
        .await
        .expect("active transport");
    let instance = transport.instance_id().clone();
    let session_before = host
        .session(&manifest.plugin_id)
        .await
        .expect("negotiated session");

    // Unload removes the process, its registrations and its inventory entry:
    // unloading is a deliberate host action, so nothing stale is reported.
    assert!(host.unload(&manifest.plugin_id).await);
    let after_unload = host.inventory().await;
    assert!(
        after_unload
            .iter()
            .all(|entry| entry.plugin_id != manifest.plugin_id),
        "an unloaded extension is not reported as known: {after_unload:?}"
    );
    assert_eq!(host.live_process_count().await, 0);
    transport.shutdown().await;

    // Reload creates a strictly newer generation with a new instance identity,
    // so a late disposer from the first generation cannot remove it.
    let reloaded = host
        .reload(
            &manifest.plugin_id,
            Some(&grant),
            &environment(FixtureMode::Normal),
        )
        .await
        .expect("reload runs");
    let LoadOutcome::Active {
        generation: second_generation,
        ..
    } = reloaded
    else {
        panic!("reload must activate: {reloaded:?}");
    };
    assert!(
        second_generation > first_generation,
        "a reload must produce a strictly newer generation"
    );
    let second_transport = host
        .transport(&manifest.plugin_id)
        .await
        .expect("reloaded transport");
    assert_ne!(second_transport.instance_id(), &instance);
    assert_eq!(
        second_transport.session().plugin_id,
        session_before.plugin_id,
        "the plugin identity is stable across generations"
    );
    assert_eq!(second_transport.scope_id(), &scope);

    // Unloading a plugin that is not loaded is reported, not silently ignored.
    assert!(!host.unload("p6.never.loaded").await);

    host.shutdown_all().await;
    assert_eq!(host.live_process_count().await, 0);
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One predecessor-regression walk over real processes.
async fn p6_s06_extension_paths_preserve_predecessor_contracts() {
    let manifest = fixture_manifest();
    let grant = grant_for(&manifest, Vec::new());

    // K01: an absent executable fails before any admission, with a typed error.
    let error = ExtensionTransport::connect(
        std::path::Path::new("definitely-missing-p6-plugin"),
        &manifest,
        &grant,
        ScopeId::generate(),
        1,
        environment(FixtureMode::Normal),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ExtensionNotFound);

    // K03: a failed activation leaves no live process behind.
    let (host, _scope) = runtime();
    let failed = host
        .load(
            std::path::Path::new("definitely-missing-p6-plugin"),
            manifest.clone(),
            Some(&grant),
            environment(FixtureMode::Normal),
        )
        .await;
    assert!(matches!(failed, LoadOutcome::Inactive { .. }));
    assert_eq!(host.live_process_count().await, 0);

    // A handshake the host refuses also leaves nothing running.
    let rejected = host
        .load(
            fixture_plugin(),
            manifest.clone(),
            Some(&grant),
            environment(FixtureMode::BadProtocol),
        )
        .await;
    match rejected {
        LoadOutcome::Inactive { reason, .. } => {
            assert_eq!(reason, InactiveReason::HandshakeRejected);
        }
        LoadOutcome::Active { .. } => panic!("a refused handshake must not activate"),
    }
    assert_eq!(host.live_process_count().await, 0);

    // K05: a stale registration token cannot undo the current generation.
    let mut registry = harness_kernel::ScopedRegistry::default();
    let scope = ScopeId::generate();
    registry.add_root(scope.clone()).expect("root");
    let first_instance = PluginInstanceId::generate();
    let stale = registry
        .register(
            &scope,
            "extension.tools:p6.fixture.tools",
            first_instance,
            1,
        )
        .expect("first registration");
    let second_instance = PluginInstanceId::generate();
    registry.undo(&stale);
    let current = registry
        .register(
            &scope,
            "extension.tools:p6.fixture.tools",
            second_instance.clone(),
            2,
        )
        .expect("second registration");
    assert!(
        !registry.undo(&stale),
        "a retired generation cannot remove the replacement"
    );
    assert_eq!(
        registry
            .lookup(&scope, "extension.tools:p6.fixture.tools")
            .expect("still registered")
            .token,
        current
    );

    // K11: a plugin that crashes after an effect leaves the call uncertain and
    // never authorizes a blind retry.
    let crashing = ExtensionTransport::connect(
        fixture_plugin(),
        &manifest,
        &grant,
        ScopeId::generate(),
        1,
        environment(FixtureMode::CrashAfterEffect),
    )
    .await;
    match crashing {
        Ok(transport) => {
            let outcome = transport
                .call_with_deadline("tool.write_note", json!({}), 1_500)
                .await
                .expect("call is sent");
            assert!(
                outcome.answered().is_some() || outcome.is_uncertain(),
                "a crash around an effect is either answered or explicitly uncertain, got {outcome:?}"
            );
            transport.shutdown().await;
        }
        Err(error) => {
            // The fixture may crash during the handshake itself, which is also a
            // bounded, typed refusal rather than a hang.
            assert!(
                matches!(
                    error.code(),
                    ErrorCode::ExtensionProtocolError | ErrorCode::ExtensionProtocolUnsupported
                ),
                "a crashing plugin must fail closed: {error}"
            );
        }
    }

    // The provider bridge normalizes only the shapes the runtime already knows.
    let normalized = ExtensionProvider::normalize_stream(&json!({
        "events": [
            {"kind": "started"},
            {"kind": "text_delta", "text": "hello"},
            {"kind": "completed", "stop_reason": "stop"}
        ]
    }))
    .expect("a valid stream normalizes");
    assert_eq!(normalized.len(), 3);
    let error = ExtensionProvider::normalize_stream(&json!({
        "events": [{"kind": "invented_event"}]
    }))
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ProviderProtocol);
    let error = ExtensionProvider::normalize_stream(&json!({})).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ProviderProtocol);
}

// ---------------------------------------------------------------------------
// P6-S07
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p6_s07_fixture_extension_installs_registers_and_unloads() {
    // The CLI inspects and reports capabilities without starting anything.
    let capabilities = support::run_cli(&["extensions", "capabilities", "--json"]);
    assert!(capabilities.status.success(), "{capabilities:?}");
    let parsed: serde_json::Value =
        serde_json::from_slice(&capabilities.stdout).expect("capabilities json");
    assert_eq!(parsed["extension_protocol_versions"], json!([1]));
    assert_eq!(parsed["host_methods"].as_array().expect("methods").len(), 4);
    assert!(
        parsed["unsupported"]
            .as_array()
            .expect("unsupported")
            .iter()
            .any(|value| value == "marketplace"),
        "unsupported surfaces are declared, not implied"
    );

    // A manifest is inspectable without running the plugin.
    let directory = tempfile::tempdir().expect("tempdir");
    let manifest = fixture_manifest();
    let path = support::write_manifest(directory.path(), &manifest);
    let inspect = support::run_cli(&[
        "extensions",
        "inspect",
        "--manifest",
        &path.to_string_lossy(),
        "--json",
    ]);
    assert!(inspect.status.success(), "{inspect:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&inspect.stdout).expect("inspect json");
    assert_eq!(parsed["plugin_id"], manifest.plugin_id);
    assert_eq!(parsed["started"], json!(false));
    assert_eq!(parsed["requires_user_trust"], json!(true));

    // A dry run reports the exact missing grants and starts nothing.
    let dry_run = support::run_cli(&[
        "extensions",
        "register",
        "--manifest",
        &path.to_string_lossy(),
        "--executable",
        &fixture_plugin().to_string_lossy(),
        "--json",
    ]);
    assert!(dry_run.status.success(), "{dry_run:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&dry_run.stdout).expect("dry run json");
    assert_eq!(parsed["started"], json!(false));
    assert_eq!(
        parsed["missing_capabilities"],
        json!([ExtensionCapability::Tools.as_str()])
    );

    // A confirmed registration with a trust file activates through a real
    // process, then unloads it.
    let trust_path = directory.path().join("trust.json");
    let trust = grant_for(&manifest, Vec::new());
    std::fs::write(
        &trust_path,
        serde_json::to_vec_pretty(&trust).expect("trust serializes"),
    )
    .expect("trust writes");
    let registered = support::run_cli(&[
        "extensions",
        "register",
        "--manifest",
        &path.to_string_lossy(),
        "--executable",
        &fixture_plugin().to_string_lossy(),
        "--trust",
        &trust_path.to_string_lossy(),
        "--confirm",
        "--json",
    ]);
    assert!(registered.status.success(), "{registered:?}");
    let parsed: serde_json::Value =
        serde_json::from_slice(&registered.stdout).expect("register json");
    assert_eq!(parsed["state"], "active");
    assert_eq!(parsed["generation"], 1);

    // Skills in the repository are discoverable and grant nothing by existing.
    let skills_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../harness-extensions/fixtures/skills");
    let skills = support::run_cli(&[
        "extensions",
        "skills",
        "--directory",
        &skills_dir.to_string_lossy(),
        "--json",
    ]);
    assert!(skills.status.success(), "{skills:?}");
    let parsed: serde_json::Value = serde_json::from_slice(&skills.stdout).expect("skills json");
    assert_eq!(parsed["skill_count"], 2);
    assert_eq!(parsed["grants_resolved"], 0);
}

// ---------------------------------------------------------------------------
// Cross-cutting: the host never inherits its whole environment
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p6_extension_processes_receive_a_minimal_environment() {
    // The transport clears the environment and then adds only the allowlisted
    // names plus explicit overrides. The plugin reports the names it actually
    // received, so the claim is checked against a real process rather than the
    // host's own environment.
    let manifest = fixture_manifest();
    let grant = grant_for(&manifest, Vec::new());
    let transport = ExtensionTransport::connect(
        fixture_plugin(),
        &manifest,
        &grant,
        ScopeId::generate(),
        1,
        environment(FixtureMode::Normal),
    )
    .await
    .expect("handshake");
    let outcome = transport.call("env.names", json!({})).await.expect("call");
    let payload = outcome.answered().expect("answer");
    let names = payload["names"]
        .as_array()
        .expect("names array")
        .iter()
        .filter_map(|value| value.as_str())
        .collect::<Vec<_>>();

    // Exactly the allowlist plus the one override the host chose to pass.
    for name in &names {
        assert!(
            harness_extensions::ENVIRONMENT_ALLOWLIST.contains(name) || *name == "P6_FIXTURE_MODE",
            "the plugin received an unlisted variable: {name}"
        );
    }
    assert!(
        names.contains(&"P6_FIXTURE_MODE"),
        "an explicit override must reach the plugin"
    );
    assert!(
        !names.iter().any(|name| name.contains("SECRET")
            || name.contains("TOKEN")
            || name.contains("KEY")
            || name.contains("PASSWORD")),
        "no secret-bearing variable may reach a plugin: {names:?}"
    );
    transport.shutdown().await;

    // A plugin whose manifest asks for a non-allowlisted name is refused
    // outright, which is the only path that could have leaked one.
    let mut asking = manifest.clone();
    asking.requested_environment = vec!["P6_HOST_ONLY_MARKER".to_owned()];
    let error = asking.validate().unwrap_err();
    assert_eq!(error.code(), ErrorCode::EnvironmentDenied);
}

/// The provider bridge refuses to run when its extension is not active.
#[tokio::test]
async fn p6_extension_provider_requires_an_active_capability() {
    let (host, _scope) = runtime();
    let provider = ExtensionProvider::new(Arc::new(host), "p6.absent").expect("provider");
    let request = harness_providers::ProviderRequest::new(
        harness_types::RequestId::generate(),
        "fixture-model-1",
        vec![harness_providers::ProviderMessage::new(
            harness_providers::MessageRole::User,
            "hello",
        )],
    );
    let error = harness_providers::ModelProvider::stream(
        &provider,
        request,
        harness_providers::CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ServiceUnavailable);

    // An empty plugin id is refused at construction.
    assert_code(
        ExtensionProvider::new(Arc::new(runtime().0), "   ").map(|_| ()),
        ErrorCode::InvalidPayload,
    );
}

/// An extension error always carries a stable machine-readable code.
#[test]
fn p6_extension_errors_are_typed() {
    let error = ExtensionError::new(ErrorCode::FrameLimitExceeded, "too large");
    assert_eq!(error.code(), ErrorCode::FrameLimitExceeded);
    assert!(error.to_string().contains("frame_limit_exceeded"));
    assert_eq!(error.message(), "too large");
}

/// A trust grant validates its own shape.
#[test]
fn p6_trust_grants_require_a_principal() {
    let manifest = fixture_manifest();
    let mut grant: TrustGrant = grant_for(&manifest, Vec::new());
    grant.granted_by = String::new();
    assert_code(grant.validate(), ErrorCode::InvalidPayload);
}
