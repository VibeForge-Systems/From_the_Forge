//! Staging: where decrypted and copied files live while a job runs (§10).
//!
//! ```text
//! <library>/.oma-staging/          verified real directory; 0700 on Unix
//!   s<16hex>/                      one per job
//!     .lock                        held with File::try_lock for the session
//!     m/Manifest.db[-wal]
//!     x/<n>                        extract_file outputs
//!     db/<n>/<basename>[-wal]      extract_database outputs
//! ```
//!
//! On Windows these guards cannot make staging private: staging inherits the
//! library's permissions. On Unix, a volume that does not keep POSIX modes
//! (FAT, exFAT or NTFS mounted without a private umask, many network shares)
//! reports group or other bits on the new 0700 directory, and staging is
//! refused there with `LoosePermissions`; it is never used non-privately.

use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::discover::{Candidate, STAGING_DIR_NAME};
use crate::error::{BackupError, IoOp, StagingRefusal};
use crate::guard::is_link;

/// Name of the lock file inside a session.
const LOCK_NAME: &str = ".lock";
/// Sessions younger than this are never swept.
const SWEEP_MIN_AGE: Duration = Duration::from_secs(5 * 60);
/// Backoff between removal attempts (five attempts in all).
const RETRY_BACKOFF_MS: [u64; 4] = [50, 100, 200, 400];
const MAX_ATTEMPTS: u8 = 5;

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The verified `<library>/.oma-staging` directory.
#[derive(Clone)]
pub struct StagingRoot {
    dir: PathBuf,
}

impl std::fmt::Debug for StagingRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StagingRoot(<path>)")
    }
}

fn staging_io(op: IoOp, e: &io::Error) -> BackupError {
    BackupError::staging_io(op, e)
}

fn refused(r: StagingRefusal) -> BackupError {
    BackupError::StagingRefused(r)
}

fn new_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        DirBuilder::new().mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        DirBuilder::new().create(path)
    }
}

