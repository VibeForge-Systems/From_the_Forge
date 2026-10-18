//! The import and verify jobs, end to end without Tauri, on the synthetic
//! import catalogue (spec §18.10): the gate (confirm, decline, cancel,
//! timeout, the exit handler), the terminal events, the details while the
//! job waits, the list merged with the job table, `matchingBackupIds`, and
//! the verify job.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oma_archive::space::SpaceProbe;
use oma_archive::{ArchiveId, LineageChoice, VerifyDepth};
use oma_backup::{inspect, Candidate, Limits};

use super::dto::{
    ArchiveStatusDto, GateEndingDto, ImportOptionsInput, ImportOutcomeDto, NotImportableDto,
    ResumeOptionsDto, SessionStateDto, VerifyOutcomeDto,
};
use super::gate::tests::FakeClock;
use super::gate::{GateAnswer, GATE_TIMEOUT};
use super::worker::{run_import, run_verify, ImportSpec, VerifySpec};
use super::{details, listing, ArchiveEnv};
use crate::backups::dto::{JobEvent, JobKind, JobPhase};
use crate::backups::worker::JobSpec;
use crate::backups::{listing as backup_listing, JobEventSink, JobSubject, Jobs};
use crate::error::ErrorCode;

const UDID: &str = "SYNTHETIC0-0000000000000000";

/// 1 TiB free of 2 TiB, so no test depends on the runner's disk.
struct FakeSpace;

impl SpaceProbe for FakeSpace {
    fn available(&self, _: &Path) -> io::Result<u64> {
        Ok(1 << 40)
    }
    fn total(&self, _: &Path) -> io::Result<u64> {
        Ok(2 << 40)
    }
}

fn env() -> ArchiveEnv {
    ArchiveEnv {
        space: Some(Arc::new(FakeSpace)),
    }
}

fn fixtures(catalogue: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("tests")
        .join("fixtures")
        .join(catalogue)
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("mkdir");
    for e in std::fs::read_dir(src).expect("read_dir") {
        let e = e.expect("entry");
        let to = dst.join(e.file_name());
        if e.file_type().expect("type").is_dir() {
            copy_tree(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), &to).expect("copy");
        }
    }
}

struct Fx {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    library: PathBuf,
    archive: ArchiveId,
}

fn fixture(catalogue: &str, name: &str) -> Fx {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("backups").join(UDID);
    copy_tree(&fixtures(catalogue).join(name).join(UDID), &dir);
    let library = tmp.path().join("library");
    std::fs::create_dir(&library).expect("mkdir");
    let library = dunce::canonicalize(&library).expect("canonical");
    let archive = env()
        .library(&library)
        .expect("library")
        .create(None)
        .expect("create");
    Fx {
        _tmp: tmp,
        dir,
        library,
        archive,
    }
}

#[derive(Clone, Default)]
struct Recording(Arc<Mutex<Vec<JobEvent>>>);

impl JobEventSink for Recording {
    fn send(&mut self, event: JobEvent) {
        self.0.lock().expect("lock").push(event);
    }
}

impl Recording {
    fn events(&self) -> Vec<JobEvent> {
        self.0.lock().expect("lock").clone()
    }
}

fn terminal(events: &[JobEvent]) -> JobEvent {
    let t: Vec<&JobEvent> = events.iter().filter(|e| e.is_terminal()).collect();
    assert_eq!(t.len(), 1, "exactly one terminal event: {events:?}");
    assert!(events.last().is_some_and(JobEvent::is_terminal), "last");
    t[0].clone()
}

fn options() -> ImportOptionsInput {
    ImportOptionsInput {
        include_recently_deleted: false,
        copy_attachments: true,
    }
}

/// A running import job on its own thread.
struct Running {
    job_id: String,
    rec: Recording,
    handle: std::thread::JoinHandle<()>,
}

