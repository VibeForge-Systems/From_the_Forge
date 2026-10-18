//! The validation job worker (§11.2): sweep, stage, open, validate, close,
//! then exactly one terminal event.
//!
//! It runs on a dedicated `std::thread` named `oma-job-<n>` and talks to the
//! UI only through [`JobEventSink`], so it is tested without Tauri.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use oma_backup::{
    sweep_stale, BackupError, CancelToken, CleanupReport, Inspection, JobCtx, Limits, OpenBackup,
    OpenError, Password, Phase, Progress, ProgressSink, StagingRoot, ValidationReport,
};

use super::dto::{
    clamp, millis, rfc3339, safe_opt, CleanupDto, JobEvent, JobPhase, ManifestStatsDto,
    SmsDbStatusDto, ValidationReportDto,
};
use super::{messages, JobEventSink, Jobs, LastOutcome};

/// Forward a progress event at most this often (plus every phase change).
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Everything one job needs. The password is moved in and consumed by
/// `OpenBackup::open`, which zeroizes it.
pub struct JobSpec {
    pub job_id: String,
    pub backup_id: String,
    pub inspection: Inspection,
    pub library: PathBuf,
    pub password: Option<Password>,
    pub limits: Limits,
    pub cancel: CancelToken,
}

/// The report as it crosses IPC: numbers clamped, sentences written here.
pub fn report_dto(r: &ValidationReport, encrypted: bool) -> ValidationReportDto {
    let s = &r.stats;
    let mut saturated = false;
    let mut c = |v: u64| {
        let (v, sat) = clamp(v);
        saturated |= sat;
        v
    };
    let stats = ManifestStatsDto {
        total_rows: c(s.total_rows),
        files: c(s.files),
        directories: c(s.directories),
        symlinks: c(s.symlinks),
        other_flags: c(s.other_flags),
        declared_bytes: c(s.declared_bytes),
        declared_bytes_saturated: s.declared_bytes_saturated,
        missing_blobs: c(s.missing_blobs),
        size_mismatches: c(s.size_mismatches),
        unsafe_blobs: c(s.unsafe_blobs),
        unreadable_blobs: c(s.unreadable_blobs),
        invalid_rows: c(s.invalid_rows),
        invalid_file_ids: c(s.invalid_file_ids),
        file_id_mismatches: c(s.file_id_mismatches),
        paths_unusual_for_export: c(s.paths_unusual_for_export),
        unreadable_records: c(s.unreadable_records),
        files_without_key: c(s.files_without_key),
        files_with_unavailable_class: c(s.files_with_unavailable_class),
        has_properties_table: s.has_properties_table,
        views: s.views,
        triggers: s.triggers,
        saturated: false,
    };
    let stats = ManifestStatsDto { saturated, ..stats };
    let m = &r.sms_db;
    ValidationReportDto {
        importable: r.importable(),
        checks: r
            .checks
            .iter()
            .map(|ch| messages::check(ch, encrypted))
            .collect(),
        stats,
        sms_db: SmsDbStatusDto {
            listed: m.listed,
            ambiguous: m.ambiguous,
            blob_present: m.blob_present,
            declared_size: safe_opt(m.declared_size),
            size_consistent: m.size_consistent,
            header_ok: m.header_ok,
            padding_ok: m.padding_ok,
            wal_listed: m.wal_listed,
            shm_listed: m.shm_listed,
        },
        duration_ms: millis(r.elapsed),
        validated_at: rfc3339(SystemTime::now()),
    }
}

/// Forwards progress to the sink when the phase changes, or when at least
/// [`PROGRESS_INTERVAL`] has passed and `done` has changed.
struct Throttle<'a> {
    job_id: &'a str,
    jobs: &'a Jobs,
    sink: &'a mut dyn JobEventSink,
    last_phase: Option<Phase>,
    last_done: u64,
    last_sent: Instant,
}

