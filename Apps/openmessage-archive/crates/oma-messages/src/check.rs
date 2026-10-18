//! `check()`: the extraction check's orchestration (M3 §2 step 5).
//!
//! ```text
//! a. stage sms.db (and its -wal, best effort); stamps, digest, one byte total
//! b. header facts, WAL facts
//! c. hardened open under SMS_DB_LIMITS and SMS_READ_POLICY
//!    - the schema loads only above the limits -> Unsupported(SchemaExceedsLimits)
//! d. scan sqlite_schema (no virtual table connected) -> VirtualTablePresent
//! e. quick_check under an op budget -> Damaged / Incomplete
//! f. probe -> Unsupported(reasons)
//! g. passes: handles, chats, chat links, messages, attachments; after each
//!    full pass, rows streamed == count(*), else Damaged(RowsNotStreamed)
//!    then the aggregates (integer SQL under budgets; GUID checks in Rust)
//! h. assert query_only == 1, authorizer denials == 0, total_changes == 0
//! i. drop the connection; remove the staged copy (a failure is recorded,
//!    not fatal; the backup's close() retries)
//! j. recheck that the backup's stamped files are unchanged
//! ```
//!
//! Nothing is persisted. The report holds aggregates only.

use std::ops::ControlFlow;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use oma_backup::{
    CancelToken, OpenBackup, StagedConnection, StagedDatabase, TextEncoding, WalOutcome, WalPolicy,
};

use crate::aggregates::{self, AggBudget, AggregateId, Aggregates};
use crate::attachments::{AttachmentLocator, LookupOutcome, NotAttemptedReason};
use crate::col::{Col, IssueKind};
use crate::db::{connect, stage, Connect, Cursor, ProbeOptions, SmsDb, PAGE_ROWS_DEFAULT};
use crate::error::{BackupStage, MessagesError};
use crate::guidset::{GuidSets, GUID_SET_CAP};
use crate::progress::MessagesCtx;
use crate::records::{
    AttachmentRecord, AttributedOutcome, ChatRecord, EditLabel, HandleRecord, MessageReadOptions,
    MessageRecord,
};
use crate::report::{
    add_opt, bump, bump_opt, zero_if, AssociationCounts, AttachmentCounts, AttributedState,
    ChatCounts, DateCounts, DbFacts, ExtractionReport, ExtractionStatus, FailureCounts,
    HandleCounts, IntegrityOutcome, ItemKindCounts, LinkCounts, MessageCounts, ProbeCounts,
    Qualifier, RawHistogram, RecentlyDeletedCounts, SchemaReport, ServiceCounts, SummaryCounts,
    TextState, UnitHistogram, WalReport, RANGE_FLOOR_UNIX_NS,
};
use crate::schema::known::{attachment as ac, chat as cc, chat_message_join, message as mc};
use crate::schema::{
    Capabilities, DamageReason, IncompleteReason, KnownColumn, KnownTable, Probe, UnsupportedReason,
};
use crate::typedstream::{AttrFlag, AttrKey, TsBudget};
use crate::value::ValueIssues;
use crate::{codes::AssociatedKind, schema::known::chat_recoverable_message_join};

/// Options for [`check`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CheckOptions {
    /// How a listed `-wal` is treated (default `BestEffort`, D23).
    pub wal: WalPolicy,
    /// Rows per page (default 512, clamped to 16..=4096).
    pub page_rows: u32,
    /// The typedstream budget for each body.
    pub ts_budget: TsBudget,
    /// Probe each attachment file that is found (default `true`).
    pub probe_attachments: bool,
    agg: AggBudget,
    probe: ProbeOptions,
}

impl Default for CheckOptions {
    fn default() -> Self {
        CheckOptions {
            wal: WalPolicy::BestEffort,
            page_rows: PAGE_ROWS_DEFAULT,
            ts_budget: TsBudget::DEFAULT,
            probe_attachments: true,
            agg: AggBudget::default(),
            probe: ProbeOptions::default(),
        }
    }
}

impl CheckOptions {
    /// The `-wal` policy.
    pub fn with_wal(mut self, wal: WalPolicy) -> Self {
        self.wal = wal;
        self
    }

    /// Rows per page, clamped.
    pub fn with_page_rows(mut self, n: u32) -> Self {
        self.probe = self.probe.with_page_rows(n);
        self.page_rows = self.probe.page_rows();
        self
    }

    /// The typedstream budget.
    pub fn with_ts_budget(mut self, b: TsBudget) -> Self {
        self.ts_budget = b;
        self
    }

    /// Whether to probe attachment files.
    pub fn with_probe_attachments(mut self, on: bool) -> Self {
        self.probe_attachments = on;
        self
    }

    /// The aggregate op budget, `per_row × rows + base`. It can only be
    /// tightened below the defaults.
    pub fn with_aggregate_budget(mut self, per_row: u64, base: u64) -> Self {
        let d = AggBudget::default();
        self.agg = AggBudget {
            per_row: per_row.min(d.per_row),
            base: base.min(d.base),
        };
        self
    }

    /// The `quick_check` op budget; it can only be tightened.
    pub fn with_integrity_budget(mut self, per_page: u64, base: u64) -> Self {
        self.probe = self.probe.with_integrity_budget(per_page, base);
        self
    }
}

/// What reading the staged copy came to.
enum Read {
    SchemaExceedsLimits,
    Unsupported(Vec<UnsupportedReason>, Box<Capabilities>),
    Damaged(DamageReason, Option<Box<Capabilities>>),
    Incomplete(IncompleteReason),
    Readable(Box<Parts>),
}

