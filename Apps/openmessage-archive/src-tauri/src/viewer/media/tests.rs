//! The `oma-media` protocol without a window (M5 spec §13.6), on an
//! archive imported from the synthetic viewer catalogue's media entry.
//! Decoding goes through a fake backend here (the real helper process is
//! tested in `tests/decode_helper.rs`); reads, hashes, structural checks
//! and ranges are the real ones.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use oma_archive::{AttachmentId, BlobId, CancelToken, RenderError, RenderLimits, RenderedFormat};
use serde_json::json;

use super::engine::{MediaConfig, DECODE_QUEUE};
use super::helper::{HelperImage, RenderBackend, RenderOutcome};
use super::*;
use crate::backups::Jobs;
use crate::settings::MediaPlayback;
use crate::viewer::dto::{
    AttachmentInspectionDto, ContentKindDto, ImageSizeDto, MediaClassDto, PlaybackDto,
};
use crate::viewer::input::InspectAttachmentRequest;
use crate::viewer::session::{ViewerSession, VERIFY_QUEUE};
use crate::viewer::tests::{env, fx, id_of, walk_threads, Fx};
use crate::viewer::{inspect, open, ViewerHost};

const WAIT: Duration = Duration::from_secs(60);
const FAKE_JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xD9];

// ------------------------------------------------------------ the fakes

/// A gate the fake backend waits at, counting who arrived.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    opened: Condvar,
    entered: Mutex<usize>,
    arrived: Condvar,
}

impl Gate {
    fn release(&self) {
        *self.open.lock().expect("gate") = true;
        self.opened.notify_all();
    }

    fn pass(&self) {
        *self.entered.lock().expect("gate") += 1;
        self.arrived.notify_all();
        let mut open = self.open.lock().expect("gate");
        while !*open {
            open = self.opened.wait(open).expect("gate");
        }
    }

