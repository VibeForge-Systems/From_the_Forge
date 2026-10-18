//! `StoreFs`: the file-system operations whose order matters for crash
//! safety (spec §9.1, §18.2, §18.4).
//!
//! Archive creation and the attachment store make these calls through a
//! [`StoreFs`], which is [`RealFs`] in production. A test can
//! pass a recording implementation (`Library::with_store_fs`) and check
//! the order of operations: for creation, the directory fsyncs come before
//! the no-replace rename, and the library directory is flushed after it.
//!
//! The trait sees paths, so an implementation must never log them.

use std::io;
use std::path::Path;

pub use oma_fs::{DirSync, Placed};

/// The ordered file-system operations of creation and the store.
pub trait StoreFs: Send + Sync {
    /// Create `path` (0700 on Unix) if absent and flush its parent
    /// ([`oma_fs::create_dir_durable`]).
    fn create_dir_durable(&self, path: &Path) -> io::Result<()>;
    /// Flush a directory's entries ([`oma_fs::fsync_dir`]).
    fn fsync_dir(&self, path: &Path) -> io::Result<DirSync>;
    /// Rename a directory, never replacing an existing target
    /// ([`oma_fs::rename_dir_noreplace`]).
    fn rename_dir_noreplace(&self, from: &Path, to: &Path) -> io::Result<Placed>;
    /// Move a file, never replacing an existing target
    /// ([`oma_fs::place_noreplace`]).
    fn place_noreplace(&self, from: &Path, to: &Path) -> io::Result<Placed>;
    /// Remove a file the store owns (a partial copy, or an unreferenced
    /// store file being replaced). A link is removed itself, never its
    /// target. A test can make it fail, as a scanner holding the file open
    /// does on Windows.
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }
}

/// The real file system, through `oma-fs`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealFs;

impl StoreFs for RealFs {
    fn create_dir_durable(&self, path: &Path) -> io::Result<()> {
        oma_fs::create_dir_durable(path)
    }

    fn fsync_dir(&self, path: &Path) -> io::Result<DirSync> {
        oma_fs::fsync_dir(path)
    }

    fn rename_dir_noreplace(&self, from: &Path, to: &Path) -> io::Result<Placed> {
        oma_fs::rename_dir_noreplace(from, to)
    }

    fn place_noreplace(&self, from: &Path, to: &Path) -> io::Result<Placed> {
        oma_fs::place_noreplace(from, to)
    }
}
