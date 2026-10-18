//! Every sentence of the archive commands and jobs, and the mapping from
//! `oma_archive` errors and outcomes to IPC errors (spec §15.4).
//!
//! The crate's errors carry kinds and codes, never content; the shell
//! writes the sentences. No sentence names an archive id, a label, a path
//! or a value from an archive.

use std::io;

use oma_archive::{ArchiveError, LocationRefusal, NotImportable, StopReason};
use oma_backup::Limits;

use crate::backups::messages as bm;
use crate::error::{ErrorCode, IpcError};

pub const ARCHIVE_LOCKED: &str = "This archive is in use by another program, or by another copy of OpenMessage Archive. Close it there, then try again.";
pub const ARCHIVE_NEWER: &str =
    "This archive was made by a newer version of OpenMessage Archive. Update the app to open it.";
pub const ARCHIVE_NEEDS_UPGRADE: &str = "This archive was made by an older version of OpenMessage Archive and must be upgraded before it can be used. This version cannot upgrade it yet.";
pub const ARCHIVE_NEWER_RULES: &str = "This archive was written by a newer version of OpenMessage Archive. It can be read and verified, but this version cannot import into it. Update the app to import.";
pub const ARCHIVE_DAMAGED: &str = "This archive's database is damaged or was changed by another program, so it cannot be trusted. It was left as it is.";
pub const NOT_AN_ARCHIVE: &str = "This folder is not an OpenMessage Archive archive.";
pub const ARCHIVE_NOT_FOUND: &str =
    "That archive is no longer in your archive folder. Refresh the list.";
pub const ARCHIVE_PERMISSIONS: &str = "This archive, or your archive folder, can be changed by other users of this computer, belongs to another user, or is a link. Fix its permissions or choose another archive folder.";
pub const LOCK_UNRELIABLE: &str = "File locking does not work reliably on the drive that holds your archive folder. Choose an archive folder on a local drive.";
pub const NETWORK_VOLUME: &str = "Your archive folder is on a network drive. Archives must be on a local drive, because file locking and the database journal are not reliable over a network.";
pub const VIRTUALISED_VOLUME: &str = "Your archive folder is on a virtual machine's shared folder. Archives must be on a local drive, because file locking and the database journal are not reliable there.";
pub const INSUFFICIENT_SPACE: &str = "There is not enough free space in your archive folder. Free some space, then continue the import.";
pub const SOURCE_CHANGED: &str = "The backup changed while it was being imported. Wait until no backup is in progress, then continue the import.";
pub const SOURCE_TOO_LARGE: &str = "This backup holds more records than this version can import (more than 4,194,304 in one table, or in Recently Deleted). Nothing was imported.";
pub const JOB_NOT_WAITING: &str = "That import is not waiting for confirmation. It may have finished, been cancelled, or timed out after two minutes.";
pub const INVALID_LABEL: &str = "Archive names can be up to 200 bytes long and cannot contain control or text-direction characters.";
pub const INVALID_ARCHIVE_ID: &str =
    "That is not an archive in your archive folder. Refresh the list.";
pub const LINEAGE_REQUIRED: &str = "Choose whether this backup is from the same phone as an earlier import, or from a different phone.";
pub const UNKNOWN_LINEAGE: &str = "That import set is not in this archive. Choose again.";
pub const BUSY_ARCHIVE: &str =
    "This archive is already being imported into or verified. Wait for it to finish or cancel it.";
pub const NOT_CONFIGURED: &str = "Choose an archive folder first. Archives are created there.";
pub const LIBRARY_NOT_A_DIRECTORY: &str =
    "Your archive folder is not a folder any more. Choose it again.";
pub const LIBRARY_ROOT: &str =
    "Your archive folder is the top of a drive. Choose a folder inside it.";
pub const LIBRARY_INSIDE_BACKUP: &str =
    "Your archive folder is inside an iPhone backup or a backup store. Choose a folder elsewhere.";
pub const CREATE_SPACE: &str =
    "There is not enough free space in your archive folder to create an archive.";
pub const MIGRATION_SPACE: &str =
    "There is not enough free space in your archive folder to upgrade this archive.";
pub const ARCHIVE_IO: &str = "The archive could not be read or written. Check that the drive is connected and has free space, then try again.";
pub const NOT_IMPORTABLE_BACKUP: &str = "This backup did not pass validation, so it was not imported. Validate it on the Backups page to see why.";
pub const NOT_IMPORTABLE_SCHEMA: &str =
    "The backup's Messages database is too large or complex to read safely. Nothing was imported.";
pub const NOT_IMPORTABLE_UNSUPPORTED: &str = "The backup's Messages database uses a layout this version does not support. Nothing was imported.";
pub const NOT_IMPORTABLE_DAMAGED: &str =
    "The backup's Messages database is damaged. Nothing was imported.";
