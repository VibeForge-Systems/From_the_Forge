//! The library: the folder that holds archives (spec §4.3, §5).
//!
//! `Library::new` checks the folder once; `create`, `open`, `open_read`,
//! `list` and `sweep` work inside it. Archive directories are named by
//! [`ArchiveId`] only; nothing from the webview or from an archive's
//! contents ever becomes a path (I2, I4).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use oma_fs::{Placed, VolumeKind};
use rusqlite::TransactionBehavior;

use crate::archive::Archive;
use crate::conn::{create_database_file, WriteConn};
use crate::error::{ArchiveError, DamageKind, IoOp, LocationRefusal, RefusalKind, Stage};
use crate::export::ExportHold;
use crate::id::{ArchiveId, Label};
use crate::layout::{
    ArchiveDir, ATTACHMENTS_DIR, IMPORTS_DIR, INCOMING_DIR, LOCK_FILE, NEW_PREFIX, TOP_LEVEL_NAMES,
};
use crate::lock::{ArchiveLock, LockProbe};
use crate::progress::{Clock, SystemClock};
use crate::reader::{ArchiveInfo, ArchiveReader};
use crate::schema::{build_schema, SchemaPlan, APPLICATION_ID, FORMAT, FTS_TOKENIZER};
use crate::space::{FsSpace, SpaceProbe, CREATE_MIN_AVAILABLE};
use crate::store_fs::{RealFs, StoreFs};
use crate::APP_VERSION;

/// The most archive directories [`Library::list`] reports.
pub const LIST_LIMIT: usize = 4096;
/// The most directory entries [`Library::list`] and [`Library::sweep`]
/// look at.
const SCAN_LIMIT: usize = 100_000;
/// Creation directories younger than this are never swept.
pub const SWEEP_MIN_AGE: Duration = Duration::from_secs(5 * 60);
/// Attempts at the final rename on Windows, where a scanner may briefly
/// hold a handle in the new directory (**UNVERIFIED** how often).
const RENAME_ATTEMPTS: u32 = 6;
const RENAME_RETRY_WAIT: Duration = Duration::from_millis(200);

/// One directory in the library that looks like an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ArchiveEntry {
    Ready(Box<ArchiveInfo>),
    /// Another handle holds the lock. The shell merges this with its job
    /// table; listing never waits.
    InUse {
        id: ArchiveId,
    },
    NeedsMigration {
        id: ArchiveId,
        version: u32,
    },
    NewerSchema {
        id: ArchiveId,
        version: u32,
    },
    NotAnArchive {
        id: ArchiveId,
    },
    Damaged {
        id: ArchiveId,
        kind: DamageKind,
    },
    /// A link, failed permissions, a refused volume or an I/O error.
    Refused {
        id: ArchiveId,
        why: RefusalKind,
    },
}

/// What [`Library::sweep`] did. Counts only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct SweepReport {
    /// Stale creation directories removed.
    pub removed: u32,
    /// Creation directories whose lock is held (a creation in progress).
    pub in_use: u32,
    /// Creation directories younger than the minimum age.
    pub too_recent: u32,
    /// Creation directories left alone because they hold something other
    /// than the expected files, contain a link, or could not be removed.
    pub left_alone: u32,
}

/// The folder that holds archives.
pub struct Library {
    root: PathBuf,
    volume: VolumeKind,
    space: Arc<dyn SpaceProbe>,
    clock: Arc<dyn Clock>,
    store: Arc<dyn StoreFs>,
}

impl std::fmt::Debug for Library {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Library")
            .field("volume", &self.volume)
            .finish_non_exhaustive()
    }
}

fn refuse_volume(kind: VolumeKind) -> Result<(), ArchiveError> {
    match kind {
        VolumeKind::Network(_) => Err(ArchiveError::NetworkVolume),
        VolumeKind::Virtualised(_) => Err(ArchiveError::VirtualisedVolume),
        _ => Ok(()),
    }
}

