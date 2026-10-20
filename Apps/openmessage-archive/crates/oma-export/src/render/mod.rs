//! The export-render helper's side (M6 spec §5.7, D133; feature `render`).
//!
//! The app's binary started with `--oma-export-helper` calls [`serve`] on
//! its stdin and stdout before anything else (no Tauri, no logging, no
//! settings). One helper serves one format of one export. PDF layout and
//! serialisation (slice 6j) and XLSX writing (deferred) run here, so
//! hostile text that crashes shaping, or a full disk under a writer's
//! temporary files, ends one export, never the app.
//!
//! [`serve`] reads the request frames of [`crate::helper::frame`] (each
//! length checked before allocating), hands them to the backend the
//! `Begin` names, and answers `EndVolume` and `End` with the output in
//! `Chunk` frames of at most [`CHUNK_BYTES`] and one `Done`. Any refusal is
//! one `Error` frame with a closed code, after which the helper exits.
//!
//! **Backends in this build.** [`pdf`] (slice 6j1: layout and
//! serialisation with the pinned fonts). `Xlsx` is deferred and refused
//! with `Unsupported`. Debug builds also have [`stub`], a test backend that
//! writes a minimal PDF of counts (no content) and can be told to crash,
//! hang, panic, stop reading, send oversized, early or garbage frames,
//! report a full disk or plant a forbidden name, so the host's supervision
//! and the output checks are tested against a real process. Release
//! builds refuse it like any unknown backend.
//!
//! Nothing here logs. The binary's panic hook (installed by the entry)
//! prints a location only, never a payload; stderr is counted by the
//! host, never read.

pub mod pdf;
#[cfg(debug_assertions)]
pub mod stub;

use std::io::{Read, Write};

use crate::helper::frame::{
    self, decode_image, BackendKind, Begin, Direction, DoneCounts, FrameError, FrameKind,
    HelperErrorCode, ResourcePiece, MAX_RESOURCE_BYTES, MAX_RESOURCE_SLOTS,
};

/// The largest `Chunk` the helper sends.
pub const CHUNK_BYTES: usize = 1 << 20;

/// Exit codes of [`serve`].
pub const EXIT_OK: i32 = 0;
/// A malformed, oversized or out-of-direction frame, or a broken pipe.
pub const EXIT_PROTOCOL: i32 = 2;
/// The helper refused (after sending an `Error` frame).
pub const EXIT_REFUSED: i32 = 3;

/// A finished output.
#[derive(Clone, PartialEq, Eq)]
pub struct Output {
    pub bytes: Vec<u8>,
    /// `total_bytes` is filled in by [`serve`].
    pub counts: DoneCounts,
}

impl std::fmt::Debug for Output {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Output")
            .field("len", &self.bytes.len())
            .field("counts", &self.counts)
            .finish()
    }
}

/// Why a backend stops the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Halt {
    /// Send one `Error` frame with this code, then exit.
    Refuse(HelperErrorCode),
    /// Exit now with this code, sending nothing (a crash, as seen by the
    /// host).
    Exit(i32),
    /// Debug builds only: write these bytes to stdout as they are (no
    /// framing), then exit with the code, or go on serving with `None`.
    /// The stub's oversized, early and garbage replies.
    #[cfg(debug_assertions)]
    Raw {
        bytes: Vec<u8>,
        then_exit: Option<i32>,
    },
}

/// One format's work inside the helper.
pub trait OutputBackend {
    /// The resources the session sent (fonts, in slice 6j), by slot, once,
    /// before the first record.
    fn resources(&mut self, slots: Vec<Vec<u8>>) -> Result<(), Halt>;
    /// A conversation record (already masked), as the main process framed
    /// it.
    fn conversation(&mut self, record: &[u8]) -> Result<(), Halt>;
    /// An item record (already masked).
    fn item(&mut self, record: &[u8]) -> Result<(), Halt>;
    /// A checked decode-helper rendition for the attachment file with this
    /// ordinal.
    fn image(&mut self, file_ordinal: u64, bytes: &[u8]) -> Result<(), Halt>;
    /// Finish the current output and return it; the next record starts
    /// the next volume.
    fn end_volume(&mut self) -> Result<Output, Halt>;
}

