//! The content-addressed attachment store (spec §9): the parts M4 needs
//! before the import exists.
//!
//! - [`StorageName`]: a `blob.storage_name` read from the database is
//!   parsed against `^[0-9a-f]{64}$` before any path is built from it (I2).
//! - [`sweep_incoming`]: removes `attachments/.incoming/<n>.partial` files a
//!   crash left behind (`begin_write`, §5.3).
//! - [`reconcile_intents`]: for each `blob_intent`, deletes the placed file
//!   if no `blob` row claims it, flushes its directory, then deletes the
//!   intent (§9.1, "Reconcile").
//! - [`store_dir`]: the one check every store path goes through before a
//!   file under `attachments/<hh>/` is touched.
//! - [`Partial`]: one copy in flight, `attachments/.incoming/<n>.partial`,
//!   created new (0600 on Unix), synced and length-checked before it is
//!   placed, and removed (with retries on Windows) when it is not.
//! - [`Placer`]: phase C of the copy procedure (§9.1): the no-replace
//!   placement, deduplication against a referenced or just-placed file,
//!   adoption of an unreferenced file that re-hashes to its name, and the
//!   replacement of one that does not; then one flush per touched `<hh>`
//!   directory.
//!
//! - [`BlobReader`] and [`open_checked`]: the one way attachment bytes are
//!   read back (`open_blob`, the full verifier; spec §4.3, §9.5): the
//!   name parsed, the directories checked, the file opened without
//!   following a link, its length compared with `byte_len`, and its
//!   content re-hashed while it streams.
//! - [`RangeReader`] and [`open_range`] (M5 §5.4): the viewer's random
//!   access to a file `verify_blob` has hashed, with the same checks as
//!   [`open_checked`] plus a comparison of the file's identity
//!   ([`FileIdentity`]) with the one recorded at verification; and
//!   [`read_sniff_window`], the first 4 KiB for the sniffer, the one read
//!   that needs no verification. These and [`open_checked`] are the only
//!   readers of store files.
//! - [`walk_store`]: the verifier's bounded walk of `attachments/`.
//!
//! The pass that drives the phases (A to D) is `import/attachments.rs`.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use oma_fs::{check_private, Expect, PermProblem};
use rusqlite::TransactionBehavior;
use sha2::{Digest, Sha256};

use crate::conn::WriteConn;
use crate::error::{ArchiveError, IoOp, Stage};
use crate::fault::{point, FaultPoint};
use crate::id::to_hex;
use crate::layout::ArchiveDir;
use crate::store_fs::{Placed, StoreFs};

/// A storage name: 64 lowercase hex characters (name scheme 1).
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct StorageName(String);

impl std::fmt::Debug for StorageName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StorageName(..)")
    }
}

impl StorageName {
    /// Parse a stored name. Anything but 64 lowercase hex characters is
    /// refused, so `..`, separators and upper case never reach a path (the
    /// schema's CHECK is not trusted on a crafted file).
    pub(crate) fn parse(s: &str) -> Option<StorageName> {
        let ok = s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        ok.then(|| StorageName(s.to_owned()))
    }

    /// The name scheme 1 name of a SHA-256 digest.
    pub(crate) fn of_sha256(sha: &[u8]) -> Option<StorageName> {
        if sha.len() == 32 {
            Some(StorageName(to_hex(sha)))
        } else {
            None
        }
    }

    /// `attachments/<first two>/<name>`.
    pub(crate) fn path_in(&self, dir: &ArchiveDir) -> std::path::PathBuf {
        let hh = self.0.get(..2).unwrap_or("00");
        dir.store_file(hh, &self.0)
    }

    /// The name itself.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// `attachments/<first two>`.
    fn dir_in(&self, dir: &ArchiveDir) -> PathBuf {
        let hh = self.0.get(..2).unwrap_or("00");
        dir.attachments().join(hh)
    }

    /// The 32 digest bytes the name spells.
    pub(crate) fn sha256(&self) -> Option<[u8; 32]> {
        let mut out = [0u8; 32];
        for (o, pair) in out.iter_mut().zip(self.0.as_bytes().chunks(2)) {
            let s = std::str::from_utf8(pair).ok()?;
            *o = u8::from_str_radix(s, 16).ok()?;
        }
        Some(out)
    }
}

/// `true` for `^[0-9]+\.partial$`.
fn is_partial_name(name: &str) -> bool {
    name.strip_suffix(".partial")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// What a sweep did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Swept {
    pub(crate) removed: u32,
    /// Matching files that could not be removed.
    pub(crate) left: u32,
}

