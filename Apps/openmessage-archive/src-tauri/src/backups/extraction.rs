//! The extraction check's IPC types (M3 §13, §16.3, §16.4) and the mapping
//! from `oma_messages::ExtractionReport` into them.
//!
//! The report crosses IPC as counts, closed kinds and three dates; there is
//! no free-text string in it (I7), and no name from the database. Each kind
//! is a camelCase string literal, so the frontend can label it from an
//! exhaustive table.
//!
//! - Every `u64` is clamped to `Number.MAX_SAFE_INTEGER`; a clamp sets
//!   `saturated`. An `Option<u64>` counter keeps `null` for "not recorded by
//!   this database" and is clamped like any other value when it is present,
//!   because turning a huge count into `null` would claim it was not
//!   recorded.
//! - Every raw `i64` (only `clientVersion` today) is checked against
//!   ±(2^53 − 1). An out-of-range value is never sent as a number; the DTO
//!   sets `clientVersionOutOfSafeRange` instead.
//! - The date range is the only data-derived value besides counts. It is
//!   sent as RFC 3339 strings, kept in memory only, and never logged.
//!
//! This slice carries what the must-ship report sections render (§17.4).
//! The deferred sections (services, reactions, replies, edits, the body
//! breakdown, raw-value histograms) stay in the Rust report until their UI
//! lands (§23.2).

use std::time::{Duration, UNIX_EPOCH};

use oma_backup::{OpenMode, TextEncoding, WalOutcome};
use oma_messages::{
    DamageReason, ExtractionReport, ExtractionStatus, FailureCounts, IncompleteReason,
    IntegrityOutcome, KnownColumn, KnownTable, Qualifier, TsErrorKind, UnsupportedReason,
};
use serde::Serialize;
use ts_rs::TS;

use super::dto::{clamp, millis, rfc3339, MAX_SAFE_INTEGER};

/// iOS versions of **real** backups the extraction check has been tested
/// on. Empty in M3: no real backup has been tested (§13.1). The comparison
/// runs here; only the resulting bool crosses IPC.
pub const TESTED_REAL_IOS_VERSIONS: &[&str] = &[];

/// Whether a backup's iOS version is outside [`TESTED_REAL_IOS_VERSIONS`].
/// A version that is not recorded is untested.
pub fn ios_version_untested(version: Option<&str>) -> bool {
    version.is_none_or(|v| !TESTED_REAL_IOS_VERSIONS.contains(&v))
}

/// An `i64` that fits a JavaScript number exactly, or `None`.
pub fn safe_i64(v: i64) -> Option<i64> {
    let max = i64::try_from(MAX_SAFE_INTEGER).unwrap_or(i64::MAX);
    (-max..=max).contains(&v).then_some(v)
}

/// What an extraction-check job came to, as the terminal event names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ExtractionOutcomeDto {
    /// Readable, and no qualifier applies.
    Readable,
    /// Readable, with at least one stated limit.
    ReadableWithQualifiers,
    /// The integrity check failed, or a pass streamed the wrong row count.
    Damaged,
    /// A layout this version does not support.
    Unsupported,
    /// The integrity check did not finish within its limit.
    Incomplete,
    /// Validation found errors, so the Messages database was not read.
    SkippedNotImportable,
}

/// A stated limit on a readable result (D43). `iosVersionUntested` is the
/// shell's; the others come from the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum QualifierDto {
    IosVersionUntested,
    UnknownTables {
        count: u32,
    },
    UnknownColumns {
        count: u32,
    },
    WrongCaseColumns {
        count: u32,
    },
    WalSkipped,
    WalStateUnknown,
    WalFramesInvalid,
    SourceChanged,
    AggregatesIncomplete,
    ValuesWithheld,
    NonUtf8Encoding,
    SizeMismatch,
    /// A qualifier this version of the shell cannot name.
    Other,
}

/// A table the check knows, by our own name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum KnownTableDto {
    Message,
    Handle,
    Chat,
    ChatMessageJoin,
    ChatHandleJoin,
    MessageAttachmentJoin,
    Attachment,
    ChatRecoverableMessageJoin,
    RecoverableMessagePart,
    DeletedMessages,
    SyncDeletedMessages,
    SyncDeletedChats,
    SyncDeletedAttachments,
    UnsyncedRemovedRecoverableMessages,
    SqliteDatabaseProperties,
    /// A table this version of the shell cannot name.
    Other,
}

impl From<KnownTable> for KnownTableDto {
    fn from(t: KnownTable) -> Self {
        use KnownTable as K;
        match t {
            K::Message => Self::Message,
            K::Handle => Self::Handle,
            K::Chat => Self::Chat,
            K::ChatMessageJoin => Self::ChatMessageJoin,
            K::ChatHandleJoin => Self::ChatHandleJoin,
            K::MessageAttachmentJoin => Self::MessageAttachmentJoin,
            K::Attachment => Self::Attachment,
            K::ChatRecoverableMessageJoin => Self::ChatRecoverableMessageJoin,
            K::RecoverableMessagePart => Self::RecoverableMessagePart,
            K::DeletedMessages => Self::DeletedMessages,
            K::SyncDeletedMessages => Self::SyncDeletedMessages,
            K::SyncDeletedChats => Self::SyncDeletedChats,
            K::SyncDeletedAttachments => Self::SyncDeletedAttachments,
            K::UnsyncedRemovedRecoverableMessages => Self::UnsyncedRemovedRecoverableMessages,
            K::SqliteDatabaseProperties => Self::SqliteDatabaseProperties,
            _ => Self::Other,
        }
    }
}

