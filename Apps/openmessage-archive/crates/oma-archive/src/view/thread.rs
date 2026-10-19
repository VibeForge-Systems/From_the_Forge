//! Thread pages and the reply thread (M5 spec §3.5).
//!
//! **Keyset paging.** A thread is the chat's timeline index,
//! `chat_message_timeline (chat_id, sort_key, message_id)`, read from an
//! exclusive bound `(sort_key, message_id)` with row-value comparisons, so
//! a page costs the same at any depth. Every read is bounded to one range:
//! the **dated** range (`sort_key < X'01'`) or the **undated** one
//! (`sort_key >= X'01'`), which is reachable only through
//! [`Anchor::Undated`] (D91): an undated row never poses as the newest.
//!
//! **Anchors are message ids**; sort keys never leave the crate. A page
//! returns the keys it served as opaque [`SortKeyHint`]s; the caller (the
//! shell's viewer session) remembers them and passes the anchor's back.
//! Paging continues from the remembered key, so when a higher-ranked
//! import moved the anchor (its date changed), no row between the old and
//! the new position is skipped, and [`ThreadPage::anchor_moved`] tells the
//! UI to reload around the anchor.
//!
//! **Folding** (§3.5 step 3, D93). Only reactions fold: `assoc_present =
//! 1`, `assoc_kind` 10–17 or 20–27, and a target that resolves through
//! `message_guid` (`key_kind = 1`) to a timeline item of the **same** chat.
//! A folded row is not an item; every other row is. The page stops at
//! `want` visible items, at `8 × want` rows scanned, or at the `Scan`
//! budget; a tripped scan that produced an item returns `partial`.
//!
//! **Assembly** (§3.11) runs as a second phase under its own budget, in the
//! same read transaction: nearest items first, so a trip keeps the items
//! closest to the anchor and marks the page `extras_incomplete`. The page
//! also stops before a running upper bound of its serialised size passes
//! [`PAGE_BYTES`](super::assemble::PAGE_BYTES).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension};

use super::assemble::{self, AssembledMessage, Caps, Ctx, Extras, Reveal, PAGE_BYTES};
use super::conversations::SQL_COUNT_UNDATED;
use super::{folds_into, CappedCount, ChatId, HandleId, MessageId, Participant};
use crate::conn::{Phase, ViewBudget};
use crate::error::ArchiveError;
use crate::reader::ArchiveReader;
use crate::sortkey;

/// The most visible items one page holds.
pub const PAGE_ITEMS: u16 = 100;
/// The most replies [`ArchiveReader::reply_thread`] lists.
pub const REPLY_THREAD_LIMIT: usize = 500;
/// The most handles one page carries.
pub const PAGE_HANDLES: usize = 500;
/// Rows scanned per visible item asked for, at most.
const SCAN_FACTOR: usize = 8;
/// The cap of the undated count a page offers.
const UNDATED_CAP: u64 = 10_000;

/// Where a page starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Anchor {
    /// The newest **dated** items (the direction is ignored).
    Newest,
    /// The oldest dated items (the direction is ignored).
    Oldest,
    /// The start of the undated section, in source order (the direction
    /// is ignored).
    Undated,
    /// A message of this chat, with the direction; `include` puts the
    /// anchor itself on an `Older` or `Newer` page.
    Message { id: MessageId, include: bool },
}

/// Which way a page reads from a message anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Older,
    Newer,
    /// Half the items on each side, the anchor in the middle.
    Around,
}

/// A sort key the crate served, remembered by the caller and passed back
/// with its anchor. Opaque: it never crosses IPC, and `Debug` prints
/// nothing of it.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SortKeyHint(Vec<u8>);

impl std::fmt::Debug for SortKeyHint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SortKeyHint(..)")
    }
}

/// Which part of the timeline a page is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PageRange {
    /// The dated range: `has_newer` is false at the dated end, and
    /// [`ThreadPage::undated`] offers the undated section.
    Dated,
    /// The undated section, in source order: `has_older` is false at its
    /// start, and the UI offers the dated end ([`Anchor::Newest`]).
    Undated,
    /// A reply thread: the originator, then its replies across
    /// conversations.
    Replies,
}

