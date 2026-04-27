use anyhow::Context as _;
use bstr::{BStr, BString, ByteSlice};

use crate::HunkHeader;

/// Given an `old_image` and a `new_image`, along with `hunks` that represent selections in `new_image`, apply these
/// hunks to `old_image` and return the newly constructed image.
/// This works like an overlay where selections from `new_image` are inserted into `new_image` with `hunks` as Windows,
/// and selections in `old_image` are discarded.
///
/// Note that we assume that both images are human-readable because we assume lines to be present,
/// either with Windows or Unix newlines, and we assume that the hunks match up with these lines.
/// This constraint means that the tokens used for diffing are the same lines.
pub fn apply_hunks(
    old_image: &BStr,
    new_image: &BStr,
    hunks: &[HunkHeader],
) -> anyhow::Result<BString> {
    let mut old_cursor = 1; /* 1-based counting */
    let mut old_iter = old_image.lines_with_terminator();
    let mut new_cursor = 1; /* 1-based counting */
    let mut new_iter = new_image.lines_with_terminator();
    let mut result_image: BString = Vec::with_capacity(old_image.len().max(new_image.len())).into();

    // To each selected hunk, put the old-lines into a buffer.
    // Skip over the old hunk in old hunk in old lines.
    // Skip all new lines till the beginning of the new hunk.
    // Write the new hunk.
    // Repeat for each hunk, and write all remaining old lines.
    for selected_hunk in hunks {
        let old_skips = (selected_hunk.old_start as usize)
            .checked_sub(old_cursor)
            .with_context(|| {
                format!(
                    "`old_skips = start({start}) - cursor({old_cursor})` mut be >= 0, hunk = {selected_hunk:?}",
                    start = selected_hunk.old_start
                )
            })?;
        let catchup_base_lines = old_iter.by_ref().take(old_skips);
        for old_line in catchup_base_lines {
            result_image.extend_from_slice(old_line);
        }
        let _consume_old_hunk_to_replace_with_new = old_iter
            .by_ref()
            .take(selected_hunk.old_lines as usize)
            .count();
        old_cursor += old_skips + selected_hunk.old_lines as usize;

        let new_skips = (selected_hunk.new_start as usize)
            .checked_sub(new_cursor)
            .context("hunks for new lines must be in order")?;
        if selected_hunk.new_lines == 0 {
            let _explicit_skips = new_iter.by_ref().take(new_skips).count();
        } else {
            let new_hunk_lines = new_iter
                .by_ref()
                .skip(new_skips)
                .take(selected_hunk.new_lines as usize);
            for new_line in new_hunk_lines {
                result_image.extend_from_slice(new_line);
            }
        }
        new_cursor += new_skips + selected_hunk.new_lines as usize;
    }

    for line in old_iter {
        result_image.extend_from_slice(line);
    }
    Ok(result_image)
}

/// The range of a hunk as denoted by a 1-based starting line, and the amount of lines from there.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct HunkRange {
    /// The number of the first line in the hunk, 1 based.
    pub start: u32,
    /// The amount of lines in the range.
    ///
    /// If `0`, this is an empty hunk.
    pub lines: u32,
}

// TODO: one day make this struct use `HunkRange` instead of loose fields.
impl HunkHeader {
    /// Return our old-range as self-contained structure.
    pub fn old_range(&self) -> HunkRange {
        HunkRange {
            start: self.old_start,
            lines: self.old_lines,
        }
    }

    /// Return our new-range as self-contained structure.
    pub fn new_range(&self) -> HunkRange {
        HunkRange {
            start: self.new_start,
            lines: self.new_lines,
        }
    }

    /// Return `true` if this hunk is fully contained in the other hunk.
    pub fn contains(self, other: HunkHeader) -> bool {
        self.old_range().contains(other.old_range()) && self.new_range().contains(other.new_range())
    }
}

impl HunkRange {
    /// Calculate the line number that is one past of what we include, i.e. the first excluded line number.
    pub fn end(&self) -> u32 {
        self.start + self.lines
    }
    /// Calculate line number of the last line.
    pub fn last_line(&self) -> u32 {
        if self.lines == 0 {
            return self.start;
        }
        self.start + self.lines - 1
    }
    /// Return `true` if a hunk with `start` and `lines` is fully contained in this hunk.
    pub fn contains(self, other: HunkRange) -> bool {
        other.start >= self.start && other.end() <= self.end()
    }

