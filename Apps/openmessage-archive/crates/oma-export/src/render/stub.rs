//! The test backend of the export-render helper (debug builds only).
//!
//! It writes a minimal, well-formed PDF per volume that holds **counts
//! only** (conversations, items, images, resource bytes): never a byte of
//! a record. Its options pick one fault, so the host's supervision, the
//! frame bounds and the output checks are tested against a real helper
//! process (`src-tauri/tests/export_helper.rs`):
//!
//! ```text
//! options: fault u8 | arg u64 LE      (empty: no fault)
//! ```
//!
//! Release builds do not contain this module; the helper refuses the
//! `Stub` backend there like any backend it does not have.

use std::io::Write;
use std::time::Duration;

use super::{Halt, Output, OutputBackend};
use crate::helper::frame::{self, DoneCounts, FrameKind, HelperErrorCode};

/// The faults the stub can play.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    None,
    /// Exit with code 101 at the `arg`-th item, replying nothing.
    CrashAtItem,
    /// Never answer `EndVolume`.
    HangAtEnd,
    /// Answer `EndVolume` with a frame header claiming 16 MiB + 1.
    OversizeAtEnd,
    /// Answer `EndVolume` with bytes that are not a frame.
    GarbageAtEnd,
    /// Send a `Chunk` at the first item, before any `EndVolume`.
    WriteEarly,
    /// Put an `/OpenAction` with `/JavaScript` into the PDF.
    PlantJavaScript,
    /// Answer `EndVolume` with `Error(NoSpace)`.
    NoSpaceAtEnd,
    /// Panic at the first item, with a payload naming a sentinel.
    PanicAtItem,
    /// Stop reading at the first item (sleep forever).
    StopReading,
    /// Write `arg` bytes to stderr when the session begins.
    Stderr,
    /// Add an unreferenced stream of `arg` spaces (an output of several
    /// chunks).
    Pad,
    /// Answer with a `Done` whose total disagrees with the chunks.
    WrongTotal,
    /// Exit with code 0 at `EndVolume`, replying nothing.
    ExitAtEnd,
    /// Stop reading at the first item and write `Error` frames forever:
    /// with `arg` 0 each claims 16 MiB (and carries it), otherwise each is
    /// a valid one-byte `Error(BadRequest)`.
    FloodErrors,
}

impl Fault {
    pub const ALL: [Fault; 15] = [
        Fault::None,
        Fault::CrashAtItem,
        Fault::HangAtEnd,
        Fault::OversizeAtEnd,
        Fault::GarbageAtEnd,
        Fault::WriteEarly,
        Fault::PlantJavaScript,
        Fault::NoSpaceAtEnd,
        Fault::PanicAtItem,
        Fault::StopReading,
        Fault::Stderr,
        Fault::Pad,
        Fault::WrongTotal,
        Fault::ExitAtEnd,
        Fault::FloodErrors,
    ];

    fn code(self) -> u8 {
        Fault::ALL
            .iter()
            .position(|f| *f == self)
            .and_then(|i| u8::try_from(i).ok())
            .unwrap_or(0)
    }

    /// The `Begin` options that select this fault with `arg`.
    pub fn options(self, arg: u64) -> Vec<u8> {
        let mut v = vec![self.code()];
        v.extend_from_slice(&arg.to_le_bytes());
        v
    }
}

/// The sentinel the panic fault's payload carries (it must never reach
/// the host: the panic hook prints a location only).
pub const PANIC_SENTINEL: &str = "OMA-STUB-PANIC-PAYLOAD-SENTINEL";

/// The stub backend.
#[derive(Debug)]
pub struct Stub {
    fault: Fault,
    arg: u64,
    volume: u64,
    conversations: u64,
    items: u64,
    images: u64,
    resource_bytes: u64,
}

/// Write `Error` frames to stdout until the pipe breaks, then sleep.
fn flood_errors(arg: u64) -> ! {
    let (len, payload) = if arg == 0 {
        (
            frame::MAX_FRAME_BYTES,
            vec![0u8; frame::MAX_FRAME_BYTES as usize],
        )
    } else {
        (1, vec![HelperErrorCode::BadRequest.code()])
    };
    let header = frame::raw_header(FrameKind::Error, len);
    let mut out = std::io::stdout().lock();
    while out
        .write_all(&header)
        .and_then(|()| out.write_all(&payload))
        .and_then(|()| out.flush())
        .is_ok()
    {}
    forever()
}

fn forever() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

impl Stub {
    pub fn new(options: &[u8]) -> Result<Stub, Halt> {
        let (fault, arg) = match options.split_first() {
            None => (Fault::None, 0),
            Some((&code, rest)) => {
                let fault = Fault::ALL
                    .get(usize::from(code))
                    .copied()
                    .ok_or(Halt::Refuse(HelperErrorCode::BadRequest))?;
                let mut a = [0u8; 8];
                if rest.len() != 8 {
                    return Err(Halt::Refuse(HelperErrorCode::BadRequest));
                }
                a.copy_from_slice(rest);
                (fault, u64::from_le_bytes(a))
            }
        };
        if fault == Fault::Stderr {
            let noise = vec![b'x'; usize::try_from(arg).unwrap_or(0)];
            let _ = std::io::stderr().write_all(&noise);
        }
        Ok(Stub {
            fault,
            arg,
            volume: 0,
            conversations: 0,
            items: 0,
            images: 0,
            resource_bytes: 0,
        })
    }
}