pub const NOT_IMPORTABLE_INCOMPLETE: &str =
    "The backup's Messages database could not be read completely. Nothing was imported.";
pub const NOT_IMPORTABLE_OTHER: &str =
    "This backup cannot be imported by this version. Nothing was imported.";

/// Every sentence here, for the event-size guard.
pub const ALL: &[&str] = &[
    ARCHIVE_LOCKED,
    ARCHIVE_NEWER,
    ARCHIVE_NEEDS_UPGRADE,
    ARCHIVE_NEWER_RULES,
    ARCHIVE_DAMAGED,
    NOT_AN_ARCHIVE,
    ARCHIVE_NOT_FOUND,
    ARCHIVE_PERMISSIONS,
    LOCK_UNRELIABLE,
    NETWORK_VOLUME,
    VIRTUALISED_VOLUME,
    INSUFFICIENT_SPACE,
    SOURCE_CHANGED,
    SOURCE_TOO_LARGE,
    JOB_NOT_WAITING,
    INVALID_LABEL,
    INVALID_ARCHIVE_ID,
    LINEAGE_REQUIRED,
    UNKNOWN_LINEAGE,
    BUSY_ARCHIVE,
    NOT_CONFIGURED,
    LIBRARY_NOT_A_DIRECTORY,
    LIBRARY_ROOT,
    LIBRARY_INSIDE_BACKUP,
    CREATE_SPACE,
    MIGRATION_SPACE,
    ARCHIVE_IO,
    NOT_IMPORTABLE_BACKUP,
    NOT_IMPORTABLE_SCHEMA,
    NOT_IMPORTABLE_UNSUPPORTED,
    NOT_IMPORTABLE_DAMAGED,
    NOT_IMPORTABLE_INCOMPLETE,
    NOT_IMPORTABLE_OTHER,
];

/// The longest sentence (for the 1 KiB event guard).
pub fn longest_sentence() -> &'static str {
    ALL.iter().copied().max_by_key(|s| s.len()).unwrap_or("")
}

/// `SQLITE_FULL`.
const SQLITE_FULL: i32 = 13;

/// An `oma_archive` error as an IPC error. `limits` words the errors that
/// came from reading the backup.
pub fn to_ipc(e: &ArchiveError, limits: &Limits) -> IpcError {
    use ErrorCode as C;
    let (code, msg) = match e {
        ArchiveError::Locked => (C::ArchiveLocked, ARCHIVE_LOCKED),
        ArchiveError::LockUnreliable => (C::LockUnreliable, LOCK_UNRELIABLE),
        ArchiveError::NewerSchema { .. } => (C::ArchiveNewer, ARCHIVE_NEWER),
        ArchiveError::NeedsMigration { .. } => (C::ArchiveNeedsUpgrade, ARCHIVE_NEEDS_UPGRADE),
        ArchiveError::NewerRules => (C::ArchiveNewerRules, ARCHIVE_NEWER_RULES),
        ArchiveError::NotAnArchive => (C::NotAnArchive, NOT_AN_ARCHIVE),
        ArchiveError::Damaged(_) => (C::ArchiveDamaged, ARCHIVE_DAMAGED),
        ArchiveError::Permissions(_) => (C::ArchivePermissions, ARCHIVE_PERMISSIONS),
        ArchiveError::NetworkVolume => (C::NetworkVolume, NETWORK_VOLUME),
        ArchiveError::VirtualisedVolume => (C::VirtualisedVolume, VIRTUALISED_VOLUME),
        ArchiveError::Location(r) => match r {
            LocationRefusal::NotADirectory => (C::NotConfigured, LIBRARY_NOT_A_DIRECTORY),
            LocationRefusal::FilesystemRoot => (C::NotConfigured, LIBRARY_ROOT),
            LocationRefusal::InsideBackup => (C::NotConfigured, LIBRARY_INSIDE_BACKUP),
            LocationRefusal::InsufficientSpace => (C::InsufficientSpace, CREATE_SPACE),
            LocationRefusal::InsufficientSpaceForMigration => {
                (C::InsufficientSpace, MIGRATION_SPACE)
            }
            _ => (C::NotConfigured, NOT_CONFIGURED),
        },
        ArchiveError::Messages(m) => return bm::messages_to_ipc(m, limits),
        ArchiveError::UnknownLineage => (C::InvalidInput, UNKNOWN_LINEAGE),
        ArchiveError::Sqlite {
            code: Some(SQLITE_FULL),
            ..
        } => (C::InsufficientSpace, INSUFFICIENT_SPACE),
        ArchiveError::Sqlite { code, .. } => {
            return IpcError::new(
                C::Io,
                match code {
                    Some(n) => format!("The archive database reported an error (SQLite error {n}). Nothing after the last completed step was written."),
                    None => "The archive database reported an error. Nothing after the last completed step was written.".to_owned(),
                },
            )
        }
        ArchiveError::Blob(_) => (C::Io, ARCHIVE_IO),
        ArchiveError::Io { kind, .. } => match kind {
            io::ErrorKind::NotFound => (C::ArchiveNotFound, ARCHIVE_NOT_FOUND),
            io::ErrorKind::StorageFull => (C::InsufficientSpace, INSUFFICIENT_SPACE),
            io::ErrorKind::PermissionDenied => (C::ArchivePermissions, ARCHIVE_PERMISSIONS),
            _ => (C::Io, ARCHIVE_IO),
        },
        ArchiveError::Cancelled => return IpcError::internal("cancel reported as an error"),
        ArchiveError::Internal(context) => return IpcError::internal(context),
        // `ArchiveError` is non-exhaustive.
        _ => return IpcError::internal("unknown archive error"),
    };
    IpcError::new(code, msg)
}

