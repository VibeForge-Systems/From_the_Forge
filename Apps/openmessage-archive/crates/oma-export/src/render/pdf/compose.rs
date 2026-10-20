//! Pages: blocks placed into lines, lines onto pages, pages into one
//! krilla document per volume (M6 spec §5.6).
//!
//! A transcript, not bubbles: 15 mm margins, Noto Sans 9.5 pt body. An
//! entry is its header line (the time, the sender and each qualifier, each
//! laid out alone so each is its own bidi isolate, flowed left to right
//! and wrapped between pieces) and its paragraphs, indented by level. A
//! conversation heading starts a new page and a top-level outline entry;
//! a day heading adds an outline entry under it. Headings are kept with
//! the line after them. The document information title is the generic
//! title from the options (D112).
//!
//! Lines are kept for the page being filled and drawn when it is full:
//! every glyph is drawn with its paragraph's text as krilla's text, so
//! the text layer (ToUnicode, and ActualText where a glyph's text is not
//! its own) is exactly the drawn text (§4.7).

use krilla::destination::XyzDestination;
use krilla::geom::Point;
use krilla::metadata::Metadata;
use krilla::outline::{Outline, OutlineNode};
use krilla::page::PageSettings;
use krilla::text::{GlyphId, KrillaGlyph};
use krilla::Document;

use super::layout::{Paragraph, PlacedLine, Typesetter};
use super::library::Library;
use crate::pdf::doc::{HeadingLevel, PageSize, Para, PdfText};

/// Millimetres to points.
const MM: f32 = 72.0 / 25.4;
/// The margin on every side.
pub const MARGIN: f32 = 15.0 * MM;
/// The body size.
pub const BODY: f32 = 9.5;
/// A conversation or section heading's size.
pub const TITLE: f32 = 13.0;
/// A day heading's size.
pub const DAY: f32 = 10.5;
/// One indent level.
pub const INDENT: f32 = 12.0;
/// The gap between header pieces, as a multiple of the body size.
const PIECE_GAP: f32 = 0.45;
/// The space after an entry.
const ENTRY_GAP: f32 = 4.0;
/// The space before a heading.
const HEADING_GAP: f32 = 8.0;

/// A line placed on the current page (no `Debug`: it holds its text).
#[derive(Clone)]
struct Placed {
    /// The left edge of the line's box.
    x: f32,
    /// The top of the line.
    y: f32,
    line: PlacedLine,
    text: std::sync::Arc<str>,
}

/// What the document's glyphs covered (tests check no glyph is clipped).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Extent {
    pub min_x: f32,
    pub max_x: f32,
    pub min_y: f32,
    pub max_y: f32,
    pub glyphs: u64,
}

/// What a volume reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VolumeCounts {
    pub pages: u64,
    pub notdef_chars: u64,
    pub wrapped_tokens: u64,
    pub combining_runs: u64,
}

/// One volume being composed.
pub struct Composer {
    page: (f32, f32),
    title: String,
    doc: Document,
    placed: Vec<Placed>,
    /// The next line's top on the current page.
    y: f32,
    /// Pages finished so far (the current page's index).
    pages: usize,
    outline: Outline,
    conversation: Option<OutlineNode>,
    counts: VolumeCounts,
    extent: Option<Extent>,
}

impl std::fmt::Debug for Composer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Composer")
            .field("pages", &self.pages)
            .field("counts", &self.counts)
            .finish_non_exhaustive()
    }
}

/// Why a volume could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeError {
    /// krilla refused the page size.
    Page,
    /// krilla could not serialise the document.
    Serialise,
}

fn count_combining_markers(text: &str) -> u64 {
    let n = text.matches("\u{27E8}+").count();
    u64::try_from(n).unwrap_or(u64::MAX)
}

impl Composer {
    pub fn new(size: PageSize, title: &str) -> Composer {
        Composer {
            page: size.points(),
            title: title.to_owned(),
            doc: Document::new(),
            placed: Vec::new(),
            y: MARGIN,
            pages: 0,
            outline: Outline::new(),
            conversation: None,
            counts: VolumeCounts::default(),
            extent: None,
        }
    }

    fn content_width(&self) -> f32 {
        self.page.0 - 2.0 * MARGIN
    }

    fn bottom(&self) -> f32 {
        self.page.1 - MARGIN
    }

