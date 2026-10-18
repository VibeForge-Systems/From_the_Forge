//! The conversation list (M5 spec §3.5): an ordered id snapshot and the
//! rows for the visible window.
//!
//! **Why an id snapshot and not an SQL keyset.** v1 has no index on a
//! chat's last activity, and `chat_stats` is rewritten only at finalize, so
//! it is stale while an import runs. The order is therefore computed per
//! call from the timeline index, which stays current as batches commit:
//! each chat's newest **dated** key (`sort_key < X'01'`), newest first;
//! chats with no dated row after every other, by id. The UI virtualises
//! over the snapshot and fetches rows per visible window.
//!
//! Counts and first and last dates come from `chat_stats`, "as of the last
//! completed import" ([`Conversation::stats_stale`] says when an import
//! ran after it). The live facts (the last dated instant, the first date
//! of 2007 or later, the undated count, the preview) come from the
//! timeline index.
//!
//! The label follows §8.4's precedence and invents nothing: the chat's
//! name, "name recorded but unreadable" (no fall-through), the recorded
//! participants, the chat identifier, or nothing.

use std::time::Instant;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension};

use super::text::{mask_utf16, ContentText, MaskKind, OptContent, TextPiece};
use super::{
    folds_into, stored_as_blob, CappedCount, ChatId, DirectionCode, HandleId, MessageId,
    COL_BODY_TEXT_RAW, COL_CHAT_DISPLAY_NAME, COL_CHAT_IDENTIFIER, COL_HANDLE_ADDRESS,
    IDENTIFIER_UTF16, PREVIEW_UTF16,
};
use crate::conn::{Phase, ViewBudget};
use crate::error::ArchiveError;
use crate::reader::ArchiveReader;
use crate::schema::codes::{
    AssocKindCode, DisplayNoneReason, Entity, ItemKindCode, ServiceClassCode, SessionState,
    ValueState,
};
use crate::sortkey;

/// The most ids [`ArchiveReader::conversation_index`] returns.
pub const CONVERSATION_INDEX_LIMIT: usize = 100_000;
/// The most ids one [`ArchiveReader::conversations`] call takes.
pub const CONVERSATIONS_PER_CALL: usize = 200;
/// The newest dated rows the preview looks at for an item that is not a
/// folded reaction.
pub const PREVIEW_SCAN_ROWS: u32 = 50;
/// The participants a label names before "+N".
pub const LABEL_PARTICIPANTS: usize = 3;
/// The longest name filter, in UTF-8 bytes.
pub const NAME_FILTER_MAX_BYTES: usize = 128;
/// The cap of the live undated and pre-2007 counts.
const COUNT_CAP: u64 = 10_000;
/// 2007-01-01T00:00:00Z in Unix nanoseconds: the display floor of
/// `supported-formats.md` (dates before it are marked and never used as a
/// chat's first date).
pub(crate) const FLOOR_2007_NS: i64 = 1_167_609_600_000_000_000;
/// The bit of `one_time_code` in `message_body_part.flags`
/// (`ATTR_FLAG_BITS[4]`, checked by a test).
pub(crate) const ONE_TIME_CODE_BIT: usize = 4;

/// The order of [`ArchiveReader::conversation_index`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConversationSort {
    /// By each chat's newest dated row (reactions and events included),
    /// newest first; chats with no dated row last, by id.
    Recent,
    /// By the label's text, compared case-insensitively; chats without a
    /// usable label last, by id.
    Name,
    /// By `chat_stats.message_count`, largest first; chats without
    /// statistics last, by id.
    Count,
}

/// Why a name filter was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NameFilterError {
    /// Nothing but whitespace.
    Empty,
    /// More than [`NAME_FILTER_MAX_BYTES`] bytes.
    TooLong,
    /// A control character.
    Control,
}

/// A user's filter over chat names, identifiers and participant addresses
/// (matched in Rust, case-insensitively). `Debug` prints no text.
#[derive(Clone, PartialEq, Eq)]
pub struct NameFilter {
    folded: String,
}

impl std::fmt::Debug for NameFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NameFilter(<{} bytes>)", self.folded.len())
    }
}

