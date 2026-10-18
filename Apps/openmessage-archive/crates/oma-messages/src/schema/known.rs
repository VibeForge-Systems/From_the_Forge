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
    CAP_ATTRIBUTED_BODY, CAP_GUID, CAP_IDENTIFIER, CAP_OTHER_TEXT, CAP_PROPERTIES, CAP_SUMMARY,
    CAP_TEXT, INTEGER_BYTES,
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
}

/// One known column.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ColumnSpec {
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
            ColumnKind::Text | ColumnKind::Blob => self.cap as u64,
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
/// order, and the `COLUMNS` slice in the same order.
macro_rules! columns {
    ($m:ident: $( $c:ident = ($name:literal, $kind:ident, $cap:expr, $req:expr, $zero:expr) ),* $(,)?) => {
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
    ROWID = ("ROWID", RowId, NO_CAP, REQ, false),
    GUID = ("guid", Text, CAP_GUID, REQ, false),
    DATE = ("date", Time, NO_CAP, REQ, false),
    IS_FROM_ME = ("is_from_me", Bool, NO_CAP, REQ, false),
    TEXT = ("text", Text, CAP_TEXT, OPT, false),
    ATTRIBUTED_BODY = ("attributedBody", Blob, CAP_ATTRIBUTED_BODY, OPT, false),
    SERVICE = ("service", Text, CAP_OTHER_TEXT, OPT, false),
    HANDLE_ID = ("handle_id", Integer, NO_CAP, OPT, false),
    OTHER_HANDLE = ("other_handle", Integer, NO_CAP, OPT, false),
    DESTINATION_CALLER_ID = ("destination_caller_id", Text, CAP_IDENTIFIER, OPT, false),
    SUBJECT = ("subject", Text, CAP_OTHER_TEXT, OPT, false),
    DATE_READ = ("date_read", Time, NO_CAP, OPT, false),
    DATE_DELIVERED = ("date_delivered", Time, NO_CAP, OPT, true),
    DATE_PLAYED = ("date_played", Time, NO_CAP, OPT, false),
    DATE_EDITED = ("date_edited", Time, NO_CAP, OPT, true),
    DATE_RETRACTED = ("date_retracted", Time, NO_CAP, OPT, false),
    IS_READ = ("is_read", Bool, NO_CAP, OPT, false),
    IS_DELIVERED = ("is_delivered", Bool, NO_CAP, OPT, false),
    IS_SENT = ("is_sent", Bool, NO_CAP, OPT, false),
    IS_AUDIO_MESSAGE = ("is_audio_message", Bool, NO_CAP, OPT, false),
    CACHE_HAS_ATTACHMENTS = ("cache_has_attachments", Bool, NO_CAP, OPT, false),
    ITEM_TYPE = ("item_type", Integer, NO_CAP, OPT, false),
    GROUP_ACTION_TYPE = ("group_action_type", Integer, NO_CAP, OPT, false),
    GROUP_TITLE = ("group_title", Text, CAP_OTHER_TEXT, OPT, false),
    SHARE_STATUS = ("share_status", Integer, NO_CAP, OPT, false),
    SHARE_DIRECTION = ("share_direction", Bool, NO_CAP, OPT, false),
    ASSOCIATED_MESSAGE_GUID = ("associated_message_guid", Text, CAP_GUID, OPT, false),
    ASSOCIATED_MESSAGE_TYPE = ("associated_message_type", Integer, NO_CAP, OPT, false),
    ASSOCIATED_MESSAGE_EMOJI = ("associated_message_emoji", Text, CAP_OTHER_TEXT, OPT, false),
    ASSOCIATED_MESSAGE_RANGE_LOCATION = ("associated_message_range_location", Integer, NO_CAP, OPT, false),
    ASSOCIATED_MESSAGE_RANGE_LENGTH = ("associated_message_range_length", Integer, NO_CAP, OPT, false),
    BALLOON_BUNDLE_ID = ("balloon_bundle_id", Text, CAP_OTHER_TEXT, OPT, false),
    EXPRESSIVE_SEND_STYLE_ID = ("expressive_send_style_id", Text, CAP_OTHER_TEXT, OPT, false),
    PAYLOAD_DATA = ("payload_data", TypeOnly, NO_CAP, OPT, false),
    MESSAGE_SUMMARY_INFO = ("message_summary_info", Blob, CAP_SUMMARY, OPT, false),
    THREAD_ORIGINATOR_GUID = ("thread_originator_guid", Text, CAP_GUID, OPT, false),
    THREAD_ORIGINATOR_PART = ("thread_originator_part", Text, CAP_OTHER_TEXT, OPT, false),
    REPLY_TO_GUID = ("reply_to_guid", Text, CAP_GUID, OPT, false),
    PART_COUNT = ("part_count", Integer, NO_CAP, OPT, false),
    SCHEDULE_TYPE = ("schedule_type", Integer, NO_CAP, OPT, false),
    SCHEDULE_STATE = ("schedule_state", Integer, NO_CAP, OPT, false),
    FILTER_ACTION = ("filter_action", Integer, NO_CAP, OPT, false),
    FILTER_SUB_ACTION = ("filter_sub_action", Integer, NO_CAP, OPT, false),
    ERROR = ("error", Integer, NO_CAP, OPT, false),
    WAS_DOWNGRADED = ("was_downgraded", Bool, NO_CAP, OPT, false),
);

columns!(handle:
    ROWID = ("ROWID", RowId, NO_CAP, REQ, false),
    ID = ("id", Text, CAP_IDENTIFIER, REQ, false),
    SERVICE = ("service", Text, CAP_OTHER_TEXT, OPT, false),
    COUNTRY = ("country", Text, CAP_OTHER_TEXT, OPT, false),
    UNCANONICALIZED_ID = ("uncanonicalized_id", Text, CAP_IDENTIFIER, OPT, false),
    PERSON_CENTRIC_ID = ("person_centric_id", Text, CAP_IDENTIFIER, OPT, false),
);

columns!(chat:
    ROWID = ("ROWID", RowId, NO_CAP, REQ, false),
    GUID = ("guid", Text, CAP_GUID, REQ, false),
    STYLE = ("style", Integer, NO_CAP, OPT, false),
    CHAT_IDENTIFIER = ("chat_identifier", Text, CAP_IDENTIFIER, OPT, false),
    SERVICE_NAME = ("service_name", Text, CAP_OTHER_TEXT, OPT, false),
    DISPLAY_NAME = ("display_name", Text, CAP_OTHER_TEXT, OPT, false),
    ROOM_NAME = ("room_name", Text, CAP_OTHER_TEXT, OPT, false),
    GROUP_ID = ("group_id", Text, CAP_IDENTIFIER, OPT, false),
    PROPERTIES = ("properties", BlobSize, CAP_PROPERTIES, OPT, false),
    IS_ARCHIVED = ("is_archived", Bool, NO_CAP, OPT, false),
    IS_FILTERED = ("is_filtered", Integer, NO_CAP, OPT, false),
    IS_BLACKHOLED = ("is_blackholed", Bool, NO_CAP, OPT, false),
    IS_PENDING_REVIEW = ("is_pending_review", Bool, NO_CAP, OPT, false),
    IS_RECOVERED = ("is_recovered", Bool, NO_CAP, OPT, false),
    LAST_READ_MESSAGE_TIMESTAMP = ("last_read_message_timestamp", Time, NO_CAP, OPT, false),
);

columns!(chat_message_join:
    CHAT_ID = ("chat_id", Integer, NO_CAP, REQ, false),
    MESSAGE_ID = ("message_id", Integer, NO_CAP, REQ, false),
    MESSAGE_DATE = ("message_date", Time, NO_CAP, OPT, false),
);

columns!(chat_handle_join:
    CHAT_ID = ("chat_id", Integer, NO_CAP, REQ, false),
    HANDLE_ID = ("handle_id", Integer, NO_CAP, REQ, false),
);

columns!(message_attachment_join:
    MESSAGE_ID = ("message_id", Integer, NO_CAP, REQ, false),
    ATTACHMENT_ID = ("attachment_id", Integer, NO_CAP, REQ, false),
);

columns!(attachment:
    ROWID = ("ROWID", RowId, NO_CAP, REQ, false),
    GUID = ("guid", Text, CAP_GUID, REQ, false),
    FILENAME = ("filename", Text, CAP_IDENTIFIER, OPT, false),
    UTI = ("uti", Text, CAP_OTHER_TEXT, OPT, false),
    MIME_TYPE = ("mime_type", Text, CAP_OTHER_TEXT, OPT, false),
    TRANSFER_NAME = ("transfer_name", Text, CAP_IDENTIFIER, OPT, false),
    TOTAL_BYTES = ("total_bytes", Integer, NO_CAP, OPT, false),
    IS_STICKER = ("is_sticker", Bool, NO_CAP, OPT, false),
    HIDE_ATTACHMENT = ("hide_attachment", Bool, NO_CAP, OPT, false),
    IS_OUTGOING = ("is_outgoing", Bool, NO_CAP, OPT, false),
    TRANSFER_STATE = ("transfer_state", Integer, NO_CAP, OPT, false),
    CREATED_DATE = ("created_date", Time, NO_CAP, OPT, false),
    START_DATE = ("start_date", Time, NO_CAP, OPT, false),
    EMOJI_IMAGE_SHORT_DESCRIPTION = ("emoji_image_short_description", Text, CAP_OTHER_TEXT, OPT, false),
    ORIGINAL_GUID = ("original_guid", Text, CAP_GUID, OPT, false),
);

columns!(chat_recoverable_message_join:
    CHAT_ID = ("chat_id", Integer, NO_CAP, REQ, false),
    MESSAGE_ID = ("message_id", Integer, NO_CAP, REQ, false),
    DELETE_DATE = ("delete_date", Time, NO_CAP, OPT, false),
    CK_SYNC_STATE = ("ck_sync_state", Integer, NO_CAP, OPT, false),
);

columns!(sqlite_database_properties:
    KEY = ("key", Text, CAP_OTHER_TEXT, OPT, false),
    VALUE = ("value", Text, CAP_OTHER_TEXT, OPT, false),
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

// Every table fits a `KnownColumnSet` (64 bits) and a `u8` index.
#[allow(clippy::indexing_slicing)]
const _: () = {
    let all = KnownTable::ALL;
    let mut i = 0;
    while i < all.len() {
        assert!(all[i].columns().len() <= 64);
        assert!(all[i].index() == i);
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
                    ColumnKind::Text | ColumnKind::Blob | ColumnKind::BlobSize
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
}
