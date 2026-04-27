import { memoize } from "@gitbutler/shared/memoization";
import {
	lineIdKey,
	parseHunk,
	SectionType,
	type LineId,
	type LineLock,
} from "@gitbutler/ui/utils/diffParsing";
import type { UnifiedDiff } from "$lib/hunks/diff";
import type { HunkLocks } from "$lib/hunks/dependencies";
import type { DiffHunk, HunkHeader } from "@gitbutler/but-sdk";

export type HunkAssignmentError = {
	description: string;
};

export function shouldRaiseHunkAssignmentError(
	error: HunkAssignmentError | null,
): error is HunkAssignmentError {
	if (!error) return false;
	if (error.description === "DefaultTargetNotFound") return false;
	return true;
}

type DeltaLineGroup = {
	type: DeltaLineType;
	lines: LineId[];
};

function getAnchorLineNumber(lineNumber: number, action: "discard" | "commit"): number {
	switch (action) {
		case "discard":
			return lineNumber;
		case "commit":
			return 0;
	}
}
/**
 * Turn a grouping of lines into a hunk header.
 *
 * This expects the lines to be in order, consecutive and to all be of the same type.
 */
function lineGroupsToHunkHeader(
	lineGroup: DeltaLineGroup,
	parentHunkHeader: HunkHeader,
	action: "discard" | "commit",
): HunkHeader {
	const lineCount = lineGroup.lines.length;
	if (lineCount === 0) {
		throw new Error("Line group has no lines");
	}

	const firstLine = lineGroup.lines[0]!;

	switch (lineGroup.type) {
		case "added": {
			const oldStart = getAnchorLineNumber(parentHunkHeader.oldStart, action);
			const oldLines = getAnchorLineNumber(parentHunkHeader.oldLines, action);
			if (firstLine.newLine === undefined) {
				throw new Error("Line has no new line number");
			}
			const newStart = firstLine.newLine;
			const newLines = lineCount;
			return { oldStart, oldLines, newStart, newLines };
		}
		case "removed": {
			const newStart = getAnchorLineNumber(parentHunkHeader.newStart, action);
			const newLines = getAnchorLineNumber(parentHunkHeader.newLines, action);
			if (firstLine.oldLine === undefined) {
				throw new Error("Line has no old line number");
			}
			const oldStart = firstLine.oldLine;
			const oldLines = lineCount;
			return { oldStart, oldLines, newStart, newLines };
		}
	}
}

type DeltaLineType = "added" | "removed";

function lineType(line: LineId): DeltaLineType | undefined {
	if (line.oldLine === undefined && line.newLine === undefined) {
		throw new Error("Line has no line numbers");
	}
	if (line.oldLine === undefined) {
		return "added";
	}
	if (line.newLine === undefined) {
		return "removed";
	}
	return undefined;
}

const memoizedParseHunk = memoize(parseHunk);

/**
 * Group the selected lines of a diff for the backend.
 *
 * This groups them:
 * - In order based on the provided diff
 * - By type (added, removed, context)
 * - By consecutive line numbers
 */
export function extractLineGroups(lineIds: LineId[], diff: string): [DeltaLineGroup[], HunkHeader] {
	const lineGroups: DeltaLineGroup[] = [];
	let currentGroup: DeltaLineGroup | undefined = undefined;
	const lineKeys = new Set(lineIds.map((lineId) => lineIdKey(lineId)));
	const parsedHunk = memoizedParseHunk(diff);

	for (const section of parsedHunk.contentSections) {
		for (const line of section.lines) {
			const lineId = {
				oldLine: line.beforeLineNumber,
				newLine: line.afterLineNumber,
			};
			const deltaType = lineType(lineId);
			const key = lineIdKey(lineId);

			if (!lineKeys.has(key) || deltaType === undefined) {
				// start a new group
				if (currentGroup !== undefined) {
					currentGroup = undefined;
				}
				continue;
			}

			if (currentGroup === undefined || currentGroup.type !== deltaType) {
				currentGroup = { type: deltaType, lines: [] };
				lineGroups.push(currentGroup);
			}
			currentGroup.lines.push(lineId);
		}
	}
	const parentHunkHeader: HunkHeader = {
		oldStart: parsedHunk.oldStart,
		oldLines: parsedHunk.oldLines,
		newStart: parsedHunk.newStart,
		newLines: parsedHunk.newLines,
	};
	return [lineGroups, parentHunkHeader];
}

