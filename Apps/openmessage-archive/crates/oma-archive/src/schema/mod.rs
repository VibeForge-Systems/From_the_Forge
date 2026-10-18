//! Schema v1 of `archive.sqlite` (spec §6, §7).
//!
//! - `PRAGMA application_id` is [`APPLICATION_ID`] ("OMA1", unregistered;
//!   D75), at header offset 68.
//! - `PRAGMA user_version` holds the schema version, mirrored in
//!   `archive_meta.schema_version` because `sqlcipher_export()` drops
//!   `user_version`.
//! - `PRAGMA schema_version` is never written.
//! - `v1.sql` freezes at the M4 release tag; after that, every change is a
//!   migration ([`migrate`]).

pub mod codes;
pub(crate) mod fingerprint;
pub(crate) mod migrate;

use rusqlite::Connection;

use crate::error::{ArchiveError, Stage};
use migrate::Migration;

/// The schema version this build writes and reads.
pub const SCHEMA_VERSION: u32 = 1;

/// `PRAGMA application_id`: the ASCII bytes "OMA1".
pub const APPLICATION_ID: i32 = 0x4F4D_4131;

/// The derivation-rule version of this build. An archive whose
/// `max_deriver_version` is higher opens for reading and verification only
/// (`ImportBlock::NewerRules`, §5.3 step 7).
pub const DERIVER_VERSION: u32 = 1;

/// The typedstream decoder version of this build (as [`DERIVER_VERSION`]).
pub const DECODER_VERSION: u32 = 1;

/// `archive_meta.format`.
pub(crate) const FORMAT: &str = "openmessage-archive";

/// `archive_meta.fts_tokenizer`: a recorded copy of the tokenizer arguments
/// in `v1.sql`.
pub(crate) const FTS_TOKENIZER: &str = "unicode61 remove_diacritics 2";

/// The schema v1 DDL (spec §6.4).
pub(crate) const V1_SQL: &str = include_str!("v1.sql");

/// The schema a build reads and writes: its version and the migrations
/// that lead to it from v1. A shipped build always uses
/// [`SchemaPlan::BUILD`]; the `fault-injection` feature lets tests swap in
/// a plan with a test-only v2.
#[derive(Clone, Copy)]
pub(crate) struct SchemaPlan {
    pub(crate) version: u32,
    pub(crate) migrations: &'static [Migration],
}

impl SchemaPlan {
    pub(crate) const BUILD: SchemaPlan = SchemaPlan {
        version: SCHEMA_VERSION,
        migrations: migrate::MIGRATIONS,
    };

    /// The plan in force: [`SchemaPlan::BUILD`], unless a test installed
    /// another one.
    pub(crate) fn current() -> SchemaPlan {
        crate::fault::schema_plan().unwrap_or(SchemaPlan::BUILD)
    }

    /// The consecutive steps from `from` to `to`, or `None` if the plan
    /// has no such chain.
    pub(crate) fn chain(&self, from: u32, to: u32) -> Option<Vec<&'static Migration>> {
        let mut steps = Vec::new();
        let mut at = from;
        while at < to {
            let step = self.migrations.iter().find(|m| m.from == at)?;
            if step.to <= at || step.to > to {
                return None;
            }
            steps.push(step);
            at = step.to;
        }
        (at == to).then_some(steps)
    }
}

/// Apply `v1.sql` and then each migration's DDL up to `version`, on a
/// connection inside the caller's transaction (creation) or on a scratch
/// in-memory database (the expected side of the fingerprint). Migrations'
/// data steps never run here: a new archive has no rows to transform.
pub(crate) fn build_schema(
    conn: &Connection,
    plan: &SchemaPlan,
    version: u32,
    stage: Stage,
) -> Result<(), ArchiveError> {
    if version < 1 {
        return Err(ArchiveError::Internal("no schema below version 1"));
    }
    let chain = plan
        .chain(1, version)
        .ok_or(ArchiveError::Internal("no migration chain to that version"))?;
    conn.execute_batch(V1_SQL)
        .map_err(|e| ArchiveError::sqlite(stage, &e))?;
    for step in chain {
        conn.execute_batch(step.ddl)
            .map_err(|e| ArchiveError::sqlite(stage, &e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_application_id_spells_oma1() {
        assert_eq!(APPLICATION_ID.to_be_bytes(), *b"OMA1");
    }

    #[test]
    fn v1_sql_has_lf_line_endings() {
        // The text is stored verbatim in sqlite_schema; CRLF on one
        // platform would make its archives mismatch on the other.
        assert!(!V1_SQL.contains('\r'));
    }

    #[test]
    fn the_build_plan_has_no_migrations_and_chains_to_itself() {
        let plan = SchemaPlan::BUILD;
        assert_eq!(plan.version, 1);
        assert!(plan.migrations.is_empty());
        assert_eq!(plan.chain(1, 1).map(|c| c.len()), Some(0));
        assert!(plan.chain(1, 2).is_none());
    }
}
