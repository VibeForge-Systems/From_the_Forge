//! `ImportCounts`: what an import did, as numbers only (spec §8.13).
//!
//! The counts are written to `import_session.counts` as JSON in the same
//! transaction as every batch, reloaded on resume and added to, never reset.
//! Nothing here is content: no value, key, digest, date or path.
//!
//! Slices 4f to 4h count the handle, chat, message and attachment-row
//! passes, the body versions, the three link passes and the Recently
//! Deleted policy and markers. The attachment file counts (copies, the
//! store) arrive with the store (slice 4i).

use serde::{Deserialize, Serialize};

/// What one entity's pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct EntityCounts {
    /// Source records completed by the pass.
    pub seen: u64,
    /// Records that became a new archive row.
    pub inserted: u64,
    /// Records merged into an existing row.
    pub already_present: u64,
    /// Records imported unkeyed: no usable key, or a key that occurs more
    /// than once in the source (spec §6.2).
    pub unkeyed: u64,
    /// Stored cells (value, state and issues) an observation replaced.
    pub cells_replaced: u64,
    /// Observations in states 2–4 that did not replace the stored value.
    pub not_applied: u64,
    /// Values added to `value_history`.
    pub history_added: u64,
    /// Cells written with at least one `value_issue` row.
    pub value_issue_cells: u64,
}

/// What P3 did with body versions (`message_body`, spec §8.7, D58).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct BodyCounts {
    /// New body versions (a raw body state not seen for the message).
    pub added: u64,
    /// Observations of a body version already stored.
    pub unchanged: u64,
    /// Stored versions re-derived in place by a newer decoder.
    pub rederived: u64,
    /// Messages whose current body moved to another version.
    pub current_moved: u64,
    /// New versions whose display text is the decoded `attributedBody`.
    pub display_attributed_body: u64,
    /// New versions whose display text is the `text` column.
    pub display_text: u64,
    /// New versions whose display text is the `text` column, decoded
    /// lossily (not UTF-8).
    pub display_text_lossy: u64,
    /// New versions with no display text: `attributedBody` unusable.
    pub none_attributed_body_unusable: u64,
    /// New versions with no display text: `text` is empty.
    pub none_text_empty: u64,
    /// New versions with no display text: no source.
    pub none_no_source: u64,
    /// New versions whose decoded `attributedBody` and `text` differ.
    pub sources_differ: u64,
    /// New versions whose `attributedBody` failed to decode.
    pub decode_failed: u64,
    /// New versions whose decoded runs stopped early (a run issue).
    pub run_issues: u64,
}

/// What the Recently Deleted policy did (spec §8.8, D60).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct RecentlyDeletedCounts {
    /// Distinct message rowids P0 found in Recently Deleted.
    pub messages: u64,
    /// Recently Deleted messages not imported (the default exclusion).
    pub excluded: u64,
    /// Recently Deleted messages already in the archive: re-observed for
    /// the lineage only, not updated from the deleted row.
    pub not_updated: u64,
    /// Recently Deleted messages imported (the opt-in).
    pub imported: u64,
    /// Attachments linked only to excluded messages: no row (and, once the
    /// store exists, no file; spec §8.8).
    pub attachments_excluded: u64,
    /// P8: markers written (`message_recently_deleted` rows).
    pub marked: u64,
    /// P8: Recently Deleted join rows not marked, because their message is
    /// not mapped in this session (excluded, missing or unreadable).
    pub not_marked: u64,
}

/// What one link pass did (P4 `chat_handle`, P5 `chat_message`, P7
/// `message_attachment`; spec §8.4). The *first* side is the chat (P4,
/// P5) or the message (P7); the *second* is the handle, the message or
/// the attachment.
///
/// Every source join row is counted once in `seen`, and once in exactly one
/// of `inserted`, `already_present`, `unresolved` and
/// `skipped_recently_deleted`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct LinkCounts {
    /// Source join rows completed by the pass.
    pub seen: u64,
    /// Rows that became a new link.
    pub inserted: u64,
    /// Rows naming a link already stored (its ranks and values merged).
    pub already_present: u64,
    /// Rows not linked because a side named no row of this session: an
    /// unreadable or NULL id, or a rowid that is not in the source table (or
    /// whose row was not imported). Never inserted (spec §8.4).
    pub unresolved: u64,
    /// Of `unresolved`: rows whose first side did not resolve.
    pub unresolved_first: u64,
    /// Of `unresolved`: rows whose second side did not resolve.
    pub unresolved_second: u64,
    /// Rows skipped because a side is a Recently Deleted message the policy
    /// left out or only re-observed, or an attachment linked only to such
    /// messages (spec §8.8). Counted here, not as unresolved.
    pub skipped_recently_deleted: u64,
}

/// Everything an import counted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct ImportCounts {
    /// P1.
    pub handles: EntityCounts,
    /// P2.
    pub chats: EntityCounts,
    /// P3: message rows. Records the Recently Deleted policy skipped or
    /// only re-observed are counted in `recently_deleted`, not here.
    pub messages: EntityCounts,
    /// P3: body versions.
    pub bodies: BodyCounts,
    /// P4: chat ↔ handle links.
    pub chat_handles: LinkCounts,
    /// P5: chat ↔ message links.
    pub chat_messages: LinkCounts,
    /// P6: attachment rows (the files are slice 4i's). Records the Recently
    /// Deleted policy skipped are counted in `recently_deleted`, not here.
    pub attachments: EntityCounts,
    /// P7: message ↔ attachment links.
    pub message_attachments: LinkCounts,
    /// P0, P3, P5b, P6 and P8: the Recently Deleted policy and markers.
    pub recently_deleted: RecentlyDeletedCounts,
    /// Batch transactions committed (depends on the batch budget).
    pub batches: u64,
    /// Times this session was continued after a stop, cancel or crash.
    pub resumes: u64,
}

impl ImportCounts {
    /// The JSON stored in `import_session.counts`.
    pub(crate) fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| String::from("{}"))
    }

    /// Counts stored by an earlier run of the session. Unknown fields (from
    /// a newer build) are ignored; a session written by a newer build is
    /// never resumed anyway (§8.2).
    pub(crate) fn from_json(s: &str) -> Option<ImportCounts> {
        serde_json::from_str(s).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_round_trip_and_hold_numbers_only() {
        let mut c = ImportCounts::default();
        c.handles.seen = 3;
        c.chats.unkeyed = 1;
        c.batches = 2;
        let json = c.to_json();
        assert!(json.contains("\"alreadyPresent\":0"), "{json}");
        assert_eq!(ImportCounts::from_json(&json), Some(c.clone()));
        // Older JSON without a field loads with zero.
        let old = ImportCounts::from_json("{\"handles\":{\"seen\":4}}").expect("parses");
        assert_eq!(old.handles.seen, 4);
        assert_eq!(old.chats, EntityCounts::default());
        assert_eq!(ImportCounts::from_json("not json"), None);
    }
}
