//! `ExtractionReport`: aggregates only (M3 §13, D31, D43).
//!
//! The report holds counts, raw codes in capped histograms, closed kinds,
//! and one data-derived value besides counts: the date range of
//! `message.date`, which is shown, kept in memory, never persisted and never
//! logged (`DateCounts`'s `Debug` withholds it). There are no strings from
//! the database.
//!
//! - **Every counter that depends on an optional table or column is an
//!   `Option<u64>`.** `None` means "not recorded by this database" (or, for
//!   an aggregate, "over its op budget", which also sets the qualifier
//!   `AggregatesIncomplete`). It is never shown as 0 (I5). Counters that
//!   need nothing optional are `u64`.
//! - Counters saturate.
//! - For `Unsupported`, `Damaged` and `Incomplete` the report carries the
//!   status, the schema facts the scan and probe established, the database
//!   facts and the times; every other group is `None` ("not read").
//!
//! **Counting rules for internal objects** (also in `schema/scan.rs`):
//! `sqlite_autoindex_*` indexes are excluded from `indexes`;
//! `sqlite_sequence` and `sqlite_stat*` are counted only in
//! `internal_objects` and `stat_tables`; `unknown_tables` counts ordinary
//! tables outside the known list and outside `sqlite_*`.

use std::fmt;
use std::time::{Duration, SystemTime};

use oma_backup::{OpenMode, TextEncoding, WalFacts, WalOutcome};

use crate::attachments::{NotAttemptedReason, PathClass, ATTACHMENT_LOOKUP_DOMAIN};
use crate::codes::{ItemKind, Reaction, ServiceClass};
use crate::schema::{
    Capabilities, DamageReason, Features, IncompleteReason, KnownColumn, KnownTable,
    UnsupportedReason,
};
use crate::time::TimeInterp;
use crate::typedstream::TsErrorKind;
use crate::value::ValueIssues;

/// `RANGE_FLOOR`: 2007-01-01T00:00:00Z in Unix nanoseconds. Message dates
/// before it are left out of the date range and counted. It is a display
/// choice, not a fact from any source.
pub const RANGE_FLOOR_UNIX_NS: i64 = 1_167_609_600_000_000_000;

/// The most distinct values a raw-value histogram keeps; later values go
/// to `other`.
pub const HISTOGRAM_CAP: usize = 16;

pub(crate) fn bump(n: &mut u64) {
    *n = n.saturating_add(1);
}

pub(crate) fn bump_opt(n: &mut Option<u64>) {
    if let Some(v) = n {
        bump(v);
    }
}

pub(crate) fn add_opt(n: &mut Option<u64>, by: u64) {
    if let Some(v) = n {
        *v = v.saturating_add(by);
    }
}

/// `Some(0)` when recorded, `None` otherwise.
pub(crate) fn zero_if(recorded: bool) -> Option<u64> {
    recorded.then_some(0)
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// The outcome of a check (D43).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExtractionStatus {
    /// The database has the required minimum and every row was read. Each
    /// qualifier is a stated limit; with any qualifier the UI never says
    /// just "can be read".
    Readable {
        /// The limits that apply, in a fixed order.
        qualifiers: Vec<Qualifier>,
    },
    /// The integrity check failed, or a pass streamed a different number of
    /// rows than `count(*)`. Nothing more is reported.
    Damaged(DamageReason),
    /// The database lacks the required minimum, has a virtual table, has an
    /// unsafe rowid shape, or exceeds the schema limits.
    Unsupported(Vec<UnsupportedReason>),
    /// The integrity check did not finish within its budget.
    Incomplete(IncompleteReason),
}

/// A stated limit on a readable result (D43).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Qualifier {
    /// Ordinary tables this version does not know.
    UnknownTables(u32),
    /// Columns of known tables this version does not know.
    UnknownColumns(u32),
    /// Known columns whose name differs only in case (treated as absent).
    WrongCaseColumns(u32),
    /// The backup lists a `-wal` that could not be provided: the newest
    /// messages may be missing.
    WalSkipped,
    /// No `-wal` is listed, but the database header says WAL mode: the
    /// newest messages may be missing.
    WalStateUnknown,
    /// The staged `-wal` has frames that failed validation: the newest
    /// messages may be missing.
    WalFramesInvalid,
    /// The backup changed during the check: the counts may come from an
    /// inconsistent copy. M4 treats this as fatal.
    SourceChanged,
    /// At least one aggregate is missing (over its budget, or a GUID set
    /// reached its cap).
    AggregatesIncomplete,
    /// Some values were over their cap or unreadable and were not counted.
    ValuesWithheld,
    /// The database is not UTF-8; SQLite converted its text.
    NonUtf8Encoding,
    /// The staged size differs from Manifest's `Size`.
    SizeMismatch,
}

// ---------------------------------------------------------------------------
// Schema and database facts
// ---------------------------------------------------------------------------

/// What the scan and probe established, per known table and overall.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SchemaReport {
    /// One entry per known table, in [`KnownTable::ALL`] order; empty when
    /// the probe did not run.
    pub tables: Vec<TableReport>,
    /// Ordinary tables outside the known list and outside `sqlite_*`.
    pub unknown_tables: u32,
    /// Views.
    pub views: u32,
    /// Triggers.
    pub triggers: u32,
    /// Indexes, without `sqlite_autoindex_*`.
    pub indexes: u32,
    /// `sqlite_stat*` tables.
    pub stat_tables: u32,
    /// `sqlite_sequence` and other internal objects.
    pub internal_objects: u32,
    /// `PRAGMA encoding`, when a connection opened.
    pub encoding: Option<TextEncoding>,
    /// `_ClientVersion`, raw (its mapping to iOS versions is
    /// **UNVERIFIED**).
    pub client_version: Option<i64>,
    /// Whether `_ClientVersion` parsed.
    pub client_version_parsed: bool,
    /// Features derived from the present columns (never an iOS version).
    pub features: Features,
}

