//! Phase 7d: move and uncommit *sub-ranges* of a committed hunk.
//!
//! The two RPCs in this module are thin wrappers around
//! [`commit_move_changes_between_with_perm`] and
//! [`commit_uncommit_changes_with_perm`]: they resolve `(commit_id, path,
//! anchor)` against the commit's first-parent diff, encode the user-supplied
//! [`but_hunk_assignment::RowRange`] into the null-side `HunkHeader` form the
//! commit engine expects (via
//! [`but_hunk_assignment::encode_sub_hunk_for_commit`]), and forward to the
//! existing move / uncommit pipelines as a single-element `Vec<DiffSpec>`.
//!
//! Override migration on commit rewrite is **not** part of this phase; an
//! override anchored to the source commit goes stale after a successful move
//! and the next commit-diff render will see no sub-hunks until the user
//! re-splits. Phase 7f closes that loop by remapping the override key to the
//! rewritten commit id and dropping ranges consumed by the move.

use anyhow::{Context as _, anyhow, bail};
use bstr::BString;
use but_api_macros::but_api;
use but_core::{DiffSpec, DryRun, HunkHeader, sync::RepoExclusive};
use but_ctx::Context;
use but_hunk_assignment::{HunkAssignment, RowRange};
use gix::prelude::ObjectIdExt;
use tracing::instrument;

use super::{
    move_changes::commit_move_changes_between_with_perm,
    types::MoveChangesResult,
    uncommit::commit_uncommit_changes_with_perm,
};

/// Resolve `(commit_id, path, anchor, range)` to a single
/// [`DiffSpec`] whose `hunk_headers` are the null-side per-row encoding the
/// commit engine consumes.
///
/// Trims leading / trailing context rows from `range` and validates the
/// trimmed range is non-empty and within the anchor's row count. Never emits
/// a header that covers the entire anchor — callers should reject that
/// case before reaching this helper (the popover already does).
fn encode_sub_hunk_diff_spec(
    ctx: &Context,
    commit_id: gix::ObjectId,
    path: BString,
    anchor: HunkHeader,
    range: RowRange,
) -> anyhow::Result<DiffSpec> {
    let context_lines = ctx.settings.context_lines;
    let repo = ctx.repo.get()?;
    let attached = commit_id.attach(&repo);
    let details =
        but_core::diff::CommitDetails::from_commit_id(attached, /* line_stats = */ false)?;

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
    let previous_path = change.previous_path().map(ToOwned::to_owned);
    let patch = change
        .unified_patch(&repo, context_lines)?
        .ok_or_else(|| anyhow!("path {} has no unified patch", path))?;
    let hunks = match patch {
        but_core::UnifiedPatch::Patch { hunks, .. } => hunks,
        _ => bail!(
            "path {} is not a textual patch in commit {}",
            path,
            commit_id
        ),
    };
    let hunk = hunks
        .iter()
        .find(|h| HunkHeader::from(*h) == anchor)
        .ok_or_else(|| anyhow!("anchor hunk not found in commit {}'s diff", commit_id))?;

    let kinds = but_hunk_assignment::sub_hunk::parse_row_kinds(hunk.diff.as_ref());
    let trimmed = but_hunk_assignment::sub_hunk::trim_context(range, &kinds)
        .ok_or_else(|| anyhow!("sub-range is context-only or empty after trim"))?;
    but_hunk_assignment::sub_hunk::validate_ranges(&[trimmed], kinds.len() as u32)
        .with_context(|| "invalid sub-range for sub-hunk move")?;

    let headers = but_hunk_assignment::encode_sub_hunk_for_commit(anchor, trimmed, &kinds);
    if headers.is_empty() {
        bail!("sub-range encoded to zero hunk headers");
    }
    Ok(DiffSpec {
        previous_path,
        path,
        hunk_headers: headers,
    })
}

