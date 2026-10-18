//! Single-file extraction for M3 (§9). M2 does not call this in production;
//! the integration tests exercise it.
//!
//! Every destination is a sequential name inside the session (D10):
//! `x/<n>` for files and `db/<n>/<basename>` for databases, where the
//! basename has passed `oma_fs::sanitize_filename` unchanged (otherwise
//! `main.db` is used). No other string from the backup becomes a path.
#![deny(clippy::arithmetic_side_effects)]

use std::fs;
use std::io::BufWriter;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::cancel::{CancelToken, JobCtx, Phase};
use crate::cbc_stream::{decrypt_cbc_pkcs7_stream, CbcInput};
use crate::error::{BackupError, FileUnavailable, IoOp, MalformedDetail};
use crate::guard::{open_source_file, BlobDirGuard, OpenFailure};
use crate::keys::unwrap_file_key;
use crate::manifest_db::{
    open_staged, EntryKind, InterruptWatcher, ManifestEntry, OpenMode, INTERRUPT_POLL,
    STAGED_DB_LIMITS,
};
use crate::open::{copy_to_staging, partial_path, OpenBackup};
use crate::plists::BackupFile;
use crate::staging::{create_new_file, RemoveOnDrop};
use crate::validate::{blob_path, entry_is_consistent, BlobState};

/// A file extracted into staging. Borrows the [`OpenBackup`], so the path
/// cannot outlive the staging that holds it.
#[derive(Debug)]
pub struct ExtractedFile<'a> {
    /// Where the plaintext is.
    pub path: PathBuf,
    /// Its actual length (from the padding, for encrypted files).
    pub len: u64,
    /// `Size` from the record.
    pub declared_size: Option<u64>,
    /// `len == Size`, when `Size` is known. Differences are routine.
    pub size_matches: Option<bool>,
    _session: PhantomData<&'a OpenBackup>,
}

impl ExtractedFile<'_> {
    /// Delete the plaintext now, rather than when the session closes (for
    /// example once M3 has imported it).
    pub fn remove(self) -> Result<(), BackupError> {
        fs::remove_file(&self.path).map_err(|e| BackupError::staging_io(IoOp::Remove, &e))
    }
}

/// A database (and its `-wal`, if listed) extracted side by side, so SQLite
/// finds the WAL. Open it with [`StagedDatabase::open`], the hardened open;
/// never open `path` directly.
#[derive(Debug)]
pub struct StagedDatabase<'a> {
    /// The main database file.
    pub path: PathBuf,
    /// The `-wal`, when the backup lists one.
    pub wal: Option<PathBuf>,
    _session: PhantomData<&'a OpenBackup>,
}

impl<'a> StagedDatabase<'a> {
    /// Open the staged database with the hardening of §7.2 (`DEFENSIVE`,
    /// `TRUSTED_SCHEMA` off, `query_only`, in-memory temp storage, no mmap,
    /// SQLite limits) and an interrupt watcher on `cancel`. The connection
    /// borrows the [`OpenBackup`], so it is closed before `close()` deletes
    /// the file.
    pub fn open(&self, cancel: &CancelToken) -> Result<StagedConnection<'a>, BackupError> {
        let (conn, mode) = open_staged(&self.path, BackupFile::Blob, &STAGED_DB_LIMITS)?;
        let watcher =
            InterruptWatcher::start(conn.get_interrupt_handle(), cancel.clone(), INTERRUPT_POLL)?;
        Ok(StagedConnection {
            _watcher: watcher,
            conn,
            mode,
            _session: PhantomData,
        })
    }
}

/// A hardened, query-only connection to a staged database.
pub struct StagedConnection<'a> {
    // Field order is drop order: the watcher stops before the connection closes.
    _watcher: InterruptWatcher,
    conn: Connection,
    mode: OpenMode,
    _session: PhantomData<&'a OpenBackup>,
}

impl StagedConnection<'_> {
    /// The connection. `query_only` is on; use it for reads only.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// How the staged copy was opened.
    pub fn mode(&self) -> OpenMode {
        self.mode
    }
}