/// Why a page focuses an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FocusReason {
    /// The anchor itself.
    Anchor,
    /// The anchor is a folded reaction: its target.
    ReactionTarget,
    /// The anchor is not visible and its target is not either: the next
    /// visible item.
    NextVisible,
}

/// The item a page centres on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Focus {
    pub id: MessageId,
    pub reason: FocusReason,
}

/// One page of a thread.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ThreadPage {
    /// Visible items, oldest first.
    pub items: Vec<AssembledMessage>,
    /// Every handle the items refer to, once.
    pub handles: Vec<Participant>,
    /// More than [`PAGE_HANDLES`] handles were referred to.
    pub handles_capped: bool,
    pub range: PageRange,
    /// The oldest row scanned: a valid anchor even when folded.
    pub older_anchor: Option<MessageId>,
    /// The newest row scanned.
    pub newer_anchor: Option<MessageId>,
    pub has_older: bool,
    pub has_newer: bool,
    /// The chat's undated rows, reactions included, as the conversation
    /// list counts them (dated pages only): it offers the undated section.
    pub undated: Option<CappedCount>,
    /// The scan stopped on its budget or deadline after at least one item.
    pub partial: bool,
    /// The assembly stopped on its budget: some items carry no extras
    /// ("Some reactions, attachments or labels could not be loaded").
    pub extras_incomplete: bool,
    /// The anchor's sort key changed since it was served: reload around
    /// it.
    pub anchor_moved: bool,
    pub focus: Option<Focus>,
    /// The page stopped before its serialised-size bound.
    pub cut_for_size: bool,
    served: Vec<(MessageId, SortKeyHint)>,
}

impl ThreadPage {
    /// The keys served for the page's anchors and focus, to remember.
    pub fn served_keys(&self) -> &[(MessageId, SortKeyHint)] {
        &self.served
    }
}

// ---------------------------------------------------------------------------
// Statements (each pinned in tests/query_plans.rs)
// ---------------------------------------------------------------------------

const SQL_KEY: &str = "SELECT sort_key FROM chat_message WHERE chat_id = ?1 AND message_id = ?2";
pub(crate) const SQL_DATED_OLDER: &str = "SELECT message_id, sort_key FROM chat_message \
     WHERE chat_id = ?1 AND (sort_key, message_id) < (?2, ?3) AND sort_key < X'01' \
     ORDER BY sort_key DESC, message_id DESC LIMIT ?4";
pub(crate) const SQL_DATED_NEWER: &str = "SELECT message_id, sort_key FROM chat_message \
     WHERE chat_id = ?1 AND (sort_key, message_id) > (?2, ?3) AND sort_key < X'01' \
     ORDER BY sort_key, message_id LIMIT ?4";
const SQL_UNDATED_OLDER: &str = "SELECT message_id, sort_key FROM chat_message \
     WHERE chat_id = ?1 AND (sort_key, message_id) < (?2, ?3) AND sort_key >= X'01' \
     ORDER BY sort_key DESC, message_id DESC LIMIT ?4";
const SQL_UNDATED_NEWER: &str = "SELECT message_id, sort_key FROM chat_message \
     WHERE chat_id = ?1 AND (sort_key, message_id) > (?2, ?3) AND sort_key >= X'01' \
     ORDER BY sort_key, message_id LIMIT ?4";
const SQL_ORIGINATOR: &str = "SELECT key_kind, guid, sort_key FROM message WHERE id = ?1";
const SQL_REPLIES: &str = "SELECT id, sort_key FROM message \
     WHERE thread_originator_guid = ?1 AND assoc_present = 0 AND item_kind = 1 LIMIT ?2";

