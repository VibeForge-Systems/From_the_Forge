//! The job worker (§11.2, M3 §2): sweep, stage, open, validate, then for an
//! extraction check (if the backup is importable) stage, check and delete
//! the Messages database copy, recheck the backup, close; then exactly one
//! terminal event.
//!
//! It runs on a dedicated `std::thread` named `oma-job-<n>` and talks to the
//! UI only through [`JobEventSink`], so it is tested without Tauri.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use oma_backup::{
    sweep_stale, BackupError, CancelToken, CleanupReport, Inspection, JobCtx, Limits, OpenBackup,
    OpenError, Password, Progress, ProgressSink, StagingRoot, ValidationReport,
};
use oma_messages::{CheckOptions, MessagesCtx, MessagesError, MessagesProgress, MessagesSink};

use super::dto::{
    clamp, millis, rfc3339, safe_opt, CleanupDto, JobEvent, JobKind, JobPhase, ManifestStatsDto,
    SmsDbStatusDto, ValidationReportDto,
};
use super::extraction::{self, ExtractionEnding};
use super::{messages, JobEventSink, Jobs, LastOutcome};

/// Forward a progress event at most this often (plus every phase change).
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Everything one job needs. The password is moved in and consumed by
/// `OpenBackup::open`, which zeroizes it.
pub struct JobSpec {
    pub job_id: String,
    pub backup_id: String,
    pub kind: JobKind,
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
/// [`PROGRESS_INTERVAL`] has passed and `done` has changed. One throttle
/// serves both progress sources of a job (M3 §16.1): the backup crate's
/// phases and the messages check's, which map one to one onto [`JobPhase`].
pub(crate) struct Throttle<'a> {
    job_id: &'a str,
    jobs: &'a Jobs,
    sink: &'a mut dyn JobEventSink,
    last_phase: Option<JobPhase>,
    last_done: u64,
    last_sent: Instant,
}

impl<'a> Throttle<'a> {
    pub(crate) fn new(job_id: &'a str, jobs: &'a Jobs, sink: &'a mut dyn JobEventSink) -> Self {
        Throttle {
            job_id,
            jobs,
            sink,
            last_phase: None,
            last_done: 0,
            last_sent: Instant::now(),
        }
    }

    /// Send a non-progress event (the import's `importPreflight`).
    pub(crate) fn send(&mut self, event: JobEvent) {
        self.sink.send(event);
    }

    pub(crate) fn forward(&mut self, phase: JobPhase, done: u64, total: Option<u64>) {
        let phase_changed = self.last_phase != Some(phase);
        let due = self.last_sent.elapsed() >= PROGRESS_INTERVAL && done != self.last_done;
        if !(phase_changed || due) {
            return;
        }
        if phase_changed {
            tracing::debug!(job_id = self.job_id, phase = ?phase, "job phase");
            self.jobs.set_phase(self.job_id, phase);
        }
        self.last_phase = Some(phase);
        self.last_done = done;
        self.last_sent = Instant::now();
        let (done, _) = clamp(done);
        self.sink.send(JobEvent::Progress {
            job_id: self.job_id.to_owned(),
            phase,
            done,
            total: safe_opt(total),
        });
    }
}

impl ProgressSink for Throttle<'_> {
    fn report(&mut self, p: Progress) {
        self.forward(JobPhase::from(p.phase), p.done, p.total);
    }
}

impl MessagesSink for Throttle<'_> {
    fn report(&mut self, p: MessagesProgress) {
        self.forward(JobPhase::from(p.phase), p.done, p.total);
    }
}

/// An error that ended a job.
enum Failure {
    Backup(BackupError),
    Messages(MessagesError),
}

enum Ending {
    Finished(Box<ValidationReport>, bool),
    /// The validation report, whether the backup is encrypted, and what the
    /// extraction part came to.
    ExtractionFinished(Box<ValidationReport>, bool, ExtractionEnding),
    Failed(Failure),
    Cancelled,
}

