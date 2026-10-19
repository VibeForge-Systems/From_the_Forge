//! Viewer read measurements (M5 spec §11, §13.10, slice 5k). Ignored by
//! default; the numbers are **synthetic, local, one host**, never claims
//! about real data, and no timing is ever asserted.
//!
//! It generates three bulk archives with the bulk generator
//! (`tests/common/bulk.rs`) and prints, per operation, the median and the
//! worst of five warm runs:
//!
//! - **the main archive**: `OMA_PERF_MESSAGES` messages (default 500,000)
//!   in 2,000 chats, one of which holds 40 % of them (200,000 by default):
//!   `conversation_index` (recent, name and count), `conversations` for one
//!   list window of rows, `thread_page` of 50 at the newest, the oldest and
//!   the middle of the largest chat in both directions, `seek_date` to the
//!   middle plus an `Around` page, and `day_counts` for one month;
//! - **a list archive** of `OMA_PERF_LIST_MESSAGES` messages (default
//!   100,000) in 20,000 chats: `conversation_index` (recent);
//! - **a reaction archive** of `OMA_PERF_REACTION_MESSAGES` messages
//!   (default 50,000) of which 90 % are reactions: `thread_page` of 50 at
//!   the newest and the middle of its largest chat, with the page's
//!   `partial` flag.
//!
//! The test profile builds `oma-archive` and the bundled SQLite at
//! `opt-level = 3` (the workspace's dev profile), so it runs as is:
//!
//! ```sh
//! cargo test -p oma-archive --test perf_viewer -- --ignored --nocapture
//! ```
//!
//! The image renditions are measured by `media_corpus.rs`'s
//! `perf_render_a_large_photo` (in process, release profile), and the
//! protocol's chunks by the shell's tests; neither is repeated here.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::time::{Duration, Instant};

use common::bulk::{generate, BulkSpec};
use common::import::read_only;
use common::*;
use oma_archive::{
    Anchor, ArchiveError, ArchiveReader, ChatId, ConversationSort, Direction, Library, MessageId,
    Reveal, SeekDir, ThreadPage,
};

const RUNS: usize = 5;