impl std::fmt::Debug for StagedConnection<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StagedConnection")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl OpenBackup {
    /// Extract one file into `<session>/x/<n>`.
    pub fn extract_file<'a>(
        &'a self,
        entry: &ManifestEntry,
        ctx: &mut JobCtx<'_>,
    ) -> Result<ExtractedFile<'a>, BackupError> {
        let session = self.session()?;
        let x = session.subdir(&["x"])?;
        let dest = x.join(session.next_index().to_string());
        self.extract_to(entry, &dest, ctx)
    }

    /// Extract a database and its `-wal` (never its `-shm`) into
    /// `<session>/db/<n>/`.
    pub fn extract_database<'a>(
        &'a self,
        domain: &str,
        relative_path: &str,
        ctx: &mut JobCtx<'_>,
    ) -> Result<StagedDatabase<'a>, BackupError> {
        let main = self
            .find_file(domain, relative_path)?
            .ok_or(BackupError::FileUnavailable(FileUnavailable::NotListed))?;
        let wal = self.find_file(domain, &format!("{relative_path}-wal"))?;

        let basename = relative_path.rsplit(['/', '\\']).next().unwrap_or_default();
        let wal_name = format!("{basename}-wal");
        let (main_name, wal_name) = if oma_fs::sanitize_filename(basename) == basename
            && oma_fs::sanitize_filename(&wal_name) == wal_name
        {
            (basename.to_owned(), wal_name)
        } else {
            // Files are matched by role, not by name, so a fixed pair is safe.
            ("main.db".to_owned(), "main.db-wal".to_owned())
        };

        let session = self.session()?;
        let n = session.next_index().to_string();
        let dir = session.subdir(&["db", &n])?;
        let path = dir.join(&main_name);
        self.extract_to(&main, &path, ctx)?;
        // A main database without its -wal is not handed out; remove it.
        let remove_main = RemoveOnDrop::new(&path);
        let wal = match wal {
            Some(w) => {
                let wal_path = dir.join(&wal_name);
                self.extract_to(&w, &wal_path, ctx)?;
                Some(wal_path)
            }
            None => None,
        };
        remove_main.disarm();
        Ok(StagedDatabase {
            path,
            wal,
            _session: PhantomData,
        })
    }

    fn extract_to<'a>(
        &'a self,
        entry: &ManifestEntry,
        dest: &Path,
        ctx: &mut JobCtx<'_>,
    ) -> Result<ExtractedFile<'a>, BackupError> {
        if entry.kind != EntryKind::File {
            return Err(BackupError::FileUnavailable(FileUnavailable::NotAFile));
        }
        if !entry_is_consistent(entry) {
            return Err(BackupError::FileUnavailable(FileUnavailable::InvalidEntry));
        }
        let root = self.inspection.dir().to_path_buf();
        let mut guard = BlobDirGuard::new(root.clone());
        match self.blob_state(&root, &mut guard, &entry.file_id)? {
            BlobState::Missing => {
                return Err(BackupError::FileUnavailable(FileUnavailable::BlobMissing))
            }
            BlobState::Unsafe => {
                return Err(BackupError::FileUnavailable(
                    FileUnavailable::BlobNotRegular,
                ))
            }
            BlobState::Present(_) => {}
        }
        let (mut src, len) = match open_source_file(&blob_path(&root, &entry.file_id)?) {
            Ok(f) => f,
            Err(OpenFailure::Missing) => {
                return Err(BackupError::FileUnavailable(FileUnavailable::BlobMissing))
            }
            Err(OpenFailure::NotRegular) => {
                return Err(BackupError::FileUnavailable(
                    FileUnavailable::BlobNotRegular,
                ))
            }
            Err(OpenFailure::Io(e)) => return Err(BackupError::source_io(IoOp::Open, &e)),
        };
        let declared_size = entry.record.as_ref().and_then(|r| r.size);

        let written = match &self.keys {
            None => copy_to_staging(
                &mut src,
                len,
                dest,
                BackupFile::Blob,
                ctx,
                Phase::Extracting,
                (0, Some(len)),
            )?,
            Some(keys) => {
                let record = entry
                    .record
                    .as_ref()
                    .ok_or(BackupError::FileUnavailable(FileUnavailable::NoKey))?;
                match record.wrapped_key() {
                    None => {
                        if declared_size.unwrap_or(0) > 0 || len > 0 {
                            return Err(BackupError::FileUnavailable(FileUnavailable::NoKey));
                        }
                        write_empty(dest)?
                    }
                    Some(wrapped) => {
                        let class = record.protection_class.ok_or(BackupError::FileUnavailable(
                            FileUnavailable::NoProtectionClass,
                        ))?;
                        if len == 0 {
                            if declared_size.unwrap_or(0) > 0 {
                                return Err(BackupError::malformed(
                                    BackupFile::Blob,
                                    MalformedDetail::Empty,
                                ));
                            }
                            write_empty(dest)?
                        } else {
                            let key =
                                unwrap_file_key(&keys.class_keys, &keys.keybag, class, wrapped)?;
                            let partial = partial_path(dest);
                            let out = create_new_file(&partial)
                                .map_err(|e| BackupError::staging_io(IoOp::Create, &e))?;
                            // Before the writer: the writer closes first.
                            let remove_partial = RemoveOnDrop::new(&partial);
                            let mut out = BufWriter::new(out);
                            let n = decrypt_cbc_pkcs7_stream(
                                &key,
                                &[0u8; 16],
                                CbcInput {
                                    src: &mut src,
                                    src_len: len,
                                    file: BackupFile::Blob,
                                },
                                &mut out,
                                ctx,
                                Phase::Extracting,
                            )?;
                            drop(out);
                            fs::rename(&partial, dest)
                                .map_err(|e| BackupError::staging_io(IoOp::Rename, &e))?;
                            remove_partial.disarm();
                            n
                        }
                    }
                }
            }
        };
        Ok(ExtractedFile {
            path: dest.to_path_buf(),
            len: written,
            declared_size,
            size_matches: declared_size.map(|s| s == written),
            _session: PhantomData,
        })
    }
}

fn write_empty(dest: &Path) -> Result<u64, BackupError> {
    create_new_file(dest).map_err(|e| BackupError::staging_io(IoOp::Create, &e))?;
    Ok(0)
}
