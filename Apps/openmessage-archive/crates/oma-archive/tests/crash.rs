//! Crash by kill (spec §18.4, D85, C3.7).
//!
//! The parent re-executes this test binary with `OMA_TEST_FAULT=<point>:<n>`.
//! The child (`crash_child`, a no-op without the variable, so ordinary and
//! `--ignored` runs are unaffected) is built with the `fault-injection`
//! feature: at the n-th hit of the point it prints `FAULT <point> <n>` and
//! blocks on stdin. The parent then kills it with `Child::kill`, a real
//! crash by kill on both operating systems. Clocks are fixed and the batch
//! time budget is off, so batch boundaries are deterministic.
//!
//! After every kill the parent opens the archive read-only first, checks
//! `integrity_check`, `foreign_key_check` and the FTS index, continues the
//! import, and compares the result with an uninterrupted import.
//!
//! What this shows: consistency after a **process kill**. A kill leaves the
//! kernel's page cache intact, so it does not exercise fsync ordering;
//! power loss is not tested (§9.4).
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use common::import::*;
use common::*;
use oma_archive::hooks::{arm, count, hits, FaultPoint};
use oma_archive::{ArchiveEntry, ArchiveId, CancelToken, Library, LineageChoice, LineageId};

const ENV_FAULT: &str = "OMA_TEST_FAULT";
const ENV_SCENARIO: &str = "OMA_TEST_SCENARIO";
const ENV_LIBRARY: &str = "OMA_TEST_LIBRARY";
const ENV_ARCHIVE: &str = "OMA_TEST_ARCHIVE";
const ENV_BACKUP: &str = "OMA_TEST_BACKUP";
const ENV_LINEAGE: &str = "OMA_TEST_LINEAGE";

/// How long a child may take to reach its fault point or finish.
const CHILD_TIMEOUT: Duration = Duration::from_secs(120);

/// The child side. Does nothing unless the parent set `OMA_TEST_FAULT`.
#[test]
fn crash_child() {
    let Ok(spec) = std::env::var(ENV_FAULT) else {
        return;
    };
    let (name, n) = spec.split_once(':').expect("<point>:<n>");
    let point = FaultPoint::from_name(name).expect("known point");
    let nth: u32 = n.parse().expect("n");
    let _guard = arm(
        point,
        nth,
        Box::new(|p, n| {
            let mut out = std::io::stdout();
            // A line of its own: libtest's "test crash_child ... " has no
            // newline yet.
            let _ = writeln!(out, "\nFAULT {} {n}", p.name());
            let _ = out.flush();
            // Wait to be killed; exit if the parent goes away.
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            std::process::exit(3);
        }),
    );
    let library = PathBuf::from(std::env::var(ENV_LIBRARY).expect("library"));
    let lib = Library::new(&library).expect("library");
    match std::env::var(ENV_SCENARIO).expect("scenario").as_str() {
        "create" => {
            lib.create(None).expect("create");
        }
        "import" => {
            let id = ArchiveId::parse(&std::env::var(ENV_ARCHIVE).expect("id")).expect("id");
            let backup = PathBuf::from(std::env::var(ENV_BACKUP).expect("backup"));
            let lineage = match std::env::var(ENV_LINEAGE).expect("lineage").as_str() {
                "new" => LineageChoice::New,
                n => LineageChoice::Existing(LineageId::new(n.parse().expect("lineage"))),
            };
            let mut a = lib.open(&id).expect("open");
            let src = open_dir(&backup, None);
            import_with(&mut a, &src, &opts(16), lineage, &CancelToken::new()).expect("import");
            drop(a);
            src.ob.close();
        }
        other => panic!("unknown scenario {other}"),
    }
    println!("\nDONE");
}

#[derive(Debug, PartialEq, Eq)]
enum Child {
    /// The child reached the fault point and was killed there.
    Killed,
    /// The child finished without reaching it.
    Finished,
}

