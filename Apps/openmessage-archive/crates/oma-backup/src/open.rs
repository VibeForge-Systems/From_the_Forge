//! Opening a backup for one job: stage Manifest.db (copied or decrypted),
//! open it hardened, and hold the keys and staging until `close()` (D5).

use std::fs;
use std::io::{BufWriter, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::cancel::{JobCtx, Phase};
use crate::cbc_stream::{copy_plain, decrypt_cbc_pkcs7_stream, CbcInput, Output};
use crate::error::{BackupError, IoOp, IoSide, MalformedDetail};
use crate::guard::{open_source_file, OpenFailure};
use crate::inspect::{Classification, Inspection, SQLITE_HEADER};
use crate::keybag::{Keybag, KeybagNote};
use crate::keys::{unlock, unwrap_manifest_key, ClassKeys, Password};
use crate::limits::{LimitKind, Limits};
use crate::manifest_db::{ManifestDb, ManifestEntry, OpenMode};
use crate::plists::BackupFile;
use crate::staging::{create_new_file, CleanupReport, RemoveOnDrop, StagingSession};

/// Unlocked key material, zeroized on drop.
pub(crate) struct Keys {
    pub keybag: Keybag,
    pub class_keys: ClassKeys,
}

/// What happened while staging Manifest.db.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CopyFacts {
    /// The backup had a `Manifest.db-wal`.
    pub wal_present: bool,
    /// It was copied beside the staged database and applies.
    pub wal_applied: bool,
    /// The staged database's header says WAL mode (bytes 18–19 are 2).
    pub header_wal_mode: bool,
}

/// Length and modification time of a file, to detect a backup that changes
/// while it is being checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp(Option<(u64, Option<SystemTime>)>);

impl Stamp {
    pub(crate) fn of(path: &Path) -> Stamp {
        Stamp(
            fs::symlink_metadata(path)
                .ok()
                .map(|m| (m.len(), m.modified().ok())),
        )
    }
}

/// Stamps of `Status.plist` and `Manifest.db`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChangeStamp {
    status: Stamp,
    manifest_db: Stamp,
}

impl ChangeStamp {
    pub(crate) fn take(dir: &Path) -> ChangeStamp {
        ChangeStamp {
            status: Stamp::of(&dir.join("Status.plist")),
            manifest_db: Stamp::of(&dir.join("Manifest.db")),
        }
    }
}

/// A backup opened for one job.
pub struct OpenBackup {
    // Field order fixes drop order: db, keys, staging.
    pub(crate) db: Option<ManifestDb>,
    pub(crate) keys: Option<Keys>,
    pub(crate) staging: Option<StagingSession>,
    pub(crate) inspection: Inspection,
    pub(crate) limits: Limits,
    pub(crate) copy: CopyFacts,
    pub(crate) before: ChangeStamp,
    /// Stamps of blobs staged since open (sms.db and its `-wal`), taken
    /// before each was copied (M3 §3.2).
    pub(crate) extra_stamps: Mutex<Vec<(PathBuf, Stamp)>>,
}

impl std::fmt::Debug for OpenBackup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenBackup")
            .field("id", &self.inspection.id)
            .field("encrypted", &self.keys.is_some())
            .finish_non_exhaustive()
    }
}

/// An `open` failure, with the cleanup that followed it.
#[derive(Debug)]
pub struct OpenError {
    /// Why opening failed.
    pub error: BackupError,
    /// Whether the staging session was removed.
    pub cleanup: CleanupReport,
}

fn source_open(path: &Path, file: BackupFile) -> Result<(fs::File, u64), BackupError> {
    open_source_file(path).map_err(|e| match e {
        OpenFailure::Missing => BackupError::malformed(file, MalformedDetail::Missing),
        OpenFailure::NotRegular => BackupError::malformed(file, MalformedDetail::NotRegularFile),
        OpenFailure::Io(e) => BackupError::source_io(IoOp::Open, &e),
    })
}

/// Stream `len` bytes from `src` to a new file at `dst`, via `dst.partial`.
pub(crate) fn copy_to_staging(
    src: &mut impl Read,
    len: u64,
    dst: &Path,
    file: BackupFile,
    ctx: &mut JobCtx<'_>,
    phase: Phase,
    progress_base: (u64, Option<u64>),
) -> Result<u64, BackupError> {
    let partial = partial_path(dst);
    let out = create_new_file(&partial).map_err(|e| BackupError::staging_io(IoOp::Create, &e))?;
    // Declared before the writer, so the writer is closed first on an error.
    let remove_partial = RemoveOnDrop::new(&partial);
    let mut out = BufWriter::new(out);
    let done = copy_plain(
        src,
        len,
        Output {
            dst: &mut out,
            side: IoSide::Staging,
        },
        file,
        ctx,
        (phase, progress_base.0, progress_base.1),
    )?;
    drop(out);
    fs::rename(&partial, dst).map_err(|e| BackupError::staging_io(IoOp::Rename, &e))?;
    remove_partial.disarm();
    Ok(done)
}

