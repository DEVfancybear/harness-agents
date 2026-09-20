//! Launch context for the interactive app.
//!
//! Resolution is deliberate and cheap: read the caller directory, the project
//! identity, the Git root, the user paths and the non-secret configuration. It
//! never opens the store, never resolves a model, and never makes a network
//! call, so opening the app cannot start work or block on a provider.

use std::path::{Path, PathBuf};

use harness_types::{ContentHash, ErrorCode, HarnessError};

use super::config::{self, ConfigState};
use super::credentials::{self, CredentialSource};
use super::paths::{self, HostPlatform, LaunchEnvironment, PathRequest, ResolvedPaths};

/// Credential variables probed for presence at launch.
///
/// Only presence is checked: the value is never read, logged, stored, or placed
/// in a command history. The canonical list lives in [`credentials`], which also
/// owns the order the variables are probed in.
pub const CREDENTIAL_VARIABLES: [&str; 2] = credentials::CREDENTIAL_VARIABLES;

/// `DeepSeek`'s documented endpoint, used when the operator names no other one.
///
/// Defaulting here is not guessing: these are the values the provider publishes
/// (<https://api-docs.deepseek.com/>). A bare `DEEPSEEK_API_KEY` is therefore a
/// complete setup, while an explicit `HA_PROVIDER_ENDPOINT` or `HA_PROVIDER_MODEL`
/// still wins for another provider or another model.
pub const DEEPSEEK_ENDPOINT: &str = "https://api.deepseek.com";

/// The model `DeepSeek`'s documentation recommends for new callers.
pub const DEEPSEEK_MODEL: &str = "deepseek-flash";

/// Inputs for the launch context.
#[derive(Clone, Debug)]
pub struct LaunchRequest {
    /// Explicit project directory; a relative path resolves from the caller directory.
    pub cwd: Option<PathBuf>,
    pub caller_dir: PathBuf,
    pub platform: HostPlatform,
    pub environment: LaunchEnvironment,
    /// Explicit data directory override from the launch options.
    pub explicit_data_dir: Option<PathBuf>,
}

/// Stable identity of the opened project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectIdentity {
    pub root: PathBuf,
    pub key: String,
    pub digest: ContentHash,
}

/// Whether a provider credential source exists at launch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderState {
    /// No credential source: the app opens a setup state and dispatches nothing.
    SetupRequired { reason: String },
    /// A credential source is present; the model is resolved on the first request.
    ///
    /// The source carries the *name* of where the key lives — an environment
    /// variable or the file `/key` saved — and never the key itself.
    CredentialPresent { source: CredentialSource },
}

impl ProviderState {
    /// The source name for a rendered line; never the value.
    ///
    /// Kept as the one place that turns a state into operator-facing text, so a
    /// future header or status line cannot invent a second spelling of it.
    #[allow(
        dead_code,
        reason = "the header composes its own line; tests assert this"
    )]
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::CredentialPresent { source } => source.describe(),
            Self::SetupRequired { reason } => reason.clone(),
        }
    }
}

/// Everything the app needs to render its header and open lazily.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchContext {
    pub project: ProjectIdentity,
    pub caller_dir: PathBuf,
    pub git_root: Option<PathBuf>,
    pub paths: ResolvedPaths,
    pub config: ConfigState,
    pub provider: ProviderState,
    pub setup_required: bool,
}

impl LaunchContext {
    /// Lines rendered before the prompt, including the actual resolved paths.
    #[must_use]
    pub fn header_lines(&self) -> Vec<String> {
        let provider = match &self.provider {
            ProviderState::CredentialPresent { source } => {
                format!(
                    "credential from {}, model resolved on first request",
                    source.describe()
                )
            }
            ProviderState::SetupRequired { .. } => "setup required".to_owned(),
        };
        let git = match &self.git_root {
            Some(root) => format!("repository at {}", root.display()),
            None => "not a Git repository, Git features unavailable".to_owned(),
        };
        vec![
            format!("Harness Agents {}", env!("CARGO_PKG_VERSION")),
            format!(
                "Project: {}    Provider: {provider}",
                self.project.root.display()
            ),
            "Session: new    Mode: trusted host".to_owned(),
            format!("Git:     {git}"),
            format!(
                "Config:  {} [{}]",
                self.config.describe(),
                self.paths.config_origin.label()
            ),
            format!(
                "Data:    {} [{}]",
                self.paths.data_dir.display(),
                self.paths.data_origin.label()
            ),
            format!("Store:   {}", self.project_store_dir().display()),
        ]
    }

