//! Every user-facing sentence of the Backups page, and the mapping from
//! `oma_backup` errors to IPC errors (§11.7, §14).
//!
//! The crate's errors carry kinds, never content; the shell writes the
//! sentences. A message may name a folder the user chose, but never a device
//! name, a relativePath, a SnapshotState value or SQLite message text. The
//! raw Status.plist value appears only in `stateReason` and `StatusDetails`,
//! as sanitised `DisplayText`.

use std::io;

use oma_backup::{
    BackupError, BackupFile, Check, CheckId, Classification, FileUnavailable, Finding, IoSide,
    KeybagNote, LimitKind, Limits, MalformedDetail, NoteKind, ParseNote, RefusalReason,
    SnapshotState, StagingRefusal, UnsupportedReason,
};

use oma_messages::{BackupStage, MessagesError};

use super::dto::{BackupState, ValidationCheck};
use crate::error::{ErrorCode, IpcError};
use crate::settings::CheckLevel;

/// "Choose an archive folder first…" (§11.3 step 4).
pub const NOT_CONFIGURED: &str =
    "Choose an archive folder first. Checking a backup copies its file index there temporarily.";
pub const NOT_INSPECTED: &str =
    "Not inspected yet, because there are many backups here. Select it to inspect it.";
pub const LIBRARY_INSIDE_BACKUP: &str =
    "Your archive folder is inside this backup. Move the archive folder elsewhere before checking this backup.";
pub const BACKUP_NOT_FOUND: &str = "That backup is no longer available. Refresh the list.";
pub const BUSY_SAME: &str = "A check is already running for this backup.";
pub const BUSY_OTHER: &str = "Another backup is being checked. Wait for it to finish or cancel it.";
pub const BUSY_LISTING: &str = "The list of backups is already being refreshed.";
pub const BUSY_LOCATION: &str = "Wait for the current backup check to finish or cancel it.";
pub const LOCATION_CHANGED: &str =
    "The archive location changed while the check was starting. Check the backup again.";
pub const BUSY_FORGET: &str =
    "A backup in this folder is being checked. Wait for the check to finish or cancel it.";
pub const PASSWORD_REQUIRED: &str = "This backup is encrypted. Enter its password to check it.";
pub const PASSWORD_TOO_LONG: &str = "Passwords longer than 1,024 bytes are not accepted.";
pub const WRONG_PASSWORD: &str = "That password did not unlock the backup. Check it and try again. If you are sure it is right, the backup's key data may be damaged.";
pub const DISK_FULL: &str =
    "There is not enough free space in your archive folder to check this backup.";
pub const DISK_FULL_ARCHIVE: &str =
    "There is not enough free space in your archive folder to copy this attachment.";
pub const BACKUP_IN_USE: &str = "The backup is in use by another program, such as Apple Devices or iTunes. Wait for it to finish, then check again.";
pub const STAGING_NOT_SAFE: &str = "The temporary folder inside your archive folder is not safe to use (it is a link, or belongs to another user). Remove `.oma-staging` from your archive folder, then try again.";
pub const STAGING_NOT_PRIVATE: &str = "The temporary folder inside your archive folder can be read by other users of this computer. If the archive folder is on a drive that does not keep Linux file permissions (for example a FAT, exFAT or NTFS drive, or a network share), choose an archive folder on another drive. Otherwise remove `.oma-staging` from your archive folder, then try again.";
pub const LOCK_UNSUPPORTED: &str = "Your archive folder is on a drive that does not support the file locking this check needs. Choose an archive folder on a local drive.";
/// What to do about a backup that did not finish. The iOS 16 passcode
/// confirmation is Apple's (support.apple.com/en-us/109054); see
/// `docs/supported-formats.md`.
pub const RUN_BACKUP_AGAIN: &str = "Run the backup again with the app or tool that made it, and keep the iPhone unlocked until it completes. On iOS 16 or later, enter the iPhone's passcode when the iPhone asks you to confirm the backup.";
pub const SOURCE_CHANGED: &str = "The backup changed while a file was being copied from it. Run the check again when no backup is in progress.";
pub const TOO_MANY_FOLDERS: &str = "You can add up to 32 backup folders. Forget one first.";

/// `1234567` → `1,234,567`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn mib(bytes: u64) -> String {
    if bytes >= 1 << 30 && bytes.is_multiple_of(1 << 30) {
        format!("{} GiB", thousands(bytes >> 30))
    } else if bytes >= 1 << 20 && bytes.is_multiple_of(1 << 20) {
        format!("{} MiB", thousands(bytes >> 20))
    } else if bytes >= 1 << 10 && bytes.is_multiple_of(1 << 10) {
        format!("{} KiB", thousands(bytes >> 10))
    } else {
        format!("{} bytes", thousands(bytes))
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

pub fn file_name(f: BackupFile) -> &'static str {
    match f {
        BackupFile::InfoPlist => "Info.plist",
        BackupFile::StatusPlist => "Status.plist",
        BackupFile::ManifestPlist => "Manifest.plist",
        BackupFile::ManifestDb => "Manifest.db",
        BackupFile::ManifestDbWal => "Manifest.db-wal",
        BackupFile::ManifestMbdb => "Manifest.mbdb",
        BackupFile::Keybag => "The key bag in Manifest.plist",
        BackupFile::ManifestKey => "The ManifestKey in Manifest.plist",
        BackupFile::Blob => "A file record",
    }
}

fn cap_for(file: BackupFile, limits: &Limits) -> Option<u64> {
    match file {
        BackupFile::InfoPlist => Some(limits.info_plist_bytes),
        BackupFile::StatusPlist => Some(limits.status_plist_bytes),
        BackupFile::ManifestPlist => Some(limits.manifest_plist_bytes),
        BackupFile::ManifestDb | BackupFile::ManifestDbWal => Some(limits.manifest_db_bytes),
        BackupFile::Keybag => Some(limits.keybag_bytes),
        BackupFile::Blob => Some(limits.mbfile_bytes),
        BackupFile::ManifestMbdb | BackupFile::ManifestKey => None,
    }
}

