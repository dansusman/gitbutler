<script lang="ts" module>
	import type { DiffHunk } from "@gitbutler/but-sdk";
	export interface HunkContextItem {
		hunk: DiffHunk;
		selectedLines: LineId[] | undefined;
		beforeLineNumber: number | undefined;
		afterLineNumber: number | undefined;
	}

	export function isHunkContextItem(item: unknown): item is HunkContextItem {
		return typeof item === "object" && item !== null && "hunk" in item && isDiffHunk(item.hunk);
	}
</script>

<script lang="ts">
	import IrcSendToSubmenus from "$components/diff/IrcSendToSubmenus.svelte";
	import { ANNOTATION_SERVICE } from "$lib/annotations/annotationService.svelte";
	import { BACKEND } from "$lib/backend";
	import { getEditorUri, URL_SERVICE } from "$lib/backend/url";
	import { isDiffHunk, lineIdsToHunkHeaders } from "$lib/hunks/hunk";
	import { IRC_API_SERVICE } from "$lib/irc/ircApiService";
	import { vscodePath } from "$lib/project/project";
	import { PROJECTS_SERVICE } from "$lib/project/projectsService";
	import { SETTINGS } from "$lib/settings/userSettings";
	import { STACK_SERVICE } from "$lib/stacks/stackService.svelte";
	import { inject } from "@gitbutler/core/context";
	import { ContextMenu, ContextMenuItem, ContextMenuSection, TestId } from "@gitbutler/ui";
	import type { TreeChange } from "@gitbutler/but-sdk";
	import type { LineId } from "@gitbutler/ui/utils/diffParsing";

	interface Props {
		trigger: HTMLElement | undefined;
		projectId: string;
		change: TreeChange;
		discardable: boolean;
		selectable: boolean;
		selectAllHunkLines: (hunk: DiffHunk) => void;
		unselectAllHunkLines: (hunk: DiffHunk) => void;
		invertHunkSelection: (hunk: DiffHunk) => void;
		onAnnotateLine?: (filePath: string, oldRange: { startLine: number; endLine: number } | undefined, newRange: { startLine: number; endLine: number } | undefined, diffLines: import('$lib/annotations/annotationService.svelte').DiffLine[]) => void;
	}

	const {
		trigger,
		projectId,
		change,
		discardable,
		selectable,
		selectAllHunkLines,
		unselectAllHunkLines,
		invertHunkSelection,
		onAnnotateLine,
	}: Props = $props();

	const stackService = inject(STACK_SERVICE);
	const ircApiService = inject(IRC_API_SERVICE);
	const projectService = inject(PROJECTS_SERVICE);
	const annotationService = inject(ANNOTATION_SERVICE);
	const backend = inject(BACKEND);
	const urlService = inject(URL_SERVICE);

	const userSettings = inject(SETTINGS);

	const filePath = $derived(change.path);
	let contextMenu: ReturnType<typeof ContextMenu> | undefined;

	function getDiscardLineLabel(item: HunkContextItem) {
		const { selectedLines } = item;

		if (selectedLines !== undefined && selectedLines.length > 0)
			return `Discard ${selectedLines.length} selected lines`;

		return "";
	}

	async function discardHunk(item: HunkContextItem) {
		const previousPathBytes =
			change.status.type === "Rename" ? change.status.subject.previousPathBytes : null;

		unselectAllHunkLines(item.hunk);

		const isWholeFileChange =
			change.status.type === "Addition" || change.status.type === "Deletion";

		await stackService.discardChanges({
			projectId,
			worktreeChanges: [
				{
					previousPathBytes,
					pathBytes: change.pathBytes,
					hunkHeaders: isWholeFileChange ? [] : [item.hunk],
				},
			],
		});
	}

	async function discardHunkLines(item: HunkContextItem) {
		if (item.selectedLines === undefined || item.selectedLines.length === 0) return;
		const previousPathBytes =
			change.status.type === "Rename" ? change.status.subject.previousPathBytes : null;

		unselectAllHunkLines(item.hunk);

		await stackService.discardChanges({
			projectId,
			worktreeChanges: [
				{
					previousPathBytes,
					pathBytes: change.pathBytes,
					hunkHeaders: lineIdsToHunkHeaders(item.selectedLines, item.hunk.diff, "discard"),
				},
			],
		});
	}

	export function open(e: MouseEvent | HTMLElement | undefined, item: HunkContextItem) {
		contextMenu?.open(e, item);
	}

	export function close() {
		contextMenu?.close();
	}
