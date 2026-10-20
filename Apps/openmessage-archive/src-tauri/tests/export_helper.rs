//! The export-render helper as a real process (M6 spec §5.7, §14.2): the
//! app's own binary, built by Cargo, started with `--oma-export-helper`
//! and driven through `oma_export::helper::RenderSession` with the debug
//! build's test backend (`oma_export::render::stub`), which plays one
//! fault per session.
//!
//! Covered: a session's round trip (resources over several frames,
//! records, images, two volumes, an output of several chunks), XLSX
//! refused by this build, the real PDF backend (slice 6j1) fed the pinned
//! fonts and its refusals (fonts off their pins, no options), a crash, a hang (the reply timeout), a
//! helper that stops reading while the worker is blocked writing (the
//! stall timeout, the supervisor's cancel and the job's cancel hook), an
//! oversized frame, a flood of `Error` frames while the worker is blocked
//! writing (never queued: memory stays bounded), garbage, an early write,
//! a `Done` that disagrees with
//! its chunks, an output over the volume bound, a planted `/JavaScript`
//! (refused by the main process's check), a full disk reported by the
//! helper and met by the main process's write, a panic whose payload never
//! reaches stderr, and stderr counted, never kept.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use oma_archive::CancelToken;
use oma_export::helper::frame::{self, BackendKind, Begin, FrameKind, HelperErrorCode};
use oma_export::helper::{HelperLaunch, RenderSession, SessionLimits};
use oma_export::hidden::MarkCounts;
use oma_export::pdf::doc::{
    Block, HanFace, HeadingLevel, PageSize, Para, PdfOptions, PdfText, TextBuilder,
};
use oma_export::render::stub::{Fault, PANIC_SENTINEL};
use oma_export::{ExportError, HelperFailure, LimitKind};
use openmessage_archive_lib::backups::dto::JobKind;
use openmessage_archive_lib::backups::{Jobs, LastOutcome};
use openmessage_archive_lib::exports::pdf_fonts;
use openmessage_archive_lib::exports::render_helper::{register_kill, EXPORT_HELPER_FLAG};

const BIN: &str = env!("CARGO_BIN_EXE_openmessage-archive");
/// A record the main process would send (content the stub never writes).
const RECORD: &[u8] = br#"{"kind":"message","text":"SENTINEL-EXPORT-HELPER-BODY"}"#;

fn launch() -> HelperLaunch {
    HelperLaunch::new(PathBuf::from(BIN), vec![OsString::from(EXPORT_HELPER_FLAG)])
}

fn quick() -> SessionLimits {
    SessionLimits {
        reply_timeout: Duration::from_secs(2),
        stall_timeout: Duration::from_secs(2),
        ..SessionLimits::default()
    }
}

fn stub(fault: Fault, arg: u64) -> Begin {
    Begin {
        backend: BackendKind::Stub,
        options: fault.options(arg),
    }
}

fn start(fault: Fault, arg: u64, limits: SessionLimits) -> RenderSession {
    RenderSession::start(&launch(), &stub(fault, arg), &CancelToken::new(), limits)
        .expect("start the helper")
}

/// Send a conversation and `items` items, then end the volume; the first
/// error of the sequence.
fn run(s: &mut RenderSession, items: usize) -> Result<(), ExportError> {
    s.conversation(RECORD)?;
    for _ in 0..items {
        s.item(RECORD)?;
    }
    s.end_volume(true).map(|_| ())
}

