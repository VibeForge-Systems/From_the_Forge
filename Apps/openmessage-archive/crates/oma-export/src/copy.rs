//! "Save a copy" of one attachment (M6 spec §6.6, slice 6k2).
//!
//! [`save_copy`] writes one attachment's stored file to the name the user
//! chose, with no manifest and no records:
//!
//! - **Bytes** come only from `ExportSnapshot::open_blob`, whose
//!   `BlobReader` hashes what it yields and fails its last read on a
//!   length or digest mismatch (§6.1). They go to the
//!   `.oma-export-<32 hex>.partial-file` beside the target
//!   ([`Staging::single_file`], [`Staging::create_verified`]), in 1 MiB
//!   chunks with the cancel token checked per chunk. A mismatch removes
//!   the partial file and refuses ([`ExportError::ChecksumMismatch`]:
//!   "The file does not match its recorded checksum and was not saved").
//! - **The kind** is sniffed again from the first 4 KiB of the same
//!   stream: when it differs from the kind the dialog's name was checked
//!   against, nothing is saved (`ConfirmationStale`), so a risky-extension
//!   confirmation is never applied to other bytes.
//! - **Placement** is [`Staging::publish`]: no-replace, or a replace of a
//!   regular file the user confirmed in the app (§8.4). Then a Mark of the
//!   Web on Windows ([`Staging::mark_published_file`], owner Q3c); a
//!   failure to write it is reported, never fatal (its effect is
//!   **UNVERIFIED**).
//! - **FAT** (Linux, `Fat(Msdos)`): a file of 4 GiB or more is refused
//!   before any byte is read.
//!
//! Nothing here logs.

use std::io::{self, Read, Write};
use std::path::Path;

use oma_archive::{ArchiveError, BlobId, CancelToken, ExportSnapshot, SniffedKind, SNIFF_WINDOW};

use crate::attach::{VolumeRule, COPY_CHUNK};
use crate::error::ExportError;
use crate::naming::{BundlePath, TargetName};
use crate::pipeline::ExportFailure;
use crate::stage::{Published, Replace, Staging};

/// What to copy, and where.
#[derive(Debug, Clone)]
pub struct CopyRequest<'a> {
    pub blob: BlobId,
    /// The stored length the dialog saw.
    pub expected_bytes: u64,
    /// The kind the dialog's name was checked against.
    pub sniffed: Option<SniffedKind>,
    /// The canonical destination folder (checked by the caller, §8.2).
    pub parent: &'a Path,
    pub target: &'a TargetName,
    pub replace: Replace,
    /// The per-file size limit of the destination's volume.
    pub volume: VolumeRule,
}

/// Where a copy is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyStep {
    /// Bytes are being copied (`done` of `total`).
    Copying,
    /// The copy is flushed and placed.
    Finishing,
    /// A cancel or a failure: the partial file is being removed.
    RemovingPartial,
}

/// Progress of a copy, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyProgress {
    pub step: CopyStep,
    pub done: u64,
    pub total: u64,
}

/// What a copy did.
#[derive(Debug, Clone)]
pub struct CopyReport {
    pub published: Published,
    pub bytes: u64,
    /// `Some(true)` written, `Some(false)` not written (Windows), `None`:
    /// not a platform that writes one.
    pub mark_of_the_web: Option<bool>,
}

/// Read until `buf` holds `want` bytes or the reader ends.
fn fill(r: &mut impl Read, buf: &mut Vec<u8>, want: usize) -> io::Result<()> {
    let mut chunk = vec![0u8; want];
    while buf.len() < want {
        let room = want - buf.len();
        let slice = chunk.get_mut(..room).unwrap_or(&mut []);
        match r.read(slice) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(slice.get(..n).unwrap_or(&[])),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Save a copy of `req.blob` (see the module comment). On any error the
/// partial file is removed first, and the failure says whether that
/// worked (`None`: nothing had been created).
pub fn save_copy(
    snap: &ExportSnapshot<'_>,
    req: &CopyRequest<'_>,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(CopyProgress),
) -> Result<CopyReport, ExportFailure> {
    let mut reader = match snap.open_blob(req.blob) {
        Ok(r) => r,
        Err(ArchiveError::Blob(_)) => return Err(ExportError::ChecksumMismatch.into()),
        Err(e) => return Err(e.into()),
    };
    let total = reader.len();
    if total != req.expected_bytes {
        return Err(ExportError::ConfirmationStale.into());
    }
    if !req.volume.fits(total) {
        return Err(ExportError::Write {
            kind: io::ErrorKind::FileTooLarge,
        }
        .into());
    }
    let mut head = Vec::with_capacity(SNIFF_WINDOW);
    if fill(&mut reader, &mut head, SNIFF_WINDOW).is_err() {
        return Err(ExportError::ChecksumMismatch.into());
    }
    if Some(oma_archive::media::sniff(&head, total)) != req.sniffed {
        return Err(ExportError::ConfirmationStale.into());
    }
    if cancel.is_cancelled() {
        return Err(ExportError::Cancelled.into());
    }
    let mut staging = Staging::single_file(req.parent)?;
    let mut p = CopyProgress {
        step: CopyStep::Copying,
        done: 0,
        total,
    };
    progress(p);
    match copy_into(&mut staging, &mut reader, &head, cancel, &mut p, progress) {
        Ok(()) => {}
        Err(error) => {
            p.step = CopyStep::RemovingPartial;
            progress(p);
            return Err(ExportFailure {
                error,
                staging_removed: Some(staging.remove()),
            });
        }
    }
    p.step = CopyStep::Finishing;
    progress(p);
    let published = match staging.publish(req.target, req.replace) {
        Ok(pb) => pb,
        Err(error) => {
            p.step = CopyStep::RemovingPartial;
            progress(p);
            return Err(ExportFailure {
                error,
                staging_removed: Some(staging.remove()),
            });
        }
    };
    let mark_of_the_web = match staging.mark_published_file() {
        Ok(true) => Some(true),
        Ok(false) => None,
        Err(_) => Some(false),
    };
    Ok(CopyReport {
        published,
        bytes: total,
        mark_of_the_web,
    })
}

/// Copy `head` and the rest of `reader` into a verified staged file.
fn copy_into(
    staging: &mut Staging,
    reader: &mut oma_archive::BlobReader,
    head: &[u8],
    cancel: &CancelToken,
    p: &mut CopyProgress,
    progress: &mut dyn FnMut(CopyProgress),
) -> Result<(), ExportError> {
    let mut out = staging.create_verified(&BundlePath::saved_copy())?;
    out.write_all(head).map_err(|e| ExportError::io(&e))?;
    p.done = head.len() as u64;
    let mut buf = vec![0u8; COPY_CHUNK];
    loop {
        if cancel.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            // `InvalidData` at the end is the digest or length check; any
            // other read error means the copy cannot be verified.
            Err(_) => return Err(ExportError::ChecksumMismatch),
        };
        out.write_all(buf.get(..n).unwrap_or(&[]))
            .map_err(|e| ExportError::io(&e))?;
        p.done = p.done.saturating_add(n as u64);
        progress(*p);
    }
    let digest = reader
        .verified_digest()
        .ok_or(ExportError::ChecksumMismatch)?;
    staging.close_verified(out, digest, p.total)?;
    staging.sync_batch()
}
