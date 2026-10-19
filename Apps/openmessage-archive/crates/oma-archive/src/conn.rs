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
//! | viewer phases: op budget, cancel and deadline together (M5 §3.2) | yes | no |
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
use std::time::{Duration, Instant};

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

/// Shared with the progress handler. Three modes, never mixed by one
/// caller: an op budget alone (`budgeted`, the M4 reads), a cancel token
/// alone (`cancellable`, the verifier's long reads), or a viewer phase
/// (`viewing`): an op budget, an optional cancel token and a deadline
/// together.
#[derive(Default)]
struct Budget {
    active: AtomicBool,
    ticks_left: AtomicU64,
    tripped: AtomicBool,
    cancellable: AtomicBool,
    cancel: Mutex<Option<oma_backup::CancelToken>>,
    cancelled: AtomicBool,
    deadline_on: AtomicBool,
    deadline: Mutex<Option<Instant>>,
    deadline_hit: AtomicBool,
    /// Progress-handler ticks seen in the current viewer phase (for the
    /// `cancel_at_view_tick` test fault).
    phase_ticks: AtomicU64,
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

    /// The deadline's check: `true` stops the statement.
    fn deadline_passed(&self) -> bool {
        let hit = self
            .deadline
            .lock()
            .ok()
            .and_then(|g| *g)
            .is_some_and(|d| Instant::now() >= d);
        if hit {
            self.deadline_hit.store(true, Ordering::Relaxed);
        }
        hit
    }

    /// The progress handler: `true` interrupts the running statement.
    fn tick(&self) -> bool {
        if let Some(n) = crate::fault::cancel_at_view_tick() {
            if self.deadline_on.load(Ordering::Relaxed)
                && self.phase_ticks.fetch_add(1, Ordering::Relaxed) + 1 == n
            {
                if let Ok(g) = self.cancel.lock() {
                    if let Some(t) = g.as_ref() {
                        t.cancel();
                    }
                }
            }
        }
        if self.cancellable.load(Ordering::Relaxed) && self.cancel_hit() {
            return true;
        }
        if self.deadline_on.load(Ordering::Relaxed) && self.deadline_passed() {
            return true;
        }
        if !self.active.load(Ordering::Relaxed) {
            return false;
        }
        let left = self.ticks_left.load(Ordering::Relaxed);
        if left == 0 {
            self.tripped.store(true, Ordering::Relaxed);
            return true;
        }
        self.ticks_left
            .store(left.saturating_sub(1), Ordering::Relaxed);
        false
    }

    /// Why the current viewer phase was stopped, if it was. A cancel wins
    /// over the budget and the deadline.
    fn stop(&self) -> Option<Stop> {
        if self.cancelled.load(Ordering::Relaxed) {
            Some(Stop::Cancelled)
        } else if self.tripped.load(Ordering::Relaxed) || self.deadline_hit.load(Ordering::Relaxed)
        {
            Some(Stop::TooBroad)
        } else {
            None
        }
    }
}

/// The work budget of one viewer phase (M5 spec §3.2). Each phase of a
/// viewer call runs under its own budget, which resets at the phase's
/// start, so a trip in one phase never interrupts the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ViewBudget {
    /// `conversation_index`, `conversations`.
    List,
    /// The keyset scan of `thread_page` and `reply_thread`.
    Scan,
    /// The batch loads for one page or one detail.
    Assembly,
    /// `day_counts`.
    Month,
    /// `message_detail`, `message_text`.
    Detail,
    /// The full-text collection of a words search.
    Search,
    /// `search_hits` for one page, highlighting included.
    Hits,
    /// One batch of the exact-text scan.
    ScanBatch,
    /// `attachment`, `inspect_blob` and the blob row reads.
    Lookup,
}

impl ViewBudget {
    /// The phase's VM-operation budget. Re-measured by the slice that
    /// first uses each phase.
    pub(crate) const fn ops(self) -> u64 {
        match self {
            ViewBudget::List => 5_000_000,
            ViewBudget::Scan | ViewBudget::Month | ViewBudget::ScanBatch => 2_000_000,
            ViewBudget::Assembly | ViewBudget::Detail => 4_000_000,
            ViewBudget::Search => 30_000_000,
            ViewBudget::Hits => 6_000_000,
            ViewBudget::Lookup => 100_000,
        }
    }

