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

type ProjectStore = HashMap<(BString, HunkHeader), SubHunkOverride>;

static GLOBAL_STORE: OnceLock<Mutex<HashMap<PathBuf, ProjectStore>>> = OnceLock::new();

fn global_store() -> &'static Mutex<HashMap<PathBuf, ProjectStore>> {
    GLOBAL_STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// List the current overrides for the project at `gitdir`.
pub fn list_overrides(gitdir: &Path) -> Vec<SubHunkOverride> {
    let store = global_store().lock().expect("override store poisoned");
    store
        .get(gitdir)
        .map(|p| p.values().cloned().collect())
        .unwrap_or_default()
}

/// Insert or replace an override.
pub fn upsert_override(gitdir: &Path, ov: SubHunkOverride) {
    let mut store = global_store().lock().expect("override store poisoned");
    let project = store.entry(gitdir.to_path_buf()).or_default();
    project.insert((ov.path.clone(), ov.anchor), ov);
}

/// Look up a single override by `(gitdir, path, anchor)`. Returns a clone
/// so callers can read without holding the store lock.
pub fn get_override(
    gitdir: &Path,
    path: &BString,
    anchor: HunkHeader,
) -> Option<SubHunkOverride> {
    let store = global_store().lock().expect("override store poisoned");
    store.get(gitdir)?.get(&(path.clone(), anchor)).cloned()
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
pub fn remove_override(
    gitdir: &Path,
    path: &BString,
    anchor: HunkHeader,
) -> Option<SubHunkOverride> {
    let mut store = global_store().lock().expect("override store poisoned");
    let project = store.get_mut(gitdir)?;
    project.remove(&(path.clone(), anchor))
}

/// Drop overrides whose `(path, anchor)` are listed in `keys`.
///
/// Used by [`reconcile_with_overrides`] to silently drop overrides whose
/// anchor no longer matches a natural hunk in the worktree.
pub fn drop_overrides(gitdir: &Path, keys: &[(BString, HunkHeader)]) {
    if keys.is_empty() {
        return;
    }
    let mut store = global_store().lock().expect("override store poisoned");
    if let Some(project) = store.get_mut(gitdir) {
        for key in keys {
            project.remove(key);
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
    project.remove(&(path.clone(), old_anchor));
    for m in migrated {
        project.insert((m.path.clone(), m.anchor), m);
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
    let overrides = list_overrides(gitdir);
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
        SubHunkOverride {
            path: BString::from("foo.rs"),
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
        stale.path = BString::from("missing.rs");
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
        let ov = SubHunkOverride {
            path: BString::from("foo.rs"),
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
        let original = SubHunkOverride {
            path: BString::from("src/foo.rs"),
            anchor: anchor(),
            ranges,
            assignments,
            rows: kinds,
            anchor_diff: sample_diff(),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let restored: SubHunkOverride =
            serde_json::from_str(&json).expect("deserialize");
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
}
