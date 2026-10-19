//! Search (M5 spec §4): the query grammar ([`query`]), words mode through
//! FTS5 with a chat-scoped plan ([`words`]), the keyset-batched exact-text
//! scan that matches in Rust ([`scan`]), and safe highlighting
//! ([`highlight`]).
//!
//! - **One call, one snapshot.** [`ArchiveReader::search`] collects a
//!   [`SearchResultSet`] of message and body ids; it holds no text.
//!   [`ArchiveReader::search_hits`] reads one page of it (at most
//!   [`HITS_PER_CALL`]) with highlighted, masked snippets.
//! - **Order.** Newest first: dated hits by `(sort_key DESC, id DESC)`,
//!   then undated hits, listed after them under "Undated" (D91). A message
//!   is listed once, by its current body when that matched.
//! - **Caps, honestly worded.** At most [`SEARCH_MATCH_CAP`] matches are
//!   kept. Words mode collects in FTS rowid order (import order, not
//!   date), so a capped global words search is *not* the newest
//!   [`SEARCH_MATCH_CAP`] ([`SearchResultSet::newest_kept`] is `false`).
//! - **Budgets.** Words mode runs under `ViewBudget::Search` and a 5 s
//!   deadline; each exact-text batch under `ViewBudget::ScanBatch` within
//!   one 20 s deadline; a page of hits under `Assembly`, then `Hits` (2 s)
//!   for `highlight()`. Over budget is `QueryTooBroad`, a cancel
//!   `Cancelled`, never `Damaged`; an FTS error is `SearchUnavailable`.
//! - **Defaults (owner Q3b, D115).** Current bodies only; turning
//!   `include_earlier` on is the explicit reveal of earlier and unsent
//!   text for search. One-time codes are always withheld in snippets.
//! - **Nothing is persisted** (owner Q1c): no search history.
//!
//! Query text is held as `Redacted` and never logged; `Debug` of every
//! type here prints lengths, ids and codes only.

pub mod highlight;
pub mod query;
pub(crate) mod scan;
pub(crate) mod words;

use std::ops::Range;
use std::time::Instant;

use oma_messages::Redacted;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension};

use crate::conn::{Phase, SearchInterrupt, ViewBudget};
use crate::error::ArchiveError;
use crate::progress::CancelToken;
use crate::reader::ArchiveReader;
use crate::schema::codes::{AssocKindCode, ItemKindCode};
use crate::sortkey;
use crate::view::conversations::{ONE_TIME_CODE_BIT, SQL_FLAGGED_PARTS};
use crate::view::detail::SQL_ANY_UNSENT;
use crate::view::{BodyId, CappedCount, ChatId, DirectionCode, MessageId};

pub use highlight::{Highlight, HitPiece, HitText, NoHighlight, SNIPPET_CHARS};
pub use query::{NEEDLE_CHARS, PREFIX_MIN_CHARS, QUERY_BYTES, QUERY_TERMS, TERM_BYTES};

/// The most matches a search keeps (§4.3).
pub const SEARCH_MATCH_CAP: usize = 10_000;
/// The most hits one [`ArchiveReader::search_hits`] call reads.
pub const HITS_PER_CALL: usize = 50;
/// The other conversations a hit counts before it says "N+".
pub const HIT_OTHER_CHATS: u64 = 100;

/// What the request asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SearchMode {
    /// Words through the full-text index, or exact text when the query
    /// holds an emoji or a script the index does not split into words.
    #[default]
    Auto,
    /// Exact text, always.
    Exact,
}

/// The mode a search ran in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SearchRan {
    /// FTS5 `MATCH` on words (case- and accent-insensitive, as unicode61
    /// folds them).
    Words,
    /// The exact-text scan: case-, accent- and normalisation-sensitive.
    Exact,
}

/// How a words search collected its matches (§4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WordsPlan {
    /// FTS5 over the whole archive, filters applied per match.
    Global,
    /// The conversation's timeline walked in keyset batches, each message's
    /// bodies checked through an FTS5 rowid-constrained `MATCH`.
    Scoped,
}