/// Phase 7d: move a sub-range of `source_commit_id`'s hunk at `(path,
/// anchor)` into `destination_commit_id`.
///
/// Wraps [`commit_move_changes_between`] for sub-hunks. The sub-range is
/// encoded via [`encode_sub_hunk_diff_spec`] and forwarded as a
/// single-element `Vec<DiffSpec>`. The remainder of the source hunk stays at
/// `source_commit_id`; the rewritten source commit's tree omits exactly the
/// rows in `range`.
///
/// When `dry_run` is enabled, the returned workspace previews the rewritten
/// commits without materializing the rebase.
///
/// **Override migration is deferred to Phase 7f.** A `Commit { id: source,
/// path }`-keyed override on `source_commit_id` is *not* automatically
/// rekeyed to the rewritten commit id by this RPC; the next commit-diff
/// render against the rewritten commit will see no sub-hunks until the user
/// re-splits.
#[but_api(napi, try_from = crate::commit::json::MoveChangesResult)]
#[instrument(skip_all, err(Debug))]
pub fn move_sub_hunk(
    ctx: &mut Context,
    source_commit_id: gix::ObjectId,
    destination_commit_id: gix::ObjectId,
    path: BString,
    anchor: HunkHeader,
    range: RowRange,
    dry_run: DryRun,
) -> anyhow::Result<MoveChangesResult> {
    let mut guard = ctx.exclusive_worktree_access();
    move_sub_hunk_with_perm(
        ctx,
        source_commit_id,
        destination_commit_id,
        path,
        anchor,
        range,
        dry_run,
        guard.write_permission(),
    )
}

/// Implementation of [`move_sub_hunk`] under caller-held exclusive
/// repository access.
pub fn move_sub_hunk_with_perm(
    ctx: &mut Context,
    source_commit_id: gix::ObjectId,
    destination_commit_id: gix::ObjectId,
    path: BString,
    anchor: HunkHeader,
    range: RowRange,
    dry_run: DryRun,
    perm: &mut RepoExclusive,
) -> anyhow::Result<MoveChangesResult> {
    let spec =
        encode_sub_hunk_diff_spec(ctx, source_commit_id, path, anchor, range)?;
    let result = commit_move_changes_between_with_perm(
        ctx,
        source_commit_id,
        destination_commit_id,
        vec![spec],
        dry_run,
        perm,
    )?;
    if dry_run == DryRun::No {
        migrate_overrides_after_rewrite(ctx, &result, perm)?;
    }
    Ok(result)
}

/// Phase 7d: extract a sub-range of `commit_id`'s hunk at `(path, anchor)`
/// back into the worktree as uncommitted changes.
///
/// Wraps [`commit_uncommit_changes`] for sub-hunks. The sub-range is encoded
/// via [`encode_sub_hunk_diff_spec`] and forwarded as a single-element
/// `Vec<DiffSpec>`. The remainder of the source hunk stays at `commit_id`;
/// the rewritten commit's tree omits exactly the rows in `range`, and those
/// rows reappear as worktree changes (assigned to `assign_to` if set, or
/// left unassigned).
///
/// When `dry_run` is enabled, the returned workspace previews the extracted
/// changes without materializing the rebase or persisting hunk assignments.
///
/// **Override migration is deferred to Phase 7f**, same as
/// [`move_sub_hunk`].
#[but_api(napi, try_from = crate::commit::json::MoveChangesResult)]
#[instrument(skip_all, err(Debug))]
pub fn uncommit_sub_hunk(
    ctx: &mut Context,
    commit_id: gix::ObjectId,
    path: BString,
    anchor: HunkHeader,
    range: RowRange,
    assign_to: Option<but_core::ref_metadata::StackId>,
    dry_run: DryRun,
) -> anyhow::Result<MoveChangesResult> {
    let mut guard = ctx.exclusive_worktree_access();
    uncommit_sub_hunk_with_perm(
        ctx,
        commit_id,
        path,
        anchor,
        range,
        assign_to,
        dry_run,
        guard.write_permission(),
    )
}

