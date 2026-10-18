//! `chat_recoverable_message_join`: Recently Deleted links. What M4 imports
//! from them is M4's policy (D33). The deleted text itself
//! (`recoverable_message_part.part_text`) is never read; the authorizer
//! denies it.

use crate::col::Col;
use crate::db::{FromRow, RowReader};
use crate::error::{MessagesError, Stage};
use crate::progress::MessagesPhase;
use crate::schema::known::chat_recoverable_message_join as c;
use crate::schema::KnownTable;
use crate::time::CocoaTime;

use super::shape_debug;

/// One `chat_recoverable_message_join` row.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecoverableLink {
    /// The implicit `rowid`.
    pub rowid: i64,
    /// `chat_id`.
    pub chat_id: Col<i64>,
    /// `message_id`.
    pub message_id: Col<i64>,
    /// `delete_date`; its unit is **UNVERIFIED**, so it is classified per
    /// value.
    pub delete_date: Col<CocoaTime>,
    /// `ck_sync_state`, raw.
    pub ck_sync_state: Col<i64>,
}

shape_debug!(RecoverableLink {
    chat_id,
    message_id,
    delete_date,
    ck_sync_state
});

impl FromRow for RecoverableLink {
    const TABLE: KnownTable = KnownTable::ChatRecoverableMessageJoin;
    const PHASE: MessagesPhase = MessagesPhase::ReadingChatLinks;
    const STAGE: Stage = Stage::Recoverable;

    fn from_row(rowid: i64, r: &mut RowReader<'_, '_>) -> Result<Self, MessagesError> {
        Ok(RecoverableLink {
            rowid,
            chat_id: r.integer(c::CHAT_ID)?,
            message_id: r.integer(c::MESSAGE_ID)?,
            delete_date: r.time(c::DELETE_DATE)?,
            ck_sync_state: r.integer(c::CK_SYNC_STATE)?,
        })
    }
}