/// Declares `KnownColumnDto`: one string literal per known column, written
/// `<table>.<column>` in camelCase (our names, never the database's), and
/// the mapping from `oma_messages::KnownColumn` by table and column name.
macro_rules! known_columns {
    ($( $v:ident = $lit:literal => ($t:ident, $name:literal), )*) => {
        /// A known column, by our own name.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
        #[ts(export)]
        pub enum KnownColumnDto {
            $( #[serde(rename = $lit)] $v, )*
            /// A column this version of the shell cannot name.
            #[serde(rename = "other")]
            Other,
        }

        impl From<KnownColumn> for KnownColumnDto {
            fn from(c: KnownColumn) -> Self {
                match (c.table(), c.name()) {
                    $( (KnownTable::$t, $name) => Self::$v, )*
                    _ => Self::Other,
                }
            }
        }
    };
}

known_columns! {
        MessageRowid = "message.rowid" => (Message, "ROWID"),
        MessageGuid = "message.guid" => (Message, "guid"),
        MessageDate = "message.date" => (Message, "date"),
        MessageIsFromMe = "message.isFromMe" => (Message, "is_from_me"),
        MessageText = "message.text" => (Message, "text"),
        MessageAttributedBody = "message.attributedBody" => (Message, "attributedBody"),
        MessageService = "message.service" => (Message, "service"),
        MessageHandleId = "message.handleId" => (Message, "handle_id"),
        MessageOtherHandle = "message.otherHandle" => (Message, "other_handle"),
        MessageDestinationCallerId = "message.destinationCallerId" => (Message, "destination_caller_id"),
        MessageSubject = "message.subject" => (Message, "subject"),
        MessageDateRead = "message.dateRead" => (Message, "date_read"),
        MessageDateDelivered = "message.dateDelivered" => (Message, "date_delivered"),
        MessageDatePlayed = "message.datePlayed" => (Message, "date_played"),
        MessageDateEdited = "message.dateEdited" => (Message, "date_edited"),
        MessageDateRetracted = "message.dateRetracted" => (Message, "date_retracted"),
        MessageIsRead = "message.isRead" => (Message, "is_read"),
        MessageIsDelivered = "message.isDelivered" => (Message, "is_delivered"),
        MessageIsSent = "message.isSent" => (Message, "is_sent"),
        MessageIsAudioMessage = "message.isAudioMessage" => (Message, "is_audio_message"),
        MessageCacheHasAttachments = "message.cacheHasAttachments" => (Message, "cache_has_attachments"),
        MessageItemType = "message.itemType" => (Message, "item_type"),
        MessageGroupActionType = "message.groupActionType" => (Message, "group_action_type"),
        MessageGroupTitle = "message.groupTitle" => (Message, "group_title"),
        MessageShareStatus = "message.shareStatus" => (Message, "share_status"),
        MessageShareDirection = "message.shareDirection" => (Message, "share_direction"),
        MessageAssociatedMessageGuid = "message.associatedMessageGuid" => (Message, "associated_message_guid"),
        MessageAssociatedMessageType = "message.associatedMessageType" => (Message, "associated_message_type"),
        MessageAssociatedMessageEmoji = "message.associatedMessageEmoji" => (Message, "associated_message_emoji"),
        MessageAssociatedMessageRangeLocation = "message.associatedMessageRangeLocation" => (Message, "associated_message_range_location"),
        MessageAssociatedMessageRangeLength = "message.associatedMessageRangeLength" => (Message, "associated_message_range_length"),
        MessageBalloonBundleId = "message.balloonBundleId" => (Message, "balloon_bundle_id"),
        MessageExpressiveSendStyleId = "message.expressiveSendStyleId" => (Message, "expressive_send_style_id"),
        MessagePayloadData = "message.payloadData" => (Message, "payload_data"),
        MessageMessageSummaryInfo = "message.messageSummaryInfo" => (Message, "message_summary_info"),
        MessageThreadOriginatorGuid = "message.threadOriginatorGuid" => (Message, "thread_originator_guid"),
        MessageThreadOriginatorPart = "message.threadOriginatorPart" => (Message, "thread_originator_part"),
        MessageReplyToGuid = "message.replyToGuid" => (Message, "reply_to_guid"),
        MessagePartCount = "message.partCount" => (Message, "part_count"),
        MessageScheduleType = "message.scheduleType" => (Message, "schedule_type"),
        MessageScheduleState = "message.scheduleState" => (Message, "schedule_state"),
        MessageFilterAction = "message.filterAction" => (Message, "filter_action"),
        MessageFilterSubAction = "message.filterSubAction" => (Message, "filter_sub_action"),
        MessageError = "message.error" => (Message, "error"),
        MessageWasDowngraded = "message.wasDowngraded" => (Message, "was_downgraded"),
        HandleRowid = "handle.rowid" => (Handle, "ROWID"),
        HandleId = "handle.id" => (Handle, "id"),
        HandleService = "handle.service" => (Handle, "service"),
        HandleCountry = "handle.country" => (Handle, "country"),
        HandleUncanonicalizedId = "handle.uncanonicalizedId" => (Handle, "uncanonicalized_id"),
        HandlePersonCentricId = "handle.personCentricId" => (Handle, "person_centric_id"),
        ChatRowid = "chat.rowid" => (Chat, "ROWID"),
        ChatGuid = "chat.guid" => (Chat, "guid"),
        ChatStyle = "chat.style" => (Chat, "style"),
        ChatChatIdentifier = "chat.chatIdentifier" => (Chat, "chat_identifier"),
        ChatServiceName = "chat.serviceName" => (Chat, "service_name"),
        ChatDisplayName = "chat.displayName" => (Chat, "display_name"),
        ChatRoomName = "chat.roomName" => (Chat, "room_name"),
        ChatGroupId = "chat.groupId" => (Chat, "group_id"),
        ChatProperties = "chat.properties" => (Chat, "properties"),
        ChatIsArchived = "chat.isArchived" => (Chat, "is_archived"),
        ChatIsFiltered = "chat.isFiltered" => (Chat, "is_filtered"),
        ChatIsBlackholed = "chat.isBlackholed" => (Chat, "is_blackholed"),
        ChatIsPendingReview = "chat.isPendingReview" => (Chat, "is_pending_review"),
        ChatIsRecovered = "chat.isRecovered" => (Chat, "is_recovered"),
        ChatLastReadMessageTimestamp = "chat.lastReadMessageTimestamp" => (Chat, "last_read_message_timestamp"),
        ChatMessageJoinChatId = "chatMessageJoin.chatId" => (ChatMessageJoin, "chat_id"),
        ChatMessageJoinMessageId = "chatMessageJoin.messageId" => (ChatMessageJoin, "message_id"),
        ChatMessageJoinMessageDate = "chatMessageJoin.messageDate" => (ChatMessageJoin, "message_date"),
        ChatHandleJoinChatId = "chatHandleJoin.chatId" => (ChatHandleJoin, "chat_id"),
        ChatHandleJoinHandleId = "chatHandleJoin.handleId" => (ChatHandleJoin, "handle_id"),
        MessageAttachmentJoinMessageId = "messageAttachmentJoin.messageId" => (MessageAttachmentJoin, "message_id"),
        MessageAttachmentJoinAttachmentId = "messageAttachmentJoin.attachmentId" => (MessageAttachmentJoin, "attachment_id"),
        AttachmentRowid = "attachment.rowid" => (Attachment, "ROWID"),
        AttachmentGuid = "attachment.guid" => (Attachment, "guid"),
        AttachmentFilename = "attachment.filename" => (Attachment, "filename"),
        AttachmentUti = "attachment.uti" => (Attachment, "uti"),
        AttachmentMimeType = "attachment.mimeType" => (Attachment, "mime_type"),
        AttachmentTransferName = "attachment.transferName" => (Attachment, "transfer_name"),
        AttachmentTotalBytes = "attachment.totalBytes" => (Attachment, "total_bytes"),
        AttachmentIsSticker = "attachment.isSticker" => (Attachment, "is_sticker"),
        AttachmentHideAttachment = "attachment.hideAttachment" => (Attachment, "hide_attachment"),
        AttachmentIsOutgoing = "attachment.isOutgoing" => (Attachment, "is_outgoing"),
        AttachmentTransferState = "attachment.transferState" => (Attachment, "transfer_state"),
        AttachmentCreatedDate = "attachment.createdDate" => (Attachment, "created_date"),
        AttachmentStartDate = "attachment.startDate" => (Attachment, "start_date"),
        AttachmentEmojiImageShortDescription = "attachment.emojiImageShortDescription" => (Attachment, "emoji_image_short_description"),
        AttachmentOriginalGuid = "attachment.originalGuid" => (Attachment, "original_guid"),
        ChatRecoverableMessageJoinChatId = "chatRecoverableMessageJoin.chatId" => (ChatRecoverableMessageJoin, "chat_id"),
        ChatRecoverableMessageJoinMessageId = "chatRecoverableMessageJoin.messageId" => (ChatRecoverableMessageJoin, "message_id"),
        ChatRecoverableMessageJoinDeleteDate = "chatRecoverableMessageJoin.deleteDate" => (ChatRecoverableMessageJoin, "delete_date"),
        ChatRecoverableMessageJoinCkSyncState = "chatRecoverableMessageJoin.ckSyncState" => (ChatRecoverableMessageJoin, "ck_sync_state"),
        SqliteDatabasePropertiesKey = "sqliteDatabaseProperties.key" => (SqliteDatabaseProperties, "key"),
        SqliteDatabasePropertiesValue = "sqliteDatabaseProperties.value" => (SqliteDatabaseProperties, "value"),
}

/// Why a database is unsupported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum UnsupportedReasonDto {
    MissingTable { table: KnownTableDto },
    NotOrdinaryTable { table: KnownTableDto },
    MissingRequiredColumn { column: KnownColumnDto },
    RowidShape { table: KnownTableDto },
    VirtualTablePresent,
    SchemaExceedsLimits,
    Other,
}

