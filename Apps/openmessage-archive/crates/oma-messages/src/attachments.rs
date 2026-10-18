//! Attachment resolution (M3 §12, D50).
//!
//! `attachment.filename` is untrusted. It is a lookup key, never a path:
//! nothing is ever joined onto a filesystem path from it. Each value is
//! classified by its prefix, and only two classes are looked up in
//! Manifest.db, both under `Library/SMS/`:
//!
//! | Prefix | [`PathClass`] | Lookup | Status |
//! |---|---|---|---|
//! | `~/Library/SMS/Attachments/` | `SmsAttachments` | `MediaDomain`, `~/` stripped | Sourced (S2, S8) |
//! | `~/Library/SMS/StickerCache/` | `SmsStickerCache` | `MediaDomain`, `~/` stripped | Domain **UNVERIFIED** |
//! | `~/Library/Messages/` | `MacMessages` | not attempted | the macOS form (S3); on iOS **UNVERIFIED** |
//! | other `~/…` | `OtherHomeRelative` | not attempted | **UNVERIFIED** |
//! | `/var/mobile/…` | `AbsoluteVarMobile` | not attempted | occurrence **UNVERIFIED** |
//! | `/private/var/mobile/…` | `AbsolutePrivateVarMobile` | not attempted | occurrence **UNVERIFIED** |
//! | empty, or anything else | `Unrecognized` | not attempted | |
//!
//! Prefixes are compared byte for byte. An exact hit elsewhere in
//! `MediaDomain` could reach files that are not Messages attachments, which
//! M4 would then inherit, so the other classes stay counts until hardware
//! data verifies them.
//!
//! **Unusual names gate the lookup.** The candidate relative path must be
//! UTF-8; must hold no NUL and no backslash; must have no empty, `.` or
//! `..` component; and must pass `oma_fs::resolve_within` lexically (which
//! also refuses Windows device names, `:` streams, control characters, bidi
//! overrides and trailing dots). A refusal means "not attempted", counted
//! as an unusual name. Values over the 4 KiB cap are never read, so they
//! are never looked up either.
//!
//! **Mismatched forms:** an NFC/NFD or case difference between `filename`
//! and Manifest's `relativePath` reads as a miss. How often that happens is
//! **UNVERIFIED** (§25).
//!
//! **Fatal versus counted.** `find_file`'s `Ambiguous` and `InvalidEntry`
//! are outcomes. SQLite and I/O errors and cancellation stay errors
//! ([`crate::BackupStage::ManifestLookup`]). A probe never fails (it reads
//! metadata only, and unwraps an encrypted file's key in memory without
//! decrypting anything). No attachment plaintext is written.
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unreachable,
    clippy::unimplemented,
    clippy::string_slice
)]

use std::fmt;
use std::path::Path;

use oma_backup::{AttachmentKey, BackupError, BlobProbe, FileProber, FileUnavailable, OpenBackup};

use crate::col::Col;
use crate::error::{BackupStage, MessagesError};
use crate::redact::{Redacted, SqlText};

/// The only domain lookups use. The report states it as a constant.
pub const ATTACHMENT_LOOKUP_DOMAIN: &str = "MediaDomain";

/// The class of an `attachment.filename` value, by prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum PathClass {
    /// `~/Library/SMS/Attachments/…`: looked up (sourced: S2, S8).
    SmsAttachments,
    /// `~/Library/SMS/StickerCache/…`: looked up; the domain is
    /// **UNVERIFIED**, so a miss claims no absence.
    SmsStickerCache,
    /// `~/Library/Messages/…`: the macOS form; not looked up.
    MacMessages,
    /// Any other `~/…`: not looked up.
    OtherHomeRelative,
    /// `/var/mobile/…`: not looked up.
    AbsoluteVarMobile,
    /// `/private/var/mobile/…`: not looked up.
    AbsolutePrivateVarMobile,
    /// Empty, or any other form: not looked up.
    Unrecognized,
}

