//! `Archive`: one opened, locked archive (spec §4.3, §5.3, §5.4).
//!
//! Opening writes nothing: the archive holds its lock and a ReadOnly
//! connection. The first write is [`Archive::begin_write`], which opens the
//! ReadWrite connection and, unless told to skip it, cleans up what a crash
//! left behind. [`Archive::migrate`] is the only way to an upgraded schema.

use std::sync::Arc;

use rusqlite::TransactionBehavior;

use crate::blobs::{reconcile_intents, sweep_incoming};
use crate::conn::WriteConn;
use crate::error::{primary_code, ArchiveError, Stage};
use crate::id::ArchiveId;
use crate::layout::ArchiveDir;
use crate::lock::ArchiveLock;
use crate::progress::{ArchiveCtx, ArchiveEvent, Clock};
use crate::provenance;
use crate::reader::{ArchiveReader, ImportBlock};
use crate::schema::codes::SessionState;
use crate::schema::fingerprint;
use crate::schema::migrate::{run_step, MigrationReport};
use crate::schema::SchemaPlan;
use crate::space::{migration_needs, reserve_for, SpaceProbe};
use crate::store_fs::StoreFs;
use crate::APP_VERSION;

/// Whether [`Archive::begin_write`] runs the crash cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Sweep partial files and temporary provenance files, reconcile blob
    /// intents and mark running sessions interrupted (the lock proves their
    /// writer is gone). Used before an import.
    Run,
    /// Open the writer only, so a verify still sees what a crash left.
    Skip,
}

/// What [`Archive::begin_write`]'s recovery did. Counts only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct RecoveryReport {
    /// `false` with [`Recovery::Skip`], or when recovery already ran on this
    /// open.
    pub ran: bool,
    pub partials_removed: u32,
    pub partials_left: u32,
    pub provenance_tmp_removed: u32,
    pub provenance_tmp_left: u32,
    /// Intents whose `blob` row exists (dropped).
    pub intents_committed: u32,
    /// Intents without a `blob` row (the placed file was removed).
    pub intents_rolled_back: u32,
    /// Placed files that could not be removed; their intents stay.
    pub intent_files_left: u32,
    /// Sessions in state 0 marked interrupted (4).
    pub sessions_interrupted: u32,
}

/// The result of the close-time checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CheckpointOutcome {
    /// The WAL was checkpointed and truncated.
    Done,
    /// A reader kept the checkpoint from finishing; SQLite completes it
    /// later.
    Busy,
    /// The checkpoint failed with this primary code.
    Failed(Option<i32>),
}

/// What [`Archive::close`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct CloseReport {
    /// `None` when no writer was opened (nothing to checkpoint).
    pub checkpoint: Option<CheckpointOutcome>,
}

/// One opened archive: its lock, a ReadOnly connection and, after
/// [`Archive::begin_write`], a ReadWrite one.
pub struct Archive {
    // Field order is drop order: connections close before the lock goes.
    writer: Option<WriteConn>,
    reader: ArchiveReader,
    dir: ArchiveDir,
    _lock: ArchiveLock,
    recovered: bool,
    plan: SchemaPlan,
    space: Arc<dyn SpaceProbe>,
    clock: Arc<dyn Clock>,
    store: Arc<dyn StoreFs>,
}

impl std::fmt::Debug for Archive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Archive")
            .field("schema_version", &self.reader.schema_version())
            .field("writer", &self.writer.is_some())
            .finish_non_exhaustive()
    }
}

impl Archive {
    pub(crate) fn new(
        lock: ArchiveLock,
        reader: ArchiveReader,
        dir: ArchiveDir,
        plan: SchemaPlan,
        space: Arc<dyn SpaceProbe>,
        clock: Arc<dyn Clock>,
        store: Arc<dyn StoreFs>,
    ) -> Archive {
        Archive {
            writer: None,
            reader,
            dir,
            _lock: lock,
            recovered: false,
            plan,
            space,
            clock,
            store,
        }
    }

    /// The reader on this archive's ReadOnly connection.
    pub fn reader(&self) -> &ArchiveReader {
        &self.reader
    }

    pub fn id(&self) -> &ArchiveId {
        self.reader.id()
    }

    pub fn schema_version(&self) -> u32 {
        self.reader.schema_version()
    }

