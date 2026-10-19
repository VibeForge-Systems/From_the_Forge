//! The masking an export adds to D115 (M6 spec §3.8, owner Q4, D125).
//!
//! The viewer's masking applies as it is (one-time codes, earlier text,
//! decoded edit text, the subject history of an unsent message). An export
//! widens it, and both widenings are released only by the export's own
//! [`Reveal`] (the two per-export opt-ins; the viewer's
//! `revealEarlierText` setting never reaches here):
//!
//! - **Attachments of unsent parts** ([`AttachmentAccess`]). A message
//!   whose current body lists an unsent part keeps an attachment's bytes
//!   only when a part of the current body that is **not** listed as unsent
//!   names it: a run whose `attachment_guid` equals the attachment's GUID
//!   and whose `part_index` is recorded. Every other attachment of such a
//!   message (all of them when every part is unsent) is withheld unless
//!   the export reveals earlier content. Withholding removes the stored
//!   file from the record (`blob` and the stored size are `None`), so a
//!   writer that follows the record cannot copy, preview or save it; the
//!   declared facts (name, type, sizes) stay, as the viewer shows them.
//!   The GUID equality is **UNVERIFIED** (as in M5), so an attachment it
//!   cannot place in a kept part is withheld: the rule errs towards
//!   withholding.
//!
//!   The archive keeps these links: links are a union and a link once
//!   observed is never removed (`import/links.rs`; `tests/links.rs`,
//!   `links_follow_rank_in_both_orders`: "a link of an older backup stays
//!   when a newer one lacks it"). So a backup taken after the unsend
//!   leaves the earlier backup's attachment linked to the message, and
//!   this rule is what keeps it out of an export.
//! - **The plain-column alternative text** ([`AlternativeText`]): the other
//!   stored text field when the two differ (`sources_differ = 1`). It is
//!   withheld whole when the message's earlier text is withheld (an unsent
//!   part, or no current text: the column can hold the text from before
//!   the change), and, separately, when the current body has any flagged
//!   one-time code (its ranges cannot be mapped onto the plain column with
//!   certainty). A withheld value is never read.
//!
//! Nothing here logs.

use rusqlite::OptionalExtension;

use crate::stmt::StmtCache;
use crate::view::assemble::{Reveal, WithheldReason};
use crate::view::conversations::ONE_TIME_CODE_BIT;
use crate::view::detail::withheld_reason;
use crate::view::{AssembledMessage, BodyView, ContentText, ExportItem};

/// Whether an export carries an attachment's stored file (M6 spec §3.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AttachmentAccess {
    /// Not content captured before an unsend: the file is the export's to
    /// copy (when one is stored).
    Included,
    /// An attachment of an unsent part, kept out of copies, previews and
    /// "Save a copy" ("withheldUnsent"); the record has no file.
    Withheld(WithheldReason),
    /// An attachment of an unsent part, included because the export
    /// reveals earlier content.
    Revealed(WithheldReason),
}

impl AttachmentAccess {
    /// `true` when the export must not copy the file.
    pub fn is_withheld(self) -> bool {
        matches!(self, AttachmentAccess::Withheld(_))
    }
}

/// Why the alternative text is withheld.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AltTextWithheld {
    /// The message's earlier text is withheld (D115), so the plain column
    /// may hold text from before the change.
    EarlierContent(WithheldReason),
    /// The current body has a flagged one-time code.
    OneTimeCode,
}

/// Which stored field the alternative text is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AltTextSource {
    /// The plain `text` column; the shown text is the decoded
    /// `attributedBody`.
    TextColumn,
    /// The decoded `attributedBody`; the shown text came from the `text`
    /// column because the decoded text was empty (D58), so this is always
    /// empty.
    DecodedAttributedBody,
}

/// The other stored text of a message's current body, when the two
/// differ (§4.2 `alternative_text`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AlternativeText {
    /// The fields agree, cannot be compared (`sources_differ` NULL), or
    /// there is no current body.
    None,
    /// The other field, whole (no cap).
    Text {
        source: AltTextSource,
        text: ContentText,
    },
    /// Withheld unless the export reveals it.
    Withheld(AltTextWithheld),
}

pub(crate) const SQL_ATTACHMENT_KEPT: &str = "SELECT 1 FROM message_body_part p \
       JOIN attachment a ON a.id = ?2 AND a.key_kind = 1 AND p.attachment_guid = a.guid \
     WHERE p.body_id = ?1 AND p.part_index IS NOT NULL \
       AND NOT EXISTS (SELECT 1 FROM message_unsent_part u \
         WHERE u.body_id = ?1 AND u.part = p.part_index) \
     LIMIT 1";
pub(crate) const SQL_ALT_FACTS: &str =
    "SELECT display_source, sources_differ FROM message_body WHERE id = ?1";
pub(crate) const SQL_ALT_TEXT: &str = "SELECT text_raw FROM message_body WHERE id = ?1";
pub(crate) const SQL_ANY_CODE: &str = "SELECT 1 FROM message_body_part \
     WHERE body_id = ?1 AND (flags & ?2) <> 0 LIMIT 1";

#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    [
        ("export attachment kept by a part", SQL_ATTACHMENT_KEPT),
        ("export alternative text facts", SQL_ALT_FACTS),
        ("export alternative text", SQL_ALT_TEXT),
        ("export any flagged code", SQL_ANY_CODE),
    ]
    .into_iter()
    .map(|(n, s)| (n, s.to_owned()))
    .collect()
}