/// The problem phrase of "{File} could not be read: {problem}." (§14).
pub fn problem(file: BackupFile, detail: MalformedDetail, limits: &Limits) -> String {
    match detail {
        MalformedDetail::Missing => "it is missing".into(),
        MalformedDetail::NotRegularFile => "it is not a regular file".into(),
        MalformedDetail::TooLarge => match cap_for(file, limits) {
            Some(cap) => format!("it is larger than {}", mib(cap)),
            None => "it is too large".into(),
        },
        MalformedDetail::PlistSyntax => "it is not a valid property list".into(),
        MalformedDetail::BudgetExceeded => "its structure exceeds safe processing limits".into(),
        MalformedDetail::UnexpectedType(key) => format!("{key} has an unexpected type"),
        MalformedDetail::NotSqlite => "it is not an SQLite database".into(),
        MalformedDetail::Inconsistent("not encrypted but not SQLite") => {
            "it is not an SQLite database, although Manifest.plist says the backup is not encrypted"
                .into()
        }
        MalformedDetail::Inconsistent("encrypted but plaintext") => {
            "Manifest.plist says the backup is encrypted, but this file is not encrypted".into()
        }
        MalformedDetail::Inconsistent("manifest key does not unwrap") => {
            "its key does not unlock with the backup's key bag".into()
        }
        MalformedDetail::Inconsistent("file key does not unwrap") => {
            "a file's key does not unlock with the backup's key bag".into()
        }
        MalformedDetail::Inconsistent(_) => "its contents contradict each other".into(),
        MalformedDetail::ReadError(kind) => format!("it could not be read ({kind})"),
        MalformedDetail::Truncated => "it ends early".into(),
        MalformedDetail::BadLength { .. } => "a value has the wrong length".into(),
        MalformedDetail::DuplicateClass(_) => "it lists the same key class twice".into(),
        MalformedDetail::Schema(what) => {
            format!("it does not have the expected structure ({what})")
        }
        MalformedDetail::Padding => "its encryption padding is invalid".into(),
        MalformedDetail::NotBlockAligned => {
            "its length is not a whole number of encryption blocks".into()
        }
        MalformedDetail::Empty => "it is empty".into(),
        MalformedDetail::UidOutOfRange | MalformedDetail::NestingTooDeep => {
            "its structure is invalid".into()
        }
    }
}

pub fn unsupported_reason(r: UnsupportedReason) -> String {
    match r {
        UnsupportedReason::LegacyMbdb => "This backup uses format 2.4 (Manifest.mbdb). OpenMessage Archive reads backups that have a Manifest.db.".into(),
        UnsupportedReason::NoKeybag => "This backup is encrypted, but its Manifest.plist has no key bag, so it cannot be unlocked.".into(),
        UnsupportedReason::EncryptedWithoutManifestKey => "This backup is encrypted, but its Manifest.plist has no ManifestKey. This encryption layout is not supported.".into(),
        UnsupportedReason::EncryptionUnknown => "Manifest.plist does not say whether this backup is encrypted, so it cannot be read safely.".into(),
        UnsupportedReason::SingleStageKdf => "This backup's key bag uses a single-stage key derivation, which is not supported.".into(),
        UnsupportedReason::NoPassphraseClasses => "This backup's key bag has no keys protected by the backup password, so it cannot be unlocked with one.".into(),
        UnsupportedReason::KeyType(k) => format!("This backup uses a key type ({k}) that is not supported."),
    }
}

/// `state` and `stateReason` for a classification (§14).
pub fn state_of(
    c: &Classification,
    status_plist_present: bool,
    snapshot_raw: Option<&str>,
    limits: &Limits,
) -> (BackupState, String) {
    match c {
        Classification::Ready => (BackupState::Ready, "Ready to check.".into()),
        Classification::NeedsPassword => (BackupState::NeedsPassword, PASSWORD_REQUIRED.into()),
        Classification::Incomplete {
            snapshot,
            snapshot_dir,
        } => {
            let what = match (snapshot, snapshot_raw) {
                _ if !status_plist_present => "no Status.plist was found".to_owned(),
                (SnapshotState::Absent, _) => "Status.plist has no SnapshotState".to_owned(),
                (SnapshotState::WrongType, _) => {
                    "Status.plist's SnapshotState has an unexpected type".to_owned()
                }
                (_, Some(raw)) => format!("Status.plist reports \u{201c}{raw}\u{201d}"),
                (_, None) => "Status.plist has no SnapshotState".to_owned(),
            };
            let mut reason = format!("This backup did not finish ({what}).");
            if *snapshot_dir {
                reason.push_str(" A Snapshot folder is present, so the backup may still be in progress or may have been interrupted.");
            }
            reason.push(' ');
            reason.push_str(RUN_BACKUP_AGAIN);
            reason.push_str(" Then check again.");
            (BackupState::Incomplete, reason)
        }
        Classification::Unsupported(r) => (BackupState::Unsupported, unsupported_reason(*r)),
        Classification::Unreadable { file, detail } => (
            BackupState::Unreadable,
            format!(
                "{} could not be read: {}.",
                file_name(*file),
                problem(*file, *detail, limits)
            ),
        ),
    }
}

pub fn refusal(r: RefusalReason) -> &'static str {
    match r {
        RefusalReason::NonUnicodePath => "The folder path contains characters that are not valid Unicode. Rename the folder or choose another.",
        RefusalReason::NotFound => "The folder could not be found.",
        RefusalReason::NotADirectory => "The selected item is not a folder.",
        RefusalReason::NoBackupsFound => "No iPhone backup was found in this folder. A backup folder contains Info.plist, Manifest.plist, Status.plist and Manifest.db. Choose that folder, or the folder that contains it.",
        RefusalReason::InsideStaging => "This is one of OpenMessage Archive's temporary folders.",
        RefusalReason::BackupContainsArchiveLibrary => "Your archive folder is inside this backup. Backups must never be written to, so move the archive folder elsewhere first.",
    }
}

