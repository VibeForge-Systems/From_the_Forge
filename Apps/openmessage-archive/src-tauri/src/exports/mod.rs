//! Exports: the shell side of milestone 6 (spec §9), slices 6h1 and 6h2.
//!
//! Nothing here depends on Tauri, so drafts, the destination checks, the
//! job and the DTOs are tested without a window; `commands::exports` adds
//! the native dialogs, the blocking pool and the channels.
//!
//! - **Drafts** ([`ExportHost`]): one slot; a new preview replaces it (and
//!   cancels its count). A draft holds the parsed plan, the archive
//!   generation and exact counts of its preview, the validated zone table
//!   and the chosen destination; it expires after [`DRAFT_TTL`] and
//!   `start_export` consumes it. `close_viewer` and `hide_viewer` (the
//!   privacy screen and idle hide) discard it.
//! - **Consent and drift** (D147): `start_export` compares the archive
//!   generation with the draft's, and the job recounts under the lock;
//!   either difference is `exportConfirmationStale` before anything is
//!   written.
//! - **The job** ([`worker`]): `JobKind::Export` on the archive's subject,
//!   holding the archive's lock (`Library::hold_for_export`) until its
//!   cleanup is over; its reader is its own, so damaged, older or newer
//!   archives are refused with their existing codes. It survives the
//!   viewer closing.
//! - **Destinations** ([`destination`], slice 6h2's token): each checked
//!   choice gets a random `DestinationToken`, kept in the draft;
//!   `start_export` must echo the latest one ([`ExportHost::take_for_start`]).
//! - **Previews** ([`previews`], slice 6h2): the job's own decode-helper
//!   pool behind `oma_export`'s preview feed.
//! - **The export-render helper** ([`render_helper`], slice 6i): the
//!   `--oma-export-helper` entry, how the app launches it, and its kill
//!   on the job's cancel path. Its first backend, PDF, exists from slice
//!   6j1 and is an export format from slice 6j2 ([`worker::PdfSource`]);
//!   XLSX stays refused at the draft with `exportUnavailable`.
//! - **The PDF fonts** ([`pdf_fonts`], slice 6j1): read from the resource
//!   folder and checked against their SHA-256 pins before any byte can be
//!   sent to the helper.
//! - **Logs** carry kinds, buckets and outcomes only (I6): never a path, a
//!   name, an id, a zone, a query, an exact count or content.

pub mod destination;
pub mod dto;
pub mod input;
pub mod messages;
pub mod pdf_fonts;
pub mod previews;
pub mod render_helper;
#[cfg(test)]
mod tests;
pub mod worker;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use oma_archive::{
    ArchiveError, ArchiveId, CancelToken, ExportPreview, ExportScopeIds, PreviewOptions,
    PreviewProgress, SearchRequest, SearchResultSet, SearchScope, EXPORT_SEARCH_CAP,
};
use oma_export::time::clamp_window;
use oma_export::{ExportPlan, ExportZone, ZoneTable};

use crate::archives::gate::{GateClock, MonotonicClock};
use crate::archives::ArchiveEnv;
use crate::error::{ErrorCode, IpcError, IpcResult};
use crate::viewer::ViewerHost;
use destination::{Chosen, SpaceNeeds};
use dto::{
    DraftId, ExportPreviewDto, PreviewEvent, PreviewExportRequest, SearchPreviewDto,
    ZoneTableInputDto,
};
use input::{DraftSpec, Source};
use messages as em;

/// How long a draft stays usable (spec §9.2).
pub const DRAFT_TTL: Duration = Duration::from_secs(30 * 60);

const DAY_MS: i64 = 86_400_000;
/// The largest zone offset, in ms: a local day's instants lie within it.
const MAX_OFFSET_MS: i64 = 18 * 3_600_000;

/// The live search a draft exports: its request (with the query, held
/// only until the job's re-run and re-check return) and its set.
pub struct DraftSearch {
    pub request: SearchRequest,
    pub set: Arc<SearchResultSet>,
    pub all_matches: bool,
}

