# Line-by-Line Commits (Sub-Hunk Splitting)

> **Status:** Design in progress. This doc captures decisions reached during the design interview. Sections will be added as remaining questions are resolved.

## Motivation

Today, the smallest unit of stack assignment in GitButler is a **hunk**: a contiguous block of `+`/`-` lines as produced by `git diff`. A user who wants the top half of a hunk on branch A and the bottom half on branch B has no way to express that — they must either:

- Edit the file to split the change physically (wasteful, error-prone), or
- Commit the whole hunk to one branch and rewrite history afterwards.

This proposal lets the user **split any hunk into sub-hunks** along arbitrary row boundaries, then drag each piece to its target stack independently. The result is rendered as multiple normal-looking hunks in the diff view; everything downstream (drag-and-drop, hunk locks, dependency analysis, commit creation) keeps working unchanged because each sub-hunk is a real `HunkHeader` once split.

## User-Facing Behavior

### The gesture

- **Single-line click** on a delta line: toggles staging for that one line. Unchanged from today.
- **Click and drag** across multiple lines in a hunk: visually highlights the range and, on mouseup, opens an **inline popover** anchored to the selection. The popover contains:

  | Action | Effect |
  |---|---|
  | **Stage** / **Unstage** *(default-focused)* | Toggles staged state for every delta line in the selection. Label flips based on whether any line is currently unstaged. |
  | **Comment** | Opens the existing annotation editor for the selected range. |
  | **Split** | Splits the containing hunk into 2 or 3 sub-hunks at the selection's boundaries. |
  | **Cancel** | Dismisses the popover. Same as click-outside or Esc. |

  Pressing **Enter** triggers Stage/Unstage. Pressing **Esc** or clicking outside dismisses the popover.