/// The groups of a readable report.
struct Parts {
    caps: Capabilities,
    handles: HandleCounts,
    chats: ChatCounts,
    messages: MessageCounts,
    dates: DateCounts,
    attachments: AttachmentCounts,
    links: LinkCounts,
    recently_deleted: RecentlyDeletedCounts,
    failures: FailureCounts,
    aggregates_incomplete: bool,
}

/// Run the extraction check on an opened backup (§2 step 5). Unsupported,
/// damaged and incomplete databases, decode failures, missing attachments,
/// a skipped `-wal`, oversized values and aggregates over budget are all
/// report values, not errors.
pub fn check(
    ob: &OpenBackup,
    opts: &CheckOptions,
    ctx: &mut MessagesCtx<'_>,
) -> Result<ExtractionReport, MessagesError> {
    let started = Instant::now();
    let checked_at = SystemTime::now();
    let checked_at_ns = checked_at
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(i64::MAX);
    ctx.check()?;

    // a, b
    let staged = stage(ob, opts.wal, ctx)?;
    // The `-wal` scan checks the token per frame. A cancelled scan leaves
    // `facts` as `None`; `connect` then checks the token first, so the job
    // ends `Cancelled` with the staged copy removed below.
    let mut db = db_facts(&staged, ctx.cancel);

    // c-h, with the staged copy removed whatever happened.
    let read = read_staged(&staged, ob, opts, checked_at_ns, &mut db, ctx);
    let removed = staged.remove();
    if let Err(e) = &removed {
        tracing::warn!(error = ?e, "staged sms.db not removed after the row pass");
    }
    db.plaintext_removed_after_row_pass = removed.is_ok();
    let read = read?;

    // j
    ctx.check()?;
    db.source_changed_during_check = !matches!(ob.unchanged_since_open(), Ok(true));

    let mut report = ExtractionReport {
        status: ExtractionStatus::Unsupported(Vec::new()),
        schema: SchemaReport::default(),
        database: db,
        handles: None,
        chats: None,
        messages: None,
        dates: None,
        attachments: None,
        links: None,
        recently_deleted: None,
        failures: None,
        checked_at,
        elapsed: started.elapsed(),
    };
    match read {
        Read::SchemaExceedsLimits => {
            report.status =
                ExtractionStatus::Unsupported(vec![UnsupportedReason::SchemaExceedsLimits]);
        }
        Read::Unsupported(reasons, caps) => {
            report.schema = SchemaReport::from_capabilities(&caps);
            report.status = ExtractionStatus::Unsupported(reasons);
        }
        Read::Damaged(reason, caps) => {
            if let Some(caps) = caps {
                report.schema = SchemaReport::from_capabilities(&caps);
            }
            report.status = ExtractionStatus::Damaged(reason);
        }
        Read::Incomplete(reason) => report.status = ExtractionStatus::Incomplete(reason),
        Read::Readable(p) => {
            let p = *p;
            report.schema = SchemaReport::from_capabilities(&p.caps);
            let qualifiers = qualifiers(&report.schema, &report.database, &p);
            report.status = ExtractionStatus::Readable { qualifiers };
            report.handles = Some(p.handles);
            report.chats = Some(p.chats);
            report.messages = Some(p.messages);
            report.dates = Some(p.dates);
            report.attachments = Some(p.attachments);
            report.links = Some(p.links);
            report.recently_deleted = Some(p.recently_deleted);
            report.failures = Some(p.failures);
        }
    }
    report.elapsed = started.elapsed();
    tracing::info!(
        status = report.status_kind(),
        qualifiers = report.qualifiers().len(),
        messages = report.messages.as_ref().map_or(0, |m| m.rows),
        elapsed_ms = u64::try_from(report.elapsed.as_millis()).unwrap_or(u64::MAX),
        plaintext_removed = report.database.plaintext_removed_after_row_pass,
        "extraction check finished"
    );
    Ok(report)
}

fn db_facts(staged: &StagedDatabase<'_>, cancel: &CancelToken) -> DbFacts {
    let header = staged.header_facts().ok();
    let facts = staged.wal_facts(cancel).ok().flatten();
    let bytes = match staged.wal_outcome {
        WalOutcome::Staged { bytes } => Some(bytes),
        _ => None,
    };
    DbFacts {
        open_mode: None,
        wal: WalReport {
            listed: !matches!(staged.wal_outcome, WalOutcome::NotListed),
            outcome: staged.wal_outcome,
            bytes,
            header_says_wal: header.map(|h| h.says_wal()),
            size_matches: staged.wal_size_matches,
            facts,
        },
        size_matches: staged.size_matches,
        integrity: None,
        page_size: None,
        staged_bytes: staged.main_bytes.saturating_add(bytes.unwrap_or(0)),
        plaintext_removed_after_row_pass: false,
        source_changed_during_check: false,
    }
}

