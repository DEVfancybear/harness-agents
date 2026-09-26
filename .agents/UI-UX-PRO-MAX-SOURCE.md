# UI/UX Pro Max skill source

- Upstream: https://github.com/nextlevelbuilder/ui-ux-pro-max-skill (skill source under `src/ui-ux-pro-max`, rendered per platform by its CLI)
- Source commit: `d62fe62f88acb6755d2e5819a1b1e19eb447a8f1`
- Imported: `ui-ux-pro-max` in its installed form (`SKILL.md`, `references/`, `data/`, and the search scripts `search.py`, `core.py`, `design_system.py`, `reasoning_contract.py`).
- License: MIT; see [LICENSE-ui-ux-pro-max](LICENSE-ui-ux-pro-max). Font metadata licensing is kept in `skills/ui-ux-pro-max/data/google-font-licenses.json`, and dataset provenance in `data/data-provenance.json`.

Project adaptations:

- `SKILL.md`: added `version: d62fe62f88ac` for Harness Agents skill discovery; the search commands use this project's skill path (`<skill-directory>/scripts/search.py`) instead of the Claude Code plugin root; the missing-Python note points at `references/quick-reference.md` instead of a README that is not shipped.
- Left out upstream's maintainer tooling, which the skill never runs: `scripts/tests/`, `scripts/validate_data.py`, and its catalog-refresh input `data/phosphor-icons-upstream.json` (824 KB). Every data file the search reads is included and unchanged.

Verified on Windows 11 with Python 3: `--design-system`, `--domain` and `--stack` searches run with the standard library only.

Harness Agents discovers project skills only when the project is trusted. This import does not change project trust.
