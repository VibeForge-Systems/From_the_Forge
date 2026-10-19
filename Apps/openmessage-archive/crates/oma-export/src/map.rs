//! From the archive's export reads to records (M6 spec §4.2, §4.9, D142).
//!
//! [`Mapper`] turns an `ExportMessage` (assembled under `Caps::EXPORT` in
//! `oma-archive`, masked per the export's reveal) into an [`ItemRecord`],
//! and conversations, handles and body versions into their records. It
//! reads the archive only through `ExportSnapshot`'s public methods.
//!
//! - **Release-mode checks.** A value that still carries a cap marker is
//!   `Limit(TruncatedUnderExport)`; an item whose extras were not loaded
//!   is `Limit(ExtrasNotLoaded)`; an `oma-archive` variant this build has
//!   no code for is `Limit(UnmappedVariant)` (its enums are
//!   `#[non_exhaustive]`, so a new one can never be written as something
//!   else). None of them is a `debug_assert`.
//! - **Codes keep their numbers** ([`Code::coded`]) from the raw codes
//!   `oma-archive` keeps beside the view types.
//! - **Ordinals** come from [`Ordinals`] (Q5's lean: export-local by
//!   default); a reference outside the scope carries no id.
//! - **Times** are placed in the export's zone ([`ExportZone`]); an
//!   instant outside the window is written in UTC and counted, and one the
//!   table should cover but does not stops the export
//!   (`ZoneTableStale`).
//! - **Files.** Whether an attachment's file is copied is decided by the
//!   caller ([`AttachmentFiles`], the copy of slice 6e); the mapper alone
//!   decides the outcomes that need no copy: names only (`notRequested`),
//!   withheld (`withheldUnsent`) and no stored file (`noStoredFile`).
//!
//! Nothing here logs.

use std::collections::BTreeMap;

use oma_archive::schema::codes::ValueState;
use oma_archive::{
    AltTextSource, AltTextWithheld, AlternativeText, AssocVisibility, AttachmentAccess, BlobId,
    BodyVersion, BodyView, CardState, ChatId, ChipSender, ChipState, ContentText, EarlierText,
    EditEntryText, EditTextsOutcome, ExportAttachment, ExportMessage, ExportSnapshot, Formatting,
    HandleId, HistoryColumn, HistoryState, HistoryValue, MaskKind, MessageId, MessageLabel,
    OptContent, RawDirection, RawInt, RawNumber, RawTime, RawValue, ReactionKind, ReplyRef,
    SegmentKind, SenderReason, SourceIdentifiers, SourceOf, StateBool, TextPiece,
    ValueHistoryEntry, VersionCodes, VersionText, WithheldReason,
};

use crate::codes::{
    AltTextSourceCode, AltTextWithheldCode, AssocVisibilityCode, AttachmentAccessCode,
    AttachmentOutcome, CardStateCode, ChipSenderCode, ChipStateCode, Code, Direction,
    EditTextsOutcomeCode, ExportCode, FormattingCode, HistoryColumnCode, MaskKindCode,
    MessageLabelCode, NoTextReason, ReactionKindCode, ReplyResolution, SegmentKindCode,
    SenderReasonCode, SniffedKindCode, Style, WithheldReasonCode,
};
use crate::error::{ExportError, LimitKind};
use crate::ids::{IdentifierMode, MessageRef, Ordinal, Ordinals};
use crate::plan::RecordOptions;
use crate::record::*;
use crate::time::{format_iso, Localized, Precision, ZoneTable};

const FLOOR_2007_NS: i64 = 1_167_609_600 * 1_000_000_000;

fn limit(k: LimitKind) -> ExportError {
    ExportError::Limit(k)
}

fn unmapped() -> ExportError {
    limit(LimitKind::UnmappedVariant)
}

/// The zone the export's times are written in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportZone {
    /// UTC: every instant is written with offset +00:00, no table needed.
    Utc,
    /// A validated table from the webview.
    Table(ZoneTable),
}

impl ExportZone {
    fn localize(&self, utc_ns: i64) -> Result<Localized, ExportError> {
        match self {
            ExportZone::Utc => Ok(Localized::Local { offset_seconds: 0 }),
            ExportZone::Table(t) => t.localize(utc_ns),
        }
    }
}

/// The caller's decision for one attachment whose stored file the export
/// may copy (slice 6e's copy decides it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileDecision {
    /// Copied, under its allowlisted extension.
    Copied,
    /// The same blob was already copied for another record.
    Deduplicated,
    /// Copied with `.blocked` appended.
    BlockedExtension,
    ChecksumMismatch,
    TooLargeForVolume,
    WriteFailed,
}

/// What happened to one attachment's file: the decision, and for a file
/// that was written, the file ordinal it was written under (the mapper
/// checks it against its own first-reference ordinal), its bundle path
/// and what its bytes were sniffed as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOutcome {
    pub decision: FileDecision,
    /// The file ordinal, for `Copied`, `Deduplicated` and
    /// `BlockedExtension`.
    pub file: Option<Ordinal>,
    /// `attachments/<shard>/<name>`, `/`-separated (content).
    pub path: Option<String>,
    /// What the copy stream's first bytes were sniffed as; `None` when the
    /// file was not read (or a kind with no code).
    pub sniffed: Option<oma_archive::SniffedKind>,
}

impl FileOutcome {
    /// An outcome with no file written and nothing read.
    pub fn without_file(decision: FileDecision) -> FileOutcome {
        FileOutcome {
            decision,
            file: None,
            path: None,
            sniffed: None,
        }
    }
}

/// Decides, for an attachment with a stored file the export may copy,
/// what happens to the file (the copy itself is `attach.rs`).
pub trait AttachmentFiles {
    fn decide(
        &mut self,
        attachment: &ExportAttachment,
        blob: BlobId,
    ) -> Result<FileOutcome, ExportError>;
}

/// Counts the mapper keeps for the manifest (spec §7.1); content-free.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecordCounts {
    pub items: u64,
    pub before_2007: u64,
    pub no_text: u64,
    pub lossy: u64,
    pub order_ties: u64,
    pub one_time_codes_masked: u64,
    pub earlier_text_withheld: u64,
    pub edit_text_withheld: u64,
    pub alternative_text_withheld: u64,
    pub times_outside_zone_window: u64,
    pub attachment_records: u64,
    pub attachments_with_file: u64,
    pub attachments_not_requested: u64,
    /// Records without a file, by reason token (`withheldUnsent`,
    /// `checksumMismatch`, …, and `noStoredFile` by card token).
    pub attachments_excluded: BTreeMap<String, u64>,
    pub files_deduplicated: u64,
    pub files_blocked: u64,
}

/// Where an item is written: which conversation file, what it folds into
/// there, and whether its time ties a neighbour's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Placement {
    /// `None`: the "No conversation" file.
    pub conversation: Option<ChatId>,
    /// A reaction row's target in that conversation.
    pub folded_into: Option<MessageId>,
    pub order_tie: bool,
}

