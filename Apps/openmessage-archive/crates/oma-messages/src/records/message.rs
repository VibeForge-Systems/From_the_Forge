//! `message`: one message row, with its body, association, reply and edit
//! facts (M3 §9, §10.8, §11).
//!
//! Rules for M4 and M5, documented here because the record cannot enforce
//! them:
//!
//! - **Direction** is `is_from_me`, raw. The S2 shared-location exception
//!   (`item_type = 4`, `other_handle ≠ 0`, `share_direction = false`) is
//!   exposed only as [`MessageRecord::from_me_derived_for_location`]; the raw
//!   value is never overwritten.
//! - **`handle_id` is not the sender on rows from the owner.** In S3 a row
//!   with `is_from_me = 1` has `handle_id` set to the other party (reviewer
//!   observation), and 0 is a placeholder. Never attribute a sender from
//!   `handle_id` alone.
//! - **`is_read`, `is_delivered` and `is_sent`:** 0 does not mean "unread"
//!   or "not delivered". Their semantics by direction and service are
//!   **UNVERIFIED**; never show them as states (I5).
//! - **`cache_has_attachments`** is set by a trigger on insert and never
//!   cleared (S3 DDL): a cache flag, not a fact about attachments.
//! - **Bodies.** `text` and the decoded `attributedBody` are both kept;
//!   neither is preferred. In S3 two of three rows have `text = ''` (not
//!   NULL) beside an `attributedBody`, so "text, else decoded" would drop
//!   bodies. Which source is current after an edit is **UNVERIFIED**.
//! - **Group-ness** is not a field (D39).

use crate::assoc::Association;
use crate::codes::{ItemKind, ServiceClass};
use crate::col::{Col, IssueKind};
use crate::db::RowReader;
use crate::error::MessagesError;
use crate::guidset::GuidSets;
use crate::redact::{RawBlob, Redacted, SqlText};
use crate::schema::known::message as c;
use crate::summary::{self, SummaryInfo};
use crate::time::{CocoaTime, TimeInterp};
use crate::typedstream::{decode_attributed_body, AttributedBody, TsBudget, TsErrorKind};
use crate::value::SqlType;

use super::handle::service_class;
use super::shape_debug;

/// Options for [`crate::SmsDb::for_each_message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MessageReadOptions {
    /// Decode `attributedBody` (default `true`). When `false`, a present
    /// body is [`AttributedOutcome::NotDecoded`].
    pub decode_attributed_body: bool,
    /// Keep the raw `attributedBody` and `message_summary_info` bytes in the
    /// record (default `false`, D32). Only values within their caps are
    /// ever read.
    pub keep_raw_blobs: bool,
    /// The typedstream budget for each body.
    pub ts_budget: TsBudget,
}

impl Default for MessageReadOptions {
    fn default() -> Self {
        MessageReadOptions {
            decode_attributed_body: true,
            keep_raw_blobs: false,
            ts_budget: TsBudget::DEFAULT,
        }
    }
}

impl MessageReadOptions {
    /// Whether to decode `attributedBody`.
    pub fn with_decode_attributed_body(mut self, on: bool) -> Self {
        self.decode_attributed_body = on;
        self
    }

    /// Whether to keep the raw blobs.
    pub fn with_keep_raw_blobs(mut self, on: bool) -> Self {
        self.keep_raw_blobs = on;
        self
    }

    /// The typedstream budget (already clamped by its own constructors).
    pub fn with_ts_budget(mut self, budget: TsBudget) -> Self {
        self.ts_budget = budget;
        self
    }
}

/// What became of a row's `attributedBody`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum AttributedOutcome {
    /// The column does not exist.
    Absent,
    /// SQL NULL.
    Null,
    /// Over its 16 MiB cap; never read.
    Oversized {
        /// Its size in bytes.
        bytes: u64,
    },
    /// Not a blob or text value.
    Unreadable(IssueKind),
    /// Present, but decoding was turned off.
    NotDecoded,
    /// Decoded. Run problems, if any, are in `run_issue`.
    Decoded(Box<AttributedBody>),
    /// Decoding failed with one of the fatal kinds.
    Failed(TsErrorKind),
}

