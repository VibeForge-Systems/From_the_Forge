//! The tables and columns the check knows (M3 §6.1): one static,
//! table-driven list.
//!
//! Each column has a [`ColumnKind`], whether it is required, its byte cap
//! (§5.4) and whether a zero value means "unset" (§8.1). Nothing else is
//! read: positional `SELECT *` is never used, because Apple appends columns
//! and S1 says optional columns may be "reordered and omitted".
//!
//! Sources: the column names and the required set come from S2 (which treats
//! exactly `ROWID`, `guid`, `date` and `is_from_me` as the required
//! `message` columns), S3, S5, S8, S12 and S13. "Zero means unset" is
//! recorded only for `date_delivered` and `date_edited` (S3, observed).
//! Every column outside the core set already cited in `supported-formats.md`
//! is **UNVERIFIED** in-repo until that document's per-column citations land
//! (§21). Snapshots A (S12) and C (S3) are macOS `chat.db`; whether iOS
//! sms.db has the same columns is **UNVERIFIED**.

use std::fmt;

use crate::caps::{
    CAP_ATTRIBUTED_BODY, CAP_GUID, CAP_IDENTIFIER, CAP_OTHER_TEXT, CAP_PAYLOAD_DATA,
    CAP_PROPERTIES, CAP_SUMMARY, CAP_TEXT, INTEGER_BYTES,
};

/// How a known column is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ColumnKind {
    /// The `ROWID` alias of a table whose rows are keyed by it. Read
    /// strictly as an integer.
    RowId,
    /// An integer or a raw code.
    Integer,
    /// 0 or 1.
    Bool,
    /// A raw Cocoa timestamp (§8.1).
    Time,
    /// Text, under a byte cap.
    Text,
    /// A blob, under a byte cap.
    Blob,
    /// A blob of which only the storage class and size are read.
    BlobSize,
    /// A blob of which only the storage class (`typeof`) is read.
    TypeOnly,
    /// A blob whose storage class is always read, and whose value is read,
    /// under the column's cap, only when the caller keeps raw blobs (M4 R3:
    /// `message.payload_data`).
    BlobOnRequest,
}

/// One known column.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ColumnSpec {
    /// The stable numeric code (see [`KnownColumn::code`]).
    pub(crate) code: u16,
    pub(crate) name: &'static str,
    pub(crate) kind: ColumnKind,
    /// The byte cap for `Text`, `Blob` and `BlobSize`; 0 otherwise.
    pub(crate) cap: u32,
    pub(crate) required: bool,
    pub(crate) zero_means_unset: bool,
}

impl ColumnSpec {
    /// What the column counts toward the row budget.
    const fn row_bytes(&self) -> u64 {
        match self.kind {
            ColumnKind::Text | ColumnKind::Blob | ColumnKind::BlobOnRequest => self.cap as u64,
            ColumnKind::RowId
            | ColumnKind::Integer
            | ColumnKind::Bool
            | ColumnKind::Time
            | ColumnKind::BlobSize
            | ColumnKind::TypeOnly => INTEGER_BYTES,
        }
    }
}

/// Declares one table's columns: an index constant per column, in list
/// order, and the `COLUMNS` slice in the same order. Each column carries its
/// stable code (`KnownColumn::code`), written out so that reordering the
/// list cannot change one.
macro_rules! columns {
    ($m:ident: $( $c:ident = $code:literal => ($name:literal, $kind:ident, $cap:expr, $req:expr, $zero:expr) ),* $(,)?) => {
        pub(crate) mod $m {
            // The message and attachment constants are used by the passes
            // of later slices; the list itself is used by the probe today.
            #![allow(dead_code)]
            #[allow(unused_imports)]
            use super::*;

            #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
            #[repr(u8)]
            enum Ix { $($c),* }

            $( pub(crate) const $c: u8 = Ix::$c as u8; )*

            pub(crate) const COLUMNS: &[ColumnSpec] = &[
                $( ColumnSpec {
                    code: $code,
                    name: $name,
                    kind: ColumnKind::$kind,
                    cap: $cap,
                    required: $req,
                    zero_means_unset: $zero,
                } ),*
            ];
        }
    };
}

const REQ: bool = true;
const OPT: bool = false;
const NO_CAP: u32 = 0;