#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    [
        ("viewer timeline key", SQL_KEY),
        ("viewer thread dated older", SQL_DATED_OLDER),
        ("viewer thread dated newer", SQL_DATED_NEWER),
        ("viewer thread undated older", SQL_UNDATED_OLDER),
        ("viewer thread undated newer", SQL_UNDATED_NEWER),
        ("viewer reply originator", SQL_ORIGINATOR),
        ("viewer replies", SQL_REPLIES),
    ]
    .into_iter()
    .map(|(n, s)| (n, s.to_owned()))
    .collect()
}

// ---------------------------------------------------------------------------
// The scan phase
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Way {
    Older,
    Newer,
}

fn sql_of(range: PageRange, way: Way) -> &'static str {
    match (range, way) {
        (PageRange::Undated, Way::Older) => SQL_UNDATED_OLDER,
        (PageRange::Undated, Way::Newer) => SQL_UNDATED_NEWER,
        (_, Way::Older) => SQL_DATED_OLDER,
        (_, Way::Newer) => SQL_DATED_NEWER,
    }
}

fn range_of(key: &[u8]) -> PageRange {
    if sortkey::is_dated(key) {
        PageRange::Dated
    } else {
        PageRange::Undated
    }
}

fn bytes_of(v: ValueRef<'_>) -> Vec<u8> {
    match v {
        ValueRef::Text(b) | ValueRef::Blob(b) => b.to_vec(),
        _ => Vec::new(),
    }
}

/// One row read from the timeline.
#[derive(Clone)]
struct Row {
    id: i64,
    key: Vec<u8>,
    /// The target this row folds into, if it does.
    folded: Option<i64>,
}

/// An exclusive keyset bound.
#[derive(Clone)]
pub(crate) struct Bound {
    pub(crate) key: Vec<u8>,
    pub(crate) id: i64,
}

pub(crate) fn batch(
    c: &Connection,
    sql: &str,
    chat: i64,
    from: &Bound,
    limit: usize,
) -> rusqlite::Result<Vec<(i64, Vec<u8>)>> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut st = c.prepare(sql)?;
    let rows = st.query_map(
        rusqlite::params![chat, from.key.as_slice(), from.id, limit],
        |r| Ok((r.get(0)?, bytes_of(r.get_ref(1)?))),
    )?;
    rows.collect()
}

fn key_of(c: &Connection, chat: i64, id: i64) -> rusqlite::Result<Option<Vec<u8>>> {
    c.prepare(SQL_KEY)?
        .query_row([chat, id], |r| Ok(bytes_of(r.get_ref(0)?)))
        .optional()
}

/// `true` for the outcome a budget or deadline trip maps to.
fn too_broad(e: &ArchiveError) -> bool {
    matches!(e, ArchiveError::QueryTooBroad)
}

/// What one direction of a scan read.
struct Side {
    /// In scan order (nearest the bound first).
    rows: Vec<Row>,
    /// More rows lie beyond the last one read.
    more: bool,
    /// The budget or deadline stopped the scan.
    tripped: bool,
}

/// Whether any row lies beyond `from` in `way` (a trip counts as yes).
fn probe(
    p: &Phase<'_>,
    chat: i64,
    range: PageRange,
    way: Way,
    from: &Bound,
) -> Result<(bool, bool), ArchiveError> {
    match p.sql(|c| batch(c, sql_of(range, way), chat, from, 1)) {
        Ok(v) => Ok((!v.is_empty(), false)),
        Err(e) if too_broad(&e) => Ok((true, true)),
        Err(e) => Err(e),
    }
}

