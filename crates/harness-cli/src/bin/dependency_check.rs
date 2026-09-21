#![forbid(unsafe_code)]

//! Internal dependency-edge checker (M0-02).
//!
//! It reads the root workspace's crate manifests and the committed allowlist
//! `schemas/dependency-allowlist.v1.json`, then reports every internal
//! (`harness-*`) edge that is not declared, plus every declared-forbidden edge.
//!
//! `--extra-edge from->to` injects a synthetic edge so a negative control can
//! prove the checker fails when it must, without editing a real manifest.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::Parser;
use serde_json::{Value, json};

#[derive(Debug, Parser)]
#[command(name = "dependency_check", about)]
struct Cli {
    /// Workspace root that contains `crates/` and `schemas/`.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// Synthetic internal edge `from->to`, repeatable. Test negative control only.
    #[arg(long = "extra-edge")]
    extra_edges: Vec<String>,
}

#[derive(Debug, PartialEq)]
enum Violation {
    UnlistedCrate {
        crate_name: String,
    },
    UndeclaredEdge {
        from: String,
        to: String,
    },
    ForbiddenEdge {
        from: String,
        to: String,
        reason: String,
    },
    UnknownCrateInAllowlist {
        crate_name: String,
    },
    MissingAllowlistFile {
        path: String,
    },
}