pub(crate) fn partial_path(dst: &Path) -> PathBuf {
    let mut name = dst
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".partial");
    dst.with_file_name(name)
}

/// Bytes 18 and 19 of an SQLite header are both 2 in WAL mode.
fn header_says_wal(path: &Path) -> bool {
    let mut header = [0u8; 20];
    fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut header))
        .is_ok()
        && header.starts_with(SQLITE_HEADER)
        && header.get(18..20) == Some(&[2, 2][..])
}

impl OpenBackup {
    /// Copy (unencrypted) or decrypt (encrypted) Manifest.db into `staging`,
    /// then open it hardened. `password` is consumed and zeroized before this
    /// returns. On error the session is closed and its report returned.
    pub fn open(
        inspection: Inspection,
        staging: StagingSession,
        password: Option<Password>,
        limits: &Limits,
        ctx: &mut JobCtx<'_>,
    ) -> Result<Self, OpenError> {
        let before = ChangeStamp::take(inspection.dir());
        let mut ob = OpenBackup {
            db: None,
            keys: None,
            staging: Some(staging),
            inspection,
            limits: limits.clone(),
            copy: CopyFacts::default(),
            before,
            extra_stamps: Mutex::new(Vec::new()),
        };
        match ob.open_inner(password, ctx) {
            Ok(()) => {
                tracing::info!(
                    backup_id = ob.inspection.id.as_str(),
                    encrypted = ob.keys.is_some(),
                    "manifest staged"
                );
                Ok(ob)
            }
            Err(error) => {
                let cleanup = ob.close();
                Err(OpenError { error, cleanup })
            }
        }
    }

    fn open_inner(
        &mut self,
        password: Option<Password>,
        ctx: &mut JobCtx<'_>,
    ) -> Result<(), BackupError> {
        let encrypted = match self.inspection.classification {
            Classification::Ready => false,
            Classification::NeedsPassword => true,
            Classification::Incomplete { snapshot, .. } => {
                return Err(BackupError::Incomplete { snapshot })
            }
            Classification::Unsupported(r) => return Err(BackupError::Unsupported(r)),
            Classification::Unreadable { file, detail } => {
                return Err(BackupError::Malformed { file, detail })
            }
        };
        let staging = self
            .staging
            .as_ref()
            .ok_or(BackupError::Internal("no staging session"))?;
        let m = staging.subdir(&["m"])?;
        let staged_db = m.join("Manifest.db");
        let dir = self.inspection.dir().to_path_buf();

        let (mut src, len) = source_open(&dir.join("Manifest.db"), BackupFile::ManifestDb)?;
        if len > self.limits.manifest_db_bytes {
            return Err(BackupError::limit(
                LimitKind::ManifestDbBytes,
                len,
                self.limits.manifest_db_bytes,
            ));
        }
        let wal = match open_source_file(&dir.join("Manifest.db-wal")) {
            Ok(found) => Some(found),
            Err(OpenFailure::Missing | OpenFailure::NotRegular) => None,
            Err(OpenFailure::Io(e)) => return Err(BackupError::source_io(IoOp::Open, &e)),
        };
        self.copy.wal_present = wal.is_some();

        if !encrypted {
            drop(password);
            let total = len.saturating_add(wal.as_ref().map_or(0, |(_, l)| *l));
            ctx.report(Phase::CopyingManifest, 0, Some(total));
            copy_to_staging(
                &mut src,
                len,
                &staged_db,
                BackupFile::ManifestDb,
                ctx,
                Phase::CopyingManifest,
                (0, Some(total)),
            )?;
            if let Some((mut wal_file, wal_len)) = wal {
                if wal_len > self.limits.manifest_db_bytes {
                    return Err(BackupError::limit(
                        LimitKind::ManifestDbBytes,
                        wal_len,
                        self.limits.manifest_db_bytes,
                    ));
                }
                // The backup's -shm is never copied: SQLite rebuilds it, and
                // its content is controlled by whoever made the backup.
                copy_to_staging(
                    &mut wal_file,
                    wal_len,
                    &m.join("Manifest.db-wal"),
                    BackupFile::ManifestDbWal,
                    ctx,
                    Phase::CopyingManifest,
                    (len, Some(total)),
                )?;
            }
        } else {
            let password = match password {
                Some(p) if !p.is_empty() => p,
                _ => return Err(BackupError::PasswordRequired),
            };
            let manifest = self
                .inspection
                .manifest
                .as_ref()
                .ok_or(BackupError::Internal("encrypted without Manifest.plist"))?;
            let keybag_bytes =
                manifest
                    .backup_key_bag
                    .as_deref()
                    .ok_or(BackupError::Unsupported(
                        crate::error::UnsupportedReason::NoKeybag,
                    ))?;
            let manifest_key = manifest
                .manifest_key
                .as_deref()
                .ok_or(BackupError::Unsupported(
                    crate::error::UnsupportedReason::EncryptedWithoutManifestKey,
                ))?;
            let keybag = Keybag::parse(keybag_bytes, &self.limits)?;
            let class_keys = unlock(&keybag, password, ctx)?;
            let mkey = unwrap_manifest_key(&class_keys, &keybag, manifest_key)?;
            self.keys = Some(Keys { keybag, class_keys });

            if len == 0 {
                return Err(BackupError::malformed(
                    BackupFile::ManifestDb,
                    MalformedDetail::Empty,
                ));
            }
            let partial = partial_path(&staged_db);
            let out =
                create_new_file(&partial).map_err(|e| BackupError::staging_io(IoOp::Create, &e))?;
            let mut out = BufWriter::new(out);
            decrypt_cbc_pkcs7_stream(
                &mkey,
                &[0u8; 16],
                CbcInput {
                    src: &mut src,
                    src_len: len,
                    file: BackupFile::ManifestDb,
                },
                Output {
                    dst: &mut out,
                    side: IoSide::Staging,
                },
                ctx,
                Phase::DecryptingManifest,
            )?;
            drop(out);
            fs::rename(&partial, &staged_db)
                .map_err(|e| BackupError::staging_io(IoOp::Rename, &e))?;
            // How -wal behaves in encrypted backups is UNVERIFIED; it is not applied.
        }
        drop(src);

        self.copy.header_wal_mode = header_says_wal(&staged_db);
        self.copy.wal_applied = !encrypted && self.copy.wal_present && self.copy.header_wal_mode;
        let db = ManifestDb::open_hardened(&staged_db, ctx.cancel)?;
        self.db = Some(db);
        Ok(())
    }

