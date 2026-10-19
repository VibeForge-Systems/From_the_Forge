//! The search query grammar (M5 spec §4.1, §4.2). User text never reaches
//! FTS5's `MATCH` directly: Rust parses it into terms and builds the FTS5
//! expression itself, bound as a parameter.
//!
//! - `query := term (WS term)*`, `term := word | "quoted phrase"`. A quote
//!   opens a phrase only at the start of a term; an unterminated phrase
//!   runs to the end of the query. Inside a word a `"` is an ordinary
//!   character.
//! - **Refused** (`InvalidQuery`): more than [`QUERY_BYTES`] bytes,
//!   [`QUERY_TERMS`] terms or [`TERM_BYTES`] bytes per term, and any NUL or
//!   C0 control character (whether FTS5's parser reads the query as a C
//!   string is UNVERIFIED, so none is ever passed).
//! - **Built:** each term becomes an FTS5 string with its `"` doubled;
//!   terms are joined with ` AND `; the last unquoted term gets ` *` only
//!   when its last token has at least [`PREFIX_MIN_CHARS`] characters. A
//!   term with no token character (Unicode letters, numbers or private
//!   use) is dropped; no term left is `InvalidQuery`. No other FTS5 syntax
//!   is reachable: `OR`, `NOT`, `NEAR`, `^`, `*` and column filters are
//!   all quoted text.
//! - **Exact text:** the whole query, trimmed, is **one** needle of at most
//!   [`NEEDLE_CHARS`] code points.
//! - **Mode:** a query with an emoji or a character of the CJK, Hiragana,
//!   Katakana, Hangul, Thai, Lao, Khmer, Myanmar or Tibetan ranges (this
//!   module's own approximate table) runs as exact text; any other as
//!   words. The caller may ask for exact text always.

use crate::error::{ArchiveError, QueryProblem};

/// The longest query, in UTF-8 bytes.
pub const QUERY_BYTES: usize = 512;
/// The most terms in a words query.
pub const QUERY_TERMS: usize = 16;
/// The longest term, in UTF-8 bytes.
pub const TERM_BYTES: usize = 128;
/// The longest exact-text needle, in code points.
pub const NEEDLE_CHARS: usize = 64;
/// The shortest last token that gets a prefix `*`, in characters.
pub const PREFIX_MIN_CHARS: usize = 3;

/// One parsed term.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Term {
    pub(crate) text: String,
    pub(crate) quoted: bool,
}

impl std::fmt::Debug for Term {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Query text is content (I6): its length only.
        write!(
            f,
            "Term({} bytes, quoted: {})",
            self.text.len(),
            self.quoted
        )
    }
}

/// `true` for the characters FTS5's unicode61 tokenizer keeps in tokens:
/// letters and numbers (an approximation through `is_alphanumeric`, which
/// also counts some combining marks) and private-use characters.
pub(crate) fn is_token_char(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(
            c,
            '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}'
        )
}

/// `true` for NUL and the C0 control characters.
fn is_c0(c: char) -> bool {
    c <= '\u{1F}'
}

/// The characters of the last token of `s` (a run of token characters).
fn last_token_chars(s: &str) -> usize {
    s.chars()
        .rev()
        .skip_while(|c| !is_token_char(*c))
        .take_while(|c| is_token_char(*c))
        .count()
}

fn check_common(raw: &str) -> Result<(), ArchiveError> {
    if raw.len() > QUERY_BYTES {
        return Err(ArchiveError::InvalidQuery(QueryProblem::TooLong));
    }
    if raw.chars().any(is_c0) {
        return Err(ArchiveError::InvalidQuery(QueryProblem::ControlCharacter));
    }
    Ok(())
}

/// The terms of `raw`, every limit checked, terms without a token
/// character dropped.
pub(crate) fn terms(raw: &str) -> Result<Vec<Term>, ArchiveError> {
    check_common(raw)?;
    let mut out = Vec::new();
    let mut chars = raw.char_indices().peekable();
    let mut parsed = 0usize;
    loop {
        while chars.next_if(|(_, c)| c.is_whitespace()).is_some() {}
        let Some(&(start, first)) = chars.peek() else {
            break;
        };
        let term = if first == '"' {
            chars.next();
            let from = start + 1;
            let mut end = raw.len();
            for (i, c) in chars.by_ref() {
                if c == '"' {
                    end = i;
                    break;
                }
            }
            Term {
                text: raw.get(from..end).unwrap_or("").to_owned(),
                quoted: true,
            }
        } else {
            let mut end = raw.len();
            while let Some(&(i, c)) = chars.peek() {
                if c.is_whitespace() {
                    end = i;
                    break;
                }
                chars.next();
            }
            Term {
                text: raw.get(start..end).unwrap_or("").to_owned(),
                quoted: false,
            }
        };
        parsed += 1;
        if parsed > QUERY_TERMS {
            return Err(ArchiveError::InvalidQuery(QueryProblem::TooManyTerms));
        }
        if term.text.len() > TERM_BYTES {
            return Err(ArchiveError::InvalidQuery(QueryProblem::TermTooLong));
        }
        if term.text.chars().any(is_token_char) {
            out.push(term);
        }
    }
    if out.is_empty() {
        return Err(ArchiveError::InvalidQuery(QueryProblem::NoTerms));
    }
    Ok(out)
}

