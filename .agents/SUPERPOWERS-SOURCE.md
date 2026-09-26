# Superpowers skills source

- Upstream: https://github.com/obra/superpowers
- Source commit: `5bf4e78011075bcfc0dc295f0724994cd123ee71`
- Upstream package version: `6.4.1`
- Imported: all 15 directories under upstream `skills/`, including their reference files and scripts.
- Adaptation: added `version: 6.4.1` to each skill's YAML front matter so Harness Agents can report and pin the imported release. Skill bodies and supporting files are otherwise copied from upstream.
- Harness Agents addition: `using-superpowers/references/ha-tools.md` maps the actions skills ask for to ha's tools, and `using-superpowers/SKILL.md` lists it under Platform Adaptation.
- License: MIT; see [LICENSE-superpowers](LICENSE-superpowers).

Harness Agents discovers these skills from `.agents/skills` only while this project is trusted. Project trust is controlled by the user config and is not changed by this import.