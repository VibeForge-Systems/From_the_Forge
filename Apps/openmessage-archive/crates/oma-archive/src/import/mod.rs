//! The import (spec §8; slices 4f to 4j).
//!
//! [`Archive::import`] runs the whole import, in the order of spec §2:
//!
//! 1. stage sms.db (and its `-wal`, best effort) and probe it;
//! 2. compute the session's facts: the content digest and its framing, the
//!    read profile, the rank (§8.7), the qualifiers;
//! 3. P0, the Recently Deleted scan (§8.2 step 2), then the duplicate-key
//!    scan with the Recently Deleted classification and the key tallies
//!    (step 3), then the scan for attachments linked only to excluded
//!    messages (step 4, which refuses a source over its cap), then the
//!    attachment and space estimates (step 5 and §12; an available space
//!    below the reserve plus the WAL allowance refuses the import before
//!    anything is written);
//! 4. **the gate** (step 7, `gate.rs`): the [`Preflight`], with the resume
//!    plan (step 1), the lineages and their device evidence, the key and
//!    Recently Deleted facts, the space estimate and the order facts, is
//!    given to the [`ImportGate`]. Decline returns
//!    [`ImportOutcome::Declined`] with nothing written;
//! 5. the archive's first write ([`Archive::begin_write`] with recovery),
//!    then the session bookkeeping transaction (§8.3), continuing the
//!    unfinished session only if the chosen lineage is its own;
//! 6. the passes, in batches (§8.5), each committing its cursor, counts and
//!    issues with its rows: **P1 handles, P2 chats, P3 messages** (with
//!    their body versions, the FTS index through its triggers, and the
//!    Recently Deleted policy), **P4 chat ↔ handle, P5 chat ↔ message, P5b**
//!    (the Recently Deleted attachment exclusion, re-derived from durable
//!    state; it writes nothing), **P6 attachments** (their rows, and their
//!    files copied into the content-addressed store; `attachments.rs`),
//!    **P7 message ↔ attachment and P8 the Recently Deleted markers**;
//! 7. the source check after P8: a backup that changed since it was opened
//!    stops the session in a resumable state (§9.3), as a file that
//!    changed during its copy does; then the check that the staged copy
//!    was only read;
//! 8. the staged sms.db is removed (§8.11);
//! 9. **P9, finalize** (`finalize.rs`): the statistics, the session-local
//!    cleanup, the terminal state with `full_observation`, the FTS merge
//!    steps and the checkpoint;
//! 10. **the provenance file** `imports/session-<n>.json` (best effort) at
//!     every terminal state this import records: completed, cancelled,
//!     stopped or failed.
//!
//! Before every write transaction the space guard (§12) checks the free
//! space and bounds `max_page_count`; a shortfall, or `SQLITE_FULL`, stops
//! the session in a resumable state (6).
//!
//! The shell runs it in its import job (`src-tauri/src/archives/worker.rs`).

mod attachments;
mod batch;
mod bind;
mod chats;
mod counts;
mod entity;
mod finalize;
mod gate;
mod handles;
mod keys;
mod links;
mod map;
mod merge;
mod messages;
mod preflight;
mod rd;
mod recently_deleted;
mod session;

use oma_backup::{OpenBackup, WalPolicy};
use oma_messages::{
    connect, stage, Connect, MessagesCtx, MessagesError, MessagesPhase, MessagesProgress, Probe,
    ProbeOptions, ServiceClass, SmsDb,
};

pub use batch::BatchBudget;
pub use counts::{
    AttachmentFileCounts, BodyCounts, EntityCounts, ImportCounts, LinkCounts, RecentlyDeletedCounts,
};
#[cfg(feature = "fault-injection")]
pub(crate) use finalize::bulk_finalize;
pub use finalize::FinalizeReport;
pub use gate::{
    DeviceEvidence, FreshReason, GateDecision, ImportGate, KeyCountFacts, KeyFacts, LineageFacts,
    OrderFacts, OrderSource, Preflight, ProceedWith, ProceedWithSuggestion, RdFacts, ResumePlan,
    LINEAGES_LIMIT,
};
#[cfg(feature = "fault-injection")]
pub use preflight::{KEY_SCAN_CAP, RECOVERABLE_SCAN_CAP};
pub(crate) use session::{supersede_sessions, unfinished_sessions};
pub use session::{ORDER_KEY_FLOOR_NS, ORDER_KEY_SLACK_NS};

use crate::archive::{Archive, Recovery};
use crate::conn::{JOURNAL_SIZE_LIMIT, READ_OP_BUDGET};
use crate::error::{primary_code, ArchiveError, DamageKind, Stage};
use crate::layout::ArchiveDir;
use crate::progress::{ArchiveCtx, ArchiveEvent, ImportPhase};
use crate::provenance;
use crate::schema::codes::{Entity, IssueCode, Pass, ServiceClassCode, SessionState};
use crate::space::{reserve_for, wal_allowance, SpaceEstimate, SpaceGuard};
use batch::{run_pass, PassEnd, PassError, Run};
use bind::Cell;
use finalize::NotCompleted;
use preflight::{
    decide_resume, load_dup_keys, scan_keys, scan_recoverable, KeyScan, RecoverableScan, ScanError,
};

