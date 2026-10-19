//! The viewer end to end without Tauri (M5 spec §13.7), on archives
//! imported from the synthetic viewer catalogue (`tests/fixtures/viewer`,
//! never written: backups are opened in place with a temporary staging
//! directory): the session lifecycle, every command's operation, input
//! handling, masking and retraction, search, playback checks, the page
//! size bound, the DTO guards and the command wiring.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use oma_archive::space::SpaceProbe;
use oma_archive::{
    Archive, ArchiveCtx, ArchiveId, CancelToken, FixedClock, ImportOptions, ImportOutcome,
    LineageChoice, LineageId, NoProgress, ProceedWith,
};
use oma_backup::{
    inspect as inspect_backup, Candidate, JobCtx, Limits, OpenBackup, Progress, StagingRoot,
};
use serde_json::{json, Value};
use ts_rs::{Config, TS};

use super::dto::*;
use super::input::*;
use super::*;
use crate::ts_guard;

const UDID: &str = "SYNTHETIC0-0000000000000000";

/// 1 TiB free of 2 TiB, so no test depends on the runner's disk.
struct FakeSpace;

impl SpaceProbe for FakeSpace {
    fn available(&self, _: &Path) -> io::Result<u64> {
        Ok(1 << 40)
    }
    fn total(&self, _: &Path) -> io::Result<u64> {
        Ok(2 << 40)
    }
}

pub(crate) fn env() -> ArchiveEnv {
    ArchiveEnv {
        space: Some(Arc::new(FakeSpace)),
    }
}

fn viewer_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("viewer")
}

fn model(entry: &str) -> Value {
    let p = viewer_root()
        .join(format!("{entry}.expected"))
        .join("content.json");
    serde_json::from_slice(&std::fs::read(p).expect("model")).expect("json")
}

/// Import one catalogue backup into archive `id` of `library`.
pub(crate) fn import_entry(
    library: &Path,
    id: &ArchiveId,
    entry: &str,
    lineage: LineageChoice,
    step: &Value,
) -> LineageId {
    let staging = tempfile::tempdir().expect("staging");
    let limits = Limits::default();
    let dir = viewer_root().join(entry).join(UDID);
    let candidate = Candidate::for_dir(&dir).expect("candidate");
    let inspection = inspect_backup(&candidate, &limits, &mut 0).expect("inspect");
    let root = StagingRoot::new(staging.path(), &inspection.root).expect("staging root");
    let session = root.begin().expect("begin");
    let token = CancelToken::new();
    let mut sink = |_: Progress| {};
    let mut ctx = JobCtx::new(&token, &mut sink);
    let ob = OpenBackup::open(inspection, session, None, &limits, &mut ctx)
        .unwrap_or_else(|e| panic!("open failed: {:?}", e.error));
    let lib = env().library(library).expect("library");
    let mut archive: Archive = lib.open(id).expect("open archive");
    let mut o = ImportOptions::default();
    o.include_recently_deleted = step["includeRecentlyDeleted"] == true;
    o.copy_attachments = step["copyAttachments"] != false;
    o.ios_version_untested = step["iosVersionUntested"] == true;
    let clock = FixedClock(1_798_761_600 * 1_000_000_000);
    let mut progress = NoProgress;
    let mut actx = ArchiveCtx {
        cancel: &token,
        progress: &mut progress,
        clock: &clock,
    };
    let out = archive
        .import(&ob, &o, &mut ProceedWith(lineage), &mut actx)
        .expect("import");
    drop(archive);
    assert!(ob.close().removed, "staging removed");
    match out {
        ImportOutcome::Completed(s) => s.lineage,
        other => panic!("import did not complete: {other:?}"),
    }
}

/// Import the first order of `entry`'s model into a new archive of
/// `library`.
fn import_order(library: &Path, entry: &str) -> ArchiveId {
    let m = model(entry);
    let lib = env().library(library).expect("library");
    let id = lib.create(None).expect("create");
    let mut lineages: BTreeMap<String, LineageId> = BTreeMap::new();
    for step in m["orders"][0].as_array().expect("steps") {
        let e = step["entry"].as_str().expect("entry");
        let label = step["lineage"].as_str().expect("lineage").to_owned();
        let choice = match lineages.get(&label) {
            Some(l) => LineageChoice::Existing(*l),
            None => LineageChoice::New,
        };
        let l = import_entry(library, &id, e, choice, step);
        lineages.insert(label, l);
    }
    id
}

/// An archive with the main viewer entry in `library` (for other modules'
/// tests). The returned directory is kept for the archive's lifetime.
pub(crate) fn synthetic_archive(library: &Path) -> (tempfile::TempDir, ArchiveId) {
    let keep = tempfile::tempdir().expect("tempdir");
    (keep, import_order(library, "view-main-plain"))
}

pub(crate) struct Fx {
    _tmp: tempfile::TempDir,
    pub(crate) library: PathBuf,
    pub(crate) id: ArchiveId,
    model: Value,
}

pub(crate) fn fx(entry: &str) -> Fx {
    let tmp = tempfile::tempdir().expect("tempdir");
    let library = tmp.path().join("lib");
    std::fs::create_dir(&library).expect("mkdir");
    let id = import_order(&library, entry);
    Fx {
        _tmp: tmp,
        library,
        id,
        model: model(entry),
    }
}

