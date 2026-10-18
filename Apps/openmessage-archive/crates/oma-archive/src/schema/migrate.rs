//! Forward-only migrations (spec §7.2, D87).
//!
//! A migration runs only through an explicit `Archive::migrate()`. Import,
//! listing and the reader never migrate. M4 ships no real migration: the
//! list below is empty, and the tests exercise the framework with a
//! test-only v1 → v2 step behind the `fault-injection` feature.
//!
//! Each step:
//!
//! 1. `PRAGMA foreign_keys = OFF`, outside any transaction;
//! 2. `BEGIN IMMEDIATE`;
//! 3. the step's DDL, then its data changes;
//! 4. `PRAGMA foreign_key_check` (any row fails the step);
//! 5. the fingerprint for the step's target version;
//! 6. the FTS rank-1 `integrity-check`;
//! 7. `PRAGMA user_version`, `archive_meta.schema_version` and a
//!    `schema_migration` row;
//! 8. `COMMIT`, then `PRAGMA foreign_keys = ON`.
//!
//! Any failure rolls back, and the caller drops the write connection, so the
//! next connection sets `foreign_keys` again.
//!
//! **The pre-migration copy is mandatory for every real migration** and is
//! not implemented yet (it ships with the first real migration, M5). A step
//! not marked `test_only` is therefore refused before it changes anything.

use rusqlite::{Transaction, TransactionBehavior};

use crate::conn::WriteConn;
use crate::error::{primary_code, ArchiveError, DamageKind, Stage};
use crate::schema::fingerprint;
use crate::schema::SchemaPlan;

/// One forward step.
pub(crate) struct Migration {
    pub(crate) from: u32,
    pub(crate) to: u32,
    /// The schema change. It is also replayed on an in-memory database to
    /// build the expected objects of version `to` (§7.3), so it must not
    /// depend on data.
    pub(crate) ddl: &'static str,
    /// Data changes, run after `ddl` inside the same transaction.
    pub(crate) data: fn(&Transaction<'_>) -> rusqlite::Result<()>,
    /// A test-only step runs without the pre-migration copy; every other
    /// step is refused until that copy exists.
    pub(crate) test_only: bool,
}

/// The real migrations of this build: none in M4.
pub(crate) static MIGRATIONS: &[Migration] = &[];

/// A data step that changes nothing.
#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn no_data(_tx: &Transaction<'_>) -> rusqlite::Result<()> {
    Ok(())
}

/// What `Archive::migrate` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MigrationReport {
    /// The version before.
    pub from: u32,
    /// The version after.
    pub to: u32,
    /// Steps applied.
    pub steps: u32,
    /// Unfinished import sessions marked superseded before the first step.
    pub sessions_superseded: u32,
    /// `false` when the free space could not be read, so the 3× check was
    /// skipped (an error never counts as zero space).
    pub space_checked: bool,
}

/// Run one step on `w`. `now_ns` and `app_version` fill the
/// `schema_migration` row.
pub(crate) fn run_step(
    w: &mut WriteConn,
    plan: &SchemaPlan,
    step: &Migration,
    now_ns: i64,
    app_version: &str,
) -> Result<(), ArchiveError> {
    if !step.test_only {
        return Err(ArchiveError::Internal(
            "a real migration needs the pre-migration copy, which is not implemented",
        ));
    }
    let expected_to = fingerprint::expected(plan, step.to)?;
    let sql = |e: rusqlite::Error| ArchiveError::sqlite(Stage::Migrate, &e);
    w.conn()
        .pragma_update(None, "foreign_keys", false)
        .map_err(sql)?;
    let tx = Transaction::new(w.conn_mut(), TransactionBehavior::Immediate).map_err(sql)?;
    tx.execute_batch(step.ddl).map_err(sql)?;
    (step.data)(&tx).map_err(sql)?;
    crate::fault::point(crate::fault::FaultPoint::MigrationStep);

    let fk_violation = {
        let mut stmt = tx.prepare("PRAGMA foreign_key_check").map_err(sql)?;
        let mut rows = stmt.query([]).map_err(sql)?;
        rows.next().map_err(sql)?.is_some()
    };
    if fk_violation {
        return Err(ArchiveError::Damaged(DamageKind::MigrationFailed));
    }
    match fingerprint::check(&tx, &expected_to) {
        Ok(_) => {}
        Err(ArchiveError::Damaged(DamageKind::SchemaMismatch)) => {
            return Err(ArchiveError::Damaged(DamageKind::MigrationFailed))
        }
        Err(e) => return Err(e),
    }
    if let Err(e) = tx.execute(
        "INSERT INTO message_fts(message_fts, rank) VALUES('integrity-check', 1)",
        [],
    ) {
        // SQLITE_CORRUPT (11) is the integrity check's verdict; anything
        // else is an error running it.
        return Err(if primary_code(&e) == Some(11) {
            ArchiveError::Damaged(DamageKind::MigrationFailed)
        } else {
            sql(e)
        });
    }
    tx.pragma_update(None, "user_version", step.to)
        .map_err(sql)?;
    let updated = tx
        .execute(
            "UPDATE archive_meta SET schema_version = ?1 WHERE id = 1",
            [step.to],
        )
        .map_err(sql)?;
    if updated != 1 {
        return Err(ArchiveError::Damaged(DamageKind::MetaMissing));
    }
    tx.execute(
        "INSERT INTO schema_migration (version, from_version, applied_at_ns, app_version) \
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![step.to, step.from, now_ns, app_version],
    )
    .map_err(sql)?;
    crate::fault::point(crate::fault::FaultPoint::MigrationBeforeCommit);
    tx.commit().map_err(sql)?;
    w.conn()
        .pragma_update(None, "foreign_keys", true)
        .map_err(sql)?;
    Ok(())
}
