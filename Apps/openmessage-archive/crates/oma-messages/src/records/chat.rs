//! `chat`: one chat row. Several rows may be one conversation (for example
//! one per service), and group-ness is not a field (D39).

use crate::codes::ServiceClass;
use crate::col::Col;
use crate::db::{FromRow, RowReader};
use crate::error::{MessagesError, Stage};
use crate::progress::MessagesPhase;
use crate::raw::RawIssue;
use crate::redact::{Redacted, SqlText};
use crate::schema::known::chat as c;
use crate::schema::KnownTable;
use crate::time::CocoaTime;
use crate::value::BlobSize;

use super::handle::service_class;
use super::shape_debug;

/// One `chat` row.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChatRecord {
    /// `ROWID`.
    pub rowid: i64,
    /// `guid`.
    pub guid: Col<Redacted<SqlText>>,
    /// `style`, raw. Its meaning is **UNVERIFIED** (D39).
    pub style: Col<i64>,
    /// `chat_identifier`.
    pub chat_identifier: Col<Redacted<SqlText>>,
    /// `service_name`, raw.
    pub service_name: Col<Redacted<SqlText>>,
    /// The class of `service_name`, when it is readable text.
    pub service_class: Option<ServiceClass>,
    /// `display_name`.
    pub display_name: Col<Redacted<SqlText>>,
    /// `room_name`.
    pub room_name: Col<Redacted<SqlText>>,
    /// `group_id`.
    pub group_id: Col<Redacted<SqlText>>,
    /// `properties`: presence and size only in M3 (reading its keys is
    /// deferred).
    pub properties: Col<BlobSize>,
    /// `is_archived`.
    pub is_archived: Col<bool>,
    /// `is_filtered`, raw; its values are **UNVERIFIED**.
    pub is_filtered: Col<i64>,
    /// `is_blackholed`.
    pub is_blackholed: Col<bool>,
    /// `is_pending_review`.
    pub is_pending_review: Col<bool>,
    /// `is_recovered`.
    pub is_recovered: Col<bool>,
    /// `last_read_message_timestamp`; its unit is **UNVERIFIED**, so it is
    /// classified per value.
    pub last_read_message_timestamp: Col<CocoaTime>,
    /// The raw values of unreadable cells, when kept (M4 R2).
    pub raw_issues: Vec<RawIssue>,
}

shape_debug!(ChatRecord {
    guid, style, chat_identifier, service_name, display_name, room_name, group_id,
    properties, is_archived, is_filtered, is_blackholed, is_pending_review, is_recovered,
    last_read_message_timestamp,
} kinds { service_class, raw_issues });

impl FromRow for ChatRecord {
    const TABLE: KnownTable = KnownTable::Chat;
    const PHASE: MessagesPhase = MessagesPhase::ReadingChats;
    const STAGE: Stage = Stage::Chats;

    fn from_row(rowid: i64, r: &mut RowReader<'_, '_>) -> Result<Self, MessagesError> {
        let service_name = r.text(c::SERVICE_NAME)?;
        Ok(ChatRecord {
            rowid,
            guid: r.text(c::GUID)?,
            style: r.integer(c::STYLE)?,
            chat_identifier: r.text(c::CHAT_IDENTIFIER)?,
            service_class: service_class(&service_name),
            service_name,
            display_name: r.text(c::DISPLAY_NAME)?,
            room_name: r.text(c::ROOM_NAME)?,
            group_id: r.text(c::GROUP_ID)?,
            properties: r.blob_size(c::PROPERTIES)?,
            is_archived: r.boolean(c::IS_ARCHIVED)?,
            is_filtered: r.integer(c::IS_FILTERED)?,
            is_blackholed: r.boolean(c::IS_BLACKHOLED)?,
            is_pending_review: r.boolean(c::IS_PENDING_REVIEW)?,
            is_recovered: r.boolean(c::IS_RECOVERED)?,
            last_read_message_timestamp: r.time(c::LAST_READ_MESSAGE_TIMESTAMP)?,
            raw_issues: r.take_raw_issues(),
        })
    }
}
