//! The import and verify jobs (spec §2, §15.1). Like the backup worker,
//! each runs on its own `std::thread`, talks to the UI only through
//! [`JobEventSink`], frees its job slot before its terminal event, and
//! sends exactly one.
//!
//! The import: sweep the staging folder; open the archive (read-only first,
//! under its lock); open and validate the backup; then `Archive::import`,
//! whose gate blocks this thread at [`Gate::wait`] until `confirm_import`,
//! a cancel, the exit handler or the two-minute timeout; then close the
//! backup (which removes the staging folder) and the archive.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;

use oma_archive::{
    ArchiveCtx, ArchiveError, ArchiveEvent, ArchiveId, ArchiveSink, GateDecision, ImportGate,
    ImportOptions, ImportOutcome, Preflight, SystemClock, VerifyDepth,
};
use oma_backup::{
    sweep_stale, CancelToken, CleanupReport, JobCtx, OpenBackup, OpenError, Progress, ProgressSink,
    StagingRoot,
};

use super::dto::{
    import_phase, preflight_dto, verify_phase, GateEndingDto, ImportOptionsInput, ImportOutcomeDto,
    NotImportableDto,
};
use super::gate::{Gate, GateAnswer, GateEnding};
use super::{messages, ArchiveEnv};
use crate::backups::dto::{CleanupDto, JobEvent, JobPhase};
use crate::backups::worker::{nothing_staged, JobSpec, Throttle};
use crate::backups::{extraction, messages as bm, JobEventSink, Jobs, LastOutcome};
use crate::error::IpcError;

/// Everything an import job needs. `job` is the backup part, prepared like
/// a validation (re-inspected, the password checked, the location
/// confirmed); its password is consumed by `OpenBackup::open`.
pub struct ImportSpec {
    pub job: JobSpec,
    pub archive_id: ArchiveId,
    pub options: ImportOptionsInput,
    pub gate: Arc<Gate>,
    pub env: ArchiveEnv,
}

/// Everything a verify job needs.
pub struct VerifySpec {
    pub job_id: String,
    pub archive_id: ArchiveId,
    pub library: PathBuf,
    pub depth: VerifyDepth,
    pub cancel: CancelToken,
    pub env: ArchiveEnv,
}

/// Progress from the backup, the import and the verify, through one
/// throttle shared with the gate.
struct Fwd<'r, 'a>(&'r RefCell<Throttle<'a>>);

impl ProgressSink for Fwd<'_, '_> {
    fn report(&mut self, p: Progress) {
        self.0
            .borrow_mut()
            .forward(JobPhase::from(p.phase), p.done, p.total);
    }
}

impl ArchiveSink for Fwd<'_, '_> {
    fn event(&mut self, event: ArchiveEvent) {
        let mapped = match event {
            ArchiveEvent::Import { phase, done, total } => {
                import_phase(phase).map(|p| (p, done, total))
            }
            ArchiveEvent::Verify { phase, done, total } => {
                verify_phase(phase).map(|p| (p, done, total))
            }
            _ => None,
        };
        if let Some((phase, done, total)) = mapped {
            self.0.borrow_mut().forward(phase, done, total);
        }
    }
}

/// The `ImportGate` of a job: publish the preflight, announce it, wait.
struct ShellGate<'r, 'a> {
    gate: &'r Gate,
    cancel: &'r CancelToken,
    jobs: &'r Jobs,
    job_id: &'r str,
    throttle: &'r RefCell<Throttle<'a>>,
    ending: Option<GateEnding>,
}

