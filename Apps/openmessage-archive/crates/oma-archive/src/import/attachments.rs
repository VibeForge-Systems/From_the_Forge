//! P6, the attachment **rows** (spec §8.4): keyed on the exact GUID bytes,
//! merged by the column rules of §8.7, mapped for P7, and recorded in
//! `lineage_seen`.
//!
//! **No file is read or copied here.** The content-addressed store (the
//! locator, `blob`, `blob_intent`, `attachment_observation` and
//! `current_blob_id`) is slice 4i, which adds it to this pass; until then
//! every attachment row has no blob.
//!
//! An attachment linked only to Recently Deleted messages that the policy
//! left out (P5b) gets no row and is counted (§8.8). It is skipped before
//! it is identified, so it takes no occurrence number.

use std::ops::ControlFlow;

use oma_messages::{
    AttachmentRecord, Cursor, KnownTable, MessagesCtx, MessagesError, PassOutcome, SmsDb,
};
use rusqlite::Connection;

use super::batch::{Brk, IssueDeltas, PassWriter, Run};
use super::bind::Cell;
use super::counts::ImportCounts;
use super::entity::{keep_raw, text_len, write_records, EntityKind, EntityRecord, KeyStyle};
use super::merge::TableDef;
use super::rd::RdSkip;
use crate::progress::ImportPhase;
use crate::schema::codes::{Entity, Pass};

pub(crate) struct Attachments<'a> {
    kind: EntityKind,
    rd: &'a RdSkip,
}

fn rename(source: &'static str) -> &'static str {
    match source {
        "created_date" => "created",
        "start_date" => "start",
        "emoji_image_short_description" => "emoji_description",
        other => other,
    }
}

impl<'a> Attachments<'a> {
    pub(crate) fn new(rd: &'a RdSkip) -> Attachments<'a> {
        Attachments {
            kind: EntityKind {
                def: TableDef::new(Entity::Attachment, "attachment", false, &[]),
                key: KeyStyle::Guid(0),
                lineage_seen: true,
                derive_insert: |_| Vec::new(),
                derive_update: |_, _| Vec::new(),
            },
            rd,
        }
    }

    fn cells(&self, r: &AttachmentRecord) -> Vec<Cell> {
        let mut cells = vec![
            Cell::text(&r.guid),
            Cell::text(&r.original_guid),
            Cell::text(&r.filename),
            Cell::text(&r.uti),
            Cell::text(&r.mime_type),
            Cell::text(&r.transfer_name),
            Cell::int(&r.total_bytes),
            Cell::boolean(&r.is_sticker),
            Cell::boolean(&r.hide_attachment),
            Cell::boolean(&r.is_outgoing),
            Cell::int(&r.transfer_state),
            Cell::time(&r.created_date),
            Cell::time(&r.start_date),
            Cell::text(&r.emoji_image_short_description),
        ];
        keep_raw(&self.kind.def, &mut cells, &r.raw_issues, rename);
        cells
    }
}

impl PassWriter for Attachments<'_> {
    type Record = AttachmentRecord;
    const PASS: Pass = Pass::Attachments;
    const PHASE: ImportPhase = ImportPhase::ImportingAttachments;
    const SOURCE: KnownTable = KnownTable::Attachment;

    fn rowid(r: &AttachmentRecord) -> i64 {
        r.rowid
    }

    fn size(r: &AttachmentRecord) -> u64 {
        [
            &r.guid,
            &r.original_guid,
            &r.filename,
            &r.uti,
            &r.mime_type,
            &r.transfer_name,
            &r.emoji_image_short_description,
        ]
        .iter()
        .map(|c| text_len(c))
        .fold(128u64, u64::saturating_add)
    }

    fn read(
        &self,
        db: &SmsDb<'_>,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: &mut dyn FnMut(AttachmentRecord) -> ControlFlow<Brk>,
    ) -> Result<PassOutcome<Brk>, MessagesError> {
        db.for_each_attachment(from, None, ctx, f)
    }

    fn write(
        &self,
        tx: &Connection,
        run: &Run,
        records: &[AttachmentRecord],
        counts: &mut ImportCounts,
        issues: &mut IssueDeltas,
    ) -> rusqlite::Result<()> {
        let mut excluded = 0u64;
        let recs: Vec<EntityRecord> = records
            .iter()
            .filter(|r| {
                let skip = self.rd.skips_attachment(r.rowid);
                if skip {
                    excluded = excluded.saturating_add(1);
                }
                !skip
            })
            .map(|r| EntityRecord {
                rowid: r.rowid,
                cells: self.cells(r),
            })
            .collect();
        write_records(
            tx,
            &self.kind,
            run,
            recs.into_iter(),
            &mut counts.attachments,
            issues,
        )?;
        let rd = &mut counts.recently_deleted;
        rd.attachments_excluded = rd.attachments_excluded.saturating_add(excluded);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cells_follow_the_column_list() {
        let rd = RdSkip::default();
        let a = Attachments::new(&rd);
        let names: Vec<&str> = a.kind.def.cols.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            [
                "guid",
                "original_guid",
                "filename",
                "uti",
                "mime_type",
                "transfer_name",
                "total_bytes",
                "is_sticker",
                "hide_attachment",
                "is_outgoing",
                "transfer_state",
                "created",
                "start",
                "emoji_description"
            ]
        );
        for (source, archive) in [
            ("created_date", "created"),
            ("start_date", "start"),
            ("emoji_image_short_description", "emoji_description"),
            ("filename", "filename"),
        ] {
            assert_eq!(rename(source), archive);
        }
    }
}
