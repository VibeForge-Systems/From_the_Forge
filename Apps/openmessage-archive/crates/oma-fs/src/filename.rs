//! Turning untrusted names into filenames that are safe on Windows and Linux.

/// Linux `NAME_MAX` is 255 bytes; Windows allows 255 UTF-16 code units.
/// Limiting to 255 UTF-8 bytes satisfies both.
pub const MAX_FILENAME_BYTES: usize = 255;

/// Extensions longer than this are not preserved on truncation; a name with a
/// 40-byte "extension" is more likely a dotted name than a real file type.
const MAX_PRESERVED_EXTENSION_BYTES: usize = 16;

const FALLBACK_NAME: &str = "unnamed";

/// Windows device names. Reserved with or without an extension and in any
/// case: `con.txt` opens the console, not a file. The superscript-digit forms
/// are reserved too, which most sanitizers miss.
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
    "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³", "CONIN$", "CONOUT$",
];

/// Produce a single path component that is safe to create on Windows and
/// Linux from an untrusted name (an attachment's original filename, a contact
/// name used in an export folder, and so on).
///
/// The result never contains a path separator, so it cannot introduce
/// directories or traverse upward. It is a *display-preserving* transform:
/// ordinary names come back unchanged, and only characters that are invalid,
/// dangerous, or misleading are replaced.
///
/// What is handled:
/// - `/` and `\` (separators on one platform or the other) become `_`.
/// - Characters invalid on Windows (`<>:"|?*`) and all control characters
///   become `_`. `:` also blocks NTFS alternate data streams.
/// - Unicode bidirectional overrides and isolates are removed, so a name
///   cannot render as `photo.jpg` while really ending in `.exe`.
/// - Trailing dots and spaces are trimmed (Windows silently strips them,
///   which would make two different names collide).
/// - Windows reserved device names get a `_` prefix.
/// - `.`, `..` and empty results become `unnamed`.
/// - Names longer than 255 bytes are truncated on a character boundary,
///   keeping a short extension when there is one.
///
/// Not handled: Unicode normalization (NFC vs NFD) and case-insensitive
/// collisions. Callers that write many files into one directory must still
/// de-duplicate names themselves.
pub fn sanitize_filename(untrusted: &str) -> String {
    let mut name: String = untrusted
        .chars()
        .filter(|c| !is_bidi_control(*c))
        .map(|c| if is_forbidden(c) { '_' } else { c })
        .collect();

    let trimmed_len = name.trim_end_matches(['.', ' ']).len();
    name.truncate(trimmed_len);
    let name = name.trim_start_matches(' ').to_owned();

    if name.is_empty() || name == "." || name == ".." {
        return FALLBACK_NAME.to_owned();
    }

    let name = if is_windows_reserved(&name) {
        format!("_{name}")
    } else {
        name
    };

    truncate_preserving_extension(&name, MAX_FILENAME_BYTES)
}

fn is_forbidden(c: char) -> bool {
    matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*') || c.is_control()
}

fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

fn is_windows_reserved(name: &str) -> bool {
    // Windows compares the part before the first dot, ignoring trailing spaces
    // ("CON .txt" is also the console).
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    WINDOWS_RESERVED
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(stem))
}

