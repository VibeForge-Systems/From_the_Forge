//! The import core (spec §8; slice 4f).
//!
//! [`Archive::import_core`] runs the parts of the import that exist so
//! far, in the order of spec §2:
//!
//! 1. stage sms.db (and its `-wal`, best effort) and probe it;
//! 2. compute the session's facts: the content digest and its framing, the
//!    read profile, the rank (§8.7), the qualifiers;
//! 3. the duplicate-key scan (§8.2 step 3), which always runs, then the
//!    resume decision (step 1);
//! 4. the archive's first write ([`Archive::begin_write`] with recovery),
//!    then the session bookkeeping transaction (§8.3);
//! 5. **P1 handles and P2 chats**, in batches (§8.5), each committing its
//!    cursor, counts and issues with its rows.
//!
//! **What is not here yet.** The Recently Deleted and attachment
//! preflight, the gate, P3–P8 and finalize arrive in slices 4g–4j. Until
//! finalize exists, a session whose passes all ran is left **running**
//! (state 0) and the outcome is [`CoreOutcome::PassesDone`]: it is never
//! marked completed or a full observation. The next write open marks it
//! interrupted, and an import of the same content continues it (its passes
//! are already done). A cancel, a stop or a failure is recorded as such.
//!
//! The shell does not call this yet (slice 4l).

mod batch;
mod bind;
mod chats;
mod counts;
mod entity;
mod handles;
mod keys;
mod map;
mod merge;
mod preflight;
mod session;

use oma_backup::{OpenBackup, WalPolicy};
use oma_messages::{
    connect, stage, Connect, MessagesCtx, MessagesError, MessagesPhase, MessagesProgress, Probe,
    ProbeOptions, ServiceClass, SmsDb,
};

pub use batch::BatchBudget;
pub use counts::{EntityCounts, ImportCounts};
pub use preflight::KEY_SCAN_CAP;
pub(crate) use session::{supersede_sessions, unfinished_sessions};
pub use session::{ORDER_KEY_FLOOR_NS, ORDER_KEY_SLACK_NS};

use crate::archive::{Archive, Recovery};
use crate::conn::READ_OP_BUDGET;
use crate::error::{primary_code, ArchiveError, DamageKind, Stage};
use crate::progress::{ArchiveCtx, ArchiveEvent, ImportPhase};
use crate::schema::codes::{Entity, IssueCode, Pass, ServiceClassCode, SessionState};
use batch::{run_pass, PassEnd, PassError, Run};
use bind::Cell;
use preflight::{decide_resume, load_dup_keys, scan_keys, KeyScan};

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
    /// Every pass this build has (P1, P2) is done. The session stays
    /// running (state 0) until finalize exists (slice 4j); see the module
    /// documentation.
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
        // The key scan always runs, before the resume decision: the gate
        // (slice 4j) shows its evidence before the lineage is chosen. A
        // continued session keeps its stored duplicate set, which is the
        // same set, because the binding includes the content digest.
        ctx.progress.event(ArchiveEvent::Import {
            phase: ImportPhase::ScanningKeys,
            done: 0,
            total: None,
        });
        let scanned = match scan_keys(&db, mctx) {
            Ok(KeyScan::Done(d)) => d,
            Ok(KeyScan::OverLimit(e)) => {
                let t = match e {
                    Entity::Handle => KeyScanTable::Handles,
                    Entity::Chat => KeyScanTable::Chats,
                    Entity::Message => KeyScanTable::Messages,
                    _ => KeyScanTable::Attachments,
                };
                return Ok(CoreOutcome::NotImportable(NotImportable::OverKeyScanCap(t)));
            }
            Err(MessagesError::Cancelled) => return Ok(CoreOutcome::Cancelled(None)),
            Err(e) => return Err(messages_err(e)),
        };
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
        let summary = |counts: &ImportCounts| ImportSummary {
            session: started.session,
            lineage: LineageId(started.lineage),
            resumed: started.resumed,
            superseded: started.superseded,
            counts: counts.clone(),
        };

        // The passes.
        let p1 = run_pass(
            &handles::Handles::new(),
            &db,
            wc,
            &run,
            &mut counts,
            ctx,
            mctx,
        );
        let ended = match p1 {
            Ok(PassEnd::Done) => {
                run_pass(&chats::Chats::new(), &db, wc, &run, &mut counts, ctx, mctx)
                    .map(|e| (e, Pass::Chats))
                    .map_err(|e| (e, Pass::Chats))
            }
            Ok(e) => Ok((e, Pass::Handles)),
            Err(e) => Err((e, Pass::Handles)),
        };
        let end_now = ctx.clock.now_ns();
        match ended {
            Ok((PassEnd::Done, _)) => {
                tracing::info!(
                    session = started.session,
                    handles = counts.handles.seen,
                    chats = counts.chats.seen,
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
    }
}
