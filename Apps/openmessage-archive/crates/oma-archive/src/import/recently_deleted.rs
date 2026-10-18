//! P8, the Recently Deleted markers (spec §8.4, §8.8, D60).
//!
//! One `message_recently_deleted` row per `chat_recoverable_message_join`
//! row whose message is mapped in **this** session: imported (the opt-in)
//! or already archived and re-observed (the default). A left-out message
//! has no row to mark, so its join rows are counted as not marked. The
//! markers are per session, keyed by the join row (`session_id`,
//! `src_rowid`), so a message deleted from two chats has two markers
//! (C1.12).
//!
//! The chat side is resolved when it can be (`chat_ref`), and the raw
//! source value is kept either way. The deleted text itself
//! (`recoverable_message_part.part_text`) is never read: the reader's
//! authorizer denies it. As for the link tables, a cell's `value_issue`
//! detail is not kept (the table has no single row id); its state is.

use std::ops::ControlFlow;

use oma_messages::{
    Cursor, KnownTable, MessagesCtx, MessagesError, PassOutcome, RecoverableLink, SmsDb,
};
use rusqlite::{Connection, Statement};

use super::batch::{record, Brk, IssueDeltas, PassWriter, Run};
use super::bind::Cell;
use super::counts::ImportCounts;
use super::links::LinkStmts;
use super::merge::Param;
use crate::progress::ImportPhase;
use crate::schema::codes::{Entity, Pass, TimeUnit};

/// P8.
pub(crate) struct RecentlyDeleted;

fn pair(c: &Cell) -> [Param<'_>; 2] {
    [
        c.value.as_ref().map_or(Param::Null, Param::Val),
        Param::opt_int(c.st_code()),
    ]
}

impl RecentlyDeleted {
    /// Whether the join row's message is mapped (and the marker present).
    fn write_one(
        run: &Run,
        r: &RecoverableLink,
        ls: &mut LinkStmts<'_>,
        insert: &mut Statement<'_>,
    ) -> rusqlite::Result<bool> {
        let Some(message) = ls.resolve(run.session, Entity::Message, &r.message_id)? else {
            return Ok(false);
        };
        let chat = ls.resolve(run.session, Entity::Chat, &r.chat_id)?;
        let chat_rowid = Cell::int(&r.chat_id);
        let delete = Cell::time(&r.delete_date);
        let ck = Cell::int(&r.ck_sync_state);
        let [chat_raw, chat_st] = pair(&chat_rowid);
        let [ck_raw, ck_st] = pair(&ck);
        let [del_raw, del_st] = pair(&delete);
        insert.execute(rusqlite::params![
            run.session,
            r.rowid,
            message,
            chat,
            chat_raw,
            chat_st,
            del_raw,
            del_st,
            Param::opt_int(delete.unit.map(TimeUnit::code)),
            Param::opt_int(delete.utc_ns),
            ck_raw,
            ck_st
        ])?;
        Ok(true)
    }
}

impl PassWriter for RecentlyDeleted {
    type Record = RecoverableLink;
    const PASS: Pass = Pass::RecentlyDeleted;
    const PHASE: ImportPhase = ImportPhase::ImportingRecentlyDeleted;
    const SOURCE: KnownTable = KnownTable::ChatRecoverableMessageJoin;

    fn rowid(r: &RecoverableLink) -> i64 {
        r.rowid
    }

    fn size(_: &RecoverableLink) -> u64 {
        48
    }

    fn read(
        &self,
        db: &SmsDb<'_>,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: &mut dyn FnMut(RecoverableLink) -> ControlFlow<Brk>,
    ) -> Result<PassOutcome<Brk>, MessagesError> {
        db.for_each_recoverable(from, ctx, f)
    }

    fn write(
        &self,
        tx: &Connection,
        run: &Run,
        records: &[RecoverableLink],
        counts: &mut ImportCounts,
        _issues: &mut IssueDeltas,
    ) -> rusqlite::Result<()> {
        let mut ls = LinkStmts::prepare(tx)?;
        let mut insert = tx.prepare(
            "INSERT INTO message_recently_deleted (session_id, src_rowid, message_id, chat_ref, \
                 chat_rowid, chat_rowid_st, delete_raw, delete_st, delete_unit, delete_utc_ns, \
                 ck_sync_state, ck_sync_state_st) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
             ON CONFLICT (session_id, src_rowid) DO NOTHING",
        )?;
        for r in records {
            let marked = record(tx, || Self::write_one(run, r, &mut ls, &mut insert))?;
            let rd = &mut counts.recently_deleted;
            if marked {
                rd.marked = rd.marked.saturating_add(1);
            } else {
                rd.not_marked = rd.not_marked.saturating_add(1);
            }
        }
        Ok(())
    }
}
