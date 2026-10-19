//! The decode helper process (M5 spec §5.5, D113).
//!
//! Images are decoded only here: the app's own binary started again with
//! [`HELPER_FLAG`], before Tauri is initialised, calling only
//! `oma_archive::media::render::render`. A decoder panic, an allocation
//! failure or a memory-safety bug in a decoder's `unsafe` SIMD code ends
//! one helper (one failed preview), never the app, and cannot reach the
//! main process's keys, archive connections or IPC. **It is not an OS
//! sandbox:** it runs as the same user.
//!
//! **The protocol** is length-prefixed frames on stdin and stdout. No
//! path, id or name crosses; stderr is discarded.
//!
//! ```text
//! request:  "OMAD" | version 1 | rendition | max_input u64 | max_decoded u64
//!           | max_side u32 | decoder_alloc u64 | len u64 | verified bytes
//! response: "OMAR" | status | format | flags | width u32 | height u32
//!           | source_width u32 | source_height u32 | len u64 | encoded bytes
//! ```
//!
//! All integers are little-endian. `status` is 0 or a
//! [`RenderError::code`]; `flags` bit 0 is "has alpha", bit 1 "animated".
//! The helper exits on any malformed frame, and the host treats any
//! malformed reply, a closed pipe or a reply later than
//! [`DECODE_TIMEOUT`] as a crash: it kills the helper and starts a new one
//! for the next request.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oma_archive::{RenderError, RenderLimits, RenderedFormat, Rendition};

/// The argument that starts the binary as a decode helper.
pub const HELPER_FLAG: &str = "--oma-decode-helper";
/// The longest a helper may take for one rendition.
pub const DECODE_TIMEOUT: Duration = Duration::from_secs(20);
/// The largest encoded rendition a host accepts back (a 2048 px PNG is at
/// most about 16 MiB).
pub const MAX_REPLY_BYTES: u64 = 32 << 20;

const REQ_MAGIC: &[u8; 4] = b"OMAD";
const REP_MAGIC: &[u8; 4] = b"OMAR";
const VERSION: u8 = 1;
/// The fixed part of a request: magic, version, rendition, four limits and
/// the payload length.
pub const REQUEST_HEADER: usize = 4 + 1 + 1 + 8 + 8 + 4 + 8 + 8;
/// The fixed part of a reply.
pub const REPLY_HEADER: usize = 4 + 1 + 1 + 1 + 4 * 4 + 8;

const RENDITION_THUMB: u8 = 1;
const RENDITION_DISPLAY: u8 = 2;
/// Debug builds only: the helper exits without replying, as a crash would
/// (the restart and crash-cap tests).
#[cfg(debug_assertions)]
pub const RENDITION_CRASH_FOR_TEST: u8 = 0xEE;

/// A rendition the helper made.
#[derive(Clone, PartialEq, Eq)]
pub struct HelperImage {
    pub format: RenderedFormat,
    pub width: u32,
    pub height: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub has_alpha: bool,
    pub animated: bool,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for HelperImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperImage")
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

/// What one render request ended in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderOutcome {
    Image(HelperImage),
    /// The helper refused the bytes (a closed code; not a crash).
    Refused(RenderError),
    /// The helper died, hung, or answered garbage.
    Crashed,
}

/// Something that turns verified bytes into a rendition: the helper
/// process in the app, a fake in tests.
pub trait RenderBackend: Send {
    fn render(
        &mut self,
        bytes: &[u8],
        rendition: Rendition,
        limits: &RenderLimits,
    ) -> RenderOutcome;
}

/// Makes one backend per decode worker.
pub type BackendFactory = Arc<dyn Fn() -> Box<dyn RenderBackend> + Send + Sync>;

// ------------------------------------------------------------------ frames

fn rendition_code(r: Rendition) -> u8 {
    match r {
        Rendition::Display => RENDITION_DISPLAY,
        _ => RENDITION_THUMB,
    }
}

/// The request frame's header for `len` payload bytes. The payload
/// follows it unchanged; nothing else is sent.
pub fn request_header(code: u8, limits: &RenderLimits, len: u64) -> Vec<u8> {
    let mut h = Vec::with_capacity(REQUEST_HEADER);
    h.extend_from_slice(REQ_MAGIC);
    h.push(VERSION);
    h.push(code);
    h.extend_from_slice(&limits.max_input_bytes.to_le_bytes());
    h.extend_from_slice(&limits.max_decoded_bytes.to_le_bytes());
    h.extend_from_slice(&limits.max_side.to_le_bytes());
    h.extend_from_slice(&limits.decoder_alloc.to_le_bytes());
    h.extend_from_slice(&len.to_le_bytes());
    h
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&b[at..at + 4]);
    u32::from_le_bytes(a)
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(a)
}

