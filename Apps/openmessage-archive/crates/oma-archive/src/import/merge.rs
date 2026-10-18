//! The merge engine (spec §8.7, D57, D82).
//!
//! Each source record is merged by read-modify-write inside its savepoint:
//! one indexed read of the stored row, a decision per column in Rust, then
//! an `UPDATE` that sets **only the changed columns**, so the `UPDATE OF`
//! triggers fire only on real changes. A new row is a plain `INSERT`.
//!
//! The column rules ([`decide`]):
//!
//! - **Key** columns never change.
//! - **Raw** (and the raw integer of a **Time** quad): a Value or Null
//!   observation replaces the stored cell when its rank is at least the
//!   row's `last_rank`, or when the stored state is 2, 3 or 4. An
//!   observation in state 2, 3 or 4 never replaces a Value or Null; it
//!   replaces another 2/3/4 state only at a rank at least `last_rank`, and
//!   is otherwise counted as not applied.
//! - **History** columns follow the Raw rule, and every distinct observed
//!   Value or Null other than the current one is kept in `value_history`
//!   (set semantics; the old current moves there when it is replaced).
//! - A cell's `value_issue` rows are rewritten whenever the cell changes.
//! - `first_rank`/`last_rank` are the minimum and maximum observed rank;
//!   the matching `*_session_id` names the session that set it (an equal
//!   rank keeps the earlier session, so re-importing identical content
//!   changes nothing).
//!
//! `value_history.first_rank` is the lowest rank of an observation that
//! put the value into history. A value that leaves the current slot is
//! recorded at the row's `last_rank` before the replacing observation:
//! exact for two sessions, an upper bound with more (spec §8.7's
//! three-session caveat).

use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::{Connection, Statement, ToSql};
use sha2::{Digest, Sha256};

use super::bind::{Cell, Issue, Raw, Val};
use super::keys::Rank;
use crate::schema::codes::ARCHIVE_COLUMNS;
use crate::schema::codes::{ArchiveColumn, ColumnRule, Entity, KeyKind, ValueState};

/// One SQL parameter, borrowed where it holds content.
pub(crate) enum Param<'a> {
    Null,
    Int(i64),
    Bytes(&'a [u8]),
    /// A derived `TEXT` column (`message_body.display_text`).
    Text(&'a str),
    Val(&'a Val),
    Raw(&'a Raw),
}

impl ToSql for Param<'_> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        match self {
            Param::Null => Ok(ToSqlOutput::Borrowed(ValueRef::Null)),
            Param::Int(i) => Ok(ToSqlOutput::Borrowed(ValueRef::Integer(*i))),
            Param::Bytes(b) => Ok(ToSqlOutput::Borrowed(ValueRef::Blob(b))),
            Param::Text(t) => Ok(ToSqlOutput::Borrowed(ValueRef::Text(t.as_bytes()))),
            Param::Val(v) => v.to_sql(),
            Param::Raw(r) => r.to_sql(),
        }
    }
}

impl Param<'_> {
    pub(crate) fn opt_int(v: Option<i64>) -> Param<'static> {
        v.map_or(Param::Null, Param::Int)
    }

    /// The same parameter, reborrowed.
    pub(crate) fn borrow(&self) -> Param<'_> {
        match self {
            Param::Null => Param::Null,
            Param::Int(i) => Param::Int(*i),
            Param::Bytes(b) => Param::Bytes(b),
            Param::Text(t) => Param::Text(t),
            Param::Val(v) => Param::Val(v),
            Param::Raw(r) => Param::Raw(r),
        }
    }
}

/// An archive table with source-value columns.
pub(crate) struct TableDef {
    pub(crate) entity: Entity,
    pub(crate) table: &'static str,
    /// The source-value columns, in [`ARCHIVE_COLUMNS`] order.
    pub(crate) cols: Vec<ArchiveColumn>,
    /// The table has a `natural_key` column (handles).
    pub(crate) natural_key: bool,
    /// Derived columns set on insert, after the value columns.
    pub(crate) derived: &'static [&'static str],
}

