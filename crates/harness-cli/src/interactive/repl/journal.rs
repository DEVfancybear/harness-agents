//! prime-agent's orphan-process journal (`core/orphan-process-journal.ts`).
//!
//! Every kernel this process starts is recorded, one JSON line per state change,
//! in a journal owned by this process: `active` when it is spawned, inactive once
//! it was stopped. A kernel watches its owner and exits with it, but a crash that
//! leaves one behind (or a watchdog that could not run) must not keep a stray
//! Python alive forever, so the next start reaps what a dead owner left active.
//!
//! As in prime-agent a record carries the process's start identity, and a pid is
//! only killed when that identity still matches: a pid the system has since
//! reused for something else is never touched. Identity-free records are reaped
//! on POSIX only; on Windows the kernel's kill-on-close job already took its tree
//! down, and a bare pid could name an unrelated process by now.
//!
//! The journal is one file per owner (`<owner pid>.jsonl`), so two `ha` processes
//! sharing a data directory never write the same file.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::process::Command;

/// The only record version prime-agent writes and reads.
const RECORD_VERSION: u64 = 1;
/// How long one identity or liveness query may take.
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long one tree kill may take (prime-agent's `killOrphanProcess`).
const KILL_TIMEOUT: Duration = Duration::from_secs(10);

/// Where the journals of every owner live.
#[must_use]
pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("repl").join("orphans")
}

/// The journal one owner writes.
#[must_use]
pub fn path_for(dir: &Path, owner: u32) -> PathBuf {
    dir.join(format!("{owner}.jsonl"))
}

/// A process the journal still records as running.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveOrphan {
    pub pid: u32,
    /// Set on records a kernel wrote for its own `bash()` children.
    pub kernel_pid: Option<u32>,
    /// Missing on identity-free records.
    pub process_start_id: Option<String>,
}

/// Append one state change (prime-agent's `recordOrphanProcessState`). Tracking
/// must never make a working kernel fail, so an unwritable journal is ignored.
pub fn record(path: &Path, owner: u32, pid: u32, active: bool, start_id: Option<&str>) {
    if pid == 0 {
        return;
    }
    let mut record = json!({
        "version": RECORD_VERSION,
        "pid": pid,
        "ownerPid": owner,
        "active": active,
        "recordedAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    });
    if active && let Some(start_id) = start_id {
        record["processStartId"] = Value::String(start_id.to_owned());
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    if let Ok(mut file) = options.open(path) {
        let _ = file.write_all(format!("{record}\n").as_bytes());
        let _ = file.sync_all();
    }
}

/// The processes `owner`'s journal still records as active, latest record per pid
/// (prime-agent's `readActiveOrphanProcesses`). A crash can truncate only the final
/// append, so a line that does not parse is skipped.
#[must_use]
pub fn read_active(contents: &str, owner: u32) -> Vec<ActiveOrphan> {
    let mut order = Vec::new();
    let mut latest: HashMap<u32, Value> = HashMap::new();
    for line in contents.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let pid = record["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| *pid > 0);
        let valid = record["version"].as_u64() == Some(RECORD_VERSION)
            && record["ownerPid"].as_u64() == Some(u64::from(owner))
            && record["active"].is_boolean()
            && record["recordedAt"].is_string();
        let (Some(pid), true) = (pid, valid) else {
            continue;
        };
        if latest.insert(pid, record).is_none() {
            order.push(pid);
        }
    }
    order
        .into_iter()
        .filter_map(|pid| {
            let record = &latest[&pid];
            if record["active"].as_bool() != Some(true) {
                return None;
            }
            // A start id that is present but not a string is not a record prime writes.
            let start_id = match record.get("processStartId") {
                None => None,
                Some(Value::String(id)) => Some(id.clone()),
                Some(_) => return None,
            };
            Some(ActiveOrphan {
                pid,
                kernel_pid: record["kernelPid"]
                    .as_u64()
                    .and_then(|pid| u32::try_from(pid).ok()),
                process_start_id: start_id,
            })
        })
        .collect()
}

/// Whether a journaled process may be killed (prime-agent's `shouldReapOrphanProcess`):
/// with an identity, only while the pid still names that same process; without one,
/// only on POSIX.
#[must_use]
pub fn reap_decision(recorded: Option<&str>, current: Option<&str>, windows: bool) -> bool {
    match recorded {
        None => !windows,
        Some(recorded) => current == Some(recorded),
    }
}

/// Reap what dead owners left active, then drop their journals. `own` is this
/// process, whose journal is never touched here.
pub async fn reap_stale(dir: &Path, own: u32) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(owner) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".jsonl"))
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        if owner == own || process_alive(owner).await {
            continue;
        }
        let contents = std::fs::read_to_string(&path).unwrap_or_default();
        for orphan in read_active(&contents, owner) {
            let current = match orphan.process_start_id {
                Some(_) => process_start_id(orphan.pid).await,
                None => None,
            };
            if reap_decision(
                orphan.process_start_id.as_deref(),
                current.as_deref(),
                cfg!(windows),
            ) {
                kill_tree(orphan.pid).await;
            }
        }
        let _ = std::fs::remove_file(&path);
    }
}