impl From<UnsupportedReason> for UnsupportedReasonDto {
    fn from(r: UnsupportedReason) -> Self {
        match r {
            UnsupportedReason::MissingTable(t) => Self::MissingTable { table: t.into() },
            UnsupportedReason::NotOrdinaryTable(t) => Self::NotOrdinaryTable { table: t.into() },
            UnsupportedReason::MissingRequiredColumn(c) => {
                Self::MissingRequiredColumn { column: c.into() }
            }
            UnsupportedReason::RowidShape(t) => Self::RowidShape { table: t.into() },
            UnsupportedReason::VirtualTablePresent => Self::VirtualTablePresent,
            UnsupportedReason::SchemaExceedsLimits => Self::SchemaExceedsLimits,
            _ => Self::Other,
        }
    }
}

/// Why a database is damaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export)]
pub enum DamageReasonDto {
    IntegrityProblems,
    RowsNotStreamed { table: KnownTableDto },
    Other,
}

/// Why a check is incomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum IncompleteReasonDto {
    IntegrityCheckOverBudget,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum TextEncodingDto {
    Utf8,
    Utf16le,
    Utf16be,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum OpenModeDto {
    ReadOnly,
    /// Read-write with `query_only`, on our own copy.
    ReadWriteQueryOnly,
}

/// What happened to the database's `-wal`. "Staged" means copied beside the
/// database; that SQLite applied its frames is inferred, never observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum WalOutcomeDto {
    NotListed,
    Staged,
    Skipped,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum IntegrityDto {
    Ok,
    Problems,
    OverBudget,
    Other,
}

/// Every problem kind the report counts. None of them stopped the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum FailureKindDto {
    TsUnsupportedHeader,
    TsTruncated,
    TsUnexpectedTag,
    TsBadReference,
    TsUnsupportedType,
    TsTooDeep,
    TsBudget,
    TsMaterialisationBudget,
    TsTrailingBytes,
    TsNotAttributedString,
    TsInvalidUtf8,
    TsRunLengthMismatch,
    TsRunOverflow,
    TsRunSplitsSurrogate,
    TsBadRunIndex,
    SummaryTooLarge,
    SummaryPlistInvalid,
    SummaryPlistBudget,
    SummaryNotDictionary,
    SummaryShape,
    SummaryUnknownKeys,
    ValueOversized,
    TypeMismatch,
    RealInIntegerColumn,
    BoolOutOfRange,
    InvalidUtf8Text,
    BlobInTextColumn,
    TextInBlobColumn,
    PlistInvalid,
    PlistOverBudget,
    PlistNotDictionary,
    DateAmbiguous,
    DateNegative,
    AssocGuidUnparsed,
    UnknownAttributeKey,
    UnknownAttributeClass,
    UnreadableAttributeValue,
    OrphanJoinRow,
    AttachmentAmbiguous,
    AttachmentInvalidEntry,
    AttachmentUnusualName,
    AttachmentBlobMissing,
    AttachmentBlobUnreadable,
    AttachmentKeyProblem,
    AttachmentBlobSizeMismatch,
    AggregateOverBudget,
}

