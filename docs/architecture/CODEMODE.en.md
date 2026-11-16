# Codemode: one program instead of many tool calls

English | [Tiếng Việt](CODEMODE.vi.md)

When a step needs many tool calls whose results the model only filters,
counts or combines, every result still enters the conversation and is sent
again with every later request. Codemode lets the model write that step as
one JavaScript program instead: the program calls the tools as
`await tools.<name>(args)`, and only what it prints with `text()` or
`console.log` and what it returns comes back. The calls it made, and their
full results, stay out of the context.

It follows pi's codemode (`@earendil-works/pi-codemode`, the package at
`github.com/earendil-works/pi/tree/main/packages/codemode`). pi runs the
program in QuickJS compiled to WebAssembly, in a worker thread. ha has **no
VM**: the program is run by an interpreter written in Rust, in the
`harness-codemode` crate, on a thread of its own.

It is **off by default**: it adds a tool, and the tools' declarations, to
every request. Turn it on with `/codemode on`, `"codemode": true` in
settings.json, or `HA_CODEMODE=on`, which overrides the setting.

Code: `crates/harness-codemode` (the lexer, parser and evaluator, the
built-ins, the `// @options` line, the declarations, the runner),
`crates/harness-tools/src/runtime/codemode.rs` (the tool, the tools a
program reaches, the store), `run_codemode`, `codemode_batch`,
`codemode_gate` and `codemode_request` in
`crates/harness-tools/src/runtime/turn_driver.rs` (the gate for the
program's calls), and the switch in
`crates/harness-cli/src/host/coordination/codemode.rs`.

## 1. Is ha already equivalent? The gap table

ha had several features near codemode before it. None of them was codemode:

| pi codemode | What ha had | Gap, and what was done |
|---|---|---|
| The model writes one program that calls the host's tools; nested calls and their results never enter the LLM context | The Python kernel (`python_repl`, `HA_REPL`) runs model-written code, and `rlm.mcp.call_tool` reaches MCP tools from it. It cannot call ha's own tools (`read_file`, `search_text`, `run_process`, `web_search`, memory, ...), and it needs Python 3.11+ | **Gap.** Ported: the `codemode` tool, whose programs reach every tool of the turn |
| The program's only capability is calling the injected tools: no timers, `fetch`, `process`, `require`, modules | The kernel is full Python with the operating system at hand; it is approved like `run_shell` | **Gap.** Ported: the interpreter has no capability besides `tools` |
| Every nested call is the host's call | `use_tool` and the deferred tools (`HA_TOOL_TIERS`) reach a tool by name, one model call per call. The action bank (`/bank`) runs calls the model fixed in advance, with literal arguments and guards, but no logic between them | **Gap.** A program computes the next call's arguments from the last result. Reused: each nested call goes through the turn's gate (below), deferred tools included |
| `tools.<name>` with names normalized to identifiers (`my-tool` is `tools.my_tool`), `ALL_TOOLS`, an error naming the close matches for a member that does not exist | Nothing | **Gap.** Ported as they are |
| `text()`, `console.*`, `exit()`, the result's output and value, the error kinds `script`, `timeout`, `aborted`, `sandbox`, line numbers `codemode.js:<line>` | A cell's printed output | **Gap.** Ported |
| `store(key, value)` / `load(key)`: small JSON kept across runs, 256 Ki characters a value, 1 Mi in all, written only by a run that succeeded | The kernel keeps its variables between cells | **Gap** for codemode. Ported; the store lasts for the app session, by conversation |
| The `// @options: {"timeout_ms", "max_output_tokens"}` first line | `python_repl`'s `timeout_ms` | **Gap.** Ported; `timeout_ms` is the run's deadline (at most 30 minutes) and `max_output_tokens` the size of what the model reads |
| `renderDeclarations` / `schemaToType`: TypeScript declarations from the tools' JSON Schemas, constraint tags, local `$ref` | `tool_schema` returns a tool's raw JSON Schema | **Gap.** Ported; the tool's description declares the tools the model was sent, short enough for a budget, and names the deferred ones |
| Output limits (16 Mi characters, 100,000 items), a memory limit, catchable deep recursion, a script that waits on a promise nothing can settle fails at once | The cell timeout and output cut | **Gap.** Ported |
| Calls not awaited when the script ends are cancelled and recorded as `cancelled` | - | Ported, differently: a call runs when it is awaited, so a call nothing awaits never runs and is recorded as cancelled |
| Approval hooks for nested calls are skipped unless the host routes them through `runToolCall()` | ha's gate | Better in ha: every nested call crosses the same gate as the model's own |
| QuickJS compiled to WebAssembly, worker threads, `InlineCodemodeSandbox`, `RemoteCodemodeSandbox`, the Cloudflare Durable Object, `acquireWorker`, `onWorkerStarted`, the interrupt budget | - | **Not ported**: ha needs no VM. The interpreter runs on its own thread; the deadline and cancellation are polled every 256 steps |
| `sandbox.start()`: deterministic runs (virtual clock, seeded randomness, results in `seq` order, fingerprints) for replay after a restart | ha does not replay a program from a log | **Not ported**: nothing in ha would use it |
| `CODEMODE_SOURCE_GRAMMAR`, a Lark grammar for grammar-constrained tool input | ha's providers take JSON tool arguments | **Not ported** |
| `image()` | Tool results with images reach the model as attachments | **Not ported**: a program's results are text |

## 2. The language

A program is the body of an async function: `return` and top-level `await`
work. The interpreter reads the JavaScript programs are written in:
`const`/`let`/`var`, functions and arrows (async too), destructuring with
defaults and rest, spread, template literals, optional chaining and `??`,
`if`/`for`/`for...of`/`for...in`/`while`/`do`/`switch`, labels,
`try`/`catch`/`finally`, regular expressions (with look-around and
back-references, through `fancy-regex`), and automatic semicolon insertion.
The built-ins are `JSON`, `Math`, `Object`, `Array`, `String`, `Number`,
`Boolean`, `Promise`, `Map`, `Set`, `RegExp`, `Date` (UTC), the errors,
`parseInt`/`parseFloat`, the URI functions and `structuredClone`. Objects,
`Map` and `Set` keep insertion order.

What a program cannot use is refused with a message that says so: `class`,
generators, getters and setters, `import`/`export`, BigInt, tagged
templates, `with`. There are no timers, so `setTimeout` is not defined and a
`new Promise` nothing resolves fails at once. String positions count
characters (Unicode scalar values), not UTF-16 code units.

## 3. A program's calls

`tools.<name>(args)` returns a promise. The call runs when something awaits
it; `await Promise.all([...])` hands the host every call the list holds at
once. A call nothing awaits never runs and is listed in the result as not
having run.

The driver answers each batch as it answers a batch of the model's own
calls:

1. A call to a control tool (`codemode`, `action_bank`, `drop_context`,
   `tool_schema`, `use_tool`, `ask_user`) or to a tool the turn does not have
   is refused. The arguments are checked against the tool's schema, with the
   same coercion and error text as a model call.
2. Each call crosses its gate in the order the program made them: policy,
   the command guard, `pre_tool_use` hooks, the approval, the durable intent.
   A call the gate refuses is an `Error` the program can catch.
3. The side effects run side by side and settle in order, with their
   receipts and `post_tool_use` hooks. A call that must run alone (a write,
   a process) makes the batch run one call after another.
4. Each call is a card of its own for the user (`<codemode call id>.1`, `.2`,
   ...) and one of the turn's executions, so the changed files, the verify
   reminder and the goal evidence see it. The model sees only the program's
   result.

A call resolves to the text the model would have read for it; `read_file`
gives the file's text without line numbers (secrets stay redacted, and a long
file is cut with a note). The program parses JSON results itself.