    /// Return `true` if this range is equal to or intersects with the other
    /// range.
    pub fn intersects(self, other: HunkRange) -> bool {
        if self.start <= other.start && other.start <= self.last_line() {
            return true;
        }

        if self.start <= other.last_line() && other.last_line() <= self.last_line() {
            return true;
        }

        if other.start <= self.start && self.start <= other.last_line() {
            return true;
        }

        if other.start <= self.last_line() && self.last_line() <= other.last_line() {
            return true;
        }

        false
    }

    /// Return `true` if this range is a null-range, a marker value that doesn't happen.
    pub fn is_null(&self) -> bool {
        self.start == 0 && self.lines == 0
    }
}

#[cfg(test)]
mod test {
    use super::*;

    mod contains {
        use super::*;

        #[test]
        fn contains_returns_true_if_a_smaller_range_is_inside_a_larger_range() {
            let larger = HunkRange {
                start: 1,
                lines: 10,
            };
            let smaller = HunkRange { start: 2, lines: 5 };
            assert!(larger.contains(smaller));
            assert!(!smaller.contains(larger));
        }

        #[test]
        fn contains_returns_true_if_two_equal_ranges() {
            let range = HunkRange {
                start: 1,
                lines: 10,
            };
            assert!(range.contains(range));

            let zero_range = HunkRange { start: 1, lines: 0 };
            assert!(zero_range.contains(zero_range));
        }

        #[test]
        fn a_zero_range_does_not_contain_zero_range_next_to_it() {
            let zero_range = HunkRange { start: 1, lines: 0 };
            let next_to_zero_range = HunkRange { start: 2, lines: 0 };
            assert!(!zero_range.contains(next_to_zero_range));
            assert!(!next_to_zero_range.contains(zero_range));
        }

        #[test]
        fn a_one_range_contains_a_zero_range() {
            let one_range = HunkRange { start: 1, lines: 1 };
            let zero_range = HunkRange { start: 1, lines: 0 };
            assert!(one_range.contains(zero_range));
            assert!(!zero_range.contains(one_range));
        }
    }

    mod intersects {
        use super::*;

        #[test]
        fn intersects_returns_true_if_a_smaller_range_is_inside_a_larger_range() {
            let larger = HunkRange {
                start: 1,
                lines: 10,
            };
            let smaller = HunkRange { start: 2, lines: 5 };
            assert!(larger.intersects(smaller));
            assert!(smaller.intersects(larger));
        }

        #[test]
        fn intersects_returns_true_if_two_equal_ranges() {
            let range = HunkRange {
                start: 1,
                lines: 10,
            };
            assert!(range.intersects(range));

            let zero_range = HunkRange { start: 1, lines: 0 };
            assert!(zero_range.intersects(zero_range));
        }

        #[test]
        fn a_zero_range_does_not_intersects_zero_range_next_to_it() {
            let zero_range = HunkRange { start: 1, lines: 0 };
            let next_to_zero_range = HunkRange { start: 2, lines: 0 };
            assert!(!zero_range.intersects(next_to_zero_range));
            assert!(!next_to_zero_range.intersects(zero_range));
        }

        #[test]
        fn a_one_range_intersects_a_zero_range() {
            let one_range = HunkRange { start: 1, lines: 1 }; // Line 1
            let zero_range = HunkRange { start: 1, lines: 0 }; // No lines
            assert!(one_range.intersects(zero_range));
            assert!(zero_range.intersects(one_range));
        }

        #[test]
        fn a_one_range_intersects_a_zero_range_next_to_it() {
            let one_range = HunkRange { start: 1, lines: 1 }; // Line 1
            let zero_range = HunkRange { start: 2, lines: 0 }; // No lines
            assert!(!one_range.intersects(zero_range));
            assert!(!zero_range.intersects(one_range));
        }

        #[test]
        fn a_one_range_intersects_a_zero_range_before_it() {
            let one_range = HunkRange { start: 1, lines: 1 }; // Line 1
            let zero_range = HunkRange { start: 0, lines: 0 }; // No lines
            assert!(!one_range.intersects(zero_range));
            assert!(!zero_range.intersects(one_range));
        }

