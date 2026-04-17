<script lang="ts">
	import { ANNOTATION_SERVICE, formatLineLabel, type Annotation } from "$lib/annotations/annotationService.svelte";
	import { inject } from "@gitbutler/core/context";

	interface Props {
		annotation: Annotation;
		onEditClick: () => void;
	}

	const { annotation, onEditClick }: Props = $props();

	const annotationService = inject(ANNOTATION_SERVICE);

	const lineLabel = $derived(formatLineLabel(annotation.oldRange, annotation.newRange));

	let confirmingDelete = $state(false);

	function handleDelete() {
		if (!confirmingDelete) {
			confirmingDelete = true;
			return;
		}
		annotationService.remove(annotation.context, annotation.filePath, annotation.oldRange, annotation.newRange);
		confirmingDelete = false;
	}

	function cancelDelete() {
		confirmingDelete = false;
	}
</script>

<div class="annotation-card">
	<div class="annotation-card__header">
		<span class="text-11 text-semibold clr-text-2">{lineLabel}</span>
		<div class="annotation-card__actions">
			<button class="annotation-card__action" onclick={onEditClick} title="Edit">✏️</button>
			{#if confirmingDelete}
			<button class="annotation-card__action annotation-card__action--confirm" onclick={handleDelete} title="Confirm delete">Yes</button>
			<button class="annotation-card__action" onclick={cancelDelete} title="Cancel">No</button>
		{:else}
			<button class="annotation-card__action" onclick={handleDelete} title="Delete">🗑️</button>
		{/if}
		</div>
	</div>
	<p class="annotation-card__text text-12">{annotation.text}</p>
</div>

<style lang="postcss">
	.annotation-card {
		display: flex;
		flex-direction: column;
		gap: 4px;
		margin-top: 4px;
		padding: 8px 12px;
		border: 1px solid var(--border-2);
		border-left: 3px solid var(--fill-pop-bg);
		border-radius: var(--radius-s);
		background-color: var(--bg-2);
	}

	.annotation-card__header {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}

	.annotation-card__actions {
		display: flex;
		gap: 4px;
	}

	.annotation-card__action {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 22px;
		height: 22px;
		padding: 0;
		border: none;
		border-radius: var(--radius-s);
		background: transparent;
		font-size: 12px;
		cursor: pointer;
		opacity: 0.6;

		&:hover {
			opacity: 1;
			background-color: var(--bg-3);
		}
	}

	.annotation-card__text {
		margin: 0;
		color: var(--text-1);
		white-space: pre-wrap;
		word-break: break-word;
	}
</style>