/// Read rows beyond the exclusive bound `from` in `way`, folding each,
/// until `want` visible rows, `max` rows, the end of the range, or the
/// budget.
fn scan(
    p: &Phase<'_>,
    chat: i64,
    range: PageRange,
    way: Way,
    from: &Bound,
    want: usize,
    max: usize,
) -> Result<Side, ArchiveError> {
    let mut side = Side {
        rows: Vec::new(),
        more: false,
        tripped: false,
    };
    if want == 0 {
        let (more, tripped) = probe(p, chat, range, way, from)?;
        side.more = more;
        side.tripped = tripped;
        return Ok(side);
    }
    let sql = sql_of(range, way);
    let limit = want.saturating_mul(2).max(1);
    let mut cursor = from.clone();
    let mut visible = 0usize;
    loop {
        let got = match p.sql(|c| batch(c, sql, chat, &cursor, limit)) {
            Ok(v) => v,
            Err(e) if too_broad(&e) => {
                side.tripped = true;
                side.more = true;
                return Ok(side);
            }
            Err(e) => return Err(e),
        };
        let n = got.len();
        for (i, (id, key)) in got.into_iter().enumerate() {
            let folded = match p
                .checkpoint()
                .and_then(|()| p.sql(|c| folds_into(c, chat, id)))
            {
                Ok(f) => f,
                Err(e) if too_broad(&e) => {
                    side.tripped = true;
                    side.more = true;
                    return Ok(side);
                }
                Err(e) => return Err(e),
            };
            visible += usize::from(folded.is_none());
            cursor = Bound {
                key: key.clone(),
                id,
            };
            side.rows.push(Row { id, key, folded });
            if visible >= want || side.rows.len() >= max {
                side.more = if i + 1 < n {
                    true
                } else if n == limit {
                    let (more, tripped) = probe(p, chat, range, way, &cursor)?;
                    side.tripped = tripped;
                    more
                } else {
                    false
                };
                return Ok(side);
            }
        }
        if n < limit {
            return Ok(side);
        }
    }
}

/// Where a page's nearest item is, for keeping the nearest items when the
/// assembly or the size bound cuts the page.
#[derive(Clone, Copy)]
enum Centre {
    /// The newest end (an `Older` page).
    NewerEnd,
    /// The oldest end (a `Newer` page, a reply thread).
    OlderEnd,
    /// This visible index (an `Around` page).
    At(usize),
}

/// The scan's result, before assembly.
struct Scanned {
    range: PageRange,
    /// Every row scanned, oldest first.
    rows: Vec<Row>,
    centre: Centre,
    focus: Option<Focus>,
    has_older: bool,
    has_newer: bool,
    partial: bool,
    anchor_moved: bool,
    undated: Option<CappedCount>,
}

fn undated_count(p: &Phase<'_>, chat: i64) -> Option<CappedCount> {
    let limit = CappedCount::limit(UNDATED_CAP);
    p.sql(|c| {
        c.prepare(SQL_COUNT_UNDATED)?
            .query_row(rusqlite::params![chat, limit], |r| r.get::<_, i64>(0))
    })
    .ok()
    .map(|n| CappedCount::of(n, UNDATED_CAP))
}

/// A page read one way from the exclusive bound `from`.
fn one_side(
    p: &Phase<'_>,
    chat: i64,
    range: PageRange,
    way: Way,
    from: Bound,
    want: usize,
    anchor_moved: bool,
) -> Result<Scanned, ArchiveError> {
    let side = scan(
        p,
        chat,
        range,
        way,
        &from,
        want,
        want.saturating_mul(SCAN_FACTOR),
    )?;
    // Whether anything lies on the other side of the bound (the bound's
    // own position included).
    let back = match way {
        Way::Older => Bound {
            key: from.key.clone(),
            id: from.id.saturating_sub(1),
        },
        Way::Newer => Bound {
            key: from.key.clone(),
            id: from.id.saturating_add(1),
        },
    };
    let other_way = match way {
        Way::Older => Way::Newer,
        Way::Newer => Way::Older,
    };
    let (other, other_tripped) = if side.tripped {
        (true, true)
    } else {
        probe(p, chat, range, other_way, &back)?
    };
    let visible = side.rows.iter().any(|r| r.folded.is_none());
    if side.tripped && !visible {
        return Err(ArchiveError::QueryTooBroad);
    }
    let mut rows = side.rows;
    let (has_older, has_newer, centre) = match way {
        Way::Older => {
            rows.reverse();
            (side.more, other, Centre::NewerEnd)
        }
        Way::Newer => (other, side.more, Centre::OlderEnd),
    };
    let tripped = side.tripped || other_tripped;
    Ok(Scanned {
        range,
        rows,
        centre,
        focus: None,
        has_older,
        has_newer,
        partial: tripped,
        anchor_moved,
        undated: if range == PageRange::Dated && !tripped {
            undated_count(p, chat)
        } else {
            None
        },
    })
}