/// One known table.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TableReport {
    /// The table.
    pub table: KnownTable,
    /// Exactly one ordinary table has its name.
    pub present: bool,
    /// `count(*)`, when present.
    pub rows: Option<u64>,
    /// Known columns not present (by exact name, not generated), when the
    /// table is present.
    pub missing_columns: Vec<KnownColumn>,
    /// Columns not on the known list.
    pub unknown_columns: u32,
    /// Known columns that are hidden or generated.
    pub generated_columns: u32,
    /// Known columns whose name differs only in case.
    pub wrong_case_columns: u32,
}

impl SchemaReport {
    pub(crate) fn from_capabilities(caps: &Capabilities) -> Self {
        let tables = caps
            .tables
            .iter()
            .map(|tc| TableReport {
                table: tc.table,
                present: tc.present,
                rows: tc.rows,
                missing_columns: if tc.present {
                    tc.table
                        .known_columns()
                        .filter(|c| !tc.present_columns.contains(*c))
                        .collect()
                } else {
                    Vec::new()
                },
                unknown_columns: tc.unknown_columns,
                generated_columns: tc.generated_columns,
                wrong_case_columns: tc.wrong_case_columns,
            })
            .collect();
        SchemaReport {
            tables,
            unknown_tables: caps.unknown_tables,
            views: caps.views,
            triggers: caps.triggers,
            indexes: caps.indexes,
            stat_tables: caps.stat_tables,
            internal_objects: caps.internal_objects,
            encoding: caps.encoding,
            client_version: caps.client_version,
            client_version_parsed: caps.client_version_parsed,
            features: caps.features,
        }
    }

    /// One known table's entry.
    pub fn table(&self, t: KnownTable) -> Option<&TableReport> {
        self.tables.iter().find(|r| r.table == t)
    }

    /// Unknown columns summed over the known tables.
    pub fn unknown_columns(&self) -> u32 {
        self.tables
            .iter()
            .fold(0u32, |n, t| n.saturating_add(t.unknown_columns))
    }

    /// Wrong-case columns summed over the known tables.
    pub fn wrong_case_columns(&self) -> u32 {
        self.tables
            .iter()
            .fold(0u32, |n, t| n.saturating_add(t.wrong_case_columns))
    }
}

/// The integrity check's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IntegrityOutcome {
    /// `quick_check` returned `ok`.
    Ok,
    /// It returned anything else (the text is never kept).
    Problems,
    /// It ran out of its op budget.
    OverBudget,
}

/// The staged `-wal`. "Staged" means copied beside the database; that
/// SQLite applied its frames is inferred, never observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct WalReport {
    /// The backup lists a `-wal`.
    pub listed: bool,
    /// What happened to it.
    pub outcome: WalOutcome,
    /// The staged length, when staged.
    pub bytes: Option<u64>,
    /// Header bytes 18–19 of the main file are both 2.
    pub header_says_wal: Option<bool>,
    /// The staged length equals Manifest's `Size`, when staged and known.
    pub size_matches: Option<bool>,
    /// Header, salt and checksum facts, when staged.
    pub facts: Option<WalFacts>,
}

/// Facts about the staged database itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DbFacts {
    /// How the staged copy was opened; `None` when no connection opened.
    pub open_mode: Option<OpenMode>,
    /// The `-wal`.
    pub wal: WalReport,
    /// The main file's staged length equals Manifest's `Size`, when known.
    pub size_matches: Option<bool>,
    /// `quick_check`, when it ran.
    pub integrity: Option<IntegrityOutcome>,
    /// `PRAGMA page_size`, when a connection opened.
    pub page_size: Option<u64>,
    /// Bytes staged: the main file plus the `-wal`.
    pub staged_bytes: u64,
    /// The staged copy was deleted straight after the row pass. When
    /// `false`, the backup's `close()` removes it.
    pub plaintext_removed_after_row_pass: bool,
    /// The backup's stamped files changed during the check.
    pub source_changed_during_check: bool,
}

// ---------------------------------------------------------------------------
// Histograms
// ---------------------------------------------------------------------------

/// Counts per service class. Raw values are never echoed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServiceCounts {
    /// `iMessage`.
    pub imessage: u64,
    /// `SMS`.
    pub sms: u64,
    /// `RCS`.
    pub rcs: u64,
    /// `iMessageLite` (the literal string; no meaning claimed).
    pub imessage_lite: u64,
    /// The empty string.
    pub empty: u64,
    /// Any other text.
    pub other: u64,
    /// NULL, unreadable or oversized.
    pub not_text: u64,
}

impl ServiceCounts {
    pub(crate) fn add(&mut self, c: Option<ServiceClass>) {
        bump(match c {
            Some(ServiceClass::IMessage) => &mut self.imessage,
            Some(ServiceClass::Sms) => &mut self.sms,
            Some(ServiceClass::Rcs) => &mut self.rcs,
            Some(ServiceClass::IMessageLite) => &mut self.imessage_lite,
            Some(ServiceClass::Empty) => &mut self.empty,
            Some(_) => &mut self.other,
            None => &mut self.not_text,
        });
    }
}

