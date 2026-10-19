//! The viewer's reads (M5 spec §3): lock-free, query-only methods on
//! [`crate::ArchiveReader`] that work while an import runs.
//!
//! - Every method checks the schema version, then opens **one** `BEGIN
//!   DEFERRED … COMMIT` read transaction around its phases (one snapshot
//!   per call, never one across calls), and runs each phase under its own
//!   [`ViewBudget`](crate::conn::ViewBudget): a broad query ends in
//!   `QueryTooBroad` or `Cancelled`, never `Damaged`.
//! - Ids are archive row numbers. Rows are never deleted (D57), so an id is
//!   stable for the archive's life.
//! - Content leaves as [`OptContent`]/[`ContentText`] (`Redacted` inside),
//!   so `Debug` never prints it. Sort keys never leave the crate.
//! - "Recent", "newest" and "on or after" are bounded to the dated range
//!   (`sort_key < X'01'`, [`crate::sortkey`]): an undated row never poses
//!   as the newest.
//!
//! Slice 5b1 adds the conversation list ([`conversations`]); slice 5b2 the
//! keyset thread pages and the reply thread ([`thread`]) over one assembly
//! layer ([`assemble`]); slice 5b3 the message detail with edit texts
//! decoded at read time, the reaction history, `message_text`, value
//! history and the attachment facts and cards ([`detail`]), and date seek
//! and day counts ([`calendar`]). Search and media follow in later slices.

pub mod assemble;
pub mod calendar;
pub mod conversations;
pub mod detail;
pub mod text;
pub mod thread;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension};

pub use assemble::{
    AssembledMessage, AssocVisibility, AssociationFacts, AttachmentRef, BodyText, BodyView,
    ChipSender, ChipState, EarlierText, EditFacts, Formatting, GroupEvent, MessageLabel, NoText,
    ReactionChip, ReactionKind, ReactionSummary, RecentlyDeletedMarker, RecordedBy, RecordedTime,
    ReplyRef, Reveal, Segment, SegmentKind, SenderReason, WithheldReason, EMOJI_BYTES, PAGE_BYTES,
    QUOTE_UTF16,
};
pub use calendar::{DayCounts, SeekDir, DAY_BOUNDS};
pub use conversations::{
    ChatLabel, ChatStatsFacts, Conversation, ConversationIndex, ConversationSort, NameFilter,
    NameFilterError, Participant, Preview, PreviewBody, CONVERSATIONS_PER_CALL,
    CONVERSATION_INDEX_LIMIT, LABEL_PARTICIPANTS, PREVIEW_SCAN_ROWS,
};
pub use detail::{
    AttachmentFacts, BlobFacts, BodyVersion, CardState, ChatFlags, ChunkText, DetailTimes,
    EditEntryText, EditEntryView, EditTextsOutcome, EditTime, HistoryColumn, HistoryValue,
    IdentifierProbe, MessageDetail, Observation, OlderBackup, RawNumber, RawValue, ReactionHistory,
    ReactionRecord, TextChunk, VerifyFacts, VersionText, DETAIL_CHATS, DETAIL_EDIT_ENTRIES,
    DETAIL_VERSIONS, REACTION_HISTORY_LIMIT, REACTION_HISTORY_PAGE, TEXT_CHUNK_UTF16,
};
pub use text::{
    ContentText, MaskKind, OptContent, TextPiece, UrlRun, BODY_PAGE_UTF16, IDENTIFIER_UTF16,
    PREVIEW_UTF16,
};
pub use thread::{
    Anchor, Direction, Focus, FocusReason, PageRange, SortKeyHint, ThreadPage, PAGE_HANDLES,
    PAGE_ITEMS, REPLY_THREAD_LIMIT,
};

use crate::schema::codes::{AssocKindCode, Entity};

macro_rules! row_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(i64);

        impl $name {
            /// The id with this row number.
            pub fn new(id: i64) -> $name {
                $name(id)
            }

            /// The row number.
            pub fn get(self) -> i64 {
                self.0
            }
        }
    };
}