impl TableDef {
    pub(crate) fn new(
        entity: Entity,
        table: &'static str,
        natural_key: bool,
        derived: &'static [&'static str],
    ) -> TableDef {
        TableDef {
            entity,
            table,
            cols: ARCHIVE_COLUMNS
                .iter()
                .filter(|c| c.entity == entity)
                .copied()
                .collect(),
            natural_key,
            derived,
        }
    }

    /// The index of a column by its archive name.
    pub(crate) fn index(&self, name: &str) -> Option<usize> {
        self.cols.iter().position(|c| c.name == name)
    }

    fn is_time(c: &ArchiveColumn) -> bool {
        c.rule == ColumnRule::Time
    }

    /// `SELECT` of the bookkeeping and every value column, by `cond`.
    pub(crate) fn select_sql(&self, cond: &str) -> String {
        let mut cols = vec![
            "id".to_owned(),
            "first_rank".to_owned(),
            "first_session_id".to_owned(),
            "last_rank".to_owned(),
            "last_session_id".to_owned(),
        ];
        for c in &self.cols {
            if Self::is_time(c) {
                cols.push(format!("{}_raw", c.name));
            } else {
                cols.push(c.name.to_owned());
            }
            cols.push(format!("{}_st", c.name));
        }
        format!(
            "SELECT {} FROM {} WHERE {}",
            cols.join(", "),
            self.table,
            cond
        )
    }

    /// The column names a cell writes.
    fn cell_columns(c: &ArchiveColumn, out: &mut Vec<String>) {
        if Self::is_time(c) {
            out.push(format!("{}_raw", c.name));
            out.push(format!("{}_st", c.name));
            out.push(format!("{}_unit", c.name));
            out.push(format!("{}_utc_ns", c.name));
        } else {
            out.push(c.name.to_owned());
            out.push(format!("{}_st", c.name));
        }
    }

    /// The `INSERT` of a new row.
    pub(crate) fn insert_sql(&self) -> String {
        let mut cols = vec!["key_kind".to_owned()];
        if self.natural_key {
            cols.push("natural_key".to_owned());
        }
        cols.push("unkeyed_fp".to_owned());
        cols.push("unkeyed_occ".to_owned());
        for c in &self.cols {
            Self::cell_columns(c, &mut cols);
        }
        cols.extend(self.derived.iter().map(|d| (*d).to_owned()));
        for c in [
            "first_session_id",
            "first_rank",
            "last_session_id",
            "last_rank",
        ] {
            cols.push(c.to_owned());
        }
        let marks: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
        format!(
            "INSERT INTO {} ({}) VALUES ({})",
            self.table,
            cols.join(", "),
            marks.join(", ")
        )
    }
}

/// How a row is identified (spec §6.2).
pub(crate) enum Ident {
    /// Keyed: the GUID is in the cells; a handle also has its natural key.
    Keyed { natural_key: Option<Vec<u8>> },
    /// Unkeyed: the fingerprint and the occurrence number.
    Unkeyed { fp: [u8; 32], occ: i64 },
}

impl Ident {
    pub(crate) fn is_keyed(&self) -> bool {
        matches!(self, Ident::Keyed { .. })
    }
}

/// A stored row, as the merge reads it.
pub(crate) struct StoredRow {
    pub(crate) id: i64,
    pub(crate) first_rank: Rank,
    pub(crate) last_rank: Rank,
    pub(crate) last_session: i64,
    pub(crate) cells: Vec<Cell>,
}

fn bad(what: &'static str) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(what.into())
}

/// Read a row selected by [`TableDef::select_sql`] (issues not yet
/// attached).
pub(crate) fn read_stored(def: &TableDef, r: &rusqlite::Row<'_>) -> rusqlite::Result<StoredRow> {
    let rank = |i: usize| -> rusqlite::Result<Rank> {
        let b: Vec<u8> = r.get(i)?;
        Rank::from_slice(&b).ok_or_else(|| bad("stored rank"))
    };
    let mut cells = Vec::with_capacity(def.cols.len());
    let mut i = 5usize;
    for _ in &def.cols {
        let value = match r.get_ref(i)? {
            ValueRef::Null => None,
            ValueRef::Integer(n) => Some(Val::Int(n)),
            ValueRef::Blob(b) => Some(Val::bytes(b)),
            ValueRef::Text(_) | ValueRef::Real(_) => return Err(bad("stored value class")),
        };
        let st: Option<i64> = r.get(i.saturating_add(1))?;
        let st = match st {
            None => None,
            Some(c) => Some(ValueState::from_code(c).ok_or_else(|| bad("stored state"))?),
        };
        cells.push(Cell {
            value,
            st,
            unit: None,
            utc_ns: None,
            issues: Vec::new(),
        });
        i = i.saturating_add(2);
    }
    Ok(StoredRow {
        id: r.get(0)?,
        first_rank: rank(1)?,
        last_rank: rank(3)?,
        last_session: r.get(4)?,
        cells,
    })
}

