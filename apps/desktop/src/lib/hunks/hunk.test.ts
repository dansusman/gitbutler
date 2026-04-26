import {
	lineIdsToHunkHeaders,
	extractLineGroups,
	extractAllGroups,
	hunkContainsHunk,
	hunkContainsLine,
	getLineLocks,
	orderHeaders,
	diffToHunkHeaders,
	splitDiffHunkByHeaders,
	bodyRowRangeFromSelection,
	countDeltaRowsInRange,
	countBodyRows,
	expandRangeToAbsorbBlankAddRows,
} from "$lib/hunks/hunk";
import type { DiffHunk } from "@gitbutler/but-sdk";
import { describe, expect, test } from "vitest";
import type { LineId } from "@gitbutler/ui/utils/diffParsing";

describe.concurrent("lineIdsToHunkHeaders", () => {
	test("should return empty array when given no line IDs", () => {
		expect(lineIdsToHunkHeaders([], "", "discard")).toEqual([]);
		expect(lineIdsToHunkHeaders([], "", "commit")).toEqual([]);
	});

	test("should return a single hunk header when given a single line ID", () => {
		const lineIds = [{ oldLine: 2, newLine: undefined }];
		const hunkDiff = `@@ -1,3 +1,2 @@
  line 1
- line 2
  line 3
`;
		expect(lineIdsToHunkHeaders(lineIds, hunkDiff, "discard")).toEqual([
			{ oldStart: 2, oldLines: 1, newStart: 1, newLines: 2 },
		]);
		expect(lineIdsToHunkHeaders(lineIds, hunkDiff, "commit")).toEqual([
			{ oldStart: 2, oldLines: 1, newStart: 0, newLines: 0 },
		]);
	});

	test("can deal with a big diff and a neat selection", () => {
		const hunkDiff = `@@ -1,10 +1,12 @@
 1
 2
 3
- 4
+ new 4
 5
- 6
- 7
+ new 6
+ new 7
+ an extra line
+ another extra line
 8
 9
 10
`;
		const lineIds = [
			{ oldLine: 4, newLine: undefined }, // 4
			{ oldLine: undefined, newLine: 6 }, // new 6
			{ oldLine: undefined, newLine: 7 }, // new 7
		];
		expect(lineIdsToHunkHeaders(lineIds, hunkDiff, "discard")).toEqual([
			{ oldStart: 4, oldLines: 1, newStart: 1, newLines: 12 },
			{ oldStart: 1, oldLines: 10, newStart: 6, newLines: 2 },
		]);
		expect(lineIdsToHunkHeaders(lineIds, hunkDiff, "commit")).toEqual([
			{ oldStart: 4, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 6, newLines: 2 },
		]);
	});

	test("can deal with a big diff and an overlapping selection", () => {
		const hunkDiff = `@@ -1,10 +1,12 @@
 1
 2
 3
- 4
+ new 4
 5
- 6
- 7
+ new 6
+ new 7
+ an extra line
+ another extra line
 8
 9
 10
`;
		const lineIds = [
			{ oldLine: 4, newLine: undefined }, // 4
			{ oldLine: undefined, newLine: 4 }, // new 4
			{ oldLine: 6, newLine: undefined }, // 6
			{ oldLine: undefined, newLine: 6 }, // new 6
			{ oldLine: undefined, newLine: 7 }, // new 7
		];
		expect(lineIdsToHunkHeaders(lineIds, hunkDiff, "discard")).toEqual([
			{ oldStart: 4, oldLines: 1, newStart: 1, newLines: 12 },
			{ oldStart: 1, oldLines: 10, newStart: 4, newLines: 1 },
			{ oldStart: 6, oldLines: 1, newStart: 1, newLines: 12 },
			{ oldStart: 1, oldLines: 10, newStart: 6, newLines: 2 },
		]);
		expect(lineIdsToHunkHeaders(lineIds, hunkDiff, "commit")).toEqual([
			{ oldStart: 4, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 4, newLines: 1 },
			{ oldStart: 6, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 6, newLines: 2 },
		]);
	});

	test("can deal with a big diff and an overlapping selection, unordered", () => {
		const hunkDiff = `@@ -1,10 +1,12 @@
 1
 2
 3
- 4
+ new 4
 5
- 6
- 7
+ new 6
+ new 7
+ an extra line
+ another extra line
 8
 9
 10
`;
		const lineIds = [
			{ oldLine: undefined, newLine: 7 }, // new 7
			{ oldLine: undefined, newLine: 4 }, // new 4
			{ oldLine: 6, newLine: undefined }, // 6
			{ oldLine: undefined, newLine: 6 }, // new 6
			{ oldLine: 4, newLine: undefined }, // 4
		];
		expect(lineIdsToHunkHeaders(lineIds, hunkDiff, "discard")).toEqual([
			{ oldStart: 4, oldLines: 1, newStart: 1, newLines: 12 },
			{ oldStart: 1, oldLines: 10, newStart: 4, newLines: 1 },
			{ oldStart: 6, oldLines: 1, newStart: 1, newLines: 12 },
			{ oldStart: 1, oldLines: 10, newStart: 6, newLines: 2 },
		]);
		expect(lineIdsToHunkHeaders(lineIds, hunkDiff, "commit")).toEqual([
			{ oldStart: 4, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 4, newLines: 1 },
			{ oldStart: 6, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 6, newLines: 2 },
		]);
	});
});

