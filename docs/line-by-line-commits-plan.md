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
| 6 — Polish (hunk-dep on sub-hunks, storybook, etc.) | **In progress** (item #1 shipped this session) | _pending_ |
| 6.5a — Serde-fy `SubHunkOverride` (in-memory round-trip) | **Shipped** | _pending_ |
| 6.5b — `but-db` `sub_hunk_overrides` schema + CRUD | **Shipped** | _pending_ |
| 6.5c — Hydration on-demand (`ensure_hydrated`) + bridge (`to_db_row` / `from_db_row`) | **Shipped (this session)** | _pending_ |
| 6.5d — Write-through on `split_hunk` / `unsplit_hunk` + read-path hydration in `changes_in_worktree_with_perm` | **Shipped (this session)** | _pending_ |
| 6.5e — Size guard (`MAX_OVERRIDE_DB_BYTES = 64 KB`) | **Shipped (this session)** | _pending_ |
| 6.5d-followup — Wire `reconcile_with_overrides_persistent` into `assignments_with_fallback` / `assign` (write-through on partial-commit migration / drops) | **Shipped (this session)** | _pending_ |
| **7 — Splitting committed work + cross-stack moves of split pieces** | **In progress** (7a shipped this session) | _pending_ |
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
  unchanged. **✅ Shipped.**
- **7b:** wire commit diffs through `reconcile_with_overrides`-style
  pass so committed hunks render with the split icon when an override
  exists. **✅ Backend foundation shipped** (commit-side query
  helpers + worktree-reconcile isolation; RPC + frontend wiring
  follows in 7c).
- **7c-1:** add `pub origin: SubHunkOriginLocation` to
  `SubHunkOverride`; switch `key_for_override` to read it; rewire
  `upsert_override_at` so `location` is authoritative for both the
  store key and the stored `ov.origin`. **✅ Shipped.**
- **7c-2:** `but-db` `sub_hunk_overrides` schema v2 — add
  `commit_id BLOB NOT NULL DEFAULT X''` column to the primary key,
  bump `OVERRIDE_DB_SCHEMA_VERSION` to 2, widen `to_db_row` /
  `from_db_row` to encode/decode the column. **✅ Shipped.**
- **7c-3:** `split_hunk_in_commit` and `unsplit_hunk_in_commit` RPCs
  in `but-api/src/diff.rs`; new `remove_override_at` /
  `remove_override_persistent_at` helpers; Tauri allowlist + main.rs
  invoke list. **✅ Shipped.**
- **7c-4:** `tree_change_diffs_in_commit` RPC + new
  `apply_commit_overrides_to_patch` materialization helper that
  replaces matching natural hunks in a `UnifiedPatch` with N
  sub-hunks carrying synthesized headers and sliced diff bodies.
  **✅ Shipped this session.**
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

## What landed in the phase 6.5b session

### Phase 6.5b — `but-db` `sub_hunk_overrides` schema + CRUD (shipped)

- **Migration.** New file
  `crates/but-db/src/table/sub_hunk_overrides.rs` containing a single
  `M::up(20260424120000, SchemaVersion::Zero, ...)` that creates the
  `sub_hunk_overrides` table with the columns the plan calls for:
  `gitdir TEXT`, `path BLOB`, four `anchor_*` integers, three
  JSON-encoded text columns (`ranges_json`, `assignments_json`,
  `rows_json`), the cached `anchor_diff` blob, and a
  `schema_version` integer. Primary key spans `(gitdir, path,
  anchor_old_start, anchor_old_lines, anchor_new_start,
  anchor_new_lines)` to match the in-memory key shape exactly.
  Registered in `crates/but-db/src/table/mod.rs` and the `MIGRATIONS`
  array in `crates/but-db/src/lib.rs`.
- **Row type and handles.** Public `SubHunkOverrideRow` plus
  `SubHunkOverridesHandle` / `SubHunkOverridesHandleMut` wrappers
  exposed from `but_db`. Read API: `list_all`, `list_for_gitdir`,
  `get`. Write API: `upsert` (single-statement `ON CONFLICT … DO
  UPDATE`), `delete`, `delete_for_gitdir`. The crate intentionally
  treats the JSON columns as opaque strings — `but-hunk-assignment`
  will own the serde<→`SubHunkOverride` bridge in 6.5c, which keeps
  `but-db` from depending on `but-hunk-assignment` (only the inverse
  edge already exists).
- **Tests.** 11 new integration tests in
  `crates/but-db/tests/db/table/sub_hunk_overrides.rs` covering empty
  list, upsert+get round-trip, upsert-replaces-existing, list filter
  by `gitdir`, primary-key disambiguation between two rows that
  differ only on `anchor_old_lines`, single-row delete, no-op delete
  of a missing row, `delete_for_gitdir` scoping, byte-exact blob
  round-trip (path with embedded NUL/`\xff`, `anchor_diff` carrying
  every `u8`), and transaction commit/rollback.
- **Snapshot updates.** `crates/but-db/tests/db/migration.rs`'s
  `run_ours` schema dump and migration-list dump updated for the new
  table and the `20260424120000` migration version.

### What's next (6.5c–e)

6.5c hydration: on `Context` open, read every override row for the
project's `gitdir` via `SubHunkOverridesHandle::list_for_gitdir`,
deserialize each row's three JSON columns + the four anchor integers
+ `anchor_diff` into a `SubHunkOverride`, and call
`upsert_override(gitdir, ov)` for each. Then run
`reconcile_with_overrides(gitdir, &mut assignments)` once against the
current worktree so any anchors that no longer match are dropped (and
the `delete` write-through from 6.5d removes them from disk).

6.5d write-through: each of `upsert_override`, `remove_override`,
`drop_overrides`, and `migrate_stored_override_multi` in
`crates/but-hunk-assignment/src/sub_hunk.rs` needs a paired DB call.
The cleanest route is a small helper that takes `&Context` (or just
`&DbHandle`) and the same `(path, anchor)` key, so the in-memory and
disk mutations can't drift. This is also where the
`SubHunkOverride` ↔ `SubHunkOverrideRow` bridge lives:
- `ranges_json = serde_json::to_string(&ov.ranges)?`
- `assignments_json = serde_json::to_string(&ov.assignments)?`
  (note: the field already uses the `assignments_pairs` serde
  helper that emits a JSON array of `[range, target]` pairs, so
  this just calls into that)
- `rows_json = serde_json::to_string(&ov.rows)?`
- `anchor_diff = ov.anchor_diff.to_vec()`
- four anchor integers from `ov.anchor`
- `schema_version = 1`

6.5e size guards: refuse to persist (and drop the override) if
`anchor_diff.len() + rows_json.len() > 64 KB`. Surface as a single
`tracing::warn!`. Plus a crash-recovery test: write an override,
simulate process death between `upsert_override` and
`reconcile_with_overrides`, reopen, confirm hydration runs and the
override survives or is reconciled correctly.

## What landed in the phase 6.5c–e session

### Bridge: `SubHunkOverride` ↔ `but_db::SubHunkOverrideRow`

- `to_db_row(gitdir, ov) -> Result<Option<SubHunkOverrideRow>>` and
  `from_db_row(row) -> Result<SubHunkOverride>` in
  `crates/but-hunk-assignment/src/sub_hunk.rs`. JSON-encodes
  `ranges`, `rows`, and `assignments` (the last as a `Vec<(RowRange,
  HunkAssignmentTarget)>` array of pairs to mirror the in-memory
  `assignments_pairs` serde helper, since JSON object keys can't
  carry `RowRange` structs).
- `OVERRIDE_DB_SCHEMA_VERSION = 1` is stamped on every row written;
  `from_db_row` rejects unknown versions with a clear error so a
  future binary downgrade never silently corrupts the in-memory
  shape.
- `HunkAssignmentTarget` gained `PartialEq + Eq` so the reconcile
  write-through can detect "no-op migration" cases without diffing
  serialized JSON.

### Size guard (Phase 6.5e)

- `MAX_OVERRIDE_DB_BYTES = 64 * 1024`. `to_db_row` returns
  `Ok(None)` (with a `tracing::warn!`) when
  `anchor_diff.len() + rows_json.len()` exceeds the cap.
- `upsert_override_persistent` honors the cap by **keeping the
  in-memory entry** but actively **deleting any stale on-disk row
  for the same `(gitdir, path, anchor)` key**. That way the user's
  current session still gets the split UX while preventing the row
  from coming back at next launch.

### Hydration (Phase 6.5c)

- `hydrate_from_db(db, gitdir) -> Result<usize>` reads every row for
  the project and `upsert_override`s it into the in-memory store.
  Rows that fail to deserialize are logged and skipped — the
  reconcile pass on the next worktree read will drop their
  in-memory state if the anchor is gone.
- `ensure_hydrated(db, gitdir)` is the lazy entry point: a process-
  wide `OnceLock<Mutex<HashSet<PathBuf>>>` tracks which `gitdir`s
  have been hydrated, so every persistent helper can call it for
  free at top-of-function. Errors are logged and swallowed; the
  user's mutation still goes through.
- This avoids the alternative of teaching `Context::open` how to
  hydrate, which is awkward because `Context.db` is an
  `OnDemandCache` and can't synchronously do DB reads at
  construction time.

### Write-through (Phase 6.5d)

- New helpers in `sub_hunk.rs`:
  - `upsert_override_persistent(db, gitdir, ov) -> Result<bool>`
  - `remove_override_persistent(db, gitdir, path, anchor)`
  - `drop_overrides_persistent(db, gitdir, keys)`
  - `reconcile_with_overrides_persistent(db, gitdir, &mut
    assignments)` — runs the in-memory reconcile then
    write-throughs deletes (for dropped overrides) and upserts (for
    migrated overrides whose `ranges` / `assignments` / `anchor_diff`
    actually changed).
- Wired into `but-api/src/diff.rs`: `split_hunk_with_perm` now
  calls `upsert_override_persistent`; `unsplit_hunk_with_perm` now
  calls `remove_override_persistent`.

### Tests (9 new, all green; total 77 in `but-hunk-assignment`)

- `db_row_round_trip_preserves_override`
- `from_db_row_rejects_unknown_schema_version`
- `to_db_row_size_guard_drops_oversize`
- `upsert_override_persistent_writes_through`
- `upsert_override_persistent_drops_disk_when_oversize`
- `remove_override_persistent_clears_both_layers`
- `drop_overrides_persistent_clears_both_layers`
- `hydrate_from_db_rebuilds_in_memory_store`
- `hydrate_from_db_skips_malformed_rows`
- `ensure_hydrated_runs_once_per_gitdir` (uses a deliberate poison
  row to confirm the second call is a true no-op)

### Smoke-test fix: read-path hydration

First end-to-end pass against the dev app revealed that splits
*written* through `upsert_override_persistent` were on disk after
`Cmd-Q`, but did **not** render after relaunch. Root cause:
`ensure_hydrated` was only invoked from the `*_persistent` *write*
helpers, never from the read path that fires on app open. The fix
is a one-liner in `crates/but-api/src/diff.rs::changes_in_worktree_with_perm`:
before the worktree-changes pipeline runs, call
`but_hunk_assignment::ensure_hydrated(&db, &gitdir)`. Because
`ensure_hydrated` is a process-wide once-per-gitdir guard, this is
free on subsequent calls and covers every read path the desktop
ever reaches (opening a project always triggers
`changes_in_worktree`).

Manual GUI smoke against `~/buttest`:
1. Split `splittest_pure_add.md` and `athirdfile.md`.
2. `Cmd-Q`. Confirm `sqlite3 .git/gitbutler/but.sqlite "SELECT
   count(*) FROM sub_hunk_overrides"` returns `2`.
3. Relaunch via `pnpm dev:desktop`.
4. Sub-hunks render with the same boundaries as before quit. ✅

### What's intentionally *not* shipped this session

- **`reconcile_with_overrides_persistent` is not yet called from
  `assignments_with_fallback` / `assign`.** Those functions take a
  `HunkAssignmentsHandleMut` (a savepoint-borrowed handle), so to
  also do sub-hunk DB writes we'd need to widen their signatures to
  take `&mut DbHandle` and create the savepoint internally. That's
  a workspace-wide signature change touching many callers; better
  to do it as its own focused refactor. Until then:
  - The `split_hunk` / `unsplit_hunk` paths *do* persist their own
    mutations (above), so the user's intentional gestures survive
    a relaunch.
  - Read-path hydration runs at the `changes_in_worktree_with_perm`
    entry, so the in-memory store is correctly populated on launch.
  - The migration / drop side-effects of `reconcile_with_overrides`
    (i.e. partial-commit anchor migrations and stale-anchor drops)
    do **not** yet write through. On the next relaunch, hydration
    re-introduces the pre-reconcile state; the next reconcile pass
    will recompute the same drops/migrations in memory and the
    eventual `split_hunk` / `unsplit_hunk` will re-persist them.
    No data loss, but a wasted reconcile pass per launch on stale
    overrides. Acceptable for v1; the followup refactor closes the
    loop.

### Recommended next-session pickup

1. Phase 5d (Playwright) and Phase 6 polish (hunk-dep on sub-hunks,
   icon polish, Storybook).
2. Phase 7 (committed-hunk splits + cross-stack moves), which now
   has a clean schema (1) it can extend to (gitdir, commit_id, path,
   anchor_*) via a `schema_version=2` migration.

## What landed in the 6.5d-followup session

### Phase 6.5d-followup — Persistent variants of `assign` / `assignments_with_fallback`

Rather than break the existing `HunkAssignmentsHandleMut`-based
signatures (which would force a workspace-wide refactor of every
transaction-based caller in `but-claude`, `but/`, `gitbutler-watcher`,
etc.), this session added two new entry points alongside the existing
ones:

- `but_hunk_assignment::assignments_with_fallback_persistent(db: &mut
  DbHandle, repo, ws, worktree_changes, context_lines)`
- `but_hunk_assignment::assign_persistent(db: &mut DbHandle, repo, ws,
  requests, context_lines)`

Both run `sub_hunk::reconcile_with_overrides_persistent` *before*
taking the `hunk_assignments` savepoint, so override migrations and
stale-anchor drops triggered by a partial-commit reconcile now write
through to the `sub_hunk_overrides` table. The savepoint is created
internally on the same `&mut DbHandle`, so the override write-through
and the assignments write don't have to be interleaved at the call
site.

Wired into the high-traffic worktree paths in `crates/but-api/src/diff.rs`:

- `changes_in_worktree_with_perm` (the read path that fires on every
  worktree refresh; previously wrapped in an `immediate_transaction`
  that's now redundant — both the savepoint and the override CRUD
  auto-commit through the underlying connection).
- `assign_hunk_only_with_perm` (the write path behind
  `assign_hunk` / `assign_hunk_only`).
- The `split_hunk_with_perm` / `unsplit_hunk_with_perm` post-mutation
  reconcile so the materialize step also write-throughs any drops the
  reconcile may produce.

Left on the legacy / non-persistent variant for now (out of session
scope; they'll inherit the persistence on the next pass):

- `crates/but-api/src/legacy/virtual_branches.rs::unapply_stack_with_perm`
- `crates/but-api/src/commit/{uncommit,undo}.rs` (these are
  transaction-scoped via `db.transaction()`; routing them through
  `*_persistent` would either (a) require dropping the explicit
  transaction wrapper or (b) plumbing an override-aware variant onto
  `Transaction`). Since these run after a commit/uncommit anyway,
  the next `changes_in_worktree_with_perm` reconcile picks up the
  shape change and write-throughs whatever the override reconcile
  produces.
- `crates/but-claude/src/{hooks,session}.rs`,
  `crates/but-cursor/src/lib.rs`, `crates/but-tools/src/workspace.rs`,
  `crates/but-rules/src/{lib,handler}.rs`,
  `crates/gitbutler-watcher/src/handler.rs`,
  `crates/but/src/...` and the integration tests in
  `crates/gitbutler-branch-actions/tests/...` — these all read
  assignments from peripheral surfaces and can keep using the
  in-memory variant. The desktop's worktree refresh path is the
  authoritative reconcile.

Net effect: the wasted-reconcile-on-launch caveat from the prior
session is closed for the desktop. After a partial commit that
drops or migrates an override, the next worktree read writes the
new override shape (or its absence) to disk, and a relaunch hydrates
the canonical post-reconcile state directly.

### Tests

No new tests in this session — the new entry points reuse the
existing `reconcile_with_overrides_persistent` machinery (whose
integration tests in `but-hunk-assignment` cover round-trip / drop /
migrate semantics) plus the existing assignments reconcile (covered
by the 30+ unit tests in `crates/but-hunk-assignment/src/lib.rs`).
The full `but-hunk-assignment` test suite (77 tests) still passes.

### Smoke-test fix #1 — watcher path also needs the persistent variant

First manual GUI pass against `~/buttest`:

1. Split `athirdfile.md` into A/B/C.
2. Edit the file to delete Section B's content. UI showed only A and
   C as expected.
3. `Cmd-Q`. Inspected `sub_hunk_overrides`: row was **stale** —
   anchor still `+1,13` with three ranges, even though the natural
   hunk had shrunk to `+1,8`.

Root cause: the file-edit event flows through
`gitbutler-watcher::handler::emit_worktree_changes`, which was still
calling the legacy `but_hunk_assignment::assignments_with_fallback`
(in-memory only). The watcher migrated the in-memory override and
pushed assignments to the frontend; disk stayed at the pre-edit
shape. The eventual `changes_in_worktree_with_perm` call — which
*does* use the persistent variant — then saw
`memory-before == memory-after` and emitted no writes.

Fix:

- Routed the watcher's `assignments_and_errors` helper through
  `assignments_with_fallback_persistent` (`&mut DbHandle` was
  already available at every call site in
  `emit_worktree_changes`).

### Smoke-test fix #2 — `reconcile_with_overrides_persistent` must compare disk vs memory

Even with fix #1, the persistent variant had a latent correctness
bug: it snapshotted the in-memory store before *and* after running
`reconcile_with_overrides`, then wrote the diff to disk. That works
when the persistent variant is the only mutator, but as soon as
*any* non-persistent caller (the legacy watcher path before fix #1,
or any peripheral caller in `but-claude` / `but-cursor` /
`but-tools` / `but-rules` / `but/` / etc.) runs first, the
in-memory store moves ahead of disk and `before == after` from the
persistent variant's perspective even though disk is wrong.

Fix in `crates/but-hunk-assignment/src/sub_hunk.rs`:

- Reworked `reconcile_with_overrides_persistent` to read the
  authoritative *disk* state via
  `db.sub_hunk_overrides().list_for_gitdir(&key)?` after the
  in-memory reconcile, key both sides by `(path, anchor)`, and emit
  writes whenever they diverge:
  - Disk has a row with no in-memory match → `delete`.
  - In-memory entry has no disk match → `upsert`.
  - Both present but the serialized row differs → `upsert`.
  - Identical → skip.
- The `to_db_row(...)? == None` branch (size-guard refusal) still
  proactively `delete`s any stale row for the same key, matching
  the existing `upsert_override_persistent` semantics.

This closes the drift window regardless of which non-persistent
caller mutated memory first; the next persistent call always
reconciles disk to canonical post-reconcile memory.

### Re-validation against `~/buttest`

1. Split `splittest_pure_add.md` into A/B/C, partial-commit B.
2. Split `athirdfile.md` into A/B/C, edit out Section B.
3. `Cmd-Q`. Inspect `sub_hunk_overrides`:

```
path                   o_s  o_l  n_s  n_l  ranges_json
splittest_pure_add.md  1    6    1    19   [{0,9},{15,19}]
athirdfile.md          1    1    1    8    [{1,4},{4,8}]
```

Both rows match the live `git diff HEAD` shape exactly. The
migrated post-partial-commit override on `splittest_pure_add.md`
and the migrated post-edit override on `athirdfile.md` are now
durably persisted on the same tick as the in-memory mutation.
✅

## What landed in the phase 6 polish (item #1) session

### Phase 6 polish #1 — Hunk-dependency analysis on sub-hunks

**Premise.** The override pass in `but-hunk-assignment` materializes
sub-hunks as ordinary `HunkAssignment` rows whose synthesized
`HunkHeader` carries narrower `(new_start, new_lines)` than the
parent natural hunk. The hunk-dependency engine
(`but-hunk-dependency::ranges::PathRanges::intersects`) is structurally
agnostic to that width — it consumes plain `(start, lines)` pairs.
Sub-hunks should therefore lock exactly the slice of committed range
they overlap, with no special-casing.

**What was missing.** Nothing in the production code path. But the
contract was implicit: a future refactor that re-widened a sub-hunk
to its parent anchor before intersection (a plausible "fix" to a
caller that mistakenly thinks sub-hunks are a different shape) would
break lock attribution silently, with no test guarding the boundary.

**What landed.** Two new pinning unit tests in
`crates/but-hunk-dependency/src/ranges/tests/path.rs`:

- `sub_hunk_narrower_range_locks_only_overlapping_slice` — committed
  modification at line 4 of a 7-line file. The natural parent hunk
  spanning lines 1..=7 locks; the three sub-hunks the user might
  carve out (lines 1..=3, 4..=4, 5..=7) lock only the middle one.
- `sub_hunk_one_row_lock_at_committed_addition_boundary` —
  committed pure-addition spanning new-side lines 4..=8. 1-row
  sub-hunks at every line inside the window lock; 1-row sub-hunks
  at lines 3 and 9 (immediately outside) don't.

The tests are deliberately at the `PathRanges` layer (rather than
end-to-end through `assignments_with_fallback` +
`hunk_dependencies_for_workspace_changes_by_worktree_dir`) because
the layering is already correct and the coupling to be guarded is
purely numeric: any sub-hunk synthesized by the override pass turns
into a `(new_start, new_lines)` query at this layer, so pinning the
intersection contract here covers every downstream consumer.

**Tests:** 33 unit + 20 integration in `but-hunk-dependency` all
green; no other crate touched.

### What's still left in Phase 6

2. **Visual polish on the split icon** — design review, not a code task.
3. **Storybook story for `HunkDiff` with `isSubHunk: true`** — UI-only.
4. **Stage state migration across split / unsplit (Option B)** — punt
   until users hit the dropped-stage case in practice.
5. **CLI parity** — punt; needs cross-process IPC or DB-only access
   path to the override store.
6. **Right-click "Split hunk before this line"** in
   `HunkContextMenu.svelte` — single-click 2-way split for keyboard
   accessibility.
7. **Right-click "Commit this line"** — composite shortcut; needs
   stack-target picker design.
8. **Doc updates** per the corrections list at the top of this file.

### Recommended next-session pickup

1. **Phase 5d** — Playwright happy-path covering drag→popover→Split
   →drag-to-stack→unsplit. Smoke-tested manually; spec is
   straightforward.
2. **Open partial-commit duplication issue** (see `⚠ Open` section
   above) — needs trace logging in `to_additive_hunks` and/or
   `safe_checkout`'s 3-way merge to confirm whether sub-hunk
   encoding emits overlapping null-side ranges. Higher priority
   than further phase-6 polish: this is a correctness bug, not a
   UX-polish gap.
3. **Phase 7** — committed-hunk splits + cross-stack moves of split
   pieces. Inherits the `sub_hunk_overrides` schema from 6.5 via a
   `schema_version=2` migration that widens the primary key to
   `(gitdir, commit_id, path, anchor_*)`.

## What landed in the phase 7a session

### Phase 7a — Generalize `SubHunkOverride` keying (shipped)

**Why:** Phase 7 widens the override store key from `(path, anchor)`
to a sum type that distinguishes worktree-anchored overrides from
overrides anchored to a hunk inside a specific commit's diff against
its parent. Landing the type ahead of any commit-side functionality
means 7b/7c are purely additive — no key-shape churn during the
gnarly rebase / cross-stack move work.

**What landed:**

- New `pub enum SubHunkOriginLocation` in
  `crates/but-hunk-assignment/src/sub_hunk.rs`:
  - `Worktree { path: BString }` — implied by every public API today.
  - `Commit { id: gix::ObjectId, path: BString }` — defined but not
    yet constructed; 7c adds the `split_hunk` variant that emits it.
  - Derives `Hash + Eq + Ord + Clone + Serialize + Deserialize` plus
    accessors `path()`, `commit_id()`, `is_worktree()` and constructors
    `worktree(path)` / `commit(id, path)`.
- Process-wide store key widened from `(BString, HunkHeader)` to
  `(SubHunkOriginLocation, HunkHeader)` via a new
  `type StoreKey = (SubHunkOriginLocation, HunkHeader)`. Two
  internal helpers thread the worktree variant through every
  existing key construction site:
  - `worktree_key(path, anchor)` for callers that already have
    `(path, anchor)`.
  - `key_for_override(ov)` for callers that have a `SubHunkOverride`
    in hand. This is the integration point that 7b/c will widen
    once `SubHunkOverride` itself gains an explicit `origin` field.
- All existing public functions kept their signatures so caller code
  in `but-api/src/diff.rs` (and the desktop frontend invocation
  shape) is unchanged:
  - `upsert_override`, `get_override`, `remove_override`
  - `drop_overrides`, `migrate_stored_override`,
    `migrate_stored_override_multi`
  - the `_persistent` variants and `reconcile_with_overrides{,_persistent}`
- `reconcile_with_overrides_persistent` carries a Phase 7a comment
  documenting that its `(BString, HunkHeader)`-keyed `disk_keyed` /
  `mem_keyed` joins are intentionally narrow today: the
  `sub_hunk_overrides` table has no `commit_id` column (added in
  7c via a `schema_version=2` migration), and nothing constructs
  `Commit`-shaped in-memory keys yet. 7c widens both sides here in
  one motion.

**Tests** (3 new, total 80 in `but-hunk-assignment`):

- `origin_location_worktree_and_commit_have_distinct_keys` — Worktree
  and Commit variants on the same `(path, anchor)` produce distinct
  HashMap entries.
- `origin_location_serde_round_trip` — both variants round-trip
  losslessly through `serde_json` (worktree path bytes preserved
  via `BString`'s native serde; commit id via the
  `but_serde::object_id` hex module).
- `worktree_keyed_overrides_round_trip_through_store` — the
  `(path, anchor)` public APIs (`upsert_override` / `get_override` /
  `remove_override`) still work end-to-end through the new keying,
  pinning that 7a is observably a no-op on the worktree path.

### What's intentionally *not* shipped this session (deferred to 7b–g)

- **`SubHunkOverride` does not yet carry `origin: SubHunkOriginLocation`.**
  Adding the field touches ~30 construction sites in the test suite
  plus the bridge code; deferring to 7b/c keeps 7a a tight diff and
  lets the field be introduced alongside the first real use of the
  `Commit` variant.
- **No DB schema changes.** The `sub_hunk_overrides` table still
  matches the v1 (Phase 6.5b) shape. A `schema_version=2` migration
  adding nullable `commit_id BLOB` is part of 7c.
- **No commit-diff rendering, no commit-side `split_hunk` RPC, no
  cross-stack move of sub-hunks.** Those are 7b, 7c, 7d respectively.

### Recommended next-session pickup

1. **Phase 7b** — render committed hunks through a
   `reconcile_with_overrides`-style pass on commit diffs. Requires
   identifying the desktop's per-commit hunk fetch path (likely in
   `crates/but-api/src/commit/...` or wherever the diff for a
   selected commit is requested) and threading the override pass
   through it. Until 7c lands the `Commit`-keyed `split_hunk`,
   this is observably a no-op — but it gates 7c's frontend work.
2. **Phase 7c** — `SubHunkOverride::origin` field + commit-keyed
   `split_hunk` RPC + DB schema v2 migration adding `commit_id BLOB`.
   This is the bulk of phase 7's backend.
3. The open partial-commit duplication bug (still unresolved) is
   independent of phase 7 keying and worth interleaving whenever
   the trace-driven diagnosis is convenient.

## What landed in the phase 7b session (backend foundation)

### Phase 7b — Commit-anchored override query helpers + reconcile isolation (shipped)

**Why this exists.** Phase 7a widened the in-memory store key to
`(SubHunkOriginLocation, HunkHeader)`, but the surrounding read paths
(`list_overrides`) and the worktree reconcile (`reconcile_with_overrides`)
still treated every entry uniformly. As soon as Phase 7c starts
emitting `Commit { id, path }`-keyed overrides, the worktree reconcile
would have called `apply_overrides_to_assignments` on them — finding no
matching `path`/`anchor` in the worktree-derived `assignments` and
either dropping or migrating them against worktree shape. That's
silent corruption: the user splits a commit's hunk in 7c, then the
next worktree refresh would erase the split state.

7b closes that latent bug and provides the public API surface 7c will
consume.

**What landed.** All in `crates/but-hunk-assignment/src/sub_hunk.rs`:

- New origin-aware list helpers:
  - `list_worktree_overrides(gitdir) -> Vec<SubHunkOverride>`
  - `list_commit_overrides(gitdir, commit_id) -> Vec<SubHunkOverride>`
  - Internal `list_overrides_filtered(gitdir, predicate)`. The
    pre-existing `list_overrides` is preserved as the unfiltered
    diagnostic variant.
- New write helper `upsert_override_at(gitdir, location, ov)` that
  takes an explicit `SubHunkOriginLocation`. The pre-existing
  `upsert_override` keeps its signature and routes through this with
  `SubHunkOriginLocation::worktree(...)`. A `debug_assert_eq!`
  enforces that `location.path() == ov.path` so a future caller
  can't desynchronize them.
- New read helper `get_commit_override(gitdir, commit_id, path,
  anchor) -> Option<SubHunkOverride>` that builds a `Commit`-shaped
  key. Returns `None` for any input today because nothing constructs
  `Commit`-keyed overrides yet (that's 7c).
- `reconcile_with_overrides` now reads `list_worktree_overrides` so
  commit-anchored overrides are excluded from worktree-shape
  reconciliation. Inline comment documents the contract.
- `reconcile_with_overrides_persistent`'s memory-side join also
  filters to worktree-only via `list_worktree_overrides`. The
  disk-side join stays restricted to whatever the
  `sub_hunk_overrides` table contains, which is worktree-only until
  the 7c schema bump (`commit_id BLOB` column, `schema_version=2`).

**What's *not* in 7b** (deferred to 7c):

- `SubHunkOverride` does *not* yet carry an explicit `origin` field.
  Adding it touches ~10 construction sites and ~40 read sites; 7c
  introduces it alongside the first real `Commit` constructor. Until
  then, the keying is final but the value's "self-knowledge" of
  where it's anchored is implicit in the store key.
- No new RPC. The desktop's per-commit hunk fetch path
  (`tree_change_diffs`) is unchanged. 7c adds the commit-scoped
  variant that runs the override pass.
- DB schema unchanged. v2 (`commit_id BLOB` nullable) is part of 7c.

**Tests** (3 new, total 83 in `but-hunk-assignment`):

- `commit_anchored_overrides_are_isolated_from_worktree_lookups` —
  insert one Worktree-keyed and one Commit-keyed override on the
  same `(path, anchor)`; confirm they coexist; `get_override` /
  `get_commit_override` / `list_worktree_overrides` /
  `list_commit_overrides` all filter correctly; the unfiltered
  `list_overrides` still sees both.
- `worktree_reconcile_does_not_drop_commit_anchored_overrides` —
  insert a Commit-keyed override; run `reconcile_with_overrides`
  against an empty assignments list (the case that *would* have
  silently corrupted state pre-7b); confirm the commit-keyed
  override is still present, and a second reconcile remains a no-op
  for the commit side.
- `upsert_override_at_debug_asserts_path_consistency` — happy-path
  cover for the new `upsert_override_at` constructor with matching
  location/`ov` paths.

### Recommended next-session pickup

1. **Phase 7c** — the bulk of phase 7's backend:
   - Add `pub origin: SubHunkOriginLocation` to `SubHunkOverride`,
     update the ~10 construction sites, retire the
     `key_for_override` "deferred refactor" comment.
   - Add the `commit_id BLOB` column + `schema_version=2` migration
     to `sub_hunk_overrides`. `to_db_row` / `from_db_row` widen to
     read/write it; `MAX_OVERRIDE_DB_BYTES` size guard unchanged.
   - Add `split_hunk_in_commit(commit_id, path, anchor, ranges)`
     RPC mirroring `split_hunk` but routing through
     `upsert_override_at`. Tauri allowlist + napi entry.
   - Add `tree_change_diffs_in_commit(commit_id, change)` RPC that
     runs an override pass on the unified patch (commit-side analog
     of `reconcile_with_overrides`).
   - Frontend: thread `commitId` through the diff view's RPC choice
     so commit-diff views call the new variant. Reuse
     `splitDiffHunkByHeaders` unchanged.
2. **Phase 7d** — `move_sub_hunk` / `uncommit_sub_hunk` RPCs that
   wrap `move_changes_between_commits` / `uncommit_changes` for
   sub-ranges, encoding via the existing `encode_sub_hunk_for_commit`.
3. The open partial-commit duplication bug remains independent of
   phase 7 keying and worth interleaving whenever convenient.

## What landed in the phase 7c-1 session

### Phase 7c-1 — `SubHunkOverride.origin` field

**Why this exists.** Phase 7a/7b widened the in-memory store key to
`(SubHunkOriginLocation, HunkHeader)` but kept the override **value**
ignorant of where it was anchored — `key_for_override(ov)`
unconditionally constructed `Worktree { path: ov.path.clone() }`.
That meant any code holding a `SubHunkOverride` *outside* the store
(migration helpers, the upcoming 7d `move_sub_hunk` RPC, future
disk-row bridging) couldn't tell whether it was looking at a
worktree- or commit-anchored override without knowing which key it
came from. The plan doc explicitly flagged this as 7b/c follow-up.

7c-1 closes that gap so 7c-2 (DB schema v2) and 7c-3+ (commit-side
RPCs) can be written cleanly.

**Changes** (all in `crates/but-hunk-assignment/src/sub_hunk.rs`,
exported from `lib.rs`):

- `SubHunkOverride` gains `pub origin: SubHunkOriginLocation` with
  `#[serde(default = "...")]` so legacy snapshots without the field
  deserialize as a sentinel empty-path Worktree variant. The
  hydration / read path is responsible for filling in the real
  origin from the surrounding `(path, commit_id?)` context.
- `SubHunkOriginLocation` gets an `impl Default` (empty-path
  Worktree) and a `default_for_serde` helper used as the field's
  serde default.
- Invariant: `origin.path() == &self.path`. Documented inline; the
  redundant `path` field is retained for backward compat with the
  ~40 read sites that access `ov.path` directly. A future cleanup
  may replace `path` with a `pub fn path(&self) -> &BString`
  accessor.
- `key_for_override(ov)` now reads `ov.origin` directly. The
  worktree-only shim from 7a/7b is retired.
- `upsert_override(gitdir, ov)` reads `ov.origin` as authoritative
  (no more silent worktree assumption).
- `upsert_override_at(gitdir, location, mut ov)` overwrites
  `ov.origin = location.clone()` before storing, so callers can
  hand in a stale-origin `ov` and the stored value's `origin`
  always matches its key.
- All 9 `SubHunkOverride { ... }` construction sites updated to
  populate `origin`:
  - migration paths (`migrate_override_multi`, `migrate_override`)
    inherit `ov.origin.clone()` from the source — a worktree-side
    migration stays worktree-shaped, a commit-side migration
    (Phase 7f) stays commit-shaped.
  - `from_db_row` synthesizes `Worktree { path }` because v1 rows
    have no `commit_id` column. Phase 7c-2 reads the new column
    directly.
  - test helpers + `but-api/src/diff.rs::split_hunk_with_perm`
    construct `Worktree { path }` explicitly.
- New public exports: `SubHunkOriginLocation`, `upsert_override_at`,
  `list_worktree_overrides`, `list_commit_overrides`,
  `get_commit_override`.

**Bug fix flushed out by this session.** The pre-existing test
`reconcile_with_overrides_prunes_stale_entries` constructed an
override via `make_stored_override` (path "foo.rs") then mutated
`stale.path = "missing.rs"` post-construction. Pre-7c-1 the store
key derived from `ov.path`, so the mutation moved the entry's key
along with the field. Post-7c-1 the key derives from `ov.origin`,
so the bare `path` mutation desynchronized the key from the value.
The test now updates both `path` and `origin` together. This
exposes a coherent contract: callers that mutate `ov.path` directly
must also update `ov.origin` (or use `upsert_override_at` which
takes the location authoritatively). The struct-level
`debug_assert_eq!` in `upsert_override_at` enforces it.

**Tests** (3 new, total 86 in `but-hunk-assignment`):

- `upsert_override_routes_through_origin_field` — set `ov.origin`
  to a `Commit { id, path }` shape and call the bare
  `upsert_override`; the entry lands under the commit-keyed slot,
  not under the worktree-keyed one implied by `ov.path`.
- `upsert_override_at_overrides_origin_field_for_storage` — pass a
  `Worktree`-origin `ov` plus a `Commit { id }` location to
  `upsert_override_at`; verify `stored.origin == location` after
  retrieval.
- `sub_hunk_override_serde_default_origin_for_legacy_snapshots` —
  parse a legacy in-memory snapshot JSON without the `origin`
  field; verify `#[serde(default)]` fires and the field is filled
  with the empty-path Worktree sentinel.

### Pre-existing test drift (unrelated)

`cargo test --workspace` shows 11 failures in `crates/but/src/id/tests.rs`
(insta snapshot whitespace drift on `sub_hunk_origin: None,` rows).
Confirmed identical on `master` pre-session via `git stash` + retest.
Not caused by 7c-1; flagged here so it doesn't get attributed to a
later phase. A snapshot review (`cargo insta review` in `crates/but`)
clears them.

### Recommended next-session pickup

1. **Phase 7c-2** — `but-db` `sub_hunk_overrides` schema v2:
   - Add `commit_id BLOB` column (nullable; null \u2261 worktree).
   - Bump `OVERRIDE_DB_SCHEMA_VERSION` to 2 with a Diesel migration
     under `crates/but-db/migrations/`.
   - Update `to_db_row` / `from_db_row` to read/write the column;
     `from_db_row` builds the right `SubHunkOriginLocation` variant
     directly instead of always Worktree.
   - The reconcile join in `reconcile_with_overrides_persistent`
     widens both sides to the new key shape (the pre-existing
     comment in that function documents this is the planned
     widening point).
   - Add tests for v1 \u2192 v2 row migration on hydration (existing
     v1 rows hydrate as Worktree-keyed; v2 rows with non-null
     `commit_id` hydrate as Commit-keyed).
2. **Phase 7c-3** — `split_hunk_in_commit` + `unsplit_hunk_in_commit`
   RPCs in `crates/but-api/src/diff.rs`. Same shape as the worktree
   variants but routing through `upsert_override_at` /
   `remove_override` with a `Commit { id }` location. Tauri
   allowlist + napi entries; frontend type stubs.
3. **Phase 7c-4** — commit-diff override-aware RPC. The desktop's
   `tree_change_diffs` returns a `UnifiedPatch`; add a
   `tree_change_diffs_in_commit(commit_id, change)` that wraps the
   patch's hunks through a commit-side override-materialization
   pass so sub-hunk boundaries appear in the rendered diff.
4. **Phase 7c-5** — frontend: thread `commitId` through the diff
   view's RPC choice; reuse `splitDiffHunkByHeaders` unchanged.

## What landed in the phase 7c-2 session

### Phase 7c-2 — `sub_hunk_overrides` schema v2 (commit_id column)

**Why this exists.** Phase 7a/7b widened the in-memory store key to
`(SubHunkOriginLocation, HunkHeader)` and 7c-1 added `origin` to the
`SubHunkOverride` value, but the on-disk shape was still v1: a single
`(gitdir, path, anchor_*)` PK that could only hold one override per
`(path, anchor)` regardless of origin. As soon as 7c-3 emits
`Commit { id, path }`-keyed overrides via the commit-side `split_hunk`
RPC, persistence would silently collapse worktree- and commit-keyed
overrides into the same row.

**Changes in `crates/but-db`** (`table/sub_hunk_overrides.rs`):

- New M::up migration `20260501120000` that recreates the table
  with `commit_id BLOB NOT NULL DEFAULT X''` added to the primary
  key. Existing v1 rows are backfilled with `X''` (= worktree).
  Strategy mirrors the existing `worktrees` rebuild migrations
  (create new table, INSERT…SELECT old, DROP old, RENAME new → old).
  Tagged `SchemaVersion::Zero` because old binaries that don't know
  about line-by-line commits never touch this table; and binaries
  that do know read everything they need via the same
  `SELECT_COLUMNS` (which just gained the column) and emit upserts
  with the new shape.
- `SubHunkOverrideRow` gains `pub commit_id: Vec<u8>` with
  `#[serde(default)]` for backward-compat against in-memory
  snapshots that may pre-date 7c-2.
- `SELECT_COLUMNS` and `map_row` widened.
- `get` and `delete` now take a `commit_id: &[u8]` parameter.
- `upsert` plumbs `row.commit_id` through both the INSERT clause
  and the ON CONFLICT predicate.

**Changes in `crates/but-hunk-assignment`** (`sub_hunk.rs`):

- `OVERRIDE_DB_SCHEMA_VERSION` bumped from `1` to `2`. The
  `from_db_row` schema-version check now rejects anything not
  exactly `2` (consistent with the existing forward-incompat
  policy).
- `to_db_row` encodes `ov.origin`'s commit id into the row's
  `commit_id` field: empty `Vec::new()` for `Worktree`, the OID's
  raw bytes (sha1-20 / sha256-32) for `Commit`.
- `from_db_row` decodes: empty → `SubHunkOriginLocation::worktree`,
  non-empty → `gix::ObjectId::try_from(bytes)` →
  `SubHunkOriginLocation::commit`. Parse failures bubble up as
  errors so corrupt rows surface loudly rather than getting
  silently coerced to worktree.
- New helper `origin_commit_id_bytes(origin) -> Vec<u8>` for the
  size-guard delete path that doesn't have a `to_db_row` candidate
  to pull from.
- `upsert_override_persistent` size-guard delete and all five
  `.delete()` callers updated to pass `commit_id`. Worktree-only
  paths (`remove_override_persistent`, `drop_overrides_persistent`,
  the `reconcile_with_overrides_persistent` join) hard-code
  `&[]`. The reconcile pass now filters disk rows to
  `commit_id.is_empty()` so a future commit-anchored row on disk
  doesn't get incorrectly deleted as part of a worktree reconcile.

**Tests** (3 new — 1 in `but-db`, 2 in `but-hunk-assignment`):

- `but-db`: `primary_key_distinguishes_commit_id` — insert a
  worktree row (`commit_id = b""`) and a commit row (sentinel
  20-byte blob) on the same `(path, anchor)`; verify both coexist;
  `get` and `delete` discriminate correctly by commit_id.
- `but-hunk-assignment`:
  `commit_keyed_override_round_trips_through_db_bridge` — build
  a `Commit { id, path }`-origin override, run it through
  `to_db_row` → `from_db_row`, verify the origin survives and
  `row.commit_id == id.as_bytes()`.
- `but-hunk-assignment`:
  `worktree_keyed_override_encodes_empty_commit_id` — symmetric
  case: worktree-anchored overrides encode an empty `commit_id`
  blob and decode back to the `Worktree` variant.
- Plus updates to existing snapshot tests in `but-db`'s
  `migration::run::run_ours` to reflect the new table shape and
  the new migration timestamp.

**Test totals after this session:** `but-db` 133, `but-hunk-assignment`
88 — all green; workspace builds clean (the pre-existing `crates/but`
insta drift on `sub_hunk_origin` indentation is still present and
still independent).

### Recommended next-session pickup

1. **Phase 7c-3** — `split_hunk_in_commit` and
   `unsplit_hunk_in_commit` RPCs in `crates/but-api/src/diff.rs`.
   Same shape as the worktree variants but routing through
   `upsert_override_at` /
   `remove_override` (a new variant of the latter that takes a
   `SubHunkOriginLocation`) with a `Commit { id }` location.
   Tauri allowlist + napi entries + frontend type stubs.
2. **Phase 7c-4** — commit-diff override-aware RPC. The desktop's
   `tree_change_diffs` returns a `UnifiedPatch`; add a
   `tree_change_diffs_in_commit(commit_id, change)` that wraps the
   patch's hunks through a commit-side override-materialization
   pass so sub-hunk boundaries appear in the rendered diff.
3. **Phase 7c-5** — frontend: thread `commitId` through the diff
   view's RPC choice; reuse `splitDiffHunkByHeaders` unchanged.
4. Phase 7d / 7f remain untouched (move + uncommit of sub-hunks,
   override migration on commit rewrite). 7c-3..5 are the path
   to user-visible commit-side splits.

## What landed in the phase 7c-3 session

### Phase 7c-3 — Commit-side `split_hunk` / `unsplit_hunk` RPCs

**Why this exists.** Phases 7a/7b/7c-1/7c-2 made the in-memory store
and on-disk schema commit-aware, but no public API was yet capable of
*creating* a `Commit { id, path }`-keyed override. 7c-3 adds the two
RPCs that let a commit-diff view register a sub-hunk split.

**Changes in `crates/but-hunk-assignment`** (`sub_hunk.rs`):

- New `pub fn remove_override_at(gitdir, location, anchor)` — generic
  in-memory remove keyed by `SubHunkOriginLocation`. Worktree variant
  `remove_override` now delegates to it.
- New `pub fn remove_override_persistent_at(db, gitdir, location, anchor)`
  — disk write-through analog. Encodes the location's commit id via
  `origin_commit_id_bytes` for the `delete` row predicate.
- `lib.rs` re-exports the two new helpers.

**Changes in `crates/but-api`** (`src/diff.rs`):

- `split_hunk_in_commit(ctx, commit_id, path, anchor, ranges)` plus
  `_with_perm` variant. Mirrors `split_hunk` but resolves the anchor
  against `CommitDetails::from_commit_id(commit_id, line_stats=No)`
  's `diff_with_first_parent` (the commit's diff against its first
  parent) instead of the worktree. Re-split semantics work the same
  way: an existing commit-keyed override on `(commit, path, anchor)`
  has its `ranges` partition refined via
  `merge_user_ranges_into_partition`. The override is persisted via
  the existing `upsert_override_persistent` (which already encodes
  origin into `commit_id` on the row through `to_db_row`).
- `unsplit_hunk_in_commit(ctx, commit_id, path, anchor)` plus
  `_with_perm` variant. Routes through
  `remove_override_persistent_at` with a `Commit { id, path }`
  location.
- Both RPCs `#[but_api(napi)]`-decorated and `#[instrument]`-traced
  to mirror the worktree variants' shape.
- Both RPCs use `RepoExclusive` permission scope (DB write-through
  + future commit-tree work in 7d).

**Changes in `crates/gitbutler-tauri`**:

- `permissions/default.toml`: added `"split_hunk_in_commit"` and
  `"unsplit_hunk_in_commit"` to the allowlist (sibling of
  `"split_hunk"`).
- `main.rs`: registered
  `diff::tauri_split_hunk_in_commit::split_hunk_in_commit` and
  `diff::tauri_unsplit_hunk_in_commit::unsplit_hunk_in_commit` in
  the `tauri::generate_handler!` invoke list.

**Tests** (1 new, total 89 in `but-hunk-assignment`):

- `remove_override_persistent_at_clears_commit_keyed_row` —
  end-to-end coverage of the new write/read symmetry: write a
  Commit-keyed override via `upsert_override_persistent`, confirm
  both in-memory and disk see it; verify the worktree-side
  `remove_override_persistent` does *not* touch the commit-keyed
  row (the origin isolation introduced by 7b is preserved across
  the persistent helpers); finally clear via the new `_at` helper
  and confirm both layers are empty.

**Test totals after this session**: `but-hunk-assignment` 89,
`but-db` 134 (carried from 7c-2), all green; workspace builds clean.

### What's intentionally *not* shipped this session (deferred to 7c-4 / 7c-5)

- **No commit-diff override-aware diff RPC.** The desktop's
  `tree_change_diffs(change)` doesn't know which commit the change
  came from, so even after `split_hunk_in_commit` lands an override
  on disk, the commit-diff UI still renders the natural hunk. 7c-4
  adds `tree_change_diffs_in_commit(commit_id, change)` that runs a
  commit-side override-materialization pass on the patch's hunks
  before returning.
- **No frontend wiring.** 7c-5 threads `commitId` through the
  diff-view RPC choice; reuses `splitDiffHunkByHeaders` unchanged.

### Recommended next-session pickup

1. **Phase 7c-4** — `tree_change_diffs_in_commit` RPC. The
   override-materialization pass for commit diffs reuses
   `materialize_override` semantics from `sub_hunk.rs`, but emits
   results into a `UnifiedPatch` rather than `HunkAssignment` rows.
   Probably easiest to add a parallel
   `materialize_override_into_patch(patch, override) -> UnifiedPatch`
   helper rather than retro-fitting the worktree path.
2. **Phase 7c-5** — frontend: in `apps/desktop/src/lib/worktree/
   worktreeEndpoints.ts` (or a new `commitEndpoints.ts`), add
   a `getCommitDiff` query that calls the new RPC, plus
   `splitHunkInCommit` / `unsplitHunkInCommit` mutations. The
   `UnifiedDiffView` for commit views switches to the new endpoints
   when `commitId` is non-null. Reuse `splitDiffHunkByHeaders`.
3. **Phase 7d** — `move_sub_hunk` / `uncommit_sub_hunk`: now that
   commit-side overrides can be created and persisted, the next
   workflow is "drag a commit's sub-hunk to another commit / back
   to the worktree". Wraps `move_changes_between_commits` /
   `uncommit_changes` for sub-ranges via
   `encode_sub_hunk_for_commit`.
4. **Phase 7f** — override migration on commit rewrite. When a
   source commit gets rewritten by 7d's move flow, the commit-keyed
   override on the source needs to migrate to the rewritten commit
   id (via the same content-match logic Phase 4.5 used for the
   worktree case).

## What landed in the phase 7c-4 session

### Phase 7c-4 — Commit-diff override-aware diff RPC

**Why this exists.** Phase 7c-3 made it possible to *create* a
`Commit { id, path }`-keyed override via `split_hunk_in_commit`, but
the desktop's per-commit diff fetch path (`tree_change_diffs(change)`)
doesn't know which commit the change belongs to and so couldn't apply
the override on read. 7c-4 adds the parallel RPC that takes
`commit_id` explicitly and runs an override-materialization pass on
the resulting unified patch.

**Why backend-side materialization (not frontend).** For the worktree
case, sub-hunks come through `HunkAssignment` rows whose synthesized
`HunkHeader`s the frontend feeds into `splitDiffHunkByHeaders` to
slice the natural diff text. The commit-diff API has no such
`HunkAssignment` channel — the unified patch is the whole API
surface. Easiest path: have the backend return a `UnifiedPatch` whose
`hunks` already include the sub-hunks as if they were natural
multi-hunk patches. The frontend renders them with no special-case
logic.

**Changes in `crates/but-hunk-assignment`** (`sub_hunk.rs`):

- New `pub fn apply_commit_overrides_to_patch(patch, overrides) ->
  UnifiedPatch`. For each natural hunk in `patch`, if any override
  in `overrides` has a matching anchor (exact `HunkHeader` equality),
  replace the hunk with N sub-hunks built from `override.ranges`.
  Each sub-hunk's `diff` is `<synthesized @@ header>\n<body slice>`,
  built from `synthesize_header` + `sub_diff_body`. Hunks without a
  match pass through unchanged. Binary / TooLarge patches pass
  through unchanged. `lines_added` / `lines_removed` are preserved
  (splitting doesn't change row totals).
- Defensive: skips `range.is_empty()` and empty-body sub-hunks. The
  override's stored ranges should already be non-empty post-upsert,
  but the helper is reachable from any caller.
- Exported from `lib.rs`.

**Changes in `crates/but-api`** (`src/diff.rs`):

- New RPC `tree_change_diffs_in_commit(ctx, commit_id, change) ->
  Option<UnifiedPatch>`. Same shape as `tree_change_diffs` but takes
  `commit_id` explicitly. Calls `change.unified_patch` to compute
  the natural patch, runs `ensure_hydrated` (Phase 6.5c) so
  persisted commit-keyed overrides land in memory if this is the
  first read after relaunch, then filters
  `list_commit_overrides(gitdir, commit_id)` by `path` and applies
  `apply_commit_overrides_to_patch`.
- The override list is sorted by `(anchor.new_start, anchor.new_lines)`
  before application so the materialized result is deterministic
  across calls.
- `#[but_api(napi)]` + `#[instrument(skip_all)]` decorated to
  mirror existing patterns. Read-only context (`ctx.workspace_and_db`).

**Changes in `crates/gitbutler-tauri`**:

- `permissions/default.toml`: added `"tree_change_diffs_in_commit"`
  to the allowlist.
- `main.rs`: registered
  `diff::tauri_tree_change_diffs_in_commit::tree_change_diffs_in_commit`
  in the `tauri::generate_handler!` invoke list.

**Tests** (4 new, total 93 in `but-hunk-assignment`):

- `apply_commit_overrides_passes_unmatched_hunks_through` — patch
  with one hunk and no overrides comes back bit-identical.
- `apply_commit_overrides_passes_binary_through` — `UnifiedPatch::Binary`
  short-circuits.
- `apply_commit_overrides_replaces_matching_hunk_with_sub_hunks` —
  6-row anchor (ctx -r +a -r +a ctx) with a user-pick of rows 2..4
  produces three sub-hunks (leading residual + user + trailing
  residual). Each sub-hunk's `diff` starts with its own `@@`
  header and the header numbers match the field set on the
  `DiffHunk`.
- `apply_commit_overrides_skips_overrides_for_other_hunks` — patch
  with two natural hunks where the override matches only one; the
  other passes through unchanged.

**Test totals after this session**: `but-hunk-assignment` 93,
`but-db` 134, `but-api` builds clean. Workspace builds clean.

### What's *not* shipped this session (deferred to 7c-5)

- **No frontend wiring.** The desktop still calls `tree_change_diffs`
  for every diff fetch. 7c-5 threads `commitId` through the diff
  view's RPC choice and reuses the existing render path; no diff
  splitting on the frontend is needed because the backend now
  returns sub-hunks as if they were natural hunks.

### Recommended next-session pickup

1. **Phase 7c-5** — frontend RPC routing. Add a
   `getDiffInCommit({projectId, commitId, change})` query in
   `apps/desktop/src/lib/worktree/worktreeEndpoints.ts` (or a new
   `commitEndpoints.ts`) targeting the `tree_change_diffs_in_commit`
   command. `splitHunkInCommit` / `unsplitHunkInCommit` mutations
   the same. Where the diff view is invoked with a `commitId`,
   call the new endpoint instead of `getDiff`. The split-icon /
   sub-hunk re-split UI already exists from Phase 4–5 and works
   off the `DiffHunk` shape, so once the backend returns sub-hunks
   the icon + drag affordances should light up automatically. The
   only additional UI work is wiring the popover's `Split` action
   to `splitHunkInCommit` when in a commit view.
2. **Phase 7d** — `move_sub_hunk` / `uncommit_sub_hunk` RPCs.
3. **Phase 7f** — override migration on commit rewrite.
