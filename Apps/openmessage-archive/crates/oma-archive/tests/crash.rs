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
//! `integrity_check`, `foreign_key_check` and the FTS index **on a copy**
//! of the database, its `-wal` and its `-shm` (a read-write check would
//! checkpoint and delete the `-wal`), asserts that the `-wal` the kill left
//! is still there unchanged, continues the import against those files, and
//! compares the result with an uninterrupted import. Each child stages
//! under a directory the parent removes (a killed child never cleans up).
//!
//! Every point in `FaultPoint::ALL` is killed at (a test fails when one
//! is not): the batch, record, session and recovery points by the import
//! plan, the attachment store's points (partial created, copying, synced,
//! intents committed, placed, directories flushed) by the store plan on a
//! catalogue entry with attachment files, the terminal commit by a child
//! that cancels during the chat pass and by one at the completion, the
//! gate (nothing written), finalize before completion (the statistics and
//! each cleanup chunk: the session continues), after completion (the
//! terminal commit, each FTS merge step, the checkpoint and both
//! provenance points: the session stays completed and the next write open
//! sweeps the `.tmp`), a migration step and its commit (version 1 stays),
//! and the two creation points by the creation test. After every kill, no
//! `blob` row references a missing file; after the resume, no store file
//! is unreferenced and no intent is left.
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
use oma_archive::hooks::{arm, count, hits, install, FaultPoint, Faults, TestSchema};
use oma_archive::{
    ArchiveCtx, ArchiveEntry, ArchiveEvent, ArchiveId, ArchiveSink, CancelToken, FixedClock,
    ImportPhase, Library, LineageChoice, LineageId, NoProgress, Recovery,
};

/// Cancels `token` once the chat pass has committed a batch.
struct CancelInChats(CancelToken);

impl ArchiveSink for CancelInChats {
    fn event(&mut self, event: ArchiveEvent) {
        if let ArchiveEvent::Import {
            phase: ImportPhase::ImportingChats,
            done,
            ..
        } = event
        {
            if done > 0 {
                self.0.cancel();
            }
        }
    }
}

