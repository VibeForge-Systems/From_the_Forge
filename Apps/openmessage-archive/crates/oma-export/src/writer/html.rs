//! The HTML writer (M6 spec §5.5, D132).
//!
//! **Files.** A bundle gets `html/c0001.html` per conversation file
//! (`html/no-conversation.html` for the messages in none), split into
//! `c0001-2.html` … beyond [`MAX_PAGE_ITEMS`] items or [`MAX_PAGE_BYTES`]
//! bytes **including** the embedded `data:` bytes as written; the larger
//! previews `html/previews/p<n>.jpg|png` when "Larger previews" is on; and
//! `html/index.html`, which lists the conversations by ordinal with their
//! names in the body only. A single file is one page, never split.
//! Window titles are generic ("Messages export — conversation 1", D112).
//!
//! **One fixed template.** Every page is the same skeleton: `<meta
//! charset>`, then the Content-Security-Policy meta **first** (CSP3 §3.3:
//! a meta policy does not cover what comes before it), `default-src
//! 'none'; img-src data:; style-src 'sha256-…'; base-uri 'none';
//! form-action 'none'`, then the referrer and colour-scheme metas, the
//! generic title and the one stylesheet [`STYLE`], whose hash is pinned in
//! [`STYLE_SHA256`] (asserted by a test). The body: two skip links, a
//! header with the plaintext notice and the one `h1`, `main` with one
//! `section` and `h2` per day and one `article` per item (labelled by its
//! `h3`: sender and time), then the days' `nav` **after** `main`, then the
//! `footer id="about"`.
//!
//! **Escaping is the control; the CSP is defence in depth.**
//! - Content goes only into text nodes, with `& < > " '` escaped as
//!   `&amp; &lt; &gt; &quot; &#x27;` (OWASP XSS Prevention Cheat Sheet,
//!   rule 1), each run inside `<bdi dir="auto">` (`white-space:
//!   pre-wrap` keeps its line breaks; CR and CRLF become LF).
//! - Markers, placeholders and segment labels are **elements**
//!   (`<span class="mark">U+202E</span>`, `<span class="ph">one-time code
//!   hidden</span>`, `<span class="label">…</span>`), never text, so a
//!   literal `⟨` in content stays literal and can forge nothing. Controls
//!   and noncharacters (both HTML parse errors when raw) are always
//!   marked; hidden characters are marked in recorded text
//!   when the option is on (the default) and always in header fields
//!   (senders, names, file names, tags). Combining marks are all kept;
//!   each `article` has `overflow: hidden; contain: paint`.
//! - Attribute values are only ours: ids from ordinals (`m45`, `m45-h`,
//!   `d2024-03-10`), ISO times, image sizes, `data:` URIs of checked
//!   renditions, class tokens, and relative links of a closed grammar
//!   (`#m<n>`, `#d<date>`, `#main`, `#days`, `#about`, `index.html`,
//!   `c<nnnn>[-k].html`, `no-conversation[-k].html`,
//!   `previews/p<n>.jpg|png`).
//! - Never present: `<script>`, event or `style` attributes, `<base>`,
//!   forms, frames, `<object>`, `<embed>`, `<link>`, a refresh meta,
//!   fonts, `<audio>` or `<video>`. URLs in messages are text, not links.
//!
//! **Images (D113, D132).** Thumbnails come from a
//! [`PreviewSource`](crate::preview::PreviewSource) (the decode helper's
//! `thumb` rendition), are checked ([`crate::preview::check_rendition`])
//! and embedded as `data:` while the page's [`MAX_PAGE_DATA_BYTES`] budget
//! of base64 lasts; beyond it the card says "Preview not embedded (size
//! limit)", counted with the unavailable previews. A display rendition is
//! linked as "Larger preview" (bundles, larger previews on). **Originals
//! are never linked or embedded**: a copied file is named by its path as
//! text.
//!
//! **Reply links point backward only**: to `#m<n>` when the originator was
//! already written on the same page; otherwise "item n, elsewhere in this
//! conversation", "in another conversation of this export" or "not in
//! this export". A reaction's target follows the same rule.
//!
//! **Per-part bodies.** The record's segments carry no part index, so a
//! body is rendered whole, with its edits and unsends as tags (as in TXT;
//! a deviation from §5.5, recorded in `architecture.md`).
//!
//! Every sentence comes from `words.rs`. Nothing here logs.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use crate::codes::{
    AttachmentAccessCode, CardStateCode, ChipSenderCode, Direction, SegmentKindCode, Style,
};
use crate::error::{ExportError, LimitKind};
use crate::hidden::{self, CharClass, MarkCounts};
use crate::ids::Ordinal;
use crate::map::ExportZone;
use crate::naming::{BundlePath, FileStem};
use crate::plan::{Clock, Format, Packaging};
use crate::preview::{
    self, check_rendition, CheckedRendition, Preview, PreviewSource, RenditionSize,
};
use crate::record::*;
use crate::stage::{StagedFile, Staging};
use crate::time::{CivilDate, HumanStyle};
use crate::words::html_text as t;

use super::shared::{
    date_text, flatten, is_undated, item_tags, sender_tags, time_text, SenderTags,
};
use super::{
    is, phrase, AboutDraft, ConversationFile, ConversationTotals, ExportContext, ExportWriter,
    Words, WriterReport,
};

/// Items on one page before it is split.
pub const MAX_PAGE_ITEMS: u64 = 5_000;
/// Bytes of one page, embedded `data:` bytes included, before it is split.
pub const MAX_PAGE_BYTES: u64 = 20 << 20;
/// Base64 bytes of embedded thumbnails one page may carry.
pub const MAX_PAGE_DATA_BYTES: u64 = 16 << 20;
/// Room kept on a page for its navigation and footer.
pub const PAGE_RESERVE: u64 = 256 << 10;

/// When a bundle's page is split and how much `data:` it may embed. The
/// export always uses [`PageLimits::SPEC`]; tests lower them to reach the
/// splits and the budget with the small catalogues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageLimits {
    pub items: u64,
    pub bytes: u64,
    pub data_bytes: u64,
    /// Room kept for the navigation and the footer.
    pub reserve: u64,
}

impl PageLimits {
    /// Spec §5.5: 5,000 items or 20 MiB per page, 16 MiB of base64.
    pub const SPEC: PageLimits = PageLimits {
        items: MAX_PAGE_ITEMS,
        bytes: MAX_PAGE_BYTES,
        data_bytes: MAX_PAGE_DATA_BYTES,
        reserve: PAGE_RESERVE,
    };
}

/// The one stylesheet. Its SHA-256 is [`STYLE_SHA256`], the only
/// `style-src` the CSP allows; contrast is at least 4.5:1 in both colour
/// schemes (a test computes it from the custom properties).
pub const STYLE: &str = ":root{color-scheme:light dark;--fg:#1b1b1b;--bg:#ffffff;--muted:#545454;--line:#c4c4c4;--link:#0b57d0;--mark-fg:#6b2d00;--mark-bg:#fff0d6;--ph-fg:#1d3d6b;--ph-bg:#e5eefb}\
@media (prefers-color-scheme:dark){:root{--fg:#e6e6e6;--bg:#141414;--muted:#b4b4b4;--line:#4a4a4a;--link:#8ab4f8;--mark-fg:#ffd9a8;--mark-bg:#3d2a12;--ph-fg:#c7dcff;--ph-bg:#1f2d44}}\
html{background:var(--bg)}\
body{margin:0 auto;max-width:48rem;padding:1rem;font:1rem/1.5 system-ui,sans-serif;color:var(--fg);background:var(--bg)}\
a{color:var(--link)}\
.skip{position:absolute;left:-10000px;top:0}\
.skip:focus{position:static}\
h1{font-size:1.4rem}\
h2{font-size:1.1rem;margin:1.5rem 0 .5rem}\
h3{font-size:1rem;font-weight:normal;margin:0}\
.who{font-weight:bold}\
.notice,.q,.note,.sub,.t,time{color:var(--muted)}\
bdi{white-space:pre-wrap;overflow-wrap:anywhere}\
p{margin:.25rem 0}\
article{overflow:hidden;contain:paint;border-top:1px solid var(--line);padding:.5rem 0}\
article.me{border-left:4px solid var(--line);padding-left:.75rem}\
.label{font-weight:bold}\
.mark{font-family:ui-monospace,monospace;font-size:.85em;color:var(--mark-fg);background:var(--mark-bg);border:1px solid var(--line);border-radius:3px;padding:0 .15em}\
.ph{font-style:italic;color:var(--ph-fg);background:var(--ph-bg);border-radius:3px;padding:0 .15em}\
.b{font-weight:bold}.i{font-style:italic}.u{text-decoration:underline}.s{text-decoration:line-through}.u.s{text-decoration:underline line-through}\
.att{margin:.5rem 0;padding-left:.75rem;border-left:2px solid var(--line)}\
figure{margin:.5rem 0}\
img{max-width:100%;height:auto}\
.chips,.versions,.about,.legend,.convs{padding-left:1.25rem}\
nav ul{list-style:none;padding:0;display:flex;flex-wrap:wrap;gap:.25rem .75rem}\
footer{margin-top:2rem;border-top:1px solid var(--line)}\
@media print{.skip,nav{display:none}article{break-inside:avoid}a{color:inherit}details{display:block}}";

