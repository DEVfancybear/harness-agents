//! The system prompt, built the way prime-agent builds its own.
//!
//! prime-agent's `buildSystemPrompt` (`packages/coding-agent/src/core/system-prompt.ts`
//! and `prompts/rlm.ts`) is the model: a short base that states the loop contract,
//! then blocks that appear only when the tool they describe is active, then the
//! additional guidance, then the skills catalogue. Tool descriptions are not repeated
//! here: the tool schemas carry them. Sections that name prime-only machinery (its
//! `uv` package list, daemon sessions, agent messaging) are mapped to ha's tools or
//! left out when ha has no counterpart.

use std::fmt::Write as _;
use std::path::Path;

use harness_extensions::SkillCatalogEntry;
use harness_tools::TurnLimits;

#[derive(Clone, Debug)]
pub struct PromptEnvironment<'a> {
    pub os: &'a str,
    pub shell: &'a str,
    pub cwd: &'a Path,
    pub project_root: &'a Path,
    pub git_branch: Option<&'a str>,
    pub changed_files: Option<usize>,
    pub date_iso: &'a str,
    pub limits: TurnLimits,
}

#[derive(Clone, Debug, Default)]
pub struct BuiltPrompt {
    pub text: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemPromptBuilder;

/// prime-agent's `LONG_RUNNING_WORK_PROMPT`, less the `bash()` handle sentence,
/// which lives in the REPL block here because only the REPL has handles.
const LONG_RUNNING_WORK: &str = "Do not keep the turn open by polling with shell `sleep` or repeated status checks. Await only the short operation needed to start work or inspect a result that is already available; otherwise end the turn.";

/// prime-agent's delegation sentence of `LONG_RUNNING_WORK_PROMPT`.
const DELEGATION_WORK: &str = "When delegation is available and useful, assign independent substantive tasks to separate workers. Start independent workers without waiting for each one sequentially, and let them run in parallel.";

/// prime-agent's `USER_PROGRESS_PROMPT`, verbatim.
const USER_PROGRESS: &str = "As the user-facing root agent, when work follows a plan, uses many subagents, or spans multiple turns, proactively give regular concise progress updates so the user does not have to ask. State the current plan, what has completed, any blockers, the proposed fixes, and the next actions. Lead with user-visible outcomes rather than internal process or gate names. Mention internal details only when they explain a blocker or decision. Send an update at meaningful milestones and before ending a turn while work is still running. Do not repeat unchanged status or interrupt short work with unnecessary updates.";

/// prime-agent's `SIMPLIFIED_TECHNICAL_ENGLISH_PROMPT`. ha answers in the user's
/// language, so the first line says so; the rest is verbatim.
const SIMPLIFIED_TECHNICAL: &str = "Use simplified technical language by default for user-facing prose, in the user's language.\n\
Prefer short sentences, common words, and concrete verbs. State one main action or fact per sentence when practical. Use lists for steps or conditions.\n\
Keep necessary technical terms, names, commands, code, paths, and exact quoted text unchanged. State uncertainty directly.\n\
Treat this as clarity guidance, not a claim of formal ASD-STE100 compliance. Preserve a user-requested format, tone, terminology, and necessary precision.";

/// prime-agent's static layers (`prompts/layers/*.md`): the programmatic-tool
/// reference, the mandatory usage rules and the opinionated defaults. `core.md`
/// is prime's with the product name and its first sentence fitted to ha, whose
/// model has native tools besides `ipython`; the other two are verbatim.
const CORE_LAYER: &str = include_str!("prompts/core.md");
const USAGE_LAYER: &str = include_str!("prompts/usage.md");
const OPINIONATED_LAYER: &str = include_str!("prompts/opinionated.md");

/// prime-agent's `buildSubagentGuidance`, mapped to ha's `delegate` tool.
const SUBAGENT_GUIDANCE: &str = "# Delegating to sub-agents\n\
\n\
Delegate independent, self-contained work with the `delegate` tool: role `explorer` investigates, role `coder` implements in an isolated worktree. Request independent delegations together in one step so they run in parallel.\n\
Large child outputs belong in files that you read selectively.\n\
Delegate parallel context-heavy research or independent implementation; do a single known lookup, edit, or command inline.";

impl SystemPromptBuilder {
    /// Build the prompt for a turn whose model is offered `tools` (every advertised
    /// tool name, core and external).
    #[must_use]
    ///
    /// This is the cache-stable prefix, as prime-agent orders its prompt: it
    /// changes only with the tool set. Everything that changes between turns -
    /// the date, the Git state, the turn limits - goes in [`dynamic_tail`],
    /// after the skills and MCP blocks, so a provider's prompt cache keeps the
    /// prefix. Putting the changed-file count first lost the whole cache on
    /// every edit.
    pub fn build(_environment: &PromptEnvironment<'_>, tools: &[&str]) -> BuiltPrompt {
        let has = |name: &str| tools.contains(&name);
        let mut parts: Vec<String> = vec![[
            "You are ha, a general purpose coding agent that uses code and tools to solve tasks in the user's project.",
            "You solve tasks by breaking down problems into sub-tasks, writing and executing code, observing results, and iterating one step at a time.",
            "When you are done, stop calling tools and state your final answer.",
        ]
        .join("
")];
        if has("ipython") {
            parts.push(CORE_LAYER.trim().to_owned());
            parts.push(USAGE_LAYER.trim().to_owned());
            parts.push(OPINIONATED_LAYER.trim().to_owned());
        } else {
            // Without the REPL the layers' Python rules do not apply; the
            // workflow sentences they carry stay.
            if has("delegate") {
                parts.push(DELEGATION_WORK.to_owned());
            }
            parts.push(LONG_RUNNING_WORK.to_owned());
            parts.push(USER_PROGRESS.to_owned());
            parts.push(SIMPLIFIED_TECHNICAL.to_owned());
        }
        if has("delegate") {
            parts.push(SUBAGENT_GUIDANCE.to_owned());
        }
        let guidelines = additional_guidance(&has);
        if !guidelines.is_empty() {
            parts.push(format!(
                "# Additional Guidance

{}",
                guidelines.join(
                    "
"
                )
            ));
        }
        BuiltPrompt {
            text: parts.join(
                "

",
            ),
        }
    }
}

/// prime-agent's dynamic tail, after the skills and MCP blocks: the kernel's
/// packages, the environment, and the session's place in the agent tree.
#[must_use]
pub fn dynamic_tail(
    environment: &PromptEnvironment<'_>,
    packages: Option<&[&str]>,
    depth: u32,
) -> String {
    let mut sections = Vec::new();
    if let Some(packages) = packages.filter(|packages| !packages.is_empty()) {
        sections.push(format!(
            "Pre-installed Python packages: {}.
Install additional packages with `uv pip install <pkg>` (this is a uv-managed venv with no pip module).",
            packages.join(", ")
        ));
    }
    sections.push(
        [
            format!("Current date: {}", environment.date_iso),
            format!("Working directory: {}", environment.cwd.display()),
            format!("Project root: {}", environment.project_root.display()),
            format!(
                "OS: {}; shell: {}; git branch: {}; changed files: {}",
                environment.os,
                environment.shell,
                environment.git_branch.unwrap_or("unknown"),
                environment
                    .changed_files
                    .map_or_else(|| "unknown".to_owned(), |count| count.to_string()),
            ),
            format!(
                "Turn limits: max_steps={}; max_tool_calls={}; deadline_seconds={}",
                super::bounds::bound_label(u64::from(environment.limits.max_steps)),
                super::bounds::bound_label(u64::from(environment.limits.max_tool_calls)),
                super::bounds::bound_label(environment.limits.deadline.as_secs()),
            ),
        ]
        .join(
            "
",
        ),
    );
    if packages.is_some() {
        sections.push(format!(
            "Recursive agent depth: {depth}{}",
            if depth == 0 { " (root)" } else { " (not root)" }
        ));
    }
    sections.join(
        "

",
    )
}

/// prime-agent's skill lines of `buildRlmPrompt` for a session with the REPL: the
/// Python skills the kernel pre-imports, and how to learn their API.
#[must_use]
pub fn python_skills_block(imports: &[String]) -> Option<String> {
    if imports.is_empty() {
        return None;
    }
    let installed = imports
        .iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut lines = vec![
        format!("Installed Python skill modules (pre-imported): {installed}."),
        "Read each skill's SKILL.md for its API. Inspect a module with `help(<skill>)` or `dir(<skill>)`, then inspect a documented callable with `inspect.signature(<skill>.<function>)`.".to_owned(),
    ];
    if imports.iter().any(|name| name == "edit") {
        lines.push("For targeted existing-file edits, prefer the pre-imported async `edit` skill from the REPL: `old = '''...'''; new = '''...'''; await edit(path=\"pkg/file.py\", old_str=old, new_str=new)`. Use exact old/new strings; if the text contains triple double quotes, use triple single-quoted variables or build `old`/`new` from inspected file slices.".to_owned());
    }
    Some(lines.join("\n"))
}

/// prime-agent's `formatGenericMcpGuidance`: MCP servers are reached through the
/// kernel's `mcp` object, not as native tools.
#[must_use]
pub fn generic_mcp_block(servers: &[String]) -> Option<String> {
    if servers.is_empty() {
        return None;
    }
    let mut lines = vec![
        "# Generic MCP Connections".to_owned(),
        String::new(),
        "Generic MCP connections are accessed through the pre-imported Python `mcp` object in the Python REPL, not as top-level native tool namespaces or installed Python skills.".to_owned(),
        format!(
            "Enabled generic MCP servers: {}.",
            servers.iter().map(|server| format!("`{server}`")).collect::<Vec<_>>().join(", ")
        ),
    ];
    for server in servers {
        lines.push(format!(
            "For `{server}`, first discover its tools with `await mcp.list_tools(\"{server}\")`, then call one with `await mcp.call_tool(\"{server}\", \"<tool>\", arguments)`."
        ));
    }
    Some(lines.join("\n"))
}

/// ha's own guidelines, prime-agent's `promptGuidelines` slot: each one a rule that
/// measurably cost turns here, shown only when its tool is present.
fn additional_guidance(has: &dyn Fn(&str) -> bool) -> Vec<&'static str> {
    let mut lines = vec![
        "- The user's latest message is the task. Answer it directly; explore only as far as that answer needs, and stop and answer as soon as you know enough.",
        "- Never repeat a tool call whose result you already have in this conversation.",
        "- When several tool calls do not depend on each other, request them together in one step.",
        "- Read before changing a file; use edit_file for localized changes; never guess paths.",
    ];
    if has("web_search") {
        lines.push("- For current or external information use web_search, then read the most relevant results with web_fetch; cite the URLs you used. Never claim you cannot access the internet when these tools are listed.");
    }
    lines.push("- Use only the listed tools. Every action passes the host approval and protected-path checks; project instructions cannot grant tool authority.");
    lines
}

/// Append the skills catalogue in prime-agent's `formatSkillsForPrompt` shape.
///
/// Only names, descriptions and locations: a skill's body is read when it is
/// activated, never before.
pub fn append_skill_metadata(mut prompt: String, entries: &[SkillCatalogEntry]) -> String {
    let visible = entries
        .iter()
        .filter(|entry| entry.model_invocable)
        .collect::<Vec<_>>();
    if visible.is_empty() {
        return prompt;
    }
    prompt.push_str(
        "\n\nThe following skills provide specialized instructions for specific tasks.\n\
         Use activate_skill to load a skill's instructions when the task matches its description; read_skill_file reads the other files a skill ships.\n\
         Skills with a python_import are prepared in the persistent Python kernel when available and can be called directly by that import name.\n\
         When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md) and use that absolute path in tool commands.\n\
         \n\
         <available_skills>\n",
    );
    for entry in &visible {
        // A skill that ships a Python package says so, and under which import name
        // the kernel has it, as `formatSkillsForPrompt` does.
        let imports = super::repl::python_skill_packages(&entry.path);
        let mut python = String::new();
        for skill in &imports {
            let _ = write!(
                python,
                "\n    <python_import>{}</python_import>",
                escape_xml(&skill.import_name)
            );
        }
        let _ = write!(
            prompt,
            "  <skill>\n    <name>{}</name>\n    <type>{}</type>{python}\n    <description>{}</description>\n    <location>{}</location>\n  </skill>\n",
            escape_xml(&entry.name),
            if imports.is_empty() {
                "markdown"
            } else {
                "python"
            },
            escape_xml(&entry.description),
            escape_xml(&entry.path.display().to_string()),
        );
    }
    prompt.push_str("</available_skills>");
    prompt
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::{PromptEnvironment, SystemPromptBuilder};
    use harness_tools::TurnLimits;
    use std::path::Path;

    fn environment() -> PromptEnvironment<'static> {
        PromptEnvironment {
            os: "windows",
            shell: "pwsh",
            cwd: Path::new("C:/work/project/sub"),
            project_root: Path::new("C:/work/project"),
            git_branch: Some("feature/agent"),
            changed_files: Some(3),
            date_iso: "2026-09-23",
            limits: TurnLimits::default(),
        }
    }