/// Whose messages to search, on `is_from_me` as recorded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SenderFilter {
    #[default]
    Anyone,
    /// `is_from_me = 1`.
    Me,
    /// `is_from_me = 0`. Messages whose direction is not recorded match
    /// neither `Me` nor `Others`.
    Others,
}

/// The filters of a search (§4.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct SearchFilters {
    /// One conversation (the chat-scoped plan).
    pub chat: Option<ChatId>,
    /// Dated at or after this UTC instant (Unix ns). A date filter always
    /// leaves undated messages out.
    pub from_utc_ns: Option<i64>,
    /// Dated strictly before this UTC instant (Unix ns).
    pub until_utc_ns: Option<i64>,
    /// Only messages with an attachment row (`message_attachment`, never
    /// the cache flag).
    pub has_attachment: bool,
    pub sender: SenderFilter,
}

/// One search request. The query text is `Redacted`: `Debug` prints its
/// length only.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SearchRequest {
    query: Redacted<String>,
    pub mode: SearchMode,
    pub filters: SearchFilters,
    /// Search earlier body versions and text kept from before an unsend
    /// too: the explicit reveal for search (D115). Off by default.
    pub include_earlier: bool,
    /// Search reactions and system events too. Off by default.
    pub include_events: bool,
}

impl SearchRequest {
    /// A request for `query` with the defaults: automatic mode, no
    /// filters, current bodies of messages only.
    pub fn new(query: &str) -> SearchRequest {
        SearchRequest {
            query: Redacted::new(query.to_owned()),
            mode: SearchMode::Auto,
            filters: SearchFilters::default(),
            include_earlier: false,
            include_events: false,
        }
    }

    /// The mode this request runs in, or why its query is refused.
    pub fn resolved_mode(&self) -> Result<SearchRan, ArchiveError> {
        Ok(self.parse()?.ran())
    }

    fn parse(&self) -> Result<Parsed, ArchiveError> {
        let raw = self.query.expose().as_str();
        let exact = self.mode == SearchMode::Exact || raw.chars().any(query::needs_exact);
        if exact {
            Ok(Parsed::Exact(query::needle(raw)?))
        } else {
            let terms = query::terms(raw)?;
            let (expr, prefix) = query::fts_expression(&terms);
            Ok(Parsed::Words { expr, prefix })
        }
    }
}

/// A parsed query: the FTS5 expression, or the exact-text needle.
pub(crate) enum Parsed {
    Words { expr: String, prefix: bool },
    Exact(String),
}

impl Parsed {
    fn ran(&self) -> SearchRan {
        match self {
            Parsed::Words { .. } => SearchRan::Words,
            Parsed::Exact(_) => SearchRan::Exact,
        }
    }
}

/// Progress of an exact-text scan, pushed after each batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScanProgress {
    /// Messages examined so far.
    pub examined: u64,
    /// Messages matched so far.
    pub matched: u64,
}

/// Why an exact-text scan stopped before the end of its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ScanStop {
    /// [`scan::SCAN_EXAMINED_LIMIT`] messages were examined.
    ExaminedLimit,
    /// [`SEARCH_MATCH_CAP`] matches were kept.
    MatchCap,
    /// A batch's budget or the scan's 20 s deadline ran out.
    TooBroad,
}

/// What an exact-text scan covered: "Searched the newest N dated
/// messages".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScanCoverage {
    pub examined: u64,
    /// Every message in range was examined.
    pub complete: bool,
    pub stop: Option<ScanStop>,
}

/// Which stored version of the message matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HitVersion {
    /// The current body.
    Current,
    /// An earlier body version: "matched an earlier version".
    Earlier,
    /// An earlier body of a message with an unsent part: "unsent; earlier
    /// text kept".
    Unsent,
}

/// One entry of a result set: ids only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct SetEntry {
    pub message: MessageId,
    /// The body that matched (the current one when it did).
    pub body: BodyId,
    pub current: bool,
    pub dated: bool,
}

