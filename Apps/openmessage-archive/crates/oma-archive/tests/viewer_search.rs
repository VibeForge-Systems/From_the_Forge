//! Search (M5 spec §4, §13.4, slice 5c).
//!
//! - **An independent oracle.** For every viewer catalogue entry and
//!   scenario, queries drawn from the stored bodies run in both modes and
//!   under every filter (sender, attachment, events, earlier versions,
//!   dates, each conversation) and must equal an oracle built here from
//!   plain rows: exact text with `str::contains`, words through a plain
//!   FTS5 `MATCH` with the filters applied in Rust. Every hit's facts
//!   (lowest chat, other chats, date, version) match, its snippet is a
//!   piece of the stored text, and no one-time code or withheld earlier
//!   text of the model's `masked` lists ever appears in a snippet unless
//!   `include_earlier` (the reveal for search) asks for earlier text.
//! - **The chat-scoped plan** returns the same set, in the same order, as
//!   the global plan with the chat filter.
//! - **Capping, order, CJK, case and exact-scan bounds** on bulk archives.
//! - **Cancellation, deterministic:** a token set before the call, one set
//!   between two scan batches, and one set at the n-th progress-handler
//!   tick of the FTS collection; plus `SearchInterrupt` from another
//!   thread. Budgets give `QueryTooBroad` (never `Damaged`), or partial
//!   coverage for the scan; a tripped `Hits` budget gives unhighlighted
//!   hits.
//! - **Damage:** a corrupted FTS shadow table gives `SearchUnavailable`
//!   and browsing still works. Searches write nothing.
//! - **The unsearchable count** matches the stored NULL bodies.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use common::bulk::{generate, BulkSpec};
use common::import::read_only;
use common::viewer::{import_order, model_docs};
use common::*;
use oma_archive::hooks::{install, Faults};
use oma_archive::{
    Anchor, ArchiveError, ArchiveId, ArchiveReader, CancelToken, ChatId, Direction, Highlight,
    HitPiece, HitVersion, Library, NoHighlight, QueryProblem, Reveal, ScanProgress, ScanStop,
    SearchMode, SearchRan, SearchRequest, SearchResultSet, SenderFilter, WordsPlan, HITS_PER_CALL,
    SCAN_BATCH, SEARCH_MATCH_CAP,
};
use serde_json::Value;

// ---------------------------------------------------------------------------
// The oracle: plain rows, filters in Rust
// ---------------------------------------------------------------------------

struct Msg {
    key: Vec<u8>,
    current: Option<i64>,
    present: i64,
    item: i64,
    from_me: Option<i64>,
    attachment: bool,
    chats: Vec<i64>,
    /// (body id, display_text)
    bodies: Vec<(i64, Option<String>)>,
    unsent: bool,
}

struct Oracle {
    msgs: BTreeMap<i64, Msg>,
    conn: rusqlite::Connection,
}

fn dated_ns(key: &[u8]) -> Option<i64> {
    match key {
        [0, rest @ ..] if rest.len() == 8 => {
            let b: [u8; 8] = rest.try_into().expect("8");
            Some(i64::from_be_bytes(
                (u64::from_be_bytes(b) ^ (1 << 63)).to_be_bytes(),
            ))
        }
        _ => None,
    }
}

fn oracle(db: &std::path::Path) -> Oracle {
    let c = read_only(db);
    let mut msgs: BTreeMap<i64, Msg> = BTreeMap::new();
    {
        let mut st = c
            .prepare(
                "SELECT id, sort_key, current_body_id, assoc_present, item_kind, is_from_me \
                 FROM message",
            )
            .expect("prepare");
        let mut rows = st.query([]).expect("query");
        while let Some(r) = rows.next().expect("row") {
            msgs.insert(
                r.get(0).expect("id"),
                Msg {
                    key: r.get(1).expect("key"),
                    current: r.get(2).expect("current"),
                    present: r.get(3).expect("present"),
                    item: r.get(4).expect("item"),
                    from_me: r.get(5).expect("from me"),
                    attachment: false,
                    chats: Vec::new(),
                    bodies: Vec::new(),
                    unsent: false,
                },
            );
        }
    }
    for (m, b, t) in c
        .prepare("SELECT message_id, id, display_text FROM message_body")
        .expect("prepare")
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<Result<Vec<(i64, i64, Option<String>)>, _>>()
        .expect("bodies")
    {
        msgs.get_mut(&m).expect("message").bodies.push((b, t));
    }
    for (chat, m) in c
        .prepare("SELECT chat_id, message_id FROM chat_message")
        .expect("prepare")
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("chats")
    {
        msgs.get_mut(&m).expect("message").chats.push(chat);
    }
    for m in c
        .prepare("SELECT DISTINCT message_id FROM message_attachment")
        .expect("prepare")
        .query_map([], |r| r.get::<_, i64>(0))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("attachments")
    {
        msgs.get_mut(&m).expect("message").attachment = true;
    }
    let unsent: BTreeSet<i64> = c
        .prepare("SELECT DISTINCT body_id FROM message_unsent_part")
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("unsent");
    for m in msgs.values_mut() {
        m.chats.sort_unstable();
        m.unsent = m.current.is_some_and(|b| unsent.contains(&b));
    }
    Oracle { msgs, conn: c }
}

