//! Staging, publish and leftovers (M6 spec §8.3–§8.6, D126, D140, D143,
//! D144).
//!
//! **A bundle** is written into `<parent>/.oma-export-<32 hex>.partial/`,
//! which holds a marker file (`.oma-export-partial`, fixed ASCII content)
//! locked with `File::try_lock` for the whole job. Provenance comes last
//! (`manifest.rs`), then one no-replace rename publishes the folder under
//! its name (` (2)` … ` (99)` when taken). Only after that rename is the
//! marker removed and its lock released, so a crash at any earlier moment
//! leaves a marked `.partial` folder that [`find_leftovers`] finds.
//!
//! **A single file** is written to `<parent>/.oma-export-<32 hex>.partial-file`
//! (locked while open), then placed under its name without replacing
//! anything, or, only after the app's own confirmation, over an existing
//! regular file (§8.4).
//!
//! **Linux** (D143): the canonical parent is opened `O_DIRECTORY |
//! O_NOFOLLOW` and its `(dev, ino)` recorded; the staging folder is made
//! with `mkdirat` (0700) and opened the same way. Every later create
//! (`openat` with `O_CREAT | O_EXCL | O_NOFOLLOW`, 0600), `mkdirat`,
//! rename and removal is relative to those handles, so renaming the
//! staging folder or planting a link in the parent cannot redirect a
//! write. Before the publish rename, the parent must still canonicalise to
//! the same path and `(dev, ino)`, and the name must still be the staging
//! folder the export made; otherwise `DestinationChanged`.
//!
//! **Elsewhere (Windows)**: std has no handle-relative API without
//! `unsafe`, so staging works on paths below the canonical parent, with
//! the same names, `create_new` everywhere and the same checks by name and
//! type (a reparse point is refused). The residual is disclosed: between
//! those checks and each create or the rename, a junction swapped in by
//! another account could redirect a write (spec §11.1). Files take the
//! destination folder's ACL (D140). Renames and removals retry with bounded
//! backoff on sharing violations (§8.6); if the publish rename still fails
//! while the marker is held, the marker's handle is closed and the rename
//! tried once more (the lock is then not held for that last moment; the
//! sweep's 5-minute rule still protects the folder). Whether an open
//! handle under the folder blocks the rename at all is **UNVERIFIED**.
//!
//! **The age rule tracks liveness.** Where the marker's lock cannot be
//! taken (a file system without locks) or is not seen by other machines
//! (`flock` on some network mounts is local to each client), the sweep's
//! 5-minute rule is the only protection, and it reads the folder's and the
//! marker's modification times. A bundle's files land in subfolders, so
//! neither time would move while the job runs: [`Staging::sync_batch`]
//! therefore sets the marker's modification time to now once per batch
//! (best effort). A live export's marker is then never older than its
//! longest batch; a single batch that takes over 5 minutes (one very large
//! copy to a slow network folder) is the residual, and is disclosed.
//!
//! **Open handles are bounded.** Closed files keep their handle until
//! their `sync_all`; at [`MAX_UNSYNCED`] closed files the batch is flushed
//! early, and on Linux at most [`MAX_OPEN_SUBDIRS`] subfolder handles stay
//! open (older ones are flushed, closed and reopened through the staging
//! handle if needed), so a photo-heavy batch stays far below a 1,024
//! descriptor limit.
//!
//! **Attachment copies** (slice 6e2) are created with
//! [`Staging::create_verified`] and closed with
//! [`Staging::close_verified`]: their digest is the one the archive's
//! reader verified, so they are not hashed a second time. A failed copy is
//! [`Staging::discard`]ed. [`Staging::write_mark_of_the_web`] adds the
//! `Zone.Identifier` stream on Windows.
//!
//! Nothing here writes outside `<parent>` or uses a temporary directory.
//! Errors carry kinds only, and `Debug` prints no path or name.

use std::collections::BTreeSet;
use std::fs::{self, File, TryLockError};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use crate::error::{ExportError, LimitKind};
use crate::fault::{self, FaultPoint};
use crate::naming::{self, BundlePath, EntryKind, EntryName, TargetName, MARKER};
use crate::plan::Packaging;

/// Leftovers younger than this are never swept (as `oma-backup`'s
/// staging): another instance may still be writing them.
pub const SWEEP_MIN_AGE: Duration = Duration::from_secs(5 * 60);

/// The marker's content: fixed ASCII, no content of the export.
pub const MARKER_CONTENT: &[u8] =
    b"OpenMessage Archive: an unfinished export. It is safe to delete when no export is running.\n";

/// The buffer of each staged file.
const WRITE_BUFFER: usize = 64 * 1024;

/// Closed files whose `sync_all` may wait for the batch's end; one more
/// flushes them all early (each holds a descriptor until then).
pub const MAX_UNSYNCED: usize = 64;

/// The most subfolder handles staging keeps open on Linux.
pub const MAX_OPEN_SUBDIRS: usize = 32;

/// A Mark of the Web's content (owner Q3c): the Internet zone.
pub const MARK_OF_THE_WEB: &[u8] = b"[ZoneTransfer]\r\nZoneId=3\r\n";

/// The bounded backoff of §8.6: 100 ms doubling to 2 s, at most 10 s in
/// all (these sum to 9.1 s).
pub const RETRY_BACKOFF_MS: [u64; 8] = [100, 200, 400, 800, 1_600, 2_000, 2_000, 2_000];

fn io_err(e: io::Error) -> ExportError {
    ExportError::io(&e)
}

fn limit(k: LimitKind) -> ExportError {
    ExportError::Limit(k)
}

/// `true` for the errors §8.6 retries: on Windows, `ERROR_ACCESS_DENIED`
/// (5) and `ERROR_SHARING_VIOLATION` (32), which an antivirus scan or the
/// Search indexer holding a handle can cause. Nothing is retried elsewhere.
pub fn is_transient(e: &io::Error) -> bool {
    #[cfg(windows)]
    {
        matches!(e.raw_os_error(), Some(5 | 32))
    }
    #[cfg(not(windows))]
    {
        let _ = e;
        false
    }
}

/// Run `op`, retrying errors `transient` accepts after each delay of
/// [`RETRY_BACKOFF_MS`], through `sleep`.
pub fn retry_with<T>(
    sleep: &mut dyn FnMut(Duration),
    transient: &dyn Fn(&io::Error) -> bool,
    mut op: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    let mut delays = RETRY_BACKOFF_MS.iter();
    loop {
        match op() {
            Err(e) if transient(&e) => match delays.next() {
                Some(ms) => sleep(Duration::from_millis(*ms)),
                None => return Err(e),
            },
            other => return other,
        }
    }
}

