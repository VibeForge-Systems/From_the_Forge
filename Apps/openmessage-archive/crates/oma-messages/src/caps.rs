//! Per-column byte caps and the row budget (M3 §5.4).
//!
//! Content columns are selected as
//! `CASE WHEN octet_length("c") <= <cap> THEN "c" END, octet_length("c")`,
//! with the cap written into the SQL as an integer literal. A value over its
//! cap is never materialised: it becomes [`crate::Col::Oversized`] and is
//! counted. In a UTF-16 database `octet_length` counts UTF-16 bytes, so the
//! cap there is approximate.
//!
//! The `const` assertions at the bottom check that, for every known table,
//! the caps of all its columns sum to no more than [`ROW_BYTES_BUDGET`].

use crate::schema::KnownTable;

const KIB: u32 = 1024;
const MIB: u32 = 1024 * KIB;

/// `message.attributedBody`; equals the typedstream decoder's input cap.
pub const CAP_ATTRIBUTED_BODY: u32 = 16 * MIB;
/// `message.text`.
pub const CAP_TEXT: u32 = 16 * MIB;
/// `message.message_summary_info`, so an oversized summary is decided
/// without reading it.
pub const CAP_SUMMARY: u32 = MIB;
/// `chat.properties`.
pub const CAP_PROPERTIES: u32 = 256 * KIB;
/// `message.payload_data`, read only when raw blobs are kept (M4 R3). A
/// larger value is [`crate::Col::Oversized`] and is never read.
pub const CAP_PAYLOAD_DATA: u32 = MIB;
/// The raw value of an unreadable cell in a column without a byte cap (an
/// integer, boolean or time column holding text or a blob), kept in
/// [`crate::RawIssue`] only up to this size (M4 R2). Columns with a cap use
/// their own.
pub const CAP_RAW_UNREADABLE: u32 = 4 * KIB;
/// Every `guid` column, `associated_message_guid`, `thread_originator_guid`
/// and `reply_to_guid`.
pub const CAP_GUID: u32 = 256;
/// Addresses and other identifiers (`handle.id`, `uncanonicalized_id`,
/// `chat_identifier`, `person_centric_id`, `destination_caller_id`,
/// `group_id`), and `attachment.filename` and `transfer_name`.
pub const CAP_IDENTIFIER: u32 = 4 * KIB;
/// Other text (`subject`, `display_name`, `room_name`, `group_title`,
/// `service`, ...).
pub const CAP_OTHER_TEXT: u32 = 64 * KIB;

/// The bytes one row may materialise, across all its columns. SQLite may
/// hold a copy as well, so allow about twice this per row.
pub const ROW_BYTES_BUDGET: u64 = 40 * 1024 * 1024;

/// What an integer, boolean or time column counts toward the row budget.
pub const INTEGER_BYTES: u64 = 8;

const fn within_budget(t: KnownTable) -> bool {
    t.row_cap_bytes() <= ROW_BYTES_BUDGET
}

// Indexing in a `const` block: an out-of-range index is a compile error.
#[allow(clippy::indexing_slicing)]
const _: () = {
    let all = KnownTable::ALL;
    let mut i = 0;
    while i < all.len() {
        assert!(
            within_budget(all[i]),
            "a table's caps exceed ROW_BYTES_BUDGET"
        );
        i += 1;
    }
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The `const` assertion above runs at compile time; this test records
    /// the figures and that `message`, the widest table, still has headroom.
    #[test]
    fn every_table_fits_the_row_budget() {
        for t in KnownTable::ALL {
            assert!(t.row_cap_bytes() <= ROW_BYTES_BUDGET, "{t:?}");
        }
        let m = KnownTable::Message.row_cap_bytes();
        assert!(m > u64::from(CAP_TEXT + CAP_ATTRIBUTED_BODY));
        assert!(m < ROW_BYTES_BUDGET);
    }
}