#[test]
fn a_session_round_trips_records_resources_and_volumes() {
    let mut s = start(Fault::None, 0, SessionLimits::default());
    // A resource larger than one frame goes in pieces.
    let font = vec![0xA5u8; (frame::MAX_FRAME_BYTES as usize) + 4096];
    s.resource(0, &font).expect("resource");
    s.resource(1, b"small").expect("resource");
    s.conversation(RECORD).expect("conversation");
    for _ in 0..3 {
        s.item(RECORD).expect("item");
    }
    s.image(1, b"\x89PNG\r\n\x1a\n").expect("image");
    let v1 = s.end_volume(false).expect("volume 1");
    assert_eq!(v1.counts.units, 1);
    assert_eq!(v1.counts.total_bytes, v1.bytes.len() as u64);
    assert!(v1.bytes.starts_with(b"%PDF-1.7"));
    s.item(RECORD).expect("item in volume 2");
    let v2 = s.end_volume(true).expect("volume 2");
    assert_eq!(oma_export::check::pdf::check(&v2.bytes), Ok(()));
    // The helper holds no record text in its output.
    for v in [&v1, &v2] {
        assert!(!v.bytes.windows(8).any(|w| w == b"SENTINEL"));
    }
    assert_eq!(s.finish(), 0);
}

#[test]
fn an_output_of_several_chunks_is_reassembled_and_bounded() {
    let mut s = start(Fault::Pad, 3 << 20, SessionLimits::default());
    let v = s.end_volume(true).expect("volume");
    assert!(v.bytes.len() > 3 << 20);
    assert_eq!(oma_export::check::pdf::check(&v.bytes), Ok(()));
    s.finish();

    let small = SessionLimits {
        max_volume_bytes: 1 << 20,
        ..quick()
    };
    let mut s = start(Fault::Pad, 3 << 20, small);
    assert!(matches!(
        s.end_volume(true),
        Err(ExportError::HelperFailed(HelperFailure::OutputTooLarge))
    ));
    // Every later call repeats the failure.
    assert!(matches!(
        s.item(RECORD),
        Err(ExportError::HelperFailed(HelperFailure::OutputTooLarge))
    ));
}

#[test]
fn xlsx_is_refused_by_this_build() {
    let begin = Begin {
        backend: BackendKind::Xlsx,
        options: Vec::new(),
    };
    let r = RenderSession::start(&launch(), &begin, &CancelToken::new(), quick())
        .and_then(|mut s| run(&mut s, 1));
    assert!(
        matches!(
            r,
            Err(ExportError::HelperFailed(HelperFailure::Refused(
                HelperErrorCode::Unsupported
            )))
        ),
        "{r:?}"
    );
}

fn pdf_begin() -> Begin {
    Begin {
        backend: BackendKind::Pdf,
        options: PdfOptions {
            page: PageSize::A4,
            han: HanFace::Simplified,
            title: "Messages export \u{2014} conversation 1".into(),
        }
        .encode(),
    }
}

fn fonts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources")
        .join("fonts")
}

