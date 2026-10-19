//! Safe highlighting (M5 spec §4.5).
//!
//! A naive `highlight()` is unsafe: a body that already holds the marker
//! character gives ambiguous output, and FTS5 returns NUL-terminated text,
//! so a body with an embedded NUL comes back silently cut. So:
//!
//! 1. A body holding U+FDD0, U+FDD1 or NUL is never highlighted.
//! 2. `highlight()` runs with the noncharacters U+FDD0 and U+FDD1 as
//!    markers, bound as parameters, on the base table, for the page's
//!    hits only, under `ViewBudget::Hits` (the caller).
//! 3. [`parse_marked`] splits the output into hit ranges and strips the
//!    markers, and **checks that what is left equals the stored text**; on
//!    any mismatch the hit is sent unhighlighted.
//! 4. One-time codes are masked over the result ([`build`]): a hit that
//!    lies only inside a withheld code is reported as hidden, never shown.
//! 5. The snippet, at most [`SNIPPET_CHARS`] characters around the first
//!    visible hit, is cut in Rust with `cut_start`/`cut_end` flags; no
//!    ellipsis is inserted into the text.
//!
//! Exact-text hits are found by the same substring search the scan uses
//! ([`find_all`]). Text crosses as [`ContentText`] pieces, so `Debug`
//! prints lengths only.

use crate::view::{ContentText, MaskKind};

/// The marker before a hit (a Unicode noncharacter).
pub(crate) const MARK_OPEN: char = '\u{FDD0}';
/// The marker after a hit.
pub(crate) const MARK_CLOSE: char = '\u{FDD1}';
/// The most characters (code points) of a snippet.
pub const SNIPPET_CHARS: usize = 200;
/// Characters shown before the first visible hit, when there are enough.
const SNIPPET_LEAD: usize = 60;

/// A piece of a hit's snippet.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HitPiece {
    /// Shown text; `hit` marks a match (the UI wraps it in `<mark>`).
    Text { text: ContentText, hit: bool },
    /// Withheld text (a one-time code, D115), matched or not.
    Masked(MaskKind),
}

/// Why a hit's text is shown without highlights.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NoHighlight {
    /// The body holds U+FDD0, U+FDD1 or NUL, so `highlight()` is not run.
    MarkerInText,
    /// `highlight()`'s output, markers stripped, was not the stored text.
    Mismatch,
    /// The page's highlight budget or deadline ran out before this hit.
    Budget,
    /// The match could not be found again in the stored text (it changed
    /// since the search, for example through a newer decoder).
    NotFound,
}

/// Whether a hit's matches are marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Highlight {
    /// The pieces mark every match inside the snippet.
    Shown,
    /// The body matched, but no match is marked.
    NotShown(NoHighlight),
}

/// A hit's snippet.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HitText {
    /// The snippet, in order; one-time codes withheld.
    pub pieces: Vec<HitPiece>,
    /// Text before the snippet was left out (the UI draws an ellipsis).
    pub cut_start: bool,
    /// Text after the snippet was left out.
    pub cut_end: bool,
    pub highlight: Highlight,
    /// Every match lies inside withheld text: "Matched hidden text
    /// (one-time code)". The code is not shown.
    pub matched_hidden: bool,
    /// The whole body's length in UTF-16 units.
    pub full_utf16: u32,
}

/// `true` when `highlight()` may run on `text`.
pub(crate) fn highlightable(text: &str) -> bool {
    !text.contains([MARK_OPEN, MARK_CLOSE, '\0'])
}

/// The byte ranges of the marked hits in `marked`, over the text with the
/// markers stripped, when that text equals `original` and the markers
/// pair up; `None` otherwise.
pub(crate) fn parse_marked(marked: &str, original: &str) -> Option<Vec<(usize, usize)>> {
    let mut stripped = String::with_capacity(original.len());
    let mut ranges = Vec::new();
    let mut open: Option<usize> = None;
    for c in marked.chars() {
        match (c, open) {
            (MARK_OPEN, None) => open = Some(stripped.len()),
            (MARK_CLOSE, Some(start)) => {
                if stripped.len() > start {
                    ranges.push((start, stripped.len()));
                }
                open = None;
            }
            (MARK_OPEN | MARK_CLOSE, _) => return None,
            _ => stripped.push(c),
        }
    }
    (open.is_none() && stripped == original).then_some(ranges)
}