impl PathClass {
    /// Every class, in a fixed order.
    pub const ALL: [PathClass; 7] = [
        PathClass::SmsAttachments,
        PathClass::SmsStickerCache,
        PathClass::MacMessages,
        PathClass::OtherHomeRelative,
        PathClass::AbsoluteVarMobile,
        PathClass::AbsolutePrivateVarMobile,
        PathClass::Unrecognized,
    ];

    /// Whether values of this class are looked up (D50).
    pub const fn looked_up(self) -> bool {
        matches!(self, PathClass::SmsAttachments | PathClass::SmsStickerCache)
    }
}

const HOME: &[u8] = b"~/";
const SMS_ATTACHMENTS: &[u8] = b"~/Library/SMS/Attachments/";
const SMS_STICKER_CACHE: &[u8] = b"~/Library/SMS/StickerCache/";
const MAC_MESSAGES: &[u8] = b"~/Library/Messages/";
const VAR_MOBILE: &[u8] = b"/var/mobile/";
const PRIVATE_VAR_MOBILE: &[u8] = b"/private/var/mobile/";

/// Classify a filename by its prefix (byte for byte).
pub fn classify(filename: &[u8]) -> PathClass {
    if filename.starts_with(SMS_ATTACHMENTS) {
        PathClass::SmsAttachments
    } else if filename.starts_with(SMS_STICKER_CACHE) {
        PathClass::SmsStickerCache
    } else if filename.starts_with(MAC_MESSAGES) {
        PathClass::MacMessages
    } else if filename.starts_with(HOME) {
        PathClass::OtherHomeRelative
    } else if filename.starts_with(VAR_MOBILE) {
        PathClass::AbsoluteVarMobile
    } else if filename.starts_with(PRIVATE_VAR_MOBILE) {
        PathClass::AbsolutePrivateVarMobile
    } else {
        PathClass::Unrecognized
    }
}

/// Why no lookup was attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum NotAttemptedReason {
    /// `filename` is NULL.
    Null,
    /// `filename` is over its 4 KiB cap and was never read.
    TooLong,
    /// The candidate path is unusual (see the module documentation).
    UnusualName,
    /// The class is not looked up (D50).
    ClassNotLookedUp,
    /// The value has no recognised prefix (or is empty).
    Unrecognized,
    /// The `filename` column does not exist in this database.
    ColumnAbsent,
    /// `filename` holds a value that is not text.
    Unreadable,
}

impl NotAttemptedReason {
    /// Every reason, in a fixed order.
    pub const ALL: [NotAttemptedReason; 7] = [
        NotAttemptedReason::Null,
        NotAttemptedReason::TooLong,
        NotAttemptedReason::UnusualName,
        NotAttemptedReason::ClassNotLookedUp,
        NotAttemptedReason::Unrecognized,
        NotAttemptedReason::ColumnAbsent,
        NotAttemptedReason::Unreadable,
    ];
}

/// What the lookup of one attachment found.
#[derive(Clone)]
#[non_exhaustive]
pub enum LookupOutcome {
    /// No lookup was attempted.
    NotAttempted(NotAttemptedReason),
    /// Manifest.db lists the file. `key` is accepted by
    /// `OpenBackup::extract_file` (M4), and `blob` is the probe of its file
    /// (`None` when probing was turned off).
    Found {
        /// The opaque key of the listed file.
        key: AttachmentKey,
        /// The probe of its blob.
        blob: Option<BlobProbe>,
    },
    /// `SmsAttachments` only: the sourced key is not listed. The UI may say
    /// "not in this backup".
    NotListedAtSourcedKey,
    /// `SmsStickerCache`: not listed at the assumed key. Absence is **not**
    /// claimed, because the domain is **UNVERIFIED**.
    NotFoundAtAssumedKey {
        /// The class looked up.
        class: PathClass,
    },
    /// More than one Manifest row has the fileID.
    Ambiguous,
    /// The Manifest row does not match its lookup, or is malformed.
    InvalidEntry,
}

