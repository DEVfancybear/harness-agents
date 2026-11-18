//! When the computer last started, for the bug report: a process whose log has
//! no exit record and that started before then ended with the computer - a
//! power loss, a hard reset, a blue screen or an update's restart - which no
//! process can write down. Seen in bug report 20261118-160828: a worker with no
//! exit record whose last line was two minutes after Windows' last record
//! before an unexpected shutdown (Kernel-Power 41).

/// The time the computer started, in Unix milliseconds, when this platform
/// says it.
#[must_use]
pub fn booted_ms(now_ms: i64) -> Option<i64> {
    uptime_ms().map(|uptime| now_ms.saturating_sub(uptime))
}

#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "one kernel32 call with no arguments and no pointers"
)]
fn uptime_ms() -> Option<i64> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetTickCount64() -> u64;
    }
    // SAFETY: GetTickCount64 takes no arguments, touches no memory of ours and
    // cannot fail; it returns the milliseconds since the system started.
    let ticks = unsafe { GetTickCount64() };
    i64::try_from(ticks).ok()
}

#[cfg(target_os = "linux")]
fn uptime_ms() -> Option<i64> {
    // `/proc/uptime`: the seconds since boot, then the idle seconds.
    let text = std::fs::read_to_string("/proc/uptime").ok()?;
    let seconds: f64 = text.split_whitespace().next()?.parse().ok()?;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "milliseconds since boot fit"
    )]
    Some((seconds * 1000.0) as i64)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn uptime_ms() -> Option<i64> {
    None
}

/// What a process without an exit record that started at `started_ms` came to,
/// when the computer started after it: it ended with the computer.
#[must_use]
pub fn ended_with_the_computer(started_ms: i64, booted_ms: Option<i64>) -> bool {
    booted_ms.is_some_and(|booted| started_ms < booted)
}

#[cfg(test)]
mod tests {
    /// The computer has been up for some time, and it started in the past.
    #[test]
    fn the_computer_started_before_now() {
        let now = 1_795_000_000_000;
        if let Some(booted) = super::booted_ms(now) {
            assert!(booted <= now && booted > 0, "{booted}");
        }
    }

    #[test]
    fn a_process_started_before_the_boot_ended_with_the_computer() {
        let booted = Some(1_000);
        assert!(super::ended_with_the_computer(999, booted));
        assert!(!super::ended_with_the_computer(1_001, booted));
        assert!(!super::ended_with_the_computer(999, None));
    }
}
