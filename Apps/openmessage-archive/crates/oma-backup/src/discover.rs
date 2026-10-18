//! Finding backups: default locations, scanning a root, and classifying a
//! folder the user picked (§3).
//!
//! A folder is identified only by its marker files, never by its name: modern
//! UDIDs are 25 characters with a hyphen and older ones 40 hex characters,
//! and neither is a reliable signal.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::guard::is_link;
use crate::limits::Limits;

/// Which default location a root is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RootKind {
    /// `<home>\Apple\MobileSync\Backup`: the Apple Devices app, or iTunes from
    /// the Microsoft Store. The subfolder is **UNVERIFIED by Apple**.
    AppleDevicesOrStoreITunes,
    /// `<roaming>\Apple Computer\MobileSync\Backup`: iTunes from the desktop
    /// installer. The subfolder is **UNVERIFIED by Apple**.
    DesktopITunes,
    /// `<roaming>\iMazing\Backups`: iMazing's own default backup location
    /// (note the plural `Backups`; an older iMazing guide says `Backup`).
    /// Source: observed in iMazing 3.x's Backup Location settings on a
    /// Windows 11 machine, 2026-10-18, where it was the default. The location
    /// is configurable in iMazing, so a relocated store is found only when
    /// the user adds its folder. iMazing's `iMazing.Versions` folder there is
    /// never listed (see [`IMAZING_VERSIONS_DIR_NAME`]).
    IMazing,
    /// A folder the user added.
    Added,
}

/// A default backup root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultRoot {
    /// Which location this is.
    pub kind: RootKind,
    /// The path to scan.
    pub path: PathBuf,
}

/// The standard Windows backup roots, built from the home and roaming
/// AppData folders the shell resolves. A pure function, so it is tested on
/// every OS; the shell calls it only on Windows and passes `(None, None)`
/// elsewhere. Linux has no standard location. Every root is always returned
/// when its base is known, because a machine can have several of these apps
/// installed.
pub fn default_backup_roots(
    home: Option<&Path>,
    roaming_app_data: Option<&Path>,
) -> Vec<DefaultRoot> {
    let mut roots = Vec::new();
    if let Some(home) = home {
        roots.push(DefaultRoot {
            kind: RootKind::AppleDevicesOrStoreITunes,
            path: home.join("Apple").join("MobileSync").join("Backup"),
        });
    }
    if let Some(roaming) = roaming_app_data {
        roots.push(DefaultRoot {
            kind: RootKind::DesktopITunes,
            path: roaming
                .join("Apple Computer")
                .join("MobileSync")
                .join("Backup"),
        });
        roots.push(DefaultRoot {
            kind: RootKind::IMazing,
            path: roaming.join("iMazing").join("Backups"),
        });
    }
    roots
}

/// The folder in which iMazing keeps archived backup snapshots, at its
/// backup location
/// (<https://imazing.com/guides/understanding-the-imazingversions-folder-in-imazing-backup-locations>).
/// Its internal layout is **UNVERIFIED**, so a root scan never lists it as a
/// backup, even if it happens to contain backup marker files. Matched by
/// exact name.
pub const IMAZING_VERSIONS_DIR_NAME: &str = "iMazing.Versions";

/// The outcome of scanning one root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootScanState {
    /// The root exists and was listed.
    Present,
    /// The root does not exist (normal).
    Absent,
    /// The root could not be read.
    Unreadable(io::ErrorKind),
    /// The root has more entries than `limits.root_entries`; only that many
    /// were read.
    TooManyEntries,
}

/// One backup folder found by a scan or picked by the user. `Debug` omits
/// the paths: a folder name is usually the device's UDID.
#[derive(Clone, PartialEq, Eq)]
pub struct Candidate {
    /// `dunce::canonicalize`: used for display and for [`crate::BackupId`].
    pub display_canonical: PathBuf,
    /// `std::fs::canonicalize`: used only for containment checks. On Windows
    /// this is always the verbatim (`\\?\`) form, so prefix comparisons never
    /// mix the two forms.
    pub containment: PathBuf,
    /// The folder, or the root it was found under, is a link or junction.
    pub via_link: bool,
}