fn read_staged(
    staged: &StagedDatabase<'_>,
    ob: &OpenBackup,
    opts: &CheckOptions,
    checked_at_ns: i64,
    db: &mut DbFacts,
    ctx: &mut MessagesCtx<'_>,
) -> Result<Read, MessagesError> {
    let conn = match connect(staged, ctx)? {
        Connect::Ready(c) => c,
        Connect::SchemaExceedsLimits => return Ok(Read::SchemaExceedsLimits),
    };
    db.open_mode = Some(conn.mode());
    db.page_size = conn.page_size().ok();
    // The public `page_rows` field is the source of truth (it may have been
    // set directly); `with_page_rows` clamps it again.
    let probe_opts = opts.probe.with_page_rows(opts.page_rows);
    let read = match SmsDb::open_with(&conn, &probe_opts, ctx)? {
        Probe::Supported(sms) => {
            db.integrity = Some(IntegrityOutcome::Ok);
            read_all(&sms, ob, opts, checked_at_ns, ctx)?
        }
        Probe::Unsupported(u) => {
            if u.reasons != [UnsupportedReason::VirtualTablePresent] {
                db.integrity = Some(IntegrityOutcome::Ok);
            }
            Read::Unsupported(u.reasons, Box::new(u.capabilities))
        }
        Probe::Damaged(r) => {
            db.integrity = Some(IntegrityOutcome::Problems);
            Read::Damaged(r, None)
        }
        Probe::Incomplete(r) => {
            db.integrity = Some(IntegrityOutcome::OverBudget);
            Read::Incomplete(r)
        }
    };
    assert_unwritten(&conn)?;
    drop(conn);
    Ok(read)
}

/// §7.5: three checks at the end. None is a proof; the authorizer is the
/// guard.
fn assert_unwritten(conn: &StagedConnection<'_>) -> Result<(), MessagesError> {
    let query_only = conn
        .query_only()
        .map_err(|e| MessagesError::from_backup(e, BackupStage::Opening))?;
    if !query_only || conn.authorizer_denials() != 0 || conn.total_changes() != 0 {
        return Err(MessagesError::Internal("connection was written"));
    }
    Ok(())
}

fn go<T>(_: T) -> ControlFlow<()> {
    ControlFlow::Continue(())
}

fn rows_of(caps: &Capabilities, t: KnownTable) -> Option<u64> {
    caps.table(t).and_then(|c| c.rows)
}

fn has(caps: &Capabilities, t: KnownTable, index: u8) -> bool {
    caps.has_column(KnownColumn::new(t, index))
}

/// A full pass that streamed a different number of rows than `count(*)`.
fn streamed(caps: &Capabilities, t: KnownTable, rows: u64) -> Option<DamageReason> {
    match rows_of(caps, t) {
        Some(n) if n != rows => Some(DamageReason::RowsNotStreamed(t)),
        _ => None,
    }
}

macro_rules! full_pass {
    ($caps:expr, $table:expr, $out:expr, $failures:expr) => {{
        let out = $out;
        $failures.values.merge(&out.value_issues);
        if let Some(d) = streamed($caps, $table, out.rows) {
            return Ok(Read::Damaged(d, Some(Box::new($caps.clone()))));
        }
        out.rows
    }};
}