impl Violation {
    fn to_json(&self) -> Value {
        match self {
            Self::UnlistedCrate { crate_name } => json!({
                "code": "gate_configuration_error",
                "kind": "unlisted_crate",
                "crate": crate_name,
            }),
            Self::UndeclaredEdge { from, to } => json!({
                "code": "gate_configuration_error",
                "kind": "undeclared_edge",
                "from": from,
                "to": to,
            }),
            Self::ForbiddenEdge { from, to, reason } => json!({
                "code": "gate_configuration_error",
                "kind": "forbidden_edge",
                "from": from,
                "to": to,
                "reason": reason,
            }),
            Self::UnknownCrateInAllowlist { crate_name } => json!({
                "code": "gate_configuration_error",
                "kind": "unknown_crate_in_allowlist",
                "crate": crate_name,
            }),
            Self::MissingAllowlistFile { path } => json!({
                "code": "gate_configuration_error",
                "kind": "missing_allowlist",
                "path": path,
            }),
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok((crates, edges, violations)) => {
            let report = json!({
                "schema_version": 1,
                "command": "dependency_check",
                "status": if violations.is_empty() { "ok" } else { "error" },
                "crate_count": crates,
                "edge_count": edges,
                "violations": violations.iter().map(Violation::to_json).collect::<Vec<_>>(),
            });
            println!("{report}");
            if violations.is_empty() {
                ExitCode::SUCCESS
            } else {
                // gate_configuration_error maps to exit code 2.
                ExitCode::from(2)
            }
        }
        Err(message) => {
            println!(
                "{}",
                json!({
                    "schema_version": 1,
                    "command": "dependency_check",
                    "status": "error",
                    "violations": [{"code": "gate_configuration_error", "kind": "checker_failed", "message": message}],
                })
            );
            ExitCode::from(2)
        }
    }
}

type CheckResult = Result<(usize, usize, Vec<Violation>), String>;

#[allow(clippy::too_many_lines)]
fn run(cli: &Cli) -> CheckResult {
    let root = cli
        .root
        .canonicalize()
        .map_err(|error| format!("root {} is not readable: {error}", cli.root.display()))?;
    let allowlist_path = root.join("schemas/dependency-allowlist.v1.json");
    let mut violations = Vec::new();
    let Ok(raw) = std::fs::read_to_string(&allowlist_path) else {
        violations.push(Violation::MissingAllowlistFile {
            path: "schemas/dependency-allowlist.v1.json".to_owned(),
        });
        return Ok((0, 0, violations));
    };
    let allowlist: Value = serde_json::from_str(&raw)
        .map_err(|error| format!("allowlist is not valid JSON: {error}"))?;
    let allow = allowlist
        .get("allow")
        .and_then(Value::as_object)
        .ok_or_else(|| "allowlist has no allow map".to_owned())?;
    let allowed: BTreeMap<String, BTreeSet<String>> = allow
        .iter()
        .map(|(name, targets)| {
            (
                name.clone(),
                targets
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
            )
        })
        .collect();

    let mut manifests = discover_manifests(&root)?;
    for extra in &cli.extra_edges {
        let (from, to) = extra
            .split_once("->")
            .ok_or_else(|| format!("--extra-edge must look like from->to, got {extra}"))?;
        manifests
            .entry(from.to_owned())
            .or_default()
            .insert(to.to_owned());
    }
    if manifests.is_empty() {
        return Err("no crate manifests were discovered".to_owned());
    }
    for crate_name in allowed.keys() {
        if !manifests.contains_key(crate_name) {
            violations.push(Violation::UnknownCrateInAllowlist {
                crate_name: crate_name.clone(),
            });
        }
    }

    let forbidden = allowlist
        .get("forbidden")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut edge_count = 0_usize;
    for (from, targets) in &manifests {
        let Some(declared) = allowed.get(from) else {
            violations.push(Violation::UnlistedCrate {
                crate_name: from.clone(),
            });
            continue;
        };
        for to in targets {
            edge_count += 1;
            if !declared.contains(to) {
                violations.push(Violation::UndeclaredEdge {
                    from: from.clone(),
                    to: to.clone(),
                });
            }
            for rule in &forbidden {
                if rule.get("from").and_then(Value::as_str) != Some(from.as_str()) {
                    continue;
                }
                let matches = match rule.get("to") {
                    Some(Value::String(pattern)) => pattern == "*" || pattern == to,
                    Some(Value::Array(patterns)) => patterns
                        .iter()
                        .filter_map(Value::as_str)
                        .any(|pattern| pattern == to),
                    _ => false,
                };
                if matches {
                    violations.push(Violation::ForbiddenEdge {
                        from: from.clone(),
                        to: to.clone(),
                        reason: rule
                            .get("reason")
                            .and_then(Value::as_str)
                            .unwrap_or("declared forbidden")
                            .to_owned(),
                    });
                }
            }
        }
    }
    Ok((manifests.len(), edge_count, violations))
}

/// Collect the internal (`harness-*`) keys of one dependency table.
fn collect_internal_edges(table: Option<&toml::Value>, edges: &mut BTreeSet<String>) {
    let Some(table) = table.and_then(toml::Value::as_table) else {
        return;
    };
    for dependency in table.keys() {
        if dependency.starts_with("harness-") {
            edges.insert(dependency.clone());
        }
    }
}

/// Every `crates/<name>/Cargo.toml` internal dependency edge.
fn discover_manifests(root: &Path) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let crates_dir = root.join("crates");
    let entries = std::fs::read_dir(&crates_dir)
        .map_err(|error| format!("{} is not readable: {error}", crates_dir.display()))?;
    let mut manifests = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let manifest_path = entry.path().join("Cargo.toml");
        if !manifest_path.is_file() {
            continue;
        }
        let raw = std::fs::read_to_string(&manifest_path)
            .map_err(|error| format!("{} is not readable: {error}", manifest_path.display()))?;
        let manifest: toml::Value = toml::from_str(&raw)
            .map_err(|error| format!("{} is not valid TOML: {error}", manifest_path.display()))?;
        let name = manifest
            .get("package")
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
            .ok_or_else(|| format!("{} has no package name", manifest_path.display()))?
            .to_owned();
        let mut edges = BTreeSet::new();
        let sections = ["dependencies", "dev-dependencies", "build-dependencies"];
        for section in sections {
            collect_internal_edges(manifest.get(section), &mut edges);
        }
        // Target-specific tables are real edges too: a dependency added under
        // `[target.'cfg(windows)'.dependencies]` must not escape the allowlist.
        if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
            for target in targets.values() {
                for section in sections {
                    collect_internal_edges(target.get(section), &mut edges);
                }
            }
        }
        manifests.insert(name, edges);
    }
    Ok(manifests)
}
