//! Date navigation (M5 spec §3.5, §8.7): `seek_date` to a visible item and
//! `day_counts` of visible items.
//!
//! - **Dated only.** Both read the dated range (`sort_key < X'01'`); an
//!   undated row is never "on or after" a date and is never counted in a
//!   day (D91). Keys come from [`crate::sortkey`], the import's one
//!   encoder, so a seek key equals the import's key for the same instant.
//! - **Visible items.** A reaction that folds into a timeline item of the
//!   same chat (§3.5 step 3) is not an item: a seek moves past it to the
//!   next visible item, and a day does not count it. The day count's SQL
//!   states the folding rule of [`super::folds_into`] in one statement; a
//!   NULL `assoc_kind` is not a reaction (`coalesce`, so SQL's NULL logic
//!   never drops a visible row).
//! - **Rust is time-zone-free (D104).** The caller passes UTC instants: the
//!   webview builds local-day bounds with ICU and DST-correct days.

use std::time::Instant;

use super::thread::{batch, Bound, SQL_DATED_NEWER, SQL_DATED_OLDER};
use super::{folds_into, ChatId, MessageId};
use crate::conn::{Phase, ViewBudget};
use crate::error::ArchiveError;
use crate::reader::ArchiveReader;
use crate::sortkey;

/// The most bounds one [`ArchiveReader::day_counts`] call takes: 42 days,
/// a six-week calendar.
pub const DAY_BOUNDS: usize = 43;
/// Timeline rows a seek reads per statement.
const SEEK_BATCH: usize = 64;

/// Which way a seek reads from its instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SeekDir {
    /// The first visible item dated at or after the instant.
    OnOrAfter,
    /// The last visible item dated strictly before the instant.
    Before,
}

/// Visible items per interval, one count per pair of consecutive bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DayCounts {
    pub counts: Vec<u64>,
}

const SQL_DAY_COUNT: &str = "SELECT count(*) FROM chat_message cm \
     JOIN message m ON m.id = cm.message_id \
     WHERE cm.chat_id = ?1 AND cm.sort_key >= ?2 AND cm.sort_key < ?3 \
       AND cm.sort_key < X'01' \
       AND NOT (m.assoc_present = 1 \
         AND (coalesce(m.assoc_kind, 0) BETWEEN 10 AND 17 \
           OR coalesce(m.assoc_kind, 0) BETWEEN 20 AND 27) \
         AND EXISTS (SELECT 1 FROM message t \
           JOIN chat_message x ON x.chat_id = cm.chat_id AND x.message_id = t.id \
           WHERE t.key_kind = 1 AND t.guid = m.assoc_target_guid))";

#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    vec![("viewer day count", SQL_DAY_COUNT.to_owned())]
}

fn seek(p: &Phase<'_>, chat: i64, ns: i64, dir: SeekDir) -> Result<Option<i64>, ArchiveError> {
    let (sql, mut cursor) = match dir {
        // Every key equal to the instant's is at or after it.
        SeekDir::OnOrAfter => (
            SQL_DATED_NEWER,
            Bound {
                key: sortkey::dated(ns).to_vec(),
                id: i64::MIN,
            },
        ),
        SeekDir::Before => (
            SQL_DATED_OLDER,
            Bound {
                key: sortkey::dated(ns).to_vec(),
                id: i64::MIN,
            },
        ),
    };
    loop {
        let got = p.sql(|c| batch(c, sql, chat, &cursor, SEEK_BATCH))?;
        let n = got.len();
        for (id, key) in got {
            p.checkpoint()?;
            if p.sql(|c| folds_into(c, chat, id))?.is_none() {
                return Ok(Some(id));
            }
            cursor = Bound { key, id };
        }
        if n < SEEK_BATCH {
            return Ok(None);
        }
    }
}

impl ArchiveReader {
    /// The visible item of chat `chat` nearest the UTC instant `utc_ns`
    /// in `dir` (§3.5): the first dated item at or after it, or the last
    /// one before it. Folded reactions are skipped; undated rows are never
    /// returned. `None` when there is none (for example past the last
    /// dated item). The `Scan` phase under a 2 s deadline; a trip is
    /// `QueryTooBroad`.
    pub fn seek_date(
        &self,
        chat: ChatId,
        utc_ns: i64,
        dir: SeekDir,
    ) -> Result<Option<MessageId>, ArchiveError> {
        self.require_current()?;
        let conn = self.conn();
        let deadline = Instant::now() + ViewBudget::Scan.window();
        conn.snapshot(|| {
            conn.viewing(ViewBudget::Scan, None, deadline, |p| {
                seek(p, chat.get(), utc_ns, dir).map(|o| o.map(MessageId::new))
            })
        })
    }

    /// Visible items of chat `chat` in each half-open interval
    /// `[bounds[i], bounds[i + 1])` of UTC instants (§3.5, §8.7): reactions
    /// folded into the chat's items are not counted, and undated rows never
    /// are. `bounds` must hold 2 to [`DAY_BOUNDS`] strictly increasing
    /// instants. The `Month` phase under a 2 s deadline; a trip is
    /// `QueryTooBroad`.
    pub fn day_counts(
        &self,
        chat: ChatId,
        bounds_utc_ns: &[i64],
    ) -> Result<DayCounts, ArchiveError> {
        self.require_current()?;
        if bounds_utc_ns.len() < 2
            || bounds_utc_ns.len() > DAY_BOUNDS
            || bounds_utc_ns.windows(2).any(|w| w.first() >= w.get(1))
        {
            return Err(ArchiveError::Internal("day bounds"));
        }
        let conn = self.conn();
        let deadline = Instant::now() + ViewBudget::Month.window();
        conn.snapshot(|| {
            conn.viewing(ViewBudget::Month, None, deadline, |p| {
                let mut counts = Vec::with_capacity(bounds_utc_ns.len() - 1);
                for w in bounds_utc_ns.windows(2) {
                    let (Some(&a), Some(&b)) = (w.first(), w.get(1)) else {
                        continue;
                    };
                    p.checkpoint()?;
                    let n: i64 = p.sql(|c| {
                        c.prepare(SQL_DAY_COUNT)?.query_row(
                            rusqlite::params![
                                chat.get(),
                                sortkey::dated(a).as_slice(),
                                sortkey::dated(b).as_slice()
                            ],
                            |r| r.get(0),
                        )
                    })?;
                    counts.push(u64::try_from(n).unwrap_or(0));
                }
                Ok(DayCounts { counts })
            })
        })
    }
}