fn with_retries<T>(op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    retry_with(&mut std::thread::sleep, &is_transient, op)
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// A file written and closed inside staging: its place, length and SHA-256,
/// hashed while it was written (never read back).
#[derive(Clone, PartialEq, Eq)]
pub struct FileEntry {
    path: BundlePath,
    bytes: u64,
    sha256: [u8; 32],
}

impl std::fmt::Debug for FileEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileEntry")
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

impl FileEntry {
    pub fn path(&self) -> &BundlePath {
        &self.path
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn sha256(&self) -> [u8; 32] {
        self.sha256
    }

    pub fn sha256_hex(&self) -> String {
        naming::hex(&self.sha256)
    }
}

/// A file being written inside staging. Every byte written is hashed and
/// counted as it is accepted. Hand it back with [`Staging::close`].
pub struct StagedFile {
    path: BundlePath,
    out: BufWriter<File>,
    /// `None` for a file whose content another reader already verified
    /// (an attachment copy, [`Staging::create_verified`]).
    hasher: Option<Sha256>,
    bytes: u64,
    token: u64,
}

impl std::fmt::Debug for StagedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StagedFile")
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

impl StagedFile {
    /// Bytes accepted so far.
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }

    pub fn path(&self) -> &BundlePath {
        &self.path
    }
}

impl Write for StagedFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.out.write(buf)?;
        if let Some(h) = &mut self.hasher {
            h.update(buf.get(..n).unwrap_or(buf));
        }
        self.bytes = self.bytes.saturating_add(n as u64);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

// ---------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------

/// How a publish may treat an existing target (§8.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Replace {
    /// Never replace anything (every bundle, and a single file without the
    /// app's confirmation).
    Never,
    /// The user confirmed "Replace '<name>'?" in the app: a single file may
    /// replace an existing **regular file** (never a folder or a link).
    ConfirmedRegularFile,
}

/// Where a publish placed the export.
#[derive(Clone, PartialEq, Eq)]
pub struct Published {
    /// The name it was published under (the user's choice; never log it).
    pub name: TargetName,
    /// The ` (n)` suffix used, 1 for none.
    pub candidate: u32,
    /// `false` when the marker could not be removed from the published
    /// bundle (harmless: it is outside `files` and `SHA256SUMS`).
    pub marker_removed: bool,
    /// `true` when a single file replaced an existing one.
    pub replaced: bool,
    /// `false` when the parent folder could not be flushed after the
    /// rename (the export is published either way: the rename is the
    /// point of no return, §8.5).
    pub parent_synced: bool,
}

impl std::fmt::Debug for Published {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Published")
            .field("candidate", &self.candidate)
            .field("marker_removed", &self.marker_removed)
            .field("replaced", &self.replaced)
            .field("parent_synced", &self.parent_synced)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Live,
    Published,
    Removed,
}

/// One export's staging inside its destination.
pub struct Staging {
    packaging: Packaging,
    parent: PathBuf,
    entry: EntryName,
    sys: sys::Handles,
    marker: Option<File>,
    marker_locked: bool,
    files: Vec<FileEntry>,
    /// Every file created, closed or not (removal walks this list).
    created: Vec<BundlePath>,
    keys: BTreeSet<String>,
    open: BTreeSet<u64>,
    next_token: u64,
    unsynced: Vec<File>,
    state: State,
}

impl std::fmt::Debug for Staging {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Staging")
            .field("packaging", &self.packaging)
            .field("entry", &self.entry)
            .field("files", &self.files.len())
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// The parent must be absolute, canonical (as `dunce` makes it) and a real
/// directory: the checks of §8.2 ran on that form, and staging runs on it.
fn check_canonical(parent: &Path) -> Result<(), ExportError> {
    let canonical = dunce::canonicalize(parent).map_err(|_| ExportError::DestinationChanged)?;
    if canonical != parent || !parent.is_absolute() {
        return Err(ExportError::DestinationChanged);
    }
    let meta = fs::symlink_metadata(parent).map_err(|_| ExportError::DestinationChanged)?;
    if !meta.is_dir() || sys::is_link(&meta) {
        return Err(ExportError::DestinationChanged);
    }
    Ok(())
}

impl Staging {
    /// Start a bundle in `parent` (canonical, §8.2): the 0700 staging
    /// folder, then its marker, locked.
    pub fn bundle(parent: &Path) -> Result<Staging, ExportError> {
        check_canonical(parent)?;
        let entry = EntryName::random(EntryKind::PartialDir)?;
        let mut sys =
            sys::Handles::open_parent(parent).map_err(|_| ExportError::DestinationChanged)?;
        sys.make_staging_dir(parent, &entry.to_name())
            .map_err(io_err)?;
        let mut staging = Staging::new(Packaging::Bundle, parent, entry, sys);
        fault::point(FaultPoint::StagingCreated);
        let mut marker = staging.sys.create_marker().map_err(io_err)?;
        marker.write_all(MARKER_CONTENT).map_err(io_err)?;
        marker.sync_all().map_err(io_err)?;
        staging.marker_locked = match marker.try_lock() {
            Ok(()) => true,
            // A new file another handle has locked: never ours to share.
            Err(TryLockError::WouldBlock) => return Err(limit(LimitKind::FileStillOpen)),
            // The file system has no locks (some network mounts): the
            // sweep's age rule is then the only protection.
            Err(TryLockError::Error(_)) => false,
        };
        staging.marker = Some(marker);
        staging.sys.sync_staging().map_err(io_err)?;
        Ok(staging)
    }

    /// Start a single file in `parent` (canonical): nothing is created
    /// until the writer asks for its one output.
    pub fn single_file(parent: &Path) -> Result<Staging, ExportError> {
        check_canonical(parent)?;
        let entry = EntryName::random(EntryKind::PartialFile)?;
        let sys = sys::Handles::open_parent(parent).map_err(|_| ExportError::DestinationChanged)?;
        Ok(Staging::new(Packaging::SingleFile, parent, entry, sys))
    }

    fn new(packaging: Packaging, parent: &Path, entry: EntryName, sys: sys::Handles) -> Staging {
        Staging {
            packaging,
            parent: parent.to_path_buf(),
            entry,
            sys,
            marker: None,
            marker_locked: false,
            files: Vec::new(),
            created: Vec::new(),
            keys: BTreeSet::new(),
            open: BTreeSet::new(),
            next_token: 0,
            unsynced: Vec::new(),
            state: State::Live,
        }
    }

    pub fn packaging(&self) -> Packaging {
        self.packaging
    }

    /// The staging entry's own name (content-free; what a failed removal
    /// names, §8.5).
    pub fn entry(&self) -> EntryName {
        self.entry
    }

    /// Whether the marker's lock is held (`false` on a file system without
    /// locks).
    pub fn marker_locked(&self) -> bool {
        self.marker_locked
    }

    /// The files closed so far, in the order they were closed.
    pub fn files(&self) -> &[FileEntry] {
        &self.files
    }

    /// Files created and not yet closed.
    pub fn open_files(&self) -> usize {
        self.open.len()
    }

    /// Handles staging holds besides the open files, the folders and the
    /// marker: closed files awaiting their `sync_all` (at most
    /// [`MAX_UNSYNCED`] − 1) and, on Linux, subfolder handles (at most
    /// [`MAX_OPEN_SUBDIRS`] plus the current folder's parents).
    pub fn held_handles(&self) -> usize {
        self.unsynced.len() + self.sys.open_subdirs()
    }

    fn live(&self) -> Result<(), ExportError> {
        if self.state == State::Live {
            Ok(())
        } else {
            Err(limit(LimitKind::RowOrder))
        }
    }

    /// Create a new file at `path` inside staging (0600 on Linux; never
    /// replacing anything). In single-file mode the first and only output
    /// is the `.partial-file`, whatever its bundle path.
    pub fn create(&mut self, path: &BundlePath) -> Result<StagedFile, ExportError> {
        self.create_with(path, true)
    }

    /// Like [`Staging::create`], for a file whose bytes come from a reader
    /// that verifies them itself (an attachment's `BlobReader`): nothing is
    /// hashed while it is written, and [`Staging::close_verified`] records
    /// the reader's digest (spec §6.1, "one hash"). Bundles only.
    pub fn create_verified(&mut self, path: &BundlePath) -> Result<StagedFile, ExportError> {
        if self.packaging != Packaging::Bundle {
            return Err(limit(LimitKind::RowOrder));
        }
        self.create_with(path, false)
    }

