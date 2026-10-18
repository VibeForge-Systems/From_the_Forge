//! Cancellation, resume and in-process crashes of the import core (spec
//! §8.5, §8.6, §18.4).
//!
//! - A cancel at **every** record commits the records completed before it,
//!   with the importer's own cursor, and a resume reaches the same archive
//!   (content and session bookkeeping) as an uninterrupted import.
//! - The property test stops a synthetic import with 16-row batches at
//!   random fault points (a panic unwinds through the open batch, which
//!   rolls back as a crash would), with `i64::MIN` and `i64::MAX` rowids,
//!   duplicated keys and same-fingerprint rows on both sides of the batch
//!   boundaries. The result equals one uninterrupted run.
//!
//! Process kills are in `crash.rs`.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

use common::import::*;
use common::*;
use oma_archive::hooks::{arm, count, hits, FaultPoint};
use oma_archive::{CancelToken, CoreOutcome, LineageChoice};

/// The source rowids of each pass of a finished session, in pass order.
fn pass_rowids(db: &std::path::Path, session: i64) -> Vec<Vec<i64>> {
    let c = read_only(db);
    [1i64, 2]
        .iter()
        .map(|src| {
            c.prepare(
                "SELECT src_rowid FROM import_rowid_map WHERE session_id = ?1 AND src = ?2 \
                 ORDER BY src_rowid",
            )
            .expect("prepare")
            .query_map(rusqlite::params![session, src], |r| r.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows")
        })
        .collect()
}

fn cursors(db: &std::path::Path, session: i64) -> BTreeMap<i64, (Option<i64>, i64, i64)> {
    let c = read_only(db);
    let mut stmt = c
        .prepare(
            "SELECT pass, last_rowid, rows_done, done FROM import_cursor WHERE session_id = ?1",
        )
        .expect("prepare");
    let out = stmt
        .query_map([session], |r| {
            Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
        })
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    out
}

#[test]
fn a_cancel_at_every_record_commits_what_completed_and_resumes_to_the_same_archive() {
    let entry = "imp-keys-hostile-plain";
    // The uninterrupted import, counting the records it reads.
    let (btmp, blib) = library();
    let bid = blib.create(None).expect("create");
    let mut a = blib.open(&bid).expect("open");
    let src = open_entry(entry);
    let guard = count();
    let base = import(&mut a, &src, &opts(16), LineageChoice::New);
    let records = hits(FaultPoint::Record);
    drop(guard);
    drop(a);
    src.ob.close();
    let bdb = db_path(&btmp, &bid);
    let want = logical_snapshot(&bdb, Scope::WithSessions);
    let order = pass_rowids(&bdb, base.session);
    let total: usize = order.iter().map(Vec::len).sum();
    assert_eq!(
        usize::try_from(records).expect("n"),
        total,
        "one hit per record"
    );
    assert!(total >= 10, "{total}");

    for k in 1..=records {
        let (tmp, lib) = library();
        let id = lib.create(None).expect("create");
        let mut a = lib.open(&id).expect("open");
        let src = open_entry(entry);
        let token = CancelToken::new();
        let t = token.clone();
        let guard = arm(FaultPoint::Record, k, Box::new(move |_, _| t.cancel()));
        let out = import_with(&mut a, &src, &opts(16), LineageChoice::New, &token).expect("import");
        drop(guard);
        let CoreOutcome::Cancelled(Some(s)) = out else {
            panic!("record {k}: not cancelled: {out:?}");
        };
        let done = u64::from(k) - 1;
        assert_eq!(
            s.counts.handles.seen + s.counts.chats.seen,
            done,
            "record {k}"
        );
        drop(a);
        let db = db_path(&tmp, &id);
        // The cursor is the last completed record of the pass it stopped in.
        let cur = cursors(&db, s.session);
        let n_handles = u64::try_from(order[0].len()).expect("n");
        let (pass, within) = if done <= n_handles {
            (1, done)
        } else {
            (2, done - n_handles)
        };
        if within > 0 {
            let at = usize::try_from(within - 1).expect("i");
            let (last, rows, finished) = cur[&pass];
            assert_eq!(
                last,
                Some(order[usize::try_from(pass - 1).expect("p")][at]),
                "record {k}"
            );
            assert_eq!(rows, i64::try_from(within).expect("n"));
            assert_eq!(finished, i64::from(pass == 1 && within == n_handles));
        }
        if pass == 2 {
            assert_eq!(cur[&1].1, i64::try_from(n_handles).expect("n"));
            assert_eq!(cur[&1].2, 1);
        }
        let state: i64 = read_only(&db)
            .query_row("SELECT state FROM import_session", [], |r| r.get(0))
            .expect("state");
        assert_eq!(state, 2, "cancelled");
        assert_consistent(&db);

        // Continue.
        let mut a = lib.open(&id).expect("open");
        let s2 = import(&mut a, &src, &opts(16), LineageChoice::Existing(s.lineage));
        assert!(s2.resumed, "record {k}");
        assert_eq!(s2.session, s.session);
        assert_eq!(s2.counts.handles, base.counts.handles, "record {k}");
        assert_eq!(s2.counts.chats, base.counts.chats, "record {k}");
        assert_eq!(s2.counts.resumes, 1);
        drop(a);
        src.ob.close();
        assert_eq!(
            logical_snapshot(&db, Scope::WithSessions),
            want,
            "record {k}"
        );
    }
}

/// One crash of an in-process import: a panic at the `nth` hit of `point`.
fn crash_once(
    lib: &oma_archive::Library,
    id: &oma_archive::ArchiveId,
    backup: &std::path::Path,
    lineage: LineageChoice,
    point: FaultPoint,
    nth: u32,
) -> Option<oma_archive::ImportSummary> {
    let mut a = lib.open(id).expect("open");
    let src = open_dir(backup, None);
    let guard = arm(
        point,
        nth,
        Box::new(|p, n| panic!("injected crash at {} {n}", p.name())),
    );
    let r = catch_unwind(AssertUnwindSafe(|| {
        import_with(&mut a, &src, &opts(16), lineage, &CancelToken::new())
    }));
    drop(guard);
    drop(a);
    src.ob.close();
    match r {
        Ok(Ok(CoreOutcome::PassesDone(s))) => Some(s),
        Ok(other) => panic!("unexpected outcome {other:?}"),
        Err(_) => None,
    }
}

#[test]
fn random_crashes_at_batch_boundaries_never_change_the_result() {
    let sms = synthetic_sms(7, 70, 45);
    let btmp = tempfile::tempdir().expect("tempdir");
    let backup = backup_with_sms(&btmp.path().join("backup"), &sms);

    // The uninterrupted run, and how often each point is hit.
    let (tmp0, lib0) = library();
    let id0 = lib0.create(None).expect("create");
    let mut a = lib0.open(&id0).expect("open");
    let src = open_dir(&backup, None);
    let guard = count();
    let base = import(&mut a, &src, &opts(16), LineageChoice::New);
    let max: BTreeMap<FaultPoint, u32> = [
        FaultPoint::Record,
        FaultPoint::BatchBeforeCommit,
        FaultPoint::BatchAfterCommit,
        FaultPoint::SessionCommitted,
    ]
    .into_iter()
    .map(|p| (p, hits(p)))
    .collect();
    drop(guard);
    drop(a);
    src.ob.close();
    assert_eq!(base.counts.handles.seen, 70);
    assert_eq!(base.counts.chats.seen, 45);
    assert!(
        base.counts.handles.unkeyed >= 10,
        "{:?}",
        base.counts.handles
    );
    assert!(base.counts.chats.unkeyed >= 8, "{:?}", base.counts.chats);
    assert!(max[&FaultPoint::BatchAfterCommit] >= 8, "{max:?}");
    let db0 = db_path(&tmp0, &id0);
    let want = logical_snapshot(&db0, Scope::WithSessions);
    // Both extreme rowids were imported.
    let c = read_only(&db0);
    for extreme in [i64::MIN, i64::MAX] {
        let n: i64 = c
            .query_row(
                "SELECT count(*) FROM import_rowid_map WHERE src_rowid = ?1",
                [extreme],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(n, 2, "{extreme}");
    }
    drop(c);

    let mut rng = Rng::new(0x5eed_0f00d);
    let points: Vec<FaultPoint> = max.keys().copied().collect();
    let runs = 12;
    let mut total_crashes = 0;
    for run in 0..runs {
        let (tmp, lib) = library();
        let id = lib.create(None).expect("create");
        let mut lineage = LineageChoice::New;
        let mut crashes = 0;
        let done = loop {
            assert!(crashes < 40, "run {run}: no progress");
            let point = points[usize::try_from(rng.below(points.len() as u64)).expect("i")];
            let nth = 1 + u32::try_from(rng.below(u64::from(max[&point]))).expect("n");
            match crash_once(&lib, &id, &backup, lineage, point, nth) {
                Some(s) => break s,
                None => {
                    crashes += 1;
                    let db = db_path(&tmp, &id);
                    assert_consistent(&db);
                    // A crash after the session committed leaves its
                    // lineage; continuing needs the same choice (D80).
                    let l: Option<i64> = read_only(&db)
                        .query_row("SELECT max(id) FROM lineage", [], |r| r.get(0))
                        .expect("lineage");
                    if let Some(l) = l {
                        lineage = LineageChoice::Existing(oma_archive::LineageId::new(l));
                    }
                    // Mostly crash again; sometimes let the import finish.
                    if rng.below(4) == 0 {
                        let mut a = lib.open(&id).expect("open");
                        let src = open_dir(&backup, None);
                        let s = import(&mut a, &src, &opts(16), lineage);
                        drop(a);
                        src.ob.close();
                        break s;
                    }
                }
            }
        };
        total_crashes += crashes;
        assert_eq!(done.counts.handles, base.counts.handles, "run {run}");
        assert_eq!(done.counts.chats, base.counts.chats, "run {run}");
        let db = db_path(&tmp, &id);
        assert_consistent(&db);
        assert_eq!(
            logical_snapshot(&db, Scope::WithSessions),
            want,
            "run {run} after {crashes} crashes"
        );
    }
    assert!(
        total_crashes >= runs,
        "{total_crashes} crashes in {runs} runs"
    );
}
