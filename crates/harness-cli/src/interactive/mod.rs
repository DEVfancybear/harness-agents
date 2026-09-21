//! Interactive `ha` launch surface.
//!
//! Owner: `HA_LAUNCH` H01 — dispatch contract, terminal detection and the
//! non-terminal guard. H02 adds the resolved launch context and H03 replaces the
//! minimal boot shell with the controller/renderer pair. Nothing here starts a
//! provider, a store writer or a network call.

pub mod app;
pub mod attachments;
pub mod bootstrap;
pub mod bounds;
pub mod config;
pub mod controller;
pub mod credentials;
pub mod detector;
pub mod events;
pub mod extensions;
pub mod headless;
pub mod input;
pub mod memory;
pub mod paths;
pub mod project;
pub mod service;
pub mod terminal;
pub mod tui;
pub mod view;

use std::path::PathBuf;
use std::process::ExitCode;

use harness_types::HarnessError;

/// Exit code reserved for usage violations and the non-terminal guard; it is the
/// same code clap uses for parser errors.
pub const USAGE_EXIT_CODE: u8 = 2;

/// A launch request that the parser accepted but the launch contract rejects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageError {
    message: String,
}

impl UsageError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "ha: {}", self.message)
    }
}

/// The launch shape selected by the dispatch layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LaunchMode {
    /// Open the interactive app on a real terminal.
    Interactive {
        cwd: Option<PathBuf>,
        resume: Option<String>,
        /// Explicit opt-in to the labelled fixture backend; never a default and
        /// never a silent production fallback.
        fixture: bool,
        /// Force the plain renderer instead of the TUI.
        ///
        /// Set by `ha chat --plain` or by `HA_UI=plain`; the TUI is the default on
        /// a console that can hold it.
        plain: bool,
    },
    /// Run exactly one turn without a terminal.
    Headless {
        prompt: String,
        json: bool,
        cwd: Option<PathBuf>,
        resume: Option<String>,
        options: HeadlessOptions,
    },
}

/// Optional headless-run inputs that shape the goal, budget and backend.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HeadlessOptions {
    /// Explicit deterministic mock backend; never a default and never a silent
    /// production fallback. The JSON result labels it.
    pub mock: bool,
    /// Goal objective. Without it the turn is one bounded pass.
    pub goal: Option<String>,
    /// Evidence kinds the goal requires: `response`, `tool_execution`,
    /// `file_change`, `check`, `artifact`.
    pub criteria: Vec<String>,
    /// Host continuations one run may spend on the goal.
    pub max_continuations: Option<u32>,
    /// Token budget for the run, when the caller wants one.
    pub budget_tokens: Option<u64>,
}

/// Validate parser output into a launch mode.
///
/// Clap already enforces the flag conflicts, so this is the typed backstop that
/// keeps the contract testable without spawning a process, and that keeps a
/// missing prompt from ever being treated as free text.
#[allow(
    clippy::fn_params_excessive_bools,
    clippy::too_many_arguments,
    reason = "one flag per launch mode"
)]
pub fn mode_from_args(
    cwd: Option<PathBuf>,
    resume: Option<String>,
    fixture: bool,
    headless: bool,
    prompt: Option<String>,
    json: bool,
    plain: bool,
    options: HeadlessOptions,
) -> Result<LaunchMode, UsageError> {
    if !headless {
        if prompt.is_some() {
            return Err(UsageError::new(
                "--prompt is only valid with --headless; run ha chat --headless --prompt <text>",
            ));
        }
        if json {
            return Err(UsageError::new(
                "--json is only valid with --headless; run ha chat --headless --prompt <text> --json",
            ));
        }
        if options.mock
            || options.goal.is_some()
            || !options.criteria.is_empty()
            || options.max_continuations.is_some()
            || options.budget_tokens.is_some()
        {
            return Err(UsageError::new(
                "--mock, --goal, --criteria, --max-continuations and --budget are only valid with --headless",
            ));
        }
        return Ok(LaunchMode::Interactive {
            cwd,
            resume,
            fixture,
            // Clap already rejects `--plain --headless`; the typed backstop keeps
            // a headless run from ever being routed through the TUI.
            plain,
        });
    }
    if fixture {
        return Err(UsageError::new(
            "--fixture is only valid for the interactive app; a headless turn uses --mock for its explicit deterministic profile",
        ));
    }
    if plain {
        return Err(UsageError::new(
            "--plain is only valid for the interactive app; a headless turn never draws a viewport",
        ));
    }
    let Some(prompt) = prompt else {
        return Err(UsageError::new(
            "--headless requires --prompt <text>; the prompt is never read from stdin",
        ));
    };
    if prompt.trim().is_empty() {
        return Err(UsageError::new("--prompt must not be empty"));
    }
    Ok(LaunchMode::Headless {
        prompt,
        json,
        cwd,
        resume,
        options,
    })
}

