//! Backup discovery, details and validation jobs: the shell side of
//! milestone 2.
//!
//! Nothing here depends on Tauri, so the registry, the job table and the
//! worker are tested without a window. The command layer
//! (`commands::backups`, `commands::jobs`) adds the dialogs, the blocking
//! pool and the `Channel` transport.
//!
//! Lock discipline: no lock in this module is held across file I/O, a
//! dialog, a blocking call or an await. Callers clone what they need and
//! drop the guard.

pub mod dto;
pub mod extraction;
pub mod listing;
pub mod messages;
pub mod worker;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use oma_backup::{CancelToken, Candidate};

use crate::archives::dto::{ArchiveJobDto, ImportPreflightDto};
use crate::archives::gate::{Gate, GateAnswer, GateClock, MonotonicClock, GATE_TIMEOUT};
use crate::error::{ErrorCode, IpcError, IpcResult};
use dto::{
    BackupList, BackupLocation, BackupSummary, CleanupDto, JobEvent, JobKind, JobPhase, JobSummary,
    ValidationReportDto,
};
use extraction::ExtractionReportDto;

/// One job at a time, globally (D7).
pub const MAX_CONCURRENT_JOBS: usize = 1;

/// Where a job's events go. `Channel<JobEvent>` implements it in the command
/// layer; tests use a recording sink. A failed delivery is not a signal that
/// the webview is gone, so it never cancels the job (§11.2).
pub trait JobEventSink: Send {
    fn send(&mut self, event: JobEvent);
}

// ---------------------------------------------------------------- registry

/// A scanned location: a default root or a folder the user added.
#[derive(Debug, Clone)]
pub struct LocationEntry {
    pub dto: BackupLocation,
    /// Added by the user (and so can be forgotten).
    pub added: bool,
}

/// A listed backup.
#[derive(Clone)]
pub struct BackupEntry {
    pub candidate: Candidate,
    pub summary: BackupSummary,
}

/// The last listing: ids the webview may echo back, and what they mean.
/// Ids are computed in Rust only; an unknown id is `backupNotFound`.
#[derive(Default)]
pub struct BackupRegistry {
    pub locations: Vec<LocationEntry>,
    pub order: Vec<String>,
    pub backups: HashMap<String, BackupEntry>,
    pub budget_exhausted: bool,
}

impl BackupRegistry {
    pub fn candidate(&self, backup_id: &str) -> Option<Candidate> {
        self.backups.get(backup_id).map(|b| b.candidate.clone())
    }

    pub fn location(&self, location_id: &str) -> Option<&LocationEntry> {
        self.locations.iter().find(|l| l.dto.id == location_id)
    }

    /// Store a freshly inspected summary (for example after a `notInspected`
    /// backup was selected).
    pub fn update_summary(&mut self, summary: BackupSummary) {
        if let Some(entry) = self.backups.get_mut(&summary.id) {
            entry.summary = summary;
        }
    }

    /// Drop a location and the backups listed under it.
    pub fn forget_location(&mut self, location_id: &str) {
        self.locations.retain(|l| l.dto.id != location_id);
        let gone: Vec<String> = self
            .backups
            .iter()
            .filter(|(_, b)| b.summary.location_id == location_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &gone {
            self.backups.remove(id);
        }
        self.order.retain(|id| !gone.contains(id));
        for loc in &mut self.locations {
            loc.dto.backup_count = self
                .backups
                .values()
                .filter(|b| b.summary.location_id == loc.dto.id)
                .count() as u64;
        }
    }

    /// The list, with each summary's job and report fields filled in from
    /// the job table at the moment of the call.
    pub fn list(&self, jobs: &Jobs) -> BackupList {
        BackupList {
            locations: self.locations.iter().map(|l| l.dto.clone()).collect(),
            backups: self
                .order
                .iter()
                .filter_map(|id| self.backups.get(id))
                .map(|b| jobs.decorate(b.summary.clone()))
                .collect(),
            budget_exhausted: self.budget_exhausted,
        }
    }
}

// ---------------------------------------------------------------- jobs

/// What a job works on (D73): a backup, an archive, or both (an import).
/// Two jobs that share either are never active together.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobSubject {
    pub backup_id: Option<String>,
    pub archive_id: Option<String>,
}

