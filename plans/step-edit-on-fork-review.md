# Review: `step-edit-on-fork` Plan

**Reviewer:** Agent  
**Date:** 2026-04-29  
**Plan file:** `.cursor/plans/step-edit-on-fork_bc5de8c3.plan.md`  
**Status:** All 10 original findings resolved. Plan is ready for implementation.

---

## Original Findings — Resolution Status

### 1. (High) Cascade-delete on edit is destructive with no undo path — RESOLVED

**Original:** Fork edits silently cascade-deleted downstream steps with only a toast as mitigation.

**Resolution:** Added always-confirm Save modal (T8) with dry-run cascade count endpoint (T2c). Every save now shows the cascade count up front — fork edits, main edits with auto-fork, and main edits with env-bypass all go through the confirm dialog. The modal defaults to Cancel. T10 now has 6 test cases covering the cascade toast.

### 2. (Medium) `fork-and-edit-step` step_number sequencing unspecified — RESOLVED

**Original:** Plan assumed `step_number` would match the original but didn't spec how.

**Resolution:** T4 now explicitly says "step_number explicitly set to at_step, bypassing the timeline's auto-increment counter." T5 tests assert `step_number == at_step (not at_step+1 from auto-increment)`.

### 3. (Medium) `request_hash` normalization not specified for textarea path — RESOLVED

**Original:** Raw textarea text could produce different hashes than the original recording.

**Resolution:** New decision: "Canonical JSON on save (review #3)". T6 specifies `JSON.stringify(JSON.parse(textareaText))` before sending. T3 and T5 both include canonical-form roundtrip tests.

### 4. (Medium) No optimistic locking / conflict detection — RESOLVED (documented)

**Original:** Two concurrent editors could silently clobber each other.

**Resolution:** Added to "Risks & non-goals" with explicit rationale: "Acceptable for v1 (single-operator dev tool); revisit with If-Match/version columns if multi-operator usage emerges." T3 now includes a last-write-wins behavioral test.

### 5. (Medium) `REWIND_ALLOW_MAIN_EDITS` not exposed to frontend — RESOLVED

**Original:** Frontend had no way to know the server's env var setting.

**Resolution:** T2b extends `GET /api/health` with `allow_main_edits: bool`. Added to surfaces touched table. T6 reads it from the existing health query. T10 tests the branch.

### 6. (Low) No size guard on PATCH body — RESOLVED

**Original:** Two blobs could exceed the 10MB router limit.

**Resolution:** T2 now specifies "413 if either blob exceeds 5 MB (half of the existing 10 MB body limit)." T3 includes a body-too-large test.

### 7. `StoreEvent` variant not listed — RESOLVED

**Original:** `StoreEvent::StepUpdated` didn't exist and no file was listed.

**Resolution:** T1b explicitly adds the variant in `crates/rewind-web/src/lib.rs`. Surfaces touched table now includes "Store events" row with the full variant shape: `{ session_id, step_id, timeline_id, deleted_count }`.

### 8. No migration note for the store — RESOLVED

**Original:** Plan didn't confirm whether schema changes were needed.

**Resolution:** T1 and the surfaces touched table now state "No schema migration — all operate on existing columns (`request_blob`, `response_blob`, `request_hash`, `parent_timeline_id`)."

### 9. (Medium) No test for concurrent cascade + replay race — RESOLVED (documented)

**Original:** A replay writing steps while an edit cascade-deletes them could fail confusingly.

**Resolution:** Added as explicit risk (review #9): "v1 documents this; v2 should refuse PATCH while a replay job for the same timeline is in running state." T11 docs section covers this as a known limitation.

### 10. (Low) Frontend tests don't cover cascade toast — RESOLVED

**Original:** T10 had 5 test cases; cascade toast was missing.

**Resolution:** T10 now lists 6 scenarios. The 6th: "Cascade toast appears when mutation response has `deleted_downstream_count > 0`; absent when count is 0."

---

## Nits — Resolution Status

| Nit | Status |
|-----|--------|
| T12 hardcoded version `0.14.3 -> 0.14.4` | **Fixed.** Now says "read current Cargo.toml at implementation time; do not hardcode." |
| T13 "mirror binary to artifactory" undocumented | **Clarified.** Now says "dev1 deployment requirement, not in CLAUDE.md but documented in DEPLOYMENT-GAPS.md." |
| PATCH path not explicitly wrapped in a transaction | **Fixed.** T2 now says "single-tx (BEGIN; update_step_blobs; delete_steps_after; COMMIT)." |

---

## Remaining Minor Observations (non-blocking)

1. **Test count in acceptance criteria grew from 5+4 to 8+6** — good. The new total (≥14 new tests) is proportionate to the feature surface.

2. **The dry-run endpoint (T2c) adds a network round-trip before every save.** For a local dev tool with SQLite this is negligible, but if latency becomes noticeable on remote servers, the dry-run could be folded into the PATCH response via a `dry_run=true` query param instead of a separate GET.

3. **T8 always-confirm modal default-focuses Cancel** — good UX safety choice for a destructive action.

---

## Verdict

The updated plan addresses all findings. The main improvements:

- **Always-confirm modal with cascade count** eliminates the silent-destruction risk.
- **Canonical JSON normalization** is now specified end-to-end (frontend send → server hash → replay lookup).
- **Health endpoint extension** gives the frontend the `allow_main_edits` flag without a new endpoint.
- **Known limitations are documented** (no optimistic locking, no mid-replay coordination) with explicit v2 upgrade paths.

**Recommendation:** Proceed with implementation.
