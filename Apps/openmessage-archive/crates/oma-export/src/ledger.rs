//! The column ledger (M6 spec §4.8, D142).
//!
//! Every source-value column of schema v1 (`ARCHIVE_COLUMNS`, each with a
//! never-reused code) has exactly one disposition here:
//!
//! - [`Disposition::Raw`]: exported as recorded, with its state;
//! - [`Disposition::Derived`]: exported only through what the import
//!   derived from it (the rule is named);
//! - [`Disposition::Excluded`]: not exported, for a reason code
//!   ([`ExclusionReason`]).
//!
//! Each entry also names where the value appears in the record (a key
//! path in the JSON form, `[]` for an array element), so
//! `tests/record_catalogue.rs` can check that every exported column really
//! appears. Three rules are tests:
//!
//! - no column without a disposition, and no disposition for a column
//!   that does not exist ([`check`]);
//! - the manifest's `exclusions` come from here ([`exclusions`]), never
//!   from a hand-written list;
//! - the D94 columns, `payload_data`, attachment `filename` and the
//!   history columns of the identifier opt-in never appear by default.
//!
//! Columns M5 showed only in the details are in the record (subject, the
//! account, `date_edited`, `date_retracted`, the chat flags, attachment
//! `uti`, `hide_attachment`, `is_outgoing`, `transfer_state`, `created_*`
//! and `start_*`, handle `country`, Recently Deleted `delete_*` with
//! `ck_sync_state`, and `chat_message.join_date`), and so are the raw
//! columns no viewer read carried (slice 6d reads them through
//! `ExportMessage::raw_columns` and `ChatColumns`).

use oma_archive::schema::codes::{ArchiveColumn, ARCHIVE_COLUMNS};

pub use crate::codes::ExclusionReason;

/// What happens to one archive column in an export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Disposition {
    /// Exported raw at `path`.
    Raw { path: &'static str },
    /// Exported through the derived value at `path`, by `rule`.
    Derived {
        path: &'static str,
        rule: &'static str,
    },
    /// Not exported. With [`ExclusionReason::SourceIdentifierOptIn`],
    /// `opt_in_path` is where the D141 opt-in writes it.
    Excluded {
        reason: ExclusionReason,
        opt_in_path: Option<&'static str>,
    },
}

/// One ledger entry: an `ArchiveColumn` code and its disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LedgerEntry {
    pub code: u16,
    pub disposition: Disposition,
}

const fn raw(code: u16, path: &'static str) -> LedgerEntry {
    LedgerEntry {
        code,
        disposition: Disposition::Raw { path },
    }
}

const fn derived(code: u16, path: &'static str, rule: &'static str) -> LedgerEntry {
    LedgerEntry {
        code,
        disposition: Disposition::Derived { path, rule },
    }
}

const fn excluded(code: u16, reason: ExclusionReason) -> LedgerEntry {
    LedgerEntry {
        code,
        disposition: Disposition::Excluded {
            reason,
            opt_in_path: None,
        },
    }
}

const fn opt_in(code: u16, path: &'static str) -> LedgerEntry {
    LedgerEntry {
        code,
        disposition: Disposition::Excluded {
            reason: ExclusionReason::SourceIdentifierOptIn,
            opt_in_path: Some(path),
        },
    }
}

use ExclusionReason::{D115Raw, RawSourceBytes, SessionLocalRowid, StorageInternal, D94};

