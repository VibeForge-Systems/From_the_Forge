//! The PDF writer (M6 spec §5.6, §5.7, §7.3; slice 6j2).
//!
//! The main process never lays out or serialises a PDF: it turns each
//! record into [`Block`]s and sends them to a [`PdfRenderer`], which in
//! the app is the export-render helper ([`HelperRenderers`] starts one
//! `RenderSession` per export, with the pinned fonts it was given as
//! [`VerifiedFonts`]). Every volume comes back checked
//! (`check::check_output`) before a byte of it is staged.
//!
//! **What is drawn.** The same lines as TXT, from the same functions
//! ([`super::lines`]): a conversation heading (a new page and an outline
//! entry), the conversation's facts, a day heading per local day (and
//! "Undated" last), then per item a header row of separate isolates (the
//! time, `item N`, the sender, each tag in brackets) and its labels and
//! recorded text, attachments with an optional preview, reactions,
//! history and stored versions; then the conversation's footer (counts
//! and the legend). A single file ends with "About this export" (§7.3),
//! the manifest's fields without `files`. Recorded values are marked with
//! TXT's policies through `pdf::doc::TextBuilder`; nothing recorded is
//! ever passed as our own words.
//!
//! **Files and volumes.** A bundle has one PDF per conversation file,
//! `pdf/c0001.pdf`, cut into volumes `pdf/c0001-2.pdf` … at an item
//! boundary when the volume's **estimate** ([`VolumeLimits`]) would pass
//! 2,000 pages or 40 MiB; the true size is known only when the helper
//! returns it (krilla serialises at the end), so the estimate is
//! deliberately generous. A volume after a cut repeats the conversation
//! heading with "(part N)" and the current day heading. A single file is
//! never cut; it is bounded by the helper's 256 MiB output limit.
//!
//! **Bounds.** A block is at most [`MAX_BLOCK_BYTES`] encoded: a longer
//! entry continues in further blocks (with an empty header row), and a
//! line of recorded text longer than [`MAX_PARA_BYTES`] is split into
//! paragraphs at character boundaries, each split counted with the forced
//! wraps.
//!
//! **Previews** come from the export's [`PreviewSource`] (the decode-helper
//! pool's feed), only for attachments whose access is `included` or
//! `revealed`; each thumbnail passes `preview::check_rendition` before it
//! is sent as an `Image` frame (a failed check fails the export,
//! `HelperOutputRejected`). Unavailable previews are a line, and counted.
//!
//! Nothing here logs; `Debug` prints counts only.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use oma_archive::CancelToken;

use crate::codes::AttachmentAccessCode;
use crate::error::{ExportError, HelperFailure, LimitKind};
use crate::helper::{
    BackendKind, Begin, HelperKill, HelperLaunch, RenderSession, SessionLimits, Volume,
};
use crate::hidden::MarkCounts;
use crate::ids::Ordinal;
use crate::map::ExportZone;
use crate::naming::{BundlePath, FileStem};
use crate::pdf::doc::{
    Block, HanFace, HeadingLevel, PageSize, Para, ParaRole, PdfOptions, PdfText, TextBuilder,
    MAX_HEAD_PIECES,
};
use crate::pdf::fonts::VerifiedFonts;
use crate::plan::{self, Clock, Format, Packaging};
use crate::preview::{check_rendition, CheckedRendition, Preview, PreviewSource, RenditionSize};
use crate::record::*;
use crate::stage::Staging;
use crate::time::{CivilDate, HumanStyle};
use crate::words::pdf_text;

use super::lines::{self, Emit, Part, Run};
use super::shared::{
    date_text, flatten, is_undated, item_tags, sender_tags, time_text, SenderTags,
};
use super::txt::split_lines;
use super::{
    is, AboutDraft, ConversationFile, ConversationTotals, ExportContext, ExportWriter, Words,
    WriterReport,
};

/// The most bytes one encoded block may have (the frame bound is 16 MiB).
pub const MAX_BLOCK_BYTES: usize = 8 << 20;
/// The longest paragraph of recorded text, in bytes, before it is split.
pub const MAX_PARA_BYTES: usize = 32 << 10;
/// The most previews one block draws. Each block's images are sent just
/// before it, so the helper never holds more than this many waiting
/// (its own bound, `render::pdf::compose::MAX_PENDING_IMAGES`, is 64).
pub const MAX_FIGURES_PER_BLOCK: usize = 32;

/// The output of one volume, as the helper (or a test's renderer)
/// returns it.
pub trait PdfRenderer: Send {
    /// A conversation heading block.
    fn conversation(&mut self, block: &[u8]) -> Result<(), ExportError>;
    /// Any other block.
    fn item(&mut self, block: &[u8]) -> Result<(), ExportError>;
    /// A checked rendition for the next block's figure `n`.
    fn image(&mut self, n: u64, bytes: &[u8]) -> Result<(), ExportError>;
    /// Finish the volume and return it, checked. With `last` nothing
    /// follows.
    fn end_volume(&mut self, last: bool) -> Result<Volume, ExportError>;
    /// Close the renderer; returns the bytes its process wrote to stderr
    /// (never its text).
    fn close(self: Box<Self>) -> u64;
}

impl PdfRenderer for RenderSession {
    fn conversation(&mut self, block: &[u8]) -> Result<(), ExportError> {
        RenderSession::conversation(self, block)
    }

    fn item(&mut self, block: &[u8]) -> Result<(), ExportError> {
        RenderSession::item(self, block)
    }

    fn image(&mut self, n: u64, bytes: &[u8]) -> Result<(), ExportError> {
        RenderSession::image(self, n, bytes)
    }

    fn end_volume(&mut self, last: bool) -> Result<Volume, ExportError> {
        RenderSession::end_volume(self, last)
    }

    fn close(self: Box<Self>) -> u64 {
        self.finish()
    }
}

