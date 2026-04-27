
use anyhow::{Context as _, anyhow, bail};
use bstr::BString;
use but_api_macros::but_api;
use but_core::{HunkHeader, sync::RepoExclusive, ui::TreeChange};
use but_ctx::Context;
use but_hunk_assignment::{HunkAssignmentRequest, RowRange, SubHunkOverride, WorktreeChanges};
use but_hunk_dependency::ui::hunk_dependencies_for_workspace_changes_by_worktree_dir;
use but_oplog::legacy::{OperationKind, SnapshotDetails};
use gix::prelude::ObjectIdExt;
use tracing::instrument;

boolean_enums::gen_boolean_enum!(pub serde ComputeLineStats);

use but_core::diff::CommitDetails;

/// JSON types
// TODO: add schemars
pub mod json {
    use but_core::diff::LineStats;
    use serde::Serialize;

    /// The JSON sibling of [but_core::diff::CommitDetails].
    #[derive(Debug, Serialize)]
    #[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
    #[serde(rename_all = "camelCase")]
    pub struct CommitDetails {
        /// The commit itself.
        // TODO: make this our own json structure - this one is GUI specific and isn't great
        pub commit: but_workspace::ui::Commit,
        /// The changes
        pub changes: Vec<but_core::ui::TreeChange>,
        /// The stats of the changes.
        // TODO: adapt the frontend to be more specific as well.
        #[serde(rename = "stats")]
        pub line_stats: Option<LineStats>,
        /// Conflicting entries in `commit` as stored in the conflict commit metadata.
        pub conflict_entries: Option<but_core::commit::ConflictEntries>,
    }
    #[cfg(feature = "export-schema")]
    but_schemars::register_sdk_type!(CommitDetails);

    impl From<but_core::diff::CommitDetails> for CommitDetails {
        fn from(value: but_core::diff::CommitDetails) -> Self {
            let but_core::diff::CommitDetails {
                commit,
                diff_with_first_parent,
                line_stats,
                conflict_entries,
            } = value;

            CommitDetails {
                commit: commit.into(),
                changes: diff_with_first_parent.into_iter().map(Into::into).collect(),
                line_stats,
                conflict_entries,
            }
        }
    }
}

/// Computes the tree diff for `commit_id` against its first parent and
/// optionally calculates `line_stats`.
///
/// For lower-level implementation details, see
/// [`but_core::diff::CommitDetails::from_commit_id()`].
#[but_api(json::CommitDetails)]
#[instrument(err(Debug))]
pub fn commit_details(
    ctx: &Context,
    commit_id: gix::ObjectId,
    line_stats: ComputeLineStats,
) -> anyhow::Result<CommitDetails> {
    let repo = ctx.repo.get()?;
    CommitDetails::from_commit_id(commit_id.attach(&repo), line_stats.into())
}

/// Computes commit details for `commit_id` with line statistics enabled.
///
/// This exists for callers that always want line statistics without passing
/// `line_stats` explicitly.
#[but_api(napi, json::CommitDetails)]
#[instrument(err(Debug))]
pub fn commit_details_with_line_stats(
    ctx: &Context,
    commit_id: gix::ObjectId,
) -> anyhow::Result<CommitDetails> {
    commit_details(ctx, commit_id, ComputeLineStats::Yes)
}

/// Produces a unified patch for `change`.
///
/// `change` must not be a type change or a submodule change. For lower-level
/// implementation details, see [`but_core::TreeChange::unified_patch()`].
#[but_api(napi)]
#[instrument(err(Debug))]
pub fn tree_change_diffs(
    ctx: &Context,
    change: TreeChange,
) -> anyhow::Result<Option<but_core::UnifiedPatch>> {
    let change: but_core::TreeChange = change.into();
    let repo = ctx.repo.get()?;
    change.unified_patch(&repo, ctx.settings.context_lines)
}