    fn create_with(&mut self, path: &BundlePath, hash: bool) -> Result<StagedFile, ExportError> {
        self.live()?;
        let file = match self.packaging {
            Packaging::Bundle => {
                if !self.keys.insert(naming::collision_key(path)) {
                    return Err(naming::collision());
                }
                self.sys.create_file(path).map_err(io_err)?
            }
            Packaging::SingleFile => {
                if !self.created.is_empty() {
                    return Err(limit(LimitKind::RowOrder));
                }
                let f = self
                    .sys
                    .create_partial_file(&self.parent, &self.entry.to_name())
                    .map_err(io_err)?;
                // Locked while it is open, so another instance's sweep
                // skips it; on a file system without locks the age rule
                // protects it.
                if let Err(TryLockError::WouldBlock) = f.try_lock() {
                    return Err(limit(LimitKind::FileStillOpen));
                }
                f
            }
        };
        if self.created.is_empty() && self.packaging == Packaging::Bundle {
            fault::point(FaultPoint::FirstFileStaged);
        }
        self.created.push(path.clone());
        let token = self.next_token;
        self.next_token += 1;
        self.open.insert(token);
        Ok(StagedFile {
            path: path.clone(),
            out: BufWriter::with_capacity(WRITE_BUFFER, file),
            hasher: hash.then(Sha256::new),
            bytes: 0,
            token,
        })
    }

    /// Close `f`: flush it and record its length and hash. Its `sync_all`
    /// follows in the next [`Staging::sync_batch`].
    pub fn close(&mut self, f: StagedFile) -> Result<FileEntry, ExportError> {
        if f.hasher.is_none() {
            return Err(limit(LimitKind::RowOrder));
        }
        self.close_with(f, None)
    }

    /// Close a file made by [`Staging::create_verified`], recording
    /// `sha256`, the digest its reader verified over exactly
    /// `expected_bytes` bytes. A file of another length is refused
    /// (`RowCountMismatch`) and stays listed for removal.
    pub fn close_verified(
        &mut self,
        f: StagedFile,
        sha256: [u8; 32],
        expected_bytes: u64,
    ) -> Result<FileEntry, ExportError> {
        if f.hasher.is_some() {
            return Err(limit(LimitKind::RowOrder));
        }
        if f.bytes != expected_bytes {
            self.open.remove(&f.token);
            return Err(limit(LimitKind::RowCountMismatch));
        }
        self.close_with(f, Some(sha256))
    }

    fn close_with(
        &mut self,
        f: StagedFile,
        verified: Option<[u8; 32]>,
    ) -> Result<FileEntry, ExportError> {
        if !self.open.remove(&f.token) {
            return Err(limit(LimitKind::RowOrder));
        }
        let StagedFile {
            path,
            out,
            hasher,
            bytes,
            ..
        } = f;
        let file = out.into_inner().map_err(|e| io_err(e.into_error()))?;
        let sha256 = match (verified, hasher) {
            (Some(d), _) => d,
            (None, Some(h)) => h.finalize().into(),
            (None, None) => return Err(limit(LimitKind::RowOrder)),
        };
        let entry = FileEntry {
            path,
            bytes,
            sha256,
        };
        self.files.push(entry.clone());
        self.unsynced.push(file);
        if self.unsynced.len() >= MAX_UNSYNCED {
            self.sync_batch()?;
        }
        Ok(entry)
    }

    /// Drop `f` unfinished and remove it (an attachment whose copy failed
    /// its checksum or could not be written, spec §6.1). Its name stays
    /// used. `Ok(false)` when the removal failed: the file is then still
    /// in the staging folder (never in [`Staging::files`]), and the caller
    /// must fail the export rather than publish a bundle that holds it;
    /// [`Staging::remove`] retries it with the folder.
    pub fn discard(&mut self, f: StagedFile) -> Result<bool, ExportError> {
        self.live()?;
        if !self.open.remove(&f.token) || self.packaging != Packaging::Bundle {
            return Err(limit(LimitKind::RowOrder));
        }
        let path = f.path.clone();
        drop(f);
        let removed = with_retries(|| {
            self.sys
                .remove_file(&self.parent, &self.entry.to_name(), &path)
        })
        .is_ok();
        if removed {
            if let Some(i) = self.created.iter().rposition(|p| *p == path) {
                self.created.remove(i);
            }
        }
        Ok(removed)
    }

    /// Write a Mark of the Web beside the closed file `e` (owner Q3c,
    /// spec §6.4): the NTFS stream `<file>:Zone.Identifier` holding
    /// `[ZoneTransfer]` and `ZoneId=3` (the Internet zone), created new
    /// and private. Windows only; elsewhere nothing is written and
    /// `Ok(false)` is returned. An error means it was not written (on FAT
    /// or exFAT, for example): the caller counts it (`motwNotWritten`) and
    /// carries on. What Windows does with the mark (SmartScreen, Office
    /// Protected View) is **UNVERIFIED**.
    ///
    /// `e` must be the file closed last (the copy just made): a constant
    /// check, never a scan of every file.
    pub fn write_mark_of_the_web(&mut self, e: &FileEntry) -> Result<bool, ExportError> {
        self.live()?;
        if self.packaging != Packaging::Bundle || self.files.last() != Some(e) {
            return Err(limit(LimitKind::RowOrder));
        }
        #[cfg(windows)]
        {
            let mut f = self.sys.create_zone_identifier(e.path()).map_err(io_err)?;
            f.write_all(MARK_OF_THE_WEB).map_err(io_err)?;
            self.unsynced.push(f);
            if self.unsynced.len() >= MAX_UNSYNCED {
                self.sync_batch()?;
            }
            Ok(true)
        }
        #[cfg(not(windows))]
        {
            Ok(false)
        }
    }

    /// `sync_all` every file closed since the last call, then close their
    /// handles (§8.3 step 3: one pass per batch). A bundle's marker gets
    /// the current time as its modification time (best effort), so the
    /// sweep's age rule sees a running export as young (module docs).
    pub fn sync_batch(&mut self) -> Result<(), ExportError> {
        for f in self.unsynced.drain(..) {
            f.sync_all().map_err(io_err)?;
        }
        if let Some(m) = &self.marker {
            let _ = m.set_modified(SystemTime::now());
        }
        Ok(())
    }

    /// Flush the staging folder and its subfolders (before publish).
    pub(crate) fn sync_dirs(&mut self) -> Result<(), ExportError> {
        self.sys.sync_subdirs().map_err(io_err)?;
        self.sys.sync_staging().map_err(io_err)
    }

    fn ready_to_publish(&mut self) -> Result<(), ExportError> {
        self.live()?;
        if !self.open.is_empty() {
            return Err(limit(LimitKind::FileStillOpen));
        }
        self.sync_batch()?;
        Ok(())
    }

    /// Check that the destination is still the one the export started in
    /// (D143): the canonical parent unchanged, and the staging entry still
    /// the export's own.
    fn check_destination(&self) -> Result<(), ExportError> {
        check_canonical(&self.parent)?;
        let same = self
            .sys
            .same_destination(&self.parent, &self.entry.to_name())
            .unwrap_or(false);
        if same {
            Ok(())
        } else {
            Err(ExportError::DestinationChanged)
        }
    }

    /// Publish (§8.3 steps 5–6, §8.4). A bundle is renamed, never replacing
    /// anything, under `name`, then ` (2)` … ` (99)`; a single file is
    /// placed under `name` exactly, replacing only a regular file and only
    /// with [`Replace::ConfirmedRegularFile`]. On an error nothing was
    /// published and [`Staging::remove`] cleans up.
    pub fn publish(
        &mut self,
        name: &TargetName,
        replace: Replace,
    ) -> Result<Published, ExportError> {
        self.ready_to_publish()?;
        match self.packaging {
            Packaging::Bundle => self.publish_bundle(name, replace),
            Packaging::SingleFile => self.publish_single(name, replace),
        }
    }

