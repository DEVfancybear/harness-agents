# Working on Harness Agents

Instructions for any agent (or person) changing this repository.

## Done means tested live

A change is not done, and is not released, until it has passed **live**: the real `ha`
driven against a real model, the way a user would run it. Unit tests and the gate below
are required too, but they are not enough. They have passed while the product was broken.

- **What needs it:** every change a user can see or that touches the agent loop, tools,
  delegation and children, compaction, memory and knowledge, diagnostics and the bug
  report, the worker, or the TUI. Documentation and comments alone are the exception.
- **How:** build the release `ha` and `pty_probe` (`cargo build --release -p harness-cli`,
  `cargo build -p harness-cli --example pty_probe`). Then run the suites in
  [`scripts/testing/live-tests`](scripts/testing/live-tests/README.md) that cover what
  you changed; that README's table says what each suite covers.
- **Nothing covers it?** Add checks to the nearest suite, or write a new one. A check
  should fail on the previous release and pass on yours. Run it against the previous
  build as a control when you can: that is the evidence the fix is real.
- **Model:** `MODEL=openai-codex/gpt-6-luna` with `THINKING=max`, unless the maintainer
  names another. `HA_CREDENTIALS_DIR` points at the credentials. Never print, copy or
  commit their values.
- **All checks pass, on the exact binary that is released.**
  - A failure "caused by the model" is not a pass. Make the test's precondition explicit
    and judge only when it held. Retry when the model skipped a step (for example, it
    did not wait).
  - If a product change made the failure expected, fix the test to match. Then rerun.
- **Rerun only what failed:**
  - After a fix to a test script alone, rerun just the failing suites, on the same
    build. Suites that passed stay passed.
  - After a code change, the gate runs again, plus every live suite the change can
    affect.
- **Report the evidence:**
  - each suite's `RESULT: N passed, M failed` line;
  - the SHA-256 of the `ha.exe` it ran on and the commit;
  - where the logs and `$LIVE_ROOT/<suite>` evidence are;
  - for a control run, what failed on the previous build.

## The gate

Run these before any live run that counts. Unset the live-test variables first:
`HA`, `HA_HOME`, `HA_CREDENTIALS_DIR`, `HA_PYTHON`, `PROBE`, `PROBE_WT`, `MODEL`,
`THINKING`, `LIVE_ROOT`, `PYTHONPATH`.

```
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --no-fail-fast
```

`m9_04_release_candidate_has_checksums_and_is_not_published` needs a candidate under
`target/release-candidate`. Build one with
`pwsh -NoProfile -File scripts/release/New-HaRelease.ps1`.

## Versions and releases

- Do not bump the version or release unless the maintainer says so.
- To release:
  1. Bump `[workspace.package] version` and the workspace crates in `Cargo.lock`.
  2. Build the candidate with `New-HaRelease.ps1` and run the live suites on its `ha.exe`.
  3. Commit `chore(release): ha X.Y.Z` and push `master`.
  4. Tag `ha-vX.Y.Z` and create the GitHub release with the zip and `checksums.txt`.
  5. Download the release and check that the SHA-256 matches.
- Work in a worktree of your own. Do not merge or edit in a checkout someone else is
  using.
