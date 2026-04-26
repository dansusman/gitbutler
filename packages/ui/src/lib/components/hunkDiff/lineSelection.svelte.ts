import { isMobileTouchDevice } from "$lib/utils/browserAgent";
import { type Row } from "$lib/utils/diffParsing";

export interface LineSelectionParams {
	index: number;
	oldLine: number | undefined;
	newLine: number | undefined;
	shift: boolean;
	ctrlOrMeta: boolean;
	startIndex: number;
	rows: Row[] | undefined;
}

export interface LineRange {
	startLine: number;
	endLine: number;
}

export interface LineDragEndParams {
	oldRange: LineRange | undefined;
	newRange: LineRange | undefined;
	startIdx: number;
	endIdx: number;
	rows: Row[] | undefined;
	/**
	 * The DOM coordinates of the mouseup that ended the drag. Forwarded
	 * so callers can anchor a popover (see Phase 5 of the line-by-line
	 * commits design). May be `undefined` for synthetic / touch paths.
	 */
	clientX?: number;
	clientY?: number;
}

type ToggleLineSelectionFn = (params: LineSelectionParams) => void;
type LineDragEndFn = (params: LineDragEndParams) => void;

interface TouchCoords {
	x: number;
	y: number;
}

export default class LineSelection {
	private readonly mobileTouchDevice = isMobileTouchDevice();
	private rows: Row[] | undefined;
	private _touchStart = $state<TouchCoords>();
	private _touchMove = $state<TouchCoords>();
	private _selectionStart = $state<number>();
	private _selectionEnd = $state<number>();
	private onLineClick: ToggleLineSelectionFn | undefined;
	private onDragEnd: LineDragEndFn | undefined;
	private _annotDragStartIdx = $state<number | undefined>();
	private _annotDragEndIdx = $state<number | undefined>();

	get annotDragStartIdx() {
		return this._annotDragStartIdx;
	}

	get annotDragEndIdx() {
		return this._annotDragEndIdx;
	}

	constructor() {
		if (typeof document !== 'undefined') {
			document.addEventListener('mouseup', (ev: MouseEvent) => {
				if (this._selectionStart !== undefined) {
					this.onEnd(ev);
				}
			});
		}
	}

	setRows(rows: Row[]) {
		this.rows = rows;
	}

	setOnLineClick(fn: ToggleLineSelectionFn | undefined) {
		this.onLineClick = fn;
	}

	setOnDragEnd(fn: LineDragEndFn | undefined) {
		this.onDragEnd = fn;
	}

	/**
	 * The shift/ctrl modifier state captured at gesture start. Used so
	 * single-click staging (deferred to mouseup) can preserve modifier
	 * semantics from the original mousedown.
	 */
	private _startShift = false;
	private _startCtrlOrMeta = false;
	private _startRow: Row | undefined;

	onStart(ev: MouseEvent, row: Row, index: number) {
		if (ev.buttons !== 1) return;
		if (this.mobileTouchDevice) return;
		ev.preventDefault();
		ev.stopPropagation();

		this._selectionStart = index;
		this._startShift = ev.shiftKey;
		this._startCtrlOrMeta = ev.ctrlKey || ev.metaKey;
		this._startRow = row;
		if (this.onDragEnd) {
			this._annotDragStartIdx = index;
			this._annotDragEndIdx = index;
		}
		// Phase 5a (line-by-line commits): no longer stage on mousedown.
		// Single-line clicks stage on mouseup-no-movement (see `onEnd`);
		// multi-line drags open the selection popover via `onDragEnd`.
		// This makes the popover the single decision point for what a
		// drag means.
	}

	onMoveOver(ev: MouseEvent, _row: Row, index: number) {
		if (this.mobileTouchDevice) return;
		if (this._selectionStart === undefined) return;
		if (ev.buttons === 1) {
			ev.preventDefault();
			ev.stopPropagation();

			this._selectionEnd = index;
			if (this.onDragEnd) {
				this._annotDragEndIdx = index;
			}
			// Phase 5a: drag no longer stages mid-motion. Only updates the
			// visual selection range.
		}
	}