impl ImportGate for ShellGate<'_, '_> {
    fn confirm(&mut self, preflight: &Preflight) -> GateDecision {
        self.gate.open();
        self.jobs
            .set_preflight(self.job_id, Some(preflight_dto(preflight)));
        self.throttle.borrow_mut().send(JobEvent::ImportPreflight {
            job_id: self.job_id.to_owned(),
        });
        let ending = self.gate.wait(self.cancel);
        self.jobs.set_preflight(self.job_id, None);
        tracing::info!(job_id = self.job_id, ending = ?ending_kind(&ending), "gate answered");
        self.ending = Some(ending);
        match ending {
            GateEnding::Answered(GateAnswer::Proceed(lineage)) => GateDecision::Proceed { lineage },
            _ => GateDecision::Decline,
        }
    }
}

/// The ending without the lineage, for the log.
fn ending_kind(e: &GateEnding) -> &'static str {
    match e {
        GateEnding::Answered(GateAnswer::Proceed(_)) => "proceed",
        GateEnding::Answered(GateAnswer::Decline) => "declined",
        GateEnding::Cancelled => "cancelled",
        GateEnding::TimedOut => "timedOut",
    }
}

#[allow(clippy::large_enum_variant)] // one per job
enum ImportEnding {
    Imported(ImportOutcome, Option<GateEnding>),
    /// Validation found an error: the backup is not importable.
    NotValid,
    Failed(IpcError),
    Cancelled,
}

/// Run one import to its end (module documentation).
pub fn run_import(spec: ImportSpec, jobs: &Jobs, sink: &mut dyn JobEventSink) {
    let ImportSpec {
        job,
        archive_id,
        options,
        gate,
        env,
    } = spec;
    let JobSpec {
        job_id,
        inspection,
        library,
        password,
        limits,
        cancel,
        ..
    } = job;
    tracing::info!(job_id = %job_id, kind = "import", "job started");
    let untested = extraction::ios_version_untested(
        inspection.ios_version().as_ref().map(|s| s.value.as_str()),
    );
    let sweep = sweep_stale(&library);
    if sweep.removed + sweep.failed + sweep.in_use > 0 {
        tracing::info!(
            removed = sweep.removed,
            in_use = sweep.in_use,
            failed = sweep.failed,
            "staging swept before job"
        );
    }

    let (ending, cleanup) = {
        let throttle = RefCell::new(Throttle::new(&job_id, jobs, &mut *sink));
        let fail = |e: &ArchiveError| ImportEnding::Failed(messages::to_ipc(e, &limits));
        'run: {
            if cancel.is_cancelled() {
                break 'run (ImportEnding::Cancelled, nothing_staged());
            }
            let lib = match env.library(&library) {
                Ok(l) => l,
                Err(e) => break 'run (fail(&e), nothing_staged()),
            };
            let mut archive = match lib.open(&archive_id) {
                Ok(a) => a,
                Err(e) => break 'run (fail(&e), nothing_staged()),
            };
            if archive.needs_migration() {
                let e = ArchiveError::NeedsMigration {
                    found: archive.schema_version(),
                };
                archive.close();
                break 'run (fail(&e), nothing_staged());
            }
            if archive.import_blocked().is_some() {
                archive.close();
                break 'run (fail(&ArchiveError::NewerRules), nothing_staged());
            }
            let backup_fail = |e: oma_backup::BackupError| match e {
                oma_backup::BackupError::Cancelled => ImportEnding::Cancelled,
                e => ImportEnding::Failed(bm::to_ipc(&e, &limits)),
            };
            let session = match StagingRoot::new(&library, &inspection.root).and_then(|r| r.begin())
            {
                Ok(s) => s,
                Err(e) => {
                    archive.close();
                    break 'run (backup_fail(e), nothing_staged());
                }
            };
            let opened = {
                let mut fwd = Fwd(&throttle);
                let mut ctx = JobCtx::new(&cancel, &mut fwd);
                OpenBackup::open(inspection, session, password, &limits, &mut ctx)
            };
            let ob = match opened {
                Ok(ob) => ob,
                Err(OpenError { error, cleanup }) => {
                    archive.close();
                    break 'run (backup_fail(error), cleanup);
                }
            };
            let validated = {
                let mut fwd = Fwd(&throttle);
                let mut ctx = JobCtx::new(&cancel, &mut fwd);
                ob.validate(&mut ctx)
            };
            let ending = match validated {
                Err(e) => backup_fail(e),
                Ok(report) if !report.importable() => ImportEnding::NotValid,
                Ok(_) => {
                    let mut opts = ImportOptions::default();
                    opts.include_recently_deleted = options.include_recently_deleted;
                    opts.copy_attachments = options.copy_attachments;
                    opts.ios_version_untested = untested;
                    let mut shell_gate = ShellGate {
                        gate: &gate,
                        cancel: &cancel,
                        jobs,
                        job_id: &job_id,
                        throttle: &throttle,
                        ending: None,
                    };
                    let mut fwd = Fwd(&throttle);
                    let clock = SystemClock;
                    let mut ctx = ArchiveCtx {
                        cancel: &cancel,
                        progress: &mut fwd,
                        clock: &clock,
                    };
                    match archive.import(&ob, &opts, &mut shell_gate, &mut ctx) {
                        Ok(o) => ImportEnding::Imported(o, shell_gate.ending),
                        Err(ArchiveError::Cancelled) => ImportEnding::Cancelled,
                        Err(e) => fail(&e),
                    }
                }
            };
            let cleanup = ob.close();
            let closed = archive.close();
            tracing::debug!(checkpoint = ?closed.checkpoint, "archive closed");
            (ending, cleanup)
        }
    };

    let staging_removed = cleanup.removed;
    report_cleanup(&job_id, &cleanup);
    let event = import_event(&job_id, ending, staging_removed);
    tracing::info!(job_id = %job_id, event = event_kind(&event), "job finished");
    jobs.finish(
        &job_id,
        LastOutcome {
            report: None,
            extraction: None,
            cleanup: Some(CleanupDto { staging_removed }),
        },
    );
    sink.send(event);
}