/// A capped histogram of raw integer codes whose meanings are
/// **UNVERIFIED**: at most [`HISTOGRAM_CAP`] distinct values, in ascending
/// order, then `other`. Never a map keyed by attacker values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RawHistogram {
    /// `(value, count)`, ascending by value.
    pub entries: Vec<(i64, u64)>,
    /// Values seen after the cap was reached.
    pub other: u64,
    /// NULL values.
    pub null: u64,
    /// Unreadable or oversized values.
    pub unreadable: u64,
}

impl RawHistogram {
    pub(crate) fn add(&mut self, v: &crate::col::Col<i64>) {
        use crate::col::Col;
        match v {
            Col::Absent => {}
            Col::Null => bump(&mut self.null),
            Col::Unreadable(_) | Col::Oversized { .. } => bump(&mut self.unreadable),
            Col::Value(x) => match self.entries.binary_search_by_key(x, |e| e.0) {
                Ok(i) => {
                    if let Some(e) = self.entries.get_mut(i) {
                        bump(&mut e.1);
                    }
                }
                Err(i) if self.entries.len() < HISTOGRAM_CAP => self.entries.insert(i, (*x, 1)),
                Err(_) => bump(&mut self.other),
            },
        }
    }
}

/// How the values of one time column were classified (§8.1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnitHistogram {
    /// 0.
    pub zero: u64,
    /// Below 0.
    pub negative: u64,
    /// In the seconds window.
    pub seconds: u64,
    /// In the nanoseconds window.
    pub nanoseconds: u64,
    /// Outside both windows.
    pub ambiguous: u64,
    /// NULL (or, for edit entries, no `d`).
    pub null: u64,
    /// Not an integer.
    pub unreadable: u64,
}

impl UnitHistogram {
    pub(crate) fn add_interp(&mut self, t: TimeInterp) {
        bump(match t {
            TimeInterp::Zero => &mut self.zero,
            TimeInterp::Negative => &mut self.negative,
            TimeInterp::Seconds { .. } => &mut self.seconds,
            TimeInterp::Nanoseconds { .. } => &mut self.nanoseconds,
            _ => &mut self.ambiguous,
        });
    }

    pub(crate) fn add(&mut self, v: &crate::col::Col<crate::time::CocoaTime>) {
        use crate::col::Col;
        match v {
            Col::Absent => {}
            Col::Null => bump(&mut self.null),
            Col::Unreadable(_) | Col::Oversized { .. } => bump(&mut self.unreadable),
            Col::Value(t) => self.add_interp(t.interp),
        }
    }
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

/// `handle` rows: one per address and service pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct HandleCounts {
    /// Rows ("addresses": address/service pairs).
    pub rows: u64,
    /// By the class of `service`.
    pub by_service: Option<ServiceCounts>,
}

/// `chat` rows. Several rows may be one conversation; group-ness is not
/// counted (D39).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChatCounts {
    /// Rows.
    pub rows: u64,
    /// By the class of `service_name`.
    pub by_service: Option<ServiceCounts>,
    /// Chats linked to no handle (derived; the owner is never linked).
    pub with_no_other_handle: Option<u64>,
    /// Chats linked to exactly one distinct handle.
    pub with_one_other_handle: Option<u64>,
    /// Chats linked to two or more distinct handles.
    pub with_several_other_handles: Option<u64>,
    /// `is_archived` set.
    pub archived: Option<u64>,
    /// `is_filtered`, raw.
    pub is_filtered: Option<RawHistogram>,
    /// `style`, raw (meaning **UNVERIFIED**).
    pub style: Option<RawHistogram>,
    /// Rows with a non-NULL `properties` (presence only).
    pub with_properties: Option<u64>,
}

/// Counts per [`ItemKind`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ItemKindCounts {
    /// Ordinary messages.
    pub message: u64,
    /// Participant added.
    pub participant_added: u64,
    /// Derived "own number changed".
    pub own_number_changed: u64,
    /// Participant removed.
    pub participant_removed: u64,
    /// Group renamed.
    pub group_renamed: u64,
    /// Participant left.
    pub participant_left: u64,
    /// Group photo changed.
    pub group_photo_changed: u64,
    /// Group photo removed.
    pub group_photo_removed: u64,
    /// Background changed.
    pub background_changed: u64,
    /// Background removed.
    pub background_removed: u64,
    /// Legacy shared location.
    pub legacy_shared_location: u64,
    /// Audio kept.
    pub audio_kept: u64,
    /// SharePlay.
    pub share_play: u64,
    /// Unmapped combinations.
    pub unknown: u64,
}

impl ItemKindCounts {
    pub(crate) fn add(&mut self, k: &ItemKind) {
        bump(match k {
            ItemKind::Message => &mut self.message,
            ItemKind::ParticipantAdded => &mut self.participant_added,
            ItemKind::OwnNumberChanged => &mut self.own_number_changed,
            ItemKind::ParticipantRemoved => &mut self.participant_removed,
            ItemKind::GroupRenamed => &mut self.group_renamed,
            ItemKind::ParticipantLeft => &mut self.participant_left,
            ItemKind::GroupPhotoChanged => &mut self.group_photo_changed,
            ItemKind::GroupPhotoRemoved => &mut self.group_photo_removed,
            ItemKind::BackgroundChanged => &mut self.background_changed,
            ItemKind::BackgroundRemoved => &mut self.background_removed,
            ItemKind::LegacySharedLocation => &mut self.legacy_shared_location,
            ItemKind::AudioKept => &mut self.audio_kept,
            ItemKind::SharePlay => &mut self.share_play,
            _ => &mut self.unknown,
        });
    }
}