export function extractAllGroups(diff: string): [DeltaLineGroup[], HunkHeader] {
	const lineGroups: DeltaLineGroup[] = [];
	let currentGroup: DeltaLineGroup | undefined = undefined;
	const parsedHunk = memoizedParseHunk(diff);

	for (const section of parsedHunk.contentSections) {
		for (const line of section.lines) {
			const lineId = {
				oldLine: line.beforeLineNumber,
				newLine: line.afterLineNumber,
			};
			const deltaType = lineType(lineId);
			if (deltaType === undefined) {
				// start a new group
				if (currentGroup !== undefined) {
					currentGroup = undefined;
				}
				continue;
			}

			if (currentGroup === undefined || currentGroup.type !== deltaType) {
				currentGroup = { type: deltaType, lines: [] };
				lineGroups.push(currentGroup);
			}
			currentGroup.lines.push(lineId);
		}
	}
	const parentHunkHeader: HunkHeader = {
		oldStart: parsedHunk.oldStart,
		oldLines: parsedHunk.oldLines,
		newStart: parsedHunk.newStart,
		newLines: parsedHunk.newLines,
	};
	return [lineGroups, parentHunkHeader];
}

/**
 * Build a list of hunk headers from a list of line IDs.
 *
 * Iterate over the lines of the parsed diff, match them against the given line IDs
 * in order to ensure the correct order of the lines.
 */
export function lineIdsToHunkHeaders(
	lineIds: LineId[],
	diff: string,
	action: "discard" | "commit",
): HunkHeader[] {
	if (lineIds.length === 0) {
		return [];
	}

	const [lineGroups, parentHunkHeader] = extractLineGroups(lineIds, diff);

	return lineGroups.map((lineGroup) => lineGroupsToHunkHeader(lineGroup, parentHunkHeader, action));
}

/**
 * Build a list of hunk headers that cover the entire diff.
 *
 * This is used when the user selects the entire hunk.
 */
export function diffToHunkHeaders(diff: string, action: "discard" | "commit"): HunkHeader[] {
	const [lineGroups, parentHunkHeader] = extractAllGroups(diff);

	return lineGroups.map((lineGroup) => lineGroupsToHunkHeader(lineGroup, parentHunkHeader, action));
}

/**
 * Find the `DiffHunk` in `changeDiff` that corresponds to `hunk`.
 *
 * First tries an exact header match (a natural hunk produced by `git diff`).
 * Falls back to a containment match — the signature of a sub-hunk produced
 * by `split_hunk` on the backend. In the containment case, the returned
 * `DiffHunk` is a synthetic slice of the parent natural hunk carrying just
 * the rows belonging to `hunk`.
 */
export function findHunkDiff(
	changeDiff: UnifiedDiff | null,
	hunk: HunkHeader,
): DiffHunk | undefined {
	if (changeDiff?.type !== "Patch") return undefined;

	const exact = changeDiff.subject.hunks.find((hunkDiff) => hunkHeaderEquals(hunkDiff, hunk));
	if (exact) return exact;

	for (const candidate of changeDiff.subject.hunks) {
		if (
			hunk.oldStart >= candidate.oldStart &&
			hunk.oldStart + hunk.oldLines <= candidate.oldStart + candidate.oldLines &&
			hunk.newStart >= candidate.newStart &&
			hunk.newStart + hunk.newLines <= candidate.newStart + candidate.newLines
		) {
			const split = splitDiffHunkByHeaders(candidate, [hunk]);
			const match = split.find((s) => hunkHeaderEquals(s.hunk, hunk));
			if (match) return match.hunk;
		}
	}
	return undefined;
}

export function isDiffHunk(something: unknown): something is DiffHunk {
	return (
		typeof something === "object" &&
		something !== null &&
		"oldStart" in something &&
		typeof (something as any).oldStart === "number" &&
		"oldLines" in something &&
		typeof (something as any).oldLines === "number" &&
		"newStart" in something &&
		typeof (something as any).newStart === "number" &&
		"newLines" in something &&
		typeof (something as any).newLines === "number" &&
		"diff" in something &&
		typeof (something as any).diff === "string"
	);
}