/// The FTS5 expression for `terms`: quoted strings joined with ` AND `,
/// the last unquoted term a prefix when its last token is long enough.
/// Also whether it has a prefix.
pub(crate) fn fts_expression(terms: &[Term]) -> (String, bool) {
    let parts = term_expressions(terms);
    let prefix = parts.last().is_some_and(|p| p.ends_with(" *"));
    (parts.join(" AND "), prefix)
}

/// Each term's own FTS5 expression, in order: [`fts_expression`] is these
/// joined with ` AND `. An export re-checks a hit term by term against
/// the text it writes (M6 spec §3.6).
pub(crate) fn term_expressions(terms: &[Term]) -> Vec<String> {
    terms
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut out = String::with_capacity(t.text.len() + 4);
            out.push('"');
            out.push_str(&t.text.replace('"', "\"\""));
            out.push('"');
            if i + 1 == terms.len() && !t.quoted && last_token_chars(&t.text) >= PREFIX_MIN_CHARS {
                out.push_str(" *");
            }
            out
        })
        .collect()
}

/// The exact-text needle of `raw`: the whole query, trimmed.
pub(crate) fn needle(raw: &str) -> Result<String, ArchiveError> {
    check_common(raw)?;
    let n = raw.trim();
    if n.is_empty() {
        return Err(ArchiveError::InvalidQuery(QueryProblem::NoTerms));
    }
    if n.chars().count() > NEEDLE_CHARS {
        return Err(ArchiveError::InvalidQuery(QueryProblem::NeedleTooLong));
    }
    Ok(n.to_owned())
}

