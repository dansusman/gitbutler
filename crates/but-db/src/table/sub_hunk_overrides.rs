#![allow(missing_docs)]

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::{DbHandle, M, SchemaVersion, Transaction};

pub(crate) const M: &[M<'static>] = &[M::up(
    20260424120000,
    SchemaVersion::Zero,
    "CREATE TABLE `sub_hunk_overrides`(
	`gitdir` TEXT NOT NULL,
	`path` BLOB NOT NULL,
	`anchor_old_start` INTEGER NOT NULL,
	`anchor_old_lines` INTEGER NOT NULL,
	`anchor_new_start` INTEGER NOT NULL,
	`anchor_new_lines` INTEGER NOT NULL,
	`ranges_json` TEXT NOT NULL,
	`assignments_json` TEXT NOT NULL,
	`rows_json` TEXT NOT NULL,
	`anchor_diff` BLOB NOT NULL,
	`schema_version` INTEGER NOT NULL,
	PRIMARY KEY(
		`gitdir`,
		`path`,
		`anchor_old_start`,
		`anchor_old_lines`,
		`anchor_new_start`,
		`anchor_new_lines`
	)
);",
)];

/// One row of the `sub_hunk_overrides` table.
///
/// This is the on-disk shape of a `SubHunkOverride` from
/// `but-hunk-assignment`. The complex fields (`ranges`, `assignments`,
/// `rows`) are stored as JSON strings so that callers can pick whatever
/// serde representation they want; this crate intentionally does not
/// know the in-memory shape of those fields to avoid a dependency cycle.
///
/// Tests are in `but-db/tests/db/table/sub_hunk_overrides.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubHunkOverrideRow {
    /// The `gitdir` the override belongs to. Per-project DBs make this
    /// redundant in practice but it is kept as part of the primary key
    /// to defend against ever-shared DBs.
    pub gitdir: String,
    /// Raw bytes of the worktree-relative path the override anchors on.
    pub path: Vec<u8>,
    pub anchor_old_start: u32,
    pub anchor_old_lines: u32,
    pub anchor_new_start: u32,
    pub anchor_new_lines: u32,
    /// JSON-encoded `Vec<RowRange>`.
    pub ranges_json: String,
    /// JSON-encoded `Vec<(RowRange, HunkAssignmentTarget)>`.
    pub assignments_json: String,
    /// JSON-encoded `Vec<RowKind>`.
    pub rows_json: String,
    /// Raw bytes of the cached anchor diff body (with `@@` header).
    pub anchor_diff: Vec<u8>,
    /// Forward-compat version stamp. Start at `1`; bump when the on-disk
    /// shape changes incompatibly.
    pub schema_version: u32,
}

impl DbHandle {
    pub fn sub_hunk_overrides(&self) -> SubHunkOverridesHandle<'_> {
        SubHunkOverridesHandle { conn: &self.conn }
    }

    pub fn sub_hunk_overrides_mut(&mut self) -> SubHunkOverridesHandleMut<'_> {
        SubHunkOverridesHandleMut { conn: &self.conn }
    }
}

impl<'conn> Transaction<'conn> {
    pub fn sub_hunk_overrides(&self) -> SubHunkOverridesHandle<'_> {
        SubHunkOverridesHandle { conn: self.inner() }
    }

    pub fn sub_hunk_overrides_mut(&mut self) -> SubHunkOverridesHandleMut<'_> {
        SubHunkOverridesHandleMut { conn: self.inner() }
    }
}

pub struct SubHunkOverridesHandle<'conn> {
    conn: &'conn rusqlite::Connection,
}

pub struct SubHunkOverridesHandleMut<'conn> {
    conn: &'conn rusqlite::Connection,
}

const SELECT_COLUMNS: &str = "gitdir, path, anchor_old_start, anchor_old_lines, \
     anchor_new_start, anchor_new_lines, ranges_json, assignments_json, \
     rows_json, anchor_diff, schema_version";

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SubHunkOverrideRow> {
    Ok(SubHunkOverrideRow {
        gitdir: row.get(0)?,
        path: row.get(1)?,
        anchor_old_start: row.get(2)?,
        anchor_old_lines: row.get(3)?,
        anchor_new_start: row.get(4)?,
        anchor_new_lines: row.get(5)?,
        ranges_json: row.get(6)?,
        assignments_json: row.get(7)?,
        rows_json: row.get(8)?,
        anchor_diff: row.get(9)?,
        schema_version: row.get(10)?,
    })
}