/// Create a new file that must not exist yet: mode 0600 on Unix.
pub(crate) fn create_new_file(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// Removes a staged file when dropped, unless disarmed. Every error path of
/// an extraction leaves no plaintext behind (a `.partial`, or a main database
/// whose `-wal` then failed); a successful one disarms it.
pub(crate) struct RemoveOnDrop {
    path: Option<PathBuf>,
}

impl RemoveOnDrop {
    pub(crate) fn new(path: &Path) -> Self {
        RemoveOnDrop {
            path: Some(path.to_path_buf()),
        }
    }

    /// Keep the file.
    pub(crate) fn disarm(mut self) {
        self.path = None;
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(p) = self.path.take() {
            // Best effort; whatever remains goes with the session at close.
            let _ = fs::remove_file(p);
        }
    }
}

/// Guards 1 and 2: the library is a directory, and `.oma-staging` (if it
/// exists) is a real directory at exactly `<canonical library>/.oma-staging`.
/// Returns the canonical library and whether staging exists.
fn verify_staging_location(library: &Path) -> Result<(PathBuf, PathBuf, bool), BackupError> {
    let meta = fs::metadata(library).map_err(|e| staging_io(IoOp::Metadata, &e))?;
    if !meta.is_dir() {
        return Err(refused(StagingRefusal::NotARealDirectory));
    }
    let canonical_library =
        fs::canonicalize(library).map_err(|e| staging_io(IoOp::Canonicalize, &e))?;
    let staging = canonical_library.join(STAGING_DIR_NAME);
    match fs::symlink_metadata(&staging) {
        Ok(m) => {
            if !m.is_dir() || is_link(&m) {
                return Err(refused(StagingRefusal::NotARealDirectory));
            }
            let canonical =
                fs::canonicalize(&staging).map_err(|e| staging_io(IoOp::Canonicalize, &e))?;
            if canonical != staging {
                return Err(refused(StagingRefusal::NotARealDirectory));
            }
            Ok((canonical_library, staging, true))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((canonical_library, staging, false)),
        Err(e) => Err(staging_io(IoOp::Metadata, &e)),
    }
}

/// Guards 4 to 6: staging is not inside this backup, any backup,
/// `MobileSync/Backup`, or iMazing's default `iMazing/Backups` store.
fn check_not_in_backup(staging: &Path, backup: &Candidate) -> Result<(), BackupError> {
    if staging.starts_with(&backup.containment)
        || oma_fs::enclosing_backup_dir(staging).is_some()
        || oma_fs::is_within_mobilesync(staging)
        || oma_fs::is_within_imazing_backups(staging)
    {
        return Err(refused(StagingRefusal::Guard));
    }
    Ok(())
}

/// Guard 3 (Unix): the directory is ours and private.
#[cfg(unix)]
fn check_owner_and_mode(staging: &Path) -> Result<(), BackupError> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(staging).map_err(|e| staging_io(IoOp::Metadata, &e))?;
    if meta.mode() & 0o077 != 0 {
        return Err(refused(StagingRefusal::LoosePermissions));
    }
    let probe = staging.join(format!(".probe-{}", session_suffix()));
    let file = create_new_file(&probe).map_err(|e| staging_io(IoOp::Create, &e))?;
    let probe_uid = file.metadata().map(|m| m.uid());
    drop(file);
    let _ = fs::remove_file(&probe);
    let probe_uid = probe_uid.map_err(|e| staging_io(IoOp::Metadata, &e))?;
    if probe_uid != meta.uid() {
        return Err(refused(StagingRefusal::WrongOwner));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_owner_and_mode(_staging: &Path) -> Result<(), BackupError> {
    Ok(())
}

/// 16 hex characters: SHA-256(pid ‖ nanos ‖ counter)[..8].
fn session_suffix() -> String {
    use std::fmt::Write as _;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let counter = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut h = Sha256::new();
    h.update(std::process::id().to_be_bytes());
    h.update(nanos.to_be_bytes());
    h.update(counter.to_be_bytes());
    let digest = h.finalize();
    let mut out = String::with_capacity(16);
    for b in digest.iter().take(8) {
        let _ = write!(out, "{b:02x}");
    }
    out
}

impl StagingRoot {
    /// Verify (creating if needed) `<library>/.oma-staging` for checking
    /// `backup_root`, running every guard in §10.2.
    pub fn new(library: &Path, backup_root: &Candidate) -> Result<Self, BackupError> {
        let (_, staging, exists) = verify_staging_location(library)?;
        // Check the would-be location before creating anything.
        check_not_in_backup(&staging, backup_root)?;
        if !exists {
            match new_dir(&staging) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(staging_io(IoOp::Create, &e)),
            }
            let (_, again, exists) = verify_staging_location(library)?;
            if !exists || again != staging {
                return Err(refused(StagingRefusal::NotARealDirectory));
            }
        }
        check_owner_and_mode(&staging)?;
        check_not_in_backup(&staging, backup_root)?;
        // Guards compare std canonical forms; files are opened through the
        // simplified form, so SQLite on Windows gets an ordinary path unless
        // the path is too long for one (then dunce keeps `\\?\`).
        Ok(StagingRoot {
            dir: dunce::simplified(&staging).to_path_buf(),
        })
    }

    /// The `.oma-staging` directory.
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Start a session: a fresh `s<16hex>/` with a held `.lock`.
    pub fn begin(&self) -> Result<StagingSession, BackupError> {
        let mut last = None;
        for _ in 0..2 {
            let dir = self.dir.join(format!("s{}", session_suffix()));
            match new_dir(&dir) {
                Ok(()) => return lock_session(dir),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = Some(e),
                Err(e) => return Err(staging_io(IoOp::Create, &e)),
            }
        }
        let e = last.unwrap_or_else(|| io::Error::from(io::ErrorKind::AlreadyExists));
        Err(staging_io(IoOp::Create, &e))
    }
}

fn lock_session(dir: PathBuf) -> Result<StagingSession, BackupError> {
    let lock_path = dir.join(LOCK_NAME);
    let created = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&lock_path);
    let lock = match created {
        Ok(f) => f,
        Err(e) => {
            let _ = fs::remove_dir(&dir);
            return Err(staging_io(IoOp::Create, &e));
        }
    };
    match lock.try_lock() {
        Ok(()) => Ok(StagingSession {
            dir,
            lock: Some(lock),
            next: AtomicU64::new(0),
            closed: false,
        }),
        Err(err) => {
            drop(lock);
            let _ = fs::remove_file(&lock_path);
            let _ = fs::remove_dir(&dir);
            match err {
                TryLockError::WouldBlock => {
                    Err(BackupError::Internal("a new lock file is already locked"))
                }
                TryLockError::Error(_) => Err(refused(StagingRefusal::LockUnsupported)),
            }
        }
    }
}

/// One job's staging directory, holding its lock until closed.
pub struct StagingSession {
    dir: PathBuf,
    lock: Option<File>,
    next: AtomicU64,
    closed: bool,
}

impl std::fmt::Debug for StagingSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StagingSession")
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

/// The outcome of removing a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupReport {
    /// The session directory is gone.
    pub removed: bool,
    /// Removal attempts made (1 to 5).
    pub attempts: u8,
    /// The kind of the last error, when not removed.
    pub last_error: Option<io::ErrorKind>,
}