/// The entries an oracle expects: (message, body, current), newest first,
/// and how many are dated.
fn expected(
    o: &Oracle,
    q: &SearchRequest,
    matches: &dyn Fn(i64, &str) -> bool,
) -> (Vec<(i64, i64, bool)>, usize) {
    let f = &q.filters;
    let lo = f.from_utc_ns;
    let hi = f.until_utc_ns;
    let mut out: Vec<(Vec<u8>, i64, i64, bool)> = Vec::new();
    for (&id, m) in &o.msgs {
        if !q.include_events && !(m.present == 0 && m.item == 1) {
            continue;
        }
        match f.sender {
            SenderFilter::Me if m.from_me != Some(1) => continue,
            SenderFilter::Others if m.from_me != Some(0) => continue,
            _ => {}
        }
        if f.has_attachment && !m.attachment {
            continue;
        }
        if let Some(chat) = f.chat {
            if !m.chats.contains(&chat.get()) {
                continue;
            }
        }
        if lo.is_some() || hi.is_some() {
            let Some(ns) = dated_ns(&m.key) else { continue };
            if lo.is_some_and(|l| ns < l) || hi.is_some_and(|h| ns >= h) {
                continue;
            }
        }
        let mut found: Vec<(i64, bool)> = m
            .bodies
            .iter()
            .filter(|(b, _)| q.include_earlier || m.current == Some(*b))
            .filter(|(b, t)| t.as_deref().is_some_and(|t| matches(*b, t)))
            .map(|(b, _)| (*b, m.current == Some(*b)))
            .collect();
        found.sort_by_key(|(b, cur)| (!*cur, std::cmp::Reverse(*b)));
        if let Some(&(b, cur)) = found.first() {
            out.push((m.key.clone(), id, b, cur));
        }
    }
    out.sort_by(|a, b| {
        let ad = a.0.first() == Some(&0);
        let bd = b.0.first() == Some(&0);
        bd.cmp(&ad).then(b.0.cmp(&a.0)).then(b.1.cmp(&a.1))
    });
    let dated = out.iter().filter(|e| e.0.first() == Some(&0)).count();
    (
        out.into_iter().map(|(_, m, b, c)| (m, b, c)).collect(),
        dated,
    )
}

