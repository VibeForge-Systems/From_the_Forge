//! The error type every IPC command returns.
//!
//! Errors cross the IPC boundary as `{ code, message }`. The code is a stable,
//! machine-readable identifier the frontend can branch on; the message is a
//! human sentence safe to display. Neither may carry message bodies,
//! passwords, or attachment contents — only the kind of failure and, where it
//! helps the user act, a path they chose themselves.

use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ErrorCode {
    /// The settings file could not be written, or is held read-only because
    /// it was written by a newer version.
    SettingsReadOnly,
    /// A filesystem operation failed.
    Io,
    /// A required setting (such as the archive location) is not configured.
    NotConfigured,
    /// The native dialog or OS integration failed.
    Platform,
    /// An invariant inside the application was violated. Always a bug.
    Internal,
    /// An unknown or stale backup, location or job id, or a backup folder
    /// that has vanished.
    BackupNotFound,
    /// The folder is not an iPhone backup. Reserved for the library mapping;
    /// folder-picker refusals travel as `AddBackupFolderResult.outcome`.
    NotABackup,
    /// The backup was recognised but uses a layout or encryption scheme this
    /// version does not read.
    UnsupportedBackup,
    /// The backup did not finish (SnapshotState is not `finished`, or
    /// Status.plist is missing).
    IncompleteBackup,
    /// The backup's content is damaged or hostile.
    MalformedBackup,
    /// The backup is encrypted and no password was given.
    PasswordRequired,
    /// The password is longer than 1,024 bytes as UTF-8. It is refused, never
    /// truncated.
    PasswordTooLong,
    /// The password did not unlock the backup's first class key.
    WrongPassword,
    /// A backup check or a listing is already running.
    Busy,
    // Milestone 4: archives (spec §15.4). Each has a fixed sentence in
    // `archives::messages`.
    /// Another program, or another copy of the app, holds the archive.
    ArchiveLocked,
    /// The archive was written by a newer version of the app.
    ArchiveNewer,
    /// The archive has an older schema and needs an upgrade first.
    ArchiveNeedsUpgrade,
    /// The archive was written by newer rules: readable, not importable.
    ArchiveNewerRules,
    /// The archive's structure cannot be trusted.
    ArchiveDamaged,
    /// The folder is not an archive.
    NotAnArchive,
    /// No archive with that id in the archive folder.
    ArchiveNotFound,
    /// The archive, or the archive folder, failed the permission or link
    /// checks.
    ArchivePermissions,
    /// Locks cannot be trusted on the archive folder's volume.
    LockUnreliable,
    /// The archive folder is on a network volume (D54).
    NetworkVolume,
    /// The archive folder is on a virtualised shared folder (D54).
    VirtualisedVolume,
    /// Not enough free space in the archive folder.
    InsufficientSpace,
    /// The backup changed while it was read.
    SourceChanged,
    /// The backup is larger than the import can take (the key-scan or
    /// Recently Deleted caps).
    SourceTooLarge,
    /// `confirm_import` for a job that is not waiting at the gate.
    JobNotWaiting,
    /// An input failed validation (a label, an id, a lineage).
    InvalidInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS, thiserror::Error)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
#[error("{message}")]
pub struct IpcError {
    pub code: ErrorCode,
    pub message: String,
}

impl IpcError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn internal(context: &str) -> Self {
        tracing::error!(context, "internal error");
        Self::new(
            ErrorCode::Internal,
            format!("Internal error ({context}). Please report this as a bug."),
        )
    }
}

pub type IpcResult<T> = Result<T, IpcError>;