/// Options for an import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImportOptions {
    /// Import messages only in Recently Deleted (D60). Default `false`.
    pub include_recently_deleted: bool,
    /// Copy attachment files. Default `true`.
    pub copy_attachments: bool,
    /// How a listed sms.db `-wal` that cannot be provided is treated.
    /// Default [`WalPolicy::BestEffort`] (D23).
    pub wal: WalPolicy,
    /// The iOS version is not one the app was tested with (computed by the
    /// shell; D79). Recorded as a qualifier.
    pub ios_version_untested: bool,
    /// The batch budget (§8.5).
    pub batch: BatchBudget,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            include_recently_deleted: false,
            copy_attachments: true,
            wal: WalPolicy::BestEffort,
            ios_version_untested: false,
            batch: BatchBudget::default(),
        }
    }
}

/// A lineage: "newer backups of the same phone" (D80).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LineageId(pub(crate) i64);

impl LineageId {
    /// A lineage id read from [`ImportSummary`] or the archive's lineage
    /// list.
    pub fn new(id: i64) -> LineageId {
        LineageId(id)
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

/// The lineage the user chose at the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageChoice {
    /// The same phone as this lineage.
    Existing(LineageId),
    /// A different source: a new lineage.
    New,
}

/// Which keyed table went over the key-scan cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyScanTable {
    Handles,
    Chats,
    Messages,
    Attachments,
}

/// Why a backup cannot be imported. Nothing was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NotImportable {
    /// sms.db's schema loads only above the reader's limits.
    SchemaExceedsLimits,
    /// The probe found an unsupported database.
    Unsupported,
    /// The probe found a damaged database.
    Damaged,
    /// The probe could not finish.
    Incomplete,
    /// A keyed table has more than [`KEY_SCAN_CAP`] rows (§23).
    OverKeyScanCap(KeyScanTable),
    /// Recently Deleted lists more than [`RECOVERABLE_SCAN_CAP`] rows, or
    /// its excluded messages link more than that many distinct attachment
    /// rowids, so the default exclusion could not be applied (§23).
    OverRecentlyDeletedCap,
}

/// Why an import stopped in a resumable state (state 6, D88).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StopReason {
    /// Not enough space: available space fell below the reserve plus the
    /// WAL allowance before a write or a file copy, `max_page_count` could
    /// not be set as requested, or SQLite or the file system reported the
    /// disk full (`SQLITE_FULL`, `ENOSPC`).
    InsufficientSpace,
    /// The backup changed while it was read: a file changed during its copy,
    /// or the backup's stamps differ after P8 (§9.3).
    SourceChanged,
}

impl StopReason {
    fn issue(self) -> IssueCode {
        match self {
            StopReason::InsufficientSpace => IssueCode::InsufficientSpace,
            StopReason::SourceChanged => IssueCode::SourceChanged,
        }
    }
}

/// What an import did. Content-free.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImportSummary {
    /// The `import_session` id.
    pub session: i64,
    /// The session's lineage.
    pub lineage: LineageId,
    /// An unfinished session was continued.
    pub resumed: bool,
    /// Unfinished sessions superseded by this one.
    pub superseded: u32,
    /// The counts, including earlier segments of a continued session.
    pub counts: ImportCounts,
    /// The preflight's space estimate for this attempt (spec §12).
    pub estimate: SpaceEstimate,
    /// The staged (plaintext) sms.db was removed (spec §8.11). When not,
    /// closing the backup removes it, and the M2 sweep covers a crash.
    pub staging_removed: bool,
    /// `imports/session-<n>.json` was written for this terminal state
    /// (spec §8.10; best effort, so `false` never changes the outcome).
    pub provenance_written: bool,
    /// What finalize did after the session completed; `None` for any other
    /// outcome.
    pub finalize: Option<FinalizeReport>,
}

/// The outcome of [`Archive::import`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImportOutcome {
    /// Every pass ran and finalize recorded the session completed (state
    /// 1), with `full_observation` set (spec §8.7).
    Completed(ImportSummary),
    /// Cancelled. With a session, its completed records are committed and
    /// it is resumable (state 2); `None` when the cancel came before the
    /// session started (at the gate included), and nothing was written.
    Cancelled(Option<ImportSummary>),
    /// Stopped in a resumable state (state 6, D88). When finalize's own
    /// terminal bookkeeping failed for lack of space, the session is left
    /// running instead; the next write open marks it interrupted (4),
    /// which is just as resumable (spec §8.9 step 4).
    Stopped {
        reason: StopReason,
        summary: ImportSummary,
    },
    /// The gate declined; nothing was written (spec §8.2 step 7).
    Declined,
    /// The backup cannot be imported; nothing was written.
    NotImportable(NotImportable),
    /// Available space is below the reserve plus the WAL allowance (spec
    /// §12): refused before the gate, with nothing written. An estimate
    /// above the available space is only a warning
    /// ([`SpaceEstimate::warns`]), shown at the gate.
    InsufficientSpace(SpaceEstimate),
}

