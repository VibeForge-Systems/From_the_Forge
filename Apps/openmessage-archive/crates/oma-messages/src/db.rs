//! Staging, the hardened connection, the probe gate and the record passes
//! (M3 §4.4, §7).
//!
//! **Statement shape (§7.1).** Each record query is built once per table from
//! the static list, filtered to the columns that are present. Identifiers are
//! always double-quoted and double-quoted string literals are off, so a
//! builder or probe mismatch fails to prepare instead of returning the column
//! name as data. Values are read through a position map built with the SQL.
//! Content columns are selected under their caps (§5.4):
//!
//! ```sql
//! SELECT "ROWID", CASE WHEN octet_length("id") <= 4096 THEN "id" END, octet_length("id"), ...
//! FROM "handle" WHERE "ROWID" > ?1 ORDER BY "ROWID" LIMIT ?2   -- Cursor::After
//! -- Cursor::Start: no WHERE clause; ORDER BY "ROWID" LIMIT ?1
//! ```
//!
//! **Keyset paging.** Tables keyed by a validated `ROWID` alias page on it;
//! join tables page on their implicit `rowid`. There are no joins in record
//! queries. One row at a time goes from the stepping statement to the
//! callback.
//!
//! **Break and resume.** The callback returns `ControlFlow<B>`. `last` is the
//! last rowid delivered; resuming from `Cursor::After(last)` against the same
//! staged content (`oma_backup::ContentDigest`) yields exactly the remaining
//! rows. `Cursor::Start` has no lower bound, so a row with rowid `i64::MIN`
//! is not skipped.
//!
//! **Cancellation (§7.2).** The token is checked at every phase boundary,
//! after every statement, between pages, and every 128 rows inside a page.
//! Inside a long statement, oma-backup's progress handler and interrupt
//! watcher stop it.

use std::fmt;
use std::ops::ControlFlow;

use oma_backup::{
    OpenBackup, ReadPolicy, StagedConnection, StagedDatabase, StagedDbLimits, WalPolicy,
    SMS_DB_DOMAIN, SMS_DB_PATH,
};
use rusqlite::types::ValueRef;
use rusqlite::{Row, Statement};

use crate::attachments::AttachmentLocator;
use crate::col::Col;
use crate::col::IssueKind;
use crate::error::{BackupStage, MessagesError, Stage};
use crate::guidset::GuidSets;
use crate::progress::{BackupSinkAdapter, MessagesCtx, MessagesPhase};
use crate::records::{
    AttachmentRecord, ChatHandleLink, ChatMessageLink, ChatRecord, HandleRecord,
    MessageAttachmentLink, MessageReadOptions, MessageRecord, RecoverableLink,
};
use crate::redact::{RawBlob, Redacted, SqlText};
use crate::schema::{
    self, Capabilities, ColumnKind, DamageReason, IncompleteReason, Integrity, KnownColumn,
    KnownTable, Probe, TableCaps, TableLayout, Unsupported, UnsupportedReason,
};
use crate::time::CocoaTime;
use crate::value::{self, BlobSize, SqlType, ValueIssues};

/// The SQLite limits for the staged sms.db (§7.4). Each is clamped to
/// `StagedDbLimits::ceiling()`, so it can only tighten the hardened open.
///
/// | Limit | Value | Reason |
/// |---|---|---|
/// | `length` | 32 MiB | A backstop above every cap; reached only if a cap is wrong |
/// | `sql_length` | 16 KiB | Our longest statement must fit (a test asserts the margin); schema SQL too |
/// | `column` | 2000 | Covers wide `message` tables |
/// | `expr_depth` | 50 | |
/// | `vdbe_op` | 25,000 | Program size; a test asserts every statement is below half |
/// | `function_arg` | 8 | Also applies to trigger SQL parsed with the schema |
/// | `like_pattern_length` | 50 | |
/// | `variable_number` | 8 | At most two parameters are bound; caps are literals |
///
/// How much headroom real iOS DDL has under these limits is **UNVERIFIED**.
pub const SMS_DB_LIMITS: StagedDbLimits = StagedDbLimits::ceiling()
    .with_length(32 * 1024 * 1024)
    .with_sql_length(16 * 1024)
    .with_column(2000)
    .with_expr_depth(50)
    .with_vdbe_op(25_000)
    .with_function_arg(8)
    .with_like_pattern_length(50)
    .with_variable_number(8);

