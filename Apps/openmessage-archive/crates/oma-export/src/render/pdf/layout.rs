//! Paragraph layout with parley: shaping, bidi, line breaking and font
//! fallback (M6 spec §5.6).
//!
//! - **One paragraph per call.** Its base direction is its first strong
//!   character's (parley resolves the paragraph level from the text when
//!   none is given), and it is aligned to its start, so an RTL paragraph
//!   is right-aligned. A header piece is laid out the same way, alone, so
//!   each piece is its own bidi isolate: an RTL sender cannot reorder a
//!   qualifier next to it.
//! - **Never clip.** Breaks fall at line-break opportunities (UAX #14,
//!   with the dictionary segmenters for Thai and other scripts without
//!   spaces, parley's `complex-scripts`), and an unbroken token wider
//!   than the line breaks anywhere (`OverflowWrap::Anywhere`); each such
//!   line is counted as a forced wrap.
//! - **Font runs** follow [`super::library::StackKind`]: CJK runs by
//!   their script (kana → Japanese, hangul → Korean, other Han → the
//!   export's Han face), VS16 emoji first, VS15 text only.
//! - **`.notdef`.** A cluster no bundled font can draw is shaped with
//!   glyph 0; its characters are counted. The glyphs keep the cluster's
//!   text range, so the text layer still carries the true characters.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

use parley::{
    Alignment, AlignmentOptions, BreakReason, FontFamily, LayoutContext, LineHeight, OverflowWrap,
    PositionedLayoutItem, StyleProperty,
};

use super::library::{Library, StackKind};

/// The line height, as a multiple of the font size.
pub const LINE_HEIGHT: f32 = 1.3;

/// One glyph, in krilla's units (fractions of the font size).
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedGlyph {
    pub id: u32,
    pub x_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
    /// The cluster's byte range in the paragraph's text.
    pub range: Range<usize>,
}

/// A run of glyphs in one font.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedRun {
    pub slot: usize,
    pub size: f32,
    /// The pen position of the first glyph, from the line's left edge.
    pub x: f32,
    /// The run's total advance.
    pub advance: f32,
    pub glyphs: Vec<PlacedGlyph>,
}

/// One laid-out line.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedLine {
    pub runs: Vec<PlacedRun>,
    pub height: f32,
    /// The baseline, from the line's top.
    pub baseline: f32,
    /// The ink advance (without trailing whitespace).
    pub advance: f32,
    pub forced: bool,
}

/// A laid-out paragraph. `Debug` prints counts only, never the text.
#[derive(Clone)]
pub struct Paragraph {
    /// The text every glyph's range points into (the drawn text, and the
    /// text layer).
    pub text: Arc<str>,
    pub lines: Vec<PlacedLine>,
    pub rtl: bool,
    /// Characters drawn as `.notdef`.
    pub notdef_chars: u64,
    /// Lines broken at an arbitrary point.
    pub forced_wraps: u64,
}

impl std::fmt::Debug for Paragraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Paragraph")
            .field("len", &self.text.len())
            .field("lines", &self.lines.len())
            .field("rtl", &self.rtl)
            .field("notdef_chars", &self.notdef_chars)
            .field("forced_wraps", &self.forced_wraps)
            .finish()
    }
}

/// Lays out paragraphs with the session's fonts.
pub struct Typesetter {
    pub fonts: Library,
    lcx: LayoutContext<()>,
}

impl std::fmt::Debug for Typesetter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Typesetter")
            .field("fonts", &self.fonts)
            .finish_non_exhaustive()
    }
}

fn in_ranges(c: char, ranges: &[(u32, u32)]) -> bool {
    let c = u32::from(c);
    ranges.iter().any(|(a, b)| *a <= c && c <= *b)
}

/// Hiragana, Katakana and their extensions, and halfwidth Katakana.
const KANA: &[(u32, u32)] = &[
    (0x3040, 0x30FF),
    (0x31F0, 0x31FF),
    (0xFF65, 0xFF9F),
    (0x1AFF0, 0x1B16F),
];
/// Hangul Jamo, compatibility Jamo, syllables and halfwidth forms.
const HANGUL: &[(u32, u32)] = &[
    (0x1100, 0x11FF),
    (0x3130, 0x318F),
    (0xA960, 0xA97F),
    (0xAC00, 0xD7FF),
    (0xFFA0, 0xFFDC),
];
/// Han, CJK radicals, punctuation, symbols, compatibility and fullwidth
/// forms: the characters a CJK run is made of (with kana and hangul).
const CJK_OTHER: &[(u32, u32)] = &[
    (0x2E80, 0x2FDF),
    (0x3000, 0x303F),
    (0x3190, 0x31EF),
    (0x3200, 0x33FF),
    (0x3400, 0x4DBF),
    (0x4E00, 0x9FFF),
    (0xF900, 0xFAFF),
    (0xFE30, 0xFE4F),
    (0xFF00, 0xFF64),
    (0xFFE0, 0xFFEF),
    (0x20000, 0x3FFFF),
];