fn open_fx(f: &Fx, host: &ViewerHost) -> ViewerSessionDto {
    open(host, &f.library, &env(), &Jobs::default(), f.id.as_str()).expect("open")
}

fn strings(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().flat_map(strings).collect(),
        Value::Object(o) => o.values().flat_map(strings).collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn id_of<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).expect("input")
}

fn to_json<T: serde::Serialize>(t: &T) -> String {
    serde_json::to_string(t).expect("json")
}

fn page_req(
    viewer: u32,
    chat: SafeId,
    anchor: Value,
    dir: &str,
    reveal: bool,
) -> GetThreadPageRequest {
    let mut r = json!({
        "viewer": viewer, "chatId": chat.get(), "anchor": anchor,
        "direction": dir, "limit": 100,
    });
    if reveal {
        r["reveal"] = json!({ "earlierText": true, "oneTimeCode": true });
    }
    id_of(r)
}

/// Every page of every conversation (newest to oldest, then the undated
/// section), as JSON.
pub(crate) fn walk_threads(s: &ViewerSession, viewer: u32, reveal: bool) -> Vec<ThreadPageDto> {
    let index = list_conversations(s, &id_of(json!({ "viewer": viewer, "sort": "recent" })))
        .expect("index");
    let mut pages = Vec::new();
    for chat in &index.ids {
        let mut page = thread_page(
            s,
            &page_req(viewer, *chat, json!({ "kind": "newest" }), "older", reveal),
        )
        .expect("newest page");
        let mut guard = 0;
        loop {
            let older = page.has_older.then_some(page.older_anchor).flatten();
            pages.push(page);
            let Some(anchor) = older else { break };
            guard += 1;
            assert!(guard < 200, "paging ends");
            page = thread_page(
                s,
                &page_req(
                    viewer,
                    *chat,
                    json!({ "kind": "message", "message": anchor.get() }),
                    "older",
                    reveal,
                ),
            )
            .expect("older page");
        }
        pages.push(
            thread_page(
                s,
                &page_req(viewer, *chat, json!({ "kind": "undated" }), "newer", reveal),
            )
            .expect("undated page"),
        );
    }
    pages
}

fn body_ids(m: &MessageDto) -> Option<SafeId> {
    match &m.body {
        BodyViewDto::Text { body, .. } => Some(*body),
        BodyViewDto::NoText { detail } => Some(detail.body),
        BodyViewDto::NoBody => None,
    }
}

/// Every output of every read command over one archive, as JSON.
fn every_output(s: &ViewerSession, viewer: u32, reveal: bool) -> Vec<String> {
    let mut out = Vec::new();
    let rv = if reveal {
        json!({ "earlierText": true, "oneTimeCode": true })
    } else {
        json!({})
    };
    for sort in ["recent", "name", "count"] {
        let index = list_conversations(s, &id_of(json!({ "viewer": viewer, "sort": sort })))
            .expect("index");
        for chunk in index.ids.chunks(200) {
            let ids: Vec<i64> = chunk.iter().map(|i| i.get()).collect();
            let rows = get_conversations(s, &id_of(json!({ "viewer": viewer, "ids": ids })), true)
                .expect("rows");
            out.push(to_json(&rows));
        }
        out.push(to_json(&index));
    }
    for page in walk_threads(s, viewer, reveal) {
        let json = to_json(&page);
        assert!(
            json.len() <= PAGE_JSON_MAX,
            "a page of {} bytes",
            json.len()
        );
        out.push(json);
        for (i, m) in page.items.iter().enumerate() {
            let detail = message_detail(
                s,
                &id_of(json!({
                    "viewer": viewer, "messageId": m.id.get(), "reveal": rv,
                    "reactionPage": if i == 0 { json!({}) } else { Value::Null },
                })),
            )
            .expect("detail");
            let dj = to_json(&detail);
            assert!(dj.len() <= PAGE_JSON_MAX, "a detail of {} bytes", dj.len());
            for v in &detail.versions {
                let chunk = message_text(
                    s,
                    &id_of(json!({ "viewer": viewer, "bodyId": v.body.get(), "fromUtf16": 0, "reveal": rv })),
                )
                .expect("chunk");
                out.push(to_json(&chunk));
            }
            out.push(dj);
            if m.reply.is_some() || m.reply_count.is_some_and(|c| c.n > 0) {
                let t = reply_thread(
                    s,
                    &id_of(json!({ "viewer": viewer, "messageId": m.id.get(), "reveal": rv })),
                )
                .expect("reply thread");
                out.push(to_json(&t));
            }
            for a in &m.attachments {
                for for_playback in [false, true] {
                    let i = inspect(
                        s,
                        &id_of(json!({ "viewer": viewer, "attachmentId": a.id.get(), "forPlayback": for_playback })),
                        crate::settings::MediaPlayback::Confirm,
                    )
                    .expect("inspect");
                    out.push(to_json(&i));
                }
            }
        }
    }
    for (mode, query) in [("auto", "SYNTH"), ("exact", "SYNTH"), ("auto", "example")] {
        let started = search(
            s,
            &id_of(json!({
                "viewer": viewer, "query": query, "mode": mode,
                "includeEarlier": reveal, "includeEvents": true,
            })),
            &mut |_| {},
        )
        .expect("search");
        let mut offset: u32 = 0;
        while u64::from(offset) < started.total && offset < 500 {
            let hits = search_results(
                s,
                &id_of(json!({ "viewer": viewer, "searchId": started.search_id, "offset": offset, "limit": 50 })),
            )
            .expect("hits");
            out.push(to_json(&hits));
            offset += 50;
        }
        out.push(to_json(&started));
    }
    out
}