    fn publish_bundle(
        &mut self,
        name: &TargetName,
        replace: Replace,
    ) -> Result<Published, ExportError> {
        if replace != Replace::Never {
            return Err(limit(LimitKind::RowOrder));
        }
        self.sync_dirs()?;
        self.sys.close_subdirs();
        self.check_destination()?;
        let from = self.entry.to_name();
        let mut placed = None;
        for n in 1..=naming::MAX_BUNDLE_SUFFIX {
            let to = name.candidate(n);
            let mut r = with_retries(|| {
                self.sys
                    .rename_dir_noreplace(&self.parent, &from, to.as_str())
            });
            if matches!(&r, Err(e) if is_transient(e)) && self.marker.is_some() {
                // §8.6: the marker's own handle may be what blocks the
                // rename; close it and try once more.
                self.marker = None;
                r = with_retries(|| {
                    self.sys
                        .rename_dir_noreplace(&self.parent, &from, to.as_str())
                });
            }
            match r {
                Ok(()) => {
                    placed = Some((n, to));
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io_err(e)),
            }
        }
        let Some((candidate, to)) = placed else {
            return Err(ExportError::TargetExists);
        };
        // The point of no return (§8.5): from here the export is
        // published, and what follows is best effort.
        self.state = State::Published;
        fault::point(FaultPoint::PublishedBeforeDirFlush);
        let parent_synced = self.sys.sync_parent(&self.parent).is_ok();
        fault::point(FaultPoint::PublishedBeforeMarkerRemoval);
        // The bundle's name no longer matches the sweep's pattern, so the
        // lock can go first (Windows cannot remove an open file's entry
        // at once).
        self.marker = None;
        let marker_removed =
            with_retries(|| self.sys.remove_marker(&self.parent, to.as_str())).is_ok();
        // Best effort: the bundle's own entries are flushed with it.
        let _ = self.sys.sync_published(&self.parent, to.as_str());
        Ok(Published {
            name: to,
            candidate,
            marker_removed,
            replaced: false,
            parent_synced,
        })
    }

    fn publish_single(
        &mut self,
        name: &TargetName,
        replace: Replace,
    ) -> Result<Published, ExportError> {
        if self.created.is_empty() {
            return Err(limit(LimitKind::RowOrder));
        }
        fault::point(FaultPoint::SingleFileStaged);
        self.check_destination()?;
        let from = self.entry.to_name();
        let replaced = match replace {
            Replace::Never => {
                match with_retries(|| {
                    self.sys
                        .place_file_noreplace(&self.parent, &from, name.as_str())
                }) {
                    Ok(()) => false,
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        return Err(ExportError::TargetExists)
                    }
                    Err(e) => return Err(io_err(e)),
                }
            }
            Replace::ConfirmedRegularFile => {
                match self
                    .sys
                    .target_kind(&self.parent, name.as_str())
                    .map_err(io_err)?
                {
                    sys::Target::Absent => {
                        match with_retries(|| {
                            self.sys
                                .place_file_noreplace(&self.parent, &from, name.as_str())
                        }) {
                            Ok(()) => false,
                            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                                return Err(ExportError::TargetExists)
                            }
                            Err(e) => return Err(io_err(e)),
                        }
                    }
                    sys::Target::RegularFile => {
                        with_retries(|| {
                            self.sys
                                .rename_file_replacing(&self.parent, &from, name.as_str())
                        })
                        .map_err(io_err)?;
                        true
                    }
                    sys::Target::Other => return Err(ExportError::TargetExists),
                }
            }
        };
        self.state = State::Published;
        let parent_synced = self.sys.sync_parent(&self.parent).is_ok();
        Ok(Published {
            name: name.clone(),
            candidate: 1,
            marker_removed: true,
            replaced,
            parent_synced,
        })
    }

    /// Remove what this export created (§8.5): its known files, its
    /// subfolders, the marker and the staging folder (after checking it is
    /// still the export's own), or the `.partial-file`. Returns
    /// `stagingRemoved`. Removal retries per §8.6 and is otherwise best
    /// effort; anything it did not create keeps the folder in place.
    ///
    /// The marker goes only after every file and subfolder this export
    /// made is gone: when one of them could not be removed (a handle held
    /// past the retries, a permission changed), the marker stays on disk,
    /// unlocked, so the folder remains a marked leftover that a later
    /// sweep offers to remove (D144).
    pub fn remove(&mut self) -> bool {
        if self.state != State::Live {
            return self.state == State::Removed;
        }
        self.state = State::Removed;
        // Close every handle first (Windows cannot remove an open file's
        // folder).
        self.unsynced.clear();
        match self.packaging {
            Packaging::Bundle => {
                let mut ok = true;
                for p in self.created.iter().rev() {
                    ok &= with_retries(|| {
                        self.sys.remove_file(&self.parent, &self.entry.to_name(), p)
                    })
                    .is_ok();
                }
                ok &= self.sys.remove_subdirs(&self.parent, &self.entry.to_name());
                // Close (and so unlock) the marker either way.
                self.marker = None;
                if !ok {
                    // Some of our own files are still there: keep the
                    // marker so the sweep can finish the job later.
                    let _ = self.sys.sync_parent(&self.parent);
                    return false;
                }
                ok &= with_retries(|| self.sys.remove_marker(&self.parent, &self.entry.to_name()))
                    .is_ok();
                let same = self
                    .sys
                    .same_destination(&self.parent, &self.entry.to_name())
                    .unwrap_or(false);
                ok &= same
                    && with_retries(|| {
                        self.sys
                            .remove_staging_dir(&self.parent, &self.entry.to_name())
                    })
                    .is_ok();
                let _ = self.sys.sync_parent(&self.parent);
                ok
            }
            Packaging::SingleFile => {
                if self.created.is_empty() {
                    return true;
                }
                let same = self
                    .sys
                    .same_destination(&self.parent, &self.entry.to_name())
                    .unwrap_or(false);
                let ok = same
                    && with_retries(|| {
                        self.sys
                            .remove_partial_file(&self.parent, &self.entry.to_name())
                    })
                    .is_ok();
                let _ = self.sys.sync_parent(&self.parent);
                ok
            }
        }
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        // An export that neither published nor removed (an early return or
        // a panic unwinding): remove what it made. `panic = "abort"` and a
        // crash skip this; the sweep finds those leftovers.
        if self.state == State::Live {
            let _ = self.remove();
        }
    }
}

/// Create and remove a `.oma-export-<32 hex>.probe` in `parent` (canonical):
/// whether the export can create files there (§8.2). `Ok(false)` when the
/// creation is refused for permissions or a read-only volume; other errors
/// are returned. A probe a crash leaves is swept like the other leftovers.
pub fn probe_writable(parent: &Path) -> Result<bool, ExportError> {
    check_canonical(parent)?;
    let entry = EntryName::random(EntryKind::Probe)?;
    let mut h = sys::Handles::open_parent(parent).map_err(|_| ExportError::DestinationChanged)?;
    let name = entry.to_name();
    match h.create_partial_file(parent, &name) {
        Ok(f) => {
            drop(f);
            with_retries(|| h.remove_partial_file(parent, &name)).map_err(io_err)?;
            Ok(true)
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(io_err(e)),
    }
}

// ---------------------------------------------------------------------------
// Leftovers (§8.5)
// ---------------------------------------------------------------------------

/// One leftover found by [`find_leftovers`]: its name (content-free) and
/// what removing it must find again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leftover {
    pub name: EntryName,
    /// A folder carried the marker (an empty, marker-less `.partial` folder
    /// is a crash between its creation and the marker's).
    pub has_marker: bool,
    id: Option<(u64, u64)>,
}

