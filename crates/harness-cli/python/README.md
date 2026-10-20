# Vendored REPL runtime

`rlm/` is the kernel-side runtime of [prime-agent](https://github.com/PrimeIntellect-ai/prime-agent)
(`prime-agent-runtime/src/rlm`, MIT, see `LICENSE.prime-agent`), based on commit
`8d58e5b`. Only the standard-library modules are vendored: `repl.py` (the JSON-lines
protocol described in prime-agent's `repl.md`), `bash.py` and `_winjob.py` (`bash()`
handles), `harness.py` and `__init__.py` (the `rlm` namespace), `factory.py` with the
bundled `machines/` library (`rlm.factory`), and `mcp.py` / `mcp_base.py` (the kernel's
`mcp` object; the `mcp` package itself comes with the kernel venv). The `tyro` CLI helper
(`skill.py`) is not included.

Where ha differs from prime-agent:

- `harness.py` is a shim. prime-agent's kernel keeps the harness state itself; in ha the
  host keeps it (`src/host/memory/harness_kernel.rs`), beside the `memory` tool and
  `/refine` that write the same `harness_state.json`. Each `rlm.harness` call is one
  blocking `harness` host request (`repl.host_request_blocking`, answered on the reader
  thread because the loop thread is the one waiting). The API is prime-agent's; the factory
  spec dry run stays in the kernel.
- `factory.py` reads the opt-in `factory.enabled` from ha's `settings.json`
  (`HA_SETTINGS_FILE`); it is off by default, as in prime-agent. In ha,
  `/factory on`, `/factory off` and `/factory status` persist/read this setting.
  `rlm.factory` exposes async `run`, `status`, `stop`, `resume`, `graph` and `watch`
  controls; `help()` remains readable while disabled. Run state lives only in
  kernel memory and does not survive a kernel restart; children remain
  supervisor-owned.
- `__init__.py` also exposes `rlm.factory` on the module, so it survives `import rlm` in a cell
  as `rlm.harness` does.
- `__init__.py` binds `mcp` lazily (`_LazyModule`), so a kernel that never touches MCP does
  not import it; `mcp.py` takes its result parser from `mcp_base.py` and searches configured
  connections as well as connected ones; `_winjob.py` reads a process's start time
  (`process_start_ticks`) for the host.

`ha` embeds these files, writes them under its data directory on first use, and starts
`python -m rlm.repl` with that directory on `PYTHONPATH`. The host bootstraps `rlm`,
`bash` and a lazy `mcp` global. `HA_REPL=off` disables the REPL, `HA_PYTHON` selects
an interpreter and `HA_REPL_PREWARM=off` defers startup until the first cell.

The shipped `rlm` namespace also provides:

- `spawn` for supervisor-owned children (`rlm(...)` and `rlm.run(...)` are unavailable),
  plus `find_models(query="", limit=8)` for selectors backed by active credentials;
- `list_subagents` and `collect(targets=None, timeout_ms=0)` for direct-child result
  snapshots. Zero timeout is non-blocking; positive timeouts return current snapshots
  on expiry without steering the parent;
- `progress_note` for brief child progress (512 UTF-16 code units, throttled to about
  one per 10 seconds), `delete_subagent` for running or retained direct children,
  and `rename` for the current session or a direct child by handle/row/session id,
  not child name;
- `create_session` for resident depth-0 daemon sessions, available only to
  daemon-backed depth-0 callers.

These async operations require the corresponding ha host handlers; importing the
vendored module alone does not create a supervisor or invoke providers.
 Update them by merging the same
files from a newer prime-agent checkout (three-way, against the commit recorded here, so
the differences above survive) and recording the new commit.
