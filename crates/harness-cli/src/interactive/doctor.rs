//! `/doctor` and `ha doctor`: how ready a repository is for an agent, scored on
//! learn-harness-engineering's five subsystems - instructions, tools,
//! environment, state and feedback.
//!
//! Every check is structural - a file exists, a lock file sits beside its
//! manifest, the entry file stays a short map, its links resolve, the project
//! names its checks, the handoff is not older than the last commit - and each
//! one that falls short says how to fix it. The weakest subsystem is named, as
//! the place to start; the audit does not claim it is the bottleneck, which
//! only failed runs can show.

use std::fmt::Write as _;
use std::path::Path;

/// What the configuration says, for the checks that read it.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub trusted: bool,
    pub checks: usize,
    pub judge: bool,
    pub hooks: usize,
    pub mcp_servers: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Level {
    Pass,
    /// Worth fixing; the harness works without it.
    Warn,
    /// A missing piece the course treats as essential.
    Fail,
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub level: Level,
    pub text: String,
    /// How to fix it, when it is not a pass.
    pub fix: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Subsystem {
    pub name: &'static str,
    pub findings: Vec<Finding>,
}

impl Subsystem {
    fn score(&self) -> (usize, usize) {
        let passed = self
            .findings
            .iter()
            .filter(|finding| finding.level == Level::Pass)
            .count();
        (passed, self.findings.len())
    }

    fn pass(&mut self, text: impl Into<String>) {
        self.findings.push(Finding {
            level: Level::Pass,
            text: text.into(),
            fix: None,
        });
    }

    fn short(&mut self, level: Level, text: impl Into<String>, fix: impl Into<String>) {
        self.findings.push(Finding {
            level,
            text: text.into(),
            fix: Some(fix.into()),
        });
    }
}

/// The relative markdown link targets of `text` that do not exist under `base`.
fn broken_links(text: &str, base: &Path) -> Vec<String> {
    let mut broken = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("](") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find(')') else {
            break;
        };
        let target = rest[..end].trim();
        rest = &rest[end..];
        let target = target.split('#').next().unwrap_or_default().trim();
        let target = target.split_whitespace().next().unwrap_or_default();
        if target.is_empty() || target.contains("://") || target.starts_with("mailto:") {
            continue;
        }
        if !base.join(target).exists() {
            broken.push(target.to_owned());
        }
    }
    broken
}

/// The manifests a project may have and the lock files that pin them.
const LOCKS: [(&str, &[&str]); 7] = [
    ("Cargo.toml", &["Cargo.lock"]),
    (
        "package.json",
        &[
            "package-lock.json",
            "pnpm-lock.yaml",
            "yarn.lock",
            "bun.lockb",
            "bun.lock",
        ],
    ),
    (
        "pyproject.toml",
        &["uv.lock", "poetry.lock", "pdm.lock", "requirements.txt"],
    ),
    ("Pipfile", &["Pipfile.lock"]),
    ("go.mod", &["go.sum"]),
    ("Gemfile", &["Gemfile.lock"]),
    ("composer.json", &["composer.lock"]),
];

/// Files that pin a runtime or toolchain version.
const PINS: [&str; 9] = [
    "rust-toolchain.toml",
    "rust-toolchain",
    ".nvmrc",
    ".node-version",
    ".python-version",
    ".tool-versions",
    "mise.toml",
    ".mise.toml",
    "go.mod",
];