/// What [`find_leftovers`] found and skipped.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LeftoverScan {
    pub found: Vec<Leftover>,
    /// Matching entries younger than the minimum age.
    pub skipped_recent: u32,
    /// Matching entries whose marker (or partial file) is locked: a live
    /// export, of this instance or another.
    pub skipped_locked: u32,
    /// Matching names that are not what the pattern promises (a link, the
    /// wrong type, a non-empty folder without the marker).
    pub skipped_other: u32,
}

/// What [`remove_leftovers`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SweepReport {
    pub removed: u32,
    /// Changed since the scan (gone, replaced, locked, younger), so left.
    pub skipped: u32,
    pub failed: u32,
}

fn age(meta: &fs::Metadata, now: SystemTime) -> Option<Duration> {
    meta.modified()
        .ok()
        .and_then(|m| now.duration_since(m).ok())
}

/// Whether `file` is locked by someone else. `None`: no lock support.
fn locked_elsewhere(file: &File) -> Option<bool> {
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            Some(false)
        }
        Err(TryLockError::WouldBlock) => Some(true),
        Err(TryLockError::Error(_)) => None,
    }
}

/// List the leftovers in `parent` (canonical) that may be removed: entries
/// named `.oma-export-<32 hex>.partial` (real folders with the marker, or
/// empty ones), `.partial-file` and `.probe` (regular files), at least
/// `min_age` old ([`SWEEP_MIN_AGE`] in the app), and not locked. Nothing
/// is removed here; the dialog offers the count (§8.5).
pub fn find_leftovers(parent: &Path, min_age: Duration) -> Result<LeftoverScan, ExportError> {
    check_canonical(parent)?;
    let now = SystemTime::now();
    let mut scan = LeftoverScan::default();
    let entries = fs::read_dir(parent).map_err(io_err)?;
    let mut names: Vec<EntryName> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str().and_then(EntryName::parse))
        .collect();
    names.sort();
    for name in names {
        match inspect_leftover(parent, name, now, min_age) {
            Inspected::Found(l) => scan.found.push(l),
            Inspected::Recent => scan.skipped_recent += 1,
            Inspected::Locked => scan.skipped_locked += 1,
            Inspected::Other => scan.skipped_other += 1,
        }
    }
    Ok(scan)
}

enum Inspected {
    Found(Leftover),
    Recent,
    Locked,
    Other,
}

fn inspect_leftover(
    parent: &Path,
    name: EntryName,
    now: SystemTime,
    min_age: Duration,
) -> Inspected {
    let path = naming::child(parent, &name.to_name());
    let Ok(meta) = fs::symlink_metadata(&path) else {
        return Inspected::Other;
    };
    if sys::is_link(&meta) {
        return Inspected::Other;
    }
    let mut newest = age(&meta, now);
    let id = sys::meta_id(&meta);
    let mut has_marker = false;
    match name.kind() {
        EntryKind::PartialDir => {
            if !meta.is_dir() {
                return Inspected::Other;
            }
            match oma_fs::open_nofollow(&naming::child(&path, MARKER)) {
                Ok(marker) => {
                    if locked_elsewhere(&marker) == Some(true) {
                        return Inspected::Locked;
                    }
                    if let Ok(m) = marker.metadata() {
                        newest = match (newest, age(&m, now)) {
                            (Some(a), Some(b)) => Some(a.min(b)),
                            _ => None,
                        };
                    }
                    has_marker = true;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    let empty = fs::read_dir(&path).map(|mut d| d.next().is_none());
                    if !matches!(empty, Ok(true)) {
                        return Inspected::Other;
                    }
                }
                Err(_) => return Inspected::Other,
            }
        }
        EntryKind::PartialFile | EntryKind::Probe => {
            if !meta.is_file() {
                return Inspected::Other;
            }
            if name.kind() == EntryKind::PartialFile {
                match oma_fs::open_nofollow(&path) {
                    Ok(f) => {
                        if locked_elsewhere(&f) == Some(true) {
                            return Inspected::Locked;
                        }
                    }
                    Err(_) => return Inspected::Other,
                }
            }
        }
    }
    if !newest.is_some_and(|a| a >= min_age) {
        return Inspected::Recent;
    }
    Inspected::Found(Leftover {
        name,
        has_marker,
        id,
    })
}

/// Remove exactly the `pinned` leftovers listed at choose time (§8.5, never
/// a fresh listing), each re-checked by name, type, marker, lock and (on
/// Linux) `(dev, ino)`; a folder's marker stays locked by this call while
/// it is removed. On Linux a folder is emptied through handles opened
/// without following links.
pub fn remove_leftovers(parent: &Path, pinned: &[Leftover]) -> SweepReport {
    let mut report = SweepReport::default();
    if check_canonical(parent).is_err() {
        report.failed = u32::try_from(pinned.len()).unwrap_or(u32::MAX);
        return report;
    }
    let Ok(h) = sys::Handles::open_parent(parent) else {
        report.failed = u32::try_from(pinned.len()).unwrap_or(u32::MAX);
        return report;
    };
    for l in pinned {
        match remove_one(&h, parent, l) {
            Ok(true) => report.removed += 1,
            Ok(false) => report.skipped += 1,
            Err(_) => report.failed += 1,
        }
    }
    let _ = h.sync_parent(parent);
    report
}

fn remove_one(h: &sys::Handles, parent: &Path, l: &Leftover) -> io::Result<bool> {
    let name = l.name.to_name();
    let path = naming::child(parent, &name);
    let meta = match fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if sys::is_link(&meta) || (l.id.is_some() && sys::meta_id(&meta) != l.id) {
        return Ok(false);
    }
    match l.name.kind() {
        EntryKind::PartialDir => {
            if !meta.is_dir() {
                return Ok(false);
            }
            if l.has_marker {
                let marker = match oma_fs::open_nofollow(&naming::child(&path, MARKER)) {
                    Ok(m) => m,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
                    Err(e) => return Err(e),
                };
                match marker.try_lock() {
                    Ok(()) | Err(TryLockError::Error(_)) => {}
                    Err(TryLockError::WouldBlock) => return Ok(false),
                }
                // Held (and locked) until the folder is gone. On Windows
                // the open marker cannot be deleted under the lock, so the
                // tree removal closes it last (see `sys`).
                h.remove_tree(parent, &name, l.id, Some(marker))?;
            } else {
                // A crash before the marker: only an empty folder.
                with_retries(|| h.remove_staging_dir(parent, &name))?;
            }
            Ok(true)
        }
        EntryKind::PartialFile | EntryKind::Probe => {
            if !meta.is_file() {
                return Ok(false);
            }
            if l.name.kind() == EntryKind::PartialFile {
                let f = oma_fs::open_nofollow(&path)?;
                if locked_elsewhere(&f) == Some(true) {
                    return Ok(false);
                }
            }
            with_retries(|| h.remove_partial_file(parent, &name))?;
            Ok(true)
        }
    }
}

