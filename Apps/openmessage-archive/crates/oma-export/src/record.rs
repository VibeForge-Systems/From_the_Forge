//! The canonical export record (M6 spec §4.2, D123, D142).
//!
//! One record shape per entity, rendered by every writer; nothing reads
//! the archive by another route. These are **dedicated `oma-export`
//! types**, never serde-derived archive types, so a change to
//! `AssembledMessage` cannot change the export format by accident. JSON is
//! the records' serde form (camelCase keys), so the JSON schema of slice
//! 6f describes exactly these structs.
//!
//! - **Codes** are [`Code`]s (`{token, code}`); a number this build does
//!   not know keeps its number.
//! - **Absence has explicit states** (C2.4): [`Included`] (`notIncluded`
//!   by an option, with counts), [`Determinable`] (`notDeterminable`, with
//!   a reason), a text's `missing` state, and the attachment outcome
//!   `notRequested`. Every field is always present; `null` stands for a
//!   value the archive recorded as absent (the `state` beside it says
//!   why) or for a fact that does not apply to this item (no
//!   association, no reply, no event), never for "not exported".
//! - **Raw integers** that can exceed 2^53 − 1 (times, `schedule_type`,
//!   sizes, archive ids under D141) are decimal strings; ordinals are
//!   numbers.
//! - **References outside the scope** carry no id ([`MessageRef`]
//!   `notInExport`).
//! - **Content** is held as [`Content`], whose `Debug` prints a length
//!   only (I6): a record can be debug-printed without leaking text.
//! - **Read, delivery and sent state** have no field (D94); a test walks
//!   every serialised record for the banned keys.

use serde::Serialize;

use crate::codes::Code;
use crate::ids::Ordinal;

/// Content text (a body, a name, an address). `Debug` prints its length
/// only; serialised as the string.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Content(String);

impl Content {
    pub fn new(s: String) -> Content {
        Content(s)
    }

    /// The text. Never log it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Content {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Content({} bytes)", self.0.len())
    }
}

/// A raw integer, an archive id or a stored number as a decimal string
/// (spec §5.1: values that can exceed 2^53 − 1 are strings). `Debug`
/// prints the digit count only: ids are never logged (I6).
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Decimal(String);

impl Decimal {
    /// An integer, in decimal.
    pub fn of(v: i64) -> Decimal {
        Decimal(v.to_string())
    }

    /// A real number as stored, in Rust's shortest round-trip form
    /// (`1.5`, `inf`).
    pub fn real(v: f64) -> Decimal {
        Decimal(format!("{v:?}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Decimal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Decimal({} chars)", self.0.len())
    }
}

/// An option of the plan that can leave a part of a record out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PlanOption {
    History,
    Reactions,
    Events,
    Identifiers,
}

/// No counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct NoCounts {}

/// A part of the record that an option may leave out: never `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Included<T, C = NoCounts> {
    /// Left out by `option`; `counts` says how much was left out.
    NotIncluded {
        option: PlanOption,
        counts: C,
    },
    Included {
        value: T,
    },
}

/// Why a fact cannot be determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NotDeterminableReason {
    /// The message has no unique identifier in its source, so nothing
    /// can name it (replies cannot be linked to it).
    Unkeyed,
}

/// A fact that may not be determinable: never `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Determinable<T> {
    NotDeterminable { reason: NotDeterminableReason },
    Determined { value: T },
}

/// A content value as recorded, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextValue {
    /// `null` when the archive holds no value; `missing` says why.
    pub text: Option<Content>,
    /// A `valueState` code when there is no value.
    pub missing: Option<Code>,
    /// A byte that was not UTF-8 was replaced with U+FFFD.
    pub lossy: bool,
    /// The source stored the value as bytes in a text column.
    pub stored_as_blob: bool,
    pub bidi_controls: bool,
    pub invisible: bool,
}

/// A raw integer column as recorded: a decimal string, or why there is
/// none. Its meaning is not claimed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawIntRecord {
    pub raw: Option<Decimal>,
    pub state: Option<Code>,
}

