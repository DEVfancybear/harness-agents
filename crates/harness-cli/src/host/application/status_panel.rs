//! `/status` as a panel of sections: the session, the model it calls, what
//! it has spent, the workspace and where its files live.
//!
//! The facts come from the header the status line also reads and from the
//! provider check, both lines of text. This module reads them into labelled
//! fields once, so the panel names each fact once: the header's `Project:`
//! line holds the folder and the provider, and the provider check started
//! every one of its seven lines with `Provider:`.

use super::events::{BadgeKind, RefLine};

/// What `/status` adds to the header.
pub struct Extra {
    pub cost: String,
    pub phase: String,
    /// The project id, or why there is none yet.
    pub project: String,
    /// [`super::bounds::describe`]'s line.
    pub bounds: String,
    /// [`super::service::SessionPort::provider_diagnostics`]' lines.
    pub provider: Vec<String>,
}

/// The `/status` panel.
#[must_use]
#[allow(clippy::too_many_lines, reason = "one panel, a section at a time")]
pub fn rows(header: &[String], extra: &Extra) -> Vec<RefLine> {
    let facts: Vec<(String, String)> = header.iter().flat_map(|line| facts(line)).collect();
    let fact = |label: &str| {
        facts
            .iter()
            .find(|(name, _)| name == label)
            .map(|(_, value)| value.clone())
    };
    let version = header
        .iter()
        .find_map(|line| line.strip_prefix("Harness Agents "))
        .unwrap_or_default();
    let mut rows = Vec::new();
    let mut section = |title: &str, note: &str, fields: Vec<RefLine>| {
        if fields.is_empty() {
            return;
        }
        if !rows.is_empty() {
            rows.push(RefLine::Blank);
        }
        rows.push(RefLine::Heading {
            title: title.to_owned(),
            note: note.to_owned(),
        });
        rows.extend(fields);
    };

    let mut session = Vec::new();
    // The header's `Session: new` says nothing: every session starts so.
    if let Some(value) = fact("Mode") {
        session.push(RefLine::field("Mode", value));
    }
    session.push(RefLine::field("Phase", extra.phase.clone()));
    if let Some(value) = fact("Permissions") {
        session.push(RefLine::Field {
            label: "Permissions".to_owned(),
            tone: if value.contains("full") || value.contains("yolo") {
                BadgeKind::Warn
            } else {
                BadgeKind::Neutral
            },
            value,
        });
    }
    let note = if version.is_empty() {
        String::new()
    } else {
        format!("Harness Agents {version}")
    };
    section("Session", &note, session);

    let service = fact("Service");
    let (model, endpoint) = service.as_deref().map_or((None, None), |label| {
        label
            .split_once(" via ")
            .map_or((Some(label.to_owned()), None), |(model, endpoint)| {
                (Some(model.to_owned()), Some(endpoint.to_owned()))
            })
    });
    let mut provider = Vec::new();
    if let Some(model) = &model {
        provider.push(RefLine::Field {
            label: "Model".to_owned(),
            value: model.clone(),
            tone: BadgeKind::Accent,
        });
    }
    if let Some(endpoint) = &endpoint {
        provider.push(RefLine::field("Endpoint", endpoint.clone()));
    }
    // The provider check reads the configuration: after a `/model` choice
    // its provider is not the one in use, and is shown on its own.
    let configured = extra
        .provider
        .iter()
        .find_map(|line| line.strip_prefix("Provider: ready, would call "));
    let in_use =
        model.is_none() || configured.is_none_or(|called| Some(called) == model.as_deref());
    if in_use {
        provider.extend(provider_rows(
            &extra.provider,
            model.as_deref(),
            endpoint.as_deref(),
        ));
        section("Model", "", provider);
    } else {
        section("Model", "", provider);
        section(
            "Default",
            "the configured provider, not the model in use",
            provider_rows(&extra.provider, None, None),
        );
    }

    let mut usage = Vec::new();
    if let Some(context) = fact("Context") {
        let (used, windows): (Vec<&str>, Vec<&str>) = context
            .split(" · ")
            .partition(|part| !part.ends_with("% left"));
        if !used.is_empty() {
            usage.push(RefLine::field("Context", used.join(" · ")));
        }
        if !windows.is_empty() {
            let lowest = windows
                .iter()
                .filter_map(|window| {
                    window
                        .strip_suffix("% left")
                        .and_then(|rest| rest.rsplit(' ').next())
                        .and_then(|number| number.parse::<f64>().ok())
                })
                .fold(100.0_f64, f64::min);
            usage.push(RefLine::Field {
                label: "Limits".to_owned(),
                value: windows.join(" · "),
                tone: if lowest <= 40.0 {
                    BadgeKind::Warn
                } else {
                    BadgeKind::Ok
                },
            });
        }
    }
    if let Some(cache) = fact("Cache") {
        usage.push(RefLine::field("Cache", cache));
    }
    usage.push(RefLine::field("Cost", extra.cost.clone()));
    let bounds = extra.bounds.trim_start_matches("Bounds:").trim();
    usage.push(RefLine::field("Bounds", bounds.to_owned()));
    section("Usage", "", usage);

    let mut workspace = Vec::new();
    if let Some(folder) = fact("Project") {
        workspace.push(RefLine::field("Folder", folder));
    }
    workspace.push(RefLine::field("Project", extra.project.clone()));
    for label in ["Git", "Branch", "AGENTS.md"] {
        if let Some(value) = fact(label) {
            workspace.push(RefLine::field(label, value));
        }
    }
    section("Workspace", "", workspace);

    let files = ["Config", "Data", "Store"]
        .into_iter()
        .filter_map(|label| fact(label).map(|value| RefLine::field(label, value)))
        .collect();
    section("Files", "", files);
    rows
}

