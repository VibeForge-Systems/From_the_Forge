//! Export read measurements (M6 spec §3.4, §12, slice 6b2): the
//! throughput baseline of the export reads. Ignored by default; the
//! numbers are **synthetic, local, one host**, never claims about real
//! data, and no timing is ever asserted.
//!
//! It generates one bulk archive (`OMA_PERF_MESSAGES` messages, default
//! 500,000, in 2,000 chats, one holding 40 % of them; 5 % reactions, 1 %
//! attachments without files) and prints, once each:
//!
//! - `export_preview` of the whole archive (the exact counts) and of the
//!   largest conversation;
//! - `scope_index` of the whole archive;
//! - the job's walk at `ExportDepth::Items`: every conversation's timeline
//!   and the messages in no conversation through the cursors (pre-measure
//!   and byte batches included), each batch assembled with its uncapped
//!   rows, as messages per second;
//! - the "all matches" re-run (`search_ids`, cap 1,000,000) of a common
//!   word in words mode and as exact text, and `search_scope` over its
//!   result;
//! - the process's peak resident set (`VmHWM`, Linux).
//!
//! ```sh
//! cargo test -p oma-archive --test perf_export -- --ignored --nocapture
//! ```
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::time::{Duration, Instant};

use common::bulk::{generate, BulkSpec};
use common::import::read_only;
use common::*;
use oma_archive::{
    CancelToken, ChatId, ExportDepth, ExportScopeIds, PreviewOptions, Reveal, SearchMode,
    SearchRequest, TimeRange, EXPORT_SEARCH_CAP,
};

fn env_count(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn secs(d: Duration) -> String {
    format!("{:.2} s", d.as_secs_f64())
}

fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

#[test]
#[ignore = "a measurement; run with --ignored --nocapture"]
fn measure_export_reads() {
    let n = env_count("OMA_PERF_MESSAGES", 500_000);
    let mut spec = BulkSpec::new(n);
    spec.chats = 2_000;
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let t = Instant::now();
    {
        let mut a = lib.open(&id).expect("open");
        let rep = generate(&mut a, &spec);
        println!("RECORD bulk: {rep:?}");
    }
    println!("RECORD bulk: generated in {}", secs(t.elapsed()));
    let db = db_path(&tmp, &id);
    println!(
        "RECORD bulk: database {} MiB",
        std::fs::metadata(&db).map_or(0, |m| m.len() >> 20)
    );
    let (big, word) = {
        let c = read_only(&db);
        let big: i64 = c
            .query_row(
                "SELECT chat_id FROM chat_message GROUP BY chat_id ORDER BY count(*) DESC \
                 LIMIT 1",
                [],
                |r| r.get(0),
            )
            .expect("largest chat");
        (ChatId::new(big), "zephyr".to_owned())
    };
    let r = lib.open_read(&id).expect("reader");
    let opts = PreviewOptions::new(Reveal::default(), ExportDepth::Items);
    let whole = ExportScopeIds::Archive {
        range: TimeRange::All,
    };
    for (what, scope) in [
        ("archive", whole.clone()),
        (
            "largest conversation",
            ExportScopeIds::Conversation {
                chat: big,
                range: TimeRange::All,
            },
        ),
    ] {
        let t = Instant::now();
        let p = r
            .export_preview(&scope, opts, &CancelToken::new(), &mut |_| {})
            .expect("preview");
        println!(
            "RECORD export_preview ({what}): {} | {} rows, {} distinct, {} folded",
            secs(t.elapsed()),
            p.timeline_rows,
            p.distinct_messages,
            p.folded_reaction_rows
        );
    }
    let hold = lib.hold_for_export(&id).expect("hold");
    r.with_export_snapshot(&hold, &CancelToken::new(), |s| {
        let t = Instant::now();
        let ix = s.scope_index(&whole)?;
        println!(
            "RECORD scope_index (archive): {} | {} ids",
            secs(t.elapsed()),
            ix.len()
        );
        // The job's walk: cursors, pre-measure, byte batches, assembly.
        let t = Instant::now();
        let (mut items, mut batches) = (0u64, 0u64);
        let mut chats: Vec<Option<ChatId>> = s.chat_ids()?.into_iter().map(Some).collect();
        chats.push(None);
        for chat in chats {
            let mut cur = match chat {
                Some(c) => s.timeline(c, TimeRange::All),
                None => s.unassigned(TimeRange::All),
            };
            while let Some(b) = cur.next_batch()? {
                batches += 1;
                items += b.items.len() as u64;
                let xs = s.assemble(&b.items, Reveal::default(), ExportDepth::Items)?;
                std::hint::black_box(xs);
            }
        }
        let el = t.elapsed();
        println!(
            "RECORD walk + assemble (archive, Items): {} | {items} items in {batches} batches, \
             {:.0} items/s",
            secs(el),
            items as f64 / el.as_secs_f64()
        );
        for mode in [SearchMode::Auto, SearchMode::Exact] {
            let mut q = SearchRequest::new(&word);
            q.mode = mode;
            let t = Instant::now();
            let set = s.search_ids(&q, EXPORT_SEARCH_CAP, &mut |_| {})?;
            println!(
                "RECORD search_ids ({mode:?}, cap 1,000,000): {} | {} hits, capped {}",
                secs(t.elapsed()),
                set.entries.len(),
                set.capped
            );
            let t = Instant::now();
            let sc = s.search_scope(&q, &set, opts)?;
            println!(
                "RECORD search_scope ({mode:?}): {} | {} kept in {} groups, {} excluded",
                secs(t.elapsed()),
                sc.kept,
                sc.groups.len(),
                sc.excluded.total()
            );
        }
        Ok::<_, oma_archive::ArchiveError>(())
    })
    .expect("snapshot");
    println!(
        "RECORD peak resident set: {} MiB",
        peak_rss_kib().map_or(0, |k| k >> 10)
    );
}
