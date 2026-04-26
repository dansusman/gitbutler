// @vitest-environment jsdom
import LineSelection, {
	type LineDragEndParams,
	type LineSelectionParams,
} from "$components/hunkDiff/lineSelection.svelte";
import { SectionType, type Row } from "$lib/utils/diffParsing";
import { describe, expect, test, vi } from "vitest";

function row(index: number, kind: "+" | "-" | " "): Row {
	const isDelta = kind !== " ";
	const oldLine = kind === "+" ? undefined : index + 1;
	const newLine = kind === "-" ? undefined : index + 1;
	return {
		encodedLineId: `mock|${oldLine ?? ""}|${newLine ?? ""}` as unknown as Row["encodedLineId"],
		beforeLineNumber: oldLine,
		afterLineNumber: newLine,
		tokens: [`line-${index}`],
		type:
			kind === "+"
				? SectionType.AddedLines
				: kind === "-"
					? SectionType.RemovedLines
					: SectionType.Context,
		size: 0,
		isLast: false,
		isDeltaLine: isDelta,
		locks: undefined,
	};
}

function makeRows(): Row[] {
	return [row(0, " "), row(1, "+"), row(2, "+"), row(3, "+"), row(4, " ")];
}

function leftMouseEvent(props: Partial<MouseEvent> = {}): MouseEvent {
	const ev = new MouseEvent("mousedown", { button: 0, buttons: 1, ...props });
	return ev;
}

describe("LineSelection — Phase 5 gesture rewrite", () => {
	test("single click on a delta row fires onDragEnd with a 1-row range when popover is wired", () => {
		const sel = new LineSelection();
		const rows = makeRows();
		sel.setRows(rows);
		const onLineClick = vi.fn<(p: LineSelectionParams) => void>();
		const onDragEnd = vi.fn<(p: LineDragEndParams) => void>();
		sel.setOnLineClick(onLineClick);
		sel.setOnDragEnd(onDragEnd);

		sel.onStart(leftMouseEvent(), rows[2]!, 2);
		expect(onLineClick).not.toHaveBeenCalled();

		sel.onEnd(new MouseEvent("mouseup", { clientX: 50, clientY: 60 }));

		// With onDragEnd wired (the popover host), single-clicks fire
		// onDragEnd with start === end so the host can open the popover
		// for tap-to-stage / tap-to-split. The legacy onLineClick path
		// stays dormant in this configuration.
		expect(onLineClick).not.toHaveBeenCalled();
		expect(onDragEnd).toHaveBeenCalledTimes(1);
		const params = onDragEnd.mock.calls[0]![0];
		expect(params.startIdx).toBe(2);
		expect(params.endIdx).toBe(2);
		expect(params.clientX).toBe(50);
	});

	test("single click falls back to onLineClick when onDragEnd is not wired", () => {
		const sel = new LineSelection();
		const rows = makeRows();
		sel.setRows(rows);
		const onLineClick = vi.fn<(p: LineSelectionParams) => void>();
		sel.setOnLineClick(onLineClick);
		// No onDragEnd wired (e.g. Storybook or commit-view-without-popover).

		sel.onStart(leftMouseEvent(), rows[2]!, 2);
		sel.onEnd(new MouseEvent("mouseup"));

		expect(onLineClick).toHaveBeenCalledTimes(1);
	});

	test("multi-row drag fires onDragEnd with mouseup coordinates and skips onLineClick", () => {
		const sel = new LineSelection();
		const rows = makeRows();
		sel.setRows(rows);
		const onLineClick = vi.fn<(p: LineSelectionParams) => void>();
		const onDragEnd = vi.fn<(p: LineDragEndParams) => void>();
		sel.setOnLineClick(onLineClick);
		sel.setOnDragEnd(onDragEnd);

		sel.onStart(leftMouseEvent(), rows[1]!, 1);
		sel.onMoveOver(leftMouseEvent({}), rows[2]!, 2);
		sel.onMoveOver(leftMouseEvent({}), rows[3]!, 3);
		sel.onEnd(new MouseEvent("mouseup", { clientX: 123, clientY: 456 }));

		expect(onLineClick).not.toHaveBeenCalled();
		expect(onDragEnd).toHaveBeenCalledTimes(1);
		const params = onDragEnd.mock.calls[0]![0];
		expect(params.startIdx).toBe(1);
		expect(params.endIdx).toBe(3);
		expect(params.clientX).toBe(123);
		expect(params.clientY).toBe(456);
		expect(params.newRange).toEqual({ startLine: 2, endLine: 4 });
	});

	test("drag does not stage anything mid-motion (no onLineClick during onMoveOver)", () => {
		const sel = new LineSelection();
		const rows = makeRows();
		sel.setRows(rows);
		const onLineClick = vi.fn<(p: LineSelectionParams) => void>();
		sel.setOnLineClick(onLineClick);
		sel.setOnDragEnd(vi.fn<(p: LineDragEndParams) => void>());

		sel.onStart(leftMouseEvent(), rows[1]!, 1);
		sel.onMoveOver(leftMouseEvent({}), rows[2]!, 2);
		sel.onMoveOver(leftMouseEvent({}), rows[3]!, 3);

		// onLineClick must not fire mid-drag — the popover is the single
		// decision point per the design doc.
		expect(onLineClick).not.toHaveBeenCalled();
	});

	test("modifier state at mousedown is preserved through deferred onLineClick fallback", () => {
		const sel = new LineSelection();
		const rows = makeRows();
		sel.setRows(rows);
		const onLineClick = vi.fn<(p: LineSelectionParams) => void>();
		sel.setOnLineClick(onLineClick);
		// No onDragEnd — forces the legacy onLineClick fallback.

		const ev = new MouseEvent("mousedown", {
			button: 0,
			buttons: 1,
			shiftKey: true,
			ctrlKey: true,
		});
		sel.onStart(ev, rows[2]!, 2);
		sel.onEnd();

		expect(onLineClick).toHaveBeenCalledTimes(1);
		const params = onLineClick.mock.calls[0]![0];
		expect(params.shift).toBe(true);
		expect(params.ctrlOrMeta).toBe(true);
	});

	test("onEnd is a no-op when no gesture started", () => {
		const sel = new LineSelection();
		sel.setRows(makeRows());
		const onLineClick = vi.fn<(p: LineSelectionParams) => void>();
		const onDragEnd = vi.fn<(p: LineDragEndParams) => void>();
		sel.setOnLineClick(onLineClick);
		sel.setOnDragEnd(onDragEnd);

		sel.onEnd(new MouseEvent("mouseup"));

		expect(onLineClick).not.toHaveBeenCalled();
		expect(onDragEnd).not.toHaveBeenCalled();
	});
});
