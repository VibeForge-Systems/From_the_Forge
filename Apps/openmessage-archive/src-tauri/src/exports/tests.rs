//! Exports end to end without Tauri (M6 spec §14.5, slice 6h1), on
//! archives imported from the synthetic viewer catalogue: input
//! validation, drafts and their discarding, zones, the destination
//! checks, start's confirmations and drift checks, the job (lock, busy,
//! phases, cancel, the recount), the DTO guards and the command wiring.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oma_archive::{CancelToken, LineageChoice};
use serde_json::{json, Value};
use ts_rs::{Config, TS};

use super::destination::{self, DestinationProbe, Picked, SpaceNeeds};
use super::dto::*;
use super::worker::{self, ExportSpec};
use super::*;
use crate::archives::gate::GateClock;
use crate::backups::dto::{JobEvent, JobKind, JobPhase, MAX_SAFE_INTEGER};
use crate::backups::{JobEventSink, JobSubject, Jobs};
use crate::commands::AppState;
use crate::settings::SettingsStore;
use crate::ts_guard;
use crate::viewer::input::parse_raw;
use crate::viewer::tests::{env, id_of, import_entry, synthetic_archive};

const SENTINEL: &str = "synthetic-export-input-sentinel";
/// 2026-10-19T00:00:00Z.
const NOW_MS: i64 = 1_792_368_000_000;

// ---------------------------------------------------------------- helpers

/// A clock the test moves.
#[derive(Default)]
struct FakeClock(AtomicU64);

impl GateClock for FakeClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.load(Ordering::SeqCst))
    }
}

/// A file system with 1 TiB free, local, private, or as a test sets it.
#[derive(Clone)]
struct FakeProbe {
    volume: oma_fs::VolumeKind,
    available: Option<u64>,
    shared: bool,
}

impl Default for FakeProbe {
    fn default() -> Self {
        FakeProbe {
            volume: oma_fs::VolumeKind::Local,
            available: Some(1 << 40),
            shared: false,
        }
    }
}

impl DestinationProbe for FakeProbe {
    fn volume(&self, _: &Path) -> std::io::Result<oma_fs::VolumeKind> {
        Ok(self.volume)
    }
    fn available(&self, _: &Path) -> std::io::Result<u64> {
        self.available
            .ok_or_else(|| std::io::Error::other("unknown"))
    }
    fn shared(&self, _: &Path) -> bool {
        self.shared
    }
}

/// Events a job sent, optionally cancelling at the first rows written.
struct Rec {
    events: Arc<Mutex<Vec<JobEvent>>>,
    cancel_on_rows: Option<CancelToken>,
}

impl JobEventSink for Rec {
    fn send(&mut self, event: JobEvent) {
        if let (Some(t), JobEvent::Progress { phase, .. }) = (&self.cancel_on_rows, &event) {
            if *phase == JobPhase::WritingMessages {
                t.cancel();
            }
        }
        self.events.lock().expect("events").push(event);
    }
}

/// A library with the main viewer archive, and an empty destination.
struct Fx {
    _tmp: tempfile::TempDir,
    _keep: tempfile::TempDir,
    library: PathBuf,
    dest: PathBuf,
    id: oma_archive::ArchiveId,
}

fn fx() -> Fx {
    fx_of(None)
}

/// [`fx`] with viewer catalogue entry `entry` instead of the main one.
fn fx_of(entry: Option<&str>) -> Fx {
    let tmp = tempfile::tempdir().expect("tempdir");
    let library = tmp.path().join("lib");
    let dest = tmp.path().join("out");
    std::fs::create_dir(&library).expect("lib");
    std::fs::create_dir(&dest).expect("out");
    let (keep, id) = match entry {
        None => synthetic_archive(&library),
        Some(e) => (
            tempfile::tempdir().expect("tempdir"),
            crate::viewer::tests::import_order(&library, e),
        ),
    };
    let dest = dunce::canonicalize(&dest).expect("canonical");
    Fx {
        _tmp: tmp,
        _keep: keep,
        library,
        dest,
        id,
    }
}

fn options(over: Value) -> Value {
    let mut o = json!({
        "attachments": "none", "keepExtensions": false, "previews": false,
        "largerPreviews": false, "history": false, "reactions": true, "events": true,
        "revealOneTimeCodes": false, "revealEarlierContent": false,
        "hiddenCharacterMarkers": true, "seconds": true, "includeIdentifiers": false,
        "clock": "h23", "lineEndings": "lf", "zone": "utc",
    });
    if let (Some(o), Some(over)) = (o.as_object_mut(), over.as_object()) {
        for (k, v) in over {
            o.insert(k.clone(), v.clone());
        }
    }
    o
}

fn body(request: Value) -> Value {
    json!({ "request": request })
}

fn preview_req(scope: Value, formats: Value, over: Value) -> IpcResult<PreviewExportRequest> {
    parse_raw(Some(&body(json!({
        "scope": scope, "formats": formats, "options": options(over),
    }))))
}

fn archive_scope(f: &Fx) -> Value {
    json!({ "kind": "archive", "archiveId": f.id.as_str() })
}

/// Make a draft and count it; the events the count sent.
fn counted(host: &ExportHost, f: &Fx, req: &PreviewExportRequest) -> (DraftId, Vec<PreviewEvent>) {
    let viewer = crate::viewer::ViewerHost::default();
    let work = begin_preview(host, &viewer, req, NOW_MS).expect("draft");
    let id = work.draft.clone();
    let mut events = Vec::new();
    run_count(host, &env(), &f.library, work, NOW_MS, &mut |e| {
        events.push(e)
    });
    (id, events)
}

fn ready(events: &[PreviewEvent]) -> ExportPreviewDto {
    match events.last() {
        Some(PreviewEvent::Ready { preview, .. }) => preview.clone(),
        other => panic!("not ready: {other:?}"),
    }
}

/// A draft counted, with its destination in `f.dest`.
fn chosen(host: &ExportHost, f: &Fx, req: &PreviewExportRequest) -> (DraftId, ExportPreviewDto) {
    let (id, events) = counted(host, f, req);
    let preview = ready(&events);
    let plan = prepare_destination(host, &id, None, NOW_MS).expect("dialog plan");
    let picked = match plan.picked {
        Picked::Folder => f.dest.clone(),
        Picked::File(_) => f.dest.join("chat.txt"),
    };
    attach_destination(
        host,
        &id,
        &picked,
        &plan,
        Some(&f.library),
        &FakeProbe::default(),
    )
    .expect("destination");
    (id, preview)
}

fn confirmations(over: Value) -> ExportConfirmationsInput {
    let mut c = json!({
        "warnings": [], "replace": false, "reveal": null,
        "keepRiskyExtension": false, "removeLeftovers": false,
    });
    if let (Some(c), Some(over)) = (c.as_object_mut(), over.as_object()) {
        for (k, v) in over {
            c.insert(k.clone(), v.clone());
        }
    }
    id_of(c)
}

/// Run a prepared draft as `start_export` would, synchronously.
fn run_job(
    host: &Arc<ExportHost>,
    jobs: &Jobs,
    f: &Fx,
    id: &DraftId,
    c: ExportConfirmationsInput,
    cancel_on_rows: bool,
) -> IpcResult<Vec<JobEvent>> {
    run_job_with(host, jobs, f, id, c, cancel_on_rows, fake_renderers(), None)
}

/// A hook that gets the job's cancel token before the worker runs.
type Canceller = Box<dyn FnOnce(CancelToken) + Send>;

/// [`run_job`] with the decode helpers' backends and a canceller.
#[allow(clippy::too_many_arguments)]
fn run_job_with(
    host: &Arc<ExportHost>,
    jobs: &Jobs,
    f: &Fx,
    id: &DraftId,
    c: ExportConfirmationsInput,
    cancel_on_rows: bool,
    renderers: crate::viewer::media::helper::BackendFactory,
    canceller: Option<Canceller>,
) -> IpcResult<Vec<JobEvent>> {
    let token = token_of(host, id);
    let draft = host.take_for_start(id, &token)?;
    let prepared = worker::prepare(draft, &c)?;
    worker::recheck(
        &prepared,
        &env(),
        &f.library,
        &c.warnings,
        &FakeProbe::default(),
    )?;
    let r = jobs.reserve(
        JobSubject::archive(prepared.archive.as_str()),
        JobKind::Export,
    )?;
    let hold = env()
        .library(&f.library)
        .and_then(|l| l.hold_for_export(&prepared.archive))
        .map_err(|e| messages::archive(&e))?;
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut sink = Rec {
        events: Arc::clone(&events),
        cancel_on_rows: cancel_on_rows.then(|| r.cancel.clone()),
    };
    let spec = ExportSpec {
        job_id: r.job_id.clone(),
        prepared,
        library: f.library.clone(),
        env: env(),
        hold,
        cancel: r.cancel.clone(),
        host: Arc::clone(host),
        renderers,
    };
    if let Some(c) = canceller {
        c(r.cancel.clone());
    }
    worker::run_export(spec, jobs, &mut sink);
    drop(r.done);
    let out = events.lock().expect("events").clone();
    Ok(out)
}