impl std::fmt::Debug for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Candidate")
            .field("via_link", &self.via_link)
            .finish_non_exhaustive()
    }
}

impl Candidate {
    /// Build a candidate for an existing directory (for example a remembered
    /// added backup). Does not check that it looks like a backup.
    pub fn for_dir(path: &Path) -> Result<Candidate, RefusalReason> {
        if path.to_str().is_none() {
            return Err(RefusalReason::NonUnicodePath);
        }
        let via_link = fs::symlink_metadata(path)
            .map(|m| is_link(&m))
            .map_err(|_| RefusalReason::NotFound)?;
        let display = dunce::canonicalize(path).map_err(|_| RefusalReason::NotFound)?;
        let containment = fs::canonicalize(path).map_err(|_| RefusalReason::NotFound)?;
        if !fs::metadata(&containment).is_ok_and(|m| m.is_dir()) {
            return Err(RefusalReason::NotADirectory);
        }
        if display.to_str().is_none() {
            return Err(RefusalReason::NonUnicodePath);
        }
        Ok(Candidate {
            display_canonical: display,
            containment,
            via_link,
        })
    }
}

/// The result of [`scan_root`]. `Debug` omits the paths.
#[derive(Clone)]
pub struct RootScan {
    /// The root as given.
    pub root: PathBuf,
    /// The root's display-canonical path, when it exists.
    pub display_canonical: Option<PathBuf>,
    /// The root itself is a link or junction.
    pub via_link: bool,
    /// What happened.
    pub state: RootScanState,
    /// Backups found, at most `limits.backups_per_root`, sorted by path.
    pub candidates: Vec<Candidate>,
    /// Backups found beyond `limits.backups_per_root`, counted only.
    pub over_cap: u32,
    /// Entries skipped because their names are not valid Unicode.
    pub skipped_non_unicode: u32,
}

impl std::fmt::Debug for RootScan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RootScan")
            .field("via_link", &self.via_link)
            .field("state", &self.state)
            .field("candidates", &self.candidates.len())
            .field("over_cap", &self.over_cap)
            .field("skipped_non_unicode", &self.skipped_non_unicode)
            .finish_non_exhaustive()
    }
}

/// Scan one root, one level deep (§3.2). Links are followed at the root level
/// and for its direct children only. Never fails: problems are states.
pub fn scan_root(root: &Path, limits: &Limits) -> RootScan {
    let mut scan = RootScan {
        root: root.to_path_buf(),
        display_canonical: None,
        via_link: false,
        state: RootScanState::Present,
        candidates: Vec::new(),
        over_cap: 0,
        skipped_non_unicode: 0,
    };
    match fs::symlink_metadata(root) {
        Ok(meta) => scan.via_link = is_link(&meta),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            scan.state = RootScanState::Absent;
            return scan;
        }
        Err(e) => {
            scan.state = RootScanState::Unreadable(e.kind());
            return scan;
        }
    }
    let (display, containment) = match (dunce::canonicalize(root), fs::canonicalize(root)) {
        (Ok(d), Ok(c)) => (d, c),
        (Err(e), _) | (_, Err(e)) => {
            scan.state = if e.kind() == io::ErrorKind::NotFound {
                RootScanState::Absent
            } else {
                RootScanState::Unreadable(e.kind())
            };
            return scan;
        }
    };
    scan.display_canonical = Some(display.clone());
    match fs::metadata(&containment) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            scan.state = RootScanState::Unreadable(io::ErrorKind::NotADirectory);
            return scan;
        }
        Err(e) => {
            scan.state = RootScanState::Unreadable(e.kind());
            return scan;
        }
    }
    let entries = match fs::read_dir(&display) {
        Ok(it) => it,
        Err(e) => {
            scan.state = RootScanState::Unreadable(e.kind());
            return scan;
        }
    };

    let entry_cap = usize::try_from(limits.root_entries).unwrap_or(usize::MAX);
    let mut names: Vec<String> = Vec::new();
    let mut seen: usize = 0;
    for entry in entries {
        if seen >= entry_cap {
            scan.state = RootScanState::TooManyEntries;
            break;
        }
        seen = seen.saturating_add(1);
        let Ok(entry) = entry else { continue };
        match entry.file_name().into_string() {
            Ok(name) => names.push(name),
            Err(_) => scan.skipped_non_unicode = scan.skipped_non_unicode.saturating_add(1),
        }
    }
    // read_dir order is unspecified; sort so the cap is deterministic.
    names.sort();

    let per_root = usize::try_from(limits.backups_per_root).unwrap_or(usize::MAX);
    for name in names {
        if name == IMAZING_VERSIONS_DIR_NAME {
            continue;
        }
        // `name` comes from the filesystem listing, not from backup data.
        let child = display.join(&name);
        let Some(candidate) = candidate_child(&child, &containment, scan.via_link) else {
            continue;
        };
        if scan.candidates.len() >= per_root {
            scan.over_cap = scan.over_cap.saturating_add(1);
        } else {
            scan.candidates.push(candidate);
        }
    }
    scan
}