describe.concurrent("extractLineGroups", () => {
	test("should return an empty array when given no line IDs", () => {
		const hunkDiff = `@@ -1,4 +1,2 @@
- line 1
- line 2
+ new line 1
+ new line 2
- line 3
- line 4
`;
		expect(extractLineGroups([], hunkDiff)).toEqual([
			[],
			{
				oldStart: 1,
				oldLines: 4,
				newStart: 1,
				newLines: 2,
			},
		]);
	});

	test("should return the correct line groups for each type", () => {
		const lineIds: LineId[] = [
			{ oldLine: 1, newLine: undefined },
			{ oldLine: 2, newLine: undefined },
			{ oldLine: undefined, newLine: 1 },
			{ oldLine: undefined, newLine: 2 },
			{ oldLine: 3, newLine: undefined },
			{ oldLine: 4, newLine: undefined },
		];

		const hunkDiff = `@@ -1,4 +1,2 @@
- line 1
- line 2
+ new line 1
+ new line 2
- line 3
- line 4
`;

		expect(extractLineGroups(lineIds, hunkDiff)).toEqual([
			[
				{ type: "removed", lines: [lineIds[0], lineIds[1]] },
				{ type: "added", lines: [lineIds[2], lineIds[3]] },
				{ type: "removed", lines: [lineIds[4], lineIds[5]] },
			],
			{
				oldStart: 1,
				oldLines: 4,
				newStart: 1,
				newLines: 2,
			},
		]);
	});

	test("should be able to deal with non-consecutive line numbers", () => {
		const lineIds: LineId[] = [
			{ oldLine: 1, newLine: undefined },
			{ oldLine: 3, newLine: undefined },
			{ oldLine: undefined, newLine: 2 },
			{ oldLine: 4, newLine: undefined },
		];

		const hunkDiff = `@@ -1,4 +1,2 @@
- line 1
line 2
- line 3
+ new line 2
- line 4
`;

		expect(extractLineGroups(lineIds, hunkDiff)).toEqual([
			[
				{ type: "removed", lines: [lineIds[0]] },
				{ type: "removed", lines: [lineIds[1]] },
				{ type: "added", lines: [lineIds[2]] },
				{ type: "removed", lines: [lineIds[3]] },
			],
			{
				oldStart: 1,
				oldLines: 4,
				newStart: 1,
				newLines: 2,
			},
		]);
	});

	test("should be able to deal with non-consecutive, out of orderline numbers", () => {
		const lineIds: LineId[] = [
			{ oldLine: 3, newLine: undefined },
			{ oldLine: 4, newLine: undefined },
			{ oldLine: undefined, newLine: 2 },
			{ oldLine: 1, newLine: undefined },
		];

		const hunkDiff = `@@ -1,4 +1,2 @@
- line 1
line 2
- line 3
+ new line 2
- line 4
`;

		expect(extractLineGroups(lineIds, hunkDiff)).toEqual([
			[
				{ type: "removed", lines: [lineIds[3]] },
				{ type: "removed", lines: [lineIds[0]] },
				{ type: "added", lines: [lineIds[2]] },
				{ type: "removed", lines: [lineIds[1]] },
			],
			{
				oldStart: 1,
				oldLines: 4,
				newStart: 1,
				newLines: 2,
			},
		]);
	});
});

describe("hunkContainsHunk", () => {
	const baseHunk = {
		oldStart: 10,
		oldLines: 10,
		newStart: 20,
		newLines: 10,
		diff: "",
	};
	test("returns true if hunk b is fully inside hunk a", () => {
		const inner = { ...baseHunk, oldStart: 12, oldLines: 5, newStart: 22, newLines: 5, diff: "" };
		expect(hunkContainsHunk(baseHunk, inner)).toBe(true);
	});
	test("returns false if hunk b is not fully inside hunk a", () => {
		const outer = { ...baseHunk, oldStart: 5, oldLines: 20, newStart: 15, newLines: 20, diff: "" };
		expect(hunkContainsHunk(baseHunk, outer)).toBe(false);
	});
	test("returns true when hunk b ends at the exact same line as hunk a", () => {
		// Hunk a: oldStart=10, oldLines=10 -> covers old lines 10-19
		// Hunk b: oldStart=15, oldLines=5 -> covers old lines 15-19 (ends at same line)
		const inner = { oldStart: 15, oldLines: 5, newStart: 25, newLines: 5, diff: "" };
		expect(hunkContainsHunk(baseHunk, inner)).toBe(true);
	});
	test("returns false when hunk b extends beyond hunk a by one line", () => {
		// Hunk a: oldStart=10, oldLines=10 -> covers old lines 10-19
		// Hunk b: oldStart=15, oldLines=6 -> covers old lines 15-20 (extends beyond by 1)
		const extending = {
			oldStart: 15,
			oldLines: 6,
			newStart: 25,
			newLines: 6,
			diff: "",
		};
		expect(hunkContainsHunk(baseHunk, extending)).toBe(false);
	});
	test("returns true when ranges are identical", () => {
		expect(hunkContainsHunk(baseHunk, baseHunk)).toBe(true);
	});
});

describe("hunkContainsLine", () => {
	const hunk = { oldStart: 5, oldLines: 5, newStart: 10, newLines: 5, diff: "" };
	test("returns true for a line inside the hunk (old line)", () => {
		expect(hunkContainsLine(hunk, { oldLine: 7, newLine: undefined })).toBe(true);
	});
	test("returns true for a line inside the hunk (new line)", () => {
		expect(hunkContainsLine(hunk, { oldLine: undefined, newLine: 12 })).toBe(true);
	});
	test("returns false for a line outside the hunk", () => {
		expect(hunkContainsLine(hunk, { oldLine: 20, newLine: undefined })).toBe(false);
	});
	test("returns true for a line with both old and new inside", () => {
		expect(hunkContainsLine(hunk, { oldLine: 6, newLine: 11 })).toBe(true);
	});
	test("returns false for a line with both old and new outside", () => {
		expect(hunkContainsLine(hunk, { oldLine: 1, newLine: 1 })).toBe(false);
	});
});