/// The token of draft `id`'s latest destination.
fn token_of(host: &ExportHost, id: &DraftId) -> DestinationToken {
    host.with_draft(id, |d| {
        Ok(d.destination
            .as_ref()
            .map(|c| c.token.clone())
            .expect("a destination"))
    })
    .expect("draft")
}

/// A decode-helper backend for the tests: a real 1 × 1 PNG for every
/// image (both renditions), counting what it was asked.
#[derive(Clone, Default)]
struct FakeBackend {
    asked: Arc<AtomicU64>,
}

/// A real 1 × 1 PNG (every chunk's CRC is right).
const PNG_1X1: &str = "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d4944415478da636460f85f0f0002870180eb47ba920000000049454e44ae426082";

fn png_1x1() -> Vec<u8> {
    (0..PNG_1X1.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&PNG_1X1[i..i + 2], 16).expect("hex"))
        .collect()
}

impl crate::viewer::media::helper::RenderBackend for FakeBackend {
    fn render(
        &mut self,
        _bytes: &[u8],
        _rendition: oma_archive::Rendition,
        _limits: &oma_archive::RenderLimits,
    ) -> crate::viewer::media::helper::RenderOutcome {
        self.asked.fetch_add(1, Ordering::SeqCst);
        crate::viewer::media::helper::RenderOutcome::Image(
            crate::viewer::media::helper::HelperImage {
                format: oma_archive::RenderedFormat::Png,
                width: 1,
                height: 1,
                source_width: 1,
                source_height: 1,
                has_alpha: true,
                animated: false,
                bytes: png_1x1(),
            },
        )
    }
}

fn fake_renderers() -> crate::viewer::media::helper::BackendFactory {
    Arc::new(|| {
        Box::new(FakeBackend::default()) as Box<dyn crate::viewer::media::helper::RenderBackend>
    })
}

fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn all_bytes(dir: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).expect("read_dir") {
        let p = e.expect("entry").path();
        if p.is_dir() {
            out.extend(all_bytes(&p));
        } else {
            out.extend(std::fs::read(&p).expect("read"));
        }
    }
    out
}

// ---------------------------------------------------------------- inputs

/// Every command's input refuses unknown fields and bad values with a
/// fixed sentence that echoes nothing.
#[test]
fn every_export_command_validates_input() {
    type Check = fn(&Value) -> Option<IpcError>;
    let bodies: Vec<(Value, Check)> = vec![
        (
            json!({ "scope": { "kind": "archive", "archiveId": SENTINEL }, "formats": ["txt"], "options": options(json!({})) }),
            |b| {
                preview_req_raw(b)
                    .and_then(|r| input::parse(&r).map(|_| ()))
                    .err()
            },
        ),
        (
            json!({ "scope": { "kind": "archive", "archiveId": "a" }, "formats": [SENTINEL], "options": options(json!({})) }),
            |b| {
                preview_req_raw(b)
                    .and_then(|r| input::parse(&r).map(|_| ()))
                    .err()
            },
        ),
        (
            json!({ "scope": { "kind": "archive", "archiveId": "a", "path": SENTINEL }, "formats": ["txt"], "options": options(json!({})) }),
            |b| {
                preview_req_raw(b)
                    .and_then(|r| input::parse(&r).map(|_| ()))
                    .err()
            },
        ),
        (json!({ "draftId": SENTINEL }), |b| {
            parse_raw::<DraftRef>(Some(&body(b.clone()))).err()
        }),
        (
            json!({ "draftId": "0".repeat(32), "destination": SENTINEL }),
            |b| parse_raw::<ChooseExportDestinationRequest>(Some(&body(b.clone()))).err(),
        ),
        (
            json!({ "draftId": "A".repeat(32), "confirmations": {} }),
            |b| parse_raw::<StartExportRequest>(Some(&body(b.clone()))).err(),
        ),
        (json!({ "jobId": "job-1", "fileName": SENTINEL }), |b| {
            parse_raw::<ShowExportInFolderRequest>(Some(&body(b.clone()))).err()
        }),
    ];
    for (b, check) in &bodies {
        let e = check(b).unwrap_or_else(|| panic!("not refused: {b}"));
        assert_eq!(e.code, ErrorCode::InvalidInput, "{b}");
        assert!(!e.message.contains(SENTINEL), "{}", e.message);
    }
    // Dates: not a date, inverted, out of range.
    for (from, until) in [
        ("2026-02-30", "2026-03-01"),
        ("2026-03-02", "2026-03-01"),
        ("1899-12-31", "1900-01-02"),
        ("2026-1-01", "2026-02-01"),
    ] {
        let e = preview_req(
            json!({ "kind": "dateRange", "archiveId": "a".repeat(33), "fromLocalDate": from, "untilLocalDate": until }),
            json!(["txt"]),
            json!({}),
        )
        .and_then(|r| input::parse(&r).map(|_| ()))
        .expect_err("refused");
        assert_eq!(e.code, ErrorCode::InvalidInput, "{from} {until}");
    }
    // Ids are SafeIds.
    assert!(preview_req(
        json!({ "kind": "conversation", "archiveId": "a", "chatId": 0 }),
        json!(["txt"]),
        json!({}),
    )
    .and_then(|r| input::parse(&r).map(|_| ()))
    .is_err());
    let good = "0123456789abcdef0123456789abcdef";
    assert_eq!(
        DraftId::parse(good)
            .map(|d| d.as_str().to_owned())
            .as_deref(),
        Some(good)
    );
    assert!(DraftId::parse(&good.to_uppercase()).is_none());
    assert_eq!(
        format!("{:?}", DraftId::parse(good).expect("id")),
        "DraftId(..)"
    );
}

fn preview_req_raw(b: &Value) -> IpcResult<PreviewExportRequest> {
    parse_raw(Some(&body(b.clone())))
}

/// What this version does not build is refused with `exportUnavailable`
/// (never faked); combinations that do not apply are `invalidInput`.
#[test]
fn unbuilt_formats_and_bad_combinations_are_refused() {
    let f = fx();
    let scope = archive_scope(&f);
    let refused = |formats: Value, over: Value, scope: &Value| {
        preview_req(scope.clone(), formats, over)
            .and_then(|r| input::parse(&r).map(|_| ()))
            .expect_err("refused")
    };
    for (formats, over, sentence) in [
        (
            json!(["pdf"]),
            json!({ "pageSize": "a4" }),
            messages::UNAVAILABLE_FORMAT,
        ),
        (
            json!(["txt", "xlsx"]),
            json!({}),
            messages::UNAVAILABLE_FORMAT,
        ),
        (
            json!(["txt"]),
            json!({ "events": false }),
            messages::UNAVAILABLE_EVENTS,
        ),
    ] {
        let e = refused(formats, over, &scope);
        assert_eq!(
            (e.code, e.message.as_str()),
            (ErrorCode::ExportUnavailable, sentence)
        );
    }
    let e = refused(
        json!(["txt"]),
        json!({}),
        &json!({ "kind": "attachment", "viewer": 1, "attachmentId": 3 }),
    );
    assert_eq!(e.code, ErrorCode::ExportUnavailable);
    for (formats, over) in [
        (json!([]), json!({})),
        (json!(["txt", "txt"]), json!({})),
        (json!(["csv"]), json!({})),
        (json!(["txt"]), json!({ "csvProfile": "spreadsheet" })),
        (json!(["txt"]), json!({ "keepExtensions": true })),
        (
            json!(["txt", "html"]),
            json!({ "includeIdentifiers": true }),
        ),
        (json!(["txt"]), json!({ "pageSize": "letter" })),
        // Previews apply to HTML (and PDF) only; larger ones need them.
        (json!(["txt"]), json!({ "previews": true })),
        (json!(["html"]), json!({ "largerPreviews": true })),
        (
            json!(["txt", "csv", "json", "html", "txt", "csv", "json"]),
            json!({}),
        ),
    ] {
        let e = refused(formats.clone(), over, &scope);
        assert_eq!(e.code, ErrorCode::InvalidInput, "{formats}");
    }
    // A good one: JSON with identifiers, CSV with its profile.
    let ok = preview_req(
        scope,
        json!(["json", "csv"]),
        json!({ "includeIdentifiers": true, "csvProfile": "dataTools" }),
    )
    .expect("parses");
    input::parse(&ok).expect("valid");
    // Previews are built (slice 6h2).
    let previews = preview_req(
        archive_scope(&f),
        json!(["html"]),
        json!({ "previews": true, "largerPreviews": true }),
    )
    .expect("parses");
    input::parse(&previews).expect("previews are available");
}

// ---------------------------------------------------------------- drafts