    /// The inspection this backup was opened from.
    pub fn inspection(&self) -> &Inspection {
        &self.inspection
    }

    /// How the staged Manifest.db was opened (recorded for the WAL question
    /// in §7.2).
    pub fn manifest_open_mode(&self) -> Option<OpenMode> {
        self.db.as_ref().map(|d| d.mode)
    }

    /// The staging session directory, while open.
    pub fn staging_path(&self) -> Option<&Path> {
        self.staging.as_ref().map(StagingSession::path)
    }

    /// Notes from the keybag (encrypted backups only).
    pub fn keybag_notes(&self) -> &[KeybagNote] {
        self.keys
            .as_ref()
            .map_or(&[], |k| k.keybag.notes.as_slice())
    }

    /// `true` when the backup is encrypted (and was unlocked).
    pub fn is_encrypted(&self) -> bool {
        self.keys.is_some()
    }

    pub(crate) fn db(&self) -> Result<&ManifestDb, BackupError> {
        self.db.as_ref().ok_or(BackupError::Internal("closed"))
    }

    pub(crate) fn session(&self) -> Result<&StagingSession, BackupError> {
        self.staging.as_ref().ok_or(BackupError::Internal("closed"))
    }

    /// Look up a file by domain and relative path (§9).
    pub fn find_file(
        &self,
        domain: &str,
        relative_path: &str,
    ) -> Result<Option<ManifestEntry>, BackupError> {
        self.db()?.find(domain, relative_path, &self.limits)
    }

    /// `true` when `Status.plist`, `Manifest.db` and every blob staged
    /// through [`OpenBackup::extract_database_with`] (sms.db and its `-wal`)
    /// still have the length and modification time they had before they
    /// were read. The stamps themselves are not exposed.
    pub fn unchanged_since_open(&self) -> Result<bool, BackupError> {
        if ChangeStamp::take(self.inspection.dir()) != self.before {
            return Ok(false);
        }
        let extra = self
            .extra_stamps
            .lock()
            .map_err(|_| BackupError::Internal("stamp list poisoned"))?;
        Ok(extra.iter().all(|(path, stamp)| Stamp::of(path) == *stamp))
    }

    /// Remember the stamps of blobs that were just staged.
    pub(crate) fn record_stamps(&self, stamps: Vec<(PathBuf, Stamp)>) -> Result<(), BackupError> {
        self.extra_stamps
            .lock()
            .map_err(|_| BackupError::Internal("stamp list poisoned"))?
            .extend(stamps);
        Ok(())
    }

    /// Close the connection and its interrupt watcher, zeroize the keys,
    /// delete plaintext first, release `.lock`, then remove the session
    /// directory with a bounded retry (§10.3).
    pub fn close(mut self) -> CleanupReport {
        self.shutdown()
    }

    fn shutdown(&mut self) -> CleanupReport {
        if let Some(db) = self.db.take() {
            db.close();
        }
        self.keys = None;
        match self.staging.take() {
            Some(s) => s.close(),
            None => CleanupReport {
                removed: true,
                attempts: 0,
                last_error: None,
            },
        }
    }
}

/// A fallback only: `close()` is the normal path.
impl Drop for OpenBackup {
    fn drop(&mut self) {
        if self.db.is_some() || self.keys.is_some() || self.staging.is_some() {
            let report = self.shutdown();
            tracing::warn!(removed = report.removed, "backup dropped without close");
        }
    }
}