impl From<TsErrorKind> for FailureKindDto {
    fn from(k: TsErrorKind) -> Self {
        use TsErrorKind as K;
        match k {
            K::UnsupportedHeader => Self::TsUnsupportedHeader,
            K::Truncated => Self::TsTruncated,
            K::UnexpectedTag => Self::TsUnexpectedTag,
            K::BadReference => Self::TsBadReference,
            K::UnsupportedType => Self::TsUnsupportedType,
            K::TooDeep => Self::TsTooDeep,
            K::Budget => Self::TsBudget,
            K::MaterialisationBudget => Self::TsMaterialisationBudget,
            K::TrailingBytes => Self::TsTrailingBytes,
            K::NotAttributedString => Self::TsNotAttributedString,
            K::InvalidUtf8 => Self::TsInvalidUtf8,
            K::RunLengthMismatch => Self::TsRunLengthMismatch,
            K::RunOverflow => Self::TsRunOverflow,
            K::RunSplitsSurrogate => Self::TsRunSplitsSurrogate,
            K::BadRunIndex => Self::TsBadRunIndex,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProblemDto {
    pub kind: FailureKindDto,
    pub count: u64,
}

/// One known table, as the scan and probe found it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TableReportDto {
    pub table: KnownTableDto,
    pub present: bool,
    /// `count(*)`, when present.
    pub rows: Option<u64>,
    /// Known columns not present, when the table is present.
    pub missing_columns: Vec<KnownColumnDto>,
    pub unknown_columns: u32,
    pub generated_columns: u32,
    pub wrong_case_columns: u32,
}

/// Features derived from the present columns; never an iOS version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FeaturesDto {
    pub attributed_body: bool,
    pub tapbacks: bool,
    pub emoji_tapbacks: bool,
    pub replies: bool,
    pub edits_and_unsends: bool,
    pub recently_deleted: bool,
    pub scheduled: bool,
    pub filtering: bool,
    pub attachments: bool,
    pub genmoji: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SchemaReportDto {
    /// One entry per known table; empty when the probe did not run.
    pub tables: Vec<TableReportDto>,
    /// Known columns not present, summed over the present tables.
    pub missing_columns: u32,
    pub unknown_tables: u32,
    pub unknown_columns: u32,
    pub wrong_case_columns: u32,
    pub views: u32,
    pub triggers: u32,
    pub indexes: u32,
    pub stat_tables: u32,
    pub encoding: Option<TextEncodingDto>,
    /// `_ClientVersion`, raw, when it parsed and fits a JavaScript number.
    /// Its mapping to iOS versions is **UNVERIFIED**.
    pub client_version: Option<i64>,
    pub client_version_parsed: bool,
    /// It parsed but is outside ±(2^53 − 1), so it is not sent.
    pub client_version_out_of_safe_range: bool,
    pub features: FeaturesDto,
}

/// Header and frame facts of a staged `-wal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct WalFramesDto {
    pub header_valid: bool,
    pub page_size_matches: bool,
    pub valid_committed_frames: u64,
    pub uncommitted_valid_frames: u64,
    pub frames_after_first_invalid: u64,
    pub trailing_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DbFactsDto {
    /// `null` when no connection opened.
    pub open_mode: Option<OpenModeDto>,
    pub wal_listed: bool,
    pub wal_outcome: WalOutcomeDto,
    pub wal_bytes: Option<u64>,
    pub wal_header_says_wal: Option<bool>,
    pub wal_size_matches: Option<bool>,
    pub wal_frames: Option<WalFramesDto>,
    pub size_matches: Option<bool>,
    /// `null` when the integrity check did not run.
    pub integrity: Option<IntegrityDto>,
    pub page_size: Option<u64>,
    pub staged_bytes: u64,
}

/// The counts section: rows by table, the message partition and messages
/// in no chat. `null` counters are "not recorded by this database".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct CountsDto {
    pub message_rows: u64,
    pub association_rows: Option<u64>,
    /// Rows with `item_type ≠ 0` and no association: group events and every
    /// other non-message item kind (shown as "Non-message rows").
    pub group_action_rows: Option<u64>,
    pub recently_deleted_rows: Option<u64>,
    pub other_rows: Option<u64>,
    pub chat_rows: u64,
    /// `handle` rows: address and service pairs.
    pub handle_rows: u64,
    pub attachment_rows: Option<u64>,
    pub messages_in_no_chat: Option<u64>,
}

/// The date range of `message.date` and what was left out of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DatesDto {
    /// RFC 3339 UTC. Shown, never logged or persisted.
    pub earliest_message_at: Option<String>,
    /// RFC 3339 UTC. Shown, never logged or persisted.
    pub latest_message_at: Option<String>,
    /// Scheduled messages (their date is the planned send time).
    pub excluded_scheduled: Option<u64>,
    /// Before 2007-01-01.
    pub excluded_before_floor: u64,
    /// After the check started.
    pub excluded_after_check: u64,
    /// Zero, negative, ambiguous, NULL or unreadable.
    pub excluded_unusable: u64,
}

/// Where attachment files are. "Not in this backup" is said only for
/// `notListedAtSourcedKey`; a key that does not unwrap is never "damaged".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AttachmentOutcomesDto {
    pub rows: Option<u64>,
    pub found: u64,
    pub not_listed_at_sourced_key: u64,
    pub not_found_at_assumed_key: u64,
    pub not_looked_up: u64,
    pub ambiguous: u64,
    pub invalid_entry: u64,
    pub file_present: u64,
    pub file_missing: u64,
    /// A regular file whose length differs from the backup's record of it.
    pub file_size_mismatch: u64,
    /// A link, directory or device, or metadata that could not be read.
    pub file_not_readable: u64,
    /// No `EncryptionKey` or no `ProtectionClass`.
    pub no_key: u64,
    pub key_does_not_unwrap: u64,
    pub class_key_unavailable: u64,
    /// Any other probe outcome.
    pub probe_other: u64,
    pub not_probed: u64,
    /// Bytes of present blobs (ciphertext for encrypted backups).
    pub present_blob_bytes: u64,
}