    fn wait_entered(&self, n: usize) {
        let mut e = self.entered.lock().expect("gate");
        while *e < n {
            let (g, t) = self.arrived.wait_timeout(e, WAIT).expect("gate");
            assert!(!t.timed_out(), "workers did not arrive");
            e = g;
        }
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Image,
    Refuse,
    Crash,
}

#[derive(Default)]
struct Counts {
    created: AtomicUsize,
    calls: AtomicUsize,
    dropped: AtomicUsize,
}

struct Fake {
    mode: Mode,
    gate: Option<Arc<Gate>>,
    counts: Arc<Counts>,
}

impl RenderBackend for Fake {
    fn render(
        &mut self,
        bytes: &[u8],
        _: oma_archive::Rendition,
        _: &RenderLimits,
    ) -> RenderOutcome {
        assert!(!bytes.is_empty(), "verified bytes reach the backend");
        self.counts.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(g) = &self.gate {
            g.pass();
        }
        match self.mode {
            Mode::Image => RenderOutcome::Image(HelperImage {
                format: RenderedFormat::Jpeg,
                width: 6,
                height: 4,
                source_width: 6,
                source_height: 4,
                has_alpha: false,
                animated: false,
                bytes: FAKE_JPEG.to_vec(),
            }),
            Mode::Refuse => RenderOutcome::Refused(RenderError::Malformed),
            Mode::Crash => RenderOutcome::Crashed,
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.counts.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

fn config(mode: Mode, gate: Option<Arc<Gate>>) -> (MediaConfig, Arc<Counts>) {
    let counts = Arc::new(Counts::default());
    let c = Arc::clone(&counts);
    let cfg = MediaConfig {
        backend: Arc::new(move || {
            c.created.fetch_add(1, Ordering::SeqCst);
            Box::new(Fake {
                mode,
                gate: gate.clone(),
                counts: Arc::clone(&c),
            }) as Box<dyn RenderBackend>
        }),
        crash_cap: 5,
    };
    (cfg, counts)
}

// ------------------------------------------------------------- the rig

struct Rig {
    f: Fx,
    host: ViewerHost,
    v: u32,
    counts: Arc<Counts>,
    /// Every token the pages minted, with its attachment.
    tokens: BTreeMap<String, AttachmentId>,
}

fn rig(mode: Mode, gate: Option<Arc<Gate>>) -> Rig {
    let f = fx("view-media-plain");
    let (cfg, counts) = config(mode, gate);
    let host = ViewerHost::with_media(cfg);
    let v = open(&host, &f.library, &env(), &Jobs::default(), f.id.as_str())
        .expect("open")
        .viewer;
    let s = host.get(v).expect("session");
    let mut tokens = BTreeMap::new();
    for page in walk_threads(&s, v, false) {
        for m in &page.items {
            for a in &m.attachments {
                assert_eq!(a.token.is_some(), a.stored, "a token for each stored file");
                if let Some(t) = &a.token {
                    tokens.insert(t.as_str().to_owned(), AttachmentId::new(a.id.get()));
                }
            }
        }
    }
    assert!(tokens.len() >= 6, "the media entry has files");
    Rig {
        f,
        host,
        v,
        counts,
        tokens,
    }
}

impl Rig {
    fn session(&self) -> Arc<ViewerSession> {
        self.host.get(self.v).expect("session")
    }

    fn inspect(&self, id: AttachmentId, for_playback: bool) -> AttachmentInspectionDto {
        let req: InspectAttachmentRequest = id_of(json!({
            "viewer": self.v, "attachmentId": id.get(), "forPlayback": for_playback,
        }));
        inspect(&self.session(), &req, MediaPlayback::ClickToPlay).expect("inspect")
    }

    /// Inspect every token's file, grouped by class: (token, attachment).
    fn classes(&self) -> BTreeMap<&'static str, Vec<(String, AttachmentId)>> {
        let mut out: BTreeMap<&'static str, Vec<(String, AttachmentId)>> = BTreeMap::new();
        for (t, id) in &self.tokens {
            let d = self.inspect(*id, false);
            assert_eq!(
                d.token.as_ref().map(|x| x.as_str()),
                Some(t.as_str()),
                "stable"
            );
            let class = match d.content {
                ContentKindDto::Sniffed {
                    class: MediaClassDto::Image,
                    ..
                } => "image",
                ContentKindDto::Sniffed {
                    class: MediaClassDto::Playable { .. },
                    ..
                } => "playable",
                _ => "other",
            };
            out.entry(class).or_default().push((t.clone(), *id));
        }
        out
    }

    fn blob(&self, id: AttachmentId) -> BlobId {
        let s = self.session();
        let facts = s.pages().expect("reader").attachment(id).expect("facts");
        facts.blob.expect("a stored file").id
    }

    fn bytes(&self, id: AttachmentId) -> Vec<u8> {
        let blob = self.blob(id);
        self.session()
            .pages()
            .expect("reader")
            .read_blob_verified(blob, u64::MAX)
            .expect("bytes")
            .expose()
            .clone()
    }
}

fn reply() -> (Reply, Receiver<MediaResponse>) {
    let (tx, rx) = mpsc::channel();
    (
        Reply::new(Box::new(move |r| {
            let _ = tx.send(r);
        })),
        rx,
    )
}

struct Ask<'a> {
    label: &'a str,
    method: &'a str,
    ranges: &'a [&'a str],
    origins: &'a [&'a str],
    playback: bool,
}

const GET: Ask<'static> = Ask {
    label: "main",
    method: "GET",
    ranges: &[],
    origins: &[],
    playback: true,
};

fn ask(host: &ViewerHost, target: &str, a: &Ask<'_>) -> Receiver<MediaResponse> {
    let (r, rx) = reply();
    dispatch(
        host,
        &MediaRequest {
            label: a.label,
            method: a.method,
            target,
            ranges: a.ranges,
            origins: a.origins,
            playback_allowed: a.playback,
        },
        r,
    );
    rx
}

fn wait(rx: &Receiver<MediaResponse>) -> MediaResponse {
    rx.recv_timeout(WAIT).expect("an answer")
}

/// Answered before `dispatch` returned.
fn now(rx: &Receiver<MediaResponse>) -> MediaResponse {
    rx.try_recv().expect("answered at once")
}

fn send(host: &ViewerHost, target: &str, a: &Ask<'_>) -> MediaResponse {
    wait(&ask(host, target, a))
}

fn url(token: &str, rendition: &str) -> String {
    format!("/v1/{token}/{rendition}")
}

fn assert_hardened(r: &MediaResponse) {
    for (k, v) in SECURITY_HEADERS {
        assert_eq!(r.header(k), Some(v), "{k}");
    }
    assert!(r.header("Content-Disposition").is_none());
    assert!(r.header("Cross-Origin-Resource-Policy").is_none());
}

