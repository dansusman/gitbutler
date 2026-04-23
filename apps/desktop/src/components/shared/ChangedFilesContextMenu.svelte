<!-- This is a V3 replacement for `FileContextMenu.svelte` -->
<script lang="ts">
	import ReduxResult from "$components/shared/ReduxResult.svelte";
	import AbsorbPlanModal from "$components/stack/AbsorbPlanModal.svelte";
	import DiscardChangesModal from "$components/workspace/DiscardChangesModal.svelte";
	import StashIntoBranchModal from "$components/workspace/StashIntoBranchModal.svelte";
	import { ACTION_SERVICE } from "$lib/actions/actionService.svelte";
	import { AI_SERVICE } from "$lib/ai/service";
	import { ANNOTATION_SERVICE, type AnnotationContext } from "$lib/annotations/annotationService.svelte";
	import { BACKEND } from "$lib/backend";
	import { CLIPBOARD_SERVICE } from "$lib/backend/clipboard";
	import { getEditorUri, URL_SERVICE } from "$lib/backend/url";
	import { changesToDiffSpec } from "$lib/commits/utils";
	import { projectAiExperimentalFeaturesEnabled, projectAiGenEnabled } from "$lib/config/config";
	import { FILE_SERVICE } from "$lib/files/fileService";
	import { isTreeChange } from "$lib/hunks/change";
	import { vscodePath } from "$lib/project/project";
	import { PROJECTS_SERVICE } from "$lib/project/projectsService";
	import { FILE_SELECTION_MANAGER } from "$lib/selection/fileSelectionManager.svelte";
	import { SETTINGS } from "$lib/settings/userSettings";
	import { STACK_SERVICE } from "$lib/stacks/stackService.svelte";
	import { UI_STATE, withStackBusy } from "$lib/state/uiState.svelte";
	import { inject } from "@gitbutler/core/context";
	import {
		ContextMenu,
		ContextMenuItem,
		ContextMenuItemSubmenu,
		ContextMenuSection,
		chipToasts,
		TestId,
	} from "@gitbutler/ui";
	import type { SelectionId } from "$lib/selection/key";
	import type { TreeChange } from "@gitbutler/but-sdk";

	const DEFAULT_MODEL = "gpt-4";

	type Props = {
		projectId: string;
		stackId: string | undefined;
		selectionId: SelectionId;
		trigger?: HTMLElement;
		leftClickTrigger?: HTMLElement;
		editMode?: boolean;
		align?: "start" | "center" | "end";
		onopen?: () => void;
		onclose?: () => void;
	};

	type ChangedFilesItem = {
		changes: TreeChange[];
	};

	function isChangedFilesItem(item: unknown): item is ChangedFilesItem {
		return (
			typeof item === "object" &&
			item !== null &&
			"changes" in item &&
			Array.isArray(item.changes) &&
			item.changes.every(isTreeChange)
		);
	}

	type ChangedFolderItem = ChangedFilesItem & {
		path: string;
	};

	function isChangedFolderItem(item: ChangedFilesItem): item is ChangedFolderItem {
		return "path" in item && typeof item.path === "string";
	}

	const {
		trigger,
		leftClickTrigger,
		selectionId,
		stackId,
		projectId,
		editMode = false,
		align,
		onopen,
		onclose,
	}: Props = $props();
	const stackService = inject(STACK_SERVICE);
	const uiState = inject(UI_STATE);
	const idSelection = inject(FILE_SELECTION_MANAGER);
	const aiService = inject(AI_SERVICE);
	const annotationService = inject(ANNOTATION_SERVICE);

	const annotationCtx: AnnotationContext = $derived.by(() => {
		if (selectionId.type === 'commit' && 'commitId' in selectionId) {
			return { type: 'commit', commitId: selectionId.commitId };
		}
		if (selectionId.type === 'branch' && 'branchName' in selectionId) {
			return { type: 'branch', branchName: selectionId.branchName };
		}
		return { type: 'worktree' };
	});
	const annotationCount = $derived(annotationService.countForContext(annotationCtx));
	const actionService = inject(ACTION_SERVICE);
	const fileService = inject(FILE_SERVICE);
	const urlService = inject(URL_SERVICE);
	const clipboardService = inject(CLIPBOARD_SERVICE);
	const backend = inject(BACKEND);
	const [autoCommit, autoCommitting] = actionService.autoCommit;
	const [branchChanges, branchingChanges] = actionService.branchChanges;
	const [, absorbingChanges] = stackService.absorb;
	const [splitOffChanges] = stackService.splitBranch;
	const [splitBranchIntoDependentBranch] = stackService.splitBranchIntoDependentBranch;

	const projectService = inject(PROJECTS_SERVICE);

	const userSettings = inject(SETTINGS);
	const isUncommitted = $derived(selectionId.type === "worktree");
	const isBranchFiles = $derived(selectionId.type === "branch");
	const selectionBranchName = $derived(
		selectionId.type === "branch" ? selectionId.branchName : undefined,
	);

	// Platform-specific label for "Show in Finder/Explorer/File Manager"
	const showInFolderLabel = (() => {
		switch (backend.platformName) {
			case "macos":
				return "Show in Finder";
			case "windows":
				return "Show in Explorer";
			default:
				return "Show in File Manager";
		}
	})();

	let menuOpen = $state(false);
	let menuTarget = $state<MouseEvent | HTMLElement>();
	let menuItem = $state<ChangedFilesItem>();

	let discardModal: ReturnType<typeof DiscardChangesModal>;
	let stashModal: ReturnType<typeof StashIntoBranchModal>;
	let absorbModal: ReturnType<typeof AbsorbPlanModal>;
	let aiConfigurationValid = $state(false);

	const aiGenEnabled = $derived(projectAiGenEnabled(projectId));
	const experimentalFeaturesEnabled = $derived(projectAiExperimentalFeaturesEnabled(projectId));

	const canUseGBAI = $derived(
		$aiGenEnabled && aiConfigurationValid && $experimentalFeaturesEnabled,
	);

	function isDeleted(item: ChangedFilesItem): boolean {
		if (isChangedFolderItem(item)) {
			return false;
		}
		return item.changes.some((change) => {
			return change.status.type === "Deletion";
		});
	}

	function getItemPath(item: ChangedFilesItem): string | null {
		if (isChangedFolderItem(item)) {
			return item.path;
		}
		if (item.changes.length === 1) {
			return item.changes[0]!.path;
		}
		return null;
	}

	export function open(e: MouseEvent | HTMLElement, newItem: ChangedFilesItem) {
		menuTarget = e;
		menuItem = newItem;
		menuOpen = true;
		aiService.validateGitButlerAPIConfiguration().then((value) => {
			aiConfigurationValid = value;
		});
	}

	export function close() {
		menuOpen = false;
	}

	async function uncommitChanges(stackId: string, commitId: string, changes: TreeChange[]) {
		menuOpen = false;
		await withStackBusy(uiState, projectId, { commitId, stackIds: [stackId] }, async () => {
			const { workspace } = await stackService.uncommitChanges({
				projectId,
				stackId,
				commitId,
				changes: changesToDiffSpec(changes),
				dryRun: false,
			});
			const newCommitId = workspace.replacedCommits[commitId];
			const branchName = uiState.lane(stackId).selection.current?.branchName;
			const selectedFiles = changes.map((change) => ({ ...selectionId, path: change.path }));

			// Unselect the uncommitted files
			idSelection.removeMany(selectedFiles);

			if (newCommitId && branchName) {
				const previewOpen = uiState.lane(stackId).selection.current?.previewOpen ?? false;
				// Update the selection to the new commit
				uiState.lane(stackId).selection.set({ branchName, commitId: newCommitId, previewOpen });
			}
		});
	}

	async function triggerAutoCommit(changes: TreeChange[]) {
		try {
			uiState.global.modal.set({
				type: "auto-commit",
				projectId,
			});
			await autoCommit({
				projectId,
				target: {
					type: "treeChanges",
					subject: { changes, assignedStackId: stackId ?? null },
				},
				useAi: $aiGenEnabled,
			});
		} catch (error) {
			console.error("Auto commit failed:", error);
			uiState.global.modal.set(undefined);
		}
	}

	async function triggerBranchChanges(changes: TreeChange[]) {
		if (!canUseGBAI) {
			chipToasts.error("GitButler AI is not configured or enabled for this project.");
			return;
		}

		try {
			await chipToasts.promise(branchChanges({ projectId, changes, model: DEFAULT_MODEL }), {
				loading: "Creating a branch and committing changes",
				success: "Branching changes succeeded",
				error: "Branching changes failed",
			});
		} catch (error) {
			console.error("Branching changes failed:", error);
		}
	}

	async function split(changes: TreeChange[]) {
		if (!stackId) {
			chipToasts.error("No stack selected to split off changes.");
			return;
		}

		if (selectionId.type !== "branch") {
			chipToasts.error("Please select a branch to split off changes.");
			return;
		}

		const branchName = selectionId.branchName;

		const fileNames = changes.map((change) => change.path);

		try {
			await chipToasts.promise(
				(async () => {
					const newBranchName = await stackService.fetchNewBranchName(projectId);

					if (!newBranchName) {
						throw new Error("Failed to generate a new branch name.");
					}

					await splitOffChanges({
						projectId,
						sourceStackId: stackId,
						sourceBranchName: branchName,
						fileChangesToSplitOff: fileNames,
						newBranchName: newBranchName,
					});
				})(),
				{
					loading: "Splitting off changes",
					success: "Changes split off into a new branch",
					error: "Failed to split off changes",
				},
			);
		} catch (error) {
			console.error("Failed to split off changes:", error);
		}
	}

	async function splitIntoDependentBranch(changes: TreeChange[]) {
		if (!stackId) {
			chipToasts.error("No stack selected to split off changes.");
			return;
		}

		if (selectionId.type !== "branch") {
			chipToasts.error("Please select a branch to split off changes.");
			return;
		}

		const branchName = selectionId.branchName;
		const fileNames = changes.map((change) => change.path);

		try {
			await chipToasts.promise(
				(async () => {
					const newBranchName = await stackService.fetchNewBranchName(projectId);

					if (!newBranchName) {
						throw new Error("Failed to generate a new branch name.");
					}

					await splitBranchIntoDependentBranch({
						projectId,
						sourceStackId: stackId,
						sourceBranchName: branchName,
						fileChangesToSplitOff: fileNames,
						newBranchName: newBranchName,
					});
				})(),
				{
					loading: "Splitting into dependent branch",
					success: "Changes split into a dependent branch",
					error: "Failed to split into dependent branch",
				},
			);
		} catch (error) {
			console.error("Failed to split into dependent branch:", error);
		}
	}