/// Maps one export's reads to records.
#[derive(Debug)]
pub struct Mapper {
    ordinals: Ordinals,
    zone: ExportZone,
    options: RecordOptions,
    counts: RecordCounts,
    /// The file outcome of each attachment of a message that is a
    /// timeline item of more than one exported conversation, decided at
    /// its first placement and repeated at the others (slice 6f), so the
    /// record's intrinsic part is the same in every conversation file
    /// (spec §4.9) and the copy is asked once.
    shared_files: BTreeMap<(MessageId, oma_archive::AttachmentId), FileRecord>,
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

fn content(t: &ContentText) -> Result<TextValue, ExportError> {
    if t.truncated {
        return Err(limit(LimitKind::TruncatedUnderExport));
    }
    Ok(TextValue {
        text: Some(Content::new(t.text.expose().clone())),
        missing: None,
        lossy: t.lossy,
        stored_as_blob: t.stored_as_blob,
        bidi_controls: t.bidi_controls,
        invisible: t.invisible,
    })
}

fn missing(state: ValueState) -> TextValue {
    TextValue {
        text: None,
        missing: Some(Code::of(state)),
        lossy: false,
        stored_as_blob: false,
        bidi_controls: false,
        invisible: false,
    }
}

/// A content value as a record's [`TextValue`].
pub fn text_value(o: &OptContent) -> Result<TextValue, ExportError> {
    match o {
        OptContent::Text(t) => content(t),
        OptContent::Missing(s) => Ok(missing(*s)),
        _ => Err(unmapped()),
    }
}

fn opt_text(o: Option<&OptContent>) -> Result<TextValue, ExportError> {
    text_value(o.ok_or_else(|| limit(LimitKind::ColumnNotRead))?)
}

fn raw_int(r: RawInt) -> RawIntRecord {
    RawIntRecord {
        raw: r.raw.map(Decimal::of),
        state: r.state.map(Code::coded),
    }
}

fn raw_value(r: RawValue) -> RawIntRecord {
    RawIntRecord {
        raw: r.value.map(Decimal::of),
        state: r.state.map(Code::of),
    }
}

fn state_bool(b: StateBool) -> StateBoolRecord {
    StateBoolRecord {
        value: b.value,
        state: b.state.map(Code::coded),
        derived: true,
    }
}

fn raw_time(t: RawTime) -> RawTimeRecord {
    RawTimeRecord {
        raw: t.raw.map(Decimal::of),
        state: t.state.map(Code::coded),
        unit: t.unit.map(Code::coded),
        unit_rule: UNIT_RULE,
    }
}

fn withheld_reason(r: WithheldReason) -> Result<Code, ExportError> {
    Ok(Code::of(match r {
        WithheldReason::Unsent => WithheldReasonCode::Unsent,
        WithheldReason::EarlierVersion => WithheldReasonCode::EarlierVersion,
        WithheldReason::NoCurrentText => WithheldReasonCode::NoCurrentText,
        _ => return Err(unmapped()),
    }))
}

fn mask_kind(m: MaskKind) -> Result<Code, ExportError> {
    match m {
        MaskKind::OneTimeCode => Ok(Code::of(MaskKindCode::OneTimeCode)),
        _ => Err(unmapped()),
    }
}

fn history_column(c: HistoryColumn) -> Result<Code, ExportError> {
    Ok(Code::of(match c {
        HistoryColumn::Subject => HistoryColumnCode::Subject,
        HistoryColumn::GroupTitle => HistoryColumnCode::GroupTitle,
        HistoryColumn::DestinationCallerId => HistoryColumnCode::DestinationCallerId,
        HistoryColumn::ChatDisplayName => HistoryColumnCode::ChatDisplayName,
        HistoryColumn::TransferName => HistoryColumnCode::TransferName,
        HistoryColumn::EmojiDescription => HistoryColumnCode::EmojiDescription,
        _ => return Err(unmapped()),
    }))
}

fn raw_direction(d: RawDirection) -> Result<RawDirectionRecord, ExportError> {
    Ok(match d {
        RawDirection::FromMe => RawDirectionRecord {
            token: Code::of(Direction::FromMe),
            state: None,
            raw: None,
        },
        RawDirection::Received => RawDirectionRecord {
            token: Code::of(Direction::Received),
            state: None,
            raw: None,
        },
        RawDirection::NotRecorded { state } => RawDirectionRecord {
            token: Code::of(Direction::NotRecorded),
            state: state.map(Code::coded),
            raw: None,
        },
        RawDirection::UnknownValue { raw } => RawDirectionRecord {
            token: Code::of(Direction::UnknownValue),
            state: None,
            raw: Some(Decimal::of(raw)),
        },
        _ => return Err(unmapped()),
    })
}

fn pieces(text: &[TextPiece], counts: &mut RecordCounts) -> Result<Vec<PieceRecord>, ExportError> {
    text.iter()
        .map(|p| match p {
            TextPiece::Text(t) => Ok(PieceRecord {
                kind: Code::of(SegmentKindCode::Text),
                text: content(t)?.text,
                withheld: None,
            }),
            TextPiece::Masked(m) => {
                counts.one_time_codes_masked += 1;
                Ok(PieceRecord {
                    kind: Code::of(SegmentKindCode::Withheld),
                    text: None,
                    withheld: Some(mask_kind(*m)?),
                })
            }
            _ => Err(unmapped()),
        })
        .collect()
}

/// Order ties (spec §4.10): `true` where an item's recorded instant
/// equals its neighbour's in the same file. Undated items (`None`) never
/// tie.
pub fn order_ties(times: &[Option<i64>]) -> Vec<bool> {
    (0..times.len())
        .map(|i| {
            let t = times.get(i).copied().flatten();
            let prev = i
                .checked_sub(1)
                .and_then(|j| times.get(j))
                .copied()
                .flatten();
            let next = times.get(i + 1).copied().flatten();
            t.is_some() && (t == prev || t == next)
        })
        .collect()
}

impl Mapper {
    /// A mapper for one export.
    pub fn new(ordinals: Ordinals, zone: ExportZone, options: RecordOptions) -> Mapper {
        Mapper {
            ordinals,
            zone,
            options,
            counts: RecordCounts::default(),
            shared_files: BTreeMap::new(),
        }
    }

    pub fn ordinals(&self) -> &Ordinals {
        &self.ordinals
    }

    pub fn counts(&self) -> &RecordCounts {
        &self.counts
    }

    fn opted_in(&self) -> bool {
        self.options.identifiers == IdentifierMode::ArchiveAndSource
    }

    fn message_ref(&self, id: MessageId) -> MessageRefRecord {
        match self.ordinals.message_ref(id) {
            MessageRef::InExport(n) => MessageRefRecord::InExport { n },
            MessageRef::NotInExport => MessageRefRecord::NotInExport,
        }
    }

    fn session(&mut self, id: i64) -> Ordinal {
        self.ordinals.sessions.ordinal(id)
    }

    fn handle_ordinal(&mut self, id: HandleId) -> Ordinal {
        self.ordinals.handles.ordinal(id)
    }

