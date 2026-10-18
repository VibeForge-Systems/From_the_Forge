//! P3: messages (spec §8.4, §8.7, §8.8, §10, D58, D59, D61).
//!
//! Each source message becomes, inside its savepoint:
//!
//! - a **`message` row**, keyed on its exact GUID bytes when that GUID is
//!   usable and unique in the source, otherwise unkeyed (the fingerprint
//!   covers every raw field and the body's `raw_hash`). Its raw cells merge
//!   by the column rules of §8.7 ([`super::merge`]); its derived columns
//!   (`service_class`, `item_kind`, the `assoc_*` parse, `sort_key`) are
//!   recomputed from the **stored** raw values after the merge, and only
//!   the ones that changed are written;
//! - a **body version** (`message_body`) per distinct raw body state: the
//!   `text`, `attributedBody` and `message_summary_info` cells (raw bytes
//!   kept, D59) hashed into `raw_hash`. A version is never replaced or
//!   deleted: an edited or unsent message gains a version, and earlier
//!   text stays indexed and searchable (D61). `current_body_id` follows
//!   rank. The same raw body seen by a newer decoder is re-derived in
//!   place. Its parts, edit entries and unsent parts are child rows;
//! - the FTS index, through the guarded triggers on `display_text` (§10);
//! - the map row, and `lineage_seen`.
//!
//! **`display_text`** (D58): the decoded `attributedBody` when it decoded
//! to non-empty text; else the `text` column when it is non-empty valid
//! UTF-8; else the `text` column decoded lossily (`display_source = 3`);
//! else NULL with a [`DisplayNoneReason`].
//!
//! **`handle_ref` and `other_handle_ref`** resolve the raw source rowid
//! through this session's map. A rowid means something only inside its
//! session, so the reference is set by the observation that takes the
//! `handle_rowid` cell (the rank rule), and kept otherwise; 0, a missing
//! rowid and an unmapped one give NULL.
//!
//! **Recently Deleted** (§8.8): with the default exclusion, a row in
//! [`RdPlan::excluded`] is skipped (no row, no map), and a row in
//! [`RdPlan::archived`] is only mapped and re-observed for the lineage:
//! none of its columns, bodies or ranks change. With the opt-in, every row
//! merges like any other.

use std::ops::ControlFlow;

use oma_messages::{
    AssocRaw, AssocTarget, AssociatedKind, AttrFlags, AttrKeysPresent, AttributedBody,
    AttributedOutcome, Col, Cursor, EditLabel, FieldOutcome, ItemKind, KnownTable,
    MessageReadOptions, MessageRecord, MessagesCtx, MessagesError, PassOutcome, Reaction,
    SharedValue, SmsDb, SummaryInfo, SummaryTimeRaw, TimeInterp, TsErrorKind,
};
use rusqlite::{Connection, OptionalExtension, Statement};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::batch::{record, Brk, IssueDeltas, PassWriter, Run};
use super::bind::{Cell, Raw};
use super::counts::{BodyCounts, ImportCounts};
use super::entity::{keep_raw, text_len};
use super::keys::{encode, fingerprint, Field, Rank};
use super::map::MapStmts;
use super::merge::{
    attach_issues, decide, insert, merge, read_stored, Decision, Ident, MergeStmts, Param, TableDef,
};
use super::preflight::{guid_digest, RdPlan};
use super::service_class;
use crate::progress::ImportPhase;
use crate::schema::codes::{
    AssocKindCode, AssocPresent, AssocTargetShape, AttributedOutcome as AttrCode,
    DisplayNoneReason, DisplaySource, EditLabelCode, Entity, FieldOutcome as FieldCode, IssueCode,
    ItemKindCode, Pass, SummaryOutcome, SummaryTimeKind, TsErrorCode, ValueState, ATTR_FLAG_BITS,
    ATTR_KEY_BITS,
};
use crate::schema::{DECODER_VERSION, DERIVER_VERSION};

// The indices of the cells P3 reads back, in `ARCHIVE_COLUMNS` order
// (checked by a test).
const GUID: usize = 0;
const HANDLE: usize = 2;
const OTHER_HANDLE: usize = 3;
const SERVICE: usize = 5;
const DATE: usize = 6;
const ITEM_TYPE: usize = 18;
const GROUP_ACTION_TYPE: usize = 19;
const ASSOC_TYPE: usize = 23;
const ASSOC_GUID: usize = 24;
const ASSOC_EMOJI: usize = 25;
const ASSOC_LOCATION: usize = 26;
const ASSOC_LENGTH: usize = 27;

/// The `value_issue.col` codes of a body version's raw cells.
const BODY_TEXT: u16 = 501;
const BODY_ATTRIBUTED: u16 = 502;
const BODY_SUMMARY: u16 = 503;

/// The domain prefix of a body version's `raw_hash`.
const BODY_HASH_DOMAIN: &[u8] = b"oma-body-raw-v1";

/// The derived columns P3 sets on insert, after the value columns.
const DERIVED: &[&str] = &[
    "service_class",
    "item_kind",
    "assoc_present",
    "assoc_kind",
    "assoc_target_shape",
    "assoc_target_guid",
    "assoc_target_part",
    "handle_ref",
    "other_handle_ref",
    "sort_key",
    "first_src_rowid",
    "derived_by",
];

fn bad(what: &'static str) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(what.into())
}