const ENV_FAULT: &str = "OMA_TEST_FAULT";
const ENV_SCENARIO: &str = "OMA_TEST_SCENARIO";
const ENV_LIBRARY: &str = "OMA_TEST_LIBRARY";
const ENV_ARCHIVE: &str = "OMA_TEST_ARCHIVE";
const ENV_BACKUP: &str = "OMA_TEST_BACKUP";
const ENV_LINEAGE: &str = "OMA_TEST_LINEAGE";
/// Set: the child cancels its import once the chat pass committed a batch.
const ENV_CANCEL: &str = "OMA_TEST_CANCEL_IN_CHATS";

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
    // The parent's fake space probe, so no child depends on the runner's
    // free disk (spec §12).
    let lib = Library::new(&library)
        .expect("library")
        .with_space_probe(fake_space(Some(TEST_AVAILABLE), Some(2 * TEST_AVAILABLE)));
    match std::env::var(ENV_SCENARIO).expect("scenario").as_str() {
        "create" => {
            lib.create(None).expect("create");
        }
        "migrate" => {
            let id = ArchiveId::parse(&std::env::var(ENV_ARCHIVE).expect("id")).expect("id");
            let _faults = install(Faults {
                schema: Some(TestSchema::V2),
                ..Faults::default()
            });
            let mut a = lib.open(&id).expect("open");
            a.migrate(&mut ArchiveCtx {
                cancel: &CancelToken::new(),
                progress: &mut NoProgress,
                clock: &FixedClock(MODEL_NOW_NS),
            })
            .expect("migrate");
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
            let token = CancelToken::new();
            if std::env::var_os(ENV_CANCEL).is_some() {
                let clock = FixedClock(MODEL_NOW_NS);
                let mut sink = CancelInChats(token.clone());
                let mut ctx = ArchiveCtx {
                    cancel: &token,
                    progress: &mut sink,
                    clock: &clock,
                };
                a.import(
                    &src.ob,
                    &opts(16),
                    &mut oma_archive::ProceedWith(lineage),
                    &mut ctx,
                )
                .expect("import");
            } else {
                import_with(&mut a, &src, &opts(16), lineage, &token).expect("import");
            }
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

/// The synthetic source every import crash uses: enough handles, chats,
/// messages and links for several 16-row batches in most passes.
struct Fixture {
    _tmp: tempfile::TempDir,
    backup: PathBuf,
    want: serde_json::Value,
    hits: Vec<(FaultPoint, u32)>,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let backup = backup_with_sms(&tmp.path().join("backup"), &synthetic_sms(11, 70, 45, 40));
    fixture_for(tmp, backup)
}

/// A catalogue entry with attachment files, opened in place (staging is
/// elsewhere; the catalogue is never written).
fn store_fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    fixture_for(tmp, imports_root().join("imp-dev1-v1-plain").join(UDID))
}

fn fixture_for(tmp: tempfile::TempDir, backup: PathBuf) -> Fixture {
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
/// earlier kills), then check, continue and compare. With `cancel`, each
/// child cancels once the chat pass committed a batch.
fn kill_import_and_resume(fx: &Fixture, kills: &[(FaultPoint, u32)], cancel: bool) {
    let (tmp, lib) = library();
    let staging = tempfile::tempdir().expect("staging");
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    for (i, (point, n)) in kills.iter().enumerate() {
        let mut envs = vec![
            (ENV_FAULT, format!("{}:{n}", point.name())),
            (ENV_SCENARIO, "import".to_owned()),
            (ENV_LIBRARY, tmp.path().display().to_string()),
            (ENV_ARCHIVE, id.as_str().to_owned()),
            (ENV_BACKUP, fx.backup.display().to_string()),
            (ENV_LINEAGE, lineage_arg(&db)),
            (ENV_STAGING, staging.path().display().to_string()),
        ];
        if cancel {
            envs.push((ENV_CANCEL, "1".to_owned()));
        }
        assert_eq!(
            run_child(&envs),
            Child::Killed,
            "{} {n} not reached",
            point.name()
        );
        // The killed child's staging is left in the parent's directory,
        // which removes it; nothing else stages there.
        let leftovers = std::fs::read_dir(staging.path()).expect("staging").count();
        assert!(leftovers <= i + 1, "{leftovers} staging directories");
        // The kill left a -wal; nothing below may change it.
        let wal = wal_state(&db);
        assert!(
            wal.is_some(),
            "no -wal after a kill at {} {n}",
            point.name()
        );
        // Read-only first: the archive opens, then the file checks on a
        // copy.
        let a = lib.open(&id).expect("open after a kill");
        drop(a);
        assert_consistent_copy(&db);
        if cancel {
            let state: i64 = read_only(&db)
                .query_row("SELECT max(state) FROM import_session", [], |r| r.get(0))
                .expect("state");
            assert_eq!(state, 2, "the cancel committed before the kill");
        }
        assert_eq!(wal_state(&db), wal, "the checks changed the -wal");
    }
    // The resume runs against the hot -wal (and the dead child's -shm).
    assert!(wal_state(&db).is_some());
    let mut a = lib.open(&id).expect("open");
    let src = open_dir(&fx.backup, None);
    import(&mut a, &src, &opts(16), lineage_choice(&lineage_arg(&db)));
    drop(a);
    src.ob.close();
    assert_consistent(&db);
    assert_store_consistent(&db, true);
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
        // 70 handles, 45 chats, 40 messages, 63 chat-handle links, 43
        // chat-message links, 13 attachments, 15 message-attachment links
        // and 3 Recently Deleted rows: a kill in every pass.
        assert_eq!(records, 292);
        out.extend(
            [1, 17, 58, 131, 170, 210, 230, 260, 280, records].map(|n| (FaultPoint::Record, n)),
        );
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
        kill_import_and_resume(&fx, &[kill], false);
    }
}

/// The store's points (spec §18.4): after creating a partial file, during
/// the copy, after `sync_all`, after the intent commit, after a rename and
/// after the directory flush. Windows runs the intent and rename points
/// (spec §18.12).
#[test]
fn a_kill_at_any_store_fault_point_resumes_to_the_uninterrupted_result() {
    let fx = store_fixture();
    let hits = |p: FaultPoint| fx.hits.iter().find(|(q, _)| *q == p).map_or(0, |(_, n)| *n);
    let points: &[FaultPoint] = if cfg!(windows) {
        &[
            FaultPoint::AttachmentIntentCommitted,
            FaultPoint::AttachmentPlaced,
        ]
    } else {
        &[
            FaultPoint::AttachmentPartialCreated,
            FaultPoint::AttachmentCopying,
            FaultPoint::AttachmentSynced,
            FaultPoint::AttachmentIntentCommitted,
            FaultPoint::AttachmentPlaced,
            FaultPoint::AttachmentDirSynced,
        ]
    };
    for p in points {
        let n = hits(*p);
        assert!(n >= 1, "{} never hit", p.name());
        let mut at = vec![1, n];
        at.dedup();
        for i in at {
            kill_import_and_resume(&fx, &[(*p, i)], false);
        }
    }
    // Six files in one batch: one intent commit and one flush round.
    assert_eq!(hits(FaultPoint::AttachmentPlaced), 6);
    assert_eq!(hits(FaultPoint::AttachmentIntentCommitted), 1);
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
        false,
    );
}