/// The current body of `m`, when it has one.
pub(crate) fn current_body(m: &AssembledMessage) -> Option<i64> {
    match &m.body {
        BodyView::Text(t) => Some(t.body.get()),
        BodyView::NoText(n) => Some(n.body.get()),
        BodyView::NoBody => None,
    }
}

/// Whether a part of body `body` that is not listed as unsent names
/// attachment `attachment` (see the module comment).
pub(crate) fn kept_by_a_part(
    s: &StmtCache<'_>,
    body: i64,
    attachment: i64,
) -> rusqlite::Result<bool> {
    Ok(s.prepare(SQL_ATTACHMENT_KEPT)?
        .query_row([body, attachment], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Whether body `body` has a part flagged as a one-time code.
pub(crate) fn has_code(s: &StmtCache<'_>, body: i64) -> rusqlite::Result<bool> {
    Ok(s.prepare(SQL_ANY_CODE)?
        .query_row(rusqlite::params![body, 1i64 << ONE_TIME_CODE_BIT], |_| {
            Ok(())
        })
        .optional()?
        .is_some())
}

/// The access of each of `m`'s attachments, in order, under `reveal`.
pub(crate) fn attachment_access(
    s: &StmtCache<'_>,
    m: &AssembledMessage,
    reveal: Reveal,
) -> rusqlite::Result<Vec<AttachmentAccess>> {
    let unsent = !m.edit.unsent_parts.is_empty();
    let body = current_body(m);
    let mut out = Vec::with_capacity(m.attachments.len());
    for a in &m.attachments {
        let of_unsent_part = match body {
            Some(b) if unsent => !kept_by_a_part(s, b, a.id.get())?,
            _ => false,
        };
        out.push(match (of_unsent_part, reveal.earlier_text) {
            (false, _) => AttachmentAccess::Included,
            (true, false) => AttachmentAccess::Withheld(WithheldReason::Unsent),
            (true, true) => AttachmentAccess::Revealed(WithheldReason::Unsent),
        });
    }
    Ok(out)
}

/// Take the stored file out of every withheld attachment of `item`: the
/// record keeps the declared facts and loses the blob and its size.
pub(crate) fn withhold_files(item: &mut ExportItem, access: &[AttachmentAccess]) {
    for (i, a) in access.iter().enumerate() {
        if !a.is_withheld() {
            continue;
        }
        if let Some(r) = item.message.attachments.get_mut(i) {
            r.blob = None;
            r.stored_bytes = None;
        }
        if let Some(c) = item.codes.attachments.get_mut(i) {
            c.stored_bytes = None;
        }
    }
}

/// The alternative text of `m` under `reveal` (see the module comment).
pub(crate) fn alternative_text(
    s: &StmtCache<'_>,
    m: &AssembledMessage,
    reveal: Reveal,
) -> rusqlite::Result<AlternativeText> {
    let Some(body) = current_body(m) else {
        return Ok(AlternativeText::None);
    };
    let facts: Option<(Option<i64>, Option<i64>)> = s
        .prepare(SQL_ALT_FACTS)?
        .query_row([body], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((source, Some(1))) = facts else {
        return Ok(AlternativeText::None);
    };
    if !reveal.earlier_text {
        if let Some(reason) = withheld_reason(m) {
            return Ok(AlternativeText::Withheld(AltTextWithheld::EarlierContent(
                reason,
            )));
        }
    }
    if !reveal.one_time_code && has_code(s, body)? {
        return Ok(AlternativeText::Withheld(AltTextWithheld::OneTimeCode));
    }
    if source != Some(1) {
        // The shown text is the `text` column: the decoded
        // `attributedBody` it differs from held no text (D58).
        return Ok(AlternativeText::Text {
            source: AltTextSource::DecodedAttributedBody,
            text: ContentText::from_str("", u32::MAX),
        });
    }
    let raw: Option<Vec<u8>> = s
        .prepare(SQL_ALT_TEXT)?
        .query_row([body], |r| {
            Ok(match r.get_ref(0)? {
                rusqlite::types::ValueRef::Blob(b) | rusqlite::types::ValueRef::Text(b) => {
                    Some(b.to_vec())
                }
                _ => None,
            })
        })
        .optional()?
        .flatten();
    let Some(raw) = raw else {
        return Ok(AlternativeText::None);
    };
    let (text, lossy) = match String::from_utf8(raw) {
        Ok(t) => (t, false),
        Err(e) => (String::from_utf8_lossy(e.as_bytes()).into_owned(), true),
    };
    Ok(AlternativeText::Text {
        source: AltTextSource::TextColumn,
        text: ContentText::from_str(&text, u32::MAX).lossy(lossy),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_withheld_access_keeps_the_file_out() {
        assert!(!AttachmentAccess::Included.is_withheld());
        assert!(AttachmentAccess::Withheld(WithheldReason::Unsent).is_withheld());
        assert!(!AttachmentAccess::Revealed(WithheldReason::Unsent).is_withheld());
    }

    #[test]
    fn the_kept_part_rule_names_unsent_parts_by_their_index() {
        let sql = SQL_ATTACHMENT_KEPT.replace(char::is_whitespace, "");
        assert!(sql.contains("u.part=p.part_index"), "{sql}");
        assert!(sql.contains("p.part_indexISNOTNULL"), "{sql}");
        assert!(sql.contains("a.key_kind=1"), "{sql}");
    }
}
