# Anthropic skills source

- Upstream: https://github.com/anthropics/skills/tree/33375500bcea98d610eb30ce10ac4e59b89c390d/skills/web-artifacts-builder
- Source commit: `33375500bcea98d610eb30ce10ac4e59b89c390d`
- Imported: `web-artifacts-builder`, including its scripts and the shadcn/ui component archive.
- License: Apache-2.0; see `skills/web-artifacts-builder/LICENSE.txt`.

Project adaptations (modified files carry these changes):

- `SKILL.md`: added `version: 33375500bcea` for Harness Agents skill discovery; commands use this project's skill path (`<skill-directory>/scripts/...`); the claude.ai artifact steps now hand the user the path of `bundle.html` and name each platform's opener; stated the Bash, Node.js and network requirements.
- `scripts/init-artifact.sh`: removes any template icon link (newer Vite templates link `favicon.svg`, which Parcel cannot resolve, where older ones linked `vite.svg`); refuses a Windows project path longer than 110 characters, which otherwise fails bundling on the 260-character path limit.
- `scripts/bundle-artifact.sh`: approves the build scripts of Parcel's native helpers in `pnpm-workspace.yaml`, since pnpm 10+ does not run them and newer pnpm fails the install (`ERR_PNPM_IGNORED_BUILDS`); prints the full path of `bundle.html`.
- `scripts/shadcn-components.tar.gz` and `LICENSE.txt` are unchanged.

Verified on Windows 11 with Git Bash, Node.js 24 and pnpm 12: the project initializes, bundles to one HTML file, and the bundled app renders and runs in a browser.
