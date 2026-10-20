//! The export cursors (M6 spec §3.2, §3.4, §4.10).
//!
//! - [`TimelineCursor`] walks one conversation's timeline
//!   (`chat_message_timeline`), or the messages in no conversation
//!   (`message_sort`, probing `chat_message_by_message`), by keyset over
//!   `(sort_key, id)`: the dated range first, bounded by the export's
//!   half-open range, then, for [`TimeRange::All`] only, the undated range
//!   (`sort_key >= X'01'`, ordered by the source order of the
//!   earliest-ranked session, §4.10). A dated range never reaches an
//!   undated row.
//! - [`IdCursor`] walks a given list of message ids in its order.
//! - [`HistoryCursor`] streams one message's body versions one at a time,
//!   oldest rank first, each with every phone edit entry (no cap).
//!
//! **Materialised, then reset.** `next_batch` steps its statement to the
//! end of the batch and resets it (the statement goes back to the
//! snapshot's cache, which resets it) before anything else runs on the
//! connection: the reaction fold check, the pre-measure and, after it
//! returns, `assemble`, `history` and `open_blob`. No statement ever steps
//! while another runs on the same connection. Sort keys stay inside the
//! cursor.
//!
//! **Folding.** In a conversation, a reaction row whose target is a
//! timeline item of the same conversation folds into it (M5 §3.5 step 3):
//! it is reported in [`CursorBatch::folded`] with its target, never as an
//! item. Outside a conversation nothing folds.
//!
//! **Batches** are cut by [`BatchLimits`] over the items' pre-measure; the
//! keyset position moves only past what the batch returns, so rows cut off
//! by the byte budget are read again by the next call. [`TimelineCursor::rewind`]
//! starts over (the job's pre-count and each later pass use the same
//! snapshot).

use sha2::{Digest, Sha256};

use super::measure::{cut, measure_one, BatchLimits, ExportDepth};
use super::scope::left_out;
use super::ExportSnapshot;
use crate::error::ArchiveError;
use crate::schema::codes::{DisplayNoneReason, EditLabelCode};
use crate::sortkey;
use crate::view::assemble::{Caps, Reveal, WithheldReason};
use crate::view::detail::{version_row, version_view, withheld_reason};
use crate::view::raw::{coded, opt_coded, Coded};
use crate::view::{folds_into, BodyVersion, BodyView, MessageId};

/// Which messages an export covers in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TimeRange {
    /// Every message: the dated range, then the undated one.
    All,
    /// Messages dated in `[from_ns, until_ns)` (Unix nanoseconds, UTC).
    /// Undated messages are never in a dated range.
    Dated { from_ns: i64, until_ns: i64 },
}

impl TimeRange {
    /// The dated sort-key bounds `[from, until)` of the range.
    pub(crate) fn dated_bounds(self) -> (Vec<u8>, Vec<u8>) {
        match self {
            TimeRange::All => (vec![sortkey::DATED_TAG], sortkey::DATED_END.to_vec()),
            TimeRange::Dated { from_ns, until_ns } => (
                sortkey::dated(from_ns).to_vec(),
                sortkey::dated(until_ns).to_vec(),
            ),
        }
    }
}

/// One batch of a cursor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct CursorBatch {
    /// The timeline items, in timeline order.
    pub items: Vec<MessageId>,
    /// Reaction rows folded into a target in this conversation, as
    /// `(row, target)`, in timeline order.
    pub folded: Vec<(MessageId, MessageId)>,
    /// The items' pre-measure, in bytes.
    pub measured_bytes: u64,
}

// ---------------------------------------------------------------------------
// Statements (each pinned in tests/query_plans.rs)
// ---------------------------------------------------------------------------

pub(crate) const SQL_TIMELINE_DATED: &str = "SELECT message_id, sort_key FROM chat_message \
     WHERE chat_id = ?1 AND (sort_key, message_id) > (?2, ?3) AND sort_key < X'01' \
       AND sort_key >= ?4 AND sort_key < ?5 \
     ORDER BY sort_key, message_id LIMIT ?6";
pub(crate) const SQL_TIMELINE_UNDATED: &str = "SELECT message_id, sort_key FROM chat_message \
     WHERE chat_id = ?1 AND (sort_key, message_id) > (?2, ?3) AND sort_key >= X'01' \
     ORDER BY sort_key, message_id LIMIT ?4";
