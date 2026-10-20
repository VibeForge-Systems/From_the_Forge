//! The preview's exact counts, the dated span and the archive generation
//! (M6 spec §3.4, §3.7, D147).
//!
//! ## Counts
//!
//! [`ExportPreview`] holds counts and sizes only. They are **exact**: one
//! walk over every timeline row of the scope (the walk the job's pre-count
//! and the scope index use, `scope::walk`), with no op budget and no
//! deadline, stopped only by the cancel token, posting [`PreviewProgress`]
//! as it goes. [`ArchiveReader::export_preview`](crate::ArchiveReader::export_preview)
//! runs it in its own read transaction; the job runs the same code inside
//! its snapshot ([`super::ExportSnapshot::precount`]) and compares the
//! reveal counts with what the user confirmed. Per distinct message the
//! walk runs a few primary-key statements (its bodies, their flagged
//! ranges, whether an unsent part is listed, its attachments), never the
//! assembly.
//!
//! Every distinct message the export writes is counted once, whether it
//! is a timeline item or a reaction row folded into its target: the
//! pipeline writes a folded row as a whole item too (body, attachments
//! and copies, history), so its reveal flags, attachments and stored
//! files are in every count below, and `distinct_messages` includes it
//! (the manifest's `crossConversationRows` is `timeline_rows −
//! distinct_messages`).
//!
//! The two reveal counts are the confirmation's "N messages" and are
//! never understated (D147): a message counts for
//!
//! - **one-time codes** when any of its body versions has a part flagged
//!   as a code (whether or not that version would be written), or, when
//!   history is written ([`ExportDepth::WithHistory`]), when a phone edit
//!   entry holds edit text (`body_present = 1`): that text is decoded at
//!   read time, its codes come from the summary (masked whole when its
//!   runs are incomplete), and the reveal lifts both, so every such
//!   message is counted without decoding it;
//! - **earlier content** when its current body lists an unsent part (its
//!   earlier text, decoded edit text, alternative text, subject history and
//!   the attachments of unsent parts are withheld), or when its current
//!   body has no text and another version holds text or a phone edit entry
//!   holds edit text (D115's other withheld cases).
//!
//! `text_bytes` is an estimate's input, computed after masking under the
//! preview's reveal: the shown current text (masked code bytes removed, a
//! body whose code ranges are not trusted counted as withheld), revealed
//! earlier text, and with history every version that would be written.
//! Decoded edit text and the alternative text are not counted.
//! `image_previews` counts attachments with a stored file whose
//! **declared** type is an image: it sizes the estimate only; the job
//! sniffs every file.
//!
//! ## The generation
//!
//! A draft records the generation its preview saw; `start_export`
//! compares it and refuses a changed one. Only an import writes archive
//! rows, and every import works under a session row, so the generation is
//! the newest session (its id, state and resume count), the newest
//! completed session, and the highest row id of each table an import adds
//! content to: messages, body versions, attachments and stored files.
//! Rows are never deleted (D57), so every insert moves a highest id, and
//! each is one seek on the table's primary key. The value is opaque: it
//! is only compared.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension};

use super::cursor::TimeRange;
use super::mask::kept_by_a_part;
use super::measure::ExportDepth;
use super::scope::{walk, ExportScopeIds, ScopeRow};
use super::ExportSnapshot;
use crate::error::ArchiveError;
use crate::sortkey;
use crate::stmt::StmtCache;
use crate::view::assemble::Reveal;
use crate::view::conversations::ONE_TIME_CODE_BIT;
use crate::view::detail::SQL_ANY_UNSENT;

/// What identifies the archive's content at one moment. Equal
/// generations mean no import wrote in between. Opaque: only compared,
/// and `Debug` prints none of it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArchiveGeneration {
    newest_completed_session: Option<i64>,
    /// The newest session's id, state code and resume count (a resumed
    /// session writes under the same id).
    newest_session: Option<(i64, i64, i64)>,
    max_message: Option<i64>,
    max_body: Option<i64>,
    max_attachment: Option<i64>,
    max_blob: Option<i64>,
}

impl std::fmt::Debug for ArchiveGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ArchiveGeneration(..)")
    }
}

