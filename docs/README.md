# Documentation index

Everything an operator, contributor or reviewer reads lives under this folder. The
restructure that produced this layout moved files only: no contract, constant or
behaviour changed, and every reference was rewritten to the new path.

## Index

| Folder | Documents |
| --- | --- |
| `architecture/` | [Architecture overview](architecture/ARCHITECTURE_OVERVIEW.en.md) · [Tổng quan kiến trúc](architecture/ARCHITECTURE_OVERVIEW.vi.md) · [Plugin architecture](architecture/PLUGIN_ARCHITECTURE.en.md) · [Kiến trúc plugin](architecture/PLUGIN_ARCHITECTURE.vi.md) |
| `guides/` | [Operator guide](guides/OPERATOR_GUIDE.en.md) · [Hướng dẫn vận hành](guides/OPERATOR_GUIDE.vi.md) · [Terminal interface](guides/TUI.md) · [Computer use](guides/COMPUTER_USE.md) · [Build and release](guides/BUILD_AND_RELEASE.md) |
| `adr/` | Decision records ADR-N01 to ADR-N12 |
| `support/` | [Strict execution support matrix](support/STRICT_EXECUTION_SUPPORT.vi.md) |
| `assets/` | Screenshots the README and the guides embed |

The scripts those documents name live under `scripts/`, grouped by what they do:
`common/` (shared helpers), `install/`, `release/`, `testing/` (pseudo-terminal
acceptance, provider smoke test and the live tests) and `verify/` (the phase,
milestone, launch and documentation gates).

## Migration map

Every row is a rename; content changed only where a path was written down.

| Before | After |
| --- | --- |
| `scripts/GateTestIsolation.ps1` | `scripts/common/GateTestIsolation.ps1` |
| `scripts/Get-HaUv.ps1` | `scripts/common/Get-HaUv.ps1` |
| `scripts/Install-Ha.ps1` | `scripts/install/Install-Ha.ps1` |
| `scripts/New-HaNpmPackages.ps1` | `scripts/release/New-HaNpmPackages.ps1` |
| `scripts/New-HaRelease.ps1` | `scripts/release/New-HaRelease.ps1` |
| `scripts/Invoke-HaPtyAcceptance.ps1` | `scripts/testing/Invoke-HaPtyAcceptance.ps1` |
| `scripts/Smoke-HaProvider.ps1` | `scripts/testing/Smoke-HaProvider.ps1` |
| `scripts/Read-HaTranscript.mjs` | `scripts/testing/Read-HaTranscript.mjs` |
| `scripts/smoke_computer_use.py` | `scripts/testing/smoke_computer_use.py` |
| `scripts/live-tests/` (twelve files) | `scripts/testing/live-tests/` |
| `scripts/Verify-Docs.ps1` | `scripts/verify/Verify-Docs.ps1` |
| `scripts/Verify-HaLaunch.ps1` | `scripts/verify/Verify-HaLaunch.ps1` |
| `scripts/Verify-Milestone.ps1` | `scripts/verify/Verify-Milestone.ps1` |
| `scripts/Verify-Phase.ps1` | `scripts/verify/Verify-Phase.ps1` |
| `docs/ARCHITECTURE_OVERVIEW.en.md` | `docs/architecture/ARCHITECTURE_OVERVIEW.en.md` |
| `docs/ARCHITECTURE_OVERVIEW.vi.md` | `docs/architecture/ARCHITECTURE_OVERVIEW.vi.md` |
| `docs/PLUGIN_ARCHITECTURE.en.md` | `docs/architecture/PLUGIN_ARCHITECTURE.en.md` |
| `docs/PLUGIN_ARCHITECTURE.vi.md` | `docs/architecture/PLUGIN_ARCHITECTURE.vi.md` |
| `docs/OPERATOR_GUIDE.en.md` | `docs/guides/OPERATOR_GUIDE.en.md` |
| `docs/OPERATOR_GUIDE.vi.md` | `docs/guides/OPERATOR_GUIDE.vi.md` |
| `docs/BUILD_AND_RELEASE.md` | `docs/guides/BUILD_AND_RELEASE.md` |
| `docs/COMPUTER_USE.md` | `docs/guides/COMPUTER_USE.md` |
| `docs/TUI.md` | `docs/guides/TUI.md` |

## References

- The crate layout this workspace's split was reviewed against: [Prime Agent crates](https://github.com/PrimeIntellect-ai/prime-agent/tree/main/crates).
