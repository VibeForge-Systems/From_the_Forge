//! Durable placement: directory fsync, durable directory creation, a rename
//! that never replaces its target, and an open that never follows a link.
//!
//! These are the primitives the archive's content-addressed store and its
//! creation path are built from. Each one states what it guarantees on each
//! platform, and where a guarantee rests on something not yet observed it is
//! marked **UNVERIFIED**.

use std::fs::{self, File};
use std::io;
use std::path::Path;

/// `FILE_FLAG_BACKUP_SEMANTICS`: required for `CreateFileW` to open a
/// directory.
#[cfg(windows)]
pub(crate) const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// `FILE_FLAG_OPEN_REPARSE_POINT`: open a reparse point (symbolic link or
/// junction) itself instead of its target.
#[cfg(windows)]
pub(crate) const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
/// `FILE_ATTRIBUTE_REPARSE_POINT`.
#[cfg(windows)]
pub(crate) const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// The outcome of [`fsync_dir`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirSync {
    /// The directory's entries were flushed.
    Synced,
    /// The platform or file system refused to flush a directory. Durability
    /// of new entries then rests on the file system (on Windows, NTFS's own
    /// metadata journaling: **UNVERIFIED**).
    Unsupported,
}

/// Flush a directory, so that entries created, renamed or removed in it
/// survive a crash.
///
/// - **Unix:** `fsync(2)` on the directory, which that page says is needed
///   for a new entry to be durable. `EINVAL` (a file system that cannot sync
///   directories) gives [`DirSync::Unsupported`].
/// - **Windows:** the directory is opened with `FILE_FLAG_BACKUP_SEMANTICS`
///   and write access, then `FlushFileBuffers`. Microsoft documents that the
///   call needs `GENERIC_WRITE`; whether a directory handle accepts it is
///   **UNVERIFIED** until the Windows CI test records it. An access-denied,
///   invalid-function, invalid-parameter or not-supported refusal gives
///   [`DirSync::Unsupported`].
///
/// Any other error (missing directory, not a directory) is returned.
pub fn fsync_dir(path: &Path) -> io::Result<DirSync> {
    imp::fsync_dir(path)
}

/// Create the directory `path` if it is absent (mode 0700 on Unix), and
/// then flush its parent **even when it already existed**: a crash may have
/// left an earlier creation unflushed.
///
/// An existing `path` must be a real directory: a link, junction or file
/// there is an `AlreadyExists` error. Only the last component is created;
/// a missing parent is `NotFound`.
pub fn create_dir_durable(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => ensure_real_dir(&meta)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => match new_private_dir(path) {
            Ok(()) => {}
            // Created concurrently: accept it only if it is a real directory.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                ensure_real_dir(&fs::symlink_metadata(path)?)?;
            }
            Err(e) => return Err(e),
        },
        Err(e) => return Err(e),
    }
    let parent = match path.parent() {
        Some(p) if p.as_os_str().is_empty() => Path::new("."),
        Some(p) => p,
        // A root: nothing above it to flush.
        None => return Ok(()),
    };
    fsync_dir(parent).map(|_| ())
}

fn ensure_real_dir(meta: &fs::Metadata) -> io::Result<()> {
    if meta.is_dir() && !is_link_or_reparse(meta) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the path exists and is not a real directory",
        ))
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

/// `true` for a symbolic link and, on Windows, for any reparse point
/// (junctions included), whatever std's `is_symlink` makes of it.
fn is_link_or_reparse(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// How [`place_noreplace`] placed the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placed {
    /// Linux `renameat2(RENAME_NOREPLACE)`: atomic, and refused by the
    /// kernel when the target exists.
    Renamed,
    /// A hard link to the target, then removal of the source. `link(2)` and
    /// `CreateHardLinkW` both refuse an existing target. When removing the
    /// source failed, `source_removed` is `false`: the target is placed, and
    /// the leftover source is the caller's to clean up.
    Linked { source_removed: bool },
    /// The volume has neither: an existence check, then a plain rename. Not
    /// atomic against a concurrent writer, so callers use it only while
    /// they hold a lock that excludes other writers of the target directory.
    CheckedRename,
}

