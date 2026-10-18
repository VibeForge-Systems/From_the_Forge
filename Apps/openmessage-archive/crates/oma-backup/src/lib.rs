//! iPhone backup discovery, manifest parsing, keybag unlocking, Manifest.db
//! validation and single-file extraction for OpenMessage Archive.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod bplist;
mod cancel;
mod cbc_stream;
mod db_limits;
mod discover;
mod error;
mod extract;
mod guard;
mod id;
mod inspect;
mod kdf;
mod keybag;
mod keys;
mod limits;
mod manifest_db;
mod mbfile;
mod open;
mod plists;
mod prober;
mod schema_sql;
mod staged_conn;
mod staging;
mod text;
mod validate;
mod wal;

pub use cancel::{CancelToken, JobCtx, Phase, Progress, ProgressSink};
pub use db_limits::StagedDbLimits;
pub use discover::{
    classify_added_folder, default_backup_roots, library_relation, scan_root, AddedFolder,
    Candidate, DefaultRoot, LibraryRelation, RefusalReason, RootKind, RootScan, RootScanState,
    IMAZING_VERSIONS_DIR_NAME, STAGING_DIR_NAME,
};
pub use error::{
    BackupError, FileUnavailable, IoOp, IoSide, MalformedDetail, StagingRefusal, UnsupportedReason,
};
pub use extract::{
    AttachmentKey, ContentDigest, ExtractedFile, FileEntry, StagedDatabase, WalOutcome, WalPolicy,
};
pub use id::{BackupId, LocationId};
pub use inspect::{inspect, Classification, Inspection, Layout};
pub use keybag::KeybagNote;
pub use keys::Password;
pub use limits::{LimitKind, Limits, SMS_DB_BYTES_DEFAULT};
pub use manifest_db::{EntryKind, ManifestEntry, OpenMode};
pub use mbfile::MbFile;
pub use open::{OpenBackup, OpenError};
pub use plists::{
    BackupFile, InfoPlist, ManifestPlist, NoteKind, ParseNote, SnapshotState, Sourced, StatusPlist,
};
pub use prober::{BlobProbe, FileProber};
pub use schema_sql::sql_declares_virtual_table;
pub use staged_conn::{Budgeted, ReadPolicy, StagedConnection, TextEncoding, OPS_PER_TICK};
#[cfg(windows)]
pub use staging::FILE_ATTRIBUTE_TEMPORARY;
pub use staging::{sweep_stale, CleanupReport, StagingRoot, StagingSession, SweepReport};
pub use text::DisplayText;
pub use validate::{
    Check, CheckId, CheckLevel, Finding, ManifestStats, SmsDbStatus, ValidationReport,
    SMS_DB_DOMAIN, SMS_DB_FILE_ID, SMS_DB_PATH,
};
pub use wal::{HeaderFacts, WalFacts};
/// Re-exported so callers can build a [`Password`] without naming the crate.
pub use zeroize::Zeroizing;