/// `base64(sha256(STYLE))`, the CSP's one `style-src` hash.
pub const STYLE_SHA256: &str = "Z91ZbynAupJu8/VVGqRordFganqeV8lY8seML4WnNX8=";

/// The Content-Security-Policy every page carries, first after the
/// charset.
pub fn csp() -> String {
    format!(
        "default-src 'none'; img-src data:; style-src 'sha256-{STYLE_SHA256}'; base-uri 'none'; form-action 'none'"
    )
}

fn io(e: std::io::Error) -> ExportError {
    ExportError::io(&e)
}

fn order() -> ExportError {
    ExportError::Limit(LimitKind::RowOrder)
}

/// `s` escaped for a text node or an attribute value (OWASP rule 1).
fn escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
}

/// A noncharacter (U+FDD0–U+FDEF, and the last two code points of every
/// plane): an HTML parse error when raw, so it is always marked.
fn is_noncharacter(c: char) -> bool {
    let cp = u32::from(c);
    (0xFDD0..=0xFDEF).contains(&cp) || (cp & 0xFFFE) == 0xFFFE
}

/// An HTML fragment being built, with what marking it did.
#[derive(Default)]
struct H {
    s: String,
    counts: MarkCounts,
}

impl H {
    /// Markup of ours, written as it is.
    fn raw(&mut self, s: &str) {
        self.s.push_str(s);
    }

    /// Words of ours (or a number), escaped.
    fn text(&mut self, s: &str) {
        escape_into(&mut self.s, s);
    }

    /// An element of ours holding words of ours.
    fn el(&mut self, open: &str, words: &str, close: &str) {
        self.raw(open);
        self.text(words);
        self.raw(close);
    }

    /// Recorded content inside its own `<bdi dir="auto">` (with `class`
    /// tokens of ours): escaped, CR and CRLF as LF, controls marked as
    /// elements, hidden characters marked when `mark_hidden`.
    fn content(&mut self, s: &str, mark_hidden: bool, class: &str) {
        if class.is_empty() {
            self.raw("<bdi dir=\"auto\">");
        } else {
            self.raw("<bdi dir=\"auto\" class=\"");
            self.text(class);
            self.raw("\">");
        }
        self.content_inner(s, mark_hidden);
        self.raw("</bdi>");
    }

    fn content_inner(&mut self, s: &str, mark_hidden: bool) {
        let bytes = s.as_bytes();
        for p in hidden::classify(s) {
            let piece = s.get(p.start..p.end).unwrap_or("");
            if p.class == CharClass::Cr {
                // CRLF counts once; a lone CR is a line break too.
                if bytes.get(p.end) != Some(&b'\n') {
                    self.s.push('\n');
                }
                continue;
            }
            let marks = p.class.is_control() || (mark_hidden && p.class.is_hidden());
            if marks {
                for c in piece.chars() {
                    self.raw("<span class=\"mark\">");
                    self.text(&hidden::code_point(u32::from(c)));
                    self.raw("</span>");
                    if p.class.is_hidden() {
                        self.counts.hidden_marked += 1;
                    } else {
                        self.counts.controls_escaped += 1;
                    }
                }
            } else {
                for c in piece.chars() {
                    if is_noncharacter(c) {
                        // A parse error in HTML (noncharacter in the input
                        // stream): marked like a control.
                        self.raw("<span class=\"mark\">");
                        self.text(&hidden::code_point(u32::from(c)));
                        self.raw("</span>");
                        self.counts.controls_escaped += 1;
                    } else {
                        let mut b = [0u8; 4];
                        escape_into(&mut self.s, c.encode_utf8(&mut b));
                    }
                }
            }
        }
    }

    /// A placeholder of ours as an element.
    fn ph(&mut self, words: &str) {
        self.el("<span class=\"ph\">", words, "</span>");
    }

    /// An attribute value of ours, escaped anyway.
    fn attr(&mut self, name: &str, value: &str) {
        self.raw(" ");
        self.raw(name);
        self.raw("=\"");
        self.text(value);
        self.raw("\"");
    }
}

/// A piece of recorded text: content, or a placeholder of ours.
enum Run<'a> {
    Text { text: &'a str, class: String },
    Placeholder(String),
}

/// Which section of `main` an item belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Section {
    Day(CivilDate),
    Undated,
    NoTime,
}

/// A preview decided for one attachment of the pending item.
enum Pic {
    /// Not asked (previews off, not included) or not an image.
    None,
    Unavailable,
    Ready {
        thumb: CheckedRendition,
        display: Option<CheckedRendition>,
    },
}

/// The item waiting for its versions.
struct Pending {
    item: ItemRecord,
    versions: Vec<VersionRecord>,
    pics: Vec<Pic>,
}

/// One page being written.
struct Page {
    file: StagedFile,
    part: u32,
    bytes: u64,
    data_bytes: u64,
    items: u64,
    /// Item ordinals already written on this page (reply links).
    written: BTreeSet<Ordinal>,
    /// The page's sections, for the days' navigation: id and heading.
    days: Vec<(String, String)>,
    ids: BTreeSet<String>,
    section: Option<Section>,
}

/// What rendering an item would write and count.
struct Rendered {
    html: String,
    counts: MarkCounts,
    data_bytes: u64,
    embedded: u64,
    unavailable: u64,
    size_limited: u64,
    reply_links: u64,
    larger: Vec<(Ordinal, CheckedRendition)>,
    section: Option<(Section, String, String)>,
}

/// One conversation file being written.
struct Conv {
    stem: FileStem,
    /// Conversation ordinal, or `None` for the messages in none.
    n: Option<Ordinal>,
    /// The page header (after the notice): `h1` and the header lines.
    header: String,
    header_counts: MarkCounts,
    page: Option<Page>,
    parts: u32,
    pending: Option<Pending>,
    items: u64,
    folded: u64,
}

/// What `index.html` lists for one conversation file.
struct IndexEntry {
    stem: FileStem,
    n: Option<Ordinal>,
    name: Option<String>,
    items: u64,
    parts: u32,
}

/// The HTML writer.
pub struct HtmlWriter {
    markers: bool,
    style: HumanStyle,
    single: bool,
    previews: bool,
    larger: bool,
    zone_name: String,
    words: Words,
    source: Option<Box<dyn PreviewSource>>,
    /// Handle ordinal → its recorded address (raw; escaped at each use).
    handles: BTreeMap<Ordinal, Option<String>>,
    conv: Option<Conv>,
    /// The single file, kept open from the first conversation to `finish`.
    single_page: Option<Page>,
    index: Vec<IndexEntry>,
    larger_written: BTreeSet<Ordinal>,
    counts: MarkCounts,
    embedded: u64,
    unavailable: u64,
    size_limited: u64,
    reply_links: u64,
    pages: u64,
    report: WriterReport,
    limits: PageLimits,
    /// The last folded reaction row handed over (its versions are not
    /// shown).
    folded_last: Option<Ordinal>,
}

impl std::fmt::Debug for HtmlWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HtmlWriter")
            .field("single", &self.single)
            .field("previews", &self.previews)
            .field("handles", &self.handles.len())
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

impl Default for HtmlWriter {
    fn default() -> HtmlWriter {
        HtmlWriter::new()
    }
}

