import { getHunkLineSelector } from "../src/hunk.ts";
import { getBaseURL, startGitButler, type GitButler } from "../src/setup.ts";
import { test } from "../src/test.ts";
import {
	clickByTestId,
	getByTestId,
	waitForTestId,
	waitForTestIdToNotExist,
} from "../src/util.ts";
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
 * Phase 7g — committed sub-hunk split happy-path.
 *
 * Validates the commit-side split / unsplit cycle wired in 7c-5:
 *   1. Make a multi-row hunk inside a commit on a fresh stack.
 *   2. Click the commit row to open its details.
 *   3. Click the file to surface the unified diff view.
 *   4. Drag-select rows in the diff → selection popover appears.
 *   5. Click `Split` → hunk fans out into N sub-hunks rendered with
 *      the un-split icon (gated by `data-testid="unsplit-sub-hunk-button"`).
 *   6. Click the un-split icon → sub-hunks merge back into a single
 *      natural hunk.
 *
 * Combined, this exercises the `tree_change_diffs_in_commit` +
 * `list_commit_override_anchors` + `split_hunk_in_commit` +
 * `unsplit_hunk_in_commit` round-trip end-to-end.
 */
test("commit-side split → unsplit round-trip via the popover gesture", async ({
	page,
	context,
}, testInfo) => {
	const workdir = testInfo.outputPath("workdir");
	const configdir = testInfo.outputPath("config");
	gitbutler = await startGitButler(workdir, configdir, context);

	const projectName = "commit-split-project";
	const fileName = "sections.md";
	const projectPath = gitbutler.pathInWorkdir(projectName + "/");
	const filePath = join(projectPath, fileName);

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

	// Make a multi-row addition: 6 added rows under one natural hunk.
	const added = [
		"section A line 1",
		"section A line 2",
		"section B line 1",
		"section B line 2",
		"section C line 1",
		"section C line 2",
	];
	writeFileSync(filePath, baseline + added.join("\n") + "\n", "utf-8");

	// Start a commit so the file lands in a fresh stack's tip.
	await clickByTestId(page, "commit-to-new-branch-button");

	const uncommittedChangesList = getByTestId(page, "uncommitted-changes-file-list");
	const fileItem = uncommittedChangesList
		.getByTestId("file-list-item")
		.filter({ hasText: fileName });
	await expect(fileItem).toBeVisible();

	// Set a commit title and submit.
	const titleInput = getByTestId(page, "commit-drawer-title-input");
	await titleInput.fill("Add three sections to sections.md");
	await clickByTestId(page, "commit-drawer-action-button");

	// Verify the commit landed.
	const commits = getByTestId(page, "commit-row");
	await expect(commits).toHaveCount(1);
	const commitRow = commits.first();

	// Click the commit row to open its details.
	await commitRow.click();
	await waitForTestId(page, "commit-drawer");

	// The commit's changed files appear inside the row's expanded
	// container. Click the file to surface the unified diff view.
	const commitFile = page
		.locator(".changed-files-container")
		.getByTestId("file-list-item")
		.filter({ hasText: fileName })
		.first();
	await expect(commitFile).toBeVisible();
	await commitFile.click();

	const unifiedDiffView = getByTestId(page, "unified-diff-view");
	await expect(unifiedDiffView).toBeVisible();

	// Single natural hunk before split.
	await expect(unifiedDiffView.locator("thead .table__title-content")).toHaveCount(1);
	// And no un-split affordance yet.
	await expect(getByTestId(page, "unsplit-sub-hunk-button")).toHaveCount(0);

	// Drag-select the middle two added rows. Right-side line numbers
	// after the baseline context (line 1) are 2..7 for the six added
	// rows; pick rows 4 and 5 (the "section B" pair).
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

	// Selection popover opens (Phase 7c-5 lifted the Phase 7 gate, so
	// `Split` is enabled in commit views).
	await waitForTestId(page, "hunk-selection-popover");
	const splitBtn = getByTestId(page, "hunk-selection-popover-split");
	await expect(splitBtn).toBeEnabled();
	await clickByTestId(page, "hunk-selection-popover-split");
	await waitForTestIdToNotExist(page, "hunk-selection-popover");

	// After split: more than one rendered hunk header, and at least one
	// un-split affordance is now visible.
	await expect
		.poll(async () => unifiedDiffView.locator("thead .table__title-content").count())
		.toBeGreaterThan(1);
	const unsplitButtons = getByTestId(page, "unsplit-sub-hunk-button");
	await expect
		.poll(async () => unsplitButtons.count())
		.toBeGreaterThanOrEqual(1);

	// Click the first un-split icon → hunks collapse back to one
	// natural hunk and the un-split affordance disappears.
	await unsplitButtons.first().click();
	await expect
		.poll(async () => unifiedDiffView.locator("thead .table__title-content").count())
		.toBe(1);
	await expect(getByTestId(page, "unsplit-sub-hunk-button")).toHaveCount(0);
});
