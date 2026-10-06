//! prime-agent's informational commands: `model list` and `prompt`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use harness_types::{ErrorCode, HarnessError};

use super::paths::{HostPlatform, LaunchEnvironment, PathRequest};

fn data_dir(environment: &LaunchEnvironment) -> Result<PathBuf, HarnessError> {
    super::paths::resolve(&PathRequest {
        platform: HostPlatform::current(),
        environment,
        explicit_data_dir: None,
    })
    .map(|paths| paths.data_dir)
    .map_err(|error| HarnessError::new(ErrorCode::StorageOpenFailed, error.to_string()))
}

/// prime-agent's `formatTokenCount`: `1M`/`1.5M`, `128K`/`163.8K`, plain
/// below a thousand.
fn token_count(count: Option<u64>) -> String {
    let Some(count) = count else {
        return "-".to_owned();
    };
    #[allow(clippy::cast_precision_loss, reason = "a display figure")]
    let value = count as f64;
    let (scaled, unit) = if count >= 1_000_000 {
        (value / 1_000_000.0, "M")
    } else if count >= 1_000 {
        (value / 1_000.0, "K")
    } else {
        return count.to_string();
    };
    if scaled.fract() == 0.0 {
        format!("{scaled}{unit}")
    } else {
        format!("{scaled:.1}{unit}")
    }
}

/// `ha model list [search]`: prime-agent's catalog table of the models this
/// user can call (a credential for their provider), filtered by a search.
///
/// # Errors
/// The data directory cannot be resolved.
pub fn model_list(search: Option<&str>, json: bool) -> Result<ExitCode, HarnessError> {
    let environment = LaunchEnvironment::capture();
    let data_dir = data_dir(&environment)?;
    let catalog = super::providers::Catalog::load(&data_dir);
    let terms = search
        .unwrap_or_default()
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut models = catalog
        .models()
        .iter()
        .filter(|model| {
            super::credentials::source_for(
                &environment,
                &data_dir,
                &model.provider,
                &model.key_env(),
            )
            .is_some()
        })
        .filter(|model| {
            let haystack = format!("{} {} {}", model.provider, model.id, model.name).to_lowercase();
            terms.iter().all(|term| haystack.contains(term))
        })
        .collect::<Vec<_>>();
    models.sort_by(|a, b| a.provider.cmp(&b.provider).then_with(|| a.id.cmp(&b.id)));
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "models": models,
            }))
            .unwrap_or_default()
        );
        return Ok(ExitCode::SUCCESS);
    }
    if models.is_empty() {
        match search {
            Some(search) if !catalog.models().is_empty() => {
                println!("No models matching \"{search}\"");
            }
            _ => println!(
                "No models available. Use /login to log into a provider via OAuth or API key."
            ),
        }
        return Ok(ExitCode::SUCCESS);
    }
    let rows = models
        .iter()
        .map(|model| {
            [
                model.provider.clone(),
                model.id.clone(),
                token_count(model.context_window),
                token_count(model.max_tokens),
                if model.reasoning { "yes" } else { "no" }.to_owned(),
                if model.input.iter().any(|input| input == "image") {
                    "yes"
                } else {
                    "no"
                }
                .to_owned(),
            ]
        })
        .collect::<Vec<_>>();
    let header = [
        "provider", "model", "context", "max-out", "thinking", "images",
    ];
    let widths = (0..header.len())
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .chain([header[column].len()])
                .max()
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    let line = |cells: &[&str]| {
        cells
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    println!("{}", line(&header));
    for row in &rows {
        println!(
            "{}",
            line(&row.iter().map(String::as_str).collect::<Vec<_>>())
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// `ha prompt [--cwd DIR] [--json]`: prime-agent's prompt dump - the system
/// prompt a headless run in this project would be sent, with its static
/// prefix and dynamic tail told apart.
///
/// # Errors
/// The project directory cannot be opened.
pub fn prompt(cwd: Option<&Path>, json: bool) -> Result<ExitCode, HarnessError> {
    let root = match cwd {
        Some(cwd) => cwd.to_path_buf(),
        None => std::env::current_dir()
            .map_err(|error| HarnessError::new(ErrorCode::StorageOpenFailed, error.to_string()))?,
    };
    let environment = LaunchEnvironment::capture();
    let tools = harness_tools::coding_tool_schemas();
    let mut names = tools
        .iter()
        .filter_map(|schema| {
            schema
                .pointer("/function/name")
                .and_then(serde_json::Value::as_str)
        })
        .collect::<Vec<_>>();
    if super::web::WebHost::from_environment(&environment).is_some() {
        names.extend(["web_search", "web_fetch"]);
    }
    let (git_branch, changed_files) = super::service::prompt_git_facts(&root);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let prompt_environment = super::prompt::PromptEnvironment {
        os: std::env::consts::OS,
        shell: if cfg!(windows) { "PowerShell" } else { "sh" },
        cwd: &root,
        project_root: &root,
        git_branch: git_branch.as_deref(),
        changed_files,
        date_iso: &today,
        limits: super::bounds::limits_from_environment(&environment),
    };
    let static_prefix = super::prompt::SystemPromptBuilder::build(&prompt_environment, &names).text;
    let tail = super::prompt::dynamic_tail(&prompt_environment, None, 0);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "segments": [
                    {"name": "static", "cached": true, "text": static_prefix},
                    {"name": "dynamic-tail", "cached": false, "text": tail},
                ],
                "cached_prefix_len": static_prefix.len(),
            }))
            .unwrap_or_default()
        );
    } else {
        println!("{static_prefix}\n\n{tail}");
    }
    Ok(ExitCode::SUCCESS)
}