/// A page around `id` (current key `key`).
fn around(
    p: &Phase<'_>,
    chat: i64,
    id: i64,
    key: Vec<u8>,
    want: usize,
    anchor_moved: bool,
) -> Result<Scanned, ArchiveError> {
    let mut centre = Row {
        id,
        key,
        folded: None,
    };
    let mut reason = FocusReason::Anchor;
    if let Some(target) = p.sql(|c| folds_into(c, chat, id))? {
        if let Some(tk) = p.sql(|c| key_of(c, chat, target))? {
            centre = Row {
                id: target,
                key: tk,
                folded: p.sql(|c| folds_into(c, chat, target))?,
            };
            reason = FocusReason::ReactionTarget;
        }
        if centre.folded.is_some() || centre.id == id {
            // The target is not visible either: the next visible item.
            let from = Bound {
                key: centre.key.clone(),
                id: centre.id,
            };
            let next = scan(
                p,
                chat,
                range_of(&centre.key),
                Way::Newer,
                &from,
                1,
                want.saturating_mul(SCAN_FACTOR),
            )?;
            match next.rows.into_iter().find(|r| r.folded.is_none()) {
                Some(v) => {
                    centre = v;
                    reason = FocusReason::NextVisible;
                }
                None => {
                    let range = range_of(&from.key);
                    return one_side(p, chat, range, Way::Older, from, want, anchor_moved);
                }
            }
        }
    }
    let range = range_of(&centre.key);
    let bound = Bound {
        key: centre.key.clone(),
        id: centre.id,
    };
    let want_older = want.saturating_sub(1) / 2;
    let want_newer = want.saturating_sub(1).saturating_sub(want_older);
    let max = want.saturating_mul(SCAN_FACTOR);
    let older = scan(p, chat, range, Way::Older, &bound, want_older, max)?;
    let newer = scan(p, chat, range, Way::Newer, &bound, want_newer, max)?;
    let tripped = older.tripped || newer.tripped;
    let mut rows: Vec<Row> = older.rows.into_iter().rev().collect();
    let at = rows.iter().filter(|r| r.folded.is_none()).count();
    let focus = Focus {
        id: MessageId::new(centre.id),
        reason,
    };
    rows.push(centre);
    rows.extend(newer.rows);
    Ok(Scanned {
        range,
        rows,
        centre: Centre::At(at),
        focus: Some(focus),
        has_older: older.more,
        has_newer: newer.more,
        partial: tripped,
        anchor_moved,
        undated: if range == PageRange::Dated && !tripped {
            undated_count(p, chat)
        } else {
            None
        },
    })
}