/// A fact derived from a column's presence, with the column's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateBoolRecord {
    pub value: Option<bool>,
    pub state: Option<Code>,
    /// Always `true`: the value is derived from the column's length.
    pub derived: bool,
}

/// The rule that derives a time's unit (`supported-formats.md`, the
/// magnitude windows).
pub const UNIT_RULE: &str = "magnitudeWindows";

/// A time exported raw only (its unit is **UNVERIFIED** for these
/// columns): never localised.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawTimeRecord {
    pub raw: Option<Decimal>,
    pub state: Option<Code>,
    /// Derived ([`UNIT_RULE`]); `null` without a value.
    pub unit: Option<Code>,
    pub unit_rule: &'static str,
}

/// A message's recorded time, placed in the export's zone (spec §4.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeRecord {
    pub raw: Option<Decimal>,
    pub state: Option<Code>,
    pub unit: Option<Code>,
    pub unit_rule: &'static str,
    /// The instant in UTC (`…Z`), with a fraction only for a nanosecond
    /// unit; `null` when the unit gives no instant.
    pub utc: Option<String>,
    /// The instant in the export's zone with its offset; `null` without an
    /// instant or outside the zone window (then `utc` is what is shown).
    pub local: Option<String>,
    pub offset_seconds: Option<i32>,
    /// Outside the zone table's window: written in UTC and counted.
    pub outside_zone_window: bool,
    /// Dated before 2007-01-01 (derived).
    pub before2007: bool,
    /// A scheduled message: the time is the planned send time.
    pub planned: bool,
    /// The instant, Unix nanoseconds UTC, for the human-readable writers
    /// (day headings, their own time format). Not serialised: `utc` and
    /// `local` are the JSON form.
    #[serde(skip)]
    pub utc_ns: Option<i64>,
}

/// A message reference: in this export with its ordinal, or not, with no
/// id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "scope", rename_all = "camelCase")]
pub enum MessageRefRecord {
    InExport { n: Ordinal },
    NotInExport,
}

/// A conversation reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "scope", rename_all = "camelCase")]
pub enum ConversationRefRecord {
    InExport {
        n: Ordinal,
    },
    NotInExport,
    /// The source named no conversation that resolved in the archive.
    NotRecorded,
}

/// The backup set a direction value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RecordedByRecord {
    /// One lineage recorded the message.
    SingleLineage,
    /// More than one did: the lineage and the import session whose backup
    /// set the value (export-local ordinals).
    Lineage { lineage: Ordinal, session: Ordinal },
}

/// `is_from_me`, with four tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectionRecord {
    /// A `direction` code.
    pub token: Code,
    /// For `notRecorded`: the column's state.
    pub state: Option<Code>,
    /// For `unknownValue`: the recorded value.
    pub raw: Option<Decimal>,
    pub recorded_by: RecordedByRecord,
}

/// `is_from_me` of a reaction row, with four tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawDirectionRecord {
    /// A `direction` code.
    pub token: Code,
    pub state: Option<Code>,
    pub raw: Option<Decimal>,
}

/// The sender, as recorded; never inferred from a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SenderRecord {
    /// A `senderReason` code.
    pub reason: Code,
    /// The handle's ordinal, for `resolved` only.
    pub handle: Option<Ordinal>,
}

/// One segment of a body (spec §4.2, C2.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentRecord {
    /// A `segmentKind` code.
    pub kind: Code,
    /// The text of a text segment.
    pub text: Option<Content>,
    /// A `maskKind` code for a withheld segment.
    pub withheld: Option<Code>,
    /// The inline attachment's record ordinal.
    pub attachment: Option<Ordinal>,
    pub transcription: bool,
    pub mention: bool,
    pub url: bool,
    pub url_hidden: bool,
    /// A one-time code shown because the export reveals codes.
    pub one_time_code: bool,
    /// `style` codes.
    pub styles: Vec<Code>,
    /// UTF-16 offset and length in the exported text: the concatenation
    /// of the text segments. Every other segment has length 0, so a
    /// withheld code's length never shows (spec §3.7).
    pub utf16_start: u32,
    pub utf16_len: u32,
}

