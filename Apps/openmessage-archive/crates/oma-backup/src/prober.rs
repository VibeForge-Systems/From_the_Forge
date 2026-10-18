//! Probing an attachment's blob without extracting it (M3 §3.5, D30).
//!
//! A probe reads metadata only. For an encrypted backup it also unwraps the
//! file's key in memory, to catch a damaged `EncryptionKey`, and zeroizes
//! it straight away. It never decrypts or writes file content.
//!
//! A blob whose length does not match the Manifest's `Size` (padded to the
//! next AES block for an encrypted backup, as validation compares it) is
//! [`BlobProbe::SizeMismatch`], never `Present`: a truncated or empty blob
//! is not the file the backup recorded.

use std::path::PathBuf;

use crate::error::{BackupError, FileUnavailable};
use crate::extract::AttachmentKey;
use crate::guard::BlobDirGuard;
use crate::keys::unwrap_file_key;
use crate::manifest_db::EntryKind;
use crate::open::OpenBackup;
use crate::plists::BackupFile;
use crate::validate::{entry_is_consistent, padded_len, BlobState};

/// What a probe found. Faults on the source side are outcomes, never errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BlobProbe {
    /// The blob is a regular file of this length (the ciphertext length for
    /// an encrypted backup). For an encrypted file with content, its key
    /// also unwrapped.
    Present {
        /// Length in bytes.
        len: u64,
    },
    /// The blob is not in the backup.
    Missing,
    /// The blob, or its `xx/` directory, is a link, directory or device.
    NotRegular,
    /// The blob's metadata could not be read.
    Unreadable(std::io::ErrorKind),
    /// The entry is a directory, symlink or other non-file.
    NotAFile,
    /// The entry's fileID does not match its domain and path, or is not a
    /// fileID.
    InvalidEntry,
    /// Encrypted backups only: a file with content has no `EncryptionKey`.
    NoKey,
    /// Encrypted backups only: the file has no `ProtectionClass`.
    NoProtectionClass,
    /// Encrypted backups only: the class key the file needs is unavailable.
    ClassKeyUnavailable,
    /// Encrypted backups only: the file's key did not unwrap.
    KeyDoesNotUnwrap,
    /// The blob is a regular file, but its length differs from the
    /// Manifest's `Size` (for an encrypted backup, from `Size` padded to the
    /// next 16-byte block). For an encrypted file, its key unwrapped. The
    /// declared size stays in the entry's Manifest record.
    SizeMismatch {
        /// The blob's length in bytes.
        len: u64,
    },
}

/// Probes blobs for one open backup. It reuses one guard, so each `xx/`
/// directory is checked once.
pub struct FileProber<'a> {
    ob: &'a OpenBackup,
    root: PathBuf,
    guard: BlobDirGuard,
}

impl std::fmt::Debug for FileProber<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileProber").finish_non_exhaustive()
    }
}

impl OpenBackup {
    /// A prober for this backup's blobs.
    pub fn file_prober(&self) -> FileProber<'_> {
        let root = self.inspection.dir().to_path_buf();
        FileProber {
            ob: self,
            guard: BlobDirGuard::new(root.clone()),
            root,
        }
    }
}

impl FileProber<'_> {
    /// Probe the blob of a located file. Never returns an error, and never
    /// reads or writes file content.
    pub fn probe(&mut self, key: &AttachmentKey) -> BlobProbe {
        let entry = &key.0;
        if entry.kind != EntryKind::File {
            return BlobProbe::NotAFile;
        }
        if !entry_is_consistent(entry) {
            return BlobProbe::InvalidEntry;
        }
        let len = match self
            .ob
            .blob_state(&self.root, &mut self.guard, &entry.file_id)
        {
            Ok(BlobState::Present(len)) => len,
            Ok(BlobState::Missing) => return BlobProbe::Missing,
            Ok(BlobState::Unsafe) => return BlobProbe::NotRegular,
            Err(BackupError::Io { kind, .. }) => return BlobProbe::Unreadable(kind),
            Err(_) => return BlobProbe::Unreadable(std::io::ErrorKind::Other),
        };
        let declared = entry.record.as_ref().and_then(|r| r.size);
        let Some(keys) = &self.ob.keys else {
            return match declared {
                Some(d) if d != len => BlobProbe::SizeMismatch { len },
                _ => BlobProbe::Present { len },
            };
        };
        let Some(record) = entry.record.as_ref() else {
            return BlobProbe::NoKey;
        };
        let Some(wrapped) = record.wrapped_key() else {
            // As extraction: an empty file needs no key.
            return if declared.unwrap_or(0) > 0 || len > 0 {
                BlobProbe::NoKey
            } else {
                BlobProbe::Present { len }
            };
        };
        let Some(class) = record.protection_class else {
            return BlobProbe::NoProtectionClass;
        };
        // An encrypted file with content is at least one AES block; a
        // declared size of 0 is not compared (as extraction: an empty file
        // needs nothing).
        let declared = declared.filter(|d| *d > 0);
        if len == 0 && declared.is_none() {
            return BlobProbe::Present { len };
        }
        match unwrap_file_key(&keys.class_keys, &keys.keybag, class, wrapped) {
            // The key is Zeroizing and is dropped (wiped) here.
            Ok(_key) => {}
            Err(BackupError::FileUnavailable(FileUnavailable::ClassKeyUnavailable))
            | Err(BackupError::Unsupported(_))
            | Err(BackupError::Malformed {
                file: BackupFile::Keybag,
                ..
            }) => return BlobProbe::ClassKeyUnavailable,
            Err(_) => return BlobProbe::KeyDoesNotUnwrap,
        }
        match declared {
            Some(d) if padded_len(d) != Some(len) => BlobProbe::SizeMismatch { len },
            _ => BlobProbe::Present { len },
        }
    }
}