/// CI configurations that verify each change outside the agent.
const CI: [&str; 5] = [
    ".github/workflows",
    ".gitlab-ci.yml",
    "azure-pipelines.yml",
    ".circleci",
    "Jenkinsfile",
];

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn instructions(global_config_dir: &Path, root: &Path, cwd: &Path) -> Subsystem {
    let mut subsystem = Subsystem {
        name: "Instructions",
        findings: Vec::new(),
    };
    let entry = ["AGENTS.md", "CLAUDE.md"]
        .iter()
        .map(|name| root.join(name))
        .find(|path| path.is_file());
    let Some(entry) = entry else {
        subsystem.short(
            Level::Fail,
            "no AGENTS.md (or CLAUDE.md) at the project root",
            "write a short map: what the project is, how to run and verify it, the hard rules, links to deeper docs (/init drafts one)",
        );
        return subsystem;
    };
    let name = entry
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    subsystem.pass(format!("{name} at the project root"));
    let oversized = super::instructions::oversized(global_config_dir, root, cwd);
    if oversized.is_empty() {
        subsystem.pass(format!(
            "instruction files stay within {} lines",
            super::instructions::ENTRY_FILE_MAX_LINES
        ));
    } else {
        for (source, lines) in oversized {
            subsystem.short(
                Level::Warn,
                format!("{source} has {lines} lines"),
                format!(
                    "keep it a map of at most {} lines and move topic detail into linked docs read on demand; rules in the middle of a long file get missed",
                    super::instructions::ENTRY_FILE_MAX_LINES
                ),
            );
        }
    }
    let text = std::fs::read_to_string(&entry).unwrap_or_default();
    let broken = broken_links(&text, root);
    if broken.is_empty() {
        subsystem.pass(format!("the links in {name} resolve"));
    } else {
        subsystem.short(
            Level::Warn,
            format!("{name} links to missing files: {}", broken.join(", ")),
            "fix or remove the links; a stale map sends the agent the wrong way",
        );
    }
    subsystem
}

fn tools(root: &Path, facts: &Facts) -> Subsystem {
    let mut subsystem = Subsystem {
        name: "Tools",
        findings: Vec::new(),
    };
    if facts.trusted {
        subsystem.pass("the project is trusted: its config, hooks and skills load");
    } else if root.join(".harness").is_dir() {
        subsystem.short(
            Level::Warn,
            "the project is not trusted: its .harness config, hooks and skills are ignored",
            "/trust yes, if you trust this checkout",
        );
    }
    let skills = root.join(".harness/skills");
    let count = std::fs::read_dir(&skills).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().join("SKILL.md").is_file())
            .count()
    });
    if count > 0 {
        subsystem.pass(format!("{count} project skill(s) in .harness/skills"));
    }
    if facts.hooks > 0 {
        subsystem.pass(format!("{} hook(s) configured", facts.hooks));
    }
    if facts.mcp_servers > 0 {
        subsystem.pass(format!("{} MCP server(s) configured", facts.mcp_servers));
    }
    if subsystem.findings.is_empty() {
        subsystem.pass("the built-in tools only");
    }
    subsystem
}

fn environment(root: &Path) -> Subsystem {
    let mut subsystem = Subsystem {
        name: "Environment",
        findings: Vec::new(),
    };
    if git(root, &["rev-parse", "--is-inside-work-tree"]).as_deref() == Some("true") {
        subsystem.pass("a Git repository: changes are versioned and reversible");
    } else {
        subsystem.short(
            Level::Fail,
            "not a Git repository",
            "git init and commit a baseline; checkpoints, worktrees and handoffs rely on it",
        );
    }
    for (manifest, locks) in LOCKS {
        if !root.join(manifest).is_file() {
            continue;
        }
        if locks.iter().any(|lock| root.join(lock).is_file()) {
            subsystem.pass(format!("{manifest} has its lock file"));
        } else {
            subsystem.short(
                Level::Warn,
                format!("{manifest} has no lock file ({})", locks.join(" / ")),
                "commit the lock file so every session installs the same versions",
            );
        }
    }
    if let Some(pin) = PINS.iter().find(|pin| root.join(pin).is_file()) {
        subsystem.pass(format!("the toolchain is pinned ({pin})"));
    } else {
        subsystem.short(
            Level::Warn,
            "no toolchain version pin",
            "pin the runtime (rust-toolchain.toml, .nvmrc, .python-version, .tool-versions)",
        );
    }
    subsystem
}

