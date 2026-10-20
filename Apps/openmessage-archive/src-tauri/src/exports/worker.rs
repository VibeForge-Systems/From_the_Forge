//! The export job (M6 spec §9.1, D139): `start_export`'s checks and the
//! worker thread.
//!
//! `start_export` takes the draft (it is consumed whatever happens next),
//! checks the confirmations against it ([`prepare`]), compares the archive
//! generation and re-runs the destination checks, reserves the job slot on
//! the archive's subject and takes the archive's lock
//! (`Library::hold_for_export`); then [`run_export`] runs on its own
//! thread:
//!
//! 1. the leftovers the user asked to remove (exactly those listed when
//!    the folder was chosen, each re-checked);
//! 2. its own reader, one export snapshot under the lock: the generation
//!    again, the search scope re-run and re-checked (the query is dropped
//!    as soon as that returns), then `oma_export::pipeline::run_detailed`,
//!    whose pre-count stops the job if more messages qualify than the user
//!    confirmed;
//! 3. the lock is released, the slot freed, and exactly one terminal event
//!    sent: `exportFinished` (counts only), `cancelled` or `failed`, each
//!    saying whether the unfinished output was removed.
//!
//! **Previews (slice 6h2).** With previews on, the export starts its own
//! decode-helper pool ([`super::previews::HelperPool`], from
//! [`ExportSpec::renderers`]) and a feed the HTML writer reads; the pool
//! is shut down when the pipeline returns (its helpers killed on cancel or
//! failure), and only the byte count of the helpers' stderr is logged, as
//! a bucket.
//!
//! **PDF (slice 6j2).** With PDF in the plan, the fonts are read from the
//! app's resource folder and checked against their pins
//! ([`super::pdf_fonts::load`]) before anything is written (a missing or
//! altered font fails the export, `HelperFailure::Fonts`); the PDF writer
//! then starts one export-render helper ([`PdfSource`]), whose kill is
//! on the job's cancel path.
//!
//! Phases: `preparingExport`, `writingMessages` (timeline rows),
//! `finishingExport`, and `removingPartial` while a cancelled or failed
//! export's staging is removed (the job keeps its slot and the lock until
//! that ends).

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use oma_archive::{
    ArchiveGeneration, ArchiveId, CancelToken, ExportHold, PreviewOptions, SearchRan, SenderFilter,
    EXPORT_SEARCH_CAP,
};
use oma_export::helper::{HelperKill, HelperLaunch};
use oma_export::manifest::SearchFacts;
use oma_export::pipeline::{self, ExportFailure, ExportProgress, ExportReport, ExportStep};
use oma_export::preview::{PreviewFeed, RenditionPool};
use oma_export::writer::csv::CsvWriter;
use oma_export::writer::html::HtmlWriter;
use oma_export::writer::json::JsonWriter;
use oma_export::writer::pdf::{HelperRenderers, PdfRendererFactory, PdfWriter};
use oma_export::writer::txt::TxtWriter;
use oma_export::{
    EntryId, ExportError, ExportPlan, ExportWriter, ExportZone, Format, HelperFailure, LimitKind,
    Replace,
};

use super::destination::{self, Chosen, DestinationProbe};
use super::dto::{
    AttachmentExclusionsDto, ExportConfirmationsInput, ExportOutcomeDto, ExportSizeBucketDto,
    ExportSummaryDto,
};
use super::messages as em;
use super::previews::HelperPool;
use super::{Draft, DraftSearch, ExportHost};
use crate::archives::ArchiveEnv;
use crate::backups::dto::{clamp, JobEvent, JobPhase};
use crate::backups::worker::Throttle;
use crate::backups::{JobEventSink, Jobs, LastOutcome};
use crate::error::{ErrorCode, IpcError, IpcResult};
use crate::viewer::bucket;
use crate::viewer::media::helper::BackendFactory;