impl JobSubject {
    pub fn backup(id: &str) -> JobSubject {
        JobSubject {
            backup_id: Some(id.to_owned()),
            archive_id: None,
        }
    }

    pub fn archive(id: &str) -> JobSubject {
        JobSubject {
            backup_id: None,
            archive_id: Some(id.to_owned()),
        }
    }

    pub fn import(backup_id: &str, archive_id: &str) -> JobSubject {
        JobSubject {
            backup_id: Some(backup_id.to_owned()),
            archive_id: Some(archive_id.to_owned()),
        }
    }

    fn shares_backup(&self, other: &JobSubject) -> bool {
        self.backup_id.is_some() && self.backup_id == other.backup_id
    }

    fn shares_archive(&self, other: &JobSubject) -> bool {
        self.archive_id.is_some() && self.archive_id == other.archive_id
    }
}

impl From<&str> for JobSubject {
    fn from(backup_id: &str) -> Self {
        JobSubject::backup(backup_id)
    }
}

impl From<&String> for JobSubject {
    fn from(backup_id: &String) -> Self {
        JobSubject::backup(backup_id)
    }
}

/// A running job.
pub struct ActiveJob {
    pub subject: JobSubject,
    pub kind: JobKind,
    pub cancel: CancelToken,
    pub phase: Option<JobPhase>,
    /// An import's gate (spec §15.1).
    pub gate: Option<Arc<Gate>>,
    /// What the gate shows, while the import waits there.
    pub preflight: Option<ImportPreflightDto>,
}

/// What the last job for a backup left behind. In memory only.
///
/// `report` and `extraction` are replaced together, whenever a job ends
/// with a validation report: a Validate job sets `extraction` to `None`, so
/// an old extraction report is never shown beside a newer validation
/// (§16.1). A failed or cancelled job keeps both. Import summaries persist
/// in the archive instead (M4 §15.1).
#[derive(Debug, Clone, Default)]
pub struct LastOutcome {
    pub report: Option<ValidationReportDto>,
    pub extraction: Option<ExtractionReportDto>,
    pub cleanup: Option<CleanupDto>,
}

#[derive(Default)]
struct JobTable {
    active: HashMap<String, ActiveJob>,
    last: HashMap<String, LastOutcome>,
    completions: HashMap<String, Receiver<()>>,
}

/// The job slot handed to a worker. Dropping `done` signals completion.
pub struct Reservation {
    pub job_id: String,
    pub cancel: CancelToken,
    pub done: Sender<()>,
    /// For an import: the gate its worker waits at.
    pub gate: Option<Arc<Gate>>,
}

/// The job table: `Mutex<HashMap<JobId, ActiveJob>>`, an `AtomicU64`
/// counter, the last outcome per backup, and the completion receivers the
/// exit handler waits on.
pub struct Jobs {
    table: Mutex<JobTable>,
    counter: AtomicU64,
    gate_clock: Arc<dyn GateClock>,
    gate_timeout: Duration,
}

impl Default for Jobs {
    fn default() -> Self {
        Jobs::with_gate(Arc::new(MonotonicClock::default()), GATE_TIMEOUT)
    }
}

impl Jobs {
    /// A job table whose import gates use `clock` and `timeout` (tests).
    pub fn with_gate(clock: Arc<dyn GateClock>, timeout: Duration) -> Jobs {
        Jobs {
            table: Mutex::new(JobTable::default()),
            counter: AtomicU64::new(1),
            gate_clock: clock,
            gate_timeout: timeout,
        }
    }