pub(crate) const SQL_GENERATION: &str = "SELECT \
       (SELECT max(id) FROM import_session WHERE state = 1), \
       (SELECT max(id) FROM message), \
       (SELECT max(id) FROM message_body), \
       (SELECT max(id) FROM attachment), \
       (SELECT max(id) FROM blob)";
pub(crate) const SQL_NEWEST_SESSION: &str =
    "SELECT id, state, resumes FROM import_session ORDER BY id DESC LIMIT 1";

pub(crate) const SQL_PRE_MESSAGE: &str =
    "SELECT sort_key, current_body_id FROM message WHERE id = ?1";
pub(crate) const SQL_PRE_BODIES: &str = "SELECT id, first_rank, octet_length(display_text), \
       display_source, run_issue FROM message_body WHERE message_id = ?1";
pub(crate) const SQL_PRE_CODE_RANGES: &str = "SELECT utf8_start, utf8_end \
     FROM message_body_part WHERE body_id = ?1 AND (flags & ?2) <> 0";
pub(crate) const SQL_PRE_EDIT_TEXT: &str = "SELECT 1 FROM message_body b \
     JOIN message_edit_entry e ON e.body_id = b.id \
     WHERE b.message_id = ?1 AND e.body_present = 1 LIMIT 1";
pub(crate) const SQL_PRE_ATTACHMENTS: &str = "SELECT ma.attachment_id, a.current_blob_id, \
       bl.byte_len, a.mime_type \
     FROM message_attachment ma JOIN attachment a ON a.id = ma.attachment_id \
       LEFT JOIN blob bl ON bl.id = a.current_blob_id \
     WHERE ma.message_id = ?1";
pub(crate) const SQL_SPAN_CHAT_FIRST: &str = "SELECT sort_key FROM chat_message \
     WHERE chat_id = ?1 AND sort_key >= ?2 AND sort_key < ?3 ORDER BY sort_key LIMIT 1";
pub(crate) const SQL_SPAN_CHAT_LAST: &str = "SELECT sort_key FROM chat_message \
     WHERE chat_id = ?1 AND sort_key >= ?2 AND sort_key < ?3 ORDER BY sort_key DESC LIMIT 1";
pub(crate) const SQL_SPAN_ALL_FIRST: &str = "SELECT sort_key FROM message \
     WHERE sort_key >= ?1 AND sort_key < ?2 ORDER BY sort_key LIMIT 1";
pub(crate) const SQL_SPAN_ALL_LAST: &str = "SELECT sort_key FROM message \
     WHERE sort_key >= ?1 AND sort_key < ?2 ORDER BY sort_key DESC LIMIT 1";
pub(crate) const SQL_UNDATED_CHAT: &str =
    "SELECT count(*) FROM chat_message WHERE chat_id = ?1 AND sort_key >= X'01'";
pub(crate) const SQL_UNDATED_ALL: &str = "SELECT count(*) FROM message WHERE sort_key >= X'01'";

#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    [
        ("export generation", SQL_GENERATION),
        ("export newest session", SQL_NEWEST_SESSION),
        ("export precount message", SQL_PRE_MESSAGE),
        ("export precount bodies", SQL_PRE_BODIES),
        ("export precount code ranges", SQL_PRE_CODE_RANGES),
        ("export precount edit text", SQL_PRE_EDIT_TEXT),
        ("export precount attachments", SQL_PRE_ATTACHMENTS),
        ("export span chat first", SQL_SPAN_CHAT_FIRST),
        ("export span chat last", SQL_SPAN_CHAT_LAST),
        ("export span all first", SQL_SPAN_ALL_FIRST),
        ("export span all last", SQL_SPAN_ALL_LAST),
        ("export undated in chat", SQL_UNDATED_CHAT),
        ("export undated messages", SQL_UNDATED_ALL),
    ]
    .into_iter()
    .map(|(n, s)| (n, s.to_owned()))
    .collect()
}

