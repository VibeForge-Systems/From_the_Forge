//! Content text as it leaves the crate (M5 spec §3.8): lossy decoding,
//! the value state, caps, the bidi and invisible-character facts, URL runs
//! and masking.
//!
//! - Every content string is an [`OptContent`]: the text, or the reason
//!   there is none ([`ValueState`]), so "empty", "NULL", "column absent",
//!   "too large to store" and "unreadable" stay distinct.
//! - Raw BLOB columns (addresses, chat names, identifiers) are decoded
//!   lossily, with [`ContentText::lossy`] set when a byte was replaced.
//! - Caps cut at a char boundary, so never inside a surrogate pair, and
//!   set [`ContentText::truncated`]; [`ContentText::full_utf16`] is the
//!   uncut length.
//! - The facts are computed over the whole value, not just the part that
//!   was kept. **Stored data is never changed**: only the copy that leaves
//!   the crate is cut.
//!
//! Text is held as `Redacted<String>`, so `Debug` prints only a length.

use oma_messages::Redacted;
use rusqlite::types::ValueRef;

use crate::schema::codes::ValueState;

/// UTF-16 units of a body in a thread page.
pub const BODY_PAGE_UTF16: u32 = 8192;
/// UTF-16 units of a list preview.
pub const PREVIEW_UTF16: u32 = 512;
/// UTF-16 units of an address, a chat name or a file name.
pub const IDENTIFIER_UTF16: u32 = 256;

/// One content string, cut to its cap, with facts about the whole value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ContentText {
    /// The text, cut at a char boundary to the cap. Never log it.
    pub text: Redacted<String>,
    /// A byte that is not UTF-8 was replaced with U+FFFD when decoding.
    pub lossy: bool,
    /// SQLite reported the source value as a BLOB in a text column
    /// (`value_issue` kind 3).
    pub stored_as_blob: bool,
    /// The text was cut to its cap; the UI shows an explicit marker.
    pub truncated: bool,
    /// The length of the whole value in UTF-16 units.
    pub full_utf16: u32,
    /// The whole value holds a bidirectional control (U+061C, U+200E,
    /// U+200F, U+202A–U+202E, U+2066–U+2069).
    pub bidi_controls: bool,
    /// The whole value holds an invisible character (U+200B–U+200D,
    /// U+2060, U+FEFF, U+FFFC, a tag character U+E0000–U+E007F) or a run of
    /// more than four variation selectors.
    pub invisible: bool,
}

/// A content value, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum OptContent {
    /// The value (possibly empty).
    Text(ContentText),
    /// No value: NULL in the source, the column absent from it, too large
    /// to store, or unreadable.
    Missing(ValueState),
}

impl OptContent {
    /// The text, when there is a value.
    pub fn text(&self) -> Option<&ContentText> {
        match self {
            OptContent::Text(t) => Some(t),
            OptContent::Missing(_) => None,
        }
    }
}

/// `true` for the Unicode bidirectional formatting characters.
pub(crate) fn is_bidi_control(c: char) -> bool {
    crate::id::is_bidi_control(c)
}

/// `true` for the characters that render as nothing (§3.8), variation
/// selectors excepted (only a run of them counts).
pub(crate) fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}' | '\u{FFFC}' | '\u{E0000}'..='\u{E007F}'
    )
}

/// `true` for a variation selector (VS1–VS16, VS17–VS256).
fn is_variation_selector(c: char) -> bool {
    matches!(c, '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}')
}

/// The most variation selectors in a row that do not count as invisible.
const VS_RUN_ALLOWED: usize = 4;

/// The two facts of a whole value: (bidi controls, invisible characters).
pub(crate) fn facts(s: &str) -> (bool, bool) {
    let mut bidi = false;
    let mut invisible = false;
    let mut vs_run = 0usize;
    for c in s.chars() {
        bidi |= is_bidi_control(c);
        invisible |= is_invisible(c);
        if is_variation_selector(c) {
            vs_run += 1;
            invisible |= vs_run > VS_RUN_ALLOWED;
        } else {
            vs_run = 0;
        }
    }
    (bidi, invisible)
}

/// UTF-16 units of `s`, saturating at `u32::MAX`.
pub(crate) fn utf16_len(s: &str) -> u32 {
    u32::try_from(s.encode_utf16().count()).unwrap_or(u32::MAX)
}

/// The longest prefix of `s` of at most `cap` UTF-16 units that ends at a
/// char boundary, and whether that cut anything.
pub(crate) fn cut_utf16(s: &str, cap: u32) -> (&str, bool) {
    let mut units = 0u32;
    for (i, c) in s.char_indices() {
        let w = u32::try_from(c.len_utf16()).unwrap_or(2);
        if units.saturating_add(w) > cap {
            return (s.get(..i).unwrap_or(""), true);
        }
        units += w;
    }
    (s, false)
}

