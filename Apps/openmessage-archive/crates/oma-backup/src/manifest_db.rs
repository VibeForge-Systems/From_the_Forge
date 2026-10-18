//! The staged Manifest.db: hardened read-only open, structural checks, the
//! row reader and lookups (§7.2–§7.4).
//!
//! Only staged copies are ever opened; the backup's own Manifest.db is never
//! opened with SQLite, because SQLite creates a `-shm` beside any WAL
//! database it opens.
#![deny(clippy::arithmetic_side_effects)]

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::limits::Limit as SqlLimit;
use rusqlite::types::ValueRef;
use rusqlite::{config::DbConfig, Connection, InterruptHandle, OpenFlags, Row};
use sha1::{Digest, Sha1};

use crate::cancel::CancelToken;
use crate::error::{sqlite_error, BackupError, MalformedDetail};
use crate::limits::{LimitKind, Limits};
use crate::mbfile::MbFile;
use crate::plists::BackupFile;

/// How often the interrupt watcher polls the cancellation token.
pub(crate) const INTERRUPT_POLL: Duration = Duration::from_millis(50);

/// What a `Files` row describes, from its `flags` column (1, 2, 4; MVT
/// `manifest.py`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// `flags = 1`.
    File,
    /// `flags = 2`.
    Directory,
    /// `flags = 4`.
    Symlink,
    /// Any other value.
    Other(i64),
}

impl EntryKind {
    fn from_flags(flags: i64) -> Self {
        match flags {
            1 => EntryKind::File,
            2 => EntryKind::Directory,
            4 => EntryKind::Symlink,
            other => EntryKind::Other(other),
        }
    }
}

/// One validated `Files` row.
#[derive(Clone)]
pub struct ManifestEntry {
    /// `[0-9a-f]{40}`, equal to `SHA1(domain ‖ "-" ‖ relativePath)`.
    pub file_id: String,
    /// An opaque lookup key; never logged, never a path.
    pub domain: String,
    /// An opaque lookup key; never logged, never a path (D19).
    pub relative_path: String,
    /// From `flags`.
    pub kind: EntryKind,
    /// `None` if the `file` blob was NULL, of the wrong type, or unreadable.
    pub record: Option<MbFile>,
}

/// Prints the kind and whether a record is present only.
impl fmt::Debug for ManifestEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManifestEntry")
            .field("kind", &self.kind)
            .field("record", &self.record)
            .finish_non_exhaustive()
    }
}