columns!(message:
    ROWID = 101 => ("ROWID", RowId, NO_CAP, REQ, false),
    GUID = 102 => ("guid", Text, CAP_GUID, REQ, false),
    DATE = 103 => ("date", Time, NO_CAP, REQ, false),
    IS_FROM_ME = 104 => ("is_from_me", Bool, NO_CAP, REQ, false),
    TEXT = 105 => ("text", Text, CAP_TEXT, OPT, false),
    ATTRIBUTED_BODY = 106 => ("attributedBody", Blob, CAP_ATTRIBUTED_BODY, OPT, false),
    SERVICE = 107 => ("service", Text, CAP_OTHER_TEXT, OPT, false),
    HANDLE_ID = 108 => ("handle_id", Integer, NO_CAP, OPT, false),
    OTHER_HANDLE = 109 => ("other_handle", Integer, NO_CAP, OPT, false),
    DESTINATION_CALLER_ID = 110 => ("destination_caller_id", Text, CAP_IDENTIFIER, OPT, false),
    SUBJECT = 111 => ("subject", Text, CAP_OTHER_TEXT, OPT, false),
    DATE_READ = 112 => ("date_read", Time, NO_CAP, OPT, false),
    DATE_DELIVERED = 113 => ("date_delivered", Time, NO_CAP, OPT, true),
    DATE_PLAYED = 114 => ("date_played", Time, NO_CAP, OPT, false),
    DATE_EDITED = 115 => ("date_edited", Time, NO_CAP, OPT, true),
    DATE_RETRACTED = 116 => ("date_retracted", Time, NO_CAP, OPT, false),
    IS_READ = 117 => ("is_read", Bool, NO_CAP, OPT, false),
    IS_DELIVERED = 118 => ("is_delivered", Bool, NO_CAP, OPT, false),
    IS_SENT = 119 => ("is_sent", Bool, NO_CAP, OPT, false),
    IS_AUDIO_MESSAGE = 120 => ("is_audio_message", Bool, NO_CAP, OPT, false),
    CACHE_HAS_ATTACHMENTS = 121 => ("cache_has_attachments", Bool, NO_CAP, OPT, false),
    ITEM_TYPE = 122 => ("item_type", Integer, NO_CAP, OPT, false),
    GROUP_ACTION_TYPE = 123 => ("group_action_type", Integer, NO_CAP, OPT, false),
    GROUP_TITLE = 124 => ("group_title", Text, CAP_OTHER_TEXT, OPT, false),
    SHARE_STATUS = 125 => ("share_status", Integer, NO_CAP, OPT, false),
    SHARE_DIRECTION = 126 => ("share_direction", Bool, NO_CAP, OPT, false),
    ASSOCIATED_MESSAGE_GUID = 127 => ("associated_message_guid", Text, CAP_GUID, OPT, false),
    ASSOCIATED_MESSAGE_TYPE = 128 => ("associated_message_type", Integer, NO_CAP, OPT, false),
    ASSOCIATED_MESSAGE_EMOJI = 129 => ("associated_message_emoji", Text, CAP_OTHER_TEXT, OPT, false),
    ASSOCIATED_MESSAGE_RANGE_LOCATION = 130 => ("associated_message_range_location", Integer, NO_CAP, OPT, false),
    ASSOCIATED_MESSAGE_RANGE_LENGTH = 131 => ("associated_message_range_length", Integer, NO_CAP, OPT, false),
    BALLOON_BUNDLE_ID = 132 => ("balloon_bundle_id", Text, CAP_OTHER_TEXT, OPT, false),
    EXPRESSIVE_SEND_STYLE_ID = 133 => ("expressive_send_style_id", Text, CAP_OTHER_TEXT, OPT, false),
    PAYLOAD_DATA = 134 => ("payload_data", BlobOnRequest, CAP_PAYLOAD_DATA, OPT, false),
    MESSAGE_SUMMARY_INFO = 135 => ("message_summary_info", Blob, CAP_SUMMARY, OPT, false),
    THREAD_ORIGINATOR_GUID = 136 => ("thread_originator_guid", Text, CAP_GUID, OPT, false),
    THREAD_ORIGINATOR_PART = 137 => ("thread_originator_part", Text, CAP_OTHER_TEXT, OPT, false),
    REPLY_TO_GUID = 138 => ("reply_to_guid", Text, CAP_GUID, OPT, false),
    PART_COUNT = 139 => ("part_count", Integer, NO_CAP, OPT, false),
    SCHEDULE_TYPE = 140 => ("schedule_type", Integer, NO_CAP, OPT, false),
    SCHEDULE_STATE = 141 => ("schedule_state", Integer, NO_CAP, OPT, false),
    FILTER_ACTION = 142 => ("filter_action", Integer, NO_CAP, OPT, false),
    FILTER_SUB_ACTION = 143 => ("filter_sub_action", Integer, NO_CAP, OPT, false),
    ERROR = 144 => ("error", Integer, NO_CAP, OPT, false),
    WAS_DOWNGRADED = 145 => ("was_downgraded", Bool, NO_CAP, OPT, false),
);