fn report_cleanup(job_id: &str, cleanup: &CleanupReport) {
    if !cleanup.removed {
        tracing::warn!(
            job_id = %job_id,
            attempts = cleanup.attempts,
            last_error = ?cleanup.last_error,
            "staging not removed"
        );
    }
}

fn event_kind(e: &JobEvent) -> &'static str {
    match e {
        JobEvent::ImportFinished { outcome, .. } => match outcome {
            ImportOutcomeDto::Completed => "completed",
            ImportOutcomeDto::Stopped => "stopped",
            ImportOutcomeDto::CancelledAtGate => "cancelledAtGate",
            ImportOutcomeDto::NotImportable => "notImportable",
        },
        JobEvent::VerifyFinished { .. } => "verifyFinished",
        JobEvent::Failed { .. } => "failed",
        JobEvent::Cancelled { .. } => "cancelled",
        _ => "other",
    }
}

/// The terminal event of an import.
fn import_event(job_id: &str, ending: ImportEnding, staging_removed: bool) -> JobEvent {
    let job_id = job_id.to_owned();
    let cancelled = || JobEvent::Cancelled {
        job_id: job_id.clone(),
        staging_removed,
    };
    let failed = |error| JobEvent::Failed {
        job_id: job_id.clone(),
        error,
        staging_removed,
    };
    match ending {
        ImportEnding::Cancelled => cancelled(),
        ImportEnding::Failed(error) => failed(error),
        ImportEnding::NotValid => JobEvent::ImportFinished {
            job_id: job_id.clone(),
            outcome: ImportOutcomeDto::NotImportable,
            stop_reason: None,
            not_importable: Some(NotImportableDto::BackupNotImportable),
            gate: None,
            session: None,
            notice: Some(messages::backup_not_importable_notice()),
            staging_removed,
            plaintext_removed: None,
        },
        ImportEnding::Imported(outcome, gate) => match outcome {
            ImportOutcome::Completed(s) => JobEvent::ImportFinished {
                job_id: job_id.clone(),
                outcome: ImportOutcomeDto::Completed,
                stop_reason: None,
                not_importable: None,
                gate: None,
                session: Some(s.session),
                notice: None,
                staging_removed,
                plaintext_removed: Some(s.staging_removed),
            },
            ImportOutcome::Stopped { reason, summary } => JobEvent::ImportFinished {
                job_id: job_id.clone(),
                outcome: ImportOutcomeDto::Stopped,
                stop_reason: Some(reason.into()),
                not_importable: None,
                gate: None,
                session: Some(summary.session),
                notice: Some(messages::stop_notice(reason)),
                staging_removed,
                plaintext_removed: Some(summary.staging_removed),
            },
            ImportOutcome::Declined => JobEvent::ImportFinished {
                job_id: job_id.clone(),
                outcome: ImportOutcomeDto::CancelledAtGate,
                stop_reason: None,
                not_importable: None,
                gate: Some(match gate {
                    Some(GateEnding::Cancelled) => GateEndingDto::Cancelled,
                    Some(GateEnding::TimedOut) => GateEndingDto::TimedOut,
                    _ => GateEndingDto::Declined,
                }),
                session: None,
                notice: None,
                staging_removed,
                plaintext_removed: None,
            },
            ImportOutcome::NotImportable(n) => JobEvent::ImportFinished {
                job_id: job_id.clone(),
                outcome: ImportOutcomeDto::NotImportable,
                stop_reason: None,
                not_importable: Some(n.into()),
                gate: None,
                session: None,
                notice: Some(messages::not_importable_notice(n)),
                staging_removed,
                plaintext_removed: None,
            },
            // Refused before the gate: nothing started, so nothing to
            // continue (unlike a stop's INSUFFICIENT_SPACE).
            ImportOutcome::InsufficientSpace(estimate) => {
                failed(messages::start_space_notice(&estimate))
            }
            ImportOutcome::Cancelled(_) => cancelled(),
            // `ImportOutcome` is non-exhaustive.
            _ => failed(IpcError::internal("unknown import outcome")),
        },
    }
}