impl fmt::Debug for LookupOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LookupOutcome::NotAttempted(r) => f.debug_tuple("NotAttempted").field(r).finish(),
            // AttachmentKey's own Debug shows the entry kind only.
            LookupOutcome::Found { key, blob } => f
                .debug_struct("Found")
                .field("key", key)
                .field("blob", blob)
                .finish(),
            LookupOutcome::NotListedAtSourcedKey => f.write_str("NotListedAtSourcedKey"),
            LookupOutcome::NotFoundAtAssumedKey { class } => f
                .debug_struct("NotFoundAtAssumedKey")
                .field("class", class)
                .finish(),
            LookupOutcome::Ambiguous => f.write_str("Ambiguous"),
            LookupOutcome::InvalidEntry => f.write_str("InvalidEntry"),
        }
    }
}

/// Where an attachment's file is, as far as the check can tell.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AttachmentLocation {
    /// The class of `filename`; `None` when it is NULL, absent, unreadable
    /// or over its cap.
    pub class: Option<PathClass>,
    /// What the lookup found.
    pub lookup: LookupOutcome,
}

/// The relative path a looked-up class maps to: `~/` stripped. `None`
/// when the name is unusual.
fn candidate(filename: &[u8]) -> Option<&str> {
    let rel = filename.strip_prefix(HOME)?;
    let rel = std::str::from_utf8(rel).ok()?;
    if rel.contains(['\0', '\\']) {
        return None;
    }
    if rel
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return None;
    }
    oma_fs::resolve_within(Path::new("x"), rel).ok()?;
    Some(rel)
}

/// Looks attachments up in the backup's Manifest.db and probes their
/// files. It borrows the open backup, so it cannot outlive it.
pub struct AttachmentLocator<'a> {
    ob: &'a OpenBackup,
    prober: Option<FileProber<'a>>,
}

