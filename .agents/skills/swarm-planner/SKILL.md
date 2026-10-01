---
name: swarm-planner
version: 1102681
description: Creates dependency-aware implementation plans optimized for parallel multi-agent execution, saved as `<topic>-plan.md` with explicit `depends_on` per task. Use when the user runs /skill:swarm-planner to plan a feature or product before executing it with /skill:parallel-task, /skill:parallel-task-spark, /skill:super-swarm or /skill:super-swarm-spark.
disable-model-invocation: true
---

# Swarm-Ready Planner

Create implementation plans with explicit task dependencies optimized for parallel agent execution. Do NOT implement - only create the plan. The text after `/skill:swarm-planner` is the feature or product to plan, not a request to build it: the user runs the plan later with /skill:parallel-task.

## Core Principles

1. **Explore Codebase**: Investigate architecture, patterns, existing implementations, dependencies, and frameworks in use.
2. **Fresh Documentation First**: Fetch current docs for ANY external library, framework, or API before planning tasks.
3. **Ask Questions**: Clarify ambiguities and seek clarification on scope, constraints, or priorities throughout the planning process. At any time.
4. **Explicit Dependencies**: Every task declares what it depends on, enabling maximum parallelization.
5. **Atomic Tasks**: Each task is independently executable by a single agent.
6. **Review Before Yield**: A subagent reviews the plan for gaps before finalizing.

## Process

### 1. Research

Codebase investigation (`read_file`, `glob`, `search_text`, `git_log`):
- Architecture, patterns, existing implementations
- Dependencies and frameworks in use

If the architecture is unclear or missing, STOP AND YIELD to the user and request input with `ask_user` before moving on. Always offer recommendations. If architecture is present, move on.

### 2. Documentation (REQUIRED for external dependencies)

Fetch current docs for any libraries, frameworks or APIs that are or will be used in the project: the Context7 MCP server when it is connected (`/mcp`), otherwise web search (`web_search`, or the `websearch` skill). This ensures version-accurate APIs, correct parameters, and current best practices.

### 3. STOP and Request User Input

When anything is unclear or could reasonably be done multiple ways:
- Stop and ask clarifying questions immediately with `ask_user`
- Do not make assumptions about scope, constraints, or priorities
- Questions should reduce risk and eliminate ambiguity
- Always offer recommendations for clarification questions

### 4. Create Dependency-Aware Plan

Each task MUST include:
- **id**: Unique identifier (e.g., `T1`, `T2.1`)
- **depends_on**: Array of task IDs that must complete first (empty `[]` for root tasks)
- **description**: What the task accomplishes
- **location**: File paths involved
- **validation**: Acceptance criteria

```
T1: [depends_on: []] Create database schema migration
T2: [depends_on: []] Install required packages
T3: [depends_on: [T1]] Create repository layer
T4: [depends_on: [T1]] Create service interfaces
T5: [depends_on: [T3, T4]] Implement business logic
T6: [depends_on: [T2, T5]] Add API endpoints
T7: [depends_on: [T6]] Write integration tests
```

Tasks with empty/satisfied dependencies can run in parallel (T1, T2 above).

Write the plan with the template in [references/plan-template.md](references/plan-template.md) (read it with `read_skill_file`).

### 5. Save Plan

Save to `<topic>-plan.md` in the working directory with `write_file`.

### 6. Subagent Review

After saving, start one reviewer child from the `ipython` tool and wait for its answer:

```python
await rlm.spawn("""Review this implementation plan for:
1. Missing dependencies between tasks
2. Ordering issues that would cause failures
3. Missing error handling or edge cases
4. Gaps, holes, gotchas.

Provide specific, actionable feedback. Do not ask questions.

Plan location: [file path]
Context: [brief context about the task]""", name="plan-reviewer")
review = await rlm.collect(["plan-reviewer"], timeout_ms=600000)
```

If the reviewer provides actionable feedback, revise the plan before yielding.

### 7. Yield

Stop here: tell the user where the plan is saved and that `/skill:parallel-task <plan file>` executes it. Do not write or edit any file other than the plan.

## Important

- Every task must have an explicit `depends_on` field
- Root tasks (no dependencies) can be executed in parallel immediately
- Do NOT implement - only create the plan
- Always fetch current docs for external dependencies before finalizing tasks
- Always ask questions where ambiguity exists
