//! The byte pre-measure and byte-sized batches (M6 spec §3.3).
//!
//! Before a batch is assembled, each candidate message is measured in SQL
//! from the stored lengths alone (`octet_length`, which SQLite answers from
//! the record header without loading the value): the current body's
//! displayed text and its `attributedBody`, plus, with history, every
//! version's displayed text and `message_summary_info` (the edit texts are
//! decoded from it). A batch then holds at most [`BatchLimits::max_ids`]
//! messages **and** at most [`BatchLimits::max_bytes`] measured; a single
//! message over the byte budget is a batch of its own, and its history is
//! streamed one version at a time. The import caps each stored cell at
//! 16 MiB, so the measure is an upper bound on what assembling a message
//! loads, never an estimate of what it writes.
//!
//! One short statement per candidate (prepared once per snapshot) rather
//! than one statement over a list of ids: the archive has no table-valued
//! id list without a temporary table or an extension.

use crate::stmt::StmtCache;

/// How a cursor cuts its batches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct BatchLimits {
    /// The most messages in a batch.
    pub max_ids: usize,
    /// The most bytes measured in a batch (a single larger message is a
    /// batch of its own).
    pub max_bytes: u64,
}

impl BatchLimits {
    /// 256 messages and 32 MiB (M6 spec §3.3).
    pub const DEFAULT: BatchLimits = BatchLimits {
        max_ids: 256,
        max_bytes: 32 << 20,
    };

    /// Other limits (tests, or a caller with a smaller memory budget). A
    /// batch always takes at least one message.
    pub fn new(max_ids: usize, max_bytes: u64) -> BatchLimits {
        BatchLimits {
            max_ids: max_ids.max(1),
            max_bytes,
        }
    }
}

impl Default for BatchLimits {
    fn default() -> BatchLimits {
        BatchLimits::DEFAULT
    }
}

/// How deep an export assembles a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ExportDepth {
    /// The timeline item: its current body, attachments, reactions and
    /// the detail columns.
    #[default]
    Items,
    /// Also every captured body version with its phone edit entries.
    WithHistory,
}

pub(crate) const SQL_MEASURE_CURRENT: &str = "SELECT \
       coalesce(octet_length(b.display_text), 0) + coalesce(octet_length(b.attributed_raw), 0) \
     FROM message m JOIN message_body b ON b.id = m.current_body_id WHERE m.id = ?1";
pub(crate) const SQL_MEASURE_VERSIONS: &str = "SELECT coalesce(sum( \
       coalesce(octet_length(display_text), 0) + coalesce(octet_length(summary_raw), 0)), 0) \
     FROM message_body WHERE message_id = ?1";

#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    vec![
        (
            "export measure current body",
            SQL_MEASURE_CURRENT.to_owned(),
        ),
        ("export measure versions", SQL_MEASURE_VERSIONS.to_owned()),
    ]
}

fn u64_of(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

/// The measured bytes of message `id` at `depth` (0 for a message without
/// a current body, or that does not exist).
pub(crate) fn measure_one(s: &StmtCache<'_>, id: i64, depth: ExportDepth) -> rusqlite::Result<u64> {
    use rusqlite::OptionalExtension;
    let current: i64 = s
        .prepare(SQL_MEASURE_CURRENT)?
        .query_row([id], |r| r.get(0))
        .optional()?
        .unwrap_or(0);
    let versions: i64 = match depth {
        ExportDepth::Items => 0,
        ExportDepth::WithHistory => s
            .prepare(SQL_MEASURE_VERSIONS)?
            .query_row([id], |r| r.get(0))?,
    };
    Ok(u64_of(current).saturating_add(u64_of(versions)))
}

/// How many of `sizes` (in order) the next batch takes: at most
/// `limits.max_ids`, and no more than `limits.max_bytes` in total unless
/// the first one alone is larger. Zero only for an empty list.
pub(crate) fn cut(sizes: &[u64], limits: BatchLimits) -> usize {
    let mut total = 0u64;
    let mut n = 0usize;
    for &size in sizes.iter().take(limits.max_ids.max(1)) {
        let next = total.saturating_add(size);
        if n > 0 && next > limits.max_bytes {
            break;
        }
        total = next;
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_cut_by_count_and_bytes_and_a_big_message_goes_alone() {
        let l = BatchLimits::new(3, 100);
        assert_eq!(cut(&[], l), 0);
        assert_eq!(cut(&[10, 10, 10, 10], l), 3);
        assert_eq!(cut(&[60, 30, 20], l), 2);
        assert_eq!(cut(&[60, 40, 1], l), 2);
        // Over the budget alone: a batch of one, then the rest.
        assert_eq!(cut(&[500, 1], l), 1);
        assert_eq!(cut(&[1, 500], l), 1);
        assert_eq!(cut(&[u64::MAX, u64::MAX], l), 1);
        assert_eq!(BatchLimits::new(0, 0).max_ids, 1);
        assert_eq!(
            BatchLimits::DEFAULT,
            BatchLimits {
                max_ids: 256,
                max_bytes: 32 * 1024 * 1024
            }
        );
    }
}
