//! Cooperative cancellation and progress reporting.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::error::BackupError;

/// A cancellation flag shared between the job and whoever may cancel it.
/// Cloning shares the flag.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A fresh, uncancelled token.
    pub fn new() -> Self {
        CancelToken::default()
    }

    /// Request cancellation. Work stops at its next checkpoint.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    /// `true` once [`CancelToken::cancel`] has been called.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// `Err(BackupError::Cancelled)` once cancelled.
    pub fn check(&self) -> Result<(), BackupError> {
        if self.is_cancelled() {
            Err(BackupError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// The phase a job is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Copying `Manifest.db` (and its `-wal`) into staging.
    CopyingManifest,
    /// Deriving the passphrase key from the password.
    DerivingKey,
    /// Unwrapping class keys.
    UnwrappingKeys,
    /// Decrypting `Manifest.db` into staging.
    DecryptingManifest,
    /// Structural checks on the staged `Manifest.db`.
    CheckingDatabase,
    /// The row pass over `Files`.
    CheckingFiles,
    /// The in-place check of sms.db.
    CheckingMessagesDb,
    /// Extracting a file into staging (the M3 extraction API; not used by
    /// validation).
    Extracting,
    /// Copying one listed file into a caller's writer
    /// ([`crate::OpenBackup::copy_file_to`]; the M4 attachment copy).
    /// `done` and `total` are bytes of the stored blob.
    Copying,
}

/// One progress report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// The current phase.
    pub phase: Phase,
    /// Units done (bytes, rows or iterations, depending on the phase).
    pub done: u64,
    /// Total units, when known.
    pub total: Option<u64>,
}

/// Receives progress reports.
pub trait ProgressSink {
    /// Called with each report. Must be cheap; it runs on the job thread.
    fn report(&mut self, p: Progress);
}

impl<F: FnMut(Progress)> ProgressSink for F {
    fn report(&mut self, p: Progress) {
        self(p)
    }
}

/// Bundles cancellation and progress so that no signature needs more than
/// seven parameters.
pub struct JobCtx<'a> {
    /// The job's cancellation token.
    pub cancel: &'a CancelToken,
    /// The job's progress sink.
    pub progress: &'a mut dyn ProgressSink,
}

impl<'a> JobCtx<'a> {
    /// Bundle a token and a sink.
    pub fn new(cancel: &'a CancelToken, progress: &'a mut dyn ProgressSink) -> Self {
        JobCtx { cancel, progress }
    }

    /// Send one report.
    pub(crate) fn report(&mut self, phase: Phase, done: u64, total: Option<u64>) {
        self.progress.report(Progress { phase, done, total });
    }

    /// `Err(Cancelled)` once cancelled.
    pub(crate) fn check(&self) -> Result<(), BackupError> {
        self.cancel.check()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_the_flag() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(a.check().is_ok());
        b.cancel();
        assert!(a.is_cancelled());
        assert!(matches!(a.check(), Err(BackupError::Cancelled)));
    }

    #[test]
    fn closures_are_sinks() {
        let mut seen = Vec::new();
        {
            let token = CancelToken::new();
            let mut sink = |p: Progress| seen.push(p);
            let mut ctx = JobCtx::new(&token, &mut sink);
            ctx.report(Phase::CheckingFiles, 3, Some(10));
        }
        assert_eq!(
            seen,
            vec![Progress {
                phase: Phase::CheckingFiles,
                done: 3,
                total: Some(10)
            }]
        );
    }
}