/// The read policy for sms.db: `recoverable_message_part.part_text` (the
/// text of Recently Deleted messages) is never read. The authorizer
/// enforces it, so it does not depend on convention.
pub const SMS_READ_POLICY: ReadPolicy =
    ReadPolicy::deny_columns(&[("recoverable_message_part", "part_text")]);

/// The default rows per page.
pub const PAGE_ROWS_DEFAULT: u32 = 512;
/// The smallest page.
pub const PAGE_ROWS_MIN: u32 = 16;
/// The largest page.
pub const PAGE_ROWS_MAX: u32 = 4096;

/// The cancellation check inside a page runs every this many rows.
const CHECK_EVERY_ROWS: u64 = 128;

/// `quick_check` op budget: `QC_OPS_PER_PAGE × page_count + QC_OPS_BASE`.
/// On two synthetic sms.db stand-ins (87 and 810 pages, a table and an
/// index, bundled SQLite 3.53.2), the smallest per-page budget that passed
/// was 311 and 334 operations; this allows over 100×. The perf test of a
/// later slice sets the final value. **UNVERIFIED** for real data.
pub(crate) const QC_OPS_PER_PAGE: u64 = 40_000;
/// See [`QC_OPS_PER_PAGE`].
pub(crate) const QC_OPS_BASE: u64 = 1_000_000;

/// Copy sms.db (and its `-wal`, under `wal`) into the backup's staging
/// session (§2 step a). Progress is reported as
/// [`MessagesPhase::StagingMessagesDb`], one combined byte total.
pub fn stage<'a>(
    ob: &'a OpenBackup,
    wal: WalPolicy,
    ctx: &mut MessagesCtx<'_>,
) -> Result<StagedDatabase<'a>, MessagesError> {
    ctx.check()?;
    ctx.report(MessagesPhase::StagingMessagesDb, 0, None);
    let mut adapter = BackupSinkAdapter {
        inner: &mut *ctx.progress,
    };
    let mut job = oma_backup::JobCtx::new(ctx.cancel, &mut adapter);
    ob.extract_database_with(SMS_DB_DOMAIN, SMS_DB_PATH, wal, &mut job)
        .map_err(|e| MessagesError::from_backup(e, BackupStage::Staging))
}

/// The result of [`connect`].
#[derive(Debug)]
pub enum Connect<'s> {
    /// The hardened, read-only connection.
    Ready(StagedConnection<'s>),
    /// The schema loads only above [`SMS_DB_LIMITS`]: the database is
    /// [`UnsupportedReason::SchemaExceedsLimits`].
    SchemaExceedsLimits,
}

impl Connect<'_> {
    /// The probe result for [`Connect::SchemaExceedsLimits`].
    pub fn unsupported() -> Unsupported {
        Unsupported::schema_exceeds_limits()
    }
}

/// Open the staged sms.db hardened, under [`SMS_DB_LIMITS`] and
/// [`SMS_READ_POLICY`] (§2 step c). The connection borrows `staged`, so the
/// staged files cannot be removed while it is open.
pub fn connect<'s>(
    staged: &'s StagedDatabase<'_>,
    ctx: &MessagesCtx<'_>,
) -> Result<Connect<'s>, MessagesError> {
    ctx.check()?;
    match staged.open_with(ctx.cancel, &SMS_DB_LIMITS, &SMS_READ_POLICY) {
        Ok(c) => Ok(Connect::Ready(c)),
        Err(oma_backup::BackupError::SchemaExceedsLimits) => Ok(Connect::SchemaExceedsLimits),
        Err(e) => Err(MessagesError::from_backup(e, BackupStage::Opening)),
    }
}

/// Options for [`SmsDb::open_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProbeOptions {
    page_rows: u32,
    integrity_ops_per_page: u64,
    integrity_ops_base: u64,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        ProbeOptions {
            page_rows: PAGE_ROWS_DEFAULT,
            integrity_ops_per_page: QC_OPS_PER_PAGE,
            integrity_ops_base: QC_OPS_BASE,
        }
    }
}

impl ProbeOptions {
    /// Rows per page, clamped to `PAGE_ROWS_MIN..=PAGE_ROWS_MAX`.
    pub fn with_page_rows(mut self, n: u32) -> Self {
        self.page_rows = n.clamp(PAGE_ROWS_MIN, PAGE_ROWS_MAX);
        self
    }

