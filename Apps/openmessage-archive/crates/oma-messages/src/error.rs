//! Errors. Like `oma_backup::BackupError`, they carry kinds and stages,
//! never content: no SQL text, no SQLite message, no table or column name
//! from the database.
//!
//! Not errors (they are values elsewhere): an unsupported, damaged or
//! incomplete schema check ([`crate::Probe`]); unreadable or oversized
//! values ([`crate::Col`]); a skipped `-wal`.

use oma_backup::BackupError;

/// Everything that stops a messages check.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MessagesError {
    /// A backup operation failed.
    #[error("backup")]
    Backup {
        /// The backup error, which carries kinds only.
        #[source]
        source: BackupError,
        /// What was being done with the backup.
        stage: BackupStage,
    },
    /// SQLite reported an error on the staged sms.db. Only the primary
    /// result code is kept.
    #[error("SQLite error")]
    Sqlite {
        /// The primary result code, if there was one.
        code: Option<i32>,
        /// Which step of the check ran the statement.
        stage: Stage,
    },
    /// The job was cancelled.
    #[error("cancelled")]
    Cancelled,
    /// A condition that should be impossible.
    #[error("internal")]
    Internal(&'static str),
}

/// What was being done with the backup when a [`MessagesError::Backup`]
/// happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BackupStage {
    /// Copying sms.db (and its `-wal`) into staging.
    Staging,
    /// Opening the staged copy.
    Opening,
    /// Looking an attachment up in Manifest.db.
    ManifestLookup,
    /// Probing an attachment's file in the backup.
    AttachmentProbe,
    /// Removing the staged copy.
    Removing,
}

/// The step of the check that ran a failing SQLite statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Stage {
    /// The `sqlite_schema` scan.
    Scan,
    /// `quick_check`.
    Integrity,
    /// The schema probe and the row counts.
    Probe,
    /// The `handle` pass.
    Handles,
    /// The `chat` pass.
    Chats,
    /// The `chat_handle_join` and `chat_message_join` passes.
    ChatLinks,
    /// The `message` pass.
    Messages,
    /// The `message_attachment_join` and `attachment` passes.
    Attachments,
    /// The `chat_recoverable_message_join` pass.
    Recoverable,
    /// The aggregates.
    Aggregates,
}

impl MessagesError {
    /// Map an error from a statement on the staged connection. Cancellation
    /// stays cancellation; SQLite errors keep their code and gain a stage.
    pub(crate) fn from_conn(e: BackupError, stage: Stage) -> Self {
        match e {
            BackupError::Cancelled => MessagesError::Cancelled,
            BackupError::Sqlite { code } => MessagesError::Sqlite { code, stage },
            BackupError::Internal(what) => MessagesError::Internal(what),
            other => MessagesError::Backup {
                source: other,
                stage: BackupStage::Opening,
            },
        }
    }

    /// Map an error from a backup operation.
    pub(crate) fn from_backup(e: BackupError, stage: BackupStage) -> Self {
        match e {
            BackupError::Cancelled => MessagesError::Cancelled,
            other => MessagesError::Backup {
                source: other,
                stage,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_errors_keep_their_code_and_gain_a_stage() {
        assert!(matches!(
            MessagesError::from_conn(BackupError::Cancelled, Stage::Handles),
            MessagesError::Cancelled
        ));
        assert!(matches!(
            MessagesError::from_conn(BackupError::Sqlite { code: Some(11) }, Stage::Chats),
            MessagesError::Sqlite {
                code: Some(11),
                stage: Stage::Chats
            }
        ));
        assert!(matches!(
            MessagesError::from_backup(BackupError::Cancelled, BackupStage::Staging),
            MessagesError::Cancelled
        ));
        assert!(matches!(
            MessagesError::from_backup(BackupError::SourceChanged, BackupStage::Staging),
            MessagesError::Backup {
                source: BackupError::SourceChanged,
                stage: BackupStage::Staging
            }
        ));
    }
}
