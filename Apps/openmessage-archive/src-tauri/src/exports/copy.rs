//! "Save a copy" of one attachment (M6 spec §6.6, slice 6k2): the shell
//! side, without Tauri.
//!
//! - **The preview** ([`run_copy_preview`]) reads the attachment's facts in
//!   its own read transaction ([`oma_archive::ArchiveReader::attachment_copy_preview`]):
//!   its stored file, the kind its first 4 KiB sniff as and whether it is
//!   an attachment of an unsent part (owner Q4). An attachment with no
//!   readable stored file is `attachmentUnavailable`. The offered name is
//!   `oma_export::naming::copy_name` (the bundle's defusing rule without
//!   the ordinal) and stays in Rust: it reaches the webview only as the
//!   destination's `displayName`, after the save dialog.
//! - **The dialog** ([`dialog_plan`]) is a save dialog whose title names
//!   content captured before an unsend; the name the user keeps or types
//!   is checked by `destination::inspect` (refused when `sanitize_filename`
//!   would change it; `riskyName` when its extension could run programs).
//! - **Start** ([`prepare`]): an attachment of an unsent part needs
//!   `reveal.earlierContent: 1` (the one attachment), and nothing else
//!   does; a risky name needs `keepRiskyExtension`, and a safe one must
//!   not carry it; the destination's warnings and an existing file's
//!   replacement are confirmed as for an export.
//! - **The job** ([`run_copy`]) holds the archive's lock like an export,
//!   re-reads the facts in its snapshot (a changed archive or attachment
//!   is `exportConfirmationStale`) and copies through
//!   `oma_export::copy::save_copy`; it ends with `exportFinished` (one
//!   file, its size bucket), `cancelled` or `failed`.
//!
//! Logs carry kinds and outcomes only: never the name, an id or a size.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use oma_archive::{
    ArchiveError, ArchiveGeneration, ArchiveId, AttachmentId, CancelToken, ExportHold,
};
use oma_export::attach::VolumeRule;
use oma_export::copy::{save_copy, CopyProgress, CopyRequest, CopyStep};
use oma_export::pipeline::ExportFailure;
use oma_export::{ExportError, Replace};

use super::destination::{self, Chosen, DestinationProbe, Picked, SpaceNeeds};
use super::dto::{
    AttachmentExclusionsDto, CopyPreviewDto, DraftId, ExportConfirmationsInput, ExportOutcomeDto,
    ExportSizeBucketDto, ExportSummaryDto, PreviewEvent, RevealConfirmationInput,
};
use super::messages as em;
use super::{CopyDraft, CopyFacts, DialogPlan, Draft, DraftWork, ExportHost};
use crate::archives::ArchiveEnv;
use crate::backups::dto::{JobEvent, JobPhase};
use crate::backups::worker::Throttle;
use crate::backups::{JobEventSink, Jobs, LastOutcome};
use crate::error::{ErrorCode, IpcError, IpcResult};