/// The SQL that asks whether a message key is already a keyed message
/// (the preflight's overlap lookup; spec §8.2 step 3).
pub(crate) const MESSAGE_KEY_ARCHIVED_SQL: &str =
    "SELECT 1 FROM message WHERE key_kind = 1 AND guid = ?1";

/// How [`Archive::import_staged`] ended.
#[allow(clippy::large_enum_variant)] // made once per import
enum Staged {
    /// A final outcome.
    Done(ImportOutcome),
    /// Every pass is done; finalize is next, after the staged copy is
    /// removed.
    Finalize(Box<Finish>),
}

/// What finalize needs from the passes.
struct Finish {
    run: Run,
    counts: ImportCounts,
    started: session::Started,
    estimate: SpaceEstimate,
    qualifiers: i64,
}

impl Finish {
    fn summary(&self, counts: &ImportCounts) -> ImportSummary {
        summary_of(&self.started, counts, self.estimate)
    }
}

fn summary_of(
    started: &session::Started,
    counts: &ImportCounts,
    estimate: SpaceEstimate,
) -> ImportSummary {
    ImportSummary {
        session: started.session,
        lineage: LineageId(started.lineage),
        resumed: started.resumed,
        superseded: started.superseded,
        counts: counts.clone(),
        estimate,
        staging_removed: false,
        provenance_written: false,
        finalize: None,
    }
}

/// Mark the summary of an outcome with the staging removal.
fn with_staging(outcome: ImportOutcome, removed: bool) -> ImportOutcome {
    let mark = |mut s: ImportSummary| {
        s.staging_removed = removed;
        s
    };
    match outcome {
        ImportOutcome::Completed(s) => ImportOutcome::Completed(mark(s)),
        ImportOutcome::Cancelled(Some(s)) => ImportOutcome::Cancelled(Some(mark(s))),
        ImportOutcome::Stopped { reason, summary } => ImportOutcome::Stopped {
            reason,
            summary: mark(summary),
        },
        other => other,
    }
}

/// The import's index-dependent statements, by name, exactly as the code
/// prepares them, for the query-plan pins (spec §18.2, C3.22).
#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    use merge::{KEY_GUID, KEY_NATURAL, KEY_UNKEYED, STORED_ISSUES_SQL};
    let (h, c, m, a) = (
        handles::table_def(),
        chats::table_def(),
        messages::table_def(),
        attachments::table_def(),
    );
    vec![
        ("handle by natural key", h.select_sql(KEY_NATURAL)),
        ("handle unkeyed", h.select_sql(KEY_UNKEYED)),
        ("chat by guid", c.select_sql(KEY_GUID)),
        ("chat unkeyed", c.select_sql(KEY_UNKEYED)),
        ("message by guid", m.select_sql(KEY_GUID)),
        ("message unkeyed", m.select_sql(KEY_UNKEYED)),
        ("attachment by guid", a.select_sql(KEY_GUID)),
        ("attachment unkeyed", a.select_sql(KEY_UNKEYED)),
        ("value issues of a row", STORED_ISSUES_SQL.to_owned()),
        (
            "body version by raw hash",
            messages::BODY_FIND_SQL.to_owned(),
        ),
        ("overlap lookup", MESSAGE_KEY_ARCHIVED_SQL.to_owned()),
        (
            "current blob by guid",
            attachments::CURRENT_BLOB_SQL.to_owned(),
        ),
        ("map resolution", links::RESOLVE_SQL.to_owned()),
        ("occurrence counter", map::OCCURRENCE_SQL.to_owned()),
        ("finalize chat_stats", finalize::CHAT_STATS_SQL.to_owned()),
    ]
}

/// The archive code of a readable service text.
pub(crate) fn service_class(cell: Option<&Cell>) -> Option<i64> {
    let b = cell?.value.as_ref()?.as_bytes()?;
    let class = match ServiceClass::classify(b) {
        ServiceClass::IMessage => ServiceClassCode::IMessage,
        ServiceClass::Sms => ServiceClassCode::Sms,
        ServiceClass::Rcs => ServiceClassCode::Rcs,
        ServiceClass::IMessageLite => ServiceClassCode::IMessageLite,
        ServiceClass::Empty => ServiceClassCode::Empty,
        _ => ServiceClassCode::Other,
    };
    Some(class.code())
}

fn messages_err(e: MessagesError) -> ArchiveError {
    ArchiveError::Messages(e)
}