columns!(handle:
    ROWID = 201 => ("ROWID", RowId, NO_CAP, REQ, false),
    ID = 202 => ("id", Text, CAP_IDENTIFIER, REQ, false),
    SERVICE = 203 => ("service", Text, CAP_OTHER_TEXT, OPT, false),
    COUNTRY = 204 => ("country", Text, CAP_OTHER_TEXT, OPT, false),
    UNCANONICALIZED_ID = 205 => ("uncanonicalized_id", Text, CAP_IDENTIFIER, OPT, false),
    PERSON_CENTRIC_ID = 206 => ("person_centric_id", Text, CAP_IDENTIFIER, OPT, false),
);

columns!(chat:
    ROWID = 301 => ("ROWID", RowId, NO_CAP, REQ, false),
    GUID = 302 => ("guid", Text, CAP_GUID, REQ, false),
    STYLE = 303 => ("style", Integer, NO_CAP, OPT, false),
    CHAT_IDENTIFIER = 304 => ("chat_identifier", Text, CAP_IDENTIFIER, OPT, false),
    SERVICE_NAME = 305 => ("service_name", Text, CAP_OTHER_TEXT, OPT, false),
    DISPLAY_NAME = 306 => ("display_name", Text, CAP_OTHER_TEXT, OPT, false),
    ROOM_NAME = 307 => ("room_name", Text, CAP_OTHER_TEXT, OPT, false),
    GROUP_ID = 308 => ("group_id", Text, CAP_IDENTIFIER, OPT, false),
    PROPERTIES = 309 => ("properties", BlobSize, CAP_PROPERTIES, OPT, false),
    IS_ARCHIVED = 310 => ("is_archived", Bool, NO_CAP, OPT, false),
    IS_FILTERED = 311 => ("is_filtered", Integer, NO_CAP, OPT, false),
    IS_BLACKHOLED = 312 => ("is_blackholed", Bool, NO_CAP, OPT, false),
    IS_PENDING_REVIEW = 313 => ("is_pending_review", Bool, NO_CAP, OPT, false),
    IS_RECOVERED = 314 => ("is_recovered", Bool, NO_CAP, OPT, false),
    LAST_READ_MESSAGE_TIMESTAMP = 315 => ("last_read_message_timestamp", Time, NO_CAP, OPT, false),
);

columns!(chat_message_join:
    CHAT_ID = 401 => ("chat_id", Integer, NO_CAP, REQ, false),
    MESSAGE_ID = 402 => ("message_id", Integer, NO_CAP, REQ, false),
    MESSAGE_DATE = 403 => ("message_date", Time, NO_CAP, OPT, false),
);

columns!(chat_handle_join:
    CHAT_ID = 501 => ("chat_id", Integer, NO_CAP, REQ, false),
    HANDLE_ID = 502 => ("handle_id", Integer, NO_CAP, REQ, false),
);

columns!(message_attachment_join:
    MESSAGE_ID = 601 => ("message_id", Integer, NO_CAP, REQ, false),
    ATTACHMENT_ID = 602 => ("attachment_id", Integer, NO_CAP, REQ, false),
);

columns!(attachment:
    ROWID = 701 => ("ROWID", RowId, NO_CAP, REQ, false),
    GUID = 702 => ("guid", Text, CAP_GUID, REQ, false),
    FILENAME = 703 => ("filename", Text, CAP_IDENTIFIER, OPT, false),
    UTI = 704 => ("uti", Text, CAP_OTHER_TEXT, OPT, false),
    MIME_TYPE = 705 => ("mime_type", Text, CAP_OTHER_TEXT, OPT, false),
    TRANSFER_NAME = 706 => ("transfer_name", Text, CAP_IDENTIFIER, OPT, false),
    TOTAL_BYTES = 707 => ("total_bytes", Integer, NO_CAP, OPT, false),
    IS_STICKER = 708 => ("is_sticker", Bool, NO_CAP, OPT, false),
    HIDE_ATTACHMENT = 709 => ("hide_attachment", Bool, NO_CAP, OPT, false),
    IS_OUTGOING = 710 => ("is_outgoing", Bool, NO_CAP, OPT, false),
    TRANSFER_STATE = 711 => ("transfer_state", Integer, NO_CAP, OPT, false),
    CREATED_DATE = 712 => ("created_date", Time, NO_CAP, OPT, false),
    START_DATE = 713 => ("start_date", Time, NO_CAP, OPT, false),
    EMOJI_IMAGE_SHORT_DESCRIPTION = 714 => ("emoji_image_short_description", Text, CAP_OTHER_TEXT, OPT, false),
    ORIGINAL_GUID = 715 => ("original_guid", Text, CAP_GUID, OPT, false),
);

