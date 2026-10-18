//! The one opener of `archive.sqlite` (spec §5.5, gap G6). Every connection
//! to an archive database is made here, so M8 can add its key pragma in one
//! place.
//!
//! | Setting | ReadOnly | ReadWrite |
//! |---|---|---|
//! | open flags | `READ_ONLY` | `READ_WRITE` (never `CREATE`: the file is created empty, 0600, first) |
//! | VFS | `win32-longpath` with a verbatim path on Windows; default elsewhere | same |
//! | `DEFENSIVE` on, `TRUSTED_SCHEMA` off, DQS off | yes | yes |
//! | triggers, views | off | triggers on (the FTS triggers), views off |
//! | `query_only` | on | off |
//! | `foreign_keys` | on | on |
//! | `synchronous` | — | FULL |
//! | `temp_store` MEMORY, `mmap_size` 0 | yes | yes |
//! | `secure_delete` | — | on |
//! | `journal_size_limit` | — | 64 MiB |
//! | `journal_mode` | read, never set | WAL, set only at creation |
//! | `SQLITE_LIMIT_ATTACHED` 0 and the length limits | yes | yes |
//! | op budget (progress handler) | yes | no |
//! | `locking_mode` | NORMAL | NORMAL |
//!
//! No code outside this module may open, copy, hash or map the database or
//! its `-wal`/`-shm` while a connection is open: on Unix, closing any
//! descriptor to the database drops SQLite's POSIX locks
//! (sqlite.org/howtocorrupt.html §2.2). The path is only reachable through
//! [`DbPathKey`], which only this module can make.
#![allow(clippy::disallowed_methods)] // the one place that opens connections

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::config::DbConfig;
use rusqlite::limits::Limit;
use rusqlite::{Connection, OpenFlags};

use crate::error::{primary_code, ArchiveError, DamageKind, IoOp, Stage, SQLITE_INTERRUPT};
use crate::layout::ArchiveDir;

/// The key to [`ArchiveDir::database`]. Only this module can make one.
pub(crate) struct DbPathKey(());

/// The progress handler runs once per this many VM operations.
const OPS_PER_TICK: i32 = 1000;
/// The op budget for one read-only statement group in `list`, `open` and
/// the reader. The fingerprint reads about 70 schema rows; this leaves a
/// wide margin and still bounds a crafted file.
pub(crate) const READ_OP_BUDGET: u64 = 5_000_000;
/// `journal_size_limit`: the WAL is truncated to this after checkpoints.
pub(crate) const JOURNAL_SIZE_LIMIT: i64 = 64 << 20;
/// `SQLITE_LIMIT_LENGTH`: the largest value. The largest capped source value
/// is `attributedBody` at 16 MiB.
const LIMIT_LENGTH: i32 = 64 << 20;
/// `SQLITE_LIMIT_SQL_LENGTH`.
const LIMIT_SQL_LENGTH: i32 = 1 << 20;
/// How long a connection waits on SQLite's own locks.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Shared with the progress handler: either an op budget (`active`) or a
/// cancel token (`cancellable`, the verifier's long reads), never both.
#[derive(Default)]
struct Budget {
    active: AtomicBool,
    ticks_left: AtomicU64,
    tripped: AtomicBool,
    cancellable: AtomicBool,
    cancel: Mutex<Option<oma_backup::CancelToken>>,
    cancelled: AtomicBool,
}

impl Budget {
    /// The cancellable mode's check: `true` stops the statement.
    fn cancel_hit(&self) -> bool {
        let hit = self
            .cancel
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(oma_backup::CancelToken::is_cancelled))
            .unwrap_or(false);
        if hit {
            self.cancelled.store(true, Ordering::Relaxed);
        }
        hit
    }
}

/// A hardened read-only connection with an op budget.
pub(crate) struct ReadConn {
    conn: Connection,
    budget: Arc<Budget>,
}