/// A source column name to its archive name (`value_issue` raw values).
fn rename(source: &'static str) -> &'static str {
    match source {
        "handle_id" => "handle_rowid",
        "other_handle" => "other_handle_rowid",
        "associated_message_type" => "assoc_type",
        "associated_message_guid" => "assoc_guid_raw",
        "associated_message_emoji" => "assoc_emoji",
        "associated_message_range_location" => "assoc_range_location",
        "associated_message_range_length" => "assoc_range_length",
        other => other,
    }
}

/// The message table's merge definition.
pub(crate) fn table_def() -> TableDef {
    TableDef::new(Entity::Message, "message", false, DERIVED)
}

pub(crate) struct Messages {
    def: TableDef,
    rd: RdPlan,
    opts: MessageReadOptions,
}

impl Messages {
    /// P3 under the Recently Deleted plan of the preflight. Raw blobs are
    /// always kept (D59) and `attributedBody` is decoded.
    pub(crate) fn new(rd: RdPlan) -> Messages {
        Messages {
            def: table_def(),
            rd,
            opts: MessageReadOptions::default()
                .with_decode_attributed_body(true)
                .with_keep_raw_blobs(true),
        }
    }

    fn cells(&self, r: &MessageRecord) -> Vec<Cell> {
        let a = &r.assoc_raw;
        let payload = r.payload_data.as_ref().map_or_else(
            // Raw blobs are always kept, so the reader always reads it.
            || Cell::blob(&Col::Absent),
            Cell::blob,
        );
        let mut cells = vec![
            Cell::text(&r.guid),
            Cell::boolean(&r.is_from_me),
            Cell::int(&r.handle_id),
            Cell::int(&r.other_handle),
            Cell::text(&r.destination_caller_id),
            Cell::text(&r.service),
            Cell::time(&r.date),
            Cell::time(&r.date_read),
            Cell::time(&r.date_delivered),
            Cell::time(&r.date_played),
            Cell::time(&r.date_edited),
            Cell::time(&r.date_retracted),
            Cell::boolean(&r.is_read),
            Cell::boolean(&r.is_delivered),
            Cell::boolean(&r.is_sent),
            Cell::boolean(&r.is_audio_message),
            Cell::boolean(&r.cache_has_attachments),
            Cell::text(&r.subject),
            Cell::int(&r.item_type),
            Cell::int(&r.group_action_type),
            Cell::text(&r.group_title),
            Cell::int(&r.share_status),
            Cell::boolean(&r.share_direction),
            Cell::int(&a.raw_type),
            Cell::text(&a.raw_guid),
            Cell::text(&a.emoji),
            Cell::int(&a.range_location),
            Cell::int(&a.range_length),
            Cell::text(&r.reply.thread_originator_guid),
            Cell::text(&r.reply.thread_originator_part),
            Cell::text(&r.reply.reply_to_guid),
            Cell::text(&r.balloon_bundle_id),
            Cell::text(&r.expressive_send_style_id),
            payload,
            Cell::int(&r.part_count),
            Cell::int(&r.schedule_type),
            Cell::int(&r.schedule_state),
            Cell::int(&r.filter_action),
            Cell::int(&r.filter_sub_action),
            Cell::int(&r.error),
            Cell::boolean(&r.was_downgraded),
        ];
        keep_raw(&self.def, &mut cells, &r.raw_issues, rename);
        cells
    }
}

// ---------------------------------------------------------------------------
// Derived message columns
// ---------------------------------------------------------------------------

/// The derived columns that depend on raw cells only.
#[derive(Clone, PartialEq, Eq)]
struct CellDerived {
    service_class: Option<i64>,
    item_kind: i64,
    assoc_present: i64,
    assoc_kind: Option<i64>,
    assoc_shape: Option<i64>,
    assoc_guid: Option<Zeroizing<Vec<u8>>>,
    assoc_part: Option<i64>,
}

fn col_i64(cells: &[Cell], i: usize) -> Col<i64> {
    cells.get(i).map_or(Col::Absent, Cell::as_col_i64)
}

fn item_kind_code(k: &ItemKind) -> ItemKindCode {
    match k {
        ItemKind::Message => ItemKindCode::Message,
        ItemKind::ParticipantAdded => ItemKindCode::ParticipantAdded,
        ItemKind::OwnNumberChanged => ItemKindCode::OwnNumberChanged,
        ItemKind::ParticipantRemoved => ItemKindCode::ParticipantRemoved,
        ItemKind::GroupRenamed => ItemKindCode::GroupRenamed,
        ItemKind::ParticipantLeft => ItemKindCode::ParticipantLeft,
        ItemKind::GroupPhotoChanged => ItemKindCode::GroupPhotoChanged,
        ItemKind::GroupPhotoRemoved => ItemKindCode::GroupPhotoRemoved,
        ItemKind::BackgroundChanged => ItemKindCode::BackgroundChanged,
        ItemKind::BackgroundRemoved => ItemKindCode::BackgroundRemoved,
        ItemKind::LegacySharedLocation => ItemKindCode::LegacySharedLocation,
        ItemKind::AudioKept => ItemKindCode::AudioKept,
        ItemKind::SharePlay => ItemKindCode::SharePlay,
        _ => ItemKindCode::Unknown,
    }
}

fn reaction_codes(r: Reaction) -> (AssocKindCode, AssocKindCode) {
    use AssocKindCode as K;
    match r {
        Reaction::Love => (K::TapbackLove, K::RemovedLove),
        Reaction::Like => (K::TapbackLike, K::RemovedLike),
        Reaction::Dislike => (K::TapbackDislike, K::RemovedDislike),
        Reaction::Laugh => (K::TapbackLaugh, K::RemovedLaugh),
        Reaction::Emphasize => (K::TapbackEmphasize, K::RemovedEmphasize),
        Reaction::Question => (K::TapbackQuestion, K::RemovedQuestion),
        Reaction::Emoji => (K::TapbackEmoji, K::RemovedEmoji),
        Reaction::Sticker => (K::TapbackSticker, K::RemovedSticker),
        _ => (K::Unknown, K::Unknown),
    }
}

fn assoc_kind_code(k: AssociatedKind) -> AssocKindCode {
    match k {
        AssociatedKind::NoAssociation => AssocKindCode::NoAssociation,
        AssociatedKind::StickerLegacy => AssocKindCode::StickerLegacy,
        AssociatedKind::PollVote => AssocKindCode::PollVote,
        AssociatedKind::Tapback { reaction, removal } => {
            let (add, remove) = reaction_codes(reaction);
            if removal {
                remove
            } else {
                add
            }
        }
        _ => AssocKindCode::Unknown,
    }
}

/// The derived columns of the raw cells `cells` (stored or observed).
fn derive_cells(cells: &[Cell]) -> CellDerived {
    let item = ItemKind::classify(
        &col_i64(cells, ITEM_TYPE),
        &col_i64(cells, GROUP_ACTION_TYPE),
        &col_i64(cells, HANDLE),
        &col_i64(cells, OTHER_HANDLE),
    );
    let text = |i: usize| cells.get(i).map_or(Col::Absent, Cell::as_col_text);
    let raw_type = col_i64(cells, ASSOC_TYPE);
    let assoc = AssocRaw::new(
        raw_type.clone(),
        text(ASSOC_GUID),
        text(ASSOC_EMOJI),
        col_i64(cells, ASSOC_LOCATION),
        col_i64(cells, ASSOC_LENGTH),
    )
    .association();
    let mut out = CellDerived {
        service_class: service_class(cells.get(SERVICE)),
        item_kind: item_kind_code(&item).code(),
        assoc_present: AssocPresent::No.code(),
        assoc_kind: None,
        assoc_shape: None,
        assoc_guid: None,
        assoc_part: None,
    };
    match assoc {
        Some(a) => {
            out.assoc_present = AssocPresent::Yes.code();
            out.assoc_kind = a.kind.map(|k| assoc_kind_code(k).code());
            match &a.target {
                Col::Value(t) => {
                    let (shape, part) = match t {
                        AssocTarget::Part { part, .. } => {
                            (AssocTargetShape::Part, Some(i64::from(*part)))
                        }
                        AssocTarget::Bubble { .. } => (AssocTargetShape::Bubble, None),
                        AssocTarget::Bare { .. } => (AssocTargetShape::Bare, None),
                        _ => (AssocTargetShape::Unparsed, None),
                    };
                    out.assoc_shape = Some(shape.code());
                    out.assoc_part = part;
                    out.assoc_guid = t
                        .guid()
                        .map(|g| Zeroizing::new(g.expose().as_bytes().to_vec()));
                }
                // A target that could not be read is not parsed.
                Col::Oversized { .. } | Col::Unreadable(_) => {
                    out.assoc_shape = Some(AssocTargetShape::Unparsed.code());
                }
                _ => {}
            }
            // The CHECK: a present association has a shape or a kind.
            if out.assoc_kind.is_none() && out.assoc_shape.is_none() {
                out.assoc_shape = Some(AssocTargetShape::Unparsed.code());
            }
        }
        None => {
            // No association, unless the type could not be read: then
            // whether there is one is not determinable.
            if matches!(raw_type, Col::Oversized { .. } | Col::Unreadable(_)) {
                out.assoc_present = AssocPresent::Undeterminable.code();
            }
        }
    }
    out
}

/// `message.sort_key` (spec §6.5): dated messages by their instant, then
/// undated ones in the source order of the earliest-ranked session. The
/// one encoder is [`crate::sortkey`], which the bulk generator and the
/// viewer's seeks share.
fn sort_key(date: Option<&Cell>, first_rank: &Rank, first_src_rowid: i64) -> Vec<u8> {
    crate::sortkey::encode(
        date.and_then(Cell::time_utc_ns),
        first_rank.as_bytes(),
        first_src_rowid,
    )
}

// ---------------------------------------------------------------------------
// Body versions
// ---------------------------------------------------------------------------

/// The derived columns of one body version.
#[derive(Clone, PartialEq, Eq)]
struct BodyDerived {
    attributed_outcome: i64,
    attributed_error: Option<i64>,
    run_issue: Option<i64>,
    unknown_attr_keys: Option<i64>,
    unknown_attr_classes: Option<i64>,
    display_text: Option<Zeroizing<String>>,
    display_source: Option<DisplaySource>,
    display_none_reason: Option<DisplayNoneReason>,
    sources_differ: Option<i64>,
    summary_outcome: i64,
    otr: Option<i64>,
    ec_state: Option<i64>,
    rp_state: Option<i64>,
    has_tmp: Option<i64>,
    summary_unknown_keys: Option<i64>,
    edit_label: (i64, Option<i64>, Option<i64>),
}

/// One `message_body_part` row.
struct PartRow {
    part_index: Option<i64>,
    utf16: (i64, i64),
    utf8: (i64, i64),
    attachment_guid: Option<Zeroizing<Vec<u8>>>,
    mention: Option<Zeroizing<Vec<u8>>>,
    flags: i64,
    keys_present: i64,
    text_effect: Option<i64>,
}

/// One `message_edit_entry` row.
struct EditRow {
    part: i64,
    ordinal: i64,
    at_kind: Option<i64>,
    at_raw: Raw,
    at_utc_ns: Option<i64>,
    body_present: bool,
}

/// One observed body: its raw cells, hash, derived columns and child rows.
struct BodyObs {
    text: Cell,
    attributed: Cell,
    summary: Cell,
    raw_hash: [u8; 32],
    derived: BodyDerived,
    parts: Vec<PartRow>,
    edits: Vec<EditRow>,
    unsent: Vec<i64>,
}

fn ts_code(k: TsErrorKind) -> TsErrorCode {
    match k {
        TsErrorKind::UnsupportedHeader => TsErrorCode::UnsupportedHeader,
        TsErrorKind::Truncated => TsErrorCode::Truncated,
        TsErrorKind::UnexpectedTag => TsErrorCode::UnexpectedTag,
        TsErrorKind::BadReference => TsErrorCode::BadReference,
        TsErrorKind::UnsupportedType => TsErrorCode::UnsupportedType,
        TsErrorKind::TooDeep => TsErrorCode::TooDeep,
        TsErrorKind::Budget => TsErrorCode::Budget,
        TsErrorKind::MaterialisationBudget => TsErrorCode::MaterialisationBudget,
        TsErrorKind::TrailingBytes => TsErrorCode::TrailingBytes,
        TsErrorKind::NotAttributedString => TsErrorCode::NotAttributedString,
        TsErrorKind::InvalidUtf8 => TsErrorCode::InvalidUtf8,
        TsErrorKind::RunLengthMismatch => TsErrorCode::RunLengthMismatch,
        TsErrorKind::RunOverflow => TsErrorCode::RunOverflow,
        TsErrorKind::RunSplitsSurrogate => TsErrorCode::RunSplitsSurrogate,
        TsErrorKind::BadRunIndex => TsErrorCode::BadRunIndex,
    }
}

fn field_code<T>(f: &FieldOutcome<T>) -> i64 {
    match f {
        FieldOutcome::Missing => FieldCode::Missing.code(),
        FieldOutcome::Value(_) => FieldCode::Value.code(),
        FieldOutcome::Bad(_) => FieldCode::Bad.code(),
    }
}

fn edit_label_code(l: EditLabel) -> (i64, Option<i64>, Option<i64>) {
    let n = |v: u32| Some(i64::from(v));
    let (code, a, b) = match l {
        EditLabel::NotRecorded => (EditLabelCode::NotRecorded, None, None),
        EditLabel::SummaryUnreadable => (EditLabelCode::SummaryUnreadable, None, None),
        EditLabel::UnsentPartsAndEditHistory {
            unsent,
            edited_parts,
        } => (
            EditLabelCode::UnsentPartsAndEditHistory,
            n(unsent),
            n(edited_parts),
        ),
        EditLabel::UnsentParts { parts } => (EditLabelCode::UnsentParts, n(parts), None),
        EditLabel::EditHistory { parts, max_entries } => {
            (EditLabelCode::EditHistory, n(parts), n(max_entries))
        }
        EditLabel::DateEditedNoBody => (EditLabelCode::DateEditedNoBody, None, None),
        EditLabel::DateEditedOnly => (EditLabelCode::DateEditedOnly, None, None),
        EditLabel::NoEditRecorded => (EditLabelCode::NoEditRecorded, None, None),
        EditLabel::Undetermined => (EditLabelCode::Undetermined, None, None),
        _ => (EditLabelCode::Undeterminable, None, None),
    };
    (code.code(), a, b)
}

fn bits<T: Copy>(set: impl Iterator<Item = T>, name: fn(T) -> &'static str, table: &[&str]) -> i64 {
    let mut out = 0i64;
    for x in set {
        if let Some(i) = table.iter().position(|n| *n == name(x)) {
            out |= 1i64
                .checked_shl(u32::try_from(i).unwrap_or(63))
                .unwrap_or(0);
        }
    }
    out
}

fn flag_bits(f: AttrFlags) -> i64 {
    bits(f.iter(), |x| x.name(), &ATTR_FLAG_BITS)
}

fn key_bits(k: AttrKeysPresent) -> i64 {
    bits(k.iter(), |x| x.name(), &ATTR_KEY_BITS)
}

fn shared_text(
    b: &AttributedBody,
    id: Option<oma_messages::ValueId>,
) -> Option<Zeroizing<Vec<u8>>> {
    match b.values.get(id?.index())? {
        SharedValue::Text(t) => Some(Zeroizing::new(t.expose().as_bytes().to_vec())),
        _ => None,
    }
}

fn wide(v: u32) -> i64 {
    i64::from(v)
}

/// The decoded parts of a body (`message_body_part`).
fn part_rows(b: &AttributedBody) -> Vec<PartRow> {
    b.parts
        .iter()
        .map(|p| PartRow {
            part_index: p.part_index,
            utf16: (wide(p.utf16.start), wide(p.utf16.end)),
            utf8: (wide(p.utf8.start), wide(p.utf8.end)),
            attachment_guid: shared_text(b, p.attachment_guid),
            mention: shared_text(b, p.mention),
            flags: flag_bits(p.flags),
            keys_present: key_bits(p.keys_present),
            text_effect: p.text_effect,
        })
        .collect()
}

/// The edit entries (`ec`) and unsent parts (`rp`) of a parsed summary.
fn summary_rows(info: &SummaryInfo) -> (Vec<EditRow>, Vec<i64>) {
    let mut edits = Vec::new();
    if let FieldOutcome::Value(parts) = &info.ec {
        for p in parts {
            for (ordinal, e) in (0i64..).zip(&p.entries) {
                let (at_kind, at_raw, at_utc_ns) = match &e.at {
                    None => (None, Raw::Null, None),
                    Some(t) => {
                        let (kind, raw) = match t.raw {
                            SummaryTimeRaw::Integer(i) => (SummaryTimeKind::Integer, Raw::Int(i)),
                            SummaryTimeRaw::Real(f) => {
                                (SummaryTimeKind::Real, Raw::Real(f.to_bits()))
                            }
                            // A plist date: the instant is the value.
                            _ => (SummaryTimeKind::Date, Raw::Null),
                        };
                        let utc = match t.interp {
                            TimeInterp::Seconds { unix_ns }
                            | TimeInterp::Nanoseconds { unix_ns } => Some(unix_ns),
                            _ => None,
                        };
                        (Some(kind.code()), raw, utc)
                    }
                };
                edits.push(EditRow {
                    part: i64::from(p.part),
                    ordinal,
                    at_kind,
                    at_raw,
                    at_utc_ns,
                    body_present: e.body_present,
                });
            }
        }
    }
    let unsent = info
        .rp
        .value()
        .map(|v| v.iter().map(|p| i64::from(*p)).collect())
        .unwrap_or_default();
    (edits, unsent)
}

/// The body of `r`: raw cells (the raw bytes the reader kept), the hash,
/// the derived columns (D58 display text) and the child rows.
fn body_obs(r: &MessageRecord) -> rusqlite::Result<BodyObs> {
    let raw_value = |raw: &Option<oma_messages::Redacted<oma_messages::RawBlob>>| {
        raw.as_ref()
            .map(|b| {
                Cell::blob(&Col::Value(oma_messages::Redacted::new(
                    oma_messages::RawBlob::from_bytes(b.expose().as_bytes()),
                )))
            })
            .ok_or_else(|| bad("raw body bytes not kept"))
    };
    let mut text = Cell::text(&r.body.text_column);
    let (mut attributed, outcome) = match &r.body.attributed {
        AttributedOutcome::Absent => (Cell::blob(&Col::Absent), AttrCode::Absent),
        AttributedOutcome::Null => (Cell::blob(&Col::Null), AttrCode::Null),
        AttributedOutcome::Oversized { bytes } => (
            Cell::blob(&Col::Oversized { bytes: *bytes }),
            AttrCode::Oversized,
        ),
        AttributedOutcome::Unreadable(k) => {
            (Cell::blob(&Col::Unreadable(*k)), AttrCode::Unreadable)
        }
        AttributedOutcome::NotDecoded => (raw_value(&r.raw_attributed_body)?, AttrCode::NotDecoded),
        AttributedOutcome::Decoded(_) => (raw_value(&r.raw_attributed_body)?, AttrCode::Decoded),
        AttributedOutcome::Failed(_) => {
            (raw_value(&r.raw_attributed_body)?, AttrCode::DecodeFailed)
        }
        _ => (raw_value(&r.raw_attributed_body)?, AttrCode::NotDecoded),
    };
    let mut summary = match (&r.summary, &r.raw_message_summary_info) {
        (_, Some(_)) => raw_value(&r.raw_message_summary_info)?,
        (Col::Absent, None) => Cell::blob(&Col::Absent),
        (Col::Null, None) => Cell::blob(&Col::Null),
        (Col::Oversized { bytes }, None) => Cell::blob(&Col::Oversized { bytes: *bytes }),
        (Col::Unreadable(k), None) => Cell::blob(&Col::Unreadable(*k)),
        (Col::Value(_), None) => return Err(bad("raw summary bytes not kept")),
    };
    // The raw values of unreadable body cells (R2).
    for ri in &r.raw_issues {
        match ri.column.name() {
            "text" => text.keep_raw(&ri.raw),
            "attributedBody" => attributed.keep_raw(&ri.raw),
            "message_summary_info" => summary.keep_raw(&ri.raw),
            _ => {}
        }
    }
    let mut enc = Vec::new();
    text.fp_encode(BODY_TEXT, &mut enc);
    attributed.fp_encode(BODY_ATTRIBUTED, &mut enc);
    summary.fp_encode(BODY_SUMMARY, &mut enc);
    let mut h = Sha256::new();
    h.update(BODY_HASH_DOMAIN);
    h.update(&enc);
    let raw_hash: [u8; 32] = h.finalize().into();

    let decoded = match &r.body.attributed {
        AttributedOutcome::Decoded(b) => Some(b.as_ref()),
        _ => None,
    };
    let text_bytes = text.value.as_ref().and_then(|v| v.as_bytes());
    let ab_text = decoded.map(|b| b.text.expose().as_str());
    let mut d = BodyDerived {
        attributed_outcome: outcome.code(),
        attributed_error: match &r.body.attributed {
            AttributedOutcome::Failed(k) => Some(ts_code(*k).code()),
            _ => None,
        },
        run_issue: decoded
            .and_then(|b| b.run_issue)
            .map(|i| ts_code(i.kind).code()),
        unknown_attr_keys: decoded.map(|b| i64::from(b.unknown_attribute_keys)),
        unknown_attr_classes: decoded.map(|b| i64::from(b.unknown_attribute_classes)),
        display_text: None,
        display_source: None,
        display_none_reason: None,
        sources_differ: match (ab_text, text_bytes) {
            (Some(a), Some(t)) => Some(i64::from(a.as_bytes() != t)),
            _ => None,
        },
        summary_outcome: match &r.summary {
            Col::Absent => SummaryOutcome::Absent,
            Col::Null => SummaryOutcome::Null,
            Col::Oversized { .. } => SummaryOutcome::Oversized,
            Col::Unreadable(_) => SummaryOutcome::Unreadable,
            Col::Value(_) => SummaryOutcome::Parsed,
        }
        .code(),
        otr: None,
        ec_state: None,
        rp_state: None,
        has_tmp: None,
        summary_unknown_keys: None,
        edit_label: edit_label_code(r.edit_label),
    };
    // D58.
    if let Some(t) = ab_text.filter(|t| !t.is_empty()) {
        d.display_text = Some(Zeroizing::new(t.to_owned()));
        d.display_source = Some(DisplaySource::AttributedBody);
    } else if let Some(b) = text_bytes.filter(|b| !b.is_empty()) {
        match std::str::from_utf8(b) {
            Ok(s) => {
                d.display_text = Some(Zeroizing::new(s.to_owned()));
                d.display_source = Some(DisplaySource::Text);
            }
            Err(_) => {
                d.display_text = Some(Zeroizing::new(String::from_utf8_lossy(b).into_owned()));
                d.display_source = Some(DisplaySource::TextLossy);
            }
        }
    } else if attributed.value.is_some()
        || matches!(
            attributed.st,
            Some(ValueState::Oversized | ValueState::Unreadable)
        )
    {
        d.display_none_reason = Some(DisplayNoneReason::AttributedBodyUnusable);
    } else if text_bytes.is_some() {
        d.display_none_reason = Some(DisplayNoneReason::TextEmpty);
    } else {
        d.display_none_reason = Some(DisplayNoneReason::NoSource);
    }
    let (mut edits, mut unsent) = (Vec::new(), Vec::new());
    if let Col::Value(info) = &r.summary {
        d.otr = info.otr.value().map(|n| i64::from(*n));
        d.ec_state = Some(field_code(&info.ec));
        d.rp_state = Some(field_code(&info.rp));
        d.has_tmp = Some(i64::from(info.has_tmp));
        d.summary_unknown_keys = Some(i64::from(info.unknown_keys));
        (edits, unsent) = summary_rows(info);
    }
    Ok(BodyObs {
        text,
        attributed,
        summary,
        raw_hash,
        derived: d,
        parts: decoded.map(part_rows).unwrap_or_default(),
        edits,
        unsent,
    })
}

/// The statements of the body versions, prepared once per batch.
struct BodyStmts<'c> {
    find: Statement<'c>,
    insert: Statement<'c>,
    rederive: Statement<'c>,
    relabel: Statement<'c>,
    part: Statement<'c>,
    edit: Statement<'c>,
    unsent: Statement<'c>,
    clear_parts: Statement<'c>,
    clear_edits: Statement<'c>,
    clear_unsent: Statement<'c>,
}

