import { getHunkLineSelector } from "../src/hunk.ts";
import { getBaseURL, startGitButler, type GitButler } from "../src/setup.ts";
import { test } from "../src/test.ts";
import { clickByTestId, getByTestId, waitForTestId, waitForTestIdToNotExist } from "../src/util.ts";
import { expect } from "@playwright/test";
import { writeFileSync } from "fs";
import { join } from "path";

let gitbutler: GitButler;

test.use({
	baseURL: getBaseURL(),
});

test.afterEach(async () => {
	await gitbutler?.destroy();
});

/**
 * Phase 5 (line-by-line commits) gesture coverage.
 *
 * Verifies the popover-on-drag behavior: a multi-row drag inside a hunk
 * gutter opens the selection popover instead of jumping straight to the
 * annotation editor, while a single click still toggles staging on the
 * targeted line.
 */
test("multi-row drag opens the selection popover", async ({ page, context }, testInfo) => {
	const workdir = testInfo.outputPath("workdir");
	const configdir = testInfo.outputPath("config");
	gitbutler = await startGitButler(workdir, configdir, context);

	const projectName = "popover-project";
	const fileName = "demo.txt";
	const projectPath = gitbutler.pathInWorkdir(projectName + "/");
	const filePath = join(projectPath, fileName);

	const baselineLines = [
		"alpha 1",
		"alpha 2",
		"alpha 3",
		"alpha 4",
		"alpha 5",
		"alpha 6",
	];
	const baseline = baselineLines.join("\n") + "\n";

	await gitbutler.runScript("project-with-remote-branches.sh");
	await gitbutler.runScript("project-with-remote-branches__commit-file-into-remote-base.sh", [
		"Seed file with baseline",
		fileName,
		baseline,
	]);
	await gitbutler.runScript("project-with-remote-branches__clone-into-new-project.sh", [
		projectName,
	]);
	await gitbutler.runScript("project-with-remote-branches__delete-project.sh", [
		"local-clone",
	]);

	await page.goto("/");
	await waitForTestId(page, "workspace-view");

	// Modify several adjacent lines to produce a single multi-row hunk.
	const modifiedLines = baselineLines.map((line, i) =>
		i >= 1 && i <= 3 ? `${line} (modified)` : line,
	);
	writeFileSync(filePath, modifiedLines.join("\n") + "\n", "utf-8");

	// Stay in worktree view (do NOT click `commit-to-new-branch-button`)
	// so `onLineDragEnd` is enabled and the popover gesture is wired.
	const uncommittedChangesList = getByTestId(page, "uncommitted-changes-file-list");
	const fileItem = uncommittedChangesList
		.getByTestId("file-list-item")
		.filter({ hasText: fileName });
	await expect(fileItem).toBeVisible();
	await fileItem.click();

	const unifiedDiffView = getByTestId(page, "unified-diff-view");
	await expect(unifiedDiffView).toBeVisible();

	// Drag from the line-number gutter of one delta line to another. The
	// modified rows show up as `+` in the right gutter at line numbers
	// 2 / 4 / 6 (3 sequential added lines after their corresponding
	// removed lines).
	const startLine = unifiedDiffView
		.locator(getHunkLineSelector(fileName, 2, "right"))
		.first();
	const endLine = unifiedDiffView
		.locator(getHunkLineSelector(fileName, 4, "right"))
		.first();
	await expect(startLine).toBeVisible();
	await expect(endLine).toBeVisible();

	const startBox = await startLine.boundingBox();
	const endBox = await endLine.boundingBox();
	if (!startBox || !endBox) throw new Error("missing bounding boxes for gutter cells");

	const startX = startBox.x + startBox.width / 2;
	const startY = startBox.y + startBox.height / 2;
	const endX = endBox.x + endBox.width / 2;
	const endY = endBox.y + endBox.height / 2;

	await page.mouse.move(startX, startY);
	await page.mouse.down();
	// Move in steps so onmouseenter fires on intermediate rows.
	await page.mouse.move((startX + endX) / 2, (startY + endY) / 2, { steps: 5 });
	await page.mouse.move(endX, endY, { steps: 5 });
	await page.mouse.up();

	// The selection popover should appear with the four expected items.
	const popover = await waitForTestId(page, "hunk-selection-popover");
	await expect(popover).toBeVisible();
	await expect(getByTestId(page, "hunk-selection-popover-stage")).toBeVisible();
	await expect(getByTestId(page, "hunk-selection-popover-comment")).toBeVisible();
	await expect(getByTestId(page, "hunk-selection-popover-split")).toBeVisible();
	await expect(getByTestId(page, "hunk-selection-popover-cancel")).toBeVisible();

	// Esc dismisses.
	await page.keyboard.press("Escape");
	await waitForTestIdToNotExist(page, "hunk-selection-popover");
});

