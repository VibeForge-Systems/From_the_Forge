//! The schema fingerprint (spec §7.3, C1.8, C3.13).
//!
//! The expected objects for a version are built in an in-memory database on
//! the running SQLite, from `v1.sql` plus every migration up to that
//! version. The archive's `sqlite_schema` must match them:
//!
//! - every object we write (tables, indexes, triggers, the `CREATE VIRTUAL
//!   TABLE` statement, automatic indexes) exactly on `(type, name,
//!   tbl_name, sql)`;
//! - the FTS5 shadow tables (`message_fts_data`, `_idx`, `_docsize`,
//!   `_config`) by type, name and table only, because FTS5 writes their DDL;
//! - `sqlite_stat1` and `sqlite_stat4` are excluded and reported (the app
//!   never runs `ANALYZE`);
//! - anything else, or any difference, is `Damaged(SchemaMismatch)`.
//!
//! It reads only `sqlite_schema`, at most a few rows more than expected, so
//! a planted schema with thousands of objects costs no more than a normal
//! one.

use std::collections::BTreeMap;

use rusqlite::Connection;

use crate::conn::open_memory;
use crate::error::{ArchiveError, DamageKind, Stage};
use crate::schema::{build_schema, SchemaPlan};

/// FTS5's shadow tables for `message_fts` (external content, so no
/// `_content` table).
const SHADOW_TABLES: [&str; 4] = [
    "message_fts_data",
    "message_fts_idx",
    "message_fts_docsize",
    "message_fts_config",
];

/// Statistics tables that are tolerated and reported.
const STAT_TABLES: [&str; 2] = ["sqlite_stat1", "sqlite_stat4"];

/// Rows read beyond the expected count before giving up.
const EXTRA_ROWS_READ: usize = 8;

/// One `sqlite_schema` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchemaRow {
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) tbl_name: String,
    pub(crate) sql: Option<String>,
}

/// What a passing fingerprint check noticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct FingerprintFacts {
    /// `sqlite_stat1` or `sqlite_stat4` is present.
    pub(crate) stat_tables: bool,
}

fn read_rows(conn: &Connection, limit: usize) -> rusqlite::Result<Option<Vec<SchemaRow>>> {
    let mut stmt = conn
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY rowid LIMIT ?1")?;
    let limit_sql = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut rows = stmt.query([limit_sql])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let kind: Option<String> = r.get(0).ok();
        let name: Option<String> = r.get(1).ok();
        let tbl_name: Option<String> = r.get(2).ok();
        let sql: Result<Option<String>, _> = r.get(3);
        match (kind, name, tbl_name, sql) {
            (Some(kind), Some(name), Some(tbl_name), Ok(sql)) => out.push(SchemaRow {
                kind,
                name,
                tbl_name,
                sql,
            }),
            // A row of the wrong storage class is not ours.
            _ => return Ok(None),
        }
    }
    Ok(Some(out))
}

/// The expected objects for `version` under `plan`.
pub(crate) fn expected(plan: &SchemaPlan, version: u32) -> Result<Vec<SchemaRow>, ArchiveError> {
    if plan.chain(1, version).is_none() {
        // A version this build has no recipe for cannot be checked.
        return Err(ArchiveError::Damaged(DamageKind::SchemaMismatch));
    }
    let mem = open_memory()?;
    build_schema(&mem, plan, version, Stage::Fingerprint)?;
    let mut rows = read_rows(&mem, usize::MAX)
        .map_err(|e| ArchiveError::sqlite(Stage::Fingerprint, &e))?
        .ok_or(ArchiveError::Internal("unreadable expected schema"))?;
    if crate::fault::skew_shadow_ddl() {
        for r in &mut rows {
            if SHADOW_TABLES.contains(&r.name.as_str()) {
                r.sql = r
                    .sql
                    .as_ref()
                    .map(|s| format!("{s} /* other FTS5 release */"));
            }
        }
    }
    Ok(rows)
}

/// Compare `actual` against `expected`.
pub(crate) fn compare(
    expected: &[SchemaRow],
    actual: &[SchemaRow],
) -> Result<FingerprintFacts, ArchiveError> {
    let mismatch = || ArchiveError::Damaged(DamageKind::SchemaMismatch);
    let mut want: BTreeMap<(&str, &str), &SchemaRow> = BTreeMap::new();
    for r in expected {
        want.insert((r.kind.as_str(), r.name.as_str()), r);
    }
    let mut facts = FingerprintFacts::default();
    let mut matched = 0usize;
    let mut seen: BTreeMap<(&str, &str), ()> = BTreeMap::new();
    for r in actual {
        if r.kind == "table" && STAT_TABLES.contains(&r.name.as_str()) {
            facts.stat_tables = true;
            continue;
        }
        let key = (r.kind.as_str(), r.name.as_str());
        if seen.insert(key, ()).is_some() {
            return Err(mismatch());
        }
        let w = want.get(&key).ok_or_else(mismatch)?;
        let same = if SHADOW_TABLES.contains(&r.name.as_str()) {
            w.tbl_name == r.tbl_name
        } else {
            w.tbl_name == r.tbl_name && w.sql == r.sql
        };
        if !same {
            return Err(mismatch());
        }
        matched = matched.saturating_add(1);
    }
    if matched != want.len() {
        return Err(mismatch());
    }
    Ok(facts)
}

