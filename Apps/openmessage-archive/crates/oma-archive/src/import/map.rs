//! Session-local bookkeeping written inside each batch (spec §6.2, §8.4):
//!
//! - `import_rowid_map`: source ROWID → archive id, so later passes rebuild
//!   joins from archive ids (a source ROWID is never a key outside its
//!   session);
//! - `import_occurrence`: the durable occurrence counter of each unkeyed
//!   fingerprint, incremented in the same transaction as the row, so a
//!   resume continues the count exactly where the last commit left it;
//! - `lineage_seen`: which lineages observed a chat, message or attachment,
//!   and at which highest rank (spec §8.7, "not in the latest import").
//!
//! All three are upserts: a source ROWID or fingerprint is never inserted
//! twice in one session, and the upsert keeps a replay harmless.

use rusqlite::{Connection, Statement};

use super::keys::Rank;
use crate::schema::codes::Entity;

pub(crate) struct MapStmts<'c> {
    map: Statement<'c>,
    occ_bump: Statement<'c>,
    occ_get: Statement<'c>,
    seen: Statement<'c>,
}

impl<'c> MapStmts<'c> {
    pub(crate) fn prepare(c: &'c Connection) -> rusqlite::Result<MapStmts<'c>> {
        Ok(MapStmts {
            map: c.prepare(
                "INSERT INTO import_rowid_map (session_id, src, src_rowid, archive_id) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT (session_id, src, src_rowid) DO UPDATE SET archive_id = excluded.archive_id",
            )?,
            occ_bump: c.prepare(
                "INSERT INTO import_occurrence (session_id, entity, fp, seen) VALUES (?1, ?2, ?3, 1) \
                 ON CONFLICT (session_id, entity, fp) DO UPDATE SET seen = seen + 1",
            )?,
            occ_get: c.prepare(
                "SELECT seen FROM import_occurrence WHERE session_id = ?1 AND entity = ?2 AND fp = ?3",
            )?,
            seen: c.prepare(
                "INSERT INTO lineage_seen (entity, row_id, lineage_id, last_rank, last_session_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT (entity, row_id, lineage_id) DO UPDATE \
                   SET last_rank = excluded.last_rank, last_session_id = excluded.last_session_id \
                   WHERE excluded.last_rank > lineage_seen.last_rank",
            )?,
        })
    }

    /// Record that source `rowid` of `src` is archive row `id`.
    pub(crate) fn map(
        &mut self,
        session: i64,
        src: Entity,
        rowid: i64,
        id: i64,
    ) -> rusqlite::Result<()> {
        self.map
            .execute(rusqlite::params![session, src.code(), rowid, id])?;
        Ok(())
    }

    /// The occurrence number (from 0) of the next row with fingerprint
    /// `fp`: how many earlier rows of this source had it.
    pub(crate) fn next_occurrence(
        &mut self,
        session: i64,
        entity: Entity,
        fp: &[u8; 32],
    ) -> rusqlite::Result<i64> {
        self.occ_bump
            .execute(rusqlite::params![session, entity.code(), fp.as_slice()])?;
        let seen: i64 = self.occ_get.query_row(
            rusqlite::params![session, entity.code(), fp.as_slice()],
            |r| r.get(0),
        )?;
        Ok(seen.saturating_sub(1))
    }

    /// Record that `lineage` observed row `id` at `rank`.
    pub(crate) fn seen(
        &mut self,
        entity: Entity,
        id: i64,
        lineage: i64,
        rank: &Rank,
        session: i64,
    ) -> rusqlite::Result<()> {
        self.seen.execute(rusqlite::params![
            entity.code(),
            id,
            lineage,
            rank.as_bytes().as_slice(),
            session
        ])?;
        Ok(())
    }
}