impl ContentText {
    /// `s`, cut to `cap` UTF-16 units, with its facts.
    pub(crate) fn from_str(s: &str, cap: u32) -> ContentText {
        let (kept, truncated) = cut_utf16(s, cap);
        let (bidi_controls, invisible) = facts(s);
        ContentText {
            text: Redacted::new(kept.to_owned()),
            lossy: false,
            stored_as_blob: false,
            truncated,
            full_utf16: utf16_len(s),
            bidi_controls,
            invisible,
        }
    }

    /// Raw bytes decoded lossily (U+FFFD for each invalid sequence), cut
    /// to `cap` UTF-16 units.
    pub(crate) fn from_bytes(b: &[u8], cap: u32) -> ContentText {
        let decoded = String::from_utf8_lossy(b);
        let lossy = matches!(decoded, std::borrow::Cow::Owned(_));
        let mut t = ContentText::from_str(&decoded, cap);
        t.lossy = lossy;
        t
    }

    /// Mark the source value as stored as a BLOB.
    pub(crate) fn stored_as_blob(mut self, yes: bool) -> ContentText {
        self.stored_as_blob = yes;
        self
    }

    /// Mark the text as lossily decoded (for example a body taken from the
    /// text column with replacement characters, `display_source = 3`).
    pub(crate) fn lossy(mut self, yes: bool) -> ContentText {
        self.lossy |= yes;
        self
    }
}

impl OptContent {
    /// A stored `x`/`x_st` pair: the value (TEXT or BLOB, decoded lossily
    /// and cut to `cap`) when `x_st` is NULL, else the state `x_st` names.
    /// A cell that breaks the pair's CHECK, or an unknown state, is
    /// [`ValueState::Unreadable`].
    pub(crate) fn from_cell(value: ValueRef<'_>, st: Option<i64>, cap: u32) -> OptContent {
        if let Some(code) = st {
            return OptContent::Missing(
                ValueState::from_code(code).unwrap_or(ValueState::Unreadable),
            );
        }
        match value {
            ValueRef::Text(b) | ValueRef::Blob(b) => {
                OptContent::Text(ContentText::from_bytes(b, cap))
            }
            ValueRef::Null | ValueRef::Integer(_) | ValueRef::Real(_) => {
                OptContent::Missing(ValueState::Unreadable)
            }
        }
    }
}

/// A run of text that looks like a URL (§3.6): it begins `http://` or
/// `https://` (ASCII, any case) and continues to the next whitespace. A
/// clean-room rule, not a URL parser; URLs are never made clickable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct UrlRun {
    /// The first UTF-16 unit of the run.
    pub start_utf16: u32,
    /// One past the last UTF-16 unit of the run.
    pub end_utf16: u32,
    /// The run holds a bidi control or an invisible character, so what it
    /// shows is not what it says ("Copy link" warns, §9.3).
    pub hidden: bool,
}

fn starts_url(rest: &str) -> bool {
    let b = rest.as_bytes();
    let has = |p: &[u8]| b.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p));
    has(b"http://") || has(b"https://")
}

/// Every URL run in `text`, in order.
pub(crate) fn url_runs(text: &str) -> Vec<UrlRun> {
    let mut out = Vec::new();
    let mut units = 0u32;
    let mut current: Option<UrlRun> = None;
    for (i, c) in text.char_indices() {
        let w = u32::try_from(c.len_utf16()).unwrap_or(2);
        match current.as_mut() {
            Some(run) if c.is_whitespace() => {
                run.end_utf16 = units;
                out.push(*run);
                current = None;
            }
            Some(run) => {
                run.hidden |= is_bidi_control(c) || is_invisible(c) || is_variation_selector(c);
            }
            None if starts_url(text.get(i..).unwrap_or("")) => {
                current = Some(UrlRun {
                    start_utf16: units,
                    end_utf16: units,
                    hidden: false,
                });
            }
            None => {}
        }
        units = units.saturating_add(w);
    }
    if let Some(mut run) = current {
        run.end_utf16 = units;
        out.push(run);
    }
    out
}

/// Why a piece of text is withheld (D115).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MaskKind {
    /// A one-time code, shown only after "Show code".
    OneTimeCode,
}

/// A piece of a masked text: shown text, or a withheld range.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TextPiece {
    Text(ContentText),
    Masked(MaskKind),
}