#[test]
fn every_command_runs_and_masks_until_revealed() {
    let mut revealed = Vec::new();
    let mut masked_all = BTreeSet::new();
    for entry in [
        "view-main-plain",
        "view-lin-a2-plain",
        "view-hostile-text-plain",
        "view-media-plain",
    ] {
        let f = fx(entry);
        let host = ViewerHost::default();
        let opened = open_fx(&f, &host);
        assert!(!opened.imports.is_empty(), "{entry}");
        assert_eq!(opened.skipped_rows, 0);
        let s = host.get(opened.viewer).expect("session");
        let masked: Vec<String> = ["oneTimeCodes", "earlierText"]
            .iter()
            .flat_map(|k| strings(&f.model["viewer"]["masked"][*k]))
            .collect();
        for out in every_output(&s, opened.viewer, false) {
            for m in &masked {
                assert!(!out.contains(m.as_str()), "{entry}: masked text in {out}");
            }
            for banned in ["sortKey", "\"guid\"", "storageName", "sha256", "\"rank\""] {
                assert!(!out.contains(banned), "{entry}: {banned}");
            }
        }
        revealed.extend(every_output(&s, opened.viewer, true));
        masked_all.extend(masked);
        host.close(opened.viewer).expect("close");
    }
    assert!(!masked_all.is_empty(), "the catalogue masks something");
    for m in &masked_all {
        assert!(
            revealed.iter().any(|o| o.contains(m.as_str())),
            "revealing shows {m:?}"
        );
    }
}

#[test]
fn sessions_open_close_and_go_stale() {
    let f = fx("view-main-plain");
    let host = ViewerHost::default();
    assert!(!host.is_open());
    let first = open_fx(&f, &host);
    assert!(host.is_open());
    let s1 = host.get(first.viewer).expect("open");
    let second = open_fx(&f, &host);
    assert_ne!(first.viewer, second.viewer, "a new generation");
    assert_eq!(
        host.get(first.viewer).expect_err("stale").code,
        ErrorCode::ViewerClosed
    );
    assert!(s1.closing.is_cancelled(), "the old session was closed");
    let e = list_conversations_by(&host, first.viewer).expect_err("stale");
    assert_eq!(e.code, ErrorCode::ViewerClosed);
    assert!(list_conversations_by(&host, second.viewer).is_ok());
    host.close(first.viewer).expect("a stale close is fine");
    assert!(host.is_open(), "a stale close leaves the session");
    host.close(second.viewer).expect("close");
    assert!(!host.is_open());
    assert_eq!(
        host.get(second.viewer).expect_err("closed").code,
        ErrorCode::ViewerClosed
    );
    // Refusals open nothing.
    let e = open(&host, &f.library, &env(), &Jobs::default(), "../x").expect_err("bad id");
    assert_eq!(e.code, ErrorCode::InvalidInput);
    let missing = format!("a{}", "0".repeat(32));
    let e = open(&host, &f.library, &env(), &Jobs::default(), &missing).expect_err("missing");
    assert_ne!(e.code, ErrorCode::Internal);
    assert!(!host.is_open());
    // An archive that has gone (its directory disappeared) closes the
    // session: the answer is viewerClosed.
    let opened = open_fx(&f, &host);
    let e = with_session(&host, opened.viewer, |_| -> IpcResult<()> {
        Err(IpcError::new(ErrorCode::ArchiveNotFound, "gone"))
    })
    .expect_err("gone");
    assert_eq!(e.code, ErrorCode::ViewerClosed);
    assert!(!host.is_open());
}

fn list_conversations_by(host: &ViewerHost, viewer: u32) -> IpcResult<ConversationIndexDto> {
    with_session(host, viewer, |s| {
        list_conversations(s, &id_of(json!({ "viewer": viewer, "sort": "recent" })))
    })
}

