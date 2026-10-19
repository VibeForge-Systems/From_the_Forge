//! Words mode (M5 spec §4.3): FTS5 `MATCH` on an expression Rust built.
//!
//! - **Global plan:** FTS5 over the whole archive, newest body rowid
//!   first, each match joined to its body and message by primary key and
//!   filtered in SQL, read until [`super::SEARCH_MATCH_CAP`] + 1 distinct
//!   **messages** (the cap counts messages, as the exact scan does, never
//!   body rows). Rowid order is import order, not date, so a capped global
//!   search is not the newest matches, and says so. When the cut leaves an
//!   admitted message without its current body, that body is probed, so a
//!   hit is never labelled "earlier" when its current text matches too.
//! - **Scoped plan** (a conversation filter): with the global plan only,
//!   a common word in a small chat walks every archive-wide hit. So a
//!   chat of at most [`SCOPED_CHAT_ROWS`] timeline rows (counted through
//!   the covering index, capped one above) is walked itself, newest first,
//!   in keyset batches of [`SCOPED_BATCH`] messages; each kept message's
//!   bodies are checked with a rowid-constrained `MATCH`. **A query with a
//!   prefix term always uses the global plan:** FTS5 builds a prefix's
//!   merged doclist on every `MATCH` it starts, so one rowid-constrained
//!   call per body would repeat that work per message (measured in slice
//!   5c; see `docs/architecture.md`).
//! - Both run in one `ViewBudget::Search` phase with the cancel token and
//!   a 5 s deadline. Any SQLite error but an interrupt is
//!   `SearchUnavailable`: the index cannot be read, browsing still works.

use std::collections::HashMap;
use std::time::Instant;

use rusqlite::{Connection, OptionalExtension};

use super::{
    fts_err, KeyRange, Match, RowFilter, RowStmts, SearchRequest, WordsPlan, SEARCH_MATCH_CAP,
};
use crate::conn::{Phase, ReadConn, ViewBudget};
use crate::error::ArchiveError;
use crate::progress::CancelToken;

/// The largest chat (timeline rows) the scoped plan walks itself. The
/// spec's draft said 50,000; slice 5c measured the scoped walk at about
/// 30–80 µs per message (one rowid-constrained `MATCH` each) against about
/// 0.2 s for the global plan with the chat filter on a word in a fifth of
/// 500,000 messages, so the plans cross over at a few thousand rows
/// (synthetic, one host; `docs/architecture.md`).
pub const SCOPED_CHAT_ROWS: u64 = 5_000;
/// Messages per scoped keyset batch.
pub const SCOPED_BATCH: usize = 500;

pub(crate) const SQL_GLOBAL: &str = "SELECT b.id, m.id, m.sort_key, m.current_body_id \
     FROM message_fts f JOIN message_body b ON b.id = f.rowid \
     JOIN message m ON m.id = b.message_id \
     WHERE f.message_fts MATCH ?1 \
       AND (?2 OR m.current_body_id = b.id) \
       AND (?3 OR (m.assoc_present = 0 AND m.item_kind = 1)) \
       AND m.sort_key >= ?4 AND m.sort_key < ?5 \
       AND (?6 = 0 OR EXISTS (SELECT 1 FROM message_attachment ma WHERE ma.message_id = m.id)) \
       AND (?7 IS NULL OR m.is_from_me = ?7) \
       AND (?8 IS NULL OR EXISTS (SELECT 1 FROM chat_message cm \
             WHERE cm.chat_id = ?8 AND cm.message_id = m.id)) \
     ORDER BY f.rowid DESC LIMIT ?9";
pub(crate) const SQL_CHAT_ROWS: &str =
    "SELECT count(*) FROM (SELECT 1 FROM chat_message WHERE chat_id = ?1 LIMIT ?2)";
pub(crate) const SQL_CHAT_BATCH: &str = "SELECT message_id, sort_key FROM chat_message \
     WHERE chat_id = ?1 AND (sort_key, message_id) < (?2, ?3) AND sort_key >= ?4 \
     ORDER BY sort_key DESC, message_id DESC LIMIT ?5";
pub(crate) const SQL_ROW: &str =
    "SELECT current_body_id, assoc_present, item_kind, is_from_me FROM message WHERE id = ?1";