/// The data root a launch would use for this environment.
///
/// Shared by the interactive launch and by the `ha memory` commands, so the store a
/// chat turn wrote is the store those commands read without anyone repeating the
/// path. It applies the same rule the launch does: `HA_HOME/data` when `HA_HOME` is
/// set, otherwise the platform default (`%LOCALAPPDATA%\HarnessAgents\data` on
/// Windows).
///
/// # Errors
/// Fails when the platform provides no data directory and the environment names no
/// `HA_HOME`, which is the same condition that stops a launch.
pub fn default_data_dir(environment: &paths::LaunchEnvironment) -> Result<PathBuf, HarnessError> {
    if let Some(home) = environment
        .value("HA_HOME")
        .filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(home).join("data"));
    }
    paths::resolve(&paths::PathRequest {
        platform: paths::HostPlatform::current(),
        environment,
        explicit_data_dir: None,
    })
    .map(|resolved| resolved.data_dir)
}

/// The identity and store a workspace root resolves to.
pub struct ResolvedProject {
    pub id: harness_types::ProjectId,
    /// The directory that holds this project's store.
    ///
    /// The `ha memory` commands take this as `--data-dir`, not the data root: they
    /// open the project store directly. Resolving the identity without also handing
    /// back the store would leave the caller with an id and the wrong directory.
    pub store_dir: PathBuf,
}

/// The project identity a workspace root is registered under.
///
/// The app neither shows this id nor offers a way to look it up, and a
/// project-scoped read without it returns nothing at all. This resolves the identity
/// the chat registered for that root, which is what lets `ha memory --cwd` inspect
/// what a turn stored.
///
/// The store directory is derived by running the launch's own resolution rather than
/// re-deriving the key here: the key hashes a *displayable* path (the Windows
/// `\\?\` verbatim prefix removed), so a second, look-alike implementation produced
/// a different directory for the same root. Measured: that mistake pointed the lookup
/// at a store the chat never wrote.
///
/// # Errors
/// Fails when the root cannot be resolved or its store cannot be opened: an
/// unregistered root has no identity yet, and saying so is better than inventing one.
pub async fn registered_project(root: &std::path::Path) -> Result<ResolvedProject, HarnessError> {
    let environment = paths::LaunchEnvironment::capture();
    let context = bootstrap::resolve(bootstrap::LaunchRequest {
        cwd: Some(root.to_path_buf()),
        caller_dir: std::env::current_dir().map_err(|error| {
            HarnessError::new(
                harness_types::ErrorCode::StorageOpenFailed,
                format!("the current directory could not be resolved: {error}"),
            )
        })?,
        platform: paths::HostPlatform::current(),
        environment,
        explicit_data_dir: None,
    })?;
    let store_dir = context.project_store_dir();
    let store = harness_store_sqlite::SqliteStore::open_read_only(store_dir.clone())
        .await
        .map_err(|error| {
            HarnessError::new(
                error.code(),
                format!(
                    "no project is registered for {}: {error}",
                    context.project.root.display()
                ),
            )
        })?;
    let id = project::resolve_project_id(&store, &context.project.root).await?;
    store
        .close()
        .await
        .map_err(|error| HarnessError::new(error.code(), format!("store close failed: {error}")))?;
    Ok(ResolvedProject { id, store_dir })
}

/// Environment variable that selects the renderer explicitly.
pub const UI_VARIABLE: &str = "HA_UI";

/// Whether the environment asks for the plain renderer.
///
/// Only the exact value `plain` counts: an unknown `HA_UI` value is not a silent
/// opt-in to anything, and the TUI stays the default.
#[must_use]
pub fn plain_requested(ui: Option<&str>) -> bool {
    matches!(ui, Some(value) if value.eq_ignore_ascii_case("plain"))
}