fn start(fx: &Fx, jobs: &Arc<Jobs>, sink: Option<Box<dyn JobEventSink>>) -> Running {
    let candidate = Candidate::for_dir(&fx.dir).expect("candidate");
    let limits = Limits::default();
    let inspection = inspect(&candidate, &limits, &mut 0).expect("inspect");
    let backup_id = inspection.id.as_str().to_owned();
    let r = jobs
        .reserve(
            JobSubject::import(&backup_id, fx.archive.as_str()),
            JobKind::Import,
        )
        .expect("reserve");
    let spec = ImportSpec {
        job: JobSpec {
            job_id: r.job_id.clone(),
            backup_id,
            kind: JobKind::Import,
            inspection,
            library: fx.library.clone(),
            password: None,
            limits,
            cancel: r.cancel.clone(),
        },
        archive_id: fx.archive.clone(),
        options: options(),
        gate: r.gate.clone().expect("gate"),
        env: env(),
    };
    let rec = Recording::default();
    let mut sink: Box<dyn JobEventSink> = sink.unwrap_or_else(|| Box::new(rec.clone()));
    let worker_jobs = jobs.clone();
    let done = r.done;
    let handle = std::thread::spawn(move || {
        run_import(spec, &worker_jobs, sink.as_mut());
        drop(done);
    });
    Running {
        job_id: r.job_id,
        rec,
        handle,
    }
}

/// Wait until the job shows a preflight at its gate.
fn at_gate(jobs: &Jobs, fx: &Fx) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if let Some((job, Some(_))) = jobs.active_for_archive(fx.archive.as_str()) {
            assert!(job.waiting_at_gate);
            assert_eq!(job.phase, Some(JobPhase::AwaitingConfirmation));
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the import never reached the gate");
}

fn file_bytes(p: &Path) -> Option<Vec<u8>> {
    std::fs::read(p).ok()
}

/// What "Decline writes nothing" compares: the database and its `-wal`
/// (an empty `-wal` counts as none: closing the last connection removes
/// it).
fn data(fx: &Fx) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let dir = fx.library.join(fx.archive.as_str());
    (
        file_bytes(&dir.join("archive.sqlite")),
        file_bytes(&dir.join("archive.sqlite-wal")).filter(|w| !w.is_empty()),
    )
}

fn staging_is_empty(library: &Path) -> bool {
    match std::fs::read_dir(library.join(".oma-staging")) {
        Ok(entries) => entries.count() == 0,
        Err(_) => true,
    }
}

#[test]
fn an_import_waits_at_the_gate_then_completes() {
    let fx = fixture("imports", "imp-dev1-v1-plain");
    let jobs = Arc::new(Jobs::default());
    let run = start(&fx, &jobs, None);
    at_gate(&jobs, &fx);

    // While the job waits: the details show the job and its preflight,
    // and the list shows the archive as importing.
    let d = details(&fx.library, &env(), &fx.archive, &jobs, &[]).expect("details");
    let job = d.active_job.expect("active job");
    assert_eq!(job.kind, JobKind::Import);
    assert!(job.waiting_at_gate);
    let p = d.preflight.expect("preflight");
    assert!(p.keys.messages.rows > 0);
    assert!(p.lineages.is_empty(), "a fresh archive has no lineage");
    let l = listing(Some(&fx.library), &env(), &jobs);
    assert_eq!(l.entries[0].status, ArchiveStatusDto::Importing);
    assert!(run
        .rec
        .events()
        .iter()
        .any(|e| matches!(e, JobEvent::ImportPreflight { .. })));

    jobs.confirm(&run.job_id, GateAnswer::Proceed(LineageChoice::New))
        .expect("confirm");
    run.handle.join().expect("join");
    let events = run.rec.events();
    match terminal(&events) {
        JobEvent::ImportFinished {
            outcome,
            session,
            staging_removed,
            plaintext_removed,
            notice,
            ..
        } => {
            assert_eq!(outcome, ImportOutcomeDto::Completed);
            assert_eq!(session, Some(1));
            assert!(staging_removed);
            assert_eq!(plaintext_removed, Some(true));
            assert!(notice.is_none());
        }
        other => panic!("unexpected {other:?}"),
    }
    for phase in [
        JobPhase::StagingMessagesDb,
        JobPhase::ScanningKeys,
        JobPhase::AwaitingConfirmation,
        JobPhase::ImportingHandles,
        JobPhase::ImportingMessages,
        JobPhase::Finalizing,
    ] {
        assert!(
            events
                .iter()
                .any(|e| matches!(e, JobEvent::Progress { phase: p, .. } if *p == phase)),
            "no {phase:?}"
        );
    }
    assert!(!jobs.any_active());
    assert!(staging_is_empty(&fx.library));

    let d = details(&fx.library, &env(), &fx.archive, &jobs, &[]).expect("details");
    assert!(d.active_job.is_none() && d.preflight.is_none());
    let last = d.last_import.expect("last import");
    assert_eq!(last.state, SessionStateDto::Completed);
    let counts = last.counts.expect("counts");
    assert!(counts.messages.inserted > 0);
    assert!(d.summary.stats.is_some_and(|s| s.messages > 0));
    assert_eq!(d.sessions.len(), 1);
    assert!(!d.resumable && d.matching_backup_ids.is_empty());
    assert_eq!(d.resume_options, None);
    let json = serde_json::to_string(&d).expect("json");
    assert!(!json.contains(UDID), "no device identifier in the details");
    let l = listing(Some(&fx.library), &env(), &jobs);
    assert_eq!(l.entries[0].status, ArchiveStatusDto::Ready);
}