describe("getLineLocks", () => {
	const diff = `@@ -1,3 +1,3 @@\n line 1\n-line 2\n+line 2 changed\n line 3`;
	const hunk = { oldStart: 1, oldLines: 3, newStart: 1, newLines: 3, diff };
	const locks = [
		{
			hunk: { oldStart: 2, oldLines: 1, newStart: 2, newLines: 1, diff },
			locks: [{ target: { type: "stack" as const, subject: "stack1" }, commitId: "commit1" }],
		},
	];
	test("returns line locks for lines covered by locks", () => {
		const [fullyLocked, lineLocks] = getLineLocks(hunk, locks);
		expect(fullyLocked).toBe(true);
		expect(lineLocks).toEqual([
			{
				oldLine: 2,
				newLine: undefined,
				locks: [{ target: { type: "stack", subject: "stack1" }, commitId: "commit1" }],
			},
			{
				oldLine: undefined,
				newLine: 2,
				locks: [{ target: { type: "stack", subject: "stack1" }, commitId: "commit1" }],
			},
		]);
	});
	test("returns empty array if no locks match", () => {
		const noLocks = [
			{
				hunk: { oldStart: 10, oldLines: 1, newStart: 10, newLines: 1, diff: "" },
				locks: [{ target: { type: "stack" as const, subject: "stack2" }, commitId: "commit2" }],
			},
		];
		const [fullyLocked, lineLocks] = getLineLocks(hunk, noLocks);
		expect(fullyLocked).toBe(false);
		expect(lineLocks).toEqual([]);
	});

	test("returns partially locked for hunks with only some lines covered", () => {
		// Diff with three changed lines (lines 2, 3, 4)
		const partialDiff = `@@ -1,5 +1,5 @@\n line 1\n-line 2\n-line 3\n-line 4\n+line 2 changed\n+line 3 changed\n+line 4 changed\n line 5`;
		const partialHunk = { oldStart: 1, oldLines: 5, newStart: 1, newLines: 5, diff: partialDiff };
		const partialLocks = [
			{
				hunk: { oldStart: 3, oldLines: 1, newStart: 3, newLines: 1, diff: partialDiff },
				locks: [{ target: { type: "stack" as const, subject: "stack1" }, commitId: "commit1" }],
			},
		];
		// Only line 3 is locked, lines 2 and 4 are not
		const [fullyLocked, lineLocks] = getLineLocks(partialHunk, partialLocks);
		expect(fullyLocked).toBe(false);
		expect(lineLocks).toEqual([
			{
				oldLine: 3,
				newLine: undefined,
				locks: [{ target: { type: "stack", subject: "stack1" }, commitId: "commit1" }],
			},
			{
				oldLine: undefined,
				newLine: 3,
				locks: [{ target: { type: "stack", subject: "stack1" }, commitId: "commit1" }],
			},
		]);
	});
});

describe("orderHeaders", () => {
	test("should properly order the headers", () => {
		const headers = [
			{
				oldStart: 0,
				oldLines: 0,
				newStart: 3,
				newLines: 1,
			},
			{
				oldStart: 0,
				oldLines: 0,
				newStart: 5,
				newLines: 1,
			},
			{
				oldStart: 3,
				oldLines: 1,
				newStart: 0,
				newLines: 0,
			},
			{
				oldStart: 5,
				oldLines: 1,
				newStart: 0,
				newLines: 0,
			},
		];

		const orderedHeaders = headers.sort(orderHeaders);

		expect(orderedHeaders).toEqual([
			{
				oldStart: 0,
				oldLines: 0,
				newStart: 3,
				newLines: 1,
			},
			{
				oldStart: 3,
				oldLines: 1,
				newStart: 0,
				newLines: 0,
			},
			{
				oldStart: 0,
				oldLines: 0,
				newStart: 5,
				newLines: 1,
			},
			{
				oldStart: 5,
				oldLines: 1,
				newStart: 0,
				newLines: 0,
			},
		]);
	});

	test("should order headers with mixed zeroed and non-zeroed starts", () => {
		const headers = [
			{ oldStart: 0, oldLines: 0, newStart: 10, newLines: 2 },
			{ oldStart: 2, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 1, newLines: 1 },
			{ oldStart: 5, oldLines: 2, newStart: 0, newLines: 0 },
		];
		const ordered = headers.sort(orderHeaders);
		expect(ordered).toEqual([
			{ oldStart: 0, oldLines: 0, newStart: 1, newLines: 1 },
			{ oldStart: 2, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 5, oldLines: 2, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 10, newLines: 2 },
		]);
	});

	test("should order headers with all oldStart zeroed", () => {
		const headers = [
			{ oldStart: 0, oldLines: 0, newStart: 8, newLines: 1 },
			{ oldStart: 0, oldLines: 0, newStart: 2, newLines: 1 },
			{ oldStart: 0, oldLines: 0, newStart: 5, newLines: 1 },
		];
		const ordered = headers.sort(orderHeaders);
		expect(ordered).toEqual([
			{ oldStart: 0, oldLines: 0, newStart: 2, newLines: 1 },
			{ oldStart: 0, oldLines: 0, newStart: 5, newLines: 1 },
			{ oldStart: 0, oldLines: 0, newStart: 8, newLines: 1 },
		]);
	});

	test("should order headers with all newStart zeroed", () => {
		const headers = [
			{ oldStart: 7, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 2, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 5, oldLines: 1, newStart: 0, newLines: 0 },
		];
		const ordered = headers.sort(orderHeaders);
		expect(ordered).toEqual([
			{ oldStart: 2, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 5, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 7, oldLines: 1, newStart: 0, newLines: 0 },
		]);
	});

	test("should handle headers with both starts zeroed (should remain stable)", () => {
		const headers = [
			{ oldStart: 0, oldLines: 0, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 0, newLines: 0 },
		];
		const ordered = headers.sort(orderHeaders);
		expect(ordered).toEqual([
			{ oldStart: 0, oldLines: 0, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 0, newLines: 0 },
		]);
	});

	test("should order headers with negative values", () => {
		const headers = [
			{ oldStart: -2, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: -5, newLines: 1 },
			{ oldStart: 1, oldLines: 1, newStart: 0, newLines: 0 },
		];
		const ordered = headers.sort(orderHeaders);
		expect(ordered).toEqual([
			{ oldStart: 0, oldLines: 0, newStart: -5, newLines: 1 },
			{ oldStart: -2, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 1, oldLines: 1, newStart: 0, newLines: 0 },
		]);
	});
});