/// See [`changes_in_worktree_with_perm()`].
#[but_api(napi)]
#[instrument(err(Debug))]
pub fn changes_in_worktree(ctx: &mut Context) -> anyhow::Result<WorktreeChanges> {
    let mut guard = ctx.exclusive_worktree_access();
    changes_in_worktree_with_perm(ctx, guard.write_permission())
}

/// This UI-version of [`but_core::diff::worktree_changes()`] simplifies the `git status` information for display in
/// the user interface as it is right now. From here, it's always possible to add more information as the need arises.
///
/// ### Notable Transformations
/// * There is no notion of an index (`.git/index`) - all changes seem to have happened in the worktree.
/// * Modifications that were made to the index will be ignored *only if* there is a worktree modification to the same file.
/// * conflicts are ignored
///
/// All ignored status changes are also provided so they can be displayed separately.
///
/// For lower-level implementation details, see
/// [`but_core::diff::worktree_changes()`],
/// [`but_hunk_assignment::assignments_with_fallback()`], and
/// [`but_hunk_dependency::ui::hunk_dependencies_for_workspace_changes_by_worktree_dir()`].
#[but_api(napi)]
#[instrument(skip_all, err(Debug))]
pub fn changes_in_worktree_with_perm(
    ctx: &mut Context,
    perm: &mut RepoExclusive,
) -> anyhow::Result<WorktreeChanges> {
    let context_lines = ctx.settings.context_lines;
    let gitdir = ctx.gitdir.clone();

    let (repo, ws, mut db) = ctx.workspace_mut_and_db_mut_with_perm(perm)?;

    // Phase 6.5c: lazy-hydrate persisted sub-hunk overrides on the first
    // worktree-changes read for this `gitdir`. `ensure_hydrated` is a
    // process-wide once-per-gitdir guard, so subsequent calls are
    // free; placing it at this entry point covers app launch (and any
    // path that asks the desktop for worktree assignments).
    but_hunk_assignment::ensure_hydrated(&db, &gitdir);

    let changes = but_core::diff::worktree_changes(&repo)?;

    let dependencies = hunk_dependencies_for_workspace_changes_by_worktree_dir(
        &repo,
        &ws,
        Some(changes.changes.clone()),
    );

    // Phase 6.5d-followup: route through the persistent variant so the
    // override reconcile pass writes through partial-commit migrations
    // and stale-anchor drops to the `sub_hunk_overrides` table. The
    // savepoint over `hunk_assignments` is created internally.
    let (assignments, assignments_error) = {
        but_hunk_assignment::assignments_with_fallback_persistent(
            &mut db,
            &repo,
            &ws,
            Some(changes.changes.clone()),
            context_lines,
        )?
    };

    drop((repo, ws, db));
    #[cfg(feature = "legacy")]
    but_rules::handler::process_workspace_rules(ctx, &assignments, perm).ok();

    Ok(WorktreeChanges {
        worktree_changes: changes.into(),
        assignments,
        assignments_error: assignments_error.map(|err| serde_error::Error::new(&*err)),
        dependencies: dependencies.as_ref().ok().cloned(),
        dependencies_error: dependencies
            .as_ref()
            .err()
            .map(|err| serde_error::Error::new(&**err)),
    })
}

/// Persists `assignments` for the current workspace without creating an oplog
/// entry.
///
/// This acquires exclusive worktree access from `ctx` before writing
/// assignments.
///
/// See [`assign_hunk_only_with_perm()`] for details.
#[but_api]
#[instrument(skip_all, err(Debug))]
pub fn assign_hunk_only(
    ctx: &mut Context,
    assignments: Vec<HunkAssignmentRequest>,
) -> anyhow::Result<()> {
    let mut guard = ctx.exclusive_worktree_access();
    assign_hunk_only_with_perm(ctx, assignments, guard.write_permission())
}