    /// The `quick_check` op budget, `per_page × page_count + base`. It can
    /// only be tightened below the defaults, never loosened.
    pub fn with_integrity_budget(mut self, per_page: u64, base: u64) -> Self {
        self.integrity_ops_per_page = per_page.min(QC_OPS_PER_PAGE);
        self.integrity_ops_base = base.min(QC_OPS_BASE);
        self
    }

    /// Rows per page.
    pub fn page_rows(&self) -> u32 {
        self.page_rows
    }
}

/// A position into a table's keyset order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    /// From the first row; no lower bound.
    Start,
    /// The rows whose key is greater than this one.
    After(i64),
}

/// What one pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PassOutcome<B> {
    /// Rows delivered to the callback by this call.
    pub rows: u64,
    /// The key of the last row delivered; resume from `Cursor::After(last)`.
    pub last: Option<i64>,
    /// The value the callback stopped with, if it stopped.
    pub broke: Option<B>,
    /// Value-level issues in the rows delivered.
    pub value_issues: ValueIssues,
}

impl<B> PassOutcome<B> {
    fn empty() -> Self {
        PassOutcome {
            rows: 0,
            last: None,
            broke: None,
            value_issues: ValueIssues::default(),
        }
    }
}

/// One table's record query: the SQL for both cursor forms, and where each
/// known column landed in the result.
pub(crate) struct Select {
    table: KnownTable,
    sql_start: String,
    sql_after: String,
    /// Indexed by the column's index in the known list.
    pos: Vec<Option<usize>>,
}

impl Select {
    fn build(t: KnownTable, tc: &TableCaps) -> Option<Self> {
        let key = match t.layout() {
            TableLayout::RowidAlias => "\"ROWID\"",
            TableLayout::Join => "rowid",
            TableLayout::CountOnly | TableLayout::Properties => return None,
        };
        let mut exprs: Vec<String> = Vec::new();
        let mut pos = Vec::new();
        if t.layout() == TableLayout::Join {
            exprs.push(key.to_owned());
        }
        for c in t.known_columns() {
            if !tc.present_columns.contains(c) {
                pos.push(None);
                continue;
            }
            let name = c.name();
            let at = exprs.len();
            match c.kind() {
                Some(ColumnKind::RowId) => exprs.push(format!("\"{name}\"")),
                Some(ColumnKind::Integer | ColumnKind::Bool | ColumnKind::Time) => {
                    exprs.push(format!("\"{name}\""));
                }
                Some(ColumnKind::Text | ColumnKind::Blob) => {
                    let cap = c.cap().unwrap_or(0);
                    exprs.push(format!(
                        "CASE WHEN octet_length(\"{name}\") <= {cap} THEN \"{name}\" END"
                    ));
                    exprs.push(format!("octet_length(\"{name}\")"));
                }
                Some(ColumnKind::BlobSize) => {
                    exprs.push(format!("typeof(\"{name}\")"));
                    exprs.push(format!("octet_length(\"{name}\")"));
                }
                Some(ColumnKind::TypeOnly) => exprs.push(format!("typeof(\"{name}\")")),
                None => {
                    pos.push(None);
                    continue;
                }
            }
            pos.push(Some(at));
        }
        let cols = exprs.join(", ");
        let table = t.name();
        Some(Select {
            table: t,
            sql_start: format!("SELECT {cols} FROM \"{table}\" ORDER BY {key} LIMIT ?1"),
            sql_after: format!(
                "SELECT {cols} FROM \"{table}\" WHERE {key} > ?1 ORDER BY {key} LIMIT ?2"
            ),
            pos,
        })
    }

    fn at(&self, index: u8) -> Option<usize> {
        self.pos.get(usize::from(index)).copied().flatten()
    }
}

/// Reads one row's known columns through a [`Select`]'s position map.
pub(crate) struct RowReader<'a, 'r> {
    row: &'a Row<'r>,
    sel: &'a Select,
    issues: &'a mut ValueIssues,
}