/// `hex(SHA1(domain ‖ "-" ‖ relative_path))`.
pub(crate) fn file_id_for(domain: &str, relative_path: &str) -> String {
    use std::fmt::Write as _;
    let mut h = Sha1::new();
    h.update(domain.as_bytes());
    h.update(b"-");
    h.update(relative_path.as_bytes());
    let mut out = String::with_capacity(40);
    for b in h.finalize().iter() {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// `true` for exactly 40 lowercase hex characters.
pub(crate) fn is_file_id_shape(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Polls the cancellation token and interrupts SQLite once it is set. One
/// per connection, for the connection's lifetime.
pub(crate) struct InterruptWatcher {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl InterruptWatcher {
    pub(crate) fn start(
        handle: InterruptHandle,
        cancel: CancelToken,
        poll: Duration,
    ) -> Result<Self, BackupError> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_seen = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("oma-sqlite-interrupt".into())
            .spawn(move || {
                while !stop_seen.load(Ordering::Relaxed) {
                    if cancel.is_cancelled() {
                        handle.interrupt();
                    }
                    std::thread::park_timeout(poll);
                }
            })
            .map_err(|_| BackupError::Internal("could not start the interrupt watcher"))?;
        Ok(InterruptWatcher {
            stop,
            thread: Some(thread),
        })
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

impl Drop for InterruptWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// How the staged copy was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// `SQLITE_OPEN_READ_ONLY`.
    ReadOnly,
    /// Read-write with `query_only = ON`: used only when a read-only
    /// connection cannot open a staged WAL database. The copy is ours, not
    /// the backup.
    ReadWriteQueryOnly,
}

/// A hardened connection to a staged database, with its interrupt watcher.
pub(crate) struct ManifestDb {
    // Field order is drop order: the watcher stops before the connection closes.
    watcher: InterruptWatcher,
    conn: Connection,
    cancel: CancelToken,
    pub(crate) mode: OpenMode,
}

/// Structural facts from §7.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SchemaFacts {
    /// The first schema problem found, if any: a missing or hidden column,
    /// `Files` not an ordinary table, a virtual table, or zero rows.
    pub problem: Option<MalformedDetail>,
    pub has_properties_table: bool,
    pub views: u32,
    pub triggers: u32,
    pub rows: u64,
}

const REQUIRED_COLUMNS: [&str; 5] = ["fileID", "domain", "relativePath", "flags", "file"];

/// `sqlite3_limit` values for one kind of staged database
/// (sqlite.org/security.html).
pub(crate) struct SqlLimits {
    length: i32,
    sql_length: i32,
    expr_depth: i32,
    column: i32,
    vdbe_op: i32,
    function_arg: i32,
    like_pattern_length: i32,
    variable_number: i32,
}

/// Manifest.db: five known columns and fixed, simple queries.
pub(crate) const MANIFEST_LIMITS: SqlLimits = SqlLimits {
    length: 1 << 20,
    sql_length: 1 << 16,
    expr_depth: 100,
    column: 100,
    vdbe_op: 25_000,
    function_arg: 8,
    like_pattern_length: 50,
    variable_number: 10,
};

/// Other staged databases (sms.db for M3). Wider than Manifest.db's: the
/// `message` table has many columns and bodies can be long. Provisional: M3
/// tunes these against its queries.
pub(crate) const STAGED_DB_LIMITS: SqlLimits = SqlLimits {
    length: 64 << 20,
    sql_length: 1 << 16,
    expr_depth: 100,
    column: 2000,
    vdbe_op: 250_000,
    function_arg: 32,
    like_pattern_length: 50,
    variable_number: 999,
};

fn harden(conn: &Connection, limits: &SqlLimits) -> rusqlite::Result<()> {
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    conn.pragma_update(None, "query_only", true)?;
    conn.pragma_update(None, "cell_size_check", true)?;
    // Never spill decrypted pages to the OS temp directory.
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "mmap_size", 0)?;
    conn.set_limit(SqlLimit::SQLITE_LIMIT_LENGTH, limits.length)?;
    conn.set_limit(SqlLimit::SQLITE_LIMIT_SQL_LENGTH, limits.sql_length)?;
    conn.set_limit(SqlLimit::SQLITE_LIMIT_ATTACHED, 0)?;
    conn.set_limit(SqlLimit::SQLITE_LIMIT_EXPR_DEPTH, limits.expr_depth)?;
    conn.set_limit(SqlLimit::SQLITE_LIMIT_COLUMN, limits.column)?;
    conn.set_limit(SqlLimit::SQLITE_LIMIT_VDBE_OP, limits.vdbe_op)?;
    conn.set_limit(SqlLimit::SQLITE_LIMIT_FUNCTION_ARG, limits.function_arg)?;
    conn.set_limit(SqlLimit::SQLITE_LIMIT_TRIGGER_DEPTH, 1)?;
    conn.set_limit(
        SqlLimit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH,
        limits.like_pattern_length,
    )?;
    conn.set_limit(
        SqlLimit::SQLITE_LIMIT_VARIABLE_NUMBER,
        limits.variable_number,
    )?;
    // Touch the schema, so a database that cannot be read fails here.
    conn.query_row("SELECT count(*) FROM sqlite_schema", [], |r| {
        r.get::<_, i64>(0)
    })?;
    Ok(())
}

/// Open a staged database with the hardening of §7.2: read-only, or (for a
/// WAL database a read-only connection cannot open) read-write with
/// `query_only`. Only ever a staged copy, never a file in the backup.
pub(crate) fn open_staged(
    path: &Path,
    file: BackupFile,
    limits: &SqlLimits,
) -> Result<(Connection, OpenMode), BackupError> {
    let read_only = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|c| harden(&c, limits).map(|()| c));
    match read_only {
        Ok(c) => Ok((c, OpenMode::ReadOnly)),
        Err(e) if e.sqlite_error_code() == Some(rusqlite::ErrorCode::CannotOpen) => {
            // A staged WAL database without a -shm may need a writable
            // connection to build its index. The copy is ours.
            let c = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|e| open_error(&e, file))?;
            harden(&c, limits).map_err(|e| open_error(&e, file))?;
            Ok((c, OpenMode::ReadWriteQueryOnly))
        }
        Err(e) => Err(open_error(&e, file)),
    }
}

