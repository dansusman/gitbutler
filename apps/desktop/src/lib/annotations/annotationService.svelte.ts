import { InjectionToken } from "@gitbutler/core/context";

export const ANNOTATION_SERVICE = new InjectionToken<AnnotationService>("AnnotationService");

export interface LineRange {
	startLine: number;
	endLine: number;
}

export interface DiffLine {
	prefix: '+' | '-' | ' ';
	content: string;
}

export type AnnotationContext =
	| { type: 'commit'; commitId: string }
	| { type: 'branch'; branchName: string }
	| { type: 'worktree' };

export function annotationContextKey(ctx: AnnotationContext): string {
	switch (ctx.type) {
		case 'commit':
			return `commit:${ctx.commitId}`;
		case 'branch':
			return `branch:${ctx.branchName}`;
		case 'worktree':
			return 'worktree';
	}
}

export interface Annotation {
	filePath: string;
	oldRange: LineRange | undefined;
	newRange: LineRange | undefined;
	diffLines: DiffLine[];
	text: string;
	createdAt: Date;
	context: AnnotationContext;
}

function annotationKey(context: AnnotationContext, filePath: string, oldRange: LineRange | undefined, newRange: LineRange | undefined): string {
	const ctxPart = annotationContextKey(context);
	const oldPart = oldRange ? `old:${oldRange.startLine}-${oldRange.endLine}` : '';
	const newPart = newRange ? `new:${newRange.startLine}-${newRange.endLine}` : '';
	return `${ctxPart}|${filePath}:${oldPart}:${newPart}`;
}

export function formatLineLabel(
	oldRange: LineRange | undefined,
	newRange: LineRange | undefined,
	opts: { capitalize?: boolean } = {},
): string {
	const cap = opts.capitalize !== false;
	const isContextLine =
		oldRange !== undefined &&
		newRange !== undefined &&
		oldRange.startLine === oldRange.endLine &&
		newRange.startLine === newRange.endLine;

	if (isContextLine) {
		return `${cap ? 'Line' : 'line'} ${newRange.startLine}`;
	}

	const isContextRange =
		oldRange !== undefined &&
		newRange !== undefined &&
		oldRange.endLine - oldRange.startLine === newRange.endLine - newRange.startLine;

	if (isContextRange) {
		const r = newRange;
		return r.startLine === r.endLine
			? `${cap ? 'Line' : 'line'} ${r.startLine}`
			: `${cap ? 'Lines' : 'lines'} ${r.startLine}\u2013${r.endLine}`;
	}

	const parts: string[] = [];
	if (oldRange) {
		const r = oldRange;
		parts.push(r.startLine === r.endLine
			? `${cap ? 'Old' : 'old'} line ${r.startLine}`
			: `${cap ? 'Old' : 'old'} lines ${r.startLine}\u2013${r.endLine}`);
	}
	if (newRange) {
		const r = newRange;
		parts.push(r.startLine === r.endLine
			? `${cap ? 'New' : 'new'} line ${r.startLine}`
			: `${cap ? 'New' : 'new'} lines ${r.startLine}\u2013${r.endLine}`);
	}
	return parts.join(', ');
}

export class AnnotationService {
	private annotations: Map<string, Annotation> = $state(new Map());

	get all(): Annotation[] {
		return Array.from(this.annotations.values());
	}

	get count(): number {
		return this.annotations.size;
	}

	allForContext(context: AnnotationContext): Annotation[] {
		const ctxKey = annotationContextKey(context);
		return this.all.filter((a) => annotationContextKey(a.context) === ctxKey);
	}

	countForContext(context: AnnotationContext): number {
		return this.allForContext(context).length;
	}

	add(context: AnnotationContext, filePath: string, oldRange: LineRange | undefined, newRange: LineRange | undefined, diffLines: DiffLine[], text: string): void {
		const key = annotationKey(context, filePath, oldRange, newRange);
		this.annotations.set(key, {
			filePath,
			oldRange,
			newRange,
			diffLines,
			text,
			createdAt: new Date(),
			context,
		});
		this.annotations = new Map(this.annotations);
	}

	remove(context: AnnotationContext, filePath: string, oldRange: LineRange | undefined, newRange: LineRange | undefined): void {
		const key = annotationKey(context, filePath, oldRange, newRange);
		this.annotations.delete(key);
		this.annotations = new Map(this.annotations);
	}

	get(context: AnnotationContext, filePath: string, oldRange: LineRange | undefined, newRange: LineRange | undefined): Annotation | undefined {
		return this.annotations.get(annotationKey(context, filePath, oldRange, newRange));
	}

	getForFile(context: AnnotationContext, filePath: string): Annotation[] {
		return this.allForContext(context).filter((a) => a.filePath === filePath);
	}