/// The checks of `Library::new`, run again before every creation.
fn check_root(root: &Path) -> Result<(PathBuf, VolumeKind), ArchiveError> {
    // A UNC path is recognised without touching the disk (Windows).
    if let Ok(kind) = oma_fs::volume_kind(root) {
        refuse_volume(kind)?;
    }
    let canonical =
        dunce::canonicalize(root).map_err(|e| ArchiveError::io(IoOp::Canonicalize, &e))?;
    let meta =
        fs::symlink_metadata(&canonical).map_err(|e| ArchiveError::io(IoOp::Metadata, &e))?;
    if !meta.is_dir() {
        return Err(ArchiveError::Location(LocationRefusal::NotADirectory));
    }
    if oma_fs::is_filesystem_root(&canonical) {
        return Err(ArchiveError::Location(LocationRefusal::FilesystemRoot));
    }
    if oma_fs::enclosing_backup_dir(&canonical).is_some()
        || oma_fs::is_within_mobilesync(&canonical)
        || oma_fs::is_within_imazing_backups(&canonical)
    {
        return Err(ArchiveError::Location(LocationRefusal::InsideBackup));
    }
    // An unreadable volume kind is "unknown", not a refusal: drive letters
    // on Windows are always unknown (oma-fs `volume`).
    let kind = oma_fs::volume_kind(&canonical).unwrap_or(VolumeKind::Unknown);
    refuse_volume(kind)?;
    oma_fs::check_owned_unshared(&canonical, oma_fs::Expect::Dir)
        .map_err(ArchiveError::Permissions)?;
    Ok((canonical, kind))
}

