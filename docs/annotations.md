# Code Annotations

Annotations let you leave review comments directly on diff lines inside GitButler. Comments are stored in-memory for the current session and can be exported as Markdown — useful for code reviews, PR feedback, or personal notes while reading through changes.

## Adding Annotations

### From the diff view (drag to select)

1. In any unified diff, **click and drag** across one or more lines.
2. When you release, an inline editor appears anchored to the selected range.
3. Type your comment and press **⌘+Enter** (or click **Comment**) to save.
4. Press **Escape** or click **Cancel** to discard.

### From the line context menu

1. **Right-click** any line in a diff hunk.
2. Select **Add comment**.
3. The annotation editor opens for that single line.

## Viewing Annotations

Saved annotations appear as cards directly below the relevant hunk. Each card shows:

- The line range (e.g. "Line 42" or "Lines 10–15")
- Your comment text

The selected lines are highlighted with a warm accent color so you can quickly spot annotated regions.

## Editing & Deleting

- Click the **✏️** button on an annotation card to re-open the editor with the existing text.
- Click the **🗑️** button once to enter confirmation mode, then click **Yes** to delete (or **No** to cancel).

## Annotation Context

Every annotation is scoped to a **context** — the thing you were looking at when you created it:

| Context | When it applies |
|---------|----------------|
| **Commit** | Viewing a specific commit's diff |
| **Branch** | Viewing a branch's combined diff |
| **Worktree** | Viewing uncommitted changes |

This means you can annotate the same file in different commits independently. Export and clear operations respect context boundaries.

## Exporting

Annotations can be exported as Markdown via context menus on **commits** or **changed files**.

### Export options

| Action | Scope | Available from |
|--------|-------|----------------|
| **Copy annotations for this commit/context** | Current context only | Commit context menu, file context menu |
| **Export annotations for this commit/context** | Current context only (save to file) | Commit context menu, file context menu |
| **Copy all annotations to clipboard** | All contexts | Commit context menu, file context menu |
| **Export all annotations** | All contexts (save to file) | Commit context menu, file context menu |
| **Clear all annotations** | Removes everything | Commit context menu, file context menu |

These options only appear when at least one annotation exists.

### Markdown format

Exported Markdown follows this structure:

```markdown
# Code Review Annotations

> **Commit:** fix: handle edge case in parser

_Generated 4/17/2026, 10:30:00 AM_

## src/lib/parser.ts

- **line 42:**
  ```diff
  -const result = parse(input);
  +const result = safeParse(input);
  ```
  This change fixes the crash when input is undefined.

- **lines 58–60:** Consider adding a test for the empty string case.

## src/lib/utils.ts

- **old line 12, new line 15:** Why was this moved?
```

Key formatting details:

- **Single lines** render as `line N`.
- **Ranges** render as `lines N–M`.
- **Context lines** (same line in old and new) use the new line number.
- **Mixed ranges** (different old/new) render as `old line N, new line M` or `old lines N–M, new lines N–M`.
- When the selected range includes actual diff content, a fenced `diff` code block is included above the comment text.
- When exporting all annotations, they are grouped by context (commit/branch/worktree) then by file, sorted by line number.

## Storage

Annotations are **in-memory only** — they do not persist across app restarts. This is intentional: they're meant for ephemeral review sessions, not permanent records. Export before closing if you need to keep them.