/// `true` for a character that sends an automatic query to exact text: an
/// emoji, or a script unicode61 does not split into words (our own
/// approximate range table, §4.2).
pub(crate) fn needs_exact(c: char) -> bool {
    matches!(
        u32::from(c),
        // Thai, Lao, Tibetan, Myanmar, Hangul Jamo, Khmer
        0x0E00..=0x0FFF
            | 0x1000..=0x109F
            | 0x1100..=0x11FF
            | 0x1780..=0x17FF
            | 0x19E0..=0x19FF
            // CJK radicals, symbols, kana, Bopomofo, Hangul compatibility
            // Jamo, CJK strokes, enclosed and compatibility forms,
            // extension A, unified ideographs
            | 0x2E80..=0x2FDF
            | 0x3000..=0x9FFF
            // Myanmar extensions, Hangul Jamo extension A, Hangul syllables
            // and Jamo extension B
            | 0xA960..=0xA97F
            | 0xA9E0..=0xA9FF
            | 0xAA60..=0xAA7F
            | 0xAC00..=0xD7FF
            // CJK compatibility ideographs, half-width kana and Hangul
            | 0xF900..=0xFAFF
            | 0xFF66..=0xFFDC
            // Kana supplements, ideographs beyond the BMP
            | 0x1AFF0..=0x1B16F
            | 0x20000..=0x3FFFF
            // Emoji and pictographs, regional indicators, symbols,
            // dingbats, technical symbols and arrows that are emoji,
            // variation selector 16, ZWJ, keycap, tags
            | 0x1F000..=0x1FAFF
            | 0x2300..=0x23FF
            | 0x2600..=0x27BF
            | 0x2B00..=0x2BFF
            | 0xFE0F
            | 0x200D
            | 0x20E3
            | 0xE0020..=0xE007F
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built(raw: &str) -> Result<String, QueryProblem> {
        match terms(raw) {
            Ok(t) => Ok(fts_expression(&t).0),
            Err(ArchiveError::InvalidQuery(p)) => Err(p),
            Err(e) => panic!("{e:?}"),
        }
    }

    #[test]
    fn fts5_syntax_is_always_quoted_text() {
        let cases = [
            ("AND", r#""AND" *"#),
            ("OR", r#""OR""#),
            ("NOT x", r#""NOT" AND "x""#),
            ("NEAR(a b)", r#""NEAR(a" AND "b)""#),
            ("x:hi", r#""x:hi""#),
            ("hi\"", r#""hi""""#),
            ("^start", r#""^start" *"#),
            ("-minus", r#""-minus" *"#),
            ("+plus", r#""+plus" *"#),
            ("(paren)", r#""(paren)" *"#),
            ("star*", r#""star*" *"#),
            ("\"\"\"hi\"\"\"", r#""hi""#),
            ("say \"two words\"", r#""say" AND "two words""#),
            ("\"open phrase", r#""open phrase""#),
        ];
        for (raw, want) in cases {
            assert_eq!(built(raw).as_deref(), Ok(want), "{raw}");
        }
        // Terms with no token character are dropped; none left is invalid.
        for raw in ["\"", "\"\"", "*", "^", "-", "+", "()", ":", "  ", ""] {
            assert_eq!(built(raw), Err(QueryProblem::NoTerms), "{raw:?}");
        }
        assert_eq!(built("* hello").as_deref(), Ok(r#""hello" *"#));
    }

    #[test]
    fn prefixes_need_three_characters_in_the_last_unquoted_term() {
        assert_eq!(built("ab").as_deref(), Ok(r#""ab""#));
        assert_eq!(built("abc").as_deref(), Ok(r#""abc" *"#));
        assert_eq!(built("abc de").as_deref(), Ok(r#""abc" AND "de""#));
        assert_eq!(built("de abc").as_deref(), Ok(r#""de" AND "abc" *"#));
        assert_eq!(built("\"abc\"").as_deref(), Ok(r#""abc""#));
        // The last token counts, not the term: `x-ab` ends in a 2-char token.
        assert_eq!(built("x-ab").as_deref(), Ok(r#""x-ab""#));
        assert_eq!(built("ab-cde").as_deref(), Ok(r#""ab-cde" *"#));
        assert_eq!(built("abc.").as_deref(), Ok(r#""abc." *"#));
        // Characters, not bytes.
        assert_eq!(built("éé").as_deref(), Ok(r#""éé""#));
    }

    #[test]
    fn limits_and_control_characters_are_refused() {
        let sixteen = vec!["ab"; 16].join(" ");
        assert!(built(&sixteen).is_ok());
        let seventeen = vec!["ab"; 17].join(" ");
        assert_eq!(built(&seventeen), Err(QueryProblem::TooManyTerms));
        assert!(built(&"a".repeat(128)).is_ok());
        assert_eq!(built(&"a".repeat(129)), Err(QueryProblem::TermTooLong));
        let long = vec!["a".repeat(100); 6].join(" ");
        assert!(long.len() > 512);
        assert_eq!(built(&long), Err(QueryProblem::TooLong));
        let fits = vec!["a".repeat(127); 4].join(" ");
        assert_eq!(fits.len(), 511);
        assert!(built(&fits).is_ok());
        let over = vec!["a".repeat(128); 4].join(" ");
        assert_eq!(built(&over), Err(QueryProblem::TooLong));
        for c in 0u32..0x20 {
            let ch = char::from_u32(c).expect("c0");
            let raw = format!("ab{ch}cd");
            assert_eq!(
                built(&raw),
                Err(QueryProblem::ControlCharacter),
                "U+{c:04X}"
            );
            assert!(matches!(
                needle(&raw),
                Err(ArchiveError::InvalidQuery(QueryProblem::ControlCharacter))
            ));
        }
        // DEL and C1 controls are not C0.
        assert!(built("ab\u{7F}cd").is_ok());
    }

    #[test]
    fn the_needle_is_the_whole_trimmed_query() {
        assert_eq!(needle("  two words ").expect("needle"), "two words");
        assert!(matches!(
            needle("   "),
            Err(ArchiveError::InvalidQuery(QueryProblem::NoTerms))
        ));
        assert!(needle(&"字".repeat(64)).is_ok());
        assert!(matches!(
            needle(&"字".repeat(65)),
            Err(ArchiveError::InvalidQuery(QueryProblem::NeedleTooLong))
        ));
        // Quotes and syntax are literal in a needle.
        assert_eq!(needle("\"a OR b\"").expect("needle"), "\"a OR b\"");
    }

    #[test]
    fn the_mode_table() {
        for s in [
            "東京",
            "ひらがな",
            "カタカナ",
            "한국어",
            "ภาษาไทย",
            "ພາສາລາວ",
            "ខ្មែរ",
            "မြန်မာ",
            "བོད",
            "\u{1F600}",
            "\u{2764}\u{FE0F}",
            "\u{1F1EF}\u{1F1F5}",
            "\u{20000}",
        ] {
            assert!(s.chars().any(needs_exact), "{s}");
        }
        for s in [
            "hello",
            "café",
            "straße",
            "Ελληνικά",
            "русский",
            "עברית",
            "العربية",
            "123",
        ] {
            assert!(!s.chars().any(needs_exact), "{s}");
        }
    }

    #[test]
    fn debug_prints_no_term_text() {
        let t = terms("SYNTH-SECRET").expect("terms");
        assert!(!format!("{t:?}").contains("SYNTH"));
    }
}
