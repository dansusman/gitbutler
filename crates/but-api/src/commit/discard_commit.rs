use crate::WorkspaceState;
use but_api_macros::but_api;
use but_core::{DryRun, sync::RepoExclusive};
use but_oplog::legacy::{OperationKind, SnapshotDetails, Trailer};
use but_rebase::graph_rebase::Editor;
use tracing::instrument;

use crate::commit::types::CommitDiscardResult;

/// Discard `subject_commit_id` using the behavior described by
/// [`commit_discard_only_with_perm()`].
///
/// When `dry_run` is enabled, the returned workspace previews the discard
/// without materializing the rebase.
#[but_api(try_from = crate::commit::json::CommitDiscardResult)]
pub fn commit_discard_only(
    ctx: &mut but_ctx::Context,
    subject_commit_id: gix::ObjectId,
    dry_run: DryRun,
) -> anyhow::Result<CommitDiscardResult> {
    let mut guard = ctx.exclusive_worktree_access();
    commit_discard_only_with_perm(ctx, subject_commit_id, dry_run, guard.write_permission())
}

/// Discard `subject_commit_id` under caller-held exclusive repository access.
///
/// This materializes the discard rebase and returns the commit-ID mapping for
/// rewritten descendants. When `dry_run` is enabled, it returns the projected
/// workspace state without materializing the rebase. This variant does not
/// create an oplog entry. For lower-level implementation details, see
/// [`but_workspace::commit::discard_commit()`].
pub fn commit_discard_only_with_perm(
    ctx: &mut but_ctx::Context,
    subject_commit_id: gix::ObjectId,
    dry_run: DryRun,
    perm: &mut RepoExclusive,
) -> anyhow::Result<CommitDiscardResult> {
    let result = {
        let mut meta = ctx.meta()?;
        let (repo, mut ws, _) = ctx.workspace_mut_and_db_with_perm(perm)?;
        let editor = Editor::create(&mut ws, &mut meta, &repo)?;

        let rebase = but_workspace::commit::discard_commit(editor, subject_commit_id)?;

        let workspace = WorkspaceState::from_successful_rebase(rebase, &repo, dry_run)?;

        CommitDiscardResult {
            discarded_commit: subject_commit_id,
            workspace,
        }
    };
    super::sub_hunk::migrate_overrides_after_workspace_rewrite(
        ctx,
        &result.workspace,
        dry_run,
        perm,
    )?;
    Ok(result)
}

/// Discard `subject_commit_id` using the behavior described by
/// [`commit_discard_with_perm()`].
///
/// When `dry_run` is enabled, the returned workspace previews the discard and
/// no oplog entry is persisted.
#[but_api(napi, try_from = crate::commit::json::CommitDiscardResult)]
#[instrument(err(Debug))]
pub fn commit_discard(
    ctx: &mut but_ctx::Context,
    subject_commit_id: gix::ObjectId,
    dry_run: DryRun,
) -> anyhow::Result<CommitDiscardResult> {
    let mut guard = ctx.exclusive_worktree_access();
    commit_discard_with_perm(ctx, subject_commit_id, dry_run, guard.write_permission())
}

/// Discard `subject_commit_id` under caller-held exclusive repository access
/// and record an oplog snapshot on success.
///
/// This prepares a best-effort `DiscardCommit` oplog snapshot annotated with
/// `subject_commit_id`, discards the commit, and commits the snapshot only if
/// the operation succeeds. When `dry_run` is enabled, it returns the projected
/// workspace state and skips oplog persistence. For lower-level implementation
/// details, see
/// [`but_workspace::commit::discard_commit()`].
pub fn commit_discard_with_perm(
    ctx: &mut but_ctx::Context,
    subject_commit_id: gix::ObjectId,
    dry_run: DryRun,
    perm: &mut RepoExclusive,
) -> anyhow::Result<CommitDiscardResult> {
    let details = SnapshotDetails::new(OperationKind::DiscardCommit).with_trailers(vec![Trailer {
        key: "sha".to_string(),
        value: subject_commit_id.to_string(),
    }]);
    let maybe_oplog_entry = but_oplog::UnmaterializedOplogSnapshot::from_details_with_perm(
        ctx,
        details,
        perm.read_permission(),
        dry_run,
    );

    let res = commit_discard_only_with_perm(ctx, subject_commit_id, dry_run, perm);
    if let Some(snapshot) = maybe_oplog_entry
        && res.is_ok()
    {
        snapshot.commit(ctx, perm).ok();
    }
    res
}
