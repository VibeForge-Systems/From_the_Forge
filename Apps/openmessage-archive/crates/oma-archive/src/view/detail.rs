//! Message detail, the reaction history, `message_text`, value history and
//! the attachment facts with their card (M5 spec §3.6, §3.7, §3.8, §3.9,
//! §5.9, §7.3 "Message detail adds").
//!
//! - **Edit history is decoded at read time** (owner decision Q3a, §3.7):
//!   [`oma_messages::edit_texts`] reads `ec[*].t` from the stored
//!   `summary_raw` of each body version that has edit entries. Nothing is
//!   stored or indexed. Entries are matched to `message_edit_entry` rows by
//!   `(part, ordinal)`, in stored order ("oldest first" is **UNVERIFIED**);
//!   on any mismatch every entry says the text could not be matched.
//! - **Masking and retraction (D115, owner decision Q3b)** are applied here
//!   as in the thread pages: one-time codes are withheld unless the request
//!   reveals them, and text from before an unsend (or from before the
//!   current version lost its text) is withheld, earlier versions and
//!   decoded edit text alike, unless the request carries
//!   [`Reveal::earlier_text`].
//! - **Body versions are not edits**: a version is one stored raw state, so
//!   a version whose text equals the previous one says so
//!   ([`BodyVersion::text_unchanged`]).
//! - **The card (§5.9)** is decided from the highest-ranked observation's
//!   lookup code first, then its copy code. A present blob always shows
//!   the file; when the newest observation found no file, the card says
//!   the file is from an older backup. What a present file is (an image, a
//!   playable file, a file card or unrecognised content) is decided from its
//!   bytes by `ArchiveReader::inspect_blob` (`crate::media`, slice 5d),
//!   which the shell calls for the attachments it shows.
//! - The unkeyed identifier probe ("unkeyed messages share this
//!   identifier") scans `message` under its own `Detail` phase; a trip
//!   reads [`IdentifierProbe::NotChecked`], never an error.
//! - Read, delivery and sent state are never read (D94).

use std::collections::BTreeSet;
use std::time::Instant;

use oma_messages::{edit_texts, EditBody, TsBudget};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension};

use super::assemble::{
    self, bytes_of, chip_sender, emoji_content, recorded_time, AssembledMessage, BodyView, Caps,
    ChipSender, Ctx, EarlierText, NoText, RecordedTime, ReplyRef, Reveal, WithheldReason,
};
use super::conversations::{masked_preview, ONE_TIME_CODE_BIT, SQL_FLAGGED_PARTS};
use super::text::{mask_utf16, utf16_len, ContentText, MaskKind, OptContent, TextPiece};
use super::{
    AssocVisibility, AttachmentId, BodyId, CappedCount, ChatId, MessageId, BODY_PAGE_UTF16,
    IDENTIFIER_UTF16, PREVIEW_UTF16,
};
use crate::conn::{Phase, ViewBudget};
use crate::error::ArchiveError;
use crate::import::ts_code;
use crate::reader::{ArchiveReader, BlobId};
use crate::schema::codes::{
    AssocKindCode, AttributedOutcome, BlobVerifyState, CopyCode, DisplayNoneReason, DisplaySource,
    EditLabelCode, Entity, LookupCode, PathClassCode, SummaryTimeKind, TsErrorCode, ValueState,
};

/// Reaction rows per page of [`ArchiveReader::reaction_history`].
pub const REACTION_HISTORY_PAGE: usize = 200;
/// The most reaction rows one target's history reads (sorted in Rust);
/// beyond, the history says it is incomplete.
pub const REACTION_HISTORY_LIMIT: usize = 10_000;
/// UTF-16 units per [`ArchiveReader::message_text`] chunk.
pub const TEXT_CHUNK_UTF16: u32 = 32_768;
/// Body versions a detail lists (oldest rank first); beyond, a count.
pub const DETAIL_VERSIONS: usize = 50;
/// Edit entries a detail lists per body version.
pub const DETAIL_EDIT_ENTRIES: usize = 200;
/// Chats a detail lists the flags of.
pub const DETAIL_CHATS: usize = 100;

/// `ArchiveColumn` codes of the history columns the viewer shows (§3.6,
/// D82); checked against `ARCHIVE_COLUMNS` by a test.
const COL_CHAT_DISPLAY_NAME: i64 = 205;
const COL_DESTINATION_CALLER_ID: i64 = 305;
const COL_SUBJECT: i64 = 318;
const COL_GROUP_TITLE: i64 = 321;
const COL_TRANSFER_NAME: i64 = 406;
const COL_EMOJI_DESCRIPTION: i64 = 414;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A raw integer column as recorded: its value, or why there is none.
/// Its meaning is not claimed (`style`, `is_filtered`, `is_archived`,
/// `hide_attachment`: "meaning unverified").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RawValue {
    pub value: Option<i64>,
    /// `None` with a value.
    pub state: Option<ValueState>,
}

impl RawValue {
    fn of(value: Option<i64>, st: Option<i64>) -> RawValue {
        RawValue {
            value: value.filter(|_| st.is_none()),
            state: st.map(|s| ValueState::from_code(s).unwrap_or(ValueState::Unreadable)),
        }
    }
}

/// The history columns the viewer shows ("Other values recorded by other
/// backups", §3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum HistoryColumn {
    Subject,
    GroupTitle,
    /// `destination_caller_id`: "Account (meaning unverified)".
    DestinationCallerId,
    ChatDisplayName,
    TransferName,
    EmojiDescription,
}

impl HistoryColumn {
    fn of(code: i64) -> Option<HistoryColumn> {
        Some(match code {
            COL_SUBJECT => HistoryColumn::Subject,
            COL_GROUP_TITLE => HistoryColumn::GroupTitle,
            COL_DESTINATION_CALLER_ID => HistoryColumn::DestinationCallerId,
            COL_CHAT_DISPLAY_NAME => HistoryColumn::ChatDisplayName,
            COL_TRANSFER_NAME => HistoryColumn::TransferName,
            COL_EMOJI_DESCRIPTION => HistoryColumn::EmojiDescription,
            _ => return None,
        })
    }

    fn cap(self) -> u32 {
        match self {
            HistoryColumn::Subject => PREVIEW_UTF16,
            _ => IDENTIFIER_UTF16,
        }
    }
}

/// A value another backup recorded for a history column, other than the
/// current one. The value shown follows the highest-ranked backup.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HistoryValue {
    pub column: HistoryColumn,
    /// `Missing(Null)` when that backup recorded SQL NULL.
    pub value: OptContent,
    /// The session whose backup first recorded it.
    pub first_session: i64,
}

/// The text of one body version.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum VersionText {
    /// The version's text, capped at [`BODY_PAGE_UTF16`], one-time codes
    /// withheld unless revealed.
    Text(Vec<TextPiece>),
    /// Earlier text withheld (D115) until "Show earlier text".
    Withheld(WithheldReason),
    /// The version has no text, and why.
    NoText(NoText),
}

/// How an edit entry's time `d` was stored: "time as recorded (unit
/// assumed)" (§3.7; the units of `ec[*].d` are **UNVERIFIED**).
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct EditTime {
    /// `None` when the entry has no `d`.
    pub kind: Option<SummaryTimeKind>,
    pub raw: Option<RawNumber>,
    /// The instant the import derived, when it could.
    pub utc_ns: Option<i64>,
}

/// A raw number as stored in an `ANY` column.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum RawNumber {
    Integer(i64),
    Real(f64),
}

/// The decoded text of one edit entry (`ec[part][ordinal].t`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EditEntryText {
    /// Decoded, one-time codes withheld unless revealed.
    Text(Vec<TextPiece>),
    /// Withheld with the earlier text (D115).
    Withheld(WithheldReason),
    /// The entry records no text.
    NoBody,
    /// `t` did not decode (the `TsErrorCode` sentence).
    Failed(TsErrorCode),
    /// The decoded entries could not be matched to the stored ones, or the
    /// summary could not be read again: "edit text could not be matched".
    NotMatched,
}

