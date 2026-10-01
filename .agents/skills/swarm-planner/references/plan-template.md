# Plan Template

Save the plan as `<topic>-plan.md`. The `status`, `log` and `files edited/created` fields are filled in by the executing subagents.

````markdown
# Plan: [Task Name]

**Generated**: [Date]

## Overview
[Summary of task and approach]

## Prerequisites
- [Tools, libraries, access needed]

## Dependency Graph

```
[Visual representation of task dependencies]
T1 ──┬── T3 ──┐
     │        ├── T5 ── T6 ── T7
T2 ──┴── T4 ──┘
```

## Tasks

### T1: [Name]
- **depends_on**: []
- **location**: [file paths]
- **description**: [what to do]
- **validation**: [how to verify]
- **status**: Not Completed
- **log**: [leave empty, to be filled out later]
- **files edited/created**: [leave empty, to be filled out later]

### T2: [Name]
- **depends_on**: []
- **location**: [file paths]
- **description**: [what to do]
- **validation**: [how to verify]
- **status**: Not Completed
- **log**: [leave empty, to be filled out later]
- **files edited/created**: [leave empty, to be filled out later]

### T3: [Name]
- **depends_on**: [T1]
- **location**: [file paths]
- **description**: [what to do]
- **validation**: [how to verify]
- **status**: Not Completed
- **log**: [leave empty, to be filled out later]
- **files edited/created**: [leave empty, to be filled out later]

[... continue for all tasks ...]

## Parallel Execution Groups

| Wave | Tasks | Can Start When |
|------|-------|----------------|
| 1 | T1, T2 | Immediately |
| 2 | T3, T4 | Wave 1 complete |
| 3 | T5 | T3, T4 complete |
| ... | ... | ... |

## Testing Strategy
- [How to test]
- [What to verify]

## Risks & Mitigations
- [What could go wrong + how to handle]
````