/**
 * A patch that if applied to the previous state of the resource would yield the current state.
 * Includes all non-overlapping hunks, including their context lines.
 */
export type Patch = {
	/** All non-overlapping hunks, including their context lines. */
	readonly hunks: DiffHunk[];
	/**
	 * If `true`, a binary to text filter (`textconv` in Git config) was used to obtain the `hunks` in the diff.
	 * This means hunk-based operations must be disabled.
	 */
	readonly isResultOfBinaryToTextConversion: boolean;
	/** The number of lines added in the patch. */
	readonly linesAdded: number;
	/** The number of lines removed in the patch. */
	readonly linesRemoved: number;
};

export function isFileDeletionHunk(hunk: DiffHunk): boolean {
	return hunk.newStart === 1 && hunk.newLines === 0;
}

export function canBePartiallySelected(patch: Patch): boolean {
	if (patch.hunks.length === 0) {
		// Should never happen, but just in case
		return false;
	}

	const onlyHunk = patch.hunks[0];
	if (patch.hunks.length === 1 && onlyHunk && isFileDeletionHunk(onlyHunk)) {
		// Only one hunk and it's a file deletion
		return false;
	}

	// TODO: Check if the hunks come from the diff filter
	// See: https://github.com/gitbutlerapp/gitbutler/pull/7893

	return true;
}

export function hunkContainsHunk(a: DiffHunk, b: DiffHunk): boolean {
	return (
		a.oldStart <= b.oldStart &&
		a.oldStart + a.oldLines - 1 >= b.oldStart + b.oldLines - 1 &&
		a.newStart <= b.newStart &&
		a.newStart + a.newLines - 1 >= b.newStart + b.newLines - 1
	);
}

/**
 * Whether `header` lies entirely within `natural` according to its old
 * and/or new line ranges. Pure-add headers (oldLines=0) are matched by
 * their new range; pure-remove headers (newLines=0) by their old range;
 * mixed headers must satisfy both.
 */
function headerWithinHunk(natural: DiffHunk, header: HunkHeader): boolean {
	const oldOk =
		header.oldLines === 0
			? true
			: header.oldStart >= natural.oldStart &&
				header.oldStart + header.oldLines <= natural.oldStart + natural.oldLines;
	const newOk =
		header.newLines === 0
			? true
			: header.newStart >= natural.newStart &&
				header.newStart + header.newLines <= natural.newStart + natural.newLines;
	return oldOk && newOk;
}

/**
 * Render the `@@ ... @@` header line for a `HunkHeader`, matching the
 * format produced by `git diff` (the file-portion is omitted, as in
 * `but-core`'s diff output).
 */
function renderHunkHeaderLine(header: HunkHeader): string {
	return `@@ -${header.oldStart},${header.oldLines} +${header.newStart},${header.newLines} @@`;
}

/**
 * Decide whether a row of `kind` at line numbers `(oldLine, newLine)` is
 * "owned" by `header`. Used to partition rows of a natural hunk among the
 * sub-hunks that materialize from a backend split.
 */
function rowBelongsToHeader(
	kind: " " | "+" | "-",
	oldLine: number,
	newLine: number,
	header: HunkHeader,
): boolean {
	const inOld = oldLine >= header.oldStart && oldLine < header.oldStart + header.oldLines;
	const inNew = newLine >= header.newStart && newLine < header.newStart + header.newLines;
	if (kind === " ") return inOld && inNew;
	if (kind === "-") return inOld;
	return inNew;
}

/**
 * Output of [`splitDiffHunkByHeaders`].
 *
 * `anchor` is set when `hunk` is a sub-hunk — the backend split a single
 * natural hunk into multiple pieces. It points at the natural anchor so the
 * caller can render an "un-split" affordance and call `unsplit_hunk` with
 * the right key. `anchor` is `undefined` for natural hunks that pass
 * through unchanged.
 */
export type SplitDiffHunk = {
	hunk: DiffHunk;
	anchor?: HunkHeader;
	/**
	 * Phase 7e: when the sub-hunk was emitted by a commit-anchored
	 * override (rather than synthesized via `splitDiffHunkByHeaders`),
	 * this is the row range the override partitions out. Drag handlers
	 * use it to call `move_sub_hunk` / `uncommit_sub_hunk`.
	 */
	subRange?: { start: number; end: number };
};