impl std::fmt::Debug for DraftSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DraftSearch")
            .field("hits", &self.set.entries.len())
            .field("all_matches", &self.all_matches)
            .finish_non_exhaustive()
    }
}

/// A preview's results.
#[derive(Debug, Clone)]
pub struct Counted {
    pub preview: ExportPreview,
    pub search: Option<SearchPreviewDto>,
}

/// A prepared export (spec §9.2).
#[derive(Debug)]
pub struct Draft {
    pub id: DraftId,
    created: Duration,
    pub archive: ArchiveId,
    pub spec: DraftSpec,
    pub search: Option<DraftSearch>,
    /// `None` until a table arrives (any zone but UTC).
    pub zone: Option<ExportZone>,
    pub counted: Option<Counted>,
    /// Stops the preview's count when the draft is replaced or discarded.
    pub count: CancelToken,
    pub destination: Option<Chosen>,
}

impl Draft {
    /// What the space checks need: the files copied, and the estimate.
    pub fn needs(&self, plan: &ExportPlan) -> SpaceNeeds {
        let Some(c) = &self.counted else {
            return SpaceNeeds {
                copied_bytes: 0,
                largest_copied: 0,
                estimate: 0,
            };
        };
        let copies = plan.options().attachments == oma_export::AttachmentMode::CopyOriginals;
        let dto = ExportPreviewDto::build(&c.preview, plan, None, None);
        SpaceNeeds {
            copied_bytes: if copies { c.preview.blob_bytes } else { 0 },
            largest_copied: if copies {
                c.preview.largest_blob_bytes
            } else {
                0
            },
            estimate: dto.estimated_bytes,
        }
    }
}

/// The export state of the app: the draft slot, and the folders of
/// finished exports for "Show in folder" (in memory only, dropped on
/// exit).
pub struct ExportHost {
    slot: Mutex<Option<Draft>>,
    finished: Mutex<HashMap<String, PathBuf>>,
    clock: Arc<dyn GateClock>,
}

impl std::fmt::Debug for ExportHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExportHost").finish_non_exhaustive()
    }
}

impl Default for ExportHost {
    fn default() -> Self {
        ExportHost::with_clock(Arc::new(MonotonicClock::default()))
    }
}

fn poisoned() -> IpcError {
    IpcError::internal("export lock poisoned")
}

impl ExportHost {
    /// A host whose drafts expire by `clock` (tests).
    pub fn with_clock(clock: Arc<dyn GateClock>) -> ExportHost {
        ExportHost {
            slot: Mutex::new(None),
            finished: Mutex::new(HashMap::new()),
            clock,
        }
    }