describe.concurrent("extractAllGroups", () => {
	test("should extract all added and removed lines from a simple diff", () => {
		const hunkDiff = `@@ -1,3 +1,3 @@
 line 1
-line 2
+line 2 changed
 line 3
`;
		const [lineGroups, parentHunkHeader] = extractAllGroups(hunkDiff);

		expect(parentHunkHeader).toEqual({
			oldStart: 1,
			oldLines: 3,
			newStart: 1,
			newLines: 3,
		});

		expect(lineGroups).toEqual([
			{
				type: "removed",
				lines: [{ oldLine: 2, newLine: undefined }],
			},
			{
				type: "added",
				lines: [{ oldLine: undefined, newLine: 2 }],
			},
		]);
	});

	test("should extract multiple consecutive removed lines", () => {
		const hunkDiff = `@@ -1,4 +1,3 @@
 line 1
-line 2
-line 3
-line 4
+new line 2
+new line 3
`;
		const [lineGroups, parentHunkHeader] = extractAllGroups(hunkDiff);

		expect(parentHunkHeader).toEqual({
			oldStart: 1,
			oldLines: 4,
			newStart: 1,
			newLines: 3,
		});

		expect(lineGroups).toEqual([
			{
				type: "removed",
				lines: [
					{ oldLine: 2, newLine: undefined },
					{ oldLine: 3, newLine: undefined },
					{ oldLine: 4, newLine: undefined },
				],
			},
			{
				type: "added",
				lines: [
					{ oldLine: undefined, newLine: 2 },
					{ oldLine: undefined, newLine: 3 },
				],
			},
		]);
	});

	test("should extract multiple consecutive added lines", () => {
		const hunkDiff = `@@ -1,3 +1,4 @@
 line 1
-old line
+new line 2
+new line 3
+new line 4
 line 5
`;
		const [lineGroups, parentHunkHeader] = extractAllGroups(hunkDiff);

		expect(parentHunkHeader).toEqual({
			oldStart: 1,
			oldLines: 3,
			newStart: 1,
			newLines: 4,
		});

		expect(lineGroups).toEqual([
			{
				type: "removed",
				lines: [{ oldLine: 2, newLine: undefined }],
			},
			{
				type: "added",
				lines: [
					{ oldLine: undefined, newLine: 2 },
					{ oldLine: undefined, newLine: 3 },
					{ oldLine: undefined, newLine: 4 },
				],
			},
		]);
	});

	test("should group non-consecutive changes separately", () => {
		const hunkDiff = `@@ -1,6 +1,6 @@
 line 1
-line 2
+line 2 changed
 line 3
 line 4
-line 5
+line 5 changed
 line 6
`;
		const [lineGroups, parentHunkHeader] = extractAllGroups(hunkDiff);

		expect(parentHunkHeader).toEqual({
			oldStart: 1,
			oldLines: 6,
			newStart: 1,
			newLines: 6,
		});

		expect(lineGroups).toEqual([
			{
				type: "removed",
				lines: [{ oldLine: 2, newLine: undefined }],
			},
			{
				type: "added",
				lines: [{ oldLine: undefined, newLine: 2 }],
			},
			{
				type: "removed",
				lines: [{ oldLine: 5, newLine: undefined }],
			},
			{
				type: "added",
				lines: [{ oldLine: undefined, newLine: 5 }],
			},
		]);
	});

	test("should handle diff with only added lines", () => {
		const hunkDiff = `@@ -1,2 +1,4 @@
 line 1
+new line 2
+new line 3
 line 4
`;
		const [lineGroups, parentHunkHeader] = extractAllGroups(hunkDiff);

		expect(parentHunkHeader).toEqual({
			oldStart: 1,
			oldLines: 2,
			newStart: 1,
			newLines: 4,
		});

		expect(lineGroups).toEqual([
			{
				type: "added",
				lines: [
					{ oldLine: undefined, newLine: 2 },
					{ oldLine: undefined, newLine: 3 },
				],
			},
		]);
	});

	test("should handle diff with only removed lines", () => {
		const hunkDiff = `@@ -1,4 +1,2 @@
 line 1
-line 2
-line 3
 line 4
`;
		const [lineGroups, parentHunkHeader] = extractAllGroups(hunkDiff);

		expect(parentHunkHeader).toEqual({
			oldStart: 1,
			oldLines: 4,
			newStart: 1,
			newLines: 2,
		});

		expect(lineGroups).toEqual([
			{
				type: "removed",
				lines: [
					{ oldLine: 2, newLine: undefined },
					{ oldLine: 3, newLine: undefined },
				],
			},
		]);
	});

	test("should handle diff with only context lines (no changes)", () => {
		const hunkDiff = `@@ -1,3 +1,3 @@
 line 1
 line 2
 line 3
`;
		const [lineGroups, parentHunkHeader] = extractAllGroups(hunkDiff);

		expect(parentHunkHeader).toEqual({
			oldStart: 1,
			oldLines: 3,
			newStart: 1,
			newLines: 3,
		});

		expect(lineGroups).toEqual([]);
	});

	test("should handle complex diff with multiple change groups", () => {
		const hunkDiff = `@@ -1,10 +1,12 @@
 1
 2
 3
- 4
+ new 4
 5
- 6
- 7
+ new 6
+ new 7
+ an extra line
+ another extra line
 8
 9
 10
`;
		const [lineGroups, parentHunkHeader] = extractAllGroups(hunkDiff);

		expect(parentHunkHeader).toEqual({
			oldStart: 1,
			oldLines: 10,
			newStart: 1,
			newLines: 12,
		});

		expect(lineGroups).toEqual([
			{
				type: "removed",
				lines: [{ oldLine: 4, newLine: undefined }],
			},
			{
				type: "added",
				lines: [{ oldLine: undefined, newLine: 4 }],
			},
			{
				type: "removed",
				lines: [
					{ oldLine: 6, newLine: undefined },
					{ oldLine: 7, newLine: undefined },
				],
			},
			{
				type: "added",
				lines: [
					{ oldLine: undefined, newLine: 6 },
					{ oldLine: undefined, newLine: 7 },
					{ oldLine: undefined, newLine: 8 },
					{ oldLine: undefined, newLine: 9 },
				],
			},
		]);
	});

	test("should handle file deletion (all lines removed)", () => {
		const hunkDiff = `@@ -1,3 +0,0 @@
-line 1
-line 2
-line 3
`;
		const [lineGroups, parentHunkHeader] = extractAllGroups(hunkDiff);

		expect(parentHunkHeader).toEqual({
			oldStart: 1,
			oldLines: 3,
			newStart: 0,
			newLines: 0,
		});

		expect(lineGroups).toEqual([
			{
				type: "removed",
				lines: [
					{ oldLine: 1, newLine: undefined },
					{ oldLine: 2, newLine: undefined },
					{ oldLine: 3, newLine: undefined },
				],
			},
		]);
	});

	test("should handle new file creation (all lines added)", () => {
		const hunkDiff = `@@ -0,0 +1,3 @@
+line 1
+line 2
+line 3
`;
		const [lineGroups, parentHunkHeader] = extractAllGroups(hunkDiff);

		expect(parentHunkHeader).toEqual({
			oldStart: 0,
			oldLines: 0,
			newStart: 1,
			newLines: 3,
		});

		expect(lineGroups).toEqual([
			{
				type: "added",
				lines: [
					{ oldLine: undefined, newLine: 1 },
					{ oldLine: undefined, newLine: 2 },
					{ oldLine: undefined, newLine: 3 },
				],
			},
		]);
	});
});

