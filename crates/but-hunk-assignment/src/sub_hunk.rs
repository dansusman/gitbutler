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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
/// `ranges` are the user-carved sub-ranges. Rows of the anchor not covered by
/// any range are *residuals* — emitted as their own sub-hunks but inheriting
/// the anchor's pre-split assignment.
#[derive(Debug, Clone)]
pub struct SubHunkOverride {
    pub path: BString,
    pub anchor: HunkHeader,
    /// Sorted, disjoint, non-empty, all contained within the anchor.
    pub ranges: Vec<RowRange>,
    /// Per-range stack reassignment, if any. Ranges absent from this map (and
    /// all residual ranges) inherit the anchor's pre-split assignment.
    pub assignments: BTreeMap<RowRange, HunkAssignmentTarget>,
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
    if total >= row_count {
        bail!("ranges cover the entire anchor; nothing to split out");
    }
    Ok(())
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
    if validate_ranges(&override_.ranges, row_count).is_err() {
        return vec![anchor_assignment.clone()];
    }

    let anchor_branch_ref = anchor_assignment.branch_ref_bytes.clone();

    // Build the full ordered list of sub-ranges: user ranges + residuals.
    let mut emitted: Vec<(RowRange, Option<&HunkAssignmentTarget>)> = Vec::new();
    let mut cursor = 0u32;
    for r in &override_.ranges {
        if r.start > cursor {
            emitted.push((RowRange { start: cursor, end: r.start }, None));
        }
        emitted.push((*r, override_.assignments.get(r)));
        cursor = r.end;
    }
    if cursor < row_count {
        emitted.push((RowRange { start: cursor, end: row_count }, None));
    }

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

/// Apply all `overrides` to `assignments` in place, replacing each matched
/// anchor with its materialized sub-hunks. Overrides whose anchor is not
/// present in `assignments` are silently dropped from the in-memory store at
/// the call site (see [`reconcile_with_overrides`]).
///
/// Returns the indices into `overrides` whose anchors did not match any
/// assignment, so the caller can prune them.
pub fn apply_overrides_to_assignments(
    assignments: &mut Vec<HunkAssignment>,
    overrides: &[SubHunkOverride],
) -> Vec<usize> {
    let mut unmatched = Vec::new();
    for (idx, ov) in overrides.iter().enumerate() {
        let anchor_idx = assignments.iter().position(|a| {
            a.path_bytes == ov.path && a.hunk_header == Some(ov.anchor)
        });
        let Some(i) = anchor_idx else {
            unmatched.push(idx);
            continue;
        };
        let anchor = assignments[i].clone();
        let sub_hunks = materialize_override(&anchor, ov);
        assignments.splice(i..=i, sub_hunks);
    }
    unmatched
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

#[cfg(test)]
pub(crate) fn clear_store_for_tests() {
    let mut store = global_store().lock().expect("override store poisoned");
    store.clear();
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
    let unmatched_idx = apply_overrides_to_assignments(assignments, &overrides);
    if !unmatched_idx.is_empty() {
        let to_drop: Vec<(BString, HunkHeader)> = unmatched_idx
            .into_iter()
            .map(|i| (overrides[i].path.clone(), overrides[i].anchor))
            .collect();
        drop_overrides(gitdir, &to_drop);
    }
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

    #[test]
    fn materialize_override_three_way_split() {
        // anchor body: ctx -r +a -r +a ctx (rows 0..6)
        // user range: rows 2..4 (the +a -r middle pair)
        let ov = SubHunkOverride {
            path: BString::from("foo.rs"),
            anchor: anchor(),
            ranges: vec![RowRange { start: 2, end: 4 }],
            assignments: BTreeMap::new(),
        };
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
        let ov = SubHunkOverride {
            path: BString::from("foo.rs"),
            anchor: anchor(),
            ranges: vec![RowRange { start: 0, end: 3 }],
            assignments: BTreeMap::new(),
        };
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
        let ov = SubHunkOverride {
            path: BString::from("foo.rs"),
            anchor: anchor(),
            ranges: vec![r],
            assignments,
        };
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
        let ov = SubHunkOverride {
            path: BString::from("foo.rs"),
            anchor: stale_anchor,
            ranges: vec![RowRange { start: 0, end: 1 }],
            assignments: BTreeMap::new(),
        };
        let unmatched = apply_overrides_to_assignments(&mut assignments, &[ov]);
        assert_eq!(unmatched, vec![0]);
        assert_eq!(assignments.len(), 1, "anchor not split when override unmatched");
    }

    #[test]
    fn reconcile_with_overrides_prunes_stale_entries() {
        clear_store_for_tests();
        let gitdir = std::path::Path::new("/test/gitdir/reconcile-prune");
        let stale_anchor = HunkHeader {
            old_start: 999,
            old_lines: 1,
            new_start: 999,
            new_lines: 1,
        };
        upsert_override(
            gitdir,
            SubHunkOverride {
                path: BString::from("foo.rs"),
                anchor: stale_anchor,
                ranges: vec![RowRange { start: 0, end: 1 }],
                assignments: BTreeMap::new(),
            },
        );
        upsert_override(
            gitdir,
            SubHunkOverride {
                path: BString::from("foo.rs"),
                anchor: anchor(),
                ranges: vec![RowRange { start: 2, end: 4 }],
                assignments: BTreeMap::new(),
            },
        );
        assert_eq!(list_overrides(gitdir).len(), 2);

        let mut assignments = vec![anchor_assignment()];
        reconcile_with_overrides(gitdir, &mut assignments);

        // Stale override is dropped; live override materialized into 3 sub-hunks.
        assert_eq!(assignments.len(), 3);
        let remaining = list_overrides(gitdir);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].anchor, anchor());

        clear_store_for_tests();
    }
}