/// The ledger, in code order.
pub const LEDGER: &[LedgerEntry] = &[
    // handle
    raw(101, "handle.address"),
    raw(102, "handle.service"),
    raw(103, "handle.country"),
    opt_in(104, "handle.source.value.uncanonicalizedId"),
    opt_in(105, "handle.source.value.personCentricId"),
    // chat
    opt_in(201, "conversation.source.value.guid"),
    raw(202, "conversation.style"),
    raw(203, "conversation.chatIdentifier"),
    raw(204, "conversation.serviceName"),
    raw(205, "conversation.name"),
    opt_in(206, "conversation.source.value.roomName"),
    opt_in(207, "conversation.source.value.groupId"),
    raw(208, "conversation.propertiesBytes"),
    raw(209, "conversation.isArchived"),
    raw(210, "conversation.isFiltered"),
    raw(211, "conversation.isBlackholed"),
    raw(212, "conversation.isPendingReview"),
    raw(213, "conversation.isRecovered"),
    excluded(214, D94),
    // message
    opt_in(301, "item.source.value.guid"),
    raw(302, "item.direction"),
    excluded(303, SessionLocalRowid),
    excluded(304, SessionLocalRowid),
    raw(305, "item.account"),
    raw(306, "item.raw.service"),
    raw(307, "item.date"),
    excluded(308, D94),
    excluded(309, D94),
    excluded(310, D94),
    raw(311, "item.dateEdited"),
    raw(312, "item.dateRetracted"),
    excluded(313, D94),
    excluded(314, D94),
    excluded(315, D94),
    raw(316, "item.raw.isAudioMessage"),
    raw(317, "item.raw.cacheHasAttachments"),
    raw(318, "item.subject"),
    raw(319, "item.raw.itemType"),
    raw(320, "item.raw.groupActionType"),
    raw(321, "item.event.title"),
    raw(322, "item.raw.shareStatus"),
    raw(323, "item.raw.shareDirection"),
    raw(324, "item.raw.assocType"),
    opt_in(325, "item.source.value.assocGuid"),
    raw(326, "item.association.emoji"),
    raw(327, "item.raw.assocRangeLocation"),
    raw(328, "item.raw.assocRangeLength"),
    opt_in(329, "item.source.value.threadOriginatorGuid"),
    raw(330, "item.raw.threadOriginatorPart"),
    opt_in(331, "item.source.value.replyToGuid"),
    derived(332, "item.appContent", "presence"),
    derived(333, "item.effect", "presence"),
    excluded(334, RawSourceBytes),
    raw(335, "item.raw.partCount"),
    raw(336, "item.scheduleType"),
    raw(337, "item.raw.scheduleState"),
    raw(338, "item.raw.filterAction"),
    raw(339, "item.raw.filterSubAction"),
    excluded(340, D94),
    raw(341, "item.raw.wasDowngraded"),
    // attachment
    opt_in(401, "attachment.source.value.guid"),
    opt_in(402, "attachment.source.value.originalGuid"),
    excluded(403, StorageInternal),
    raw(404, "attachment.uti"),
    raw(405, "attachment.mimeType"),
    raw(406, "attachment.name"),
    raw(407, "attachment.totalBytes"),
    raw(408, "attachment.isSticker"),
    raw(409, "attachment.hideAttachment"),
    raw(410, "attachment.isOutgoing"),
    raw(411, "attachment.transferState"),
    raw(412, "attachment.created"),
    raw(413, "attachment.start"),
    raw(414, "attachment.emojiDescription"),
    // message_body: the decoded text is exported (`item.body`, derived).
    // The plain `text` column is exported as the alternative text when it
    // differs from the displayed text, masked and withheld per §3.8
    // (`export/mask.rs`: withheld whole with earlier content, or with a
    // flagged code in the current body); the other raw inputs are not.
    derived(
        501,
        "item.alternativeText.text",
        "plainColumnWhenSourcesDiffer, masked per §3.8",
    ),
    excluded(502, RawSourceBytes),
    excluded(503, D115Raw),
    // chat_message
    raw(601, "item.context.joinDate"),
    // message_recently_deleted
    excluded(701, SessionLocalRowid),
    raw(702, "item.recentlyDeleted[].deleted"),
    raw(703, "item.recentlyDeleted[].ckSyncState"),
];

/// The ledger entry of `code`.
pub fn entry(code: u16) -> Option<LedgerEntry> {
    LEDGER.iter().copied().find(|e| e.code == code)
}

/// Why the ledger does not match the schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerGap {
    /// A v1 column without a disposition.
    Missing(u16),
    /// A disposition for a code v1 does not have.
    Unknown(u16),
    /// Two dispositions for one code.
    Duplicate(u16),
}

/// Every mismatch between [`LEDGER`] and `ARCHIVE_COLUMNS`.
pub fn check() -> Vec<LedgerGap> {
    let mut gaps = Vec::new();
    for c in ARCHIVE_COLUMNS {
        match LEDGER.iter().filter(|e| e.code == c.code).count() {
            0 => gaps.push(LedgerGap::Missing(c.code)),
            1 => {}
            _ => gaps.push(LedgerGap::Duplicate(c.code)),
        }
    }
    for e in LEDGER {
        if ArchiveColumn::from_code(e.code).is_none() {
            gaps.push(LedgerGap::Unknown(e.code));
        }
    }
    gaps
}

/// The manifest's `exclusions`: every reason the ledger uses, by code
/// (spec §4.8: generated, never hand-written).
pub fn exclusions() -> Vec<ExclusionReason> {
    let mut out: Vec<ExclusionReason> = LEDGER
        .iter()
        .filter_map(|e| match e.disposition {
            Disposition::Excluded { reason, .. } => Some(reason),
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_v1_column_has_exactly_one_disposition() {
        assert_eq!(check(), Vec::<LedgerGap>::new());
        assert!(LEDGER.windows(2).all(|w| match w {
            [a, b] => a.code < b.code,
            _ => true,
        }));
    }

    #[test]
    fn exclusions_come_from_the_ledger() {
        assert_eq!(exclusions(), ExclusionReason::ALL.to_vec());
    }

    #[test]
    fn read_delivery_and_sent_state_is_never_exported() {
        for code in [214, 308, 309, 310, 313, 314, 315, 340] {
            assert_eq!(
                entry(code).map(|e| e.disposition),
                Some(Disposition::Excluded {
                    reason: ExclusionReason::D94,
                    opt_in_path: None
                }),
                "{code}"
            );
        }
        // Spec §3.8: these four never appear in a record, opt-in or not
        // for payload_data and filename, by default for the handle ids.
        for code in [334, 403] {
            assert!(matches!(
                entry(code).map(|e| e.disposition),
                Some(Disposition::Excluded {
                    opt_in_path: None,
                    ..
                })
            ));
        }
        for code in [104, 105] {
            assert!(matches!(
                entry(code).map(|e| e.disposition),
                Some(Disposition::Excluded {
                    reason: ExclusionReason::SourceIdentifierOptIn,
                    ..
                })
            ));
        }
    }
}
