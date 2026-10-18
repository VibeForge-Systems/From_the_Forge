//! Strings from a backup, made safe to display.

use std::fmt;

/// Maximum characters kept, including the trailing ellipsis when truncated.
pub const DISPLAY_TEXT_MAX_CHARS: usize = 128;

/// A string from a backup made safe to display: at most 128 characters, with
/// control characters and bidi controls (U+202A–202E, U+2066–2069,
/// U+200E/F, U+061C) removed and a trailing "…" when truncated.
///
/// `Debug` prints the length only, so an accidental `?value` in a log line
/// cannot leak a device name.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct DisplayText(String);

fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

impl DisplayText {
    /// Sanitise and bound an untrusted string.
    pub fn from_untrusted(s: &str) -> Self {
        let mut kept = s
            .chars()
            .filter(|c| !c.is_control() && !is_bidi_control(*c));
        let mut out = String::new();
        let mut count = 0usize;
        while let Some(c) = kept.next() {
            if count == DISPLAY_TEXT_MAX_CHARS.saturating_sub(1) {
                // One slot left: keep this character only if it is the last one.
                if kept.next().is_some() {
                    out.push('…');
                } else {
                    out.push(c);
                }
                return DisplayText(out);
            }
            out.push(c);
            count = count.saturating_add(1);
        }
        DisplayText(out)
    }

    /// The sanitised text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DisplayText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DisplayText(<{} chars>)", self.0.chars().count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_text_is_unchanged() {
        assert_eq!(
            DisplayText::from_untrusted("Synthetic Test Phone").as_str(),
            "Synthetic Test Phone"
        );
        assert_eq!(DisplayText::from_untrusted("").as_str(), "");
    }

    #[test]
    fn control_and_bidi_characters_are_removed() {
        let t =
            DisplayText::from_untrusted("a\u{202E}b\u{2066}c\u{200F}d\u{061C}e\n\tf\u{0}g\u{7f}");
        assert_eq!(t.as_str(), "abcdefg");
    }

    #[test]
    fn long_text_is_truncated_to_128_chars_with_an_ellipsis() {
        let exact = "é".repeat(128);
        assert_eq!(DisplayText::from_untrusted(&exact).as_str(), exact);
        let long = "é".repeat(500);
        let t = DisplayText::from_untrusted(&long);
        assert_eq!(t.as_str().chars().count(), 128);
        assert!(t.as_str().ends_with('…'));
        // Removed characters do not count toward the limit.
        let padded = format!("{}{}", "\u{202E}".repeat(300), "x".repeat(10));
        assert_eq!(
            DisplayText::from_untrusted(&padded).as_str(),
            "x".repeat(10)
        );
    }

    #[test]
    fn debug_prints_the_length_only() {
        let t = DisplayText::from_untrusted("Synthetic Test Phone");
        let d = format!("{t:?}");
        assert!(!d.contains("Synthetic"));
        assert!(d.contains("20"));
    }
}