/// A message's body sources. Both are kept; neither is preferred.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Body {
    /// `text`, as stored.
    pub text_column: Col<Redacted<SqlText>>,
    /// `attributedBody`, decoded.
    pub attributed: AttributedOutcome,
}

/// Reply facts (§11.2).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReplyInfo {
    /// `thread_originator_guid`. A row is a reply when this is non-empty.
    pub thread_originator_guid: Col<Redacted<SqlText>>,
    /// `thread_originator_part`, raw; its format is **UNVERIFIED**.
    pub thread_originator_part: Col<Redacted<SqlText>>,
    /// `reply_to_guid`. **Not** a reply marker: S3 has rows with it and no
    /// `thread_originator_guid`. Counted on its own.
    pub reply_to_guid: Col<Redacted<SqlText>>,
}

impl ReplyInfo {
    /// `true` when `thread_originator_guid` holds a non-empty value.
    pub fn is_reply(&self) -> bool {
        self.thread_originator_guid
            .value()
            .is_some_and(|g| !g.expose().is_empty())
    }
}

/// What the row records about edits and unsends (§11.3). Derived by an
/// ordered list of rules, first match wins; the labels are neutral and
/// claim no more than the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EditLabel {
    /// 1. Neither `date_edited` nor `message_summary_info` exists.
    NotRecorded,
    /// 2. The summary is unreadable or over its cap.
    SummaryUnreadable,
    /// 3. `rp` has entries and `ec` has parts (S2, S8).
    UnsentPartsAndEditHistory {
        /// `rp` entries.
        unsent: u32,
        /// `ec` parts.
        edited_parts: u32,
    },
    /// 4. `rp` has entries (S2, S8).
    UnsentParts {
        /// `rp` entries.
        parts: u32,
    },
    /// 5. `ec` has parts (S2, S4, S8).
    EditHistory {
        /// `ec` parts.
        parts: u32,
        /// The most entries any part has.
        max_entries: u32,
    },
    /// 6. `date_edited` is set, `text` is NULL or empty, `attributedBody` is
    ///    NULL, and there is no `ec` or `rp`. **Heuristic (S4), UNVERIFIED**;
    ///    never call it "unsent".
    DateEditedNoBody,
    /// 7. `date_edited` is set.
    DateEditedOnly,
    /// 8. `date_edited` is zero, NULL or absent, and the summary has no
    ///    `ec` or `rp`.
    NoEditRecorded,
    /// 9. Anything else, for example `date_edited` ambiguous or negative,
    ///    or a malformed `ec` or `rp`.
    Undetermined,
}

impl EditLabel {
    /// Apply the rules of §11.3 in order.
    ///
    /// "`date_edited` set" means a value classified as seconds or
    /// nanoseconds. "No `ec` or `rp`" means each is missing or empty (a
    /// malformed one is not "no"); a NULL or absent summary has neither.
    pub fn derive(
        date_edited: &Col<CocoaTime>,
        summary: &Col<SummaryInfo>,
        text: &Col<Redacted<SqlText>>,
        attributed: &AttributedOutcome,
    ) -> Self {
        if matches!(date_edited, Col::Absent) && matches!(summary, Col::Absent) {
            return EditLabel::NotRecorded;
        }
        if matches!(summary, Col::Unreadable(_) | Col::Oversized { .. }) {
            return EditLabel::SummaryUnreadable;
        }
        let info = summary.value();
        let unsent = info.and_then(SummaryInfo::unsent_parts).unwrap_or(0);
        let edited = info.and_then(SummaryInfo::edited_parts).unwrap_or(0);
        if unsent > 0 && edited > 0 {
            return EditLabel::UnsentPartsAndEditHistory {
                unsent,
                edited_parts: edited,
            };
        }
        if unsent > 0 {
            return EditLabel::UnsentParts { parts: unsent };
        }
        if edited > 0 {
            let max_entries = info
                .and_then(|s| s.ec.value())
                .and_then(|parts| parts.iter().map(|p| p.entries.len()).max())
                .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX));
            return EditLabel::EditHistory {
                parts: edited,
                max_entries,
            };
        }
        // From here, ec and rp each have no entries: missing, empty or bad.
        let neither = info.is_none_or(|s| !s.ec.is_bad() && !s.rp.is_bad());
        let set = matches!(
            date_edited.value().map(|t| t.interp),
            Some(TimeInterp::Seconds { .. } | TimeInterp::Nanoseconds { .. })
        );
        let text_empty = match text {
            Col::Null => true,
            Col::Value(t) => t.expose().is_empty(),
            _ => false,
        };
        if set && neither && text_empty && matches!(attributed, AttributedOutcome::Null) {
            return EditLabel::DateEditedNoBody;
        }
        if set {
            return EditLabel::DateEditedOnly;
        }
        let unset = match date_edited {
            Col::Absent | Col::Null => true,
            Col::Value(t) => t.interp == TimeInterp::Zero,
            _ => false,
        };
        if unset && neither {
            return EditLabel::NoEditRecorded;
        }
        EditLabel::Undetermined
    }
}