fn run_child(envs: &[(&str, String)]) -> Child {
    let exe = std::env::current_exe().expect("test binary");
    let mut cmd = Command::new(exe);
    cmd.args([
        "crash_child",
        "--exact",
        "--nocapture",
        "--test-threads",
        "1",
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn");
    let stdout = child.stdout.take().expect("stdout");
    // Lines arrive through a channel so a child that hangs fails the test
    // instead of blocking it.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut done = false;
    loop {
        match rx.recv_timeout(CHILD_TIMEOUT) {
            Ok(line) if line.starts_with("FAULT ") => {
                child.kill().expect("kill");
                child.wait().expect("wait");
                return Child::Killed;
            }
            Ok(line) => done |= line == "DONE",
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the child neither reached the point nor finished");
            }
        }
    }
    let status = child.wait().expect("wait");
    assert!(status.success() && done, "the child failed: {status:?}");
    Child::Finished
}

fn lineage_arg(db: &Path) -> String {
    let l: Option<i64> = read_only(db)
        .query_row("SELECT max(id) FROM lineage", [], |r| r.get(0))
        .expect("lineage");
    l.map_or_else(|| "new".to_owned(), |l| l.to_string())
}

fn lineage_choice(arg: &str) -> LineageChoice {
    match arg {
        "new" => LineageChoice::New,
        n => LineageChoice::Existing(LineageId::new(n.parse().expect("n"))),
    }
}

/// The synthetic source every import crash uses: enough handles and chats
/// for several 16-row batches per pass.
struct Fixture {
    _tmp: tempfile::TempDir,
    backup: PathBuf,
    want: serde_json::Value,
    hits: Vec<(FaultPoint, u32)>,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let backup = backup_with_sms(&tmp.path().join("backup"), &synthetic_sms(11, 70, 45));
    let (ltmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut a = lib.open(&id).expect("open");
    let src = open_dir(&backup, None);
    let guard = count();
    import(&mut a, &src, &opts(16), LineageChoice::New);
    let hits = FaultPoint::ALL.iter().map(|p| (*p, hits(*p))).collect();
    drop(guard);
    drop(a);
    src.ob.close();
    let want = logical_snapshot(&db_path(&ltmp, &id), Scope::WithSessions);
    Fixture {
        _tmp: tmp,
        backup,
        want,
        hits,
    }
}

/// Kill an import at the n-th hit of `point` (after `before`, a list of
/// earlier kills), then check, continue and compare.
fn kill_import_and_resume(fx: &Fixture, kills: &[(FaultPoint, u32)]) {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    for (point, n) in kills {
        let envs = [
            (ENV_FAULT, format!("{}:{n}", point.name())),
            (ENV_SCENARIO, "import".to_owned()),
            (ENV_LIBRARY, tmp.path().display().to_string()),
            (ENV_ARCHIVE, id.as_str().to_owned()),
            (ENV_BACKUP, fx.backup.display().to_string()),
            (ENV_LINEAGE, lineage_arg(&db)),
        ];
        assert_eq!(
            run_child(&envs),
            Child::Killed,
            "{} {n} not reached",
            point.name()
        );
        // Read-only first: the archive opens, then the file checks.
        let a = lib.open(&id).expect("open after a kill");
        drop(a);
        assert_consistent(&db);
    }
    let mut a = lib.open(&id).expect("open");
    let src = open_dir(&fx.backup, None);
    import(&mut a, &src, &opts(16), lineage_choice(&lineage_arg(&db)));
    drop(a);
    src.ob.close();
    assert_consistent(&db);
    assert_eq!(
        logical_snapshot(&db, Scope::WithSessions),
        fx.want,
        "after kills at {:?}",
        kills
            .iter()
            .map(|(p, n)| format!("{} {n}", p.name()))
            .collect::<Vec<_>>()
    );
}

/// The points and hit numbers each run kills at. Windows runs a subset
/// (spec §18.12): the batch commit points.
fn plan(fx: &Fixture) -> Vec<(FaultPoint, u32)> {
    let hits = |p: FaultPoint| fx.hits.iter().find(|(q, _)| *q == p).map_or(0, |(_, n)| *n);
    let mut out = Vec::new();
    let batch_points: &[FaultPoint] =
        &[FaultPoint::BatchBeforeCommit, FaultPoint::BatchAfterCommit];
    for p in batch_points {
        let n = hits(*p);
        assert!(n >= 6, "{} hit {n} times", p.name());
        if cfg!(windows) {
            out.extend([(*p, 1), (*p, n)]);
        } else {
            out.extend((1..=n).map(|i| (*p, i)));
        }
    }
    if !cfg!(windows) {
        let records = hits(FaultPoint::Record);
        assert_eq!(records, 115);
        out.extend([1, 17, 58, records].map(|n| (FaultPoint::Record, n)));
        for p in [
            FaultPoint::SessionCommitted,
            FaultPoint::RecoverySwept,
            FaultPoint::RecoveryReconciled,
            FaultPoint::RecoveryMarked,
        ] {
            assert_eq!(hits(p), 1, "{}", p.name());
            out.push((p, 1));
        }
    }
    out
}

#[test]
fn a_kill_at_any_import_fault_point_resumes_to_the_uninterrupted_result() {
    let fx = fixture();
    for kill in plan(&fx) {
        kill_import_and_resume(&fx, &[kill]);
    }
}

#[test]
fn kills_in_a_row_including_during_recovery_still_resume() {
    let fx = fixture();
    // A batch committed, then the next run dies while recovering, then
    // again mid-pass.
    kill_import_and_resume(
        &fx,
        &[
            (FaultPoint::BatchAfterCommit, 2),
            (FaultPoint::RecoveryMarked, 1),
            (FaultPoint::BatchBeforeCommit, 3),
        ],
    );
}

#[test]
fn a_kill_during_creation_leaves_no_half_archive() {
    for point in [
        FaultPoint::CreateBeforeRename,
        FaultPoint::CreateAfterRename,
    ] {
        let (tmp, lib) = library();
        let envs = [
            (ENV_FAULT, format!("{}:1", point.name())),
            (ENV_SCENARIO, "create".to_owned()),
            (ENV_LIBRARY, tmp.path().display().to_string()),
        ];
        assert_eq!(run_child(&envs), Child::Killed, "{}", point.name());
        let listed = lib.list().expect("list");
        match point {
            FaultPoint::CreateBeforeRename => {
                // Only the creation directory exists; it is never listed,
                // and the sweep removes it.
                assert!(listed.is_empty(), "{listed:?}");
                let report = lib.sweep_older_than(Duration::ZERO);
                assert_eq!(report.removed, 1, "{report:?}");
                assert_eq!(std::fs::read_dir(root(&tmp)).expect("dir").count(), 0);
            }
            _ => {
                let [ArchiveEntry::Ready(info)] = listed.as_slice() else {
                    panic!("{listed:?}");
                };
                let a = lib.open(&info.id).expect("open");
                drop(a);
                assert_consistent(&db_path(&tmp, &info.id));
                assert_eq!(lib.sweep_older_than(Duration::ZERO).removed, 0);
            }
        }
    }
}