impl fmt::Debug for AttachmentLocator<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentLocator")
            .field("probes_blobs", &self.prober.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a> AttachmentLocator<'a> {
    /// A locator for `ob`. With `probe_blobs`, each file found is probed
    /// (metadata, and for an encrypted backup a key unwrap in memory).
    pub fn new(ob: &'a OpenBackup, probe_blobs: bool) -> Self {
        AttachmentLocator {
            ob,
            prober: probe_blobs.then(|| ob.file_prober()),
        }
    }

    /// Classify, look up and probe one `filename`.
    pub fn locate(
        &mut self,
        filename: &Col<Redacted<SqlText>>,
    ) -> Result<AttachmentLocation, MessagesError> {
        let not = |class, reason| AttachmentLocation {
            class,
            lookup: LookupOutcome::NotAttempted(reason),
        };
        let bytes = match filename {
            Col::Absent => return Ok(not(None, NotAttemptedReason::ColumnAbsent)),
            Col::Null => return Ok(not(None, NotAttemptedReason::Null)),
            Col::Oversized { .. } => return Ok(not(None, NotAttemptedReason::TooLong)),
            Col::Unreadable(_) => return Ok(not(None, NotAttemptedReason::Unreadable)),
            Col::Value(v) => v.expose().as_bytes(),
        };
        let class = classify(bytes);
        if class == PathClass::Unrecognized {
            return Ok(not(Some(class), NotAttemptedReason::Unrecognized));
        }
        if !class.looked_up() {
            return Ok(not(Some(class), NotAttemptedReason::ClassNotLookedUp));
        }
        let Some(rel) = candidate(bytes) else {
            return Ok(not(Some(class), NotAttemptedReason::UnusualName));
        };
        let lookup = match self.ob.find_attachment(ATTACHMENT_LOOKUP_DOMAIN, rel) {
            Ok(Some(key)) => {
                let blob = self.prober.as_mut().map(|p| p.probe(&key));
                LookupOutcome::Found { key, blob }
            }
            Ok(None) if class == PathClass::SmsAttachments => LookupOutcome::NotListedAtSourcedKey,
            Ok(None) => LookupOutcome::NotFoundAtAssumedKey { class },
            Err(BackupError::FileUnavailable(FileUnavailable::Ambiguous)) => {
                LookupOutcome::Ambiguous
            }
            Err(BackupError::FileUnavailable(FileUnavailable::InvalidEntry)) => {
                LookupOutcome::InvalidEntry
            }
            Err(e) => return Err(MessagesError::from_backup(e, BackupStage::ManifestLookup)),
        };
        Ok(AttachmentLocation {
            class: Some(class),
            lookup,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §19.1 `attachments`: every class row.
    #[test]
    fn classes() {
        for (name, want) in [
            (
                &b"~/Library/SMS/Attachments/aa/00/X/a.heic"[..],
                PathClass::SmsAttachments,
            ),
            (
                b"~/Library/SMS/StickerCache/00/X/s.heic",
                PathClass::SmsStickerCache,
            ),
            (
                b"~/Library/Messages/Attachments/a.heic",
                PathClass::MacMessages,
            ),
            (b"~/Documents/a.heic", PathClass::OtherHomeRelative),
            (b"~/", PathClass::OtherHomeRelative),
            (
                b"/var/mobile/Library/SMS/Attachments/a",
                PathClass::AbsoluteVarMobile,
            ),
            (
                b"/private/var/mobile/Library/SMS/a",
                PathClass::AbsolutePrivateVarMobile,
            ),
            (b"", PathClass::Unrecognized),
            (b"Library/SMS/Attachments/a", PathClass::Unrecognized),
            // Byte for byte: a case difference is not the sourced prefix.
            (b"~/library/sms/attachments/a", PathClass::OtherHomeRelative),
            (b"C:\\x", PathClass::Unrecognized),
        ] {
            assert_eq!(classify(name), want, "{name:?}");
        }
        assert_eq!(
            PathClass::ALL.iter().filter(|c| c.looked_up()).count(),
            2,
            "D50"
        );
    }

    /// Unusual names are never looked up.
    #[test]
    fn unusual_names() {
        let ok = b"~/Library/SMS/Attachments/aa/00/SYNTH-0001/IMG_0001.HEIC";
        assert_eq!(
            candidate(ok),
            Some("Library/SMS/Attachments/aa/00/SYNTH-0001/IMG_0001.HEIC")
        );
        // A long path of ordinary components passes; one over-long
        // component is not a safe filename.
        let deep = format!("~/Library/SMS/Attachments/{}x", "a/".repeat(1500));
        assert!(candidate(deep.as_bytes()).is_some());
        let long = format!("~/Library/SMS/Attachments/{}", "a".repeat(4000));
        assert_eq!(candidate(long.as_bytes()), None);
        for bad in [
            &b"~/Library/SMS/Attachments/../../etc/x"[..],
            b"~/Library/SMS/Attachments/a/..",
            b"~/Library/SMS/Attachments/..\\..\\x",
            b"~/Library/SMS/Attachments/C:\\x",
            b"~/Library/SMS/Attachments/a\0b",
            "~/Library/SMS/Attachments/a\u{202e}gpj.exe".as_bytes(),
            b"~/Library/SMS/Attachments/a.",
            b"~/Library/SMS/Attachments/CON",
            b"~/Library/SMS/Attachments/",
            b"~/Library/SMS/Attachments//a",
            b"~/Library/SMS/Attachments/./a",
            b"~/Library/SMS/Attachments/a:b",
            b"~/Library/SMS/Attachments/\xff\xfe",
        ] {
            assert_eq!(candidate(bad), None, "{bad:?}");
        }
    }
}
