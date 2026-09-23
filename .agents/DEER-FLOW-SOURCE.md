# Deer Flow skills source

- Upstream: https://github.com/bytedance/deer-flow/tree/fc26204debe2808fa1b4064f8fddfea6db2c71f8/skills/public
- Source commit: `fc26204debe2808fa1b4064f8fddfea6db2c71f8`
- Imported: `deep-research`, `find-skills`, and `github-deep-research`, including their scripts and report template.
- License: MIT; see [LICENSE-deer-flow](LICENSE-deer-flow).

Project adaptations:

- Added `version: fc26204debe2` to each `SKILL.md` for Harness Agents skill discovery and version pinning.
- Replaced Deer Flow specific `web_fetch` and `web_search` names with instructions to use the available web search and page-open tools.
- Updated `github-deep-research` commands to use this project's skill path. Its GitHub API logic and report template are unchanged; the helper needs Python 3 and can optionally use `GITHUB_TOKEN` from the process environment.
- Updated `find-skills` to install into this project's `.agents/skills` with the Skills CLI. This workflow needs Node.js and `npx`. Its Bash wrapper refuses to replace an existing skill.

Harness Agents discovers project skills only when the project is trusted. This import does not change project trust.
