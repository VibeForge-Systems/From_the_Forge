//! One assembly layer, two consumers (M5 spec §3.11): turning one message
//! id into an [`AssembledMessage`].
//!
//! - Every fact is a closed code or an [`OptContent`]; nothing is invented
//!   (I5). Derived facts (folding, chip states, labels) say they are
//!   derived through their type: a chip state is a [`ChipState`], never a
//!   claim about the phone.
//! - **Caps** ([`Caps`]) are a parameter. The thread pages, the reply
//!   thread and later the details and search hits pass the UI caps
//!   ([`Caps::PAGE`]): 8,192 UTF-16 units per body, 2,000 segments, 200
//!   reaction rows, 50 chips, 100 attachments. M6 will stream the same
//!   assembly without them, under a cancel token only.
//! - **Masking (D115)** happens here, before anything leaves the crate: a
//!   one-time code is a [`SegmentKind::Withheld`] segment unless the
//!   request carries [`Reveal::one_time_code`], and text captured before an
//!   unsend is [`EarlierText::Withheld`] unless it carries
//!   [`Reveal::earlier_text`]. A reply quote never reveals.
//! - Read, delivery and sent state are never read (D94).
//!
//! The work is split in two so a tripped `Assembly` budget can return the
//! items it has (§3.2): [`base`] (the message, its body, edits, labels and
//! attachments) and [`extras`] (reaction chips, the reply reference and the
//! reply count).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension};

use super::conversations::{preview_of, PreviewBody, FLOOR_2007_NS, ONE_TIME_CODE_BIT};
use super::text::{facts, url_runs, utf16_len, ContentText, MaskKind, OptContent, TextPiece};
use super::{
    is_reaction, stored_as_blob, AttachmentId, BodyId, CappedCount, ChatId, DirectionCode,
    HandleId, MessageId, COL_BODY_TEXT_RAW, IDENTIFIER_UTF16,
};
use crate::import::LineageId;
use crate::reader::BlobId;
use crate::schema::codes::{
    AssocKindCode, AssocTargetShape, AttributedOutcome, DisplayNoneReason, DisplaySource,
    EditLabelCode, Entity, ItemKindCode, ServiceClassCode, SessionState, TimeUnit, TsErrorCode,
    ValueState,
};
use crate::sortkey;

/// The bytes of `assoc_emoji` kept before a marker (§3.8).
pub const EMOJI_BYTES: usize = 64;
/// UTF-16 units of a reply quote (two lines of the originator's body).
pub const QUOTE_UTF16: u32 = 280;
/// The lines a reply quote keeps.
pub const QUOTE_LINES: usize = 2;
/// The rough upper bound of one serialised page (§7.3): the builder stops
/// before it.
pub const PAGE_BYTES: usize = 1 << 20;

/// What a request asks to reveal (D115). Both are `false` unless the user
/// pressed "Show code" or "Show earlier text" for that message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Reveal {
    /// Show text captured before an unsend, or before the current version
    /// lost its text.
    pub earlier_text: bool,
    /// Show one-time codes.
    pub one_time_code: bool,
}

/// The caps one consumer applies to an assembled message (§3.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Caps {
    /// UTF-16 units of shown body text.
    pub body_utf16: u32,
    /// Segments of one body; beyond, the body is plain text with a note.
    pub segments: usize,
    /// Reaction rows read per target; beyond, no chip state is derived.
    pub reactions: u64,
    /// Chips per message.
    pub chips: usize,
    /// Attachments per message.
    pub attachments: usize,
    /// The reply count's cap ("1,000+ replies").
    pub reply_count: u64,
}

impl Caps {
    /// The UI caps of thread pages and the reply thread.
    pub(crate) const PAGE: Caps = Caps {
        body_utf16: super::BODY_PAGE_UTF16,
        segments: 2000,
        reactions: 200,
        chips: 50,
        attachments: 100,
        reply_count: 1000,
    };
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A timestamp quad as recorded (`x_raw`, `x_st`, `x_unit`, `x_utc_ns`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecordedTime {
    /// The instant, when the unit is seconds or nanoseconds.
    pub utc_ns: Option<i64>,
    /// The raw value as recorded.
    pub raw: Option<i64>,
    /// How the raw value was read; `None` without a value.
    pub unit: Option<TimeUnit>,
    /// Why there is no value; `None` with one.
    pub state: Option<ValueState>,
    /// Dated before 2007-01-01: "outside the range this app expects".
    pub before_2007: bool,
}

/// Who sent a message, as recorded (§3.6). The sender is never filled in
/// from a chat's participants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SenderReason {
    /// `is_from_me = 1`: the backed-up phone's account. `handle_id` is
    /// then the other party, so it is never shown as the sender.
    FromMe,
    /// The recorded handle, resolved in this archive.
    Resolved(HandleId),
    /// `handle_id` is 0 or NULL in the source: "No sender recorded".
    NoSender,
    /// The column is not in this backup.
    ColumnAbsent,
    /// The value could not be read.
    Unreadable,
    /// A non-zero `handle_id` that resolves to no handle here.
    NotFound,
}

/// The backup set a direction value came from, when more than one lineage
/// recorded the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecordedBy {
    pub lineage: LineageId,
    /// The import session whose backup set the value.
    pub session: i64,
}

/// A label on one message (§3.6). Each is derived and says so in its
/// sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum MessageLabel {
    /// No usable date: listed in the undated section.
    Undated,
    /// Dated before 2007-01-01.
    Before2007,
    /// No unique identifier in its source; it may appear more than once.
    Unkeyed,
    /// `schedule_type` is non-zero (the value).
    ScheduleType(i64),
    /// App or link content (`balloon_bundle_id`), not decoded.
    AppContent,
    /// Sent with an effect (`expressive_send_style_id`), never animated.
    Effect,
    /// Not seen by the newest complete import of this lineage.
    NotInLatest(LineageId),
    /// Set or last seen by an import of an untested iOS version.
    Provisional,
}

/// "Listed in Recently Deleted in the backup of D", one per session.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecentlyDeletedMarker {
    /// The import session whose backup listed it.
    pub session: i64,
    /// The chat the listing named, when resolved.
    pub chat: Option<ChatId>,
    /// Later completed sessions of the same lineage that no longer list
    /// it ("not listed in the backup of D2").
    pub not_listed_later: Vec<i64>,
}

/// Why an association row is a timeline item, or what it folds into
/// (§3.5 step 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AssocVisibility {
    /// `assoc_present = 2`: whether it is an association is not known.
    Undeterminable,
    /// A present association without a kind.
    NoKind,
    /// A kind that never folds: a legacy sticker, a poll vote, an unknown
    /// type ([`AssociationFacts::kind`] says which).
    NotAReaction,
    /// A reaction whose target could not be read (`assoc_target_shape =
    /// 4`).
    TargetUnreadable,
    /// A reaction whose target is not uniquely identified in this archive.
    TargetNotUnique,
    /// A reaction whose target is a message in another conversation.
    TargetInAnotherChat(MessageId),
    /// A reaction folded into its target, a timeline item of this chat.
    Folded(MessageId),
}

/// The association facts of a message that is not a plain message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AssociationFacts {
    pub visibility: AssocVisibility,
    /// `None` when NULL or a code this build does not know.
    pub kind: Option<AssocKindCode>,
    /// `Bare` is resolved like any other target; the details panel notes
    /// that this form is unverified.
    pub shape: Option<AssocTargetShape>,
    pub target_part: Option<i64>,
    /// `assoc_emoji`, capped at [`EMOJI_BYTES`].
    pub emoji: Option<OptContent>,
}

/// A group event or another non-message item (§3.6). No actor or target
/// roles are asserted: the handles are listed, nothing more.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GroupEvent {
    /// `handle_ref` then `other_handle_ref`, when resolved.
    pub handles: Vec<HandleId>,
    /// `group_title`, for a rename.
    pub title: Option<OptContent>,
    /// The raw pair, for "Unrecognised item (item_type N, group_action_type
    /// M)".
    pub item_type: Option<i64>,
    pub group_action_type: Option<i64>,
}

/// What a body segment is.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SegmentKind {
    /// Text, rendered as text.
    Text(ContentText),
    /// Withheld in Rust (D115).
    Withheld(MaskKind),
    /// A U+FFFC whose part carries the `inline_emoji` flag: "Inline emoji
    /// image (not shown)".
    InlineEmoji,
    /// A U+FFFC whose part names this message's attachment (the equality is
    /// UNVERIFIED).
    Attachment(AttachmentId),
    /// Any other U+FFFC: "Attachment placeholder not matched to a file".
    UnmatchedPlaceholder,
}

/// One segment of a body: a run with the same formatting.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Segment {
    pub kind: SegmentKind,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    /// A mention: a styled span, never a link.
    pub mention: bool,
    /// "Transcription recorded on the phone".
    pub transcription: bool,
    /// A revealed one-time code.
    pub one_time_code: bool,
    /// Part of a URL run (text only, never clickable).
    pub url: bool,
    /// The URL run holds a bidi control or an invisible character.
    pub url_hidden: bool,
}

/// Whether formatting ranges were applied (§3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Formatting {
    /// No formatting ranges recorded.
    Plain,
    /// Applied: decoded `attributedBody`, no run issue.
    Applied,
    /// Ranges recorded, but the text came from the `text` column.
    NotApplied,
    /// "Formatting could not be read" (`run_issue`).
    Unreadable(Option<TsErrorCode>),
    /// More than [`Caps::segments`] segments: plain text with a note
    /// (one-time codes still withheld).
    TooManySegments,
}