    fn lock(&self) -> IpcResult<MutexGuard<'_, Option<Draft>>> {
        self.slot.lock().map_err(|_| poisoned())
    }

    /// Put `draft` in the slot, cancelling the count of the one it
    /// replaces.
    fn install(&self, draft: Draft) -> IpcResult<()> {
        let old = self.lock()?.replace(draft);
        if let Some(old) = old {
            old.count.cancel();
        }
        Ok(())
    }

    /// Run `f` on draft `id`, or `exportDraftExpired` (unknown, replaced,
    /// discarded or older than [`DRAFT_TTL`], which is then dropped).
    pub fn with_draft<T>(
        &self,
        id: &DraftId,
        f: impl FnOnce(&mut Draft) -> IpcResult<T>,
    ) -> IpcResult<T> {
        let mut slot = self.lock()?;
        let now = self.clock.now();
        match slot.as_mut() {
            Some(d) if d.id == *id => {
                if now.saturating_sub(d.created) >= DRAFT_TTL {
                    d.count.cancel();
                    *slot = None;
                    return Err(em::expired());
                }
                f(d)
            }
            _ => Err(em::expired()),
        }
    }

    /// Take draft `id` out of the slot: the id and age are checked and the
    /// draft taken under one guard, so a draft installed meanwhile (a new
    /// preview on another thread) is never the one taken. An expired draft
    /// is dropped with its count stopped.
    pub fn take(&self, id: &DraftId) -> IpcResult<Draft> {
        let now = self.clock.now();
        let taken = {
            let mut slot = self.lock()?;
            match slot.as_ref() {
                Some(d) if d.id == *id => slot.take(),
                _ => None,
            }
        };
        let Some(d) = taken else {
            return Err(em::expired());
        };
        if now.saturating_sub(d.created) >= DRAFT_TTL {
            d.count.cancel();
            return Err(em::expired());
        }
        Ok(d)
    }

    /// `start_export`: take draft `id` (consumed whatever follows) and
    /// check that `token` is its latest destination's. A token of an
    /// earlier choice, or none chosen, is `exportConfirmationStale`, and
    /// the draft's count is stopped.
    pub fn take_for_start(&self, id: &DraftId, token: &dto::DestinationToken) -> IpcResult<Draft> {
        let draft = self.take(id)?;
        match &draft.destination {
            Some(c) if c.token == *token => Ok(draft),
            _ => {
                draft.count.cancel();
                Err(em::stale())
            }
        }
    }

    /// Drop draft `id` and stop its count. False when it is not the
    /// current draft.
    pub fn discard(&self, id: &DraftId) -> bool {
        let taken = match self.slot.lock() {
            Ok(mut s) if s.as_ref().is_some_and(|d| d.id == *id) => s.take(),
            _ => None,
        };
        taken.map(|d| d.count.cancel()).is_some()
    }

    /// Drop whatever draft there is (the privacy controls, D114/§9.2).
    pub fn discard_any(&self) {
        let taken = self.slot.lock().ok().and_then(|mut s| s.take());
        if let Some(d) = taken {
            d.count.cancel();
            tracing::info!(outcome = "discarded", "export draft");
        }
    }

    /// A draft is held.
    pub fn has_draft(&self) -> bool {
        self.slot.lock().map(|s| s.is_some()).unwrap_or(false)
    }

    /// Remember where job `job_id` published (its canonical folder).
    pub fn remember_finished(&self, job_id: &str, folder: PathBuf) {
        if let Ok(mut m) = self.finished.lock() {
            m.insert(job_id.to_owned(), folder);
        }
    }

    /// The folder of a finished export, if this run of the app made it.
    pub fn finished_folder(&self, job_id: &str) -> Option<PathBuf> {
        self.finished.lock().ok()?.get(job_id).cloned()
    }

    fn now(&self) -> Duration {
        self.clock.now()
    }
}

// ------------------------------------------------------------------ zones

/// `[lo, hi]` widened to cover `now` ± a day, then clamped to the zone
/// window's limits (1900 to now + 1 year, §4.6).
fn window_with_now(lo: i64, hi: i64, now_ms: i64) -> (i64, i64) {
    let (min, max) = clamp_window(now_ms);
    let lo = lo.min(now_ms.saturating_sub(DAY_MS)).clamp(min, max);
    let hi = hi.max(now_ms.saturating_add(DAY_MS)).clamp(min, max);
    (lo, hi)
}

/// The window a zone table must cover for a dated span (or none).
pub fn span_window(span: Option<(i64, i64)>, now_ms: i64) -> (i64, i64) {
    let (lo, hi) = span.unwrap_or((now_ms, now_ms));
    window_with_now(lo, hi, now_ms)
}

/// The window a zone table must cover for local dates `[from, until)`:
/// their UTC days widened by the largest offset.
pub fn date_window(spec: &DraftSpec, now_ms: i64) -> Option<(i64, i64)> {
    let (from, until) = spec.date_range()?;
    let lo = from
        .days_from_epoch()
        .saturating_mul(DAY_MS)
        .saturating_sub(MAX_OFFSET_MS);
    let hi = until
        .days_from_epoch()
        .saturating_mul(DAY_MS)
        .saturating_add(MAX_OFFSET_MS);
    Some(window_with_now(lo, hi, now_ms))
}

