# Prime Agent skills source

- Upstream: https://github.com/PrimeIntellect-ai/prime-agent/tree/e260085dd8f742e0def3d871860c9a888b114851/packages/coding-agent/skills
- Source commit: `e260085dd8f742e0def3d871860c9a888b114851`
- Imported: `agent-message`, `agent-observe`, `attach-image`, `compact`, `edit`, `goal`, `mcp`, `prime-intellect`, `refine`, `rlm-heartbeat`, `skill-creator`, and `websearch`, with their Python packages and references.
- License: MIT; see [LICENSE-prime-agent](LICENSE-prime-agent).

Project adaptations:

- Added `version: e260085` to each `SKILL.md` for Harness Agents skill discovery and version pinning.
- The Python packages are unchanged. Harness Agents puts each skill's `src` directory on the Python REPL kernel's `PYTHONPATH` and imports it by its package name when the kernel starts, as Prime Agent pre-imports its Python skills; a skill whose import fails is reported unavailable instead of failing the kernel.
- `websearch/SKILL.md`: the setup section names `SERPER_API_KEY` instead of Prime Agent's `/login` flow, which Harness Agents does not have.
- The shell entry points (`<skill> ...`, `rlm.skill:cli`) are not installed: call the skills from the REPL.
- Host requests the skills send are answered by Harness Agents where it has the feature, and with an explicit error where it does not (for example daemon sessions).