/// Starts the export's renderer when the first conversation file begins.
pub trait PdfRendererFactory: Send {
    fn start(&mut self, options: &PdfOptions) -> Result<Box<dyn PdfRenderer>, ExportError>;
}

/// The app's renderers: an export-render helper per export, given the
/// verified fonts. `on_start` receives each helper's kill handle (the
/// shell puts it on the job's cancel path); `stderr` adds up the bytes
/// the helpers wrote to stderr.
pub struct HelperRenderers {
    launch: HelperLaunch,
    fonts: VerifiedFonts,
    cancel: CancelToken,
    limits: SessionLimits,
    on_start: Box<dyn FnMut(HelperKill) + Send>,
    stderr: Arc<AtomicU64>,
}

impl std::fmt::Debug for HelperRenderers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HelperRenderers").finish_non_exhaustive()
    }
}

impl HelperRenderers {
    pub fn new(
        launch: HelperLaunch,
        fonts: VerifiedFonts,
        cancel: CancelToken,
        on_start: Box<dyn FnMut(HelperKill) + Send>,
    ) -> HelperRenderers {
        HelperRenderers {
            launch,
            fonts,
            cancel,
            limits: SessionLimits::default(),
            on_start,
            stderr: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The helpers' stderr byte count, shared (read it after the export).
    pub fn stderr_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.stderr)
    }
}

/// A [`RenderSession`] that adds its stderr count up when it closes.
struct Counted {
    session: RenderSession,
    stderr: Arc<AtomicU64>,
}

impl PdfRenderer for Counted {
    fn conversation(&mut self, block: &[u8]) -> Result<(), ExportError> {
        self.session.conversation(block)
    }
    fn item(&mut self, block: &[u8]) -> Result<(), ExportError> {
        self.session.item(block)
    }
    fn image(&mut self, n: u64, bytes: &[u8]) -> Result<(), ExportError> {
        self.session.image(n, bytes)
    }
    fn end_volume(&mut self, last: bool) -> Result<Volume, ExportError> {
        self.session.end_volume(last)
    }
    fn close(self: Box<Self>) -> u64 {
        let n = self.session.finish();
        self.stderr.fetch_add(n, Ordering::Relaxed);
        n
    }
}

impl PdfRendererFactory for HelperRenderers {
    fn start(&mut self, options: &PdfOptions) -> Result<Box<dyn PdfRenderer>, ExportError> {
        let begin = Begin {
            backend: BackendKind::Pdf,
            options: options.encode(),
        };
        let mut session = RenderSession::start(&self.launch, &begin, &self.cancel, self.limits)?;
        (self.on_start)(session.kill_handle());
        session.send_fonts(&self.fonts)?;
        Ok(Box::new(Counted {
            session,
            stderr: Arc::clone(&self.stderr),
        }))
    }
}

/// The lines a day heading takes in the estimate.
const DAY_LINES: u64 = 3;

/// When a bundle's volume is cut (spec §5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeLimits {
    pub pages: u64,
    pub bytes: u64,
}

impl VolumeLimits {
    /// 2,000 pages or 40 MiB, estimated.
    pub const SPEC: VolumeLimits = VolumeLimits {
        pages: 2_000,
        bytes: 40 << 20,
    };
}

/// The estimate's constants. Lines and widths are counted generously so
/// that a volume is cut before it truly reaches a limit.
mod estimate {
    /// Lines a page holds (A4 holds about 61 body lines at 9.5 pt; Letter
    /// about 56).
    pub const LINES_PER_PAGE: u64 = 52;
    /// Width units a line holds (a Latin character is one unit, a CJK or
    /// emoji character two); the content width holds about 100.
    pub const UNITS_PER_LINE: u64 = 85;
    /// Lines a preview takes.
    pub const FIGURE_LINES: u64 = 16;
    /// Bytes per drawn character (content stream, text layer, tags):
    /// about 1.5 measured in slice 6j2 (the hostile catalogue's 1 MiB
    /// body), rounded up generously.
    pub const BYTES_PER_CHAR: u64 = 4;
    /// Bytes per distinct character in a volume's font subsets: a colour
    /// emoji (drawn as a Type 3 glyph of vector paths), a CJK ideograph,
    /// any other character.
    pub const EMOJI_GLYPH: u64 = 8 << 10;
    pub const CJK_GLYPH: u64 = 512;
    pub const OTHER_GLYPH: u64 = 256;
    /// A volume's fixed overhead (the outline, the tag tree's skeleton,
    /// the fonts' tables).
    pub const FIXED_BYTES: u64 = 1 << 20;

    /// The subset cost of `c`, the first time a volume draws it.
    pub fn glyph(c: char) -> u64 {
        let u = u32::from(c);
        if u >= 0x1F000 || (0x2600..=0x27BF).contains(&u) {
            EMOJI_GLYPH
        } else if (0x2E80..=0x9FFF).contains(&u)
            || (0xAC00..=0xD7AF).contains(&u)
            || (0x20000..=0x3FFFF).contains(&u)
        {
            CJK_GLYPH
        } else {
            OTHER_GLYPH
        }
    }
}

/// The width units of `s` for the estimate.
fn units(s: &str) -> u64 {
    s.chars()
        .map(|c| {
            let u = u32::from(c);
            if (0x0300..=0x036F).contains(&u) || (0xFE00..=0xFE0F).contains(&u) {
                0
            } else if u >= 0x1100 && !(0x2000..=0x2BFF).contains(&u) {
                2
            } else {
                1
            }
        })
        .sum()
}

/// What a block adds to the volume.
#[derive(Debug, Clone, Copy, Default)]
struct Cost {
    lines: u64,
    pages: u64,
    bytes: u64,
}

