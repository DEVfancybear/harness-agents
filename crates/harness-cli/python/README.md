# Vendored REPL runtime

`rlm/` is the kernel-side runtime of [prime-agent](https://github.com/PrimeIntellect-ai/prime-agent)
(`prime-agent-runtime/src/rlm`, MIT, see `LICENSE.prime-agent`), taken unchanged at commit
`e260085`. Only the standard-library modules are vendored: `repl.py` (the JSON-lines
protocol described in prime-agent's `repl.md`), `bash.py` and `_winjob.py` (`bash()`
handles), `harness.py` and `__init__.py` (the `rlm` namespace), and `mcp.py` / `mcp_base.py` (the
kernel's `mcp` object; the `mcp` package itself comes with the kernel venv). The `tyro` CLI
helper (`skill.py`) is not included.

`ha` embeds these files, writes them under its data directory on first use, and starts
`python -m rlm.repl` with that directory on `PYTHONPATH`. Update them by copying the
same files from a newer prime-agent checkout and recording the commit here.