/// A stored body version, as the merge reads it.
struct StoredBody {
    id: i64,
    first_rank: Rank,
    last_rank: Rank,
    decoder_version: i64,
    edit_label: (i64, Option<i64>, Option<i64>),
}

/// A message's body version by its raw hash.
pub(crate) const BODY_FIND_SQL: &str =
    "SELECT id, first_rank, last_rank, decoder_version, edit_label, edit_label_a, edit_label_b \
     FROM message_body WHERE message_id = ?1 AND raw_hash = ?2";

impl<'c> BodyStmts<'c> {
    fn prepare(c: &'c Connection) -> rusqlite::Result<BodyStmts<'c>> {
        Ok(BodyStmts {
            find: c.prepare(BODY_FIND_SQL)?,
            insert: c.prepare(
                "INSERT INTO message_body (message_id, raw_hash, text_raw, text_raw_st, \
                     attributed_raw, attributed_raw_st, summary_raw, summary_raw_st, \
                     attributed_outcome, attributed_error, run_issue, unknown_attr_keys, \
                     unknown_attr_classes, display_text, display_source, display_none_reason, \
                     sources_differ, summary_outcome, otr, ec_state, rp_state, has_tmp, \
                     summary_unknown_keys, edit_label, edit_label_a, edit_label_b, \
                     decoder_version, first_session_id, first_rank, last_session_id, last_rank) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
                         ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?28, \
                         ?29) \
                 ON CONFLICT (message_id, raw_hash) DO NOTHING",
            )?,
            rederive: c.prepare(
                "UPDATE message_body SET attributed_outcome = ?2, attributed_error = ?3, \
                     run_issue = ?4, unknown_attr_keys = ?5, unknown_attr_classes = ?6, \
                     display_text = ?7, display_source = ?8, display_none_reason = ?9, \
                     sources_differ = ?10, summary_outcome = ?11, otr = ?12, ec_state = ?13, \
                     rp_state = ?14, has_tmp = ?15, summary_unknown_keys = ?16, \
                     edit_label = ?17, edit_label_a = ?18, edit_label_b = ?19, \
                     decoder_version = ?20 \
                 WHERE id = ?1",
            )?,
            relabel: c.prepare(
                "UPDATE message_body SET edit_label = ?2, edit_label_a = ?3, edit_label_b = ?4 \
                 WHERE id = ?1",
            )?,
            part: c.prepare(
                "INSERT INTO message_body_part (body_id, ordinal, part_index, utf16_start, \
                     utf16_end, utf8_start, utf8_end, attachment_guid, mention, flags, \
                     keys_present, text_effect) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
                 ON CONFLICT DO NOTHING",
            )?,
            edit: c.prepare(
                "INSERT INTO message_edit_entry (body_id, part, ordinal, at_kind, at_raw, \
                     at_utc_ns, body_present) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT DO NOTHING",
            )?,
            unsent: c.prepare(
                "INSERT INTO message_unsent_part (body_id, ordinal, part) VALUES (?1, ?2, ?3) \
                 ON CONFLICT DO NOTHING",
            )?,
            clear_parts: c.prepare("DELETE FROM message_body_part WHERE body_id = ?1")?,
            clear_edits: c.prepare("DELETE FROM message_edit_entry WHERE body_id = ?1")?,
            clear_unsent: c.prepare("DELETE FROM message_unsent_part WHERE body_id = ?1")?,
        })
    }