impl Archive {
    /// Import a validated, opened backup into this archive (module
    /// documentation). `gate` is asked once, after the preflight reads and
    /// before anything is written; the lineage it chooses decides whether
    /// an unfinished session continues (D80).
    ///
    /// The staged sms.db is removed after the last pass, before finalize
    /// (the backup's `close` is the backstop).
    pub fn import(
        &mut self,
        ob: &OpenBackup,
        opts: &ImportOptions,
        gate: &mut dyn ImportGate,
        ctx: &mut ArchiveCtx<'_>,
    ) -> Result<ImportOutcome, ArchiveError> {
        if self.needs_migration() {
            return Err(ArchiveError::NeedsMigration {
                found: self.schema_version(),
            });
        }
        if self.import_blocked().is_some() {
            return Err(ArchiveError::NewerRules);
        }
        let cancel = ctx.cancel;
        // Staging reports its bytes as the import's staging phase (§8.12;
        // `stage` reports its start, with `done: 0`).
        let staged = {
            let progress = &mut *ctx.progress;
            let mut forward = |p: MessagesProgress| {
                if p.phase == MessagesPhase::StagingMessagesDb {
                    progress.event(ArchiveEvent::Import {
                        phase: ImportPhase::StagingMessagesDb,
                        done: p.done,
                        total: p.total,
                    });
                }
            };
            let mut sctx = MessagesCtx::new(cancel, &mut forward);
            match stage(ob, opts.wal, &mut sctx) {
                Ok(s) => s,
                Err(MessagesError::Cancelled) => return Ok(ImportOutcome::Cancelled(None)),
                Err(e) => return Err(messages_err(e)),
            }
        };
        let mut msink = |_: MessagesProgress| {};
        let mut mctx = MessagesCtx::new(cancel, &mut msink);
        let result = self.import_staged(ob, &staged, opts, gate, ctx, &mut mctx);
        // §8.11: the plaintext copy goes before finalize.
        let removed = staged.remove().is_ok();
        if !removed {
            tracing::warn!("the staged sms.db was not removed; closing the backup removes it");
        }
        match result? {
            Staged::Done(outcome) => Ok(with_staging(outcome, removed)),
            Staged::Finalize(finish) => self.finish(*finish, ctx).map(|o| with_staging(o, removed)),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn import_staged(
        &mut self,
        ob: &OpenBackup,
        staged: &oma_backup::StagedDatabase<'_>,
        opts: &ImportOptions,
        gate: &mut dyn ImportGate,
        ctx: &mut ArchiveCtx<'_>,
        mctx: &mut MessagesCtx<'_>,
    ) -> Result<Staged, ArchiveError> {
        let done = |o: ImportOutcome| Ok(Staged::Done(o));
        ctx.progress.event(ArchiveEvent::Import {
            phase: ImportPhase::ReadingSchema,
            done: 0,
            total: None,
        });
        let conn = match connect(staged, mctx) {
            Ok(Connect::Ready(c)) => c,
            Ok(Connect::SchemaExceedsLimits) => {
                return done(ImportOutcome::NotImportable(
                    NotImportable::SchemaExceedsLimits,
                ))
            }
            Err(MessagesError::Cancelled) => return done(ImportOutcome::Cancelled(None)),
            Err(e) => return Err(messages_err(e)),
        };
        let probe_opts = ProbeOptions::default().with_keep_raw_values(true);
        let db = match SmsDb::open_with(&conn, &probe_opts, mctx) {
            Ok(Probe::Supported(db)) => db,
            Ok(Probe::Unsupported(_)) => {
                return done(ImportOutcome::NotImportable(NotImportable::Unsupported))
            }
            Ok(Probe::Damaged(_)) => {
                return done(ImportOutcome::NotImportable(NotImportable::Damaged))
            }
            Ok(_) => return done(ImportOutcome::NotImportable(NotImportable::Incomplete)),
            Err(MessagesError::Cancelled) => return done(ImportOutcome::Cancelled(None)),
            Err(e) => return Err(messages_err(e)),
        };

        // Facts (reads only). The lineage is chosen at the gate.
        let now = ctx.clock.now_ns();
        let mut facts =
            match session::facts(ob, staged, db.capabilities(), opts, now, None, ctx.cancel) {
                Ok(f) => f,
                Err(oma_backup::BackupError::Cancelled) => {
                    return done(ImportOutcome::Cancelled(None))
                }
                Err(e) => {
                    return Err(messages_err(MessagesError::Backup {
                        source: e,
                        stage: oma_messages::BackupStage::Opening,
                    }))
                }
            };
        // P0 and the key scan always run, before the gate: it shows their
        // evidence before the lineage is chosen. A continued session keeps
        // its stored duplicate set, which is the same set, because the
        // binding includes the content digest.
        ctx.progress.event(ArchiveEvent::Import {
            phase: ImportPhase::ScanningRecentlyDeleted,
            done: 0,
            total: None,
        });
        let rd = match scan_recoverable(&db, mctx) {
            Ok(RecoverableScan::Done(rd)) => rd,
            Ok(RecoverableScan::OverLimit) => {
                return done(ImportOutcome::NotImportable(
                    NotImportable::OverRecentlyDeletedCap,
                ))
            }
            Err(MessagesError::Cancelled) => return done(ImportOutcome::Cancelled(None)),
            Err(e) => return Err(messages_err(e)),
        };
        let rd_messages = u64::try_from(rd.len()).unwrap_or(u64::MAX);
        ctx.progress.event(ArchiveEvent::Import {
            phase: ImportPhase::ScanningKeys,
            done: 0,
            total: None,
        });
        // The archive lookup of each message key runs on the reader, under
        // an op budget that grows with the message table (bounded by the
        // key-scan cap).
        let exclude_rd = !opts.include_recently_deleted;
        let message_rows = db
            .capabilities()
            .table(oma_messages::KnownTable::Message)
            .and_then(|t| t.rows)
            .unwrap_or(0);
        let ops = READ_OP_BUDGET.saturating_add(message_rows.saturating_mul(256));
        let scanned = self.reader().budgeted(ops, Stage::Preflight, |c| {
            let mut stmt = c.prepare(MESSAGE_KEY_ARCHIVED_SQL)?;
            let mut lookup = |k: &[u8]| stmt.exists([k]);
            Ok(scan_keys(&db, mctx, rd, exclude_rd, &mut lookup))
        })?;
        let (scanned, rd_plan, tallies) = match scanned {
            Ok(KeyScan::Done(d, plan, tallies)) => (d, plan, tallies),
            Ok(KeyScan::OverLimit(e)) => {
                let t = match e {
                    Entity::Handle => KeyScanTable::Handles,
                    Entity::Chat => KeyScanTable::Chats,
                    Entity::Message => KeyScanTable::Messages,
                    _ => KeyScanTable::Attachments,
                };
                return done(ImportOutcome::NotImportable(NotImportable::OverKeyScanCap(
                    t,
                )));
            }
            Err(ScanError::Read(MessagesError::Cancelled)) => {
                return done(ImportOutcome::Cancelled(None))
            }
            Err(ScanError::Read(e)) => return Err(messages_err(e)),
            Err(ScanError::Archive(e)) => return Err(ArchiveError::sqlite(Stage::Preflight, &e)),
        };
        // Step 4: the attachments linked only to excluded messages, for the
        // cap and the estimate; P5b re-derives the set after P3 from
        // durable state.
        let mut rd_only: Vec<i64> = Vec::new();
        if !rd_plan.excluded.is_empty() {
            ctx.progress.event(ArchiveEvent::Import {
                phase: ImportPhase::ScanningRecentlyDeleted,
                done: 0,
                total: None,
            });
            match rd::scan_attachments(&db, &rd_plan.excluded, mctx) {
                Ok(rd::AttachmentScan::Done(a)) => rd_only = a,
                Ok(rd::AttachmentScan::OverLimit) => {
                    return done(ImportOutcome::NotImportable(
                        NotImportable::OverRecentlyDeletedCap,
                    ))
                }
                Err(MessagesError::Cancelled) => return done(ImportOutcome::Cancelled(None)),
                Err(e) => return Err(messages_err(e)),
            }
        }
        // Step 5 and §12: the attachment and space estimates.
        let attachments = if opts.copy_attachments {
            ctx.progress.event(ArchiveEvent::Import {
                phase: ImportPhase::EstimatingAttachments,
                done: 0,
                total: None,
            });
            let rows = db
                .capabilities()
                .table(oma_messages::KnownTable::Attachment)
                .and_then(|t| t.rows)
                .unwrap_or(0);
            let ops = READ_OP_BUDGET.saturating_add(rows.saturating_mul(256));
            let est = self.reader().budgeted(ops, Stage::Preflight, |c| {
                let mut stmt = c.prepare(attachments::CURRENT_BLOB_SQL)?;
                let mut lookup = |k: &[u8]| -> rusqlite::Result<Option<u64>> {
                    use rusqlite::OptionalExtension;
                    stmt.query_row([k], |r| r.get::<_, i64>(1))
                        .optional()
                        .map(|l| l.and_then(|l| u64::try_from(l).ok()))
                };
                Ok(attachments::estimate(
                    &db,
                    ob,
                    &scanned,
                    &rd_only,
                    mctx,
                    &mut lookup,
                ))
            })?;
            match est {
                Ok(Ok(e)) => e,
                Ok(Err(e)) => return Err(ArchiveError::sqlite(Stage::Preflight, &e)),
                Err(MessagesError::Cancelled) => return done(ImportOutcome::Cancelled(None)),
                Err(e) => return Err(messages_err(e)),
            }
        } else {
            attachments::AttachmentEstimate::default()
        };
        let (probe, dir, store) = self.store_parts();
        let reserve = reserve_for(probe.total(dir.root()).ok());
        let allowance = wal_allowance(
            opts.batch.bytes(),
            u64::try_from(JOURNAL_SIZE_LIMIT).unwrap_or(0),
        );
        let staged_bytes = u64::try_from(facts.binding.main_len)
            .unwrap_or(0)
            .saturating_add(
                facts
                    .binding
                    .wal_len
                    .and_then(|l| u64::try_from(l).ok())
                    .unwrap_or(0),
            );
        let estimate = SpaceEstimate::new(
            staged_bytes,
            attachments.bytes,
            attachments.files,
            attachments.largest,
            reserve,
            allowance,
            probe.available(dir.root()).ok(),
        );
        if estimate.refuses() {
            tracing::info!(
                needed = estimate.needed,
                floor = estimate.floor(),
                "import refused: not enough free space"
            );
            return done(ImportOutcome::InsufficientSpace(estimate));
        }
        let guard = SpaceGuard::new(probe, dir.root(), reserve, allowance);

        // The gate (§8.2 step 7). Nothing has been written.
        let side = self
            .reader()
            .budgeted(READ_OP_BUDGET, Stage::Preflight, |c| {
                gate::archive_side(c, &facts, &facts.binding)
            })?;
        let preflight = gate::preflight(
            side,
            &facts,
            &tallies,
            &rd_plan,
            u64::try_from(rd_only.len()).unwrap_or(u64::MAX),
            estimate,
            ob.is_encrypted(),
            oma_fs::cloud_sync_provider(dir.root()).is_some(),
        );
        if ctx.cancel.is_cancelled() {
            return done(ImportOutcome::Cancelled(None));
        }
        ctx.progress.event(ArchiveEvent::Import {
            phase: ImportPhase::AwaitingConfirmation,
            done: 0,
            total: None,
        });
        crate::fault::point(crate::fault::FaultPoint::GateWaiting);
        let lineage = match gate.confirm(&preflight) {
            GateDecision::Proceed { lineage } => lineage,
            GateDecision::Decline => {
                tracing::info!("import declined at the gate");
                return done(ImportOutcome::Declined);
            }
        };
        drop(preflight);
        if ctx.cancel.is_cancelled() {
            return done(ImportOutcome::Cancelled(None));
        }
        if let LineageChoice::Existing(id) = lineage {
            let known = self
                .reader()
                .budgeted(READ_OP_BUDGET, Stage::Preflight, |c| {
                    c.query_row(
                        "SELECT EXISTS (SELECT 1 FROM lineage WHERE id = ?1)",
                        [id.0],
                        |r| r.get::<_, bool>(0),
                    )
                })?;
            if !known {
                return Err(ArchiveError::UnknownLineage);
            }
        }
        facts.binding.lineage = match lineage {
            LineageChoice::Existing(id) => Some(id.0),
            LineageChoice::New => None,
        };
        let decision = self
            .reader()
            .budgeted(READ_OP_BUDGET, Stage::Preflight, |c| {
                decide_resume(c, &facts.binding, false)
            })?;

        // The first write, then the session.
        ctx.progress.event(ArchiveEvent::Import {
            phase: ImportPhase::PreparingArchive,
            done: 0,
            total: None,
        });
        self.begin_write(Recovery::Run)?;
        let w = self.ensure_writer()?;
        let wc = w.conn_mut();
        let dups = match &decision.resume {
            Some(r) => {
                load_dup_keys(wc, r.id).map_err(|e| ArchiveError::sqlite(Stage::Session, &e))?
            }
            None => scanned,
        };
        // A continued session merges at its stored rank (spec §8.7: one
        // session, one rank), whatever this attempt's dates give.
        let rank = decision.resume.as_ref().map_or(facts.rank, |r| r.rank);
        let started = session::begin_session(
            wc,
            &facts,
            lineage,
            decision.resume.clone(),
            &decision.supersede,
            &dups,
            now,
        )?;
        tracing::info!(
            session = started.session,
            resumed = started.resumed,
            superseded = started.superseded,
            dated = rank.is_dated(),
            qualifiers = facts.qualifiers,
            "import session started"
        );
        let run = Run {
            session: started.session,
            lineage: started.lineage,
            rank,
            dups,
            budget: opts.batch,
            guard,
        };
        let mut counts = started.counts.clone();
        // P0 is re-run on every resume and finds the same set.
        counts.recently_deleted.messages = rd_messages;
        let summary = |counts: &ImportCounts| summary_of(&started, counts, estimate);

        // The passes, in order; each stops the chain unless it is done.
        let ended = (|| {
            let step = |r: Result<PassEnd, PassError>, pass: Pass| match r {
                Ok(PassEnd::Done) => Ok(None),
                Ok(e) => Ok(Some((e, pass))),
                Err(e) => Err((e, pass)),
            };
            let p = run_pass(
                &handles::Handles::new(),
                &db,
                wc,
                &run,
                &mut counts,
                ctx,
                mctx,
            );
            if let Some(end) = step(p, Pass::Handles)? {
                return Ok(end);
            }
            let p = run_pass(&chats::Chats::new(), &db, wc, &run, &mut counts, ctx, mctx);
            if let Some(end) = step(p, Pass::Chats)? {
                return Ok(end);
            }
            let p = run_pass(
                &messages::Messages::new(rd_plan.clone()),
                &db,
                wc,
                &run,
                &mut counts,
                ctx,
                mctx,
            );
            if let Some(end) = step(p, Pass::Messages)? {
                return Ok(end);
            }
            // The Recently Deleted decisions, from this session's map.
            let mut skip = rd::derive_after_p3(wc, run.session, &rd_plan)
                .map_err(|e| (PassError::Batch(e), Pass::Messages))?;
            let p = run_pass(&links::ChatHandles, &db, wc, &run, &mut counts, ctx, mctx);
            if let Some(end) = step(p, Pass::ChatHandles)? {
                return Ok(end);
            }
            let p = run_pass(
                &links::ChatMessages::new(&skip),
                &db,
                wc,
                &run,
                &mut counts,
                ctx,
                mctx,
            );
            if let Some(end) = step(p, Pass::ChatMessages)? {
                return Ok(end);
            }
            // P5b: reads only; re-run on every resume.
            if !skip.excluded.is_empty() {
                ctx.progress.event(ArchiveEvent::Import {
                    phase: ImportPhase::ScanningRecentlyDeleted,
                    done: 0,
                    total: None,
                });
            }
            match rd::scan_attachments(&db, &skip.excluded, mctx) {
                Ok(rd::AttachmentScan::Done(a)) => skip.attachments = a,
                Ok(rd::AttachmentScan::OverLimit) => {
                    return Err((PassError::ScanOverLimit, Pass::Attachments))
                }
                Err(MessagesError::Cancelled) => {
                    return Ok((PassEnd::Cancelled, Pass::Attachments))
                }
                Err(e) => return Err((PassError::Read(e), Pass::Attachments)),
            }
            let p = run_pass(
                &attachments::Attachments::new(
                    &skip,
                    attachments::StoreCtx {
                        ob,
                        dir: dir.clone(),
                        store: std::sync::Arc::clone(&store),
                        cancel: ctx.cancel,
                        copy: opts.copy_attachments,
                    },
                ),
                &db,
                wc,
                &run,
                &mut counts,
                ctx,
                mctx,
            );
            if let Some(end) = step(p, Pass::Attachments)? {
                return Ok(end);
            }
            let p = run_pass(
                &links::MessageAttachments::new(&skip),
                &db,
                wc,
                &run,
                &mut counts,
                ctx,
                mctx,
            );
            if let Some(end) = step(p, Pass::MessageAttachments)? {
                return Ok(end);
            }
            let p = run_pass(
                &recently_deleted::RecentlyDeleted,
                &db,
                wc,
                &run,
                &mut counts,
                ctx,
                mctx,
            );
            if let Some(end) = step(p, Pass::RecentlyDeleted)? {
                return Ok(end);
            }
            // §9.3: the backup must be as it was when it was opened. An
            // unreadable stamp counts as a change (the stop is resumable).
            if !ob.unchanged_since_open().unwrap_or(false) {
                return Err((
                    PassError::Stopped(StopReason::SourceChanged),
                    Pass::RecentlyDeleted,
                ));
            }
            Ok((PassEnd::Done, Pass::RecentlyDeleted))
        })();
        let end_now = ctx.clock.now_ns();
        match ended {
            Ok((PassEnd::Done, _)) => {
                // The staged copy was only read (M3 §7.5): `query_only`, no
                // authorizer refusal (`part_text` included) and no change.
                // None is the guard; the authorizer is.
                let untouched = conn.query_only().unwrap_or(false)
                    && conn.authorizer_denials() == 0
                    && conn.total_changes() == 0;
                if !untouched {
                    SpaceGuard::lift(wc);
                    session::end_session(
                        wc,
                        started.session,
                        SessionState::Failed,
                        Some((Pass::RecentlyDeleted.code(), IssueCode::SourceReadFailed)),
                        &counts,
                        end_now,
                    );
                    provenance::write(&dir, wc, started.session);
                    tracing::warn!(
                        session = started.session,
                        "the staged sms.db was not only read"
                    );
                    return Err(ArchiveError::Internal(
                        "the staged sms.db connection was written or refused an action",
                    ));
                }
                tracing::info!(
                    session = started.session,
                    attachment_files_copied = counts.attachment_files.copied,
                    attachment_files_already_archived = counts.attachment_files.already_archived,
                    attachment_bytes_copied = counts.attachment_files.bytes_copied,
                    handles = counts.handles.seen,
                    chats = counts.chats.seen,
                    messages = counts.messages.seen,
                    bodies = counts.bodies.added,
                    chat_handles = counts.chat_handles.seen,
                    chat_messages = counts.chat_messages.seen,
                    attachments = counts.attachments.seen,
                    message_attachments = counts.message_attachments.seen,
                    rd_excluded = counts.recently_deleted.excluded,
                    rd_attachments_excluded = counts.recently_deleted.attachments_excluded,
                    rd_marked = counts.recently_deleted.marked,
                    batches = counts.batches,
                    "import passes done"
                );
                Ok(Staged::Finalize(Box::new(Finish {
                    run,
                    counts,
                    started,
                    estimate,
                    qualifiers: facts.qualifiers,
                })))
            }
            Ok((PassEnd::Cancelled, _)) => {
                SpaceGuard::lift(wc);
                session::end_session(
                    wc,
                    started.session,
                    SessionState::Cancelled,
                    None,
                    &counts,
                    end_now,
                );
                let mut s = summary(&counts);
                s.provenance_written = provenance::write(&dir, wc, started.session);
                tracing::info!(session = started.session, "import cancelled");
                done(ImportOutcome::Cancelled(Some(s)))
            }
            Err((e, pass)) => {
                SpaceGuard::lift(wc);
                let mut s = summary(&counts);
                match end_with_error(wc, &dir, started.session, pass, e, &counts, end_now) {
                    Ok((reason, written)) => {
                        s.provenance_written = written;
                        done(ImportOutcome::Stopped { reason, summary: s })
                    }
                    Err(err) => Err(err),
                }
            }
        }
    }

    /// Finalize (P9) and the provenance file, after the staged copy is
    /// removed.
    fn finish(
        &mut self,
        f: Finish,
        ctx: &mut ArchiveCtx<'_>,
    ) -> Result<ImportOutcome, ArchiveError> {
        let dir = self.store_parts().1;
        let w = self.ensure_writer()?;
        let wc = w.conn_mut();
        let result = finalize::finalize(wc, &f.run, &f.counts, f.qualifiers, ctx);
        SpaceGuard::lift(wc);
        let session = f.started.session;
        let end_now = ctx.clock.now_ns();
        let mut summary = f.summary(&f.counts);
        match result {
            Ok(report) => {
                summary.finalize = Some(report);
                summary.provenance_written = provenance::write(&dir, wc, session);
                tracing::info!(
                    session,
                    fts_merge_steps = report.fts_merge_steps,
                    fts_merge_complete = report.fts_merge_complete,
                    session_rows_removed = report.session_rows_removed,
                    provenance_written = summary.provenance_written,
                    "import completed"
                );
                Ok(ImportOutcome::Completed(summary))
            }
            Err(NotCompleted::Cancelled) => {
                session::end_session(
                    wc,
                    session,
                    SessionState::Cancelled,
                    None,
                    &f.counts,
                    end_now,
                );
                summary.provenance_written = provenance::write(&dir, wc, session);
                tracing::info!(session, "import cancelled while finalizing");
                Ok(ImportOutcome::Cancelled(Some(summary)))
            }
            Err(NotCompleted::Failed(e)) => {
                match end_with_error(wc, &dir, session, Pass::Finalize, e, &f.counts, end_now) {
                    Ok((reason, written)) => {
                        summary.provenance_written = written;
                        Ok(ImportOutcome::Stopped { reason, summary })
                    }
                    Err(err) => Err(err),
                }
            }
            // The session stays running (spec §8.9 step 4): no terminal
            // state is recorded, and no provenance file is written.
            Err(NotCompleted::TerminalFailed(e)) => match finalize::terminal_stop(&e) {
                Some(reason) => {
                    tracing::info!(
                        session,
                        ?reason,
                        "the terminal state was not recorded; the session stays resumable"
                    );
                    Ok(ImportOutcome::Stopped { reason, summary })
                }
                None => {
                    let (_, issue, out) = classify(e);
                    tracing::warn!(
                        session,
                        ?issue,
                        "the terminal state was not recorded; the session stays resumable"
                    );
                    Err(out.err().unwrap_or(ArchiveError::Internal(
                        "the terminal state was not recorded",
                    )))
                }
            },
        }
    }
}

/// Record a pass or finalize error (spec §8.5): the state, the issue, and
/// the provenance file. A stop gives its reason and whether the provenance
/// file was written; anything else gives the error.
fn end_with_error(
    wc: &mut rusqlite::Connection,
    dir: &ArchiveDir,
    session: i64,
    pass: Pass,
    e: PassError,
    counts: &ImportCounts,
    now_ns: i64,
) -> Result<(StopReason, bool), ArchiveError> {
    let (state, issue, out) = classify(e);
    session::end_session(
        wc,
        session,
        state,
        Some((pass.code(), issue)),
        counts,
        now_ns,
    );
    let written = provenance::write(dir, wc, session);
    match out {
        Ok(reason) => {
            tracing::info!(session, ?reason, "import stopped");
            Ok((reason, written))
        }
        Err(err) => {
            tracing::warn!(session, error = %err, ?issue, "import failed");
            Err(err)
        }
    }
}

/// What a pass error means for the session (spec §8.5): the state to
/// record, the issue code, and either a stop reason or the error.
fn classify(e: PassError) -> (SessionState, IssueCode, Result<StopReason, ArchiveError>) {
    const SQLITE_FULL: i32 = 13;
    const SQLITE_CONSTRAINT: i32 = 19;
    match e {
        PassError::Batch(e) => match primary_code(&e) {
            Some(SQLITE_FULL) => (
                SessionState::Stopped,
                IssueCode::InsufficientSpace,
                Ok(StopReason::InsufficientSpace),
            ),
            Some(SQLITE_CONSTRAINT) => (
                SessionState::Failed,
                IssueCode::ConstraintFailed,
                Err(ArchiveError::Internal("archive constraint failed")),
            ),
            Some(_) => (
                SessionState::Failed,
                IssueCode::ArchiveError,
                Err(ArchiveError::sqlite(Stage::Batch, &e)),
            ),
            None => (
                SessionState::Failed,
                IssueCode::ArchiveError,
                Err(ArchiveError::Damaged(DamageKind::Unreadable)),
            ),
        },
        PassError::Read(e) => (
            SessionState::Failed,
            IssueCode::SourceReadFailed,
            Err(messages_err(e)),
        ),
        PassError::CountMismatch => (
            SessionState::Failed,
            IssueCode::ArchiveError,
            Err(ArchiveError::Internal(
                "pass row count differs from the probe",
            )),
        ),
        // The preflight ran the same scan under the same cap on the same
        // staged copy, so this is unreachable unless the copy changed.
        PassError::ScanOverLimit => (
            SessionState::Failed,
            IssueCode::ArchiveError,
            Err(ArchiveError::Internal(
                "the Recently Deleted attachment scan exceeded its cap after the preflight",
            )),
        ),
        PassError::Stopped(r) => (SessionState::Stopped, r.issue(), Ok(r)),
        PassError::Store(kind) => (
            SessionState::Failed,
            IssueCode::ArchiveError,
            Err(ArchiveError::Io {
                op: crate::error::IoOp::Sync,
                kind,
                os: None,
            }),
        ),
    }
}