columns!(chat_recoverable_message_join:
    CHAT_ID = 801 => ("chat_id", Integer, NO_CAP, REQ, false),
    MESSAGE_ID = 802 => ("message_id", Integer, NO_CAP, REQ, false),
    DELETE_DATE = 803 => ("delete_date", Time, NO_CAP, OPT, false),
    CK_SYNC_STATE = 804 => ("ck_sync_state", Integer, NO_CAP, OPT, false),
);

columns!(sqlite_database_properties:
    KEY = 901 => ("key", Text, CAP_OTHER_TEXT, OPT, false),
    VALUE = 902 => ("value", Text, CAP_OTHER_TEXT, OPT, false),
);

/// A table the check knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum KnownTable {
    /// `message` (required).
    Message,
    /// `handle` (required).
    Handle,
    /// `chat` (required).
    Chat,
    /// `chat_message_join` (required).
    ChatMessageJoin,
    /// `chat_handle_join` (required).
    ChatHandleJoin,
    /// `message_attachment_join` (optional).
    MessageAttachmentJoin,
    /// `attachment` (optional).
    Attachment,
    /// `chat_recoverable_message_join` (optional; Recently Deleted).
    ChatRecoverableMessageJoin,
    /// `recoverable_message_part` (optional; `count(*)` only, and no column
    /// of it is ever named).
    RecoverableMessagePart,
    /// `deleted_messages` (optional; `count(*)` only).
    DeletedMessages,
    /// `sync_deleted_messages` (optional; `count(*)` only).
    SyncDeletedMessages,
    /// `sync_deleted_chats` (optional; `count(*)` only).
    SyncDeletedChats,
    /// `sync_deleted_attachments` (optional; `count(*)` only).
    SyncDeletedAttachments,
    /// `unsynced_removed_recoverable_messages` (optional; `count(*)` only).
    UnsyncedRemovedRecoverableMessages,
    /// `_SqliteDatabaseProperties` (optional; the `_ClientVersion` value).
    SqliteDatabaseProperties,
}

/// How a known table's rows are keyed and read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TableLayout {
    /// Keyed by a declared `ROWID INTEGER PRIMARY KEY` alias.
    RowidAlias,
    /// A join table keyed by its implicit `rowid`.
    Join,
    /// Only `count(*)` is read.
    CountOnly,
    /// `_SqliteDatabaseProperties`: one key is read.
    Properties,
}

impl KnownTable {
    /// Every known table, in probe order.
    pub const ALL: [KnownTable; 15] = [
        KnownTable::Message,
        KnownTable::Handle,
        KnownTable::Chat,
        KnownTable::ChatMessageJoin,
        KnownTable::ChatHandleJoin,
        KnownTable::MessageAttachmentJoin,
        KnownTable::Attachment,
        KnownTable::ChatRecoverableMessageJoin,
        KnownTable::RecoverableMessagePart,
        KnownTable::DeletedMessages,
        KnownTable::SyncDeletedMessages,
        KnownTable::SyncDeletedChats,
        KnownTable::SyncDeletedAttachments,
        KnownTable::UnsyncedRemovedRecoverableMessages,
        KnownTable::SqliteDatabaseProperties,
    ];