/// A header line's facts: `Project: C:\work    Provider: key set` holds two.
/// The header lines up its values with spaces (`Git:     clean`), so a run
/// of four spaces separates one fact from the next only when a label follows.
fn facts(line: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = line;
    while let Some((label, tail)) = rest.split_once(':') {
        let label = label.trim();
        if label.is_empty() || label.contains(char::is_whitespace) {
            break;
        }
        let tail = tail.trim_start();
        // The next fact starts after four spaces with a `Label:` of its own.
        let next = tail.match_indices("    ").find_map(|(at, _)| {
            let after = tail[at..].trim_start();
            after
                .split_once(':')
                .filter(|(word, _)| {
                    !word.is_empty() && !word.contains(char::is_whitespace) && !word.contains('\\')
                })
                .map(|_| (at, tail.len() - after.len()))
        });
        let Some((end, start)) = next else {
            found.push((label.to_owned(), tail.trim().to_owned()));
            break;
        };
        found.push((label.to_owned(), tail[..end].trim().to_owned()));
        rest = &tail[start..];
    }
    found
}

/// `/model`'s panel: the model in use, then the provider check.
#[must_use]
pub fn model_rows(service: &str, provider: &[String]) -> Vec<RefLine> {
    let (model, endpoint) = service
        .split_once(" via ")
        .map_or((service, None), |(model, endpoint)| (model, Some(endpoint)));
    let mut rows = vec![RefLine::Field {
        label: "Model".to_owned(),
        value: model.to_owned(),
        tone: BadgeKind::Accent,
    }];
    if let Some(endpoint) = endpoint {
        rows.push(RefLine::field("Endpoint", endpoint.to_owned()));
    }
    rows.extend(provider_rows(provider, Some(model), endpoint));
    rows
}