impl SubHunkOverridesHandle<'_> {
    /// List every override row for `gitdir`. Used by hydration on
    /// `Context` open.
    pub fn list_for_gitdir(&self, gitdir: &str) -> rusqlite::Result<Vec<SubHunkOverrideRow>> {
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM sub_hunk_overrides WHERE gitdir = ?1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([gitdir], map_row)?;
        rows.collect::<Result<Vec<_>, _>>()
    }

    /// List every override row, regardless of `gitdir`. Mostly for
    /// debugging and tests.
    pub fn list_all(&self) -> rusqlite::Result<Vec<SubHunkOverrideRow>> {
        let sql = format!("SELECT {SELECT_COLUMNS} FROM sub_hunk_overrides");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_row)?;
        rows.collect::<Result<Vec<_>, _>>()
    }

    /// Look up a single override row by primary key.
    pub fn get(
        &self,
        gitdir: &str,
        path: &[u8],
        anchor_old_start: u32,
        anchor_old_lines: u32,
        anchor_new_start: u32,
        anchor_new_lines: u32,
    ) -> rusqlite::Result<Option<SubHunkOverrideRow>> {
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM sub_hunk_overrides \
             WHERE gitdir = ?1 AND path = ?2 \
               AND anchor_old_start = ?3 AND anchor_old_lines = ?4 \
               AND anchor_new_start = ?5 AND anchor_new_lines = ?6"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        stmt.query_row(
            rusqlite::params![
                gitdir,
                path,
                anchor_old_start,
                anchor_old_lines,
                anchor_new_start,
                anchor_new_lines,
            ],
            map_row,
        )
        .optional()
    }
}

impl SubHunkOverridesHandleMut<'_> {
    /// Enable read-only access functions.
    pub fn to_ref(&self) -> SubHunkOverridesHandle<'_> {
        SubHunkOverridesHandle { conn: self.conn }
    }

    /// Insert or replace an override row by primary key. Mirrors the
    /// `upsert_override` semantics in `but-hunk-assignment`.
    pub fn upsert(&mut self, row: SubHunkOverrideRow) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO sub_hunk_overrides (\
                 gitdir, path, anchor_old_start, anchor_old_lines, \
                 anchor_new_start, anchor_new_lines, ranges_json, \
                 assignments_json, rows_json, anchor_diff, schema_version\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
             ON CONFLICT(gitdir, path, anchor_old_start, anchor_old_lines, \
                         anchor_new_start, anchor_new_lines) DO UPDATE SET \
                 ranges_json = excluded.ranges_json, \
                 assignments_json = excluded.assignments_json, \
                 rows_json = excluded.rows_json, \
                 anchor_diff = excluded.anchor_diff, \
                 schema_version = excluded.schema_version",
            rusqlite::params![
                row.gitdir,
                row.path,
                row.anchor_old_start,
                row.anchor_old_lines,
                row.anchor_new_start,
                row.anchor_new_lines,
                row.ranges_json,
                row.assignments_json,
                row.rows_json,
                row.anchor_diff,
                row.schema_version,
            ],
        )?;
        Ok(())
    }

    /// Delete a single override row by primary key. Returns the number
    /// of rows deleted (`0` or `1`).
    pub fn delete(
        &mut self,
        gitdir: &str,
        path: &[u8],
        anchor_old_start: u32,
        anchor_old_lines: u32,
        anchor_new_start: u32,
        anchor_new_lines: u32,
    ) -> rusqlite::Result<usize> {
        self.conn.execute(
            "DELETE FROM sub_hunk_overrides \
             WHERE gitdir = ?1 AND path = ?2 \
               AND anchor_old_start = ?3 AND anchor_old_lines = ?4 \
               AND anchor_new_start = ?5 AND anchor_new_lines = ?6",
            rusqlite::params![
                gitdir,
                path,
                anchor_old_start,
                anchor_old_lines,
                anchor_new_start,
                anchor_new_lines,
            ],
        )
    }

    /// Delete every override row for `gitdir`.
    pub fn delete_for_gitdir(&mut self, gitdir: &str) -> rusqlite::Result<usize> {
        self.conn.execute(
            "DELETE FROM sub_hunk_overrides WHERE gitdir = ?1",
            [gitdir],
        )
    }
}