/// A direct child of a root, if it is a directory that looks like a backup.
fn candidate_child(
    child: &Path,
    root_containment: &Path,
    root_via_link: bool,
) -> Option<Candidate> {
    let link_meta = fs::symlink_metadata(child).ok()?;
    let child_is_link = is_link(&link_meta);
    if !fs::metadata(child).ok()?.is_dir() {
        return None;
    }
    let display = dunce::canonicalize(child).ok()?;
    let containment = fs::canonicalize(child).ok()?;
    display.to_str()?;
    if !oma_fs::looks_like_backup_dir(&containment) {
        return None;
    }
    // A child that resolves outside the root is still listed, as a link:
    // users do link individual backups, and every guard works on
    // containment paths anyway.
    let outside = !containment.starts_with(root_containment);
    Some(Candidate {
        display_canonical: display,
        containment,
        via_link: child_is_link || root_via_link || outside,
    })
}

/// What a picked folder turned out to be.
#[derive(Debug, Clone)]
pub enum AddedFolder {
    /// The folder is a backup.
    Backup(Candidate),
    /// The folder is inside a backup (such as its `3d/`); the backup is used.
    BackupAncestor {
        /// The enclosing backup.
        backup: Candidate,
    },
    /// The folder contains backups.
    Parent(RootScan),
}

/// Why a picked folder was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefusalReason {
    /// The path is not valid Unicode.
    NonUnicodePath,
    /// The path does not exist or cannot be resolved.
    NotFound,
    /// The path is not a directory.
    NotADirectory,
    /// Neither a backup, inside one, nor a folder that contains one.
    NoBackupsFound,
    /// The folder is inside the archive library's staging directory.
    InsideStaging,
    /// The archive library is the backup folder or inside it.
    BackupContainsArchiveLibrary,
}

/// The name of the staging directory inside the archive library.
pub const STAGING_DIR_NAME: &str = ".oma-staging";

/// Decide what a folder picked in the native dialog is (§3.3).
pub fn classify_added_folder(
    picked: &Path,
    archive_library: Option<&Path>,
    limits: &Limits,
) -> Result<AddedFolder, RefusalReason> {
    // 1. Unicode.
    if picked.to_str().is_none() {
        return Err(RefusalReason::NonUnicodePath);
    }
    // 2. Exists and is a directory.
    let containment = fs::canonicalize(picked).map_err(|_| RefusalReason::NotFound)?;
    if !fs::metadata(&containment).is_ok_and(|m| m.is_dir()) {
        return Err(RefusalReason::NotADirectory);
    }
    let library_containment = archive_library.and_then(|l| fs::canonicalize(l).ok());
    // 3. Not inside staging.
    if let Some(lib) = &library_containment {
        let staging = lib.join(STAGING_DIR_NAME);
        let staging = fs::canonicalize(&staging).unwrap_or(staging);
        if containment.starts_with(&staging) {
            return Err(RefusalReason::InsideStaging);
        }
    }
    // 4 and 5. A backup, or inside one.
    let backup = if oma_fs::looks_like_backup_dir(&containment) {
        Some((Candidate::for_dir(picked)?, false))
    } else if let Some(ancestor) = oma_fs::enclosing_backup_dir(&containment) {
        Some((Candidate::for_dir(ancestor)?, true))
    } else {
        None
    };
    if let Some((candidate, is_ancestor)) = backup {
        // 6. The library must not be the backup or inside it.
        if let Some(lib) = &library_containment {
            if lib.starts_with(&candidate.containment) {
                return Err(RefusalReason::BackupContainsArchiveLibrary);
            }
        }
        return Ok(if is_ancestor {
            AddedFolder::BackupAncestor { backup: candidate }
        } else {
            AddedFolder::Backup(candidate)
        });
    }
    // 7 and 8. A folder that contains backups.
    let scan = scan_root(picked, limits);
    if scan.candidates.is_empty() && scan.over_cap == 0 {
        return Err(RefusalReason::NoBackupsFound);
    }
    Ok(AddedFolder::Parent(scan))
}