    /// Setup instructions when the app cannot dispatch work yet.
    #[must_use]
    pub fn setup_hint(&self) -> Option<String> {
        if !self.setup_required {
            return None;
        }
        let mut reasons = Vec::new();
        if self.config.is_first_run() {
            reasons.push(format!(
                "no configuration file yet at {}",
                self.paths.config_file.display()
            ));
        }
        if let ProviderState::SetupRequired { reason } = &self.provider {
            reasons.push(format!(
                "provider {reason}; set {} to a key kept outside this repository, or paste it here with /key",
                CREDENTIAL_VARIABLES.join(" or ")
            ));
        }
        Some(format!(
            "setup required: {}. Nothing is sent to a provider until this is resolved; /key saves an API key, /exit quits.",
            reasons.join("; ")
        ))
    }

    /// Directory that will own this project's store when the first request arrives.
    #[must_use]
    pub fn project_store_dir(&self) -> PathBuf {
        self.paths.project_data_dir(&self.project.key)
    }

    /// The context after the app saved a usable credential.
    ///
    /// `/key` is a complete setup, not half of one. The credential file is not
    /// part of the strict configuration and never will be, so the app also writes
    /// the minimal valid configuration file when none exists — otherwise the user
    /// pastes a working key and still faces a first-run gate.
    ///
    /// Only a **missing** file is written. A corrupt one is left exactly as it is:
    /// it is never silently replaced by defaults, and this returns the error the
    /// normal load reports so the app can say what is wrong with it.
    pub fn credential_saved(&self, source: CredentialSource) -> Result<Self, HarnessError> {
        let config = match std::fs::read_to_string(&self.paths.config_file) {
            Ok(_) => config::load(&self.paths.config_file)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                write_minimal_config(&self.paths.config_file)?;
                config::load(&self.paths.config_file)?
            }
            Err(error) => {
                return Err(HarnessError::new(
                    ErrorCode::ConfigReadError,
                    format!(
                        "configuration file {} could not be read: {error}",
                        self.paths.config_file.display()
                    ),
                ));
            }
        };
        let provider = ProviderState::CredentialPresent { source };
        let setup_required =
            config.is_first_run() || matches!(provider, ProviderState::SetupRequired { .. });
        let mut updated = self.clone();
        updated.config = config;
        updated.provider = provider;
        updated.setup_required = setup_required;
        Ok(updated)
    }
}

/// Write the smallest configuration file the strict schema accepts.
///
/// `schema_version` is the only required field, and every other section defaults,
/// so this cannot disagree with the P0 contract: it writes no provider and no
/// credential setting, which remains the point of keeping them out of this file.
fn write_minimal_config(path: &Path) -> Result<(), HarnessError> {
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory).map_err(|error| {
            HarnessError::new(
                ErrorCode::StorageOpenFailed,
                format!(
                    "configuration directory {} could not be created: {error}",
                    directory.display()
                ),
            )
        })?;
    }
    std::fs::write(path, "schema_version = 1\n").map_err(|error| {
        HarnessError::new(
            ErrorCode::StorageOpenFailed,
            format!(
                "configuration file {} could not be written: {error}",
                path.display()
            ),
        )
    })
}

/// Resolve the launch context for one interactive session.
pub fn resolve(request: LaunchRequest) -> Result<LaunchContext, HarnessError> {
    let requested = match &request.cwd {
        Some(cwd) if cwd.is_absolute() => cwd.clone(),
        Some(cwd) => request.caller_dir.join(cwd),
        None => request.caller_dir.clone(),
    };
    let metadata = std::fs::metadata(&requested).map_err(|error| {
        HarnessError::new(
            ErrorCode::StorageOpenFailed,
            format!(
                "project directory {} cannot be opened: {error}",
                requested.display()
            ),
        )
    })?;
    if !metadata.is_dir() {
        return Err(HarnessError::new(
            ErrorCode::StorageOpenFailed,
            format!(
                "project path {} is not a directory; pass --cwd with a project directory",
                requested.display()
            ),
        ));
    }
    let canonical = std::fs::canonicalize(&requested).map_err(|error| {
        HarnessError::new(
            ErrorCode::StorageOpenFailed,
            format!(
                "project directory {} cannot be resolved: {error}",
                requested.display()
            ),
        )
    })?;
    let root = displayable(&canonical);
    let digest = ContentHash::from_bytes(root.to_string_lossy().as_bytes());
    let git_root = find_git_root(&canonical);

    let paths = paths::resolve(&PathRequest {
        platform: request.platform,
        environment: &request.environment,
        explicit_data_dir: request.explicit_data_dir.as_deref(),
    })?;
    let config = config::load(&paths.config_file)?;
    let provider = provider_state(&request.environment, &paths.data_dir);
    let setup_required =
        config.is_first_run() || matches!(provider, ProviderState::SetupRequired { .. });

    Ok(LaunchContext {
        project: ProjectIdentity {
            root,
            key: project_key(digest.as_str()),
            digest,
        },
        caller_dir: request.caller_dir,
        git_root,
        paths,
        config,
        provider,
        setup_required,
    })
}