/// Counts per tapback reaction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReactionCounts {
    /// Love.
    pub love: u64,
    /// Like.
    pub like: u64,
    /// Dislike.
    pub dislike: u64,
    /// Laugh.
    pub laugh: u64,
    /// Emphasize.
    pub emphasize: u64,
    /// Question.
    pub question: u64,
    /// Emoji tapback.
    pub emoji: u64,
    /// Sticker tapback.
    pub sticker: u64,
}

impl ReactionCounts {
    pub(crate) fn add(&mut self, r: Reaction) {
        bump(match r {
            Reaction::Love => &mut self.love,
            Reaction::Like => &mut self.like,
            Reaction::Dislike => &mut self.dislike,
            Reaction::Laugh => &mut self.laugh,
            Reaction::Emphasize => &mut self.emphasize,
            Reaction::Question => &mut self.question,
            Reaction::Emoji => &mut self.emoji,
            _ => &mut self.sticker,
        });
    }
}

/// The form of association targets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TargetCounts {
    /// `p:N/<guid>`.
    pub part: u64,
    /// `bp:<guid>`.
    pub bubble: u64,
    /// A bare GUID (shape **UNVERIFIED**).
    pub bare: u64,
    /// A value that did not parse.
    pub unparsed: u64,
    /// NULL or empty (with a non-zero type).
    pub null: u64,
    /// Over the 256-byte cap.
    pub oversized: u64,
    /// Not text.
    pub unreadable: u64,
}

/// Associated rows: tapbacks, stickers and poll votes (§11.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AssociationCounts {
    /// 1000.
    pub sticker_legacy: u64,
    /// 2000–2007.
    pub tapback_added: ReactionCounts,
    /// 3000–3007.
    pub tapback_removed: ReactionCounts,
    /// 4000.
    pub poll_vote: u64,
    /// Type 0 with a target GUID.
    pub no_association_type: u64,
    /// Any other value (including 2 and 3).
    pub unknown_type: u64,
    /// The type is NULL, unreadable or absent.
    pub type_not_recorded: u64,
    /// Target forms.
    pub targets: TargetCounts,
    /// Parsed targets matching no `message.guid` (by 64-bit hash).
    pub target_not_found: Option<u64>,
}

/// Edit labels (§11.3); exactly one per row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct EditLabelCounts {
    /// Rule 1.
    pub not_recorded: u64,
    /// Rule 2.
    pub summary_unreadable: u64,
    /// Rule 3.
    pub unsent_parts_and_edit_history: u64,
    /// Rule 4.
    pub unsent_parts: u64,
    /// Rule 5.
    pub edit_history: u64,
    /// Rule 6 (heuristic, **UNVERIFIED**; never called "unsent").
    pub date_edited_no_body: u64,
    /// Rule 7.
    pub date_edited_only: u64,
    /// Rule 8.
    pub no_edit_recorded: u64,
    /// Rule 9.
    pub undetermined: u64,
}

/// `message_summary_info` outcomes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SummaryCounts {
    /// Parsed (individual fields may still be bad).
    pub parsed: u64,
    /// NULL.
    pub null: u64,
    /// Not a readable property list (invalid, over a budget, not a
    /// dictionary, or not a blob).
    pub unreadable: u64,
    /// Over the 1 MiB cap.
    pub oversized: u64,
    /// The `tmp` key is present (translations; not decoded).
    pub tmp_key_present: u64,
}

/// The state of a row's `text` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum TextState {
    /// The column does not exist.
    Absent,
    /// NULL.
    Null,
    /// The empty string.
    Empty,
    /// Non-empty.
    NonEmpty,
    /// Unreadable or over its cap.
    Withheld,
}

/// The state of a row's `attributedBody` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum AttributedState {
    /// The column does not exist.
    Absent,
    /// NULL.
    Null,
    /// A value within its cap.
    Present,
    /// Unreadable or over its cap.
    Withheld,
}

impl TextState {
    /// Every state, in a fixed order.
    pub const ALL: [TextState; 5] = [
        TextState::Absent,
        TextState::Null,
        TextState::Empty,
        TextState::NonEmpty,
        TextState::Withheld,
    ];
}

impl AttributedState {
    /// Every state, in a fixed order.
    pub const ALL: [AttributedState; 4] = [
        AttributedState::Absent,
        AttributedState::Null,
        AttributedState::Present,
        AttributedState::Withheld,
    ];
}