/// A message's current body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BodyRecord {
    #[serde(rename_all = "camelCase")]
    Text {
        segments: Vec<SegmentRecord>,
        /// A `formatting` code.
        formatting: Code,
        /// UTF-16 length of the exported text.
        utf16_len: u32,
        bidi_controls: bool,
        invisible: bool,
        lossy: bool,
        stored_as_blob: bool,
    },
    /// No text; `reason` is a `noTextReason` code.
    NoText { reason: Code },
}

/// How the current body was decoded (codes as stored).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BodyFactsRecord {
    /// `false` when the message has no current body row; every code below
    /// is then `null`.
    pub body_row: bool,
    pub display_source: Option<Code>,
    pub display_none_reason: Option<Code>,
    pub attributed_outcome: Option<Code>,
    pub attributed_error: Option<Code>,
    pub run_issue: Option<Code>,
    /// The plain `text` column's state.
    pub text_raw_state: Option<Code>,
    /// The two stored text fields differ; `null` when not comparable.
    pub sources_differ: Option<bool>,
}

/// One piece of a masked text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PieceRecord {
    /// A `segmentKind` code: `text` or `withheld`.
    pub kind: Code,
    pub text: Option<Content>,
    pub withheld: Option<Code>,
}

/// The plain-column alternative text (spec §3.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum AltTextRecord {
    /// The two fields agree, cannot be compared, or there is no body.
    None,
    Text {
        /// An `altTextSource` code.
        source: Code,
        text: TextValue,
    },
    /// An `altTextWithheld` code, and the `withheldReason` when it is
    /// earlier content.
    Withheld { why: Code, reason: Option<Code> },
}

/// Edits and unsends (codes 6 and 7 never say unsent; "Unsent" covers
/// only the listed parts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditRecord {
    /// An `editLabel` code; `null` without a current body row.
    pub label: Option<Code>,
    pub edited_parts: Vec<Decimal>,
    pub unsent_parts: Vec<Decimal>,
    pub unsent_whole: bool,
    pub part_count: Option<u32>,
    pub stored_versions: u32,
}

/// Earlier body text (D115).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum EarlierRecord {
    None,
    /// Earlier text exists and is not withheld (see `history`).
    Available,
    /// A `withheldReason` code.
    Withheld {
        reason: Code,
    },
    /// Revealed by the export's opt-in: the newest earlier version with
    /// text, and the session whose backup first held it.
    Revealed {
        reason: Code,
        pieces: Vec<PieceRecord>,
        session: Ordinal,
    },
}

/// How much history there is (the versions follow as their own records
/// when included).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryCounts {
    pub captured_versions: u64,
    pub phone_entries: u64,
}

/// A derived label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelRecord {
    /// A `messageLabel` code.
    pub kind: Code,
    /// `scheduleType`: the recorded value.
    pub value: Option<Decimal>,
    /// `notInLatest`: the lineage's ordinal.
    pub lineage: Option<Ordinal>,
    pub derived: bool,
}

/// One Recently Deleted listing, raw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentlyDeletedRecord {
    /// The import session whose backup listed it.
    pub session: Ordinal,
    pub conversation: ConversationRefRecord,
    /// Later completed sessions of the same lineage that no longer list
    /// it.
    pub not_listed_later: Vec<Ordinal>,
    pub deleted: RawTimeRecord,
    pub ck_sync_state: RawIntRecord,
}

/// The association of a row that is not a plain message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssociationRecord {
    /// `false` for `assoc_present = 2`: whether it is an association is
    /// not known.
    pub determinable: bool,
    /// An `assocKind` code.
    pub kind: Option<Code>,
    /// An `assocTargetShape` code.
    pub shape: Option<Code>,
    /// An `assocVisibility` code.
    pub visibility: Code,
    /// The target, when it resolved to one message.
    pub target: Option<MessageRefRecord>,
    pub target_part: Option<Decimal>,
    pub emoji: Option<TextValue>,
}

/// One recorded reaction row on this message, in scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReactionRowRecord {
    pub n: Ordinal,
    /// An `assocKind` code.
    pub kind: Code,
    pub adds: bool,
    pub emoji: Option<TextValue>,
    pub direction: RawDirectionRecord,
    pub handle: Option<Ordinal>,
    pub date: TimeRecord,
    pub target_part: Option<Decimal>,
}

