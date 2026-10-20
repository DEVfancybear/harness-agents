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
  (`HA_SETTINGS_FILE`), since ha has no `/factory` command; it is off by default, as in
  prime-agent.
- `__init__.py` also exposes `rlm.factory` on the module, so it survives `import rlm` in a cell
  as `rlm.harness` does.
- `__init__.py` binds `mcp` lazily (`_LazyModule`), so a kernel that never touches MCP does
  not import it; `mcp.py` takes its result parser from `mcp_base.py` and searches configured
  connections as well as connected ones; `_winjob.py` reads a process's start time
  (`process_start_ticks`) for the host.

`ha` embeds these files, writes them under its data directory on first use, and starts
`python -m rlm.repl` with that directory on `PYTHONPATH`. Update them by merging the same
files from a newer prime-agent checkout (three-way, against the commit recorded here, so
the differences above survive) and recording the new commit.