/// Remove a regular, non-link file; anything else is left alone.
pub(crate) fn remove_regular(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && !m.file_type().is_symlink() => {
            fs::remove_file(path)?;
            Ok(true)
        }
        Ok(_) => Ok(false),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Remove every regular, non-link file in `dir` whose name `matches`.
/// Other entries are left alone. At most `limit` entries are looked at.
pub(crate) fn sweep_matching(
    dir: &Path,
    matches: fn(&str) -> bool,
    limit: usize,
) -> Result<Swept, ArchiveError> {
    let mut swept = Swept::default();
    let entries = fs::read_dir(dir).map_err(|e| ArchiveError::io(IoOp::ReadDir, &e))?;
    for entry in entries.take(limit) {
        let entry = entry.map_err(|e| ArchiveError::io(IoOp::ReadDir, &e))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !matches(name) {
            continue;
        }
        match remove_regular(&entry.path()) {
            Ok(true) => swept.removed = swept.removed.saturating_add(1),
            Ok(false) => {}
            Err(_) => swept.left = swept.left.saturating_add(1),
        }
    }
    Ok(swept)
}

/// The most entries a sweep looks at (bounded work on a planted tree).
pub(crate) const SWEEP_LIMIT: usize = 100_000;

/// Sweep `attachments/.incoming/`.
pub(crate) fn sweep_incoming(dir: &ArchiveDir) -> Result<Swept, ArchiveError> {
    sweep_matching(&dir.incoming(), is_partial_name, SWEEP_LIMIT)
}

/// What [`reconcile_intents`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Reconciled {
    /// Intents whose blob row exists: dropped.
    pub(crate) committed: u32,
    /// Intents without a blob row: placed file removed (if present).
    pub(crate) rolled_back: u32,
    /// Placed files that could not be removed, or whose removal could not
    /// be made durable, or whose `<hh>` directory is not a private real
    /// directory: the intent stays.
    pub(crate) files_left: u32,
}

