//! Errors. They carry kinds, never content.
//!
//! No variant holds a path, a device name, a `relativePath`, a SnapshotState
//! value, a password or any SQLite message text, so `Debug` and `Display` are
//! safe to log. User-facing sentences are written by the shell, not here.

use std::io;

use crate::discover::RefusalReason;
use crate::limits::LimitKind;
use crate::plists::{BackupFile, SnapshotState};

/// Everything that can go wrong while reading a backup.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// The folder is not a backup the app can list.
    #[error("not a backup")]
    NotABackup(RefusalReason),
    /// A backup layout or encryption scheme the app does not read.
    #[error("unsupported backup")]
    Unsupported(UnsupportedReason),
    /// The backup did not finish.
    #[error("incomplete backup")]
    Incomplete {
        /// The closed classification of `SnapshotState`, never its raw text.
        snapshot: SnapshotState,
    },
    /// Damaged or hostile content.
    #[error("malformed {file:?}")]
    Malformed {
        /// The file the problem was found in.
        file: BackupFile,
        /// What was wrong with it.
        detail: MalformedDetail,
    },
    /// A cap in [`crate::Limits`] was exceeded.
    #[error("limit exceeded")]
    LimitExceeded {
        /// Which cap.
        what: LimitKind,
        /// The value found. For [`LimitKind::PasswordBytes`] this is always
        /// `cap + 1`: the length of a password is never recorded.
        value: u64,
        /// The cap.
        cap: u64,
    },
    /// The backup is encrypted and no password was given.
    #[error("password required")]
    PasswordRequired,
    /// The first class key failed its integrity check.
    #[error("password incorrect or keybag damaged")]
    WrongPassword,
    /// The first class key unwrapped but a later one did not.
    #[error("keybag damaged")]
    KeybagDamaged,
    /// The job was cancelled.
    #[error("cancelled")]
    Cancelled,
    /// A file listed in the manifest cannot be provided.
    #[error("file not available")]
    FileUnavailable(FileUnavailable),
    /// An operating-system I/O error.
    #[error("I/O error during {op:?}")]
    Io {
        /// What was being done.
        op: IoOp,
        /// Whether the backup (source) or staging was being accessed.
        side: IoSide,
        /// The error kind.
        kind: io::ErrorKind,
        /// The raw OS error number, which is not content.
        os_code: Option<i32>,
    },
    /// SQLite reported an error. Only the primary result code is kept.
    #[error("SQLite error")]
    Sqlite {
        /// The primary result code (`extended & 0xff`), if there was one.
        code: Option<i32>,
    },
    /// A source file changed while it was being copied: its length or
    /// modification time differed after the copy. A staged copy may be
    /// inconsistent, so it is removed; after
    /// [`crate::OpenBackup::copy_file_to`] the caller discards what its
    /// writer received.
    #[error("source changed during copy")]
    SourceChanged,
    /// A staged database's schema could not be loaded under the limits the
    /// caller asked for, but loads under [`crate::StagedDbLimits::ceiling`].
    /// The caller decides whether that makes the database unsupported.
    #[error("schema exceeds limits")]
    SchemaExceedsLimits,
    /// The staging directory failed a safety guard.
    #[error("staging refused")]
    StagingRefused(StagingRefusal),
    /// A condition that should be impossible.
    #[error("internal")]
    Internal(&'static str),
}

impl BackupError {
    /// An I/O error on `side`, keeping only its kind and OS code.
    pub(crate) fn io(op: IoOp, side: IoSide, err: &io::Error) -> Self {
        BackupError::Io {
            op,
            side,
            kind: err.kind(),
            os_code: err.raw_os_error(),
        }
    }

    /// An I/O error on the backup side.
    pub(crate) fn source_io(op: IoOp, err: &io::Error) -> Self {
        BackupError::io(op, IoSide::Source, err)
    }

    /// An I/O error on the staging side.
    pub(crate) fn staging_io(op: IoOp, err: &io::Error) -> Self {
        BackupError::io(op, IoSide::Staging, err)
    }

    /// A malformed-content error.
    pub(crate) fn malformed(file: BackupFile, detail: MalformedDetail) -> Self {
        BackupError::Malformed { file, detail }
    }

    /// A limit error.
    pub(crate) fn limit(what: LimitKind, value: u64, cap: u64) -> Self {
        BackupError::LimitExceeded { what, value, cap }
    }
}

/// The operation that failed in [`BackupError::Io`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoOp {
    /// Reading metadata (`stat`).
    Metadata,
    /// Opening a file.
    Open,
    /// Reading a file.
    Read,
    /// Seeking in a file.
    Seek,
    /// Creating a file or directory.
    Create,
    /// Writing a file.
    Write,
    /// Renaming a file.
    Rename,
    /// Removing a file or directory.
    Remove,
    /// Listing a directory.
    ReadDir,
    /// Canonicalising a path.
    Canonicalize,
    /// Taking or releasing a file lock.
    Lock,
}

/// Which side of the job an I/O error happened on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoSide {
    /// The backup being read.
    Source,
    /// The staging directory inside the archive library.
    Staging,
    /// The caller's writer in [`crate::OpenBackup::copy_file_to`] (M4: a
    /// partial file in the archive's attachment store). `kind` and
    /// `os_code` are kept so the caller can tell a full volume
    /// (`StorageFull`) or a per-file size limit (`FileTooLarge`, `EFBIG`,
    /// `ERROR_FILE_TOO_LARGE`) from other failures.
    Sink,
}

