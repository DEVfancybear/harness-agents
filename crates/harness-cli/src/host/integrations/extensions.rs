//! Local extensions in a chat turn.
//!
//! P6 gives the harness a protocol for trusted local extensions, a CLI that can
//! inspect and register them, and a dispatcher that routes an external tool through
//! the same gate as a built-in one. What it deliberately does **not** have is tool
//! discovery: a manifest declares the coarse `tools` capability and nothing about the
//! names inside it, and there is no `tools.list` host method. The host therefore owns
//! what the model may see, which is the safe direction — an extension cannot announce
//! itself to the model — and it is what this module implements.
//!
//! Extensions are off unless `HA_EXTENSIONS=on`. With it on, one turn:
//!
//! - reads every installation under the extensions root, verifies the executable
//!   digest against both the manifest and the trust grant, and starts only what is
//!   trusted; anything else is reported with its reason and never started;
//! - advertises the tools those installations declare to the model as
//!   `plugin__<plugin>__<tool>`, and resolves exactly those names back into
//!   `CodingToolAction::ExternalTool`, so the call still crosses policy, approval,
//!   durable intent and receipt;
//! - shuts the extension processes down at the end of the turn.
//!
//! Nothing here can authorize anything: it decides visibility, never permission.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harness_extensions::{
    EnvironmentOverrides, ExtensionRuntime, ExtensionToolDispatcher, LoadOutcome, TrustGrant,
    executable_digest, host::read_manifest,
};
use harness_kernel::ScopedRegistry;
use harness_tools::{CodingToolAction, ExternalToolCatalog, ExternalToolDispatcher, ExternalTools};
use harness_types::{ErrorCode, HarnessError, ScopeId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::paths::LaunchEnvironment;

/// Environment variable that switches local extensions on; only the value `on` counts.
pub const EXTENSIONS_VARIABLE: &str = "HA_EXTENSIONS";

/// Environment variable naming the directory that holds installations.
pub const EXTENSIONS_ROOT_VARIABLE: &str = "HA_EXTENSIONS_ROOT";

/// Longest external tool call a host advertises.
const MAX_TOOL_TIMEOUT_MS: u64 = 120_000;

/// Default timeout for a declared tool that does not state one.
const DEFAULT_TOOL_TIMEOUT_MS: u64 = 10_000;

/// Whether the environment asks for local extensions.
///
/// Only the exact value `on` counts: an unknown value must not start a process.
#[must_use]
pub fn extensions_requested(value: Option<&str>) -> bool {
    matches!(value, Some(value) if value.eq_ignore_ascii_case("on"))
}

/// Read [`EXTENSIONS_VARIABLE`] from the injected launch environment.
#[must_use]
pub fn extensions_requested_from_environment(environment: &LaunchEnvironment) -> bool {
    extensions_requested(
        environment
            .value(EXTENSIONS_VARIABLE)
            .and_then(|value| value.to_str()),
    )
}

/// The directory installations are read from.
#[must_use]
pub fn extensions_root(environment: &LaunchEnvironment, data_dir: &Path) -> PathBuf {
    environment
        .value(EXTENSIONS_ROOT_VARIABLE)
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .map_or_else(|| data_dir.join("extensions"), PathBuf::from)
}

/// One tool an installation declares, and the schema the model is shown.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledTool {
    /// Method name the extension answers, e.g. `tool.read_observation`.
    pub name: String,
    pub description: String,
    /// JSON schema of the arguments; it describes input, it grants nothing.
    pub parameters: Value,
    #[serde(default = "default_tool_timeout")]
    pub timeout_ms: u64,
}

const fn default_tool_timeout() -> u64 {
    DEFAULT_TOOL_TIMEOUT_MS
}

