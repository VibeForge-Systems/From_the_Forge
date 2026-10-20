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
//! **Not yet here (slice 6j2):** images (`Image` frames are refused with
//! `Unsupported`), the volume estimate and its 2,000-page and 40 MiB
//! cuts, tagged structure, and the "About this export" page (the main
//! process will send it as a `Section` heading and entries).

pub mod compose;
pub mod layout;
pub mod library;

use super::{Halt, Output, OutputBackend};
use crate::helper::frame::{DoneCounts, HelperErrorCode};
use crate::pdf::doc::{Block, HeadingLevel, PdfOptions};

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
                },
                true,
            ) => c.heading(ts, HeadingLevel::Conversation, text),
            (Block::Heading { level, text }, false) if *level != HeadingLevel::Conversation => {
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

    fn image(&mut self, _file_ordinal: u64, _bytes: &[u8]) -> Result<(), Halt> {
        // Thumbnails are slice 6j2's.
        Err(refuse(HelperErrorCode::Unsupported))
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
                notdef_chars: counts.notdef_chars,
                wrapped_tokens: counts.wrapped_tokens,
                combining_runs: counts.combining_runs,
                total_bytes: 0,
            },
        })
    }
}