/// A body that has text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BodyText {
    pub body: BodyId,
    pub segments: Vec<Segment>,
    pub formatting: Formatting,
    /// `None` for a code this build does not know.
    pub source: Option<DisplaySource>,
    /// The whole text's length in UTF-16 units; the rest comes from
    /// `message_text` (5b3).
    pub full_utf16: u32,
    /// The shown text was cut to the cap.
    pub truncated: bool,
    pub bidi_controls: bool,
    pub invisible: bool,
    /// "Some characters could not be decoded" (`display_source = 3`).
    pub lossy: bool,
    pub stored_as_blob: bool,
    /// The two stored text fields differ; `None` when not comparable.
    pub sources_differ: Option<bool>,
}

/// A body without text, and why (§3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct NoText {
    pub body: BodyId,
    pub reason: Option<DisplayNoneReason>,
    pub attributed: Option<AttributedOutcome>,
    pub attributed_error: Option<TsErrorCode>,
    /// The plain `text` column's state ("too large", "unreadable", "not in
    /// this backup"); `None` when it holds a value.
    pub text_column: Option<ValueState>,
}

/// The current body of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BodyView {
    Text(BodyText),
    NoText(NoText),
    /// The message has no current body row.
    NoBody,
}

/// Why earlier text is withheld (D115).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WithheldReason {
    /// The message has an unsent part ("before it was unsent").
    Unsent,
    /// Edit code 6 ("from an earlier version"); never called unsent.
    EarlierVersion,
    /// The current body has no text and an earlier one does.
    NoCurrentText,
}

/// Earlier body text, per D115.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EarlierText {
    /// No earlier version holds other text.
    None,
    /// Earlier text exists and is not withheld (the details list it).
    Available,
    /// Withheld until "Show earlier text".
    Withheld(WithheldReason),
    /// Revealed on request: the newest earlier version with text, and the
    /// session whose backup first held it.
    Revealed {
        reason: WithheldReason,
        text: ContentText,
        session: i64,
    },
}

/// Edits, unsends and stored versions of a message (§3.6, §3.7).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EditFacts {
    /// `None` for a code this build does not know. Codes 0, 1, 2 and 9
    /// never read "not edited"; code 6 is never called unsent.
    pub label: Option<EditLabelCode>,
    /// The parts with recorded edit entries ("Edited" marks these only).
    pub edited_parts: Vec<i64>,
    /// The parts listed as unsent.
    pub unsent_parts: Vec<i64>,
    /// The parts known for the bubble ("1 of 3 parts unsent").
    pub part_count: u32,
    /// Every part is unsent: a whole-bubble label.
    pub unsent_whole: bool,
    /// Body rows stored for the message ("N stored versions").
    pub stored_versions: u32,
    pub earlier: EarlierText,
}

/// What a reaction chip is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ReactionKind {
    Love,
    Like,
    Dislike,
    Laugh,
    Emphasize,
    Question,
    Emoji,
    Sticker,
}

impl ReactionKind {
    /// The kind of a tapback code and whether it adds (10–17) or removes
    /// (20–27).
    fn of(code: i64) -> Option<(ReactionKind, bool)> {
        let kind = match code % 10 {
            0 => ReactionKind::Love,
            1 => ReactionKind::Like,
            2 => ReactionKind::Dislike,
            3 => ReactionKind::Laugh,
            4 => ReactionKind::Emphasize,
            5 => ReactionKind::Question,
            6 => ReactionKind::Emoji,
            7 => ReactionKind::Sticker,
            _ => return None,
        };
        match code {
            10..=17 => Some((kind, true)),
            20..=27 => Some((kind, false)),
            _ => None,
        }
    }
}

/// Who a chip's rows came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ChipSender {
    /// `is_from_me = 1`.
    Me,
    /// `is_from_me = 0` with a resolved handle.
    Handle(HandleId),
    /// The sender cannot be determined: the row never combines with
    /// others.
    NotRecorded,
}

/// A chip's state, **derived** from the recorded rows (D93).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChipState {
    /// The latest recorded row adds the reaction.
    Shown,
    /// The latest recorded row removes it.
    Hidden,
    /// A relevant row is undated, or two tie on their date.
    Undeterminable,
    /// One row from an undeterminable sender that adds.
    Added,
    /// One row from an undeterminable sender that removes.
    Removed,
}

/// One chip: a (kind or emoji, sender) group of reaction rows.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReactionChip {
    pub kind: ReactionKind,
    /// The emoji of an emoji chip.
    pub emoji: Option<OptContent>,
    pub sender: ChipSender,
    /// The recorded rows, by id.
    pub rows: Vec<MessageId>,
    pub state: ChipState,
}

/// The reactions recorded on one target.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReactionSummary {
    /// Reaction rows, capped: over the cap, `chips` is empty ("Over 200
    /// reactions recorded; see details").
    pub rows: CappedCount,
    pub chips: Vec<ReactionChip>,
    /// Chips beyond [`Caps::chips`].
    pub more_chips: u64,
    /// "Removal of an emoji reaction (emoji not recorded)": never paired.
    pub unpaired: Vec<MessageId>,
}

/// A reply's reference to the message it replies to (§3.6).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReplyRef {
    /// The originator is in this conversation.
    Resolved {
        originator: MessageId,
        quote: PreviewBody,
    },
    /// The originator is a message in another conversation.
    InAnotherConversation {
        originator: MessageId,
        chat: Option<ChatId>,
        quote: PreviewBody,
    },
    /// "No uniquely identified message with this identifier is in the
    /// archive".
    NotUniquelyIdentified,
}

/// An attachment as a page lists it. The card is decided later (5b3, §5.9).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AttachmentRef {
    pub id: AttachmentId,
    /// `mime_type`, declared (a label only).
    pub declared_type: OptContent,
    /// `transfer_name`, never a path.
    pub name: OptContent,
    pub sticker: Option<bool>,
    /// `total_bytes`, declared.
    pub declared_bytes: Option<i64>,
    pub blob: Option<BlobId>,
    /// `blob.byte_len`, stored.
    pub stored_bytes: Option<i64>,
    /// Placed inline by a matching U+FFFC.
    pub placed_inline: bool,
}

/// One message, assembled (§3.11).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AssembledMessage {
    pub id: MessageId,
    /// `None` for an item code this build does not know.
    pub item: Option<ItemKindCode>,
    pub direction: DirectionCode,
    /// Set when more than one lineage recorded the message.
    pub direction_recorded_by: Option<RecordedBy>,
    pub sender: SenderReason,
    pub service: Option<ServiceClassCode>,
    pub date: RecordedTime,
    pub body: BodyView,
    pub edit: EditFacts,
    /// Sorted.
    pub labels: Vec<MessageLabel>,
    pub recently_deleted: Vec<RecentlyDeletedMarker>,
    /// For a row with an association (`assoc_present` 1 or 2).
    pub association: Option<AssociationFacts>,
    /// For any item that is not a plain message.
    pub event: Option<GroupEvent>,
    pub attachments: Vec<AttachmentRef>,
    /// Attachments beyond [`Caps::attachments`] ("N more; see details").
    pub more_attachments: u64,
    /// The extras below were loaded; `false` after a tripped `Assembly`
    /// budget, and the item then shows no chips rather than wrong ones.
    pub extras: bool,
    pub reactions: Option<ReactionSummary>,
    pub reply: Option<ReplyRef>,
    /// "N replies" across conversations, capped; absent on unkeyed
    /// messages and when there is none.
    pub reply_count: Option<CappedCount>,
}

// ---------------------------------------------------------------------------
// Statements (each pinned in tests/query_plans.rs)
// ---------------------------------------------------------------------------

const SQL_SESSIONS: &str = "SELECT id, lineage_id, rank, ios_version_untested, state, \
       full_observation FROM import_session";
const SQL_ITEM: &str = "SELECT key_kind, guid, is_from_me, is_from_me_st, handle_rowid, \
       handle_rowid_st, handle_ref, other_handle_ref, service_class, date_raw, date_st, \
       date_unit, date_utc_ns, item_type, group_action_type, item_kind, group_title, \
       group_title_st, assoc_present, assoc_kind, assoc_target_shape, assoc_target_guid, \
       assoc_target_part, assoc_emoji, assoc_emoji_st, length(balloon_bundle_id) > 0, \
       length(expressive_send_style_id) > 0, schedule_type, current_body_id, sort_key, \
       last_session_id \
     FROM message WHERE id = ?1";
const SQL_BODY: &str = "SELECT display_text, display_source, display_none_reason, \
       attributed_outcome, attributed_error, run_issue, sources_differ, text_raw_st, \
       edit_label, last_session_id \
     FROM message_body WHERE id = ?1";
const SQL_PARTS: &str = "SELECT utf16_start, utf16_end, attachment_guid, mention IS NOT NULL, \
       flags FROM message_body_part WHERE body_id = ?1 ORDER BY ordinal";
const SQL_PART_INDEXES: &str = "SELECT part_index FROM message_body_part \
     WHERE body_id = ?1 AND part_index IS NOT NULL";
