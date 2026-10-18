//! Reading the iPhone Messages database (sms.db) of an opened backup, for
//! OpenMessage Archive. The crate does not depend on Tauri, and it persists
//! nothing.
//!
//! Given an [`oma_backup::OpenBackup`] that has been opened and validated:
//!
//! 1. [`stage`] copies sms.db (and its `-wal`, best effort) into staging;
//! 2. [`connect`] opens the staged copy hardened, with [`SMS_DB_LIMITS`]
//!    and [`SMS_READ_POLICY`] (a read-only authorizer that also denies
//!    `recoverable_message_part.part_text`);
//! 3. [`SmsDb::open`] scans the schema without connecting any virtual table,
//!    runs `quick_check` under an op budget, and probes every known table by
//!    capability. Unsupported, damaged and incomplete databases are values
//!    ([`Probe`]), not errors;
//! 4. the `for_each_*` passes stream typed records, page by page, through a
//!    callback that may stop with its own value. A pass resumes from
//!    [`Cursor::After`] the last rowid it delivered.
//!
//! Every source field is a [`Col`], which keeps "the column is absent",
//! "the value is NULL", "the value is unreadable" and "the value was withheld
//! because it is over its cap" apart. Content and identifiers are held as
//! [`Redacted`] bytes whose `Debug` prints only a length.
//!
//! The message, attachment and aggregate passes, the typedstream decoder and
//! the report land in later slices of Milestone 3.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod caps;
mod codes;
mod col;
mod db;
mod error;
mod progress;
mod records;
mod redact;
mod schema;
mod time;
mod value;

pub use caps::{
    CAP_ATTRIBUTED_BODY, CAP_GUID, CAP_IDENTIFIER, CAP_OTHER_TEXT, CAP_PROPERTIES, CAP_SUMMARY,
    CAP_TEXT, INTEGER_BYTES, ROW_BYTES_BUDGET,
};
pub use codes::ServiceClass;
pub use col::{Col, ColShape, IssueKind};
pub use db::{
    connect, stage, Connect, Cursor, PassOutcome, ProbeOptions, SmsDb, PAGE_ROWS_DEFAULT,
    PAGE_ROWS_MAX, PAGE_ROWS_MIN, SMS_DB_LIMITS, SMS_READ_POLICY,
};
pub use error::{BackupStage, MessagesError, Stage};
pub use progress::{MessagesCtx, MessagesPhase, MessagesProgress, MessagesSink};
pub use records::{
    ChatHandleLink, ChatMessageLink, ChatRecord, HandleRecord, MessageAttachmentLink,
    RecoverableLink,
};
pub use redact::{ByteLen, RawBlob, Redacted, SqlText};
pub use schema::{
    Capabilities, ColumnKind, DamageReason, Features, IncompleteReason, KnownColumn,
    KnownColumnSet, KnownTable, Probe, TableCaps, Unsupported, UnsupportedReason,
};
pub use time::{CocoaTime, TimeInterp, COCOA_EPOCH_UNIX_S, NS_MAX, NS_THRESHOLD, SECONDS_MAX};
pub use value::{BlobSize, SqlType, ValueIssues};

/// Re-exported so callers can name the encoding a database declares.
pub use oma_backup::TextEncoding;
