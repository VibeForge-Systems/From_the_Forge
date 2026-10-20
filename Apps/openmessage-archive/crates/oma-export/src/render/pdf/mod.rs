//! The helper's PDF backend (M6 spec §5.6, D133; slice 6j1): krilla
//! 0.8.2 for the PDF, parley for shaping, bidi, line breaking and font
//! fallback (owner Q1), with only the pinned bundled fonts (owner Q2).
//!
//! The session is: `Begin` with [`PdfOptions`], the fonts as `Resource`
//! slots (checked against the pins again, [`library`]), then one
//! [`Block`] per `Conversation` (a conversation heading) or `Item` frame
//! (a day or section heading, or an entry), then `EndVolume`. The
//! backend words, marks and formats nothing: every string arrives as a
//! [`crate::pdf::doc::PdfText`] the main process built with the marker
//! rule, and is drawn with that same string as its text layer.
//!
//! `Done` reports the volume's pages, the characters drawn as `.notdef`,
//! the lines broken inside a token, and the combining runs the main
//! process limited (counted from their markers).
//!
//! **Slice 6j2** added previews (`Image` frames: a checked JPEG or PNG
//! rendition, decoded by krilla's `raster-images` here in the helper and
//! drawn by the block after it), the tagged structure
//! ([`compose`]), and a volume's document title from its conversation
//! heading. Volumes are cut by the main process (`writer::pdf`), which
//! also sends the "About this export" page as a `Section` heading and
//! entries; every `EndVolume` starts the next volume afresh.
//!
//! [`InProcess`] runs this backend in the caller's process for tests
//! (spec §14.2, "helper protocol, in-process with the `render`
//! feature"), with the same output check as the helper's host.

pub mod compose;
pub mod layout;
pub mod library;

use std::sync::mpsc;

use super::{Halt, Output, OutputBackend};
use crate::check::{check_output, OutputKind};
use crate::error::{ExportError, HelperFailure};
use crate::helper::frame::{DoneCounts, HelperErrorCode};
use crate::helper::Volume;
use crate::pdf::doc::{Block, HeadingLevel, PdfOptions};
use crate::writer::pdf::PdfRenderer;

use compose::Composer;
use layout::Typesetter;
use library::Library;

/// The PDF backend.
pub struct PdfBackend {
    options: PdfOptions,
    typesetter: Option<Typesetter>,
    composer: Option<Composer>,
}

impl std::fmt::Debug for PdfBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfBackend")
            .field("page", &self.options.page)
            .field("fonts", &self.typesetter.is_some())
            .finish_non_exhaustive()
    }
}

fn refuse(code: HelperErrorCode) -> Halt {
    Halt::Refuse(code)
}

impl PdfBackend {
    /// A backend for the `Begin` options `options`.
    pub fn new(options: &[u8]) -> Result<PdfBackend, Halt> {
        let options =
            PdfOptions::decode(options).map_err(|_| refuse(HelperErrorCode::BadRequest))?;
        Ok(PdfBackend {
            options,
            typesetter: None,
            composer: None,
        })
    }

    fn parts(&mut self) -> Result<(&mut Typesetter, &mut Composer), Halt> {
        let ts = self
            .typesetter
            .as_mut()
            .ok_or(refuse(HelperErrorCode::BadRequest))?;
        let composer = self
            .composer
            .get_or_insert_with(|| Composer::new(self.options.page, &self.options.title));
        Ok((ts, composer))
    }

    fn block(&mut self, record: &[u8], conversation: bool) -> Result<(), Halt> {
        let block = Block::decode(record).map_err(|_| refuse(HelperErrorCode::BadRequest))?;
        let (ts, c) = self.parts()?;
        let done = match (&block, conversation) {
            (
                Block::Heading {
                    level: HeadingLevel::Conversation,
                    text,
                    title,
                },
                true,
            ) => {
                if let Some(t) = title {
                    c.set_title(t);
                }
                c.heading(ts, HeadingLevel::Conversation, text)
            }
            (
                Block::Heading {
                    level, text, title, ..
                },
                false,
            ) if *level != HeadingLevel::Conversation && title.is_none() => {
                c.heading(ts, *level, text)
            }
            (Block::Entry { head, body }, false) => c.entry(ts, head, body),
            _ => return Err(refuse(HelperErrorCode::BadRequest)),
        };
        done.map_err(|_| refuse(HelperErrorCode::Internal))
    }
}

impl OutputBackend for PdfBackend {
    fn resources(&mut self, slots: Vec<Vec<u8>>) -> Result<(), Halt> {
        let fonts = Library::new(slots, self.options.han)
            .map_err(|_| refuse(HelperErrorCode::BadRequest))?;
        self.typesetter = Some(Typesetter::new(fonts));
        Ok(())
    }

    fn conversation(&mut self, record: &[u8]) -> Result<(), Halt> {
        self.block(record, true)
    }

    fn item(&mut self, record: &[u8]) -> Result<(), Halt> {
        self.block(record, false)
    }