    /// The table's name in sms.db.
    pub const fn name(self) -> &'static str {
        match self {
            KnownTable::Message => "message",
            KnownTable::Handle => "handle",
            KnownTable::Chat => "chat",
            KnownTable::ChatMessageJoin => "chat_message_join",
            KnownTable::ChatHandleJoin => "chat_handle_join",
            KnownTable::MessageAttachmentJoin => "message_attachment_join",
            KnownTable::Attachment => "attachment",
            KnownTable::ChatRecoverableMessageJoin => "chat_recoverable_message_join",
            KnownTable::RecoverableMessagePart => "recoverable_message_part",
            KnownTable::DeletedMessages => "deleted_messages",
            KnownTable::SyncDeletedMessages => "sync_deleted_messages",
            KnownTable::SyncDeletedChats => "sync_deleted_chats",
            KnownTable::SyncDeletedAttachments => "sync_deleted_attachments",
            KnownTable::UnsyncedRemovedRecoverableMessages => {
                "unsynced_removed_recoverable_messages"
            }
            KnownTable::SqliteDatabaseProperties => "_SqliteDatabaseProperties",
        }
    }

    /// Whether a database without this table is unsupported.
    pub const fn required(self) -> bool {
        matches!(
            self,
            KnownTable::Message
                | KnownTable::Handle
                | KnownTable::Chat
                | KnownTable::ChatMessageJoin
                | KnownTable::ChatHandleJoin
        )
    }

    /// The position in [`KnownTable::ALL`].
    pub(crate) const fn index(self) -> usize {
        match self {
            KnownTable::Message => 0,
            KnownTable::Handle => 1,
            KnownTable::Chat => 2,
            KnownTable::ChatMessageJoin => 3,
            KnownTable::ChatHandleJoin => 4,
            KnownTable::MessageAttachmentJoin => 5,
            KnownTable::Attachment => 6,
            KnownTable::ChatRecoverableMessageJoin => 7,
            KnownTable::RecoverableMessagePart => 8,
            KnownTable::DeletedMessages => 9,
            KnownTable::SyncDeletedMessages => 10,
            KnownTable::SyncDeletedChats => 11,
            KnownTable::SyncDeletedAttachments => 12,
            KnownTable::UnsyncedRemovedRecoverableMessages => 13,
            KnownTable::SqliteDatabaseProperties => 14,
        }
    }

    /// The base of this table's column codes: every column of the table
    /// has a [`KnownColumn::code`] in `base + 1 ..= base + 99`. Stable,
    /// like the codes; 0 for the tables whose columns are never read.
    pub const fn code_base(self) -> u16 {
        match self {
            KnownTable::Message => 100,
            KnownTable::Handle => 200,
            KnownTable::Chat => 300,
            KnownTable::ChatMessageJoin => 400,
            KnownTable::ChatHandleJoin => 500,
            KnownTable::MessageAttachmentJoin => 600,
            KnownTable::Attachment => 700,
            KnownTable::ChatRecoverableMessageJoin => 800,
            KnownTable::SqliteDatabaseProperties => 900,
            KnownTable::RecoverableMessagePart
            | KnownTable::DeletedMessages
            | KnownTable::SyncDeletedMessages
            | KnownTable::SyncDeletedChats
            | KnownTable::SyncDeletedAttachments
            | KnownTable::UnsyncedRemovedRecoverableMessages => 0,
        }
    }

    pub(crate) const fn layout(self) -> TableLayout {
        match self {
            KnownTable::Message
            | KnownTable::Handle
            | KnownTable::Chat
            | KnownTable::Attachment => TableLayout::RowidAlias,
            KnownTable::ChatMessageJoin
            | KnownTable::ChatHandleJoin
            | KnownTable::MessageAttachmentJoin
            | KnownTable::ChatRecoverableMessageJoin => TableLayout::Join,
            KnownTable::RecoverableMessagePart
            | KnownTable::DeletedMessages
            | KnownTable::SyncDeletedMessages
            | KnownTable::SyncDeletedChats
            | KnownTable::SyncDeletedAttachments
            | KnownTable::UnsyncedRemovedRecoverableMessages => TableLayout::CountOnly,
            KnownTable::SqliteDatabaseProperties => TableLayout::Properties,
        }
    }

    pub(crate) const fn columns(self) -> &'static [ColumnSpec] {
        match self {
            KnownTable::Message => message::COLUMNS,
            KnownTable::Handle => handle::COLUMNS,
            KnownTable::Chat => chat::COLUMNS,
            KnownTable::ChatMessageJoin => chat_message_join::COLUMNS,
            KnownTable::ChatHandleJoin => chat_handle_join::COLUMNS,
            KnownTable::MessageAttachmentJoin => message_attachment_join::COLUMNS,
            KnownTable::Attachment => attachment::COLUMNS,
            KnownTable::ChatRecoverableMessageJoin => chat_recoverable_message_join::COLUMNS,
            KnownTable::SqliteDatabaseProperties => sqlite_database_properties::COLUMNS,
            KnownTable::RecoverableMessagePart
            | KnownTable::DeletedMessages
            | KnownTable::SyncDeletedMessages
            | KnownTable::SyncDeletedChats
            | KnownTable::SyncDeletedAttachments
            | KnownTable::UnsyncedRemovedRecoverableMessages => &[],
        }
    }

    /// Every known column of this table, in list order.
    pub fn known_columns(self) -> impl Iterator<Item = KnownColumn> {
        (0..self.columns().len()).filter_map(move |i| {
            u8::try_from(i)
                .ok()
                .map(|index| KnownColumn::new(self, index))
        })
    }

    /// The sum of the caps of all the table's columns (§5.4); checked
    /// against `ROW_BYTES_BUDGET` at compile time.
    pub const fn row_cap_bytes(self) -> u64 {
        let cols = self.columns();
        let mut sum: u64 = 0;
        let mut i = 0;
        while i < cols.len() {
            // Indexing in a const fn: `i < len` is checked by the loop.
            #[allow(clippy::indexing_slicing)]
            let c = &cols[i];
            sum = sum.saturating_add(c.row_bytes());
            i += 1;
        }
        sum
    }
}