/// Slice 6j1: the real PDF backend in the real helper process, fed the
/// pinned fonts through `send_fonts` (only checked fonts can be sent).
#[test]
fn the_pdf_backend_makes_a_checked_pdf_in_the_helper_process() {
    let fonts = pdf_fonts::load(&fonts_dir()).expect("the pinned fonts");
    let mut s = RenderSession::start(
        &launch(),
        &pdf_begin(),
        &CancelToken::new(),
        SessionLimits::default(),
    )
    .expect("start");
    s.send_fonts(&fonts).expect("fonts");
    let heading = Block::Heading {
        level: HeadingLevel::Conversation,
        text: PdfText::ours("Conversation 1"),
        title: None,
    };
    s.conversation(&heading.encode()).expect("conversation");
    let mut n = MarkCounts::default();
    let (sender, _) = TextBuilder::new().header("Alice").build();
    let entry = Block::Entry {
        head: vec![PdfText::ours("[2024-03-10 14:02:11 +01:00]"), sender],
        body: Para::recorded(
            2,
            "hello \u{05E9}\u{05DC}\u{05D5}\u{05DD} \u{1F600}",
            true,
            &mut n,
        ),
    };
    for _ in 0..200 {
        s.item(&entry.encode()).expect("item");
    }
    let v = s
        .end_volume(true)
        .expect("a PDF that passed the output check");
    assert!(v.bytes.starts_with(b"%PDF-1.7"));
    assert!(v.counts.units >= 2, "{:?}", v.counts);
    assert_eq!(v.counts.notdef_chars, 0);
    assert_eq!(s.finish(), 0);

    // Fonts that do not match their pins are refused by the helper too
    // (sent here through the raw resource path the app never uses).
    let mut s =
        RenderSession::start(&launch(), &pdf_begin(), &CancelToken::new(), quick()).expect("start");
    for (slot, bytes) in fonts.slots().iter().enumerate() {
        let mut b = bytes.to_vec();
        if slot == 0 {
            b[500] ^= 1;
        }
        s.resource(u8::try_from(slot).expect("slot"), &b)
            .expect("resource");
    }
    let r = s
        .conversation(&heading.encode())
        .and_then(|()| s.end_volume(true).map(|_| ()));
    assert!(
        matches!(
            r,
            Err(ExportError::HelperFailed(HelperFailure::Refused(
                HelperErrorCode::BadRequest
            )))
        ),
        "{r:?}"
    );
    // A PDF session without its options is refused.
    let begin = Begin {
        backend: BackendKind::Pdf,
        options: Vec::new(),
    };
    let r = RenderSession::start(&launch(), &begin, &CancelToken::new(), quick())
        .and_then(|mut s| run(&mut s, 1));
    assert!(
        matches!(
            r,
            Err(ExportError::HelperFailed(HelperFailure::Refused(
                HelperErrorCode::BadRequest
            )))
        ),
        "{r:?}"
    );
}

#[test]
fn a_crash_fails_the_session() {
    let mut s = start(Fault::CrashAtItem, 2, quick());
    let r = run(&mut s, 5);
    assert!(
        matches!(r, Err(ExportError::HelperFailed(HelperFailure::Crashed))),
        "{r:?}"
    );
    let mut s = start(Fault::ExitAtEnd, 0, quick());
    let r = run(&mut s, 1);
    assert!(
        matches!(r, Err(ExportError::HelperFailed(HelperFailure::Crashed))),
        "{r:?}"
    );
}

#[test]
fn a_helper_that_never_answers_is_killed_after_the_reply_timeout() {
    let mut s = start(Fault::HangAtEnd, 0, quick());
    let t = Instant::now();
    let r = run(&mut s, 1);
    assert!(
        matches!(r, Err(ExportError::HelperFailed(HelperFailure::Timeout))),
        "{r:?}"
    );
    let took = t.elapsed();
    assert!(
        took >= Duration::from_secs(2) && took < Duration::from_secs(20),
        "{took:?}"
    );
}

/// Images large enough to fill the stdin pipe at once, so the worker's
/// write blocks while the helper sleeps without reading.
fn flood(s: &mut RenderSession) -> Result<(), ExportError> {
    let big = vec![0u8; 8 << 20];
    s.conversation(RECORD)?;
    s.item(RECORD)?;
    for n in 0..64 {
        s.image(n, &big)?;
    }
    s.end_volume(true).map(|_| ())
}

#[test]
fn a_blocked_write_is_ended_by_the_stall_timeout() {
    let mut s = start(Fault::StopReading, 0, quick());
    let t = Instant::now();
    let r = flood(&mut s);
    assert!(
        matches!(r, Err(ExportError::HelperFailed(HelperFailure::Timeout))),
        "{r:?}"
    );
    assert!(t.elapsed() < Duration::from_secs(20));
}