impl NameFilter {
    /// A filter from the user's text, trimmed of surrounding whitespace.
    pub fn new(text: &str) -> Result<NameFilter, NameFilterError> {
        if text.len() > NAME_FILTER_MAX_BYTES {
            return Err(NameFilterError::TooLong);
        }
        if text.chars().any(char::is_control) {
            return Err(NameFilterError::Control);
        }
        let t = text.trim();
        if t.is_empty() {
            return Err(NameFilterError::Empty);
        }
        Ok(NameFilter {
            folded: t.to_lowercase(),
        })
    }

    fn matches(&self, value: &str) -> bool {
        value.to_lowercase().contains(&self.folded)
    }
}

/// The ordered chat ids of an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConversationIndex {
    /// At most [`CONVERSATION_INDEX_LIMIT`] ids, in order.
    pub ids: Vec<ChatId>,
    /// How many chats matched (before the limit).
    pub total: u64,
    /// More chats matched than the limit returned.
    pub truncated: bool,
}

/// `chat_stats`, as of the last completed import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChatStatsFacts {
    /// Every `chat_message` row, reactions and events included ("N items
    /// recorded").
    pub items_recorded: u64,
    pub undated: u64,
    pub first_utc_ns: Option<i64>,
    pub last_utc_ns: Option<i64>,
}

/// One recorded participant, as recorded (no formatting, no contacts).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Participant {
    pub handle: HandleId,
    /// "Address not recorded" when missing; never an empty string.
    pub address: OptContent,
    pub service: Option<ServiceClassCode>,
}

/// A chat's label (§8.4 precedence). Nothing is invented.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChatLabel {
    /// `display_name`, a non-empty value.
    Name(ContentText),
    /// `display_name` is recorded but too large or unreadable. It does not
    /// fall through to the participants.
    NameUnreadable(ValueState),
    /// The recorded participants ("a, b, c +N", "from participants").
    Participants { first: Vec<Participant>, more: u64 },
    /// `chat_identifier`.
    Identifier(ContentText),
    /// "Conversation without a name or participants (#id)".
    Unnamed,
}

/// What a conversation's preview shows.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PreviewBody {
    /// The current body's text, capped at [`PREVIEW_UTF16`], with every
    /// one-time code withheld (D115; a list preview never reveals). One
    /// [`TextPiece::Text`] when nothing is masked.
    Text(Vec<TextPiece>),
    /// The current body has no text; the reason, when it is a known code.
    NoText { reason: Option<DisplayNoneReason> },
    /// The message has no current body row.
    NoBody,
    /// A group event or another non-message item; the UI words it from
    /// [`Preview::item`].
    Event,
    /// A reaction that stays visible (its target is not a timeline item of
    /// this chat); the UI words it from the kind.
    Reaction(Option<AssocKindCode>),
}

/// The newest dated item that is not a folded reaction, within
/// [`PREVIEW_SCAN_ROWS`] rows.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Preview {
    pub message: MessageId,
    pub utc_ns: i64,
    /// `None` for an item code this build does not know.
    pub item: Option<ItemKindCode>,
    pub direction: DirectionCode,
    pub body: PreviewBody,
}

/// One conversation row.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Conversation {
    pub id: ChatId,
    pub label: ChatLabel,
    /// `chat.service_class`; `None` when NULL or unknown.
    pub service: Option<ServiceClassCode>,
    /// `chat_handle` rows ("N addresses recorded in this conversation",
    /// people who left included).
    pub addresses: u64,
    /// More than one recorded address. `chat.style` is never read.
    pub group: bool,
    /// `None`: "statistics update when the import finishes".
    pub stats: Option<ChatStatsFacts>,
    /// An import ran after the statistics were computed (the newest
    /// session is not a completed one), so they may be out of date.
    pub stats_stale: bool,
    /// The newest dated row's instant (live).
    pub last_dated_utc_ns: Option<i64>,
    /// The oldest dated row on or after 2007-01-01 (live).
    pub first_dated_2007_utc_ns: Option<i64>,
    /// Dated rows before 2007-01-01 ("and N earlier-dated items").
    pub earlier_dated: CappedCount,
    /// Undated rows ("Undated items (N)").
    pub undated: CappedCount,
    /// No row at all.
    pub empty: bool,
    pub preview: Option<Preview>,
}

