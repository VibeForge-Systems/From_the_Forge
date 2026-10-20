//! Attachment files (M6 spec §6.1, §6.3, §6.4; owner Q3, D134).
//!
//! [`Attacher`] decides and does the copy of every attachment file an
//! export writes. The record mapping calls it through
//! [`AttachmentFiles`] (`Mapper::item`), one [`AttachSession`] per item.
//!
//! - **Copy with verification (§6.1).** The bytes come only from
//!   `ExportSnapshot::open_blob`, whose `BlobReader` hashes what it yields
//!   and fails its last read on a length or digest mismatch. They go
//!   straight to the file's **final name inside staging**
//!   ([`Staging::create_verified`]; staging is the atomic unit, so there is
//!   no per-file rename), in 1 MiB chunks, checking the cancel token per
//!   chunk. There is no `verify_blob` followed by a second open.
//! - **One hash.** The digest the reader verified is the stored digest; the
//!   manifest and `SHA256SUMS` use it ([`Staging::close_verified`]). The
//!   copy is never hashed or read a second time.
//! - **The sniffed kind** comes from the first 4 KiB of the same stream
//!   (`oma_archive::sniff`), read before the file is created: the name's
//!   extension rule needs it. Never the declared type.
//! - **Names (§6.2)** are `naming::attachment_name`: the 6-digit file
//!   ordinal, the untrusted `transfer_name` without hidden characters,
//!   through `sanitize_filename_max` within the path budget, sharded by
//!   1,000.
//! - **Defusing (owner Q3b, §6.4).** An extension is kept only when it is
//!   on `naming::PASSIVE` for the sniffed kind; everything else gets
//!   `.blocked` appended (`blockedExtension`). The confirmed opt-out
//!   "Keep original file extensions" (off by default) turns the rule off.
//! - **Mark of the Web (owner Q3c).** On Windows each copy gets a
//!   `Zone.Identifier` stream with `ZoneId=3`; a failure (FAT, exFAT, a
//!   network share) is counted (`motwNotWritten`) and never fails the
//!   export. Nothing is written elsewhere. Its effect on SmartScreen and
//!   Office Protected View is **UNVERIFIED**.
//! - **Deduplication.** One file per `BlobId` per export; a later record
//!   of the same blob is `deduplicated` and refers to the same file (which
//!   carries the first record's name). A blob whose copy failed fails the
//!   same way for every record, without a second attempt.
//! - **Exclusions (§6.3)**, each recorded per attachment and counted:
//!   `checksumMismatch` (the store file is missing, unreadable, of another
//!   length or digest; the partial copy is removed), `tooLargeForVolume`
//!   (a blob of 4 GiB or more on FAT12/16/32, detected on Linux before any
//!   byte is read; on Windows FAT cannot be detected without `unsafe`, so
//!   a write refused at the 4 GiB boundary is classified so instead; exFAT
//!   is never limited) and `writeFailed` (any other write error; the copy
//!   is removed and the export carries on). A full volume is not an
//!   exclusion: it stops the export (`NoSpace`).
//!
//! - **Previews from the copy stream (§6.1, §6.5; slice 6h2).** With
//!   previews on ([`Attacher::keep_preview_bytes`]), the copy of an image
//!   (sniffed PNG, JPEG, GIF, WebP or HEIF) of at most 64 MiB also fills a
//!   buffer, kept only once the reader verified the whole file; the
//!   pipeline takes it for the preview feed right after the item is
//!   mapped ([`Attacher::take_preview_bytes`]) and drops what is left.
//!   Without a copy, [`read_preview_input`] reads the file once through
//!   the same verifying reader.
//!
//! Nothing here logs.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::path::Path;

use oma_archive::{
    ArchiveError, BlobId, BlobReader, CancelToken, ExportAttachment, ExportSnapshot, SniffedKind,
    SNIFF_WINDOW,
};

use crate::error::ExportError;
use crate::fault::{self, FaultPoint};
use crate::ids::Ordinal;
use crate::map::{AttachmentFiles, FileDecision, FileOutcome};
use crate::naming::{self, BundlePath, TargetName};
use crate::plan::{AttachmentMode, ExportPlan};
use crate::preview::{PreviewInput, MAX_PREVIEW_INPUT};
use crate::stage::{StagedFile, Staging};

/// The copy's chunk (spec §6.1): the cancel token is checked per chunk.
pub const COPY_CHUNK: usize = 1 << 20;

