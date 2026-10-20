//! What the main process sends the PDF backend (M6 spec §5.6, §4.7, D138).
//!
//! The export-render helper lays out and serialises; it does not word,
//! mark or format anything. The main process turns each record into
//! [`Block`]s (headings and transcript entries made of [`Para`]graphs) and
//! sends each as one `Conversation` or `Item` frame ([`Block::encode`]).
//!
//! **Every string the helper draws is a [`PdfText`],** and a `PdfText` is
//! made only here:
//!
//! - [`TextBuilder::content`] marks recorded text with
//!   [`MarkPolicy::text_body`] (controls always escaped, hidden characters
//!   marked when the "hidden-character markers" option is on, a literal
//!   U+27E8 escaped, a combining run over 16 marks cut to its first 16
//!   plus `⟨+N combining marks⟩`), exactly as TXT does;
//! - [`TextBuilder::header`] marks a header field (a sender, a name, a
//!   qualifier's recorded value) with [`MarkPolicy::text_header`]: hidden
//!   characters always marked, line breaks visible, never a new line;
//! - [`TextBuilder::ours`] appends our own words (`words.rs` sentences,
//!   formatted times and numbers, placeholders), verbatim.
//!
//! **The text layer follows the visible text (§4.7).** The helper draws
//! each `PdfText` and gives krilla the same string as the glyphs' text,
//! so the PDF's ToUnicode and ActualText carry exactly what is drawn:
//! with markers on, both carry `⟨U+202E⟩`; with markers off, both carry
//! the true character. There is no second string from which a text layer
//! could differ.
//!
//! **Paragraphs.** Recorded text is split at its line breaks (CRLF, LF,
//! CR, U+2028, U+2029, U+0085, as TXT), one [`Para`] per line, so a bidi
//! paragraph ends at every line break (UAX #9) and a control in a body
//! cannot reorder another line. Each paragraph's base direction is its
//! first strong character's (the helper's layout); an item's header is a
//! list of separate pieces, each laid out as its own isolate.
//!
//! The encoding is JSON (`serde_json`, unknown fields refused), bounded
//! by the frame (16 MiB) and by [`MAX_HEAD_PIECES`] and [`MAX_PARAS`].

use serde::{Deserialize, Serialize};

use crate::hidden::{self, MarkCounts, MarkPolicy};
use crate::writer::txt::split_lines;

/// The `Begin` options' format version.
pub const OPTIONS_VERSION: u8 = 1;
/// The longest document title, in bytes.
pub const MAX_TITLE_BYTES: usize = 256;
/// The most pieces an entry's header may have.
pub const MAX_HEAD_PIECES: usize = 256;
/// The most paragraphs one block may have.
pub const MAX_PARAS: usize = 65_536;
/// The deepest paragraph indent.
pub const MAX_INDENT: u8 = 3;

/// The page size (A4 by default, US Letter as an option; §5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PageSize {
    #[default]
    A4,
    Letter,
}

impl PageSize {
    /// Width and height in PDF points.
    pub fn points(self) -> (f32, f32) {
        match self {
            PageSize::A4 => (595.275_6, 841.889_8),
            PageSize::Letter => (612.0, 792.0),
        }
    }

    fn code(self) -> u8 {
        match self {
            PageSize::A4 => 1,
            PageSize::Letter => 2,
        }
    }

    fn from_code(c: u8) -> Option<PageSize> {
        [PageSize::A4, PageSize::Letter]
            .into_iter()
            .find(|p| p.code() == c)
    }
}

/// Which Chinese face Han text outside Japanese and Korean runs uses
/// (SC by default, TC by option; §5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HanFace {
    #[default]
    Simplified,
    Traditional,
}

impl HanFace {
    fn code(self) -> u8 {
        match self {
            HanFace::Simplified => 1,
            HanFace::Traditional => 2,
        }
    }

    fn from_code(c: u8) -> Option<HanFace> {
        [HanFace::Simplified, HanFace::Traditional]
            .into_iter()
            .find(|p| p.code() == c)
    }
}

/// The PDF backend's `Begin` options. No content, no path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfOptions {
    pub page: PageSize,
    pub han: HanFace,
    /// The document information title: generic words only ("Messages
    /// export — conversation 1", D112), never a conversation's name.
    pub title: String,
}

/// Why `Begin` options were refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionsRefusal {
    Malformed,
    Version,
    Title,
}

impl PdfOptions {
    pub fn encode(&self) -> Vec<u8> {
        let mut v = vec![OPTIONS_VERSION, self.page.code(), self.han.code()];
        v.extend_from_slice(self.title.as_bytes());
        v
    }