#[allow(clippy::too_many_lines)]
fn read_all(
    db: &SmsDb<'_>,
    ob: &OpenBackup,
    opts: &CheckOptions,
    checked_at_ns: i64,
    ctx: &mut MessagesCtx<'_>,
) -> Result<Read, MessagesError> {
    let caps = db.capabilities().clone();
    let mut failures = FailureCounts::default();

    // Handles.
    let mut handles = HandleCounts {
        rows: 0,
        by_service: has(
            &caps,
            KnownTable::Handle,
            crate::schema::known::handle::SERVICE,
        )
        .then(ServiceCounts::default),
    };
    let out = db.for_each_handle(Cursor::Start, ctx, |h: HandleRecord| {
        if let Some(s) = handles.by_service.as_mut() {
            s.add(h.service_class);
        }
        ControlFlow::<()>::Continue(())
    })?;
    handles.rows = full_pass!(&caps, KnownTable::Handle, out, failures);

    // Chats.
    let ch = |i| has(&caps, KnownTable::Chat, i);
    let mut chats = ChatCounts {
        by_service: ch(cc::SERVICE_NAME).then(ServiceCounts::default),
        archived: zero_if(ch(cc::IS_ARCHIVED)),
        is_filtered: ch(cc::IS_FILTERED).then(RawHistogram::default),
        style: ch(cc::STYLE).then(RawHistogram::default),
        with_properties: zero_if(ch(cc::PROPERTIES)),
        ..ChatCounts::default()
    };
    let mut dates = DateCounts {
        chat_last_read: ch(cc::LAST_READ_MESSAGE_TIMESTAMP).then(UnitHistogram::default),
        chat_message_date: has(
            &caps,
            KnownTable::ChatMessageJoin,
            chat_message_join::MESSAGE_DATE,
        )
        .then(UnitHistogram::default),
        recoverable_delete_date: has(
            &caps,
            KnownTable::ChatRecoverableMessageJoin,
            chat_recoverable_message_join::DELETE_DATE,
        )
        .then(UnitHistogram::default),
        edit_entry_date: has(&caps, KnownTable::Message, mc::MESSAGE_SUMMARY_INFO)
            .then(UnitHistogram::default),
        excluded_scheduled: zero_if(has(&caps, KnownTable::Message, mc::SCHEDULE_TYPE)),
        ..DateCounts::default()
    };
    let out = db.for_each_chat(Cursor::Start, ctx, |c: ChatRecord| {
        if let Some(s) = chats.by_service.as_mut() {
            s.add(c.service_class);
        }
        if c.is_archived.value() == Some(&true) {
            bump_opt(&mut chats.archived);
        }
        if let Some(h) = chats.is_filtered.as_mut() {
            h.add(&c.is_filtered);
        }
        if let Some(h) = chats.style.as_mut() {
            h.add(&c.style);
        }
        if matches!(c.properties, Col::Value(_) | Col::Oversized { .. }) {
            bump_opt(&mut chats.with_properties);
        }
        if let Some(h) = dates.chat_last_read.as_mut() {
            h.add(&c.last_read_message_timestamp);
        }
        ControlFlow::<()>::Continue(())
    })?;
    chats.rows = full_pass!(&caps, KnownTable::Chat, out, failures);

    // Chat links.
    let mut links = LinkCounts::default();
    let out = db.for_each_chat_handle(Cursor::Start, ctx, go)?;
    links.chat_handle_rows = full_pass!(&caps, KnownTable::ChatHandleJoin, out, failures);
    let out = db.for_each_chat_message(Cursor::Start, ctx, |l| {
        if let Some(h) = dates.chat_message_date.as_mut() {
            h.add(&l.message_date);
        }
        ControlFlow::<()>::Continue(())
    })?;
    links.chat_message_rows = full_pass!(&caps, KnownTable::ChatMessageJoin, out, failures);

    let recoverable_present = caps
        .table(KnownTable::ChatRecoverableMessageJoin)
        .is_some_and(|t| t.present);
    let mut deleted_ids: Vec<i64> = Vec::new();
    let mut deleted_overflow = false;
    let out = db.for_each_recoverable(Cursor::Start, ctx, |l| {
        if let Some(h) = dates.recoverable_delete_date.as_mut() {
            h.add(&l.delete_date);
        }
        if let Col::Value(id) = l.message_id {
            if deleted_ids.len() < GUID_SET_CAP {
                deleted_ids.push(id);
            } else {
                deleted_overflow = true;
            }
        }
        ControlFlow::<()>::Continue(())
    })?;
    let recoverable_rows = full_pass!(&caps, KnownTable::ChatRecoverableMessageJoin, out, failures);
    links.recoverable_rows = recoverable_present.then_some(recoverable_rows);
    deleted_ids.sort_unstable();
    deleted_ids.dedup();
    let deleted_known = recoverable_present && !deleted_overflow;

    // Messages.
    let mut guids = GuidSets::new();
    let mut messages = MessageCounts::new(&caps, recoverable_present, deleted_known);
    let read_opts = MessageReadOptions::default().with_ts_budget(opts.ts_budget);
    let out = db.for_each_message(Cursor::Start, &read_opts, Some(&mut guids), ctx, |m| {
        let deleted = deleted_known.then(|| deleted_ids.binary_search(&m.rowid).is_ok());
        messages.add(&m, deleted, &mut failures, &mut dates, checked_at_ns);
        ControlFlow::<()>::Continue(())
    })?;
    let message_rows = full_pass!(&caps, KnownTable::Message, out, failures);
    messages.rows = message_rows;
    drop(deleted_ids);

    // Attachments.
    let maj_present = caps
        .table(KnownTable::MessageAttachmentJoin)
        .is_some_and(|t| t.present);
    let out = db.for_each_message_attachment(Cursor::Start, ctx, go)?;
    let maj_rows = full_pass!(&caps, KnownTable::MessageAttachmentJoin, out, failures);
    links.message_attachment_rows = maj_present.then_some(maj_rows);

    let att_present = caps
        .table(KnownTable::Attachment)
        .is_some_and(|t| t.present);
    let mut attachments = AttachmentCounts::new(att_present.then_some(0));
    if att_present {
        let a = |i| has(&caps, KnownTable::Attachment, i);
        attachments.filename_null = zero_if(a(ac::FILENAME));
        attachments.filename_too_long = zero_if(a(ac::FILENAME));
        attachments.declared_total_bytes = zero_if(a(ac::TOTAL_BYTES));
        attachments.stickers = zero_if(a(ac::IS_STICKER));
        attachments.hidden = zero_if(a(ac::HIDE_ATTACHMENT));
        attachments.transfer_state = a(ac::TRANSFER_STATE).then(RawHistogram::default);
        attachments.with_genmoji_description = zero_if(a(ac::EMOJI_IMAGE_SHORT_DESCRIPTION));
    }
    let mut locator = AttachmentLocator::new(ob, opts.probe_attachments);
    let out = db.for_each_attachment(Cursor::Start, Some(&mut locator), ctx, |a| {
        attachments.add(&a, &mut failures);
        ControlFlow::<()>::Continue(())
    })?;
    let att_rows = full_pass!(&caps, KnownTable::Attachment, out, failures);
    attachments.rows = att_present.then_some(att_rows);

    // Aggregates.
    let agg = aggregates::run(db.connection(), &caps, opts.agg, &mut guids, ctx)?;
    apply_aggregates(
        &agg,
        has(&caps, KnownTable::Message, mc::ASSOCIATED_MESSAGE_GUID),
        &mut chats,
        &mut messages,
        &mut attachments,
        &mut links,
    );
    failures.aggregate_over_budget = agg.over_budget;
    failures.orphan_join_row = [
        links.orphan_chat_message_chat,
        links.orphan_chat_message_message,
        links.orphan_chat_handle_chat,
        links.orphan_chat_handle_handle,
        links.orphan_message_attachment_message,
        links.orphan_message_attachment_attachment,
        links.orphan_recoverable_chat,
        links.orphan_recoverable_message,
    ]
    .iter()
    .flatten()
    .fold(0u64, |n, v| n.saturating_add(*v));
    for h in [
        Some(&dates.message_date),
        dates.chat_message_date.as_ref(),
        dates.chat_last_read.as_ref(),
        dates.recoverable_delete_date.as_ref(),
        dates.edit_entry_date.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        failures.date_ambiguous = failures.date_ambiguous.saturating_add(h.ambiguous);
        failures.date_negative = failures.date_negative.saturating_add(h.negative);
    }

    let recently_deleted = RecentlyDeletedCounts {
        chat_recoverable_message_join: rows_of(&caps, KnownTable::ChatRecoverableMessageJoin),
        recoverable_message_part: rows_of(&caps, KnownTable::RecoverableMessagePart),
        deleted_messages: rows_of(&caps, KnownTable::DeletedMessages),
        sync_deleted_messages: rows_of(&caps, KnownTable::SyncDeletedMessages),
        sync_deleted_chats: rows_of(&caps, KnownTable::SyncDeletedChats),
        sync_deleted_attachments: rows_of(&caps, KnownTable::SyncDeletedAttachments),
        unsynced_removed_recoverable_messages: rows_of(
            &caps,
            KnownTable::UnsyncedRemovedRecoverableMessages,
        ),
    };
    let aggregates_incomplete =
        agg.over_budget > 0 || !agg.guids_complete || (recoverable_present && deleted_overflow);
    Ok(Read::Readable(Box::new(Parts {
        caps,
        handles,
        chats,
        messages,
        dates,
        attachments,
        links,
        recently_deleted,
        failures,
        aggregates_incomplete,
    })))
}

