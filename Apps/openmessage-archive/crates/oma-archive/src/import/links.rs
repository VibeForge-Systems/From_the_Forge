//! P4, P5 and P7: the link passes (spec §8.4, §8.7 "Joins", §6.5).
//!
//! Each source join row names two source rowids. Both are resolved through
//! this session's `import_rowid_map` (a source rowid is never a key outside
//! its session, §6.2), so a link is always between two committed archive
//! rows. A row with a side that does not resolve (unreadable, NULL, or a
//! rowid with no imported row) is **counted, never inserted**.
//!
//! Links are a union: a link once observed is never removed. Per link:
//!
//! - `last_rank` is the highest rank that observed it, and
//!   `last_session_id` the session with that rank;
//! - `first_session_id` is the lowest-ranked session that observed it (an
//!   equal rank keeps the stored one), so the result does not depend on the
//!   order of imports;
//! - `chat_message.join_date` (the source's `message_date`) follows the raw
//!   rule of §8.7 against the link's `last_rank`; its `sort_key` is the
//!   message's, kept in step by the `message_sort_au` trigger;
//! - `message_attachment.ord_rank`/`ord_rowid` are replaced when the rank is
//!   at least `ord_rank`: the order of the highest-ranked backup that saw
//!   the link (§6.5).
//!
//! The Recently Deleted policy ([`RdSkip`]): a join of a message left out or
//! only re-observed, and (P7) of an attachment linked only to left-out
//! messages, is skipped and counted as such, not as unresolved (§8.4, §8.8).
//!
//! A join cell's `value_issue` detail is not kept: these tables have no
//! single row id. Its state (`*_st`) is.

use std::collections::HashMap;
use std::ops::ControlFlow;

use oma_messages::{
    ChatHandleLink, ChatMessageLink, Col, Cursor, KnownTable, MessageAttachmentLink, MessagesCtx,
    MessagesError, PassOutcome, SmsDb,
};
use rusqlite::{Connection, OptionalExtension, Statement};

use super::batch::{record, Brk, IssueDeltas, PassWriter, Run};
use super::bind::{Cell, Val};
use super::counts::{ImportCounts, LinkCounts};
use super::keys::Rank;
use super::merge::{decide, Decision};
use super::rd::RdSkip;
use crate::progress::ImportPhase;
use crate::schema::codes::{Entity, IssueCode, Pass, TimeUnit, ValueState};

fn bad(what: &'static str) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(what.into())
}

/// What one join row did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkOutcome {
    Inserted,
    Present,
    Unresolved { first: bool, second: bool },
    SkippedRd,
}

fn tally(c: &mut LinkCounts, o: LinkOutcome) {
    c.seen = c.seen.saturating_add(1);
    match o {
        LinkOutcome::Inserted => c.inserted = c.inserted.saturating_add(1),
        LinkOutcome::Present => c.already_present = c.already_present.saturating_add(1),
        LinkOutcome::Unresolved { first, second } => {
            c.unresolved = c.unresolved.saturating_add(1);
            if first {
                c.unresolved_first = c.unresolved_first.saturating_add(1);
            }
            if second {
                c.unresolved_second = c.unresolved_second.saturating_add(1);
            }
        }
        LinkOutcome::SkippedRd => {
            c.skipped_recently_deleted = c.skipped_recently_deleted.saturating_add(1);
        }
    }
}

/// The statements every link pass shares, prepared once per batch.
pub(crate) struct LinkStmts<'c> {
    resolve: Statement<'c>,
    session_rank: Statement<'c>,
    ranks: HashMap<i64, Option<Rank>>,
}