/// The size from which FAT12/16/32 cannot hold a file (its largest file is
/// 4 GiB − 1 byte).
pub const FAT_FILE_LIMIT: u64 = 4 << 30;

/// What the destination's file system limits (spec §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VolumeRule {
    /// No known per-file limit (exFAT included).
    None,
    /// Files larger than this many bytes are excluded as
    /// `tooLargeForVolume` before they are read: FAT12/16/32's
    /// `FAT_FILE_LIMIT − 1` (tests use smaller limits).
    MaxFileBytes(u64),
}

impl VolumeRule {
    /// FAT12/16/32.
    pub const FAT: VolumeRule = VolumeRule::MaxFileBytes(FAT_FILE_LIMIT - 1);

    /// The rule for a volume kind: only `Fat(Msdos)`; exFAT has no 4 GiB
    /// limit.
    pub fn for_kind(kind: oma_fs::VolumeKind) -> VolumeRule {
        match kind {
            oma_fs::VolumeKind::Fat(oma_fs::FatKind::Msdos) => VolumeRule::FAT,
            _ => VolumeRule::None,
        }
    }

    /// Whether a file of `len` bytes fits.
    pub fn fits(self, len: u64) -> bool {
        match self {
            VolumeRule::None => true,
            VolumeRule::MaxFileBytes(max) => len <= max,
        }
    }

    /// The rule for the volume `parent` is on (Linux detects FAT; elsewhere
    /// and on any error, no rule).
    pub fn of(parent: &Path) -> VolumeRule {
        oma_fs::volume_kind(parent).map_or(VolumeRule::None, VolumeRule::for_kind)
    }
}

/// How attachments are copied in one export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AttachPolicy {
    /// "Keep original file extensions" (owner Q3b; off, confirmed in
    /// Review).
    pub keep_extensions: bool,
    /// The bytes an attachment's `<name>` may take
    /// (`naming::attachment_name_budget`).
    pub name_budget: usize,
    pub volume: VolumeRule,
    /// Write a Mark of the Web on each copy (owner Q3c). It is written
    /// only on Windows; tests turn it off to see the count.
    pub mark_of_the_web: bool,
}

impl AttachPolicy {
    /// The policy of `plan` for a bundle `bundle` in the canonical
    /// `parent`. `None` when the plan copies no files (names only).
    pub fn for_plan(plan: &ExportPlan, parent: &Path, bundle: &TargetName) -> Option<AttachPolicy> {
        let o = plan.options();
        (o.attachments == AttachmentMode::CopyOriginals).then(|| AttachPolicy {
            keep_extensions: o.keep_extensions,
            name_budget: naming::attachment_name_budget(parent, bundle),
            volume: VolumeRule::of(parent),
            mark_of_the_web: true,
        })
    }
}

/// What the copies did, for the manifest (spec §7.1); content-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct AttachCounts {
    /// Distinct files written.
    pub files_written: u64,
    pub bytes_written: u64,
    /// Files written with `.blocked` appended.
    pub blocked: u64,
    /// Files whose name is the ordinal only (a deep parent).
    pub names_shortened: u64,
    pub motw_written: u64,
    /// Marks of the Web that could not be written (Windows only).
    pub motw_not_written: u64,
    /// Distinct blobs excluded, by reason.
    pub checksum_mismatch: u64,
    pub too_large_for_volume: u64,
    pub write_failed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Slot {
    Written {
        file: Ordinal,
        path: String,
        sniffed: Option<SniffedKind>,
    },
    Failed {
        decision: FileDecision,
        sniffed: Option<SniffedKind>,
    },
}

/// Every attachment file of one export.
pub struct Attacher {
    policy: AttachPolicy,
    slots: BTreeMap<BlobId, Slot>,
    next_file: Ordinal,
    counts: AttachCounts,
    /// Previews on: the verified bytes of images copied for the current
    /// item, by blob.
    tee: Option<BTreeMap<BlobId, Vec<u8>>>,
}

impl std::fmt::Debug for Attacher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attacher")
            .field("policy", &self.policy)
            .field("blobs", &self.slots.len())
            .field("counts", &self.counts)
            .finish()
    }
}

/// One item's view of the [`Attacher`]: what the mapper calls.
pub struct AttachSession<'a, 's> {
    attacher: &'a mut Attacher,
    snap: &'a ExportSnapshot<'s>,
    staging: &'a mut Staging,
    cancel: &'a CancelToken,
}

impl std::fmt::Debug for AttachSession<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttachSession").finish_non_exhaustive()
    }
}