/// A draft that passed `start_export`'s checks.
pub struct Prepared {
    pub archive: ArchiveId,
    pub plan: ExportPlan,
    pub zone: ExportZone,
    pub chosen: Chosen,
    pub replace: Replace,
    pub search: Option<DraftSearch>,
    pub generation: ArchiveGeneration,
    pub remove_leftovers: bool,
    pub needs: destination::SpaceNeeds,
    /// The reveal counts' size, for the start log (a bucket).
    pub messages: u64,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("scope", &self.plan.scope().kind())
            .field("formats", &self.plan.formats())
            .finish_non_exhaustive()
    }
}

fn invalid(sentence: &str) -> IpcError {
    IpcError::new(ErrorCode::InvalidInput, sentence)
}

/// One reveal's confirmation: required exactly when the reveal is on, and
/// equal to the count the draft's preview made (never a number the
/// webview chose): a different one is `exportConfirmationStale`.
fn confirmed_count(on: bool, given: Option<u64>, counted: u64) -> IpcResult<Option<u64>> {
    match (on, given) {
        (false, None) => Ok(None),
        (true, Some(n)) if n == counted => Ok(Some(n)),
        (true, Some(_)) => Err(em::stale()),
        _ => Err(invalid(em::REVEAL_NOT_CONFIRMED)),
    }
}

/// Check `confirmations` against `draft` (taken from the slot): it must be
/// counted, have its zone and a destination; each reveal that is on needs
/// its own count, equal to the draft's, and a reveal that is off none;
/// kept extensions must be confirmed; every destination warning that
/// needs it must be confirmed; an existing single file is replaced only
/// when confirmed. The draft's preview count is stopped whatever happens
/// (it is consumed here).
pub fn prepare(draft: Draft, c: &ExportConfirmationsInput) -> IpcResult<Prepared> {
    draft.count.cancel();
    let Draft {
        archive,
        spec,
        search,
        zone,
        counted,
        destination: chosen,
        ..
    } = draft;
    let (Some(counted), Some(zone), Some(chosen)) = (counted, zone, chosen) else {
        return Err(invalid(em::NOT_PREPARED));
    };
    let r = spec.options.reveal;
    let (codes, earlier) = match (r.any(), c.reveal) {
        (false, None) => (None, None),
        (true, Some(given)) => (
            confirmed_count(
                r.one_time_codes,
                given.one_time_codes,
                counted.preview.messages_with_flagged_code,
            )?,
            confirmed_count(
                r.earlier_content,
                given.earlier_content,
                counted.preview.messages_with_earlier_content,
            )?,
        ),
        _ => return Err(invalid(em::REVEAL_NOT_CONFIRMED)),
    };
    if c.keep_risky_extension != spec.options.keep_extensions {
        return Err(invalid(em::EXTENSIONS_NOT_CONFIRMED));
    }
    let plan = spec.plan(codes, earlier).map_err(|e| em::to_ipc(&e))?;
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
    let needs = {
        let copies = plan.options().attachments == oma_export::AttachmentMode::CopyOriginals;
        let dto = super::dto::ExportPreviewDto::build(&counted.preview, &plan, None, None);
        destination::SpaceNeeds {
            copied_bytes: if copies {
                counted.preview.blob_bytes
            } else {
                0
            },
            largest_copied: if copies {
                counted.preview.largest_blob_bytes
            } else {
                0
            },
            estimate: dto.estimated_bytes,
        }
    };
    Ok(Prepared {
        archive,
        plan,
        zone,
        chosen,
        replace,
        search,
        generation: counted.preview.generation,
        remove_leftovers: c.remove_leftovers,
        needs,
        messages: counted.preview.distinct_messages,
    })
}

/// The checks `start_export` repeats on disk before it reserves the job:
/// the archive opens (damaged, older and newer ones are refused with
/// their codes), its generation is the draft's, and the destination
/// passes its checks again with the warnings the user confirmed.
pub fn recheck(
    p: &Prepared,
    env: &ArchiveEnv,
    library: &std::path::Path,
    confirmed: &[super::dto::ExportWarningDto],
    probe: &dyn DestinationProbe,
) -> IpcResult<()> {
    let reader = env
        .library(library)
        .and_then(|l| l.open_read(&p.archive))
        .map_err(|e| em::archive(&e))?;
    let generation = reader.archive_generation().map_err(|e| em::archive(&e))?;
    if generation != p.generation {
        return Err(em::stale());
    }
    destination::recheck(&p.chosen, Some(library), p.needs, confirmed, probe)
}

