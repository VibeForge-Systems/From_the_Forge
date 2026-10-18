//! Step 3: the probe by capability (M3 §6.2, §6.3, D28, D43).
//!
//! For each known table:
//!
//! 1. exactly one `sqlite_schema` row may name it, and it must be an
//!    ordinary table (from the scan); anything else is `NotOrdinaryTable`;
//! 2. `pragma_table_xinfo` lists its columns, with the table name bound;
//! 3. known columns match by **exact** name. A known column whose name
//!    differs only in case is treated as absent and counted in
//!    `wrong_case_columns`, so a wrong-case *required* column makes the
//!    database unsupported, although SQLite's own lookup would match it.
//!    Unknown columns are counted; their names are never kept;
//! 4. a hidden or generated known column is treated as absent and counted;
//! 5. the rowid shape is checked (below);
//! 6. `count(*)` gives the row total each pass is later compared with.
//!
//! **Rowid shape.** A table keyed by `ROWID` must declare it `INTEGER`
//! (ASCII-case-insensitive) as its only primary-key column, and
//! `pragma_index_list` must have no `pk` index: that rejects
//! `INTEGER PRIMARY KEY DESC`, which SQLite documents is not a rowid alias
//! (S20). A join table must have no column named `rowid`, `_rowid_` or `oid`
//! and must not be `WITHOUT ROWID`, so its implicit `rowid` can key the pass.

use oma_backup::{BackupError, Budgeted, StagedConnection, TextEncoding};
use rusqlite::types::ValueRef;

use super::known::{
    message, sqlite_database_properties, ColumnKind, KnownColumn, KnownColumnSet, KnownTable,
    TableLayout,
};
use super::scan::ScanFacts;
use crate::db::SmsDb;
use crate::error::{MessagesError, Stage};
use crate::progress::{MessagesCtx, MessagesPhase};

/// The result of [`SmsDb::open`]. Only `Supported` can be read; the other
/// three are values with reasons, not errors (D28).
#[derive(Debug)]
pub enum Probe<'s> {
    /// The database has the required minimum and a safe shape.
    Supported(SmsDb<'s>),
    /// The database lacks the required minimum, has a virtual table, has an
    /// unsafe rowid shape, or exceeds the schema limits (D43).
    Unsupported(Unsupported),
    /// The integrity check failed, or a pass did not stream every row.
    Damaged(DamageReason),
    /// The integrity check ran out of budget.
    Incomplete(IncompleteReason),
}

/// Why a database is unsupported, with what the probe learnt anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Unsupported {
    /// Every reason found, in probe order.
    pub reasons: Vec<UnsupportedReason>,
    /// The capabilities found before and while deciding.
    pub capabilities: Capabilities,
}

impl Unsupported {
    /// A database whose schema loads only above [`crate::SMS_DB_LIMITS`].
    pub fn schema_exceeds_limits() -> Self {
        Unsupported {
            reasons: vec![UnsupportedReason::SchemaExceedsLimits],
            capabilities: Capabilities::default(),
        }
    }
}

/// One reason a database is unsupported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum UnsupportedReason {
    /// A required table does not exist.
    MissingTable(KnownTable),
    /// A known table's name is used by something other than exactly one
    /// ordinary table (a view, an index, a virtual table).
    NotOrdinaryTable(KnownTable),
    /// A present table lacks a required column (by exact name).
    MissingRequiredColumn(KnownColumn),
    /// A table's rows cannot be keyed safely by `ROWID` or `rowid`.
    RowidShape(KnownTable),
    /// The database has a virtual table (D44). Nothing else ran.
    VirtualTablePresent,
    /// The schema loads only above [`crate::SMS_DB_LIMITS`].
    SchemaExceedsLimits,
}

/// Why a database is damaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DamageReason {
    /// `quick_check` returned something other than `ok` (D37).
    IntegrityProblems,
    /// A full pass streamed a different number of rows than `count(*)`.
    RowsNotStreamed(KnownTable),
}

