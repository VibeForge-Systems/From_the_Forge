//! The references and identifiers an export record needs beyond the
//! assembled message (M6 spec §4.2, §4.9, slice 6d).
//!
//! - [`ExportSnapshot::chats_of`](super::ExportSnapshot::chats_of): the
//!   conversations a message is a timeline item of, so a record can say
//!   whether a reply's originator or a reaction's target is in the same
//!   conversation file (`inConversation`), without assembling it.
//! - [`ExportSnapshot::reply_ids`](super::ExportSnapshot::reply_ids): the
//!   replies to a keyed message, by id, under the same rule as the
//!   viewer's reply count (plain messages that are not associations), so a
//!   record can split the count into replies in the export and replies
//!   elsewhere without naming the latter.
//! - [`ExportSnapshot::source_identifiers`](super::ExportSnapshot::source_identifiers):
//!   the source GUIDs (a message's own and the three it references, an
//!   attachment's `original_guid`), a chat's
//!   `room_name` and `group_id`, and a handle's `uncanonicalized_id` and
//!   `person_centric_id`, read **only** for the D141 identifier opt-in
//!   (owner Q5's lean: off by default). `oma-export` calls it only when the
//!   plan opts in.
//! - [`MessageRawColumns`]: the message columns no viewer read carries,
//!   raw, so the export's column ledger can give each a disposition
//!   (slice 6d): read for every exported message by
//!   [`ExportSnapshot::assemble`](super::ExportSnapshot::assemble).
//!
//! Nothing here logs; the values are content (`OptContent`).

use rusqlite::OptionalExtension;

use crate::stmt::StmtCache;
use crate::view::assemble::Caps;
use crate::view::raw::RawInt;
use crate::view::{AttachmentId, ChatId, HandleId, MessageId, OptContent};

/// The row whose source identifiers to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SourceOf {
    Message(MessageId),
    Chat(ChatId),
    Handle(HandleId),
    Attachment(AttachmentId),
}

/// A row's source identifiers, uncapped (D141). Each field is `None` when
/// the entity has no such column; a column the source did not record is
/// `Some(Missing(state))`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SourceIdentifiers {
    /// `guid` of a message, chat or attachment.
    pub guid: Option<OptContent>,
    /// A message's `associated_message_guid` (raw), `thread_originator_guid`
    /// and `reply_to_guid`.
    pub assoc_guid: Option<OptContent>,
    pub thread_originator_guid: Option<OptContent>,
    pub reply_to_guid: Option<OptContent>,
    /// An attachment's `original_guid`.
    pub original_guid: Option<OptContent>,
    /// A chat's `room_name` and `group_id`.
    pub room_name: Option<OptContent>,
    pub group_id: Option<OptContent>,
    /// A handle's `uncanonicalized_id`.
    pub uncanonicalized_id: Option<OptContent>,
    /// A handle's `person_centric_id`.
    pub person_centric_id: Option<OptContent>,
}

pub(crate) const SQL_REPLY_IDS: &str = "SELECT r.id FROM message m \
       JOIN message r ON r.thread_originator_guid = m.guid \
     WHERE m.id = ?1 AND m.key_kind = 1 AND r.assoc_present = 0 AND r.item_kind = 1 \
     ORDER BY r.id";
/// The message columns no viewer read carries (M6 spec §4.8), raw. Their
/// meaning is not claimed; codes are kept as recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MessageRawColumns {
    /// `service`, the raw string (the record's service class is derived
    /// from it).
    pub service: OptContent,
    pub is_audio_message: RawInt,
    /// A cache flag in the source, not a fact about the message.
    pub cache_has_attachments: RawInt,
    pub share_status: RawInt,
    pub share_direction: RawInt,
    /// `associated_message_type` (the record's association kind is
    /// derived from it).
    pub assoc_type: RawInt,
    pub assoc_range_location: RawInt,
    pub assoc_range_length: RawInt,
    /// `thread_originator_part`, raw (format **UNVERIFIED**).
    pub thread_originator_part: OptContent,
    pub part_count: RawInt,
    pub schedule_state: RawInt,
    pub filter_action: RawInt,
    pub filter_sub_action: RawInt,
    pub was_downgraded: RawInt,
}

pub(crate) const SQL_MESSAGE_RAW: &str = "SELECT service, service_st, is_audio_message, \
       is_audio_message_st, cache_has_attachments, cache_has_attachments_st, share_status, \
       share_status_st, share_direction, share_direction_st, assoc_type, assoc_type_st, \
       assoc_range_location, assoc_range_location_st, assoc_range_length, \
       assoc_range_length_st, thread_originator_part, thread_originator_part_st, part_count, \
       part_count_st, schedule_state, schedule_state_st, filter_action, filter_action_st, \
       filter_sub_action, filter_sub_action_st, was_downgraded, was_downgraded_st \
     FROM message WHERE id = ?1";
pub(crate) const SQL_MESSAGE_SOURCE: &str = "SELECT guid, guid_st, assoc_guid_raw, \
       assoc_guid_raw_st, thread_originator_guid, thread_originator_guid_st, reply_to_guid, \
       reply_to_guid_st FROM message WHERE id = ?1";