    fn lock(&self) -> IpcResult<MutexGuard<'_, JobTable>> {
        self.table
            .lock()
            .map_err(|_| IpcError::internal("job table lock poisoned"))
    }

    /// Reserve the job slot for `subject`, or `busy`: when any active job
    /// shares its backup or its archive, or when the one slot is taken
    /// (D7, D73).
    pub fn reserve(&self, subject: impl Into<JobSubject>, kind: JobKind) -> IpcResult<Reservation> {
        let subject = subject.into();
        let mut t = self.lock()?;
        if t.active.values().any(|j| j.subject.shares_backup(&subject)) {
            return Err(IpcError::new(ErrorCode::Busy, messages::BUSY_SAME));
        }
        if t.active
            .values()
            .any(|j| j.subject.shares_archive(&subject))
        {
            return Err(IpcError::new(
                ErrorCode::Busy,
                crate::archives::messages::BUSY_ARCHIVE,
            ));
        }
        if t.active.len() >= MAX_CONCURRENT_JOBS {
            return Err(IpcError::new(ErrorCode::Busy, messages::BUSY_OTHER));
        }
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("job-{n}");
        let cancel = CancelToken::new();
        let (done, rx) = mpsc::channel();
        let gate = (kind == JobKind::Import)
            .then(|| Arc::new(Gate::new(self.gate_clock.clone(), self.gate_timeout)));
        t.active.insert(
            job_id.clone(),
            ActiveJob {
                subject,
                kind,
                cancel: cancel.clone(),
                phase: None,
                gate: gate.clone(),
                preflight: None,
            },
        );
        t.completions.insert(job_id.clone(), rx);
        Ok(Reservation {
            job_id,
            cancel,
            done,
            gate,
        })
    }

    /// Give up a reservation whose worker never started.
    pub fn release(&self, job_id: &str) {
        if let Ok(mut t) = self.lock() {
            t.active.remove(job_id);
            t.completions.remove(job_id);
        }
    }

    /// Request cancellation. False when the job is unknown or already over.
    /// A job waiting at the gate wakes and declines.
    pub fn cancel(&self, job_id: &str) -> bool {
        let gate = match self.lock() {
            Ok(t) => match t.active.get(job_id) {
                Some(j) => {
                    j.cancel.cancel();
                    j.gate.clone()
                }
                None => return false,
            },
            Err(_) => return false,
        };
        if let Some(g) = gate {
            g.wake();
        }
        true
    }

    pub fn set_phase(&self, job_id: &str, phase: JobPhase) {
        if let Ok(mut t) = self.lock() {
            if let Some(j) = t.active.get_mut(job_id) {
                j.phase = Some(phase);
            }
        }
    }

    /// Show (or clear) what an import's gate asks.
    pub fn set_preflight(&self, job_id: &str, preflight: Option<ImportPreflightDto>) {
        if let Ok(mut t) = self.lock() {
            if let Some(j) = t.active.get_mut(job_id) {
                j.preflight = preflight;
            }
        }
    }

    /// Answer an import waiting at its gate (`confirm_import`). A
    /// lineage the preflight does not list is `invalidInput`; a job that is
    /// unknown, over, not an import or not at the gate is `jobNotWaiting`.
    pub fn confirm(&self, job_id: &str, answer: GateAnswer) -> IpcResult<()> {
        use crate::archives::messages as am;
        let gate = {
            let t = self.lock()?;
            let job = t.active.get(job_id);
            let (Some(gate), Some(preflight)) = (
                job.and_then(|j| j.gate.clone()),
                job.and_then(|j| j.preflight.as_ref()),
            ) else {
                return Err(IpcError::new(ErrorCode::JobNotWaiting, am::JOB_NOT_WAITING));
            };
            if let GateAnswer::Proceed(oma_archive::LineageChoice::Existing(id)) = answer {
                if !preflight.lineages.iter().any(|l| l.lineage_id == id.get()) {
                    return Err(IpcError::new(ErrorCode::InvalidInput, am::UNKNOWN_LINEAGE));
                }
            }
            gate
        };
        gate.answer(answer)
            .map_err(|_| IpcError::new(ErrorCode::JobNotWaiting, am::JOB_NOT_WAITING))
    }

    /// Store the outcome and free the slot, atomically, before the terminal
    /// event is sent: when the UI sees `finished`, "Validate again" is
    /// already allowed and `get_backup_details` already has the report.
    pub fn finish(&self, job_id: &str, outcome: LastOutcome) {
        if let Ok(mut t) = self.lock() {
            if let Some(job) = t.active.remove(job_id) {
                if let Some(backup_id) = job.subject.backup_id {
                    let last = t.last.entry(backup_id).or_default();
                    if outcome.report.is_some() {
                        last.report = outcome.report;
                        last.extraction = outcome.extraction;
                    }
                    last.cleanup = outcome.cleanup;
                }
            }
            t.completions.remove(job_id);
        }
    }

    /// The active job on this backup, if any.
    pub fn active_for(&self, backup_id: &str) -> Option<JobSummary> {
        let t = self.lock().ok()?;
        t.active
            .iter()
            .find(|(_, j)| j.subject.backup_id.as_deref() == Some(backup_id))
            .map(|(id, j)| JobSummary {
                job_id: id.clone(),
                backup_id: backup_id.to_owned(),
                kind: j.kind,
                phase: j.phase,
            })
    }

    /// The active job on this archive, and what its gate asks, if any.
    pub fn active_for_archive(
        &self,
        archive_id: &str,
    ) -> Option<(ArchiveJobDto, Option<ImportPreflightDto>)> {
        let t = self.lock().ok()?;
        t.active
            .iter()
            .find(|(_, j)| j.subject.archive_id.as_deref() == Some(archive_id))
            .map(|(id, j)| {
                (
                    ArchiveJobDto {
                        job_id: id.clone(),
                        kind: j.kind,
                        phase: j.phase,
                        backup_id: j.subject.backup_id.clone(),
                        waiting_at_gate: j.preflight.is_some(),
                    },
                    j.preflight.clone(),
                )
            })
    }

    pub fn any_active(&self) -> bool {
        self.lock().map(|t| !t.active.is_empty()).unwrap_or(true)
    }

    pub fn last(&self, backup_id: &str) -> LastOutcome {
        self.lock()
            .ok()
            .and_then(|t| t.last.get(backup_id).cloned())
            .unwrap_or_default()
    }

    /// Fill in `activeJobId` and `lastReportImportable`.
    pub fn decorate(&self, mut s: BackupSummary) -> BackupSummary {
        s.active_job_id = self.active_for(&s.id).map(|j| j.job_id);
        s.last_report_importable = self.last(&s.id).report.map(|r| r.importable);
        s
    }

    /// The `RunEvent::Exit` handler: cancel every job and wake every gate,
    /// then wait for the workers to finish their cleanup, up to `budget` in
    /// total. A batch in flight finishes or rolls back. Workers are
    /// `std::thread`s, so this cannot deadlock an async runtime.
    pub fn cancel_all_and_wait(&self, budget: Duration) -> bool {
        let (receivers, gates): (Vec<Receiver<()>>, Vec<Arc<Gate>>) = match self.lock() {
            Ok(mut t) => {
                let mut gates = Vec::new();
                for j in t.active.values() {
                    j.cancel.cancel();
                    gates.extend(j.gate.clone());
                }
                (t.completions.drain().map(|(_, rx)| rx).collect(), gates)
            }
            Err(_) => return false,
        };
        for g in gates {
            g.wake();
        }
        let deadline = Instant::now() + budget;
        let mut all = true;
        for rx in receivers {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
                Err(RecvTimeoutError::Timeout) => all = false,
            }
        }
        all
    }
}