/// How one copy ended.
enum Copied {
    Done(StagedFile, [u8; 32]),
    /// The source failed its checksum or could not be read.
    Mismatch(StagedFile),
    /// The destination refused the write.
    WriteError(StagedFile, io::Error),
}

impl Attacher {
    pub fn new(policy: AttachPolicy) -> Attacher {
        Attacher {
            policy,
            slots: BTreeMap::new(),
            next_file: 1,
            counts: AttachCounts::default(),
            tee: None,
        }
    }

    /// Keep the copy stream of images for previews (spec §6.1).
    pub fn keep_preview_bytes(&mut self) {
        self.tee = Some(BTreeMap::new());
    }

    /// The verified bytes of `blob`, if its copy kept them (once).
    pub fn take_preview_bytes(&mut self, blob: BlobId) -> Option<Vec<u8>> {
        self.tee.as_mut()?.remove(&blob)
    }

    /// Drop every kept buffer (after each item).
    pub fn clear_preview_bytes(&mut self) {
        if let Some(t) = self.tee.as_mut() {
            t.clear();
        }
    }

    pub fn policy(&self) -> AttachPolicy {
        self.policy
    }

    pub fn counts(&self) -> AttachCounts {
        self.counts
    }

    /// The mapper's view for one item.
    pub fn session<'a, 's>(
        &'a mut self,
        snap: &'a ExportSnapshot<'s>,
        staging: &'a mut Staging,
        cancel: &'a CancelToken,
    ) -> AttachSession<'a, 's> {
        AttachSession {
            attacher: self,
            snap,
            staging,
            cancel,
        }
    }
}

/// `true` for an error at the FAT 4 GiB boundary (spec §6.3): `EFBIG`
/// (`FileTooLarge`) anywhere; on Windows also `ERROR_INVALID_PARAMETER`,
/// or a space error once 4 GiB − 1 bytes were written.
fn at_volume_limit(e: &io::Error, written: u64) -> bool {
    if e.kind() == io::ErrorKind::FileTooLarge {
        return true;
    }
    #[cfg(windows)]
    {
        if written >= FAT_FILE_LIMIT - 1 {
            return e.raw_os_error() == Some(87)
                || matches!(ExportError::io(e), ExportError::NoSpace);
        }
    }
    let _ = written;
    false
}

/// The kinds the export previews (the decode helper decides the rest).
pub fn is_previewable(kind: SniffedKind) -> bool {
    matches!(
        kind,
        SniffedKind::Png
            | SniffedKind::Jpeg
            | SniffedKind::Gif
            | SniffedKind::Webp
            | SniffedKind::Heif
    )
}

/// One verified read of `blob` for its preview when the export copies no
/// file (spec §6.1, §6.5): the first 4 KiB are sniffed (a file that is not
/// an image is not read further); an image over [`MAX_PREVIEW_INPUT`] is
/// `Unavailable` unread; otherwise the whole file is read through the
/// verifying reader, checking `cancel` per 1 MiB, and a read that fails
/// its length or digest check is `Unavailable`. A store file that cannot
/// be opened gives no preview card (its record says why).
pub fn read_preview_input(
    snap: &ExportSnapshot<'_>,
    blob: BlobId,
    cancel: &CancelToken,
) -> Result<PreviewInput, ExportError> {
    let mut reader = match snap.open_blob(blob) {
        Ok(r) => r,
        Err(ArchiveError::Blob(_)) => return Ok(PreviewInput::NotAnImage),
        Err(e) => return Err(e.into()),
    };
    let mut buf = Vec::with_capacity(SNIFF_WINDOW);
    if fill(&mut reader, &mut buf, SNIFF_WINDOW).is_err() {
        return Ok(PreviewInput::NotAnImage);
    }
    if !is_previewable(oma_archive::media::sniff(&buf, reader.len())) {
        return Ok(PreviewInput::NotAnImage);
    }
    if reader.len() > MAX_PREVIEW_INPUT {
        return Ok(PreviewInput::Unavailable);
    }
    let total = usize::try_from(reader.len()).unwrap_or(usize::MAX);
    buf.reserve(total.saturating_sub(buf.len()));
    let mut chunk = vec![0u8; COPY_CHUNK];
    loop {
        if cancel.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(chunk.get(..n).unwrap_or(&[])),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Ok(PreviewInput::Unavailable),
        }
    }
    Ok(match reader.verified_digest() {
        Some(_) => PreviewInput::Bytes(buf),
        None => PreviewInput::Unavailable,
    })
}

