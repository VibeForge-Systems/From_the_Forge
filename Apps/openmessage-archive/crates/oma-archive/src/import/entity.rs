//! Writing the records of one keyed entity (handles and chats in slice
//! 4f): identify each record (spec §6.2), merge it into its row or insert
//! a new one (§8.7), and record the session bookkeeping (map, occurrence,
//! lineage).
//!
//! - **Keyed:** the identifying field is a readable, non-empty value and
//!   its key digest is not in the session's duplicate set. Lookup through
//!   the exact-byte partial UNIQUE index.
//! - **Unkeyed:** otherwise. The fingerprint covers every raw field, and
//!   the occurrence number comes from the durable counter, so a resumed
//!   session numbers the rest of the rows exactly as an uninterrupted one.

use oma_messages::{Col, RawIssue, Redacted, SqlText};
use rusqlite::{Connection, OptionalExtension};

use super::batch::{record, IssueDeltas, Run};
use super::bind::Cell;
use super::counts::EntityCounts;
use super::keys::fingerprint;
use super::map::MapStmts;
use super::merge::{
    attach_issues, insert, merge, read_stored, DeriveUpdate, Ident, MergeStmts, Param, TableDef,
    KEY_GUID, KEY_NATURAL, KEY_UNKEYED, STORED_ISSUES_SQL,
};
use super::preflight::{guid_digest, handle_digest};
use crate::schema::codes::IssueCode;

/// How an entity is keyed.
#[derive(Clone, Copy)]
pub(crate) enum KeyStyle {
    /// By the exact GUID bytes in this cell.
    Guid(usize),
    /// By the tagged (address, service) of these cells (handles).
    Natural { address: usize, service: usize },
}

/// One entity's write rules.
pub(crate) struct EntityKind {
    pub(crate) def: TableDef,
    pub(crate) key: KeyStyle,
    /// Record which lineage saw the row (`lineage_seen`).
    pub(crate) lineage_seen: bool,
    /// The derived columns on insert, one per `def.derived`.
    pub(crate) derive_insert: fn(&[Cell]) -> Vec<Option<i64>>,
    /// The derived columns to set when these cells changed.
    pub(crate) derive_update: DeriveUpdate,
}

/// A source record, as cells in `def.cols` order.
pub(crate) struct EntityRecord {
    pub(crate) rowid: i64,
    pub(crate) cells: Vec<Cell>,
}