const SQL_EDITED_PARTS: &str =
    "SELECT DISTINCT part FROM message_edit_entry WHERE body_id = ?1 ORDER BY part";
const SQL_UNSENT_PARTS: &str =
    "SELECT part FROM message_unsent_part WHERE body_id = ?1 ORDER BY ordinal";
const SQL_VERSIONS: &str = "SELECT b.id, b.first_rank, b.first_session_id, \
       b.display_text IS NOT NULL AND b.display_text IS NOT c.display_text \
     FROM message_body b LEFT JOIN message_body c ON c.id = ?2 \
     WHERE b.message_id = ?1";
const SQL_BODY_TEXT: &str = "SELECT display_text FROM message_body WHERE id = ?1";
const SQL_LINEAGES_SEEN: &str =
    "SELECT lineage_id, last_rank FROM lineage_seen WHERE entity = 3 AND row_id = ?1";
const SQL_RD_MARKERS: &str =
    "SELECT session_id, chat_ref FROM message_recently_deleted WHERE message_id = ?1";
const SQL_ATTACHMENTS: &str = "SELECT ma.attachment_id, a.key_kind, a.guid, a.mime_type, \
       a.mime_type_st, a.transfer_name, a.transfer_name_st, a.is_sticker, a.total_bytes, \
       a.current_blob_id, bl.byte_len \
     FROM message_attachment ma JOIN attachment a ON a.id = ma.attachment_id \
     LEFT JOIN blob bl ON bl.id = a.current_blob_id \
     WHERE ma.message_id = ?1 ORDER BY ma.ord_rank, ma.ord_rowid LIMIT ?2";
const SQL_ATTACHMENT_COUNT: &str = "SELECT count(*) FROM message_attachment WHERE message_id = ?1";
const SQL_REACTIONS: &str = "SELECT id, assoc_kind, assoc_emoji, assoc_emoji_st, is_from_me, \
       is_from_me_st, handle_ref, sort_key FROM message \
     WHERE assoc_target_guid = ?1 AND assoc_present = 1 \
       AND (assoc_kind BETWEEN 10 AND 17 OR assoc_kind BETWEEN 20 AND 27) LIMIT ?2";
const SQL_KEYED_BY_GUID: &str = "SELECT id FROM message WHERE key_kind = 1 AND guid = ?1";
const SQL_IN_CHAT: &str = "SELECT 1 FROM chat_message WHERE chat_id = ?1 AND message_id = ?2";
const SQL_SHARE_A_CHAT: &str = "SELECT 1 FROM chat_message a \
     JOIN chat_message b ON b.chat_id = a.chat_id AND b.message_id = ?2 \
     WHERE a.message_id = ?1 LIMIT 1";
const SQL_FIRST_CHAT: &str =
    "SELECT chat_id FROM chat_message WHERE message_id = ?1 ORDER BY chat_id LIMIT 1";
const SQL_REPLY_COUNT: &str = "SELECT count(*) FROM (SELECT 1 FROM message \
     WHERE thread_originator_guid = ?1 AND assoc_present = 0 AND item_kind = 1 LIMIT ?2)";
const SQL_REPLY_OF: &str = "SELECT thread_originator_guid FROM message WHERE id = ?1";
const SQL_HANDLE: &str = "SELECT address, address_st, service_class FROM handle WHERE id = ?1";

pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    [
        ("viewer sessions", SQL_SESSIONS),
        ("viewer message row", SQL_ITEM),
        ("viewer body row", SQL_BODY),
        ("viewer body parts", SQL_PARTS),
        ("viewer body part indexes", SQL_PART_INDEXES),
        ("viewer edited parts", SQL_EDITED_PARTS),
        ("viewer unsent parts", SQL_UNSENT_PARTS),
        ("viewer body versions", SQL_VERSIONS),
        ("viewer body text", SQL_BODY_TEXT),
        ("viewer lineages seen", SQL_LINEAGES_SEEN),
        ("viewer recently deleted markers", SQL_RD_MARKERS),
        ("viewer attachments", SQL_ATTACHMENTS),
        ("viewer attachment count", SQL_ATTACHMENT_COUNT),
        ("viewer reactions per target", SQL_REACTIONS),
        ("viewer shared chat", SQL_SHARE_A_CHAT),
        ("viewer first chat of a message", SQL_FIRST_CHAT),
        ("viewer reply count", SQL_REPLY_COUNT),
        ("viewer reply reference", SQL_REPLY_OF),
        ("viewer handle", SQL_HANDLE),
    ]
    .into_iter()
    .map(|(n, s)| (n, s.to_owned()))
    .collect()
}

// ---------------------------------------------------------------------------
// The per-call context
// ---------------------------------------------------------------------------

struct SessionFacts {
    lineage: i64,
    rank: Vec<u8>,
    untested: bool,
    completed: bool,
}

/// What every message of one call shares: the import sessions, read once.
pub(crate) struct Ctx {
    sessions: HashMap<i64, SessionFacts>,
    /// Per lineage, the highest rank of a full observation.
    full_rank: HashMap<i64, Vec<u8>>,
}

impl Ctx {
    pub(crate) fn load(c: &Connection) -> rusqlite::Result<Ctx> {
        let mut sessions = HashMap::new();
        let mut full_rank: HashMap<i64, Vec<u8>> = HashMap::new();
        let mut st = c.prepare(SQL_SESSIONS)?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let lineage: i64 = r.get(1)?;
            let rank: Vec<u8> = r.get(2)?;
            let untested: i64 = r.get(3)?;
            let state: i64 = r.get(4)?;
            let full: Option<i64> = r.get(5)?;
            if full == Some(1) {
                let e = full_rank.entry(lineage).or_default();
                if rank > *e {
                    e.clone_from(&rank);
                }
            }
            sessions.insert(
                id,
                SessionFacts {
                    lineage,
                    rank,
                    untested: untested == 1,
                    completed: state == SessionState::Completed.code(),
                },
            );
        }
        Ok(Ctx {
            sessions,
            full_rank,
        })
    }

    fn untested(&self, session: Option<i64>) -> bool {
        session
            .and_then(|s| self.sessions.get(&s))
            .is_some_and(|s| s.untested)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn bytes_of(v: ValueRef<'_>) -> Option<Vec<u8>> {
    match v {
        ValueRef::Text(b) | ValueRef::Blob(b) => Some(b.to_vec()),
        _ => None,
    }
}

fn opt_content(v: Option<&[u8]>, st: Option<i64>, cap: u32) -> OptContent {
    OptContent::from_cell(
        match v {
            Some(b) => ValueRef::Blob(b),
            None => ValueRef::Null,
        },
        st,
        cap,
    )
}

/// `assoc_emoji` as content: at most [`EMOJI_BYTES`] bytes, cut at a char
/// boundary and marked truncated beyond.
fn emoji_content(v: Option<&[u8]>, st: Option<i64>) -> OptContent {
    match (v, st) {
        (Some(b), None) if b.len() > EMOJI_BYTES => {
            let decoded = String::from_utf8_lossy(b);
            let mut end = EMOJI_BYTES.min(decoded.len());
            while end > 0 && !decoded.is_char_boundary(end) {
                end -= 1;
            }
            let mut t = ContentText::from_str(decoded.get(..end).unwrap_or(""), u32::MAX);
            t.truncated = true;
            t.full_utf16 = utf16_len(&decoded);
            t.lossy = matches!(decoded, std::borrow::Cow::Owned(_));
            OptContent::Text(t)
        }
        _ => opt_content(v, st, u32::MAX),
    }
}

fn u32_of(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn exists(c: &Connection, sql: &str, a: i64, b: i64) -> rusqlite::Result<bool> {
    Ok(c.prepare(sql)?
        .query_row([a, b], |_| Ok(()))
        .optional()?
        .is_some())
}

/// The keyed message a GUID resolves to (`message_guid`, `key_kind = 1`).
fn keyed(c: &Connection, guid: &[u8]) -> rusqlite::Result<Option<i64>> {
    c.prepare(SQL_KEYED_BY_GUID)?
        .query_row([guid], |r| r.get(0))
        .optional()
}

// ---------------------------------------------------------------------------
// Segments (§3.6 formatting, URL runs, placement, masking)
// ---------------------------------------------------------------------------

const FLAG_BOLD: i64 = 1;
const FLAG_ITALIC: i64 = 1 << 1;
const FLAG_UNDERLINE: i64 = 1 << 2;
const FLAG_STRIKE: i64 = 1 << 3;
const FLAG_OTC: i64 = 1 << ONE_TIME_CODE_BIT;
const FLAG_INLINE_EMOJI: i64 = 1 << 5;
const FLAG_TRANSCRIPTION: i64 = 1 << 6;

/// One `message_body_part` row, as the segment builder needs it.
pub(crate) struct PartRange {
    start: u32,
    end: u32,
    attachment_guid: Option<Vec<u8>>,
    mention: bool,
    flags: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    InlineEmoji,
    Attachment(i64),
    Unmatched,
}

/// The class of one char: the segment it belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Class {
    part: Option<usize>,
    masked: bool,
    url: Option<usize>,
}

/// The inputs of [`segments`].
pub(crate) struct SegmentInput<'a> {
    pub text: &'a str,
    pub parts: &'a [PartRange],
    /// Formatting applies (`display_source = 1`, no `run_issue`).
    pub applied: bool,
    /// Attachment GUIDs of the message, for inline placement.
    pub attachments: &'a [(Vec<u8>, i64)],
    pub reveal_code: bool,
    pub cap: u32,
    pub max_segments: usize,
}