// ---------------------------------------------------------------------------
// Statements (each pinned in tests/query_plans.rs)
// ---------------------------------------------------------------------------

const SQL_ORDER_RECENT: &str = "SELECT c.id, \
       (SELECT sort_key FROM chat_message \
        WHERE chat_id = c.id AND sort_key < X'01' ORDER BY sort_key DESC LIMIT 1) k \
     FROM chat c ORDER BY k DESC NULLS LAST, c.id";
const SQL_ORDER_COUNT: &str = "SELECT c.id FROM chat c \
     LEFT JOIN chat_stats s ON s.chat_id = c.id \
     ORDER BY s.message_count DESC NULLS LAST, c.id";
const SQL_ALL_NAMES: &str = "SELECT id, display_name, display_name_st, \
       chat_identifier, chat_identifier_st FROM chat ORDER BY id";
const SQL_ALL_PARTICIPANTS: &str = "SELECT ch.chat_id, h.address, h.address_st \
     FROM chat_handle ch JOIN handle h ON h.id = ch.handle_id \
     ORDER BY ch.chat_id, ch.handle_id";
const SQL_NEWEST_SESSION: &str = "SELECT state FROM import_session ORDER BY id DESC LIMIT 1";
const SQL_CHAT_ROW: &str = "SELECT c.display_name, c.display_name_st, \
       c.chat_identifier, c.chat_identifier_st, c.service_class, \
       s.chat_id, s.message_count, s.undated_count, s.first_sort_ns, s.last_sort_ns \
     FROM chat c LEFT JOIN chat_stats s ON s.chat_id = c.id WHERE c.id = ?1";
const SQL_PARTICIPANTS: &str = "SELECT h.id, h.address, h.address_st, h.service_class \
     FROM chat_handle ch JOIN handle h ON h.id = ch.handle_id \
     WHERE ch.chat_id = ?1 ORDER BY ch.handle_id LIMIT ?2";
const SQL_PARTICIPANT_COUNT: &str = "SELECT count(*) FROM chat_handle WHERE chat_id = ?1";
const SQL_NEWEST_DATED: &str = "SELECT message_id, sort_key FROM chat_message \
     WHERE chat_id = ?1 AND sort_key < X'01' \
     ORDER BY sort_key DESC, message_id DESC LIMIT ?2";
const SQL_FIRST_DATED_FROM: &str = "SELECT sort_key FROM chat_message \
     WHERE chat_id = ?1 AND sort_key >= ?2 AND sort_key < X'01' \
     ORDER BY sort_key, message_id LIMIT 1";
const SQL_COUNT_BEFORE: &str = "SELECT count(*) FROM (SELECT 1 FROM chat_message \
     WHERE chat_id = ?1 AND sort_key < ?2 LIMIT ?3)";
pub(crate) const SQL_COUNT_UNDATED: &str = "SELECT count(*) FROM (SELECT 1 FROM chat_message \
     WHERE chat_id = ?1 AND sort_key >= X'01' LIMIT ?2)";
const SQL_PREVIEW_ROW: &str = "SELECT m.item_kind, m.is_from_me, m.is_from_me_st, \
       m.assoc_present, m.assoc_kind, b.id, b.display_text, b.display_source, \
       b.display_none_reason, b.run_issue \
     FROM message m LEFT JOIN message_body b ON b.id = m.current_body_id WHERE m.id = ?1";
const SQL_FLAGGED_PARTS: &str = "SELECT utf16_start, utf16_end FROM message_body_part \
     WHERE body_id = ?1 AND (flags & ?2) <> 0";

pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    [
        ("viewer conversation order (recent)", SQL_ORDER_RECENT),
        ("viewer conversation order (count)", SQL_ORDER_COUNT),
        ("viewer chat names", SQL_ALL_NAMES),
        ("viewer chat participants (all)", SQL_ALL_PARTICIPANTS),
        ("viewer newest session", SQL_NEWEST_SESSION),
        ("viewer conversation row", SQL_CHAT_ROW),
        ("viewer participants", SQL_PARTICIPANTS),
        ("viewer participant count", SQL_PARTICIPANT_COUNT),
        ("viewer newest dated rows", SQL_NEWEST_DATED),
        ("viewer first dated from", SQL_FIRST_DATED_FROM),
        ("viewer count dated before", SQL_COUNT_BEFORE),
        ("viewer count undated", SQL_COUNT_UNDATED),
        ("viewer preview row", SQL_PREVIEW_ROW),
        ("viewer flagged parts", SQL_FLAGGED_PARTS),
    ]
    .into_iter()
    .map(|(n, s)| (n, s.to_owned()))
    .collect()
}