fn env_count(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn time<T>(runs: usize, mut f: impl FnMut() -> T) -> (Duration, Duration, T) {
    // One untimed run first: every figure is warm.
    let _ = f();
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

/// A bulk archive of `spec`, its reader and its database path.
fn bulk(what: &str, spec: &BulkSpec) -> (tempfile::TempDir, Library, ArchiveReader) {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let t = Instant::now();
    {
        let mut a = lib.open(&id).expect("open");
        let rep = generate(&mut a, spec);
        println!("RECORD {what}: {rep:?}");
    }
    println!("RECORD {what}: generated in {}", ms(t.elapsed()));
    println!(
        "RECORD {what}: database {} MiB",
        std::fs::metadata(db_path(&tmp, &id)).map_or(0, |m| m.len() >> 20)
    );
    let r = lib.open_read(&id).expect("reader");
    (tmp, lib, r)
}

fn page_line(
    what: &str,
    (med, worst, out): (Duration, Duration, Result<ThreadPage, ArchiveError>),
) {
    match out {
        Ok(p) => println!(
            "RECORD {what}: median {} worst {} | {} items, partial {}, extras incomplete {}, cut {}",
            ms(med),
            ms(worst),
            p.items.len(),
            p.partial,
            p.extras_incomplete,
            p.cut_for_size
        ),
        Err(e) => println!("RECORD {what}: median {} worst {} | {e:?}", ms(med), ms(worst)),
    }
}

fn page(
    r: &ArchiveReader,
    chat: ChatId,
    anchor: Anchor,
    dir: Direction,
) -> Result<ThreadPage, ArchiveError> {
    r.thread_page(chat, anchor, dir, 50, None, Reveal::default())
}

/// The dated range of `chat` and the id of a message near its middle date.
fn middle(tmp: &tempfile::TempDir, r: &ArchiveReader, chat: ChatId) -> (i64, i64, i64) {
    let c = read_only(&db_path(tmp, r.id()));
    let (lo, hi): (i64, i64) = c
        .query_row(
            "SELECT min(m.date_utc_ns), max(m.date_utc_ns) FROM chat_message cm \
             JOIN message m ON m.id = cm.message_id \
             WHERE cm.chat_id = ?1 AND m.date_utc_ns IS NOT NULL",
            [chat.get()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("range");
    (lo, hi, lo + (hi - lo) / 2)
}

#[test]
#[ignore = "a measurement; run with --ignored --nocapture"]
fn measure_viewer_reads() {
    let n = env_count("OMA_PERF_MESSAGES", 500_000);
    let mut spec = BulkSpec::new(n);
    spec.chats = 2_000;
    let (tmp, _lib, r) = bulk("main", &spec);

    for (what, sort) in [
        ("recent", ConversationSort::Recent),
        ("name", ConversationSort::Name),
        ("count", ConversationSort::Count),
    ] {
        let (med, worst, idx) = time(RUNS, || r.conversation_index(sort, None));
        println!(
            "RECORD conversation_index ({what}): median {} worst {} | {:?} chats",
            ms(med),
            ms(worst),
            idx.map(|i| i.ids.len())
        );
    }
    let idx = r
        .conversation_index(ConversationSort::Count, None)
        .expect("index");
    let big = idx.ids[0];
    let window: Vec<ChatId> = idx.ids.iter().copied().take(50).collect();
    let (med, worst, rows) = time(RUNS, || r.conversations(&window));
    println!(
        "RECORD conversations (50 rows with previews): median {} worst {} | {:?}",
        ms(med),
        ms(worst),
        rows.map(|v| v.len())
    );
    let rows = r.conversations(&idx.ids[..1]).expect("row");
    println!(
        "RECORD largest chat: {:?} items recorded",
        rows[0].stats.as_ref().map(|s| s.items_recorded)
    );

    page_line(
        "thread_page newest 50 (largest chat)",
        time(RUNS, || page(&r, big, Anchor::Newest, Direction::Older)),
    );
    page_line(
        "thread_page oldest 50 (largest chat)",
        time(RUNS, || page(&r, big, Anchor::Oldest, Direction::Newer)),
    );

    let (lo, hi, mid) = middle(&tmp, &r, big);
    println!(
        "RECORD largest chat spans {} days",
        (hi - lo) / 86_400_000_000_000
    );
    let (med, worst, found) = time(RUNS, || r.seek_date(big, mid, SeekDir::OnOrAfter));
    println!(
        "RECORD seek_date to the middle: median {} worst {} | found {}",
        ms(med),
        ms(worst),
        matches!(found, Ok(Some(_)))
    );
    let anchor: MessageId = found.expect("seek").expect("an item");
    let at = |dir| Anchor::Message {
        id: anchor,
        include: dir != Direction::Around,
    };
    page_line(
        "thread_page around the middle (50)",
        time(RUNS, || {
            page(&r, big, at(Direction::Around), Direction::Around)
        }),
    );
    page_line(
        "thread_page older from the middle (50)",
        time(RUNS, || {
            page(&r, big, at(Direction::Older), Direction::Older)
        }),
    );
    page_line(
        "thread_page newer from the middle (50)",
        time(RUNS, || {
            page(&r, big, at(Direction::Newer), Direction::Newer)
        }),
    );
    let (med, worst, _) = time(RUNS, || {
        let id = r
            .seek_date(big, mid, SeekDir::OnOrAfter)
            .expect("seek")
            .expect("item");
        page(
            &r,
            big,
            Anchor::Message { id, include: false },
            Direction::Around,
        )
    });
    println!(
        "RECORD seek_date plus an Around page: median {} worst {}",
        ms(med),
        ms(worst)
    );

    // One month of local days (here UTC days) around the middle.
    let day = 86_400_000_000_000i64;
    let start = mid - mid.rem_euclid(day);
    let bounds: Vec<i64> = (0..=31).map(|i| start + i * day).collect();
    let (med, worst, counts) = time(RUNS, || r.day_counts(big, &bounds));
    println!(
        "RECORD day_counts (one month, largest chat): median {} worst {} | {:?} items",
        ms(med),
        ms(worst),
        counts.map(|c| c.counts.iter().sum::<u64>())
    );
    drop(r);
    drop(tmp);

    let list_n = env_count("OMA_PERF_LIST_MESSAGES", 100_000);
    let mut list = BulkSpec::new(list_n);
    list.chats = 20_000;
    list.handles = list.handles.max(20_000);
    let (tmp, _lib, r) = bulk("list", &list);
    let (med, worst, idx) = time(RUNS, || {
        r.conversation_index(ConversationSort::Recent, None)
    });
    println!(
        "RECORD conversation_index (recent, many chats): median {} worst {} | {:?} chats",
        ms(med),
        ms(worst),
        idx.map(|i| i.ids.len())
    );
    drop(r);
    drop(tmp);

    let react_n = env_count("OMA_PERF_REACTION_MESSAGES", 50_000);
    let mut react = BulkSpec::new(react_n);
    react.reaction_permille = 900;
    let (tmp, _lib, r) = bulk("reactions", &react);
    let idx = r
        .conversation_index(ConversationSort::Count, None)
        .expect("index");
    let big = idx.ids[0];
    page_line(
        "thread_page newest 50 (90 % reactions)",
        time(RUNS, || page(&r, big, Anchor::Newest, Direction::Older)),
    );
    let (_, _, mid) = middle(&tmp, &r, big);
    if let Ok(Some(id)) = r.seek_date(big, mid, SeekDir::OnOrAfter) {
        page_line(
            "thread_page around the middle (90 % reactions)",
            time(RUNS, || {
                page(
                    &r,
                    big,
                    Anchor::Message { id, include: false },
                    Direction::Around,
                )
            }),
        );
    }
}