/// Message bodies (§10.8). All counts.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BodyCounts {
    /// `text` × `attributedBody`, every combination in
    /// [`TextState::ALL`] × [`AttributedState::ALL`] order.
    pub text_by_attributed: Vec<(TextState, AttributedState, u64)>,
    /// `attributedBody` decoded with no run issue and no unreadable
    /// attribute value.
    pub decoded_ok: u64,
    /// Decoded, with a run issue or an unreadable attribute value.
    pub decoded_degraded: u64,
    /// Decoding failed (by kind in `FailureCounts::typedstream`).
    pub decode_failed: u64,
    /// Present but not decoded (decoding turned off).
    pub not_decoded: u64,
    /// Both `text` and a decoded body are present, and equal.
    pub text_equals_decoded: u64,
    /// Both are present, and differ (whether they always match is
    /// **UNVERIFIED**).
    pub text_differs_from_decoded: u64,
    /// Decoded bodies with more than one part.
    pub more_than_one_part: u64,
    /// Decoded bodies with a mention.
    pub with_mention: u64,
    /// Decoded bodies with the link key present.
    pub with_link_key: u64,
    /// Decoded bodies with the audio-transcription flag.
    pub with_audio_transcription: u64,
}

impl Default for BodyCounts {
    fn default() -> Self {
        let mut text_by_attributed = Vec::new();
        for t in TextState::ALL {
            for a in AttributedState::ALL {
                text_by_attributed.push((t, a, 0));
            }
        }
        BodyCounts {
            text_by_attributed,
            decoded_ok: 0,
            decoded_degraded: 0,
            decode_failed: 0,
            not_decoded: 0,
            text_equals_decoded: 0,
            text_differs_from_decoded: 0,
            more_than_one_part: 0,
            with_mention: 0,
            with_link_key: 0,
            with_audio_transcription: 0,
        }
    }
}

impl BodyCounts {
    pub(crate) fn add_cross(&mut self, t: TextState, a: AttributedState) {
        if let Some(e) = self
            .text_by_attributed
            .iter_mut()
            .find(|e| e.0 == t && e.1 == a)
        {
            bump(&mut e.2);
        }
    }

    /// One cell of the `text` × `attributedBody` table.
    pub fn cross(&self, t: TextState, a: AttributedState) -> u64 {
        self.text_by_attributed
            .iter()
            .find(|e| e.0 == t && e.1 == a)
            .map_or(0, |e| e.2)
    }
}

/// `message` rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct MessageCounts {
    /// Rows.
    pub rows: u64,
    /// Rows with an association (first in the partition).
    pub with_association: Option<u64>,
    /// Rows with `item_type ≠ 0`, not already counted above. Despite the
    /// field's name (M3 §13.2), this is every non-message item kind, not only
    /// group actions: `item_type` 4 to 6 (shared location, kept audio,
    /// SharePlay) and unknown values are counted here too; [`ItemKind`]
    /// separates them. The UI labels it "Non-message rows".
    ///
    /// [`ItemKind`]: crate::ItemKind
    pub group_actions: Option<u64>,
    /// Rows a Recently Deleted link names, not already counted above.
    pub recently_deleted: Option<u64>,
    /// The remainder ("other rows").
    pub other_rows: Option<u64>,
    /// `is_from_me` = 1.
    pub from_me: u64,
    /// `is_from_me` = 0.
    pub not_from_me: u64,
    /// `is_from_me` NULL.
    pub direction_null: u64,
    /// `is_from_me` unreadable.
    pub direction_unreadable: u64,
    /// Not from me with `handle_id` 0.
    pub not_from_me_handle_zero: Option<u64>,
    /// A non-zero `handle_id` matching no handle.
    pub handle_unmatched: Option<u64>,
    /// A non-zero `other_handle` matching no handle.
    pub other_handle_unmatched: Option<u64>,
    /// By the class of `service`.
    pub by_service: Option<ServiceCounts>,
    /// By item kind (§11.4).
    pub item_kinds: Option<ItemKindCounts>,
    /// Associations (§11.1).
    pub associations: Option<AssociationCounts>,
    /// Replies: a non-empty `thread_originator_guid`.
    pub replies: Option<u64>,
    /// Replies whose originator matches no `message.guid` (by hash).
    pub reply_originator_not_found: Option<u64>,
    /// Rows with a non-empty `reply_to_guid` (not a reply marker).
    pub reply_to_guid_set: Option<u64>,
    /// Edit labels.
    pub edit_labels: EditLabelCounts,
    /// `ec` parts with exactly one entry.
    pub ec_single_entry_parts: Option<u64>,
    /// `message_summary_info`.
    pub summaries: Option<SummaryCounts>,
    /// `schedule_type` non-zero.
    pub scheduled: Option<u64>,
    /// `filter_action`, raw.
    pub filter_action: Option<RawHistogram>,
    /// `payload_data` present (storage class only).
    pub payload_data_present: Option<u64>,
    /// The cache flag `cache_has_attachments` set.
    pub cache_has_attachments_set: Option<u64>,
    /// `date_retracted` holds a date.
    pub date_retracted_set: Option<u64>,
    /// Bodies.
    pub bodies: BodyCounts,
}

/// Date facts (§8.3). `Debug` withholds the range.
#[derive(Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DateCounts {
    /// `message.date`.
    pub message_date: UnitHistogram,
    /// `chat_message_join.message_date` (unit **UNVERIFIED**).
    pub chat_message_date: Option<UnitHistogram>,
    /// `chat.last_read_message_timestamp` (unit **UNVERIFIED**).
    pub chat_last_read: Option<UnitHistogram>,
    /// `chat_recoverable_message_join.delete_date` (unit **UNVERIFIED**).
    pub recoverable_delete_date: Option<UnitHistogram>,
    /// `message_summary_info` `ec[*].d` (rule **UNVERIFIED**).
    pub edit_entry_date: Option<UnitHistogram>,
    /// The earliest included `message.date`, Unix ns. Shown, never logged.
    pub earliest_message: Option<i64>,
    /// The latest included `message.date`, Unix ns. Shown, never logged.
    pub latest_message: Option<i64>,
    /// Left out: scheduled (`date` is the planned send time; S6).
    pub excluded_scheduled: Option<u64>,
    /// Left out: zero, negative, ambiguous, NULL or unreadable.
    pub excluded_not_classified: u64,
    /// Left out: before [`RANGE_FLOOR_UNIX_NS`].
    pub excluded_before_floor: u64,
    /// Left out: after the check started.
    pub excluded_after_checked_at: u64,
}