/// The matches of one search, newest first. It holds ids only; the
/// shell keeps one live set per viewer session.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SearchResultSet {
    pub ran: SearchRan,
    /// Words mode only.
    pub plan: Option<WordsPlan>,
    /// Dated entries first, then undated ones.
    pub entries: Vec<SetEntry>,
    /// The first `dated` entries are dated; the rest go under "Undated".
    pub dated: usize,
    /// More than [`SEARCH_MATCH_CAP`] matches: only that many were kept.
    pub capped: bool,
    /// When capped: whether the kept matches are the newest ones. A global
    /// words search collects in import order, so it says "not necessarily
    /// the newest" (§4.3).
    pub newest_kept: bool,
    /// Exact text only.
    pub coverage: Option<ScanCoverage>,
}

/// One hit, as a page of results shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SearchHit {
    pub message: MessageId,
    pub body: BodyId,
    /// The message's lowest chat id; `None` when it is in no conversation.
    pub chat: Option<ChatId>,
    /// "Also in N other conversations".
    pub other_chats: CappedCount,
    /// `None` for an undated message.
    pub utc_ns: Option<i64>,
    pub direction: DirectionCode,
    pub item: Option<ItemKindCode>,
    /// A reaction or another association (when events are included); a
    /// folded reaction opens its target (§3.5 step 5).
    pub association: Option<AssocKindCode>,
    /// Read when the hit is served, not when the search ran.
    pub version: HitVersion,
    /// `None` when the body has no text any more (it changed since the
    /// search), or when it is no longer the current body and the request
    /// did not include earlier versions (D115).
    pub text: Option<HitText>,
}

// ---------------------------------------------------------------------------
// Statements
// ---------------------------------------------------------------------------

const SQL_HIT_MESSAGE: &str = "SELECT current_body_id, sort_key, is_from_me, is_from_me_st, \
       item_kind, assoc_present, assoc_kind FROM message WHERE id = ?1";
const SQL_HIT_BODY: &str =
    "SELECT display_text, display_source, run_issue FROM message_body WHERE id = ?1";
const SQL_HIT_CHATS: &str =
    "SELECT chat_id FROM chat_message WHERE message_id = ?1 ORDER BY chat_id LIMIT ?2";
const SQL_HIGHLIGHT: &str = "SELECT highlight(message_fts, 0, ?1, ?2) FROM message_fts \
     WHERE message_fts MATCH ?3 AND rowid = ?4";
const SQL_UNSEARCHABLE: &str = "SELECT count(*) FROM message m \
     LEFT JOIN message_body b ON b.id = m.current_body_id \
     WHERE m.assoc_present = 0 AND m.item_kind = 1 AND b.display_text IS NULL";

#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    let mut v: Vec<(&'static str, String)> = [
        ("search hit message", SQL_HIT_MESSAGE),
        ("search hit body", SQL_HIT_BODY),
        ("search hit chats", SQL_HIT_CHATS),
        ("search highlight", SQL_HIGHLIGHT),
        ("search unsearchable count", SQL_UNSEARCHABLE),
    ]
    .into_iter()
    .map(|(n, s)| (n, s.to_owned()))
    .collect();
    v.extend(words::plan_statements());
    v.extend(scan::plan_statements());
    v
}

// ---------------------------------------------------------------------------
// Shared pieces
// ---------------------------------------------------------------------------

/// A half-open range of sort keys, `[lo, hi)`.
pub(crate) type KeyRange = (Vec<u8>, Vec<u8>);

/// The sort-key ranges a search walks, newest first: the dated range
/// bounded by the date filters, then (with no date filter) the undated
/// range. `[lo, hi)` each.
pub(crate) fn key_ranges(f: &SearchFilters) -> Result<Vec<KeyRange>, ArchiveError> {
    if let (Some(a), Some(b)) = (f.from_utc_ns, f.until_utc_ns) {
        if a >= b {
            return Err(ArchiveError::Internal("search date range"));
        }
    }
    let dated_end = sortkey::DATED_END.to_vec();
    if f.from_utc_ns.is_none() && f.until_utc_ns.is_none() {
        return Ok(vec![
            (Vec::new(), dated_end.clone()),
            (dated_end, vec![sortkey::UNDATED_PREFIX + 1]),
        ]);
    }
    let lo = f
        .from_utc_ns
        .map_or(Vec::new(), |ns| sortkey::dated(ns).to_vec());
    let hi = f
        .until_utc_ns
        .map_or(dated_end, |ns| sortkey::dated(ns).to_vec());
    Ok(vec![(lo, hi)])
}