    pub fn decode(p: &[u8]) -> Result<PdfOptions, OptionsRefusal> {
        let (&version, rest) = p.split_first().ok_or(OptionsRefusal::Malformed)?;
        if version != OPTIONS_VERSION {
            return Err(OptionsRefusal::Version);
        }
        let (&page, rest) = rest.split_first().ok_or(OptionsRefusal::Malformed)?;
        let (&han, title) = rest.split_first().ok_or(OptionsRefusal::Malformed)?;
        let page = PageSize::from_code(page).ok_or(OptionsRefusal::Malformed)?;
        let han = HanFace::from_code(han).ok_or(OptionsRefusal::Malformed)?;
        let title = std::str::from_utf8(title).map_err(|_| OptionsRefusal::Title)?;
        if title.len() > MAX_TITLE_BYTES || title.chars().any(char::is_control) {
            return Err(OptionsRefusal::Title);
        }
        Ok(PdfOptions {
            page,
            han,
            title: title.to_owned(),
        })
    }
}

/// A string the helper may draw: built only by [`TextBuilder`].
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PdfText(String);

impl std::fmt::Debug for PdfText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfText")
            .field("len", &self.0.len())
            .finish()
    }
}

impl PdfText {
    /// The text to draw (and to put in the text layer).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Our own words only (see [`TextBuilder::ours`]).
    pub fn ours(s: &str) -> PdfText {
        PdfText(s.to_owned())
    }

    /// This text cut into pieces of at most `max` bytes (at least one
    /// character each), at character boundaries: an overlong line of
    /// recorded text becomes several paragraphs (each piece keeps the
    /// marking it was built with).
    pub fn split_bytes(&self, max: usize) -> Vec<PdfText> {
        let s = self.0.as_str();
        let mut out = Vec::new();
        let mut start = 0;
        while start < s.len() {
            let mut end = start.saturating_add(max.max(1)).min(s.len());
            while end > start && !s.is_char_boundary(end) {
                end -= 1;
            }
            if end == start {
                end = s
                    .get(start..)
                    .and_then(|r| r.chars().next())
                    .map_or(s.len(), |c| start + c.len_utf8());
            }
            out.push(PdfText(s.get(start..end).unwrap_or("").to_owned()));
            start = end;
        }
        if out.is_empty() {
            out.push(PdfText::default());
        }
        out
    }
}

/// Builds one [`PdfText`] from recorded text and our own words.
#[derive(Default)]
pub struct TextBuilder {
    text: String,
    counts: MarkCounts,
}

impl std::fmt::Debug for TextBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Recorded text; never printed (I6).
        f.debug_struct("TextBuilder")
            .field("len", &self.text.len())
            .finish_non_exhaustive()
    }
}

impl TextBuilder {
    pub fn new() -> TextBuilder {
        TextBuilder::default()
    }

    /// Recorded body text **without line breaks** (use
    /// [`Para::recorded`] for text that may hold them), marked per
    /// [`MarkPolicy::text_body`].
    pub fn content(mut self, raw: &str, markers: bool) -> TextBuilder {
        let (t, n) = hidden::mark(raw, MarkPolicy::text_body(markers));
        self.text.push_str(&t);
        self.counts.add(n);
        self
    }

    /// A recorded header field, marked per [`MarkPolicy::text_header`].
    pub fn header(mut self, raw: &str) -> TextBuilder {
        let (t, n) = hidden::mark(raw, MarkPolicy::text_header());
        self.text.push_str(&t);
        self.counts.add(n);
        self
    }

    /// Our own words (a `words.rs` sentence, a formatted time, a
    /// placeholder such as `⟨one-time code hidden⟩`), verbatim. Never
    /// pass recorded text here.
    pub fn ours(mut self, s: &str) -> TextBuilder {
        self.text.push_str(s);
        self
    }

    /// A piece another `TextBuilder` built (its marking was counted
    /// when it was built).
    pub fn piece(mut self, t: &PdfText) -> TextBuilder {
        self.text.push_str(&t.0);
        self
    }

    /// Recorded body text without line breaks, marked per
    /// [`MarkPolicy::text_body`], appended in place.
    pub fn push_content(&mut self, raw: &str, markers: bool) {
        let (t, n) = hidden::mark(raw, MarkPolicy::text_body(markers));
        self.text.push_str(&t);
        self.counts.add(n);
    }

    /// Our own words, appended in place.
    pub fn push_ours(&mut self, s: &str) {
        self.text.push_str(s);
    }

    /// The text, and what marking changed.
    pub fn build(self) -> (PdfText, MarkCounts) {
        (PdfText(self.text), self.counts)
    }
}

/// What a paragraph is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ParaRole {
    /// A label of ours ("Transcription recorded on the phone:").
    Label,
    /// Recorded text, or a line made of it.
    Text,
    /// A preview image (the `Image` frame sent before the block); its
    /// text is empty.
    Figure,
}

