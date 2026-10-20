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
//!
//! **Fault points** (spec §18.4, D85) mark the moments a crash test kills
//! the process at: [`point`] is called there. With the feature, each hit is
//! counted per thread, and [`hooks::arm`] runs an action at the n-th hit of
//! one point (the crash harness prints `FAULT <point> <n>` and blocks until
//! the parent kills it; other tests cancel or panic). Without the feature,
//! [`point`] is an empty inline function.

use crate::schema::SchemaPlan;

/// A moment a crash test can stop the process at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum FaultPoint {
    /// The start of one source record in a pass callback, before the
    /// cancellation check.
    Record,
    /// A batch's rows, cursor and counts are written; `COMMIT` is next.
    BatchBeforeCommit,
    /// Right after a batch's `COMMIT`.
    BatchAfterCommit,
    /// The supersede and new-session (or resume) transaction committed.
    SessionCommitted,
    /// A session's terminal state committed: completed (finalize), or
    /// cancelled, stopped or failed.
    TerminalCommitted,
    /// `begin_write`: partial files and provenance temporaries swept.
    RecoverySwept,
    /// `begin_write`: blob intents reconciled.
    RecoveryReconciled,
    /// `begin_write`: running sessions marked interrupted.
    RecoveryMarked,
    /// P6 phase A: a partial file was created; the copy is next.
    AttachmentPartialCreated,
    /// P6 phase A: the first write of a copy into its partial file.
    AttachmentCopying,
    /// P6 phase A: a partial file is written and synced (`sync_all`).
    AttachmentSynced,
    /// P6 phase B: the batch's blob intents committed.
    AttachmentIntentCommitted,
    /// P6 phase C: a file was renamed into the store; its directory's fsync
    /// is next.
    AttachmentPlaced,
    /// P6 phase C: the store directories are flushed; the row batch (phase
    /// D) is next.
    AttachmentDirSynced,
    /// Archive creation: the `.oma-new-*` directory is complete and synced;
    /// the rename is next.
    CreateBeforeRename,
    /// Archive creation: renamed into place; the library directory's fsync
    /// is next.
    CreateAfterRename,
    /// The import is about to ask the gate (spec §8.2 step 7); nothing has
    /// been written.
    GateWaiting,
    /// Finalize: `chat_stats` and `archive_stats` committed.
    FinalizeStats,
    /// Finalize: one chunk of the session-local cleanup committed.
    FinalizeCleanup,
    /// Finalize: one FTS merge step committed (between steps).
    FtsMergeStep,
    /// Finalize: `wal_checkpoint(TRUNCATE)` is next.
    Checkpoint,
    /// Provenance: `imports/session-<n>.json.tmp` was created; its content
    /// is next.
    ProvenanceTmpCreated,
    /// Provenance: the `.tmp` file is written and synced; the rename is
    /// next.
    ProvenanceBeforeRename,
    /// A migration step's DDL and data ran inside its transaction; its
    /// checks are next.
    MigrationStep,
    /// A migration step passed its checks; its `COMMIT` is next.
    MigrationBeforeCommit,
}

#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
impl FaultPoint {
    /// Every point, in a fixed order.
    pub const ALL: [FaultPoint; 25] = [
        FaultPoint::Record,
        FaultPoint::BatchBeforeCommit,
        FaultPoint::BatchAfterCommit,
        FaultPoint::SessionCommitted,
        FaultPoint::TerminalCommitted,
        FaultPoint::RecoverySwept,
        FaultPoint::RecoveryReconciled,
        FaultPoint::RecoveryMarked,
        FaultPoint::AttachmentPartialCreated,
        FaultPoint::AttachmentCopying,
        FaultPoint::AttachmentSynced,
        FaultPoint::AttachmentIntentCommitted,
        FaultPoint::AttachmentPlaced,
        FaultPoint::AttachmentDirSynced,
        FaultPoint::CreateBeforeRename,
        FaultPoint::CreateAfterRename,
        FaultPoint::GateWaiting,
        FaultPoint::FinalizeStats,
        FaultPoint::FinalizeCleanup,
        FaultPoint::FtsMergeStep,
        FaultPoint::Checkpoint,
        FaultPoint::ProvenanceTmpCreated,
        FaultPoint::ProvenanceBeforeRename,
        FaultPoint::MigrationStep,
        FaultPoint::MigrationBeforeCommit,
    ];