describe.concurrent("diffToHunkHeaders", () => {
	test("should return empty array for diff with no changes", () => {
		const hunkDiff = `@@ -1,3 +1,3 @@
 line 1
 line 2
 line 3
`;
		expect(diffToHunkHeaders(hunkDiff, "discard")).toEqual([]);
		expect(diffToHunkHeaders(hunkDiff, "commit")).toEqual([]);
	});

	test("should convert single removed line to hunk header for discard action", () => {
		const hunkDiff = `@@ -1,3 +1,2 @@
 line 1
-line 2
 line 3
`;
		expect(diffToHunkHeaders(hunkDiff, "discard")).toEqual([
			{ oldStart: 2, oldLines: 1, newStart: 1, newLines: 2 },
		]);
	});

	test("should convert single removed line to hunk header for commit action", () => {
		const hunkDiff = `@@ -1,3 +1,2 @@
 line 1
-line 2
 line 3
`;
		expect(diffToHunkHeaders(hunkDiff, "commit")).toEqual([
			{ oldStart: 2, oldLines: 1, newStart: 0, newLines: 0 },
		]);
	});

	test("should convert single added line to hunk header for discard action", () => {
		const hunkDiff = `@@ -1,2 +1,3 @@
 line 1
+new line 2
 line 3
`;
		expect(diffToHunkHeaders(hunkDiff, "discard")).toEqual([
			{ oldStart: 1, oldLines: 2, newStart: 2, newLines: 1 },
		]);
	});

	test("should convert single added line to hunk header for commit action", () => {
		const hunkDiff = `@@ -1,2 +1,3 @@
 line 1
+new line 2
 line 3
`;
		expect(diffToHunkHeaders(hunkDiff, "commit")).toEqual([
			{ oldStart: 0, oldLines: 0, newStart: 2, newLines: 1 },
		]);
	});

	test("should convert multiple consecutive removed lines", () => {
		const hunkDiff = `@@ -1,5 +1,2 @@
 line 1
-line 2
-line 3
-line 4
 line 5
`;
		expect(diffToHunkHeaders(hunkDiff, "discard")).toEqual([
			{ oldStart: 2, oldLines: 3, newStart: 1, newLines: 2 },
		]);
		expect(diffToHunkHeaders(hunkDiff, "commit")).toEqual([
			{ oldStart: 2, oldLines: 3, newStart: 0, newLines: 0 },
		]);
	});

	test("should convert multiple consecutive added lines", () => {
		const hunkDiff = `@@ -1,2 +1,5 @@
 line 1
+new line 2
+new line 3
+new line 4
 line 5
`;
		expect(diffToHunkHeaders(hunkDiff, "discard")).toEqual([
			{ oldStart: 1, oldLines: 2, newStart: 2, newLines: 3 },
		]);
		expect(diffToHunkHeaders(hunkDiff, "commit")).toEqual([
			{ oldStart: 0, oldLines: 0, newStart: 2, newLines: 3 },
		]);
	});

	test("should convert mixed add/remove to separate hunk headers", () => {
		const hunkDiff = `@@ -1,3 +1,3 @@
 line 1
-line 2
+line 2 changed
 line 3
`;
		expect(diffToHunkHeaders(hunkDiff, "discard")).toEqual([
			{ oldStart: 2, oldLines: 1, newStart: 1, newLines: 3 },
			{ oldStart: 1, oldLines: 3, newStart: 2, newLines: 1 },
		]);
		expect(diffToHunkHeaders(hunkDiff, "commit")).toEqual([
			{ oldStart: 2, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 2, newLines: 1 },
		]);
	});

	test("should handle multiple separate change groups", () => {
		const hunkDiff = `@@ -1,6 +1,6 @@
 line 1
-line 2
+line 2 changed
 line 3
 line 4
-line 5
+line 5 changed
 line 6
`;
		expect(diffToHunkHeaders(hunkDiff, "discard")).toEqual([
			{ oldStart: 2, oldLines: 1, newStart: 1, newLines: 6 },
			{ oldStart: 1, oldLines: 6, newStart: 2, newLines: 1 },
			{ oldStart: 5, oldLines: 1, newStart: 1, newLines: 6 },
			{ oldStart: 1, oldLines: 6, newStart: 5, newLines: 1 },
		]);
		expect(diffToHunkHeaders(hunkDiff, "commit")).toEqual([
			{ oldStart: 2, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 2, newLines: 1 },
			{ oldStart: 5, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 5, newLines: 1 },
		]);
	});

	test("should handle complex diff with multiple change types", () => {
		const hunkDiff = `@@ -1,10 +1,12 @@
 1
 2
 3
- 4
+ new 4
 5
- 6
- 7
+ new 6
+ new 7
+ an extra line
+ another extra line
 8
 9
 10
`;
		expect(diffToHunkHeaders(hunkDiff, "discard")).toEqual([
			{ oldStart: 4, oldLines: 1, newStart: 1, newLines: 12 },
			{ oldStart: 1, oldLines: 10, newStart: 4, newLines: 1 },
			{ oldStart: 6, oldLines: 2, newStart: 1, newLines: 12 },
			{ oldStart: 1, oldLines: 10, newStart: 6, newLines: 4 },
		]);
		expect(diffToHunkHeaders(hunkDiff, "commit")).toEqual([
			{ oldStart: 4, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 4, newLines: 1 },
			{ oldStart: 6, oldLines: 2, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 6, newLines: 4 },
		]);
	});

	test("should handle file deletion (all lines removed)", () => {
		const hunkDiff = `@@ -1,3 +0,0 @@
-line 1
-line 2
-line 3
`;
		expect(diffToHunkHeaders(hunkDiff, "discard")).toEqual([
			{ oldStart: 1, oldLines: 3, newStart: 0, newLines: 0 },
		]);
		expect(diffToHunkHeaders(hunkDiff, "commit")).toEqual([
			{ oldStart: 1, oldLines: 3, newStart: 0, newLines: 0 },
		]);
	});

	test("should handle new file creation (all lines added)", () => {
		const hunkDiff = `@@ -0,0 +1,3 @@
+line 1
+line 2
+line 3
`;
		expect(diffToHunkHeaders(hunkDiff, "discard")).toEqual([
			{ oldStart: 0, oldLines: 0, newStart: 1, newLines: 3 },
		]);
		expect(diffToHunkHeaders(hunkDiff, "commit")).toEqual([
			{ oldStart: 0, oldLines: 0, newStart: 1, newLines: 3 },
		]);
	});

	test("should handle diff with only removals", () => {
		const hunkDiff = `@@ -1,5 +1,2 @@
 line 1
-line 2
-line 3
-line 4
 line 5
`;
		expect(diffToHunkHeaders(hunkDiff, "discard")).toEqual([
			{ oldStart: 2, oldLines: 3, newStart: 1, newLines: 2 },
		]);
		expect(diffToHunkHeaders(hunkDiff, "commit")).toEqual([
			{ oldStart: 2, oldLines: 3, newStart: 0, newLines: 0 },
		]);
	});

	test("should handle diff with only additions", () => {
		const hunkDiff = `@@ -1,2 +1,5 @@
 line 1
+new line 2
+new line 3
+new line 4
 line 5
`;
		expect(diffToHunkHeaders(hunkDiff, "discard")).toEqual([
			{ oldStart: 1, oldLines: 2, newStart: 2, newLines: 3 },
		]);
		expect(diffToHunkHeaders(hunkDiff, "commit")).toEqual([
			{ oldStart: 0, oldLines: 0, newStart: 2, newLines: 3 },
		]);
	});

	test("should handle diff at the beginning of file", () => {
		const hunkDiff = `@@ -1,3 +1,4 @@
-old first line
+new first line
+another new line
 line 2
 line 3
`;
		expect(diffToHunkHeaders(hunkDiff, "discard")).toEqual([
			{ oldStart: 1, oldLines: 1, newStart: 1, newLines: 4 },
			{ oldStart: 1, oldLines: 3, newStart: 1, newLines: 2 },
		]);
		expect(diffToHunkHeaders(hunkDiff, "commit")).toEqual([
			{ oldStart: 1, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 1, newLines: 2 },
		]);
	});

	test("should handle diff at the end of file", () => {
		const hunkDiff = `@@ -1,3 +1,4 @@
 line 1
 line 2
-old last line
+new last line
+another new line
`;
		expect(diffToHunkHeaders(hunkDiff, "discard")).toEqual([
			{ oldStart: 3, oldLines: 1, newStart: 1, newLines: 4 },
			{ oldStart: 1, oldLines: 3, newStart: 3, newLines: 2 },
		]);
		expect(diffToHunkHeaders(hunkDiff, "commit")).toEqual([
			{ oldStart: 3, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 3, newLines: 2 },
		]);
	});

	test("should handle alternating additions and removals", () => {
		const hunkDiff = `@@ -1,5 +1,5 @@
 line 1
-line 2
+new line 2
-line 3
+new line 3
 line 5
`;
		expect(diffToHunkHeaders(hunkDiff, "discard")).toEqual([
			{ oldStart: 2, oldLines: 1, newStart: 1, newLines: 5 },
			{ oldStart: 1, oldLines: 5, newStart: 2, newLines: 1 },
			{ oldStart: 3, oldLines: 1, newStart: 1, newLines: 5 },
			{ oldStart: 1, oldLines: 5, newStart: 3, newLines: 1 },
		]);
		expect(diffToHunkHeaders(hunkDiff, "commit")).toEqual([
			{ oldStart: 2, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 2, newLines: 1 },
			{ oldStart: 3, oldLines: 1, newStart: 0, newLines: 0 },
			{ oldStart: 0, oldLines: 0, newStart: 3, newLines: 1 },
		]);
	});
});

