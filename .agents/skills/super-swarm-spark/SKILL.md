---
name: super-swarm-spark
version: 1102681
description: Executes a `*-plan.md` with a rolling pool of up to 12 parallel Sparky subagents (GPT-6 Luna at high thinking) that ignores dependency maps, gives each subagent canonical file paths, checks every result, then runs a final integration and test pass. Use when the user runs /skill:super-swarm-spark with a plan file and optional task IDs.
disable-model-invocation: true
---

# Parallel Task Executor (Sparky Rolling 12-Agent Pool)

You are an Orchestrator for subagents. Parse plan files and delegate tasks in parallel using a rolling pool of up to 12 concurrent Sparky subagents. Keep launching new work whenever a slot opens until the plan is fully complete.

Primary orchestration goals:
- Keep the project moving continuously
- Ignore dependency maps
- Keep up to 12 agents running whenever pending work exists
- Give every subagent maximum path/file context
- Prevent filename/folder-name drift across parallel tasks
- Check every subagent result
- Ensure the plan file is updated as tasks complete
- Perform final integration fixes after all task execution
- Add/adjust tests, then run tests and fix failures

## Subagents in ha

Launch subagents from the `ipython` tool with `rlm`; read [references/ha-subagents.md](references/ha-subagents.md) with `read_skill_file` before the first launch:

```python
sparky = "<the gpt-6-luna selector from await rlm.find_models('gpt-6-luna')>"
await rlm.spawn(task_prompt, name="T1", model=sparky, thinking="high")   # returns at admission
done = await rlm.collect(["T1", "T2"], timeout_ms=60000)
```

ha runs three children at a time and admits at most eleven (three running, eight waiting): the twelfth spawn is refused with "the delegation queue is full". Fill the pool up to what ha admits, and when a spawn is refused, collect a settled child before spawning the next.

Start every task subagent with `rlm.spawn`, never with the `delegate` tool: a `coder` delegate works in a git worktree of its own, so its commits and plan updates never reach this checkout.

## Process

### Step 1: Parse Request

Extract from the user request:
1. **Plan file**: The markdown plan to read
2. **Task subset** (optional): Specific task IDs to run

If no subset is provided, run the full plan.

### Step 2: Read & Parse Plan

1. Find task subsections (e.g., `### T1:` or `### Task 1.1:`)
2. For each task, extract:
   - Task ID and name
   - Task linkage metadata for context only
   - Full content (description, location, acceptance criteria, validation)
3. Build the task list
4. If a task subset was requested, filter to only those IDs.

### Step 3: Build Context Pack Per Task

Before launching a task, prepare a context pack that includes:
- Canonical file paths and folder paths the task must touch
- Planned new filenames (exact names, not suggestions)
- Neighboring tasks that touch the same files/folders
- Naming constraints and conventions from the plan/repo
- Any known cross-task expectations that could cause conflicts

Rules:
- Do not allow subagents to invent alternate file names for the same intent.
- Require explicit file targets in every subagent assignment.
- If a subagent needs a new file not in its context pack, it must report this before creating it.

### Step 4: Launch Subagents (Rolling Pool, Max 12)

Run a rolling scheduler:
- States: `pending`, `running`, `completed`, `failed`
- Launch up to 12 tasks immediately (or fewer if less are pending, or as many as ha admits)
- Whenever any running task finishes, validate/update the plan for that task, then launch the next pending task immediately
- Continue until no pending or running tasks remain

Spawn each task as a child named after its task ID with the Task Prompt Template below. Do not wait for grouped batches. The only concurrency limit is 12 active Sparky subagents (and what ha admits).