/// Rename the finished creation directory into place, never replacing.
fn rename_into_place(store: &dyn StoreFs, from: &Path, to: &Path) -> io::Result<Placed> {
    let mut attempt = 1u32;
    loop {
        match store.rename_dir_noreplace(from, to) {
            Ok(p) => return Ok(p),
            Err(e)
                if cfg!(windows)
                    && crate::lock::is_transient_sharing_error(&e)
                    && attempt < RENAME_ATTEMPTS =>
            {
                attempt = attempt.saturating_add(1);
                std::thread::sleep(RENAME_RETRY_WAIT);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Removes a creation directory on every early return.
struct RemoveOnDrop(Option<PathBuf>);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            // Our own fresh directory; `remove_dir_all` does not follow
            // links inside it.
            let _ = fs::remove_dir_all(p);
        }
    }
}

impl Library {
    /// Check `root` and use it as the library. It must be a real directory
    /// that is not a file-system root and not inside any backup (I1); on a
    /// volume that is neither network nor virtualised (D54); and on Unix
    /// owned by the effective user and not writable by others.
    pub fn new(root: &Path) -> Result<Library, ArchiveError> {
        let (root, volume) = check_root(root)?;
        Ok(Library {
            root,
            volume,
            space: Arc::new(FsSpace),
            clock: Arc::new(SystemClock),
            store: Arc::new(RealFs),
        })
    }

    /// Use another free-space probe (tests, or a shell-wide one).
    pub fn with_space_probe(mut self, probe: Arc<dyn SpaceProbe>) -> Library {
        self.space = probe;
        self
    }

    /// Use another clock.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Library {
        self.clock = clock;
        self
    }

    /// Make creation's ordered file-system calls through `store` (a test
    /// records them; production uses [`RealFs`]).
    pub fn with_store_fs(mut self, store: Arc<dyn StoreFs>) -> Library {
        self.store = store;
        self
    }

    /// The library volume's kind. `Fat` deserves a warning (files over
    /// 4 GiB fail); `Unknown` is normal on Windows drive letters.
    pub fn volume(&self) -> VolumeKind {
        self.volume
    }

    /// §5.3 steps 1–3: a real directory holding `archive.sqlite`, private
    /// on Unix, on an accepted volume. Returns whether the lock file exists.
    fn precheck(&self, dir: &ArchiveDir) -> Result<bool, ArchiveError> {
        let meta =
            fs::symlink_metadata(dir.root()).map_err(|e| ArchiveError::io(IoOp::Metadata, &e))?;
        if meta.file_type().is_symlink() {
            return Err(ArchiveError::Permissions(oma_fs::PermProblem::Link));
        }
        if !meta.is_dir() {
            return Err(ArchiveError::NotAnArchive);
        }
        if !dir.database_exists()? {
            return Err(ArchiveError::NotAnArchive);
        }
        let lock_present = dir.lock_exists()?;
        dir.check_private_all(lock_present)
            .map_err(ArchiveError::Permissions)?;
        if let Ok(kind) = oma_fs::volume_kind(dir.root()) {
            refuse_volume(kind)?;
        }
        Ok(lock_present)
    }

    /// Open an archive (§5.3): the checks, the lock, a ReadOnly connection,
    /// the header, the fingerprint and the meta row. Writes nothing, except
    /// that a missing lock file is recreated. An older schema opens, with
    /// [`Archive::needs_migration`] set.
    pub fn open(&self, id: &ArchiveId) -> Result<Archive, ArchiveError> {
        let dir = ArchiveDir::of(&self.root, id);
        let lock_present = self.precheck(&dir)?;
        let lock = if lock_present {
            ArchiveLock::acquire(&dir.lock())?
        } else {
            tracing::info!("archive lock file missing; recreated");
            ArchiveLock::create(&dir.lock())?
        };
        let plan = SchemaPlan::current();
        let reader = ArchiveReader::open(&dir, id.clone(), &plan)?;
        Ok(Archive::new(
            lock,
            reader,
            dir,
            plan,
            Arc::clone(&self.space),
            Arc::clone(&self.clock),
            Arc::clone(&self.store),
        ))
    }

    /// Take archive `id`'s lock file for an export job's lifetime (M6 spec
    /// §3.5, D124): the same checks and the same `ArchiveLock::acquire` as
    /// [`Library::open`], so an import or a verify, in this process or in
    /// another instance, gets `Locked` while the hold lives, and
    /// [`Library::list`] reports the archive as in use. Readers stay
    /// lock-free. `Locked` when the lock is already held. Like `open`, a
    /// missing lock file is recreated.
    ///
    /// [`ArchiveReader::with_export_snapshot`] requires the hold of the
    /// archive it reads.
    pub fn hold_for_export(&self, id: &ArchiveId) -> Result<ExportHold, ArchiveError> {
        let dir = ArchiveDir::of(&self.root, id);
        let lock_present = self.precheck(&dir)?;
        let lock = if lock_present {
            ArchiveLock::acquire(&dir.lock())?
        } else {
            tracing::info!("archive lock file missing; recreated");
            ArchiveLock::create(&dir.lock())?
        };
        Ok(ExportHold::new(lock, dir))
    }

    /// A lock-free, query-only reader (D84). Refuses an older schema with
    /// `NeedsMigration` and a newer one with `NewerSchema`.
    pub fn open_read(&self, id: &ArchiveId) -> Result<ArchiveReader, ArchiveError> {
        let dir = ArchiveDir::of(&self.root, id);
        self.precheck(&dir)?;
        let plan = SchemaPlan::current();
        let reader = ArchiveReader::open(&dir, id.clone(), &plan)?;
        if reader.schema_version() < plan.version {
            return Err(ArchiveError::NeedsMigration {
                found: reader.schema_version(),
            });
        }
        Ok(reader)
    }

    /// The archives in the library, in id order. Only real `a<32hex>`
    /// directories are reported; dot-directories, links named otherwise and
    /// foreign names are skipped. For each, the lock is tried and released
    /// at once (never waiting), then a ReadOnly connection checks the
    /// header and fingerprint before any table is read. Never writes
    /// archive data.
    pub fn list(&self) -> Result<Vec<ArchiveEntry>, ArchiveError> {
        let entries = fs::read_dir(&self.root).map_err(|e| ArchiveError::io(IoOp::ReadDir, &e))?;
        let plan = SchemaPlan::current();
        let mut out = Vec::new();
        for entry in entries.take(SCAN_LIMIT) {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(|n| ArchiveId::parse(n).ok()) else {
                continue;
            };
            if out.len() >= LIST_LIMIT {
                tracing::info!(limit = LIST_LIMIT, "archive listing truncated");
                break;
            }
            out.push(self.list_one(id, &plan));
        }
        out.sort_by(|a, b| entry_id(a).cmp(entry_id(b)));
        Ok(out)
    }

    fn list_one(&self, id: ArchiveId, plan: &SchemaPlan) -> ArchiveEntry {
        let dir = ArchiveDir::of(&self.root, &id);
        let refused = |why| ArchiveEntry::Refused {
            id: id.clone(),
            why,
        };
        let lock_present = match self.precheck(&dir) {
            Ok(p) => p,
            Err(e) => return self.entry_for_error(id.clone(), e),
        };
        if lock_present {
            match ArchiveLock::probe(&dir.lock()) {
                Ok(LockProbe::Free) => {}
                Ok(LockProbe::Held) => return ArchiveEntry::InUse { id },
                Err(ArchiveError::Permissions(p)) => return refused(RefusalKind::Permissions(p)),
                Err(ArchiveError::Io { kind, .. }) => return refused(RefusalKind::Io(kind)),
                Err(_) => return refused(RefusalKind::Io(io::ErrorKind::Other)),
            }
        }
        let reader = match ArchiveReader::open(&dir, id.clone(), plan) {
            Ok(r) => r,
            Err(e) => return self.entry_for_error(id, e),
        };
        if reader.schema_version() < plan.version {
            return ArchiveEntry::NeedsMigration {
                id,
                version: reader.schema_version(),
            };
        }
        match reader.info() {
            Ok(info) => ArchiveEntry::Ready(Box::new(info)),
            Err(e) => self.entry_for_error(id, e),
        }
    }

    fn entry_for_error(&self, id: ArchiveId, e: ArchiveError) -> ArchiveEntry {
        match e {
            ArchiveError::Permissions(oma_fs::PermProblem::Link) => ArchiveEntry::Refused {
                id,
                why: RefusalKind::Link,
            },
            ArchiveError::Permissions(p) => ArchiveEntry::Refused {
                id,
                why: RefusalKind::Permissions(p),
            },
            ArchiveError::NetworkVolume | ArchiveError::VirtualisedVolume => {
                ArchiveEntry::Refused {
                    id,
                    why: RefusalKind::Volume,
                }
            }
            ArchiveError::Io { kind, .. } => ArchiveEntry::Refused {
                id,
                why: RefusalKind::Io(kind),
            },
            ArchiveError::NewerSchema { found } => ArchiveEntry::NewerSchema { id, version: found },
            ArchiveError::NeedsMigration { found } => {
                ArchiveEntry::NeedsMigration { id, version: found }
            }
            ArchiveError::NotAnArchive => ArchiveEntry::NotAnArchive { id },
            ArchiveError::Damaged(kind) => ArchiveEntry::Damaged { id, kind },
            ArchiveError::Locked => ArchiveEntry::InUse { id },
            _ => ArchiveEntry::Damaged {
                id,
                kind: DamageKind::Unreadable,
            },
        }
    }

    /// Create an archive (§5.2) and return its id.
    ///
    /// Everything is built in `.oma-new-<32hex>` (0700) under its own lock:
    /// the database (created empty with mode 0600, then WAL, the v1 DDL,
    /// the header pragmas, the meta row and the FTS `secure-delete` option
    /// in one transaction, then a TRUNCATE checkpoint), then `attachments/`,
    /// `attachments/.incoming/` and `imports/`, each flushed. The directory
    /// is flushed, its permissions are checked as an open checks them
    /// (so a drive that ignores modes fails here, not on every later open),
    /// every handle is closed, and it is renamed to `a<32hex>` without
    /// replacing anything; then the library is flushed. A failure before
    /// the rename removes the creation directory. Once the rename has
    /// succeeded the id is returned: a failed library flush is logged as a
    /// durability warning.
    pub fn create(&self, label: Option<Label>) -> Result<ArchiveId, ArchiveError> {
        let (root, _) = check_root(&self.root)?;
        if root != self.root {
            return Err(ArchiveError::Location(LocationRefusal::NotADirectory));
        }
        match self.space.available(&self.root) {
            Ok(avail) if avail < CREATE_MIN_AVAILABLE => {
                return Err(ArchiveError::Location(LocationRefusal::InsufficientSpace));
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(kind = ?e.kind(), "free space unknown at creation"),
        }
        let plan = SchemaPlan::current();
        let (id, uuid) = ArchiveId::generate();
        let creating = ArchiveDir::creating(&self.root, &id);
        let target = ArchiveDir::of(&self.root, &id);
        if fs::symlink_metadata(target.root()).is_ok() {
            return Err(ArchiveError::Internal("archive id collision"));
        }
        new_private_dir(creating.root()).map_err(|e| ArchiveError::io(IoOp::CreateDir, &e))?;
        let mut cleanup = RemoveOnDrop(Some(creating.root().to_path_buf()));

        let lock = ArchiveLock::create(&creating.lock())?;
        create_database_file(&creating)?;
        {
            let mut w = WriteConn::open_new(&creating)?;
            let sql = |e: rusqlite::Error| ArchiveError::sqlite(Stage::Create, &e);
            let tx = rusqlite::Transaction::new(w.conn_mut(), TransactionBehavior::Immediate)
                .map_err(sql)?;
            build_schema(&tx, &plan, plan.version, Stage::Create)?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)
                .map_err(sql)?;
            tx.pragma_update(None, "user_version", plan.version)
                .map_err(sql)?;
            tx.execute(
                "INSERT INTO archive_meta (id, format, schema_version, archive_uuid, \
                     created_at_ns, created_by, label, name_scheme, max_deriver_version, \
                     max_decoder_version, fts_tokenizer) \
                 VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, 1, 0, 0, ?7)",
                rusqlite::params![
                    FORMAT,
                    plan.version,
                    uuid.as_slice(),
                    self.clock.now_ns(),
                    APP_VERSION,
                    label.as_ref().map(Label::as_str),
                    FTS_TOKENIZER
                ],
            )
            .map_err(sql)?;
            tx.execute(
                "INSERT INTO message_fts(message_fts, rank) VALUES('secure-delete', 1)",
                [],
            )
            .map_err(sql)?;
            tx.commit().map_err(sql)?;
            let busy: i64 = w
                .conn()
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))
                .map_err(sql)?;
            if busy != 0 {
                return Err(ArchiveError::Internal("checkpoint busy on a new archive"));
            }
        }
        for d in [
            creating.attachments(),
            creating.incoming(),
            creating.imports(),
        ] {
            self.store
                .create_dir_durable(&d)
                .map_err(|e| ArchiveError::io(IoOp::CreateDir, &e))?;
        }
        // SQLite flushes the directory only when it creates a journal or
        // WAL, not for the main file (C1.11).
        self.store
            .fsync_dir(creating.root())
            .map_err(|e| ArchiveError::io(IoOp::Sync, &e))?;
        // What every later open checks (§5.3 step 2), checked before the
        // archive becomes visible: a drive that ignores the requested modes
        // (vfat or exFAT mounted with the usual umask reports 0755 and
        // 0644) would otherwise give an archive that can never be opened.
        // The guard removes the creation directory.
        creating
            .check_private_all(true)
            .map_err(ArchiveError::Permissions)?;
        drop(lock);

        crate::fault::point(crate::fault::FaultPoint::CreateBeforeRename);
        let placed = rename_into_place(self.store.as_ref(), creating.root(), target.root())
            .map_err(|e| ArchiveError::io(IoOp::Rename, &e))?;
        cleanup.0 = None;
        crate::fault::point(crate::fault::FaultPoint::CreateAfterRename);
        // The archive is complete and visible now, so a failed flush of the
        // library is a durability warning, not a failed creation: a caller
        // told "failed" would retry and orphan this archive.
        match self.store.fsync_dir(&self.root) {
            Ok(synced) => tracing::info!(?placed, ?synced, "archive created"),
            Err(e) => tracing::warn!(
                ?placed,
                kind = ?e.kind(),
                "archive created; flushing the library folder failed, so a power loss may \
                 undo the creation"
            ),
        }
        Ok(id)
    }

    /// Remove stale creation directories (§4.3): real, unlocked
    /// `.oma-new-<32hex>` directories at least [`SWEEP_MIN_AGE`] old that
    /// hold only the files and empty directories creation makes. Anything
    /// else is counted and left alone.
    pub fn sweep(&self) -> SweepReport {
        self.sweep_older_than(SWEEP_MIN_AGE)
    }

    /// [`Library::sweep`] with another minimum age.
    pub fn sweep_older_than(&self, min_age: Duration) -> SweepReport {
        let mut report = SweepReport::default();
        let Ok(entries) = fs::read_dir(&self.root) else {
            return report;
        };
        for entry in entries.take(SCAN_LIMIT) {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let Some(hex) = name.to_str().and_then(|n| n.strip_prefix(NEW_PREFIX)) else {
                continue;
            };
            let mut id_text = String::from("a");
            id_text.push_str(hex);
            if ArchiveId::parse(&id_text).is_err() {
                continue;
            }
            match sweep_one(&entry.path(), min_age) {
                SweepOne::Removed => report.removed = report.removed.saturating_add(1),
                SweepOne::InUse => report.in_use = report.in_use.saturating_add(1),
                SweepOne::TooRecent => report.too_recent = report.too_recent.saturating_add(1),
                SweepOne::LeftAlone => report.left_alone = report.left_alone.saturating_add(1),
            }
        }
        if report.left_alone > 0 {
            tracing::info!(
                left_alone = report.left_alone,
                "creation directories with unexpected contents were left alone"
            );
        }
        report
    }
}