/// Move the file `from` to `to`, never replacing an existing `to`.
///
/// An existing target is an error of kind [`io::ErrorKind::AlreadyExists`],
/// and `from` is left in place. The strategies are tried in order, each
/// falling back to the next only when the volume does not support it:
///
/// 1. Linux: `renameat2` with `RENAME_NOREPLACE`. `EINVAL` (the file system
///    does not support the flag, per rename(2)) or `ENOSYS` falls back.
/// 2. Hard link, then remove the source. A volume without hard links
///    (`EPERM`/`ENOTSUP` on Linux FAT, invalid-function or not-supported on
///    Windows FAT and exFAT) falls back.
/// 3. Existence check, then rename (see [`Placed::CheckedRename`]).
///
/// That `CreateHardLinkW` refuses an existing target is checked by the
/// cross-platform tests, which run on Windows CI.
pub fn place_noreplace(from: &Path, to: &Path) -> io::Result<Placed> {
    place_from(Step::first(), from, to)
}

/// One placement strategy, in fallback order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    #[cfg(target_os = "linux")]
    RenameNoReplace,
    Link,
    CheckedRename,
}

impl Step {
    fn first() -> Step {
        #[cfg(target_os = "linux")]
        {
            Step::RenameNoReplace
        }
        #[cfg(not(target_os = "linux"))]
        {
            Step::Link
        }
    }
}

/// Run the strategies from `step` onwards. Separate from
/// [`place_noreplace`] so every fallback is exercised by tests, whatever
/// the test machine's file system supports.
fn place_from(mut step: Step, from: &Path, to: &Path) -> io::Result<Placed> {
    loop {
        match step {
            #[cfg(target_os = "linux")]
            Step::RenameNoReplace => match rename_noreplace(from, to) {
                Ok(()) => return Ok(Placed::Renamed),
                Err(e) if noreplace_unsupported(&e) => step = Step::Link,
                Err(e) => return Err(e),
            },
            Step::Link => match link_then_remove(from, to) {
                Ok(placed) => return Ok(placed),
                Err(e) if links_unsupported(&e) => step = Step::CheckedRename,
                Err(e) => return Err(e),
            },
            Step::CheckedRename => return checked_rename(from, to),
        }
    }
}

#[cfg(target_os = "linux")]
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use rustix::fs::{renameat_with, RenameFlags, CWD};
    renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE).map_err(io::Error::from)
}

/// `true` when `renameat2(RENAME_NOREPLACE)` is not available here.
#[cfg(target_os = "linux")]
fn noreplace_unsupported(e: &io::Error) -> bool {
    use rustix::io::Errno;
    matches!(
        e.raw_os_error(),
        Some(code) if code == Errno::INVAL.raw_os_error() || code == Errno::NOSYS.raw_os_error()
    )
}

fn link_then_remove(from: &Path, to: &Path) -> io::Result<Placed> {
    fs::hard_link(from, to)?;
    let source_removed = fs::remove_file(from).is_ok();
    Ok(Placed::Linked { source_removed })
}

/// `true` when the volume cannot create hard links.
fn links_unsupported(e: &io::Error) -> bool {
    let Some(code) = e.raw_os_error() else {
        return false;
    };
    #[cfg(target_os = "linux")]
    {
        use rustix::io::Errno;
        // link(2): EPERM "the file system does not support the creation of
        // hard links" (FAT); EOPNOTSUPP (== ENOTSUP on Linux) on others.
        code == Errno::PERM.raw_os_error() || code == Errno::OPNOTSUPP.raw_os_error()
    }
    #[cfg(windows)]
    {
        // ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED.
        matches!(code, 1 | 50)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = code;
        false
    }
}

fn checked_rename(from: &Path, to: &Path) -> io::Result<Placed> {
    match fs::symlink_metadata(to) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the placement target already exists",
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::rename(from, to)?;
            Ok(Placed::CheckedRename)
        }
        Err(e) => Err(e),
    }
}

/// Why [`open_nofollow`] refused a path that exists. Carried inside the
/// returned `io::Error`; read it with [`open_refusal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum OpenRefusal {
    /// The last component is a symbolic link (or, on Windows, any reparse
    /// point, junctions included).
    #[error("refused to open a link")]
    Link,
    /// Not a regular file (a directory, FIFO, device or socket).
    #[error("refused to open something that is not a regular file")]
    NotRegular,
}