/// Attach the stored `value_issue` rows (`SELECT col, kind, bytes, raw`) to
/// a stored row's cells.
pub(crate) fn attach_issues(
    def: &TableDef,
    row: &mut StoredRow,
    stmt: &mut Statement<'_>,
) -> rusqlite::Result<()> {
    let mut rows = stmt.query(rusqlite::params![def.entity.code(), row.id])?;
    while let Some(r) = rows.next()? {
        let col: i64 = r.get(0)?;
        let issue = Issue {
            kind: r.get(1)?,
            bytes: r.get(2)?,
            raw: Raw::from_ref(r.get_ref(3)?),
        };
        let at = def
            .cols
            .iter()
            .position(|c| i64::from(c.code) == col)
            .ok_or_else(|| bad("value_issue column"))?;
        if let Some(cell) = row.cells.get_mut(at) {
            cell.issues.push(issue);
        }
    }
    for c in &mut row.cells {
        c.sort_issues();
    }
    Ok(())
}

/// The derived columns to set when the given cells changed (for example
/// `service_class` when the service text changed).
pub(crate) type DeriveUpdate = fn(&[usize], &[Cell]) -> Vec<(&'static str, Option<i64>)>;

/// What the column rule decides for one cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// The stored cell stays. `not_applied` for an observation in state
    /// 2–4 that was not allowed to replace it.
    Keep { not_applied: bool },
    /// The observation takes the cell; `changed` when the value, state or
    /// issues differ.
    Replace { changed: bool },
}

/// The column rule of spec §8.7 for one raw cell.
pub(crate) fn decide(stored: &Cell, observed: &Cell, rank: Rank, last_rank: Rank) -> Decision {
    let strong = observed.is_strong();
    let stored_weak = !stored.is_strong();
    let replace = if strong {
        rank >= last_rank || stored_weak
    } else {
        stored_weak && rank >= last_rank
    };
    if !replace {
        return Decision::Keep {
            not_applied: !strong,
        };
    }
    let same = stored.value == observed.value
        && stored.st == observed.st
        && stored.issues == observed.issues;
    Decision::Replace { changed: !same }
}

/// The statements every merge uses, prepared once per batch transaction.
pub(crate) struct MergeStmts<'c> {
    issue_delete: Statement<'c>,
    issue_insert: Statement<'c>,
    hist_insert: Statement<'c>,
    hist_lower: Statement<'c>,
    hist_delete: Statement<'c>,
}

