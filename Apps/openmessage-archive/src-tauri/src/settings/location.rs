//! Deciding whether a directory may hold the archive library.
//!
//! The rules encode two project invariants. Archive data must never be
//! written inside an iPhone backup (the original is immutable), and the user
//! must be told when message data would leave the machine through a sync
//! client. Each rule is reported as a separate check so the UI can show the
//! user exactly what passed and what did not.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum CheckLevel {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct LocationCheck {
    /// Stable identifier, e.g. `outside-backup`. Tests and the UI key on it.
    pub id: String,
    pub level: CheckLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum LocationState {
    /// No archive location has been chosen.
    Unset,
    /// Every check passed.
    Ready,
    /// Usable, but at least one check deserves the user's attention.
    Warning,
    /// At least one check failed; the location cannot be used.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ArchiveLocationStatus {
    /// The path as it will be used: canonical when it could be resolved,
    /// otherwise as configured.
    pub path: Option<String>,
    pub state: LocationState,
    pub checks: Vec<LocationCheck>,
}

impl ArchiveLocationStatus {
    pub fn unset() -> Self {
        Self {
            path: None,
            state: LocationState::Unset,
            checks: Vec::new(),
        }
    }

    pub fn is_usable(&self) -> bool {
        matches!(self.state, LocationState::Ready | LocationState::Warning)
    }
}

/// Run every check against `path`. Never fails: problems become checks.
///
/// Checks stop at the first error that makes later checks meaningless (a
/// missing directory cannot be probed for writability), so a failing status
/// lists what was established, not a cascade of follow-on failures.
pub fn inspect_location(path: &Path) -> ArchiveLocationStatus {
    let mut checks = Vec::new();
    let display = path.to_str().map(str::to_owned);

    let finish = |path: Option<String>, checks: Vec<LocationCheck>| {
        let worst = checks.iter().map(|c| c.level).max();
        let state = match worst {
            Some(CheckLevel::Error) => LocationState::Error,
            Some(CheckLevel::Warning) => LocationState::Warning,
            _ => LocationState::Ready,
        };
        ArchiveLocationStatus {
            path,
            state,
            checks,
        }
    };

    if path.to_str().is_none() {
        checks.push(error(
            "unicode-path",
            "The folder path contains characters that are not valid Unicode. Choose a folder whose path is plain text.",
        ));
        return finish(None, checks);
    }

    if !path.is_absolute() {
        checks.push(error("absolute", "The folder path must be absolute."));
        return finish(display, checks);
    }

    let canonical: PathBuf = match dunce::canonicalize(path) {
        Ok(p) if p.is_dir() => p,
        Ok(_) => {
            checks.push(error("exists", "The path exists but is not a folder."));
            return finish(display, checks);
        }
        Err(_) => {
            checks.push(error(
                "exists",
                "The folder could not be found. If it is on a removable or network drive, reconnect it and check again.",
            ));
            return finish(display, checks);
        }
    };
    checks.push(ok("exists", "The folder exists."));
    let display = canonical.to_str().map(str::to_owned).or(display);

    if oma_fs::is_filesystem_root(&canonical) {
        checks.push(error(
            "not-root",
            "A drive or filesystem root cannot be the archive location. Choose or create a dedicated folder.",
        ));
        return finish(display, checks);
    }

    if let Some(backup) = oma_fs::enclosing_backup_dir(&canonical) {
        let shown = backup.display();
        checks.push(error(
            "outside-backup",
            format!(
                "This folder is inside an iPhone backup ({shown}). Archives are stored outside backups so the original backup is never modified."
            ),
        ));
        return finish(display, checks);
    }
    if oma_fs::is_within_mobilesync(&canonical) {
        checks.push(error(
            "outside-backup",
            "This folder is inside the MobileSync backup store that Finder, iTunes, and Apple Devices manage. Choose a folder outside it.",
        ));
        return finish(display, checks);
    }
    if oma_fs::is_within_imazing_backups(&canonical) {
        checks.push(error(
            "outside-backup",
            "This folder is inside the iMazing backup store (iMazing's Backups folder) that iMazing manages. Choose a folder outside it.",
        ));
        return finish(display, checks);
    }
    checks.push(ok(
        "outside-backup",
        "The folder is not inside an iPhone backup.",
    ));

    // A volume kind that cannot be read adds nothing: the checks below
    // still run, and archives re-check the volume when they are opened.
    if let Ok(kind) = oma_fs::volume_kind(&canonical) {
        if let Some(check) = local_volume_check(kind) {
            let is_error = check.level == CheckLevel::Error;
            checks.push(check);
            if is_error {
                return finish(display, checks);
            }
        }
    }

    match probe_writable(&canonical) {
        Ok(()) => checks.push(ok("writable", "The folder is writable.")),
        Err(e) => {
            checks.push(error(
                "writable",
                format!("The folder is not writable ({}).", e.kind()),
            ));
            return finish(display, checks);
        }
    }

    // Staging decrypted copies needs a drive that keeps a 0700 folder
    // private. FAT, exFAT and NTFS on Linux (and many network shares) report
    // modes from mount options instead, and staging refuses such a folder, so
    // say so here rather than on every check.
    #[cfg(unix)]
    match keeps_private_permissions(&canonical) {
        Ok(true) => checks.push(ok(
            "private",
            "The folder's drive keeps files private to your user account.",
        )),
        Ok(false) => {
            checks.push(error(
                "private",
                "This folder's drive does not keep Linux file permissions (for example a FAT, exFAT or NTFS drive, or a network share), so other users of this computer could read the copies made while checking a backup. Choose a folder on another drive, or mount this one with a private umask (dmask=077, fmask=077).",
            ));
            return finish(display, checks);
        }
        Err(e) => {
            checks.push(error(
                "private",
                format!(
                    "The folder's permissions could not be checked ({}).",
                    e.kind()
                ),
            ));
            return finish(display, checks);
        }
    }

    match oma_fs::cloud_sync_provider(&canonical) {
        Some(provider) => checks.push(warning(
            "local-only",
            format!(
                "This folder looks like it is synchronized by {provider}. Archived messages and attachments would leave this computer. Choose a local folder unless that is what you intend."
            ),
        )),
        None => checks.push(ok(
            "local-only",
            "The folder is not in a recognized cloud-sync location.",
        )),
    }

    finish(display, checks)
}