/// A draft counts exactly (the same numbers as the archive's own
/// preview), replaces the previous one, and expires after 30 minutes.
#[test]
fn a_draft_counts_exactly_replaces_the_last_and_expires() {
    let f = fx();
    let clock = Arc::new(FakeClock::default());
    let host = ExportHost::with_clock(clock.clone());
    let req = preview_req(archive_scope(&f), json!(["txt"]), json!({})).expect("req");
    let (first, _) = counted(&host, &f, &req);
    let (id, events) = counted(&host, &f, &req);
    assert_ne!(first, id);
    assert_eq!(
        host.with_draft(&first, |_| Ok(()))
            .expect_err("replaced")
            .code,
        ErrorCode::ExportDraftExpired
    );
    let p = ready(&events);
    let reader = env()
        .library(&f.library)
        .and_then(|l| l.open_read(&f.id))
        .expect("reader");
    let exact = reader
        .export_preview(
            &oma_archive::ExportScopeIds::Archive {
                range: oma_archive::TimeRange::All,
            },
            oma_archive::PreviewOptions::default(),
            &CancelToken::new(),
            &mut |_| {},
        )
        .expect("preview");
    assert_eq!(p.timeline_rows, exact.timeline_rows);
    assert_eq!(p.distinct_messages, exact.distinct_messages);
    assert_eq!(
        p.messages_with_flagged_code,
        exact.messages_with_flagged_code
    );
    assert!(
        p.messages_with_flagged_code >= 1,
        "the catalogue plants a code"
    );
    assert_eq!(p.packaging, PackagingDto::Bundle);
    assert!(p.zone_window_utc_ms.is_none(), "UTC needs no table");
    assert!(p.search.is_none() && !p.clamped);
    // 29 minutes on: still there; 30: expired and dropped.
    clock.0.store(29 * 60 * 1000, Ordering::SeqCst);
    host.with_draft(&id, |_| Ok(())).expect("alive");
    clock.0.store(30 * 60 * 1000, Ordering::SeqCst);
    assert_eq!(
        host.with_draft(&id, |_| Ok(())).expect_err("expired").code,
        ErrorCode::ExportDraftExpired
    );
    assert!(!host.has_draft());
}

/// A draft that leaves the slot any way but a successful start stops its
/// count: `take` of an expired draft, a stale destination token, and a
/// `start_export` refused because the count is still running. A count
/// whose draft is gone, but was not stopped, answers `failed` with
/// `exportDraftExpired` instead of nothing; `take` never takes another
/// draft than the one named.
#[test]
fn drafts_that_leave_the_slot_stop_their_count_and_counts_always_answer() {
    let f = fx();
    let clock = Arc::new(FakeClock::default());
    let host = ExportHost::with_clock(clock.clone());
    let viewer = crate::viewer::ViewerHost::default();
    let req = preview_req(archive_scope(&f), json!(["txt"]), json!({})).expect("req");
    // A start refused because the count has not finished: the draft is
    // consumed and its count stopped.
    let work = begin_preview(&host, &viewer, &req, NOW_MS).expect("draft");
    let e = worker::prepare(
        host.take(&work.draft).expect("draft"),
        &confirmations(json!({})),
    )
    .expect_err("not counted");
    assert_eq!(e.message, messages::NOT_PREPARED);
    assert!(work.token.is_cancelled(), "the count was stopped");
    // A stale token: stopped too.
    let work = begin_preview(&host, &viewer, &req, NOW_MS).expect("draft");
    let other = DestinationToken::random().expect("token");
    assert_eq!(
        host.take_for_start(&work.draft, &other)
            .expect_err("no destination")
            .code,
        ErrorCode::ExportConfirmationStale
    );
    assert!(work.token.is_cancelled());
    // `take` of a draft that is not in the slot takes nothing.
    let a = begin_preview(&host, &viewer, &req, NOW_MS).expect("draft");
    let b = begin_preview(&host, &viewer, &req, NOW_MS).expect("draft");
    assert!(host.take(&a.draft).is_err());
    assert!(host.has_draft(), "b is still there");
    assert!(!b.token.is_cancelled());
    // An expired draft: dropped by `take`, its count stopped.
    clock.0.store(30 * 60 * 1000, Ordering::SeqCst);
    assert_eq!(
        host.take(&b.draft).expect_err("expired").code,
        ErrorCode::ExportDraftExpired
    );
    assert!(b.token.is_cancelled() && !host.has_draft());
    // A count that outlived its draft (here: the slot emptied without
    // stopping it) still answers.
    let work = begin_preview(&host, &viewer, &req, 30 * 60 * 1000).expect("draft");
    let id = work.draft.clone();
    let fresh = CancelToken::new();
    let work = CountWork {
        token: fresh,
        ..work
    };
    host.discard(&id);
    let mut events = Vec::new();
    run_count(&host, &env(), &f.library, work, NOW_MS, &mut |e| {
        events.push(e)
    });
    match events.last() {
        Some(PreviewEvent::Failed { code, .. }) => {
            assert_eq!(*code, ErrorCode::ExportDraftExpired)
        }
        other => panic!("{other:?}"),
    }
}

/// `close_viewer`, idle hide (which closes the viewer) and `hide_viewer`
/// each discard the draft and stop its count (§9.2).
#[test]
fn the_privacy_controls_discard_drafts() {
    let f = fx();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(SettingsStore::load(tmp.path().join("settings.json")));
    let req = preview_req(archive_scope(&f), json!(["txt"]), json!({})).expect("req");
    for control in ["close_viewer", "idle hide", "hide_viewer"] {
        let opened = crate::viewer::open(
            &state.viewer,
            &f.library,
            &env(),
            &state.jobs,
            f.id.as_str(),
        )
        .expect("open");
        let work = begin_preview(&state.exports, &state.viewer, &req, NOW_MS).expect("draft");
        assert!(state.exports.has_draft());
        match control {
            "hide_viewer" => state.hide_viewer(opened.viewer).expect("hide"),
            _ => state.close_viewer(opened.viewer).expect("close"),
        }
        assert!(!state.exports.has_draft(), "{control}");
        assert!(work.token.is_cancelled(), "{control}: the count stops");
        // A count that ends after the discard sends nothing.
        let mut events = Vec::new();
        run_count(&state.exports, &env(), &f.library, work, NOW_MS, &mut |e| {
            events.push(e)
        });
        assert!(events.is_empty(), "{control}: {events:?}");
        state.viewer.close_any();
    }
}

/// A date range in a zone other than UTC asks for its table before
/// counting; with it, the range is counted; every other scope gets its
/// table with the destination, validated against the counted span.
#[test]
fn zones_arrive_before_counting_a_date_range_and_with_the_destination_otherwise() {
    let f = fx();
    let host = ExportHost::default();
    let range = json!({ "kind": "dateRange", "archiveId": f.id.as_str(),
        "fromLocalDate": "1990-01-01", "untilLocalDate": "2026-10-01" });
    let req = preview_req(range.clone(), json!(["txt"]), json!({ "zone": "system" })).expect("req");
    let (_, events) = counted(&host, &f, &req);
    let window = match events.as_slice() {
        [PreviewEvent::NeedsZoneTable {
            span_utc_ms: None,
            window_utc_ms,
            ..
        }] => *window_utc_ms,
        other => panic!("{other:?}"),
    };
    let table = |w: [i64; 2]| -> Value {
        json!({ "name": "Etc/GMT-2", "windowUtcMs": w, "transitions": [[w[0], 7200]] })
    };
    let mut with_zone = serde_json::to_value(json!({
        "scope": range, "formats": ["txt"], "options": options(json!({ "zone": "named" })),
        "zone": table(window),
    }))
    .expect("json");
    let req: PreviewExportRequest = parse_raw(Some(&body(with_zone.clone()))).expect("req");
    let (id, events) = counted(&host, &f, &req);
    let p = ready(&events);
    assert!(p.zone_window_utc_ms.is_none(), "the table is attached");
    // The zone goes with the draft: choose needs none, and refuses one.
    let e = prepare_destination(&host, &id, Some(&id_of(table(window))), NOW_MS)
        .expect_err("already has one");
    assert_eq!(e.code, ErrorCode::InvalidInput);
    prepare_destination(&host, &id, None, NOW_MS).expect("plan");
    // A table that does not cover the window is refused.
    with_zone["zone"] = table([window[0] + 1, window[1]]);
    let req: PreviewExportRequest = parse_raw(Some(&body(with_zone))).expect("req");
    let viewer = crate::viewer::ViewerHost::default();
    assert_eq!(
        begin_preview(&host, &viewer, &req, NOW_MS)
            .err()
            .map(|e| e.code),
        Some(ErrorCode::InvalidInput)
    );
    // The whole archive in a named zone: counted first, the table later.
    let req = preview_req(
        archive_scope(&f),
        json!(["txt"]),
        json!({ "zone": "system" }),
    )
    .expect("req");
    let (id, events) = counted(&host, &f, &req);
    let w = ready(&events)
        .zone_window_utc_ms
        .expect("a window to build");
    assert_eq!(
        prepare_destination(&host, &id, None, NOW_MS)
            .expect_err("needs a table")
            .code,
        ErrorCode::InvalidInput
    );
    let short: ZoneTableInputDto = id_of(table([w[0] + 86_400_000, w[1]]));
    assert_eq!(
        prepare_destination(&host, &id, Some(&short), NOW_MS)
            .expect_err("short")
            .code,
        ErrorCode::InvalidInput
    );
    let plan = prepare_destination(&host, &id, Some(&id_of(table(w))), NOW_MS).expect("plan");
    // 2026-10-19T00:00Z at +02:00.
    assert_eq!(plan.default_name, "Messages export 2026-10-19 02-00");
}

// ----------------------------------------------------------- destination

