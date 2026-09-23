//! The M12 capability probes: real processes, real observations.
//!
//! Every verdict in a [`CapabilityMatrix`] comes from one of these functions.
//! None of them asks a child "were you blocked?"; each one makes the child
//! *try* something and then looks at the effect from the outside — a tick file
//! that stops growing, a canary that does or does not come back, a connection
//! that does or does not arrive.
//!
//! Two rules hold for every probe here:
//!
//! * A probe that cannot run is an **error**, never an enforcement. A fixture
//!   that failed to start must not be able to produce a green verdict.
//! * Only the boundaries that exist are probed as boundaries. Memory and
//!   process-count caps are probed by *observing that nothing caps them*, which
//!   is why their verdicts are `unsupported` with a structural reason attached.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use harness_providers::CancellationToken;
use harness_types::{ContentHash, ErrorCode, HarnessError};
use tokio::{io::AsyncReadExt, net::TcpListener, time::timeout};

use crate::{
    capture::{ProcessSpoolConfig, SpoolLimits},
    process::{self, HostEnvironment, ProcessResult},
    secrets::ProcessEnvironment,
};

use super::{
    capability::{
        CONTAINMENT_BACKEND, CONTAINMENT_BACKEND_VERSION, Capability, CapabilityEvidence,
        CapabilityFinding, CapabilityMatrix, HostIdentity,
    },
    probe_control::{self, ControlCommand},
};

/// The variable the probe injects into a *synthetic* host environment. It is
/// deliberately outside [`crate::PROCESS_ENVIRONMENT_ALLOWLIST`], so the only
/// way a child can see it is by inheriting the host environment.
pub const PROBE_CANARY_NAME: &str = "HA_M12_PROBE_HOST_CANARY";

/// How long a boundary-break control lets the fixture run before killing only
/// the process it started.
const CONTROL_RUN: Duration = Duration::from_millis(1200);

/// How long the control's descendants live. Long enough that the escape window
/// (about two seconds) sits well inside it, short enough that a deliberately
/// leaked tree is gone quickly.
const CONTROL_LIFETIME_MS: u64 = 4_000;

/// How long the containment probe lets the tree run before the cancel.
const CONTAIN_TIME: Duration = Duration::from_millis(1500);

/// Bound on how long an escape may be waited for before it is called absent.
const ESCAPE_WINDOW: Duration = Duration::from_secs(4);

const TICK_LIFETIME_MS: u64 = 9_000;
const FLOOD_BYTES: u64 = 256 * 1024;
const FLOOD_QUOTA_BYTES: u64 = 64 * 1024;
const MEMORY_MIB: u64 = 256;
const DESCENDANTS: u32 = 4;
/// Long enough for the crowd to be observably alive, short enough that a probe
/// run does not linger: the sample is taken half a second in.
const CROWD_LIFETIME_MS: u64 = 1_500;

/// The fixture executable the probes drive, with any argument prefix.
#[derive(Clone, Debug)]
pub struct ProbeChild {
    executable: PathBuf,
    prefix: Vec<String>,
}

impl ProbeChild {
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>, prefix: Vec<String>) -> Self {
        Self {
            executable: executable.into(),
            prefix,
        }
    }

    /// The fixture shipped beside this binary.
    pub fn for_current_executable(prefix: Vec<String>) -> Result<Self, HarnessError> {
        let executable = std::env::current_exe().map_err(|error| {
            HarnessError::new(
                ErrorCode::ServiceUnavailable,
                format!("cannot locate the running executable for the probe fixture: {error}"),
            )
        })?;
        Ok(Self::new(executable, prefix))
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    fn argv(&self, mode: &[&str]) -> Vec<String> {
        let mut args = self.prefix.clone();
        args.extend(mode.iter().map(|value| (*value).to_owned()));
        args
    }
}

/// What a boundary-break control observed, for a caller that must prove the
/// probe can lose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundaryBreakObservation {
    pub detail: String,
    /// Ticks written by the tree after the direct child was killed but before
    /// the escape window closed.
    pub ticks_after_kill: u64,
    pub escaped: bool,
}

/// Runs every probe and returns the measured matrix.
#[derive(Clone, Debug)]
pub struct CapabilityProbe {
    root: PathBuf,
    child: ProbeChild,
    host: HostEnvironment,
    spool: ProcessSpoolConfig,
    nonce: String,
}

