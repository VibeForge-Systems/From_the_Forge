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
//! [`Archive::import_core`] is the transactional, resumable import as far
//! as it is built (slices 4f to 4i: the session, the Recently Deleted and
//! duplicate-key scans, the attachment and space estimates, the merge
//! engine, the handle, chat and message passes, the link passes, the
//! attachments with their files in the content-addressed store, the
//! Recently Deleted markers, and the space guards; see its module
//! documentation).
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

#[cfg(feature = "fault-injection")]
pub use fault::hooks;

pub use archive::{Archive, CheckpointOutcome, CloseReport, Recovery, RecoveryReport};
pub use error::{ArchiveError, DamageKind, IoOp, LocationRefusal, PermProblem, RefusalKind, Stage};
pub use id::{ArchiveId, IdError, Label, LabelState, ARCHIVE_ID_LEN, LABEL_MAX_BYTES};
pub use import::{
    AttachmentFileCounts, BatchBudget, BodyCounts, CoreOutcome, EntityCounts, ImportCounts,
    ImportOptions, ImportSummary, KeyScanTable, LineageChoice, LineageId, LinkCounts,
    NotImportable, RecentlyDeletedCounts, StopReason, KEY_SCAN_CAP, ORDER_KEY_FLOOR_NS,
    ORDER_KEY_SLACK_NS, RECOVERABLE_SCAN_CAP,
};
pub use library::{ArchiveEntry, Library, SweepReport, LIST_LIMIT, SWEEP_MIN_AGE};
pub use lock::{LOCK_RETRIES, LOCK_RETRY_WAIT};
pub use progress::{
    ArchiveCtx, ArchiveEvent, ArchiveSink, CancelToken, Clock, FixedClock, ImportPhase, NoProgress,
    SystemClock,
};
pub use reader::{
    ArchiveInfo, ArchiveReader, ArchiveStats, ImportBlock, ResumableSession, SessionCounts,
    SessionInfo, SESSIONS_LIMIT,
};
pub use schema::migrate::MigrationReport;
pub use schema::{APPLICATION_ID, DECODER_VERSION, DERIVER_VERSION, SCHEMA_VERSION};
pub use space::SpaceEstimate;
pub use store_fs::{DirSync, Placed, RealFs, StoreFs};

/// The app version recorded in `archive_meta.created_by`,
/// `import_session.app_version` and `schema_migration.app_version`.
pub(crate) const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