impl<'c> MergeStmts<'c> {
    pub(crate) fn prepare(c: &'c Connection) -> rusqlite::Result<MergeStmts<'c>> {
        Ok(MergeStmts {
            issue_delete: c.prepare(
                "DELETE FROM value_issue WHERE entity = ?1 AND row_id = ?2 AND col = ?3",
            )?,
            issue_insert: c.prepare(
                "INSERT INTO value_issue (entity, row_id, col, kind, bytes, raw) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT DO NOTHING",
            )?,
            hist_insert: c.prepare(
                "INSERT INTO value_history \
                   (entity, row_id, col, value_hash, value, first_rank, first_session_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT DO NOTHING",
            )?,
            hist_lower: c.prepare(
                "UPDATE value_history SET first_rank = ?5, first_session_id = ?6 \
                 WHERE entity = ?1 AND row_id = ?2 AND col = ?3 AND value_hash = ?4 \
                   AND first_rank > ?5",
            )?,
            hist_delete: c.prepare(
                "DELETE FROM value_history \
                 WHERE entity = ?1 AND row_id = ?2 AND col = ?3 AND value_hash = ?4",
            )?,
        })
    }

    /// Write a cell's `value_issue` rows (the caller deleted the old ones).
    pub(crate) fn write_issues(
        &mut self,
        entity: Entity,
        row: i64,
        col: u16,
        cell: &Cell,
    ) -> rusqlite::Result<()> {
        for i in &cell.issues {
            self.issue_insert.execute(rusqlite::params![
                entity.code(),
                row,
                col,
                i.kind,
                i.bytes,
                Param::Raw(&i.raw)
            ])?;
        }
        Ok(())
    }

    /// Record `cell` in `value_history` at `rank`; returns whether it was
    /// new.
    fn history_add(
        &mut self,
        entity: Entity,
        row: i64,
        col: u16,
        cell: &Cell,
        rank: &Rank,
        session: i64,
    ) -> rusqlite::Result<bool> {
        let hash = history_hash(cell);
        let value = cell.history_value();
        let n = self.hist_insert.execute(rusqlite::params![
            entity.code(),
            row,
            col,
            hash.as_slice(),
            Param::Raw(&value),
            rank.as_bytes().as_slice(),
            session
        ])?;
        if n == 0 {
            self.hist_lower.execute(rusqlite::params![
                entity.code(),
                row,
                col,
                hash.as_slice(),
                rank.as_bytes().as_slice(),
                session
            ])?;
        }
        Ok(n > 0)
    }

    fn history_remove(
        &mut self,
        entity: Entity,
        row: i64,
        col: u16,
        cell: &Cell,
    ) -> rusqlite::Result<()> {
        let hash = history_hash(cell);
        self.hist_delete
            .execute(rusqlite::params![entity.code(), row, col, hash.as_slice()])?;
        Ok(())
    }
}

/// `value_history.value_hash`: SHA-256 of the tagged pair and the storage
/// class.
pub(crate) fn history_hash(cell: &Cell) -> [u8; 32] {
    Sha256::digest(cell.history_encoding()).into()
}

/// What merging one record did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Applied {
    /// The row's id.
    pub(crate) id: i64,
    /// The record became a new row.
    pub(crate) inserted: bool,
    /// The indices of the cells that changed.
    pub(crate) changed: Vec<usize>,
    pub(crate) not_applied: u64,
    pub(crate) history_added: u64,
    pub(crate) value_issue_cells: u64,
}

/// The values of the cells' columns, in [`TableDef::insert_sql`] order.
fn push_cell<'a>(c: &ArchiveColumn, cell: &'a Cell, out: &mut Vec<Param<'a>>) {
    out.push(cell.value.as_ref().map_or(Param::Null, Param::Val));
    out.push(Param::opt_int(cell.st_code()));
    if c.rule == ColumnRule::Time {
        out.push(Param::opt_int(cell.unit.map(|u| u.code())));
        out.push(Param::opt_int(cell.utc_ns));
    }
}