/// Validate the webview's table for `window`.
pub fn zone_table(z: &ZoneTableInputDto, window: (i64, i64), now_ms: i64) -> IpcResult<ExportZone> {
    ZoneTable::new(input::zone_input(z), Some(window), now_ms)
        .map(ExportZone::Table)
        .map_err(|e| em::to_ipc(&e))
}

// ---------------------------------------------------------------- preview

/// What a preview's count runs over.
pub enum CountWhat {
    Scope(ExportScopeIds),
    Search {
        request: SearchRequest,
        set: Arc<SearchResultSet>,
        rerun: bool,
    },
    /// A date range whose zone table has not arrived: nothing to count
    /// yet; the webview builds the table for this window.
    NeedsZone {
        window: (i64, i64),
    },
}

/// A count to run on the blocking pool.
pub struct CountWork {
    pub draft: DraftId,
    pub archive: ArchiveId,
    pub plan: ExportPlan,
    pub opts: PreviewOptions,
    pub what: CountWhat,
    pub token: CancelToken,
    /// The draft has its zone (UTC, or a date range's table).
    pub zone_set: bool,
}

/// `preview_export`, the synchronous part: parse and validate the
/// request, take the viewer's live set for a search scope, and install a
/// new draft (replacing the old one and stopping its count). The count
/// itself is [`run_count`].
pub fn begin_preview(
    host: &ExportHost,
    viewer: &ViewerHost,
    req: &PreviewExportRequest,
    now_ms: i64,
) -> IpcResult<CountWork> {
    let spec = input::parse(req)?;
    let plan = spec.probe_plan()?;
    let (archive, search) = match &spec.source {
        Source::Archive(id) => (id.clone(), None),
        Source::Search {
            viewer: generation,
            search_id,
            all_matches,
        } => {
            let s = viewer.get(*generation)?;
            let slot = s.searches()?;
            let Some(live) = slot.current.as_ref().filter(|l| l.id == *search_id) else {
                return Err(crate::viewer::messages::superseded());
            };
            (
                s.archive.clone(),
                Some(DraftSearch {
                    request: live.request.clone(),
                    set: Arc::clone(&live.set),
                    all_matches: *all_matches,
                }),
            )
        }
    };
    // The zone: UTC needs no table; a date range needs its table before
    // anything is counted; every other scope gets it with the
    // destination, once the span is known.
    let date_window = date_window(&spec, now_ms);
    let zone = match (&req.zone, spec.utc(), date_window) {
        (None, true, _) => Some(ExportZone::Utc),
        (Some(z), false, Some(w)) => Some(zone_table(z, w, now_ms)?),
        (None, false, _) => None,
        (Some(_), _, _) => {
            return Err(IpcError::new(
                ErrorCode::InvalidInput,
                em::ZONE_TABLE_REFUSED,
            ))
        }
    };
    let opts = PreviewOptions::new(plan.reveal(), plan.depth());
    let what = match (&search, &zone, date_window) {
        (Some(s), _, _) => CountWhat::Search {
            request: s.request.clone(),
            set: Arc::clone(&s.set),
            rerun: s.all_matches,
        },
        (None, None, Some(window)) => CountWhat::NeedsZone { window },
        (None, z, _) => {
            // Only a date range reads the zone, and it has one here.
            let zone = z.clone().unwrap_or(ExportZone::Utc);
            let ids = oma_export::pipeline::scope_ids(&plan, &zone)
                .map_err(|e| em::to_ipc(&e))?
                .ok_or_else(|| IpcError::internal("scope without ids"))?;
            CountWhat::Scope(ids)
        }
    };
    let id = DraftId::random().ok_or_else(|| IpcError::internal("draft id"))?;
    let token = CancelToken::new();
    let zone_set = zone.is_some();
    host.install(Draft {
        id: id.clone(),
        created: host.now(),
        archive: archive.clone(),
        spec,
        search,
        zone,
        counted: None,
        count: token.clone(),
        destination: None,
    })?;
    tracing::info!(
        scope = plan.scope().kind(),
        formats = plan.formats().len(),
        reveal = plan.options().reveal.any(),
        "export draft"
    );
    Ok(CountWork {
        draft: id,
        archive,
        plan,
        opts,
        what,
        token,
        zone_set,
    })
}