// ---------------------------------------------------------------------------
// The index
// ---------------------------------------------------------------------------

fn cell(b: &Option<Vec<u8>>) -> ValueRef<'_> {
    match b {
        Some(b) => ValueRef::Blob(b.as_slice()),
        None => ValueRef::Null,
    }
}

fn bytes_of(v: ValueRef<'_>) -> Option<Vec<u8>> {
    match v {
        ValueRef::Text(b) | ValueRef::Blob(b) => Some(b.to_vec()),
        _ => None,
    }
}

/// What the name sort and the filter compare: the whole values, decoded
/// lossily (never cut), held only for the call.
struct Names {
    display: Option<Vec<u8>>,
    display_st: Option<i64>,
    identifier: Option<Vec<u8>>,
    addresses: Vec<Vec<u8>>,
}

impl Names {
    fn text(b: &[u8]) -> String {
        String::from_utf8_lossy(b).into_owned()
    }

    /// The label's text, lower-cased, for the name sort; `None` sorts last.
    fn sort_key(&self) -> Option<String> {
        match (&self.display, self.display_st) {
            (Some(d), None) if !d.is_empty() => return Some(Self::text(d).to_lowercase()),
            (_, Some(st))
                if st == ValueState::Oversized.code() || st == ValueState::Unreadable.code() =>
            {
                return None
            }
            _ => {}
        }
        if !self.addresses.is_empty() {
            let shown: Vec<String> = self
                .addresses
                .iter()
                .take(LABEL_PARTICIPANTS)
                .map(|a| Self::text(a))
                .collect();
            return Some(shown.join(", ").to_lowercase());
        }
        self.identifier
            .as_deref()
            .filter(|i| !i.is_empty())
            .map(|i| Self::text(i).to_lowercase())
    }

    fn matches(&self, f: &NameFilter) -> bool {
        let one = |b: &Option<Vec<u8>>| b.as_deref().is_some_and(|b| f.matches(&Self::text(b)));
        one(&self.display)
            || one(&self.identifier)
            || self.addresses.iter().any(|a| f.matches(&Self::text(a)))
    }
}

fn load_names(c: &Connection) -> rusqlite::Result<Vec<(i64, Names)>> {
    let mut out: Vec<(i64, Names)> = Vec::new();
    {
        let mut st = c.prepare(SQL_ALL_NAMES)?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let display_st: Option<i64> = r.get(2)?;
            let identifier_st: Option<i64> = r.get(4)?;
            out.push((
                r.get(0)?,
                Names {
                    display: bytes_of(r.get_ref(1)?).filter(|_| display_st.is_none()),
                    display_st,
                    identifier: bytes_of(r.get_ref(3)?).filter(|_| identifier_st.is_none()),
                    addresses: Vec::new(),
                },
            ));
        }
    }
    let mut st = c.prepare(SQL_ALL_PARTICIPANTS)?;
    let mut rows = st.query([])?;
    let mut at = 0usize;
    while let Some(r) = rows.next()? {
        let chat: i64 = r.get(0)?;
        let st: Option<i64> = r.get(2)?;
        let Some(addr) = bytes_of(r.get_ref(1)?).filter(|_| st.is_none()) else {
            continue;
        };
        // Both lists are in chat id order: advance to this chat.
        while out.get(at).is_some_and(|(id, _)| *id < chat) {
            at += 1;
        }
        if let Some((id, names)) = out.get_mut(at) {
            if *id == chat {
                names.addresses.push(addr);
            }
        }
    }
    Ok(out)
}

fn ordered_ids(c: &Connection, sql: &str) -> rusqlite::Result<Vec<i64>> {
    let mut st = c.prepare(sql)?;
    let rows = st.query_map([], |r| r.get::<_, i64>(0))?;
    rows.collect()
}