impl Cost {
    fn of_text(t: &PdfText, line_units: u64) -> Cost {
        let s = t.as_str();
        let u = units(s);
        Cost {
            lines: u.div_ceil(line_units.max(1)).max(1),
            pages: 0,
            bytes: u64::try_from(s.chars().count())
                .unwrap_or(u64::MAX)
                .saturating_mul(estimate::BYTES_PER_CHAR),
        }
    }

    fn add(&mut self, o: Cost) {
        self.lines = self.lines.saturating_add(o.lines);
        self.pages = self.pages.saturating_add(o.pages);
        self.bytes = self.bytes.saturating_add(o.bytes);
    }

    fn total_pages(&self) -> u64 {
        self.pages
            .saturating_add(self.lines.div_ceil(estimate::LINES_PER_PAGE))
    }
}

/// An entry being built: the day heading it opens (sent with it, after
/// any cut, so a volume never ends with a day heading), its header row,
/// paragraphs and previews.
#[derive(Default)]
struct Pending {
    day: Option<String>,
    head: Vec<PdfText>,
    body: Vec<Para>,
    images: Vec<(u64, CheckedRendition)>,
}

/// The conversation file being written.
#[derive(Debug, Clone, Copy)]
struct FileState {
    stem: FileStem,
    /// The conversation's ordinal (`None`: the no-conversation file).
    conversation: Option<Ordinal>,
    part: u32,
    day: Option<CivilDate>,
    undated: bool,
    items: u64,
    folded: u64,
    /// Entries in the current volume.
    entries: u64,
}

/// The PDF writer.
pub struct PdfWriter {
    factory: Box<dyn PdfRendererFactory>,
    renderer: Option<Box<dyn PdfRenderer>>,
    source: Option<Box<dyn PreviewSource>>,
    limits: VolumeLimits,
    page: PageSize,
    markers: bool,
    previews: bool,
    style: HumanStyle,
    single: bool,
    zone_name: String,
    words: Words,
    handles: BTreeMap<Ordinal, PdfText>,
    /// The conversation heading of the current file (repeated after a cut).
    heading: Option<PdfText>,
    file: Option<FileState>,
    pending: Option<Pending>,
    volume: Cost,
    /// The characters the current volume draws (the subset estimate).
    glyphs: std::collections::BTreeSet<char>,
    counts: MarkCounts,
    report: WriterReport,
    last_item: Option<Ordinal>,
    pages: u64,
    volumes: u64,
    notdef: u64,
    wrapped: u64,
    splits: u64,
    /// Previews sent for the current volume, and drawn in the volumes
    /// returned (the renderer's own count, checked against what was sent).
    volume_figures: u64,
    figures: u64,
    unavailable: u64,
}

impl std::fmt::Debug for PdfWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfWriter")
            .field("single", &self.single)
            .field("volumes", &self.volumes)
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

fn order() -> ExportError {
    ExportError::Limit(LimitKind::RowOrder)
}

fn encode(b: &Block) -> Result<Vec<u8>, ExportError> {
    let v = serde_json::to_vec(b).map_err(|_| ExportError::Limit(LimitKind::NotSerializable))?;
    if v.len() > MAX_BLOCK_BYTES {
        return Err(ExportError::Limit(LimitKind::FrameTooLarge));
    }
    Ok(v)
}

/// The encoded size of a paragraph, generously (JSON may escape a
/// character as six bytes).
fn para_bytes(p: &Para) -> usize {
    p.text.as_str().len().saturating_mul(6).saturating_add(64)
}

impl PdfWriter {
    /// A writer whose renderer `factory` starts; previews from `source`
    /// when the plan has them on.
    pub fn new(
        factory: Box<dyn PdfRendererFactory>,
        source: Option<Box<dyn PreviewSource>>,
    ) -> PdfWriter {
        PdfWriter {
            factory,
            renderer: None,
            source,
            limits: VolumeLimits::SPEC,
            page: PageSize::A4,
            markers: true,
            previews: false,
            style: HumanStyle {
                h23: true,
                seconds: true,
            },
            single: false,
            zone_name: "UTC".to_owned(),
            words: Words::new(),
            handles: BTreeMap::new(),
            heading: None,
            file: None,
            pending: None,
            volume: Cost::default(),
            glyphs: std::collections::BTreeSet::new(),
            counts: MarkCounts::default(),
            report: WriterReport::new(Format::Pdf),
            last_item: None,
            pages: 0,
            volumes: 0,
            notdef: 0,
            wrapped: 0,
            splits: 0,
            volume_figures: 0,
            figures: 0,
            unavailable: 0,
        }
    }

    /// Lower the volume limits (tests).
    pub fn with_limits(mut self, limits: VolumeLimits) -> PdfWriter {
        self.limits = limits;
        self
    }

    /// Width units a line holds at `indent` (Letter is narrower than
    /// A4 by about 3 %, inside the estimate's margin).
    fn line_units(indent: u8) -> u64 {
        estimate::UNITS_PER_LINE.saturating_sub(u64::from(indent) * 2)
    }

    fn renderer(&mut self) -> Result<&mut Box<dyn PdfRenderer>, ExportError> {
        self.renderer.as_mut().ok_or_else(order)
    }

    fn file(&mut self) -> Result<&mut FileState, ExportError> {
        self.file.as_mut().ok_or_else(order)
    }

    fn build(&mut self, b: TextBuilder) -> PdfText {
        let (t, n) = b.build();
        self.counts.add(n);
        t
    }

    // ---------------------------------------------------------------------
    // Sending blocks
    // ---------------------------------------------------------------------

    fn send_item(&mut self, b: &Block) -> Result<(), ExportError> {
        let v = encode(b)?;
        self.renderer()?.item(&v)
    }