#[test]
fn the_dialog_title_names_each_sensitive_option() {
    let f = fx();
    let parse = |over: Value| {
        input::parse(&preview_req(archive_scope(&f), json!(["json"]), over).expect("req"))
            .expect("spec")
            .probe_plan()
            .expect("plan")
    };
    assert_eq!(destination::dialog_title(&parse(json!({}))), "Save export");
    let t = destination::dialog_title(&parse(json!({
        "revealOneTimeCodes": true, "revealEarlierContent": true,
        "attachments": "copyOriginals", "keepExtensions": true,
    })));
    for part in [
        "includes one-time codes",
        "includes text captured before an unsend or edit",
        "keeps risky file extensions",
    ] {
        assert!(t.contains(part), "{t}");
    }
    let t = destination::dialog_title(&parse(json!({ "revealEarlierContent": true })));
    assert!(!t.contains("one-time") && t.contains("unsend"), "{t}");
}

/// The checks refuse, warn and name (§8.1, §8.2), against fake probes.
#[test]
fn destination_checks_refuse_warn_and_name() {
    let f = fx();
    let needs = SpaceNeeds {
        copied_bytes: 10 << 20,
        largest_copied: 5 << 30,
        estimate: 100 << 20,
    };
    let probe = FakeProbe::default();
    let inspect = |p: &Path, picked: Picked, probe: &FakeProbe| {
        destination::inspect(
            p,
            picked,
            "Messages export 2026-10-19 14-32",
            Some(&f.library),
            needs,
            probe,
        )
    };
    let code = |r: IpcResult<destination::Chosen>| r.expect_err("refused").code;
    // Refused: the archive folder, inside it, a backup store, a file, a
    // missing folder.
    let inside = f.library.join(f.id.as_str());
    for p in [&f.library, &inside] {
        assert_eq!(
            code(inspect(p, Picked::Folder, &probe)),
            ErrorCode::ExportDestinationRefused
        );
    }
    let store = f.dest.join("Apple").join("MobileSync").join("Backup");
    std::fs::create_dir_all(&store).expect("store");
    assert_eq!(
        code(inspect(&store, Picked::Folder, &probe)),
        ErrorCode::ExportDestinationRefused
    );
    let file = f.dest.join("a-file");
    std::fs::write(&file, b"x").expect("file");
    assert_eq!(
        code(inspect(&file, Picked::Folder, &probe)),
        ErrorCode::ExportDestinationRefused
    );
    assert_eq!(
        code(inspect(&f.dest.join("missing"), Picked::Folder, &probe)),
        ErrorCode::ExportDestinationRefused
    );
    // Too little space for the copied files.
    let small = FakeProbe {
        available: Some(1 << 20),
        ..FakeProbe::default()
    };
    assert_eq!(
        code(inspect(&f.dest, Picked::Folder, &small)),
        ErrorCode::InsufficientSpace
    );
    // Warnings, sorted; unknown space is "unknown", never zero.
    let worried = FakeProbe {
        volume: oma_fs::VolumeKind::Fat(oma_fs::FatKind::Msdos),
        available: Some(50 << 20),
        shared: true,
    };
    let c = inspect(&f.dest, Picked::Folder, &worried).expect("warned");
    assert_eq!(
        c.warnings,
        vec![
            ExportWarningDto::SharedFolder,
            ExportWarningDto::FatVolume,
            ExportWarningDto::MayNotFit
        ]
    );
    assert_eq!(
        (c.dto.enough, c.dto.free_space),
        (SpaceVerdictDto::No, Some(FreeSpaceDto::Under100Mb))
    );
    let net = FakeProbe {
        volume: oma_fs::VolumeKind::Network(oma_fs::NetworkKind::Smb),
        available: None,
        shared: false,
    };
    let c = inspect(&f.dest, Picked::Folder, &net).expect("warned");
    assert_eq!(c.warnings, vec![ExportWarningDto::NetworkOrVirtual]);
    assert_eq!(
        (c.dto.enough, c.dto.free_space),
        (SpaceVerdictDto::Unknown, None)
    );
    // A sync client's folder, by name.
    let synced = f.dest.join("Dropbox");
    std::fs::create_dir(&synced).expect("dropbox");
    let c = inspect(&synced, Picked::Folder, &probe).expect("synced");
    assert_eq!(c.warnings, vec![ExportWarningDto::SyncFolder]);
    assert_eq!(c.dto.sync_provider, Some(SyncProviderDto::Dropbox));
    // Bundle names: the first free one is shown.
    std::fs::create_dir(f.dest.join("Messages export 2026-10-19 14-32")).expect("taken");
    let c = inspect(&f.dest, Picked::Folder, &probe).expect("named");
    assert_eq!(
        serde_json::to_value(&c.dto.display_name).expect("json"),
        json!("Messages export 2026-10-19 14-32 (2)")
    );
    assert_eq!(c.parent, f.dest);
    // Single files: the format's extension is enforced; reserved and
    // unsafe names are refused; an existing regular file needs "Replace".
    let one = |name: &str| {
        inspect(
            &f.dest.join(name),
            Picked::File(oma_export::Format::Txt),
            &probe,
        )
    };
    let c = one("notes.html").expect("appended");
    assert_eq!(
        serde_json::to_value(&c.dto.display_name).expect("json"),
        json!("notes.html.txt")
    );
    assert!(!c.target_exists);
    let c = one("Chat.TXT").expect("own extension");
    assert_eq!(c.target.as_str(), "Chat.TXT");
    for bad in [".oma-export-0123.txt", "a:b.txt"] {
        assert_eq!(code(one(bad)), ErrorCode::ExportNameRefused, "{bad}");
    }
    std::fs::write(f.dest.join("old.txt"), b"x").expect("old");
    assert!(one("old.txt").expect("exists").target_exists);
    std::fs::create_dir(f.dest.join("dir.txt")).expect("dir");
    assert_eq!(code(one("dir.txt")), ErrorCode::ExportTargetExists);
    // Leftovers are counted (an old enough, unlocked partial file).
    let leftover = f
        .dest
        .join(".oma-export-0123456789abcdef0123456789abcdef.partial-file");
    std::fs::write(&leftover, b"x").expect("leftover");
    let old = std::time::SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&leftover)
        .and_then(|f| f.set_modified(old))
        .expect("age");
    assert_eq!(
        inspect(&f.dest, Picked::Folder, &probe)
            .expect("ok")
            .dto
            .leftovers,
        1
    );
    // Start re-checks: a new warning that was not confirmed, and a folder
    // that changed, are refused.
    let c = inspect(&f.dest, Picked::Folder, &probe).expect("ok");
    let e = destination::recheck(&c, Some(&f.library), needs, &[], &net).expect_err("new warning");
    assert_eq!(e.code, ErrorCode::ExportConfirmationStale);
    destination::recheck(
        &c,
        Some(&f.library),
        needs,
        &[ExportWarningDto::NetworkOrVirtual],
        &net,
    )
    .expect("confirmed");
    let moved = f.dest.with_file_name("moved");
    std::fs::rename(&f.dest, &moved).expect("move");
    let e = destination::recheck(&c, Some(&f.library), needs, &[], &probe).expect_err("moved");
    assert_eq!(e.code, ErrorCode::ExportDestinationRefused);
}

// ------------------------------------------------------------------ start

/// `start_export`'s confirmations: everything the draft needs must be
/// confirmed, exactly.
#[test]
fn start_checks_every_confirmation() {
    let f = fx();
    let host = ExportHost::default();
    let take = |req: &PreviewExportRequest, with_destination: bool| {
        let id = if with_destination {
            chosen(&host, &f, req).0
        } else {
            counted(&host, &f, req).0
        };
        host.take(&id).expect("draft")
    };
    let plain = preview_req(archive_scope(&f), json!(["txt"]), json!({})).expect("req");
    let e = worker::prepare(take(&plain, false), &confirmations(json!({})))
        .expect_err("no destination");
    assert_eq!(e.message, messages::NOT_PREPARED);
    let e = worker::prepare(
        take(&plain, true),
        &confirmations(json!({ "reveal": { "oneTimeCodes": 1 } })),
    )
    .expect_err("reveal off");
    assert_eq!(e.message, messages::REVEAL_NOT_CONFIRMED);
    let codes = preview_req(
        archive_scope(&f),
        json!(["txt"]),
        json!({ "revealOneTimeCodes": true }),
    )
    .expect("req");
    let e = worker::prepare(take(&codes, true), &confirmations(json!({}))).expect_err("no count");
    assert_eq!(e.message, messages::REVEAL_NOT_CONFIRMED);
    let keep = preview_req(
        archive_scope(&f),
        json!(["txt"]),
        json!({ "attachments": "copyOriginals", "keepExtensions": true }),
    )
    .expect("req");
    let e = worker::prepare(take(&keep, true), &confirmations(json!({}))).expect_err("extensions");
    assert_eq!(e.message, messages::EXTENSIONS_NOT_CONFIRMED);
    worker::prepare(
        take(&keep, true),
        &confirmations(json!({ "keepRiskyExtension": true })),
    )
    .expect("confirmed");
    // A single file over an existing one needs "Replace".
    let single = preview_req(
        json!({ "kind": "conversation", "archiveId": f.id.as_str(), "chatId": 1 }),
        json!(["txt"]),
        json!({}),
    )
    .expect("req");
    std::fs::write(f.dest.join("chat.txt"), b"old").expect("existing");
    let e = worker::prepare(take(&single, true), &confirmations(json!({}))).expect_err("replace");
    assert_eq!(e.code, ErrorCode::ExportTargetExists);
    let p = worker::prepare(
        take(&single, true),
        &confirmations(json!({ "replace": true })),
    )
    .expect("replace confirmed");
    assert_eq!(p.replace, oma_export::Replace::ConfirmedRegularFile);
    // A stale draft id.
    assert_eq!(
        host.take(&DraftId::parse(&"0".repeat(32)).expect("id"))
            .expect_err("stale")
            .code,
        ErrorCode::ExportDraftExpired
    );
}