#[test]
fn inputs_are_validated_in_rust() {
    let f = fx("view-main-plain");
    let host = ViewerHost::default();
    let v = open_fx(&f, &host).viewer;
    let s = host.get(v).expect("session");
    let index =
        list_conversations(&s, &id_of(json!({ "viewer": v, "sort": "recent" }))).expect("index");
    let chat = index.ids[0];
    let invalid = |e: IpcError| assert_eq!(e.code, ErrorCode::InvalidInput, "{e:?}");
    for bad in [
        json!(0),
        json!(-3),
        json!(9_007_199_254_740_992_i64),
        json!(1.5),
    ] {
        invalid(
            thread_page(
                &s,
                &id_of(
                    json!({ "viewer": v, "chatId": bad, "anchor": { "kind": "newest" },
                               "direction": "older", "limit": 10 }),
                ),
            )
            .expect_err("bad chat id"),
        );
        invalid(
            message_detail(&s, &id_of(json!({ "viewer": v, "messageId": bad })))
                .expect_err("bad message id"),
        );
    }
    for limit in [0, 101] {
        invalid(
            thread_page(
                &s,
                &id_of(
                    json!({ "viewer": v, "chatId": chat.get(), "anchor": { "kind": "newest" },
                               "direction": "older", "limit": limit }),
                ),
            )
            .expect_err("limit"),
        );
    }
    let ids: Vec<i64> = (1..=201).collect();
    invalid(
        get_conversations(&s, &id_of(json!({ "viewer": v, "ids": ids })), true)
            .expect_err("201 ids"),
    );
    for bounds in [
        json!([0]),
        json!([5, 5]),
        json!([9, 1]),
        json!((0..44).collect::<Vec<i64>>()),
    ] {
        invalid(
            day_counts(
                &s,
                &id_of(json!({ "viewer": v, "chatId": chat.get(), "boundsUtcMs": bounds })),
            )
            .expect_err("bounds"),
        );
    }
    let counts = day_counts(
        &s,
        &id_of(json!({ "viewer": v, "chatId": chat.get(), "boundsUtcMs": [-1_000_000_000_000_i64, 0, 4_000_000_000_000_i64] })),
    )
    .expect("counts");
    assert_eq!(counts.counts.len(), 2);
    // A filter that is too long or holds a control is refused with the
    // fixed sentence; a blank one means none.
    for bad in ["x".repeat(129), "a\u{7}b".to_owned()] {
        let e = list_conversations(
            &s,
            &id_of(json!({ "viewer": v, "sort": "name", "filter": bad })),
        )
        .expect_err("filter");
        assert_eq!(e.message, messages::INVALID_FILTER);
    }
    let all = list_conversations(
        &s,
        &id_of(json!({ "viewer": v, "sort": "recent", "filter": "  " })),
    )
    .expect("blank");
    assert_eq!(all.ids, index.ids);
    // An anchor in another conversation, an unknown message, body or
    // attachment.
    let far = MAX_SAFE;
    let e = thread_page(
        &s,
        &id_of(json!({ "viewer": v, "chatId": chat.get(), "anchor": { "kind": "message", "message": far },
                       "direction": "older", "limit": 10 })),
    )
    .expect_err("anchor");
    assert_eq!(e.code, ErrorCode::AnchorNotFound);
    let e =
        message_detail(&s, &id_of(json!({ "viewer": v, "messageId": far }))).expect_err("message");
    assert_eq!(e.code, ErrorCode::MessageNotFound);
    let e = message_text(
        &s,
        &id_of(json!({ "viewer": v, "bodyId": far, "fromUtf16": 0 })),
    )
    .expect_err("body");
    assert_eq!(e.code, ErrorCode::MessageNotFound);
    let e = inspect(
        &s,
        &id_of(json!({ "viewer": v, "attachmentId": 1, "forPlayback": false })),
        crate::settings::MediaPlayback::Confirm,
    )
    .expect_err("not surfaced");
    assert_eq!(e.code, ErrorCode::AttachmentUnavailable);
    // An out-of-range text offset is clamped.
    let page = thread_page(
        &s,
        &page_req(v, chat, json!({ "kind": "newest" }), "older", false),
    )
    .expect("page");
    let body = page.items.iter().find_map(body_ids).expect("a body");
    let chunk = message_text(
        &s,
        &id_of(json!({ "viewer": v, "bodyId": body.get(), "fromUtf16": u32::MAX })),
    )
    .expect("clamped");
    assert_eq!(chunk.from_utf16, chunk.full_utf16);
    // Search refusals.
    let e = search(
        &s,
        &id_of(json!({ "viewer": v, "query": "\u{1}", "mode": "auto" })),
        &mut |_| {},
    )
    .expect_err("control");
    assert_eq!(e.code, ErrorCode::InvalidQuery);
    let e = search(
        &s,
        &id_of(json!({ "viewer": v, "query": "abc", "mode": "auto",
                       "filters": { "fromUtcMs": 10, "untilUtcMs": 10 } })),
        &mut |_| {},
    )
    .expect_err("empty range");
    assert_eq!(e.code, ErrorCode::InvalidInput);
    let e = search_results(
        &s,
        &id_of(json!({ "viewer": v, "searchId": 77, "offset": 0, "limit": 10 })),
    )
    .expect_err("no such search");
    assert_eq!(e.code, ErrorCode::SearchSuperseded);
}