/// Persists `assignments` under caller-held exclusive repository access without
/// creating an oplog entry.
///
/// For lower-level implementation details, see
/// [`but_hunk_assignment::assign()`].
pub fn assign_hunk_only_with_perm(
    ctx: &mut Context,
    assignments: Vec<HunkAssignmentRequest>,
    perm: &mut RepoExclusive,
) -> anyhow::Result<()> {
    let context_lines = ctx.settings.context_lines;
    let (repo, ws, mut db) = ctx.workspace_mut_and_db_mut_with_perm(perm)?;
    // Phase 6.5d-followup: persistent variant so override migrations /
    // drops triggered by the assignment reconcile survive an app
    // relaunch.
    but_hunk_assignment::assign_persistent(
        &mut db,
        &repo,
        &ws,
        assignments,
        context_lines,
    )?;
    Ok(())
}

/// Persists `assignments` for the current workspace and records an oplog
/// snapshot on success.
///
/// This acquires exclusive worktree access from `ctx` before writing
/// assignments.
///
/// See [`assign_hunk_with_perm()`] for details.
#[but_api(napi)]
#[instrument(skip_all, err(Debug))]
pub fn assign_hunk(
    ctx: &mut Context,
    assignments: Vec<HunkAssignmentRequest>,
) -> anyhow::Result<()> {
    let mut guard = ctx.exclusive_worktree_access();
    assign_hunk_with_perm(ctx, assignments, guard.write_permission())
}

/// Persists `assignments` under caller-held exclusive repository access and
/// records an oplog snapshot on success.
///
/// It behaves like [`assign_hunk_only_with_perm()`], but first prepares a
/// best-effort `MoveHunk` oplog snapshot and commits the snapshot only if the
/// assignment succeeds.
pub fn assign_hunk_with_perm(
    ctx: &mut Context,
    assignments: Vec<HunkAssignmentRequest>,
    perm: &mut RepoExclusive,
) -> anyhow::Result<()> {
    // this oplog entry is currently a noop (i.e. restoring it does nothing) but we do wanna
    // support it in the future so leaving it here for consistency
    let maybe_oplog_entry = but_oplog::UnmaterializedOplogSnapshot::from_details_with_perm(
        ctx,
        SnapshotDetails::new(OperationKind::MoveHunk),
        perm.read_permission(),
        but_core::DryRun::No,
    );

    let res = assign_hunk_only_with_perm(ctx, assignments, perm);
    if let Some(snapshot) = maybe_oplog_entry
        && res.is_ok()
    {
        snapshot.commit(ctx, perm).ok();
    }
    res
}

/// Split the natural hunk identified by `(path, anchor)` into sub-hunks at the
/// row boundaries described by `ranges`.
///
/// `ranges` are 0-based, half-open row indices into the anchor hunk's diff
/// body (excluding the `@@` header line). Leading and trailing context rows
/// are silently trimmed from each range before validation. After trimming,
/// the request must satisfy:
///
/// - at least one non-empty range,
/// - ranges sorted, disjoint, and contained within the anchor's row count,
/// - ranges do not collectively cover every row in the anchor.
///
/// The override is stored in process memory only (see
/// [`but_hunk_assignment::sub_hunk`]); it is dropped on app relaunch and on
/// the next reconcile pass that fails to find a natural hunk with this exact
/// anchor (e.g. after the user edits the file). Reassigning the resulting
/// sub-hunks to other stacks goes through the regular `assign_hunk` flow.
#[but_api(napi)]
#[instrument(skip_all, err(Debug))]
pub fn split_hunk(
    ctx: &mut Context,
    path: BString,
    anchor: HunkHeader,
    ranges: Vec<RowRange>,
) -> anyhow::Result<()> {
    let mut guard = ctx.exclusive_worktree_access();
    split_hunk_with_perm(ctx, path, anchor, ranges, guard.write_permission())
}