fn open_error(e: &rusqlite::Error, file: BackupFile) -> BackupError {
    match e.sqlite_error_code() {
        Some(rusqlite::ErrorCode::NotADatabase) => {
            BackupError::malformed(file, MalformedDetail::NotSqlite)
        }
        _ => sqlite_error(e, false),
    }
}

impl ManifestDb {
    /// Open a staged database read-only and hardened (§7.2).
    pub(crate) fn open_hardened(path: &Path, cancel: &CancelToken) -> Result<Self, BackupError> {
        Self::open_with_poll(path, cancel, INTERRUPT_POLL, BackupFile::ManifestDb)
    }

    pub(crate) fn open_with_poll(
        path: &Path,
        cancel: &CancelToken,
        poll: Duration,
        file: BackupFile,
    ) -> Result<Self, BackupError> {
        let (conn, mode) = open_staged(path, file, &MANIFEST_LIMITS)?;
        let watcher = InterruptWatcher::start(conn.get_interrupt_handle(), cancel.clone(), poll)?;
        Ok(ManifestDb {
            watcher,
            conn,
            cancel: cancel.clone(),
            mode,
        })
    }

    fn err(&self, e: &rusqlite::Error) -> BackupError {
        sqlite_error(e, self.cancel.is_cancelled())
    }

    /// The connection, for tests and for callers that need one more query.
    #[cfg(test)]
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// `PRAGMA quick_check(1)` returned `ok`.
    pub(crate) fn quick_check(&self) -> Result<bool, BackupError> {
        let result: String = self
            .conn
            .query_row("PRAGMA quick_check(1)", [], |r| r.get(0))
            .map_err(|e| self.err(&e))?;
        Ok(result == "ok")
    }

    /// The structural checks of §7.3 (after `quick_check`).
    pub(crate) fn check_schema(&self, limits: &Limits) -> Result<SchemaFacts, BackupError> {
        let mut facts = SchemaFacts::default();
        let mut stmt = self
            .conn
            .prepare("SELECT type, name, sql FROM sqlite_schema")
            .map_err(|e| self.err(&e))?;
        let mut rows = stmt.query([]).map_err(|e| self.err(&e))?;
        let mut files_entries: u32 = 0;
        let mut files_is_table = false;
        let mut files_is_virtual = false;
        let mut other_virtual = false;
        while let Some(row) = rows.next().map_err(|e| self.err(&e))? {
            let kind = text(row.get_ref(0));
            let name = text(row.get_ref(1));
            let sql = text(row.get_ref(2));
            let is_virtual = sql
                .as_deref()
                .map(|s| s.trim_start().to_ascii_uppercase())
                .is_some_and(|s| s.starts_with("CREATE VIRTUAL TABLE"));
            match kind.as_deref() {
                Some("view") => facts.views = facts.views.saturating_add(1),
                Some("trigger") => facts.triggers = facts.triggers.saturating_add(1),
                _ => {}
            }
            match name.as_deref() {
                Some("Files") => {
                    files_entries = files_entries.saturating_add(1);
                    files_is_table = kind.as_deref() == Some("table");
                    files_is_virtual = is_virtual;
                }
                Some("Properties") if kind.as_deref() == Some("table") => {
                    facts.has_properties_table = true;
                }
                _ => {
                    if is_virtual {
                        other_virtual = true;
                    }
                }
            }
        }
        drop(rows);
        drop(stmt);

        if files_entries != 1 || !files_is_table {
            facts.problem = Some(MalformedDetail::Schema("Files is not an ordinary table"));
            return Ok(facts);
        }
        if files_is_virtual {
            facts.problem = Some(MalformedDetail::Schema("Files is a virtual table"));
            return Ok(facts);
        }
        if other_virtual {
            facts.problem = Some(MalformedDetail::Schema("a virtual table is present"));
            return Ok(facts);
        }

        // Columns, by name, none hidden or generated.
        let mut stmt = self
            .conn
            .prepare("SELECT name, hidden FROM pragma_table_xinfo('Files')")
            .map_err(|e| self.err(&e))?;
        let mut rows = stmt.query([]).map_err(|e| self.err(&e))?;
        let mut found = [false; 5];
        while let Some(row) = rows.next().map_err(|e| self.err(&e))? {
            let Some(name) = text(row.get_ref(0)) else {
                continue;
            };
            let hidden = row
                .get_ref(1)
                .ok()
                .and_then(|v| v.as_i64().ok())
                .unwrap_or(1);
            for (slot, want) in found.iter_mut().zip(REQUIRED_COLUMNS) {
                if name == want {
                    if hidden != 0 {
                        facts.problem = Some(MalformedDetail::Schema(
                            "a required column is hidden or generated",
                        ));
                    }
                    *slot = true;
                }
            }
        }
        drop(rows);
        drop(stmt);
        if facts.problem.is_some() {
            return Ok(facts);
        }
        if !found.iter().all(|f| *f) {
            facts.problem = Some(MalformedDetail::Schema("a required column is missing"));
            return Ok(facts);
        }

        let rows: i64 = self
            .conn
            .query_row("SELECT count(*) FROM Files", [], |r| r.get(0))
            .map_err(|e| self.err(&e))?;
        let rows = u64::try_from(rows).unwrap_or(0);
        facts.rows = rows;
        if rows > limits.manifest_rows {
            return Err(BackupError::limit(
                LimitKind::ManifestRows,
                rows,
                limits.manifest_rows,
            ));
        }
        if rows == 0 {
            facts.problem = Some(MalformedDetail::Empty);
            return Ok(facts);
        }
        let max_len: Option<i64> = self
            .conn
            .query_row("SELECT max(length(file)) FROM Files", [], |r| r.get(0))
            .map_err(|e| self.err(&e))?;
        let max_len = max_len.and_then(|m| u64::try_from(m).ok()).unwrap_or(0);
        if max_len > limits.mbfile_bytes {
            return Err(BackupError::limit(
                LimitKind::MbFileBytes,
                max_len,
                limits.mbfile_bytes,
            ));
        }
        Ok(facts)
    }

