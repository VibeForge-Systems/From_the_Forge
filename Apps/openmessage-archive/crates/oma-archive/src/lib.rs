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
mod layout;
mod library;
mod lock;
mod progress;
mod provenance;
mod reader;
pub mod schema;
pub mod space;

#[cfg(feature = "fault-injection")]
pub use fault::hooks;

pub use archive::{Archive, CheckpointOutcome, CloseReport, Recovery, RecoveryReport};
pub use error::{ArchiveError, DamageKind, IoOp, LocationRefusal, PermProblem, RefusalKind, Stage};
pub use id::{ArchiveId, IdError, Label, LabelState, ARCHIVE_ID_LEN, LABEL_MAX_BYTES};
pub use library::{ArchiveEntry, Library, SweepReport, LIST_LIMIT, SWEEP_MIN_AGE};
pub use lock::{LOCK_RETRIES, LOCK_RETRY_WAIT};
pub use progress::{
    ArchiveCtx, ArchiveEvent, ArchiveSink, CancelToken, Clock, FixedClock, NoProgress, SystemClock,
};
pub use reader::{
    ArchiveInfo, ArchiveReader, ArchiveStats, ImportBlock, ResumableSession, SessionCounts,
    SessionInfo, SESSIONS_LIMIT,
};
pub use schema::migrate::MigrationReport;
pub use schema::{APPLICATION_ID, DECODER_VERSION, DERIVER_VERSION, SCHEMA_VERSION};

/// The app version recorded in `archive_meta.created_by`,
/// `import_session.app_version` and `schema_migration.app_version`.
pub(crate) const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