/// Rows between two `progress` events.
const PROGRESS_ROWS: u64 = 4_096;

/// Count a draft's scope exactly (spec §3.7) and send the outcome:
/// `ready` (stored in the draft first), `needsZoneTable` or `failed`. A
/// count stopped by a discard or a replacement sends nothing; a count that
/// finished after its draft expired (or was taken by `start_export`, which
/// stops it) sends `failed` with `exportDraftExpired`, so the webview never
/// waits for an answer that cannot come.
pub fn run_count(
    host: &ExportHost,
    env: &ArchiveEnv,
    library: &Path,
    work: CountWork,
    now_ms: i64,
    emit: &mut dyn FnMut(PreviewEvent),
) {
    let CountWork {
        draft,
        archive,
        plan,
        opts,
        what,
        token,
        zone_set,
    } = work;
    if let CountWhat::NeedsZone { window } = what {
        emit(PreviewEvent::NeedsZoneTable {
            draft_id: draft,
            span_utc_ms: None,
            window_utc_ms: [window.0, window.1],
        });
        return;
    }
    let started = std::time::Instant::now();
    let result = {
        let mut last = 0u64;
        let id = draft.clone();
        let mut progress = |p: PreviewProgress| {
            if p.rows >= last.saturating_add(PROGRESS_ROWS) {
                last = p.rows;
                emit(PreviewEvent::Progress {
                    draft_id: id.clone(),
                    rows: crate::backups::dto::clamp(p.rows).0,
                });
            }
        };
        count(env, library, &archive, opts, what, &token, &mut progress)
    };
    match result {
        Ok((preview, search)) => {
            let span = preview.span_utc_ms;
            let counted = Counted { preview, search };
            // A discard or a new draft stopped this count first: nobody
            // waits for it.
            let stopped = token.is_cancelled();
            let stored = host.with_draft(&draft, |d| {
                d.counted = Some(counted);
                Ok(())
            });
            if let Err(e) = stored {
                if !stopped {
                    tracing::info!(outcome = ?e.code, "export preview");
                    emit(PreviewEvent::Failed {
                        draft_id: draft,
                        code: e.code,
                    });
                }
                return;
            }
            let window = (!zone_set).then(|| span_window(span, now_ms));
            tracing::info!(
                outcome = "ready",
                rows = crate::viewer::bucket(
                    usize::try_from(preview.timeline_rows).unwrap_or(usize::MAX)
                ),
                elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                "export preview"
            );
            emit(PreviewEvent::Ready {
                draft_id: draft,
                preview: ExportPreviewDto::build(&preview, &plan, window, search),
            });
        }
        Err(ArchiveError::Cancelled) => {
            tracing::info!(outcome = "cancelled", "export preview");
        }
        Err(e) => {
            let code = em::archive(&e).code;
            tracing::info!(outcome = ?code, "export preview");
            emit(PreviewEvent::Failed {
                draft_id: draft,
                code,
            });
        }
    }
}

fn search_dto(sc: &SearchScope, all_matches: bool) -> SearchPreviewDto {
    SearchPreviewDto {
        kept: crate::backups::dto::clamp(sc.kept).0,
        excluded: sc.excluded.into(),
        capped: sc.capped,
        newest_kept: sc.newest_kept,
        coverage_complete: sc.coverage_complete,
        all_matches,
    }
}