impl StagingSession {
    /// The session directory.
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// A per-session sequence number, for file names that carry no backup
    /// data.
    pub(crate) fn next_index(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Create (if needed) a subdirectory from fixed, crate-chosen components.
    pub(crate) fn subdir(&self, components: &[&str]) -> Result<PathBuf, BackupError> {
        let mut path = self.dir.clone();
        for c in components {
            path.push(c);
            match new_dir(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let m =
                        fs::symlink_metadata(&path).map_err(|e| staging_io(IoOp::Metadata, &e))?;
                    if !m.is_dir() || is_link(&m) {
                        return Err(refused(StagingRefusal::NotARealDirectory));
                    }
                }
                Err(e) => return Err(staging_io(IoOp::Create, &e)),
            }
        }
        Ok(path)
    }

    /// Delete plaintext first, release the lock, then remove the directory,
    /// with a bounded retry.
    pub fn close(mut self) -> CleanupReport {
        self.close_inner()
    }

    fn close_inner(&mut self) -> CleanupReport {
        self.closed = true;
        let report = remove_session(&self.dir, self.lock.take());
        log_cleanup(&report);
        report
    }
}

impl Drop for StagingSession {
    fn drop(&mut self) {
        if !self.closed {
            let report = self.close_inner();
            tracing::warn!(
                removed = report.removed,
                attempts = report.attempts,
                "staging session dropped without close"
            );
        }
    }
}

fn log_cleanup(report: &CleanupReport) {
    if report.removed {
        tracing::info!(attempts = report.attempts, "staging removed");
    } else {
        tracing::warn!(
            attempts = report.attempts,
            error = ?report.last_error,
            "staging not removed"
        );
    }
}

fn retryable(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(32 | 33))
}

/// Remove a session directory: plaintext subdirectories first, then other
/// entries, then the lock (unlocked and closed first), then the directory.
fn remove_session(dir: &Path, mut lock: Option<File>) -> CleanupReport {
    let mut attempts: u8 = 0;
    loop {
        attempts = attempts.saturating_add(1);
        let result = (|| -> io::Result<()> {
            for sub in ["m", "x", "db"] {
                remove_tree_no_follow(&dir.join(sub))?;
            }
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries {
                    let entry = entry?;
                    if entry.file_name() != LOCK_NAME {
                        remove_tree_no_follow(&entry.path())?;
                    }
                }
            }
            if let Some(file) = lock.take() {
                let _ = file.unlock();
                drop(file);
            }
            match fs::remove_file(dir.join(LOCK_NAME)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            match fs::remove_dir(dir) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            }
        })();
        match result {
            Ok(()) => {
                return CleanupReport {
                    removed: true,
                    attempts,
                    last_error: None,
                }
            }
            Err(e) => {
                let index = usize::from(attempts.saturating_sub(1));
                match RETRY_BACKOFF_MS.get(index) {
                    Some(ms) if attempts < MAX_ATTEMPTS && retryable(&e) => {
                        std::thread::sleep(Duration::from_millis(*ms));
                    }
                    _ => {
                        return CleanupReport {
                            removed: false,
                            attempts,
                            last_error: Some(e.kind()),
                        }
                    }
                }
            }
        }
    }
}

