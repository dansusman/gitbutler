use std::collections::HashSet;

use crate::WorkspaceState;
use anyhow::Context as _;
use but_api_macros::but_api;
use but_core::{DryRun, sync::RepoExclusive};
use but_hunk_assignment::{HunkAssignmentRequest, HunkAssignmentTarget};
use but_oplog::legacy::{OperationKind, SnapshotDetails, Trailer};
use but_rebase::graph_rebase::Editor;
use tracing::instrument;

use crate::commit::types::CommitUndoResult;

/// Undo `subject_commit_id` using the behavior described by
/// [`commit_undo_only_with_perm()`].
///
/// When `dry_run` is enabled, the returned workspace previews the undo result
/// without materializing the rewrite or persisting an oplog entry.
#[but_api(napi, try_from = crate::commit::json::CommitUndoResult)]
#[instrument(err(Debug))]
pub fn commit_undo(
    ctx: &mut but_ctx::Context,
    subject_commit_id: gix::ObjectId,
    assign_to: Option<but_core::ref_metadata::StackId>,
    dry_run: DryRun,
) -> anyhow::Result<CommitUndoResult> {
    let mut guard = ctx.exclusive_worktree_access();
    commit_undo_with_perm(
        ctx,
        subject_commit_id,
        assign_to,
        dry_run,
        guard.write_permission(),
    )
}

/// Undo `subject_commit_id` using the behavior described by
/// [`commit_undo_only_with_perm()`].
///
/// When `dry_run` is enabled, the returned workspace previews the undo result
/// without materializing the rewrite.
pub fn commit_undo_only(
    ctx: &mut but_ctx::Context,
    subject_commit_id: gix::ObjectId,
    assign_to: Option<but_core::ref_metadata::StackId>,
    dry_run: DryRun,
) -> anyhow::Result<CommitUndoResult> {
    let mut guard = ctx.exclusive_worktree_access();
    commit_undo_only_with_perm(
        ctx,
        subject_commit_id,
        assign_to,
        dry_run,
        guard.write_permission(),
    )
}

/// Undo `subject_commit_id` using the behavior described by
/// [`commit_undo_only_with_perm()`].
///
/// When `dry_run` is enabled, the returned workspace previews the undo result
/// and skips oplog persistence.
pub fn commit_undo_with_perm(
    ctx: &mut but_ctx::Context,
    subject_commit_id: gix::ObjectId,
    assign_to: Option<but_core::ref_metadata::StackId>,
    dry_run: DryRun,
    perm: &mut RepoExclusive,
) -> anyhow::Result<CommitUndoResult> {
    let details = SnapshotDetails::new(OperationKind::UndoCommit).with_trailers(vec![Trailer {
        key: "sha".to_string(),
        value: subject_commit_id.to_string(),
    }]);
    let maybe_oplog_entry = but_oplog::UnmaterializedOplogSnapshot::from_details_with_perm(
        ctx,
        details,
        perm.read_permission(),
        dry_run,
    );

    let res = commit_undo_only_with_perm(ctx, subject_commit_id, assign_to, dry_run, perm);
    if let Some(snapshot) = maybe_oplog_entry
        && res.is_ok()
    {
        snapshot.commit(ctx, perm).ok();
    }
    res
}

/// Undo `subject_commit_id`, under caller-held exclusive repository access.
///
/// This will move the changes in the commit to be unassigned and discard the
/// commit. When `dry_run` is enabled, it returns a preview of the resulting
/// workspace state without materializing the rewrite.
pub fn commit_undo_only_with_perm(
    ctx: &mut but_ctx::Context,
    subject_commit_id: gix::ObjectId,
    assign_to: Option<but_core::ref_metadata::StackId>,
    dry_run: DryRun,
    perm: &mut RepoExclusive,
) -> anyhow::Result<CommitUndoResult> {
    let result = commit_undo_only_with_perm_inner(
        ctx, subject_commit_id, assign_to, dry_run, perm,
    )?;
    super::sub_hunk::migrate_overrides_after_workspace_rewrite(
        ctx,
        &result.workspace,
        dry_run,
        perm,
    )?;
    Ok(result)
}

fn commit_undo_only_with_perm_inner(
    ctx: &mut but_ctx::Context,
    subject_commit_id: gix::ObjectId,
    assign_to: Option<but_core::ref_metadata::StackId>,
    dry_run: DryRun,
    perm: &mut RepoExclusive,
) -> anyhow::Result<CommitUndoResult> {
    let context_lines = ctx.settings.context_lines;
    let mut meta = ctx.meta()?;
    let (repo, mut ws, mut db) = ctx.workspace_mut_and_db_mut_with_perm(perm)?;
    let mut tx = db.transaction()?;

    let before_assignments = if assign_to.is_some() {
        let (assignments, _) = but_hunk_assignment::assignments_with_fallback(
            tx.hunk_assignments_mut()?,
            &repo,
            &ws,
            None::<Vec<but_core::TreeChange>>,
            context_lines,
        )?;
        Some(assignments)
    } else {
        None
    };

    let editor = Editor::create(&mut ws, &mut meta, &repo)?;

    let rebase = but_workspace::commit::discard_commit(editor, subject_commit_id)
        .with_context(|| format!("failed to discard {}", subject_commit_id.to_hex()))?;

    let (workspace, replaced_commits, repo) = if dry_run.into() {
        let graph = rebase.overlayed_graph()?;
        (
            &mut graph.into_workspace()?,
            rebase.history.commit_mappings(),
            rebase.repository(),
        )
    } else {
        let materialized = rebase.materialize_without_checkout()?;
        (
            materialized.workspace,
            materialized.history.commit_mappings(),
            &*repo,
        )
    };

    if let (Some(before_assignments), Some(assign_to)) = (before_assignments, assign_to) {
        let (after_assignments, _) = but_hunk_assignment::assignments_with_fallback(
            tx.hunk_assignments_mut()?,
            repo,
            workspace,
            None::<Vec<but_core::TreeChange>>,
            context_lines,
        )?;

        let before_ids: HashSet<_> = before_assignments
            .into_iter()
            .filter_map(|assignment| assignment.id)
            .collect();

        let to_assign: Vec<_> = after_assignments
            .into_iter()
            .filter(|assignment| assignment.id.is_some_and(|id| !before_ids.contains(&id)))
            .map(|assignment| HunkAssignmentRequest {
                hunk_header: assignment.hunk_header,
                path_bytes: assignment.path_bytes,
                target: Some(HunkAssignmentTarget::Stack {
                    stack_id: assign_to,
                }),
            })
            .collect();

        but_hunk_assignment::assign(
            tx.hunk_assignments_mut()?,
            repo,
            workspace,
            to_assign,
            context_lines,
        )?;
    }

    if dry_run == DryRun::No {
        tx.commit()?;
    }

    Ok(CommitUndoResult {
        undone_commit: subject_commit_id,
        workspace: WorkspaceState::from_workspace(workspace, repo, replaced_commits)?,
    })
}