/// One stored `message_edit_entry` row, with its text decoded at read
/// time.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct EditEntryView {
    pub part: i64,
    /// The stored position within the part.
    pub ordinal: i64,
    pub at: EditTime,
    pub text: EditEntryText,
}

/// Why a version's edit texts were not decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EditTextsOutcome {
    /// No entry records a text.
    NotNeeded,
    /// Decoded and matched.
    Decoded,
    /// Decoded, but the entries do not align with the stored ones.
    Unmatched,
    /// The stored summary is missing, could not be read, or has no usable
    /// `ec`.
    SummaryUnreadable,
}

/// One stored body version ("Versions this archive captured", §3.7).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct BodyVersion {
    pub body: BodyId,
    pub current: bool,
    /// The session whose backup first held it (the earliest by rank).
    pub first_session: i64,
    pub last_session: i64,
    pub text: VersionText,
    /// The same displayed text as the version before it: "Stored data
    /// changed; text unchanged".
    pub text_unchanged: bool,
    /// `None` for a code this build does not know.
    pub edit_label: Option<EditLabelCode>,
    /// "Versions recorded on the phone", in stored order.
    pub edits: Vec<EditEntryView>,
    /// Entries beyond [`DETAIL_EDIT_ENTRIES`].
    pub more_edits: u64,
    pub edit_texts: EditTextsOutcome,
}

/// The timestamps the detail shows raw: `date`, `date_edited` and
/// `date_retracted` only (D94: never read, delivered or played).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DetailTimes {
    pub date: RecordedTime,
    pub edited: RecordedTime,
    pub retracted: RecordedTime,
}

/// One conversation the message is in, with its raw flags ("meaning
/// unverified") and its name history.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChatFlags {
    pub chat: ChatId,
    pub style: RawValue,
    pub is_filtered: RawValue,
    pub is_archived: RawValue,
    pub history: Vec<HistoryValue>,
}

/// What the unkeyed identifier probe found for an identifier that no
/// keyed message has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IdentifierProbe {
    /// No message in this archive has the identifier.
    NotInArchive,
    /// Messages without a unique identifier in their source share it.
    SharedByUnkeyed,
    /// The probe went over its budget.
    NotChecked,
}

/// What an attachment's card shows (§5.9), before the sniff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CardState {
    /// A blob is present: the file. Its sniff
    /// ([`crate::ArchiveReader::inspect_blob`], [`crate::MediaClass`])
    /// decides between a rendition, the Play card, a file card and
    /// "Unrecognised content".
    File {
        /// The highest-ranked observation found no file: "Showing the file
        /// from the backup of D (`held_by`); not found in the backup of D2
        /// (`newest`)".
        older_backup: Option<OlderBackup>,
    },
    /// "No copy of this file was attempted by any import".
    NoObservation,
    /// `NotListedAtSourcedKey`: "This file was not in the backup of D".
    NotInBackup,
    /// `NotFoundAtAssumedKey`: "Not found where this app looked; it may be
    /// stored elsewhere in the backup".
    NotFoundAtAssumedKey,
    /// `NotLookedUp` or `NotRequested`: "Not copied: attachment copy was
    /// off for that import".
    NotCopied,
    /// `NotAttemptedClass`: "This kind of file location is not looked up
    /// by this version".
    ClassNotLookedUp,
    /// `NotAttempted*`: "The file's recorded location could not be used".
    LocationUnusable(LookupCode),
    /// "Listed in the backup but could not be decrypted".
    NotDecrypted,
    /// "Listed in the backup but could not be read".
    CouldNotBeRead,
    /// "Too large to copy".
    TooLarge,
    /// "Not available (lookup code n, copy code m)".
    Unavailable { lookup: i64, copy: i64 },
}

/// Where a file shown from an older backup came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct OlderBackup {
    /// The session whose bytes the blob holds.
    pub held_by: i64,
    /// The highest-ranked session, which found no file.
    pub newest: i64,
}

/// One import's observation of an attachment, as codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Observation {
    pub session: i64,
    /// `None` for a code this build does not know (`lookup_raw` holds it).
    pub lookup: Option<LookupCode>,
    pub lookup_raw: i64,
    pub copy: Option<CopyCode>,
    pub copy_raw: i64,
    pub path_class: Option<PathClassCode>,
    /// The blob read in that session, if any.
    pub blob: Option<BlobId>,
    pub size_matched_manifest: Option<bool>,
}

/// The current blob of an attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BlobFacts {
    pub id: BlobId,
    /// `blob.byte_len`, stored.
    pub stored_bytes: i64,
    /// The session whose bytes it holds: the highest-ranked observation
    /// that read it, else the session that first stored it.
    pub held_by: i64,
}

/// A full verify's report on the current blob (§5.9): historical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct VerifyFacts {
    /// `None` for a code this build does not know (`raw` holds it).
    pub state: Option<BlobVerifyState>,
    pub raw: i64,
    pub verified_at_ns: Option<i64>,
    /// A later observation replaced the file: "this result may be out of
    /// date; run Verify".
    pub may_be_stale: bool,
}

/// Everything recorded about one attachment (§3.9).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AttachmentFacts {
    pub id: AttachmentId,
    /// `mime_type`, declared (a label only).
    pub declared_type: OptContent,
    /// `uti`, declared.
    pub uti: OptContent,
    /// `transfer_name`, never a path.
    pub name: OptContent,
    /// `total_bytes`, declared.
    pub declared_bytes: RawValue,
    pub sticker: RawValue,
    /// "hide_attachment = 1 (meaning unverified)".
    pub hide_attachment: RawValue,
    /// The Genmoji description.
    pub emoji_description: OptContent,
    pub blob: Option<BlobFacts>,
    /// Ordered by session rank, oldest first.
    pub observations: Vec<Observation>,
    pub card: CardState,
    /// Only for a non-zero `verify_state`.
    pub verify: Option<VerifyFacts>,
    pub history: Vec<HistoryValue>,
}

/// Everything the details panel shows for one message (§7.3).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct MessageDetail {
    /// The message as a page shows it (UI caps), its extras loaded.
    pub message: AssembledMessage,
    /// Body versions, oldest rank first.
    pub versions: Vec<BodyVersion>,
    /// Versions beyond [`DETAIL_VERSIONS`].
    pub more_versions: u64,
    pub times: DetailTimes,
    /// `destination_caller_id`: "Account (meaning unverified)".
    pub account: OptContent,
    pub subject: OptContent,
    /// Other values of the message's history columns.
    pub history: Vec<HistoryValue>,
    /// The conversations the message is in (at most [`DETAIL_CHATS`]).
    pub chats: Vec<ChatFlags>,
    /// At most [`Caps::PAGE`]'s attachments, with their facts and cards.
    pub attachments: Vec<AttachmentFacts>,
    /// For a reply whose originator is not uniquely identified.
    pub reply_probe: Option<IdentifierProbe>,
    /// For a reaction whose target is not uniquely identified.
    pub reaction_target_probe: Option<IdentifierProbe>,
}

/// One recorded reaction row on a target (§3.6: "every add and remove row
/// with its sender, time and `assoc_target_part`").
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReactionRecord {
    pub id: MessageId,
    pub kind: Option<AssocKindCode>,
    /// `true` for 10–17, `false` for 20–27.
    pub adds: bool,
    /// `assoc_emoji`, capped.
    pub emoji: Option<OptContent>,
    pub sender: ChipSender,
    pub time: RecordedTime,
    pub target_part: Option<i64>,
}

/// Where the next page of a reaction history starts: the date key and id
/// of the last row served. Opaque; `Debug` prints nothing.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ReactionCursor {
    key: Vec<u8>,
    id: i64,
}

impl std::fmt::Debug for ReactionCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReactionCursor(..)")
    }
}

impl ReactionCursor {
    /// The last row the cursor follows.
    pub fn after(&self) -> MessageId {
        MessageId::new(self.id)
    }
}