const VS15: char = '\u{FE0E}';
const VS16: char = '\u{FE0F}';
const ZWJ: char = '\u{200D}';

fn is_cjk(c: char) -> bool {
    in_ranges(c, KANA) || in_ranges(c, HANGUL) || in_ranges(c, CJK_OTHER)
}

fn is_emoji_tail(c: char) -> bool {
    // Keycap, skin tones, tags.
    c == '\u{20E3}'
        || ('\u{1F3FB}'..='\u{1F3FF}').contains(&c)
        || ('\u{E0020}'..='\u{E007F}').contains(&c)
}

/// The non-default stacks of `text`, as byte ranges in order.
pub fn stack_ranges(text: &str) -> Vec<(Range<usize>, StackKind)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut kinds = vec![StackKind::Default; chars.len()];
    // CJK runs, decided per run.
    let mut i = 0;
    while i < chars.len() {
        if !chars.get(i).is_some_and(|(_, c)| is_cjk(*c)) {
            i += 1;
            continue;
        }
        let start = i;
        while chars.get(i).is_some_and(|(_, c)| is_cjk(*c)) {
            i += 1;
        }
        let run = chars.get(start..i).unwrap_or(&[]);
        let kind = if run.iter().any(|(_, c)| in_ranges(*c, KANA)) {
            StackKind::Japanese
        } else if run.iter().any(|(_, c)| in_ranges(*c, HANGUL)) {
            StackKind::Korean
        } else {
            StackKind::Chinese
        };
        for k in kinds.get_mut(start..i).unwrap_or(&mut []) {
            *k = kind;
        }
    }
    // Variation selectors: the character before, and its sequence.
    for (at, (_, c)) in chars.iter().enumerate() {
        if *c != VS15 && *c != VS16 {
            continue;
        }
        let Some(base) = at.checked_sub(1) else {
            continue;
        };
        let kind = if *c == VS16 {
            StackKind::EmojiFirst
        } else {
            StackKind::TextOnly
        };
        let mut start = base;
        // Back over a ZWJ sequence this character ends.
        while start >= 2 && chars.get(start - 1).is_some_and(|(_, p)| *p == ZWJ) {
            start -= 2;
        }
        let mut end = at + 1;
        if kind == StackKind::EmojiFirst {
            loop {
                match chars.get(end) {
                    Some((_, n)) if is_emoji_tail(*n) || *n == VS16 => end += 1,
                    Some((_, n)) if *n == ZWJ && end + 1 < chars.len() => end += 2,
                    _ => break,
                }
            }
        }
        for k in kinds
            .get_mut(start..end.min(chars.len()))
            .unwrap_or(&mut [])
        {
            *k = kind;
        }
    }
    let mut out: Vec<(Range<usize>, StackKind)> = Vec::new();
    for ((at, c), kind) in chars.iter().zip(kinds) {
        let end = at + c.len_utf8();
        match out.last_mut() {
            Some((r, k)) if *k == kind && r.end == *at => r.end = end,
            _ => out.push((*at..end, kind)),
        }
    }
    out.retain(|(_, k)| *k != StackKind::Default);
    out
}

impl Typesetter {
    pub fn new(fonts: Library) -> Typesetter {
        Typesetter {
            fonts,
            lcx: LayoutContext::new(),
        }
    }