/// Read until `buf` holds `want` bytes or the reader ends.
fn fill(r: &mut BlobReader, buf: &mut Vec<u8>, want: usize) -> io::Result<()> {
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

impl AttachSession<'_, '_> {
    fn transfer_name(a: &ExportAttachment) -> Option<&str> {
        a.facts.name.text().map(|t| t.text.expose().as_str())
    }

    fn copy(
        &mut self,
        a: &ExportAttachment,
        blob: BlobId,
    ) -> Result<(Slot, FileDecision), ExportError> {
        let mismatch = |sniffed| {
            (
                Slot::Failed {
                    decision: FileDecision::ChecksumMismatch,
                    sniffed,
                },
                FileDecision::ChecksumMismatch,
            )
        };
        let mut reader = match self.snap.open_blob(blob) {
            Ok(r) => r,
            // The store file is missing, of another length, not a regular
            // file or unreadable: it does not match its record.
            Err(ArchiveError::Blob(_)) => {
                self.attacher.counts.checksum_mismatch += 1;
                return Ok(mismatch(None));
            }
            Err(e) => return Err(e.into()),
        };
        let p = self.attacher.policy;
        if !p.volume.fits(reader.len()) {
            self.attacher.counts.too_large_for_volume += 1;
            let d = FileDecision::TooLargeForVolume;
            return Ok((
                Slot::Failed {
                    decision: d,
                    sniffed: None,
                },
                d,
            ));
        }
        if self.cancel.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        let mut head = Vec::with_capacity(SNIFF_WINDOW);
        if fill(&mut reader, &mut head, SNIFF_WINDOW).is_err() {
            self.attacher.counts.checksum_mismatch += 1;
            return Ok(mismatch(None));
        }
        let sniffed = oma_archive::media::sniff(&head, reader.len());
        let file = self.attacher.next_file;
        let name = naming::attachment_name(
            file,
            Self::transfer_name(a),
            Some(sniffed),
            p.name_budget,
            p.keep_extensions,
        );
        let path = BundlePath::attachment(&name);
        let out = match self.staging.create_verified(&path) {
            Ok(f) => f,
            Err(ExportError::Write { .. }) => {
                self.attacher.counts.write_failed += 1;
                let d = FileDecision::WriteFailed;
                return Ok((
                    Slot::Failed {
                        decision: d,
                        sniffed: Some(sniffed),
                    },
                    d,
                ));
            }
            Err(e) => return Err(e),
        };
        let expected = reader.len();
        let mut tee = (self.attacher.tee.is_some()
            && is_previewable(sniffed)
            && expected <= MAX_PREVIEW_INPUT)
            .then(|| Vec::with_capacity(usize::try_from(expected).unwrap_or(0)));
        match self.stream(&mut reader, out, &head, tee.as_mut())? {
            Copied::Done(f, sha) => {
                if let (Some(t), Some(bytes)) = (self.attacher.tee.as_mut(), tee) {
                    t.insert(blob, bytes);
                }
                let entry = self.staging.close_verified(f, sha, expected)?;
                let n = entry.bytes();
                self.attacher.next_file += 1;
                let c = &mut self.attacher.counts;
                c.files_written += 1;
                c.bytes_written = c.bytes_written.saturating_add(n);
                c.names_shortened += u64::from(name.shortened);
                c.blocked += u64::from(name.blocked);
                if p.mark_of_the_web {
                    match self.staging.write_mark_of_the_web(&entry) {
                        Ok(true) => self.attacher.counts.motw_written += 1,
                        Ok(false) => {}
                        Err(ExportError::Write { .. } | ExportError::NoSpace) => {
                            self.attacher.counts.motw_not_written += 1;
                        }
                        Err(e) => return Err(e),
                    }
                }
                let decision = if name.blocked {
                    FileDecision::BlockedExtension
                } else {
                    FileDecision::Copied
                };
                Ok((
                    Slot::Written {
                        file,
                        path: path.to_slash(),
                        sniffed: Some(sniffed),
                    },
                    decision,
                ))
            }
            Copied::Mismatch(f) => {
                self.discard(f)?;
                self.attacher.counts.checksum_mismatch += 1;
                Ok(mismatch(Some(sniffed)))
            }
            Copied::WriteError(f, e) => {
                let written = f.bytes_written();
                self.discard(f)?;
                let decision = if at_volume_limit(&e, written) {
                    self.attacher.counts.too_large_for_volume += 1;
                    FileDecision::TooLargeForVolume
                } else if matches!(ExportError::io(&e), ExportError::NoSpace) {
                    return Err(ExportError::NoSpace);
                } else {
                    self.attacher.counts.write_failed += 1;
                    FileDecision::WriteFailed
                };
                Ok((
                    Slot::Failed {
                        decision,
                        sniffed: Some(sniffed),
                    },
                    decision,
                ))
            }
        }
    }

    /// Remove a failed copy; a copy that cannot be removed fails the
    /// export (a published bundle must not hold it).
    fn discard(&mut self, f: StagedFile) -> Result<(), ExportError> {
        if self.staging.discard(f)? {
            Ok(())
        } else {
            Err(ExportError::Write {
                kind: io::ErrorKind::Other,
            })
        }
    }

    /// Write `head`, then the rest of `reader`, into `out`, in chunks.
    fn stream(
        &mut self,
        reader: &mut BlobReader,
        mut out: StagedFile,
        head: &[u8],
        mut tee: Option<&mut Vec<u8>>,
    ) -> Result<Copied, ExportError> {
        if let Err(e) = out.write_all(head) {
            return Ok(Copied::WriteError(out, e));
        }
        if let Some(t) = tee.as_mut() {
            t.extend_from_slice(head);
        }
        fault::point(FaultPoint::MidAttachmentCopy);
        let mut buf = vec![0u8; COPY_CHUNK];
        loop {
            if self.cancel.is_cancelled() {
                drop(out);
                return Err(ExportError::Cancelled);
            }
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                // `InvalidData` at the end is the digest or length check;
                // any other read error means the copy cannot be verified.
                Err(_) => return Ok(Copied::Mismatch(out)),
            };
            let got = buf.get(..n).unwrap_or(&[]);
            if let Err(e) = out.write_all(got) {
                return Ok(Copied::WriteError(out, e));
            }
            if let Some(t) = tee.as_mut() {
                t.extend_from_slice(got);
            }
        }
        if let Err(e) = out.flush() {
            return Ok(Copied::WriteError(out, e));
        }
        match reader.verified_digest() {
            Some(sha) => Ok(Copied::Done(out, sha)),
            None => Ok(Copied::Mismatch(out)),
        }
    }
}

