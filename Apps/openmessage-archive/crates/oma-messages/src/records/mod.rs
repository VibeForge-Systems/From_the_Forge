//! Typed records, one per source row (M3 §9).
//!
//! Every source field is a [`crate::Col`]. Every record is
//! `#[non_exhaustive]`, so fields added later do not break callers, and every
//! `Debug` impl is hand-written to print the rowid and each field's shape
//! (absent, NULL, value, unreadable or oversized), never a value.
//!
//! Every record also has `raw_issues` (M4 R2): the raw value of each of its
//! unreadable cells, filled only when raw values are kept
//! ([`crate::ProbeOptions::with_keep_raw_values`], or
//! [`MessageReadOptions::keep_raw_blobs`] for messages), and empty
//! otherwise.

mod attachment;
mod chat;
mod handle;
mod joins;
mod keys;
mod message;
mod recoverable;

pub use attachment::AttachmentRecord;
pub use chat::ChatRecord;
pub use handle::HandleRecord;
pub use joins::{ChatHandleLink, ChatMessageLink, MessageAttachmentLink};
pub use keys::{HandleKeyRow, KeyRow};
pub use message::{
    AttributedOutcome, Body, EditLabel, MessageReadOptions, MessageRecord, ReplyInfo,
};
pub use recoverable::RecoverableLink;

/// `Debug` for a record: the rowid, then each listed field's
/// [`crate::ColShape`], then each listed derived kind as is.
macro_rules! shape_debug {
    ($ty:ident { $($field:ident),* $(,)? } $(kinds { $($kind:ident),* $(,)? })?) => {
        impl std::fmt::Debug for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($ty))
                    .field("rowid", &self.rowid)
                    $(.field(stringify!($field), &self.$field.shape()))*
                    $($(.field(stringify!($kind), &self.$kind))*)?
                    .finish_non_exhaustive()
            }
        }
    };
}
pub(crate) use shape_debug;
