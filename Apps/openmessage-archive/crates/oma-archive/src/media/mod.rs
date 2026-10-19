//! Media in the crate (M5 spec §5, slice 5d): what an attachment's bytes
//! are, whether a container may be streamed, and how an image becomes a
//! rendition.
//!
//! - [`sniff`]: the clean-room magic table (§5.2). The stored type is a
//!   label only.
//! - [`container`]: the structural check before streaming (§5.3).
//! - [`render`]: the pure function that turns verified image bytes into a
//!   re-encoded rendition, run **only inside the decode helper process**
//!   (§5.5, D113).
//! - The [`ArchiveReader`] methods here are the viewer's blob readers
//!   (§5.4), all over `blobs.rs`, the one module that opens store files:
//!   - [`ArchiveReader::inspect_blob`] sniffs the first 4 KiB, the one read
//!     of a file that has not been verified;
//!   - [`ArchiveReader::verify_blob`] hashes the whole file, cancellably,
//!     and records its identity ([`BlobIdentity`]);
//!   - [`ArchiveReader::check_container`] and [`ArchiveReader::open_range`]
//!     need that identity and refuse a file whose identity changed since;
//!   - [`ArchiveReader::read_blob_verified`] reads a whole file to the end
//!     through the hashing reader, for the decode helper: images are decoded
//!     only from that buffer, never from a second open.
//! - **Integrity, not safety.** `verify_blob` proves the bytes are the ones
//!   imported; a file crafted by its sender passes it.
//! - **Remaining time-of-check gap.** On Windows the identity is `(len,
//!   mtime)` only, so a same-size rewrite that keeps the mtime is not
//!   detected; on Unix an in-place rewrite that keeps the inode, size and
//!   mtime is not either (§5.4, §19). The app never replaces placed files.
//!
//! These modules open no file themselves (`tests/source_rules.rs`), and
//! nothing here logs a byte, a name or an id.

pub mod container;
pub mod render;
pub mod sniff;

use std::io::Read;
use std::time::{Duration, Instant};

use oma_messages::Redacted;
use rusqlite::types::ValueRef;
use rusqlite::OptionalExtension;

pub use container::{
    ContainerRefusal, ContainerVerdict, ReadAt, CONTAINER_BOXES, MAX_BOXES, MAX_DEPTH,
};
pub use render::{
    render, RenderError, RenderLimits, Rendered, RenderedFormat, Rendition, DECODER_ALLOC,
    JPEG_QUALITY, MAX_DECODED_BYTES, MAX_INPUT_BYTES, MAX_SIDE,
};
pub use sniff::{sniff, MediaClass, SniffedKind, SNIFF_WINDOW};

use crate::blobs::{self, FileIdentity, RangeReader, StorageName};
use crate::conn::ViewBudget;
use crate::error::{ArchiveError, BlobProblem};
use crate::progress::CancelToken;
use crate::reader::{ArchiveReader, BlobId};

/// How long one structural check may take (§5.3).
pub const CONTAINER_CHECK_TIME: Duration = Duration::from_secs(2);
/// The read size of `verify_blob`; the cancel token is checked between
/// reads.
const VERIFY_CHUNK: usize = 1 << 20;

/// What [`ArchiveReader::inspect_blob`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Sniffed {
    pub kind: SniffedKind,
    /// What the viewer offers for it.
    pub class: MediaClass,
    /// `blob.byte_len`: the stored size.
    pub stored_bytes: u64,
}

/// The identity a successful [`ArchiveReader::verify_blob`] recorded: the
/// only key to [`ArchiveReader::check_container`] and
/// [`ArchiveReader::open_range`]. It cannot be built any other way, so a
/// blob can be streamed only after it was verified (the shell keeps these
/// per viewer session). `Debug` prints nothing of it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BlobIdentity {
    blob: BlobId,
    file: FileIdentity,
}

impl std::fmt::Debug for BlobIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BlobIdentity(..)")
    }
}

impl BlobIdentity {
    /// The blob it was recorded for.
    pub fn blob(&self) -> BlobId {
        self.blob
    }

    /// The file's length when it was verified.
    pub fn len(&self) -> u64 {
        self.file.len()
    }

    /// `true` for an empty file.
    pub fn is_empty(&self) -> bool {
        self.file.len() == 0
    }
}

const SQL_BLOB_ROW: &str = "SELECT sha256, byte_len, storage_name FROM blob WHERE id = ?1";

/// Every media statement whose plan depends on an index (pinned in
/// `tests/query_plans.rs`).
#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    vec![("viewer blob row", SQL_BLOB_ROW.to_owned())]
}

fn io_problem(e: &std::io::Error) -> ArchiveError {
    if e.kind() == std::io::ErrorKind::InvalidData {
        ArchiveError::Blob(BlobProblem::DigestMismatch)
    } else {
        ArchiveError::Blob(BlobProblem::Io(e.kind()))
    }
}

