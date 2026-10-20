//! What a provider says about the account's limits, read off every response.
//!
//! Nothing here knows a model or a plan: the headers name their own windows.
//! Three families are read, each the way its provider documents it:
//!
//! - `x-codex-{primary,secondary}-*`: a `ChatGPT` subscription's usage windows (the
//!   percentage used, the window length, when it resets);
//! - `x-ratelimit-{limit,remaining,reset}-<kind>`: `OpenAI`-style rate limits,
//!   which other OpenAI-compatible providers send too;
//! - `anthropic-ratelimit-<kind>-{limit,remaining,reset}`: Anthropic's.
//!
//! The latest snapshot of each provider is kept for the life of the process, so
//! `/usage` can show it between turns.

use reqwest::header::HeaderMap;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One limit window: how much of it is used and when it starts over.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LimitWindow {
    /// What the provider calls it (`5h`, `7d`, `requests`, `tokens`).
    pub label: String,
    /// The share used, 0-100, when the provider says or it can be worked out.
    pub used_percent: Option<f64>,
    pub remaining: Option<String>,
    pub limit: Option<String>,
    /// When it resets, as the provider spells it or as time from the response.
    pub resets: Option<String>,
}

/// Every window one response reported, and when.
#[derive(Clone, Debug, PartialEq)]
pub struct LimitSnapshot {
    pub at: SystemTime,
    pub windows: Vec<LimitWindow>,
}

impl LimitSnapshot {
    /// The window closest to running out, for a status line.
    #[must_use]
    pub fn tightest(&self) -> Option<&LimitWindow> {
        self.windows
            .iter()
            .filter(|window| window.used_percent.is_some())
            .max_by(|left, right| {
                left.used_percent
                    .unwrap_or(0.0)
                    .total_cmp(&right.used_percent.unwrap_or(0.0))
            })
    }

    /// One line per window, for `/usage`.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.windows
            .iter()
            .map(|window| {
                let mut parts = Vec::new();
                if let Some(percent) = window.used_percent {
                    parts.push(format!("{percent:.0}% used"));
                }
                match (&window.remaining, &window.limit) {
                    (Some(remaining), Some(limit)) => {
                        parts.push(format!("{remaining}/{limit} left"));
                    }
                    (Some(remaining), None) => parts.push(format!("{remaining} left")),
                    _ => {}
                }
                if let Some(resets) = &window.resets {
                    parts.push(format!("resets {resets}"));
                }
                format!("{}: {}", window.label, parts.join(", "))
            })
            .collect()
    }
}

fn registry() -> &'static Mutex<BTreeMap<String, LimitSnapshot>> {
    static LATEST: OnceLock<Mutex<BTreeMap<String, LimitSnapshot>>> = OnceLock::new();
    LATEST.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Keep what a response of `provider_id` said about its limits, if anything.
pub fn record(provider_id: &str, headers: &HeaderMap) {
    let windows = parse(headers, SystemTime::now());
    if windows.is_empty() {
        return;
    }
    if let Ok(mut latest) = registry().lock() {
        latest.insert(
            provider_id.to_owned(),
            LimitSnapshot {
                at: SystemTime::now(),
                windows,
            },
        );
    }
}

/// The last limits `provider_id` reported in this process.
#[must_use]
pub fn latest(provider_id: &str) -> Option<LimitSnapshot> {
    registry().lock().ok()?.get(provider_id).cloned()
}