/// The fan-out directory `attachments/<hh>/` of `name`, if it is safe to
/// work in: `attachments/` and `attachments/<hh>/` are each a real
/// directory (not a link, junction or other reparse point) and, on Unix,
/// owned by the effective user with no group or other bits
/// ([`oma_fs::check_private`]). `Ok(None)` when `<hh>` does not exist.
/// Every store path is resolved through this check before a file in it is
/// touched, so a planted link never leads outside the archive.
pub(crate) fn store_dir(
    dir: &ArchiveDir,
    name: &StorageName,
) -> Result<Option<std::path::PathBuf>, PermProblem> {
    check_private(&dir.attachments(), Expect::Dir)?;
    let file = name.path_in(dir);
    let Some(hh) = file.parent() else {
        return Err(PermProblem::WrongType {
            expected: Expect::Dir,
        });
    };
    match check_private(hh, Expect::Dir) {
        Ok(()) => Ok(Some(hh.to_path_buf())),
        Err(PermProblem::Io(io::ErrorKind::NotFound)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// What one intent comes to.
enum IntentEnd {
    /// The blob row exists; the intent is dropped.
    Committed,
    /// The placed file is gone (removed now, or never there); the intent is
    /// dropped once `hh` (if any) is flushed.
    RolledBack { hh: Option<std::path::PathBuf> },
    /// Left as it is: the intent stays.
    Left,
}

/// The reconcile of §9.1, in one transaction, in the store's order: remove
/// the placed files, flush each `<hh>` directory a file was removed from
/// (through `store`), and only then delete the intents and commit. A power
/// loss can then never keep the intent's deletion but lose the unlink.
pub(crate) fn reconcile_intents(
    w: &mut WriteConn,
    dir: &ArchiveDir,
    store: &dyn StoreFs,
) -> Result<Reconciled, ArchiveError> {
    let sql = |e: rusqlite::Error| ArchiveError::sqlite(Stage::Recovery, &e);
    let tx =
        rusqlite::Transaction::new(w.conn_mut(), TransactionBehavior::Immediate).map_err(sql)?;
    let intents: Vec<(Vec<u8>, i64, bool)> = {
        let mut stmt = tx
            .prepare(
                "SELECT i.sha256, i.session_id, \
                        EXISTS (SELECT 1 FROM blob b WHERE b.sha256 = i.sha256) \
                 FROM blob_intent i",
            )
            .map_err(sql)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)?
    };

    // 1. The file operations.
    let mut ends = Vec::with_capacity(intents.len());
    let mut to_flush: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    for (sha, _, has_blob) in &intents {
        if *has_blob {
            ends.push(IntentEnd::Committed);
            continue;
        }
        // A stored intent always has 32 bytes (the schema checks it), but a
        // crafted file is not trusted: without a name there is no file.
        let end = match StorageName::of_sha256(sha) {
            None => IntentEnd::RolledBack { hh: None },
            Some(name) => match store_dir(dir, &name) {
                Ok(None) => IntentEnd::RolledBack { hh: None },
                Ok(Some(hh)) => match remove_regular(&name.path_in(dir)) {
                    Ok(true) => {
                        to_flush.insert(hh.clone());
                        IntentEnd::RolledBack { hh: Some(hh) }
                    }
                    Ok(false) => IntentEnd::RolledBack { hh: None },
                    Err(_) => IntentEnd::Left,
                },
                Err(_) => IntentEnd::Left,
            },
        };
        ends.push(end);
    }

    // 2. Flush every directory a file was removed from.
    let mut unflushed: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    for hh in to_flush {
        if store.fsync_dir(&hh).is_err() {
            unflushed.insert(hh);
        }
    }

    // 3. The intents, then the commit.
    let mut out = Reconciled::default();
    for ((sha, session, _), end) in intents.iter().zip(ends) {
        match end {
            IntentEnd::Committed => out.committed = out.committed.saturating_add(1),
            IntentEnd::RolledBack { hh } if hh.as_ref().is_some_and(|h| unflushed.contains(h)) => {
                out.files_left = out.files_left.saturating_add(1);
                continue;
            }
            IntentEnd::RolledBack { .. } => out.rolled_back = out.rolled_back.saturating_add(1),
            IntentEnd::Left => {
                out.files_left = out.files_left.saturating_add(1);
                continue;
            }
        }
        tx.execute(
            "DELETE FROM blob_intent WHERE sha256 = ?1 AND session_id = ?2",
            rusqlite::params![sha, session],
        )
        .map_err(sql)?;
    }
    tx.commit().map_err(sql)?;
    Ok(out)
}

/// What a store file is, by `symlink_metadata` (nothing is followed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoreFileState {
    /// A regular file (not a link) of this length.
    Regular(u64),
    /// Nothing is there (or its `<hh>` directory does not exist).
    Missing,
    /// Something else: a link, a directory, a device, an unreadable entry,
    /// or a `<hh>` directory that is not a private real directory.
    Unusable,
}

/// The state of the store file `name` (spec §9.1 phase A: "a regular,
/// non-link file of length `byte_len`").
pub(crate) fn store_file_state(dir: &ArchiveDir, name: &StorageName) -> StoreFileState {
    match store_dir(dir, name) {
        Ok(Some(_)) => match fs::symlink_metadata(name.path_in(dir)) {
            Ok(m) if m.is_file() && !m.file_type().is_symlink() => StoreFileState::Regular(m.len()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => StoreFileState::Missing,
            _ => StoreFileState::Unusable,
        },
        Ok(None) => StoreFileState::Missing,
        Err(_) => StoreFileState::Unusable,
    }
}

/// How often a partial file's removal is tried on Windows, where a scanner
/// may hold it open for a moment (spec §9.1 step 5).
const REMOVE_ATTEMPTS: u32 = 5;
/// The wait between those attempts.
const REMOVE_RETRY_WAIT: std::time::Duration = std::time::Duration::from_millis(20);
/// How many names `<n>.partial` are tried before giving up.
const PARTIAL_NAME_ATTEMPTS: u32 = 64;
/// The read buffer of a re-hash.
const HASH_BUFFER: usize = 64 << 10;

/// Remove `path` through `store`; a missing file counts as removed. On
/// Windows a failure is retried a few times. `false` when it is still
/// there.
fn remove_with_retries(store: &dyn StoreFs, path: &Path) -> bool {
    let mut attempt = 1u32;
    loop {
        match store.remove_file(path) {
            Ok(()) => return true,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return true,
            Err(_) if cfg!(windows) && attempt < REMOVE_ATTEMPTS => {
                attempt = attempt.saturating_add(1);
                std::thread::sleep(REMOVE_RETRY_WAIT);
            }
            Err(_) => return false,
        }
    }
}

/// One attachment copy in flight: `attachments/.incoming/<n>.partial`.
/// Callers place it ([`Placer::place`]) or remove it ([`Partial::remove`],
/// which counts a file it cannot remove). Dropping one that was neither
/// (an early return, a panic) closes the file and makes one best-effort
/// attempt to remove it, so a decrypted copy is not left behind until the
/// next sweep (spec §9.1 step 5); a file that attempt cannot remove is
/// still swept by the next write open.
pub(crate) struct Partial {
    /// `None` once the file was placed or a removal was attempted.
    path: Option<PathBuf>,
    file: Option<fs::File>,
}

impl std::fmt::Debug for Partial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Partial(<path>)")
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        self.file = None;
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

impl Partial {
    /// Create the next free `<n>.partial` with `create_new` (0600 on Unix,
    /// no extra attributes on Windows). `next` is the caller's counter.
    pub(crate) fn create(dir: &ArchiveDir, next: &mut u64) -> io::Result<Partial> {
        for _ in 0..PARTIAL_NAME_ATTEMPTS {
            let path = dir.partial(*next);
            *next = next.wrapping_add(1);
            match oma_fs::create_new_private(&path) {
                Ok(file) => {
                    return Ok(Partial {
                        path: Some(path),
                        file: Some(file),
                    })
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "no free partial file name",
        ))
    }

    /// The partial file's path, while it is still in `.incoming`.
    fn path(&self) -> io::Result<&Path> {
        self.path
            .as_deref()
            .ok_or_else(|| io::Error::other("the partial file is gone"))
    }

    /// The open file to write the copy into.
    pub(crate) fn file(&mut self) -> io::Result<&mut fs::File> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("the partial file is closed"))
    }

    /// Flush the file to the device (`sync_all`, through `store` so a test
    /// sees it), check that it holds `len` bytes, and close it (spec §9.1
    /// step 4).
    pub(crate) fn finish(&mut self, store: &dyn StoreFs, len: u64) -> io::Result<()> {
        let path = self.path()?.to_path_buf();
        let file = self.file()?;
        store.sync_file(file, &path)?;
        let on_disk = file.metadata()?.len();
        self.file = None;
        if on_disk != len {
            return Err(io::Error::other("the partial file has the wrong length"));
        }
        Ok(())
    }

    /// The file was moved into the store: nothing is left to remove.
    fn placed(mut self) {
        self.file = None;
        self.path = None;
    }

    /// Remove the partial file (retried on Windows). `false` when it is
    /// still there; the next write open sweeps it.
    pub(crate) fn remove(mut self, store: &dyn StoreFs) -> bool {
        self.file = None;
        match self.path.take() {
            Some(path) => remove_with_retries(store, &path),
            None => true,
        }
    }
}

/// SHA-256 of a store file, opened without following a link and checked
/// to be a regular file. `None` when it cannot be read as one.
fn hash_store_file(path: &Path) -> Option<[u8; 32]> {
    let mut f = oma_fs::open_nofollow(path).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; HASH_BUFFER];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(buf.get(..n)?);
    }
    Some(h.finalize().into())
}

/// The length of a store file opened without following a link, if it is a
/// regular file.
fn regular_len(path: &Path) -> Option<u64> {
    oma_fs::open_nofollow(path)
        .and_then(|f| f.metadata())
        .ok()
        .map(|m| m.len())
}