/// Remove `path` without following links: a link is unlinked, never
/// traversed. A missing path is not an error.
fn remove_tree_no_follow(path: &Path) -> io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if meta.is_dir() && !is_link(&meta) {
        for entry in fs::read_dir(path)? {
            remove_tree_no_follow(&entry?.path())?;
        }
        fs::remove_dir(path)
    } else {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            // A directory link or junction on Windows is removed as a directory.
            Err(e) if is_link(&meta) => fs::remove_dir(path).map_err(|_| e),
            Err(e) => Err(e),
        }
    }
}

/// Counts from [`sweep_stale`]. Only these counts are logged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Stale sessions removed.
    pub removed: u32,
    /// Sessions whose lock is held (or could not be tested).
    pub in_use: u32,
    /// Entries left alone: foreign names, links, young or lock-less sessions.
    pub skipped: u32,
    /// Removals that failed, or a staging directory that failed its guards.
    pub failed: u32,
}

fn is_session_name(name: &str) -> bool {
    name.len() == 17
        && name.starts_with('s')
        && name
            .bytes()
            .skip(1)
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_old(meta: &fs::Metadata) -> bool {
    meta.modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .is_some_and(|age| age >= SWEEP_MIN_AGE)
}

/// Remove sessions left behind by a crash or a failed cleanup (§10.4).
/// Touches nothing unless the staging directory passes guards 1 and 2, only
/// ever considers `s<16hex>` real directories, and never creates `.lock`.
pub fn sweep_stale(library: &Path) -> SweepReport {
    let mut report = SweepReport::default();
    let staging = match verify_staging_location(library) {
        Ok((_, staging, true)) => staging,
        Ok((_, _, false)) => return report,
        Err(_) => {
            report.failed = 1;
            tracing::warn!("staging sweep refused: guard failed");
            return report;
        }
    };
    let Ok(entries) = fs::read_dir(&staging) else {
        report.failed = 1;
        return report;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            report.skipped = report.skipped.saturating_add(1);
            continue;
        };
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) if is_session_name(name) && m.is_dir() && !is_link(&m) => m,
            _ => {
                report.skipped = report.skipped.saturating_add(1);
                continue;
            }
        };
        if !is_old(&meta) {
            report.skipped = report.skipped.saturating_add(1);
            continue;
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.join(LOCK_NAME));
        let lock = match lock {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // A crash between mkdir and lock leaves an empty directory;
                // remove_dir only succeeds on an empty one.
                if fs::remove_dir(&path).is_ok() {
                    report.removed = report.removed.saturating_add(1);
                } else {
                    report.skipped = report.skipped.saturating_add(1);
                }
                continue;
            }
            Err(_) => {
                report.in_use = report.in_use.saturating_add(1);
                continue;
            }
        };
        match lock.try_lock() {
            Ok(()) => {
                let r = remove_session(&path, Some(lock));
                if r.removed {
                    report.removed = report.removed.saturating_add(1);
                } else {
                    report.failed = report.failed.saturating_add(1);
                }
            }
            Err(_) => report.in_use = report.in_use.saturating_add(1),
        }
    }
    tracing::info!(
        removed = report.removed,
        in_use = report.in_use,
        skipped = report.skipped,
        failed = report.failed,
        "staging sweep"
    );
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Setup {
        _tmp: tempfile::TempDir,
        library: PathBuf,
        backup: Candidate,
    }

    fn setup() -> Setup {
        let tmp = tempfile::tempdir().expect("tempdir");
        let library = tmp.path().join("library");
        fs::create_dir(&library).expect("mkdir");
        let backup_dir = tmp.path().join("backup");
        fs::create_dir(&backup_dir).expect("mkdir");
        for m in [
            "Info.plist",
            "Manifest.plist",
            "Status.plist",
            "Manifest.db",
        ] {
            fs::write(backup_dir.join(m), b"x").expect("marker");
        }
        let backup = Candidate::for_dir(&backup_dir).expect("candidate");
        Setup {
            _tmp: tmp,
            library,
            backup,
        }
    }

    /// Make a directory look ten minutes old.
    fn age(path: &Path) {
        let old = SystemTime::now() - Duration::from_secs(10 * 60);
        #[cfg(windows)]
        let f = {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_WRITE_ATTRIBUTES. A directory handle needs
            // FILE_FLAG_BACKUP_SEMANTICS, which std does not set by default.
            OpenOptions::new()
                .access_mode(0x0100)
                .custom_flags(0x0200_0000)
                .open(path)
        };
        #[cfg(not(windows))]
        let f = File::open(path);
        f.expect("open dir")
            .set_modified(old)
            .expect("set_modified");
    }

    #[test]
    fn begin_and_close_remove_everything() {
        let s = setup();
        let root = StagingRoot::new(&s.library, &s.backup).expect("root");
        let session = root.begin().expect("begin");
        let dir = session.path().to_path_buf();
        let name = dir.file_name().and_then(|n| n.to_str()).expect("name");
        assert!(is_session_name(name), "{name}");
        assert!(dir.join(LOCK_NAME).is_file());
        let m = session.subdir(&["m"]).expect("m");
        fs::write(m.join("Manifest.db"), b"plaintext").expect("write");
        let x = session.subdir(&["db", "0"]).expect("db");
        fs::write(x.join("sms.db"), b"plaintext").expect("write");
        let report = session.close();
        assert!(report.removed);
        assert_eq!(report.attempts, 1);
        assert!(!dir.exists());
        assert!(root.path().is_dir(), "the staging root itself stays");
    }

    #[test]
    fn drop_is_a_fallback_close() {
        let s = setup();
        let root = StagingRoot::new(&s.library, &s.backup).expect("root");
        let dir = {
            let session = root.begin().expect("begin");
            session.path().to_path_buf()
        };
        assert!(!dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn modes_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let s = setup();
        let root = StagingRoot::new(&s.library, &s.backup).expect("root");
        let mode = |p: &Path| fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(root.path()), 0o700);
        let session = root.begin().expect("begin");
        assert_eq!(mode(session.path()), 0o700);
        let m = session.subdir(&["m"]).expect("m");
        assert_eq!(mode(&m), 0o700);
        let f = m.join("f");
        drop(create_new_file(&f).expect("create"));
        assert_eq!(mode(&f), 0o600);
        session.close();
    }

    #[cfg(unix)]
    #[test]
    fn a_loose_or_linked_staging_dir_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let s = setup();
        let staging = s.library.join(STAGING_DIR_NAME);
        fs::create_dir(&staging).expect("mkdir");
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(matches!(
            StagingRoot::new(&s.library, &s.backup),
            Err(BackupError::StagingRefused(
                StagingRefusal::LoosePermissions
            ))
        ));
        fs::remove_dir(&staging).expect("rmdir");

        let elsewhere = s.library.parent().expect("parent").join("elsewhere");
        fs::create_dir(&elsewhere).expect("mkdir");
        fs::write(elsewhere.join("keep"), b"x").expect("write");
        std::os::unix::fs::symlink(&elsewhere, &staging).expect("symlink");
        assert!(matches!(
            StagingRoot::new(&s.library, &s.backup),
            Err(BackupError::StagingRefused(
                StagingRefusal::NotARealDirectory
            ))
        ));
        let report = sweep_stale(&s.library);
        assert_eq!(report.failed, 1);
        assert!(
            elsewhere.join("keep").exists(),
            "the sweep left the link alone"
        );
    }

    #[test]
    fn guards_refuse_libraries_inside_backups_mobilesync_and_imazing() {
        let s = setup();
        // A library inside the backup being checked.
        let inner = s.backup.containment.join("lib");
        fs::create_dir(&inner).expect("mkdir");
        assert!(matches!(
            StagingRoot::new(&inner, &s.backup),
            Err(BackupError::StagingRefused(StagingRefusal::Guard))
        ));
        assert!(
            !inner.join(STAGING_DIR_NAME).exists(),
            "nothing was created"
        );
        // A library inside MobileSync/Backup.
        let ms = s.library.join("MobileSync").join("Backup").join("lib");
        fs::create_dir_all(&ms).expect("mkdir");
        assert!(matches!(
            StagingRoot::new(&ms, &s.backup),
            Err(BackupError::StagingRefused(StagingRefusal::Guard))
        ));
        // A library inside iMazing's default store (any case).
        let im = s.library.join("imazing").join("BACKUPS").join("lib");
        fs::create_dir_all(&im).expect("mkdir");
        assert!(matches!(
            StagingRoot::new(&im, &s.backup),
            Err(BackupError::StagingRefused(StagingRefusal::Guard))
        ));
        assert!(!im.join(STAGING_DIR_NAME).exists(), "nothing was created");
    }

    #[test]
    fn sweep_rules() {
        let s = setup();
        let root = StagingRoot::new(&s.library, &s.backup).expect("root");
        let staging = root.path().to_path_buf();

        // An unlocked old session: removed.
        let stale = staging.join("s0000000000000001");
        fs::create_dir(&stale).expect("mkdir");
        fs::write(stale.join(LOCK_NAME), b"").expect("lock");
        fs::create_dir(stale.join("m")).expect("mkdir");
        fs::write(stale.join("m").join("Manifest.db"), b"plaintext").expect("w");
        age(&stale);

        // A locked old session: kept.
        let held = root.begin().expect("begin");
        age(held.path());

        // A young unlocked session: kept.
        let young = staging.join("s0000000000000002");
        fs::create_dir(&young).expect("mkdir");
        fs::write(young.join(LOCK_NAME), b"").expect("lock");

        // A non-empty stray without .lock: kept, and no .lock is created.
        let stray = staging.join("s0000000000000003");
        fs::create_dir(&stray).expect("mkdir");
        fs::write(stray.join("x"), b"x").expect("w");
        age(&stray);

        // An empty old directory without .lock: removed.
        let empty = staging.join("s0000000000000004");
        fs::create_dir(&empty).expect("mkdir");
        age(&empty);

        // A foreign name: untouched.
        let foreign = staging.join("not-ours");
        fs::create_dir(&foreign).expect("mkdir");
        age(&foreign);

        let report = sweep_stale(&s.library);
        assert!(!stale.exists());
        assert!(held.path().exists());
        assert!(young.exists());
        assert!(stray.exists());
        assert!(
            !stray.join(LOCK_NAME).exists(),
            "the sweep never creates .lock"
        );
        assert!(!empty.exists());
        assert!(foreign.exists());
        assert_eq!(report.removed, 2);
        assert_eq!(report.in_use, 1);
        assert_eq!(report.skipped, 3);
        assert_eq!(report.failed, 0);
        held.close();
    }

    #[cfg(unix)]
    #[test]
    fn a_link_inside_a_session_is_unlinked_not_followed() {
        let s = setup();
        let root = StagingRoot::new(&s.library, &s.backup).expect("root");
        let outside = s.library.parent().expect("parent").join("outside");
        fs::create_dir(&outside).expect("mkdir");
        fs::write(outside.join("precious"), b"keep me").expect("w");
        let stale = root.path().join("s00000000000000aa");
        fs::create_dir(&stale).expect("mkdir");
        fs::write(stale.join(LOCK_NAME), b"").expect("lock");
        std::os::unix::fs::symlink(&outside, stale.join("x")).expect("symlink");
        age(&stale);
        let report = sweep_stale(&s.library);
        assert_eq!(report.removed, 1);
        assert!(!stale.exists());
        assert!(outside.join("precious").exists());
    }

    #[test]
    fn a_missing_staging_root_is_not_created_by_the_sweep() {
        let s = setup();
        assert_eq!(sweep_stale(&s.library), SweepReport::default());
        assert!(!s.library.join(STAGING_DIR_NAME).exists());
    }
}