/// Remove `owner`'s own journal once nothing in it is active any more (prime-agent's
/// `clearOrphanProcessJournal` after a clean exit).
pub fn clear_if_idle(path: &Path, owner: u32) {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return;
    };
    if read_active(&contents, owner).is_empty() {
        let _ = std::fs::remove_file(path);
    }
}

fn quiet(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // A bare helper name must not resolve to a planted copy in the working directory.
        .env("NoDefaultCurrentDirectoryInExePath", "1")
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        command.creation_flags(0x0800_0000);
    }
    command
}

/// Absolute paths for Windows helpers, as the runtime's `_system32` builds them.
#[cfg(windows)]
fn system32(parts: &[&str]) -> PathBuf {
    let mut path = PathBuf::from(
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows")),
    );
    path.push("System32");
    for part in parts {
        path.push(part);
    }
    path
}

async fn output(mut command: Command, limit: Duration) -> Option<std::process::Output> {
    tokio::time::timeout(limit, command.output())
        .await
        .ok()
        .and_then(Result::ok)
}

/// The process's start identity, in the exact format the runtime's `bash.py`
/// (`_process_start_id`) and prime-agent's session lease write it.
pub async fn process_start_id(pid: u32) -> Option<String> {
    #[cfg(windows)]
    {
        let mut command = quiet(system32(&["WindowsPowerShell", "v1.0", "powershell.exe"]));
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "([System.Diagnostics.Process]::GetProcessById({pid})).StartTime.ToUniversalTime().Ticks"
            ),
        ]);
        let output = output(command, QUERY_TIMEOUT).await?;
        let ticks = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (!ticks.is_empty() && ticks.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| format!("win:{ticks}"))
    }
    #[cfg(not(windows))]
    {
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            && let Some(close) = stat.rfind(')')
        {
            let fields = stat
                .get(close + 2..)
                .unwrap_or_default()
                .split(' ')
                .collect::<Vec<_>>();
            if let Some(start) = fields.get(19).filter(|field| !field.is_empty()) {
                return Some(format!("proc:{start}"));
            }
        }
        let ps = if cfg!(target_os = "macos") {
            "/bin/ps"
        } else {
            "ps"
        };
        let mut command = quiet(ps);
        command.args(["-p", &pid.to_string(), "-o", "lstart="]);
        let output = output(command, QUERY_TIMEOUT).await?;
        let started = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (!started.is_empty()).then(|| format!("ps:{started}"))
    }
}

/// Whether `pid` still runs. An answer that cannot be had is "alive": a journal
/// kept a little longer costs nothing, reaping a live owner's kernel would.
pub async fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if pid == std::process::id() {
        return true;
    }
    #[cfg(windows)]
    {
        let mut command = quiet(system32(&["tasklist.exe"]));
        command.args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"]);
        let Some(output) = output(command, QUERY_TIMEOUT).await else {
            return true;
        };
        if !output.status.success() {
            return true;
        }
        tasklist_alive(pid, &String::from_utf8_lossy(&output.stdout)).unwrap_or(true)
    }
    #[cfg(not(windows))]
    {
        if Path::new("/proc/self").exists() {
            return Path::new(&format!("/proc/{pid}")).exists();
        }
        let mut command = quiet("kill");
        command.args(["-0", &pid.to_string()]);
        output(command, QUERY_TIMEOUT)
            .await
            .is_none_or(|output| output.status.success())
    }
}

/// `tasklist /FO CSV /NH`: a row whose second field is exactly the pid means
/// alive, nothing but `INFO:` lines means gone, anything else is unknown.
#[cfg_attr(not(windows), allow(dead_code))]
fn tasklist_alive(pid: u32, stdout: &str) -> Option<bool> {
    let lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    for line in &lines {
        if let Some(rest) = line.strip_prefix('"')
            && let Some((_, rest)) = rest.split_once("\",\"")
            && let Some((reported, _)) = rest.split_once('"')
            && reported.parse::<u32>().ok() == Some(pid)
        {
            return Some(true);
        }
    }
    (!lines.is_empty()
        && lines
            .iter()
            .all(|line| line.to_ascii_uppercase().starts_with("INFO:")))
    .then_some(false)
}