fn limit_message(what: LimitKind, value: u64, cap: u64) -> String {
    let (v, c) = (thousands(value), thousands(cap));
    match what {
        LimitKind::Dpic | LimitKind::Iter => format!(
            "The backup's key data asks for {v} key-derivation rounds; the limit is {c}. The backup may be damaged."
        ),
        LimitKind::KeybagBytes => format!(
            "The backup's key bag is larger than {}. The backup may be damaged.",
            mib(cap)
        ),
        LimitKind::KeybagClasses => format!(
            "The backup's key bag lists more than {c} key classes. The backup may be damaged."
        ),
        LimitKind::ManifestDbBytes => format!(
            "The backup's file index (Manifest.db) is larger than {}, the current limit.",
            mib(cap)
        ),
        LimitKind::ManifestRows => format!(
            "The backup's file index lists more than {c} files, the current limit."
        ),
        LimitKind::MbFileBytes | LimitKind::MbFileObjects => {
            "A file record in the backup's file index exceeds safe processing limits. The backup may be damaged.".into()
        }
        LimitKind::PlistBytes | LimitKind::PlistEvents | LimitKind::PlistDepth => {
            "A property list in the backup exceeds safe processing limits. The backup may be damaged.".into()
        }
        LimitKind::RootEntries => format!("The folder has more than {c} entries."),
        LimitKind::PasswordBytes => PASSWORD_TOO_LONG.into(),
        LimitKind::SmsDbBytes => format!(
            "The backup's Messages database is larger than {}, the current limit.",
            mib(cap)
        ),
        LimitKind::SummaryArray => {
            "A value in the backup's Messages database exceeds safe processing limits.".into()
        }
        LimitKind::AttachmentBytes => format!(
            "An attachment in the backup is larger than {}, the current limit.",
            mib(cap)
        ),
    }
}

fn file_unavailable(f: FileUnavailable) -> &'static str {
    match f {
        FileUnavailable::BlobMissing => "a file the check needs is missing from the backup",
        FileUnavailable::BlobNotRegular => "a file the check needs is not a plain file",
        FileUnavailable::NoKey => "a file the check needs has no encryption key",
        FileUnavailable::NoProtectionClass => "a file the check needs has no protection class",
        FileUnavailable::ClassKeyUnavailable => "a key class the check needs is not available",
        FileUnavailable::Ambiguous => "a file the check needs is listed more than once",
        FileUnavailable::NotAFile => "an entry the check needs is not a file",
        FileUnavailable::NotListed => "a file the check needs is not listed",
        FileUnavailable::InvalidEntry => "an entry the check needs is invalid",
    }
}

fn io_message(side: IoSide, kind: io::ErrorKind, os_code: Option<i32>) -> String {
    // Windows ERROR_SHARING_VIOLATION (32) and ERROR_LOCK_VIOLATION (33). On
    // other systems those numbers mean something else.
    let sharing = cfg!(windows) && matches!(os_code, Some(32 | 33));
    match side {
        IoSide::Staging if kind == io::ErrorKind::StorageFull => DISK_FULL.into(),
        IoSide::Staging => format!(
            "A temporary file in your archive folder could not be written or removed ({kind})."
        ),
        IoSide::Source if sharing => BACKUP_IN_USE.into(),
        IoSide::Source => format!("The backup could not be read ({kind})."),
        // The archive's attachment copy (M4). Its import job words its own
        // outcomes; this is the fallback sentence.
        IoSide::Sink if kind == io::ErrorKind::StorageFull => DISK_FULL_ARCHIVE.into(),
        IoSide::Sink => {
            format!("An attachment could not be written to your archive folder ({kind}).")
        }
    }
}

/// `BackupError` → `IpcError` (§11.7). `Cancelled` is never an `IpcError`;
/// callers turn it into `JobEvent::Cancelled` first.
pub fn to_ipc(e: &BackupError, limits: &Limits) -> IpcError {
    match e {
        BackupError::NotABackup(r) => IpcError::new(ErrorCode::NotABackup, refusal(*r)),
        BackupError::Unsupported(r) => {
            IpcError::new(ErrorCode::UnsupportedBackup, unsupported_reason(*r))
        }
        BackupError::Incomplete { .. } => IpcError::new(
            ErrorCode::IncompleteBackup,
            format!("This backup did not finish. {RUN_BACKUP_AGAIN} Then try again."),
        ),
        BackupError::Malformed { file, detail } => IpcError::new(
            ErrorCode::MalformedBackup,
            format!(
                "{} could not be read: {}. The backup may be damaged.",
                file_name(*file),
                problem(*file, *detail, limits)
            ),
        ),
        BackupError::LimitExceeded {
            what: LimitKind::PasswordBytes,
            ..
        } => IpcError::new(ErrorCode::PasswordTooLong, PASSWORD_TOO_LONG),
        BackupError::LimitExceeded { what, value, cap } => IpcError::new(
            ErrorCode::MalformedBackup,
            limit_message(*what, *value, *cap),
        ),
        BackupError::PasswordRequired => {
            IpcError::new(ErrorCode::PasswordRequired, PASSWORD_REQUIRED)
        }
        BackupError::WrongPassword => IpcError::new(ErrorCode::WrongPassword, WRONG_PASSWORD),
        BackupError::KeybagDamaged => IpcError::new(
            ErrorCode::MalformedBackup,
            "The password unlocked part of the backup's key data, but not all of it. The backup's key data may be damaged.",
        ),
        BackupError::FileUnavailable(f) => IpcError::new(
            ErrorCode::MalformedBackup,
            format!(
                "The backup could not be checked: {}. The backup may be damaged.",
                file_unavailable(*f)
            ),
        ),
        BackupError::Sqlite { code } => IpcError::new(
            ErrorCode::MalformedBackup,
            match code {
                Some(c) => format!(
                    "The backup's file index could not be read (SQLite error {c}). The backup may be damaged."
                ),
                None => "The backup's file index could not be read. The backup may be damaged."
                    .to_owned(),
            },
        ),
        BackupError::Io {
            side,
            kind,
            os_code,
            ..
        } => IpcError::new(ErrorCode::Io, io_message(*side, *kind, *os_code)),
        BackupError::SourceChanged => IpcError::new(ErrorCode::Io, SOURCE_CHANGED),
        BackupError::SchemaExceedsLimits => IpcError::new(
            ErrorCode::UnsupportedBackup,
            "A database in this backup has a structure larger than this app's safety limits allow.",
        ),
        BackupError::StagingRefused(StagingRefusal::Guard) => {
            IpcError::internal("staging guard refused")
        }
        BackupError::StagingRefused(StagingRefusal::LockUnsupported) => {
            IpcError::new(ErrorCode::Io, LOCK_UNSUPPORTED)
        }
        BackupError::StagingRefused(StagingRefusal::LoosePermissions) => {
            IpcError::new(ErrorCode::Io, STAGING_NOT_PRIVATE)
        }
        BackupError::StagingRefused(_) => IpcError::new(ErrorCode::Io, STAGING_NOT_SAFE),
        BackupError::Internal(what) => IpcError::internal(what),
        BackupError::Cancelled => IpcError::internal("cancelled job reported as an error"),
    }
}

