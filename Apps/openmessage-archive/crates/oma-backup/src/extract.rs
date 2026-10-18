//! Single-file and database extraction for M3 (§9; M3 §3).
//!
//! Every destination is a sequential name inside the session (D10):
//! `x/<n>` for files and `db/<n>/<basename>` for databases, where the
//! basename has passed `oma_fs::sanitize_filename` unchanged (otherwise
//! `main.db` is used). No other string from the backup becomes a path.
#![deny(clippy::arithmetic_side_effects)]

use std::fs;
use std::io::{self, BufWriter, Read, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::cancel::{CancelToken, JobCtx, Phase, Progress, ProgressSink};
use crate::cbc_stream::{decrypt_cbc_pkcs7_stream, CbcInput};
use crate::db_limits::StagedDbLimits;
use crate::error::{BackupError, FileUnavailable, IoOp, MalformedDetail};
use crate::guard::{open_source_file, BlobDirGuard, OpenFailure};
use crate::keys::unwrap_file_key;
use crate::limits::LimitKind;
use crate::manifest_db::{open_staged, EntryKind, InterruptWatcher, ManifestEntry, INTERRUPT_POLL};
use crate::open::{copy_to_staging, partial_path, OpenBackup, Stamp};
use crate::plists::BackupFile;
use crate::staged_conn::{ReadPolicy, StagedConnection};
use crate::staging::{create_new_file, RemoveOnDrop};
use crate::validate::{blob_path, entry_is_consistent, BlobState};
use crate::wal::{HeaderFacts, WalFacts};

mod sealed {
    /// Implemented only by the crate's file handles.
    pub trait Sealed {
        fn manifest_entry(&self) -> &crate::manifest_db::ManifestEntry;
    }
}

/// Something [`OpenBackup::extract_file`] accepts: a [`ManifestEntry`] or an
/// [`AttachmentKey`]. Sealed; it cannot be implemented outside the crate.
pub trait FileEntry: sealed::Sealed {}

impl sealed::Sealed for ManifestEntry {
    fn manifest_entry(&self) -> &ManifestEntry {
        self
    }
}
impl FileEntry for ManifestEntry {}

/// An opaque handle to a listed file, from [`OpenBackup::find_attachment`].
/// It can be probed with a [`crate::FileProber`] and extracted with
/// [`OpenBackup::extract_file`] without repeating the lookup. Its `Debug`
/// shows the entry kind only, never a domain, path or key.
#[derive(Clone)]
pub struct AttachmentKey(pub(crate) ManifestEntry);

impl std::fmt::Debug for AttachmentKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("AttachmentKey").field(&self.0.kind).finish()
    }
}

impl sealed::Sealed for AttachmentKey {
    fn manifest_entry(&self) -> &ManifestEntry {
        &self.0
    }
}
impl FileEntry for AttachmentKey {}

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

/// How [`OpenBackup::extract_database_with`] treats a listed `-wal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalPolicy {
    /// A `-wal` that is listed but cannot be provided fails the extraction
    /// (M2 behaviour, [`OpenBackup::extract_database`]).
    Required,
    /// A `-wal` that is listed but cannot be provided
    /// ([`BackupError::FileUnavailable`] only) is skipped and recorded in
    /// [`StagedDatabase::wal_outcome`]. Staging I/O errors, cancellation and
    /// limits still fail.
    BestEffort,
}

/// What happened to a database's `-wal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WalOutcome {
    /// The backup lists no `-wal`.
    NotListed,
    /// The `-wal` was staged beside the database.
    Staged {
        /// Its staged (plaintext) length.
        bytes: u64,
    },
    /// Listed but not provided (only under [`WalPolicy::BestEffort`]).
    /// Nothing was left on disk.
    Skipped(FileUnavailable),
}

/// SHA-256 of a staged database's plaintext: the main file, then its
/// `-wal` when staged. It identifies the exact content a pass read, so a
/// resume can be bound to it. Its `Debug` prints `<digest>`; it has no
/// `Display` and no serialization, so it never reaches a log or the UI.
#[derive(Clone, PartialEq, Eq)]
pub struct ContentDigest([u8; 32]);

impl ContentDigest {
    /// The raw digest, for binding a resume cursor to this content.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for ContentDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<digest>")
    }
}