fn index_in(
    p: &Phase<'_>,
    sort: ConversationSort,
    filter: Option<&NameFilter>,
) -> Result<ConversationIndex, ArchiveError> {
    let names = if filter.is_some() || sort == ConversationSort::Name {
        Some(p.sql(load_names)?)
    } else {
        None
    };
    let ids: Vec<i64> = match sort {
        ConversationSort::Recent => p.sql(|c| ordered_ids(c, SQL_ORDER_RECENT))?,
        ConversationSort::Count => p.sql(|c| ordered_ids(c, SQL_ORDER_COUNT))?,
        ConversationSort::Name => {
            let mut keyed: Vec<(Option<String>, i64)> = names
                .as_ref()
                .map(|n| n.iter().map(|(id, n)| (n.sort_key(), *id)).collect())
                .unwrap_or_default();
            keyed.sort_by(|a, b| match (&a.0, &b.0) {
                (Some(x), Some(y)) => x.cmp(y).then(a.1.cmp(&b.1)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.1.cmp(&b.1),
            });
            keyed.into_iter().map(|(_, id)| id).collect()
        }
    };
    let kept: Vec<i64> = match (filter, &names) {
        (Some(f), Some(names)) => {
            let matching: std::collections::HashSet<i64> = names
                .iter()
                .filter(|(_, n)| n.matches(f))
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter().filter(|id| matching.contains(id)).collect()
        }
        _ => ids,
    };
    let total = u64::try_from(kept.len()).unwrap_or(u64::MAX);
    let truncated = kept.len() > CONVERSATION_INDEX_LIMIT;
    Ok(ConversationIndex {
        ids: kept
            .into_iter()
            .take(CONVERSATION_INDEX_LIMIT)
            .map(ChatId::new)
            .collect(),
        total,
        truncated,
    })
}

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

/// A stored text pair as an [`OptContent`] capped for an identifier, with
/// its `stored_as_blob` fact.
fn identifier(
    c: &Connection,
    v: ValueRef<'_>,
    st: Option<i64>,
    entity: Entity,
    id: i64,
    col: i64,
) -> rusqlite::Result<OptContent> {
    Ok(match OptContent::from_cell(v, st, IDENTIFIER_UTF16) {
        OptContent::Text(t) => {
            OptContent::Text(t.stored_as_blob(stored_as_blob(c, entity, id, col)?))
        }
        missing => missing,
    })
}

fn participants(c: &Connection, chat: i64) -> rusqlite::Result<(Vec<Participant>, u64)> {
    let total: i64 = c
        .prepare(SQL_PARTICIPANT_COUNT)?
        .query_row([chat], |r| r.get(0))?;
    let limit = i64::try_from(LABEL_PARTICIPANTS).unwrap_or(3);
    let mut st = c.prepare(SQL_PARTICIPANTS)?;
    let mut rows = st.query(rusqlite::params![chat, limit])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let address = identifier(
            c,
            r.get_ref(1)?,
            r.get(2)?,
            Entity::Handle,
            id,
            COL_HANDLE_ADDRESS,
        )?;
        let service: Option<i64> = r.get(3)?;
        out.push(Participant {
            handle: HandleId::new(id),
            address,
            service: service.and_then(ServiceClassCode::from_code),
        });
    }
    Ok((out, u64::try_from(total).unwrap_or(0)))
}

fn label(display: OptContent, first: Vec<Participant>, total: u64, ident: OptContent) -> ChatLabel {
    match display {
        OptContent::Text(t) if t.full_utf16 > 0 => return ChatLabel::Name(t),
        OptContent::Missing(st @ (ValueState::Oversized | ValueState::Unreadable)) => {
            return ChatLabel::NameUnreadable(st)
        }
        _ => {}
    }
    if !first.is_empty() {
        let shown = u64::try_from(first.len()).unwrap_or(0);
        return ChatLabel::Participants {
            first,
            more: total.saturating_sub(shown),
        };
    }
    match ident {
        OptContent::Text(t) if t.full_utf16 > 0 => ChatLabel::Identifier(t),
        _ => ChatLabel::Unnamed,
    }
}