/// Run one job to its end. Always closes the backup, stores the outcome and
/// frees the job slot before sending exactly one terminal event.
pub fn run(spec: JobSpec, jobs: &Jobs, sink: &mut dyn JobEventSink) {
    let JobSpec {
        job_id,
        backup_id,
        kind,
        inspection,
        library,
        password,
        limits,
        cancel,
    } = spec;
    tracing::info!(job_id = %job_id, backup_id = %backup_id, kind = ?kind, "job started");
    let ios_version_untested = extraction::ios_version_untested(
        inspection.ios_version().as_ref().map(|s| s.value.as_str()),
    );

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
        let mut throttle = Throttle::new(&job_id, jobs, &mut *sink);
        execute(
            kind,
            inspection,
            &library,
            password,
            &limits,
            &cancel,
            &mut throttle,
        )
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
                    extraction: None,
                    cleanup: cleanup_dto,
                },
            );
            JobEvent::Finished {
                job_id,
                report: Box::new(dto),
                staging_removed,
            }
        }
        Ending::ExtractionFinished(report, encrypted, ext) => {
            let dto = report_dto(&report, encrypted);
            let ext = extraction::report_dto(&ext, &job_id, staging_removed, ios_version_untested);
            let summary = ext.summary();
            tracing::info!(
                job_id = %job_id,
                importable = dto.importable,
                outcome = ?summary.outcome,
                qualifiers = ext.qualifiers.len(),
                "job finished"
            );
            jobs.finish(
                &job_id,
                LastOutcome {
                    report: Some(dto),
                    extraction: Some(ext),
                    cleanup: cleanup_dto,
                },
            );
            JobEvent::ExtractionFinished {
                job_id,
                outcome: summary.outcome,
                source_changed: summary.source_changed,
                staging_removed,
                plaintext_removed_after_row_pass: summary.plaintext_removed_after_row_pass,
            }
        }
        Ending::Cancelled => {
            tracing::info!(job_id = %job_id, "job cancelled");
            jobs.finish(
                &job_id,
                LastOutcome {
                    report: None,
                    extraction: None,
                    cleanup: cleanup_dto,
                },
            );
            JobEvent::Cancelled {
                job_id,
                staging_removed,
            }
        }
        Ending::Failed(error) => {
            let ipc = match &error {
                Failure::Backup(e) => messages::to_ipc(e, &limits),
                Failure::Messages(e) => messages::messages_to_ipc(e, &limits),
            };
            tracing::info!(job_id = %job_id, code = ?ipc.code, "job failed");
            jobs.finish(
                &job_id,
                LastOutcome {
                    report: None,
                    extraction: None,
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

pub(crate) fn nothing_staged() -> CleanupReport {
    CleanupReport {
        removed: true,
        attempts: 0,
        last_error: None,
    }
}

fn execute(
    kind: JobKind,
    inspection: Inspection,
    library: &std::path::Path,
    password: Option<Password>,
    limits: &Limits,
    cancel: &CancelToken,
    throttle: &mut Throttle<'_>,
) -> (Ending, CleanupReport) {
    let fail = |e: BackupError| match e {
        BackupError::Cancelled => Ending::Cancelled,
        e => Ending::Failed(Failure::Backup(e)),
    };
    if cancel.is_cancelled() {
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
    let opened = {
        let mut ctx = JobCtx::new(cancel, &mut *throttle);
        OpenBackup::open(inspection, session, password, limits, &mut ctx)
    };
    let ob = match opened {
        Ok(ob) => ob,
        Err(OpenError { error, cleanup }) => return (fail(error), cleanup),
    };
    let encrypted = ob.is_encrypted();
    let validated = {
        let mut ctx = JobCtx::new(cancel, &mut *throttle);
        ob.validate(&mut ctx)
    };
    let ending = match (kind, validated) {
        (_, Err(e)) => fail(e),
        // Import and Verify jobs run in `archives::worker`, never here.
        (JobKind::Import | JobKind::Verify, Ok(_)) => Ending::Failed(Failure::Backup(
            BackupError::Internal("an archive job ran in the backup worker"),
        )),
        (JobKind::Validate, Ok(report)) => Ending::Finished(Box::new(report), encrypted),
        (JobKind::ExtractionCheck, Ok(report)) if !report.importable() => {
            Ending::ExtractionFinished(
                Box::new(report),
                encrypted,
                ExtractionEnding::SkippedNotImportable,
            )
        }
        (JobKind::ExtractionCheck, Ok(report)) => {
            // Stage, check and delete the Messages database copy, then
            // recheck the backup (M3 §2 step 5). The copy is also removed by
            // close() below if the check could not remove it.
            let mut mctx = MessagesCtx::new(cancel, &mut *throttle);
            match oma_messages::check(&ob, &CheckOptions::default(), &mut mctx) {
                Ok(x) => Ending::ExtractionFinished(
                    Box::new(report),
                    encrypted,
                    ExtractionEnding::Checked(Box::new(x)),
                ),
                Err(MessagesError::Cancelled) => Ending::Cancelled,
                Err(e) => Ending::Failed(Failure::Messages(e)),
            }
        }
    };
    let cleanup = ob.close();
    (ending, cleanup)
}

/// Start the worker thread. On success the job owns the reservation; the
/// sender is dropped when the worker ends, which is the completion signal.
pub fn spawn(
    spec: JobSpec,
    jobs: Arc<Jobs>,
    sink: Box<dyn JobEventSink>,
    done: Sender<()>,
) -> std::io::Result<()> {
    let job_id = spec.job_id.clone();
    spawn_with(&job_id, jobs, sink, done, move |jobs, sink| {
        run(spec, jobs, sink)
    })
}

/// Start a worker thread named `oma-<job id>` running `work`, which must
/// end by sending exactly one terminal event (backup jobs through [`run`],
/// archive jobs through `archives::worker`).
pub fn spawn_with(
    job_id: &str,
    jobs: Arc<Jobs>,
    mut sink: Box<dyn JobEventSink>,
    done: Sender<()>,
    work: impl FnOnce(&Jobs, &mut dyn JobEventSink) + Send + 'static,
) -> std::io::Result<()> {
    let name = format!("oma-{job_id}");
    std::thread::Builder::new().name(name).spawn(move || {
        work(&jobs, sink.as_mut());
        drop(done);
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backups::dto::JobEvent;
    use crate::backups::extraction::{ExtractionOutcomeDto, QualifierDto};
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
        fixture_from(&fixtures(), name)
    }

    /// An entry of the synthetic messages catalogue (M3).
    fn messages_fixture(name: &str) -> Fx {
        fixture_from(
            &PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../tests/fixtures/messages"
            )),
            name,
        )
    }

    fn fixture_from(root: &Path, name: &str) -> Fx {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("backups").join(UDID);
        copy_tree(&root.join(name).join(UDID), &dir);
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
        spec_kind(fx, JobKind::Validate, job_id, cancel, password)
    }

    fn spec_kind(
        fx: &Fx,
        kind: JobKind,
        job_id: &str,
        cancel: CancelToken,
        password: Option<&str>,
    ) -> JobSpec {
        let candidate = Candidate::for_dir(&fx.dir).expect("candidate");
        let limits = Limits::default();
        let inspection = inspect(&candidate, &limits, &mut 0).expect("inspect");
        JobSpec {
            job_id: job_id.to_owned(),
            backup_id: inspection.id.as_str().to_owned(),
            kind,
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
        run_kind(fx, JobKind::Validate, password)
    }

    fn run_kind(fx: &Fx, kind: JobKind, password: Option<&str>) -> (Vec<JobEvent>, Jobs) {
        let jobs = Jobs::default();
        let s = spec_kind(fx, kind, "", CancelToken::new(), password);
        let r = jobs.reserve(&s.backup_id, kind).expect("reserve");
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
        let r = jobs
            .reserve(&s.backup_id, JobKind::Validate)
            .expect("reserve");
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
        let r = jobs
            .reserve(&s.backup_id, JobKind::Validate)
            .expect("reserve");
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
        let r = jobs
            .reserve(&s.backup_id, JobKind::Validate)
            .expect("reserve");
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

    // ------------------------------------------------------------ M3

    fn backup_id_of(fx: &Fx) -> String {
        Candidate::for_dir(&fx.dir)
            .ok()
            .and_then(|c| oma_backup::BackupId::for_path(&c.display_canonical))
            .expect("id")
            .as_str()
            .to_owned()
    }

    #[test]
    fn an_extraction_check_reports_progress_then_one_summary() {
        let fx = messages_fixture("msgs-full-plain");
        let (events, jobs) = run_kind(&fx, JobKind::ExtractionCheck, None);
        for phase in [
            JobPhase::CopyingManifest,
            JobPhase::StagingMessagesDb,
            JobPhase::CheckingIntegrity,
            JobPhase::ReadingMessages,
            JobPhase::CheckingAttachments,
            JobPhase::Summarising,
        ] {
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, JobEvent::Progress { phase: p, .. } if *p == phase)),
                "no {phase:?} progress"
            );
        }
        let job_id = match terminal(&events) {
            JobEvent::ExtractionFinished {
                job_id,
                outcome,
                source_changed,
                staging_removed,
                plaintext_removed_after_row_pass,
            } => {
                // iosVersionUntested applies to every readable result in M3, so a real or
                // synthetic backup is never plain "readable".
                assert_eq!(*outcome, ExtractionOutcomeDto::ReadableWithQualifiers);
                assert!(!*source_changed);
                assert!(*staging_removed && *plaintext_removed_after_row_pass);
                job_id.clone()
            }
            other => panic!("unexpected {other:?}"),
        };
        let last = jobs.last(&backup_id_of(&fx));
        assert!(last.report.is_some_and(|r| r.importable));
        let ext = last.extraction.expect("extraction report");
        assert_eq!(ext.job_id, job_id, "reports are keyed by job");
        assert_eq!(
            ext.qualifiers.first(),
            Some(&QualifierDto::IosVersionUntested)
        );
        assert!(ext
            .qualifiers
            .contains(&QualifierDto::UnknownColumns { count: 1 }));
        let counts = ext.counts.expect("counts");
        assert!(counts.message_rows > 0 && counts.chat_rows > 0 && counts.handle_rows > 0);
        assert!(ext.dates.is_some() && ext.attachments.is_some() && ext.problems.is_some());
        assert!(!jobs.any_active());
        assert!(staging_is_empty(&fx.library));
    }

    #[test]
    fn an_encrypted_extraction_check_unlocks_and_cleans_up() {
        let fx = messages_fixture("msgs-full-enc");
        let (events, _) = run_kind(&fx, JobKind::ExtractionCheck, Some(PASSWORD));
        assert!(matches!(
            terminal(&events),
            JobEvent::ExtractionFinished {
                outcome: ExtractionOutcomeDto::ReadableWithQualifiers,
                staging_removed: true,
                plaintext_removed_after_row_pass: true,
                ..
            }
        ));
        assert!(staging_is_empty(&fx.library));
    }

    #[test]
    fn a_backup_that_is_not_importable_is_skipped() {
        let fx = fixture("missing-sms-blob");
        let (events, jobs) = run_kind(&fx, JobKind::ExtractionCheck, None);
        assert!(!events.iter().any(|e| matches!(
            e,
            JobEvent::Progress {
                phase: JobPhase::StagingMessagesDb,
                ..
            }
        )));
        assert!(matches!(
            terminal(&events),
            JobEvent::ExtractionFinished {
                outcome: ExtractionOutcomeDto::SkippedNotImportable,
                staging_removed: true,
                ..
            }
        ));
        let last = jobs.last(&backup_id_of(&fx));
        assert!(last.report.is_some_and(|r| !r.importable));
        let ext = last.extraction.expect("extraction");
        assert_eq!(ext.outcome, ExtractionOutcomeDto::SkippedNotImportable);
        assert!(ext.schema.is_none() && ext.counts.is_none());
    }

    #[test]
    fn unsupported_damaged_and_virtual_table_outcomes() {
        for (name, outcome) in [
            ("msgs-unsupported-plain", ExtractionOutcomeDto::Unsupported),
            ("msgs-vtab-plain", ExtractionOutcomeDto::Unsupported),
            ("msgs-corrupt-plain", ExtractionOutcomeDto::Damaged),
        ] {
            let fx = messages_fixture(name);
            let (events, jobs) = run_kind(&fx, JobKind::ExtractionCheck, None);
            match terminal(&events) {
                JobEvent::ExtractionFinished {
                    outcome: o,
                    staging_removed,
                    ..
                } => {
                    assert_eq!(*o, outcome, "{name}");
                    assert!(*staging_removed);
                }
                other => panic!("{name}: unexpected {other:?}"),
            }
            let ext = jobs
                .last(&backup_id_of(&fx))
                .extraction
                .expect("extraction");
            assert!(ext.qualifiers.is_empty(), "{name}");
            assert!(ext.counts.is_none() && ext.problems.is_none(), "{name}");
            assert!(ext.database.is_some(), "{name}");
            match outcome {
                ExtractionOutcomeDto::Unsupported => {
                    assert!(!ext.unsupported_reasons.is_empty(), "{name}")
                }
                _ => assert!(ext.damage_reason.is_some(), "{name}"),
            }
            assert!(staging_is_empty(&fx.library));
        }
    }

    /// A truncated sms.db (as an interrupted backup leaves it) passes
    /// validation's header check but cannot be opened: the job fails with
    /// the Messages-database sentence, and nothing stays staged.
    #[test]
    fn a_truncated_sms_db_fails_with_the_messages_sentence_and_cleans_up() {
        let fx = messages_fixture("msgs-full-plain");
        let blob = fx
            .dir
            .join(&oma_backup::SMS_DB_FILE_ID[..2])
            .join(oma_backup::SMS_DB_FILE_ID);
        let bytes = std::fs::read(&blob).expect("blob");
        std::fs::write(&blob, &bytes[..4096]).expect("truncate");
        let (events, _) = run_kind(&fx, JobKind::ExtractionCheck, None);
        match terminal(&events) {
            JobEvent::Failed {
                error,
                staging_removed,
                ..
            } => {
                assert_eq!(error.code, ErrorCode::MalformedBackup);
                assert_eq!(
                    error.message,
                    "The Messages database copy could not be opened (SQLite error 11)."
                );
                assert!(*staging_removed);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(staging_is_empty(&fx.library));
    }

    #[test]
    fn cancel_while_reading_messages_ends_cancelled() {
        let fx = messages_fixture("msgs-full-plain");
        let jobs = Arc::new(Jobs::default());
        let s = spec_kind(&fx, JobKind::ExtractionCheck, "", CancelToken::new(), None);
        let r = jobs
            .reserve(&s.backup_id, JobKind::ExtractionCheck)
            .expect("reserve");
        let s = JobSpec {
            job_id: r.job_id.clone(),
            cancel: r.cancel.clone(),
            ..s
        };
        /// Cancels as soon as the message pass is reported.
        struct CancelOnMessages(Recording, Arc<Jobs>, String);
        impl JobEventSink for CancelOnMessages {
            fn send(&mut self, event: JobEvent) {
                if matches!(
                    event,
                    JobEvent::Progress {
                        phase: JobPhase::ReadingMessages,
                        ..
                    }
                ) {
                    self.1.cancel(&self.2);
                }
                self.0.send(event);
            }
        }
        let rec = Recording::default();
        let mut sink = CancelOnMessages(rec.clone(), jobs.clone(), r.job_id.clone());
        run(s, &jobs, &mut sink);
        let events = rec.events();
        match terminal(&events) {
            JobEvent::Cancelled {
                staging_removed, ..
            } => assert!(*staging_removed),
            other => panic!("unexpected {other:?}"),
        }
        assert!(jobs.last(&backup_id_of(&fx)).extraction.is_none());
        assert!(staging_is_empty(&fx.library));
    }

    /// D47: the terminal event is a summary, far below the 8 KiB channel
    /// limit, whatever the report holds.
    #[test]
    fn the_largest_extraction_finished_event_is_under_1_kib() {
        let event = JobEvent::ExtractionFinished {
            job_id: format!("job-{}", u64::MAX),
            outcome: ExtractionOutcomeDto::SkippedNotImportable,
            source_changed: true,
            staging_removed: false,
            plaintext_removed_after_row_pass: false,
        };
        let json = serde_json::to_string(&event).expect("json");
        assert!(json.len() < 1024, "{} bytes", json.len());
        assert_eq!(
            serde_json::to_value(&event).expect("json")["kind"],
            "extractionFinished"
        );
    }

    /// A real report from the catalogue, for mapping tests.
    fn checked_report(name: &str) -> oma_messages::ExtractionReport {
        let fx = messages_fixture(name);
        let candidate = Candidate::for_dir(&fx.dir).expect("candidate");
        let limits = Limits::default();
        let inspection = inspect(&candidate, &limits, &mut 0).expect("inspect");
        let root = StagingRoot::new(&fx.library, &inspection.root).expect("root");
        let session = root.begin().expect("session");
        let token = CancelToken::new();
        let mut sink = |_: Progress| {};
        let mut ctx = JobCtx::new(&token, &mut sink);
        let Ok(ob) = OpenBackup::open(inspection, session, None, &limits, &mut ctx) else {
            panic!("the fixture opens");
        };
        let mut msink = |_: MessagesProgress| {};
        let mut mctx = MessagesCtx::new(&token, &mut msink);
        let report = oma_messages::check(&ob, &CheckOptions::default(), &mut mctx).expect("check");
        let _ = ob.close();
        report
    }

    fn assert_numbers_safe(v: &serde_json::Value, path: &str) {
        match v {
            serde_json::Value::Number(n) => {
                let max = crate::backups::dto::MAX_SAFE_INTEGER;
                if let Some(u) = n.as_u64() {
                    assert!(u <= max, "{path}: {u}");
                } else if let Some(i) = n.as_i64() {
                    assert!(i.unsigned_abs() <= max, "{path}: {i}");
                } else {
                    panic!("{path}: not an integer");
                }
            }
            serde_json::Value::Array(a) => {
                for (i, x) in a.iter().enumerate() {
                    assert_numbers_safe(x, &format!("{path}[{i}]"));
                }
            }
            serde_json::Value::Object(o) => {
                for (k, x) in o {
                    assert_numbers_safe(x, &format!("{path}.{k}"));
                }
            }
            _ => {}
        }
    }

    /// §16.4: every u64 is clamped (and sets `saturated`), and every raw
    /// Without an `attachment` table nothing was looked up, so the
    /// attachment section is `null` ("not recorded"), never a set of zeros.
    #[test]
    fn no_attachment_table_sends_no_attachment_counters() {
        let r = checked_report("msgs-minimal-plain");
        assert!(r.attachments.as_ref().is_some_and(|a| a.rows.is_none()));
        let dto = extraction::report_dto(
            &ExtractionEnding::Checked(Box::new(r)),
            "job-1",
            true,
            false,
        );
        assert!(dto.attachments.is_none());
        assert_eq!(dto.counts.expect("counts").attachment_rows, None);
        let full = checked_report("msgs-full-plain");
        let dto = extraction::report_dto(
            &ExtractionEnding::Checked(Box::new(full)),
            "job-2",
            true,
            false,
        );
        assert!(dto.attachments.is_some());
    }

    /// i64 outside ±(2^53 − 1) is withheld, never sent as a number.
    #[test]
    fn the_extraction_dto_clamps_u64_max_and_unsafe_i64() {
        let mut r = checked_report("msgs-full-plain");
        let max = u64::MAX;
        r.database.staged_bytes = max;
        r.database.page_size = Some(max);
        r.database.wal.bytes = Some(max);
        for t in &mut r.schema.tables {
            t.rows = Some(max);
        }
        r.schema.client_version = Some(i64::MAX);
        r.schema.client_version_parsed = true;
        if let Some(m) = r.messages.as_mut() {
            m.rows = max;
            m.with_association = Some(max);
            m.group_actions = Some(max);
            m.recently_deleted = Some(max);
            m.other_rows = Some(max);
        }
        if let Some(c) = r.chats.as_mut() {
            c.rows = max;
        }
        if let Some(h) = r.handles.as_mut() {
            h.rows = max;
        }
        if let Some(l) = r.links.as_mut() {
            l.messages_in_no_chat = Some(max);
        }
        if let Some(d) = r.dates.as_mut() {
            d.earliest_message = Some(i64::MIN);
            d.latest_message = Some(i64::MAX);
            d.excluded_scheduled = Some(max);
            d.excluded_before_floor = max;
            d.excluded_after_checked_at = max;
            d.excluded_not_classified = max;
        }
        if let Some(a) = r.attachments.as_mut() {
            a.rows = Some(max);
            a.present_blob_bytes = max;
            a.lookup.found = max;
            a.lookup.not_listed_at_sourced_key = max;
            a.lookup.not_found_at_assumed_key = max;
            a.lookup.ambiguous = max;
            a.lookup.invalid_entry = max;
            for e in &mut a.lookup.not_attempted {
                e.1 = max;
            }
            a.probes.present = max;
            a.probes.missing = max;
            a.probes.not_regular = max;
            a.probes.unreadable = max;
            a.probes.no_key = max;
            a.probes.no_protection_class = max;
            a.probes.key_does_not_unwrap = max;
            a.probes.class_key_unavailable = max;
            a.probes.not_a_file = max;
            a.probes.invalid_entry = max;
            a.probes.other = max;
            a.probes.not_probed = max;
        }
        if let Some(f) = r.failures.as_mut() {
            for e in &mut f.typedstream {
                e.1 = max;
            }
            f.summary_too_large = max;
            f.values.value_oversized = max;
            f.attachment_key_problem = max;
            f.orphan_join_row = max;
        }
        r.elapsed = Duration::MAX;
        let dto =
            extraction::report_dto(&ExtractionEnding::Checked(Box::new(r)), "job-1", true, true);
        let json = serde_json::to_value(&dto).expect("json");
        assert_numbers_safe(&json, "report");
        assert!(dto.saturated);
        let counts = dto.counts.expect("counts");
        assert_eq!(counts.message_rows, crate::backups::dto::MAX_SAFE_INTEGER);
        assert_eq!(
            counts.association_rows,
            Some(crate::backups::dto::MAX_SAFE_INTEGER),
            "a huge optional counter stays recorded"
        );
        let schema = dto.schema.expect("schema");
        assert_eq!(schema.client_version, None);
        assert!(schema.client_version_out_of_safe_range);
        assert!(json.to_string().find("bigint").is_none());
    }

    #[test]
    fn safe_i64_bounds() {
        let max = i64::try_from(crate::backups::dto::MAX_SAFE_INTEGER).expect("fits");
        assert_eq!(extraction::safe_i64(max), Some(max));
        assert_eq!(extraction::safe_i64(-max), Some(-max));
        assert_eq!(extraction::safe_i64(max + 1), None);
        assert_eq!(extraction::safe_i64(-max - 1), None);
        assert_eq!(extraction::safe_i64(i64::MIN), None);
        assert_eq!(extraction::safe_i64(0), Some(0));
    }
}