/// An import between the preview and the start changes the generation:
/// start refuses with `exportConfirmationStale`.
#[test]
fn a_changed_archive_refuses_start() {
    let f = fx();
    let host = ExportHost::default();
    let req = preview_req(archive_scope(&f), json!(["txt"]), json!({})).expect("req");
    let (id, _) = chosen(&host, &f, &req);
    import_entry(
        &f.library,
        &f.id,
        "view-lin-b-plain",
        LineageChoice::New,
        &json!({}),
    );
    let p = worker::prepare(host.take(&id).expect("draft"), &confirmations(json!({}))).expect("ok");
    let e = worker::recheck(&p, &env(), &f.library, &[], &FakeProbe::default()).expect_err("stale");
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            ErrorCode::ExportConfirmationStale,
            messages::CONFIRMATION_STALE
        )
    );
}

// -------------------------------------------------------------------- job

/// The job end to end: it holds the archive's lock (a second holder or
/// writer is refused), is busy against an import or a verify both ways,
/// shows as "exporting", survives the viewer closing, reports its phases
/// and ends with counts only; its folder is remembered for "Show in
/// folder".
#[test]
fn an_export_holds_the_lock_and_publishes() {
    let f = fx();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(SettingsStore::load(tmp.path().join("settings.json")));
    let host = Arc::clone(&state.exports);
    let req = preview_req(
        archive_scope(&f),
        json!(["txt", "json", "csv", "html"]),
        json!({ "attachments": "copyOriginals", "csvProfile": "spreadsheet" }),
    )
    .expect("req");
    let (id, preview) = chosen(&host, &f, &req);
    let draft = host.take(&id).expect("draft");
    let c = confirmations(json!({}));
    let prepared = worker::prepare(draft, &c).expect("prepared");
    worker::recheck(&prepared, &env(), &f.library, &[], &FakeProbe::default()).expect("recheck");
    let r = state
        .jobs
        .reserve(JobSubject::archive(f.id.as_str()), JobKind::Export)
        .expect("reserve");
    // Busy both ways: nothing else on this archive, nor anywhere (D7).
    for kind in [JobKind::Verify, JobKind::Import, JobKind::Export] {
        let e = state
            .jobs
            .reserve(JobSubject::archive(f.id.as_str()), kind)
            .err()
            .expect("busy");
        assert_eq!(e.code, ErrorCode::Busy);
    }
    let lib = env().library(&f.library).expect("library");
    let hold = lib.hold_for_export(&f.id).expect("hold");
    // The lock: another holder, and a writer, are refused.
    assert!(matches!(
        lib.hold_for_export(&f.id),
        Err(oma_archive::ArchiveError::Locked)
    ));
    assert!(matches!(
        lib.open(&f.id),
        Err(oma_archive::ArchiveError::Locked)
    ));
    let listing = crate::archives::listing(Some(&f.library), &env(), &state.jobs);
    assert_eq!(
        listing.entries.first().map(|e| e.status),
        Some(crate::archives::dto::ArchiveStatusDto::Exporting)
    );
    // The viewer closing does not touch the job.
    state.close_viewer(1).expect("close");
    assert!(state.jobs.any_active());
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut sink = Rec {
        events: Arc::clone(&events),
        cancel_on_rows: None,
    };
    let spec = ExportSpec {
        job_id: r.job_id.clone(),
        prepared,
        library: f.library.clone(),
        env: env(),
        hold,
        cancel: r.cancel.clone(),
        host: Arc::clone(&host),
        renderers: fake_renderers(),
    };
    worker::run_export(spec, &state.jobs, &mut sink);
    drop(r.done);
    assert!(!state.jobs.any_active(), "the slot is free");
    lib.hold_for_export(&f.id).expect("the lock was released");
    let events = events.lock().expect("events").clone();
    let phases: Vec<JobPhase> = events
        .iter()
        .filter_map(|e| match e {
            JobEvent::Progress { phase, .. } => Some(*phase),
            _ => None,
        })
        .collect();
    for p in [
        JobPhase::PreparingExport,
        JobPhase::WritingMessages,
        JobPhase::FinishingExport,
    ] {
        assert!(phases.contains(&p), "{p:?} in {phases:?}");
    }
    assert!(!phases.contains(&JobPhase::RemovingPartial));
    let Some(JobEvent::ExportFinished {
        outcome,
        summary,
        staging_removed,
        ..
    }) = events.last()
    else {
        panic!("{events:?}");
    };
    assert!(*staging_removed && summary.bundle);
    assert_eq!(
        summary.items + summary.folded_reaction_rows,
        preview.timeline_rows
    );
    assert_eq!(summary.conversations, preview.conversations);
    assert!(summary.one_time_codes_masked >= 1, "masked by default");
    assert!(summary.attachments_copied >= 1);
    let excluded = summary.attachments_excluded;
    let any_excluded = excluded.withheld_unsent
        + excluded.no_stored_file
        + excluded.checksum_mismatch
        + excluded.too_large_for_volume
        + excluded.write_failed
        > 0;
    assert_eq!(
        *outcome == ExportOutcomeDto::CompletedWithExclusions,
        any_excluded
    );
    // One published folder; nothing unfinished left beside it.
    let names = entries(&f.dest);
    assert_eq!(
        names,
        vec!["Messages export 2026-10-19 00-00".to_owned()],
        "{names:?}"
    );
    let bundle = f.dest.join(&names[0]);
    for must in ["README.txt", "manifest.json", "SHA256SUMS"] {
        assert!(bundle.join(must).is_file(), "{must}");
    }
    let bytes = all_bytes(&bundle);
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("SYNTH-OTC-482913"), "the code is masked");
    assert!(summary.files_written as usize >= 4);
    assert_eq!(host.finished_folder(&r.job_id), Some(f.dest.clone()));
    assert_eq!(host.finished_folder("job-999"), None);
}

/// The confirmed N must be the draft's own count, never a number the
/// webview chose: a smaller, larger or huge one is refused as stale before
/// any job starts or any file exists; with the counted N the code is
/// written and the manifest records it.
#[test]
fn a_confirmed_count_other_than_the_drafts_refuses_before_any_file() {
    let f = fx();
    let host = Arc::new(ExportHost::default());
    let jobs = Jobs::default();
    let req = preview_req(
        archive_scope(&f),
        json!(["txt"]),
        json!({ "revealOneTimeCodes": true }),
    )
    .expect("req");
    let (_, preview) = chosen(&host, &f, &req);
    let n = preview.messages_with_flagged_code;
    assert!(n >= 1);
    for wrong in [json!(n - 1), json!(n + 1), json!(u64::MAX)] {
        let (id, _) = chosen(&host, &f, &req);
        let e = run_job(
            &host,
            &jobs,
            &f,
            &id,
            confirmations(json!({ "reveal": { "oneTimeCodes": wrong } })),
            false,
        )
        .expect_err("stale");
        assert_eq!(e.code, ErrorCode::ExportConfirmationStale, "{wrong}");
        // The other reveal's count is not a confirmation of this one.
        let (id, _) = chosen(&host, &f, &req);
        let e = run_job(
            &host,
            &jobs,
            &f,
            &id,
            confirmations(json!({ "reveal": { "earlierContent": n } })),
            false,
        )
        .expect_err("the wrong reveal");
        assert_eq!(e.message, messages::REVEAL_NOT_CONFIRMED);
    }
    assert!(entries(&f.dest).is_empty(), "nothing was written");
    let (id, _) = chosen(&host, &f, &req);
    let events = run_job(
        &host,
        &jobs,
        &f,
        &id,
        confirmations(json!({ "reveal": { "oneTimeCodes": n } })),
        false,
    )
    .expect("ran");
    assert!(
        matches!(events.last(), Some(JobEvent::ExportFinished { .. })),
        "{events:?}"
    );
    let names = entries(&f.dest);
    let bytes = all_bytes(&f.dest.join(&names[0]));
    assert!(
        String::from_utf8_lossy(&bytes).contains("SYNTH-OTC-482913"),
        "revealed"
    );
}