    /// Call `f` for every `Files` row.
    pub(crate) fn for_each_row(
        &self,
        mut f: impl FnMut(&Row<'_>) -> Result<(), BackupError>,
    ) -> Result<(), BackupError> {
        let mut stmt = self
            .conn
            .prepare("SELECT fileID, domain, relativePath, flags, file FROM Files")
            .map_err(|e| self.err(&e))?;
        let mut rows = stmt.query([]).map_err(|e| self.err(&e))?;
        while let Some(row) = rows.next().map_err(|e| self.err(&e))? {
            f(row)?;
        }
        Ok(())
    }

    /// Look up a file by domain and relative path (§9). Two rows with the
    /// fileID are refused as ambiguous; a row whose storage classes, domain
    /// or path do not match is refused as invalid.
    pub(crate) fn find(
        &self,
        domain: &str,
        relative_path: &str,
        limits: &Limits,
    ) -> Result<Option<ManifestEntry>, BackupError> {
        let id = file_id_for(domain, relative_path);
        let mut stmt = self
            .conn
            .prepare("SELECT fileID, domain, relativePath, flags, file FROM Files WHERE fileID = ?1 LIMIT 2")
            .map_err(|e| self.err(&e))?;
        let mut rows = stmt.query([&id]).map_err(|e| self.err(&e))?;
        let mut first: Option<RowCheck> = None;
        let mut count: u8 = 0;
        while let Some(row) = rows.next().map_err(|e| self.err(&e))? {
            count = count.saturating_add(1);
            if first.is_none() {
                first = Some(check_row(row, limits));
            }
        }
        if count > 1 {
            return Err(BackupError::FileUnavailable(
                crate::error::FileUnavailable::Ambiguous,
            ));
        }
        match first {
            None => Ok(None),
            Some(RowCheck::Parsed {
                entry, id_matches, ..
            }) if id_matches && entry.domain == domain && entry.relative_path == relative_path => {
                Ok(Some(entry))
            }
            Some(_) => Err(BackupError::FileUnavailable(
                crate::error::FileUnavailable::InvalidEntry,
            )),
        }
    }

    /// Stop the watcher and close the connection.
    pub(crate) fn close(self) {
        let ManifestDb {
            mut watcher, conn, ..
        } = self;
        watcher.stop();
        let _ = conn.close();
    }
}

fn text(v: rusqlite::Result<ValueRef<'_>>) -> Option<String> {
    match v.ok()? {
        ValueRef::Text(t) => std::str::from_utf8(t).ok().map(str::to_owned),
        _ => None,
    }
}

/// What one row turned out to be.
pub(crate) enum RowCheck {
    /// Wrong storage classes, invalid UTF-8 or an embedded NUL.
    InvalidRow,
    /// The row's columns had the right types.
    Parsed {
        entry: ManifestEntry,
        /// `fileID` is 40 lowercase hex characters.
        id_shape_ok: bool,
        /// `fileID` equals the hash of domain and path.
        id_matches: bool,
        /// A `file` blob was present but did not decode.
        record_unreadable: bool,
    },
}

fn utf8_text(v: ValueRef<'_>) -> Option<String> {
    match v {
        ValueRef::Text(t) => {
            let s = std::str::from_utf8(t).ok()?;
            if s.contains('\0') {
                None
            } else {
                Some(s.to_owned())
            }
        }
        _ => None,
    }
}

/// Check one row without letting a bad value abort the pass.
pub(crate) fn check_row(row: &Row<'_>, limits: &Limits) -> RowCheck {
    let get = |i: usize| row.get_ref(i).ok();
    let (Some(id), Some(domain), Some(rel), Some(flags), Some(file)) =
        (get(0), get(1), get(2), get(3), get(4))
    else {
        return RowCheck::InvalidRow;
    };
    let (Some(file_id), Some(domain), Some(relative_path)) =
        (utf8_text(id), utf8_text(domain), utf8_text(rel))
    else {
        return RowCheck::InvalidRow;
    };
    let ValueRef::Integer(flags) = flags else {
        return RowCheck::InvalidRow;
    };
    let (record, record_unreadable) = match file {
        ValueRef::Null => (None, false),
        ValueRef::Blob(b) => match MbFile::decode(b, limits) {
            Ok(m) => (Some(m), false),
            Err(_) => (None, true),
        },
        _ => return RowCheck::InvalidRow,
    };
    let id_shape_ok = is_file_id_shape(&file_id);
    let id_matches = id_shape_ok && file_id_for(&domain, &relative_path) == file_id;
    RowCheck::Parsed {
        entry: ManifestEntry {
            file_id,
            domain,
            relative_path,
            kind: EntryKind::from_flags(flags),
            record,
        },
        id_shape_ok,
        id_matches,
        record_unreadable,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arithmetic_side_effects)]
    use super::*;
    use std::path::PathBuf;

