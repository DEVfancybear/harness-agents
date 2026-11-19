//! What `ha package` and the TUI's `/package` share: the words of a package
//! command, the manager they run it with, and the lines they answer with.

use std::path::Path;

use super::{
    ConfiguredPackage, PackageManager, ProgressEvent, ProgressEventKind, SettingsManager,
    UserOrProject,
};

/// One package command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Install { source: String, local: bool },
    Remove { source: String, local: bool },
    Update { source: Option<String> },
    List,
}

/// What `/package` takes, one line per action.
pub const USAGE: &[&str] = &[
    "/package install <source> [--local]  npm:@scope/name, git:github.com/user/repo, https://..., ./path",
    "/package remove <source> [--local]   remove an installed package (--local: the project's)",
    "/package update [source]             update every installed package, or one",
    "/package list                        the installed packages",
];

/// Read `/package`'s words: none or `list` lists.
///
/// # Errors
///
/// The usage of the action whose words do not fit it.
pub fn parse(words: &[String]) -> Result<Action, String> {
    let local = words.iter().any(|word| word == "--local");
    let rest = words
        .iter()
        .skip(1)
        .filter(|word| *word != "--local")
        .map(String::as_str)
        .collect::<Vec<_>>();
    match (words.first().map(String::as_str), rest.as_slice()) {
        (None, _) | (Some("list" | "ls"), []) => Ok(Action::List),
        (Some("install" | "add"), [source]) => Ok(Action::Install {
            source: (*source).to_owned(),
            local,
        }),
        (Some("install" | "add"), _) => {
            Err("usage: /package install <source> [--local]".to_owned())
        }
        (Some("remove" | "uninstall" | "rm"), [source]) => Ok(Action::Remove {
            source: (*source).to_owned(),
            local,
        }),
        (Some("remove" | "uninstall" | "rm"), _) => {
            Err("usage: /package remove <source> [--local]".to_owned())
        }
        (Some("update"), []) if !local => Ok(Action::Update { source: None }),
        (Some("update"), [source]) if !local => Ok(Action::Update {
            source: Some((*source).to_owned()),
        }),
        (Some("update"), _) => Err("usage: /package update [source]".to_owned()),
        (Some("list" | "ls"), _) => Err("usage: /package list".to_owned()),
        (Some(other), _) => Err(format!(
            "unknown /package action {other}: install, remove, update or list"
        )),
    }
}

/// The manager for `workspace` and the user settings in `agent_dir`, telling
/// `progress` each step it starts (`Installing npm:x...`).
pub fn manager(
    workspace: &Path,
    agent_dir: &Path,
    progress: impl Fn(&str) + Send + 'static,
) -> PackageManager {
    let settings = SettingsManager::create(workspace, agent_dir);
    let mut manager = PackageManager::new(workspace, agent_dir, settings);
    manager.set_progress_callback(Box::new(move |event: &ProgressEvent| {
        if event.kind == ProgressEventKind::Start
            && let Some(message) = &event.message
        {
            progress(message);
        }
    }));
    manager
}

/// Run `action`; what it did, in one line, or the list.
///
/// # Errors
///
/// Why the install, removal or update failed, or that no installed package
/// matches the source to remove.
pub fn run(manager: &mut PackageManager, action: &Action) -> Result<Vec<String>, String> {
    let scope = |local: bool| {
        if local {
            UserOrProject::Project
        } else {
            UserOrProject::User
        }
    };
    let failed = |error: anyhow::Error| format!("Error: {error}");
    match action {
        Action::Install { source, local } => {
            manager
                .install_and_persist(source, scope(*local))
                .map_err(failed)?;
            Ok(vec![format!("Installed {source}")])
        }
        Action::Remove { source, local } => {
            if manager
                .remove_and_persist(source, scope(*local))
                .map_err(failed)?
            {
                Ok(vec![format!("Removed {source}")])
            } else {
                Err(format!("No matching package found for {source}"))
            }
        }
        Action::Update { source } => {
            manager.update(source.as_deref()).map_err(failed)?;
            Ok(vec![source.as_ref().map_or_else(
                || "Updated packages".to_owned(),
                |source| format!("Updated {source}"),
            )])
        }
        Action::List => Ok(list_lines(&manager.list_configured_packages())),
    }
}