/// The extraction check's report, as `get_backup_details` returns it. The
/// terminal event carries only its summary (D47).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExtractionReportDto {
    pub job_id: String,
    pub outcome: ExtractionOutcomeDto,
    /// Empty unless readable; `iosVersionUntested` first.
    pub qualifiers: Vec<QualifierDto>,
    pub unsupported_reasons: Vec<UnsupportedReasonDto>,
    pub damage_reason: Option<DamageReasonDto>,
    pub incomplete_reason: Option<IncompleteReasonDto>,
    pub source_changed: bool,
    /// The staging folder was removed when the backup was closed.
    pub staging_removed: bool,
    /// The plaintext copy was deleted straight after the row pass. True
    /// when nothing was staged.
    pub plaintext_removed_after_row_pass: bool,
    /// `null` when the check was skipped.
    pub schema: Option<SchemaReportDto>,
    pub database: Option<DbFactsDto>,
    /// `null` unless readable ("not read").
    pub counts: Option<CountsDto>,
    pub dates: Option<DatesDto>,
    pub attachments: Option<AttachmentOutcomesDto>,
    /// Non-zero problem counts, in a fixed order; `null` unless readable.
    pub problems: Option<Vec<ProblemDto>>,
    /// RFC 3339 UTC; `null` when the check was skipped.
    pub checked_at: Option<String>,
    pub duration_ms: u64,
    /// At least one counter was clamped to `Number.MAX_SAFE_INTEGER`.
    pub saturated: bool,
}

/// The terminal event's payload (D47): a summary only. The full report is
/// fetched with `get_backup_details`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractionSummary {
    pub outcome: ExtractionOutcomeDto,
    pub source_changed: bool,
    pub plaintext_removed_after_row_pass: bool,
}

impl ExtractionReportDto {
    pub fn summary(&self) -> ExtractionSummary {
        ExtractionSummary {
            outcome: self.outcome,
            source_changed: self.source_changed,
            plaintext_removed_after_row_pass: self.plaintext_removed_after_row_pass,
        }
    }
}

/// How the extraction part of a job ended.
pub enum ExtractionEnding {
    /// `oma_messages::check` returned a report.
    Checked(Box<ExtractionReport>),
    /// Validation found errors; the Messages database was not read.
    SkippedNotImportable,
}

/// Clamps counters and remembers whether any was clamped.
#[derive(Default)]
struct Counter {
    saturated: bool,
}

impl Counter {
    fn n(&mut self, v: u64) -> u64 {
        let (v, sat) = clamp(v);
        self.saturated |= sat;
        v
    }

    fn o(&mut self, v: Option<u64>) -> Option<u64> {
        v.map(|v| self.n(v))
    }
}

/// Unix nanoseconds as RFC 3339 UTC (second precision).
fn unix_ns_rfc3339(ns: i64) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let t = if secs >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_secs(secs.unsigned_abs()))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(secs.unsigned_abs()))
    };
    rfc3339(t.unwrap_or(UNIX_EPOCH))
}

fn qualifier(q: Qualifier) -> QualifierDto {
    match q {
        Qualifier::UnknownTables(count) => QualifierDto::UnknownTables { count },
        Qualifier::UnknownColumns(count) => QualifierDto::UnknownColumns { count },
        Qualifier::WrongCaseColumns(count) => QualifierDto::WrongCaseColumns { count },
        Qualifier::WalSkipped => QualifierDto::WalSkipped,
        Qualifier::WalStateUnknown => QualifierDto::WalStateUnknown,
        Qualifier::WalFramesInvalid => QualifierDto::WalFramesInvalid,
        Qualifier::SourceChanged => QualifierDto::SourceChanged,
        Qualifier::AggregatesIncomplete => QualifierDto::AggregatesIncomplete,
        Qualifier::ValuesWithheld => QualifierDto::ValuesWithheld,
        Qualifier::NonUtf8Encoding => QualifierDto::NonUtf8Encoding,
        Qualifier::SizeMismatch => QualifierDto::SizeMismatch,
        _ => QualifierDto::Other,
    }
}

fn schema(r: &ExtractionReport, c: &mut Counter) -> SchemaReportDto {
    let s = &r.schema;
    let tables: Vec<TableReportDto> = s
        .tables
        .iter()
        .map(|t| TableReportDto {
            table: t.table.into(),
            present: t.present,
            rows: c.o(t.rows),
            missing_columns: t.missing_columns.iter().map(|k| (*k).into()).collect(),
            unknown_columns: t.unknown_columns,
            generated_columns: t.generated_columns,
            wrong_case_columns: t.wrong_case_columns,
        })
        .collect();
    let missing_columns = tables.iter().fold(0u32, |n, t| {
        n.saturating_add(u32::try_from(t.missing_columns.len()).unwrap_or(u32::MAX))
    });
    let f = s.features;
    SchemaReportDto {
        tables,
        missing_columns,
        unknown_tables: s.unknown_tables,
        unknown_columns: s.unknown_columns(),
        wrong_case_columns: s.wrong_case_columns(),
        views: s.views,
        triggers: s.triggers,
        indexes: s.indexes,
        stat_tables: s.stat_tables,
        encoding: s.encoding.map(|e| match e {
            TextEncoding::Utf8 => TextEncodingDto::Utf8,
            TextEncoding::Utf16le => TextEncodingDto::Utf16le,
            TextEncoding::Utf16be => TextEncodingDto::Utf16be,
            _ => TextEncodingDto::Other,
        }),
        client_version: s.client_version.and_then(safe_i64),
        client_version_parsed: s.client_version_parsed,
        client_version_out_of_safe_range: s.client_version.is_some_and(|v| safe_i64(v).is_none()),
        features: FeaturesDto {
            attributed_body: f.attributed_body,
            tapbacks: f.tapbacks,
            emoji_tapbacks: f.emoji_tapbacks,
            replies: f.replies,
            edits_and_unsends: f.edits_and_unsends,
            recently_deleted: f.recently_deleted,
            scheduled: f.scheduled,
            filtering: f.filtering,
            attachments: f.attachments,
            genmoji: f.genmoji,
        },
    }
}