impl<'a> RowReader<'a, '_> {
    fn get(&self, i: usize) -> Result<ValueRef<'a>, MessagesError> {
        let row: &'a Row<'_> = self.row;
        row.get_ref(i)
            .map_err(|_| MessagesError::Internal("result column out of range"))
    }

    fn pair(&self, i: usize) -> Result<(ValueRef<'a>, ValueRef<'a>), MessagesError> {
        let next = i
            .checked_add(1)
            .ok_or(MessagesError::Internal("result column out of range"))?;
        Ok((self.get(i)?, self.get(next)?))
    }

    fn column(&self, index: u8) -> KnownColumn {
        KnownColumn::new(self.sel.table, index)
    }

    pub(crate) fn integer(&mut self, index: u8) -> Result<Col<i64>, MessagesError> {
        match self.sel.at(index) {
            None => Ok(Col::Absent),
            Some(i) => Ok(value::integer(self.get(i)?, self.issues)),
        }
    }

    pub(crate) fn boolean(&mut self, index: u8) -> Result<Col<bool>, MessagesError> {
        match self.sel.at(index) {
            None => Ok(Col::Absent),
            Some(i) => Ok(value::boolean(self.get(i)?, self.issues)),
        }
    }

    pub(crate) fn time(&mut self, index: u8) -> Result<Col<CocoaTime>, MessagesError> {
        match self.sel.at(index) {
            None => Ok(Col::Absent),
            Some(i) => Ok(value::time(self.get(i)?, self.issues)),
        }
    }

    pub(crate) fn text(&mut self, index: u8) -> Result<Col<Redacted<SqlText>>, MessagesError> {
        match self.sel.at(index) {
            None => Ok(Col::Absent),
            Some(i) => {
                let (v, len) = self.pair(i)?;
                Ok(value::text(v, len, self.column(index), self.issues))
            }
        }
    }

    pub(crate) fn blob(&mut self, index: u8) -> Result<Col<Redacted<RawBlob>>, MessagesError> {
        match self.sel.at(index) {
            None => Ok(Col::Absent),
            Some(i) => {
                let (v, len) = self.pair(i)?;
                Ok(value::blob(v, len, self.column(index), self.issues))
            }
        }
    }

    pub(crate) fn type_only(&mut self, index: u8) -> Result<Col<SqlType>, MessagesError> {
        match self.sel.at(index) {
            None => Ok(Col::Absent),
            Some(i) => Ok(value::type_only(self.get(i)?, self.issues)),
        }
    }

    /// A value found unreadable after it was read (for example a property
    /// list that does not parse), counted with the pass's issues.
    pub(crate) fn unreadable<T>(&mut self, kind: IssueKind) -> Col<T> {
        self.issues.unreadable(kind)
    }

    pub(crate) fn blob_size(&mut self, index: u8) -> Result<Col<BlobSize>, MessagesError> {
        match self.sel.at(index) {
            None => Ok(Col::Absent),
            Some(i) => {
                let (ty, len) = self.pair(i)?;
                Ok(value::blob_size(ty, len, self.column(index), self.issues))
            }
        }
    }
}

/// A record built from one row of its table.
pub(crate) trait FromRow: Sized {
    /// The table the record comes from.
    const TABLE: KnownTable;
    /// The pass's phase.
    const PHASE: MessagesPhase;
    /// The stage SQLite errors are attributed to.
    const STAGE: Stage;
    /// Build the record. `rowid` is the row's key, already read strictly.
    fn from_row(rowid: i64, r: &mut RowReader<'_, '_>) -> Result<Self, MessagesError>;
}

/// Which table a pass reads, and how it reports.
#[derive(Clone, Copy)]
struct PassSpec {
    table: KnownTable,
    phase: MessagesPhase,
    stage: Stage,
}

/// A staged sms.db that passed the probe: the gate and the streaming API.
/// It borrows the connection, which borrows the staged database, so nothing
/// outlives staging and `StagedDatabase::remove` cannot run early.
pub struct SmsDb<'s> {
    conn: &'s StagedConnection<'s>,
    caps: Capabilities,
    selects: Vec<Select>,
    opts: ProbeOptions,
}

impl fmt::Debug for SmsDb<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmsDb")
            .field("capabilities", &self.caps)
            .field("options", &self.opts)
            .finish_non_exhaustive()
    }
}

