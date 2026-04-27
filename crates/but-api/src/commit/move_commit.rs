use anyhow::bail;
use but_api_macros::but_api;
use but_core::{DryRun, sync::RepoExclusive};
use but_oplog::legacy::{OperationKind, SnapshotDetails};
use but_rebase::graph_rebase::{
    Editor, LookupStep as _,
    mutate::{InsertSide, RelativeTo},
};
use tracing::instrument;

use crate::WorkspaceState;

use super::types::CommitMoveResult;

/// Moves `subject_commit_ids` to `side` of `relative_to`.
///
/// This acquires exclusive worktree access from `ctx` before moving the
/// commit.
///
/// When `dry_run` is enabled, the returned workspace previews the moved commit
/// without materializing the rebase. For details, see
/// [`commit_move_only_with_perm()`].
#[but_api(try_from = crate::commit::json::CommitMoveResult)]
pub fn commit_move_only(
    ctx: &mut but_ctx::Context,
    subject_commit_ids: Vec<gix::ObjectId>,
    #[but_api(crate::commit::json::RelativeTo)] relative_to: RelativeTo,
    side: InsertSide,
    dry_run: DryRun,
) -> anyhow::Result<CommitMoveResult> {
    let mut guard = ctx.exclusive_worktree_access();
    commit_move_only_with_perm(
        ctx,
        subject_commit_ids,
        relative_to,
        side,
        dry_run,
        guard.write_permission(),
    )
}

/// Move `subject_commit_ids` to the `side` of `relative_to` under
/// caller-held exclusive repository access.
///
/// This returns the post-operation workspace view without creating an oplog
/// entry. When `dry_run` is enabled, it returns a preview of the resulting
/// workspace state without materializing the rebase. For lower-level
/// implementation details, see [`but_workspace::commit::move_commit()`].
pub fn commit_move_only_with_perm(
    ctx: &mut but_ctx::Context,
    subject_commit_ids: Vec<gix::ObjectId>,
    relative_to: RelativeTo,
    side: InsertSide,
    dry_run: DryRun,
    perm: &mut RepoExclusive,
) -> anyhow::Result<CommitMoveResult> {
    if subject_commit_ids.is_empty() {
        bail!("No commits were provided to move")
    }

    let result = commit_move_only_with_perm_inner(
        ctx,
        subject_commit_ids,
        relative_to,
        side,
        dry_run,
        perm,
    )?;
    super::sub_hunk::migrate_overrides_after_workspace_rewrite(
        ctx,
        &result.workspace,
        dry_run,
        perm,
    )?;
    Ok(result)
}

fn commit_move_only_with_perm_inner(
    ctx: &mut but_ctx::Context,
    subject_commit_ids: Vec<gix::ObjectId>,
    relative_to: RelativeTo,
    side: InsertSide,
    dry_run: DryRun,
    perm: &mut RepoExclusive,
) -> anyhow::Result<CommitMoveResult> {
    let mut meta = ctx.meta()?;
    let (repo, mut ws, _) = ctx.workspace_mut_and_db_with_perm(perm)?;
    let editor = Editor::create(&mut ws, &mut meta, &repo)?;

    let ordered_selectors = editor.order_commit_selectors_by_parentage(subject_commit_ids)?;
    let mut ordered_ids = ordered_selectors
        .iter()
        .map(|selector| editor.lookup_pick(*selector))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let ordered_ids = if matches!(side, InsertSide::Above) {
        ordered_ids.reverse();
        ordered_ids
    } else {
        ordered_ids
    };

    let mut subjects = ordered_ids.into_iter();
    let first_subject = subjects
        .next()
        .expect("non-empty commit list always has a first subject");

    let mut rebase =
        but_workspace::commit::move_commit(editor, first_subject, relative_to.clone(), side)?;

    for original_subject_id in subjects {
        let remapped_ids = rebase.history.commit_mappings();
        let subject_id = remapped_ids
            .get(&original_subject_id)
            .copied()
            .unwrap_or(original_subject_id);

        let remapped_relative_to = match &relative_to {
            RelativeTo::Commit(target_commit_id) => RelativeTo::Commit(
                remapped_ids
                    .get(target_commit_id)
                    .copied()
                    .unwrap_or(*target_commit_id),
            ),
            RelativeTo::Reference(reference_name) => RelativeTo::Reference(reference_name.clone()),
        };

        rebase = but_workspace::commit::move_commit(
            rebase.into_editor(),
            subject_id,
            remapped_relative_to,
            side,
        )?;
    }

    Ok(CommitMoveResult {
        workspace: WorkspaceState::from_successful_rebase(rebase, &repo, dry_run)?,
    })
}

/// Moves `subject_commit_ids` to `side` of `relative_to` and records an oplog
/// snapshot on success.
///
/// This acquires exclusive worktree access from `ctx` before moving the
/// commit.
///
/// When `dry_run` is enabled, the returned workspace previews the moved commit
/// and no oplog entry is persisted. For details, see [`commit_move_with_perm()`].
#[but_api(napi, try_from = crate::commit::json::CommitMoveResult)]
#[instrument(err(Debug))]
pub fn commit_move(
    ctx: &mut but_ctx::Context,
    subject_commit_ids: Vec<gix::ObjectId>,
    #[but_api(crate::commit::json::RelativeTo)] relative_to: RelativeTo,
    side: InsertSide,
    dry_run: DryRun,
) -> anyhow::Result<CommitMoveResult> {
    let mut guard = ctx.exclusive_worktree_access();
    commit_move_with_perm(
        ctx,
        subject_commit_ids,
        relative_to,
        side,
        dry_run,
        guard.write_permission(),
    )
}

/// Moves `subject_commit_ids` to `side` of `relative_to` under caller-held
/// exclusive repository access and records an oplog snapshot on success.
///
/// It prepares a best-effort `MoveCommit` oplog snapshot, performs the move,
/// and commits the snapshot only if the operation succeeds. When `dry_run` is
/// enabled, it returns a preview of the resulting workspace state and skips
/// oplog persistence. For lower-level implementation details, see
/// [`but_workspace::commit::move_commit()`].
pub fn commit_move_with_perm(
    ctx: &mut but_ctx::Context,
    subject_commit_ids: Vec<gix::ObjectId>,
    relative_to: RelativeTo,
    side: InsertSide,
    dry_run: DryRun,
    perm: &mut RepoExclusive,
) -> anyhow::Result<CommitMoveResult> {
    let maybe_oplog_entry = but_oplog::UnmaterializedOplogSnapshot::from_details_with_perm(
        ctx,
        SnapshotDetails::new(OperationKind::MoveCommit),
        perm.read_permission(),
        dry_run,
    );

    let res = commit_move_only_with_perm(ctx, subject_commit_ids, relative_to, side, dry_run, perm);
    if let Some(snapshot) = maybe_oplog_entry
        && res.is_ok()
    {
        snapshot.commit(ctx, perm).ok();
    }
    res
}