/// Who a chip's rows came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChipSenderRecord {
    /// A `chipSender` code.
    pub kind: Code,
    pub handle: Option<Ordinal>,
}

/// A reaction chip, derived (D93).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChipRecord {
    /// A `reactionKind` code.
    pub kind: Code,
    pub emoji: Option<TextValue>,
    pub sender: ChipSenderRecord,
    /// A `chipState` code.
    pub state: Code,
    /// The rows in this export.
    pub rows: Vec<Ordinal>,
    /// The chip counts rows outside this export.
    pub includes_rows_outside_scope: bool,
    pub derived: bool,
}

/// The rule the chips are derived by.
pub const CHIP_RULE: &str = "d93LatestDatedRow";

/// The reactions recorded on this message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReactionsRecord {
    /// Raw rows, only those in this export.
    pub rows: Vec<ReactionRowRecord>,
    pub chips: Vec<ChipRecord>,
    pub rule: &'static str,
    /// Removals of an emoji reaction whose emoji was not recorded: never
    /// paired.
    pub unpaired: Vec<MessageRefRecord>,
}

/// How many reaction rows an option left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReactionCounts {
    pub rows: u64,
}

/// A reply's reference to its originator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplyRecord {
    /// A `replyResolution` code.
    pub resolution: Code,
    pub originator: Option<MessageRefRecord>,
}

/// Replies to this message, split by whether they are in this export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplyCountRecord {
    pub in_export: u64,
    pub elsewhere: u64,
}

/// A conversation event or another non-message item. No roles are
/// asserted: the handles are listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventRecord {
    /// An `itemKind` code.
    pub kind: Code,
    pub handles: Vec<Ordinal>,
    pub title: Option<TextValue>,
}

/// One value of a history column other than the current one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ValueHistoryRecord {
    #[serde(rename_all = "camelCase")]
    Value {
        /// A `historyColumn` code.
        column: Code,
        value: TextValue,
        first_session: Ordinal,
    },
    /// Withheld with the earlier content (spec §3.8).
    Withheld { column: Code, reason: Code },
}

/// The derivation versions behind the derived values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivationRecord {
    pub derived_by: i64,
    pub decoder_version: Option<i64>,
}

/// The message columns no viewer shows, raw (meaning not claimed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawColumnsRecord {
    pub service: TextValue,
    pub item_type: RawIntRecord,
    pub group_action_type: RawIntRecord,
    pub is_audio_message: RawIntRecord,
    pub cache_has_attachments: RawIntRecord,
    pub share_status: RawIntRecord,
    pub share_direction: RawIntRecord,
    pub assoc_type: RawIntRecord,
    pub assoc_range_location: RawIntRecord,
    pub assoc_range_length: RawIntRecord,
    pub thread_originator_part: TextValue,
    pub part_count: RawIntRecord,
    pub schedule_state: RawIntRecord,
    pub filter_action: RawIntRecord,
    pub filter_sub_action: RawIntRecord,
    pub was_downgraded: RawIntRecord,
}

/// A message's source identifiers (D141 opt-in only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSourceRecord {
    /// The archive row id, a decimal string.
    pub archive_id: Decimal,
    pub guid: TextValue,
    pub assoc_guid: TextValue,
    pub thread_originator_guid: TextValue,
    pub reply_to_guid: TextValue,
}

/// The per-conversation-file part of an item (spec §4.2): the only part
/// that differs between conversation files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemContext {
    /// `null`: the message is in no conversation.
    pub conversation: Option<Ordinal>,
    pub also_in: AlsoIn,
    /// A reaction row folded into a target (structured formats).
    pub folded_into: Option<Ordinal>,
    /// Whether the reply's originator is in this conversation; `null`
    /// without a resolved originator or a conversation.
    pub reply_in_conversation: Option<bool>,
    /// Whether the association's target is in this conversation.
    pub target_in_conversation: Option<bool>,
    /// `chat_message.join_date` of this conversation's row, raw (unit
    /// **UNVERIFIED**); `null` without a conversation.
    pub join_date: Option<RawTimeRecord>,
    /// The recorded time equals a neighbour's: the order is import order,
    /// not a recorded fact (spec §4.10).
    pub order_tie: bool,
}