/// Both reveals at once: each is confirmed against its own count (one N
/// for both is stale unless the counts are equal), and the manifest
/// records both.
#[test]
fn both_reveals_are_each_confirmed_against_their_own_count() {
    let f = fx();
    let host = Arc::new(ExportHost::default());
    let jobs = Jobs::default();
    let req = preview_req(
        archive_scope(&f),
        json!(["json"]),
        json!({ "revealOneTimeCodes": true, "revealEarlierContent": true }),
    )
    .expect("req");
    let (_, p) = chosen(&host, &f, &req);
    let (nc, ne) = (
        p.messages_with_flagged_code,
        p.messages_with_earlier_content,
    );
    assert!(nc >= 1);
    // The larger count stands for neither of them unless they are equal.
    let one = nc.max(ne) + 1;
    let (id, _) = chosen(&host, &f, &req);
    let e = run_job(
        &host,
        &jobs,
        &f,
        &id,
        confirmations(json!({ "reveal": { "oneTimeCodes": one, "earlierContent": one } })),
        false,
    )
    .expect_err("one N for both");
    assert_eq!(e.code, ErrorCode::ExportConfirmationStale);
    // Only one of the two confirmed.
    let (id, _) = chosen(&host, &f, &req);
    let e = run_job(
        &host,
        &jobs,
        &f,
        &id,
        confirmations(json!({ "reveal": { "oneTimeCodes": nc } })),
        false,
    )
    .expect_err("earlier content unconfirmed");
    assert_eq!(e.message, messages::REVEAL_NOT_CONFIRMED);
    let (id, _) = chosen(&host, &f, &req);
    let events = run_job(
        &host,
        &jobs,
        &f,
        &id,
        confirmations(json!({ "reveal": { "oneTimeCodes": nc, "earlierContent": ne } })),
        false,
    )
    .expect("ran");
    assert!(
        matches!(events.last(), Some(JobEvent::ExportFinished { .. })),
        "{events:?}"
    );
    let names = entries(&f.dest);
    let m: Value = serde_json::from_slice(
        &std::fs::read(f.dest.join(&names[0]).join("manifest.json")).expect("manifest"),
    )
    .expect("json");
    assert_eq!(m["options"]["reveal"]["oneTimeCodesConfirmedUpTo"], nc);
    assert_eq!(m["options"]["reveal"]["earlierContentConfirmedUpTo"], ne);
}

/// A cancel while the messages are written: `removingPartial`, then
/// `cancelled` with the staging removed and nothing left.
#[test]
fn a_cancel_removes_the_unfinished_export() {
    let f = fx();
    let host = Arc::new(ExportHost::default());
    let jobs = Jobs::default();
    let req = preview_req(archive_scope(&f), json!(["txt", "json"]), json!({})).expect("req");
    let (id, _) = chosen(&host, &f, &req);
    let events = run_job(&host, &jobs, &f, &id, confirmations(json!({})), true).expect("ran");
    let phases: Vec<JobPhase> = events
        .iter()
        .filter_map(|e| match e {
            JobEvent::Progress { phase, .. } => Some(*phase),
            _ => None,
        })
        .collect();
    assert!(phases.contains(&JobPhase::RemovingPartial), "{phases:?}");
    assert!(
        matches!(
            events.last(),
            Some(JobEvent::Cancelled {
                staging_removed: true,
                ..
            })
        ),
        "{events:?}"
    );
    assert!(entries(&f.dest).is_empty(), "{:?}", entries(&f.dest));
    assert!(!jobs.any_active());
}

/// A search scope exports the live set the viewer has (no query crosses
/// IPC); the draft holds it, so the job runs after the viewer closed.
#[test]
fn a_search_scope_exports_the_live_set() {
    let f = fx();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(SettingsStore::load(tmp.path().join("settings.json")));
    let opened = crate::viewer::open(
        &state.viewer,
        &f.library,
        &env(),
        &state.jobs,
        f.id.as_str(),
    )
    .expect("open");
    let session = state.viewer.get(opened.viewer).expect("session");
    let started = crate::viewer::search(
        &session,
        &id_of(json!({ "viewer": opened.viewer, "query": "SYNTH", "mode": "exact" })),
        &mut |_| {},
    )
    .expect("search");
    assert!(started.total > 3);
    let req = preview_req(
        json!({ "kind": "searchResults", "viewer": opened.viewer, "searchId": started.search_id, "allMatches": false }),
        json!(["json"]),
        json!({}),
    )
    .expect("req");
    let work = begin_preview(&state.exports, &state.viewer, &req, NOW_MS).expect("draft");
    let id = work.draft.clone();
    let mut events = Vec::new();
    run_count(&state.exports, &env(), &f.library, work, NOW_MS, &mut |e| {
        events.push(e)
    });
    let p = ready(&events);
    let s = p.search.expect("search facts");
    assert_eq!(
        s.kept
            + s.excluded.one_time_codes
            + s.excluded.earlier_content
            + s.excluded.both
            + s.excluded.not_written,
        started.total
    );
    assert!(s.kept >= 1 && !s.all_matches);
    // A superseded search id is refused.
    let stale = preview_req(
        json!({ "kind": "searchResults", "viewer": opened.viewer, "searchId": started.search_id + 1, "allMatches": false }),
        json!(["json"]),
        json!({}),
    )
    .expect("req");
    assert_eq!(
        begin_preview(&state.exports, &state.viewer, &stale, NOW_MS)
            .err()
            .map(|e| e.code),
        Some(ErrorCode::SearchSuperseded)
    );
    // (That refusal left the draft alone.)
    let plan = prepare_destination(&state.exports, &id, None, NOW_MS).expect("plan");
    attach_destination(
        &state.exports,
        &id,
        &f.dest,
        &plan,
        Some(&f.library),
        &FakeProbe::default(),
    )
    .expect("destination");
    let draft = state.exports.take(&id).expect("draft");
    let prepared = worker::prepare(draft, &confirmations(json!({}))).expect("prepared");
    // The viewer closes; the job does not need it.
    state.close_viewer(opened.viewer).expect("close");
    let r = state
        .jobs
        .reserve(JobSubject::archive(f.id.as_str()), JobKind::Export)
        .expect("reserve");
    let hold = env()
        .library(&f.library)
        .and_then(|l| l.hold_for_export(&f.id))
        .expect("hold");
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut sink = Rec {
        events: Arc::clone(&events),
        cancel_on_rows: None,
    };
    worker::run_export(
        ExportSpec {
            job_id: r.job_id.clone(),
            prepared,
            library: f.library.clone(),
            env: env(),
            hold,
            cancel: r.cancel.clone(),
            host: Arc::clone(&state.exports),
            renderers: fake_renderers(),
        },
        &state.jobs,
        &mut sink,
    );
    let events = events.lock().expect("events").clone();
    match events.last() {
        Some(JobEvent::ExportFinished { summary, .. }) => {
            assert_eq!(summary.items, s.kept);
            assert_eq!(summary.search_hits_excluded, Some(started.total - s.kept));
        }
        other => panic!("{other:?}"),
    }
    let names = entries(&f.dest);
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(f.dest.join(&names[0]).join("manifest.json")).expect("manifest"),
    )
    .expect("json");
    assert_eq!(manifest["search"]["mode"], "exact");
    // Nothing that gives the excluded count away with a public cap (§3.6).
    let facts = manifest["search"].as_object().expect("search");
    for k in ["capped", "newestKept", "coverageComplete"] {
        assert!(!facts.contains_key(k), "{k}");
    }
    assert_eq!(
        manifest["search"]["mayBeIncomplete"],
        s.capped || !s.coverage_complete || started.total > s.kept
    );
    assert!(
        !manifest.to_string().contains("\"SYNTH\""),
        "no query in the manifest"
    );
}

// ----------------------------------------------------------------- events

/// `exportFinished` stays under 1 KiB at its largest (D47) and carries
/// counts and codes only.
#[test]
fn export_finished_is_small() {
    let m = MAX_SAFE_INTEGER;
    let summary = ExportSummaryDto {
        items: m,
        folded_reaction_rows: m,
        conversations: m,
        files_written: m,
        attachments_copied: m,
        attachments_blocked: m,
        attachments_excluded: AttachmentExclusionsDto {
            withheld_unsent: m,
            no_stored_file: m,
            checksum_mismatch: m,
            too_large_for_volume: m,
            write_failed: m,
        },
        one_time_codes_masked: m,
        earlier_text_withheld: m,
        edit_text_withheld: m,
        alternative_text_withheld: m,
        hidden_characters_marked: m,
        controls_escaped: m,
        neutralised_cells: m,
        times_outside_zone_window: m,
        undated_excluded: m,
        search_hits_excluded: Some(m),
        bundle: true,
        size: ExportSizeBucketDto::AtLeast10Gb,
        clamped: true,
    };
    let e = JobEvent::ExportFinished {
        job_id: format!("job-{}", u64::MAX),
        outcome: ExportOutcomeDto::CompletedWithExclusions,
        summary,
        staging_removed: true,
    };
    let json = serde_json::to_string(&e).expect("json");
    assert!(json.len() < 1024, "{} bytes: {json}", json.len());
    assert!(e.is_terminal());
    assert_eq!(e.job_id(), format!("job-{}", u64::MAX));
    assert_eq!(ExportSizeBucketDto::of(0), ExportSizeBucketDto::Under1Mb);
    assert_eq!(
        ExportSizeBucketDto::of(1 << 30),
        ExportSizeBucketDto::Under10Gb
    );
}