/// Kill a journaled process and its tree (prime-agent's `killOrphanProcess`):
/// `taskkill /T` by absolute path on Windows, the process group then the pid
/// elsewhere.
pub async fn kill_tree(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let mut command = quiet(system32(&["taskkill.exe"]));
        command.args(["/F", "/T", "/PID", &pid.to_string()]);
        output(command, KILL_TIMEOUT)
            .await
            .is_some_and(|output| output.status.success())
    }
    #[cfg(not(windows))]
    {
        for target in [format!("-{pid}"), pid.to_string()] {
            let mut command = quiet("kill");
            command.args(["-KILL", "--", &target]);
            if output(command, KILL_TIMEOUT)
                .await
                .is_some_and(|output| output.status.success())
            {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{ActiveOrphan, clear_if_idle, path_for, read_active, reap_decision, record};

    #[test]
    fn the_latest_record_per_pid_decides_and_foreign_or_broken_lines_are_skipped() {
        let lines = [
            r#"{"version":1,"pid":10,"ownerPid":7,"active":true,"processStartId":"win:1","recordedAt":"t"}"#,
            r#"{"version":1,"pid":11,"ownerPid":7,"active":true,"recordedAt":"t"}"#,
            r#"{"version":1,"pid":10,"ownerPid":7,"active":false,"recordedAt":"t"}"#,
            r#"{"version":1,"pid":12,"ownerPid":8,"active":true,"recordedAt":"t"}"#,
            r#"{"version":2,"pid":13,"ownerPid":7,"active":true,"recordedAt":"t"}"#,
            r#"{"version":1,"pid":14,"ownerPid":7,"kernelPid":11,"active":true,"processStartId":"proc:5","recordedAt":"t"}"#,
            r#"{"version":1,"pid":15,"ownerPid":7,"act"#,
        ]
        .join("\n");
        assert_eq!(
            read_active(&lines, 7),
            vec![
                ActiveOrphan {
                    pid: 11,
                    kernel_pid: None,
                    process_start_id: None,
                },
                ActiveOrphan {
                    pid: 14,
                    kernel_pid: Some(11),
                    process_start_id: Some("proc:5".to_owned()),
                },
            ]
        );
    }

    #[test]
    fn only_a_matching_identity_is_reaped_and_bare_pids_never_on_windows() {
        assert!(reap_decision(Some("win:1"), Some("win:1"), true));
        assert!(!reap_decision(Some("win:1"), Some("win:2"), true));
        assert!(!reap_decision(Some("win:1"), None, true));
        assert!(!reap_decision(None, None, true));
        assert!(reap_decision(None, None, false));
    }

    #[test]
    fn records_round_trip_and_an_idle_journal_is_removed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = path_for(directory.path(), 7);
        record(&path, 7, 42, true, Some("win:9"));
        let contents = std::fs::read_to_string(&path).expect("journal");
        assert_eq!(
            read_active(&contents, 7),
            vec![ActiveOrphan {
                pid: 42,
                kernel_pid: None,
                process_start_id: Some("win:9".to_owned()),
            }]
        );
        clear_if_idle(&path, 7);
        assert!(path.is_file(), "an active record keeps the journal");
        record(&path, 7, 42, false, None);
        clear_if_idle(&path, 7);
        assert!(!path.exists());
    }

    #[test]
    fn tasklist_rows_are_matched_exactly() {
        assert_eq!(
            super::tasklist_alive(12, "\"python.exe\",\"12\",\"Console\",\"1\",\"9 K\""),
            Some(true)
        );
        assert_eq!(
            super::tasklist_alive(12, "\"python.exe\",\"123\",\"Console\",\"1\",\"9 K\""),
            None
        );
        assert_eq!(
            super::tasklist_alive(
                12,
                "INFO: No tasks are running which match the specified criteria."
            ),
            Some(false)
        );
    }

    /// A dead owner's journal is reaped and removed; an identity that no longer
    /// matches kills nothing. This process's own journal is left alone.
    #[tokio::test]
    async fn a_dead_owners_journal_is_reaped_and_removed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        // A process that already exited is an owner that is certainly gone.
        let mut exited = if cfg!(windows) {
            let mut command = std::process::Command::new("cmd");
            command.args(["/C", "exit"]);
            command
        } else {
            std::process::Command::new("true")
        };
        let mut child = exited.spawn().expect("a short process");
        let dead_owner = child.id();
        child.wait().expect("it exits");
        let stale = path_for(directory.path(), dead_owner);
        record(&stale, dead_owner, 4_000_000_001, true, Some("win:0"));
        let own = std::process::id();
        let mine = path_for(directory.path(), own);
        record(&mine, own, 4_000_000_002, true, Some("win:0"));
        super::reap_stale(directory.path(), own).await;
        assert!(!stale.exists(), "a dead owner's journal is dropped");
        assert!(mine.is_file(), "this process's journal is its own");
    }
}