/// "The backup changed…" for the Messages database copy (M3 §16.5).
pub const SMS_SOURCE_CHANGED: &str = "The backup changed while the Messages database was being copied. Run the check again when no backup is in progress.";

/// What went wrong opening the staged copy, without "file index" (§16.5).
fn opening_detail(e: &BackupError) -> String {
    match e {
        BackupError::Sqlite { code: Some(c) } => format!("SQLite error {c}"),
        BackupError::Sqlite { code: None } => "SQLite reported an error".into(),
        BackupError::Io { kind, .. } => format!("a file could not be read: {kind}"),
        BackupError::LimitExceeded { .. } | BackupError::SchemaExceedsLimits => {
            "it exceeds this app's safety limits".into()
        }
        _ => "an unexpected error occurred".into(),
    }
}

/// `MessagesError` → `IpcError`, by stage (M3 §16.5). No new `ErrorCode`
/// (D41). `Cancelled` is never an `IpcError`; callers turn it into
/// `JobEvent::Cancelled` first.
pub fn messages_to_ipc(e: &MessagesError, limits: &Limits) -> IpcError {
    match e {
        MessagesError::Sqlite {
            code: Some(c @ (11 | 26)),
            ..
        } => IpcError::new(
            ErrorCode::MalformedBackup,
            format!("The Messages database in this backup is damaged and could not be read (SQLite error {c})."),
        ),
        MessagesError::Sqlite { code: Some(c), .. } => IpcError::new(
            ErrorCode::MalformedBackup,
            format!("The Messages database in this backup could not be read (SQLite error {c})."),
        ),
        MessagesError::Sqlite { code: None, .. } => IpcError::new(
            ErrorCode::MalformedBackup,
            "The Messages database in this backup could not be read.",
        ),
        MessagesError::Backup { source, stage } => match (stage, source) {
            (BackupStage::Staging, BackupError::FileUnavailable(f)) => IpcError::new(
                ErrorCode::MalformedBackup,
                format!(
                    "The Messages database could not be copied from the backup: {}.",
                    file_unavailable(*f)
                ),
            ),
            (BackupStage::Staging | BackupStage::Opening, BackupError::SourceChanged) => {
                IpcError::new(ErrorCode::Io, SMS_SOURCE_CHANGED)
            }
            (_, BackupError::Cancelled) => IpcError::internal("cancelled job reported as an error"),
            (BackupStage::Opening, e) => IpcError::new(
                ErrorCode::MalformedBackup,
                format!(
                    "The Messages database copy could not be opened ({}).",
                    opening_detail(e)
                ),
            ),
            (BackupStage::Removing, _) => {
                IpcError::internal("removing the staged copy reported as an error")
            }
            // Staging (other errors), ManifestLookup and AttachmentProbe:
            // the backup's own sentences, where "file index" is right.
            (_, e) => to_ipc(e, limits),
        },
        MessagesError::Cancelled => IpcError::internal("cancelled job reported as an error"),
        MessagesError::Internal(what) => IpcError::internal(what),
        _ => IpcError::internal("unknown messages error"),
    }
}

/// The error a classification that is not importable maps to, before a job
/// starts (§11.3 step 2).
pub fn classification_error(c: &Classification, limits: &Limits) -> Option<IpcError> {
    match c {
        Classification::Ready | Classification::NeedsPassword => None,
        Classification::Incomplete { snapshot, .. } => Some(to_ipc(
            &BackupError::Incomplete {
                snapshot: *snapshot,
            },
            limits,
        )),
        Classification::Unsupported(r) => Some(to_ipc(&BackupError::Unsupported(*r), limits)),
        Classification::Unreadable { file, detail } => Some(to_ipc(
            &BackupError::Malformed {
                file: *file,
                detail: *detail,
            },
            limits,
        )),
    }
}

fn level(l: oma_backup::CheckLevel) -> CheckLevel {
    match l {
        oma_backup::CheckLevel::Ok => CheckLevel::Ok,
        oma_backup::CheckLevel::Warning => CheckLevel::Warning,
        oma_backup::CheckLevel::Error => CheckLevel::Error,
    }
}

