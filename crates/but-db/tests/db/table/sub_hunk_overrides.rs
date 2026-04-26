use but_db::SubHunkOverrideRow;

use crate::table::in_memory_db;

fn sample_row(gitdir: &str, path: &[u8], anchor_old_start: u32) -> SubHunkOverrideRow {
    SubHunkOverrideRow {
        gitdir: gitdir.to_string(),
        path: path.to_vec(),
        anchor_old_start,
        anchor_old_lines: 5,
        anchor_new_start: 10,
        anchor_new_lines: 7,
        ranges_json: "[{\"start\":0,\"end\":2},{\"start\":2,\"end\":5}]".to_string(),
        assignments_json: "[]".to_string(),
        rows_json: "[\"add\",\"add\",\"remove\",\"add\",\"add\"]".to_string(),
        anchor_diff: b"@@ -10,5 +10,7 @@\n a\n+b\n+c\n-d\n+e\n+f\n a\n".to_vec(),
        schema_version: 1,
    }
}

#[test]
fn list_empty() -> anyhow::Result<()> {
    let db = in_memory_db();
    let rows = db.sub_hunk_overrides().list_all()?;
    assert!(rows.is_empty());
    Ok(())
}

#[test]
fn upsert_and_get() -> anyhow::Result<()> {
    let mut db = in_memory_db();
    let row = sample_row("/repo/.git", b"src/lib.rs", 10);

    db.sub_hunk_overrides_mut().upsert(row.clone())?;

    let got = db.sub_hunk_overrides().get(
        &row.gitdir,
        &row.path,
        row.anchor_old_start,
        row.anchor_old_lines,
        row.anchor_new_start,
        row.anchor_new_lines,
    )?;
    assert_eq!(got, Some(row));
    Ok(())
}

#[test]
fn upsert_replaces_existing() -> anyhow::Result<()> {
    let mut db = in_memory_db();
    let mut row = sample_row("/repo/.git", b"src/lib.rs", 10);
    db.sub_hunk_overrides_mut().upsert(row.clone())?;

    row.ranges_json = "[{\"start\":1,\"end\":3}]".to_string();
    row.schema_version = 1;
    db.sub_hunk_overrides_mut().upsert(row.clone())?;

    let all = db.sub_hunk_overrides().list_all()?;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].ranges_json, "[{\"start\":1,\"end\":3}]");
    Ok(())
}

#[test]
fn list_for_gitdir_filters() -> anyhow::Result<()> {
    let mut db = in_memory_db();
    let a = sample_row("/repo-a/.git", b"a.rs", 10);
    let b = sample_row("/repo-b/.git", b"b.rs", 20);
    db.sub_hunk_overrides_mut().upsert(a.clone())?;
    db.sub_hunk_overrides_mut().upsert(b.clone())?;

    let only_a = db.sub_hunk_overrides().list_for_gitdir("/repo-a/.git")?;
    assert_eq!(only_a, vec![a]);

    let only_b = db.sub_hunk_overrides().list_for_gitdir("/repo-b/.git")?;
    assert_eq!(only_b, vec![b]);
    Ok(())
}

#[test]
fn primary_key_distinguishes_anchors() -> anyhow::Result<()> {
    let mut db = in_memory_db();
    let mut r1 = sample_row("/repo/.git", b"src/lib.rs", 10);
    let mut r2 = sample_row("/repo/.git", b"src/lib.rs", 10);
    r2.anchor_old_lines = 99;

    db.sub_hunk_overrides_mut().upsert(r1.clone())?;
    db.sub_hunk_overrides_mut().upsert(r2.clone())?;

    let all = db.sub_hunk_overrides().list_all()?;
    assert_eq!(all.len(), 2);

    r1.ranges_json = "[{\"start\":0,\"end\":1}]".to_string();
    db.sub_hunk_overrides_mut().upsert(r1.clone())?;

    let got1 = db.sub_hunk_overrides().get(
        &r1.gitdir,
        &r1.path,
        r1.anchor_old_start,
        r1.anchor_old_lines,
        r1.anchor_new_start,
        r1.anchor_new_lines,
    )?;
    assert_eq!(got1.unwrap().ranges_json, "[{\"start\":0,\"end\":1}]");

    let got2 = db.sub_hunk_overrides().get(
        &r2.gitdir,
        &r2.path,
        r2.anchor_old_start,
        r2.anchor_old_lines,
        r2.anchor_new_start,
        r2.anchor_new_lines,
    )?;
    assert_eq!(got2, Some(r2));
    Ok(())
}

