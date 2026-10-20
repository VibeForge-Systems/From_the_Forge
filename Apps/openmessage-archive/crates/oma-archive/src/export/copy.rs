//! "Save a copy" of one attachment (M6 spec §6.6, slice 6k2): the reads.
//!
//! [`ExportSnapshot::attachment_copy`] says what a copy of one attachment
//! would be: its declared name (content), its stored file and whether it
//! is an **attachment of an unsent part** (§3.8, owner Q4). The rule is
//! the export's ([`super::mask::attachment_access`]) over every message
//! that links the attachment, and errs towards withholding: one linking
//! message whose current body lists an unsent part and names the
//! attachment in no kept part is enough. Such a copy needs the
//! earlier-content confirmation, exactly as a bundle would.
//!
//! [`ArchiveReader::attachment_copy_preview`] runs the same read in its
//! own read transaction (no lock: the job repeats it under the lock and
//! compares) and sniffs the first 4 KiB of the stored file through the
//! verifying reader, for the save dialog's name and its extension rule.
//! The job sniffs again from its own copy stream; the bytes are verified
//! only there, by the reader's final digest check.
//!
//! Nothing here logs.

use std::io::Read;

use super::mask::{attachment_access, AttachmentAccess};
use super::ExportSnapshot;
use crate::error::ArchiveError;
use crate::media::{sniff, SniffedKind, SNIFF_WINDOW};
use crate::progress::CancelToken;
use crate::reader::{ArchiveReader, BlobId};
use crate::stmt::StmtCache;
use crate::view::assemble::{Caps, Ctx, Reveal};
use crate::view::uncapped::{checked, export_item};
use crate::view::{AttachmentId, OptContent};

use super::preview::ArchiveGeneration;

/// The messages that link an attachment (through
/// `message_attachment_by_attachment`).
pub(crate) const SQL_ATTACHMENT_MESSAGES: &str =
    "SELECT message_id FROM message_attachment WHERE attachment_id = ?1 ORDER BY message_id";

/// What a copy of one attachment would be.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AttachmentCopy {
    /// `transfer_name` as declared (content: never log it).
    pub name: OptContent,
    /// The stored file and its stored length; `None` without one.
    pub blob: Option<(BlobId, u64)>,
    /// An attachment of an unsent part of a message that links it
    /// (§3.8): copied only with the earlier-content confirmation.
    pub of_unsent_part: bool,
}

impl std::fmt::Debug for AttachmentCopy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the name, an id or a size.
        f.debug_struct("AttachmentCopy")
            .field("has_file", &self.blob.is_some())
            .field("of_unsent_part", &self.of_unsent_part)
            .finish_non_exhaustive()
    }
}

/// A copy's facts before the dialog: the copy, the kind its stored file
/// sniffs as (`None` without a file or when its head cannot be read) and
/// the archive generation they were read at.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CopyPreview {
    pub copy: AttachmentCopy,
    pub sniffed: Option<SniffedKind>,
    pub generation: ArchiveGeneration,
}

impl ExportSnapshot<'_> {
    /// Whether any message linking attachment `id` withholds it as an
    /// attachment of an unsent part (the export's rule, under no reveal).
    fn of_unsent_part(&self, id: AttachmentId) -> Result<bool, ArchiveError> {
        let messages: Vec<i64> = self.run(|| {
            let mut st = self.stmts.prepare(SQL_ATTACHMENT_MESSAGES)?;
            let rows = st.query_map([id.get()], |r| r.get(0))?;
            rows.collect()
        })?;
        for m in messages {
            self.checkpoint()?;
            let Some(item) = self.run(|| {
                export_item(
                    &self.stmts,
                    &self.ctx,
                    None,
                    m,
                    &Caps::EXPORT,
                    Reveal::default(),
                )
            })?
            else {
                continue;
            };
            let item = checked(item)?;
            let access =
                self.run(|| attachment_access(&self.stmts, &item.message, Reveal::default()))?;
            let withheld = item
                .message
                .attachments
                .iter()
                .zip(&access)
                .any(|(a, x)| a.id == id && *x != AttachmentAccess::Included);
            if withheld {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// What a copy of attachment `id` would be (see the module comment).
    /// An attachment that does not exist is `AnchorNotFound`.
    pub fn attachment_copy(&self, id: AttachmentId) -> Result<AttachmentCopy, ArchiveError> {
        self.checkpoint()?;
        let x = self
            .run(|| super::rows::attachment(&self.stmts, id, AttachmentAccess::Included))?
            .ok_or(ArchiveError::AnchorNotFound)?;
        let unsent = self.of_unsent_part(id)?;
        Ok(AttachmentCopy {
            name: x.facts.name,
            blob: x
                .facts
                .blob
                .map(|b| (b.id, u64::try_from(b.stored_bytes).unwrap_or(0))),
            of_unsent_part: unsent,
        })
    }

    /// The kind blob `id`'s first 4 KiB sniff as, read through the
    /// verifying reader (`None` when the file cannot be opened or read).
    pub fn sniff_blob(&self, id: BlobId) -> Result<Option<SniffedKind>, ArchiveError> {
        let mut r = match self.open_blob(id) {
            Ok(r) => r,
            Err(ArchiveError::Blob(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        let len = r.len();
        let mut head = vec![0u8; SNIFF_WINDOW];
        let mut got = 0usize;
        while got < head.len() {
            let Some(rest) = head.get_mut(got..) else {
                break;
            };
            match r.read(rest) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Ok(None),
            }
        }
        head.truncate(got);
        Ok(Some(sniff(&head, len)))
    }
}

impl ArchiveReader {
    /// "Save a copy"'s facts of attachment `id` (M6 spec §6.6), in ONE
    /// read transaction as [`ArchiveReader::export_preview`] runs, stopped
    /// only by `cancel`. Needs no hold: the job reads the same facts again
    /// under the lock and compares.
    pub fn attachment_copy_preview(
        &self,
        id: AttachmentId,
        cancel: &CancelToken,
    ) -> Result<CopyPreview, ArchiveError> {
        self.require_current()?;
        self.conn().exporting(cancel, |phase| {
            let ctx = phase.sql(Ctx::load)?;
            let snap = ExportSnapshot {
                reader: self,
                phase,
                stmts: StmtCache::new(phase.connection()),
                ctx,
                without_events: std::cell::Cell::new(false),
            };
            let copy = snap.attachment_copy(id)?;
            let sniffed = match copy.blob {
                Some((b, _)) => snap.sniff_blob(b)?,
                None => None,
            };
            Ok(CopyPreview {
                copy,
                sniffed,
                generation: snap.generation()?,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_link_lookup_reads_by_attachment() {
        assert!(super::SQL_ATTACHMENT_MESSAGES.contains("WHERE attachment_id = ?1"));
    }
}