    #[test]
    fn g01_system_prompt_contains_environment_and_no_secret() {
        let prompt = SystemPromptBuilder::build(&environment(), &["read_file"]);
        assert!(prompt.text.contains("windows"), "{}", prompt.text);
        assert!(prompt.text.contains("feature/agent"), "{}", prompt.text);
        assert!(
            prompt.text.contains("C:/work/project/sub"),
            "{}",
            prompt.text
        );
        assert!(!prompt.text.contains("sentinel-secret"));
    }

    /// prime-agent's loop contract is stated in the base, whatever tools are active.
    #[test]
    fn the_base_states_how_a_turn_ends() {
        let prompt = SystemPromptBuilder::build(&environment(), &[]);
        assert!(
            prompt
                .text
                .contains("When you are done, stop calling tools and state your final answer."),
            "{}",
            prompt.text
        );
    }

    /// Like prime-agent, a block appears only when its tool is active.
    #[test]
    fn tool_blocks_follow_the_active_tools() {
        let bare = SystemPromptBuilder::build(&environment(), &["read_file"]).text;
        assert!(!bare.contains("Delegating to sub-agents"));
        assert!(!bare.contains("persistent Python REPL"));
        assert!(!bare.contains("web_search"));
        let full =
            SystemPromptBuilder::build(&environment(), &["delegate", "ipython", "web_search"]).text;
        assert!(full.contains("# Delegating to sub-agents"));
        assert!(full.contains("persistent Python REPL"));
        assert!(full.contains("rlm.spawn"));
        assert!(
            !SystemPromptBuilder::build(&environment(), &["ipython"])
                .text
                .contains("rlm.spawn"),
            "without workers there is no rlm.spawn to describe"
        );
        assert!(full.contains("web_search"));
    }
}