/// The `local-volume` check for a volume kind, or `None` when the kind is
/// not known and there is nothing to say.
///
/// - Network: an error. SQLite's write-ahead log, which archives use, "does
///   not work over a network filesystem" (sqlite.org/wal.html), and staging
///   needs file locks.
/// - Virtualised (FUSE, 9P, VirtualBox shared folders): a warning. Backup
///   checks keep working there, as before, but archives refuse such a
///   folder because file locks and the write-ahead log are unreliable on it.
/// - FAT and exFAT: a warning. No hard links, and FAT's 4 GiB file limit.
fn local_volume_check(kind: oma_fs::VolumeKind) -> Option<LocationCheck> {
    use oma_fs::VolumeKind;
    let label = kind.label();
    match kind {
        VolumeKind::Local => Some(ok("local-volume", "The folder is on a local drive.")),
        VolumeKind::Network(_) => Some(error(
            "local-volume",
            format!(
                "This folder is on a network drive ({label}). The archive database cannot work reliably over a network file system, and checking backups needs file locks. Choose a folder on a drive inside this computer."
            ),
        )),
        VolumeKind::Virtualised(_) => Some(warning(
            "local-volume",
            format!(
                "This folder is on a shared or user-space file system ({label}), such as a virtual machine's shared folder. Backups can be checked here, but archives will not be created in it, because file locks and the database's write-ahead log are unreliable there. Choose a folder on a drive inside this computer to keep archives."
            ),
        )),
        VolumeKind::Fat(_) => Some(warning(
            "local-volume",
            format!(
                "This folder is on a {label} drive. Such drives cannot hold files of 4 GiB or more (FAT) and do not support hard links, so very large attachments may not be archived here."
            ),
        )),
        _ => None,
    }
}

/// Create, write, and remove a uniquely named file. Proves write access the
/// way the archive will use it, which permission bits alone cannot (ACLs,
/// read-only mounts, and quota all show up here).
fn probe_writable(dir: &Path) -> std::io::Result<()> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let probe = dir.join(format!(".oma-write-probe-{}-{nanos}", std::process::id()));
    let result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|mut f| f.write_all(b"probe").and_then(|()| f.sync_all()));
    let removed = fs::remove_file(&probe);
    result.and(removed)
}

/// Create a folder with mode 0700 and report whether the drive kept it
/// private. The umask can only remove bits, so group or other bits on the new
/// folder mean the filesystem ignores the requested mode.
#[cfg(unix)]
fn keeps_private_permissions(dir: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let probe = dir.join(format!(".oma-mode-probe-{}-{nanos}", std::process::id()));
    fs::DirBuilder::new().mode(0o700).create(&probe)?;
    let mode = fs::symlink_metadata(&probe).map(|m| m.mode());
    let removed = fs::remove_dir(&probe);
    let mode = mode?;
    removed?;
    Ok(is_private_mode(mode))
}