    /// The point's name in `OMA_TEST_FAULT` and in `FAULT` lines.
    pub fn name(self) -> &'static str {
        match self {
            FaultPoint::Record => "record",
            FaultPoint::BatchBeforeCommit => "batch-before-commit",
            FaultPoint::BatchAfterCommit => "batch-after-commit",
            FaultPoint::SessionCommitted => "session-committed",
            FaultPoint::TerminalCommitted => "terminal-committed",
            FaultPoint::RecoverySwept => "recovery-swept",
            FaultPoint::RecoveryReconciled => "recovery-reconciled",
            FaultPoint::RecoveryMarked => "recovery-marked",
            FaultPoint::AttachmentPartialCreated => "attachment-partial-created",
            FaultPoint::AttachmentCopying => "attachment-copying",
            FaultPoint::AttachmentSynced => "attachment-synced",
            FaultPoint::AttachmentIntentCommitted => "attachment-intent-committed",
            FaultPoint::AttachmentPlaced => "attachment-placed",
            FaultPoint::AttachmentDirSynced => "attachment-dir-synced",
            FaultPoint::CreateBeforeRename => "create-before-rename",
            FaultPoint::CreateAfterRename => "create-after-rename",
            FaultPoint::GateWaiting => "gate-waiting",
            FaultPoint::FinalizeStats => "finalize-stats",
            FaultPoint::FinalizeCleanup => "finalize-cleanup",
            FaultPoint::FtsMergeStep => "fts-merge-step",
            FaultPoint::Checkpoint => "checkpoint",
            FaultPoint::ProvenanceTmpCreated => "provenance-tmp-created",
            FaultPoint::ProvenanceBeforeRename => "provenance-before-rename",
            FaultPoint::MigrationStep => "migration-step",
            FaultPoint::MigrationBeforeCommit => "migration-before-commit",
        }
    }

    /// The point with this name.
    pub fn from_name(name: &str) -> Option<FaultPoint> {
        FaultPoint::ALL.into_iter().find(|p| p.name() == name)
    }
}

