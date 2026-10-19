//! JSON text that is safe to paste and to embed (M6 spec §5.1, D138).
//!
//! `serde_json` escapes `"`, `\` and C0 controls, but writes U+2028,
//! U+2029, U+0085, DEL and the other C1 controls raw. Those end a line in
//! JavaScript source, in many editors and in Python's `str.splitlines()`,
//! so they are escaped here as `\uXXXX`. Every character outside a JSON
//! string in `serde_json`'s output is ASCII, so the post-pass only ever
//! touches characters inside strings and the result is the same JSON value.
//!
//! Slice 6e uses it for `manifest.json`; the JSON writer (6f) uses the same
//! rule for its records.

use serde::Serialize;

use crate::error::{ExportError, LimitKind};

/// Whether `c` is escaped beyond what `serde_json` does.
pub fn needs_escape(c: char) -> bool {
    matches!(c, '\u{7F}'..='\u{9F}' | '\u{2028}' | '\u{2029}')
}

/// `json` with every [`needs_escape`] character written as `\uXXXX`.
pub fn escape_separators(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        if needs_escape(c) {
            out.push_str(&format!("\\u{:04x}", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    out
}

/// `v` as pretty-printed JSON (two-space indent, LF), escaped, with a
/// final newline.
pub fn to_pretty<T: Serialize>(v: &T) -> Result<String, ExportError> {
    let s = serde_json::to_string_pretty(v)
        .map_err(|_| ExportError::Limit(LimitKind::NotSerializable))?;
    let mut s = escape_separators(&s);
    s.push('\n');
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_separators_del_and_c1_are_escaped_and_the_value_is_kept() {
        let v = serde_json::json!({ "a": "x\u{2028}y\u{2029}z\u{85}\u{7f}\u{9f}\n\u{a0}é" });
        let s = to_pretty(&v).expect("json");
        for c in ['\u{2028}', '\u{2029}', '\u{85}', '\u{7f}', '\u{9f}'] {
            assert!(!s.contains(c), "{c:?}");
        }
        assert!(s.contains("\\u2028") && s.contains("\\u0085") && s.contains("\\u007f"));
        // NBSP and é stay as they are.
        assert!(s.contains('\u{a0}') && s.contains('é'));
        let back: serde_json::Value = serde_json::from_str(&s).expect("parse");
        assert_eq!(back, v);
        assert_eq!(s.lines().count(), 3);
        assert!(s.ends_with("}\n"));
    }
}