/// The sentence of a stop (state 6): resumable, never "failed".
pub fn stop_notice(r: StopReason) -> IpcError {
    match r {
        StopReason::SourceChanged => IpcError::new(ErrorCode::SourceChanged, SOURCE_CHANGED),
        _ => IpcError::new(ErrorCode::InsufficientSpace, INSUFFICIENT_SPACE),
    }
}

/// The sentence of a backup that cannot be imported.
pub fn not_importable_notice(n: NotImportable) -> IpcError {
    use ErrorCode as C;
    let (code, msg) = match n {
        NotImportable::SchemaExceedsLimits => (C::SourceTooLarge, NOT_IMPORTABLE_SCHEMA),
        NotImportable::OverKeyScanCap(_) | NotImportable::OverRecentlyDeletedCap => {
            (C::SourceTooLarge, SOURCE_TOO_LARGE)
        }
        NotImportable::Unsupported => (C::UnsupportedBackup, NOT_IMPORTABLE_UNSUPPORTED),
        NotImportable::Damaged => (C::MalformedBackup, NOT_IMPORTABLE_DAMAGED),
        NotImportable::Incomplete => (C::MalformedBackup, NOT_IMPORTABLE_INCOMPLETE),
        _ => (C::UnsupportedBackup, NOT_IMPORTABLE_OTHER),
    };
    IpcError::new(code, msg)
}

/// The sentence of a backup whose validation failed.
pub fn backup_not_importable_notice() -> IpcError {
    IpcError::new(ErrorCode::UnsupportedBackup, NOT_IMPORTABLE_BACKUP)
}