/// Insert a new row for `cells` (one per [`TableDef::cols`]) with the
/// derived values `derived` (one per [`TableDef::derived`]). Returns the
/// new row's id.
#[allow(clippy::too_many_arguments)]
pub(crate) fn insert(
    def: &TableDef,
    insert: &mut Statement<'_>,
    m: &mut MergeStmts<'_>,
    conn: &Connection,
    ident: &Ident,
    cells: &[Cell],
    derived: &[Param<'_>],
    rank: &Rank,
    session: i64,
) -> rusqlite::Result<Applied> {
    let mut p: Vec<Param<'_>> =
        Vec::with_capacity(cells.len().saturating_mul(2).saturating_add(12));
    match ident {
        Ident::Keyed { natural_key } => {
            p.push(Param::Int(KeyKind::Exact.code()));
            if def.natural_key {
                p.push(natural_key.as_deref().map_or(Param::Null, Param::Bytes));
            }
            p.push(Param::Null);
            p.push(Param::Null);
        }
        Ident::Unkeyed { fp, occ } => {
            p.push(Param::Int(KeyKind::Unkeyed.code()));
            if def.natural_key {
                p.push(Param::Null);
            }
            p.push(Param::Bytes(fp));
            p.push(Param::Int(*occ));
        }
    }
    for (c, cell) in def.cols.iter().zip(cells) {
        push_cell(c, cell, &mut p);
    }
    p.extend(derived.iter().map(Param::borrow));
    p.push(Param::Int(session));
    p.push(Param::Bytes(rank.as_bytes()));
    p.push(Param::Int(session));
    p.push(Param::Bytes(rank.as_bytes()));
    insert.execute(rusqlite::params_from_iter(p.iter()))?;
    let id = conn.last_insert_rowid();
    let mut out = Applied {
        id,
        inserted: true,
        ..Applied::default()
    };
    for (c, cell) in def.cols.iter().zip(cells) {
        if !cell.issues.is_empty() {
            m.write_issues(def.entity, id, c.code, cell)?;
            out.value_issue_cells = out.value_issue_cells.saturating_add(1);
        }
    }
    Ok(out)
}

/// Merge `cells` into the stored row. `derive` gives the derived columns
/// to set for the changed cell indices (for example `service_class` when
/// the service text changed).
#[allow(clippy::too_many_arguments)]
pub(crate) fn merge(
    def: &TableDef,
    m: &mut MergeStmts<'_>,
    conn: &Connection,
    stored: &StoredRow,
    cells: &[Cell],
    rank: &Rank,
    session: i64,
    derive: DeriveUpdate,
) -> rusqlite::Result<Applied> {
    let mut out = Applied {
        id: stored.id,
        ..Applied::default()
    };
    for (i, (c, (old, obs))) in def
        .cols
        .iter()
        .zip(stored.cells.iter().zip(cells))
        .enumerate()
    {
        if c.rule == ColumnRule::Key {
            continue;
        }
        let d = decide(old, obs, *rank, stored.last_rank);
        let changed = match d {
            Decision::Keep { not_applied } => {
                if not_applied {
                    out.not_applied = out.not_applied.saturating_add(1);
                }
                false
            }
            Decision::Replace { changed } => changed,
        };
        if changed {
            out.changed.push(i);
        }
        if c.rule == ColumnRule::History {
            let current = if changed { obs } else { old };
            let cur_hash = history_hash(current);
            if changed && old.is_strong() && history_hash(old) != cur_hash {
                let added = m.history_add(
                    def.entity,
                    stored.id,
                    c.code,
                    old,
                    &stored.last_rank,
                    stored.last_session,
                )?;
                out.history_added = out.history_added.saturating_add(u64::from(added));
            }
            if obs.is_strong() && history_hash(obs) != cur_hash {
                let added = m.history_add(def.entity, stored.id, c.code, obs, rank, session)?;
                out.history_added = out.history_added.saturating_add(u64::from(added));
            }
            if changed && obs.is_strong() {
                m.history_remove(def.entity, stored.id, c.code, obs)?;
            }
        }
    }

    // The row's ranks.
    let raise_last = *rank > stored.last_rank;
    let lower_first = *rank < stored.first_rank;
    let derived = if out.changed.is_empty() {
        Vec::new()
    } else {
        derive(&out.changed, cells)
    };
    if !out.changed.is_empty() || raise_last || lower_first {
        let mut sets: Vec<String> = Vec::new();
        let mut p: Vec<Param<'_>> = Vec::new();
        for &i in &out.changed {
            let (Some(c), Some(cell)) = (def.cols.get(i), cells.get(i)) else {
                continue;
            };
            let mut names = Vec::new();
            TableDef::cell_columns(c, &mut names);
            let mut vals = Vec::new();
            push_cell(c, cell, &mut vals);
            for (n, v) in names.into_iter().zip(vals) {
                p.push(v);
                sets.push(format!("{n} = ?{}", p.len()));
            }
        }
        for (n, v) in &derived {
            p.push(Param::opt_int(*v));
            sets.push(format!("{n} = ?{}", p.len()));
        }
        if raise_last {
            p.push(Param::Bytes(rank.as_bytes()));
            sets.push(format!("last_rank = ?{}", p.len()));
            p.push(Param::Int(session));
            sets.push(format!("last_session_id = ?{}", p.len()));
        }
        if lower_first {
            p.push(Param::Bytes(rank.as_bytes()));
            sets.push(format!("first_rank = ?{}", p.len()));
            p.push(Param::Int(session));
            sets.push(format!("first_session_id = ?{}", p.len()));
        }
        p.push(Param::Int(stored.id));
        let sql = format!(
            "UPDATE {} SET {} WHERE id = ?{}",
            def.table,
            sets.join(", "),
            p.len()
        );
        let n = conn.execute(&sql, rusqlite::params_from_iter(p.iter()))?;
        if n != 1 {
            return Err(bad("merge update"));
        }
    }
    for &i in &out.changed {
        let (Some(c), Some(cell)) = (def.cols.get(i), cells.get(i)) else {
            continue;
        };
        m.issue_delete
            .execute(rusqlite::params![def.entity.code(), stored.id, c.code])?;
        if !cell.issues.is_empty() {
            m.write_issues(def.entity, stored.id, c.code, cell)?;
            out.value_issue_cells = out.value_issue_cells.saturating_add(1);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oma_messages::{Col, IssueKind, Redacted, SqlText};

    fn text(s: &str) -> Cell {
        Cell::text(&Col::Value(Redacted::new(SqlText::from_bytes(
            s.as_bytes(),
        ))))
    }

    fn rank(k: i64) -> Rank {
        Rank::new(Some(k), &[0u8; 32])
    }

    /// Every (stored state × observed state × rank relation) cell of the
    /// §8.7 table.
    #[test]
    fn the_column_rule_table() {
        let states: Vec<(&str, Cell)> = vec![
            ("value", text("SYNTH-A")),
            ("null", Cell::text(&Col::Null)),
            ("absent", Cell::text(&Col::Absent)),
            ("oversized", Cell::text(&Col::Oversized { bytes: 9 })),
            (
                "unreadable",
                Cell::text(&Col::Unreadable(IssueKind::TypeMismatch)),
            ),
        ];
        let mut observed_states = states.clone();
        observed_states.push(("value2", text("SYNTH-B")));
        let last = rank(10);
        for (sname, stored) in &states {
            for (oname, observed) in &observed_states {
                for (rname, r) in [
                    ("lower", rank(5)),
                    ("equal", rank(10)),
                    ("higher", rank(20)),
                ] {
                    let d = decide(stored, observed, r, last);
                    let strong_obs = matches!(*oname, "value" | "null" | "value2");
                    let strong_stored = matches!(*sname, "value" | "null");
                    let replaces = if strong_obs {
                        rname != "lower" || !strong_stored
                    } else {
                        !strong_stored && rname != "lower"
                    };
                    let want = if replaces {
                        Decision::Replace {
                            changed: stored != observed,
                        }
                    } else {
                        Decision::Keep {
                            not_applied: !strong_obs,
                        }
                    };
                    assert_eq!(d, want, "stored {sname}, observed {oname}, rank {rname}");
                }
            }
        }
        // A weak observation never replaces a Value or Null, at any rank.
        for weak in [&states[2].1, &states[3].1, &states[4].1] {
            for strong in [&states[0].1, &states[1].1] {
                assert_eq!(
                    decide(strong, weak, rank(99), last),
                    Decision::Keep { not_applied: true }
                );
            }
        }
        // Same value, different issues (storage class) is a change.
        let blob = Cell::text(&Col::Value(Redacted::new(SqlText::from_bytes_with_class(
            b"SYNTH-A",
            oma_messages::TextClass::Blob,
        ))));
        assert_eq!(
            decide(&states[0].1, &blob, rank(10), last),
            Decision::Replace { changed: true }
        );
    }

    #[test]
    fn sql_is_built_from_the_static_list() {
        let def = TableDef::new(Entity::Chat, "chat", false, &["service_class"]);
        let ins = def.insert_sql();
        assert!(
            ins.starts_with("INSERT INTO chat (key_kind, unkeyed_fp, unkeyed_occ, guid, guid_st,")
        );
        assert!(ins.contains("last_read_raw, last_read_st, last_read_unit, last_read_utc_ns"));
        assert!(ins.contains("service_class, first_session_id"));
        let sel = def.select_sql("id = ?1");
        assert!(sel.contains("last_read_raw, last_read_st"));
        assert_eq!(def.index("display_name"), Some(4));
        let h = TableDef::new(Entity::Handle, "handle", true, &["service_class"]);
        assert!(h.insert_sql().contains("key_kind, natural_key, unkeyed_fp"));
        assert_eq!(h.cols.len(), 5);
    }
}