/// An attachment file opened for reading (spec §4.3 `open_blob`, §9.5).
///
/// It yields at most the row's `byte_len` bytes and hashes them as they
/// stream. At the end of the file it checks that exactly `byte_len` bytes
/// came and that they hash to the row's `sha256`; otherwise that final
/// read fails with [`std::io::ErrorKind::InvalidData`] (and so does every
/// read after it). A reader that stops early gets no such check, so a
/// caller that must trust the bytes reads to the end before using them.
pub struct BlobReader {
    file: io::Take<fs::File>,
    len: u64,
    read: u64,
    hasher: Sha256,
    expected: [u8; 32],
    end: Option<bool>,
}

impl std::fmt::Debug for BlobReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobReader")
            .field("len", &self.len)
            .field("read", &self.read)
            .finish_non_exhaustive()
    }
}

impl BlobReader {
    /// The file's length, which equals the row's `byte_len`.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// `true` for an empty file.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The digest every byte read so far was checked against, once the
    /// reader has returned `Ok(0)` with the length and the digest both
    /// matching; `None` before that, or after a mismatch. An export copy
    /// records this instead of hashing the copy a second time (M6 spec
    /// §6.1, "one hash").
    pub fn verified_digest(&self) -> Option<[u8; 32]> {
        (self.end == Some(true)).then_some(self.expected)
    }

    /// The identity of the open file (M5 §5.4).
    pub(crate) fn identity(&self) -> io::Result<FileIdentity> {
        Ok(FileIdentity::of(&self.file.get_ref().metadata()?))
    }

    fn mismatch() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the attachment file does not match its digest",
        )
    }
}

impl Read for BlobReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.end {
            Some(true) => return Ok(0),
            Some(false) => return Err(BlobReader::mismatch()),
            None => {}
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let n = self.file.read(buf)?;
        if n == 0 {
            let digest: [u8; 32] = std::mem::take(&mut self.hasher).finalize().into();
            let ok = self.read == self.len && digest == self.expected;
            self.end = Some(ok);
            return if ok {
                Ok(0)
            } else {
                Err(BlobReader::mismatch())
            };
        }
        let chunk = buf
            .get(..n)
            .ok_or_else(|| io::Error::other("short buffer"))?;
        self.hasher.update(chunk);
        self.read = self
            .read
            .saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
        Ok(n)
    }
}

/// The checks every reader of a store file makes (spec §9.5): `attachments/`
/// and `<hh>/` must be private real directories, the file is opened with
/// `open_nofollow` and must be a regular file of `byte_len` bytes. Returns
/// the open file and its metadata.
fn open_store_file(
    dir: &ArchiveDir,
    name: &StorageName,
    byte_len: i64,
) -> Result<(fs::File, fs::Metadata), crate::error::BlobProblem> {
    use crate::error::BlobProblem;
    match store_dir(dir, name) {
        Ok(Some(_)) => {}
        Ok(None) => return Err(BlobProblem::Missing),
        Err(_) => return Err(BlobProblem::NotRegular),
    }
    let file = match oma_fs::open_nofollow(&name.path_in(dir)) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(BlobProblem::Missing),
        Err(e) if oma_fs::open_refusal(&e).is_some() => return Err(BlobProblem::NotRegular),
        Err(e) => return Err(BlobProblem::Io(e.kind())),
    };
    let meta = file.metadata().map_err(|e| BlobProblem::Io(e.kind()))?;
    if u64::try_from(byte_len).ok() != Some(meta.len()) {
        return Err(BlobProblem::LengthMismatch);
    }
    Ok((file, meta))
}

/// Open the store file `name` for a row of `byte_len` bytes (spec §9.5):
/// `attachments/` and `<hh>/` must be private real directories, the file is
/// opened with `open_nofollow` and must be a regular file of `byte_len`
/// bytes. The returned reader re-hashes what it yields against the digest
/// the name spells.
pub(crate) fn open_checked(
    dir: &ArchiveDir,
    name: &StorageName,
    byte_len: i64,
) -> Result<BlobReader, crate::error::BlobProblem> {
    use crate::error::BlobProblem;
    let expected = name.sha256().ok_or(BlobProblem::BadName)?;
    let (file, meta) = open_store_file(dir, name, byte_len)?;
    let len = meta.len();
    Ok(BlobReader {
        file: file.take(len),
        len,
        read: 0,
        hasher: Sha256::new(),
        expected,
        end: None,
    })
}

/// The identity of a store file (M5 §5.4): on Unix `(dev, ino, len,
/// mtime)`, on Windows `(len, mtime)` only (the file index needs an
/// unstable API or `unsafe`; M5 §20 item 1). `Debug` prints nothing of it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    len: u64,
    mtime: Option<SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

impl std::fmt::Debug for FileIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FileIdentity(..)")
    }
}

impl FileIdentity {
    pub(crate) fn of(m: &fs::Metadata) -> FileIdentity {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        FileIdentity {
            len: m.len(),
            mtime: m.modified().ok(),
            #[cfg(unix)]
            dev: m.dev(),
            #[cfg(unix)]
            ino: m.ino(),
        }
    }

    /// The length it was recorded with.
    pub(crate) fn len(&self) -> u64 {
        self.len
    }
}

