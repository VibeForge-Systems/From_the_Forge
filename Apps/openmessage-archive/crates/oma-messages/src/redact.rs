//! Wrapper types for content and identifiers (M3 §5.1).
//!
//! - [`Redacted<T>`] prints `<redacted: N bytes>` from `Debug`. It has no
//!   `Display` and no `Serialize`; the value is read with
//!   [`Redacted::expose`].
//! - [`SqlText`] is TEXT (or BLOB-stored text) kept as the bytes SQLite
//!   returned. It is never repaired, and it is zeroized on drop.
//! - [`RawBlob`] is a BLOB kept on request. It is zeroized on drop.
//!
//! What is **not** zeroized: SQLite's page cache and buffers, and rusqlite's
//! intermediate copies. "Never repaired" holds for UTF-8 databases only; in a
//! UTF-16 database SQLite converts text itself.

use std::fmt;

use zeroize::Zeroize;

/// The number of bytes a redacted value holds; the only fact `Debug`
/// prints about it.
pub trait ByteLen {
    /// The length in bytes.
    fn byte_len(&self) -> usize;
}

/// Content or an identifier from the database. `Debug` prints only its
/// length.
#[derive(Clone, PartialEq, Eq)]
pub struct Redacted<T: ByteLen>(T);

impl<T: ByteLen> Redacted<T> {
    /// Wrap a value.
    pub fn new(value: T) -> Self {
        Redacted(value)
    }

    /// The value. Never log or `Debug`-print what this returns.
    pub fn expose(&self) -> &T {
        &self.0
    }

    /// The value, unwrapped. Never log or `Debug`-print it.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T: ByteLen> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<redacted: {} bytes>", self.0.byte_len())
    }
}

impl<T: ByteLen> ByteLen for Redacted<T> {
    fn byte_len(&self) -> usize {
        self.0.byte_len()
    }
}

/// Text from the database, as the bytes SQLite returned. It may not be
/// valid UTF-8; [`SqlText::to_str`] says whether it is. Zeroized on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct SqlText(Box<[u8]>);

impl SqlText {
    /// Copy bytes SQLite returned.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        SqlText(bytes.into())
    }

    /// The bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The text, when it is valid UTF-8.
    pub fn to_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }

    /// The length in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` for the empty string.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl ByteLen for SqlText {
    fn byte_len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for SqlText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SqlText(<{} bytes>)", self.0.len())
    }
}

impl Drop for SqlText {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// A BLOB from the database, kept only when the caller asks for raw blobs.
/// Zeroized on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct RawBlob(Box<[u8]>);

impl RawBlob {
    /// Copy bytes SQLite returned.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        RawBlob(bytes.into())
    }

    /// The bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The length in bytes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` for an empty blob.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl ByteLen for RawBlob {
    fn byte_len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for RawBlob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RawBlob(<{} bytes>)", self.0.len())
    }
}

impl Drop for RawBlob {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENTINEL: &str = "SYNTH-BODY-redact";

    #[test]
    fn debug_prints_only_a_length() {
        let t = Redacted::new(SqlText::from_bytes(SENTINEL.as_bytes()));
        let shown = format!("{t:?} {:?} {:#?}", t.expose(), t);
        assert!(!shown.contains(SENTINEL), "{shown}");
        assert!(shown.contains("<redacted: 17 bytes>"), "{shown}");
        let b = Redacted::new(RawBlob::from_bytes(SENTINEL.as_bytes()));
        let shown = format!("{b:?} {:?}", b.expose());
        assert!(!shown.contains(SENTINEL), "{shown}");
        assert_eq!(b.byte_len(), 17);
    }

    #[test]
    fn text_is_kept_as_bytes_and_never_repaired() {
        let bad = SqlText::from_bytes(&[b'a', 0xff, b'b']);
        assert_eq!(bad.as_bytes(), &[b'a', 0xff, b'b']);
        assert_eq!(bad.to_str(), None);
        assert_eq!(bad.len(), 3);
        let good = SqlText::from_bytes("héllo".as_bytes());
        assert_eq!(good.to_str(), Some("héllo"));
        assert!(SqlText::from_bytes(b"").is_empty());
        assert!(RawBlob::from_bytes(b"").is_empty());
        assert_eq!(
            Redacted::new(good.clone()).into_inner().as_bytes(),
            good.as_bytes()
        );
    }
}