    /// How long a call whose first phase is this one may take in total:
    /// 2 s for a page, a detail, a month or one page of search hits; 5 s
    /// for the conversation list and a words search; 20 s for an
    /// exact-text scan.
    pub(crate) const fn window(self) -> Duration {
        match self {
            ViewBudget::List | ViewBudget::Search => Duration::from_secs(5),
            ViewBudget::Hits => Duration::from_secs(2),
            ViewBudget::ScanBatch => Duration::from_secs(20),
            ViewBudget::Scan
            | ViewBudget::Assembly
            | ViewBudget::Month
            | ViewBudget::Detail
            | ViewBudget::Lookup => Duration::from_secs(2),
        }
    }
}

/// Why a viewer phase was stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    /// The budget or the deadline ran out: `QueryTooBroad`.
    TooBroad,
    /// The cancel token was set, or the connection was interrupted through
    /// its [`rusqlite::InterruptHandle`]: `Cancelled`.
    Cancelled,
}

impl Stop {
    fn error(self) -> ArchiveError {
        match self {
            Stop::TooBroad => ArchiveError::QueryTooBroad,
            Stop::Cancelled => ArchiveError::Cancelled,
        }
    }
}

/// The connection as one viewer phase sees it.
pub(crate) struct Phase<'a> {
    conn: &'a Connection,
    budget: &'a Budget,
}

