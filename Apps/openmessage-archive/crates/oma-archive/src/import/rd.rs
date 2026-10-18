//! The Recently Deleted state the passes after P3 share (spec §8.4 P5b,
//! §8.8, D60).
//!
//! - **Which messages were left out or only re-observed** is re-derived
//!   after P3 from durable state: of the rows the preflight classified
//!   ([`RdPlan`]), a row P3 did not map in this session was left out
//!   (excluded), and a row it mapped was only re-observed. A resumed session
//!   therefore makes the same decisions as an uninterrupted one, whatever
//!   the archive looked like when the resumed attempt's preflight ran.
//! - **P5b, the attachments linked only to excluded messages**: two streams
//!   of `message_attachment_join`. The first collects the attachment rowids
//!   linked to an excluded message; the second removes those also linked to
//!   any other message. What remains gets no attachment row (P6) and no
//!   link (P7), and, once the store exists (slice 4i), no file. A join row
//!   with an unreadable side links nothing and is ignored here. The
//!   preflight runs the same scan first, so the cap below refuses a source
//!   before anything is written.
//!
//! Joins of a left-out or re-observed message are skipped (P5, P7) and
//! counted as such: a re-observed message is "not updated from its deleted
//! state" (§8.8), which includes its links.
//!
//! Only rowids are held, never a key or a value. With the opt-in
//! (`include_recently_deleted`) every list here is empty.

use std::ops::ControlFlow;

use oma_messages::{Col, Cursor, MessageAttachmentLink, MessagesCtx, MessagesError, SmsDb};
use rusqlite::{Connection, OptionalExtension};

use super::preflight::RdPlan;
use crate::schema::codes::Entity;

/// The most distinct attachment rowids the scan holds (32 MiB of rowids;
/// the same figure as P0's cap).
pub(crate) const RD_ATTACHMENT_CAP: usize = 4_194_304;

/// The Recently Deleted decisions the passes after P3 apply. Every list is
/// sorted. Its `Debug` prints sizes only.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct RdSkip {
    /// Message rowids left out: no row, no map, no link, no marker.
    pub(crate) excluded: Vec<i64>,
    /// Message rowids only re-observed: mapped, but no link is written.
    pub(crate) reobserved: Vec<i64>,
    /// Attachment rowids linked only to excluded messages (P5b).
    pub(crate) attachments: Vec<i64>,
}

impl std::fmt::Debug for RdSkip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RdSkip(excluded {}, reobserved {}, attachments {})",
            self.excluded.len(),
            self.reobserved.len(),
            self.attachments.len()
        )
    }
}

impl RdSkip {
    /// The links of this message rowid are skipped.
    pub(crate) fn skips_message(&self, rowid: i64) -> bool {
        self.excluded.binary_search(&rowid).is_ok() || self.reobserved.binary_search(&rowid).is_ok()
    }

    /// This attachment rowid gets no row and no link.
    pub(crate) fn skips_attachment(&self, rowid: i64) -> bool {
        self.attachments.binary_search(&rowid).is_ok()
    }
}