    fn image(&mut self, file_ordinal: u64, bytes: &[u8]) -> Result<(), Halt> {
        // The main process checked the rendition (`preview::check_rendition`);
        // it is decoded here, in the helper, never in the app.
        let data = krilla::Data::from(bytes.to_vec());
        let image = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
            krilla::image::Image::from_png(data, true)
        } else if bytes.starts_with(&[0xFF, 0xD8]) {
            krilla::image::Image::from_jpeg(data, true)
        } else {
            return Err(refuse(HelperErrorCode::BadRequest));
        }
        .map_err(|_| refuse(HelperErrorCode::BadRequest))?;
        let (_, c) = self.parts()?;
        c.add_image(file_ordinal, image)
            .map_err(|_| refuse(HelperErrorCode::BadRequest))
    }

    fn end_volume(&mut self) -> Result<Output, Halt> {
        let (_, _) = self.parts()?;
        let composer = self
            .composer
            .take()
            .ok_or(refuse(HelperErrorCode::Internal))?;
        let ts = self
            .typesetter
            .as_mut()
            .ok_or(refuse(HelperErrorCode::BadRequest))?;
        let (bytes, counts, _) = composer
            .finish(&mut ts.fonts)
            .map_err(|_| refuse(HelperErrorCode::Internal))?;
        Ok(Output {
            bytes,
            counts: DoneCounts {
                units: counts.pages,
                figures: counts.figures,
                notdef_chars: counts.notdef_chars,
                wrapped_tokens: counts.wrapped_tokens,
                combining_runs: counts.combining_runs,
                total_bytes: 0,
            },
        })
    }
}

/// The PDF backend in the caller's process (tests only: the app always
/// runs it in the helper), on a thread of its own (krilla's document is
/// not `Send`). Each `end_volume` runs [`check_output`] as the helper's
/// host does.
pub struct InProcess {
    tx: Option<mpsc::Sender<Req>>,
    rx: mpsc::Receiver<Result<Option<Volume>, ExportError>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

enum Req {
    Conversation(Vec<u8>),
    Item(Vec<u8>),
    Image(u64, Vec<u8>),
    EndVolume,
}

impl std::fmt::Debug for InProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InProcess").finish_non_exhaustive()
    }
}

fn halted(h: &Halt) -> ExportError {
    match h {
        Halt::Refuse(HelperErrorCode::NoSpace) => ExportError::NoSpace,
        _ => ExportError::HelperFailed(HelperFailure::Protocol),
    }
}

fn serve_in_process(
    mut backend: PdfBackend,
    rx: &mpsc::Receiver<Req>,
    tx: &mpsc::Sender<Result<Option<Volume>, ExportError>>,
) {
    while let Ok(req) = rx.recv() {
        let r = match req {
            Req::Conversation(b) => backend.conversation(&b).map(|()| None),
            Req::Item(b) => backend.item(&b).map(|()| None),
            Req::Image(n, b) => backend.image(n, &b).map(|()| None),
            Req::EndVolume => backend.end_volume().map(Some),
        };
        let r = match r {
            Ok(Some(out)) => match check_output(OutputKind::Pdf, &out.bytes) {
                Ok(_) => {
                    let mut counts = out.counts;
                    counts.total_bytes = u64::try_from(out.bytes.len()).unwrap_or(u64::MAX);
                    Ok(Some(Volume {
                        bytes: out.bytes,
                        counts,
                    }))
                }
                Err(_) => Err(ExportError::HelperOutputRejected),
            },
            Ok(None) => Ok(None),
            Err(h) => Err(halted(&h)),
        };
        if tx.send(r).is_err() {
            return;
        }
    }
}

impl InProcess {
    /// A backend for `options`, with the font slots (in pin order).
    pub fn new(options: &PdfOptions, fonts: Vec<Vec<u8>>) -> Result<InProcess, ExportError> {
        let options = options.encode();
        let (req_tx, req_rx) = mpsc::channel();
        let (res_tx, res_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("oma-pdf-in-process".to_owned())
            .spawn(move || {
                let started = PdfBackend::new(&options).and_then(|mut b| {
                    b.resources(fonts)?;
                    Ok(b)
                });
                match started {
                    Ok(backend) => {
                        if res_tx.send(Ok(None)).is_ok() {
                            serve_in_process(backend, &req_rx, &res_tx);
                        }
                    }
                    Err(h) => {
                        let _ = res_tx.send(Err(halted(&h)));
                    }
                }
            })
            .map_err(|_| ExportError::HelperFailed(HelperFailure::Spawn))?;
        let mut me = InProcess {
            tx: Some(req_tx),
            rx: res_rx,
            thread: Some(thread),
        };
        match me.rx.recv() {
            Ok(Ok(_)) => Ok(me),
            Ok(Err(e)) => {
                me.tx = None;
                Err(e)
            }
            Err(_) => Err(ExportError::HelperFailed(HelperFailure::Crashed)),
        }
    }

    fn call(&mut self, r: Req) -> Result<Option<Volume>, ExportError> {
        let crashed = || ExportError::HelperFailed(HelperFailure::Crashed);
        self.tx
            .as_ref()
            .ok_or_else(crashed)?
            .send(r)
            .map_err(|_| crashed())?;
        self.rx.recv().map_err(|_| crashed())?
    }
}

impl PdfRenderer for InProcess {
    fn conversation(&mut self, block: &[u8]) -> Result<(), ExportError> {
        self.call(Req::Conversation(block.to_vec())).map(|_| ())
    }

    fn item(&mut self, block: &[u8]) -> Result<(), ExportError> {
        self.call(Req::Item(block.to_vec())).map(|_| ())
    }

    fn image(&mut self, n: u64, bytes: &[u8]) -> Result<(), ExportError> {
        self.call(Req::Image(n, bytes.to_vec())).map(|_| ())
    }

    fn end_volume(&mut self, _last: bool) -> Result<Volume, ExportError> {
        self.call(Req::EndVolume)?
            .ok_or(ExportError::HelperFailed(HelperFailure::Protocol))
    }

    fn close(mut self: Box<Self>) -> u64 {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = std::thread::JoinHandle::join(t);
        }
        0
    }
}