row_id! {
    /// The id of a `chat` row: one conversation.
    ChatId
}
row_id! {
    /// The id of a `message` row.
    MessageId
}
row_id! {
    /// The id of a `handle` row: one recorded address and service.
    HandleId
}
row_id! {
    /// The id of a `message_body` row: one stored version of a body.
    BodyId
}
row_id! {
    /// The id of an `attachment` row.
    AttachmentId
}

/// A count read with a `LIMIT` of one more than its cap: `capped` when
/// there were more rows than the cap (the UI then says "N+").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CappedCount {
    pub n: u64,
    pub capped: bool,
}

impl CappedCount {
    /// `count` rows read with a limit of `cap + 1`.
    pub(crate) fn of(count: i64, cap: u64) -> CappedCount {
        let n = u64::try_from(count).unwrap_or(0);
        CappedCount {
            n: n.min(cap),
            capped: n > cap,
        }
    }

    /// The `LIMIT` that reads a count capped at `cap`.
    pub(crate) fn limit(cap: u64) -> i64 {
        i64::try_from(cap.saturating_add(1)).unwrap_or(i64::MAX)
    }
}

/// `message.is_from_me` as the viewer shows it (§3.6). Read, delivery and
/// sent state are never shown (D94).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DirectionCode {
    /// `is_from_me = 1`: sent from the backed-up phone's account.
    You,
    /// `is_from_me = 0`.
    Received,
    /// NULL, absent, unreadable, or a value other than 0 and 1.
    NotRecorded,
}

impl DirectionCode {
    pub(crate) fn of(is_from_me: Option<i64>, st: Option<i64>) -> DirectionCode {
        match (is_from_me, st) {
            (Some(1), None) => DirectionCode::You,
            (Some(0), None) => DirectionCode::Received,
            _ => DirectionCode::NotRecorded,
        }
    }
}

/// `true` for the tapback codes that fold into their target (§3.5 step 3):
/// 10–17 (add) and 20–27 (remove).
pub(crate) fn is_reaction(kind: Option<i64>) -> bool {
    matches!(kind, Some(10..=17 | 20..=27))
}

/// A closed code read from a column; `None` for NULL or a code this build
/// does not know.
pub(crate) fn assoc_kind(v: Option<i64>) -> Option<AssocKindCode> {
    v.and_then(AssocKindCode::from_code)
}

/// `value_issue` kind 3: SQLite reported the value as a BLOB in a text
/// column (R7).
const TEXT_STORED_AS_BLOB: i64 = 3;

/// `ArchiveColumn` codes the viewer reads issues for (checked against
/// `ARCHIVE_COLUMNS` by a test).
pub(crate) const COL_HANDLE_ADDRESS: i64 = 101;
pub(crate) const COL_CHAT_IDENTIFIER: i64 = 203;
pub(crate) const COL_CHAT_DISPLAY_NAME: i64 = 205;
pub(crate) const COL_BODY_TEXT_RAW: i64 = 501;

const SQL_VALUE_ISSUE: &str = "SELECT 1 FROM value_issue \
     WHERE entity = ?1 AND row_id = ?2 AND col = ?3 AND kind = ?4";

/// Whether the value of `entity` row `id`, column `col`, was stored as a
/// BLOB in the source.
pub(crate) fn stored_as_blob(
    c: &Connection,
    entity: Entity,
    id: i64,
    col: i64,
) -> rusqlite::Result<bool> {
    c.prepare(SQL_VALUE_ISSUE)?
        .query_row(
            rusqlite::params![entity.code(), id, col, TEXT_STORED_AS_BLOB],
            |_| Ok(()),
        )
        .optional()
        .map(|r| r.is_some())
}

const SQL_FOLD_ROW: &str =
    "SELECT assoc_present, assoc_kind, assoc_target_guid FROM message WHERE id = ?1";
const SQL_KEYED_BY_GUID: &str = "SELECT id FROM message WHERE key_kind = 1 AND guid = ?1";
const SQL_IN_CHAT: &str = "SELECT 1 FROM chat_message WHERE chat_id = ?1 AND message_id = ?2";

