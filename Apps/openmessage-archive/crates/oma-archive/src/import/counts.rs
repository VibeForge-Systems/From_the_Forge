//! `ImportCounts`: what an import did, as numbers only (spec §8.13).
//!
//! The counts are written to `import_session.counts` as JSON in the same
//! transaction as every batch, reloaded on resume and added to, never reset.
//! Nothing here is content: no value, key, digest, date or path.
//!
//! Slices 4f to 4i count the handle, chat, message and attachment passes,
//! the body versions, the three link passes, the Recently Deleted policy
//! and markers, and the attachment store (copies and placements).

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
    /// Attachments linked only to excluded messages: no row and no file
    /// (spec §8.8).
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

/// What P6 did with attachment files (spec §8.13, §9.1). Every attachment
/// row P6 wrote is counted in exactly one of the outcome fields, from
/// `copied` to `write_failed` (`size_mismatch` is counted on top).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct AttachmentFileCounts {
    /// Copied and placed as a new store file.
    pub copied: u64,
    /// Copied and placed again because the current blob's store file was
    /// missing or unusable.
    pub replaced: u64,
    /// Copied, then dropped: the store already held the file.
    pub deduplicated: u64,
    /// Copied, then an unreferenced store file of that name re-hashed equal
    /// and was adopted.
    pub adopted: u64,
    /// Copied, and an unreferenced store file of that name that did not
    /// match was replaced.
    pub orphan_replaced: u64,
    /// Not read: the current blob's file is present with the same length
    /// (presence and length only, never the bytes; spec §9.2).
    pub already_archived: u64,
    /// Copied, but a store file in the way is unusable; left untouched.
    pub store_conflicts: u64,
    /// Not read: `copy_attachments` is off.
    pub not_requested: u64,
    /// Not read: no lookup was attempted (the name's class or form).
    pub not_looked_up: u64,
    /// Not read: not listed in this backup.
    pub not_in_backup: u64,
    /// Not read: listed, but its blob or key is unusable.
    pub blob_unavailable: u64,
    /// Not read: over the attachment size limit.
    pub over_limit: u64,
    /// The copy failed: too large for the archive volume (counted per
    /// file; the import continues).
    pub too_large_for_volume: u64,
    /// The copy failed reading or decrypting the backup's file.
    pub read_failed: u64,
    /// The copy failed writing or placing the store file.
    pub write_failed: u64,
    /// Of the files read: the plaintext length differs from Manifest
    /// `Size` (stored anyway).
    pub size_mismatch: u64,
    /// Partial files that could not be removed (swept at the next open).
    pub partials_left: u64,
    /// Plaintext bytes written to partial files.
    pub bytes_copied: u64,
    /// File and directory flushes.
    pub fsyncs: u64,
    /// New `blob` rows.
    pub blobs_added: u64,
    /// Attachments whose current blob was set or moved.
    pub current_moved: u64,
}

impl AttachmentFileCounts {
    /// Add `other` to these counts.
    pub(crate) fn add(&mut self, o: &AttachmentFileCounts) {
        let pairs: [(&mut u64, u64); 21] = [
            (&mut self.copied, o.copied),
            (&mut self.replaced, o.replaced),
            (&mut self.deduplicated, o.deduplicated),
            (&mut self.adopted, o.adopted),
            (&mut self.orphan_replaced, o.orphan_replaced),
            (&mut self.already_archived, o.already_archived),
            (&mut self.store_conflicts, o.store_conflicts),
            (&mut self.not_requested, o.not_requested),
            (&mut self.not_looked_up, o.not_looked_up),
            (&mut self.not_in_backup, o.not_in_backup),
            (&mut self.blob_unavailable, o.blob_unavailable),
            (&mut self.over_limit, o.over_limit),
            (&mut self.too_large_for_volume, o.too_large_for_volume),
            (&mut self.read_failed, o.read_failed),
            (&mut self.write_failed, o.write_failed),
            (&mut self.size_mismatch, o.size_mismatch),
            (&mut self.partials_left, o.partials_left),
            (&mut self.bytes_copied, o.bytes_copied),
            (&mut self.fsyncs, o.fsyncs),
            (&mut self.blobs_added, o.blobs_added),
            (&mut self.current_moved, o.current_moved),
        ];
        for (a, b) in pairs {
            *a = a.saturating_add(b);
        }
    }

    /// The rows counted in an outcome field (every P6 row exactly once).
    pub fn outcomes(&self) -> u64 {
        [
            self.copied,
            self.replaced,
            self.deduplicated,
            self.adopted,
            self.orphan_replaced,
            self.already_archived,
            self.store_conflicts,
            self.not_requested,
            self.not_looked_up,
            self.not_in_backup,
            self.blob_unavailable,
            self.over_limit,
            self.too_large_for_volume,
            self.read_failed,
            self.write_failed,
        ]
        .iter()
        .fold(0u64, |a, b| a.saturating_add(*b))
    }
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
    /// P6: attachment rows. Records the Recently Deleted policy skipped are
    /// counted in `recently_deleted`, not here.
    pub attachments: EntityCounts,
    /// P6: attachment files and the store.
    pub attachment_files: AttachmentFileCounts,
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