/// Read the facts of a copy draft's attachment and send `copyReady` (the
/// facts stored in the draft first) or `failed`. A count stopped by a
/// discard or a replacement sends nothing.
#[allow(clippy::too_many_arguments)]
pub fn run_copy_preview(
    host: &ExportHost,
    env: &ArchiveEnv,
    library: &Path,
    draft: &DraftId,
    archive: &ArchiveId,
    attachment: AttachmentId,
    token: &CancelToken,
    emit: &mut dyn FnMut(PreviewEvent),
) {
    let read = env
        .library(library)
        .and_then(|l| l.open_read(archive))
        .and_then(|r| r.attachment_copy_preview(attachment, token));
    let failed = |code: ErrorCode| PreviewEvent::Failed {
        draft_id: draft.clone(),
        code,
    };
    let preview = match read {
        Ok(p) => p,
        Err(ArchiveError::Cancelled) => {
            tracing::info!(
                outcome = "cancelled",
                scope = "attachment",
                "export preview"
            );
            return;
        }
        Err(ArchiveError::AnchorNotFound) => {
            tracing::info!(
                outcome = "unavailable",
                scope = "attachment",
                "export preview"
            );
            emit(failed(ErrorCode::AttachmentUnavailable));
            return;
        }
        Err(e) => {
            let code = em::archive(&e).code;
            tracing::info!(outcome = ?code, scope = "attachment", "export preview");
            emit(failed(code));
            return;
        }
    };
    let (Some((_, file_bytes)), Some(_)) = (preview.copy.blob, preview.sniffed) else {
        // No stored file, or one that cannot be read.
        tracing::info!(
            outcome = "unavailable",
            scope = "attachment",
            "export preview"
        );
        emit(failed(ErrorCode::AttachmentUnavailable));
        return;
    };
    let offered = oma_export::naming::copy_name(
        preview.copy.name.text().map(|t| t.text.expose().as_str()),
        preview.sniffed,
    );
    let earlier_content = preview.copy.of_unsent_part;
    let stopped = token.is_cancelled();
    let stored = host.with_draft(draft, |d| match &mut d.work {
        DraftWork::Copy(c) => {
            c.facts = Some(CopyFacts {
                preview,
                offered: offered.name,
            });
            Ok(())
        }
        DraftWork::Export(_) => Err(IpcError::internal("not a copy draft")),
    });
    if let Err(e) = stored {
        if !stopped {
            tracing::info!(outcome = ?e.code, scope = "attachment", "export preview");
            emit(failed(e.code));
        }
        return;
    }
    tracing::info!(
        outcome = "ready",
        scope = "attachment",
        earlier_content,
        blocked = offered.blocked,
        "export preview"
    );
    emit(PreviewEvent::CopyReady {
        draft_id: draft.clone(),
        copy: CopyPreviewDto {
            earlier_content,
            file_bytes,
        },
    });
}

/// `choose_export_destination` for a copy draft: a save dialog offering
/// the defused name, titled with the draft's one sensitive fact. A copy
/// takes no zone table.
pub fn dialog_plan(
    c: &CopyDraft,
    zone: Option<&super::dto::ZoneTableInputDto>,
) -> IpcResult<DialogPlan> {
    let Some(facts) = &c.facts else {
        return Err(IpcError::new(ErrorCode::InvalidInput, em::NOT_PREPARED));
    };
    if zone.is_some() {
        return Err(IpcError::new(
            ErrorCode::InvalidInput,
            em::ZONE_TABLE_REFUSED,
        ));
    }
    Ok(DialogPlan {
        title: destination::copy_title(facts.preview.copy.of_unsent_part),
        picked: Picked::Copy(facts.preview.sniffed),
        default_name: facts.offered.clone(),
        needs: c.needs(),
    })
}

/// A copy draft that passed `start_export`'s checks.
pub struct PreparedCopy {
    pub archive: ArchiveId,
    pub attachment: AttachmentId,
    pub facts: CopyFacts,
    pub chosen: Chosen,
    pub replace: Replace,
    pub remove_leftovers: bool,
    pub needs: SpaceNeeds,
}

impl std::fmt::Debug for PreparedCopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedCopy")
            .field("facts", &self.facts)
            .field("chosen", &self.chosen)
            .finish_non_exhaustive()
    }
}

fn invalid(sentence: &str) -> IpcError {
    IpcError::new(ErrorCode::InvalidInput, sentence)
}