Every launch must be a Sparky subagent. Upstream sets `agent_type: sparky`, the am-will/codex-skills role with the instructions "You are a general-purpose worker agent. Execute tasks as instructed, write clean code, and follow project conventions." Upstream runs it on `gpt-5.3-codex-spark`, which a ChatGPT account cannot use; in ha Sparky runs on `gpt-6-luna` at `high` thinking. ha has no agent roles, so every launch passes `model=` with the `gpt-6-luna` selector from `await rlm.find_models("gpt-6-luna")` and `thinking="high"`, and the task prompt starts with that instruction line. Any other model is invalid for this skill: if `rlm.find_models("gpt-6-luna")` does not offer it (it needs a ChatGPT login), stop and tell the user to log in or use /skill:super-swarm instead. If the first Sparky child fails because the provider refuses the model for this account, stop the same way and quote the provider's error; do not fall back to another model.

### Task Prompt Template

Fill in one prompt per task from its context pack and send it exactly as written - every instruction is part of the task contract.

```
You are a general-purpose worker agent. Execute tasks as instructed, write clean code, and follow project conventions.

You are implementing a specific task from a development plan.

## Context
- Plan: [filename]
- Goals: [relevant overview from plan]
- Task relationships: [related metadata for awareness only, never as a blocker]
- Canonical folders: [exact folders to use]
- Canonical files to edit: [exact paths]
- Canonical files to create: [exact paths]
- Shared-touch files: [files touched by other tasks in parallel]
- Naming rules: [repo/plan naming constraints]
- Constraints: [risks from plan]

## Your Task
**Task [ID]: [Name]**

Location: [File paths]
Description: [Full description]

Acceptance Criteria:
[List from plan]

Validation:
[Tests or verification from plan]

## Instructions
1. Examine the plan and all listed canonical paths before editing
2. Implement changes for all acceptance criteria
3. Keep work atomic and committable
4. For each file: read first, edit carefully, preserve formatting
5. Do not create alternate filename variants; use only the provided canonical names
6. If you need to touch/create a path not listed, stop and report it first
7. Run validation if feasible
8. ALWAYS mark completed tasks IN THE *-plan.md file AS SOON AS YOU COMPLETE IT! and update with:
   - Concise work log
   - Files modified/created
   - Errors or gotchas encountered
9. Commit your work
   - Note: There are other agents working in parallel to you, so only stage and commit the files you worked on. NEVER PUSH. ONLY COMMIT.
10. Double check that you updated the *-plan.md file and committed your work before yielding
11. Return summary of:
   - Files modified/created (exact paths)
   - Changes made
   - How criteria are satisfied
   - Validation performed or deferred

## Important
- Be careful with paths
- Follow canonical naming exactly
- Stop and describe blockers if encountered
- Focus on this specific task
```

### Step 5: Validate Every Completion

As each subagent finishes:
1. Inspect output for correctness and completeness.
2. Validate against expected outcomes for that task.
3. Ensure plan file completion state + logs were updated correctly.
4. Retry/escalate on failure.
5. Keep the scheduler full: after validation, immediately launch the next pending task if a slot is open.

### Step 6: Final Orchestrator Integration Pass

After all subagents are done:
1. Reconcile parallel-work conflicts and cross-task breakage.
2. Resolve duplicate/variant filenames and converge to canonical paths.
3. Ensure the plan is fully and accurately updated.
4. Add or adjust tests to cover integration/regression gaps.
5. Run required tests.
6. Fix failures.
7. Re-run tests until green (or report explicit blockers with evidence).

Completion bar:
- All plan tasks marked complete with logs
- Integrated codebase builds/tests per plan expectations
- No unresolved path/name divergence introduced by parallel execution

Finish with the summary in [references/execution-summary.md](references/execution-summary.md).

## Scheduling Policy (Required)

- Max concurrent subagents: **12**
- If pending tasks exist and running count is below 12: launch more immediately
- Do not pause due to relationship metadata
- Continue until the full plan (or requested subset) is complete and integrated

## Error Handling

- Task subset not found: List available task IDs
- Parse failure: Show what was tried, ask for clarification with `ask_user`
- Path ambiguity across tasks: pick one canonical path, announce it, and enforce it in all task prompts

## Example Usage

```
/skill:super-swarm-spark plan.md
/skill:super-swarm-spark ./plans/auth-plan.md T1 T2 T4
/skill:super-swarm-spark user-profile-plan.md --tasks T3 T7
```