/// A parsed request header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestHeader {
    pub code: u8,
    pub limits: RenderLimits,
    pub len: u64,
}

/// Parse a request header; `None` for anything malformed.
pub fn parse_request_header(h: &[u8; REQUEST_HEADER]) -> Option<RequestHeader> {
    if &h[0..4] != REQ_MAGIC || h[4] != VERSION {
        return None;
    }
    let defaults = RenderLimits::default();
    // The helper never runs looser than the app's limits.
    let mut limits = RenderLimits::with_decoded_cap(u64_at(h, 14).min(defaults.max_decoded_bytes));
    limits.max_input_bytes = u64_at(h, 6).min(defaults.max_input_bytes);
    limits.max_side = u32_at(h, 22).min(defaults.max_side);
    limits.decoder_alloc = u64_at(h, 26).min(defaults.decoder_alloc);
    Some(RequestHeader {
        code: h[5],
        limits,
        len: u64_at(h, 34),
    })
}

fn reply_header(status: u8, img: Option<&HelperImage>) -> Vec<u8> {
    let mut h = Vec::with_capacity(REPLY_HEADER);
    h.extend_from_slice(REP_MAGIC);
    h.push(status);
    let (format, flags, dims, len) = match img {
        Some(i) => (
            match i.format {
                RenderedFormat::Png => 2,
                _ => 1,
            },
            u8::from(i.has_alpha) | (u8::from(i.animated) << 1),
            [i.width, i.height, i.source_width, i.source_height],
            u64::try_from(i.bytes.len()).unwrap_or(u64::MAX),
        ),
        None => (0, 0, [0; 4], 0),
    };
    h.push(format);
    h.push(flags);
    for d in dims {
        h.extend_from_slice(&d.to_le_bytes());
    }
    h.extend_from_slice(&len.to_le_bytes());
    h
}

// -------------------------------------------------------------- the entry

/// `true` when the process was started as a decode helper: exactly one
/// argument, [`HELPER_FLAG`].
pub fn is_helper_invocation() -> bool {
    let mut args = std::env::args_os().skip(1);
    matches!(
        (args.next(), args.next()),
        (Some(a), None) if a == HELPER_FLAG
    )
}

/// The helper's main loop: one request at a time until stdin closes.
/// Returns the process exit code. Called by `main` before anything else,
/// so no Tauri, logging or settings code runs in the helper.
pub fn run_helper() -> i32 {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    loop {
        let mut head = [0u8; REQUEST_HEADER];
        match read_full(&mut input, &mut head) {
            Ok(true) => {}
            Ok(false) => return 0,
            Err(_) => return 2,
        }
        let Some(req) = parse_request_header(&head) else {
            return 2;
        };
        if req.len > RenderLimits::default().max_input_bytes {
            return 2;
        }
        let Ok(len) = usize::try_from(req.len) else {
            return 2;
        };
        let mut bytes = vec![0u8; len];
        if !matches!(read_full(&mut input, &mut bytes), Ok(true)) {
            return 2;
        }
        let rendition = match req.code {
            RENDITION_THUMB => Rendition::Thumb,
            RENDITION_DISPLAY => Rendition::Display,
            #[cfg(debug_assertions)]
            RENDITION_CRASH_FOR_TEST => return 101,
            _ => return 2,
        };
        let (status, img) = match oma_archive::media::render::render(&bytes, rendition, &req.limits)
        {
            Ok(r) => (
                0,
                Some(HelperImage {
                    format: r.format,
                    width: r.width,
                    height: r.height,
                    source_width: r.source_width,
                    source_height: r.source_height,
                    has_alpha: r.has_alpha,
                    animated: r.animated,
                    bytes: r.bytes.expose().clone(),
                }),
            ),
            Err(e) => (e.code(), None),
        };
        drop(bytes);
        let head = reply_header(status, img.as_ref());
        let sent = output
            .write_all(&head)
            .and_then(|()| match &img {
                Some(i) => output.write_all(&i.bytes),
                None => Ok(()),
            })
            .and_then(|()| output.flush());
        if sent.is_err() {
            return 2;
        }
    }
}

