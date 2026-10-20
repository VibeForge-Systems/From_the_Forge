//! The main process's side of the export-render helper (M6 spec §5.7).
//!
//! A [`RenderSession`] is one helper process serving one output format of
//! one export:
//!
//! ```text
//! start(Begin)  resource*  (conversation | item | image)*  end_volume  …  finish
//! ```
//!
//! **Threads.** Each session has three helper threads besides the caller:
//!
//! - the **reader** reads reply frames from the helper's stdout as soon as
//!   they arrive, so the helper never blocks on a full stdout pipe while
//!   the caller is blocked writing to its stdin. A frame header over 16 MiB
//!   is refused before anything is allocated (`Limit(FrameTooLarge)`); a
//!   `Chunk` or `Done` before `EndVolume` (an early write), a wrong magic
//!   or a reply over [`SessionLimits::max_volume_bytes`] kills the helper
//!   at once, from the reader;
//! - the **supervisor** wakes every 10 ms: it kills the helper when the
//!   export's cancel token is set (the caller need not reach a check), when
//!   a write to stdin has been blocked for [`SessionLimits::stall_timeout`],
//!   or when the output has not arrived within
//!   [`SessionLimits::reply_timeout`] (120 s) of `EndVolume`;
//! - the **stderr** drain keeps only the byte count ([`RenderSession::stderr_bytes`]).
//!
//! A kill closes the pipes, so a caller blocked on a write or a read
//! returns, and the session's error says why: `Cancelled`, a
//! `HelperFailed` kind, or `Limit(FrameTooLarge)`. An `Error(NoSpace)`
//! from the helper is `NoSpace`. [`HelperKill`] lets the shell's cancel
//! path kill the helper directly too.
//!
//! **Output.** `end_volume` collects the chunks, checks that their sum is
//! the `Done` total, and runs the output check of the session's kind
//! ([`crate::check::check_output`]) **before** returning a byte; a refusal
//! kills the helper and is `HelperOutputRejected`. The caller writes the
//! checked bytes to a staged file ([`RenderSession::end_volume_into`]); a
//! full disk there is `NoSpace`.
//!
//! The helper is started with `CREATE_NO_WINDOW` on Windows. Dropping a
//! session kills and reaps its helper; nothing here waits for a helper
//! that does not answer.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oma_archive::CancelToken;

use super::frame::{
    self, BackendKind, Begin, Direction, DoneCounts, Frame, FrameError, FrameKind, HelperErrorCode,
    ResourcePiece, IMAGE_HEADER, MAX_FRAME_BYTES, RESOURCE_HEADER,
};
use crate::check::{check_output, OutputKind};
use crate::error::{ExportError, HelperFailure, LimitKind};

/// How long the output may take after `EndVolume` (spec §5.7).
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(120);
/// How long one write to the helper's stdin may stay blocked.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(120);
/// The largest output one volume may have.
pub const MAX_VOLUME_BYTES: u64 = 256 << 20;
/// How often the supervisor wakes.
const TICK: Duration = Duration::from_millis(10);
/// How long a helper may take to exit after its stdin closed.
const EXIT_GRACE: Duration = Duration::from_secs(5);
/// How long a failed session waits for the reader's last word.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// How to start a helper: the app's own binary with
/// `--oma-export-helper`, or, in tests, the binary Cargo built.
#[derive(Clone)]
pub struct HelperLaunch {
    program: PathBuf,
    args: Vec<OsString>,
}

impl std::fmt::Debug for HelperLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the program's path.
        f.write_str("HelperLaunch")
    }
}

impl HelperLaunch {
    pub fn new(program: PathBuf, args: Vec<OsString>) -> HelperLaunch {
        HelperLaunch { program, args }
    }

    fn spawn(&self) -> io::Result<Child> {
        let mut c = Command::new(&self.program);
        c.args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: no console window flashes up.
            c.creation_flags(0x0800_0000);
        }
        c.spawn()
    }
}

