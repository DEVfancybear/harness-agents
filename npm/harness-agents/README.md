# harness-agents

`ha` is a local coding agent written in Rust. It runs in your terminal against your project, calls a model provider (DeepSeek by default), reads and edits code through approval-gated tools, and persists conversations and tool results locally, with learned memory and Markdown knowledge in scoped local files, so a session can be resumed, inspected or continued later.

It includes an interactive terminal UI, one-shot `ha exec`, delegation, and local background agents (enabled by default for interactive sessions). Durable task checklists and checked goals support longer work; `/knowledge` searches saved research and `ha knowledge serve` explicitly opens its local console. The Python factory runs stored state machines of child agents only after opt-in (`/factory on`); it is off by default.

```powershell
npm install -g harness-agents
ha --version
ha            # open the app, then /login to pick a provider
```

npm puts `ha` in its global directory (`npm prefix -g`); if `ha` is not found in a new terminal, add that directory to your PATH.

Windows 10/11 x64 only for now; Linux is not supported yet. The package carries `ha.exe`, the pinned `uv.exe` its Python kernel uses, `ha.release.json` (version, rustc, source revision and the executable's digest) and `checksums.txt`.

Documentation, source and releases: https://github.com/DEVfancybear/harness-agents
