//! Progress and cancellation for the messages check (M3 §4.5, D24).
//!
//! `oma_backup::Phase` stays free of M3 knowledge: the messages check has
//! its own phases, and a private adapter maps the backup crate's
//! `Extracting` reports onto [`MessagesPhase::StagingMessagesDb`].

use oma_backup::{CancelToken, Phase, Progress, ProgressSink};

use crate::error::MessagesError;

/// A phase of the messages check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MessagesPhase {
    /// Copying sms.db and its `-wal` into staging; one combined byte total.
    StagingMessagesDb,
    /// The `sqlite_schema` scan and the virtual-table refusal.
    ScanningSchema,
    /// `quick_check`; no counts.
    CheckingIntegrity,
    /// The probe; `done` counts known tables probed.
    ReadingSchema,
    /// The `handle` pass; `done` counts rows.
    ReadingHandles,
    /// The `chat` pass.
    ReadingChats,
    /// The chat link passes (`chat_handle_join`, `chat_message_join`,
    /// `chat_recoverable_message_join`).
    ReadingChatLinks,
    /// The `message` pass.
    ReadingMessages,
    /// The attachment passes.
    CheckingAttachments,
    /// The aggregates.
    Summarising,
}

/// One progress report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessagesProgress {
    /// The current phase.
    pub phase: MessagesPhase,
    /// Units done (bytes, tables or rows, depending on the phase).
    pub done: u64,
    /// Total units, when known.
    pub total: Option<u64>,
}

/// Receives progress reports.
pub trait MessagesSink {
    /// Called with each report. Must be cheap; it runs on the job thread.
    fn report(&mut self, p: MessagesProgress);
}

impl<F: FnMut(MessagesProgress)> MessagesSink for F {
    fn report(&mut self, p: MessagesProgress) {
        self(p)
    }
}

/// Cancellation and progress for one messages check.
pub struct MessagesCtx<'a> {
    /// The job's cancellation token.
    pub cancel: &'a CancelToken,
    /// The job's progress sink.
    pub progress: &'a mut dyn MessagesSink,
    last_phase: Option<MessagesPhase>,
}

impl<'a> MessagesCtx<'a> {
    /// Bundle a token and a sink.
    pub fn new(cancel: &'a CancelToken, progress: &'a mut dyn MessagesSink) -> Self {
        MessagesCtx {
            cancel,
            progress,
            last_phase: None,
        }
    }

    /// Send one report. A change of phase is logged once, at `info`, by
    /// kind only.
    pub(crate) fn report(&mut self, phase: MessagesPhase, done: u64, total: Option<u64>) {
        if self.last_phase != Some(phase) {
            self.last_phase = Some(phase);
            tracing::info!(phase = ?phase, "messages check phase");
        }
        self.progress
            .report(MessagesProgress { phase, done, total });
    }

    /// `Err(Cancelled)` once cancelled.
    pub(crate) fn check(&self) -> Result<(), MessagesError> {
        if self.cancel.is_cancelled() {
            Err(MessagesError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Forwards `oma_backup`'s staging progress as
/// [`MessagesPhase::StagingMessagesDb`]. Other backup phases are not part of
/// the messages check and are dropped.
pub(crate) struct BackupSinkAdapter<'p> {
    pub(crate) inner: &'p mut dyn MessagesSink,
}

impl ProgressSink for BackupSinkAdapter<'_> {
    fn report(&mut self, p: Progress) {
        if p.phase == Phase::Extracting {
            self.inner.report(MessagesProgress {
                phase: MessagesPhase::StagingMessagesDb,
                done: p.done,
                total: p.total,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_adapter_maps_extracting_only() {
        let mut seen = Vec::new();
        {
            let mut sink = |p: MessagesProgress| seen.push(p);
            let mut a = BackupSinkAdapter { inner: &mut sink };
            a.report(Progress {
                phase: Phase::Extracting,
                done: 5,
                total: Some(9),
            });
            a.report(Progress {
                phase: Phase::CheckingFiles,
                done: 1,
                total: None,
            });
        }
        assert_eq!(
            seen,
            vec![MessagesProgress {
                phase: MessagesPhase::StagingMessagesDb,
                done: 5,
                total: Some(9)
            }]
        );
    }

    #[test]
    fn the_context_reports_and_checks() {
        let token = CancelToken::new();
        let mut seen = Vec::new();
        {
            let mut sink = |p: MessagesProgress| seen.push(p.phase);
            let mut ctx = MessagesCtx::new(&token, &mut sink);
            ctx.report(MessagesPhase::ScanningSchema, 0, None);
            ctx.report(MessagesPhase::ScanningSchema, 1, None);
            assert!(ctx.check().is_ok());
            token.cancel();
            assert!(matches!(ctx.check(), Err(MessagesError::Cancelled)));
        }
        assert_eq!(seen.len(), 2);
    }
}
