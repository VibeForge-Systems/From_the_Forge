//! Guards on every file opened from a backup (D9).
//!
//! A file is opened only when `symlink_metadata` reports a regular file that
//! is not a link, and the opened handle is checked again. A FIFO, device,
//! socket, directory or link is never opened, so a hostile backup cannot make
//! a read block forever or reach outside the backup through a link.
//!
//! The window between the check and the open is out of scope: the threat is a
//! static hostile backup, not a concurrent local attacker who could read the
//! file anyway.

use std::collections::HashMap;
use std::fs::{self, File, Metadata};
use std::io;
use std::path::{Path, PathBuf};

/// `true` when `meta` (from `symlink_metadata`) describes a link.
///
/// On Windows, std's `FileType::is_symlink` is understood to be true for
/// name-surrogate reparse points (symbolic links and junctions) and false for
/// other reparse points such as cloud-file placeholders. That reading of std
/// is **UNVERIFIED** here (the std source was not available offline); the
/// Windows junction test in CI exercises it.
pub(crate) fn is_link(meta: &Metadata) -> bool {
    meta.file_type().is_symlink()
}

/// What a path is, without following links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Probe {
    /// Nothing at that path.
    Missing,
    /// A link, directory, FIFO, device or socket.
    NotRegular,
    /// A regular file of this length.
    Regular { len: u64 },
}

impl Probe {
    pub(crate) fn is_regular(self) -> bool {
        matches!(self, Probe::Regular { .. })
    }
}

/// Classify `path` with `symlink_metadata`. `NotFound` is `Missing`, not an
/// error; any other I/O error is returned.
pub(crate) fn probe(path: &Path) -> io::Result<Probe> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.is_file() && !is_link(&meta) {
                Ok(Probe::Regular { len: meta.len() })
            } else {
                Ok(Probe::NotRegular)
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Probe::Missing),
        Err(e) => Err(e),
    }
}

/// Why [`open_source_file`] did not return a file.
#[derive(Debug)]
pub(crate) enum OpenFailure {
    Missing,
    NotRegular,
    Io(io::Error),
}

/// Open a file from a backup read-only, after the guard. Returns the file and
/// its length as reported by the handle.
pub(crate) fn open_source_file(path: &Path) -> Result<(File, u64), OpenFailure> {
    match probe(path) {
        Ok(Probe::Regular { .. }) => {}
        Ok(Probe::Missing) => return Err(OpenFailure::Missing),
        Ok(Probe::NotRegular) => return Err(OpenFailure::NotRegular),
        Err(e) => return Err(OpenFailure::Io(e)),
    }
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(OpenFailure::Missing),
        Err(e) => return Err(OpenFailure::Io(e)),
    };
    let meta = file.metadata().map_err(OpenFailure::Io)?;
    if !meta.is_file() {
        return Err(OpenFailure::NotRegular);
    }
    Ok((file, meta.len()))
}

/// The state of one `xx/` blob directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirState {
    /// A real directory under the backup root.
    Ok,
    /// No such directory: every blob under it is missing.
    Missing,
    /// A link, a non-directory, or a directory that resolves outside the
    /// backup: no blob under it is read.
    Unsafe,
}

/// Validates each `xx/` blob directory once (D9) and caches the result.
pub(crate) struct BlobDirGuard {
    containment_root: PathBuf,
    cache: HashMap<String, DirState>,
}

/// There are only 256 two-hex-digit prefixes.
const BLOB_DIR_CACHE: usize = 256;

impl BlobDirGuard {
    /// `containment_root` is the backup's `std::fs::canonicalize` path.
    pub(crate) fn new(containment_root: PathBuf) -> Self {
        BlobDirGuard {
            containment_root,
            cache: HashMap::with_capacity(BLOB_DIR_CACHE),
        }
    }

    /// Check the directory `dir`, whose name is the hex prefix `prefix`.
    pub(crate) fn check(&mut self, prefix: &str, dir: &Path) -> io::Result<DirState> {
        if let Some(state) = self.cache.get(prefix) {
            return Ok(*state);
        }
        let state = self.evaluate(dir)?;
        if self.cache.len() < BLOB_DIR_CACHE {
            self.cache.insert(prefix.to_owned(), state);
        }
        Ok(state)
    }

    fn evaluate(&self, dir: &Path) -> io::Result<DirState> {
        let meta = match fs::symlink_metadata(dir) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(DirState::Missing),
            Err(e) => return Err(e),
        };
        if !meta.is_dir() || is_link(&meta) {
            return Ok(DirState::Unsafe);
        }
        let canonical = fs::canonicalize(dir)?;
        if canonical.starts_with(&self.containment_root) && canonical != self.containment_root {
            Ok(DirState::Ok)
        } else {
            Ok(DirState::Unsafe)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_classifies_files_dirs_and_absence() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let f = tmp.path().join("f");
        fs::write(&f, b"abc").expect("write");
        assert_eq!(probe(&f).expect("probe"), Probe::Regular { len: 3 });
        assert_eq!(probe(tmp.path()).expect("probe"), Probe::NotRegular);
        assert_eq!(
            probe(&tmp.path().join("missing")).expect("probe"),
            Probe::Missing
        );
        assert!(open_source_file(&f).is_ok());
        assert!(matches!(
            open_source_file(tmp.path()),
            Err(OpenFailure::NotRegular)
        ));
        assert!(matches!(
            open_source_file(&tmp.path().join("missing")),
            Err(OpenFailure::Missing)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn links_and_fifos_are_never_opened() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("target");
        fs::write(&target, b"outside").expect("write");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert_eq!(probe(&link).expect("probe"), Probe::NotRegular);
        assert!(matches!(
            open_source_file(&link),
            Err(OpenFailure::NotRegular)
        ));

        let fifo = tmp.path().join("fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo");
        assert!(status.success());
        // Opening a FIFO for reading would block forever; the guard never opens it.
        assert!(matches!(
            open_source_file(&fifo),
            Err(OpenFailure::NotRegular)
        ));
    }

    #[test]
    fn blob_dirs_are_checked_once_and_cached() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = fs::canonicalize(tmp.path()).expect("canonical");
        fs::create_dir(root.join("3d")).expect("mkdir");
        fs::write(root.join("ab"), b"not a dir").expect("write");
        let mut g = BlobDirGuard::new(root.clone());
        assert_eq!(g.check("3d", &root.join("3d")).expect("ok"), DirState::Ok);
        assert_eq!(
            g.check("00", &root.join("00")).expect("ok"),
            DirState::Missing
        );
        assert_eq!(
            g.check("ab", &root.join("ab")).expect("ok"),
            DirState::Unsafe
        );
        // Cached: removing the directory does not change the answer.
        fs::remove_dir(root.join("3d")).expect("rmdir");
        assert_eq!(g.check("3d", &root.join("3d")).expect("ok"), DirState::Ok);
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_blob_dir_is_unsafe() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir");
        let root = fs::canonicalize(tmp.path()).expect("canonical");
        std::os::unix::fs::symlink(outside.path(), root.join("3d")).expect("symlink");
        let mut g = BlobDirGuard::new(root.clone());
        assert_eq!(
            g.check("3d", &root.join("3d")).expect("ok"),
            DirState::Unsafe
        );
    }
}