/// Why a check is incomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IncompleteReason {
    /// `quick_check` ran out of its op budget.
    IntegrityCheckOverBudget,
}

/// What the probe found. Counts only; no name from the database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Capabilities {
    /// One entry per known table, in [`KnownTable::ALL`] order. Empty when
    /// the probe stopped at the scan.
    pub tables: Vec<TableCaps>,
    /// Ordinary tables outside the known list and outside `sqlite_*`.
    pub unknown_tables: u32,
    /// Views.
    pub views: u32,
    /// Triggers.
    pub triggers: u32,
    /// Indexes, without `sqlite_autoindex_*`.
    pub indexes: u32,
    /// `sqlite_stat*` tables, which can steer query plans.
    pub stat_tables: u32,
    /// `sqlite_sequence` and other internal `sqlite_*` objects.
    pub internal_objects: u32,
    /// `PRAGMA encoding`; `None` when no connection was opened.
    pub encoding: Option<TextEncoding>,
    /// `_SqliteDatabaseProperties` `_ClientVersion`, parsed as strict ASCII
    /// decimal. Its mapping to iOS versions is **UNVERIFIED**.
    pub client_version: Option<i64>,
    /// Whether `client_version` parsed.
    pub client_version_parsed: bool,
    /// Features derived from the present columns.
    pub features: Features,
}

impl Capabilities {
    /// The entry for one known table.
    pub fn table(&self, t: KnownTable) -> Option<&TableCaps> {
        self.tables.iter().find(|c| c.table == t)
    }

    /// Whether a known column is present (exact name, not generated, in an
    /// ordinary table).
    pub fn has_column(&self, c: KnownColumn) -> bool {
        self.table(c.table())
            .is_some_and(|t| t.present && t.present_columns.contains(c))
    }

    fn has(&self, t: KnownTable, index: u8) -> bool {
        self.has_column(KnownColumn::new(t, index))
    }

    pub(crate) fn from_scan(scan: &ScanFacts) -> Self {
        Capabilities {
            tables: Vec::new(),
            unknown_tables: scan.unknown_tables,
            views: scan.views,
            triggers: scan.triggers,
            indexes: scan.indexes,
            stat_tables: scan.stat_tables,
            internal_objects: scan.internal_objects,
            encoding: Some(scan.encoding),
            client_version: None,
            client_version_parsed: false,
            features: Features::default(),
        }
    }
}

/// What the probe found for one known table.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TableCaps {
    /// The table.
    pub table: KnownTable,
    /// Exactly one ordinary table has this name.
    pub present: bool,
    /// `count(*)`, when present.
    pub rows: Option<u64>,
    /// The known columns present by exact name and not generated.
    pub present_columns: KnownColumnSet,
    /// Columns that are not on the known list (names never kept).
    pub unknown_columns: u32,
    /// Known columns that are hidden or generated, treated as absent.
    pub generated_columns: u32,
    /// Known columns whose name differs only in case, treated as absent.
    pub wrong_case_columns: u32,
}

impl TableCaps {
    fn absent(table: KnownTable) -> Self {
        TableCaps {
            table,
            present: false,
            rows: None,
            present_columns: KnownColumnSet::default(),
            unknown_columns: 0,
            generated_columns: 0,
            wrong_case_columns: 0,
        }
    }
}

/// Features derived from the present columns. They are labelled "derived"
/// and are **never mapped to an iOS version** (I5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Features {
    /// `message.attributedBody`.
    pub attributed_body: bool,
    /// `message.associated_message_guid` and `associated_message_type`.
    pub tapbacks: bool,
    /// `message.associated_message_emoji`.
    pub emoji_tapbacks: bool,
    /// `message.thread_originator_guid`.
    pub replies: bool,
    /// `message.message_summary_info` with `date_edited` or `date_retracted`.
    pub edits_and_unsends: bool,
    /// The `chat_recoverable_message_join` table.
    pub recently_deleted: bool,
    /// `message.schedule_type`.
    pub scheduled: bool,
    /// `message.filter_action` or `chat.is_filtered`.
    pub filtering: bool,
    /// The `attachment` and `message_attachment_join` tables.
    pub attachments: bool,
    /// `attachment.emoji_image_short_description`.
    pub genmoji: bool,
}

