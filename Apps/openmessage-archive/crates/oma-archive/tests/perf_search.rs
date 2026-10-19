//! Search measurements (M5 spec §4, §11, slice 5c). Ignored by default; the
//! numbers are **synthetic, local, one host**, never claims about real
//! data, and no timing is ever asserted.
//!
//! It generates a bulk archive (`OMA_PERF_MESSAGES` messages, default
//! 500,000; 300 chats, so an ordinary chat holds about 1,000 messages and
//! one holds 40 %) and prints, per operation, the median and the worst of
//! five warm runs:
//!
//! - words mode: a common word (global), the same word scoped to a
//!   1,000-message chat, the same word scoped to that chat through the
//!   global plan, and two prefixes (`lor*`, one word's prefix; `syn*`,
//!   shared by one distinct `SYNTHBULK<n>` term per message: a hostile
//!   prefix of `OMA_PERF_MESSAGES` terms);
//! - the exact-text scan of the whole archive;
//! - `search_hits` of 50 hits with highlighting, for the common word and
//!   for a prefix;
//! - the cost of one rowid-constrained `MATCH` per body, plain and with a
//!   prefix (why the scoped plan refuses prefixes);
//! - the unsearchable count;
//! - the latency of a cancel (token, and `SearchInterrupt`) during the
//!   hostile prefix's collection.
//!
//! The test profile builds `oma-archive` and the bundled SQLite at
//! `opt-level = 3` (the workspace's dev profile), so it runs as is:
//!
//! ```sh
//! OMA_PERF_MESSAGES=500000 cargo test -p oma-archive --test perf_search -- --ignored --nocapture
//! ```
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::time::{Duration, Instant};

use common::bulk::{generate, BulkSpec};
use common::import::read_only;
use common::*;
use oma_archive::hooks::{install, Faults};
use oma_archive::{
    ArchiveError, ArchiveReader, CancelToken, ChatId, ConversationSort, SearchMode, SearchRequest,
    WordsPlan,
};

fn time<T>(runs: usize, mut f: impl FnMut() -> T) -> (Duration, Duration, T) {
    let mut v = Vec::with_capacity(runs);
    let mut last = None;
    for _ in 0..runs {
        let t = Instant::now();
        last = Some(f());
        v.push(t.elapsed());
    }
    v.sort();
    (v[v.len() / 2], v[v.len() - 1], last.expect("ran"))
}

fn ms(d: Duration) -> String {
    format!("{:.2} ms", d.as_secs_f64() * 1000.0)
}

fn search(
    r: &ArchiveReader,
    q: &SearchRequest,
) -> Result<oma_archive::SearchResultSet, ArchiveError> {
    r.search(q, &CancelToken::new(), &mut |_| {})
}