fn database(r: &ExtractionReport, c: &mut Counter) -> DbFactsDto {
    let d = &r.database;
    DbFactsDto {
        open_mode: d.open_mode.map(|m| match m {
            OpenMode::ReadOnly => OpenModeDto::ReadOnly,
            OpenMode::ReadWriteQueryOnly => OpenModeDto::ReadWriteQueryOnly,
        }),
        wal_listed: d.wal.listed,
        wal_outcome: match d.wal.outcome {
            WalOutcome::NotListed => WalOutcomeDto::NotListed,
            WalOutcome::Staged { .. } => WalOutcomeDto::Staged,
            WalOutcome::Skipped(_) => WalOutcomeDto::Skipped,
            _ => WalOutcomeDto::Other,
        },
        wal_bytes: c.o(d.wal.bytes),
        wal_header_says_wal: d.wal.header_says_wal,
        wal_size_matches: d.wal.size_matches,
        wal_frames: d.wal.facts.map(|f| WalFramesDto {
            header_valid: f.header_valid,
            page_size_matches: f.page_size_matches,
            valid_committed_frames: c.n(f.valid_committed_frames),
            uncommitted_valid_frames: c.n(f.uncommitted_valid_frames),
            frames_after_first_invalid: c.n(f.frames_after_first_invalid),
            trailing_bytes: c.n(f.trailing_bytes),
        }),
        size_matches: d.size_matches,
        integrity: d.integrity.map(|i| match i {
            IntegrityOutcome::Ok => IntegrityDto::Ok,
            IntegrityOutcome::Problems => IntegrityDto::Problems,
            IntegrityOutcome::OverBudget => IntegrityDto::OverBudget,
            _ => IntegrityDto::Other,
        }),
        page_size: c.o(d.page_size),
        staged_bytes: c.n(d.staged_bytes),
    }
}

fn counts(r: &ExtractionReport, c: &mut Counter) -> Option<CountsDto> {
    let (m, ch, h) = (r.messages.as_ref()?, r.chats.as_ref()?, r.handles.as_ref()?);
    Some(CountsDto {
        message_rows: c.n(m.rows),
        association_rows: c.o(m.with_association),
        group_action_rows: c.o(m.group_actions),
        recently_deleted_rows: c.o(m.recently_deleted),
        other_rows: c.o(m.other_rows),
        chat_rows: c.n(ch.rows),
        handle_rows: c.n(h.rows),
        attachment_rows: c.o(r.attachments.as_ref().and_then(|a| a.rows)),
        messages_in_no_chat: c.o(r.links.as_ref().and_then(|l| l.messages_in_no_chat)),
    })
}

fn dates(r: &ExtractionReport, c: &mut Counter) -> Option<DatesDto> {
    let d = r.dates.as_ref()?;
    Some(DatesDto {
        earliest_message_at: d.earliest_message.map(unix_ns_rfc3339),
        latest_message_at: d.latest_message.map(unix_ns_rfc3339),
        excluded_scheduled: c.o(d.excluded_scheduled),
        excluded_before_floor: c.n(d.excluded_before_floor),
        excluded_after_check: c.n(d.excluded_after_checked_at),
        excluded_unusable: c.n(d.excluded_not_classified),
    })
}

/// `null` when the database has no `attachment` table: then nothing was
/// looked up or probed, and the lookup and probe counters (plain zeros in
/// the report) are "not recorded", never 0 (I5).
fn attachments(r: &ExtractionReport, c: &mut Counter) -> Option<AttachmentOutcomesDto> {
    let a = r.attachments.as_ref().filter(|a| a.rows.is_some())?;
    let l = &a.lookup;
    let p = &a.probes;
    let not_looked_up = l
        .not_attempted
        .iter()
        .fold(0u64, |n, e| n.saturating_add(e.1));
    Some(AttachmentOutcomesDto {
        rows: c.o(a.rows),
        found: c.n(l.found),
        not_listed_at_sourced_key: c.n(l.not_listed_at_sourced_key),
        not_found_at_assumed_key: c.n(l.not_found_at_assumed_key),
        not_looked_up: c.n(not_looked_up),
        ambiguous: c.n(l.ambiguous),
        invalid_entry: c.n(l.invalid_entry),
        file_present: c.n(p.present),
        file_missing: c.n(p.missing),
        file_size_mismatch: c.n(p.size_mismatch),
        file_not_readable: c.n(p.not_regular.saturating_add(p.unreadable)),
        no_key: c.n(p.no_key.saturating_add(p.no_protection_class)),
        key_does_not_unwrap: c.n(p.key_does_not_unwrap),
        class_key_unavailable: c.n(p.class_key_unavailable),
        probe_other: c
            .n(p.not_a_file
                .saturating_add(p.invalid_entry)
                .saturating_add(p.other)),
        not_probed: c.n(p.not_probed),
        present_blob_bytes: c.n(a.present_blob_bytes),
    })
}

