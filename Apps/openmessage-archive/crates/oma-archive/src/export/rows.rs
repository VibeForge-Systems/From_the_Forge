//! The uncapped rows and the ledger columns of an export (M6 spec §3.1
//! item 7, §3.4, §4.8).
//!
//! The viewer shows some columns only in `message_detail`, and caps the
//! lists it shows there (`REACTION_HISTORY_LIMIT`, `DETAIL_VERSIONS`,
//! `DETAIL_EDIT_ENTRIES`, `DETAIL_CHATS`). An export reads them all,
//! uncapped, here:
//!
//! - [`DetailColumns`]: the message's raw `date_edited` and
//!   `date_retracted`, the account (`destination_caller_id`, "meaning
//!   unverified") and the subject, uncapped, and its Recently Deleted
//!   listings with their raw `delete_*` time and `ck_sync_state`;
//! - [`ChatMembership`]: every conversation the message is a timeline item
//!   of, with the raw `join_date` (unit **UNVERIFIED**);
//! - [`ReactionRow`]: every add and remove row that targets a keyed
//!   message, with its raw direction and time, sorted by date key then id;
//! - [`ValueHistoryEntry`]: the M5 closed column set only
//!   ([`HistoryColumn`]), uncapped; a subject value recorded for a message
//!   with an unsent part is withheld unless the export reveals earlier
//!   content (spec §3.8);
//! - [`HistoryState`]: how many body versions and phone edit entries exist
//!   (the versions themselves stream through the history cursor);
//! - [`ExportAttachment`], [`ChatColumns`], [`HandleColumns`]: the
//!   attachment, conversation and handle columns the record needs, values
//!   uncapped.
//!
//! Read, delivery and sent state are never read (D94), nor are source
//! identifiers (GUIDs, `uncanonicalized_id`, `person_centric_id`: the D141
//! opt-in is slice 6d's) or session-local rowids. Nothing here is logged.

use rusqlite::types::ValueRef;
use rusqlite::OptionalExtension;

use crate::schema::codes::{AssocKindCode, Entity, ServiceClassCode};
use crate::stmt::StmtCache;
use crate::view::assemble::{bytes_of, emoji_content, Caps, WithheldReason};
use crate::view::detail::{
    attachment_facts, history, withheld_reason, RawValue, SQL_REACTION_ROWS, SQL_TARGET,
};
use crate::view::raw::{opt_coded, Coded, RawDirection, RawInt, RawTime};
use crate::view::{
    AssembledMessage, AttachmentFacts, AttachmentId, ChatId, HandleId, HistoryColumn, HistoryValue,
    MessageId, OptContent,
};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One Recently Deleted listing of a message, raw (§4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecentlyDeletedRow {
    /// The import session whose backup listed it.
    pub session: i64,
    /// The conversation the listing named, when resolved.
    pub chat: Option<ChatId>,
    /// `delete_*` as stored; the unit is **UNVERIFIED**.
    pub deleted: RawTime,
    /// `ck_sync_state`, raw.
    pub ck_sync_state: RawInt,
}

/// The columns M5 shows only in a message's detail (§4.8), uncapped.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DetailColumns {
    /// `date_edited_*` as stored (never localised).
    pub date_edited: RawTime,
    /// `date_retracted_*` as stored.
    pub date_retracted: RawTime,
    /// `destination_caller_id`: "Account (meaning unverified)", the
    /// user's own address.
    pub account: OptContent,
    pub subject: OptContent,
    /// Ordered by session, then the listing's order.
    pub recently_deleted: Vec<RecentlyDeletedRow>,
}

/// A conversation a message is a timeline item of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChatMembership {
    pub chat: ChatId,
    /// `chat_message.join_date_*` as stored; the unit is **UNVERIFIED**.
    pub join_date: RawTime,
}

