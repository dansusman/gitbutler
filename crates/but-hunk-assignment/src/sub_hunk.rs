//! Sub-hunk splits ("line-by-line commits"), v1 backend foundation.
//!
//! See `docs/line-by-line-commits.md` for the full design. This module
//! implements:
//! - the `RowRange` and `SubHunkOverride` types,
//! - a process-wide in-memory override store keyed by project gitdir,
//! - validation for incoming split requests,
//! - synthesis of natural-rendering `HunkHeader`s for sub-ranges,
//! - the reconcile post-pass that materializes sub-hunks as `HunkAssignment`s.
//!
//! The store is intentionally not persisted: overrides are dropped on app
//! relaunch and silently dropped when the natural hunk's shape changes
//! (anchor lookup miss).

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use anyhow::{Result, bail};
use bstr::{BString, ByteSlice};
use but_core::HunkHeader;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{HunkAssignment, HunkAssignmentTarget};

/// Serde helpers for [`SubHunkOverride::assignments`]: serialize a
/// `BTreeMap<RowRange, HunkAssignmentTarget>` as a JSON array of
/// `[range, target]` pairs. Required because JSON object keys must be
/// strings, while `RowRange` serializes as an object.
mod assignments_pairs {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::{HunkAssignmentTarget, RowRange};

    pub fn serialize<S>(
        map: &BTreeMap<RowRange, HunkAssignmentTarget>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let pairs: Vec<(&RowRange, &HunkAssignmentTarget)> = map.iter().collect();
        pairs.serialize(serializer)
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<BTreeMap<RowRange, HunkAssignmentTarget>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let pairs: Vec<(RowRange, HunkAssignmentTarget)> = Vec::deserialize(deserializer)?;
        Ok(pairs.into_iter().collect())
    }
}

/// A row range within a natural hunk's diff body.
///
/// Rows are 0-based indices into the unified-diff *body* (i.e. the `@@ ... @@`
/// header line is excluded). `start` is inclusive, `end` is exclusive; an
/// empty range has `end <= start`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RowRange {
    pub start: u32,
    pub end: u32,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(RowRange);

impl RowRange {
    pub fn len(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// The kind of a single row in a unified-diff hunk body.
///
/// Serializable so that [`SubHunkOverride`] (and its `rows` field in
/// particular) can be persisted to disk per Phase 6.5 of the
/// line-by-line commits design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RowKind {
    /// A ` ` (space-prefixed) context row.
    Context,
    /// A `+` added row.
    Add,
    /// A `-` removed row.
    Remove,
}

/// Where a sub-hunk override is anchored.
///
/// Phase 7 generalizes the override store key from `(path, anchor)` to
/// `(origin, anchor)` so that overrides on hunks inside an existing
/// **commit** can coexist with overrides on hunks in the **worktree**.
/// The keying axis is final as of 7a; only the [`Self::Worktree`]
/// variant is constructed today — 7c is the phase that actually emits
/// [`Self::Commit`]-keyed overrides via a new `split_hunk` variant
/// scoped to a `(commit_id, path, anchor)` triple.
///
/// On-disk persistence (Phase 6.5b) currently holds Worktree-keyed
/// overrides only. The `sub_hunk_overrides` schema gains a nullable
/// `commit_id` column in 7c (treated as v2 of the schema; null ≡
/// Worktree).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase", tag = "type", content = "subject")]
pub enum SubHunkOriginLocation {
    /// Override anchored to a hunk in the live worktree diff for `path`.
    Worktree { path: BString },
    /// Override anchored to a hunk in the diff of `id` against its
    /// parent, for `path`.
    Commit {
        #[serde(with = "but_serde::object_id")]
        id: gix::ObjectId,
        path: BString,
    },
}

impl Default for SubHunkOriginLocation {
    /// Empty-path worktree variant. Useful only as a serde default for
    /// [`SubHunkOverride::origin`] so backward-compat snapshots without
    /// the field deserialize cleanly; live callers should always
    /// construct via [`Self::worktree`] or [`Self::commit`].
    fn default() -> Self {
        Self::Worktree {
            path: BString::default(),
        }
    }
}

impl SubHunkOriginLocation {
    /// Sentinel default used by `#[serde(default)]` on
    /// [`SubHunkOverride::origin`]. The hydration path overwrites it
    /// with the canonical worktree-shaped origin built from
    /// `SubHunkOverride::path`.
    pub fn default_for_serde() -> Self {
        Self::default()
    }
}

impl SubHunkOriginLocation {
    /// Construct a worktree-anchored override key for `path`.
    pub fn worktree(path: BString) -> Self {
        Self::Worktree { path }
    }

    /// Construct a commit-anchored override key for `path` inside the
    /// commit `id`.
    pub fn commit(id: gix::ObjectId, path: BString) -> Self {
        Self::Commit { id, path }
    }

    /// The path the override is anchored on. Always present regardless
    /// of variant.
    pub fn path(&self) -> &BString {
        match self {
            Self::Worktree { path } | Self::Commit { path, .. } => path,
        }
    }

    /// The commit id the override is anchored to, or `None` for the
    /// worktree case.
    pub fn commit_id(&self) -> Option<gix::ObjectId> {
        match self {
            Self::Worktree { .. } => None,
            Self::Commit { id, .. } => Some(*id),
        }
    }

    /// True iff this is a worktree-anchored override.
    pub fn is_worktree(&self) -> bool {
        matches!(self, Self::Worktree { .. })
    }
}

/// In-memory provenance attached to a sub-hunk `HunkAssignment` so that the
/// commit pipeline can re-encode the sub-range using the engine-native
/// null-side encoding (see [`encode_sub_hunk_for_commit`]).
///
/// Populated by [`materialize_override`] and intentionally `#[serde(skip)]`
/// on `HunkAssignment` — sub-hunks are never persisted as such; the override
/// store is rebuilt in memory and the origin is recomputed on each reconcile.
#[derive(Debug, Clone)]
pub struct SubHunkOrigin {
    /// The natural worktree hunk this sub-hunk was carved out of.
    pub anchor: HunkHeader,
    /// The row range within the anchor's diff body that this sub-hunk
    /// represents.
    pub range: RowRange,
    /// The parsed row kinds of the *whole* anchor body. Carried so commit
    /// encoding can resolve absolute line numbers without re-parsing.
    pub rows: Vec<RowKind>,
}

/// A user-issued sub-hunk split, keyed by `(path, anchor)`.
///
/// As of phase 4.5 ("override survival across partial commits"), `ranges`
/// stores the *full* partition of the anchor — both the user-carved sub-
/// ranges and any residual gaps between them. Residual ranges that are
/// pure-context are dropped at upsert time. Storing residuals as first-
/// class state lets them survive partial commits: when the anchor's row
/// shape changes (e.g. because some rows were just committed), the
/// migration pass in [`reconcile_with_overrides`] can remap each
/// surviving range to the new natural hunk's row space.
///
/// `Serialize` / `Deserialize` are derived so that the override store
/// can be persisted to disk per Phase 6.5 of the line-by-line commits
/// design (see `docs/line-by-line-commits-plan.md`). The on-disk
/// representation is JSON-encoded into a `but-db` table; an in-memory
/// round-trip test exists in this module's test suite (Phase 6.5a).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubHunkOverride {
    /// Where this override is anchored. Phase 7c (line-by-line commits)
    /// uses this to disambiguate worktree-anchored overrides from
    /// overrides anchored to a hunk inside a specific commit's diff.
    ///
    /// Invariant: `origin.path() == &self.path`. The redundant `path`
    /// field is retained as a convenience for the many existing
    /// callers that read `ov.path` directly; a future cleanup may
    /// replace it with a `pub fn path(&self) -> &BString` accessor.
    ///
    /// `#[serde(default)]` so older on-disk / in-memory snapshots
    /// without this field deserialize as worktree-anchored. The
    /// default value is patched up post-deserialize when reading
    /// from the `sub_hunk_overrides` table; the JSON-roundtrip path
    /// always carries the field.
    #[serde(default = "SubHunkOriginLocation::default_for_serde")]
    pub origin: SubHunkOriginLocation,
    pub path: BString,
    pub anchor: HunkHeader,
    /// Sorted, disjoint, non-empty, all contained within the anchor. Covers
    /// every non-context row of the anchor at the time the override was
    /// last (re)materialized; pure-context residuals are omitted.
    pub ranges: Vec<RowRange>,
    /// Per-range stack reassignment, if any. Ranges absent from this map
    /// inherit the anchor's pre-split assignment.
    ///
    /// Serialized as a sequence of `(range, target)` pairs because JSON
    /// object keys are restricted to strings, and `RowRange` is a struct
    /// whose serde representation is an object.
    #[serde(with = "assignments_pairs")]
    pub assignments: BTreeMap<RowRange, HunkAssignmentTarget>,
    /// Cached parsed row kinds of `anchor`'s diff body. Carried so the
    /// migration pass can compute line-number fingerprints without
    /// re-parsing.
    pub rows: Vec<RowKind>,
    /// Cached anchor diff body (with `@@` header). Used by the migration
    /// pass for content-based row matching when the anchor's shape
    /// changes.
    pub anchor_diff: BString,
}

/// Parse a unified-diff body into per-row kinds.
///
/// Skips `@@` header lines and `\` "no newline" markers. Empty lines are
/// treated as context (matching the existing behavior of
/// `line_nums_from_hunk` in `lib.rs`).
pub fn parse_row_kinds(diff: &[u8]) -> Vec<RowKind> {
    diff.lines()
        .filter_map(|line| match line.first() {
            Some(b'+') => Some(RowKind::Add),
            Some(b'-') => Some(RowKind::Remove),
            Some(b'@') => None,
            Some(b'\\') => None,
            _ => Some(RowKind::Context),
        })
        .collect()
}

/// Trim leading and trailing context rows from `range`.
///
/// Returns `None` if every row in the range is context (the split would carry
/// no `+`/`-` content, which the spec rejects implicitly via "selection
/// consists only of context rows").
pub fn trim_context(range: RowRange, kinds: &[RowKind]) -> Option<RowRange> {
    let mut start = range.start as usize;
    let mut end = range.end as usize;
    while start < end && matches!(kinds.get(start), Some(RowKind::Context)) {
        start += 1;
    }
    while end > start && matches!(kinds.get(end - 1), Some(RowKind::Context)) {
        end -= 1;
    }
    if end <= start {
        None
    } else {
        Some(RowRange {
            start: start as u32,
            end: end as u32,
        })
    }
}

/// Validate `ranges` against an anchor of `row_count` rows.
///
/// Validation rules (from the spec):
/// - non-empty list,
/// - each range non-empty,
/// - each range fully within `[0, row_count)`,
/// - ranges sorted by `start`,
/// - ranges disjoint,
/// - ranges do not collectively cover the entire anchor (rule 3: "selection
///   is the entire hunk").
pub fn validate_ranges(ranges: &[RowRange], row_count: u32) -> Result<()> {
    validate_ranges_inner(ranges, row_count, /* allow_full_coverage = */ false)
}

/// Like [`validate_ranges`] but allows the ranges to cover the entire
/// anchor. Used to validate stored overrides whose `ranges` include
/// residuals.
pub fn validate_ranges_stored(ranges: &[RowRange], row_count: u32) -> Result<()> {
    validate_ranges_inner(ranges, row_count, /* allow_full_coverage = */ true)
}

fn validate_ranges_inner(
    ranges: &[RowRange],
    row_count: u32,
    allow_full_coverage: bool,
) -> Result<()> {
    if ranges.is_empty() {
        bail!("split requires at least one range");
    }
    let mut prev_end = 0u32;
    let mut total = 0u32;
    for (i, r) in ranges.iter().enumerate() {
        if r.is_empty() {
            bail!("range {i} is empty");
        }
        if r.end > row_count {
            bail!(
                "range {i} ({}, {}) exceeds anchor row count {row_count}",
                r.start,
                r.end
            );
        }
        if i > 0 && r.start < prev_end {
            bail!("range {i} overlaps or is not sorted relative to range {}", i - 1);
        }
        prev_end = r.end;
        total += r.len();
    }
    if !allow_full_coverage && total >= row_count {
        bail!("ranges cover the entire anchor; nothing to split out");
    }
    Ok(())
}

/// Materialize residuals into the user-supplied `ranges`, producing the
/// full-coverage form stored on `SubHunkOverride.ranges`.
///
/// Residual gaps that are pure-context are dropped (they would render as
/// degenerate sub-hunks with no `+`/`-` content). The result is sorted
/// and disjoint; assumes `user_ranges` already passed [`validate_ranges`].
pub fn materialize_residual_ranges(
    user_ranges: &[RowRange],
    kinds: &[RowKind],
) -> Vec<RowRange> {
    let row_count = kinds.len() as u32;
    let mut out: Vec<RowRange> = Vec::new();
    let mut cursor = 0u32;
    for r in user_ranges {
        if r.start > cursor {
            let gap = RowRange { start: cursor, end: r.start };
            if let Some(trimmed) = trim_context(gap, kinds) {
                out.push(trimmed);
            }
        }
        out.push(*r);
        cursor = r.end;
    }
    if cursor < row_count {
        let gap = RowRange { start: cursor, end: row_count };
        if let Some(trimmed) = trim_context(gap, kinds) {
            out.push(trimmed);
        }
    }
    out.sort_by_key(|r| r.start);
    out
}

/// Synthesize a natural-rendering `HunkHeader` for a sub-range of `anchor`.
///
/// This is the header used by the diff view, the SQLite assignments table,
/// and `but-hunk-dependency`. It carries narrower numeric ranges than the
/// anchor but is otherwise indistinguishable from a header produced by `git
/// diff` directly.
///
/// `kinds` is the parsed row sequence of the anchor's diff body, see
/// [`parse_row_kinds`].
pub fn synthesize_header(anchor: &HunkHeader, kinds: &[RowKind], range: RowRange) -> HunkHeader {
    let row_count = kinds.len();
    let start = (range.start as usize).min(row_count);
    let end = (range.end as usize).min(row_count);

    let mut old_offset_before = 0u32;
    let mut new_offset_before = 0u32;
    for kind in &kinds[..start] {
        match kind {
            RowKind::Context => {
                old_offset_before += 1;
                new_offset_before += 1;
            }
            RowKind::Remove => old_offset_before += 1,
            RowKind::Add => new_offset_before += 1,
        }
    }

    let mut old_lines = 0u32;
    let mut new_lines = 0u32;
    for kind in &kinds[start..end] {
        match kind {
            RowKind::Context => {
                old_lines += 1;
                new_lines += 1;
            }
            RowKind::Remove => old_lines += 1,
            RowKind::Add => new_lines += 1,
        }
    }

    HunkHeader {
        old_start: anchor.old_start + old_offset_before,
        old_lines,
        new_start: anchor.new_start + new_offset_before,
        new_lines,
    }
}

/// Slice the unified-diff body of `anchor_diff` to just the rows in `range`.
///
/// The resulting `BString` has no `@@` header and is suitable to attach to
/// the sub-hunk's `HunkAssignment.diff` field for downstream consumers.
pub fn sub_diff_body(anchor_diff: &[u8], range: RowRange) -> BString {
    let mut out: Vec<u8> = Vec::new();
    let mut row_idx: u32 = 0;
    for line in anchor_diff.lines_with_terminator() {
        let first = line.first().copied();
        let is_row = !matches!(first, Some(b'@') | Some(b'\\') | None);
        if is_row {
            if row_idx >= range.start && row_idx < range.end {
                out.extend_from_slice(line);
            }
            row_idx += 1;
        } else if matches!(first, Some(b'\\')) {
            // Trailing "no newline" markers travel with the previous row.
            if row_idx > 0 && row_idx - 1 >= range.start && row_idx - 1 < range.end {
                out.extend_from_slice(line);
            }
        }
    }
    BString::from(out)
}