/// Implementation of [`uncommit_sub_hunk`] under caller-held exclusive
/// repository access.
pub fn uncommit_sub_hunk_with_perm(
    ctx: &mut Context,
    commit_id: gix::ObjectId,
    path: BString,
    anchor: HunkHeader,
    range: RowRange,
    assign_to: Option<but_core::ref_metadata::StackId>,
    dry_run: DryRun,
    perm: &mut RepoExclusive,
) -> anyhow::Result<MoveChangesResult> {
    let spec = encode_sub_hunk_diff_spec(ctx, commit_id, path, anchor, range)?;
    let result =
        commit_uncommit_changes_with_perm(ctx, commit_id, vec![spec], assign_to, dry_run, perm)?;
    if dry_run == DryRun::No {
        migrate_overrides_after_rewrite(ctx, &result, perm)?;
    }
    Ok(result)
}

/// Phase 7f: build synthetic [`HunkAssignment`]s from `commit_id`'s
/// first-parent diff so the override migration helper has natural-hunk
/// candidates to align against.
///
/// One assignment per `(path, hunk)` pair; `path_bytes`, `hunk_header`,
/// and `diff` are populated. `sub_hunk_origin` is `None` because these
/// are natural commit-side hunks, not materialized sub-hunks. All other
/// fields are defaulted — the migration only reads the four populated
/// ones.
fn commit_diff_assignments(
    repo: &gix::Repository,
    commit_id: gix::ObjectId,
    context_lines: u32,
) -> anyhow::Result<Vec<HunkAssignment>> {
    let attached = commit_id.attach(repo);
    let details =
        but_core::diff::CommitDetails::from_commit_id(attached, /* line_stats = */ false)?;
    let mut out = Vec::new();
    for change in &details.diff_with_first_parent {
        let Some(patch) = change.unified_patch(repo, context_lines)? else {
            continue;
        };
        let hunks = match patch {
            but_core::UnifiedPatch::Patch { hunks, .. } => hunks,
            _ => continue,
        };
        for h in hunks {
            out.push(HunkAssignment {
                id: None,
                hunk_header: Some(HunkHeader::from(&h)),
                path: change.path.to_string(),
                path_bytes: change.path.clone(),
                stack_id: None,
                branch_ref_bytes: None,
                line_nums_added: None,
                line_nums_removed: None,
                diff: Some(h.diff),
                sub_hunk_origin: None,
            });
        }
    }
    Ok(out)
}

/// Phase 7f: walk `result.workspace.replaced_commits` and migrate every
/// commit-anchored override keyed on a rewritten `old_id` onto the
/// corresponding `new_id` via
/// [`but_hunk_assignment::migrate_commit_overrides_persistent`].
///
/// Errors during migration are logged and swallowed: the move/uncommit
/// itself already succeeded, and a migration failure should not be
/// surfaced as an RPC error to the user. Pathological cases (corrupt
/// row, missing commit object) drop the override; the user can re-split.
fn migrate_overrides_after_rewrite(
    ctx: &mut Context,
    result: &MoveChangesResult,
    perm: &mut RepoExclusive,
) -> anyhow::Result<()> {
    let mappings = &result.workspace.replaced_commits;
    if mappings.is_empty() {
        return Ok(());
    }
    let context_lines = ctx.settings.context_lines;
    let gitdir = ctx.gitdir.clone();
    let (repo, _ws, mut db) = ctx.workspace_mut_and_db_mut_with_perm(perm)?;
    for (old_id, new_id) in mappings {
        if old_id == new_id {
            continue;
        }
        let new_assignments = match commit_diff_assignments(&repo, *new_id, context_lines) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    target: "sub_hunk",
                    old_id = %old_id,
                    new_id = %new_id,
                    error = ?err,
                    "phase 7f: could not build new-commit assignments; skipping migration",
                );
                continue;
            }
        };
        if let Err(err) = but_hunk_assignment::migrate_commit_overrides_persistent(
            &mut db,
            &gitdir,
            *old_id,
            *new_id,
            &new_assignments,
        ) {
            tracing::warn!(
                target: "sub_hunk",
                old_id = %old_id,
                new_id = %new_id,
                error = ?err,
                "phase 7f: migration helper returned error; skipping",
            );
        }
    }
    Ok(())
}