/// A fault point: a hit is counted, and an armed action may run, with the
/// `fault-injection` feature; nothing happens without it.
#[inline]
pub(crate) fn point(p: FaultPoint) {
    #[cfg(feature = "fault-injection")]
    {
        hooks::hit(p);
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        let _ = p;
    }
}

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
        /// Fail every import batch just before its `COMMIT` with this
        /// SQLite primary result code (the batch then rolls back).
        pub batch_error: Option<i32>,
        /// Fail every attachment intent transaction (spec §9.1 phase B)
        /// just before its `COMMIT` with this SQLite primary result code.
        pub intent_error: Option<i32>,
        /// Fail the verifier's read of the `blob` table with
        /// `SQLITE_CORRUPT`: `0` its row count, `n` its n-th page.
        pub verify_blob_error: Option<u32>,
        /// Fail every attachment copy's first write to its partial file with
        /// this raw OS error code (for example `EFBIG`).
        pub copy_write_error: Option<i32>,
        /// The space guard sets `max_page_count` to the current page count
        /// plus this many pages (negative: below the current size), whatever
        /// the space probe says.
        pub max_page_room: Option<i64>,
        /// Fail finalize's terminal bookkeeping (the "completed" update)
        /// with this SQLite primary result code.
        pub terminal_error: Option<i32>,
        /// Fail every provenance file write before its `.tmp` is created.
        pub provenance_fails: bool,
        /// Replace every viewer phase's op budget (VM operations; M5 §3.2).
        pub view_op_budget: Option<u64>,
        /// Set a viewer phase's cancel token at the n-th tick of its
        /// progress handler (counted from 1 per phase): a cancel that
        /// arrives in the middle of a statement (M5 §13.4).
        pub cancel_at_view_tick: Option<u64>,
        /// Replace the largest chat (in timeline rows) that a chat-scoped
        /// words search walks itself (M5 §4.3); larger chats use the global
        /// plan with a chat filter. `Some(0)` forces the global plan.
        pub scoped_search_rows: Option<u64>,
        /// Replace the most messages one exact-text scan examines
        /// (M5 §4.4).
        pub scan_examined_limit: Option<u64>,
        /// Replace the most messages one words search keeps
        /// (`SEARCH_MATCH_CAP`, M5 §4.3).
        pub search_match_cap: Option<usize>,
        /// Replace the 2 s window of a page call (`thread_page`,
        /// `reply_thread`; milliseconds).
        pub page_window_ms: Option<u64>,
        /// Sleep this long between a page call's `Scan` and `Assembly`
        /// phases: a `Scan` that used up the call's deadline (M5 §3.2).
        pub pause_after_scan_ms: Option<u64>,
        /// Replace the deadline of the `highlight()` phase of
        /// `search_hits` (milliseconds; M5 §4.5). The op budget cannot see
        /// work inside FTS5's own calls, so the deadline is what stops it.
        pub hits_window_ms: Option<u64>,
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

    pub use super::FaultPoint;

    /// The import's index-dependent statements, by name, exactly as the
    /// code prepares them (for `tests/query_plans.rs`).
    pub fn plan_statements() -> Vec<(&'static str, String)> {
        crate::import::plan_statements()
    }

    /// The viewer's index-dependent statements, by name, exactly as the
    /// code prepares them (for `tests/query_plans.rs`, M5 §3.5).
    pub fn viewer_statements() -> Vec<(&'static str, String)> {
        let mut v = crate::view::plan_statements();
        v.extend(crate::search::plan_statements());
        v.extend(crate::media::plan_statements());
        v
    }

    /// The export reads' statements, by name, exactly as the code
    /// prepares them (for `tests/query_plans.rs`, M6 spec §3.4).
    pub fn export_statements() -> Vec<(&'static str, String)> {
        crate::export::plan_statements()
    }

    /// Whether any statement on the snapshot's connection has stepped
    /// and not been reset (`sqlite3_stmt_busy` over every statement): the
    /// cursors materialise and reset before they return (M6 spec §3.2).
    /// Test-only.
    pub fn statements_stepping(snapshot: &crate::ExportSnapshot<'_>) -> bool {
        snapshot.statements_stepping()
    }

    pub use crate::bulk::{sort_key, BulkFinalize, BulkWriter};

    /// Caps to lower below [`Caps::EXPORT`](crate::view::assemble::Caps)
    /// in a test, so a missed cap can be shown to be reported (M6 spec
    /// §14.1). `None` keeps the export's "no cap".
    #[derive(Debug, Clone, Copy, Default)]
    pub struct LowerCaps {
        pub body_utf16: Option<u32>,
        pub identifier_utf16: Option<u32>,
        pub version_utf16: Option<u32>,
        pub subject_utf16: Option<u32>,
        pub emoji_bytes: Option<usize>,
        pub attachments: Option<usize>,
        pub reactions: Option<u64>,
    }

    fn export_caps(lower: LowerCaps) -> crate::view::assemble::Caps {
        let e = crate::view::assemble::Caps::EXPORT;
        crate::view::assemble::Caps {
            body_utf16: lower.body_utf16.unwrap_or(e.body_utf16),
            identifier_utf16: lower.identifier_utf16.unwrap_or(e.identifier_utf16),
            version_utf16: lower.version_utf16.unwrap_or(e.version_utf16),
            subject_utf16: lower.subject_utf16.unwrap_or(e.subject_utf16),
            emoji_bytes: lower.emoji_bytes.unwrap_or(e.emoji_bytes),
            attachments: lower.attachments.unwrap_or(e.attachments),
            reactions: lower.reactions.unwrap_or(e.reactions),
            ..e
        }
    }

    /// One message assembled as an export will assemble it (M6 slice 6a):
    /// `Caps::EXPORT` lowered by `lower`, base and extras, the raw codes,
    /// in one read transaction with no op budget, then the cap check that
    /// runs in every build. Test-only: exports read through
    /// `ArchiveReader::with_export_snapshot`.
    pub fn export_item(
        reader: &crate::ArchiveReader,
        chat: Option<crate::ChatId>,
        id: crate::MessageId,
        reveal: crate::Reveal,
        lower: LowerCaps,
    ) -> Result<crate::ExportItem, crate::ArchiveError> {
        reader.export_item(
            chat,
            id,
            reveal,
            export_caps(lower),
            &crate::CancelToken::new(),
        )
    }

    /// One handle's address under `Caps::EXPORT` lowered by `lower` (M6
    /// slice 6a). Test-only.
    pub fn export_handle(
        reader: &crate::ArchiveReader,
        id: crate::HandleId,
        lower: LowerCaps,
    ) -> Result<crate::Participant, crate::ArchiveError> {
        reader.export_handle(id, export_caps(lower), &crate::CancelToken::new())
    }

    /// `message_detail` under `Caps::EXPORT` lowered by `lower`: the
    /// details' caps threaded through (M6 slice 6a). Test-only.
    pub fn export_detail(
        reader: &crate::ArchiveReader,
        id: crate::MessageId,
        reveal: crate::Reveal,
        lower: LowerCaps,
    ) -> Result<crate::MessageDetail, crate::ArchiveError> {
        reader.detail_with(id, reveal, export_caps(lower))
    }

    /// A write handle on `archive` for generating schema-valid v1 rows
    /// directly (M5 spec §14.2): the archive's own ReadWrite connection,
    /// after the usual first-write recovery, with the import's transaction
    /// boundary and finalize steps. Test-only.
    pub fn bulk_writer(
        archive: &mut crate::Archive,
    ) -> Result<BulkWriter<'_>, crate::ArchiveError> {
        BulkWriter::new(archive)
    }

    /// An action run at the n-th hit of a fault point.
    pub type FaultAction = Box<dyn FnMut(FaultPoint, u32)>;

    struct Trip {
        point: FaultPoint,
        nth: u32,
        action: Option<FaultAction>,
    }

    thread_local! {
        static HITS: RefCell<std::collections::BTreeMap<FaultPoint, u32>> =
            const { RefCell::new(std::collections::BTreeMap::new()) };
        static TRIP: RefCell<Option<Trip>> = const { RefCell::new(None) };
    }

    /// Disarms the trip (and clears the hit counts) when dropped.
    pub struct TripGuard(());

    impl Drop for TripGuard {
        fn drop(&mut self) {
            TRIP.with(|t| *t.borrow_mut() = None);
            HITS.with(|h| h.borrow_mut().clear());
        }
    }

    /// Run `action` at the `nth` hit (1-based) of `point` on this thread,
    /// counting from now. Hit counts restart at zero.
    #[must_use]
    pub fn arm(point: FaultPoint, nth: u32, action: FaultAction) -> TripGuard {
        HITS.with(|h| h.borrow_mut().clear());
        TRIP.with(|t| {
            *t.borrow_mut() = Some(Trip {
                point,
                nth,
                action: Some(action),
            });
        });
        TripGuard(())
    }

    /// Count hits from zero without an action (to learn how many hits a
    /// run has).
    #[must_use]
    pub fn count() -> TripGuard {
        HITS.with(|h| h.borrow_mut().clear());
        TRIP.with(|t| *t.borrow_mut() = None);
        TripGuard(())
    }

    /// The hits of `point` on this thread since the last [`arm`] or
    /// [`count`].
    pub fn hits(point: FaultPoint) -> u32 {
        HITS.with(|h| h.borrow().get(&point).copied().unwrap_or(0))
    }

    /// What the ordering test sees, in order (spec §18.4 `ordering.rs`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Observed {
        /// A fault point was reached.
        Point(FaultPoint),
        /// A transaction on the archive's write connection is committing
        /// (rusqlite's commit hook).
        Commit,
    }

    /// Receives every [`Observed`] event on this thread.
    pub type Observer = Box<dyn FnMut(Observed)>;

    thread_local! {
        static OBSERVER: RefCell<Option<Observer>> = const { RefCell::new(None) };
    }

    /// Removes the observer when dropped.
    pub struct ObserveGuard(());

    impl Drop for ObserveGuard {
        fn drop(&mut self) {
            OBSERVER.with(|o| *o.borrow_mut() = None);
        }
    }

    /// Send every fault point hit and every write-connection commit on
    /// this thread to `observer` until the guard is dropped. Write
    /// connections opened while it is installed get the commit hook.
    #[must_use]
    pub fn observe(observer: Observer) -> ObserveGuard {
        OBSERVER.with(|o| *o.borrow_mut() = Some(observer));
        ObserveGuard(())
    }

    pub(crate) fn observing() -> bool {
        OBSERVER.with(|o| o.borrow().is_some())
    }

    pub(crate) fn notify(event: Observed) {
        // Take the observer out while it runs, so it may not re-enter.
        let taken = OBSERVER.with(|o| o.borrow_mut().take());
        if let Some(mut f) = taken {
            f(event);
            OBSERVER.with(|o| {
                let mut slot = o.borrow_mut();
                if slot.is_none() {
                    *slot = Some(f);
                }
            });
        }
    }

    pub(crate) fn hit(p: FaultPoint) {
        notify(Observed::Point(p));
        let n = HITS.with(|h| {
            let mut h = h.borrow_mut();
            let n = h.entry(p).or_insert(0);
            *n = n.saturating_add(1);
            *n
        });
        // Take the action out while it runs, so it may itself reach a
        // fault point without a re-entrant borrow.
        let action = TRIP.with(|t| {
            let mut t = t.borrow_mut();
            match t.as_mut() {
                Some(trip) if trip.point == p && trip.nth == n => trip.action.take(),
                _ => None,
            }
        });
        if let Some(mut a) = action {
            a(p, n);
            TRIP.with(|t| {
                if let Some(trip) = t.borrow_mut().as_mut() {
                    trip.action = Some(a);
                }
            });
        }
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

/// The viewer phase budget to use instead of `ops`, if a test set one.
#[inline]
pub(crate) fn view_op_budget(ops: u64) -> u64 {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current().view_op_budget.unwrap_or(ops)
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        ops
    }
}