/// Re-derive the message decisions after P3 from this session's map:
/// every classified row P3 did not map was left out, every one it mapped
/// was only re-observed. The attachments are filled by [`scan_attachments`].
pub(crate) fn derive_after_p3(
    conn: &Connection,
    session: i64,
    plan: &RdPlan,
) -> rusqlite::Result<RdSkip> {
    let mut out = RdSkip::default();
    let mut stmt = conn.prepare(
        "SELECT 1 FROM import_rowid_map WHERE session_id = ?1 AND src = ?2 AND src_rowid = ?3",
    )?;
    let mut mapped = |rowid: i64| -> rusqlite::Result<bool> {
        Ok(stmt
            .query_row(
                rusqlite::params![session, Entity::Message.code(), rowid],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    };
    for rowid in &plan.excluded {
        if !mapped(*rowid)? {
            out.excluded.push(*rowid);
        }
    }
    for rowid in &plan.archived {
        if mapped(*rowid)? {
            out.reobserved.push(*rowid);
        } else {
            // The archived row could not be found again: left out.
            out.excluded.push(*rowid);
        }
    }
    out.excluded.sort_unstable();
    out.excluded.dedup();
    out.reobserved.sort_unstable();
    out.reobserved.dedup();
    Ok(out)
}

/// What the attachment scan found.
pub(crate) enum AttachmentScan {
    /// The attachment rowids linked only to `excluded` messages, sorted.
    Done(Vec<i64>),
    /// More than [`RD_ATTACHMENT_CAP`] distinct candidates.
    OverLimit,
}

/// Sort and deduplicate `v`; whether it then fits the cap.
fn compact(v: &mut Vec<i64>) -> bool {
    v.sort_unstable();
    v.dedup();
    v.len() <= RD_ATTACHMENT_CAP
}

/// The attachment rowids linked only to the message rowids in `excluded`
/// (sorted): spec §8.2 step 4 and P5b. No pass runs when `excluded` is
/// empty.
pub(crate) fn scan_attachments(
    db: &SmsDb<'_>,
    excluded: &[i64],
    ctx: &mut MessagesCtx<'_>,
) -> Result<AttachmentScan, MessagesError> {
    if excluded.is_empty() {
        return Ok(AttachmentScan::Done(Vec::new()));
    }
    let is_excluded = |m: i64| excluded.binary_search(&m).is_ok();
    // 1. Linked to an excluded message.
    let mut candidates: Vec<i64> = Vec::new();
    let mut over = false;
    db.for_each_message_attachment(Cursor::Start, ctx, |l: MessageAttachmentLink| {
        if let (Col::Value(m), Col::Value(a)) = (&l.message_id, &l.attachment_id) {
            if is_excluded(*m) {
                candidates.push(*a);
                if candidates.len() > RD_ATTACHMENT_CAP.saturating_mul(2)
                    && !compact(&mut candidates)
                {
                    over = true;
                    return ControlFlow::Break(());
                }
            }
        }
        ControlFlow::Continue(())
    })?;
    if over || !compact(&mut candidates) {
        return Ok(AttachmentScan::OverLimit);
    }
    if candidates.is_empty() {
        return Ok(AttachmentScan::Done(candidates));
    }
    // 2. Not linked to any other message.
    let mut keep = vec![true; candidates.len()];
    db.for_each_message_attachment(Cursor::Start, ctx, |l: MessageAttachmentLink| {
        if let (Col::Value(m), Col::Value(a)) = (&l.message_id, &l.attachment_id) {
            if !is_excluded(*m) {
                if let Ok(at) = candidates.binary_search(a) {
                    if let Some(k) = keep.get_mut(at) {
                        *k = false;
                    }
                }
            }
        }
        ControlFlow::<()>::Continue(())
    })?;
    let out: Vec<i64> = candidates
        .into_iter()
        .zip(keep)
        .filter_map(|(a, k)| k.then_some(a))
        .collect();
    Ok(AttachmentScan::Done(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_follow_the_sorted_lists() {
        let s = RdSkip {
            excluded: vec![-3, 7],
            reobserved: vec![2],
            attachments: vec![i64::MIN, 5],
        };
        assert!(s.skips_message(7) && s.skips_message(2) && s.skips_message(-3));
        assert!(!s.skips_message(5));
        assert!(s.skips_attachment(i64::MIN) && s.skips_attachment(5));
        assert!(!s.skips_attachment(7));
        assert_eq!(
            format!("{s:?}"),
            "RdSkip(excluded 2, reobserved 1, attachments 2)"
        );
        let mut v = vec![3, 1, 3, 2];
        assert!(compact(&mut v));
        assert_eq!(v, [1, 2, 3]);
    }
}