/// Every problem count, in a fixed order, including zeros.
pub fn problem_counts(f: &FailureCounts) -> Vec<(FailureKindDto, u64)> {
    use FailureKindDto as K;
    let v = &f.values;
    let mut out: Vec<(FailureKindDto, u64)> = f
        .typedstream
        .iter()
        .map(|(k, n)| (FailureKindDto::from(*k), *n))
        .collect();
    out.extend([
        (K::SummaryTooLarge, f.summary_too_large),
        (K::SummaryPlistInvalid, f.summary_plist_invalid),
        (K::SummaryPlistBudget, f.summary_plist_budget),
        (K::SummaryNotDictionary, f.summary_not_dictionary),
        (K::SummaryShape, f.summary_shape),
        (K::SummaryUnknownKeys, f.summary_unknown_keys),
        (K::ValueOversized, v.value_oversized),
        (K::TypeMismatch, v.type_mismatch),
        (K::RealInIntegerColumn, v.real_in_integer_column),
        (K::BoolOutOfRange, v.bool_out_of_range),
        (K::InvalidUtf8Text, v.invalid_utf8_text),
        (K::BlobInTextColumn, v.blob_in_text_column),
        (K::TextInBlobColumn, v.text_in_blob_column),
        (K::PlistInvalid, v.plist_invalid),
        (K::PlistOverBudget, v.plist_over_budget),
        (K::PlistNotDictionary, v.plist_not_dictionary),
        (K::DateAmbiguous, f.date_ambiguous),
        (K::DateNegative, f.date_negative),
        (K::AssocGuidUnparsed, f.assoc_guid_unparsed),
        (K::UnknownAttributeKey, f.unknown_attribute_key),
        (K::UnknownAttributeClass, f.unknown_attribute_class),
        (K::UnreadableAttributeValue, f.unreadable_attribute_value),
        (K::OrphanJoinRow, f.orphan_join_row),
        (K::AttachmentAmbiguous, f.attachment_ambiguous),
        (K::AttachmentInvalidEntry, f.attachment_invalid_entry),
        (K::AttachmentUnusualName, f.attachment_unusual_name),
        (K::AttachmentBlobMissing, f.attachment_blob_missing),
        (K::AttachmentBlobUnreadable, f.attachment_blob_unreadable),
        (K::AttachmentKeyProblem, f.attachment_key_problem),
        (
            K::AttachmentBlobSizeMismatch,
            f.attachment_blob_size_mismatch,
        ),
        (K::AggregateOverBudget, u64::from(f.aggregate_over_budget)),
    ]);
    out
}