/// A hardened read-write connection.
pub(crate) struct WriteConn {
    conn: Connection,
}

fn open_connection(path: &Path, flags: OpenFlags) -> rusqlite::Result<Connection> {
    #[cfg(windows)]
    {
        // Bundled SQLite's default Windows VFS caps a path at MAX_PATH; the
        // `win32-longpath` VFS with a verbatim (`\\?\`) path does not. The
        // M2 pattern (oma-backup `manifest_db.rs`). WAL and `-shm` on this
        // VFS are checked by `tests/windows.rs` (D86).
        let verbatim = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        Connection::open_with_flags_and_vfs(verbatim, flags, "win32-longpath")
    }
    #[cfg(not(windows))]
    {
        Connection::open_with_flags(path, flags)
    }
}

/// The settings both profiles share.
fn harden_common(conn: &Connection) -> rusqlite::Result<()> {
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DDL, false)?;
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false)?;
    conn.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, LIMIT_LENGTH)?;
    conn.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, LIMIT_SQL_LENGTH)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "mmap_size", 0)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(())
}

fn open_err(e: &rusqlite::Error) -> ArchiveError {
    match primary_code(e) {
        Some(crate::error::SQLITE_NOTADB) => ArchiveError::NotAnArchive,
        _ => ArchiveError::sqlite(Stage::Open, e),
    }
}

/// Create the empty database file, mode 0600 on Unix, so that SQLite's
/// `-wal` and `-shm` take the same mode (`findCreateFileMode` in the
/// bundled `sqlite3.c`).
pub(crate) fn create_database_file(dir: &ArchiveDir) -> Result<(), ArchiveError> {
    let path = dir.database(&DbPathKey(()));
    let f =
        oma_fs::create_new_private(&path).map_err(|e| ArchiveError::io(IoOp::CreateFile, &e))?;
    f.sync_all().map_err(|e| ArchiveError::io(IoOp::Sync, &e))?;
    Ok(())
}

impl ReadConn {
    /// The ReadOnly profile. Never writes the database; SQLite may create
    /// the `-shm` (and an empty `-wal`) beside a WAL database it reads.
    pub(crate) fn open(dir: &ArchiveDir) -> Result<ReadConn, ArchiveError> {
        let path = dir.database(&DbPathKey(()));
        let conn = open_connection(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| open_err(&e))?;
        let budget = Arc::new(Budget::default());
        let configure = || -> rusqlite::Result<()> {
            harden_common(&conn)?;
            conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)?;
            conn.pragma_update(None, "query_only", true)?;
            let handler_budget = Arc::clone(&budget);
            conn.progress_handler(
                OPS_PER_TICK,
                Some(move || {
                    if handler_budget.cancellable.load(Ordering::Relaxed) {
                        return handler_budget.cancel_hit();
                    }
                    if !handler_budget.active.load(Ordering::Relaxed) {
                        return false;
                    }
                    let left = handler_budget.ticks_left.load(Ordering::Relaxed);
                    if left == 0 {
                        handler_budget.tripped.store(true, Ordering::Relaxed);
                        return true;
                    }
                    handler_budget
                        .ticks_left
                        .store(left.saturating_sub(1), Ordering::Relaxed);
                    false
                }),
            )?;
            Ok(())
        };
        configure().map_err(|e| match primary_code(&e) {
            Some(crate::error::SQLITE_NOTADB) => ArchiveError::NotAnArchive,
            _ => ArchiveError::sqlite(Stage::Configure, &e),
        })?;
        Ok(ReadConn { conn, budget })
    }

    /// Run `f` under an op budget of `ops` VM operations. A budget that runs
    /// out gives `Damaged(WorkBudget)`. `stage` names SQLite errors.
    pub(crate) fn budgeted<T>(
        &self,
        ops: u64,
        stage: Stage,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, ArchiveError> {
        let ops = crate::fault::read_op_budget(ops);
        let ticks = ops / u64::try_from(OPS_PER_TICK).unwrap_or(1000);
        self.budget.tripped.store(false, Ordering::Relaxed);
        self.budget.ticks_left.store(ticks, Ordering::Relaxed);
        self.budget.active.store(true, Ordering::Relaxed);
        let result = f(&self.conn);
        self.budget.active.store(false, Ordering::Relaxed);
        let tripped = self.budget.tripped.swap(false, Ordering::Relaxed);
        result.map_err(|e| {
            if tripped && primary_code(&e) == Some(SQLITE_INTERRUPT) {
                ArchiveError::Damaged(DamageKind::WorkBudget)
            } else if primary_code(&e) == Some(crate::error::SQLITE_NOTADB) {
                ArchiveError::NotAnArchive
            } else {
                ArchiveError::sqlite(stage, &e)
            }
        })
    }
}