    fn find(&mut self, message: i64, hash: &[u8; 32]) -> rusqlite::Result<Option<StoredBody>> {
        self.find
            .query_row(rusqlite::params![message, hash.as_slice()], |r| {
                let rank = |i: usize| -> rusqlite::Result<Rank> {
                    let b: Vec<u8> = r.get(i)?;
                    Rank::from_slice(&b).ok_or_else(|| bad("stored body rank"))
                };
                Ok(StoredBody {
                    id: r.get(0)?,
                    first_rank: rank(1)?,
                    last_rank: rank(2)?,
                    decoder_version: r.get(3)?,
                    edit_label: (r.get(4)?, r.get(5)?, r.get(6)?),
                })
            })
            .optional()
    }

    fn children(&mut self, id: i64, b: &BodyObs) -> rusqlite::Result<()> {
        for (ordinal, p) in (0i64..).zip(&b.parts) {
            self.part.execute(rusqlite::params![
                id,
                ordinal,
                p.part_index,
                p.utf16.0,
                p.utf16.1,
                p.utf8.0,
                p.utf8.1,
                p.attachment_guid
                    .as_deref()
                    .map_or(Param::Null, |g| Param::Bytes(g)),
                p.mention
                    .as_deref()
                    .map_or(Param::Null, |g| Param::Bytes(g)),
                p.flags,
                p.keys_present,
                p.text_effect
            ])?;
        }
        for e in &b.edits {
            self.edit.execute(rusqlite::params![
                id,
                e.part,
                e.ordinal,
                e.at_kind,
                Param::Raw(&e.at_raw),
                e.at_utc_ns,
                i64::from(e.body_present)
            ])?;
        }
        for (ordinal, part) in (0i64..).zip(&b.unsent) {
            self.unsent.execute(rusqlite::params![id, ordinal, part])?;
        }
        Ok(())
    }
}