/// Fill `buf`; `Ok(false)` on EOF before the first byte.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut at = 0;
    while at < buf.len() {
        match r.read(&mut buf[at..]) {
            Ok(0) if at == 0 => return Ok(false),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => at += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

// --------------------------------------------------------------- the host

/// How to start a helper: the app's own binary with [`HELPER_FLAG`], or
/// (in tests) the binary Cargo built.
#[derive(Debug, Clone)]
pub struct HelperCommand {
    program: PathBuf,
    args: Vec<OsString>,
}

impl HelperCommand {
    pub fn new(program: PathBuf, args: Vec<OsString>) -> HelperCommand {
        HelperCommand { program, args }
    }

    /// This binary, started with [`HELPER_FLAG`].
    pub fn current_exe() -> io::Result<HelperCommand> {
        Ok(HelperCommand::new(
            std::env::current_exe()?,
            vec![OsString::from(HELPER_FLAG)],
        ))
    }

    fn spawn(&self) -> io::Result<Child> {
        let mut c = Command::new(&self.program);
        c.args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: no console window flashes up.
            c.creation_flags(0x0800_0000);
        }
        c.spawn()
    }
}

struct Running {
    child: Arc<Mutex<Child>>,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl Running {
    fn kill(self) {
        drop(self.stdin);
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// One long-lived helper process, started on first use and restarted
/// after a crash.
pub struct HelperHost {
    command: Option<HelperCommand>,
    running: Option<Running>,
    timeout: Duration,
    /// Helpers started so far (for tests).
    spawned: u32,
}

impl HelperHost {
    pub fn new(command: Option<HelperCommand>) -> HelperHost {
        HelperHost {
            command,
            running: None,
            timeout: DECODE_TIMEOUT,
            spawned: 0,
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> HelperHost {
        self.timeout = timeout;
        self
    }

    pub fn spawned(&self) -> u32 {
        self.spawned
    }

    fn ensure(&mut self) -> Option<&mut Running> {
        if self.running.is_none() {
            let mut child = self.command.as_ref()?.spawn().ok()?;
            self.spawned += 1;
            let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            };
            self.running = Some(Running {
                child: Arc::new(Mutex::new(child)),
                stdin,
                stdout,
            });
        }
        self.running.as_mut()
    }

    /// Send one request (`code` is the rendition's, or the test crash).
    pub fn request(&mut self, bytes: &[u8], code: u8, limits: &RenderLimits) -> RenderOutcome {
        let timeout = self.timeout;
        let Some(run) = self.ensure() else {
            return RenderOutcome::Crashed;
        };
        // A watchdog kills a helper that does not answer in time; the
        // blocked read then sees EOF.
        let (done, wait) = mpsc::channel::<()>();
        let child = Arc::clone(&run.child);
        let dog = std::thread::Builder::new()
            .name("oma-decode-watchdog".to_owned())
            .spawn(move || {
                if let Err(RecvTimeoutError::Timeout) = wait.recv_timeout(timeout) {
                    if let Ok(mut c) = child.lock() {
                        let _ = c.kill();
                    }
                }
            });
        let outcome = exchange(run, bytes, code, limits);
        drop(done);
        if let Ok(h) = dog {
            let _ = h.join();
        }
        match outcome {
            Some(o) => o,
            None => {
                if let Some(r) = self.running.take() {
                    r.kill();
                }
                RenderOutcome::Crashed
            }
        }
    }
}

/// One request and its reply; `None` for anything that is not a
/// well-formed reply.
fn exchange(
    run: &mut Running,
    bytes: &[u8],
    code: u8,
    limits: &RenderLimits,
) -> Option<RenderOutcome> {
    let len = u64::try_from(bytes.len()).ok()?;
    run.stdin
        .write_all(&request_header(code, limits, len))
        .ok()?;
    run.stdin.write_all(bytes).ok()?;
    run.stdin.flush().ok()?;
    let mut head = [0u8; REPLY_HEADER];
    if !read_full(&mut run.stdout, &mut head).ok()? {
        return None;
    }
    if &head[0..4] != REP_MAGIC {
        return None;
    }
    let status = head[4];
    let len = u64_at(&head, 23);
    if status != 0 {
        if len != 0 {
            return None;
        }
        return RenderError::from_code(status).map(RenderOutcome::Refused);
    }
    if len == 0 || len > MAX_REPLY_BYTES {
        return None;
    }
    let format = match head[5] {
        1 => RenderedFormat::Jpeg,
        2 => RenderedFormat::Png,
        _ => return None,
    };
    let mut out = vec![0u8; usize::try_from(len).ok()?];
    if !read_full(&mut run.stdout, &mut out).ok()? {
        return None;
    }
    Some(RenderOutcome::Image(HelperImage {
        format,
        width: u32_at(&head, 7),
        height: u32_at(&head, 11),
        source_width: u32_at(&head, 15),
        source_height: u32_at(&head, 19),
        has_alpha: head[6] & 1 != 0,
        animated: head[6] & 2 != 0,
        bytes: out,
    }))
}

impl RenderBackend for HelperHost {
    fn render(
        &mut self,
        bytes: &[u8],
        rendition: Rendition,
        limits: &RenderLimits,
    ) -> RenderOutcome {
        self.request(bytes, rendition_code(rendition), limits)
    }
}

impl Drop for HelperHost {
    fn drop(&mut self) {
        if let Some(r) = self.running.take() {
            r.kill();
        }
    }
}

/// The app's backend factory: a helper per decode worker, started from
/// this binary. Without a resolvable binary every render is a crash, so
/// previews turn off after the cap.
pub fn process_factory() -> BackendFactory {
    let command = HelperCommand::current_exe().ok();
    Arc::new(move || Box::new(HelperHost::new(command.clone())) as Box<dyn RenderBackend>)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_frames_carry_the_limits_the_length_and_nothing_else() {
        let limits = RenderLimits::default();
        let payload = b"\x89PNG not really";
        let h = request_header(RENDITION_THUMB, &limits, payload.len() as u64);
        assert_eq!(h.len(), REQUEST_HEADER);
        let mut fixed = [0u8; REQUEST_HEADER];
        fixed.copy_from_slice(&h);
        let parsed = parse_request_header(&fixed).expect("parse");
        assert_eq!(parsed.code, RENDITION_THUMB);
        assert_eq!(parsed.limits, limits);
        assert_eq!(parsed.len, payload.len() as u64);
        // Every byte of the header is accounted for by those fields: the
        // magic, the version, the code, four limits and the length. Nothing
        // names a path, an id or a file name.
        let mut rebuilt = Vec::new();
        rebuilt.extend_from_slice(b"OMAD");
        rebuilt.push(1);
        rebuilt.push(RENDITION_THUMB);
        rebuilt.extend_from_slice(&limits.max_input_bytes.to_le_bytes());
        rebuilt.extend_from_slice(&limits.max_decoded_bytes.to_le_bytes());
        rebuilt.extend_from_slice(&limits.max_side.to_le_bytes());
        rebuilt.extend_from_slice(&limits.decoder_alloc.to_le_bytes());
        rebuilt.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        assert_eq!(h, rebuilt);
    }

    #[test]
    fn the_helper_never_runs_looser_than_the_app_limits() {
        let mut loose = RenderLimits::default();
        loose.max_input_bytes = u64::MAX;
        loose.max_decoded_bytes = u64::MAX;
        loose.max_side = u32::MAX;
        loose.decoder_alloc = u64::MAX;
        let h = request_header(RENDITION_DISPLAY, &loose, 0);
        let mut fixed = [0u8; REQUEST_HEADER];
        fixed.copy_from_slice(&h);
        assert_eq!(
            parse_request_header(&fixed).expect("parse").limits,
            RenderLimits::default()
        );
        let tight = RenderLimits::with_decoded_cap(1 << 20);
        let h = request_header(RENDITION_DISPLAY, &tight, 0);
        fixed.copy_from_slice(&h);
        assert_eq!(parse_request_header(&fixed).expect("parse").limits, tight);
        fixed[0] = b'X';
        assert!(parse_request_header(&fixed).is_none());
    }

    #[test]
    fn reply_headers_have_a_fixed_size() {
        let img = HelperImage {
            format: RenderedFormat::Png,
            width: 6,
            height: 4,
            source_width: 6,
            source_height: 4,
            has_alpha: true,
            animated: false,
            bytes: vec![1, 2, 3],
        };
        assert_eq!(reply_header(0, Some(&img)).len(), REPLY_HEADER);
        assert_eq!(reply_header(5, None).len(), REPLY_HEADER);
    }

    #[test]
    fn a_host_without_a_binary_reports_a_crash() {
        let mut h = HelperHost::new(None);
        assert_eq!(
            h.render(b"x", Rendition::Thumb, &RenderLimits::default()),
            RenderOutcome::Crashed
        );
        let mut h = HelperHost::new(Some(HelperCommand::new(
            PathBuf::from("/nonexistent/oma-helper-binary"),
            vec![],
        )));
        assert_eq!(
            h.render(b"x", Rendition::Thumb, &RenderLimits::default()),
            RenderOutcome::Crashed
        );
        assert_eq!(h.spawned(), 0);
    }
}