impl ReadConn {
    /// Run `f` without an op budget but stopped by `cancel` (checked every
    /// [`OPS_PER_TICK`] VM operations): the verifier's long reads, which
    /// run only on the user's request (spec §4.5, §11). A stop gives
    /// `Cancelled`.
    pub(crate) fn cancellable<T>(
        &self,
        cancel: &oma_backup::CancelToken,
        stage: Stage,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, ArchiveError> {
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        if let Ok(mut slot) = self.budget.cancel.lock() {
            *slot = Some(cancel.clone());
        }
        self.budget.cancelled.store(false, Ordering::Relaxed);
        self.budget.cancellable.store(true, Ordering::Relaxed);
        let result = f(&self.conn);
        self.budget.cancellable.store(false, Ordering::Relaxed);
        if let Ok(mut slot) = self.budget.cancel.lock() {
            *slot = None;
        }
        let cancelled = self.budget.cancelled.swap(false, Ordering::Relaxed);
        result.map_err(|e| {
            if cancelled && primary_code(&e) == Some(SQLITE_INTERRUPT) {
                ArchiveError::Cancelled
            } else if primary_code(&e) == Some(crate::error::SQLITE_NOTADB) {
                ArchiveError::NotAnArchive
            } else {
                ArchiveError::sqlite(stage, &e)
            }
        })
    }
}

impl WriteConn {
    /// The ReadWrite profile on an existing database. The database must
    /// already be in WAL mode (set once, at creation).
    pub(crate) fn open(dir: &ArchiveDir) -> Result<WriteConn, ArchiveError> {
        let w = Self::open_unchecked(dir)?;
        let mode: String = w
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .map_err(|e| ArchiveError::sqlite(Stage::Configure, &e))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(ArchiveError::Damaged(DamageKind::MetaInconsistent));
        }
        Ok(w)
    }

    fn open_unchecked(dir: &ArchiveDir) -> Result<WriteConn, ArchiveError> {
        let path = dir.database(&DbPathKey(()));
        let conn = open_connection(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| open_err(&e))?;
        let configure = || -> rusqlite::Result<()> {
            harden_common(&conn)?;
            conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, true)?;
            conn.pragma_update(None, "synchronous", "FULL")?;
            conn.pragma_update(None, "secure_delete", true)?;
            conn.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT)?;
            Ok(())
        };
        configure().map_err(|e| match primary_code(&e) {
            Some(crate::error::SQLITE_NOTADB) => ArchiveError::NotAnArchive,
            _ => ArchiveError::sqlite(Stage::Configure, &e),
        })?;
        crate::fault::observe_commits(&conn);
        Ok(WriteConn { conn })
    }

    /// The ReadWrite profile on the freshly created empty file, switched to
    /// WAL (the only place `journal_mode` is ever set).
    pub(crate) fn open_new(dir: &ArchiveDir) -> Result<WriteConn, ArchiveError> {
        let w = Self::open_unchecked(dir)?;
        let mode: String = w
            .conn
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
            .map_err(|e| ArchiveError::sqlite(Stage::Create, &e))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(ArchiveError::Internal("WAL mode was refused"));
        }
        Ok(w)
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    pub(crate) fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