    const SCHEMA: &str = "CREATE TABLE Files (fileID TEXT PRIMARY KEY, domain TEXT, relativePath TEXT, flags INTEGER, file BLOB);
        CREATE INDEX FilesDomainIdx ON Files(domain);
        CREATE TABLE Properties (key TEXT PRIMARY KEY, value BLOB);";

    fn db(sql: &str) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("Manifest.db");
        let c = Connection::open(&path).expect("create");
        c.execute_batch(sql).expect("schema");
        drop(c);
        (tmp, path)
    }

    fn insert(path: &Path, domain: &str, rel: &str, flags: i64) {
        let c = Connection::open(path).expect("open");
        c.execute(
            "INSERT INTO Files VALUES (?1, ?2, ?3, ?4, NULL)",
            rusqlite::params![file_id_for(domain, rel), domain, rel, flags],
        )
        .expect("insert");
    }

    fn open(path: &Path) -> ManifestDb {
        ManifestDb::open_hardened(path, &CancelToken::new()).expect("open")
    }

    #[test]
    fn known_file_id() {
        assert_eq!(
            file_id_for("HomeDomain", "Library/SMS/sms.db"),
            "3d0d7e5fb2ce288813306e4d4636395e047a3d28"
        );
        assert!(is_file_id_shape("3d0d7e5fb2ce288813306e4d4636395e047a3d28"));
        assert!(!is_file_id_shape(
            "3D0D7E5FB2CE288813306E4D4636395E047A3D28"
        ));
        assert!(!is_file_id_shape("../../xx"));
    }

    #[test]
    fn a_valid_database_passes_and_rejects_writes() {
        let (_t, path) = db(SCHEMA);
        insert(&path, "HomeDomain", "Library/SMS/sms.db", 1);
        let m = open(&path);
        assert_eq!(m.mode, OpenMode::ReadOnly);
        assert!(m.quick_check().expect("qc"));
        let facts = m.check_schema(&Limits::default()).expect("schema");
        assert_eq!(facts.problem, None);
        assert_eq!(facts.rows, 1);
        assert!(facts.has_properties_table);
        assert!(m
            .conn()
            .execute("INSERT INTO Files VALUES ('a','b','c',1,NULL)", [])
            .is_err());
        m.close();
    }

    fn problem(sql: &str) -> Option<MalformedDetail> {
        let (_t, path) = db(sql);
        let m = open(&path);
        m.check_schema(&Limits::default()).expect("schema").problem
    }

    #[test]
    fn schema_problems() {
        assert_eq!(
            problem("CREATE TABLE Files (fileID TEXT, domain TEXT, flags INTEGER, file BLOB);"),
            Some(MalformedDetail::Schema("a required column is missing"))
        );
        assert_eq!(
            problem("CREATE TABLE T (fileID TEXT, domain TEXT, relativePath TEXT, flags INTEGER, file BLOB);
                     CREATE VIEW Files AS SELECT * FROM T;"),
            Some(MalformedDetail::Schema("Files is not an ordinary table"))
        );
        assert_eq!(
            problem(
                "CREATE VIRTUAL TABLE Files USING fts5(fileID, domain, relativePath, flags, file);"
            ),
            Some(MalformedDetail::Schema("Files is a virtual table"))
        );
        assert_eq!(
            problem(
                "CREATE TABLE Files (fileID TEXT, domain TEXT, relativePath TEXT, flags INTEGER,
                     file BLOB GENERATED ALWAYS AS (domain) VIRTUAL);"
            ),
            Some(MalformedDetail::Schema(
                "a required column is hidden or generated"
            ))
        );
        assert_eq!(problem(SCHEMA), Some(MalformedDetail::Empty));
    }

    #[test]
    fn triggers_are_counted_and_never_fired() {
        let (_t, path) = db(&format!(
            "{SCHEMA}
             CREATE TABLE Log (n INTEGER);
             CREATE TRIGGER t AFTER INSERT ON Files BEGIN INSERT INTO Log VALUES (1); END;
             CREATE VIEW v AS SELECT fileID FROM Files;"
        ));
        insert(&path, "HomeDomain", "a", 1);
        let c = Connection::open(&path).expect("open");
        c.execute("DELETE FROM Log", []).expect("reset log");
        drop(c);
        let m = open(&path);
        let facts = m.check_schema(&Limits::default()).expect("schema");
        assert_eq!(facts.triggers, 1);
        assert_eq!(facts.views, 1);
        assert_eq!(facts.problem, None);
        m.for_each_row(|_| Ok(())).expect("rows");
        let fired: i64 = m
            .conn()
            .query_row("SELECT count(*) FROM Log", [], |r| r.get(0))
            .expect("log");
        assert_eq!(fired, 0);
    }

    #[test]
    fn oversize_records_and_row_caps() {
        let (_t, path) = db(SCHEMA);
        let c = Connection::open(&path).expect("open");
        c.execute(
            "INSERT INTO Files VALUES ('x','d','r',1,?1)",
            [vec![0u8; 100 * 1024 + 1]],
        )
        .expect("insert");
        drop(c);
        let m = open(&path);
        assert!(matches!(
            m.check_schema(&Limits::default()),
            Err(BackupError::LimitExceeded {
                what: LimitKind::MbFileBytes,
                ..
            })
        ));
        let tight = Limits {
            manifest_rows: 0,
            ..Limits::default()
        };
        assert!(matches!(
            m.check_schema(&tight),
            Err(BackupError::LimitExceeded {
                what: LimitKind::ManifestRows,
                ..
            })
        ));
    }

    #[test]
    fn a_non_sqlite_file_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("Manifest.db");
        std::fs::write(&path, vec![0x42u8; 8192]).expect("write");
        assert!(matches!(
            ManifestDb::open_hardened(&path, &CancelToken::new()),
            Err(BackupError::Malformed {
                detail: MalformedDetail::NotSqlite,
                ..
            })
        ));
    }

    #[test]
    fn bad_rows_are_counted_not_fatal_and_lookups_refuse_ambiguity() {
        let (_t, path) = db("CREATE TABLE Files (fileID, domain, relativePath, flags, file);");
        let c = Connection::open(&path).expect("open");
        let sms = file_id_for("HomeDomain", "Library/SMS/sms.db");
        c.execute(
            "INSERT INTO Files VALUES (?1, 'HomeDomain', 'Library/SMS/sms.db', 1, NULL)",
            [&sms],
        )
        .expect("insert");
        c.execute(
            "INSERT INTO Files VALUES (?1, 'HomeDomain', 'Library/SMS/sms.db', 1, NULL)",
            [&sms],
        )
        .expect("duplicate");
        c.execute("INSERT INTO Files VALUES (1, 'd', 'r', 1, NULL)", [])
            .expect("int id");
        c.execute(
            "INSERT INTO Files VALUES ('x', CAST(X'FF00' AS TEXT), 'r', 1, NULL)",
            [],
        )
        .expect("bad utf8");
        c.execute("INSERT INTO Files VALUES ('y', 'd', 'r', 'one', NULL)", [])
            .expect("text flags");
        for rel in ["a:b", "What?.jpg", "trailing.", "../../escape"] {
            c.execute(
                "INSERT INTO Files VALUES (?1, 'MediaDomain', ?2, 1, NULL)",
                [&file_id_for("MediaDomain", rel), rel],
            )
            .expect("odd");
        }
        drop(c);
        let m = open(&path);
        let mut invalid = 0;
        let mut parsed = 0;
        m.for_each_row(|row| {
            match check_row(row, &Limits::default()) {
                RowCheck::InvalidRow => invalid += 1,
                RowCheck::Parsed { .. } => parsed += 1,
            }
            Ok(())
        })
        .expect("pass completes");
        assert_eq!(invalid, 3);
        assert_eq!(parsed, 6);
        assert!(matches!(
            m.find("HomeDomain", "Library/SMS/sms.db", &Limits::default()),
            Err(BackupError::FileUnavailable(
                crate::error::FileUnavailable::Ambiguous
            ))
        ));
        for rel in ["a:b", "What?.jpg", "trailing.", "../../escape"] {
            let e = m
                .find("MediaDomain", rel, &Limits::default())
                .expect("lookup")
                .expect("found");
            assert_eq!(e.relative_path, rel);
        }
        assert!(m
            .find("MediaDomain", "absent", &Limits::default())
            .expect("lookup")
            .is_none());
    }

    #[test]
    fn interrupts_during_quick_check_and_the_row_pass_are_cancelled() {
        let (_t, path) = db(SCHEMA);
        let c = Connection::open(&path).expect("open");
        c.execute_batch(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200000)
             INSERT INTO Files SELECT printf('%040d', i), 'HomeDomain', 'path/' || i, 1, zeroblob(64) FROM n;",
        )
        .expect("fill");
        drop(c);

        // quick_check with an already-cancelled token: the watcher (polling
        // every millisecond here) interrupts the running statement.
        let token = CancelToken::new();
        let m = ManifestDb::open_with_poll(
            &path,
            &token,
            Duration::from_millis(1),
            BackupFile::ManifestDb,
        )
        .expect("open");
        token.cancel();
        assert!(matches!(m.quick_check(), Err(BackupError::Cancelled)));
        m.close();

        // The row pass: cancel from inside the callback; the next step is
        // interrupted.
        let token = CancelToken::new();
        let m = ManifestDb::open_with_poll(
            &path,
            &token,
            Duration::from_millis(1),
            BackupFile::ManifestDb,
        )
        .expect("open");
        let mut seen = 0u64;
        let r = m.for_each_row(|_| {
            seen += 1;
            if seen == 10 {
                token.cancel();
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(())
        });
        assert!(matches!(r, Err(BackupError::Cancelled)), "{r:?}");
        assert!(seen < 200_000);
    }

    #[test]
    fn a_corrupt_page_fails_quick_check() {
        let (_t, path) = db(SCHEMA);
        let c = Connection::open(&path).expect("open");
        c.execute_batch(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000)
             INSERT INTO Files SELECT printf('%040d', i), 'HomeDomain', 'path/' || i, 1, NULL FROM n;",
        )
        .expect("fill");
        let root: i64 = c
            .query_row(
                "SELECT rootpage FROM sqlite_schema WHERE name = 'FilesDomainIdx'",
                [],
                |r| r.get(0),
            )
            .expect("rootpage");
        let page_size: i64 = c
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .expect("page size");
        drop(c);
        let mut bytes = std::fs::read(&path).expect("read");
        let start = ((root - 1) * page_size) as usize;
        // Scribble over the index root page's cell pointers and content.
        for b in &mut bytes[start + 8..start + page_size as usize] {
            *b = 0xA5;
        }
        std::fs::write(&path, &bytes).expect("write");
        let m = open(&path);
        assert!(!matches!(m.quick_check(), Ok(true)));
    }
}