impl HtmlWriter {
    /// A writer without a preview source: a plan with previews on is
    /// refused at `begin` (`OptionDoesNotApply`) rather than written with
    /// every image "unavailable".
    pub fn new() -> HtmlWriter {
        HtmlWriter {
            markers: true,
            style: HumanStyle {
                h23: true,
                seconds: true,
            },
            single: false,
            previews: false,
            larger: false,
            zone_name: "UTC".to_owned(),
            words: Words::new(),
            source: None,
            handles: BTreeMap::new(),
            conv: None,
            single_page: None,
            index: Vec::new(),
            larger_written: BTreeSet::new(),
            counts: MarkCounts::default(),
            embedded: 0,
            unavailable: 0,
            size_limited: 0,
            reply_links: 0,
            pages: 0,
            report: WriterReport::new(Format::Html),
            limits: PageLimits::SPEC,
            folded_last: None,
        }
    }

    /// The same writer with other page limits (tests only reach the
    /// splits this way; an export uses [`PageLimits::SPEC`]).
    pub fn with_limits(self, limits: PageLimits) -> HtmlWriter {
        HtmlWriter { limits, ..self }
    }

    /// A writer whose previews come from `source` (spec §6.5).
    pub fn with_previews(source: Box<dyn PreviewSource>) -> HtmlWriter {
        HtmlWriter {
            source: Some(source),
            ..HtmlWriter::new()
        }
    }

    fn address(&self, h: Ordinal) -> Option<&str> {
        self.handles.get(&h).and_then(|a| a.as_deref())
    }

    /// A handle's address as content, or our words when none is recorded.
    fn handle_into(&self, out: &mut H, h: Ordinal) {
        match self.address(h) {
            Some(a) if !a.is_empty() => out.content(a, true, ""),
            _ => out.text(&format!("address {h} (not recorded)")),
        }
    }

    /// A recorded value (a header field: hidden characters always marked).
    fn value_into(out: &mut H, v: &TextValue) -> bool {
        match &v.text {
            Some(c) => {
                out.content(c.as_str(), true, "");
                true
            }
            None => false,
        }
    }

    fn missing_words(&self, v: &TextValue) -> String {
        v.missing
            .map(|m| self.words.say("valueState", m))
            .unwrap_or_default()
    }

    // -----------------------------------------------------------------------
    // Pages
    // -----------------------------------------------------------------------

    fn head(title: &str, out: &mut H) {
        out.raw("<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta http-equiv=\"Content-Security-Policy\"");
        out.attr("content", &csp());
        out.raw(">\n<meta name=\"referrer\" content=\"no-referrer\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<meta name=\"color-scheme\" content=\"light dark\">\n<title>");
        out.text(title);
        out.raw("</title>\n<style>");
        out.raw(STYLE);
        out.raw("</style>\n</head>\n<body>\n");
    }

    fn page_title(c: &Conv, part: u32) -> String {
        let what = match c.n {
            Some(n) => format!("conversation {n}"),
            None => "messages in no conversation".to_owned(),
        };
        if part > 1 {
            format!("{} \u{2014} {what}, part {part}", t::TITLE)
        } else {
            format!("{} \u{2014} {what}", t::TITLE)
        }
    }

    fn open_page(&mut self, out: &mut Staging) -> Result<(), ExportError> {
        let c = self.conv.as_mut().ok_or_else(order)?;
        c.parts += 1;
        let part = c.parts;
        let mut h = H::default();
        HtmlWriter::head(&HtmlWriter::page_title(c, part), &mut h);
        h.raw("<a class=\"skip\" href=\"#main\">");
        h.text(t::SKIP_MAIN);
        h.raw("</a> <a class=\"skip\" href=\"#days\">");
        h.text(t::SKIP_DAYS);
        h.raw("</a>\n<header>\n<p class=\"notice\">");
        h.text(t::NOTICE);
        h.raw(" <a href=\"#about\">");
        h.text(t::ABOUT);
        h.raw("</a></p>\n");
        h.raw(&c.header);
        if part > 1 {
            h.raw("<p class=\"note\">");
            h.text(&format!(
                "Part {part}. The earlier parts are linked at the end of this page."
            ));
            h.raw("</p>\n");
        }
        h.raw("</header>\n<main id=\"main\">\n");
        let path = BundlePath::html_page(c.stem, part);
        let mut file = out.create(&path)?;
        file.write_all(h.s.as_bytes()).map_err(io)?;
        let page = Page {
            bytes: file.bytes_written(),
            file,
            part,
            data_bytes: 0,
            items: 0,
            written: BTreeSet::new(),
            days: Vec::new(),
            ids: BTreeSet::new(),
            section: None,
        };
        self.counts.add(c.header_counts);
        self.pages += 1;
        if self.single {
            self.single_page = Some(page);
        } else {
            c.page = Some(page);
        }
        Ok(())
    }

    fn page_mut(&mut self) -> Result<&mut Page, ExportError> {
        if self.single {
            self.single_page.as_mut().ok_or_else(order)
        } else {
            self.conv
                .as_mut()
                .and_then(|c| c.page.as_mut())
                .ok_or_else(order)
        }
    }

    fn page_ref(&self) -> Result<&Page, ExportError> {
        if self.single {
            self.single_page.as_ref().ok_or_else(order)
        } else {
            self.conv
                .as_ref()
                .and_then(|c| c.page.as_ref())
                .ok_or_else(order)
        }
    }

    /// The end of a page: the open section, `main`, the days' navigation
    /// and (in a bundle) the links to the other parts, then the footer.
    fn page_tail(
        &self,
        page: &Page,
        next: bool,
        conv_totals: Option<(u64, u64)>,
        about: Option<&[String]>,
    ) -> String {
        let mut h = H::default();
        if page.section.is_some() {
            h.raw("</section>\n");
        }
        if page.items == 0 && page.part <= 1 {
            h.el("<p class=\"note\">", t::EMPTY, "</p>\n");
        }
        h.raw("</main>\n<nav id=\"days\"");
        h.attr("aria-label", t::DAYS);
        h.raw(">\n<ul>");
        for (id, label) in &page.days {
            h.raw("<li><a");
            h.attr("href", &format!("#{id}"));
            h.raw(">");
            h.text(label);
            h.raw("</a></li>");
        }
        h.raw("</ul>\n");
        if !self.single {
            let stem = self.conv.as_ref().map(|c| c.stem);
            if let Some(stem) = stem {
                if page.part > 1 || next {
                    h.el("<p class=\"parts\">", t::PARTS, "");
                    for k in 1..page.part {
                        h.raw(" <a");
                        h.attr("href", &href(&BundlePath::html_page(stem, k)));
                        h.raw(">");
                        h.text(&k.to_string());
                        h.raw("</a>");
                    }
                    h.raw(" ");
                    h.text(&page.part.to_string());
                    if next {
                        h.raw(" <a");
                        h.attr("href", &href(&BundlePath::html_page(stem, page.part + 1)));
                        h.raw(">");
                        h.text(t::NEXT);
                        h.raw("</a>");
                    }
                    h.raw("</p>\n");
                }
            }
            h.raw("<p><a");
            h.attr("href", &href(&BundlePath::html_index()));
            h.raw(">");
            h.text(t::INDEX_LINK);
            h.raw("</a></p>\n");
        }
        h.raw("</nav>\n<footer id=\"about\">\n");
        h.el("<p class=\"label\">", t::ABOUT, "</p>\n");
        h.el(
            "<p>",
            &format!("Items on this page: {}", page.items),
            "</p>\n",
        );
        if let Some((items, folded)) = conv_totals {
            h.el(
                "<p>",
                &format!(
                    "Items in this conversation file: {items}. Reactions shown on the messages they react to: {folded}."
                ),
                "</p>\n",
            );
        }
        h.raw("<ul class=\"legend\">\n");
        for l in t::LEGEND {
            h.el("<li>", l, "</li>\n");
        }
        h.raw("</ul>\n");
        match about {
            Some(lines) => {
                h.raw("<ul class=\"about\">\n");
                for l in lines {
                    h.raw("<li>");
                    h.content(l, true, "");
                    h.raw("</li>\n");
                }
                h.raw("</ul>\n");
            }
            None => h.el("<p>", t::ABOUT_BUNDLE, "</p>\n"),
        }
        h.raw("</footer>\n</body>\n</html>\n");
        h.s
    }