/// The generation, read in the caller's transaction.
pub(crate) fn generation(c: &Connection) -> rusqlite::Result<ArchiveGeneration> {
    use rusqlite::OptionalExtension;
    let newest_session = c
        .prepare(SQL_NEWEST_SESSION)?
        .query_row([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    c.prepare(SQL_GENERATION)?.query_row([], |r| {
        Ok(ArchiveGeneration {
            newest_completed_session: r.get(0)?,
            newest_session,
            max_message: r.get(1)?,
            max_body: r.get(2)?,
            max_attachment: r.get(3)?,
            max_blob: r.get(4)?,
        })
    })
}

// ---------------------------------------------------------------------------
// The preview
// ---------------------------------------------------------------------------

/// What a preview counts under (the draft's options that change a
/// count).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub struct PreviewOptions {
    /// The export's own reveal (never the viewer's setting).
    pub reveal: Reveal,
    pub depth: ExportDepth,
    /// The "events" option off (slice 6k2): group and system events, and
    /// reaction rows folded into them, are left out of the counts (and
    /// the snapshot leaves them out from then on).
    pub leave_out_events: bool,
}

impl PreviewOptions {
    pub fn new(reveal: Reveal, depth: ExportDepth) -> PreviewOptions {
        PreviewOptions {
            reveal,
            depth,
            leave_out_events: false,
        }
    }

    /// The same options, leaving events out when `leave_out` is set.
    #[must_use]
    pub fn leaving_out_events(mut self, leave_out: bool) -> PreviewOptions {
        self.leave_out_events = leave_out;
        self
    }
}

/// Progress of a preview: timeline rows counted so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct PreviewProgress {
    pub rows: u64,
}

/// Rows between two progress reports.
const PROGRESS_EVERY: u64 = 1_024;

/// The exact counts of an export's scope (M6 spec §3.7): counts and sizes
/// only, never content. See the module comment for each definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExportPreview {
    /// Conversations with at least one timeline row in scope.
    pub conversations: u64,
    /// Timeline rows in scope: every conversation's rows (items and folded
    /// reactions) plus each message in no conversation.
    pub timeline_rows: u64,
    /// Distinct messages among the timeline rows, items and folded
    /// reactions alike (a message in two exported conversations counts
    /// once).
    pub distinct_messages: u64,
    /// Reaction rows folded into a target (rows, per conversation).
    pub folded_reaction_rows: u64,
    /// Undated messages a dated range leaves out: in the conversation, or
    /// in the archive (0 for the whole timeline and for given messages).
    pub undated_excluded: u64,
    /// Timeline items in no conversation.
    pub unassigned: u64,
    /// Attachment records of the distinct messages (one per message link).
    pub attachments: u64,
    /// Of those, the records whose stored file the export would copy.
    pub attachments_with_file: u64,
    /// Records withheld as attachments of unsent parts (§3.8).
    pub attachments_withheld_unsent: u64,
    /// Distinct stored files the export would copy, their total and the
    /// largest size, in bytes (the FAT 4 GiB check).
    pub distinct_blobs: u64,
    pub blob_bytes: u64,
    pub largest_blob_bytes: u64,
    /// The reveal-codes confirmation's N.
    pub messages_with_flagged_code: u64,
    /// The reveal-earlier confirmation's N.
    pub messages_with_earlier_content: u64,
    /// UTF-8 bytes of text after masking, for estimates only.
    pub text_bytes: u64,
    /// Attachments with a file whose declared type is an image, for
    /// estimates only.
    pub image_previews: u64,
    /// Timeline rows left out because the export leaves events out:
    /// group and system events, and reaction rows folded into them (0
    /// when events are included).
    pub events_left_out: u64,
    /// The earliest and latest dated timeline row in scope, in Unix
    /// milliseconds (the first floored, the last rounded up); `None` with
    /// no dated row.
    pub span_utc_ms: Option<(i64, i64)>,
    pub generation: ArchiveGeneration,
}

/// One body of a message, as the pre-count reads it.
struct PreBody {
    id: i64,
    rank: Vec<u8>,
    /// `octet_length(display_text)`; `None` without displayed text.
    len: Option<i64>,
    trusted: bool,
}