/// Check `c` against a copy `draft` (taken from the slot; its count is
/// stopped whatever happens): its facts and destination; the
/// earlier-content confirmation exactly when the attachment belongs to an
/// unsent part (`earlierContent: 1`, no code reveal); `keepRiskyExtension`
/// exactly when the name is risky; the destination's warnings; an
/// existing file replaced only when confirmed.
pub fn prepare(draft: Draft, c: &ExportConfirmationsInput) -> IpcResult<PreparedCopy> {
    draft.count.cancel();
    let Draft {
        archive,
        work,
        destination: chosen,
        ..
    } = draft;
    let DraftWork::Copy(copy) = work else {
        return Err(IpcError::internal("not a copy draft"));
    };
    let needs = copy.needs();
    let (Some(facts), Some(chosen)) = (copy.facts, chosen) else {
        return Err(invalid(em::NOT_PREPARED));
    };
    let wanted = facts
        .preview
        .copy
        .of_unsent_part
        .then_some(RevealConfirmationInput {
            one_time_codes: None,
            earlier_content: Some(1),
        });
    if c.reveal != wanted {
        return Err(invalid(em::REVEAL_NOT_CONFIRMED));
    }
    if c.keep_risky_extension != chosen.risky_name {
        return Err(invalid(em::RISKY_NAME_NOT_CONFIRMED));
    }
    let unconfirmed = chosen
        .warnings
        .iter()
        .any(|w| w.needs_confirmation() && !c.warnings.contains(w));
    if unconfirmed {
        return Err(invalid(em::WARNINGS_NOT_CONFIRMED));
    }
    let replace = match (chosen.target_exists, c.replace) {
        (true, true) => Replace::ConfirmedRegularFile,
        (true, false) => {
            return Err(IpcError::new(
                ErrorCode::ExportTargetExists,
                em::TARGET_EXISTS,
            ))
        }
        (false, _) => Replace::Never,
    };
    Ok(PreparedCopy {
        archive,
        attachment: copy.spec.attachment,
        facts,
        chosen,
        replace,
        remove_leftovers: c.remove_leftovers,
        needs,
    })
}

/// The checks `start_export` repeats on disk: the archive opens, its
/// generation is the preview's, and the destination passes again with the
/// warnings the user confirmed.
pub fn recheck(
    p: &PreparedCopy,
    env: &ArchiveEnv,
    library: &Path,
    confirmed: &[super::dto::ExportWarningDto],
    probe: &dyn DestinationProbe,
) -> IpcResult<()> {
    let reader = env
        .library(library)
        .and_then(|l| l.open_read(&p.archive))
        .map_err(|e| em::archive(&e))?;
    let generation: ArchiveGeneration = reader.archive_generation().map_err(|e| em::archive(&e))?;
    if generation != p.facts.preview.generation {
        return Err(em::stale());
    }
    destination::recheck(&p.chosen, Some(library), p.needs, confirmed, probe)
}

/// Everything the copy's worker needs.
pub struct CopyJob {
    pub job_id: String,
    pub prepared: PreparedCopy,
    pub library: PathBuf,
    pub env: ArchiveEnv,
    pub hold: ExportHold,
    pub cancel: CancelToken,
    pub host: Arc<ExportHost>,
}

fn phase(step: CopyStep) -> JobPhase {
    match step {
        CopyStep::Copying => JobPhase::CopyingAttachments,
        CopyStep::Finishing => JobPhase::FinishingExport,
        CopyStep::RemovingPartial => JobPhase::RemovingPartial,
    }
}

/// The copy inside one snapshot under the lock.
fn copy(
    job: &CopyJob,
    progress: &mut dyn FnMut(CopyProgress),
) -> Result<oma_export::copy::CopyReport, ExportFailure> {
    let p = &job.prepared;
    let reader = job
        .env
        .library(&job.library)
        .and_then(|l| l.open_read(&p.archive))?;
    reader.with_export_snapshot(&job.hold, &job.cancel, |snap| {
        if snap.generation()? != p.facts.preview.generation {
            return Err(ExportError::ConfirmationStale.into());
        }
        let now = snap.attachment_copy(p.attachment)?;
        if now != p.facts.preview.copy {
            return Err(ExportError::ConfirmationStale.into());
        }
        let (blob, expected_bytes) = now
            .blob
            .ok_or(ExportError::ConfirmationStale)
            .map_err(ExportFailure::from)?;
        let req = CopyRequest {
            blob,
            expected_bytes,
            sniffed: p.facts.preview.sniffed,
            parent: &p.chosen.parent,
            target: &p.chosen.target,
            replace: p.replace,
            volume: VolumeRule::of(&p.chosen.parent),
        };
        save_copy(snap, &req, &job.cancel, progress)
    })
}