// Every table fits a `KnownColumnSet` (64 bits) and a `u8` index. Every
// column code lies in its table's range (so codes are unique across tables)
// and is unique within its table.
#[allow(clippy::indexing_slicing)]
const _: () = {
    let all = KnownTable::ALL;
    let mut i = 0;
    while i < all.len() {
        let cols = all[i].columns();
        assert!(cols.len() <= 64);
        assert!(all[i].index() == i);
        let base = all[i].code_base();
        let mut j = 0;
        while j < cols.len() {
            let code = cols[j].code;
            assert!(base > 0 && code > base && code < base + 100);
            let mut k = j + 1;
            while k < cols.len() {
                assert!(cols[k].code != code, "a column code is used twice");
                k += 1;
            }
            j += 1;
        }
        i += 1;
    }
};

/// One known column of one known table.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KnownColumn {
    table: KnownTable,
    index: u8,
}

impl KnownColumn {
    pub(crate) const fn new(table: KnownTable, index: u8) -> Self {
        KnownColumn { table, index }
    }

    pub(crate) fn spec(self) -> Option<&'static ColumnSpec> {
        self.table.columns().get(usize::from(self.index))
    }

    /// The column's table.
    pub fn table(self) -> KnownTable {
        self.table
    }

    /// The column's name.
    pub fn name(self) -> &'static str {
        self.spec().map_or("", |s| s.name)
    }

    /// How the column is read.
    pub fn kind(self) -> Option<ColumnKind> {
        self.spec().map(|s| s.kind)
    }

    /// Whether a present table without this column is unsupported.
    pub fn required(self) -> bool {
        self.spec().is_some_and(|s| s.required)
    }

    /// The byte cap, for text and blob columns.
    pub fn cap(self) -> Option<u32> {
        self.spec().and_then(|s| (s.cap > 0).then_some(s.cap))
    }

    /// Whether a zero value is recorded as meaning "unset" (only
    /// `date_delivered` and `date_edited`; S3, observed).
    pub fn zero_means_unset(self) -> bool {
        self.spec().is_some_and(|s| s.zero_means_unset)
    }

    pub(crate) fn index(self) -> u8 {
        self.index
    }

    /// The column's stable numeric code: `table.code_base()` plus a number
    /// written beside the column in the static list. A code is never
    /// reused or renumbered, so it can be stored (M4 `value_issue.col`); a
    /// column removed from the list retires its code. `tests` pins every
    /// code.
    pub fn code(self) -> u16 {
        self.spec().map_or(0, |s| s.code)
    }

    /// The column with this code, if the list has one.
    pub fn from_code(code: u16) -> Option<KnownColumn> {
        KnownTable::ALL
            .into_iter()
            .flat_map(KnownTable::known_columns)
            .find(|c| c.code() == code)
    }
}

impl fmt::Debug for KnownColumn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Both names are ours (static), never the database's.
        write!(f, "{}.{}", self.table.name(), self.name())
    }
}

/// A set of the known columns of one table.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct KnownColumnSet(u64);

impl KnownColumnSet {
    /// Whether the set holds the column with `c`'s index. The set does not
    /// record its table; ask the [`crate::TableCaps`] it came from.
    pub fn contains(&self, c: KnownColumn) -> bool {
        self.contains_index(c.index)
    }

    pub(crate) fn contains_index(&self, index: u8) -> bool {
        1u64.checked_shl(u32::from(index))
            .is_some_and(|bit| self.0 & bit != 0)
    }

    pub(crate) fn insert(&mut self, index: u8) {
        if let Some(bit) = 1u64.checked_shl(u32::from(index)) {
            self.0 |= bit;
        }
    }

    /// The number of columns in the set.
    pub fn len(&self) -> u32 {
        self.0.count_ones()
    }