impl fmt::Debug for DateCounts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DateCounts")
            .field("message_date", &self.message_date)
            .field("chat_message_date", &self.chat_message_date)
            .field("chat_last_read", &self.chat_last_read)
            .field("recoverable_delete_date", &self.recoverable_delete_date)
            .field("edit_entry_date", &self.edit_entry_date)
            .field("has_range", &self.earliest_message.is_some())
            .field("excluded_scheduled", &self.excluded_scheduled)
            .field("excluded_not_classified", &self.excluded_not_classified)
            .field("excluded_before_floor", &self.excluded_before_floor)
            .field("excluded_after_checked_at", &self.excluded_after_checked_at)
            .finish_non_exhaustive()
    }
}

/// Blob probe outcomes for attachments that were found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProbeCounts {
    /// A regular file (ciphertext length for encrypted backups).
    pub present: u64,
    /// The blob is not in the backup.
    pub missing: u64,
    /// A link, directory or device.
    pub not_regular: u64,
    /// Its metadata could not be read.
    pub unreadable: u64,
    /// The entry is not a file.
    pub not_a_file: u64,
    /// The entry's fileID does not match.
    pub invalid_entry: u64,
    /// Encrypted: no `EncryptionKey`.
    pub no_key: u64,
    /// Encrypted: no `ProtectionClass`.
    pub no_protection_class: u64,
    /// Encrypted: the class key is unavailable.
    pub class_key_unavailable: u64,
    /// Encrypted: the key did not unwrap (never called "damaged").
    pub key_does_not_unwrap: u64,
    /// A regular file whose length differs from the Manifest's `Size`
    /// (padded to a block when encrypted): truncated or replaced, so not
    /// counted as present.
    pub size_mismatch: u64,
    /// Any outcome this version does not name.
    pub other: u64,
    /// Found, but probing was turned off.
    pub not_probed: u64,
}

/// Lookup outcomes (§12.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LookupCounts {
    /// Listed in Manifest.db.
    pub found: u64,
    /// `SmsAttachments` not listed: "not in this backup".
    pub not_listed_at_sourced_key: u64,
    /// `SmsStickerCache` not listed (absence not claimed).
    pub not_found_at_assumed_key: u64,
    /// More than one row.
    pub ambiguous: u64,
    /// A malformed or mismatched row.
    pub invalid_entry: u64,
    /// Not attempted, by reason, in [`NotAttemptedReason::ALL`] order.
    pub not_attempted: Vec<(NotAttemptedReason, u64)>,
}

/// `attachment` rows and where their files are.
///
/// When the database has no `attachment` table, `rows` is `None` and
/// nothing was looked up or probed: the plain counters (`by_class`,
/// `lookup`, `probes`, `present_blob_bytes`) are then zeros that mean "not
/// recorded", and consumers treat the whole section as not recorded (the
/// shell sends no attachments section at all).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AttachmentCounts {
    /// Rows; `None` when the table does not exist.
    pub rows: Option<u64>,
    /// Attachments a `message_attachment_join` row names.
    pub linked: Option<u64>,
    /// The others.
    pub not_linked: Option<u64>,
    /// `filename` NULL.
    pub filename_null: Option<u64>,
    /// `filename` over 4 KiB (never read).
    pub filename_too_long: Option<u64>,
    /// By path class, in [`PathClass::ALL`] order.
    pub by_class: Vec<(PathClass, u64)>,
    /// Lookup outcomes.
    pub lookup: LookupCounts,
    /// Probe outcomes of found files.
    pub probes: ProbeCounts,
    /// Declared `total_bytes`, summed over non-negative values.
    pub declared_total_bytes: Option<u64>,
    /// Bytes of present blobs (ciphertext for encrypted backups).
    pub present_blob_bytes: u64,
    /// `is_sticker` set.
    pub stickers: Option<u64>,
    /// `hide_attachment` set.
    pub hidden: Option<u64>,
    /// `transfer_state`, raw (meanings **UNVERIFIED**).
    pub transfer_state: Option<RawHistogram>,
    /// Rows with an `emoji_image_short_description` (Genmoji; not decoded).
    pub with_genmoji_description: Option<u64>,
    /// The only domain looked up.
    pub lookup_domain: &'static str,
}

impl AttachmentCounts {
    pub(crate) fn new(rows: Option<u64>) -> Self {
        AttachmentCounts {
            rows,
            linked: None,
            not_linked: None,
            filename_null: None,
            filename_too_long: None,
            by_class: PathClass::ALL.iter().map(|c| (*c, 0)).collect(),
            lookup: LookupCounts {
                not_attempted: NotAttemptedReason::ALL.iter().map(|r| (*r, 0)).collect(),
                ..LookupCounts::default()
            },
            probes: ProbeCounts::default(),
            declared_total_bytes: None,
            present_blob_bytes: 0,
            stickers: None,
            hidden: None,
            transfer_state: None,
            with_genmoji_description: None,
            lookup_domain: ATTACHMENT_LOOKUP_DOMAIN,
        }
    }