</script>

<ContextMenu
	testId={TestId.HunkContextMenu}
	bind:this={contextMenu}
	rightClickTrigger={trigger}
	align="start"
	side="bottom"
>
	{#snippet children(item)}
		{#if isHunkContextItem(item)}
			{#if discardable}
				<ContextMenuSection>
					<ContextMenuItem
						testId={TestId.HunkContextMenu_DiscardChange}
						label="Discard change"
						icon="bin"
						onclick={() => {
							discardHunk(item);
							contextMenu?.close();
						}}
					/>
					{#if item.selectedLines !== undefined && item.selectedLines.length > 0 && change.status.type !== "Addition" && change.status.type !== "Deletion"}
						<ContextMenuItem
							testId={TestId.HunkContextMenu_DiscardLines}
							label={getDiscardLineLabel(item)}
							icon="checklist-remove"
							onclick={() => {
								discardHunkLines(item);
								contextMenu?.close();
							}}
						/>
					{/if}
				</ContextMenuSection>
			{/if}
			<ContextMenuSection>
				<ContextMenuItem
					testId={TestId.HunkContextMenu_OpenInEditor}
					label="Open in {$userSettings.defaultCodeEditor.displayName}"
					icon="open-in-ide"
					onclick={async () => {
						const project = await projectService.fetchProject(projectId);
						if (project?.path) {
							const lineNumber =
								item.beforeLineNumber ?? item.afterLineNumber ?? item.hunk.newStart;
							const path = getEditorUri({
								schemeId: $userSettings.defaultCodeEditor.schemeIdentifer,
								path: [vscodePath(project.path), filePath],
								line: lineNumber,
							});
							urlService.openExternalUrl(path);
						}
						contextMenu?.close();
					}}
				/>
				<ContextMenuItem
					label="Open in Xcode"
					icon="open-in-ide"
					onclick={async () => {
						const project = await projectService.fetchProject(projectId);
						if (project?.path) {
							await backend.invoke("open_in_xcode", { path: project.path, line: null });
						}
						contextMenu?.close();
					}}
				/>
			</ContextMenuSection>

			<ContextMenuSection>
				{@const oldLine = item.beforeLineNumber}
				{@const newLine = item.afterLineNumber}
				<ContextMenuItem
					label="Add comment"
					icon="edit"
					onclick={() => {
						const oldRange = oldLine !== undefined ? { startLine: oldLine, endLine: oldLine } : undefined;
						const newRange = newLine !== undefined ? { startLine: newLine, endLine: newLine } : undefined;
						onAnnotateLine?.(filePath, oldRange, newRange, []);
						contextMenu?.close();
					}}
				/>
			</ContextMenuSection>

			<IrcSendToSubmenus
				{projectId}
				onSend={(target) => {
					const data = JSON.stringify({ change, diff: item.hunk });
					ircApiService.sendMessageWithData({
						target,
						message: change.path,
						data,
					});
				}}
				closeMenu={() => contextMenu?.close()}
			/>

			{#if selectable}
				<ContextMenuSection>
					<ContextMenuItem
						testId={TestId.HunkContextMenu_SelectAll}
						label="Select all"
						icon="select-all"
						onclick={() => {
							selectAllHunkLines(item.hunk);
							contextMenu?.close();
						}}
					/>
					<ContextMenuItem
						testId={TestId.HunkContextMenu_UnselectAll}
						label="Unselect all"
						icon="select-all-remove"
						onclick={() => {
							unselectAllHunkLines(item.hunk);
							contextMenu?.close();
						}}
					/>
					<ContextMenuItem
						testId={TestId.HunkContextMenu_InvertSelection}
						label="Invert selection"
						icon="select-all-inverse"
						onclick={() => {
							invertHunkSelection(item.hunk);
							contextMenu?.close();
						}}
					/>
				</ContextMenuSection>
			{/if}
		{:else}
			<p class="text-12 text-semibold clr-text-2">Malformed item (·•᷄‎ࡇ•᷅ )</p>
		{/if}
	{/snippet}
</ContextMenu>