#[test]
fn cancel_kills_a_helper_the_worker_is_blocked_on() {
    // Through the token: the supervisor kills, the worker never reaches a
    // check of its own.
    let cancel = CancelToken::new();
    let slow = SessionLimits::default();
    let mut s = RenderSession::start(&launch(), &stub(Fault::StopReading, 0), &cancel, slow)
        .expect("start");
    let c = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        c.cancel();
        Instant::now()
    });
    let r = flood(&mut s);
    let ended = Instant::now();
    let cancelled_at = canceller.join().expect("join");
    assert!(matches!(r, Err(ExportError::Cancelled)), "{r:?}");
    assert!(ended.duration_since(cancelled_at) < Duration::from_secs(2));

    // Through the job's cancel path: `Jobs::cancel` runs the kill itself.
    let jobs = Jobs::default();
    let job = jobs.reserve("export-test", JobKind::Validate).expect("job");
    let mut s = RenderSession::start(
        &launch(),
        &stub(Fault::StopReading, 0),
        &CancelToken::new(),
        slow,
    )
    .expect("start");
    assert!(register_kill(&jobs, &job.job_id, s.kill_handle()));
    let worker = std::thread::spawn(move || {
        let r = flood(&mut s);
        (r, Instant::now())
    });
    std::thread::sleep(Duration::from_millis(300));
    let at = Instant::now();
    assert!(jobs.cancel(&job.job_id));
    let (r, ended) = worker.join().expect("join");
    assert!(matches!(r, Err(ExportError::Cancelled)), "{r:?}");
    assert!(ended.duration_since(at) < Duration::from_secs(2));
    jobs.finish(&job.job_id, LastOutcome::default());
}

/// The process's peak resident set in bytes (Linux only).
#[cfg(target_os = "linux")]
fn peak_rss() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmHWM:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        })
        .map_or(0, |kb| kb * 1024)
}

/// Review finding: a helper that stops reading and floods `Error` frames
/// while the worker is blocked writing. The reader stops at the first
/// one (or refuses an `Error` longer than its one byte before allocating
/// it), so the main process's memory stays bounded and the session ends
/// well before the stall timeout.
#[test]
fn a_flood_of_error_frames_is_not_queued() {
    #[cfg(target_os = "linux")]
    let before = peak_rss();
    let t = Instant::now();
    // 16 MiB frames: refused at the header, a protocol failure.
    let mut s = start(Fault::FloodErrors, 0, quick());
    let r = flood(&mut s);
    assert!(
        matches!(r, Err(ExportError::HelperFailed(HelperFailure::Protocol))),
        "{r:?}"
    );
    // Valid one-byte frames: the first is the session's answer.
    let mut s = start(Fault::FloodErrors, 1, quick());
    let r = flood(&mut s);
    assert!(
        matches!(
            r,
            Err(ExportError::HelperFailed(HelperFailure::Refused(
                HelperErrorCode::BadRequest
            )))
        ),
        "{r:?}"
    );
    assert!(t.elapsed() < Duration::from_secs(20), "{:?}", t.elapsed());
    #[cfg(target_os = "linux")]
    {
        let grew = peak_rss().saturating_sub(before);
        assert!(grew < 1 << 30, "peak resident set grew by {grew} bytes");
    }
}

#[test]
fn malformed_replies_kill_the_helper() {
    let mut s = start(Fault::OversizeAtEnd, 0, quick());
    let r = run(&mut s, 1);
    assert!(
        matches!(r, Err(ExportError::Limit(LimitKind::FrameTooLarge))),
        "{r:?}"
    );
    for fault in [Fault::GarbageAtEnd, Fault::WrongTotal] {
        let mut s = start(fault, 0, quick());
        let r = run(&mut s, 1);
        assert!(
            matches!(r, Err(ExportError::HelperFailed(HelperFailure::Protocol))),
            "{fault:?}: {r:?}"
        );
    }
    // A chunk nobody asked for, written while the worker still sends.
    let mut s = start(Fault::WriteEarly, 0, quick());
    let r = run(&mut s, 50);
    assert!(
        matches!(r, Err(ExportError::HelperFailed(HelperFailure::Protocol))),
        "{r:?}"
    );
}