    /// One class's count.
    pub fn class(&self, c: PathClass) -> u64 {
        self.by_class.iter().find(|e| e.0 == c).map_or(0, |e| e.1)
    }

    /// One not-attempted reason's count.
    pub fn not_attempted(&self, r: NotAttemptedReason) -> u64 {
        self.lookup
            .not_attempted
            .iter()
            .find(|e| e.0 == r)
            .map_or(0, |e| e.1)
    }
}

/// Link tables, orphans and chat membership.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LinkCounts {
    /// `chat_message_join` rows.
    pub chat_message_rows: u64,
    /// `chat_handle_join` rows.
    pub chat_handle_rows: u64,
    /// `message_attachment_join` rows.
    pub message_attachment_rows: Option<u64>,
    /// `chat_recoverable_message_join` rows.
    pub recoverable_rows: Option<u64>,
    /// `chat_message_join.chat_id` naming no chat.
    pub orphan_chat_message_chat: Option<u64>,
    /// `chat_message_join.message_id` naming no message.
    pub orphan_chat_message_message: Option<u64>,
    /// `chat_handle_join.chat_id` naming no chat.
    pub orphan_chat_handle_chat: Option<u64>,
    /// `chat_handle_join.handle_id` naming no handle.
    pub orphan_chat_handle_handle: Option<u64>,
    /// `message_attachment_join.message_id` naming no message.
    pub orphan_message_attachment_message: Option<u64>,
    /// `message_attachment_join.attachment_id` naming no attachment.
    pub orphan_message_attachment_attachment: Option<u64>,
    /// `chat_recoverable_message_join.chat_id` naming no chat.
    pub orphan_recoverable_chat: Option<u64>,
    /// `chat_recoverable_message_join.message_id` naming no message.
    pub orphan_recoverable_message: Option<u64>,
    /// Messages in no chat.
    pub messages_in_no_chat: Option<u64>,
    /// Of those, the ones a Recently Deleted link names.
    pub messages_in_no_chat_recently_deleted: Option<u64>,
    /// Messages in more than one chat.
    pub messages_in_several_chats: Option<u64>,
}

/// Recently Deleted tables: counts only (D33). No deleted text is read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecentlyDeletedCounts {
    /// `chat_recoverable_message_join` rows.
    pub chat_recoverable_message_join: Option<u64>,
    /// `recoverable_message_part` rows (`part_text` is denied).
    pub recoverable_message_part: Option<u64>,
    /// `deleted_messages` rows.
    pub deleted_messages: Option<u64>,
    /// `sync_deleted_messages` rows.
    pub sync_deleted_messages: Option<u64>,
    /// `sync_deleted_chats` rows.
    pub sync_deleted_chats: Option<u64>,
    /// `sync_deleted_attachments` rows.
    pub sync_deleted_attachments: Option<u64>,
    /// `unsynced_removed_recoverable_messages` rows.
    pub unsynced_removed_recoverable_messages: Option<u64>,
}

/// Problems by kind. None of them stopped the check.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FailureCounts {
    /// Typedstream failures and run issues, one entry per kind in
    /// [`TsErrorKind::ALL`] order.
    pub typedstream: Vec<(TsErrorKind, u64)>,
    /// `message_summary_info` over 1 MiB.
    pub summary_too_large: u64,
    /// Not a valid property list.
    pub summary_plist_invalid: u64,
    /// Over a property-list budget.
    pub summary_plist_budget: u64,
    /// The root is not a dictionary.
    pub summary_not_dictionary: u64,
    /// Parsed summaries with at least one field of the wrong shape.
    pub summary_shape: u64,
    /// Unknown summary keys (names never kept).
    pub summary_unknown_keys: u64,
    /// Value-level issues over every pass.
    pub values: ValueIssues,
    /// Ambiguous values in the reported time columns.
    pub date_ambiguous: u64,
    /// Negative values in the reported time columns.
    pub date_negative: u64,
    /// Association targets that did not parse.
    pub assoc_guid_unparsed: u64,
    /// Unknown attribute keys in decoded bodies.
    pub unknown_attribute_key: u64,
    /// Attribute values of an unknown class.
    pub unknown_attribute_class: u64,
    /// Known attribute values that could not be read.
    pub unreadable_attribute_value: u64,
    /// Orphan join rows, summed.
    pub orphan_join_row: u64,
    /// Attachment lookups that were ambiguous.
    pub attachment_ambiguous: u64,
    /// Attachment lookups that met an invalid entry.
    pub attachment_invalid_entry: u64,
    /// Attachment names too unusual to look up.
    pub attachment_unusual_name: u64,
    /// Found attachments whose blob is missing.
    pub attachment_blob_missing: u64,
    /// Found attachments whose blob is not a readable regular file.
    pub attachment_blob_unreadable: u64,
    /// Found attachments of an encrypted backup whose key is missing or
    /// does not unwrap.
    pub attachment_key_problem: u64,
    /// Found attachments whose blob's length differs from the Manifest's
    /// `Size`.
    pub attachment_blob_size_mismatch: u64,
    /// Aggregate statements over their budget.
    pub aggregate_over_budget: u32,
}