/// The other conversations a message is a timeline item of.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlsoIn {
    pub in_export: Vec<Ordinal>,
    /// Conversations not in this export (a count, no ids).
    pub elsewhere: u64,
}

/// One timeline item (spec §4.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemRecord {
    pub n: Ordinal,
    pub source: Included<MessageSourceRecord>,
    /// An `itemKind` code.
    pub item: Code,
    pub direction: DirectionRecord,
    pub sender: SenderRecord,
    /// A `serviceClass` code; `null` when the source recorded none.
    pub service: Option<Code>,
    pub date: TimeRecord,
    pub date_edited: RawTimeRecord,
    pub date_retracted: RawTimeRecord,
    pub subject: TextValue,
    /// `destination_caller_id`: "Account (meaning unverified)", the user's
    /// own address.
    pub account: TextValue,
    pub body: BodyRecord,
    pub alternative_text: AltTextRecord,
    pub body_facts: BodyFactsRecord,
    pub edit: EditRecord,
    pub earlier_text: EarlierRecord,
    pub history: Included<HistoryCounts, HistoryCounts>,
    pub labels: Vec<LabelRecord>,
    pub recently_deleted: Vec<RecentlyDeletedRecord>,
    pub association: Option<AssociationRecord>,
    pub reactions: Included<ReactionsRecord, ReactionCounts>,
    pub reply: Option<ReplyRecord>,
    pub reply_count: Determinable<ReplyCountRecord>,
    pub event: Option<EventRecord>,
    pub attachments: Vec<AttachmentRecord>,
    pub schedule_type: RawIntRecord,
    pub app_content: StateBoolRecord,
    pub effect: StateBoolRecord,
    pub value_history: Vec<ValueHistoryRecord>,
    pub raw: RawColumnsRecord,
    pub derivation: DerivationRecord,
    pub context: ItemContext,
}

/// One import's observation of an attachment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationRecord {
    pub session: Ordinal,
    /// `lookup` and `copy` codes (unknown numbers kept).
    pub lookup: Code,
    pub copy: Code,
    pub path_class: Option<Code>,
    pub size_matched_manifest: Option<bool>,
    /// The stored file holds this session's bytes.
    pub holds_stored_file: bool,
}

/// An attachment's card (M5 §5.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardRecord {
    /// A `cardState` code.
    pub state: Code,
    /// For `file`: shown from an older backup (session ordinals).
    pub older_backup_held_by: Option<Ordinal>,
    pub older_backup_newest: Option<Ordinal>,
    /// For `locationUnusable`: the `lookup` code.
    pub location_lookup: Option<Code>,
    /// For `unavailable`: the raw lookup and copy codes.
    pub unavailable_lookup: Option<Decimal>,
    pub unavailable_copy: Option<Decimal>,
}

/// A full verify's historical report on the stored file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyRecord {
    /// A `blobVerifyState` code (unknown numbers kept).
    pub state: Code,
    pub may_be_stale: bool,
}

/// What the export did with an attachment's file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRecord {
    /// An `attachmentOutcome` code.
    pub outcome: Code,
    /// The file ordinal it shares, when a file was written.
    pub file: Option<Ordinal>,
    /// The file's place in the bundle (`attachments/<shard>/<name>`,
    /// `/`-separated), when a file was written. Its name part comes from
    /// `transfer_name`, so it is content.
    pub path: Option<Content>,
    /// A `sniffedKind` code: what the copied bytes are (the first 4 KiB of
    /// the copy stream, never the declared type); `null` when this export
    /// did not read the file.
    pub sniffed: Option<Code>,
}

/// An attachment's source identifiers (D141 opt-in only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentSourceRecord {
    pub archive_id: Decimal,
    pub guid: TextValue,
    pub original_guid: TextValue,
}