    fn close_page(
        &mut self,
        next: bool,
        conv_totals: Option<(u64, u64)>,
        about: Option<&[String]>,
        out: &mut Staging,
    ) -> Result<(), ExportError> {
        let page = if self.single {
            self.single_page.take()
        } else {
            self.conv.as_mut().and_then(|c| c.page.take())
        }
        .ok_or_else(order)?;
        let tail = self.page_tail(&page, next, conv_totals, about);
        let mut page = page;
        page.file.write_all(tail.as_bytes()).map_err(io)?;
        out.close(page.file)?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Items
    // -----------------------------------------------------------------------

    /// Ask the source for the pending item's previews and check them.
    fn pics(&mut self, it: &ItemRecord) -> Result<Vec<Pic>, ExportError> {
        let mut out = Vec::with_capacity(it.attachments.len());
        for a in &it.attachments {
            // Included, or revealed by the earlier-content opt-in; never a
            // withheld file.
            let asked = self.previews
                && (is(a.access, AttachmentAccessCode::Included)
                    || is(a.access, AttachmentAccessCode::Revealed));
            let p = match (asked, self.source.as_mut()) {
                (true, Some(src)) => src.renditions(a, self.larger)?,
                _ => Preview::None,
            };
            out.push(match p {
                Preview::None => Pic::None,
                Preview::Unavailable => Pic::Unavailable,
                Preview::Ready { thumb, display } => Pic::Ready {
                    thumb: check_rendition(thumb, RenditionSize::Thumb)?,
                    display: match display {
                        Some(d) if self.larger => Some(check_rendition(d, RenditionSize::Display)?),
                        _ => None,
                    },
                },
            });
        }
        Ok(out)
    }

    /// Write the pending item onto the current page, opening the next
    /// part first when it would overflow this one (bundles only).
    fn flush(&mut self, out: &mut Staging) -> Result<(), ExportError> {
        let Some(p) = self.conv.as_mut().and_then(|c| c.pending.take()) else {
            return Ok(());
        };
        if self.page_ref().is_err() {
            self.open_page(out)?;
        }
        if !self.single && self.page_ref()?.items >= self.limits.items {
            self.split(out)?;
        }
        let mut r = self.render(self.page_ref()?, &p)?;
        if !self.single {
            let page = self.page_ref()?;
            let nav = (page.days.len() as u64 + 1) * 96;
            let fits =
                page.bytes + r.html.len() as u64 + nav + self.limits.reserve <= self.limits.bytes;
            if page.items > 0 && !fits {
                self.split(out)?;
                r = self.render(self.page_ref()?, &p)?;
            }
        }
        self.apply(r, &p, out)
    }

    /// Close this page with a link to the next part and open that part.
    fn split(&mut self, out: &mut Staging) -> Result<(), ExportError> {
        self.close_page(true, None, None, out)?;
        self.open_page(out)
    }

    fn apply(&mut self, r: Rendered, p: &Pending, out: &mut Staging) -> Result<(), ExportError> {
        for (n, rendition) in r.larger {
            if self.larger_written.insert(n) {
                let mut f = out.create(&BundlePath::html_preview(n, rendition.ext()))?;
                f.write_all(rendition.bytes()).map_err(io)?;
                out.close(f)?;
            }
        }
        self.counts.add(r.counts);
        self.embedded += r.embedded;
        self.unavailable += r.unavailable;
        self.size_limited += r.size_limited;
        self.reply_links += r.reply_links;
        let page = self.page_mut()?;
        page.file.write_all(r.html.as_bytes()).map_err(io)?;
        page.bytes = page.file.bytes_written();
        page.data_bytes += r.data_bytes;
        page.items += 1;
        page.written.insert(p.item.n);
        if let Some((s, id, label)) = r.section {
            page.section = Some(s);
            page.ids.insert(id.clone());
            page.days.push((id, label));
        }
        Ok(())
    }

    /// The HTML of one item (with its section heading when it opens one),
    /// for `page`.
    fn render(&self, page: &Page, p: &Pending) -> Result<Rendered, ExportError> {
        let it = &p.item;
        let mut h = H::default();
        let mut r = Rendered {
            html: String::new(),
            counts: MarkCounts::default(),
            data_bytes: 0,
            embedded: 0,
            unavailable: 0,
            size_limited: 0,
            reply_links: 0,
            larger: Vec::new(),
            section: None,
        };
        // The section: the undated section is the items the archive sorts
        // as undated; an item in the dated range without a usable time
        // stays where it is.
        let time = time_text(&it.date, self.style);
        let want = match (&time, is_undated(it)) {
            (_, true) => Some(Section::Undated),
            (Some((_, day)), false) => Some(Section::Day(*day)),
            (None, false) => page.section.is_none().then_some(Section::NoTime),
        };
        if let Some(s) = want {
            if page.section.as_ref() != Some(&s) {
                let (base, label) = match &s {
                    Section::Day(d) => (format!("d{}", date_text(*d)), date_text(*d)),
                    Section::Undated => ("undated".to_owned(), t::UNDATED.to_owned()),
                    Section::NoTime => ("notime".to_owned(), t::NO_TIME_SECTION.to_owned()),
                };
                let mut id = base.clone();
                let mut k = 2;
                while page.ids.contains(&id) {
                    id = format!("{base}-{k}");
                    k += 1;
                }
                if page.section.is_some() {
                    h.raw("</section>\n");
                }
                h.raw("<section");
                h.attr("aria-labelledby", &id);
                h.raw("><h2");
                h.attr("id", &id);
                h.raw(">");
                h.text(&label);
                h.raw("</h2>\n");
                r.section = Some((s, id, label));
            }
        }
        // The article and its heading.
        let n = it.n;
        h.raw("<article");
        h.attr("id", &format!("m{n}"));
        h.attr("aria-labelledby", &format!("m{n}-h"));
        if is(it.direction.token, Direction::FromMe) {
            h.attr("class", "me");
        }
        h.raw(">\n<h3");
        h.attr("id", &format!("m{n}-h"));
        h.raw("><span class=\"who\">");
        let SenderTags { resolved, mut tags } = sender_tags(&self.words, it);
        if resolved {
            match it.sender.handle {
                Some(hd) if self.handles.contains_key(&hd) => self.handle_into(&mut h, hd),
                _ => h.text(&self.words.say("senderReason", it.sender.reason)),
            }
        } else if !tags.is_empty() {
            let who = tags.remove(0);
            h.content(&who, true, "");
        }
        h.raw("</span> ");
        match (&time, is_undated(it)) {
            (_, true) => h.el("<span class=\"t\">", t::UNDATED, "</span>"),
            (Some((human, _)), false) => {
                h.raw("<time");
                if let Some(u) = it.date.utc.as_deref().and_then(datetime) {
                    h.attr("datetime", &u);
                }
                h.raw(">");
                h.text(human);
                h.raw("</time>");
            }
            (None, false) => h.el("<span class=\"t\">", t::NO_TIME, "</span>"),
        }
        h.raw(" <span class=\"t\">");
        h.text(&format!("item {n}"));
        h.raw("</span></h3>\n");
        item_tags(&self.words, it, &mut tags);
        if !tags.is_empty() {
            h.raw("<p class=\"q\">");
            for (i, tag) in tags.iter().enumerate() {
                if i > 0 {
                    h.raw(" ");
                }
                h.text("[");
                h.content_inner(tag, true);
                h.text("]");
            }
            h.raw("</p>\n");
        }
        self.body(&mut h, it);
        self.details(&mut h, it, page, &mut r);
        self.attachments(&mut h, it, &p.pics, page, &mut r);
        self.reactions(&mut h, it);
        self.history(&mut h, it);
        if !p.versions.is_empty() {
            h.raw("<details><summary>");
            h.text(t::VERSIONS);
            h.raw("</summary>\n<ol class=\"versions\">\n");
            for v in &p.versions {
                self.version(&mut h, v);
            }
            h.raw("</ol></details>\n");
        }
        h.raw("</article>\n");
        r.html = h.s;
        r.counts = h.counts;
        Ok(r)
    }

    /// Runs of recorded text in one paragraph, after an optional label.
    fn runs(&self, h: &mut H, class: &str, label: Option<&str>, runs: &[Run<'_>]) {
        h.raw("<p");
        h.attr("class", class);
        h.raw(">");
        if let Some(l) = label {
            h.el("<span class=\"label\">", l, "</span> ");
        }
        for r in runs {
            match r {
                Run::Text { text, class } => h.content(text, self.markers, class),
                Run::Placeholder(p) => h.ph(p),
            }
        }
        h.raw("</p>\n");
    }

    fn note(h: &mut H, words: &str) {
        h.el("<p class=\"note\">", words, "</p>\n");
    }

    fn pieces<'a>(pieces: &'a [PieceRecord]) -> Vec<Run<'a>> {
        pieces
            .iter()
            .map(|p| match &p.text {
                Some(t) if is(p.kind, SegmentKindCode::Text) => Run::Text {
                    text: t.as_str(),
                    class: String::new(),
                },
                _ => Run::Placeholder(phrase("oneTimeCodeMarker").to_owned()),
            })
            .collect()
    }