/// The body split into segments, whether the cap cut it, and the
/// attachments placed inline. Returns `None` when there are more than
/// `max_segments` segments (the caller falls back to plain text).
fn build_segments(
    inp: &SegmentInput<'_>,
    applied: bool,
) -> Option<(Vec<Segment>, bool, BTreeSet<i64>)> {
    let urls = url_runs(inp.text);
    let otc: Vec<(u32, u32)> = inp
        .parts
        .iter()
        .filter(|p| p.flags & FLAG_OTC != 0)
        .map(|p| (p.start, p.end))
        .collect();
    let mut out: Vec<Segment> = Vec::new();
    let mut placed = BTreeSet::new();
    let mut left = inp.cap;
    let mut truncated = false;
    // The open text run: its class and byte range.
    let mut open: Option<(Class, usize, usize)> = None;
    // The parts by start, for a binary search per char.
    let mut by_start: Vec<(u32, u32, usize)> = inp
        .parts
        .iter()
        .enumerate()
        .map(|(i, p)| (p.start, p.end, i))
        .collect();
    by_start.sort_unstable();
    let mut ui = 0usize;
    let mut units = 0u32;

    let seg = |kind: SegmentKind, class: Class, parts: &[PartRange]| -> Segment {
        let flags = class.part.and_then(|i| parts.get(i)).map_or(0, |p| p.flags);
        let mention = class
            .part
            .and_then(|i| parts.get(i))
            .is_some_and(|p| p.mention);
        let url = class.url.and_then(|i| urls.get(i));
        Segment {
            kind,
            bold: flags & FLAG_BOLD != 0,
            italic: flags & FLAG_ITALIC != 0,
            underline: flags & FLAG_UNDERLINE != 0,
            strikethrough: flags & FLAG_STRIKE != 0,
            mention,
            transcription: flags & FLAG_TRANSCRIPTION != 0,
            one_time_code: class.masked,
            url: url.is_some(),
            url_hidden: url.is_some_and(|u| u.hidden),
        }
    };

    // Close the open run: a text segment (cut to what is left of the cap)
    // or a withheld one. Returns false once the cap is reached.
    let flush = |open: &mut Option<(Class, usize, usize)>,
                 out: &mut Vec<Segment>,
                 left: &mut u32,
                 truncated: &mut bool|
     -> bool {
        let Some((class, a, b)) = open.take() else {
            return true;
        };
        if class.masked && !inp.reveal_code {
            out.push(seg(
                SegmentKind::Withheld(MaskKind::OneTimeCode),
                Class { url: None, ..class },
                inp.parts,
            ));
            return true;
        }
        let piece = inp.text.get(a..b).unwrap_or("");
        let t = ContentText::from_str(piece, *left);
        *left = left.saturating_sub(utf16_len(piece));
        let stop = t.truncated;
        out.push(seg(SegmentKind::Text(t), class, inp.parts));
        if stop {
            *truncated = true;
            return false;
        }
        true
    };

    for (i, ch) in inp.text.char_indices() {
        let w = u32::try_from(ch.len_utf16()).unwrap_or(2);
        let (start, end) = (units, units.saturating_add(w));
        units = end;
        // The part holding this char: the last one starting at or before
        // it, if it reaches past it.
        let part = if applied {
            let n = by_start.partition_point(|&(s, _, _)| s <= start);
            n.checked_sub(1)
                .and_then(|k| by_start.get(k))
                .filter(|&&(_, e, _)| start < e)
                .map(|&(_, _, i)| i)
        } else {
            None
        };
        while urls.get(ui).is_some_and(|u| u.end_utf16 <= start) {
            ui += 1;
        }
        let url = urls
            .get(ui)
            .filter(|u| u.start_utf16 <= start && start < u.end_utf16)
            .map(|_| ui);
        let masked = match part.and_then(|j| inp.parts.get(j)) {
            Some(p) => p.flags & FLAG_OTC != 0,
            None => otc.iter().any(|&(s, e)| start < e && s < end),
        };
        let class = Class { part, masked, url };
        // A placeholder char in applied mode is its own segment.
        let place = if applied && ch == '\u{FFFC}' && !masked {
            Some(match part.and_then(|j| inp.parts.get(j)) {
                Some(p) if p.flags & FLAG_INLINE_EMOJI != 0 => Place::InlineEmoji,
                Some(p) => match p.attachment_guid.as_deref().and_then(|g| {
                    inp.attachments
                        .iter()
                        .find(|(ag, _)| ag.as_slice() == g)
                        .map(|(_, id)| *id)
                }) {
                    Some(id) => Place::Attachment(id),
                    None => Place::Unmatched,
                },
                None => Place::Unmatched,
            })
        } else {
            None
        };
        if let Some(place) = place {
            if !flush(&mut open, &mut out, &mut left, &mut truncated) {
                break;
            }
            let kind = match place {
                Place::InlineEmoji => SegmentKind::InlineEmoji,
                Place::Attachment(id) => {
                    placed.insert(id);
                    SegmentKind::Attachment(AttachmentId::new(id))
                }
                Place::Unmatched => SegmentKind::UnmatchedPlaceholder,
            };
            out.push(seg(kind, class, inp.parts));
        } else {
            match open.as_mut() {
                Some((c, _, b)) if *c == class => *b = i + ch.len_utf8(),
                _ => {
                    if !flush(&mut open, &mut out, &mut left, &mut truncated) {
                        break;
                    }
                    open = Some((class, i, i + ch.len_utf8()));
                }
            }
        }
        if out.len() > inp.max_segments {
            return None;
        }
    }
    if !truncated {
        flush(&mut open, &mut out, &mut left, &mut truncated);
    }
    if out.len() > inp.max_segments {
        return None;
    }
    Some((out, truncated, placed))
}