/// Everything the worker needs.
pub struct ExportSpec {
    pub job_id: String,
    pub prepared: Prepared,
    pub library: PathBuf,
    pub env: ArchiveEnv,
    pub hold: ExportHold,
    pub cancel: CancelToken,
    pub host: Arc<ExportHost>,
    /// The decode helpers' backends (the app's: this binary with
    /// `--oma-decode-helper`; tests give fakes).
    pub renderers: BackendFactory,
    /// Where the PDF writer's renderer comes from.
    pub pdf: PdfSource,
}

/// The PDF writer's renderer (slice 6j2).
pub enum PdfSource {
    /// The app: an export-render helper (`launch`; `None` when the
    /// binary's path is unknown), given the fonts read from `fonts` (the
    /// resource folder Tauri resolved; `None` when it could not).
    Helper {
        launch: Option<HelperLaunch>,
        fonts: Option<PathBuf>,
    },
    /// Tests: a renderer factory given directly.
    #[cfg(test)]
    Given(std::sync::Mutex<Option<Box<dyn PdfRendererFactory>>>),
}

impl std::fmt::Debug for PdfSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never a path.
        f.write_str("PdfSource")
    }
}

/// The helpers' kills, run by the job's cancel hook.
pub type KillSlot = Arc<Mutex<Vec<HelperKill>>>;

/// The PDF renderer factory for this export: the fonts are loaded and
/// checked first (`HelperFailure::Fonts` otherwise), so nothing unchecked
/// can reach a helper.
fn pdf_factory(
    spec: &ExportSpec,
    kills: &KillSlot,
    stderr: &mut Option<Arc<std::sync::atomic::AtomicU64>>,
) -> Result<Box<dyn PdfRendererFactory>, ExportError> {
    match &spec.pdf {
        PdfSource::Helper { launch, fonts } => {
            let fonts_err = || ExportError::HelperFailed(HelperFailure::Fonts);
            let dir = fonts.as_ref().ok_or_else(fonts_err)?;
            let fonts = super::pdf_fonts::load(dir).map_err(|_| fonts_err())?;
            let launch = launch
                .clone()
                .ok_or(ExportError::HelperFailed(HelperFailure::Spawn))?;
            let slot = Arc::clone(kills);
            let r = HelperRenderers::new(
                launch,
                fonts,
                spec.cancel.clone(),
                Box::new(move |k: HelperKill| {
                    if let Ok(mut v) = slot.lock() {
                        v.push(k);
                    }
                }),
            );
            *stderr = Some(r.stderr_counter());
            Ok(Box::new(r))
        }
        #[cfg(test)]
        PdfSource::Given(f) => f
            .lock()
            .ok()
            .and_then(|mut g| g.take())
            .ok_or(ExportError::HelperFailed(HelperFailure::Spawn)),
    }
}

/// One writer per format of the plan; HTML and PDF read previews from
/// `feed` when there is one, and PDF renders with `pdf`. XLSX never
/// reaches here (refused when the draft was made).
fn writers(
    plan: &ExportPlan,
    feed: Option<&PreviewFeed>,
    pdf: &mut Option<Box<dyn PdfRendererFactory>>,
) -> Result<Vec<Box<dyn ExportWriter>>, ExportError> {
    plan.formats()
        .iter()
        .map(|f| -> Result<Box<dyn ExportWriter>, ExportError> {
            Ok(match f {
                Format::Txt => Box::new(TxtWriter::new()),
                Format::Csv => Box::new(CsvWriter::new()),
                Format::Json => Box::new(JsonWriter::new()),
                Format::Html => match feed {
                    Some(feed) => Box::new(HtmlWriter::with_previews(feed.source())),
                    None => Box::new(HtmlWriter::new()),
                },
                Format::Pdf => {
                    let factory = pdf
                        .take()
                        .ok_or(ExportError::Limit(LimitKind::UnmappedVariant))?;
                    Box::new(PdfWriter::new(factory, feed.map(PreviewFeed::source)))
                }
                Format::Xlsx => return Err(ExportError::Limit(LimitKind::UnmappedVariant)),
            })
        })
        .collect()
}

