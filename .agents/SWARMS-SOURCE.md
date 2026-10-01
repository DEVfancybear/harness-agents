# Swarms skills source

- Upstream: https://github.com/am-will/swarms
- Source commit: `110268148a1fdf149a19e4dab848a1c0fca9835d` (2026-04-18)
- Imported: `swarm-planner`, `parallel-task`, `parallel-task-spark`, `super-swarm`, `super-swarm-spark`. Not imported: `co-design` and `parallel-task-tmux`.
- Role definitions: the `sparky` and `tdd_test_writer` agent roles the skills name come from https://github.com/am-will/codex-skills at commit `9f954c3` (`agents/sparky.toml`, `agents/testing-quality/tdd_test_writer.toml`).
- License: the upstream README states MIT; the repository ships no license file.

Harness Agents adaptations, written to the layout of the bundled `skill-creator` skill:

- Front matter: `version: 1102681` (the upstream commit), a description that says what the skill does and when to use it, and `disable-model-invocation: true` in place of upstream's "explicit invocation only" wording, so the user starts each skill with `/skill:<name>`.
- Progressive disclosure: the plan template, execution summaries, the ha subagent notes and the TDD test writer brief live in `references/`; each task prompt template stays in `SKILL.md`, because it is the contract every subagent runs under (a run that had to fetch it from `references/` launched children without it).
- Tools: subagents start with `rlm.spawn` and are awaited with `rlm.collect` from the `ipython` tool (`references/ha-subagents.md`); questions go through `ask_user`; documentation comes from a connected Context7 MCP server, else web search.
- Roles: ha has no agent roles. A Sparky launch passes the `gpt-6-luna` selector from `rlm.find_models()` with `thinking="high"` - upstream's `gpt-5.3-codex-spark` is refused for ChatGPT accounts ("not supported when using Codex with a ChatGPT account"), and the user chose `gpt-6-luna` - and starts the prompt with the role's instruction line; the TDD RED phase is a child given `tdd_test_writer`'s instructions (`references/tdd-test-writer.md`).
- Limits: ha runs three children at a time and admits eleven, so the rolling pool of `super-swarm` fills up to what ha admits. Upstream `super-swarm` says both 12 and 15 agents; the import keeps 12, which its scheduling policy and title use.
