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
//!
//! **Previews (slice 6j2).** A figure paragraph draws the checked
//! rendition the `Image` frame before its block sent, scaled to fit
//! [`FIGURE_MAX`] and the line, never upscaled; an image waits (at most
//! [`MAX_PENDING_IMAGES`]) until its figure draws it, then is dropped.
//!
//! **Tagged structure (slice 6j2), with no conformance claim (§0.2).**
//! A conversation or section heading is an `H1`, a day heading an `H2`,
//! an entry a `Div` holding a `P` for its header row, a `P` per
//! paragraph and a `Figure` per preview (its alternative text is our own
//! words, "Preview of attachment N"). Every drawn line is one marked
//! span under its group, so the tag tree's reading order is the
//! transcript's order. Whether a screen reader reads it well is
//! **UNVERIFIED** (§21).

use std::collections::BTreeMap;
use std::num::NonZeroU16;

use krilla::destination::XyzDestination;
use krilla::geom::{Point, Size, Transform};
use krilla::image::Image;
use krilla::metadata::Metadata;
use krilla::outline::{Outline, OutlineNode};
use krilla::page::PageSettings;
use krilla::surface::Surface;
use krilla::tagging::{ContentTag, Identifier, Node, SpanTag, Tag, TagGroup, TagKind, TagTree};
use krilla::text::{GlyphId, KrillaGlyph};
use krilla::Document;

use super::layout::{Paragraph, PlacedLine, Typesetter};
use super::library::Library;
use crate::pdf::doc::{HeadingLevel, PageSize, Para, ParaRole, PdfText};

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
/// The largest a preview is drawn, in points (either side).
pub const FIGURE_MAX: f32 = 180.0;
/// The space around a preview.
const FIGURE_GAP: f32 = 3.0;
/// The most images waiting for their block.
pub const MAX_PENDING_IMAGES: usize = 64;
// The main process sends a block's images just before it, at most this
// many a block.
const _: () = assert!(crate::writer::pdf::MAX_FIGURES_PER_BLOCK <= MAX_PENDING_IMAGES);

/// What a placed piece draws.
#[derive(Clone)]
enum Draw {
    Line {
        line: PlacedLine,
        text: std::sync::Arc<str>,
    },
    Image {
        image: Image,
        width: f32,
        height: f32,
    },
}

/// A piece placed on the current page (no `Debug`: it holds its text).
#[derive(Clone)]
struct Placed {
    /// The left edge of the piece's box.
    x: f32,
    /// The top of the piece.
    y: f32,
    draw: Draw,
    /// The tag group it belongs to.
    tag: usize,
}

/// A tag group of the volume, children in reading order.
struct TagNode {
    kind: GroupKind,
    children: Vec<TagChild>,
}

enum TagChild {
    Group(usize),
    Leaf(Identifier),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GroupKind {
    H1,
    H2,
    Div,
    P,
    Figure(u64),
}

impl GroupKind {
    fn tag(&self) -> TagKind {
        match self {
            GroupKind::H1 => Tag::Hn(NonZeroU16::MIN, None).into(),
            GroupKind::H2 => Tag::Hn(NonZeroU16::MIN.saturating_add(1), None).into(),
            GroupKind::Div => Tag::Div.into(),
            GroupKind::P => Tag::P.into(),
            GroupKind::Figure(n) => Tag::Figure(Some(format!("Preview of attachment {n}"))).into(),
        }
    }
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
    /// Previews drawn.
    pub figures: u64,
}

/// One volume being composed.
pub struct Composer {
    page: (f32, f32),
    title: String,
    /// The title came from a conversation heading (the first wins).
    titled: bool,
    doc: Document,
    placed: Vec<Placed>,
    /// Images sent for the next block, by attachment record number.
    images: BTreeMap<u64, Image>,
    /// The volume's tag groups, and the top-level ones in order.
    tags: Vec<TagNode>,
    roots: Vec<usize>,
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
    /// A figure names an image that was not sent, or too many images
    /// wait for their block.
    Image,
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
            titled: false,
            doc: Document::new(),
            placed: Vec::new(),
            images: BTreeMap::new(),
            tags: Vec::new(),
            roots: Vec::new(),
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

