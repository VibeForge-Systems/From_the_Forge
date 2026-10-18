//! Errors. No variant carries a path, a label, an archive id, a digest, a
//! string read from an archive or SQLite's error text, so every one is safe
//! to log and to show (spec §4.4, §13.2).

use std::io;

pub use oma_fs::PermProblem;

/// Why an archive's contents cannot be trusted as an archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DamageKind {
    /// The schema objects differ from the expected objects for the file's
    /// version (§7.3): an extra trigger, view, index or table, a missing
    /// one, or different SQL text.
    SchemaMismatch,
    /// The `archive_meta` row is missing.
    MetaMissing,
    /// `archive_meta` contradicts the file header or itself (for example
    /// `schema_version` differs from `PRAGMA user_version`), or the
    /// database is not in WAL mode.
    MetaInconsistent,
    /// A migration step failed one of its checks (foreign keys, the
    /// fingerprint for the new version, or the FTS integrity check). The
    /// step was rolled back.
    MigrationFailed,
    /// A value could not be read as the schema declares it.
    Unreadable,
    /// Reading the archive's schema or bookkeeping took more work than the
    /// op budget allows (§4.5): the file was crafted or is badly damaged.
    WorkBudget,
}

/// Why a location was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LocationRefusal {
    /// The library is not a real directory.
    NotADirectory,
    /// The library is a drive or file-system root.
    FilesystemRoot,
    /// The library is inside an iPhone backup, `MobileSync/Backup` or
    /// iMazing's backup store (invariant I1).
    InsideBackup,
    /// Less than the space needed to create an archive is available.
    InsufficientSpace,
    /// Less than three times the database size plus the reserve is
    /// available, so a migration was not started (§7.2).
    InsufficientSpaceForMigration,
}

/// Why a directory in the library is listed as refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RefusalKind {
    /// The `a<32hex>` entry is a link (or a junction), not a directory.
    Link,
    /// The entry or a file inside it failed the permission check.
    Permissions(PermProblem),
    /// The archive is on a network or virtualised volume.
    Volume,
    /// The entry could not be inspected.
    Io(io::ErrorKind),
}

/// Which step a SQLite error came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Stage {
    /// Opening a connection.
    Open,
    /// Applying the connection settings of §5.5.
    Configure,
    /// Reading the header or the schema for the fingerprint.
    Fingerprint,
    /// Reading `archive_meta`.
    Meta,
    /// Creating an archive.
    Create,
    /// The recovery work of `begin_write`.
    Recovery,
    /// A migration.
    Migrate,
    /// A read through the reader.
    Read,
    /// The checkpoint at close.
    Checkpoint,
    /// The import's read-only preflight (resume decision, stored keys).
    Preflight,
    /// The import's session bookkeeping (§8.3) and terminal state.
    Session,
    /// An import batch.
    Batch,
    /// The verifier's reads and its result rows.
    Verify,
}

/// Which file-system operation an I/O error came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IoOp {
    Metadata,
    Canonicalize,
    ReadDir,
    CreateDir,
    CreateFile,
    OpenLock,
    Lock,
    Rename,
    Remove,
    Sync,
}

/// Why an attachment file could not be opened through `open_blob`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BlobProblem {
    /// No `blob` row has this id.
    NotFound,
    /// The row's `storage_name` does not parse as 64 lowercase hex
    /// characters, or does not spell its `sha256` (a crafted file).
    BadName,
    /// No file at the storage name.
    Missing,
    /// A link, a directory or another non-regular file is there, or
    /// `attachments/` or its `<hh>` directory is not a private real
    /// directory.
    NotRegular,
    /// The file's length differs from the row's `byte_len`.
    LengthMismatch,
    /// Opening or inspecting the file failed.
    Io(io::ErrorKind),
}

/// An error from the archive library.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ArchiveError {
    /// Another job or program holds the archive's lock.
    #[error("archive in use")]
    Locked,
    /// The lock self-test failed: a second handle could take the lock that
    /// the first one holds, so locks cannot be trusted on this volume.
    #[error("lock unreliable")]
    LockUnreliable,
    /// The archive was written by a newer version of the app.
    #[error("archive newer")]
    NewerSchema { found: u32 },
    /// The archive has an older schema; only an explicit migration proceeds.
    #[error("archive needs upgrade")]
    NeedsMigration { found: u32 },
    /// The archive was written by newer derivation or decoder rules, so this
    /// build may read and verify it but not import into it.
    #[error("newer rules")]
    NewerRules,
    /// The directory or file is not an archive.
    #[error("not an archive")]
    NotAnArchive,
    /// The archive's structure cannot be trusted.
    #[error("archive damaged")]
    Damaged(DamageKind),
    /// A permission or link check failed.
    #[error("permissions")]
    Permissions(PermProblem),
    /// The location is on a network volume (D54).
    #[error("network volume")]
    NetworkVolume,
    /// The location is on a virtualised shared folder (D54).
    #[error("virtualised volume")]
    VirtualisedVolume,
    /// The location was refused.
    #[error("location refused")]
    Location(LocationRefusal),
    /// Reading the backup or its staged sms.db failed. The error carries
    /// kinds and codes only.
    #[error("messages")]
    Messages(#[source] oma_messages::MessagesError),
    /// An import named a lineage this archive does not have.
    #[error("unknown lineage")]
    UnknownLineage,
    /// A SQLite error. `code` is the primary result code only; the error
    /// text is never kept.
    #[error("SQLite error")]
    Sqlite { code: Option<i32>, stage: Stage },
    /// An attachment file could not be opened (`open_blob`).
    #[error("attachment file unavailable")]
    Blob(BlobProblem),
    /// A file-system error.
    #[error("I/O")]
    Io {
        op: IoOp,
        kind: io::ErrorKind,
        os: Option<i32>,
    },
    /// The work was cancelled.
    #[error("cancelled")]
    Cancelled,
    /// A bug.
    #[error("internal")]
    Internal(&'static str),
}

impl ArchiveError {
    pub(crate) fn io(op: IoOp, e: &io::Error) -> Self {
        ArchiveError::Io {
            op,
            kind: e.kind(),
            os: e.raw_os_error(),
        }
    }

    pub(crate) fn sqlite(stage: Stage, e: &rusqlite::Error) -> Self {
        ArchiveError::Sqlite {
            code: primary_code(e),
            stage,
        }
    }
}

/// The primary result code of a SQLite error, if it is one.
pub(crate) fn primary_code(e: &rusqlite::Error) -> Option<i32> {
    match e {
        rusqlite::Error::SqliteFailure(f, _) => Some(f.extended_code & 0xff),
        _ => None,
    }
}

/// `SQLITE_INTERRUPT`.
pub(crate) const SQLITE_INTERRUPT: i32 = 9;
/// `SQLITE_NOTADB`.
pub(crate) const SQLITE_NOTADB: i32 = 26;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_carries_no_detail() {
        let e = ArchiveError::Sqlite {
            code: Some(11),
            stage: Stage::Read,
        };
        assert_eq!(e.to_string(), "SQLite error");
        let e = ArchiveError::io(IoOp::Rename, &io::Error::from(io::ErrorKind::NotFound));
        assert_eq!(e.to_string(), "I/O");
        assert!(matches!(
            e,
            ArchiveError::Io {
                kind: io::ErrorKind::NotFound,
                ..
            }
        ));
    }
}