fn apply_aggregates(
    agg: &Aggregates,
    association_targets_recorded: bool,
    chats: &mut ChatCounts,
    messages: &mut MessageCounts,
    attachments: &mut AttachmentCounts,
    links: &mut LinkCounts,
) {
    use AggregateId as A;
    chats.with_no_other_handle = agg.get(A::ChatsWithNoHandle);
    chats.with_one_other_handle = agg.get(A::ChatsWithOneHandle);
    chats.with_several_other_handles = agg.get(A::ChatsWithSeveralHandles);
    messages.handle_unmatched = agg.get(A::MessageHandleUnmatched);
    messages.other_handle_unmatched = agg.get(A::MessageOtherHandleUnmatched);
    if messages.replies.is_some() {
        messages.reply_originator_not_found = agg.guids.reply_originator_not_found;
    }
    // Targets come only from `associated_message_guid`; without that column
    // none could be collected, so the count is not recorded (I5), not 0.
    if let Some(a) = messages.associations.as_mut() {
        a.target_not_found = association_targets_recorded
            .then_some(agg.guids.association_target_not_found)
            .flatten();
    }
    attachments.linked = agg.get(A::AttachmentsLinked);
    attachments.not_linked = match (attachments.rows, attachments.linked) {
        (Some(rows), Some(linked)) => Some(rows.saturating_sub(linked)),
        _ => None,
    };
    links.orphan_chat_message_chat = agg.get(A::OrphanChatMessageChat);
    links.orphan_chat_message_message = agg.get(A::OrphanChatMessageMessage);
    links.orphan_chat_handle_chat = agg.get(A::OrphanChatHandleChat);
    links.orphan_chat_handle_handle = agg.get(A::OrphanChatHandleHandle);
    links.orphan_message_attachment_message = agg.get(A::OrphanMessageAttachmentMessage);
    links.orphan_message_attachment_attachment = agg.get(A::OrphanMessageAttachmentAttachment);
    links.orphan_recoverable_chat = agg.get(A::OrphanRecoverableChat);
    links.orphan_recoverable_message = agg.get(A::OrphanRecoverableMessage);
    links.messages_in_no_chat = agg.get(A::MessagesInNoChat);
    links.messages_in_no_chat_recently_deleted = agg.get(A::MessagesInNoChatRecentlyDeleted);
    links.messages_in_several_chats = agg.get(A::MessagesInSeveralChats);
}

fn qualifiers(schema: &SchemaReport, db: &DbFacts, p: &Parts) -> Vec<Qualifier> {
    let mut q = Vec::new();
    if schema.unknown_tables > 0 {
        q.push(Qualifier::UnknownTables(schema.unknown_tables));
    }
    let unknown = schema.unknown_columns();
    if unknown > 0 {
        q.push(Qualifier::UnknownColumns(unknown));
    }
    let wrong = schema.wrong_case_columns();
    if wrong > 0 {
        q.push(Qualifier::WrongCaseColumns(wrong));
    }
    if matches!(db.wal.outcome, WalOutcome::Skipped(_)) {
        q.push(Qualifier::WalSkipped);
    }
    if matches!(db.wal.outcome, WalOutcome::NotListed) && db.wal.header_says_wal == Some(true) {
        q.push(Qualifier::WalStateUnknown);
    }
    // A zero-length -wal is a normal state (SQLite truncates it), not an
    // invalid one: a header that does not validate counts only when there
    // are bytes after it to lose.
    let wal_bytes = db.wal.bytes.unwrap_or(0);
    if db
        .wal
        .facts
        .is_some_and(|f| f.frames_after_first_invalid > 0 || (!f.header_valid && wal_bytes > 0))
    {
        q.push(Qualifier::WalFramesInvalid);
    }
    if db.source_changed_during_check {
        q.push(Qualifier::SourceChanged);
    }
    if p.aggregates_incomplete {
        q.push(Qualifier::AggregatesIncomplete);
    }
    if withheld(&p.failures.values) {
        q.push(Qualifier::ValuesWithheld);
    }
    if schema.encoding.is_some_and(|e| e != TextEncoding::Utf8) {
        q.push(Qualifier::NonUtf8Encoding);
    }
    if db.size_matches == Some(false) || db.wal.size_matches == Some(false) {
        q.push(Qualifier::SizeMismatch);
    }
    q
}