/// The installed packages, the user's then the project's, each with where it
/// is installed.
#[must_use]
pub fn list_lines(packages: &[ConfiguredPackage]) -> Vec<String> {
    if packages.is_empty() {
        return vec!["No packages installed.".to_owned()];
    }
    let mut lines = Vec::new();
    for (scope, heading) in [
        (UserOrProject::User, "User packages:"),
        (UserOrProject::Project, "Project packages:"),
    ] {
        let section = packages
            .iter()
            .filter(|package| package.scope == scope)
            .collect::<Vec<_>>();
        if section.is_empty() {
            continue;
        }
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(heading.to_owned());
        for package in section {
            lines.push(if package.filtered {
                format!("  {} (filtered)", package.source)
            } else {
                format!("  {}", package.source)
            });
            if let Some(path) = &package.installed_path {
                lines.push(format!("    {}", path.display()));
            }
        }
    }
    lines
}

/// The installed sources for the slash menu, each with its scope.
#[must_use]
pub fn choices(workspace: &Path, agent_dir: &Path) -> Vec<(String, String)> {
    let settings = SettingsManager::create(workspace, agent_dir);
    PackageManager::new(workspace, agent_dir, settings)
        .list_configured_packages()
        .into_iter()
        .map(|package| {
            let scope = match package.scope {
                UserOrProject::User => "user package",
                UserOrProject::Project => "project package (--local)",
            };
            (package.source, scope.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Action, choices, list_lines, manager, parse, run};

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn the_words_name_one_action_or_say_how_it_is_used() {
        assert_eq!(parse(&[]), Ok(Action::List));
        assert_eq!(parse(&words("list")), Ok(Action::List));
        assert_eq!(
            parse(&words("install npm:@foo/bar --local")),
            Ok(Action::Install {
                source: "npm:@foo/bar".to_owned(),
                local: true
            })
        );
        assert_eq!(
            parse(&words("uninstall ./pkg")),
            Ok(Action::Remove {
                source: "./pkg".to_owned(),
                local: false
            })
        );
        assert_eq!(parse(&words("update")), Ok(Action::Update { source: None }));
        assert_eq!(
            parse(&words("update git:github.com/u/r")),
            Ok(Action::Update {
                source: Some("git:github.com/u/r".to_owned())
            })
        );
        assert!(
            parse(&words("install"))
                .unwrap_err()
                .contains("install <source>")
        );
        assert!(
            parse(&words("remove a b"))
                .unwrap_err()
                .contains("remove <source>")
        );
        assert!(parse(&words("update --local")).is_err());
        assert!(parse(&words("frobnicate")).unwrap_err().contains("unknown"));
    }

    #[test]
    fn a_local_package_installs_lists_offers_and_removes() {
        let root = tempfile::tempdir().expect("temp");
        let workspace = root.path().join("work");
        let agent_dir = root.path().join("config");
        let package = root.path().join("pkg");
        std::fs::create_dir_all(package.join("skills/hello")).expect("package");
        std::fs::write(
            package.join("skills/hello/SKILL.md"),
            "---\nname: hello\ndescription: say hello\n---\nSay hello.\n",
        )
        .expect("skill");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&agent_dir).expect("config");
        let source = package.display().to_string();
        let steps = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&steps);
        let mut packages = manager(&workspace, &agent_dir, move |step| {
            seen.lock().expect("steps").push(step.to_owned());
        });

        assert_eq!(
            run(&mut packages, &Action::List),
            Ok(vec!["No packages installed.".to_owned()])
        );
        assert_eq!(
            run(
                &mut packages,
                &Action::Install {
                    source: source.clone(),
                    local: false
                }
            ),
            Ok(vec![format!("Installed {source}")])
        );
        let listed = run(&mut packages, &Action::List).expect("list");
        assert_eq!(listed[0], "User packages:");
        // Settings keep a local path relative to themselves; the list shows
        // where it is too.
        assert_eq!(listed[1], "  ../pkg");
        assert!(listed[2].ends_with("pkg"), "{listed:?}");
        let offered = choices(&workspace, &agent_dir);
        assert_eq!(
            offered,
            vec![("../pkg".to_owned(), "user package".to_owned())]
        );
        // What the menu offers removes it.
        let named = offered[0].0.clone();
        assert_eq!(
            run(
                &mut packages,
                &Action::Remove {
                    source: named.clone(),
                    local: false
                }
            ),
            Ok(vec![format!("Removed {named}")])
        );
        assert!(choices(&workspace, &agent_dir).is_empty());
        assert_eq!(
            run(
                &mut packages,
                &Action::Remove {
                    source: source.clone(),
                    local: false
                }
            ),
            Err(format!("No matching package found for {source}"))
        );
        // The package directory is the user's: removing its source leaves it.
        assert!(package.join("skills/hello/SKILL.md").is_file());
        assert!(
            list_lines(&[])
                .first()
                .is_some_and(|line| line == "No packages installed.")
        );
    }
}