</script>

{#if menuOpen && menuItem}
	{@const item = menuItem}
	{@const deletion = isDeleted(item)}
	{@const itemPath = getItemPath(item)}
	<ContextMenu
		{leftClickTrigger}
		rightClickTrigger={trigger}
		side="bottom"
		{align}
		target={menuTarget}
		{onopen}
		onclose={() => {
			menuOpen = false;
			onclose?.();
		}}
	>
		{#if isChangedFilesItem(item)}
			{#if item.changes.length > 0 && !editMode}
				<ContextMenuSection>
					{@const changes = item.changes}
					{#if isUncommitted}
						<ContextMenuItem
							label="Discard changes…"
							testId={TestId.FileListItemContextMenu_DiscardChanges}
							icon="bin"
							onclick={() => {
								discardModal.show(item);
								menuOpen = false;
							}}
						/>
						<ContextMenuItem
							label="Stash into branch…"
							icon="branch-bottom-up-arrow"
							onclick={() => {
								stashModal.show(item);
								menuOpen = false;
							}}
						/>
						<ContextMenuItem
							label="Absorb changes"
							icon="commit-absorb"
							testId={TestId.FileListItemContextMenu_Absorb}
							onclick={() => {
								absorbModal.show(item.changes);
								menuOpen = false;
							}}
							disabled={absorbingChanges.current.isLoading}
						/>
						<ContextMenuItem
							label="Auto commit"
							icon="commit-ai"
							onclick={() => {
								menuOpen = false;
								triggerAutoCommit(changes);
							}}
							disabled={autoCommitting.current.isLoading}
						/>
					{/if}
					{#if selectionId.type === "commit" && stackId && !editMode}
						{@const commitId = selectionId.commitId}
						<ContextMenuItem
							label="Uncommit changes"
							icon="commit-undo"
							onclick={async () => uncommitChanges(stackId, commitId, changes)}
						/>
					{/if}

					{#if isBranchFiles && stackId && selectionBranchName}
						{@const branchIsConflicted = stackService.isBranchConflicted(
							projectId,
							stackId,
							selectionBranchName,
						)}
						<ReduxResult {projectId} result={branchIsConflicted?.result}>
							{#snippet children(isConflicted)}
								{#if isConflicted === false}
									<ContextMenuItem
										label="Split off changes"
										icon="split"
										onclick={() => {
											menuOpen = false;
											split(changes);
										}}
									/>
									<ContextMenuItem
										label="Split into dependent branch"
										icon="stack-plus"
										onclick={() => {
											menuOpen = false;
											splitIntoDependentBranch(changes);
										}}
									/>
								{/if}
							{/snippet}
						</ReduxResult>
					{/if}
				</ContextMenuSection>
			{/if}

			{#if itemPath}
				<ContextMenuSection>
					<ContextMenuItemSubmenu label="Copy path" icon="copy">
						{#snippet submenu(_sub)}
							<ContextMenuSection>
								<ContextMenuItem
									label="Copy path"
									onclick={async () => {
										menuOpen = false;
										const project = await projectService.fetchProject(projectId);
										const projectPath = project?.path;
										if (projectPath) {
											const absPath = await backend.joinPath(projectPath, itemPath);

											await clipboardService.write(absPath, {
												message: "Absolute path copied",
												errorMessage: "Failed to copy absolute path",
											});
										}
									}}
								/>
								<ContextMenuItem
									label="Copy relative path"
									onclick={async () => {
										menuOpen = false;
										await clipboardService.write(itemPath, {
											message: "Relative path copied",
											errorMessage: "Failed to copy relative path",
										});
									}}
								/>
							</ContextMenuSection>
						{/snippet}
					</ContextMenuItemSubmenu>
				</ContextMenuSection>
			{/if}

			<ContextMenuSection>
				{#if !isChangedFolderItem(item)}
					<ContextMenuItem
						label="Open in {$userSettings.defaultCodeEditor.displayName}"
						icon="open-in-ide"
						disabled={deletion}
						onclick={async () => {
							menuOpen = false;
							try {
								const project = await projectService.fetchProject(projectId);
								const projectPath = project?.path;
								if (projectPath) {
									for (let change of item.changes) {
										const path = getEditorUri({
											schemeId: $userSettings.defaultCodeEditor.schemeIdentifer,
											path: [vscodePath(projectPath), change.path],
										});
										urlService.openExternalUrl(path);
									}
								}
							} catch {
								chipToasts.error("Failed to open in editor");
								console.error("Failed to open in editor");
							}
						}}
					/>
					<ContextMenuItem
						label="Open in Xcode"
						icon="open-in-ide"
						disabled={deletion}
						onclick={async () => {
							try {
								const project = await projectService.fetchProject(projectId);
								const projectPath = project?.path;
								if (projectPath) {
									const filePaths = item.changes.map((c) => c.path);
									for (const fp of filePaths) {
										await backend.invoke("open_in_xcode", { projectPath, filePath: fp, line: null });
									}
								}
								chipToasts.success("Opened in Xcode");
								menuOpen = false;
							} catch {
								chipToasts.error("Failed to open in Xcode");
								console.error("Failed to open in Xcode");
							}
						}}
					/>
				{/if}
				{#if itemPath}
					<ContextMenuItem
						label={showInFolderLabel}
						icon="open-in-folder"
						onclick={async () => {
							menuOpen = false;
							const project = await projectService.fetchProject(projectId);
							const projectPath = project?.path;
							if (projectPath) {
								const absPath = await backend.joinPath(projectPath, itemPath);
								await fileService.showFileInFolder(absPath);
							}
						}}
					/>
				{/if}
			</ContextMenuSection>

			{#if annotationService.count > 0}
				<ContextMenuSection>
					{#if annotationCount > 0}
						<ContextMenuItem
							label="Export annotations for this context ({annotationCount})"
							icon="copy"
							onclick={async () => {
								const md = annotationService.toMarkdown(annotationCtx);
								try {
									const { save } = await import("@tauri-apps/plugin-dialog");
									const { writeTextFile } = await import("@tauri-apps/plugin-fs");
									const filePath = await save({
										defaultPath: "annotations.md",
										filters: [{ name: "Markdown", extensions: ["md"] }],
									});
									if (filePath) {
										await writeTextFile(filePath, md);
										chipToasts.success(`Annotations exported to ${filePath}`);
									}
								} catch (err) {
									console.error("Failed to export annotations", err);
									chipToasts.error("Failed to export annotations");
								}
								contextMenu.close();
							}}
						/>
						<ContextMenuItem
							label="Copy annotations for this context"
							icon="copy"
							onclick={() => {
								const md = annotationService.toMarkdown(annotationCtx);
								clipboardService.write(md, { message: "Annotations copied to clipboard" });
								contextMenu.close();
							}}
						/>
					{/if}
					<ContextMenuItem
						label="Export all annotations ({annotationService.count})"
						icon="copy"
						onclick={async () => {
							const md = annotationService.toMarkdownAll();
							try {
								const { save } = await import("@tauri-apps/plugin-dialog");
								const { writeTextFile } = await import("@tauri-apps/plugin-fs");
								const filePath = await save({
									defaultPath: "annotations.md",
									filters: [{ name: "Markdown", extensions: ["md"] }],
								});
								if (filePath) {
									await writeTextFile(filePath, md);
									chipToasts.success(`Annotations exported to ${filePath}`);
								}
							} catch (err) {
								console.error("Failed to export annotations", err);
								chipToasts.error("Failed to export annotations");
							}
							contextMenu.close();
						}}
					/>
					<ContextMenuItem
						label="Copy all annotations to clipboard"
						icon="copy"
						onclick={() => {
							const md = annotationService.toMarkdownAll();
							clipboardService.write(md, { message: "Annotations copied to clipboard" });
							contextMenu.close();
						}}
					/>
					<ContextMenuItem
						label="Clear all annotations"
						icon="bin"
						onclick={() => {
							annotationService.clear();
							chipToasts.success("All annotations cleared");
							contextMenu.close();
						}}
					/>
				</ContextMenuSection>
			{/if}

			{#if canUseGBAI && isUncommitted}
				<ContextMenuSection>
					<ContextMenuItemSubmenu label="Experimental AI" icon="lab">
						{#snippet submenu(_sub)}
							<ContextMenuSection>
								<ContextMenuItem
									label="Branch changes"
									onclick={() => {
										menuOpen = false;
										triggerBranchChanges(item.changes);
									}}
									disabled={branchingChanges.current.isLoading}
								/>
							</ContextMenuSection>
						{/snippet}
					</ContextMenuItemSubmenu>
				</ContextMenuSection>
			{/if}
		{:else}
			<ContextMenuSection>
				<p class="text-13">'Woops! Malformed data :(</p>
			</ContextMenuSection>
		{/if}
	</ContextMenu>
{/if}

<DiscardChangesModal bind:this={discardModal} {projectId} {selectionId} />
<StashIntoBranchModal bind:this={stashModal} {projectId} />
<AbsorbPlanModal bind:this={absorbModal} {projectId} {stackId} />
