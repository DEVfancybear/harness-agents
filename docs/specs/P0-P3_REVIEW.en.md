# P0–P3 source review and repair specification

English | [Tiếng Việt](P0-P3_REVIEW.vi.md)

Date: 2026-09-11. Scope: review the existing P0–P3 implementation against its
specifications and runbooks; repair demonstrated defects without starting P4.
Spec approval: not obtained (autonomous run under the user's review/update request).

## Acceptance and failure model

1. P0 IDs reject non-RFC UUID variants, matching the existing generated schemas.
2. P2 context admission budgets rendered block labels and separators as well as
   text. Mandatory overflow fails explicitly; optional blocks cannot push the
   rendered packet estimate beyond the available budget.
3. P3 provider arguments reject unknown fields and invalid optional path/isolation
   types instead of silently broadening scope or downgrading isolation.
4. P3 path policy compares normalized components, including Windows case, and
   denies recursive reads of ancestors containing denied paths. Explicit `.`
   and `./` resolve to the workspace root for directory operations.
5. Bounded P3 reads reject malformed UTF-8 inside the retained prefix; only an
   incomplete trailing character caused by truncation may be removed.
6. P3 search redacts the complete source line before shortening its preview.

Each defect gets a regression test observed failing before its fix. Tests use
real disposable files/SQLite and existing public services; no live provider,
credentials, host activation, new dependency or migration is needed. Preserve
schema versions, valid inputs, P0–P3 required tests and unrelated work. No commit,
push or publication is included.

## Verification

Run targeted RED/GREEN tests, then `scripts/Verify-Phase.ps1 -Phase P3 -Json`
and `scripts/Verify-Docs.ps1 -SelfTest`. Record exact results and limitations in
the paired review evidence. Existing phase evidence is historical, not evidence
for the modified tree. Coverage, independent review, unrun platforms and any
additional review findings must be reported honestly.

## Revision 2 — P1 call tracking

Reject duplicate active service call IDs before incrementing the in-flight
counter. Otherwise settling either duplicate removes the other from the
uncertainty set. IDs may be reused after settlement; provider loss still
drains accepted calls and joins the resource. Test the real service lease.