#[test]
fn searches_supersede_and_cancel() {
    let f = fx("view-main-plain");
    let host = ViewerHost::default();
    let v = open_fx(&f, &host).viewer;
    let s = host.get(v).expect("session");
    let run = |q: &str, mode: &str| {
        search(
            &s,
            &id_of(json!({ "viewer": v, "query": q, "mode": mode })),
            &mut |_| {},
        )
        .expect("search")
    };
    let first = run("SYNTH", "auto");
    assert!(first.total > 0, "the catalogue matches");
    assert_eq!(first.mode, SearchRanDto::Words);
    let page = search_results(
        &s,
        &id_of(json!({ "viewer": v, "searchId": first.search_id, "offset": 0, "limit": 50 })),
    )
    .expect("hits");
    assert!(!page.hits.is_empty());
    let mut progress = Vec::new();
    let second = search(
        &s,
        &id_of(json!({ "viewer": v, "query": "SYNTH", "mode": "exact" })),
        &mut |p| progress.push(p),
    )
    .expect("exact");
    assert_eq!(second.mode, SearchRanDto::Exact);
    assert!(second.coverage.is_some());
    assert!(!progress.is_empty(), "the scan reports progress");
    assert!(
        progress.iter().all(|p| p.search_id == second.search_id),
        "progress names the running search"
    );
    assert_ne!(first.search_id, second.search_id);
    let e = search_results(
        &s,
        &id_of(json!({ "viewer": v, "searchId": first.search_id, "offset": 0, "limit": 5 })),
    )
    .expect_err("superseded");
    assert_eq!(e.code, ErrorCode::SearchSuperseded);
    cancel_search(&s, second.search_id).expect("cancel");
    let e = search_results(
        &s,
        &id_of(json!({ "viewer": v, "searchId": second.search_id, "offset": 0, "limit": 5 })),
    )
    .expect_err("cancelled");
    assert_eq!(e.code, ErrorCode::SearchSuperseded);
    // A search whose token is already set ends as superseded: a cancel
    // reaches it through the slot.
    {
        let mut slot = s.searches().expect("slot");
        slot.running = Some((999, CancelToken::new()));
    }
    let third = run("example", "auto");
    assert!(s.searches().expect("slot").running.is_none());
    cancel_search(&s, 12345).expect("an unknown id is fine");
    // A scan is cancelled by the id its progress names, before it returns
    // (the panel's Cancel, slice 5i); it ends as superseded.
    let mut seen = Vec::new();
    let e = search(
        &s,
        &id_of(json!({ "viewer": v, "query": "SYNTH", "mode": "exact" })),
        &mut |p| {
            seen.push(p.search_id);
            cancel_search(&s, p.search_id).expect("cancel mid-scan");
        },
    )
    .expect_err("cancelled mid-scan");
    assert_eq!(e.code, ErrorCode::SearchSuperseded);
    assert!(!seen.is_empty());
    assert!(s.searches().expect("slot").running.is_none());
    // The search kept before the cancelled scan is still readable.
    assert!(search_results(
        &s,
        &id_of(json!({ "viewer": v, "searchId": third.search_id, "offset": 0, "limit": 1 }))
    )
    .is_ok());
    // Limits.
    for limit in [0, 51] {
        let e = search_results(
            &s,
            &id_of(
                json!({ "viewer": v, "searchId": third.search_id, "offset": 0, "limit": limit }),
            ),
        )
        .expect_err("limit");
        assert_eq!(e.code, ErrorCode::InvalidInput);
    }
}

/// A new search interrupts the running one **before** it waits for the
/// search reader (§7.1): while search A holds the reader (here, inside
/// its progress callback), search B's interrupt must already have been
/// sent. If B waited for the reader first, the interrupt would come only
/// after A let go, and this test would see none.
#[test]
fn a_new_search_interrupts_the_old_one_before_waiting_for_the_reader() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};
    let f = fx("view-main-plain");
    let host = ViewerHost::default();
    let v = open_fx(&f, &host).viewer;
    let s = host.get(v).expect("session");
    let s = &*s;
    let (holding_tx, holding_rx) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let a = scope.spawn(move || {
            let mut seen = None;
            let mut first = true;
            let r = search(
                s,
                &id_of(json!({ "viewer": v, "query": "SYNTH", "mode": "exact" })),
                &mut |_| {
                    if !first {
                        return;
                    }
                    first = false;
                    // A holds the search reader here (its own interrupt
                    // was sent before).
                    let before = s.search_interrupt.count();
                    let _ = holding_tx.send(());
                    let until = Instant::now() + Duration::from_secs(60);
                    while Instant::now() < until {
                        if s.search_interrupt.count() > before {
                            seen = Some(true);
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    seen = Some(false);
                },
            );
            (r, seen)
        });
        holding_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("A holds the reader");
        let b = search(
            s,
            &id_of(json!({ "viewer": v, "query": "SYNTH", "mode": "auto" })),
            &mut |_| {},
        );
        let (a, seen) = a.join().expect("join");
        assert_eq!(seen, Some(true), "B interrupted A before waiting");
        assert_eq!(
            a.expect_err("A superseded").code,
            ErrorCode::SearchSuperseded
        );
        assert!(b.expect("B completes").total > 0);
    });
}