/// Every sentence is content-free and fixed; every export code has one.
#[test]
fn every_export_code_has_a_sentence() {
    use ErrorCode as C;
    for code in [
        C::ExportDestinationRefused,
        C::ExportTargetExists,
        C::ExportNameRefused,
        C::ExportDraftExpired,
        C::ExportConfirmationStale,
        C::ExportZoneTableStale,
        C::ExportWriteFailed,
        C::ExportHelperFailed,
        C::ExportUnavailable,
    ] {
        assert!(messages::sentence(code).is_some(), "{code:?}");
    }
    for s in messages::ALL {
        assert!(!s.contains('/') && !s.contains('\\'), "{s}");
    }
    use oma_export::ExportError as E;
    for (e, code) in [
        (E::ZoneTableStale, C::ExportZoneTableStale),
        (E::ConfirmationStale, C::ExportConfirmationStale),
        (
            E::Write {
                kind: std::io::ErrorKind::PermissionDenied,
            },
            C::ExportWriteFailed,
        ),
        (E::NoSpace, C::InsufficientSpace),
        (E::TargetExists, C::ExportTargetExists),
        (E::NameRefused, C::ExportNameRefused),
        (E::DestinationChanged, C::ExportDestinationRefused),
        (E::HelperOutputRejected, C::ExportHelperFailed),
        (
            E::Archive(oma_archive::ArchiveError::AnchorNotFound),
            C::InvalidInput,
        ),
        (
            E::Archive(oma_archive::ArchiveError::Locked),
            C::ArchiveLocked,
        ),
    ] {
        assert_eq!(messages::to_ipc(&e).code, code, "{e:?}");
    }
}

// ----------------------------------------------------------------- guards

/// Keys no export type may have, compared ignoring case: the viewer's
/// list (D94) plus the export's own (§9.3).
const EXPORT_BANNED_KEYS: &[&str] = &[
    "guid",
    "rank",
    "rowid",
    "cursor",
    "sortKey",
    "path",
    "storageName",
    "sha256",
    "digest",
    "udid",
    "deviceName",
    "productType",
    "isRead",
    "isDelivered",
    "isSent",
    "dateRead",
    "dateDelivered",
    "datePlayed",
    "lastRead",
    "read",
    "delivered",
    "status",
    "receipt",
    "destination",
    "fileName",
    "query",
    "filter",
    "provider",
    "text",
    "body",
];

/// The only `string` fields: ids the shell handed out.
const EXPORT_STRING_FIELDS: &[&str] = &[
    "ExportScopeInput.archiveId",
    "ShowExportInFolderRequest.jobId",
];

#[test]
fn the_export_guard_admits_only_brands_and_ids() {
    let decls = dto::guard_walk();
    let listed: BTreeSet<String> = ts_guard::M6_EXPORT_TYPES
        .iter()
        .chain(ts_guard::M6_SHARED_TYPES)
        .map(|s| (*s).to_owned())
        .collect();
    let walked: BTreeSet<String> = decls.keys().cloned().collect();
    assert_eq!(
        walked, listed,
        "M6_EXPORT_TYPES and M6_SHARED_TYPES match the walk"
    );
    let mut checked = 0;
    for (ty, decl) in &decls {
        if ts_guard::M6_SHARED_TYPES.contains(&ty.as_str()) {
            continue;
        }
        assert!(!decl.contains("bigint"), "{ty}: {decl}");
        for (key, field_ty) in ts_guard::fields(decl) {
            let qualified = format!("{ty}.{key}");
            assert!(
                !EXPORT_BANNED_KEYS
                    .iter()
                    .any(|b| b.eq_ignore_ascii_case(&key)),
                "banned key {qualified}"
            );
            if ts_guard::is_string(&field_ty) {
                assert!(
                    EXPORT_STRING_FIELDS.contains(&qualified.as_str()),
                    "{qualified} is typed {field_ty}"
                );
            }
            if field_ty.contains("UntrustedText") {
                assert_eq!(qualified, "ExportDestinationDto.displayName");
            }
            if key == "chatId" || key == "attachmentId" {
                assert!(field_ty.contains("SafeId"), "{qualified}: {field_ty}");
            }
            if key == "draftId" {
                assert_eq!(field_ty, "DraftId", "{qualified}");
            }
            checked += 1;
        }
    }
    assert!(checked > 60, "the parser found the fields ({checked})");
    let cfg = Config::from_env();
    assert!(DraftId::decl(&cfg).contains("string & { readonly __draftId"));
    assert!(LocalDate::decl(&cfg).contains("= string"));
    assert!(ZoneName::decl(&cfg).contains("= string"));
}

/// The export walk is disjoint from the backups, archive and viewer walks
/// (apart from the shared types), and the archive guard rejects the
/// export's brands.
#[test]
fn the_export_walk_is_disjoint_and_the_archive_guard_rejects_its_brands() {
    let others: BTreeSet<&str> = ts_guard::M2_M3_TYPES
        .iter()
        .chain(ts_guard::M4_EVENT_TYPES)
        .chain(ts_guard::M6_EVENT_TYPES)
        .chain(crate::viewer::dto::M5_VIEWER_TYPES)
        .copied()
        .collect();
    for ty in ts_guard::M6_EXPORT_TYPES {
        assert!(!others.contains(ty), "{ty} is in another walk");
    }
    let archive = crate::archives::dto::guard_walk();
    for ty in ts_guard::M6_EXPORT_TYPES {
        assert!(!archive.contains_key(*ty), "{ty} reaches the archive walk");
    }
    for ty in ts_guard::M6_EVENT_TYPES {
        assert!(archive.contains_key(*ty), "{ty} is in the archive walk");
    }
    // The archive guard's leak check names an export brand in a field.
    #[derive(serde::Serialize, TS)]
    #[serde(rename_all = "camelCase")]
    struct LeakyArchiveDto {
        archive_id: String,
        draft: DraftId,
    }
    let mut decls = std::collections::BTreeMap::new();
    decls.insert(
        "LeakyArchiveDto".to_owned(),
        LeakyArchiveDto::decl(&Config::from_env()),
    );
    let leaks = crate::archives::dto::brand_leaks(&decls);
    assert_eq!(leaks, vec!["LeakyArchiveDto.draft".to_owned()]);
    assert!(crate::archives::dto::brand_leaks(&archive).is_empty());
}

// ------------------------------------------------------------- the wiring

const EXPORT_COMMANDS: &[&str] = &[
    "preview_export",
    "choose_export_destination",
    "start_export",
    "discard_export_draft",
    "show_export_in_folder",
];

fn read(rel: &str) -> String {
    let p = rel
        .split('/')
        .fold(PathBuf::from(env!("CARGO_MANIFEST_DIR")), |p, s| p.join(s));
    std::fs::read_to_string(p).expect("source")
}

/// Every export command goes through the five steps: the handler is
/// registered, declared in `COMMANDS`, granted, and wrapped in `ipc.ts`
/// (with `callRedacted`, so a rejection never echoes raw text).
#[test]
fn every_export_command_is_wired_through_the_five_steps() {
    let lib = read("src/lib.rs");
    let build = read("build.rs");
    let caps: Value = serde_json::from_str(&read("capabilities/default.json")).expect("json");
    let granted: BTreeSet<String> = caps["permissions"]
        .as_array()
        .expect("permissions")
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let ipc = read("../src/services/ipc.ts");
    let commands = read("src/commands/exports.rs");
    assert_eq!(
        commands.matches("#[tauri::command]").count(),
        EXPORT_COMMANDS.len()
    );
    assert_eq!(
        commands.matches("request: Request<'_>").count(),
        EXPORT_COMMANDS.len()
    );
    for c in EXPORT_COMMANDS {
        assert!(
            commands.contains(&format!("pub async fn {c}("))
                || commands.contains(&format!("pub fn {c}(")),
            "{c} handler"
        );
        assert!(
            lib.contains(&format!("commands::exports::{c},")),
            "{c} registered"
        );
        assert!(build.contains(&format!("\"{c}\",")), "{c} in COMMANDS");
        assert!(
            granted.contains(&format!("allow-{}", c.replace('_', "-"))),
            "{c} granted"
        );
        assert!(ipc.contains(&format!("'{c}'")), "{c} wrapped in ipc.ts");
    }
    assert_eq!(
        ipc.matches("'export").count(),
        9,
        "the nine export codes are listed"
    );
}

// ------------------------------------------------- slice 6h2: destinations