pub(crate) const SQL_UNASSIGNED_DATED: &str = "SELECT id, sort_key FROM message m \
     WHERE (sort_key, id) > (?1, ?2) AND sort_key < X'01' \
       AND sort_key >= ?3 AND sort_key < ?4 \
       AND NOT EXISTS (SELECT 1 FROM chat_message WHERE message_id = m.id) \
     ORDER BY sort_key, id LIMIT ?5";
pub(crate) const SQL_UNASSIGNED_UNDATED: &str = "SELECT id, sort_key FROM message m \
     WHERE (sort_key, id) > (?1, ?2) AND sort_key >= X'01' \
       AND NOT EXISTS (SELECT 1 FROM chat_message WHERE message_id = m.id) \
     ORDER BY sort_key, id LIMIT ?3";
pub(crate) const SQL_VERSION_LIST: &str =
    "SELECT id, first_rank FROM message_body WHERE message_id = ?1";
pub(crate) const SQL_VERSION_ROW: &str = "SELECT id, first_rank, first_session_id, \
       last_session_id, display_text, display_source, display_none_reason, attributed_outcome, \
       attributed_error, run_issue, text_raw_st, edit_label \
     FROM message_body WHERE id = ?1";

#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    [
        ("export timeline dated", SQL_TIMELINE_DATED),
        ("export timeline undated", SQL_TIMELINE_UNDATED),
        ("export unassigned dated", SQL_UNASSIGNED_DATED),
        ("export unassigned undated", SQL_UNASSIGNED_UNDATED),
        ("export version list", SQL_VERSION_LIST),
        ("export version row", SQL_VERSION_ROW),
    ]
    .into_iter()
    .map(|(n, s)| (n, s.to_owned()))
    .collect()
}

// ---------------------------------------------------------------------------
// Timeline and unassigned cursors
// ---------------------------------------------------------------------------

/// A timeline row and the target it folds into, if it folds.
pub(crate) type FoldedRow = (i64, Option<i64>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    Dated,
    Undated,
    Done,
}

/// A keyset cursor over one conversation's timeline, or over the messages
/// in no conversation. Opaque: its sort keys never leave the crate.
pub struct TimelineCursor<'a, 's> {
    snap: &'a ExportSnapshot<'s>,
    /// `None`: the messages in no conversation.
    chat: Option<i64>,
    range: TimeRange,
    pass: Pass,
    after_key: Vec<u8>,
    after_id: i64,
    limits: BatchLimits,
    depth: ExportDepth,
}

impl std::fmt::Debug for TimelineCursor<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimelineCursor")
            .field("unassigned", &self.chat.is_none())
            .finish_non_exhaustive()
    }
}

impl<'a, 's> TimelineCursor<'a, 's> {
    pub(crate) fn new(
        snap: &'a ExportSnapshot<'s>,
        chat: Option<i64>,
        range: TimeRange,
    ) -> TimelineCursor<'a, 's> {
        let mut c = TimelineCursor {
            snap,
            chat,
            range,
            pass: Pass::Dated,
            after_key: Vec::new(),
            after_id: i64::MIN,
            limits: BatchLimits::DEFAULT,
            depth: ExportDepth::Items,
        };
        c.rewind();
        c
    }

    /// Cut batches by `limits` instead of [`BatchLimits::DEFAULT`].
    #[must_use]
    pub fn with_limits(mut self, limits: BatchLimits) -> TimelineCursor<'a, 's> {
        self.limits = limits;
        self
    }

    /// Pre-measure at `depth` (the default is [`ExportDepth::Items`]).
    #[must_use]
    pub fn with_depth(mut self, depth: ExportDepth) -> TimelineCursor<'a, 's> {
        self.depth = depth;
        self
    }

    /// Start again from the first row.
    pub fn rewind(&mut self) {
        let empty =
            matches!(self.range, TimeRange::Dated { from_ns, until_ns } if from_ns >= until_ns);
        self.pass = if empty { Pass::Done } else { Pass::Dated };
        // The keyset starts at the range's lower bound, so the first batch
        // seeks there through the index instead of filtering the rows
        // before it.
        self.after_key = self.range.dated_bounds().0;
        self.after_id = i64::MIN;
    }

