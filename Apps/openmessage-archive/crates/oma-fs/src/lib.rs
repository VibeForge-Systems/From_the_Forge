//! Filesystem safety primitives for OpenMessage Archive.
//!
//! Everything that turns untrusted text into a filesystem path goes through
//! this crate. iPhone backups are treated as hostile input: a manifest can name
//! any relative path, and a message attachment can carry any filename. The
//! functions here fail closed — a path that cannot be proven safe is rejected,
//! not "fixed up" into something that might still escape.
//!
//! The crate has no knowledge of the desktop shell, so the backup, extraction
//! and export libraries can depend on it directly.

mod contain;
mod filename;
mod location;

pub use contain::{resolve_within, PathError};
pub use filename::{sanitize_filename, MAX_FILENAME_BYTES};
pub use location::{
    cloud_sync_provider, enclosing_backup_dir, is_filesystem_root, is_within_imazing_backups,
    is_within_mobilesync, looks_like_backup_dir,
};
