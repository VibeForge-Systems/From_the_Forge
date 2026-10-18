//! P2: chats (spec §8.4). Keyed on the exact GUID bytes; each chat row is
//! recorded in `lineage_seen` for "not in the latest import" (§8.7).

use std::ops::ControlFlow;

use oma_messages::{
    ChatRecord, Cursor, KnownTable, MessagesCtx, MessagesError, PassOutcome, SmsDb,
};
use rusqlite::Connection;

use super::batch::{Brk, IssueDeltas, PassWriter, Run};
use super::bind::Cell;
use super::counts::ImportCounts;
use super::entity::{keep_raw, text_len, write_records, EntityKind, EntityRecord, KeyStyle};
use super::merge::TableDef;
use super::service_class;
use crate::progress::ImportPhase;
use crate::schema::codes::{Entity, Pass};

/// The index of `service_name` in the chat's cells.
const SERVICE_NAME: usize = 3;

pub(crate) struct Chats {
    kind: EntityKind,
}

fn rename(source: &'static str) -> &'static str {
    match source {
        "properties" => "properties_bytes",
        "last_read_message_timestamp" => "last_read",
        other => other,
    }
}

impl Chats {
    pub(crate) fn new() -> Chats {
        Chats {
            kind: EntityKind {
                def: TableDef::new(Entity::Chat, "chat", false, &["service_class"]),
                key: KeyStyle::Guid(0),
                lineage_seen: true,
                derive_insert: |cells| vec![service_class(cells.get(SERVICE_NAME))],
                derive_update: |changed, cells| {
                    if changed.contains(&SERVICE_NAME) {
                        vec![("service_class", service_class(cells.get(SERVICE_NAME)))]
                    } else {
                        Vec::new()
                    }
                },
            },
        }
    }

    fn cells(&self, r: &ChatRecord) -> Vec<Cell> {
        let mut cells = vec![
            Cell::text(&r.guid),
            Cell::int(&r.style),
            Cell::text(&r.chat_identifier),
            Cell::text(&r.service_name),
            Cell::text(&r.display_name),
            Cell::text(&r.room_name),
            Cell::text(&r.group_id),
            Cell::blob_size(&r.properties),
            Cell::boolean(&r.is_archived),
            Cell::int(&r.is_filtered),
            Cell::boolean(&r.is_blackholed),
            Cell::boolean(&r.is_pending_review),
            Cell::boolean(&r.is_recovered),
            Cell::time(&r.last_read_message_timestamp),
        ];
        keep_raw(&self.kind.def, &mut cells, &r.raw_issues, rename);
        cells
    }
}

impl PassWriter for Chats {
    type Record = ChatRecord;
    const PASS: Pass = Pass::Chats;
    const PHASE: ImportPhase = ImportPhase::ImportingChats;
    const SOURCE: KnownTable = KnownTable::Chat;

    fn rowid(r: &ChatRecord) -> i64 {
        r.rowid
    }

    fn size(r: &ChatRecord) -> u64 {
        [
            &r.guid,
            &r.chat_identifier,
            &r.service_name,
            &r.display_name,
            &r.room_name,
            &r.group_id,
        ]
        .iter()
        .map(|c| text_len(c))
        .fold(64u64, u64::saturating_add)
    }

    fn read(
        &self,
        db: &SmsDb<'_>,
        _conn: &Connection,
        _run: &Run,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: &mut dyn FnMut(ChatRecord) -> ControlFlow<Brk>,
    ) -> Result<PassOutcome<Brk>, MessagesError> {
        db.for_each_chat(from, ctx, f)
    }

    fn write(
        &self,
        tx: &Connection,
        run: &Run,
        records: &[ChatRecord],
        counts: &mut ImportCounts,
        issues: &mut IssueDeltas,
    ) -> rusqlite::Result<()> {
        let recs = records.iter().map(|r| EntityRecord {
            rowid: r.rowid,
            cells: self.cells(r),
        });
        write_records(
            tx,
            &self.kind,
            run,
            recs,
            &mut counts.chats,
            issues,
            &mut |_, _| Ok(()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cells_follow_the_column_list() {
        let c = Chats::new();
        let names: Vec<&str> = c.kind.def.cols.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            [
                "guid",
                "style",
                "chat_identifier",
                "service_name",
                "display_name",
                "room_name",
                "group_id",
                "properties_bytes",
                "is_archived",
                "is_filtered",
                "is_blackholed",
                "is_pending_review",
                "is_recovered",
                "last_read"
            ]
        );
        assert_eq!(c.kind.def.index("service_name"), Some(SERVICE_NAME));
        let h = super::super::handles::Handles::new();
        drop(h);
    }
}