/// A database (and its `-wal`, when staged) extracted side by side, so
/// SQLite finds the WAL. Open it with [`StagedDatabase::open_with`] or
/// [`StagedDatabase::open`], the hardened opens; never open `path` directly.
#[derive(Debug)]
pub struct StagedDatabase<'a> {
    /// The main database file.
    pub path: PathBuf,
    /// The staged `-wal`, when there is one.
    pub wal: Option<PathBuf>,
    /// What happened to the `-wal`.
    pub wal_outcome: WalOutcome,
    /// The main file's staged (plaintext) length.
    pub main_bytes: u64,
    /// The main file's length equals its Manifest `Size`, when known.
    pub size_matches: Option<bool>,
    /// The staged `-wal`'s length equals its Manifest `Size`, when staged
    /// and known.
    pub wal_size_matches: Option<bool>,
    digest: ContentDigest,
    _session: PhantomData<&'a OpenBackup>,
}

/// Adds a fixed offset and a combined total to every progress report, so
/// the main file and the `-wal` report one running total.
struct OffsetSink<'p> {
    inner: &'p mut dyn ProgressSink,
    base: u64,
    total: Option<u64>,
}

impl ProgressSink for OffsetSink<'_> {
    fn report(&mut self, p: Progress) {
        self.inner.report(Progress {
            phase: p.phase,
            done: self.base.saturating_add(p.done),
            total: self.total,
        });
    }
}

/// Feeds every byte read into a hasher.
struct HashingReader<'h, R> {
    inner: R,
    hasher: Option<&'h mut Sha256>,
}

impl<R: Read> Read for HashingReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if let Some(h) = self.hasher.as_deref_mut() {
            h.update(buf.get(..n).unwrap_or(&[]));
        }
        Ok(n)
    }
}

/// Feeds every byte written into a hasher.
struct HashingWriter<'h, W> {
    inner: W,
    hasher: Option<&'h mut Sha256>,
}