#[test]
fn playback_is_checked_in_rust_and_can_be_off() {
    let f = fx("view-media-plain");
    let host = ViewerHost::default();
    let v = open_fx(&f, &host).viewer;
    let s = host.get(v).expect("session");
    let mut seen = BTreeMap::new();
    for page in walk_threads(&s, v, false) {
        for m in &page.items {
            for a in &m.attachments {
                let req: InspectAttachmentRequest =
                    id_of(json!({ "viewer": v, "attachmentId": a.id.get(), "forPlayback": true }));
                let off = inspect(&s, &req, crate::settings::MediaPlayback::Off).expect("off");
                assert_eq!(off.playback, Some(PlaybackDto::Off));
                let on =
                    inspect(&s, &req, crate::settings::MediaPlayback::ClickToPlay).expect("on");
                let quick: InspectAttachmentRequest =
                    id_of(json!({ "viewer": v, "attachmentId": a.id.get() }));
                assert_eq!(
                    inspect(&s, &quick, crate::settings::MediaPlayback::Confirm)
                        .expect("quick")
                        .playback,
                    None
                );
                match (&on.content, on.playback) {
                    (
                        ContentKindDto::Sniffed {
                            class: MediaClassDto::Playable { .. },
                            ..
                        },
                        Some(p),
                    ) => {
                        assert!(
                            !matches!(p, PlaybackDto::NotPlayable | PlaybackDto::Off),
                            "{p:?}"
                        );
                    }
                    (_, Some(p)) => assert_eq!(p, PlaybackDto::NotPlayable),
                    (_, None) => panic!("asked for playback"),
                }
                seen.insert(a.id, on.playback);
            }
        }
    }
    assert!(
        seen.values()
            .any(|p| matches!(p, Some(PlaybackDto::Allowed { .. }))),
        "some file plays"
    );
    assert!(
        seen.values()
            .any(|p| matches!(p, Some(PlaybackDto::Refused { .. }))),
        "some file is refused by the structural check"
    );
}

#[test]
fn served_keys_are_remembered_and_bounded() {
    let f = fx("view-main-plain");
    let host = ViewerHost::default();
    let v = open_fx(&f, &host).viewer;
    let s = host.get(v).expect("session");
    let index =
        list_conversations(&s, &id_of(json!({ "viewer": v, "sort": "recent" }))).expect("index");
    let chat = ChatId::new(index.ids[0].get());
    let page = s
        .pages()
        .expect("pages")
        .thread_page(
            chat,
            oa::Anchor::Newest,
            oa::Direction::Older,
            10,
            None,
            oa::Reveal::default(),
        )
        .expect("page");
    let (_, hint) = page.served_keys()[0].clone();
    let dto = thread_page(
        &s,
        &page_req(v, index.ids[0], json!({ "kind": "newest" }), "older", false),
    )
    .expect("page");
    let anchor = dto.older_anchor.expect("an older anchor");
    let keys = s.served_keys().expect("keys");
    assert!(
        keys.get(chat, MessageId::new(anchor.get())).is_some(),
        "the anchor's key is remembered"
    );
    drop(keys);
    let mut lru = session::SortKeyLru::default();
    for i in 0..(session::SERVED_KEYS as i64 + 50) {
        lru.remember(chat, MessageId::new(i + 1), hint.clone());
    }
    assert_eq!(lru.len(), session::SERVED_KEYS);
    assert!(lru.get(chat, MessageId::new(1)).is_none(), "oldest evicted");
    assert!(lru
        .get(chat, MessageId::new(session::SERVED_KEYS as i64 + 50))
        .is_some());
}

/// The reaction history pages by a keyset Rust remembers: each next page
/// is asked for by the last row's id, every row comes once, and an id
/// that was never a served page end is refused.
#[test]
fn reaction_history_pages_by_remembered_positions() {
    let f = fx("view-main-plain");
    let host = ViewerHost::default();
    let v = open_fx(&f, &host).viewer;
    let s = host.get(v).expect("session");
    let detail = |m: i64, page: Value| {
        message_detail(
            &s,
            &id_of(json!({ "viewer": v, "messageId": m, "reactionPage": page })),
        )
    };
    let mut paged = 0;
    for page in walk_threads(&s, v, false) {
        for m in &page.items {
            let first = detail(m.id.get(), json!({})).expect("detail");
            let h = first.reactions.expect("history");
            if !h.has_more {
                continue;
            }
            let mut seen: BTreeSet<i64> = h.rows.iter().map(|r| r.id.get()).collect();
            let mut last = h.rows.last().expect("rows").id.get();
            loop {
                let next = detail(m.id.get(), json!({ "after": last }))
                    .expect("next page")
                    .reactions
                    .expect("history");
                for r in &next.rows {
                    assert!(seen.insert(r.id.get()), "a row served twice");
                }
                if !next.has_more {
                    break;
                }
                last = next.rows.last().expect("rows").id.get();
            }
            assert_eq!(u64::try_from(seen.len()).expect("n"), h.total.n);
            // A row that ended no served page is refused.
            let e = detail(m.id.get(), json!({ "after": m.id.get() })).expect_err("unknown");
            assert_eq!(e.code, ErrorCode::InvalidInput);
            paged += 1;
        }
    }
    assert!(paged > 0, "a target with more than one page");
}