/// Implementation of [`split_hunk`] that runs under caller-held exclusive
/// repository access. Useful for callers that already hold the lock.
pub fn split_hunk_with_perm(
    ctx: &mut Context,
    path: BString,
    anchor: HunkHeader,
    ranges: Vec<RowRange>,
    perm: &mut RepoExclusive,
) -> anyhow::Result<()> {
    let context_lines = ctx.settings.context_lines;
    let gitdir = ctx.gitdir.clone();
    let (repo, ws, mut db) = ctx.workspace_mut_and_db_mut_with_perm(perm)?;

    // Find the natural hunk matching (path, anchor) and recover its row body.
    let changes = but_core::diff::worktree_changes(&repo)?.changes;
    let change = changes
        .iter()
        .find(|c| c.path == path)
        .ok_or_else(|| anyhow!("path {} not present in worktree changes", path))?;
    let patch = change
        .unified_patch(&repo, context_lines)?
        .ok_or_else(|| anyhow!("path {} has no unified patch", path))?;
    let hunks = match patch {
        but_core::UnifiedPatch::Patch { hunks, .. } => hunks,
        _ => bail!("path {} is not a textual patch", path),
    };
    let hunk = hunks
        .iter()
        .find(|h| HunkHeader::from(*h) == anchor)
        .ok_or_else(|| anyhow!("anchor hunk not found in current worktree diff"))?;
    let kinds = but_hunk_assignment::sub_hunk::parse_row_kinds(hunk.diff.as_ref());
    let row_count = kinds.len() as u32;

    let trimmed: Vec<RowRange> = ranges
        .into_iter()
        .filter_map(|r| but_hunk_assignment::sub_hunk::trim_context(r, &kinds))
        .collect();
    but_hunk_assignment::sub_hunk::validate_ranges(&trimmed, row_count)
        .with_context(|| "invalid split request")?;

    // Reconstruct the anchor's full diff (header + body) so the migration
    // pass can content-match rows when the anchor's shape changes.
    let mut anchor_diff = BString::default();
    anchor_diff.extend_from_slice(
        format!(
            "@@ -{},{} +{},{} @@\n",
            anchor.old_start, anchor.old_lines, anchor.new_start, anchor.new_lines
        )
        .as_bytes(),
    );
    anchor_diff.extend_from_slice(hunk.diff.as_ref());

    // Merge with an existing override for the same `(path, anchor)` so a
    // user can refine an already-split natural hunk by re-splitting one
    // of its sub-hunks. Without this, calling `split_hunk` again would
    // wipe out the previous splits.
    let existing = but_hunk_assignment::get_override(&gitdir, &path, anchor);
    let merged_assignments = existing
        .as_ref()
        .map(|ov| ov.assignments.clone())
        .unwrap_or_default();
    let stored_ranges = match existing.as_ref() {
        Some(ov) => but_hunk_assignment::merge_user_ranges_into_partition(&ov.ranges, &trimmed),
        None => {
            // Phase 4.5: store residuals as first-class ranges so they can survive
            // partial commits via the migration pass.
            but_hunk_assignment::sub_hunk::materialize_residual_ranges(&trimmed, &kinds)
        }
    };

    // Phase 6.5d: route through `upsert_override_persistent` so the
    // override survives an app relaunch. Falls back to in-memory only
    // when the row exceeds the size guard (see
    // `MAX_OVERRIDE_DB_BYTES`).
    but_hunk_assignment::upsert_override_persistent(
        &mut db,
        &gitdir,
        SubHunkOverride {
            origin: but_hunk_assignment::SubHunkOriginLocation::worktree(
                path.clone(),
            ),
            path: path.clone(),
            anchor,
            ranges: stored_ranges,
            assignments: merged_assignments,
            rows: kinds,
            anchor_diff,
        },
    )?;

    // Trigger a reconcile so the persisted assignments and downstream consumers
    // see the materialized sub-hunks. Use the persistent variant so any
    // override migration the reconcile triggers writes through to disk.
    but_hunk_assignment::assignments_with_fallback_persistent(
        &mut db,
        &repo,
        &ws,
        Some(changes),
        context_lines,
    )?;
    Ok(())
}

