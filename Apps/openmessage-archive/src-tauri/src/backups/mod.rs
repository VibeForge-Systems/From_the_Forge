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
pub mod listing;
pub mod messages;
pub mod worker;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use oma_backup::{CancelToken, Candidate};

use crate::error::{ErrorCode, IpcError, IpcResult};
use dto::{
    BackupList, BackupLocation, BackupSummary, CleanupDto, JobEvent, JobPhase, JobSummary,
    ValidationReportDto,
};

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

/// A running job.
pub struct ActiveJob {
    pub backup_id: String,
    pub cancel: CancelToken,
    pub phase: Option<JobPhase>,
}

/// What the last job for a backup left behind. In memory only.
#[derive(Debug, Clone, Default)]
pub struct LastOutcome {
    pub report: Option<ValidationReportDto>,
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
}

/// The job table: `Mutex<HashMap<JobId, ActiveJob>>`, an `AtomicU64`
/// counter, the last outcome per backup, and the completion receivers the
/// exit handler waits on.
pub struct Jobs {
    table: Mutex<JobTable>,
    counter: AtomicU64,
}

impl Default for Jobs {
    fn default() -> Self {
        Jobs {
            table: Mutex::new(JobTable::default()),
            counter: AtomicU64::new(1),
        }
    }
}

impl Jobs {
    fn lock(&self) -> IpcResult<MutexGuard<'_, JobTable>> {
        self.table
            .lock()
            .map_err(|_| IpcError::internal("job table lock poisoned"))
    }

    /// Reserve the job slot for `backup_id`, or `busy`.
    pub fn reserve(&self, backup_id: &str) -> IpcResult<Reservation> {
        let mut t = self.lock()?;
        if t.active.values().any(|j| j.backup_id == backup_id) {
            return Err(IpcError::new(ErrorCode::Busy, messages::BUSY_SAME));
        }
        if t.active.len() >= MAX_CONCURRENT_JOBS {
            return Err(IpcError::new(ErrorCode::Busy, messages::BUSY_OTHER));
        }
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let job_id = format!("job-{n}");
        let cancel = CancelToken::new();
        let (done, rx) = mpsc::channel();
        t.active.insert(
            job_id.clone(),
            ActiveJob {
                backup_id: backup_id.to_owned(),
                cancel: cancel.clone(),
                phase: None,
            },
        );
        t.completions.insert(job_id.clone(), rx);
        Ok(Reservation {
            job_id,
            cancel,
            done,
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
    pub fn cancel(&self, job_id: &str) -> bool {
        match self.lock() {
            Ok(t) => match t.active.get(job_id) {
                Some(j) => {
                    j.cancel.cancel();
                    true
                }
                None => false,
            },
            Err(_) => false,
        }
    }

    pub fn set_phase(&self, job_id: &str, phase: JobPhase) {
        if let Ok(mut t) = self.lock() {
            if let Some(j) = t.active.get_mut(job_id) {
                j.phase = Some(phase);
            }
        }
    }

    /// Store the outcome and free the slot, atomically, before the terminal
    /// event is sent: when the UI sees `finished`, "Validate again" is
    /// already allowed and `get_backup_details` already has the report.
    pub fn finish(&self, job_id: &str, outcome: LastOutcome) {
        if let Ok(mut t) = self.lock() {
            if let Some(job) = t.active.remove(job_id) {
                let last = t.last.entry(job.backup_id).or_default();
                if outcome.report.is_some() {
                    last.report = outcome.report;
                }
                last.cleanup = outcome.cleanup;
            }
            t.completions.remove(job_id);
        }
    }

    pub fn active_for(&self, backup_id: &str) -> Option<JobSummary> {
        let t = self.lock().ok()?;
        t.active
            .iter()
            .find(|(_, j)| j.backup_id == backup_id)
            .map(|(id, j)| JobSummary {
                job_id: id.clone(),
                backup_id: j.backup_id.clone(),
                phase: j.phase,
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

    /// The `RunEvent::Exit` handler: cancel every job, then wait for the
    /// workers to finish their cleanup, up to `budget` in total. Workers are
    /// `std::thread`s, so this cannot deadlock an async runtime.
    pub fn cancel_all_and_wait(&self, budget: Duration) -> bool {
        let receivers: Vec<Receiver<()>> = match self.lock() {
            Ok(mut t) => {
                for j in t.active.values() {
                    j.cancel.cancel();
                }
                t.completions.drain().map(|(_, rx)| rx).collect()
            }
            Err(_) => return false,
        };
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
        let a = jobs.reserve("backup-a").expect("first");
        assert_eq!(a.job_id, "job-1");
        let same = jobs.reserve("backup-a").err().expect("busy");
        assert_eq!(same.code, ErrorCode::Busy);
        assert_eq!(same.message, messages::BUSY_SAME);
        let other = jobs.reserve("backup-b").err().expect("busy");
        assert_eq!(other.code, ErrorCode::Busy);
        assert_eq!(other.message, messages::BUSY_OTHER);
        assert!(jobs.any_active());
        jobs.finish(&a.job_id, LastOutcome::default());
        assert!(!jobs.any_active());
        let b = jobs.reserve("backup-b").expect("free again");
        assert_eq!(b.job_id, "job-2");
    }

    #[test]
    fn cancel_unknown_or_finished_returns_false() {
        let jobs = Jobs::default();
        assert!(!jobs.cancel("job-99"));
        let r = jobs.reserve("b").expect("reserve");
        assert!(jobs.cancel(&r.job_id));
        assert!(r.cancel.is_cancelled());
        jobs.finish(&r.job_id, LastOutcome::default());
        assert!(!jobs.cancel(&r.job_id));
    }

    #[test]
    fn exit_waits_for_workers_and_cancels_them() {
        let jobs = std::sync::Arc::new(Jobs::default());
        let r = jobs.reserve("b").expect("reserve");
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