impl OutputBackend for Stub {
    fn resources(&mut self, slots: Vec<Vec<u8>>) -> Result<(), Halt> {
        self.resource_bytes = slots
            .iter()
            .map(|s| u64::try_from(s.len()).unwrap_or(u64::MAX))
            .fold(0u64, u64::saturating_add);
        Ok(())
    }

    fn conversation(&mut self, _record: &[u8]) -> Result<(), Halt> {
        self.conversations += 1;
        Ok(())
    }

    #[allow(clippy::panic)]
    fn item(&mut self, _record: &[u8]) -> Result<(), Halt> {
        self.items += 1;
        match self.fault {
            Fault::CrashAtItem if self.items >= self.arg => Err(Halt::Exit(101)),
            Fault::PanicAtItem => panic!("{PANIC_SENTINEL}"),
            Fault::StopReading => forever(),
            Fault::FloodErrors => flood_errors(self.arg),
            Fault::WriteEarly if self.items == 1 => {
                let mut bytes = Vec::new();
                frame::write_frame(&mut bytes, FrameKind::Chunk, b"%PDF-")
                    .map_err(|_| Halt::Exit(1))?;
                Err(Halt::Raw {
                    bytes,
                    then_exit: None,
                })
            }
            _ => Ok(()),
        }
    }

    fn image(&mut self, _file_ordinal: u64, _bytes: &[u8]) -> Result<(), Halt> {
        self.images += 1;
        Ok(())
    }

    fn end_volume(&mut self) -> Result<Output, Halt> {
        self.volume += 1;
        match self.fault {
            Fault::HangAtEnd => forever(),
            Fault::OversizeAtEnd => {
                return Err(Halt::Raw {
                    bytes: frame::raw_header(FrameKind::Chunk, frame::MAX_FRAME_BYTES + 1).to_vec(),
                    then_exit: None,
                })
            }
            Fault::GarbageAtEnd => {
                return Err(Halt::Raw {
                    bytes: b"this is not a frame of the export protocol\n".to_vec(),
                    then_exit: None,
                })
            }
            Fault::NoSpaceAtEnd => return Err(Halt::Refuse(HelperErrorCode::NoSpace)),
            Fault::ExitAtEnd => return Err(Halt::Exit(0)),
            _ => {}
        }
        let padding = if self.fault == Fault::Pad {
            usize::try_from(self.arg).unwrap_or(0)
        } else {
            0
        };
        let bytes = pdf(
            &format!(
                "% stub volume {}: {} conversations, {} items, {} images, {} resource bytes\nBT ET\n",
                self.volume, self.conversations, self.items, self.images, self.resource_bytes
            ),
            padding,
            self.fault == Fault::PlantJavaScript,
        );
        if self.fault == Fault::WrongTotal {
            let mut raw = Vec::new();
            let done = DoneCounts {
                units: 1,
                total_bytes: u64::try_from(bytes.len()).unwrap_or(0) + 1,
                ..DoneCounts::default()
            };
            frame::write_frame(&mut raw, FrameKind::Chunk, &bytes)
                .and_then(|()| frame::write_frame(&mut raw, FrameKind::Done, &done.encode()))
                .map_err(|_| Halt::Exit(1))?;
            return Err(Halt::Raw {
                bytes: raw,
                then_exit: None,
            });
        }
        self.conversations = 0;
        self.items = 0;
        self.images = 0;
        Ok(Output {
            bytes,
            counts: DoneCounts {
                units: 1,
                ..DoneCounts::default()
            },
        })
    }
}

/// A minimal PDF: one empty page whose content stream (Flate) is
/// `content`, and an unreferenced, unfiltered stream of `padding` spaces.
pub fn pdf(content: &str, padding: usize, plant_javascript: bool) -> Vec<u8> {
    let z = miniz_oxide::deflate::compress_to_vec_zlib(content.as_bytes(), 6);
    let catalog = if plant_javascript {
        "<< /Type /Catalog /Pages 2 0 R /OpenAction << /S /JavaScript /JS (app.alert\\(1\\)) >> >>"
    } else {
        "<< /Type /Catalog /Pages 2 0 R >>"
    };
    let mut out: Vec<u8> = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    let mut object = |out: &mut Vec<u8>, n: usize, body: &[u8]| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    };
    object(&mut out, 1, catalog.as_bytes());
    object(&mut out, 2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    object(
        &mut out,
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R >>",
    );
    let mut body = format!("<< /Length {} /Filter /FlateDecode >>\nstream\n", z.len()).into_bytes();
    body.extend_from_slice(&z);
    body.extend_from_slice(b"\nendstream");
    object(&mut out, 4, &body);
    let mut body = format!("<< /Length {padding} >>\nstream\n").into_bytes();
    body.resize(body.len() + padding, b' ');
    body.extend_from_slice(b"\nendstream");
    object(&mut out, 5, &body);
    let xref = out.len();
    out.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stub_pdf_passes_the_check_and_the_planted_one_does_not() {
        let ok = pdf("% counts\nBT ET\n", 10, false);
        assert_eq!(crate::check::pdf::check(&ok), Ok(()));
        let bad = pdf("% counts\nBT ET\n", 0, true);
        assert_eq!(
            crate::check::pdf::check(&bad),
            Err(crate::check::pdf::PdfRefusal::ForbiddenName)
        );
    }

    #[test]
    fn fault_options_round_trip() {
        for f in Fault::ALL {
            let s = Stub::new(&f.options(7)).expect("stub");
            assert_eq!((s.fault, s.arg), (f, 7));
        }
        assert!(Stub::new(&[200, 0, 0, 0, 0, 0, 0, 0, 0]).is_err());
        assert!(Stub::new(&[1, 0]).is_err());
    }
}