/**
 * If `subHeaders` describes one or more sub-hunks of `natural` (the
 * materialized output of a `split_hunk` operation on the backend),
 * partition `natural`'s body rows among the sub-hunks and return one
 * synthetic `DiffHunk` per sub-hunk. Each synthetic hunk reuses the
 * row content of `natural` verbatim, just regrouped under its own
 * `@@` header. Returned items carry an `anchor` pointing at `natural`
 * so callers can offer an un-split UI affordance.
 *
 * If `subHeaders` is empty or contains a single header equal to
 * `natural`, returns `[{ hunk: natural }]` unchanged (no anchor).
 *
 * Headers that don't fit within `natural` are ignored. Rows that don't
 * belong to any header (which shouldn't happen for valid backend output
 * since residuals are always emitted) are dropped silently.
 */
export function splitDiffHunkByHeaders(
	natural: DiffHunk,
	subHeaders: HunkHeader[],
): SplitDiffHunk[] {
	if (subHeaders.length === 0) return [{ hunk: natural }];
	const sorted = subHeaders
		.filter((h) => headerWithinHunk(natural, h))
		.slice()
		.sort(orderHeaders);
	if (sorted.length === 0) return [{ hunk: natural }];
	if (sorted.length === 1 && hunkHeaderEquals(natural, sorted[0]!))
		return [{ hunk: natural }];

	const lines = natural.diff.split("\n");
	// Trailing newline produces an empty final element after split; preserve it.
	const hasTrailingNewline = lines[lines.length - 1] === "";
	if (hasTrailingNewline) lines.pop();

	const buckets: string[][] = sorted.map(() => []);
	let oldLine = natural.oldStart;
	let newLine = natural.newStart;

	for (let i = 0; i < lines.length; i++) {
		const row = lines[i]!;
		if (i === 0 && row.startsWith("@@")) continue;
		const first = row[0];
		if (first === "\\") {
			// "\ No newline" markers travel with the previous row's bucket.
			for (const bucket of buckets) {
				if (bucket.length > 0) {
					const lastInBucket = bucket[bucket.length - 1]!;
					if (lastInBucket === lines[i - 1]) {
						bucket.push(row);
						break;
					}
				}
			}
			continue;
		}
		const kind: " " | "+" | "-" =
			first === "+" ? "+" : first === "-" ? "-" : " ";
		for (let bi = 0; bi < sorted.length; bi++) {
			if (rowBelongsToHeader(kind, oldLine, newLine, sorted[bi]!)) {
				buckets[bi]!.push(row);
				break;
			}
		}
		if (kind === " " || kind === "-") oldLine++;
		if (kind === " " || kind === "+") newLine++;
	}

	const anchor: HunkHeader = {
		oldStart: natural.oldStart,
		oldLines: natural.oldLines,
		newStart: natural.newStart,
		newLines: natural.newLines,
	};
	return sorted.map((header, bi) => {
		const body = buckets[bi]!;
		const headerLine = renderHunkHeaderLine(header);
		const diff = [headerLine, ...body, ...(hasTrailingNewline ? [""] : [])].join("\n");
		return {
			hunk: {
				oldStart: header.oldStart,
				oldLines: header.oldLines,
				newStart: header.newStart,
				newLines: header.newLines,
				diff,
			},
			anchor,
		};
	});
}

/**
 * Determines whether two hunk headers cover the same positions and ranges.
 *
 * This does not mean that they represent the same diffs or are even for the
 * same file. As such, this should only be used to compare headers within the
 * same file.
 */
export function hunkHeaderEquals(a: HunkHeader, b: HunkHeader): boolean {
	if (a.newLines !== b.newLines) return false;
	if (a.oldLines !== b.oldLines) return false;
	if (a.newStart !== b.newStart) return false;
	if (a.oldStart !== b.oldStart) return false;
	return true;
}