	hasAnyForLine(context: AnnotationContext, filePath: string, lineNumber: number): boolean {
		return this.allForContext(context).some((a) => {
			if (a.filePath !== filePath) return false;
			if (a.oldRange && lineNumber >= a.oldRange.startLine && lineNumber <= a.oldRange.endLine) return true;
			if (a.newRange && lineNumber >= a.newRange.startLine && lineNumber <= a.newRange.endLine) return true;
			return false;
		});
	}

	clear(): void {
		this.annotations = new Map();
	}

	clearContext(context: AnnotationContext): void {
		const ctxKey = annotationContextKey(context);
		for (const [key, annotation] of this.annotations) {
			if (annotationContextKey(annotation.context) === ctxKey) {
				this.annotations.delete(key);
			}
		}
		this.annotations = new Map(this.annotations);
	}

	toMarkdownAll(): string {
		const grouped = new Map<string, Map<string, Annotation[]>>();
		for (const annotation of this.all) {
			const ctxKey = annotationContextKey(annotation.context);
			if (!grouped.has(ctxKey)) {
				grouped.set(ctxKey, new Map());
			}
			const fileMap = grouped.get(ctxKey)!;
			const existing = fileMap.get(annotation.filePath) ?? [];
			existing.push(annotation);
			fileMap.set(annotation.filePath, existing);
		}

		const lines: string[] = [];
		lines.push("# Code Review Annotations");
		lines.push("");
		lines.push(`_Generated ${new Date().toLocaleString()}_`);
		lines.push("");

		for (const [ctxKey, fileMap] of grouped) {
			const sampleAnnotation = this.all.find((a) => annotationContextKey(a.context) === ctxKey)!;
			const ctx = sampleAnnotation.context;
			if (ctx.type === 'commit') {
				lines.push(`## Commit: ${ctx.commitId.slice(0, 7)}`);
			} else if (ctx.type === 'branch') {
				lines.push(`## Branch: ${ctx.branchName}`);
			} else {
				lines.push(`## Unstaged changes`);
			}
			lines.push("");

			for (const [filePath, fileAnnotations] of fileMap) {
				const sorted = fileAnnotations.sort((a, b) => {
					const aLine = a.newRange?.startLine ?? a.oldRange?.startLine ?? 0;
					const bLine = b.newRange?.startLine ?? b.oldRange?.startLine ?? 0;
					return aLine - bLine;
				});
				lines.push(`### ${filePath}`);
				lines.push("");

				for (const annotation of sorted) {
					this.renderAnnotationMarkdown(annotation, lines);
				}

				lines.push("");
			}
		}

		return lines.join("\n");
	}

	toMarkdown(context: AnnotationContext, commitMessage?: string): string {
		const contextAnnotations = this.allForContext(context);
		const grouped = new Map<string, Annotation[]>();
		for (const annotation of contextAnnotations) {
			const existing = grouped.get(annotation.filePath) ?? [];
			existing.push(annotation);
			grouped.set(annotation.filePath, existing);
		}

		const lines: string[] = [];
		lines.push("# Code Review Annotations");
		lines.push("");

		if (commitMessage) {
			lines.push(`> **Commit:** ${commitMessage}`);
			lines.push("");
		} else if (context.type === 'worktree') {
			lines.push("> **Context:** Unstaged changes");
			lines.push("");
		} else if (context.type === 'branch') {
			lines.push(`> **Branch:** ${context.branchName}`);
			lines.push("");
		}

		lines.push(`_Generated ${new Date().toLocaleString()}_`);
		lines.push("");

		this.renderFileGroups(grouped, lines, '##');

		return lines.join("\n");
	}

	private renderFileGroups(grouped: Map<string, Annotation[]>, lines: string[], headingPrefix: string): void {
		for (const [filePath, fileAnnotations] of grouped) {
			const sorted = fileAnnotations.sort((a, b) => {
				const aLine = a.newRange?.startLine ?? a.oldRange?.startLine ?? 0;
				const bLine = b.newRange?.startLine ?? b.oldRange?.startLine ?? 0;
				return aLine - bLine;
			});
			lines.push(`${headingPrefix} ${filePath}`);
			lines.push("");

			for (const annotation of sorted) {
				this.renderAnnotationMarkdown(annotation, lines);
			}

			lines.push("");
		}
	}

	private renderAnnotationMarkdown(annotation: Annotation, lines: string[]): void {
		const label = formatLineLabel(annotation.oldRange, annotation.newRange, { capitalize: false });
		const hasDiffLines = annotation.diffLines.length > 0;

		if (hasDiffLines) {
			lines.push(`- **${label}:**`);
			lines.push('  ```diff');
			for (const dl of annotation.diffLines) {
				lines.push(`  ${dl.prefix}${dl.content}`);
			}
			lines.push('  ```');
			lines.push(`  ${annotation.text}`);
		} else {
			lines.push(`- **${label}:** ${annotation.text}`);
		}
	}
}
