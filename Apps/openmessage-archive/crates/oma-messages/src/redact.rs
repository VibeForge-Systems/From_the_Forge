//! Wrapper types for content and identifiers (M3 §5.1).
//!
//! - [`Redacted<T>`] prints `<redacted: N bytes>` from `Debug`. It has no
//!   `Display` and no `Serialize`; the value is read with
//!   [`Redacted::expose`].
//! - [`SqlText`] is TEXT (or BLOB-stored text) kept as the bytes SQLite
//!   returned, with the storage class SQLite reported ([`TextClass`]). It is
//!   never repaired, and it is zeroized on drop.
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

/// Decoded text owned by this crate (for example an `attributedBody`'s
/// text). `Zeroizing` clears it on drop.
impl ByteLen for zeroize::Zeroizing<String> {
    fn byte_len(&self) -> usize {
        self.len()
    }
}

/// The storage class SQLite reported for a text-bearing cell (M4 R7).
///
/// sms.db's text columns are typeless in places, so a cell may hold a BLOB.
/// The bytes are the same either way; the class is kept so a store that
/// writes every value as a BLOB (the M4 archive) does not erase it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextClass {
    /// SQLite reported `text`.
    Text,
    /// SQLite reported `blob`.
    Blob,
}

/// Text from the database, as the bytes SQLite returned, with the storage
/// class it reported. It may not be valid UTF-8; [`SqlText::to_str`] says
/// whether it is. Two values are equal only when their bytes and their
/// classes are. Zeroized on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct SqlText {
    bytes: Box<[u8]>,
    class: TextClass,
}

impl SqlText {
    /// Copy bytes SQLite returned as `text`.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        SqlText::from_bytes_with_class(bytes, TextClass::Text)
    }

    /// Copy bytes SQLite returned with the given storage class.
    pub fn from_bytes_with_class(bytes: &[u8], class: TextClass) -> Self {
        SqlText {
            bytes: bytes.into(),
            class,
        }
    }

    /// The bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The storage class SQLite reported for the cell (M4 R7).
    pub fn class(&self) -> TextClass {
        self.class
    }

    /// The text, when it is valid UTF-8.
    pub fn to_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.bytes).ok()
    }

    /// The length in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// `true` for the empty string.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl ByteLen for SqlText {
    fn byte_len(&self) -> usize {
        self.bytes.len()
    }
}

impl fmt::Debug for SqlText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.class {
            TextClass::Text => write!(f, "SqlText(<{} bytes>)", self.bytes.len()),
            TextClass::Blob => write!(f, "SqlText(<{} bytes>, blob)", self.bytes.len()),
        }
    }
}

impl Drop for SqlText {
    fn drop(&mut self) {
        self.bytes.zeroize();
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
        // The storage class is kept, shown, and part of equality (R7).
        assert_eq!(good.class(), TextClass::Text);
        let as_blob = SqlText::from_bytes_with_class(good.as_bytes(), TextClass::Blob);
        assert_eq!(as_blob.class(), TextClass::Blob);
        assert_eq!(as_blob.as_bytes(), good.as_bytes());
        assert_ne!(as_blob, good);
        assert_eq!(format!("{as_blob:?}"), "SqlText(<6 bytes>, blob)");
        assert_eq!(format!("{good:?}"), "SqlText(<6 bytes>)");
        assert!(RawBlob::from_bytes(b"").is_empty());
        assert_eq!(
            Redacted::new(good.clone()).into_inner().as_bytes(),
            good.as_bytes()
        );
    }
}