/// The provider check's lines as fields. The configured endpoint and model
/// are shown only where they differ from the ones the session calls: a
/// `/model` choice outranks the configuration.
fn provider_rows(lines: &[String], model: Option<&str>, endpoint: Option<&str>) -> Vec<RefLine> {
    let tone = |label: &str, value: String, tone: BadgeKind| RefLine::Field {
        label: label.to_owned(),
        value,
        tone,
    };
    // Beside the model in use, the configured endpoint and model are named as
    // such; on their own (the default provider) they are just the endpoint
    // and the model.
    let alone = model.is_none();
    let configured = |label: &str, value: &str, source: &str, current: Option<&str>| {
        if current.is_some_and(|current| current == value) {
            None
        } else {
            let label = if alone {
                label.trim_start_matches("Config ")
            } else {
                label
            };
            let label = if label == "URL" { "Endpoint" } else { label };
            let label = if label == "model" { "Model" } else { label };
            Some(RefLine::field(label, format!("{value} ({source})")))
        }
    };
    let mut rows = Vec::new();
    for line in lines {
        let fact = line.strip_prefix("Provider: ").unwrap_or(line);
        let row = if let Some(source) = fact.strip_prefix("credential from ") {
            Some(RefLine::field("Credential", source.to_owned()))
        } else if let Some(protection) = fact.strip_prefix("credential file protection: ") {
            Some(RefLine::field("Protection", protection.to_owned()))
        } else if fact.starts_with("no credential for ") {
            Some(tone("Credential", fact.to_owned(), BadgeKind::Warn))
        } else if let Some(value) =
            fact.strip_prefix("provider.endpoint not set, using the default ")
        {
            configured("Config URL", value, "default", endpoint)
        } else if let Some(value) = fact.strip_prefix("provider.model not set, using the default ")
        {
            configured("Config model", value, "default", model)
        } else if let Some(rest) = fact
            .strip_prefix("endpoint ")
            .filter(|rest| rest.contains('='))
        {
            environment(rest)
                .and_then(|(variable, value)| configured("Config URL", &value, &variable, endpoint))
        } else if let Some(rest) = fact
            .strip_prefix("model ")
            .filter(|rest| rest.contains('='))
        {
            environment(rest)
                .and_then(|(variable, value)| configured("Config model", &value, &variable, model))
        } else if let Some(rest) = fact.strip_prefix("provider.endpoint=") {
            let (value, source) = layered(rest);
            configured("Config URL", value, source, endpoint)
        } else if let Some(rest) = fact.strip_prefix("provider.model=") {
            let (value, source) = layered(rest);
            configured("Config model", value, source, model)
        } else if let Some(called) = fact.strip_prefix("ready, would call ") {
            Some(tone(
                "Status",
                if alone || model == Some(called) {
                    "ready".to_owned()
                } else {
                    format!("ready, configured for {called}")
                },
                BadgeKind::Ok,
            ))
        } else if fact.starts_with("endpoint answered") {
            Some(tone(
                "Network",
                "endpoint answers".to_owned(),
                BadgeKind::Ok,
            ))
        } else if let Some(reason) = fact.strip_prefix("endpoint did not answer") {
            Some(tone(
                "Network",
                format!("endpoint did not answer{reason}"),
                BadgeKind::Warn,
            ))
        } else if let Some(reason) = fact.strip_prefix("not ready") {
            Some(tone(
                "Status",
                format!("not ready{reason}"),
                BadgeKind::Warn,
            ))
        } else if fact.starts_with("configuration unavailable") {
            Some(tone("Status", fact.to_owned(), BadgeKind::Warn))
        } else if rows.is_empty() {
            Some(RefLine::field("Provider", fact.to_owned()))
        } else {
            Some(RefLine::Text(fact.to_owned()))
        };
        rows.extend(row);
    }
    rows
}

/// `DEEPSEEK_ENDPOINT=https://x (environment)` as the variable and the value.
fn environment(rest: &str) -> Option<(String, String)> {
    let (variable, value) = rest.split_once('=')?;
    let (value, _) = layered(value);
    Some((variable.to_owned(), value.to_owned()))
}

/// `https://x (user config)` as the value and the layer it came from.
fn layered(rest: &str) -> (&str, &str) {
    rest.strip_suffix(')')
        .and_then(|rest| rest.rsplit_once(" ("))
        .unwrap_or((rest, "config"))
}

#[cfg(test)]
mod tests {
    use super::{Extra, rows};
    use crate::interactive::events::RefLine;