    /// The conversation heading of the current file, with its document
    /// title; "(part N)" after a cut.
    fn send_conversation_heading(&mut self) -> Result<(), ExportError> {
        let f = *self.file.as_ref().ok_or_else(order)?;
        let mut title = match f.conversation {
            Some(n) => format!("{} \u{2014} conversation {n}", pdf_text::TITLE),
            None => format!("{} \u{2014} messages in no conversation", pdf_text::TITLE),
        };
        let mut text = self.heading.clone().ok_or_else(order)?;
        if f.part > 1 {
            title.push_str(&format!(", part {}", f.part));
            text = TextBuilder::new()
                .piece(&text)
                .ours(&format!(" (part {})", f.part))
                .build()
                .0;
        }
        let b = Block::Heading {
            level: HeadingLevel::Conversation,
            text,
            title: Some(title),
        };
        let v = encode(&b)?;
        self.renderer()?.conversation(&v)?;
        self.volume.pages = self.volume.pages.saturating_add(1);
        Ok(())
    }

    fn send_day(&mut self, text: &str) -> Result<(), ExportError> {
        let b = Block::Heading {
            level: HeadingLevel::Day,
            text: PdfText::ours(text),
            title: None,
        };
        self.volume.lines = self.volume.lines.saturating_add(DAY_LINES);
        self.send_item(&b)
    }

    /// The current day heading's words (repeated after a cut).
    fn day_words(f: &FileState) -> Option<String> {
        if f.undated {
            Some(pdf_text::UNDATED.to_owned())
        } else {
            f.day.map(date_text)
        }
    }

    /// End the current volume and stage it.
    fn end_volume(&mut self, out: &mut Staging, last: bool) -> Result<(), ExportError> {
        let f = *self.file.as_ref().ok_or_else(order)?;
        let path = BundlePath::pdf_volume(f.stem, f.part);
        let v = self.renderer()?.end_volume(last)?;
        // The renderer drew exactly the previews this volume sent (checked
        // before a byte is staged).
        if v.counts.figures != self.volume_figures {
            return Err(ExportError::HelperFailed(HelperFailure::Protocol));
        }
        let mut file = out.create(&path)?;
        let written = file
            .write_all(&v.bytes)
            .and_then(|()| file.flush())
            .map_err(|e| ExportError::io(&e));
        if let Err(e) = written {
            let _ = out.discard(file);
            return Err(e);
        }
        out.close(file)?;
        let c = v.counts;
        self.volume_figures = 0;
        self.figures = self.figures.saturating_add(c.figures);
        self.pages = self.pages.saturating_add(c.units);
        self.notdef = self.notdef.saturating_add(c.notdef_chars);
        self.wrapped = self.wrapped.saturating_add(c.wrapped_tokens);
        self.volumes += 1;
        self.volume = Cost::default();
        self.glyphs.clear();
        Ok(())
    }

    /// Cut the volume before `cost` would pass a limit (bundles only, and
    /// never before the volume's first entry). After a cut the new volume
    /// has its conversation heading and the current day heading; returns
    /// whether it cut.
    fn maybe_cut(&mut self, cost: Cost, out: &mut Staging) -> Result<bool, ExportError> {
        let f = *self.file.as_ref().ok_or_else(order)?;
        if self.single || f.entries == 0 {
            return Ok(false);
        }
        let mut next = self.volume;
        next.add(cost);
        let bytes = next.bytes.saturating_add(estimate::FIXED_BYTES);
        if next.total_pages() <= self.limits.pages && bytes <= self.limits.bytes {
            return Ok(false);
        }
        self.end_volume(out, false)?;
        let f = self.file()?;
        f.part += 1;
        f.entries = 0;
        let day = Self::day_words(f);
        self.send_conversation_heading()?;
        if let Some(d) = day {
            self.send_day(&d)?;
        }
        Ok(true)
    }

    /// Split paragraphs longer than [`MAX_PARA_BYTES`] (each split is
    /// counted with the forced wraps).
    fn split_long(&mut self, paras: Vec<Para>) -> Vec<Para> {
        let mut body = Vec::with_capacity(paras.len());
        for para in paras {
            if para.text.as_str().len() <= MAX_PARA_BYTES {
                body.push(para);
                continue;
            }
            let pieces = para.text.split_bytes(MAX_PARA_BYTES);
            self.splits += u64::try_from(pieces.len().saturating_sub(1)).unwrap_or(0);
            for t in pieces {
                body.push(Para::built(para.indent, para.role, t));
            }
        }
        body
    }

    /// Send the pending entry: its day heading if it opens a day, then
    /// its block(s), each after the previews it draws.
    fn flush(&mut self, out: &mut Staging) -> Result<(), ExportError> {
        let Some(p) = self.pending.take() else {
            return Ok(());
        };
        let body = self.split_long(p.body);
        let mut cost = Cost::default();
        for h in &p.head {
            cost.bytes = cost.bytes.saturating_add(Cost::of_text(h, 1).bytes);
        }
        cost.lines = 1 + u64::try_from(p.head.len() / 6).unwrap_or(0);
        if p.day.is_some() {
            cost.lines = cost.lines.saturating_add(DAY_LINES);
        }
        for para in &body {
            if para.role == ParaRole::Figure {
                cost.lines = cost.lines.saturating_add(estimate::FIGURE_LINES);
            } else {
                cost.add(Cost::of_text(&para.text, Self::line_units(para.indent)));
            }
        }
        for (_, r) in &p.images {
            cost.bytes = cost
                .bytes
                .saturating_add(u64::try_from(r.bytes().len()).unwrap_or(u64::MAX));
        }
        let texts = p.head.iter().chain(body.iter().map(|b| &b.text));
        cost.bytes = cost.bytes.saturating_add(self.new_glyphs(texts.clone()));
        let cut = self.maybe_cut(cost, out)?;
        self.seen_glyphs(texts);
        if let (false, Some(d)) = (cut, p.day) {
            self.send_day(&d)?;
        }
        self.send_entry(p.head, body, p.images)?;
        self.volume.add(cost);
        self.file()?.entries += 1;
        Ok(())
    }