/// `text` split into pieces, with every char that overlaps one of the
/// half-open UTF-16 `ranges` withheld as `kind` (a range that splits a
/// surrogate pair masks the whole char: masking widens, never narrows).
/// The shown pieces share one budget of `cap` UTF-16 units; the piece
/// that reaches it is cut and marked truncated, and nothing follows it.
pub(crate) fn mask_utf16(
    text: &str,
    ranges: &[(u32, u32)],
    kind: MaskKind,
    cap: u32,
) -> Vec<TextPiece> {
    let masked = |start: u32, end: u32| ranges.iter().any(|&(s, e)| start < e && s < end);
    let mut spans: Vec<(bool, usize, usize)> = Vec::new();
    let mut units = 0u32;
    for (i, c) in text.char_indices() {
        let w = u32::try_from(c.len_utf16()).unwrap_or(2);
        let m = masked(units, units.saturating_add(w));
        let end = i + c.len_utf8();
        match spans.last_mut() {
            Some(last) if last.0 == m => last.2 = end,
            _ => spans.push((m, i, end)),
        }
        units = units.saturating_add(w);
    }
    let mut out = Vec::with_capacity(spans.len());
    let mut left = cap;
    for (m, a, b) in spans {
        if m {
            out.push(TextPiece::Masked(kind));
            continue;
        }
        let piece = text.get(a..b).unwrap_or("");
        let t = ContentText::from_str(piece, left);
        left = left.saturating_sub(utf16_len(piece));
        let stop = t.truncated;
        out.push(TextPiece::Text(t));
        if stop {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(t: &ContentText) -> &str {
        t.text.expose()
    }

    #[test]
    fn each_state_is_missing_with_its_state() {
        for st in ValueState::ALL {
            assert_eq!(
                OptContent::from_cell(ValueRef::Null, Some(st.code()), 10),
                OptContent::Missing(*st)
            );
        }
        // An unknown state, and a cell that breaks the pair's CHECK.
        assert_eq!(
            OptContent::from_cell(ValueRef::Null, Some(9), 10),
            OptContent::Missing(ValueState::Unreadable)
        );
        assert_eq!(
            OptContent::from_cell(ValueRef::Null, None, 10),
            OptContent::Missing(ValueState::Unreadable)
        );
        assert_eq!(
            OptContent::from_cell(ValueRef::Integer(3), None, 10),
            OptContent::Missing(ValueState::Unreadable)
        );
        // Empty is a value, distinct from NULL.
        let empty = OptContent::from_cell(ValueRef::Blob(b""), None, 10);
        assert_eq!(empty.text().map(|t| t.full_utf16), Some(0));
    }

    #[test]
    fn lossy_decoding_sets_lossy_and_keeps_the_input() {
        let raw = [b'a', 0xff, b'b'];
        let t = ContentText::from_bytes(&raw, 10);
        assert!(t.lossy);
        assert_eq!(shown(&t), "a\u{FFFD}b");
        assert_eq!(raw, [b'a', 0xff, b'b'], "stored bytes are unchanged");
        let ok = ContentText::from_bytes("héllo".as_bytes(), 10);
        assert!(!ok.lossy);
        assert!(ContentText::from_str("x", 4).lossy(true).lossy);
        assert!(
            ContentText::from_str("x", 4)
                .stored_as_blob(true)
                .stored_as_blob
        );
    }

    #[test]
    fn caps_cut_at_char_boundaries_and_never_split_a_surrogate_pair() {
        // U+1F600 is two UTF-16 units.
        let s = "ab\u{1F600}cd";
        let t = ContentText::from_str(s, 3);
        assert_eq!(shown(&t), "ab");
        assert!(t.truncated);
        assert_eq!(t.full_utf16, 6);
        let t = ContentText::from_str(s, 4);
        assert_eq!(shown(&t), "ab\u{1F600}");
        let t = ContentText::from_str(s, 6);
        assert_eq!(shown(&t), s);
        assert!(!t.truncated);
        let t = ContentText::from_str("é", 0);
        assert_eq!((shown(&t), t.truncated), ("", true));
        // A long identifier: cut to 256 units and marked.
        let long = "x".repeat(300);
        let t = ContentText::from_str(&long, IDENTIFIER_UTF16);
        assert_eq!(t.text.expose().len(), 256);
        assert!(t.truncated);
        assert_eq!(t.full_utf16, 300);
    }

    #[test]
    fn every_listed_code_point_sets_its_fact() {
        let bidi = [
            '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}',
            '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ];
        for c in bidi {
            let t = ContentText::from_str(&format!("a{c}b"), 10);
            assert!(t.bidi_controls && !t.invisible, "U+{:04X}", u32::from(c));
        }
        let invisible = [
            '\u{200B}',
            '\u{200C}',
            '\u{200D}',
            '\u{2060}',
            '\u{FEFF}',
            '\u{FFFC}',
            '\u{E0000}',
            '\u{E0041}',
            '\u{E007F}',
        ];
        for c in invisible {
            let t = ContentText::from_str(&format!("a{c}b"), 10);
            assert!(t.invisible && !t.bidi_controls, "U+{:04X}", u32::from(c));
        }
        // Four variation selectors in a row are allowed, five are not.
        let four = format!("\u{2764}{}", "\u{FE0F}".repeat(4));
        assert!(!ContentText::from_str(&four, 99).invisible);
        let five = format!("\u{2764}{}", "\u{FE0F}".repeat(5));
        assert!(ContentText::from_str(&five, 99).invisible);
        let split = "\u{FE0F}\u{FE0F}\u{FE0F}x\u{FE0F}\u{FE0F}\u{FE0F}";
        assert!(!ContentText::from_str(split, 99).invisible);
        // Facts cover the whole value, past the cut.
        let late = format!("{}\u{202E}", "a".repeat(20));
        let t = ContentText::from_str(&late, 5);
        assert!(t.truncated && t.bidi_controls);
        // Plain text; a ZWJ sequence sets the fact (U+200D is listed), and
        // the UI keeps such characters inside the isolate (§8.1).
        assert_eq!(facts("hello"), (false, false));
        assert_eq!(facts("\u{1F468}\u{200D}\u{1F469}"), (false, true));
    }

    #[test]
    fn url_runs_and_their_hidden_flag() {
        let runs = url_runs("see https://a.example/x and HTTP://b.example.");
        assert_eq!(runs.len(), 2);
        assert_eq!((runs[0].start_utf16, runs[0].end_utf16), (4, 23));
        assert_eq!((runs[1].start_utf16, runs[1].end_utf16), (28, 45));
        assert!(!runs[0].hidden && !runs[1].hidden);
        // Offsets are UTF-16 units: an astral char before counts twice.
        let runs = url_runs("\u{1F600} http://x\u{202E}y\tz");
        assert_eq!(runs.len(), 1);
        assert_eq!((runs[0].start_utf16, runs[0].end_utf16), (3, 13));
        assert!(runs[0].hidden);
        let runs = url_runs("http://x\u{200B}y");
        assert!(runs[0].hidden);
        // Not a scheme this rule knows, or not a URL at all.
        assert!(url_runs("ftp://x mailto:y http:/z https//w").is_empty());
        assert!(url_runs("").is_empty());
        // Back to back without whitespace: one run.
        assert_eq!(url_runs("http://a/http://b").len(), 1);
    }

    #[test]
    fn masking_withholds_one_time_codes_and_widens_over_surrogates() {
        let text = "Code 123456 now";
        let pieces = mask_utf16(text, &[(5, 11)], MaskKind::OneTimeCode, 100);
        assert_eq!(pieces.len(), 3);
        let TextPiece::Text(a) = &pieces[0] else {
            panic!("text")
        };
        assert_eq!(shown(a), "Code ");
        assert_eq!(pieces[1], TextPiece::Masked(MaskKind::OneTimeCode));
        let TextPiece::Text(b) = &pieces[2] else {
            panic!("text")
        };
        assert_eq!(shown(b), " now");
        let joined: String = pieces
            .iter()
            .filter_map(|p| match p {
                TextPiece::Text(t) => Some(t.text.expose().as_str()),
                TextPiece::Masked(_) => None,
            })
            .collect();
        assert!(!joined.contains("123456"));
        // A range that covers half of a surrogate pair masks the char.
        let pieces = mask_utf16("a\u{1F600}b", &[(2, 3)], MaskKind::OneTimeCode, 100);
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[1], TextPiece::Masked(MaskKind::OneTimeCode));
        // Everything masked; nothing masked.
        assert_eq!(
            mask_utf16("abc", &[(0, 3)], MaskKind::OneTimeCode, 100),
            vec![TextPiece::Masked(MaskKind::OneTimeCode)]
        );
        assert_eq!(mask_utf16("abc", &[], MaskKind::OneTimeCode, 100).len(), 1);
        // The cap is shared: the piece that reaches it is cut, and nothing
        // follows.
        let pieces = mask_utf16("abcd 99 efgh", &[(5, 7)], MaskKind::OneTimeCode, 3);
        assert_eq!(pieces.len(), 1);
        let TextPiece::Text(t) = &pieces[0] else {
            panic!("text")
        };
        assert_eq!((shown(t), t.truncated), ("abc", true));
    }

    #[test]
    fn debug_prints_no_text() {
        let t = ContentText::from_str("SYNTH-SECRET", 99);
        let shown = format!("{t:?} {:?}", OptContent::Text(t.clone()));
        assert!(!shown.contains("SYNTH"), "{shown}");
    }
}
