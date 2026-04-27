mod to_additive_hunks {
    use crate::tree::to_additive_hunks;

    // This needs a copy here to get the types to match, maybe due to cycle-breaking?
    fn hunk_header(old: &str, new: &str) -> crate::HunkHeader {
        let ((old_start, old_lines), (new_start, new_lines)) =
            but_testsupport::hunk_header_raw(old, new);
        crate::HunkHeader {
            old_start,
            old_lines,
            new_start,
            new_lines,
        }
    }

    #[test]
    fn rejected() {
        let wth = vec![hunk_header("-1,10", "+1,10")];
        insta::assert_debug_snapshot!(to_additive_hunks(
            [
                // rejected as old is out of bounds
                hunk_header("-20,1", "+1,10"),
                // rejected as new is out of bounds
                hunk_header("+1,10", "+20,1"),
                // rejected as it doesn't match any anchor point, nor does it match hunks without context
                hunk_header("-0,0", "+2,10")
            ],
            &wth,
            &wth,
        ).unwrap(), @r#"
        (
            [],
            [
                HunkHeader("-20,1", "+1,10"),
                HunkHeader("-1,10", "+20,1"),
                HunkHeader("-0,0", "+2,10"),
            ],
        )
        "#);
    }

    #[test]
    fn only_selections() {
        let wth = vec![hunk_header("-1,10", "+1,10")];
        insta::assert_debug_snapshot!(to_additive_hunks(
            [
                hunk_header("-1,1", "+0,0"),
                hunk_header("-5,2", "+0,0"),
                hunk_header("-10,1", "+0,0")
            ],
            &wth,
            &wth,
        ).unwrap(), @r#"
        (
            [
                HunkHeader("-1,1", "+1,0"),
                HunkHeader("-5,2", "+4,0"),
                HunkHeader("-10,1", "+7,0"),
            ],
            [],
        )
        "#);
        insta::assert_debug_snapshot!(to_additive_hunks(
            [
                hunk_header("-0,0", "+1,1"),
                hunk_header("-0,0", "+5,2"),
                hunk_header("-0,0", "+10,1")
            ],
            &wth,
            &wth,
        ).unwrap(), @r#"
        (
            [
                HunkHeader("-1,0", "+1,1"),
                HunkHeader("-4,0", "+5,2"),
                HunkHeader("-7,0", "+10,1"),
            ],
            [],
        )
        "#);
        insta::assert_debug_snapshot!(to_additive_hunks(
            [
                hunk_header("-0,0", "+1,1"),
                hunk_header("-5,2", "+0,0"),
                hunk_header("-0,0", "+10,1")
            ],
            &wth,
            &wth,
        ).unwrap(), @r#"
        (
            [
                HunkHeader("-1,0", "+1,1"),
                HunkHeader("-5,2", "+6,0"),
                HunkHeader("-11,0", "+10,1"),
            ],
            [],
        )
        "#);
        insta::assert_debug_snapshot!(to_additive_hunks(
            [
                hunk_header("-1,1", "+0,0"),
                hunk_header("-0,0", "+5,2"),
                hunk_header("-10,1", "+0,0")
            ],
            &wth,
            &wth,
        ).unwrap(), @r#"
        (
            [
                HunkHeader("-1,1", "+1,0"),
                HunkHeader("-6,0", "+5,2"),
                HunkHeader("-10,1", "+11,0"),
            ],
            [],
        )
        "#);
    }

    #[test]
    fn selections_and_full_hunks() {
        let wth = vec![
            hunk_header("-1,10", "+1,10"),
            hunk_header("-15,5", "+20,5"),
            hunk_header("-25,5", "+40,5"),
        ];
        insta::assert_debug_snapshot!(to_additive_hunks(
            [
                // full match
                hunk_header("-1,10", "+1,10"),
                // partial match to same hunk
                hunk_header("-15,2", "+0,0"),
                hunk_header("-0,0", "+22,3"),
                // Last hunk isn't used
            ],
            &wth,
            &wth,
        ).unwrap(), @r#"
        (
            [
                HunkHeader("-1,10", "+1,10"),
                HunkHeader("-15,2", "+20,0"),
                HunkHeader("-19,0", "+22,3"),
            ],
            [],
        )
        "#);
    }

    #[test]
    fn only_full_hunks() {
        let wth = vec![
            hunk_header("-1,10", "+1,10"),
            hunk_header("-15,5", "+20,5"),
            hunk_header("-25,5", "+40,5"),
        ];
        insta::assert_debug_snapshot!(to_additive_hunks(
            [
                // full match
                hunk_header("-1,10", "+1,10"),
                hunk_header("-15,5", "+20,5"),
                // Last hunk isn't used
            ],
            &wth,
            &wth,
        ).unwrap(), @r#"
        (
            [
                HunkHeader("-1,10", "+1,10"),
                HunkHeader("-15,5", "+20,5"),
            ],
            [],
        )
        "#);
    }

    #[test]
    fn worktree_hunks_without_context_lines() {
        // diff --git a/file b/file
        // index 190423f..b513cb5 100644
        // --- a/file
        // +++ b/file
        // @@ -93,8 +93,10 @@
        //  93
        //  94
        //  95
        // -96
        // +110
        // +111
        //  97
        // +95
        //  98
        //  99
        // -100
        // +119
        let wth = vec![hunk_header("-93,8", "+93,10")];

        // diff --git a/file b/file
        // index 190423f..b513cb5 100644
        // --- a/file
        // +++ b/file
        // @@ -96 +96,2 @@
        // -96
        // +110
        // +111
        // @@ -97,0 +99 @@
        // +95
        // @@ -100 +102 @@
        // -100
        // +119
        let wth0 = vec![
            hunk_header("-96,1", "+96,2"),
            hunk_header("-98,0", "+99,1"),
            hunk_header("-100,1", "+102,1"),
        ];

        insta::assert_debug_snapshot!(to_additive_hunks(
            [hunk_header("-96,1", "+0,0")],
            &wth,
            &wth0,
        ).unwrap(), @r#"
        (
            [
                HunkHeader("-96,1", "+96,0"),
            ],
            [],
        )
        "#);
        insta::assert_debug_snapshot!(to_additive_hunks(
            [hunk_header("-96,1", "+0,0"), hunk_header("-0,0", "+96,2")],
            &wth,
            &wth0,
        ).unwrap(), @r#"
        (
            [
                HunkHeader("-96,1", "+96,0"),
                HunkHeader("-97,0", "+96,2"),
            ],
            [],
        )
        "#);
        insta::assert_debug_snapshot!(to_additive_hunks(
            [hunk_header("-0,0", "+96,2")],
            &wth,
            &wth0,
        ).unwrap(), @r#"
        (
            [
                HunkHeader("-96,0", "+96,2"),
            ],
            [],
        )
        "#);
    }

    #[test]
    fn real_world_issue() {
        let wth = vec![hunk_header("-1,214", "+1,55")];
        let wth0 = vec![
            hunk_header("-4,13", "+4,0"),
            hunk_header("-18,19", "+5,1"),
            hunk_header("-38,79", "+7,3"),
            hunk_header("-118,64", "+11,0"),
            hunk_header("-183,1", "+12,1"),
            hunk_header("-185,15", "+14,2"),
            hunk_header("-201,5", "+17,5"),
            hunk_header("-207,1", "+23,26"),
            hunk_header("-209,3", "+50,3"),
        ];

        let actual = to_additive_hunks(
            [
                hunk_header("-0,0", "+23,26"),
                hunk_header("-0,0", "+50,3"),
                hunk_header("-207,1", "+0,0"),
                hunk_header("-209,3", "+0,0"),
            ],
            &wth,
            &wth0,
        )
        .unwrap();
        insta::assert_debug_snapshot!(actual, @r#"
        (
            [
                HunkHeader("-207,1", "+23,26"),
                HunkHeader("-209,3", "+50,3"),
            ],
            [],
        )
        "#);

        let actual = to_additive_hunks(
            [
                hunk_header("-0,0", "+23,1"),
                hunk_header("-0,0", "+25,1"),
                hunk_header("-0,0", "+27,2"),
                hunk_header("-0,0", "+30,2"),
                hunk_header("-0,0", "+50,3"),
                hunk_header("-207,1", "+0,0"),
                hunk_header("-209,1", "+0,0"),
                hunk_header("-211,1", "+0,0"),
            ],
            &wth,
            &wth0,
        )
        .unwrap();
        insta::assert_debug_snapshot!(actual, @r#"
        (
            [
                HunkHeader("-207,1", "+23,1"),
                HunkHeader("-208,0", "+25,1"),
                HunkHeader("-208,0", "+27,2"),
                HunkHeader("-208,0", "+30,2"),
                HunkHeader("-209,1", "+50,3"),
                HunkHeader("-211,1", "+53,0"),
            ],
            [],
        )
        "#);

        let actual = to_additive_hunks(
            [
                hunk_header("-207,1", "+0,0"),
                hunk_header("-209,1", "+0,0"),
                hunk_header("-211,1", "+0,0"),
                hunk_header("-0,0", "+23,1"),
                hunk_header("-0,0", "+25,1"),
                hunk_header("-0,0", "+27,2"),
                hunk_header("-0,0", "+30,2"),
                hunk_header("-0,0", "+50,3"),
            ],
            &wth,
            &wth0,
        )
        .unwrap();
        insta::assert_debug_snapshot!(actual, @r#"
        (
            [
                HunkHeader("-207,1", "+23,1"),
                HunkHeader("-208,0", "+25,1"),
                HunkHeader("-208,0", "+27,2"),
                HunkHeader("-208,0", "+30,2"),
                HunkHeader("-209,1", "+50,3"),
                HunkHeader("-211,1", "+53,0"),
            ],
            [],
        )
        "#);
    }

    /// A sub-hunk's *synthesized natural-rendering* header (produced by
    /// `but_hunk_assignment::sub_hunk::synthesize_header`) carries narrower
    /// numeric ranges than its natural anchor but has neither `old_range()`
    /// nor `new_range()` actually `is_null()` (start != 0). The desktop's
    /// `AmendCommitWithHunkDzHandler` historically passed the synth header
    /// verbatim into the commit pipeline, where `to_additive_hunks` rejected
    /// it (no exact match in `worktree_hunks`, falls through to `rejected`).
    /// The frontend must re-encode it as a pair of null-side headers; this
    /// test pins the expected accept-behavior for those re-encoded headers.
    #[test]
    fn pure_add_sub_hunk_via_null_side_encoding() {
        // 5-row pure-add natural anchor.
        let wth = vec![hunk_header("-1,0", "+1,5")];

        // Synth header for the middle row (row index 2) would be
        // (-1,0 +3,1) — not is_null() because old_start=1.
        // The frontend re-encodes via `diffToHunkHeaders("commit")` to:
        let actual = to_additive_hunks(
            [hunk_header("-0,0", "+3,1")],
            &wth,
            &wth,
        )
        .unwrap();
        insta::assert_debug_snapshot!(actual, @r#"
        (
            [
                HunkHeader("-1,0", "+3,1"),
            ],
            [],
        )
        "#);
    }

    /// The synth header form (old_lines=0 but old_start != 0) is what the
    /// frontend used to ship before the null-side re-encoding fix landed.
    /// `to_additive_hunks` should *reject* it so we get the visible "Missing
    /// diff spec association" error instead of silently committing the wrong
    /// content. This test pins that rejection so future code never
    /// accidentally accepts the half-null synth form.
    #[test]
    fn synth_sub_hunk_header_without_re_encoding_is_rejected() {
        let wth = vec![hunk_header("-1,0", "+1,5")];
        let synth = hunk_header("-1,0", "+3,1"); // synth header for row 2.
        let (accepted, rejected) =
            to_additive_hunks([synth], &wth, &wth).unwrap();
        assert!(
            accepted.is_empty(),
            "synth sub-hunk header must be rejected, got {accepted:?}"
        );
        assert_eq!(rejected, vec![synth]);
    }

    #[test]
    fn only_selections_workspace_example() {
        let wth = vec![hunk_header("-1,10", "+1,10")];
        let actual = to_additive_hunks(
            [
                // commit NOT '2,3' of the old
                hunk_header("-2,2", "+0,0"),
                // commit NOT '6,7' of the old
                hunk_header("-6,2", "+0,0"),
                // commit NOT '9' of the old
                hunk_header("-9,1", "+0,0"),
                // commit NOT '10' of the old
                hunk_header("-10,1", "+0,0"),
                // commit '11' of the new
                hunk_header("-0,0", "+1,1"),
                // commit '15,16' of the new
                hunk_header("-0,0", "+5,2"),
                // commit '19,20' of the new
                hunk_header("-0,0", "+9,2"),
            ],
            &wth,
            &wth,
        )
        .unwrap();
        insta::assert_debug_snapshot!(actual, @r#"
        (
            [
                HunkHeader("-2,2", "+1,1"),
                HunkHeader("-6,2", "+5,2"),
                HunkHeader("-9,1", "+9,2"),
                HunkHeader("-10,1", "+11,0"),
            ],
            [],
        )
        "#);
    }
}