#[test]
fn decline_cancel_and_timeout_at_the_gate_write_nothing() {
    for ending in [
        GateEndingDto::Declined,
        GateEndingDto::Cancelled,
        GateEndingDto::TimedOut,
    ] {
        let fx = fixture("imports", "imp-dev1-v1-plain");
        let before = data(&fx);
        let clock = Arc::new(FakeClock::default());
        let jobs = Arc::new(Jobs::with_gate(clock.clone(), GATE_TIMEOUT));
        let run = start(&fx, &jobs, None);
        at_gate(&jobs, &fx);
        match ending {
            GateEndingDto::Declined => jobs
                .confirm(&run.job_id, GateAnswer::Decline)
                .expect("decline"),
            GateEndingDto::Cancelled => assert!(jobs.cancel(&run.job_id)),
            GateEndingDto::TimedOut => {
                // The deadline was fixed when the gate opened, before the
                // preflight was published; the waiting thread re-reads the
                // clock at least every GATE_POLL.
                clock.advance(GATE_TIMEOUT);
            }
        }
        // Bounded in real time, so a gate that never times out fails the
        // test instead of hanging the binary.
        let deadline = Instant::now() + Duration::from_secs(60);
        while !run.handle.is_finished() {
            assert!(
                Instant::now() < deadline,
                "{ending:?}: the job never left the gate"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        run.handle.join().expect("join");
        match terminal(&run.rec.events()) {
            JobEvent::ImportFinished {
                outcome,
                gate,
                session,
                staging_removed,
                ..
            } => {
                assert_eq!(outcome, ImportOutcomeDto::CancelledAtGate, "{ending:?}");
                assert_eq!(gate, Some(ending));
                assert_eq!(session, None);
                assert!(staging_removed);
            }
            other => panic!("{ending:?}: unexpected {other:?}"),
        }
        assert_eq!(data(&fx), before, "{ending:?} wrote to the archive");
        assert!(!jobs.any_active());
        assert!(staging_is_empty(&fx.library));
        assert!(jobs.confirm(&run.job_id, GateAnswer::Decline).is_err());
    }
}

#[test]
fn the_exit_handler_wakes_a_waiting_gate() {
    let fx = fixture("imports", "imp-dev1-v1-plain");
    let jobs = Arc::new(Jobs::default());
    let run = start(&fx, &jobs, None);
    at_gate(&jobs, &fx);
    let started = Instant::now();
    assert!(jobs.cancel_all_and_wait(Duration::from_secs(5)));
    assert!(started.elapsed() < Duration::from_secs(5));
    run.handle.join().expect("join");
    assert!(matches!(
        terminal(&run.rec.events()),
        JobEvent::ImportFinished {
            outcome: ImportOutcomeDto::CancelledAtGate,
            gate: Some(GateEndingDto::Cancelled),
            ..
        }
    ));
}

/// Cancels the job when the chat pass starts; confirms at the gate.
struct CancelAtChats {
    rec: Recording,
    jobs: Arc<Jobs>,
    job_id: Arc<Mutex<String>>,
}

impl JobEventSink for CancelAtChats {
    fn send(&mut self, event: JobEvent) {
        let id = self.job_id.lock().expect("lock").clone();
        match &event {
            JobEvent::ImportPreflight { .. } => {
                // The gate is open before the event is sent.
                self.jobs
                    .confirm(&id, GateAnswer::Proceed(LineageChoice::New))
                    .expect("confirm from the sink");
            }
            JobEvent::Progress {
                phase: JobPhase::ImportingChats,
                ..
            } => {
                self.jobs.cancel(&id);
            }
            _ => {}
        }
        self.rec.send(event);
    }
}

#[test]
fn a_cancel_during_the_passes_ends_cancelled_and_preselects_the_backup() {
    let fx = fixture("imports", "imp-dev1-v1-plain");
    let jobs = Arc::new(Jobs::default());
    let job_id = Arc::new(Mutex::new(String::new()));
    let rec = Recording::default();
    let sink = CancelAtChats {
        rec: rec.clone(),
        jobs: jobs.clone(),
        job_id: job_id.clone(),
    };
    // The sink reads the job id on every event: hold it until it is set.
    let mut guard = job_id.lock().expect("lock");
    let run = start(&fx, &jobs, Some(Box::new(sink)));
    *guard = run.job_id.clone();
    drop(guard);
    run.handle.join().expect("join");
    match terminal(&rec.events()) {
        JobEvent::Cancelled {
            staging_removed, ..
        } => assert!(staging_removed),
        other => panic!("unexpected {other:?}"),
    }
    // The session is unfinished; the backup it came from is pre-selected.
    let candidate = Candidate::for_dir(&fx.dir).expect("candidate");
    let insp = inspect(&candidate, &Limits::default(), &mut 0).expect("inspect");
    let summary = backup_listing::summarize(&insp, "loc", None, &Limits::default());
    let mut other = summary.clone();
    other.id = "another".into();
    other.last_backup_date = Some("2001-01-01T00:00:00Z".into());
    let d = details(
        &fx.library,
        &env(),
        &fx.archive,
        &jobs,
        &[summary.clone(), other],
    )
    .expect("details");
    assert!(d.resumable);
    assert_eq!(d.summary.resumable_state, Some(SessionStateDto::Cancelled));
    assert_eq!(d.matching_backup_ids, vec![summary.id]);
    // "Continue importing…" starts from the session's own options (D64).
    assert_eq!(
        d.resume_options,
        Some(ResumeOptionsDto {
            include_recently_deleted: options().include_recently_deleted,
            copy_attachments: options().copy_attachments,
        })
    );
    assert_eq!(
        d.last_import.map(|l| l.state),
        Some(SessionStateDto::Cancelled)
    );
}

#[test]
fn a_backup_that_fails_validation_is_not_imported() {
    let fx = fixture("backups", "missing-sms-blob");
    let before = data(&fx);
    let jobs = Arc::new(Jobs::default());
    let run = start(&fx, &jobs, None);
    run.handle.join().expect("join");
    match terminal(&run.rec.events()) {
        JobEvent::ImportFinished {
            outcome,
            not_importable,
            notice,
            ..
        } => {
            assert_eq!(outcome, ImportOutcomeDto::NotImportable);
            assert_eq!(not_importable, Some(NotImportableDto::BackupNotImportable));
            assert_eq!(notice.map(|n| n.code), Some(ErrorCode::UnsupportedBackup));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(data(&fx), before);
}

#[test]
fn an_archive_that_is_gone_fails_with_its_code() {
    let mut fx = fixture("imports", "imp-dev1-v1-plain");
    fx.archive = ArchiveId::parse(&format!("a{}", "0".repeat(32))).expect("id");
    let jobs = Arc::new(Jobs::default());
    let run = start(&fx, &jobs, None);
    run.handle.join().expect("join");
    match terminal(&run.rec.events()) {
        JobEvent::Failed {
            error,
            staging_removed,
            ..
        } => {
            assert_eq!(error.code, ErrorCode::ArchiveNotFound);
            assert!(staging_removed);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn the_list_merges_in_use_with_the_job_table() {
    let fx = fixture("imports", "imp-dev1-v1-plain");
    let jobs = Jobs::default();
    let lib = env().library(&fx.library).expect("library");
    let held = lib.open(&fx.archive).expect("open");
    let status = |jobs: &Jobs| listing(Some(&fx.library), &env(), jobs).entries[0].status;
    assert_eq!(status(&jobs), ArchiveStatusDto::InUse);
    let r = jobs
        .reserve(
            JobSubject::import("b", fx.archive.as_str()),
            JobKind::Import,
        )
        .expect("reserve");
    assert_eq!(status(&jobs), ArchiveStatusDto::Importing);
    jobs.release(&r.job_id);
    let r = jobs
        .reserve(JobSubject::archive(fx.archive.as_str()), JobKind::Verify)
        .expect("reserve");
    assert_eq!(status(&jobs), ArchiveStatusDto::Verifying);
    jobs.release(&r.job_id);
    held.close();
    let l = listing(Some(&fx.library), &env(), &jobs);
    assert_eq!(l.entries[0].status, ArchiveStatusDto::Ready);
    let s = l.entries[0].summary.as_ref().expect("summary");
    assert_eq!(s.archive_id, fx.archive.as_str());
    assert!(s.stats.is_none() && s.label.is_none());
    let none = listing(None, &env(), &jobs);
    assert_eq!(
        none.library_state,
        super::dto::LibraryStateDto::NotConfigured
    );
    let gone = listing(Some(&fx.library.join("missing")), &env(), &jobs);
    assert_eq!(gone.library_state, super::dto::LibraryStateDto::Unusable);
    assert!(gone.library_error.is_some());
}

#[test]
fn a_verify_job_reports_and_records() {
    let fx = fixture("imports", "imp-dev1-v1-plain");
    let jobs = Arc::new(Jobs::default());
    for (depth, cancel_first) in [(VerifyDepth::Quick, false), (VerifyDepth::Full, true)] {
        let r = jobs
            .reserve(JobSubject::archive(fx.archive.as_str()), JobKind::Verify)
            .expect("reserve");
        if cancel_first {
            r.cancel.cancel();
        }
        let mut rec = Recording::default();
        run_verify(
            VerifySpec {
                job_id: r.job_id.clone(),
                archive_id: fx.archive.clone(),
                library: fx.library.clone(),
                depth,
                cancel: r.cancel.clone(),
                env: env(),
            },
            &jobs,
            &mut rec,
        );
        let events = rec.events();
        if cancel_first {
            assert!(matches!(
                terminal(&events),
                JobEvent::Cancelled {
                    staging_removed: true,
                    ..
                }
            ));
        } else {
            match terminal(&events) {
                JobEvent::VerifyFinished {
                    job_id,
                    outcome,
                    recorded,
                } => {
                    assert_eq!(job_id, r.job_id);
                    assert_eq!(outcome, VerifyOutcomeDto::Ok);
                    assert!(recorded);
                }
                other => panic!("unexpected {other:?}"),
            }
            assert!(events.iter().any(|e| matches!(
                e,
                JobEvent::Progress {
                    phase: JobPhase::VerifyingDatabase,
                    ..
                }
            )));
        }
        assert!(!jobs.any_active());
    }
    let d = details(&fx.library, &env(), &fx.archive, &jobs, &[]).expect("details");
    let v = d.last_verify.expect("last verify");
    assert_eq!(v.outcome, Some(VerifyOutcomeDto::Ok));
    assert!(v.counts.is_some_and(|c| !c.has_problems));
}
