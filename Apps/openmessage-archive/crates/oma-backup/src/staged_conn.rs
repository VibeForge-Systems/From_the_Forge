//! The read-only connection to a staged database (M3 §3.1, §7.5).
//!
//! The connection itself is never handed out: holding a `&Connection` would
//! let a caller remove the authorizer, because `authorizer(None)` takes
//! `&self`. Callers get statements, a few facts, and op budgets.
//!
//! Three guards stack on the hardening in `manifest_db::harden`:
//!
//! - **An authorizer** (sqlite.org/c3ref/set_authorizer.html) allows only
//!   `SELECT`, reads of any column outside the [`ReadPolicy`]'s denied list,
//!   six functions, and the read forms of six pragmas. Everything else is
//!   `SQLITE_DENY` and counted.
//! - **A progress handler** (sqlite.org/c3ref/progress_handler.html) runs
//!   every [`OPS_PER_TICK`] VM operations. It interrupts once the job is
//!   cancelled, and once an [op budget](StagedConnection::with_op_budget) is
//!   spent.
//! - **The interrupt watcher** from M2 still polls the cancellation token.
#![deny(clippy::arithmetic_side_effects)]

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{Connection, Statement};

use crate::cancel::CancelToken;
use crate::error::{sqlite_error, BackupError};
use crate::manifest_db::{InterruptWatcher, OpenMode};

/// The progress handler runs once per this many SQLite VM operations.
pub const OPS_PER_TICK: u64 = 1000;

/// The primary result code for an interrupted statement.
const SQLITE_INTERRUPT: i32 = 9;

/// Functions the authorizer allows, compared ASCII-case-insensitively.
const ALLOWED_FUNCTIONS: [&str; 6] = [
    "count",
    "typeof",
    "octet_length",
    "length",
    "instr",
    "substr",
];

/// Pragmas the authorizer allows with or without an argument. `table_xinfo`
/// and `index_list` take a table name; `quick_check` takes a row cap.
const ALLOWED_PRAGMAS_WITH_ARG: [&str; 3] = ["table_xinfo", "index_list", "quick_check"];

/// Pragmas the authorizer allows only in their read form (no argument).
const ALLOWED_PRAGMAS_READ: [&str; 4] = ["encoding", "page_size", "page_count", "query_only"];

/// Columns that must never be read, on top of everything the authorizer
/// denies anyway. The names are compared ASCII-case-insensitively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct ReadPolicy {
    denied_columns: &'static [(&'static str, &'static str)],
}

impl ReadPolicy {
    /// Deny reading each `(table, column)` pair.
    pub const fn deny_columns(denied: &'static [(&'static str, &'static str)]) -> Self {
        ReadPolicy {
            denied_columns: denied,
        }
    }

    fn denies(&self, table: &str, column: &str) -> bool {
        self.denied_columns
            .iter()
            .any(|(t, c)| t.eq_ignore_ascii_case(table) && c.eq_ignore_ascii_case(column))
    }
}

/// The text encoding a database declares (`PRAGMA encoding`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TextEncoding {
    /// `UTF-8`.
    Utf8,
    /// `UTF-16le`.
    Utf16le,
    /// `UTF-16be`.
    Utf16be,
}

/// The result of [`StagedConnection::with_op_budget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Budgeted<T> {
    /// The work finished within the budget.
    Done(T),
    /// The budget ran out and the running statement was interrupted. Any
    /// partial result was dropped.
    OverBudget,
}

/// State shared with the authorizer and the progress handler.
#[derive(Default)]
struct Guard {
    denials: AtomicU32,
    budget_active: AtomicBool,
    ticks_left: AtomicU64,
    budget_tripped: AtomicBool,
}

/// Whether the authorizer allows `action` under `policy`.
fn allowed(action: &AuthAction<'_>, policy: &ReadPolicy) -> bool {
    match action {
        AuthAction::Select => true,
        AuthAction::Read {
            table_name,
            column_name,
        } => !policy.denies(table_name, column_name),
        AuthAction::Function { function_name } => ALLOWED_FUNCTIONS
            .iter()
            .any(|f| f.eq_ignore_ascii_case(function_name)),
        AuthAction::Pragma {
            pragma_name,
            pragma_value,
        } => {
            ALLOWED_PRAGMAS_WITH_ARG
                .iter()
                .any(|p| p.eq_ignore_ascii_case(pragma_name))
                || (pragma_value.is_none()
                    && ALLOWED_PRAGMAS_READ
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(pragma_name)))
        }
        _ => false,
    }
}

