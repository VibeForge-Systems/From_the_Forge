//! The import core (spec §8; slices 4f to 4h).
//!
//! [`Archive::import_core`] runs the parts of the import that exist so
//! far, in the order of spec §2:
//!
//! 1. stage sms.db (and its `-wal`, best effort) and probe it;
//! 2. compute the session's facts: the content digest and its framing, the
//!    read profile, the rank (§8.7), the qualifiers;
//! 3. P0, the Recently Deleted scan (§8.2 step 2), then the duplicate-key
//!    scan with the Recently Deleted classification (step 3), then the scan
//!    for attachments linked only to excluded messages (step 4, which
//!    refuses a source over its cap), which always run, then the resume
//!    decision (step 1);
//! 4. the archive's first write ([`Archive::begin_write`] with recovery),
//!    then the session bookkeeping transaction (§8.3);
//! 5. the passes, in batches (§8.5), each committing its cursor, counts and
//!    issues with its rows: **P1 handles, P2 chats, P3 messages** (with
//!    their body versions, the FTS index through its triggers, and the
//!    Recently Deleted policy), **P4 chat ↔ handle, P5 chat ↔ message, P5b**
//!    (the Recently Deleted attachment exclusion, re-derived from durable
//!    state; it writes nothing), **P6 attachment rows, P7 message ↔
//!    attachment and P8 the Recently Deleted markers**.
//!
//! **What is not here yet.** P6 writes the attachment rows only: no file
//! is looked up, read or copied, and no `blob` or `attachment_observation`
//! row exists until the store arrives (slice 4i). The attachment estimate,
//! the gate and finalize arrive in slices 4i and 4j. Until finalize exists,
//! a session whose passes all ran is left **running** (state 0) and the
//! outcome is [`CoreOutcome::PassesDone`]: it is never marked completed or
//! a full observation. The next write open marks it interrupted, and an
//! import of the same content continues it (its passes are already done).
//! A cancel, a stop or a failure is recorded as such.
//!
//! The shell does not call this yet (slice 4l).

mod attachments;
mod batch;
mod bind;
mod chats;
mod counts;
mod entity;
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
pub use counts::{BodyCounts, EntityCounts, ImportCounts, LinkCounts, RecentlyDeletedCounts};
pub use preflight::{KEY_SCAN_CAP, RECOVERABLE_SCAN_CAP};
pub(crate) use session::{supersede_sessions, unfinished_sessions};
pub use session::{ORDER_KEY_FLOOR_NS, ORDER_KEY_SLACK_NS};

use crate::archive::{Archive, Recovery};
use crate::conn::READ_OP_BUDGET;
use crate::error::{primary_code, ArchiveError, DamageKind, Stage};
use crate::progress::{ArchiveCtx, ArchiveEvent, ImportPhase};
use crate::schema::codes::{Entity, IssueCode, Pass, ServiceClassCode, SessionState};
use batch::{run_pass, PassEnd, PassError, Run};
use bind::Cell;
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
    /// SQLite reported the disk or the database full (`SQLITE_FULL`).
    InsufficientSpace,
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
}

