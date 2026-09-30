# harness-agents

`ha` is a local coding agent written in Rust. It runs in your terminal against your project, calls a model provider (DeepSeek by default), reads and edits code through approval-gated tools, and keeps every conversation, tool result and learned fact in a local SQLite store, so a session can be resumed, inspected or continued later.

```powershell
npm install -g harness-agents
ha --version
ha            # open the app, then /login to pick a provider
```

Windows 10/11 x64 only for now; Linux is not supported yet. The package carries `ha.exe`, the pinned `uv.exe` its Python kernel uses, `ha.release.json` (version, rustc, source revision and the executable's digest) and `checksums.txt`.

Documentation, source and releases: https://github.com/DEVfancybear/harness-agents