    /// Send an entry as one block, or several under the block bound and
    /// [`MAX_FIGURES_PER_BLOCK`] (a continuation has an empty header
    /// row). Each block's previews go as `Image` frames just before it.
    fn send_entry(
        &mut self,
        head: Vec<PdfText>,
        body: Vec<Para>,
        images: Vec<(u64, CheckedRendition)>,
    ) -> Result<(), ExportError> {
        let mut images: BTreeMap<u64, CheckedRendition> = images.into_iter().collect();
        let mut head = head;
        let mut chunk: Vec<Para> = Vec::new();
        let mut size = head
            .iter()
            .map(|h| h.as_str().len().saturating_mul(6))
            .sum::<usize>();
        let mut figures = 0usize;
        for para in body {
            let b = para_bytes(&para);
            let figure = para.role == ParaRole::Figure;
            let full = size.saturating_add(b) > MAX_BLOCK_BYTES / 2
                || (figure && figures >= MAX_FIGURES_PER_BLOCK);
            if !chunk.is_empty() && full {
                let body = std::mem::take(&mut chunk);
                self.send_block(std::mem::take(&mut head), body, &mut images)?;
                size = 0;
                figures = 0;
            }
            size = size.saturating_add(b);
            figures += usize::from(figure);
            chunk.push(para);
        }
        self.send_block(head, chunk, &mut images)
    }

    /// One block of an entry, after the previews its figures draw.
    fn send_block(
        &mut self,
        head: Vec<PdfText>,
        body: Vec<Para>,
        images: &mut BTreeMap<u64, CheckedRendition>,
    ) -> Result<(), ExportError> {
        for p in &body {
            if let (ParaRole::Figure, Some(n)) = (p.role, p.image) {
                if let Some(r) = images.remove(&n) {
                    self.renderer()?.image(n, r.bytes())?;
                    self.volume_figures += 1;
                }
            }
        }
        self.send_item(&Block::Entry { head, body })
    }

    /// Paragraphs of our own words (the conversation's facts, the footer,
    /// About), with any marked value they hold; split and chunked like an
    /// entry, so a long recorded name cannot pass the block bound.
    fn send_labels(&mut self, lines: Vec<PdfText>) -> Result<(), ExportError> {
        let body: Vec<Para> = lines
            .into_iter()
            .map(|t| Para::built(0, ParaRole::Label, t))
            .collect();
        let body = self.split_long(body);
        let mut cost = Cost::default();
        for p in &body {
            cost.add(Cost::of_text(&p.text, Self::line_units(0)));
        }
        cost.bytes = cost
            .bytes
            .saturating_add(self.new_glyphs(body.iter().map(|b| &b.text)));
        self.seen_glyphs(body.iter().map(|b| &b.text));
        self.volume.add(cost);
        self.send_entry(Vec::new(), body, Vec::new())
    }

    /// The subset cost of the characters `texts` would add to the volume.
    fn new_glyphs<'a>(&self, texts: impl Iterator<Item = &'a PdfText>) -> u64 {
        let mut fresh = std::collections::BTreeSet::new();
        for t in texts {
            for c in t.as_str().chars() {
                if !self.glyphs.contains(&c) {
                    fresh.insert(c);
                }
            }
        }
        fresh
            .into_iter()
            .map(estimate::glyph)
            .fold(0u64, u64::saturating_add)
    }

    fn seen_glyphs<'a>(&mut self, texts: impl Iterator<Item = &'a PdfText>) {
        for t in texts {
            self.glyphs.extend(t.as_str().chars());
        }
    }

    fn pending(&mut self) -> Result<&mut Pending, ExportError> {
        self.pending.as_mut().ok_or_else(order)
    }

    // ---------------------------------------------------------------------
    // The item's header row
    // ---------------------------------------------------------------------

    /// The item's header row, and the day heading it opens (if any; the
    /// file's current day moves to it now, the heading is sent with the
    /// entry).
    fn head(&mut self, it: &ItemRecord) -> Result<(Vec<PdfText>, Option<String>), ExportError> {
        let undated = is_undated(it);
        let f = *self.file.as_ref().ok_or_else(order)?;
        let mut opens = None;
        let time = match (time_text(&it.date, self.style), undated) {
            (_, true) => {
                if !f.undated {
                    self.file()?.undated = true;
                    opens = Some(pdf_text::UNDATED.to_owned());
                }
                pdf_text::UNDATED.to_owned()
            }
            (Some((t, day)), false) => {
                if f.day != Some(day) || f.undated {
                    let fs = self.file()?;
                    fs.day = Some(day);
                    fs.undated = false;
                    opens = Some(date_text(day));
                }
                t
            }
            (None, false) => pdf_text::NO_TIME.to_owned(),
        };
        let SenderTags { resolved, mut tags } = sender_tags(&self.words, it);
        item_tags(&self.words, it, &mut tags);
        let mut head = vec![
            PdfText::ours(&time),
            PdfText::ours(&format!("item {}", it.n)),
        ];
        if resolved {
            let sender = match it.sender.handle.and_then(|h| self.handles.get(&h).cloned()) {
                Some(v) => v,
                None => PdfText::ours(&self.words.say("senderReason", it.sender.reason)),
            };
            head.push(sender);
        }
        for t in tags {
            let b = TextBuilder::new().ours("[").header(&t).ours("]");
            let t = self.build(b);
            head.push(t);
        }
        head.truncate(MAX_HEAD_PIECES);
        Ok((head, opens))
    }

    fn about(&mut self, about: &AboutDraft<'_>) -> Result<(), ExportError> {
        let b = Block::Heading {
            level: HeadingLevel::Section,
            text: PdfText::ours(pdf_text::ABOUT),
            title: None,
        };
        self.send_item(&b)?;
        let v = serde_json::to_value(about.body)
            .map_err(|_| ExportError::Limit(LimitKind::NotSerializable))?;
        let mut flat = Vec::new();
        flatten("", &v, &mut flat);
        // Every value is a number, a boolean, a closed token or a
        // validated name, but mark anyway, as TXT does.
        let lines: Vec<PdfText> = flat
            .iter()
            .map(|l| {
                let (t, n) = TextBuilder::new().header(l).build();
                self.counts.add(n);
                t
            })
            .collect();
        for chunk in lines.chunks(256) {
            self.send_labels(chunk.to_vec())?;
        }
        Ok(())
    }
}