/// Run one verify to its end.
pub fn run_verify(spec: VerifySpec, jobs: &Jobs, sink: &mut dyn JobEventSink) {
    let VerifySpec {
        job_id,
        archive_id,
        library,
        depth,
        cancel,
        env,
    } = spec;
    tracing::info!(job_id = %job_id, kind = "verify", depth = ?depth, "job started");
    let result = {
        let throttle = RefCell::new(Throttle::new(&job_id, jobs, &mut *sink));
        if cancel.is_cancelled() {
            Err(None)
        } else {
            env.library(&library)
                .and_then(|lib| lib.open(&archive_id))
                .and_then(|mut archive| {
                    let mut fwd = Fwd(&throttle);
                    let clock = SystemClock;
                    let mut ctx = ArchiveCtx {
                        cancel: &cancel,
                        progress: &mut fwd,
                        clock: &clock,
                    };
                    let r = archive.verify(depth, &mut ctx);
                    archive.close();
                    r
                })
                .map_err(Some)
        }
    };
    let event = match result {
        Ok(report) => JobEvent::VerifyFinished {
            job_id: job_id.clone(),
            outcome: report.outcome.into(),
            recorded: report.run.is_some(),
        },
        Err(None) | Err(Some(ArchiveError::Cancelled)) => JobEvent::Cancelled {
            job_id: job_id.clone(),
            staging_removed: true,
        },
        Err(Some(e)) => JobEvent::Failed {
            job_id: job_id.clone(),
            error: messages::to_ipc(&e, &oma_backup::Limits::default()),
            staging_removed: true,
        },
    };
    tracing::info!(job_id = %job_id, event = event_kind(&event), "job finished");
    jobs.finish(&job_id, LastOutcome::default());
    sink.send(event);
}