/// Read [`UI_VARIABLE`] from the process environment.
#[must_use]
pub fn plain_requested_from_environment() -> bool {
    plain_requested(std::env::var(UI_VARIABLE).ok().as_deref())
}

/// Run one launch request. Usage problems return exit code 2; real failures
/// return a typed error that the binary maps to exit code 1.
pub async fn launch(mode: LaunchMode) -> Result<ExitCode, HarnessError> {
    match mode {
        LaunchMode::Headless {
            prompt,
            json,
            cwd,
            resume,
            options,
        } => {
            Box::pin(headless::run(headless::HeadlessRequest {
                prompt,
                json,
                cwd,
                resume,
                options,
            }))
            .await
        }
        LaunchMode::Interactive {
            cwd,
            resume,
            fixture,
            plain,
        } => {
            launch_interactive_with(
                &detector::SystemTerminalDetector,
                cwd,
                resume,
                fixture,
                plain,
            )
            .await
        }
    }
}

/// Interactive launch with an injected detector so tests do not need a terminal.
pub async fn launch_interactive_with(
    detector: &dyn detector::TerminalDetector,
    cwd: Option<PathBuf>,
    resume: Option<String>,
    fixture: bool,
    plain: bool,
) -> Result<ExitCode, HarnessError> {
    let capability = detector.capability();
    if !capability.is_interactive() {
        eprint!("{}", non_terminal_guidance(capability));
        return Ok(ExitCode::from(USAGE_EXIT_CODE));
    }
    app::run(app::AppLaunch {
        cwd,
        resume,
        fixture,
        plain,
    })
    .await
}

/// Guidance printed to stderr when the interactive app cannot own a terminal.
#[must_use]
pub fn non_terminal_guidance(capability: detector::TerminalCapability) -> String {
    format!(
        "ha: no interactive terminal (stdin tty: {}, stdout tty: {}).\n\
         The interactive app needs a terminal it can own, so it was not started and stdin was not read.\n\
         Run one turn explicitly instead:\n  ha chat --headless --prompt \"<your request>\" [--json]\n",
        capability.stdin_is_terminal, capability.stdout_is_terminal
    )
}

#[cfg(test)]
mod tests {
    use super::{
        HeadlessOptions, LaunchMode, USAGE_EXIT_CODE, mode_from_args, non_terminal_guidance,
    };
    use crate::interactive::bootstrap;
    use crate::interactive::detector::TerminalCapability;
    use crate::interactive::paths::{self, LaunchEnvironment};
    use std::path::PathBuf;

    #[test]
    fn h01_mode_from_args_accepts_bare_and_optioned_interactive_launch() {
        assert_eq!(
            mode_from_args(
                None,
                None,
                false,
                false,
                None,
                false,
                false,
                HeadlessOptions::default()
            )
            .expect("bare launch is valid"),
            LaunchMode::Interactive {
                cwd: None,
                resume: None,
                fixture: false,
                plain: false,
            }
        );
        assert_eq!(
            mode_from_args(
                Some(PathBuf::from("C:/work/project")),
                Some("session_1".to_owned()),
                true,
                false,
                None,
                false,
                true,
                HeadlessOptions::default(),
            )
            .expect("interactive launch with options is valid"),
            LaunchMode::Interactive {
                cwd: Some(PathBuf::from("C:/work/project")),
                resume: Some("session_1".to_owned()),
                fixture: true,
                plain: true,
            }
        );
    }

    #[test]
    fn t07_plain_is_rejected_for_a_headless_turn() {
        let error = mode_from_args(
            None,
            None,
            false,
            true,
            Some("hi".to_owned()),
            false,
            true,
            HeadlessOptions::default(),
        )
        .expect_err("a headless turn never draws a viewport");
        assert!(error.to_string().contains("--plain is only valid"));
    }

    #[test]
    fn t07_plain_requested_only_honours_the_exact_value() {
        assert!(super::plain_requested(Some("plain")));
        assert!(super::plain_requested(Some("PLAIN")));
        assert!(!super::plain_requested(Some("tui")));
        assert!(!super::plain_requested(Some("")));
        assert!(!super::plain_requested(None));
    }