describe.concurrent("splitDiffHunkByHeaders", () => {
	const natural = {
		oldStart: 10,
		oldLines: 5,
		newStart: 10,
		newLines: 5,
		// 6 body rows: ctx, -, +, -, +, ctx
		diff: `@@ -10,5 +10,5 @@
 c1
-r1
+a1
-r2
+a2
 c2
`,
	};

	test("returns natural unchanged when no headers are supplied", () => {
		const result = splitDiffHunkByHeaders(natural, []);
		expect(result).toEqual([{ hunk: natural }]);
		expect(result[0]!.anchor).toBeUndefined();
	});

	test("returns natural unchanged when only header equals natural", () => {
		const result = splitDiffHunkByHeaders(natural, [
			{ oldStart: 10, oldLines: 5, newStart: 10, newLines: 5 },
		]);
		expect(result).toEqual([{ hunk: natural }]);
		expect(result[0]!.anchor).toBeUndefined();
	});

	test("three-way split produces three sub-hunks covering all rows", () => {
		// Sub-headers as synthesize_header would produce them for ranges
		// 0..1 (ctx), 1..3 (-r +a), 3..6 (-r +a ctx).
		const subs = [
			// row 0: ctx -> old/new = 10..11
			{ oldStart: 10, oldLines: 1, newStart: 10, newLines: 1 },
			// rows 1..3: -r +a -> old 11..12, new 11..12
			{ oldStart: 11, oldLines: 1, newStart: 11, newLines: 1 },
			// rows 3..6: -r +a ctx -> old 12..15, new 12..15
			{ oldStart: 12, oldLines: 3, newStart: 12, newLines: 3 },
		];
		const result = splitDiffHunkByHeaders(natural, subs);
		expect(result).toHaveLength(3);
		expect(result[0]!.hunk.diff).toBe("@@ -10,1 +10,1 @@\n c1\n");
		expect(result[1]!.hunk.diff).toBe("@@ -11,1 +11,1 @@\n-r1\n+a1\n");
		expect(result[2]!.hunk.diff).toBe("@@ -12,3 +12,3 @@\n-r2\n+a2\n c2\n");
		for (const item of result) {
			expect(item.anchor).toEqual({
				oldStart: 10,
				oldLines: 5,
				newStart: 10,
				newLines: 5,
			});
		}
	});

	test("pure-add sub-hunk picks only the + row", () => {
		const pureAdd = {
			oldStart: 1,
			oldLines: 0,
			newStart: 1,
			newLines: 3,
			diff: `@@ -1,0 +1,3 @@
+a1
+a2
+a3
`,
		};
		const result = splitDiffHunkByHeaders(pureAdd, [
			{ oldStart: 1, oldLines: 0, newStart: 1, newLines: 1 },
			{ oldStart: 1, oldLines: 0, newStart: 2, newLines: 2 },
		]);
		expect(result).toHaveLength(2);
		expect(result[0]!.hunk.diff).toBe("@@ -1,0 +1,1 @@\n+a1\n");
		expect(result[1]!.hunk.diff).toBe("@@ -1,0 +2,2 @@\n+a2\n+a3\n");
		expect(result[0]!.anchor).toEqual({
			oldStart: 1,
			oldLines: 0,
			newStart: 1,
			newLines: 3,
		});
	});

	test("ignores headers that don't fit within the natural hunk", () => {
		const result = splitDiffHunkByHeaders(natural, [
			{ oldStart: 999, oldLines: 1, newStart: 999, newLines: 1 },
		]);
		expect(result).toEqual([{ hunk: natural }]);
	});

	test("preserves '\\ No newline at end of file' markers with their row", () => {
		const noNewline = {
			oldStart: 1,
			oldLines: 2,
			newStart: 1,
			newLines: 2,
			diff: `@@ -1,2 +1,2 @@
-r1
+a1
 c1
\\ No newline at end of file
`,
		};
		const result = splitDiffHunkByHeaders(noNewline, [
			{ oldStart: 1, oldLines: 1, newStart: 1, newLines: 1 },
			{ oldStart: 2, oldLines: 1, newStart: 2, newLines: 1 },
		]);
		expect(result).toHaveLength(2);
		expect(result[1]!.hunk.diff).toContain("\\ No newline at end of file");
	});
});