/// The fixed sentence of each archive error code (spec §15.4).
pub fn sentence(code: ErrorCode) -> Option<&'static str> {
    use ErrorCode as C;
    Some(match code {
        C::ArchiveLocked => ARCHIVE_LOCKED,
        C::ArchiveNewer => ARCHIVE_NEWER,
        C::ArchiveNeedsUpgrade => ARCHIVE_NEEDS_UPGRADE,
        C::ArchiveNewerRules => ARCHIVE_NEWER_RULES,
        C::ArchiveDamaged => ARCHIVE_DAMAGED,
        C::NotAnArchive => NOT_AN_ARCHIVE,
        C::ArchiveNotFound => ARCHIVE_NOT_FOUND,
        C::ArchivePermissions => ARCHIVE_PERMISSIONS,
        C::LockUnreliable => LOCK_UNRELIABLE,
        C::NetworkVolume => NETWORK_VOLUME,
        C::VirtualisedVolume => VIRTUALISED_VOLUME,
        C::InsufficientSpace => INSUFFICIENT_SPACE,
        C::SourceChanged => SOURCE_CHANGED,
        C::SourceTooLarge => SOURCE_TOO_LARGE,
        C::JobNotWaiting => JOB_NOT_WAITING,
        C::InvalidInput => INVALID_ARCHIVE_ID,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oma_archive::{BlobProblem, DamageKind, IoOp, KeyScanTable, PermProblem, Stage};

    /// Every `ArchiveError` variant this build knows maps to its code,
    /// with a sentence that names nothing from the archive.
    #[test]
    fn every_archive_error_maps_to_its_code() {
        use ErrorCode as C;
        let limits = Limits::default();
        let io = |kind| ArchiveError::Io {
            op: IoOp::Metadata,
            kind,
            os: None,
        };
        let cases: Vec<(ArchiveError, ErrorCode)> = vec![
            (ArchiveError::Locked, C::ArchiveLocked),
            (ArchiveError::LockUnreliable, C::LockUnreliable),
            (ArchiveError::NewerSchema { found: 9 }, C::ArchiveNewer),
            (
                ArchiveError::NeedsMigration { found: 0 },
                C::ArchiveNeedsUpgrade,
            ),
            (ArchiveError::NewerRules, C::ArchiveNewerRules),
            (ArchiveError::NotAnArchive, C::NotAnArchive),
            (
                ArchiveError::Damaged(DamageKind::SchemaMismatch),
                C::ArchiveDamaged,
            ),
            (
                ArchiveError::Permissions(PermProblem::Link),
                C::ArchivePermissions,
            ),
            (ArchiveError::NetworkVolume, C::NetworkVolume),
            (ArchiveError::VirtualisedVolume, C::VirtualisedVolume),
            (
                ArchiveError::Location(LocationRefusal::InsideBackup),
                C::NotConfigured,
            ),
            (
                ArchiveError::Location(LocationRefusal::NotADirectory),
                C::NotConfigured,
            ),
            (
                ArchiveError::Location(LocationRefusal::FilesystemRoot),
                C::NotConfigured,
            ),
            (
                ArchiveError::Location(LocationRefusal::InsufficientSpace),
                C::InsufficientSpace,
            ),
            (
                ArchiveError::Location(LocationRefusal::InsufficientSpaceForMigration),
                C::InsufficientSpace,
            ),
            (
                ArchiveError::Messages(oma_messages::MessagesError::Cancelled),
                C::Internal,
            ),
            (ArchiveError::UnknownLineage, C::InvalidInput),
            (
                ArchiveError::Sqlite {
                    code: Some(13),
                    stage: Stage::Batch,
                },
                C::InsufficientSpace,
            ),
            (
                ArchiveError::Sqlite {
                    code: Some(11),
                    stage: Stage::Read,
                },
                C::Io,
            ),
            (
                ArchiveError::Sqlite {
                    code: None,
                    stage: Stage::Read,
                },
                C::Io,
            ),
            (ArchiveError::Blob(BlobProblem::Missing), C::Io),
            (io(io::ErrorKind::NotFound), C::ArchiveNotFound),
            (io(io::ErrorKind::StorageFull), C::InsufficientSpace),
            (io(io::ErrorKind::PermissionDenied), C::ArchivePermissions),
            (io(io::ErrorKind::Other), C::Io),
            (ArchiveError::Cancelled, C::Internal),
            (ArchiveError::Internal("x"), C::Internal),
        ];
        for (e, code) in cases {
            let ipc = to_ipc(&e, &limits);
            assert_eq!(ipc.code, code, "{e:?}");
            assert!(!ipc.message.is_empty());
            assert!(!ipc.message.contains('/') && !ipc.message.contains('\\'));
        }
        assert!(to_ipc(
            &ArchiveError::Sqlite {
                code: Some(11),
                stage: Stage::Read
            },
            &limits
        )
        .message
        .contains("SQLite error 11"));
    }

    #[test]
    fn every_outcome_has_a_notice() {
        use ErrorCode as C;
        for (n, code) in [
            (NotImportable::SchemaExceedsLimits, C::SourceTooLarge),
            (NotImportable::Unsupported, C::UnsupportedBackup),
            (NotImportable::Damaged, C::MalformedBackup),
            (NotImportable::Incomplete, C::MalformedBackup),
            (
                NotImportable::OverKeyScanCap(KeyScanTable::Messages),
                C::SourceTooLarge,
            ),
            (NotImportable::OverRecentlyDeletedCap, C::SourceTooLarge),
        ] {
            assert_eq!(not_importable_notice(n).code, code, "{n:?}");
        }
        assert_eq!(
            stop_notice(StopReason::SourceChanged).code,
            C::SourceChanged
        );
        assert_eq!(
            stop_notice(StopReason::InsufficientSpace).code,
            C::InsufficientSpace
        );
        assert_eq!(backup_not_importable_notice().code, C::UnsupportedBackup);
    }

    /// Each new code has its fixed sentence, and the sentences are
    /// distinct.
    #[test]
    fn every_archive_code_has_a_sentence() {
        use ErrorCode as C;
        let codes = [
            C::ArchiveLocked,
            C::ArchiveNewer,
            C::ArchiveNeedsUpgrade,
            C::ArchiveNewerRules,
            C::ArchiveDamaged,
            C::NotAnArchive,
            C::ArchiveNotFound,
            C::ArchivePermissions,
            C::LockUnreliable,
            C::NetworkVolume,
            C::VirtualisedVolume,
            C::InsufficientSpace,
            C::SourceChanged,
            C::SourceTooLarge,
            C::JobNotWaiting,
            C::InvalidInput,
        ];
        let mut seen = std::collections::HashSet::new();
        for c in codes {
            let s = sentence(c).expect("sentence");
            assert!(seen.insert(s), "{c:?}");
            assert!(ALL.contains(&s));
        }
        assert_eq!(sentence(C::Busy), None);
        let all: std::collections::HashSet<&str> = ALL.iter().copied().collect();
        assert_eq!(all.len(), ALL.len(), "sentences are distinct");
    }
}