impl Features {
    fn derive(caps: &Capabilities) -> Self {
        use super::known::{attachment, chat};
        let m = |i| caps.has(KnownTable::Message, i);
        let present = |t| caps.table(t).is_some_and(|c| c.present);
        Features {
            attributed_body: m(message::ATTRIBUTED_BODY),
            tapbacks: m(message::ASSOCIATED_MESSAGE_GUID) && m(message::ASSOCIATED_MESSAGE_TYPE),
            emoji_tapbacks: m(message::ASSOCIATED_MESSAGE_EMOJI),
            replies: m(message::THREAD_ORIGINATOR_GUID),
            edits_and_unsends: m(message::MESSAGE_SUMMARY_INFO)
                && (m(message::DATE_EDITED) || m(message::DATE_RETRACTED)),
            recently_deleted: present(KnownTable::ChatRecoverableMessageJoin),
            scheduled: m(message::SCHEDULE_TYPE),
            filtering: m(message::FILTER_ACTION) || caps.has(KnownTable::Chat, chat::IS_FILTERED),
            attachments: present(KnownTable::Attachment)
                && present(KnownTable::MessageAttachmentJoin),
            genmoji: caps.has(
                KnownTable::Attachment,
                attachment::EMOJI_IMAGE_SHORT_DESCRIPTION,
            ),
        }
    }
}

/// The op budget for reading `_ClientVersion`: the table has no index on
/// `key` in general, so the lookup scans it.
const PROPERTIES_OPS_BASE: u64 = 1_000_000;
const PROPERTIES_OPS_PER_ROW: u64 = 100;

/// Probe every known table. Returns the capabilities and every reason the
/// database is unsupported (empty when it is supported).
pub(crate) fn probe(
    conn: &StagedConnection<'_>,
    scan: &ScanFacts,
    ctx: &mut MessagesCtx<'_>,
) -> Result<(Capabilities, Vec<UnsupportedReason>), MessagesError> {
    let mut caps = Capabilities::from_scan(scan);
    let mut reasons = Vec::new();
    let total = u64::try_from(KnownTable::ALL.len()).ok();
    for (done, t) in KnownTable::ALL.into_iter().enumerate() {
        ctx.check()?;
        ctx.report(
            MessagesPhase::ReadingSchema,
            u64::try_from(done).unwrap_or(u64::MAX),
            total,
        );
        let entry = scan.known.get(t.index()).copied().unwrap_or_default();
        let tc = probe_table(
            conn,
            t,
            entry.entries,
            entry.is_ordinary_table(),
            &mut reasons,
        )?;
        caps.tables.push(tc);
    }
    if caps.has(
        KnownTable::SqliteDatabaseProperties,
        sqlite_database_properties::KEY,
    ) && caps.has(
        KnownTable::SqliteDatabaseProperties,
        sqlite_database_properties::VALUE,
    ) {
        let rows = caps
            .table(KnownTable::SqliteDatabaseProperties)
            .and_then(|c| c.rows)
            .unwrap_or(0);
        let parsed = client_version(conn, rows)?;
        caps.client_version = parsed;
        caps.client_version_parsed = parsed.is_some();
    }
    caps.features = Features::derive(&caps);
    ctx.report(MessagesPhase::ReadingSchema, total.unwrap_or(0), total);
    Ok((caps, reasons))
}

fn err(e: BackupError) -> MessagesError {
    MessagesError::from_conn(e, Stage::Probe)
}