/// One attachment record (spec §4.2, C2.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRecord {
    pub n: Ordinal,
    pub source: Included<AttachmentSourceRecord>,
    pub file: FileRecord,
    /// An `attachmentAccess` code (attachments of unsent parts, §3.8).
    pub access: Code,
    /// The `withheldReason` for a withheld or revealed attachment.
    pub access_reason: Option<Code>,
    /// `transfer_name`, as content (uncapped, unsanitised).
    pub name: TextValue,
    pub mime_type: TextValue,
    pub uti: TextValue,
    pub total_bytes: RawIntRecord,
    /// The stored file's size; `null` without a stored file.
    pub stored_bytes: Option<Decimal>,
    pub is_sticker: RawIntRecord,
    pub hide_attachment: RawIntRecord,
    pub is_outgoing: RawIntRecord,
    pub transfer_state: RawIntRecord,
    pub emoji_description: TextValue,
    pub created: RawTimeRecord,
    pub start: RawTimeRecord,
    /// Placed inline by a matching U+FFFC.
    pub placed_inline: bool,
    pub card: CardRecord,
    pub held_by: Option<Ordinal>,
    pub observations: Vec<ObservationRecord>,
    pub verify: Option<VerifyRecord>,
    pub history: Vec<ValueHistoryRecord>,
}

/// A conversation's source identifiers (D141 opt-in only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSourceRecord {
    pub archive_id: Decimal,
    pub guid: TextValue,
    pub room_name: TextValue,
    pub group_id: TextValue,
}

/// One conversation (spec §4.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRecord {
    pub n: Ordinal,
    pub source: Included<ConversationSourceRecord>,
    /// `display_name`.
    pub name: TextValue,
    pub chat_identifier: TextValue,
    pub service_name: TextValue,
    pub service: Option<Code>,
    pub style: RawIntRecord,
    pub is_filtered: RawIntRecord,
    pub is_archived: RawIntRecord,
    pub is_blackholed: RawIntRecord,
    pub is_pending_review: RawIntRecord,
    pub is_recovered: RawIntRecord,
    pub properties_bytes: RawIntRecord,
    /// Every recorded participant, those who left included.
    pub participants: Vec<Ordinal>,
    pub name_history: Vec<ValueHistoryRecord>,
}

/// A handle's source identifiers (D141 opt-in only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandleSourceRecord {
    pub archive_id: Decimal,
    pub uncanonicalized_id: TextValue,
    pub person_centric_id: TextValue,
}

/// One handle (spec §4.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HandleRecord {
    pub n: Ordinal,
    pub source: Included<HandleSourceRecord>,
    pub address: TextValue,
    /// `service`, raw.
    pub service: TextValue,
    pub service_class: Option<Code>,
    pub country: TextValue,
}

/// A phone edit entry's recorded time: never localised ("time as recorded
/// (unit assumed)").
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditTimeRecord {
    /// A `summaryTimeKind` code.
    pub kind: Option<Code>,
    /// `integer` or `real`, as stored in the `ANY` column.
    pub raw_type: Option<&'static str>,
    pub raw: Option<Decimal>,
}

/// The text of one phone edit entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum EditEntryTextRecord {
    Text {
        pieces: Vec<PieceRecord>,
    },
    Withheld {
        reason: Code,
    },
    NoBody,
    #[serde(rename_all = "camelCase")]
    Failed {
        decode_error: Code,
    },
    NotMatched,
}

/// One stored phone edit entry.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditEntryRecord {
    pub part: Decimal,
    pub ordinal: Decimal,
    pub at: EditTimeRecord,
    pub text: EditEntryTextRecord,
}

/// The text of one stored body version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum VersionTextRecord {
    Text { pieces: Vec<PieceRecord> },
    Withheld { reason: Code },
    NoText { reason: Code },
}

/// One stored body version of a message (history included).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionRecord {
    /// The message's ordinal.
    pub message: Ordinal,
    pub current: bool,
    pub first_session: Ordinal,
    pub last_session: Ordinal,
    pub text: VersionTextRecord,
    pub text_unchanged: bool,
    pub edit_label: Option<Code>,
    pub edits: Vec<EditEntryRecord>,
    /// An `editTextsOutcome` code.
    pub edit_texts: Code,
}