/// `start_export` carries the token of the latest destination: an earlier
/// choice's token (or a made-up one) is refused and consumes the draft.
#[test]
fn only_the_latest_destination_token_starts_an_export() {
    let f = fx();
    let host = Arc::new(ExportHost::default());
    let req = preview_req(archive_scope(&f), json!(["txt", "json"]), json!({})).expect("req");
    let (id, _) = chosen(&host, &f, &req);
    let first = token_of(&host, &id);
    // Choose again: a new token replaces the first.
    let plan = prepare_destination(&host, &id, None, NOW_MS).expect("plan");
    let again = attach_destination(
        &host,
        &id,
        &f.dest,
        &plan,
        Some(&f.library),
        &FakeProbe::default(),
    )
    .expect("again");
    assert_ne!(again.destination_token, first);
    assert_eq!(token_of(&host, &id), again.destination_token);
    let e = host.take_for_start(&id, &first).expect_err("stale token");
    assert_eq!(e.code, ErrorCode::ExportConfirmationStale);
    assert!(!host.has_draft(), "the draft is consumed");
    // A draft without a destination refuses any token.
    let (id, _) = counted(&host, &f, &req);
    let e = host
        .take_for_start(&id, &again.destination_token)
        .expect_err("none chosen");
    assert_eq!(e.code, ErrorCode::ExportConfirmationStale);
    // The token is a 32-hex brand; `Debug` prints no digits.
    let text = serde_json::to_string(&again.destination_token).expect("json");
    assert_eq!(text.len(), 34, "{text}");
    assert_eq!(
        format!("{:?}", again.destination_token),
        "DestinationToken(..)"
    );
    assert!(parse_raw::<StartExportRequest>(Some(&body(json!({
        "draftId": "0".repeat(32), "destinationToken": "XYZ",
        "confirmations": {
            "warnings": [], "replace": false, "reveal": null,
            "keepRiskyExtension": false, "removeLeftovers": false,
        },
    }))))
    .is_err());
}

// ---------------------------------------------- slice 6h2: the helper pool

#[test]
fn the_pool_has_at_most_two_helpers_and_at_least_one() {
    use super::previews::pool_size;
    assert_eq!([0, 1, 2, 3, 4, 64].map(pool_size), [1, 1, 1, 2, 2, 2]);
}

/// The pool answers each submission once, with both renditions when the
/// display rendition was asked; a closed pool answers "crashed".
#[test]
fn the_pool_answers_every_submission() {
    use oma_export::preview::{PoolOutcome, RenditionPool};
    let backend = FakeBackend::default();
    let asked = Arc::clone(&backend.asked);
    let factory: crate::viewer::media::helper::BackendFactory = Arc::new(move || {
        Box::new(backend.clone()) as Box<dyn crate::viewer::media::helper::RenderBackend>
    });
    let pool = super::previews::HelperPool::start(&factory, 2);
    assert_eq!(pool.workers(), 2);
    let token = CancelToken::new();
    let tickets: Vec<_> = (0..6)
        .map(|i| pool.submit(vec![1, 2, 3], i % 2 == 0))
        .collect();
    for (i, t) in tickets.into_iter().enumerate() {
        match t.wait(&token).expect("answered") {
            PoolOutcome::Ready { thumb, display } => {
                assert_eq!(thumb.0, png_1x1());
                assert_eq!(display.is_some(), i % 2 == 0);
            }
            other => panic!("{other:?}"),
        }
    }
    // Three pairs (two renders each through the default) and three thumbs.
    assert_eq!(asked.load(Ordering::SeqCst), 9);
    assert_eq!(pool.shutdown(false), 0);
}

/// Cancel kills a helper in the middle of a render (it would otherwise
/// run until the 20 s timeout): the request ends as a crash at once.
#[cfg(unix)]
#[test]
fn a_killing_shutdown_ends_a_hung_helper_at_once() {
    use crate::viewer::media::helper::{HelperCommand, HelperHost, RenderBackend};
    use oma_export::preview::{PoolOutcome, RenditionPool};
    let factory: crate::viewer::media::helper::BackendFactory = Arc::new(|| {
        Box::new(
            HelperHost::new(Some(HelperCommand::new(
                PathBuf::from("/bin/sh"),
                vec!["-c".into(), "cat >/dev/null".into()],
            )))
            .with_timeout(Duration::from_secs(60)),
        ) as Box<dyn RenderBackend>
    });
    let pool = super::previews::HelperPool::start(&factory, 1);
    let ticket = pool.submit(vec![0u8; 64], true);
    // Let the worker start the helper and send the request.
    std::thread::sleep(Duration::from_millis(300));
    let at = std::time::Instant::now();
    pool.shutdown(true);
    let outcome = ticket.wait(&CancelToken::new()).expect("answered");
    assert_eq!(outcome, PoolOutcome::Crashed);
    assert!(at.elapsed() < Duration::from_secs(5), "{:?}", at.elapsed());
}

/// An HTML export with previews runs the pool: every embedded image is a
/// rendition the helpers made (here the fake's 1 × 1 PNG), never an
/// original, and the larger ones are written beside the pages.
#[test]
fn an_html_export_with_previews_embeds_the_pools_renditions() {
    let f = fx_of(Some("view-media-plain"));
    let host = Arc::new(ExportHost::default());
    let jobs = Jobs::default();
    let req = preview_req(
        archive_scope(&f),
        json!(["html"]),
        json!({ "attachments": "copyOriginals", "previews": true, "largerPreviews": true }),
    )
    .expect("req");
    let (id, _) = chosen(&host, &f, &req);
    let backend = FakeBackend::default();
    let asked = Arc::clone(&backend.asked);
    let factory: crate::viewer::media::helper::BackendFactory = Arc::new(move || {
        Box::new(backend.clone()) as Box<dyn crate::viewer::media::helper::RenderBackend>
    });
    let events = run_job_with(
        &host,
        &jobs,
        &f,
        &id,
        confirmations(json!({})),
        false,
        factory,
        None,
    )
    .expect("ran");
    assert!(
        matches!(events.last(), Some(JobEvent::ExportFinished { .. })),
        "{events:?}"
    );
    assert!(asked.load(Ordering::SeqCst) > 0, "the pool rendered");
    let names = entries(&f.dest);
    let bundle = f.dest.join(&names[0]);
    let mut pages = String::new();
    for e in std::fs::read_dir(bundle.join("html")).expect("html") {
        let p = e.expect("entry").path();
        if p.extension().is_some_and(|x| x == "html") {
            pages.push_str(&std::fs::read_to_string(&p).expect("page"));
        }
    }
    let b64 = oma_export::preview::base64(&png_1x1());
    assert!(pages.contains(&format!("data:image/png;base64,{b64}")));
    let m: Value =
        serde_json::from_slice(&std::fs::read(bundle.join("manifest.json")).expect("manifest"))
            .expect("json");
    assert!(m["counts"]["attachments"]["previewsEmbedded"].as_u64() > Some(0));
    assert_eq!(
        m["counts"]["attachments"]["previewsSkippedAfterHelperFailures"],
        0
    );
}

/// A backend whose render hangs (a helper that never answers, with no
/// process to kill): it flags that it started and sleeps.
#[derive(Clone, Default)]
struct HangingBackend {
    started: Arc<std::sync::atomic::AtomicBool>,
}

impl crate::viewer::media::helper::RenderBackend for HangingBackend {
    fn render(
        &mut self,
        _bytes: &[u8],
        _rendition: oma_archive::Rendition,
        _limits: &oma_archive::RenderLimits,
    ) -> crate::viewer::media::helper::RenderOutcome {
        self.started.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_secs(10));
        crate::viewer::media::helper::RenderOutcome::Crashed
    }
}

/// Cancel latency (spec §8.5, §12) while the writer waits for a helper
/// that hangs: the job sends `cancelled` with the staging removed within
/// a second of the cancel, without waiting for the helper.
#[test]
fn a_cancel_during_a_hung_preview_ends_the_job_within_a_second() {
    let f = fx_of(Some("view-media-plain"));
    let host = Arc::new(ExportHost::default());
    let jobs = Jobs::default();
    let req = preview_req(
        archive_scope(&f),
        json!(["html"]),
        json!({ "previews": true }),
    )
    .expect("req");
    let (id, _) = chosen(&host, &f, &req);
    let backend = HangingBackend::default();
    let started = Arc::clone(&backend.started);
    let factory: crate::viewer::media::helper::BackendFactory = Arc::new(move || {
        Box::new(backend.clone()) as Box<dyn crate::viewer::media::helper::RenderBackend>
    });
    let cancelled_at = Arc::new(Mutex::new(None));
    let at = Arc::clone(&cancelled_at);
    let canceller: Canceller = Box::new(move |token| {
        drop(std::thread::spawn(move || {
            let until = std::time::Instant::now() + Duration::from_secs(60);
            while !started.load(Ordering::SeqCst) {
                assert!(std::time::Instant::now() < until, "a render started");
                std::thread::sleep(Duration::from_millis(5));
            }
            *at.lock().expect("at") = Some(std::time::Instant::now());
            token.cancel();
        }));
    });
    let events = run_job_with(
        &host,
        &jobs,
        &f,
        &id,
        confirmations(json!({})),
        false,
        factory,
        Some(canceller),
    )
    .expect("ran");
    let ended = std::time::Instant::now();
    let at = cancelled_at.lock().expect("at").expect("cancelled");
    let latency = ended.saturating_duration_since(at);
    println!(
        "RECORD cancel_latency_hung_preview_ms={}",
        latency.as_millis()
    );
    assert!(
        matches!(
            events.last(),
            Some(JobEvent::Cancelled {
                staging_removed: true,
                ..
            })
        ),
        "{events:?}"
    );
    assert!(latency < Duration::from_secs(1), "{latency:?}");
    assert!(entries(&f.dest).is_empty(), "{:?}", entries(&f.dest));
}