/// The target of message `id` when it folds into chat `chat`'s timeline
/// (§3.5 step 3): a reaction (`assoc_present = 1`, kind 10–17 or 20–27)
/// whose target GUID resolves through `message_guid` (`key_kind = 1`) to a
/// message that is a timeline item of the same chat. `None` when the row
/// stays visible.
pub(crate) fn folds_into(c: &Connection, chat: i64, id: i64) -> rusqlite::Result<Option<i64>> {
    let row: Option<(i64, Option<i64>, Option<Vec<u8>>)> = c
        .prepare(SQL_FOLD_ROW)?
        .query_row([id], |r| {
            let guid = match r.get_ref(2)? {
                ValueRef::Blob(b) | ValueRef::Text(b) => Some(b.to_vec()),
                _ => None,
            };
            Ok((r.get(0)?, r.get(1)?, guid))
        })
        .optional()?;
    let Some((present, kind, Some(guid))) = row else {
        return Ok(None);
    };
    if present != 1 || !is_reaction(kind) {
        return Ok(None);
    }
    let target: Option<i64> = c
        .prepare(SQL_KEYED_BY_GUID)?
        .query_row([guid], |r| r.get(0))
        .optional()?;
    let Some(target) = target else {
        return Ok(None);
    };
    let in_chat = c
        .prepare(SQL_IN_CHAT)?
        .query_row([chat, target], |_| Ok(()))
        .optional()?
        .is_some();
    Ok(in_chat.then_some(target))
}

/// Every viewer statement whose plan depends on an index, by name, exactly
/// as the code prepares it (pinned in `tests/query_plans.rs`).
#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    let mut v: Vec<(&'static str, String)> = vec![
        ("viewer value issue", SQL_VALUE_ISSUE.to_owned()),
        ("viewer fold row", SQL_FOLD_ROW.to_owned()),
        ("viewer keyed message by guid", SQL_KEYED_BY_GUID.to_owned()),
        ("viewer timeline membership", SQL_IN_CHAT.to_owned()),
    ];
    v.extend(conversations::plan_statements());
    v.extend(thread::plan_statements());
    v.extend(assemble::plan_statements());
    v.extend(detail::plan_statements());
    v.extend(calendar::plan_statements());
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::codes::{ArchiveColumn, ATTR_FLAG_BITS};

    #[test]
    fn column_codes_name_the_columns_they_claim() {
        for (code, entity, name) in [
            (COL_HANDLE_ADDRESS, Entity::Handle, "address"),
            (COL_CHAT_IDENTIFIER, Entity::Chat, "chat_identifier"),
            (COL_CHAT_DISPLAY_NAME, Entity::Chat, "display_name"),
            (COL_BODY_TEXT_RAW, Entity::MessageBody, "text_raw"),
        ] {
            let c = ArchiveColumn::from_code(u16::try_from(code).expect("u16")).expect("known");
            assert_eq!((c.entity, c.name), (entity, name));
        }
        assert_eq!(
            ATTR_FLAG_BITS[conversations::ONE_TIME_CODE_BIT],
            "one_time_code"
        );
    }

    #[test]
    fn reactions_are_the_tapback_codes_only() {
        let folding: Vec<i64> = AssocKindCode::ALL
            .iter()
            .map(|k| k.code())
            .filter(|k| is_reaction(Some(*k)))
            .collect();
        assert_eq!(
            folding,
            vec![10, 11, 12, 13, 14, 15, 16, 17, 20, 21, 22, 23, 24, 25, 26, 27]
        );
        assert!(!is_reaction(None));
        assert!(!is_reaction(Some(18)));
    }

    #[test]
    fn direction_and_capped_counts() {
        assert_eq!(DirectionCode::of(Some(1), None), DirectionCode::You);
        assert_eq!(DirectionCode::of(Some(0), None), DirectionCode::Received);
        assert_eq!(DirectionCode::of(Some(2), None), DirectionCode::NotRecorded);
        assert_eq!(DirectionCode::of(None, Some(1)), DirectionCode::NotRecorded);
        assert_eq!(
            CappedCount::of(3, 10),
            CappedCount {
                n: 3,
                capped: false
            }
        );
        assert_eq!(
            CappedCount::of(10, 10),
            CappedCount {
                n: 10,
                capped: false
            }
        );
        assert_eq!(
            CappedCount::of(11, 10),
            CappedCount {
                n: 10,
                capped: true
            }
        );
        assert_eq!(CappedCount::limit(10), 11);
    }
}
