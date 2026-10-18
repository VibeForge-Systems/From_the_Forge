//! Disk space (spec §12, §18.5, D68): the estimate warns and refuses only
//! below the reserve plus the WAL allowance, with nothing written; a
//! shortfall before a write transaction stops cleanly and resumes; a tiny
//! `max_page_count` gives `SQLITE_FULL`, a rollback, a consistent cursor
//! and a resume; a limit below the current size is caught by the read-back.
//! Every probe is fake, so no test depends on the runner's free disk.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use common::import::*;
use common::*;
use oma_archive::hooks::{install, Faults};
use oma_archive::space::{reserve_for, wal_allowance, SpaceProbe};
use oma_archive::{
    ArchiveCtx, ArchiveEvent, ArchiveSink, CancelToken, FixedClock, ImportOutcome, ImportPhase,
    Library, LineageChoice, LineageId, StopReason,
};

/// The floor of an import with `opts(_)`: the reserve for an unknown
/// volume size plus the WAL allowance (two 32 MiB batches and the 64 MiB
/// WAL limit).
fn floor_unknown_total() -> u64 {
    reserve_for(None) + wal_allowance(32 << 20, 64 << 20)
}

const ENTRY: &str = "imp-dev1-v1-plain";

fn uninterrupted() -> serde_json::Value {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut a = lib.open(&id).expect("open");
    let src = open_entry(ENTRY);
    import(&mut a, &src, &opts(16), LineageChoice::New);
    drop(a);
    logical_snapshot(&db_path(&tmp, &id), Scope::Content)
}