fn assert_refused(r: &MediaResponse, what: &str) {
    assert_eq!(r.status, 404, "{what}");
    assert!(r.body.is_empty(), "{what}: empty body");
    assert!(r.header("Content-Type").is_none(), "{what}: no type");
    assert_hardened(r);
}

fn ranged<'a>(ranges: &'a [&'a str]) -> Ask<'a> {
    Ask { ranges, ..GET }
}

// ------------------------------------------------------------- the tests

/// §13.6 refusals: each a `404` with an empty body, answered at once.
#[test]
fn malformed_and_unauthorised_requests_are_refused_at_once() {
    let r = rig(Mode::Image, None);
    let images = r.classes().remove("image").expect("images");
    let (tok, _) = &images[0];
    assert_eq!(send(&r.host, &url(tok, "thumb"), &GET).status, 200);
    let refuse = |target: &str, a: &Ask<'_>, what: &str| {
        let rx = ask(&r.host, target, a);
        assert_refused(&now(&rx), what);
    };
    refuse(
        &url(tok, "thumb"),
        &Ask {
            label: "other",
            ..GET
        },
        "label",
    );
    for m in ["POST", "PUT", "OPTIONS", "DELETE", "get", ""] {
        refuse(&url(tok, "thumb"), &Ask { method: m, ..GET }, m);
    }
    let upper = tok.to_ascii_uppercase();
    for target in [
        "/".to_owned(),
        "/v1/".to_owned(),
        format!("/v1/{}/thumb", &tok[..31]),
        format!("/v1/{tok}a/thumb"),
        format!("/v1/{upper}/thumb"),
        format!("/v1/{tok}/thumb/"),
        format!("/v1/{tok}/../x"),
        format!("/v1/{tok}/%2e%2e"),
        "/v1/%2e%2e/thumb".to_owned(),
        format!("/v1/{tok}/thumb?a=1"),
        format!("/v1/{tok}/thumb#x"),
        format!("/v2/{tok}/thumb"),
        format!("/v1/{tok}/raw"),
        format!("v1/{tok}/thumb"),
        format!("//v1/{tok}/thumb"),
        "/v1//thumb".to_owned(),
        format!("/v1/{tok}"),
    ] {
        refuse(&target, &GET, &target);
    }
    refuse(&url(&"0".repeat(32), "thumb"), &GET, "unknown token");
    // Origin: absent or the app's own is fine; anything else is not.
    let app = crate::window::app_origin();
    assert_eq!(
        send(
            &r.host,
            &url(tok, "thumb"),
            &Ask {
                origins: &[app],
                ..GET
            }
        )
        .status,
        200
    );
    refuse(
        &url(tok, "thumb"),
        &Ask {
            origins: &["https://example.com"],
            ..GET
        },
        "foreign origin",
    );
    refuse(
        &url(tok, "thumb"),
        &Ask {
            origins: &[app, app],
            ..GET
        },
        "two origins",
    );
    // A malformed Range is a refusal on any rendition.
    refuse(
        &url(tok, "thumb"),
        &ranged(&["bytes=0-1,2-3"]),
        "multi-range",
    );
}

/// Tokens die with the session: after `close_viewer`, and after a reopen
/// (an archive switch or a reload), and the session's helpers end too.
#[test]
fn tokens_and_helpers_die_with_the_session() {
    let r = rig(Mode::Image, None);
    let images = r.classes().remove("image").expect("images");
    let (tok, _) = &images[0];
    assert_eq!(send(&r.host, &url(tok, "thumb"), &GET).status, 200);
    let old = r.session();
    assert!(r.counts.created.load(Ordering::SeqCst) >= 1);
    // A reopen replaces the session: the old token is unknown to the new one.
    let v2 = open(
        &r.host,
        &r.f.library,
        &env(),
        &Jobs::default(),
        r.f.id.as_str(),
    )
    .expect("reopen")
    .viewer;
    assert_ne!(v2, r.v);
    assert_refused(&now(&ask(&r.host, &url(tok, "thumb"), &GET)), "stale token");
    old.media().join();
    assert_eq!(
        r.counts.created.load(Ordering::SeqCst),
        r.counts.dropped.load(Ordering::SeqCst),
        "every helper of the old session ended"
    );
    // The new session mints its own tokens.
    let s2 = r.host.get(v2).expect("session");
    let mut fresh = Vec::new();
    for page in walk_threads(&s2, v2, false) {
        for m in &page.items {
            fresh.extend(m.attachments.iter().filter_map(|a| a.token.clone()));
        }
    }
    assert!(!fresh.is_empty());
    assert!(fresh.iter().all(|t| !r.tokens.contains_key(t.as_str())));
    // Closed: nothing is served.
    r.host.close(v2).expect("close");
    for t in &fresh {
        assert_refused(
            &now(&ask(&r.host, &url(t.as_str(), "thumb"), &GET)),
            "closed",
        );
    }
}