/// Attach the raw values the reader kept (R2) to their cells. `rename`
/// maps a source column name to its archive name.
pub(crate) fn keep_raw(
    def: &TableDef,
    cells: &mut [Cell],
    raw: &[RawIssue],
    rename: fn(&'static str) -> &'static str,
) {
    for r in raw {
        let name = rename(r.column.name());
        if let Some(c) = def.index(name).and_then(|i| cells.get_mut(i)) {
            c.keep_raw(&r.raw);
        }
    }
}

/// The byte length of a text column's value, for the batch budget.
pub(crate) fn text_len(c: &Col<Redacted<SqlText>>) -> u64 {
    c.value()
        .map_or(8, |t| u64::try_from(t.expose().len()).unwrap_or(u64::MAX))
}

/// What [`write_records`] runs inside each record's savepoint, once the
/// record's row exists: the source rowid and the archive row id.
pub(crate) type AfterRecord<'a> = &'a mut dyn FnMut(i64, i64) -> rusqlite::Result<()>;

/// Write `records` inside the open batch. `after` runs inside each record's
/// savepoint, after its row and map are written (P6 writes the file
/// observation there).
pub(crate) fn write_records(
    tx: &Connection,
    kind: &EntityKind,
    run: &Run,
    records: impl Iterator<Item = EntityRecord>,
    counts: &mut EntityCounts,
    issues: &mut IssueDeltas,
    after: AfterRecord<'_>,
) -> rusqlite::Result<()> {
    let def = &kind.def;
    let key_cond = match kind.key {
        KeyStyle::Guid(_) => KEY_GUID,
        KeyStyle::Natural { .. } => KEY_NATURAL,
    };
    let mut by_key = tx.prepare(&def.select_sql(key_cond))?;
    let mut by_unkeyed = tx.prepare(&def.select_sql(KEY_UNKEYED))?;
    let mut stored_issues = tx.prepare(STORED_ISSUES_SQL)?;
    let mut ins = tx.prepare(&def.insert_sql())?;
    let mut ms = MergeStmts::prepare(tx)?;
    let mut maps = MapStmts::prepare(tx)?;
    let entity_code = u8::try_from(def.entity.code()).unwrap_or(0);

    for rec in records {
        let applied = record(tx, || {
            let keyed = match kind.key {
                KeyStyle::Guid(i) => rec
                    .cells
                    .get(i)
                    .and_then(guid_digest)
                    .filter(|(d, _)| !run.dups.contains(def.entity, d))
                    .map(|(_, k)| (k, None)),
                KeyStyle::Natural { address, service } => {
                    match (rec.cells.get(address), rec.cells.get(service)) {
                        (Some(a), Some(s)) => handle_digest(a, s)
                            .filter(|(d, _)| !run.dups.contains(def.entity, d))
                            .map(|(_, nk)| (nk.clone(), Some(nk))),
                        _ => None,
                    }
                }
            };
            let (ident, found) = match keyed {
                Some((key, natural_key)) => {
                    let found = by_key
                        .query_row([key.as_slice()], |r| read_stored(def, r))
                        .optional()?;
                    (Ident::Keyed { natural_key }, found)
                }
                None => {
                    let mut enc = Vec::new();
                    for (c, cell) in def.cols.iter().zip(&rec.cells) {
                        cell.fp_encode(c.code, &mut enc);
                    }
                    let fp = fingerprint(entity_code, &enc);
                    let occ = maps.next_occurrence(run.session, def.entity, &fp)?;
                    let found = by_unkeyed
                        .query_row(rusqlite::params![fp.as_slice(), occ], |r| {
                            read_stored(def, r)
                        })
                        .optional()?;
                    (Ident::Unkeyed { fp, occ }, found)
                }
            };
            let applied = match found {
                Some(mut stored) => {
                    attach_issues(def, &mut stored, &mut stored_issues)?;
                    merge(
                        def,
                        &mut ms,
                        tx,
                        &stored,
                        &rec.cells,
                        &run.rank,
                        run.session,
                        kind.derive_update,
                    )?
                }
                None => {
                    let derived: Vec<Param<'_>> = (kind.derive_insert)(&rec.cells)
                        .into_iter()
                        .map(Param::opt_int)
                        .collect();
                    insert(
                        def,
                        &mut ins,
                        &mut ms,
                        tx,
                        &ident,
                        &rec.cells,
                        &derived,
                        &run.rank,
                        run.session,
                    )?
                }
            };
            maps.map(run.session, def.entity, rec.rowid, applied.id)?;
            if kind.lineage_seen {
                maps.seen(def.entity, applied.id, run.lineage, &run.rank, run.session)?;
            }
            after(rec.rowid, applied.id)?;
            Ok((applied, ident.is_keyed()))
        })?;
        let (a, keyed) = applied;
        counts.seen = counts.seen.saturating_add(1);
        if a.inserted {
            counts.inserted = counts.inserted.saturating_add(1);
        } else {
            counts.already_present = counts.already_present.saturating_add(1);
        }
        if !keyed {
            counts.unkeyed = counts.unkeyed.saturating_add(1);
        }
        let changed = u64::try_from(a.changed.len()).unwrap_or(u64::MAX);
        counts.cells_replaced = counts.cells_replaced.saturating_add(changed);
        counts.not_applied = counts.not_applied.saturating_add(a.not_applied);
        counts.history_added = counts.history_added.saturating_add(a.history_added);
        counts.value_issue_cells = counts.value_issue_cells.saturating_add(a.value_issue_cells);
        issues.add(IssueCode::ObservationNotApplied, a.not_applied);
    }
    Ok(())
}