impl ProgressSink for Throttle<'_> {
    fn report(&mut self, p: Progress) {
        let phase_changed = self.last_phase != Some(p.phase);
        let due = self.last_sent.elapsed() >= PROGRESS_INTERVAL && p.done != self.last_done;
        if !(phase_changed || due) {
            return;
        }
        if phase_changed {
            tracing::debug!(job_id = self.job_id, phase = ?p.phase, "job phase");
            self.jobs.set_phase(self.job_id, JobPhase::from(p.phase));
        }
        self.last_phase = Some(p.phase);
        self.last_done = p.done;
        self.last_sent = Instant::now();
        let (done, _) = clamp(p.done);
        self.sink.send(JobEvent::Progress {
            job_id: self.job_id.to_owned(),
            phase: JobPhase::from(p.phase),
            done,
            total: safe_opt(p.total),
        });
    }
}

enum Ending {
    Finished(Box<ValidationReport>, bool),
    Failed(BackupError),
    Cancelled,
}

/// Run one job to its end. Always closes the backup, stores the outcome and
/// frees the job slot before sending exactly one terminal event.
pub fn run(spec: JobSpec, jobs: &Jobs, sink: &mut dyn JobEventSink) {
    let JobSpec {
        job_id,
        backup_id,
        inspection,
        library,
        password,
        limits,
        cancel,
    } = spec;
    tracing::info!(job_id = %job_id, backup_id = %backup_id, "job started");

    let sweep = sweep_stale(&library);
    if sweep.removed + sweep.failed + sweep.in_use > 0 {
        tracing::info!(
            removed = sweep.removed,
            in_use = sweep.in_use,
            skipped = sweep.skipped,
            failed = sweep.failed,
            "staging swept before job"
        );
    }

    let (ending, cleanup) = {
        let mut throttle = Throttle {
            job_id: &job_id,
            jobs,
            sink: &mut *sink,
            last_phase: None,
            last_done: 0,
            last_sent: Instant::now(),
        };
        let mut ctx = JobCtx::new(&cancel, &mut throttle);
        execute(inspection, &library, password, &limits, &mut ctx)
    };

    let staging_removed = cleanup.removed;
    if !staging_removed {
        tracing::warn!(
            job_id = %job_id,
            attempts = cleanup.attempts,
            last_error = ?cleanup.last_error,
            "staging not removed"
        );
    }
    let cleanup_dto = Some(CleanupDto { staging_removed });
    let event = match ending {
        Ending::Finished(report, encrypted) => {
            let dto = report_dto(&report, encrypted);
            tracing::info!(job_id = %job_id, importable = dto.importable, "job finished");
            jobs.finish(
                &job_id,
                LastOutcome {
                    report: Some(dto.clone()),
                    cleanup: cleanup_dto,
                },
            );
            JobEvent::Finished {
                job_id,
                report: Box::new(dto),
                staging_removed,
            }
        }
        Ending::Cancelled => {
            tracing::info!(job_id = %job_id, "job cancelled");
            jobs.finish(
                &job_id,
                LastOutcome {
                    report: None,
                    cleanup: cleanup_dto,
                },
            );
            JobEvent::Cancelled {
                job_id,
                staging_removed,
            }
        }
        Ending::Failed(error) => {
            let ipc = messages::to_ipc(&error, &limits);
            tracing::info!(job_id = %job_id, code = ?ipc.code, "job failed");
            jobs.finish(
                &job_id,
                LastOutcome {
                    report: None,
                    cleanup: cleanup_dto,
                },
            );
            JobEvent::Failed {
                job_id,
                error: ipc,
                staging_removed,
            }
        }
    };
    sink.send(event);
}

fn nothing_staged() -> CleanupReport {
    CleanupReport {
        removed: true,
        attempts: 0,
        last_error: None,
    }
}