## 4. What the model reads

The output in order, then `=> <JSON of the returned value>` or the error,
then the calls that did not succeed:

```text
functions: 1, 3
=> 4
```

```text
started
Script error: Error: stop here
    at codemode.js:2
[1 of 3 tool calls did not succeed: run_process failed]
```

The output is cut in the middle past 60,000 characters, or past
`max_output_tokens` × 4 when the options line sets it. A run that fails keeps
its output up to the failure and writes nothing to the store.

## 5. Limits

| Limit | Value |
|---|---|
| A run, its calls included | 5 minutes; `timeout_ms` up to 30 minutes |
| Output | 16 Mi characters or 100,000 `text()`/`console` calls; past either the script fails, even inside `catch` |
| Memory | about 512 MiB of strings, arrays and objects; past it the script fails as out of memory |
| Recursion | 400 nested calls, then a catchable `RangeError` |
| Store | 256 Ki characters of JSON a value, 1 Mi in all |
| Arrays | 50 million items |

A stopped script is told at its next poll; one stuck in a single long
built-in is not waited for past 3 seconds.

## 6. Diagnostics

Each run writes a `codemode/run` record (calls, failed calls, output items,
milliseconds, the error kind), and each nested call writes the intent,
receipt and gate records any call writes. `/codemode status` says whether it is on and what overrides it.

## 7. Tests

- `crates/harness-codemode`: the lexer, parser, values, declarations and
  options line (unit tests); scripts end to end - output, return values,
  calls and batches, the store, errors and their lines, the built-ins,
  regular expressions, promises, the missing capabilities, the deadline, the
  limits (`tests/script.rs`); the runner's deadline and cancellation while a
  tool runs or the script computes (`tests/execute.rs`).
- `crates/harness-cli/tests/codemode.rs`: the real turn loop with a
  scripted model - the calls go through the gate and only the output
  reaches the model, a refused write is an error the program catches,
  arguments are checked and control tools are unreachable, a failing program
  reports its error with its output, and the tool is offered only when on.
- `scripts/testing/live-tests/codemode.sh`: the real `ha` on a real model.
