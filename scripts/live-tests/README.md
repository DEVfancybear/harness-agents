# Live tests

End-to-end checks that drive a real `ha` against a real model, the way a user would: agents are started in a pseudo-console, sent messages with `ha send --wait`, restarted and resumed, and what they answered and what they left on disk is checked. Each check prints `PASS` or `FAIL`; the script ends with `RESULT: N passed, M failed` and exits non-zero on a failure. The conversation (`conversation.md`), the recordings and the state stay in `$LIVE_ROOT/<test>` as evidence.

They cost model calls and take minutes, so CI does not run them; run them before a release that touches what they cover.

| Script | Covers |
|---|---|
| `kernel.sh` | `rlm.harness` kept by the Rust host, the `memory` tool and the kernel seeing each other's writes, `/factory`, prime-agent's newer `rlm` (`collect`'s `answer_text`, `rename`, `bash()`, `mcp`), a namespace surviving a restart, `/refinements off` |
| `memory.sh` | After LongMemEval: extraction, project isolation, knowledge update and renamed ids, recall through history, abstention, duplicate refusal, delete and restore, provenance, `/refinements off`, judged skill outcomes, a fact a longer entry holds refused by the tool and the kernel, a `constraints` entry in every turn, the task an earlier conversation left, a quiet session reviewed |
| `prime.sh` | What 0.5.8 ported from prime-agent: `python_repl`, the restore notice, background `bash()`, the Bash card, the prompt stash, cancelling a summary, children in `/session`'s cost, `/new --name`, per-model prompt additions, `compaction.thresholdPercent`, `ha exec` waiting for its children, ACP's `max_tokens`, extra CA certificates, an MCP server's registered OAuth client and callback port |
| `knowledge.sh` | What 0.6.2 changed: the knowledge catalog ranked by the earlier message on a "continue", a page refused for holding a key, `/refine --global` refused a duplicate of a global entry, Deep Dream keeping a constraint's path and a pinned entry and deleting at most half, a review's page not duplicating a subject and marked as the review's, the next turn told what the review changed, and the bug report holding the reviews. And what 0.6.3 changed: `ha exec` reviewing its conversation, a review whose apply cannot take the lock (held by another process) leaving no page, skill or refinement, a failed global part leaving the local refinement standing, a page's age in the catalog, a conversation's own memory consolidated past forty entries, and hybrid search embedding every query variant (against a fake embedding server) |
| `diag.sh` | What 0.6.1 records: a headless run's process and turn records, a compaction with its numbers, a refused provider, a second run refused naming the store's holder (`writer.owner.json`), a background agent's records by agent and conversation, `/bug-report` in the terminal, a worker killed under its supervisor, and `ha bug-report` naming those problems with no credential in it |
| `compaction.sh` | A long task through many compactions (`HA_COMPACT_AT=40000`): rules, a decision and a codeword set at the start and a rule added mid-task are recalled at the end without reading anything and hold on disk; the task state, the summaries, the recaps and the request to save before a summary are checked in the store |

## Running

```bash
cargo build --release -p harness-cli --bin ha
cargo build -p harness-cli --example pty_probe
export HA_CREDENTIALS_DIR="$LOCALAPPDATA/HarnessAgents/data/private"   # where the model's key is
bash scripts/live-tests/memory.sh
```

`env.sh` describes the variables (`HA`, `PROBE`, `PY`, `MODEL`, `LIVE_ROOT`, `HA_CONFIG`); the defaults suit a Windows checkout with Git Bash. Each test copies `HA_CONFIG` into a fresh home under `LIVE_ROOT`, so it never touches the user's own state. `PY` must be a Python 3.11+ with the kernel's packages (the kernel venv's interpreter is one); `kernel.sh` also runs the REPL on it. `prime.sh` reads the recorded screens with `screen.py`, which needs `pyte` (`pip install pyte`, or a `pylib/` beside it, or on `PYTHONPATH`).