/// The body split into segments (§3.6): formatting only when it applies,
/// URL runs marked, U+FFFC placed, one-time codes withheld unless
/// revealed. Over the segment cap, the plain text (codes still withheld).
/// Returns the segments, the formatting outcome, whether the cap cut the
/// text, and the attachments placed inline.
pub(crate) fn segments(inp: &SegmentInput<'_>) -> (Vec<Segment>, bool, bool, BTreeSet<i64>) {
    let has_codes = inp.parts.iter().any(|p| p.flags & FLAG_OTC != 0);
    if inp.applied {
        if let Some((s, t, placed)) = build_segments(inp, true) {
            return (s, true, t, placed);
        }
    } else if has_codes && !inp.reveal_code {
        // The ranges cannot be trusted: withhold the whole body.
        return (
            vec![Segment {
                kind: SegmentKind::Withheld(MaskKind::OneTimeCode),
                bold: false,
                italic: false,
                underline: false,
                strikethrough: false,
                mention: false,
                transcription: false,
                one_time_code: true,
                url: false,
                url_hidden: false,
            }],
            false,
            false,
            BTreeSet::new(),
        );
    }
    // Plain text: no formatting, codes masked by range, URL runs marked.
    let plain = SegmentInput {
        parts: if inp.applied { inp.parts } else { &[] },
        ..*inp
    };
    let unlimited = SegmentInput {
        max_segments: usize::MAX,
        ..plain
    };
    match build_segments(&unlimited, false) {
        Some((s, t, _)) => (s, false, t, BTreeSet::new()),
        None => (Vec::new(), false, false, BTreeSet::new()),
    }
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

/// The `message` row's columns that assembly reads.
struct ItemRow {
    keyed: bool,
    guid: Option<Vec<u8>>,
    is_from_me: Option<i64>,
    is_from_me_st: Option<i64>,
    handle_rowid: Option<i64>,
    handle_rowid_st: Option<i64>,
    handle_ref: Option<i64>,
    other_handle_ref: Option<i64>,
    service: Option<i64>,
    date_raw: Option<i64>,
    date_st: Option<i64>,
    date_unit: Option<i64>,
    date_utc_ns: Option<i64>,
    item_type: Option<i64>,
    group_action_type: Option<i64>,
    item_kind: i64,
    group_title: Option<Vec<u8>>,
    group_title_st: Option<i64>,
    assoc_present: i64,
    assoc_kind: Option<i64>,
    assoc_shape: Option<i64>,
    assoc_target: Option<Vec<u8>>,
    assoc_part: Option<i64>,
    assoc_emoji: Option<Vec<u8>>,
    assoc_emoji_st: Option<i64>,
    app_content: bool,
    effect: bool,
    schedule_type: Option<i64>,
    body: Option<i64>,
    sort_key: Vec<u8>,
    last_session: i64,
}

fn item_row(c: &Connection, id: i64) -> rusqlite::Result<Option<ItemRow>> {
    c.prepare(SQL_ITEM)?
        .query_row([id], |r| {
            let key_kind: i64 = r.get(0)?;
            let flag = |i: usize| -> rusqlite::Result<bool> {
                Ok(r.get::<_, Option<i64>>(i)?.is_some_and(|v| v != 0))
            };
            Ok(ItemRow {
                keyed: key_kind == 1,
                guid: bytes_of(r.get_ref(1)?),
                is_from_me: r.get(2)?,
                is_from_me_st: r.get(3)?,
                handle_rowid: r.get(4)?,
                handle_rowid_st: r.get(5)?,
                handle_ref: r.get(6)?,
                other_handle_ref: r.get(7)?,
                service: r.get(8)?,
                date_raw: r.get(9)?,
                date_st: r.get(10)?,
                date_unit: r.get(11)?,
                date_utc_ns: r.get(12)?,
                item_type: r.get(13)?,
                group_action_type: r.get(14)?,
                item_kind: r.get(15)?,
                group_title: bytes_of(r.get_ref(16)?),
                group_title_st: r.get(17)?,
                assoc_present: r.get(18)?,
                assoc_kind: r.get(19)?,
                assoc_shape: r.get(20)?,
                assoc_target: bytes_of(r.get_ref(21)?),
                assoc_part: r.get(22)?,
                assoc_emoji: bytes_of(r.get_ref(23)?),
                assoc_emoji_st: r.get(24)?,
                app_content: flag(25)?,
                effect: flag(26)?,
                schedule_type: r.get(27)?,
                body: r.get(28)?,
                sort_key: bytes_of(r.get_ref(29)?).unwrap_or_default(),
                last_session: r.get(30)?,
            })
        })
        .optional()
}

fn sender(row: &ItemRow) -> SenderReason {
    if row.is_from_me == Some(1) && row.is_from_me_st.is_none() {
        return SenderReason::FromMe;
    }
    match row.handle_rowid_st.and_then(ValueState::from_code) {
        Some(ValueState::Absent) => return SenderReason::ColumnAbsent,
        Some(ValueState::Null) => return SenderReason::NoSender,
        Some(_) => return SenderReason::Unreadable,
        None if row.handle_rowid_st.is_some() => return SenderReason::Unreadable,
        None => {}
    }
    match (row.handle_rowid, row.handle_ref) {
        (None | Some(0), _) => SenderReason::NoSender,
        (_, None) => SenderReason::NotFound,
        (_, Some(h)) => SenderReason::Resolved(HandleId::new(h)),
    }
}

fn recorded_time(
    raw: Option<i64>,
    st: Option<i64>,
    unit: Option<i64>,
    utc: Option<i64>,
) -> RecordedTime {
    RecordedTime {
        utc_ns: utc,
        raw,
        unit: unit.and_then(TimeUnit::from_code),
        state: st.map(|s| ValueState::from_code(s).unwrap_or(ValueState::Unreadable)),
        before_2007: utc.is_some_and(|ns| ns < FLOOR_2007_NS),
    }
}

/// The association facts and the visibility class of `row` in `chat` (or,
/// without a chat, against every chat the message is in).
fn association(
    c: &Connection,
    chat: Option<i64>,
    id: i64,
    row: &ItemRow,
) -> rusqlite::Result<Option<AssociationFacts>> {
    if row.assoc_present == 0 {
        return Ok(None);
    }
    let kind = row.assoc_kind.and_then(AssocKindCode::from_code);
    let shape = row.assoc_shape.and_then(AssocTargetShape::from_code);
    let visibility = if row.assoc_present != 1 {
        AssocVisibility::Undeterminable
    } else if row.assoc_kind.is_none() {
        AssocVisibility::NoKind
    } else if !is_reaction(row.assoc_kind) {
        AssocVisibility::NotAReaction
    } else if row.assoc_shape == Some(AssocTargetShape::Unparsed.code()) {
        AssocVisibility::TargetUnreadable
    } else {
        match row.assoc_target.as_deref() {
            None => AssocVisibility::TargetNotUnique,
            Some(g) => match keyed(c, g)? {
                None => AssocVisibility::TargetNotUnique,
                Some(t) => {
                    let here = match chat {
                        Some(chat) => exists(c, SQL_IN_CHAT, chat, t)?,
                        None => exists(c, SQL_SHARE_A_CHAT, id, t)?,
                    };
                    if here {
                        AssocVisibility::Folded(MessageId::new(t))
                    } else {
                        AssocVisibility::TargetInAnotherChat(MessageId::new(t))
                    }
                }
            },
        }
    };
    Ok(Some(AssociationFacts {
        visibility,
        kind,
        shape,
        target_part: row.assoc_part,
        emoji: (row.assoc_emoji.is_some() || row.assoc_emoji_st.is_some())
            .then(|| emoji_content(row.assoc_emoji.as_deref(), row.assoc_emoji_st)),
    }))
}

fn event(row: &ItemRow, item: Option<ItemKindCode>) -> Option<GroupEvent> {
    if item == Some(ItemKindCode::Message) {
        return None;
    }
    Some(GroupEvent {
        handles: [row.handle_ref, row.other_handle_ref]
            .into_iter()
            .flatten()
            .map(HandleId::new)
            .collect(),
        title: (item == Some(ItemKindCode::GroupRenamed)).then(|| {
            opt_content(
                row.group_title.as_deref(),
                row.group_title_st,
                IDENTIFIER_UTF16,
            )
        }),
        item_type: row.item_type,
        group_action_type: row.group_action_type,
    })
}

/// The GUIDs of a message's keyed attachments, with their ids.
type KeyedGuids = Vec<(Vec<u8>, i64)>;

/// A body view, its edit label, the session that last saw it, and the
/// attachments placed inline.
type BodyParts = (BodyView, Option<i64>, Option<i64>, BTreeSet<i64>);

fn attachments(
    c: &Connection,
    id: i64,
    caps: &Caps,
) -> rusqlite::Result<(Vec<AttachmentRef>, u64, KeyedGuids)> {
    let limit = i64::try_from(caps.attachments).unwrap_or(i64::MAX);
    let mut st = c.prepare(SQL_ATTACHMENTS)?;
    let mut rows = st.query(rusqlite::params![id, limit])?;
    let mut out = Vec::new();
    let mut guids = Vec::new();
    while let Some(r) = rows.next()? {
        let aid: i64 = r.get(0)?;
        let key_kind: i64 = r.get(1)?;
        if key_kind == 1 {
            if let Some(g) = bytes_of(r.get_ref(2)?) {
                guids.push((g, aid));
            }
        }
        let sticker: Option<i64> = r.get(7)?;
        out.push(AttachmentRef {
            id: AttachmentId::new(aid),
            declared_type: OptContent::from_cell(r.get_ref(3)?, r.get(4)?, IDENTIFIER_UTF16),
            name: OptContent::from_cell(r.get_ref(5)?, r.get(6)?, IDENTIFIER_UTF16),
            sticker: sticker.map(|v| v == 1),
            declared_bytes: r.get(8)?,
            blob: r.get::<_, Option<i64>>(9)?.map(BlobId::new),
            stored_bytes: r.get(10)?,
            placed_inline: false,
        });
    }
    let more = if out.len() >= caps.attachments {
        let total: i64 = c
            .prepare(SQL_ATTACHMENT_COUNT)?
            .query_row([id], |r| r.get(0))?;
        u64::try_from(total)
            .unwrap_or(0)
            .saturating_sub(u64::try_from(out.len()).unwrap_or(0))
    } else {
        0
    };
    Ok((out, more, guids))
}

/// The current body, segmented.
fn body(
    c: &Connection,
    body_id: Option<i64>,
    atts: &[(Vec<u8>, i64)],
    caps: &Caps,
    reveal: Reveal,
) -> rusqlite::Result<BodyParts> {
    let Some(b) = body_id else {
        return Ok((BodyView::NoBody, None, None, BTreeSet::new()));
    };
    type BodyRow = (
        Option<String>,
        Option<i64>,
        Option<i64>,
        i64,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        i64,
        i64,
    );
    let row: Option<BodyRow> = c
        .prepare(SQL_BODY)?
        .query_row([b], |r| {
            let text = match r.get_ref(0)? {
                ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
                _ => None,
            };
            Ok((
                text,
                r.get(1)?,
                r.get(2)?,
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
    let Some((text, source, none, outcome, error, run_issue, differ, text_st, label, last)) = row
    else {
        return Ok((BodyView::NoBody, None, None, BTreeSet::new()));
    };
    let Some(text) = text.filter(|_| source.is_some()) else {
        return Ok((
            BodyView::NoText(NoText {
                body: BodyId::new(b),
                reason: none.and_then(DisplayNoneReason::from_code),
                attributed: AttributedOutcome::from_code(outcome),
                attributed_error: error.and_then(TsErrorCode::from_code),
                text_column: text_st.and_then(ValueState::from_code),
            }),
            Some(label),
            Some(last),
            BTreeSet::new(),
        ));
    };
    let parts: Vec<PartRange> = {
        let clamp = |v: i64| u32::try_from(v.max(0)).unwrap_or(u32::MAX);
        let mut st = c.prepare(SQL_PARTS)?;
        let rows = st.query_map([b], |r| {
            Ok(PartRange {
                start: clamp(r.get(0)?),
                end: clamp(r.get(1)?),
                attachment_guid: bytes_of(r.get_ref(2)?),
                mention: r.get::<_, i64>(3)? != 0,
                flags: r.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let applicable = source == Some(DisplaySource::AttributedBody.code()) && run_issue.is_none();
    let inp = SegmentInput {
        text: &text,
        parts: &parts,
        applied: applicable && !parts.is_empty(),
        attachments: atts,
        reveal_code: reveal.one_time_code,
        cap: caps.body_utf16,
        max_segments: caps.segments,
    };
    let (segs, applied, truncated, placed) = segments(&inp);
    let formatting = if run_issue.is_some() {
        Formatting::Unreadable(run_issue.and_then(TsErrorCode::from_code))
    } else if parts.is_empty() {
        Formatting::Plain
    } else if !applicable {
        Formatting::NotApplied
    } else if applied {
        Formatting::Applied
    } else {
        Formatting::TooManySegments
    };
    let (bidi_controls, invisible) = facts(&text);
    let blob = matches!(source, Some(2 | 3))
        && stored_as_blob(c, Entity::MessageBody, b, COL_BODY_TEXT_RAW)?;
    Ok((
        BodyView::Text(BodyText {
            body: BodyId::new(b),
            segments: segs,
            formatting,
            source: source.and_then(DisplaySource::from_code),
            full_utf16: utf16_len(&text),
            truncated,
            bidi_controls,
            invisible,
            lossy: source == Some(DisplaySource::TextLossy.code()),
            stored_as_blob: blob,
            sources_differ: differ.map(|d| d == 1),
        }),
        Some(label),
        Some(last),
        placed,
    ))
}

fn ints(c: &Connection, sql: &str, id: i64) -> rusqlite::Result<Vec<i64>> {
    let mut st = c.prepare(sql)?;
    let rows = st.query_map([id], |r| r.get::<_, i64>(0))?;
    rows.collect()
}

fn edits(
    c: &Connection,
    message: i64,
    view: &BodyView,
    label: Option<i64>,
    caps: &Caps,
    reveal: Reveal,
) -> rusqlite::Result<EditFacts> {
    let body = match view {
        BodyView::Text(t) => Some(t.body.get()),
        BodyView::NoText(n) => Some(n.body.get()),
        BodyView::NoBody => None,
    };
    let (edited, unsent, part_indexes) = match body {
        Some(b) => (
            ints(c, SQL_EDITED_PARTS, b)?,
            ints(c, SQL_UNSENT_PARTS, b)?,
            ints(c, SQL_PART_INDEXES, b)?,
        ),
        None => (Vec::new(), Vec::new(), Vec::new()),
    };
    let mut parts: BTreeSet<i64> = part_indexes.into_iter().collect();
    if parts.is_empty() && matches!(view, BodyView::Text(_)) {
        parts.insert(0);
    }
    let unsent_set: BTreeSet<i64> = unsent.iter().copied().collect();
    let unsent_whole = !unsent.is_empty() && parts.is_subset(&unsent_set);
    let part_count = u32_of(parts.union(&unsent_set).count());

    // Versions, ordered by first rank: which earlier ones hold other text.
    let mut versions: Vec<(Vec<u8>, i64, i64, bool)> = {
        let mut st = c.prepare(SQL_VERSIONS)?;
        let rows = st.query_map(rusqlite::params![message, body], |r| {
            Ok((
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<i64>>(3)?.is_some_and(|v| v == 1),
            ))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    versions.sort();
    let stored = u32_of(versions.len());
    let other: Vec<&(Vec<u8>, i64, i64, bool)> = versions
        .iter()
        .filter(|v| Some(v.1) != body && v.3)
        .collect();
    let label_code = label.and_then(EditLabelCode::from_code);
    let earlier = if other.is_empty() {
        EarlierText::None
    } else if !unsent.is_empty() || !matches!(view, BodyView::Text(_)) {
        let reason = if !unsent.is_empty() {
            WithheldReason::Unsent
        } else if label_code == Some(EditLabelCode::DateEditedNoBody) {
            WithheldReason::EarlierVersion
        } else {
            WithheldReason::NoCurrentText
        };
        match other.last() {
            Some(newest) if reveal.earlier_text => {
                let text: Option<String> = c
                    .prepare(SQL_BODY_TEXT)?
                    .query_row([newest.1], |r| {
                        Ok(match r.get_ref(0)? {
                            ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
                            _ => None,
                        })
                    })
                    .optional()?
                    .flatten();
                match text {
                    Some(t) => EarlierText::Revealed {
                        reason,
                        text: ContentText::from_str(&t, caps.body_utf16),
                        session: newest.2,
                    },
                    None => EarlierText::Withheld(reason),
                }
            }
            _ => EarlierText::Withheld(reason),
        }
    } else {
        EarlierText::Available
    };
    Ok(EditFacts {
        label: label_code,
        edited_parts: edited,
        unsent_parts: unsent,
        part_count,
        unsent_whole,
        stored_versions: stored,
        earlier,
    })
}

/// Labels and provenance (§3.6), and the lineage that set the direction
/// when more than one recorded the message.
fn labels(
    c: &Connection,
    ctx: &Ctx,
    id: i64,
    row: &ItemRow,
    body_session: Option<i64>,
) -> rusqlite::Result<(
    Vec<MessageLabel>,
    Vec<RecentlyDeletedMarker>,
    Option<RecordedBy>,
)> {
    let mut labels = Vec::new();
    if !sortkey::is_dated(&row.sort_key) {
        labels.push(MessageLabel::Undated);
    } else if sortkey::decode_dated(&row.sort_key).is_some_and(|ns| ns < FLOOR_2007_NS) {
        labels.push(MessageLabel::Before2007);
    }
    if !row.keyed {
        labels.push(MessageLabel::Unkeyed);
    }
    if let Some(st) = row.schedule_type.filter(|v| *v != 0) {
        labels.push(MessageLabel::ScheduleType(st));
    }
    if row.app_content {
        labels.push(MessageLabel::AppContent);
    }
    if row.effect {
        labels.push(MessageLabel::Effect);
    }
    let seen: Vec<(i64, Vec<u8>)> = {
        let mut st = c.prepare(SQL_LINEAGES_SEEN)?;
        let rows = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (lineage, last_rank) in &seen {
        if ctx
            .full_rank
            .get(lineage)
            .is_some_and(|full| last_rank < full)
        {
            labels.push(MessageLabel::NotInLatest(LineageId::new(*lineage)));
        }
    }
    if ctx.untested(Some(row.last_session)) || ctx.untested(body_session) {
        labels.push(MessageLabel::Provisional);
    }
    labels.sort();
    let recorded_by = if seen.len() > 1 {
        ctx.sessions.get(&row.last_session).map(|s| RecordedBy {
            lineage: LineageId::new(s.lineage),
            session: row.last_session,
        })
    } else {
        None
    };
    // Recently Deleted markers, one per session.
    let markers: Vec<(i64, Option<i64>)> = {
        let mut st = c.prepare(SQL_RD_MARKERS)?;
        let rows = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let listing: BTreeSet<i64> = markers.iter().map(|(s, _)| *s).collect();
    let mut rd: BTreeMap<i64, RecentlyDeletedMarker> = BTreeMap::new();
    for (session, chat) in markers {
        let Some(s) = ctx.sessions.get(&session) else {
            continue;
        };
        let mut later: Vec<i64> = ctx
            .sessions
            .iter()
            .filter(|(sid, o)| {
                o.lineage == s.lineage && o.rank > s.rank && o.completed && !listing.contains(sid)
            })
            .map(|(sid, _)| *sid)
            .collect();
        later.sort_unstable();
        rd.entry(session).or_insert(RecentlyDeletedMarker {
            session,
            chat: chat.map(ChatId::new),
            not_listed_later: later,
        });
    }
    Ok((labels, rd.into_values().collect(), recorded_by))
}

/// The base of one message: everything but the extras. `chat` is the
/// conversation it is shown in (`None` in the reply thread). `None` when
/// the message does not exist.
pub(crate) fn base(
    c: &Connection,
    ctx: &Ctx,
    chat: Option<i64>,
    id: i64,
    caps: &Caps,
    reveal: Reveal,
) -> rusqlite::Result<Option<(AssembledMessage, Extras)>> {
    let Some(row) = item_row(c, id)? else {
        return Ok(None);
    };
    let item = ItemKindCode::from_code(row.item_kind);
    let (mut atts, more, guids) = attachments(c, id, caps)?;
    let (body, label, body_session, placed) = body(c, row.body, &guids, caps, reveal)?;
    for a in &mut atts {
        a.placed_inline = placed.contains(&a.id.get());
    }
    let edit = edits(c, id, &body, label, caps, reveal)?;
    let (labels, recently_deleted, recorded_by) = labels(c, ctx, id, &row, body_session)?;
    let msg = AssembledMessage {
        id: MessageId::new(id),
        item,
        direction: DirectionCode::of(row.is_from_me, row.is_from_me_st),
        direction_recorded_by: recorded_by,
        sender: sender(&row),
        service: row.service.and_then(ServiceClassCode::from_code),
        date: recorded_time(row.date_raw, row.date_st, row.date_unit, row.date_utc_ns),
        body,
        edit,
        labels,
        recently_deleted,
        association: association(c, chat, id, &row)?,
        event: event(&row, item),
        attachments: atts,
        more_attachments: more,
        extras: false,
        reactions: None,
        reply: None,
        reply_count: None,
    };
    let extras = Extras {
        guid: row.guid.filter(|_| row.keyed),
    };
    Ok(Some((msg, extras)))
}

/// What [`extras`] needs from the base row.
pub(crate) struct Extras {
    /// The GUID of a keyed message (reactions and replies resolve to keyed
    /// messages only).
    guid: Option<Vec<u8>>,
}

/// One reaction row on a target.
struct ReactionRow {
    id: i64,
    kind: ReactionKind,
    adds: bool,
    emoji: Option<Vec<u8>>,
    emoji_st: Option<i64>,
    sender: ChipSender,
    key: Vec<u8>,
}

fn reactions(
    c: &Connection,
    guid: &[u8],
    caps: &Caps,
) -> rusqlite::Result<Option<ReactionSummary>> {
    let limit = CappedCount::limit(caps.reactions);
    let mut st = c.prepare(SQL_REACTIONS)?;
    let mut rows = st.query(rusqlite::params![guid, limit])?;
    let mut all: Vec<ReactionRow> = Vec::new();
    while let Some(r) = rows.next()? {
        let code: i64 = r.get(1)?;
        let Some((kind, adds)) = ReactionKind::of(code) else {
            continue;
        };
        let fm: Option<i64> = r.get(4)?;
        let fm_st: Option<i64> = r.get(5)?;
        let handle: Option<i64> = r.get(6)?;
        let sender = match (fm, fm_st, handle) {
            (Some(1), None, _) => ChipSender::Me,
            (Some(0), None, Some(h)) => ChipSender::Handle(HandleId::new(h)),
            _ => ChipSender::NotRecorded,
        };
        all.push(ReactionRow {
            id: r.get(0)?,
            kind,
            adds,
            emoji: bytes_of(r.get_ref(2)?),
            emoji_st: r.get(3)?,
            sender,
            key: bytes_of(r.get_ref(7)?).unwrap_or_default(),
        });
    }
    if all.is_empty() {
        return Ok(None);
    }
    let count = CappedCount::of(i64::try_from(all.len()).unwrap_or(i64::MAX), caps.reactions);
    if count.capped {
        return Ok(Some(ReactionSummary {
            rows: count,
            chips: Vec::new(),
            more_chips: 0,
            unpaired: Vec::new(),
        }));
    }
    // Group by (kind, emoji bytes, sender); undeterminable senders and
    // emoji removals without an emoji stand alone.
    type Key = (ReactionKind, Option<Vec<u8>>, ChipSender);
    let mut groups: BTreeMap<Key, Vec<&ReactionRow>> = BTreeMap::new();
    let mut single: Vec<ReactionChip> = Vec::new();
    let mut unpaired: Vec<MessageId> = Vec::new();
    for r in &all {
        let emoji = (r.kind == ReactionKind::Emoji)
            .then(|| r.emoji.clone())
            .flatten();
        if r.kind == ReactionKind::Emoji && !r.adds && emoji.is_none() {
            unpaired.push(MessageId::new(r.id));
            continue;
        }
        if r.sender == ChipSender::NotRecorded {
            single.push(ReactionChip {
                kind: r.kind,
                emoji: (r.kind == ReactionKind::Emoji)
                    .then(|| emoji_content(r.emoji.as_deref(), r.emoji_st)),
                sender: ChipSender::NotRecorded,
                rows: vec![MessageId::new(r.id)],
                state: if r.adds {
                    ChipState::Added
                } else {
                    ChipState::Removed
                },
            });
            continue;
        }
        groups.entry((r.kind, emoji, r.sender)).or_default().push(r);
    }
    let mut chips: Vec<ReactionChip> = groups
        .into_iter()
        .map(|((kind, _, sender), rows)| {
            let undated = rows.iter().any(|r| !sortkey::is_dated(&r.key));
            let mut keys: Vec<&[u8]> = rows.iter().map(|r| r.key.as_slice()).collect();
            keys.sort_unstable();
            let tied = keys.windows(2).any(|w| w.first() == w.get(1));
            let state = if undated || tied {
                ChipState::Undeterminable
            } else {
                match rows.iter().max_by(|a, b| a.key.cmp(&b.key)) {
                    Some(latest) if latest.adds => ChipState::Shown,
                    _ => ChipState::Hidden,
                }
            };
            let emoji = rows
                .first()
                .filter(|_| kind == ReactionKind::Emoji)
                .map(|r| emoji_content(r.emoji.as_deref(), r.emoji_st));
            let mut ids: Vec<MessageId> = rows.iter().map(|r| MessageId::new(r.id)).collect();
            ids.sort();
            ReactionChip {
                kind,
                emoji,
                sender,
                rows: ids,
                state,
            }
        })
        .collect();
    chips.extend(single);
    let more = chips.len().saturating_sub(caps.chips);
    chips.truncate(caps.chips);
    unpaired.sort();
    Ok(Some(ReactionSummary {
        rows: count,
        chips,
        more_chips: u64::try_from(more).unwrap_or(u64::MAX),
        unpaired,
    }))
}

/// Cut a quote to its first [`QUOTE_LINES`] lines.
fn two_lines(body: PreviewBody) -> PreviewBody {
    let PreviewBody::Text(pieces) = body else {
        return body;
    };
    let mut out = Vec::new();
    let mut lines = 1usize;
    for p in pieces {
        match p {
            TextPiece::Text(t) => {
                let s = t.text.expose();
                let mut cut = None;
                for (i, ch) in s.char_indices() {
                    if ch == '\n' {
                        if lines == QUOTE_LINES {
                            cut = Some(i);
                            break;
                        }
                        lines += 1;
                    }
                }
                match cut {
                    Some(i) => {
                        let mut kept = ContentText::from_str(s.get(..i).unwrap_or(""), u32::MAX);
                        kept.truncated = true;
                        kept.lossy = t.lossy;
                        kept.stored_as_blob = t.stored_as_blob;
                        out.push(TextPiece::Text(kept));
                        return PreviewBody::Text(out);
                    }
                    None => out.push(TextPiece::Text(t)),
                }
            }
            masked => out.push(masked),
        }
    }
    PreviewBody::Text(out)
}

fn reply(c: &Connection, chat: Option<i64>, id: i64) -> rusqlite::Result<Option<ReplyRef>> {
    let guid: Option<Vec<u8>> = c
        .prepare(SQL_REPLY_OF)?
        .query_row([id], |r| Ok(bytes_of(r.get_ref(0)?)))
        .optional()?
        .flatten();
    let Some(guid) = guid.filter(|g| !g.is_empty()) else {
        return Ok(None);
    };
    let Some(o) = keyed(c, &guid)? else {
        return Ok(Some(ReplyRef::NotUniquelyIdentified));
    };
    let quote = preview_of(c, o, 0, QUOTE_UTF16)?
        .map(|p| two_lines(p.body))
        .unwrap_or(PreviewBody::NoBody);
    let here = match chat {
        Some(chat) => exists(c, SQL_IN_CHAT, chat, o)?,
        None => exists(c, SQL_SHARE_A_CHAT, id, o)?,
    };
    if here {
        return Ok(Some(ReplyRef::Resolved {
            originator: MessageId::new(o),
            quote,
        }));
    }
    let first: Option<i64> = c
        .prepare(SQL_FIRST_CHAT)?
        .query_row([o], |r| r.get(0))
        .optional()?;
    Ok(Some(ReplyRef::InAnotherConversation {
        originator: MessageId::new(o),
        chat: first.map(ChatId::new),
        quote,
    }))
}

/// The extras of one message: reaction chips, the reply reference and the
/// reply count. Sets [`AssembledMessage::extras`].
pub(crate) fn extras(
    c: &Connection,
    chat: Option<i64>,
    m: &mut AssembledMessage,
    x: &Extras,
    caps: &Caps,
) -> rusqlite::Result<()> {
    if let Some(guid) = &x.guid {
        m.reactions = reactions(c, guid, caps)?;
        let limit = CappedCount::limit(caps.reply_count);
        let n: i64 = c
            .prepare(SQL_REPLY_COUNT)?
            .query_row(rusqlite::params![guid, limit], |r| r.get(0))?;
        m.reply_count = (n > 0).then(|| CappedCount::of(n, caps.reply_count));
    }
    m.reply = reply(c, chat, m.id.get())?;
    m.extras = true;
    Ok(())
}

/// One handle as a page lists it.
pub(crate) fn handle(c: &Connection, id: i64) -> rusqlite::Result<Option<super::Participant>> {
    c.prepare(SQL_HANDLE)?
        .query_row([id], |r| {
            let service: Option<i64> = r.get(2)?;
            Ok((
                OptContent::from_cell(r.get_ref(0)?, r.get(1)?, IDENTIFIER_UTF16),
                service,
            ))
        })
        .optional()
        .map(|o| {
            o.map(|(address, service)| super::Participant {
                handle: HandleId::new(id),
                address,
                service: service.and_then(ServiceClassCode::from_code),
            })
        })
}

/// The handles a message refers to: its sender, its event's handles and
/// its chip senders.
pub(crate) fn handles_of(m: &AssembledMessage) -> Vec<HandleId> {
    let mut out = Vec::new();
    if let SenderReason::Resolved(h) = m.sender {
        out.push(h);
    }
    if let Some(e) = &m.event {
        out.extend(e.handles.iter().copied());
    }
    if let Some(r) = &m.reactions {
        for chip in &r.chips {
            if let ChipSender::Handle(h) = chip.sender {
                out.push(h);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Size bound (§7.3)
// ---------------------------------------------------------------------------

/// Bytes per UTF-16 unit allowed for escaping in the serialised form.
const BYTES_PER_UNIT: usize = 6;
/// A fixed overhead per field or small value.
const FIELD: usize = 48;

fn text_bound(t: &ContentText) -> usize {
    utf16_len(t.text.expose()) as usize * BYTES_PER_UNIT + 8 * FIELD
}

fn opt_bound(o: &OptContent) -> usize {
    match o {
        OptContent::Text(t) => text_bound(t),
        OptContent::Missing(_) => FIELD,
    }
}

fn preview_bound(p: &PreviewBody) -> usize {
    match p {
        PreviewBody::Text(pieces) => pieces
            .iter()
            .map(|p| match p {
                TextPiece::Text(t) => text_bound(t),
                TextPiece::Masked(_) => FIELD,
            })
            .sum(),
        _ => FIELD,
    }
}

/// A running upper bound of one message's serialised size.
pub(crate) fn size_bound(m: &AssembledMessage) -> usize {
    let mut n = 40 * FIELD;
    if let BodyView::Text(t) = &m.body {
        for s in &t.segments {
            n += 10 * FIELD;
            if let SegmentKind::Text(t) = &s.kind {
                n += text_bound(t);
            }
        }
    }
    if let EarlierText::Revealed { text, .. } = &m.edit.earlier {
        n += text_bound(text);
    }
    n += (m.edit.edited_parts.len() + m.edit.unsent_parts.len()) * 24;
    n += m.labels.len() * FIELD + m.recently_deleted.len() * 4 * FIELD;
    if let Some(a) = &m.association {
        n += 6 * FIELD + a.emoji.as_ref().map_or(0, opt_bound);
    }
    if let Some(e) = &m.event {
        n += 4 * FIELD + e.handles.len() * 24 + e.title.as_ref().map_or(0, opt_bound);
    }
    for a in &m.attachments {
        n += 8 * FIELD + opt_bound(&a.declared_type) + opt_bound(&a.name);
    }
    if let Some(r) = &m.reactions {
        n += 4 * FIELD + r.unpaired.len() * 24;
        for chip in &r.chips {
            n += 5 * FIELD + chip.rows.len() * 24 + chip.emoji.as_ref().map_or(0, opt_bound);
        }
    }
    if let Some(r) = &m.reply {
        n += 3 * FIELD
            + match r {
                ReplyRef::Resolved { quote, .. }
                | ReplyRef::InAnotherConversation { quote, .. } => preview_bound(quote),
                ReplyRef::NotUniquelyIdentified => 0,
            };
    }
    n
}

/// The size bound of one handle row.
pub(crate) fn handle_bound(p: &super::Participant) -> usize {
    3 * FIELD + opt_bound(&p.address)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(start: u32, end: u32, flags: i64, guid: Option<&str>) -> PartRange {
        PartRange {
            start,
            end,
            attachment_guid: guid.map(|g| g.as_bytes().to_vec()),
            mention: false,
            flags,
        }
    }

    fn input<'a>(
        text: &'a str,
        parts: &'a [PartRange],
        applied: bool,
        atts: &'a [(Vec<u8>, i64)],
    ) -> SegmentInput<'a> {
        SegmentInput {
            text,
            parts,
            applied,
            attachments: atts,
            reveal_code: false,
            cap: 8192,
            max_segments: 2000,
        }
    }

    fn texts(s: &[Segment]) -> Vec<String> {
        s.iter()
            .map(|s| match &s.kind {
                SegmentKind::Text(t) => t.text.expose().clone(),
                SegmentKind::Withheld(_) => "<withheld>".into(),
                SegmentKind::InlineEmoji => "<emoji>".into(),
                SegmentKind::Attachment(a) => format!("<att {}>", a.get()),
                SegmentKind::UnmatchedPlaceholder => "<unmatched>".into(),
            })
            .collect()
    }

    #[test]
    fn formatting_codes_and_placeholders_become_segments() {
        // "Hi " bold, "123456" a one-time code, U+FFFC an attachment, an
        // inline emoji, an unmatched placeholder.
        let text = "Hi 123456\u{FFFC}\u{FFFC}\u{FFFC}";
        let parts = [
            part(0, 3, FLAG_BOLD, None),
            part(3, 9, FLAG_OTC, None),
            part(9, 10, 0, Some("ATT-1")),
            part(10, 11, FLAG_INLINE_EMOJI, None),
            part(11, 12, 0, Some("ATT-NONE")),
        ];
        let atts = [(b"ATT-1".to_vec(), 7)];
        let (segs, applied, truncated, placed) = segments(&input(text, &parts, true, &atts));
        assert!(applied && !truncated);
        assert_eq!(
            texts(&segs),
            ["Hi ", "<withheld>", "<att 7>", "<emoji>", "<unmatched>"]
        );
        assert!(segs.first().is_some_and(|s| s.bold));
        assert!(placed.contains(&7));
        // Revealed, the code is text marked as a code.
        let mut inp = input(text, &parts, true, &atts);
        inp.reveal_code = true;
        let (segs, ..) = segments(&inp);
        assert_eq!(texts(&segs).get(1).map(String::as_str), Some("123456"));
        assert!(segs.get(1).is_some_and(|s| s.one_time_code));
    }

    #[test]
    fn untrusted_ranges_withhold_the_whole_body() {
        let parts = [part(3, 9, FLAG_OTC, None)];
        let (segs, applied, ..) = segments(&input("Hi 123456", &parts, false, &[]));
        assert!(!applied);
        assert_eq!(texts(&segs), ["<withheld>"]);
        // Plain text without codes: one segment, U+FFFC stays text.
        let (segs, ..) = segments(&input("a\u{FFFC}b", &[], false, &[]));
        assert_eq!(texts(&segs), ["a\u{FFFC}b"]);
    }

    #[test]
    fn url_runs_split_segments_and_carry_the_hidden_fact() {
        let text = "see https://a.invalid/x and http://b\u{202E}c.invalid";
        let (segs, ..) = segments(&input(text, &[], false, &[]));
        assert_eq!(
            texts(&segs),
            [
                "see ",
                "https://a.invalid/x",
                " and ",
                "http://b\u{202E}c.invalid"
            ]
        );
        let flags: Vec<(bool, bool)> = segs.iter().map(|s| (s.url, s.url_hidden)).collect();
        assert_eq!(
            flags,
            [(false, false), (true, false), (false, false), (true, true)]
        );
    }

    #[test]
    fn the_cap_cuts_and_too_many_segments_fall_back_with_codes_masked() {
        let text = "x".repeat(20);
        let mut inp = input(&text, &[], false, &[]);
        inp.cap = 5;
        let (segs, _, truncated, _) = segments(&inp);
        assert!(truncated);
        assert_eq!(texts(&segs), ["xxxxx"]);
        // 6 alternating parts over a cap of 3 segments: plain, but the code
        // stays withheld.
        let parts: Vec<PartRange> = (0..6u32)
            .map(|i| {
                let f = if i == 2 {
                    FLAG_OTC
                } else {
                    FLAG_BOLD * i64::from(i % 2)
                };
                part(i * 2, i * 2 + 2, f, None)
            })
            .collect();
        let text = "aabbccddeeff";
        let mut inp = input(text, &parts, true, &[]);
        inp.max_segments = 3;
        let (segs, applied, ..) = segments(&inp);
        assert!(!applied);
        assert_eq!(texts(&segs), ["aabb", "<withheld>", "ddeeff"]);
    }

    #[test]
    fn a_code_splitting_a_surrogate_pair_masks_the_whole_char() {
        let text = "a\u{1F600}b";
        let parts = [part(0, 2, FLAG_OTC, None)];
        let mut inp = input(text, &parts, false, &[]);
        inp.reveal_code = false;
        // Not applied with codes: withheld whole.
        let (segs, ..) = segments(&inp);
        assert_eq!(texts(&segs), ["<withheld>"]);
        // In the plain fallback, a range that covers 'a' and half the emoji
        // masks the emoji whole.
        let mut inp = input(text, &parts, true, &[]);
        inp.max_segments = 0;
        let (segs, ..) = segments(&inp);
        assert_eq!(texts(&segs), ["<withheld>", "b"]);
    }

    #[test]
    fn reaction_kinds_cover_the_tapback_codes() {
        for code in (10..=17).chain(20..=27) {
            let (_, adds) = ReactionKind::of(code).expect("kind");
            assert_eq!(adds, code < 20);
        }
        assert!(ReactionKind::of(18).is_none());
        assert!(ReactionKind::of(3).is_none());
        assert_eq!(ReactionKind::of(16).map(|k| k.0), Some(ReactionKind::Emoji));
    }

    #[test]
    fn quotes_keep_two_lines() {
        let body = PreviewBody::Text(vec![TextPiece::Text(ContentText::from_str(
            "one\ntwo\nthree",
            100,
        ))]);
        let PreviewBody::Text(p) = two_lines(body) else {
            panic!("text");
        };
        let [TextPiece::Text(t)] = p.as_slice() else {
            panic!("one piece");
        };
        assert_eq!(t.text.expose(), "one\ntwo");
        assert!(t.truncated);
    }

    #[test]
    fn long_emoji_are_cut_at_a_char_boundary() {
        let long = "\u{1F600}".repeat(20); // 80 bytes
        let OptContent::Text(t) = emoji_content(Some(long.as_bytes()), None) else {
            panic!("text");
        };
        assert!(t.truncated);
        assert_eq!(t.text.expose().len(), 64);
        assert_eq!(
            emoji_content(None, Some(1)),
            OptContent::Missing(ValueState::Null)
        );
    }
}
