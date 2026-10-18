//! Performance measurements (spec §18.11). Ignored by default; results are
//! recorded in `docs/compatibility.md` as **synthetic, local, one host**.
//!
//! `measure_growth_factor_k` imports a synthetic sms.db (the generator of
//! `common::import::synthetic_sms`, `OMA_PERF_MESSAGES` messages, default
//! 20,000) and reports the archive database's size against the staged
//! sms.db: the growth factor `K` of the space estimate (spec §12), which
//! the code uses as a provisional 3.0. It also reports the elapsed time and,
//! on Linux, the peak resident set size (`VmHWM`).
//!
//! ```sh
//! OMA_PERF_MESSAGES=100000 cargo test -p oma-archive --release --test perf -- --ignored --nocapture
//! ```
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::time::Instant;

use common::import::*;
use common::*;
use oma_archive::space::GROWTH_K_TENTHS;
use oma_archive::LineageChoice;

fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

#[test]
#[ignore = "a measurement; run with --ignored --nocapture"]
fn measure_growth_factor_k() {
    let messages: usize = std::env::var("OMA_PERF_MESSAGES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let handles = (messages / 50).max(16);
    let chats = (messages / 100).max(16);
    let t0 = Instant::now();
    let sms = synthetic_sms(5, handles, chats, messages);
    let generated = t0.elapsed();
    let tmp = tempfile::tempdir().expect("tempdir");
    let backup = backup_with_sms(&tmp.path().join("backup"), &sms);
    let (ltmp, lib) = library();
    let id = lib.create(None).expect("create");
    let empty = std::fs::metadata(db_path(&ltmp, &id)).expect("db").len();
    let mut a = lib.open(&id).expect("open");
    let src = open_dir(&backup, None);
    let t1 = Instant::now();
    let s = import(&mut a, &src, &opts(1000), LineageChoice::New);
    let imported = t1.elapsed();
    let close = a.close();
    src.ob.close();
    let db = db_path(&ltmp, &id);
    let size = std::fs::metadata(&db).expect("db").len();
    let wal = std::fs::metadata(db.with_file_name("archive.sqlite-wal"))
        .map(|m| m.len())
        .unwrap_or(0);
    let staged = sms.len() as f64;
    let grown = (size + wal).saturating_sub(empty) as f64;
    println!(
        "PERF messages={messages} handles={handles} chats={chats} staged_bytes={} \
         archive_bytes={size} wal_bytes={wal} empty_archive_bytes={empty} \
         K={:.2} K_with_empty_schema={:.2} K_provisional={:.1} generate_s={:.2} import_s={:.2} \
         batches={} peak_rss_kib={:?} checkpoint={:?}",
        sms.len(),
        grown / staged,
        (size + wal) as f64 / staged,
        GROWTH_K_TENTHS as f64 / 10.0,
        generated.as_secs_f64(),
        imported.as_secs_f64(),
        s.counts.batches,
        peak_rss_kib(),
        close.checkpoint,
    );
    assert!(s.counts.messages.seen > 0);
}
