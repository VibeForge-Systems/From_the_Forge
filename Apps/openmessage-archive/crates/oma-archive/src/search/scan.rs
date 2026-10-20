//! The exact-text scan (M5 spec §4.4): for CJK, Thai, emoji and anything
//! else unicode61 does not split into words, or whenever the user asks.
//!
//! - **Keyset batches.** Each batch reads at most [`SCAN_BATCH`] keys,
//!   newest first, on `message_sort` (or `chat_message_timeline` for one
//!   conversation), carrying `(sort_key, id)` between batches in Rust. The
//!   filters are applied in Rust, so the `LIMIT` bounds **rows examined**,
//!   not matches. The dated range is walked first, then the undated one.
//! - **Matching runs in Rust** (`str::contains`, linear in the text), on
//!   each kept message's current body (or every body version with
//!   `include_earlier`), read by primary key. No single SQL opcode runs for
//!   long, and the cancel token and the deadline are checked between rows.
//! - **Bounds:** `ViewBudget::ScanBatch` per batch, at most
//!   [`SCAN_EXAMINED_LIMIT`] messages examined, [`super::SEARCH_MATCH_CAP`]
//!   matches, one 20 s deadline and the cancel token. A cancel is
//!   `Cancelled`; any other stop ends the scan early, and the coverage
//!   says what was examined.
//! - Case-, accent- and normalisation-sensitive: an NFC needle does not
//!   find NFD text.

use std::time::Instant;

use super::words::chat_batch;
use super::KeyRange;
use super::{
    text_ref, Match, RowFacts, RowFilter, RowStmts, ScanCoverage, ScanProgress, ScanStop,
    SearchRequest, SEARCH_MATCH_CAP,
};
use crate::conn::{Phase, ReadConn, ViewBudget};
use crate::error::ArchiveError;
use crate::progress::CancelToken;

/// Keys read per batch.
pub const SCAN_BATCH: usize = 5_000;
/// Rows between two checks of the cancel token and the deadline.
const CHECK_EVERY: usize = 64;
/// The most messages one scan examines.
pub const SCAN_EXAMINED_LIMIT: u64 = 2_000_000;

/// One batch of the whole-archive scan, streamed: each message with its
/// current body's text, so a row is matched as it is read (never a whole
/// batch of texts in memory).
pub(crate) const SQL_SCAN_BATCH: &str = "SELECT m.id, m.sort_key, m.current_body_id, \
       m.assoc_present, m.item_kind, m.is_from_me, b.display_text \
     FROM message m LEFT JOIN message_body b ON b.id = m.current_body_id \
     WHERE (m.sort_key, m.id) < (?1, ?2) AND m.sort_key >= ?3 \
     ORDER BY m.sort_key DESC, m.id DESC LIMIT ?4";
pub(crate) const SQL_BODY_TEXT: &str = "SELECT display_text FROM message_body WHERE id = ?1";
pub(crate) const SQL_BODY_TEXTS: &str =
    "SELECT id, display_text FROM message_body WHERE message_id = ?1";

#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    [
        ("search scan batch", SQL_SCAN_BATCH),
        ("search scan body text", SQL_BODY_TEXT),
        ("search scan body texts", SQL_BODY_TEXTS),
        ("search attachment exists", super::SQL_HAS_ATTACHMENT),
    ]
    .into_iter()
    .map(|(n, s)| (n, s.to_owned()))
    .collect()
}

/// The bodies of message `id` that contain `needle`: (body, current).
/// `current`: the current body's text when the caller already read it
/// (the streamed whole-archive batch); `None` reads it by primary key.
fn matching_bodies(
    st: &mut RowStmts<'_>,
    id: i64,
    facts: &RowFacts,
    earlier: bool,
    needle: &str,
    current: Option<Option<&str>>,
) -> rusqlite::Result<Vec<(i64, bool)>> {
    use rusqlite::OptionalExtension;
    if earlier {
        let mut rows = st.body_texts.query([id])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let body: i64 = r.get(0)?;
            if text_ref(r.get_ref(1)?).is_some_and(|t| t.contains(needle)) {
                out.push((body, facts.current_body == Some(body)));
            }
        }
        return Ok(out);
    }
    let Some(body) = facts.current_body else {
        return Ok(Vec::new());
    };
    if let Some(text) = current {
        let found = text.is_some_and(|t| t.contains(needle));
        return Ok(if found {
            vec![(body, true)]
        } else {
            Vec::new()
        });
    }
    let found = st
        .body_text
        .query_row([body], |r| {
            Ok(text_ref(r.get_ref(0)?).is_some_and(|t| t.contains(needle)))
        })
        .optional()?
        .unwrap_or(false);
    Ok(if found {
        vec![(body, true)]
    } else {
        Vec::new()
    })
}

