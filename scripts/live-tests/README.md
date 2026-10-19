# Live tests

End-to-end checks that drive a real `ha` against a real model, the way a user would: agents are started in a pseudo-console, sent messages with `ha send --wait`, restarted and resumed, and what they answered and what they left on disk is checked. Each check prints `PASS` or `FAIL`; the script ends with `RESULT: N passed, M failed` and exits non-zero on a failure. The conversation (`conversation.md`), the recordings and the state stay in `$LIVE_ROOT/<test>` as evidence.

They cost model calls and take minutes, so CI does not run them; run them before a release that touches what they cover.

| Script | Covers |
|---|---|
| `kernel.sh` | `rlm.harness` kept by the Rust host, the `memory` tool and the kernel seeing each other's writes, `/factory`, prime-agent's newer `rlm` (`collect`'s `answer_text`, `rename`, `bash()`, `mcp`), a namespace surviving a restart, `/refinements off` |
| `memory.sh` | After LongMemEval: extraction, project isolation, knowledge update and renamed ids, recall through history, abstention, duplicate refusal, delete and restore, provenance, `/refinements off`, judged skill outcomes |

## Running

```bash
cargo build --release -p harness-cli --bin ha
cargo build -p harness-cli --example pty_probe
export HA_CREDENTIALS_DIR="$LOCALAPPDATA/HarnessAgents/data/private"   # where the model's key is
bash scripts/live-tests/memory.sh
```

`env.sh` describes the variables (`HA`, `PROBE`, `PY`, `MODEL`, `LIVE_ROOT`, `HA_CONFIG`); the defaults suit a Windows checkout with Git Bash. Each test copies `HA_CONFIG` into a fresh home under `LIVE_ROOT`, so it never touches the user's own state. `PY` must be a Python 3.11+ with the kernel's packages (the kernel venv's interpreter is one); `kernel.sh` also runs the REPL on it.