/// The manifest's facts of a search scope (never the query, never the
/// excluded count or anything that gives it: see [`SearchFacts`]).
fn search_facts(s: &DraftSearch, sc: &oma_archive::SearchScope, ran: SearchRan) -> SearchFacts {
    let f = &s.request.filters;
    let mut kinds = Vec::new();
    if f.chat.is_some() {
        kinds.push("conversation");
    }
    if f.from_utc_ns.is_some() {
        kinds.push("from");
    }
    if f.until_utc_ns.is_some() {
        kinds.push("until");
    }
    if f.has_attachment {
        kinds.push("hasAttachment");
    }
    if f.sender != SenderFilter::Anyone {
        kinds.push("sender");
    }
    SearchFacts {
        mode: match ran {
            SearchRan::Words => "words",
            SearchRan::Exact => "exact",
            // `SearchRan` is non-exhaustive: a new mode is named, never
            // written as another.
            _ => "other",
        },
        filter_kinds: kinds,
        include_earlier_for_matching: s.request.include_earlier,
        matches_written: sc.kept,
        may_be_incomplete: SearchFacts::may_be_incomplete(
            sc.capped,
            sc.coverage_complete,
            sc.excluded.total(),
        ),
        all_matches: s.all_matches,
    }
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

fn phase(step: ExportStep) -> JobPhase {
    match step {
        ExportStep::Preparing => JobPhase::PreparingExport,
        ExportStep::Writing => JobPhase::WritingMessages,
        ExportStep::Finishing => JobPhase::FinishingExport,
        ExportStep::RemovingPartial => JobPhase::RemovingPartial,
    }
}

/// The export inside one snapshot. Returns the report and, for a search
/// scope, the hits left out.
fn export(
    spec: &ExportSpec,
    search: Option<DraftSearch>,
    kills: &KillSlot,
    progress: &mut dyn FnMut(ExportProgress),
) -> Result<(ExportReport, Option<u64>), ExportFailure> {
    let p = &spec.prepared;
    // The PDF fonts are checked before the archive is read or anything is
    // written.
    let mut pdf_stderr = None;
    let mut pdf = if p.plan.formats().contains(&Format::Pdf) {
        Some(pdf_factory(spec, kills, &mut pdf_stderr)?)
    } else {
        None
    };
    let reader = spec
        .env
        .library(&spec.library)
        .and_then(|l| l.open_read(&p.archive))?;
    reader.with_export_snapshot(&spec.hold, &spec.cancel, |snap| {
        progress(ExportProgress::default());
        if snap.generation()? != p.generation {
            return Err(ExportError::ConfirmationStale.into());
        }
        let (scope, facts, excluded) = match search {
            None => (
                pipeline::scope_ids(&p.plan, &p.zone)?
                    .ok_or(ExportError::Limit(LimitKind::ScopeMismatch))?,
                None,
                None,
            ),
            Some(s) => {
                let opts = PreviewOptions::new(p.plan.reveal(), p.plan.depth());
                let rerun;
                let set = if s.all_matches {
                    rerun = snap.search_ids(&s.request, EXPORT_SEARCH_CAP, &mut |_| {})?;
                    &rerun
                } else {
                    s.set.as_ref()
                };
                let sc = snap.search_scope(&s.request, set, opts)?;
                let facts = search_facts(&s, &sc, set.ran);
                let excluded = sc.excluded.total();
                // The query is not needed any more (§3.6).
                drop(s);
                (sc.into_scope(), Some(facts), Some(excluded))
            }
        };
        let export_id = EntryId::random()?;
        // The previews' pool and feed, started only now that the scope is
        // settled (§6.5).
        let pool = p.plan.options().previews.then(|| {
            Arc::new(HelperPool::start(
                &spec.renderers,
                super::previews::default_pool_size(),
            ))
        });
        let feed = pool.as_ref().map(|pool| {
            PreviewFeed::new(
                Arc::clone(pool) as Arc<dyn RenditionPool>,
                p.plan.options().larger_previews,
                spec.cancel.clone(),
            )
        });
        let run = pipeline::ExportRun {
            plan: &p.plan,
            zone: &p.zone,
            scope: &scope,
            export_id,
            created_utc_ns: now_ns(),
            app_version: env!("CARGO_PKG_VERSION"),
            parent: &p.chosen.parent,
            target: &p.chosen.target,
            replace: p.replace,
            attach: None,
            search: facts,
            previews: feed.clone(),
        };
        let result = match writers(&p.plan, feed.as_ref(), &mut pdf) {
            Ok(w) => pipeline::run_detailed(snap, &run, w, &spec.cancel, progress),
            Err(e) => Err(e.into()),
        };
        drop(run);
        drop(feed);
        if let Some(n) = pdf_stderr.as_ref() {
            let n = n.load(std::sync::atomic::Ordering::Relaxed);
            tracing::debug!(
                stderr_size = bucket(usize::try_from(n).unwrap_or(usize::MAX)),
                "export pdf helper"
            );
        }
        if let Some(pool) = pool.and_then(|p| Arc::try_unwrap(p).ok()) {
            let stderr = pool.shutdown(result.is_err());
            tracing::debug!(
                stderr_size = bucket(usize::try_from(stderr).unwrap_or(usize::MAX)),
                "export previews"
            );
        }
        Ok((result?, excluded))
    })
}

/// The summary of a published export, and whether it left anything out.
pub fn summary(
    report: &ExportReport,
    search_excluded: Option<u64>,
    bundle: bool,
) -> (ExportOutcomeDto, ExportSummaryDto) {
    let c = &report.counts;
    let mut ex = AttachmentExclusionsDto::default();
    for (token, n) in &c.attachments.excluded {
        let slot = match token.as_str() {
            "withheldUnsent" => &mut ex.withheld_unsent,
            "checksumMismatch" => &mut ex.checksum_mismatch,
            "tooLargeForVolume" => &mut ex.too_large_for_volume,
            "writeFailed" => &mut ex.write_failed,
            // Every card state: the archive holds no file for it.
            _ => &mut ex.no_stored_file,
        };
        *slot = slot.saturating_add(*n);
    }
    let mut clamped = false;
    let mut k = |v: u64| {
        let (v, cut) = clamp(v);
        clamped |= cut;
        v
    };
    let excluded_files = ex
        .withheld_unsent
        .saturating_add(ex.no_stored_file)
        .saturating_add(ex.checksum_mismatch)
        .saturating_add(ex.too_large_for_volume)
        .saturating_add(ex.write_failed);
    let ex = AttachmentExclusionsDto {
        withheld_unsent: k(ex.withheld_unsent),
        no_stored_file: k(ex.no_stored_file),
        checksum_mismatch: k(ex.checksum_mismatch),
        too_large_for_volume: k(ex.too_large_for_volume),
        write_failed: k(ex.write_failed),
    };
    let dto = ExportSummaryDto {
        items: k(c.items),
        folded_reaction_rows: k(c.folded_reaction_rows),
        conversations: k(c.conversations),
        files_written: k(report.output_files),
        attachments_copied: k(report.attachments.files_written),
        attachments_blocked: k(report.attachments.blocked),
        attachments_excluded: ex,
        one_time_codes_masked: k(c.one_time_codes_masked),
        earlier_text_withheld: k(c.earlier_text_withheld),
        edit_text_withheld: k(c.edit_text_withheld),
        alternative_text_withheld: k(c.alternative_text_withheld),
        hidden_characters_marked: k(c.hidden_characters_marked),
        controls_escaped: k(c.controls_escaped),
        neutralised_cells: k(c.neutralised_cells),
        times_outside_zone_window: k(c.times_outside_zone_window),
        undated_excluded: k(c.undated_excluded),
        search_hits_excluded: search_excluded.map(&mut k),
        bundle,
        size: ExportSizeBucketDto::of(report.output_bytes),
        clamped: false,
    };
    let left_out = excluded_files > 0 || c.undated_excluded > 0 || search_excluded.unwrap_or(0) > 0;
    let outcome = if left_out {
        ExportOutcomeDto::CompletedWithExclusions
    } else {
        ExportOutcomeDto::Completed
    };
    (outcome, ExportSummaryDto { clamped, ..dto })
}

fn event_kind(e: &JobEvent) -> &'static str {
    match e {
        JobEvent::ExportFinished {
            outcome: ExportOutcomeDto::Completed,
            ..
        } => "completed",
        JobEvent::ExportFinished { .. } => "completedWithExclusions",
        JobEvent::Failed { .. } => "failed",
        JobEvent::Cancelled { .. } => "cancelled",
        _ => "other",
    }
}