pub(crate) const SQL_BODY_IDS: &str = "SELECT id FROM message_body WHERE message_id = ?1";
pub(crate) const SQL_PROBE: &str =
    "SELECT 1 FROM message_fts WHERE message_fts MATCH ?1 AND rowid = ?2";

#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    [
        ("search words global", SQL_GLOBAL),
        ("search chat rows", SQL_CHAT_ROWS),
        ("search chat batch", SQL_CHAT_BATCH),
        ("search message filter row", SQL_ROW),
        ("search body ids", SQL_BODY_IDS),
        ("search words probe", SQL_PROBE),
    ]
    .into_iter()
    .map(|(n, s)| (n, s.to_owned()))
    .collect()
}

fn limit(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// One keyset batch of chat `chat`'s timeline, newest first, below
/// `from` and at or above `lo`. A walk starts at `(hi, i64::MIN)`, so
/// the row-value bound alone keeps it below the range's end (and the
/// planner seeks on it).
pub(crate) fn chat_batch(
    c: &Connection,
    chat: i64,
    from: (&[u8], i64),
    lo: &[u8],
    n: usize,
) -> rusqlite::Result<Vec<(i64, Vec<u8>)>> {
    let mut st = c.prepare(SQL_CHAT_BATCH)?;
    let rows = st.query_map(rusqlite::params![chat, from.0, from.1, lo, limit(n)], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })?;
    rows.collect()
}

/// The global plan: the matches of at most [`SEARCH_MATCH_CAP`] distinct
/// messages, and whether there were more.
fn global(
    p: &Phase<'_>,
    q: &SearchRequest,
    expr: &str,
    range: (&[u8], &[u8]),
) -> Result<(Vec<Match>, bool), ArchiveError> {
    let cap = crate::fault::search_match_cap(SEARCH_MATCH_CAP);
    let who: Option<i64> = match q.filters.sender {
        super::SenderFilter::Anyone => None,
        super::SenderFilter::Me => Some(1),
        super::SenderFilter::Others => Some(0),
    };
    // message -> (its current body, whether that body was collected)
    let mut admitted: HashMap<i64, (Option<i64>, bool)> = HashMap::new();
    let mut out: Vec<Match> = Vec::new();
    let mut capped = false;
    // The lowest body rowid read: every matching body above it was seen.
    let mut lowest: Option<i64> = None;
    p.sql(|c| {
        let mut st = c.prepare(SQL_GLOBAL)?;
        let mut rows = st.query(rusqlite::params![
            expr,
            q.include_earlier,
            q.include_events,
            range.0,
            range.1,
            q.filters.has_attachment,
            who,
            q.filters.chat.map(|c| c.get()),
            // Without earlier versions a message has one matching body,
            // so the cap holds in SQL; with them, rows are read until the
            // cap's distinct messages (bounded by the phase's budget).
            if q.include_earlier {
                -1
            } else {
                limit(cap + 1)
            }
        ])?;
        while let Some(r) = rows.next()? {
            let body: i64 = r.get(0)?;
            let message: i64 = r.get(1)?;
            let current_body: Option<i64> = r.get(3)?;
            let current = current_body == Some(body);
            let full = admitted.len() == cap;
            match admitted.get_mut(&message) {
                Some(e) => e.1 |= current,
                None if full => {
                    capped = true;
                    break;
                }
                None => {
                    admitted.insert(message, (current_body, current));
                }
            }
            lowest = Some(body);
            out.push(Match {
                body,
                message,
                key: r.get(2)?,
                current,
            });
        }
        Ok(())
    })?;
    // The cut may have left an admitted message's current body unread
    // (its rowid is below the last one read): probe it.
    if capped && q.include_earlier {
        let mut extra = Vec::new();
        let mut probe = p.conn().prepare(SQL_PROBE).map_err(|e| p.err(&e))?;
        for m in &out {
            let Some(&(Some(cur), false)) = admitted.get(&m.message) else {
                continue;
            };
            if lowest.is_some_and(|low| cur >= low) {
                continue;
            }
            p.checkpoint()?;
            let hit = probe
                .query_row(rusqlite::params![expr, cur], |_| Ok(()))
                .optional()
                .map_err(|e| p.err(&e))?;
            if let Some(e) = admitted.get_mut(&m.message) {
                e.1 = true;
            }
            if hit.is_some() {
                extra.push(Match {
                    body: cur,
                    message: m.message,
                    key: m.key.clone(),
                    current: true,
                });
            }
        }
        out.extend(extra);
    }
    Ok((out, capped))
}