pub(crate) const SQL_CHAT_SOURCE: &str = "SELECT guid, guid_st, room_name, room_name_st, \
       group_id, group_id_st FROM chat WHERE id = ?1";
pub(crate) const SQL_ATTACHMENT_SOURCE: &str = "SELECT guid, guid_st, original_guid, \
       original_guid_st FROM attachment WHERE id = ?1";
pub(crate) const SQL_HANDLE_SOURCE: &str = "SELECT uncanonicalized_id, uncanonicalized_id_st, \
       person_centric_id, person_centric_id_st FROM handle WHERE id = ?1";

#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    [
        ("export reply ids", SQL_REPLY_IDS),
        ("export message raw columns", SQL_MESSAGE_RAW),
        ("export message source ids", SQL_MESSAGE_SOURCE),
        ("export chat source ids", SQL_CHAT_SOURCE),
        ("export attachment source ids", SQL_ATTACHMENT_SOURCE),
        ("export handle source ids", SQL_HANDLE_SOURCE),
    ]
    .into_iter()
    .map(|(n, s)| (n, s.to_owned()))
    .collect()
}

/// The replies to message `id` (none for an unkeyed message), ascending.
pub(crate) fn reply_ids(s: &StmtCache<'_>, id: i64) -> rusqlite::Result<Vec<MessageId>> {
    let mut st = s.prepare(SQL_REPLY_IDS)?;
    let rows = st.query_map([id], |r| Ok(MessageId::new(r.get(0)?)))?;
    rows.collect()
}

/// Column pair `i`, `i + 1` (value, state) as content, uncapped (v1's
/// `CHECK` keeps exactly one of the value and its state).
fn cell(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<OptContent> {
    Ok(OptContent::from_cell(
        r.get_ref(i)?,
        r.get(i + 1)?,
        Caps::EXPORT.identifier_utf16,
    ))
}

fn int(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<RawInt> {
    Ok(RawInt::of(r.get(i)?, r.get(i + 1)?))
}

/// Message `id`'s raw columns; `None` when it does not exist.
pub(crate) fn raw_columns(
    s: &StmtCache<'_>,
    id: i64,
) -> rusqlite::Result<Option<MessageRawColumns>> {
    s.prepare(SQL_MESSAGE_RAW)?
        .query_row([id], |r| {
            Ok(MessageRawColumns {
                service: cell(r, 0)?,
                is_audio_message: int(r, 2)?,
                cache_has_attachments: int(r, 4)?,
                share_status: int(r, 6)?,
                share_direction: int(r, 8)?,
                assoc_type: int(r, 10)?,
                assoc_range_location: int(r, 12)?,
                assoc_range_length: int(r, 14)?,
                thread_originator_part: cell(r, 16)?,
                part_count: int(r, 18)?,
                schedule_state: int(r, 20)?,
                filter_action: int(r, 22)?,
                filter_sub_action: int(r, 24)?,
                was_downgraded: int(r, 26)?,
            })
        })
        .optional()
}

/// The source identifiers of `of`; `None` when the row does not exist.
pub(crate) fn source_identifiers(
    s: &StmtCache<'_>,
    of: SourceOf,
) -> rusqlite::Result<Option<SourceIdentifiers>> {
    let none = SourceIdentifiers {
        guid: None,
        assoc_guid: None,
        thread_originator_guid: None,
        reply_to_guid: None,
        original_guid: None,
        room_name: None,
        group_id: None,
        uncanonicalized_id: None,
        person_centric_id: None,
    };
    let (sql, id): (&'static str, i64) = match of {
        SourceOf::Message(id) => (SQL_MESSAGE_SOURCE, id.get()),
        SourceOf::Chat(id) => (SQL_CHAT_SOURCE, id.get()),
        SourceOf::Attachment(id) => (SQL_ATTACHMENT_SOURCE, id.get()),
        SourceOf::Handle(id) => (SQL_HANDLE_SOURCE, id.get()),
    };
    s.prepare(sql)?
        .query_row([id], |r| {
            Ok(match of {
                SourceOf::Message(_) => SourceIdentifiers {
                    guid: Some(cell(r, 0)?),
                    assoc_guid: Some(cell(r, 2)?),
                    thread_originator_guid: Some(cell(r, 4)?),
                    reply_to_guid: Some(cell(r, 6)?),
                    ..none
                },
                SourceOf::Chat(_) => SourceIdentifiers {
                    guid: Some(cell(r, 0)?),
                    room_name: Some(cell(r, 2)?),
                    group_id: Some(cell(r, 4)?),
                    ..none
                },
                SourceOf::Attachment(_) => SourceIdentifiers {
                    guid: Some(cell(r, 0)?),
                    original_guid: Some(cell(r, 2)?),
                    ..none
                },
                SourceOf::Handle(_) => SourceIdentifiers {
                    uncanonicalized_id: Some(cell(r, 0)?),
                    person_centric_id: Some(cell(r, 2)?),
                    ..none
                },
            })
        })
        .optional()
}