impl ArchiveReader {
    /// The parsed storage name and `byte_len` of blob `id`, under the
    /// `Lookup` phase in its own snapshot (no read transaction is held
    /// while a file is read). The name must spell the row's `sha256`.
    fn blob_row(&self, id: BlobId) -> Result<(StorageName, i64), ArchiveError> {
        self.require_current()?;
        let conn = self.conn();
        let deadline = Instant::now() + ViewBudget::Lookup.window();
        let row: Option<(Vec<u8>, i64, Option<String>)> = conn.snapshot(|| {
            conn.viewing(ViewBudget::Lookup, None, deadline, |p| {
                p.sql(|c| {
                    c.prepare(SQL_BLOB_ROW)?
                        .query_row([id.get()], |r| {
                            let sha = match r.get_ref(0)? {
                                ValueRef::Blob(b) => b.to_vec(),
                                _ => Vec::new(),
                            };
                            let len = match r.get_ref(1)? {
                                ValueRef::Integer(n) => n,
                                _ => -1,
                            };
                            let name = match r.get_ref(2)? {
                                ValueRef::Text(t) => std::str::from_utf8(t).ok().map(str::to_owned),
                                _ => None,
                            };
                            Ok((sha, len, name))
                        })
                        .optional()
                })
            })
        })?;
        let (sha, len, name) = row.ok_or(ArchiveError::Blob(BlobProblem::NotFound))?;
        let name = name
            .as_deref()
            .and_then(StorageName::parse)
            .filter(|n| n.sha256().is_some_and(|s| s.as_slice() == sha.as_slice()))
            .ok_or(ArchiveError::Blob(BlobProblem::BadName))?;
        Ok((name, len))
    }

    /// Sniff blob `id` (§5.2): its first [`SNIFF_WINDOW`] bytes, read after
    /// the store checks of `open_blob` but **not hashed** (the one
    /// exception to "verified before use"; the bytes go only to the
    /// sniffer).
    pub fn inspect_blob(&self, id: BlobId) -> Result<Sniffed, ArchiveError> {
        let (name, len) = self.blob_row(id)?;
        let head = blobs::read_sniff_window(self.dir(), &name, len).map_err(ArchiveError::Blob)?;
        let stored_bytes = u64::try_from(len).unwrap_or(0);
        let kind = sniff(&head, stored_bytes);
        Ok(Sniffed {
            kind,
            class: kind.class(),
            stored_bytes,
        })
    }

    /// Hash all of blob `id` against its digest (§5.4), stopping at the
    /// next 1 MiB read once `cancel` is set, and record the file's
    /// identity. A different digest is
    /// [`BlobProblem::DigestMismatch`]; a file whose identity changed while
    /// it was read is [`BlobProblem::Changed`]. Integrity only: a file
    /// crafted by its sender passes.
    pub fn verify_blob(
        &self,
        id: BlobId,
        cancel: &CancelToken,
    ) -> Result<BlobIdentity, ArchiveError> {
        let (name, len) = self.blob_row(id)?;
        let mut reader = blobs::open_checked(self.dir(), &name, len).map_err(ArchiveError::Blob)?;
        let before = reader
            .identity()
            .map_err(|e| ArchiveError::Blob(BlobProblem::Io(e.kind())))?;
        let mut buf = vec![0u8; VERIFY_CHUNK];
        loop {
            if cancel.is_cancelled() {
                return Err(ArchiveError::Cancelled);
            }
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(io_problem(&e)),
            }
        }
        let after = reader
            .identity()
            .map_err(|e| ArchiveError::Blob(BlobProblem::Io(e.kind())))?;
        if after != before {
            return Err(ArchiveError::Blob(BlobProblem::Changed));
        }
        Ok(BlobIdentity {
            blob: id,
            file: before,
        })
    }

    /// Open blob `id` for random access (§5.4): only with the identity
    /// [`ArchiveReader::verify_blob`] recorded for it
    /// ([`BlobProblem::NotVerified`] otherwise), and only while the file
    /// still has that identity ([`BlobProblem::Changed`]). Open one per
    /// request and drop it after.
    pub fn open_range(
        &self,
        id: BlobId,
        ident: &BlobIdentity,
    ) -> Result<RangeReader, ArchiveError> {
        if ident.blob != id {
            return Err(ArchiveError::Blob(BlobProblem::NotVerified));
        }
        let (name, len) = self.blob_row(id)?;
        blobs::open_range(self.dir(), &name, len, &ident.file).map_err(ArchiveError::Blob)
    }

    /// The structural check of blob `id`, sniffed as `kind` (§5.3), over the
    /// verified file through [`ArchiveReader::open_range`], within
    /// [`CONTAINER_CHECK_TIME`]. Stopped by `cancel` (`Cancelled`).
    pub fn check_container(
        &self,
        id: BlobId,
        ident: &BlobIdentity,
        kind: SniffedKind,
        cancel: &CancelToken,
    ) -> Result<ContainerVerdict, ArchiveError> {
        let mut range = self.open_range(id, ident)?;
        let deadline = Instant::now() + CONTAINER_CHECK_TIME;
        container::check(&mut range, kind, Some(cancel), deadline).map_err(|s| match s {
            container::CheckStop::Cancelled => ArchiveError::Cancelled,
            container::CheckStop::Io(k) => ArchiveError::Blob(BlobProblem::Io(k)),
        })
    }

    /// All of blob `id`, read to the end through the hashing reader (§5.4):
    /// the only bytes an image is decoded from. Over `max` bytes is
    /// [`BlobProblem::OverLimit`] (nothing is read); a different digest is
    /// [`BlobProblem::DigestMismatch`] and nothing is returned.
    pub fn read_blob_verified(
        &self,
        id: BlobId,
        max: u64,
    ) -> Result<Redacted<Vec<u8>>, ArchiveError> {
        let (name, len) = self.blob_row(id)?;
        let size =
            u64::try_from(len).map_err(|_| ArchiveError::Blob(BlobProblem::LengthMismatch))?;
        if size > max {
            return Err(ArchiveError::Blob(BlobProblem::OverLimit));
        }
        let mut reader = blobs::open_checked(self.dir(), &name, len).map_err(ArchiveError::Blob)?;
        let mut out = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
        reader.read_to_end(&mut out).map_err(|e| io_problem(&e))?;
        Ok(Redacted::new(out))
    }
}