describe("bodyRowRangeFromSelection / countDeltaRowsInRange / countBodyRows", () => {
	const mixed = {
		oldStart: 5,
		oldLines: 3,
		newStart: 5,
		newLines: 4,
		diff: `@@ -5,3 +5,4 @@
 c1
-r1
+a1
+a2
 c2
`,
	};

	test("countBodyRows skips header and trailing newline", () => {
		expect(countBodyRows(mixed)).toBe(5);
	});

	test("selecting all delta rows in newRange returns the contiguous range", () => {
		const r = bodyRowRangeFromSelection(mixed, undefined, { startLine: 5, endLine: 7 });
		expect(r).toEqual({ start: 0, end: 4 });
	});

	test("selecting only the removed line yields a single-row range", () => {
		const r = bodyRowRangeFromSelection(mixed, { startLine: 6, endLine: 6 }, undefined);
		expect(r).toEqual({ start: 1, end: 2 });
	});

	test("selecting only the added lines yields a 2-row range", () => {
		const r = bodyRowRangeFromSelection(mixed, undefined, { startLine: 6, endLine: 7 });
		expect(r).toEqual({ start: 2, end: 4 });
	});

	test("selecting only context returns the context row range", () => {
		const r = bodyRowRangeFromSelection(mixed, undefined, { startLine: 5, endLine: 5 });
		// row 0 is the leading context "c1" (newLine=5)
		expect(r).toEqual({ start: 0, end: 1 });
	});

	test("countDeltaRowsInRange counts only +/- rows", () => {
		// Range covering everything: 1 removed + 2 added = 3
		expect(countDeltaRowsInRange(mixed, { start: 0, end: 5 })).toBe(3);
		// Range covering just the leading context row:
		expect(countDeltaRowsInRange(mixed, { start: 0, end: 1 })).toBe(0);
	});

	test("returns null when no rows match the selection", () => {
		const r = bodyRowRangeFromSelection(
			mixed,
			{ startLine: 100, endLine: 200 },
			{ startLine: 100, endLine: 200 },
		);
		expect(r).toBeNull();
	});
});