impl<'c> LinkStmts<'c> {
    pub(crate) fn prepare(c: &'c Connection) -> rusqlite::Result<LinkStmts<'c>> {
        Ok(LinkStmts {
            resolve: c.prepare(
                "SELECT archive_id FROM import_rowid_map \
                 WHERE session_id = ?1 AND src = ?2 AND src_rowid = ?3",
            )?,
            session_rank: c.prepare("SELECT rank FROM import_session WHERE id = ?1")?,
            ranks: HashMap::new(),
        })
    }

    /// The archive row a source rowid of `src` names in this session.
    pub(crate) fn resolve(
        &mut self,
        session: i64,
        src: Entity,
        col: &Col<i64>,
    ) -> rusqlite::Result<Option<i64>> {
        match col {
            Col::Value(rowid) => self
                .resolve
                .query_row(rusqlite::params![session, src.code(), rowid], |r| r.get(0))
                .optional(),
            _ => Ok(None),
        }
    }

    /// A stored session's rank (cached for the batch); `None` when it
    /// cannot be read.
    fn rank_of(&mut self, session: i64) -> rusqlite::Result<Option<Rank>> {
        if let Some(r) = self.ranks.get(&session) {
            return Ok(*r);
        }
        let b: Option<Vec<u8>> = self
            .session_rank
            .query_row([session], |r| r.get(0))
            .optional()?;
        let r = b.as_deref().and_then(Rank::from_slice);
        self.ranks.insert(session, r);
        Ok(r)
    }

    /// The bookkeeping of a stored link after this observation.
    fn bookkeeping(&mut self, run: &Run, stored: &Book) -> rusqlite::Result<Book> {
        let mut out = *stored;
        if run.rank > stored.last_rank {
            out.last_rank = run.rank;
            out.last_session = run.session;
        }
        if stored.first_session != run.session {
            if let Some(first) = self.rank_of(stored.first_session)? {
                if run.rank < first {
                    out.first_session = run.session;
                }
            }
        }
        Ok(out)
    }
}

/// A link's bookkeeping columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Book {
    first_session: i64,
    last_session: i64,
    last_rank: Rank,
}

impl Book {
    fn new(run: &Run) -> Book {
        Book {
            first_session: run.session,
            last_session: run.session,
            last_rank: run.rank,
        }
    }

    /// Read from `first_session_id, last_session_id, last_rank` at `i`.
    fn read(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<Book> {
        Ok(Book {
            first_session: r.get(i)?,
            last_session: r.get(i.saturating_add(1))?,
            last_rank: rank_col(r, i.saturating_add(2))?,
        })
    }
}

fn changed_one(n: usize) -> rusqlite::Result<()> {
    if n == 1 {
        Ok(())
    } else {
        Err(bad("link update"))
    }
}

fn rank_col(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<Rank> {
    let b: Vec<u8> = r.get(i)?;
    Rank::from_slice(&b).ok_or_else(|| bad("stored rank"))
}

/// Resolve both sides; the unresolved outcome when either fails.
fn both(first: Option<i64>, second: Option<i64>) -> Result<(i64, i64), LinkOutcome> {
    match (first, second) {
        (Some(a), Some(b)) => Ok((a, b)),
        (a, b) => Err(LinkOutcome::Unresolved {
            first: a.is_none(),
            second: b.is_none(),
        }),
    }
}

fn is_value_in(col: &Col<i64>, f: impl Fn(i64) -> bool) -> bool {
    matches!(col, Col::Value(v) if f(*v))
}

// ---------------------------------------------------------------------------
// P4: chat <-> handle
// ---------------------------------------------------------------------------

/// P4.
pub(crate) struct ChatHandles;

struct ChatHandleStmts<'c> {
    find: Statement<'c>,
    insert: Statement<'c>,
    update: Statement<'c>,
}

impl ChatHandles {
    fn write_one(
        run: &Run,
        r: &ChatHandleLink,
        ls: &mut LinkStmts<'_>,
        st: &mut ChatHandleStmts<'_>,
    ) -> rusqlite::Result<LinkOutcome> {
        let chat = ls.resolve(run.session, Entity::Chat, &r.chat_id)?;
        let handle = ls.resolve(run.session, Entity::Handle, &r.handle_id)?;
        let (chat, handle) = match both(chat, handle) {
            Ok(p) => p,
            Err(o) => return Ok(o),
        };
        let stored: Option<Book> = st
            .find
            .query_row([chat, handle], |row| Book::read(row, 0))
            .optional()?;
        let Some(stored) = stored else {
            let b = Book::new(run);
            st.insert.execute(rusqlite::params![
                chat,
                handle,
                b.first_session,
                b.last_session,
                b.last_rank.as_bytes().as_slice()
            ])?;
            return Ok(LinkOutcome::Inserted);
        };
        let b = ls.bookkeeping(run, &stored)?;
        if b != stored {
            changed_one(st.update.execute(rusqlite::params![
                chat,
                handle,
                b.first_session,
                b.last_session,
                b.last_rank.as_bytes().as_slice()
            ])?)?;
        }
        Ok(LinkOutcome::Present)
    }
}

impl PassWriter for ChatHandles {
    type Record = ChatHandleLink;
    const PASS: Pass = Pass::ChatHandles;
    const PHASE: ImportPhase = ImportPhase::ImportingChatLinks;
    const SOURCE: KnownTable = KnownTable::ChatHandleJoin;