impl Default for FailureCounts {
    fn default() -> Self {
        FailureCounts {
            typedstream: TsErrorKind::ALL.iter().map(|k| (*k, 0)).collect(),
            summary_too_large: 0,
            summary_plist_invalid: 0,
            summary_plist_budget: 0,
            summary_not_dictionary: 0,
            summary_shape: 0,
            summary_unknown_keys: 0,
            values: ValueIssues::default(),
            date_ambiguous: 0,
            date_negative: 0,
            assoc_guid_unparsed: 0,
            unknown_attribute_key: 0,
            unknown_attribute_class: 0,
            unreadable_attribute_value: 0,
            orphan_join_row: 0,
            attachment_ambiguous: 0,
            attachment_invalid_entry: 0,
            attachment_unusual_name: 0,
            attachment_blob_missing: 0,
            attachment_blob_unreadable: 0,
            attachment_key_problem: 0,
            attachment_blob_size_mismatch: 0,
            aggregate_over_budget: 0,
        }
    }
}

impl FailureCounts {
    pub(crate) fn typedstream(&mut self, kind: TsErrorKind) {
        if let Some(e) = self.typedstream.iter_mut().find(|e| e.0 == kind) {
            bump(&mut e.1);
        }
    }

    /// One typedstream kind's count.
    pub fn typedstream_count(&self, kind: TsErrorKind) -> u64 {
        self.typedstream
            .iter()
            .find(|e| e.0 == kind)
            .map_or(0, |e| e.1)
    }
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

/// The result of [`crate::check`]: aggregates only.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExtractionReport {
    /// The outcome and its qualifiers.
    pub status: ExtractionStatus,
    /// What the scan and probe established.
    pub schema: SchemaReport,
    /// Facts about the staged database.
    pub database: DbFacts,
    /// `handle`; `None` when not read.
    pub handles: Option<HandleCounts>,
    /// `chat`; `None` when not read.
    pub chats: Option<ChatCounts>,
    /// `message`; `None` when not read.
    pub messages: Option<MessageCounts>,
    /// Dates; `None` when not read.
    pub dates: Option<DateCounts>,
    /// `attachment`; `None` when not read.
    pub attachments: Option<AttachmentCounts>,
    /// Links and orphans; `None` when not read.
    pub links: Option<LinkCounts>,
    /// Recently Deleted; `None` when not read.
    pub recently_deleted: Option<RecentlyDeletedCounts>,
    /// Problems by kind; `None` when not read.
    pub failures: Option<FailureCounts>,
    /// When the check started.
    pub checked_at: SystemTime,
    /// How long it took.
    pub elapsed: Duration,
}

impl ExtractionReport {
    /// The qualifiers, when readable; empty otherwise.
    pub fn qualifiers(&self) -> &[Qualifier] {
        match &self.status {
            ExtractionStatus::Readable { qualifiers } => qualifiers,
            _ => &[],
        }
    }

    /// A short kind name for logs: `readable`, `damaged`, `unsupported` or
    /// `incomplete`.
    pub fn status_kind(&self) -> &'static str {
        match self.status {
            ExtractionStatus::Readable { .. } => "readable",
            ExtractionStatus::Damaged(_) => "damaged",
            ExtractionStatus::Unsupported(_) => "unsupported",
            ExtractionStatus::Incomplete(_) => "incomplete",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::col::{Col, IssueKind};

    #[test]
    fn histograms_cap_and_order() {
        let mut h = RawHistogram::default();
        for v in (0..20i64).rev() {
            h.add(&Col::Value(v));
        }
        h.add(&Col::Value(19));
        h.add(&Col::Value(3));
        h.add(&Col::Null);
        h.add(&Col::Unreadable(IssueKind::TypeMismatch));
        h.add(&Col::Absent);
        assert_eq!(h.entries.len(), HISTOGRAM_CAP);
        let values: Vec<i64> = h.entries.iter().map(|e| e.0).collect();
        let mut sorted = values.clone();
        sorted.sort_unstable();
        assert_eq!(values, sorted);
        assert_eq!(h.entries.first(), Some(&(4, 1)));
        assert_eq!(h.entries.last(), Some(&(19, 2)));
        assert_eq!(h.other, 5, "0..=3 arrived after the cap, and 3 twice");
        assert_eq!((h.null, h.unreadable), (1, 1));
    }

    #[test]
    fn the_date_range_is_not_debug_printed() {
        let d = DateCounts {
            earliest_message: Some(1_234_567_890_123),
            latest_message: Some(9_876_543_210_987),
            ..DateCounts::default()
        };
        let s = format!("{d:?}");
        assert!(
            !s.contains("1234567890123") && !s.contains("9876543210987"),
            "{s}"
        );
        assert!(s.contains("has_range: true"));
    }

    #[test]
    fn optional_counters() {
        let mut a = zero_if(true);
        bump_opt(&mut a);
        add_opt(&mut a, 2);
        assert_eq!(a, Some(3));
        let mut b = zero_if(false);
        bump_opt(&mut b);
        add_opt(&mut b, 2);
        assert_eq!(b, None);
        let body = BodyCounts::default();
        assert_eq!(body.text_by_attributed.len(), 20);
        let att = AttachmentCounts::new(Some(0));
        assert_eq!(att.by_class.len(), PathClass::ALL.len());
        assert_eq!(
            att.lookup.not_attempted.len(),
            NotAttemptedReason::ALL.len()
        );
        assert_eq!(
            FailureCounts::default().typedstream.len(),
            TsErrorKind::ALL.len()
        );
    }
}