/// The tick of a viewer phase at which its cancel token is set, if a test
/// set one.
#[inline]
pub(crate) fn cancel_at_view_tick() -> Option<u64> {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current().cancel_at_view_tick
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        None
    }
}

/// The words search's match cap to use instead of `n`, if a test set one.
#[inline]
pub(crate) fn search_match_cap(n: usize) -> usize {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current().search_match_cap.unwrap_or(n)
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        n
    }
}

/// The scoped-search threshold to use instead of `rows`, if a test set one.
#[inline]
pub(crate) fn scoped_search_rows(rows: u64) -> u64 {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current().scoped_search_rows.unwrap_or(rows)
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        rows
    }
}

/// The exact-text scan's examined limit to use instead of `n`, if a test
/// set one.
#[inline]
pub(crate) fn scan_examined_limit(n: u64) -> u64 {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current().scan_examined_limit.unwrap_or(n)
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        n
    }
}

/// A page call's window to use instead of `w`, if a test set one.
#[inline]
pub(crate) fn page_window(w: std::time::Duration) -> std::time::Duration {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current()
            .page_window_ms
            .map_or(w, std::time::Duration::from_millis)
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        w
    }
}

/// Sleep between a page call's `Scan` and `Assembly`, if a test asked.
#[inline]
pub(crate) fn pause_after_scan() {
    #[cfg(feature = "fault-injection")]
    {
        if let Some(ms) = hooks::current().pause_after_scan_ms {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }
}

/// The `highlight()` phase's window to use instead of `w`, if a test set
/// one.
#[inline]
pub(crate) fn hits_window(w: std::time::Duration) -> std::time::Duration {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current()
            .hits_window_ms
            .map_or(w, std::time::Duration::from_millis)
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        w
    }
}

