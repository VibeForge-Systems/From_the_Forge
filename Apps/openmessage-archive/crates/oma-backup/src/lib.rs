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
mod staging;
mod text;
mod validate;

pub use cancel::{CancelToken, JobCtx, Phase, Progress, ProgressSink};
pub use discover::{
    classify_added_folder, default_backup_roots, library_relation, scan_root, AddedFolder,
    Candidate, DefaultRoot, LibraryRelation, RefusalReason, RootKind, RootScan, RootScanState,
    IMAZING_VERSIONS_DIR_NAME, STAGING_DIR_NAME,
};
pub use error::{
    BackupError, FileUnavailable, IoOp, IoSide, MalformedDetail, StagingRefusal, UnsupportedReason,
};
pub use extract::{ExtractedFile, StagedConnection, StagedDatabase};
pub use id::{BackupId, LocationId};
pub use inspect::{inspect, Classification, Inspection, Layout};
pub use keybag::KeybagNote;
pub use keys::Password;
pub use limits::{LimitKind, Limits};
pub use manifest_db::{EntryKind, ManifestEntry, OpenMode};
pub use mbfile::MbFile;
pub use open::{OpenBackup, OpenError};
pub use plists::{
    BackupFile, InfoPlist, ManifestPlist, NoteKind, ParseNote, SnapshotState, Sourced, StatusPlist,
};
pub use staging::{sweep_stale, CleanupReport, StagingRoot, StagingSession, SweepReport};
pub use text::DisplayText;
pub use validate::{
    Check, CheckId, CheckLevel, Finding, ManifestStats, SmsDbStatus, ValidationReport,
    SMS_DB_DOMAIN, SMS_DB_FILE_ID, SMS_DB_PATH,
};
/// Re-exported so callers can build a [`Password`] without naming the crate.
pub use zeroize::Zeroizing;