/// Where the scan is and what it has found, kept across batches (and
/// across a batch that stops part-way).
struct State<'n> {
    filter: RowFilter,
    earlier: bool,
    needle: &'n str,
    matches: Vec<Match>,
    examined: u64,
    matched: u64,
    stop: Option<ScanStop>,
    examined_limit: u64,
    /// The most matches kept: [`SEARCH_MATCH_CAP`] for the viewer, the
    /// export's cap for its re-run (M6 spec §3.6).
    cap: u64,
}

/// What one visited row needs besides its id and key.
struct Visit<'r> {
    facts: Option<RowFacts>,
    /// The current body's text, when the batch statement read it.
    current: Option<Option<&'r str>>,
}

impl State<'_> {
    /// Examine one message. `Ok(false)` ends the scan (a limit was
    /// reached; `stop` says which).
    fn visit(
        &mut self,
        st: &mut RowStmts<'_>,
        id: i64,
        key: &[u8],
        v: Visit<'_>,
    ) -> rusqlite::Result<bool> {
        if self.examined >= self.examined_limit {
            self.stop = Some(ScanStop::ExaminedLimit);
            return Ok(false);
        }
        let facts = match v.facts {
            Some(f) => Some(f),
            None => st.facts(id)?,
        };
        self.examined += 1;
        let Some(f) = facts else { return Ok(true) };
        if !self.filter.keeps(&mut st.attachment, id, &f)? {
            return Ok(true);
        }
        let found = matching_bodies(st, id, &f, self.earlier, self.needle, v.current)?;
        if found.is_empty() {
            return Ok(true);
        }
        self.matched += 1;
        if self.matched > self.cap {
            self.stop = Some(ScanStop::MatchCap);
            return Ok(false);
        }
        for (body, current) in found {
            self.matches.push(Match {
                message: id,
                body,
                key: key.to_vec(),
                current,
            });
        }
        Ok(true)
    }
}

/// Run the scan: the matches (at most [`SEARCH_MATCH_CAP`]) and what it
/// covered. Each batch runs in its own `ViewBudget::ScanBatch` phase,
/// within one 20 s deadline.
pub(crate) fn run(
    conn: &ReadConn,
    q: &SearchRequest,
    needle: &str,
    ranges: &[KeyRange],
    cancel: &CancelToken,
    progress: &mut dyn FnMut(ScanProgress),
) -> Result<(Vec<Match>, ScanCoverage), ArchiveError> {
    let deadline = Instant::now() + ViewBudget::ScanBatch.window();
    let cap = u64::try_from(SEARCH_MATCH_CAP).unwrap_or(u64::MAX);
    walk(q, needle, ranges, cap, progress, |batch| {
        conn.viewing(ViewBudget::ScanBatch, Some(cancel), deadline, batch)
    })
}

/// The same scan inside a phase the caller set up (an export snapshot's
/// cancellable phase, M6 spec §3.6): no op budget and no deadline, so it
/// stops only at the examined bound, at `cap` matches, at the end of its
/// range or on a cancel.
pub(crate) fn run_in(
    p: &Phase<'_>,
    q: &SearchRequest,
    needle: &str,
    ranges: &[KeyRange],
    cap: u64,
    progress: &mut dyn FnMut(ScanProgress),
) -> Result<(Vec<Match>, ScanCoverage), ArchiveError> {
    walk(q, needle, ranges, cap, progress, |batch| batch(p))
}