/// The error an import batch must fail with, if a test set one.
#[inline]
pub(crate) fn batch_error() -> Option<rusqlite::Error> {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current()
            .batch_error
            .map(|code| rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None))
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        None
    }
}

/// The error an attachment intent transaction must fail with, if a test
/// set one.
#[inline]
pub(crate) fn intent_error() -> Option<rusqlite::Error> {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current()
            .intent_error
            .map(|code| rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None))
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        None
    }
}

/// Whether the verifier's `blob` read must fail at `step` (0: the row
/// count; n: the n-th page), if a test set it.
#[inline]
pub(crate) fn verify_blob_error(step: u32) -> rusqlite::Result<()> {
    #[cfg(feature = "fault-injection")]
    {
        if hooks::current().verify_blob_error == Some(step) {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                None,
            ));
        }
        Ok(())
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        let _ = step;
        Ok(())
    }
}

/// The raw OS error an attachment copy's first write must fail with, if a
/// test set one.
#[inline]
pub(crate) fn copy_write_error() -> Option<i32> {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current().copy_write_error
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        None
    }
}

/// The page room the space guard must give, if a test set one.
#[inline]
pub(crate) fn max_page_room() -> Option<i64> {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current().max_page_room
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        None
    }
}

/// The error finalize's terminal bookkeeping must fail with, if a test set
/// one.
#[inline]
pub(crate) fn terminal_error() -> Option<rusqlite::Error> {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current()
            .terminal_error
            .map(|code| rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None))
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        None
    }
}

/// The provenance write must fail.
#[inline]
pub(crate) fn provenance_fails() -> bool {
    #[cfg(feature = "fault-injection")]
    {
        hooks::current().provenance_fails
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        false
    }
}

/// Give a new write connection the commit hook when a test observes this
/// thread (`hooks::observe`); nothing without the feature.
#[inline]
pub(crate) fn observe_commits(conn: &rusqlite::Connection) {
    #[cfg(feature = "fault-injection")]
    {
        if hooks::observing() {
            let _ = conn.commit_hook(Some(|| {
                hooks::notify(hooks::Observed::Commit);
                false
            }));
        }
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        let _ = conn;
    }
}