/// Run `f`, a single long statement in its own transaction, with the
/// archive interrupt **armed** (spec §8.6): a progress handler checks
/// `cancel` every [`OPS_PER_TICK`] VM operations and interrupts the
/// statement once it is set, which SQLite reports as `SQLITE_INTERRUPT`.
///
/// The handler is removed before this returns, so the caller's `COMMIT`
/// (or `ROLLBACK`) and every batch statement run disarmed: an interrupted
/// INSERT, UPDATE or DELETE inside an explicit transaction rolls the whole
/// transaction back (sqlite.org/c3ref/interrupt.html), so the batch path
/// never arms it. Only the write connection is armed; read connections
/// keep their op budget.
pub(crate) fn armed<T>(
    conn: &Connection,
    cancel: &oma_backup::CancelToken,
    f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    let token = cancel.clone();
    conn.progress_handler(OPS_PER_TICK, Some(move || token.is_cancelled()))?;
    let out = f(conn);
    let disarmed = conn.progress_handler(0, None::<fn() -> bool>);
    match (out, disarmed) {
        (Ok(v), Ok(())) => Ok(v),
        (Err(e), _) | (Ok(_), Err(e)) => Err(e),
    }
}

/// An in-memory database with the ReadWrite hardening, for building the
/// expected schema objects (§7.3).
pub(crate) fn open_memory() -> Result<Connection, ArchiveError> {
    let conn = Connection::open_in_memory().map_err(|e| ArchiveError::sqlite(Stage::Open, &e))?;
    harden_common(&conn).map_err(|e| ArchiveError::sqlite(Stage::Configure, &e))?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG: &str =
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000000) \
                        SELECT count(*) FROM n";

    #[test]
    fn an_armed_statement_stops_on_cancel_and_the_handler_is_removed() {
        let conn = open_memory().expect("memory");
        let token = oma_backup::CancelToken::new();
        let n: i64 = armed(&conn, &token, |c| c.query_row(LONG, [], |r| r.get(0))).expect("runs");
        assert_eq!(n, 2_000_000);
        token.cancel();
        let e = armed(&conn, &token, |c| {
            c.query_row(LONG, [], |r| r.get::<_, i64>(0))
        })
        .expect_err("interrupted");
        assert_eq!(primary_code(&e), Some(SQLITE_INTERRUPT));
        // Disarmed afterwards: the same statement runs although the token
        // stays cancelled.
        let n: i64 = conn.query_row(LONG, [], |r| r.get(0)).expect("disarmed");
        assert_eq!(n, 2_000_000);
    }

    #[test]
    fn a_cancellable_read_stops_inside_a_statement() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = ArchiveDir::at(tmp.path().to_path_buf());
        create_database_file(&dir).expect("file");
        drop(WriteConn::open_new(&dir).expect("writer"));
        let r = ReadConn::open(&dir).expect("reader");
        let token = oma_backup::CancelToken::new();
        let n: i64 = r
            .cancellable(&token, Stage::Read, |c| {
                c.query_row(LONG, [], |row| row.get(0))
            })
            .expect("runs");
        assert_eq!(n, 2_000_000);
        let endless = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) \
                       SELECT count(*) FROM n";
        let later = token.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            later.cancel();
        });
        let e = r
            .cancellable(&token, Stage::Read, |c| {
                c.query_row(endless, [], |row| row.get::<_, i64>(0))
            })
            .expect_err("stopped");
        canceller.join().expect("join");
        assert!(matches!(e, ArchiveError::Cancelled), "{e:?}");
        // The budget mode works again afterwards.
        let n: i64 = r
            .budgeted(READ_OP_BUDGET, Stage::Read, |c| {
                c.query_row("SELECT 7", [], |row| row.get(0))
            })
            .expect("budgeted");
        assert_eq!(n, 7);
    }
}