/// A line's parts as one `PdfText` (values already marked).
fn concat(parts: Vec<Part<PdfText>>) -> TextBuilder {
    let mut b = TextBuilder::new();
    for p in parts {
        b = match p {
            Part::Ours(s) => b.ours(&s),
            Part::Value(v) => b.piece(&v),
        };
    }
    b
}

impl Emit for PdfWriter {
    type V = PdfText;

    fn words(&self) -> &Words {
        &self.words
    }

    fn value(&mut self, raw: &str) -> PdfText {
        let b = TextBuilder::new().header(raw);
        self.build(b)
    }

    fn handle_value(&self, h: Ordinal) -> Option<PdfText> {
        self.handles.get(&h).cloned()
    }

    fn label(&mut self, parts: Vec<Part<PdfText>>) -> Result<(), ExportError> {
        let t = self.build(concat(parts));
        self.pending()?
            .body
            .push(Para::built(1, ParaRole::Label, t));
        Ok(())
    }

    fn content(&mut self, parts: Vec<Part<PdfText>>) -> Result<(), ExportError> {
        let t = self.build(concat(parts));
        self.pending()?
            .body
            .push(Para::built(2, ParaRole::Label, t));
        Ok(())
    }

    fn text(&mut self, label: Vec<Part<PdfText>>, runs: &[Run<'_>]) -> Result<(), ExportError> {
        self.label(label)?;
        let markers = self.markers;
        let mut lines = vec![TextBuilder::new()];
        // The recorded text between two placeholders or line breaks is
        // marked as one string: the pieces carry no styles in PDF, and a
        // combining run split across formatting segments must be cut like
        // one in a single segment (review fix, §4.7).
        let mut pending = String::new();
        let flush = |lines: &mut Vec<TextBuilder>, pending: &mut String| {
            if !pending.is_empty() {
                if let Some(l) = lines.last_mut() {
                    l.push_content(pending, markers);
                }
                pending.clear();
            }
        };
        for r in runs {
            match r {
                Run::Placeholder(p) => {
                    flush(&mut lines, &mut pending);
                    if let Some(l) = lines.last_mut() {
                        l.push_ours(p);
                    }
                }
                Run::Text(t) => {
                    for (i, part) in split_lines(t).into_iter().enumerate() {
                        if i > 0 {
                            flush(&mut lines, &mut pending);
                            lines.push(TextBuilder::new());
                        }
                        pending.push_str(part);
                    }
                }
            }
        }
        flush(&mut lines, &mut pending);
        for l in lines {
            let t = self.build(l);
            self.pending()?.body.push(Para::built(2, ParaRole::Text, t));
        }
        Ok(())
    }

    fn after_attachment(&mut self, a: &AttachmentRecord) -> Result<(), ExportError> {
        let asked = self.previews
            && (is(a.access, AttachmentAccessCode::Included)
                || is(a.access, AttachmentAccessCode::Revealed));
        let p = match (asked, self.source.as_mut()) {
            (true, Some(src)) => src.renditions(a, false)?,
            _ => Preview::None,
        };
        match p {
            Preview::None => {}
            Preview::Unavailable => {
                self.unavailable += 1;
                self.pending()?
                    .body
                    .push(Para::label(2, pdf_text::PREVIEW_UNAVAILABLE));
            }
            Preview::Ready { thumb, .. } => {
                let r = check_rendition(thumb, RenditionSize::Thumb)?;
                let n = a.n;
                let pending = self.pending()?;
                pending.images.retain(|(k, _)| *k != n);
                pending.images.push((n, r));
                pending.body.push(Para::figure(2, n));
            }
        }
        Ok(())
    }
}

impl ExportWriter for PdfWriter {
    fn format(&self) -> Format {
        Format::Pdf
    }

    fn begin(&mut self, ctx: &ExportContext<'_>, _out: &mut Staging) -> Result<(), ExportError> {
        let o = ctx.plan.options();
        self.markers = o.hidden_character_markers;
        self.previews = o.previews;
        self.style = HumanStyle {
            h23: o.clock == Clock::H23,
            seconds: o.seconds,
        };
        self.page = match o.page_size {
            Some(plan::PageSize::Letter) => PageSize::Letter,
            Some(plan::PageSize::A4) | None => PageSize::A4,
        };
        self.single = ctx.plan.packaging() == Packaging::SingleFile;
        self.zone_name = match ctx.zone {
            ExportZone::Utc => "UTC".to_owned(),
            ExportZone::Table(t) => t.name().to_owned(),
        };
        Ok(())
    }

    fn handle(&mut self, h: &HandleRecord, _out: &mut Staging) -> Result<(), ExportError> {
        let a = match &h.address.text {
            Some(a) if !a.as_str().is_empty() => {
                let b = TextBuilder::new().header(a.as_str());
                self.build(b)
            }
            _ => PdfText::ours(&format!("address {} (not recorded)", h.n)),
        };
        self.handles.insert(h.n, a);
        Ok(())
    }

