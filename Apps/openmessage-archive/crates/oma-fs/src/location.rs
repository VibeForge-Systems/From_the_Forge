//! Facts about where a directory sits: inside an iPhone backup, inside the
//! system's MobileSync backup store, at a filesystem root, or in a folder a
//! cloud client synchronizes.
//!
//! These guards protect the project's two hardest invariants: the original
//! backup is never modified, and message data stays local unless the user
//! deliberately puts it somewhere else.

use std::path::{Component, Path};

/// Files found at the top of an iPhone backup directory. `Manifest.db` is
/// iOS 10 and later; `Manifest.mbdb` is iOS 9 and earlier; the plists are
/// present in both. A backup that is still being written may lack the
/// manifest database, so the test below requires any two markers rather than
/// one specific file.
const BACKUP_MARKERS: &[&str] = &[
    "Manifest.db",
    "Manifest.mbdb",
    "Manifest.plist",
    "Info.plist",
    "Status.plist",
];

/// `true` when `dir` has at least two of the files that make up the top
/// level of an iPhone backup. A heuristic: it identifies the layout, not a
/// valid backup — validation is the backup importer's job.
pub fn looks_like_backup_dir(dir: &Path) -> bool {
    BACKUP_MARKERS
        .iter()
        .filter(|marker| dir.join(marker).is_file())
        .count()
        >= 2
}

/// The nearest of `path` and its ancestors that looks like an iPhone backup,
/// if any. Used to refuse writing archive data inside a backup.
pub fn enclosing_backup_dir(path: &Path) -> Option<&Path> {
    path.ancestors().find(|dir| looks_like_backup_dir(dir))
}

/// `true` when `path` is, or is inside, a `MobileSync/Backup` directory —
/// the store Finder, iTunes and the Apple Devices app manage. Writing there
/// can confuse those tools and risks the user's real backups. Matched by
/// component, case-insensitively, so it holds on every platform's layout
/// (`%APPDATA%\Apple Computer\MobileSync\Backup`,
/// `%USERPROFILE%\Apple\MobileSync\Backup`,
/// `~/Library/Application Support/MobileSync/Backup`).
pub fn is_within_mobilesync(path: &Path) -> bool {
    let names: Vec<&str> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    names
        .windows(2)
        .any(|w| w[0].eq_ignore_ascii_case("MobileSync") && w[1].eq_ignore_ascii_case("Backup"))
}

/// `true` when `path` is a filesystem root (`/`, `C:\`, `\\server\share\`).
/// Expects an absolute, canonical path.
pub fn is_filesystem_root(path: &Path) -> bool {
    path.parent().is_none()
}

/// Folder names that desktop sync clients create by default, matched against
/// whole path components. `OneDrive - <Organization>` is the business form.
const CLOUD_SYNC_FOLDERS: &[(&str, &str)] = &[
    ("OneDrive", "OneDrive"),
    ("Dropbox", "Dropbox"),
    ("iCloudDrive", "iCloud Drive"),
    ("iCloud Drive", "iCloud Drive"),
    ("Google Drive", "Google Drive"),
    ("GoogleDrive", "Google Drive"),
    ("My Drive", "Google Drive"),
    ("pCloudDrive", "pCloud"),
    ("pCloud Drive", "pCloud"),
    ("Nextcloud", "Nextcloud"),
    ("ownCloud", "ownCloud"),
    ("MEGA", "MEGA"),
    ("Sync", "Sync.com"),
];