    fn style_class(seg: &SegmentRecord) -> String {
        let mut c: Vec<String> = Vec::new();
        for s in &seg.styles {
            for (v, token) in [
                (Style::Bold, "b"),
                (Style::Italic, "i"),
                (Style::Underline, "u"),
                (Style::Strikethrough, "s"),
            ] {
                if is(*s, v) {
                    c.push(token.to_owned());
                }
            }
        }
        if seg.mention {
            c.push("mention".to_owned());
        }
        if seg.url {
            c.push("url".to_owned());
        }
        let mut out = String::new();
        for (i, x) in c.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(x);
        }
        out
    }

    fn body(&self, h: &mut H, it: &ItemRecord) {
        match &it.body {
            BodyRecord::Text {
                segments, lossy, ..
            } => {
                let mut main: Vec<Run<'_>> = Vec::new();
                let mut spoken: Vec<Run<'_>> = Vec::new();
                for seg in segments {
                    let k = seg.kind;
                    let run = if is(k, SegmentKindCode::Text) {
                        match &seg.text {
                            Some(t) => Run::Text {
                                text: t.as_str(),
                                class: HtmlWriter::style_class(seg),
                            },
                            None => continue,
                        }
                    } else if is(k, SegmentKindCode::Withheld) {
                        Run::Placeholder(phrase("oneTimeCodeMarker").to_owned())
                    } else if is(k, SegmentKindCode::Attachment) {
                        match seg.attachment {
                            Some(n) => Run::Placeholder(format!("Attachment {n}")),
                            None => Run::Placeholder(self.words.say("segmentKind", k)),
                        }
                    } else {
                        Run::Placeholder(self.words.say("segmentKind", k))
                    };
                    if seg.transcription {
                        spoken.push(run);
                    } else {
                        main.push(run);
                    }
                }
                if !main.is_empty() {
                    self.runs(h, "body", None, &main);
                }
                if !spoken.is_empty() {
                    self.runs(
                        h,
                        "seg transcription",
                        Some(phrase("transcription")),
                        &spoken,
                    );
                }
                if *lossy {
                    HtmlWriter::note(
                        h,
                        "Some bytes of this text were not valid text and show as U+FFFD",
                    );
                }
            }
            BodyRecord::NoText { reason } => {
                HtmlWriter::note(h, &self.words.say("noTextReason", *reason));
            }
        }
        if it.body_facts.sources_differ == Some(true) {
            HtmlWriter::note(h, phrase("sourcesDiffer"));
        }
        match &it.alternative_text {
            AltTextRecord::None => {}
            AltTextRecord::Text { source, text } => {
                let label = self.words.say("altTextSource", *source);
                match &text.text {
                    Some(c) => self.runs(
                        h,
                        "seg",
                        Some(&format!("{label}:")),
                        &[Run::Text {
                            text: c.as_str(),
                            class: String::new(),
                        }],
                    ),
                    None => HtmlWriter::note(h, &format!("{label}: {}", self.missing_words(text))),
                }
            }
            AltTextRecord::Withheld { why, .. } => {
                HtmlWriter::note(h, &self.words.say("altTextWithheld", *why));
            }
        }
        match &it.earlier_text {
            EarlierRecord::None | EarlierRecord::Available => {}
            EarlierRecord::Withheld { reason } => {
                HtmlWriter::note(h, &self.words.say("withheldReason", *reason));
            }
            EarlierRecord::Revealed { reason, pieces, .. } => {
                let label = format!("{}:", self.words.say("revealedReason", *reason));
                self.runs(h, "seg", Some(&label), &HtmlWriter::pieces(pieces));
            }
        }
    }

    /// `label <bdi>value</bdi>` in a note, or the value's missing state.
    fn value_note(&self, h: &mut H, label: &str, v: &TextValue) {
        h.raw("<p class=\"note\">");
        h.text(label);
        h.text(" ");
        if !HtmlWriter::value_into(h, v) {
            h.text(&self.missing_words(v));
        }
        h.raw("</p>\n");
    }

    /// A reference to item `n`: a backward link when it is on this page.
    fn item_ref(h: &mut H, n: Ordinal, page: &Page, r: &mut Rendered) {
        if page.written.contains(&n) {
            h.raw("<a");
            h.attr("href", &format!("#m{n}"));
            h.raw(">");
            h.text(&format!("item {n}"));
            h.raw("</a>");
            r.reply_links += 1;
        } else {
            h.text(&format!("item {n}"));
        }
    }

    fn details(&self, h: &mut H, it: &ItemRecord, page: &Page, r: &mut Rendered) {
        if it.subject.text.is_some() {
            self.value_note(h, "Subject:", &it.subject);
        }
        if it.account.text.is_some() {
            self.value_note(h, &format!("{}:", phrase("account")), &it.account);
        }
        if let Some(ev) = &it.event {
            let what = self.words.say("itemKind", ev.kind);
            HtmlWriter::note(h, &format!("Event: {what}"));
            if !ev.handles.is_empty() {
                h.raw("<p class=\"note\">");
                h.text("Addresses recorded with it: ");
                for (i, hd) in ev.handles.iter().enumerate() {
                    if i > 0 {
                        h.text(", ");
                    }
                    if self.handles.contains_key(hd) {
                        self.handle_into(h, *hd);
                    } else {
                        h.text(&hd.to_string());
                    }
                }
                h.raw("</p>\n");
            }
            if let Some(tv) = &ev.title {
                if tv.text.is_some() {
                    self.value_note(h, "Name recorded with it:", tv);
                }
            }
        }
        if let Some(a) = &it.association {
            h.raw("<p class=\"note\">");
            h.text("Reaction");
            if let Some(k) = a.kind {
                h.text(&format!(": {}", self.words.say("assocKind", k)));
            }
            if let Some(e) = &a.emoji {
                if e.text.is_some() {
                    h.text(" ");
                    HtmlWriter::value_into(h, e);
                }
            }
            match a.target {
                Some(MessageRefRecord::InExport { n }) => {
                    h.text(", on ");
                    HtmlWriter::item_ref(h, n, page, r);
                }
                Some(MessageRefRecord::NotInExport) => {
                    h.text(&format!(", on a message {}", phrase("notInExport")));
                }
                None => {}
            }
            h.text(&format!(
                " ({})",
                self.words.say("assocVisibility", a.visibility)
            ));
            h.raw("</p>\n");
        }
        if let Some(rp) = &it.reply {
            h.raw("<p class=\"note\">");
            match rp.originator {
                Some(MessageRefRecord::InExport { n }) => {
                    if page.written.contains(&n) {
                        h.text("Reply to ");
                        HtmlWriter::item_ref(h, n, page, r);
                    } else if it.context.reply_in_conversation == Some(false) {
                        h.text(&format!(
                            "Reply to item {n}, in another conversation of this export"
                        ));
                    } else {
                        h.text("Reply to ");
                        h.text(&phrase("replyElsewhere").replace("{n}", &n.to_string()));
                    }
                }
                Some(MessageRefRecord::NotInExport) => {
                    h.text(&format!("Reply to a message {}", phrase("notInExport")));
                }
                None => h.text(&self.words.say("replyResolution", rp.resolution)),
            }
            h.raw("</p>\n");
        }
        if let Determinable::Determined { value } = &it.reply_count {
            if value.in_export > 0 || value.elsewhere > 0 {
                HtmlWriter::note(
                    h,
                    &format!(
                        "Replies: {} in this export, {} {}",
                        value.in_export,
                        value.elsewhere,
                        phrase("notInExport")
                    ),
                );
            }
        }
    }

    fn attachments(&self, h: &mut H, it: &ItemRecord, pics: &[Pic], page: &Page, r: &mut Rendered) {
        for (i, a) in it.attachments.iter().enumerate() {
            h.raw("<div class=\"att\">\n<p><span class=\"label\">");
            h.text(&format!("Attachment {}:", a.n));
            h.raw("</span> ");
            let named = match &a.name.text {
                Some(n) if n.as_str().is_empty() => {
                    h.text("an empty name recorded");
                    false
                }
                Some(n) => {
                    h.content(n.as_str(), true, "");
                    true
                }
                None => {
                    h.text(&format!(
                        "no name recorded ({})",
                        self.missing_words(&a.name)
                    ));
                    false
                }
            };
            h.raw("</p>\n<p class=\"sub\">");
            h.text(&self.words.say("attachmentOutcome", a.file.outcome));
            if let Some(path) = &a.file.path {
                // The copy's path, as text: originals are never linked.
                h.text(": ");
                h.content(path.as_str(), true, "");
            }
            h.raw("</p>\n");
            if let Some(k) = a.file.sniffed {
                HtmlWriter::sub(h, &format!("Content: {}", self.words.say("sniffedKind", k)));
            }
            if a.mime_type.text.is_some() {
                h.raw("<p class=\"sub\">");
                h.text("Declared type: ");
                HtmlWriter::value_into(h, &a.mime_type);
                h.raw("</p>\n");
            }
            if !is(a.access, AttachmentAccessCode::Included) {
                HtmlWriter::sub(h, &self.words.say("attachmentAccess", a.access));
            }
            if !is(a.card.state, CardStateCode::File) {
                let mut s = self.words.say("cardState", a.card.state);
                if let (Some(l), Some(c)) = (&a.card.unavailable_lookup, &a.card.unavailable_copy) {
                    s = s
                        .replace("{lookup}", l.as_str())
                        .replace("{copy}", c.as_str());
                }
                HtmlWriter::sub(h, &s);
            }
            if a.emoji_description.text.is_some() {
                h.raw("<p class=\"sub\">");
                h.text("Description recorded with it: ");
                HtmlWriter::value_into(h, &a.emoji_description);
                h.raw("</p>\n");
            }
            for hv in &a.history {
                self.value_history(h, hv, "sub");
            }
            match pics.get(i) {
                Some(Pic::Ready { thumb, display }) => {
                    let uri_len = (format!("data:{};base64,", thumb.mime()).len()
                        + preview::base64_len(thumb.bytes().len()))
                        as u64;
                    if page.data_bytes + r.data_bytes + uri_len > self.limits.data_bytes {
                        h.el("<p class=\"card\">", t::SIZE_LIMIT, "</p>\n");
                        r.size_limited += 1;
                    } else {
                        h.raw("<figure><img alt=\"\"");
                        h.attr("width", &thumb.width().to_string());
                        h.attr("height", &thumb.height().to_string());
                        h.attr("src", &preview::data_uri(thumb));
                        h.raw("><figcaption>");
                        if named {
                            HtmlWriter::value_into(h, &a.name);
                        } else {
                            h.text(&format!("Attachment {}", a.n));
                        }
                        h.text(&format!(" \u{2014} {}", t::PREVIEW));
                        h.raw("</figcaption></figure>\n");
                        r.data_bytes += uri_len;
                        r.embedded += 1;
                    }
                    if let Some(d) = display {
                        if !self.single {
                            h.raw("<p class=\"sub\"><a");
                            h.attr("href", &href(&BundlePath::html_preview(a.n, d.ext())));
                            h.raw(">");
                            h.text(t::LARGER);
                            h.raw("</a></p>\n");
                            r.larger.push((a.n, d.clone()));
                        }
                    }
                }
                Some(Pic::Unavailable) => {
                    h.el("<p class=\"card\">", t::UNAVAILABLE, "</p>\n");
                    r.unavailable += 1;
                }
                Some(Pic::None) | None => {}
            }
            h.raw("</div>\n");
        }
    }

    fn sub(h: &mut H, words: &str) {
        h.el("<p class=\"sub\">", words, "</p>\n");
    }

    fn reactions(&self, h: &mut H, it: &ItemRecord) {
        match &it.reactions {
            Included::NotIncluded { counts, .. } => {
                if counts.rows > 0 {
                    HtmlWriter::note(
                        h,
                        &format!(
                            "Reactions not included in this export: {} recorded",
                            counts.rows
                        ),
                    );
                }
            }
            Included::Included { value } => {
                if value.chips.is_empty() {
                    return;
                }
                h.raw("<ul class=\"chips\">\n");
                for c in &value.chips {
                    h.raw("<li>");
                    if c.includes_rows_outside_scope {
                        h.text(&format!("Reaction ({}): ", phrase("chipsOutsideScope")));
                    } else {
                        h.text(&format!("Reaction ({}): ", phrase("derived")));
                    }
                    h.text(&self.words.say("reactionKind", c.kind));
                    if let Some(e) = &c.emoji {
                        if e.text.is_some() {
                            h.text(" ");
                            HtmlWriter::value_into(h, e);
                        }
                    }
                    h.text(" by ");
                    match c.sender.handle {
                        Some(hd)
                            if is(c.sender.kind, ChipSenderCode::Handle)
                                && self.handles.contains_key(&hd) =>
                        {
                            self.handle_into(h, hd)
                        }
                        _ => h.text(&self.words.say("chipSender", c.sender.kind)),
                    }
                    h.text(&format!(", {}", self.words.say("chipState", c.state)));
                    h.raw("</li>\n");
                }
                h.raw("</ul>\n");
            }
        }
    }

    fn value_history(&self, h: &mut H, v: &ValueHistoryRecord, class: &str) {
        h.raw("<p");
        h.attr("class", class);
        h.raw(">");
        match v {
            ValueHistoryRecord::Value { column, value, .. } => {
                let col = self.words.say("historyColumn", *column);
                h.text(&format!("Other recorded value of {col}: "));
                if !HtmlWriter::value_into(h, value) {
                    h.text(&self.missing_words(value));
                }
            }
            ValueHistoryRecord::Withheld { column, reason } => {
                let col = self.words.say("historyColumn", *column);
                h.text(&format!(
                    "Other recorded value of {col}: {}",
                    self.words.say("withheldReason", *reason)
                ));
            }
        }
        h.raw("</p>\n");
    }

    fn history(&self, h: &mut H, it: &ItemRecord) {
        if let Included::NotIncluded { counts, .. } = &it.history {
            if counts.captured_versions > 1 || counts.phone_entries > 0 {
                HtmlWriter::note(
                    h,
                    &format!(
                        "Stored versions not included in this export: {} captured, {} phone edit entries",
                        counts.captured_versions, counts.phone_entries
                    ),
                );
            }
        }
        for v in &it.value_history {
            self.value_history(h, v, "note");
        }
    }

    fn version(&self, h: &mut H, v: &VersionRecord) {
        h.raw("<li>");
        let label = format!(
            "Stored version{}, kept by imports {} to {}:",
            if v.current { " (current)" } else { "" },
            v.first_session,
            v.last_session
        );
        match &v.text {
            VersionTextRecord::Text { pieces } => {
                self.runs(h, "seg", Some(&label), &HtmlWriter::pieces(pieces))
            }
            VersionTextRecord::Withheld { reason } => HtmlWriter::note(
                h,
                &format!("{label} {}", self.words.say("withheldReason", *reason)),
            ),
            VersionTextRecord::NoText { reason, .. } => HtmlWriter::note(
                h,
                &format!("{label} {}", self.words.say("noTextReason", *reason)),
            ),
        }
        for e in &v.edits {
            let at = e.at.raw.as_ref().map_or("not recorded", |r| r.as_str());
            let label = format!(
                "Phone edit entry, part {}, {} {at}:",
                e.part.as_str(),
                phrase("phoneEditTime")
            );
            match &e.text {
                EditEntryTextRecord::Text { pieces } => {
                    self.runs(h, "seg", Some(&label), &HtmlWriter::pieces(pieces))
                }
                EditEntryTextRecord::Withheld { reason } => HtmlWriter::note(
                    h,
                    &format!("{label} {}", self.words.say("withheldReason", *reason)),
                ),
                EditEntryTextRecord::NoBody => {
                    HtmlWriter::note(h, &format!("{label} No text recorded for this version"))
                }
                EditEntryTextRecord::Failed { .. } => {
                    HtmlWriter::note(h, &format!("{label} Edit text could not be decoded"))
                }
                EditEntryTextRecord::NotMatched => {
                    HtmlWriter::note(h, &format!("{label} Edit text could not be matched"))
                }
            }
        }
        h.raw("</li>\n");
    }

    // -----------------------------------------------------------------------
    // The conversation header and the index
    // -----------------------------------------------------------------------

    fn conversation_header(&self, c: ConversationFile<'_>) -> (String, MarkCounts, Option<String>) {
        let mut h = H::default();
        let mut name = None;
        match c {
            ConversationFile::Conversation(r) => {
                h.raw("<h1>");
                h.text(&format!("Conversation {}: ", r.n));
                match &r.name.text {
                    Some(n) if !n.as_str().is_empty() => {
                        h.content(n.as_str(), true, "");
                        name = Some(n.as_str().to_owned());
                    }
                    _ => h.text("No name recorded"),
                }
                h.raw("</h1>\n");
                HtmlWriter::note(
                    &mut h,
                    &format!(
                        "Addresses recorded in this conversation (includes people who left): {}",
                        r.participants.len()
                    ),
                );
                for v in &r.name_history {
                    h.raw("<p class=\"note\">");
                    h.text("Other recorded name of this conversation: ");
                    match v {
                        ValueHistoryRecord::Value { value, .. } => {
                            if !HtmlWriter::value_into(&mut h, value) {
                                h.text(&self.missing_words(value));
                            }
                        }
                        ValueHistoryRecord::Withheld { reason, .. } => {
                            h.text(&self.words.say("withheldReason", *reason));
                        }
                    }
                    h.raw("</p>\n");
                }
            }
            ConversationFile::NoConversation => {
                h.el(
                    "<h1>",
                    "No conversation: messages recorded in no conversation",
                    "</h1>\n",
                );
            }
        }
        HtmlWriter::note(
            &mut h,
            &format!("Times: {} (offsets shown on every item)", self.zone_name),
        );
        (h.s, h.counts, name)
    }

    fn write_index(&mut self, about: &[String], out: &mut Staging) -> Result<(), ExportError> {
        let mut h = H::default();
        HtmlWriter::head(t::TITLE, &mut h);
        h.raw("<a class=\"skip\" href=\"#main\">");
        h.text(t::SKIP_LIST);
        h.raw("</a>\n<header>\n<p class=\"notice\">");
        h.text(t::NOTICE);
        h.raw(" <a href=\"#about\">");
        h.text(t::ABOUT);
        h.raw("</a></p>\n");
        h.el("<h1>", t::TITLE, "</h1>\n");
        h.raw("</header>\n<main id=\"main\">\n");
        h.el("<p class=\"label\">", t::INDEX_HEADING, "</p>\n");
        h.raw("<ul class=\"convs\">\n");
        for e in &self.index {
            h.raw("<li><a");
            h.attr("href", &href(&BundlePath::html_page(e.stem, 1)));
            h.raw(">");
            match e.n {
                Some(n) => h.text(&format!("Conversation {n}")),
                None => h.text("Messages in no conversation"),
            }
            h.raw("</a>");
            if let Some(n) = &e.name {
                h.text(": ");
                h.content(n, true, "");
            }
            h.text(&format!(
                " \u{2014} {} {}",
                e.items,
                if e.items == 1 { "item" } else { "items" }
            ));
            if e.parts > 1 {
                h.text("; parts:");
                for k in 2..=e.parts {
                    h.raw(" <a");
                    h.attr("href", &href(&BundlePath::html_page(e.stem, k)));
                    h.raw(">");
                    h.text(&k.to_string());
                    h.raw("</a>");
                }
            }
            h.raw("</li>\n");
        }
        h.raw("</ul>\n</main>\n<footer id=\"about\">\n");
        h.el("<p class=\"label\">", t::ABOUT, "</p>\n");
        h.raw("<ul class=\"legend\">\n");
        for l in t::LEGEND {
            h.el("<li>", l, "</li>\n");
        }
        h.raw("</ul>\n<ul class=\"about\">\n");
        for l in about {
            h.raw("<li>");
            h.content(l, true, "");
            h.raw("</li>\n");
        }
        h.raw("</ul>\n</footer>\n</body>\n</html>\n");
        self.counts.add(h.counts);
        let mut f = out.create(&BundlePath::html_index())?;
        f.write_all(h.s.as_bytes()).map_err(io)?;
        out.close(f)?;
        self.pages += 1;
        Ok(())
    }
}