- The drag itself **does not toggle staging mid-drag** (deliberate change from current behavior; today's drag both stages and opens annotations as a side-effect of the same motion). The popover becomes the single decision point for what the drag means.

### What "Split" produces

A split is **always a 3-way operation** conceptually: the selected range becomes the middle sub-hunk, with leading and trailing residual sub-hunks on either side. When the selection touches a hunk edge, the corresponding residual is empty and is not materialized — yielding a natural 2-way split at the edges.

Examples (selection in **bold**):

```
Hunk has rows 0–7.

Selection 0–2  →  [0–2] | [3–7]                   (2-way, leading edge)
Selection 5–7  →  [0–4] | [5–7]                   (2-way, trailing edge)
Selection 3–5  →  [0–2] | [3–5] | [6–7]           (3-way)
```

### Rendering

Each resulting sub-hunk renders **identically to a natural git hunk**: its own `@@ -x,y +a,b @@` header, its own gutter line numbers, its own outline, separated by the standard hunk gap. From the rest of the system's point of view, sub-hunks *are* hunks.

The only visual difference is a **small icon in the sub-hunk's header bar** indicating "this hunk was split from a larger one." The icon is the affordance for **un-split**: clicking it dissolves the sub-hunks back into the natural hunk. The icon's tooltip explains the synthesized header range and offers the un-split action.

Natural (unsplit) hunks have no such icon.

### Un-split

Clicking the split icon on any sub-hunk merges all sibling sub-hunks back into the original natural hunk:

- Stack assignments made *on individual sub-hunks* (e.g. dragging one piece to a different stack) are dropped. The merged hunk reverts to the assignment of the original anchor (the parent hunk's pre-split assignment).
- If un-split would lose a stack reassignment, a confirmation prompt appears:
  > "Un-split will discard your reassignment of these lines to *Stack X*. Continue?"
- If no reassignment was made, un-split happens without prompt.
- Stage state changes made on the sub-hunks during the split are also dropped; the natural hunk's pre-split stage state reactivates (see *Stage state* below).

### Validation rules for Split

The Split button is disabled (or the Split action no-ops with a tooltip) in these cases:

1. **Selection consists only of context rows** (no `+`/`-`). Splitting context-only lines changes nothing.
2. **Selection straddles two hunks.** Already prevented at the gesture layer (`LineSelection` is per-hunk-component); listed here for completeness.
3. **Selection is the entire hunk.** Nothing to split out.

These cases are **silently handled, not rejected**:

4. **Leading or trailing rows of the selection are context.** Context rows are trimmed from the selection's boundaries before computing the sub-hunk's `HunkHeader`. Context belongs to whichever neighbor needs it.

These cases are **allowed through** even though they may produce dependent sub-hunks:

5. **Selection bisects a mixed `-`/`+` block** (e.g. removed lines on one side of the split, added lines on the other). The resulting sub-hunks may not apply independently against an arbitrary base. They surface as normal hunk-dependency locks via `but-hunk-dependency`, which is GitButler's existing mechanism for inter-change dependencies. Same lazygit-style "you know what you're doing" stance.

## Data Model

### Persistence

Sub-hunk splits are **stored in process memory only**, on the Rust side, in the workspace/project handle alongside existing assignment state. They are **not** written to the SQLite `HunkAssignment` table.

Lifetime:

- **Empty on app launch.** Splits do not survive a relaunch.
- **Survives navigation, branch operations, refreshes.** Anything short of relaunch or a structural file edit.
- **Auto-invalidated by file edits** that change the underlying hunk shape — see *Reconciliation* below.

### Override store shape

Each split is represented as a `SubHunkOverride`:

```text
SubHunkOverride {
  path:               BString,
  anchor:             HunkHeader,     // the natural hunk we're splitting
  ranges:             Vec<RowRange>,  // sorted, disjoint sub-ranges within the anchor
  assignments:        Map<RowRange, HunkAssignmentTarget>,
                                      // per-range stack assignment, if reassigned
}
```

The store is keyed by `(path, anchor)`. Each range corresponds to one resulting sub-hunk; rows in the anchor not covered by any range are residual sub-hunks (which inherit the anchor's pre-split assignment).

### Reconciliation

`reconcile_assignments` runs after every worktree refresh. Today it takes natural hunks from `git diff` plus existing `HunkAssignment` rows and produces updated assignments. The new pass:

1. Compute natural hunks from `git diff` as today.
2. For each `SubHunkOverride` in the in-memory store:
   - Look up a natural hunk on the same path whose `HunkHeader` matches `anchor` exactly.
   - If found, **synthesize sub-hunks** from the override's ranges and emit a `HunkAssignment` per sub-hunk, applying the per-range targets where set, falling back to the anchor's own assignment for residuals.
   - If not found (the diff has shifted, file was edited, anchor no longer exists), **drop the override silently**. The natural hunks render as-is.
3. For natural hunks with no override, emit assignments as today.

This makes the auto-invalidate-on-edit behavior fall out of the model for free: there is no special "detect a code edit" code path. An edit that reshapes the hunk simply causes anchor lookup to fail, and the override is discarded.

### Stage state across split / unsplit

Stage state lives in the frontend Redux slice (`apps/desktop/src/lib/selection/uncommitted.ts`), keyed by `compositeKey({ stackId, path, hunkHeader })` with a `LineId[]` payload (empty array = whole-hunk-staged sentinel).

Behavior:

- **On split:** the parent hunk's stage entry is **left dormant**, not deleted. It points at a `HunkHeader` that doesn't currently render, so it has no UI effect. New sub-hunks have no entries → render as unstaged.
- **On unsplit:** the override is removed; the natural hunk's `HunkHeader` reappears in the rendered set; the dormant parent entry reactivates → original stage state is restored.
- **On file-edit invalidation:** same as unsplit — dormant parent entry reactivates.
- **Sub-hunk staging done during a split is lost** when the split is dissolved (by unsplit or invalidation). This is consistent with "unsplit means revert to pre-split state."

This is the v1 implementation (Option A in the design discussion). A future Option B could migrate stage state per-line across split boundaries (so changes made on sub-hunks survive unsplit), but is purely additive and not required for v1.

## Implementation Notes

### Frontend

- **Gesture:** `packages/ui/src/lib/components/hunkDiff/lineSelection.svelte.ts` already supports drag-to-select with `onDragEnd`. The per-row staging side-effect (`onLineClick` firing during `onMoveOver`) needs to be removed; staging now happens via the popover's Stage action only.
- **Popover:** reuse `packages/ui/src/lib/components/ContextMenu.svelte`. Already used for the existing right-click hunk menu (`UnifiedDiffView.svelte:420`); provides anchored positioning, click-outside dismissal, and Esc handling out of the box.
- **Drag-to-stack of sub-hunks:** unchanged. Sub-hunks materialize as ordinary `HunkAssignment`s with synthesized `HunkHeader`s, so the existing `HunkDropDataV3` / `AssignmentDropHandler` flow handles them with no changes.

### Backend

- **New override store:** in-memory `Map<(path, HunkHeader), SubHunkOverride>` on the workspace handle in `but-hunk-assignment`. No SQLite schema change.
- **Reconcile extension:** add a post-pass to `reconcile_assignments` that applies overrides to natural hunks (see *Reconciliation* above).
- **New RPCs** — a dedicated, symmetric pair, kept separate from `assignHunk`:

  ```rust
  pub async fn split_hunk(
      project_id: ProjectId,
      path: BString,
      anchor: HunkHeader,
      ranges: Vec<RowRange>,
  ) -> Result<()>;

  pub async fn unsplit_hunk(
      project_id: ProjectId,
      path: BString,
      anchor: HunkHeader,
  ) -> Result<()>;
  ```

  Both insert/remove a single `SubHunkOverride` and trigger reconcile. `ranges` are validated server-side (sorted, disjoint, within `anchor`'s row count, not all-context) — redundantly with the frontend validation. Reassigning a sub-hunk to a different stack after splitting goes through the existing `assignHunk` unchanged, since the split has already materialized the sub-hunks as real `HunkAssignment`s.
- **Sub-hunk header synthesis:** computing each sub-hunk's `(old_start, old_lines, new_start, new_lines)` from the anchor + row range. Context lines at the boundaries are assigned to the neighbor that owns them per validation rule 4.

### Existing systems that "just work" because sub-hunks are real hunks

- `HunkDropDataV3` carries one `HunkHeader`. Sub-hunks have synthesized headers; drag and drop already works.
- `AssignmentDropHandler` calls `assignHunk` with a `HunkHeader`. No change needed.
- `but-hunk-dependency` operates on `(old_start, old_lines)` numerically (`ranges/mod.rs:151` `intersection`) and on the hunk's diff text for adjacency checks. Synthesized sub-hunk headers carry valid (narrower) numeric ranges and a corresponding slice of the anchor's diff text, so dependency analysis works identically to natural hunks.
- `DiffSpec` materialization at commit time uses the **anchor-paired sub-hunk encoding** that `create_commit` already supports (see `crates/but-workspace/src/commit_engine/mod.rs:90-118`). For a sub-hunk, one side of each emitted `HunkHeader` carries the anchor's full range (the "fixed anchor"); the other side carries the sub-range. Pure-add or pure-remove sub-hunks emit one header; mixed-direction sub-hunks emit two (one per side). See *Sub-hunk encoding* below.
- The diff view (`HunkDiff.svelte`) renders one hunk per `HunkHeader`. After split, multiple `HunkHeader`s → multiple rendered hunks.

## Sub-Hunk Encoding (Commit Time)

`create_commit` already accepts sub-hunk specifications via an anchor-pairing convention: one side of the `HunkHeader` carries the natural hunk's full range (the unchanging anchor), the other side carries the sub-range to commit. The override store does **not** persist these encoded headers; it stores row ranges (`(start_row, end_row)` within the anchor) as the canonical user intent, and translates to the encoded form at commit time via a dedicated function:

```rust
pub fn encode_sub_hunk_for_commit(
    anchor: HunkHeader,
    range: RowRange,
    rows: &[DiffRow],   // from the parsed unified patch
) -> Vec<HunkHeader>;   // 1 header for pure-add/remove, 2 for mixed
```

Encoding rules:

- **Pure-add sub-hunk** (selection contains only `+` rows): one `HunkHeader` whose `old` side is the anchor's full old range and whose `new` side is the sub-range.
- **Pure-remove sub-hunk** (only `-` rows): mirror image — `new` side is the anchor's full new range, `old` side is the sub-range.
- **Mixed sub-hunk** (both `+` and `-`): emit *both* of the above headers in the same `DiffSpec.hunk_headers` (the engine OR's them).

This function is unit-testable in isolation against a table of `(anchor, range, expected headers)` cases. The existing `From<HunkAssignment> for DiffSpec` impl in `crates/but-hunk-assignment/src/lib.rs:131` becomes a thin caller — it delegates to `encode_sub_hunk_for_commit` when the assignment is override-derived, and emits the existing single-header form otherwise.

Storing the canonical row range (not the encoded headers) means:

- **Reconcile uses rows directly** to partition the anchor into sub-hunks.
- **The diff view uses rows directly** to render each sub-hunk.
- **Commit-time encoding is a pure function** of `(anchor, range, rows)`, recomputed on demand. No cache-invalidation concerns.

## v2 Follow-Ups (Out of Scope for v1)

- **Right-click "Split hunk before this line"** in `HunkContextMenu.svelte`. Single-click 2-way split at the boundary above the right-clicked line; calls `split_hunk(path, anchor, ranges = [(boundary, anchor.row_count)])`. Provides keyboard + accessibility access to splitting.
- **Right-click "Commit this line"** — composite shortcut that splits the line into a 1-row sub-hunk and opens the commit composer scoped to just that sub-hunk's `DiffSpec`. Resolves into the existing commit flow. Requires additional design for stack-target selection and message-composer surfacing.
- **Stage state migration across split** (Option B from Q10) — partition the parent hunk's `LineId[]` per sub-hunk on split; merge back on unsplit. Purely additive on top of v1's dormant-entry behavior.
- **`but` CLI surface** for `split` / `unsplit`. Requires either promoting the override store to persistence or adding a desktop↔CLI IPC channel.

## Test Scope

The feature touches several layers; each needs targeted coverage.

**`encode_sub_hunk_for_commit` (Rust unit tests).** Table-driven against `(anchor, range, rows) → Vec<HunkHeader>` with cases:

- Pure-add at hunk start, middle, end.
- Pure-remove at hunk start, middle, end.
- Mixed `-`/`+` block, range bisecting it (verifies two-header emission).
- Range with leading/trailing context rows (verifies trimming).
- Single-row range.
- Range spanning the entire hunk minus one row at each end.

**Reconcile pass with overrides (Rust integration tests in `but-hunk-assignment`).** Cases:

- Override anchor matches a current natural hunk → sub-hunks materialize correctly with residuals inheriting anchor's assignment.
- Anchor doesn't match (file edited) → override silently dropped, natural hunks render.
- Override with all rows covered by ranges → no residual.
- Multiple overrides on different anchors in the same file.
- Override on a file that was deleted from worktree.

**Stage state dormancy (TS unit tests in `uncommitted.ts`).** Cases:

- Stage 5 of 8 lines, split, unsplit → original 5 lines staged.
- Stage 5 of 8 lines, split, edit file (anchor invalidates), reconcile → dormant entry reactivates on natural hunk.
- Stage on sub-hunk during split, unsplit → sub-hunk staging is dropped, parent's pre-split stage state reactivates.

**End-to-end (Playwright).** A single spec covering the happy path:

- Open a file with a multi-row hunk in worktree.
- Drag-select 3 rows in the middle.
- Verify popover appears with Stage / Comment / Split / Cancel.
- Click Split. Verify the hunk now renders as 3 sub-hunks with the split icon on each.
- Drag the middle sub-hunk to a different stack. Verify it appears under that stack.
- Click the split icon on any sub-hunk. Verify confirmation prompt (because reassignment was made). Confirm.
- Verify hunk is back to one natural hunk; assignment is original.

**Hunk-dependency interaction.** Existing dependency tests in `but-hunk-dependency/src/ranges/tests/` already cover natural hunks; add a test that constructs a sub-hunk via the override path and verifies dependency analysis treats it identically.

## Implementation Phasing

Suggested order, smallest-scope-first so each phase is independently shippable behind a feature flag:

1. **Backend foundation.** `SubHunkOverride` type, in-memory store on the workspace handle, `split_hunk` / `unsplit_hunk` RPCs, reconcile post-pass. Validate via Rust integration tests; no UI yet.
2. **Commit-time encoding.** `encode_sub_hunk_for_commit` plus the `From<HunkAssignment> for DiffSpec` delegation. Verify against a hand-constructed override + `create_commit` end-to-end test.
3. **Diff-view rendering of sub-hunks.** Plumb the override-derived `HunkAssignment`s into the existing `HunkDiff` render path. Verify by manually invoking `split_hunk` via the dev console; visually confirm two/three sub-hunks render correctly.
4. **Sub-hunk header icon + un-split flow.** Adds the visual marker, the un-split confirmation prompt, and wires the icon click to `unsplit_hunk`.
5. **Gesture rewrite + popover.** Remove per-row staging during drag, add the `ContextMenu` popover with Stage / Comment / Split / Cancel, wire Split action to call `split_hunk`. This is the user-visible "feature on" moment.
6. **Polish.** Edge-case handling (validation rule messages, reassignment confirmation copy, accessibility labels on the popover items).

## Open Questions
- CLI parity: does `but` need to expose split/unsplit?

These will be answered in subsequent rounds of the design interview.

## Decision Log

| # | Question | Decision |
|---|---|---|
| 1 | Visual model of partial-hunk assignment | Split into separate visible hunks, each with its own `@@` header. |
| 2 | Where can the user place a split? | Any row boundary. Mixed-direction splits surface as hunk-dependency locks via existing system. |
| 3 | Persistence | In-memory only, Rust-side. Dropped on relaunch and on anchor-header mismatch (file edit). |
| 4 | Gesture | Drag-select range → `ContextMenu` popover with **Stage** / **Comment** / **Split** / **Cancel**. Drag no longer toggles staging mid-motion. |
| 5 | Split shape | Always 3-way conceptually; collapses to 2-way at edges. Reject context-only / whole-hunk / cross-hunk; trim context at boundaries; allow mixed-direction. |
| 6 | Modal entry-point disambiguation | No modifier. Multi-row drag always opens the popover; single-row click stages directly. |
| 7 | Modal contents | Single Stage/Unstage toggle (default-focused) + Comment + Split + Cancel. Anchored popover, reusing existing `ContextMenu` component. |
| 8 | Visual marker on sub-hunks | Subtle icon in the hunk header indicating "split sub-hunk." Hosts the un-split affordance. |
| 9 | Un-split support | Yes. Click icon → dissolve sub-hunks back to natural hunk. Confirmation prompt only when reassignment to another stack would be lost. |
| 10 | Stage state migration across split | Option A: don't migrate. Parent's stage entry left dormant; reactivates on unsplit/invalidate. Sub-hunk staging changes lost on unsplit. |
| 11 | Backend API surface | Dedicated `split_hunk` / `unsplit_hunk` RPC pair. Reassignment of resulting sub-hunks reuses the existing `assignHunk` unchanged. |
| 12 | Sub-hunk encoding for commit | Encoding lives in a dedicated `encode_sub_hunk_for_commit` function (testable in isolation). Override store persists canonical row ranges, not encoded headers; translation to anchor-paired `HunkHeader`s happens at commit time. |
| 13 | Keyboard / context-menu access for v1 | None. v1 is drag-to-modal only. Right-click "Split hunk before this line" and "Commit this line" deferred to v2. |
| 14 | CLI parity | None for v1. `split_hunk` / `unsplit_hunk` exposed only via desktop Tauri RPC. CLI surface deferred; in-memory override store would require either persistence or desktop↔CLI IPC, both disproportionate. |
| 15 | AI / codegen rule interactions | None special. Sub-hunks are `HunkAssignment` rows; existing rules engine (`but-rules`) and AI surfaces (`but-claude`) consume them unchanged. Auto-assign rules apply to sub-hunks like any other hunk. |
