//! The archive lock (spec §5.3 step 4, D53).
//!
//! `archive.lock` is a sidecar file, created once when the archive is
//! created and never deleted. It is opened without following links and
//! locked with `File::try_lock`, which is `flock(LOCK_EX | LOCK_NB)` on
//! Linux and `LockFileEx` on Windows.
//!
//! **Self-test.** After taking the lock, a second handle to the same file
//! must be refused. On Linux that holds because `flock` locks belong to the
//! open file description; on Windows because `LockFileEx` refuses a
//! process's own second handle. A volume where the second handle gets the
//! lock cannot be trusted, and the open fails with `LockUnreliable`. The
//! self-test covers the sidecar only, not SQLite's own locks; volumes where
//! those are doubtful are refused by the volume check before this point.

use std::fs::{File, TryLockError};
use std::io;
use std::path::Path;
use std::time::Duration;

use oma_fs::{open_nofollow, open_refusal, Expect, OpenRefusal, PermProblem};

use crate::error::{ArchiveError, IoOp};

/// Retries after the first attempt when the lock is held. Windows releases
/// a terminated process's locks "after a resource-dependent delay"
/// (Microsoft Learn, LockFileEx remarks).
pub const LOCK_RETRIES: u32 = 5;
/// The wait between attempts.
pub const LOCK_RETRY_WAIT: Duration = Duration::from_millis(200);

/// A held archive lock. Dropping it closes the handle, which releases the
/// lock; the file stays.
pub(crate) struct ArchiveLock {
    _file: File,
}

impl std::fmt::Debug for ArchiveLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ArchiveLock")
    }
}

/// What a one-shot probe of the lock found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockProbe {
    /// Nobody held it; it was taken and released at once.
    Free,
    /// Another handle holds it.
    Held,
}

fn open_lock_file(path: &Path) -> Result<File, ArchiveError> {
    open_nofollow(path).map_err(|e| match open_refusal(&e) {
        Some(OpenRefusal::Link) => ArchiveError::Permissions(PermProblem::Link),
        Some(_) => ArchiveError::Permissions(PermProblem::WrongType {
            expected: Expect::File,
        }),
        None => ArchiveError::io(IoOp::OpenLock, &e),
    })
}

impl ArchiveLock {
    /// Open `path` and take the lock, retrying [`LOCK_RETRIES`] times at
    /// [`LOCK_RETRY_WAIT`] while another handle holds it; then run the
    /// self-test.
    pub(crate) fn acquire(path: &Path) -> Result<ArchiveLock, ArchiveError> {
        let file = open_lock_file(path)?;
        let mut attempt = 0u32;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if attempt < LOCK_RETRIES => {
                    attempt = attempt.saturating_add(1);
                    std::thread::sleep(LOCK_RETRY_WAIT);
                }
                Err(TryLockError::WouldBlock) => return Err(ArchiveError::Locked),
                Err(TryLockError::Error(e)) => return Err(ArchiveError::io(IoOp::Lock, &e)),
            }
        }
        self_test(path)?;
        Ok(ArchiveLock { _file: file })
    }

    /// Create `path` (it must not exist; mode 0600 on Unix), take the lock
    /// at once and run the self-test. Used when an archive is created, and
    /// when an archive's lock file is missing (§5.3 step 4).
    pub(crate) fn create(path: &Path) -> Result<ArchiveLock, ArchiveError> {
        let created =
            oma_fs::create_new_private(path).map_err(|e| ArchiveError::io(IoOp::CreateFile, &e))?;
        drop(created);
        let file = open_lock_file(path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(ArchiveError::Locked),
            Err(TryLockError::Error(e)) => return Err(ArchiveError::io(IoOp::Lock, &e)),
        }
        self_test(path)?;
        Ok(ArchiveLock { _file: file })
    }

    /// Take the lock and release it at once, never waiting (for listing).
    pub(crate) fn probe(path: &Path) -> Result<LockProbe, ArchiveError> {
        let file = open_lock_file(path)?;
        match file.try_lock() {
            Ok(()) => {
                // Closing the handle releases the lock.
                drop(file);
                Ok(LockProbe::Free)
            }
            Err(TryLockError::WouldBlock) => Ok(LockProbe::Held),
            Err(TryLockError::Error(e)) => Err(ArchiveError::io(IoOp::Lock, &e)),
        }
    }
}

/// A second handle to the same file must not get the lock.
fn self_test(path: &Path) -> Result<(), ArchiveError> {
    let second = open_lock_file(path)?;
    let verdict = match second.try_lock() {
        Err(TryLockError::WouldBlock) => Ok(()),
        Ok(()) => Err(ArchiveError::LockUnreliable),
        Err(TryLockError::Error(e)) => {
            tracing::debug!(kind = ?e.kind(), "lock self-test: the second handle errored");
            Err(ArchiveError::LockUnreliable)
        }
    };
    drop(second);
    if crate::fault::lock_self_test_fails() {
        return Err(ArchiveError::LockUnreliable);
    }
    verdict
}

/// `true` when an I/O error is a sharing violation or access denial that a
/// scanner holding a handle can cause on Windows (codes 32 and 5).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn is_transient_sharing_error(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(5 | 32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_acquire_is_locked_and_release_frees_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("archive.lock");
        let held = ArchiveLock::create(&path).expect("create");
        assert_eq!(ArchiveLock::probe(&path).expect("probe"), LockProbe::Held);
        let started = std::time::Instant::now();
        assert!(matches!(
            ArchiveLock::acquire(&path),
            Err(ArchiveError::Locked)
        ));
        // Five retries at 200 ms.
        assert!(started.elapsed() >= Duration::from_millis(900));
        drop(held);
        assert_eq!(ArchiveLock::probe(&path).expect("probe"), LockProbe::Free);
        let again = ArchiveLock::acquire(&path).expect("acquire");
        drop(again);
        assert!(path.is_file(), "the lock file stays");
    }

    #[test]
    fn create_refuses_an_existing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("archive.lock");
        std::fs::write(&path, b"").expect("write");
        assert!(matches!(
            ArchiveLock::create(&path),
            Err(ArchiveError::Io {
                kind: io::ErrorKind::AlreadyExists,
                ..
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_lock_file_is_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real = tmp.path().join("real");
        std::fs::write(&real, b"").expect("write");
        let link = tmp.path().join("archive.lock");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        assert!(matches!(
            ArchiveLock::acquire(&link),
            Err(ArchiveError::Permissions(PermProblem::Link))
        ));
    }

    #[test]
    fn sharing_errors_are_recognised() {
        assert!(is_transient_sharing_error(&io::Error::from_raw_os_error(
            32
        )));
        assert!(is_transient_sharing_error(&io::Error::from_raw_os_error(5)));
        assert!(!is_transient_sharing_error(&io::Error::from_raw_os_error(
            2
        )));
    }
}
