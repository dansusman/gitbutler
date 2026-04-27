# `safe_checkout` worktree duplication on partial commits

Status: **fixed** (Option A landed). Tracks the "Hypothesis #2" leftover
from the `⚠ Open issue` section in
[`docs/line-by-line-commits-plan.md`](./line-by-line-commits-plan.md).

The `KeepAndPreferTheirs` arm of
`crates/but-core/src/worktree/checkout/utils.rs::merge_worktree_changes_into_destination_or_keep_snapshot`
now bypasses `merge_trees` and directly overlays the snapshot's
worktree blobs onto the destination tree for each selected path. Pinned
by the unit test
`crates/but-core/tests/core/worktree/checkout.rs::worktree::checkout::keep_and_prefer_theirs_does_not_duplicate_overlapping_additions`,
which reproduces the duplication shape (theirs has a Section X prefix
that shifts theirs' Section A insert to a different line offset than
ours') and asserts the worktree is byte-identical to theirs after
safe_checkout. Pre-fix that test fails with Section A duplicated; post
fix the worktree is unchanged.

## TL;DR

After a successful sub-hunk amend the **commit's tree is correct** but the
**worktree gets the just-committed rows duplicated**. Root cause is in
`crates/but-core/src/worktree/checkout/utils.rs::safe_checkout`'s 3-way
merge: `base = pre-commit HEAD`, `ours = post-commit HEAD`, `theirs =
current worktree` — gix's tree-merge doesn't recognize "ours and theirs
both add the same content at compatible-but-different positions" as the
same change, so it applies **both** adds.

`KeepAndPreferTheirs` only flips `FileFavor::Theirs` on conflicts gix
*detects*. For non-conflicting overlapping additions there's no
`FileFavor` decision to make and the merge over-merges silently.

## Concrete repro (verified in the running dev app)

State of `~/buttest`'s `iuhiuh` stack after a clean run on the
`fa75ca4c3e` binary:

- `iuhiuh` tip `c31ddb0` (the just-amended `fdfdfdf` commit): correct.
  ```
  
  ## Section A
  - alpha line one
  - alpha line two
  - alpha line three
  
  ## Section B
  - beta line one
  - beta line two
  - beta line three
  
  ```
  10 rows, Section A above Section B, exactly what the user dragged.

- `~/buttest/splittest_pure_add.md` worktree: **Section A duplicated**.
  ```
  # split test — pure add

  this whole file is an uncommitted new file, so its diff is one big
  pure-add hunk. try splitting it row-by-row from the dev console.

  ## Section A
  - alpha line one
  - alpha line two
  - alpha line three

  ## Section A          ← duplicate, came from safe_checkout merge
  - alpha line one
  - alpha line two
  - alpha line three

  ## Section B
  - beta line one
  - beta line two
  - beta line three

  ## Section C
  - gamma line one
  - gamma line two
  - gamma line three
  ```

### Sequence

```
1. fresh clean state: HEAD has retest (Section B only); worktree has full A/B/C.
2. drag Section B sub-hunk → "retest"
   commit_amend  hunk_headers=[(-0,0 +10,6)]   →  retest tree gets Section B
   safe_checkout  conflicting_worktree_changes_opts=KeepAndPreferTheirs
3. drag Section A sub-hunk → "fdfdfdf"
   commit_amend  hunk_headers=[(-0,0 +5,5)]    →  fdfdfdf tree gets Section A
   safe_checkout  conflicting_worktree_changes_opts=KeepAndPreferTheirs   ← BUG fires here
4. observe: worktree now has Section A duplicated (rows 6-9 + 11-14).
   iuhiuh's tree is still correct (Section A appears once at the right place).
```

No `commit_uncommit_changes` involved (an earlier user report had a stray
`uncommit` call from accidentally hitting `UncommitDzHandler` — that's a
separate UX issue, not this bug).

### Log signature

In `/tmp/gitbutler-dev.log` the bug shows up as a sequence of:

```
commit_amend → safe_checkout(...conflicting_worktree_changes_opts=KeepAndPreferTheirs)
              → worktree_changes_no_renames
              → create_tree
              → checkout/function.rs close
```

…with no error, just a silently-duplicated worktree blob afterwards. The
post-amend `changes_in_worktree` reconcile then sees the worktree's new
shape (e.g. anchor `(-1,6 +1,24)` instead of the expected `(-1,10 +1,19)`)
and migrates the override accordingly — locking in the duplication.

## What we already fixed (recap)

`docs/line-by-line-commits-plan.md`'s `⚠ Open issue` enumerated three
hypotheses. Two are now closed by today's commits:

- **Hypothesis #1** (`encode_sub_hunk_for_commit` overlapping null-side
  ranges): fixed by `7f0b3bf399` (running pure-add/pure-remove offset
  totals) + `fa75ca4c3e` (clamp to wh's row budget). Unit tests in
  `crates/but-core/src/hunks.rs::test::apply_hunks_multi_pure_add` and
  integration tests in
  `crates/but-workspace/tests/workspace/commit_engine/amend_commit.rs`.
- **Hypothesis #3** (`From<HunkAssignment> for DiffSpec` emitting both
  natural anchor and encoded sub-range): not the cause; the `if/else if`
  branch in `crates/but-hunk-assignment/src/lib.rs:153` only emits one
  shape per call.

