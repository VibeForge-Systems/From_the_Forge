//! Test hooks (spec D85).
//!
//! Without the non-default `fault-injection` feature, every hook below is a
//! constant (`false` or `None`) that the compiler folds away, and the
//! public [`hooks`] module does not exist. Only this crate's own tests turn
//! the feature on.
//!
//! The hooks are per thread, so parallel tests do not see each other's
//! faults: [`hooks::install`] sets them for the calling thread until the
//! returned guard is dropped.

use crate::schema::SchemaPlan;

#[cfg(feature = "fault-injection")]
pub mod hooks {
    //! Per-thread faults for tests.
    use std::cell::RefCell;

    use rusqlite::Transaction;

    use crate::schema::migrate::{no_data, Migration};
    use crate::schema::SchemaPlan;

    /// A test-only schema plan: a v2 that adds one table and one index.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TestSchema {
        /// The v1 → v2 step succeeds.
        V2,
        /// The step's data change fails after its DDL ran.
        V2FailsMidStep,
        /// The step leaves a row that `foreign_key_check` reports.
        V2ForeignKeyViolation,
        /// The step leaves an FTS index entry with no content row, so the
        /// rank-1 integrity check fails.
        V2FtsCorruption,
        /// The step's DDL differs from the DDL the fingerprint expects.
        V2WrongDdl,
        /// The step is not marked test-only, so it needs the
        /// pre-migration copy and is refused.
        V2NeedsCopy,
    }

    /// The faults in force on this thread.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Faults {
        /// The lock self-test reports that a second handle got the lock.
        pub lock_self_test_fails: bool,
        /// Use a test-only schema plan instead of the build's.
        pub schema: Option<TestSchema>,
        /// Alter the expected FTS5 shadow-table DDL text, as a different
        /// SQLite release might write it.
        pub skew_shadow_ddl: bool,
        /// Replace the read op budget (VM operations per statement group).
        pub read_op_budget: Option<u64>,
    }

    thread_local! {
        static FAULTS: RefCell<Faults> = RefCell::new(Faults::default());
    }

    /// Restores the previous faults when dropped.
    pub struct FaultGuard {
        previous: Faults,
    }

    impl Drop for FaultGuard {
        fn drop(&mut self) {
            let previous = self.previous;
            FAULTS.with(|f| *f.borrow_mut() = previous);
        }
    }

    /// Install `faults` for this thread until the guard is dropped.
    #[must_use]
    pub fn install(faults: Faults) -> FaultGuard {
        let previous = FAULTS.with(|f| std::mem::replace(&mut *f.borrow_mut(), faults));
        FaultGuard { previous }
    }

    pub(crate) fn current() -> Faults {
        FAULTS.with(|f| *f.borrow())
    }

    const V2_DDL: &str = "CREATE TABLE test_v2_note (\n  id INTEGER PRIMARY KEY,\n  note TEXT NOT NULL\n) STRICT;\nCREATE INDEX test_v2_note_by_note ON test_v2_note(note);";

    fn fail_mid_step(tx: &Transaction<'_>) -> rusqlite::Result<()> {
        tx.execute("INSERT INTO test_v2_note (note) VALUES ('half done')", [])?;
        Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ABORT),
            None,
        ))
    }

    fn fk_violation(tx: &Transaction<'_>) -> rusqlite::Result<()> {
        tx.execute(
            "INSERT INTO import_cursor (session_id, pass, last_rowid, rows_done, done) \
             VALUES (424242, 1, NULL, 0, 0)",
            [],
        )?;
        Ok(())
    }

    fn fts_corruption(tx: &Transaction<'_>) -> rusqlite::Result<()> {
        tx.execute(
            "INSERT INTO message_fts (rowid, display_text) VALUES (424242, 'ghost entry')",
            [],
        )?;
        Ok(())
    }

    fn wrong_ddl(tx: &Transaction<'_>) -> rusqlite::Result<()> {
        tx.execute_batch("CREATE INDEX test_v2_unexpected ON test_v2_note(id, note);")
    }

    const fn v2(data: fn(&Transaction<'_>) -> rusqlite::Result<()>, test_only: bool) -> Migration {
        Migration {
            from: 1,
            to: 2,
            ddl: V2_DDL,
            data,
            test_only,
        }
    }

    static V2_OK: [Migration; 1] = [v2(no_data, true)];
    static V2_FAILS: [Migration; 1] = [v2(fail_mid_step, true)];
    static V2_FK: [Migration; 1] = [v2(fk_violation, true)];
    static V2_FTS: [Migration; 1] = [v2(fts_corruption, true)];
    static V2_WRONG: [Migration; 1] = [v2(wrong_ddl, true)];
    static V2_COPY: [Migration; 1] = [v2(no_data, false)];

    pub(crate) fn plan(schema: TestSchema) -> SchemaPlan {
        let migrations: &'static [Migration] = match schema {
            TestSchema::V2 => &V2_OK,
            TestSchema::V2FailsMidStep => &V2_FAILS,
            TestSchema::V2ForeignKeyViolation => &V2_FK,
            TestSchema::V2FtsCorruption => &V2_FTS,
            TestSchema::V2WrongDdl => &V2_WRONG,
            TestSchema::V2NeedsCopy => &V2_COPY,
        };
        SchemaPlan {
            version: 2,
            migrations,
        }
    }
}

/// The lock self-test must report failure.
#[inline]
pub(crate) fn lock_self_test_fails() -> bool {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current().lock_self_test_fails
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        false
    }
}

/// A test-only schema plan, if one is installed.
#[inline]
pub(crate) fn schema_plan() -> Option<SchemaPlan> {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current().schema.map(hooks::plan)
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        None
    }
}

/// Alter the expected shadow-table DDL.
#[inline]
pub(crate) fn skew_shadow_ddl() -> bool {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current().skew_shadow_ddl
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        false
    }
}

/// The read op budget to use instead of `ops`, if a test set one.
#[inline]
pub(crate) fn read_op_budget(ops: u64) -> u64 {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current().read_op_budget.unwrap_or(ops)
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        ops
    }
}