#[test]
fn a_planted_name_is_refused_before_any_byte_is_returned() {
    let mut s = start(Fault::PlantJavaScript, 0, quick());
    let mut sink = Vec::new();
    let r = s
        .conversation(RECORD)
        .and_then(|()| s.end_volume_into(&mut sink, false));
    assert!(matches!(r, Err(ExportError::HelperOutputRejected)), "{r:?}");
    assert!(sink.is_empty());
    assert!(matches!(
        s.item(RECORD),
        Err(ExportError::HelperOutputRejected)
    ));
}

/// A writer that is out of space.
struct Full;

impl Write for Full {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::StorageFull))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_full_disk_is_no_space() {
    // Reported by the helper (its own writes, as XLSX's temporary files
    // would be).
    let mut s = start(Fault::NoSpaceAtEnd, 0, quick());
    let r = run(&mut s, 1);
    assert!(matches!(r, Err(ExportError::NoSpace)), "{r:?}");
    // Met by the main process writing the checked output.
    let mut s = start(Fault::None, 0, quick());
    s.conversation(RECORD).expect("conversation");
    let r = s.end_volume_into(&mut Full, true);
    assert!(matches!(r, Err(ExportError::NoSpace)), "{r:?}");
    s.finish();
    #[cfg(target_os = "linux")]
    {
        // A real ENOSPC from the kernel.
        #[allow(clippy::disallowed_methods)]
        let mut full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .expect("/dev/full");
        let mut s = start(Fault::None, 0, quick());
        let r = s.end_volume_into(&mut full, true);
        assert!(matches!(r, Err(ExportError::NoSpace)), "{r:?}");
    }
}

#[test]
fn stderr_is_counted_never_kept() {
    let mut s = start(Fault::Stderr, 5000, quick());
    s.end_volume(true).expect("volume");
    assert!(s.finish() >= 5000);
}

#[test]
fn a_panic_ends_the_session_and_its_payload_never_reaches_stderr() {
    let mut s = start(Fault::PanicAtItem, 0, quick());
    let r = run(&mut s, 3);
    assert!(
        matches!(r, Err(ExportError::HelperFailed(HelperFailure::Crashed))),
        "{r:?}"
    );
    // The same panic, with the helper's stderr read directly.
    let mut child = Command::new(BIN)
        .arg(EXPORT_HELPER_FLAG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut input = child.stdin.take().expect("stdin");
    frame::write_frame(
        &mut input,
        FrameKind::Begin,
        &stub(Fault::PanicAtItem, 0).encode(),
    )
    .expect("begin");
    frame::write_frame(&mut input, FrameKind::Item, RECORD).expect("item");
    drop(input);
    let mut err = String::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut err)
        .expect("read stderr");
    let status = child.wait().expect("wait");
    assert!(!status.success());
    assert!(err.contains("panic in thread"), "{err}");
    assert!(!err.contains(PANIC_SENTINEL), "the payload leaked: {err}");
    assert!(!err.contains("SENTINEL-EXPORT-HELPER-BODY"));
}

#[test]
fn an_oversized_request_is_refused_before_it_is_sent() {
    let mut s = start(Fault::None, 0, quick());
    let big = vec![0u8; frame::MAX_FRAME_BYTES as usize];
    assert!(matches!(
        s.image(1, &big),
        Err(ExportError::Limit(LimitKind::FrameTooLarge))
    ));
    assert!(matches!(
        s.item(&big[..1])
            .and_then(|()| s.item(&vec![0u8; frame::MAX_FRAME_BYTES as usize + 1])),
        Err(ExportError::Limit(LimitKind::FrameTooLarge))
    ));
    // The session is unharmed: nothing was written.
    s.conversation(RECORD).expect("conversation");
    s.end_volume(true).expect("volume");
    s.finish();
}