    /// Up to `n` rows after the position in the current pass, read to the
    /// end and reset before this returns.
    fn rows(&self, n: usize) -> Result<Vec<(i64, Vec<u8>)>, ArchiveError> {
        let limit = i64::try_from(n).unwrap_or(i64::MAX);
        let (from, until) = self.range.dated_bounds();
        let key = self.after_key.as_slice();
        let s = &self.snap.stmts;
        let read = |sql: &'static str, params: &[&dyn rusqlite::ToSql]| {
            self.snap.run(|| {
                let mut st = s.prepare(sql)?;
                let rows = st.query_map(params, |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect::<rusqlite::Result<Vec<(i64, Vec<u8>)>>>()
            })
        };
        match (self.pass, self.chat) {
            (Pass::Done, _) => Ok(Vec::new()),
            (Pass::Dated, Some(chat)) => read(
                SQL_TIMELINE_DATED,
                &[&chat, &key, &self.after_id, &from, &until, &limit],
            ),
            (Pass::Undated, Some(chat)) => {
                read(SQL_TIMELINE_UNDATED, &[&chat, &key, &self.after_id, &limit])
            }
            (Pass::Dated, None) => read(
                SQL_UNASSIGNED_DATED,
                &[&key, &self.after_id, &from, &until, &limit],
            ),
            (Pass::Undated, None) => read(SQL_UNASSIGNED_UNDATED, &[&key, &self.after_id, &limit]),
        }
    }

    fn end_pass(&mut self) {
        self.pass = match (self.pass, self.range) {
            (Pass::Dated, TimeRange::All) => Pass::Undated,
            _ => Pass::Done,
        };
        self.after_key = sortkey::DATED_END.to_vec();
        self.after_id = i64::MIN;
    }

    /// The next rows in timeline order, each with the target it folds
    /// into, or `None` at the end: the walk of the preview, the pre-count
    /// and the scope index (no pre-measure, no byte cut). The statement is
    /// reset before this returns.
    pub(crate) fn next_batch_rows(&mut self) -> Result<Option<Vec<FoldedRow>>, ArchiveError> {
        loop {
            self.snap.checkpoint()?;
            if self.pass == Pass::Done {
                return Ok(None);
            }
            let rows = self.rows(self.limits.max_ids)?;
            let Some((last_id, last_key)) = rows.last().map(|(i, k)| (*i, k.clone())) else {
                self.end_pass();
                continue;
            };
            let mut out = Vec::with_capacity(rows.len());
            for (id, _) in &rows {
                let fold = match self.chat {
                    Some(chat) => self.snap.run(|| folds_into(&self.snap.stmts, chat, *id))?,
                    None => None,
                };
                out.push((*id, fold));
            }
            self.after_key = last_key;
            self.after_id = last_id;
            return Ok(Some(out));
        }
    }

    /// The next batch, or `None` at the end. A batch can hold folded rows
    /// only.
    pub fn next_batch(&mut self) -> Result<Option<CursorBatch>, ArchiveError> {
        loop {
            self.snap.checkpoint()?;
            if self.pass == Pass::Done {
                return Ok(None);
            }
            let rows = self.rows(self.limits.max_ids)?;
            if rows.is_empty() {
                self.end_pass();
                continue;
            }
            // The batch statement has been reset: the fold checks and the
            // measure run alone on the connection.
            let mut folds: Vec<Option<i64>> = Vec::with_capacity(rows.len());
            let mut skip: Vec<bool> = Vec::with_capacity(rows.len());
            for (id, _) in &rows {
                let fold = match self.chat {
                    Some(chat) => self.snap.run(|| folds_into(&self.snap.stmts, chat, *id))?,
                    None => None,
                };
                skip.push(left_out(self.snap, *id, fold)?);
                folds.push(fold);
            }
            let mut sizes = Vec::new();
            for (((id, _), fold), skipped) in rows.iter().zip(&folds).zip(&skip) {
                if fold.is_none() && !skipped {
                    sizes.push(
                        self.snap
                            .run(|| measure_one(&self.snap.stmts, *id, self.depth))?,
                    );
                }
            }
            let take = cut(&sizes, self.limits);
            let mut batch = CursorBatch::default();
            let mut consumed = 0usize;
            for (((id, _), fold), skipped) in rows.iter().zip(&folds).zip(&skip) {
                match (fold, skipped) {
                    // Left out (an event, or a reaction folded into one):
                    // passed over, never returned.
                    (_, true) => {}
                    (Some(target), false) => batch
                        .folded
                        .push((MessageId::new(*id), MessageId::new(*target))),
                    (None, false) => {
                        if batch.items.len() == take {
                            break;
                        }
                        let size = sizes.get(batch.items.len()).copied().unwrap_or(0);
                        batch.measured_bytes = batch.measured_bytes.saturating_add(size);
                        batch.items.push(MessageId::new(*id));
                    }
                }
                consumed += 1;
            }
            if let Some((id, key)) = consumed.checked_sub(1).and_then(|i| rows.get(i)) {
                self.after_key.clone_from(key);
                self.after_id = *id;
            }
            if batch.items.is_empty() && batch.folded.is_empty() {
                // Every row read was left out: read on.
                continue;
            }
            return Ok(Some(batch));
        }
    }
}