impl<'s> SmsDb<'s> {
    /// Scan, `quick_check`, then probe (§6.2), with default options.
    /// Unsupported, damaged and incomplete databases are values (D28).
    pub fn open(
        conn: &'s StagedConnection<'s>,
        ctx: &mut MessagesCtx<'_>,
    ) -> Result<Probe<'s>, MessagesError> {
        Self::open_with(conn, &ProbeOptions::default(), ctx)
    }

    /// [`SmsDb::open`] with options.
    pub fn open_with(
        conn: &'s StagedConnection<'s>,
        opts: &ProbeOptions,
        ctx: &mut MessagesCtx<'_>,
    ) -> Result<Probe<'s>, MessagesError> {
        ctx.check()?;
        ctx.report(MessagesPhase::ScanningSchema, 0, None);
        let scan = schema::scan(conn, ctx)?;
        if scan.virtual_tables > 0 {
            return Ok(Probe::Unsupported(Unsupported {
                reasons: vec![UnsupportedReason::VirtualTablePresent],
                capabilities: Capabilities::from_scan(&scan),
            }));
        }

        ctx.check()?;
        ctx.report(MessagesPhase::CheckingIntegrity, 0, None);
        let pages = conn
            .page_count()
            .map_err(|e| MessagesError::from_conn(e, Stage::Integrity))?;
        let ops = opts
            .integrity_ops_per_page
            .saturating_mul(pages)
            .saturating_add(opts.integrity_ops_base);
        match schema::quick_check(conn, ops)? {
            Integrity::Ok => {}
            Integrity::Problems => return Ok(Probe::Damaged(DamageReason::IntegrityProblems)),
            Integrity::OverBudget => {
                return Ok(Probe::Incomplete(
                    IncompleteReason::IntegrityCheckOverBudget,
                ))
            }
        }

        ctx.check()?;
        let (caps, reasons) = schema::probe(conn, &scan, ctx)?;
        if !reasons.is_empty() {
            return Ok(Probe::Unsupported(Unsupported {
                reasons,
                capabilities: caps,
            }));
        }
        let selects = caps
            .tables
            .iter()
            .filter(|tc| tc.present)
            .filter_map(|tc| Select::build(tc.table, tc))
            .collect();
        Ok(Probe::Supported(SmsDb {
            conn,
            caps,
            selects,
            opts: *opts,
        }))
    }

    /// What the probe found.
    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// The connection, for the end-of-check assertions (§7.5).
    pub fn connection(&self) -> &'s StagedConnection<'s> {
        self.conn
    }

    /// The options the database was opened with.
    pub fn options(&self) -> &ProbeOptions {
        &self.opts
    }

    /// Every record query, both cursor forms. Our own SQL, built from the
    /// static list; exposed for the statement-size tests.
    pub fn record_queries(&self) -> impl Iterator<Item = &str> {
        self.selects
            .iter()
            .flat_map(|s| [s.sql_start.as_str(), s.sql_after.as_str()])
    }

    /// Every `handle` row (one address and service).
    pub fn for_each_handle<B>(
        &self,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: impl FnMut(HandleRecord) -> ControlFlow<B>,
    ) -> Result<PassOutcome<B>, MessagesError> {
        self.pass(from, ctx, f)
    }

    /// Every `chat` row.
    pub fn for_each_chat<B>(
        &self,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: impl FnMut(ChatRecord) -> ControlFlow<B>,
    ) -> Result<PassOutcome<B>, MessagesError> {
        self.pass(from, ctx, f)
    }

    /// Every `chat_handle_join` row, keyed by its implicit `rowid`.
    pub fn for_each_chat_handle<B>(
        &self,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: impl FnMut(ChatHandleLink) -> ControlFlow<B>,
    ) -> Result<PassOutcome<B>, MessagesError> {
        self.pass(from, ctx, f)
    }

    /// Every `chat_message_join` row, keyed by its implicit `rowid`.
    pub fn for_each_chat_message<B>(
        &self,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: impl FnMut(ChatMessageLink) -> ControlFlow<B>,
    ) -> Result<PassOutcome<B>, MessagesError> {
        self.pass(from, ctx, f)
    }

    /// Every `message` row, with its body decoded, its association, reply
    /// and summary parsed, and its edit label derived (§9–§11). With
    /// `guids`, the row's message, reply-originator and association-target
    /// GUIDs are collected for the "not found" counts (§7.3).
    pub fn for_each_message<B>(
        &self,
        from: Cursor,
        opts: &MessageReadOptions,
        guids: Option<&mut GuidSets>,
        ctx: &mut MessagesCtx<'_>,
        f: impl FnMut(MessageRecord) -> ControlFlow<B>,
    ) -> Result<PassOutcome<B>, MessagesError> {
        let mut guids = guids;
        self.pass_with(
            PassSpec {
                table: KnownTable::Message,
                phase: MessagesPhase::ReadingMessages,
                stage: Stage::Messages,
            },
            from,
            ctx,
            |key, r| MessageRecord::from_row(key, r, opts, guids.as_deref_mut()),
            f,
        )
    }

    /// Every `message_attachment_join` row. An empty pass when the optional
    /// table is absent.
    pub fn for_each_message_attachment<B>(
        &self,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: impl FnMut(MessageAttachmentLink) -> ControlFlow<B>,
    ) -> Result<PassOutcome<B>, MessagesError> {
        self.pass(from, ctx, f)
    }

    /// Every `attachment` row. With a locator, each row's `filename` is
    /// classified, looked up in Manifest.db and its file probed (§12); the
    /// cancellation token is checked before each lookup. An empty pass when
    /// the optional table is absent.
    pub fn for_each_attachment<B>(
        &self,
        from: Cursor,
        locate: Option<&mut AttachmentLocator<'_>>,
        ctx: &mut MessagesCtx<'_>,
        f: impl FnMut(AttachmentRecord) -> ControlFlow<B>,
    ) -> Result<PassOutcome<B>, MessagesError> {
        let mut locate = locate;
        let cancel = ctx.cancel;
        self.pass_with(
            PassSpec {
                table: KnownTable::Attachment,
                phase: MessagesPhase::CheckingAttachments,
                stage: Stage::Attachments,
            },
            from,
            ctx,
            |key, r| {
                if locate.is_some() && cancel.is_cancelled() {
                    return Err(MessagesError::Cancelled);
                }
                AttachmentRecord::from_row(key, r, locate.as_deref_mut())
            },
            f,
        )
    }

    /// Every `chat_recoverable_message_join` row (Recently Deleted links).
    /// An empty pass when the optional table is absent.
    pub fn for_each_recoverable<B>(
        &self,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: impl FnMut(RecoverableLink) -> ControlFlow<B>,
    ) -> Result<PassOutcome<B>, MessagesError> {
        self.pass(from, ctx, f)
    }

    /// The aggregates (§7.3): integer SQL under op budgets, then the GUID
    /// counts from `guids` (filled by [`SmsDb::for_each_message`]). The sets
    /// are sorted in place, so they are taken mutably.
    pub fn aggregates(
        &self,
        guids: &mut GuidSets,
        ctx: &mut MessagesCtx<'_>,
    ) -> Result<crate::aggregates::Aggregates, MessagesError> {
        crate::aggregates::run(
            self.conn,
            &self.caps,
            crate::aggregates::AggBudget::default(),
            guids,
            ctx,
        )
    }

    fn prepare(&self, sql: &str, stage: Stage) -> Result<Statement<'s>, MessagesError> {
        self.conn
            .prepare_read(sql)
            .map_err(|e| MessagesError::from_conn(e, stage))
    }

    /// A pass over a [`FromRow`] record.
    fn pass<R: FromRow, B>(
        &self,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: impl FnMut(R) -> ControlFlow<B>,
    ) -> Result<PassOutcome<B>, MessagesError> {
        let spec = PassSpec {
            table: R::TABLE,
            phase: R::PHASE,
            stage: R::STAGE,
        };
        self.pass_with(spec, from, ctx, R::from_row, f)
    }

    /// The pass engine: keyset pages, one row at a time through `build` to
    /// `f`.
    fn pass_with<R, B>(
        &self,
        spec: PassSpec,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        mut build: impl FnMut(i64, &mut RowReader<'_, '_>) -> Result<R, MessagesError>,
        mut f: impl FnMut(R) -> ControlFlow<B>,
    ) -> Result<PassOutcome<B>, MessagesError> {
        let mut out = PassOutcome::empty();
        let Some(sel) = self.selects.iter().find(|s| s.table == spec.table) else {
            // An absent optional table; required tables always have a query.
            return Ok(out);
        };
        let total = self.caps.table(spec.table).and_then(|t| t.rows);
        let stage = spec.stage;
        let err = |e: rusqlite::Error| MessagesError::from_conn(self.conn.sqlite_error(&e), stage);
        ctx.check()?;
        ctx.report(spec.phase, 0, total);

        let limit = i64::from(self.opts.page_rows);
        let mut start: Option<Statement<'s>> = None;
        let mut after: Option<Statement<'s>> = None;
        let mut cursor = from;
        loop {
            let mut in_page: i64 = 0;
            {
                let query = match cursor {
                    Cursor::Start => {
                        if start.is_none() {
                            start = Some(self.prepare(&sel.sql_start, stage)?);
                        }
                        start
                            .as_mut()
                            .ok_or(MessagesError::Internal("statement"))?
                            .query([limit])
                    }
                    Cursor::After(key) => {
                        if after.is_none() {
                            after = Some(self.prepare(&sel.sql_after, stage)?);
                        }
                        after
                            .as_mut()
                            .ok_or(MessagesError::Internal("statement"))?
                            .query([key, limit])
                    }
                };
                let mut rows = query.map_err(err)?;
                while let Some(row) = rows.next().map_err(err)? {
                    // The key is read strictly; §6.2's shape rules make any
                    // other storage class impossible.
                    let key = match row.get_ref(0) {
                        Ok(ValueRef::Integer(k)) => k,
                        _ => return Err(MessagesError::Internal("row key is not an integer")),
                    };
                    let mut reader = RowReader {
                        row,
                        sel,
                        issues: &mut out.value_issues,
                    };
                    let record = build(key, &mut reader)?;
                    out.rows = out.rows.saturating_add(1);
                    out.last = Some(key);
                    in_page = in_page.saturating_add(1);
                    if out.rows.is_multiple_of(CHECK_EVERY_ROWS) {
                        ctx.check()?;
                        ctx.report(spec.phase, out.rows, total);
                    }
                    if let ControlFlow::Break(b) = f(record) {
                        out.broke = Some(b);
                        return Ok(out);
                    }
                }
            }
            ctx.check()?;
            if in_page < limit {
                break;
            }
            cursor = Cursor::After(
                out.last
                    .ok_or(MessagesError::Internal("page without a key"))?,
            );
        }
        ctx.report(spec.phase, out.rows, total);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_and_options_clamp() {
        let [length, sql, expr, column, vdbe, args, like, vars] = SMS_DB_LIMITS.values();
        assert_eq!(
            (length, sql, expr, column, vdbe, args, like, vars),
            (32 << 20, 16 << 10, 50, 2000, 25_000, 8, 50, 8)
        );
        for (v, ceiling) in SMS_DB_LIMITS
            .values()
            .into_iter()
            .zip(StagedDbLimits::ceiling().values())
        {
            assert!(v <= ceiling);
        }
        assert_eq!(ProbeOptions::default().with_page_rows(1).page_rows(), 16);
        assert_eq!(
            ProbeOptions::default().with_page_rows(1 << 20).page_rows(),
            4096
        );
        assert_eq!(ProbeOptions::default().page_rows(), 512);
        let loose = ProbeOptions::default().with_integrity_budget(u64::MAX, u64::MAX);
        assert_eq!(loose, ProbeOptions::default());
    }

    /// The handle query for a full table, as built.
    #[test]
    fn select_shape() {
        let mut tc_cols = crate::schema::KnownColumnSet::default();
        for c in KnownTable::Handle.known_columns() {
            tc_cols.insert(c.index());
        }
        let tc = TableCaps {
            table: KnownTable::Handle,
            present: true,
            rows: Some(0),
            present_columns: tc_cols,
            unknown_columns: 0,
            generated_columns: 0,
            wrong_case_columns: 0,
        };
        let s = Select::build(KnownTable::Handle, &tc).expect("handle has a query");
        assert!(s.sql_start.starts_with(
            "SELECT \"ROWID\", CASE WHEN octet_length(\"id\") <= 4096 THEN \"id\" END, octet_length(\"id\"), "
        ));
        assert!(s
            .sql_start
            .ends_with(" FROM \"handle\" ORDER BY \"ROWID\" LIMIT ?1"));
        assert!(s
            .sql_after
            .ends_with(" FROM \"handle\" WHERE \"ROWID\" > ?1 ORDER BY \"ROWID\" LIMIT ?2"));
        assert_eq!(s.at(0), Some(0));
        assert_eq!(s.at(1), Some(1));
        assert_eq!(s.at(2), Some(3));
    }
}
