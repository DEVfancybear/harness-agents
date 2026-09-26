# Harness Agents (ha) Tool Mapping

Skills speak in actions ("dispatch a subagent", "create a todo", "read a file"). In Harness Agents (`ha`) these resolve to the tools below. Trust your actual tool list over this table when they disagree: a tool is only there when the host offers it (the `ipython` REPL needs an interpreter, delegation needs a delegate host, and so on).

| Action skills request | ha equivalent |
| --- | --- |
| Invoke a skill (`Skill` tool) | `activate_skill` with the skill's name; read its other files with `read_skill_file` |
| Read a file (`Read`) | `read_file` (`offset` is zero-based lines, `limit` bounds the range) |
| Find files (`Glob`, `LS`) | `glob`, `list_files` |
| Search file contents (`Grep`) | `search_text` |
| Edit a file (`Edit`, `MultiEdit`) | `edit_file` (exact match first, then whitespace/quote-tolerant; refuses a non-unique match) or `apply_patch` |
| Create or overwrite a file (`Write`) | `write_file` |
| Run a command (`Bash`) | `run_shell` for a shell command line, `run_process` for one executable with arguments; page long output with `read_process_output` |
| Git state (`git status`, `git diff`, `git log`) | `git_status`, `git_diff`, `git_log` (or `run_shell` for anything else) |
| Task tracking (`TodoWrite`, "create a todo", "mark complete") | `task_update` |
| Ask the user a question (`AskUserQuestion`) | `ask_user` with the question and up to 9 options |
| Dispatch a subagent (`Task`, `Subagent (general-purpose):` template) | `delegate` with `role` `explorer` (read-only) or `coder` (its own git worktree), or `rlm.spawn` from the `ipython` tool |
| Run several subagents in parallel | `rlm.spawn(...)` for each, then `rlm.collect(...)` in the `ipython` tool |
| Search past conversations | `history_search`, `history_read` |
| Run Python / notebooks | the `ipython` tool: a persistent kernel whose variables survive between calls |

## Subagents

`ha` runs subagents inside the same session, under the parent turn's permission mode and rules, and each child is bounded by the same step, tool-call and time limits as a normal turn. There are two ways to start one:

- **`delegate` tool** — one child per call, and the call returns its report. `explorer` children can only read; `coder` children work in an isolated git worktree and report what they changed there. Their changes are not merged into the user's checkout; review and apply them yourself.
- **`rlm` in the `ipython` tool** — for parallel work. `await rlm.spawn(prompt, name=...)` returns as soon as the child is admitted; `await rlm.collect([...], timeout_ms=...)` waits for their results, and `await rlm.list_subagents()` shows their state. Names must be unique among siblings. Children cannot delegate again.

Give each child a self-contained brief (it does not see your conversation) and ask for a concise report with file paths.

## Task lists

`task_update` records progress notes on the current task. For a longer plan, keep the Superpowers plan file (or a Markdown checklist) and update it as steps complete. Older Superpowers docs may refer to `TodoWrite`; treat that as `task_update`.

## Permissions

Tools that change files or run commands may open an approval panel, depending on the session's mode (`/permissions ask | auto-edit | full-auto`). A denied or expired approval returns an error to you: do not retry the same action in a loop — ask the user or choose another approach.