// ---------------------------------------------------------------------------
// Id-list cursor
// ---------------------------------------------------------------------------

/// A cursor over a given list of message ids, in the list's order, cut
/// into byte-sized batches. Nothing folds.
pub struct IdCursor<'a, 's> {
    snap: &'a ExportSnapshot<'s>,
    ids: Vec<i64>,
    next: usize,
    limits: BatchLimits,
    depth: ExportDepth,
}

impl std::fmt::Debug for IdCursor<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdCursor")
            .field("ids", &self.ids.len())
            .finish_non_exhaustive()
    }
}

impl<'a, 's> IdCursor<'a, 's> {
    pub(crate) fn new(snap: &'a ExportSnapshot<'s>, ids: &[MessageId]) -> IdCursor<'a, 's> {
        IdCursor {
            snap,
            ids: ids.iter().map(|m| m.get()).collect(),
            next: 0,
            limits: BatchLimits::DEFAULT,
            depth: ExportDepth::Items,
        }
    }

    /// Cut batches by `limits` instead of [`BatchLimits::DEFAULT`].
    #[must_use]
    pub fn with_limits(mut self, limits: BatchLimits) -> IdCursor<'a, 's> {
        self.limits = limits;
        self
    }

    /// Pre-measure at `depth`.
    #[must_use]
    pub fn with_depth(mut self, depth: ExportDepth) -> IdCursor<'a, 's> {
        self.depth = depth;
        self
    }

    /// Start again from the first id.
    pub fn rewind(&mut self) {
        self.next = 0;
    }

    /// The next batch, or `None` at the end. Ids the snapshot leaves out
    /// (events, when it leaves them out) are passed over.
    pub fn next_batch(&mut self) -> Result<Option<CursorBatch>, ArchiveError> {
        loop {
            self.snap.checkpoint()?;
            let rest = self.ids.get(self.next..).unwrap_or(&[]);
            if rest.is_empty() {
                return Ok(None);
            }
            // (id, its size; `None`: left out)
            let mut read: Vec<(i64, Option<u64>)> = Vec::new();
            for id in rest.iter().take(self.limits.max_ids) {
                let size = if left_out(self.snap, *id, None)? {
                    None
                } else {
                    Some(
                        self.snap
                            .run(|| measure_one(&self.snap.stmts, *id, self.depth))?,
                    )
                };
                read.push((*id, size));
            }
            let sizes: Vec<u64> = read.iter().filter_map(|(_, s)| *s).collect();
            let take = cut(&sizes, self.limits);
            let mut items = Vec::new();
            let mut measured = 0u64;
            let mut consumed = 0usize;
            for (id, size) in &read {
                if let Some(size) = size {
                    if items.len() == take {
                        break;
                    }
                    items.push(MessageId::new(*id));
                    measured = measured.saturating_add(*size);
                }
                consumed += 1;
            }
            self.next += consumed;
            if items.is_empty() {
                continue;
            }
            return Ok(Some(CursorBatch {
                items,
                folded: Vec::new(),
                measured_bytes: measured,
            }));
        }
    }
}

// ---------------------------------------------------------------------------
// History sub-cursor
// ---------------------------------------------------------------------------

/// One message's body versions, oldest rank first, one at a time (M6 spec
/// §3.3): only one version's text is in memory at once, and "text
/// unchanged" compares with the previous version by digest. Every phone
/// edit entry of a version is read (no cap). Masking follows the export's
/// reveal exactly as the details do (D115): earlier versions and decoded
/// edit text are withheld for a message whose earlier text is withheld,
/// and one-time codes are masked, unless revealed.
pub struct HistoryCursor<'a, 's> {
    snap: &'a ExportSnapshot<'s>,
    versions: Vec<i64>,
    next: usize,
    current: Option<i64>,
    withhold: Option<WithheldReason>,
    reveal: Reveal,
    /// The digest of the previous version's shown text (`None` inside:
    /// it showed none); `None` before the first.
    previous: Option<Option<[u8; 32]>>,
}

