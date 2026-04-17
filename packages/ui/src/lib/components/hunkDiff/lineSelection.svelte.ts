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
			document.addEventListener('mouseup', () => {
				if (this._selectionStart !== undefined) {
					this.onEnd();
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

	onStart(ev: MouseEvent, row: Row, index: number) {
		if (ev.buttons !== 1) return;
		if (this.mobileTouchDevice) return;
		ev.preventDefault();
		ev.stopPropagation();

		this._selectionStart = index;
		if (this.onDragEnd) {
			this._annotDragStartIdx = index;
			this._annotDragEndIdx = index;
		}
		this.onLineClick?.({
			index,
			oldLine: row.beforeLineNumber,
			newLine: row.afterLineNumber,
			shift: ev.shiftKey,
			ctrlOrMeta: ev.ctrlKey || ev.metaKey,
			startIndex: index,
			rows: this.rows,
		});
	}

	onMoveOver(ev: MouseEvent, row: Row, index: number) {
		if (this.mobileTouchDevice) return;
		if (this._selectionStart === undefined) return;
		if (ev.buttons === 1) {
			ev.preventDefault();
			ev.stopPropagation();

			this._selectionEnd = index;
			if (this.onDragEnd) {
				this._annotDragEndIdx = index;
			}
			this.onLineClick?.({
				index,
				oldLine: row.beforeLineNumber,
				newLine: row.afterLineNumber,
				shift: ev.shiftKey,
				ctrlOrMeta: ev.ctrlKey || ev.metaKey,
				startIndex: this._selectionStart,
				rows: this.rows,
			});
		}
	}

	onEnd() {
		if (this._selectionStart !== undefined && this.onDragEnd && this.rows) {
			const endIdx = this._selectionEnd ?? this._selectionStart;
			const lo = Math.min(this._selectionStart, endIdx);
			const hi = Math.max(this._selectionStart, endIdx);
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
				});
			}
		}
		this._touchMove = undefined;
		this._touchStart = undefined;
		this._selectionStart = undefined;
		this._selectionEnd = undefined;
		this._annotDragStartIdx = undefined;
		this._annotDragEndIdx = undefined;
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