/// Reverse a previous [`split_hunk`] for `(path, anchor)`. Materialized
/// sub-hunks reabsorb into the natural anchor on the next reconcile.
///
/// Per-sub-hunk reassignments to other stacks are dropped (the merged hunk
/// reverts to the anchor's pre-split assignment). The frontend is responsible
/// for surfacing a confirmation prompt before calling this when a
/// reassignment would be lost.
#[but_api(napi)]
#[instrument(skip_all, err(Debug))]
pub fn unsplit_hunk(
    ctx: &mut Context,
    path: BString,
    anchor: HunkHeader,
) -> anyhow::Result<()> {
    let mut guard = ctx.exclusive_worktree_access();
    unsplit_hunk_with_perm(ctx, path, anchor, guard.write_permission())
}

/// Implementation of [`unsplit_hunk`] that runs under caller-held exclusive
/// repository access.
pub fn unsplit_hunk_with_perm(
    ctx: &mut Context,
    path: BString,
    anchor: HunkHeader,
    perm: &mut RepoExclusive,
) -> anyhow::Result<()> {
    let context_lines = ctx.settings.context_lines;
    let gitdir = ctx.gitdir.clone();

    let (repo, ws, mut db) = ctx.workspace_mut_and_db_mut_with_perm(perm)?;
    // Phase 6.5d: write-through to disk so the override doesn't
    // resurrect on next launch via `hydrate_from_db`.
    let _removed = but_hunk_assignment::remove_override_persistent(
        &mut db, &gitdir, &path, anchor,
    )?;
    but_hunk_assignment::assignments_with_fallback_persistent(
        &mut db,
        &repo,
        &ws,
        None::<Vec<but_core::TreeChange>>,
        context_lines,
    )?;
    Ok(())
}

/// Phase 7c-3: split a hunk inside an existing commit's diff against
/// its first parent into one or more sub-hunks.
///
/// Mirrors [`split_hunk`] but the override is anchored to a specific
/// `commit_id` rather than the live worktree. Until Phase 7c-4 wires
/// the commit-side override-aware diff RPC, the resulting sub-hunks
/// are persisted to disk but not yet visible in the commit-diff UI.
///
/// Validation rules (mirrors the worktree variant):
/// - the anchor must match a hunk in the commit's first-parent diff
///   for `path`,
/// - at least one non-empty range,
/// - ranges sorted, disjoint, and contained within the anchor's row
///   count,
/// - ranges do not collectively cover every row in the anchor.
#[but_api(napi)]
#[instrument(skip_all, err(Debug))]
pub fn split_hunk_in_commit(
    ctx: &mut Context,
    commit_id: gix::ObjectId,
    path: BString,
    anchor: HunkHeader,
    ranges: Vec<RowRange>,
) -> anyhow::Result<()> {
    let mut guard = ctx.exclusive_worktree_access();
    split_hunk_in_commit_with_perm(
        ctx,
        commit_id,
        path,
        anchor,
        ranges,
        guard.write_permission(),
    )
}