    /// The archive has an older schema: only [`Archive::migrate`] proceeds.
    pub fn needs_migration(&self) -> bool {
        self.reader.schema_version() < self.plan.version
    }

    /// Set when imports are refused although reading and verifying work.
    pub fn import_blocked(&self) -> Option<ImportBlock> {
        self.reader.inspected().import_blocked
    }

    /// What the attachment store and the space guards work with: the
    /// space probe, the archive directory and the store's file operations.
    pub(crate) fn store_parts(&self) -> (Arc<dyn SpaceProbe>, ArchiveDir, Arc<dyn StoreFs>) {
        (
            Arc::clone(&self.space),
            self.dir.clone(),
            Arc::clone(&self.store),
        )
    }

    /// The schema plan this archive was opened under.
    pub(crate) fn plan(&self) -> SchemaPlan {
        self.plan
    }

    /// What the verifier works with at once: the reader, the writer (when
    /// [`Archive::begin_write`] opened one) and the archive directory.
    pub(crate) fn verify_parts(&mut self) -> (&ArchiveReader, Option<&mut WriteConn>, &ArchiveDir) {
        (&self.reader, self.writer.as_mut(), &self.dir)
    }

    /// Open the attachment file of blob `id`; see
    /// [`ArchiveReader::open_blob`].
    pub fn open_blob(
        &self,
        id: crate::reader::BlobId,
    ) -> Result<crate::blobs::BlobReader, ArchiveError> {
        self.reader.open_blob(id)
    }

    pub(crate) fn ensure_writer(&mut self) -> Result<&mut WriteConn, ArchiveError> {
        if self.writer.is_none() {
            self.writer = Some(WriteConn::open(&self.dir)?);
        }
        self.writer
            .as_mut()
            .ok_or(ArchiveError::Internal("writer missing"))
    }

    /// The first write (§5.3): open the ReadWrite connection and, with
    /// [`Recovery::Run`], clean up after a crash. Refused on an archive
    /// that needs a migration.
    pub fn begin_write(&mut self, recovery: Recovery) -> Result<RecoveryReport, ArchiveError> {
        if self.needs_migration() {
            return Err(ArchiveError::NeedsMigration {
                found: self.schema_version(),
            });
        }
        let already_recovered = self.recovered;
        let now = self.clock.now_ns();
        let dir = self.dir.clone();
        let store = Arc::clone(&self.store);
        let w = self.ensure_writer()?;
        let mut report = RecoveryReport::default();
        if recovery == Recovery::Skip || already_recovered {
            return Ok(report);
        }
        report.ran = true;
        let partials = sweep_incoming(&dir)?;
        report.partials_removed = partials.removed;
        report.partials_left = partials.left;
        let tmp = provenance::sweep_tmp(&dir)?;
        report.provenance_tmp_removed = tmp.removed;
        report.provenance_tmp_left = tmp.left;
        crate::fault::point(crate::fault::FaultPoint::RecoverySwept);
        let reconciled = reconcile_intents(w, &dir, store.as_ref())?;
        crate::fault::point(crate::fault::FaultPoint::RecoveryReconciled);
        report.intents_committed = reconciled.committed;
        report.intents_rolled_back = reconciled.rolled_back;
        report.intent_files_left = reconciled.files_left;
        let interrupted = w
            .conn()
            .execute(
                "UPDATE import_session SET state = ?1, ended_at_ns = ?2 WHERE state = ?3",
                rusqlite::params![
                    SessionState::Interrupted.code(),
                    now,
                    SessionState::Running.code()
                ],
            )
            .map_err(|e| ArchiveError::sqlite(Stage::Recovery, &e))?;
        report.sessions_interrupted = u32::try_from(interrupted).unwrap_or(u32::MAX);
        crate::fault::point(crate::fault::FaultPoint::RecoveryMarked);
        self.recovered = true;
        tracing::info!(
            partials_removed = report.partials_removed,
            partials_left = report.partials_left,
            provenance_tmp_removed = report.provenance_tmp_removed,
            intents_rolled_back = report.intents_rolled_back,
            intent_files_left = report.intent_files_left,
            sessions_interrupted = report.sessions_interrupted,
            "archive opened for writing"
        );
        Ok(report)
    }

