//! Every path inside an archive (spec §5.1), built with `Path::join` from
//! this module's constants and a parsed [`ArchiveId`] only.
//!
//! ```text
//! <library>/
//!   .oma-new-<32hex>/            an archive being created
//!   a<32hex>/                    one archive; 0700 on Unix
//!     archive.lock               sidecar lock; created once, never deleted
//!     archive.sqlite             0600 on Unix (+ SQLite's -wal and -shm)
//!     attachments/
//!       .incoming/               <n>.partial while a copy is in flight
//!       <hh>/<64 hex>            content-addressed plaintext
//!     imports/
//!       session-<n>.json         content-free provenance
//! ```
//!
//! The database path is handed out only to `conn.rs` (it takes a key only
//! that module can make), so no other module can open, copy or hash the
//! database file while a connection is open (§5.5).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use oma_fs::{check_private, Expect, PermProblem};

use crate::conn::DbPathKey;
use crate::error::ArchiveError;
use crate::id::ArchiveId;

pub(crate) const LOCK_FILE: &str = "archive.lock";
pub(crate) const DB_FILE: &str = "archive.sqlite";
const WAL_FILE: &str = "archive.sqlite-wal";
const SHM_FILE: &str = "archive.sqlite-shm";
const JOURNAL_FILE: &str = "archive.sqlite-journal";
pub(crate) const ATTACHMENTS_DIR: &str = "attachments";
pub(crate) const INCOMING_DIR: &str = ".incoming";
pub(crate) const IMPORTS_DIR: &str = "imports";
/// The prefix of a directory that holds an archive being created.
pub(crate) const NEW_PREFIX: &str = ".oma-new-";

/// The names a fully created archive directory holds, and nothing else
/// (used by the creation-directory sweep).
pub(crate) const TOP_LEVEL_NAMES: [&str; 7] = [
    LOCK_FILE,
    DB_FILE,
    WAL_FILE,
    SHM_FILE,
    JOURNAL_FILE,
    ATTACHMENTS_DIR,
    IMPORTS_DIR,
];

/// The directory of one archive (or of one being created).
#[derive(Clone)]
pub(crate) struct ArchiveDir {
    root: PathBuf,
}

impl std::fmt::Debug for ArchiveDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ArchiveDir(<path>)")
    }
}

impl ArchiveDir {
    /// `<library>/a<32hex>`.
    pub(crate) fn of(library: &Path, id: &ArchiveId) -> ArchiveDir {
        ArchiveDir {
            root: library.join(id.as_str()),
        }
    }

    /// `<library>/.oma-new-<32hex>` for the id about to be created.
    pub(crate) fn creating(library: &Path, id: &ArchiveId) -> ArchiveDir {
        let mut name = String::from(NEW_PREFIX);
        name.push_str(id.hex());
        ArchiveDir {
            root: library.join(name),
        }
    }