impl AttachmentFiles for AttachSession<'_, '_> {
    fn decide(
        &mut self,
        attachment: &ExportAttachment,
        blob: BlobId,
    ) -> Result<FileOutcome, ExportError> {
        let (slot, decision) = match self.attacher.slots.get(&blob).cloned() {
            Some(s @ Slot::Written { .. }) => (s, FileDecision::Deduplicated),
            Some(s @ Slot::Failed { decision, .. }) => (s, decision),
            None => {
                let (slot, d) = self.copy(attachment, blob)?;
                self.attacher.slots.insert(blob, slot.clone());
                (slot, d)
            }
        };
        Ok(match slot {
            Slot::Written {
                file,
                path,
                sniffed,
            } => FileOutcome {
                decision,
                file: Some(file),
                path: Some(path),
                sniffed,
            },
            Slot::Failed { sniffed, .. } => FileOutcome {
                sniffed,
                ..FileOutcome::without_file(decision)
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oma_fs::{FatKind, VolumeKind};

    #[test]
    fn only_fat12_16_32_is_limited_to_4_gib() {
        assert_eq!(
            VolumeRule::for_kind(VolumeKind::Fat(FatKind::Msdos)),
            VolumeRule::FAT
        );
        // exFAT has no 4 GiB limit (oma-fs volume.rs).
        for k in [
            VolumeKind::Fat(FatKind::ExFat),
            VolumeKind::Local,
            VolumeKind::Unknown,
        ] {
            assert_eq!(VolumeRule::for_kind(k), VolumeRule::None, "{k:?}");
        }
        assert!(VolumeRule::FAT.fits(FAT_FILE_LIMIT - 1));
        assert!(!VolumeRule::FAT.fits(FAT_FILE_LIMIT));
        assert!(VolumeRule::None.fits(u64::MAX));
    }

    #[test]
    fn a_write_refused_at_the_boundary_is_too_large_for_the_volume() {
        let big = io::Error::from(io::ErrorKind::FileTooLarge);
        assert!(at_volume_limit(&big, 0));
        let other = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(!at_volume_limit(&other, FAT_FILE_LIMIT));
        #[cfg(windows)]
        {
            let invalid = io::Error::from_raw_os_error(87);
            assert!(at_volume_limit(&invalid, FAT_FILE_LIMIT - 1));
            assert!(!at_volume_limit(&invalid, 10));
        }
    }
}
