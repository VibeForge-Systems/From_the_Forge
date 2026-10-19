//! Hidden characters, controls and the reserved marker (M6 spec §4.7,
//! D138, D121).
//!
//! **The classifier is D121's rule, inverted.** It marks exactly what
//! "Copy text without invisible characters" (`withoutHiddenCharacters` in
//! `src/services/copyText.ts`) drops, character for character:
//!
//! - bidi controls (U+061C, U+200E, U+200F, U+202A–U+202E,
//!   U+2066–U+2069);
//! - invisible characters (U+200B–U+200D, U+2060, U+FEFF, U+FFFC,
//!   U+E0000–U+E007F);
//! - a ZWJ or ZWNJ unless the character kept before it and the character
//!   after it are both non-ASCII letters, marks or emoji
//!   ([`crate::unicode_tables::JOINABLE`]);
//! - tag characters (U+E0020–U+E007F) unless the character kept before
//!   is U+1F3F4 (black flag) or another tag;
//! - variation-selector runs longer than 4;
//! - controls: C0 and C1 other than TAB, LF and CR, and DEL. The copy
//!   keeps CR; the export's text formats escape it as a control.
//!
//! The shared vectors in `tests/fixtures/export/hidden-vectors.json` are
//! checked against [`classify`] (`tests/hidden_vectors.rs`) and against
//! `withoutHiddenCharacters` (Vitest), so the two stay each other's
//! inverse.
//!
//! **One reserved marker form,** `⟨…⟩` (U+27E8 … U+27E9), for every
//! marker, escape and placeholder ([`mark`]):
//!
//! - hidden characters and controls: `⟨U+202E⟩`, `⟨U+001B⟩`;
//! - combining floods: the first 16 marks, then `⟨+N combining marks⟩`;
//! - a literal U+27E8 in content: `⟨U+27E8⟩`, so a sender cannot forge a
//!   marker (U+27E9 alone cannot open one).
//!
//! U+FFFC that the assembly resolved into a placeholder segment never
//! reaches this module (the segment has no text), so only an unresolved
//! one is marked.

use crate::unicode_tables::{JOINABLE, MARK};

/// The opening character of every marker.
pub const MARKER_OPEN: char = '\u{27E8}';
/// The closing character of every marker.
pub const MARKER_CLOSE: char = '\u{27E9}';
/// Longest run of combining marks kept on one base in TXT and PDF.
pub const COMBINING_RUN_LIMIT: usize = 16;
/// Longest run of variation selectors that is not hidden.
pub const VS_RUN_LIMIT: usize = 4;

const BLACK_FLAG: char = '\u{1F3F4}';

/// How one piece of a text is classed (the vector file's classes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CharClass {
    /// Not marked; kept by the copy.
    Keep,
    /// A carriage return: kept by the copy, escaped as a control by the
    /// export's text formats.
    Cr,
    Bidi,
    Invisible,
    /// A ZWJ or ZWNJ outside a joining context.
    Joiner,
    /// A tag character outside a flag's tag sequence.
    Tag,
    /// A run of more than [`VS_RUN_LIMIT`] variation selectors.
    VsRun,
    /// C0, C1 (other than TAB, LF and CR) and DEL.
    Control,
}

impl CharClass {
    /// The vector file's name for the class.
    pub fn name(self) -> &'static str {
        match self {
            CharClass::Keep => "keep",
            CharClass::Cr => "cr",
            CharClass::Bidi => "bidi",
            CharClass::Invisible => "invisible",
            CharClass::Joiner => "joiner",
            CharClass::Tag => "tag",
            CharClass::VsRun => "vsRun",
            CharClass::Control => "control",
        }
    }

    /// A hidden character: marked when markers are on, always in header
    /// fields.
    pub fn is_hidden(self) -> bool {
        matches!(
            self,
            CharClass::Bidi
                | CharClass::Invisible
                | CharClass::Joiner
                | CharClass::Tag
                | CharClass::VsRun
        )
    }

    /// A control: always escaped in the text formats.
    pub fn is_control(self) -> bool {
        matches!(self, CharClass::Control | CharClass::Cr)
    }
}

/// One piece of a classified text: a byte range and its class. `Keep`
/// pieces are maximal runs; a variation-selector run is one piece; every
/// other marked character is a piece of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub class: CharClass,
    pub start: usize,
    pub end: usize,
}

