import { changesToDiffSpec, changesToDiffSpecForCommit } from "$lib/commits/utils";
import { describe, expect, test } from "vitest";
import type { UnifiedDiff } from "$lib/hunks/diff";
import type { DiffService } from "$lib/hunks/diffService.svelte";
import type { HunkAssignment, TreeChange } from "@gitbutler/but-sdk";

function makeChange(path: string): TreeChange {
	return {
		path,
		pathBytes: new TextEncoder().encode(path) as unknown as TreeChange["pathBytes"],
		status: {
			type: "Modification",
			subject: {
				previousState: { id: "x", kind: "Blob" },
				state: { id: "y", kind: "Blob" },
				flags: null,
			},
		},
	} as unknown as TreeChange;
}

/**
 * Build a fake `UnifiedDiff` carrying a single 3-row pure-add natural hunk
 * for `path`. Body: `+A`, `+B`, `+C` at new-side lines 1..3.
 */
function pureAddDiff(): UnifiedDiff {
	return {
		type: "Patch",
		subject: {
			isResultOfBinaryToText: false,
			lines: { added: 3, removed: 0 },
			hunks: [
				{
					oldStart: 1,
					oldLines: 0,
					newStart: 1,
					newLines: 3,
					diff: "@@ -1,0 +1,3 @@\n+A\n+B\n+C\n",
				},
			],
		},
	} as unknown as UnifiedDiff;
}

function fakeDiffService(diff: UnifiedDiff | null): DiffService {
	return {
		fetchDiff: async () => diff,
	} as unknown as DiffService;
}

describe.concurrent("changesToDiffSpec", () => {
	test("forwards the assignment's hunkHeader verbatim (legacy path)", () => {
		const assignments: Record<string, HunkAssignment[]> = {
			"foo.md": [
				{
					hunkHeader: { oldStart: 1, oldLines: 0, newStart: 2, newLines: 1 },
				} as unknown as HunkAssignment,
			],
		};
		const out = changesToDiffSpec([makeChange("foo.md")], assignments);
		expect(out).toHaveLength(1);
		expect(out[0]!.hunkHeaders).toEqual([
			{ oldStart: 1, oldLines: 0, newStart: 2, newLines: 1 },
		]);
	});
});

describe.concurrent("changesToDiffSpecForCommit", () => {
	test("re-encodes a sub-hunk synth header into a single null-side header", async () => {
		// Synth header for the middle row of a 3-row pure-add hunk:
		// (-1,0 +2,1). Old-side has start != 0 so `is_null()` would be false
		// on the backend, which would reject it as "Missing diff spec
		// association" if forwarded verbatim.
		const synthSubHunk: HunkAssignment = {
			hunkHeader: { oldStart: 1, oldLines: 0, newStart: 2, newLines: 1 },
		} as unknown as HunkAssignment;

		const out = await changesToDiffSpecForCommit(
			"proj",
			[makeChange("foo.md")],
			{ "foo.md": [synthSubHunk] },
			fakeDiffService(pureAddDiff()),
		);

		expect(out).toHaveLength(1);
		expect(out[0]!.hunkHeaders).toEqual([
			// Pure null-side: old_start=0, old_lines=0. Backend's
			// `to_additive_hunks` handles this via containment matching
			// against the natural worktree hunk.
			{ oldStart: 0, oldLines: 0, newStart: 2, newLines: 1 },
		]);
	});

	test("re-encodes a natural hunk into the same null-side runs", async () => {
		// A whole natural hunk (3 +rows) goes through the same encoder as
		// sub-hunks; the encoder collapses the contiguous adds into one run.
		const natural: HunkAssignment = {
			hunkHeader: { oldStart: 1, oldLines: 0, newStart: 1, newLines: 3 },
		} as unknown as HunkAssignment;

		const out = await changesToDiffSpecForCommit(
			"proj",
			[makeChange("foo.md")],
			{ "foo.md": [natural] },
			fakeDiffService(pureAddDiff()),
		);

		expect(out[0]!.hunkHeaders).toEqual([
			{ oldStart: 0, oldLines: 0, newStart: 1, newLines: 3 },
		]);
	});

	test("drops a stale assignment whose header doesn't match any hunk", async () => {
		// Header points at line 99, far outside the diff. Should be silently
		// dropped (matches the new-commit path's behavior).
		const stale: HunkAssignment = {
			hunkHeader: { oldStart: 99, oldLines: 0, newStart: 99, newLines: 1 },
		} as unknown as HunkAssignment;

		const out = await changesToDiffSpecForCommit(
			"proj",
			[makeChange("foo.md")],
			{ "foo.md": [stale] },
			fakeDiffService(pureAddDiff()),
		);

		expect(out[0]!.hunkHeaders).toEqual([]);
	});

	test("emits empty hunkHeaders when no assignments are provided for a path", async () => {
		const out = await changesToDiffSpecForCommit(
			"proj",
			[makeChange("foo.md")],
			{},
			fakeDiffService(pureAddDiff()),
		);

		expect(out).toHaveLength(1);
		expect(out[0]!.hunkHeaders).toEqual([]);
	});
});
