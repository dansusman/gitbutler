# Locks-on-residuals reproducer

Goal: figure out whether the `🔒` icons that decorate residual sub-hunks
after a partial commit are inherent to `but-hunk-dependency` (because
the file exists in HEAD only as a result of the commit on this stack)
or whether the dependency analysis is being too aggressive even for
files that already exist in HEAD.

## Setup

Use `~/buttest/athirdfile.md` as-is — it already exists in HEAD with
the baseline line `Adding this new file as a stacked diff And
changing something from the upstack PR`. No baseline-commit step
needed.

Any other already-in-HEAD file would work too; the requirement is
only that the file's existence does not depend on the stack we're
about to commit to. Run `git -C ~/buttest ls-tree HEAD` to see the
current set.

## Repro

1. Edit `~/buttest/athirdfile.md` and append three brand-new
   sections at the end of the file:

   ```
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
   ```

   Save. The worktree-changes panel should show one natural hunk on
   `athirdfile.md`, status `Modified`, with all the appended rows as
   `+`.

2. Split the natural hunk into three ranges so each `## Section X`
   block lives in its own sub-hunk. This keeps the original baseline
   rows out of every range — they're context above the diff.

3. Commit the middle sub-hunk (`## Section B`) to any stack.

4. Inspect the diff view for `athirdfile.md`:

   - **Expected (if locks track real content dependency):** the
     leading residual (`## Section A` block) should be unlocked. It
     was added in the worktree and the commit only touched lines
     belonging to Section B; Section A doesn't depend on the file's
     creation because the file was already in HEAD before the
     workflow started.
   - **Expected (if locks track "any change to a file whose HEAD
     entry was modified by the commit on this stack"):** Section A
     stays locked, same as the pure-add scenario in
     `splittest_pure_add.md`.

   Either outcome is informative — it tells us whether
   `but-hunk-dependency` is paying attention to per-line changes or
   only to per-file ownership.

## Comparison run on `splittest_pure_add.md`

For contrast, the existing `splittest_pure_add.md` workflow
(`~/buttest/splittest_pure_add.md`) starts the file as untracked,
which means *every* row in the worktree implicitly depends on the
commit that introduces the file to HEAD. Locks on every residual are
expected there. The two files together let us tell the difference
between "locks because the file is new on this stack" and "locks
because the dependency analysis is over-tagging existing-file
modifications".

## Notes for whoever investigates

- Override store is in-memory only. A full app relaunch drops every
  split and you start over from natural hunks.
- The override survives partial commits via the migration pass added
  in `crates/but-hunk-assignment/src/sub_hunk.rs::migrate_override_multi`.
  If the residuals re-collapse to one hunk after the commit, the
  migration likely failed; check the dev log for `target=sub_hunk`
  trace lines.
- Lock data comes from `but-hunk-dependency`. If we want the leading
  residual unlocked in case 1's first interpretation, the change goes
  there, not in the sub-hunk module.
