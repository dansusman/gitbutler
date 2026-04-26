<script lang="ts">
	/**
	 * Phase 5 popover for the line-by-line commits feature.
	 *
	 * Anchored to the mouseup / click location of a hunk gesture. Hosts
	 * the four actions defined in the design doc's
	 * "User-Facing Behavior → The gesture" section:
	 *
	 *   Stage / Unstage  (default-focused; label flips by current state)
	 *   Comment          (opens the existing annotation editor)
	 *   Split            (splits the containing natural hunk along the
	 *                    selection's row boundaries)
	 *   Cancel           (dismisses)
	 *
	 * `Esc` and click-outside dismiss via the underlying `ContextMenu`.
	 * `Enter` triggers Stage/Unstage.
	 *
	 * Implementation note: the popover takes plain `(clientX, clientY)`
	 * coordinates rather than a `MouseEvent`. We then mount a transient
	 * zero-size div at those coordinates and pass that as the
	 * `ContextMenu`'s anchor. This avoids storing a `MouseEvent` (whose
	 * `clientX/Y` getters trip Svelte 5's `state_descriptors_fixed`
	 * runtime check when held in `$state`) inside `ContextMenu`'s saved
	 * target.
	 */
	import { ContextMenu, ContextMenuItem, ContextMenuSection, TestId } from "@gitbutler/ui";

	type Props = {
		clientX: number;
		clientY: number;
		stageLabel: "Stage" | "Unstage";
		splitDisabled: boolean;
		splitDisabledReason: string | undefined;
		onstage: () => void;
		oncomment: () => void;
		onsplit: () => void;
		onclose: () => void;
	};

	const {
		clientX,
		clientY,
		stageLabel,
		splitDisabled,
		splitDisabledReason,
		onstage,
		oncomment,
		onsplit,
		onclose,
	}: Props = $props();

	let anchor: HTMLDivElement | undefined = $state();

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === "Enter") {
			e.preventDefault();
			onstage();
			onclose();
		}
	}
</script>

<svelte:window onkeydown={handleKeydown} />

<!--
	Transient zero-size anchor at the gesture endpoint. Positioned
	`fixed` so the coords are viewport-relative — same frame as
	`MouseEvent.clientX / clientY`.
-->
<div
	bind:this={anchor}
	aria-hidden="true"
	style="position: fixed; left: {clientX}px; top: {clientY}px; width: 0; height: 0; pointer-events: none;"
></div>

{#if anchor}
	<ContextMenu testId={TestId.HunkSelectionPopover} target={anchor} side="bottom" align="start" {onclose}>
		<ContextMenuSection>
			<ContextMenuItem
				testId={TestId.HunkSelectionPopover_Stage}
				label={stageLabel}
				icon="tick"
				onclick={() => {
					onstage();
					onclose();
				}}
			/>
			<ContextMenuItem
				testId={TestId.HunkSelectionPopover_Comment}
				label="Comment"
				icon="edit"
				onclick={() => {
					oncomment();
					onclose();
				}}
			/>
			<ContextMenuItem
				testId={TestId.HunkSelectionPopover_Split}
				label="Split"
				icon="split"
				disabled={splitDisabled}
				caption={splitDisabledReason}
				onclick={() => {
					if (splitDisabled) return;
					onsplit();
					onclose();
				}}
			/>
		</ContextMenuSection>
		<ContextMenuSection>
			<ContextMenuItem
				testId={TestId.HunkSelectionPopover_Cancel}
				label="Cancel"
				icon="cross"
				onclick={() => {
					onclose();
				}}
			/>
		</ContextMenuSection>
	</ContextMenu>
{/if}