/// The [`OpenRefusal`] inside an error from [`open_nofollow`], if any.
pub fn open_refusal(e: &io::Error) -> Option<OpenRefusal> {
    e.get_ref()?.downcast_ref::<OpenRefusal>().copied()
}

fn refusal(r: OpenRefusal) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, r)
}

/// Open `path` read-only without following a link in its last component,
/// and confirm through the open handle that it is a regular file.
///
/// - **Linux:** `openat(AT_FDCWD, path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC |
///   O_NONBLOCK | O_NOCTTY)`. A link gives `ELOOP`, reported as
///   [`OpenRefusal::Link`]. `O_NONBLOCK` keeps a planted FIFO from blocking
///   the open; it has no effect on reads of a regular file.
/// - **Windows:** `FILE_FLAG_OPEN_REPARSE_POINT`, so a link or junction is
///   opened as itself, then refused by its reparse-point attribute.
///
/// Links in earlier components are followed: callers check the directories
/// they own separately (with `symlink_metadata` or [`crate::check_private`]).
pub fn open_nofollow(path: &Path) -> io::Result<File> {
    let file = imp::open_nofollow(path)?;
    let meta = file.metadata()?;
    if is_link_or_reparse(&meta) {
        return Err(refusal(OpenRefusal::Link));
    }
    if !meta.is_file() {
        return Err(refusal(OpenRefusal::NotRegular));
    }
    Ok(file)
}

#[cfg(unix)]
mod imp {
    use super::DirSync;
    use std::fs::File;
    use std::io;
    use std::path::Path;

    pub(super) fn fsync_dir(path: &Path) -> io::Result<DirSync> {
        let dir = File::open(path)?;
        if !dir.metadata()?.is_dir() {
            return Err(io::Error::from(io::ErrorKind::NotADirectory));
        }
        match dir.sync_all() {
            Ok(()) => Ok(DirSync::Synced),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
                ) =>
            {
                Ok(DirSync::Unsupported)
            }
            Err(e) => Err(e),
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn open_nofollow(path: &Path) -> io::Result<File> {
        use rustix::fs::{openat, Mode, OFlags, CWD};
        use rustix::io::Errno;
        let flags =
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOCTTY;
        match openat(CWD, path, flags, Mode::empty()) {
            Ok(fd) => Ok(File::from(fd)),
            Err(Errno::LOOP) => Err(super::refusal(super::OpenRefusal::Link)),
            Err(e) => Err(io::Error::from(e)),
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) fn open_nofollow(_path: &Path) -> io::Result<File> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "open_nofollow is implemented for Linux and Windows only",
        ))
    }
}

#[cfg(windows)]
mod imp {
    use super::{DirSync, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT};
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::Path;

    /// ERROR_INVALID_FUNCTION, ERROR_ACCESS_DENIED, ERROR_NOT_SUPPORTED,
    /// ERROR_INVALID_PARAMETER: the ways a directory flush is refused.
    fn refused(e: &io::Error) -> bool {
        matches!(e.raw_os_error(), Some(1 | 5 | 50 | 87))
    }

    pub(super) fn fsync_dir(path: &Path) -> io::Result<DirSync> {
        // The read-only open proves the directory exists and is one, so a
        // refusal of the write open below is about flushing, not the path.
        let probe = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        if !probe.metadata()?.is_dir() {
            return Err(io::Error::from(io::ErrorKind::NotADirectory));
        }
        drop(probe);
        let dir = match OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
        {
            Ok(dir) => dir,
            Err(e) if refused(&e) => return Ok(DirSync::Unsupported),
            Err(e) => return Err(e),
        };
        match dir.sync_all() {
            Ok(()) => Ok(DirSync::Synced),
            Err(e) if refused(&e) => Ok(DirSync::Unsupported),
            Err(e) => Err(e),
        }
    }

    pub(super) fn open_nofollow(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::path::PathBuf;

    fn write(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).expect("write");
    }

    fn read(path: &Path) -> Vec<u8> {
        fs::read(path).expect("read")
    }