/// The likely sync client when a path sits inside a folder that is
/// conventionally synchronized off the machine. This is a heuristic on
/// folder *names*: it can miss a renamed sync root and can flag an ordinary
/// folder that happens to share a name. Use it to warn, never to block.
pub fn cloud_sync_provider(path: &Path) -> Option<&'static str> {
    path.components().find_map(|component| {
        let Component::Normal(name) = component else {
            return None;
        };
        let name = name.to_str()?;
        if name.starts_with("OneDrive - ") {
            return Some("OneDrive");
        }
        CLOUD_SYNC_FOLDERS
            .iter()
            .find(|(folder, _)| folder.eq_ignore_ascii_case(name))
            .map(|(_, provider)| *provider)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"").expect("write marker");
    }

    #[test]
    fn a_backup_layout_is_recognized_and_a_single_marker_is_not() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        assert!(!looks_like_backup_dir(dir));
        touch(dir, "Info.plist");
        assert!(!looks_like_backup_dir(dir), "one marker is not enough");
        touch(dir, "Manifest.db");
        assert!(looks_like_backup_dir(dir));
    }

    #[test]
    fn an_in_progress_backup_without_manifest_db_is_still_recognized() {
        let tmp = tempfile::tempdir().expect("tempdir");
        touch(tmp.path(), "Info.plist");
        touch(tmp.path(), "Status.plist");
        assert!(looks_like_backup_dir(tmp.path()));
    }

    #[test]
    fn markers_must_be_files_not_directories() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(tmp.path().join("Manifest.db")).expect("mkdir");
        fs::create_dir(tmp.path().join("Info.plist")).expect("mkdir");
        assert!(!looks_like_backup_dir(tmp.path()));
    }

    #[test]
    fn enclosing_backup_is_found_from_a_nested_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let backup = tmp.path().join("00008030-001A2B3C4D5E6F70");
        let nested = backup.join("3d").join("deeper");
        fs::create_dir_all(&nested).expect("mkdir");
        touch(&backup, "Manifest.plist");
        touch(&backup, "Manifest.db");

        assert_eq!(enclosing_backup_dir(&nested), Some(backup.as_path()));
        assert_eq!(enclosing_backup_dir(&backup), Some(backup.as_path()));
        assert_eq!(enclosing_backup_dir(tmp.path()), None);
    }

    #[test]
    fn mobilesync_store_is_detected_on_every_platform_layout() {
        for p in [
            "C:\\Users\\a\\AppData\\Roaming\\Apple Computer\\MobileSync\\Backup",
            "C:\\Users\\a\\Apple\\MobileSync\\Backup\\00008030-0011",
            "/Users/a/Library/Application Support/MobileSync/Backup/x",
            "/home/a/mobilesync/backup",
        ] {
            let path: PathBuf = p.split(['\\', '/']).collect();
            assert!(is_within_mobilesync(&path), "{p}");
        }
        let unrelated: PathBuf = ["home", "a", "MobileSync", "Archive"].iter().collect();
        assert!(!is_within_mobilesync(&unrelated));
        let reversed: PathBuf = ["Backup", "MobileSync"].iter().collect();
        assert!(!is_within_mobilesync(&reversed));
    }

    #[test]
    fn filesystem_roots_are_detected() {
        assert!(is_filesystem_root(Path::new("/")));
        assert!(!is_filesystem_root(Path::new("/home")));
        #[cfg(windows)]
        {
            assert!(is_filesystem_root(Path::new("C:\\")));
            assert!(!is_filesystem_root(Path::new("C:\\Users")));
        }
    }

    #[test]
    fn cloud_sync_folders_are_flagged_by_whole_component() {
        let cases: &[(&[&str], Option<&str>)] = &[
            (&["home", "a", "Dropbox", "archives"], Some("Dropbox")),
            (&["Users", "a", "OneDrive - Contoso", "x"], Some("OneDrive")),
            (&["Users", "a", "onedrive"], Some("OneDrive")),
            (&["Users", "a", "iCloudDrive"], Some("iCloud Drive")),
            (
                &["Volumes", "GoogleDrive", "My Drive"],
                Some("Google Drive"),
            ),
            (&["home", "a", "DropboxArchive"], None),
            (&["home", "a", "Documents", "Messages"], None),
        ];
        for (parts, expected) in cases {
            let path: PathBuf = parts.iter().collect();
            assert_eq!(cloud_sync_provider(&path), *expected, "{path:?}");
        }
    }
}