fn scan_page(
    p: &Phase<'_>,
    chat: i64,
    anchor: Anchor,
    dir: Direction,
    want: usize,
    hint: Option<&SortKeyHint>,
) -> Result<Scanned, ArchiveError> {
    let dated_end = || Bound {
        key: sortkey::DATED_END.to_vec(),
        id: i64::MIN,
    };
    match anchor {
        Anchor::Newest => one_side(
            p,
            chat,
            PageRange::Dated,
            Way::Older,
            dated_end(),
            want,
            false,
        ),
        Anchor::Oldest => one_side(
            p,
            chat,
            PageRange::Dated,
            Way::Newer,
            Bound {
                key: Vec::new(),
                id: i64::MIN,
            },
            want,
            false,
        ),
        Anchor::Undated => one_side(
            p,
            chat,
            PageRange::Undated,
            Way::Newer,
            dated_end(),
            want,
            false,
        ),
        Anchor::Message { id, include } => {
            let id = id.get();
            let current = p
                .sql(|c| key_of(c, chat, id))?
                .ok_or(ArchiveError::AnchorNotFound)?;
            let moved = hint.is_some_and(|h| h.0 != current);
            let key = match hint {
                Some(h) if dir != Direction::Around => h.0.clone(),
                _ => current.clone(),
            };
            let range = range_of(&key);
            match dir {
                Direction::Older => {
                    let id = if include { id.saturating_add(1) } else { id };
                    one_side(p, chat, range, Way::Older, Bound { key, id }, want, moved)
                }
                Direction::Newer => {
                    let id = if include { id.saturating_sub(1) } else { id };
                    one_side(p, chat, range, Way::Newer, Bound { key, id }, want, moved)
                }
                Direction::Around => around(p, chat, id, current, want, moved),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The assembly phase
// ---------------------------------------------------------------------------

/// Visible indices nearest first.
fn priority(n: usize, centre: Centre) -> Vec<usize> {
    match centre {
        Centre::NewerEnd => (0..n).rev().collect(),
        Centre::OlderEnd => (0..n).collect(),
        Centre::At(c) => {
            let c = c.min(n.saturating_sub(1));
            let mut out = Vec::with_capacity(n);
            if n > 0 {
                out.push(c);
            }
            let mut d = 1usize;
            while out.len() < n {
                if let Some(i) = c.checked_sub(d) {
                    out.push(i);
                }
                if c + d < n {
                    out.push(c + d);
                }
                d += 1;
            }
            out
        }
    }
}

fn assemble_page(
    p: &Phase<'_>,
    chat: Option<i64>,
    s: Scanned,
    reveal: Reveal,
) -> Result<ThreadPage, ArchiveError> {
    let caps = Caps::PAGE;
    let visible: Vec<&Row> = s.rows.iter().filter(|r| r.folded.is_none()).collect();
    let prio = priority(visible.len(), s.centre);
    let ctx = p.sql(Ctx::load)?;
    let mut built: Vec<Option<(AssembledMessage, Extras)>> =
        (0..visible.len()).map(|_| None).collect();
    let mut incomplete = false;
    let mut kept = 0usize;
    for &i in &prio {
        let Some(row) = visible.get(i) else { continue };
        let got = p
            .checkpoint()
            .and_then(|()| p.sql(|c| assemble::base(c, &ctx, chat, row.id, &caps, reveal)));
        match got {
            Ok(m) => {
                if let Some(slot) = built.get_mut(i) {
                    *slot = m;
                }
            }
            Err(e) if too_broad(&e) => {
                incomplete = true;
                break;
            }
            Err(e) => return Err(e),
        }
        kept += 1;
    }
    if kept == 0 && !visible.is_empty() {
        return Err(ArchiveError::QueryTooBroad);
    }
    let order: Vec<usize> = prio.iter().copied().take(kept).collect();
    if !incomplete {
        for &i in &order {
            let Some(Some((m, x))) = built.get_mut(i) else {
                continue;
            };
            let got = p
                .checkpoint()
                .and_then(|()| p.sql(|c| assemble::extras(c, chat, m, x, &caps)));
            match got {
                Ok(()) => {}
                Err(e) if too_broad(&e) => {
                    incomplete = true;
                    break;
                }
                Err(e) => return Err(e),
            }
        }
    }
    // Handles and the size bound, nearest first.
    let mut handles: BTreeMap<HandleId, Participant> = BTreeMap::new();
    let mut handles_capped = false;
    let mut size = 0usize;
    let mut included: BTreeSet<usize> = BTreeSet::new();
    let mut cut_for_size = false;
    for &i in &order {
        let Some(Some((m, _))) = built.get(i) else {
            included.insert(i);
            continue;
        };
        let mut add = assemble::size_bound(m);
        let mut fresh: Vec<Participant> = Vec::new();
        for h in assemble::handles_of(m) {
            if handles.contains_key(&h) || fresh.iter().any(|f| f.handle == h) {
                continue;
            }
            if handles.len() + fresh.len() >= PAGE_HANDLES {
                handles_capped = true;
                continue;
            }
            if incomplete {
                continue;
            }
            match p.sql(|c| assemble::handle(c, h.get())) {
                Ok(Some(ph)) => {
                    add += assemble::handle_bound(&ph);
                    fresh.push(ph);
                }
                Ok(None) => {}
                Err(e) if too_broad(&e) => incomplete = true,
                Err(e) => return Err(e),
            }
        }
        if !included.is_empty() && size.saturating_add(add) > PAGE_BYTES {
            cut_for_size = true;
            break;
        }
        size = size.saturating_add(add);
        for ph in fresh {
            handles.insert(ph.handle, ph);
        }
        included.insert(i);
    }

    // The kept items are contiguous around the centre.
    let lo = included.first().copied();
    let hi = included.last().copied();
    let mut has_older = s.has_older;
    let mut has_newer = s.has_newer;
    let mut older_anchor = s.rows.first().map(|r| r.id);
    let mut newer_anchor = s.rows.last().map(|r| r.id);
    if let Some(lo) = lo.filter(|&lo| lo > 0) {
        has_older = true;
        older_anchor = visible.get(lo).map(|r| r.id);
    }
    if let Some(hi) = hi.filter(|&hi| hi + 1 < visible.len()) {
        has_newer = true;
        newer_anchor = visible.get(hi).map(|r| r.id);
    }
    let items: Vec<AssembledMessage> = built
        .into_iter()
        .enumerate()
        .filter(|(i, _)| included.contains(i))
        .filter_map(|(_, m)| m.map(|(m, _)| m))
        .collect();
    let focus = s.focus.filter(|f| items.iter().any(|m| m.id == f.id));
    let mut served: Vec<(MessageId, SortKeyHint)> = Vec::new();
    for id in [older_anchor, newer_anchor, focus.map(|f| f.id.get())]
        .into_iter()
        .flatten()
    {
        if served.iter().any(|(m, _)| m.get() == id) {
            continue;
        }
        if let Some(r) = s.rows.iter().find(|r| r.id == id) {
            served.push((MessageId::new(id), SortKeyHint(r.key.clone())));
        }
    }
    Ok(ThreadPage {
        items,
        handles: handles.into_values().collect(),
        handles_capped,
        range: s.range,
        older_anchor: older_anchor.map(MessageId::new),
        newer_anchor: newer_anchor.map(MessageId::new),
        has_older,
        has_newer,
        undated: s.undated,
        partial: s.partial,
        extras_incomplete: incomplete,
        anchor_moved: s.anchor_moved,
        focus,
        cut_for_size,
        served,
    })
}

impl ArchiveReader {
    /// One page of chat `chat`'s timeline (M5 spec §3.5): at most `want`
    /// visible items (clamped to 1..=[`PAGE_ITEMS`]), oldest first, read
    /// from `anchor` in `dir`. Pass the anchor's remembered key as `hint`
    /// (from an earlier page's [`ThreadPage::served_keys`]); paging then
    /// continues from it, and `anchor_moved` says the anchor's key changed.
    ///
    /// One snapshot, in two phases: `Scan`, then `Assembly`, under a 2 s
    /// deadline. An anchor that is not in this chat is `AnchorNotFound`;
    /// a scan that trips before any item is `QueryTooBroad`, never
    /// `Damaged`.
    pub fn thread_page(
        &self,
        chat: ChatId,
        anchor: Anchor,
        dir: Direction,
        want: u16,
        hint: Option<&SortKeyHint>,
        reveal: Reveal,
    ) -> Result<ThreadPage, ArchiveError> {
        self.require_current()?;
        let want = usize::from(want.clamp(1, PAGE_ITEMS));
        let conn = self.conn();
        let deadline = Instant::now() + ViewBudget::Scan.window();
        conn.snapshot(|| {
            let scanned = conn.viewing(ViewBudget::Scan, None, deadline, |p| {
                scan_page(p, chat.get(), anchor, dir, want, hint)
            })?;
            conn.viewing(ViewBudget::Assembly, None, deadline, |p| {
                assemble_page(p, Some(chat.get()), scanned, reveal)
            })
        })
    }

    /// The reply thread of `originator` (§3.6): the originator, then up to
    /// [`REPLY_THREAD_LIMIT`] replies across conversations (rows whose
    /// `thread_originator_guid` is its GUID, `assoc_present = 0` and
    /// `item_kind = 1`), in timeline order. An unkeyed originator has no
    /// replies. `has_newer` says there were more.
    pub fn reply_thread(
        &self,
        originator: MessageId,
        reveal: Reveal,
    ) -> Result<ThreadPage, ArchiveError> {
        self.require_current()?;
        let conn = self.conn();
        let deadline = Instant::now() + ViewBudget::Scan.window();
        conn.snapshot(|| {
            let scanned = conn.viewing(ViewBudget::Scan, None, deadline, |p| {
                let row: Option<(i64, Option<Vec<u8>>, Vec<u8>)> = p.sql(|c| {
                    c.prepare(SQL_ORIGINATOR)?
                        .query_row([originator.get()], |r| {
                            let guid = match r.get_ref(1)? {
                                ValueRef::Text(b) | ValueRef::Blob(b) => Some(b.to_vec()),
                                _ => None,
                            };
                            Ok((r.get(0)?, guid, bytes_of(r.get_ref(2)?)))
                        })
                        .optional()
                })?;
                let (key_kind, guid, key) = row.ok_or(ArchiveError::AnchorNotFound)?;
                let mut replies: Vec<(Vec<u8>, i64)> = Vec::new();
                if let Some(guid) = guid.filter(|g| key_kind == 1 && !g.is_empty()) {
                    let limit = i64::try_from(REPLY_THREAD_LIMIT + 1).unwrap_or(i64::MAX);
                    replies = p.sql(|c| {
                        let mut st = c.prepare(SQL_REPLIES)?;
                        let rows = st.query_map(rusqlite::params![guid, limit], |r| {
                            Ok((bytes_of(r.get_ref(1)?), r.get(0)?))
                        })?;
                        rows.collect()
                    })?;
                }
                replies.retain(|(_, id)| *id != originator.get());
                replies.sort();
                let more = replies.len() > REPLY_THREAD_LIMIT;
                replies.truncate(REPLY_THREAD_LIMIT);
                let mut rows = vec![Row {
                    id: originator.get(),
                    key,
                    folded: None,
                }];
                rows.extend(replies.into_iter().map(|(key, id)| Row {
                    id,
                    key,
                    folded: None,
                }));
                Ok(Scanned {
                    range: PageRange::Replies,
                    rows,
                    centre: Centre::OlderEnd,
                    focus: Some(Focus {
                        id: originator,
                        reason: FocusReason::Anchor,
                    }),
                    has_older: false,
                    has_newer: more,
                    partial: false,
                    anchor_moved: false,
                    undated: None,
                })
            })?;
            conn.viewing(ViewBudget::Assembly, None, deadline, |p| {
                assemble_page(p, None, scanned, reveal)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_keeps_the_nearest_items_first() {
        assert_eq!(priority(4, Centre::NewerEnd), vec![3, 2, 1, 0]);
        assert_eq!(priority(3, Centre::OlderEnd), vec![0, 1, 2]);
        assert_eq!(priority(5, Centre::At(1)), vec![1, 0, 2, 3, 4]);
        assert_eq!(priority(5, Centre::At(4)), vec![4, 3, 2, 1, 0]);
        assert_eq!(priority(0, Centre::At(0)), Vec::<usize>::new());
    }

    #[test]
    fn hints_print_nothing_and_ranges_follow_the_key() {
        let h = SortKeyHint(sortkey::dated(5).to_vec());
        assert_eq!(format!("{h:?}"), "SortKeyHint(..)");
        assert_eq!(range_of(&sortkey::dated(5)), PageRange::Dated);
        assert_eq!(
            range_of(&sortkey::undated(&[0u8; 41], 1)),
            PageRange::Undated
        );
    }
}
