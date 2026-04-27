import { InjectionToken } from "@gitbutler/core/context";
import { isDefined } from "@gitbutler/ui/utils/typeguards";
import type { UnifiedDiff } from "$lib/hunks/diff";
import type { BackendApi } from "$lib/state/backendApi";
import type { TreeChange } from "@gitbutler/but-sdk";

export type ChangeDiff = {
	path: string;
	diff: UnifiedDiff | null;
};

export const DIFF_SERVICE = new InjectionToken<DiffService>("DiffService");

export class DiffService {
	constructor(private backendApi: BackendApi) {}

	getDiff(projectId: string, change: TreeChange, commitId?: string) {
		if (commitId) {
			return this.backendApi.endpoints.getDiffInCommit.useQuery({
				projectId,
				commitId,
				change,
			});
		}
		return this.backendApi.endpoints.getDiff.useQuery({ projectId, change });
	}

	listCommitOverrideAnchors(projectId: string, commitId: string, path: number[]) {
		return this.backendApi.endpoints.listCommitOverrideAnchors.useQuery({
			projectId,
			commitId,
			path,
		});
	}

	get assignHunk() {
		return this.backendApi.endpoints.assignHunk.mutate;
	}

	get splitHunk() {
		return this.backendApi.endpoints.splitHunk.mutate;
	}

	get unsplitHunk() {
		return this.backendApi.endpoints.unsplitHunk.mutate;
	}

	get splitHunkInCommit() {
		return this.backendApi.endpoints.splitHunkInCommit.mutate;
	}

	get unsplitHunkInCommit() {
		return this.backendApi.endpoints.unsplitHunkInCommit.mutate;
	}

	async fetchDiff(projectId: string, change: TreeChange, commitId?: string) {
		if (commitId) {
			const { getDiffInCommit } = this.backendApi.endpoints;
			return await getDiffInCommit.fetch({ projectId, commitId, change });
		}
		const { getDiff } = this.backendApi.endpoints;
		return await getDiff.fetch({ projectId, change });
	}

	getChanges(projectId: string, changes: TreeChange[]) {
		const args = changes.map((change) => ({ projectId, change }));
		const { getDiff } = this.backendApi.endpoints;
		return getDiff.useQueries(args, {
			transform: (data, args): ChangeDiff => ({ path: args.change.path, diff: data }),
		});
	}

	async fetchChanges(projectId: string, changes: TreeChange[]): Promise<ChangeDiff[]> {
		const args = changes.map((change) => ({ projectId, change }));
		const responses = await Promise.all(
			args.map((arg) =>
				this.backendApi.endpoints.getDiff.fetch(arg, {
					transform: (diff, args) => ({
						path: args.change.path,
						diff,
					}),
				}),
			),
		);
		return responses.filter(isDefined);
	}
}