    /// Lay out `text` as one paragraph `width` points wide, at `size`
    /// points.
    pub fn paragraph(&mut self, text: &str, width: f32, size: f32) -> Paragraph {
        let ranges = stack_ranges(text);
        let default = self.fonts.stack(StackKind::Default).to_vec();
        let stacks: Vec<(Range<usize>, Vec<_>)> = ranges
            .into_iter()
            .map(|(r, k)| (r, self.fonts.stack(k).to_vec()))
            .collect();
        let mut builder = self
            .lcx
            .ranged_builder(&mut self.fonts.cx, text, 1.0, false);
        builder.push_default(StyleProperty::FontFamily(FontFamily::List(Cow::Borrowed(
            &default,
        ))));
        builder.push_default(StyleProperty::FontSize(size));
        builder.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(
            LINE_HEIGHT,
        )));
        builder.push_default(StyleProperty::OverflowWrap(OverflowWrap::Anywhere));
        for (r, stack) in &stacks {
            builder.push(
                StyleProperty::FontFamily(FontFamily::List(Cow::Borrowed(stack))),
                r.clone(),
            );
        }
        let mut layout = builder.build(text);
        layout.break_all_lines(Some(width.max(1.0)));
        layout.align(Alignment::Start, AlignmentOptions::default());

        // The characters drawn as `.notdef`, by byte offset.
        let mut missing = std::collections::BTreeSet::new();
        let mut forced = 0u64;
        let mut lines = Vec::new();
        for line in layout.lines() {
            let m = line.metrics();
            let top = m.block_min_coord;
            let is_forced = line.break_reason() == BreakReason::Emergency;
            if is_forced {
                forced += 1;
            }
            let mut runs = Vec::new();
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(gr) = item else {
                    continue;
                };
                let run = gr.run();
                let Some(slot) = self.fonts.slot_of(run.font()) else {
                    continue;
                };
                let size = run.font_size();
                if size <= 0.0 {
                    continue;
                }
                let mut glyphs: Vec<PlacedGlyph> = Vec::new();
                // Where the last cluster's glyphs start in `glyphs`.
                let mut last_cluster = 0usize;
                for cluster in run.visual_clusters() {
                    let range = cluster.text_range();
                    if cluster.is_ligature_continuation() {
                        // The ligature's glyphs cover this cluster's text
                        // too: every glyph of the cluster keeps one range.
                        for g in glyphs.get_mut(last_cluster..).unwrap_or(&mut []) {
                            g.range.start = g.range.start.min(range.start);
                            g.range.end = g.range.end.max(range.end);
                        }
                        continue;
                    }
                    last_cluster = glyphs.len();
                    for g in cluster.glyphs() {
                        glyphs.push(PlacedGlyph {
                            id: g.id,
                            x_advance: g.advance / size,
                            x_offset: g.x / size,
                            y_offset: g.y / size,
                            range: range.clone(),
                        });
                    }
                }
                for g in glyphs.iter().filter(|g| g.id == 0) {
                    let at = g.range.start;
                    if let Some(s) = text.get(g.range.clone()) {
                        missing.extend(s.char_indices().map(|(i, _)| at + i));
                    }
                }
                if !glyphs.is_empty() {
                    runs.push(PlacedRun {
                        slot,
                        size,
                        x: gr.offset(),
                        advance: gr.advance(),
                        glyphs,
                    });
                }
            }
            lines.push(PlacedLine {
                runs,
                height: m.block_max_coord - top,
                baseline: m.baseline - top,
                advance: m.advance - m.trailing_whitespace,
                forced: is_forced,
            });
        }
        Paragraph {
            text: Arc::from(text),
            lines,
            rtl: layout.is_rtl(),
            notdef_chars: u64::try_from(missing.len()).unwrap_or(u64::MAX),
            forced_wraps: forced,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(&str, StackKind)> {
        stack_ranges(text)
            .into_iter()
            .map(|(r, k)| (&text[r], k))
            .collect()
    }

    #[test]
    fn cjk_runs_take_their_face_from_their_script() {
        assert_eq!(kinds("abc"), []);
        assert_eq!(
            kinds("中文 東京は晴れ 한국어"),
            [
                ("中文", StackKind::Chinese),
                ("東京は晴れ", StackKind::Japanese),
                ("한국어", StackKind::Korean)
            ]
        );
        // Kana anywhere in a run makes the whole run Japanese.
        assert_eq!(kinds("漢字です。"), [("漢字です。", StackKind::Japanese)]);
    }

    #[test]
    fn variation_selectors_pick_the_emoji_or_the_text_face() {
        assert_eq!(
            kinds("a\u{2764}\u{FE0F}b"),
            [("\u{2764}\u{FE0F}", StackKind::EmojiFirst)]
        );
        assert_eq!(
            kinds("\u{2708}\u{FE0E}"),
            [("\u{2708}\u{FE0E}", StackKind::TextOnly)]
        );
        // A keycap and a ZWJ sequence stay whole.
        assert_eq!(
            kinds("1\u{FE0F}\u{20E3}"),
            [("1\u{FE0F}\u{20E3}", StackKind::EmojiFirst)]
        );
        let rainbow = "\u{1F3F3}\u{FE0F}\u{200D}\u{1F308}";
        assert_eq!(kinds(rainbow), [(rainbow, StackKind::EmojiFirst)]);
        let couple = "\u{1F9D1}\u{200D}\u{2764}\u{FE0F}\u{200D}\u{1F9D1}";
        assert_eq!(kinds(couple), [(couple, StackKind::EmojiFirst)]);
        // A selector with nothing before it changes nothing.
        assert_eq!(kinds("\u{FE0F}x"), []);
        // VS16 on a CJK symbol wins over the CJK run.
        assert_eq!(
            kinds("\u{303D}\u{FE0F}"),
            [("\u{303D}\u{FE0F}", StackKind::EmojiFirst)]
        );
    }
}