/// The first [`crate::media::SNIFF_WINDOW`] bytes of the store file `name`
/// (M5 §5.2), after [`open_checked`]'s checks. Not hashed: these bytes go
/// only to the sniffer, and are the one read of an unverified file the
/// viewer makes.
pub(crate) fn read_sniff_window(
    dir: &ArchiveDir,
    name: &StorageName,
    byte_len: i64,
) -> Result<Vec<u8>, crate::error::BlobProblem> {
    use crate::error::BlobProblem;
    let (file, meta) = open_store_file(dir, name, byte_len)?;
    let window = u64::try_from(crate::media::SNIFF_WINDOW).unwrap_or(4096);
    let want = meta.len().min(window);
    let mut out = Vec::with_capacity(usize::try_from(want).unwrap_or(0));
    file.take(want)
        .read_to_end(&mut out)
        .map_err(|e| BlobProblem::Io(e.kind()))?;
    Ok(out)
}

/// Random access to a store file that `verify_blob` hashed (M5 §5.4).
/// Opened per request by [`open_range`] and dropped after it. `Debug`
/// prints the length only.
pub struct RangeReader {
    file: fs::File,
    len: u64,
}

impl std::fmt::Debug for RangeReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RangeReader")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl RangeReader {
    /// The file's length, which equals the row's `byte_len`.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// `true` for an empty file.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Read from `offset` into `buf`: `min(buf.len(), len - offset)` bytes,
    /// all of them. An `offset` at or past the end is refused
    /// ([`io::ErrorKind::InvalidInput`]); a file that ends early (it
    /// shrank) is [`io::ErrorKind::UnexpectedEof`].
    pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if offset >= self.len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "read past the end of the attachment file",
            ));
        }
        let left = self.len - offset;
        let want = usize::try_from(left).map_or(buf.len(), |l| l.min(buf.len()));
        let dst = buf
            .get_mut(..want)
            .ok_or_else(|| io::Error::other("short buffer"))?;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(dst)?;
        Ok(want)
    }
}

impl crate::media::ReadAt for RangeReader {
    fn size(&self) -> u64 {
        self.len
    }

    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let n = self.read_at(offset, buf)?;
        if n == buf.len() {
            Ok(())
        } else {
            Err(io::Error::from(io::ErrorKind::UnexpectedEof))
        }
    }
}

/// Open the store file `name` for random access (M5 §5.4): the checks of
/// [`open_checked`], then the file's identity must equal `ident`, the
/// identity recorded when `verify_blob` hashed it; any change is
/// [`crate::error::BlobProblem::Changed`].
pub(crate) fn open_range(
    dir: &ArchiveDir,
    name: &StorageName,
    byte_len: i64,
    ident: &FileIdentity,
) -> Result<RangeReader, crate::error::BlobProblem> {
    use crate::error::BlobProblem;
    let (file, meta) = open_store_file(dir, name, byte_len)?;
    if FileIdentity::of(&meta) != *ident {
        return Err(BlobProblem::Changed);
    }
    Ok(RangeReader {
        file,
        len: meta.len(),
    })
}

/// What [`walk_store`] found under `attachments/`. Counts only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct StoreWalk {
    /// Entries looked at (both levels, `.incoming/` included).
    pub(crate) entries: u64,
    /// Names the store never writes, and directories below `<hh>/`
    /// (never descended into).
    pub(crate) unexpected: u64,
    /// Regular store files no `blob` row names.
    pub(crate) orphans: u64,
    /// Links, junctions and other reparse points (never followed).
    pub(crate) links: u64,
    /// `attachments/.incoming/<n>.partial` files.
    pub(crate) partials: u64,
    /// With `check_modes`: entries that fail [`check_private`] for their
    /// type (on Unix, the wrong owner or group or other bits).
    pub(crate) not_private: u64,
    /// Directories that could not be listed.
    pub(crate) unlisted: u64,
    /// The walk stopped at its entry limit.
    pub(crate) truncated: bool,
}

/// The type of a directory entry, by `symlink_metadata` (nothing is
/// followed). A Windows reparse point counts as a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Link,
    Dir,
    File,
    Other,
}

fn entry_kind(path: &Path) -> EntryKind {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return EntryKind::Other;
    };
    if meta.file_type().is_symlink() {
        return EntryKind::Link;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return EntryKind::Link;
        }
    }
    if meta.is_dir() {
        EntryKind::Dir
    } else if meta.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

/// `true` for two lowercase hex characters.
fn is_fan_out_name(name: &str) -> bool {
    name.len() == 2 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The walk's state: the limit and the counts.
struct Walker<'a> {
    referenced: Option<&'a BTreeSet<[u8; 32]>>,
    limit: u64,
    check_modes: bool,
    cancel: &'a oma_backup::CancelToken,
    out: StoreWalk,
}

/// Why a walk stopped early.
pub(crate) struct WalkCancelled;