    fn panel() -> Vec<RefLine> {
        let header = [
            "Harness Agents 0.6.9",
            r"Project: F:\work\ninja    Provider: credential from openai-codex login",
            "Session: new    Mode: trusted host",
            "Git:     clean",
            "Branch: master",
            "AGENTS.md: 2 files",
            r"Config:  C:\Users\me\.ha\config.toml [default]",
            r"Data:    C:\Users\me\AppData\Local\HarnessAgents\data [default]",
            r"Store:   C:\Users\me\AppData\Local\HarnessAgents\data\projects\ab12",
            "Service: gpt-5.6-terra via https://chatgpt.com/backend-api/codex",
            "Cache: 91%",
            "Context: 42% · 113k/272k · 7d 68% left",
            "Permissions: full-auto",
        ]
        .map(str::to_owned);
        rows(
            &header,
            &Extra {
                cost: "$0.3585 (children $0.2072)".to_owned(),
                phase: "running".to_owned(),
                project: "project_01a0 (store scope for this workspace)".to_owned(),
                bounds: "Bounds:  steps 8, tool calls 16; up to 3 automatic continuation(s)"
                    .to_owned(),
                provider: [
                    "Provider: openai-codex (responses)",
                    "Provider: credential from login file (value hidden)",
                    "Provider: credential file protection: owner only",
                    "Provider: provider.endpoint not set, using the default https://chatgpt.com/backend-api/codex",
                    "Provider: provider.model=gpt-5.6-terra (user config)",
                    "Provider: ready, would call gpt-5.6-terra",
                    "Provider: endpoint answered a TCP connection",
                ]
                .map(str::to_owned)
                .to_vec(),
            },
        )
    }

    fn labels(rows: &[RefLine]) -> Vec<String> {
        rows.iter()
            .filter_map(|row| match row {
                RefLine::Field { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect()
    }

    /// The screenshot's panel said `Project:` twice and `Provider:` seven
    /// times; each fact now has one label, under a heading.
    #[test]
    fn every_fact_is_named_once_under_its_section() {
        let rows = panel();
        let labels = labels(&rows);
        let mut unique = labels.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), labels.len(), "{labels:?}");
        let headings: Vec<String> = rows
            .iter()
            .filter_map(|row| match row {
                RefLine::Heading { title, .. } => Some(title.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            headings,
            ["Session", "Model", "Usage", "Workspace", "Files"]
        );
        let plain: Vec<String> = rows.iter().map(RefLine::plain).collect();
        for expected in [
            r"Folder: F:\work\ninja",
            "Project: project_01a0 (store scope for this workspace)",
            "Model: gpt-5.6-terra",
            "Endpoint: https://chatgpt.com/backend-api/codex",
            "Provider: openai-codex (responses)",
            "Status: ready",
            "Network: endpoint answers",
            "Context: 42% · 113k/272k",
            "Limits: 7d 68% left",
            "Bounds: steps 8, tool calls 16; up to 3 automatic continuation(s)",
            "Mode: trusted host",
            "Phase: running",
        ] {
            assert!(
                plain.iter().any(|line| line == expected),
                "{expected} in {plain:#?}"
            );
        }
        // The configured endpoint and model are the ones in use: said once.
        assert!(
            !labels.iter().any(|label| label.starts_with("Config ")),
            "{labels:?}"
        );
    }

    #[test]
    fn a_configured_model_other_than_the_one_in_use_is_shown() {
        let mut header = vec!["Service: deepseek-v4-flash via https://api.deepseek.com".to_owned()];
        header.push("Permissions: ask".to_owned());
        let rows = rows(
            &header,
            &Extra {
                cost: "$0".to_owned(),
                phase: "idle".to_owned(),
                project: "p".to_owned(),
                bounds: "Bounds: steps 8".to_owned(),
                provider: vec![
                    "Provider: openai-codex (responses)".to_owned(),
                    "Provider: provider.model=gpt-5.6-terra (user config)".to_owned(),
                    "Provider: ready, would call gpt-5.6-terra".to_owned(),
                ],
            },
        );
        // Seen live: the Model section said gpt-6-luna and, under it,
        // `Provider deepseek`. The configured provider has a section of its own.
        let plain: Vec<String> = rows.iter().map(RefLine::plain).collect();
        let default = plain
            .iter()
            .position(|line| line.starts_with("Default"))
            .unwrap_or_else(|| panic!("no Default section in {plain:#?}"));
        let model = plain
            .iter()
            .position(|line| line == "Model")
            .expect("a Model section");
        assert!(model < default, "{plain:#?}");
        assert_eq!(plain[model + 1], "Model: deepseek-v4-flash");
        for expected in [
            "Provider: openai-codex (responses)",
            "Model: gpt-5.6-terra (user config)",
            "Status: ready",
        ] {
            let at = plain.iter().position(|line| line == expected);
            assert!(
                at.is_some_and(|at| at > default),
                "{expected} under Default in {plain:#?}"
            );
        }
    }
}