#[test]
fn delete_removes_only_target_row() -> anyhow::Result<()> {
    let mut db = in_memory_db();
    let row1 = sample_row("/repo/.git", b"a.rs", 10);
    let row2 = sample_row("/repo/.git", b"b.rs", 20);
    db.sub_hunk_overrides_mut().upsert(row1.clone())?;
    db.sub_hunk_overrides_mut().upsert(row2.clone())?;

    let n = db.sub_hunk_overrides_mut().delete(
        &row1.gitdir,
        &row1.path,
        row1.anchor_old_start,
        row1.anchor_old_lines,
        row1.anchor_new_start,
        row1.anchor_new_lines,
    )?;
    assert_eq!(n, 1);

    let all = db.sub_hunk_overrides().list_all()?;
    assert_eq!(all, vec![row2]);
    Ok(())
}

#[test]
fn delete_nonexistent_is_noop() -> anyhow::Result<()> {
    let mut db = in_memory_db();
    let n = db
        .sub_hunk_overrides_mut()
        .delete("/no/such/.git", b"x", 0, 0, 0, 0)?;
    assert_eq!(n, 0);
    Ok(())
}

#[test]
fn delete_for_gitdir_clears_only_that_gitdir() -> anyhow::Result<()> {
    let mut db = in_memory_db();
    let a1 = sample_row("/repo-a/.git", b"a.rs", 10);
    let a2 = sample_row("/repo-a/.git", b"b.rs", 30);
    let b1 = sample_row("/repo-b/.git", b"x.rs", 50);
    db.sub_hunk_overrides_mut().upsert(a1.clone())?;
    db.sub_hunk_overrides_mut().upsert(a2.clone())?;
    db.sub_hunk_overrides_mut().upsert(b1.clone())?;

    let n = db.sub_hunk_overrides_mut().delete_for_gitdir("/repo-a/.git")?;
    assert_eq!(n, 2);

    let remaining = db.sub_hunk_overrides().list_all()?;
    assert_eq!(remaining, vec![b1]);
    Ok(())
}

#[test]
fn round_trip_preserves_blob_bytes_exactly() -> anyhow::Result<()> {
    let mut db = in_memory_db();
    let mut row = sample_row("/repo/.git", b"crates/funky\xff\x00name.rs", 10);
    row.anchor_diff = (0u8..=255u8).collect();
    db.sub_hunk_overrides_mut().upsert(row.clone())?;

    let got = db.sub_hunk_overrides().get(
        &row.gitdir,
        &row.path,
        row.anchor_old_start,
        row.anchor_old_lines,
        row.anchor_new_start,
        row.anchor_new_lines,
    )?;
    assert_eq!(got, Some(row));
    Ok(())
}

#[test]
fn with_transaction_commit_persists() -> anyhow::Result<()> {
    let mut db = in_memory_db();
    let row = sample_row("/repo/.git", b"src/lib.rs", 10);

    let mut trans = db.transaction()?;
    trans.sub_hunk_overrides_mut().upsert(row.clone())?;
    trans.commit()?;

    let all = db.sub_hunk_overrides().list_all()?;
    assert_eq!(all, vec![row]);
    Ok(())
}

#[test]
fn with_transaction_rollback_discards() -> anyhow::Result<()> {
    let mut db = in_memory_db();
    let row = sample_row("/repo/.git", b"src/lib.rs", 10);

    let mut trans = db.transaction()?;
    trans.sub_hunk_overrides_mut().upsert(row)?;
    trans.rollback()?;

    let all = db.sub_hunk_overrides().list_all()?;
    assert!(all.is_empty());
    Ok(())
}