/// Where a backup sits relative to the archive library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryRelation {
    /// Neither contains the other.
    Separate,
    /// The backup is inside the library (allowed, with a note).
    BackupInsideLibrary,
    /// The library is the backup or inside it: the backup must not be
    /// checked, because staging would write into it.
    LibraryInsideBackup,
}

/// Re-checked on every listing (§3.3).
pub fn library_relation(backup: &Candidate, archive_library: &Path) -> LibraryRelation {
    let Ok(lib) = fs::canonicalize(archive_library) else {
        return LibraryRelation::Separate;
    };
    if lib.starts_with(&backup.containment) {
        LibraryRelation::LibraryInsideBackup
    } else if backup.containment.starts_with(&lib) {
        LibraryRelation::BackupInsideLibrary
    } else {
        LibraryRelation::Separate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_backup(dir: &Path) {
        fs::create_dir_all(dir).expect("mkdir");
        for m in [
            "Info.plist",
            "Manifest.plist",
            "Status.plist",
            "Manifest.db",
        ] {
            fs::write(dir.join(m), b"x").expect("marker");
        }
    }

    fn small(entries: u32, per_root: u32) -> Limits {
        Limits {
            root_entries: entries,
            backups_per_root: per_root,
            ..Limits::default()
        }
    }

    #[test]
    fn default_roots_for_every_combination() {
        let home = Path::new("C:/Users/a");
        let roaming = Path::new("C:/Users/a/AppData/Roaming");
        assert!(default_backup_roots(None, None).is_empty());
        let only_home = default_backup_roots(Some(home), None);
        assert_eq!(only_home.len(), 1);
        assert_eq!(
            only_home.first().map(|r| r.kind),
            Some(RootKind::AppleDevicesOrStoreITunes)
        );
        assert!(only_home
            .first()
            .is_some_and(|r| r.path.ends_with(Path::new("Apple/MobileSync/Backup"))));
        let only_roaming = default_backup_roots(None, Some(roaming));
        assert_eq!(
            only_roaming.first().map(|r| r.kind),
            Some(RootKind::DesktopITunes)
        );
        assert!(only_roaming.first().is_some_and(|r| r
            .path
            .ends_with(Path::new("Apple Computer/MobileSync/Backup"))));
        assert_eq!(only_roaming.len(), 2);
        let kinds: Vec<RootKind> = default_backup_roots(Some(home), Some(roaming))
            .iter()
            .map(|r| r.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![
                RootKind::AppleDevicesOrStoreITunes,
                RootKind::DesktopITunes,
                RootKind::IMazing
            ]
        );
    }

    #[test]
    fn the_imazing_root_is_under_roaming_app_data() {
        let roaming = Path::new("C:/Users/a/AppData/Roaming");
        let imazing: Vec<DefaultRoot> = default_backup_roots(None, Some(roaming))
            .into_iter()
            .filter(|r| r.kind == RootKind::IMazing)
            .collect();
        assert_eq!(imazing.len(), 1);
        // The observed name is plural `Backups`, not `Backup`.
        assert!(imazing
            .first()
            .is_some_and(|r| r.path == roaming.join("iMazing").join("Backups")));
        // Never offered without a roaming AppData folder.
        assert!(default_backup_roots(Some(Path::new("C:/Users/a")), None)
            .iter()
            .all(|r| r.kind != RootKind::IMazing));
    }

    #[test]
    fn the_imazing_versions_folder_is_never_a_candidate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_backup(&tmp.path().join("SYNTHETIC0-0000000000000000"));
        // Marker files directly inside the folder: it would otherwise look
        // exactly like a backup.
        make_backup(&tmp.path().join(IMAZING_VERSIONS_DIR_NAME));
        make_backup(
            &tmp.path()
                .join(IMAZING_VERSIONS_DIR_NAME)
                .join("SYNTHETIC1-0000000000000000"),
        );
        let scan = scan_root(tmp.path(), &Limits::default());
        assert_eq!(scan.state, RootScanState::Present);
        assert_eq!(scan.candidates.len(), 1);
        assert_eq!(scan.over_cap, 0);
        let versions = fs::canonicalize(tmp.path().join(IMAZING_VERSIONS_DIR_NAME)).expect("canon");
        assert!(scan
            .candidates
            .iter()
            .all(|c| !c.containment.starts_with(&versions)));
        // Picking the parent folder applies the same rule.
        match classify_added_folder(tmp.path(), None, &Limits::default()) {
            Ok(AddedFolder::Parent(scan)) => assert_eq!(scan.candidates.len(), 1),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn absent_and_non_directory_roots() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let scan = scan_root(&tmp.path().join("nope"), &Limits::default());
        assert_eq!(scan.state, RootScanState::Absent);
        let file = tmp.path().join("file");
        fs::write(&file, b"x").expect("write");
        let scan = scan_root(&file, &Limits::default());
        assert!(matches!(scan.state, RootScanState::Unreadable(_)));
    }

    #[test]
    fn backups_are_found_and_non_backups_skipped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_backup(&tmp.path().join("SYNTHETIC0-0000000000000000"));
        make_backup(&tmp.path().join("b"));
        fs::create_dir(tmp.path().join("not-a-backup")).expect("mkdir");
        fs::write(tmp.path().join("loose-file"), b"x").expect("write");
        let scan = scan_root(tmp.path(), &Limits::default());
        assert_eq!(scan.state, RootScanState::Present);
        assert_eq!(scan.candidates.len(), 2);
        assert!(scan.candidates.iter().all(|c| !c.via_link));
        assert!(!scan.via_link);
    }

    #[test]
    fn entry_and_candidate_caps() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for i in 0..6 {
            make_backup(&tmp.path().join(format!("b{i}")));
        }
        let scan = scan_root(tmp.path(), &small(5, 1024));
        assert_eq!(scan.state, RootScanState::TooManyEntries);
        assert_eq!(scan.candidates.len(), 5);
        let scan = scan_root(tmp.path(), &small(1024, 4));
        assert_eq!(scan.state, RootScanState::Present);
        assert_eq!(scan.candidates.len(), 4);
        assert_eq!(scan.over_cap, 2);
    }

    #[test]
    #[ignore = "creates 1025 directories and 257 backups; run with --ignored"]
    fn production_caps() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for i in 0..257 {
            make_backup(&tmp.path().join(format!("b{i:04}")));
        }
        for i in 0..768 {
            fs::create_dir(tmp.path().join(format!("d{i:04}"))).expect("mkdir");
        }
        let scan = scan_root(tmp.path(), &Limits::default());
        assert_eq!(scan.state, RootScanState::TooManyEntries);
        assert_eq!(scan.candidates.len(), 256);
        assert_eq!(scan.over_cap, 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_root_and_a_linked_child_are_via_link() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real = tmp.path().join("real");
        make_backup(&real.join("b"));
        let outside = tmp.path().join("outside");
        make_backup(&outside);
        std::os::unix::fs::symlink(&outside, real.join("linked")).expect("symlink");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let scan = scan_root(&link, &Limits::default());
        assert!(scan.via_link);
        assert_eq!(scan.candidates.len(), 2);
        let scan = scan_root(&real, &Limits::default());
        assert!(!scan.via_link);
        let linked: Vec<bool> = scan.candidates.iter().map(|c| c.via_link).collect();
        assert_eq!(linked, vec![false, true]);
    }

    #[cfg(windows)]
    #[test]
    fn short_names_and_case_differences_are_not_links() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_backup(&tmp.path().join("b"));
        // The temp dir under a CI runner often contains an 8.3 short name
        // (RUNNER~1); either way, an upper-cased spelling is the same folder.
        let upper = PathBuf::from(tmp.path().to_string_lossy().to_uppercase());
        for root in [tmp.path().to_path_buf(), upper] {
            let scan = scan_root(&root, &Limits::default());
            assert!(!scan.via_link, "{root:?}");
            assert!(scan.candidates.iter().all(|c| !c.via_link));
        }
    }

    #[test]
    fn classify_every_outcome() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let limits = Limits::default();
        let parent = tmp.path().join("parent");
        let backup = parent.join("SYNTHETIC0-0000000000000000");
        make_backup(&backup);
        fs::create_dir(backup.join("3d")).expect("mkdir");
        let library = tmp.path().join("library");
        fs::create_dir_all(library.join(STAGING_DIR_NAME).join("s0")).expect("mkdir");
        let empty = tmp.path().join("empty");
        fs::create_dir(&empty).expect("mkdir");
        let file = tmp.path().join("file");
        fs::write(&file, b"x").expect("write");

        assert!(matches!(
            classify_added_folder(&backup, Some(&library), &limits),
            Ok(AddedFolder::Backup(_))
        ));
        match classify_added_folder(&backup.join("3d"), Some(&library), &limits) {
            Ok(AddedFolder::BackupAncestor { backup: b }) => {
                assert_eq!(b.containment, fs::canonicalize(&backup).expect("canon"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            classify_added_folder(&parent, Some(&library), &limits),
            Ok(AddedFolder::Parent(scan)) if scan.candidates.len() == 1
        ));
        assert_eq!(
            classify_added_folder(&empty, Some(&library), &limits).err(),
            Some(RefusalReason::NoBackupsFound)
        );
        assert_eq!(
            classify_added_folder(&tmp.path().join("missing"), None, &limits).err(),
            Some(RefusalReason::NotFound)
        );
        assert_eq!(
            classify_added_folder(&file, None, &limits).err(),
            Some(RefusalReason::NotADirectory)
        );
        assert_eq!(
            classify_added_folder(
                &library.join(STAGING_DIR_NAME).join("s0"),
                Some(&library),
                &limits
            )
            .err(),
            Some(RefusalReason::InsideStaging)
        );
    }

    #[test]
    fn a_library_inside_a_picked_backup_is_refused_but_a_shared_parent_is_allowed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let limits = Limits::default();
        // A drive-root-style parent holding both backups and the library.
        let root = tmp.path().join("drive");
        make_backup(&root.join("backup"));
        let library = root.join("library");
        fs::create_dir_all(&library).expect("mkdir");
        assert!(matches!(
            classify_added_folder(&root, Some(&library), &limits),
            Ok(AddedFolder::Parent(_))
        ));
        // The library inside a backup.
        let inner_lib = root.join("backup").join("lib");
        fs::create_dir_all(&inner_lib).expect("mkdir");
        assert_eq!(
            classify_added_folder(&root.join("backup"), Some(&inner_lib), &limits).err(),
            Some(RefusalReason::BackupContainsArchiveLibrary)
        );
        let candidate = Candidate::for_dir(&root.join("backup")).expect("candidate");
        assert_eq!(
            library_relation(&candidate, &inner_lib),
            LibraryRelation::LibraryInsideBackup
        );
        assert_eq!(
            library_relation(&candidate, &library),
            LibraryRelation::Separate
        );
        assert_eq!(
            library_relation(&candidate, &root),
            LibraryRelation::BackupInsideLibrary
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_paths_are_refused() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let p = Path::new(OsStr::from_bytes(b"/tmp/\xff"));
        assert_eq!(
            classify_added_folder(p, None, &Limits::default()).err(),
            Some(RefusalReason::NonUnicodePath)
        );
    }
}