#[test]
fn oversized_pages_are_cut_to_the_bound() {
    let f = fx("view-hostile-text-plain");
    let host = ViewerHost::default();
    let v = open_fx(&f, &host).viewer;
    let s = host.get(v).expect("session");
    let mut biggest = 0;
    for page in walk_threads(&s, v, true) {
        biggest = biggest.max(to_json(&page).len());
        let mut grown = page.clone();
        if grown.items.is_empty() {
            continue;
        }
        while to_json(&grown).len() <= PAGE_JSON_MAX {
            let more = grown.items.clone();
            grown.items.extend(more);
        }
        let n = grown.items.len();
        fit_page(&mut grown, false).expect("fits");
        assert!(to_json(&grown).len() <= PAGE_JSON_MAX);
        assert!(grown.cut_for_size && grown.has_older && grown.items.len() < n);
        assert_eq!(grown.older_anchor, grown.items.first().map(|m| m.id));
        break;
    }
    assert!(biggest > 0 && biggest <= PAGE_JSON_MAX);
    // An anchor the cut moved onto an item gets that item's key
    // remembered, so the next page from it can still tell it moved.
    let index =
        list_conversations(&s, &id_of(json!({ "viewer": v, "sort": "recent" }))).expect("index");
    let chat = ChatId::new(index.ids[0].get());
    let page = s
        .pages()
        .expect("pages")
        .thread_page(
            chat,
            oa::Anchor::Newest,
            oa::Direction::Older,
            10,
            None,
            oa::Reveal::default(),
        )
        .expect("page");
    let mut dto = ThreadPageDto::build(&page);
    assert!(dto.items.len() >= 2, "a page to cut");
    // Grow it just past the bound (each added item's size is known), so
    // the cut removes a few items only.
    let base = dto.items.clone();
    let sizes: Vec<usize> = base.iter().map(|m| to_json(m).len() + 1).collect();
    let mut total = to_json(&dto).len();
    let mut i = 0;
    while total <= PAGE_JSON_MAX {
        dto.items.push(base[i % base.len()].clone());
        total += sizes[i % base.len()];
        i += 1;
    }
    fit_page(&mut dto, false).expect("fits");
    let moved = MessageId::new(dto.older_anchor.expect("moved").get());
    let before = s.served_keys().expect("keys").get(chat, moved).is_some();
    remember_moved_anchors(&s, chat, &page, &dto).expect("remember");
    let keys = s.served_keys().expect("keys");
    assert!(
        before || keys.get(chat, moved).is_some(),
        "the moved anchor's key is remembered"
    );
    assert_eq!(keys.get(chat, moved), page.key_of(moved));
}

// ------------------------------------------------------------- the guards

/// Every viewer type reachable from the commands' outputs and inputs.
fn viewer_walk() -> BTreeMap<String, String> {
    let mut w = ts_guard::Walk::new();
    w.root::<ViewerSessionDto>();
    w.root::<ConversationIndexDto>();
    w.root::<ConversationRowsDto>();
    w.root::<ThreadPageDto>();
    w.root::<MessageDetailDto>();
    w.root::<TextChunkDto>();
    w.root::<DayCountsDto>();
    w.root::<SeekResultDto>();
    w.root::<SearchStartedDto>();
    w.root::<SearchHitsDto>();
    w.root::<ScanProgressDto>();
    w.root::<AttachmentInspectionDto>();
    w.root::<OpenViewerRequest>();
    w.root::<ViewerRef>();
    w.root::<ListConversationsRequest>();
    w.root::<GetConversationsRequest>();
    w.root::<GetThreadPageRequest>();
    w.root::<GetReplyThreadRequest>();
    w.root::<GetMessageDetailRequest>();
    w.root::<GetMessageTextRequest>();
    w.root::<GetDayCountsRequest>();
    w.root::<SeekDateRequest>();
    w.root::<SearchMessagesRequest>();
    w.root::<GetSearchResultsRequest>();
    w.root::<CancelSearchRequest>();
    w.root::<InspectAttachmentRequest>();
    w.decls()
}

/// Keys no viewer type may have (D94, §7.4), compared case-insensitively.
const VIEWER_BANNED_KEYS: &[&str] = &[
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
];

/// The only `string` fields: the inputs' archive id and free texts.
const VIEWER_STRING_FIELDS: &[&str] = &[
    "OpenViewerRequest.archiveId",
    "ListConversationsRequest.filter",
    "SearchMessagesRequest.query",
];