impl CapabilityProbe {
    /// A probe rooted at `root`, with a synthetic host environment that carries
    /// the canary the allowlist probe must not let through.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, child: ProbeChild) -> Self {
        let root = root.into();
        let nonce = harness_types::HostId::generate().as_str().to_owned();
        let host = HostEnvironment::from_process().with_values([
            (PROBE_CANARY_NAME.to_owned(), nonce.clone()),
            (
                "HA_M12_PROBE_HOST_TOKEN".to_owned(),
                format!("token-{nonce}"),
            ),
        ]);
        Self {
            spool: ProcessSpoolConfig::new(
                root.join("spool"),
                SpoolLimits {
                    max_capture_bytes: FLOOD_QUOTA_BYTES,
                    head_preview_bytes: 2048,
                    tail_preview_bytes: 2048,
                },
            ),
            root,
            child,
            host,
            nonce,
        }
    }

    /// Replace the host environment the probes measure the allowlist against.
    #[must_use]
    pub fn with_host_environment(mut self, host: HostEnvironment) -> Self {
        self.host = host;
        self
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the fixture is allowed to run: the one directory a containment
    /// backend would confine it to if it had one.
    fn run_root(&self) -> PathBuf {
        self.root.join("sandbox")
    }

    /// Deliberately outside the run root: the canary side of the boundary.
    fn outside_root(&self) -> PathBuf {
        self.root.join("outside")
    }

    fn prepare_roots(&self) -> Result<(), HarnessError> {
        for path in [
            self.run_root(),
            self.outside_root(),
            self.spool.root().to_owned(),
        ] {
            std::fs::create_dir_all(&path).map_err(|error| {
                HarnessError::new(
                    ErrorCode::StorageOpenFailed,
                    format!("cannot create probe directory {}: {error}", path.display()),
                )
            })?;
        }
        Ok(())
    }

    /// Run the fixture through the production execution path.
    async fn run_contained(
        &self,
        mode: &[&str],
        timeout_ms: u64,
        cancellation: CancellationToken,
    ) -> Result<ProcessResult, HarnessError> {
        let args = self.child.argv(mode);
        process::run_structured_with_host(
            &self.run_root(),
            self.child.executable().to_string_lossy().as_ref(),
            &args,
            timeout_ms,
            cancellation,
            &ProcessEnvironment::empty(),
            &self.spool,
            &self.host,
        )
        .await
    }

    /// Measure every capability on this host.
    pub async fn run(&self) -> Result<CapabilityMatrix, HarnessError> {
        self.prepare_roots()?;
        let version = probe_control::observe_platform_version(&self.run_root()).await?;
        let findings = vec![
            self.probe_containment().await?,
            self.probe_tree_kill().await?,
            self.probe_environment().await?,
            self.probe_deadline().await?,
            self.probe_output().await?,
            self.probe_filesystem_read().await?,
            self.probe_filesystem_write().await?,
            self.probe_network().await?,
            self.probe_credential_socket().await?,
            self.probe_memory().await?,
            self.probe_process_count().await?,
        ];
        let matrix = CapabilityMatrix::new(HostIdentity::observed(version), findings);
        matrix.validate()?;
        Ok(matrix)
    }

    // -- the boundaries that exist -----------------------------------------

    /// Run the fixture through the production execution path, optionally
    /// canceling it after a delay.
    async fn run_fixture(
        &self,
        mode: &[&str],
        cancel_after: Option<Duration>,
    ) -> Result<ProcessResult, HarnessError> {
        let cancel = CancellationToken::new();
        let Some(delay) = cancel_after else {
            return self.run_contained(mode, 30_000, cancel).await;
        };
        let (result, ()) = tokio::join!(self.run_contained(mode, 30_000, cancel.clone()), async {
            tokio::time::sleep(delay).await;
            cancel.cancel();
        });
        result
    }

    /// A canceled run of four writers, and the tick file they share.
    async fn canceled_tree(
        &self,
        ticks: PathBuf,
    ) -> Result<(ProcessResult, u64, u64, u128), HarnessError> {
        let file = ticks.to_string_lossy().into_owned();
        let lifetime = TICK_LIFETIME_MS.to_string();
        let descendants = DESCENDANTS.to_string();
        let started = std::time::Instant::now();
        let result = self
            .run_fixture(
                &["ticks", &file, &descendants, &lifetime],
                Some(CONTAIN_TIME),
            )
            .await?;
        let elapsed = started.elapsed().as_millis();
        let (before, after) = tick_pair(&ticks).await;
        Ok((result, before, after, elapsed))
    }

    /// A run whose direct child exits at once while its descendants keep
    /// writing: the shape that catches a completion report that is only about
    /// the direct child.
    async fn detached_tree(
        &self,
        ticks: PathBuf,
    ) -> Result<(ProcessResult, u64, u64, u128), HarnessError> {
        let file = ticks.to_string_lossy().into_owned();
        let lifetime = TICK_LIFETIME_MS.to_string();
        let descendants = DESCENDANTS.to_string();
        let started = std::time::Instant::now();
        let result = self
            .run_fixture(&["spawn-detach", &file, &descendants, &lifetime], None)
            .await?;
        let elapsed = started.elapsed().as_millis();
        let (before, after) = tick_pair(&ticks).await;
        Ok((result, before, after, elapsed))
    }

    async fn probe_containment(&self) -> Result<CapabilityFinding, HarnessError> {
        let canceled = self
            .canceled_tree(self.run_root().join("containment-ticks.txt"))
            .await?;
        let detached = self
            .detached_tree(self.run_root().join("detach-ticks.txt"))
            .await?;
        let cancel_stopped = canceled.1 == canceled.2 && canceled.1 > 0;
        let detach_stopped = detached.1 == detached.2 && detached.1 > 0;
        let method = "run a real child with detached descendants that write a tick file every 100 ms, end the run two ways (cancel, and a direct child that exits at once), then sample the tick file twice 800 ms apart after each run returned";
        let observation = format!(
            "cancel path: run {} ms, canceled={}, cleanup={}, ticks {} -> {}; natural-exit path: run {} ms, cleanup={}, ticks {} -> {} (tick lifetime {} ms, so no descendant can have exited on its own); the natural-exit run labels its tree {} while descendants were still writing, which is why the post-run sample and not the label is the evidence",
            canceled.3,
            canceled.0.canceled,
            canceled.0.tree_cleanup.as_str(),
            canceled.1,
            canceled.2,
            detached.3,
            detached.0.tree_cleanup.as_str(),
            detached.1,
            detached.2,
            TICK_LIFETIME_MS,
            detached.0.tree_cleanup.as_str(),
        );
        if cancel_stopped && detach_stopped {
            Ok(CapabilityFinding::enforced(
                Capability::ProcessContainment,
                CapabilityEvidence::new("P-CONT", method, observation),
            ))
        } else {
            Ok(CapabilityFinding::unsupported(
                Capability::ProcessContainment,
                CapabilityEvidence::new(
                    "P-CONT",
                    method,
                    format!("a descendant was still writing after the run returned: {observation}"),
                ),
            ))
        }
    }

    async fn probe_tree_kill(&self) -> Result<CapabilityFinding, HarnessError> {
        let canceled = self
            .canceled_tree(self.run_root().join("tree-ticks.txt"))
            .await?;
        let (result, before, after, elapsed) = canceled;
        let spawn_marker = result.stdout.contains("spawned=");
        let stopped = before == after && before > 0;
        let method = "cancel a real tree of four writers and require a reaped tree, a canceled run, and no writer still writing after the run returns";
        if result.canceled
            && result.tree_cleanup.as_str() == "killed_and_reaped"
            && spawn_marker
            && stopped
        {
            Ok(CapabilityFinding::enforced(
                Capability::ProcessTreeKill,
                CapabilityEvidence::new(
                    "P-TREE",
                    method,
                    format!(
                        "the cancel landed {elapsed} ms in, {DESCENDANTS} descendants were live, the runner reported killed_and_reaped, and the tick file held at {before} across the sampling window"
                    ),
                ),
            ))
        } else {
            Ok(CapabilityFinding::unsupported(
                Capability::ProcessTreeKill,
                CapabilityEvidence::new(
                    "P-TREE",
                    method,
                    format!(
                        "the tree kill was not confirmed: canceled={} cleanup={} ticks {before} -> {after} marker={spawn_marker} run={elapsed} ms",
                        result.canceled,
                        result.tree_cleanup.as_str()
                    ),
                ),
            ))
        }
    }

    async fn probe_environment(&self) -> Result<CapabilityFinding, HarnessError> {
        let result = self
            .run_contained(&["env"], 20_000, CancellationToken::new())
            .await?;
        let canary_seen = result
            .stdout
            .lines()
            .any(|line| line.starts_with(&format!("{PROBE_CANARY_NAME}=")));
        let allowlisted_seen = result.stdout.lines().any(|line| {
            line.starts_with("PATH=")
                || line.starts_with("SystemRoot=")
                || line.starts_with("HOME=")
        });
        let canary_digest = ContentHash::from_bytes(self.nonce.as_bytes());
        let canary_digest = canary_digest.as_str();
        let method = "start a real child against a synthetic host environment containing a non-allowlisted canary, then read the environment block the child itself printed";
        if canary_seen || !allowlisted_seen {
            Ok(CapabilityFinding::unsupported(
                Capability::EnvironmentAllowlist,
                CapabilityEvidence::new(
                    "P-ENV",
                    method,
                    format!(
                        "the child's environment block leaked host values (canary seen: {canary_seen}, allowlisted variable present: {allowlisted_seen}); canary digest {canary_digest}"
                    ),
                ),
            ))
        } else {
            Ok(CapabilityFinding::enforced(
                Capability::EnvironmentAllowlist,
                CapabilityEvidence::new(
                    "P-ENV",
                    method,
                    format!(
                        "the child's environment block contained the allowlisted variables and not the canary; the withheld value's digest is {canary_digest}"
                    ),
                ),
            ))
        }
    }

    async fn probe_deadline(&self) -> Result<CapabilityFinding, HarnessError> {
        let result = self
            .run_contained(&["sleep", "60000"], 1_500, CancellationToken::new())
            .await?;
        let method = "give a real child a 1.5 s deadline while it sleeps for 60 s, and require the runner to report a timed-out run with a reaped tree";
        if result.timed_out && !result.canceled {
            Ok(CapabilityFinding::enforced(
                Capability::DeadlineEnforced,
                CapabilityEvidence::new(
                    "P-DEADLINE",
                    method,
                    format!(
                        "the run returned at its deadline with timed_out=true, canceled=false, cleanup={}",
                        result.tree_cleanup.as_str()
                    ),
                ),
            ))
        } else {
            Ok(CapabilityFinding::unsupported(
                Capability::DeadlineEnforced,
                CapabilityEvidence::new(
                    "P-DEADLINE",
                    method,
                    format!(
                        "the deadline was not enforced: timed_out={} canceled={}",
                        result.timed_out, result.canceled
                    ),
                ),
            ))
        }
    }

    async fn probe_output(&self) -> Result<CapabilityFinding, HarnessError> {
        let bytes = FLOOD_BYTES.to_string();
        let result = self
            .run_contained(&["flood", &bytes], 30_000, CancellationToken::new())
            .await?;
        let bounded = result.stdout_truncated
            && u64::try_from(result.stdout.len()).unwrap_or(u64::MAX) <= FLOOD_QUOTA_BYTES;
        let method = format!(
            "have a real child write {FLOOD_BYTES} bytes to stdout while the spool quota is {FLOOD_QUOTA_BYTES} bytes"
        );
        if bounded {
            Ok(CapabilityFinding::enforced(
                Capability::OutputBounds,
                CapabilityEvidence::new(
                    "P-OUTPUT",
                    &method,
                    format!(
                        "the capture reported truncation and carried {} preview bytes; the quota held",
                        result.stdout.len()
                    ),
                ),
            ))
        } else {
            Ok(CapabilityFinding::unsupported(
                Capability::OutputBounds,
                CapabilityEvidence::new(
                    "P-OUTPUT",
                    &method,
                    format!(
                        "the capture exceeded its quota: truncated={} preview_bytes={}",
                        result.stdout_truncated,
                        result.stdout.len()
                    ),
                ),
            ))
        }
    }

    // -- the boundaries that do not exist -----------------------------------

    async fn probe_filesystem_read(&self) -> Result<CapabilityFinding, HarnessError> {
        let canary = self.outside_root().join("read-canary.txt");
        let payload = format!("M12-READ-CANARY-{}", self.nonce);
        std::fs::write(&canary, &payload).map_err(|error| {
            HarnessError::new(
                ErrorCode::StorageWriteFailed,
                format!("cannot write the read canary: {error}"),
            )
        })?;
        let path = canary.to_string_lossy().into_owned();
        let result = self
            .run_contained(&["read", &path], 20_000, CancellationToken::new())
            .await?;
        let method = "place a canary file outside the run root and ask a real child to read it";
        if result.stdout.contains(&payload) {
            Ok(CapabilityFinding::unsupported(
                Capability::FilesystemReadConfinement,
                CapabilityEvidence::new(
                    "P-FS-R",
                    method,
                    format!(
                        "the child read {} outside the run root and returned its contents; canary digest {}",
                        canary.display(),
                        ContentHash::from_bytes(payload.as_bytes()).as_str()
                    ),
                ),
            ))
        } else {
            Ok(CapabilityFinding::enforced(
                Capability::FilesystemReadConfinement,
                CapabilityEvidence::new(
                    "P-FS-R",
                    method,
                    "the child could not return the canary outside the run root",
                ),
            ))
        }
    }

    async fn probe_filesystem_write(&self) -> Result<CapabilityFinding, HarnessError> {
        let target = self
            .outside_root()
            .join(format!("write-canary-{}.txt", self.nonce));
        let payload = format!("M12-WRITE-CANARY-{}", self.nonce);
        let path = target.to_string_lossy().into_owned();
        let result = self
            .run_contained(
                &["write", &path, &payload],
                20_000,
                CancellationToken::new(),
            )
            .await?;
        let method = "ask a real child to create a file outside the run root";
        let escaped = std::fs::read_to_string(&target).is_ok_and(|text| text == payload);
        if escaped {
            Ok(CapabilityFinding::unsupported(
                Capability::FilesystemWriteConfinement,
                CapabilityEvidence::new(
                    "P-FS-W",
                    method,
                    format!(
                        "the child created {} outside the run root with digest {}; exit={:?}",
                        target.display(),
                        ContentHash::from_bytes(payload.as_bytes()).as_str(),
                        result.exit_code
                    ),
                ),
            ))
        } else {
            Ok(CapabilityFinding::enforced(
                Capability::FilesystemWriteConfinement,
                CapabilityEvidence::new(
                    "P-FS-W",
                    method,
                    "the child could not create a file outside the run root",
                ),
            ))
        }
    }

    async fn probe_network(&self) -> Result<CapabilityFinding, HarnessError> {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|error| {
            HarnessError::new(
                ErrorCode::ServiceUnavailable,
                format!("cannot open the probe listener: {error}"),
            )
        })?;
        let port = listener
            .local_addr()
            .map_err(|error| {
                HarnessError::new(
                    ErrorCode::ServiceUnavailable,
                    format!("cannot read the probe listener address: {error}"),
                )
            })?
            .port();
        let nonce = self.nonce.clone();
        let port_text = port.to_string();
        let accept = async {
            let Ok(Ok((mut stream, _))) = timeout(ESCAPE_WINDOW, listener.accept()).await else {
                return String::new();
            };
            let mut buffer = vec![0_u8; 256];
            match timeout(Duration::from_secs(2), stream.read(&mut buffer)).await {
                Ok(Ok(read)) => String::from_utf8_lossy(&buffer[..read]).into_owned(),
                _ => String::new(),
            }
        };
        let mode = ["connect", port_text.as_str(), nonce.as_str()];
        let (result, received) = tokio::join!(
            self.run_contained(&mode, 20_000, CancellationToken::new()),
            accept
        );
        let result = result?;
        let method =
            "run a loopback listener and ask a real child to connect to it and send a token";
        if received.contains(&nonce) {
            Ok(CapabilityFinding::unsupported(
                Capability::NetworkEgressDenial,
                CapabilityEvidence::new(
                    "P-NET",
                    method,
                    format!(
                        "a contained child opened 127.0.0.1:{port} and delivered its token; digest {}",
                        ContentHash::from_bytes(nonce.as_bytes()).as_str()
                    ),
                ),
            ))
        } else {
            let _ = result;
            Ok(CapabilityFinding::enforced(
                Capability::NetworkEgressDenial,
                CapabilityEvidence::new(
                    "P-NET",
                    method,
                    format!("no connection reached 127.0.0.1:{port} within the probe window"),
                ),
            ))
        }
    }

    #[cfg(windows)]
    async fn probe_credential_socket(&self) -> Result<CapabilityFinding, HarnessError> {
        use tokio::net::windows::named_pipe::ServerOptions;

        let name = format!(r"\\.\pipe\ha-m12-{}", self.nonce);
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .map_err(|error| {
                HarnessError::new(
                    ErrorCode::ServiceUnavailable,
                    format!("cannot create the probe named pipe: {error}"),
                )
            })?;
        let nonce = self.nonce.clone();
        let accept = async {
            let Ok(Ok(())) = timeout(ESCAPE_WINDOW, server.connect()).await else {
                return String::new();
            };
            let mut buffer = vec![0_u8; 256];
            match timeout(Duration::from_secs(2), server.read(&mut buffer)).await {
                Ok(Ok(read)) => String::from_utf8_lossy(&buffer[..read]).into_owned(),
                _ => String::new(),
            }
        };
        let mode = ["pipe", name.as_str(), nonce.as_str()];
        let (result, received) = tokio::join!(
            self.run_contained(&mode, 20_000, CancellationToken::new()),
            accept
        );
        let _ = result?;
        let method = "host a named pipe the way a host credential agent would and ask a real child to open it";
        if received.contains(&nonce) {
            Ok(CapabilityFinding::unsupported(
                Capability::CredentialSocketDenial,
                CapabilityEvidence::new(
                    "P-SOCK",
                    method,
                    format!("a contained child opened {name} and wrote its token"),
                ),
            ))
        } else {
            Ok(CapabilityFinding::enforced(
                Capability::CredentialSocketDenial,
                CapabilityEvidence::new(
                    "P-SOCK",
                    method,
                    format!("no child reached the probe pipe {name} within the probe window"),
                ),
            ))
        }
    }

    #[cfg(unix)]
    async fn probe_credential_socket(&self) -> Result<CapabilityFinding, HarnessError> {
        let path = self
            .outside_root()
            .join(format!("agent-{}.sock", self.nonce));
        let listener = tokio::net::UnixListener::bind(&path).map_err(|error| {
            HarnessError::new(
                ErrorCode::ServiceUnavailable,
                format!("cannot create the probe socket: {error}"),
            )
        })?;
        let nonce = self.nonce.clone();
        let accept = async {
            let Ok(Ok((mut stream, _))) = timeout(ESCAPE_WINDOW, listener.accept()).await else {
                return String::new();
            };
            let mut buffer = vec![0_u8; 256];
            match timeout(Duration::from_secs(2), stream.read(&mut buffer)).await {
                Ok(Ok(read)) => String::from_utf8_lossy(&buffer[..read]).into_owned(),
                _ => String::new(),
            }
        };
        let address = path.to_string_lossy().into_owned();
        let mode = ["pipe", address.as_str(), nonce.as_str()];
        let (result, received) = tokio::join!(
            self.run_contained(&mode, 20_000, CancellationToken::new()),
            accept
        );
        let _ = result?;
        let method = "host a unix socket the way a host credential agent would and ask a real child to open it";
        if received.contains(&nonce) {
            Ok(CapabilityFinding::unsupported(
                Capability::CredentialSocketDenial,
                CapabilityEvidence::new(
                    "P-SOCK",
                    method,
                    format!("a contained child connected to {address} and wrote its token"),
                ),
            ))
        } else {
            Ok(CapabilityFinding::enforced(
                Capability::CredentialSocketDenial,
                CapabilityEvidence::new(
                    "P-SOCK",
                    method,
                    format!("no child reached the probe socket {address} within the probe window"),
                ),
            ))
        }
    }

    async fn probe_memory(&self) -> Result<CapabilityFinding, HarnessError> {
        let mib = MEMORY_MIB.to_string();
        let result = self
            .run_contained(&["alloc", &mib], 60_000, CancellationToken::new())
            .await?;
        let allocated = result.stdout.contains(&format!("allocated={MEMORY_MIB}"));
        Ok(CapabilityFinding::unsupported(
            Capability::ResourceLimitMemory,
            CapabilityEvidence::new(
                "P-MEM",
                "ask a real child to allocate a fixed amount and check whether any configured cap stops it",
                format!(
                    "structural: the pinned backend {CONTAINMENT_BACKEND} {CONTAINMENT_BACKEND_VERSION} sets only JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE and exposes no memory limit, and the API that would set one (SetInformationJobObject) needs unsafe FFI, which this workspace forbids; observed: a contained child allocated {MEMORY_MIB} MiB with no such cap in force (allocated={allocated}, exit={:?})",
                    result.exit_code
                ),
            ),
        ))
    }

    async fn probe_process_count(&self) -> Result<CapabilityFinding, HarnessError> {
        let count = DESCENDANTS.to_string();
        let lifetime = CROWD_LIFETIME_MS.to_string();
        let result = self
            .run_contained(
                &["spawn-count", &count, &lifetime],
                60_000,
                CancellationToken::new(),
            )
            .await?;
        let alive = result
            .stdout
            .split_whitespace()
            .find_map(|part| part.strip_prefix("alive="))
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        let spawned = result
            .stdout
            .split_whitespace()
            .find_map(|part| part.strip_prefix("spawned="))
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        Ok(CapabilityFinding::unsupported(
            Capability::ResourceLimitProcessCount,
            CapabilityEvidence::new(
                "P-PROC",
                "ask a real child to spawn a fixed number of descendants and check whether any configured cap stops it",
                format!(
                    "structural: the pinned backend {CONTAINMENT_BACKEND} {CONTAINMENT_BACKEND_VERSION} sets only JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE and exposes no active-process limit; observed: a contained child spawned {spawned} of {DESCENDANTS} descendants with {alive} still running half a second later (exit={:?})",
                    result.exit_code
                ),
            ),
        ))
    }

    // -- negative controls a caller can assert on ---------------------------

    /// Run the containment fixture with the job-object wrapper omitted.
    ///
    /// The production path is expected to reap the tree; this control is
    /// expected to leave it running. A probe that returned "enforced" here
    /// would be measuring nothing at all.
    pub async fn boundary_break_control(&self) -> Result<BoundaryBreakObservation, HarnessError> {
        self.prepare_roots()?;
        let ticks = self.run_root().join("control-ticks.txt");
        let command = ControlCommand::new(self.child.executable(), self.run_root()).with_args(
            self.child.argv(&[
                "ticks",
                ticks.to_string_lossy().as_ref(),
                &DESCENDANTS.to_string(),
                &CONTROL_LIFETIME_MS.to_string(),
            ]),
        );
        let observation =
            probe_control::run_then_kill_direct_child(&command, &[], CONTROL_RUN).await?;
        let baseline = tick_count(&ticks);
        tokio::time::sleep(Duration::from_millis(800)).await;
        let after = tick_count(&ticks);
        let escaped = after > baseline;
        // The escape is the finding; the leak it created is this control's to
        // bound, and it does that by pid, never by image name.
        let leaked = read_pids(&ticks);
        let ended = probe_control::terminate_pids(&self.run_root(), &leaked).await;
        let remaining = tick_count(&ticks);
        tokio::time::sleep(Duration::from_millis(400)).await;
        let settled = tick_count(&ticks) == remaining;
        Ok(BoundaryBreakObservation {
            detail: format!(
                "direct child killed={} exit={:?} after {} ms; ticks {baseline} -> {after} with no job object; {} of {} leaked descendants terminated, writes stopped afterwards={settled}; stdout={:?}",
                observation.killed,
                observation.exit_code,
                observation.elapsed_ms,
                ended,
                leaked.len(),
                observation.stdout.trim()
            ),
            ticks_after_kill: after,
            escaped,
        })
    }

    /// Run the environment fixture with no allowlist: the canary must leak.
    pub async fn environment_inheritance_control(&self) -> Result<bool, HarnessError> {
        self.prepare_roots()?;
        let command = ControlCommand::new(self.child.executable(), self.run_root())
            .with_args(self.child.argv(&["env"]));
        let canary = vec![(
            PROBE_CANARY_NAME.to_owned(),
            format!("control-{}", self.nonce),
        )];
        let observation =
            probe_control::run_without_allowlist(&command, &canary, Duration::from_secs(20))
                .await?;
        Ok(observation
            .stdout
            .contains(&format!("{PROBE_CANARY_NAME}=control-{}", self.nonce)))
    }

    #[must_use]
    pub fn nonce(&self) -> &str {
        &self.nonce
    }
}

fn tick_count(path: &Path) -> u64 {
    std::fs::read_to_string(path).map_or(0, |text| text.lines().count() as u64)
}

/// The pids the fixture wrote beside its tick file, so a control can bound the
/// leak it deliberately created.
fn read_pids(ticks: &Path) -> Vec<u32> {
    let path = PathBuf::from(format!("{}.pids", ticks.display()));
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter_map(|line| line.trim().parse::<u32>().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Sample the tick file, wait, and sample it again.
async fn tick_pair(path: &Path) -> (u64, u64) {
    tokio::time::sleep(Duration::from_millis(600)).await;
    let before = tick_count(path);
    tokio::time::sleep(Duration::from_millis(800)).await;
    let after = tick_count(path);
    (before, after)
}