fn state(root: &Path) -> Subsystem {
    let mut subsystem = Subsystem {
        name: "State",
        findings: Vec::new(),
    };
    let progress = root.join(super::lifecycle::PROGRESS_FILE);
    match std::fs::metadata(&progress).and_then(|meta| meta.modified()) {
        Err(_) => subsystem.short(
            Level::Warn,
            format!("no handoff in {}", super::lifecycle::PROGRESS_FILE),
            "/handoff writes one; the next session starts from it instead of rediscovering the project",
        ),
        Ok(written) => {
            let last_commit = git(root, &["log", "-1", "--format=%ct"])
                .and_then(|seconds| seconds.parse::<u64>().ok())
                .map(|seconds| std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds));
            if last_commit.is_some_and(|commit| commit > written) {
                subsystem.short(
                    Level::Warn,
                    format!(
                        "{} is older than the last commit",
                        super::lifecycle::PROGRESS_FILE
                    ),
                    "/handoff brings it up to date",
                );
            } else {
                subsystem.pass(format!(
                    "{} is up to date",
                    super::lifecycle::PROGRESS_FILE
                ));
            }
        }
    }
    match super::features::load(root) {
        Err(error) => subsystem.short(Level::Fail, error, "fix the JSON, or move it aside"),
        Ok(None) => subsystem.short(
            Level::Warn,
            format!("no feature list ({})", super::features::FEATURES_FILE),
            "for work that spans sessions, have the agent split it into features with the feature tool, each with the command that verifies it",
        ),
        Ok(Some(list)) => {
            let counts = list.counts();
            subsystem.pass(format!(
                "feature list: {} of {} verified passing",
                counts.verified, counts.total
            ));
            if counts.active > 1 {
                subsystem.short(
                    Level::Fail,
                    format!("{} features are active at once", counts.active),
                    "finish or block all but one; work on one feature at a time",
                );
            }
            if counts.unverified > 0 {
                subsystem.short(
                    Level::Warn,
                    format!(
                        "{} feature(s) marked passing without harness evidence",
                        counts.unverified
                    ),
                    "verify them with the feature tool; an edited state is not evidence",
                );
            }
        }
    }
    subsystem
}

fn feedback(root: &Path, facts: &Facts) -> Subsystem {
    let mut subsystem = Subsystem {
        name: "Feedback",
        findings: Vec::new(),
    };
    if facts.checks > 0 {
        subsystem.pass(format!(
            "{} [verify] check(s): goals, features and /autonomous are verified by the harness",
            facts.checks
        ));
    } else {
        subsystem.short(
            Level::Fail,
            "no [verify] checks",
            "add [[verify.checks]] (name, command, hint) to .harness/config.toml - cheap ones first, then tests - so \"done\" is decided by commands, not by the agent",
        );
    }
    if facts.judge {
        subsystem.pass("an independent verifier judges goals before they complete");
    } else {
        subsystem.short(
            Level::Warn,
            "goals complete without an independent verifier ([verify] judge = false)",
            "turn the judge back on unless the checks cover everything a goal asks",
        );
    }
    if let Some(ci) = CI.iter().find(|ci| root.join(ci).exists()) {
        subsystem.pass(format!("CI runs outside the agent ({ci})"));
    } else {
        subsystem.short(
            Level::Warn,
            "no CI configuration",
            "run the same checks in CI so every change is verified outside the agent too",
        );
    }
    subsystem
}

/// The audit, subsystem by subsystem.
#[must_use]
pub fn audit(global_config_dir: &Path, root: &Path, cwd: &Path, facts: &Facts) -> Vec<Subsystem> {
    vec![
        instructions(global_config_dir, root, cwd),
        tools(root, facts),
        environment(root),
        state(root),
        feedback(root, facts),
    ]
}

/// The audit as lines for the screen.
#[must_use]
pub fn lines(root: &Path, subsystems: &[Subsystem]) -> Vec<String> {
    let mut lines = vec![
        format!("harness audit of {}", root.display()),
        String::new(),
    ];
    for subsystem in subsystems {
        let (passed, total) = subsystem.score();
        lines.push(format!("{}  {passed}/{total}", subsystem.name));
        for finding in &subsystem.findings {
            let mark = match finding.level {
                Level::Pass => "✓",
                Level::Warn => "!",
                Level::Fail => "✗",
            };
            let mut line = format!("  {mark} {}", finding.text);
            if let Some(fix) = &finding.fix {
                let _ = write!(line, "\n      fix: {fix}");
            }
            lines.push(line);
        }
    }
    let weakest = subsystems
        .iter()
        .filter(|subsystem| {
            subsystem
                .findings
                .iter()
                .any(|finding| finding.level != Level::Pass)
        })
        .min_by(|left, right| {
            let ratio = |subsystem: &Subsystem| {
                let (passed, total) = subsystem.score();
                (passed * 1000) / total.max(1)
            };
            ratio(left).cmp(&ratio(right))
        });
    lines.push(String::new());
    lines.push(weakest.map_or_else(
        || "every check passes".to_owned(),
        |subsystem| {
            format!(
                "start with {}: it scores lowest here (failed runs, not this audit, show the real bottleneck)",
                subsystem.name
            )
        },
    ));
    lines
}

