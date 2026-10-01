# Subagents in ha

The upstream skill (am-will/swarms) is written for Codex and Claude Code subagents. In Harness Agents (`ha`) a subagent is a child started from the `ipython` tool with the `rlm` object.

| Upstream action | ha |
| --- | --- |
| Launch a subagent with a description and prompt | `await rlm.spawn(prompt, name="T3")`; the name must be unique among your children, so use the task ID |
| Launch several in parallel | one `rlm.spawn` call per child; each returns as soon as the child is admitted |
| Wait for subagents to finish | `await rlm.collect(["T3", "T4"], timeout_ms=1800000)` returns each child's status, answer preview and error |
| See which subagents exist | `await rlm.list_subagents()` |
| Pick a model or role | `model=` takes exactly a selector from `await rlm.find_models()`; `thinking=` takes a level that model supports, and an unsupported level fails the spawn |
| Retry a failed task | spawn it again under a new name (`T3-retry`) with what went wrong |
| AskUserQuestion / request_user_input | `ask_user` |

Use `rlm.spawn`, not the `delegate` tool: a `coder` delegate works in a git worktree of its own and its changes are not merged into this checkout.

## Limits

- Three children run at a time; up to eight more wait in a queue for a slot. A spawn beyond that is refused with "the delegation queue is full": collect a finished child first, then spawn the next.
- A child can start children of its own (one more level); a grandchild cannot.
- A child does not see your conversation: the task prompt must carry everything it needs.
- Collect the results you need with a positive `timeout_ms` before ending your turn.

## What a child can do

A child runs with your tools and your permission mode in this checkout - it edits files, runs tests and commits like you do. Because siblings share the checkout, every child stages and commits only its own task's files and never pushes.

The user watches the children with `/agents` and their messages to each other with `/agents messages`.
