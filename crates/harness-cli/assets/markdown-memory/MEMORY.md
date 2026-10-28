# MEMORY.md - Routing Index

> Pointers only, never facts. Hard cap ~15,000 chars. Detail lives in `memory/`.
> Update this index in the same commit as any detail-file change.

---

## Active Context

Task-scoped pointers, not mandatory preloads (max 2-3):

- (none yet)

---

## People

| Name | Role | Detail file | Trigger keywords |
|------|------|-------------|------------------|

---

## Projects

| Project | Detail file | Trigger keywords |
|---------|-------------|------------------|

---

## Current Decisions

Behavior-changing defaults only; full dated records in `memory/decisions/`.

- (none yet)

---

## Active Preferences

- (Add stable operator preferences here as they are stated, with dates.)

---

## Recall Rules

1. Retrieve the matching `memory/people/` file when a person's context matters to the task, not for a passing mention.
2. Retrieve the matching `memory/projects/` file when project history changes the answer.
3. Making a decision? Search `memory/decisions/` and recent daily notes for precedent.
4. Markdown is canonical; any search index is a locator, not an answer.
5. Bounded evidence: at most 5 sources per recall; label unresolved current-state claims stale or unknown.

---

## Daily Logs

Raw logs in `memory/YYYY-MM-DD.md`. Retrieve dated excerpts when recent history affects the task; no unconditional preload.

---

*Last updated: YYYY-MM-DD. Keep this a routing index, not a changelog.*