/// Values that were not read: over their cap, or unreadable.
fn withheld(v: &ValueIssues) -> bool {
    v.value_oversized > 0
        || v.type_mismatch > 0
        || v.bool_out_of_range > 0
        || v.plist_invalid > 0
        || v.plist_over_budget > 0
        || v.plist_not_dictionary > 0
}

impl MessageCounts {
    fn new(caps: &Capabilities, recoverable_present: bool, deleted_known: bool) -> Self {
        let m = |i| has(caps, KnownTable::Message, i);
        let assoc = m(mc::ASSOCIATED_MESSAGE_TYPE) || m(mc::ASSOCIATED_MESSAGE_GUID);
        let partition_known = !recoverable_present || deleted_known;
        MessageCounts {
            with_association: zero_if(assoc),
            group_actions: zero_if(m(mc::ITEM_TYPE)),
            recently_deleted: zero_if(recoverable_present && deleted_known),
            other_rows: zero_if(partition_known),
            not_from_me_handle_zero: zero_if(m(mc::HANDLE_ID)),
            by_service: m(mc::SERVICE).then(ServiceCounts::default),
            item_kinds: m(mc::ITEM_TYPE).then(ItemKindCounts::default),
            associations: assoc.then(AssociationCounts::default),
            replies: zero_if(m(mc::THREAD_ORIGINATOR_GUID)),
            reply_to_guid_set: zero_if(m(mc::REPLY_TO_GUID)),
            ec_single_entry_parts: zero_if(m(mc::MESSAGE_SUMMARY_INFO)),
            summaries: m(mc::MESSAGE_SUMMARY_INFO).then(SummaryCounts::default),
            scheduled: zero_if(m(mc::SCHEDULE_TYPE)),
            filter_action: m(mc::FILTER_ACTION).then(RawHistogram::default),
            payload_data_present: zero_if(m(mc::PAYLOAD_DATA)),
            cache_has_attachments_set: zero_if(m(mc::CACHE_HAS_ATTACHMENTS)),
            date_retracted_set: zero_if(m(mc::DATE_RETRACTED)),
            ..MessageCounts::default()
        }
    }

    #[allow(clippy::too_many_lines)]
    fn add(
        &mut self,
        m: &MessageRecord,
        deleted: Option<bool>,
        failures: &mut FailureCounts,
        dates: &mut DateCounts,
        checked_at_ns: i64,
    ) {
        // The partition: association, non-message item (`item_type ≠ 0`,
        // the `group_actions` field), Recently Deleted, other.
        if m.association.is_some() && self.with_association.is_some() {
            bump_opt(&mut self.with_association);
        } else if self.group_actions.is_some() && m.item_type.value().is_some_and(|t| *t != 0) {
            bump_opt(&mut self.group_actions);
        } else if deleted == Some(true) {
            bump_opt(&mut self.recently_deleted);
        } else {
            bump_opt(&mut self.other_rows);
        }

        match &m.is_from_me {
            Col::Value(true) => bump(&mut self.from_me),
            Col::Value(false) => {
                bump(&mut self.not_from_me);
                if m.handle_id.value() == Some(&0) {
                    bump_opt(&mut self.not_from_me_handle_zero);
                }
            }
            Col::Null => bump(&mut self.direction_null),
            _ => bump(&mut self.direction_unreadable),
        }
        if let Some(s) = self.by_service.as_mut() {
            s.add(m.service_class);
        }
        if let Some(k) = self.item_kinds.as_mut() {
            k.add(&m.item_kind);
        }
        if let (Some(c), Some(a)) = (self.associations.as_mut(), m.association.as_ref()) {
            match a.kind {
                Some(AssociatedKind::NoAssociation) => bump(&mut c.no_association_type),
                Some(AssociatedKind::StickerLegacy) => bump(&mut c.sticker_legacy),
                Some(AssociatedKind::Tapback { reaction, removal }) => {
                    if removal {
                        c.tapback_removed.add(reaction);
                    } else {
                        c.tapback_added.add(reaction);
                    }
                }
                Some(AssociatedKind::PollVote) => bump(&mut c.poll_vote),
                Some(_) => bump(&mut c.unknown_type),
                None => bump(&mut c.type_not_recorded),
            }
            let t = &mut c.targets;
            match &a.target {
                Col::Value(crate::assoc::AssocTarget::Part { .. }) => bump(&mut t.part),
                Col::Value(crate::assoc::AssocTarget::Bubble { .. }) => bump(&mut t.bubble),
                Col::Value(crate::assoc::AssocTarget::Bare { .. }) => bump(&mut t.bare),
                Col::Value(_) => {
                    bump(&mut t.unparsed);
                    bump(&mut failures.assoc_guid_unparsed);
                }
                Col::Null => bump(&mut t.null),
                Col::Oversized { .. } => bump(&mut t.oversized),
                Col::Unreadable(_) => bump(&mut t.unreadable),
                Col::Absent => {}
            }
        }
        if m.reply.is_reply() {
            bump_opt(&mut self.replies);
        }
        if m.reply
            .reply_to_guid
            .value()
            .is_some_and(|g| !g.expose().is_empty())
        {
            bump_opt(&mut self.reply_to_guid_set);
        }

        let l = &mut self.edit_labels;
        bump(match m.edit_label {
            EditLabel::NotRecorded => &mut l.not_recorded,
            EditLabel::SummaryUnreadable => &mut l.summary_unreadable,
            EditLabel::UnsentPartsAndEditHistory { .. } => &mut l.unsent_parts_and_edit_history,
            EditLabel::UnsentParts { .. } => &mut l.unsent_parts,
            EditLabel::EditHistory { .. } => &mut l.edit_history,
            EditLabel::DateEditedNoBody => &mut l.date_edited_no_body,
            EditLabel::DateEditedOnly => &mut l.date_edited_only,
            EditLabel::NoEditRecorded => &mut l.no_edit_recorded,
            _ => &mut l.undetermined,
        });

        if let Some(s) = self.summaries.as_mut() {
            match &m.summary {
                Col::Value(info) => {
                    bump(&mut s.parsed);
                    if info.has_tmp {
                        bump(&mut s.tmp_key_present);
                    }
                    if info.ec.is_bad() || info.rp.is_bad() || info.otr.is_bad() {
                        bump(&mut failures.summary_shape);
                    }
                    failures.summary_unknown_keys = failures
                        .summary_unknown_keys
                        .saturating_add(u64::from(info.unknown_keys));
                    if let Some(parts) = info.ec.value() {
                        for p in parts {
                            if p.entries.len() == 1 {
                                bump_opt(&mut self.ec_single_entry_parts);
                            }
                            if let Some(h) = dates.edit_entry_date.as_mut() {
                                for e in &p.entries {
                                    match e.at {
                                        Some(t) => h.add_interp(t.interp),
                                        None => bump(&mut h.null),
                                    }
                                }
                            }
                        }
                    }
                }
                Col::Null => bump(&mut s.null),
                Col::Oversized { .. } => {
                    bump(&mut s.oversized);
                    bump(&mut failures.summary_too_large);
                }
                Col::Unreadable(k) => {
                    bump(&mut s.unreadable);
                    match k {
                        IssueKind::PlistInvalid => bump(&mut failures.summary_plist_invalid),
                        IssueKind::PlistOverBudget => bump(&mut failures.summary_plist_budget),
                        IssueKind::PlistNotDictionary => {
                            bump(&mut failures.summary_not_dictionary);
                        }
                        _ => {}
                    }
                }
                Col::Absent => {}
            }
        }
        if m.schedule_type.value().is_some_and(|v| *v != 0) {
            bump_opt(&mut self.scheduled);
        }
        if let Some(h) = self.filter_action.as_mut() {
            h.add(&m.filter_action);
        }
        if matches!(m.payload_data_type, Col::Value(_)) {
            bump_opt(&mut self.payload_data_present);
        }
        if m.cache_has_attachments.value() == Some(&true) {
            bump_opt(&mut self.cache_has_attachments_set);
        }
        if m.date_retracted.value().and_then(|t| t.unix_ns()).is_some() {
            bump_opt(&mut self.date_retracted_set);
        }

        self.add_body(m, failures);

        // Dates (§8.3).
        dates.message_date.add(&m.date);
        if m.schedule_type.value().is_some_and(|v| *v != 0) {
            bump_opt(&mut dates.excluded_scheduled);
        } else {
            match m.date.value().and_then(|t| t.unix_ns()) {
                None => bump(&mut dates.excluded_not_classified),
                Some(ns) if ns < RANGE_FLOOR_UNIX_NS => bump(&mut dates.excluded_before_floor),
                Some(ns) if ns > checked_at_ns => bump(&mut dates.excluded_after_checked_at),
                Some(ns) => {
                    dates.earliest_message = Some(dates.earliest_message.map_or(ns, |e| e.min(ns)));
                    dates.latest_message = Some(dates.latest_message.map_or(ns, |e| e.max(ns)));
                }
            }
        }
    }