/// The current body's text with its one-time codes withheld. Ranges apply
/// only when the formatting was decoded and read cleanly
/// (`display_source = 1`, no `run_issue`, §3.6); otherwise a body with a
/// one-time-code part is withheld whole.
fn masked_preview(
    c: &Connection,
    body: i64,
    text: &str,
    source: Option<i64>,
    run_issue: Option<i64>,
    cap: u32,
) -> rusqlite::Result<Vec<TextPiece>> {
    let mut st = c.prepare(SQL_FLAGGED_PARTS)?;
    let ranges: Vec<(i64, i64)> = st
        .query_map(rusqlite::params![body, 1i64 << ONE_TIME_CODE_BIT], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let lossy = source == Some(3);
    let blob = matches!(source, Some(2 | 3))
        && stored_as_blob(c, Entity::MessageBody, body, COL_BODY_TEXT_RAW)?;
    if ranges.is_empty() {
        let t = ContentText::from_str(text, cap)
            .lossy(lossy)
            .stored_as_blob(blob);
        return Ok(vec![TextPiece::Text(t)]);
    }
    if source != Some(1) || run_issue.is_some() {
        return Ok(vec![TextPiece::Masked(MaskKind::OneTimeCode)]);
    }
    let clamp = |v: i64| u32::try_from(v.max(0)).unwrap_or(u32::MAX);
    let ranges: Vec<(u32, u32)> = ranges
        .into_iter()
        .map(|(a, b)| (clamp(a), clamp(b)))
        .collect();
    Ok(mask_utf16(text, &ranges, MaskKind::OneTimeCode, cap)
        .into_iter()
        .map(|p| match p {
            TextPiece::Text(t) => TextPiece::Text(t.lossy(lossy).stored_as_blob(blob)),
            masked => masked,
        })
        .collect())
}

type PreviewRow = (
    i64,
    Option<i64>,
    Option<i64>,
    i64,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

/// The preview of `message` (also a reply's quote): its kind, direction
/// and current body, with every one-time code withheld, the text capped at
/// `cap` UTF-16 units.
pub(crate) fn preview_of(
    c: &Connection,
    message: i64,
    utc_ns: i64,
    cap: u32,
) -> rusqlite::Result<Option<Preview>> {
    let row: Option<PreviewRow> = c
        .prepare(SQL_PREVIEW_ROW)?
        .query_row([message], |r| {
            let text = match r.get_ref(6)? {
                ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
                _ => None,
            };
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                text,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
            ))
        })
        .optional()?;
    let Some((item, from_me, from_me_st, present, kind, body, text, source, none, run_issue)) = row
    else {
        return Ok(None);
    };
    let item = ItemKindCode::from_code(item);
    let body = if item != Some(ItemKindCode::Message) {
        PreviewBody::Event
    } else if present == 1 && super::is_reaction(kind) {
        PreviewBody::Reaction(super::assoc_kind(kind))
    } else {
        match body {
            None => PreviewBody::NoBody,
            Some(b) if source.is_some() => PreviewBody::Text(masked_preview(
                c,
                b,
                text.as_deref().unwrap_or(""),
                source,
                run_issue,
                cap,
            )?),
            Some(_) => PreviewBody::NoText {
                reason: none.and_then(DisplayNoneReason::from_code),
            },
        }
    };
    Ok(Some(Preview {
        message: MessageId::new(message),
        utc_ns,
        item,
        direction: DirectionCode::of(from_me, from_me_st),
        body,
    }))
}

