import { diffToHunkHeaders, findHunkDiff } from "$lib/hunks/hunk";
import { isDefined } from "@gitbutler/ui/utils/typeguards";
import type { UnifiedDiff } from "$lib/hunks/diff";
import type { DiffService } from "$lib/hunks/diffService.svelte";
import type { DiffSpec, HunkAssignment, HunkHeader } from "@gitbutler/but-sdk";
import type { TreeChange } from "@gitbutler/but-sdk";

/** Helper function that turns tree changes into a diff spec */
export function changesToDiffSpec(
	changes: TreeChange[],
	assignments?: Record<string, HunkAssignment[]>,
): DiffSpec[] {
	return changes.map((change) => {
		const previousPathBytes =
			change.status.type === "Rename" ? change.status.subject.previousPathBytes : null;
		const assignment = assignments?.[change.path];
		const hunkHeaders = assignment?.map((a) => a.hunkHeader).filter(isDefined) ?? [];

		return {
			previousPathBytes,
			pathBytes: change.pathBytes,
			hunkHeaders,
		};
	});
}

/**
 * Like `changesToDiffSpec` but routes each assignment's `hunkHeader` through
 * the same null-side-encoded form the new-commit pipeline produces.
 *
 * Crucial for sub-hunks (produced by `split_hunk`): their `hunkHeader` is a
 * synthesized natural-rendering header that does not appear in the worktree
 * diff verbatim, so the backend's `to_additive_hunks` rejects it as
 * "Missing diff spec association". Re-encoding via
 * `findHunkDiff` + `diffToHunkHeaders("commit")` produces the per-run
 * `(-old,N +0,0)` / `(-0,0 +new,N)` form the engine expects.
 *
 * Pass-through for natural hunks: `findHunkDiff` returns the natural hunk
 * diff and `diffToHunkHeaders` produces equivalent null-side headers.
 */
export async function changesToDiffSpecForCommit(
	projectId: string,
	changes: TreeChange[],
	assignments: Record<string, HunkAssignment[]> | undefined,
	diffService: DiffService,
): Promise<DiffSpec[]> {
	const out: DiffSpec[] = [];
	for (const change of changes) {
		const previousPathBytes =
			change.status.type === "Rename" ? change.status.subject.previousPathBytes : null;
		const pathAssignments = assignments?.[change.path] ?? [];

		const hunkHeaders: HunkHeader[] = [];
		let diff: UnifiedDiff | null = null;
		for (const assignment of pathAssignments) {
			if (!assignment.hunkHeader) continue;
			if (diff === null) {
				diff = await diffService.fetchDiff(projectId, change);
			}
			const hunkDiff = findHunkDiff(diff, assignment.hunkHeader);
			if (!hunkDiff) {
				// Stale selection — skip; matches the new-commit path's
				// behavior of dropping shifted hunks rather than failing.
				continue;
			}
			hunkHeaders.push(...diffToHunkHeaders(hunkDiff.diff, "commit"));
		}

		out.push({
			previousPathBytes,
			pathBytes: change.pathBytes,
			hunkHeaders,
		});
	}
	return out;
}

export function findEarliestConflict<T extends { hasConflicts?: boolean }>(
	commits: T[],
): T | undefined {
	if (!commits.length) return undefined;

	for (let i = commits.length - 1; i >= 0; i--) {
		const commit = commits[i]!;
		if (commit.hasConflicts) {
			return commit;
		}
	}

	return undefined;
}