pub(crate) fn in_table(table: &[(u32, u32)], c: char) -> bool {
    let cp = u32::from(c);
    table
        .binary_search_by(|&(lo, hi)| {
            if hi < cp {
                std::cmp::Ordering::Less
            } else if lo > cp {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// A combining mark (`\p{M}`).
pub fn is_mark(c: char) -> bool {
    in_table(MARK, c)
}

fn joinable(c: Option<char>) -> bool {
    c.is_some_and(|c| u32::from(c) > 0x7F && in_table(JOINABLE, c))
}

/// The bidi controls (as `ContentText::bidi_controls` and `SafeText`).
pub fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// The invisible characters (variation selectors excepted).
pub fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}' | '\u{FFFC}' | '\u{E0000}'..='\u{E007F}'
    )
}

/// A variation selector (VS1–VS16, VS17–VS256).
pub fn is_variation_selector(c: char) -> bool {
    matches!(c, '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}')
}

fn is_tag(c: char) -> bool {
    matches!(c, '\u{E0020}'..='\u{E007F}')
}

/// C0 and C1 controls and DEL, other than TAB, LF and CR.
fn is_control(c: char) -> bool {
    let cp = u32::from(c);
    !matches!(c, '\t' | '\n' | '\r') && (cp < 0x20 || (0x7F..=0x9F).contains(&cp))
}

/// `text` split into classed pieces (the rule above, as
/// `withoutHiddenCharacters` applies it).
pub fn classify(text: &str) -> Vec<Piece> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let end_of = |i: usize| -> usize {
        chars
            .get(i)
            .map_or(text.len(), |&(at, c)| at + c.len_utf8())
    };
    let mut out: Vec<Piece> = Vec::new();
    let mut last_kept: Option<char> = None;
    let push = |out: &mut Vec<Piece>, class: CharClass, start: usize, end: usize| {
        if class == CharClass::Keep {
            if let Some(p) = out.last_mut() {
                if p.class == CharClass::Keep && p.end == start {
                    p.end = end;
                    return;
                }
            }
        }
        out.push(Piece { class, start, end });
    };
    let mut i = 0;
    while let Some(&(at, c)) = chars.get(i) {
        if is_variation_selector(c) {
            let mut j = i;
            while chars.get(j).is_some_and(|&(_, d)| is_variation_selector(d)) {
                j += 1;
            }
            let run_end = end_of(j - 1);
            if j - i <= VS_RUN_LIMIT {
                push(&mut out, CharClass::Keep, at, run_end);
                last_kept = chars.get(j - 1).map(|&(_, d)| d);
            } else {
                push(&mut out, CharClass::VsRun, at, run_end);
            }
            i = j;
            continue;
        }
        let class = if c == '\u{200C}' || c == '\u{200D}' {
            let next = chars.get(i + 1).map(|&(_, d)| d);
            if joinable(last_kept) && joinable(next) {
                CharClass::Keep
            } else {
                CharClass::Joiner
            }
        } else if is_tag(c) {
            if last_kept.is_some_and(|p| p == BLACK_FLAG || is_tag(p)) {
                CharClass::Keep
            } else {
                CharClass::Tag
            }
        } else if is_bidi_control(c) {
            CharClass::Bidi
        } else if is_invisible(c) {
            CharClass::Invisible
        } else if is_control(c) {
            CharClass::Control
        } else if c == '\r' {
            CharClass::Cr
        } else {
            CharClass::Keep
        };
        if matches!(class, CharClass::Keep | CharClass::Cr) {
            last_kept = Some(c);
        }
        push(&mut out, class, at, at + c.len_utf8());
        i += 1;
    }
    out
}

/// `text` without its hidden characters and controls (CR kept): what
/// "Copy text without invisible characters" copies.
pub fn without_hidden(text: &str) -> String {
    classify(text)
        .into_iter()
        .filter(|p| matches!(p.class, CharClass::Keep | CharClass::Cr))
        .filter_map(|p| text.get(p.start..p.end))
        .collect()
}

/// The distinct code points [`mark`] would mark (hidden characters with
/// markers on, and controls), sorted, as `U+XXXX`.
pub fn marked_code_points(text: &str) -> Vec<String> {
    let mut cps: Vec<u32> = classify(text)
        .into_iter()
        .filter(|p| p.class != CharClass::Keep)
        .filter_map(|p| text.get(p.start..p.end))
        .flat_map(|s| s.chars().map(u32::from).collect::<Vec<_>>())
        .collect();
    cps.sort_unstable();
    cps.dedup();
    cps.into_iter().map(code_point).collect()
}

/// `U+XXXX` (at least four hex digits, upper case).
pub fn code_point(cp: u32) -> String {
    format!("U+{cp:04X}")
}

/// `⟨U+XXXX⟩`.
pub fn marker(c: char) -> String {
    format!("{MARKER_OPEN}{}{MARKER_CLOSE}", code_point(u32::from(c)))
}

