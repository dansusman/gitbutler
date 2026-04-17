<script lang="ts">
	import { ANNOTATION_SERVICE, formatLineLabel, type AnnotationContext, type DiffLine } from "$lib/annotations/annotationService.svelte";
	import { inject } from "@gitbutler/core/context";

	interface Props {
		filePath: string;
		oldRange: { startLine: number; endLine: number } | undefined;
		newRange: { startLine: number; endLine: number } | undefined;
		diffLines: DiffLine[];
		annotationContext: AnnotationContext;
		onclose: () => void;
	}

	const { filePath, oldRange, newRange, diffLines, annotationContext, onclose }: Props = $props();

	const annotationService = inject(ANNOTATION_SERVICE);

	const existing = $derived(annotationService.get(annotationContext, filePath, oldRange, newRange));
	let text = $state(existing?.text ?? "");
	let inputEl: HTMLTextAreaElement | undefined = $state();

	$effect(() => {
		inputEl?.focus();
	});

	function save() {
		const trimmed = text.trim();
		if (trimmed.length > 0) {
			annotationService.add(annotationContext, filePath, oldRange, newRange, diffLines, trimmed);
		}
		onclose();
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
			e.preventDefault();
			save();
		} else if (e.key === "Escape") {
			e.preventDefault();
			onclose();
		}
	}

	const lineLabel = $derived(formatLineLabel(oldRange, newRange, { capitalize: false }));
</script>

<div class="annotation-editor">
	<div class="annotation-editor__header">
		<span class="text-12 text-semibold clr-text-1">
			Comment on {lineLabel}
		</span>
	</div>
	<textarea
		bind:this={inputEl}
		bind:value={text}
		class="annotation-editor__input text-12"
		placeholder="Add your comment here..."
		rows="3"
		onkeydown={handleKeydown}
	></textarea>
	<div class="annotation-editor__actions">
		<span class="text-11 clr-text-3">⌘+Enter to save</span>
		<div class="annotation-editor__buttons">
			<button class="annotation-editor__btn" onclick={onclose}>
				Cancel
			</button>
			<button class="annotation-editor__btn annotation-editor__btn--primary" onclick={save}>
				Comment
			</button>
		</div>
	</div>
</div>

<style lang="postcss">
	.annotation-editor {
		display: flex;
		flex-direction: column;
		gap: 8px;
		margin-top: 4px;
		padding: 12px;
		border: 1px solid var(--focus-stroke);
		border-radius: var(--radius-m);
		background-color: var(--bg-1);
	}

	.annotation-editor__header {
		display: flex;
		align-items: center;
	}

	.annotation-editor__input {
		width: 100%;
		min-height: 60px;
		padding: 8px;
		border: 1px solid var(--border-2);
		border-radius: var(--radius-s);
		background-color: var(--bg-2);
		color: var(--text-1);
		font-family: var(--font-mono);
		font-size: 13px;
		resize: vertical;

		&:focus {
			outline: none;
			border-color: var(--focus-stroke);
		}

		&::placeholder {
			color: var(--text-3);
		}
	}

	.annotation-editor__actions {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}

	.annotation-editor__buttons {
		display: flex;
		gap: 6px;
	}

	.annotation-editor__btn {
		padding: 4px 12px;
		border: 1px solid var(--border-2);
		border-radius: var(--radius-s);
		background-color: var(--bg-2);
		color: var(--text-1);
		font-size: 12px;
		cursor: pointer;

		&:hover {
			background-color: var(--bg-3);
		}
	}

	.annotation-editor__btn--primary {
		border-color: var(--fill-pop-bg);
		background-color: var(--fill-pop-bg);
		color: var(--fill-pop-text);

		&:hover {
			opacity: 0.9;
		}
	}
</style>