    /// The glyphs' extent so far (`None` before the first glyph).
    pub fn extent(&self) -> Option<Extent> {
        self.extent
    }

    fn note(&mut self, p: &Paragraph) {
        self.counts.notdef_chars += p.notdef_chars;
        self.counts.wrapped_tokens += p.forced_wraps;
        self.counts.combining_runs += count_combining_markers(&p.text);
    }

    /// Draw the current page's lines and start a new page.
    fn flush_page(&mut self, fonts: &mut Library) -> Result<(), ComposeError> {
        let settings = PageSettings::from_wh(self.page.0, self.page.1).ok_or(ComposeError::Page)?;
        let placed = std::mem::take(&mut self.placed);
        let mut extent = self.extent;
        let mut page = self.doc.start_page_with(settings);
        let mut surface = page.surface();
        for p in &placed {
            let baseline = p.y + p.line.baseline;
            for r in &p.line.runs {
                let Some(font) = fonts.krilla_font(r.slot) else {
                    continue;
                };
                let glyphs: Vec<KrillaGlyph> = r
                    .glyphs
                    .iter()
                    .map(|g| {
                        KrillaGlyph::new(
                            GlyphId::new(g.id),
                            g.x_advance,
                            g.x_offset,
                            g.y_offset,
                            0.0,
                            g.range.clone(),
                            None,
                        )
                    })
                    .collect();
                let x0 = p.x + r.x;
                let e = extent.get_or_insert(Extent {
                    min_x: x0,
                    max_x: x0,
                    min_y: p.y,
                    max_y: p.y,
                    glyphs: 0,
                });
                e.min_x = e.min_x.min(x0);
                e.max_x = e.max_x.max(x0 + r.advance);
                e.min_y = e.min_y.min(p.y);
                e.max_y = e.max_y.max(p.y + p.line.height);
                e.glyphs += u64::try_from(glyphs.len()).unwrap_or(0);
                surface.draw_glyphs(
                    Point::from_xy(x0, baseline),
                    &glyphs,
                    font,
                    &p.text,
                    r.size,
                    false,
                );
            }
        }
        surface.finish();
        page.finish();
        self.extent = extent;
        self.pages += 1;
        self.y = MARGIN;
        Ok(())
    }

    /// Make room for `height` points, starting a new page if needed.
    fn room(&mut self, fonts: &mut Library, height: f32) -> Result<(), ComposeError> {
        if self.y + height > self.bottom() && (self.y > MARGIN || !self.placed.is_empty()) {
            self.flush_page(fonts)?;
        }
        Ok(())
    }

    fn place(
        &mut self,
        fonts: &mut Library,
        x: f32,
        line: PlacedLine,
        text: &std::sync::Arc<str>,
    ) -> Result<(), ComposeError> {
        self.room(fonts, line.height)?;
        let height = line.height;
        self.placed.push(Placed {
            x,
            y: self.y,
            line,
            text: std::sync::Arc::clone(text),
        });
        self.y += height;
        Ok(())
    }

    fn page_started(&self) -> bool {
        self.y > MARGIN || !self.placed.is_empty()
    }

    /// A heading.
    pub fn heading(
        &mut self,
        ts: &mut Typesetter,
        level: HeadingLevel,
        text: &PdfText,
    ) -> Result<(), ComposeError> {
        let size = match level {
            HeadingLevel::Day => DAY,
            HeadingLevel::Conversation | HeadingLevel::Section => TITLE,
        };
        let para = ts.paragraph(text.as_str(), self.content_width(), size);
        self.note(&para);
        let height: f32 = para.lines.iter().map(|l| l.height).sum();
        match level {
            HeadingLevel::Conversation | HeadingLevel::Section => {
                if self.page_started() {
                    self.flush_page(&mut ts.fonts)?;
                }
            }
            HeadingLevel::Day => {
                if self.page_started() {
                    self.y += HEADING_GAP;
                }
                // Keep the heading with the line after it.
                self.room(
                    &mut ts.fonts,
                    height + BODY * 2.0 * super::layout::LINE_HEIGHT,
                )?;
            }
        }
        let dest = XyzDestination::new(self.pages, Point::from_xy(MARGIN, self.y));
        let node = OutlineNode::new(text.as_str().to_owned(), dest);
        match level {
            HeadingLevel::Conversation => {
                if let Some(done) = self.conversation.take() {
                    self.outline.push_child(done);
                }
                self.conversation = Some(node);
            }
            HeadingLevel::Section => {
                if let Some(done) = self.conversation.take() {
                    self.outline.push_child(done);
                }
                self.outline.push_child(node);
            }
            HeadingLevel::Day => match self.conversation.as_mut() {
                Some(c) => c.push_child(node),
                None => self.outline.push_child(node),
            },
        }
        let text = std::sync::Arc::clone(&para.text);
        for line in para.lines {
            self.place(&mut ts.fonts, MARGIN, line, &text)?;
        }
        self.y += ENTRY_GAP;
        Ok(())
    }