/// The helper itself refuses an oversized request header without reading
/// or allocating its payload, and exits.
#[test]
fn the_helper_exits_on_an_oversized_request() {
    let mut child = Command::new(BIN)
        .arg(EXPORT_HELPER_FLAG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let mut input = child.stdin.take().expect("stdin");
    input
        .write_all(&frame::raw_header(FrameKind::Begin, u32::MAX))
        .expect("header");
    let mut out = Vec::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_end(&mut out)
        .expect("read");
    let status = child.wait().expect("wait");
    drop(input);
    assert_eq!(status.code(), Some(oma_export::render::EXIT_PROTOCOL));
    assert!(out.is_empty());
}

/// A real 1 × 1 PNG (every chunk's CRC is right).
const PNG_1X1: &str = "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d4944415478da636460f85f0f0002870180eb47ba920000000049454e44ae426082";

/// Slice 6j2: the PDF writer's renderers (`HelperRenderers`) through the
/// real helper: the kill handle is handed over at start, a preview image
/// is drawn as a tagged figure, a volume's document title comes from its
/// conversation heading, and a second volume follows the first in the
/// same process.
#[test]
fn the_pdf_writers_renderer_draws_previews_and_titles_volumes() {
    use oma_export::writer::pdf::{HelperRenderers, PdfRendererFactory};
    use std::sync::{Arc, Mutex};

    let fonts = pdf_fonts::load(&fonts_dir()).expect("the pinned fonts");
    let kills = Arc::new(Mutex::new(0usize));
    let k = Arc::clone(&kills);
    let mut factory = HelperRenderers::new(
        launch(),
        fonts,
        CancelToken::new(),
        Box::new(move |_kill| *k.lock().expect("lock") += 1),
    );
    let stderr = factory.stderr_counter();
    let options = PdfOptions {
        page: PageSize::Letter,
        han: HanFace::Simplified,
        title: "Messages export".into(),
    };
    let mut r = factory.start(&options).expect("start");
    assert_eq!(*kills.lock().expect("lock"), 1, "the kill is handed over");
    let png: Vec<u8> = (0..PNG_1X1.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&PNG_1X1[i..i + 2], 16).expect("hex"))
        .collect();
    for volume in 1..=2u32 {
        let heading = Block::Heading {
            level: HeadingLevel::Conversation,
            text: PdfText::ours("Conversation 1"),
            title: Some(format!(
                "Messages export \u{2014} conversation 1, part {volume}"
            )),
        };
        r.conversation(&heading.encode()).expect("conversation");
        r.image(7, &png).expect("image");
        let mut n = MarkCounts::default();
        let mut body = Para::recorded(2, "a preview follows", true, &mut n);
        body.push(Para::figure(2, 7));
        let entry = Block::Entry {
            head: vec![
                PdfText::ours("2024-03-10 14:02:11 +00:00"),
                PdfText::ours("item 1"),
            ],
            body,
        };
        r.item(&entry.encode()).expect("item");
        let v = r.end_volume(false).expect("a checked volume");
        assert_eq!(v.counts.figures, 1, "{:?}", v.counts);
        let has = |needle: &[u8]| v.bytes.windows(needle.len()).any(|w| w == needle);
        assert!(
            has(b"/StructTreeRoot") && has(b"/Figure"),
            "tagged, with a figure"
        );
        assert!(has(b"/Image"), "the preview is an image XObject");
    }
    // A figure whose image was never sent is refused.
    let mut n = MarkCounts::default();
    let mut body = Para::recorded(2, "x", true, &mut n);
    body.push(Para::figure(2, 99));
    let entry = Block::Entry {
        head: Vec::new(),
        body,
    };
    let refused = r
        .item(&entry.encode())
        .and_then(|()| r.end_volume(true).map(|_| ()));
    assert!(refused.is_err(), "{refused:?}");
    r.close();
    assert_eq!(stderr.load(std::sync::atomic::Ordering::Relaxed), 0);
}