fn finding_sentence(f: &Finding) -> Option<String> {
    Some(match f {
        Finding::NotEncrypted => "The backup is not encrypted.".into(),
        Finding::KeybagNotes(n) => format!(
            "The backup was unlocked. Its key data has {} that do{} not affect decryption.",
            plural(u64::from(*n), "unusual feature", "unusual features"),
            if *n == 1 { "es" } else { "" }
        ),
        Finding::WalApplied => {
            "The file index's pending changes (Manifest.db-wal) were applied to the copy.".into()
        }
        Finding::WalNotApplied => "The backup's file index has changes that were not applied; the index may be slightly out of date.".into(),
        Finding::QuickCheckFailed => {
            "The file index database is damaged: SQLite's integrity check failed.".into()
        }
        Finding::SchemaProblem(what) => {
            format!("The file index does not have the expected structure ({what}).")
        }
        Finding::NoRows => "The file index lists no files.".into(),
        Finding::PropertiesAbsent => "The file index has no Properties table.".into(),
        Finding::Views(n) => format!(
            "The file index defines {} (never executed).",
            plural(u64::from(*n), "view", "views")
        ),
        Finding::Triggers(n) => format!(
            "The file index defines {} (never run).",
            plural(u64::from(*n), "trigger", "triggers")
        ),
        Finding::UnreadableRecords(n) => format!(
            "{} could not be read.",
            plural(*n, "file record", "file records")
        ),
        Finding::InvalidRows(n) => format!(
            "{} in the file index {} invalid.",
            plural(*n, "row", "rows"),
            if *n == 1 { "is" } else { "are" }
        ),
        Finding::InvalidFileIds(n) => format!(
            "{} {} not valid.",
            plural(*n, "file identifier", "file identifiers"),
            if *n == 1 { "is" } else { "are" }
        ),
        Finding::FileIdMismatches(n) => format!(
            "{} {} not match the file's name.",
            plural(*n, "file identifier", "file identifiers"),
            if *n == 1 { "does" } else { "do" }
        ),
        Finding::MissingBlobs(n) => format!(
            "{} {} missing from the backup.",
            plural(*n, "file", "files"),
            if *n == 1 { "is" } else { "are" }
        ),
        Finding::UnsafeBlobs(n) => format!(
            "{} {} skipped because {} not plain files.",
            plural(*n, "file", "files"),
            if *n == 1 { "was" } else { "were" },
            if *n == 1 { "it is" } else { "they are" }
        ),
        Finding::UnreadableBlobs(n) => format!(
            "{} could not be examined (for example, permission was denied).",
            plural(*n, "file", "files"),
        ),
        Finding::SizeMismatches(n) => format!(
            "{} from the actual size. Recorded sizes often differ from actual sizes; this is expected.",
            plural(*n, "recorded size differs", "recorded sizes differ")
        ),
        Finding::DeclaredBytesSaturated => {
            "The total recorded size is too large to count exactly.".into()
        }
        Finding::PathsUnusualForExport(n) => format!(
            "{} will need adjusting when exported.",
            plural(*n, "name", "names")
        ),
        Finding::FilesWithoutKey(n) => format!(
            "{} no encryption key.",
            plural(*n, "file has", "files have")
        ),
        Finding::FilesWithUnavailableClass(n) => format!(
            "{} cannot be decrypted because {} key class is not available.",
            plural(*n, "file", "files"),
            if *n == 1 { "its" } else { "their" }
        ),
        Finding::SmsClassUnavailable => {
            "The key class the Messages database needs is not available.".into()
        }
        Finding::SmsNotListed => "The Messages database (sms.db) is not listed in the file index.".into(),
        Finding::SmsAmbiguous => {
            "The Messages database (sms.db) is listed more than once in the file index.".into()
        }
        Finding::SmsInvalidEntry => {
            "The file index entry for the Messages database (sms.db) is invalid.".into()
        }
        Finding::SmsBlobMissing => "The Messages database (sms.db) is missing from the backup.".into(),
        Finding::SmsBlobNotRegular => {
            "The Messages database (sms.db) is not a plain file, so it was not read.".into()
        }
        Finding::SmsNoKey => "The Messages database (sms.db) has no usable encryption key.".into(),
        Finding::SmsKeyDoesNotUnwrap => {
            "The Messages database's encryption key does not unlock.".into()
        }
        Finding::SmsHeaderNotSqlite => {
            "The Messages database (sms.db) does not start like an SQLite database.".into()
        }
        Finding::SmsBadPadding => {
            "The Messages database (sms.db) ends with invalid encryption padding.".into()
        }
        Finding::SmsSizeDiffers => {
            "The Messages database's size differs from its recorded size.".into()
        }
        Finding::BackupChanged => "The backup changed while it was being checked. Wait for Apple Devices or iTunes to finish, then check again.".into(),
    })
}

fn passed_sentence(id: CheckId, encrypted: bool) -> &'static str {
    match id {
        CheckId::Status => "The backup finished.",
        CheckId::Format => "The backup uses format 3 (Manifest.db).",
        CheckId::Encryption => "The backup was unlocked.",
        CheckId::ManifestCopy => {
            if encrypted {
                "The file index (Manifest.db) was decrypted into the archive folder for checking."
            } else {
                "The file index (Manifest.db) was copied into the archive folder for checking."
            }
        }
        CheckId::ManifestIntegrity => "The file index database passed SQLite's integrity check.",
        CheckId::ManifestSchema => "The file index has the expected structure.",
        CheckId::FileRecords => "Every file record could be read.",
        CheckId::FileIds => "Every file identifier is valid and matches its file.",
        CheckId::Blobs => "Every listed file is present.",
        CheckId::BlobSizes => "Every recorded size matches.",
        CheckId::Paths => "No names will need adjusting when exported.",
        CheckId::KeyClasses => "Every file has the keys it needs.",
        CheckId::MessagesDb => "The Messages database (sms.db) is listed, present and starts like an SQLite database. It was checked where it is and was not copied.",
        CheckId::Unchanged => "The backup did not change during the check.",
    }
}

/// One report check as a `ValidationCheck`: the pass sentence when there is
/// nothing to say, otherwise the findings' sentences.
pub fn check(c: &Check, encrypted: bool) -> ValidationCheck {
    let mut sentences: Vec<String> = Vec::new();
    if matches!(c.id, CheckId::ManifestCopy | CheckId::MessagesDb)
        && c.level == oma_backup::CheckLevel::Ok
    {
        sentences.push(passed_sentence(c.id, encrypted).to_owned());
    }
    sentences.extend(c.findings.iter().filter_map(|f| match (c.id, f) {
        (CheckId::KeyClasses, Finding::NotEncrypted) => {
            Some("No encryption keys are needed: the backup is not encrypted.".to_owned())
        }
        _ => finding_sentence(f),
    }));
    if sentences.is_empty() {
        sentences.push(passed_sentence(c.id, encrypted).to_owned());
    }
    ValidationCheck {
        id: c.id.as_str().to_owned(),
        level: level(c.level),
        message: sentences.join(" "),
    }
}

