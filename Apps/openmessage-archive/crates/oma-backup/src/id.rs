//! Opaque, stable identifiers, computed in Rust only.
//!
//! Each id is the first 16 bytes of `SHA-256(prefix ‖ path)`, as 32
//! lowercase hex characters, where `path` is the display-canonical UTF-8 path.
//! The webview can only echo an id it was given; it cannot construct one.

use std::fmt::Write as _;
use std::path::Path;

use sha2::{Digest, Sha256};

fn hashed_id(prefix: &[u8], path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let mut hasher = Sha256::new();
    hasher.update(prefix);
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        // Writing to a String cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    Some(out)
}

/// Identifies one backup folder.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BackupId(String);

impl BackupId {
    /// The id for a display-canonical path, or `None` when the path is not
    /// valid Unicode.
    pub fn for_path(display_canonical: &Path) -> Option<Self> {
        hashed_id(b"oma-backup-id\0", display_canonical).map(BackupId)
    }

    /// The 32-character hex form.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identifies one scanned location (a default root or an added folder).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocationId(String);

impl LocationId {
    /// The id for a display-canonical path, or `None` when the path is not
    /// valid Unicode.
    pub fn for_path(display_canonical: &Path) -> Option<Self> {
        hashed_id(b"oma-location-id\0", display_canonical).map(LocationId)
    }

    /// The 32-character hex form.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn ids_are_stable_hex_and_domain_separated() {
        let p = PathBuf::from("/backups/SYNTHETIC0-0000000000000000");
        let a = BackupId::for_path(&p).expect("utf-8");
        let b = BackupId::for_path(&p).expect("utf-8");
        assert_eq!(a, b);
        assert_eq!(a.as_str().len(), 32);
        assert!(a
            .as_str()
            .bytes()
            .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')));
        let l = LocationId::for_path(&p).expect("utf-8");
        assert_ne!(a.as_str(), l.as_str());
        let other = BackupId::for_path(Path::new("/backups/other")).expect("utf-8");
        assert_ne!(a, other);
    }

    #[test]
    fn known_answer() {
        // SHA-256("oma-backup-id\0/x")[..16], computed independently with Python hashlib.
        let id = BackupId::for_path(Path::new("/x")).expect("utf-8");
        assert_eq!(id.as_str(), KNOWN_X);
    }

    const KNOWN_X: &str = "76e46e1c230236cbaf26303248edee16";

    #[cfg(unix)]
    #[test]
    fn non_unicode_paths_have_no_id() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let p = Path::new(OsStr::from_bytes(b"/bad\xff"));
        assert!(BackupId::for_path(p).is_none());
        assert!(LocationId::for_path(p).is_none());
    }
}
