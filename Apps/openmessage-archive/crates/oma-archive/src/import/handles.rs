//! P1: handles (spec §8.4). One archive row per (address, service), keyed
//! on the exact bytes of both (S5's own uniqueness; no normalisation).

use std::ops::ControlFlow;

use oma_messages::{
    Cursor, HandleRecord, KnownTable, MessagesCtx, MessagesError, PassOutcome, SmsDb,
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

pub(crate) struct Handles {
    kind: EntityKind,
}

fn rename(source: &'static str) -> &'static str {
    match source {
        "id" => "address",
        other => other,
    }
}

/// The handle table's merge definition.
pub(crate) fn table_def() -> TableDef {
    TableDef::new(Entity::Handle, "handle", true, &["service_class"])
}

impl Handles {
    pub(crate) fn new() -> Handles {
        Handles {
            kind: EntityKind {
                def: table_def(),
                key: KeyStyle::Natural {
                    address: 0,
                    service: 1,
                },
                lineage_seen: false,
                derive_insert: |cells| vec![service_class(cells.get(1))],
                // The service is a key column: it never changes.
                derive_update: |_, _| Vec::new(),
            },
        }
    }

    fn cells(&self, r: &HandleRecord) -> Vec<Cell> {
        let mut cells = vec![
            Cell::text(&r.id),
            Cell::text(&r.service),
            Cell::text(&r.country),
            Cell::text(&r.uncanonicalized_id),
            Cell::text(&r.person_centric_id),
        ];
        keep_raw(&self.kind.def, &mut cells, &r.raw_issues, rename);
        cells
    }
}

impl PassWriter for Handles {
    type Record = HandleRecord;
    const PASS: Pass = Pass::Handles;
    const PHASE: ImportPhase = ImportPhase::ImportingHandles;
    const SOURCE: KnownTable = KnownTable::Handle;

    fn rowid(r: &HandleRecord) -> i64 {
        r.rowid
    }

    fn size(r: &HandleRecord) -> u64 {
        [
            &r.id,
            &r.service,
            &r.country,
            &r.uncanonicalized_id,
            &r.person_centric_id,
        ]
        .iter()
        .map(|c| text_len(c))
        .fold(0u64, u64::saturating_add)
    }

    fn read(
        &self,
        db: &SmsDb<'_>,
        _conn: &Connection,
        _run: &Run,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: &mut dyn FnMut(HandleRecord) -> ControlFlow<Brk>,
    ) -> Result<PassOutcome<Brk>, MessagesError> {
        db.for_each_handle(from, ctx, f)
    }

    fn write(
        &self,
        tx: &Connection,
        run: &Run,
        records: &[HandleRecord],
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
            &mut counts.handles,
            issues,
            &mut |_, _| Ok(()),
        )
    }
}