#[test]
fn the_viewer_guard_admits_only_branded_strings_and_safe_ids() {
    let decls = viewer_walk();
    let mut checked = 0;
    let mut outputs = BTreeSet::new();
    for (ty, decl) in &decls {
        assert!(!decl.contains("bigint"), "{ty}: {decl}");
        if !M5_VIEWER_INPUTS.contains(&ty.as_str()) {
            outputs.insert(ty.clone());
        }
        for (key, field_ty) in ts_guard::fields(decl) {
            let qualified = format!("{ty}.{key}");
            assert!(
                !VIEWER_BANNED_KEYS
                    .iter()
                    .any(|b| b.eq_ignore_ascii_case(&key)),
                "banned key {qualified}"
            );
            if ts_guard::is_string(&field_ty) {
                assert!(
                    VIEWER_STRING_FIELDS.contains(&qualified.as_str()),
                    "{qualified} is typed {field_ty}"
                );
            }
            // Ids are SafeId, never a bare number.
            if key == "id" || key.ends_with("Id") && key != "searchId" {
                assert!(
                    field_ty.contains("SafeId") || qualified == "OpenViewerRequest.archiveId",
                    "{qualified} is typed {field_ty}"
                );
            }
            checked += 1;
        }
    }
    assert!(checked > 300, "the parser found the fields ({checked})");
    // The brands are declared once each, with their shapes.
    let cfg = Config::from_env();
    assert!(UntrustedText::decl(&cfg).contains("string & { readonly __untrusted"));
    assert!(DecimalString::decl(&cfg).contains("string & { readonly __decimal"));
    assert!(SafeId::decl(&cfg).contains("number & { readonly __id"));
    assert!(MediaToken::decl(&cfg).contains("string & { readonly __mediaToken"));
    assert!(MediaBase::decl(&cfg).contains("string & { readonly __mediaBase"));
    // The type list is the walk's outputs.
    let listed: BTreeSet<String> = M5_VIEWER_TYPES.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(outputs, listed, "M5_VIEWER_TYPES matches the walk");
    let inputs: BTreeSet<String> = M5_VIEWER_INPUTS.iter().map(|s| (*s).to_owned()).collect();
    let walked: BTreeSet<String> = decls
        .keys()
        .filter(|k| !listed.contains(*k))
        .cloned()
        .collect();
    assert_eq!(walked, inputs, "M5_VIEWER_INPUTS matches the walk");
}

#[test]
fn brand_constructors_reject_bad_values() {
    for bad in [0, -1, MAX_SAFE + 1, i64::MIN, i64::MAX] {
        assert!(SafeId::new(bad).is_none(), "{bad}");
    }
    assert_eq!(SafeId::new(MAX_SAFE).map(SafeId::get), Some(MAX_SAFE));
    for ok in ["0", "-1", "12345678901234567890", "-9223372036854775808"] {
        assert!(DecimalString::parse(ok).is_some(), "{ok}");
    }
    for bad in ["", "-", "1.5", "1e3", "123456789012345678901", " 1", "١"] {
        assert!(DecimalString::parse(bad).is_none(), "{bad}");
    }
    assert_eq!(DecimalString::of(i64::MIN).as_str(), "-9223372036854775808");
    let mut cx = Conv::default();
    assert!(cx.id(0).is_none() && cx.id(MAX_SAFE + 1).is_none());
    assert_eq!(cx.skipped, 2);
}

/// The archive guard's walk and the viewer's are disjoint.
#[test]
fn the_archive_and_viewer_walks_are_disjoint() {
    let viewer: BTreeSet<String> = viewer_walk().into_keys().collect();
    let archive = crate::archives::dto::guard_walk();
    for ty in archive.keys() {
        assert!(!viewer.contains(ty), "{ty} is in both walks");
    }
}

// ------------------------------------------------------------- the wiring

const VIEWER_COMMANDS: &[&str] = &[
    "open_viewer",
    "close_viewer",
    "hide_viewer",
    "list_conversations",
    "get_conversations",
    "get_thread_page",
    "get_reply_thread",
    "get_message_detail",
    "get_message_text",
    "get_day_counts",
    "seek_date",
    "search_messages",
    "get_search_results",
    "cancel_search",
    "inspect_attachment",
];

fn read(rel: &str) -> String {
    let p = rel
        .split('/')
        .fold(PathBuf::from(env!("CARGO_MANIFEST_DIR")), |p, s| p.join(s));
    std::fs::read_to_string(p).expect("source")
}

/// Every command goes through the five steps (§7.2): the handler is
/// registered, declared in `COMMANDS`, granted, and wrapped in `ipc.ts`.
#[test]
fn every_command_is_wired_through_the_five_steps() {
    let lib = read("src/lib.rs");
    let build = read("build.rs");
    let caps: Value = serde_json::from_str(&read("capabilities/default.json")).expect("json");
    let granted: BTreeSet<String> = strings(&caps["permissions"]).into_iter().collect();
    let ipc = read("../src/services/ipc.ts");
    let commands = read("src/commands/viewer.rs");
    let handlers = commands.matches("#[tauri::command]").count();
    assert_eq!(handlers, VIEWER_COMMANDS.len(), "one handler per command");
    for c in VIEWER_COMMANDS {
        assert!(
            commands.contains(&format!("pub async fn {c}("))
                || commands.contains(&format!("pub fn {c}(")),
            "{c} handler"
        );
        assert!(
            lib.contains(&format!("commands::viewer::{c},")),
            "{c} registered"
        );
        assert!(build.contains(&format!("\"{c}\",")), "{c} in COMMANDS");
        assert!(
            granted.contains(&format!("allow-{}", c.replace('_', "-"))),
            "{c} granted"
        );
        assert!(ipc.contains(&format!("'{c}'")), "{c} wrapped in ipc.ts");
    }
    // The string-input commands parse the raw request and are redacted on
    // the frontend.
    for c in ["list_conversations", "search_messages"] {
        assert!(ipc.contains("callRedacted<") && ipc.contains(&format!("'{c}'")));
    }
    assert_eq!(commands.matches("request: Request<'_>").count(), 2);
}