/// A kill right after a cancelled session's terminal state committed: the
/// session is cancelled (state 2), resumable, and continues to the
/// uninterrupted result.
#[test]
fn a_kill_after_the_terminal_commit_resumes() {
    let fx = fixture();
    kill_import_and_resume(&fx, &[(FaultPoint::TerminalCommitted, 1)], true);
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

/// Run an import child killed at the n-th hit of `point` on a fresh
/// archive; returns the library, the archive and its database.
fn kill_once(
    fx: &Fixture,
    point: FaultPoint,
    n: u32,
) -> (
    tempfile::TempDir,
    Library,
    ArchiveId,
    PathBuf,
    tempfile::TempDir,
) {
    let (tmp, lib) = library();
    let staging = tempfile::tempdir().expect("staging");
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let envs = vec![
        (ENV_FAULT, format!("{}:{n}", point.name())),
        (ENV_SCENARIO, "import".to_owned()),
        (ENV_LIBRARY, tmp.path().display().to_string()),
        (ENV_ARCHIVE, id.as_str().to_owned()),
        (ENV_BACKUP, fx.backup.display().to_string()),
        (ENV_LINEAGE, "new".to_owned()),
        (ENV_STAGING, staging.path().display().to_string()),
    ];
    assert_eq!(
        run_child(&envs),
        Child::Killed,
        "{} {n} not reached",
        point.name()
    );
    (tmp, lib, id, db, staging)
}

fn fx_hits(fx: &Fixture, p: FaultPoint) -> u32 {
    fx.hits.iter().find(|(q, _)| *q == p).map_or(0, |(_, n)| *n)
}

/// A kill while the gate waits: nothing was written, and a new import
/// gives the uninterrupted result.
#[test]
fn a_kill_at_the_gate_writes_nothing() {
    if cfg!(windows) {
        return;
    }
    let fx = fixture();
    assert_eq!(fx_hits(&fx, FaultPoint::GateWaiting), 1);
    let (tmp, lib) = library();
    let staging = tempfile::tempdir().expect("staging");
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let before = data_snapshot(&tmp, &id);
    let envs = vec![
        (ENV_FAULT, format!("{}:1", FaultPoint::GateWaiting.name())),
        (ENV_SCENARIO, "import".to_owned()),
        (ENV_LIBRARY, tmp.path().display().to_string()),
        (ENV_ARCHIVE, id.as_str().to_owned()),
        (ENV_BACKUP, fx.backup.display().to_string()),
        (ENV_LINEAGE, "new".to_owned()),
        (ENV_STAGING, staging.path().display().to_string()),
    ];
    assert_eq!(run_child(&envs), Child::Killed);
    assert_eq!(data_snapshot(&tmp, &id), before);
    let mut a = lib.open(&id).expect("open");
    let src = open_dir(&fx.backup, None);
    import(&mut a, &src, &opts(16), LineageChoice::New);
    drop(a);
    src.ob.close();
    assert_eq!(logical_snapshot(&db, Scope::WithSessions), fx.want);
}

/// Kills in finalize before the terminal state (spec §18.4: the map
/// cleanup): the session continues to the uninterrupted result.
#[test]
fn a_kill_in_finalize_before_completion_resumes() {
    if cfg!(windows) {
        return;
    }
    let fx = fixture();
    assert_eq!(fx_hits(&fx, FaultPoint::FinalizeStats), 1);
    let cleanup = fx_hits(&fx, FaultPoint::FinalizeCleanup);
    // One chunk per session-local table.
    assert_eq!(cleanup, 3);
    kill_import_and_resume(&fx, &[(FaultPoint::FinalizeStats, 1)], false);
    for n in 1..=cleanup {
        kill_import_and_resume(&fx, &[(FaultPoint::FinalizeCleanup, n)], false);
    }
}

/// Kills after the session completed (spec §18.4: between FTS merge
/// steps, at the checkpoint, during the provenance `.tmp` write and before
/// its rename): the session stays completed, the archive is consistent,
/// the next write open sweeps the `.tmp`, and the archive equals the
/// uninterrupted one.
#[test]
fn a_kill_after_completion_leaves_a_completed_consistent_archive() {
    if cfg!(windows) {
        return;
    }
    let fx = fixture();
    assert_eq!(fx_hits(&fx, FaultPoint::TerminalCommitted), 1);
    let merges = fx_hits(&fx, FaultPoint::FtsMergeStep);
    assert!(merges >= 1);
    let mut kills = vec![(FaultPoint::TerminalCommitted, 1)];
    kills.extend((1..=merges).map(|n| (FaultPoint::FtsMergeStep, n)));
    for p in [
        FaultPoint::Checkpoint,
        FaultPoint::ProvenanceTmpCreated,
        FaultPoint::ProvenanceBeforeRename,
    ] {
        assert_eq!(fx_hits(&fx, p), 1, "{}", p.name());
        kills.push((p, 1));
    }
    for (point, n) in kills {
        let (_tmp, lib, id, db, _staging) = kill_once(&fx, point, n);
        let state: i64 = read_only(&db)
            .query_row("SELECT state FROM import_session", [], |r| r.get(0))
            .expect("state");
        assert_eq!(state, 1, "{} {n}", point.name());
        let a = lib.open(&id).expect("open after a kill");
        drop(a);
        assert_consistent_copy(&db);
        let imports = db.with_file_name("imports");
        let mut a = lib.open(&id).expect("open");
        a.begin_write(Recovery::Run).expect("write open");
        drop(a);
        let names: Vec<String> = std::fs::read_dir(&imports)
            .expect("imports")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|n| !n.ends_with(".tmp")),
            "{} {n}: {names:?}",
            point.name()
        );
        assert_consistent(&db);
        assert_eq!(
            logical_snapshot(&db, Scope::WithSessions),
            fx.want,
            "after a kill at {} {n}",
            point.name()
        );
    }
}

