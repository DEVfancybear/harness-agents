---
name: super-swarm
version: 1102681
description: Executes a `*-plan.md` with a rolling pool of up to 12 parallel subagents that ignores dependency maps, gives each subagent canonical file paths, checks every result with RED -> GREEN evidence, then runs a final integration and test pass. Use when the user runs /skill:super-swarm with a plan file and optional task IDs.
disable-model-invocation: true
---

# Parallel Task Executor (Rolling 12-Agent Pool)

You are an Orchestrator for subagents. Parse plan files and delegate tasks in parallel using a rolling pool of up to 12 concurrent subagents. Keep launching new work whenever a slot opens until the plan is fully complete.

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
await rlm.spawn(task_prompt, name="T1")          # returns at admission, never waits
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

Spawn each task as a child named after its task ID with the Task Prompt Template below. Do not wait for grouped batches. The only concurrency limit is 12 active subagents (and what ha admits).

### Task Prompt Template

Fill in one prompt per task from its context pack and send it exactly as written - every instruction is part of the task contract.

Before the first launch, read [references/tdd-test-writer.md](references/tdd-test-writer.md) with `read_skill_file` and append its brief (everything after its `---` line) to every task prompt under a `## TDD test writer brief` heading: the child does not see this skill and passes that brief on to its own test-writer child.

```
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
1. Read the working plan and fully understand this task before coding.
2. Examine the plan and all listed canonical paths before editing.
3. Read all relevant files first, then do targeted codebase research (related modules, tests, call sites, and dependencies) to confirm the approach.
4. Default to TDD RED phase first using a test-writer subagent: from the `ipython` tool, `await rlm.spawn(brief, name="[ID]-tdd")` with the TDD test writer brief below followed by the task, then `await rlm.collect(["[ID]-tdd"], timeout_ms=1200000)`.
   - Pass task context, canonical paths, and acceptance criteria.
   - Require tests-only edits.
   - Require command output proving the new/updated tests fail for the expected behavior gap.
   - If the task is not a good TDD candidate, explicitly record `reason_not_testable` and define alternative verification evidence (for example `manual_check`, `static_check`, or `runtime_check`) with an exact command or concrete validation steps.
5. Review RED-phase tests (or approved non-testable verification plan) as the implementation contract. Do not weaken or remove tests unless requirements changed.
6. Implement production changes for all acceptance criteria.
7. Keep work atomic and committable.
8. For each file: read first, edit carefully, preserve formatting.
9. Do not create alternate filename variants; use only the provided canonical names.
10. If you need to touch/create a path not listed, stop and report it first.
11. Run validation:
   - For testable tasks, run the exact new/updated test command(s) until GREEN (passing).
   - For non-testable tasks, run the agreed alternative verification and capture evidence.
   - Run any additional validation steps from the plan if feasible.
12. Commit your work.
   - Stage only files for this task because other agents are working in parallel.
   - NEVER PUSH. ONLY COMMIT.
13. After the commit, update the `*-plan.md` task entry with:
   - Completion status
   - Concise work log
   - Files modified/created
   - Errors or gotchas encountered
14. Return summary of:
   - Files modified/created (exact paths)
   - Changes made
   - How criteria are satisfied
   - Verification evidence: RED -> GREEN or documented non-testable alternative
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
3. Ensure RED -> GREEN test evidence or explicit non-testable verification evidence is present.
4. Ensure the task commit exists (`git_log`) and the plan file completion state + logs were updated correctly.
5. Retry/escalate on failure.
6. Keep the scheduler full: after validation, immediately launch the next pending task if a slot is open.

### Step 6: Final Orchestrator Integration Pass

After all subagents are done:
1. Reconcile parallel-work conflicts and cross-task breakage.
2. Resolve duplicate/variant filenames and converge to canonical paths.
3. Ensure the plan is fully and accurately updated.
4. Add or adjust tests to cover integration/regression gaps where task-level RED coverage missed cross-task behavior.
5. Run required tests.
6. Fix failures.
7. Re-run tests until GREEN (or report explicit blockers with evidence).

Completion bar:
- All plan tasks marked complete with logs
- Every task has RED -> GREEN evidence or documented non-testable verification with concrete commands/steps
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
/skill:super-swarm plan.md
/skill:super-swarm ./plans/auth-plan.md T1 T2 T4
/skill:super-swarm user-profile-plan.md --tasks T3 T7
```
