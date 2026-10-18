//! Is a file or directory private to the user running the app?
//!
//! Archives and staging hold plaintext message data, so on Unix every
//! directory and database file the app owns must belong to the effective
//! user and carry no group or other permission bits. A volume that does not
//! keep POSIX modes (FAT, exFAT, NTFS on Linux, many network shares) reports
//! modes from its mount options instead, and fails this check.
//!
//! The check reads metadata only; it never creates, writes or changes
//! anything, so it is safe in a read-only open.
//!
//! On Windows there are no POSIX modes, and files inherit the parent's ACL.
//! The check there confirms only that the path is a real (non-link) object of
//! the expected type; the inherited ACL is disclosed in `docs/privacy.md`
//! instead.

use std::fs;
use std::io;
use std::path::Path;

/// What [`check_private`] expects to find.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    Dir,
    File,
}

impl std::fmt::Display for Expect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Expect::Dir => "directory",
            Expect::File => "regular file",
        })
    }
}

/// Why a path is not private. Carries no path, so it is safe to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PermProblem {
    /// The metadata could not be read (missing, or permission denied).
    #[error("could not be inspected ({0:?})")]
    Io(io::ErrorKind),
    /// The path is a symbolic link (or, on Windows, a junction).
    #[error("is a link")]
    Link,
    /// The path exists but is not what was expected.
    #[error("is not a {expected}")]
    WrongType { expected: Expect },
    /// Owned by a user other than the effective user (Unix).
    #[error("is owned by another user")]
    WrongOwner,
    /// Group or other permission bits are set (Unix). `mode` is the full
    /// `st_mode`, including the file-type bits.
    #[error("is readable or writable by other users (mode {mode:o})")]
    Loose { mode: u32 },
    /// This platform has no way to read the effective user without `unsafe`
    /// (Unix other than Linux; not a supported target).
    #[error("ownership cannot be checked on this platform")]
    Unsupported,
}

/// Check that `path` is a real `kind` (not a link), and, on Unix, that it
/// is owned by the effective user with no group or other permission bits.
/// Links are not followed. See the module documentation for Windows.
pub fn check_private(path: &Path, kind: Expect) -> Result<(), PermProblem> {
    let meta = fs::symlink_metadata(path).map_err(|e| PermProblem::Io(e.kind()))?;
    if meta.file_type().is_symlink() {
        return Err(PermProblem::Link);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Any reparse point (junctions included) is refused, whatever std's
        // `is_symlink` makes of it.
        if meta.file_attributes() & crate::place::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(PermProblem::Link);
        }
    }
    let type_ok = match kind {
        Expect::Dir => meta.is_dir(),
        Expect::File => meta.is_file(),
    };
    if !type_ok {
        return Err(PermProblem::WrongType { expected: kind });
    }
    owner_and_mode(&meta)
}

#[cfg(target_os = "linux")]
fn owner_and_mode(meta: &fs::Metadata) -> Result<(), PermProblem> {
    use std::os::unix::fs::MetadataExt;
    let euid = rustix::process::geteuid().as_raw();
    mode_and_owner_verdict(meta.mode(), meta.uid(), euid)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn owner_and_mode(_meta: &fs::Metadata) -> Result<(), PermProblem> {
    Err(PermProblem::Unsupported)
}

#[cfg(not(unix))]
fn owner_and_mode(_meta: &fs::Metadata) -> Result<(), PermProblem> {
    Ok(())
}

/// The Unix rule as a pure function: owner first, then mode.
#[cfg_attr(not(unix), allow(dead_code))]
fn mode_and_owner_verdict(mode: u32, uid: u32, euid: u32) -> Result<(), PermProblem> {
    if uid != euid {
        return Err(PermProblem::WrongOwner);
    }
    if mode & 0o077 != 0 {
        return Err(PermProblem::Loose { mode });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unix_rule_as_a_table() {
        assert_eq!(mode_and_owner_verdict(0o040700, 1000, 1000), Ok(()));
        assert_eq!(mode_and_owner_verdict(0o100600, 1000, 1000), Ok(()));
        assert_eq!(mode_and_owner_verdict(0o100400, 0, 0), Ok(()));
        for mode in [0o040755, 0o040770, 0o040701, 0o100640, 0o100604] {
            assert_eq!(
                mode_and_owner_verdict(mode, 1000, 1000),
                Err(PermProblem::Loose { mode }),
                "{mode:o}"
            );
        }
        // Owner is checked first: a foreign 0700 directory is not ours.
        assert_eq!(
            mode_and_owner_verdict(0o040700, 0, 1000),
            Err(PermProblem::WrongOwner)
        );
    }

    #[test]
    fn missing_paths_and_wrong_types_are_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("missing");
        assert_eq!(
            check_private(&missing, Expect::Dir),
            Err(PermProblem::Io(io::ErrorKind::NotFound))
        );
        let file = tmp.path().join("f");
        fs::write(&file, b"x").expect("write");
        assert_eq!(
            check_private(&file, Expect::Dir),
            Err(PermProblem::WrongType {
                expected: Expect::Dir
            })
        );
        assert_eq!(
            check_private(tmp.path(), Expect::File),
            Err(PermProblem::WrongType {
                expected: Expect::File
            })
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn private_modes_pass_and_loose_modes_fail_on_disk() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("d");
        fs::create_dir(&dir).expect("mkdir");
        let file = dir.join("f");
        fs::write(&file, b"x").expect("write");

        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("chmod");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("chmod");
        assert_eq!(check_private(&dir, Expect::Dir), Ok(()));
        assert_eq!(check_private(&file, Expect::File), Ok(()));

        fs::set_permissions(&dir, fs::Permissions::from_mode(0o750)).expect("chmod");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("chmod");
        assert!(matches!(
            check_private(&dir, Expect::Dir),
            Err(PermProblem::Loose { .. })
        ));
        assert!(matches!(
            check_private(&file, Expect::File),
            Err(PermProblem::Loose { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_refused_even_to_a_private_target() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("d");
        fs::create_dir(&dir).expect("mkdir");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("chmod");
        let link = tmp.path().join("l");
        std::os::unix::fs::symlink(&dir, &link).expect("symlink");
        assert_eq!(check_private(&link, Expect::Dir), Err(PermProblem::Link));
    }

    #[test]
    fn problems_display_without_a_path() {
        let shown = PermProblem::Loose { mode: 0o040755 }.to_string();
        assert_eq!(shown, "is readable or writable by other users (mode 40755)");
        assert_eq!(
            PermProblem::WrongType {
                expected: Expect::File
            }
            .to_string(),
            "is not a regular file"
        );
    }
}