/// A `list_backups` in progress. Released on drop.
pub struct ListingGuard<'a>(&'a AtomicBool);

impl<'a> ListingGuard<'a> {
    pub fn try_acquire(flag: &'a AtomicBool) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| ListingGuard(flag))
    }
}

impl Drop for ListingGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_job_at_a_time() {
        let jobs = Jobs::default();
        let a = jobs.reserve("backup-a", JobKind::Validate).expect("first");
        assert_eq!(a.job_id, "job-1");
        let same = jobs
            .reserve("backup-a", JobKind::Validate)
            .err()
            .expect("busy");
        assert_eq!(same.code, ErrorCode::Busy);
        assert_eq!(same.message, messages::BUSY_SAME);
        let other = jobs
            .reserve("backup-b", JobKind::Validate)
            .err()
            .expect("busy");
        assert_eq!(other.code, ErrorCode::Busy);
        assert_eq!(other.message, messages::BUSY_OTHER);
        assert!(jobs.any_active());
        jobs.finish(&a.job_id, LastOutcome::default());
        assert!(!jobs.any_active());
        let b = jobs
            .reserve("backup-b", JobKind::Validate)
            .expect("free again");
        assert_eq!(b.job_id, "job-2");
    }

    /// D73: a job that shares the backup or the archive of an active job
    /// is busy, with the sentence for which one; otherwise the one slot
    /// decides (D7).
    #[test]
    fn reserve_is_busy_by_subject() {
        use crate::archives::messages::BUSY_ARCHIVE;
        let jobs = Jobs::default();
        let import = jobs
            .reserve(JobSubject::import("b1", "a1"), JobKind::Import)
            .expect("import");
        assert!(import.gate.is_some());
        let busy = |s: JobSubject, k| jobs.reserve(s, k).err().expect("busy");
        let e = busy(JobSubject::backup("b1"), JobKind::Validate);
        assert_eq!(
            (e.code, e.message.as_str()),
            (ErrorCode::Busy, messages::BUSY_SAME)
        );
        let e = busy(JobSubject::archive("a1"), JobKind::Verify);
        assert_eq!(
            (e.code, e.message.as_str()),
            (ErrorCode::Busy, BUSY_ARCHIVE)
        );
        let e = busy(JobSubject::import("b2", "a1"), JobKind::Import);
        assert_eq!(e.message, BUSY_ARCHIVE);
        let e = busy(JobSubject::archive("a2"), JobKind::Verify);
        assert_eq!(e.message, messages::BUSY_OTHER);
        // The import shows on its backup and on its archive.
        assert_eq!(jobs.active_for("b1").map(|j| j.kind), Some(JobKind::Import));
        let (job, preflight) = jobs.active_for_archive("a1").expect("on the archive");
        assert_eq!(job.backup_id.as_deref(), Some("b1"));
        assert!(!job.waiting_at_gate && preflight.is_none());
        assert!(jobs.active_for_archive("a2").is_none());
        jobs.finish(&import.job_id, LastOutcome::default());
        // A verify has no backup and leaves no backup outcome behind.
        let v = jobs
            .reserve(JobSubject::archive("a1"), JobKind::Verify)
            .expect("verify");
        assert!(v.gate.is_none());
        assert!(jobs.active_for("b1").is_none());
        jobs.finish(&v.job_id, LastOutcome::default());
        assert!(jobs.last("b1").report.is_none());
    }

    /// The exit handler wakes an import waiting at its gate.
    #[test]
    fn exit_wakes_a_waiting_gate() {
        let jobs = std::sync::Arc::new(Jobs::default());
        let r = jobs
            .reserve(JobSubject::import("b", "a"), JobKind::Import)
            .expect("reserve");
        let gate = r.gate.clone().expect("gate");
        let watched = gate.clone();
        let worker_jobs = jobs.clone();
        let handle = std::thread::spawn(move || {
            let ending = gate.wait(&r.cancel);
            worker_jobs.finish(&r.job_id, LastOutcome::default());
            drop(r.done);
            ending
        });
        let started = Instant::now();
        while !watched.is_waiting() {
            std::thread::yield_now();
        }
        assert!(jobs.cancel_all_and_wait(Duration::from_secs(5)));
        assert_eq!(
            handle.join().expect("join"),
            crate::archives::gate::GateEnding::Cancelled
        );
        assert!(started.elapsed() < GATE_TIMEOUT);
    }

    #[test]
    fn cancel_unknown_or_finished_returns_false() {
        let jobs = Jobs::default();
        assert!(!jobs.cancel("job-99"));
        let r = jobs.reserve("b", JobKind::Validate).expect("reserve");
        assert!(jobs.cancel(&r.job_id));
        assert!(r.cancel.is_cancelled());
        jobs.finish(&r.job_id, LastOutcome::default());
        assert!(!jobs.cancel(&r.job_id));
    }

    #[test]
    fn exit_waits_for_workers_and_cancels_them() {
        let jobs = std::sync::Arc::new(Jobs::default());
        let r = jobs.reserve("b", JobKind::Validate).expect("reserve");
        let worker_jobs = jobs.clone();
        let handle = std::thread::spawn(move || {
            while !r.cancel.is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            worker_jobs.finish(&r.job_id, LastOutcome::default());
            drop(r.done);
        });
        assert!(jobs.cancel_all_and_wait(Duration::from_secs(5)));
        handle.join().expect("join");
        assert!(!jobs.any_active());
    }

    fn validation(importable: bool) -> ValidationReportDto {
        let stats = dto::ManifestStatsDto {
            total_rows: 0,
            files: 0,
            directories: 0,
            symlinks: 0,
            other_flags: 0,
            declared_bytes: 0,
            declared_bytes_saturated: false,
            missing_blobs: 0,
            size_mismatches: 0,
            unsafe_blobs: 0,
            unreadable_blobs: 0,
            invalid_rows: 0,
            invalid_file_ids: 0,
            file_id_mismatches: 0,
            paths_unusual_for_export: 0,
            unreadable_records: 0,
            files_without_key: 0,
            files_with_unavailable_class: 0,
            has_properties_table: true,
            views: 0,
            triggers: 0,
            saturated: false,
        };
        ValidationReportDto {
            importable,
            checks: Vec::new(),
            stats,
            sms_db: dto::SmsDbStatusDto {
                listed: true,
                ambiguous: false,
                blob_present: Some(true),
                declared_size: None,
                size_consistent: None,
                header_ok: Some(true),
                padding_ok: None,
                wal_listed: false,
                shm_listed: false,
            },
            duration_ms: 0,
            validated_at: "2026-10-18T00:00:00Z".into(),
        }
    }

    /// §16.1: a Validate job that follows an extraction check replaces the
    /// whole outcome, so the old extraction report is never shown beside
    /// the newer validation. A failed job keeps both.
    #[test]
    fn a_later_validation_clears_the_extraction_report() {
        let jobs = Jobs::default();
        let r = jobs
            .reserve("b", JobKind::ExtractionCheck)
            .expect("reserve");
        assert_eq!(
            jobs.active_for("b").map(|j| j.kind),
            Some(JobKind::ExtractionCheck)
        );
        let skipped = extraction::report_dto(
            &extraction::ExtractionEnding::SkippedNotImportable,
            &r.job_id,
            true,
            true,
        );
        jobs.finish(
            &r.job_id,
            LastOutcome {
                report: Some(validation(false)),
                extraction: Some(skipped),
                cleanup: Some(CleanupDto {
                    staging_removed: true,
                }),
            },
        );
        assert!(jobs.last("b").extraction.is_some());

        let r = jobs.reserve("b", JobKind::Validate).expect("reserve");
        jobs.finish(
            &r.job_id,
            LastOutcome {
                report: None,
                extraction: None,
                cleanup: None,
            },
        );
        assert!(
            jobs.last("b").extraction.is_some(),
            "a failed or cancelled job keeps the last reports"
        );

        let r = jobs.reserve("b", JobKind::Validate).expect("reserve");
        jobs.finish(
            &r.job_id,
            LastOutcome {
                report: Some(validation(true)),
                extraction: None,
                cleanup: Some(CleanupDto {
                    staging_removed: true,
                }),
            },
        );
        let last = jobs.last("b");
        assert!(last.report.is_some_and(|r| r.importable));
        assert!(last.extraction.is_none());
    }

    #[test]
    fn listing_guard_is_exclusive() {
        let flag = AtomicBool::new(false);
        let g = ListingGuard::try_acquire(&flag).expect("first");
        assert!(ListingGuard::try_acquire(&flag).is_none());
        drop(g);
        assert!(ListingGuard::try_acquire(&flag).is_some());
    }

    #[test]
    fn unknown_ids_are_not_found() {
        let reg = BackupRegistry::default();
        assert!(reg.candidate("0123").is_none());
        assert!(reg.location("0123").is_none());
    }
}