/// A session's limits (tests shorten the timeouts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLimits {
    pub reply_timeout: Duration,
    pub stall_timeout: Duration,
    pub max_volume_bytes: u64,
}

impl Default for SessionLimits {
    fn default() -> SessionLimits {
        SessionLimits {
            reply_timeout: REPLY_TIMEOUT,
            stall_timeout: STALL_TIMEOUT,
            max_volume_bytes: MAX_VOLUME_BYTES,
        }
    }
}

/// Why a helper was killed (the first reason wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Cancelled,
    Timeout,
    Oversize,
    Protocol,
    OutputTooLarge,
}

struct Shared {
    child: Mutex<Child>,
    stop: Mutex<Option<Stop>>,
    /// The caller sent `EndVolume` or `End` and waits for the output.
    awaiting: AtomicBool,
    /// When the supervisor kills the helper if nothing changes.
    deadline: Mutex<Option<Instant>>,
    stderr: AtomicU64,
}

impl Shared {
    fn kill(&self) {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
        }
    }

    /// Record `why` (unless a reason is already recorded) and kill.
    fn stop(&self, why: Stop) {
        if let Ok(mut s) = self.stop.lock() {
            if s.is_none() {
                *s = Some(why);
            }
        }
        self.kill();
    }

    fn reason(&self) -> Option<Stop> {
        self.stop.lock().ok().and_then(|s| *s)
    }

    fn arm(&self, after: Duration) {
        if let Ok(mut d) = self.deadline.lock() {
            *d = Instant::now().checked_add(after);
        }
    }

    fn disarm(&self) {
        if let Ok(mut d) = self.deadline.lock() {
            *d = None;
        }
    }

    fn overdue(&self) -> bool {
        self.deadline
            .lock()
            .ok()
            .and_then(|d| *d)
            .is_some_and(|d| Instant::now() >= d)
    }
}

/// Kills a session's helper from another thread (the shell's cancel
/// path). The session then ends with `Cancelled`.
#[derive(Clone)]
pub struct HelperKill(Arc<Shared>);

impl std::fmt::Debug for HelperKill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HelperKill")
    }
}

impl HelperKill {
    pub fn kill(&self) {
        self.0.stop(Stop::Cancelled);
    }
}

/// What the reader thread saw.
enum Event {
    Frame(Frame),
    /// The helper's stdout closed (it exited or was killed).
    Closed,
    /// The reader killed the helper (the reason is recorded).
    Stopped,
}

/// A checked output and its counts.
#[derive(Clone, PartialEq, Eq)]
pub struct Volume {
    pub bytes: Vec<u8>,
    pub counts: DoneCounts,
}

impl std::fmt::Debug for Volume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Volume")
            .field("len", &self.bytes.len())
            .field("counts", &self.counts)
            .finish()
    }
}

/// One helper process serving one output format.
pub struct RenderSession {
    shared: Arc<Shared>,
    stdin: Option<ChildStdin>,
    events: Receiver<Event>,
    cancel: CancelToken,
    limits: SessionLimits,
    output: OutputKind,
    /// Dropped to stop the supervisor.
    supervisor: Option<Sender<()>>,
    /// The helper is dead or no longer trusted: every call fails.
    broken: Option<ExportError>,
    ended: bool,
}

impl std::fmt::Debug for RenderSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderSession")
            .field("output", &self.output)
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