/// The reaction rows recorded on one target, paged by a keyset over
/// (date key, id): a row an import adds between two calls is never served
/// twice, and no row is skipped because another was added before it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReactionHistory {
    pub target: MessageId,
    /// An unkeyed target has no resolvable reactions.
    pub keyed: bool,
    /// This page's rows, ordered by date (undated last), then id.
    pub rows: Vec<ReactionRecord>,
    /// Every row read, capped at [`REACTION_HISTORY_LIMIT`].
    pub total: CappedCount,
    pub has_more: bool,
    /// The cursor of the next page, when there is one.
    pub next: Option<ReactionCursor>,
}

/// What one chunk of a body holds.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChunkText {
    /// The chunk, one-time codes withheld unless revealed.
    Text(Vec<TextPiece>),
    /// The body is earlier text, withheld (D115).
    Withheld(WithheldReason),
    /// The body has no text.
    NoText,
}

/// A chunk of a body's text from [`ArchiveReader::message_text`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TextChunk {
    pub body: BodyId,
    /// The first UTF-16 unit served (clamped, moved off a low surrogate).
    pub from_utf16: u32,
    /// One past the last unit served.
    pub end_utf16: u32,
    pub full_utf16: u32,
    pub text: ChunkText,
}

// ---------------------------------------------------------------------------
// Statements (each pinned in tests/query_plans.rs)
// ---------------------------------------------------------------------------

const SQL_DETAIL_ROW: &str = "SELECT date_raw, date_st, date_unit, date_utc_ns, \
       date_edited_raw, date_edited_st, date_edited_unit, date_edited_utc_ns, \
       date_retracted_raw, date_retracted_st, date_retracted_unit, date_retracted_utc_ns, \
       destination_caller_id, destination_caller_id_st, subject, subject_st, \
       thread_originator_guid, assoc_target_guid \
     FROM message WHERE id = ?1";
const SQL_DETAIL_VERSIONS: &str = "SELECT id, first_rank, first_session_id, last_session_id, \
       display_text, display_source, display_none_reason, attributed_outcome, \
       attributed_error, run_issue, text_raw_st, edit_label \
     FROM message_body WHERE message_id = ?1";
const SQL_EDIT_ENTRIES: &str = "SELECT part, ordinal, at_kind, at_raw, at_utc_ns, body_present \
     FROM message_edit_entry WHERE body_id = ?1 ORDER BY part, ordinal LIMIT ?2";
const SQL_EDIT_ENTRY_COUNT: &str = "SELECT count(*) FROM message_edit_entry WHERE body_id = ?1";
const SQL_SUMMARY_RAW: &str = "SELECT summary_raw FROM message_body WHERE id = ?1";
const SQL_UNKEYED_PROBE: &str = "SELECT count(*) FROM (SELECT 1 FROM message \
     WHERE key_kind = 2 AND guid = ?1 LIMIT 2)";
const SQL_MESSAGE_CHATS: &str = "SELECT c.id, c.style, c.style_st, c.is_filtered, \
       c.is_filtered_st, c.is_archived, c.is_archived_st \
     FROM chat_message cm JOIN chat c ON c.id = cm.chat_id \
     WHERE cm.message_id = ?1 ORDER BY cm.message_id, cm.chat_id LIMIT ?2";
const SQL_VALUE_HISTORY: &str = "SELECT col, value, first_rank, first_session_id \
     FROM value_history WHERE entity = ?1 AND row_id = ?2";
const SQL_ATTACHMENT_IDS: &str = "SELECT attachment_id FROM message_attachment \
     WHERE message_id = ?1 ORDER BY ord_rank, ord_rowid LIMIT ?2";
const SQL_ATTACHMENT_ROW: &str = "SELECT a.mime_type, a.mime_type_st, a.uti, a.uti_st, \
       a.transfer_name, a.transfer_name_st, a.total_bytes, a.total_bytes_st, a.is_sticker, \
       a.is_sticker_st, a.hide_attachment, a.hide_attachment_st, a.emoji_description, \
       a.emoji_description_st, a.current_blob_id, b.byte_len, b.first_session_id, \
       b.verify_state, b.verified_at_ns \
     FROM attachment a LEFT JOIN blob b ON b.id = a.current_blob_id WHERE a.id = ?1";
const SQL_OBSERVATIONS: &str = "SELECT o.session_id, o.path_class, o.lookup, o.copy, \
       o.blob_id, o.size_matched_manifest, s.rank, s.started_at_ns \
     FROM attachment_observation o JOIN import_session s ON s.id = o.session_id \
     WHERE o.attachment_id = ?1";
const SQL_TARGET: &str = "SELECT key_kind, guid FROM message WHERE id = ?1";
const SQL_REACTION_ROWS: &str = "SELECT id, assoc_kind, assoc_emoji, assoc_emoji_st, \
       is_from_me, is_from_me_st, handle_ref, date_raw, date_st, date_unit, date_utc_ns, \
       assoc_target_part, sort_key FROM message \
     WHERE assoc_target_guid = ?1 AND assoc_present = 1 \
       AND (assoc_kind BETWEEN 10 AND 17 OR assoc_kind BETWEEN 20 AND 27) LIMIT ?2";
const SQL_TEXT_BODY: &str = "SELECT b.message_id, b.display_text, b.display_source, \
       b.run_issue, m.current_body_id \
     FROM message_body b JOIN message m ON m.id = b.message_id WHERE b.id = ?1";
const SQL_CURRENT_STATE: &str = "SELECT display_text IS NOT NULL AND display_source IS NOT NULL, \
       edit_label FROM message_body WHERE id = ?1";
pub(crate) const SQL_ANY_UNSENT: &str =
    "SELECT 1 FROM message_unsent_part WHERE body_id = ?1 LIMIT 1";

#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    [
        ("viewer detail row", SQL_DETAIL_ROW),
        ("viewer detail versions", SQL_DETAIL_VERSIONS),
        ("viewer edit entries", SQL_EDIT_ENTRIES),
        ("viewer edit entry count", SQL_EDIT_ENTRY_COUNT),
        ("viewer summary raw", SQL_SUMMARY_RAW),
        ("viewer unkeyed identifier probe", SQL_UNKEYED_PROBE),
        ("viewer message chats", SQL_MESSAGE_CHATS),
        ("viewer value history", SQL_VALUE_HISTORY),
        ("viewer attachment ids", SQL_ATTACHMENT_IDS),
        ("viewer attachment row", SQL_ATTACHMENT_ROW),
        ("viewer attachment observations", SQL_OBSERVATIONS),
        ("viewer reaction target", SQL_TARGET),
        ("viewer reaction history", SQL_REACTION_ROWS),
        ("viewer text body", SQL_TEXT_BODY),
        ("viewer current body state", SQL_CURRENT_STATE),
        ("viewer any unsent part", SQL_ANY_UNSENT),
    ]
    .into_iter()
    .map(|(n, s)| (n, s.to_owned()))
    .collect()
}

// ---------------------------------------------------------------------------
// Shared pieces
// ---------------------------------------------------------------------------