/// One add or remove reaction row that targets a message (`assoc_kind`
/// 10–17 or 20–27, `assoc_present = 1`): the inputs of the D93 chip rule.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReactionRow {
    pub id: MessageId,
    /// Always a known code: the statement selects 10–17 and 20–27 only.
    pub kind: Coded<AssocKindCode>,
    /// `true` for 10–17, `false` for 20–27.
    pub adds: bool,
    /// `assoc_emoji`, uncapped; `None` when neither the value nor a state
    /// was stored.
    pub emoji: Option<OptContent>,
    pub direction: RawDirection,
    /// The resolved sender handle (`handle_ref`), when there is one.
    pub handle: Option<HandleId>,
    pub date: RawTime,
    pub target_part: Option<i64>,
}

/// One value of a history column other than the current one (§3.8).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ValueHistoryEntry {
    Value(HistoryValue),
    /// A value withheld with the content captured before an unsend (the
    /// subject history of a message with an unsent part), unless the
    /// export reveals earlier content. The value is never read into it.
    Withheld {
        column: HistoryColumn,
        reason: WithheldReason,
    },
}

/// How many body versions and phone edit entries a message has; the
/// versions stream one at a time through
/// [`super::ExportSnapshot::history`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HistoryState {
    /// The export does not include history (`ExportDepth::Items`).
    NotIncluded {
        captured_versions: u64,
        phone_entries: u64,
    },
    /// Included: stream it with `history()`.
    Available {
        captured_versions: u64,
        phone_entries: u64,
    },
}

/// The derivation versions behind a message's derived columns (§4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Derivation {
    /// `message.derived_by`.
    pub derived_by: i64,
    /// The current body's `decoder_version`; `None` without a body.
    pub decoder_version: Option<i64>,
}

/// Everything an export records about one attachment (§4.2, §4.8): the
/// facts and card the viewer shows, uncapped, plus the columns M5 leaves
/// out.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExportAttachment {
    pub facts: AttachmentFacts,
    /// Whether the export may copy its file (§3.8); a withheld one has
    /// no `facts.blob`.
    pub access: super::mask::AttachmentAccess,
    /// `is_outgoing`, raw.
    pub is_outgoing: RawInt,
    /// `transfer_state`, raw (meaning not claimed).
    pub transfer_state: RawInt,
    /// `created_*` as stored.
    pub created: RawTime,
    /// `start_*` as stored.
    pub start: RawTime,
}

/// The columns of one conversation an export records (§4.2), values
/// uncapped.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChatColumns {
    pub id: ChatId,
    pub display_name: OptContent,
    pub chat_identifier: OptContent,
    /// `service_name`, raw.
    pub service_name: OptContent,
    pub service_class: Option<Coded<ServiceClassCode>>,
    pub style: RawValue,
    pub is_filtered: RawValue,
    pub is_archived: RawValue,
    /// Every recorded participant (`chat_handle`), those who left
    /// included, by handle id.
    pub participants: Vec<HandleId>,
    /// Other recorded names (`ChatDisplayName`).
    pub name_history: Vec<HistoryValue>,
}

/// The columns of one handle an export records (§4.2), values uncapped.
/// `uncanonicalized_id` and `person_centric_id` are not read: they belong
/// to the D141 identifier opt-in.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HandleColumns {
    pub id: HandleId,
    pub address: OptContent,
    /// `service`, raw.
    pub service: OptContent,
    pub service_class: Option<Coded<ServiceClassCode>>,
    pub country: OptContent,
}

// ---------------------------------------------------------------------------
// Statements (each pinned in tests/query_plans.rs)
// ---------------------------------------------------------------------------

pub(crate) const SQL_DETAIL_COLUMNS: &str = "SELECT m.date_edited_raw, m.date_edited_st, \
       m.date_edited_unit, m.date_edited_utc_ns, m.date_retracted_raw, m.date_retracted_st, \
       m.date_retracted_unit, m.date_retracted_utc_ns, m.destination_caller_id, \
       m.destination_caller_id_st, m.subject, m.subject_st, m.derived_by, b.decoder_version \
     FROM message m LEFT JOIN message_body b ON b.id = m.current_body_id WHERE m.id = ?1";
