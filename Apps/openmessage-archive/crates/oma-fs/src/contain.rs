//! Resolving untrusted relative paths without letting them leave a root.

use std::path::{Path, PathBuf};

use crate::filename::sanitize_filename;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("path is empty")]
    Empty,
    #[error("path is absolute or carries a drive or root prefix")]
    Absolute,
    #[error("path contains a parent-directory component ('..')")]
    ParentComponent,
    #[error("path component {index} is not a safe filename")]
    UnsafeComponent { index: usize },
}

/// Join an untrusted relative path (for example a `relativePath` from an
/// iPhone backup manifest) onto `root`, or refuse.
///
/// Both `/` and `\` are treated as separators on every platform, so a path
/// that is harmless on Linux cannot become a traversal on Windows. `.` and
/// empty segments (`a//b`) are dropped. Everything else fails closed:
///
/// - absolute paths, a leading separator, or a drive/UNC prefix;
/// - any `..` component, even one that would stay inside `root` — a manifest
///   has no legitimate reason to contain one;
/// - any component that [`sanitize_filename`] would alter (Windows device
///   names, `:` streams, control characters, bidi overrides, trailing dots).
///
/// This is a lexical check. It does not resolve symlinks; callers that write
/// into `root` must create it themselves and must not follow links planted
/// inside it. The returned path is `root` joined with the cleaned components.
pub fn resolve_within(root: &Path, untrusted_relative: &str) -> Result<PathBuf, PathError> {
    if untrusted_relative.is_empty() {
        return Err(PathError::Empty);
    }
    if untrusted_relative.starts_with(['/', '\\']) || has_drive_prefix(untrusted_relative) {
        return Err(PathError::Absolute);
    }

    let mut resolved = root.to_path_buf();
    let mut pushed = 0usize;
    for (index, component) in untrusted_relative.split(['/', '\\']).enumerate() {
        match component {
            "" | "." => continue,
            ".." => return Err(PathError::ParentComponent),
            _ if sanitize_filename(component) != component => {
                return Err(PathError::UnsafeComponent { index });
            }
            _ => {
                resolved.push(component);
                pushed += 1;
            }
        }
    }
    if pushed == 0 {
        return Err(PathError::Empty);
    }
    Ok(resolved)
}

fn has_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("archive-root")
    }

    #[test]
    fn ordinary_backup_paths_resolve_under_root() {
        let got = resolve_within(
            &root(),
            "Library/SMS/Attachments/0a/10/AB12CD34-0000/IMG_0001.HEIC",
        )
        .expect("valid path");
        assert_eq!(
            got,
            root()
                .join("Library")
                .join("SMS")
                .join("Attachments")
                .join("0a")
                .join("10")
                .join("AB12CD34-0000")
                .join("IMG_0001.HEIC")
        );
        assert!(got.starts_with(root()));
    }

    #[test]
    fn dot_and_empty_segments_are_ignored() {
        assert_eq!(
            resolve_within(&root(), "a/./b//c").expect("valid"),
            root().join("a").join("b").join("c")
        );
    }

    #[test]
    fn traversal_is_rejected_on_either_separator() {
        for bad in [
            "..",
            "../x",
            "a/../../x",
            "a/b/..",
            "a\\..\\..\\x",
            "Library/SMS/../../../../etc/passwd",
        ] {
            assert_eq!(
                resolve_within(&root(), bad),
                Err(PathError::ParentComponent),
                "{bad}"
            );
        }
    }

    #[test]
    fn absolute_and_prefixed_paths_are_rejected() {
        for bad in [
            "/etc/passwd",
            "\\Windows",
            "C:\\Windows",
            "c:relative",
            "\\\\server\\share",
        ] {
            assert_eq!(
                resolve_within(&root(), bad),
                Err(PathError::Absolute),
                "{bad}"
            );
        }
    }

    #[test]
    fn unsafe_components_are_rejected_with_their_index() {
        assert_eq!(
            resolve_within(&root(), "a/CON/b"),
            Err(PathError::UnsafeComponent { index: 1 })
        );
        assert_eq!(
            resolve_within(&root(), "a/file.txt:stream"),
            Err(PathError::UnsafeComponent { index: 1 })
        );
        assert_eq!(
            resolve_within(&root(), "trailing./x"),
            Err(PathError::UnsafeComponent { index: 0 })
        );
        assert_eq!(
            resolve_within(&root(), "bidi\u{202E}gpj.exe"),
            Err(PathError::UnsafeComponent { index: 0 })
        );
    }

    #[test]
    fn paths_with_no_real_components_are_empty() {
        for bad in ["", ".", "./", "a/.."] {
            assert!(resolve_within(&root(), bad).is_err(), "{bad}");
        }
        assert_eq!(resolve_within(&root(), "./."), Err(PathError::Empty));
    }
}