/// Encode a sub-range as `HunkHeader`s in the form the commit engine
/// expects (see `but_core::tree::to_additive_hunks`): contiguous runs of
/// `+` rows become `(-0,0 +new_start,count)` headers and contiguous runs
/// of `-` rows become `(-old_start,count +0,0)` headers. Context rows
/// inside the range are skipped (they aren't being added or removed).
///
/// `rows` is the parsed row sequence of the anchor's *full* diff body
/// (see [`parse_row_kinds`]); `range` indexes into it.
///
/// For a pure-add sub-range this returns one header; for pure-remove, one;
/// for a mixed sub-range with K alternating runs, K headers.
pub fn encode_sub_hunk_for_commit(
    anchor: HunkHeader,
    range: RowRange,
    rows: &[RowKind],
) -> Vec<HunkHeader> {
    let row_count = rows.len();
    let start = (range.start as usize).min(row_count);
    let end = (range.end as usize).min(row_count);

    let mut new_line = anchor.new_start;
    let mut old_line = anchor.old_start;
    for k in &rows[..start] {
        match k {
            RowKind::Context => {
                new_line += 1;
                old_line += 1;
            }
            RowKind::Add => new_line += 1,
            RowKind::Remove => old_line += 1,
        }
    }

    let mut out = Vec::new();
    let mut i = start;
    while i < end {
        match rows[i] {
            RowKind::Add => {
                let run_start = new_line;
                let mut count = 0u32;
                while i < end && matches!(rows[i], RowKind::Add) {
                    count += 1;
                    new_line += 1;
                    i += 1;
                }
                out.push(HunkHeader {
                    old_start: 0,
                    old_lines: 0,
                    new_start: run_start,
                    new_lines: count,
                });
            }
            RowKind::Remove => {
                let run_start = old_line;
                let mut count = 0u32;
                while i < end && matches!(rows[i], RowKind::Remove) {
                    count += 1;
                    old_line += 1;
                    i += 1;
                }
                out.push(HunkHeader {
                    old_start: run_start,
                    old_lines: count,
                    new_start: 0,
                    new_lines: 0,
                });
            }
            RowKind::Context => {
                old_line += 1;
                new_line += 1;
                i += 1;
            }
        }
    }
    out
}

/// Apply a single override to a single anchor `HunkAssignment`.
///
/// Returns the materialized sub-hunk assignments. If the anchor lacks a
/// `hunk_header` or a `diff` field, the override cannot be applied and the
/// anchor is returned unchanged.
fn materialize_override(
    anchor_assignment: &HunkAssignment,
    override_: &SubHunkOverride,
) -> Vec<HunkAssignment> {
    let Some(anchor_header) = anchor_assignment.hunk_header else {
        return vec![anchor_assignment.clone()];
    };
    let Some(diff) = anchor_assignment.diff.as_ref() else {
        return vec![anchor_assignment.clone()];
    };
    let kinds = parse_row_kinds(diff);
    let row_count = kinds.len() as u32;
    if validate_ranges_stored(&override_.ranges, row_count).is_err() {
        return vec![anchor_assignment.clone()];
    }

    let anchor_branch_ref = anchor_assignment.branch_ref_bytes.clone();

    // The override's `ranges` already cover all non-context rows (residuals
    // were materialized at upsert time). Emit one sub-hunk per range.
    let emitted: Vec<(RowRange, Option<&HunkAssignmentTarget>)> = override_
        .ranges
        .iter()
        .map(|r| (*r, override_.assignments.get(r)))
        .collect();

    emitted
        .into_iter()
        .filter(|(r, _)| !r.is_empty())
        .map(|(range, override_target)| {
            let header = synthesize_header(&anchor_header, &kinds, range);
            let branch_ref_bytes = match override_target {
                Some(HunkAssignmentTarget::Branch { branch_ref_bytes }) => {
                    gix::refs::FullName::try_from(branch_ref_bytes.clone()).ok()
                }
                // Stack targets need workspace-side resolution. For the v1
                // backend foundation, fall through to the anchor's branch
                // and let the regular `assign_hunk` flow handle reassignment
                // after split.
                Some(HunkAssignmentTarget::Stack { .. }) | None => anchor_branch_ref.clone(),
            };
            HunkAssignment {
                id: Some(Uuid::new_v4()),
                hunk_header: Some(header),
                path: anchor_assignment.path.clone(),
                path_bytes: anchor_assignment.path_bytes.clone(),
                stack_id: None,
                branch_ref_bytes,
                line_nums_added: None,
                line_nums_removed: None,
                diff: Some(sub_diff_body(diff, range)),
                sub_hunk_origin: Some(SubHunkOrigin {
                    anchor: anchor_header,
                    range,
                    rows: kinds.clone(),
                }),
            }
        })
        .collect()
}

/// Outcome of applying a single override during reconcile.
#[derive(Debug, Clone)]
pub enum OverrideOutcome {
    /// Anchor matched exactly; override unchanged.
    KeepAsIs,
    /// Anchor's natural shape changed but the override migrated to one or
    /// more new anchors + remapped ranges. The store should drop the old
    /// key and insert each migrated entry.
    ///
    /// A list (rather than a single value) covers the common case where a
    /// long pure-add hunk gets partially committed and the post-commit
    /// natural diff splits into multiple hunks separated by context lines:
    /// each surviving residual maps onto a different natural hunk.
    Migrated(Vec<SubHunkOverride>),
    /// Override could not be matched or migrated; drop from the store.
    Drop,
}

/// Apply all `overrides` to `assignments` in place, replacing each matched
/// anchor with its materialized sub-hunks.
///
/// Returns one [`OverrideOutcome`] per input override (positionally aligned).
/// The caller is responsible for updating the in-memory store accordingly
/// (see [`reconcile_with_overrides`]).
pub fn apply_overrides_to_assignments(
    assignments: &mut Vec<HunkAssignment>,
    overrides: &[SubHunkOverride],
) -> Vec<OverrideOutcome> {
    let mut outcomes = Vec::with_capacity(overrides.len());
    for ov in overrides {
        // Fast path: exact (path, anchor) match.
        if let Some(i) = assignments
            .iter()
            .position(|a| a.path_bytes == ov.path && a.hunk_header == Some(ov.anchor))
        {
            let anchor = assignments[i].clone();
            let sub_hunks = materialize_override(&anchor, ov);
            tracing::info!(
                target: "sub_hunk",
                path = %ov.path,
                anchor = ?ov.anchor,
                ranges = ?ov.ranges,
                emitted = sub_hunks.len(),
                "override matched anchor exactly; emitted sub-hunks",
            );
            assignments.splice(i..=i, sub_hunks);
            outcomes.push(OverrideOutcome::KeepAsIs);
            continue;
        }

        // Slow path: try to migrate the override across one or more
        // candidate natural hunks on the same path. After a partial
        // commit, the natural diff often splits into multiple hunks
        // separated by newly-context rows; each surviving residual maps
        // onto whichever candidate still contains its content.
        let migrated = migrate_override_multi(ov, assignments);
        if migrated.is_empty() {
            tracing::info!(
                target: "sub_hunk",
                path = %ov.path,
                anchor = ?ov.anchor,
                ranges = ?ov.ranges,
                natural_hunks_on_path = assignments
                    .iter()
                    .filter(|a| a.path_bytes == ov.path && a.sub_hunk_origin.is_none())
                    .filter_map(|a| a.hunk_header)
                    .count(),
                "override could not be matched or migrated; dropping",
            );
            outcomes.push(OverrideOutcome::Drop);
            continue;
        }

        // Apply migrated overrides in *descending* index order so each
        // splice does not invalidate the indices of the remaining ones.
        let mut migrated_sorted = migrated.clone();
        migrated_sorted.sort_by(|a, b| b.0.cmp(&a.0));
        for (idx, m) in &migrated_sorted {
            let anchor = assignments[*idx].clone();
            let sub_hunks = materialize_override(&anchor, m);
            tracing::info!(
                target: "sub_hunk",
                path = %ov.path,
                old_anchor = ?ov.anchor,
                new_anchor = ?m.anchor,
                new_ranges = ?m.ranges,
                emitted = sub_hunks.len(),
                "override migrated to natural hunk",
            );
            assignments.splice(*idx..=*idx, sub_hunks);
        }
        outcomes.push(OverrideOutcome::Migrated(
            migrated.into_iter().map(|(_, m)| m).collect(),
        ));
    }
    outcomes
}

/// Compute, for each row in `rows`, the full line content (the row's text
/// without its `+`/`-`/space prefix and without its trailing newline). Used
/// for content-based row matching during migration.
fn row_contents(diff: &[u8]) -> Vec<BString> {
    let mut out = Vec::new();
    for line in diff.lines() {
        match line.first() {
            Some(b'@') | Some(b'\\') => continue,
            Some(_) => out.push(BString::from(&line[1..])),
            None => out.push(BString::from("")),
        }
    }
    out
}

/// Compute the new-side line span covered by all `ranges` in an anchor.
/// Returns `(lo, hi)` as half-open `[lo, hi)` over the new-side file lines.
/// `lo == hi` means the override has no new-side rows (pure-remove). In
/// that case we use the anchor's new_start as both bounds.
fn override_new_side_span(
    anchor: &HunkHeader,
    rows: &[RowKind],
    ranges: &[RowRange],
) -> (u32, u32) {
    let mut lo = u32::MAX;
    let mut hi = 0u32;
    let mut new_line = anchor.new_start;
    for (idx, k) in rows.iter().enumerate() {
        let in_range = ranges.iter().any(|r| (r.start as usize) <= idx && idx < (r.end as usize));
        let advances_new = matches!(k, RowKind::Add | RowKind::Context);
        if in_range && advances_new {
            lo = lo.min(new_line);
            hi = hi.max(new_line + 1);
        }
        if advances_new {
            new_line += 1;
        }
    }
    if lo == u32::MAX {
        // Pure-remove override; pin to the anchor's insertion point.
        (anchor.new_start, anchor.new_start)
    } else {
        (lo, hi)
    }
}

/// Try to find natural-hunk assignment(s) in `assignments` that the
/// override has migrated to (because the anchor's row shape changed, e.g.
/// after a partial commit).
///
/// Returns one `(index, override)` pair per surviving natural hunk the
/// override now spans. An empty result means no surviving range could be
/// remapped onto any candidate — the override should be dropped.
fn migrate_override_multi(
    ov: &SubHunkOverride,
    assignments: &[HunkAssignment],
) -> Vec<(usize, SubHunkOverride)> {
    let old_contents = row_contents(ov.anchor_diff.as_ref());
    if old_contents.len() != ov.rows.len() {
        tracing::info!(
            target: "sub_hunk",
            path = %ov.path,
            old_contents = old_contents.len(),
            old_rows = ov.rows.len(),
            "migration aborted: cached anchor diff doesn't parse to the same row count as cached rows",
        );
        return Vec::new();
    }

    // Gather every natural-hunk candidate on the same path along with its
    // parsed rows + content. Same-path filter mirrors the single-candidate
    // path; we no longer require any specific number of overlapping
    // candidates, because per-range remapping decides which candidates
    // actually receive content.
    struct Candidate {
        idx: usize,
        anchor: HunkHeader,
        diff: BString,
        rows: Vec<RowKind>,
        contents: Vec<BString>,
    }
    let mut candidates: Vec<Candidate> = assignments
        .iter()
        .enumerate()
        .filter(|(_, a)| a.path_bytes == ov.path && a.sub_hunk_origin.is_none())
        .filter_map(|(idx, a)| {
            let anchor = a.hunk_header?;
            let diff = a.diff.as_ref()?.clone();
            let rows = parse_row_kinds(diff.as_ref());
            let contents = row_contents(diff.as_ref());
            if contents.len() != rows.len() {
                return None;
            }
            Some(Candidate { idx, anchor, diff, rows, contents })
        })
        .collect();

    if candidates.is_empty() {
        tracing::info!(
            target: "sub_hunk",
            path = %ov.path,
            "migration: no natural-hunk candidates on path",
        );
        return Vec::new();
    }

    // Sort candidates by `new_start` so a flattened concatenation of their
    // rows respects the worktree's actual line order. This is essential
    // for the order-preserving alignment below to land each old row on
    // its correct counterpart, especially when content is repeated
    // (e.g. multiple blank lines).
    candidates.sort_by_key(|c| c.anchor.new_start);

    tracing::info!(
        target: "sub_hunk",
        path = %ov.path,
        candidates = candidates.len(),
        old_rows = ov.rows.len(),
        old_contents = old_contents.len(),
        ranges = ?ov.ranges,
        "migration: starting global alignment",
    );

    // Build a flat (candidate_pos, local_idx, kind, content) stream over
    // every candidate's rows in worktree order. `candidate_pos` indexes
    // into the sorted `candidates` slice.
    let flat: Vec<(usize, usize, RowKind, &BString)> = candidates
        .iter()
        .enumerate()
        .flat_map(|(cp, c)| {
            c.rows
                .iter()
                .zip(c.contents.iter())
                .enumerate()
                .map(move |(li, (k, content))| (cp, li, *k, content))
        })
        .collect();

    // Order-preserving sequence alignment: for each non-context old row,
    // find a (kind, content) match in `flat` at or after the running
    // cursor. This guarantees each new row is consumed at most once and
    // duplicate-content rows (blanks, repeated boilerplate) line up in
    // worktree order instead of all collapsing onto the first match.
    let mut alignment: Vec<Option<(usize, usize)>> = vec![None; ov.rows.len()];
    let mut cursor = 0usize;
    for (i, k) in ov.rows.iter().enumerate() {
        if matches!(k, RowKind::Context) {
            continue;
        }
        let key_content = &old_contents[i];
        let found = (cursor..flat.len()).find(|j| {
            let entry = &flat[*j];
            entry.2 == *k && entry.3 == key_content
        });
        if let Some(j) = found {
            let (cp, li, _, _) = flat[j];
            alignment[i] = Some((cp, li));
            cursor = j + 1;
        }
    }

    // Materialize per-range, per-candidate new ranges from the alignment.
    // For each old range we collect the (cp, li) pairs of matched rows,
    // group by candidate, and take min/max of `li` per group.
    let mut by_candidate: BTreeMap<usize, Vec<(RowRange, Option<HunkAssignmentTarget>)>> =
        BTreeMap::new();
    for old_range in &ov.ranges {
        let start = (old_range.start as usize).min(alignment.len());
        let end = (old_range.end as usize).min(alignment.len());
        // Bucket matches by candidate position.
        let mut buckets: BTreeMap<usize, (usize, usize)> = BTreeMap::new();
        for i in start..end {
            if let Some((cp, li)) = alignment[i] {
                buckets
                    .entry(cp)
                    .and_modify(|(lo, hi)| {
                        *lo = (*lo).min(li);
                        *hi = (*hi).max(li);
                    })
                    .or_insert((li, li));
            }
        }
        if buckets.is_empty() {
            tracing::info!(
                target: "sub_hunk",
                path = %ov.path,
                old_range = ?old_range,
                "migration: range dropped (no rows survived)",
            );
            continue;
        }
        let target = ov.assignments.get(old_range).cloned();
        for (cp, (lo, hi)) in buckets {
            let c = &candidates[cp];
            let raw = RowRange { start: lo as u32, end: hi as u32 + 1 };
            let Some(trimmed) = trim_context(raw, &c.rows) else {
                continue;
            };
            tracing::info!(
                target: "sub_hunk",
                path = %ov.path,
                old_range = ?old_range,
                new_range = ?trimmed,
                candidate_anchor = ?c.anchor,
                "migration: range remapped via global alignment",
            );
            by_candidate
                .entry(c.idx)
                .or_default()
                .push((trimmed, target.clone()));
        }
    }

    tracing::info!(
        target: "sub_hunk",
        path = %ov.path,
        candidate_groups = by_candidate.len(),
        "migration: range remap complete",
    );

    let mut out: Vec<(usize, SubHunkOverride)> = Vec::new();
    for (idx, entries) in by_candidate {
        let c = candidates.iter().find(|c| c.idx == idx).expect(
            "by_candidate keys are drawn from candidates above",
        );
        // Coalesce entries that overlap on the same candidate (can happen
        // when adjacent old ranges map onto adjacent new rows): merging
        // them keeps stored ranges sorted, disjoint, and contiguous.
        let mut entries = entries;
        entries.sort_by_key(|(r, _)| r.start);
        let mut user_ranges: Vec<RowRange> = entries.iter().map(|(r, _)| *r).collect();
        user_ranges.sort_by_key(|r| r.start);
        user_ranges.dedup();

        // Add residual ranges for non-context rows in the new candidate
        // anchor that aren't covered by any of the migrated user ranges.
        // Without this step, content that reappeared in the worktree
        // (e.g. when the user uncommits a partial commit) is left as
        // "uncovered" rows and silently hidden from the diff view by
        // `materialize_override` — producing the bug where a 3-section
        // file post-uncommit displayed only one section's worth of rows.
        //
        // `materialize_residual_ranges` expects sorted, disjoint user
        // ranges and a `kinds` slice of the new anchor's body; it fills
        // gaps with trimmed-context residuals.
        let ranges = materialize_residual_ranges(&user_ranges, &c.rows);
        if validate_ranges_stored(&ranges, c.rows.len() as u32).is_err() {
            continue;
        }
        let mut assignments_map: BTreeMap<RowRange, HunkAssignmentTarget> = BTreeMap::new();
        for (r, t) in entries {
            if let Some(target) = t {
                assignments_map.insert(r, target);
            }
        }
        tracing::info!(
            target: "sub_hunk",
            path = %ov.path,
            candidate_anchor = ?c.anchor,
            user_ranges = ?user_ranges,
            full_ranges = ?ranges,
            "migration: residuals re-materialized for new anchor",
        );
        out.push((
            idx,
            SubHunkOverride {
                origin: ov.origin.clone(),
                path: ov.path.clone(),
                anchor: c.anchor,
                ranges,
                assignments: assignments_map,
                rows: c.rows.clone(),
                anchor_diff: c.diff.clone(),
            },
        ));
    }
    out
}