/// An HTML `datetime` value from the record's UTC time: at most three
/// fraction digits (HTML's "global date and time"), none when they are
/// zero; `None` for a year HTML cannot write (beyond 9999 or before 0).
fn datetime(utc: &str) -> Option<String> {
    let body = utc.strip_suffix('Z')?;
    if body.starts_with(['+', '-']) {
        return None;
    }
    let (base, frac) = match body.split_once('.') {
        Some((b, f)) => (b, f),
        None => (body, ""),
    };
    let ms: String = frac.chars().take(3).collect();
    if ms.is_empty() || ms.bytes().all(|b| b == b'0') {
        Some(format!("{base}Z"))
    } else {
        Some(format!("{base}.{ms}Z"))
    }
}

/// A link from a page in `html/` to another file there.
fn href(p: &BundlePath) -> String {
    p.href_from(Format::Html.token()).unwrap_or_default()
}

/// The About lines of `about` (the manifest's fields without `files`).
fn about_lines(about: &AboutDraft<'_>) -> Result<Vec<String>, ExportError> {
    let v = serde_json::to_value(about.body)
        .map_err(|_| ExportError::Limit(LimitKind::NotSerializable))?;
    let mut lines = Vec::new();
    flatten("", &v, &mut lines);
    Ok(lines)
}