fn count(
    env: &ArchiveEnv,
    library: &Path,
    archive: &ArchiveId,
    opts: PreviewOptions,
    what: CountWhat,
    token: &CancelToken,
    progress: &mut dyn FnMut(PreviewProgress),
) -> Result<(ExportPreview, Option<SearchPreviewDto>), ArchiveError> {
    let reader = env.library(library)?.open_read(archive)?;
    match what {
        CountWhat::Scope(ids) => reader
            .export_preview(&ids, opts, token, progress)
            .map(|p| (p, None)),
        CountWhat::Search {
            request,
            set,
            rerun,
        } => {
            let cap = rerun.then_some(EXPORT_SEARCH_CAP);
            let (sc, p) =
                reader.export_search_preview(&request, &set, cap, opts, token, progress)?;
            Ok((p, Some(search_dto(&sc, rerun))))
        }
        CountWhat::NeedsZone { .. } => Err(ArchiveError::Internal("nothing to count")),
    }
}

// ------------------------------------------------------------ destination

/// What `choose_export_destination` opens the dialog with.
#[derive(Debug, Clone)]
pub struct DialogPlan {
    pub title: String,
    pub picked: destination::Picked,
    /// The bundle's base name, or the single file's default name.
    pub default_name: String,
    pub needs: SpaceNeeds,
}

/// `choose_export_destination` before the dialog: the draft must be
/// counted; its zone table, when one is needed, arrives now and is
/// validated against the counted span (and now, for the default name).
pub fn prepare_destination(
    host: &ExportHost,
    id: &DraftId,
    zone: Option<&ZoneTableInputDto>,
    now_ms: i64,
) -> IpcResult<DialogPlan> {
    host.with_draft(id, |d| {
        let Some(counted) = &d.counted else {
            return Err(IpcError::new(ErrorCode::InvalidInput, em::NOT_PREPARED));
        };
        match (&d.zone, zone) {
            (None, Some(z)) => {
                let window = span_window(counted.preview.span_utc_ms, now_ms);
                d.zone = Some(zone_table(z, window, now_ms)?);
            }
            (None, None) => {
                return Err(IpcError::new(
                    ErrorCode::InvalidInput,
                    em::ZONE_TABLE_NEEDED,
                ))
            }
            (Some(_), Some(_)) => {
                return Err(IpcError::new(
                    ErrorCode::InvalidInput,
                    em::ZONE_TABLE_REFUSED,
                ))
            }
            (Some(_), None) => {}
        }
        let plan = d.spec.probe_plan()?;
        let zone = d.zone.clone().unwrap_or(ExportZone::Utc);
        let base = destination::default_name(&zone, now_ms.saturating_mul(1_000_000));
        let picked = match destination::single_format(&plan) {
            Some(f) => destination::Picked::File(f),
            None => destination::Picked::Folder,
        };
        let default_name = match picked {
            destination::Picked::File(f) => format!("{base}.{}", f.extension()),
            destination::Picked::Folder => base,
        };
        Ok(DialogPlan {
            title: destination::dialog_title(&plan),
            picked,
            default_name,
            needs: d.needs(&plan),
        })
    })
}

/// `choose_export_destination` after the dialog: check the location and
/// keep it in the draft (a later choice replaces it).
pub fn attach_destination(
    host: &ExportHost,
    id: &DraftId,
    picked_path: &Path,
    plan: &DialogPlan,
    library: Option<&Path>,
    probe: &dyn destination::DestinationProbe,
) -> IpcResult<dto::ExportDestinationDto> {
    let bundle_name = match plan.picked {
        destination::Picked::Folder => plan.default_name.as_str(),
        destination::Picked::File(_) => "",
    };
    let chosen = destination::inspect(
        picked_path,
        plan.picked,
        bundle_name,
        library,
        plan.needs,
        probe,
    );
    host.with_draft(id, |d| {
        let chosen = chosen?;
        let dto = chosen.dto.clone();
        tracing::info!(
            outcome = "chosen",
            warnings = chosen.warnings.len(),
            target_exists = chosen.target_exists,
            leftovers = crate::viewer::bucket(chosen.leftovers.len()),
            "export destination"
        );
        d.destination = Some(chosen);
        Ok(dto)
    })
}