/// The per-message filters, applied in Rust (the exact-text scan and the
/// scoped plan, so a batch bounds rows examined, not matches).
pub(crate) struct RowFilter {
    events: bool,
    sender: SenderFilter,
    attachment: bool,
}

/// The cheap columns a filter reads.
pub(crate) struct RowFacts {
    pub(crate) current_body: Option<i64>,
    pub(crate) assoc_present: i64,
    pub(crate) item_kind: i64,
    pub(crate) is_from_me: Option<i64>,
}

pub(crate) const SQL_HAS_ATTACHMENT: &str =
    "SELECT 1 FROM message_attachment WHERE message_id = ?1 LIMIT 1";

impl RowFilter {
    pub(crate) fn of(q: &SearchRequest) -> RowFilter {
        RowFilter {
            events: q.include_events,
            sender: q.filters.sender,
            attachment: q.filters.has_attachment,
        }
    }

    /// Whether message `id` passes; `attachment` is a prepared
    /// [`SQL_HAS_ATTACHMENT`].
    pub(crate) fn keeps(
        &self,
        attachment: &mut rusqlite::Statement<'_>,
        id: i64,
        r: &RowFacts,
    ) -> rusqlite::Result<bool> {
        if !self.events && !(r.assoc_present == 0 && r.item_kind == 1) {
            return Ok(false);
        }
        let sender_ok = match self.sender {
            SenderFilter::Anyone => true,
            SenderFilter::Me => r.is_from_me == Some(1),
            SenderFilter::Others => r.is_from_me == Some(0),
        };
        if !sender_ok {
            return Ok(false);
        }
        if self.attachment {
            return Ok(attachment.query_row([id], |_| Ok(())).optional()?.is_some());
        }
        Ok(true)
    }
}

/// The per-row statements of a scan or a scoped walk, prepared once per
/// phase (rusqlite's statement cache is not enabled in this build).
pub(crate) struct RowStmts<'c> {
    pub(crate) row: rusqlite::Statement<'c>,
    pub(crate) attachment: rusqlite::Statement<'c>,
    pub(crate) body_text: rusqlite::Statement<'c>,
    pub(crate) body_texts: rusqlite::Statement<'c>,
    pub(crate) body_ids: rusqlite::Statement<'c>,
}