pub(crate) const SQL_RD_ROWS: &str = "SELECT session_id, chat_ref, delete_raw, delete_st, \
       delete_unit, delete_utc_ns, ck_sync_state, ck_sync_state_st \
     FROM message_recently_deleted WHERE message_id = ?1 ORDER BY session_id, src_rowid";
pub(crate) const SQL_CHATS_OF: &str = "SELECT chat_id, join_date_raw, join_date_st, \
       join_date_unit, join_date_utc_ns FROM chat_message WHERE message_id = ?1 ORDER BY chat_id";
pub(crate) const SQL_VERSION_COUNT: &str =
    "SELECT count(*) FROM message_body WHERE message_id = ?1";
pub(crate) const SQL_PHONE_ENTRY_COUNT: &str = "SELECT count(*) FROM message_body b \
     JOIN message_edit_entry e ON e.body_id = b.id WHERE b.message_id = ?1";
pub(crate) const SQL_ATTACHMENT_EXTRA: &str = "SELECT is_outgoing, is_outgoing_st, \
       transfer_state, transfer_state_st, created_raw, created_st, created_unit, \
       created_utc_ns, start_raw, start_st, start_unit, start_utc_ns \
     FROM attachment WHERE id = ?1";
pub(crate) const SQL_CHAT_COLUMNS: &str = "SELECT display_name, display_name_st, \
       chat_identifier, chat_identifier_st, service_name, service_name_st, service_class, \
       style, style_st, is_filtered, is_filtered_st, is_archived, is_archived_st \
     FROM chat WHERE id = ?1";
pub(crate) const SQL_CHAT_PARTICIPANTS: &str =
    "SELECT handle_id FROM chat_handle WHERE chat_id = ?1 ORDER BY handle_id";
pub(crate) const SQL_HANDLE_COLUMNS: &str = "SELECT address, address_st, service, service_st, \
       service_class, country, country_st FROM handle WHERE id = ?1";

#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    [
        ("export detail columns", SQL_DETAIL_COLUMNS),
        ("export recently deleted rows", SQL_RD_ROWS),
        ("export chats of a message", SQL_CHATS_OF),
        ("export version count", SQL_VERSION_COUNT),
        ("export phone entry count", SQL_PHONE_ENTRY_COUNT),
        ("export attachment columns", SQL_ATTACHMENT_EXTRA),
        ("export chat columns", SQL_CHAT_COLUMNS),
        ("export chat participants", SQL_CHAT_PARTICIPANTS),
        ("export handle columns", SQL_HANDLE_COLUMNS),
    ]
    .into_iter()
    .map(|(n, s)| (n, s.to_owned()))
    .collect()
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