/// Renditions: from the decode executor, then from the LRU without any
/// queue; `HEAD` gets headers only; a rendition is served only for an
/// inspected image, never `stream` for an image or `thumb` for media.
#[test]
fn renditions_are_decoded_once_and_served_from_memory() {
    let r = rig(Mode::Image, None);
    // A token whose file was never inspected serves nothing.
    let (first, first_id) = r
        .tokens
        .iter()
        .next()
        .map(|(t, i)| (t.clone(), *i))
        .expect("a token");
    assert_refused(
        &now(&ask(&r.host, &url(&first, "thumb"), &GET)),
        "not inspected",
    );
    let classes = r.classes();
    let images = &classes["image"];
    let playable = &classes["playable"];
    let (tok, id) = &images[0];
    let before = r.inspect(*id, false);
    assert_eq!(before.original_size, None, "unknown before a rendition");
    assert!(!before.previews_off);
    let got = send(&r.host, &url(tok, "thumb"), &GET);
    assert_eq!(got.status, 200);
    assert_eq!(got.body, FAKE_JPEG);
    assert_eq!(got.header("Content-Type"), Some("image/jpeg"));
    assert_eq!(got.header("Content-Length"), Some("4"));
    assert_hardened(&got);
    assert_eq!(r.counts.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        r.inspect(*id, false).original_size,
        Some(ImageSizeDto {
            width: 6,
            height: 4
        })
    );
    // An LRU hit is answered before dispatch returns: no queue.
    let hit = now(&ask(&r.host, &url(tok, "thumb"), &GET));
    assert_eq!(hit.status, 200);
    assert_eq!(hit.body, FAKE_JPEG);
    assert_eq!(
        r.counts.calls.load(Ordering::SeqCst),
        1,
        "not decoded again"
    );
    let head = now(&ask(
        &r.host,
        &url(tok, "thumb"),
        &Ask {
            method: "HEAD",
            ..GET
        },
    ));
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    assert_eq!(head.header("Content-Length"), Some("4"));
    assert_eq!(send(&r.host, &url(tok, "display"), &GET).status, 200);
    assert_eq!(
        r.counts.calls.load(Ordering::SeqCst),
        2,
        "a second rendition"
    );
    // Wrong rendition for the kind.
    assert_refused(
        &now(&ask(&r.host, &url(tok, "stream"), &ranged(&["bytes=0-0"]))),
        "stream of an image",
    );
    let (media_tok, _) = &playable[0];
    assert_refused(
        &now(&ask(&r.host, &url(media_tok, "thumb"), &GET)),
        "thumb of media",
    );
    let _ = first_id;
}

/// The privacy screen (`hide_viewer`, slice 5j) drops the cached
/// renditions, so a rendition asked for again is decoded again; a refused
/// one stays refused, and the tokens keep working.
#[test]
fn hiding_drops_cached_renditions_but_keeps_failures() {
    let r = rig(Mode::Image, None);
    let images = r.classes().remove("image").expect("images");
    let (tok, _) = &images[0];
    assert_eq!(send(&r.host, &url(tok, "thumb"), &GET).status, 200);
    assert_eq!(r.session().media().cached_renditions(), 1);
    crate::viewer::hide(&r.session()).expect("hide");
    assert_eq!(r.session().media().cached_renditions(), 0);
    let again = send(&r.host, &url(tok, "thumb"), &GET);
    assert_eq!(again.status, 200);
    assert_eq!(again.body, FAKE_JPEG);
    assert_eq!(r.counts.calls.load(Ordering::SeqCst), 2, "decoded again");

    let refused = rig(Mode::Refuse, None);
    let images = refused.classes().remove("image").expect("images");
    let (tok, _) = &images[0];
    assert_refused(
        &send(&refused.host, &url(tok, "thumb"), &GET),
        "refused decode",
    );
    crate::viewer::hide(&refused.session()).expect("hide");
    assert_refused(
        &now(&ask(&refused.host, &url(tok, "thumb"), &GET)),
        "still remembered",
    );
    assert_eq!(refused.counts.calls.load(Ordering::SeqCst), 1);
}