fn drain_stderr(mut err: impl Read, shared: &Shared) {
    let mut buf = [0u8; 4096];
    loop {
        match err.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                shared.stderr.fetch_add(n as u64, Ordering::Relaxed);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
}

fn read_replies(mut out: ChildStdout, shared: &Shared, tx: &Sender<Event>, max_volume: u64) {
    let mut volume = 0u64;
    loop {
        let ev = match frame::read_frame(&mut out, Direction::Reply) {
            Ok(f) => match f.kind {
                // A refusal may come at any time; the helper exits after it.
                FrameKind::Error => Event::Frame(f),
                FrameKind::Chunk if shared.awaiting.load(Ordering::SeqCst) => {
                    volume = volume.saturating_add(f.payload.len() as u64);
                    if volume > max_volume {
                        shared.stop(Stop::OutputTooLarge);
                        Event::Stopped
                    } else {
                        Event::Frame(f)
                    }
                }
                FrameKind::Done if shared.awaiting.load(Ordering::SeqCst) => {
                    volume = 0;
                    shared.awaiting.store(false, Ordering::SeqCst);
                    Event::Frame(f)
                }
                // Output nobody asked for: an early or unsolicited write.
                _ => {
                    shared.stop(Stop::Protocol);
                    Event::Stopped
                }
            },
            Err(FrameError::TooLarge) => {
                shared.stop(Stop::Oversize);
                Event::Stopped
            }
            Err(FrameError::BadFrame) => {
                shared.stop(Stop::Protocol);
                Event::Stopped
            }
            Err(FrameError::Closed | FrameError::Truncated) => Event::Closed,
        };
        let last = !matches!(ev, Event::Frame(_));
        if tx.send(ev).is_err() || last {
            break;
        }
    }
}

fn supervise(shared: &Shared, cancel: &CancelToken, stop: &Receiver<()>) {
    loop {
        match stop.recv_timeout(TICK) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
        }
        if cancel.is_cancelled() {
            shared.stop(Stop::Cancelled);
            return;
        }
        if shared.overdue() {
            shared.stop(Stop::Timeout);
            return;
        }
    }
}

fn spawn_thread(name: &str, f: impl FnOnce() + Send + 'static) -> bool {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(f)
        .is_ok()
}