**Hypothesis #2** — `safe_checkout`'s 3-way merge over-merging when
`theirs` is a strict superset of `ours` — is the actual cause of the
remaining worktree duplication. **Not fixed.**

## Where the bug lives

`crates/but-core/src/worktree/checkout/utils.rs`, around the
`KeepAndPreferTheirs` branch (read for context, lines ~80–145):

```rust
let cherry_pick_options =
    if matches!(uncommitted_changes, UncommitedWorktreeChanges::KeepAndPreferTheirs) {
        let opts = repo_in_memory
            .tree_merge_options()?
            .with_file_favor(Some(gix::merge::tree::FileFavor::Theirs));
        Some(opts)
    } else {
        None
    };
let resolve = crate::snapshot::resolve_tree(
    out.snapshot_tree.attach(&repo_in_memory),
    destination_tree_id,
    snapshot::resolve_tree::Options {
        worktree_cherry_pick: cherry_pick_options,
    },
)?;
```

`resolve_tree` calls into gix's `merge_trees(base, ours, theirs, ...)`.
gix's text-line merge applies non-conflicting additions from both
sides. `FileFavor::Theirs` only resolves *conflicts gix detected*. For
"both sides add Section A at compatible positions" no conflict is
detected → both adds are applied → duplicate rows in the resulting
tree.

The resulting tree gets written to disk via `worktree_cherry_pick.tree`
in the `KeepAndPreferTheirs` arm, populating the worktree with the
duplicated content.

## Proposed fixes

### Option A — bypass the 3-way merge for `KeepAndPreferTheirs`

When the intent is "the worktree already has the right content,
including everything just committed; don't touch it", skip
`merge_trees` entirely and write `theirs` (the worktree blob) as-is.

```rust
// utils.rs near where cherry_pick_options is built
let resolve = if matches!(uncommitted_changes, UncommitedWorktreeChanges::KeepAndPreferTheirs) {
    // Worktree is the source of truth post-partial-commit. The commit
    // pipeline already wrote the canonical post-commit tree to HEAD;
    // the worktree itself doesn't need updating. Bypass merge_trees to
    // avoid gix's text-merge double-applying overlapping additions.
    snapshot::resolve_tree::theirs_only(...)?  // new helper
} else {
    crate::snapshot::resolve_tree(...standard path...)?
};
```

**Risk:** if there are *other* non-overlapping changes that
`merge_trees` was meant to bring in (e.g. a parallel rename on a
different file), bypassing the merge drops those. Need to confirm no
other code relies on `merge_trees` doing real work in the
`KeepAndPreferTheirs` path.

**Verification approach:** grep all callers of `safe_checkout` /
`UncommitedWorktreeChanges::KeepAndPreferTheirs`. They're the
post-commit-amend / post-partial-commit pipelines (`commit_amend`,
`commit_create`, etc., via `but-rebase::graph_rebase::materialize`).
For those, the worktree is intentionally a *superset* of the new HEAD
— the merge step is purely a "checkout the post-commit tree without
clobbering uncommitted changes" sanity check, not a real semantic
merge.

### Option B — pre-deduplicate `theirs` against `ours`

Before calling `merge_trees`, walk each conflicting file's blob and
strip out any rows already present in `ours` (the post-commit tree).
This preserves the rest of the merge logic.