    fn begin_conversation(
        &mut self,
        c: ConversationFile<'_>,
        _out: &mut Staging,
    ) -> Result<(), ExportError> {
        if self.file.is_some() || (self.single && self.report.conversation_files > 0) {
            return Err(order());
        }
        if self.renderer.is_none() {
            let options = PdfOptions {
                page: self.page,
                han: HanFace::Simplified,
                title: pdf_text::TITLE.to_owned(),
            };
            self.renderer = Some(self.factory.start(&options)?);
        }
        let (stem, conversation) = match c {
            ConversationFile::Conversation(r) => (FileStem::Conversation(r.n), Some(r.n)),
            ConversationFile::NoConversation => (FileStem::NoConversation, None),
        };
        self.file = Some(FileState {
            stem,
            conversation,
            part: 1,
            day: None,
            undated: false,
            items: 0,
            folded: 0,
            entries: 0,
        });
        self.report.conversation_files += 1;
        let mut facts = vec![PdfText::ours(if self.single {
            pdf_text::NOTICE_SINGLE
        } else {
            pdf_text::NOTICE
        })];
        let heading = match c {
            ConversationFile::Conversation(r) => {
                let b = TextBuilder::new().ours(&format!("Conversation {}: ", r.n));
                let b = match &r.name.text {
                    Some(n) if !n.as_str().is_empty() => b.header(n.as_str()),
                    _ => b.ours("No name recorded"),
                };
                let heading = self.build(b);
                facts.push(PdfText::ours(&format!(
                    "Addresses recorded in this conversation (includes people who left): {}",
                    r.participants.len()
                )));
                for h in &r.name_history {
                    let v = lines::name_history_value(self, h);
                    let t = self.build(concat(vec![
                        Part::Ours("Other recorded name of this conversation: ".to_owned()),
                        v,
                    ]));
                    facts.push(t);
                }
                heading
            }
            ConversationFile::NoConversation => {
                PdfText::ours("No conversation: messages recorded in no conversation")
            }
        };
        facts.push(PdfText::ours(&format!(
            "Times: {} (offsets shown on every item)",
            self.zone_name
        )));
        self.heading = Some(heading);
        self.send_conversation_heading()?;
        self.send_labels(facts)
    }

    fn item(&mut self, it: &ItemRecord, out: &mut Staging) -> Result<(), ExportError> {
        if self.file.is_none() {
            return Err(order());
        }
        self.flush(out)?;
        if it.context.folded_into.is_some() {
            let f = self.file()?;
            f.folded += 1;
            self.report.folded_rows += 1;
            self.last_item = None;
            return Ok(());
        }
        let (head, day) = self.head(it)?;
        self.pending = Some(Pending {
            day,
            head,
            ..Pending::default()
        });
        self.last_item = Some(it.n);
        lines::body(self, it)?;
        lines::details(self, it)?;
        lines::attachments(self, it)?;
        lines::reactions(self, it)?;
        lines::history(self, it)?;
        self.file()?.items += 1;
        self.report.items += 1;
        Ok(())
    }

    fn version(&mut self, v: &VersionRecord, _out: &mut Staging) -> Result<(), ExportError> {
        if self.file.is_none() {
            return Err(order());
        }
        // As in TXT: a folded reaction row's versions are in the
        // structured formats only.
        if self.last_item != Some(v.message) || self.pending.is_none() {
            return Ok(());
        }
        lines::version(self, v)
    }

    fn end_conversation(
        &mut self,
        totals: &ConversationTotals,
        out: &mut Staging,
    ) -> Result<(), ExportError> {
        self.flush(out)?;
        let f = *self.file.as_ref().ok_or_else(order)?;
        if totals.items != f.items || totals.folded_rows != f.folded {
            return Err(ExportError::Limit(LimitKind::RowCountMismatch));
        }
        self.send_item(&Block::Heading {
            level: HeadingLevel::Day,
            text: PdfText::ours(pdf_text::END),
            title: None,
        })?;
        let mut footer = vec![
            PdfText::ours(&format!("Items: {}", f.items)),
            PdfText::ours(&format!(
                "Reactions shown on the messages they react to: {}",
                f.folded
            )),
            PdfText::ours(pdf_text::LEGEND_HEADING),
        ];
        footer.extend(pdf_text::LEGEND.iter().map(|l| PdfText::ours(l)));
        if !self.single {
            footer.push(PdfText::ours(pdf_text::ABOUT_BUNDLE));
        }
        self.send_labels(footer)?;
        self.last_item = None;
        if !self.single {
            self.end_volume(out, false)?;
            self.file = None;
        }
        Ok(())
    }