/// One `message` row.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct MessageRecord {
    /// `ROWID`.
    pub rowid: i64,
    /// `guid`.
    pub guid: Col<Redacted<SqlText>>,
    /// `is_from_me`, raw.
    pub is_from_me: Col<bool>,
    /// `handle_id`; not the sender on rows from the owner (module docs).
    pub handle_id: Col<i64>,
    /// `other_handle`.
    pub other_handle: Col<i64>,
    /// `destination_caller_id`.
    pub destination_caller_id: Col<Redacted<SqlText>>,
    /// `service`, raw.
    pub service: Col<Redacted<SqlText>>,
    /// The class of `service`, when it is readable text.
    pub service_class: Option<ServiceClass>,
    /// `date`.
    pub date: Col<CocoaTime>,
    /// `date_read`.
    pub date_read: Col<CocoaTime>,
    /// `date_delivered` (zero means unset; S3, observed).
    pub date_delivered: Col<CocoaTime>,
    /// `date_played`.
    pub date_played: Col<CocoaTime>,
    /// `date_edited` (zero means unset; S3, observed).
    pub date_edited: Col<CocoaTime>,
    /// `date_retracted`: read and counted, but drives no label (S4 says it
    /// does not seem to be used).
    pub date_retracted: Col<CocoaTime>,
    /// `is_read`; semantics **UNVERIFIED**.
    pub is_read: Col<bool>,
    /// `is_delivered`; semantics **UNVERIFIED**.
    pub is_delivered: Col<bool>,
    /// `is_sent`; semantics **UNVERIFIED**.
    pub is_sent: Col<bool>,
    /// `is_audio_message`.
    pub is_audio_message: Col<bool>,
    /// `cache_has_attachments`: a cache flag set on insert, never cleared.
    pub cache_has_attachments: Col<bool>,
    /// `subject`.
    pub subject: Col<Redacted<SqlText>>,
    /// `text` and `attributedBody`.
    pub body: Body,
    /// `item_type`, raw.
    pub item_type: Col<i64>,
    /// `group_action_type`, raw.
    pub group_action_type: Col<i64>,
    /// The kind derived from the two (§11.4).
    pub item_kind: ItemKind,
    /// `group_title` (a renamed group's new title).
    pub group_title: Col<Redacted<SqlText>>,
    /// `share_status`, raw.
    pub share_status: Col<i64>,
    /// `share_direction`.
    pub share_direction: Col<bool>,
    /// `Some(true)` when the S2 shared-location rule applies (`item_type` 4,
    /// `other_handle` non-zero, `share_direction` false); `None` otherwise.
    /// `is_from_me` is never overwritten.
    pub from_me_derived_for_location: Option<bool>,
    /// The association (tapback, sticker or poll vote), if any (§11.1).
    pub association: Option<Association>,
    /// Reply facts (§11.2).
    pub reply: ReplyInfo,
    /// `balloon_bundle_id`.
    pub balloon_bundle_id: Col<Redacted<SqlText>>,
    /// `expressive_send_style_id`.
    pub expressive_send_style_id: Col<Redacted<SqlText>>,
    /// `payload_data`: its storage class only.
    pub payload_data_type: Col<SqlType>,
    /// `message_summary_info`, parsed (§11.3).
    pub summary: Col<SummaryInfo>,
    /// The edit label derived from the row (§11.3).
    pub edit_label: EditLabel,
    /// `part_count`.
    pub part_count: Col<i64>,
    /// `schedule_type`, raw. A non-zero value means `date` is the planned
    /// send time (S6).
    pub schedule_type: Col<i64>,
    /// `schedule_state`, raw.
    pub schedule_state: Col<i64>,
    /// `filter_action`, raw.
    pub filter_action: Col<i64>,
    /// `filter_sub_action`, raw.
    pub filter_sub_action: Col<i64>,
    /// `error`, raw.
    pub error: Col<i64>,
    /// `was_downgraded`.
    pub was_downgraded: Col<bool>,
    /// The raw `attributedBody`, only with `keep_raw_blobs`.
    pub raw_attributed_body: Option<Redacted<RawBlob>>,
    /// The raw `message_summary_info`, only with `keep_raw_blobs`.
    pub raw_message_summary_info: Option<Redacted<RawBlob>>,
}

