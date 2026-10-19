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
//! [`decode_attributed_body`] is a clean-room, budgeted, fail-closed decoder
//! for the typedstream blobs in `message.attributedBody`. It returns the text
//! with its runs and parts, or one of a closed set of [`TsErrorKind`]s.
//!
//! [`SmsDb::for_each_message`] reads each message with its body (the `text`
//! column and the decoded `attributedBody`, both kept), its association
//! (tapbacks, stickers, poll votes), its reply facts, its parsed
//! `message_summary_info` and a derived [`EditLabel`]. [`GuidSets`] counts
//! reply originators and association targets that match no message.
//!
//! [`SmsDb::for_each_attachment`] classifies each `attachment.filename`,
//! looks the two sourced classes up in Manifest.db, and probes the file it
//! finds without extracting it ([`AttachmentLocator`]). The aggregates
//! (orphans, chat membership, unmatched handles) run as integer SQL under op
//! budgets.
//!
//! [`check`] runs the whole extraction check on an opened backup and returns
//! an [`ExtractionReport`]: aggregates only, with no string from the
//! database.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod aggregates;
mod assoc;
mod attachments;
mod caps;
mod check;
mod codes;
mod col;
mod db;
mod error;
mod guidset;
mod progress;
mod raw;
mod records;
mod redact;
mod report;
mod schema;
mod summary;
mod time;
mod typedstream;
mod value;

pub use aggregates::{aggregate_queries, AggregateId, Aggregates, AGG_OPS_BASE, AGG_OPS_PER_ROW};
pub use assoc::{AssocRaw, AssocTarget, Association};
pub use attachments::{
    classify as classify_attachment_path, AttachmentLocation, AttachmentLocator, LookupOutcome,
    NotAttemptedReason, PathClass, ATTACHMENT_LOOKUP_DOMAIN,
};
pub use caps::{
    CAP_ATTRIBUTED_BODY, CAP_GUID, CAP_IDENTIFIER, CAP_OTHER_TEXT, CAP_PAYLOAD_DATA,
    CAP_PROPERTIES, CAP_RAW_UNREADABLE, CAP_SUMMARY, CAP_TEXT, INTEGER_BYTES, ROW_BYTES_BUDGET,
};
pub use check::{check, CheckOptions};
pub use codes::{AssociatedKind, ItemKind, Reaction, ServiceClass};
pub use col::{Col, ColShape, IssueKind};
pub use db::{
    connect, stage, Connect, Cursor, PassOutcome, ProbeOptions, SmsDb, PAGE_ROWS_DEFAULT,
    PAGE_ROWS_MAX, PAGE_ROWS_MIN, SMS_DB_LIMITS, SMS_READ_POLICY,
};
pub use error::{BackupStage, MessagesError, Stage};
pub use guidset::{GuidCounts, GuidSets, GUID_SET_CAP};
pub use progress::{MessagesCtx, MessagesPhase, MessagesProgress, MessagesSink};
pub use raw::{RawIssue, RawSql};
pub use records::{
    AttachmentRecord, AttributedOutcome, Body, ChatHandleLink, ChatMessageLink, ChatRecord,
    EditLabel, HandleKeyRow, HandleRecord, KeyRow, MessageAttachmentLink, MessageReadOptions,
    MessageRecord, RecoverableLink, ReplyInfo,
};
pub use redact::{ByteLen, RawBlob, Redacted, SqlText, TextClass};
pub use report::{
    AssociationCounts, AttachmentCounts, AttributedState, BodyCounts, ChatCounts, DateCounts,
    DbFacts, EditLabelCounts, ExtractionReport, ExtractionStatus, FailureCounts, HandleCounts,
    IntegrityOutcome, ItemKindCounts, LinkCounts, LookupCounts, MessageCounts, ProbeCounts,
    Qualifier, RawHistogram, ReactionCounts, RecentlyDeletedCounts, SchemaReport, ServiceCounts,
    SummaryCounts, TableReport, TargetCounts, TextState, UnitHistogram, WalReport, HISTOGRAM_CAP,
    RANGE_FLOOR_UNIX_NS,
};
pub use schema::{
    Capabilities, ColumnKind, DamageReason, Features, IncompleteReason, KnownColumn,
    KnownColumnSet, KnownTable, Probe, TableCaps, Unsupported, UnsupportedReason,
};
pub use summary::{
    edit_texts, EditBody, EditEntry, EditText, EditTextsError, EditedPart, FieldOutcome,
    ShapeError, SummaryInfo, SUMMARY_ARRAY_CAP, SUMMARY_MAX_DEPTH,
};
pub use time::{
    CocoaTime, SummaryTime, SummaryTimeRaw, TimeInterp, COCOA_EPOCH_UNIX_S, NS_MAX, NS_THRESHOLD,
    SECONDS_MAX,
};
pub use typedstream::{
    decode_attributed_body, decode_attributed_body_with_stats, AttrFlag, AttrFlags, AttrKey,
    AttrKeysPresent, AttributedBody, BodyPart, BodyRun, RunAttrs, RunIssue, SharedValue, TsBudget,
    TsError, TsErrorKind, TsStats, ValueId,
};
pub use value::{BlobSize, SqlType, ValueIssues};

/// Re-exported so callers can name the encoding a database declares.
pub use oma_backup::TextEncoding;