impl Walker<'_> {
    /// Count one more entry; `false` once the limit is passed.
    fn admit(&mut self) -> Result<bool, WalkCancelled> {
        if self.out.entries >= self.limit {
            self.out.truncated = true;
            return Ok(false);
        }
        self.out.entries = self.out.entries.saturating_add(1);
        if self.out.entries.is_multiple_of(1024) && self.cancel.is_cancelled() {
            return Err(WalkCancelled);
        }
        Ok(true)
    }

    fn private(&mut self, path: &Path, kind: Expect) {
        if self.check_modes && check_private(path, kind).is_err() {
            self.out.not_private = self.out.not_private.saturating_add(1);
        }
    }

    /// The entries of `dir`, or `None` (counted) when it cannot be listed.
    fn list(&mut self, dir: &Path) -> Option<fs::ReadDir> {
        match fs::read_dir(dir) {
            Ok(r) => Some(r),
            Err(_) => {
                self.out.unlisted = self.out.unlisted.saturating_add(1);
                None
            }
        }
    }

    fn incoming(&mut self, dir: &Path) -> Result<(), WalkCancelled> {
        let Some(entries) = self.list(dir) else {
            return Ok(());
        };
        for entry in entries {
            if !self.admit()? {
                return Ok(());
            }
            let Ok(entry) = entry else {
                self.out.unlisted = self.out.unlisted.saturating_add(1);
                continue;
            };
            let path = entry.path();
            let partial = entry.file_name().to_str().is_some_and(is_partial_name);
            match entry_kind(&path) {
                EntryKind::Link => self.out.links = self.out.links.saturating_add(1),
                EntryKind::File if partial => {
                    self.out.partials = self.out.partials.saturating_add(1);
                    self.private(&path, Expect::File);
                }
                _ => self.out.unexpected = self.out.unexpected.saturating_add(1),
            }
        }
        Ok(())
    }

    fn fan_out(&mut self, dir: &Path, hh: &str) -> Result<(), WalkCancelled> {
        let Some(entries) = self.list(dir) else {
            return Ok(());
        };
        for entry in entries {
            if !self.admit()? {
                return Ok(());
            }
            let Ok(entry) = entry else {
                self.out.unlisted = self.out.unlisted.saturating_add(1);
                continue;
            };
            let path = entry.path();
            let name = entry
                .file_name()
                .to_str()
                .and_then(StorageName::parse)
                .filter(|n| n.as_str().starts_with(hh));
            match (entry_kind(&path), name) {
                (EntryKind::Link, _) => self.out.links = self.out.links.saturating_add(1),
                (EntryKind::File, Some(name)) => {
                    // Without the full set of referenced digests no file
                    // is called an orphan.
                    let known = self
                        .referenced
                        .is_none_or(|r| name.sha256().is_some_and(|s| r.contains(&s)));
                    if !known {
                        self.out.orphans = self.out.orphans.saturating_add(1);
                    }
                    self.private(&path, Expect::File);
                }
                _ => self.out.unexpected = self.out.unexpected.saturating_add(1),
            }
        }
        Ok(())
    }
}

/// Walk `attachments/` (spec §11): at most two levels, no link followed,
/// at most `limit` entries (then `truncated`). `referenced` holds the
/// digests the `blob` rows name; `None` when they could not all be read,
/// and then no file is counted as an orphan. With `check_modes`, every directory and
/// file found is also checked with [`check_private`].
pub(crate) fn walk_store(
    dir: &ArchiveDir,
    referenced: Option<&BTreeSet<[u8; 32]>>,
    limit: u64,
    check_modes: bool,
    cancel: &oma_backup::CancelToken,
) -> Result<StoreWalk, WalkCancelled> {
    let mut w = Walker {
        referenced,
        limit,
        check_modes,
        cancel,
        out: StoreWalk::default(),
    };
    let root = dir.attachments();
    match entry_kind(&root) {
        EntryKind::Dir => {}
        EntryKind::Link => {
            w.out.links = 1;
            return Ok(w.out);
        }
        _ => {
            w.out.unlisted = 1;
            return Ok(w.out);
        }
    }
    w.private(&root, Expect::Dir);
    let Some(entries) = w.list(&root) else {
        return Ok(w.out);
    };
    for entry in entries {
        if !w.admit()? {
            break;
        }
        let Ok(entry) = entry else {
            w.out.unlisted = w.out.unlisted.saturating_add(1);
            continue;
        };
        let path = entry.path();
        let os_name = entry.file_name();
        let name = os_name.to_str();
        match (entry_kind(&path), name) {
            (EntryKind::Link, _) => w.out.links = w.out.links.saturating_add(1),
            (EntryKind::Dir, Some(INCOMING_NAME)) => {
                w.private(&path, Expect::Dir);
                w.incoming(&path)?;
            }
            (EntryKind::Dir, Some(hh)) if is_fan_out_name(hh) => {
                w.private(&path, Expect::Dir);
                let hh = hh.to_owned();
                w.fan_out(&path, &hh)?;
            }
            _ => w.out.unexpected = w.out.unexpected.saturating_add(1),
        }
        if w.out.truncated {
            break;
        }
    }
    Ok(w.out)
}

/// The name of `attachments/.incoming`.
const INCOMING_NAME: &str = crate::layout::INCOMING_DIR;

/// What phase C did with one copied file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Placement {
    /// Renamed into the store as a new file.
    Placed,
    /// The store already holds the file (referenced, or placed earlier in
    /// the batch) as a regular file of the right length; the copy was
    /// dropped. The full verifier re-hashes referenced files.
    Deduplicated,
    /// An unreferenced file of that name re-hashed equal and was adopted.
    Adopted,
    /// An unreferenced file of that name did not match (or was not a
    /// regular file) and was replaced.
    OrphanReplaced,
    /// The file or directory in the way is unusable and was left alone.
    Conflict,
    /// An I/O error placing the file; the copy was dropped.
    Failed(io::ErrorKind),
}

/// Phase C of the copy procedure (spec §9.1) for one batch. Every store
/// path is checked through [`store_dir`] before it is touched, files are
/// opened with `open_nofollow`, and nothing ever replaces an existing
/// file.
pub(crate) struct Placer<'a> {
    dir: &'a ArchiveDir,
    store: &'a dyn StoreFs,
    touched: BTreeSet<PathBuf>,
    /// Partial files that could not be removed.
    pub(crate) partials_left: u64,
}