/// The backend `begin` asks for, or the code it is refused with.
fn backend(begin: &Begin) -> Result<Box<dyn OutputBackend>, Halt> {
    match begin.backend {
        #[cfg(debug_assertions)]
        BackendKind::Stub => Ok(Box::new(stub::Stub::new(&begin.options)?)),
        #[cfg(not(debug_assertions))]
        BackendKind::Stub => Err(Halt::Refuse(HelperErrorCode::Unsupported)),
        BackendKind::Pdf => Ok(Box::new(pdf::PdfBackend::new(&begin.options)?)),
        // XLSX is deferred (see `docs/dependencies.md`).
        BackendKind::Xlsx => Err(Halt::Refuse(HelperErrorCode::Unsupported)),
    }
}

/// The resources being received, by slot.
#[derive(Default)]
struct Resources {
    slots: Vec<Vec<u8>>,
    totals: Vec<u64>,
    bytes: u64,
}

impl Resources {
    fn add(&mut self, p: &ResourcePiece<'_>) -> Result<(), Halt> {
        let refuse = Halt::Refuse(HelperErrorCode::ResourceLimit);
        if p.slot >= MAX_RESOURCE_SLOTS {
            return Err(refuse);
        }
        let slot = usize::from(p.slot);
        while self.slots.len() <= slot {
            self.slots.push(Vec::new());
            self.totals.push(0);
        }
        let (Some(buf), Some(total)) = (self.slots.get_mut(slot), self.totals.get_mut(slot)) else {
            return Err(refuse);
        };
        let len = u64::try_from(p.bytes.len()).map_err(|_| refuse.clone())?;
        let fresh = buf.is_empty() && *total == 0;
        if fresh {
            // A slot's total is fixed by its first piece and counted
            // against the budget at once, before anything is reserved.
            let over = self
                .bytes
                .checked_add(p.total)
                .is_none_or(|b| b > MAX_RESOURCE_BYTES);
            if over || p.total == 0 {
                return Err(refuse);
            }
            self.bytes += p.total;
            *total = p.total;
            buf.reserve_exact(usize::try_from(p.total).map_err(|_| refuse.clone())?);
        }
        let at = u64::try_from(buf.len()).map_err(|_| refuse.clone())?;
        let fits = at.checked_add(len).is_some_and(|end| end <= *total);
        if p.total != *total || p.offset != at || !fits {
            return Err(refuse);
        }
        buf.extend_from_slice(p.bytes);
        Ok(())
    }

    fn finish(self) -> Result<Vec<Vec<u8>>, Halt> {
        for (buf, total) in self.slots.iter().zip(&self.totals) {
            if u64::try_from(buf.len()).ok() != Some(*total) {
                return Err(Halt::Refuse(HelperErrorCode::ResourceLimit));
            }
        }
        Ok(self.slots)
    }
}

/// Send `out` as `Chunk` frames and one `Done`.
fn send_output(w: &mut dyn Write, out: &Output) -> std::io::Result<()> {
    for chunk in out.bytes.chunks(CHUNK_BYTES) {
        frame::write_frame(w, FrameKind::Chunk, chunk)?;
    }
    let mut counts = out.counts;
    counts.total_bytes = u64::try_from(out.bytes.len()).unwrap_or(u64::MAX);
    frame::write_frame(w, FrameKind::Done, &counts.encode())?;
    w.flush()
}

/// What to do after a backend call.
enum Next {
    /// Carry on (only the debug-only test backend's raw writes return it).
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    Go,
    Exit(i32),
}

