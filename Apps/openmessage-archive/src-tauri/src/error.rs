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
}

#[derive(Debug, Clone, Serialize, TS, thiserror::Error)]
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