test("Split splits the natural hunk into sub-hunks", async ({ page, context }, testInfo) => {
	const workdir = testInfo.outputPath("workdir");
	const configdir = testInfo.outputPath("config");
	gitbutler = await startGitButler(workdir, configdir, context);

	const projectName = "split-project";
	const fileName = "sections.md";
	const projectPath = gitbutler.pathInWorkdir(projectName + "/");
	const filePath = join(projectPath, fileName);

	// Baseline single-line file. After modification we'll have several
	// added rows in one natural hunk; we drag-select the middle ones and
	// click Split, expecting the natural hunk to fan out into sub-hunks.
	const baseline = "baseline\n";

	await gitbutler.runScript("project-with-remote-branches.sh");
	await gitbutler.runScript("project-with-remote-branches__commit-file-into-remote-base.sh", [
		"Seed sections file",
		fileName,
		baseline,
	]);
	await gitbutler.runScript("project-with-remote-branches__clone-into-new-project.sh", [
		projectName,
	]);
	await gitbutler.runScript("project-with-remote-branches__delete-project.sh", [
		"local-clone",
	]);

	await page.goto("/");
	await waitForTestId(page, "workspace-view");

	const added = [
		"section A line 1",
		"section A line 2",
		"section B line 1",
		"section B line 2",
		"section C line 1",
		"section C line 2",
	];
	writeFileSync(filePath, baseline + added.join("\n") + "\n", "utf-8");

	const uncommittedChangesList = getByTestId(page, "uncommitted-changes-file-list");
	const fileItem = uncommittedChangesList
		.getByTestId("file-list-item")
		.filter({ hasText: fileName });
	await expect(fileItem).toBeVisible();
	await fileItem.click();

	const unifiedDiffView = getByTestId(page, "unified-diff-view");
	await expect(unifiedDiffView).toBeVisible();

	// Before the split there's exactly one rendered hunk header.
	await expect(unifiedDiffView.locator('thead .table__title-content')).toHaveCount(1);

	// Drag from R3 ("section B line 1") to R5 ("section B line 2" / start of C).
	// Lines are 1-indexed in the view; baseline line 1 is context, then
	// 2..7 are added rows.
	const startLine = unifiedDiffView
		.locator(getHunkLineSelector(fileName, 4, "right"))
		.first();
	const endLine = unifiedDiffView
		.locator(getHunkLineSelector(fileName, 5, "right"))
		.first();
	await expect(startLine).toBeVisible();
	await expect(endLine).toBeVisible();

	const startBox = await startLine.boundingBox();
	const endBox = await endLine.boundingBox();
	if (!startBox || !endBox) throw new Error("missing bounding boxes");

	const startX = startBox.x + startBox.width / 2;
	const startY = startBox.y + startBox.height / 2;
	const endX = endBox.x + endBox.width / 2;
	const endY = endBox.y + endBox.height / 2;

	await page.mouse.move(startX, startY);
	await page.mouse.down();
	await page.mouse.move(endX, endY, { steps: 5 });
	await page.mouse.up();

	await waitForTestId(page, "hunk-selection-popover");
	await clickByTestId(page, "hunk-selection-popover-split");
	await waitForTestIdToNotExist(page, "hunk-selection-popover");

	// After Split the diff renders >1 hunk-header bars.
	await expect
		.poll(async () => await unifiedDiffView.locator('thead .table__title-content').count())
		.toBeGreaterThan(1);
});