    /// A recorded message time placed in the export's zone.
    fn time(&mut self, t: RawTime, planned: bool) -> Result<TimeRecord, ExportError> {
        let precision = match t.unit {
            Some(Ok(oma_archive::schema::codes::TimeUnit::Nanoseconds)) => Precision::Nanoseconds,
            _ => Precision::Seconds,
        };
        let (utc, local, offset, outside) = match t.utc_ns {
            None => (None, None, None, false),
            Some(ns) => {
                let utc = Some(format_iso(ns, None, precision));
                match self.zone.localize(ns)? {
                    Localized::Local { offset_seconds } => (
                        utc,
                        Some(format_iso(ns, Some(offset_seconds), Precision::Seconds)),
                        Some(offset_seconds),
                        false,
                    ),
                    Localized::OutsideWindow => {
                        self.counts.times_outside_zone_window += 1;
                        (utc, None, None, true)
                    }
                }
            }
        };
        Ok(TimeRecord {
            raw: t.raw.map(Decimal::of),
            state: t.state.map(Code::coded),
            unit: t.unit.map(Code::coded),
            unit_rule: UNIT_RULE,
            utc,
            local,
            offset_seconds: offset,
            outside_zone_window: outside,
            before2007: t.utc_ns.is_some_and(|ns| ns < FLOOR_2007_NS),
            planned,
            utc_ns: t.utc_ns,
        })
    }

    fn value_history(&mut self, v: &HistoryValue) -> Result<ValueHistoryRecord, ExportError> {
        Ok(ValueHistoryRecord::Value {
            column: history_column(v.column)?,
            value: text_value(&v.value)?,
            first_session: self.session(v.first_session),
        })
    }

    // -----------------------------------------------------------------------
    // Items
    // -----------------------------------------------------------------------