fn probe_table(
    conn: &StagedConnection<'_>,
    t: KnownTable,
    entries: u32,
    ordinary: bool,
    reasons: &mut Vec<UnsupportedReason>,
) -> Result<TableCaps, MessagesError> {
    let mut tc = TableCaps::absent(t);
    if entries == 0 {
        if t.required() {
            reasons.push(UnsupportedReason::MissingTable(t));
        }
        return Ok(tc);
    }
    if !ordinary {
        reasons.push(UnsupportedReason::NotOrdinaryTable(t));
        return Ok(tc);
    }
    tc.present = true;

    if t.layout() != TableLayout::CountOnly {
        let shape = read_columns(conn, t, &mut tc)?;
        for c in t.known_columns() {
            if c.required() && !tc.present_columns.contains(c) {
                reasons.push(UnsupportedReason::MissingRequiredColumn(c));
            }
        }
        let shape_ok = match t.layout() {
            TableLayout::RowidAlias => match shape.rowid {
                // A missing ROWID is already a missing required column.
                None => true,
                Some(r) => {
                    r.declared_integer
                        && r.pk == 1
                        && shape.pk_columns == 1
                        && !has_pk_index(conn, t)?
                }
            },
            TableLayout::Join => !shape.rowid_named_column && implicit_rowid_usable(conn, t)?,
            TableLayout::CountOnly | TableLayout::Properties => true,
        };
        if !shape_ok {
            reasons.push(UnsupportedReason::RowidShape(t));
        }
    }

    let sql = format!("SELECT count(*) FROM \"{}\"", t.name());
    let n = conn
        .prepare_read(&sql)
        .map_err(err)?
        .query_row([], |r| {
            Ok(match r.get_ref(0)? {
                ValueRef::Integer(n) => u64::try_from(n).ok(),
                _ => None,
            })
        })
        .map_err(|e| err(conn.sqlite_error(&e)))?;
    tc.rows = Some(n.ok_or(MessagesError::Internal("count is not an integer"))?);
    Ok(tc)
}

/// The declared `ROWID` column, when it is present by exact name.
#[derive(Debug, Clone, Copy)]
struct RowidColumn {
    declared_integer: bool,
    pk: i64,
}

/// Facts from `pragma_table_xinfo` that the rowid-shape rule needs.
#[derive(Debug, Default)]
struct Shape {
    rowid: Option<RowidColumn>,
    pk_columns: u32,
    rowid_named_column: bool,
}

/// Match the table's columns against the known list.
fn read_columns(
    conn: &StagedConnection<'_>,
    t: KnownTable,
    tc: &mut TableCaps,
) -> Result<Shape, MessagesError> {
    let mut shape = Shape::default();
    let mut st = conn
        .prepare_read("SELECT name, type, pk, hidden FROM pragma_table_xinfo(?1)")
        .map_err(err)?;
    let mut rows = st
        .query([t.name()])
        .map_err(|e| err(conn.sqlite_error(&e)))?;
    while let Some(row) = rows.next().map_err(|e| err(conn.sqlite_error(&e)))? {
        let name = match row.get_ref(0) {
            Ok(ValueRef::Text(n)) => n,
            _ => b"",
        };
        let declared_integer =
            matches!(row.get_ref(1), Ok(ValueRef::Text(ty)) if ty.eq_ignore_ascii_case(b"INTEGER"));
        let pk = match row.get_ref(2) {
            Ok(ValueRef::Integer(p)) => p,
            _ => 0,
        };
        let hidden = match row.get_ref(3) {
            Ok(ValueRef::Integer(h)) => h,
            _ => 0,
        };
        if pk != 0 {
            shape.pk_columns = shape.pk_columns.saturating_add(1);
        }
        if [&b"rowid"[..], b"_rowid_", b"oid"]
            .iter()
            .any(|r| r.eq_ignore_ascii_case(name))
        {
            shape.rowid_named_column = true;
        }
        let exact = t.known_columns().find(|c| c.name().as_bytes() == name);
        match exact {
            Some(c) if hidden != 0 => {
                // Generated (2, 3) or hidden: never read as the known column.
                tc.generated_columns = tc.generated_columns.saturating_add(1);
                if c.kind() == Some(ColumnKind::RowId) {
                    shape.rowid = None;
                }
            }
            Some(c) => {
                tc.present_columns.insert(c.index());
                if c.kind() == Some(ColumnKind::RowId) {
                    shape.rowid = Some(RowidColumn {
                        declared_integer,
                        pk,
                    });
                }
            }
            None => {
                if t.known_columns()
                    .any(|c| c.name().as_bytes().eq_ignore_ascii_case(name))
                {
                    tc.wrong_case_columns = tc.wrong_case_columns.saturating_add(1);
                } else {
                    tc.unknown_columns = tc.unknown_columns.saturating_add(1);
                }
            }
        }
    }
    Ok(shape)
}