    /// Every strategy that runs on this platform, in fallback order.
    fn steps() -> Vec<Step> {
        vec![
            #[cfg(target_os = "linux")]
            Step::RenameNoReplace,
            Step::Link,
            Step::CheckedRename,
        ]
    }

    #[test]
    fn fsync_dir_flushes_a_directory_and_refuses_a_file_or_missing_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = fsync_dir(tmp.path()).expect("fsync_dir");
        // Recorded for docs/compatibility.md; Linux local file systems sync.
        eprintln!("fsync_dir on the temp directory: {result:?}");
        #[cfg(target_os = "linux")]
        assert_eq!(result, DirSync::Synced);

        let file = tmp.path().join("f");
        write(&file, b"x");
        assert!(fsync_dir(&file).is_err());
        assert!(fsync_dir(&tmp.path().join("missing")).is_err());
    }

    #[test]
    fn create_dir_durable_creates_once_and_accepts_an_existing_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("attachments");
        create_dir_durable(&dir).expect("create");
        assert!(dir.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&dir).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{mode:o}");
        }
        // Existing: still succeeds (and flushes the parent again).
        create_dir_durable(&dir).expect("again");
        // A missing parent is not created.
        let deep = tmp.path().join("no").join("such");
        assert_eq!(
            create_dir_durable(&deep).expect_err("deep").kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn create_dir_durable_refuses_a_file_in_the_way() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("imports");
        write(&file, b"x");
        assert_eq!(
            create_dir_durable(&file).expect_err("file").kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(read(&file), b"x");
    }

    #[cfg(unix)]
    #[test]
    fn create_dir_durable_refuses_a_link_to_a_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real = tmp.path().join("real");
        fs::create_dir(&real).expect("mkdir");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        assert_eq!(
            create_dir_durable(&link).expect_err("link").kind(),
            io::ErrorKind::AlreadyExists
        );
    }

    #[test]
    fn create_dir_durable_accepts_a_relative_single_component() {
        // Exercises the "." parent without changing the process's working
        // directory: a relative path that already exists as a directory.
        let rel = PathBuf::from("src");
        if rel.is_dir() {
            create_dir_durable(&rel).expect("existing relative directory");
        }
    }

    #[test]
    fn place_noreplace_moves_into_an_empty_slot() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let from = tmp.path().join("a.partial");
        let to = tmp.path().join("a");
        write(&from, b"payload");
        let placed = place_noreplace(&from, &to).expect("place");
        eprintln!("place_noreplace on the temp directory: {placed:?}");
        #[cfg(target_os = "linux")]
        assert_eq!(placed, Placed::Renamed);
        assert_eq!(read(&to), b"payload");
        assert!(!from.exists());
    }

    #[test]
    fn every_strategy_places_and_none_replaces_an_existing_target() {
        for step in steps() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let from = tmp.path().join("new.partial");
            let to = tmp.path().join("target");

            // Empty slot: placed, source gone.
            write(&from, b"new");
            let placed = place_from(step, &from, &to).expect("place");
            let expected = match step {
                #[cfg(target_os = "linux")]
                Step::RenameNoReplace => Placed::Renamed,
                Step::Link => Placed::Linked {
                    source_removed: true,
                },
                Step::CheckedRename => Placed::CheckedRename,
            };
            assert_eq!(placed, expected, "{step:?}");
            assert_eq!(read(&to), b"new", "{step:?}");
            assert!(!from.exists(), "{step:?}");

            // Occupied slot: AlreadyExists; both files untouched.
            write(&from, b"other");
            let err = place_from(step, &from, &to).expect_err("must not replace");
            assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{step:?}");
            assert_eq!(read(&to), b"new", "{step:?}");
            assert_eq!(read(&from), b"other", "{step:?}");
        }
    }

    #[test]
    fn an_existing_directory_or_link_at_the_target_is_never_replaced() {
        for step in steps() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let from = tmp.path().join("new.partial");
            write(&from, b"new");
            let dir_target = tmp.path().join("dir");
            fs::create_dir(&dir_target).expect("mkdir");
            let err = place_from(step, &from, &dir_target).expect_err("dir target");
            // A rename onto a directory fails with a different kind on some
            // platforms; what matters is that nothing moved.
            let _ = err;
            assert!(dir_target.is_dir(), "{step:?}");
            assert_eq!(read(&from), b"new", "{step:?}");

            #[cfg(unix)]
            {
                let elsewhere = tmp.path().join("elsewhere");
                write(&elsewhere, b"keep");
                let link_target = tmp.path().join("link");
                std::os::unix::fs::symlink(&elsewhere, &link_target).expect("symlink");
                let err = place_from(step, &from, &link_target).expect_err("link target");
                assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{step:?}");
                assert!(fs::symlink_metadata(&link_target)
                    .expect("meta")
                    .file_type()
                    .is_symlink());
                assert_eq!(read(&elsewhere), b"keep", "{step:?}");
            }
        }
    }

    #[test]
    fn hard_link_refuses_an_existing_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        write(&a, b"a");
        write(&b, b"b");
        let err = fs::hard_link(&a, &b).expect_err("existing target");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(read(&b), b"b");
    }

    #[test]
    fn a_missing_source_is_an_error_at_every_step_and_creates_nothing() {
        for step in steps() {
            let tmp = tempfile::tempdir().expect("tempdir");
            let to = tmp.path().join("target");
            let err = place_from(step, &tmp.path().join("missing"), &to).expect_err("missing");
            assert_eq!(err.kind(), io::ErrorKind::NotFound, "{step:?}");
            assert!(!to.exists(), "{step:?}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_fallback_classifiers_match_the_documented_errnos() {
        use rustix::io::Errno;
        let e = |errno: Errno| io::Error::from_raw_os_error(errno.raw_os_error());
        assert!(noreplace_unsupported(&e(Errno::INVAL)));
        assert!(noreplace_unsupported(&e(Errno::NOSYS)));
        assert!(!noreplace_unsupported(&e(Errno::EXIST)));
        assert!(!noreplace_unsupported(&e(Errno::ACCESS)));
        assert!(links_unsupported(&e(Errno::PERM)));
        assert!(links_unsupported(&e(Errno::OPNOTSUPP)));
        assert!(!links_unsupported(&e(Errno::EXIST)));
        assert!(!links_unsupported(&e(Errno::NOENT)));
        assert!(!links_unsupported(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
    }

    #[cfg(windows)]
    #[test]
    fn the_fallback_classifier_matches_the_documented_windows_errors() {
        assert!(links_unsupported(&io::Error::from_raw_os_error(1)));
        assert!(links_unsupported(&io::Error::from_raw_os_error(50)));
        assert!(!links_unsupported(&io::Error::from_raw_os_error(183)));
        assert!(!links_unsupported(&io::Error::from_raw_os_error(5)));
    }

    #[test]
    fn open_nofollow_reads_a_regular_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("blob");
        write(&file, b"bytes");
        let mut f = open_nofollow(&file).expect("open");
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).expect("read");
        assert_eq!(buf, b"bytes");
        let missing = open_nofollow(&tmp.path().join("missing")).expect_err("missing");
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        assert_eq!(open_refusal(&missing), None);
    }

    #[cfg(unix)]
    #[test]
    fn open_nofollow_refuses_a_link_a_directory_and_a_fifo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("target");
        write(&target, b"secret");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let err = open_nofollow(&link).expect_err("link");
        assert_eq!(open_refusal(&err), Some(OpenRefusal::Link));

        let err = open_nofollow(tmp.path()).expect_err("directory");
        assert_eq!(open_refusal(&err), Some(OpenRefusal::NotRegular));

        let fifo = tmp.path().join("fifo");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if matches!(made, Ok(s) if s.success()) {
            // Must return at once (O_NONBLOCK) and refuse.
            let err = open_nofollow(&fifo).expect_err("fifo");
            assert_eq!(open_refusal(&err), Some(OpenRefusal::NotRegular));
        } else {
            eprintln!("skipped the FIFO case: mkfifo is not available");
        }
    }

    #[test]
    fn refusals_are_recognisable_and_other_errors_are_not() {
        let r = refusal(OpenRefusal::Link);
        assert_eq!(open_refusal(&r), Some(OpenRefusal::Link));
        assert_eq!(open_refusal(&io::Error::other("x")), None);
        assert_eq!(open_refusal(&io::Error::from_raw_os_error(2)), None);
    }
}
