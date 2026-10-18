//! Archive identifiers and labels (spec §5.1, D52).
//!
//! An archive's directory is named `a` followed by 32 lowercase hex
//! characters. The name is generated here and parsed strictly wherever it
//! comes from outside (the webview, a directory listing), so it can never
//! hold a separator, `..` or anything else a path could be built from.
//!
//! The user's label lives only in the database. It is re-validated whenever
//! it is read, and its `Debug` prints only its length (it is never logged).

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// The length of an archive id: `a` and 32 hex characters.
pub const ARCHIVE_ID_LEN: usize = 33;

/// The longest label, in UTF-8 bytes (also a CHECK in `v1.sql`).
pub const LABEL_MAX_BYTES: usize = 200;

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The id of one archive: `a` followed by 32 lowercase hex characters.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArchiveId(String);

/// Why a string is not an [`ArchiveId`] or a [`Label`]. Carries no input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IdError {
    #[error("not an archive id")]
    NotAnArchiveId,
    #[error("the label is empty")]
    EmptyLabel,
    #[error("the label is longer than 200 bytes")]
    LabelTooLong,
    #[error("the label contains a control character")]
    LabelControl,
    #[error("the label contains a bidirectional formatting character")]
    LabelBidi,
}

impl ArchiveId {
    /// Parse an id: exactly `a` and 32 characters from `0-9a-f`.
    pub fn parse(s: &str) -> Result<ArchiveId, IdError> {
        let b = s.as_bytes();
        let shaped = b.len() == ARCHIVE_ID_LEN
            && b.first() == Some(&b'a')
            && b.iter()
                .skip(1)
                .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'));
        if shaped {
            Ok(ArchiveId(s.to_owned()))
        } else {
            Err(IdError::NotAnArchiveId)
        }
    }

    /// The id as text, for the shell's DTOs. Never log it: it is part of a
    /// path (§13.2).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The 32 hex characters after the `a`.
    pub(crate) fn hex(&self) -> &str {
        self.0.get(1..).unwrap_or("")
    }

    /// A fresh id and a fresh 16-byte archive UUID (§5.1).
    ///
    /// Names are not secrets, so no CSPRNG is needed: the seed is
    /// SHA-256(pid ‖ nanos ‖ counter), the UUID is the first 16 bytes of
    /// SHA-256("uuid" ‖ seed), and the id is the first 16 bytes of
    /// SHA-256(pid ‖ nanos ‖ counter ‖ uuid). Uniqueness on disk is enforced
    /// by the no-replace rename at creation, not by this function.
    pub(crate) fn generate() -> (ArchiveId, [u8; 16]) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let counter = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let mut seed = Sha256::new();
        seed.update(pid.to_be_bytes());
        seed.update(nanos.to_be_bytes());
        seed.update(counter.to_be_bytes());
        let seed = seed.finalize();

        let mut u = Sha256::new();
        u.update(b"uuid");
        u.update(seed);
        let mut uuid = [0u8; 16];
        for (dst, src) in uuid.iter_mut().zip(u.finalize().iter()) {
            *dst = *src;
        }

        let mut h = Sha256::new();
        h.update(pid.to_be_bytes());
        h.update(nanos.to_be_bytes());
        h.update(counter.to_be_bytes());
        h.update(uuid);
        let digest = h.finalize();
        let mut s = String::with_capacity(ARCHIVE_ID_LEN);
        s.push('a');
        for b in digest.iter().take(16) {
            push_hex(&mut s, *b);
        }
        (ArchiveId(s), uuid)
    }
}

pub(crate) fn push_hex(s: &mut String, b: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for nibble in [b >> 4, b & 0x0f] {
        if let Some(c) = HEX.get(usize::from(nibble)) {
            s.push(char::from(*c));
        }
    }
}

/// Lowercase hex of `bytes`.
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::new();
    for b in bytes {
        push_hex(&mut s, *b);
    }
    s
}

/// Prints no characters of the id: archive directory names are never
/// logged (§13.2).
impl fmt::Debug for ArchiveId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ArchiveId(..)")
    }
}