    /// The record of `m` written at `at` (spec §4.2). `files` decides the
    /// files of its attachments unless the plan carries names only.
    pub fn item(
        &mut self,
        snap: &ExportSnapshot<'_>,
        m: &ExportMessage,
        at: Placement,
        files: &mut dyn AttachmentFiles,
    ) -> Result<ItemRecord, ExportError> {
        let a = &m.item.message;
        let c = &m.item.codes;
        if let Some(miss) = a.cap_miss() {
            return Err(limit(match miss {
                oma_archive::CapMiss::ExtrasNotLoaded => LimitKind::ExtrasNotLoaded,
                _ => LimitKind::TruncatedUnderExport,
            }));
        }
        let n = self
            .ordinals
            .message(a.id)
            .ok_or_else(|| limit(LimitKind::ScopeMismatch))?;
        let source = self.message_source(snap, a.id)?;
        let recorded_by = match a.direction_recorded_by {
            None => RecordedByRecord::SingleLineage,
            Some(r) => RecordedByRecord::Lineage {
                lineage: self.ordinals.lineages.ordinal(r.lineage.get()),
                session: self.session(r.session),
            },
        };
        let d = raw_direction(c.direction)?;
        let direction = DirectionRecord {
            token: d.token,
            state: d.state,
            raw: d.raw,
            recorded_by,
        };
        let sender = self.sender(a.sender)?;
        let planned = c.schedule_type.raw.is_some_and(|v| v != 0);
        let date = self.time(c.date, planned)?;
        if date.before2007 {
            self.counts.before_2007 += 1;
        }
        let body = self.body(a, c.body)?;
        let alternative_text = self.alternative_text(&m.alternative_text)?;
        let body_facts = match c.body {
            None => BodyFactsRecord {
                body_row: false,
                display_source: None,
                display_none_reason: None,
                attributed_outcome: None,
                attributed_error: None,
                run_issue: None,
                text_raw_state: None,
                sources_differ: None,
            },
            Some(b) => BodyFactsRecord {
                body_row: true,
                display_source: b.display_source.map(Code::coded),
                display_none_reason: b.display_none_reason.map(Code::coded),
                attributed_outcome: Some(Code::coded(b.attributed_outcome)),
                attributed_error: b.attributed_error.map(Code::coded),
                run_issue: b.run_issue.map(Code::coded),
                text_raw_state: b.text_raw_state.map(Code::coded),
                sources_differ: match &a.body {
                    BodyView::Text(t) => t.sources_differ,
                    _ => None,
                },
            },
        };
        let edit = EditRecord {
            label: c.body.map(|b| Code::coded(b.edit_label)),
            edited_parts: a
                .edit
                .edited_parts
                .iter()
                .map(|p| Decimal::of(*p))
                .collect(),
            unsent_parts: a
                .edit
                .unsent_parts
                .iter()
                .map(|p| Decimal::of(*p))
                .collect(),
            unsent_whole: a.edit.unsent_whole,
            part_count: a.edit.part_count,
            stored_versions: a.edit.stored_versions,
        };
        let earlier_text = self.earlier(&a.edit.earlier)?;
        let history = match m.history {
            HistoryState::NotIncluded {
                captured_versions,
                phone_entries,
            } => Included::NotIncluded {
                option: PlanOption::History,
                counts: HistoryCounts {
                    captured_versions,
                    phone_entries,
                },
            },
            HistoryState::Available {
                captured_versions,
                phone_entries,
            } => Included::Included {
                value: HistoryCounts {
                    captured_versions,
                    phone_entries,
                },
            },
            _ => return Err(unmapped()),
        };
        let labels = self.labels(&a.labels)?;
        let recently_deleted = self.recently_deleted(m)?;
        let association = self.association(m)?;
        let reactions = self.reactions(m)?;
        let reply = match &a.reply {
            None => None,
            Some(ReplyRef::Resolved { originator, .. }) => Some(ReplyRecord {
                resolution: Code::of(ReplyResolution::Resolved),
                originator: Some(self.message_ref(*originator)),
            }),
            Some(ReplyRef::InAnotherConversation { originator, .. }) => Some(ReplyRecord {
                resolution: Code::of(ReplyResolution::InAnotherConversation),
                originator: Some(self.message_ref(*originator)),
            }),
            Some(ReplyRef::NotUniquelyIdentified) => Some(ReplyRecord {
                resolution: Code::of(ReplyResolution::NotUniquelyIdentified),
                originator: None,
            }),
            Some(_) => return Err(unmapped()),
        };
        let reply_count = if a.labels.contains(&MessageLabel::Unkeyed) {
            Determinable::NotDeterminable {
                reason: NotDeterminableReason::Unkeyed,
            }
        } else {
            let ids = snap.reply_ids(a.id)?;
            let total = u64::try_from(ids.len()).unwrap_or(u64::MAX);
            if a.reply_count.map_or(0, |n| n.n) != total {
                return Err(limit(LimitKind::RowCountMismatch));
            }
            let in_export = ids.iter().filter(|r| self.ordinals.scope().contains(**r));
            let in_export = u64::try_from(in_export.count()).unwrap_or(u64::MAX);
            Determinable::Determined {
                value: ReplyCountRecord {
                    in_export,
                    elsewhere: total - in_export,
                },
            }
        };
        let event = match &a.event {
            None => None,
            Some(e) => Some(EventRecord {
                kind: Code::coded(c.item),
                handles: e.handles.iter().map(|h| self.handle_ordinal(*h)).collect(),
                title: e.title.as_ref().map(text_value).transpose()?,
            }),
        };
        let attachments = self.attachments(snap, m, files)?;
        let value_history = m
            .value_history
            .iter()
            .map(|v| match v {
                ValueHistoryEntry::Value(h) => self.value_history(h),
                ValueHistoryEntry::Withheld { column, reason } => {
                    Ok(ValueHistoryRecord::Withheld {
                        column: history_column(*column)?,
                        reason: withheld_reason(*reason)?,
                    })
                }
                _ => Err(unmapped()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let r = &m.raw_columns;
        let raw = RawColumnsRecord {
            service: text_value(&r.service)?,
            item_type: raw_int(c.item_type),
            group_action_type: raw_int(c.group_action_type),
            is_audio_message: raw_int(r.is_audio_message),
            cache_has_attachments: raw_int(r.cache_has_attachments),
            share_status: raw_int(r.share_status),
            share_direction: raw_int(r.share_direction),
            assoc_type: raw_int(r.assoc_type),
            assoc_range_location: raw_int(r.assoc_range_location),
            assoc_range_length: raw_int(r.assoc_range_length),
            thread_originator_part: text_value(&r.thread_originator_part)?,
            part_count: raw_int(r.part_count),
            schedule_state: raw_int(r.schedule_state),
            filter_action: raw_int(r.filter_action),
            filter_sub_action: raw_int(r.filter_sub_action),
            was_downgraded: raw_int(r.was_downgraded),
        };
        let context = self.context(snap, m, at)?;
        self.counts.items += 1;
        Ok(ItemRecord {
            n,
            source,
            item: Code::coded(c.item),
            direction,
            sender,
            service: c.service.map(Code::coded),
            date,
            date_edited: raw_time(m.detail.date_edited),
            date_retracted: raw_time(m.detail.date_retracted),
            subject: text_value(&m.detail.subject)?,
            account: text_value(&m.detail.account)?,
            body,
            alternative_text,
            body_facts,
            edit,
            earlier_text,
            history,
            labels,
            recently_deleted,
            association,
            reactions,
            reply,
            reply_count,
            event,
            attachments,
            schedule_type: raw_int(c.schedule_type),
            app_content: state_bool(c.app_content),
            effect: state_bool(c.effect),
            value_history,
            raw,
            derivation: DerivationRecord {
                derived_by: m.derivation.derived_by,
                decoder_version: m.derivation.decoder_version,
            },
            context,
        })
    }

    fn message_source(
        &mut self,
        snap: &ExportSnapshot<'_>,
        id: MessageId,
    ) -> Result<Included<MessageSourceRecord>, ExportError> {
        if !self.opted_in() {
            return Ok(Included::NotIncluded {
                option: PlanOption::Identifiers,
                counts: NoCounts {},
            });
        }
        let s: SourceIdentifiers = snap.source_identifiers(SourceOf::Message(id))?;
        Ok(Included::Included {
            value: MessageSourceRecord {
                archive_id: Decimal::of(id.get()),
                guid: opt_text(s.guid.as_ref())?,
                assoc_guid: opt_text(s.assoc_guid.as_ref())?,
                thread_originator_guid: opt_text(s.thread_originator_guid.as_ref())?,
                reply_to_guid: opt_text(s.reply_to_guid.as_ref())?,
            },
        })
    }

    fn sender(&mut self, s: SenderReason) -> Result<SenderRecord, ExportError> {
        let (reason, handle) = match s {
            SenderReason::FromMe => (SenderReasonCode::FromMe, None),
            SenderReason::Resolved(h) => (SenderReasonCode::Resolved, Some(self.handle_ordinal(h))),
            SenderReason::NoSender => (SenderReasonCode::NoSender, None),
            SenderReason::ColumnAbsent => (SenderReasonCode::ColumnAbsent, None),
            SenderReason::Unreadable => (SenderReasonCode::Unreadable, None),
            SenderReason::NotFound => (SenderReasonCode::NotFound, None),
            SenderReason::DirectionNotRecorded => (SenderReasonCode::DirectionNotRecorded, None),
            _ => return Err(unmapped()),
        };
        Ok(SenderRecord {
            reason: Code::of(reason),
            handle,
        })
    }

    fn body(
        &mut self,
        a: &oma_archive::AssembledMessage,
        codes: Option<oma_archive::BodyCodes>,
    ) -> Result<BodyRecord, ExportError> {
        let t = match &a.body {
            BodyView::Text(t) => t,
            BodyView::NoText(_) | BodyView::NoBody => {
                self.counts.no_text += 1;
                let stored = codes.and_then(|b| b.display_none_reason);
                let reason = match (&a.body, stored) {
                    (BodyView::NoBody, _) => NoTextReason::NoBodyRow,
                    (_, None) => NoTextReason::ReasonNotRecorded,
                    (_, Some(Ok(r))) => no_text_reason(r),
                    (_, Some(Err(_))) => NoTextReason::ReasonUnrecognised,
                };
                return Ok(BodyRecord::NoText {
                    reason: Code::of(reason),
                });
            }
            _ => return Err(unmapped()),
        };
        if t.truncated {
            return Err(limit(LimitKind::TruncatedUnderExport));
        }
        if t.lossy {
            self.counts.lossy += 1;
        }
        let formatting = Code::of(match t.formatting {
            Formatting::Plain => FormattingCode::Plain,
            Formatting::Applied => FormattingCode::Applied,
            Formatting::NotApplied => FormattingCode::NotApplied,
            Formatting::Unreadable(_) => FormattingCode::Unreadable,
            Formatting::TooManySegments => return Err(limit(LimitKind::TruncatedUnderExport)),
            _ => return Err(unmapped()),
        });
        let mut segments = Vec::with_capacity(t.segments.len());
        let mut at = 0u32;
        for s in &t.segments {
            let (kind, text, withheld, attachment) = match &s.kind {
                SegmentKind::Text(c) => (SegmentKindCode::Text, content(c)?.text, None, None),
                SegmentKind::Withheld(m) => {
                    self.counts.one_time_codes_masked += 1;
                    (SegmentKindCode::Withheld, None, Some(mask_kind(*m)?), None)
                }
                SegmentKind::InlineEmoji => (SegmentKindCode::InlineEmoji, None, None, None),
                SegmentKind::Attachment(id) => (
                    SegmentKindCode::Attachment,
                    None,
                    None,
                    Some(self.ordinals.attachments.ordinal(*id)),
                ),
                SegmentKind::UnmatchedPlaceholder => {
                    (SegmentKindCode::UnmatchedPlaceholder, None, None, None)
                }
                _ => return Err(unmapped()),
            };
            let len = text.as_ref().map_or(0, |c| {
                u32::try_from(c.as_str().encode_utf16().count()).unwrap_or(u32::MAX)
            });
            let mut styles = Vec::new();
            for (on, st) in [
                (s.bold, Style::Bold),
                (s.italic, Style::Italic),
                (s.underline, Style::Underline),
                (s.strikethrough, Style::Strikethrough),
            ] {
                if on {
                    styles.push(Code::of(st));
                }
            }
            segments.push(SegmentRecord {
                kind: Code::of(kind),
                text,
                withheld,
                attachment,
                transcription: s.transcription,
                mention: s.mention,
                url: s.url,
                url_hidden: s.url_hidden,
                one_time_code: s.one_time_code,
                styles,
                utf16_start: at,
                utf16_len: len,
            });
            at = at.saturating_add(len);
        }
        Ok(BodyRecord::Text {
            segments,
            formatting,
            utf16_len: at,
            bidi_controls: t.bidi_controls,
            invisible: t.invisible,
            lossy: t.lossy,
            stored_as_blob: t.stored_as_blob,
        })
    }

    fn alternative_text(&mut self, alt: &AlternativeText) -> Result<AltTextRecord, ExportError> {
        Ok(match alt {
            AlternativeText::None => AltTextRecord::None,
            AlternativeText::Text { source, text } => AltTextRecord::Text {
                source: Code::of(match source {
                    AltTextSource::TextColumn => AltTextSourceCode::TextColumn,
                    AltTextSource::DecodedAttributedBody => {
                        AltTextSourceCode::DecodedAttributedBody
                    }
                    _ => return Err(unmapped()),
                }),
                text: content(text)?,
            },
            AlternativeText::Withheld(w) => {
                self.counts.alternative_text_withheld += 1;
                match w {
                    AltTextWithheld::EarlierContent(r) => AltTextRecord::Withheld {
                        why: Code::of(AltTextWithheldCode::EarlierContent),
                        reason: Some(withheld_reason(*r)?),
                    },
                    AltTextWithheld::OneTimeCode => AltTextRecord::Withheld {
                        why: Code::of(AltTextWithheldCode::OneTimeCode),
                        reason: None,
                    },
                    _ => return Err(unmapped()),
                }
            }
            _ => return Err(unmapped()),
        })
    }

    fn earlier(&mut self, e: &EarlierText) -> Result<EarlierRecord, ExportError> {
        Ok(match e {
            EarlierText::None => EarlierRecord::None,
            EarlierText::Available => EarlierRecord::Available,
            EarlierText::Withheld(r) => {
                self.counts.earlier_text_withheld += 1;
                EarlierRecord::Withheld {
                    reason: withheld_reason(*r)?,
                }
            }
            EarlierText::Revealed {
                reason,
                text,
                session,
            } => EarlierRecord::Revealed {
                reason: withheld_reason(*reason)?,
                pieces: pieces(text, &mut self.counts)?,
                session: self.session(*session),
            },
            _ => return Err(unmapped()),
        })
    }

    fn labels(&mut self, labels: &[MessageLabel]) -> Result<Vec<LabelRecord>, ExportError> {
        labels
            .iter()
            .map(|l| {
                let (kind, value, lineage) = match l {
                    MessageLabel::Undated => (MessageLabelCode::Undated, None, None),
                    MessageLabel::Before2007 => (MessageLabelCode::Before2007, None, None),
                    MessageLabel::Unkeyed => (MessageLabelCode::Unkeyed, None, None),
                    MessageLabel::ScheduleType(v) => {
                        (MessageLabelCode::ScheduleType, Some(Decimal::of(*v)), None)
                    }
                    MessageLabel::AppContent => (MessageLabelCode::AppContent, None, None),
                    MessageLabel::Effect => (MessageLabelCode::Effect, None, None),
                    MessageLabel::NotInLatest(lin) => (
                        MessageLabelCode::NotInLatest,
                        None,
                        Some(self.ordinals.lineages.ordinal(lin.get())),
                    ),
                    MessageLabel::Provisional => (MessageLabelCode::Provisional, None, None),
                    _ => return Err(unmapped()),
                };
                Ok(LabelRecord {
                    kind: Code::of(kind),
                    value,
                    lineage,
                    derived: true,
                })
            })
            .collect()
    }

    fn conversation_ref(&self, chat: Option<ChatId>) -> ConversationRefRecord {
        match chat {
            None => ConversationRefRecord::NotRecorded,
            Some(c) => match self.ordinals.conversation(c) {
                Some(n) => ConversationRefRecord::InExport { n },
                None => ConversationRefRecord::NotInExport,
            },
        }
    }

    fn recently_deleted(
        &mut self,
        m: &ExportMessage,
    ) -> Result<Vec<RecentlyDeletedRecord>, ExportError> {
        let markers = &m.item.message.recently_deleted;
        let mut out = Vec::with_capacity(m.detail.recently_deleted.len());
        for row in &m.detail.recently_deleted {
            let later: Vec<i64> = markers
                .iter()
                .find(|k| k.session == row.session)
                .map(|k| k.not_listed_later.clone())
                .unwrap_or_default();
            let session = self.session(row.session);
            let not_listed_later = later.into_iter().map(|s| self.session(s)).collect();
            out.push(RecentlyDeletedRecord {
                session,
                conversation: self.conversation_ref(row.chat),
                not_listed_later,
                deleted: raw_time(row.deleted),
                ck_sync_state: raw_int(row.ck_sync_state),
            });
        }
        Ok(out)
    }

    fn association(&mut self, m: &ExportMessage) -> Result<Option<AssociationRecord>, ExportError> {
        let Some(a) = &m.item.message.association else {
            return Ok(None);
        };
        let codes = m.item.codes.association;
        let (visibility, target) = match a.visibility {
            AssocVisibility::Undeterminable => (AssocVisibilityCode::Undeterminable, None),
            AssocVisibility::NoKind => (AssocVisibilityCode::NoKind, None),
            AssocVisibility::NotAReaction => (AssocVisibilityCode::NotAReaction, None),
            AssocVisibility::TargetUnreadable => (AssocVisibilityCode::TargetUnreadable, None),
            AssocVisibility::TargetNotUnique => (AssocVisibilityCode::TargetNotUnique, None),
            AssocVisibility::TargetInAnotherChat(t) => {
                (AssocVisibilityCode::TargetInAnotherChat, Some(t))
            }
            AssocVisibility::Folded(t) => (AssocVisibilityCode::Folded, Some(t)),
            AssocVisibility::TargetFolds(t) => (AssocVisibilityCode::TargetFolds, Some(t)),
            _ => return Err(unmapped()),
        };
        Ok(Some(AssociationRecord {
            determinable: codes.is_some_and(|c| c.present == 1),
            kind: codes.and_then(|c| c.kind).map(Code::coded),
            shape: codes.and_then(|c| c.shape).map(Code::coded),
            visibility: Code::of(visibility),
            target: target.map(|t| self.message_ref(t)),
            target_part: a.target_part.map(Decimal::of),
            emoji: a.emoji.as_ref().map(text_value).transpose()?,
        }))
    }

    fn reactions(
        &mut self,
        m: &ExportMessage,
    ) -> Result<Included<ReactionsRecord, ReactionCounts>, ExportError> {
        if !self.options.reactions {
            return Ok(Included::NotIncluded {
                option: PlanOption::Reactions,
                counts: ReactionCounts {
                    rows: u64::try_from(m.reaction_rows.len()).unwrap_or(u64::MAX),
                },
            });
        }
        let mut rows = Vec::new();
        for r in &m.reaction_rows {
            let Some(n) = self.ordinals.message(r.id) else {
                continue;
            };
            let date = self.time(r.date, false)?;
            rows.push(ReactionRowRecord {
                n,
                kind: Code::coded(r.kind),
                adds: r.adds,
                emoji: r.emoji.as_ref().map(text_value).transpose()?,
                direction: raw_direction(r.direction)?,
                handle: r.handle.map(|h| self.handle_ordinal(h)),
                date,
                target_part: r.target_part.map(Decimal::of),
            });
        }
        let mut chips = Vec::new();
        let mut unpaired = Vec::new();
        if let Some(s) = &m.item.message.reactions {
            for c in &s.chips {
                let kind = match c.kind {
                    ReactionKind::Love => ReactionKindCode::Love,
                    ReactionKind::Like => ReactionKindCode::Like,
                    ReactionKind::Dislike => ReactionKindCode::Dislike,
                    ReactionKind::Laugh => ReactionKindCode::Laugh,
                    ReactionKind::Emphasize => ReactionKindCode::Emphasize,
                    ReactionKind::Question => ReactionKindCode::Question,
                    ReactionKind::Emoji => ReactionKindCode::Emoji,
                    ReactionKind::Sticker => ReactionKindCode::Sticker,
                    _ => return Err(unmapped()),
                };
                let sender = match c.sender {
                    ChipSender::Me => ChipSenderRecord {
                        kind: Code::of(ChipSenderCode::Me),
                        handle: None,
                    },
                    ChipSender::Handle(h) => ChipSenderRecord {
                        kind: Code::of(ChipSenderCode::Handle),
                        handle: Some(self.handle_ordinal(h)),
                    },
                    ChipSender::NotRecorded => ChipSenderRecord {
                        kind: Code::of(ChipSenderCode::NotRecorded),
                        handle: None,
                    },
                    _ => return Err(unmapped()),
                };
                let state = match c.state {
                    ChipState::Shown => ChipStateCode::Shown,
                    ChipState::Hidden => ChipStateCode::Hidden,
                    ChipState::Undeterminable => ChipStateCode::Undeterminable,
                    ChipState::Added => ChipStateCode::Added,
                    ChipState::Removed => ChipStateCode::Removed,
                    _ => return Err(unmapped()),
                };
                let in_scope: Vec<Ordinal> = c
                    .rows
                    .iter()
                    .filter_map(|r| self.ordinals.message(*r))
                    .collect();
                chips.push(ChipRecord {
                    kind: Code::of(kind),
                    emoji: c.emoji.as_ref().map(text_value).transpose()?,
                    sender,
                    state: Code::of(state),
                    includes_rows_outside_scope: in_scope.len() != c.rows.len(),
                    rows: in_scope,
                    derived: true,
                });
            }
            unpaired = s.unpaired.iter().map(|u| self.message_ref(*u)).collect();
        }
        Ok(Included::Included {
            value: ReactionsRecord {
                rows,
                chips,
                rule: CHIP_RULE,
                unpaired,
            },
        })
    }

    fn attachments(
        &mut self,
        snap: &ExportSnapshot<'_>,
        m: &ExportMessage,
        files: &mut dyn AttachmentFiles,
    ) -> Result<Vec<AttachmentRecord>, ExportError> {
        let refs = &m.item.message.attachments;
        let codes = &m.item.codes.attachments;
        let facts = snap.attachment_facts(m)?;
        if facts.len() != refs.len() || codes.len() != refs.len() {
            return Err(limit(LimitKind::RowCountMismatch));
        }
        // Timeline items of more than one exported conversation keep the
        // first placement's file outcome.
        let shared = m
            .chats
            .iter()
            .filter(|x| self.ordinals.conversation(x.chat).is_some())
            .count()
            > 1;
        let mut out = Vec::with_capacity(refs.len());
        for ((r, x), c) in refs.iter().zip(&facts).zip(codes) {
            let f = &x.facts;
            if f.id != r.id {
                return Err(limit(LimitKind::RowCountMismatch));
            }
            let n = self.ordinals.attachments.ordinal(f.id);
            let source = if self.opted_in() {
                let s = snap.source_identifiers(SourceOf::Attachment(f.id))?;
                Included::Included {
                    value: AttachmentSourceRecord {
                        archive_id: Decimal::of(f.id.get()),
                        guid: opt_text(s.guid.as_ref())?,
                        original_guid: opt_text(s.original_guid.as_ref())?,
                    },
                }
            } else {
                Included::NotIncluded {
                    option: PlanOption::Identifiers,
                    counts: NoCounts {},
                }
            };
            let (access, access_reason) = match x.access {
                AttachmentAccess::Included => (AttachmentAccessCode::Included, None),
                AttachmentAccess::Withheld(w) => {
                    (AttachmentAccessCode::Withheld, Some(withheld_reason(w)?))
                }
                AttachmentAccess::Revealed(w) => {
                    (AttachmentAccessCode::Revealed, Some(withheld_reason(w)?))
                }
                _ => return Err(unmapped()),
            };
            let card = self.card(f.card)?;
            let file = if shared {
                let key = (m.item.message.id, f.id);
                match self.shared_files.get(&key).cloned() {
                    Some(prev) => {
                        self.recount(&prev, &card);
                        prev
                    }
                    None => {
                        let rec = self.file_outcome(x, &card, files)?;
                        self.shared_files.insert(key, rec.clone());
                        rec
                    }
                }
            } else {
                self.file_outcome(x, &card, files)?
            };
            let held_by = f.blob.map(|b| self.session(b.held_by));
            let current_blob = f.blob.map(|b| b.id);
            let mut observations = Vec::with_capacity(f.observations.len());
            for o in &f.observations {
                observations.push(ObservationRecord {
                    session: self.session(o.session),
                    lookup: Code::coded(o.lookup.ok_or(o.lookup_raw)),
                    copy: Code::coded(o.copy.ok_or(o.copy_raw)),
                    path_class: o.path_class.map(Code::of),
                    size_matched_manifest: o.size_matched_manifest,
                    holds_stored_file: current_blob.is_some() && o.blob == current_blob,
                });
            }
            let history = f
                .history
                .iter()
                .map(|h| self.value_history(h))
                .collect::<Result<Vec<_>, _>>()?;
            out.push(AttachmentRecord {
                n,
                source,
                file,
                access: Code::of(access),
                access_reason,
                name: text_value(&f.name)?,
                mime_type: text_value(&f.declared_type)?,
                uti: text_value(&f.uti)?,
                total_bytes: raw_int(c.declared_bytes),
                stored_bytes: c.stored_bytes.map(Decimal::of),
                is_sticker: raw_int(c.sticker),
                hide_attachment: raw_value(f.hide_attachment),
                is_outgoing: raw_int(x.is_outgoing),
                transfer_state: raw_int(x.transfer_state),
                emoji_description: text_value(&f.emoji_description)?,
                created: raw_time(x.created),
                start: raw_time(x.start),
                placed_inline: r.placed_inline,
                card,
                held_by,
                observations,
                verify: f.verify.map(|v| VerifyRecord {
                    state: Code::coded(v.state.ok_or(v.raw)),
                    may_be_stale: v.may_be_stale,
                }),
                history,
            });
        }
        Ok(out)
    }

    fn card(&mut self, c: CardState) -> Result<CardRecord, ExportError> {
        let mut rec = CardRecord {
            state: Code::of(CardStateCode::File),
            older_backup_held_by: None,
            older_backup_newest: None,
            location_lookup: None,
            unavailable_lookup: None,
            unavailable_copy: None,
        };
        let state = match c {
            CardState::File { older_backup } => {
                if let Some(o) = older_backup {
                    rec.older_backup_held_by = Some(self.session(o.held_by));
                    rec.older_backup_newest = Some(self.session(o.newest));
                }
                CardStateCode::File
            }
            CardState::NoObservation => CardStateCode::NoObservation,
            CardState::NotInBackup => CardStateCode::NotInBackup,
            CardState::NotFoundAtAssumedKey => CardStateCode::NotFoundAtAssumedKey,
            CardState::NotCopied => CardStateCode::NotCopied,
            CardState::ClassNotLookedUp => CardStateCode::ClassNotLookedUp,
            CardState::LocationUnusable(l) => {
                rec.location_lookup = Some(Code::of(l));
                CardStateCode::LocationUnusable
            }
            CardState::NotDecrypted => CardStateCode::NotDecrypted,
            CardState::CouldNotBeRead => CardStateCode::CouldNotBeRead,
            CardState::TooLarge => CardStateCode::TooLarge,
            CardState::Unavailable { lookup, copy } => {
                rec.unavailable_lookup = Some(Decimal::of(lookup));
                rec.unavailable_copy = Some(Decimal::of(copy));
                CardStateCode::Unavailable
            }
            _ => return Err(unmapped()),
        };
        rec.state = Code::of(state);
        Ok(rec)
    }

    /// Count a later placement of an attachment whose outcome `prev` was
    /// decided at the message's first placement, as `file_outcome` counts
    /// a placement: a written file counts as deduplicated (its bytes were
    /// written once), every other outcome as it did the first time.
    fn recount(&mut self, prev: &FileRecord, card: &CardRecord) {
        use AttachmentOutcome as O;
        let c = &mut self.counts;
        c.attachment_records += 1;
        let excluded = |c: &mut RecordCounts, key: &str| {
            *c.attachments_excluded.entry(key.to_owned()).or_insert(0) += 1;
        };
        let code = prev.outcome.code;
        if code == O::Copied.export_code()
            || code == O::Deduplicated.export_code()
            || code == O::BlockedExtension.export_code()
        {
            c.attachments_with_file += 1;
            c.files_deduplicated += 1;
        } else if code == O::NotRequested.export_code() {
            c.attachments_not_requested += 1;
        } else if code == O::NoStoredFile.export_code() {
            excluded(c, card.state.token);
        } else {
            excluded(c, prev.outcome.token);
        }
    }

    fn file_outcome(
        &mut self,
        x: &ExportAttachment,
        card: &CardRecord,
        files: &mut dyn AttachmentFiles,
    ) -> Result<FileRecord, ExportError> {
        self.counts.attachment_records += 1;
        let excluded = |counts: &mut RecordCounts, key: &str| {
            *counts
                .attachments_excluded
                .entry(key.to_owned())
                .or_insert(0) += 1;
        };
        let none = |o: AttachmentOutcome, sniffed: Option<Code>| FileRecord {
            outcome: Code::of(o),
            file: None,
            path: None,
            sniffed,
        };
        if x.access.is_withheld() {
            excluded(&mut self.counts, "withheldUnsent");
            return Ok(none(AttachmentOutcome::WithheldUnsent, None));
        }
        let Some(blob) = x.facts.blob.map(|b| b.id) else {
            excluded(&mut self.counts, card.state.token);
            return Ok(none(AttachmentOutcome::NoStoredFile, None));
        };
        if self.options.names_only {
            self.counts.attachments_not_requested += 1;
            return Ok(none(AttachmentOutcome::NotRequested, None));
        }
        let got = files.decide(x, blob)?;
        let sniffed = match got.sniffed {
            Some(k) => Some(Code::of(SniffedKindCode::of(k).ok_or_else(unmapped)?)),
            None => None,
        };
        let written = |outcome, ord: &mut Ordinals| -> Result<FileRecord, ExportError> {
            let n = ord.files.ordinal(blob);
            // The copy named the file by the ordinal it expected; the
            // record must carry the same one.
            if got.file != Some(n) || got.path.is_none() {
                return Err(limit(LimitKind::RowCountMismatch));
            }
            Ok(FileRecord {
                outcome: Code::of(outcome),
                file: Some(n),
                path: got.path.clone().map(Content::new),
                sniffed,
            })
        };
        Ok(match got.decision {
            FileDecision::Copied => {
                self.counts.attachments_with_file += 1;
                written(AttachmentOutcome::Copied, &mut self.ordinals)?
            }
            FileDecision::Deduplicated => {
                self.counts.attachments_with_file += 1;
                self.counts.files_deduplicated += 1;
                written(AttachmentOutcome::Deduplicated, &mut self.ordinals)?
            }
            FileDecision::BlockedExtension => {
                self.counts.attachments_with_file += 1;
                self.counts.files_blocked += 1;
                written(AttachmentOutcome::BlockedExtension, &mut self.ordinals)?
            }
            FileDecision::ChecksumMismatch => {
                excluded(&mut self.counts, "checksumMismatch");
                none(AttachmentOutcome::ChecksumMismatch, sniffed)
            }
            FileDecision::TooLargeForVolume => {
                excluded(&mut self.counts, "tooLargeForVolume");
                none(AttachmentOutcome::TooLargeForVolume, sniffed)
            }
            FileDecision::WriteFailed => {
                excluded(&mut self.counts, "writeFailed");
                none(AttachmentOutcome::WriteFailed, sniffed)
            }
        })
    }

    /// The per-conversation-file part of `m`'s record at `at` (spec
    /// §4.2): the only part that differs between conversation files.
    pub fn context(
        &mut self,
        snap: &ExportSnapshot<'_>,
        m: &ExportMessage,
        at: Placement,
    ) -> Result<ItemContext, ExportError> {
        let a = &m.item.message;
        let conversation = match at.conversation {
            None => None,
            Some(c) => Some(
                self.ordinals
                    .conversation(c)
                    .ok_or_else(|| limit(LimitKind::ScopeMismatch))?,
            ),
        };
        if at
            .conversation
            .is_some_and(|c| !m.chats.iter().any(|x| x.chat == c))
        {
            return Err(limit(LimitKind::ScopeMismatch));
        }
        let mut also_in = AlsoIn::default();
        for x in &m.chats {
            if Some(x.chat) == at.conversation {
                continue;
            }
            match self.ordinals.conversation(x.chat) {
                Some(n) => also_in.in_export.push(n),
                None => also_in.elsewhere += 1,
            }
        }
        also_in.in_export.sort_unstable();
        let folded_into = match at.folded_into {
            None => None,
            Some(t) => Some(
                self.ordinals
                    .message(t)
                    .ok_or_else(|| limit(LimitKind::ScopeMismatch))?,
            ),
        };
        let in_conversation = |id: MessageId| -> Result<Option<bool>, ExportError> {
            match at.conversation {
                None => Ok(None),
                Some(c) => Ok(Some(snap.chats_of(id)?.contains(&c))),
            }
        };
        let reply_in_conversation = match &a.reply {
            Some(ReplyRef::Resolved { originator, .. })
            | Some(ReplyRef::InAnotherConversation { originator, .. }) => {
                in_conversation(*originator)?
            }
            _ => None,
        };
        let target_in_conversation = match a.association.as_ref().map(|x| x.visibility) {
            Some(
                AssocVisibility::Folded(t)
                | AssocVisibility::TargetFolds(t)
                | AssocVisibility::TargetInAnotherChat(t),
            ) => in_conversation(t)?,
            _ => None,
        };
        let join_date = at.conversation.and_then(|c| {
            m.chats
                .iter()
                .find(|x| x.chat == c)
                .map(|x| raw_time(x.join_date))
        });
        if at.order_tie {
            self.counts.order_ties += 1;
        }
        Ok(ItemContext {
            conversation,
            also_in,
            folded_into,
            reply_in_conversation,
            target_in_conversation,
            join_date,
            order_tie: at.order_tie,
        })
    }

    // -----------------------------------------------------------------------
    // Conversations, handles, versions
    // -----------------------------------------------------------------------

    /// The record of exported conversation `chat`.
    pub fn conversation(
        &mut self,
        snap: &ExportSnapshot<'_>,
        chat: ChatId,
    ) -> Result<ConversationRecord, ExportError> {
        let n = self
            .ordinals
            .conversation(chat)
            .ok_or_else(|| limit(LimitKind::ScopeMismatch))?;
        let c = snap.chat_columns(chat)?;
        let source = if self.opted_in() {
            let s = snap.source_identifiers(SourceOf::Chat(chat))?;
            Included::Included {
                value: ConversationSourceRecord {
                    archive_id: Decimal::of(chat.get()),
                    guid: opt_text(s.guid.as_ref())?,
                    room_name: opt_text(s.room_name.as_ref())?,
                    group_id: opt_text(s.group_id.as_ref())?,
                },
            }
        } else {
            Included::NotIncluded {
                option: PlanOption::Identifiers,
                counts: NoCounts {},
            }
        };
        let participants = c
            .participants
            .iter()
            .map(|h| self.handle_ordinal(*h))
            .collect();
        let name_history = c
            .name_history
            .iter()
            .map(|h| self.value_history(h))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ConversationRecord {
            n,
            source,
            name: text_value(&c.display_name)?,
            chat_identifier: text_value(&c.chat_identifier)?,
            service_name: text_value(&c.service_name)?,
            service: c.service_class.map(Code::coded),
            style: raw_value(c.style),
            is_filtered: raw_value(c.is_filtered),
            is_archived: raw_value(c.is_archived),
            is_blackholed: raw_int(c.is_blackholed),
            is_pending_review: raw_int(c.is_pending_review),
            is_recovered: raw_int(c.is_recovered),
            properties_bytes: raw_int(c.properties_bytes),
            participants,
            name_history,
        })
    }

    /// The record of handle `id`, which an exported record references (its
    /// ordinal is assigned now if it was not yet).
    pub fn handle(
        &mut self,
        snap: &ExportSnapshot<'_>,
        id: HandleId,
    ) -> Result<HandleRecord, ExportError> {
        let n = self.handle_ordinal(id);
        let h = snap.handle_columns(id)?;
        let source = if self.opted_in() {
            let s = snap.source_identifiers(SourceOf::Handle(id))?;
            Included::Included {
                value: HandleSourceRecord {
                    archive_id: Decimal::of(id.get()),
                    uncanonicalized_id: opt_text(s.uncanonicalized_id.as_ref())?,
                    person_centric_id: opt_text(s.person_centric_id.as_ref())?,
                },
            }
        } else {
            Included::NotIncluded {
                option: PlanOption::Identifiers,
                counts: NoCounts {},
            }
        };
        Ok(HandleRecord {
            n,
            source,
            address: text_value(&h.address)?,
            service: text_value(&h.service)?,
            service_class: h.service_class.map(Code::coded),
            country: text_value(&h.country)?,
        })
    }

    /// The record of one body version of message `message` (history
    /// included), from `HistoryCursor::next_coded`.
    pub fn version(
        &mut self,
        message: MessageId,
        v: &BodyVersion,
        codes: VersionCodes,
    ) -> Result<VersionRecord, ExportError> {
        if v.more_edits > 0 {
            return Err(limit(LimitKind::TruncatedUnderExport));
        }
        let n = self
            .ordinals
            .message(message)
            .ok_or_else(|| limit(LimitKind::ScopeMismatch))?;
        let text = match &v.text {
            VersionText::Text(p) => VersionTextRecord::Text {
                pieces: pieces(p, &mut self.counts)?,
            },
            VersionText::Withheld(r) => {
                self.counts.earlier_text_withheld += 1;
                VersionTextRecord::Withheld {
                    reason: withheld_reason(*r)?,
                }
            }
            VersionText::NoText(_) => VersionTextRecord::NoText {
                reason: Code::of(match codes.display_none_reason {
                    None => NoTextReason::ReasonNotRecorded,
                    Some(Ok(r)) => no_text_reason(r),
                    Some(Err(_)) => NoTextReason::ReasonUnrecognised,
                }),
            },
            _ => return Err(unmapped()),
        };
        let mut edits = Vec::with_capacity(v.edits.len());
        for e in &v.edits {
            let (raw_type, raw) = match e.at.raw {
                None => (None, None),
                Some(RawNumber::Integer(i)) => (Some("integer"), Some(Decimal::of(i))),
                Some(RawNumber::Real(f)) => (Some("real"), Some(Decimal::real(f))),
                Some(_) => return Err(unmapped()),
            };
            let text = match &e.text {
                EditEntryText::Text(p) => EditEntryTextRecord::Text {
                    pieces: pieces(p, &mut self.counts)?,
                },
                EditEntryText::Withheld(r) => {
                    self.counts.edit_text_withheld += 1;
                    EditEntryTextRecord::Withheld {
                        reason: withheld_reason(*r)?,
                    }
                }
                EditEntryText::NoBody => EditEntryTextRecord::NoBody,
                EditEntryText::Failed(t) => EditEntryTextRecord::Failed {
                    decode_error: Code::of(*t),
                },
                EditEntryText::NotMatched => EditEntryTextRecord::NotMatched,
                _ => return Err(unmapped()),
            };
            edits.push(EditEntryRecord {
                part: Decimal::of(e.part),
                ordinal: Decimal::of(e.ordinal),
                at: EditTimeRecord {
                    kind: e.at.kind.map(Code::of),
                    raw_type,
                    raw,
                },
                text,
            });
        }
        let edit_texts = Code::of(match v.edit_texts {
            EditTextsOutcome::NotNeeded => EditTextsOutcomeCode::NotNeeded,
            EditTextsOutcome::Decoded => EditTextsOutcomeCode::Decoded,
            EditTextsOutcome::Unmatched => EditTextsOutcomeCode::Unmatched,
            EditTextsOutcome::SummaryUnreadable => EditTextsOutcomeCode::SummaryUnreadable,
            _ => return Err(unmapped()),
        });
        Ok(VersionRecord {
            message: n,
            current: v.current,
            first_session: self.session(v.first_session),
            last_session: self.session(v.last_session),
            text,
            text_unchanged: v.text_unchanged,
            edit_label: Some(Code::coded(codes.edit_label)),
            edits,
            edit_texts,
        })
    }
}

fn no_text_reason(r: oma_archive::schema::codes::DisplayNoneReason) -> NoTextReason {
    use oma_archive::schema::codes::DisplayNoneReason as D;
    match r {
        D::AttributedBodyUnusable => NoTextReason::AttributedBodyUnusable,
        D::TextEmpty => NoTextReason::TextEmpty,
        D::NoSource => NoTextReason::NoSource,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ties_are_marked_on_both_neighbours_and_never_on_undated() {
        assert_eq!(
            order_ties(&[Some(1), Some(2), Some(2), Some(3), None, None]),
            [false, true, true, false, false, false]
        );
        assert_eq!(order_ties(&[Some(5)]), [false]);
        assert!(order_ties(&[]).is_empty());
    }
}
