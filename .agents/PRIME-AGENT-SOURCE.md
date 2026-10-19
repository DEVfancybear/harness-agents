# Prime Agent skills source

- Upstream: https://github.com/PrimeIntellect-ai/prime-agent/tree/8d58e5b6f9ec32b728ba596a0460afaff3fbb432/skills
- Source commit: `8d58e5b6f9ec32b728ba596a0460afaff3fbb432` (first imported at `e260085`, from `packages/coding-agent/skills`, before prime-agent moved its skills to `skills/`)
- Imported: `agent-message`, `agent-observe`, `attach-image`, `compact`, `edit`, `factory`, `goal`, `mcp`, `refine`, `rlm-heartbeat`, `skill-creator`, and `websearch`, with their Python packages and references. `prime-intellect` (Prime Intellect's own platform) is not imported.
- License: MIT; see [LICENSE-prime-agent](LICENSE-prime-agent).

Project adaptations:

- Added `version: 8d58e5b` to each `SKILL.md` for Harness Agents skill discovery and version pinning.
- The Python packages are unchanged but for `websearch` below. Harness Agents puts each skill's `src` directory on the Python REPL kernel's `PYTHONPATH` and imports it by its package name when the kernel starts, as Prime Agent pre-imports its Python skills; a skill whose import fails is reported unavailable instead of failing the kernel.
- `websearch`: the key is the one `/login serper` saves (`HA_CREDENTIAL_FILE`) or `SERPER_API_KEY`, and a missing key points to the `web_search` tool, which needs none; Prime Agent's `/mcp` setup flow is not part of Harness Agents.
- `factory`: `/factory on|off|status` works as in Prime Agent, writing `factory.enabled` in Harness Agents' `settings.json`.
- The shell entry points (`<skill> ...`, `rlm.skill:cli`) are not installed: call the skills from the REPL.
- Host requests the skills send are answered by Harness Agents where it has the feature, and with an explicit error where it does not (for example daemon sessions).