/// Why a listed file cannot be provided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileUnavailable {
    /// The blob file is not in the backup.
    BlobMissing,
    /// The blob exists but is a link, directory or device.
    BlobNotRegular,
    /// An encrypted file with content has no EncryptionKey.
    NoKey,
    /// An encrypted file has no ProtectionClass.
    NoProtectionClass,
    /// The class key the file needs is not available.
    ClassKeyUnavailable,
    /// More than one row has the file's fileID.
    Ambiguous,
    /// The entry is a directory, symlink or other non-file.
    NotAFile,
    /// No row has the file's fileID.
    NotListed,
    /// The row has the wrong storage classes, or its domain and path do not
    /// match the lookup, or its fileID is invalid.
    InvalidEntry,
}

/// Why the staging directory was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagingRefusal {
    /// Staging would sit inside a backup, `MobileSync/Backup` or
    /// `iMazing/Backups`.
    Guard,
    /// `.oma-staging` is a link, or not where it should be.
    NotARealDirectory,
    /// `.oma-staging` is owned by another user (Unix).
    WrongOwner,
    /// `.oma-staging` is readable or writable by others (Unix). Also what a
    /// volume that ignores POSIX modes (FAT, exFAT, NTFS on Linux) produces.
    LoosePermissions,
    /// The filesystem does not support the lock staging needs.
    LockUnsupported,
}

/// A backup the app recognises but does not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedReason {
    /// Format 2.4 (`Manifest.mbdb`).
    LegacyMbdb,
    /// Encrypted, but `Manifest.plist` has no `BackupKeyBag`.
    NoKeybag,
    /// Encrypted, but `Manifest.plist` has no `ManifestKey`.
    EncryptedWithoutManifestKey,
    /// `DPSL` or `DPIC` is absent from the keybag.
    SingleStageKdf,
    /// No class is usable with the password: none has `WRAP & 2`, `KTYP` 0
    /// (or absent) and a 40-byte `WPKY` in a record with a valid `CLAS`.
    NoPassphraseClasses,
    /// A needed class has a key type other than 0 (AES).
    KeyType(u32),
    /// `IsEncrypted` is absent or not a boolean.
    EncryptionUnknown,
}

/// What was wrong with a malformed file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MalformedDetail {
    /// The file is missing.
    Missing,
    /// The file is larger than its cap.
    TooLarge,
    /// The file is not a valid property list.
    PlistSyntax,
    /// A value has an unexpected type.
    UnexpectedType(&'static str),
    /// The structure exceeds a processing budget.
    BudgetExceeded,
    /// A record runs past the end of its container.
    Truncated,
    /// A value has the wrong length.
    BadLength {
        /// The keybag tag, or a pseudo-tag such as `MKEY` or `EKEY`.
        tag: [u8; 4],
    },
    /// Two keybag class records have the same `CLAS`.
    DuplicateClass(u32),
    /// The file is not an SQLite database.
    NotSqlite,
    /// The database schema is not the expected one.
    Schema(&'static str),
    /// The PKCS#7 padding is invalid.
    Padding,
    /// The ciphertext length is not a multiple of 16.
    NotBlockAligned,
    /// The file is empty.
    Empty,
    /// Two facts contradict each other.
    Inconsistent(&'static str),
    /// An NSKeyedArchiver UID points outside `$objects`.
    UidOutOfRange,
    /// NSKeyedArchiver references nest too deeply.
    NestingTooDeep,
    /// The path is a link, FIFO, device or directory.
    NotRegularFile,
    /// The file exists but could not be read while listing.
    ReadError(io::ErrorKind),
}

/// Map a rusqlite error to [`BackupError`], keeping only the primary code.
/// An interrupt after cancellation becomes [`BackupError::Cancelled`].
/// A prepare error that rusqlite reports with an SQL offset keeps its code
/// too; its message and SQL text are dropped.
pub(crate) fn sqlite_error(err: &rusqlite::Error, cancelled: bool) -> BackupError {
    let failure = match err {
        rusqlite::Error::SqlInputError { error, .. } => Some(error),
        other => other.sqlite_error(),
    };
    match failure {
        Some(e) => {
            if e.code == rusqlite::ErrorCode::OperationInterrupted && cancelled {
                return BackupError::Cancelled;
            }
            BackupError::Sqlite {
                code: Some(e.extended_code & 0xff),
            }
        }
        None => BackupError::Sqlite { code: None },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_keep_kind_and_os_code_only() {
        let e = io::Error::from_raw_os_error(2);
        match BackupError::source_io(IoOp::Open, &e) {
            BackupError::Io {
                op,
                side,
                kind,
                os_code,
            } => {
                assert_eq!(op, IoOp::Open);
                assert_eq!(side, IoSide::Source);
                assert_eq!(kind, e.kind());
                assert_eq!(os_code, Some(2));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn sqlite_messages_are_dropped() {
        let err = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(11),
            Some("secret message text".into()),
        );
        let mapped = sqlite_error(&err, false);
        let shown = format!("{mapped:?} {mapped}");
        assert!(!shown.contains("secret"));
        assert!(matches!(mapped, BackupError::Sqlite { code: Some(11) }));
    }

    #[test]
    fn prepare_errors_keep_their_code_but_not_their_text() {
        let err = rusqlite::Error::SqlInputError {
            error: rusqlite::ffi::Error::new(23),
            msg: "not authorized: secret".into(),
            sql: "SELECT secret".into(),
            offset: 7,
        };
        let mapped = sqlite_error(&err, false);
        assert!(!format!("{mapped:?} {mapped}").contains("secret"));
        assert!(matches!(mapped, BackupError::Sqlite { code: Some(23) }));
    }

    #[test]
    fn interrupt_maps_to_cancelled_only_when_cancelled() {
        let err = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(9), None);
        assert!(matches!(sqlite_error(&err, true), BackupError::Cancelled));
        assert!(matches!(
            sqlite_error(&err, false),
            BackupError::Sqlite { code: Some(9) }
        ));
    }
}