/// The derived parameters of a body, in `rederive` order (`?2`–`?20`).
fn body_params(d: &BodyDerived) -> Vec<Param<'_>> {
    vec![
        Param::Int(d.attributed_outcome),
        Param::opt_int(d.attributed_error),
        Param::opt_int(d.run_issue),
        Param::opt_int(d.unknown_attr_keys),
        Param::opt_int(d.unknown_attr_classes),
        d.display_text
            .as_ref()
            .map_or(Param::Null, |t| Param::Text(t.as_str())),
        Param::opt_int(d.display_source.map(DisplaySource::code)),
        Param::opt_int(d.display_none_reason.map(DisplayNoneReason::code)),
        Param::opt_int(d.sources_differ),
        Param::Int(d.summary_outcome),
        Param::opt_int(d.otr),
        Param::opt_int(d.ec_state),
        Param::opt_int(d.rp_state),
        Param::opt_int(d.has_tmp),
        Param::opt_int(d.summary_unknown_keys),
        Param::Int(d.edit_label.0),
        Param::opt_int(d.edit_label.1),
        Param::opt_int(d.edit_label.2),
        Param::Int(i64::from(DECODER_VERSION)),
    ]
}

fn count_new_body(c: &mut BodyCounts, d: &BodyDerived) {
    c.added = c.added.saturating_add(1);
    let n = match (d.display_source, d.display_none_reason) {
        (Some(DisplaySource::AttributedBody), _) => &mut c.display_attributed_body,
        (Some(DisplaySource::Text), _) => &mut c.display_text,
        (Some(DisplaySource::TextLossy), _) => &mut c.display_text_lossy,
        (None, Some(DisplayNoneReason::AttributedBodyUnusable)) => {
            &mut c.none_attributed_body_unusable
        }
        (None, Some(DisplayNoneReason::TextEmpty)) => &mut c.none_text_empty,
        (None, _) => &mut c.none_no_source,
    };
    *n = n.saturating_add(1);
    if d.sources_differ == Some(1) {
        c.sources_differ = c.sources_differ.saturating_add(1);
    }
    if d.attributed_outcome == AttrCode::DecodeFailed.code() {
        c.decode_failed = c.decode_failed.saturating_add(1);
    }
    if d.run_issue.is_some() {
        c.run_issues = c.run_issues.saturating_add(1);
    }
}

/// Insert or re-observe the body version `b` of message `message`; returns
/// its id. Body ranks are min/max by rank; the raw cells are fixed by the
/// hash. A version a newer decoder sees again is re-derived in place, and
/// a re-observation at a rank at least the version's `last_rank` refreshes
/// its edit label (which also reads `date_edited`).
#[allow(clippy::too_many_arguments)]
fn upsert_body(
    bs: &mut BodyStmts<'_>,
    ms: &mut MergeStmts<'_>,
    tx: &Connection,
    message: i64,
    b: &BodyObs,
    rank: &Rank,
    session: i64,
    counts: &mut BodyCounts,
) -> rusqlite::Result<i64> {
    if let Some(stored) = bs.find(message, &b.raw_hash)? {
        let mut sets: Vec<&str> = Vec::new();
        let mut p: Vec<Param<'_>> = Vec::new();
        if *rank > stored.last_rank {
            sets.push("last_rank = ?");
            p.push(Param::Bytes(rank.as_bytes()));
            sets.push("last_session_id = ?");
            p.push(Param::Int(session));
        }
        if *rank < stored.first_rank {
            sets.push("first_rank = ?");
            p.push(Param::Bytes(rank.as_bytes()));
            sets.push("first_session_id = ?");
            p.push(Param::Int(session));
        }
        if !sets.is_empty() {
            let numbered: Vec<String> = sets
                .iter()
                .enumerate()
                .map(|(i, s)| s.replace('?', &format!("?{}", i.saturating_add(1))))
                .collect();
            p.push(Param::Int(stored.id));
            let sql = format!(
                "UPDATE message_body SET {} WHERE id = ?{}",
                numbered.join(", "),
                p.len()
            );
            tx.execute(&sql, rusqlite::params_from_iter(p.iter()))?;
        }
        if stored.decoder_version != i64::from(DECODER_VERSION) {
            let mut params = vec![Param::Int(stored.id)];
            params.extend(body_params(&b.derived));
            bs.rederive
                .execute(rusqlite::params_from_iter(params.iter()))?;
            bs.clear_parts.execute([stored.id])?;
            bs.clear_edits.execute([stored.id])?;
            bs.clear_unsent.execute([stored.id])?;
            bs.children(stored.id, b)?;
            counts.rederived = counts.rederived.saturating_add(1);
        } else {
            if *rank >= stored.last_rank && stored.edit_label != b.derived.edit_label {
                let l = b.derived.edit_label;
                bs.relabel
                    .execute(rusqlite::params![stored.id, l.0, l.1, l.2])?;
            }
            counts.unchanged = counts.unchanged.saturating_add(1);
        }
        return Ok(stored.id);
    }
    fn cell_params(c: &Cell) -> [Param<'_>; 2] {
        [
            c.value.as_ref().map_or(Param::Null, Param::Val),
            Param::opt_int(c.st_code()),
        ]
    }
    let mut p: Vec<Param<'_>> = vec![Param::Int(message), Param::Bytes(&b.raw_hash)];
    p.extend(cell_params(&b.text));
    p.extend(cell_params(&b.attributed));
    p.extend(cell_params(&b.summary));
    let derived = body_params(&b.derived);
    p.extend(derived.iter().map(Param::borrow));
    p.push(Param::Int(session));
    p.push(Param::Bytes(rank.as_bytes()));
    let n = bs.insert.execute(rusqlite::params_from_iter(p.iter()))?;
    if n != 1 {
        return Err(bad("body insert"));
    }
    let id = tx.last_insert_rowid();
    for (code, cell) in [
        (BODY_TEXT, &b.text),
        (BODY_ATTRIBUTED, &b.attributed),
        (BODY_SUMMARY, &b.summary),
    ] {
        if !cell.issues.is_empty() {
            ms.write_issues(Entity::MessageBody, id, code, cell)?;
        }
    }
    bs.children(id, b)?;
    count_new_body(counts, &b.derived);
    Ok(id)
}