/// A refused decode is remembered: a hostile image is decoded at most
/// once per session.
#[test]
fn failures_are_remembered_per_rendition() {
    let r = rig(Mode::Refuse, None);
    let images = r.classes().remove("image").expect("images");
    let (tok, _) = &images[0];
    assert_refused(&send(&r.host, &url(tok, "thumb"), &GET), "refused decode");
    assert_refused(&now(&ask(&r.host, &url(tok, "thumb"), &GET)), "remembered");
    assert_eq!(r.counts.calls.load(Ordering::SeqCst), 1);
}

/// Five helper crashes turn previews off for the session.
#[test]
fn five_crashes_turn_previews_off() {
    let r = rig(Mode::Crash, None);
    let images = r.classes().remove("image").expect("images");
    let s = r.session();
    'outer: for (tok, _) in &images {
        for rendition in ["thumb", "display"] {
            assert_refused(&send(&r.host, &url(tok, rendition), &GET), "crashed");
            if s.media().crashes() >= 5 {
                break 'outer;
            }
        }
    }
    assert_eq!(s.media().crashes(), 5, "enough images for five crashes");
    assert!(s.media().previews_off());
    let calls = r.counts.calls.load(Ordering::SeqCst);
    for (tok, id) in &images {
        assert_refused(
            &now(&ask(&r.host, &url(tok, "thumb"), &GET)),
            "previews off",
        );
        assert!(r.inspect(*id, false).previews_off);
    }
    assert_eq!(
        r.counts.calls.load(Ordering::SeqCst),
        calls,
        "no decode once off"
    );
}