impl<W: Write> Write for HashingWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        if let Some(h) = self.hasher.as_deref_mut() {
            h.update(buf.get(..n).unwrap_or(&[]));
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The source side of one file to stage: its entry, blob path and length.
struct Source {
    entry: ManifestEntry,
    blob: PathBuf,
    len: u64,
}

impl<'a> StagedDatabase<'a> {
    /// Open with today's limits ([`StagedDbLimits::ceiling`]) and no denied
    /// columns. The connection borrows `self`, so the staged files cannot be
    /// removed while it is open.
    pub fn open<'s>(&'s self, cancel: &CancelToken) -> Result<StagedConnection<'s>, BackupError> {
        self.open_with(cancel, &StagedDbLimits::ceiling(), &ReadPolicy::default())
    }

    /// Open the staged database hardened (`DEFENSIVE`, `TRUSTED_SCHEMA` off,
    /// `query_only`, double-quoted strings, triggers and views off,
    /// in-memory temp storage, no mmap) with `limits`, an authorizer built
    /// from `policy`, a progress handler for op budgets and cancellation,
    /// and an interrupt watcher on `cancel`.
    ///
    /// **Schema-load retry:** if the schema cannot be loaded under `limits`,
    /// the open is retried once, still hardened, under the ceiling, and that
    /// connection is closed straight away. If it loads there, the result is
    /// [`BackupError::SchemaExceedsLimits`]; otherwise the original error
    /// stands. Error text is never inspected.
    pub fn open_with<'s>(
        &'s self,
        cancel: &CancelToken,
        limits: &StagedDbLimits,
        policy: &ReadPolicy,
    ) -> Result<StagedConnection<'s>, BackupError> {
        let (conn, mode) = match open_staged(&self.path, BackupFile::Blob, limits) {
            Ok(opened) => opened,
            Err(e) => {
                let ceiling = StagedDbLimits::ceiling();
                if *limits != ceiling && !matches!(e, BackupError::Cancelled) {
                    if let Ok((c, _)) = open_staged(&self.path, BackupFile::Blob, &ceiling) {
                        let _ = c.close();
                        return Err(BackupError::SchemaExceedsLimits);
                    }
                }
                return Err(e);
            }
        };
        let watcher =
            InterruptWatcher::start(conn.get_interrupt_handle(), cancel.clone(), INTERRUPT_POLL)?;
        StagedConnection::new(conn, mode, watcher, cancel, policy)
    }

    /// SHA-256 of the staged plaintext (main file, then `-wal`).
    pub fn content_digest(&self) -> &ContentDigest {
        &self.digest
    }

    /// The main file's header facts (bytes 16–19; sqlite.org/fileformat2.html).
    pub fn header_facts(&self) -> Result<HeaderFacts, BackupError> {
        let f = fs::File::open(&self.path).map_err(|e| BackupError::staging_io(IoOp::Open, &e))?;
        crate::wal::header_facts(f).map_err(|e| BackupError::staging_io(IoOp::Read, &e))
    }

    /// Facts about the staged `-wal`: header, salts and cumulative frame
    /// checksums. `None` when no `-wal` was staged. The scan reads the whole
    /// `-wal` (up to the staging limit), so it checks `cancel` before every
    /// frame and returns [`BackupError::Cancelled`] once it is set.
    pub fn wal_facts(&self, cancel: &CancelToken) -> Result<Option<WalFacts>, BackupError> {
        let Some(wal) = &self.wal else {
            return Ok(None);
        };
        cancel.check()?;
        let page_size = self.header_facts()?.page_size;
        let f = fs::File::open(wal).map_err(|e| BackupError::staging_io(IoOp::Open, &e))?;
        crate::wal::wal_facts(io::BufReader::new(f), page_size, cancel).map(Some)
    }

    /// Delete the staged main file, its `-wal`, and any `-shm` SQLite
    /// created, now rather than when the session closes. It consumes the
    /// database, so it cannot be called while a connection borrows it:
    ///
    /// ```compile_fail,E0505
    /// # fn f(ob: &oma_backup::OpenBackup, ctx: &mut oma_backup::JobCtx<'_>)
    /// #     -> Result<(), oma_backup::BackupError> {
    /// let staged = ob.extract_database("HomeDomain", "Library/SMS/sms.db", ctx)?;
    /// let conn = staged.open(&oma_backup::CancelToken::new())?;
    /// staged.remove()?; // error: `staged` is borrowed by `conn`
    /// drop(conn);
    /// # Ok(()) }
    /// ```
    ///
    /// Dropping the connection first compiles:
    ///
    /// ```no_run
    /// # fn f(ob: &oma_backup::OpenBackup, ctx: &mut oma_backup::JobCtx<'_>)
    /// #     -> Result<(), oma_backup::BackupError> {
    /// let staged = ob.extract_database("HomeDomain", "Library/SMS/sms.db", ctx)?;
    /// let conn = staged.open(&oma_backup::CancelToken::new())?;
    /// drop(conn);
    /// staged.remove()?;
    /// # Ok(()) }
    /// ```
    ///
    /// Every file is attempted; the first failure is returned. A failure is
    /// not fatal for the caller: whatever remains goes with the session at
    /// `close()`.
    pub fn remove(self) -> Result<(), BackupError> {
        let mut first: Option<io::Error> = None;
        let mut shm = self.path.as_os_str().to_os_string();
        shm.push("-shm");
        let mut wal = self.path.as_os_str().to_os_string();
        wal.push("-wal");
        for p in [self.path.clone(), PathBuf::from(wal), PathBuf::from(shm)] {
            match fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(error = ?e.kind(), "staged database file not removed");
                    first.get_or_insert(e);
                }
            }
        }
        match first {
            None => Ok(()),
            Some(e) => Err(BackupError::staging_io(IoOp::Remove, &e)),
        }
    }
}

impl OpenBackup {
    /// Look up a file by domain and relative path and return an opaque key
    /// for it. `Ambiguous` and `InvalidEntry` stay
    /// `Err(FileUnavailable(..))`, as in [`OpenBackup::find_file`].
    pub fn find_attachment(
        &self,
        domain: &str,
        relative_path: &str,
    ) -> Result<Option<AttachmentKey>, BackupError> {
        Ok(self.find_file(domain, relative_path)?.map(AttachmentKey))
    }