export function hunkContainsLine(hunk: DiffHunk, line: LineId): boolean {
	if (line.oldLine === undefined && line.newLine === undefined) {
		throw new Error("Line has no line numbers");
	}

	if (line.oldLine !== undefined && line.newLine !== undefined) {
		return (
			hunk.oldStart <= line.oldLine &&
			hunk.oldStart + hunk.oldLines - 1 >= line.oldLine &&
			hunk.newStart <= line.newLine &&
			hunk.newStart + hunk.newLines - 1 >= line.newLine
		);
	}

	if (line.oldLine !== undefined) {
		return hunk.oldStart <= line.oldLine && hunk.oldStart + hunk.oldLines - 1 >= line.oldLine;
	}

	if (line.newLine !== undefined) {
		return hunk.newStart <= line.newLine && hunk.newStart + hunk.newLines - 1 >= line.newLine;
	}

	throw new Error("Malformed line ID");
}

/**
 * Get the line locks for a hunk.
 */
export function getLineLocks(
	hunk: DiffHunk,
	locks: HunkLocks[],
): [boolean, LineLock[] | undefined] {
	const lineLocks: LineLock[] = [];
	const parsedHunk = memoizedParseHunk(hunk.diff);

	const locksContained = locks.filter((lock) => hunkContainsHunk(hunk, lock.hunk));

	let hunkIsFullyLocked: boolean = true;

	for (const contentSection of parsedHunk.contentSections) {
		if (contentSection.sectionType === SectionType.Context) continue;

		for (const line of contentSection.lines) {
			const lineId: LineId = {
				oldLine: line.beforeLineNumber,
				newLine: line.afterLineNumber,
			};

			const hunkLocks = locksContained.filter((lock) => hunkContainsLine(lock.hunk, lineId));
			if (hunkLocks.length === 0) {
				hunkIsFullyLocked = false;
				continue;
			}

			lineLocks.push({
				...lineId,
				locks: hunkLocks.map((lock) => lock.locks).flat(),
			});
		}
	}

	return [hunkIsFullyLocked, lineLocks];
}

/**
 * Order hunk headers from the top of a file to the bottom.
 *
 * We expect the headers to have lines selected by having a whole side 0'ed out:
 * ```json
 * {
 *		"oldStart": 0,
 *		"oldLines": 0,
 *		"newStart": 3,
 *		"newLines": 1
 * }
 * ```
 * This is how it'd look to select the added line 3.
 *
 * Sorting them, requires us to compare the non-zeroed sides to each other.
 * This is an example of what a set of sorted headers should look like:
 * ```json
 * {
 *		"oldStart": 0,
 *		"oldLines": 0,
 *		"newStart": 3,
 *		"newLines": 1
 *	},
 *	{
 *		"oldStart": 3,
 *		"oldLines": 1,
 *		"newStart": 0,
 *		"newLines": 0
 *	},
 *	{
 *		"oldStart": 0,
 *		"oldLines": 0,
 *		"newStart": 5,
 *		"newLines": 1
 *	},
 *	{
 *		"oldStart": 5,
 *		"oldLines": 1,
 *		"newStart": 0,
 *		"newLines": 0
 *	}
 * ```
 */
export function orderHeaders(a: HunkHeader, b: HunkHeader): number {
	const startA = a.oldStart || a.newStart;
	const startB = b.oldStart || b.newStart;
	return startA - startB;
}

/**
 * Given a natural hunk and a user-selected line range (in "before" /
 * "after" line-number space, as produced by the drag gesture's
 * `LineDragEndParams`), compute the corresponding body-row range
 * (`{start, end}`, half-open, body-row indices) suitable for the
 * `split_hunk` RPC.
 *
 * Walks the unified-diff body of `hunk` line-by-line, mirroring the
 * row-counter logic in `splitDiffHunkByHeaders`. A row is considered
 * "in selection" when:
 *  - it is a `+` row whose `afterLineNumber` falls within `newRange`, or
 *  - it is a `-` row whose `beforeLineNumber` falls within `oldRange`, or
 *  - it is a context row whose `afterLineNumber` falls within `newRange`
 *    (allowing leading/trailing context to be trimmed by the backend).
 *
 * Returns `null` if no body row matches the selection.
 *
 * Phase 5 (line-by-line commits): the popover's Split action calls this
 * to translate the gesture's line-number ranges into the row-range
 * payload expected by `split_hunk`. Backend trims leading/trailing
 * context implicitly per validation rule 4.
 */