impl Phase<'_> {
    /// The connection (for statements prepared once per phase; map their
    /// errors with [`Phase::err`]).
    pub(crate) fn conn(&self) -> &Connection {
        self.conn
    }

    /// Why the phase has been stopped so far, if it has: a caller that can
    /// return a partial result checks this after an interrupted statement.
    #[allow(dead_code)] // partial pages arrive with slice 5b2
    pub(crate) fn stopped(&self) -> Option<Stop> {
        self.budget.stop()
    }

    /// A SQLite error of this phase as an archive error: an interrupt is
    /// `QueryTooBroad` or `Cancelled` (an interrupt from the handle, with
    /// neither flag set, is a cancel), never `Damaged`.
    pub(crate) fn err(&self, e: &rusqlite::Error) -> ArchiveError {
        match primary_code(e) {
            Some(SQLITE_INTERRUPT) => self.budget.stop().unwrap_or(Stop::Cancelled).error(),
            Some(crate::error::SQLITE_NOTADB) => ArchiveError::NotAnArchive,
            _ => ArchiveError::sqlite(Stage::View, e),
        }
    }

    /// Between statements: stop the phase if it was already stopped, its
    /// cancel token is set or its deadline has passed. SQLite runs the
    /// progress handler only every [`OPS_PER_TICK`] operations **within**
    /// one statement, so a phase of many short statements checks here.
    pub(crate) fn checkpoint(&self) -> Result<(), ArchiveError> {
        let b = self.budget;
        if b.cancellable.load(Ordering::Relaxed) {
            b.cancel_hit();
        }
        if b.deadline_on.load(Ordering::Relaxed) {
            b.deadline_passed();
        }
        match b.stop() {
            Some(stop) => Err(stop.error()),
            None => Ok(()),
        }
    }

    /// Run `f` and map its SQLite error with [`Phase::err`].
    pub(crate) fn sql<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, ArchiveError> {
        f(self.conn).map_err(|e| self.err(&e))
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
            conn.progress_handler(OPS_PER_TICK, Some(move || handler_budget.tick()))?;
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

impl ReadConn {
    /// Run `f`, one viewer phase, under `b`'s op budget, the optional
    /// `cancel` token and `deadline`, all checked every [`OPS_PER_TICK`]
    /// VM operations (M5 spec §3.2, D90). Over budget or past the deadline
    /// gives `QueryTooBroad`; a cancel gives `Cancelled`; neither is ever
    /// `Damaged`. The budget, the cancel token and every stop flag are
    /// reset at the start of each phase, so a trip in one phase does not
    /// interrupt the next. Work inside one VM opcode (an FTS5 doclist
    /// merge, one SQL function call) cannot be stopped part-way; callers
    /// bound it by construction. SQLite counts operations per statement,
    /// so a statement shorter than [`OPS_PER_TICK`] is never stopped:
    /// a phase that runs many short statements calls
    /// [`Phase::checkpoint`] between them.
    pub(crate) fn viewing<T>(
        &self,
        b: ViewBudget,
        cancel: Option<&oma_backup::CancelToken>,
        deadline: Instant,
        f: impl FnOnce(&Phase<'_>) -> Result<T, ArchiveError>,
    ) -> Result<T, ArchiveError> {
        if cancel.is_some_and(oma_backup::CancelToken::is_cancelled) {
            return Err(ArchiveError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(ArchiveError::QueryTooBroad);
        }
        let ops = crate::fault::view_op_budget(b.ops());
        let budget = &self.budget;
        budget.tripped.store(false, Ordering::Relaxed);
        budget.cancelled.store(false, Ordering::Relaxed);
        budget.deadline_hit.store(false, Ordering::Relaxed);
        budget.phase_ticks.store(0, Ordering::Relaxed);
        budget.ticks_left.store(
            ops / u64::try_from(OPS_PER_TICK).unwrap_or(1000),
            Ordering::Relaxed,
        );
        if let Ok(mut slot) = budget.cancel.lock() {
            *slot = cancel.cloned();
        }
        if let Ok(mut slot) = budget.deadline.lock() {
            *slot = Some(deadline);
        }
        budget
            .cancellable
            .store(cancel.is_some(), Ordering::Relaxed);
        budget.deadline_on.store(true, Ordering::Relaxed);
        budget.active.store(true, Ordering::Relaxed);
        let result = f(&Phase {
            conn: &self.conn,
            budget,
        });
        budget.active.store(false, Ordering::Relaxed);
        budget.deadline_on.store(false, Ordering::Relaxed);
        budget.cancellable.store(false, Ordering::Relaxed);
        let stop = budget.stop();
        budget.tripped.store(false, Ordering::Relaxed);
        budget.cancelled.store(false, Ordering::Relaxed);
        budget.deadline_hit.store(false, Ordering::Relaxed);
        if let Ok(mut slot) = budget.cancel.lock() {
            *slot = None;
        }
        if let Ok(mut slot) = budget.deadline.lock() {
            *slot = None;
        }
        match result {
            // A closure that mapped an interrupt itself, without the
            // phase: still never `Damaged`.
            Err(ArchiveError::Sqlite {
                code: Some(SQLITE_INTERRUPT),
                ..
            }) => Err(stop.unwrap_or(Stop::Cancelled).error()),
            other => other,
        }
    }

    /// Run `f`, every phase of one viewer call, inside one `BEGIN DEFERRED
    /// … COMMIT` read transaction, so the call sees one snapshot (M5 spec
    /// §3.4). No read transaction outlives a call, so an import's
    /// `wal_checkpoint(TRUNCATE)` is never held back for longer than one
    /// call.
    pub(crate) fn snapshot<T>(
        &self,
        f: impl FnOnce() -> Result<T, ArchiveError>,
    ) -> Result<T, ArchiveError> {
        self.conn
            .execute_batch("BEGIN DEFERRED")
            .map_err(|e| ArchiveError::sqlite(Stage::View, &e))?;
        let out = f();
        let end = if out.is_ok() { "COMMIT" } else { "ROLLBACK" };
        let ended = self.conn.execute_batch(end);
        if ended.is_err() && !self.conn.is_autocommit() {
            // A transaction must never outlive the call.
            let _ = self.conn.execute_batch("ROLLBACK");
        }
        match (out, ended) {
            (Ok(v), Ok(())) => Ok(v),
            (Ok(_), Err(e)) => Err(ArchiveError::sqlite(Stage::View, &e)),
            (Err(e), _) => Err(e),
        }
    }

    /// A handle that interrupts this connection's running statement from
    /// another thread (`cancel_search`, M5 spec §3.2). Kept outside any
    /// lock on the reader; an interrupt it causes surfaces as `Cancelled`.
    pub(crate) fn interrupt_handle(&self) -> rusqlite::InterruptHandle {
        self.conn.get_interrupt_handle()
    }
}

/// Interrupts the statement a reader is running, from another thread
/// (M5 spec §3.2, §7.1): the shell keeps one outside the search reader's
/// mutex, sets the old search's cancel token, then calls
/// [`SearchInterrupt::interrupt`] **before** it waits for the mutex, so a
/// long FTS5 statement never holds a lock that cancellation needs.
///
/// An interrupt takes effect between VM opcodes of the running statement
/// and surfaces as `Cancelled`. With no statement running it does
/// nothing; the cancel token, checked between statements, covers that
/// gap.
pub struct SearchInterrupt(rusqlite::InterruptHandle);

impl SearchInterrupt {
    pub(crate) fn of(conn: &ReadConn) -> SearchInterrupt {
        SearchInterrupt(conn.interrupt_handle())
    }

    /// Interrupt the reader's running statement, if any.
    pub fn interrupt(&self) {
        self.0.interrupt();
    }
}

impl std::fmt::Debug for SearchInterrupt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SearchInterrupt")
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

    fn reader() -> (tempfile::TempDir, ReadConn) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = ArchiveDir::at(tmp.path().to_path_buf());
        create_database_file(&dir).expect("file");
        drop(WriteConn::open_new(&dir).expect("writer"));
        let r = ReadConn::open(&dir).expect("reader");
        (tmp, r)
    }

    const ENDLESS: &str = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) \
                           SELECT count(*) FROM n";

    /// Well inside the `List` budget, well over the `Lookup` one.
    const MEDIUM: &str =
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 100000) \
                          SELECT count(*) FROM n";

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    #[test]
    fn a_viewer_phase_over_budget_is_too_broad_never_damaged() {
        let (_tmp, r) = reader();
        let e = r
            .viewing(ViewBudget::Lookup, None, soon(), |p| {
                p.sql(|c| c.query_row(LONG, [], |row| row.get::<_, i64>(0)))
            })
            .expect_err("over budget");
        assert!(matches!(e, ArchiveError::QueryTooBroad), "{e:?}");
        // Within the phase the trip is visible and stays: a later
        // statement is interrupted too (a caller returns what it has).
        let (first, second, stop) = r
            .viewing(ViewBudget::Lookup, None, soon(), |p| {
                let first = p.sql(|c| c.query_row(LONG, [], |row| row.get::<_, i64>(0)));
                let second = p.sql(|c| c.query_row(LONG, [], |row| row.get::<_, i64>(0)));
                Ok((first.is_err(), second.is_err(), p.stopped()))
            })
            .expect("the closure returns what it has");
        assert!(first && second);
        assert_eq!(stop, Some(Stop::TooBroad));
    }

    #[test]
    fn a_trip_in_one_phase_does_not_interrupt_the_next() {
        let (_tmp, r) = reader();
        let out = r.snapshot(|| {
            let e = r
                .viewing(ViewBudget::Lookup, None, soon(), |p| {
                    p.sql(|c| c.query_row(MEDIUM, [], |row| row.get::<_, i64>(0)))
                })
                .expect_err("tripped");
            assert!(matches!(e, ArchiveError::QueryTooBroad));
            r.viewing(ViewBudget::List, None, soon(), |p| {
                assert_eq!(p.stopped(), None);
                p.sql(|c| c.query_row(MEDIUM, [], |row| row.get::<_, i64>(0)))
            })
        });
        assert_eq!(out.expect("the next phase runs"), 100_000);
        assert!(r.conn.is_autocommit(), "no transaction outlives the call");
        // budgeted() keeps its M4 meaning afterwards.
        let n: i64 = r
            .budgeted(READ_OP_BUDGET, Stage::Read, |c| {
                c.query_row("SELECT 7", [], |row| row.get(0))
            })
            .expect("budgeted");
        assert_eq!(n, 7);
    }

    #[test]
    fn cancel_and_deadline_stop_a_viewer_phase() {
        let (_tmp, r) = reader();
        // Already cancelled: nothing runs.
        let token = oma_backup::CancelToken::new();
        token.cancel();
        let e = r
            .viewing(ViewBudget::Search, Some(&token), soon(), |_| Ok(()))
            .expect_err("cancelled");
        assert!(matches!(e, ArchiveError::Cancelled));
        // Cancelled during a statement.
        let token = oma_backup::CancelToken::new();
        let later = token.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            later.cancel();
        });
        let huge = Instant::now() + Duration::from_secs(600);
        let e = r.viewing(ViewBudget::Search, Some(&token), huge, |p| {
            // An endless statement under an unreachable budget.
            p.sql(|c| c.query_row(ENDLESS, [], |row| row.get::<_, i64>(0)))
        });
        t.join().expect("join");
        // The Search budget (30 M ops) may run out first on a fast
        // machine; either way it is never Damaged.
        assert!(
            matches!(
                e,
                Err(ArchiveError::Cancelled | ArchiveError::QueryTooBroad)
            ),
            "{e:?}"
        );
        // A deadline already past: refused before any work.
        let e = r
            .viewing(ViewBudget::List, None, Instant::now(), |_| Ok(()))
            .expect_err("past");
        assert!(matches!(e, ArchiveError::QueryTooBroad));
        // A deadline that passes during a statement.
        let e = r
            .viewing(
                ViewBudget::List,
                None,
                Instant::now() + Duration::from_millis(1),
                |p| {
                    std::thread::sleep(Duration::from_millis(5));
                    p.sql(|c| c.query_row(LONG, [], |row| row.get::<_, i64>(0)))
                },
            )
            .expect_err("deadline");
        assert!(matches!(e, ArchiveError::QueryTooBroad), "{e:?}");
    }

    #[test]
    fn a_cancel_mid_statement_is_cancelled_under_a_large_budget() {
        let (_tmp, r) = reader();
        // The handler checks the token too: cancel after the phase's own
        // check passed; LONG fits the Search budget, so only the cancel
        // can stop it.
        let token = oma_backup::CancelToken::new();
        let e = r.viewing(ViewBudget::Search, Some(&token), soon(), |p| {
            token.cancel();
            p.sql(|c| c.query_row(LONG, [], |row| row.get::<_, i64>(0)))
        });
        assert!(matches!(e, Err(ArchiveError::Cancelled)), "{e:?}");
    }

    #[test]
    fn the_interrupt_handle_stops_a_viewer_phase_as_a_cancel() {
        let (_tmp, r) = reader();
        let handle = r.interrupt_handle();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            handle.interrupt();
        });
        let huge = Instant::now() + Duration::from_secs(600);
        let e = r.viewing(ViewBudget::Search, None, huge, |p| {
            p.sql(|c| c.query_row(ENDLESS, [], |row| row.get::<_, i64>(0)))
        });
        t.join().expect("join");
        assert!(
            matches!(
                e,
                Err(ArchiveError::Cancelled | ArchiveError::QueryTooBroad)
            ),
            "{e:?}"
        );
        // The connection works afterwards.
        let n = r
            .viewing(ViewBudget::Lookup, None, soon(), |p| {
                p.sql(|c| c.query_row("SELECT 7", [], |row| row.get::<_, i64>(0)))
            })
            .expect("works");
        assert_eq!(n, 7);
    }

    #[test]
    fn a_checkpoint_between_statements_sees_cancel_and_deadline() {
        let (_tmp, r) = reader();
        let token = oma_backup::CancelToken::new();
        let e = r.viewing(ViewBudget::List, Some(&token), soon(), |p| {
            p.checkpoint()?;
            token.cancel();
            p.checkpoint()
        });
        assert!(matches!(e, Err(ArchiveError::Cancelled)), "{e:?}");
        let e = r.viewing(
            ViewBudget::List,
            None,
            Instant::now() + Duration::from_millis(2),
            |p| {
                p.checkpoint()?;
                std::thread::sleep(Duration::from_millis(5));
                p.checkpoint()
            },
        );
        assert!(matches!(e, Err(ArchiveError::QueryTooBroad)), "{e:?}");
    }

    #[test]
    fn budgets_and_windows_are_the_designed_ones() {
        assert_eq!(ViewBudget::List.ops(), 5_000_000);
        assert_eq!(ViewBudget::Search.ops(), 30_000_000);
        assert_eq!(ViewBudget::Lookup.ops(), 100_000);
        assert_eq!(ViewBudget::Scan.window(), Duration::from_secs(2));
        assert_eq!(ViewBudget::Search.window(), Duration::from_secs(5));
        assert_eq!(ViewBudget::ScanBatch.window(), Duration::from_secs(20));
    }
}