/// No permission bits for group or others.
#[cfg(unix)]
fn is_private_mode(mode: u32) -> bool {
    mode & 0o077 == 0
}

fn ok(id: &str, message: impl Into<String>) -> LocationCheck {
    check(id, CheckLevel::Ok, message)
}

fn warning(id: &str, message: impl Into<String>) -> LocationCheck {
    check(id, CheckLevel::Warning, message)
}

fn error(id: &str, message: impl Into<String>) -> LocationCheck {
    check(id, CheckLevel::Error, message)
}

fn check(id: &str, level: CheckLevel, message: impl Into<String>) -> LocationCheck {
    LocationCheck {
        id: id.to_owned(),
        level,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level_of(status: &ArchiveLocationStatus, id: &str) -> Option<CheckLevel> {
        status.checks.iter().find(|c| c.id == id).map(|c| c.level)
    }

    #[test]
    fn a_plain_writable_folder_is_ready() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let status = inspect_location(tmp.path());
        assert_eq!(status.state, LocationState::Ready, "{status:#?}");
        assert!(status.is_usable());
        #[cfg(unix)]
        assert_eq!(level_of(&status, "private"), Some(CheckLevel::Ok));
        for id in ["exists", "outside-backup", "writable", "local-only"] {
            assert_eq!(level_of(&status, id), Some(CheckLevel::Ok), "{id}");
        }
        // Ok on a recognised local file system; absent where the kind is
        // unknown (every drive letter on Windows).
        assert!(matches!(
            level_of(&status, "local-volume"),
            Some(CheckLevel::Ok) | None
        ));
        assert_eq!(
            fs::read_dir(tmp.path()).expect("read_dir").count(),
            0,
            "the write probe must clean up after itself"
        );
    }

    /// A drive that reports modes from mount options (vfat with the usual
    /// umask 022 reports 0755) is not private; only 0700-style modes are.
    #[cfg(unix)]
    #[test]
    fn modes_with_group_or_other_bits_are_not_private() {
        assert!(is_private_mode(0o040700));
        assert!(!is_private_mode(0o040755));
        assert!(!is_private_mode(0o040777));
        assert!(!is_private_mode(0o040770));
    }

    #[test]
    fn local_volume_levels_follow_the_volume_kind() {
        use oma_fs::{FatKind, NetworkKind, VirtKind, VolumeKind};
        let level = |kind| local_volume_check(kind).map(|c| c.level);
        assert_eq!(level(VolumeKind::Local), Some(CheckLevel::Ok));
        for kind in [
            VolumeKind::Network(NetworkKind::Nfs),
            VolumeKind::Network(NetworkKind::Smb),
            VolumeKind::Network(NetworkKind::Unc),
        ] {
            assert_eq!(level(kind), Some(CheckLevel::Error), "{kind:?}");
        }
        for kind in [
            VolumeKind::Virtualised(VirtKind::Fuse),
            VolumeKind::Virtualised(VirtKind::NineP),
            VolumeKind::Virtualised(VirtKind::VboxSf),
            VolumeKind::Fat(FatKind::Msdos),
            VolumeKind::Fat(FatKind::ExFat),
        ] {
            assert_eq!(level(kind), Some(CheckLevel::Warning), "{kind:?}");
        }
        assert_eq!(level(VolumeKind::Unknown), None);
        let check = local_volume_check(VolumeKind::Network(NetworkKind::Smb)).expect("check");
        assert_eq!(check.id, "local-volume");
        assert!(check.message.contains("SMB/CIFS"), "{}", check.message);
    }

    /// A UNC path is a network volume before any disk access, so the check
    /// stops there: no write probe is made on the share.
    #[cfg(windows)]
    #[test]
    fn a_unc_location_is_refused_as_a_network_volume() {
        let status = inspect_location(Path::new(r"\\localhost\C$\Windows"));
        // The administrative share may not be reachable on every machine;
        // then `exists` fails first, which is also a refusal.
        assert_eq!(status.state, LocationState::Error);
        if level_of(&status, "exists") == Some(CheckLevel::Ok) {
            assert_eq!(level_of(&status, "local-volume"), Some(CheckLevel::Error));
        }
        assert_eq!(level_of(&status, "writable"), None);
    }

    #[test]
    fn relative_paths_are_rejected() {
        let status = inspect_location(Path::new("relative/archive"));
        assert_eq!(status.state, LocationState::Error);
        assert_eq!(level_of(&status, "absolute"), Some(CheckLevel::Error));
    }

    #[test]
    fn missing_folders_are_an_error_and_stop_further_checks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let status = inspect_location(&tmp.path().join("not-here"));
        assert_eq!(status.state, LocationState::Error);
        assert_eq!(level_of(&status, "exists"), Some(CheckLevel::Error));
        assert_eq!(level_of(&status, "writable"), None);
    }

    #[test]
    fn a_file_is_not_a_folder() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("file.txt");
        fs::write(&file, b"x").expect("write");
        let status = inspect_location(&file);
        assert_eq!(level_of(&status, "exists"), Some(CheckLevel::Error));
    }

    #[test]
    fn the_filesystem_root_is_rejected() {
        let root = if cfg!(windows) { "C:\\" } else { "/" };
        let status = inspect_location(Path::new(root));
        assert_eq!(level_of(&status, "not-root"), Some(CheckLevel::Error));
    }

    #[test]
    fn folders_inside_a_backup_are_rejected_and_the_backup_is_not_touched() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let backup = tmp.path().join("00008030-001A2B3C4D5E6F70");
        let inner = backup.join("ab");
        fs::create_dir_all(&inner).expect("mkdir");
        fs::write(backup.join("Manifest.plist"), b"").expect("write");
        fs::write(backup.join("Manifest.db"), b"").expect("write");
        let before = fs::read_dir(&inner).expect("read_dir").count();

        for candidate in [&backup, &inner] {
            let status = inspect_location(candidate);
            assert_eq!(status.state, LocationState::Error);
            assert_eq!(level_of(&status, "outside-backup"), Some(CheckLevel::Error));
            assert_eq!(
                level_of(&status, "writable"),
                None,
                "no write probe inside a backup"
            );
        }
        assert_eq!(fs::read_dir(&inner).expect("read_dir").count(), before);
    }

    #[test]
    fn the_mobilesync_store_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = tmp.path().join("MobileSync").join("Backup");
        fs::create_dir_all(&store).expect("mkdir");
        let status = inspect_location(&store);
        assert_eq!(level_of(&status, "outside-backup"), Some(CheckLevel::Error));
    }

    #[test]
    fn the_imazing_store_is_rejected_without_a_write_probe() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Case differs from iMazing's own spelling on purpose.
        let store = tmp.path().join("IMAZING").join("backups");
        let inner = store.join("library");
        fs::create_dir_all(&inner).expect("mkdir");
        for candidate in [&store, &inner] {
            let status = inspect_location(candidate);
            assert_eq!(status.state, LocationState::Error);
            let check = status
                .checks
                .iter()
                .find(|c| c.id == "outside-backup")
                .expect("outside-backup check");
            assert_eq!(check.level, CheckLevel::Error);
            assert!(check.message.contains("iMazing"), "{}", check.message);
            assert!(!check.message.contains("MobileSync"), "{}", check.message);
            assert_eq!(level_of(&status, "writable"), None);
        }
        assert_eq!(fs::read_dir(&inner).expect("read_dir").count(), 0);
        // The iMazing folder itself, outside Backups, is fine.
        let status = inspect_location(&tmp.path().join("IMAZING"));
        assert_eq!(level_of(&status, "outside-backup"), Some(CheckLevel::Ok));
    }

    #[test]
    fn a_cloud_synced_folder_is_usable_with_a_warning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let synced = tmp.path().join("Dropbox").join("Archives");
        fs::create_dir_all(&synced).expect("mkdir");
        let status = inspect_location(&synced);
        assert_eq!(status.state, LocationState::Warning);
        assert!(status.is_usable());
        assert_eq!(level_of(&status, "local-only"), Some(CheckLevel::Warning));
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_folder_fails_the_write_probe() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let ro = tmp.path().join("ro");
        fs::create_dir(&ro).expect("mkdir");
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).expect("chmod");
        // Root (CAP_DAC_OVERRIDE) ignores permission bits, so the premise of
        // this test does not hold in containers that build as root.
        let bypassed = fs::write(ro.join("canary"), b"").is_ok();
        let status = inspect_location(&ro);
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).expect("chmod back");
        if bypassed {
            eprintln!("skipped: permission bits are not enforced for this user");
            return;
        }
        assert_eq!(status.state, LocationState::Error);
        assert_eq!(level_of(&status, "writable"), Some(CheckLevel::Error));
    }
}