test("single-click on a delta line opens the popover with a 1-row selection", async ({
	page,
	context,
}, testInfo) => {
	const workdir = testInfo.outputPath("workdir");
	const configdir = testInfo.outputPath("config");
	gitbutler = await startGitButler(workdir, configdir, context);

	const projectName = "single-click-stage-project";
	const fileName = "demo.txt";
	const projectPath = gitbutler.pathInWorkdir(projectName + "/");
	const filePath = join(projectPath, fileName);

	const baselineLines = ["alpha 1", "alpha 2", "alpha 3", "alpha 4"];
	const baseline = baselineLines.join("\n") + "\n";

	await gitbutler.runScript("project-with-remote-branches.sh");
	await gitbutler.runScript("project-with-remote-branches__commit-file-into-remote-base.sh", [
		"Seed file with baseline",
		fileName,
		baseline,
	]);
	await gitbutler.runScript("project-with-remote-branches__clone-into-new-project.sh", [
		projectName,
	]);
	await gitbutler.runScript("project-with-remote-branches__delete-project.sh", [
		"local-clone",
	]);

	await page.goto("/");
	await waitForTestId(page, "workspace-view");

	// Three modified rows so partial selection is possible.
	const modified = ["alpha 1 (modified)", "alpha 2 (modified)", "alpha 3 (modified)", "alpha 4"];
	writeFileSync(filePath, modified.join("\n") + "\n", "utf-8");

	const uncommittedChangesList = getByTestId(page, "uncommitted-changes-file-list");
	const fileItem = uncommittedChangesList
		.getByTestId("file-list-item")
		.filter({ hasText: fileName });
	await expect(fileItem).toBeVisible();
	await fileItem.click();

	const unifiedDiffView = getByTestId(page, "unified-diff-view");
	await expect(unifiedDiffView).toBeVisible();

	const gutter = unifiedDiffView
		.locator(getHunkLineSelector(fileName, 2, "right"))
		.first();
	await expect(gutter).toBeVisible();

	// Use explicit mouse.down/up so we exercise the same code path as a
	// real click (Locator.click on webkit synthesizes events that
	// triggered a stale Svelte $state-descriptors warning during gesture
	// validation; see the surrounding multi-row drag test for the same
	// approach).
	const gutterBox = await gutter.boundingBox();
	if (!gutterBox) throw new Error("missing gutter bounding box");
	const gx = gutterBox.x + gutterBox.width / 2;
	const gy = gutterBox.y + gutterBox.height / 2;
	await page.mouse.move(gx, gy);
	await page.mouse.down();
	await page.mouse.up();

	// Phase 5 (line-by-line commits): a single click on a delta line
	// opens the same selection popover that a multi-row drag opens, so
	// users can pick Stage / Comment / Split with a single gesture.
	const popover = await waitForTestId(page, "hunk-selection-popover");
	await expect(popover).toBeVisible();
	await expect(getByTestId(page, "hunk-selection-popover-stage")).toBeVisible();
	await expect(getByTestId(page, "hunk-selection-popover-split")).toBeVisible();

	// Clicking Stage from the popover toggles the line's staged state
	// and dismisses the popover.
	const targetRow = unifiedDiffView.locator(`#hunk-line-demo\\.txt\\:R2`).first();
	const stagedBefore = await targetRow.getAttribute("data-test-staged");
	await clickByTestId(page, "hunk-selection-popover-stage");
	await waitForTestIdToNotExist(page, "hunk-selection-popover");
	await expect
		.poll(async () => await targetRow.getAttribute("data-test-staged"))
		.not.toBe(stagedBefore);
});