	onEnd(mouseEvent?: MouseEvent) {
		if (this._selectionStart !== undefined && this.rows) {
			const endIdx = this._selectionEnd ?? this._selectionStart;
			const lo = Math.min(this._selectionStart, endIdx);
			const hi = Math.max(this._selectionStart, endIdx);

			// Phase 5 (line-by-line commits): the popover is the single
			// decision point for what a click or drag means. We always
			// fire `onDragEnd` (covers both single-line click and
			// multi-line drag) so the host can open the popover at the
			// gesture endpoint. The legacy `onLineClick` synchronous
			// stage-on-click is only fired when no `onDragEnd` is wired,
			// preserving Storybook / non-popover consumers.
			if (!this.onDragEnd && lo === hi && this._startRow && this.onLineClick) {
				this.onLineClick({
					index: lo,
					oldLine: this._startRow.beforeLineNumber,
					newLine: this._startRow.afterLineNumber,
					shift: this._startShift,
					ctrlOrMeta: this._startCtrlOrMeta,
					startIndex: lo,
					rows: this.rows,
				});
			}

			if (this.onDragEnd) {
			const selectedRows = this.rows.slice(lo, hi + 1);

			let oldMin: number | undefined;
			let oldMax: number | undefined;
			let newMin: number | undefined;
			let newMax: number | undefined;

			for (const r of selectedRows) {
				if (r.beforeLineNumber !== undefined) {
					oldMin = oldMin === undefined ? r.beforeLineNumber : Math.min(oldMin, r.beforeLineNumber);
					oldMax = oldMax === undefined ? r.beforeLineNumber : Math.max(oldMax, r.beforeLineNumber);
				}
				if (r.afterLineNumber !== undefined) {
					newMin = newMin === undefined ? r.afterLineNumber : Math.min(newMin, r.afterLineNumber);
					newMax = newMax === undefined ? r.afterLineNumber : Math.max(newMax, r.afterLineNumber);
				}
			}

			const oldRange = oldMin !== undefined && oldMax !== undefined
				? { startLine: oldMin, endLine: oldMax } : undefined;
			const newRange = newMin !== undefined && newMax !== undefined
				? { startLine: newMin, endLine: newMax } : undefined;

			if (oldRange || newRange) {
				this.onDragEnd({
					oldRange,
					newRange,
					startIdx: lo,
					endIdx: hi,
					rows: this.rows,
					clientX: mouseEvent?.clientX,
					clientY: mouseEvent?.clientY,
				});
			}
			}
		}
		this._touchMove = undefined;
		this._touchStart = undefined;
		this._selectionStart = undefined;
		this._selectionEnd = undefined;
		this._annotDragStartIdx = undefined;
		this._annotDragEndIdx = undefined;
		this._startRow = undefined;
	}

	onTouchStart(ev: TouchEvent) {
		this._touchStart = { x: ev.touches[0].clientX, y: ev.touches[0].clientY };
	}

	onTouchMove(ev: TouchEvent) {
		this._touchMove = { x: ev.touches[0].clientX, y: ev.touches[0].clientY };
	}

	get touchStart() {
		return this._touchStart;
	}

	get touchMove() {
		return this._touchMove;
	}

	touchSelectionStart(row: Row, index: number) {
		if (this._selectionStart !== undefined) return;
		this._selectionStart = index;
		this.onLineClick?.({
			index,
			oldLine: row.beforeLineNumber,
			newLine: row.afterLineNumber,
			shift: false,
			ctrlOrMeta: false,
			startIndex: index,
			rows: this.rows,
		});
	}

	touchSelectionEnd(row: Row, index: number) {
		if (this._selectionStart === undefined || this._selectionEnd === index) return;
		this._selectionEnd = index;
		this.onLineClick?.({
			index,
			oldLine: row.beforeLineNumber,
			newLine: row.afterLineNumber,
			shift: false,
			ctrlOrMeta: false,
			startIndex: this._selectionStart,
			rows: this.rows,
		});
	}
}