/// The scoped plan: the matches of at most [`SEARCH_MATCH_CAP`] distinct
/// messages (every matching body of each), and whether there were more.
fn scoped(
    p: &Phase<'_>,
    q: &SearchRequest,
    expr: &str,
    chat: i64,
    ranges: &[KeyRange],
) -> Result<(Vec<Match>, bool), ArchiveError> {
    let cap = crate::fault::search_match_cap(SEARCH_MATCH_CAP);
    let filter = RowFilter::of(q);
    let mut out: Vec<Match> = Vec::new();
    let mut messages = 0usize;
    let c = p.conn();
    let mut st = RowStmts::new(c).map_err(|e| p.err(&e))?;
    let mut probe = c.prepare(SQL_PROBE).map_err(|e| p.err(&e))?;
    for (lo, hi) in ranges {
        let mut cursor: (Vec<u8>, i64) = (hi.clone(), i64::MIN);
        loop {
            p.checkpoint()?;
            let batch = p.sql(|c| chat_batch(c, chat, (&cursor.0, cursor.1), lo, SCOPED_BATCH))?;
            let n = batch.len();
            for (id, key) in batch {
                p.checkpoint()?;
                let facts = st.facts(id).map_err(|e| p.err(&e))?;
                if let Some(facts) = facts {
                    if filter
                        .keeps(&mut st.attachment, id, &facts)
                        .map_err(|e| p.err(&e))?
                    {
                        let bodies: Vec<i64> = if q.include_earlier {
                            st.body_ids
                                .query_map([id], |r| r.get(0))
                                .and_then(|rows| rows.collect())
                                .map_err(|e| p.err(&e))?
                        } else {
                            facts.current_body.into_iter().collect()
                        };
                        let mut hits = Vec::new();
                        for b in bodies {
                            let hit = probe
                                .query_row(rusqlite::params![expr, b], |_| Ok(()))
                                .optional()
                                .map_err(|e| p.err(&e))?;
                            if hit.is_some() {
                                hits.push(Match {
                                    message: id,
                                    body: b,
                                    key: key.clone(),
                                    current: facts.current_body == Some(b),
                                });
                            }
                        }
                        if !hits.is_empty() {
                            if messages == cap {
                                return Ok((out, true));
                            }
                            messages += 1;
                            out.extend(hits);
                        }
                    }
                }
                cursor = (key, id);
            }
            if n < SCOPED_BATCH {
                break;
            }
        }
    }
    Ok((out, false))
}

/// Collect the matches of a words search: the plan used, the matches of
/// at most [`SEARCH_MATCH_CAP`] messages, and whether there were more.
pub(crate) fn collect(
    conn: &ReadConn,
    q: &SearchRequest,
    expr: &str,
    prefix: bool,
    ranges: &[KeyRange],
    cancel: &CancelToken,
) -> Result<(WordsPlan, Vec<Match>, bool), ArchiveError> {
    let deadline = Instant::now() + ViewBudget::Search.window();
    conn.viewing(ViewBudget::Search, Some(cancel), deadline, |p| {
        let threshold = crate::fault::scoped_search_rows(SCOPED_CHAT_ROWS);
        let scoped_chat = match q.filters.chat {
            Some(chat) if !prefix && threshold > 0 => {
                let rows: i64 = p.sql(|c| {
                    c.query_row(
                        SQL_CHAT_ROWS,
                        rusqlite::params![chat.get(), super::CappedCount::limit(threshold)],
                        |r| r.get(0),
                    )
                })?;
                (u64::try_from(rows).unwrap_or(u64::MAX) <= threshold).then_some(chat.get())
            }
            _ => None,
        };
        let (plan, (matches, capped)) = match scoped_chat {
            Some(chat) => (
                WordsPlan::Scoped,
                scoped(p, q, expr, chat, ranges).map_err(fts_err)?,
            ),
            None => {
                // One statement over the whole key range the ranges span:
                // the global plan reads in rowid order anyway.
                let lo = ranges.first().map(|r| r.0.as_slice()).unwrap_or(&[]);
                let hi = ranges.last().map(|r| r.1.as_slice()).unwrap_or(&[]);
                (
                    WordsPlan::Global,
                    global(p, q, expr, (lo, hi)).map_err(fts_err)?,
                )
            }
        };
        Ok((plan, matches, capped))
    })
}