        #[test]
        fn ranges_that_are_not_fully_contained_in_each_other_intersects() {
            let left = HunkRange {
                start: 1,
                lines: 10,
            };
            let right = HunkRange {
                start: 10,
                lines: 10,
            };
            assert!(left.intersects(right));
            assert!(right.intersects(left));
        }

        #[test]
        fn ranges_that_are_next_to_each_other_but_not_intersecting() {
            let left = HunkRange {
                start: 1,
                lines: 10,
            };
            let right = HunkRange {
                start: 11,
                lines: 10,
            };
            assert!(!left.intersects(right));
            assert!(!right.intersects(left));
        }
    }

    /// Regression coverage for the multi-pure-add bug observed in the
    /// `splittest_pure_add.md` field repro: when a single worktree hunk
    /// holds multiple disjoint sub-ranges of additions and the user
    /// commits the additions individually, the
    /// `to_additive_hunks` + [`apply_hunks`] pipeline must interleave
    /// the new content correctly with the surrounding old content.
    ///
    /// Each test runs the full pipeline (encoded sub-hunks →
    /// `to_additive_hunks` → `apply_hunks`) so we can pin the
    /// commit-time semantics end-to-end.
    mod apply_hunks_multi_pure_add {
        use crate::hunks::apply_hunks;
        use crate::tree::test_helpers::to_additive_hunks_for_test;
        use crate::HunkHeader;
        use bstr::ByteSlice;

        fn h(old_start: u32, old_lines: u32, new_start: u32, new_lines: u32) -> HunkHeader {
            HunkHeader {
                old_start,
                old_lines,
                new_start,
                new_lines,
            }
        }

        /// One pure-add hunk inside a worktree hunk that also has a
        /// shared context row. The encoded sub-hunk header is
        /// `(-0,0 +2,1)`; `to_additive_hunks` must rewrite it to
        /// `(-2,0 +2,1)` (anchor *after* the shared row), not
        /// `(-1,0 +2,1)` which would land the new content before the
        /// shared row.
        #[test]
        fn pure_add_after_existing_old_row_lands_after_old() {
            let old = b"X\n";
            let new = b"X\nB\n";
            let wh = vec![h(1, 1, 1, 2)]; // worktree hunk with 0 context
            let (hunks, rejected) =
                to_additive_hunks_for_test(vec![h(0, 0, 2, 1)], &wh, &wh).unwrap();
            assert_eq!(rejected, &[] as &[HunkHeader]);
            let result = apply_hunks(old.as_bstr(), new.as_bstr(), &hunks).unwrap();
            assert_eq!(
                result.as_bstr(),
                b"X\nB\n".as_bstr(),
                "B should land after X, not before",
            );
        }

        /// Two disjoint pure-adds straddling a shared context row in
        /// the same worktree hunk. Expected:
        /// `"A\nX\nB\n"` but the pre-fix pipeline produces
        /// `"A\nB\nX\n"` (everything new bunched at the front).
        #[test]
        fn two_pure_adds_straddling_shared_old_row() {
            let old = b"X\n";
            let new = b"A\nX\nB\n";
            let wh = vec![h(1, 1, 1, 3)];
            let (hunks, rejected) =
                to_additive_hunks_for_test(vec![h(0, 0, 1, 1), h(0, 0, 3, 1)], &wh, &wh).unwrap();
            assert_eq!(rejected, &[] as &[HunkHeader]);
            let result = apply_hunks(old.as_bstr(), new.as_bstr(), &hunks).unwrap();
            assert_eq!(
                result.as_bstr(),
                b"A\nX\nB\n".as_bstr(),
                "A goes before X, B goes after X (interleaved)",
            );
        }