/// Kills inside a migration step and at its `COMMIT` (spec §18.4): the
/// archive stays at version 1, consistent, and the migration then runs.
#[test]
fn a_kill_in_a_migration_step_leaves_the_old_version() {
    if cfg!(windows) {
        return;
    }
    for point in [FaultPoint::MigrationStep, FaultPoint::MigrationBeforeCommit] {
        let (tmp, lib) = library();
        let id = lib.create(None).expect("create");
        let db = db_path(&tmp, &id);
        let envs = [
            (ENV_FAULT, format!("{}:1", point.name())),
            (ENV_SCENARIO, "migrate".to_owned()),
            (ENV_LIBRARY, tmp.path().display().to_string()),
            (ENV_ARCHIVE, id.as_str().to_owned()),
        ];
        assert_eq!(run_child(&envs), Child::Killed, "{}", point.name());
        let version: i64 = read_only(&db)
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .expect("version");
        assert_eq!(version, 1, "{}", point.name());
        assert_consistent_copy(&db);
        let _faults = install(Faults {
            schema: Some(TestSchema::V2),
            ..Faults::default()
        });
        let mut a = lib.open(&id).expect("open");
        assert!(a.needs_migration());
        let r = a
            .migrate(&mut ArchiveCtx {
                cancel: &CancelToken::new(),
                progress: &mut NoProgress,
                clock: &FixedClock(MODEL_NOW_NS),
            })
            .expect("migrates");
        assert_eq!((r.from, r.to), (1, 2));
        drop(a);
        assert_consistent(&db);
    }
}

/// Every fault point is killed at by one of the tests above (add a point
/// to `FaultPoint::ALL` and its kill together).
#[test]
fn every_fault_point_has_a_kill() {
    let covered = [
        // a_kill_at_any_import_fault_point_resumes_to_the_uninterrupted_result
        FaultPoint::Record,
        FaultPoint::BatchBeforeCommit,
        FaultPoint::BatchAfterCommit,
        FaultPoint::SessionCommitted,
        FaultPoint::RecoverySwept,
        FaultPoint::RecoveryReconciled,
        FaultPoint::RecoveryMarked,
        // a_kill_after_the_terminal_commit_resumes, and after completion
        FaultPoint::TerminalCommitted,
        // the store plan
        FaultPoint::AttachmentPartialCreated,
        FaultPoint::AttachmentCopying,
        FaultPoint::AttachmentSynced,
        FaultPoint::AttachmentIntentCommitted,
        FaultPoint::AttachmentPlaced,
        FaultPoint::AttachmentDirSynced,
        // creation
        FaultPoint::CreateBeforeRename,
        FaultPoint::CreateAfterRename,
        // the gate, finalize and provenance
        FaultPoint::GateWaiting,
        FaultPoint::FinalizeStats,
        FaultPoint::FinalizeCleanup,
        FaultPoint::FtsMergeStep,
        FaultPoint::Checkpoint,
        FaultPoint::ProvenanceTmpCreated,
        FaultPoint::ProvenanceBeforeRename,
        // migration
        FaultPoint::MigrationStep,
        FaultPoint::MigrationBeforeCommit,
    ];
    for p in FaultPoint::ALL {
        assert!(covered.contains(&p), "{} has no kill", p.name());
    }
    assert_eq!(covered.len(), FaultPoint::ALL.len());
}