fn u64_of(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

/// The UTF-8 bytes of body `b`'s text as an export shows it: masked
/// code ranges removed unless revealed; a body whose ranges cannot be
/// trusted shows none.
fn masked_len(s: &StmtCache<'_>, b: &PreBody, reveal_codes: bool) -> rusqlite::Result<u64> {
    let Some(len) = b.len else { return Ok(0) };
    let len = u64_of(len);
    if reveal_codes {
        return Ok(len);
    }
    let ranges: Vec<(i64, i64)> = {
        let mut st = s.prepare(SQL_PRE_CODE_RANGES)?;
        let rows = st.query_map(rusqlite::params![b.id, 1i64 << ONE_TIME_CODE_BIT], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if ranges.is_empty() {
        return Ok(len);
    }
    if !b.trusted {
        return Ok(0);
    }
    let masked: u64 = ranges
        .iter()
        .map(|&(a, z)| u64_of(z.min(i64::try_from(len).unwrap_or(i64::MAX)) - a.max(0)))
        .sum();
    Ok(len.saturating_sub(masked))
}

fn has_codes(s: &StmtCache<'_>, body: i64) -> rusqlite::Result<bool> {
    super::mask::has_code(s, body)
}

/// What one distinct message adds to the counts.
#[derive(Default)]
struct MessageCounts {
    dated_ns: Option<i64>,
    flagged: bool,
    earlier: bool,
    text_bytes: u64,
}

/// The counts' running state.
struct Tally {
    preview: ExportPreview,
    seen: HashSet<i64>,
    blobs: HashSet<i64>,
    chats: HashSet<Option<i64>>,
    first_ns: Option<i64>,
    last_ns: Option<i64>,
    track_span: bool,
}

impl Tally {
    fn span_ns(&mut self, ns: i64) {
        self.first_ns = Some(self.first_ns.map_or(ns, |f| f.min(ns)));
        self.last_ns = Some(self.last_ns.map_or(ns, |l| l.max(ns)));
    }
}

/// The counts of message `id` under `opts` (one distinct message).
fn count_message(
    s: &StmtCache<'_>,
    id: i64,
    opts: PreviewOptions,
    t: &mut Tally,
) -> rusqlite::Result<MessageCounts> {
    let row: Option<(Vec<u8>, Option<i64>)> = s
        .prepare(SQL_PRE_MESSAGE)?
        .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((key, current)) = row else {
        return Ok(MessageCounts::default());
    };
    let mut bodies: Vec<PreBody> = {
        let mut st = s.prepare(SQL_PRE_BODIES)?;
        let rows = st.query_map([id], |r| {
            Ok(PreBody {
                id: r.get(0)?,
                rank: r.get(1)?,
                len: r.get(2)?,
                trusted: r.get::<_, Option<i64>>(3)? == Some(1)
                    && r.get::<_, Option<i64>>(4)?.is_none(),
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    bodies.sort_by(|a, b| (&a.rank, a.id).cmp(&(&b.rank, b.id)));
    let cur = current.and_then(|c| bodies.iter().find(|b| b.id == c));
    let cur_text = cur.is_some_and(|b| b.len.is_some());
    let unsent = match current {
        Some(c) => s
            .prepare(SQL_ANY_UNSENT)?
            .query_row([c], |_| Ok(()))
            .optional()?
            .is_some(),
        None => false,
    };
    // `detail::withheld_reason`: an unsent part, or no current text.
    let withheld = unsent || !cur_text;
    let other_text = bodies
        .iter()
        .any(|b| Some(b.id) != current && b.len.is_some());
    let earlier = unsent
        || (!cur_text
            && (other_text
                || s.prepare(SQL_PRE_EDIT_TEXT)?
                    .query_row([id], |_| Ok(()))
                    .optional()?
                    .is_some()));
    let mut flagged = false;
    for b in &bodies {
        if has_codes(s, b.id)? {
            flagged = true;
            break;
        }
    }
    if !flagged && opts.depth == ExportDepth::WithHistory {
        // Decoded edit text (history only) can hold a code the summary
        // flags, or be masked whole: counted without decoding (D147).
        flagged = s
            .prepare(SQL_PRE_EDIT_TEXT)?
            .query_row([id], |_| Ok(()))
            .optional()?
            .is_some();
    }
    let reveal = opts.reveal;
    let mut text_bytes = 0u64;
    if let Some(b) = cur {
        text_bytes += masked_len(s, b, reveal.one_time_code)?;
    }
    if withheld && reveal.earlier_text {
        // The revealed earlier text: the newest other version with text.
        if let Some(b) = bodies
            .iter()
            .rev()
            .find(|b| Some(b.id) != current && b.len.is_some())
        {
            text_bytes += masked_len(s, b, reveal.one_time_code)?;
        }
    }
    if opts.depth == ExportDepth::WithHistory {
        for b in &bodies {
            if Some(b.id) == current || !withheld || reveal.earlier_text {
                text_bytes += masked_len(s, b, reveal.one_time_code)?;
            }
        }
    }
    // Attachments: one record per link; a file is copied unless withheld.
    // (attachment, blob, its size, the first bytes of the declared type)
    type AttRow = (i64, Option<i64>, Option<i64>, Option<Vec<u8>>);
    let atts: Vec<AttRow> = {
        let mut st = s.prepare(SQL_PRE_ATTACHMENTS)?;
        let rows = st.query_map([id], |r| {
            let mime = match r.get_ref(3)? {
                rusqlite::types::ValueRef::Blob(b) | rusqlite::types::ValueRef::Text(b) => {
                    Some(b.get(..6).unwrap_or(b).to_vec())
                }
                _ => None,
            };
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, mime))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (att, blob, len, mime) in atts {
        let p = &mut t.preview;
        p.attachments += 1;
        let withheld_file = match current {
            Some(c) if unsent && !reveal.earlier_text => !kept_by_a_part(s, c, att)?,
            _ => false,
        };
        if withheld_file {
            p.attachments_withheld_unsent += 1;
            continue;
        }
        let Some(blob) = blob else { continue };
        p.attachments_with_file += 1;
        if mime
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case(b"image/"))
        {
            p.image_previews += 1;
        }
        if t.blobs.insert(blob) {
            let len = u64_of(len.unwrap_or(0));
            p.distinct_blobs += 1;
            p.blob_bytes = p.blob_bytes.saturating_add(len);
            p.largest_blob_bytes = p.largest_blob_bytes.max(len);
        }
    }
    Ok(MessageCounts {
        dated_ns: sortkey::decode_dated(&key),
        flagged,
        earlier,
        text_bytes,
    })
}

/// The first and last dated sort key of `scope`'s timeline rows, by two
/// index seeks each (`None` for given messages: the walk tracks those).
fn span_seeks(
    s: &StmtCache<'_>,
    scope: &ExportScopeIds,
) -> rusqlite::Result<Option<(Option<i64>, Option<i64>)>> {
    let seek = |sql: &'static str,
                params: &[&dyn rusqlite::ToSql]|
     -> rusqlite::Result<Option<i64>> {
        let key: Option<Vec<u8>> = s.prepare(sql)?.query_row(params, |r| r.get(0)).optional()?;
        Ok(key.as_deref().and_then(sortkey::decode_dated))
    };
    Ok(match scope {
        ExportScopeIds::Conversation { chat, range } => {
            let (from, until) = range.dated_bounds();
            let c = chat.get();
            Some((
                seek(SQL_SPAN_CHAT_FIRST, &[&c, &from, &until])?,
                seek(SQL_SPAN_CHAT_LAST, &[&c, &from, &until])?,
            ))
        }
        ExportScopeIds::Archive { range } => {
            let (from, until) = range.dated_bounds();
            Some((
                seek(SQL_SPAN_ALL_FIRST, &[&from, &until])?,
                seek(SQL_SPAN_ALL_LAST, &[&from, &until])?,
            ))
        }
        ExportScopeIds::Messages(_) => None,
    })
}

/// Unix nanoseconds to milliseconds, rounded down or up.
fn ms(ns: i64, up: bool) -> i64 {
    let (q, r) = (ns.div_euclid(1_000_000), ns.rem_euclid(1_000_000));
    if up && r > 0 {
        q.saturating_add(1)
    } else {
        q
    }
}

/// The exact counts of `scope` under `opts` (see the module comment).
pub(crate) fn precount(
    snap: &ExportSnapshot<'_>,
    scope: &ExportScopeIds,
    opts: PreviewOptions,
    progress: &mut dyn FnMut(PreviewProgress),
) -> Result<ExportPreview, ArchiveError> {
    if opts.leave_out_events {
        snap.leave_out_events();
    }
    let generation = snap.generation()?;
    let seeks = snap.run(|| span_seeks(&snap.stmts, scope))?;
    let undated_excluded = match scope {
        ExportScopeIds::Conversation {
            chat,
            range: TimeRange::Dated { .. },
        } => snap.run(|| {
            snap.stmts
                .prepare(SQL_UNDATED_CHAT)?
                .query_row([chat.get()], |r| r.get::<_, i64>(0))
        })?,
        ExportScopeIds::Archive {
            range: TimeRange::Dated { .. },
        } => snap.run(|| {
            snap.stmts
                .prepare(SQL_UNDATED_ALL)?
                .query_row([], |r| r.get::<_, i64>(0))
        })?,
        _ => 0,
    };
    let mut t = Tally {
        preview: ExportPreview {
            conversations: 0,
            timeline_rows: 0,
            distinct_messages: 0,
            folded_reaction_rows: 0,
            undated_excluded: u64_of(undated_excluded),
            unassigned: 0,
            attachments: 0,
            attachments_with_file: 0,
            attachments_withheld_unsent: 0,
            distinct_blobs: 0,
            blob_bytes: 0,
            largest_blob_bytes: 0,
            messages_with_flagged_code: 0,
            messages_with_earlier_content: 0,
            text_bytes: 0,
            image_previews: 0,
            events_left_out: 0,
            span_utc_ms: None,
            generation,
        },
        seen: HashSet::new(),
        blobs: HashSet::new(),
        chats: HashSet::new(),
        first_ns: None,
        last_ns: None,
        track_span: seeks.is_none(),
    };
    let skipped = walk(snap, scope, &mut |row: ScopeRow| {
        let p = &mut t.preview;
        p.timeline_rows += 1;
        if p.timeline_rows.is_multiple_of(PROGRESS_EVERY) {
            progress(PreviewProgress {
                rows: p.timeline_rows,
            });
        }
        if row.chat.is_some() {
            t.chats.insert(row.chat);
        }
        if row.folded_into.is_some() {
            // Counted as a row here; as a message below, once, like any
            // item (the pipeline writes it whole).
            p.folded_reaction_rows += 1;
        } else if row.chat.is_none() {
            p.unassigned += 1;
        }
        if !t.seen.insert(row.id) {
            return Ok(());
        }
        t.preview.distinct_messages += 1;
        let m = snap.run(|| count_message(&snap.stmts, row.id, opts, &mut t))?;
        let p = &mut t.preview;
        p.messages_with_flagged_code += u64::from(m.flagged);
        p.messages_with_earlier_content += u64::from(m.earlier);
        p.text_bytes = p.text_bytes.saturating_add(m.text_bytes);
        if t.track_span {
            if let Some(ns) = m.dated_ns {
                t.span_ns(ns);
            }
        }
        Ok(())
    })?;
    t.preview.events_left_out = skipped;
    if let Some((first, last)) = seeks {
        t.first_ns = first;
        t.last_ns = last;
    }
    t.preview.conversations = u64::try_from(t.chats.len()).unwrap_or(u64::MAX);
    t.preview.span_utc_ms = match (t.first_ns, t.last_ns) {
        (Some(a), Some(z)) => Some((ms(a, false), ms(z, true))),
        _ => None,
    };
    progress(PreviewProgress {
        rows: t.preview.timeline_rows,
    });
    Ok(t.preview)
}

#[cfg(test)]
mod tests {
    use crate::schema::codes::SessionState;

    #[test]
    fn milliseconds_round_outwards() {
        assert_eq!(super::ms(1_999_999, false), 1);
        assert_eq!(super::ms(1_999_999, true), 2);
        assert_eq!(super::ms(2_000_000, true), 2);
        assert_eq!(super::ms(-1, false), -1);
        assert_eq!(super::ms(-1, true), 0);
    }

    #[test]
    fn the_completed_state_is_the_code_the_statement_names() {
        assert_eq!(SessionState::Completed.code(), 1);
        assert!(super::SQL_GENERATION.contains("state = 1"));
    }
}