describe("expandRangeToAbsorbBlankAddRows", () => {
	const sectioned: DiffHunk = {
		oldStart: 1,
		oldLines: 0,
		newStart: 1,
		newLines: 11,
		diff: [
			"@@ -1,0 +1,11 @@",
			"+## Section A",
			"+- alpha 1",
			"+- alpha 2",
			"+",
			"+",
			"+## Section B",
			"+- beta 1",
			"+- beta 2",
			"+",
			"+## Section C",
			"+- gamma 1",
			"",
		].join("\n"),
	};

	test("absorbs trailing blank Add rows after a user range", () => {
		// User selected Section A (rows 0..2, body indices). Expand
		// should pull the two trailing blank rows in.
		const expanded = expandRangeToAbsorbBlankAddRows(sectioned, { start: 0, end: 3 });
		expect(expanded).toEqual({ start: 0, end: 5 });
	});

	test("absorbs blank Add rows on both sides of a user range", () => {
		// User selected Section B + betas (indices 5..7). Expand should
		// pull the two leading blanks in (rows 3,4) and the trailing
		// blank between B and C (row 8).
		const expanded = expandRangeToAbsorbBlankAddRows(sectioned, { start: 5, end: 8 });
		expect(expanded).toEqual({ start: 3, end: 9 });
	});

	test("does not cross non-blank Add rows", () => {
		// Range that doesn't have adjacent blanks.
		const expanded = expandRangeToAbsorbBlankAddRows(sectioned, { start: 0, end: 1 });
		expect(expanded).toEqual({ start: 0, end: 1 });
	});

	test("never expands past hunk boundaries", () => {
		const expanded = expandRangeToAbsorbBlankAddRows(sectioned, { start: 0, end: 11 });
		expect(expanded).toEqual({ start: 0, end: 11 });
	});

	test("blank-only spans collapse onto themselves (no neighbors)", () => {
		const onlyBlanks: DiffHunk = {
			oldStart: 1,
			oldLines: 0,
			newStart: 1,
			newLines: 2,
			diff: "@@ -1,0 +1,2 @@\n+\n+\n",
		};
		// Selecting one of the two blanks expands to include the other.
		const expanded = expandRangeToAbsorbBlankAddRows(onlyBlanks, { start: 0, end: 1 });
		expect(expanded).toEqual({ start: 0, end: 2 });
	});
});
