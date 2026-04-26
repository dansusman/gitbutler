<script lang="ts">
	import AnnotationBadge from "$components/diff/AnnotationBadge.svelte";
	import AnnotationEditor from "$components/diff/AnnotationEditor.svelte";
	import HiddenDiffNotice from "$components/diff/HiddenDiffNotice.svelte";
	import HunkContextMenu from "$components/diff/HunkContextMenu.svelte";
	import HunkSelectionPopover from "$components/diff/HunkSelectionPopover.svelte";
	import ImageDiff from "$components/diff/ImageDiff.svelte";
	import LineLocksWarning from "$components/diff/LineLocksWarning.svelte";
	import ReduxResult from "$components/shared/ReduxResult.svelte";
	import { ANNOTATION_SERVICE, type AnnotationContext, type DiffLine } from "$lib/annotations/annotationService.svelte";
	import binarySvg from "$lib/assets/empty-state/binary.svg?raw";
	import emptyFileSvg from "$lib/assets/empty-state/empty-file.svg?raw";
	import tooLargeSvg from "$lib/assets/empty-state/too-large.svg?raw";
	import { DEPENDENCY_SERVICE } from "$lib/dependencies/dependencyService.svelte";
	import { DIFF_SERVICE } from "$lib/hunks/diffService.svelte";
	import { draggableChips } from "$lib/dragging/draggable";
	import { HunkDropDataV3 } from "$lib/dragging/draggables";
	import { DROPZONE_REGISTRY } from "$lib/dragging/registry";
	import {
		bodyRowRangeFromSelection,
		canBePartiallySelected,
		countBodyRows,
		countDeltaRowsInRange,
		expandRangeToAbsorbBlankAddRows,
		getLineLocks,
		hunkHeaderEquals,
		splitDiffHunkByHeaders,
		type SplitDiffHunk,
	} from "$lib/hunks/hunk";
	import { IRC_API_SERVICE } from "$lib/irc/ircApiService";
	import { type SelectionId } from "$lib/selection/key";
	import { UNCOMMITTED_SERVICE } from "$lib/selection/uncommittedService.svelte";
	import { SETTINGS_SERVICE } from "$lib/settings/appSettings";
	import { SETTINGS } from "$lib/settings/userSettings";
	import { UI_STATE } from "$lib/state/uiState.svelte";
	import { inject } from "@gitbutler/core/context";
	import { isImageFile } from "@gitbutler/shared/utils/file";
	import { EmptyStatePlaceholder, generateHunkId, HunkDiff, TestId } from "@gitbutler/ui";
	import { DRAG_STATE_SERVICE } from "@gitbutler/ui/drag/dragStateService.svelte";
	import { parseHunk, SectionType } from "@gitbutler/ui/utils/diffParsing";
	import { untrack } from "svelte";
	import type { FileDependencies } from "$lib/hunks/dependencies";
	import type { UnifiedDiff } from "$lib/hunks/diff";
	import type { Reaction } from "$lib/irc/ircEndpoints";
	import type { DiffHunk } from "@gitbutler/but-sdk";
	import type { TreeChange } from "@gitbutler/but-sdk";
	import type { LineId } from "@gitbutler/ui/utils/diffParsing";

	const LARGE_DIFF_THRESHOLD = 1000;
	const INITIAL_HUNKS = 5;
	const HUNKS_PER_FRAME = 10;

	type Props = {
		projectId: string;
		selectable: boolean;
		change: TreeChange;
		diff: UnifiedDiff | null;
		selectionId: SelectionId;
		stackId?: string;
		commitId?: string;
		draggable?: boolean;
		topPadding?: boolean;
		annotationContext?: AnnotationContext;
	};

	const {
		projectId,
		selectable = false,
		change,
		diff,
		selectionId,
		stackId,
		commitId,
		draggable,
		topPadding,
		annotationContext,
	}: Props = $props();

	const resolvedAnnotationContext: AnnotationContext = $derived.by(() => {
		if (annotationContext) return annotationContext;
		if (selectionId.type === 'commit' && 'commitId' in selectionId) {
			return { type: 'commit', commitId: selectionId.commitId };
		}
		if (selectionId.type === 'branch' && 'branchName' in selectionId) {
			return { type: 'branch', branchName: selectionId.branchName };
		}
		return { type: 'worktree' };
	});

	const uiState = inject(UI_STATE);
	const dropzoneRegistry = inject(DROPZONE_REGISTRY);
	const dragStateService = inject(DRAG_STATE_SERVICE);

	let contextMenu = $state<ReturnType<typeof HunkContextMenu>>();
	let showAnyways = $state(false);
	let viewport = $state<HTMLDivElement>();
	const projectState = $derived(uiState.project(projectId));
	const exclusiveAction = $derived(projectState.exclusiveAction.current);

	const isCommitting = $derived(
		exclusiveAction?.type === "commit" && selectionId.type === "worktree",
	);

	const isUncommittedChange = $derived(selectionId.type === "worktree");

	const uncommittedService = inject(UNCOMMITTED_SERVICE);
	const dependencyService = inject(DEPENDENCY_SERVICE);
	const diffService = inject(DIFF_SERVICE);

	function pathToBytes(p: string): number[] {
		return Array.from(new TextEncoder().encode(p));
	}

	/**
	 * Returns true if any pair of sub-hunks of `anchor` is assigned to a
	 * different branch than the others. Drives the un-split confirmation
	 * prompt: dropping the override would lose any reassignment a sub-hunk
	 * received via `assign_hunk` after split.
	 */
	function subHunksHaveDivergentAssignments(anchorHeader: {
		oldStart: number;
		oldLines: number;
		newStart: number;
		newLines: number;
	}): boolean {
		const contained = assignments.current.filter((a) => {
			if (!a.hunkHeader) return false;
			const h = a.hunkHeader;
			return (
				h.oldStart >= anchorHeader.oldStart &&
				h.oldStart + h.oldLines <= anchorHeader.oldStart + anchorHeader.oldLines &&
				h.newStart >= anchorHeader.newStart &&
				h.newStart + h.newLines <= anchorHeader.newStart + anchorHeader.newLines
			);
		});
		if (contained.length < 2) return false;
		const keys = new Set(contained.map((a) => a.stackId ?? a.branchRefBytes ?? ""));
		return keys.size > 1;
	}

	async function handleUnsplit(anchor: {
		oldStart: number;
		oldLines: number;
		newStart: number;
		newLines: number;
	}) {
		if (subHunksHaveDivergentAssignments(anchor)) {
			const confirmed = window.confirm(
				"Un-split will discard the per-line stack reassignments for this hunk. Continue?",
			);
			if (!confirmed) return;
		}
		await diffService.unsplitHunk({
			projectId,
			path: pathToBytes(change.path),
			anchor,
		});
	}

	const fileDependenciesQuery = $derived(
		selectionId.type === "worktree"
			? dependencyService.fileDependencies(projectId, change.path, stackId)
			: undefined,
	);

	const userSettings = inject(SETTINGS);
	const annotationService = inject(ANNOTATION_SERVICE);

	interface AnnotationEdit {
		filePath: string;
		oldRange: { startLine: number; endLine: number } | undefined;
		newRange: { startLine: number; endLine: number } | undefined;
		diffLines: DiffLine[];
	}

	let editingAnnotation = $state<AnnotationEdit | null>(null);

	// Phase 5 popover state. A single popover at a time across all
	// rendered hunks for this file.
	type PopoverState = {
		target: { clientX: number; clientY: number };
		// The (sub-)hunk under the gesture — used for staging and as
		// the rendering reference.
		hunk: DiffHunk;
		isSubHunk: boolean;
		// When `isSubHunk`, the natural hunk that contains `hunk` (i.e.
		// the override's anchor). Carries the full body text so Split
		// can re-issue `split_hunk` against the natural anchor and
		// refine an existing override rather than producing a
		// not-found error.
		naturalHunk: DiffHunk;
		oldRange: { startLine: number; endLine: number } | undefined;
		newRange: { startLine: number; endLine: number } | undefined;
		diffLines: DiffLine[];
		linesInSelection: LineId[];
		anyUnstaged: boolean;
	};
	let selectionPopover = $state<PopoverState | null>(null);

	/**
	 * Phase 5 (line-by-line commits): apply the popover's Stage / Unstage
	 * action to every delta line in the selection. Direction is determined
	 * by `anyUnstaged` captured at popover open: if any line in the range
	 * is currently not staged, the action stages all of them; otherwise it
	 * unstages all of them.
	 */
	function applyStageToSelection(p: PopoverState) {
		if (p.linesInSelection.length === 0) return;
		// Items pushed into a `$state`-backed object become Svelte 5
		// proxies. Passing a proxy through Redux/Immer trips
		// `state_descriptors_fixed` because the proxy's property
		// descriptors aren't plain `value`-bearing ones. Snapshot to
		// plain objects before dispatching.
		const lines: LineId[] = p.linesInSelection.map((l) => ({
			newLine: l.newLine,
			oldLine: l.oldLine,
		}));
		// Whole-file (binary / addition / deletion) changes can't be
		// partially staged — click stages the whole hunk, like the
		// pre-popover behavior.
		if (diff?.type === "Patch" && !canBePartiallySelected(diff.subject)) {
			if (p.anyUnstaged) {
				uncommittedService.checkHunk(stackId || null, change.path, p.hunk);
			} else {
				uncommittedService.uncheckHunk(stackId || null, change.path, p.hunk);
			}
			return;
		}
		if (p.anyUnstaged) {
			for (const l of lines) {
				uncommittedService.checkLine(stackId || null, change.path, p.hunk, l);
			}
		} else {
			const allLines: LineId[] = [];
			const parsed = parseHunk(p.hunk.diff);
			for (const section of parsed.contentSections) {
				for (const line of section.lines) {
					if (section.sectionType === SectionType.Context) continue;
					allLines.push({
						newLine: line.afterLineNumber,
						oldLine: line.beforeLineNumber,
					});
				}
			}
			for (const l of lines) {
				uncommittedService.uncheckLine(stackId || null, change.path, p.hunk, l, allLines);
			}
		}
	}

	/**
	 * Phase 5 (line-by-line commits): translate the popover's selection to
	 * a `RowRange` and call the backend `split_hunk` RPC. Always works
	 * against the *natural* anchor hunk so re-splitting an existing
	 * sub-hunk refines the partition (the backend merges new ranges into
	 * the existing override) instead of erroring out on anchor mismatch.
	 *
	 * Validation (mirrors backend):
	 *   1. Reject context-only selections (no `+`/`-` rows).
	 *   2. Reject selections that cover the entire natural hunk.
	 */
	async function applySplitToSelection(p: PopoverState) {
		const rawRange = bodyRowRangeFromSelection(p.naturalHunk, p.oldRange, p.newRange);
		if (!rawRange) return;
		// Phase 5 polish: absorb adjacent blank `+` rows into the user's
		// split boundary so we don't leave 1-row blank-only sub-hunks
		// at section seams (e.g. "## Section A\n...\n\n\n## Section B").
		const range = expandRangeToAbsorbBlankAddRows(p.naturalHunk, rawRange);
		const total = countBodyRows(p.naturalHunk);
		if (range.start === 0 && range.end >= total) return;
		if (countDeltaRowsInRange(p.naturalHunk, range) === 0) return;
		const anchor = {
			oldStart: p.naturalHunk.oldStart,
			oldLines: p.naturalHunk.oldLines,
			newStart: p.naturalHunk.newStart,
			newLines: p.naturalHunk.newLines,
		};
		await diffService.splitHunk({
			projectId,
			path: pathToBytes(change.path),
			anchor,
			ranges: [{ start: range.start, end: range.end }],
		});
	}

	function popoverSplitDisabled(p: PopoverState): { disabled: boolean; reason: string | undefined } {
		const range = bodyRowRangeFromSelection(p.naturalHunk, p.oldRange, p.newRange);
		if (!range) return { disabled: true, reason: "Selection is empty." };
		if (countDeltaRowsInRange(p.naturalHunk, range) === 0)
			return { disabled: true, reason: "Selection contains only context lines." };
		const total = countBodyRows(p.naturalHunk);
		if (range.start === 0 && range.end >= total)
			return { disabled: true, reason: "Selection covers the entire hunk." };
		if (!isUncommittedChange) {
			return {
				disabled: true,
				reason: "Splitting committed work is not yet supported (Phase 7).",
			};
		}
		return { disabled: false, reason: undefined };
	}

	function stripHtml(html: string): string {
		return html.replace(/<[^>]*>/g, '')
			.replace(/&lt;/g, '<')
			.replace(/&gt;/g, '>')
			.replace(/&amp;/g, '&')
			.replace(/&quot;/g, '"')
			.replace(/&#39;/g, "'");
	}

	function handleAnnotateDrag(
		filePath: string,
		oldRange: { startLine: number; endLine: number } | undefined,
		newRange: { startLine: number; endLine: number } | undefined,
		diffLines: DiffLine[],
	) {
		editingAnnotation = { filePath, oldRange, newRange, diffLines };
	}

	const fileAnnotations = $derived(annotationService.getForFile(resolvedAnnotationContext, change.path));

	function rangeOverlapsHunk(range: { startLine: number; endLine: number } | undefined, hunkStart: number, hunkLines: number): boolean {
		if (!range) return false;
		return range.startLine < hunkStart + hunkLines && range.endLine >= hunkStart;
	}

	function annotationsForHunk(hunk: DiffHunk): typeof fileAnnotations {
		return fileAnnotations.filter((a) =>
			rangeOverlapsHunk(a.oldRange, hunk.oldStart, hunk.oldLines) ||
			rangeOverlapsHunk(a.newRange, hunk.newStart, hunk.newLines)
		);
	}

	function isEditorForHunk(hunk: DiffHunk): boolean {
		if (!editingAnnotation || editingAnnotation.filePath !== change.path) return false;
		return rangeOverlapsHunk(editingAnnotation.oldRange, hunk.oldStart, hunk.oldLines) ||
			rangeOverlapsHunk(editingAnnotation.newRange, hunk.newStart, hunk.newLines);
	}



	const assignments = $derived(uncommittedService.assignmentsByPath(stackId || null, change.path));

	const ircApiService = inject(IRC_API_SERVICE);
	const settingsService = inject(SETTINGS_SERVICE);
	const settingsStore = settingsService.appSettings;
	const ircEnabled = $derived(
		($settingsStore?.featureFlags?.irc && $settingsStore?.irc?.connection?.enabled) ?? false,
	);
	const fileReactionsQuery = $derived(
		ircEnabled ? ircApiService.fileMessageReactions({ filePath: change.path }) : undefined,
	);
	const fileReactions = $derived(fileReactionsQuery?.response ?? {});

	function hunkKey(hunk: DiffHunk): string {
		return `${hunk.oldStart}:${hunk.oldLines}:${hunk.newStart}:${hunk.newLines}`;
	}

	function groupReactions(
		reactions: Reaction[],
	): { emoji: string; count: number; senders: string[] }[] {
		const map = new Map<string, string[]>();
		for (const r of reactions) {
			const senders = map.get(r.reaction) ?? [];
			senders.push(r.sender);
			map.set(r.reaction, senders);
		}
		return Array.from(map.entries()).map(([emoji, senders]) => ({
			emoji,
			count: senders.length,
			senders,
		}));
	}

	function filter(hunks: DiffHunk[]): SplitDiffHunk[] {
		if (selectionId.type !== "worktree") return hunks.map((hunk) => ({ hunk }));
		// For each natural hunk, look at the assignments for this file and:
		//   - if any whole-file (binary / too-large) assignment exists, keep the
		//     natural hunk verbatim,
		//   - else partition its rows by the assignment hunk-headers that fit
		//     within it (the materialized output of a `split_hunk` call) and
		//     emit one SplitDiffHunk per sub-range,
		//   - else (no matching assignment) drop it.
		const result: SplitDiffHunk[] = [];
		for (const hunk of hunks) {
			if (assignments.current.some((a) => a?.hunkHeader === null)) {
				result.push({ hunk });
				continue;
			}
			const headers = assignments.current
				.map((a) => a.hunkHeader)
				.filter((h): h is NonNullable<typeof h> => h !== null && h !== undefined);
			const split = splitDiffHunkByHeaders(hunk, headers);
			// `splitDiffHunkByHeaders` returns `[{ hunk }]` (no anchor) when no
			// headers fit. Drop the hunk if it has no matching assignment at all.
			if (
				split.length === 1 &&
				split[0]!.anchor === undefined &&
				!headers.some((h) => hunkHeaderEquals(hunk, h))
			) {
				continue;
			}
			result.push(...split);
		}
		return result;
	}

	const filteredHunks = $derived(
		diff?.type === "Patch" ? filter(diff.subject.hunks) : ([] as SplitDiffHunk[]),
	);
	let renderedHunkCount = $state(INITIAL_HUNKS);

	$effect(() => {
		// Reset and stream hunks progressively whenever file/diff/showAnyways changes.
		// Avoids blocking the main thread by mounting all hunk components at once.
		void change.path;
		void diff;
		void showAnyways;

		const total = untrack(() => filteredHunks.length);
		renderedHunkCount = INITIAL_HUNKS;

		if (total <= INITIAL_HUNKS) return;

		let rafId: number;
		function addMore() {
			renderedHunkCount = Math.min(renderedHunkCount + HUNKS_PER_FRAME, total);
			if (renderedHunkCount < total) {
				rafId = requestAnimationFrame(addMore);
			}
		}
		rafId = requestAnimationFrame(addMore);
		return () => cancelAnimationFrame(rafId);
	});

	function linesInclude(
		newStart: number | undefined,
		oldStart: number | undefined,
		selected: boolean,
		lines: LineId[],
	) {
		if (!selected) return false;
		return (
			lines.length === 0 || lines.some((l) => l.newLine === newStart && l.oldLine === oldStart)
		);
	}

	function selectAllHunkLines(hunk: DiffHunk) {
		uncommittedService.checkHunk(stackId || null, change.path, hunk);
	}

	function unselectAllHunkLines(hunk: DiffHunk) {
		uncommittedService.uncheckHunk(stackId || null, change.path, hunk);
	}

	function invertHunkSelection(hunk: DiffHunk) {
		// Parse the hunk to get all selectable lines
		const parsedHunk = parseHunk(hunk.diff);
		const allSelectableLines = parsedHunk.contentSections
			.flatMap((section) => section.lines)
			.filter((line) => line.beforeLineNumber !== undefined || line.afterLineNumber !== undefined)
			.map((line) => ({
				newLine: line.afterLineNumber,
				oldLine: line.beforeLineNumber,
			}));

		const selection = uncommittedService.hunkCheckStatus(stackId, change.path, hunk);
		const currentSelectedLines = selection.current.lines;
		const isSelected = selection.current.selected;

		// If nothing is selected (hunk not checked)
		if (!isSelected) {
			selectAllHunkLines(hunk);
		}
		// If all lines are selected (empty lines array indicates full selection)
		else if (isSelected && currentSelectedLines.length === 0) {
			unselectAllHunkLines(hunk);
		} else {
			const unselectedLines = allSelectableLines.filter(
				(line) =>
					!currentSelectedLines.some(
						(selectedLine) =>
							selectedLine.newLine === line.newLine && selectedLine.oldLine === line.oldLine,
					),
			);

			// First unselect all lines
			unselectAllHunkLines(hunk);

			// Then select the previously unselected lines
			unselectedLines.forEach((line) => {
				uncommittedService.checkLine(stackId || null, change.path, hunk, line);
			});
		}
	}
</script>

{#if fileDependenciesQuery}
	<ReduxResult {projectId} result={fileDependenciesQuery.result} children={unifiedDiff} />
{:else}
	{@render unifiedDiff(undefined)}
{/if}

{#snippet unifiedDiff(fileDependencies: FileDependencies | undefined)}
	<div
		data-testid={TestId.UnifiedDiffView}
		class="diff-section"
		class:top-padding={topPadding}
		bind:this={viewport}
		>
		{#if $userSettings.svgAsImage && change.path.toLowerCase().endsWith(".svg")}
			<ImageDiff {projectId} {change} {commitId} />
		{:else if diff === null}
			<div class="hunk-placehoder">
				<EmptyStatePlaceholder image={binarySvg} gap={12} topBottomPadding={34}>
					{#snippet caption()}
						Was not able to load the diff
					{/snippet}
				</EmptyStatePlaceholder>
			</div>
		{:else if diff.type === "Patch"}
			{@const linesModified = diff.subject.linesAdded + diff.subject.linesRemoved}
			{#if linesModified > LARGE_DIFF_THRESHOLD && !showAnyways}
				<HiddenDiffNotice
					handleShow={() => {
						showAnyways = true;
					}}
				/>
			{:else}
				{#each filteredHunks.slice(0, renderedHunkCount) as { hunk, anchor: subAnchor }, hunkIndex}
					{@const selection = uncommittedService.hunkCheckStatus(stackId, change.path, hunk)}
					{@const [_, lineLocks] = getLineLocks(hunk, fileDependencies?.dependencies ?? [])}
					{@const hunkId = generateHunkId(change.path, hunkIndex)}
					{@const reactions = fileReactions[hunkKey(hunk)] ?? []}
					{@const isSubHunk = subAnchor !== undefined}
					<div
						class="hunk-content"
						use:draggableChips={{
							label: hunk.diff.split("\n")[0],
							data: new HunkDropDataV3(
								change,
								hunk,
								isUncommittedChange,
								stackId || null,
								commitId,
								selectionId,
							),
							disabled: !draggable,
							chipType: "hunk",
							dropzoneRegistry,
							dragStateService,
						}}
					>
						<HunkDiff
							id={hunkId}
							draggingDisabled={!draggable}
							hideCheckboxes={!isCommitting}
							{isSubHunk}
							onUnsplit={subAnchor ? () => handleUnsplit(subAnchor) : undefined}
							filePath={change.path}
							hunkStr={hunk.diff}
							staged={selection.current.selected}
							stagedLines={selection.current.lines}
							{lineLocks}
							diffLigatures={$userSettings.diffLigatures}
							tabSize={$userSettings.tabSize}
							wrapText={$userSettings.wrapText}
							diffFont={$userSettings.diffFont}
							strongContrast={$userSettings.strongContrast}
							colorBlindFriendly={$userSettings.colorBlindFriendly}
							inlineUnifiedDiffs={$userSettings.inlineUnifiedDiffs}
							selectable={isUncommittedChange}
							onLineClick={undefined}
							onChangeStage={(selected) => {
								if (!selectable) return;
								if (selected) {
									uncommittedService.checkHunk(stackId || null, change.path, hunk);
								} else {
									uncommittedService.uncheckHunk(stackId || null, change.path, hunk);
								}
							}}
							onLineDragEnd={!isCommitting ? (params) => {
								const diffLines: DiffLine[] = [];
								const linesInSelection: LineId[] = [];
								if (params.rows) {
									const selected = params.rows.slice(params.startIdx, params.endIdx + 1);
									for (const row of selected) {
										const content = stripHtml(row.tokens.join(''));
										const prefix = row.type === SectionType.AddedLines ? '+'
											: row.type === SectionType.RemovedLines ? '-' : ' ';
										diffLines.push({ prefix, content });
										if (row.isDeltaLine) {
											linesInSelection.push({
												newLine: row.afterLineNumber,
												oldLine: row.beforeLineNumber,
											});
										}
									}
								}
								// Phase 5 (line-by-line commits): drag opens the
								// selection popover instead of going straight to the
								// annotation editor. The popover is the single decision
								// point for what the drag means.
								if (params.clientX === undefined || params.clientY === undefined) {
									// Touch path / no coords — fall back to old behavior.
									handleAnnotateDrag(change.path, params.oldRange, params.newRange, diffLines);
									return;
								}
								const sel = uncommittedService.hunkCheckStatus(stackId, change.path, hunk).current;
								const anyUnstaged = !sel.selected || (
									sel.lines.length > 0 && linesInSelection.some(
										(l) => !sel.lines.some(
											(s) => s.newLine === l.newLine && s.oldLine === l.oldLine,
										),
									)
								);
								// Resolve the natural anchor hunk so re-splitting an
								// already-split sub-hunk can target the underlying
								// `(path, naturalAnchor)` override.
								//
								// `diff.subject.hunks` is an RTK-frozen object; copying
								// the relevant fields into a plain literal avoids
								// `state_descriptors_fixed` errors when storing into
								// `$state`.
								const rawNatural = isSubHunk && subAnchor && diff.type === "Patch"
									? diff.subject.hunks.find((h) => hunkHeaderEquals(h, subAnchor!)) ?? hunk
									: hunk;
								const naturalHunk: DiffHunk = {
									oldStart: rawNatural.oldStart,
									oldLines: rawNatural.oldLines,
									newStart: rawNatural.newStart,
									newLines: rawNatural.newLines,
									diff: rawNatural.diff,
								};
								selectionPopover = {
									target: { clientX: params.clientX, clientY: params.clientY },
									hunk: {
										oldStart: hunk.oldStart,
										oldLines: hunk.oldLines,
										newStart: hunk.newStart,
										newLines: hunk.newLines,
										diff: hunk.diff,
									},
									isSubHunk,
									naturalHunk,
									oldRange: params.oldRange ? { ...params.oldRange } : undefined,
									newRange: params.newRange ? { ...params.newRange } : undefined,
									diffLines,
									linesInSelection,
									anyUnstaged,
								};
							} : undefined}
							annotHighlightRange={editingAnnotation && editingAnnotation.filePath === change.path ? { oldRange: editingAnnotation.oldRange, newRange: editingAnnotation.newRange } : undefined}
							handleLineContextMenu={(params) => {
								contextMenu?.open(params.event || params.target, {
									hunk,
									selectedLines: selection.current.lines,
									beforeLineNumber: params.beforeLineNumber,
									afterLineNumber: params.afterLineNumber,
								});
							}}
						>
							{#snippet lockWarning(locks)}
								<LineLocksWarning {projectId} {locks} />
							{/snippet}
						</HunkDiff>
						{#if reactions.length > 0}
							<div class="hunk-reactions">
								{#each groupReactions(reactions) as group}
									<span class="hunk-reaction-pill" title={group.senders.join(", ")}>
										{group.emoji}
										{#if group.count > 1}
											{group.count}
										{/if}
									</span>
								{/each}
							</div>
						{/if}
						{#each annotationsForHunk(hunk) as annotation (`${annotation.oldRange?.startLine}:${annotation.oldRange?.endLine}:${annotation.newRange?.startLine}:${annotation.newRange?.endLine}`)}
							<AnnotationBadge
								{annotation}
								onEditClick={() => { editingAnnotation = { filePath: change.path, oldRange: annotation.oldRange, newRange: annotation.newRange, diffLines: annotation.diffLines }; }}
							/>
						{/each}
						{#if isEditorForHunk(hunk)}
							<AnnotationEditor
								filePath={change.path}
								oldRange={editingAnnotation?.oldRange}
								newRange={editingAnnotation?.newRange}
								diffLines={editingAnnotation?.diffLines ?? []}
								annotationContext={resolvedAnnotationContext}
								onclose={() => { editingAnnotation = null; }}
							/>
						{/if}
					</div>
				{:else}
					{#if diff.subject.hunks.length === 0}
						<div class="hunk-placehoder">
							<EmptyStatePlaceholder image={emptyFileSvg} gap={12} topBottomPadding={34}>
								{#snippet caption()}
									It’s empty ¯\_(ツ゚)_/¯
								{/snippet}
							</EmptyStatePlaceholder>
						</div>
					{:else}
						<div class="hunk-placehoder">
							<EmptyStatePlaceholder gap={12} topBottomPadding={34}>
								{#snippet caption()}
									Loading diff…
								{/snippet}
							</EmptyStatePlaceholder>
						</div>
					{/if}
				{/each}
			{/if}
		{:else if diff.type === "TooLarge"}
			<div class="hunk-placehoder">
				<EmptyStatePlaceholder image={tooLargeSvg} gap={12} topBottomPadding={34}>
					{#snippet caption()}
						Too large to display
					{/snippet}
				</EmptyStatePlaceholder>
			</div>
		{:else if diff.type === "Binary"}
			{#if isImageFile(change.path)}
				<ImageDiff {projectId} {change} {commitId} />
			{:else}
				<div class="hunk-placehoder">
					<EmptyStatePlaceholder image={binarySvg} gap={12} topBottomPadding={34}>
						{#snippet caption()}
							Binary! Not for human eyes
						{/snippet}
					</EmptyStatePlaceholder>
				</div>
			{/if}
		{/if}
<!-- The context menu should be outside the each block. -->
		<HunkContextMenu
			bind:this={contextMenu}
			trigger={viewport}
			{projectId}
			{change}
			discardable={isUncommittedChange}
			{selectable}
			{selectAllHunkLines}
			{unselectAllHunkLines}
			{invertHunkSelection}
			onAnnotateLine={handleAnnotateDrag}
		/>
		{#if selectionPopover}
			{@const p = selectionPopover}
			{@const splitInfo = popoverSplitDisabled(p)}
			<HunkSelectionPopover
				clientX={p.target.clientX}
				clientY={p.target.clientY}
				stageLabel={p.anyUnstaged ? "Stage" : "Unstage"}
				splitDisabled={splitInfo.disabled}
				splitDisabledReason={splitInfo.reason}
				onstage={() => applyStageToSelection(p)}
				oncomment={() => handleAnnotateDrag(change.path, p.oldRange, p.newRange, p.diffLines)}
				onsplit={() => applySplitToSelection(p)}
				onclose={() => { selectionPopover = null; }}
			/>
		{/if}
	</div>
{/snippet}

<style lang="postcss">
	.diff-section {
		display: flex;
		flex-direction: column;
		align-self: stretch;
		max-width: 100%;
		padding: 0 14px 14px 14px;
		overflow-x: hidden;
		gap: 14px;
		&.top-padding {
			padding-top: 14px;
		}
	}
	.hunk-placehoder {
		border: 1px solid var(--border-3);
		border-radius: var(--radius-m);
	}

	.hunk-content {
		user-select: text;
	}

	.hunk-reactions {
		display: flex;
		align-items: center;
		padding: 4px 0 0;
		gap: 4px;
	}
	.hunk-reaction-pill {
		display: inline-flex;
		align-items: center;
		padding: 2px 6px;
		gap: 4px;
		border: 1px solid transparent;
		border-radius: 10px;
		background-color: var(--bg-2);
		font-size: 12px;
	}
</style>