/// Bodies a plain FTS5 `MATCH` finds for `expr`.
fn fts_bodies(o: &Oracle, expr: &str) -> BTreeSet<i64> {
    o.conn
        .prepare("SELECT rowid FROM message_fts WHERE message_fts MATCH ?1")
        .expect("prepare")
        .query_map([expr], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

fn got(set: &SearchResultSet) -> (Vec<(i64, i64, bool)>, usize) {
    (
        set.entries
            .iter()
            .map(|e| (e.message.get(), e.body.get(), e.current))
            .collect(),
        set.dated,
    )
}

fn run(r: &ArchiveReader, q: &SearchRequest) -> SearchResultSet {
    r.search(q, &CancelToken::new(), &mut |_| {})
        .unwrap_or_else(|e| panic!("search: {e:?}"))
}

fn pieces_text(t: &oma_archive::HitText) -> String {
    t.pieces
        .iter()
        .filter_map(|p| match p {
            HitPiece::Text { text, .. } => Some(text.text.expose().as_str()),
            _ => None,
        })
        .collect()
}

/// What the hits of a test showed.
#[derive(Default, Debug)]
struct Stats {
    words_marked: usize,
    exact_marked: usize,
    marker_fallbacks: usize,
    hidden: usize,
    masked_pieces: usize,
    earlier: usize,
    unsent: usize,
    other_chats: usize,
}

/// Every hit of `set`, page by page, checked against the oracle; returns
/// the text the snippets showed.
fn check_hits(
    r: &ArchiveReader,
    o: &Oracle,
    set: &SearchResultSet,
    q: &SearchRequest,
    st: &mut Stats,
) -> String {
    let mut shown = String::new();
    let mut start = 0;
    while start < set.entries.len() {
        let end = (start + HITS_PER_CALL).min(set.entries.len());
        let hits = r.search_hits(set, start..end, q).expect("hits");
        assert_eq!(hits.len(), end - start);
        for (h, e) in hits.iter().zip(&set.entries[start..end]) {
            assert_eq!((h.message, h.body), (e.message, e.body));
            let m = &o.msgs[&h.message.get()];
            assert_eq!(h.chat.map(ChatId::get), m.chats.first().copied());
            assert_eq!(h.other_chats.n as usize, m.chats.len().saturating_sub(1));
            assert_eq!(h.utc_ns, dated_ns(&m.key));
            let want = if e.current {
                HitVersion::Current
            } else if m.unsent {
                HitVersion::Unsent
            } else {
                HitVersion::Earlier
            };
            assert_eq!(h.version, want);
            let stored = m
                .bodies
                .iter()
                .find(|(b, _)| *b == h.body.get())
                .and_then(|(_, t)| t.clone())
                .expect("a matched body has text");
            let t = h.text.as_ref().expect("text");
            let joined = pieces_text(t);
            assert!(stored.contains(&joined), "a snippet is a piece of the text");
            if !t.cut_start
                && !t.cut_end
                && !t.pieces.iter().any(|p| matches!(p, HitPiece::Masked(_)))
            {
                assert_eq!(joined, stored, "an uncut, unmasked snippet is the text");
            }
            let marker = stored.contains(['\u{FDD0}', '\u{FDD1}', '\0']);
            let marked = t
                .pieces
                .iter()
                .any(|p| matches!(p, HitPiece::Text { hit: true, .. }));
            match set.ran {
                SearchRan::Words => st.words_marked += usize::from(marked),
                _ => st.exact_marked += usize::from(marked),
            }
            st.marker_fallbacks +=
                usize::from(t.highlight == Highlight::NotShown(NoHighlight::MarkerInText));
            st.hidden += usize::from(t.matched_hidden);
            st.masked_pieces +=
                usize::from(t.pieces.iter().any(|p| matches!(p, HitPiece::Masked(_))));
            st.earlier += usize::from(h.version == HitVersion::Earlier);
            st.unsent += usize::from(h.version == HitVersion::Unsent);
            st.other_chats += usize::from(h.other_chats.n > 0);
            match (set.ran, t.highlight) {
                (SearchRan::Words, Highlight::NotShown(NoHighlight::MarkerInText)) => {
                    assert!(marker)
                }
                (_, Highlight::Shown) => {
                    assert!(!marker || set.ran == SearchRan::Exact);
                    assert!(
                        t.matched_hidden
                            || t.pieces
                                .iter()
                                .any(|p| matches!(p, HitPiece::Text { hit: true, .. }))
                            || t.cut_start
                            || t.cut_end,
                        "a highlighted hit marks a match"
                    );
                }
                other => panic!("unexpected highlight {other:?}"),
            }
            shown.push_str(&joined);
            shown.push('\u{0}');
        }
        start = end;
    }
    shown
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Up to `n` distinct ASCII words of at least 3 letters from the stored
/// text, in a fixed order.
fn words_of(o: &Oracle, n: usize) -> Vec<String> {
    let mut all: BTreeSet<String> = BTreeSet::new();
    for m in o.msgs.values() {
        for (_, t) in &m.bodies {
            for w in t
                .as_deref()
                .unwrap_or("")
                .split(|c: char| !c.is_ascii_alphanumeric())
            {
                if w.len() >= 3 && w.len() <= 20 {
                    all.insert(w.to_ascii_lowercase());
                }
            }
        }
    }
    let step = (all.len() / n.max(1)).max(1);
    all.into_iter().step_by(step).take(n).collect()
}

/// Up to `n` short substrings of the stored text (non-ASCII included), as
/// exact-text needles.
fn needles_of(o: &Oracle, n: usize) -> Vec<String> {
    let mut all: BTreeSet<String> = BTreeSet::new();
    for m in o.msgs.values() {
        for (_, t) in &m.bodies {
            let chars: Vec<char> = t.as_deref().unwrap_or("").chars().collect();
            if chars.len() >= 6 && !chars.iter().any(|c| c.is_control()) {
                let s: String = chars[2..5].iter().collect();
                if !s.trim().is_empty() {
                    all.insert(s.trim().to_owned());
                }
            }
        }
    }
    let step = (all.len() / n.max(1)).max(1);
    all.into_iter().step_by(step).take(n).collect()
}

fn variants(q: &SearchRequest, o: &Oracle) -> Vec<SearchRequest> {
    let mut v = vec![q.clone()];
    let mut x = q.clone();
    x.include_earlier = true;
    v.push(x);
    let mut x = q.clone();
    x.include_events = true;
    v.push(x);
    for s in [SenderFilter::Me, SenderFilter::Others] {
        let mut x = q.clone();
        x.filters.sender = s;
        v.push(x);
    }
    let mut x = q.clone();
    x.filters.has_attachment = true;
    v.push(x);
    let mut keys: Vec<i64> = o.msgs.values().filter_map(|m| dated_ns(&m.key)).collect();
    keys.sort_unstable();
    if let Some(&mid) = keys.get(keys.len() / 2) {
        let mut x = q.clone();
        x.filters.from_utc_ns = Some(mid);
        v.push(x);
        let mut x = q.clone();
        x.filters.until_utc_ns = Some(mid);
        x.include_earlier = true;
        v.push(x);
    }
    let chats: BTreeSet<i64> = o.msgs.values().flat_map(|m| m.chats.clone()).collect();
    for chat in chats {
        let mut x = q.clone();
        x.filters.chat = Some(ChatId::new(chat));
        x.include_earlier = chat % 2 == 0;
        v.push(x);
    }
    v
}

#[test]
fn search_matches_an_independent_oracle_on_the_catalogue() {
    let mut searched = 0usize;
    let mut found = 0usize;
    let mut revealed = 0usize;
    let mut scoped_runs = 0usize;
    let mut stats = Stats::default();
    for (what, model) in model_docs() {
        let order = &model["orders"][0];
        let i = import_order(order, &what);
        let before = data_snapshot(&i.tmp, &i.id);
        let r = i.lib.open_read(&i.id).expect("reader");
        let o = oracle(&i.db());
        let codes = strings(&model["viewer"]["masked"]["oneTimeCodes"]);
        let earlier = strings(&model["viewer"]["masked"]["earlierText"]);
        // (query text, exact)
        let mut queries: Vec<(String, bool)> = Vec::new();
        for w in words_of(&o, 8) {
            // A quoted word has no prefix, so a chat filter takes the
            // scoped plan.
            queries.push((format!("\"{w}\""), false));
            queries.push((w, false));
        }
        for n in needles_of(&o, 6) {
            queries.push((n, true));
        }
        // The withheld strings themselves.
        for s in codes.iter().chain(&earlier) {
            if !s.trim().is_empty() && s.trim().chars().count() <= 64 {
                queries.push((s.clone(), true));
            }
        }
        for (text, exact) in queries {
            let mut base = SearchRequest::new(&text);
            if exact {
                base.mode = SearchMode::Exact;
            }
            for q in variants(&base, &o) {
                let set = run(&r, &q);
                let want = if exact {
                    assert_eq!(set.ran, SearchRan::Exact);
                    let needle = text.trim();
                    expected(&o, &q, &|_, t| t.contains(needle))
                } else {
                    assert_eq!(set.ran, SearchRan::Words);
                    let expr = if text.starts_with('"') {
                        text.clone()
                    } else {
                        format!("\"{text}\" *")
                    };
                    let bodies = fts_bodies(&o, &expr);
                    expected(&o, &q, &|b, _| bodies.contains(&b))
                };
                if set.ran == SearchRan::Words {
                    let scoped = q.filters.chat.is_some() && text.starts_with('"');
                    let plan = if scoped {
                        WordsPlan::Scoped
                    } else {
                        WordsPlan::Global
                    };
                    assert_eq!(set.plan, Some(plan));
                    if scoped {
                        scoped_runs += 1;
                        let _f = install(Faults {
                            scoped_search_rows: Some(0),
                            ..Faults::default()
                        });
                        let global = run(&r, &q);
                        assert_eq!(global.plan, Some(WordsPlan::Global));
                        assert_eq!(got(&global), got(&set), "{what}: scoped and global differ");
                    }
                }
                assert_eq!(got(&set), want, "{what}: a search differs from the oracle");
                assert!(!set.capped);
                if let Some(cov) = set.coverage {
                    assert!(cov.complete && cov.stop.is_none());
                    assert_eq!(cov.examined as usize, examined_in(&o, &q));
                }
                let shown = check_hits(&r, &o, &set, &q, &mut stats);
                for c in &codes {
                    assert!(!shown.contains(c.as_str()), "{what}: a code leaked");
                }
                if q.include_earlier {
                    revealed += usize::from(earlier.iter().any(|e| shown.contains(e.as_str())));
                } else {
                    for e in &earlier {
                        assert!(!shown.contains(e.as_str()), "{what}: earlier text leaked");
                    }
                }
                searched += 1;
                found += set.entries.len();
            }
        }
        drop(r);
        assert_eq!(data_snapshot(&i.tmp, &i.id), before, "{what}: search wrote");
    }
    eprintln!("RECORD searched={searched} found={found} revealed={revealed} scoped={scoped_runs} {stats:?}");
    assert!(
        stats.words_marked > 100 && stats.exact_marked > 100,
        "{stats:?}"
    );
    assert!(
        stats.marker_fallbacks > 0 && stats.hidden > 0 && stats.masked_pieces > 0,
        "{stats:?}"
    );
    assert!(
        stats.earlier > 0 && stats.unsent > 0 && stats.other_chats > 0,
        "{stats:?}"
    );
    assert!(searched > 500, "{searched} searches");
    assert!(found > 500, "{found} hits");
    assert!(revealed > 0, "include_earlier reveals earlier text");
    assert!(scoped_runs > 50, "{scoped_runs} scoped searches");
}

/// Messages an exact scan examines for `q`: every message in its key
/// ranges (and its chat).
fn examined_in(o: &Oracle, q: &SearchRequest) -> usize {
    o.msgs
        .values()
        .filter(|m| q.filters.chat.is_none_or(|c| m.chats.contains(&c.get())))
        .filter(|m| {
            if q.filters.from_utc_ns.is_none() && q.filters.until_utc_ns.is_none() {
                return true;
            }
            dated_ns(&m.key).is_some_and(|ns| {
                q.filters.from_utc_ns.is_none_or(|l| ns >= l)
                    && q.filters.until_utc_ns.is_none_or(|h| ns < h)
            })
        })
        .count()
}

// ---------------------------------------------------------------------------
// Bulk archives
// ---------------------------------------------------------------------------

/// Messages in the shared bulk archive.
const BULK: usize = 12_000;

/// One bulk archive, generated once for this test binary: 12,000
/// messages, six chats, 2 % undated, 5 % CJK, 5 % with an attachment.
fn bulk() -> &'static (tempfile::TempDir, ArchiveId) {
    static B: OnceLock<(tempfile::TempDir, ArchiveId)> = OnceLock::new();
    B.get_or_init(|| {
        let (tmp, lib) = library();
        let id = lib.create(None).expect("create");
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(BULK);
        spec.chats = 6;
        spec.big_chat_permille = 500;
        spec.undated_permille = 20;
        spec.cjk_permille = 50;
        spec.attachment_permille = 50;
        let rep = generate(&mut a, &spec);
        assert!(rep.undated > 50 && rep.cjk > 100, "{rep:?}");
        drop(a);
        (tmp, id)
    })
}

fn bulk_reader() -> (
    Library,
    ArchiveReader,
    &'static (tempfile::TempDir, ArchiveId),
) {
    let b = bulk();
    let lib = Library::new(b.0.path()).expect("library");
    let r = lib.open_read(&b.1).expect("reader");
    (lib, r, b)
}

fn bulk_oracle() -> Oracle {
    let b = bulk();
    oracle(&db_path(&b.0, &b.1))
}

fn exact(text: &str) -> SearchRequest {
    let mut q = SearchRequest::new(text);
    q.mode = SearchMode::Exact;
    q
}

#[test]
fn more_than_ten_thousand_matches_are_capped_and_worded_honestly() {
    let (_lib, r, _) = bulk_reader();
    let o = bulk_oracle();
    // Every body starts with `SYNTHBULK<n>`: the prefix matches them all.
    let q = SearchRequest::new("synthbulk");
    let set = run(&r, &q);
    assert_eq!(
        (set.ran, set.plan),
        (SearchRan::Words, Some(WordsPlan::Global))
    );
    assert!(set.capped && !set.newest_kept);
    assert_eq!(set.entries.len(), SEARCH_MATCH_CAP);
    // Newest first within what was collected: dated keys descending, then
    // undated.
    let keys: Vec<&Vec<u8>> = set
        .entries
        .iter()
        .map(|e| &o.msgs[&e.message.get()].key)
        .collect();
    assert!(keys[..set.dated].windows(2).all(|w| w[0] >= w[1]));
    assert!(keys[..set.dated].iter().all(|k| k[0] == 0));
    assert!(keys[set.dated..].iter().all(|k| k[0] == 1));
    assert!(keys[set.dated..].windows(2).all(|w| w[0] >= w[1]));
    // The exact scan walks newest first, so its 10,000 are the newest.
    let set = run(&r, &exact("SYNTHBULK"));
    assert!(set.capped && set.newest_kept);
    let cov = set.coverage.expect("coverage");
    assert_eq!(cov.stop, Some(ScanStop::MatchCap));
    assert!(!cov.complete);
    let (want, _) = expected(&o, &exact("SYNTHBULK"), &|_, t| t.contains("SYNTHBULK"));
    assert_eq!(got(&set).0, want[..SEARCH_MATCH_CAP].to_vec());
    // A page of hits from the capped words set, highlighted.
    let set = run(&r, &q);
    let hits = r.search_hits(&set, 0..HITS_PER_CALL, &q).expect("hits");
    assert_eq!(hits.len(), HITS_PER_CALL);
    assert!(hits.iter().all(|h| h
        .text
        .as_ref()
        .is_some_and(|t| t.highlight == Highlight::Shown)));
}

#[test]
fn the_exact_scan_examines_in_batches_and_reports_coverage() {
    let (_lib, r, _) = bulk_reader();
    let o = bulk_oracle();
    let mut seen: Vec<ScanProgress> = Vec::new();
    let q = exact(&format!("SYNTHBULK{}", BULK - 1));
    let set = r
        .search(&q, &CancelToken::new(), &mut |p| seen.push(p))
        .expect("scan");
    let cov = set.coverage.expect("coverage");
    assert!(cov.complete && cov.stop.is_none());
    assert_eq!(cov.examined as usize, o.msgs.len());
    assert_eq!(set.entries.len(), 1);
    // Progress after every batch; examined grows by at most one batch.
    assert!(seen.len() >= o.msgs.len() / SCAN_BATCH);
    let mut last = 0;
    for p in &seen {
        assert!(
            p.examined >= last && p.examined - last <= SCAN_BATCH as u64,
            "{p:?}"
        );
        last = p.examined;
    }
    assert_eq!(last, cov.examined);
    assert_eq!(seen.last().map(|p| p.matched), Some(1));
    // The examined limit stops the scan and says so.
    {
        let _f = install(Faults {
            scan_examined_limit: Some(7_000),
            ..Faults::default()
        });
        let set = run(&r, &exact("no such text anywhere"));
        let cov = set.coverage.expect("coverage");
        assert_eq!(
            (cov.examined, cov.complete, cov.stop),
            (7_000, false, Some(ScanStop::ExaminedLimit))
        );
    }
    // Case-sensitive in exact text, folded in words.
    assert!(run(&r, &exact("LOREM")).entries.is_empty());
    let lower = run(&r, &exact("lorem"));
    assert!(!lower.entries.is_empty());
    let w_upper = run(&r, &SearchRequest::new("\"LOREM\""));
    let w_lower = run(&r, &SearchRequest::new("\"lorem\""));
    assert_eq!(got(&w_upper), got(&w_lower));
    assert!(!w_lower.entries.is_empty());
}

#[test]
fn two_cjk_characters_are_found_by_the_scan_and_missed_by_fts() {
    let (_lib, r, _) = bulk_reader();
    let o = bulk_oracle();
    // Every whitespace-separated token of the stored text.
    let tokens: BTreeSet<String> = o
        .msgs
        .values()
        .flat_map(|m| m.bodies.iter())
        .flat_map(|(_, t)| {
            t.as_deref()
                .unwrap_or("")
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    // Two CJK characters from inside a longer token, never a whole token.
    let pair = tokens
        .iter()
        .filter(|t| {
            t.chars().count() >= 3 && t.chars().all(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c))
        })
        .map(|t| t.chars().take(2).collect::<String>())
        .find(|p| !tokens.contains(p))
        .expect("a CJK pair");
    assert!(
        fts_bodies(&o, &format!("\"{pair}\"")).is_empty(),
        "FTS finds 0"
    );
    let q = SearchRequest::new(&pair);
    let set = run(&r, &q);
    assert_eq!(set.ran, SearchRan::Exact, "CJK runs as exact text");
    let (want, dated) = expected(&o, &q, &|_, t| t.contains(pair.as_str()));
    assert!(!want.is_empty());
    assert_eq!(got(&set), (want, dated));
}

#[test]
fn cancellation_is_deterministic() {
    let (_lib, r, _) = bulk_reader();
    // Set before the call: nothing is read, no progress.
    let token = CancelToken::new();
    token.cancel();
    let mut calls = 0;
    for q in [SearchRequest::new("synthbulk"), exact("SYNTHBULK")] {
        let e = r
            .search(&q, &token, &mut |_| calls += 1)
            .expect_err("cancelled");
        assert!(matches!(e, ArchiveError::Cancelled), "{e:?}");
    }
    assert_eq!(calls, 0);
    // Set between two scan batches (from the progress callback): the next
    // batch never starts.
    let token = CancelToken::new();
    let mut calls = 0;
    let e = r
        .search(&exact("no such text anywhere"), &token, &mut |_| {
            calls += 1;
            token.cancel();
        })
        .expect_err("cancelled");
    assert!(matches!(e, ArchiveError::Cancelled), "{e:?}");
    assert_eq!(calls, 1);
    // Set at the third progress-handler tick of the FTS collection.
    {
        let _f = install(Faults {
            cancel_at_view_tick: Some(3),
            ..Faults::default()
        });
        let token = CancelToken::new();
        let e = r
            .search(&SearchRequest::new("synthbulk"), &token, &mut |_| {})
            .expect_err("cancelled");
        assert!(matches!(e, ArchiveError::Cancelled), "{e:?}");
        assert!(token.is_cancelled());
    }
    // The reader works afterwards.
    assert!(!run(&r, &SearchRequest::new("lorem")).entries.is_empty());
}

#[test]
fn the_search_interrupt_cancels_from_another_thread() {
    let (_lib, r, _) = bulk_reader();
    let interrupt = r.search_interrupt();
    let done = Arc::new(AtomicBool::new(false));
    let stop = Arc::clone(&done);
    let t = std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            interrupt.interrupt();
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
    });
    // A prefix over every body, under an unreachable budget: only the
    // interrupt can stop it before it finishes (it takes milliseconds).
    let mut outcomes = Vec::new();
    for _ in 0..5 {
        let e = r.search(
            &SearchRequest::new("synthbulk"),
            &CancelToken::new(),
            &mut |_| {},
        );
        outcomes.push(matches!(e, Err(ArchiveError::Cancelled)));
        assert!(
            matches!(e, Ok(_) | Err(ArchiveError::Cancelled)),
            "never damaged: {e:?}"
        );
    }
    done.store(true, Ordering::Relaxed);
    t.join().expect("join");
    assert!(
        outcomes.iter().any(|c| *c),
        "the interrupt stopped a search"
    );
    assert!(!run(&r, &SearchRequest::new("lorem")).entries.is_empty());
}

#[test]
fn budgets_give_too_broad_or_partial_coverage_never_damaged() {
    let (_lib, r, _) = bulk_reader();
    let q = SearchRequest::new("synthbulk");
    let set = run(&r, &q);
    let _f = install(Faults {
        view_op_budget: Some(0),
        ..Faults::default()
    });
    let e = r
        .search(&q, &CancelToken::new(), &mut |_| {})
        .expect_err("too broad");
    assert!(matches!(e, ArchiveError::QueryTooBroad), "{e:?}");
    let scan = run(&r, &exact("no such text anywhere"));
    let cov = scan.coverage.expect("coverage");
    assert_eq!((cov.complete, cov.stop), (false, Some(ScanStop::TooBroad)));
    drop(_f);
    // The highlight phase out of time: the page is sent unhighlighted.
    // (Its op budget cannot see work inside FTS5's own calls, which are
    // single VM opcodes; the deadline is what bounds them.)
    let _f = install(Faults {
        hits_window_ms: Some(0),
        ..Faults::default()
    });
    let hits = r.search_hits(&set, 0..10, &q).expect("hits");
    assert_eq!(hits.len(), 10);
    for h in &hits {
        let t = h.text.as_ref().expect("text");
        assert_eq!(t.highlight, Highlight::NotShown(NoHighlight::Budget));
        assert!(!t
            .pieces
            .iter()
            .any(|p| matches!(p, HitPiece::Text { hit: true, .. })));
    }
}

#[test]
fn hit_pages_are_bounded_and_requests_are_checked() {
    let (_lib, r, _) = bulk_reader();
    let q = SearchRequest::new("lorem");
    let set = run(&r, &q);
    assert!(set.entries.len() > HITS_PER_CALL + 10);
    assert!(matches!(
        r.search_hits(&set, 0..HITS_PER_CALL + 1, &q),
        Err(ArchiveError::Internal(_))
    ));
    let n = set.entries.len();
    assert_eq!(
        r.search_hits(&set, n - 3..n + 40, &q).expect("cut").len(),
        3
    );
    assert!(r
        .search_hits(&set, n + 5..n + 9, &q)
        .expect("past")
        .is_empty());
    assert!(matches!(
        r.search_hits(&set, 0..5, &exact("lorem")),
        Err(ArchiveError::Internal(_))
    ));
    // Refused queries, each with its problem.
    for (text, problem) in [
        ("\"\"", QueryProblem::NoTerms),
        ("a\u{0}b", QueryProblem::ControlCharacter),
        ("a\tb", QueryProblem::ControlCharacter),
        (&*"x ".repeat(17), QueryProblem::TooManyTerms),
        (&*"y".repeat(129), QueryProblem::TermTooLong),
        (&*"z".repeat(513), QueryProblem::TooLong),
    ] {
        let e = r
            .search(&SearchRequest::new(text), &CancelToken::new(), &mut |_| {})
            .expect_err("refused");
        assert!(
            matches!(e, ArchiveError::InvalidQuery(p) if p == problem),
            "{e:?}"
        );
    }
    let e = r
        .search(&exact(&"字".repeat(65)), &CancelToken::new(), &mut |_| {})
        .expect_err("refused");
    assert!(matches!(
        e,
        ArchiveError::InvalidQuery(QueryProblem::NeedleTooLong)
    ));
    let mut q = SearchRequest::new("lorem");
    q.filters.from_utc_ns = Some(5);
    q.filters.until_utc_ns = Some(5);
    assert!(matches!(
        r.search(&q, &CancelToken::new(), &mut |_| {}),
        Err(ArchiveError::Internal(_))
    ));
}

#[test]
fn a_damaged_index_is_search_unavailable_and_browsing_still_works() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        generate(&mut a, &BulkSpec::new(1_500));
    }
    {
        // The fault: every FTS5 leaf page overwritten (a copy only this
        // test owns).
        let c = raw(&db_path(&tmp, &id));
        let n = c
            .execute(
                "UPDATE message_fts_data SET block = randomblob(length(block)) WHERE id > 10",
                [],
            )
            .expect("corrupt");
        assert!(n > 0);
    }
    let r = lib.open_read(&id).expect("reader");
    let e = r
        .search(
            &SearchRequest::new("\"lorem\""),
            &CancelToken::new(),
            &mut |_| {},
        )
        .expect_err("unavailable");
    assert!(matches!(e, ArchiveError::SearchUnavailable), "{e:?}");
    // Exact text does not read the index; browsing works.
    let set = run(&r, &exact("lorem"));
    assert!(!set.entries.is_empty());
    assert_eq!(
        r.search_hits(&set, 0..5, &exact("lorem"))
            .expect("hits")
            .len(),
        5
    );
    let chats = r
        .conversation_index(oma_archive::ConversationSort::Recent, None)
        .expect("index");
    let page = r
        .thread_page(
            chats.ids[0],
            Anchor::Newest,
            Direction::Older,
            20,
            None,
            Reveal::default(),
        )
        .expect("page");
    assert!(!page.items.is_empty());
}

fn unsearchable_oracle(o: &Oracle) -> u64 {
    o.msgs
        .values()
        .filter(|m| m.present == 0 && m.item == 1)
        .filter(|m| {
            m.current
                .and_then(|b| m.bodies.iter().find(|(id, _)| *id == b))
                .and_then(|(_, t)| t.as_ref())
                .is_none()
        })
        .count() as u64
}

#[test]
fn the_unsearchable_count_matches_the_stored_null_bodies() {
    let mut nonzero = 0;
    for (what, model) in model_docs() {
        let i = import_order(&model["orders"][0], &what);
        let r = i.lib.open_read(&i.id).expect("reader");
        let want = unsearchable_oracle(&oracle(&i.db()));
        assert_eq!(r.unsearchable_count().expect("count"), want, "{what}");
        nonzero += usize::from(want > 0);
    }
    assert!(nonzero > 0, "the catalogue has bodies with no text");
    let (_lib, r, _) = bulk_reader();
    assert_eq!(
        r.unsearchable_count().expect("count"),
        unsearchable_oracle(&bulk_oracle())
    );
}