/// Run one export to its end, then free the lock and the slot and send
/// the terminal event.
pub fn run_export(spec: ExportSpec, jobs: &Jobs, sink: &mut dyn JobEventSink) {
    let started = Instant::now();
    let job_id = spec.job_id.clone();
    {
        let p = &spec.prepared;
        let o = p.plan.options();
        tracing::info!(
            job_id = %job_id,
            kind = "export",
            scope = p.plan.scope().kind(),
            formats = ?p.plan.formats(),
            bundle = p.plan.packaging() == oma_export::Packaging::Bundle,
            copies = o.attachments == oma_export::AttachmentMode::CopyOriginals,
            history = o.history,
            reveal_codes = o.reveal.one_time_codes,
            reveal_earlier = o.reveal.earlier_content,
            identifiers = o.identifiers == oma_export::IdentifierMode::ArchiveAndSource,
            messages = bucket(usize::try_from(p.messages).unwrap_or(usize::MAX)),
            "job started"
        );
    }
    let mut spec = spec;
    let search = spec.prepared.search.take();
    // A cancel kills the export-render helpers at once (slice 6i's hook).
    let kills: KillSlot = Arc::default();
    {
        let slot = Arc::clone(&kills);
        jobs.on_cancel(
            &job_id,
            Arc::new(move || {
                if let Ok(v) = slot.lock() {
                    for k in v.iter() {
                        k.kill();
                    }
                }
            }),
        );
    }
    let is_bundle = spec.prepared.plan.packaging() == oma_export::Packaging::Bundle;
    let result = {
        let throttle = RefCell::new(Throttle::new(&job_id, jobs, &mut *sink));
        throttle
            .borrow_mut()
            .forward(JobPhase::PreparingExport, 0, None);
        if spec.prepared.remove_leftovers && !spec.prepared.chosen.leftovers.is_empty() {
            let r = oma_export::remove_leftovers(
                &spec.prepared.chosen.parent,
                &spec.prepared.chosen.leftovers,
            );
            tracing::info!(
                removed = bucket(usize::try_from(r.removed).unwrap_or(usize::MAX)),
                skipped = bucket(usize::try_from(r.skipped).unwrap_or(usize::MAX)),
                failed = bucket(usize::try_from(r.failed).unwrap_or(usize::MAX)),
                "export leftovers"
            );
        }
        let mut forward = |p: ExportProgress| {
            let total = (p.step == ExportStep::Writing).then_some(p.total_rows);
            let done = if p.step == ExportStep::Writing {
                p.rows
            } else {
                0
            };
            throttle.borrow_mut().forward(phase(p.step), done, total);
        };
        if spec.cancel.is_cancelled() {
            Err(ExportError::Cancelled.into())
        } else {
            export(&spec, search, &kills, &mut forward)
        }
    };
    let ExportSpec {
        hold,
        host,
        prepared,
        ..
    } = spec;
    // The lock is held until the cleanup is over, then released before the
    // slot is freed (§9.1).
    drop(hold);
    let event = match result {
        Ok((report, excluded)) => {
            let (outcome, summary) = summary(&report, excluded, is_bundle);
            tracing::info!(
                job_id = %job_id,
                size = ?summary.size,
                items = bucket(usize::try_from(report.counts.items).unwrap_or(usize::MAX)),
                files = bucket(usize::try_from(report.output_files).unwrap_or(usize::MAX)),
                "export published"
            );
            host.remember_finished(&job_id, prepared.chosen.parent.clone());
            JobEvent::ExportFinished {
                job_id: job_id.clone(),
                outcome,
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
        event = event_kind(&event),
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "job finished"
    );
    jobs.finish(&job_id, LastOutcome::default());
    sink.send(event);
}