/// Legacy single-candidate migration kept for backward compatibility with
/// the older test surface; new code should prefer `migrate_override_multi`.
#[allow(dead_code)]
fn migrate_override(
    ov: &SubHunkOverride,
    assignments: &[HunkAssignment],
) -> Option<(usize, SubHunkOverride)> {
    let (ov_lo, ov_hi) = override_new_side_span(&ov.anchor, &ov.rows, &ov.ranges);

    // Candidate natural hunks on the same path whose new-side range overlaps
    // the override's new-side span.
    let candidates: Vec<usize> = assignments
        .iter()
        .enumerate()
        .filter(|(_, a)| a.path_bytes == ov.path && a.sub_hunk_origin.is_none())
        .filter(|(_, a)| {
            let Some(h) = a.hunk_header else { return false };
            let lo = h.new_start;
            let hi = h.new_start + h.new_lines;
            // Empty (insertion-point) ranges still match if equal endpoints.
            if ov_lo == ov_hi {
                lo <= ov_lo && ov_lo <= hi
            } else {
                lo < ov_hi && ov_lo < hi
            }
        })
        .map(|(i, _)| i)
        .collect();

    // Multi-candidate ambiguity: drop the override.
    if candidates.len() != 1 {
        tracing::info!(
            target: "sub_hunk",
            path = %ov.path,
            ov_new_span = ?(ov_lo, ov_hi),
            candidate_count = candidates.len(),
            "migration aborted: not exactly one overlapping natural hunk on path",
        );
        return None;
    }
    let i = candidates[0];
    let new_anchor_assignment = &assignments[i];
    let new_anchor = new_anchor_assignment.hunk_header?;
    let new_diff = new_anchor_assignment.diff.as_ref()?;
    let new_rows = parse_row_kinds(new_diff.as_ref());

    // Build content tables for both sides.
    let old_contents = row_contents(ov.anchor_diff.as_ref());
    let new_contents = row_contents(new_diff.as_ref());
    if old_contents.len() != ov.rows.len() || new_contents.len() != new_rows.len() {
        tracing::info!(
            target: "sub_hunk",
            path = %ov.path,
            old_contents = old_contents.len(),
            old_rows = ov.rows.len(),
            new_contents = new_contents.len(),
            new_rows = new_rows.len(),
            "migration aborted: row-count mismatch between cached anchor diff and parsed rows",
        );
        return None;
    }

    // For each old range, remap to a new range. Drop ranges that no longer
    // contain any non-context rows in the new anchor.
    let mut new_ranges: Vec<RowRange> = Vec::new();
    let mut new_assignments: BTreeMap<RowRange, HunkAssignmentTarget> = BTreeMap::new();
    for old_range in &ov.ranges {
        let Some(new_range) = remap_range(
            *old_range,
            &ov.rows,
            &old_contents,
            &new_rows,
            &new_contents,
        ) else {
            continue;
        };
        if let Some(target) = ov.assignments.get(old_range) {
            new_assignments.insert(new_range, target.clone());
        }
        new_ranges.push(new_range);
    }
    if new_ranges.is_empty() {
        return None;
    }
    new_ranges.sort_by_key(|r| r.start);
    // Dedupe in case multiple old ranges remapped onto the same new range.
    new_ranges.dedup();

    // Sanity: stored overrides must satisfy validate_ranges_stored.
    if validate_ranges_stored(&new_ranges, new_rows.len() as u32).is_err() {
        return None;
    }

    Some((
        i,
        SubHunkOverride {
            origin: ov.origin.clone(),
            path: ov.path.clone(),
            anchor: new_anchor,
            ranges: new_ranges,
            assignments: new_assignments,
            rows: new_rows,
            anchor_diff: new_diff.clone(),
        },
    ))
}

/// Find the smallest contiguous row range in `(new_rows, new_contents)`
/// that contains every non-context row from `old_range` in `(old_rows,
/// old_contents)`, matched by `(kind, content)`. Returns `None` if no
/// non-context row from the old range survives.
fn remap_range(
    old_range: RowRange,
    old_rows: &[RowKind],
    old_contents: &[BString],
    new_rows: &[RowKind],
    new_contents: &[BString],
) -> Option<RowRange> {
    let start = (old_range.start as usize).min(old_rows.len());
    let end = (old_range.end as usize).min(old_rows.len());

    let mut min_idx: Option<usize> = None;
    let mut max_idx: Option<usize> = None;
    for i in start..end {
        if matches!(old_rows[i], RowKind::Context) {
            continue;
        }
        let key_kind = old_rows[i];
        let key_content = &old_contents[i];
        // Find any new row with matching (kind, content). We greedily take
        // the first match — content collisions inside a single hunk are rare
        // for typical code edits.
        let found = new_rows
            .iter()
            .enumerate()
            .find(|(j, k)| **k == key_kind && new_contents[*j] == *key_content);
        if let Some((j, _)) = found {
            min_idx = Some(min_idx.map_or(j, |m| m.min(j)));
            max_idx = Some(max_idx.map_or(j, |m| m.max(j)));
        }
    }
    let (lo, hi) = match (min_idx, max_idx) {
        (Some(a), Some(b)) => (a as u32, b as u32 + 1),
        _ => return None,
    };
    trim_context(RowRange { start: lo, end: hi }, new_rows)
}

// ---------------------------------------------------------------------------
// Process-wide in-memory store
// ---------------------------------------------------------------------------

/// Process-wide store key. Phase 7a widened this from
/// `(BString, HunkHeader)` to `(SubHunkOriginLocation, HunkHeader)` so
/// commit-anchored overrides (Phase 7c) can coexist with worktree-
/// anchored ones. Today only the `Worktree` variant is ever
/// constructed.
type StoreKey = (SubHunkOriginLocation, HunkHeader);
type ProjectStore = HashMap<StoreKey, SubHunkOverride>;

static GLOBAL_STORE: OnceLock<Mutex<HashMap<PathBuf, ProjectStore>>> = OnceLock::new();

fn global_store() -> &'static Mutex<HashMap<PathBuf, ProjectStore>> {
    GLOBAL_STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Build the worktree-flavored store key for `(path, anchor)`. All
/// existing `path`/`anchor`-shaped public APIs route through this
/// helper so Phase 7a is a no-op on the worktree side.
fn worktree_key(path: &BString, anchor: HunkHeader) -> StoreKey {
    (SubHunkOriginLocation::worktree(path.clone()), anchor)
}

/// Build the store key for `ov`. Phase 7c reads `ov.origin` directly
/// (the field is now authoritative); the older worktree-only shim
/// from 7a/7b has been retired.
fn key_for_override(ov: &SubHunkOverride) -> StoreKey {
    (ov.origin.clone(), ov.anchor)
}

/// List **all** overrides for the project at `gitdir`, regardless of
/// origin. Mostly useful for diagnostics; reconcile passes should use
/// the origin-filtered variants below to avoid mixing worktree- and
/// commit-anchored overrides.
pub fn list_overrides(gitdir: &Path) -> Vec<SubHunkOverride> {
    list_overrides_filtered(gitdir, |_| true)
}

/// List only **worktree-anchored** overrides for `gitdir`.
///
/// This is what [`reconcile_with_overrides`] uses; commit-anchored
/// overrides (Phase 7c) must not be subjected to a worktree-shape
/// reconcile because they reference hunks inside a specific commit's
/// diff and would be incorrectly migrated or dropped against the
/// live worktree.
pub fn list_worktree_overrides(gitdir: &Path) -> Vec<SubHunkOverride> {
    list_overrides_filtered(gitdir, |loc| loc.is_worktree())
}

/// List only **commit-anchored** overrides for `gitdir` whose
/// `SubHunkOriginLocation::Commit { id, .. }` matches `commit_id`.
///
/// Phase 7c uses this to apply per-commit override rendering when the
/// desktop fetches a commit's diff. Until then, returns an empty
/// list because no public API constructs `Commit`-keyed overrides.
pub fn list_commit_overrides(gitdir: &Path, commit_id: gix::ObjectId) -> Vec<SubHunkOverride> {
    list_overrides_filtered(gitdir, |loc| loc.commit_id() == Some(commit_id))
}