// ---------------------------------------------------------------------------
// The message row
// ---------------------------------------------------------------------------

/// A message row's stored derived columns.
struct StoredDerived {
    cell: CellDerived,
    handle_ref: Option<i64>,
    other_handle_ref: Option<i64>,
    sort_key: Vec<u8>,
    first_src_rowid: i64,
    derived_by: i64,
    current_body_id: Option<i64>,
}

/// The statements of the message rows beyond the merge engine's.
struct MessageStmts<'c> {
    by_key: Statement<'c>,
    by_unkeyed: Statement<'c>,
    stored_issues: Statement<'c>,
    insert: Statement<'c>,
    derived: Statement<'c>,
    handle: Statement<'c>,
    archived: Statement<'c>,
}

impl<'c> MessageStmts<'c> {
    fn prepare(c: &'c Connection, def: &TableDef) -> rusqlite::Result<MessageStmts<'c>> {
        Ok(MessageStmts {
            by_key: c.prepare(&def.select_sql(super::merge::KEY_GUID))?,
            by_unkeyed: c.prepare(&def.select_sql(super::merge::KEY_UNKEYED))?,
            stored_issues: c.prepare(super::merge::STORED_ISSUES_SQL)?,
            insert: c.prepare(&def.insert_sql())?,
            derived: c.prepare(
                "SELECT service_class, item_kind, assoc_present, assoc_kind, assoc_target_shape, \
                        assoc_target_guid, assoc_target_part, handle_ref, other_handle_ref, \
                        sort_key, first_src_rowid, derived_by, current_body_id \
                 FROM message WHERE id = ?1",
            )?,
            handle: c.prepare(
                "SELECT archive_id FROM import_rowid_map \
                 WHERE session_id = ?1 AND src = ?2 AND src_rowid = ?3",
            )?,
            archived: c.prepare("SELECT id FROM message WHERE key_kind = 1 AND guid = ?1")?,
        })
    }

    fn stored_derived(&mut self, id: i64) -> rusqlite::Result<StoredDerived> {
        self.derived.query_row([id], |r| {
            Ok(StoredDerived {
                cell: CellDerived {
                    service_class: r.get(0)?,
                    item_kind: r.get(1)?,
                    assoc_present: r.get(2)?,
                    assoc_kind: r.get(3)?,
                    assoc_shape: r.get(4)?,
                    assoc_guid: r.get::<_, Option<Vec<u8>>>(5)?.map(Zeroizing::new),
                    assoc_part: r.get(6)?,
                },
                handle_ref: r.get(7)?,
                other_handle_ref: r.get(8)?,
                sort_key: r.get(9)?,
                first_src_rowid: r.get(10)?,
                derived_by: r.get(11)?,
                current_body_id: r.get(12)?,
            })
        })
    }

    /// The archive handle a raw handle rowid names in this session: NULL
    /// for 0, a non-value and an unmapped rowid.
    fn resolve(&mut self, session: i64, cell: Option<&Cell>) -> rusqlite::Result<Option<i64>> {
        match cell.map(Cell::as_col_i64) {
            Some(Col::Value(n)) if n != 0 => self
                .handle
                .query_row(rusqlite::params![session, Entity::Handle.code(), n], |r| {
                    r.get(0)
                })
                .optional(),
            _ => Ok(None),
        }
    }
}

/// What one record did, for the counts.
enum Outcome {
    /// Merged or inserted.
    Written {
        inserted: bool,
        keyed: bool,
        applied: super::merge::Applied,
        rd: bool,
    },
    /// Recently Deleted and excluded.
    Excluded,
    /// Recently Deleted and already archived: re-observed only.
    NotUpdated,
}

impl Messages {
    #[allow(clippy::too_many_arguments)]
    fn write_one(
        &self,
        tx: &Connection,
        run: &Run,
        r: &MessageRecord,
        st: &mut MessageStmts<'_>,
        ms: &mut MergeStmts<'_>,
        maps: &mut MapStmts<'_>,
        bs: &mut BodyStmts<'_>,
        counts: &mut BodyCounts,
    ) -> rusqlite::Result<Outcome> {
        let def = &self.def;
        let cells = self.cells(r);
        if self.rd.is_excluded(r.rowid) {
            return Ok(Outcome::Excluded);
        }
        if self.rd.is_archived(r.rowid) {
            // The preflight found its key unique here and archived.
            let id: Option<i64> = match cells.get(GUID).and_then(guid_digest) {
                Some((_, key)) => st
                    .archived
                    .query_row([key.as_slice()], |row| row.get(0))
                    .optional()?,
                None => None,
            };
            let Some(id) = id else {
                return Ok(Outcome::Excluded);
            };
            maps.map(run.session, Entity::Message, r.rowid, id)?;
            maps.seen(Entity::Message, id, run.lineage, &run.rank, run.session)?;
            return Ok(Outcome::NotUpdated);
        }
        let body = body_obs(r)?;
        let keyed = cells
            .get(GUID)
            .and_then(guid_digest)
            .filter(|(d, _)| !run.dups.contains(Entity::Message, d))
            .map(|(_, k)| k);
        let (ident, found) = match keyed {
            Some(key) => {
                let found = st
                    .by_key
                    .query_row([key.as_slice()], |row| read_stored(def, row))
                    .optional()?;
                (Ident::Keyed { natural_key: None }, found)
            }
            None => {
                let mut enc = Vec::new();
                for (c, cell) in def.cols.iter().zip(&cells) {
                    cell.fp_encode(c.code, &mut enc);
                }
                encode(&mut enc, Field::Bytes(&body.raw_hash));
                let code = u8::try_from(Entity::Message.code()).unwrap_or(0);
                let fp = fingerprint(code, &enc);
                let occ = maps.next_occurrence(run.session, Entity::Message, &fp)?;
                let found = st
                    .by_unkeyed
                    .query_row(rusqlite::params![fp.as_slice(), occ], |row| {
                        read_stored(def, row)
                    })
                    .optional()?;
                (Ident::Unkeyed { fp, occ }, found)
            }
        };
        let rank = &run.rank;
        let (applied, inserted) = match found {
            None => {
                let d = derive_cells(&cells);
                let handle_ref = st.resolve(run.session, cells.get(HANDLE))?;
                let other_ref = st.resolve(run.session, cells.get(OTHER_HANDLE))?;
                let key = sort_key(cells.get(DATE), rank, r.rowid);
                let derived = [
                    Param::opt_int(d.service_class),
                    Param::Int(d.item_kind),
                    Param::Int(d.assoc_present),
                    Param::opt_int(d.assoc_kind),
                    Param::opt_int(d.assoc_shape),
                    d.assoc_guid
                        .as_deref()
                        .map_or(Param::Null, |g| Param::Bytes(g)),
                    Param::opt_int(d.assoc_part),
                    Param::opt_int(handle_ref),
                    Param::opt_int(other_ref),
                    Param::Bytes(&key),
                    Param::Int(r.rowid),
                    Param::Int(i64::from(DERIVER_VERSION)),
                ];
                let applied = insert(
                    def,
                    &mut st.insert,
                    ms,
                    tx,
                    &ident,
                    &cells,
                    &derived,
                    rank,
                    run.session,
                )?;
                let body_id =
                    upsert_body(bs, ms, tx, applied.id, &body, rank, run.session, counts)?;
                tx.execute(
                    "UPDATE message SET current_body_id = ?1 WHERE id = ?2",
                    rusqlite::params![body_id, applied.id],
                )?;
                (applied, true)
            }
            Some(mut stored) => {
                attach_issues(def, &mut stored, &mut st.stored_issues)?;
                let applied = merge(def, ms, tx, &stored, &cells, rank, run.session, |_, _| {
                    Vec::new()
                })?;
                let mut current = stored.cells.clone();
                for &i in &applied.changed {
                    if let (Some(slot), Some(obs)) = (current.get_mut(i), cells.get(i)) {
                        *slot = obs.clone();
                    }
                }
                let sd = st.stored_derived(stored.id)?;
                let takes = |i: usize| match (stored.cells.get(i), cells.get(i)) {
                    (Some(old), Some(obs)) => matches!(
                        decide(old, obs, *rank, stored.last_rank),
                        Decision::Replace { .. }
                    ),
                    _ => false,
                };
                let handle_ref = if takes(HANDLE) {
                    st.resolve(run.session, cells.get(HANDLE))?
                } else {
                    sd.handle_ref
                };
                let other_ref = if takes(OTHER_HANDLE) {
                    st.resolve(run.session, cells.get(OTHER_HANDLE))?
                } else {
                    sd.other_handle_ref
                };
                let (first_rank, first_src_rowid) = if *rank < stored.first_rank {
                    (*rank, r.rowid)
                } else {
                    (stored.first_rank, sd.first_src_rowid)
                };
                let d = derive_cells(&current);
                let key = sort_key(current.get(DATE), &first_rank, first_src_rowid);
                let body_id = upsert_body(bs, ms, tx, stored.id, &body, rank, run.session, counts)?;
                let current_body = if *rank >= stored.last_rank {
                    Some(body_id)
                } else {
                    sd.current_body_id
                };
                if current_body != sd.current_body_id {
                    counts.current_moved = counts.current_moved.saturating_add(1);
                }
                let mut sets: Vec<(&str, Param<'_>)> = Vec::new();
                let derived_changed = d != sd.cell
                    || handle_ref != sd.handle_ref
                    || other_ref != sd.other_handle_ref
                    || key != sd.sort_key
                    || first_src_rowid != sd.first_src_rowid;
                if d.service_class != sd.cell.service_class {
                    sets.push(("service_class", Param::opt_int(d.service_class)));
                }
                if d.item_kind != sd.cell.item_kind {
                    sets.push(("item_kind", Param::Int(d.item_kind)));
                }
                if d.assoc_present != sd.cell.assoc_present
                    || d.assoc_kind != sd.cell.assoc_kind
                    || d.assoc_shape != sd.cell.assoc_shape
                    || d.assoc_guid != sd.cell.assoc_guid
                    || d.assoc_part != sd.cell.assoc_part
                {
                    sets.push(("assoc_present", Param::Int(d.assoc_present)));
                    sets.push(("assoc_kind", Param::opt_int(d.assoc_kind)));
                    sets.push(("assoc_target_shape", Param::opt_int(d.assoc_shape)));
                    sets.push((
                        "assoc_target_guid",
                        d.assoc_guid
                            .as_deref()
                            .map_or(Param::Null, |g| Param::Bytes(g)),
                    ));
                    sets.push(("assoc_target_part", Param::opt_int(d.assoc_part)));
                }
                if handle_ref != sd.handle_ref {
                    sets.push(("handle_ref", Param::opt_int(handle_ref)));
                }
                if other_ref != sd.other_handle_ref {
                    sets.push(("other_handle_ref", Param::opt_int(other_ref)));
                }
                if key != sd.sort_key {
                    sets.push(("sort_key", Param::Bytes(&key)));
                }
                if first_src_rowid != sd.first_src_rowid {
                    sets.push(("first_src_rowid", Param::Int(first_src_rowid)));
                }
                if derived_changed || sd.derived_by != i64::from(DERIVER_VERSION) {
                    sets.push(("derived_by", Param::Int(i64::from(DERIVER_VERSION))));
                }
                if current_body != sd.current_body_id {
                    sets.push(("current_body_id", Param::opt_int(current_body)));
                }
                if !sets.is_empty() {
                    let cols: Vec<String> = sets
                        .iter()
                        .enumerate()
                        .map(|(i, (n, _))| format!("{n} = ?{}", i.saturating_add(1)))
                        .collect();
                    let mut p: Vec<Param<'_>> = sets.iter().map(|(_, v)| v.borrow()).collect();
                    p.push(Param::Int(stored.id));
                    let sql = format!(
                        "UPDATE message SET {} WHERE id = ?{}",
                        cols.join(", "),
                        p.len()
                    );
                    tx.execute(&sql, rusqlite::params_from_iter(p.iter()))?;
                }
                (applied, false)
            }
        };
        maps.map(run.session, Entity::Message, r.rowid, applied.id)?;
        maps.seen(Entity::Message, applied.id, run.lineage, rank, run.session)?;
        Ok(Outcome::Written {
            inserted,
            keyed: ident.is_keyed(),
            applied,
            rd: self.rd.is_rd(r.rowid),
        })
    }
}

impl PassWriter for Messages {
    type Record = MessageRecord;
    const PASS: Pass = Pass::Messages;
    const PHASE: ImportPhase = ImportPhase::ImportingMessages;
    const SOURCE: KnownTable = KnownTable::Message;