    /// Extract one file into `<session>/x/<n>`. Takes a [`ManifestEntry`] or
    /// an [`AttachmentKey`].
    pub fn extract_file<'a, E: FileEntry + ?Sized>(
        &'a self,
        entry: &E,
        ctx: &mut JobCtx<'_>,
    ) -> Result<ExtractedFile<'a>, BackupError> {
        let entry = sealed::Sealed::manifest_entry(entry);
        let session = self.session()?;
        let x = session.subdir(&["x"])?;
        let dest = x.join(session.next_index().to_string());
        let len = self.source(entry)?.len;
        self.extract_to(entry, &dest, ctx, (0, Some(len)), None)
    }

    /// Extract a database and its `-wal` (never its `-shm`) into
    /// `<session>/db/<n>/`. A listed `-wal` that cannot be provided fails
    /// the extraction: this is [`OpenBackup::extract_database_with`] with
    /// [`WalPolicy::Required`].
    pub fn extract_database<'a>(
        &'a self,
        domain: &str,
        relative_path: &str,
        ctx: &mut JobCtx<'_>,
    ) -> Result<StagedDatabase<'a>, BackupError> {
        self.extract_database_with(domain, relative_path, WalPolicy::Required, ctx)
    }

    /// Extract a database and its `-wal` into `<session>/db/<n>/`.
    ///
    /// - The source blobs' combined length is checked against
    ///   [`crate::Limits::sms_db_bytes`] before anything is copied.
    /// - Each source blob is stamped (length and modification time) before
    ///   copying and again after; a difference fails with
    ///   [`BackupError::SourceChanged`] and removes the copies. The stamps
    ///   are kept, so [`OpenBackup::unchanged_since_open`] covers them.
    /// - SHA-256 of the staged plaintext is computed while copying
    ///   ([`StagedDatabase::content_digest`]).
    /// - Progress is one running total over both files.
    pub fn extract_database_with<'a>(
        &'a self,
        domain: &str,
        relative_path: &str,
        wal_policy: WalPolicy,
        ctx: &mut JobCtx<'_>,
    ) -> Result<StagedDatabase<'a>, BackupError> {
        let main_entry = self
            .find_file(domain, relative_path)?
            .ok_or(BackupError::FileUnavailable(FileUnavailable::NotListed))?;
        let main = self.source(&main_entry)?;

        let mut skipped: Option<FileUnavailable> = None;
        let wal_source = match self.find_file(domain, &format!("{relative_path}-wal")) {
            Ok(None) => None,
            Ok(Some(e)) => match self.source(&e) {
                Ok(s) => Some(s),
                Err(BackupError::FileUnavailable(f)) if wal_policy == WalPolicy::BestEffort => {
                    skipped = Some(f);
                    None
                }
                Err(e) => return Err(e),
            },
            Err(BackupError::FileUnavailable(f)) if wal_policy == WalPolicy::BestEffort => {
                skipped = Some(f);
                None
            }
            Err(e) => return Err(e),
        };

        let total = main
            .len
            .saturating_add(wal_source.as_ref().map_or(0, |w| w.len));
        if total > self.limits.sms_db_bytes {
            return Err(BackupError::limit(
                LimitKind::SmsDbBytes,
                total,
                self.limits.sms_db_bytes,
            ));
        }

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

        let mut stamps = vec![(main.blob.clone(), Stamp::of(&main.blob))];
        if let Some(w) = &wal_source {
            stamps.push((w.blob.clone(), Stamp::of(&w.blob)));
        }

        let mut hasher = Sha256::new();
        ctx.report(Phase::Extracting, 0, Some(total));
        let main_file =
            self.extract_to(&main.entry, &path, ctx, (0, Some(total)), Some(&mut hasher))?;
        // A main database without its -wal, or from a changed source, is not
        // handed out; remove it.
        let remove_main = RemoveOnDrop::new(&path);
        let mut remove_wal = None;
        let (wal, wal_outcome, wal_size_matches) = match &wal_source {
            Some(w) => {
                let wal_path = dir.join(&wal_name);
                match self.extract_to(
                    &w.entry,
                    &wal_path,
                    ctx,
                    (main.len, Some(total)),
                    Some(&mut hasher),
                ) {
                    Ok(x) => {
                        remove_wal = Some(RemoveOnDrop::new(&wal_path));
                        (
                            Some(wal_path),
                            WalOutcome::Staged { bytes: x.len },
                            x.size_matches,
                        )
                    }
                    // Key and entry problems are found before anything is
                    // written, so nothing is left on disk.
                    Err(BackupError::FileUnavailable(f)) if wal_policy == WalPolicy::BestEffort => {
                        (None, WalOutcome::Skipped(f), None)
                    }
                    Err(e) => return Err(e),
                }
            }
            None => match skipped {
                Some(f) => (None, WalOutcome::Skipped(f), None),
                None => (None, WalOutcome::NotListed, None),
            },
        };

        if stamps.iter().any(|(p, s)| Stamp::of(p) != *s) {
            return Err(BackupError::SourceChanged);
        }
        self.record_stamps(stamps)?;
        remove_main.disarm();
        if let Some(r) = remove_wal {
            r.disarm();
        }
        tracing::info!(wal = wal_outcome_kind(&wal_outcome), "database staged");
        Ok(StagedDatabase {
            path,
            wal,
            wal_outcome,
            main_bytes: main_file.len,
            size_matches: main_file.size_matches,
            wal_size_matches,
            digest: ContentDigest(hasher.finalize().into()),
            _session: PhantomData,
        })
    }

    /// Check an entry and find its blob, without opening it.
    fn source(&self, entry: &ManifestEntry) -> Result<Source, BackupError> {
        if entry.kind != EntryKind::File {
            return Err(BackupError::FileUnavailable(FileUnavailable::NotAFile));
        }
        if !entry_is_consistent(entry) {
            return Err(BackupError::FileUnavailable(FileUnavailable::InvalidEntry));
        }
        let root = self.inspection.dir().to_path_buf();
        let mut guard = BlobDirGuard::new(root.clone());
        let len = match self.blob_state(&root, &mut guard, &entry.file_id)? {
            BlobState::Missing => {
                return Err(BackupError::FileUnavailable(FileUnavailable::BlobMissing))
            }
            BlobState::Unsafe => {
                return Err(BackupError::FileUnavailable(
                    FileUnavailable::BlobNotRegular,
                ))
            }
            BlobState::Present(len) => len,
        };
        Ok(Source {
            entry: entry.clone(),
            blob: blob_path(&root, &entry.file_id)?,
            len,
        })
    }

    /// Copy or decrypt one file to `dest`. `progress` is the offset and the
    /// total reported; `hasher`, when given, receives the plaintext.
    fn extract_to<'a>(
        &'a self,
        entry: &ManifestEntry,
        dest: &Path,
        ctx: &mut JobCtx<'_>,
        progress: (u64, Option<u64>),
        hasher: Option<&mut Sha256>,
    ) -> Result<ExtractedFile<'a>, BackupError> {
        let source = self.source(entry)?;
        let (src, len) = match open_source_file(&source.blob) {
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
                &mut HashingReader { inner: src, hasher },
                len,
                dest,
                BackupFile::Blob,
                ctx,
                Phase::Extracting,
                progress,
            )?,
            Some(keys) => {
                let mut src = src;
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
                            let mut out = HashingWriter {
                                inner: BufWriter::new(out),
                                hasher,
                            };
                            let mut sink = OffsetSink {
                                inner: &mut *ctx.progress,
                                base: progress.0,
                                total: progress.1,
                            };
                            let mut offset_ctx = JobCtx::new(ctx.cancel, &mut sink);
                            let n = decrypt_cbc_pkcs7_stream(
                                &key,
                                &[0u8; 16],
                                CbcInput {
                                    src: &mut src,
                                    src_len: len,
                                    file: BackupFile::Blob,
                                },
                                &mut out,
                                &mut offset_ctx,
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

/// The outcome's kind, for logs.
fn wal_outcome_kind(o: &WalOutcome) -> &'static str {
    match o {
        WalOutcome::NotListed => "not-listed",
        WalOutcome::Staged { .. } => "staged",
        WalOutcome::Skipped(_) => "skipped",
    }
}

fn write_empty(dest: &Path) -> Result<u64, BackupError> {
    create_new_file(dest).map_err(|e| BackupError::staging_io(IoOp::Create, &e))?;
    Ok(0)
}