/// Short, filesystem-safe project key derived from the identity digest.
fn project_key(digest: &str) -> String {
    let hexadecimal = digest.strip_prefix("sha256:").unwrap_or(digest);
    let short: String = hexadecimal.chars().take(16).collect();
    format!("project-{short}")
}

fn provider_state(environment: &LaunchEnvironment, data_dir: &Path) -> ProviderState {
    match credentials::source(environment, data_dir) {
        Some(source) => ProviderState::CredentialPresent { source },
        None => ProviderState::SetupRequired {
            reason: "no credential source".to_owned(),
        },
    }
}

/// Nearest ancestor that owns a Git entry, used for project context only.
fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(directory) = current {
        if directory.join(".git").exists() {
            return Some(displayable(directory));
        }
        current = directory.parent();
    }
    None
}

/// Drop the Windows verbatim prefix so paths stay readable in the header.
fn displayable(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let mut characters = text.chars();
    let verbatim = matches!(
        (
            characters.next(),
            characters.next(),
            characters.next(),
            characters.next()
        ),
        (Some('\\'), Some('\\'), Some('?'), Some('\\'))
    );
    if verbatim {
        PathBuf::from(characters.as_str())
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::{CREDENTIAL_VARIABLES, LaunchRequest, ProviderState, displayable, resolve};
    use crate::interactive::credentials::{self, CredentialSource};
    use crate::interactive::paths::{HostPlatform, LaunchEnvironment};
    use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
    use harness_types::{ErrorCode, HostId};

    struct Fixture {
        temp: tempfile::TempDir,
        home: std::path::PathBuf,
        project: std::path::PathBuf,
        /// The project path exactly as `resolve` will report it.
        ///
        /// `resolve` canonicalises the caller directory and drops the Windows
        /// verbatim prefix. Comparing that against the raw `tempdir()` path only
        /// works where the two spellings happen to agree: on a Windows runner the
        /// temp path keeps an 8.3 short component (`RUNNER~1`), so the raw path and
        /// the canonical one differ as strings while naming the same directory.
        canonical_project: std::path::PathBuf,
    }

    impl Fixture {
        /// A fixture home and project that never touch the real user profile.
        fn new(project_name: &str) -> Self {
            let temp = tempfile::tempdir().expect("temp root");
            let home = temp.path().join("home");
            let project = temp.path().join(project_name);
            std::fs::create_dir_all(&home).expect("fixture home");
            std::fs::create_dir_all(&project).expect("fixture project");
            let canonical_project =
                displayable(&std::fs::canonicalize(&project).expect("canonical project"));
            Self {
                temp,
                home,
                project,
                canonical_project,
            }
        }

        fn request(&self, environment: &LaunchEnvironment) -> LaunchRequest {
            LaunchRequest {
                cwd: None,
                caller_dir: self.project.clone(),
                platform: HostPlatform::current(),
                environment: environment.clone(),
                explicit_data_dir: None,
            }
        }

        fn environment(&self, extra: &[(&str, &str)]) -> LaunchEnvironment {
            let mut pairs = vec![("HA_HOME", self.home.to_string_lossy().into_owned())];
            pairs.extend(
                extra
                    .iter()
                    .map(|(name, value)| (*name, (*value).to_owned())),
            );
            LaunchEnvironment::from_pairs(pairs)
        }
    }

    #[test]
    fn h02_project_identity_follows_the_caller_directory_with_spaces_and_unicode() {
        let fixture = Fixture::new("my project ünïcode");
        let environment = fixture.environment(&[]);
        let context = resolve(fixture.request(&environment)).expect("context resolves");

        assert_eq!(context.project.root, fixture.canonical_project);
        assert!(context.project.key.starts_with("project-"));
        assert!(context.project.digest.as_str().starts_with("sha256:"));
        assert_eq!(context.caller_dir, fixture.project);
        assert!(
            !context.project.root.starts_with(env!("CARGO_MANIFEST_DIR")),
            "the project must come from the caller, never from the installation directory"
        );

        let header = context.header_lines().join("\n");
        assert!(header.contains("Harness Agents"), "{header}");
        // The header prints the canonical project path, so compare against the
        // canonical spelling rather than the raw tempdir one: on a Windows runner
        // the two differ (8.3 short component) while naming the same directory.
        assert!(
            header.contains(&fixture.canonical_project.display().to_string()),
            "{header}"
        );
        assert!(header.contains("setup required"), "{header}");
    }

    #[test]
    fn h02_missing_or_non_directory_project_is_an_actionable_error() {
        let fixture = Fixture::new("project");
        let environment = fixture.environment(&[]);

        let missing = fixture.project.join("does-not-exist");
        let error = resolve(LaunchRequest {
            cwd: Some(missing.clone()),
            ..fixture.request(&environment)
        })
        .expect_err("a missing project directory is an error");
        assert_eq!(error.code(), ErrorCode::StorageOpenFailed);
        assert!(error.to_string().contains("does-not-exist"), "{error}");

        let file = fixture.temp.path().join("a-file.txt");
        std::fs::write(&file, "not a directory").expect("fixture file");
        let error = resolve(LaunchRequest {
            cwd: Some(file),
            ..fixture.request(&environment)
        })
        .expect_err("a file is not a project directory");
        assert!(error.to_string().contains("not a directory"), "{error}");
    }

    #[test]
    fn h02_relative_cwd_resolves_from_the_caller_and_git_root_comes_from_the_project_tree() {
        let fixture = Fixture::new("caller");
        let nested = fixture.project.join("nested").join("child");
        std::fs::create_dir_all(&nested).expect("nested project");
        std::fs::create_dir_all(fixture.project.join(".git")).expect("git marker");
        let environment = fixture.environment(&[]);

        let context = resolve(LaunchRequest {
            cwd: Some(std::path::PathBuf::from("nested").join("child")),
            ..fixture.request(&environment)
        })
        .expect("relative cwd resolves");
        // Same canonical form as `resolve` reports, for the reason in `Fixture`.
        let canonical_nested = displayable(&std::fs::canonicalize(&nested).expect("canonical"));
        assert_eq!(context.project.root, canonical_nested);
        assert_eq!(
            context.git_root.as_deref(),
            Some(fixture.canonical_project.as_path())
        );
        assert!(context.header_lines().join("\n").contains("repository at"));

        let plain = fixture.temp.path().join("no-git-project");
        std::fs::create_dir_all(&plain).expect("plain project");
        let context = resolve(LaunchRequest {
            cwd: Some(plain),
            ..fixture.request(&environment)
        })
        .expect("project without Git still opens");
        assert!(context.git_root.is_none());
        assert!(
            context
                .header_lines()
                .join("\n")
                .contains("not a Git repository")
        );
    }

    #[test]
    fn k01_saving_a_key_clears_the_setup_gate_and_writes_the_minimal_config() {
        let fixture = Fixture::new("project");
        let environment = fixture.environment(&[]);
        let context = resolve(fixture.request(&environment)).expect("context resolves");
        assert!(
            context.setup_required,
            "no key and no config is a setup state"
        );

        let path = credentials::resolve_file(&environment, &context.paths.data_dir);
        credentials::save(&path, "sk-bootstrap-fixture").expect("the key is saved");
        let source = credentials::source(&environment, &context.paths.data_dir)
            .expect("the saved file is a credential source");
        let updated = context
            .credential_saved(source)
            .expect("the context accepts the saved credential");

        assert!(
            !updated.setup_required,
            "a saved key plus a written config is a complete setup"
        );
        assert!(updated.setup_hint().is_none());
        assert!(
            updated
                .header_lines()
                .join("\n")
                .contains("credentials.env"),
            "the header names the source and never the value: {:?}",
            updated.header_lines()
        );
        assert!(
            !updated
                .header_lines()
                .join("\n")
                .contains("sk-bootstrap-fixture"),
            "the header must not render the key"
        );
        assert_eq!(
            std::fs::read_to_string(&updated.paths.config_file).expect("config file"),
            "schema_version = 1\n",
            "the written configuration carries no provider or credential setting"
        );
        // A second save is idempotent: the file already exists, so it is loaded
        // rather than rewritten.
        let again = updated
            .credential_saved(CredentialSource::File {
                path: path.clone(),
                protection: credentials::Protection::OwnerOnly,
            })
            .expect("a second save still resolves");
        assert!(!again.setup_required);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn h02_empty_home_without_credentials_opens_setup_state_and_writes_nothing() {
        let fixture = Fixture::new("project");
        let environment = fixture.environment(&[]);
        let context = resolve(fixture.request(&environment)).expect("context resolves");

        assert!(context.setup_required);
        assert!(context.config.is_first_run());
        assert_eq!(
            context.provider,
            ProviderState::SetupRequired {
                reason: "no credential source".to_owned()
            }
        );
        let hint = context.setup_hint().expect("setup hint");
        assert!(hint.contains("no configuration file yet"), "{hint}");
        assert!(hint.contains("DEEPSEEK_API_KEY"), "{hint}");

        assert!(
            std::fs::read_dir(&fixture.home)
                .expect("fixture home")
                .next()
                .is_none(),
            "launch must not create configuration or data files"
        );
        assert!(
            std::fs::read_dir(&fixture.project)
                .expect("fixture project")
                .next()
                .is_none(),
            "launch must not write into the project"
        );
    }

    #[test]
    fn h02_credential_presence_is_detected_without_exposing_its_value() {
        let fixture = Fixture::new("project");
        std::fs::write(fixture.home.join("config.toml"), "schema_version = 1\n")
            .expect("fixture config");
        let secret = "sk-fixture-secret-value";
        let environment = fixture.environment(&[("DEEPSEEK_API_KEY", secret)]);
        let context = resolve(fixture.request(&environment)).expect("context resolves");

        assert_eq!(
            context.provider,
            ProviderState::CredentialPresent {
                source: CredentialSource::Environment {
                    variable: "DEEPSEEK_API_KEY".to_owned()
                }
            }
        );
        assert!(!context.setup_required, "config plus credential is ready");
        assert!(context.setup_hint().is_none());

        let rendered = context.header_lines().join("\n")
            + &context.setup_hint().unwrap_or_default()
            + &context.config.describe();
        assert!(
            !rendered.contains(secret),
            "the credential value must never be rendered"
        );
        assert!(rendered.contains("DEEPSEEK_API_KEY"));
        assert_eq!(CREDENTIAL_VARIABLES[0], "DEEPSEEK_API_KEY");
    }

    #[test]
    fn h02_two_terminals_in_one_project_share_a_store_and_the_second_is_busy() {
        let fixture = Fixture::new("project");
        let environment = fixture.environment(&[]);
        let first = resolve(fixture.request(&environment)).expect("first context");
        let second = resolve(fixture.request(&environment)).expect("second context");
        assert_eq!(first.project.key, second.project.key);
        assert_eq!(first.project_store_dir(), second.project_store_dir());
        assert!(
            first.project_store_dir().starts_with(&fixture.home),
            "project state stays under the resolved data root"
        );

        let other = fixture.temp.path().join("another-project");
        std::fs::create_dir_all(&other).expect("other project");
        let elsewhere = resolve(LaunchRequest {
            cwd: Some(other),
            ..fixture.request(&environment)
        })
        .expect("another project resolves");
        assert_ne!(elsewhere.project.key, first.project.key);
        assert_ne!(elsewhere.project_store_dir(), first.project_store_dir());

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let directory = first.project_store_dir();
            let writer = SqliteStore::open_writer(WriterOpenOptions::new(
                directory.clone(),
                HostId::generate(),
            ))
            .await
            .expect("first writer opens");
            let busy = SqliteStore::open_writer(WriterOpenOptions::new(
                directory.clone(),
                HostId::generate(),
            ))
            .await
            .expect_err("a second writer in the same project must not steal the lock");
            assert_eq!(busy.code(), ErrorCode::WriterLocked);
            assert!(busy.to_string().contains("another writable host"), "{busy}");
            drop(writer);
        });
    }

    #[test]
    fn h02_unresolvable_user_locations_are_reported_instead_of_guessed() {
        let fixture = Fixture::new("project");
        let environment = LaunchEnvironment::from_pairs([("SOMETHING_ELSE", "1")]);
        let error = resolve(fixture.request(&environment))
            .expect_err("an environment without a home cannot resolve user paths");
        assert_eq!(error.code(), ErrorCode::ConfigReadError);
        assert!(error.to_string().contains("HA_HOME"), "{error}");
    }
}