#[test]
#[ignore = "a measurement; run with --ignored --nocapture"]
fn measure_search() {
    let n: usize = std::env::var("OMA_PERF_MESSAGES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500_000);
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let t = Instant::now();
    {
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(n);
        spec.chats = 300;
        let rep = generate(&mut a, &spec);
        println!("RECORD bulk {n} messages: {rep:?}");
    }
    println!("RECORD generated in {}", ms(t.elapsed()));
    let db = db_path(&tmp, &id);
    println!(
        "RECORD database {} MiB",
        std::fs::metadata(&db).map(|m| m.len() >> 20).unwrap_or(0)
    );
    let r = lib.open_read(&id).expect("reader");
    let idx = r
        .conversation_index(ConversationSort::Count, None)
        .expect("index");
    let big = idx.ids[0];
    let rows = r.conversations(&idx.ids[..2]).expect("rows");
    let small = idx.ids[1];
    println!(
        "RECORD chats {}; largest {:?}; next {:?}",
        idx.ids.len(),
        rows[0].stats.as_ref().map(|s| s.items_recorded),
        rows[1].stats.as_ref().map(|s| s.items_recorded)
    );

    let line = |what: &str,
                (med, worst, out): (
        Duration,
        Duration,
        Result<oma_archive::SearchResultSet, ArchiveError>,
    )| {
        match out {
            Ok(set) => println!(
                "RECORD {what}: median {} worst {} | {} hits, capped {}, plan {:?}, coverage {:?}",
                ms(med),
                ms(worst),
                set.entries.len(),
                set.capped,
                set.plan,
                set.coverage
            ),
            Err(e) => println!(
                "RECORD {what}: median {} worst {} | {e:?}",
                ms(med),
                ms(worst)
            ),
        }
    };

    let common_word = SearchRequest::new("\"lorem\"");
    line(
        "words common (global)",
        time(5, || search(&r, &common_word)),
    );
    let mut scoped = common_word.clone();
    scoped.filters.chat = Some(small);
    line(
        "words common scoped to a ~1,000-message chat",
        time(5, || search(&r, &scoped)),
    );
    {
        let _f = install(Faults {
            scoped_search_rows: Some(0),
            ..Faults::default()
        });
        line(
            "words common, same chat, global plan with the filter",
            time(5, || search(&r, &scoped)),
        );
    }
    let mut in_big = common_word.clone();
    in_big.filters.chat = Some(big);
    line(
        "words common in the largest chat",
        time(5, || search(&r, &in_big)),
    );
    line(
        "words prefix lor*",
        time(5, || search(&r, &SearchRequest::new("lor"))),
    );
    line(
        "words prefix synthbulk1* (one term per message)",
        time(5, || search(&r, &SearchRequest::new("synthbulk1"))),
    );
    line(
        "words prefix syn* (hostile: one term per message)",
        time(3, || search(&r, &SearchRequest::new("syn"))),
    );
    let mut scan = SearchRequest::new("no such text anywhere");
    scan.mode = SearchMode::Exact;
    line(
        "exact, whole archive, rare needle",
        time(3, || search(&r, &scan)),
    );
    let mut scan_common = SearchRequest::new("lorem");
    scan_common.mode = SearchMode::Exact;
    line(
        "exact, common needle (stops at the cap)",
        time(3, || search(&r, &scan_common)),
    );

    for (what, q) in [
        ("common word", common_word.clone()),
        ("prefix lor*", SearchRequest::new("lor")),
        ("prefix synthbulk1*", SearchRequest::new("synthbulk1")),
    ] {
        let set = search(&r, &q).expect("set");
        let (med, worst, hits) = time(5, || r.search_hits(&set, 0..50, &q).expect("hits"));
        let shown = hits
            .iter()
            .filter(|h| {
                h.text
                    .as_ref()
                    .is_some_and(|t| t.highlight == oma_archive::Highlight::Shown)
            })
            .count();
        println!(
            "RECORD search_hits 50 ({what}): median {} worst {} | {shown} highlighted",
            ms(med),
            ms(worst)
        );
    }

    // One rowid-constrained MATCH per body (what the scoped plan does).
    let c = read_only(&db);
    let bodies: Vec<i64> = c
        .prepare("SELECT current_body_id FROM message WHERE current_body_id IS NOT NULL LIMIT 200")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("bodies");
    let chat_bodies: Vec<i64> = c
        .prepare(
            "SELECT m.current_body_id FROM chat_message cm JOIN message m ON m.id = cm.message_id \
             WHERE cm.chat_id = ?1 AND m.current_body_id IS NOT NULL LIMIT 200",
        )
        .expect("prepare")
        .query_map([small.get()], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("bodies");
    {
        let mut st = c
            .prepare("SELECT 1 FROM message_fts WHERE message_fts MATCH ?1 AND rowid = ?2")
            .expect("prepare");
        let t = Instant::now();
        for b in &chat_bodies {
            let _ = st
                .query_row(rusqlite::params!["\"lorem\"", b], |_| Ok(()))
                .ok();
        }
        println!(
            "RECORD rowid-constrained MATCH \"lorem\" over one chat's bodies: {} per body ({} bodies)",
            ms(t.elapsed() / u32::try_from(chat_bodies.len().max(1)).expect("len")),
            chat_bodies.len()
        );
    }
    for expr in ["\"lorem\"", "\"lor\" *", "\"synthbulk1\" *"] {
        let mut st = c
            .prepare("SELECT 1 FROM message_fts WHERE message_fts MATCH ?1 AND rowid = ?2")
            .expect("prepare");
        let t = Instant::now();
        for b in &bodies {
            let _ = st.query_row(rusqlite::params![expr, b], |_| Ok(())).ok();
        }
        println!(
            "RECORD rowid-constrained MATCH {expr}: {} per body",
            ms(t.elapsed() / u32::try_from(bodies.len()).expect("len"))
        );
    }

    // What the scan's reads cost by themselves, outside the crate: the
    // date-ordered walk with each current body (random page reads), at the
    // default page cache and at a 64 MiB one, and the bodies in rowid order.
    for (what, cache) in [("default cache", None), ("64 MiB cache", Some(-65_536i64))] {
        let c = read_only(&db);
        if let Some(kib) = cache {
            c.pragma_update(None, "cache_size", kib).expect("cache");
        }
        let mut st = c
            .prepare(
                "SELECT m.id, b.display_text FROM message m \
                 LEFT JOIN message_body b ON b.id = m.current_body_id \
                 WHERE m.sort_key < X'02' ORDER BY m.sort_key DESC, m.id DESC",
            )
            .expect("prepare");
        for pass in 0..2 {
            let t = Instant::now();
            let mut rows = st.query([]).expect("query");
            let mut n = 0u64;
            let mut found = 0u64;
            while let Some(row) = rows.next().expect("row") {
                n += 1;
                if let rusqlite::types::ValueRef::Text(b) = row.get_ref(1).expect("text") {
                    found += u64::from(std::str::from_utf8(b).is_ok_and(|t| t.contains("zzqq")));
                }
            }
            println!(
                "RECORD raw date-ordered walk with bodies ({what}, pass {pass}): {} for {n} rows ({found})",
                ms(t.elapsed())
            );
        }
    }
    {
        let t = Instant::now();
        let n: i64 = c
            .query_row(
                "SELECT count(*) FROM message_body WHERE instr(display_text, 'zzqq') > 0",
                [],
                |row| row.get(0),
            )
            .expect("instr");
        println!(
            "RECORD raw instr over message_body in rowid order: {} ({n})",
            ms(t.elapsed())
        );
    }

    let (med, worst, count) = time(3, || r.unsearchable_count());
    println!(
        "RECORD unsearchable_count: median {} worst {} | {count:?}",
        ms(med),
        ms(worst)
    );

    // Cancel latency during the hostile prefix's collection.
    let hostile = SearchRequest::new("syn");
    let (whole, _, _) = time(1, || search(&r, &hostile));
    for via in ["token", "interrupt"] {
        let token = CancelToken::new();
        let later = token.clone();
        let interrupt = r.search_interrupt();
        let delay = whole / 4;
        let started = Instant::now();
        let t = std::thread::spawn(move || {
            std::thread::sleep(delay);
            later.cancel();
            if via == "interrupt" {
                interrupt.interrupt();
            }
            Instant::now()
        });
        let out = r.search(&hostile, &token, &mut |_| {});
        let returned = Instant::now();
        let cancelled_at = t.join().expect("join");
        println!(
            "RECORD cancel via {via} after {}: returned {} after the cancel ({:?}; total {})",
            ms(delay),
            ms(returned.saturating_duration_since(cancelled_at)),
            out.as_ref()
                .map(|s| s.entries.len())
                .map_err(|e| format!("{e:?}")),
            ms(returned - started)
        );
    }
    let _ = (ChatId::new(0), WordsPlan::Global);
}