fn u64_of(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

fn time(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<RawTime> {
    Ok(RawTime::of(
        r.get(i)?,
        r.get(i + 1)?,
        r.get(i + 2)?,
        r.get(i + 3)?,
    ))
}

/// The detail columns and derivation of message `id`; `None` when it does
/// not exist.
pub(crate) fn detail_columns(
    s: &StmtCache<'_>,
    id: i64,
) -> rusqlite::Result<Option<(DetailColumns, Derivation)>> {
    let cap = Caps::EXPORT;
    let row = s
        .prepare(SQL_DETAIL_COLUMNS)?
        .query_row([id], |r| {
            Ok((
                time(r, 0)?,
                time(r, 4)?,
                OptContent::from_cell(r.get_ref(8)?, r.get(9)?, cap.identifier_utf16),
                OptContent::from_cell(r.get_ref(10)?, r.get(11)?, cap.subject_utf16),
                Derivation {
                    derived_by: r.get(12)?,
                    decoder_version: r.get(13)?,
                },
            ))
        })
        .optional()?;
    let Some((date_edited, date_retracted, account, subject, derivation)) = row else {
        return Ok(None);
    };
    let recently_deleted: Vec<RecentlyDeletedRow> = {
        let mut st = s.prepare(SQL_RD_ROWS)?;
        let rows = st.query_map([id], |r| {
            Ok(RecentlyDeletedRow {
                session: r.get(0)?,
                chat: r.get::<_, Option<i64>>(1)?.map(ChatId::new),
                deleted: time(r, 2)?,
                ck_sync_state: RawInt::of(r.get(6)?, r.get(7)?),
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    Ok(Some((
        DetailColumns {
            date_edited,
            date_retracted,
            account,
            subject,
            recently_deleted,
        },
        derivation,
    )))
}

/// Every conversation message `id` is a timeline item of, by chat id.
pub(crate) fn chats_of(s: &StmtCache<'_>, id: i64) -> rusqlite::Result<Vec<ChatMembership>> {
    let mut st = s.prepare(SQL_CHATS_OF)?;
    let rows = st.query_map([id], |r| {
        Ok(ChatMembership {
            chat: ChatId::new(r.get(0)?),
            join_date: time(r, 1)?,
        })
    })?;
    rows.collect()
}

/// The history counts of message `id`.
pub(crate) fn history_state(
    s: &StmtCache<'_>,
    id: i64,
    included: bool,
) -> rusqlite::Result<HistoryState> {
    let versions: i64 = s
        .prepare(SQL_VERSION_COUNT)?
        .query_row([id], |r| r.get(0))?;
    let entries: i64 = s
        .prepare(SQL_PHONE_ENTRY_COUNT)?
        .query_row([id], |r| r.get(0))?;
    let (captured_versions, phone_entries) = (u64_of(versions), u64_of(entries));
    Ok(if included {
        HistoryState::Available {
            captured_versions,
            phone_entries,
        }
    } else {
        HistoryState::NotIncluded {
            captured_versions,
            phone_entries,
        }
    })
}

/// Every add and remove row targeting message `id`, uncapped, sorted by
/// date key (undated last) then id. An unkeyed target has none.
pub(crate) fn reaction_rows(s: &StmtCache<'_>, id: i64) -> rusqlite::Result<Vec<ReactionRow>> {
    let target: Option<(i64, Option<Vec<u8>>)> = s
        .prepare(SQL_TARGET)?
        .query_row([id], |r| Ok((r.get(0)?, bytes_of(r.get_ref(1)?))))
        .optional()?;
    let Some(guid) = target.and_then(|(kind, guid)| guid.filter(|g| kind == 1 && !g.is_empty()))
    else {
        return Ok(Vec::new());
    };
    let mut rows: Vec<(Vec<u8>, ReactionRow)> = {
        let mut st = s.prepare(SQL_REACTION_ROWS)?;
        let rows = st.query_map(rusqlite::params![guid, i64::MAX], |r| {
            let code: i64 = r.get(1)?;
            let emoji = bytes_of(r.get_ref(2)?);
            let emoji_st: Option<i64> = r.get(3)?;
            Ok((
                bytes_of(r.get_ref(12)?).unwrap_or_default(),
                ReactionRow {
                    id: MessageId::new(r.get(0)?),
                    kind: AssocKindCode::from_code(code).ok_or(code),
                    adds: matches!(code, 10..=17),
                    emoji: (emoji.is_some() || emoji_st.is_some()).then(|| {
                        emoji_content(emoji.as_deref(), emoji_st, Caps::EXPORT.emoji_bytes)
                    }),
                    direction: RawDirection::of(r.get(4)?, r.get(5)?),
                    handle: r.get::<_, Option<i64>>(6)?.map(HandleId::new),
                    date: time(r, 7)?,
                    target_part: r.get(11)?,
                },
            ))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    rows.sort_by(|a, b| (&a.0, a.1.id).cmp(&(&b.0, b.1.id)));
    Ok(rows.into_iter().map(|(_, r)| r).collect())
}

/// The message's history values (the closed set), uncapped. The subject's
/// are withheld for a message with an unsent part unless
/// `reveal_earlier` (§3.8).
pub(crate) fn value_history(
    c: &StmtCache<'_>,
    m: &AssembledMessage,
    reveal_earlier: bool,
) -> rusqlite::Result<Vec<ValueHistoryEntry>> {
    let values = history(c, Entity::Message, m.id.get(), &Caps::EXPORT)?;
    let unsent = !m.edit.unsent_parts.is_empty() || m.edit.unsent_whole;
    let withhold =
        (unsent && !reveal_earlier).then(|| withheld_reason(m).unwrap_or(WithheldReason::Unsent));
    Ok(values
        .into_iter()
        .map(|v| match withhold {
            Some(reason) if v.column == HistoryColumn::Subject => ValueHistoryEntry::Withheld {
                column: v.column,
                reason,
            },
            _ => ValueHistoryEntry::Value(v),
        })
        .collect())
}

/// Attachment `id`'s facts (uncapped) and the columns M5 leaves out;
/// `None` when it does not exist.
pub(crate) fn attachment(
    s: &StmtCache<'_>,
    id: AttachmentId,
    access: super::mask::AttachmentAccess,
) -> rusqlite::Result<Option<ExportAttachment>> {
    let Some(facts) = attachment_facts(s, id.get(), &Caps::EXPORT)? else {
        return Ok(None);
    };
    let extra = s
        .prepare(SQL_ATTACHMENT_EXTRA)?
        .query_row([id.get()], |r| {
            Ok((
                RawInt::of(r.get(0)?, r.get(1)?),
                RawInt::of(r.get(2)?, r.get(3)?),
                time(r, 4)?,
                time(r, 8)?,
            ))
        })
        .optional()?;
    Ok(extra.map(
        |(is_outgoing, transfer_state, created, start)| ExportAttachment {
            facts,
            access,
            is_outgoing,
            transfer_state,
            created,
            start,
        },
    ))
}

fn content(v: ValueRef<'_>, st: Option<i64>) -> OptContent {
    OptContent::from_cell(v, st, Caps::EXPORT.identifier_utf16)
}

/// Conversation `id`'s columns; `None` when it does not exist.
pub(crate) fn chat_columns(s: &StmtCache<'_>, id: ChatId) -> rusqlite::Result<Option<ChatColumns>> {
    type Row = (
        OptContent,
        OptContent,
        OptContent,
        Option<i64>,
        RawValue,
        RawValue,
        RawValue,
    );
    let row: Option<Row> = s
        .prepare(SQL_CHAT_COLUMNS)?
        .query_row([id.get()], |r| {
            Ok((
                content(r.get_ref(0)?, r.get(1)?),
                content(r.get_ref(2)?, r.get(3)?),
                content(r.get_ref(4)?, r.get(5)?),
                r.get(6)?,
                RawValue::of(r.get(7)?, r.get(8)?),
                RawValue::of(r.get(9)?, r.get(10)?),
                RawValue::of(r.get(11)?, r.get(12)?),
            ))
        })
        .optional()?;
    let Some((display_name, chat_identifier, service_name, class, style, filtered, archived)) = row
    else {
        return Ok(None);
    };
    let participants: Vec<HandleId> = {
        let mut st = s.prepare(SQL_CHAT_PARTICIPANTS)?;
        let rows = st.query_map([id.get()], |r| Ok(HandleId::new(r.get(0)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut name_history = history(s, Entity::Chat, id.get(), &Caps::EXPORT)?;
    name_history.retain(|h| h.column == HistoryColumn::ChatDisplayName);
    Ok(Some(ChatColumns {
        id,
        display_name,
        chat_identifier,
        service_name,
        service_class: opt_coded(class, ServiceClassCode::from_code),
        style,
        is_filtered: filtered,
        is_archived: archived,
        participants,
        name_history,
    }))
}

/// Handle `id`'s columns; `None` when it does not exist.
pub(crate) fn handle_columns(
    s: &StmtCache<'_>,
    id: HandleId,
) -> rusqlite::Result<Option<HandleColumns>> {
    s.prepare(SQL_HANDLE_COLUMNS)?
        .query_row([id.get()], |r| {
            Ok(HandleColumns {
                id,
                address: content(r.get_ref(0)?, r.get(1)?),
                service: content(r.get_ref(2)?, r.get(3)?),
                service_class: opt_coded(r.get(4)?, ServiceClassCode::from_code),
                country: content(r.get_ref(5)?, r.get(6)?),
            })
        })
        .optional()
}