/// The audit as JSON, for `ha doctor --json`.
#[must_use]
pub fn json(subsystems: &[Subsystem]) -> serde_json::Value {
    serde_json::json!({
        "subsystems": subsystems.iter().map(|subsystem| {
            let (passed, total) = subsystem.score();
            serde_json::json!({
                "name": subsystem.name,
                "passed": passed,
                "total": total,
                "findings": subsystem.findings.iter().map(|finding| serde_json::json!({
                    "level": match finding.level {
                        Level::Pass => "pass",
                        Level::Warn => "warn",
                        Level::Fail => "fail",
                    },
                    "text": finding.text,
                    "fix": finding.fix,
                })).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::{Facts, Level, audit, broken_links, lines};

    #[test]
    fn a_bare_directory_fails_where_the_course_says_it_must_not() {
        let root = tempfile::tempdir().expect("root");
        let subsystems = audit(
            &std::path::PathBuf::new(),
            root.path(),
            root.path(),
            &Facts::default(),
        );
        let failing = subsystems
            .iter()
            .flat_map(|subsystem| subsystem.findings.iter())
            .filter(|finding| finding.level == Level::Fail)
            .map(|finding| finding.text.clone())
            .collect::<Vec<_>>();
        assert!(
            failing.iter().any(|text| text.contains("no AGENTS.md")),
            "{failing:?}"
        );
        assert!(
            failing
                .iter()
                .any(|text| text.contains("no [verify] checks")),
            "{failing:?}"
        );
        let text = lines(root.path(), &subsystems).join("\n");
        assert!(text.contains("fix: add [[verify.checks]]"), "{text}");
        assert!(text.contains("start with"), "{text}");
    }

    #[test]
    fn a_prepared_project_passes_its_checks() {
        let root = tempfile::tempdir().expect("root");
        let path = root.path();
        std::fs::create_dir_all(path.join("docs")).expect("docs");
        std::fs::write(path.join("docs/ARCHITECTURE.md"), "layers").expect("doc");
        std::fs::write(
            path.join("AGENTS.md"),
            "# Map\nSee [architecture](docs/ARCHITECTURE.md) and [gone](docs/GONE.md).\n",
        )
        .expect("agents");
        std::fs::write(path.join("Cargo.toml"), "[package]").expect("manifest");
        std::fs::write(path.join("rust-toolchain.toml"), "[toolchain]").expect("pin");
        let facts = Facts {
            trusted: true,
            checks: 2,
            judge: true,
            ..Facts::default()
        };
        let subsystems = audit(&std::path::PathBuf::new(), path, path, &facts);
        let text = lines(path, &subsystems).join("\n");
        assert!(text.contains("✓ AGENTS.md at the project root"), "{text}");
        assert!(
            text.contains("links to missing files: docs/GONE.md"),
            "{text}"
        );
        assert!(text.contains("Cargo.toml has no lock file"), "{text}");
        assert!(
            text.contains("the toolchain is pinned (rust-toolchain.toml)"),
            "{text}"
        );
        assert!(text.contains("2 [verify] check(s)"), "{text}");
        assert_eq!(
            super::json(&subsystems)["subsystems"][0]["name"],
            "Instructions"
        );
    }

    #[test]
    fn only_relative_links_that_do_not_resolve_are_broken() {
        let root = tempfile::tempdir().expect("root");
        std::fs::write(root.path().join("here.md"), "").expect("file");
        let broken = broken_links(
            "[a](here.md) [b](https://x.y) [c](missing.md#part) [d](here.md \"title\")",
            root.path(),
        );
        assert_eq!(broken, ["missing.md"]);
    }
}
