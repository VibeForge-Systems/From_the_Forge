//! The archive library of OpenMessage Archive (Milestone 4). It does not
//! depend on Tauri.
//!
//! A [`Library`] is the folder the user chose for archives. In it:
//!
//! - [`Library::create`] makes an archive: a private `a<32hex>` directory
//!   holding `archive.lock`, `archive.sqlite` (schema v1, WAL, FTS5),
//!   `attachments/` and `imports/`, built under `.oma-new-<32hex>` and
//!   renamed into place without replacing anything;
//! - [`Library::open`] takes the archive's lock and opens it **read-only
//!   first**: links, owner and mode, the volume, the lock and its
//!   self-test, the header, the schema fingerprint and the meta row are all
//!   checked before anything is written, and opening writes nothing;
//! - [`Library::open_read`] gives a lock-free [`ArchiveReader`];
//! - [`Library::list`] reports every archive without waiting on a lock;
//! - [`Library::sweep`] removes stale creation directories.
//!
//! [`Archive::begin_write`] is the first write: it opens the one ReadWrite
//! connection and cleans up after a crash. [`Archive::migrate`] is the only
//! way to a newer schema; nothing migrates implicitly. M4 ships the
//! migration framework with no real migration.
//!
//! [`Archive::import`] is the transactional, resumable import (slices 4f
//! to 4j): the preflight reads (the Recently Deleted and duplicate-key
//! scans, the attachment and space estimates, the resume plan), the
//! [`ImportGate`], the session, the merge engine, the handle, chat and
//! message passes, the link passes, the attachments with their files in
//! the content-addressed store, the Recently Deleted markers, the space
//! guards, finalize and the provenance file; see its module
//! documentation.
//!
//! [`Archive::verify`] is the report-only integrity verifier (slice 4k,
//! spec §11), quick or full, and [`ArchiveReader::open_blob`] the one way
//! attachment bytes are read back.
//!
//! Every connection is opened by one module, with the hardening of spec
//! §5.5 (`DEFENSIVE`, `TRUSTED_SCHEMA` off, no `ATTACH`, an op budget on
//! reads), through the `win32-longpath` VFS on Windows.
//!
//! Nothing here logs a path, an archive id, a label or a value from an
//! archive (invariant I6); errors carry codes and kinds only.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod archive;
mod blobs;
mod conn;
mod error;
mod fault;
mod id;
mod import;
mod layout;
mod library;
mod lock;
mod progress;
mod provenance;
mod reader;
pub mod schema;
pub mod space;
mod store_fs;
mod verify;

#[cfg(feature = "fault-injection")]
pub use fault::hooks;

pub use archive::{Archive, CheckpointOutcome, CloseReport, Recovery, RecoveryReport};
pub use blobs::BlobReader;
pub use error::{
    ArchiveError, BlobProblem, DamageKind, IoOp, LocationRefusal, PermProblem, RefusalKind, Stage,
};
pub use id::{ArchiveId, IdError, Label, LabelState, ARCHIVE_ID_LEN, LABEL_MAX_BYTES};
pub use import::{
    AttachmentFileCounts, BatchBudget, BodyCounts, DeviceEvidence, EntityCounts, FinalizeReport,
    FreshReason, GateDecision, ImportCounts, ImportGate, ImportOptions, ImportOutcome,
    ImportSummary, KeyCountFacts, KeyFacts, KeyScanTable, LineageChoice, LineageFacts, LineageId,
    LinkCounts, NotImportable, OrderFacts, OrderSource, Preflight, ProceedWith,
    ProceedWithSuggestion, RdFacts, RecentlyDeletedCounts, ResumePlan, StopReason, KEY_SCAN_CAP,
    LINEAGES_LIMIT, ORDER_KEY_FLOOR_NS, ORDER_KEY_SLACK_NS, RECOVERABLE_SCAN_CAP,
};
pub use library::{ArchiveEntry, Library, SweepReport, LIST_LIMIT, SWEEP_MIN_AGE};
pub use lock::{LOCK_RETRIES, LOCK_RETRY_WAIT};
pub use progress::{
    ArchiveCtx, ArchiveEvent, ArchiveSink, CancelToken, Clock, FixedClock, ImportPhase, NoProgress,
    SystemClock, VerifyPhase,
};
pub use reader::{
    ArchiveInfo, ArchiveReader, ArchiveStats, BlobId, ImportBlock, LastImport, ResumableSession,
    SessionCounts, SessionInfo, SourceFacts, SESSIONS_LIMIT,
};
pub use schema::codes::{BlobVerifyState, Pass, QualifierBit, SessionState, VerifyOutcome};
pub use schema::migrate::MigrationReport;
pub use schema::{APPLICATION_ID, DECODER_VERSION, DERIVER_VERSION, SCHEMA_VERSION};
pub use space::SpaceEstimate;
pub use store_fs::{DirSync, Placed, RealFs, StoreFs};
pub use verify::{
    CheckState, SchemaCheck, VerifyCounts, VerifyDepth, VerifyReport, VerifySummary,
    IMPORTS_SCAN_SLACK, SQLITE_CHECK_ROWS, STORE_WALK_SLACK,
};

/// The app version recorded in `archive_meta.created_by`,
/// `import_session.app_version` and `schema_migration.app_version`.
pub(crate) const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