/// Every non-overlapping occurrence of `needle` in `text`, as byte ranges
/// (the scan's substring search: case-, accent- and
/// normalisation-sensitive).
pub(crate) fn find_all(text: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    text.match_indices(needle)
        .map(|(i, m)| (i, i + m.len()))
        .collect()
}

/// Overlapping or touching UTF-16 ranges merged, in order.
fn merged(ranges: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut v: Vec<(u32, u32)> = ranges.iter().copied().filter(|(s, e)| s < e).collect();
    v.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::with_capacity(v.len());
    for (s, e) in v {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// One character of a walk: its index, byte offset, whether it is
/// withheld, and the match it belongs to.
struct Ch {
    k: usize,
    byte: usize,
    masked: bool,
    hit: Option<usize>,
}

/// Walk `text` once, in linear time: `hits` are sorted, non-overlapping
/// byte ranges, `masks` sorted, disjoint UTF-16 ranges. Returns the
/// characters and UTF-16 units of the whole text.
fn walk(
    text: &str,
    hits: &[(usize, usize)],
    masks: &[(u32, u32)],
    mask_all: bool,
    mut f: impl FnMut(Ch),
) -> (usize, u32) {
    let mut units = 0u32;
    let mut hi = 0usize;
    let mut mi = 0usize;
    let mut n = 0usize;
    for (k, (i, c)) in text.char_indices().enumerate() {
        let w = u32::try_from(c.len_utf16()).unwrap_or(2);
        let end = units.saturating_add(w);
        while masks.get(mi).is_some_and(|&(_, e)| e <= units) {
            mi += 1;
        }
        let masked = mask_all || masks.get(mi).is_some_and(|&(s, _)| s < end);
        while hits.get(hi).is_some_and(|&(_, e)| e <= i) {
            hi += 1;
        }
        let hit = hits.get(hi).and_then(|&(s, _)| (s <= i).then_some(hi));
        f(Ch {
            k,
            byte: i,
            masked,
            hit,
        });
        units = end;
        n = k + 1;
    }
    (n, units)
}

/// The snippet of `text` with the byte ranges `hits` (sorted,
/// non-overlapping) marked and the UTF-16 ranges `masked` withheld
/// (`mask_all`: the whole text withheld, because a code's ranges cannot be
/// trusted). `lossy` is carried to the pieces. Linear in the text.
pub(crate) fn build(
    text: &str,
    hits: &[(usize, usize)],
    masked: &[(u32, u32)],
    mask_all: bool,
    lossy: bool,
    highlight: Highlight,
) -> HitText {
    let masks = merged(masked);
    // Pass 1: which matches are visible, and where the first one starts.
    let mut visible = vec![false; hits.len()];
    let mut first_visible: Option<usize> = None;
    let (n, units) = walk(text, hits, &masks, mask_all, |c| {
        if let (Some(h), false) = (c.hit, c.masked) {
            if let Some(v) = visible.get_mut(h) {
                *v = true;
            }
            first_visible.get_or_insert(c.k);
        }
    });
    let matched_hidden = !hits.is_empty() && !visible.iter().any(|v| *v);
    let mut start = first_visible.map_or(0, |f| f.saturating_sub(SNIPPET_LEAD));
    let end = (start + SNIPPET_CHARS).min(n);
    if end - start < SNIPPET_CHARS {
        start = end.saturating_sub(SNIPPET_CHARS);
    }
    // Pass 2: the pieces of the window, as runs of (withheld, match).
    let mut runs: Vec<(bool, bool, usize, usize)> = Vec::new();
    walk(text, hits, &masks, mask_all, |c| {
        if c.k < start || c.k >= end {
            return;
        }
        let h = c.hit.is_some() && !c.masked;
        match runs.last_mut() {
            Some(last) if last.0 == c.masked && (c.masked || last.1 == h) => {}
            Some(last) => {
                last.3 = c.byte;
                runs.push((c.masked, h, c.byte, text.len()));
            }
            None => runs.push((c.masked, h, c.byte, text.len())),
        }
        if c.k + 1 == end {
            if let Some(last) = runs.last_mut() {
                last.3 = c.byte
                    + text
                        .get(c.byte..)
                        .and_then(|r| r.chars().next())
                        .map_or(0, char::len_utf8);
            }
        }
    });
    let pieces = runs
        .into_iter()
        .map(|(m, h, a, b)| {
            if m {
                HitPiece::Masked(MaskKind::OneTimeCode)
            } else {
                HitPiece::Text {
                    text: ContentText::from_str(text.get(a..b).unwrap_or(""), u32::MAX)
                        .lossy(lossy),
                    hit: h,
                }
            }
        })
        .collect();
    HitText {
        pieces,
        cut_start: start > 0,
        cut_end: end < n,
        highlight,
        matched_hidden,
        full_utf16: units,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(t: &HitText) -> String {
        t.pieces
            .iter()
            .map(|p| match p {
                HitPiece::Text { text, hit: true } => format!("[{}]", text.text.expose()),
                HitPiece::Text { text, hit: false } => text.text.expose().clone(),
                HitPiece::Masked(_) => "#".to_owned(),
            })
            .collect()
    }

    #[test]
    fn markers_in_the_text_and_nul_are_never_highlighted() {
        assert!(highlightable("plain text"));
        assert!(!highlightable("a\u{FDD0}b"));
        assert!(!highlightable("a\u{FDD1}b"));
        assert!(!highlightable("a\0b"));
    }

    #[test]
    fn marked_output_parses_only_when_it_reconstructs_the_text() {
        let orig = "say hello to hello";
        let marked = "say \u{FDD0}hello\u{FDD1} to \u{FDD0}hello\u{FDD1}";
        assert_eq!(parse_marked(marked, orig), Some(vec![(4, 9), (13, 18)]));
        // No hit at all: an empty list, still a reconstruction.
        assert_eq!(parse_marked(orig, orig), Some(vec![]));
        // NUL truncation (what FTS5's C string would give): a mismatch.
        assert_eq!(parse_marked("say \u{FDD0}hello\u{FDD1}", orig), None);
        // Unpaired or nested markers.
        assert_eq!(parse_marked("say \u{FDD0}hello to hello", orig), None);
        assert_eq!(parse_marked("say hello\u{FDD1} to hello", orig), None);
        assert_eq!(
            parse_marked("say \u{FDD0}\u{FDD0}hello\u{FDD1} to hello", orig),
            None
        );
        // Empty marks are dropped.
        assert_eq!(
            parse_marked("say \u{FDD0}\u{FDD1}hello to hello", orig),
            Some(vec![])
        );
    }

    #[test]
    fn exact_matches_are_case_and_normalisation_sensitive() {
        assert_eq!(find_all("aXa xa", "xa"), vec![(4, 6)]);
        assert_eq!(find_all("aaaa", "aa"), vec![(0, 2), (2, 4)]);
        // NFC needle, NFD text: no match.
        assert!(find_all("cafe\u{301}", "café").is_empty());
        assert_eq!(find_all("café", "café"), vec![(0, 5)]);
        assert!(find_all("x", "").is_empty());
    }

    #[test]
    fn snippets_mark_hits_and_cut_around_the_first_visible_one() {
        let t = build("say hello", &[(4, 9)], &[], false, false, Highlight::Shown);
        assert_eq!(shown(&t), "say [hello]");
        assert!(!t.cut_start && !t.cut_end && !t.matched_hidden);
        assert_eq!(t.full_utf16, 9);
        // A long body: the window starts 60 characters before the hit.
        let text = format!("{}needle{}", "a".repeat(500), "b".repeat(500));
        let t = build(&text, &[(500, 506)], &[], false, false, Highlight::Shown);
        let s = shown(&t);
        assert!(t.cut_start && t.cut_end);
        assert_eq!(s.chars().filter(|c| *c != '[' && *c != ']').count(), 200);
        assert!(
            s.starts_with(&"a".repeat(60)) && s.contains("[needle]"),
            "{s}"
        );
        // Near the end the window slides back to keep 200 characters.
        let text = format!("{}needle", "a".repeat(500));
        let t = build(&text, &[(500, 506)], &[], false, false, Highlight::Shown);
        assert!(t.cut_start && !t.cut_end);
        assert!(shown(&t).ends_with("[needle]"));
        // No hit: the start of the text.
        let t = build(
            &"z".repeat(300),
            &[],
            &[],
            false,
            false,
            Highlight::NotShown(NoHighlight::Budget),
        );
        assert!(!t.cut_start && t.cut_end);
        assert_eq!(t.highlight, Highlight::NotShown(NoHighlight::Budget));
    }

    #[test]
    fn hits_inside_one_time_codes_are_hidden_and_never_shown() {
        // "Code 123456 now" with the code at UTF-16 5..11.
        let text = "Code 123456 now";
        let t = build(text, &[(5, 11)], &[(5, 11)], false, false, Highlight::Shown);
        assert_eq!(shown(&t), "Code # now");
        assert!(t.matched_hidden);
        assert!(!format!("{t:?}").contains("123456"));
        // A visible hit elsewhere: not hidden, and the code stays masked.
        let t = build(
            text,
            &[(0, 4), (5, 11)],
            &[(5, 11)],
            false,
            false,
            Highlight::Shown,
        );
        assert_eq!(shown(&t), "[Code] # now");
        assert!(!t.matched_hidden);
        // A hit straddling the code shows only its visible part.
        let t = build(text, &[(3, 8)], &[(5, 11)], false, false, Highlight::Shown);
        assert_eq!(shown(&t), "Cod[e ]# now");
        // Untrusted ranges: everything withheld.
        let t = build(text, &[(0, 4)], &[], true, false, Highlight::Shown);
        assert_eq!(shown(&t), "#");
        assert!(t.matched_hidden);
    }

    #[test]
    fn a_highlight_inside_a_bidi_override_stays_in_one_piece() {
        let text = "x \u{202E}abc hello def\u{202C} y";
        let start = text.find("hello").expect("hello");
        let t = build(
            text,
            &[(start, start + 5)],
            &[],
            false,
            false,
            Highlight::Shown,
        );
        let hits: Vec<&HitPiece> = t
            .pieces
            .iter()
            .filter(|p| matches!(p, HitPiece::Text { hit: true, .. }))
            .collect();
        assert_eq!(hits.len(), 1);
        let HitPiece::Text { text: piece, .. } = hits[0] else {
            panic!("text")
        };
        assert_eq!(piece.text.expose(), "hello");
        // The pieces put together are the text.
        let joined: String = t
            .pieces
            .iter()
            .map(|p| match p {
                HitPiece::Text { text, .. } => text.text.expose().clone(),
                HitPiece::Masked(_) => String::new(),
            })
            .collect();
        assert_eq!(joined, text);
        assert!(t
            .pieces
            .iter()
            .any(|p| matches!(p, HitPiece::Text { text, .. } if text.bidi_controls)));
    }

    #[test]
    fn utf16_offsets_count_surrogate_pairs() {
        // The mask range is in UTF-16 units: the emoji counts two.
        let text = "\u{1F600}1234 ok";
        let t = build(text, &[], &[(2, 6)], false, false, Highlight::Shown);
        assert_eq!(shown(&t), "\u{1F600}# ok");
        assert_eq!(t.full_utf16, 9);
    }
}