impl RenderSession {
    /// Start a helper and send `begin`. The output kind the session checks
    /// follows from the backend (the stub writes PDF).
    pub fn start(
        launch: &HelperLaunch,
        begin: &Begin,
        cancel: &CancelToken,
        limits: SessionLimits,
    ) -> Result<RenderSession, ExportError> {
        if cancel.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        let output = match begin.backend {
            BackendKind::Pdf | BackendKind::Stub => OutputKind::Pdf,
            BackendKind::Xlsx => OutputKind::Xlsx,
        };
        let spawn = || ExportError::HelperFailed(HelperFailure::Spawn);
        let mut child = launch.spawn().map_err(|_| spawn())?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(spawn());
        };
        let shared = Arc::new(Shared {
            child: Mutex::new(child),
            stop: Mutex::new(None),
            awaiting: AtomicBool::new(false),
            deadline: Mutex::new(None),
            stderr: AtomicU64::new(0),
        });
        let (tx, events) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let mut session = RenderSession {
            shared: Arc::clone(&shared),
            stdin: Some(stdin),
            events,
            cancel: cancel.clone(),
            limits,
            output,
            supervisor: Some(stop_tx),
            broken: None,
            ended: false,
        };
        let s = Arc::clone(&shared);
        let started_err = spawn_thread("oma-export-helper-stderr", move || {
            drain_stderr(stderr, &s);
        });
        let s = Arc::clone(&shared);
        let max = limits.max_volume_bytes;
        let started_reader = spawn_thread("oma-export-helper-reader", move || {
            read_replies(stdout, &s, &tx, max);
        });
        let s = Arc::clone(&shared);
        let c = cancel.clone();
        let started_supervisor = spawn_thread("oma-export-helper-supervisor", move || {
            supervise(&s, &c, &stop_rx);
        });
        if !(started_err && started_reader && started_supervisor) {
            // Dropping the session kills and reaps the helper.
            return Err(spawn());
        }
        session.send(FrameKind::Begin, &[&begin.encode()])?;
        Ok(session)
    }

    /// A handle that kills this session's helper from another thread.
    pub fn kill_handle(&self) -> HelperKill {
        HelperKill(Arc::clone(&self.shared))
    }

    /// The bytes the helper wrote to stderr so far (never its text).
    pub fn stderr_bytes(&self) -> u64 {
        self.shared.stderr.load(Ordering::Relaxed)
    }

    /// One resource (a font, in slice 6j), in pieces that fit a frame.
    /// Only before the first record.
    pub fn resource(&mut self, slot: u8, bytes: &[u8]) -> Result<(), ExportError> {
        let total = bytes.len() as u64;
        let piece = MAX_FRAME_BYTES as usize - RESOURCE_HEADER;
        let mut offset = 0u64;
        for chunk in bytes.chunks(piece.max(1)) {
            let head = ResourcePiece::header(slot, total, offset);
            self.send(FrameKind::Resource, &[&head, chunk])?;
            offset += chunk.len() as u64;
        }
        Ok(())
    }

    /// The PDF backend's fonts, every slot in [`crate::pdf::fonts::PINS`]
    /// order. Only fonts that passed [`crate::pdf::fonts::verify`] can be
    /// sent: this is the path by which font bytes reach the helper (owner
    /// Q2). Only before the first record.
    pub fn send_fonts(
        &mut self,
        fonts: &crate::pdf::fonts::VerifiedFonts,
    ) -> Result<(), ExportError> {
        for (slot, bytes) in fonts.slots().iter().enumerate() {
            let slot =
                u8::try_from(slot).map_err(|_| ExportError::Limit(LimitKind::FrameTooLarge))?;
            self.resource(slot, bytes)?;
        }
        Ok(())
    }

    /// A conversation record, already masked.
    pub fn conversation(&mut self, record: &[u8]) -> Result<(), ExportError> {
        self.send(FrameKind::Conversation, &[record])
    }

    /// An item record, already masked.
    pub fn item(&mut self, record: &[u8]) -> Result<(), ExportError> {
        self.send(FrameKind::Item, &[record])
    }

    /// A checked rendition for the attachment file `file_ordinal`.
    pub fn image(&mut self, file_ordinal: u64, bytes: &[u8]) -> Result<(), ExportError> {
        if bytes.len() > MAX_FRAME_BYTES as usize - IMAGE_HEADER {
            return Err(ExportError::Limit(LimitKind::FrameTooLarge));
        }
        self.send(FrameKind::Image, &[&file_ordinal.to_le_bytes(), bytes])
    }

    /// Finish the current output, wait for it, and check it. With `last`
    /// the helper exits afterwards (`End`), otherwise it serves the next
    /// volume (`EndVolume`).
    pub fn end_volume(&mut self, last: bool) -> Result<Volume, ExportError> {
        self.poll()?;
        self.shared.awaiting.store(true, Ordering::SeqCst);
        // The reply timeout runs from here, write included.
        self.shared.arm(self.limits.reply_timeout);
        let kind = if last {
            FrameKind::End
        } else {
            FrameKind::EndVolume
        };
        self.write(kind, &[])?;
        let mut bytes: Vec<u8> = Vec::new();
        let counts = loop {
            let ev = self.events.recv();
            let f = match ev {
                Ok(Event::Frame(f)) => f,
                Ok(Event::Closed | Event::Stopped) | Err(_) => {
                    return Err(self.fail(None, HelperFailure::Crashed))
                }
            };
            match f.kind {
                FrameKind::Chunk => {
                    let len = (bytes.len() as u64).saturating_add(f.payload.len() as u64);
                    if len > self.limits.max_volume_bytes {
                        self.shared.stop(Stop::OutputTooLarge);
                        return Err(self.fail(None, HelperFailure::OutputTooLarge));
                    }
                    bytes.extend_from_slice(&f.payload);
                }
                FrameKind::Done => match DoneCounts::decode(&f.payload) {
                    Some(c) if c.total_bytes == bytes.len() as u64 => break c,
                    _ => {
                        self.shared.stop(Stop::Protocol);
                        return Err(self.fail(None, HelperFailure::Protocol));
                    }
                },
                FrameKind::Error => {
                    let code = refusal(&f);
                    return Err(self.fail(code, HelperFailure::Protocol));
                }
                _ => {
                    self.shared.stop(Stop::Protocol);
                    return Err(self.fail(None, HelperFailure::Protocol));
                }
            }
        };
        self.shared.disarm();
        if last {
            self.ended = true;
            self.stdin = None;
        }
        if check_output(self.output, &bytes).is_err() {
            // The helper produced something it must not: it is not asked
            // for anything again.
            self.shared.kill();
            self.broken = Some(ExportError::HelperOutputRejected);
            return Err(ExportError::HelperOutputRejected);
        }
        Ok(Volume { bytes, counts })
    }

    /// [`Self::end_volume`], then write the checked bytes to `out` (a
    /// staged file). A full volume is `NoSpace`.
    pub fn end_volume_into(
        &mut self,
        out: &mut dyn Write,
        last: bool,
    ) -> Result<DoneCounts, ExportError> {
        let v = self.end_volume(last)?;
        out.write_all(&v.bytes)
            .and_then(|()| out.flush())
            .map_err(|e| ExportError::io(&e))?;
        Ok(v.counts)
    }

    /// Close the session: the helper's stdin is closed, and it is given a
    /// few seconds to exit before it is killed. Returns the bytes it wrote
    /// to stderr (never its text).
    pub fn finish(mut self) -> u64 {
        self.stdin = None;
        let deadline = Instant::now() + EXIT_GRACE;
        loop {
            let exited = self
                .shared
                .child
                .lock()
                .map(|mut c| !matches!(c.try_wait(), Ok(None)))
                .unwrap_or(true);
            if exited || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(TICK);
        }
        self.reap();
        self.ended = true;
        self.stderr_bytes()
    }

    fn reap(&mut self) {
        self.supervisor = None;
        if let Ok(mut c) = self.shared.child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// Check what the reader saw before the next request.
    fn poll(&mut self) -> Result<(), ExportError> {
        if let Some(e) = self.broken_error() {
            return Err(e);
        }
        // Between requests the reader forwards nothing but an `Error` frame
        // or the helper's end: anything waiting here ends the session.
        match self.events.try_recv() {
            Ok(Event::Frame(f)) if f.kind == FrameKind::Error => {
                let code = refusal(&f);
                return Err(self.fail(code, HelperFailure::Protocol));
            }
            Ok(Event::Frame(_)) => {
                self.shared.stop(Stop::Protocol);
                return Err(self.fail(None, HelperFailure::Protocol));
            }
            Ok(Event::Closed | Event::Stopped) | Err(TryRecvError::Disconnected) => {
                return Err(self.fail(None, HelperFailure::Crashed));
            }
            Err(TryRecvError::Empty) => {}
        }
        if self.cancel.is_cancelled() {
            self.shared.stop(Stop::Cancelled);
            return Err(self.fail(None, HelperFailure::Crashed));
        }
        Ok(())
    }

    fn broken_error(&self) -> Option<ExportError> {
        self.broken.as_ref().map(copy_error)
    }

    /// Send one request frame after [`Self::poll`]; a blocked write is
    /// killed by the supervisor after the stall timeout.
    fn send(&mut self, kind: FrameKind, parts: &[&[u8]]) -> Result<(), ExportError> {
        self.poll()?;
        self.shared.arm(self.limits.stall_timeout);
        let r = self.write(kind, parts);
        self.shared.disarm();
        r
    }

    fn write(&mut self, kind: FrameKind, parts: &[&[u8]]) -> Result<(), ExportError> {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        let len = u32::try_from(len)
            .ok()
            .filter(|l| *l <= MAX_FRAME_BYTES)
            .ok_or(ExportError::Limit(LimitKind::FrameTooLarge))?;
        if self.ended {
            return Err(ExportError::Limit(LimitKind::RowOrder));
        }
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(self.fail(None, HelperFailure::Crashed));
        };
        let written = stdin
            .write_all(&frame::raw_header(kind, len))
            .and_then(|()| parts.iter().try_for_each(|p| stdin.write_all(p)))
            .and_then(|()| stdin.flush());
        match written {
            Ok(()) => Ok(()),
            Err(_) => Err(self.fail(None, HelperFailure::Crashed)),
        }
    }

    /// The helper is gone or not trusted: kill it, collect what the reader
    /// saw last (an `Error` frame explains a helper that exited), and say
    /// why.
    fn fail(&mut self, mut code: Option<HelperErrorCode>, default: HelperFailure) -> ExportError {
        self.shared.kill();
        self.stdin = None;
        let until = Instant::now() + DRAIN_GRACE;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left) {
                Ok(Event::Frame(f)) => {
                    if f.kind == FrameKind::Error && code.is_none() {
                        code = refusal(&f);
                    }
                }
                Ok(Event::Closed | Event::Stopped) | Err(_) => break,
            }
        }
        let e = match (self.shared.reason(), code) {
            (Some(Stop::Cancelled), _) => ExportError::Cancelled,
            (Some(Stop::Timeout), _) => ExportError::HelperFailed(HelperFailure::Timeout),
            (Some(Stop::Oversize), _) => ExportError::Limit(LimitKind::FrameTooLarge),
            (Some(Stop::Protocol), _) => ExportError::HelperFailed(HelperFailure::Protocol),
            (Some(Stop::OutputTooLarge), _) => {
                ExportError::HelperFailed(HelperFailure::OutputTooLarge)
            }
            (None, _) if self.cancel.is_cancelled() => ExportError::Cancelled,
            (None, Some(HelperErrorCode::NoSpace)) => ExportError::NoSpace,
            (None, Some(c)) => ExportError::HelperFailed(HelperFailure::Refused(c)),
            (None, None) => ExportError::HelperFailed(default),
        };
        self.broken = Some(copy_error(&e));
        e
    }
}