/// One batch's work in the phase `in_phase` gives it.
type Batch<'b> = &'b mut dyn FnMut(&Phase<'_>) -> Result<(), ArchiveError>;

/// The scan over `ranges`, newest first, each batch run through
/// `in_phase`.
fn walk(
    q: &SearchRequest,
    needle: &str,
    ranges: &[KeyRange],
    cap: u64,
    progress: &mut dyn FnMut(ScanProgress),
    mut in_phase: impl FnMut(Batch<'_>) -> Result<(), ArchiveError>,
) -> Result<(Vec<Match>, ScanCoverage), ArchiveError> {
    let chat = q.filters.chat.map(|c| c.get());
    let mut s = State {
        filter: RowFilter::of(q),
        earlier: q.include_earlier,
        needle,
        matches: Vec::new(),
        examined: 0,
        matched: 0,
        stop: None,
        examined_limit: crate::fault::scan_examined_limit(SCAN_EXAMINED_LIMIT),
        cap,
    };
    'ranges: for (lo, hi) in ranges {
        let mut cursor: (Vec<u8>, i64) = (hi.clone(), i64::MIN);
        loop {
            let mut rows_read = 0usize;
            let outcome = in_phase(&mut |p: &Phase<'_>| {
                batch(p, &mut s, chat, &mut cursor, lo, &mut rows_read)
            });
            match outcome {
                Ok(()) => {}
                Err(ArchiveError::QueryTooBroad) => {
                    s.stop = Some(ScanStop::TooBroad);
                }
                Err(e) => return Err(e),
            }
            progress(ScanProgress {
                examined: s.examined,
                matched: s.matched.min(s.cap),
            });
            if s.stop.is_some() {
                break 'ranges;
            }
            if rows_read < SCAN_BATCH {
                break;
            }
        }
    }
    let coverage = ScanCoverage {
        examined: s.examined,
        complete: s.stop.is_none(),
        stop: s.stop,
    };
    Ok((s.matches, coverage))
}

/// One keyset batch of at most [`SCAN_BATCH`] keys below `cursor` and at
/// or above `lo`, visited in phase `p`; `cursor` moves past every row
/// examined.
fn batch(
    p: &Phase<'_>,
    s: &mut State<'_>,
    chat: Option<i64>,
    cursor: &mut (Vec<u8>, i64),
    lo: &[u8],
    rows_read: &mut usize,
) -> Result<(), ArchiveError> {
    let c = p.conn();
    let mut st = RowStmts::new(c).map_err(|e| p.err(&e))?;
    match chat {
        Some(chat) => {
            let rows = p.sql(|c| chat_batch(c, chat, (&cursor.0, cursor.1), lo, SCAN_BATCH))?;
            *rows_read = rows.len();
            for (id, key) in rows {
                p.checkpoint()?;
                let v = Visit {
                    facts: None,
                    current: None,
                };
                let go = s.visit(&mut st, id, &key, v).map_err(|e| p.err(&e))?;
                if !go {
                    return Ok(());
                }
                *cursor = (key, id);
            }
        }
        None => {
            let mut batch = c.prepare(SQL_SCAN_BATCH).map_err(|e| p.err(&e))?;
            let limit = i64::try_from(SCAN_BATCH).unwrap_or(i64::MAX);
            let mut rows = batch
                .query(rusqlite::params![cursor.0, cursor.1, lo, limit])
                .map_err(|e| p.err(&e))?;
            while let Some(r) = rows.next().map_err(|e| p.err(&e))? {
                if rows_read.is_multiple_of(CHECK_EVERY) {
                    p.checkpoint()?;
                }
                *rows_read += 1;
                let row = (|| -> rusqlite::Result<_> {
                    let id: i64 = r.get(0)?;
                    let key: &[u8] = r.get_ref(1)?.as_blob()?;
                    let facts = RowFacts {
                        current_body: r.get(2)?,
                        assoc_present: r.get(3)?,
                        item_kind: r.get(4)?,
                        is_from_me: r.get(5)?,
                    };
                    let text = text_ref(r.get_ref(6)?);
                    let v = Visit {
                        facts: Some(facts),
                        current: Some(text.as_deref()),
                    };
                    let go = s.visit(&mut st, id, key, v)?;
                    if go {
                        cursor.0.clear();
                        cursor.0.extend_from_slice(key);
                        cursor.1 = id;
                    }
                    Ok(go)
                })()
                .map_err(|e| p.err(&e))?;
                if !row {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}