    /// `true` when no column is in the set.
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

impl fmt::Debug for KnownColumnSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KnownColumnSet({:#x})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names are written into SQL inside double quotes, so none may hold a
    /// quote or a NUL (§6.1).
    #[test]
    fn names_are_safe_to_quote() {
        for t in KnownTable::ALL {
            for name in std::iter::once(t.name()).chain(t.columns().iter().map(|c| c.name)) {
                assert!(!name.is_empty());
                assert!(!name.contains('"') && !name.contains('\0'), "{name}");
                assert!(name.is_ascii());
            }
        }
    }

    #[test]
    fn column_names_are_unique_per_table() {
        for t in KnownTable::ALL {
            let mut names: Vec<String> = t
                .columns()
                .iter()
                .map(|c| c.name.to_ascii_lowercase())
                .collect();
            names.sort();
            let before = names.len();
            names.dedup();
            assert_eq!(names.len(), before, "{t:?}");
        }
    }

    #[test]
    fn layouts_match_their_columns() {
        for t in KnownTable::ALL {
            let cols = t.columns();
            match t.layout() {
                TableLayout::RowidAlias => {
                    let first = cols.first().expect("a rowid table has columns");
                    assert_eq!((first.name, first.kind), ("ROWID", ColumnKind::RowId));
                    assert!(first.required);
                }
                TableLayout::Join => {
                    assert!(cols.iter().all(|c| c.kind != ColumnKind::RowId));
                    assert!(cols.iter().filter(|c| c.required).count() == 2);
                }
                TableLayout::CountOnly => assert!(cols.is_empty()),
                TableLayout::Properties => assert!(cols.iter().all(|c| !c.required)),
            }
            // Only rowid tables have a RowId column, and only once.
            let rowids = cols.iter().filter(|c| c.kind == ColumnKind::RowId).count();
            assert_eq!(rowids, usize::from(t.layout() == TableLayout::RowidAlias));
            // Capped kinds have a cap; others have none.
            for c in cols {
                let capped = matches!(
                    c.kind,
                    ColumnKind::Text
                        | ColumnKind::Blob
                        | ColumnKind::BlobSize
                        | ColumnKind::BlobOnRequest
                );
                assert_eq!(capped, c.cap > 0, "{}", c.name);
            }
        }
    }

    #[test]
    fn the_required_message_columns_are_s2s() {
        let req: Vec<&str> = KnownTable::Message
            .known_columns()
            .filter(|c| c.required())
            .map(KnownColumn::name)
            .collect();
        assert_eq!(req, ["ROWID", "guid", "date", "is_from_me"]);
        let zero: Vec<String> = KnownTable::ALL
            .iter()
            .flat_map(|t| t.known_columns())
            .filter(|c| c.zero_means_unset())
            .map(|c| format!("{c:?}"))
            .collect();
        assert_eq!(zero, ["message.date_delivered", "message.date_edited"]);
    }

    #[test]
    fn index_constants_follow_the_list() {
        let c = KnownColumn::new(KnownTable::Chat, chat::PROPERTIES);
        assert_eq!(c.name(), "properties");
        assert_eq!(c.kind(), Some(ColumnKind::BlobSize));
        assert_eq!(c.cap(), Some(CAP_PROPERTIES));
        assert_eq!(
            KnownColumn::new(KnownTable::Message, message::WAS_DOWNGRADED).name(),
            "was_downgraded"
        );
        assert_eq!(
            KnownColumn::new(KnownTable::Handle, handle::PERSON_CENTRIC_ID).name(),
            "person_centric_id"
        );
        assert_eq!(
            KnownColumn::new(
                KnownTable::ChatRecoverableMessageJoin,
                chat_recoverable_message_join::CK_SYNC_STATE
            )
            .name(),
            "ck_sync_state"
        );
        let mut set = KnownColumnSet::default();
        assert!(set.is_empty());
        set.insert(chat::PROPERTIES);
        set.insert(63);
        set.insert(64); // out of range: ignored
        assert!(set.contains(c));
        assert_eq!(set.len(), 2);
    }

    /// Every column code, pinned. A failure here means a code changed or was
    /// reused: never edit an existing line; add a new column with a new
    /// code, and retire the codes of removed columns.
    #[test]
    fn column_codes_are_pinned() {
        const PINNED: &[(u16, &str)] = &[
            (101, "message.ROWID"),
            (102, "message.guid"),
            (103, "message.date"),
            (104, "message.is_from_me"),
            (105, "message.text"),
            (106, "message.attributedBody"),
            (107, "message.service"),
            (108, "message.handle_id"),
            (109, "message.other_handle"),
            (110, "message.destination_caller_id"),
            (111, "message.subject"),
            (112, "message.date_read"),
            (113, "message.date_delivered"),
            (114, "message.date_played"),
            (115, "message.date_edited"),
            (116, "message.date_retracted"),
            (117, "message.is_read"),
            (118, "message.is_delivered"),
            (119, "message.is_sent"),
            (120, "message.is_audio_message"),
            (121, "message.cache_has_attachments"),
            (122, "message.item_type"),
            (123, "message.group_action_type"),
            (124, "message.group_title"),
            (125, "message.share_status"),
            (126, "message.share_direction"),
            (127, "message.associated_message_guid"),
            (128, "message.associated_message_type"),
            (129, "message.associated_message_emoji"),
            (130, "message.associated_message_range_location"),
            (131, "message.associated_message_range_length"),
            (132, "message.balloon_bundle_id"),
            (133, "message.expressive_send_style_id"),
            (134, "message.payload_data"),
            (135, "message.message_summary_info"),
            (136, "message.thread_originator_guid"),
            (137, "message.thread_originator_part"),
            (138, "message.reply_to_guid"),
            (139, "message.part_count"),
            (140, "message.schedule_type"),
            (141, "message.schedule_state"),
            (142, "message.filter_action"),
            (143, "message.filter_sub_action"),
            (144, "message.error"),
            (145, "message.was_downgraded"),
            (201, "handle.ROWID"),
            (202, "handle.id"),
            (203, "handle.service"),
            (204, "handle.country"),
            (205, "handle.uncanonicalized_id"),
            (206, "handle.person_centric_id"),
            (301, "chat.ROWID"),
            (302, "chat.guid"),
            (303, "chat.style"),
            (304, "chat.chat_identifier"),
            (305, "chat.service_name"),
            (306, "chat.display_name"),
            (307, "chat.room_name"),
            (308, "chat.group_id"),
            (309, "chat.properties"),
            (310, "chat.is_archived"),
            (311, "chat.is_filtered"),
            (312, "chat.is_blackholed"),
            (313, "chat.is_pending_review"),
            (314, "chat.is_recovered"),
            (315, "chat.last_read_message_timestamp"),
            (401, "chat_message_join.chat_id"),
            (402, "chat_message_join.message_id"),
            (403, "chat_message_join.message_date"),
            (501, "chat_handle_join.chat_id"),
            (502, "chat_handle_join.handle_id"),
            (601, "message_attachment_join.message_id"),
            (602, "message_attachment_join.attachment_id"),
            (701, "attachment.ROWID"),
            (702, "attachment.guid"),
            (703, "attachment.filename"),
            (704, "attachment.uti"),
            (705, "attachment.mime_type"),
            (706, "attachment.transfer_name"),
            (707, "attachment.total_bytes"),
            (708, "attachment.is_sticker"),
            (709, "attachment.hide_attachment"),
            (710, "attachment.is_outgoing"),
            (711, "attachment.transfer_state"),
            (712, "attachment.created_date"),
            (713, "attachment.start_date"),
            (714, "attachment.emoji_image_short_description"),
            (715, "attachment.original_guid"),
            (801, "chat_recoverable_message_join.chat_id"),
            (802, "chat_recoverable_message_join.message_id"),
            (803, "chat_recoverable_message_join.delete_date"),
            (804, "chat_recoverable_message_join.ck_sync_state"),
            (901, "_SqliteDatabaseProperties.key"),
            (902, "_SqliteDatabaseProperties.value"),
        ];
        let all: Vec<(u16, String)> = KnownTable::ALL
            .iter()
            .flat_map(|t| t.known_columns())
            .map(|c| (c.code(), format!("{c:?}")))
            .collect();
        let pinned: Vec<(u16, String)> =
            PINNED.iter().map(|(c, n)| (*c, (*n).to_owned())).collect();
        assert_eq!(all, pinned);
        let mut codes: Vec<u16> = all.iter().map(|(c, _)| *c).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), all.len(), "codes are unique");
        for t in KnownTable::ALL {
            for c in t.known_columns() {
                assert_eq!(KnownColumn::from_code(c.code()), Some(c));
                assert_eq!(c.code() / 100, t.code_base() / 100);
            }
        }
        for unknown in [0, 1, 100, 199, 300, 999, u16::MAX] {
            assert_eq!(KnownColumn::from_code(unknown), None, "{unknown}");
        }
    }
}