/// A user-typed archive label: 1 to 200 UTF-8 bytes, with no control
/// characters and no bidirectional formatting characters.
#[derive(Clone, PartialEq, Eq)]
pub struct Label(String);

/// `true` for the Unicode bidirectional formatting characters: ALM, LRM,
/// RLM, LRE, RLE, PDF, LRO, RLO, LRI, RLI, FSI and PDI.
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

impl Label {
    /// Validate a label.
    pub fn parse(s: &str) -> Result<Label, IdError> {
        if s.is_empty() {
            return Err(IdError::EmptyLabel);
        }
        if s.len() > LABEL_MAX_BYTES {
            return Err(IdError::LabelTooLong);
        }
        if s.chars().any(char::is_control) {
            return Err(IdError::LabelControl);
        }
        if s.chars().any(is_bidi_control) {
            return Err(IdError::LabelBidi);
        }
        Ok(Label(s.to_owned()))
    }

    /// The label's text, for display as text (I7). Never log it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Prints only the length.
impl fmt::Debug for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Label(<{} bytes>)", self.0.len())
    }
}

/// A stored label, re-validated when read.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LabelState {
    /// The archive has no label.
    None,
    /// A valid label.
    Ok(Label),
    /// The stored value is not a valid label (or not text). It is not shown.
    Unreadable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_ids_parse_strictly() {
        let good = format!("a{}", "0123456789abcdef".repeat(2));
        let id = ArchiveId::parse(&good).expect("good");
        assert_eq!(id.as_str(), good);
        assert_eq!(id.hex().len(), 32);
        let bad = [
            String::new(),
            "a".into(),
            format!("A{}", "0".repeat(32)),
            format!("a{}", "0".repeat(31)),
            format!("a{}", "0".repeat(33)),
            format!("a{}", "F".repeat(32)),
            format!("b{}", "0".repeat(32)),
            format!("a{}/", "0".repeat(31)),
            format!("a{}\\", "0".repeat(31)),
            format!("a..{}", "0".repeat(30)),
            format!("a{}é", "0".repeat(30)),
            format!(".oma-new-{}", "0".repeat(32)),
            format!("a{}\0", "0".repeat(31)),
        ];
        for b in bad {
            assert_eq!(ArchiveId::parse(&b), Err(IdError::NotAnArchiveId), "{b:?}");
        }
    }

    #[test]
    fn generated_ids_parse_and_differ() {
        let (a, ua) = ArchiveId::generate();
        let (b, ub) = ArchiveId::generate();
        assert_ne!(a, b);
        assert_ne!(ua, ub);
        assert_eq!(ArchiveId::parse(a.as_str()).expect("parses"), a);
        assert_eq!(format!("{a:?}"), "ArchiveId(..)");
    }

    #[test]
    fn labels_reject_control_bidi_and_length() {
        assert!(Label::parse("Family phone 2024").is_ok());
        assert!(Label::parse("עברית and 中文").is_ok());
        assert_eq!(Label::parse(""), Err(IdError::EmptyLabel));
        assert_eq!(Label::parse("a\nb"), Err(IdError::LabelControl));
        assert_eq!(Label::parse("a\u{7f}"), Err(IdError::LabelControl));
        assert_eq!(Label::parse("a\u{202E}b"), Err(IdError::LabelBidi));
        assert_eq!(Label::parse("a\u{2066}b"), Err(IdError::LabelBidi));
        assert_eq!(Label::parse("a\u{200F}"), Err(IdError::LabelBidi));
        assert!(Label::parse(&"x".repeat(200)).is_ok());
        assert_eq!(Label::parse(&"x".repeat(201)), Err(IdError::LabelTooLong));
        // 67 three-byte characters are 201 bytes.
        assert_eq!(Label::parse(&"€".repeat(67)), Err(IdError::LabelTooLong));
        let l = Label::parse("SENTINEL-LABEL").expect("ok");
        assert_eq!(format!("{l:?}"), "Label(<14 bytes>)");
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(to_hex(&[0x00, 0xab, 0xff]), "00abff");
    }
}