impl ExportWriter for HtmlWriter {
    fn format(&self) -> Format {
        Format::Html
    }

    fn begin(&mut self, ctx: &ExportContext<'_>, _out: &mut Staging) -> Result<(), ExportError> {
        let o = ctx.plan.options();
        self.markers = o.hidden_character_markers;
        self.style = HumanStyle {
            h23: o.clock == Clock::H23,
            seconds: o.seconds,
        };
        self.single = ctx.plan.packaging() == Packaging::SingleFile;
        self.previews = o.previews;
        self.larger = o.larger_previews && !self.single;
        if self.previews && self.source.is_none() {
            return Err(ExportError::Plan(
                crate::error::PlanRefusal::OptionDoesNotApply,
            ));
        }
        self.zone_name = match ctx.zone {
            ExportZone::Utc => "UTC".to_owned(),
            ExportZone::Table(t) => t.name().to_owned(),
        };
        Ok(())
    }

    fn handle(&mut self, h: &HandleRecord, _out: &mut Staging) -> Result<(), ExportError> {
        self.handles
            .insert(h.n, h.address.text.as_ref().map(|c| c.as_str().to_owned()));
        Ok(())
    }

    fn begin_conversation(
        &mut self,
        c: ConversationFile<'_>,
        out: &mut Staging,
    ) -> Result<(), ExportError> {
        if self.conv.is_some() || (self.single && self.report.conversation_files > 0) {
            return Err(order());
        }
        let (stem, n) = match c {
            ConversationFile::Conversation(r) => (FileStem::Conversation(r.n), Some(r.n)),
            ConversationFile::NoConversation => (FileStem::NoConversation, None),
        };
        let (header, header_counts, name) = self.conversation_header(c);
        self.conv = Some(Conv {
            stem,
            n,
            header,
            header_counts,
            page: None,
            parts: 0,
            pending: None,
            items: 0,
            folded: 0,
        });
        self.report.conversation_files += 1;
        self.index.push(IndexEntry {
            stem,
            n,
            name,
            items: 0,
            parts: 0,
        });
        self.open_page(out)
    }