**Risk:** much subtler. Row-level dedup needs to be content-aware (not
just textual line dedup) to avoid losing legitimate duplicate-content
edits the user actually wanted. Probably the wrong knob.

### Option C — feed gix's merge a stricter mode

gix exposes `FileFavor::Ours` / `Theirs` / `Union` — there may be a
mode (or a future addition) that says "drop additions from `theirs`
that already match `ours`". Worth checking the gix API for an
"ancestor-aware" merge driver.

**Risk:** depends on gix capability we don't currently have. Likely
needs an upstream change.

### Recommended path

**Option A** is the cleanest and matches the intent. The
`KeepAndPreferTheirs` arm is *exclusively* used by the post-commit
worktree-sync paths and the meaning is unambiguous: "I just wrote a new
HEAD tree; the worktree is already where I want it; don't change the
worktree's blobs". Bypassing the merge is structurally correct.

Sketch:

1. Refactor `snapshot::resolve_tree` (or add a sibling
   `resolve_tree_keep_theirs`) that, when invoked, writes `theirs` to
   the destination index without invoking `merge_trees`.
2. In `utils.rs`'s `KeepAndPreferTheirs` arm, route through that.
3. Keep `KeepAndAbortOnConflict` and
   `KeepConflictingInSnapshotAndOverwrite` on the existing
   `merge_trees` path — those have legitimate merge semantics.

## Tests

### Unit (in-process, no real workspace)

Add to `crates/but-core/src/worktree/checkout/` a focused test that:

1. Constructs `base`, `ours`, and `theirs` blobs where `ours` is `base
   + 5 rows added at position N`, and `theirs` is `base + 5 rows added
   at position N + 5 rows added at position M` (a strict superset
   covering the same `ours` rows plus more).
2. Invokes the `KeepAndPreferTheirs` path.
3. Asserts the resulting worktree blob is **bit-identical to
   `theirs`** — no duplication.

This pins the contract that `KeepAndPreferTheirs` doesn't mutate
`theirs`. Pre-fix the test should fail because the merged blob has
the `ours`-side adds duplicated.

### Integration (workspace level)

Mirror the user's exact field repro in
`crates/but-workspace/tests/workspace/commit_engine/amend_commit.rs`
alongside the existing
`amend_with_section_a_above_existing_section_b` and
`amend_then_amend_alpha_lines_after_section_a` tests:

```rust
#[test]
fn worktree_not_duplicated_after_partial_commit_amend() -> anyhow::Result<()> {
    // Setup: HEAD has Section B only; worktree has full A/B/C.
    // Amend Section A onto HEAD via DiffSpec hunk_headers=[(0,0,5,5)].
    // Assert:
    //   1. amended commit's tree contains Section A above Section B (10 rows).
    //   2. worktree's splittest.md is unchanged (still 19 rows, no duplicates).
}
```

Today's `amend_with_section_a_above_existing_section_b` only checks the
commit tree. The new test additionally checks the worktree file
content after the amend. Pre-fix this test fails on the worktree
assertion; post-fix both should pass.

Note: the existing integration tests use plain git scenarios
(`writable_scenario("unborn-untracked")`) and exercise
`commit_engine::create_commit` directly — they bypass
`safe_checkout` entirely. To exercise the bug, the test needs to
either:

- Drive a full `commit_amend` through
  `but-rebase::graph_rebase::materialize` (which calls
  `safe_checkout` with `KeepAndPreferTheirs`), or
- Test `safe_checkout` / `resolve_tree` directly with hand-crafted
  trees.

The first is closer to production but heavier; the second is a tighter
unit test. Worth doing both.

### Manual GUI verification

Once the fix lands and the dev app is rebuilt:

1. Clean `~/buttest`:
   - In the GUI, undo the buggy `iuhiuh` commits (or
     `git reset --hard` to a known clean state).
   - Restore `splittest_pure_add.md` to the clean A/B/C content (no
     duplicates):
     ```bash
     cat > ~/buttest/splittest_pure_add.md <<'EOF'
     # split test — pure add

     this whole file is an uncommitted new file, so its diff is one big
     pure-add hunk. try splitting it row-by-row from the dev console.

     ## Section A
     - alpha line one
     - alpha line two
     - alpha line three

     ## Section B
     - beta line one
     - beta line two
     - beta line three

     ## Section C
     - gamma line one
     - gamma line two
     - gamma line three
     EOF
     ```
