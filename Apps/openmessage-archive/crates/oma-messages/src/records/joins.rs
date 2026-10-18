//! The link tables, each keyed by its implicit `rowid` (validated by the
//! probe: no column named `rowid`, `_rowid_` or `oid`, and not
//! `WITHOUT ROWID`). M4 joins the links in its own database; record queries
//! never join.

use crate::col::Col;
use crate::db::{FromRow, RowReader};
use crate::error::{MessagesError, Stage};
use crate::progress::MessagesPhase;
use crate::raw::RawIssue;
use crate::schema::known::{chat_handle_join, chat_message_join, message_attachment_join};
use crate::schema::KnownTable;
use crate::time::CocoaTime;

use super::shape_debug;

/// One `chat_handle_join` row: a handle in a chat.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChatHandleLink {
    /// The implicit `rowid`.
    pub rowid: i64,
    /// `chat_id`.
    pub chat_id: Col<i64>,
    /// `handle_id`.
    pub handle_id: Col<i64>,
    /// The raw values of unreadable cells, when kept (M4 R2).
    pub raw_issues: Vec<RawIssue>,
}

shape_debug!(ChatHandleLink { chat_id, handle_id } kinds { raw_issues });

impl FromRow for ChatHandleLink {
    const TABLE: KnownTable = KnownTable::ChatHandleJoin;
    const PHASE: MessagesPhase = MessagesPhase::ReadingChatLinks;
    const STAGE: Stage = Stage::ChatLinks;

    fn from_row(rowid: i64, r: &mut RowReader<'_, '_>) -> Result<Self, MessagesError> {
        Ok(ChatHandleLink {
            rowid,
            chat_id: r.integer(chat_handle_join::CHAT_ID)?,
            handle_id: r.integer(chat_handle_join::HANDLE_ID)?,
            raw_issues: r.take_raw_issues(),
        })
    }
}

/// One `chat_message_join` row: a message in a chat.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChatMessageLink {
    /// The implicit `rowid`.
    pub rowid: i64,
    /// `chat_id`.
    pub chat_id: Col<i64>,
    /// `message_id`.
    pub message_id: Col<i64>,
    /// `message_date`; its unit is **UNVERIFIED**, so it is classified per
    /// value.
    pub message_date: Col<CocoaTime>,
    /// The raw values of unreadable cells, when kept (M4 R2).
    pub raw_issues: Vec<RawIssue>,
}

shape_debug!(ChatMessageLink {
    chat_id,
    message_id,
    message_date
} kinds { raw_issues });

impl FromRow for ChatMessageLink {
    const TABLE: KnownTable = KnownTable::ChatMessageJoin;
    const PHASE: MessagesPhase = MessagesPhase::ReadingChatLinks;
    const STAGE: Stage = Stage::ChatLinks;

    fn from_row(rowid: i64, r: &mut RowReader<'_, '_>) -> Result<Self, MessagesError> {
        Ok(ChatMessageLink {
            rowid,
            chat_id: r.integer(chat_message_join::CHAT_ID)?,
            message_id: r.integer(chat_message_join::MESSAGE_ID)?,
            message_date: r.time(chat_message_join::MESSAGE_DATE)?,
            raw_issues: r.take_raw_issues(),
        })
    }
}

/// One `message_attachment_join` row: an attachment of a message.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MessageAttachmentLink {
    /// The implicit `rowid`.
    pub rowid: i64,
    /// `message_id`.
    pub message_id: Col<i64>,
    /// `attachment_id`.
    pub attachment_id: Col<i64>,
    /// The raw values of unreadable cells, when kept (M4 R2).
    pub raw_issues: Vec<RawIssue>,
}

shape_debug!(MessageAttachmentLink {
    message_id,
    attachment_id
} kinds { raw_issues });

impl FromRow for MessageAttachmentLink {
    const TABLE: KnownTable = KnownTable::MessageAttachmentJoin;
    const PHASE: MessagesPhase = MessagesPhase::CheckingAttachments;
    const STAGE: Stage = Stage::Attachments;

    fn from_row(rowid: i64, r: &mut RowReader<'_, '_>) -> Result<Self, MessagesError> {
        Ok(MessageAttachmentLink {
            rowid,
            message_id: r.integer(message_attachment_join::MESSAGE_ID)?,
            attachment_id: r.integer(message_attachment_join::ATTACHMENT_ID)?,
            raw_issues: r.take_raw_issues(),
        })
    }
}
