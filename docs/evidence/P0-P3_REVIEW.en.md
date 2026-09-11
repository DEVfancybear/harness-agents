# P0–P3 review evidence

English | [Tiếng Việt](P0-P3_REVIEW.vi.md)

Date: 2026-09-11. Contract: [review SPEC](../specs/P0-P3_REVIEW.en.md).
Spec approval: not obtained (autonomous run). Independent verification: not performed.

## Review scope and repairs

Read P0–P3 specifications and implementation runbooks; inspected shared ID/hash
contracts, kernel call/resource lifecycle, SQLite transaction/fencing/approval
paths, session recovery/context, runtime/provider flow, tool policy/parser/files/
processes and the CLI acceptance fixtures. This is a targeted source review,
not a claim that every source line or every interleaving has been verified.

| Finding | Repair and regression |
|---|---|
| P0 UUID parser accepted non-RFC variants that its schema rejects. | Check RFC variant; `review_p0_ids_reject_non_rfc_variants`. |
| P1 duplicate active call IDs share one uncertainty-set entry, so finishing one hides another. | Reject duplicates before changing counters; allow reuse after settlement; `review_p1_duplicate_active_call_ids_are_rejected`. |
| P2 admission counted only block bodies, allowing rendered context to exceed its own estimate budget. | Budget exact rendered labels/separators; reject mandatory overflow and omit oversized optional blocks; `review_p2_context_budget_includes_rendered_headers`. |
| P3 malformed optional paths became whole-workspace requests; malformed isolation became best effort; unknown fields were ignored. | Validate provided field names/types before constructing actions; `review_p3_provider_parser_rejects_invalid_optional_fields`. |
| P3 lexical path policy could be bypassed with `./`, repeated separators, Windows case, or a recursive ancestor read. | Compare normalized components and deny overlapping recursive scopes; `review_p3_policy_cannot_be_bypassed_by_path_spelling_or_ancestor`. |
| P3 explicit `.` directory incorrectly failed the parent containment check. | Accept the validated root for directory operations; `review_p3_explicit_workspace_root_can_be_listed`. |
| P3 bounded reader removed bytes until invalid UTF-8 disappeared, masking internal corruption and potentially doing quadratic work. | Trim only an incomplete trailing code point; unit `review_bounded_reader_rejects_invalid_utf8_inside_prefix`, plus valid Unicode boundary control. Existing execution preconditions already reject a stable malformed file; the direct-reader test exposes the defect. |
| P3 search truncated before redaction, hiding a sensitive marker after the preview limit. | Redact the source line first; `review_p3_search_redacts_before_truncation`. |

Ten new tests cover these repairs and existing defensive behavior. Eight new
behavioral tests were observed RED before their repairs. The execution-level
invalid-UTF-8 test and valid Unicode truncation control already passed; they are
regressions for pre-existing behavior, not evidence of previously failing behavior.

## Verification status

The final post-code-edit entry point completed with `P0_P3_REVIEW_OK`:

```powershell
pwsh -NoProfile -File scripts/Verify-P0P3Review.ps1
```

| Layer | Actual result |
|---|---|
| Formatting / clippy with warnings denied | PASS |
| Workspace tests | PASS, including all 10 new review tests |
| Exact predecessor/phase regressions | P0 8/8, P1 18/18, P2 15/15, P3 15/15 required |
| P3 discovery | 20 discovered; 15 required; all exercised by workspace tests |
| Manual mutations | 3/3 killed: UUID variant, mandatory overflow, redaction order; original source restored |
| Mutation scratch-directory guard | Non-temporary workspace rejected with exit 1 before edits |
| Documentation self-test | PASS; 12 negative controls, 61 Markdown files, 15 required language pairs |
| Real CLI smoke | PASS: nine tool schemas, Windows Job Object cleanup, strict isolation false |
| Source-copy comparison | PASS: every review-owned file matched the tested copy before final evidence metadata edits |

Verification used Windows, rustc/cargo 1.97.1 and PowerShell 7.6.5. Concurrent P4
work appeared after the initially clean checkout. The entry point exports
baseline `c9bb106cce67c5f0b7e3c02fafe1484e5f69d379` and overlays only the review-owned
files in a temporary source copy. It does not claim integration testing of the
in-progress P4 changes and leaves them untouched.

The tested copy's gate digest was
`sha256:b2245719b7c2b0333e3fa3a958dc72c2a69bca5503ef11b83e8802c117494f09`
(129 files). That digest includes draft review evidence; this final metadata
update changes it, not the tested Rust source. The ten modified Rust/test files
have aggregate SHA-256
`27e974e726815d29b6f00b113572167eba1cd2befb060895999a8a2244adf0ac`:
sort their repo-relative paths, join `path:lowercase-file-sha256` with LF and
hash those UTF-8 bytes, with no trailing LF. The owned paths are enumerated in
the entry-point script. The final evidence documents receive another structural
docs self-test after this metadata update.

## Remaining source-review findings

These are unresolved findings, not passing claims. They need a dedicated follow-up
with failure fixtures; this repair set does not establish full P0–P3 conformance.

1. **High — approval identity:** `ToolExecutionService::approve_with_expiry` and
   `approval_mismatch` bind actor/action/workspace/revisions but omit invocation,
   session and task IDs. A grant can therefore match another otherwise-identical
   prepared request before consumption. Extend durable approval identity and
   test cross-invocation/cross-task rejection, including old-record handling.
2. **High — streamed tool calls:** `parse_sse_payload` reads only the first tool
   delta and falls back to `tool-call` when an ID is omitted. It does not retain
   the provider's per-index identity across frames. Add multi-call/chunk fixtures
   before relying on this adapter for real streamed tool execution.
3. **High — stale compaction:** `RuntimeService::compact` samples its CAS sequence
   after building the candidate rather than using the candidate's source sequence.
   A concurrent appended event can be accepted without rebuilding the candidate.
   Tie CAS to the source snapshot and implement the documented bounded rebase.
4. **Medium — queued process cancellation:** the process runner acquires its
   host-wide mutex before checking cancellation, then spawns before selecting
   cancellation. A canceled queued operation can still briefly start a process.
   Make admission cancellation-aware and add a marker-file negative test.

The runbooks/handbook still say `not started`; they were retained as historical
planning documents, consistent with their catalog role. Consult phase evidence
for historical implementation status and this review for current defects.

## Limits

Context budgeting still uses the existing byte-based token estimate; this is
not a tokenizer guarantee for the complete provider request/tool schemas.
Path policy is not filesystem/network sandboxing. The redactor is a keyword
heuristic, not arbitrary secret detection. External filesystem races, real
provider behavior and all kernel cancellation interleavings are not proved.
No new dependencies, migrations, live credentials, deployment, commit or push.
No coverage tool or independent agent review was run; changed-line coverage is
unmeasured. Linux/remote CI for this source state is not verified locally.