2. In the dev app, on the `iuhiuh` stack:
   - Drag the Section B sub-hunk onto the "retest" commit row.
   - Drag the Section A sub-hunk onto the "fdfdfdf" commit row.
3. Inspect:
   - `git show iuhiuh:splittest_pure_add.md` → should be exactly the
     8-row "blank, Section A, alphas, blank, Section B, betas, blank"
     content (correct today).
   - `cat ~/buttest/splittest_pure_add.md` → should still be the
     19-row clean A/B/C file (no duplicates). **Today this is
     duplicated.**

If both look right, the fix is correct.

## Useful artifacts

- The dev app's log goes to `/tmp/gitbutler-dev.log`. Filter for
  `safe_checkout` and `KeepAndPreferTheirs` to see the post-commit
  flow:
  ```bash
  grep -E "safe_checkout|KeepAndPreferTheirs|commit_amend.*hunk_headers" /tmp/gitbutler-dev.log
  ```
- Project ID for `~/buttest`: `7ec0ca28-8920-422e-a425-3bd5fdfd50a1`.
- Branch under test: `iuhiuh`.
- Tip commit IDs from this session (for reference; they get rewritten
  every amend so don't rely on them):
  - `e66b23a` (pre-amend fdfdfdf)
  - `c31ddb0` (post-Section-A-amend fdfdfdf — has correct tree).

## Where to look in the code

| File | Why |
|---|---|
| `crates/but-core/src/worktree/checkout/utils.rs` | `safe_checkout`'s 3-way merge logic; `cherry_pick_options` selection. |
| `crates/but-core/src/worktree/checkout/mod.rs` | `UncommitedWorktreeChanges` enum (variant added in plan-doc Phase 5 fix). |
| `crates/but-core/src/snapshot/resolve_tree.rs` | The actual `merge_trees` invocation. |
| `crates/but-rebase/src/graph_rebase/materialize.rs` | Where `KeepAndPreferTheirs` is wired in (line ~56). |
| `crates/but-core/src/tree/mod.rs::to_additive_hunks` | Already-fixed half of the original `⚠ Open issue` — included for context, not the bug site. |
| `crates/but-core/src/hunks.rs::apply_hunks` | The other already-fixed half — context only. |

## Severity / shipping impact

- **Commit content is correct.** The line-by-line feature delivers the
  right blob into HEAD. Anyone reviewing the commits via
  `git show` / GitHub / pipelines sees the correct content.
- **Worktree gets visibly polluted** with duplicate sections after each
  partial commit. The next amend / split / commit sees the polluted
  worktree as the new natural diff and the dependency engine locks
  things appropriately, but the user has to manually un-pollute the
  file (e.g. revert from disk, edit out the duplicates).
- **Not a data-loss bug.** The user's pre-amend file content is
  recoverable: the duplicate is appended next to the original; the
  original isn't overwritten or destroyed.

This is bad enough to block "ship the line-by-line feature with this
known issue" framing, but not so bad that it's a corruption-class
emergency. Recommended pickup priority: high, before any further
line-by-line polish (Phase 6 items, etc.) — the rest of the feature is
correct and this is the only remaining structural bug.

## Related plan-doc sections

- `docs/line-by-line-commits-plan.md` → `⚠ Open issue` (now partially
  resolved per `7f0b3bf399` and `fa75ca4c3e`).
- `docs/line-by-line-commits-plan.md` → "Implementation Notes" item 4
  (`safe_checkout` overprotection on partial commits) — same code path,
  earlier fix added the `KeepAndPreferTheirs` variant; this issue is
  the *over-application* corollary.

## Quick start tomorrow

1. Read `crates/but-core/src/worktree/checkout/utils.rs:80-180`.
2. Pick Option A (bypass merge for `KeepAndPreferTheirs`).
3. Add the failing unit test against `safe_checkout` first to pin the
   bug.
4. Make the change, watch the test go green.
5. Add the workspace integration test to confirm worktree stays
   un-duplicated after a real amend.
6. Manually verify per the GUI recipe above.
7. Update `docs/line-by-line-commits-plan.md`'s `⚠ Open issue` section
   to mark the worktree-side fix as shipped.
