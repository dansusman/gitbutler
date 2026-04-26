# Line-by-Line Commits — Implementation Plan & Status

Companion to [`line-by-line-commits.md`](./line-by-line-commits.md). That
document is the design spec; this one tracks what's actually shipped, what
the smoke test surfaced, and what's still to do.

Last updated: end of "phase 5 polish + sub-hunk re-split + 6.5a" session.

## Status snapshot

| Phase | Status | Commits |
|---|---|---|
| 1 — Backend foundation | **Shipped** | `66404fed1d` |
| 2 — Commit-time encoding | **Shipped** | `60625307a7` |
| 3 — Diff-view rendering | **Shipped** | `807c700d5b` |
| 4 — Split icon + un-split | **Shipped** | `57f900cabe` |
| Smoke-test fixups (tauri allowlist + commit-path containment) | **Shipped** | `1e2941ece1` |
| `safe_checkout` post-commit fix | **Shipped** | `6108897acb` |
| 4.5 — Override survival across partial commits (residuals stored, multi-candidate fan-out, order-preserving alignment) | **Shipped** | `3e86b4f6e5` |
| Drag-amend re-encoding for sub-hunks (`AmendCommitWithHunkDzHandler`) | **Shipped** | `355e45ed97` |
| 4.5b — Migration re-introduces residuals after uncommit | **Shipped (this session)** | _pending_ |
| 5a/b/c — Gesture rewrite + popover (Stage / Comment / Split / Cancel) | **Shipped (this session)** | _pending_ |
| 5d — Single-tap opens popover + sub-hunk re-split (merge into existing override) | **Shipped (this session)** | _pending_ |
| 5e — Polish: blank-`+` row absorption in Split gesture; sub-hunk discard re-encoding | **Shipped (this session)** | _pending_ |
| 6 — Polish (hunk-dep on sub-hunks, storybook, etc.) | Not started | — |
| 6.5a — Serde-fy `SubHunkOverride` (in-memory round-trip) | **Shipped (this session)** | _pending_ |
| 6.5b–e — `but-db` schema, hydration, write-through, size guards | Not started — next | — |
| **7 — Splitting committed work + cross-stack moves of split pieces** | **Not started — critical for downstream workflow** | — |
| **⚠ Open: partial-commit content duplication on pure-add sub-hunks** | **Investigating** | — |

## What's validated end-to-end (manual GUI smoke test against `~/buttest`)

- ✅ Tauri RPC wire format for `split_hunk` / `unsplit_hunk`. `BString` over
  Tauri serializes as `number[]`; tests use
  `Array.from(new TextEncoder().encode(path))`.
- ✅ Anchor lookup: backend matches the frontend-supplied `HunkHeader`
  against the live worktree diff.
- ✅ Reconcile post-pass: overrides materialize as ordinary
  `HunkAssignment` rows with synthesized natural-rendering headers.
- ✅ Frontend `splitDiffHunkByHeaders` partitions the natural diff text
  into per-sub-hunk synthetic `DiffHunk`s.
- ✅ Sub-hunks render as separate hunks with their own `@@` headers and
  gutters. Each carries the split icon.
- ✅ Drag-to-stack of a sub-hunk reassigns it via the existing
  `assign_hunk` flow.
- ✅ Un-split icon collapses sub-hunks back into the natural anchor
  (after the dist rebuild of `@gitbutler/ui`).
- ✅ Partial-line commit lands cleanly (after the `safe_checkout` fix).
  Sub-hunk commit tested via the GUI checkbox + Start a commit; commit
  shows up in the stack with only the selected sub-range.
- ✅ Hunk-dependency locks behave correctly post-commit: remaining
  worktree changes show locks tying them to the stack that received the
  partial commit.

## Issues found during implementation that the design doc should be
## updated to reflect

1. **Commit-time encoding is null-side, not anchor-paired.** The design
   doc claims `create_commit` accepts the anchor-paired form (e.g.
   `-5,1 +1,10`), but reading `but-core::tree::to_additive_hunks` shows
   the engine actually expects the **null-side form** (`-5,1 +0,0` for
   pure-remove, `-0,0 +5,1` for pure-add) with the worktree-no-context
   hunk providing the implicit anchor via containment matching.
   Phase 2's `encode_sub_hunk_for_commit` produces the form the engine
   actually wants. Update the *Sub-Hunk Encoding (Commit Time)* section
   of `line-by-line-commits.md` accordingly.

2. **Tauri command allowlist.** Phase 1 didn't update
   `crates/gitbutler-tauri/permissions/default.toml`. The allowlist is
   load-bearing — Tauri rejects unknown commands with a generic
   "Command not found" error. Any new RPC must be added there. Fixed in
   `1e2941ece1` for `split_hunk` and `unsplit_hunk`. Worth a one-line
   note in the *Implementation Notes → Backend* section.

3. **Frontend commit pipeline needs sub-hunk awareness.** Phase 2 wired
   `From<HunkAssignment> for DiffSpec` on the Rust side, but the
   desktop `uncommittedService.worktreeChanges()` builds its own
   `DiffSpec[]` directly on the frontend and was looking up assignments
   against natural hunks by *exact* header equality. Sub-hunk
   assignments fail that match and were getting flagged as "stale".
   `findHunkDiff` now falls back to a containment match and returns
   the sub-hunk's synthetic diff text. Worth adding to the doc that
   any frontend-side commit/discard path that consumes assignments
   needs the same containment fallback.