    /// A directory found by listing the library, whose name was already
    /// checked by the caller.
    pub(crate) fn at(root: PathBuf) -> ArchiveDir {
        ArchiveDir { root }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn lock(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    pub(crate) fn attachments(&self) -> PathBuf {
        self.root.join(ATTACHMENTS_DIR)
    }

    pub(crate) fn incoming(&self) -> PathBuf {
        self.root.join(ATTACHMENTS_DIR).join(INCOMING_DIR)
    }

    /// `attachments/.incoming/<n>.partial`.
    pub(crate) fn partial(&self, n: u64) -> PathBuf {
        let mut name = n.to_string();
        name.push_str(".partial");
        self.incoming().join(name)
    }

    pub(crate) fn imports(&self) -> PathBuf {
        self.root.join(IMPORTS_DIR)
    }

    /// `imports/session-<n>.json`, the provenance file of session `n`.
    pub(crate) fn provenance_file(&self, session: i64) -> PathBuf {
        self.imports().join(provenance_name(session, false))
    }

    /// `imports/session-<n>.json.tmp`, its temporary file.
    pub(crate) fn provenance_tmp(&self, session: i64) -> PathBuf {
        self.imports().join(provenance_name(session, true))
    }

    /// `attachments/<hh>/<storage name>`. `name` must already be a parsed
    /// storage name (64 lowercase hex characters); `blobs.rs` guarantees it.
    pub(crate) fn store_file(&self, hh: &str, name: &str) -> PathBuf {
        self.root.join(ATTACHMENTS_DIR).join(hh).join(name)
    }

    /// The database path, for `conn.rs` only.
    pub(crate) fn database(&self, _key: &DbPathKey) -> PathBuf {
        self.root.join(DB_FILE)
    }

    /// Whether `archive.sqlite` exists as anything (`symlink_metadata`).
    pub(crate) fn database_exists(&self) -> Result<bool, ArchiveError> {
        exists(&self.root.join(DB_FILE))
    }

    /// Whether `archive.lock` exists as anything.
    pub(crate) fn lock_exists(&self) -> Result<bool, ArchiveError> {
        exists(&self.root.join(LOCK_FILE))
    }

    /// The bytes the database occupies: the main file and its `-wal`
    /// (metadata only).
    pub(crate) fn database_bytes(&self) -> u64 {
        [DB_FILE, WAL_FILE]
            .iter()
            .filter_map(|n| fs::symlink_metadata(self.root.join(n)).ok())
            .map(|m| m.len())
            .fold(0u64, u64::saturating_add)
    }

    /// The permission checks of §5.3 step 2: the directory, the database
    /// and its `-wal` and `-shm` when present, the lock file (when
    /// `lock_present`), `attachments/`, `attachments/.incoming/` and
    /// `imports/`. On Unix each must be owned by the effective user with no
    /// group or other bits; everywhere, none may be a link or the wrong
    /// type. Attachment files are left to the verifier (§24 item 8).
    pub(crate) fn check_private_all(&self, lock_present: bool) -> Result<(), PermProblem> {
        check_private(&self.root, Expect::Dir)?;
        check_private(&self.root.join(DB_FILE), Expect::File)?;
        for side in [WAL_FILE, SHM_FILE] {
            let p = self.root.join(side);
            match fs::symlink_metadata(&p) {
                Ok(_) => check_private(&p, Expect::File)?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(PermProblem::Io(e.kind())),
            }
        }
        if lock_present {
            check_private(&self.lock(), Expect::File)?;
        }
        check_private(&self.attachments(), Expect::Dir)?;
        check_private(&self.incoming(), Expect::Dir)?;
        check_private(&self.imports(), Expect::Dir)?;
        Ok(())
    }
}

impl ArchiveDir {
    /// The full verifier's owner and mode check (spec §11) of the files
    /// [`ArchiveDir::check_private_all`] covers, counting every failure
    /// instead of stopping at the first. Absent `-wal` and `-shm` files are
    /// not counted.
    pub(crate) fn count_not_private(&self) -> u64 {
        let mut checks: Vec<(PathBuf, Expect)> = vec![
            (self.root.clone(), Expect::Dir),
            (self.root.join(DB_FILE), Expect::File),
            (self.lock(), Expect::File),
            (self.attachments(), Expect::Dir),
            (self.incoming(), Expect::Dir),
            (self.imports(), Expect::Dir),
        ];
        for side in [WAL_FILE, SHM_FILE] {
            let p = self.root.join(side);
            if fs::symlink_metadata(&p).is_ok() {
                checks.push((p, Expect::File));
            }
        }
        let failed = checks
            .iter()
            .filter(|(p, kind)| check_private(p, *kind).is_err())
            .count();
        u64::try_from(failed).unwrap_or(u64::MAX)
    }
}

/// `session-<n>.json`, or with `.tmp` appended. A session id is a
/// positive integer from the database, never a string from outside.
fn provenance_name(session: i64, tmp: bool) -> String {
    let mut name = String::from("session-");
    name.push_str(&session.to_string());
    name.push_str(if tmp { ".json.tmp" } else { ".json" });
    name
}

fn exists(p: &Path) -> Result<bool, ArchiveError> {
    match fs::symlink_metadata(p) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(ArchiveError::io(crate::error::IoOp::Metadata, &e)),
    }
}