/// A hardened, query-only connection to a staged database, guarded by an
/// authorizer and a progress handler. It borrows the
/// [`StagedDatabase`](crate::StagedDatabase) it was opened from, so the
/// staged files cannot be removed while it is open.
pub struct StagedConnection<'s> {
    // Field order is drop order: the watcher stops before the connection closes.
    _watcher: InterruptWatcher,
    conn: Connection,
    mode: OpenMode,
    cancel: CancelToken,
    guard: Arc<Guard>,
    _staged: PhantomData<&'s ()>,
}

impl std::fmt::Debug for StagedConnection<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StagedConnection")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

/// Resets the op budget when `with_op_budget` returns or unwinds.
struct BudgetReset<'g>(&'g Guard);

impl Drop for BudgetReset<'_> {
    fn drop(&mut self) {
        self.0.budget_active.store(false, Ordering::Relaxed);
        self.0.ticks_left.store(0, Ordering::Relaxed);
    }
}

impl<'s> StagedConnection<'s> {
    /// Install the guards on a hardened connection. The authorizer goes in
    /// last, after `harden`'s own pragmas.
    pub(crate) fn new(
        conn: Connection,
        mode: OpenMode,
        watcher: InterruptWatcher,
        cancel: &CancelToken,
        policy: &ReadPolicy,
    ) -> Result<Self, BackupError> {
        let guard = Arc::new(Guard::default());

        let handler_guard = Arc::clone(&guard);
        let handler_cancel = cancel.clone();
        let tick = i32::try_from(OPS_PER_TICK).map_err(|_| BackupError::Internal("tick"))?;
        conn.progress_handler(
            tick,
            Some(move || {
                if handler_cancel.is_cancelled() {
                    return true;
                }
                if handler_guard.budget_active.load(Ordering::Relaxed) {
                    let left = handler_guard.ticks_left.load(Ordering::Relaxed);
                    if left == 0 {
                        handler_guard.budget_tripped.store(true, Ordering::Relaxed);
                        return true;
                    }
                    handler_guard
                        .ticks_left
                        .store(left.saturating_sub(1), Ordering::Relaxed);
                }
                false
            }),
        )
        .map_err(|e| sqlite_error(&e, cancel.is_cancelled()))?;

        let auth_guard = Arc::clone(&guard);
        let policy = *policy;
        conn.authorizer(Some(move |ctx: AuthContext<'_>| {
            if allowed(&ctx.action, &policy) {
                Authorization::Allow
            } else {
                // fetch_add wraps; a saturating update keeps the count honest.
                let _ =
                    auth_guard
                        .denials
                        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                            Some(n.saturating_add(1))
                        });
                Authorization::Deny
            }
        }))
        .map_err(|e| sqlite_error(&e, cancel.is_cancelled()))?;

        Ok(StagedConnection {
            _watcher: watcher,
            conn,
            mode,
            cancel: cancel.clone(),
            guard,
            _staged: PhantomData,
        })
    }

    /// How the staged copy was opened.
    pub fn mode(&self) -> OpenMode {
        self.mode
    }

    /// Map a rusqlite error from a statement of this connection. Only the
    /// primary result code is kept. An interrupt after cancellation becomes
    /// [`BackupError::Cancelled`]; an interrupt caused by an op budget stays
    /// `Sqlite { code: Some(9) }`, which [`Self::with_op_budget`] turns into
    /// [`Budgeted::OverBudget`].
    pub fn sqlite_error(&self, e: &rusqlite::Error) -> BackupError {
        sqlite_error(e, self.cancel.is_cancelled())
    }

    /// Prepare a read. The authorizer refuses anything else at this point.
    ///
    /// There is no statement cache: rusqlite's needs its `cache` feature,
    /// which adds crates. Keep the returned statement and re-bind it for
    /// each page instead of preparing it again.
    pub fn prepare_read(&self, sql: &str) -> Result<Statement<'_>, BackupError> {
        self.conn.prepare(sql).map_err(|e| self.sqlite_error(&e))
    }

    fn pragma_i64(&self, sql: &str) -> Result<i64, BackupError> {
        self.conn
            .query_row(sql, [], |r| r.get::<_, i64>(0))
            .map_err(|e| self.sqlite_error(&e))
    }

    /// `PRAGMA query_only` (the read form). `true` means writes are refused.
    pub fn query_only(&self) -> Result<bool, BackupError> {
        Ok(self.pragma_i64("PRAGMA query_only")? == 1)
    }

    /// Rows changed by INSERT, UPDATE and DELETE since the connection
    /// opened. It does not see DDL or pragma writes; the authorizer is the
    /// guard against those.
    pub fn total_changes(&self) -> u64 {
        self.conn.total_changes()
    }

    /// How many actions the authorizer has refused on this connection.
    pub fn authorizer_denials(&self) -> u32 {
        self.guard.denials.load(Ordering::Relaxed)
    }

    /// `PRAGMA page_size`.
    pub fn page_size(&self) -> Result<u64, BackupError> {
        u64::try_from(self.pragma_i64("PRAGMA page_size")?)
            .map_err(|_| BackupError::Internal("negative page size"))
    }

    /// `PRAGMA page_count`.
    pub fn page_count(&self) -> Result<u64, BackupError> {
        u64::try_from(self.pragma_i64("PRAGMA page_count")?)
            .map_err(|_| BackupError::Internal("negative page count"))
    }

    /// `PRAGMA encoding`.
    pub fn encoding(&self) -> Result<TextEncoding, BackupError> {
        let name: String = self
            .conn
            .query_row("PRAGMA encoding", [], |r| r.get(0))
            .map_err(|e| self.sqlite_error(&e))?;
        match name.as_str() {
            "UTF-8" => Ok(TextEncoding::Utf8),
            "UTF-16le" => Ok(TextEncoding::Utf16le),
            "UTF-16be" => Ok(TextEncoding::Utf16be),
            _ => Err(BackupError::Internal("unknown text encoding")),
        }
    }

    /// Run `f` under a budget of about `ops` SQLite VM operations, counted
    /// in ticks of [`OPS_PER_TICK`] (so a statement may run up to
    /// `OPS_PER_TICK - 1` operations past the budget). When the budget runs
    /// out the running statement is interrupted and the result is
    /// [`Budgeted::OverBudget`].
    ///
    /// The budget is told apart from cancellation by a flag the progress
    /// handler sets, never by error text. Cancellation wins: once the job
    /// is cancelled the result is `Err(Cancelled)`. Budgets do not nest.
    pub fn with_op_budget<T>(
        &self,
        ops: u64,
        f: impl FnOnce(&Self) -> Result<T, BackupError>,
    ) -> Result<Budgeted<T>, BackupError> {
        if self.guard.budget_active.load(Ordering::Relaxed) {
            return Err(BackupError::Internal("nested op budget"));
        }
        self.guard.budget_tripped.store(false, Ordering::Relaxed);
        self.guard.ticks_left.store(
            ops.checked_div(OPS_PER_TICK).unwrap_or(0),
            Ordering::Relaxed,
        );
        self.guard.budget_active.store(true, Ordering::Relaxed);
        let result = {
            let _reset = BudgetReset(&self.guard);
            f(self)
        };
        let tripped = self.guard.budget_tripped.swap(false, Ordering::Relaxed);
        if self.cancel.is_cancelled() {
            return match result {
                Err(e) => Err(e),
                Ok(_) => Err(BackupError::Cancelled),
            };
        }
        match result {
            Ok(v) if !tripped => Ok(Budgeted::Done(v)),
            // `f` swallowed the interrupt; its value may be partial.
            Ok(_) => Ok(Budgeted::OverBudget),
            Err(BackupError::Sqlite {
                code: Some(SQLITE_INTERRUPT),
            }) if tripped => Ok(Budgeted::OverBudget),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db_limits::StagedDbLimits;
    use crate::manifest_db::{harden, INTERRUPT_POLL};
    use rusqlite::config::DbConfig;
    use rusqlite::OpenFlags;

    /// What SQLite asks the authorizer for a table-valued pragma function
    /// (M3 Appendix B item 16, settled here for bundled SQLite 3.53.2): the
    /// `SELECT`, column reads of the eponymous `pragma_*` virtual table, and
    /// the pragma itself with its argument. All of them are on the allow-list.
    #[test]
    fn table_valued_pragmas_are_authorized_as_reads_and_pragmas() {
        use std::sync::Mutex;
        let c = Connection::open_in_memory().expect("open");
        c.execute_batch("CREATE TABLE message (ROWID INTEGER PRIMARY KEY, guid TEXT);")
            .expect("schema");
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = Arc::clone(&seen);
        c.authorizer(Some(move |ctx: AuthContext<'_>| {
            let what = match ctx.action {
                AuthAction::Select => "select".to_owned(),
                AuthAction::Read { table_name, .. } => format!("read:{table_name}"),
                AuthAction::Pragma {
                    pragma_name,
                    pragma_value,
                } => format!("pragma:{pragma_name}:{}", pragma_value.unwrap_or("-")),
                AuthAction::Function { function_name } => format!("function:{function_name}"),
                other => format!("other:{other:?}"),
            };
            log.lock().expect("lock").push(what);
            Authorization::Allow
        }))
        .expect("authorizer");
        for (sql, pragma) in [
            (
                "SELECT name, type, pk, hidden FROM pragma_table_xinfo('message')",
                "table_xinfo",
            ),
            (
                "SELECT origin FROM pragma_index_list('message')",
                "index_list",
            ),
        ] {
            seen.lock().expect("lock").clear();
            let mut st = c.prepare(sql).expect("prepare");
            let _ = st.query([]).expect("query").next().expect("step");
            drop(st);
            let seen = seen.lock().expect("lock").clone();
            let policy = ReadPolicy::default();
            assert!(seen.contains(&"select".to_owned()), "{seen:?}");
            assert!(
                seen.iter()
                    .any(|s| s.starts_with(&format!("read:pragma_{pragma}"))),
                "{seen:?}"
            );
            assert!(
                seen.iter()
                    .any(|s| s == &format!("pragma:{pragma}:message")),
                "{seen:?}"
            );
            // Every action seen is allowed by the staged-database allow-list.
            for s in &seen {
                let ok = match s.split(':').collect::<Vec<_>>().as_slice() {
                    ["select"] => allowed(&AuthAction::Select, &policy),
                    ["read", t] => allowed(
                        &AuthAction::Read {
                            table_name: t,
                            column_name: "name",
                        },
                        &policy,
                    ),
                    ["pragma", p, v] => allowed(
                        &AuthAction::Pragma {
                            pragma_name: p,
                            pragma_value: (*v != "-").then_some(*v),
                        },
                        &policy,
                    ),
                    _ => false,
                };
                assert!(ok, "{s} is not on the allow-list");
            }
        }
    }

    /// The read-write `query_only` fallback (`OpenMode::ReadWriteQueryOnly`)
    /// gets the same guards. Built directly, because which mode a staged
    /// copy needs depends on the platform.
    #[test]
    fn closing_a_read_write_connection_leaves_the_staged_pair_unchanged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("sms.db");
        let wal = tmp.path().join("sms.db-wal");
        let c = Connection::open(&path).expect("create");
        c.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .expect("no checkpoint");
        c.pragma_update(None, "journal_mode", "WAL").expect("wal");
        c.execute_batch(
            "CREATE TABLE message (ROWID INTEGER PRIMARY KEY, guid TEXT);
             INSERT INTO message (guid) VALUES ('g');",
        )
        .expect("schema");
        drop(c);
        let main_before = std::fs::read(&path).expect("main");
        let wal_before = std::fs::read(&wal).expect("-wal");
        assert!(!wal_before.is_empty());

        let conn =
            Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE).expect("open");
        harden(&conn, &StagedDbLimits::ceiling()).expect("harden");
        assert!(conn
            .db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE)
            .expect("db_config"));
        let n: i64 = conn
            .query_row("SELECT count(*) FROM message", [], |r| r.get(0))
            .expect("read");
        assert_eq!(n, 1);
        conn.close().map_err(|(_, e)| e).expect("close");

        assert_eq!(std::fs::read(&path).expect("main"), main_before);
        assert_eq!(std::fs::read(&wal).expect("-wal survives"), wal_before);
    }

    #[test]
    fn guards_hold_on_a_read_write_connection() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("sms.db");
        let c = Connection::open(&path).expect("create");
        c.execute_batch(
            "CREATE TABLE message (ROWID INTEGER PRIMARY KEY, guid TEXT);
             INSERT INTO message (guid) VALUES ('g');
             CREATE TABLE recoverable_message_part (part_text BLOB);",
        )
        .expect("schema");
        drop(c);
        let conn =
            Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE).expect("open");
        harden(&conn, &StagedDbLimits::ceiling()).expect("harden");
        let cancel = CancelToken::new();
        let watcher =
            InterruptWatcher::start(conn.get_interrupt_handle(), cancel.clone(), INTERRUPT_POLL)
                .expect("watcher");
        let policy = ReadPolicy::deny_columns(&[("recoverable_message_part", "part_text")]);
        let c = StagedConnection::new(
            conn,
            OpenMode::ReadWriteQueryOnly,
            watcher,
            &cancel,
            &policy,
        )
        .expect("guarded");
        assert_eq!(c.mode(), OpenMode::ReadWriteQueryOnly);
        for sql in [
            "PRAGMA query_only=0",
            "PRAGMA user_version=9",
            "ATTACH DATABASE ':memory:' AS other",
            "CREATE TEMP TABLE scratch (a)",
            "CREATE TABLE scratch (a)",
            "INSERT INTO message (guid) VALUES ('x')",
            "UPDATE message SET guid = 'y'",
            "DELETE FROM message",
            "DROP TABLE message",
            "SELECT part_text FROM recoverable_message_part",
            "BEGIN IMMEDIATE",
        ] {
            assert!(c.prepare_read(sql).is_err(), "{sql}");
        }
        assert!(c.authorizer_denials() >= 11);
        assert!(c.query_only().expect("query_only"));
        assert_eq!(c.total_changes(), 0);
        let n: i64 = c
            .prepare_read("SELECT count(*) FROM message")
            .expect("prepare")
            .query_row([], |r| r.get(0))
            .expect("count");
        assert_eq!(n, 1);
    }

    #[test]
    fn the_allow_list() {
        let p = ReadPolicy::deny_columns(&[("recoverable_message_part", "part_text")]);
        assert!(allowed(&AuthAction::Select, &p));
        assert!(allowed(
            &AuthAction::Read {
                table_name: "message",
                column_name: "text"
            },
            &p
        ));
        assert!(!allowed(
            &AuthAction::Read {
                table_name: "Recoverable_Message_Part",
                column_name: "PART_TEXT"
            },
            &p
        ));
        assert!(allowed(
            &AuthAction::Function {
                function_name: "COUNT"
            },
            &p
        ));
        assert!(!allowed(
            &AuthAction::Function {
                function_name: "load_extension"
            },
            &p
        ));
        assert!(allowed(
            &AuthAction::Pragma {
                pragma_name: "quick_check",
                pragma_value: Some("1")
            },
            &p
        ));
        assert!(allowed(
            &AuthAction::Pragma {
                pragma_name: "query_only",
                pragma_value: None
            },
            &p
        ));
        assert!(!allowed(
            &AuthAction::Pragma {
                pragma_name: "query_only",
                pragma_value: Some("0")
            },
            &p
        ));
        assert!(!allowed(
            &AuthAction::Pragma {
                pragma_name: "user_version",
                pragma_value: None
            },
            &p
        ));
        assert!(!allowed(&AuthAction::Insert { table_name: "t" }, &p));
        assert!(!allowed(&AuthAction::Recursive, &p));
    }
}