/// The summary of a saved copy: one file, counts and codes only.
pub fn summary(report: &oma_export::copy::CopyReport, blocked: bool) -> ExportSummaryDto {
    ExportSummaryDto {
        items: 0,
        folded_reaction_rows: 0,
        conversations: 0,
        files_written: 1,
        attachments_copied: 1,
        attachments_blocked: u64::from(blocked),
        attachments_excluded: AttachmentExclusionsDto::default(),
        one_time_codes_masked: 0,
        earlier_text_withheld: 0,
        edit_text_withheld: 0,
        alternative_text_withheld: 0,
        hidden_characters_marked: 0,
        controls_escaped: 0,
        neutralised_cells: 0,
        times_outside_zone_window: 0,
        undated_excluded: 0,
        events_not_included: 0,
        motw_missing: report.mark_of_the_web == Some(false),
        search_hits_excluded: None,
        bundle: false,
        size: ExportSizeBucketDto::of(report.bytes),
        clamped: false,
    }
}

/// Run one copy to its end, then free the lock and the slot and send the
/// terminal event.
pub fn run_copy(job: CopyJob, jobs: &Jobs, sink: &mut dyn JobEventSink) {
    let started = Instant::now();
    let job_id = job.job_id.clone();
    tracing::info!(
        job_id = %job_id,
        kind = "export",
        scope = "attachment",
        earlier_content = job.prepared.facts.preview.copy.of_unsent_part,
        risky_name = job.prepared.chosen.risky_name,
        "job started"
    );
    let result = {
        let throttle = RefCell::new(Throttle::new(&job_id, jobs, &mut *sink));
        throttle
            .borrow_mut()
            .forward(JobPhase::PreparingExport, 0, None);
        let p = &job.prepared;
        if p.remove_leftovers && !p.chosen.leftovers.is_empty() {
            let r = oma_export::remove_leftovers(&p.chosen.parent, &p.chosen.leftovers);
            tracing::info!(
                removed = crate::viewer::bucket(usize::try_from(r.removed).unwrap_or(usize::MAX)),
                failed = crate::viewer::bucket(usize::try_from(r.failed).unwrap_or(usize::MAX)),
                "export leftovers"
            );
        }
        let mut forward = |c: CopyProgress| {
            // One file: 0 of 1 while it is copied, then the later phases.
            throttle.borrow_mut().forward(phase(c.step), 0, Some(1));
        };
        if job.cancel.is_cancelled() {
            Err(ExportError::Cancelled.into())
        } else {
            copy(&job, &mut forward)
        }
    };
    let CopyJob {
        hold,
        host,
        prepared,
        ..
    } = job;
    // The lock is released before the slot is freed (§9.1).
    drop(hold);
    let event = match result {
        Ok(report) => {
            let blocked = prepared
                .chosen
                .target
                .as_str()
                .to_ascii_lowercase()
                .ends_with(oma_export::naming::BLOCKED);
            let summary = summary(&report, blocked);
            tracing::info!(
                job_id = %job_id,
                size = ?summary.size,
                replaced = report.published.replaced,
                mark_of_the_web = ?report.mark_of_the_web,
                "export published"
            );
            host.remember_finished(&job_id, prepared.chosen.parent.clone());
            JobEvent::ExportFinished {
                job_id: job_id.clone(),
                outcome: ExportOutcomeDto::Completed,
                summary,
                staging_removed: true,
            }
        }
        Err(ExportFailure {
            error: ExportError::Cancelled,
            staging_removed,
        }) => JobEvent::Cancelled {
            job_id: job_id.clone(),
            staging_removed: staging_removed.unwrap_or(true),
        },
        Err(ExportFailure {
            error,
            staging_removed,
        }) => JobEvent::Failed {
            job_id: job_id.clone(),
            error: em::to_ipc(&error),
            staging_removed: staging_removed.unwrap_or(true),
        },
    };
    tracing::info!(
        job_id = %job_id,
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "job finished"
    );
    jobs.finish(&job_id, LastOutcome::default());
    sink.send(event);
}