fn list_overrides_filtered<F>(gitdir: &Path, mut keep: F) -> Vec<SubHunkOverride>
where
    F: FnMut(&SubHunkOriginLocation) -> bool,
{
    let store = global_store().lock().expect("override store poisoned");
    store
        .get(gitdir)
        .map(|p| {
            p.iter()
                .filter(|((loc, _anchor), _)| keep(loc))
                .map(|(_, ov)| ov.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Insert or replace an override. Reads the override's own
/// [`SubHunkOverride::origin`] field as the authoritative store key.
pub fn upsert_override(gitdir: &Path, ov: SubHunkOverride) {
    let key = key_for_override(&ov);
    let mut store = global_store().lock().expect("override store poisoned");
    let project = store.entry(gitdir.to_path_buf()).or_default();
    project.insert(key, ov);
}

/// Insert or replace an override at an explicit
/// [`SubHunkOriginLocation`], overriding `ov.origin` for storage.
/// `location` is authoritative for both the store key and the stored
/// `ov.origin` field, so callers can hand in an `ov` constructed
/// without yet knowing its origin (e.g. migration paths that build a
/// `SubHunkOverride` from on-disk pieces and only know the location
/// at insert time).
///
/// Phase 7c's commit-side `split_hunk_in_commit` RPC enters here
/// with a `Commit { id, path }`-shaped location; existing worktree
/// callers reach the in-memory store through [`upsert_override`].
pub fn upsert_override_at(
    gitdir: &Path,
    location: SubHunkOriginLocation,
    mut ov: SubHunkOverride,
) {
    debug_assert_eq!(
        location.path(),
        &ov.path,
        "SubHunkOriginLocation.path must agree with SubHunkOverride.path"
    );
    ov.origin = location.clone();
    let anchor = ov.anchor;
    let mut store = global_store().lock().expect("override store poisoned");
    let project = store.entry(gitdir.to_path_buf()).or_default();
    project.insert((location, anchor), ov);
}

/// Look up a single worktree-anchored override by
/// `(gitdir, path, anchor)`. Returns a clone so callers can read
/// without holding the store lock.
///
/// For commit-anchored lookups, see [`get_commit_override`].
pub fn get_override(
    gitdir: &Path,
    path: &BString,
    anchor: HunkHeader,
) -> Option<SubHunkOverride> {
    let store = global_store().lock().expect("override store poisoned");
    store.get(gitdir)?.get(&worktree_key(path, anchor)).cloned()
}

/// Look up a single commit-anchored override by
/// `(gitdir, commit_id, path, anchor)`. Returns a clone so callers
/// can read without holding the store lock.
///
/// Returns `None` for any input today because Phase 7b doesn't yet
/// construct `Commit`-keyed overrides; Phase 7c populates them via
/// the commit-side `split_hunk` RPC.
pub fn get_commit_override(
    gitdir: &Path,
    commit_id: gix::ObjectId,
    path: &BString,
    anchor: HunkHeader,
) -> Option<SubHunkOverride> {
    let store = global_store().lock().expect("override store poisoned");
    let key = (
        SubHunkOriginLocation::commit(commit_id, path.clone()),
        anchor,
    );
    store.get(gitdir)?.get(&key).cloned()
}

/// Insert one or more user-selected ranges into an existing override's
/// `ranges` partition, splitting any existing range that contains a new
/// range at its boundaries. Used by `split_hunk` to support re-splitting
/// an already-split sub-hunk: rather than replacing the override (which
/// would lose the user's earlier splits), we refine the partition.
///
/// Inputs:
/// - `existing`: the override's current full-coverage `ranges` slice
///   (sorted, disjoint, including residuals).
/// - `new_user_ranges`: sorted, disjoint, non-empty user-selected ranges,
///   each entirely contained within `[0, row_count)` and within the
///   coverage of `existing`. Caller is responsible for context-trimming
///   them first.
///
/// Returns a new partition `ranges` with the same coverage as `existing`
/// but with each `new_user_range` materialized as its own boundary.
/// Output is sorted, disjoint, non-empty.
pub fn merge_user_ranges_into_partition(
    existing: &[RowRange],
    new_user_ranges: &[RowRange],
) -> Vec<RowRange> {
    if new_user_ranges.is_empty() {
        return existing.to_vec();
    }
    let mut breakpoints: Vec<u32> = Vec::new();
    for r in existing {
        breakpoints.push(r.start);
        breakpoints.push(r.end);
    }
    for r in new_user_ranges {
        breakpoints.push(r.start);
        breakpoints.push(r.end);
    }
    breakpoints.sort();
    breakpoints.dedup();

    // Build segments between breakpoints; keep only those that fall
    // inside an existing range (preserving original coverage).
    let mut out: Vec<RowRange> = Vec::new();
    for w in breakpoints.windows(2) {
        let seg = RowRange { start: w[0], end: w[1] };
        if seg.is_empty() {
            continue;
        }
        let inside_existing = existing
            .iter()
            .any(|r| r.start <= seg.start && seg.end <= r.end);
        if inside_existing {
            out.push(seg);
        }
    }
    out
}

/// Remove an override by `(path, anchor)`. Returns the removed override, if any.
///
/// Worktree-anchored only; commit-anchored overrides require a
/// `SubHunkOriginLocation`-shaped variant introduced in Phase 7c.
pub fn remove_override(
    gitdir: &Path,
    path: &BString,
    anchor: HunkHeader,
) -> Option<SubHunkOverride> {
    let mut store = global_store().lock().expect("override store poisoned");
    let project = store.get_mut(gitdir)?;
    project.remove(&worktree_key(path, anchor))
}

/// Drop overrides whose `(path, anchor)` are listed in `keys`.
///
/// Used by [`reconcile_with_overrides`] to silently drop overrides whose
/// anchor no longer matches a natural hunk in the worktree.
///
/// Worktree-anchored only — the `BString`-shaped key is implicitly
/// `SubHunkOriginLocation::Worktree`.
pub fn drop_overrides(gitdir: &Path, keys: &[(BString, HunkHeader)]) {
    if keys.is_empty() {
        return;
    }
    let mut store = global_store().lock().expect("override store poisoned");
    if let Some(project) = store.get_mut(gitdir) {
        for (path, anchor) in keys {
            project.remove(&worktree_key(path, *anchor));
        }
    }
}

/// Migrate an override in the store from its old `(path, old_anchor)` key
/// to the new `(path, migrated.anchor)` key, replacing the stored value
/// with `migrated`.
pub fn migrate_stored_override(
    gitdir: &Path,
    path: &BString,
    old_anchor: HunkHeader,
    migrated: SubHunkOverride,
) {
    migrate_stored_override_multi(gitdir, path, old_anchor, vec![migrated]);
}

/// Like [`migrate_stored_override`] but inserts multiple migrated entries.
/// Used when a partial commit splits the natural diff into more than one
/// hunk and the override's surviving residuals fan out across them.
pub fn migrate_stored_override_multi(
    gitdir: &Path,
    path: &BString,
    old_anchor: HunkHeader,
    migrated: Vec<SubHunkOverride>,
) {
    let mut store = global_store().lock().expect("override store poisoned");
    let Some(project) = store.get_mut(gitdir) else { return };
    project.remove(&worktree_key(path, old_anchor));
    for m in migrated {
        let key = key_for_override(&m);
        project.insert(key, m);
    }
}

#[cfg(test)]
pub(crate) fn clear_store_for_tests() {
    clear_store_for_tests_at(None);
}

#[cfg(test)]
pub(crate) fn clear_store_for_tests_at(gitdir: Option<&Path>) {
    let mut store = global_store().lock().expect("override store poisoned");
    match gitdir {
        Some(g) => {
            store.remove(g);
        }
        None => store.clear(),
    }
}

/// Apply the current process-wide overrides for `gitdir` to `assignments`,
/// pruning any overrides whose anchor no longer matches.
///
/// This is the post-pass that the reconcile flow in `lib.rs` calls after
/// translating natural worktree hunks into `HunkAssignment`s and before
/// reconciling against persisted SQLite state.
pub fn reconcile_with_overrides(gitdir: &Path, assignments: &mut Vec<HunkAssignment>) {
    // Phase 7b: only worktree-anchored overrides are subject to the
    // worktree-shape reconcile. Commit-anchored overrides (Phase 7c)
    // reference hunks inside a specific commit's diff and would be
    // incorrectly migrated or dropped against `assignments` (which
    // are exclusively worktree-derived).
    let overrides = list_worktree_overrides(gitdir);
    if overrides.is_empty() {
        return;
    }
    let outcomes = apply_overrides_to_assignments(assignments, &overrides);
    let mut to_drop: Vec<(BString, HunkHeader)> = Vec::new();
    for (ov, outcome) in overrides.iter().zip(outcomes.into_iter()) {
        match outcome {
            OverrideOutcome::KeepAsIs => {}
            OverrideOutcome::Drop => to_drop.push((ov.path.clone(), ov.anchor)),
            OverrideOutcome::Migrated(migrated_list) => {
                migrate_stored_override_multi(
                    gitdir,
                    &ov.path,
                    ov.anchor,
                    migrated_list,
                );
            }
        }
    }
    drop_overrides(gitdir, &to_drop);
}

// ---------------------------------------------------------------------------
// Phase 6.5c–e — disk persistence of overrides via `but-db`
//
// The on-disk row shape lives in `but_db::SubHunkOverrideRow`; that crate
// intentionally treats the JSON columns as opaque strings. The bridge
// (`to_db_row` / `from_db_row`), size guard, hydration, and write-through
// helpers all live here so that `but-db` doesn't acquire a reverse
// dependency on `but-hunk-assignment`.
// ---------------------------------------------------------------------------

/// Maximum on-disk size for a single override, summed across the
/// `anchor_diff` blob and the JSON-encoded `rows` column. Hunks bigger
/// than this are not rendered today anyway; refusing to persist them
/// keeps DB rows bounded.
pub const MAX_OVERRIDE_DB_BYTES: usize = 64 * 1024;

/// Current on-disk schema version stamped into
/// `sub_hunk_overrides.schema_version`. Bumped to `2` in Phase 7c-2
/// when the table gained a `commit_id BLOB` column in the primary
/// key. Rows written at v1 carry an empty-blob `commit_id` (NULL
/// equivalent) and are interpreted as worktree-anchored on read.
pub const OVERRIDE_DB_SCHEMA_VERSION: u32 = 2;

/// Convert an in-memory `SubHunkOverride` into the on-disk row shape.
///
/// Returns `Ok(None)` (and emits a warning) if the override exceeds
/// [`MAX_OVERRIDE_DB_BYTES`]; callers should treat that as "silently drop
/// from disk, the in-memory entry is still fine for the current session".
pub fn to_db_row(
    gitdir: &Path,
    ov: &SubHunkOverride,
) -> Result<Option<but_db::SubHunkOverrideRow>> {
    let ranges_json = serde_json::to_string(&ov.ranges)?;
    let rows_json = serde_json::to_string(&ov.rows)?;
    // Mirror the `assignments_pairs` serde helper used on the in-memory
    // struct: emit a JSON array of `[range, target]` pairs, since JSON
    // object keys must be strings and `RowRange` serializes as an object.
    let pairs: Vec<(&RowRange, &HunkAssignmentTarget)> = ov.assignments.iter().collect();
    let assignments_json = serde_json::to_string(&pairs)?;

    let total = ov.anchor_diff.len() + rows_json.len();
    if total > MAX_OVERRIDE_DB_BYTES {
        tracing::warn!(
            path = %ov.path,
            total_bytes = total,
            max = MAX_OVERRIDE_DB_BYTES,
            "sub_hunk override exceeds size guard; refusing to persist"
        );
        return Ok(None);
    }

    // Phase 7c-2: encode the override's origin into `commit_id`.
    // Worktree-anchored → empty blob; commit-anchored → the commit
    // OID's raw 20-byte (sha1) / 32-byte (sha256) representation.
    let commit_id = match &ov.origin {
        SubHunkOriginLocation::Worktree { .. } => Vec::new(),
        SubHunkOriginLocation::Commit { id, .. } => id.as_bytes().to_vec(),
    };

    Ok(Some(but_db::SubHunkOverrideRow {
        gitdir: gitdir.to_string_lossy().into_owned(),
        path: ov.path.to_vec(),
        anchor_old_start: ov.anchor.old_start,
        anchor_old_lines: ov.anchor.old_lines,
        anchor_new_start: ov.anchor.new_start,
        anchor_new_lines: ov.anchor.new_lines,
        commit_id,
        ranges_json,
        assignments_json,
        rows_json,
        anchor_diff: ov.anchor_diff.to_vec(),
        schema_version: OVERRIDE_DB_SCHEMA_VERSION,
    }))
}

/// Convert an on-disk row back into an in-memory `SubHunkOverride`.
pub fn from_db_row(row: but_db::SubHunkOverrideRow) -> Result<SubHunkOverride> {
    if row.schema_version != OVERRIDE_DB_SCHEMA_VERSION {
        bail!(
            "sub_hunk_overrides row has schema_version={}, this binary supports {}",
            row.schema_version,
            OVERRIDE_DB_SCHEMA_VERSION,
        );
    }

    let ranges: Vec<RowRange> = serde_json::from_str(&row.ranges_json)?;
    let rows: Vec<RowKind> = serde_json::from_str(&row.rows_json)?;

    let pairs: Vec<(RowRange, HunkAssignmentTarget)> =
        serde_json::from_str(&row.assignments_json)?;
    let assignments: BTreeMap<RowRange, HunkAssignmentTarget> = pairs.into_iter().collect();

    // Phase 7c-2: decode origin from `row.commit_id`. Empty blob
    // → worktree-anchored. Non-empty → parse as `gix::ObjectId`
    // (handles both sha1-20 and sha256-32 byte widths).
    let path = BString::from(row.path);
    let origin = if row.commit_id.is_empty() {
        SubHunkOriginLocation::worktree(path.clone())
    } else {
        let id = gix::ObjectId::try_from(row.commit_id.as_slice())
            .map_err(|err| anyhow::anyhow!("sub_hunk_overrides.commit_id is not a valid object id: {err}"))?;
        SubHunkOriginLocation::commit(id, path.clone())
    };
    Ok(SubHunkOverride {
        origin,
        path,
        anchor: HunkHeader {
            old_start: row.anchor_old_start,
            old_lines: row.anchor_old_lines,
            new_start: row.anchor_new_start,
            new_lines: row.anchor_new_lines,
        },
        ranges,
        assignments,
        rows,
        anchor_diff: BString::from(row.anchor_diff),
    })
}

fn gitdir_key(gitdir: &Path) -> String {
    gitdir.to_string_lossy().into_owned()
}

/// Tracks which `gitdir`s have already been hydrated from disk in this
/// process, so we never replay the load twice.
static HYDRATED_GITDIRS: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();

fn hydrated_gitdirs() -> &'static Mutex<std::collections::HashSet<PathBuf>> {
    HYDRATED_GITDIRS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Run [`hydrate_from_db`] exactly once per process per `gitdir`. Cheap
/// no-op on subsequent calls. Errors are logged and swallowed so a
/// transient DB read failure can't block the user from issuing further
/// override mutations.
pub fn ensure_hydrated(db: &but_db::DbHandle, gitdir: &Path) {
    {
        let set = hydrated_gitdirs().lock().expect("hydrate set poisoned");
        if set.contains(gitdir) {
            return;
        }
    }
    match hydrate_from_db(db, gitdir) {
        Ok(_n) => {}
        Err(err) => {
            tracing::warn!(?err, ?gitdir, "sub_hunk override hydration failed");
        }
    }
    hydrated_gitdirs()
        .lock()
        .expect("hydrate set poisoned")
        .insert(gitdir.to_path_buf());
}

#[cfg(test)]
fn clear_hydrated_for_tests(gitdir: &Path) {
    hydrated_gitdirs()
        .lock()
        .expect("hydrate set poisoned")
        .remove(gitdir);
}

/// Read every override row for `gitdir` from `db` and populate the
/// in-memory store. Returns the number of overrides hydrated.
///
/// Rows that fail to deserialize (e.g. because the on-disk shape drifted)
/// are silently skipped after a warning; the surrounding `reconcile_with_overrides`
/// pass on the next worktree read will drop their stale anchor mappings
/// from memory if they don't match anymore.
pub fn hydrate_from_db(db: &but_db::DbHandle, gitdir: &Path) -> Result<usize> {
    let key = gitdir_key(gitdir);
    let rows = db.sub_hunk_overrides().list_for_gitdir(&key)?;
    let mut count = 0;
    for row in rows {
        match from_db_row(row) {
            Ok(ov) => {
                upsert_override(gitdir, ov);
                count += 1;
            }
            Err(err) => {
                tracing::warn!(?err, "skipping malformed sub_hunk_override row");
            }
        }
    }
    Ok(count)
}

/// Insert or replace `ov` both in memory and on disk. Returns whether
/// the row was persisted (it is dropped from disk — but kept in memory —
/// when [`to_db_row`] refuses it for size).
pub fn upsert_override_persistent(
    db: &mut but_db::DbHandle,
    gitdir: &Path,
    ov: SubHunkOverride,
) -> Result<bool> {
    ensure_hydrated(db, gitdir);
    let row_opt = to_db_row(gitdir, &ov)?;
    upsert_override(gitdir, ov.clone());
    match row_opt {
        Some(row) => {
            db.sub_hunk_overrides_mut().upsert(row)?;
            Ok(true)
        }
        None => {
            // Refused for size. Make sure no stale row is left behind
            // (e.g. from an earlier, smaller version of the same anchor).
            let key = gitdir_key(gitdir);
            let commit_id_bytes = origin_commit_id_bytes(&ov.origin);
            let _ = db.sub_hunk_overrides_mut().delete(
                &key,
                &ov.path,
                ov.anchor.old_start,
                ov.anchor.old_lines,
                ov.anchor.new_start,
                ov.anchor.new_lines,
                &commit_id_bytes,
            )?;
            Ok(false)
        }
    }
}

/// Remove `(path, anchor)` both in memory and on disk
/// (worktree-anchored). Commit-anchored removals reach the disk
/// through Phase 7c-3's `unsplit_hunk_in_commit` RPC.
pub fn remove_override_persistent(
    db: &mut but_db::DbHandle,
    gitdir: &Path,
    path: &BString,
    anchor: HunkHeader,
) -> Result<Option<SubHunkOverride>> {
    ensure_hydrated(db, gitdir);
    let removed = remove_override(gitdir, path, anchor);
    let key = gitdir_key(gitdir);
    db.sub_hunk_overrides_mut().delete(
        &key,
        path,
        anchor.old_start,
        anchor.old_lines,
        anchor.new_start,
        anchor.new_lines,
        &[],
    )?;
    Ok(removed)
}

/// Drop every override in `keys` both in memory and on disk.
/// Worktree-anchored only — the `BString`-shaped key implies a
/// `commit_id = b""` row.
pub fn drop_overrides_persistent(
    db: &mut but_db::DbHandle,
    gitdir: &Path,
    keys: &[(BString, HunkHeader)],
) -> Result<()> {
    drop_overrides(gitdir, keys);
    if keys.is_empty() {
        return Ok(());
    }
    let key = gitdir_key(gitdir);
    let mut handle = db.sub_hunk_overrides_mut();
    for (path, anchor) in keys {
        handle.delete(
            &key,
            path,
            anchor.old_start,
            anchor.old_lines,
            anchor.new_start,
            anchor.new_lines,
            &[],
        )?;
    }
    Ok(())
}

/// Encode an origin's commit id into the on-disk `commit_id` blob.
/// Worktree-anchored → empty. Commit-anchored → the OID bytes.
fn origin_commit_id_bytes(origin: &SubHunkOriginLocation) -> Vec<u8> {
    match origin {
        SubHunkOriginLocation::Worktree { .. } => Vec::new(),
        SubHunkOriginLocation::Commit { id, .. } => id.as_bytes().to_vec(),
    }
}

/// `reconcile_with_overrides` plus DB write-through for any overrides that
/// the reconcile pass drops or migrates. Use this from any path that
/// already has a `&mut DbHandle`; the in-memory-only variant is preserved
/// for callers that don't.
///
/// Compares the **disk** state to the **post-reconcile in-memory** state,
/// not memory-before to memory-after, so that drift introduced by an
/// earlier non-persistent reconcile (e.g. the watcher path that calls
/// `assignments_with_fallback`) gets caught and corrected. Without that,
/// the in-memory store can be migrated by the watcher and the eventual
/// `changes_in_worktree_with_perm` reconcile sees identical before/after
/// snapshots and emits no writes.
pub fn reconcile_with_overrides_persistent(
    db: &mut but_db::DbHandle,
    gitdir: &Path,
    assignments: &mut Vec<HunkAssignment>,
) -> Result<()> {
    ensure_hydrated(db, gitdir);
    reconcile_with_overrides(gitdir, assignments);

    // Phase 7c-2: this reconcile pass is still intentionally
    // worktree-only. The disk side now has a `commit_id` column
    // (Phase 7c-2's v2 schema), so we filter to rows with
    // `commit_id.is_empty()` to mirror the worktree-only memory
    // side. Commit-anchored reconcile (against a commit's diff,
    // not the worktree) is a separate pass introduced when 7c-3
    // wires the commit-side `split_hunk_in_commit` RPC.
    let key = gitdir_key(gitdir);
    let disk_rows = db.sub_hunk_overrides().list_for_gitdir(&key)?;
    let disk_keyed: HashMap<(BString, HunkHeader), but_db::SubHunkOverrideRow> = disk_rows
        .into_iter()
        .filter(|row| row.commit_id.is_empty())
        .map(|row| {
            let key = (
                BString::from(row.path.clone()),
                HunkHeader {
                    old_start: row.anchor_old_start,
                    old_lines: row.anchor_old_lines,
                    new_start: row.anchor_new_start,
                    new_lines: row.anchor_new_lines,
                },
            );
            (key, row)
        })
        .collect();

    let mem_overrides = list_worktree_overrides(gitdir);
    let mem_keyed: HashMap<(BString, HunkHeader), &SubHunkOverride> = mem_overrides
        .iter()
        .map(|ov| ((ov.path.clone(), ov.anchor), ov))
        .collect();

    let mut handle = db.sub_hunk_overrides_mut();

    // 1) Delete every disk row whose `(path, anchor)` is no longer in memory.
    for (k, _) in &disk_keyed {
        if !mem_keyed.contains_key(k) {
            handle.delete(
                &key,
                &k.0,
                k.1.old_start,
                k.1.old_lines,
                k.1.new_start,
                k.1.new_lines,
                &[],
            )?;
        }
    }

    // 2) Upsert every in-memory override that is missing from disk or
    //    differs structurally. Skips no-op upserts to avoid pointless writes.
    for (k, ov) in &mem_keyed {
        let disk = disk_keyed.get(k);
        let candidate = match to_db_row(gitdir, ov)? {
            Some(row) => row,
            None => {
                // Size guard refused; ensure no stale row remains.
                let _ = handle.delete(
                    &key,
                    &k.0,
                    k.1.old_start,
                    k.1.old_lines,
                    k.1.new_start,
                    k.1.new_lines,
                    &[],
                )?;
                continue;
            }
        };
        if disk == Some(&candidate) {
            continue;
        }
        handle.upsert(candidate)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor() -> HunkHeader {
        // -10,5 +10,5
        HunkHeader {
            old_start: 10,
            old_lines: 5,
            new_start: 10,
            new_lines: 5,
        }
    }

    fn sample_diff() -> BString {
        // A 6-row body: ctx, -, +, -, +, ctx.
        BString::from(
            "@@ -10,5 +10,5 @@\n c1\n-r1\n+a1\n-r2\n+a2\n c2\n",
        )
    }

    #[test]
    fn parse_row_kinds_basic() {
        let kinds = parse_row_kinds(sample_diff().as_ref());
        assert_eq!(
            kinds,
            vec![
                RowKind::Context,
                RowKind::Remove,
                RowKind::Add,
                RowKind::Remove,
                RowKind::Add,
                RowKind::Context,
            ]
        );
    }

    #[test]
    fn validate_ranges_rejects_full_coverage() {
        let err = validate_ranges(&[RowRange { start: 0, end: 6 }], 6).unwrap_err();
        assert!(err.to_string().contains("entire anchor"));
    }

    #[test]
    fn validate_ranges_rejects_overlap_and_oob() {
        assert!(
            validate_ranges(
                &[RowRange { start: 0, end: 3 }, RowRange { start: 2, end: 4 }],
                6,
            )
            .is_err()
        );
        assert!(validate_ranges(&[RowRange { start: 0, end: 7 }], 6).is_err());
        assert!(validate_ranges(&[RowRange { start: 3, end: 3 }], 6).is_err());
        assert!(validate_ranges(&[], 6).is_err());
    }

    #[test]
    fn validate_ranges_accepts_partial() {
        validate_ranges(&[RowRange { start: 1, end: 4 }], 6).unwrap();
        validate_ranges(
            &[RowRange { start: 0, end: 2 }, RowRange { start: 3, end: 5 }],
            6,
        )
        .unwrap();
    }

    #[test]
    fn synthesize_header_pure_add_at_start() {
        // diff: +a +a ctx ctx
        let kinds = vec![RowKind::Add, RowKind::Add, RowKind::Context, RowKind::Context];
        let anchor = HunkHeader {
            old_start: 100,
            old_lines: 2,
            new_start: 100,
            new_lines: 4,
        };
        let h = synthesize_header(&anchor, &kinds, RowRange { start: 0, end: 2 });
        assert_eq!(h.old_start, 100);
        assert_eq!(h.old_lines, 0);
        assert_eq!(h.new_start, 100);
        assert_eq!(h.new_lines, 2);
    }

    #[test]
    fn synthesize_header_pure_remove_at_end() {
        // diff: ctx ctx -r -r
        let kinds = vec![
            RowKind::Context,
            RowKind::Context,
            RowKind::Remove,
            RowKind::Remove,
        ];
        let anchor = HunkHeader {
            old_start: 50,
            old_lines: 4,
            new_start: 50,
            new_lines: 2,
        };
        let h = synthesize_header(&anchor, &kinds, RowRange { start: 2, end: 4 });
        assert_eq!(h.old_start, 52);
        assert_eq!(h.old_lines, 2);
        assert_eq!(h.new_start, 52);
        assert_eq!(h.new_lines, 0);
    }

    #[test]
    fn synthesize_header_mixed_middle() {
        // ctx -r +a -r +a ctx, anchor -10,5 +10,5
        let kinds = parse_row_kinds(sample_diff().as_ref());
        let h = synthesize_header(&anchor(), &kinds, RowRange { start: 1, end: 5 });
        // Before row 1: 1 ctx → old/new offsets each +1.
        // In [1,5): 2 removes + 2 adds → old_lines=2, new_lines=2.
        assert_eq!(h.old_start, 11);
        assert_eq!(h.old_lines, 2);
        assert_eq!(h.new_start, 11);
        assert_eq!(h.new_lines, 2);
    }

    #[test]
    fn trim_context_drops_pure_context_selection() {
        let kinds = vec![RowKind::Context, RowKind::Context];
        assert!(trim_context(RowRange { start: 0, end: 2 }, &kinds).is_none());
    }

    #[test]
    fn trim_context_strips_boundaries() {
        // ctx + ctx → range (0,3) trims to (1,2)
        let kinds = vec![RowKind::Context, RowKind::Add, RowKind::Context];
        let trimmed = trim_context(RowRange { start: 0, end: 3 }, &kinds).unwrap();
        assert_eq!(trimmed, RowRange { start: 1, end: 2 });
    }

    #[test]
    fn sub_diff_body_extracts_row_slice() {
        let body = sample_diff();
        let slice = sub_diff_body(body.as_ref(), RowRange { start: 1, end: 3 });
        assert_eq!(slice, BString::from("-r1\n+a1\n"));
    }

    fn anchor_assignment() -> HunkAssignment {
        HunkAssignment {
            id: Some(Uuid::new_v4()),
            hunk_header: Some(anchor()),
            path: "foo.rs".to_string(),
            path_bytes: BString::from("foo.rs"),
            stack_id: None,
            branch_ref_bytes: Some(
                gix::refs::FullName::try_from("refs/heads/main".to_string()).unwrap(),
            ),
            line_nums_added: None,
            line_nums_removed: None,
            diff: Some(sample_diff()),
            sub_hunk_origin: None,
        }
    }

    /// Build a stored-form `SubHunkOverride` from user ranges, populating
    /// `rows`, `anchor_diff`, and residual ranges via `materialize_residual_ranges`.
    fn make_stored_override(
        anchor: HunkHeader,
        diff: BString,
        user_ranges: Vec<RowRange>,
        assignments: BTreeMap<RowRange, HunkAssignmentTarget>,
    ) -> SubHunkOverride {
        let kinds = parse_row_kinds(diff.as_ref());
        let ranges = materialize_residual_ranges(&user_ranges, &kinds);
        let path = BString::from("foo.rs");
        SubHunkOverride {
            origin: SubHunkOriginLocation::worktree(path.clone()),
            path,
            anchor,
            ranges,
            assignments,
            rows: kinds,
            anchor_diff: diff,
        }
    }

    #[test]
    fn materialize_override_three_way_split() {
        // anchor body: ctx -r +a -r +a ctx (rows 0..6)
        // user range: rows 2..4 (the +a -r middle pair)
        let ov = make_stored_override(
            anchor(),
            sample_diff(),
            vec![RowRange { start: 2, end: 4 }],
            BTreeMap::new(),
        );
        let subs = materialize_override(&anchor_assignment(), &ov);
        assert_eq!(subs.len(), 3, "leading residual + user range + trailing residual");
        // Each carries the anchor's branch_ref.
        for s in &subs {
            assert_eq!(
                s.branch_ref_bytes.as_ref().map(|r| r.to_string()),
                Some("refs/heads/main".to_string()),
            );
            assert_eq!(s.path, "foo.rs");
            assert!(s.hunk_header.is_some());
            assert!(s.id.is_some());
        }
    }

    #[test]
    fn materialize_override_two_way_at_edge() {
        let ov = make_stored_override(
            anchor(),
            sample_diff(),
            vec![RowRange { start: 0, end: 3 }],
            BTreeMap::new(),
        );
        let subs = materialize_override(&anchor_assignment(), &ov);
        assert_eq!(subs.len(), 2, "edge selection collapses to 2-way");
    }

    #[test]
    fn materialize_override_per_range_branch_target() {
        let other_branch = BString::from("refs/heads/feature");
        let mut assignments = BTreeMap::new();
        let r = RowRange { start: 2, end: 4 };
        assignments.insert(
            r,
            HunkAssignmentTarget::Branch {
                branch_ref_bytes: other_branch.clone(),
            },
        );
        let ov = make_stored_override(anchor(), sample_diff(), vec![r], assignments);
        let subs = materialize_override(&anchor_assignment(), &ov);
        assert_eq!(subs.len(), 3);
        let on_feature: Vec<_> = subs
            .iter()
            .filter(|s| {
                s.branch_ref_bytes.as_ref().map(|r| r.to_string())
                    == Some("refs/heads/feature".to_string())
            })
            .collect();
        let on_main: Vec<_> = subs
            .iter()
            .filter(|s| {
                s.branch_ref_bytes.as_ref().map(|r| r.to_string())
                    == Some("refs/heads/main".to_string())
            })
            .collect();
        assert_eq!(on_feature.len(), 1, "middle sub-hunk on override branch");
        assert_eq!(on_main.len(), 2, "residuals on anchor branch");
    }

    #[test]
    fn encode_pure_add_at_start() {
        // Body: +a +a ctx ctx, anchor -100,2 +100,4.
        let rows = vec![RowKind::Add, RowKind::Add, RowKind::Context, RowKind::Context];
        let anchor = HunkHeader {
            old_start: 100,
            old_lines: 2,
            new_start: 100,
            new_lines: 4,
        };
        let headers = encode_sub_hunk_for_commit(anchor, RowRange { start: 0, end: 2 }, &rows);
        assert_eq!(
            headers,
            vec![HunkHeader { old_start: 0, old_lines: 0, new_start: 100, new_lines: 2 }]
        );
    }

    #[test]
    fn encode_pure_remove_at_end() {
        // Body: ctx ctx -r -r, anchor -50,4 +50,2.
        let rows = vec![RowKind::Context, RowKind::Context, RowKind::Remove, RowKind::Remove];
        let anchor = HunkHeader {
            old_start: 50,
            old_lines: 4,
            new_start: 50,
            new_lines: 2,
        };
        let headers = encode_sub_hunk_for_commit(anchor, RowRange { start: 2, end: 4 }, &rows);
        assert_eq!(
            headers,
            vec![HunkHeader { old_start: 52, old_lines: 2, new_start: 0, new_lines: 0 }]
        );
    }

    #[test]
    fn encode_mixed_emits_two_headers() {
        // Body: ctx -r +a -r +a ctx, anchor -10,5 +10,5.
        // Range rows 1..5 covers -r +a -r +a.
        let rows = parse_row_kinds(sample_diff().as_ref());
        let headers = encode_sub_hunk_for_commit(anchor(), RowRange { start: 1, end: 5 }, &rows);
        // Walk: starting at row 1, new_line=11, old_line=11 (after row 0 ctx).
        // -r => (-11,1 +0,0); new_line stays 11, old_line=12
        // +a => (-0,0 +11,1); new_line=12
        // -r => (-12,1 +0,0); old_line=13
        // +a => (-0,0 +12,1); new_line=13
        assert_eq!(
            headers,
            vec![
                HunkHeader { old_start: 11, old_lines: 1, new_start: 0, new_lines: 0 },
                HunkHeader { old_start: 0, old_lines: 0, new_start: 11, new_lines: 1 },
                HunkHeader { old_start: 12, old_lines: 1, new_start: 0, new_lines: 0 },
                HunkHeader { old_start: 0, old_lines: 0, new_start: 12, new_lines: 1 },
            ]
        );
    }

    #[test]
    fn encode_collapses_contiguous_runs() {
        // Body: +a +a +a, anchor -1,0 +1,3.
        let rows = vec![RowKind::Add, RowKind::Add, RowKind::Add];
        let anchor = HunkHeader {
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 3,
        };
        let headers = encode_sub_hunk_for_commit(anchor, RowRange { start: 0, end: 3 }, &rows);
        assert_eq!(
            headers,
            vec![HunkHeader { old_start: 0, old_lines: 0, new_start: 1, new_lines: 3 }]
        );
    }

    #[test]
    fn encode_skips_internal_context() {
        // Body: +a ctx +a, anchor -1,1 +1,3.
        let rows = vec![RowKind::Add, RowKind::Context, RowKind::Add];
        let anchor = HunkHeader {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 3,
        };
        let headers = encode_sub_hunk_for_commit(anchor, RowRange { start: 0, end: 3 }, &rows);
        // First add at new_line=1, then ctx advances to 2, then add at new_line=2.
        assert_eq!(
            headers,
            vec![
                HunkHeader { old_start: 0, old_lines: 0, new_start: 1, new_lines: 1 },
                HunkHeader { old_start: 0, old_lines: 0, new_start: 3, new_lines: 1 },
            ]
        );
    }

    #[test]
    fn encode_single_row_range() {
        let rows = parse_row_kinds(sample_diff().as_ref());
        // Row 2 is +a (after ctx, -r). new_line=11.
        let headers = encode_sub_hunk_for_commit(anchor(), RowRange { start: 2, end: 3 }, &rows);
        assert_eq!(
            headers,
            vec![HunkHeader { old_start: 0, old_lines: 0, new_start: 11, new_lines: 1 }]
        );
    }

    #[test]
    fn encode_with_leading_context_in_range() {
        // Body: ctx -r ctx +a, range 0..4 (deliberately includes leading
        // context to verify the encoder skips it without consuming line
        // numbers from the run output). Anchor -1,2 +1,2.
        let rows = vec![RowKind::Context, RowKind::Remove, RowKind::Context, RowKind::Add];
        let anchor = HunkHeader {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 2,
        };
        let headers = encode_sub_hunk_for_commit(anchor, RowRange { start: 0, end: 4 }, &rows);
        // ctx: old=2,new=2. -r at old=2 => (-2,1 +0,0); old=3.
        // ctx: old=4,new=3. +a at new=3 => (-0,0 +3,1).
        assert_eq!(
            headers,
            vec![
                HunkHeader { old_start: 2, old_lines: 1, new_start: 0, new_lines: 0 },
                HunkHeader { old_start: 0, old_lines: 0, new_start: 3, new_lines: 1 },
            ]
        );
    }

    #[test]
    fn apply_overrides_silently_drops_unmatched_anchor() {
        let mut assignments = vec![anchor_assignment()];
        let stale_anchor = HunkHeader {
            old_start: 999,
            old_lines: 1,
            new_start: 999,
            new_lines: 1,
        };
        // Use unrelated content so migration's content match cannot pull
        // it onto the natural anchor.
        let stale_diff = BString::from(
            "@@ -999,1 +999,1 @@\n-totally-unrelated-line\n+also-unrelated\n",
        );
        let ov = make_stored_override(
            stale_anchor,
            stale_diff,
            vec![RowRange { start: 0, end: 2 }],
            BTreeMap::new(),
        );
        // Override the path to point somewhere not present in assignments
        // so migration's same-path filter excludes everything.
        let mut ov = ov;
        ov.path = BString::from("nonexistent.rs");
        let outcomes = apply_overrides_to_assignments(&mut assignments, &[ov]);
        assert_eq!(outcomes.len(), 1);
        assert!(matches!(outcomes[0], OverrideOutcome::Drop));
        assert_eq!(assignments.len(), 1, "anchor not split when override unmatched");
    }

    #[test]
    fn reconcile_with_overrides_prunes_stale_entries() {
        let gitdir = std::path::Path::new("/test/gitdir/reconcile-prune");
        clear_store_for_tests_at(Some(gitdir));
        let stale_anchor = HunkHeader {
            old_start: 999,
            old_lines: 1,
            new_start: 999,
            new_lines: 1,
        };
        // Stale override on a different path so migration cannot rescue it.
        let mut stale = make_stored_override(
            stale_anchor,
            BString::from(
                "@@ -999,1 +999,1 @@\n-unrelated\n+content\n",
            ),
            vec![RowRange { start: 0, end: 2 }],
            BTreeMap::new(),
        );
        let missing = BString::from("missing.rs");
        stale.path = missing.clone();
        stale.origin = SubHunkOriginLocation::worktree(missing);
        upsert_override(gitdir, stale);
        upsert_override(
            gitdir,
            make_stored_override(
                anchor(),
                sample_diff(),
                vec![RowRange { start: 2, end: 4 }],
                BTreeMap::new(),
            ),
        );
        assert_eq!(list_overrides(gitdir).len(), 2);

        let mut assignments = vec![anchor_assignment()];
        reconcile_with_overrides(gitdir, &mut assignments);

        // Stale override is dropped; live override materialized into 3 sub-hunks.
        assert_eq!(assignments.len(), 3);
        let remaining = list_overrides(gitdir);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].anchor, anchor());

        clear_store_for_tests_at(Some(gitdir));
    }

    // ---- Phase 4.5: residual materialization + override migration ----

    #[test]
    fn materialize_residual_ranges_full_coverage() {
        // Body: ctx -r +a -r +a ctx (rows 0..6).
        let kinds = parse_row_kinds(sample_diff().as_ref());
        // User picks middle pair (rows 2..4). Expect leading residual
        // (1..2, the -r alone) and trailing residual (4..5, the +a alone).
        // Pure-context rows 0 and 5 are dropped.
        let out = materialize_residual_ranges(&[RowRange { start: 2, end: 4 }], &kinds);
        assert_eq!(
            out,
            vec![
                RowRange { start: 1, end: 2 },
                RowRange { start: 2, end: 4 },
                RowRange { start: 4, end: 5 },
            ]
        );
    }

    #[test]
    fn materialize_residual_ranges_drops_pure_context_gap() {
        // Body: +a ctx ctx +a (rows 0..4).
        let kinds = vec![
            RowKind::Add,
            RowKind::Context,
            RowKind::Context,
            RowKind::Add,
        ];
        // User picks just row 0..1; the middle gap 1..3 is pure-context
        // and must be dropped from the output.
        let out = materialize_residual_ranges(&[RowRange { start: 0, end: 1 }], &kinds);
        assert_eq!(
            out,
            vec![RowRange { start: 0, end: 1 }, RowRange { start: 3, end: 4 }],
        );
    }

    /// Helper: build a natural-hunk `HunkAssignment` with a given header and
    /// diff body.
    fn nat_assignment(path: &str, header: HunkHeader, diff: BString) -> HunkAssignment {
        HunkAssignment {
            id: Some(Uuid::new_v4()),
            hunk_header: Some(header),
            path: path.to_string(),
            path_bytes: BString::from(path),
            stack_id: None,
            branch_ref_bytes: Some(
                gix::refs::FullName::try_from("refs/heads/main".to_string()).unwrap(),
            ),
            line_nums_added: None,
            line_nums_removed: None,
            diff: Some(diff),
            sub_hunk_origin: None,
        }
    }

    #[test]
    fn migration_remaps_residuals_after_partial_commit() {
        // Pre-commit anchor: 3 added rows, anchor -1,0 +1,3.
        //   Body:
        //     +A
        //     +B
        //     +C
        // User splits middle row (B) and commits it. Worktree text didn't
        // change, so A and C still appear in the diff but B is now part of
        // HEAD. The post-commit natural hunk is two separate `+A`/`+C`
        // hunks (or one combined hunk with B as context, depending on
        // context-line settings). We model it here as a single hunk with
        // body `+A ctx +C` at -2,1 +1,3 (B is now line 2 in HEAD as well
        // as in the worktree).
        let pre_anchor = HunkHeader {
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 3,
        };
        let pre_diff = BString::from("@@ -1,0 +1,3 @@\n+A\n+B\n+C\n");
        let ov = make_stored_override(
            pre_anchor,
            pre_diff,
            vec![RowRange { start: 1, end: 2 }], // user picked B
            BTreeMap::new(),
        );
        // Sanity: residuals should also be present.
        assert_eq!(
            ov.ranges,
            vec![
                RowRange { start: 0, end: 1 }, // +A
                RowRange { start: 1, end: 2 }, // +B (committed)
                RowRange { start: 2, end: 3 }, // +C
            ]
        );

        // Post-commit natural hunk on the same path. New-side line numbers
        // are unchanged (worktree text identical); old-side now starts at 2
        // because B was added to HEAD.
        let post_anchor = HunkHeader {
            old_start: 2,
            old_lines: 1,
            new_start: 1,
            new_lines: 3,
        };
        let post_diff = BString::from("@@ -2,1 +1,3 @@\n+A\n B\n+C\n");
        let mut assignments = vec![nat_assignment("foo.rs", post_anchor, post_diff)];

        let outcomes = apply_overrides_to_assignments(&mut assignments, &[ov.clone()]);
        assert_eq!(outcomes.len(), 1);
        let migrated_list = match &outcomes[0] {
            OverrideOutcome::Migrated(m) => m.clone(),
            other => panic!("expected Migrated, got {other:?}"),
        };
        assert_eq!(migrated_list.len(), 1, "single post-commit candidate hunk");
        let migrated = &migrated_list[0];

        // The B range (now context in the new diff) is dropped; the +A and
        // +C ranges survive.
        assert_eq!(migrated.anchor, post_anchor);
        assert_eq!(
            migrated.ranges,
            vec![
                RowRange { start: 0, end: 1 }, // +A at index 0
                RowRange { start: 2, end: 3 }, // +C at index 2
            ]
        );
        // Materialization split the post-anchor into 2 sub-hunks.
        assert_eq!(assignments.len(), 2);
    }

    #[test]
    fn migration_drops_override_when_no_rows_survive() {
        // Pre-commit override covers a single row (+A) inside a 2-row
        // pre-anchor. The user commits +A. Post-commit the natural hunk's
        // shape changed (different header) and +A no longer appears, so
        // exact match misses and migration should also drop because no
        // surviving content row matches.
        let pre_anchor = HunkHeader {
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 2,
        };
        let pre_diff = BString::from("@@ -1,0 +1,2 @@\n+A\n+B\n");
        let ov = make_stored_override(
            pre_anchor,
            pre_diff,
            vec![RowRange { start: 0, end: 1 }],
            BTreeMap::new(),
        );
        // Post-commit: anchor header differs (forces miss on exact match)
        // and the body has unrelated content (no row in the override range
        // can be re-found by content match).
        let post_anchor = HunkHeader {
            old_start: 2,
            old_lines: 0,
            new_start: 2,
            new_lines: 1,
        };
        let post_diff = BString::from("@@ -2,0 +2,1 @@\n+different\n");
        let mut assignments = vec![nat_assignment("foo.rs", post_anchor, post_diff)];

        let outcomes = apply_overrides_to_assignments(&mut assignments, &[ov]);
        assert!(
            matches!(outcomes[0], OverrideOutcome::Drop),
            "got {:?}",
            outcomes[0]
        );
    }

    #[test]
    fn migration_preserves_per_range_assignment_targets() {
        // 3-row pre-anchor, user picks middle row and reassigns it to a
        // feature branch. Commit the *first* row. Migration should preserve
        // the feature-branch reassignment for the surviving middle range.
        let pre_anchor = HunkHeader {
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 3,
        };
        let pre_diff = BString::from("@@ -1,0 +1,3 @@\n+A\n+B\n+C\n");
        let mid = RowRange { start: 1, end: 2 };
        let mut targeted = BTreeMap::new();
        targeted.insert(
            mid,
            HunkAssignmentTarget::Branch {
                branch_ref_bytes: BString::from("refs/heads/feature"),
            },
        );
        let ov = make_stored_override(pre_anchor, pre_diff, vec![mid], targeted);

        // Post-commit: row A committed, so worktree diff is +B +C.
        let post_anchor = HunkHeader {
            old_start: 2,
            old_lines: 0,
            new_start: 2,
            new_lines: 2,
        };
        let post_diff = BString::from("@@ -2,0 +2,2 @@\n+B\n+C\n");
        let mut assignments = vec![nat_assignment("foo.rs", post_anchor, post_diff)];
        let outcomes = apply_overrides_to_assignments(&mut assignments, &[ov]);
        let migrated_list = match &outcomes[0] {
            OverrideOutcome::Migrated(m) => m.clone(),
            other => panic!("expected Migrated, got {other:?}"),
        };
        assert_eq!(migrated_list.len(), 1);
        let migrated = &migrated_list[0];
        // +B is now at row index 0 in the post diff.
        let new_b = RowRange { start: 0, end: 1 };
        let target = migrated
            .assignments
            .get(&new_b)
            .expect("feature-branch assignment migrated to new range key");
        match target {
            HunkAssignmentTarget::Branch { branch_ref_bytes } => {
                assert_eq!(branch_ref_bytes, &BString::from("refs/heads/feature"));
            }
            other => panic!("unexpected target {other:?}"),
        }
    }

    /// After a partial commit on a long pure-add hunk, the post-commit
    /// natural worktree diff often splits into TWO hunks (the committed
    /// rows are now context with enough surrounding context to form a
    /// boundary). The migration must split the override across both
    /// surviving candidate hunks instead of dropping it — otherwise
    /// residuals collapse back into the natural hunks (the user-visible
    /// regression).
    #[test]
    fn migration_splits_across_multiple_candidates_after_partial_commit() {
        // 9-row pure-add anchor: lines 1..9. User splits row 4 (line 5)
        // into its own sub-hunk and commits it. Post-commit, with default
        // 3-line context, the diff becomes TWO hunks:
        //   @@ -1,0 +1,4 @@   +A +B +C +D
        //   @@ -2,0 +6,4 @@   +F +G +H +I
        let pre_anchor = HunkHeader {
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 9,
        };
        let pre_diff = BString::from(
            "@@ -1,0 +1,9 @@\n+A\n+B\n+C\n+D\n+E\n+F\n+G\n+H\n+I\n",
        );
        let ov = make_stored_override(
            pre_anchor,
            pre_diff,
            vec![RowRange { start: 4, end: 5 }], // +E
            BTreeMap::new(),
        );

        let h1 = HunkHeader { old_start: 1, old_lines: 0, new_start: 1, new_lines: 4 };
        let h2 = HunkHeader { old_start: 2, old_lines: 0, new_start: 6, new_lines: 4 };
        let mut assignments = vec![
            nat_assignment(
                "foo.rs",
                h1,
                BString::from("@@ -1,0 +1,4 @@\n+A\n+B\n+C\n+D\n"),
            ),
            nat_assignment(
                "foo.rs",
                h2,
                BString::from("@@ -2,0 +6,4 @@\n+F\n+G\n+H\n+I\n"),
            ),
        ];
        let outcomes = apply_overrides_to_assignments(&mut assignments, &[ov]);
        let migrated_list = match &outcomes[0] {
            OverrideOutcome::Migrated(m) => m.clone(),
            other => panic!("expected Migrated, got {other:?}"),
        };
        assert_eq!(
            migrated_list.len(),
            2,
            "override fans out across both surviving natural hunks",
        );
        // Both anchor hunks should now be present in `assignments`. We
        // don't assert exact ordering of materialized sub-hunks here; the
        // important guarantee is that the override survives across both
        // candidates rather than collapsing.
        let anchors: Vec<HunkHeader> = migrated_list.iter().map(|m| m.anchor).collect();
        assert!(anchors.contains(&h1));
        assert!(anchors.contains(&h2));
    }

    #[test]
    fn migration_fans_out_residuals_across_two_candidates() {
        // Pre-anchor covers new-side lines 1..4 (+A +B +C). User picks +B
        // and commits it. Post-commit two candidates remain: h1 with +A,
        // h2 with +C. The +B range no longer matches anywhere; the +A and
        // +C residuals each fan out onto their respective candidate.
        let pre_anchor = HunkHeader {
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 3,
        };
        let pre_diff = BString::from("@@ -1,0 +1,3 @@\n+A\n+B\n+C\n");
        let ov = make_stored_override(
            pre_anchor,
            pre_diff,
            vec![RowRange { start: 1, end: 2 }],
            BTreeMap::new(),
        );
        let h1 = HunkHeader { old_start: 1, old_lines: 0, new_start: 1, new_lines: 1 };
        let h2 = HunkHeader { old_start: 2, old_lines: 0, new_start: 3, new_lines: 1 };
        let mut assignments = vec![
            nat_assignment("foo.rs", h1, BString::from("@@ -1,0 +1,1 @@\n+A\n")),
            nat_assignment("foo.rs", h2, BString::from("@@ -2,0 +3,1 @@\n+C\n")),
        ];
        let outcomes = apply_overrides_to_assignments(&mut assignments, &[ov]);
        let migrated_list = match &outcomes[0] {
            OverrideOutcome::Migrated(m) => m.clone(),
            other => panic!("expected Migrated, got {other:?}"),
        };
        assert_eq!(migrated_list.len(), 2);
        let anchors: Vec<HunkHeader> = migrated_list.iter().map(|m| m.anchor).collect();
        assert!(anchors.contains(&h1));
        assert!(anchors.contains(&h2));
    }

    #[test]
    fn reconcile_rekeys_store_after_migration() {
        // NB: do not call `clear_store_for_tests()` — it would race against
        // tests running in parallel that share the global override store.
        // Use a unique gitdir for isolation instead.
        let gitdir = std::path::Path::new("/test/gitdir/reconcile-rekey-after-migration");
        let pre_anchor = HunkHeader {
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 3,
        };
        let pre_diff = BString::from("@@ -1,0 +1,3 @@\n+A\n+B\n+C\n");
        upsert_override(
            gitdir,
            make_stored_override(
                pre_anchor,
                pre_diff,
                vec![RowRange { start: 1, end: 2 }],
                BTreeMap::new(),
            ),
        );

        // Post-commit natural hunk.
        let post_anchor = HunkHeader {
            old_start: 2,
            old_lines: 1,
            new_start: 1,
            new_lines: 3,
        };
        let post_diff = BString::from("@@ -2,1 +1,3 @@\n+A\n B\n+C\n");
        let mut assignments = vec![nat_assignment("foo.rs", post_anchor, post_diff)];
        reconcile_with_overrides(gitdir, &mut assignments);

        let remaining = list_overrides(gitdir);
        assert_eq!(remaining.len(), 1, "override survived migration");
        assert_eq!(remaining[0].anchor, post_anchor, "store rekeyed to new anchor");
        assert_eq!(remaining[0].ranges.len(), 2, "B dropped, A and C survive");
    }

    /// Pinning regression for the production bug observed in
    /// `splittest_pure_add.md`: a 19-row pure-add hunk with several
    /// blank lines, split into 3 ranges (intro / Section A / Section
    /// B+C), then Section A committed. The post-commit natural diff
    /// has only ONE candidate (Section A becomes context with the
    /// surrounding adds in the same hunk). Because content-only matching
    /// would map any blank `+` row to the *first* blank in the new diff,
    /// Regression for the bug where uncommit re-introduces content
    /// that the override doesn't cover, and the previously-uncovered
    /// rows get silently dropped from the rendered diff.
    ///
    /// Scenario: user splits a 5-row pure-add hunk into
    /// [(1,2), (3,4)] (two single-row picks with residual gaps). They
    /// commit those rows, then uncommit them — the natural hunk grows
    /// back to 5 rows. The migration must re-emit residual ranges for
    /// the rows that now exist in the new anchor but weren't in the
    /// old override's ranges, otherwise `materialize_override` only
    /// emits sub-hunks for the surviving user picks and the rest is
    /// hidden.
    #[test]
    fn migration_re_introduces_residuals_after_uncommit() {
        // "Mid-state" anchor (post-commit, pre-uncommit). Only rows 1
        // and 3 survived the partial commit; rows 0, 2, 4 are now
        // context (already in HEAD).
        let mid_anchor = HunkHeader {
            old_start: 1,
            old_lines: 3,
            new_start: 1,
            new_lines: 5,
        };
        let mid_diff = BString::from(
            "@@ -1,3 +1,5 @@\n\
             \x20row0\n\
             +row1\n\
             \x20row2\n\
             +row3\n\
             \x20row4\n",
        );
        // Override knows about rows 1 and 3 only (residuals between them
        // were context and got trimmed at upsert time).
        let mid_kinds = parse_row_kinds(mid_diff.as_ref());
        let mid_ranges = materialize_residual_ranges(
            &[RowRange { start: 1, end: 2 }, RowRange { start: 3, end: 4 }],
            &mid_kinds,
        );
        // Sanity: the context-only gaps should be trimmed away.
        assert_eq!(
            mid_ranges,
            vec![RowRange { start: 1, end: 2 }, RowRange { start: 3, end: 4 }]
        );
        let path = BString::from("foo.rs");
        let ov = SubHunkOverride {
            origin: SubHunkOriginLocation::worktree(path.clone()),
            path,
            anchor: mid_anchor,
            ranges: mid_ranges,
            assignments: BTreeMap::new(),
            rows: mid_kinds,
            anchor_diff: mid_diff,
        };

        // Post-uncommit anchor: all 5 rows are added again.
        let post_anchor = HunkHeader {
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 5,
        };
        let post_diff = BString::from(
            "@@ -1,0 +1,5 @@\n+row0\n+row1\n+row2\n+row3\n+row4\n",
        );
        let mut assignments = vec![nat_assignment("foo.rs", post_anchor, post_diff)];

        let outcomes = apply_overrides_to_assignments(&mut assignments, &[ov]);
        let migrated = match &outcomes[0] {
            OverrideOutcome::Migrated(m) => m.clone(),
            other => panic!("expected Migrated, got {other:?}"),
        };
        assert_eq!(migrated.len(), 1);
        // The user's picks (rows 1 and 3) are present, AND the
        // newly-reintroduced rows 0, 2, 4 each show up as their own
        // residual sub-hunk so the diff view doesn't lose them.
        assert_eq!(
            migrated[0].ranges,
            vec![
                RowRange { start: 0, end: 1 },
                RowRange { start: 1, end: 2 },
                RowRange { start: 2, end: 3 },
                RowRange { start: 3, end: 4 },
                RowRange { start: 4, end: 5 },
            ],
            "residuals must be re-materialized so uncommitted rows aren't \
             silently hidden by `materialize_override`",
        );
    }

    /// the original implementation collapsed range (9, 19) onto
    /// `[1, 19)` — overlapping range (0, 5)'s remap of `[0, 4)` and
    /// failing `validate_ranges_stored`. The order-preserving alignment
    /// must keep them disjoint.
    #[test]
    fn migration_handles_duplicate_blank_rows_in_single_candidate() {
        let pre_anchor = HunkHeader {
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 19,
        };
        // 19 added rows with blank lines at indices 1, 4, 9, 14.
        let pre_diff = BString::from(
            "@@ -1,0 +1,19 @@\n\
             +# Split Test\n\
             +\n\
             +This whole file\n\
             +pure-add hunk\n\
             +\n\
             +## Section A\n\
             +- alpha line one\n\
             +- alpha line two\n\
             +- alpha line three\n\
             +\n\
             +## Section B\n\
             +- beta line one\n\
             +- beta line two\n\
             +- beta line three\n\
             +\n\
             +## Section C\n\
             +- gamma line one\n\
             +- gamma line two\n\
             +- gamma line three\n",
        );
        let ov = make_stored_override(
            pre_anchor,
            pre_diff,
            vec![RowRange { start: 5, end: 9 }], // Section A.
            BTreeMap::new(),
        );

        // Post-commit: Section A is now in HEAD (4 lines). Worktree
        // unchanged (19 lines). The natural diff is one hunk with the
        // 4 Section-A rows showing as context.
        let post_anchor = HunkHeader {
            old_start: 1,
            old_lines: 4,
            new_start: 1,
            new_lines: 19,
        };
        // Note: leading-space context-line markers must be encoded
        // explicitly (\x20) here because Rust's `\` line-continuation
        // syntax eats whitespace at the start of the next physical
        // source line. Without this, ` - alpha line one` would lose its
        // leading space and parse as a remove row.
        let post_diff = BString::from(
            "@@ -1,4 +1,19 @@\n\
             +# Split Test\n\
             +\n\
             +This whole file\n\
             +pure-add hunk\n\
             +\n\
             \x20## Section A\n\
             \x20- alpha line one\n\
             \x20- alpha line two\n\
             \x20- alpha line three\n\
             +\n\
             +## Section B\n\
             +- beta line one\n\
             +- beta line two\n\
             +- beta line three\n\
             +\n\
             +## Section C\n\
             +- gamma line one\n\
             +- gamma line two\n\
             +- gamma line three\n",
        );
        let mut assignments = vec![nat_assignment("foo.rs", post_anchor, post_diff)];
        let outcomes = apply_overrides_to_assignments(&mut assignments, &[ov]);
        let migrated_list = match &outcomes[0] {
            OverrideOutcome::Migrated(m) => m.clone(),
            other => panic!("expected Migrated, got {other:?}"),
        };
        assert_eq!(migrated_list.len(), 1);
        let migrated = &migrated_list[0];
        assert_eq!(migrated.anchor, post_anchor);
        // Two surviving residual ranges; intro at the top and Section
        // B+C at the bottom. Section A's range is dropped (rows now
        // context). Crucially they must be disjoint and properly
        // ordered — not the buggy [(0,4), (1,19)] collapse.
        let new_ranges = &migrated.ranges;
        assert_eq!(new_ranges.len(), 2, "got {new_ranges:?}");
        // First range covers the intro adds (rows 0..4 in new diff).
        assert_eq!(new_ranges[0], RowRange { start: 0, end: 5 });
        // Second range covers Section B+C adds (rows 9..18 in new diff).
        assert_eq!(new_ranges[1], RowRange { start: 9, end: 19 });
    }

    /// Companion to `migration_splits_across_multiple_candidates_after_partial_commit`:
    /// verify the store-side rekeying end-to-end. Single input override at
    /// the pre-commit anchor; two output overrides at the two post-commit
    /// anchors.
    #[test]
    fn reconcile_rekeys_store_into_multiple_entries_after_split() {
        let gitdir = std::path::Path::new("/test/gitdir/reconcile-rekey-multi");
        clear_store_for_tests_at(Some(gitdir));

        let pre_anchor = HunkHeader {
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 9,
        };
        let pre_diff = BString::from(
            "@@ -1,0 +1,9 @@\n+A\n+B\n+C\n+D\n+E\n+F\n+G\n+H\n+I\n",
        );
        upsert_override(
            gitdir,
            make_stored_override(
                pre_anchor,
                pre_diff,
                vec![RowRange { start: 4, end: 5 }],
                BTreeMap::new(),
            ),
        );

        let h1 = HunkHeader { old_start: 1, old_lines: 0, new_start: 1, new_lines: 4 };
        let h2 = HunkHeader { old_start: 2, old_lines: 0, new_start: 6, new_lines: 4 };
        let mut assignments = vec![
            nat_assignment(
                "foo.rs",
                h1,
                BString::from("@@ -1,0 +1,4 @@\n+A\n+B\n+C\n+D\n"),
            ),
            nat_assignment(
                "foo.rs",
                h2,
                BString::from("@@ -2,0 +6,4 @@\n+F\n+G\n+H\n+I\n"),
            ),
        ];
        reconcile_with_overrides(gitdir, &mut assignments);

        let remaining = list_overrides(gitdir);
        assert_eq!(
            remaining.len(),
            2,
            "single pre-anchor override fans out into two post-anchor entries",
        );
        let anchors: Vec<HunkHeader> = remaining.iter().map(|m| m.anchor).collect();
        assert!(anchors.contains(&h1));
        assert!(anchors.contains(&h2));
        // Pre-commit anchor key is gone.
        assert!(
            remaining.iter().all(|m| m.anchor != pre_anchor),
            "old key still present in store",
        );
        clear_store_for_tests_at(Some(gitdir));
    }

    /// Re-splitting a sub-hunk: a user has split a natural hunk into
    /// (0,5)/(5,15), then re-splits within (5,15) at row 10. The new
    /// partition should be (0,5)/(5,10)/(10,15) — the existing range
    /// containing the new boundary is split, the rest is preserved.
    #[test]
    fn merge_user_ranges_splits_at_boundary() {
        let existing = vec![
            RowRange { start: 0, end: 5 },
            RowRange { start: 5, end: 15 },
        ];
        let new_ranges = vec![RowRange { start: 10, end: 11 }];
        let merged = merge_user_ranges_into_partition(&existing, &new_ranges);
        assert_eq!(
            merged,
            vec![
                RowRange { start: 0, end: 5 },
                RowRange { start: 5, end: 10 },
                RowRange { start: 10, end: 11 },
                RowRange { start: 11, end: 15 },
            ],
        );
    }

    /// Re-splitting where the new range covers more than a single point:
    /// the existing partition is sliced into pre-overlap, overlap, and
    /// post-overlap segments around the new range.
    #[test]
    fn merge_user_ranges_carves_out_a_span() {
        let existing = vec![RowRange { start: 0, end: 20 }];
        let new_ranges = vec![RowRange { start: 7, end: 12 }];
        let merged = merge_user_ranges_into_partition(&existing, &new_ranges);
        assert_eq!(
            merged,
            vec![
                RowRange { start: 0, end: 7 },
                RowRange { start: 7, end: 12 },
                RowRange { start: 12, end: 20 },
            ],
        );
    }

    /// New range that exactly matches an existing range boundary is a
    /// no-op (already represented).
    #[test]
    fn merge_user_ranges_no_op_when_already_aligned() {
        let existing = vec![
            RowRange { start: 0, end: 5 },
            RowRange { start: 5, end: 15 },
        ];
        let new_ranges = vec![RowRange { start: 0, end: 5 }];
        let merged = merge_user_ranges_into_partition(&existing, &new_ranges);
        assert_eq!(merged, existing);
    }

    /// Empty `new_user_ranges` returns the existing partition unchanged.
    #[test]
    fn merge_user_ranges_empty_input_passthrough() {
        let existing = vec![
            RowRange { start: 0, end: 5 },
            RowRange { start: 5, end: 15 },
        ];
        let merged = merge_user_ranges_into_partition(&existing, &[]);
        assert_eq!(merged, existing);
    }

    /// Phase 6.5a (line-by-line commits): the override store needs to
    /// be serializable so it can be persisted to disk in `but-db`.
    /// Verifies that round-tripping a fully-populated `SubHunkOverride`
    /// through JSON is lossless.
    #[test]
    fn sub_hunk_override_serde_round_trip() {
        let kinds = parse_row_kinds(&sample_diff());
        let user_range = RowRange { start: 1, end: 5 };
        let ranges = materialize_residual_ranges(&[user_range], &kinds);
        let stack_id = uuid::Uuid::new_v4();
        let mut assignments: BTreeMap<RowRange, HunkAssignmentTarget> = BTreeMap::new();
        assignments.insert(
            user_range,
            HunkAssignmentTarget::Stack {
                stack_id: stack_id.into(),
            },
        );
        let path = BString::from("src/foo.rs");
        let original = SubHunkOverride {
            origin: SubHunkOriginLocation::worktree(path.clone()),
            path,
            anchor: anchor(),
            ranges,
            assignments,
            rows: kinds,
            anchor_diff: sample_diff(),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let restored: SubHunkOverride =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.origin, original.origin);
        assert_eq!(restored.path, original.path);
        assert_eq!(restored.anchor, original.anchor);
        assert_eq!(restored.ranges, original.ranges);
        assert_eq!(restored.rows.len(), original.rows.len());
        for (a, b) in restored.rows.iter().zip(original.rows.iter()) {
            assert_eq!(a, b);
        }
        assert_eq!(restored.anchor_diff, original.anchor_diff);
        assert_eq!(restored.assignments.len(), original.assignments.len());
        let restored_target = restored
            .assignments
            .get(&user_range)
            .expect("per-range assignment preserved");
        match (restored_target, original.assignments.get(&user_range).unwrap()) {
            (
                HunkAssignmentTarget::Stack { stack_id: a },
                HunkAssignmentTarget::Stack { stack_id: b },
            ) => assert_eq!(a, b),
            _ => panic!("target kind drifted across round-trip"),
        }
    }

    // -----------------------------------------------------------------
    // Phase 6.5c–e tests: bridge + DB hydration / write-through.
    // -----------------------------------------------------------------

    fn fresh_gitdir(tag: &str) -> PathBuf {
        // Pick a unique pseudo-gitdir per test so the global in-memory
        // store entries don't collide between parallel tests.
        PathBuf::from(format!(
            "/tmp/sub_hunk_persist_test/{}/{}.git",
            tag,
            uuid::Uuid::new_v4()
        ))
    }

    fn fresh_db() -> but_db::DbHandle {
        but_db::DbHandle::new_at_path(":memory:").expect("in-memory db")
    }

    fn sample_override() -> SubHunkOverride {
        let kinds = parse_row_kinds(&sample_diff());
        let user_range = RowRange { start: 1, end: 5 };
        let ranges = materialize_residual_ranges(&[user_range], &kinds);
        let mut assignments: BTreeMap<RowRange, HunkAssignmentTarget> = BTreeMap::new();
        assignments.insert(
            user_range,
            HunkAssignmentTarget::Stack {
                stack_id: uuid::Uuid::new_v4().into(),
            },
        );
        let path = BString::from("src/foo.rs");
        SubHunkOverride {
            origin: SubHunkOriginLocation::worktree(path.clone()),
            path,
            anchor: anchor(),
            ranges,
            assignments,
            rows: kinds,
            anchor_diff: sample_diff(),
        }
    }

    #[test]
    fn db_row_round_trip_preserves_override() {
        let gitdir = fresh_gitdir("round-trip");
        let original = sample_override();
        let row = to_db_row(&gitdir, &original)
            .expect("to_db_row")
            .expect("row not refused for size");
        let restored = from_db_row(row).expect("from_db_row");

        assert_eq!(restored.path, original.path);
        assert_eq!(restored.anchor, original.anchor);
        assert_eq!(restored.ranges, original.ranges);
        assert_eq!(restored.rows, original.rows);
        assert_eq!(restored.anchor_diff, original.anchor_diff);
        assert_eq!(restored.assignments.len(), original.assignments.len());
        for (k, v) in &original.assignments {
            let got = restored.assignments.get(k).expect("key preserved");
            match (v, got) {
                (
                    HunkAssignmentTarget::Stack { stack_id: a },
                    HunkAssignmentTarget::Stack { stack_id: b },
                ) => assert_eq!(a, b),
                _ => panic!("target kind drifted"),
            }
        }
    }

    #[test]
    fn from_db_row_rejects_unknown_schema_version() {
        let gitdir = fresh_gitdir("schema-version");
        let mut row = to_db_row(&gitdir, &sample_override()).unwrap().unwrap();
        row.schema_version = 999;
        let err = from_db_row(row).expect_err("future schema rejected");
        let msg = format!("{err}");
        assert!(msg.contains("schema_version"), "got: {msg}");
    }

    #[test]
    fn to_db_row_size_guard_drops_oversize() {
        let gitdir = fresh_gitdir("size-guard");
        let mut ov = sample_override();
        ov.anchor_diff = BString::from(vec![b'x'; MAX_OVERRIDE_DB_BYTES + 1]);
        let result = to_db_row(&gitdir, &ov).expect("to_db_row");
        assert!(result.is_none(), "oversize override should refuse to persist");
    }

    #[test]
    fn upsert_override_persistent_writes_through() {
        let gitdir = fresh_gitdir("upsert");
        clear_store_for_tests_at(Some(&gitdir));
        let mut db = fresh_db();
        let ov = sample_override();
        let persisted = upsert_override_persistent(&mut db, &gitdir, ov.clone()).unwrap();
        assert!(persisted);

        let key = gitdir_key(&gitdir);
        let rows = db.sub_hunk_overrides().list_for_gitdir(&key).unwrap();
        assert_eq!(rows.len(), 1);

        // In-memory store updated as well.
        let in_mem = list_overrides(&gitdir);
        assert_eq!(in_mem.len(), 1);
        assert_eq!(in_mem[0].anchor, ov.anchor);
        clear_store_for_tests_at(Some(&gitdir));
    }

    #[test]
    fn upsert_override_persistent_drops_disk_when_oversize() {
        let gitdir = fresh_gitdir("upsert-oversize");
        clear_store_for_tests_at(Some(&gitdir));
        let mut db = fresh_db();

        // First insert a small row so there's something to clean up.
        let small = sample_override();
        upsert_override_persistent(&mut db, &gitdir, small.clone()).unwrap();
        let key = gitdir_key(&gitdir);
        assert_eq!(db.sub_hunk_overrides().list_for_gitdir(&key).unwrap().len(), 1);

        // Now upsert an oversized version of the same anchor: in-memory
        // takes it, on-disk row is deleted.
        let mut big = small;
        big.anchor_diff = BString::from(vec![b'x'; MAX_OVERRIDE_DB_BYTES + 1]);
        let persisted = upsert_override_persistent(&mut db, &gitdir, big).unwrap();
        assert!(!persisted);
        assert!(db.sub_hunk_overrides().list_for_gitdir(&key).unwrap().is_empty());
        clear_store_for_tests_at(Some(&gitdir));
    }

    #[test]
    fn remove_override_persistent_clears_both_layers() {
        let gitdir = fresh_gitdir("remove");
        clear_store_for_tests_at(Some(&gitdir));
        let mut db = fresh_db();
        let ov = sample_override();
        upsert_override_persistent(&mut db, &gitdir, ov.clone()).unwrap();

        let removed =
            remove_override_persistent(&mut db, &gitdir, &ov.path, ov.anchor).unwrap();
        assert!(removed.is_some());
        let key = gitdir_key(&gitdir);
        assert!(db.sub_hunk_overrides().list_for_gitdir(&key).unwrap().is_empty());
        assert!(list_overrides(&gitdir).is_empty());
        clear_store_for_tests_at(Some(&gitdir));
    }

    #[test]
    fn drop_overrides_persistent_clears_both_layers() {
        let gitdir = fresh_gitdir("drop");
        clear_store_for_tests_at(Some(&gitdir));
        let mut db = fresh_db();
        let ov = sample_override();
        upsert_override_persistent(&mut db, &gitdir, ov.clone()).unwrap();

        drop_overrides_persistent(
            &mut db,
            &gitdir,
            &[(ov.path.clone(), ov.anchor)],
        )
        .unwrap();
        let key = gitdir_key(&gitdir);
        assert!(db.sub_hunk_overrides().list_for_gitdir(&key).unwrap().is_empty());
        assert!(list_overrides(&gitdir).is_empty());
        clear_store_for_tests_at(Some(&gitdir));
    }

    #[test]
    fn hydrate_from_db_rebuilds_in_memory_store() {
        let gitdir = fresh_gitdir("hydrate");
        clear_store_for_tests_at(Some(&gitdir));
        let mut db = fresh_db();
        let ov = sample_override();
        upsert_override_persistent(&mut db, &gitdir, ov.clone()).unwrap();

        // Wipe the in-memory store, simulating a fresh process.
        clear_store_for_tests_at(Some(&gitdir));
        assert!(list_overrides(&gitdir).is_empty());

        let n = hydrate_from_db(&db, &gitdir).unwrap();
        assert_eq!(n, 1);
        let restored = list_overrides(&gitdir);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].anchor, ov.anchor);
        assert_eq!(restored[0].path, ov.path);
        assert_eq!(restored[0].ranges, ov.ranges);
        assert_eq!(restored[0].anchor_diff, ov.anchor_diff);
        clear_store_for_tests_at(Some(&gitdir));
    }

    #[test]
    fn ensure_hydrated_runs_once_per_gitdir() {
        let gitdir = fresh_gitdir("ensure-once");
        clear_store_for_tests_at(Some(&gitdir));
        clear_hydrated_for_tests(&gitdir);
        let mut db = fresh_db();
        let ov = sample_override();
        upsert_override_persistent(&mut db, &gitdir, ov.clone()).unwrap();

        // Simulate a fresh process: drop the in-memory store, but leave
        // the row sitting on disk. Calling `ensure_hydrated` should pull
        // it back; a second call must be a no-op (we tampered with the
        // DB to detect re-hydration).
        clear_store_for_tests_at(Some(&gitdir));
        clear_hydrated_for_tests(&gitdir);

        ensure_hydrated(&db, &gitdir);
        assert_eq!(list_overrides(&gitdir).len(), 1);

        // Wipe the in-memory store and add a poison row to the DB. If
        // `ensure_hydrated` runs again it would surface the poison; the
        // once-per-gitdir guard means it must not.
        clear_store_for_tests_at(Some(&gitdir));
        let key = gitdir_key(&gitdir);
        db.sub_hunk_overrides_mut()
            .upsert(but_db::SubHunkOverrideRow {
                gitdir: key.clone(),
                path: b"src/poison.rs".to_vec(),
                anchor_old_start: 99,
                anchor_old_lines: 1,
                anchor_new_start: 99,
                anchor_new_lines: 1,
                commit_id: Vec::new(),
                ranges_json: "[]".to_string(),
                assignments_json: "[]".to_string(),
                rows_json: "[]".to_string(),
                anchor_diff: b"@@ -99 +99 @@\n".to_vec(),
                schema_version: OVERRIDE_DB_SCHEMA_VERSION,
            })
            .unwrap();
        ensure_hydrated(&db, &gitdir);
        assert!(
            list_overrides(&gitdir).is_empty(),
            "second ensure_hydrated must be a no-op"
        );
        clear_store_for_tests_at(Some(&gitdir));
        clear_hydrated_for_tests(&gitdir);
    }

    #[test]
    fn hydrate_from_db_skips_malformed_rows() {
        let gitdir = fresh_gitdir("hydrate-malformed");
        clear_store_for_tests_at(Some(&gitdir));
        let mut db = fresh_db();

        // Insert a row that we know will fail to deserialize (bogus JSON).
        let key = gitdir_key(&gitdir);
        db.sub_hunk_overrides_mut()
            .upsert(but_db::SubHunkOverrideRow {
                gitdir: key.clone(),
                path: b"src/bad.rs".to_vec(),
                anchor_old_start: 1,
                anchor_old_lines: 1,
                anchor_new_start: 1,
                anchor_new_lines: 1,
                commit_id: Vec::new(),
                ranges_json: "not-json".to_string(),
                assignments_json: "[]".to_string(),
                rows_json: "[]".to_string(),
                anchor_diff: b"@@ -1 +1 @@\n".to_vec(),
                schema_version: OVERRIDE_DB_SCHEMA_VERSION,
            })
            .unwrap();

        // And a valid row alongside it.
        let ov = sample_override();
        let row = to_db_row(&gitdir, &ov).unwrap().unwrap();
        db.sub_hunk_overrides_mut().upsert(row).unwrap();

        let n = hydrate_from_db(&db, &gitdir).unwrap();
        assert_eq!(n, 1, "only the well-formed row hydrates");
        let in_mem = list_overrides(&gitdir);
        assert_eq!(in_mem.len(), 1);
        assert_eq!(in_mem[0].anchor, ov.anchor);
        clear_store_for_tests_at(Some(&gitdir));
    }

    // ---------------------------------------------------------------
    // Phase 7a — SubHunkOriginLocation enum smoke tests.
    //
    // The widened store key is purely structural in 7a (nothing
    // constructs `Commit` variants yet), so the meaningful coverage
    // is the enum's own contract: equality, hashing, accessors,
    // serde round-trip. Phase 7c adds end-to-end coverage that two
    // overrides on the same `(path, anchor)` but different
    // `commit_id`s coexist in the store.
    // ---------------------------------------------------------------

    #[test]
    fn origin_location_worktree_and_commit_have_distinct_keys() {
        use std::collections::HashSet;
        let path = BString::from("src/foo.rs");
        let commit = gix::ObjectId::from_hex(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .unwrap();

        let wt = SubHunkOriginLocation::worktree(path.clone());
        let cm = SubHunkOriginLocation::commit(commit, path.clone());

        assert_ne!(wt, cm);
        assert_eq!(wt.path(), &path);
        assert_eq!(cm.path(), &path);
        assert!(wt.is_worktree());
        assert!(!cm.is_worktree());
        assert_eq!(wt.commit_id(), None);
        assert_eq!(cm.commit_id(), Some(commit));

        // HashMap key disambiguation: same path, same anchor, but
        // different origin must produce two distinct keys.
        let anchor = HunkHeader {
            old_start: 1,
            old_lines: 2,
            new_start: 1,
            new_lines: 3,
        };
        let mut keys: HashSet<StoreKey> = HashSet::new();
        keys.insert((wt, anchor));
        keys.insert((cm, anchor));
        assert_eq!(keys.len(), 2);
    }

    #[test]
    fn origin_location_serde_round_trip() {
        let path = BString::from("src/foo.rs");
        let commit = gix::ObjectId::from_hex(b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .unwrap();

        for original in [
            SubHunkOriginLocation::worktree(path.clone()),
            SubHunkOriginLocation::commit(commit, path.clone()),
        ] {
            let json = serde_json::to_string(&original).unwrap();
            let restored: SubHunkOriginLocation = serde_json::from_str(&json).unwrap();
            assert_eq!(restored, original);
            assert_eq!(restored.path(), original.path());
            assert_eq!(restored.commit_id(), original.commit_id());
        }
    }

    #[test]
    fn worktree_keyed_overrides_round_trip_through_store() {
        // Pre-existing call sites use `(path, anchor)`-shaped public
        // APIs which 7a routes through the worktree variant of the
        // new enum key. This pins that the routing is in fact a
        // no-op on observable behavior.
        let gitdir = std::path::PathBuf::from(format!(
            "/tmp/gitbutler-test-7a-{}",
            uuid::Uuid::new_v4()
        ));
        clear_store_for_tests_at(Some(&gitdir));

        let ov = make_stored_override(
            anchor(),
            sample_diff(),
            vec![RowRange { start: 2, end: 4 }],
            BTreeMap::new(),
        );
        let path = ov.path.clone();
        let anchor_h = ov.anchor;

        upsert_override(&gitdir, ov.clone());
        let got = get_override(&gitdir, &path, anchor_h)
            .expect("override should be retrievable via worktree key");
        assert_eq!(got.path, ov.path);
        assert_eq!(got.anchor, ov.anchor);

        let removed = remove_override(&gitdir, &path, anchor_h)
            .expect("override should be removable via worktree key");
        assert_eq!(removed.anchor, ov.anchor);
        assert!(get_override(&gitdir, &path, anchor_h).is_none());

        clear_store_for_tests_at(Some(&gitdir));
    }

    // ---------------------------------------------------------------
    // Phase 7b — commit-anchored override query helpers + worktree
    // reconcile isolation.
    //
    // 7b adds `upsert_override_at`, `list_commit_overrides`, and
    // `get_commit_override`, plus filters the worktree reconcile
    // pass so commit-anchored overrides aren't subjected to
    // worktree-shape migration / drop. These tests pin both shapes.
    // ---------------------------------------------------------------

    fn fixed_commit(hex_byte: u8) -> gix::ObjectId {
        let s = std::iter::repeat_n(hex_byte, 40).collect::<Vec<_>>();
        gix::ObjectId::from_hex(&s).unwrap()
    }

    #[test]
    fn commit_anchored_overrides_are_isolated_from_worktree_lookups() {
        let gitdir = std::path::PathBuf::from(format!(
            "/tmp/gitbutler-test-7b-isol-{}",
            uuid::Uuid::new_v4()
        ));
        clear_store_for_tests_at(Some(&gitdir));

        // `make_stored_override` hardcodes path "foo.rs"; reuse it
        // so the location.path() / ov.path agreement holds.
        let path = BString::from("foo.rs");
        let anchor_h = anchor();
        let commit = fixed_commit(b'a');

        // Insert a worktree-anchored and a commit-anchored override
        // on the *same* (path, anchor). Both should coexist.
        let wt_ov = make_stored_override(
            anchor_h,
            sample_diff(),
            vec![RowRange { start: 2, end: 4 }],
            BTreeMap::new(),
        );
        let cm_ov = make_stored_override(
            anchor_h,
            sample_diff(),
            vec![RowRange { start: 1, end: 3 }],
            BTreeMap::new(),
        );

        upsert_override(&gitdir, wt_ov.clone());
        upsert_override_at(
            &gitdir,
            SubHunkOriginLocation::commit(commit, path.clone()),
            cm_ov.clone(),
        );

        // Worktree lookup returns the worktree-keyed override only.
        let got_wt = get_override(&gitdir, &path, anchor_h).unwrap();
        assert_eq!(got_wt.ranges, wt_ov.ranges);

        // Commit lookup returns the commit-keyed override only.
        let got_cm = get_commit_override(&gitdir, commit, &path, anchor_h).unwrap();
        assert_eq!(got_cm.ranges, cm_ov.ranges);

        // list_*_overrides filters by origin.
        assert_eq!(list_worktree_overrides(&gitdir).len(), 1);
        assert_eq!(list_commit_overrides(&gitdir, commit).len(), 1);

        // A different commit_id sees no commit-keyed overrides.
        assert!(list_commit_overrides(&gitdir, fixed_commit(b'b')).is_empty());
        assert!(get_commit_override(&gitdir, fixed_commit(b'b'), &path, anchor_h).is_none());

        // The unfiltered `list_overrides` still returns both for
        // diagnostics' sake.
        assert_eq!(list_overrides(&gitdir).len(), 2);

        clear_store_for_tests_at(Some(&gitdir));
    }

    #[test]
    fn worktree_reconcile_does_not_drop_commit_anchored_overrides() {
        // The latent bug 7b fixes: pre-7b, `reconcile_with_overrides`
        // would call `list_overrides`, which returned commit-keyed
        // overrides too. Those would then be matched against
        // `assignments` (worktree-derived), fail to find a match,
        // and either be dropped or migrated against worktree
        // shape — silently corrupting commit-side state.
        let gitdir = std::path::PathBuf::from(format!(
            "/tmp/gitbutler-test-7b-reconcile-{}",
            uuid::Uuid::new_v4()
        ));
        clear_store_for_tests_at(Some(&gitdir));

        let path = BString::from("foo.rs");
        let commit = fixed_commit(b'c');
        let cm_ov = make_stored_override(
            anchor(),
            sample_diff(),
            vec![RowRange { start: 2, end: 4 }],
            BTreeMap::new(),
        );
        upsert_override_at(
            &gitdir,
            SubHunkOriginLocation::commit(commit, path.clone()),
            cm_ov.clone(),
        );

        // Run a worktree reconcile against an empty assignments list.
        // The commit-anchored override must survive untouched.
        let mut assignments: Vec<HunkAssignment> = Vec::new();
        reconcile_with_overrides(&gitdir, &mut assignments);

        let still_there = get_commit_override(&gitdir, commit, &path, anchor())
            .expect("commit-anchored override must survive worktree reconcile");
        assert_eq!(still_there.ranges, cm_ov.ranges);

        // And a second reconcile remains a no-op for the commit-side.
        reconcile_with_overrides(&gitdir, &mut assignments);
        assert!(get_commit_override(&gitdir, commit, &path, anchor()).is_some());

        clear_store_for_tests_at(Some(&gitdir));
    }

    #[test]
    fn upsert_override_at_debug_asserts_path_consistency() {
        // The path inside the location must match `ov.path`. We
        // can only assert the agreement path actually works; the
        // mismatch case fires `debug_assert_eq!` which we don't
        // want to provoke from tests.
        let gitdir = std::path::PathBuf::from(format!(
            "/tmp/gitbutler-test-7b-pathok-{}",
            uuid::Uuid::new_v4()
        ));
        clear_store_for_tests_at(Some(&gitdir));

        let path = BString::from("foo.rs");
        let commit = fixed_commit(b'd');
        let ov = make_stored_override(
            anchor(),
            sample_diff(),
            vec![RowRange { start: 2, end: 4 }],
            BTreeMap::new(),
        );

        upsert_override_at(
            &gitdir,
            SubHunkOriginLocation::commit(commit, path.clone()),
            ov,
        );
        assert!(get_commit_override(&gitdir, commit, &path, anchor()).is_some());
        clear_store_for_tests_at(Some(&gitdir));
    }

    // ---------------------------------------------------------------
    // Phase 7c-1 — SubHunkOverride.origin field is the authoritative
    // store key.
    // ---------------------------------------------------------------

    #[test]
    fn upsert_override_routes_through_origin_field() {
        // Construct an override with a `Commit`-shaped origin and
        // call the bare `upsert_override` (no `_at` variant). It
        // must land under the commit-keyed slot, not under the
        // worktree-keyed one implied by `path`.
        let gitdir = std::path::PathBuf::from(format!(
            "/tmp/gitbutler-test-7c1-origin-{}",
            uuid::Uuid::new_v4()
        ));
        clear_store_for_tests_at(Some(&gitdir));

        let commit = fixed_commit(b'e');
        let mut ov = make_stored_override(
            anchor(),
            sample_diff(),
            vec![RowRange { start: 2, end: 4 }],
            BTreeMap::new(),
        );
        ov.origin = SubHunkOriginLocation::commit(commit, ov.path.clone());
        upsert_override(&gitdir, ov.clone());

        // Worktree lookup misses; commit lookup hits.
        assert!(get_override(&gitdir, &ov.path, anchor()).is_none());
        assert!(get_commit_override(&gitdir, commit, &ov.path, anchor()).is_some());

        clear_store_for_tests_at(Some(&gitdir));
    }

    #[test]
    fn upsert_override_at_overrides_origin_field_for_storage() {
        // Even if `ov.origin` is stale, `upsert_override_at`
        // overwrites it with `location` so the stored value's
        // `origin` matches the key it was filed under.
        let gitdir = std::path::PathBuf::from(format!(
            "/tmp/gitbutler-test-7c1-at-{}",
            uuid::Uuid::new_v4()
        ));
        clear_store_for_tests_at(Some(&gitdir));

        let commit = fixed_commit(b'f');
        let ov = make_stored_override(
            anchor(),
            sample_diff(),
            vec![RowRange { start: 2, end: 4 }],
            BTreeMap::new(),
        );
        // ov.origin is Worktree by construction; upsert_override_at
        // should rewrite it to Commit.
        let original_origin = ov.origin.clone();
        assert!(matches!(
            original_origin,
            SubHunkOriginLocation::Worktree { .. }
        ));
        upsert_override_at(
            &gitdir,
            SubHunkOriginLocation::commit(commit, ov.path.clone()),
            ov.clone(),
        );
        let stored = get_commit_override(&gitdir, commit, &ov.path, anchor())
            .expect("stored under commit key");
        assert_eq!(
            stored.origin,
            SubHunkOriginLocation::commit(commit, ov.path.clone()),
            "upsert_override_at must rewrite ov.origin to match location"
        );

        clear_store_for_tests_at(Some(&gitdir));
    }

    #[test]
    fn commit_keyed_override_round_trips_through_db_bridge() {
        // Phase 7c-2: build a Commit-keyed override, push it through
        // `to_db_row` → DB → `from_db_row`, and verify the origin
        // survives. The fixed_commit() helper produces a known
        // ObjectId; the bridge must preserve its raw bytes.
        let gitdir = std::path::PathBuf::from(format!(
            "/tmp/gitbutler-test-7c2-{}",
            uuid::Uuid::new_v4()
        ));
        clear_store_for_tests_at(Some(&gitdir));

        let commit = fixed_commit(b'5');
        let path = BString::from("foo.rs");
        let mut ov = make_stored_override(
            anchor(),
            sample_diff(),
            vec![RowRange { start: 2, end: 4 }],
            BTreeMap::new(),
        );
        ov.origin = SubHunkOriginLocation::commit(commit, path.clone());

        let row = to_db_row(&gitdir, &ov)
            .expect("to_db_row")
            .expect("row not size-guarded");
        assert_eq!(
            row.commit_id,
            commit.as_bytes().to_vec(),
            "commit_id encoded as raw bytes"
        );
        assert_eq!(row.schema_version, OVERRIDE_DB_SCHEMA_VERSION);

        let restored = from_db_row(row).expect("from_db_row");
        assert_eq!(
            restored.origin,
            SubHunkOriginLocation::commit(commit, path.clone()),
            "origin survives the round trip"
        );
        assert_eq!(restored.path, path);
        assert_eq!(restored.anchor, anchor());

        clear_store_for_tests_at(Some(&gitdir));
    }

    #[test]
    fn worktree_keyed_override_encodes_empty_commit_id() {
        // Symmetric to the test above: worktree-anchored overrides
        // encode an empty `commit_id` blob.
        let gitdir = std::path::PathBuf::from(format!(
            "/tmp/gitbutler-test-7c2-wt-{}",
            uuid::Uuid::new_v4()
        ));
        clear_store_for_tests_at(Some(&gitdir));
        let ov = make_stored_override(
            anchor(),
            sample_diff(),
            vec![RowRange { start: 2, end: 4 }],
            BTreeMap::new(),
        );
        let row = to_db_row(&gitdir, &ov).unwrap().unwrap();
        assert!(row.commit_id.is_empty(), "worktree → empty commit_id");
        let restored = from_db_row(row).unwrap();
        assert!(matches!(
            restored.origin,
            SubHunkOriginLocation::Worktree { .. }
        ));
        clear_store_for_tests_at(Some(&gitdir));
    }

    #[test]
    fn sub_hunk_override_serde_default_origin_for_legacy_snapshots() {
        // Older snapshots (pre-7c-1) lack the `origin` field. Verify
        // that `#[serde(default)]` fills it in (so an old in-memory
        // dump deserializes), and that the default doesn't poison
        // anything — callers downstream re-derive a real origin
        // before storage when they have one.
        let legacy_json = r#"{
            "path": [115, 114, 99, 47, 102, 111, 111, 46, 114, 115],
            "anchor": {
                "oldStart": 10, "oldLines": 5,
                "newStart": 10, "newLines": 5
            },
            "ranges": [],
            "assignments": [],
            "rows": [],
            "anchorDiff": []
        }"#;
        let restored: SubHunkOverride = serde_json::from_str(legacy_json)
            .expect("legacy snapshot without `origin` deserializes");
        // Default origin is an empty-path Worktree variant; the
        // hydration / read path is responsible for filling it in
        // with the real path from the row.
        assert!(matches!(
            restored.origin,
            SubHunkOriginLocation::Worktree { .. }
        ));
    }
}