/// The code of an `Error` frame (`None` for a malformed one).
fn refusal(f: &Frame) -> Option<HelperErrorCode> {
    match f.payload.as_slice() {
        [c] => HelperErrorCode::from_code(*c),
        _ => None,
    }
}

/// The session's errors are kinds without payloads, so they can be
/// repeated for every later call.
fn copy_error(e: &ExportError) -> ExportError {
    match e {
        ExportError::Cancelled => ExportError::Cancelled,
        ExportError::NoSpace => ExportError::NoSpace,
        ExportError::HelperOutputRejected => ExportError::HelperOutputRejected,
        ExportError::HelperFailed(f) => ExportError::HelperFailed(*f),
        ExportError::Limit(k) => ExportError::Limit(*k),
        _ => ExportError::HelperFailed(HelperFailure::Crashed),
    }
}

impl Drop for RenderSession {
    fn drop(&mut self) {
        self.stdin = None;
        self.reap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_binary_is_a_spawn_failure() {
        let launch = HelperLaunch::new(PathBuf::from("/nonexistent/oma-export-helper"), vec![]);
        let begin = Begin {
            backend: BackendKind::Pdf,
            options: Vec::new(),
        };
        let e = RenderSession::start(
            &launch,
            &begin,
            &CancelToken::new(),
            SessionLimits::default(),
        )
        .expect_err("no binary");
        assert!(matches!(e, ExportError::HelperFailed(HelperFailure::Spawn)));
        assert_eq!(format!("{launch:?}"), "HelperLaunch");
    }

    #[test]
    fn a_cancelled_export_starts_no_helper() {
        let launch = HelperLaunch::new(PathBuf::from("/nonexistent/oma-export-helper"), vec![]);
        let c = CancelToken::new();
        c.cancel();
        let begin = Begin {
            backend: BackendKind::Pdf,
            options: Vec::new(),
        };
        assert!(matches!(
            RenderSession::start(&launch, &begin, &c, SessionLimits::default()),
            Err(ExportError::Cancelled)
        ));
    }
}