    /// A new tag group under `parent` (top level with `None`).
    fn group(&mut self, kind: GroupKind, parent: Option<usize>) -> usize {
        let at = self.tags.len();
        self.tags.push(TagNode {
            kind,
            children: Vec::new(),
        });
        match parent.and_then(|p| self.tags.get_mut(p)) {
            Some(p) => p.children.push(TagChild::Group(at)),
            None => self.roots.push(at),
        }
        at
    }

    /// The volume's document title, from the first conversation heading
    /// that carries one.
    pub fn set_title(&mut self, title: &str) {
        if !self.titled {
            self.title = title.to_owned();
            self.titled = true;
        }
    }

    /// A checked rendition for the next block's figure `n`.
    pub fn add_image(&mut self, n: u64, image: Image) -> Result<(), ComposeError> {
        if self.images.len() >= MAX_PENDING_IMAGES && !self.images.contains_key(&n) {
            return Err(ComposeError::Image);
        }
        self.images.insert(n, image);
        Ok(())
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
        let tags = &mut self.tags;
        for p in &placed {
            let (line, text) = match &p.draw {
                Draw::Line { line, text } => (line, text),
                Draw::Image {
                    image,
                    width,
                    height,
                } => {
                    let Some(size) = Size::from_wh(*width, *height) else {
                        continue;
                    };
                    let id = surface.start_tagged(ContentTag::Other);
                    surface.push_transform(&Transform::from_translate(p.x, p.y));
                    surface.draw_image(image.clone(), size);
                    surface.pop();
                    surface.end_tagged();
                    leaf(tags, p.tag, id);
                    continue;
                }
            };
            if line.runs.is_empty() {
                continue;
            }
            let id = surface.start_tagged(ContentTag::Span(SpanTag::empty()));
            draw_line(&mut surface, fonts, p, line, text, &mut extent);
            surface.end_tagged();
            leaf(tags, p.tag, id);
        }
        surface.finish();
        page.finish();
        self.extent = extent;
        self.pages += 1;
        self.y = MARGIN;
        Ok(())
    }
}

fn leaf(tags: &mut [TagNode], group: usize, id: Identifier) {
    if let Some(g) = tags.get_mut(group) {
        g.children.push(TagChild::Leaf(id));
    }
}

/// Draw one line's glyph runs.
fn draw_line(
    surface: &mut Surface<'_>,
    fonts: &mut Library,
    p: &Placed,
    line: &PlacedLine,
    text: &str,
    extent: &mut Option<Extent>,
) {
    let baseline = p.y + line.baseline;
    for r in &line.runs {
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
        e.max_y = e.max_y.max(p.y + line.height);
        e.glyphs += u64::try_from(glyphs.len()).unwrap_or(0);
        surface.draw_glyphs(
            Point::from_xy(x0, baseline),
            &glyphs,
            font,
            text,
            r.size,
            false,
        );
    }
}

impl Composer {
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
        tag: usize,
    ) -> Result<(), ComposeError> {
        self.room(fonts, line.height)?;
        let height = line.height;
        self.placed.push(Placed {
            x,
            y: self.y,
            draw: Draw::Line {
                line,
                text: std::sync::Arc::clone(text),
            },
            tag,
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
        let kind = match level {
            HeadingLevel::Day => GroupKind::H2,
            HeadingLevel::Conversation | HeadingLevel::Section => GroupKind::H1,
        };
        let tag = self.group(kind, None);
        let text = std::sync::Arc::clone(&para.text);
        for line in para.lines {
            self.place(&mut ts.fonts, MARGIN, line, &text, tag)?;
        }
        self.y += ENTRY_GAP;
        Ok(())
    }

    /// A transcript entry. A figure paragraph draws (and drops) the image
    /// sent for it ([`Self::add_image`]); an image waits for its figure,
    /// which may be in a later block of the same entry.
    pub fn entry(
        &mut self,
        ts: &mut Typesetter,
        head: &[PdfText],
        body: &[Para],
    ) -> Result<(), ComposeError> {
        let width = self.content_width();
        let gap = BODY * PIECE_GAP;
        let div = self.group(GroupKind::Div, None);
        let head_tag = self.group(GroupKind::P, Some(div));
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
                    self.flush_row(&mut ts.fonts, &mut row, head_tag)?;
                    row_x = 0.0;
                }
                let x = if row.is_empty() { 0.0 } else { row_x + gap };
                row_x = x + w;
                row.push((x, line, std::sync::Arc::clone(&text)));
                if !single {
                    self.flush_row(&mut ts.fonts, &mut row, head_tag)?;
                    row_x = 0.0;
                }
            }
        }
        self.flush_row(&mut ts.fonts, &mut row, head_tag)?;
        for p in body {
            let indent = f32::from(p.indent) * INDENT;
            if p.role == ParaRole::Figure {
                let image = p
                    .image
                    .and_then(|n| self.images.remove(&n).map(|i| (n, i)))
                    .ok_or(ComposeError::Image)?;
                self.figure(&mut ts.fonts, div, indent, image)?;
                continue;
            }
            let tag = self.group(GroupKind::P, Some(div));
            let para = ts.paragraph(p.text.as_str(), width - indent, BODY);
            self.note(&para);
            let text = std::sync::Arc::clone(&para.text);
            for line in para.lines {
                self.place(&mut ts.fonts, MARGIN + indent, line, &text, tag)?;
            }
        }
        self.y += ENTRY_GAP;
        Ok(())
    }

    /// A preview, scaled to fit [`FIGURE_MAX`] and the line (never
    /// upscaled), kept whole on one page.
    fn figure(
        &mut self,
        fonts: &mut Library,
        div: usize,
        indent: f32,
        (n, image): (u64, Image),
    ) -> Result<(), ComposeError> {
        let (pw, ph) = image.size();
        #[allow(clippy::cast_precision_loss)] // pixel sizes are at most 2048
        let (pw, ph) = (pw.max(1) as f32, ph.max(1) as f32);
        let room_w = (self.content_width() - indent).max(1.0);
        let scale = (FIGURE_MAX / pw)
            .min(FIGURE_MAX / ph)
            .min(room_w / pw)
            .min(1.0);
        let (w, h) = (pw * scale, ph * scale);
        let tag = self.group(GroupKind::Figure(n), Some(div));
        self.room(fonts, h + 2.0 * FIGURE_GAP)?;
        self.y += FIGURE_GAP;
        self.placed.push(Placed {
            x: MARGIN + indent,
            y: self.y,
            draw: Draw::Image {
                image,
                width: w,
                height: h,
            },
            tag,
        });
        self.y += h + FIGURE_GAP;
        self.counts.figures += 1;
        Ok(())
    }

    /// Place a header row: its pieces share one line box.
    fn flush_row(
        &mut self,
        fonts: &mut Library,
        row: &mut Vec<(f32, PlacedLine, std::sync::Arc<str>)>,
        tag: usize,
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
                draw: Draw::Line { line, text },
                tag,
            });
        }
        self.y += height;
        Ok(())
    }

    /// The tag tree, groups in reading order (a group with no drawn
    /// content is left out).
    fn tag_tree(&mut self) -> TagTree {
        fn build(tags: &mut Vec<Option<TagNode>>, at: usize) -> Option<TagGroup> {
            let node = tags.get_mut(at)?.take()?;
            let mut children: Vec<Node> = Vec::new();
            for c in node.children {
                match c {
                    TagChild::Leaf(id) => children.push(id.into()),
                    TagChild::Group(g) => {
                        if let Some(group) = build(tags, g) {
                            children.push(group.into());
                        }
                    }
                }
            }
            (!children.is_empty()).then(|| TagGroup::with_children(node.kind.tag(), children))
        }
        let mut tags: Vec<Option<TagNode>> = std::mem::take(&mut self.tags)
            .into_iter()
            .map(Some)
            .collect();
        let mut tree = TagTree::new();
        for r in std::mem::take(&mut self.roots) {
            if let Some(g) = build(&mut tags, r) {
                tree.push(g);
            }
        }
        tree
    }

    /// Finish the volume: draw the last page, add the outline, the tag
    /// tree and the title, and serialise.
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
        let tree = self.tag_tree();
        self.doc.set_outline(self.outline);
        self.doc.set_tag_tree(tree);
        self.doc
            .set_metadata(Metadata::new().title(self.title.clone()));
        let mut counts = self.counts;
        counts.pages = u64::try_from(self.pages).unwrap_or(u64::MAX);
        let extent = self.extent;
        let bytes = self.doc.finish().map_err(|_| ComposeError::Serialise)?;
        Ok((bytes, counts, extent))
    }
}