fn halted(w: &mut dyn Write, h: Halt) -> Next {
    match h {
        Halt::Refuse(code) => {
            let _ =
                frame::write_frame(w, FrameKind::Error, &[code.code()]).and_then(|()| w.flush());
            Next::Exit(EXIT_REFUSED)
        }
        Halt::Exit(code) => Next::Exit(code),
        #[cfg(debug_assertions)]
        Halt::Raw { bytes, then_exit } => {
            if w.write_all(&bytes).and_then(|()| w.flush()).is_err() {
                return Next::Exit(EXIT_PROTOCOL);
            }
            then_exit.map_or(Next::Go, Next::Exit)
        }
    }
}

/// Serve one session on `input` and `output` until `End`, the input's end,
/// a refusal or a protocol error. Returns the process exit code.
pub fn serve(input: &mut dyn Read, output: &mut dyn Write) -> i32 {
    let begin = match frame::read_frame(input, Direction::Request) {
        Ok(f) if f.kind == FrameKind::Begin => f,
        Ok(_) => {
            return match halted(output, Halt::Refuse(HelperErrorCode::BadRequest)) {
                Next::Exit(c) => c,
                Next::Go => EXIT_REFUSED,
            }
        }
        Err(FrameError::Closed) => return EXIT_OK,
        Err(_) => return EXIT_PROTOCOL,
    };
    let mut b = match Begin::decode(&begin.payload) {
        Ok(begin) => match backend(&begin) {
            Ok(b) => b,
            Err(h) => return exit_code(halted(output, h)),
        },
        Err(_) => return exit_code(halted(output, Halt::Refuse(HelperErrorCode::BadRequest))),
    };
    drop(begin);
    let mut resources = Some(Resources::default());
    loop {
        let f = match frame::read_frame(input, Direction::Request) {
            Ok(f) => f,
            // The host closed the session: nothing more to do.
            Err(FrameError::Closed) => return EXIT_OK,
            Err(_) => return EXIT_PROTOCOL,
        };
        let step = match f.kind {
            FrameKind::Resource => match (resources.as_mut(), ResourcePiece::decode(&f.payload)) {
                (Some(r), Some(p)) => r.add(&p),
                _ => Err(Halt::Refuse(HelperErrorCode::BadRequest)),
            },
            FrameKind::Conversation | FrameKind::Item | FrameKind::Image => {
                let ready = match resources.take() {
                    Some(r) => r.finish().and_then(|slots| b.resources(slots)),
                    None => Ok(()),
                };
                ready.and_then(|()| match f.kind {
                    FrameKind::Conversation => b.conversation(&f.payload),
                    FrameKind::Item => b.item(&f.payload),
                    _ => match decode_image(&f.payload) {
                        Some((ordinal, bytes)) => b.image(ordinal, bytes),
                        None => Err(Halt::Refuse(HelperErrorCode::BadRequest)),
                    },
                })
            }
            FrameKind::EndVolume | FrameKind::End => {
                let ready = match resources.take() {
                    Some(r) => r.finish().and_then(|slots| b.resources(slots)),
                    None => Ok(()),
                };
                match ready.and_then(|()| b.end_volume()) {
                    Ok(out) => {
                        if send_output(output, &out).is_err() {
                            return EXIT_PROTOCOL;
                        }
                        if f.kind == FrameKind::End {
                            return EXIT_OK;
                        }
                        Ok(())
                    }
                    Err(h) => Err(h),
                }
            }
            _ => Err(Halt::Refuse(HelperErrorCode::BadRequest)),
        };
        drop(f);
        if let Err(h) = step {
            if let Next::Exit(code) = halted(output, h) {
                return code;
            }
        }
    }
}

