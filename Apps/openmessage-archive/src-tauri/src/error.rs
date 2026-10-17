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