/// Implementation of [`split_hunk_in_commit`] under caller-held
/// exclusive repository access.
pub fn split_hunk_in_commit_with_perm(
    ctx: &mut Context,
    commit_id: gix::ObjectId,
    path: BString,
    anchor: HunkHeader,
    ranges: Vec<RowRange>,
    perm: &mut RepoExclusive,
) -> anyhow::Result<()> {
    let context_lines = ctx.settings.context_lines;
    let gitdir = ctx.gitdir.clone();
    let (repo, _ws, mut db) = ctx.workspace_mut_and_db_mut_with_perm(perm)?;

    // Fetch the commit's diff against its first parent. Same shape
    // `commit_details` already exposes to the desktop, but we don't
    // need line stats for an override write.
    let attached = commit_id.attach(&repo);
    let details = CommitDetails::from_commit_id(attached, /* line_stats = */ false)?;

    let change = details
        .diff_with_first_parent
        .iter()
        .find(|c| c.path == path)
        .ok_or_else(|| {
            anyhow!(
                "path {} not present in commit {}'s diff",
                path,
                commit_id
            )
        })?;
    let patch = change
        .unified_patch(&repo, context_lines)?
        .ok_or_else(|| anyhow!("path {} has no unified patch", path))?;
    let hunks = match patch {
        but_core::UnifiedPatch::Patch { hunks, .. } => hunks,
        _ => bail!("path {} is not a textual patch in commit {}", path, commit_id),
    };
    let hunk = hunks
        .iter()
        .find(|h| HunkHeader::from(*h) == anchor)
        .ok_or_else(|| {
            anyhow!("anchor hunk not found in commit {}'s diff", commit_id)
        })?;
    let kinds = but_hunk_assignment::sub_hunk::parse_row_kinds(hunk.diff.as_ref());
    let row_count = kinds.len() as u32;

    let trimmed: Vec<RowRange> = ranges
        .into_iter()
        .filter_map(|r| but_hunk_assignment::sub_hunk::trim_context(r, &kinds))
        .collect();
    but_hunk_assignment::sub_hunk::validate_ranges(&trimmed, row_count)
        .with_context(|| "invalid split request")?;

    let mut anchor_diff = BString::default();
    anchor_diff.extend_from_slice(
        format!(
            "@@ -{},{} +{},{} @@\n",
            anchor.old_start, anchor.old_lines, anchor.new_start, anchor.new_lines
        )
        .as_bytes(),
    );
    anchor_diff.extend_from_slice(hunk.diff.as_ref());

    // Re-split support: merge new ranges into an existing
    // commit-keyed override's partition rather than replacing it.
    let location =
        but_hunk_assignment::SubHunkOriginLocation::commit(commit_id, path.clone());
    let existing =
        but_hunk_assignment::get_commit_override(&gitdir, commit_id, &path, anchor);
    let merged_assignments = existing
        .as_ref()
        .map(|ov| ov.assignments.clone())
        .unwrap_or_default();
    let stored_ranges = match existing.as_ref() {
        Some(ov) => but_hunk_assignment::merge_user_ranges_into_partition(
            &ov.ranges, &trimmed,
        ),
        None => but_hunk_assignment::sub_hunk::materialize_residual_ranges(
            &trimmed, &kinds,
        ),
    };

    // `upsert_override_persistent` already encodes the location into
    // `commit_id` on the row via `to_db_row`, so a single call
    // persists the commit-keyed override correctly.
    but_hunk_assignment::upsert_override_persistent(
        &mut db,
        &gitdir,
        SubHunkOverride {
            origin: location,
            path,
            anchor,
            ranges: stored_ranges,
            assignments: merged_assignments,
            rows: kinds,
            anchor_diff,
        },
    )?;
    Ok(())
}

/// Phase 7c-3: reverse a previous [`split_hunk_in_commit`] for
/// `(commit_id, path, anchor)`. Materialized sub-hunks reabsorb into
/// the natural anchor on the next commit-diff render pass.
#[but_api(napi)]
#[instrument(skip_all, err(Debug))]
pub fn unsplit_hunk_in_commit(
    ctx: &mut Context,
    commit_id: gix::ObjectId,
    path: BString,
    anchor: HunkHeader,
) -> anyhow::Result<()> {
    let mut guard = ctx.exclusive_worktree_access();
    unsplit_hunk_in_commit_with_perm(
        ctx,
        commit_id,
        path,
        anchor,
        guard.write_permission(),
    )
}

/// Implementation of [`unsplit_hunk_in_commit`] under caller-held
/// exclusive repository access.
pub fn unsplit_hunk_in_commit_with_perm(
    ctx: &mut Context,
    commit_id: gix::ObjectId,
    path: BString,
    anchor: HunkHeader,
    perm: &mut RepoExclusive,
) -> anyhow::Result<()> {
    let gitdir = ctx.gitdir.clone();
    let (_repo, _ws, mut db) = ctx.workspace_mut_and_db_mut_with_perm(perm)?;
    let location =
        but_hunk_assignment::SubHunkOriginLocation::commit(commit_id, path);
    let _removed = but_hunk_assignment::remove_override_persistent_at(
        &mut db, &gitdir, &location, anchor,
    )?;
    Ok(())
}