    fn finish(
        mut self: Box<Self>,
        about: &AboutDraft<'_>,
        out: &mut Staging,
    ) -> Result<WriterReport, ExportError> {
        if self.single {
            if self.file.is_none() {
                return Err(order());
            }
            self.about(about)?;
            self.end_volume(out, true)?;
            self.file = None;
        } else if self.file.is_some() {
            return Err(ExportError::Limit(LimitKind::FileStillOpen));
        }
        if let Some(r) = self.renderer.take() {
            r.close();
        }
        let mut r = self.report.clone();
        let c = self.counts;
        r.counts.insert("hiddenCharactersMarked", c.hidden_marked);
        r.counts.insert("controlsEscaped", c.controls_escaped);
        r.counts
            .insert("combiningRunsLimited", c.combining_runs_limited);
        r.counts.insert("reservedEscaped", c.reserved_escaped);
        r.counts.insert("pdfUndrawableCharacters", self.notdef);
        r.counts
            .insert("pdfForcedWraps", self.wrapped.saturating_add(self.splits));
        r.counts.insert("pdfPages", self.pages);
        r.counts.insert("pdfVolumes", self.volumes);
        r.counts.insert("previewsEmbedded", self.figures);
        r.counts.insert("previewsUnavailable", self.unavailable);
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_units_count_wide_characters_twice_and_marks_not_at_all() {
        assert_eq!(units("abc"), 3);
        assert_eq!(units("中文"), 4);
        assert_eq!(units("e\u{301}"), 1);
        assert_eq!(units("😀"), 2);
        // General punctuation and arrows are narrow.
        assert_eq!(units("\u{2014}\u{27E8}"), 2);
    }

    #[test]
    fn the_estimate_counts_lines_pages_and_bytes() {
        let t = PdfText::ours(&"x".repeat(170));
        let c = Cost::of_text(&t, 85);
        assert_eq!((c.lines, c.bytes), (2, 170 * estimate::BYTES_PER_CHAR));
        assert_eq!(estimate::glyph('😀'), estimate::EMOJI_GLYPH);
        assert_eq!(estimate::glyph('中'), estimate::CJK_GLYPH);
        assert_eq!(estimate::glyph('a'), estimate::OTHER_GLYPH);
        let mut v = Cost::default();
        for _ in 0..26 {
            v.add(c);
        }
        assert_eq!(v.total_pages(), 1);
        v.add(c);
        assert_eq!(v.total_pages(), 2);
        v.pages = 3;
        assert_eq!(v.total_pages(), 5);
    }

    /// Records what an entry sends: `I<n>` for an image, `B<figures>`
    /// for a block and the figures it draws.
    struct Log(Arc<std::sync::Mutex<Vec<String>>>);

    impl PdfRenderer for Log {
        fn conversation(&mut self, _b: &[u8]) -> Result<(), ExportError> {
            Ok(())
        }
        fn item(&mut self, b: &[u8]) -> Result<(), ExportError> {
            let block = Block::decode(b).expect("valid block");
            let Block::Entry { body, .. } = block else {
                panic!("an entry");
            };
            let figs: Vec<String> = body
                .iter()
                .filter_map(|p| p.image.map(|n| n.to_string()))
                .collect();
            self.0
                .lock()
                .expect("lock")
                .push(format!("B{}", figs.join(",")));
            Ok(())
        }
        fn image(&mut self, n: u64, _b: &[u8]) -> Result<(), ExportError> {
            self.0.lock().expect("lock").push(format!("I{n}"));
            Ok(())
        }
        fn end_volume(&mut self, _last: bool) -> Result<Volume, ExportError> {
            Err(order())
        }
        fn close(self: Box<Self>) -> u64 {
            0
        }
    }

    struct NoFactory;

    impl PdfRendererFactory for NoFactory {
        fn start(&mut self, _o: &PdfOptions) -> Result<Box<dyn PdfRenderer>, ExportError> {
            Err(order())
        }
    }

    /// A real 1 x 1 PNG.
    const PNG_1X1: &str = "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d4944415478da636460f85f0f0002870180eb47ba920000000049454e44ae426082";

    #[test]
    fn an_entry_sends_each_blocks_previews_just_before_it_and_bounds_them() {
        let png: Vec<u8> = (0..PNG_1X1.len())
            .step_by(2)
            .filter_map(|i| u8::from_str_radix(PNG_1X1.get(i..i + 2)?, 16).ok())
            .collect();
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut w = PdfWriter::new(Box::new(NoFactory), None);
        w.renderer = Some(Box::new(Log(Arc::clone(&log))));
        // 70 previews in one entry: more than the helper holds waiting.
        let mut body = vec![Para::label(1, "label")];
        let mut images = Vec::new();
        for n in 1..=70u64 {
            let r = check_rendition(
                crate::preview::RenditionBytes(png.clone()),
                RenditionSize::Thumb,
            )
            .expect("checked");
            images.push((n, r));
            body.push(Para::figure(2, n));
        }
        w.send_entry(vec![PdfText::ours("12:00")], body, images)
            .expect("sent");
        let log = log.lock().expect("lock").clone();
        let blocks: Vec<&String> = log.iter().filter(|e| e.starts_with('B')).collect();
        assert_eq!(blocks.len(), 3, "{log:?}");
        // Every image comes just before the block that draws it, and no
        // block draws more than the bound.
        let mut waiting: Vec<String> = Vec::new();
        for e in &log {
            if let Some(n) = e.strip_prefix('I') {
                waiting.push(n.to_owned());
            } else {
                let figs: Vec<String> = e
                    .strip_prefix('B')
                    .unwrap_or("")
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
                assert!(figs.len() <= MAX_FIGURES_PER_BLOCK);
                assert_eq!(waiting, figs);
                waiting.clear();
            }
        }
        assert!(waiting.is_empty());
    }

    /// Review finding: a combining flood split across formatting segments
    /// reached the PDF uncut, because each segment was marked alone.
    #[test]
    fn a_combining_flood_split_across_segments_is_cut_once() {
        let marks = "\u{301}".repeat(16);
        let mut runs = vec![Run::Text("e")];
        for _ in 0..600 {
            runs.push(Run::Text(&marks));
        }
        let mut w = PdfWriter::new(Box::new(NoFactory), None);
        w.pending = Some(Pending {
            day: None,
            head: Vec::new(),
            body: Vec::new(),
            images: Vec::new(),
        });
        Emit::text(&mut w, Vec::new(), &runs).expect("text");
        let body = &w.pending.as_ref().expect("pending").body;
        let text = body.last().expect("a paragraph").text.as_str();
        let kept = text.chars().filter(|c| *c == '\u{301}').count();
        assert_eq!(kept, crate::hidden::COMBINING_RUN_LIMIT);
        assert!(text.ends_with("\u{27E8}+9584 combining marks\u{27E9}"));
        assert_eq!(w.counts.combining_runs_limited, 1);
    }

    #[test]
    fn volume_paths_count_parts_from_one() {
        let s = FileStem::Conversation(3);
        assert_eq!(
            BundlePath::pdf_volume(s, 1),
            BundlePath::format_file(Format::Pdf, s, crate::naming::FileExt::Pdf)
        );
        assert_ne!(BundlePath::pdf_volume(s, 2), BundlePath::pdf_volume(s, 1));
    }
}