    fn rowid(r: &MessageRecord) -> i64 {
        r.rowid
    }

    fn size(r: &MessageRecord) -> u64 {
        let blob = |b: &Option<oma_messages::Redacted<oma_messages::RawBlob>>| {
            b.as_ref()
                .map_or(0, |b| u64::try_from(b.expose().len()).unwrap_or(u64::MAX))
        };
        let payload = r
            .payload_data
            .as_ref()
            .and_then(|c| c.value())
            .map_or(0, |b| u64::try_from(b.expose().len()).unwrap_or(u64::MAX));
        [
            text_len(&r.guid),
            text_len(&r.body.text_column),
            text_len(&r.subject),
            text_len(&r.group_title),
            blob(&r.raw_attributed_body),
            blob(&r.raw_message_summary_info),
            payload,
        ]
        .into_iter()
        .fold(256u64, u64::saturating_add)
    }

    fn read(
        &self,
        db: &SmsDb<'_>,
        _conn: &Connection,
        _run: &Run,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: &mut dyn FnMut(MessageRecord) -> ControlFlow<Brk>,
    ) -> Result<PassOutcome<Brk>, MessagesError> {
        db.for_each_message(from, &self.opts, None, ctx, f)
    }

    fn write(
        &self,
        tx: &Connection,
        run: &Run,
        records: &[MessageRecord],
        counts: &mut ImportCounts,
        issues: &mut IssueDeltas,
    ) -> rusqlite::Result<()> {
        let mut st = MessageStmts::prepare(tx, &self.def)?;
        let mut ms = MergeStmts::prepare(tx)?;
        let mut maps = MapStmts::prepare(tx)?;
        let mut bs = BodyStmts::prepare(tx)?;
        for r in records {
            let mut bodies = counts.bodies;
            let out = record(tx, || {
                self.write_one(
                    tx,
                    run,
                    r,
                    &mut st,
                    &mut ms,
                    &mut maps,
                    &mut bs,
                    &mut bodies,
                )
            })?;
            counts.bodies = bodies;
            let m = &mut counts.messages;
            match out {
                Outcome::Excluded => {
                    let rd = &mut counts.recently_deleted;
                    rd.excluded = rd.excluded.saturating_add(1);
                }
                Outcome::NotUpdated => {
                    let rd = &mut counts.recently_deleted;
                    rd.not_updated = rd.not_updated.saturating_add(1);
                }
                Outcome::Written {
                    inserted,
                    keyed,
                    applied: a,
                    rd,
                } => {
                    m.seen = m.seen.saturating_add(1);
                    if inserted {
                        m.inserted = m.inserted.saturating_add(1);
                    } else {
                        m.already_present = m.already_present.saturating_add(1);
                    }
                    if !keyed {
                        m.unkeyed = m.unkeyed.saturating_add(1);
                    }
                    let changed = u64::try_from(a.changed.len()).unwrap_or(u64::MAX);
                    m.cells_replaced = m.cells_replaced.saturating_add(changed);
                    m.not_applied = m.not_applied.saturating_add(a.not_applied);
                    m.history_added = m.history_added.saturating_add(a.history_added);
                    m.value_issue_cells = m.value_issue_cells.saturating_add(a.value_issue_cells);
                    issues.add(IssueCode::ObservationNotApplied, a.not_applied);
                    if rd {
                        let rd = &mut counts.recently_deleted;
                        rd.imported = rd.imported.saturating_add(1);
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cells_follow_the_column_list() {
        let m = Messages::new(RdPlan::default());
        let names: Vec<&str> = m.def.cols.iter().map(|c| c.name).collect();
        assert_eq!(names.len(), 41);
        for (i, name) in [
            (GUID, "guid"),
            (HANDLE, "handle_rowid"),
            (OTHER_HANDLE, "other_handle_rowid"),
            (SERVICE, "service"),
            (DATE, "date"),
            (ITEM_TYPE, "item_type"),
            (GROUP_ACTION_TYPE, "group_action_type"),
            (ASSOC_TYPE, "assoc_type"),
            (ASSOC_GUID, "assoc_guid_raw"),
            (ASSOC_EMOJI, "assoc_emoji"),
            (ASSOC_LOCATION, "assoc_range_location"),
            (ASSOC_LENGTH, "assoc_range_length"),
        ] {
            assert_eq!(names[i], name);
        }
        assert_eq!(m.def.index("payload_data"), Some(33));
        assert_eq!(m.def.index("was_downgraded"), Some(40));
        // Every renamed source column names an archive column.
        for s in [
            "handle_id",
            "other_handle",
            "associated_message_type",
            "associated_message_guid",
            "associated_message_emoji",
            "associated_message_range_location",
            "associated_message_range_length",
        ] {
            assert!(m.def.index(rename(s)).is_some(), "{s}");
        }
        let body = [BODY_TEXT, BODY_ATTRIBUTED, BODY_SUMMARY];
        for (code, name) in body
            .iter()
            .zip(["text_raw", "attributed_raw", "summary_raw"])
        {
            let c = crate::schema::codes::ArchiveColumn::from_code(*code).expect("code");
            assert_eq!((c.entity, c.name), (Entity::MessageBody, name));
        }
    }

    #[test]
    fn sort_keys_put_dated_before_undated_in_order() {
        let r1 = Rank::new(Some(5), &[1u8; 32]);
        let r2 = Rank::new(Some(6), &[0u8; 32]);
        let date = |raw: i64| Cell::time(&Col::Value(oma_messages::CocoaTime::classify(raw)));
        let early = sort_key(Some(&date(700_000_000)), &r2, 9);
        let late = sort_key(Some(&date(700_000_001)), &r1, 1);
        let undated_a = sort_key(Some(&date(0)), &r1, i64::MAX);
        let undated_b = sort_key(Some(&date(-5)), &r2, i64::MIN);
        let undated_c = sort_key(None, &r2, 0);
        assert!(early < late && late < undated_a);
        assert!(undated_a < undated_b && undated_b < undated_c);
        assert_eq!(early.len(), 9);
        assert_eq!(undated_a.len(), 50);
        // Before the Unix epoch still sorts by instant.
        let before = sort_key(Some(&date(1)), &r1, 0);
        assert!(before < early);
        // The viewer's seek key for the same instant is the import's key
        // (M5 §3.3): Cocoa seconds plus 978,307,200, in nanoseconds.
        let ns = (700_000_000i64 + 978_307_200) * 1_000_000_000;
        assert_eq!(early, crate::sortkey::dated(ns).to_vec());
        assert_eq!(
            crate::sortkey::decode_dated(&late),
            Some(ns + 1_000_000_000)
        );
        assert!(!crate::sortkey::is_dated(&undated_a));
    }

    fn text(s: &[u8]) -> Cell {
        Cell::text(&Col::Value(oma_messages::Redacted::new(
            oma_messages::SqlText::from_bytes(s),
        )))
    }

    /// The association columns re-derive from cells as the reader derives
    /// them from the row.
    #[test]
    fn associations_derive_from_cells() {
        let mut cells: Vec<Cell> = (0..41).map(|_| Cell::int(&Col::Absent)).collect();
        cells[ITEM_TYPE] = Cell::int(&Col::Value(0));
        cells[ASSOC_TYPE] = Cell::int(&Col::Value(2001));
        cells[ASSOC_GUID] = text(b"p:3/SYNTH-MSG-GUID-0001");
        let d = derive_cells(&cells);
        assert_eq!(d.item_kind, ItemKindCode::Message.code());
        assert_eq!(d.assoc_present, AssocPresent::Yes.code());
        assert_eq!(d.assoc_kind, Some(AssocKindCode::TapbackLike.code()));
        assert_eq!(d.assoc_shape, Some(AssocTargetShape::Part.code()));
        assert_eq!(d.assoc_part, Some(3));
        assert_eq!(
            d.assoc_guid.as_deref().map(Vec::as_slice),
            Some(&b"SYNTH-MSG-GUID-0001"[..])
        );
        // '' and NULL are no association; an unreadable type is not
        // determinable.
        cells[ASSOC_TYPE] = Cell::int(&Col::Value(0));
        cells[ASSOC_GUID] = text(b"");
        assert_eq!(derive_cells(&cells).assoc_present, AssocPresent::No.code());
        cells[ASSOC_TYPE] = Cell::int(&Col::Unreadable(oma_messages::IssueKind::TypeMismatch));
        cells[ASSOC_GUID] = Cell::text(&Col::Null);
        let d = derive_cells(&cells);
        assert_eq!(d.assoc_present, AssocPresent::Undeterminable.code());
        assert_eq!((d.assoc_kind, d.assoc_shape), (None, None));
        // An unreadable target with no readable type still satisfies the
        // CHECK: a shape.
        cells[ASSOC_GUID] = Cell::text(&Col::Oversized { bytes: 999 });
        let d = derive_cells(&cells);
        assert_eq!(d.assoc_present, AssocPresent::Yes.code());
        assert_eq!(d.assoc_shape, Some(AssocTargetShape::Unparsed.code()));
        // Removals and unknown kinds.
        assert_eq!(
            assoc_kind_code(AssociatedKind::from_raw(3007)),
            AssocKindCode::RemovedSticker
        );
        assert_eq!(
            assoc_kind_code(AssociatedKind::from_raw(77)),
            AssocKindCode::Unknown
        );
    }

    #[test]
    fn edit_labels_have_codes_and_numbers() {
        assert_eq!(
            edit_label_code(EditLabel::EditHistory {
                parts: 2,
                max_entries: 3
            }),
            (5, Some(2), Some(3))
        );
        assert_eq!(
            edit_label_code(EditLabel::UnsentParts { parts: 1 }),
            (4, Some(1), None)
        );
        assert_eq!(edit_label_code(EditLabel::NoEditRecorded), (8, None, None));
        for k in TsErrorKind::ALL {
            assert_ne!(ts_code(k), TsErrorCode::Unrecognised);
        }
    }
}