/// One local installation: what to start, what trust it has, what it may offer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionInstallation {
    pub schema_version: u16,
    pub plugin_id: String,
    /// Paths are relative to the installation directory.
    pub manifest: String,
    pub executable: String,
    pub trust: String,
    /// Tools this host is willing to advertise. An empty list starts nothing: a
    /// plugin that offers no tool has nothing to do in a chat turn.
    pub tools: Vec<InstalledTool>,
}

impl ExtensionInstallation {
    fn validate(&self) -> Result<(), HarnessError> {
        if self.schema_version != 1 {
            return Err(HarnessError::new(
                ErrorCode::UnsupportedSchemaVersion,
                format!(
                    "extension installation schema {} is not supported",
                    self.schema_version
                ),
            ));
        }
        if self.plugin_id.trim().is_empty() {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "an extension installation requires a plugin id",
            ));
        }
        if self.tools.is_empty() {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                format!(
                    "extension installation {} declares no tool to advertise",
                    self.plugin_id
                ),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for tool in &self.tools {
            if tool.name.trim().is_empty() || tool.description.trim().is_empty() {
                return Err(HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "an advertised tool requires a name and a description",
                ));
            }
            if !seen.insert(tool.name.clone()) {
                return Err(HarnessError::new(
                    ErrorCode::InvalidPayload,
                    format!("tool {} is declared twice", tool.name),
                ));
            }
            if tool.timeout_ms == 0 || tool.timeout_ms > MAX_TOOL_TIMEOUT_MS {
                return Err(HarnessError::new(
                    ErrorCode::InvalidPayload,
                    format!(
                        "tool {} timeout must be 1..={MAX_TOOL_TIMEOUT_MS} ms",
                        tool.name
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// Read one installation file.
pub fn read_installation(path: &Path) -> Result<ExtensionInstallation, HarnessError> {
    let bytes = std::fs::read(path).map_err(|error| {
        HarnessError::new(
            ErrorCode::ConfigReadError,
            format!("cannot read {}: {error}", path.display()),
        )
    })?;
    let installation: ExtensionInstallation = serde_json::from_slice(&bytes).map_err(|_| {
        HarnessError::new(
            ErrorCode::ConfigParseError,
            format!("{} is not a valid installation file", path.display()),
        )
    })?;
    installation.validate()?;
    Ok(installation)
}

/// A tool a running extension may be asked for.
#[derive(Clone, Debug)]
struct AdvertisedTool {
    plugin_id: String,
    tool_name: String,
    description: String,
    parameters: Value,
    timeout_ms: u64,
}

/// The host's catalogue: what the model is shown, and what a shown name means.
///
/// Built from installations whose executable digest matched their trust grant, so a
/// name can only be advertised for a plugin the user trusted.
pub struct ExtensionCatalog {
    advertised: BTreeMap<String, AdvertisedTool>,
}

impl ExtensionCatalog {
    fn from_installations(installations: &[ExtensionInstallation]) -> Self {
        let mut advertised = BTreeMap::new();
        for installation in installations {
            for tool in &installation.tools {
                advertised.insert(
                    advertised_name(&installation.plugin_id, &tool.name),
                    AdvertisedTool {
                        plugin_id: installation.plugin_id.clone(),
                        tool_name: tool.name.clone(),
                        description: tool.description.clone(),
                        parameters: tool.parameters.clone(),
                        timeout_ms: tool.timeout_ms,
                    },
                );
            }
        }
        Self { advertised }
    }

    /// The names this catalogue advertises, for a report or a test.
    #[must_use]
    pub fn tool_names(&self) -> Vec<String> {
        self.advertised.keys().cloned().collect()
    }

    #[must_use]
    pub fn plugin_ids(&self) -> Vec<String> {
        let mut ids = self
            .advertised
            .values()
            .map(|tool| tool.plugin_id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        ids
    }
}

impl ExternalToolCatalog for ExtensionCatalog {
    fn schemas(&self) -> Vec<Value> {
        self.advertised
            .iter()
            .map(|(name, tool)| {
                json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        // The description says where the tool comes from, so the model
                        // can tell an extension apart from a built-in.
                        "description": format!("[extension {}] {}", tool.plugin_id, tool.description),
                        "parameters": tool.parameters,
                    },
                })
            })
            .collect()
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        let tool = self.advertised.get(name)?;
        Some(CodingToolAction::ExternalTool {
            plugin_id: tool.plugin_id.clone(),
            tool_name: tool.tool_name.clone(),
            arguments: arguments.clone(),
            parent_invocation_id: None,
            timeout_ms: tool.timeout_ms,
        })
    }
}

/// The provider-function name one advertised tool is called by.
///
/// A plugin id may contain characters a function name cannot, so the advertised name
/// is sanitized; the catalogue keeps the mapping, which is why resolution never
/// parses the name back.
fn advertised_name(plugin_id: &str, tool_name: &str) -> String {
    let sanitize = |value: &str| {
        value
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    format!("plugin__{}__{}", sanitize(plugin_id), sanitize(tool_name))
}

/// What one load did, including what it refused.
#[derive(Clone, Debug, Default)]
pub struct ExtensionsReport {
    pub plugins: Vec<String>,
    pub tools: Vec<String>,
    /// One line per installation that was not started, with its reason.
    pub refused: Vec<String>,
}

impl ExtensionsReport {
    /// The one line a turn shows its user.
    #[must_use]
    pub fn message(&self, root: &Path) -> String {
        if self.plugins.is_empty() && self.refused.is_empty() {
            return format!("extensions: no installation under {}", root.display());
        }
        let mut message = format!(
            "extensions: {} plugin(s), {} tool(s) exposed",
            self.plugins.len(),
            self.tools.len()
        );
        for refusal in &self.refused {
            message.push_str("; ");
            message.push_str(refusal);
        }
        message
    }
}

/// Trusted, started extensions of one turn.
pub struct ActiveExtensions {
    runtime: Arc<ExtensionRuntime>,
    catalog: Arc<ExtensionCatalog>,
    report: ExtensionsReport,
}

impl ActiveExtensions {
    /// The catalogue to advertise and resolve through.
    #[must_use]
    pub fn tools(&self) -> ExternalTools {
        ExternalTools::new(Arc::clone(&self.catalog) as Arc<dyn ExternalToolCatalog>)
    }

    /// The dispatcher that reaches the started processes.
    #[must_use]
    pub fn dispatcher(&self) -> Arc<dyn ExternalToolDispatcher> {
        Arc::new(ExtensionToolDispatcher::new(Arc::clone(&self.runtime)))
    }

    #[must_use]
    pub fn report(&self) -> &ExtensionsReport {
        &self.report
    }

    /// Stop every extension this turn started.
    pub async fn shutdown(self) {
        self.runtime.shutdown_all().await;
    }
}

/// Load every trusted installation under one root.
///
/// A missing root is an empty result, not an error: an operator who never installed
/// an extension should not have to create a directory. An installation that cannot be
/// trusted is refused with its reason and is never started.
pub async fn load_active(root: &Path) -> Result<ActiveExtensions, HarnessError> {
    let mut installations = Vec::new();
    let mut refused = Vec::new();
    let mut directories = Vec::new();
    match std::fs::read_dir(root) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    directories.push(entry.path());
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(HarnessError::new(
                ErrorCode::ConfigReadError,
                format!("cannot read {}: {error}", root.display()),
            ));
        }
    }
    directories.sort();

    let scope_id = ScopeId::generate();
    let mut registry = ScopedRegistry::default();
    registry
        .add_root(scope_id.clone())
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
    let runtime = Arc::new(ExtensionRuntime::new(scope_id, registry));

    let mut loaded = ExtensionsReport::default();
    for directory in directories {
        let installation_path = directory.join("installation.json");
        if !installation_path.exists() {
            continue;
        }
        match load_one(&runtime, &directory, &installation_path).await {
            Ok(installation) => installations.push(installation),
            Err(reason) => refused.push(reason),
        }
    }
    let catalog = ExtensionCatalog::from_installations(&installations);
    // The report says what is actually advertised, not what a file claimed: only a
    // trusted and started installation reaches the catalogue.
    loaded.plugins = catalog.plugin_ids();
    loaded.tools = catalog.tool_names();
    loaded.refused = refused;
    Ok(ActiveExtensions {
        runtime,
        catalog: Arc::new(catalog),
        report: loaded,
    })
}

/// Trust-check and start one installation.
async fn load_one(
    runtime: &Arc<ExtensionRuntime>,
    directory: &Path,
    installation_path: &Path,
) -> Result<ExtensionInstallation, String> {
    let label = directory.file_name().map_or_else(
        || directory.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let installation =
        read_installation(installation_path).map_err(|error| format!("{label}: {error}"))?;
    let manifest_path = directory.join(&installation.manifest);
    let manifest = read_manifest(&manifest_path)
        .map_err(|error| format!("{}: {error}", installation.plugin_id))?;
    if manifest.plugin_id != installation.plugin_id {
        return Err(format!(
            "{}: the manifest names plugin {}",
            installation.plugin_id, manifest.plugin_id
        ));
    }
    let executable = directory.join(&installation.executable);
    let actual = executable_digest(&executable)
        .map_err(|error| format!("{}: {error}", installation.plugin_id))?;
    if actual != manifest.executable_digest {
        return Err(format!(
            "{}: the executable does not match the digest its manifest pins",
            installation.plugin_id
        ));
    }
    let trust_path = directory.join(&installation.trust);
    let grant: TrustGrant = std::fs::read(&trust_path)
        .map_err(|error| {
            format!(
                "{}: cannot read the trust grant: {error}",
                installation.plugin_id
            )
        })
        .and_then(|bytes| {
            serde_json::from_slice(&bytes).map_err(|_| {
                format!(
                    "{}: the trust grant is not valid JSON",
                    installation.plugin_id
                )
            })
        })?;
    if grant.plugin_id != installation.plugin_id || grant.executable_digest != actual {
        return Err(format!(
            "{}: the trust grant does not name this executable; nothing was started",
            installation.plugin_id
        ));
    }

    match runtime
        .load(
            executable,
            manifest,
            Some(&grant),
            EnvironmentOverrides::new(),
        )
        .await
    {
        LoadOutcome::Active { .. } => Ok(installation),
        LoadOutcome::Inactive {
            plugin_id,
            reason,
            detail,
        } => Err(format!("{plugin_id}: {}: {detail}", reason.as_str())),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ActiveExtensions, EXTENSIONS_ROOT_VARIABLE, EXTENSIONS_VARIABLE, ExtensionCatalog,
        ExtensionInstallation, InstalledTool, advertised_name, extensions_requested,
        extensions_requested_from_environment, extensions_root, load_active, read_installation,
    };
    use crate::interactive::paths::LaunchEnvironment;
    use harness_tools::ExternalToolCatalog;
    use serde_json::{Value, json};

    fn installation(plugin_id: &str, tools: Vec<InstalledTool>) -> ExtensionInstallation {
        ExtensionInstallation {
            schema_version: 1,
            plugin_id: plugin_id.to_owned(),
            manifest: "manifest.json".to_owned(),
            executable: "plugin.exe".to_owned(),
            trust: "trust.json".to_owned(),
            tools,
        }
    }

    fn tool(name: &str) -> InstalledTool {
        InstalledTool {
            name: name.to_owned(),
            description: "reads an observation".to_owned(),
            parameters: json!({"type": "object", "properties": {"subject": {"type": "string"}}}),
            timeout_ms: 5_000,
        }
    }

    #[test]
    fn extensions_are_off_unless_the_exact_value_asks_for_them() {
        assert!(extensions_requested(Some("on")));
        assert!(extensions_requested(Some("ON")));
        assert!(!extensions_requested(Some("off")));
        assert!(!extensions_requested(Some("true")));
        assert!(!extensions_requested(None));
        assert!(extensions_requested_from_environment(
            &LaunchEnvironment::from_pairs([(EXTENSIONS_VARIABLE, "on")])
        ));
        assert!(!extensions_requested_from_environment(
            &LaunchEnvironment::from_pairs([("HA_UI", "plain")])
        ));
    }

    #[test]
    fn the_root_defaults_under_the_data_directory_and_can_be_overridden() {
        let data_dir = std::path::Path::new("C:/home/data");
        assert_eq!(
            extensions_root(&LaunchEnvironment::default(), data_dir),
            data_dir.join("extensions")
        );
        let environment =
            LaunchEnvironment::from_pairs([(EXTENSIONS_ROOT_VARIABLE, "C:/shared/extensions")]);
        assert_eq!(
            extensions_root(&environment, data_dir),
            std::path::PathBuf::from("C:/shared/extensions")
        );
    }

    /// The measured gap: P6 has no tool discovery, so the host declares what it shows.
    #[test]
    fn only_a_declared_tool_is_advertised_and_resolvable() {
        let catalog = ExtensionCatalog::from_installations(&[
            installation("p6.fixture", vec![tool("tool.read_observation")]),
            installation("other.plugin", vec![tool("tool.write_note")]),
        ]);

        let schemas = catalog.schemas();
        assert_eq!(schemas.len(), 2);
        let names = catalog.tool_names();
        assert_eq!(
            names,
            vec![
                advertised_name("other.plugin", "tool.write_note"),
                advertised_name("p6.fixture", "tool.read_observation"),
            ]
        );
        // A plugin id with a dot is sanitized for the wire, and the schema says where
        // the tool comes from.
        assert!(names[0].starts_with("plugin__other_plugin__"));
        assert!(
            schemas
                .iter()
                .any(|schema| schema["function"]["description"]
                    .as_str()
                    .is_some_and(|value| value.starts_with("[extension p6.fixture]"))),
            "{schemas:?}"
        );

        let advertised = advertised_name("p6.fixture", "tool.read_observation");
        let action = catalog
            .resolve(&advertised, &json!({"subject": "src/lib.rs"}))
            .expect("a declared tool resolves");
        match action {
            harness_tools::CodingToolAction::ExternalTool {
                plugin_id,
                tool_name,
                arguments,
                parent_invocation_id,
                timeout_ms,
            } => {
                assert_eq!(plugin_id, "p6.fixture");
                assert_eq!(tool_name, "tool.read_observation");
                assert_eq!(arguments["subject"], "src/lib.rs");
                assert!(parent_invocation_id.is_none());
                assert_eq!(timeout_ms, 5_000);
            }
            other => panic!("expected an external action, got {other:?}"),
        }

        // Nothing else resolves: not another plugin's tool, not a built-in name.
        assert!(
            catalog
                .resolve("read_file", &json!({"path": "src/lib.rs"}))
                .is_none()
        );
        assert!(
            catalog
                .resolve("tool.read_observation", &json!({}))
                .is_none()
        );
    }

    #[test]
    fn an_installation_without_a_usable_tool_is_refused() {
        let temp = tempfile::tempdir().expect("temp root");
        let path = temp.path().join("installation.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&installation("p6.fixture", Vec::new())).unwrap(),
        )
        .expect("fixture write");
        let error = read_installation(&path).expect_err("an empty tool list is refused");
        assert!(error.to_string().contains("declares no tool"), "{error}");

        let mut duplicate = installation("p6.fixture", vec![tool("a"), tool("a")]);
        duplicate.tools[1].name = "a".to_owned();
        std::fs::write(&path, serde_json::to_vec(&duplicate).unwrap()).expect("fixture write");
        assert!(read_installation(&path).is_err());

        let unknown = json!({
            "schema_version": 1,
            "plugin_id": "p6.fixture",
            "manifest": "manifest.json",
            "executable": "plugin.exe",
            "trust": "trust.json",
            "tools": [tool("a")],
            "surprise": true,
        });
        std::fs::write(&path, serde_json::to_vec(&unknown).unwrap()).expect("fixture write");
        let error = read_installation(&path).expect_err("an unknown field is refused");
        assert!(
            error.to_string().contains("not a valid installation"),
            "{error}"
        );
    }

    // ---------------------------------------------------------------------------
    // End to end: a model asks for an extension tool, and the real fixture plugin
    // answers through the same gate a built-in tool crosses.
    // ---------------------------------------------------------------------------

    use harness_extensions::{ExtensionCapability, ExtensionManifest, RestartPolicy, TrustGrant};
    use harness_providers::{
        CancellationToken, MessageRole, ModelCapabilities, ModelProvider, ProviderFuture,
        ProviderRequest, ProviderStreamEvent,
    };
    use harness_runtime::{RunRequest, RuntimeConfig, RuntimeService};
    use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
    use harness_tools::{
        ApprovalMode, ToolExecutionService, TurnDriver, TurnLimits, TurnObserver, TurnOptions,
        TurnProgress, TurnStop, coding_tool_schemas,
    };
    use harness_types::{HostId, InputId, ProjectId, SessionId, TaskId};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// A provider that answers with one scripted response per call and records what
    /// it was sent, so a test can read the request the model really saw.
    struct SequenceProvider {
        responses: Vec<Vec<ProviderStreamEvent>>,
        calls: AtomicUsize,
        seen: Mutex<Vec<ProviderRequest>>,
    }

    impl SequenceProvider {
        fn new(responses: Vec<Vec<ProviderStreamEvent>>) -> Self {
            Self {
                responses,
                calls: AtomicUsize::new(0),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn seen(&self) -> Vec<ProviderRequest> {
            self.seen.lock().expect("request log").clone()
        }
    }

    impl ModelProvider for SequenceProvider {
        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities::deepseek_fixture()
        }

        fn stream(
            &self,
            request: ProviderRequest,
            _cancellation: CancellationToken,
        ) -> ProviderFuture {
            self.seen.lock().expect("request log").push(request.clone());
            let index = self
                .calls
                .fetch_add(1, Ordering::SeqCst)
                .min(self.responses.len() - 1);
            let mut events = self.responses[index].clone();
            if let Some(ProviderStreamEvent::Started { request_id }) = events.first_mut() {
                *request_id = request.request_id;
            }
            Box::pin(async move { Ok(events) })
        }
    }

    struct SilentObserver;

    impl TurnObserver for SilentObserver {
        fn observe(&self, _progress: TurnProgress) {}
    }

    /// The compiled P6 fixture plugin, resolved the way the phase tests resolve it.
    fn fixture_plugin_binary() -> std::path::PathBuf {
        let mut path = std::env::current_exe().expect("test binary path");
        path.pop();
        if path.ends_with("deps") {
            path.pop();
        }
        let candidate = path.join(format!("p6_fixture_plugin{}", std::env::consts::EXE_SUFFIX));
        assert!(
            candidate.is_file(),
            "compiled fixture binary missing at {}",
            candidate.display()
        );
        candidate
    }

    fn fixture_manifest(plugin_id: &str, digest: harness_types::ContentHash) -> ExtensionManifest {
        ExtensionManifest {
            schema_version: harness_extensions::EXTENSION_PROTOCOL_VERSION,
            plugin_id: plugin_id.to_owned(),
            implementation_version: "0.1.0".to_owned(),
            executable_digest: digest,
            host_api_version: 1,
            config_schema_version: 1,
            provides: vec![harness_extensions::CapabilityOffer {
                capability: ExtensionCapability::Tools,
                api_version: 1,
            }],
            requires: Vec::new(),
            requested_host_methods: Vec::new(),
            requested_secrets: Vec::new(),
            requested_environment: Vec::new(),
            restart_policy: RestartPolicy::Manual,
            blocks_recovery_when_absent: false,
        }
    }

    /// Write one trusted installation of the fixture plugin and return its root.
    fn fixture_installation(root: &std::path::Path, plugin_id: &str) -> std::path::PathBuf {
        let directory = root.join(plugin_id);
        std::fs::create_dir_all(&directory).expect("installation directory");
        let executable_name = format!("p6_fixture_plugin{}", std::env::consts::EXE_SUFFIX);
        let executable = directory.join(&executable_name);
        std::fs::copy(fixture_plugin_binary(), &executable).expect("fixture copy");
        let digest = harness_extensions::executable_digest(&executable).expect("fixture digest");
        let manifest = fixture_manifest(plugin_id, digest.clone());
        std::fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).expect("manifest"),
        )
        .expect("manifest write");
        let grant = TrustGrant {
            plugin_id: plugin_id.to_owned(),
            executable_digest: digest,
            allowed_capabilities: vec![ExtensionCapability::Tools],
            allowed_secrets: Vec::new(),
            granted_by: "extension-test".to_owned(),
        };
        std::fs::write(
            directory.join("trust.json"),
            serde_json::to_vec(&grant).expect("grant"),
        )
        .expect("grant write");
        let installation = json!({
            "schema_version": 1,
            "plugin_id": plugin_id,
            "manifest": "manifest.json",
            "executable": executable_name,
            "trust": "trust.json",
            "tools": [{
                "name": "tool.read_observation",
                "description": "reads one fixture observation",
                "parameters": {
                    "type": "object",
                    "properties": {"subject": {"type": "string"}},
                    "required": ["subject"],
                },
                "timeout_ms": 5000,
            }],
        });
        std::fs::write(
            directory.join("installation.json"),
            serde_json::to_vec_pretty(&installation).expect("installation"),
        )
        .expect("installation write");
        directory
    }

    /// The whole chain: trusted install, advertised schema, model call, plugin answer.
    #[tokio::test]
    async fn a_model_call_to_a_trusted_extension_crosses_the_same_gate() {
        let temp = tempfile::tempdir().expect("temp root");
        let plugin_id = "p6.fixture.tools";
        let installation = fixture_installation(temp.path(), plugin_id);

        let active = load_active(temp.path())
            .await
            .expect("the installation loads");
        let advertised = advertised_name(plugin_id, "tool.read_observation");
        assert_eq!(active.report().plugins, vec![plugin_id.to_owned()]);
        assert_eq!(active.report().tools, vec![advertised.clone()]);
        assert!(active.report().refused.is_empty(), "{:?}", active.report());

        // The model is shown the built-ins plus the declared extension tool.
        let mut schemas = coding_tool_schemas();
        schemas.extend(active.tools().schemas());
        assert_eq!(schemas.len(), coding_tool_schemas().len() + 1);
        assert!(
            schemas
                .iter()
                .any(|schema| schema["function"]["name"] == advertised.as_str()),
            "{schemas:?}"
        );

        let (outcome, provider, store) =
            run_scripted_turn(temp.path(), &active, &advertised, schemas).await;

        assert_eq!(outcome.stop, TurnStop::Final, "{outcome:?}");
        assert_eq!(outcome.tool_calls, 1);
        assert_eq!(outcome.executions.len(), 1, "{:?}", outcome.executions);
        assert!(
            matches!(
                outcome.executions[0].output,
                harness_tools::ToolOutput::ExternalTool { .. }
            ),
            "the call must cross the external gate: {:?}",
            outcome.executions[0]
        );

        // What came back from the plugin process reached the model as a tool result.
        let seen = provider.seen();
        assert_eq!(seen.len(), 2, "one provider call per step");
        assert!(
            seen[1].messages.iter().any(|message| {
                message.role == MessageRole::Tool && message.content.contains("fixture observation")
            }),
            "the plugin's payload must reach the model: {:?}",
            seen[1].messages
        );
        assert!(
            seen[0]
                .tool_schemas
                .iter()
                .any(|schema| schema["function"]["name"] == advertised.as_str()),
            "the advertised extension tool must be in the request: {:?}",
            seen[0].tool_schemas
        );

        active.shutdown().await;
        Arc::try_unwrap(store)
            .expect("the turn released its store handles")
            .close()
            .await
            .expect("store closes");
        assert!(
            installation.join("installation.json").is_file(),
            "the fixture installation stays where it was written"
        );
    }

    /// Run one turn whose model first asks for `advertised`, then answers.
    async fn run_scripted_turn(
        root: &std::path::Path,
        active: &ActiveExtensions,
        advertised: &str,
        schemas: Vec<Value>,
    ) -> (
        harness_tools::TurnOutcome,
        Arc<SequenceProvider>,
        Arc<SqliteStore>,
    ) {
        let store = Arc::new(
            SqliteStore::open_writer(WriterOpenOptions::new(
                root.join("data"),
                HostId::generate(),
            ))
            .await
            .expect("store opens"),
        );
        let provider = Arc::new(SequenceProvider::new(vec![
            vec![
                ProviderStreamEvent::started(),
                ProviderStreamEvent::tool_delta(
                    "call-plugin",
                    advertised,
                    json!({"subject": "src/lib.rs"}).to_string(),
                ),
                ProviderStreamEvent::completed("tool_calls"),
            ],
            vec![
                ProviderStreamEvent::started(),
                ProviderStreamEvent::text("the extension answered"),
                ProviderStreamEvent::completed("stop"),
            ],
        ]));
        let runtime = Arc::new(RuntimeService::new(
            Arc::clone(&store),
            provider.clone(),
            RuntimeConfig::default(),
        ));
        let driver = TurnDriver::new(
            Arc::clone(&runtime),
            ToolExecutionService::new(Arc::clone(&store)).with_external(active.dispatcher()),
        )
        .with_external(active.tools());

        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let request = RunRequest::new(
            SessionId::generate(),
            TaskId::generate(),
            InputId::generate(),
            "ask the fixture extension",
            harness_tools::observe_workspace(ProjectId::generate(), &workspace)
                .expect("observation"),
        )
        .with_tool_schemas(schemas);
        let outcome = driver
            .run_turn(
                request,
                TurnOptions {
                    workspace_root: workspace,
                    actor_id: "extension-test".to_owned(),
                    approvals: ApprovalMode::Auto,
                    limits: TurnLimits::default(),
                },
                Arc::new(SilentObserver),
                CancellationToken::new(),
            )
            .await
            .expect("the turn runs");
        drop(driver);
        drop(runtime);
        (outcome, provider, store)
    }

    /// An installation whose executable no longer matches its trust is not started.
    #[tokio::test]
    async fn an_extension_whose_digest_moved_is_refused_not_started() {
        let temp = tempfile::tempdir().expect("temp root");
        let plugin_id = "p6.fixture.drifted";
        let directory = fixture_installation(temp.path(), plugin_id);
        // Tamper with the executable after the grant pinned its digest.
        let executable =
            directory.join(format!("p6_fixture_plugin{}", std::env::consts::EXE_SUFFIX));
        let mut bytes = std::fs::read(&executable).expect("fixture read");
        bytes.push(0);
        std::fs::write(&executable, bytes).expect("fixture write");

        let active = load_active(temp.path())
            .await
            .expect("a refusal is a report, not an error");
        assert!(active.report().plugins.is_empty(), "{:?}", active.report());
        assert!(active.report().tools.is_empty());
        assert_eq!(active.report().refused.len(), 1, "{:?}", active.report());
        assert!(
            active.report().refused[0].contains("does not match the digest"),
            "{:?}",
            active.report().refused
        );
        active.shutdown().await;
    }
}