    /// Migrate to this build's schema (§7.2, D87). Explicit only: nothing
    /// else ever migrates.
    ///
    /// In order: the fingerprint of the current version; the space check
    /// (`3 × database + reserve`); unfinished sessions superseded; then each
    /// step in its own transaction. A failed step rolls back, the write
    /// connection is dropped, and the archive stays at the last committed
    /// version.
    pub fn migrate(&mut self, ctx: &mut ArchiveCtx<'_>) -> Result<MigrationReport, ArchiveError> {
        let from = self.schema_version();
        let target = self.plan.version;
        if from > target {
            return Err(ArchiveError::NewerSchema { found: from });
        }
        let mut report = MigrationReport {
            from,
            to: from,
            steps: 0,
            sessions_superseded: 0,
            space_checked: false,
        };
        if from == target {
            return Ok(report);
        }
        let chain = self
            .plan
            .chain(from, target)
            .ok_or(ArchiveError::Internal("no migration chain"))?;
        ctx.cancel.check().map_err(|_| ArchiveError::Cancelled)?;

        // 1. Fingerprint the current version on the write connection.
        let expected_now = fingerprint::expected(&self.plan, from)?;
        let w = self.ensure_writer()?;
        fingerprint::check(w.conn(), &expected_now)?;

        // 2. Space, before anything changes.
        let root = self.dir.root();
        let reserve = reserve_for(self.space.total(root).ok());
        let needed = migration_needs(self.dir.database_bytes(), reserve);
        match self.space.available(root) {
            Ok(avail) => {
                report.space_checked = true;
                if avail < needed {
                    return Err(ArchiveError::Location(
                        crate::error::LocationRefusal::InsufficientSpaceForMigration,
                    ));
                }
            }
            Err(e) => {
                tracing::debug!(kind = ?e.kind(), "free space unknown before a migration");
            }
        }

        // 3. Unfinished sessions cannot resume under different code.
        let now = ctx.clock.now_ns();
        let w = self.ensure_writer()?;
        let superseded = {
            let sql = |e: rusqlite::Error| ArchiveError::sqlite(Stage::Migrate, &e);
            let tx = rusqlite::Transaction::new(w.conn_mut(), TransactionBehavior::Immediate)
                .map_err(sql)?;
            // The same supersede as an import's (§8.3 step 3): state 5,
            // `full_observation = 0`, and the session-local rows deleted.
            let ids = crate::import::unfinished_sessions(&tx).map_err(sql)?;
            let n = crate::import::supersede_sessions(&tx, &ids, now).map_err(sql)?;
            tx.commit().map_err(sql)?;
            n
        };
        report.sessions_superseded = superseded;

        // 4. The steps.
        for step in chain {
            if ctx.cancel.is_cancelled() {
                return Err(ArchiveError::Cancelled);
            }
            ctx.progress.event(ArchiveEvent::MigrationStep {
                from: step.from,
                to: step.to,
            });
            let plan = self.plan;
            let w = self.ensure_writer()?;
            if let Err(e) = run_step(w, &plan, step, ctx.clock.now_ns(), APP_VERSION) {
                // Dropping the connection rolls back any open transaction
                // and resets `foreign_keys` for the next connection.
                self.writer = None;
                tracing::info!(
                    from = step.from,
                    to = step.to,
                    error = %e,
                    "migration step failed and was rolled back"
                );
                return Err(e);
            }
            self.reader.set_version(step.to);
            report.to = step.to;
            report.steps = report.steps.saturating_add(1);
            tracing::info!(from = step.from, to = step.to, "migration step applied");
        }
        Ok(report)
    }

    /// Close (§5.4): checkpoint with TRUNCATE if a writer was opened (best
    /// effort), drop the connections, then the lock. The lock file stays.
    /// Dropping an `Archive` does the same without the checkpoint.
    pub fn close(mut self) -> CloseReport {
        let checkpoint = self.writer.take().map(|w| {
            let outcome = w
                .conn()
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
                    r.get::<_, i64>(0)
                })
                .map_or_else(
                    |e| CheckpointOutcome::Failed(primary_code(&e)),
                    |busy| {
                        if busy == 0 {
                            CheckpointOutcome::Done
                        } else {
                            CheckpointOutcome::Busy
                        }
                    },
                );
            tracing::debug!(?outcome, "close checkpoint");
            drop(w);
            outcome
        });
        CloseReport { checkpoint }
    }
}
