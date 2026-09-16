//! Shared fixtures for the P6 acceptance target.
//!
//! Extension behaviour is exercised through **real processes**: the fixture
//! plugin and the fixture MCP server are compiled binaries from this repository.
//! Only a paid model API or a remote MCP endpoint would be out of scope, and
//! neither is exercised.

use std::{
    path::{Path, PathBuf},
    process::Output,
};

use harness_extensions::{
    ExtensionCapability, ExtensionError, ExtensionManifest, ExtensionRuntime, RestartPolicy,
    TrustGrant, executable_digest,
};
use harness_kernel::ScopedRegistry;
use harness_types::{ContentHash, ScopeId};

/// Hostile or degraded behaviours the fixture plugin can reproduce.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixtureMode {
    Normal,
    MalformedFrame,
    OversizeFrame,
    DuplicateId,
    FloodStderr,
    IgnoreCancel,
    BadProtocol,
    UnknownCapability,
    DeniedHostMethod,
    ExitBeforeHandshake,
    CrashAfterEffect,
}

impl FixtureMode {
    #[must_use]
    pub const fn as_env(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::MalformedFrame => "malformed_frame",
            Self::OversizeFrame => "oversize_frame",
            Self::DuplicateId => "duplicate_id",
            Self::FloodStderr => "flood_stderr",
            Self::IgnoreCancel => "ignore_cancel",
            Self::BadProtocol => "bad_protocol",
            Self::UnknownCapability => "unknown_capability",
            Self::DeniedHostMethod => "denied_host_method",
            Self::ExitBeforeHandshake => "exit_before_handshake",
            Self::CrashAfterEffect => "crash_after_effect",
        }
    }
}

/// Path to the compiled fixture plugin.
#[must_use]
pub fn fixture_plugin() -> PathBuf {
    binary(
        "p6_fixture_plugin",
        option_env!("CARGO_BIN_EXE_p6_fixture_plugin"),
    )
}

/// Path to the compiled MCP fixture server.
#[must_use]
pub fn fixture_mcp_server() -> PathBuf {
    binary(
        "p6_fixture_mcp_server",
        option_env!("CARGO_BIN_EXE_p6_fixture_mcp_server"),
    )
}

/// Resolve a fixture executable.
///
/// The fixtures are binaries of the `harness-cli` crate, so the compile-time
/// environment variables resolve the real artifact instead of a path guess.
fn binary(name: &str, exported: Option<&str>) -> PathBuf {
    if let Some(path) = exported {
        let candidate = PathBuf::from(path);
        assert!(
            candidate.is_file(),
            "compiled fixture binary missing at {}",
            candidate.display()
        );
        return candidate;
    }
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    let candidate = path.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(
        candidate.is_file(),
        "compiled fixture binary missing at {}",
        candidate.display()
    );
    candidate
}

/// A manifest that pins the fixture plugin's real digest.
pub fn fixture_manifest() -> ExtensionManifest {
    manifest_for(
        "p6.fixture.tools",
        executable_digest(&fixture_plugin()).expect("fixture digest"),
        vec![ExtensionCapability::Tools],
        Vec::new(),
        Vec::new(),
    )
}

/// A manifest with explicit secrets, environment and host method requests.
pub fn manifest_for(
    plugin_id: &str,
    digest: ContentHash,
    capabilities: Vec<ExtensionCapability>,
    requested_secrets: Vec<String>,
    requested_host_methods: Vec<String>,
) -> ExtensionManifest {
    ExtensionManifest {
        schema_version: harness_extensions::EXTENSION_PROTOCOL_VERSION,
        plugin_id: plugin_id.to_owned(),
        implementation_version: "0.1.0".to_owned(),
        executable_digest: digest,
        host_api_version: 1,
        config_schema_version: 1,
        provides: capabilities
            .into_iter()
            .map(|capability| harness_extensions::CapabilityOffer {
                capability,
                api_version: 1,
            })
            .collect(),
        requires: Vec::new(),
        requested_host_methods,
        requested_secrets,
        requested_environment: Vec::new(),
        restart_policy: RestartPolicy::Manual,
        blocks_recovery_when_absent: false,
    }
}

/// A trust grant for a manifest, optionally allowing secrets.
pub fn grant_for(manifest: &ExtensionManifest, allow_secrets: Vec<String>) -> TrustGrant {
    TrustGrant {
        plugin_id: manifest.plugin_id.clone(),
        executable_digest: manifest.executable_digest.clone(),
        allowed_capabilities: manifest
            .provides
            .iter()
            .map(|offer| offer.capability)
            .collect(),
        allowed_secrets: allow_secrets,
        granted_by: "p6-acceptance".to_owned(),
    }
}

/// A runtime rooted at a fresh scope.
#[must_use]
pub fn runtime() -> (ExtensionRuntime, ScopeId) {
    let scope_id = ScopeId::generate();
    let mut registry = ScopedRegistry::default();
    registry.add_root(scope_id.clone()).expect("scope root");
    (ExtensionRuntime::new(scope_id.clone(), registry), scope_id)
}

/// Environment variables that switch the fixture into one mode.
#[must_use]
pub fn environment(mode: FixtureMode) -> harness_extensions::EnvironmentOverrides {
    let mut environment = harness_extensions::EnvironmentOverrides::new();
    environment.insert("P6_FIXTURE_MODE".to_owned(), mode.as_env().to_owned());
    environment
}

/// Run the compiled `ha` binary.
#[must_use]
pub fn run_cli(arguments: &[&str]) -> Output {
    let binary = binary_ha();
    std::process::Command::new(binary)
        .args(arguments)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("ha binary runs")
}

fn binary_ha() -> PathBuf {
    binary("ha", option_env!("CARGO_BIN_EXE_ha"))
}

/// Write a manifest JSON file into a temporary directory.
pub fn write_manifest(directory: &Path, manifest: &ExtensionManifest) -> PathBuf {
    let path = directory.join("manifest.json");
    let bytes = serde_json::to_vec_pretty(manifest).expect("manifest serializes");
    std::fs::write(&path, bytes).expect("manifest writes");
    path
}

/// Assert one extension error carries an expected code.
pub fn assert_code(result: Result<(), ExtensionError>, code: harness_types::ErrorCode) {
    match result {
        Ok(()) => panic!("expected {code:?}, got success"),
        Err(error) => assert_eq!(error.code(), code, "unexpected error: {error}"),
    }
}