impl std::fmt::Debug for HistoryCursor<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryCursor")
            .field("versions", &self.versions.len())
            .finish_non_exhaustive()
    }
}

impl<'a, 's> HistoryCursor<'a, 's> {
    pub(crate) fn new(
        snap: &'a ExportSnapshot<'s>,
        m: &crate::AssembledMessage,
        reveal: Reveal,
    ) -> Result<HistoryCursor<'a, 's>, ArchiveError> {
        let mut versions: Vec<(Vec<u8>, i64)> = snap.run(|| {
            let mut st = snap.stmts.prepare(SQL_VERSION_LIST)?;
            let rows = st.query_map([m.id.get()], |r| Ok((r.get(1)?, r.get(0)?)))?;
            rows.collect::<rusqlite::Result<_>>()
        })?;
        versions.sort();
        let current = match &m.body {
            BodyView::Text(t) => Some(t.body.get()),
            BodyView::NoText(n) => Some(n.body.get()),
            BodyView::NoBody => None,
        };
        Ok(HistoryCursor {
            snap,
            versions: versions.into_iter().map(|(_, id)| id).collect(),
            next: 0,
            current,
            withhold: withheld_reason(m),
            reveal,
            previous: None,
        })
    }

    /// How many versions the message has.
    pub fn len(&self) -> usize {
        self.versions.len()
    }

    /// `true` when the message has no stored version.
    pub fn is_empty(&self) -> bool {
        self.versions.is_empty()
    }

    /// The next version, or `None` after the last.
    pub fn next_version(&mut self) -> Result<Option<BodyVersion>, ArchiveError> {
        Ok(self.next_coded()?.map(|(v, _)| v))
    }

    /// The next version with its stored codes (slice 6d), or `None` after
    /// the last.
    pub fn next_coded(&mut self) -> Result<Option<(BodyVersion, VersionCodes)>, ArchiveError> {
        self.snap.checkpoint()?;
        let Some(&id) = self.versions.get(self.next) else {
            return Ok(None);
        };
        self.next += 1;
        let snap = self.snap;
        let row = snap
            .run(|| {
                use rusqlite::OptionalExtension;
                snap.stmts
                    .prepare(SQL_VERSION_ROW)?
                    .query_row([id], version_row)
                    .optional()
            })?
            .ok_or(ArchiveError::Damaged(crate::error::DamageKind::Unreadable))?;
        let (label, none) = row.codes();
        let codes = VersionCodes {
            edit_label: coded(label, EditLabelCode::from_code),
            display_none_reason: opt_coded(none, DisplayNoneReason::from_code),
        };
        let (mut v, shown) = snap.run(|| {
            version_view(
                &snap.stmts,
                row,
                Some(id) == self.current,
                self.withhold,
                self.reveal,
                &Caps::EXPORT,
                usize::MAX,
            )
        })?;
        let digest: Option<[u8; 32]> = shown.map(|t| Sha256::digest(t.as_bytes()).into());
        v.text_unchanged = self
            .previous
            .as_ref()
            .is_some_and(|p| p.is_some() && *p == digest);
        self.previous = Some(digest);
        Ok(Some((v, codes)))
    }
}

/// A body version's codes as stored, which [`BodyVersion`] collapses (an
/// unknown number is `None` there).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct VersionCodes {
    pub edit_label: Coded<EditLabelCode>,
    /// `None` when NULL (a version with text has none).
    pub display_none_reason: Option<Coded<DisplayNoneReason>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_map_to_half_open_dated_keys() {
        let (from, until) = TimeRange::All.dated_bounds();
        assert_eq!(from, vec![0u8]);
        assert_eq!(until, vec![1u8]);
        assert!(from.as_slice() <= sortkey::dated(i64::MIN).as_slice());
        let (from, until) = TimeRange::Dated {
            from_ns: 10,
            until_ns: 20,
        }
        .dated_bounds();
        assert!(from.as_slice() <= sortkey::dated(10).as_slice());
        assert!(sortkey::dated(19).as_slice() < until.as_slice());
        assert!(sortkey::dated(20).as_slice() >= until.as_slice());
        assert!(until.as_slice() < sortkey::DATED_END.as_slice());
    }

    #[test]
    fn the_version_row_reads_the_detail_columns() {
        let cols = crate::view::detail::VERSION_COLUMNS.replace(char::is_whitespace, "");
        let ours = SQL_VERSION_ROW.replace(char::is_whitespace, "");
        assert!(ours.contains(&cols), "{ours}");
    }
}