/// The report as it crosses IPC. `ios_version_untested` adds the shell's
/// qualifier to a readable result.
pub fn report_dto(
    ending: &ExtractionEnding,
    job_id: &str,
    staging_removed: bool,
    ios_version_untested: bool,
) -> ExtractionReportDto {
    let r = match ending {
        ExtractionEnding::Checked(r) => r,
        ExtractionEnding::SkippedNotImportable => {
            return ExtractionReportDto {
                job_id: job_id.to_owned(),
                outcome: ExtractionOutcomeDto::SkippedNotImportable,
                qualifiers: Vec::new(),
                unsupported_reasons: Vec::new(),
                damage_reason: None,
                incomplete_reason: None,
                source_changed: false,
                staging_removed,
                plaintext_removed_after_row_pass: true,
                schema: None,
                database: None,
                counts: None,
                dates: None,
                attachments: None,
                problems: None,
                checked_at: None,
                duration_ms: 0,
                saturated: false,
            }
        }
    };
    let mut c = Counter::default();
    let mut qualifiers = Vec::new();
    let mut unsupported_reasons = Vec::new();
    let mut damage_reason = None;
    let mut incomplete_reason = None;
    let outcome = match &r.status {
        ExtractionStatus::Readable { qualifiers: qs } => {
            if ios_version_untested {
                qualifiers.push(QualifierDto::IosVersionUntested);
            }
            qualifiers.extend(qs.iter().map(|q| qualifier(*q)));
            if qualifiers.is_empty() {
                ExtractionOutcomeDto::Readable
            } else {
                ExtractionOutcomeDto::ReadableWithQualifiers
            }
        }
        ExtractionStatus::Damaged(d) => {
            damage_reason = Some(match d {
                DamageReason::IntegrityProblems => DamageReasonDto::IntegrityProblems,
                DamageReason::RowsNotStreamed(t) => {
                    DamageReasonDto::RowsNotStreamed { table: (*t).into() }
                }
                _ => DamageReasonDto::Other,
            });
            ExtractionOutcomeDto::Damaged
        }
        ExtractionStatus::Unsupported(reasons) => {
            unsupported_reasons = reasons.iter().map(|u| (*u).into()).collect();
            ExtractionOutcomeDto::Unsupported
        }
        ExtractionStatus::Incomplete(i) => {
            incomplete_reason = Some(match i {
                IncompleteReason::IntegrityCheckOverBudget => {
                    IncompleteReasonDto::IntegrityCheckOverBudget
                }
                _ => IncompleteReasonDto::Other,
            });
            ExtractionOutcomeDto::Incomplete
        }
        _ => {
            incomplete_reason = Some(IncompleteReasonDto::Other);
            ExtractionOutcomeDto::Incomplete
        }
    };
    let problems = r.failures.as_ref().map(|f| {
        problem_counts(f)
            .into_iter()
            .filter(|(_, n)| *n > 0)
            .map(|(kind, n)| ProblemDto {
                kind,
                count: c.n(n),
            })
            .collect()
    });
    let schema = Some(schema(r, &mut c));
    let database = Some(database(r, &mut c));
    let counts = counts(r, &mut c);
    let dates = dates(r, &mut c);
    let attachments = attachments(r, &mut c);
    ExtractionReportDto {
        job_id: job_id.to_owned(),
        outcome,
        qualifiers,
        unsupported_reasons,
        damage_reason,
        incomplete_reason,
        source_changed: r.database.source_changed_during_check,
        staging_removed,
        plaintext_removed_after_row_pass: r.database.plaintext_removed_after_row_pass,
        schema,
        database,
        counts,
        dates,
        attachments,
        problems,
        checked_at: Some(rfc3339(r.checked_at)),
        duration_ms: millis(r.elapsed),
        saturated: c.saturated,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashSet};

    use ts_rs::Config;

    use super::*;
    use crate::backups::dto::{BackupDetails, JobEvent, JobPhase, ValidationReportDto};

    #[test]
    fn every_known_table_and_column_has_its_own_literal() {
        let mut literals = HashSet::new();
        for t in KnownTable::ALL {
            assert_ne!(KnownTableDto::from(t), KnownTableDto::Other, "{t:?}");
            for c in t.known_columns() {
                let dto = KnownColumnDto::from(c);
                assert_ne!(dto, KnownColumnDto::Other, "{c:?}");
                let lit = serde_json::to_value(dto).expect("json");
                assert!(literals.insert(lit.to_string()), "duplicate {lit}");
            }
        }
        assert_eq!(
            serde_json::to_value(KnownColumnDto::MessageDateEdited).expect("json"),
            "message.dateEdited"
        );
    }

    #[test]
    fn every_failure_kind_is_counted_once() {
        let kinds: Vec<FailureKindDto> = problem_counts(&FailureCounts::default())
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        let distinct: HashSet<String> = kinds
            .iter()
            .map(|k| serde_json::to_string(k).expect("json"))
            .collect();
        assert_eq!(distinct.len(), kinds.len(), "no kind is counted twice");
        // Every variant is counted.
        let decl = FailureKindDto::decl(&Config::from_env());
        assert_eq!(decl.matches('|').count() + 1, kinds.len(), "{decl}");
    }

    #[test]
    fn the_ios_version_is_untested_in_m3() {
        assert!(TESTED_REAL_IOS_VERSIONS.is_empty());
        assert!(ios_version_untested(Some("18.1")));
        assert!(ios_version_untested(None));
    }

    // -------------------------------------------- structural guard (§16.4)

    use crate::ts_guard::{self, fields};

    fn walk() -> BTreeMap<String, String> {
        let mut w = ts_guard::Walk::new();
        w.root::<ExtractionReportDto>();
        w.root::<ValidationReportDto>();
        w.root::<BackupDetails>();
        w.root::<JobEvent>();
        w.decls()
    }

    const FORBIDDEN_KEYS: &[&str] = &[
        "body",
        "text",
        "handle",
        "displayName",
        "chatIdentifier",
        "filename",
        "transferName",
        "subject",
        "guid",
        "emoji",
        "mention",
        "name",
        "path",
        "title",
    ];

    /// Milestone 2 fields that predate the guard and are shown on purpose:
    /// the backup folder the user chose, and the device's display name from
    /// Info.plist.
    const M2_FORBIDDEN_KEY_EXCEPTIONS: &[&str] =
        &["BackupDetails.path", "DeviceDetails.displayName"];

    /// The only `string` fields, by type and key. Every one is an id we
    /// made, a sentence we wrote, a sanitised plist value (M2) or a date.
    const STRING_FIELDS: &[&str] = &[
        // M3: our job id, and the three dates.
        "ExtractionReportDto.jobId",
        "ExtractionReportDto.checkedAt",
        "DatesDto.earliestMessageAt",
        "DatesDto.latestMessageAt",
        // M2.
        "BackupDetails.path",
        "BackupDetails.archiveSyncProvider",
        "BackupSummary.id",
        "BackupSummary.locationId",
        "BackupSummary.lastBackupDate",
        "BackupSummary.stateReason",
        "BackupSummary.activeJobId",
        "SourcedText.value",
        "StatusDetails.snapshotState",
        "StatusDetails.formatVersion",
        "StatusDetails.date",
        "StatusDetails.backupState",
        "ValidationCheck.id",
        "ValidationCheck.message",
        "ValidationReportDto.validatedAt",
        "JobSummary.jobId",
        "JobSummary.backupId",
        "JobEvent.jobId",
        "IpcError.message",
    ];

    #[test]
    fn no_reachable_ipc_type_has_a_content_key_or_a_free_string() {
        let decls = walk();
        let mut checked = 0;
        for (ty, decl) in &decls {
            assert!(!decl.contains("bigint"), "{decl}");
            // The archive event types have their own guard (archives::dto).
            if ts_guard::M4_EVENT_TYPES.contains(&ty.as_str())
                || ts_guard::M6_EVENT_TYPES.contains(&ty.as_str())
            {
                continue;
            }
            for (key, field_ty) in fields(decl) {
                let qualified = format!("{ty}.{key}");
                if FORBIDDEN_KEYS.contains(&key.as_str()) {
                    assert!(
                        M2_FORBIDDEN_KEY_EXCEPTIONS.contains(&qualified.as_str()),
                        "forbidden key {qualified}"
                    );
                }
                if ts_guard::is_string(&field_ty) {
                    assert!(
                        STRING_FIELDS.contains(&qualified.as_str()),
                        "{qualified} is typed {field_ty}"
                    );
                }
                checked += 1;
            }
        }
        assert!(checked > 100, "the parser found the fields ({checked})");
        // Literal values are not keys.
        let phases = fields(&JobPhase::decl(&Config::from_env()));
        assert!(phases.is_empty(), "{phases:?}");
        // The parser does see keys inside tagged unions.
        let q: Vec<String> = fields(&QualifierDto::decl(&Config::from_env()))
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert!(q.contains(&"kind".to_owned()) && q.contains(&"count".to_owned()));
    }

    #[test]
    fn the_walk_reaches_exactly_the_reviewed_types() {
        let reached: BTreeSet<String> = walk().into_keys().collect();
        let expected: BTreeSet<String> = ts_guard::M2_M3_TYPES
            .iter()
            .chain(ts_guard::M4_EVENT_TYPES)
            .chain(ts_guard::M6_EVENT_TYPES)
            .map(|s| (*s).to_owned())
            .collect();
        assert_eq!(reached, expected);
    }

    #[test]
    fn a_skipped_check_reports_nothing_read() {
        let dto = report_dto(&ExtractionEnding::SkippedNotImportable, "job-7", true, true);
        assert_eq!(dto.outcome, ExtractionOutcomeDto::SkippedNotImportable);
        assert!(
            dto.qualifiers.is_empty(),
            "no qualifier on a check that did not run"
        );
        assert!(dto.schema.is_none() && dto.counts.is_none() && dto.checked_at.is_none());
        assert!(dto.plaintext_removed_after_row_pass);
        let json = serde_json::to_value(&dto).expect("json");
        assert_eq!(json["outcome"], "skippedNotImportable");
        assert_eq!(json["jobId"], "job-7");
    }
}