    fn add_body(&mut self, m: &MessageRecord, failures: &mut FailureCounts) {
        let b = &mut self.bodies;
        let text = match &m.body.text_column {
            Col::Absent => TextState::Absent,
            Col::Null => TextState::Null,
            Col::Value(t) if t.expose().is_empty() => TextState::Empty,
            Col::Value(_) => TextState::NonEmpty,
            Col::Unreadable(_) | Col::Oversized { .. } => TextState::Withheld,
        };
        let attributed = match &m.body.attributed {
            AttributedOutcome::Absent => AttributedState::Absent,
            AttributedOutcome::Null => AttributedState::Null,
            AttributedOutcome::Oversized { .. } | AttributedOutcome::Unreadable(_) => {
                AttributedState::Withheld
            }
            _ => AttributedState::Present,
        };
        b.add_cross(text, attributed);
        match &m.body.attributed {
            AttributedOutcome::Decoded(body) => {
                if let Some(issue) = body.run_issue {
                    failures.typedstream(issue.kind);
                }
                if body.run_issue.is_some() || body.unreadable_attribute_values > 0 {
                    bump(&mut b.decoded_degraded);
                } else {
                    bump(&mut b.decoded_ok);
                }
                failures.unknown_attribute_key = failures
                    .unknown_attribute_key
                    .saturating_add(u64::from(body.unknown_attribute_keys));
                failures.unknown_attribute_class = failures
                    .unknown_attribute_class
                    .saturating_add(u64::from(body.unknown_attribute_classes));
                failures.unreadable_attribute_value = failures
                    .unreadable_attribute_value
                    .saturating_add(u64::from(body.unreadable_attribute_values));
                if let Col::Value(t) = &m.body.text_column {
                    if t.expose().as_bytes() == body.text.expose().as_bytes() {
                        bump(&mut b.text_equals_decoded);
                    } else {
                        bump(&mut b.text_differs_from_decoded);
                    }
                }
                if body.parts.len() > 1 {
                    bump(&mut b.more_than_one_part);
                }
                if body.parts.iter().any(|p| p.mention.is_some()) {
                    bump(&mut b.with_mention);
                }
                if body
                    .parts
                    .iter()
                    .any(|p| p.keys_present.contains(AttrKey::Link))
                {
                    bump(&mut b.with_link_key);
                }
                if body
                    .parts
                    .iter()
                    .any(|p| p.flags.contains(AttrFlag::AudioTranscription))
                {
                    bump(&mut b.with_audio_transcription);
                }
            }
            AttributedOutcome::Failed(kind) => {
                bump(&mut b.decode_failed);
                failures.typedstream(*kind);
            }
            AttributedOutcome::NotDecoded => bump(&mut b.not_decoded),
            _ => {}
        }
    }
}