fn u64_of(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

fn limit_of(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Text from a cell (TEXT or BLOB), lossily decoded.
fn text_of(v: ValueRef<'_>) -> Option<String> {
    match v {
        ValueRef::Text(b) | ValueRef::Blob(b) => Some(String::from_utf8_lossy(b).into_owned()),
        _ => None,
    }
}

/// Why earlier text of `m` is withheld (D115), when it is: the message
/// has an unsent part, or its current body has no text (code 6 reads
/// "from an earlier version").
fn withheld_reason(m: &AssembledMessage) -> Option<WithheldReason> {
    match &m.edit.earlier {
        EarlierText::Withheld(r) | EarlierText::Revealed { reason: r, .. } => return Some(*r),
        _ => {}
    }
    if !m.edit.unsent_parts.is_empty() {
        Some(WithheldReason::Unsent)
    } else if !matches!(m.body, BodyView::Text(_)) {
        Some(if m.edit.label == Some(EditLabelCode::DateEditedNoBody) {
            WithheldReason::EarlierVersion
        } else {
            WithheldReason::NoCurrentText
        })
    } else {
        None
    }
}

/// `text`, one-time-code ranges withheld unless `reveal_code`: ranges
/// apply only when the formatting was read cleanly; otherwise a text with
/// a code is withheld whole.
fn masked(
    codes: &[(u32, u32)],
    trusted: bool,
    text: &str,
    cap: u32,
    reveal_code: bool,
) -> Vec<TextPiece> {
    if codes.is_empty() || reveal_code {
        return vec![TextPiece::Text(ContentText::from_str(text, cap))];
    }
    if !trusted {
        return vec![TextPiece::Masked(MaskKind::OneTimeCode)];
    }
    mask_utf16(text, codes, MaskKind::OneTimeCode, cap)
}

/// An earlier edit text, codes masked unless `reveal_code`. When the runs
/// were cut short (`runs_complete` false), the flags of the rest are
/// unknown and a code could be missing from `codes`, so the whole text is
/// withheld whether or not a code was seen (`EditBody::Text`'s contract).
fn edit_text_pieces(
    codes: &[(u32, u32)],
    runs_complete: bool,
    text: &str,
    reveal_code: bool,
) -> Vec<TextPiece> {
    if !runs_complete && !reveal_code {
        return vec![TextPiece::Masked(MaskKind::OneTimeCode)];
    }
    masked(codes, runs_complete, text, BODY_PAGE_UTF16, reveal_code)
}

fn history(c: &Connection, entity: Entity, id: i64) -> rusqlite::Result<Vec<HistoryValue>> {
    let mut st = c.prepare(SQL_VALUE_HISTORY)?;
    let mut rows = st.query(rusqlite::params![entity.code(), id])?;
    let mut out: Vec<(HistoryColumn, Vec<u8>, HistoryValue)> = Vec::new();
    while let Some(r) = rows.next()? {
        let Some(column) = HistoryColumn::of(r.get(0)?) else {
            continue;
        };
        let value = match r.get_ref(1)? {
            ValueRef::Null => OptContent::Missing(ValueState::Null),
            ValueRef::Text(b) | ValueRef::Blob(b) => {
                OptContent::Text(ContentText::from_bytes(b, column.cap()))
            }
            ValueRef::Integer(_) | ValueRef::Real(_) => OptContent::Missing(ValueState::Unreadable),
        };
        let rank: Vec<u8> = r.get(2)?;
        out.push((
            column,
            rank,
            HistoryValue {
                column,
                value,
                first_session: r.get(3)?,
            },
        ));
    }
    out.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    Ok(out.into_iter().map(|(_, _, v)| v).collect())
}

// ---------------------------------------------------------------------------
// Attachments (§3.9, §5.9)
// ---------------------------------------------------------------------------

/// The card of an attachment without a blob, from its newest observation:
/// the lookup code first, then the copy code.
fn card_without_blob(top: Option<&Observation>) -> CardState {
    let Some(o) = top else {
        return CardState::NoObservation;
    };
    use LookupCode as L;
    match o.lookup {
        Some(L::NotLookedUp) => return CardState::NotCopied,
        _ if o.copy == Some(CopyCode::NotRequested) => return CardState::NotCopied,
        Some(L::NotListedAtSourcedKey) => return CardState::NotInBackup,
        Some(L::NotFoundAtAssumedKey) => return CardState::NotFoundAtAssumedKey,
        Some(L::NotAttemptedClass) => return CardState::ClassNotLookedUp,
        Some(
            k @ (L::NotAttemptedNull
            | L::NotAttemptedTooLong
            | L::NotAttemptedUnusualName
            | L::NotAttemptedUnrecognized
            | L::NotAttemptedColumnAbsent
            | L::NotAttemptedUnreadable
            | L::NotAttemptedOther),
        ) => return CardState::LocationUnusable(k),
        Some(L::KeyDoesNotUnwrap | L::ClassKeyUnavailable | L::NoKey | L::NoProtectionClass) => {
            return CardState::NotDecrypted
        }
        Some(
            L::BlobMissing
            | L::BlobNotRegular
            | L::BlobUnreadable
            | L::NotAFile
            | L::EntryInvalid
            | L::Ambiguous
            | L::InvalidEntry,
        ) => return CardState::CouldNotBeRead,
        _ => {}
    }
    match o.copy {
        Some(CopyCode::BlobUnavailable | CopyCode::ReadFailed) => CardState::CouldNotBeRead,
        Some(CopyCode::OverLimit | CopyCode::TooLargeForVolume) => CardState::TooLarge,
        _ => CardState::Unavailable {
            lookup: o.lookup_raw,
            copy: o.copy_raw,
        },
    }
}

type AttachmentRow = (
    OptContent,
    OptContent,
    OptContent,
    RawValue,
    RawValue,
    RawValue,
    OptContent,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

/// The facts and card of attachment `id`; `None` when it does not exist.
pub(crate) fn attachment_facts(
    c: &Connection,
    id: i64,
) -> rusqlite::Result<Option<AttachmentFacts>> {
    let row: Option<AttachmentRow> = c
        .prepare(SQL_ATTACHMENT_ROW)?
        .query_row([id], |r| {
            Ok((
                OptContent::from_cell(r.get_ref(0)?, r.get(1)?, IDENTIFIER_UTF16),
                OptContent::from_cell(r.get_ref(2)?, r.get(3)?, IDENTIFIER_UTF16),
                OptContent::from_cell(r.get_ref(4)?, r.get(5)?, IDENTIFIER_UTF16),
                RawValue::of(r.get(6)?, r.get(7)?),
                RawValue::of(r.get(8)?, r.get(9)?),
                RawValue::of(r.get(10)?, r.get(11)?),
                OptContent::from_cell(r.get_ref(12)?, r.get(13)?, PREVIEW_UTF16),
                r.get(14)?,
                r.get(15)?,
                r.get(16)?,
                r.get(17)?,
                r.get(18)?,
            ))
        })
        .optional()?;
    let Some((ty, uti, name, bytes, sticker, hide, emoji, blob, len, first, vstate, vat)) = row
    else {
        return Ok(None);
    };
    // Observations by session rank, oldest first.
    let mut obs: Vec<(Vec<u8>, i64, Observation)> = {
        let mut st = c.prepare(SQL_OBSERVATIONS)?;
        let rows = st.query_map([id], |r| {
            let lookup: i64 = r.get(2)?;
            let copy: i64 = r.get(3)?;
            let class: i64 = r.get(1)?;
            let matched: Option<i64> = r.get(5)?;
            Ok((
                r.get::<_, Vec<u8>>(6)?,
                r.get::<_, i64>(7)?,
                Observation {
                    session: r.get(0)?,
                    lookup: LookupCode::from_code(lookup),
                    lookup_raw: lookup,
                    copy: CopyCode::from_code(copy),
                    copy_raw: copy,
                    path_class: PathClassCode::from_code(class),
                    blob: r.get::<_, Option<i64>>(4)?.map(BlobId::new),
                    size_matched_manifest: matched.map(|m| m == 1),
                },
            ))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    obs.sort_by(|a, b| (&a.0, a.2.session).cmp(&(&b.0, b.2.session)));
    let top = obs.last().map(|o| &o.2);
    let blob_facts = match (blob, len) {
        (Some(b), Some(len)) => {
            let held_by = obs
                .iter()
                .rev()
                .find(|o| o.2.blob == Some(BlobId::new(b)))
                .map(|o| o.2.session)
                .or(first)
                .unwrap_or(0);
            Some(BlobFacts {
                id: BlobId::new(b),
                stored_bytes: len,
                held_by,
            })
        }
        _ => None,
    };
    let card = match &blob_facts {
        Some(bf) => CardState::File {
            older_backup: top
                .filter(|o| {
                    !matches!(
                        o.lookup,
                        Some(LookupCode::Present | LookupCode::SizeMismatch)
                    )
                })
                .map(|o| OlderBackup {
                    held_by: bf.held_by,
                    newest: o.session,
                }),
        },
        None => card_without_blob(top),
    };
    let verify = match (blob_facts.as_ref(), vstate) {
        (Some(_), Some(v)) if v != BlobVerifyState::Ok.code() => Some(VerifyFacts {
            state: BlobVerifyState::from_code(v),
            raw: v,
            verified_at_ns: vat,
            may_be_stale: obs.iter().any(|(_, started, o)| {
                o.copy == Some(CopyCode::Replaced) && vat.is_none_or(|at| *started >= at)
            }),
        }),
        _ => None,
    };
    let mut hist = history(c, Entity::Attachment, id)?;
    hist.retain(|h| {
        matches!(
            h.column,
            HistoryColumn::TransferName | HistoryColumn::EmojiDescription
        )
    });
    Ok(Some(AttachmentFacts {
        id: AttachmentId::new(id),
        declared_type: ty,
        uti,
        name,
        declared_bytes: bytes,
        sticker,
        hide_attachment: hide,
        emoji_description: emoji,
        blob: blob_facts,
        observations: obs.into_iter().map(|(_, _, o)| o).collect(),
        card,
        verify,
        history: hist,
    }))
}

// ---------------------------------------------------------------------------
// Detail
// ---------------------------------------------------------------------------

type VersionRow = (
    i64,
    Vec<u8>,
    i64,
    i64,
    Option<String>,
    Option<i64>,
    Option<i64>,
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    i64,
);

/// One stored edit entry row.
struct EntryRow {
    part: i64,
    ordinal: i64,
    at: EditTime,
    body_present: bool,
}

fn entry_rows(c: &Connection, body: i64) -> rusqlite::Result<(Vec<EntryRow>, u64)> {
    let limit = limit_of(DETAIL_EDIT_ENTRIES);
    let rows: Vec<EntryRow> = {
        let mut st = c.prepare(SQL_EDIT_ENTRIES)?;
        let rows = st.query_map(rusqlite::params![body, limit], |r| {
            let raw = match r.get_ref(3)? {
                ValueRef::Integer(i) => Some(RawNumber::Integer(i)),
                ValueRef::Real(f) => Some(RawNumber::Real(f)),
                _ => None,
            };
            Ok(EntryRow {
                part: r.get(0)?,
                ordinal: r.get(1)?,
                at: EditTime {
                    kind: r
                        .get::<_, Option<i64>>(2)?
                        .and_then(SummaryTimeKind::from_code),
                    raw,
                    utc_ns: r.get(4)?,
                },
                body_present: r.get::<_, i64>(5)? == 1,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let more = if rows.len() >= DETAIL_EDIT_ENTRIES {
        let n: i64 = c
            .prepare(SQL_EDIT_ENTRY_COUNT)?
            .query_row([body], |r| r.get(0))?;
        u64::try_from(n)
            .unwrap_or(0)
            .saturating_sub(u64_of(rows.len()))
    } else {
        0
    };
    Ok((rows, more))
}

/// The edit entries of one body version, their texts decoded from the
/// stored summary (§3.7).
fn edit_entries(
    c: &Connection,
    body: i64,
    withhold: Option<WithheldReason>,
    reveal: Reveal,
) -> rusqlite::Result<(Vec<EditEntryView>, u64, EditTextsOutcome)> {
    let (rows, more) = entry_rows(c, body)?;
    if !rows.iter().any(|r| r.body_present) {
        let views = rows
            .into_iter()
            .map(|r| EditEntryView {
                part: r.part,
                ordinal: r.ordinal,
                at: r.at,
                text: EditEntryText::NoBody,
            })
            .collect();
        return Ok((views, more, EditTextsOutcome::NotNeeded));
    }
    let raw: Option<Vec<u8>> = c
        .prepare(SQL_SUMMARY_RAW)?
        .query_row([body], |r| Ok(bytes_of(r.get_ref(0)?)))
        .optional()?
        .flatten();
    let decoded = raw
        .as_deref()
        .map(|raw| edit_texts(raw, &TsBudget::default()));
    let (outcome, texts) = match decoded {
        Some(Ok(t)) => {
            // Alignment: every stored row with a body must have a decoded
            // entry at the same (part, ordinal), and the decoded entries
            // with a body must be exactly those rows.
            let stored: BTreeSet<(i64, i64)> = rows
                .iter()
                .filter(|r| r.body_present)
                .map(|r| (r.part, r.ordinal))
                .collect();
            let with_body: BTreeSet<(i64, i64)> = t
                .iter()
                .filter(|e| !matches!(e.body, EditBody::Absent))
                .map(|e| (i64::from(e.part), i64::from(e.ordinal)))
                .collect();
            let complete = more == 0;
            let aligned = if complete {
                stored == with_body
            } else {
                stored.is_subset(&with_body)
            };
            if aligned {
                (EditTextsOutcome::Decoded, Some(t))
            } else {
                (EditTextsOutcome::Unmatched, None)
            }
        }
        Some(Err(_)) | None => (EditTextsOutcome::SummaryUnreadable, None),
    };
    let views = rows
        .into_iter()
        .map(|r| {
            let text = if !r.body_present {
                EditEntryText::NoBody
            } else if let Some(t) = &texts {
                match t
                    .iter()
                    .find(|e| i64::from(e.part) == r.part && i64::from(e.ordinal) == r.ordinal)
                    .map(|e| &e.body)
                {
                    Some(EditBody::Text {
                        text,
                        one_time_codes,
                        runs_complete,
                    }) => match withhold {
                        Some(reason) if !reveal.earlier_text => EditEntryText::Withheld(reason),
                        _ => EditEntryText::Text(edit_text_pieces(
                            one_time_codes,
                            *runs_complete,
                            text.expose(),
                            reveal.one_time_code,
                        )),
                    },
                    Some(EditBody::Failed(k)) => EditEntryText::Failed(ts_code(*k)),
                    _ => EditEntryText::NotMatched,
                }
            } else {
                EditEntryText::NotMatched
            };
            EditEntryView {
                part: r.part,
                ordinal: r.ordinal,
                at: r.at,
                text,
            }
        })
        .collect();
    Ok((views, more, outcome))
}

/// The body versions of message `id`, oldest rank first.
fn versions(
    c: &Connection,
    m: &AssembledMessage,
    current: Option<i64>,
    reveal: Reveal,
) -> rusqlite::Result<(Vec<BodyVersion>, u64)> {
    let mut rows: Vec<VersionRow> = {
        let mut st = c.prepare(SQL_DETAIL_VERSIONS)?;
        let rows = st.query_map([m.id.get()], |r| {
            let text = match r.get_ref(4)? {
                ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
                _ => None,
            };
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                text,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    rows.sort_by(|a, b| (&a.1, a.0).cmp(&(&b.1, b.0)));
    let more = u64_of(rows.len().saturating_sub(DETAIL_VERSIONS));
    rows.truncate(DETAIL_VERSIONS);
    let withhold = withheld_reason(m);
    let mut out: Vec<BodyVersion> = Vec::with_capacity(rows.len());
    let mut previous: Option<Option<String>> = None;
    for (id, _rank, first, last, text, source, none, outcome, error, run_issue, text_st, label) in
        rows
    {
        let is_current = Some(id) == current;
        let vtext = match (&text, source) {
            (Some(t), Some(_)) => match withhold {
                Some(reason) if !is_current && !reveal.earlier_text => {
                    VersionText::Withheld(reason)
                }
                _ if reveal.one_time_code => VersionText::Text(vec![TextPiece::Text(
                    ContentText::from_str(t, BODY_PAGE_UTF16),
                )]),
                _ => VersionText::Text(masked_preview(
                    c,
                    id,
                    t,
                    source,
                    run_issue,
                    BODY_PAGE_UTF16,
                )?),
            },
            _ => VersionText::NoText(NoText {
                body: BodyId::new(id),
                reason: none.and_then(DisplayNoneReason::from_code),
                attributed: AttributedOutcome::from_code(outcome),
                attributed_error: error.and_then(TsErrorCode::from_code),
                text_column: text_st.and_then(ValueState::from_code),
            }),
        };
        let shown = text.filter(|_| source.is_some());
        let text_unchanged = previous
            .as_ref()
            .is_some_and(|p| p.is_some() && *p == shown);
        let (edits, more_edits, edit_texts) = edit_entries(c, id, withhold, reveal)?;
        out.push(BodyVersion {
            body: BodyId::new(id),
            current: is_current,
            first_session: first,
            last_session: last,
            text: vtext,
            text_unchanged,
            edit_label: EditLabelCode::from_code(label),
            edits,
            more_edits,
            edit_texts,
        });
        previous = Some(shown);
    }
    Ok((out, more))
}

fn chats(c: &Connection, id: i64) -> rusqlite::Result<Vec<ChatFlags>> {
    type Row = (i64, RawValue, RawValue, RawValue);
    let rows: Vec<Row> = {
        let mut st = c.prepare(SQL_MESSAGE_CHATS)?;
        let rows = st.query_map(rusqlite::params![id, limit_of(DETAIL_CHATS)], |r| {
            Ok((
                r.get(0)?,
                RawValue::of(r.get(1)?, r.get(2)?),
                RawValue::of(r.get(3)?, r.get(4)?),
                RawValue::of(r.get(5)?, r.get(6)?),
            ))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut out = Vec::with_capacity(rows.len());
    for (chat, style, is_filtered, is_archived) in rows {
        out.push(ChatFlags {
            chat: ChatId::new(chat),
            style,
            is_filtered,
            is_archived,
            history: history(c, Entity::Chat, chat)?,
        });
    }
    Ok(out)
}

/// What the detail row adds to the assembled message.
struct DetailRow {
    times: DetailTimes,
    account: OptContent,
    subject: OptContent,
    reply_guid: Option<Vec<u8>>,
    target_guid: Option<Vec<u8>>,
}

fn detail_row(c: &Connection, id: i64) -> rusqlite::Result<Option<DetailRow>> {
    c.prepare(SQL_DETAIL_ROW)?
        .query_row([id], |r| {
            let t = |i: usize| -> rusqlite::Result<RecordedTime> {
                Ok(recorded_time(
                    r.get(i)?,
                    r.get(i + 1)?,
                    r.get(i + 2)?,
                    r.get(i + 3)?,
                ))
            };
            Ok(DetailRow {
                times: DetailTimes {
                    date: t(0)?,
                    edited: t(4)?,
                    retracted: t(8)?,
                },
                account: OptContent::from_cell(r.get_ref(12)?, r.get(13)?, IDENTIFIER_UTF16),
                subject: OptContent::from_cell(r.get_ref(14)?, r.get(15)?, PREVIEW_UTF16),
                reply_guid: bytes_of(r.get_ref(16)?).filter(|g| !g.is_empty()),
                target_guid: bytes_of(r.get_ref(17)?),
            })
        })
        .optional()
}

/// The budgeted unkeyed probe (§3.5): whether unkeyed messages share
/// `guid`. A trip reads `NotChecked`.
fn probe(p: &Phase<'_>, guid: &[u8]) -> Result<IdentifierProbe, ArchiveError> {
    match p.sql(|c| {
        c.prepare(SQL_UNKEYED_PROBE)?
            .query_row([guid], |r| r.get::<_, i64>(0))
    }) {
        Ok(0) => Ok(IdentifierProbe::NotInArchive),
        Ok(_) => Ok(IdentifierProbe::SharedByUnkeyed),
        Err(ArchiveError::QueryTooBroad) => Ok(IdentifierProbe::NotChecked),
        Err(e) => Err(e),
    }
}

/// The current body id of the assembled message.
fn current_body(m: &AssembledMessage) -> Option<i64> {
    match &m.body {
        BodyView::Text(t) => Some(t.body.get()),
        BodyView::NoText(n) => Some(n.body.get()),
        BodyView::NoBody => None,
    }
}

impl ArchiveReader {
    /// Everything the details panel shows for message `id` (M5 spec §3.7,
    /// §7.3): the message as a page shows it, its body versions with their
    /// edit entries and the edit texts decoded at read time (owner Q3a),
    /// the raw times, the account, the subject, value history, the
    /// conversations it is in with their raw flags, every attachment's
    /// facts and card, and the unkeyed identifier probe for a reply or
    /// reaction whose target is not uniquely identified. Earlier text and
    /// one-time codes are withheld unless `reveal` asks (D115). One
    /// snapshot; the `Detail` phase, then the probe in its own phase. An
    /// id that does not exist is `AnchorNotFound`.
    pub fn message_detail(
        &self,
        id: MessageId,
        reveal: Reveal,
    ) -> Result<MessageDetail, ArchiveError> {
        self.require_current()?;
        let conn = self.conn();
        let deadline = Instant::now() + ViewBudget::Detail.window();
        conn.snapshot(|| {
            let (detail, guids) = conn.viewing(ViewBudget::Detail, None, deadline, |p| {
                let ctx = p.sql(Ctx::load)?;
                let caps = Caps::PAGE;
                let (mut m, x) = p
                    .sql(|c| assemble::base(c, &ctx, None, id.get(), &caps, reveal))?
                    .ok_or(ArchiveError::AnchorNotFound)?;
                p.checkpoint()?;
                p.sql(|c| assemble::extras(c, None, &mut m, &x, &caps))?;
                p.checkpoint()?;
                let row = p
                    .sql(|c| detail_row(c, id.get()))?
                    .ok_or(ArchiveError::AnchorNotFound)?;
                let current = current_body(&m);
                let (versions, more_versions) = p.sql(|c| versions(c, &m, current, reveal))?;
                p.checkpoint()?;
                let history = p.sql(|c| history(c, Entity::Message, id.get()))?;
                let chats = p.sql(|c| chats(c, id.get()))?;
                p.checkpoint()?;
                let ids: Vec<i64> = p.sql(|c| {
                    let mut st = c.prepare(SQL_ATTACHMENT_IDS)?;
                    let rows = st.query_map(
                        rusqlite::params![id.get(), limit_of(caps.attachments)],
                        |r| r.get(0),
                    )?;
                    rows.collect()
                })?;
                let mut attachments = Vec::with_capacity(ids.len());
                for a in ids {
                    p.checkpoint()?;
                    if let Some(f) = p.sql(|c| attachment_facts(c, a))? {
                        attachments.push(f);
                    }
                }
                let reply_guid = row
                    .reply_guid
                    .clone()
                    .filter(|_| matches!(m.reply, Some(ReplyRef::NotUniquelyIdentified)));
                let target_guid = row.target_guid.clone().filter(|_| {
                    m.association
                        .as_ref()
                        .is_some_and(|a| a.visibility == AssocVisibility::TargetNotUnique)
                });
                Ok((
                    MessageDetail {
                        message: m,
                        versions,
                        more_versions,
                        times: row.times,
                        account: row.account,
                        subject: row.subject,
                        history,
                        chats,
                        attachments,
                        reply_probe: None,
                        reaction_target_probe: None,
                    },
                    (reply_guid, target_guid),
                ))
            })?;
            let mut detail = detail;
            let (reply_guid, target_guid) = guids;
            if reply_guid.is_some() || target_guid.is_some() {
                let probe_deadline = Instant::now() + ViewBudget::Detail.window();
                let (r, t) = conn.viewing(ViewBudget::Detail, None, probe_deadline, |p| {
                    let r = reply_guid.as_deref().map(|g| probe(p, g)).transpose()?;
                    let t = target_guid.as_deref().map(|g| probe(p, g)).transpose()?;
                    Ok((r, t))
                })?;
                detail.reply_probe = r;
                detail.reaction_target_probe = t;
            }
            Ok(detail)
        })
    }

    /// The facts and card of one attachment (§3.9, §5.9), under the
    /// `Lookup` phase. An id that does not exist is `AnchorNotFound`.
    pub fn attachment(&self, id: AttachmentId) -> Result<AttachmentFacts, ArchiveError> {
        self.require_current()?;
        let conn = self.conn();
        let deadline = Instant::now() + ViewBudget::Lookup.window();
        conn.snapshot(|| {
            conn.viewing(ViewBudget::Lookup, None, deadline, |p| {
                p.sql(|c| attachment_facts(c, id.get()))?
                    .ok_or(ArchiveError::AnchorNotFound)
            })
        })
    }

    /// A page of the reaction rows recorded on `target` (§3.6): every add
    /// and remove row (`assoc_kind` 10–17, 20–27, `assoc_present = 1`)
    /// whose target GUID is the keyed target's, ordered by date (undated
    /// last), then id, [`REACTION_HISTORY_PAGE`] per page: the first page
    /// without `after`, else the rows strictly after the cursor the
    /// previous page returned in `next`. At most [`REACTION_HISTORY_LIMIT`]
    /// rows are read; `total` says when there were more. An unkeyed target
    /// has none.
    pub fn reaction_history(
        &self,
        target: MessageId,
        after: Option<&ReactionCursor>,
    ) -> Result<ReactionHistory, ArchiveError> {
        self.require_current()?;
        let conn = self.conn();
        let deadline = Instant::now() + ViewBudget::Detail.window();
        conn.snapshot(|| {
            conn.viewing(ViewBudget::Detail, None, deadline, |p| {
                let row: Option<(i64, Option<Vec<u8>>)> = p.sql(|c| {
                    c.prepare(SQL_TARGET)?
                        .query_row([target.get()], |r| Ok((r.get(0)?, bytes_of(r.get_ref(1)?))))
                        .optional()
                })?;
                let (key_kind, guid) = row.ok_or(ArchiveError::AnchorNotFound)?;
                let empty = |keyed: bool| ReactionHistory {
                    target,
                    keyed,
                    rows: Vec::new(),
                    total: CappedCount {
                        n: 0,
                        capped: false,
                    },
                    has_more: false,
                    next: None,
                };
                let Some(guid) = guid.filter(|g| key_kind == 1 && !g.is_empty()) else {
                    return Ok(empty(false));
                };
                let limit = CappedCount::limit(u64_of(REACTION_HISTORY_LIMIT));
                let mut all: Vec<(Vec<u8>, ReactionRecord)> = p.sql(|c| {
                    let mut st = c.prepare(SQL_REACTION_ROWS)?;
                    let rows = st.query_map(rusqlite::params![guid, limit], |r| {
                        let code: Option<i64> = r.get(1)?;
                        let emoji: Option<Vec<u8>> = bytes_of(r.get_ref(2)?);
                        let emoji_st: Option<i64> = r.get(3)?;
                        Ok((
                            bytes_of(r.get_ref(12)?).unwrap_or_default(),
                            ReactionRecord {
                                id: MessageId::new(r.get(0)?),
                                kind: code.and_then(AssocKindCode::from_code),
                                adds: matches!(code, Some(10..=17)),
                                emoji: (emoji.is_some() || emoji_st.is_some())
                                    .then(|| emoji_content(emoji.as_deref(), emoji_st)),
                                sender: chip_sender(r.get(4)?, r.get(5)?, r.get(6)?),
                                time: recorded_time(r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?),
                                target_part: r.get(11)?,
                            },
                        ))
                    })?;
                    rows.collect()
                })?;
                let total = CappedCount::of(
                    i64::try_from(all.len()).unwrap_or(i64::MAX),
                    u64_of(REACTION_HISTORY_LIMIT),
                );
                all.truncate(REACTION_HISTORY_LIMIT);
                all.sort_by(|a, b| (&a.0, a.1.id).cmp(&(&b.0, b.1.id)));
                let mut rest: Vec<(Vec<u8>, ReactionRecord)> = match after {
                    None => all,
                    Some(cur) => all
                        .into_iter()
                        .filter(|(k, r)| (k, r.id.get()) > (&cur.key, cur.id))
                        .collect(),
                };
                let has_more = rest.len() > REACTION_HISTORY_PAGE;
                rest.truncate(REACTION_HISTORY_PAGE);
                let next = rest
                    .last()
                    .filter(|_| has_more)
                    .map(|(k, r)| ReactionCursor {
                        key: k.clone(),
                        id: r.id.get(),
                    });
                Ok(ReactionHistory {
                    target,
                    keyed: true,
                    rows: rest.into_iter().map(|(_, r)| r).collect(),
                    total,
                    has_more,
                    next,
                })
            })
        })
    }

    /// A chunk of body `body`'s text (§3.8): at most [`TEXT_CHUNK_UTF16`]
    /// UTF-16 units from `from_utf16`, which is clamped to the body's
    /// length and moved back off a low surrogate (a chunk never splits a
    /// surrogate pair; it may split a grapheme). One-time codes are
    /// withheld unless revealed; a body that is earlier text of a message
    /// with an unsent part, or whose current body has no text, is withheld
    /// unless `reveal.earlier_text` (D115). Unknown bodies are
    /// `AnchorNotFound`.
    pub fn message_text(
        &self,
        body: BodyId,
        from_utf16: u32,
        reveal: Reveal,
    ) -> Result<TextChunk, ArchiveError> {
        self.require_current()?;
        let conn = self.conn();
        let deadline = Instant::now() + ViewBudget::Detail.window();
        conn.snapshot(|| {
            conn.viewing(ViewBudget::Detail, None, deadline, |p| {
                p.sql(|c| text_chunk(c, body.get(), from_utf16, reveal))?
                    .ok_or(ArchiveError::AnchorNotFound)
            })
        })
    }
}

type TextBodyRow = (i64, Option<String>, Option<i64>, Option<i64>, Option<i64>);

/// The withholding reason for a non-current body of a message whose
/// current body is `current` (D115).
fn earlier_withheld(
    c: &Connection,
    current: Option<i64>,
) -> rusqlite::Result<Option<WithheldReason>> {
    let Some(cur) = current else {
        return Ok(Some(WithheldReason::NoCurrentText));
    };
    let unsent = c
        .prepare(SQL_ANY_UNSENT)?
        .query_row([cur], |_| Ok(()))
        .optional()?
        .is_some();
    if unsent {
        return Ok(Some(WithheldReason::Unsent));
    }
    let state: Option<(i64, i64)> = c
        .prepare(SQL_CURRENT_STATE)?
        .query_row([cur], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    Ok(match state {
        Some((1, _)) => None,
        Some((_, label)) if label == EditLabelCode::DateEditedNoBody.code() => {
            Some(WithheldReason::EarlierVersion)
        }
        _ => Some(WithheldReason::NoCurrentText),
    })
}

/// The byte range of `text` from UTF-16 unit `from` (moved back off a low
/// surrogate) spanning at most `len` units, with the units it starts and
/// ends at.
pub(crate) fn utf16_window(text: &str, from: u32, len: u32) -> (usize, usize, u32, u32) {
    let mut units = 0u32;
    let mut start: Option<(usize, u32)> = None;
    for (i, ch) in text.char_indices() {
        let next = units.saturating_add(u32::try_from(ch.len_utf16()).unwrap_or(2));
        if start.is_none() && next > from {
            start = Some((i, units));
        }
        if let Some((a, s)) = start {
            if next.saturating_sub(s) > len {
                return (a, i, s, units);
            }
        }
        units = next;
    }
    match start {
        Some((a, s)) => (a, text.len(), s, units),
        None => (text.len(), text.len(), units, units),
    }
}

fn text_chunk(
    c: &Connection,
    body: i64,
    from: u32,
    reveal: Reveal,
) -> rusqlite::Result<Option<TextChunk>> {
    let row: Option<TextBodyRow> = c
        .prepare(SQL_TEXT_BODY)?
        .query_row([body], |r| {
            Ok((
                r.get(0)?,
                text_of(r.get_ref(1)?),
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
            ))
        })
        .optional()?;
    let Some((_message, text, source, run_issue, current)) = row else {
        return Ok(None);
    };
    let Some(text) = text.filter(|_| source.is_some()) else {
        return Ok(Some(TextChunk {
            body: BodyId::new(body),
            from_utf16: 0,
            end_utf16: 0,
            full_utf16: 0,
            text: ChunkText::NoText,
        }));
    };
    let full = utf16_len(&text);
    let (a, b, start, end) = utf16_window(&text, from.min(full), TEXT_CHUNK_UTF16);
    let chunk = |t: ChunkText| TextChunk {
        body: BodyId::new(body),
        from_utf16: start,
        end_utf16: end,
        full_utf16: full,
        text: t,
    };
    if current != Some(body) && !reveal.earlier_text {
        if let Some(reason) = earlier_withheld(c, current)? {
            return Ok(Some(chunk(ChunkText::Withheld(reason))));
        }
    }
    let piece = text.get(a..b).unwrap_or("");
    // Every code range of the body, before any is dropped for lying
    // outside this chunk: whether the body has a code at all, and whether
    // its ranges can be trusted, is decided on the whole body, never on
    // the chunk (an untrusted range says nothing about where the code is).
    let all: Vec<(u32, u32)> = {
        let mut st = c.prepare(SQL_FLAGGED_PARTS)?;
        let rows = st.query_map(rusqlite::params![body, 1i64 << ONE_TIME_CODE_BIT], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        let clamp = |v: i64| u32::try_from(v.max(0)).unwrap_or(u32::MAX);
        rows.map(|r| r.map(|(s, e)| (clamp(s), clamp(e))))
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let trusted = source == Some(DisplaySource::AttributedBody.code()) && run_issue.is_none();
    let lossy = source == Some(DisplaySource::TextLossy.code());
    if !all.is_empty() && !trusted && !reveal.one_time_code {
        return Ok(Some(chunk(ChunkText::Text(vec![TextPiece::Masked(
            MaskKind::OneTimeCode,
        )]))));
    }
    // Trusted (or revealed): the ranges, moved to the chunk's start; a
    // range that ends before the chunk does not touch it.
    let ranges: Vec<(u32, u32)> = all
        .into_iter()
        .map(|(s, e)| (s.saturating_sub(start), e.saturating_sub(start)))
        .filter(|(_, e)| *e > 0)
        .collect();
    let pieces = masked(&ranges, true, piece, u32::MAX, reveal.one_time_code)
        .into_iter()
        .map(|p| match p {
            TextPiece::Text(t) => TextPiece::Text(t.lossy(lossy)),
            m => m,
        })
        .collect();
    Ok(Some(chunk(ChunkText::Text(pieces))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::codes::ArchiveColumn;

    #[test]
    fn an_edit_text_with_incomplete_runs_is_withheld_whole() {
        let whole = vec![TextPiece::Masked(MaskKind::OneTimeCode)];
        // No code decoded, but the runs stopped early: a code could be
        // missing, so nothing is shown unless codes are revealed.
        assert_eq!(edit_text_pieces(&[], false, "code 123456", false), whole);
        assert_eq!(
            edit_text_pieces(&[(5, 11)], false, "code 123456", false),
            whole
        );
        assert_eq!(
            edit_text_pieces(&[], false, "code 123456", true),
            vec![TextPiece::Text(ContentText::from_str(
                "code 123456",
                BODY_PAGE_UTF16
            ))]
        );
        // Complete runs: only the code is masked; no code, no mask.
        let p = edit_text_pieces(&[(5, 11)], true, "code 123456", false);
        assert_eq!(p.last(), Some(&TextPiece::Masked(MaskKind::OneTimeCode)));
        assert_eq!(p.len(), 2);
        assert_eq!(
            edit_text_pieces(&[], true, "hello", false),
            vec![TextPiece::Text(ContentText::from_str(
                "hello",
                BODY_PAGE_UTF16
            ))]
        );
    }

    #[test]
    fn history_codes_name_the_columns_they_claim() {
        for (code, entity, name) in [
            (COL_CHAT_DISPLAY_NAME, Entity::Chat, "display_name"),
            (
                COL_DESTINATION_CALLER_ID,
                Entity::Message,
                "destination_caller_id",
            ),
            (COL_SUBJECT, Entity::Message, "subject"),
            (COL_GROUP_TITLE, Entity::Message, "group_title"),
            (COL_TRANSFER_NAME, Entity::Attachment, "transfer_name"),
            (
                COL_EMOJI_DESCRIPTION,
                Entity::Attachment,
                "emoji_description",
            ),
        ] {
            let c = ArchiveColumn::from_code(u16::try_from(code).expect("u16")).expect("known");
            assert_eq!((c.entity, c.name), (entity, name));
            assert_eq!(c.rule, crate::schema::codes::ColumnRule::History);
        }
    }

    #[test]
    fn windows_clamp_and_never_split_a_surrogate_pair() {
        let text = "ab\u{1F600}cd";
        // From inside the pair: moved back to its start.
        assert_eq!(utf16_window(text, 3, 10), (2, 8, 2, 6));
        // A window that would end inside the pair stops before it.
        assert_eq!(utf16_window(text, 0, 3), (0, 2, 0, 2));
        assert_eq!(utf16_window(text, 0, 4), (0, 6, 0, 4));
        // From the end, and past it.
        assert_eq!(utf16_window(text, 6, 10), (8, 8, 6, 6));
        assert_eq!(utf16_window("", 0, 10), (0, 0, 0, 0));
        // Chunks concatenate to the text.
        let long: String = "x\u{1F600}".repeat(1000);
        let mut at = 0u32;
        let mut out = String::new();
        loop {
            let (a, b, s, e) = utf16_window(&long, at, 7);
            assert_eq!(s, at);
            if a == b {
                break;
            }
            out.push_str(&long[a..b]);
            at = e;
        }
        assert_eq!(out, long);
    }

    fn obs(lookup: LookupCode, copy: CopyCode) -> Observation {
        Observation {
            session: 1,
            lookup: Some(lookup),
            lookup_raw: lookup.code(),
            copy: Some(copy),
            copy_raw: copy.code(),
            path_class: None,
            blob: None,
            size_matched_manifest: None,
        }
    }

    #[test]
    fn cards_follow_the_lookup_code_then_the_copy_code() {
        use CopyCode as C;
        use LookupCode as L;
        let cases = [
            (L::NotLookedUp, C::NotRequested, CardState::NotCopied),
            (L::Present, C::NotRequested, CardState::NotCopied),
            (
                L::NotListedAtSourcedKey,
                C::NotInBackup,
                CardState::NotInBackup,
            ),
            (
                L::NotFoundAtAssumedKey,
                C::NotInBackup,
                CardState::NotFoundAtAssumedKey,
            ),
            (
                L::NotAttemptedClass,
                C::NotLookedUp,
                CardState::ClassNotLookedUp,
            ),
            (
                L::NotAttemptedNull,
                C::NotLookedUp,
                CardState::LocationUnusable(L::NotAttemptedNull),
            ),
            (
                L::KeyDoesNotUnwrap,
                C::BlobUnavailable,
                CardState::NotDecrypted,
            ),
            (
                L::NoProtectionClass,
                C::BlobUnavailable,
                CardState::NotDecrypted,
            ),
            (
                L::BlobMissing,
                C::BlobUnavailable,
                CardState::CouldNotBeRead,
            ),
            (L::Ambiguous, C::NotInBackup, CardState::CouldNotBeRead),
            (L::Present, C::ReadFailed, CardState::CouldNotBeRead),
            (L::Present, C::OverLimit, CardState::TooLarge),
            (L::SizeMismatch, C::TooLargeForVolume, CardState::TooLarge),
            (
                L::Present,
                C::WriteFailed,
                CardState::Unavailable {
                    lookup: 20,
                    copy: 14,
                },
            ),
        ];
        for (l, c, want) in cases {
            assert_eq!(card_without_blob(Some(&obs(l, c))), want, "{l:?} {c:?}");
        }
        assert_eq!(card_without_blob(None), CardState::NoObservation);
    }

    #[test]
    fn code_masking_follows_trust_and_reveal() {
        let shown = |p: &[TextPiece]| -> Vec<String> {
            p.iter()
                .map(|x| match x {
                    TextPiece::Text(t) => t.text.expose().clone(),
                    TextPiece::Masked(_) => "<code>".into(),
                })
                .collect()
        };
        let codes = [(3u32, 6u32)];
        assert_eq!(
            shown(&masked(&codes, true, "ab 123 x", 100, false)),
            ["ab ", "<code>", " x"]
        );
        assert_eq!(
            shown(&masked(&codes, false, "ab 123 x", 100, false)),
            ["<code>"]
        );
        assert_eq!(
            shown(&masked(&codes, false, "ab 123 x", 100, true)),
            ["ab 123 x"]
        );
        assert_eq!(shown(&masked(&[], false, "ab", 100, false)), ["ab"]);
    }
}