/// One paragraph of an entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Para {
    /// 0 to [`MAX_INDENT`].
    pub indent: u8,
    pub role: ParaRole,
    pub text: PdfText,
    /// For a [`ParaRole::Figure`]: the attachment record's number, as
    /// the `Image` frame named it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<u64>,
}

impl Para {
    /// Recorded text split at its line breaks into one paragraph each,
    /// every one marked per [`MarkPolicy::text_body`]; the marking counts
    /// are added to `counts`.
    pub fn recorded(indent: u8, raw: &str, markers: bool, counts: &mut MarkCounts) -> Vec<Para> {
        split_lines(raw)
            .into_iter()
            .map(|line| {
                let (text, n) = TextBuilder::new().content(line, markers).build();
                counts.add(n);
                Para {
                    indent: indent.min(MAX_INDENT),
                    role: ParaRole::Text,
                    text,
                    image: None,
                }
            })
            .collect()
    }

    /// A paragraph of our own words.
    pub fn label(indent: u8, words: &str) -> Para {
        Para {
            indent: indent.min(MAX_INDENT),
            role: ParaRole::Label,
            text: PdfText::ours(words),
            image: None,
        }
    }

    /// A line built with [`TextBuilder`] (our words and marked values).
    pub fn built(indent: u8, role: ParaRole, text: PdfText) -> Para {
        Para {
            indent: indent.min(MAX_INDENT),
            role,
            text,
            image: None,
        }
    }

    /// The preview image of attachment record `n`.
    pub fn figure(indent: u8, n: u64) -> Para {
        Para {
            indent: indent.min(MAX_INDENT),
            role: ParaRole::Figure,
            text: PdfText::default(),
            image: Some(n),
        }
    }
}

/// A heading's level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HeadingLevel {
    /// A conversation file: starts a new page and a top-level outline
    /// entry.
    Conversation,
    /// A day (or the undated section): an outline entry under the
    /// conversation.
    Day,
    /// Any other section heading ("About this export"): an outline entry
    /// at the top level, on a new page.
    Section,
}

/// One frame's content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", deny_unknown_fields)]
pub enum Block {
    Heading {
        level: HeadingLevel,
        text: PdfText,
        /// A conversation heading's document information title (generic
        /// words only, D112: "Messages export — conversation 1"); the
        /// first one in a volume is the volume's title.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// A transcript entry: a header line of separate isolates (time,
    /// sender, each qualifier), then its paragraphs.
    Entry { head: Vec<PdfText>, body: Vec<Para> },
}

/// Why a block was refused by the helper's parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockRefusal {
    Malformed,
    TooManyPieces,
    TooManyParas,
    Indent,
    /// A figure without an image, an image on another paragraph, or a
    /// figure with text.
    Figure,
    /// A document title that is too long, has a control, or is not on a
    /// conversation heading.
    Title,
}

impl Block {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn decode(p: &[u8]) -> Result<Block, BlockRefusal> {
        let b: Block = serde_json::from_slice(p).map_err(|_| BlockRefusal::Malformed)?;
        if let Block::Entry { head, body } = &b {
            if head.len() > MAX_HEAD_PIECES {
                return Err(BlockRefusal::TooManyPieces);
            }
            if body.len() > MAX_PARAS {
                return Err(BlockRefusal::TooManyParas);
            }
            if body.iter().any(|p| p.indent > MAX_INDENT) {
                return Err(BlockRefusal::Indent);
            }
            let figure_ok = |p: &Para| {
                (p.role == ParaRole::Figure) == p.image.is_some()
                    && (p.role != ParaRole::Figure || p.text.0.is_empty())
            };
            if !body.iter().all(figure_ok) {
                return Err(BlockRefusal::Figure);
            }
        }
        if let Block::Heading {
            level,
            title: Some(t),
            ..
        } = &b
        {
            let bad = *level != HeadingLevel::Conversation
                || t.len() > MAX_TITLE_BYTES
                || t.chars().any(char::is_control);
            if bad {
                return Err(BlockRefusal::Title);
            }
        }
        Ok(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_round_trip_and_refuse_content_like_titles() {
        let o = PdfOptions {
            page: PageSize::Letter,
            han: HanFace::Traditional,
            title: "Messages export — conversation 1".into(),
        };
        assert_eq!(PdfOptions::decode(&o.encode()), Ok(o.clone()));
        let mut bad = o.encode();
        bad[0] = 9;
        assert_eq!(PdfOptions::decode(&bad), Err(OptionsRefusal::Version));
        assert_eq!(PdfOptions::decode(&[1, 1]), Err(OptionsRefusal::Malformed));
        assert_eq!(
            PdfOptions::decode(&[1, 3, 1]),
            Err(OptionsRefusal::Malformed)
        );
        let mut long = vec![1, 1, 1];
        long.extend(std::iter::repeat_n(b'a', MAX_TITLE_BYTES + 1));
        assert_eq!(PdfOptions::decode(&long), Err(OptionsRefusal::Title));
        assert_eq!(
            PdfOptions::decode(b"\x01\x01\x01a\nb"),
            Err(OptionsRefusal::Title)
        );
        assert_eq!(PageSize::A4.points().0.round(), 595.0);
    }

    #[test]
    fn recorded_text_is_marked_and_split_into_paragraphs() {
        let mut n = MarkCounts::default();
        let paras = Para::recorded(
            2,
            "pay\u{202E}gnp.exe\r\n⟨x\u{1B}\u{2029}last",
            true,
            &mut n,
        );
        let texts: Vec<&str> = paras.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, ["pay⟨U+202E⟩gnp.exe", "⟨U+27E8⟩x⟨U+001B⟩", "last"]);
        assert_eq!(
            (n.hidden_marked, n.controls_escaped, n.reserved_escaped),
            (1, 1, 1)
        );
        // Markers off: the true character stays (and the text layer
        // carries it); controls are still escaped.
        let mut n = MarkCounts::default();
        let paras = Para::recorded(9, "a\u{202E}b\u{7}", false, &mut n);
        assert_eq!(paras[0].text.as_str(), "a\u{202E}b⟨U+0007⟩");
        assert_eq!(paras[0].indent, MAX_INDENT);
        // A combining flood keeps its first 16 marks.
        let mut n = MarkCounts::default();
        let flood = format!("e{}", "\u{301}".repeat(10_000));
        let paras = Para::recorded(0, &flood, true, &mut n);
        assert!(paras[0].text.as_str().ends_with("⟨+9984 combining marks⟩"));
        assert_eq!(n.combining_runs_limited, 1);
    }

