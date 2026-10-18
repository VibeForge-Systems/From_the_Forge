//! Performance measurements (spec §18.11). Ignored by default; results are
//! recorded in `docs/compatibility.md` as **synthetic, local, one host**.
//!
//! `measure_growth_factor_k` imports a synthetic sms.db (the generator of
//! `common::import::synthetic_sms`, `OMA_PERF_MESSAGES` messages, default
//! 20,000) and reports the archive database's size against the staged
//! sms.db: the growth factor `K` of the space estimate (spec §12), which
//! the code uses as a provisional 3.0. It also reports the elapsed time,
//! the wall-clock time of each import phase (from the progress events), the
//! bytes of the FTS index's data blocks, the time of a quick and a full
//! verify of the result and, on Linux, the peak resident set size
//! (`VmHWM`).
//!
//! The generator's attachment rows name files that are not in the backup,
//! so no attachment file is copied or re-hashed: this measures the
//! database side only.
//!
//! ```sh
//! OMA_PERF_MESSAGES=100000 cargo test -p oma-archive --release --test perf -- --ignored --nocapture
//! ```
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::time::{Duration, Instant};

use common::import::*;
use common::*;
use oma_archive::space::GROWTH_K_TENTHS;
use oma_archive::{
    ArchiveCtx, ArchiveEvent, ArchiveSink, CancelToken, FixedClock, ImportOutcome, LineageChoice,
    ProceedWith, VerifyDepth, VerifyOutcome,
};

fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Wall-clock time from each phase's first event to the next phase's
/// first event (the last phase ends when [`PhaseTimer::finish`] is called).
#[derive(Default)]
struct PhaseTimer {
    current: Option<(String, Instant)>,
    done: Vec<(String, Duration)>,
}

impl PhaseTimer {
    fn enter(&mut self, name: String) {
        if self.current.as_ref().map(|(n, _)| n.as_str()) == Some(name.as_str()) {
            return;
        }
        self.finish();
        self.current = Some((name, Instant::now()));
    }

    fn finish(&mut self) {
        if let Some((n, t)) = self.current.take() {
            self.done.push((n, t.elapsed()));
        }
    }

    fn line(&self) -> String {
        self.done
            .iter()
            .map(|(n, d)| format!("{n}={:.2}", d.as_secs_f64()))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl ArchiveSink for PhaseTimer {
    fn event(&mut self, event: ArchiveEvent) {
        match event {
            ArchiveEvent::Import { phase, .. } => self.enter(format!("{phase:?}")),
            ArchiveEvent::Verify { phase, .. } => self.enter(format!("{phase:?}")),
            _ => {}
        }
    }
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
    let mut phases = PhaseTimer::default();
    let t1 = Instant::now();
    let outcome = {
        let cancel = CancelToken::new();
        let clock = FixedClock(MODEL_NOW_NS);
        let mut ctx = ArchiveCtx {
            cancel: &cancel,
            progress: &mut phases,
            clock: &clock,
        };
        a.import(
            &src.ob,
            &opts(1000),
            &mut ProceedWith(LineageChoice::New),
            &mut ctx,
        )
        .expect("import")
    };
    let imported = t1.elapsed();
    phases.finish();
    let s = match outcome {
        ImportOutcome::Completed(s) => s,
        other => panic!("import did not complete: {other:?}"),
    };
    let close = a.close();
    src.ob.close();
    let db = db_path(&ltmp, &id);
    let size = std::fs::metadata(&db).expect("db").len();
    let wal = std::fs::metadata(db.with_file_name("archive.sqlite-wal"))
        .map(|m| m.len())
        .unwrap_or(0);
    let fts_bytes: i64 = {
        let c = raw(&db);
        c.query_row(
            "SELECT coalesce(sum(length(block)), 0) FROM message_fts_data",
            [],
            |r| r.get(0),
        )
        .expect("fts size")
    };
    let staged = sms.len() as f64;
    let grown = (size + wal).saturating_sub(empty) as f64;
    println!(
        "PERF messages={messages} handles={handles} chats={chats} staged_bytes={} \
         archive_bytes={size} wal_bytes={wal} empty_archive_bytes={empty} fts_data_bytes={fts_bytes} \
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
    println!("PERF import phases (s): {}", phases.line());
    assert!(s.counts.messages.seen > 0);

    for depth in [VerifyDepth::Quick, VerifyDepth::Full] {
        let mut a = lib.open(&id).expect("open for verify");
        let mut vphases = PhaseTimer::default();
        let t = Instant::now();
        let report = {
            let cancel = CancelToken::new();
            let clock = FixedClock(MODEL_NOW_NS);
            let mut ctx = ArchiveCtx {
                cancel: &cancel,
                progress: &mut vphases,
                clock: &clock,
            };
            a.verify(depth, &mut ctx).expect("verify")
        };
        let took = t.elapsed();
        vphases.finish();
        let _ = a.close();
        println!(
            "PERF verify depth={depth:?} outcome={:?} verify_s={:.2} phases (s): {} peak_rss_kib={:?}",
            report.outcome,
            took.as_secs_f64(),
            vphases.line(),
            peak_rss_kib(),
        );
        assert_eq!(report.outcome, VerifyOutcome::Ok);
    }
}