/// The handler never blocks (deterministic: the backend waits at a gate
/// the test opens), and a full decode queue drops its oldest entry with
/// `503`.
/// Two requests for the same rendition at once are decoded once: the
/// second waits for the first's answer, so a hostile image crashes the
/// helper once, not once per worker (§5.8).
#[test]
fn concurrent_requests_for_one_rendition_decode_once() {
    let gate = Arc::new(Gate::default());
    let r = rig(Mode::Crash, Some(Arc::clone(&gate)));
    let s = r.session();
    let images = r.classes().remove("image").expect("images");
    let (tok, _) = &images[0];
    let first = ask(&r.host, &url(tok, "thumb"), &GET);
    gate.wait_entered(1);
    let second = ask(&r.host, &url(tok, "thumb"), &GET);
    // The second job leaves the queue (a free worker takes it) before the
    // first decode is let go.
    let until = std::time::Instant::now() + WAIT;
    while s.media().decode_queued() > 0 {
        assert!(
            std::time::Instant::now() < until,
            "the second job was taken"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(50));
    gate.release();
    assert_refused(&wait(&first), "crashed");
    assert_refused(&wait(&second), "the same answer");
    assert_eq!(r.counts.calls.load(Ordering::SeqCst), 1, "decoded once");
    assert_eq!(s.media().crashes(), 1, "one crash counted");
}

#[test]
fn dispatch_never_blocks_and_a_full_queue_drops_the_oldest() {
    let gate = Arc::new(Gate::default());
    let r = rig(Mode::Image, Some(Arc::clone(&gate)));
    let images = r.classes().remove("image").expect("images");
    let (tok, _) = &images[0];
    let s = r.session();
    // Both decode workers take a job and wait at the gate (two different
    // renditions: a second request for one being decoded would wait for
    // it instead of taking a worker).
    let busy: Vec<_> = ["thumb", "display"]
        .iter()
        .map(|rendition| ask(&r.host, &url(tok, rendition), &GET))
        .collect();
    gate.wait_entered(2);
    // dispatch has returned for both, and neither is answered yet.
    for rx in &busy {
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }
    let queued: Vec<_> = (0..DECODE_QUEUE)
        .map(|_| ask(&r.host, &url(tok, "thumb"), &GET))
        .collect();
    assert_eq!(s.media().decode_queued(), DECODE_QUEUE);
    for rx in &queued {
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }
    // One more: the oldest queued entry is dropped with 503, at once.
    let newest = ask(&r.host, &url(tok, "thumb"), &GET);
    let dropped = now(&queued[0]);
    assert_eq!(dropped.status, 503);
    assert!(dropped.body.is_empty());
    assert_hardened(&dropped);
    gate.release();
    for rx in busy
        .iter()
        .chain(queued.iter().skip(1))
        .chain(std::iter::once(&newest))
    {
        assert_eq!(wait(rx).status, 200);
    }
}

/// Verification never occupies a decode or stream worker, and its queue
/// is bounded (one running, four waiting).
#[test]
fn verify_work_is_separate_and_bounded() {
    let r = rig(Mode::Image, None);
    let classes = r.classes();
    let (img, _) = &classes["image"][0];
    let (media, media_id) = classes["playable"]
        .iter()
        .find(|(_, id)| {
            matches!(
                r.inspect(*id, true).playback,
                Some(PlaybackDto::Allowed { .. })
            )
        })
        .cloned()
        .expect("a file that plays");
    let s = r.session();
    {
        // A long verification holds the verify reader...
        let _verifying = s.verify_reader().expect("verify reader");
        // ...while thumbnails and stream chunks are still served.
        assert_eq!(send(&r.host, &url(img, "thumb"), &GET).status, 200);
        assert_eq!(
            send(&r.host, &url(&media, "stream"), &ranged(&["bytes=0-0"])).status,
            206
        );
    }
    let slots: Vec<_> = (0..=VERIFY_QUEUE)
        .map(|_| s.verify_slot().expect("slot"))
        .collect();
    let busy = s.verify_slot().map(|_| ()).expect_err("full");
    assert_eq!(busy.code, crate::error::ErrorCode::Busy);
    drop(slots);
    assert!(s.verify_slot().is_ok());
    let _ = media_id;
}

/// `stream`: only after the full hash and the structural check passed in
/// this session, with playback on; strict single ranges; `206` chunks;
/// `416` with `*/len`; `200` whole only without `Range`.
#[test]
fn streams_follow_the_range_rules() {
    let r = rig(Mode::Image, None);
    let classes = r.classes();
    let s = r.session();
    let mut allowed = None;
    let mut refused = None;
    for (tok, id) in &classes["playable"] {
        // Before verification: nothing.
        assert_refused(
            &now(&ask(&r.host, &url(tok, "stream"), &ranged(&["bytes=0-0"]))),
            "unverified",
        );
        // Hashed but not structurally checked: still nothing.
        let blob = r.blob(*id);
        let ident = s
            .verify_reader()
            .expect("reader")
            .verify_blob(blob, &CancelToken::new())
            .expect("verify");
        s.set_verified(ident).expect("verified");
        assert_refused(
            &now(&ask(&r.host, &url(tok, "stream"), &ranged(&["bytes=0-0"]))),
            "before the structural check",
        );
        match r.inspect(*id, true).playback {
            Some(PlaybackDto::Allowed { .. }) if allowed.is_none() => {
                allowed = Some((tok.clone(), *id))
            }
            Some(PlaybackDto::Refused { .. }) if refused.is_none() => refused = Some(tok.clone()),
            _ => {}
        }
    }
    let (tok, id) = allowed.expect("a file that plays");
    let refused = refused.expect("a file the structural check refuses");
    assert_refused(
        &now(&ask(
            &r.host,
            &url(&refused, "stream"),
            &ranged(&["bytes=0-0"]),
        )),
        "refused structure",
    );
    let all = r.bytes(id);
    let len = all.len() as u64;
    assert!(len > 2 && len < CHUNK_BYTES);
    let chunk = |range: &str| send(&r.host, &url(&tok, "stream"), &ranged(&[range]));
    let first = chunk("bytes=0-0");
    assert_eq!(first.status, 206);
    assert_eq!(first.body, all[..1]);
    assert_eq!(
        first.header("Content-Range"),
        Some(format!("bytes 0-0/{len}").as_str())
    );
    assert_eq!(first.header("Content-Length"), Some("1"));
    assert_eq!(first.header("Accept-Ranges"), Some("bytes"));
    assert!(first
        .header("Content-Type")
        .is_some_and(|t| t.starts_with("video/") || t.starts_with("audio/")));
    assert_hardened(&first);
    let open_ended = chunk("bytes=0-");
    assert_eq!((open_ended.status, &open_ended.body), (206, &all));
    let last = chunk("bytes=-1");
    assert_eq!(last.body, all[all.len() - 1..]);
    let long_suffix = chunk(&format!("bytes=-{}", len + 100));
    assert_eq!(long_suffix.body, all);
    assert_eq!(
        long_suffix.header("Content-Range"),
        Some(format!("bytes 0-{}/{len}", len - 1).as_str())
    );
    for bad in ["bytes=-0", "bytes=5-2", &format!("bytes={len}-")] {
        let rx = ask(&r.host, &url(&tok, "stream"), &ranged(&[bad]));
        let got = now(&rx);
        assert_eq!(got.status, 416, "{bad}");
        assert_eq!(
            got.header("Content-Range"),
            Some(format!("bytes */{len}").as_str())
        );
        assert!(got.body.is_empty());
    }
    for bad in [
        "bytes=0-18446744073709551616",
        "bytes= 0-1",
        "bytes=0 -1",
        "bytes=0-1,2-3",
        "bytes",
        "bytes=x-1",
    ] {
        assert_refused(
            &now(&ask(&r.host, &url(&tok, "stream"), &ranged(&[bad]))),
            bad,
        );
    }
    assert_refused(
        &now(&ask(
            &r.host,
            &url(&tok, "stream"),
            &ranged(&["bytes=0-0", "bytes=0-0"]),
        )),
        "a second Range header",
    );
    // Without Range a small file is served whole, with 200 (never 206).
    let whole = send(&r.host, &url(&tok, "stream"), &GET);
    assert_eq!((whole.status, &whole.body), (200, &all));
    assert!(whole.header("Content-Range").is_none());
    // HEAD: the headers of the chunk, no body.
    let head = send(
        &r.host,
        &url(&tok, "stream"),
        &Ask {
            method: "HEAD",
            ranges: &["bytes=0-0"],
            ..GET
        },
    );
    assert_eq!(head.status, 206);
    assert!(head.body.is_empty());
    assert_eq!(head.header("Content-Length"), Some("1"));
    // Playback turned off: refused even for a checked file.
    assert_refused(
        &now(&ask(
            &r.host,
            &url(&tok, "stream"),
            &Ask {
                playback: false,
                ranges: &["bytes=0-0"],
                ..GET
            },
        )),
        "playback off",
    );
}

/// The arithmetic on lengths no fixture has: over 8 MiB without `Range`
/// is refused, and a 3 GiB file near its end is chunked exactly.
#[test]
fn stream_plans_for_large_files() {
    assert_eq!(stream_plan(None, WHOLE_LIMIT), StreamPlan::Whole);
    assert_eq!(stream_plan(None, WHOLE_LIMIT + 1), StreamPlan::Refused);
    let len: u64 = 3 << 30;
    let plan = stream_plan(Some(range::RangeSpec::From(len - 100)), len);
    assert_eq!(
        plan,
        StreamPlan::Partial {
            start: len - 100,
            end: len - 1
        }
    );
    let r = MediaResponse::partial("video/mp4", len - 100, len - 1, len, vec![0; 100]);
    assert_eq!(
        r.header("Content-Range"),
        Some("bytes 3221225372-3221225471/3221225472")
    );
    assert_eq!(
        stream_plan(Some(range::RangeSpec::From(0)), len),
        StreamPlan::Partial {
            start: 0,
            end: CHUNK_BYTES - 1
        }
    );
    assert_eq!(
        stream_plan(Some(range::RangeSpec::From(len)), len),
        StreamPlan::Unsatisfiable
    );
    assert_eq!(size_bucket(0), "<64KiB");
    assert_eq!(size_bucket(65_535), "<64KiB");
    assert_eq!(size_bucket(65_536), "<1MiB");
    assert_eq!(size_bucket(1 << 20), "<16MiB");
    assert_eq!(size_bucket(16 << 20), ">=16MiB");
}

/// A reply dropped unanswered (a closed queue) answers `503` itself.
#[test]
fn an_unanswered_reply_answers_busy() {
    let (r, rx) = reply();
    drop(r);
    assert_eq!(now(&rx).status, 503);
}
