//! `attachment`: one attachment row, and where its file is (M3 §9, §12).
//!
//! `filename` is an untrusted lookup key, never a path. `transfer_state`
//! stays raw; its meanings are **UNVERIFIED**. `total_bytes` is the size
//! the database declares, not a measured one.

use crate::attachments::{AttachmentLocation, AttachmentLocator};
use crate::col::Col;
use crate::db::RowReader;
use crate::error::MessagesError;
use crate::raw::RawIssue;
use crate::redact::{Redacted, SqlText};
use crate::schema::known::attachment as c;
use crate::time::CocoaTime;

use super::shape_debug;

/// One `attachment` row.
#[derive(Clone)]
#[non_exhaustive]
pub struct AttachmentRecord {
    /// `ROWID`.
    pub rowid: i64,
    /// `guid`.
    pub guid: Col<Redacted<SqlText>>,
    /// `filename`: an untrusted lookup key, never a path.
    pub filename: Col<Redacted<SqlText>>,
    /// `uti`.
    pub uti: Col<Redacted<SqlText>>,
    /// `mime_type`.
    pub mime_type: Col<Redacted<SqlText>>,
    /// `transfer_name`.
    pub transfer_name: Col<Redacted<SqlText>>,
    /// `total_bytes`, as declared.
    pub total_bytes: Col<i64>,
    /// `is_sticker`.
    pub is_sticker: Col<bool>,
    /// `hide_attachment`.
    pub hide_attachment: Col<bool>,
    /// `is_outgoing`.
    pub is_outgoing: Col<bool>,
    /// `transfer_state`, raw; its meanings are **UNVERIFIED**.
    pub transfer_state: Col<i64>,
    /// `created_date`; classified per value (S3 stores it in seconds next
    /// to nanosecond message dates).
    pub created_date: Col<CocoaTime>,
    /// `start_date`.
    pub start_date: Col<CocoaTime>,
    /// `emoji_image_short_description` (a Genmoji prompt).
    pub emoji_image_short_description: Col<Redacted<SqlText>>,
    /// `original_guid`.
    pub original_guid: Col<Redacted<SqlText>>,
    /// Where the file is; `None` when the pass was given no locator.
    pub location: Option<AttachmentLocation>,
    /// The raw values of unreadable cells, when kept (M4 R2).
    pub raw_issues: Vec<RawIssue>,
}

shape_debug!(AttachmentRecord {
    guid, filename, uti, mime_type, transfer_name, total_bytes, is_sticker,
    hide_attachment, is_outgoing, transfer_state, created_date, start_date,
    emoji_image_short_description, original_guid,
} kinds { location, raw_issues });

impl AttachmentRecord {
    pub(crate) fn from_row(
        rowid: i64,
        r: &mut RowReader<'_, '_>,
        locator: Option<&mut AttachmentLocator<'_>>,
    ) -> Result<Self, MessagesError> {
        let filename = r.text(c::FILENAME)?;
        let location = match locator {
            Some(l) => Some(l.locate(&filename)?),
            None => None,
        };
        Ok(AttachmentRecord {
            rowid,
            guid: r.text(c::GUID)?,
            filename,
            uti: r.text(c::UTI)?,
            mime_type: r.text(c::MIME_TYPE)?,
            transfer_name: r.text(c::TRANSFER_NAME)?,
            total_bytes: r.integer(c::TOTAL_BYTES)?,
            is_sticker: r.boolean(c::IS_STICKER)?,
            hide_attachment: r.boolean(c::HIDE_ATTACHMENT)?,
            is_outgoing: r.boolean(c::IS_OUTGOING)?,
            transfer_state: r.integer(c::TRANSFER_STATE)?,
            created_date: r.time(c::CREATED_DATE)?,
            start_date: r.time(c::START_DATE)?,
            emoji_image_short_description: r.text(c::EMOJI_IMAGE_SHORT_DESCRIPTION)?,
            original_guid: r.text(c::ORIGINAL_GUID)?,
            location,
            raw_issues: r.take_raw_issues(),
        })
    }
}