impl<'c> RowStmts<'c> {
    pub(crate) fn new(c: &'c Connection) -> rusqlite::Result<RowStmts<'c>> {
        Ok(RowStmts {
            row: c.prepare(words::SQL_ROW)?,
            attachment: c.prepare(SQL_HAS_ATTACHMENT)?,
            body_text: c.prepare(scan::SQL_BODY_TEXT)?,
            body_texts: c.prepare(scan::SQL_BODY_TEXTS)?,
            body_ids: c.prepare(words::SQL_BODY_IDS)?,
        })
    }

    /// The cheap columns of message `id`.
    pub(crate) fn facts(&mut self, id: i64) -> rusqlite::Result<Option<RowFacts>> {
        self.row
            .query_row([id], |r| {
                Ok(RowFacts {
                    current_body: r.get(0)?,
                    assoc_present: r.get(1)?,
                    item_kind: r.get(2)?,
                    is_from_me: r.get(3)?,
                })
            })
            .optional()
    }
}

/// One match before ordering: (message, body, its sort key, current).
#[derive(Debug, Clone)]
pub(crate) struct Match {
    pub(crate) message: i64,
    pub(crate) body: i64,
    pub(crate) key: Vec<u8>,
    pub(crate) current: bool,
}

/// Matches as a result set: one entry per message (its current body when
/// that matched, else the highest body id), dated newest first, then
/// undated.
fn order(mut matches: Vec<Match>) -> (Vec<SetEntry>, usize) {
    matches.sort_by(|a, b| {
        (a.message, !a.current, std::cmp::Reverse(a.body)).cmp(&(
            b.message,
            !b.current,
            std::cmp::Reverse(b.body),
        ))
    });
    matches.dedup_by_key(|m| m.message);
    let dated_end = sortkey::DATED_END;
    matches.sort_by(|a, b| {
        let ad = a.key.as_slice() < dated_end.as_slice();
        let bd = b.key.as_slice() < dated_end.as_slice();
        bd.cmp(&ad)
            .then_with(|| b.key.cmp(&a.key))
            .then_with(|| b.message.cmp(&a.message))
    });
    let dated = matches
        .iter()
        .take_while(|m| m.key.as_slice() < dated_end.as_slice())
        .count();
    let entries = matches
        .into_iter()
        .map(|m| SetEntry {
            message: MessageId::new(m.message),
            body: BodyId::new(m.body),
            current: m.current,
            dated: m.key.as_slice() < dated_end.as_slice(),
        })
        .collect();
    (entries, dated)
}

/// Text of a cell, borrowed when it is valid UTF-8.
pub(crate) fn text_ref(v: ValueRef<'_>) -> Option<std::borrow::Cow<'_, str>> {
    match v {
        ValueRef::Text(b) => Some(String::from_utf8_lossy(b)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Hits
// ---------------------------------------------------------------------------

/// What one hit needs, read in the first phase of `search_hits`.
struct HitFacts {
    entry: SetEntry,
    chat: Option<i64>,
    others: CappedCount,
    utc_ns: Option<i64>,
    direction: DirectionCode,
    item: Option<ItemKindCode>,
    association: Option<AssocKindCode>,
    version: HitVersion,
    text: Option<String>,
    lossy: bool,
    masks: Vec<(u32, u32)>,
    mask_all: bool,
}

type MessageRow = (
    Option<i64>,
    Vec<u8>,
    Option<i64>,
    Option<i64>,
    i64,
    i64,
    Option<i64>,
);

/// The facts of hit `e`, read now: whether its body is still the current
/// one is decided from `current_body_id` at read time, never from the set
/// (an import may have made it earlier since the search). An earlier body
/// shows no text unless the request included earlier versions (D115).
fn hit_facts(
    c: &Connection,
    e: SetEntry,
    include_earlier: bool,
) -> rusqlite::Result<Option<HitFacts>> {
    let row: Option<MessageRow> = c
        .prepare(SQL_HIT_MESSAGE)?
        .query_row([e.message.get()], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .optional()?;
    let Some((current_body, key, from_me, from_me_st, item, present, kind)) = row else {
        return Ok(None);
    };
    let chats: Vec<i64> = c
        .prepare(SQL_HIT_CHATS)?
        .query_map(
            rusqlite::params![e.message.get(), CappedCount::limit(HIT_OTHER_CHATS + 1)],
            |r| r.get(0),
        )?
        .collect::<rusqlite::Result<_>>()?;
    let others = i64::try_from(chats.len().saturating_sub(1)).unwrap_or(i64::MAX);
    let current = current_body == Some(e.body.get());
    let version = if current {
        HitVersion::Current
    } else {
        let unsent = match current_body {
            Some(b) => c
                .prepare(SQL_ANY_UNSENT)?
                .query_row([b], |_| Ok(()))
                .optional()?
                .is_some(),
            None => false,
        };
        if unsent {
            HitVersion::Unsent
        } else {
            HitVersion::Earlier
        }
    };
    let body: Option<(Option<String>, Option<i64>, Option<i64>)> = c
        .prepare(SQL_HIT_BODY)?
        .query_row([e.body.get()], |r| {
            Ok((
                text_ref(r.get_ref(0)?).map(|t| t.into_owned()),
                r.get(1)?,
                r.get(2)?,
            ))
        })
        .optional()?;
    let (text, source, run_issue) = body.unwrap_or((None, None, None));
    // Earlier text is shown only on the explicit reveal of earlier
    // versions: a body that stopped being current since the search sends
    // no text otherwise.
    let text = text.filter(|_| current || include_earlier);
    let masks: Vec<(u32, u32)> = if text.is_some() {
        let clamp = |v: i64| u32::try_from(v.max(0)).unwrap_or(u32::MAX);
        c.prepare(SQL_FLAGGED_PARTS)?
            .query_map(
                rusqlite::params![e.body.get(), 1i64 << ONE_TIME_CODE_BIT],
                |r| Ok((clamp(r.get(0)?), clamp(r.get(1)?))),
            )?
            .collect::<rusqlite::Result<_>>()?
    } else {
        Vec::new()
    };
    // Code ranges apply only when the formatting was read cleanly (§3.6);
    // otherwise a body with a code is withheld whole.
    let mask_all = !masks.is_empty() && (source != Some(1) || run_issue.is_some());
    Ok(Some(HitFacts {
        entry: e,
        chat: chats.first().copied(),
        others: CappedCount::of(others, HIT_OTHER_CHATS),
        utc_ns: sortkey::decode_dated(&key),
        direction: DirectionCode::of(from_me, from_me_st),
        item: ItemKindCode::from_code(item),
        association: if present == 1 {
            kind.and_then(AssocKindCode::from_code)
        } else {
            None
        },
        version,
        text,
        lossy: source == Some(3),
        masks,
        mask_all,
    }))
}

/// The marked output of `highlight()` for body `body`, or `None` when the
/// body no longer matches.
fn highlighted(p: &Phase<'_>, expr: &str, body: i64) -> Result<Option<String>, ArchiveError> {
    p.sql(|c| {
        c.prepare(SQL_HIGHLIGHT)?
            .query_row(
                rusqlite::params![
                    highlight::MARK_OPEN.to_string(),
                    highlight::MARK_CLOSE.to_string(),
                    expr,
                    body
                ],
                |r| Ok(text_ref(r.get_ref(0)?).map(|t| t.into_owned())),
            )
            .optional()
            .map(Option::flatten)
    })
}

/// Map an error of a statement that reads the FTS index: anything but an
/// interrupt, a cancel or a budget is `SearchUnavailable`.
pub(crate) fn fts_err(e: ArchiveError) -> ArchiveError {
    match e {
        ArchiveError::Sqlite { .. } => ArchiveError::SearchUnavailable,
        other => other,
    }
}

impl ArchiveReader {
    /// A handle that interrupts this reader's running statement from
    /// another thread (§3.2, §7.1). Take it once, when the reader is
    /// opened, and keep it outside the reader's mutex.
    pub fn search_interrupt(&self) -> SearchInterrupt {
        SearchInterrupt::of(self.conn())
    }

    /// Search the archive (§4): words through the full-text index, or
    /// exact text (§4.2), newest first, with the request's filters. A
    /// cancel (the token, or [`SearchInterrupt`]) is `Cancelled`; a words
    /// search over its budget or its 5 s deadline is `QueryTooBroad`; an
    /// FTS error is `SearchUnavailable`; a refused query is
    /// `InvalidQuery`. The exact-text scan pushes `progress` after each
    /// batch and reports what it covered instead of failing when it stops
    /// early. One snapshot; nothing is written or remembered.
    pub fn search(
        &self,
        q: &SearchRequest,
        cancel: &CancelToken,
        progress: &mut dyn FnMut(ScanProgress),
    ) -> Result<SearchResultSet, ArchiveError> {
        self.require_current()?;
        let parsed = q.parse()?;
        let ranges = key_ranges(&q.filters)?;
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let conn = self.conn();
        conn.snapshot(|| match &parsed {
            Parsed::Words { expr, prefix } => {
                let (plan, matches, capped) =
                    words::collect(conn, q, expr, *prefix, &ranges, cancel)?;
                let (entries, dated) = order(matches);
                Ok(SearchResultSet {
                    ran: SearchRan::Words,
                    plan: Some(plan),
                    entries,
                    dated,
                    capped,
                    newest_kept: plan == WordsPlan::Scoped,
                    coverage: None,
                })
            }
            Parsed::Exact(needle) => {
                let (matches, coverage) = scan::run(conn, q, needle, &ranges, cancel, progress)?;
                let (entries, dated) = order(matches);
                Ok(SearchResultSet {
                    ran: SearchRan::Exact,
                    plan: None,
                    entries,
                    dated,
                    capped: coverage.stop == Some(ScanStop::MatchCap),
                    newest_kept: true,
                    coverage: Some(coverage),
                })
            }
        })
    }

    /// The hits `range` of `set` (at most [`HITS_PER_CALL`]; a range past
    /// the end is cut), each with its conversation, date, direction and a
    /// snippet: highlighted and masked (§4.5). `q` must be the request
    /// that made `set`. Facts are read under `Assembly` (2 s); the
    /// `highlight()` calls of the page under `Hits` (2 s), and when that
    /// runs out the remaining hits are sent unhighlighted.
    pub fn search_hits(
        &self,
        set: &SearchResultSet,
        range: Range<usize>,
        q: &SearchRequest,
    ) -> Result<Vec<SearchHit>, ArchiveError> {
        self.require_current()?;
        let parsed = q.parse()?;
        if parsed.ran() != set.ran {
            return Err(ArchiveError::Internal("search request changed"));
        }
        let end = range.end.min(set.entries.len());
        let start = range.start.min(end);
        if end - start > HITS_PER_CALL {
            return Err(ArchiveError::Internal("search hits page"));
        }
        let entries: Vec<SetEntry> = set.entries.get(start..end).unwrap_or(&[]).to_vec();
        let conn = self.conn();
        conn.snapshot(|| {
            let deadline = Instant::now() + ViewBudget::Assembly.window();
            let facts: Vec<HitFacts> = conn.viewing(ViewBudget::Assembly, None, deadline, |p| {
                let mut out = Vec::with_capacity(entries.len());
                for e in &entries {
                    p.checkpoint()?;
                    if let Some(f) = p.sql(|c| hit_facts(c, *e, q.include_earlier))? {
                        out.push(f);
                    }
                }
                Ok(out)
            })?;
            let marks: Vec<Result<Vec<(usize, usize)>, NoHighlight>> = match &parsed {
                Parsed::Exact(needle) => facts
                    .iter()
                    .map(|f| {
                        let found = f
                            .text
                            .as_deref()
                            .map(|t| highlight::find_all(t, needle))
                            .unwrap_or_default();
                        if found.is_empty() {
                            Err(NoHighlight::NotFound)
                        } else {
                            Ok(found)
                        }
                    })
                    .collect(),
                Parsed::Words { expr, .. } => {
                    let deadline =
                        Instant::now() + crate::fault::hits_window(ViewBudget::Hits.window());
                    conn.viewing(ViewBudget::Hits, None, deadline, |p| {
                        let mut out = Vec::with_capacity(facts.len());
                        let mut stopped = false;
                        for f in &facts {
                            let Some(text) = f.text.as_deref() else {
                                out.push(Err(NoHighlight::NotFound));
                                continue;
                            };
                            if !highlight::highlightable(text) {
                                out.push(Err(NoHighlight::MarkerInText));
                                continue;
                            }
                            if stopped || p.checkpoint().is_err() {
                                stopped = true;
                                out.push(Err(NoHighlight::Budget));
                                continue;
                            }
                            match highlighted(p, expr, f.entry.body.get()) {
                                Ok(Some(marked)) => out.push(
                                    highlight::parse_marked(&marked, text)
                                        .ok_or(NoHighlight::Mismatch)
                                        .and_then(|r| {
                                            if r.is_empty() {
                                                Err(NoHighlight::NotFound)
                                            } else {
                                                Ok(r)
                                            }
                                        }),
                                ),
                                Ok(None) => out.push(Err(NoHighlight::NotFound)),
                                Err(ArchiveError::QueryTooBroad | ArchiveError::Cancelled) => {
                                    stopped = true;
                                    out.push(Err(NoHighlight::Budget));
                                }
                                Err(e) => return Err(fts_err(e)),
                            }
                        }
                        Ok(out)
                    })
                    .or_else(|e| match e {
                        // The budget ran out outside a statement: every
                        // hit unhighlighted.
                        ArchiveError::QueryTooBroad => {
                            Ok(facts.iter().map(|_| Err(NoHighlight::Budget)).collect())
                        }
                        other => Err(other),
                    })?
                }
            };
            Ok(facts
                .into_iter()
                .zip(marks)
                .map(|(f, m)| {
                    let text = f.text.as_deref().map(|t| {
                        let (hits, state) = match &m {
                            Ok(r) => (r.as_slice(), Highlight::Shown),
                            Err(why) => (&[][..], Highlight::NotShown(*why)),
                        };
                        highlight::build(t, hits, &f.masks, f.mask_all, f.lossy, state)
                    });
                    SearchHit {
                        message: f.entry.message,
                        body: f.entry.body,
                        chat: f.chat.map(ChatId::new),
                        other_chats: f.others,
                        utc_ns: f.utc_ns,
                        direction: f.direction,
                        item: f.item,
                        association: f.association,
                        version: f.version,
                        text,
                    }
                })
                .collect())
        })
    }

    /// Messages (current bodies of message items) with no searchable text
    /// (§4.2): "N messages have no searchable text". Neither mode can find
    /// them. Computed once per session; a full pass over `message` under
    /// `ViewBudget::Search` and its 5 s deadline.
    pub fn unsearchable_count(&self) -> Result<u64, ArchiveError> {
        self.require_current()?;
        let conn = self.conn();
        let deadline = Instant::now() + ViewBudget::Search.window();
        conn.snapshot(|| {
            conn.viewing(ViewBudget::Search, None, deadline, |p| {
                let n: i64 = p.sql(|c| c.query_row(SQL_UNSEARCHABLE, [], |r| r.get(0)))?;
                Ok(u64::try_from(n).unwrap_or(0))
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(message: i64, body: i64, key: Vec<u8>, current: bool) -> Match {
        Match {
            message,
            body,
            key,
            current,
        }
    }

    #[test]
    fn order_is_newest_first_one_per_message_undated_last() {
        let k = |ns: i64| sortkey::dated(ns).to_vec();
        let u = |r: u8| vec![sortkey::UNDATED_PREFIX, r];
        let (entries, dated) = order(vec![
            m(1, 10, k(5), false),
            m(1, 11, k(5), true),
            m(2, 20, k(9), false),
            m(2, 21, k(9), false),
            m(3, 30, u(1), true),
            m(4, 40, u(7), true),
            m(5, 50, k(9), true),
            m(6, 60, k(-3), true),
        ]);
        let got: Vec<(i64, i64, bool)> = entries
            .iter()
            .map(|e| (e.message.get(), e.body.get(), e.current))
            .collect();
        assert_eq!(
            got,
            vec![
                (5, 50, true),
                (2, 21, false),
                (1, 11, true),
                (6, 60, true),
                (4, 40, true),
                (3, 30, true),
            ]
        );
        assert_eq!(dated, 4);
        assert!(entries[..4].iter().all(|e| e.dated));
        assert!(entries[4..].iter().all(|e| !e.dated));
    }

    #[test]
    fn key_ranges_bound_dates_and_leave_undated_out_of_date_filters() {
        let all = key_ranges(&SearchFilters::default()).expect("ranges");
        assert_eq!(all, vec![(vec![], vec![1]), (vec![1], vec![2])]);
        let f = SearchFilters {
            from_utc_ns: Some(10),
            ..SearchFilters::default()
        };
        assert_eq!(
            key_ranges(&f).expect("ranges"),
            vec![(sortkey::dated(10).to_vec(), vec![1])]
        );
        let f = SearchFilters {
            until_utc_ns: Some(10),
            ..SearchFilters::default()
        };
        assert_eq!(
            key_ranges(&f).expect("ranges"),
            vec![(vec![], sortkey::dated(10).to_vec())]
        );
        let f = SearchFilters {
            from_utc_ns: Some(10),
            until_utc_ns: Some(10),
            ..SearchFilters::default()
        };
        assert!(key_ranges(&f).is_err());
    }

    #[test]
    fn requests_resolve_their_mode_and_debug_prints_no_query() {
        let q = SearchRequest::new("SYNTH-SECRET words");
        assert_eq!(q.resolved_mode().expect("mode"), SearchRan::Words);
        assert!(!format!("{q:?}").contains("SYNTH"));
        assert_eq!(
            SearchRequest::new("東京").resolved_mode().expect("mode"),
            SearchRan::Exact
        );
        let mut q = SearchRequest::new("plain");
        q.mode = SearchMode::Exact;
        assert_eq!(q.resolved_mode().expect("mode"), SearchRan::Exact);
        assert!(matches!(
            SearchRequest::new("\"\"").resolved_mode(),
            Err(ArchiveError::InvalidQuery(_))
        ));
    }
}