/// The outcome of [`Archive::import_core`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CoreOutcome {
    /// Every pass this build has (P1–P8, P6 without files) is done. The
    /// session stays running (state 0) until finalize exists (slice 4j);
    /// see the module documentation.
    PassesDone(ImportSummary),
    /// Cancelled. With a session, its completed records are committed and
    /// it is resumable (state 2); `None` when the cancel came before the
    /// session started, and nothing was written.
    Cancelled(Option<ImportSummary>),
    /// Stopped in a resumable state (state 6).
    Stopped {
        reason: StopReason,
        summary: ImportSummary,
    },
    /// The backup cannot be imported; nothing was written.
    NotImportable(NotImportable),
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
    /// Import a validated, opened backup into this archive: the import
    /// core of slice 4f (module documentation). `lineage` is the user's
    /// choice at the gate (D80); to continue an unfinished session, pass
    /// that session's lineage.
    ///
    /// The staged sms.db is removed before this returns (the backup's
    /// `close` is the backstop).
    pub fn import_core(
        &mut self,
        ob: &OpenBackup,
        opts: &ImportOptions,
        lineage: LineageChoice,
        ctx: &mut ArchiveCtx<'_>,
    ) -> Result<CoreOutcome, ArchiveError> {
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
                Err(MessagesError::Cancelled) => return Ok(CoreOutcome::Cancelled(None)),
                Err(e) => return Err(messages_err(e)),
            }
        };
        let mut msink = |_: MessagesProgress| {};
        let mut mctx = MessagesCtx::new(cancel, &mut msink);
        let result = self.import_staged(ob, &staged, opts, lineage, ctx, &mut mctx);
        if staged.remove().is_err() {
            tracing::warn!("the staged sms.db was not removed; closing the backup removes it");
        }
        result
    }

    fn import_staged(
        &mut self,
        ob: &OpenBackup,
        staged: &oma_backup::StagedDatabase<'_>,
        opts: &ImportOptions,
        lineage: LineageChoice,
        ctx: &mut ArchiveCtx<'_>,
        mctx: &mut MessagesCtx<'_>,
    ) -> Result<CoreOutcome, ArchiveError> {
        ctx.progress.event(ArchiveEvent::Import {
            phase: ImportPhase::ReadingSchema,
            done: 0,
            total: None,
        });
        let conn = match connect(staged, mctx) {
            Ok(Connect::Ready(c)) => c,
            Ok(Connect::SchemaExceedsLimits) => {
                return Ok(CoreOutcome::NotImportable(
                    NotImportable::SchemaExceedsLimits,
                ))
            }
            Err(MessagesError::Cancelled) => return Ok(CoreOutcome::Cancelled(None)),
            Err(e) => return Err(messages_err(e)),
        };
        let probe_opts = ProbeOptions::default().with_keep_raw_values(true);
        let db = match SmsDb::open_with(&conn, &probe_opts, mctx) {
            Ok(Probe::Supported(db)) => db,
            Ok(Probe::Unsupported(_)) => {
                return Ok(CoreOutcome::NotImportable(NotImportable::Unsupported))
            }
            Ok(Probe::Damaged(_)) => return Ok(CoreOutcome::NotImportable(NotImportable::Damaged)),
            Ok(_) => return Ok(CoreOutcome::NotImportable(NotImportable::Incomplete)),
            Err(MessagesError::Cancelled) => return Ok(CoreOutcome::Cancelled(None)),
            Err(e) => return Err(messages_err(e)),
        };

        // Facts and the resume decision (reads only).
        let now = ctx.clock.now_ns();
        let lineage_id = match lineage {
            LineageChoice::Existing(id) => Some(id.0),
            LineageChoice::New => None,
        };
        let facts = match session::facts(
            ob,
            staged,
            db.capabilities(),
            opts,
            now,
            lineage_id,
            ctx.cancel,
        ) {
            Ok(f) => f,
            Err(oma_backup::BackupError::Cancelled) => return Ok(CoreOutcome::Cancelled(None)),
            Err(e) => {
                return Err(messages_err(MessagesError::Backup {
                    source: e,
                    stage: oma_messages::BackupStage::Opening,
                }))
            }
        };
        // P0 and the key scan always run, before the resume decision: the
        // gate (slice 4j) shows their evidence before the lineage is
        // chosen. A continued session keeps its stored duplicate set, which
        // is the same set, because the binding includes the content digest.
        ctx.progress.event(ArchiveEvent::Import {
            phase: ImportPhase::ScanningRecentlyDeleted,
            done: 0,
            total: None,
        });
        let rd = match scan_recoverable(&db, mctx) {
            Ok(RecoverableScan::Done(rd)) => rd,
            Ok(RecoverableScan::OverLimit) => {
                return Ok(CoreOutcome::NotImportable(
                    NotImportable::OverRecentlyDeletedCap,
                ))
            }
            Err(MessagesError::Cancelled) => return Ok(CoreOutcome::Cancelled(None)),
            Err(e) => return Err(messages_err(e)),
        };
        let rd_messages = u64::try_from(rd.len()).unwrap_or(u64::MAX);
        ctx.progress.event(ArchiveEvent::Import {
            phase: ImportPhase::ScanningKeys,
            done: 0,
            total: None,
        });
        // The archive lookup of each Recently Deleted key runs on the
        // reader, under an op budget that grows with the set (bounded by
        // the P0 cap).
        let exclude_rd = !opts.include_recently_deleted;
        let ops = READ_OP_BUDGET.saturating_add(rd_messages.saturating_mul(256));
        let scanned = self.reader().budgeted(ops, Stage::Preflight, |c| {
            let mut stmt = c.prepare("SELECT 1 FROM message WHERE key_kind = 1 AND guid = ?1")?;
            let mut lookup = |k: &[u8]| stmt.exists([k]);
            Ok(scan_keys(&db, mctx, rd, exclude_rd, &mut lookup))
        })?;
        let (scanned, rd_plan) = match scanned {
            Ok(KeyScan::Done(d, plan)) => (d, plan),
            Ok(KeyScan::OverLimit(e)) => {
                let t = match e {
                    Entity::Handle => KeyScanTable::Handles,
                    Entity::Chat => KeyScanTable::Chats,
                    Entity::Message => KeyScanTable::Messages,
                    _ => KeyScanTable::Attachments,
                };
                return Ok(CoreOutcome::NotImportable(NotImportable::OverKeyScanCap(t)));
            }
            Err(ScanError::Read(MessagesError::Cancelled)) => {
                return Ok(CoreOutcome::Cancelled(None))
            }
            Err(ScanError::Read(e)) => return Err(messages_err(e)),
            Err(ScanError::Archive(e)) => return Err(ArchiveError::sqlite(Stage::Preflight, &e)),
        };
        // Step 4: the attachments linked only to excluded messages. Only
        // the cap matters here (the estimate that uses the set is slice
        // 4i's); P5b re-derives the set after P3 from durable state.
        if !rd_plan.excluded.is_empty() {
            ctx.progress.event(ArchiveEvent::Import {
                phase: ImportPhase::ScanningRecentlyDeleted,
                done: 0,
                total: None,
            });
            match rd::scan_attachments(&db, &rd_plan.excluded, mctx) {
                Ok(rd::AttachmentScan::Done(_)) => {}
                Ok(rd::AttachmentScan::OverLimit) => {
                    return Ok(CoreOutcome::NotImportable(
                        NotImportable::OverRecentlyDeletedCap,
                    ))
                }
                Err(MessagesError::Cancelled) => return Ok(CoreOutcome::Cancelled(None)),
                Err(e) => return Err(messages_err(e)),
            }
        }
        let decision = self
            .reader()
            .budgeted(READ_OP_BUDGET, Stage::Preflight, |c| {
                decide_resume(c, &facts.binding)
            })?;
        if ctx.cancel.is_cancelled() {
            return Ok(CoreOutcome::Cancelled(None));
        }

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
        };
        let mut counts = started.counts.clone();
        // P0 is re-run on every resume and finds the same set.
        counts.recently_deleted.messages = rd_messages;
        let summary = |counts: &ImportCounts| ImportSummary {
            session: started.session,
            lineage: LineageId(started.lineage),
            resumed: started.resumed,
            superseded: started.superseded,
            counts: counts.clone(),
        };

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
                &attachments::Attachments::new(&skip),
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
                    session::end_session(
                        wc,
                        started.session,
                        SessionState::Failed,
                        Some((Pass::RecentlyDeleted.code(), IssueCode::SourceReadFailed)),
                        &counts,
                        end_now,
                    );
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
                Ok(CoreOutcome::PassesDone(summary(&counts)))
            }
            Ok((PassEnd::Cancelled, _)) => {
                session::end_session(
                    wc,
                    started.session,
                    SessionState::Cancelled,
                    None,
                    &counts,
                    end_now,
                );
                tracing::info!(session = started.session, "import cancelled");
                Ok(CoreOutcome::Cancelled(Some(summary(&counts))))
            }
            Err((e, pass)) => {
                let (state, issue, out) = classify(e);
                session::end_session(
                    wc,
                    started.session,
                    state,
                    Some((pass.code(), issue)),
                    &counts,
                    end_now,
                );
                match out {
                    Ok(reason) => {
                        tracing::info!(session = started.session, ?reason, "import stopped");
                        Ok(CoreOutcome::Stopped {
                            reason,
                            summary: summary(&counts),
                        })
                    }
                    Err(err) => {
                        tracing::warn!(session = started.session, error = %err, ?issue, "import failed");
                        Err(err)
                    }
                }
            }
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
    }
}