fn keybag_note(n: &KeybagNote) -> &'static str {
    match n {
        KeybagNote::TrailingBytes(_) => "it has a few extra bytes at the end",
        KeybagNote::UnknownTags(_) => "it has fields this version does not know",
        KeybagNote::DuplicateTag(_) => "a field appears twice",
        KeybagNote::UnexpectedType(_) => "its type is not the one seen in local backups",
        KeybagNote::UnusualClass(_) => "it lists an unusual key class",
        KeybagNote::UnusualLength(_) => "a value has an unusual length",
        KeybagNote::UnusableClassRecord => "a key class record is unusable",
        KeybagNote::BadWrappedKeyLength(_) => "a key class has a wrapped key of unusual length",
        KeybagNote::MissingKeyType(_) => "a key class does not state its key type",
        KeybagNote::MisplacedHeaderTag(_) => "a header field appears after the key classes",
    }
}

/// Parse, layout and location notes, as checks for the details panel.
pub fn note(n: &ParseNote, limits: &Limits) -> ValidationCheck {
    let file = file_name(n.file);
    let (id, lvl, message) = match n.kind {
        NoteKind::UnexpectedType => (
            "unexpected-type",
            CheckLevel::Warning,
            format!("{file}: {} has an unexpected type and was ignored.", n.key),
        ),
        NoteKind::Truncated => (
            "truncated",
            CheckLevel::Warning,
            format!("{file} ends early."),
        ),
        NoteKind::BudgetExceeded => (
            "budget",
            CheckLevel::Warning,
            format!("{file} exceeds safe processing limits, so its details are not shown."),
        ),
        NoteKind::SnapshotDirPresent => (
            "snapshot-folder",
            CheckLevel::Warning,
            "A Snapshot folder is present. Its meaning is not confirmed; the backup may have been interrupted.".to_owned(),
        ),
        NoteKind::WalUnapplied => (
            "wal-unapplied",
            CheckLevel::Warning,
            "The backup's file index has changes that were not applied.".to_owned(),
        ),
        NoteKind::KeybagNote(k) => (
            "keybag",
            CheckLevel::Ok,
            format!("The backup's key bag is unusual: {}.", keybag_note(&k)),
        ),
        NoteKind::InsideArchiveLibrary => (
            "inside-library",
            CheckLevel::Warning,
            "This backup is inside your archive folder. Keeping backups and archives in separate folders is safer.".to_owned(),
        ),
        NoteKind::InSyncFolder(provider) => (
            "sync-folder",
            CheckLevel::Warning,
            format!("This backup is inside a {provider} folder. Reading it may make {provider} download files that are only stored online."),
        ),
        NoteKind::Unreadable(detail) => (
            "unreadable",
            CheckLevel::Warning,
            format!(
                "{file} could not be read: {}. Device details are not shown.",
                problem(n.file, detail, limits)
            ),
        ),
        NoteKind::NoKeybag => (
            "no-keybag",
            CheckLevel::Ok,
            "This unencrypted backup has no key bag. That does not affect checking it.".to_owned(),
        ),
        NoteKind::ArchiveLibraryInsideBackup => (
            "library-inside-backup",
            CheckLevel::Error,
            LIBRARY_INSIDE_BACKUP.to_owned(),
        ),
    };
    ValidationCheck {
        id: id.to_owned(),
        level: lvl,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE_SENTINEL: &str = "Synthetic Test Phone";
    const SNAPSHOT_SENTINEL: &str = "synthetic-uploading";

    fn every_error() -> Vec<(BackupError, ErrorCode)> {
        use std::io::ErrorKind as K;
        let mut v = vec![
            (
                BackupError::NotABackup(RefusalReason::NoBackupsFound),
                ErrorCode::NotABackup,
            ),
            (
                BackupError::Incomplete {
                    snapshot: SnapshotState::Other,
                },
                ErrorCode::IncompleteBackup,
            ),
            (BackupError::PasswordRequired, ErrorCode::PasswordRequired),
            (BackupError::WrongPassword, ErrorCode::WrongPassword),
            (BackupError::KeybagDamaged, ErrorCode::MalformedBackup),
            (
                BackupError::Sqlite { code: Some(11) },
                ErrorCode::MalformedBackup,
            ),
            (
                BackupError::Sqlite { code: None },
                ErrorCode::MalformedBackup,
            ),
            (
                BackupError::Io {
                    op: oma_backup::IoOp::Write,
                    side: IoSide::Staging,
                    kind: K::StorageFull,
                    os_code: None,
                },
                ErrorCode::Io,
            ),
            (
                BackupError::Io {
                    op: oma_backup::IoOp::Open,
                    side: IoSide::Source,
                    kind: K::PermissionDenied,
                    os_code: Some(32),
                },
                ErrorCode::Io,
            ),
            (
                BackupError::Io {
                    op: oma_backup::IoOp::Write,
                    side: IoSide::Sink,
                    kind: K::StorageFull,
                    os_code: None,
                },
                ErrorCode::Io,
            ),
            (
                BackupError::StagingRefused(StagingRefusal::Guard),
                ErrorCode::Internal,
            ),
            (BackupError::Internal("x"), ErrorCode::Internal),
            (BackupError::SourceChanged, ErrorCode::Io),
            (
                BackupError::SchemaExceedsLimits,
                ErrorCode::UnsupportedBackup,
            ),
        ];
        for r in [
            UnsupportedReason::LegacyMbdb,
            UnsupportedReason::NoKeybag,
            UnsupportedReason::EncryptedWithoutManifestKey,
            UnsupportedReason::SingleStageKdf,
            UnsupportedReason::NoPassphraseClasses,
            UnsupportedReason::KeyType(1),
            UnsupportedReason::EncryptionUnknown,
        ] {
            v.push((BackupError::Unsupported(r), ErrorCode::UnsupportedBackup));
        }
        for f in [
            FileUnavailable::BlobMissing,
            FileUnavailable::BlobNotRegular,
            FileUnavailable::NoKey,
            FileUnavailable::NoProtectionClass,
            FileUnavailable::ClassKeyUnavailable,
            FileUnavailable::Ambiguous,
            FileUnavailable::NotAFile,
            FileUnavailable::NotListed,
            FileUnavailable::InvalidEntry,
        ] {
            v.push((BackupError::FileUnavailable(f), ErrorCode::MalformedBackup));
        }
        for s in [
            StagingRefusal::NotARealDirectory,
            StagingRefusal::WrongOwner,
            StagingRefusal::LoosePermissions,
            StagingRefusal::LockUnsupported,
        ] {
            v.push((BackupError::StagingRefused(s), ErrorCode::Io));
        }
        for what in [
            LimitKind::Dpic,
            LimitKind::Iter,
            LimitKind::KeybagBytes,
            LimitKind::KeybagClasses,
            LimitKind::PlistBytes,
            LimitKind::PlistEvents,
            LimitKind::PlistDepth,
            LimitKind::MbFileObjects,
            LimitKind::ManifestDbBytes,
            LimitKind::ManifestRows,
            LimitKind::MbFileBytes,
            LimitKind::RootEntries,
            LimitKind::SmsDbBytes,
            LimitKind::SummaryArray,
            LimitKind::AttachmentBytes,
        ] {
            v.push((
                BackupError::LimitExceeded {
                    what,
                    value: u64::MAX,
                    cap: 20_000_000,
                },
                ErrorCode::MalformedBackup,
            ));
        }
        v.push((
            BackupError::LimitExceeded {
                what: LimitKind::PasswordBytes,
                value: 1025,
                cap: 1024,
            },
            ErrorCode::PasswordTooLong,
        ));
        for file in [
            BackupFile::InfoPlist,
            BackupFile::StatusPlist,
            BackupFile::ManifestPlist,
            BackupFile::ManifestDb,
            BackupFile::ManifestDbWal,
            BackupFile::ManifestMbdb,
            BackupFile::Keybag,
            BackupFile::ManifestKey,
            BackupFile::Blob,
        ] {
            for detail in [
                MalformedDetail::Missing,
                MalformedDetail::TooLarge,
                MalformedDetail::PlistSyntax,
                MalformedDetail::UnexpectedType("IsEncrypted"),
                MalformedDetail::BudgetExceeded,
                MalformedDetail::Truncated,
                MalformedDetail::BadLength { tag: *b"WPKY" },
                MalformedDetail::DuplicateClass(3),
                MalformedDetail::NotSqlite,
                MalformedDetail::Schema("a required column is missing"),
                MalformedDetail::Padding,
                MalformedDetail::NotBlockAligned,
                MalformedDetail::Empty,
                MalformedDetail::Inconsistent("encrypted but plaintext"),
                MalformedDetail::UidOutOfRange,
                MalformedDetail::NestingTooDeep,
                MalformedDetail::NotRegularFile,
                MalformedDetail::ReadError(std::io::ErrorKind::PermissionDenied),
            ] {
                v.push((
                    BackupError::Malformed { file, detail },
                    ErrorCode::MalformedBackup,
                ));
            }
        }
        v
    }

    #[test]
    fn every_backup_error_maps_to_its_code_with_a_clean_sentence() {
        let limits = Limits::default();
        for (err, code) in every_error() {
            let ipc = to_ipc(&err, &limits);
            assert_eq!(ipc.code, code, "{err:?}");
            assert!(!ipc.message.is_empty());
            assert!(ipc.message.ends_with('.'), "{}", ipc.message);
            for sentinel in [DEVICE_SENTINEL, SNAPSHOT_SENTINEL, "SYNTHETIC0"] {
                assert!(!ipc.message.contains(sentinel));
            }
        }
    }

    #[test]
    fn loose_staging_permissions_do_not_say_remove_and_retry_only() {
        let ipc = to_ipc(
            &BackupError::StagingRefused(StagingRefusal::LoosePermissions),
            &Limits::default(),
        );
        assert_eq!(ipc.code, ErrorCode::Io);
        assert_eq!(ipc.message, STAGING_NOT_PRIVATE);
        assert!(ipc.message.contains("another drive"));
        let other = to_ipc(
            &BackupError::StagingRefused(StagingRefusal::WrongOwner),
            &Limits::default(),
        );
        assert_eq!(other.message, STAGING_NOT_SAFE);
    }

    #[test]
    fn the_dpic_message_matches_the_spec_example() {
        let e = BackupError::LimitExceeded {
            what: LimitKind::Dpic,
            value: 4_294_967_295,
            cap: 20_000_000,
        };
        assert_eq!(
            to_ipc(&e, &Limits::default()).message,
            "The backup's key data asks for 4,294,967,295 key-derivation rounds; the limit is 20,000,000. The backup may be damaged."
        );
    }

    #[test]
    fn incomplete_reason_shows_the_sanitised_value_only_in_the_reason() {
        let c = Classification::Incomplete {
            snapshot: SnapshotState::Other,
            snapshot_dir: true,
        };
        let (state, reason) = state_of(&c, true, Some(SNAPSHOT_SENTINEL), &Limits::default());
        assert_eq!(state, BackupState::Incomplete);
        assert!(reason.contains(SNAPSHOT_SENTINEL));
        assert!(reason.contains("Snapshot folder"));
        let err = classification_error(&c, &Limits::default()).expect("error");
        assert_eq!(err.code, ErrorCode::IncompleteBackup);
        assert!(!err.message.contains(SNAPSHOT_SENTINEL));
    }

    /// Both the list's reason and the job error say the backup did not
    /// finish and how to run it again: phone unlocked, passcode confirmed
    /// on iOS 16 or later.
    #[test]
    fn incomplete_messages_say_how_to_run_the_backup_again() {
        let c = Classification::Incomplete {
            snapshot: SnapshotState::Other,
            snapshot_dir: false,
        };
        let (_, reason) = state_of(&c, true, Some("uploading"), &Limits::default());
        assert_eq!(
            reason,
            format!(
                "This backup did not finish (Status.plist reports \u{201c}uploading\u{201d}). {RUN_BACKUP_AGAIN} Then check again."
            )
        );
        let err = classification_error(&c, &Limits::default()).expect("error");
        for text in [&reason, &err.message] {
            assert!(text.starts_with("This backup did not finish"), "{text}");
            assert!(text.contains("Run the backup again"), "{text}");
            assert!(text.contains("unlocked"), "{text}");
            assert!(text.contains("iOS 16 or later"), "{text}");
            assert!(text.contains("passcode"), "{text}");
        }
        let (_, missing) = state_of(&c, false, None, &Limits::default());
        assert!(missing.starts_with("This backup did not finish (no Status.plist was found)."));
    }

    #[test]
    fn unreadable_reason_names_the_file_and_the_problem() {
        let (state, reason) = state_of(
            &Classification::Unreadable {
                file: BackupFile::StatusPlist,
                detail: MalformedDetail::TooLarge,
            },
            true,
            None,
            &Limits::default(),
        );
        assert_eq!(state, BackupState::Unreadable);
        assert_eq!(
            reason,
            "Status.plist could not be read: it is larger than 1 MiB."
        );
    }

    /// One case per row of M3 §16.5.
    #[test]
    fn every_messages_error_row_maps_by_stage() {
        use oma_messages::Stage;
        let limits = Limits::default();
        let cases: Vec<(MessagesError, ErrorCode, &str)> = vec![
            (
                MessagesError::Sqlite { code: Some(11), stage: Stage::Messages },
                ErrorCode::MalformedBackup,
                "The Messages database in this backup is damaged and could not be read (SQLite error 11).",
            ),
            (
                MessagesError::Sqlite { code: Some(26), stage: Stage::Scan },
                ErrorCode::MalformedBackup,
                "The Messages database in this backup is damaged and could not be read (SQLite error 26).",
            ),
            (
                MessagesError::Sqlite { code: Some(1), stage: Stage::Probe },
                ErrorCode::MalformedBackup,
                "The Messages database in this backup could not be read (SQLite error 1).",
            ),
            (
                MessagesError::Backup {
                    source: BackupError::FileUnavailable(FileUnavailable::BlobMissing),
                    stage: BackupStage::Staging,
                },
                ErrorCode::MalformedBackup,
                "The Messages database could not be copied from the backup: a file the check needs is missing from the backup.",
            ),
            (
                MessagesError::Backup {
                    source: BackupError::SourceChanged,
                    stage: BackupStage::Staging,
                },
                ErrorCode::Io,
                SMS_SOURCE_CHANGED,
            ),
            (
                MessagesError::Backup {
                    source: BackupError::SourceChanged,
                    stage: BackupStage::Opening,
                },
                ErrorCode::Io,
                SMS_SOURCE_CHANGED,
            ),
            (
                MessagesError::Backup {
                    source: BackupError::Sqlite { code: Some(14) },
                    stage: BackupStage::Opening,
                },
                ErrorCode::MalformedBackup,
                "The Messages database copy could not be opened (SQLite error 14).",
            ),
            (
                MessagesError::Backup {
                    source: BackupError::FileUnavailable(FileUnavailable::Ambiguous),
                    stage: BackupStage::ManifestLookup,
                },
                ErrorCode::MalformedBackup,
                "The backup could not be checked: a file the check needs is listed more than once. The backup may be damaged.",
            ),
            (
                MessagesError::Backup {
                    source: BackupError::Sqlite { code: Some(11) },
                    stage: BackupStage::AttachmentProbe,
                },
                ErrorCode::MalformedBackup,
                "The backup's file index could not be read (SQLite error 11). The backup may be damaged.",
            ),
            (
                MessagesError::Backup {
                    source: BackupError::Io {
                        op: oma_backup::IoOp::Write,
                        side: IoSide::Staging,
                        kind: io::ErrorKind::StorageFull,
                        os_code: None,
                    },
                    stage: BackupStage::Staging,
                },
                ErrorCode::Io,
                DISK_FULL,
            ),
            (
                MessagesError::Backup {
                    source: BackupError::Io {
                        op: oma_backup::IoOp::Remove,
                        side: IoSide::Staging,
                        kind: io::ErrorKind::PermissionDenied,
                        os_code: None,
                    },
                    stage: BackupStage::Removing,
                },
                ErrorCode::Internal,
                "Internal error (removing the staged copy reported as an error). Please report this as a bug.",
            ),
            (
                MessagesError::Internal("x"),
                ErrorCode::Internal,
                "Internal error (x). Please report this as a bug.",
            ),
            (
                MessagesError::Cancelled,
                ErrorCode::Internal,
                "Internal error (cancelled job reported as an error). Please report this as a bug.",
            ),
        ];
        for (e, code, message) in cases {
            let ipc = messages_to_ipc(&e, &limits);
            assert_eq!(ipc.code, code, "{e:?}");
            assert_eq!(ipc.message, message, "{e:?}");
        }
        // "File index" belongs to Manifest.db only: never in a sentence
        // about the Messages database copy.
        for stage in [BackupStage::Staging, BackupStage::Opening] {
            for source in [
                BackupError::Sqlite { code: Some(14) },
                BackupError::SchemaExceedsLimits,
                BackupError::FileUnavailable(FileUnavailable::NotListed),
                BackupError::SourceChanged,
            ] {
                let m = messages_to_ipc(&MessagesError::Backup { source, stage }, &limits).message;
                if stage == BackupStage::Opening {
                    assert!(!m.contains("file index"), "{m}");
                }
                assert!(m.ends_with('.'), "{m}");
            }
        }
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(20_000_000), "20,000,000");
    }

    #[test]
    fn finding_sentences_read_well() {
        let c = Check {
            id: CheckId::BlobSizes,
            level: oma_backup::CheckLevel::Ok,
            findings: vec![Finding::SizeMismatches(2)],
        };
        assert_eq!(
            check(&c, false).message,
            "2 recorded sizes differ from the actual size. Recorded sizes often differ from actual sizes; this is expected."
        );
        let c = Check {
            id: CheckId::KeyClasses,
            level: oma_backup::CheckLevel::Warning,
            findings: vec![Finding::FilesWithoutKey(1)],
        };
        assert_eq!(check(&c, true).message, "1 file has no encryption key.");
        let c = Check {
            id: CheckId::KeyClasses,
            level: oma_backup::CheckLevel::Ok,
            findings: vec![Finding::NotEncrypted],
        };
        assert_eq!(
            check(&c, false).message,
            "No encryption keys are needed: the backup is not encrypted."
        );
    }
}
