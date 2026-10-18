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
//!
//! The copy procedure itself (phases A–D) arrives with the attachment pass.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use oma_fs::{check_private, Expect, PermProblem};
use rusqlite::TransactionBehavior;

use crate::conn::WriteConn;
use crate::error::{ArchiveError, IoOp, Stage};
use crate::id::to_hex;
use crate::layout::ArchiveDir;
use crate::store_fs::StoreFs;

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
    /// refused, so `..`, separators and upper case never reach a path.
    /// Its callers (the blob reader and the verifier) arrive in later
    /// slices; the parser is tested now because the schema's CHECK is not
    /// trusted on a crafted file.
    #[allow(dead_code)]
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
}