/// The windows the headers describe, in the order the families are listed above.
#[must_use]
pub fn parse(headers: &HeaderMap, now: SystemTime) -> Vec<LimitWindow> {
    let text = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let mut windows = Vec::new();

    for slot in ["primary", "secondary"] {
        let Some(used) = text(&format!("x-codex-{slot}-used-percent"))
            .and_then(|value| value.parse::<f64>().ok())
        else {
            continue;
        };
        let label = text(&format!("x-codex-{slot}-window-minutes"))
            .and_then(|value| value.parse::<u64>().ok())
            .map_or_else(|| slot.to_owned(), window_label);
        let resets = text(&format!("x-codex-{slot}-reset-after-seconds"))
            .and_then(|value| value.parse::<u64>().ok())
            .or_else(|| {
                text(&format!("x-codex-{slot}-reset-at"))
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(|at| at.saturating_sub(unix_seconds(now)))
            })
            .map(|seconds| format!("in {}", duration_label(Duration::from_secs(seconds))));
        windows.push(LimitWindow {
            label,
            used_percent: Some(used.clamp(0.0, 100.0)),
            resets,
            ..LimitWindow::default()
        });
    }

    // `x-ratelimit-<field>-<kind>` and `anthropic-ratelimit-<kind>-<field>`, grouped
    // by kind in the order the headers came.
    let mut kinds: Vec<(String, LimitWindow)> = Vec::new();
    for (name, value) in headers {
        let Ok(value) = value.to_str() else { continue };
        let name = name.as_str();
        let parsed = name
            .strip_prefix("x-ratelimit-")
            .and_then(|rest| rest.split_once('-'))
            .or_else(|| {
                name.strip_prefix("anthropic-ratelimit-")
                    .and_then(|rest| rest.rsplit_once('-'))
                    .map(|(kind, field)| (field, kind))
            });
        let Some((field, kind)) = parsed else {
            continue;
        };
        if !matches!(field, "limit" | "remaining" | "reset") {
            continue;
        }
        let index = kinds
            .iter()
            .position(|(known, _)| known == kind)
            .unwrap_or_else(|| {
                kinds.push((
                    kind.to_owned(),
                    LimitWindow {
                        label: kind.to_owned(),
                        ..LimitWindow::default()
                    },
                ));
                kinds.len() - 1
            });
        let window = &mut kinds[index].1;
        let value = value.trim().to_owned();
        match field {
            "limit" => window.limit = Some(value),
            "remaining" => window.remaining = Some(value),
            _ => window.resets = Some(value),
        }
    }
    for (_, mut window) in kinds {
        let number = |value: &Option<String>| value.as_deref()?.parse::<f64>().ok();
        if let (Some(remaining), Some(limit)) = (number(&window.remaining), number(&window.limit))
            && limit > 0.0
        {
            window.used_percent = Some(((limit - remaining) / limit * 100.0).clamp(0.0, 100.0));
        }
        if window.limit.is_some() || window.remaining.is_some() {
            windows.push(window);
        }
    }
    windows
}

fn unix_seconds(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// `300` minutes is `5h`, `10080` is `7d`.
fn window_label(minutes: u64) -> String {
    if minutes > 0 && minutes.is_multiple_of(24 * 60) {
        format!("{}d", minutes / (24 * 60))
    } else if minutes > 0 && minutes.is_multiple_of(60) {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

/// `2h 5m`, `3d 4h`, `40s`.
#[must_use]
pub fn duration_label(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (days, hours, minutes) = (seconds / 86_400, seconds / 3_600 % 24, seconds / 60 % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::{LimitSnapshot, parse};
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                HeaderName::from_bytes(name.as_bytes()).expect("name"),
                HeaderValue::from_str(value).expect("value"),
            );
        }
        map
    }

    #[test]
    fn a_subscription_names_its_windows_and_their_resets() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let windows = parse(
            &headers(&[
                ("x-codex-primary-used-percent", "34.5"),
                ("x-codex-primary-window-minutes", "300"),
                ("x-codex-primary-reset-after-seconds", "7500"),
                ("x-codex-secondary-used-percent", "12"),
                ("x-codex-secondary-window-minutes", "10080"),
                ("x-codex-secondary-reset-at", "1003600"),
            ]),
            now,
        );
        let snapshot = LimitSnapshot { at: now, windows };
        assert_eq!(
            snapshot.lines(),
            [
                "5h: 34% used, resets in 2h 5m",
                "7d: 12% used, resets in 1h 0m"
            ]
        );
        assert_eq!(
            snapshot.tightest().map(|window| window.label.as_str()),
            Some("5h")
        );
    }

    #[test]
    fn rate_limit_headers_of_either_family_are_grouped_by_kind() {
        let windows = parse(
            &headers(&[
                ("x-ratelimit-limit-requests", "5000"),
                ("x-ratelimit-remaining-requests", "4000"),
                ("x-ratelimit-reset-requests", "12ms"),
                ("anthropic-ratelimit-input-tokens-limit", "400000"),
                ("anthropic-ratelimit-input-tokens-remaining", "300000"),
                ("content-type", "text/event-stream"),
            ]),
            SystemTime::now(),
        );
        let labels = windows
            .iter()
            .map(|window| (window.label.as_str(), window.used_percent.map(f64::round)))
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [("requests", Some(20.0)), ("input-tokens", Some(25.0))]
        );
        assert_eq!(windows[0].resets.as_deref(), Some("12ms"));
    }

    #[test]
    fn a_response_without_limit_headers_reports_nothing() {
        assert!(
            parse(
                &headers(&[("content-type", "application/json")]),
                SystemTime::now()
            )
            .is_empty()
        );
    }
}