        /// Same shape but with three pure-adds and two shared rows,
        /// validating the running offset survives multiple iterations.
        #[test]
        fn three_pure_adds_around_two_shared_rows() {
            let old = b"X\nY\n";
            let new = b"A\nX\nB\nY\nC\n";
            let wh = vec![h(1, 2, 1, 5)];
            let (hunks, rejected) = to_additive_hunks_for_test(
                vec![h(0, 0, 1, 1), h(0, 0, 3, 1), h(0, 0, 5, 1)],
                &wh,
                &wh,
            )
            .unwrap();
            assert_eq!(rejected, &[] as &[HunkHeader]);
            let result = apply_hunks(old.as_bstr(), new.as_bstr(), &hunks).unwrap();
            assert_eq!(
                result.as_bstr(),
                b"A\nX\nB\nY\nC\n".as_bstr(),
                "each pure-add slots between the two shared rows correctly",
            );
        }

        /// The field-observed shape user reported in the worktree-side
        /// amend flow: HEAD already contains Section B; worktree has
        /// the full A/B/C file; user drags 2 alpha lines from Section
        /// A to amend the existing Section B commit. The desktop emits
        /// `(-0,0 +7,2)` for those two rows (worktree positions 7-8).
        /// Expected: alpha lines land at the top of the resulting
        /// commit's tree (above the existing Section B), not appended
        /// at the end.
        ///
        /// Pre-fix `to_additive_hunks` over-counted preceding-context
        /// rows because the unselected pure-adds (Section A header,
        /// top-of-file lines) all sat in the same wh as the alpha
        /// lines. The clamp to `wh.old_lines` keeps the offset honest.
        #[test]
        fn commit_alpha_lines_when_only_section_b_is_in_head() {
            let old = b"\n## Section B\n- beta line one\n- beta line two\n- beta line three\n\n";
            let new = b"# split test\n\nthis whole file is uncommitted\npure-add hunk\n\n## Section A\n- alpha line one\n- alpha line two\n- alpha line three\n\n## Section B\n- beta line one\n- beta line two\n- beta line three\n\n## Section C\n- gamma line one\n- gamma line two\n- gamma line three\n";
            // Worktree-vs-HEAD: 6 old rows shared; 13 new rows added.
            // 0-context wh splits into a top region (10 added rows, no
            // shared rows around them) and a bottom region (3 added
            // rows after the shared block).
            let wh_with_context = vec![h(1, 6, 1, 19)];
            let wh_no_context = vec![h(1, 0, 1, 10), h(7, 0, 17, 3)];
            // User drags rows 7-8 (alpha line one, alpha line two).
            let (hunks, rejected) = to_additive_hunks_for_test(
                vec![h(0, 0, 7, 2)],
                &wh_with_context,
                &wh_no_context,
            )
            .unwrap();
            assert_eq!(rejected, &[] as &[HunkHeader]);
            let result = apply_hunks(old.as_bstr(), new.as_bstr(), &hunks).unwrap();
            assert_eq!(
                result.as_bstr(),
                b"- alpha line one\n- alpha line two\n\n## Section B\n- beta line one\n- beta line two\n- beta line three\n\n".as_bstr(),
                "alpha lines must land above HEAD's existing Section B, not appended after it",
            );
        }

        /// The field-observed shape: pre-commit baseline has
        /// `\n## Section B\n- beta one\n` (3 rows, the leading blank +
        /// the B header + the first beta), worktree adds Section A on
        /// top and Section C at the bottom; user commits Section A
        /// only. Expected: HEAD gains Section A above the baseline.
        #[test]
        fn commit_section_a_above_existing_section_b() {
            let old = b"\n## Section B\n- beta one\n";
            let new = b"## Section A\n- alpha one\n\n## Section B\n- beta one\n## Section C\n- gamma one\n";
            // Worktree no-context hunk shape:
            //   -1,3 +1,7 (3 old shared rows, 7 new rows total).
            let wh = vec![h(1, 3, 1, 7)];
            // User commits the leading 2 rows ("## Section A", "- alpha one")
            // as one pure-add sub-hunk.
            let (hunks, rejected) =
                to_additive_hunks_for_test(vec![h(0, 0, 1, 2)], &wh, &wh).unwrap();
            assert_eq!(rejected, &[] as &[HunkHeader]);
            let result = apply_hunks(old.as_bstr(), new.as_bstr(), &hunks).unwrap();
            assert_eq!(
                result.as_bstr(),
                b"## Section A\n- alpha one\n\n## Section B\n- beta one\n".as_bstr(),
                "Section A inserted on top; existing baseline (blank + Section B + beta one) preserved verbatim",
            );
        }
    }
}