/// Whether the table has an index of origin `pk`: then its `INTEGER
/// PRIMARY KEY` is not a rowid alias (for example, it was declared `DESC`).
fn has_pk_index(conn: &StagedConnection<'_>, t: KnownTable) -> Result<bool, MessagesError> {
    let mut st = conn
        .prepare_read("SELECT origin FROM pragma_index_list(?1)")
        .map_err(err)?;
    let mut rows = st
        .query([t.name()])
        .map_err(|e| err(conn.sqlite_error(&e)))?;
    while let Some(row) = rows.next().map_err(|e| err(conn.sqlite_error(&e)))? {
        if matches!(row.get_ref(0), Ok(ValueRef::Text(b"pk"))) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether `SELECT rowid` prepares: it does not on a `WITHOUT ROWID` table.
fn implicit_rowid_usable(
    conn: &StagedConnection<'_>,
    t: KnownTable,
) -> Result<bool, MessagesError> {
    let sql = format!("SELECT rowid FROM \"{}\" LIMIT 0", t.name());
    match conn.prepare_read(&sql) {
        Ok(_) => Ok(true),
        Err(BackupError::Cancelled) => Err(MessagesError::Cancelled),
        Err(BackupError::Sqlite { .. }) => Ok(false),
        Err(e) => Err(err(e)),
    }
}

/// Read `_ClientVersion` as strict ASCII decimal. `None` when it is absent,
/// repeated, not text, not decimal, or over budget.
fn client_version(
    conn: &StagedConnection<'_>,
    table_rows: u64,
) -> Result<Option<i64>, MessagesError> {
    let ops = PROPERTIES_OPS_BASE.saturating_add(table_rows.saturating_mul(PROPERTIES_OPS_PER_ROW));
    let result = conn.with_op_budget(ops, |c| {
        let mut st = c.prepare_read(
            "SELECT CASE WHEN octet_length(\"value\") <= 32 THEN \"value\" END \
             FROM \"_SqliteDatabaseProperties\" WHERE \"key\" = '_ClientVersion' LIMIT 2",
        )?;
        let mut rows = st.query([]).map_err(|e| c.sqlite_error(&e))?;
        let mut found: Option<Option<i64>> = None;
        while let Some(row) = rows.next().map_err(|e| c.sqlite_error(&e))? {
            if found.is_some() {
                // Two rows for one key: ambiguous.
                return Ok(None);
            }
            found = Some(match row.get_ref(0) {
                Ok(ValueRef::Text(b)) => parse_decimal(b),
                _ => None,
            });
        }
        Ok(found.flatten())
    });
    match result {
        Ok(Budgeted::Done(v)) => Ok(v),
        Ok(Budgeted::OverBudget) => Ok(None),
        Err(e) => Err(err(e)),
    }
}

/// Strict ASCII decimal: 1 to 18 digits, nothing else.
fn parse_decimal(b: &[u8]) -> Option<i64> {
    if b.is_empty() || b.len() > 18 || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(b).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_decimal() {
        assert_eq!(parse_decimal(b"9005"), Some(9005));
        assert_eq!(parse_decimal(b"0"), Some(0));
        for bad in [
            &b""[..],
            b" 9005",
            b"9005 ",
            b"+9005",
            b"-1",
            b"90.5",
            b"1234567890123456789",
            "\u{0661}".as_bytes(),
        ] {
            assert_eq!(parse_decimal(bad), None, "{bad:?}");
        }
    }
}
