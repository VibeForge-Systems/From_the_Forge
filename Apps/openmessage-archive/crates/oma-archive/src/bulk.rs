//! The test-only bulk writer (M5 spec §14.2), behind the `fault-injection`
//! feature and reached through [`crate::hooks::bulk_writer`].
//!
//! It hands a test the archive's own ReadWrite connection (hardened as the
//! import's is: `DEFENSIVE`, foreign keys on) so a generator can write
//! schema-valid v1 rows directly, in import-sized transactions, and then
//! run the import's finalize statistics and FTS merge steps. It never runs
//! in a shipped build: the feature is enabled only by this crate's own
//! tests.
//!
//! The generator itself (`tests/common/bulk.rs`) decides the rows; this
//! module only provides the transaction boundary, the import's `sort_key`
//! encoder and the finalize steps, so a generated archive is laid out like
//! an imported one.

use rusqlite::Connection;

use crate::archive::{Archive, Recovery};
use crate::error::ArchiveError;

/// A write handle for generating rows directly. Borrowed from an
/// [`Archive`] that stays open (and locked) while it lives.
pub struct BulkWriter<'a> {
    conn: &'a Connection,
    batches: u64,
}

/// What [`BulkWriter::finalize`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BulkFinalize {
    /// FTS merge steps committed.
    pub fts_merge_steps: u32,
    /// The merge ran until the index reported no more work.
    pub fts_merge_complete: bool,
}

impl<'a> BulkWriter<'a> {
    pub(crate) fn new(archive: &'a mut Archive) -> Result<BulkWriter<'a>, ArchiveError> {
        archive.begin_write(Recovery::Run)?;
        let conn = archive.ensure_writer()?.conn();
        Ok(BulkWriter { conn, batches: 0 })
    }

    /// Run `f` in one `BEGIN IMMEDIATE` transaction, committed on success
    /// and rolled back on any error: one batch.
    pub fn batch<T>(
        &mut self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let out = f(self.conn).and_then(|v| self.conn.execute_batch("COMMIT").map(|()| v));
        if out.is_err() && !self.conn.is_autocommit() {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
        if out.is_ok() {
            self.batches = self.batches.saturating_add(1);
        }
        out
    }

    /// Batches committed so far.
    pub fn batches(&self) -> u64 {
        self.batches
    }

    /// The import's finalize steps: `chat_stats`, `archive_stats`, FTS merge
    /// steps until the index reports no more work, and a `TRUNCATE`
    /// checkpoint.
    pub fn finalize(&mut self, now_ns: i64) -> rusqlite::Result<BulkFinalize> {
        let (fts_merge_steps, fts_merge_complete) =
            crate::import::bulk_finalize(self.conn, now_ns)?;
        Ok(BulkFinalize {
            fts_merge_steps,
            fts_merge_complete,
        })
    }
}

/// The import's `message.sort_key` for a dated instant (`Some(unix ns)`) or
/// an undated row (its first rank and first source rowid).
pub fn sort_key(utc_ns: Option<i64>, first_rank: &[u8; 41], first_src_rowid: i64) -> Vec<u8> {
    crate::import::encode_sort_key(utc_ns, first_rank, first_src_rowid)
}