/// Which kind of field a value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldKind {
    /// Message text: TAB and LF stay text.
    Body,
    /// A sender, conversation name, file name or qualifier: every control
    /// and line break (LF, CR, U+2028, U+2029, TAB) is marked, never a new
    /// line, and hidden characters are always marked.
    Header,
}

/// What [`mark`] applies (spec §4.7's table, per format).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MarkPolicy {
    pub field: FieldKind,
    /// Mark hidden characters in body text (the "hidden-character
    /// markers" option, on by default). Header fields always mark them.
    pub hidden: bool,
    /// Escape controls as markers (TXT, PDF, CSV "for spreadsheets").
    pub controls: bool,
    /// Escape a literal U+27E8.
    pub reserved: bool,
    /// Limit combining runs to [`COMBINING_RUN_LIMIT`] (TXT, PDF).
    pub limit_combining: bool,
}

impl MarkPolicy {
    /// TXT and PDF body text.
    pub const fn text_body(hidden_markers: bool) -> MarkPolicy {
        MarkPolicy {
            field: FieldKind::Body,
            hidden: hidden_markers,
            controls: true,
            reserved: true,
            limit_combining: true,
        }
    }

    /// TXT and PDF header fields.
    pub const fn text_header() -> MarkPolicy {
        MarkPolicy {
            field: FieldKind::Header,
            hidden: true,
            controls: true,
            reserved: true,
            limit_combining: true,
        }
    }
}

/// What [`mark`] changed, for the manifest's counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct MarkCounts {
    /// Hidden characters replaced by a marker.
    pub hidden_marked: u64,
    /// Controls (and, in header fields, line breaks) replaced by a marker.
    pub controls_escaped: u64,
    /// Combining runs cut to the limit.
    pub combining_runs_limited: u64,
    /// Literal U+27E8 characters escaped.
    pub reserved_escaped: u64,
}

impl MarkCounts {
    pub fn add(&mut self, o: MarkCounts) {
        self.hidden_marked += o.hidden_marked;
        self.controls_escaped += o.controls_escaped;
        self.combining_runs_limited += o.combining_runs_limited;
        self.reserved_escaped += o.reserved_escaped;
    }
}

fn header_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\t' | '\u{2028}' | '\u{2029}')
}