impl<'a> Placer<'a> {
    pub(crate) fn new(dir: &'a ArchiveDir, store: &'a dyn StoreFs) -> Placer<'a> {
        Placer {
            dir,
            store,
            touched: BTreeSet::new(),
            partials_left: 0,
        }
    }

    fn drop_partial(&mut self, partial: Partial) {
        if !partial.remove(self.store) {
            self.partials_left = self.partials_left.saturating_add(1);
        }
    }

    /// Place `partial` (synced, holding the plaintext whose name is
    /// `name`). `known_len` is the length the store must already hold under
    /// that name: the `blob` row's `byte_len`, or the length placed earlier
    /// in this batch; `None` when the name is unreferenced.
    pub(crate) fn place(
        &mut self,
        partial: Partial,
        name: &StorageName,
        known_len: Option<u64>,
    ) -> Placement {
        // `attachments/` and `<hh>/` must be private real directories.
        let hh = match store_dir(self.dir, name) {
            Ok(Some(hh)) => hh,
            Ok(None) => {
                if let Err(e) = self.store.create_dir_durable(&name.dir_in(self.dir)) {
                    self.drop_partial(partial);
                    return Placement::Failed(e.kind());
                }
                match store_dir(self.dir, name) {
                    Ok(Some(hh)) => hh,
                    _ => {
                        self.drop_partial(partial);
                        return Placement::Conflict;
                    }
                }
            }
            Err(_) => {
                self.drop_partial(partial);
                return Placement::Conflict;
            }
        };
        let target = name.path_in(self.dir);
        match fs::symlink_metadata(&target) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.rename(partial, &target, hh, Placement::Placed)
            }
            Err(e) => {
                self.drop_partial(partial);
                Placement::Failed(e.kind())
            }
            Ok(_) if known_len.is_some() => {
                self.drop_partial(partial);
                if regular_len(&target) == known_len {
                    Placement::Deduplicated
                } else {
                    Placement::Conflict
                }
            }
            Ok(meta) => {
                // An orphan: re-hashed before it is adopted (spec §9.1).
                let regular = meta.is_file() && !meta.file_type().is_symlink();
                if regular && hash_store_file(&target).as_ref() == name.sha256().as_ref() {
                    self.drop_partial(partial);
                    return Placement::Adopted;
                }
                if meta.is_dir() {
                    self.drop_partial(partial);
                    return Placement::Conflict;
                }
                // A link is removed itself; its target is never touched.
                if self.store.remove_file(&target).is_err() {
                    self.drop_partial(partial);
                    return Placement::Conflict;
                }
                self.rename(partial, &target, hh, Placement::OrphanReplaced)
            }
        }
    }

    fn rename(&mut self, partial: Partial, target: &Path, hh: PathBuf, ok: Placement) -> Placement {
        let placed = match partial.path() {
            Ok(from) => self.store.place_noreplace(from, target),
            Err(e) => Err(e),
        };
        match placed {
            Ok(placed) => {
                self.touched.insert(hh);
                if matches!(
                    placed,
                    Placed::Linked {
                        source_removed: false
                    }
                ) {
                    self.drop_partial(partial);
                } else {
                    partial.placed();
                }
                point(FaultPoint::AttachmentPlaced);
                ok
            }
            Err(e) => {
                self.drop_partial(partial);
                Placement::Failed(e.kind())
            }
        }
    }

    /// Flush each touched `<hh>` directory once, after every rename of the
    /// batch (spec §9.1 step 3). Returns the number of flushes, or the
    /// first error: the batch's rows must not commit before their files'
    /// entries are durable.
    pub(crate) fn sync(&mut self) -> io::Result<u64> {
        let mut n = 0u64;
        for hh in std::mem::take(&mut self.touched) {
            self.store.fsync_dir(&hh)?;
            n = n.saturating_add(1);
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_names_parse_strictly() {
        let good = "0123456789abcdef".repeat(4);
        assert!(StorageName::parse(&good).is_some());
        for bad in [
            String::new(),
            "0".repeat(63),
            "0".repeat(65),
            "A".repeat(64),
            format!("../{}", "0".repeat(61)),
            "../".repeat(21) + "a",
            format!("{}/", "0".repeat(63)),
        ] {
            assert!(StorageName::parse(&bad).is_none(), "{bad}");
        }
        assert!(StorageName::of_sha256(&[0u8; 31]).is_none());
        let n = StorageName::of_sha256(&[0xab; 32]).expect("32 bytes");
        assert_eq!(n, StorageName::parse(&"ab".repeat(32)).expect("same"));
        assert_eq!(format!("{n:?}"), "StorageName(..)");
    }

    #[test]
    fn partial_names() {
        assert!(is_partial_name("0.partial"));
        assert!(is_partial_name("123.partial"));
        assert!(!is_partial_name(".partial"));
        assert!(!is_partial_name("a.partial"));
        assert!(!is_partial_name("1.partial.x"));
        assert!(!is_partial_name("1"));
    }

    #[test]
    fn sweeps_remove_only_matching_regular_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let d = tmp.path();
        fs::write(d.join("1.partial"), b"x").expect("write");
        fs::write(d.join("keep.txt"), b"x").expect("write");
        fs::create_dir(d.join("2.partial")).expect("mkdir");
        #[cfg(unix)]
        {
            fs::write(d.join("target"), b"keep").expect("write");
            std::os::unix::fs::symlink(d.join("target"), d.join("3.partial")).expect("symlink");
        }
        let swept = sweep_matching(d, is_partial_name, SWEEP_LIMIT).expect("sweep");
        assert_eq!(
            swept,
            Swept {
                removed: 1,
                left: 0
            }
        );
        assert!(!d.join("1.partial").exists());
        assert!(d.join("keep.txt").exists());
        assert!(d.join("2.partial").is_dir());
        #[cfg(unix)]
        assert_eq!(fs::read(d.join("target")).expect("read"), b"keep");
    }

    #[test]
    fn storage_names_spell_their_digest() {
        let sha = [0x0fu8; 32];
        let n = StorageName::of_sha256(&sha).expect("name");
        assert_eq!(n.sha256(), Some(sha));
    }

    fn private_archive(tmp: &tempfile::TempDir) -> ArchiveDir {
        let root = tmp.path().join("a");
        oma_fs::create_dir_durable(&root).expect("root");
        let dir = ArchiveDir::at(root);
        oma_fs::create_dir_durable(&dir.attachments()).expect("attachments");
        oma_fs::create_dir_durable(&dir.incoming()).expect("incoming");
        dir
    }

    fn partial_with(dir: &ArchiveDir, next: &mut u64, bytes: &[u8]) -> Partial {
        use std::io::Write;
        let mut p = Partial::create(dir, next).expect("partial");
        p.file().expect("file").write_all(bytes).expect("write");
        p.finish(
            &crate::store_fs::RealFs,
            u64::try_from(bytes.len()).expect("len"),
        )
        .expect("finish");
        p
    }

    #[test]
    fn placement_never_replaces_and_adopts_only_matching_orphans() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = private_archive(&tmp);
        let store = crate::store_fs::RealFs;
        let mut next = 0u64;
        let bytes = b"synthetic attachment bytes";
        let sha: [u8; 32] = Sha256::digest(bytes).into();
        let name = StorageName::of_sha256(&sha).expect("name");
        let mut placer = Placer::new(&dir, &store);

        // New: placed, and the partial is gone.
        let p = partial_with(&dir, &mut next, bytes);
        assert_eq!(placer.place(p, &name, None), Placement::Placed);
        assert_eq!(store_file_state(&dir, &name), StoreFileState::Regular(26));
        assert_eq!(placer.sync().expect("sync"), 1);
        // Referenced with the right length: deduplicated.
        let p = partial_with(&dir, &mut next, bytes);
        assert_eq!(placer.place(p, &name, Some(26)), Placement::Deduplicated);
        // Referenced with another length: a conflict, the file untouched.
        let p = partial_with(&dir, &mut next, bytes);
        assert_eq!(placer.place(p, &name, Some(25)), Placement::Conflict);
        assert_eq!(fs::read(name.path_in(&dir)).expect("read"), bytes);
        // Unreferenced and equal: adopted.
        let p = partial_with(&dir, &mut next, bytes);
        assert_eq!(placer.place(p, &name, None), Placement::Adopted);
        // Unreferenced and different: replaced by ours.
        fs::write(name.path_in(&dir), b"other").expect("plant");
        let p = partial_with(&dir, &mut next, bytes);
        assert_eq!(placer.place(p, &name, None), Placement::OrphanReplaced);
        assert_eq!(fs::read(name.path_in(&dir)).expect("read"), bytes);
        // No partial file is left behind.
        assert_eq!(fs::read_dir(dir.incoming()).expect("dir").count(), 0);
        assert_eq!(placer.partials_left, 0);
    }

    #[cfg(unix)]
    #[test]
    fn placement_never_follows_a_link() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = private_archive(&tmp);
        let store = crate::store_fs::RealFs;
        let mut next = 0u64;
        let bytes = b"synthetic";
        let sha: [u8; 32] = Sha256::digest(bytes).into();
        let name = StorageName::of_sha256(&sha).expect("name");
        let outside = tmp.path().join("outside");
        fs::write(&outside, b"keep me").expect("outside");
        let mut placer = Placer::new(&dir, &store);
        // A link at <hex>, referenced: a conflict; the target is untouched.
        let p = partial_with(&dir, &mut next, bytes);
        assert_eq!(placer.place(p, &name, None), Placement::Placed);
        fs::remove_file(name.path_in(&dir)).expect("rm");
        std::os::unix::fs::symlink(&outside, name.path_in(&dir)).expect("link");
        assert_eq!(store_file_state(&dir, &name), StoreFileState::Unusable);
        let p = partial_with(&dir, &mut next, bytes);
        assert_eq!(placer.place(p, &name, Some(9)), Placement::Conflict);
        // Unreferenced: the link itself is replaced, never its target.
        let p = partial_with(&dir, &mut next, bytes);
        assert_eq!(placer.place(p, &name, None), Placement::OrphanReplaced);
        assert_eq!(fs::read(&outside).expect("read"), b"keep me");
        assert_eq!(fs::read(name.path_in(&dir)).expect("read"), bytes);
        // A link at <hh>: nothing is placed through it.
        let hh = name.dir_in(&dir);
        fs::remove_file(name.path_in(&dir)).expect("rm");
        fs::remove_dir(&hh).expect("rmdir");
        let elsewhere = tmp.path().join("elsewhere");
        fs::create_dir(&elsewhere).expect("mkdir");
        std::os::unix::fs::symlink(&elsewhere, &hh).expect("link");
        let p = partial_with(&dir, &mut next, bytes);
        assert_eq!(placer.place(p, &name, None), Placement::Conflict);
        assert_eq!(fs::read_dir(&elsewhere).expect("dir").count(), 0);
    }
}