fn execute(
    inspection: Inspection,
    library: &std::path::Path,
    password: Option<Password>,
    limits: &Limits,
    ctx: &mut JobCtx<'_>,
) -> (Ending, CleanupReport) {
    let fail = |e: BackupError| match e {
        BackupError::Cancelled => Ending::Cancelled,
        e => Ending::Failed(e),
    };
    if ctx.cancel.is_cancelled() {
        return (Ending::Cancelled, nothing_staged());
    }
    let root = match StagingRoot::new(library, &inspection.root) {
        Ok(r) => r,
        Err(e) => return (fail(e), nothing_staged()),
    };
    let session = match root.begin() {
        Ok(s) => s,
        Err(e) => return (fail(e), nothing_staged()),
    };
    let ob = match OpenBackup::open(inspection, session, password, limits, ctx) {
        Ok(ob) => ob,
        Err(OpenError { error, cleanup }) => return (fail(error), cleanup),
    };
    let encrypted = ob.is_encrypted();
    let result = ob.validate(ctx);
    let cleanup = ob.close();
    match result {
        Ok(report) => (Ending::Finished(Box::new(report), encrypted), cleanup),
        Err(e) => (fail(e), cleanup),
    }
}

/// Start the worker thread. On success the job owns the reservation; the
/// sender is dropped when the worker ends, which is the completion signal.
pub fn spawn(
    spec: JobSpec,
    jobs: Arc<Jobs>,
    mut sink: Box<dyn JobEventSink>,
    done: Sender<()>,
) -> std::io::Result<()> {
    let name = format!("oma-{}", spec.job_id);
    std::thread::Builder::new().name(name).spawn(move || {
        run(spec, &jobs, sink.as_mut());
        drop(done);
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backups::dto::JobEvent;
    use crate::error::ErrorCode;
    use oma_backup::{inspect, Candidate, Zeroizing};
    use std::path::Path;
    use std::sync::Mutex;

    const UDID: &str = "SYNTHETIC0-0000000000000000";
    const PASSWORD: &str = "synthetic-test-password";

    fn fixtures() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tests/fixtures/backups"
        ))
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
        tmp: tempfile::TempDir,
        dir: PathBuf,
        library: PathBuf,
    }

    fn fixture(name: &str) -> Fx {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("backups").join(UDID);
        copy_tree(&fixtures().join(name).join(UDID), &dir);
        let library = tmp.path().join("library");
        std::fs::create_dir(&library).expect("mkdir");
        Fx { tmp, dir, library }
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

    fn spec(fx: &Fx, job_id: &str, cancel: CancelToken, password: Option<&str>) -> JobSpec {
        let candidate = Candidate::for_dir(&fx.dir).expect("candidate");
        let limits = Limits::default();
        let inspection = inspect(&candidate, &limits, &mut 0).expect("inspect");
        JobSpec {
            job_id: job_id.to_owned(),
            backup_id: inspection.id.as_str().to_owned(),
            inspection,
            library: fx.library.clone(),
            password: password
                .map(|p| Password::new(Zeroizing::new(p.to_owned()), &limits).expect("password")),
            limits,
            cancel,
        }
    }

    fn staging_is_empty(library: &Path) -> bool {
        match std::fs::read_dir(library.join(".oma-staging")) {
            Ok(entries) => entries.count() == 0,
            Err(_) => true,
        }
    }

    fn run_once(fx: &Fx, password: Option<&str>) -> (Vec<JobEvent>, Jobs) {
        let jobs = Jobs::default();
        let s = spec(fx, "", CancelToken::new(), password);
        let r = jobs.reserve(&s.backup_id).expect("reserve");
        let s = JobSpec {
            job_id: r.job_id.clone(),
            cancel: r.cancel.clone(),
            ..s
        };
        let mut sink = Recording::default();
        run(s, &jobs, &mut sink);
        (sink.events(), jobs)
    }

    fn terminal(events: &[JobEvent]) -> &JobEvent {
        let terminals: Vec<&JobEvent> = events.iter().filter(|e| e.is_terminal()).collect();
        assert_eq!(terminals.len(), 1, "exactly one terminal event");
        assert!(
            events.last().is_some_and(JobEvent::is_terminal),
            "terminal is last"
        );
        terminals[0]
    }

    #[test]
    fn plain_backup_finishes_with_an_importable_report() {
        let fx = fixture("plain");
        let (events, jobs) = run_once(&fx, None);
        match terminal(&events) {
            JobEvent::Finished {
                report,
                staging_removed,
                ..
            } => {
                assert!(report.importable, "{:#?}", report.checks);
                assert!(*staging_removed);
                assert!(report.sms_db.listed && report.sms_db.header_ok == Some(true));
                assert!(report.checks.iter().any(|c| c.id == "messages-db"));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(events.iter().any(|e| matches!(
            e,
            JobEvent::Progress {
                phase: JobPhase::CopyingManifest,
                ..
            }
        )));
        assert!(!jobs.any_active());
        assert!(staging_is_empty(&fx.library));
        drop(fx.tmp);
    }

    #[test]
    fn encrypted_backup_unlocks_with_the_password() {
        let fx = fixture("enc");
        let (events, jobs) = run_once(&fx, Some(PASSWORD));
        match terminal(&events) {
            JobEvent::Finished {
                report,
                staging_removed,
                ..
            } => {
                assert!(report.importable, "{:#?}", report.checks);
                assert!(*staging_removed);
                assert_eq!(report.sms_db.padding_ok, Some(true));
            }
            other => panic!("unexpected {other:?}"),
        }
        let backup_id = Candidate::for_dir(&fx.dir)
            .ok()
            .and_then(|c| oma_backup::BackupId::for_path(&c.display_canonical))
            .expect("id");
        let last = jobs.last(backup_id.as_str());
        assert!(last.report.is_some_and(|r| r.importable));
        assert_eq!(last.cleanup.map(|c| c.staging_removed), Some(true));
        assert!(staging_is_empty(&fx.library));
    }

    #[test]
    fn wrong_password_fails_with_its_code_and_cleans_up() {
        let fx = fixture("enc");
        let (events, _) = run_once(&fx, Some("not-the-password"));
        match terminal(&events) {
            JobEvent::Failed {
                error,
                staging_removed,
                ..
            } => {
                assert_eq!(error.code, ErrorCode::WrongPassword);
                assert!(*staging_removed);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(staging_is_empty(&fx.library));
    }

    #[test]
    fn a_hostile_iteration_count_fails_before_any_derivation() {
        let fx = fixture("hostile-dpic");
        let (events, _) = run_once(&fx, Some(PASSWORD));
        assert!(!events.iter().any(|e| matches!(
            e,
            JobEvent::Progress {
                phase: JobPhase::DerivingKey,
                ..
            }
        )));
        match terminal(&events) {
            JobEvent::Failed { error, .. } => {
                assert_eq!(error.code, ErrorCode::MalformedBackup);
                assert!(error.message.contains("key-derivation rounds"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn cancel_during_key_derivation_ends_cancelled() {
        let fx = fixture("enc-slow");
        let jobs = Arc::new(Jobs::default());
        let s = spec(&fx, "", CancelToken::new(), Some(PASSWORD));
        let r = jobs.reserve(&s.backup_id).expect("reserve");
        let s = JobSpec {
            job_id: r.job_id.clone(),
            cancel: r.cancel.clone(),
            ..s
        };
        /// Cancels as soon as key derivation is reported.
        struct CancelOnDerive(Recording, Arc<Jobs>, String);
        impl JobEventSink for CancelOnDerive {
            fn send(&mut self, event: JobEvent) {
                if matches!(
                    event,
                    JobEvent::Progress {
                        phase: JobPhase::DerivingKey,
                        ..
                    }
                ) {
                    self.1.cancel(&self.2);
                }
                self.0.send(event);
            }
        }
        let rec = Recording::default();
        let mut sink = CancelOnDerive(rec.clone(), jobs.clone(), r.job_id.clone());
        run(s, &jobs, &mut sink);
        let events = rec.events();
        match terminal(&events) {
            JobEvent::Cancelled {
                staging_removed, ..
            } => assert!(*staging_removed),
            other => panic!("unexpected {other:?}"),
        }
        assert!(staging_is_empty(&fx.library));
    }

    #[test]
    fn a_backup_changed_mid_job_fails_the_unchanged_check() {
        let fx = fixture("plain");
        let jobs = Jobs::default();
        let s = spec(&fx, "", CancelToken::new(), None);
        let r = jobs.reserve(&s.backup_id).expect("reserve");
        let s = JobSpec {
            job_id: r.job_id.clone(),
            cancel: r.cancel.clone(),
            ..s
        };
        /// Appends to Status.plist the first time the file pass starts.
        struct Toucher(Recording, PathBuf, bool);
        impl JobEventSink for Toucher {
            fn send(&mut self, event: JobEvent) {
                if !self.2 && matches!(event, JobEvent::Progress { .. }) {
                    use std::io::Write;
                    let mut f = std::fs::OpenOptions::new()
                        .append(true)
                        .open(self.1.join("Status.plist"))
                        .expect("open");
                    f.write_all(b"\n").expect("append");
                    self.2 = true;
                }
                self.0.send(event);
            }
        }
        let rec = Recording::default();
        let mut sink = Toucher(rec.clone(), fx.dir.clone(), false);
        run(s, &jobs, &mut sink);
        match terminal(&rec.events()) {
            JobEvent::Finished { report, .. } => {
                assert!(!report.importable);
                let unchanged = report
                    .checks
                    .iter()
                    .find(|c| c.id == "unchanged")
                    .expect("check");
                assert_eq!(unchanged.level, crate::settings::CheckLevel::Error);
                assert!(unchanged
                    .message
                    .contains("changed while it was being checked"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_cancelled_token_before_start_creates_nothing() {
        let fx = fixture("plain");
        let jobs = Jobs::default();
        let s = spec(&fx, "", CancelToken::new(), None);
        let r = jobs.reserve(&s.backup_id).expect("reserve");
        r.cancel.cancel();
        let s = JobSpec {
            job_id: r.job_id.clone(),
            cancel: r.cancel.clone(),
            ..s
        };
        let mut sink = Recording::default();
        run(s, &jobs, &mut sink);
        assert!(matches!(
            terminal(&sink.events()),
            JobEvent::Cancelled {
                staging_removed: true,
                ..
            }
        ));
        assert!(!fx.library.join(".oma-staging").exists());
    }

    #[test]
    fn every_dto_mapping_clamps_u64_max() {
        let mut r = ValidationReport {
            checks: Vec::new(),
            stats: oma_backup::ManifestStats::default(),
            sms_db: oma_backup::SmsDbStatus::default(),
            keybag_notes: Vec::new(),
            elapsed: Duration::MAX,
        };
        let s = &mut r.stats;
        for f in [
            &mut s.total_rows,
            &mut s.files,
            &mut s.directories,
            &mut s.symlinks,
            &mut s.other_flags,
            &mut s.declared_bytes,
            &mut s.missing_blobs,
            &mut s.size_mismatches,
            &mut s.unsafe_blobs,
            &mut s.unreadable_blobs,
            &mut s.invalid_rows,
            &mut s.invalid_file_ids,
            &mut s.file_id_mismatches,
            &mut s.paths_unusual_for_export,
            &mut s.unreadable_records,
            &mut s.files_without_key,
            &mut s.files_with_unavailable_class,
        ] {
            *f = u64::MAX;
        }
        r.sms_db.declared_size = Some(u64::MAX);
        let dto = report_dto(&r, false);
        let json = serde_json::to_value(&dto).expect("json");
        for (k, v) in json["stats"].as_object().expect("object") {
            if let Some(n) = v.as_u64() {
                assert!(n <= crate::backups::dto::MAX_SAFE_INTEGER, "{k}");
            }
        }
        assert!(dto.stats.saturated);
        assert_eq!(dto.sms_db.declared_size, None);
        assert_eq!(dto.duration_ms, crate::backups::dto::MAX_SAFE_INTEGER);
    }
}
