---
name: parallel-task
version: 1102681
description: Orchestrates a `*-plan.md` (from /skill:swarm-planner) by launching parallel subagents wave by wave along each task's `depends_on`, verifying every task with RED -> GREEN test evidence, a local commit and an updated plan entry. Use when the user runs /skill:parallel-task with a plan file and optional task IDs.
disable-model-invocation: true
---

# Parallel Task Executor

You are an Orchestrator for subagents. Parse plan files and delegate tasks to parallel subagents using task dependencies, in a loop, until all tasks are completed. Your role is to ensure that subagents are launched in the correct order (in waves), that they complete their tasks correctly, and that the plan doc is updated with logs after each task is completed.

## Subagents in ha

Launch subagents from the `ipython` tool with `rlm` (read [references/ha-subagents.md](references/ha-subagents.md) with `read_skill_file` before the first launch):

```python
await rlm.spawn(task_prompt, name="T1")          # returns at admission, never waits
results = await rlm.collect(["T1", "T2"], timeout_ms=1800000)
```

ha runs three children at a time and queues eight more; a child works in this checkout with your permissions, so it can edit, test and commit.

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
   - **depends_on** list (from `- **depends_on**: [...]`)
   - Full content (description, location, acceptance criteria, validation)
3. Build the task list
4. If a task subset was requested, filter the task list to only those IDs and their required dependencies.

### Step 3: Launch Subagents

For each **unblocked** task, spawn one child named after the task ID, with the Task Prompt Template below filled in. A task is unblocked if all IDs in its depends_on list are complete. Launch all unblocked tasks in parallel (one `rlm.spawn` call each).

Each task is only complete after either RED -> GREEN test evidence or explicit non-testable verification evidence is provided, the task is committed, and the plan is updated.

### Task Prompt Template

Fill in one prompt per task and send it exactly as written - every instruction is part of the task contract.

Before the first launch, read [references/tdd-test-writer.md](references/tdd-test-writer.md) with `read_skill_file` and append its brief (everything after its `---` line) to every task prompt under a `## TDD test writer brief` heading: the child does not see this skill and passes that brief on to its own test-writer child.

```
You are implementing a specific task from a development plan.

## Context
- Plan: [filename]
- Goals: [relevant overview from plan]
- Dependencies: [prerequisites for this task]
- Related tasks: [tasks that depend on or are depended on by this task]
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
2. Read all relevant files first, then do targeted codebase research (related modules, tests, call sites, and dependencies) to confirm the approach.
3. Default to TDD RED phase first using a test-writer subagent: from the `ipython` tool, `await rlm.spawn(brief, name="[ID]-tdd")` with the TDD test writer brief below followed by the task, then `await rlm.collect(["[ID]-tdd"], timeout_ms=1200000)`.
   - Pass task context and acceptance criteria.
   - Require tests-only edits.
   - Require command output proving the new/updated tests fail for the expected behavior gap.
   - If the task is not a good TDD candidate, explicitly record `reason_not_testable` and define alternative verification evidence (for example `manual_check`, `static_check`, or `runtime_check`) with an exact command or concrete validation steps.
4. Review RED-phase tests (or approved non-testable verification plan) as the implementation contract. Do not weaken or remove tests unless requirements changed.
5. Implement production changes for all acceptance criteria.
6. Run validation:
   - For testable tasks, run the exact new/updated test command(s) until GREEN (passing).
   - For non-testable tasks, run the agreed alternative verification and capture evidence.
   - Run any additional validation steps from the plan if feasible.
7. Commit your work.
   - Stage only files for this task because other agents are working in parallel.
   - NEVER PUSH. ONLY COMMIT.
8. After the commit, update the `*-plan.md` task entry with:
   - Completion status
   - Concise work log
   - Files modified/created
   - Errors or gotchas encountered
9. Return summary of:
   - Files modified/created
   - Changes made
   - How criteria are satisfied
   - Verification evidence: RED -> GREEN or documented non-testable alternative
   - Validation performed or deferred

## Important
- Be careful with paths
- Stop and describe blockers if encountered
- Focus on this specific task
```

### Step 4: Check and Validate

After the children of a wave settle (`rlm.collect`):
1. Inspect their outputs for correctness and completeness.
2. Validate the results against the expected outcomes.
3. If the task is truly completed correctly, ensure the task commit exists (`git_log`) and that the task is marked complete with logs in the plan.
4. If a task was not successful, have the agent retry (spawn it again with what went wrong) or escalate the issue to the user.
5. Ensure that wave of work is committed locally before moving on to the next wave.

### Step 5: Repeat

1. Review the plan again to see what new set of unblocked tasks is available.
2. Continue launching unblocked tasks in parallel until the plan is done.
3. Repeat until all tasks are complete, validated (RED -> GREEN or documented non-testable verification), committed, and logged without errors.

Finish with the summary in [references/execution-summary.md](references/execution-summary.md).

## Error Handling

- Task subset not found: List available task IDs
- Parse failure: Show what was tried, ask for clarification with `ask_user`

## Example Usage

```
/skill:parallel-task plan.md
/skill:parallel-task ./plans/auth-plan.md T1 T2 T4
/skill:parallel-task user-profile-plan.md --tasks T3 T7
```