    #[test]
    fn header_fields_are_always_marked_and_ours_is_verbatim() {
        let (t, n) = TextBuilder::new()
            .ours("[")
            .header("Bob\n\u{202E}")
            .ours("] ⟨one-time code hidden⟩")
            .build();
        assert_eq!(t.as_str(), "[Bob⟨U+000A⟩⟨U+202E⟩] ⟨one-time code hidden⟩");
        assert_eq!((n.controls_escaped, n.hidden_marked), (1, 1));
        assert!(!format!("{t:?}").contains("Bob"));
    }

    #[test]
    fn blocks_round_trip_and_bounds_are_enforced() {
        let mut n = MarkCounts::default();
        let b = Block::Entry {
            head: vec![PdfText::ours("2024-03-10 14:02:11 +01:00")],
            body: Para::recorded(1, "hi", true, &mut n),
        };
        assert_eq!(Block::decode(&b.encode()), Ok(b));
        let h = Block::Heading {
            level: HeadingLevel::Day,
            text: PdfText::ours("2024-03-10"),
            title: None,
        };
        assert_eq!(Block::decode(&h.encode()), Ok(h));
        assert_eq!(Block::decode(b"{}"), Err(BlockRefusal::Malformed));
        assert_eq!(
            Block::decode(br#"{"kind":"heading","level":"day","text":"x","extra":1}"#),
            Err(BlockRefusal::Malformed)
        );
        assert_eq!(
            Block::decode(
                br#"{"kind":"entry","head":[],"body":[{"indent":4,"role":"text","text":"x"}]}"#
            ),
            Err(BlockRefusal::Indent)
        );
        // A figure needs its image and no text; a title only on a
        // conversation heading.
        let fig = Block::Entry {
            head: Vec::new(),
            body: vec![Para::figure(1, 7)],
        };
        assert_eq!(Block::decode(&fig.encode()), Ok(fig));
        assert_eq!(
            Block::decode(
                br#"{"kind":"entry","head":[],"body":[{"indent":1,"role":"figure","text":""}]}"#
            ),
            Err(BlockRefusal::Figure)
        );
        assert_eq!(
            Block::decode(
                br#"{"kind":"entry","head":[],"body":[{"indent":1,"role":"text","text":"x","image":3}]}"#
            ),
            Err(BlockRefusal::Figure)
        );
        assert_eq!(
            Block::decode(br#"{"kind":"heading","level":"day","text":"x","title":"t"}"#),
            Err(BlockRefusal::Title)
        );
        assert_eq!(
            Block::decode(
                br#"{"kind":"heading","level":"conversation","text":"x","title":"a\nb"}"#
            ),
            Err(BlockRefusal::Title)
        );
        let many = Block::Entry {
            head: vec![PdfText::ours("x"); MAX_HEAD_PIECES + 1],
            body: Vec::new(),
        };
        assert_eq!(
            Block::decode(&many.encode()),
            Err(BlockRefusal::TooManyPieces)
        );
    }
}