type ChatRow = (
    Option<Vec<u8>>,
    Option<i64>,
    Option<Vec<u8>>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

fn conversation(
    c: &Connection,
    chat: i64,
    stats_stale: bool,
) -> rusqlite::Result<Option<Conversation>> {
    let row: Option<ChatRow> = c
        .prepare(SQL_CHAT_ROW)?
        .query_row([chat], |r| {
            Ok((
                bytes_of(r.get_ref(0)?),
                r.get(1)?,
                bytes_of(r.get_ref(2)?),
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
            ))
        })
        .optional()?;
    let Some((dn, dn_st, ci, ci_st, service, stats_id, count, undated, first, last)) = row else {
        return Ok(None);
    };
    let display = identifier(
        c,
        cell(&dn),
        dn_st,
        Entity::Chat,
        chat,
        COL_CHAT_DISPLAY_NAME,
    )?;
    let ident = identifier(c, cell(&ci), ci_st, Entity::Chat, chat, COL_CHAT_IDENTIFIER)?;
    let (first_participants, addresses) = participants(c, chat)?;
    let stats = match (stats_id, count, undated) {
        (Some(_), Some(n), Some(u)) => Some(ChatStatsFacts {
            items_recorded: u64::try_from(n).unwrap_or(0),
            undated: u64::try_from(u).unwrap_or(0),
            first_utc_ns: first,
            last_utc_ns: last,
        }),
        _ => None,
    };

    // The newest dated rows: the last dated instant and the preview.
    let newest: Vec<(i64, Vec<u8>)> = c
        .prepare(SQL_NEWEST_DATED)?
        .query_map(rusqlite::params![chat, PREVIEW_SCAN_ROWS], |r| {
            Ok((r.get(0)?, bytes_of(r.get_ref(1)?).unwrap_or_default()))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let last_dated_utc_ns = newest.first().and_then(|(_, k)| sortkey::decode_dated(k));
    let mut preview = None;
    for (id, key) in &newest {
        if folds_into(c, chat, *id)?.is_some() {
            continue;
        }
        if let Some(ns) = sortkey::decode_dated(key) {
            preview = preview_of(c, *id, ns, PREVIEW_UTF16)?;
        }
        break;
    }

    let floor = sortkey::dated(FLOOR_2007_NS);
    let first_from: Option<Option<Vec<u8>>> = c
        .prepare(SQL_FIRST_DATED_FROM)?
        .query_row(rusqlite::params![chat, floor.as_slice()], |r| {
            Ok(bytes_of(r.get_ref(0)?))
        })
        .optional()?;
    let first_dated_2007_utc_ns = first_from
        .flatten()
        .and_then(|k: Vec<u8>| sortkey::decode_dated(&k));
    let limit = CappedCount::limit(COUNT_CAP);
    let earlier: i64 = c
        .prepare(SQL_COUNT_BEFORE)?
        .query_row(rusqlite::params![chat, floor.as_slice(), limit], |r| {
            r.get(0)
        })?;
    let undated_live: i64 = c
        .prepare(SQL_COUNT_UNDATED)?
        .query_row(rusqlite::params![chat, limit], |r| r.get(0))?;
    let undated_live = CappedCount::of(undated_live, COUNT_CAP);

    Ok(Some(Conversation {
        id: ChatId::new(chat),
        label: label(display, first_participants, addresses, ident),
        service: service.and_then(ServiceClassCode::from_code),
        addresses,
        group: addresses > 1,
        stats,
        stats_stale,
        last_dated_utc_ns,
        first_dated_2007_utc_ns,
        earlier_dated: CappedCount::of(earlier, COUNT_CAP),
        undated: undated_live,
        empty: last_dated_utc_ns.is_none() && undated_live.n == 0,
        preview,
    }))
}

impl ArchiveReader {
    /// The archive's chat ids in `sort` order, optionally only those whose
    /// name, identifier or a participant's address contains `filter`
    /// (case-insensitive). At most [`CONVERSATION_INDEX_LIMIT`] ids, with
    /// `truncated` set beyond. One snapshot, under the `List` budget: a
    /// budget or deadline overrun is `QueryTooBroad`.
    pub fn conversation_index(
        &self,
        sort: ConversationSort,
        filter: Option<&NameFilter>,
    ) -> Result<ConversationIndex, ArchiveError> {
        self.require_current()?;
        let conn = self.conn();
        let deadline = Instant::now() + ViewBudget::List.window();
        conn.snapshot(|| {
            conn.viewing(ViewBudget::List, None, deadline, |p| {
                index_in(p, sort, filter)
            })
        })
    }

    /// The rows of up to [`CONVERSATIONS_PER_CALL`] chats, in the order
    /// asked. An id with no chat is left out. One snapshot, under the
    /// `List` budget.
    pub fn conversations(&self, ids: &[ChatId]) -> Result<Vec<Conversation>, ArchiveError> {
        self.require_current()?;
        if ids.len() > CONVERSATIONS_PER_CALL {
            return Err(ArchiveError::Internal("too many conversation ids"));
        }
        let conn = self.conn();
        let deadline = Instant::now() + ViewBudget::List.window();
        conn.snapshot(|| {
            conn.viewing(ViewBudget::List, None, deadline, |p| {
                let newest: Option<i64> = p.sql(|c| {
                    c.prepare(SQL_NEWEST_SESSION)?
                        .query_row([], |r| r.get(0))
                        .optional()
                })?;
                let stats_stale = newest.is_some_and(|s| s != SessionState::Completed.code());
                let mut out = Vec::with_capacity(ids.len());
                for id in ids {
                    p.checkpoint()?;
                    if let Some(row) = p.sql(|c| conversation(c, id.get(), stats_stale))? {
                        out.push(row);
                    }
                }
                Ok(out)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_filters_are_validated_and_fold_case() {
        assert_eq!(NameFilter::new("  "), Err(NameFilterError::Empty));
        assert_eq!(
            NameFilter::new(&"x".repeat(129)),
            Err(NameFilterError::TooLong)
        );
        assert!(NameFilter::new(&"x".repeat(128)).is_ok());
        assert_eq!(NameFilter::new("a\u{0}b"), Err(NameFilterError::Control));
        assert_eq!(NameFilter::new("a\nb"), Err(NameFilterError::Control));
        let f = NameFilter::new(" ÉCOLE ").expect("filter");
        assert!(f.matches("Notes de l'école"));
        assert!(!f.matches("ecole"));
        assert_eq!(format!("{f:?}"), "NameFilter(<6 bytes>)");
    }

    #[test]
    fn the_floor_is_2007() {
        // 2007-01-01T00:00:00Z: 37 years of days after 1970-01-01, nine
        // of them leap years.
        let days = 37 * 365 + 9;
        assert_eq!(FLOOR_2007_NS, days * 86_400 * 1_000_000_000);
    }

    fn names(display: Option<&str>, st: Option<i64>, ident: Option<&str>, addr: &[&str]) -> Names {
        Names {
            display: display.map(|s| s.as_bytes().to_vec()),
            display_st: st,
            identifier: ident.map(|s| s.as_bytes().to_vec()),
            addresses: addr.iter().map(|a| a.as_bytes().to_vec()).collect(),
        }
    }

    #[test]
    fn the_name_sort_key_follows_the_label_precedence() {
        assert_eq!(
            names(Some("Team"), None, Some("chat1"), &["a"]).sort_key(),
            Some("team".into())
        );
        // Oversized and unreadable names never fall through.
        assert_eq!(names(None, Some(3), Some("chat1"), &["a"]).sort_key(), None);
        assert_eq!(names(None, Some(4), Some("chat1"), &["a"]).sort_key(), None);
        // An empty or NULL name falls through to the participants.
        assert_eq!(
            names(Some(""), None, Some("c"), &["B", "a", "c", "d"]).sort_key(),
            Some("b, a, c".into())
        );
        assert_eq!(
            names(None, Some(1), Some("Chat9"), &[]).sort_key(),
            Some("chat9".into())
        );
        assert_eq!(names(None, Some(2), None, &[]).sort_key(), None);
        let f = NameFilter::new("ALICE").expect("f");
        assert!(names(None, Some(1), None, &["x", "alice@example.invalid"]).matches(&f));
        assert!(!names(Some("Bob"), None, Some("c"), &["x"]).matches(&f));
    }

    #[test]
    fn labels_follow_the_precedence() {
        let t = |s: &str| OptContent::Text(ContentText::from_str(s, 256));
        let p = |id: i64| Participant {
            handle: HandleId::new(id),
            address: t("a"),
            service: None,
        };
        assert!(matches!(
            label(t("Name"), vec![p(1)], 1, t("id")),
            ChatLabel::Name(_)
        ));
        assert_eq!(
            label(
                OptContent::Missing(ValueState::Oversized),
                vec![p(1)],
                1,
                t("id")
            ),
            ChatLabel::NameUnreadable(ValueState::Oversized)
        );
        assert!(matches!(
            label(t(""), vec![p(1), p(2), p(3)], 5, t("id")),
            ChatLabel::Participants { more: 2, .. }
        ));
        assert!(matches!(
            label(OptContent::Missing(ValueState::Null), vec![], 0, t("id")),
            ChatLabel::Identifier(_)
        ));
        assert_eq!(
            label(
                OptContent::Missing(ValueState::Absent),
                vec![],
                0,
                OptContent::Missing(ValueState::Null)
            ),
            ChatLabel::Unnamed
        );
    }
}