/// Check `conn`'s schema against the expected objects for `version`. The
/// caller wraps this in an op budget where the file is untrusted.
pub(crate) fn check(
    conn: &Connection,
    expected_rows: &[SchemaRow],
) -> Result<FingerprintFacts, ArchiveError> {
    check_raw(conn, expected_rows).map_err(|e| ArchiveError::sqlite(Stage::Fingerprint, &e))?
}

/// [`check`] that keeps rusqlite's error type, for use inside a budgeted
/// read: `Ok(Err(..))` is a mismatch, `Err(..)` a SQLite error.
pub(crate) fn check_raw(
    conn: &Connection,
    expected_rows: &[SchemaRow],
) -> rusqlite::Result<Result<FingerprintFacts, ArchiveError>> {
    let limit = expected_rows
        .len()
        .saturating_add(STAT_TABLES.len())
        .saturating_add(EXTRA_ROWS_READ);
    let Some(actual) = read_rows(conn, limit)? else {
        return Ok(Err(ArchiveError::Damaged(DamageKind::SchemaMismatch)));
    };
    if actual.len() >= limit {
        return Ok(Err(ArchiveError::Damaged(DamageKind::SchemaMismatch)));
    }
    Ok(compare(expected_rows, &actual))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v1() -> Vec<SchemaRow> {
        expected(&SchemaPlan::BUILD, 1).expect("expected v1")
    }

    #[test]
    fn v1_matches_itself_and_has_the_fts_shadow_tables() {
        let rows = v1();
        assert!(rows.len() > 50, "{}", rows.len());
        for s in SHADOW_TABLES {
            assert!(rows.iter().any(|r| r.name == s), "{s}");
        }
        assert_eq!(
            compare(&rows, &rows).expect("matches"),
            FingerprintFacts::default()
        );
    }

    fn is_mismatch(r: Result<FingerprintFacts, ArchiveError>) -> bool {
        matches!(r, Err(ArchiveError::Damaged(DamageKind::SchemaMismatch)))
    }

    #[test]
    fn extra_missing_and_changed_objects_mismatch() {
        let rows = v1();
        let mut extra = rows.clone();
        extra.push(SchemaRow {
            kind: "trigger".into(),
            name: "planted".into(),
            tbl_name: "message".into(),
            sql: Some("CREATE TRIGGER planted AFTER INSERT ON message BEGIN SELECT 1; END".into()),
        });
        assert!(is_mismatch(compare(&rows, &extra)));

        let mut missing = rows.clone();
        missing.retain(|r| r.name != "message_sort");
        assert!(is_mismatch(compare(&rows, &missing)));

        let mut changed = rows.clone();
        if let Some(r) = changed.iter_mut().find(|r| r.name == "message_body_au") {
            r.sql = r.sql.as_ref().map(|s| s.replace("IS NOT", "IS"));
        }
        assert!(is_mismatch(compare(&rows, &changed)));

        let mut dup = rows.clone();
        if let Some(first) = rows.first() {
            dup.push(first.clone());
        }
        assert!(is_mismatch(compare(&rows, &dup)));
    }

    #[test]
    fn shadow_sql_differences_and_stat_tables_are_tolerated() {
        let rows = v1();
        let mut actual = rows.clone();
        for r in &mut actual {
            if SHADOW_TABLES.contains(&r.name.as_str()) {
                r.sql = Some("CREATE TABLE something_else(x)".into());
            }
        }
        actual.push(SchemaRow {
            kind: "table".into(),
            name: "sqlite_stat1".into(),
            tbl_name: "sqlite_stat1".into(),
            sql: Some("CREATE TABLE sqlite_stat1(tbl,idx,stat)".into()),
        });
        let facts = compare(&rows, &actual).expect("tolerated");
        assert!(facts.stat_tables);
        // A shadow table moved to another table is not tolerated.
        if let Some(r) = actual.iter_mut().find(|r| r.name == "message_fts_data") {
            r.tbl_name = "other".into();
        }
        assert!(is_mismatch(compare(&rows, &actual)));
    }

    #[test]
    fn a_version_without_a_recipe_is_a_mismatch() {
        assert!(matches!(
            expected(&SchemaPlan::BUILD, 2),
            Err(ArchiveError::Damaged(DamageKind::SchemaMismatch))
        ));
        assert!(matches!(
            expected(&SchemaPlan::BUILD, 0),
            Err(ArchiveError::Damaged(DamageKind::SchemaMismatch))
        ));
    }
}