    #[test]
    fn h01_mode_from_args_requires_prompt_for_headless() {
        let error = mode_from_args(
            None,
            None,
            false,
            true,
            None,
            false,
            false,
            HeadlessOptions::default(),
        )
        .expect_err("headless needs a prompt");
        assert!(error.to_string().contains("--headless requires --prompt"));

        let error = mode_from_args(
            None,
            None,
            false,
            true,
            Some("   ".to_owned()),
            false,
            false,
            HeadlessOptions::default(),
        )
        .expect_err("blank prompt is rejected");
        assert!(error.to_string().contains("must not be empty"));
    }

    #[test]
    fn h01_mode_from_args_rejects_headless_only_flags_without_headless() {
        let error = mode_from_args(
            None,
            None,
            false,
            false,
            Some("hello".to_owned()),
            false,
            false,
            HeadlessOptions::default(),
        )
        .expect_err("prompt without headless is rejected");
        assert!(
            error
                .to_string()
                .contains("--prompt is only valid with --headless")
        );

        let error = mode_from_args(
            None,
            None,
            false,
            false,
            None,
            true,
            false,
            HeadlessOptions::default(),
        )
        .expect_err("json without headless is rejected");
        assert!(
            error
                .to_string()
                .contains("--json is only valid with --headless")
        );
    }

    #[test]
    fn h03_fixture_is_rejected_for_a_headless_turn() {
        let error = mode_from_args(
            None,
            None,
            true,
            true,
            Some("fix the parser".to_owned()),
            false,
            false,
            HeadlessOptions::default(),
        )
        .expect_err("a headless turn must not silently use the fixture");
        assert!(error.to_string().contains("--fixture is only valid"));
    }

    #[test]
    fn h01_headless_mode_carries_prompt_and_json_flag() {
        let mode = mode_from_args(
            None,
            None,
            false,
            true,
            Some("fix the parser".to_owned()),
            true,
            false,
            HeadlessOptions::default(),
        )
        .expect("headless launch is valid");
        assert_eq!(
            mode,
            LaunchMode::Headless {
                prompt: "fix the parser".to_owned(),
                json: true,
                cwd: None,
                resume: None,
                options: HeadlessOptions::default(),
            }
        );
    }

    #[test]
    fn h01_non_terminal_guidance_points_at_the_explicit_headless_command() {
        let guidance = non_terminal_guidance(TerminalCapability::piped());
        assert!(guidance.contains("stdin tty: false"));
        assert!(guidance.contains("stdout tty: false"));
        assert!(guidance.contains("ha chat --headless --prompt"));
        assert_eq!(USAGE_EXIT_CODE, 2);
    }

    /// K05: the key a launch computes must not depend on how the root was named.
    ///
    /// `ha memory --cwd <root>` looks the store up by this key, so if a chat that
    /// started *in* the directory and a command that *names* the directory disagree,
    /// the lookup opens a store the chat never wrote. That is exactly the bug this
    /// guards: it was written once with a second, look-alike key derivation and the
    /// measured result was "no project is registered" for a root that had just been
    /// written.
    #[test]
    fn k05_a_root_names_the_same_project_either_way_it_is_reached() {
        let temp = tempfile::tempdir().expect("temp root");
        let root = temp.path().join("workspace");
        std::fs::create_dir(&root).expect("workspace");
        let environment = LaunchEnvironment::from_pairs([(
            "HA_HOME",
            temp.path().join("home").to_string_lossy().into_owned(),
        )]);
        let base = |cwd: Option<std::path::PathBuf>, caller: std::path::PathBuf| {
            bootstrap::resolve(bootstrap::LaunchRequest {
                cwd,
                caller_dir: caller,
                platform: paths::HostPlatform::current(),
                environment: environment.clone(),
                explicit_data_dir: None,
            })
            .expect("context resolves")
        };
        let named = base(Some(root.clone()), temp.path().to_path_buf());
        let started_in = base(None, root.clone());
        assert_eq!(
            named.project.key, started_in.project.key,
            "naming a directory and starting in it must resolve one project"
        );
        assert_eq!(named.project_store_dir(), started_in.project_store_dir());
        // And the temporary-directory spelling must not change it either: a canonical
        // path and the path the caller typed are the same workspace.
        let canonical = std::fs::canonicalize(&root).expect("canonical root");
        let via_canonical = base(Some(canonical), temp.path().to_path_buf());
        assert_eq!(
            named.project.key, via_canonical.project.key,
            "a canonical spelling must resolve the same project as the typed one"
        );
    }
}