    fn rowid(r: &ChatHandleLink) -> i64 {
        r.rowid
    }

    fn size(_: &ChatHandleLink) -> u64 {
        32
    }

    fn read(
        &self,
        db: &SmsDb<'_>,
        _conn: &Connection,
        _run: &Run,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: &mut dyn FnMut(ChatHandleLink) -> ControlFlow<Brk>,
    ) -> Result<PassOutcome<Brk>, MessagesError> {
        db.for_each_chat_handle(from, ctx, f)
    }

    fn write(
        &self,
        tx: &Connection,
        run: &Run,
        records: &[ChatHandleLink],
        counts: &mut ImportCounts,
        _issues: &mut IssueDeltas,
    ) -> rusqlite::Result<()> {
        let mut ls = LinkStmts::prepare(tx)?;
        let mut st = ChatHandleStmts {
            find: tx.prepare(
                "SELECT first_session_id, last_session_id, last_rank FROM chat_handle \
                 WHERE chat_id = ?1 AND handle_id = ?2",
            )?,
            insert: tx.prepare(
                "INSERT INTO chat_handle (chat_id, handle_id, first_session_id, \
                     last_session_id, last_rank) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?,
            update: tx.prepare(
                "UPDATE chat_handle SET first_session_id = ?3, last_session_id = ?4, \
                     last_rank = ?5 WHERE chat_id = ?1 AND handle_id = ?2",
            )?,
        };
        for r in records {
            let o = record(tx, || Self::write_one(run, r, &mut ls, &mut st))?;
            tally(&mut counts.chat_handles, o);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// P5: chat <-> message
// ---------------------------------------------------------------------------

/// P5.
pub(crate) struct ChatMessages<'a> {
    rd: &'a RdSkip,
}

struct ChatMessageStmts<'c> {
    find: Statement<'c>,
    sort_key: Statement<'c>,
    insert: Statement<'c>,
    update: Statement<'c>,
}

/// The stored join date of a link.
struct StoredDate {
    raw: Option<i64>,
    st: Option<i64>,
    unit: Option<i64>,
    utc_ns: Option<i64>,
}

impl<'a> ChatMessages<'a> {
    pub(crate) fn new(rd: &'a RdSkip) -> ChatMessages<'a> {
        ChatMessages { rd }
    }

    fn write_one(
        &self,
        run: &Run,
        r: &ChatMessageLink,
        ls: &mut LinkStmts<'_>,
        st: &mut ChatMessageStmts<'_>,
        issues: &mut IssueDeltas,
    ) -> rusqlite::Result<LinkOutcome> {
        if is_value_in(&r.message_id, |m| self.rd.skips_message(m)) {
            return Ok(LinkOutcome::SkippedRd);
        }
        let chat = ls.resolve(run.session, Entity::Chat, &r.chat_id)?;
        let message = ls.resolve(run.session, Entity::Message, &r.message_id)?;
        let (chat, message) = match both(chat, message) {
            Ok(p) => p,
            Err(o) => return Ok(o),
        };
        // The join date as a cell: its state, unit and instant (no issue
        // detail is kept for a join cell).
        let mut observed = Cell::time(&r.message_date);
        observed.issues.clear();
        let observed_date = StoredDate {
            raw: match observed.value {
                Some(Val::Int(i)) => Some(i),
                _ => None,
            },
            st: observed.st_code(),
            unit: observed.unit.map(TimeUnit::code),
            utc_ns: observed.utc_ns,
        };
        let stored: Option<(Book, StoredDate)> = st
            .find
            .query_row([chat, message], |row| {
                Ok((
                    Book::read(row, 0)?,
                    StoredDate {
                        raw: row.get(3)?,
                        st: row.get(4)?,
                        unit: row.get(5)?,
                        utc_ns: row.get(6)?,
                    },
                ))
            })
            .optional()?;
        let Some((book, date)) = stored else {
            let sort_key: Vec<u8> = st.sort_key.query_row([message], |row| row.get(0))?;
            let b = Book::new(run);
            st.insert.execute(rusqlite::params![
                chat,
                message,
                b.first_session,
                b.last_session,
                b.last_rank.as_bytes().as_slice(),
                observed_date.raw,
                observed_date.st,
                observed_date.unit,
                observed_date.utc_ns,
                sort_key
            ])?;
            return Ok(LinkOutcome::Inserted);
        };
        let state = match date.st {
            None => None,
            Some(c) => Some(ValueState::from_code(c).ok_or_else(|| bad("stored state"))?),
        };
        let stored_cell = Cell {
            value: date.raw.map(Val::Int),
            st: state,
            unit: None,
            utc_ns: None,
            issues: Vec::new(),
        };
        let mut new_date = date;
        let mut date_changed = false;
        match decide(&stored_cell, &observed, run.rank, book.last_rank) {
            Decision::Replace { changed: true } => {
                new_date = observed_date;
                date_changed = true;
            }
            Decision::Keep { not_applied: true } => {
                issues.add(IssueCode::ObservationNotApplied, 1);
            }
            _ => {}
        }
        let b = ls.bookkeeping(run, &book)?;
        if b != book || date_changed {
            changed_one(st.update.execute(rusqlite::params![
                chat,
                message,
                b.first_session,
                b.last_session,
                b.last_rank.as_bytes().as_slice(),
                new_date.raw,
                new_date.st,
                new_date.unit,
                new_date.utc_ns
            ])?)?;
        }
        Ok(LinkOutcome::Present)
    }
}

impl PassWriter for ChatMessages<'_> {
    type Record = ChatMessageLink;
    const PASS: Pass = Pass::ChatMessages;
    const PHASE: ImportPhase = ImportPhase::ImportingChatLinks;
    const SOURCE: KnownTable = KnownTable::ChatMessageJoin;

    fn rowid(r: &ChatMessageLink) -> i64 {
        r.rowid
    }

    fn size(_: &ChatMessageLink) -> u64 {
        40
    }

    fn read(
        &self,
        db: &SmsDb<'_>,
        _conn: &Connection,
        _run: &Run,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: &mut dyn FnMut(ChatMessageLink) -> ControlFlow<Brk>,
    ) -> Result<PassOutcome<Brk>, MessagesError> {
        db.for_each_chat_message(from, ctx, f)
    }

    fn write(
        &self,
        tx: &Connection,
        run: &Run,
        records: &[ChatMessageLink],
        counts: &mut ImportCounts,
        issues: &mut IssueDeltas,
    ) -> rusqlite::Result<()> {
        let mut ls = LinkStmts::prepare(tx)?;
        let mut st = ChatMessageStmts {
            find: tx.prepare(
                "SELECT first_session_id, last_session_id, last_rank, join_date_raw, \
                        join_date_st, join_date_unit, join_date_utc_ns \
                 FROM chat_message WHERE chat_id = ?1 AND message_id = ?2",
            )?,
            sort_key: tx.prepare("SELECT sort_key FROM message WHERE id = ?1")?,
            insert: tx.prepare(
                "INSERT INTO chat_message (chat_id, message_id, first_session_id, \
                     last_session_id, last_rank, join_date_raw, join_date_st, join_date_unit, \
                     join_date_utc_ns, sort_key) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?,
            update: tx.prepare(
                "UPDATE chat_message SET first_session_id = ?3, last_session_id = ?4, \
                     last_rank = ?5, join_date_raw = ?6, join_date_st = ?7, \
                     join_date_unit = ?8, join_date_utc_ns = ?9 \
                 WHERE chat_id = ?1 AND message_id = ?2",
            )?,
        };
        for r in records {
            let o = record(tx, || self.write_one(run, r, &mut ls, &mut st, issues))?;
            tally(&mut counts.chat_messages, o);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// P7: message <-> attachment
// ---------------------------------------------------------------------------

/// P7.
pub(crate) struct MessageAttachments<'a> {
    rd: &'a RdSkip,
}

struct MessageAttachmentStmts<'c> {
    find: Statement<'c>,
    insert: Statement<'c>,
    update: Statement<'c>,
}

impl<'a> MessageAttachments<'a> {
    pub(crate) fn new(rd: &'a RdSkip) -> MessageAttachments<'a> {
        MessageAttachments { rd }
    }

    fn write_one(
        &self,
        run: &Run,
        r: &MessageAttachmentLink,
        ls: &mut LinkStmts<'_>,
        st: &mut MessageAttachmentStmts<'_>,
    ) -> rusqlite::Result<LinkOutcome> {
        if is_value_in(&r.message_id, |m| self.rd.skips_message(m))
            || is_value_in(&r.attachment_id, |a| self.rd.skips_attachment(a))
        {
            return Ok(LinkOutcome::SkippedRd);
        }
        let message = ls.resolve(run.session, Entity::Message, &r.message_id)?;
        let attachment = ls.resolve(run.session, Entity::Attachment, &r.attachment_id)?;
        let (message, attachment) = match both(message, attachment) {
            Ok(p) => p,
            Err(o) => return Ok(o),
        };
        let stored: Option<(Book, Rank, i64)> = st
            .find
            .query_row([message, attachment], |row| {
                Ok((Book::read(row, 0)?, rank_col(row, 3)?, row.get(4)?))
            })
            .optional()?;
        let Some((book, ord_rank, ord_rowid)) = stored else {
            let b = Book::new(run);
            st.insert.execute(rusqlite::params![
                message,
                attachment,
                b.first_session,
                b.last_session,
                b.last_rank.as_bytes().as_slice(),
                run.rank.as_bytes().as_slice(),
                r.rowid
            ])?;
            return Ok(LinkOutcome::Inserted);
        };
        let (new_rank, new_rowid) = if run.rank >= ord_rank {
            (run.rank, r.rowid)
        } else {
            (ord_rank, ord_rowid)
        };
        let b = ls.bookkeeping(run, &book)?;
        if b != book || new_rank != ord_rank || new_rowid != ord_rowid {
            changed_one(st.update.execute(rusqlite::params![
                message,
                attachment,
                b.first_session,
                b.last_session,
                b.last_rank.as_bytes().as_slice(),
                new_rank.as_bytes().as_slice(),
                new_rowid
            ])?)?;
        }
        Ok(LinkOutcome::Present)
    }
}

impl PassWriter for MessageAttachments<'_> {
    type Record = MessageAttachmentLink;
    const PASS: Pass = Pass::MessageAttachments;
    const PHASE: ImportPhase = ImportPhase::ImportingAttachmentLinks;
    const SOURCE: KnownTable = KnownTable::MessageAttachmentJoin;

    fn rowid(r: &MessageAttachmentLink) -> i64 {
        r.rowid
    }

    fn size(_: &MessageAttachmentLink) -> u64 {
        32
    }

    fn read(
        &self,
        db: &SmsDb<'_>,
        _conn: &Connection,
        _run: &Run,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: &mut dyn FnMut(MessageAttachmentLink) -> ControlFlow<Brk>,
    ) -> Result<PassOutcome<Brk>, MessagesError> {
        db.for_each_message_attachment(from, ctx, f)
    }

    fn write(
        &self,
        tx: &Connection,
        run: &Run,
        records: &[MessageAttachmentLink],
        counts: &mut ImportCounts,
        _issues: &mut IssueDeltas,
    ) -> rusqlite::Result<()> {
        let mut ls = LinkStmts::prepare(tx)?;
        let mut st = MessageAttachmentStmts {
            find: tx.prepare(
                "SELECT first_session_id, last_session_id, last_rank, ord_rank, ord_rowid \
                 FROM message_attachment WHERE message_id = ?1 AND attachment_id = ?2",
            )?,
            insert: tx.prepare(
                "INSERT INTO message_attachment (message_id, attachment_id, first_session_id, \
                     last_session_id, last_rank, ord_rank, ord_rowid) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?,
            update: tx.prepare(
                "UPDATE message_attachment SET first_session_id = ?3, last_session_id = ?4, \
                     last_rank = ?5, ord_rank = ?6, ord_rowid = ?7 \
                 WHERE message_id = ?1 AND attachment_id = ?2",
            )?,
        };
        for r in records {
            let o = record(tx, || self.write_one(run, r, &mut ls, &mut st))?;
            tally(&mut counts.message_attachments, o);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_outcome_is_counted_once() {
        let mut c = LinkCounts::default();
        for o in [
            LinkOutcome::Inserted,
            LinkOutcome::Present,
            LinkOutcome::Unresolved {
                first: true,
                second: true,
            },
            LinkOutcome::Unresolved {
                first: false,
                second: true,
            },
            LinkOutcome::SkippedRd,
        ] {
            tally(&mut c, o);
        }
        assert_eq!(c.seen, 5);
        assert_eq!(
            c.inserted + c.already_present + c.unresolved + c.skipped_recently_deleted,
            c.seen
        );
        assert_eq!((c.unresolved_first, c.unresolved_second), (1, 2));
        assert_eq!(
            both(Some(1), None),
            Err(LinkOutcome::Unresolved {
                first: false,
                second: true
            })
        );
        assert_eq!(both(Some(1), Some(2)), Ok((1, 2)));
    }
}