fn session_state(db: &Path) -> (i64, i64) {
    read_only(db)
        .query_row(
            "SELECT state, (SELECT count(*) FROM import_issue i WHERE i.session_id = s.id \
                 AND i.kind = 2) FROM import_session s ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("session")
}

/// Continue the newest session with plenty of space and compare.
fn resume_and_compare(
    lib: &Library,
    id: &oma_archive::ArchiveId,
    db: &Path,
    want: &serde_json::Value,
) {
    let lineage: i64 = read_only(db)
        .query_row("SELECT max(id) FROM lineage", [], |r| r.get(0))
        .expect("lineage");
    let mut a = lib.open(id).expect("open");
    let src = open_entry(ENTRY);
    let s = import(
        &mut a,
        &src,
        &opts(16),
        LineageChoice::Existing(LineageId::new(lineage)),
    );
    drop(a);
    assert!(s.resumed);
    assert_consistent(db);
    assert_store_consistent(db, true);
    assert_eq!(&logical_snapshot(db, Scope::Content), want);
}

/// Below the floor the import is refused and nothing is written; above it
/// an estimate larger than the space is a warning only; unknown space is
/// neither.
#[test]
fn the_estimate_warns_and_refuses_only_below_the_floor() {
    let floor = floor_unknown_total();
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let before = data_snapshot(&tmp, &id);
    let low = lib.with_space_probe(fake_space(Some(floor - 1), None));
    let mut a = low.open(&id).expect("open");
    let src = open_entry(ENTRY);
    let o = import_with(
        &mut a,
        &src,
        &opts(16),
        LineageChoice::New,
        &CancelToken::new(),
    )
    .expect("import");
    drop(a);
    let ImportOutcome::InsufficientSpace(e) = o else {
        panic!("not refused: {o:?}");
    };
    assert!(e.refuses() && e.warns());
    assert_eq!(e.floor(), floor);
    assert_eq!(e.available, Some(floor - 1));
    assert!(e.files_to_copy == 6 && e.bytes_to_copy == 161, "{e:?}");
    assert_eq!(data_snapshot(&tmp, &id), before, "the refusal wrote");

    // Unknown space: neither a warning nor a refusal; the import runs.
    let unknown = low.with_space_probe(fake_space(None, None));
    let mut a = unknown.open(&id).expect("open");
    let s = import(&mut a, &src, &opts(16), LineageChoice::New);
    drop(a);
    assert_eq!(s.estimate.available, None);
    assert!(!s.estimate.warns() && !s.estimate.refuses());

    // Just above the floor: a warning, not a refusal. The guards then stop
    // the import safely when the first batch has no room.
    let (tmp2, lib2) = library();
    let id2 = lib2.create(None).expect("create");
    let tight = lib2.with_space_probe(fake_space(Some(floor + 1), None));
    let mut a = tight.open(&id2).expect("open");
    let o = import_with(
        &mut a,
        &src,
        &opts(16),
        LineageChoice::New,
        &CancelToken::new(),
    )
    .expect("import");
    drop(a);
    let ImportOutcome::Stopped { reason, summary } = o else {
        panic!("not stopped: {o:?}");
    };
    assert_eq!(reason, StopReason::InsufficientSpace);
    assert!(summary.estimate.warns() && !summary.estimate.refuses());
    let db2 = db_path(&tmp2, &id2);
    assert_consistent(&db2);
    let (state, _) = session_state(&db2);
    assert!(state == 6 || state == 0, "{state}");
}

/// Free space that reads low once a flag is set.
struct Scripted {
    low: AtomicBool,
}

impl SpaceProbe for Scripted {
    fn available(&self, _path: &Path) -> io::Result<u64> {
        Ok(if self.low.load(Ordering::SeqCst) {
            floor_unknown_total() - 1
        } else {
            TEST_AVAILABLE
        })
    }
    fn total(&self, _path: &Path) -> io::Result<u64> {
        Err(io::Error::other("unknown"))
    }
}

/// Sets the probe low once the message pass committed a batch.
struct LowInMessages(Arc<Scripted>);

impl ArchiveSink for LowInMessages {
    fn event(&mut self, event: ArchiveEvent) {
        if let ArchiveEvent::Import {
            phase: ImportPhase::ImportingMessages,
            done,
            ..
        } = event
        {
            if done > 0 {
                self.0.low.store(true, Ordering::SeqCst);
            }
        }
    }
}

/// A shortfall found before a write transaction stops the import (state 6,
/// `insufficient_space`) with that transaction unwritten; the resume with
/// space reaches the uninterrupted result.
#[test]
fn a_shortfall_before_a_write_stops_cleanly_and_resumes() {
    let want = uninterrupted();
    let probe = Arc::new(Scripted {
        low: AtomicBool::new(false),
    });
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let scripted = lib.with_space_probe(probe.clone());
    let mut a = scripted.open(&id).expect("open");
    let src = open_entry(ENTRY);
    let token = CancelToken::new();
    let clock = FixedClock(MODEL_NOW_NS);
    let mut sink = LowInMessages(probe.clone());
    let mut ctx = ArchiveCtx {
        cancel: &token,
        progress: &mut sink,
        clock: &clock,
    };
    let o = a
        .import(
            &src.ob,
            &opts(16),
            &mut oma_archive::ProceedWith(LineageChoice::New),
            &mut ctx,
        )
        .expect("import");
    drop(a);
    let ImportOutcome::Stopped { reason, summary } = o else {
        panic!("not stopped: {o:?}");
    };
    assert_eq!(reason, StopReason::InsufficientSpace);
    let db = db_path(&tmp, &id);
    assert_eq!(session_state(&db), (6, 1));
    assert_consistent(&db);
    // Exactly the first message batch committed: its cursor says so.
    let (rows, done): (i64, i64) = read_only(&db)
        .query_row(
            "SELECT rows_done, done FROM import_cursor WHERE pass = 3",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("cursor");
    assert_eq!((rows, done), (16, 0));
    assert_eq!(summary.counts.messages.seen, 16);
    probe.low.store(false, Ordering::SeqCst);
    resume_and_compare(&scripted, &id, &db, &want);
}

/// A page limit with little room: `SQLITE_FULL`, the batch rolls back, the
/// cursor stays at the last committed batch, and the resume reaches the
/// uninterrupted result. With no room at all the terminal bookkeeping may
/// fail too; the session then stays resumable as running (interrupted at
/// the next open).
#[test]
fn a_tiny_page_limit_stops_with_sqlite_full_and_resumes() {
    let want = uninterrupted();
    for room in [0i64, 1] {
        let (tmp, lib) = library();
        let id = lib.create(None).expect("create");
        let db = db_path(&tmp, &id);
        {
            let _g = install(Faults {
                max_page_room: Some(room),
                ..Faults::default()
            });
            let mut a = lib.open(&id).expect("open");
            let src = open_entry(ENTRY);
            let o = import_with(
                &mut a,
                &src,
                &opts(16),
                LineageChoice::New,
                &CancelToken::new(),
            )
            .expect("import");
            drop(a);
            let ImportOutcome::Stopped { reason, .. } = o else {
                panic!("room {room}: not stopped: {o:?}");
            };
            assert_eq!(reason, StopReason::InsufficientSpace);
        }
        assert_consistent(&db);
        let (state, _) = session_state(&db);
        assert!(state == 6 || state == 0, "room {room}: state {state}");
        // Every cursor names the last row of a committed batch.
        let c = read_only(&db);
        let bad: i64 = c
            .query_row(
                "SELECT count(*) FROM import_cursor c WHERE c.rows_done <> \
                     (SELECT count(*) FROM import_rowid_map m WHERE m.session_id = c.session_id \
                      AND m.src = CASE c.pass WHEN 6 THEN 4 ELSE c.pass END) \
                 AND c.pass IN (1, 2, 3, 6)",
                [],
                |r| r.get(0),
            )
            .expect("cursors");
        assert_eq!(bad, 0, "room {room}");
        drop(c);
        resume_and_compare(&lib, &id, &db, &want);
    }
}

/// A limit below the current size: SQLite keeps the current size, the
/// read-back differs from the request, and the import stops before
/// writing its first batch.
#[test]
fn a_page_limit_below_the_current_size_is_caught_by_the_read_back() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    {
        let _g = install(Faults {
            max_page_room: Some(-5),
            ..Faults::default()
        });
        let mut a = lib.open(&id).expect("open");
        let src = open_entry(ENTRY);
        let o = import_with(
            &mut a,
            &src,
            &opts(16),
            LineageChoice::New,
            &CancelToken::new(),
        )
        .expect("import");
        drop(a);
        assert!(
            matches!(
                o,
                ImportOutcome::Stopped {
                    reason: StopReason::InsufficientSpace,
                    ..
                }
            ),
            "{o:?}"
        );
    }
    let c = read_only(&db);
    let handles: i64 = c
        .query_row("SELECT count(*) FROM handle", [], |r| r.get(0))
        .expect("handles");
    assert_eq!(handles, 0, "the first batch was written");
    drop(c);
    assert_eq!(session_state(&db), (6, 1));
}