4. **`safe_checkout` overprotection on partial commits.** The
   `commit_create` → rebase → `safe_checkout` flow uses a 3-way
   cherry-pick (base = HEAD, ours = post-commit tree, theirs =
   worktree tree) to detect whether the planned checkout would clobber
   uncommitted changes. For partial commits theirs is by construction a
   superset of ours; git's text merge can't see that relationship and
   reports a conflict on overlapping hunks. Fixed by adding a
   `KeepAndPreferTheirs` variant to `UncommitedWorktreeChanges` and
   routing `but-rebase::graph_rebase::materialize` through it. This is
   actually a generic partial-commit fix, not specific to sub-hunks.
   Worth recording in the doc as a prerequisite.

5. **Override-survival semantics.** The doc says overrides are
   "auto-invalidated by file edits that change the underlying hunk
   shape" — and uses anchor mismatch as the trigger. But a *partial
   commit* also changes the natural hunk shape (HEAD changes even if
   the worktree doesn't), and the design didn't anticipate that. For
   the partial-commit workflow to feel right, overrides have to
   survive HEAD changes when they're still talking about the same
   logical content. See **Phase 4.5** below.

## Phase 4.5 — Override survival across partial commits (NEW, blocking)

**Why this exists:** the current behavior is that splitting a hunk into
3 pieces, committing the middle piece, and looking back at the file
shows a single re-collapsed natural hunk — the override was dropped
because the anchor `HunkHeader` no longer exists in the worktree diff.
Users expect the remaining two pieces to stay split. Without this, the
"split → commit each piece independently" loop doesn't actually work as
a workflow; users have to re-split after every commit. Phase 5's
gesture work would land on a backend that loses state too aggressively.

### Design

**Store residuals as explicit ranges.** Today
`SubHunkOverride.ranges` only contains user-defined ranges; residuals
are computed at apply time. Change this so `split_hunk` materializes
all sub-ranges (intentional + residuals) into `ranges` up front. A
3-way split with a user range `(5, 9)` over 19 rows becomes `ranges =
[(0,5), (5,9), (9,19)]` instead of `[(5,9)]`. This makes residuals
first-class state that survives reconcile.

**Anchor migration on shape change.** Replace the current
exact-`HunkHeader` lookup with a two-step search:

1. Exact match (current behavior, fast path).
2. Containment + content match: find a natural hunk on the same path
   whose new-side range contains the override's previous new-side
   range. If exactly one candidate exists, treat it as the migrated
   anchor.

For each surviving range, remap row indices from the old anchor's row
space to the new anchor's row space. The mapping is computable from
the row-kind sequences before and after: rows that were `+` and are
now context (newly committed) shift the index; rows that stayed `+`
or `-` keep their relative position.

**Drop ranges that are now all-context.** After remapping, run
`trim_context` on each range. If a range collapses to empty, it
represents content that was just committed — drop it from `ranges`.
If all ranges drop, the override is fully consumed and removed from
the store.

**Preserve per-range stack assignments through migration.** The
override carries `assignments: BTreeMap<RowRange, HunkAssignmentTarget>`.
On migration, rekey by the new (remapped) `RowRange`.

### Scope

- ~250 lines of Rust in `crates/but-hunk-assignment/src/sub_hunk.rs`.
- New helpers: `migrate_anchor`, `remap_range`, `commit_consumed_ranges`.
- Extend `SubHunkOverride` semantics; update `apply_overrides_to_assignments`
  to call the migration pass before emit.
- 5–8 new unit tests covering: exact-match fast path, single-candidate
  migration, multi-candidate ambiguity (drop the override), residuals
  surviving partial commit, all-ranges-consumed override eviction,
  per-range assignment rekeying, contextification at boundaries.

### Edge cases to think through

- File becomes binary or too-large after commit. Drop override.
- Multiple natural hunks on the same path before commit, only one
  after. Migration must handle 1:N collapse (drop override) and 1:1
  shape change (migrate).
- Stale ranges where the rows the user marked got committed by a
  different mechanism (revert, discard). Migrating to all-context →
  drop range. Same logic as commit-consumed.
- Conflicting partial commits from two stacks. Probably out of scope
  for v1 — accept that the override may drop in pathological cases.

### Interaction with the spec's "auto-invalidated by file edits"

A user-driven file edit is still expected to drop the override
(content semantically diverged). The migration should only kick in
when the natural-hunk new-side content is **identical** between
pre- and post-shape — i.e., when the worktree text didn't change. A
quick way to detect: compare new-side blob ID before and after. If
unchanged, run migration. If changed, drop.

## Phase 6.5 — Disk persistence of overrides, worktree scope (NEW, nice-to-have)

**Why this exists:** today the override store is process-memory only.
A full app relaunch (or crash) drops every active split, and the user
has to redo the gesture from scratch. For a feature that is meant to
feel like a normal hunk operation, vanishing on restart is jarring.
Landing persistence for the worktree case is also a prerequisite for
Phase 7 (committed-hunk splits), where losing the user's split state
mid-rewrite would be an actual data-loss-shaped problem.

### Scope

- Worktree-side overrides only. The keying axis stays
  `(gitdir, path, anchor)`.
- One source of truth per project: the project's `but-db` SQLite
  database. The existing in-memory map continues to serve as the
  runtime cache; SQLite is the durable backing store. All mutations
  are write-through.
- No cross-project leakage. Each project DB owns its own override
  table.

### Backend changes

1. **Make `SubHunkOverride` fully serializable.**
   - Add `#[derive(Serialize, Deserialize)]` to `RowKind`.
   - `RowRange`, `HunkHeader`, `HunkAssignmentTarget`, and `BString`
     are already serde-capable.
   - The `anchor_diff: BString` field stores raw diff bytes; encode
     it as `Vec<u8>` for SQLite.
2. **`but-db` schema.** Add a `sub_hunk_overrides` table:
   - `gitdir TEXT NOT NULL` (probably redundant inside a per-project
     DB but kept for safety against ever-shared DBs).
   - `path BLOB NOT NULL`.
   - `anchor_old_start INTEGER NOT NULL`, `anchor_old_lines INTEGER
     NOT NULL`, `anchor_new_start INTEGER NOT NULL`,
     `anchor_new_lines INTEGER NOT NULL`.
   - `ranges_json TEXT NOT NULL` (serialized `Vec<RowRange>`).
   - `assignments_json TEXT NOT NULL` (serialized
     `BTreeMap<RowRange, HunkAssignmentTarget>`).
   - `rows_json TEXT NOT NULL` (serialized `Vec<RowKind>`).
   - `anchor_diff BLOB NOT NULL`.
   - `schema_version INTEGER NOT NULL` (start at 1; gate loads).
   - Primary key: `(gitdir, path, anchor_old_start, anchor_old_lines,
     anchor_new_start, anchor_new_lines)`.
   - Diesel migration file under `crates/but-db/migrations/`.
3. **Hydration on project open.**
   - When `Context` opens, read the table for the project's `gitdir`
     and populate the in-memory store via `upsert_override`.
   - Run `reconcile_with_overrides` once against the current worktree
     so any stale overrides (anchor gone, no migration target) get
     dropped and the persisted state stays canonical.
4. **Write-through on every mutation.** Each of `upsert_override`,
   `remove_override`, `drop_overrides`,
   `migrate_stored_override_multi` gains a paired DB call. Wrap with
   a small helper so the in-memory + DB updates can't drift.
5. **Schema versioning.** The override shape grew during phase 4.5
   (added `rows` and `anchor_diff`). Stamp the table with
   `schema_version=1` from day one; future shape changes bump the
   version and either migrate or drop on load.
6. **Size guards.** Refuse to persist (and drop the override) if
   `anchor_diff.len() + rows_json.len() > 64 KB`. Hunks that big are
   unrenderable today anyway. Surface as a one-liner log.

### Frontend changes

None required. The override store is a backend concept; the frontend
already re-reads materialized assignments from
`assignments_with_fallback` after every mutation.

### Tests

- `but-db` round-trip: insert two overrides, reopen DB, list, confirm
  identical structures (use `BTreeMap` rather than `HashMap` so
  ordering is stable).
- Hydration runs `reconcile_with_overrides`: insert a stale override
  (anchor whose path is missing from the worktree), reopen, confirm
  the row was deleted from the table.
- Migration round-trip: simulate the post-partial-commit shape
  change, run reconcile, confirm the rekeyed override is in the DB.
- Crash-recovery smoke: kill the process between `upsert_override`
  and `reconcile_with_overrides`, reopen, confirm the override
  hydrates and is reconciled in one pass.

### Edge cases / risks

- **Cross-window consistency.** Two GitButler windows on the same
  project both hydrate from the same DB but mutate independently.
  Last-writer-wins on the DB row is acceptable for v1; document
  that opening the same project twice is unsupported for split
  state.
- **`anchor_diff` bloat.** Each override stores the parent hunk's
  full diff body. Mitigation: 64 KB cap, plus the hunk-size guard
  at split-time (see point 6 above).
- **Schema drift between phases.** Phase 7 will widen the key axis
  to include `commit_id`. The version field exists precisely so
  Phase 7 can ship a version-2 migration; the on-disk format is not
  load-bearing across the line-by-line-commits feature evolution.
- **`gitdir` canonicalization.** The store keys on `repo.git_dir()`
  in some places and `ctx.gitdir.clone()` in others. They should be
  identical, but persistence amplifies any drift. Add a debug-
  assert at hydration time that the two are equal for the project
  being opened.

### Phasing

- **6.5a:** make `SubHunkOverride` and its components serde-able;
  unit-test round-trip in pure memory.
- **6.5b:** add the `but-db` table and CRUD; integration test
  against a temp DB.
- **6.5c:** wire hydration on `Context` open; reconcile-on-load.
- **6.5d:** wire write-through on all mutations.
- **6.5e:** size guards + schema version checks + crash-recovery
  test.

### When to land it

Not blocking the demo loop — phases 4.5 and 5 are higher-leverage
for the feature feeling complete. But land it **before phase 7
starts**, not just "before 7 ships": phase 7 explicitly inherits
this schema (it widens the key from `(path, anchor)` to
`(commit_id, path, anchor)` via a `schema_version=2` migration on
the same `sub_hunk_overrides` table), and validating the on-disk
format in the simpler worktree-only world first is much cheaper
than debugging a one-shot schema rollout in the middle of
committed-hunk-rewrite churn.

Consequence: 6.5 is on the critical path to 7. If 7 starts before
6.5 lands, expect to either (a) ship a throwaway in-memory shim for
7's overrides and rebuild persistence anyway, or (b) accept that
an app crash mid-cross-stack-move silently drops the user's split
state. Both are worse than just landing 6.5 first.

## Phase 7 — Splitting committed work + cross-stack moves of split pieces (NEW, critical)

**Why this exists:** Phases 1–6 cover splitting **uncommitted** worktree
hunks. The downstream workflow we actually need is the *post-commit*
version: open an existing commit's diff, split one of its hunks into
sub-ranges, then drag a sub-range to another commit / branch / stack
(or back to the worktree) without losing the other sub-ranges. This
lets a user reorganize already-recorded work at line granularity —
essentially "interactive rebase split-edit" but for hunks instead of
whole files. Today GitButler only lets you move full hunks between
commits via the existing `move_changes_between_commits` /
`uncommit_changes` / amend flows; sub-ranges of a committed hunk are
not addressable.

### User-facing behavior

- **Open a commit** in the diff view. Hunks render normally.
- **Right-click drag-select a row range** inside one of the commit's
  hunks and pick *Split hunk* from the popover (same gesture as the
  worktree case from Phase 5).
- The hunk renders as N sub-hunks with their own `@@` headers and the
  split icon, exactly like the worktree-side feature.
- **Drag a sub-hunk to:**
  1. Another commit on the same stack → `move_changes_between_commits`
     scoped to that sub-range.
  2. A different stack → same operation but cross-stack.
  3. The worktree ("uncommit just this slice") → `uncommit_changes`
     scoped to the sub-range.
  4. Amend onto the head of a different stack → same as today's
     drag-to-commit-head amend flow.
- The remaining sub-hunks stay at the source commit. The source commit
  is rewritten to omit the moved sub-range; the destination commit is
  rewritten to include it.
- The split state on the *source* commit's hunk persists across the
  rewrite (remaining sub-hunks remain split) via the same migration
  pass introduced in Phase 4.5.

### Why this is hard

The worktree case has a single "surface": the worktree itself. The
override store is keyed by `(path, anchor)` where `anchor` is a hunk
in the live worktree diff. For committed hunks the surface is
`(commit_id, path, anchor)` — we need a separate keying axis, and we
need the override to survive when the commit it's anchored to gets
rewritten (which happens on every successful sub-hunk move). The
rebase machinery in `but-rebase` and the move helpers in
`but-workspace/src/commit_engine` operate on `DiffSpec`s; we need to
teach those paths to accept the same null-side per-run encoding the
worktree path uses, scoped to a specific source commit.

### Backend scope sketch

- **`SubHunkOverride` keyed by source location.** Generalize the
  store key from `(path, anchor)` to
  `enum SubHunkOriginLocation { Worktree { path }, Commit { id, path } }`
  plus the anchor. Worktree case stays as today; commit case anchors
  the override to a specific `gix::ObjectId`.
- **Anchor migration on commit rewrite.** When a sub-range is moved
  out of source commit `S` to destination commit `D`, both `S` and
  `D` get rewritten. The override store entries that pointed at `S`
  need to migrate to the rewritten `S'` using the same
  `migrate_override_multi` content-match logic from Phase 4.5; the
  destination side typically does not need an override (the moved
  range is now part of `D`'s natural hunks). Same drop-on-shape-
  divergence semantics.
- **`move_sub_hunk` / `uncommit_sub_hunk` RPCs.** Thin wrappers around
  the existing `move_changes_between_commits` and `uncommit_changes`
  paths that take a sub-range (`RowRange` plus anchor) instead of a
  natural `HunkHeader`, encode it via
  `encode_sub_hunk_for_commit`, and forward to the existing engines.
  Tauri allowlist + `but-api` entries to mirror.
- **Reading committed hunks.** The override pass already runs on
  worktree assignments. Add a parallel pass on a commit's diff
  (probably hooked into wherever the desktop fetches per-commit
  hunks for display) so committed hunks render with the same split
  treatment when an override exists for them.

### Frontend scope sketch

- **Commit diff view picks up sub-hunks.** Same `splitDiffHunkByHeaders`
  helper, but applied to commit diffs instead of just worktree diffs.
- **Drag handlers.** Both `commitDropHandler.ts` paths
  (`AmendCommitWithHunkDzHandler`,
  `AmendCommitWithChangeDzHandler`) already re-encode via
  `diffToHunkHeaders("commit")` (Phase 4.5 follow-up). The new
  cross-commit move flow needs the analogous re-encoding wherever
  the `move_changes_between_commits` mutation is invoked. Audit
  every site that constructs a `DiffSpec` from a `HunkAssignment` or
  `DiffHunk` and route through the encoder.
- **Origin-aware drag data.** `HunkDropDataV3` already carries the
  source `commitId` for committed-hunk drags. The new path needs to
  know whether the dragged piece is a sub-hunk so the destination
  handler asks the backend to apply the null-side encoding rather
  than treating it as a full-hunk move.

### Tests

- Unit: `move_sub_hunk` against a hand-constructed commit + a
  sub-range, verifying the rewritten source omits exactly the moved
  rows and the destination contains exactly those rows. Mirror for
  `uncommit_sub_hunk`.
- Unit: override migration when source commit rewrites — mirrors
  `migration_handles_duplicate_blank_rows_in_single_candidate` but
  with `Commit { id, path }` keying.
- Integration: cross-stack move smoke test (build two stacks with a
  shared file, drag a sub-hunk from stack A's commit to stack B's
  head, verify both stacks are consistent).
- Manual GUI repro recipe similar to
  `docs/lock-repro-steps.md` but for cross-stack moves.

### Phasing

- **7a:** generalize `SubHunkOverride` keying, keep worktree behavior
  unchanged.
- **7b:** wire commit diffs through `reconcile_with_overrides`-style
  pass so committed hunks render with the split icon when an override
  exists.
- **7c:** add `split_hunk` variant for `(commit_id, path, anchor)`,
  validate per-range constraints exactly as the worktree path does.
- **7d:** wrap `move_changes_between_commits` and `uncommit_changes`
  for sub-ranges; add Tauri / `but-api` plumbing.
- **7e:** frontend drag handlers, popover wiring, source/destination
  invalidation tags so RTK refreshes both sides.
- **7f:** override migration on source-commit rewrite (drop, migrate,
  or rekey — same enum as Phase 4.5).
- **7g:** Playwright happy-path test.

### Open questions

1. **Conflicts.** Moving a sub-range to another commit can conflict
   with intervening commits. The natural-hunk move path already
   surfaces conflicts via the existing rebase pipeline; sub-ranges
   should reuse the same surface. Design check: does
   `to_additive_hunks` produce a sane error path when the rebase
   can't apply a sub-range cleanly?
2. **Renames.** If the source commit renames a file and the user
   wants to move a sub-range across the rename boundary, how does
   the override migrate? Worktree case already has the
   `previousPathBytes` plumbing; commit case needs the same.
3. **Splitting a sub-hunk further.** Should a sub-hunk produced by
   Phase 4 be re-splittable? Probably yes; the override store keying
   already accommodates nested splits via re-issuing `split_hunk`
   with a finer range, but committed-hunk splits need the same
   re-entrancy verified.
4. **Persistence.** Closed by phase 6.5. The on-disk format that
   6.5 introduces is versioned precisely so 7 can ship a
   `schema_version=2` migration that widens the primary key from
   `(path, anchor_*)` to `(commit_id, path, anchor_*)`. Concretely,
   when 7 starts:
   - Add a non-null `commit_id BLOB` column (nullable in v1, treat
     null as "worktree") and bump `schema_version`.
   - Either backfill existing rows with `commit_id = NULL` or
     drop-and-rehydrate from the in-memory store on the first
     v2 load — 6.5 already runs reconcile-on-load, so a
     drop-and-rehydrate path costs at most one re-split for the
     few users on the upgrade boundary.
   - 7's `move_sub_hunk` / `uncommit_sub_hunk` write-through paths
     reuse the same CRUD helpers 6.5 introduces; only the key
     widens.

   This question used to read "Punt or bundle into 7f." The
   bundled answer (6.5) is the chosen path.

## Phase 5 — Gesture rewrite + popover

This is the user-facing "feature on" moment. Spec is in
`line-by-line-commits.md` § *User-Facing Behavior → The gesture* and
§ *Implementation Notes → Frontend*. Summary:

### Behavior

- **Single-line click** on a delta line: toggles staging for that one
  line. Unchanged from today.
- **Click-and-drag across multiple lines**: visually highlights the
  range and, on `mouseup`, opens an inline `ContextMenu` popover
  anchored to the selection.
- The drag itself **no longer toggles staging mid-motion** — this is a
  behavior change from today, where the same gesture both stages and
  opens the annotation editor as a side effect.
- Popover items: **Stage / Unstage** (default-focused, label flips
  based on current state) / **Comment** / **Split** / **Cancel**.
- `Enter` triggers Stage/Unstage. `Esc` or click-outside dismisses.

### Frontend changes

- `packages/ui/src/lib/components/hunkDiff/lineSelection.svelte.ts`:
  remove the per-row staging side-effect that fires during
  `onMoveOver`. The drag becomes selection-only; staging only happens
  on the popover's Stage action.
- New popover component, reusing `packages/ui/src/lib/components/ContextMenu.svelte`
  (already used for the right-click hunk menu, see
  `apps/desktop/src/components/diff/UnifiedDiffView.svelte:420`).
  Anchored positioning, click-outside dismissal, and Esc handling come
  for free.
- Wire each menu item:
  - **Stage / Unstage** → existing `uncommittedService.checkLine` /
    `uncheckLine` paths over the selected range.
  - **Comment** → existing annotation editor flow
    (`handleAnnotateDrag` already takes the right shape).
  - **Split** → call `diffService.splitHunk(...)` with the selected
    rows' `RowRange`. Convert from the gesture's
    `(beforeLineNumber, afterLineNumber)` pairs to row indices using
    the same row-kind walk used by `splitDiffHunkByHeaders`.
  - **Cancel** → just close the popover.

### Validation rules at the gesture layer (mirrors backend)

The Split button should be disabled (or no-op with a tooltip) when:
1. Selection consists only of context rows.
2. Selection is the entire hunk.

(Cross-hunk straddles are already prevented because `LineSelection` is
per-hunk-component.)

Leading/trailing context trimming happens silently before sending the
RPC — no UI error needed; the backend already handles it.

### Tests

- Vitest unit tests for the row-index → `RowRange` conversion helper.
- Playwright spec covering the happy path:
  1. Open a multi-row hunk in worktree.
  2. Drag-select 3 rows in the middle.
  3. Verify popover with Stage / Comment / Split / Cancel.
  4. Click Split → 3 sub-hunks render with split icons.
  5. Drag a sub-hunk to a different stack.
  6. Click split icon → confirm prompt → un-split → assignment back to
     original stack.

### Phasing inside phase 5

- 5a: Remove the drag-staging side effect; the gesture becomes
  selection-only.
- 5b: Add the popover with Stage/Comment/Cancel (no Split yet — just
  validate the popover plumbing against existing backend).
- 5c: Add Split and wire up `diffService.splitHunk`.
- 5d: Polish and Playwright spec.

## Phase 6 — Polish

Items deferred during 1–5:

1. **Hunk-dependency analysis on sub-hunks.** Currently
   `but-hunk-dependency` runs on natural worktree hunks, not on the
   materialized sub-hunks. The doc says sub-hunks should "just work"
   because they have valid (narrower) numeric ranges. Wire it through
   and verify; add an integration test under
   `but-hunk-dependency/src/ranges/tests/`.
2. **Visual polish on the split icon.** Phase 4 used the `split` icon
   from the icon set with 0.7 opacity / hover; the design doc calls
   for "subtle icon" with a tooltip. The tooltip exists; the icon
   could use design review.
3. **Storybook story for `HunkDiff` with `isSubHunk: true`.**
4. **Stage state migration across split / unsplit (Option B).** Phase
   1 went with Option A (drop on unsplit). Migration would partition
   the parent hunk's `LineId[]` per sub-hunk on split and merge back
   on unsplit. Purely additive on top of the existing dormant-entry
   behavior.
5. **CLI parity.** `but split` / `but unsplit`. Requires either
   promoting the override store to disk persistence or adding a
   desktop↔CLI IPC channel. Both are disproportionate for v1; punt
   until there's user demand.
6. **Right-click "Split hunk before this line"** in
   `HunkContextMenu.svelte`. Single-click 2-way split. Provides
   keyboard / accessibility access.
7. **Right-click "Commit this line"** — composite shortcut that splits
   into a 1-row sub-hunk and opens the commit composer scoped to that
   sub-hunk. Requires extra design for stack-target selection.
8. **Doc updates** per the corrections list above.

## What landed in the phase 5-polish + sub-hunk re-split + 6.5a session

### Phase 4.5b — Migration re-introduces residuals after uncommit

**Bug:** the doc-stage Phase 4.5 migration (`migrate_override_multi`)
emitted only the user-range remappings that survived the new anchor's
row shape. Rows that *re-appeared* in the natural hunk — the case
where the user uncommits a partial commit and the natural hunk grows
back — were left uncovered, and `materialize_override` silently
dropped them from the rendered diff. End-result: "Section A" /
"Section B" disappear from the worktree view after an uncommit, even
though the worktree still contains them.

**Fix:** after remapping each old user range onto the new candidate
anchor, run `materialize_residual_ranges` over the surviving set. The
function already knows how to fill non-context gaps with trimmed
residuals; it just wasn't being called on the migration path.

**Tests:**
- New regression `migration_re_introduces_residuals_after_uncommit`
  in `crates/but-hunk-assignment/src/sub_hunk.rs`. Pre-state is a
  partial-commit shape `(-1,3 +1,5)` with two user picks at indices 1
  and 3 (rows 0/2/4 are now context). Post-state is the
  uncommitted-back `(-1,0 +1,5)` with all rows added. Asserts the
  migrated override carries five disjoint ranges covering rows 0–4
  individually.
- Pre-existing `migration_handles_duplicate_blank_rows_in_single_candidate`
  test data was malformed (Rust `\` line-continuation eats the
  leading-space context marker, so `" - alpha line one"` was parsing
  as `Remove`). Switched the literal to `\x20` for context lines
  so the test reflects what real diffs look like.

### Phase 5 — Gesture rewrite + popover

- **5a (drag becomes selection-only).** `lineSelection.svelte.ts`
  no longer calls `onLineClick` during `onMoveOver`. The mouseup
  `MouseEvent` is forwarded into `LineDragEndParams.clientX` /
  `clientY` so the popover can be anchored to the gesture endpoint.
  The row-td `onmouseup` in `HunkDiffRow.svelte` was also updated to
  forward the event — it fires *before* the document-level
  `mouseup` handler and was previously calling `onEnd()` with no
  event, dropping `clientX/Y` and falling through to the annotation
  editor. Touch path unchanged.
- **5b–5c (popover + Split).** New component
  `apps/desktop/src/components/diff/HunkSelectionPopover.svelte`
  reuses `ContextMenu` and exposes Stage/Unstage / Comment / Split /
  Cancel. Wired in `UnifiedDiffView.svelte`:
  - **Stage / Unstage** runs the existing
    `uncommittedService.checkLine` / `uncheckLine` paths over the
    selected delta lines. Label flips based on whether any selected
    line is currently unstaged at popover-open time.
  - **Comment** falls through to the existing `handleAnnotateDrag`
    flow.
  - **Split** translates the gesture's line-number range to a
    body-row `RowRange` via the new `bodyRowRangeFromSelection`
    helper in `apps/desktop/src/lib/hunks/hunk.ts` and calls
    `diffService.splitHunk`. Validates per the spec (no
    context-only, no whole-hunk). Disabled (with a tooltip) on
    committed-hunk views — Phase 7 is required to actually split
    those.
  - `Enter` triggers Stage/Unstage; `Esc` and click-outside dismiss
    via `ContextMenu`.
- **Single-tap also opens the popover.** `lineSelection.onEnd` now
  fires `onDragEnd` for the no-movement case as well (when
  `onDragEnd` is wired). Single-clicks become a 1-row range that
  produces the same popover. The legacy synchronous-stage
  `onLineClick` path stays as a fallback for consumers that don't
  wire `onDragEnd` (Storybook, commit views without the popover).
- **Sub-hunk re-split.** Backend
  `split_hunk_with_perm` now merges new ranges into an existing
  override via the new `merge_user_ranges_into_partition` helper
  rather than replacing it. The frontend popover always operates
  against the *natural* anchor hunk (looked up by
  `subAnchor` when `isSubHunk`) so re-splitting an already-split
  sub-hunk refines the partition rather than erroring out on
  anchor mismatch. Tests cover split-at-boundary, span-carve,
  no-op-when-already-aligned, and empty-input passthrough.
- **Popover anchor element.** Synthesized `MouseEvent`s passed as
  `ContextMenu`'s `target` tripped Svelte 5's
  `state_descriptors_fixed` runtime check (MouseEvent properties
  are getter-only and don't satisfy the `$state`-stored object
  contract). Fixed by anchoring the popover to a transient
  zero-size `position: fixed` div at the gesture coordinates and
  passing that as the target instead.
- **`linesInSelection` snapshot before redux dispatch.** Items
  pushed into a `$state`-backed array become Svelte 5 proxies; the
  Stage handler's `uncommittedService.checkLine(... , line)` chain
  re-entered Svelte's event runtime with proxy property descriptors
  and threw `state_descriptors_fixed`. Fixed by snapshotting each
  line into a plain `{ newLine, oldLine }` literal before passing
  to the dispatch.
- **Helpers.** `bodyRowRangeFromSelection`,
  `countDeltaRowsInRange`, `countBodyRows`, and
  `expandRangeToAbsorbBlankAddRows` added to `hunk.ts` with vitest
  coverage. The last one handles the blank-residual UX issue — see
  Phase 5e below.
- **Doc cleanup.** `docs/lock-repro-steps.md` lost the dev-console
  caveat; the repro is now a pure-GUI flow.

### Phase 5e — Polish: blank-Add absorption + sub-hunk discard

- **Blank-`+` row absorption in Split gesture.** Splitting a
  multi-section pure-add hunk (e.g. ## Section A / blanks / ## Section
  B / blanks / ## Section C) used to leave the inter-section blank
  rows as their own 1-row sub-hunks. `expandRangeToAbsorbBlankAddRows`
  walks the split-RPC's body-row range outward and pulls in any
  adjacent blank-`+` rows so the user's selection eats them. Wired
  into `applySplitToSelection` in `UnifiedDiffView.svelte`.
- **Sub-hunk "Discard change" routed through `diffToHunkHeaders`.**
  `discardHunk` in `HunkContextMenu.svelte` was passing the sub-hunk's
  synthesized `HunkHeader` directly to the discard RPC, which silently
  no-op'd because no natural worktree hunk matched. Now re-encodes
  via `diffToHunkHeaders(item.hunk.diff, "discard")`, mirroring the
  same containment-aware encoding the commit path uses. Natural hunks
  pass through unchanged.

## ⚠ Open issue — partial-commit content duplication on pure-add sub-hunks

**Symptom (from `~/buttest/athirdfile.md`).** After splitting a
pure-add multi-section hunk into A/B/C and committing the middle
sub-hunk (B), the resulting HEAD blob contains Section B *twice* and
the worktree-vs-HEAD diff still shows Section B as added (i.e. the
commit didn't actually consume the rows from the worktree's
perspective). Successive partial commits keep accumulating duplicates.

**Snapshot of a buggy commit (`6d29c49 fdfdfdfdfdf` in test repo):**
- Parent's `athirdfile.md`: 1 line (`Adding…`).
- Commit's diff: `@@ -1 +1,23 @@` — 22 added rows, with Section B
  appearing both at lines 10–13 and 17–20.
- Natural worktree hunk before this commit had only ~17 added rows.
  The commit somehow produced *more* added rows than existed in the
  source, and duplicated Section B.

**Hypotheses to investigate next session.** None are confirmed yet;
add backend trace logging at the listed sites to capture the exact
encoding path:

1. `encode_sub_hunk_for_commit` produces overlapping null-side
   header runs after a migration round (e.g. two `(-0,0 +N,K)`
   ranges whose `[N, N+K)` spans intersect). `to_additive_hunks`
   then applies both, double-inserting the overlapping rows.
2. `safe_checkout`'s 3-way merge on partial commits (`base = pre-
   commit HEAD`, `ours = post-commit HEAD`, `theirs = worktree`)
   over-merges when `theirs` is a strict superset of `ours`,
   leaving the committed rows still present in the worktree as if
   they hadn't moved into HEAD.
3. `From<HunkAssignment> for DiffSpec` emits the natural-anchor
   header *and* the encoded sub-range when both `hunk_header` and
   `sub_hunk_origin` are set. (Read of the impl in
   `crates/but-hunk-assignment/src/lib.rs:145` says it picks one
   branch, but worth a defensive trace.)

**Repro recipe (manual, not yet a test).**
1. New file `f.md` with a single-line baseline. Commit on a remote
   branch; clone into a workspace; do not modify yet.
2. Append three sections separated by blank lines (the exact
   shape of `~/buttest/athirdfile.md`).
3. In the desktop, drag-select Section A, click Split. Drag-select
   Section B, click Split. Now you have three sub-hunks A/B/C.
4. Commit Section B sub-hunk to a stack.
5. Inspect HEAD's blob for `f.md`. Expectation: 4 lines added
   (Section B + 3 betas). Reality (today): more than 4 lines, with
   Section B duplicated.

**Recovery for the dev test repo:** the duplicates are durable in
HEAD; either undo the chain via `but undo` / GitButler UI and
re-split, or rewrite history manually with `git rebase -i` to drop
the bad partial commits.

**Not in this session because** the fix lives in `to_additive_hunks`
or `safe_checkout`'s 3-way merge — separate from the override-store
and gesture work this session shipped — and needs trace-driven
diagnosis I didn't complete.

## What landed in the phase 5 + 6.5a session

### Phase 5 — Gesture rewrite + popover (shipped)

- **5a (drag becomes selection-only).** `lineSelection.svelte.ts`
  no longer calls `onLineClick` during `onMoveOver`; staging on
  `onStart` was deferred to `onEnd` for the no-movement case so
  single-click staging is preserved while drag-staging side-effects
  are gone. The mouseup `MouseEvent` is forwarded into
  `LineDragEndParams.clientX` / `clientY` so the popover can be
  anchored to the gesture endpoint. The touch path is unchanged.
- **5b–5c (popover + Split).** New component
  `apps/desktop/src/components/diff/HunkSelectionPopover.svelte`
  reuses `ContextMenu` and exposes Stage/Unstage / Comment / Split /
  Cancel. Wired in `UnifiedDiffView.svelte`:
  - **Stage / Unstage** runs the existing
    `uncommittedService.checkLine` / `uncheckLine` paths over the
    selected delta lines. Label flips based on whether any selected
    line is currently unstaged at popover-open time.
  - **Comment** falls through to the existing `handleAnnotateDrag`
    flow.
  - **Split** translates the gesture's line-number range to a
    body-row `RowRange` via the new `bodyRowRangeFromSelection`
    helper in `apps/desktop/src/lib/hunks/hunk.ts` and calls
    `diffService.splitHunk`. Disabled for sub-hunks (re-splitting
    is deferred to v2 / Phase 7) and validates per the spec
    (no context-only, no whole-hunk).
  - `Enter` triggers Stage/Unstage; `Esc` and click-outside dismiss
    via `ContextMenu`.
- **Helpers.** `bodyRowRangeFromSelection`,
  `countDeltaRowsInRange`, and `countBodyRows` added to `hunk.ts`
  with vitest coverage.
- **Doc cleanup.** `docs/lock-repro-steps.md` lost the dev-console
  caveat; the repro is now a pure-GUI flow.

### Phase 6.5a — Serde-fy `SubHunkOverride` (shipped)

- `RowKind` and `SubHunkOverride` now derive `Serialize` /
  `Deserialize`. The `assignments: BTreeMap<RowRange,
  HunkAssignmentTarget>` field uses a custom `assignments_pairs`
  serde module that emits a JSON array of `[range, target]` pairs,
  because JSON object keys must be strings while `RowRange`
  serializes as an object.
- New unit test `sub_hunk::tests::sub_hunk_override_serde_round_trip`
  exercises a fully-populated override (path, anchor, ranges,
  per-range stack assignment, row kinds, anchor diff body) through
  `serde_json` and verifies losslessness. Required precondition for
  6.5b–e (DB plumbing).

## Recommended next-session order

1. **Phase 5d** (Playwright happy-path spec covering drag→popover→
   Split→drag-to-stack→unsplit). Smoke-tested manually; spec is
   straightforward to write now that the gesture and popover are
   stable.
2. **Phase 6 polish.** Items #1 (hunk-dependency on sub-hunks —
   add an integration test under
   `but-hunk-dependency/src/ranges/tests/` that constructs a
   sub-hunk via the override path and verifies dependency analysis
   treats it identically to a natural hunk) and #2/#3 (split-icon
   visual polish + Storybook).
3. **Phase 6.5b–e.** Add the `sub_hunk_overrides` table to
   `but-db` (schema, CRUD, schema-version stamp, size guards), wire
   hydration on `Context` open, and add write-through to every
   `upsert_override` / `remove_override` /
   `migrate_stored_override_multi` call in
   `crates/but-hunk-assignment/src/sub_hunk.rs`. The serde shape
   landed in 6.5a is the format that goes into `assignments_json` /
   `rows_json` columns; `anchor_diff` ships as a `BLOB`. Tests:
   round-trip via temp DB, hydration runs reconcile,
   crash-recovery between upsert and reconcile.
4. **Phase 7.** As outlined below; explicitly inherits the 6.5
   schema by widening the primary key from `(path, anchor_*)` to
   `(commit_id, path, anchor_*)` via a `schema_version=2`
   migration.

## Useful artifacts from the smoke test session

- Test fixtures live at `~/buttest/splittest_pure_add.md` (new file,
  pure-add hunk) and `~/buttest/anotherfile.md` (modified, mixed
  `+`/`-` hunk).
- Project ID for `~/buttest` in the running dev app:
  `7ec0ca28-8920-422e-a425-3bd5fdfd50a1`.
- Dev console invocation pattern (Tauri v2, `withGlobalTauri: false`,
  Safari-strict syntax):
  ```js
  import('/node_modules/.vite/deps/@tauri-apps_api_core.js').then(
    m => m.invoke('split_hunk', {
      projectId: '<id>',
      path: Array.from(new TextEncoder().encode('relative/path')),
      anchor: { oldStart, oldLines, newStart, newLines },
      ranges: [{ start: <row>, end: <row> }],
    })
  ).then(r => console.log('OK', r), e => console.error('ERR', e));
  ```
- After a raw `invoke()`, the GUI doesn't auto-refresh because RTK's
  `invalidatesTags` only fires through the redux mutation. Touch any
  file in the project to nudge the watcher and force a refetch.
