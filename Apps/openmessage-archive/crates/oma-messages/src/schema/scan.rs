//! Step 1 of the probe: the `sqlite_schema` scan (M3 §6.2).
//!
//! Reading `sqlite_schema` connects no virtual table: a module's `xConnect`
//! runs only when its table is used. So the scan can see a virtual table and
//! refuse the database before `quick_check`, which would otherwise run the
//! module's integrity code on attacker data (D44).
//!
//! A `table` row is virtual when its rootpage is 0 or when its whole `sql`
//! text, tokenised the way SQLite's schema loader reads it, starts `CREATE
//! VIRTUAL` ([`oma_backup::sql_declares_virtual_table`]). The loader skips
//! comments and any amount of whitespace and ignores the rootpage, so a
//! fixed-length prefix or a single-space match could be bypassed. The test
//! runs before the `sqlite_*` name rules, so a reserved name cannot hide a
//! virtual table either. (`pragma_table_list` would say "virtual" too, but it
//! prepares `SELECT * FROM` each virtual table to count its columns, which
//! runs the module's `xConnect`.)
//!
//! Names and SQL are read only to classify each row. They are never stored,
//! echoed or logged.
//!
//! Counting rules (also used by the report):
//! - `sqlite_autoindex_*` indexes are not counted in `indexes`;
//! - `sqlite_stat*` tables are counted in `stat_tables` only;
//! - `sqlite_sequence` and every other `sqlite_*` object are counted in
//!   `internal_objects` only;
//! - `unknown_tables` counts ordinary tables outside the known list and
//!   outside `sqlite_*`.

use oma_backup::{StagedConnection, TextEncoding};
use rusqlite::types::ValueRef;

use super::known::KnownTable;
use super::starts_with_ci;
use crate::error::{MessagesError, Stage};
use crate::progress::MessagesCtx;

/// How many `sqlite_schema` rows name one known table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct KnownEntry {
    /// Rows of any type whose name matches (`COLLATE NOCASE`, ASCII only).
    pub(crate) entries: u32,
    /// Of those, rows of type `table` that are not virtual.
    pub(crate) ordinary: u32,
}

impl KnownEntry {
    /// Exactly one row names the table, and it is an ordinary table.
    pub(crate) fn is_ordinary_table(&self) -> bool {
        self.entries == 1 && self.ordinary == 1
    }
}

/// What the scan counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScanFacts {
    pub(crate) virtual_tables: u32,
    pub(crate) unknown_tables: u32,
    pub(crate) views: u32,
    pub(crate) triggers: u32,
    pub(crate) indexes: u32,
    pub(crate) stat_tables: u32,
    pub(crate) internal_objects: u32,
    pub(crate) encoding: TextEncoding,
    pub(crate) known: [KnownEntry; KnownTable::ALL.len()],
}

fn bump(n: &mut u32) {
    *n = n.saturating_add(1);
}

/// Scan `sqlite_schema`. Phase `ScanningSchema` is reported by the caller.
pub(crate) fn scan(
    conn: &StagedConnection<'_>,
    ctx: &mut MessagesCtx<'_>,
) -> Result<ScanFacts, MessagesError> {
    let err = |e: oma_backup::BackupError| MessagesError::from_conn(e, Stage::Scan);
    let encoding = conn.encoding().map_err(err)?;
    let mut facts = ScanFacts {
        virtual_tables: 0,
        unknown_tables: 0,
        views: 0,
        triggers: 0,
        indexes: 0,
        stat_tables: 0,
        internal_objects: 0,
        encoding,
        known: [KnownEntry::default(); KnownTable::ALL.len()],
    };
    let mut st = conn
        .prepare_read("SELECT type, name, rootpage, sql FROM sqlite_schema")
        .map_err(err)?;
    let mut rows = st.query([]).map_err(|e| err(conn.sqlite_error(&e)))?;
    let mut seen: u64 = 0;
    while let Some(row) = rows.next().map_err(|e| err(conn.sqlite_error(&e)))? {
        seen = seen.saturating_add(1);
        if seen.is_multiple_of(128) {
            ctx.check()?;
        }
        let kind = match row.get_ref(0) {
            Ok(ValueRef::Text(t)) => t,
            _ => b"",
        };
        let name = match row.get_ref(1) {
            Ok(ValueRef::Text(n)) => n,
            _ => b"",
        };
        let rootpage_zero = matches!(row.get_ref(2), Ok(ValueRef::Integer(0)));
        let sql = match row.get_ref(3) {
            Ok(ValueRef::Text(s)) => s,
            _ => b"",
        };
        let known = KnownTable::ALL
            .iter()
            .position(|t| t.name().as_bytes().eq_ignore_ascii_case(name));
        let mut count_known = |ordinary: bool| {
            if let Some(entry) = known.and_then(|i| facts.known.get_mut(i)) {
                bump(&mut entry.entries);
                if ordinary {
                    bump(&mut entry.ordinary);
                }
            }
        };
        match kind {
            b"table" => {
                if rootpage_zero || oma_backup::sql_declares_virtual_table(sql) {
                    bump(&mut facts.virtual_tables);
                    count_known(false);
                } else if starts_with_ci(name, b"sqlite_stat") {
                    bump(&mut facts.stat_tables);
                } else if starts_with_ci(name, b"sqlite_") {
                    bump(&mut facts.internal_objects);
                } else if known.is_some() {
                    count_known(true);
                } else {
                    bump(&mut facts.unknown_tables);
                }
            }
            b"view" => {
                bump(&mut facts.views);
                count_known(false);
            }
            b"index" => {
                if !starts_with_ci(name, b"sqlite_autoindex_") {
                    bump(&mut facts.indexes);
                }
                count_known(false);
            }
            b"trigger" => bump(&mut facts.triggers),
            _ => bump(&mut facts.internal_objects),
        }
    }
    Ok(facts)
}