    /// A transcript entry.
    pub fn entry(
        &mut self,
        ts: &mut Typesetter,
        head: &[PdfText],
        body: &[Para],
    ) -> Result<(), ComposeError> {
        let width = self.content_width();
        let gap = BODY * PIECE_GAP;
        // The header row: pieces flowed left to right, each its own
        // isolate. `row` holds (x within the row, line, text).
        let mut row: Vec<(f32, PlacedLine, std::sync::Arc<str>)> = Vec::new();
        let mut row_x = 0.0f32;
        for piece in head {
            let para = ts.paragraph(piece.as_str(), width, BODY);
            self.note(&para);
            let text = std::sync::Arc::clone(&para.text);
            let single = para.lines.len() == 1;
            for mut line in para.lines {
                // The piece's own box starts at its first glyph.
                let left = line.runs.iter().map(|r| r.x).fold(f32::INFINITY, f32::min);
                let left = if left.is_finite() { left } else { 0.0 };
                for r in &mut line.runs {
                    r.x -= left;
                }
                let w = line.advance;
                let fits = row.is_empty() || row_x + gap + w <= width;
                if !single || !fits {
                    self.flush_row(&mut ts.fonts, &mut row)?;
                    row_x = 0.0;
                }
                let x = if row.is_empty() { 0.0 } else { row_x + gap };
                row_x = x + w;
                row.push((x, line, std::sync::Arc::clone(&text)));
                if !single {
                    self.flush_row(&mut ts.fonts, &mut row)?;
                    row_x = 0.0;
                }
            }
        }
        self.flush_row(&mut ts.fonts, &mut row)?;
        for p in body {
            let indent = f32::from(p.indent) * INDENT;
            let para = ts.paragraph(p.text.as_str(), width - indent, BODY);
            self.note(&para);
            let text = std::sync::Arc::clone(&para.text);
            for line in para.lines {
                self.place(&mut ts.fonts, MARGIN + indent, line, &text)?;
            }
        }
        self.y += ENTRY_GAP;
        Ok(())
    }

    /// Place a header row: its pieces share one line box.
    fn flush_row(
        &mut self,
        fonts: &mut Library,
        row: &mut Vec<(f32, PlacedLine, std::sync::Arc<str>)>,
    ) -> Result<(), ComposeError> {
        if row.is_empty() {
            return Ok(());
        }
        let height = row.iter().map(|(_, l, _)| l.height).fold(0.0, f32::max);
        let baseline = row.iter().map(|(_, l, _)| l.baseline).fold(0.0, f32::max);
        self.room(fonts, height)?;
        for (x, mut line, text) in row.drain(..) {
            line.baseline = baseline;
            line.height = height;
            self.placed.push(Placed {
                x: MARGIN + x,
                y: self.y,
                line,
                text,
            });
        }
        self.y += height;
        Ok(())
    }

    /// Finish the volume: draw the last page, add the outline and the
    /// title, and serialise.
    pub fn finish(
        mut self,
        fonts: &mut Library,
    ) -> Result<(Vec<u8>, VolumeCounts, Option<Extent>), ComposeError> {
        if !self.placed.is_empty() || self.pages == 0 {
            self.flush_page(fonts)?;
        }
        if let Some(done) = self.conversation.take() {
            self.outline.push_child(done);
        }
        self.doc.set_outline(self.outline);
        self.doc
            .set_metadata(Metadata::new().title(self.title.clone()));
        let mut counts = self.counts;
        counts.pages = u64::try_from(self.pages).unwrap_or(u64::MAX);
        let extent = self.extent;
        let bytes = self.doc.finish().map_err(|_| ComposeError::Serialise)?;
        Ok((bytes, counts, extent))
    }
}