fn truncate_preserving_extension(name: &str, max_bytes: usize) -> String {
    if name.len() <= max_bytes {
        return name.to_owned();
    }
    let extension = name
        .rfind('.')
        .filter(|&dot| dot > 0 && name.len() - dot <= MAX_PRESERVED_EXTENSION_BYTES)
        .map(|dot| &name[dot..])
        .unwrap_or("");
    let budget = max_bytes - extension.len();
    let stem = &name[..name.len() - extension.len()];
    let mut cut = budget.min(stem.len());
    while !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = stem[..cut].trim_end_matches(['.', ' ']).to_owned();
    if out.is_empty() {
        out.push_str(FALLBACK_NAME);
    }
    out.push_str(extension);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_are_unchanged() {
        for name in [
            "IMG_1234.HEIC",
            "Quarterly report (final).pdf",
            "voice memo 2024-01-01.m4a",
            "日本語のファイル.txt",
            "emoji 🎉 party.jpg",
            ".hidden",
        ] {
            assert_eq!(sanitize_filename(name), name);
        }
    }

    #[test]
    fn separators_cannot_create_directories_or_traverse() {
        assert_eq!(sanitize_filename("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(
            sanitize_filename("..\\..\\Windows\\win.ini"),
            ".._.._Windows_win.ini"
        );
        assert_eq!(sanitize_filename("/absolute"), "_absolute");
        assert!(!sanitize_filename("a/b\\c").contains(['/', '\\']));
    }

    #[test]
    fn dot_names_and_empty_become_fallback() {
        assert_eq!(sanitize_filename(""), "unnamed");
        assert_eq!(sanitize_filename("."), "unnamed");
        assert_eq!(sanitize_filename(".."), "unnamed");
        assert_eq!(sanitize_filename("   "), "unnamed");
        assert_eq!(sanitize_filename("..."), "unnamed");
    }

    #[test]
    fn windows_invalid_and_control_characters_are_replaced() {
        assert_eq!(sanitize_filename("a<b>c:d\"e|f?g*h"), "a_b_c_d_e_f_g_h");
        assert_eq!(sanitize_filename("tab\there\nnull\0"), "tab_here_null_");
        assert_eq!(sanitize_filename("file.txt:secret"), "file.txt_secret");
    }

    #[test]
    fn bidi_overrides_cannot_disguise_an_extension() {
        // Renders as "photoexe.jpg" in a bidi-aware UI, but ends in ".jpg" only
        // visually. With the override removed, the real order is visible.
        let spoofed = "photo\u{202E}gpj.exe";
        assert_eq!(sanitize_filename(spoofed), "photogpj.exe");
        assert_eq!(sanitize_filename("a\u{2066}b\u{2069}c"), "abc");
    }

    #[test]
    fn trailing_dots_and_spaces_are_trimmed() {
        assert_eq!(sanitize_filename("report.pdf. . "), "report.pdf");
        assert_eq!(sanitize_filename("  leading"), "leading");
    }

    #[test]
    fn windows_reserved_names_are_prefixed_in_any_case_and_with_extensions() {
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("con.txt"), "_con.txt");
        assert_eq!(sanitize_filename("Lpt1.log"), "_Lpt1.log");
        assert_eq!(sanitize_filename("COM¹"), "_COM¹");
        assert_eq!(sanitize_filename("nul.tar.gz"), "_nul.tar.gz");
        assert_eq!(sanitize_filename("CON .txt"), "_CON .txt");
        // Not reserved: the reserved word is only part of the stem.
        assert_eq!(sanitize_filename("console.txt"), "console.txt");
        assert_eq!(sanitize_filename("COM10"), "COM10");
    }

    #[test]
    fn long_names_are_truncated_on_char_boundaries_keeping_the_extension() {
        let long = format!("{}.jpeg", "é".repeat(300));
        let out = sanitize_filename(&long);
        assert!(out.len() <= MAX_FILENAME_BYTES, "{} bytes", out.len());
        assert!(out.ends_with(".jpeg"));
        assert!(out.starts_with('é'));

        let no_ext = "x".repeat(1000);
        assert_eq!(sanitize_filename(&no_ext).len(), MAX_FILENAME_BYTES);

        // An over-long "extension" is not preserved; the name is cut plainly.
        let dotted = format!("a.{}", "b".repeat(400));
        let out = sanitize_filename(&dotted);
        assert_eq!(out.len(), MAX_FILENAME_BYTES);
        assert!(out.starts_with("a.b"));
    }

    #[test]
    fn output_is_always_a_single_nonempty_component() {
        let hostile = [
            "\u{202E}",
            "/",
            "\\",
            ":",
            "....//....//",
            "\0\0\0",
            " . . ",
            "CON",
        ];
        for input in hostile {
            let out = sanitize_filename(input);
            assert!(!out.is_empty(), "{input:?}");
            assert!(
                !out.contains(['/', '\\', '\0', ':']),
                "{input:?} -> {out:?}"
            );
            assert_ne!(out, ".");
            assert_ne!(out, "..");
            assert_eq!(sanitize_filename(&out), out, "not idempotent for {input:?}");
        }
    }
}