fn exit_code(n: Next) -> i32 {
    match n {
        Next::Exit(c) => c,
        Next::Go => EXIT_REFUSED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::frame::{read_frame, write_frame};

    fn request(kind: FrameKind, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        write_frame(&mut v, kind, payload).expect("frame");
        v
    }

    fn replies(mut out: &[u8]) -> Vec<frame::Frame> {
        let mut v = Vec::new();
        while let Ok(f) = read_frame(&mut out, Direction::Reply) {
            v.push(f);
        }
        v
    }

    #[test]
    fn xlsx_is_refused_and_pdf_needs_its_options() {
        for (backend, code) in [
            (BackendKind::Xlsx, HelperErrorCode::Unsupported),
            (BackendKind::Pdf, HelperErrorCode::BadRequest),
        ] {
            let input = request(
                FrameKind::Begin,
                &Begin {
                    backend,
                    options: Vec::new(),
                }
                .encode(),
            );
            let mut out = Vec::new();
            assert_eq!(serve(&mut input.as_slice(), &mut out), EXIT_REFUSED);
            let r = replies(&out);
            assert_eq!(r.len(), 1);
            assert_eq!(r[0].kind, FrameKind::Error);
            assert_eq!(r[0].payload, [code.code()]);
        }
    }

    #[test]
    fn a_session_must_start_with_begin() {
        let input = request(FrameKind::Item, b"{}");
        let mut out = Vec::new();
        assert_eq!(serve(&mut input.as_slice(), &mut out), EXIT_REFUSED);
        assert_eq!(
            replies(&out)[0].payload,
            [HelperErrorCode::BadRequest.code()]
        );
        // An empty input is a session the host never started.
        assert_eq!(serve(&mut &b""[..], &mut Vec::new()), EXIT_OK);
        // A reply kind on the request pipe, or garbage, is a protocol
        // error: the helper exits without answering.
        let mut out = Vec::new();
        assert_eq!(serve(&mut &b"garbage garbage"[..], &mut out), EXIT_PROTOCOL);
        assert!(out.is_empty());
    }

    #[test]
    fn an_oversized_request_ends_the_helper_without_allocating() {
        let mut input = frame::raw_header(FrameKind::Begin, frame::MAX_FRAME_BYTES + 1).to_vec();
        input.extend_from_slice(b"x");
        let mut out = Vec::new();
        assert_eq!(serve(&mut input.as_slice(), &mut out), EXIT_PROTOCOL);
        assert!(out.is_empty());
    }

    #[test]
    fn resources_are_contiguous_bounded_pieces() {
        let piece = |slot, total, offset, bytes: &[u8]| {
            let mut p = ResourcePiece::header(slot, total, offset);
            p.extend_from_slice(bytes);
            p
        };
        let mut r = Resources::default();
        let p = piece(0, 6, 0, b"abc");
        r.add(&ResourcePiece::decode(&p).expect("p"))
            .expect("first");
        let p = piece(0, 6, 3, b"def");
        r.add(&ResourcePiece::decode(&p).expect("p"))
            .expect("second");
        assert_eq!(r.finish().expect("done"), vec![b"abcdef".to_vec()]);

        let refused = Err(Halt::Refuse(HelperErrorCode::ResourceLimit));
        let mut r = Resources::default();
        let p = piece(0, 6, 0, b"abc");
        r.add(&ResourcePiece::decode(&p).expect("p"))
            .expect("first");
        // A gap, a different total, an overrun.
        for bad in [
            piece(0, 6, 4, b"de"),
            piece(0, 7, 3, b"def"),
            piece(0, 6, 3, b"defg"),
        ] {
            assert_eq!(r.add(&ResourcePiece::decode(&bad).expect("p")), refused);
        }
        // Unfinished.
        assert_eq!(
            r.finish(),
            Err(Halt::Refuse(HelperErrorCode::ResourceLimit))
        );
        // Over the slot count or the byte budget.
        let mut r = Resources::default();
        let p = piece(MAX_RESOURCE_SLOTS, 1, 0, b"a");
        assert_eq!(r.add(&ResourcePiece::decode(&p).expect("p")), refused);
        let p = piece(0, MAX_RESOURCE_BYTES + 1, 0, b"a");
        assert_eq!(r.add(&ResourcePiece::decode(&p).expect("p")), refused);
    }
}