export function bodyRowRangeFromSelection(
	hunk: DiffHunk,
	oldRange: { startLine: number; endLine: number } | undefined,
	newRange: { startLine: number; endLine: number } | undefined,
): { start: number; end: number } | null {
	if (!oldRange && !newRange) return null;
	const lines = hunk.diff.split("\n");
	if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();

	let oldLine = hunk.oldStart;
	let newLine = hunk.newStart;
	let firstMatch: number | null = null;
	let lastMatch: number | null = null;
	let bodyIdx = 0;

	for (let i = 0; i < lines.length; i++) {
		const row = lines[i]!;
		if (i === 0 && row.startsWith("@@")) continue;
		if (row.startsWith("\\")) continue;
		const first = row[0];
		const kind: " " | "+" | "-" =
			first === "+" ? "+" : first === "-" ? "-" : " ";

		const inOld =
			oldRange !== undefined &&
			(kind === "-" || kind === " ") &&
			oldLine >= oldRange.startLine &&
			oldLine <= oldRange.endLine;
		const inNew =
			newRange !== undefined &&
			(kind === "+" || kind === " ") &&
			newLine >= newRange.startLine &&
			newLine <= newRange.endLine;

		if (inOld || inNew) {
			if (firstMatch === null) firstMatch = bodyIdx;
			lastMatch = bodyIdx;
		}

		if (kind === " " || kind === "-") oldLine++;
		if (kind === " " || kind === "+") newLine++;
		bodyIdx++;
	}

	if (firstMatch === null || lastMatch === null) return null;
	return { start: firstMatch, end: lastMatch + 1 };
}

/**
 * Counts non-context (`+` / `-`) rows in `hunk`'s body. Used by the
 * Phase 5 popover to detect "selection consists only of context rows"
 * and disable the Split action accordingly.
 */
export function countDeltaRowsInRange(
	hunk: DiffHunk,
	range: { start: number; end: number },
): number {
	const lines = hunk.diff.split("\n");
	if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();

	let bodyIdx = 0;
	let count = 0;
	for (let i = 0; i < lines.length; i++) {
		const row = lines[i]!;
		if (i === 0 && row.startsWith("@@")) continue;
		if (row.startsWith("\\")) continue;
		const first = row[0];
		if (bodyIdx >= range.start && bodyIdx < range.end) {
			if (first === "+" || first === "-") count++;
		}
		bodyIdx++;
	}
	return count;
}

/**
 * Total body-row count of `hunk` (excluding the `@@` header and any
 * `\ No newline` markers).
 */
export function countBodyRows(hunk: DiffHunk): number {
	const lines = hunk.diff.split("\n");
	if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
	let count = 0;
	for (let i = 0; i < lines.length; i++) {
		const row = lines[i]!;
		if (i === 0 && row.startsWith("@@")) continue;
		if (row.startsWith("\\")) continue;
		count++;
	}
	return count;
}

/**
 * Extend a body-row range outward to absorb adjacent blank `+` (Add)
 * rows. Used by the Phase 5 Split gesture so that a user-drawn split
 * boundary that lands next to a blank line doesn't leave a 1-row
 * blank-only sub-hunk behind.
 *
 * "Blank Add row" = a body line whose first character is `+` and whose
 * remaining content is empty or only whitespace. Removed (`-`) and
 * context (` `) rows do not absorb — they belong to surrounding
 * sub-hunks naturally via residuals/trim_context.
 *
 * Returns a new range `{start, end}` (half-open). If no adjacent blank
 * rows exist, the range is unchanged.
 */
export function expandRangeToAbsorbBlankAddRows(
	hunk: DiffHunk,
	range: { start: number; end: number },
): { start: number; end: number } {
	const lines = hunk.diff.split("\n");
	if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();

	// Build a flat array of body rows (skip header and `\ No newline` markers).
	const bodyRows: string[] = [];
	for (let i = 0; i < lines.length; i++) {
		const row = lines[i]!;
		if (i === 0 && row.startsWith("@@")) continue;
		if (row.startsWith("\\")) continue;
		bodyRows.push(row);
	}

	function isBlankAdd(idx: number): boolean {
		if (idx < 0 || idx >= bodyRows.length) return false;
		const row = bodyRows[idx]!;
		if (row[0] !== "+") return false;
		return row.slice(1).trim() === "";
	}

	let { start, end } = range;
	while (start > 0 && isBlankAdd(start - 1)) start--;
	while (end < bodyRows.length && isBlankAdd(end)) end++;
	return { start, end };
}