shape_debug!(MessageRecord {
    guid, is_from_me, handle_id, other_handle, destination_caller_id, service, date,
    date_read, date_delivered, date_played, date_edited, date_retracted, is_read,
    is_delivered, is_sent, is_audio_message, cache_has_attachments, subject, item_type,
    group_action_type, group_title, share_status, share_direction, balloon_bundle_id,
    expressive_send_style_id, payload_data_type, summary, part_count, schedule_type,
    schedule_state, filter_action, filter_sub_action, error, was_downgraded,
} kinds {
    service_class, body, item_kind, from_me_derived_for_location, association, reply,
    edit_label, raw_attributed_body, raw_message_summary_info,
});

impl MessageRecord {
    pub(crate) fn from_row(
        rowid: i64,
        r: &mut RowReader<'_, '_>,
        opts: &MessageReadOptions,
        guids: Option<&mut GuidSets>,
    ) -> Result<Self, MessagesError> {
        let guid = r.text(c::GUID)?;
        let service = r.text(c::SERVICE)?;
        let handle_id = r.integer(c::HANDLE_ID)?;
        let other_handle = r.integer(c::OTHER_HANDLE)?;
        let item_type = r.integer(c::ITEM_TYPE)?;
        let group_action_type = r.integer(c::GROUP_ACTION_TYPE)?;
        let share_direction = r.boolean(c::SHARE_DIRECTION)?;
        let date_edited = r.time(c::DATE_EDITED)?;

        // The body: text, then attributedBody (decoded, then dropped unless
        // kept).
        let text_column = r.text(c::TEXT)?;
        let (attributed, raw_attributed_body) = match r.blob(c::ATTRIBUTED_BODY)? {
            Col::Absent => (AttributedOutcome::Absent, None),
            Col::Null => (AttributedOutcome::Null, None),
            Col::Oversized { bytes } => (AttributedOutcome::Oversized { bytes }, None),
            Col::Unreadable(k) => (AttributedOutcome::Unreadable(k), None),
            Col::Value(raw) => {
                let outcome = if opts.decode_attributed_body {
                    match decode_attributed_body(raw.expose().as_bytes(), &opts.ts_budget) {
                        Ok(b) => AttributedOutcome::Decoded(Box::new(b)),
                        Err(e) => AttributedOutcome::Failed(e.kind()),
                    }
                } else {
                    AttributedOutcome::NotDecoded
                };
                (outcome, opts.keep_raw_blobs.then_some(raw))
            }
        };

        // The summary: parsed, then dropped unless kept.
        let (summary, raw_message_summary_info) = match r.blob(c::MESSAGE_SUMMARY_INFO)? {
            Col::Value(raw) => {
                let parsed = match summary::parse(raw.expose().as_bytes()) {
                    Ok(s) => Col::Value(s),
                    Err(kind) => r.unreadable(kind),
                };
                (parsed, opts.keep_raw_blobs.then_some(raw))
            }
            Col::Absent => (Col::Absent, None),
            Col::Null => (Col::Null, None),
            Col::Unreadable(k) => (Col::Unreadable(k), None),
            Col::Oversized { bytes } => (Col::Oversized { bytes }, None),
        };
        let edit_label = EditLabel::derive(&date_edited, &summary, &text_column, &attributed);

        let association = Association::from_columns(
            r.integer(c::ASSOCIATED_MESSAGE_TYPE)?,
            r.text(c::ASSOCIATED_MESSAGE_GUID)?,
            r.text(c::ASSOCIATED_MESSAGE_EMOJI)?,
            r.integer(c::ASSOCIATED_MESSAGE_RANGE_LOCATION)?,
            r.integer(c::ASSOCIATED_MESSAGE_RANGE_LENGTH)?,
        );
        let reply = ReplyInfo {
            thread_originator_guid: r.text(c::THREAD_ORIGINATOR_GUID)?,
            thread_originator_part: r.text(c::THREAD_ORIGINATOR_PART)?,
            reply_to_guid: r.text(c::REPLY_TO_GUID)?,
        };

        if let Some(g) = guids {
            if let Some(v) = guid.value() {
                g.add_message(v.expose().as_bytes());
            }
            if reply.is_reply() {
                if let Some(v) = reply.thread_originator_guid.value() {
                    g.add_reply_target(v.expose().as_bytes());
                }
            }
            if let Some(Col::Value(t)) = association.as_ref().map(|a| &a.target) {
                if let Some(v) = t.guid() {
                    g.add_association_target(v.expose().as_bytes());
                }
            }
        }

        let from_me_derived_for_location = (item_type.value() == Some(&4)
            && other_handle.value().is_some_and(|o| *o != 0)
            && share_direction.value() == Some(&false))
        .then_some(true);
        let item_kind =
            ItemKind::classify(&item_type, &group_action_type, &handle_id, &other_handle);

        Ok(MessageRecord {
            rowid,
            guid,
            is_from_me: r.boolean(c::IS_FROM_ME)?,
            destination_caller_id: r.text(c::DESTINATION_CALLER_ID)?,
            service_class: service_class(&service),
            service,
            date: r.time(c::DATE)?,
            date_read: r.time(c::DATE_READ)?,
            date_delivered: r.time(c::DATE_DELIVERED)?,
            date_played: r.time(c::DATE_PLAYED)?,
            date_edited,
            date_retracted: r.time(c::DATE_RETRACTED)?,
            is_read: r.boolean(c::IS_READ)?,
            is_delivered: r.boolean(c::IS_DELIVERED)?,
            is_sent: r.boolean(c::IS_SENT)?,
            is_audio_message: r.boolean(c::IS_AUDIO_MESSAGE)?,
            cache_has_attachments: r.boolean(c::CACHE_HAS_ATTACHMENTS)?,
            subject: r.text(c::SUBJECT)?,
            body: Body {
                text_column,
                attributed,
            },
            item_kind,
            item_type,
            group_action_type,
            group_title: r.text(c::GROUP_TITLE)?,
            share_status: r.integer(c::SHARE_STATUS)?,
            share_direction,
            from_me_derived_for_location,
            handle_id,
            other_handle,
            association,
            reply,
            balloon_bundle_id: r.text(c::BALLOON_BUNDLE_ID)?,
            expressive_send_style_id: r.text(c::EXPRESSIVE_SEND_STYLE_ID)?,
            payload_data_type: r.type_only(c::PAYLOAD_DATA)?,
            summary,
            edit_label,
            part_count: r.integer(c::PART_COUNT)?,
            schedule_type: r.integer(c::SCHEDULE_TYPE)?,
            schedule_state: r.integer(c::SCHEDULE_STATE)?,
            filter_action: r.integer(c::FILTER_ACTION)?,
            filter_sub_action: r.integer(c::FILTER_SUB_ACTION)?,
            error: r.integer(c::ERROR)?,
            was_downgraded: r.boolean(c::WAS_DOWNGRADED)?,
            raw_attributed_body,
            raw_message_summary_info,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summary::{EditEntry, EditedPart, FieldOutcome, ShapeError};
    use crate::time::SECONDS_MAX;

    fn entries(n: usize) -> Vec<EditEntry> {
        vec![
            EditEntry {
                at: None,
                body_present: true
            };
            n
        ]
    }

    fn info(
        ec: FieldOutcome<Vec<EditedPart>>,
        rp: FieldOutcome<Vec<u32>>,
        otr: FieldOutcome<u32>,
    ) -> SummaryInfo {
        SummaryInfo {
            otr,
            ec,
            rp,
            has_tmp: false,
            unknown_keys: 0,
        }
    }

    /// §19.1 `edit_label`: every combination of `date_edited` × `ec` × `rp`
    /// × `otr`, with every summary-column state and the body states rule 6
    /// reads. The rules are restated as mutually exclusive predicates; for
    /// each combination exactly one holds, and it is the label derived.
    #[test]
    fn every_combination_has_exactly_one_label() {
        let dates: Vec<Col<CocoaTime>> = vec![
            Col::Absent,
            Col::Null,
            Col::Value(CocoaTime::classify(0)),
            Col::Value(CocoaTime::classify(700_000_000)),
            Col::Value(CocoaTime::classify(700_000_000_000_000_000)),
            Col::Value(CocoaTime::classify(SECONDS_MAX)),
            Col::Value(CocoaTime::classify(-1)),
            Col::Unreadable(IssueKind::TypeMismatch),
        ];
        let ecs = || {
            vec![
                FieldOutcome::Missing,
                FieldOutcome::Value(vec![]),
                FieldOutcome::Value(vec![EditedPart {
                    part: 0,
                    entries: entries(1),
                }]),
                FieldOutcome::Value(vec![
                    EditedPart {
                        part: 0,
                        entries: entries(3),
                    },
                    EditedPart {
                        part: 2,
                        entries: entries(2),
                    },
                ]),
                FieldOutcome::Bad(ShapeError::WrongType),
            ]
        };
        let rps = || {
            vec![
                FieldOutcome::Missing,
                FieldOutcome::Value(vec![]),
                FieldOutcome::Value(vec![0, 1]),
                FieldOutcome::Bad(ShapeError::OutOfRange),
            ]
        };
        let mut summaries: Vec<Col<SummaryInfo>> = vec![
            Col::Absent,
            Col::Null,
            Col::Unreadable(IssueKind::PlistInvalid),
            Col::Oversized { bytes: 1 << 21 },
        ];
        for ec in ecs() {
            for rp in rps() {
                for otr in [FieldOutcome::Missing, FieldOutcome::Value(2)] {
                    summaries.push(Col::Value(info(ec.clone(), rp.clone(), otr)));
                }
            }
        }
        let texts: Vec<Col<Redacted<SqlText>>> = vec![
            Col::Absent,
            Col::Null,
            Col::Value(Redacted::new(SqlText::from_bytes(b""))),
            Col::Value(Redacted::new(SqlText::from_bytes(b"SYNTH-BODY"))),
        ];
        let bodies = [
            AttributedOutcome::Absent,
            AttributedOutcome::Null,
            AttributedOutcome::NotDecoded,
        ];

        let mut seen = std::collections::HashSet::new();
        let mut combos = 0;
        for d in &dates {
            for s in &summaries {
                for t in &texts {
                    for b in &bodies {
                        let got = EditLabel::derive(d, s, t, b);
                        let expected = restated(d, s, t, b);
                        assert_eq!(got, expected, "{d:?} {s:?} {t:?} {b:?}");
                        seen.insert(std::mem::discriminant(&got));
                        combos += 1;
                    }
                }
            }
        }
        assert_eq!(combos, 8 * 44 * 4 * 3);
        assert_eq!(seen.len(), 9, "every label occurs");
    }

    /// The §11.3 table as exclusive predicates.
    fn restated(
        d: &Col<CocoaTime>,
        s: &Col<SummaryInfo>,
        t: &Col<Redacted<SqlText>>,
        b: &AttributedOutcome,
    ) -> EditLabel {
        let info = s.value();
        let entries_in = |f: Option<&FieldOutcome<Vec<EditedPart>>>| {
            f.and_then(FieldOutcome::value).map_or(0, Vec::len)
        };
        let ec_parts = entries_in(info.map(|i| &i.ec));
        let rp_n = info.and_then(|i| i.rp.value()).map_or(0, Vec::len);
        let bad = info.is_some_and(|i| i.ec.is_bad() || i.rp.is_bad());
        let not_recorded = *d == Col::Absent && matches!(s, Col::Absent);
        let unreadable = matches!(s, Col::Unreadable(_) | Col::Oversized { .. });
        let readable = !not_recorded && !unreadable;
        let set = d.value().and_then(CocoaTime::unix_ns).is_some();
        let unset = matches!(d, Col::Absent | Col::Null)
            || d.value().is_some_and(|v| v.interp == TimeInterp::Zero);
        let no_body = matches!(t, Col::Null) || t.value().is_some_and(|x| x.expose().is_empty());
        let no_body = no_body && *b == AttributedOutcome::Null;
        let quiet = readable && ec_parts == 0 && rp_n == 0;
        let to32 = |n: usize| u32::try_from(n).expect("small");
        let max_entries = info
            .and_then(|i| i.ec.value())
            .and_then(|p| p.iter().map(|x| x.entries.len()).max())
            .map_or(0, to32);

        let rules = [
            (not_recorded, EditLabel::NotRecorded),
            (!not_recorded && unreadable, EditLabel::SummaryUnreadable),
            (
                readable && rp_n > 0 && ec_parts > 0,
                EditLabel::UnsentPartsAndEditHistory {
                    unsent: to32(rp_n),
                    edited_parts: to32(ec_parts),
                },
            ),
            (
                readable && rp_n > 0 && ec_parts == 0,
                EditLabel::UnsentParts { parts: to32(rp_n) },
            ),
            (
                readable && rp_n == 0 && ec_parts > 0,
                EditLabel::EditHistory {
                    parts: to32(ec_parts),
                    max_entries,
                },
            ),
            (quiet && set && !bad && no_body, EditLabel::DateEditedNoBody),
            (quiet && set && (bad || !no_body), EditLabel::DateEditedOnly),
            (quiet && !set && unset && !bad, EditLabel::NoEditRecorded),
            (quiet && !set && (!unset || bad), EditLabel::Undetermined),
        ];
        let holding: Vec<EditLabel> = rules.iter().filter(|(p, _)| *p).map(|(_, l)| *l).collect();
        assert_eq!(holding.len(), 1, "{d:?} {s:?} {t:?} {b:?}: {holding:?}");
        holding[0]
    }

    #[test]
    fn options_and_reply() {
        let o = MessageReadOptions::default();
        assert!(o.decode_attributed_body && !o.keep_raw_blobs);
        assert_eq!(o.ts_budget, TsBudget::DEFAULT);
        let o = o
            .with_decode_attributed_body(false)
            .with_keep_raw_blobs(true)
            .with_ts_budget(TsBudget::DEFAULT.with_max_input(10));
        assert!(!o.decode_attributed_body && o.keep_raw_blobs);
        assert_ne!(o.ts_budget, TsBudget::DEFAULT);

        let g = |s: &[u8]| Col::Value(Redacted::new(SqlText::from_bytes(s)));
        let reply = |t| ReplyInfo {
            thread_originator_guid: t,
            thread_originator_part: Col::Absent,
            reply_to_guid: g(b"SYNTH-GUID"),
        };
        assert!(reply(g(b"SYNTH-GUID")).is_reply());
        assert!(!reply(g(b"")).is_reply());
        assert!(!reply(Col::Null).is_reply());
        assert!(!reply(Col::Absent).is_reply());
        assert!(!reply(Col::Oversized { bytes: 300 }).is_reply());
    }
}