// ---------------------------------------------------------------------------
// Linux: everything relative to O_DIRECTORY | O_NOFOLLOW handles
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod sys {
    use std::collections::BTreeMap;
    use std::ffi::CString;
    use std::fs::{self, File};
    use std::io;
    use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use rustix::fs::{
        fstat, fsync, linkat, mkdirat, openat, renameat, renameat_with, statat, unlinkat, AtFlags,
        Dir, FileType, Mode, OFlags, RenameFlags, Stat, CWD,
    };
    use rustix::io::Errno;

    use crate::naming::{BundlePath, MARKER};

    type Id = (u64, u64);

    /// The most folder levels a leftover's removal descends.
    const MAX_DEPTH: u32 = 16;

    #[allow(clippy::useless_conversion, clippy::unnecessary_cast)]
    fn stat_id(st: &Stat) -> Id {
        (st.st_dev as u64, st.st_ino as u64)
    }

    fn file_type(st: &Stat) -> FileType {
        FileType::from_raw_mode(st.st_mode)
    }

    pub(super) fn is_link(meta: &fs::Metadata) -> bool {
        meta.file_type().is_symlink()
    }

    pub(super) fn meta_id(meta: &fs::Metadata) -> Option<Id> {
        Some((meta.dev(), meta.ino()))
    }

    fn open_dir_at<Fd: AsFd>(dir: Fd, name: &str) -> io::Result<OwnedFd> {
        Ok(openat(
            dir,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?)
    }

    fn create_at<Fd: AsFd>(dir: Fd, name: &str, read: bool) -> io::Result<File> {
        let access = if read { OFlags::RDWR } else { OFlags::WRONLY };
        let fd = openat(
            dir,
            name,
            access | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )?;
        Ok(File::from(fd))
    }

    /// What a single file's publish would replace.
    pub(super) enum Target {
        Absent,
        RegularFile,
        Other,
    }

    struct SubDir {
        /// The folder's components below staging.
        comps: Vec<String>,
        fd: Option<OwnedFd>,
    }

    pub(super) struct Handles {
        parent: OwnedFd,
        parent_id: Id,
        staging: Option<OwnedFd>,
        staging_id: Option<Id>,
        /// A single file's (or probe's) inode.
        partial_id: Option<Id>,
        subdirs: Vec<SubDir>,
        by_key: BTreeMap<String, usize>,
    }

    impl Handles {
        pub(super) fn open_parent(path: &Path) -> io::Result<Handles> {
            let parent = openat(
                CWD,
                path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            let parent_id = stat_id(&fstat(&parent)?);
            Ok(Handles {
                parent,
                parent_id,
                staging: None,
                staging_id: None,
                partial_id: None,
                subdirs: Vec::new(),
                by_key: BTreeMap::new(),
            })
        }

        pub(super) fn make_staging_dir(&mut self, _parent: &Path, name: &str) -> io::Result<()> {
            mkdirat(&self.parent, name, Mode::from_raw_mode(0o700))?;
            let fd = open_dir_at(&self.parent, name)?;
            self.staging_id = Some(stat_id(&fstat(&fd)?));
            self.staging = Some(fd);
            Ok(())
        }

        fn staging(&self) -> io::Result<BorrowedFd<'_>> {
            self.staging
                .as_ref()
                .map(|f| f.as_fd())
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }

        pub(super) fn create_marker(&self) -> io::Result<File> {
            create_at(self.staging()?, MARKER, true)
        }

        /// The handle of the folder `key` (`""`: staging itself).
        fn dir_fd(&self, key: &str) -> io::Result<BorrowedFd<'_>> {
            if key.is_empty() {
                return self.staging();
            }
            self.by_key
                .get(key)
                .and_then(|i| self.subdirs.get(*i))
                .and_then(|d| d.fd.as_ref())
                .map(|f| f.as_fd())
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }

        fn ensure_dirs(&mut self, path: &BundlePath) -> io::Result<String> {
            let mut parent_key = String::new();
            for depth in 1..=path.dirs().len() {
                let key = path.dir_key(depth);
                match self.by_key.get(&key).copied() {
                    Some(i) => {
                        // A folder whose handle was closed (see
                        // `trim_subdirs`): reopen it through the chain.
                        let closed = self.subdirs.get(i).is_some_and(|d| d.fd.is_none());
                        if closed {
                            let comps = self
                                .subdirs
                                .get(i)
                                .map(|d| d.comps.clone())
                                .unwrap_or_default();
                            let fd = self.open_chain(&comps)?;
                            if let Some(d) = self.subdirs.get_mut(i) {
                                d.fd = Some(fd);
                            }
                        }
                    }
                    None => {
                        let name = path
                            .dirs()
                            .get(depth - 1)
                            .cloned()
                            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
                        let fd = {
                            let pfd = self.dir_fd(&parent_key)?;
                            mkdirat(pfd, name.as_str(), Mode::from_raw_mode(0o700))?;
                            match open_dir_at(pfd, &name) {
                                Ok(fd) => fd,
                                Err(e) => {
                                    // Never leave a folder this export made
                                    // unregistered (later creates in it
                                    // would fail, and the removal would
                                    // not know it): take it back.
                                    let _ = unlinkat(pfd, name.as_str(), AtFlags::REMOVEDIR);
                                    return Err(e);
                                }
                            }
                        };
                        self.by_key.insert(key.clone(), self.subdirs.len());
                        self.subdirs.push(SubDir {
                            comps: path.dirs().iter().take(depth).cloned().collect(),
                            fd: Some(fd),
                        });
                    }
                }
                parent_key = key;
            }
            self.trim_subdirs(path.dirs())?;
            Ok(parent_key)
        }

        /// Keep at most [`super::MAX_OPEN_SUBDIRS`] subfolder handles: past
        /// that, flush and close every open one that is not the folder
        /// `keep` (its components) or one of its parents. A closed folder is
        /// reopened by `ensure_dirs` when a file goes into it again, and was
        /// flushed when it was closed.
        fn trim_subdirs(&mut self, keep: &[String]) -> io::Result<()> {
            let open = self.subdirs.iter().filter(|d| d.fd.is_some()).count();
            if open <= super::MAX_OPEN_SUBDIRS {
                return Ok(());
            }
            for d in &mut self.subdirs {
                if keep.starts_with(&d.comps) {
                    continue;
                }
                if let Some(fd) = d.fd.take() {
                    fsync(&fd)?;
                }
            }
            Ok(())
        }

        /// Subfolder handles open now.
        pub(super) fn open_subdirs(&self) -> usize {
            self.subdirs.iter().filter(|d| d.fd.is_some()).count()
        }

        pub(super) fn create_file(&mut self, path: &BundlePath) -> io::Result<File> {
            let key = self.ensure_dirs(path)?;
            create_at(self.dir_fd(&key)?, path.file(), false)
        }

        pub(super) fn create_partial_file(
            &mut self,
            _parent: &Path,
            name: &str,
        ) -> io::Result<File> {
            let f = create_at(&self.parent, name, false)?;
            self.partial_id = Some(stat_id(&fstat(&f)?));
            Ok(f)
        }

        pub(super) fn sync_subdirs(&self) -> io::Result<()> {
            for d in self.subdirs.iter().rev() {
                if let Some(fd) = &d.fd {
                    fsync(fd)?;
                }
            }
            Ok(())
        }

        pub(super) fn close_subdirs(&mut self) {
            for d in &mut self.subdirs {
                d.fd = None;
            }
        }

        pub(super) fn sync_staging(&self) -> io::Result<()> {
            Ok(fsync(self.staging()?)?)
        }

        pub(super) fn sync_parent(&self, _parent: &Path) -> io::Result<()> {
            match fsync(&self.parent) {
                Ok(()) | Err(Errno::INVAL) => Ok(()),
                Err(e) => Err(e.into()),
            }
        }

        /// The parent path still names the opened parent, and `name` in it
        /// is still the staging folder (or single file) this export made.
        pub(super) fn same_destination(&self, parent: &Path, name: &str) -> io::Result<bool> {
            let meta = fs::symlink_metadata(parent)?;
            if meta_id(&meta) != Some(self.parent_id) {
                return Ok(false);
            }
            let st = statat(&self.parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
            let want = match (self.staging_id, self.partial_id) {
                (Some(id), _) => (id, FileType::Directory),
                (None, Some(id)) => (id, FileType::RegularFile),
                (None, None) => return Ok(false),
            };
            Ok(stat_id(&st) == want.0 && file_type(&st) == want.1)
        }

        pub(super) fn rename_dir_noreplace(
            &self,
            _parent: &Path,
            from: &str,
            to: &str,
        ) -> io::Result<()> {
            match renameat_with(&self.parent, from, &self.parent, to, RenameFlags::NOREPLACE) {
                Ok(()) => Ok(()),
                Err(Errno::EXIST) | Err(Errno::NOTEMPTY) => {
                    Err(io::Error::from(io::ErrorKind::AlreadyExists))
                }
                Err(Errno::INVAL) | Err(Errno::NOSYS) => self.checked_rename(from, to),
                Err(e) => Err(e.into()),
            }
        }

        /// A file system without `RENAME_NOREPLACE`: an existence check,
        /// then a plain rename (not atomic against a concurrent creator;
        /// acceptable in a folder the user chose, §8.4).
        fn checked_rename(&self, from: &str, to: &str) -> io::Result<()> {
            match statat(&self.parent, to, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_) => Err(io::Error::from(io::ErrorKind::AlreadyExists)),
                Err(Errno::NOENT) => Ok(renameat(&self.parent, from, &self.parent, to)?),
                Err(e) => Err(e.into()),
            }
        }

        pub(super) fn place_file_noreplace(
            &self,
            _parent: &Path,
            from: &str,
            to: &str,
        ) -> io::Result<()> {
            match renameat_with(&self.parent, from, &self.parent, to, RenameFlags::NOREPLACE) {
                Ok(()) => return Ok(()),
                Err(Errno::EXIST) => return Err(io::Error::from(io::ErrorKind::AlreadyExists)),
                Err(Errno::INVAL) | Err(Errno::NOSYS) => {}
                Err(e) => return Err(e.into()),
            }
            // A hard link refuses an existing target too; then the source
            // goes.
            match linkat(&self.parent, from, &self.parent, to, AtFlags::empty()) {
                Ok(()) => {
                    let _ = unlinkat(&self.parent, from, AtFlags::empty());
                    Ok(())
                }
                Err(Errno::EXIST) => Err(io::Error::from(io::ErrorKind::AlreadyExists)),
                Err(Errno::PERM) | Err(Errno::OPNOTSUPP) => self.checked_rename(from, to),
                Err(e) => Err(e.into()),
            }
        }

        pub(super) fn target_kind(&self, _parent: &Path, name: &str) -> io::Result<Target> {
            match statat(&self.parent, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(st) if file_type(&st) == FileType::RegularFile => Ok(Target::RegularFile),
                Ok(_) => Ok(Target::Other),
                Err(Errno::NOENT) => Ok(Target::Absent),
                Err(e) => Err(e.into()),
            }
        }

        pub(super) fn rename_file_replacing(
            &self,
            _parent: &Path,
            from: &str,
            to: &str,
        ) -> io::Result<()> {
            Ok(renameat(&self.parent, from, &self.parent, to)?)
        }

        /// Remove the marker from the staging folder (its handle follows
        /// the folder through the publish rename).
        pub(super) fn remove_marker(&self, _parent: &Path, _folder: &str) -> io::Result<()> {
            match unlinkat(self.staging()?, MARKER, AtFlags::empty()) {
                Ok(()) | Err(Errno::NOENT) => Ok(()),
                Err(e) => Err(e.into()),
            }
        }

        pub(super) fn sync_published(&self, _parent: &Path, _name: &str) -> io::Result<()> {
            self.sync_staging()
        }

        /// The folder `comps` below staging, reopened through handles that
        /// never follow a link.
        fn open_chain(&self, comps: &[String]) -> io::Result<OwnedFd> {
            let mut fd = self.staging()?.try_clone_to_owned()?;
            for c in comps {
                fd = open_dir_at(&fd, c)?;
            }
            Ok(fd)
        }

        pub(super) fn remove_file(
            &self,
            _parent: &Path,
            _staging: &str,
            p: &BundlePath,
        ) -> io::Result<()> {
            let fd = match self.open_chain(p.dirs()) {
                Ok(fd) => fd,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(e),
            };
            match unlinkat(&fd, p.file(), AtFlags::empty()) {
                Ok(()) | Err(Errno::NOENT) => Ok(()),
                Err(e) => Err(e.into()),
            }
        }

        /// Remove the subfolders this export made, deepest first.
        pub(super) fn remove_subdirs(&self, _parent: &Path, _staging: &str) -> bool {
            let mut ok = true;
            for d in self.subdirs.iter().rev() {
                let Some((last, init)) = d.comps.split_last() else {
                    continue;
                };
                let r = self.open_chain(init).and_then(|fd| {
                    match unlinkat(&fd, last.as_str(), AtFlags::REMOVEDIR) {
                        Ok(()) | Err(Errno::NOENT) => Ok(()),
                        Err(e) => Err(e.into()),
                    }
                });
                ok &= r.is_ok();
            }
            ok
        }

        /// Remove the staging folder, if it is still this export's own (or
        /// a pinned leftover's): `rmdir` only removes an empty folder.
        pub(super) fn remove_staging_dir(&self, _parent: &Path, name: &str) -> io::Result<()> {
            Ok(unlinkat(&self.parent, name, AtFlags::REMOVEDIR)?)
        }

        pub(super) fn remove_partial_file(&self, _parent: &Path, name: &str) -> io::Result<()> {
            match unlinkat(&self.parent, name, AtFlags::empty()) {
                Ok(()) | Err(Errno::NOENT) => Ok(()),
                Err(e) => Err(e.into()),
            }
        }

        /// Remove a leftover folder: open it without following links,
        /// check its `(dev, ino)`, empty it through handles, then `rmdir`
        /// it by name after the same check.
        pub(super) fn remove_tree(
            &self,
            _parent: &Path,
            name: &str,
            id: Option<Id>,
            marker: Option<File>,
        ) -> io::Result<()> {
            let fd = open_dir_at(&self.parent, name)?;
            if id.is_some() && Some(stat_id(&fstat(&fd)?)) != id {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            clear_dir(&fd, 0)?;
            drop(marker);
            let st = statat(&self.parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
            if id.is_some() && Some(stat_id(&st)) != id {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            Ok(unlinkat(&self.parent, name, AtFlags::REMOVEDIR)?)
        }
    }

    fn clear_dir(fd: &OwnedFd, depth: u32) -> io::Result<()> {
        if depth > MAX_DEPTH {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        let mut names: Vec<CString> = Vec::new();
        for e in Dir::read_from(fd)? {
            let e = e?;
            let n = e.file_name();
            if n.to_bytes() != b"." && n.to_bytes() != b".." {
                names.push(n.to_owned());
            }
        }
        for n in names {
            let st = statat(fd, n.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)?;
            if file_type(&st) == FileType::Directory {
                let sub = openat(
                    fd,
                    n.as_c_str(),
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?;
                clear_dir(&sub, depth + 1)?;
                unlinkat(fd, n.as_c_str(), AtFlags::REMOVEDIR)?;
            } else {
                unlinkat(fd, n.as_c_str(), AtFlags::empty())?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Elsewhere (Windows): paths below the canonical parent
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "linux"))]
mod sys {
    use std::fs::{self, File};
    use std::io;
    use std::path::{Path, PathBuf};

    use crate::naming::{self, BundlePath, MARKER};

    type Id = (u64, u64);

    /// `FILE_ATTRIBUTE_REPARSE_POINT`.
    #[cfg(windows)]
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

    pub(super) fn is_link(meta: &fs::Metadata) -> bool {
        if meta.file_type().is_symlink() {
            return true;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return true;
            }
        }
        false
    }

    /// No stable file id without `unsafe` on Windows: re-checks are by
    /// name and type only (the disclosed residual).
    pub(super) fn meta_id(_meta: &fs::Metadata) -> Option<Id> {
        None
    }

    pub(super) enum Target {
        Absent,
        RegularFile,
        Other,
    }

    pub(super) struct Handles {
        staging: Option<PathBuf>,
        partial: Option<PathBuf>,
        /// Subfolders made, in creation order (by key).
        subdirs: Vec<(String, PathBuf)>,
    }

    fn real_dir(p: &Path) -> io::Result<bool> {
        let m = fs::symlink_metadata(p)?;
        Ok(m.is_dir() && !is_link(&m))
    }

    impl Handles {
        pub(super) fn open_parent(path: &Path) -> io::Result<Handles> {
            if !real_dir(path)? {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            }
            Ok(Handles {
                staging: None,
                partial: None,
                subdirs: Vec::new(),
            })
        }

        pub(super) fn make_staging_dir(&mut self, parent: &Path, name: &str) -> io::Result<()> {
            let p = naming::child(parent, name);
            fs::create_dir(&p)?;
            self.staging = Some(p);
            Ok(())
        }

        fn staging(&self) -> io::Result<&Path> {
            self.staging
                .as_deref()
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }

        pub(super) fn create_marker(&self) -> io::Result<File> {
            oma_fs::create_new_private(&naming::child(self.staging()?, MARKER))
        }

        pub(super) fn create_file(&mut self, path: &BundlePath) -> io::Result<File> {
            let root = self.staging()?.to_path_buf();
            for depth in 1..=path.dirs().len() {
                let key = path.dir_key(depth);
                if !self.subdirs.iter().any(|(k, _)| *k == key) {
                    let d = path.dir_under(&root, depth);
                    fs::create_dir(&d)?;
                    self.subdirs.push((key, d));
                }
            }
            let parent = path.dir_under(&root, path.dirs().len());
            if !real_dir(&parent)? {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            }
            oma_fs::create_new_private(&path.under(&root))
        }

        pub(super) fn create_partial_file(
            &mut self,
            parent: &Path,
            name: &str,
        ) -> io::Result<File> {
            let p = naming::child(parent, name);
            let f = oma_fs::create_new_private(&p)?;
            self.partial = Some(p);
            Ok(f)
        }

        /// `<file>:Zone.Identifier` beside a staged file (Mark of the Web).
        #[cfg(windows)]
        pub(super) fn create_zone_identifier(&self, path: &BundlePath) -> io::Result<File> {
            let file = path.under(self.staging()?);
            oma_fs::create_new_private(&naming::zone_identifier(&file))
        }

        pub(super) fn sync_subdirs(&self) -> io::Result<()> {
            for (_, d) in self.subdirs.iter().rev() {
                oma_fs::fsync_dir(d)?;
            }
            Ok(())
        }

        pub(super) fn close_subdirs(&mut self) {}

        /// No subfolder handles are kept here (paths only).
        pub(super) fn open_subdirs(&self) -> usize {
            0
        }

        pub(super) fn sync_staging(&self) -> io::Result<()> {
            oma_fs::fsync_dir(self.staging()?).map(|_| ())
        }

        pub(super) fn sync_parent(&self, parent: &Path) -> io::Result<()> {
            oma_fs::fsync_dir(parent).map(|_| ())
        }

        pub(super) fn same_destination(&self, parent: &Path, name: &str) -> io::Result<bool> {
            let m = fs::symlink_metadata(naming::child(parent, name))?;
            if is_link(&m) {
                return Ok(false);
            }
            Ok(if self.staging.is_some() {
                m.is_dir()
            } else {
                self.partial.is_some() && m.is_file()
            })
        }

        pub(super) fn rename_dir_noreplace(
            &self,
            parent: &Path,
            from: &str,
            to: &str,
        ) -> io::Result<()> {
            oma_fs::rename_dir_noreplace(&naming::child(parent, from), &naming::child(parent, to))
                .map(|_| ())
        }

        pub(super) fn place_file_noreplace(
            &self,
            parent: &Path,
            from: &str,
            to: &str,
        ) -> io::Result<()> {
            oma_fs::place_noreplace(&naming::child(parent, from), &naming::child(parent, to))
                .map(|_| ())
        }

        pub(super) fn target_kind(&self, parent: &Path, name: &str) -> io::Result<Target> {
            match fs::symlink_metadata(naming::child(parent, name)) {
                Ok(m) if m.is_file() && !is_link(&m) => Ok(Target::RegularFile),
                Ok(_) => Ok(Target::Other),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Target::Absent),
                Err(e) => Err(e),
            }
        }

        pub(super) fn rename_file_replacing(
            &self,
            parent: &Path,
            from: &str,
            to: &str,
        ) -> io::Result<()> {
            fs::rename(naming::child(parent, from), naming::child(parent, to))
        }

        pub(super) fn remove_marker(&self, parent: &Path, folder: &str) -> io::Result<()> {
            match fs::remove_file(naming::child(&naming::child(parent, folder), MARKER)) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            }
        }

        pub(super) fn sync_published(&self, parent: &Path, name: &str) -> io::Result<()> {
            oma_fs::fsync_dir(&naming::child(parent, name)).map(|_| ())
        }

        pub(super) fn remove_file(
            &self,
            parent: &Path,
            staging: &str,
            p: &BundlePath,
        ) -> io::Result<()> {
            match fs::remove_file(p.under(&naming::child(parent, staging))) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            }
        }

        pub(super) fn remove_subdirs(&self, _parent: &Path, _staging: &str) -> bool {
            let mut ok = true;
            for (_, d) in self.subdirs.iter().rev() {
                ok &= match super::with_retries(|| fs::remove_dir(d)) {
                    Ok(()) => true,
                    Err(e) => e.kind() == io::ErrorKind::NotFound,
                };
            }
            ok
        }

        pub(super) fn remove_staging_dir(&self, parent: &Path, name: &str) -> io::Result<()> {
            fs::remove_dir(naming::child(parent, name))
        }

        pub(super) fn remove_partial_file(&self, parent: &Path, name: &str) -> io::Result<()> {
            match fs::remove_file(naming::child(parent, name)) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            }
        }

        /// Remove a leftover folder. std's `remove_dir_all` does not follow
        /// links or junctions inside the tree; the marker's handle is
        /// closed first, since an open file cannot be deleted here.
        pub(super) fn remove_tree(
            &self,
            parent: &Path,
            name: &str,
            _id: Option<Id>,
            marker: Option<File>,
        ) -> io::Result<()> {
            drop(marker);
            let p = naming::child(parent, name);
            if !real_dir(&p)? {
                return Err(io::Error::from(io::ErrorKind::InvalidData));
            }
            super::with_retries(|| fs::remove_dir_all(&p))
        }
    }
}