    fn item(&mut self, it: &ItemRecord, out: &mut Staging) -> Result<(), ExportError> {
        if self.conv.is_none() {
            return Err(order());
        }
        if it.context.folded_into.is_some() {
            if let Some(c) = self.conv.as_mut() {
                c.folded += 1;
            }
            self.folded_last = Some(it.n);
            self.report.folded_rows += 1;
            return Ok(());
        }
        self.folded_last = None;
        self.flush(out)?;
        let pics = self.pics(it)?;
        let c = self.conv.as_mut().ok_or_else(order)?;
        c.pending = Some(Pending {
            item: it.clone(),
            versions: Vec::new(),
            pics,
        });
        c.items += 1;
        self.report.items += 1;
        Ok(())
    }

    fn version(&mut self, v: &VersionRecord, _out: &mut Staging) -> Result<(), ExportError> {
        // A folded reaction row is a chip of its target, not an item: its
        // stored versions are in the structured formats only.
        if self.folded_last == Some(v.message) {
            return Ok(());
        }
        let p = self
            .conv
            .as_mut()
            .and_then(|c| c.pending.as_mut())
            .ok_or_else(order)?;
        if p.item.n != v.message {
            return Err(order());
        }
        p.versions.push(v.clone());
        Ok(())
    }

    fn end_conversation(
        &mut self,
        totals: &ConversationTotals,
        out: &mut Staging,
    ) -> Result<(), ExportError> {
        self.flush(out)?;
        let (items, folded, parts) = {
            let c = self.conv.as_ref().ok_or_else(order)?;
            (c.items, c.folded, c.parts)
        };
        if totals.items != items || totals.folded_rows != folded {
            return Err(ExportError::Limit(LimitKind::RowCountMismatch));
        }
        if let Some(e) = self.index.last_mut() {
            e.items = items;
            e.parts = parts;
        }
        if self.single {
            // The single file is closed by `finish`, after About.
            return Ok(());
        }
        self.close_page(false, Some((items, folded)), None, out)?;
        self.conv = None;
        Ok(())
    }

    fn finish(
        mut self: Box<Self>,
        about: &AboutDraft<'_>,
        out: &mut Staging,
    ) -> Result<WriterReport, ExportError> {
        let lines = about_lines(about)?;
        if self.single {
            let (items, folded) = match self.conv.as_ref() {
                Some(c) if c.pending.is_none() => (c.items, c.folded),
                _ => return Err(order()),
            };
            self.close_page(false, Some((items, folded)), Some(&lines), out)?;
            self.conv = None;
        } else {
            if self.conv.is_some() {
                return Err(ExportError::Limit(LimitKind::FileStillOpen));
            }
            self.write_index(&lines, out)?;
        }
        let mut r = self.report.clone();
        let c = self.counts;
        r.counts.insert("hiddenCharactersMarked", c.hidden_marked);
        r.counts.insert("controlsEscaped", c.controls_escaped);
        r.counts
            .insert("combiningRunsLimited", c.combining_runs_limited);
        r.counts.insert("reservedEscaped", c.reserved_escaped);
        r.counts.insert("previewsEmbedded", self.embedded);
        // A preview over the page's budget is not embedded: the manifest
        // counts it with the unavailable ones; the report keeps both.
        r.counts
            .insert("previewsUnavailable", self.unavailable + self.size_limited);
        r.counts.insert("previewsSizeLimited", self.size_limited);
        r.counts
            .insert("largerPreviewsWritten", self.larger_written.len() as u64);
        r.counts.insert("replyLinks", self.reply_links);
        r.counts.insert("pages", self.pages);
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;

    #[test]
    fn the_style_hash_is_pinned() {
        let d = sha2::Sha256::digest(STYLE.as_bytes());
        assert_eq!(preview::base64(&d), STYLE_SHA256);
        assert!(csp().contains(&format!("'sha256-{STYLE_SHA256}'")));
        assert!(STYLE.contains("article{overflow:hidden;contain:paint"));
        assert!(STYLE.contains("@media print{") && STYLE.contains("break-inside:avoid"));
        // The style cannot close its own element or load anything.
        for bad in ["</", "url(", "@import", "expression(", "\\"] {
            assert!(!STYLE.contains(bad), "{bad}");
        }
    }

    fn rgb(hex: &str) -> [f64; 3] {
        let v = u32::from_str_radix(hex.trim_start_matches('#'), 16).expect("hex");
        let c = |s: u32| {
            let x = f64::from((v >> s) & 0xff) / 255.0;
            if x <= 0.039_28 {
                x / 12.92
            } else {
                ((x + 0.055) / 1.055).powf(2.4)
            }
        };
        [c(16), c(8), c(0)]
    }

    fn contrast(a: &str, b: &str) -> f64 {
        let l = |c: [f64; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let (x, y) = (l(rgb(a)), l(rgb(b)));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// The custom properties of one `:root{…}` block.
    fn props(block: &str) -> BTreeMap<String, String> {
        block
            .split(';')
            .filter_map(|d| d.trim().strip_prefix("--"))
            .filter_map(|d| d.split_once(':'))
            .map(|(k, v)| (k.to_owned(), v.trim_end_matches('}').to_owned()))
            .collect()
    }

    #[test]
    fn text_contrast_is_at_least_4_5_in_both_schemes() {
        let light = STYLE.split('}').next().expect("light");
        let dark = STYLE
            .split("prefers-color-scheme:dark){:root{")
            .nth(1)
            .and_then(|s| s.split('}').next())
            .expect("dark");
        for block in [light, dark] {
            let p = props(block);
            for (fg, bg) in [
                ("fg", "bg"),
                ("muted", "bg"),
                ("link", "bg"),
                ("mark-fg", "mark-bg"),
                ("ph-fg", "ph-bg"),
            ] {
                let r = contrast(&p[fg], &p[bg]);
                assert!(r >= 4.5, "{fg} on {bg}: {r:.2}");
            }
        }
    }

    #[test]
    fn content_is_escaped_and_marks_are_elements() {
        let mut h = H::default();
        h.content("<script>&\"'x\u{202E}\u{1B}\r\ny\rz\u{27E8}", true, "");
        assert_eq!(
            h.s,
            "<bdi dir=\"auto\">&lt;script&gt;&amp;&quot;&#x27;x<span class=\"mark\">U+202E</span>\
             <span class=\"mark\">U+001B</span>\ny\nz\u{27E8}</bdi>"
        );
        assert_eq!((h.counts.hidden_marked, h.counts.controls_escaped), (1, 1));
        // Markers off: hidden characters stay, controls are still marked.
        let mut h = H::default();
        h.content("a\u{200B}b\u{0}", false, "b i");
        assert_eq!(
            h.s,
            "<bdi dir=\"auto\" class=\"b i\">a\u{200B}b<span class=\"mark\">U+0000</span></bdi>"
        );
        // Noncharacters are HTML parse errors: always marked.
        let mut h = H::default();
        h.content_inner("\u{FDD0}x\u{1FFFF}", false);
        assert_eq!(
            h.s,
            "<span class=\"mark\">U+FDD0</span>x<span class=\"mark\">U+1FFFF</span>"
        );
    }

    #[test]
    fn datetimes_are_html_global_dates() {
        assert_eq!(
            datetime("2026-01-13T12:27:40.000000000Z").as_deref(),
            Some("2026-01-13T12:27:40Z")
        );
        assert_eq!(
            datetime("2026-01-13T12:27:40.123456789Z").as_deref(),
            Some("2026-01-13T12:27:40.123Z")
        );
        assert_eq!(
            datetime("2004-03-03T09:46:40Z").as_deref(),
            Some("2004-03-03T09:46:40Z")
        );
        assert_eq!(datetime("+10000-01-01T00:00:00Z"), None);
        assert_eq!(datetime("-0001-01-01T00:00:00Z"), None);
    }

    #[test]
    fn words_are_escaped_too() {
        let mut h = H::default();
        h.text("a<b>&");
        h.attr("title", "\"x\"");
        assert_eq!(h.s, "a&lt;b&gt;&amp; title=\"&quot;x&quot;\"");
    }
}