/// `text` with markers per `policy`, and what was marked.
pub fn mark(text: &str, policy: MarkPolicy) -> (String, MarkCounts) {
    let mut out = String::with_capacity(text.len());
    let mut n = MarkCounts::default();
    let mut run = 0usize;
    let mut dropped = 0usize;
    let flush = |out: &mut String, dropped: &mut usize, n: &mut MarkCounts| {
        if *dropped > 0 {
            out.push_str(&format!(
                "{MARKER_OPEN}+{} combining marks{MARKER_CLOSE}",
                *dropped
            ));
            n.combining_runs_limited += 1;
            *dropped = 0;
        }
    };
    let hidden = policy.hidden || policy.field == FieldKind::Header;
    for p in classify(text) {
        let s = text.get(p.start..p.end).unwrap_or("");
        let marks = (hidden && p.class.is_hidden()) || (policy.controls && p.class.is_control());
        if marks {
            flush(&mut out, &mut dropped, &mut n);
            run = 0;
            for c in s.chars() {
                out.push_str(&marker(c));
                if p.class.is_hidden() {
                    n.hidden_marked += 1;
                } else {
                    n.controls_escaped += 1;
                }
            }
            continue;
        }
        for c in s.chars() {
            if policy.limit_combining && is_mark(c) {
                run += 1;
                if run > COMBINING_RUN_LIMIT {
                    dropped += 1;
                    continue;
                }
            } else {
                flush(&mut out, &mut dropped, &mut n);
                run = 0;
            }
            if policy.field == FieldKind::Header && header_break(c) {
                out.push_str(&marker(c));
                n.controls_escaped += 1;
            } else if policy.reserved && c == MARKER_OPEN {
                out.push_str(&marker(c));
                n.reserved_escaped += 1;
            } else {
                out.push(c);
            }
        }
    }
    flush(&mut out, &mut dropped, &mut n);
    (out, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(text: &str) -> Vec<&'static str> {
        classify(text).into_iter().map(|p| p.class.name()).collect()
    }

    #[test]
    fn the_rule_is_the_copy_rule_inverted() {
        assert_eq!(classes("pay\u{202E}gnp.exe"), ["keep", "bidi", "keep"]);
        // A ZWJ between two emoji is kept; between ASCII letters it is not.
        assert_eq!(classes("\u{1F469}\u{200D}\u{1F4BB}"), ["keep"]);
        assert_eq!(classes("a\u{200D}b"), ["keep", "joiner", "keep"]);
        // Flag tags stay inside the flag only.
        assert_eq!(classes("\u{1F3F4}\u{E0067}\u{E0062}\u{E007F}"), ["keep"]);
        assert_eq!(classes("x\u{E0041}"), ["keep", "tag"]);
        // Four variation selectors are kept, five are a run.
        assert_eq!(classes(&format!("a{}", "\u{FE0F}".repeat(4))), ["keep"]);
        assert_eq!(
            classes(&format!("a{}b", "\u{FE0F}".repeat(5))),
            ["keep", "vsRun", "keep"]
        );
        assert_eq!(classes("a\r\nb\u{1B}"), ["keep", "cr", "keep", "control"]);
        assert_eq!(without_hidden("a\u{200B}b\u{7F}\r"), "ab\r");
    }

    #[test]
    fn markers_use_the_reserved_form() {
        let (t, n) = mark("a\u{202E}b\u{1B}", MarkPolicy::text_body(true));
        assert_eq!(t, "a⟨U+202E⟩b⟨U+001B⟩");
        assert_eq!((n.hidden_marked, n.controls_escaped), (1, 1));
        // Markers off: hidden characters stay, controls are still escaped.
        let (t, _) = mark("a\u{202E}b\u{1B}", MarkPolicy::text_body(false));
        assert_eq!(t, "a\u{202E}b⟨U+001B⟩");
        // A literal ⟨ cannot forge a marker; ⟩ alone opens nothing.
        let (t, n) = mark("⟨one-time code hidden⟩", MarkPolicy::text_body(true));
        assert_eq!(t, "⟨U+27E8⟩one-time code hidden⟩");
        assert_eq!(n.reserved_escaped, 1);
        // A backslash is never ambiguous.
        assert_eq!(
            mark("¯\\_(ツ)_/¯", MarkPolicy::text_body(true)).0,
            "¯\\_(ツ)_/¯"
        );
    }

    #[test]
    fn header_fields_always_mark_and_never_break_a_line() {
        let (t, n) = mark(
            "Bob\nDate: x\u{2028}\u{202E}",
            MarkPolicy {
                hidden: false,
                ..MarkPolicy::text_header()
            },
        );
        assert_eq!(t, "Bob⟨U+000A⟩Date: x⟨U+2028⟩⟨U+202E⟩");
        assert_eq!((n.controls_escaped, n.hidden_marked), (2, 1));
        assert!(!t.contains('\n'));
        // Body text keeps TAB and LF but escapes CR and NEL.
        let (t, _) = mark("a\tb\nc\rd\u{85}", MarkPolicy::text_body(true));
        assert_eq!(t, "a\tb\nc⟨U+000D⟩d⟨U+0085⟩");
    }

    #[test]
    fn combining_runs_are_limited_and_counted() {
        let flood = format!("e{}x", "\u{0301}".repeat(40));
        let (t, n) = mark(&flood, MarkPolicy::text_body(true));
        assert_eq!(
            t,
            format!("e{}⟨+24 combining marks⟩x", "\u{0301}".repeat(16))
        );
        assert_eq!(n.combining_runs_limited, 1);
        // Exactly 16 is not limited; a run at the end is flushed.
        let (t, n) = mark(
            &format!("e{}", "\u{0301}".repeat(16)),
            MarkPolicy::text_body(true),
        );
        assert_eq!((t.chars().count(), n.combining_runs_limited), (17, 0));
        let (t, n) = mark(
            &format!("e{}", "\u{0301}".repeat(17)),
            MarkPolicy::text_body(true),
        );
        assert!(t.ends_with("⟨+1 combining marks⟩"));
        assert_eq!(n.combining_runs_limited, 1);
        // JSON-like policy keeps every mark.
        let keep = MarkPolicy {
            limit_combining: false,
            ..MarkPolicy::text_body(true)
        };
        assert_eq!(mark(&flood, keep).0, flood);
    }

    #[test]
    fn tables_are_sorted_and_disjoint() {
        for t in [JOINABLE, MARK] {
            for w in t.windows(2) {
                if let [a, b] = w {
                    assert!(a.0 <= a.1 && a.1 < b.0);
                }
            }
        }
        assert!(is_mark('\u{0301}') && is_mark('\u{FE0F}') && !is_mark('e'));
        assert!(joinable(Some('\u{0915}')) && !joinable(Some('a')) && !joinable(None));
    }
}