impl AttachmentCounts {
    fn add(&mut self, a: &AttachmentRecord, failures: &mut FailureCounts) {
        match &a.filename {
            Col::Null => bump_opt(&mut self.filename_null),
            Col::Oversized { .. } => bump_opt(&mut self.filename_too_long),
            _ => {}
        }
        if let Some(loc) = &a.location {
            if let Some(c) = loc.class {
                if let Some(e) = self.by_class.iter_mut().find(|e| e.0 == c) {
                    bump(&mut e.1);
                }
            }
            let l = &mut self.lookup;
            match &loc.lookup {
                LookupOutcome::NotAttempted(r) => {
                    if let Some(e) = l.not_attempted.iter_mut().find(|e| e.0 == *r) {
                        bump(&mut e.1);
                    }
                    if *r == NotAttemptedReason::UnusualName {
                        bump(&mut failures.attachment_unusual_name);
                    }
                }
                LookupOutcome::Found { blob, .. } => {
                    bump(&mut l.found);
                    self.probes
                        .add(*blob, &mut self.present_blob_bytes, failures);
                }
                LookupOutcome::NotListedAtSourcedKey => bump(&mut l.not_listed_at_sourced_key),
                LookupOutcome::NotFoundAtAssumedKey { .. } => {
                    bump(&mut l.not_found_at_assumed_key);
                }
                LookupOutcome::Ambiguous => {
                    bump(&mut l.ambiguous);
                    bump(&mut failures.attachment_ambiguous);
                }
                LookupOutcome::InvalidEntry => {
                    bump(&mut l.invalid_entry);
                    bump(&mut failures.attachment_invalid_entry);
                }
            }
        }
        if let Col::Value(n) = a.total_bytes {
            if let Ok(n) = u64::try_from(n) {
                add_opt(&mut self.declared_total_bytes, n);
            }
        }
        if a.is_sticker.value() == Some(&true) {
            bump_opt(&mut self.stickers);
        }
        if a.hide_attachment.value() == Some(&true) {
            bump_opt(&mut self.hidden);
        }
        if let Some(h) = self.transfer_state.as_mut() {
            h.add(&a.transfer_state);
        }
        if matches!(a.emoji_image_short_description, Col::Value(_)) {
            bump_opt(&mut self.with_genmoji_description);
        }
    }
}

impl ProbeCounts {
    fn add(
        &mut self,
        blob: Option<oma_backup::BlobProbe>,
        present_bytes: &mut u64,
        failures: &mut FailureCounts,
    ) {
        use oma_backup::BlobProbe as P;
        match blob {
            None => bump(&mut self.not_probed),
            Some(P::Present { len }) => {
                bump(&mut self.present);
                *present_bytes = present_bytes.saturating_add(len);
            }
            Some(P::Missing) => {
                bump(&mut self.missing);
                bump(&mut failures.attachment_blob_missing);
            }
            Some(P::NotRegular) => {
                bump(&mut self.not_regular);
                bump(&mut failures.attachment_blob_unreadable);
            }
            Some(P::Unreadable(_)) => {
                bump(&mut self.unreadable);
                bump(&mut failures.attachment_blob_unreadable);
            }
            Some(P::NotAFile) => {
                bump(&mut self.not_a_file);
                bump(&mut failures.attachment_blob_unreadable);
            }
            Some(P::InvalidEntry) => {
                bump(&mut self.invalid_entry);
                bump(&mut failures.attachment_blob_unreadable);
            }
            Some(P::NoKey) => {
                bump(&mut self.no_key);
                bump(&mut failures.attachment_key_problem);
            }
            Some(P::NoProtectionClass) => {
                bump(&mut self.no_protection_class);
                bump(&mut failures.attachment_key_problem);
            }
            Some(P::ClassKeyUnavailable) => {
                bump(&mut self.class_key_unavailable);
                bump(&mut failures.attachment_key_problem);
            }
            Some(P::KeyDoesNotUnwrap) => {
                bump(&mut self.key_does_not_unwrap);
                bump(&mut failures.attachment_key_problem);
            }
            Some(P::SizeMismatch { .. }) => {
                bump(&mut self.size_mismatch);
                bump(&mut failures.attachment_blob_size_mismatch);
            }
            Some(_) => bump(&mut self.other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_only_tighten() {
        let d = CheckOptions::default();
        assert_eq!(d.wal, WalPolicy::BestEffort);
        assert_eq!(d.page_rows, 512);
        assert!(d.probe_attachments);
        let loose = d.clone().with_aggregate_budget(u64::MAX, u64::MAX);
        assert_eq!(loose, d);
        let tight = d.clone().with_aggregate_budget(1, 2);
        assert_eq!(
            tight.agg,
            AggBudget {
                per_row: 1,
                base: 2
            }
        );
        assert_eq!(d.clone().with_page_rows(1).page_rows, 16);
        assert_eq!(d.with_page_rows(1 << 20).page_rows, 4096);
    }
}