fn entry_id(e: &ArchiveEntry) -> &ArchiveId {
    match e {
        ArchiveEntry::Ready(info) => &info.id,
        ArchiveEntry::InUse { id }
        | ArchiveEntry::NeedsMigration { id, .. }
        | ArchiveEntry::NewerSchema { id, .. }
        | ArchiveEntry::NotAnArchive { id }
        | ArchiveEntry::Damaged { id, .. }
        | ArchiveEntry::Refused { id, .. } => id,
    }
}

fn new_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    {
        fs::DirBuilder::new().create(path)
    }
}

enum SweepOne {
    Removed,
    InUse,
    TooRecent,
    LeftAlone,
}

fn is_real_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
}

fn is_real_file(m: &fs::Metadata) -> bool {
    m.is_file() && !m.file_type().is_symlink()
}

/// The names in `dir`, or `None` if it cannot be read or holds more than
/// `max` entries.
fn names_in(dir: &Path, max: usize) -> Option<Vec<(String, fs::Metadata, PathBuf)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name().into_string().ok()?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).ok()?;
        out.push((name, meta, path));
        if out.len() > max {
            return None;
        }
    }
    Some(out)
}

fn sweep_one(path: &Path, min_age: Duration) -> SweepOne {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return SweepOne::LeftAlone;
    };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return SweepOne::LeftAlone;
    }
    let old_enough = meta
        .modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .is_some_and(|age| age >= min_age);
    if !old_enough {
        return SweepOne::TooRecent;
    }
    let dir = ArchiveDir::at(path.to_path_buf());
    let Some(top) = names_in(path, TOP_LEVEL_NAMES.len()) else {
        return SweepOne::LeftAlone;
    };
    // Only the expected names, each of the expected type.
    for (name, m, _) in &top {
        let expected_dir = name == ATTACHMENTS_DIR || name == IMPORTS_DIR;
        let ok = TOP_LEVEL_NAMES.contains(&name.as_str())
            && if expected_dir {
                m.is_dir() && !m.file_type().is_symlink()
            } else {
                is_real_file(m)
            };
        if !ok {
            return SweepOne::LeftAlone;
        }
    }
    if top.iter().any(|(n, _, _)| n == LOCK_FILE) {
        match ArchiveLock::probe(&dir.lock()) {
            Ok(LockProbe::Free) => {}
            Ok(LockProbe::Held) => return SweepOne::InUse,
            Err(_) => return SweepOne::LeftAlone,
        }
    }
    // attachments/ may hold only an empty .incoming/; imports/ nothing.
    if is_real_dir(&dir.attachments()) {
        let Some(inside) = names_in(&dir.attachments(), 1) else {
            return SweepOne::LeftAlone;
        };
        for (name, m, _) in &inside {
            if name != INCOMING_DIR || !m.is_dir() || m.file_type().is_symlink() {
                return SweepOne::LeftAlone;
            }
        }
        if !inside.is_empty() && names_in(&dir.incoming(), 0).is_none_or(|v| !v.is_empty()) {
            return SweepOne::LeftAlone;
        }
    }
    if is_real_dir(&dir.imports()) && names_in(&dir.imports(), 0).is_none_or(|v| !v.is_empty()) {
        return SweepOne::LeftAlone;
    }
    // Remove: files first, then the (empty) directories, then the root.
    for (_, m, file) in &top {
        if is_real_file(m) && fs::remove_file(file).is_err() {
            return SweepOne::LeftAlone;
        }
    }
    for d in [dir.incoming(), dir.attachments(), dir.imports()] {
        if is_real_dir(&d) && fs::remove_dir(&d).is_err() {
            return SweepOne::LeftAlone;
        }
    }
    if fs::remove_dir(path).is_err() {
        return SweepOne::LeftAlone;
    }
    SweepOne::Removed
}
