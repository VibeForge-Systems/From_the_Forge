//! Slice 6b2 of M6 (spec §3.6, §3.7, §3.8, §14.1): previews, search
//! scopes and the export's masking.
//!
//! - **Exact previews.** `export_preview` and the job's `precount` agree,
//!   and both match an oracle that reads the archive with SQL of its own
//!   (rows from the cursors that slice 6b1 tested against the viewer):
//!   timeline rows, folded rows, distinct messages, messages in no
//!   conversation, undated rows a dated range leaves out, attachment
//!   records, copied files, distinct blobs and bytes, the two reveal
//!   counts, the shown text bytes and the dated span from the dates
//!   themselves. A preview is cancellable and reports progress; the scope
//!   index numbers every timeline row once, without gaps.
//! - **Masking (owner Q4).** Attachments of unsent parts lose their file
//!   unless earlier content is revealed (all of them when every part is
//!   unsent); the alternative text is withheld with earlier content and,
//!   separately, with a flagged code; the preview counts both.
//! - **Search scopes.** A hit matched only in a masked code or only in
//!   withheld earlier text is excluded by default and kept with the
//!   matching reveal, in both modes (matching does cover code text); kept
//!   hits are grouped by conversation in timeline order; the "all
//!   matches" re-run equals the viewer's search under its cap and says
//!   `capped` honestly over a smaller one.
//! - **The masking oracle, archive side** (`no_masked_text_reaches_any_
//!   export_read_unless_revealed`): over the viewer catalogue and built
//!   cases, no masked or withheld sentinel appears in anything an export
//!   reads with no reveal, each appears with its own reveal, and a
//!   withheld attachment's file is named nowhere. The export catalogue
//!   (`tests/fixtures/export`, slice 6c) adds its sentinels by kind:
//!   codes, earlier text, decoded edit text, subject history, the
//!   alternative text and the files of unsent parts, plus strings that
//!   must stay visible.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::collections::{BTreeMap, BTreeSet, HashSet};

use common::bulk::{generate, BulkSpec};
use common::viewer::{
    export_docs, export_root, import_order, import_order_in, model_docs, Imported,
};
use common::*;
use oma_archive::{
    AltTextSource, AltTextWithheld, AlternativeText, ArchiveError, ArchiveId, ArchiveReader,
    AttachmentAccess, BlobId, BodyView, CancelToken, ChatId, EarlierText, EditEntryText,
    ExportDepth, ExportMessage, ExportPreview, ExportScopeIds, ExportSnapshot, Library, MessageId,
    OptContent, PreviewOptions, Reveal, ScopeGroup, SearchMode, SearchRequest, SegmentKind,
    TextPiece, TimeRange, ValueHistoryEntry, VersionText, WithheldReason, EXPORT_SEARCH_CAP,
};
use serde_json::Value;

const REVEAL_ALL: Reveal = Reveal {
    earlier_text: true,
    one_time_code: true,
};
const CODES: Reveal = Reveal {
    earlier_text: false,
    one_time_code: true,
};
const EARLIER: Reveal = Reveal {
    earlier_text: true,
    one_time_code: false,
};
/// `1 << ONE_TIME_CODE_BIT` (`view/conversations.rs`).
const CODE_FLAG: i64 = 16;

type Snap<'a, 's> = &'a ExportSnapshot<'s>;

fn snapshot<T>(
    r: &ArchiveReader,
    lib: &Library,
    id: &ArchiveId,
    f: impl FnOnce(Snap<'_, '_>) -> Result<T, ArchiveError>,
) -> T {
    let hold = lib.hold_for_export(id).expect("hold");
    r.with_export_snapshot(&hold, &CancelToken::new(), f)
        .expect("snapshot")
}

fn ints(c: &rusqlite::Connection, sql: &str, p: &[&dyn rusqlite::ToSql]) -> Vec<i64> {
    c.prepare(sql)
        .expect(sql)
        .query_map(p, |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

fn one(c: &rusqlite::Connection, sql: &str, p: &[&dyn rusqlite::ToSql]) -> i64 {
    c.query_row(sql, p, |r| r.get(0)).expect(sql)
}

fn opt(c: &rusqlite::Connection, sql: &str, p: &[&dyn rusqlite::ToSql]) -> Option<i64> {
    c.query_row(sql, p, |r| r.get(0)).expect(sql)
}

fn bulk(n: usize) -> (tempfile::TempDir, Library, ArchiveId) {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        let mut spec = BulkSpec::new(n);
        spec.chats = 6;
        spec.reaction_permille = 120;
        spec.attachment_permille = 150;
        spec.undated_permille = 20;
        generate(&mut a, &spec);
    }
    (tmp, lib, id)
}

/// Give every third attachment a stored file (a `blob` row; no store file
/// is needed for counts), two attachments sharing one blob.
fn add_blobs(c: &rusqlite::Connection) {
    let atts = ints(c, "SELECT id FROM attachment ORDER BY id", &[]);
    let mut blob = 0i64;
    for (i, a) in atts.iter().enumerate() {
        if i % 3 != 0 {
            continue;
        }
        // Every other one shares the previous blob.
        if i % 6 != 0 || blob == 0 {
            blob += 1;
            c.execute(
                "INSERT INTO blob (id, sha256, byte_len, storage_name, first_session_id) \
                 VALUES (?1, ?2, ?3, lower(hex(?2)), 1)",
                rusqlite::params![blob, sha(blob), 1000 + 37 * blob],
            )
            .expect("blob");
        }
        c.execute(
            "UPDATE attachment SET current_blob_id = ?1 WHERE id = ?2",
            [blob, *a],
        )
        .expect("link blob");
    }
}

fn sha(n: i64) -> Vec<u8> {
    use sha2::Digest;
    sha2::Sha256::digest(format!("synthetic blob {n}").as_bytes()).to_vec()
}

/// Every timeline row of `scope` through the cursors: (chat, id, folded).
fn rows_of(s: Snap<'_, '_>, scope: &ExportScopeIds) -> Vec<(Option<i64>, i64, bool)> {
    let mut out = Vec::new();
    let mut drain = |chat: Option<ChatId>, range: TimeRange| {
        let mut cur = match chat {
            Some(c) => s.timeline(c, range),
            None => s.unassigned(range),
        };
        while let Some(b) = cur.next_batch().expect("batch") {
            // Rebuild the timeline order of items and folded rows.
            let folded: BTreeSet<i64> = b.folded.iter().map(|f| f.0.get()).collect();
            for m in &b.items {
                out.push((chat.map(ChatId::get), m.get(), false));
            }
            for f in folded {
                out.push((chat.map(ChatId::get), f, true));
            }
        }
    };
    match scope {
        ExportScopeIds::Conversation { chat, range } => drain(Some(*chat), *range),
        ExportScopeIds::Archive { range } => {
            for chat in s.chat_ids().expect("chats") {
                drain(Some(chat), *range);
            }
            drain(None, *range);
        }
        ExportScopeIds::Messages(groups) => {
            for g in groups {
                for m in &g.messages {
                    out.push((g.chat.map(ChatId::get), m.get(), false));
                }
            }
        }
        _ => unreachable!("a new scope kind"),
    }
    out
}

/// The oracle's counts of `scope` with no reveal at depth `Items`, read
/// with the test's own SQL over the cursors' rows.
fn oracle(
    c: &rusqlite::Connection,
    rows: &[(Option<i64>, i64, bool)],
    scope: &ExportScopeIds,
) -> BTreeMap<&'static str, i64> {
    let mut o = BTreeMap::new();
    let items: Vec<i64> = rows.iter().filter(|r| !r.2).map(|r| r.1).collect();
    let distinct: BTreeSet<i64> = items.iter().copied().collect();
    o.insert("timeline_rows", rows.len() as i64);
    o.insert("folded", rows.iter().filter(|r| r.2).count() as i64);
    o.insert("distinct", distinct.len() as i64);
    o.insert(
        "unassigned",
        rows.iter().filter(|r| r.0.is_none() && !r.2).count() as i64,
    );
    o.insert(
        "conversations",
        rows.iter()
            .filter_map(|r| r.0)
            .collect::<BTreeSet<_>>()
            .len() as i64,
    );
    let (mut atts, mut with_file, mut images, mut text, mut flagged, mut earlier) =
        (0, 0, 0, 0, 0, 0);
    let mut blobs: BTreeMap<i64, i64> = BTreeMap::new();
    for m in &distinct {
        atts += one(
            c,
            "SELECT count(*) FROM message_attachment WHERE message_id = ?1",
            &[m],
        );
        for (blob, len, image) in c
            .prepare(
                "SELECT a.current_blob_id, b.byte_len, a.mime_type LIKE 'image/%' \
                 FROM message_attachment ma JOIN attachment a ON a.id = ma.attachment_id \
                 JOIN blob b ON b.id = a.current_blob_id WHERE ma.message_id = ?1",
            )
            .expect("prepare")
            .query_map([m], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            })
            .expect("query")
            .map(|r| r.expect("row"))
        {
            with_file += 1;
            images += i64::from(image);
            blobs.insert(blob, len);
        }
        text += one(
            c,
            "SELECT coalesce(octet_length(b.display_text), 0) FROM message m \
             LEFT JOIN message_body b ON b.id = m.current_body_id WHERE m.id = ?1",
            &[m],
        );
        flagged += one(
            c,
            "SELECT EXISTS (SELECT 1 FROM message_body b JOIN message_body_part p \
               ON p.body_id = b.id WHERE b.message_id = ?1 AND (p.flags & ?2) <> 0)",
            &[m, &CODE_FLAG],
        );
        earlier += one(
            c,
            "SELECT EXISTS (SELECT 1 FROM message m JOIN message_unsent_part u \
               ON u.body_id = m.current_body_id WHERE m.id = ?1) \
             OR ((SELECT b.display_text IS NULL FROM message m LEFT JOIN message_body b \
                    ON b.id = m.current_body_id WHERE m.id = ?1) \
                 AND (EXISTS (SELECT 1 FROM message_body b WHERE b.message_id = ?1 \
                        AND b.display_text IS NOT NULL) \
                   OR EXISTS (SELECT 1 FROM message_body b JOIN message_edit_entry e \
                        ON e.body_id = b.id WHERE b.message_id = ?1 AND e.body_present = 1)))",
            &[m],
        );
    }
    o.insert("attachments", atts);
    o.insert("with_file", with_file);
    o.insert("images", images);
    o.insert("blobs", blobs.len() as i64);
    o.insert("blob_bytes", blobs.values().sum());
    o.insert("largest", blobs.values().copied().max().unwrap_or(0));
    o.insert("text_bytes", text);
    o.insert("flagged", flagged);
    o.insert("earlier", earlier);
    let undated = match scope {
        ExportScopeIds::Conversation {
            chat,
            range: TimeRange::Dated { .. },
        } => one(
            c,
            "SELECT count(*) FROM chat_message WHERE chat_id = ?1 AND sort_key >= X'01'",
            &[&chat.get()],
        ),
        ExportScopeIds::Archive {
            range: TimeRange::Dated { .. },
        } => one(
            c,
            "SELECT count(*) FROM message WHERE sort_key >= X'01'",
            &[],
        ),
        _ => 0,
    };
    o.insert("undated", undated);
    // The dated span from the dates themselves, over every row.
    let dates: Vec<i64> = rows
        .iter()
        .filter_map(|r| {
            opt(
                c,
                "SELECT max(date_utc_ns) FROM message WHERE id = ?1 AND sort_key < X'01'",
                &[&r.1],
            )
        })
        .collect();
    if let (Some(a), Some(z)) = (dates.iter().min(), dates.iter().max()) {
        o.insert("span_first_ms", a.div_euclid(1_000_000));
        o.insert(
            "span_last_ms",
            z.div_euclid(1_000_000) + i64::from(z.rem_euclid(1_000_000) > 0),
        );
    }
    o
}

fn counts(p: &ExportPreview) -> BTreeMap<&'static str, i64> {
    let mut o = BTreeMap::new();
    let i = |v: u64| i64::try_from(v).expect("small");
    o.insert("timeline_rows", i(p.timeline_rows));
    o.insert("folded", i(p.folded_reaction_rows));
    o.insert("distinct", i(p.distinct_messages));
    o.insert("unassigned", i(p.unassigned));
    o.insert("conversations", i(p.conversations));
    o.insert("attachments", i(p.attachments));
    o.insert("with_file", i(p.attachments_with_file));
    o.insert("images", i(p.image_previews));
    o.insert("blobs", i(p.distinct_blobs));
    o.insert("blob_bytes", i(p.blob_bytes));
    o.insert("largest", i(p.largest_blob_bytes));
    o.insert("text_bytes", i(p.text_bytes));
    o.insert("flagged", i(p.messages_with_flagged_code));
    o.insert("earlier", i(p.messages_with_earlier_content));
    o.insert("undated", i(p.undated_excluded));
    if let Some((a, z)) = p.span_utc_ms {
        o.insert("span_first_ms", a);
        o.insert("span_last_ms", z);
    }
    o
}

// ---------------------------------------------------------------------------
// Previews
// ---------------------------------------------------------------------------

#[test]
fn previews_are_exact_and_equal_the_job_precount() {
    let (tmp, lib, id) = bulk(3_000);
    let c = raw(&db_path(&tmp, &id));
    add_blobs(&c);
    // Messages in no conversation: detach a few.
    let detached = ints(
        &c,
        "SELECT message_id FROM chat_message WHERE chat_id = 2 ORDER BY message_id LIMIT 7",
        &[],
    );
    for m in &detached {
        c.execute("DELETE FROM chat_message WHERE message_id = ?1", [m])
            .expect("detach");
    }
    // A message in two conversations.
    let shared = one(
        &c,
        "SELECT message_id FROM chat_message WHERE chat_id = 3 AND message_id IN \
           (SELECT id FROM message WHERE assoc_present = 0) ORDER BY message_id LIMIT 1",
        &[],
    );
    c.execute(
        "INSERT INTO chat_message (chat_id, message_id, sort_key, join_date_raw, join_date_st, \
           join_date_unit, join_date_utc_ns, first_session_id, last_session_id, last_rank) \
         SELECT 4, message_id, sort_key, join_date_raw, join_date_st, join_date_unit, \
           join_date_utc_ns, first_session_id, last_session_id, last_rank \
         FROM chat_message WHERE message_id = ?1",
        [shared],
    )
    .expect("second conversation");
    let r = lib.open_read(&id).expect("reader");
    let (lo, hi): (i64, i64) = c
        .query_row(
            "SELECT min(date_utc_ns), max(date_utc_ns) FROM message",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("dates");
    let mid = lo + (hi - lo) / 3;
    let dated = TimeRange::Dated {
        from_ns: mid,
        until_ns: hi - (hi - lo) / 4,
    };
    let big = ChatId::new(one(
        &c,
        "SELECT chat_id FROM chat_message GROUP BY chat_id ORDER BY count(*) DESC LIMIT 1",
        &[],
    ));
    let some: Vec<MessageId> = ints(&c, "SELECT id FROM message ORDER BY id DESC LIMIT 40", &[])
        .into_iter()
        .map(MessageId::new)
        .collect();
    let scopes = vec![
        ExportScopeIds::Archive {
            range: TimeRange::All,
        },
        ExportScopeIds::Archive { range: dated },
        ExportScopeIds::Conversation {
            chat: big,
            range: TimeRange::All,
        },
        ExportScopeIds::Conversation {
            chat: big,
            range: dated,
        },
        ExportScopeIds::Conversation {
            chat: ChatId::new(4),
            range: TimeRange::All,
        },
        ExportScopeIds::Messages(vec![
            ScopeGroup::new(Some(big), some[..20].to_vec()),
            ScopeGroup::new(None, some[20..].to_vec()),
        ]),
    ];
    let opts = PreviewOptions::new(Reveal::default(), ExportDepth::Items);
    for scope in &scopes {
        let mut reported = Vec::new();
        let p = r
            .export_preview(scope, opts, &CancelToken::new(), &mut |p| {
                reported.push(p.rows)
            })
            .expect("preview");
        assert!(reported.windows(2).all(|w| w[0] <= w[1]), "{scope:?}");
        assert_eq!(reported.last().copied(), Some(p.timeline_rows), "{scope:?}");
        let (pre, rows, index) = snapshot(&r, &lib, &id, |s| {
            let pre = s.precount(scope, opts, &mut |_| {})?;
            Ok((pre, rows_of(s, scope), s.scope_index(scope)?))
        });
        assert_eq!(pre, p, "{scope:?}: the pre-count equals the preview");
        assert_eq!(counts(&p), oracle(&c, &rows, scope), "{scope:?}");
        // The index numbers every row once, from 1, without gaps.
        let ids: BTreeSet<i64> = rows.iter().map(|r| r.1).collect();
        assert_eq!(index.len(), ids.len());
        for (n, m) in ids.iter().enumerate() {
            assert_eq!(index.ordinal(MessageId::new(*m)), Some(n as u64 + 1));
        }
        assert_eq!(p.generation, r.archive_generation().expect("generation"));
    }
    // The archive-wide scope saw the detached messages, the shared one
    // once, and a dated range left undated rows out.
    let all = r
        .export_preview(&scopes[0], opts, &CancelToken::new(), &mut |_| {})
        .expect("preview");
    assert_eq!(all.unassigned, detached.len() as u64);
    assert_eq!(
        all.distinct_messages + all.folded_reaction_rows + 1,
        all.timeline_rows
    );
    assert!(all.distinct_blobs > 0 && all.distinct_blobs < all.attachments_with_file);
    let ranged = r
        .export_preview(&scopes[1], opts, &CancelToken::new(), &mut |_| {})
        .expect("preview");
    assert!(ranged.undated_excluded > 0);
    assert!(ranged.timeline_rows < all.timeline_rows);
}

#[test]
fn a_preview_stops_on_cancel_and_counts_codes_and_earlier_content() {
    let (tmp, lib, id) = bulk(1_500);
    let c = raw(&db_path(&tmp, &id));
    let r = lib.open_read(&id).expect("reader");
    let scope = ExportScopeIds::Archive {
        range: TimeRange::All,
    };
    let opts = PreviewOptions::default();
    let cancelled = CancelToken::new();
    cancelled.cancel();
    assert!(matches!(
        r.export_preview(&scope, opts, &cancelled, &mut |_| {}),
        Err(ArchiveError::Cancelled)
    ));
    let token = CancelToken::new();
    let t2 = token.clone();
    let out = r.export_preview(&scope, opts, &token, &mut |_| t2.cancel());
    assert!(matches!(out, Err(ArchiveError::Cancelled)), "{out:?}");
    r.info().expect("the reader reads again");
    // A code: 6 UTF-8 bytes of a 20-byte body flagged.
    let (m, body) = text_message(&c, 0);
    c.execute(
        "UPDATE message_body SET display_text = 'abc 123456 defghijkl', display_source = 1 \
         WHERE id = ?1",
        [body],
    )
    .expect("text");
    part(&c, body, 0, Some(0), (4, 10), CODE_FLAG, None);
    // Earlier content: an unsent part.
    let (m2, body2) = text_message(&c, 1);
    unsent(&c, body2, 0);
    let before = r
        .export_preview(&scope, opts, &CancelToken::new(), &mut |_| {})
        .expect("preview");
    assert_eq!(before.messages_with_flagged_code, 1);
    assert_eq!(before.messages_with_earlier_content, 1);
    let shown = r
        .export_preview(
            &scope,
            PreviewOptions::new(CODES, ExportDepth::Items),
            &CancelToken::new(),
            &mut |_| {},
        )
        .expect("preview");
    assert_eq!(shown.text_bytes, before.text_bytes + 6, "the code's bytes");
    // Untrusted ranges withhold the whole body.
    c.execute(
        "UPDATE message_body SET display_source = 2 WHERE id = ?1",
        [body],
    )
    .expect("untrusted");
    let whole = r
        .export_preview(&scope, opts, &CancelToken::new(), &mut |_| {})
        .expect("preview");
    assert_eq!(whole.text_bytes + 20, shown.text_bytes);
    let _ = (m, m2);
}

// ---------------------------------------------------------------------------
// Built cases
// ---------------------------------------------------------------------------

/// The `n`-th plain message with a current body: (message, body).
fn text_message(c: &rusqlite::Connection, n: i64) -> (i64, i64) {
    c.query_row(
        "SELECT m.id, m.current_body_id FROM message m WHERE m.assoc_present = 0 \
           AND m.current_body_id IS NOT NULL \
           AND NOT EXISTS (SELECT 1 FROM message_attachment WHERE message_id = m.id) \
         ORDER BY m.id LIMIT 1 OFFSET ?1",
        [n],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .expect("a text message")
}

/// A run of `body`: ordinal, part index, UTF-16 range (also used as the
/// UTF-8 range: the test texts are ASCII), flags and attachment GUID.
fn part(
    c: &rusqlite::Connection,
    body: i64,
    ordinal: i64,
    part_index: Option<i64>,
    range: (i64, i64),
    flags: i64,
    guid: Option<&[u8]>,
) {
    c.execute(
        "INSERT INTO message_body_part (body_id, ordinal, part_index, utf16_start, utf16_end, \
           utf8_start, utf8_end, attachment_guid, mention, flags, keys_present, text_effect) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?4, ?5, ?6, NULL, ?7, 0, NULL)",
        rusqlite::params![body, ordinal, part_index, range.0, range.1, guid, flags],
    )
    .expect("part");
}

fn unsent(c: &rusqlite::Connection, body: i64, part: i64) {
    c.execute(
        "INSERT INTO message_unsent_part (body_id, ordinal, part) \
         VALUES (?1, (SELECT count(*) FROM message_unsent_part WHERE body_id = ?1), ?2)",
        [body, part],
    )
    .expect("unsent part");
}

/// A copy of `table`'s row `src` with a new id (the next free one) and
/// `overrides` bound as parameters; the new id.
fn copy_row(
    c: &rusqlite::Connection,
    table: &str,
    src: i64,
    overrides: &[(&str, &dyn rusqlite::ToSql)],
) -> i64 {
    let cols: Vec<String> = c
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("pragma")
        .query_map([], |r| r.get::<_, String>(1))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("columns");
    let new = one(
        c,
        &format!("SELECT coalesce(max(id), 0) + 1 FROM {table}"),
        &[],
    );
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&new, &src];
    let select: Vec<String> = cols
        .iter()
        .map(|col| {
            if col == "id" {
                return "?1".to_owned();
            }
            match overrides.iter().find(|(k, _)| k == col) {
                Some((_, v)) => {
                    params.push(*v);
                    format!("?{}", params.len())
                }
                None => col.clone(),
            }
        })
        .collect();
    c.execute(
        &format!(
            "INSERT INTO {table} ({}) SELECT {} FROM {table} WHERE id = ?2",
            cols.join(", "),
            select.join(", ")
        ),
        params.as_slice(),
    )
    .expect("copy row");
    new
}

/// The rank just below `rank` (bytewise).
fn before(mut rank: Vec<u8>) -> Vec<u8> {
    for b in rank.iter_mut().rev() {
        if *b > 0 {
            *b -= 1;
            break;
        }
        *b = 0xFF;
    }
    rank
}

/// An earlier body version of `m` with `text`, ranked before `current`.
fn earlier_version(c: &rusqlite::Connection, m: i64, current: i64, text: &str) -> i64 {
    let rank: Vec<u8> = c
        .query_row(
            "SELECT first_rank FROM message_body WHERE id = ?1",
            [current],
            |r| r.get(0),
        )
        .expect("rank");
    let hash = sha(-current).to_vec();
    let bytes = text.as_bytes().to_vec();
    let rank = before(rank);
    let source = 2i64;
    copy_row(
        c,
        "message_body",
        current,
        &[
            ("message_id", &m),
            ("raw_hash", &hash),
            ("text_raw", &bytes),
            ("display_text", &text),
            ("display_source", &source),
            ("first_rank", &rank),
        ],
    )
}

/// A message with three attachments, each with a blob, placed in parts 0,
/// 1 and 2 of its current body.
fn three_attachments(c: &rusqlite::Connection) -> (i64, i64, Vec<(i64, i64)>) {
    let (m, body) = text_message(c, 3);
    c.execute(
        "UPDATE message_body SET display_text = 'one two three', display_source = 1 \
         WHERE id = ?1",
        [body],
    )
    .expect("text");
    let mut out = Vec::new();
    let template = one(c, "SELECT min(id) FROM attachment", &[]);
    for i in 0..3i64 {
        let blob = one(c, "SELECT coalesce(max(id), 0) + 1 FROM blob", &[]);
        c.execute(
            "INSERT INTO blob (id, sha256, byte_len, storage_name, first_session_id) \
             VALUES (?1, ?2, ?3, lower(hex(?2)), 1)",
            rusqlite::params![blob, sha(1000 + blob), 5000 + i],
        )
        .expect("blob");
        let n = one(c, "SELECT max(id) + 1 FROM attachment", &[]);
        let guid = format!("SYNTH-UNSENT-ATT-{n}").into_bytes();
        let name = format!("SYNTH-UNSENT-FILE-{n}.png").into_bytes();
        let mime = b"image/png".to_vec();
        let att = copy_row(
            c,
            "attachment",
            template,
            &[
                ("guid", &guid),
                ("transfer_name", &name),
                ("mime_type", &mime),
                ("current_blob_id", &blob),
            ],
        );
        c.execute(
            "INSERT INTO message_attachment (message_id, attachment_id, ord_rank, ord_rowid, \
               first_session_id, last_session_id, last_rank) \
             SELECT ?1, ?2, first_rank, ?2, 1, 1, first_rank FROM message WHERE id = ?1",
            [m, att],
        )
        .expect("link");
        part(c, body, i, Some(i), (i, i + 1), 0, Some(&guid));
        out.push((att, blob));
    }
    (m, body, out)
}

fn assemble_one(
    r: &ArchiveReader,
    lib: &Library,
    id: &ArchiveId,
    m: i64,
    reveal: Reveal,
) -> (ExportMessage, Vec<oma_archive::ExportAttachment>) {
    snapshot(r, lib, id, |s| {
        let x = s
            .assemble(&[MessageId::new(m)], reveal, ExportDepth::Items)?
            .remove(0);
        let facts = s.attachment_facts(&x)?;
        Ok((x, facts))
    })
}

#[test]
fn attachments_of_unsent_parts_lose_their_file_unless_revealed() {
    let (tmp, lib, id) = bulk(400);
    let c = raw(&db_path(&tmp, &id));
    let (m, body, atts) = three_attachments(&c);
    let r = lib.open_read(&id).expect("reader");
    let scope = ExportScopeIds::Messages(vec![ScopeGroup::new(None, vec![MessageId::new(m)])]);
    let preview = |reveal: Reveal| {
        r.export_preview(
            &scope,
            PreviewOptions::new(reveal, ExportDepth::Items),
            &CancelToken::new(),
            &mut |_| {},
        )
        .expect("preview")
    };
    // No unsent part: every file is the export's.
    let (x, _) = assemble_one(&r, &lib, &id, m, Reveal::default());
    assert_eq!(x.attachment_access, vec![AttachmentAccess::Included; 3]);
    assert_eq!(preview(Reveal::default()).attachments_with_file, 3);
    // Part 1 unsent: only its attachment is withheld.
    unsent(&c, body, 1);
    let (x, facts) = assemble_one(&r, &lib, &id, m, Reveal::default());
    let order: Vec<i64> = x
        .item
        .message
        .attachments
        .iter()
        .map(|a| a.id.get())
        .collect();
    let withheld = order.iter().position(|a| *a == atts[1].0).expect("listed");
    for (i, a) in x.item.message.attachments.iter().enumerate() {
        if i == withheld {
            assert_eq!(
                x.attachment_access[i],
                AttachmentAccess::Withheld(WithheldReason::Unsent)
            );
            assert_eq!((a.blob, a.stored_bytes), (None, None));
            assert_eq!(x.item.codes.attachments[i].stored_bytes, None);
            assert!(facts[i].facts.blob.is_none());
            assert!(facts[i].access.is_withheld());
            // The declared facts stay.
            assert!(a.name.text().is_some());
        } else {
            assert_eq!(x.attachment_access[i], AttachmentAccess::Included);
            assert!(a.blob.is_some() && facts[i].facts.blob.is_some());
        }
    }
    let p = preview(Reveal::default());
    assert_eq!(
        (
            p.attachments,
            p.attachments_with_file,
            p.attachments_withheld_unsent
        ),
        (3, 2, 1)
    );
    assert_eq!(p.distinct_blobs, 2);
    assert_eq!(p.messages_with_earlier_content, 1);
    // Revealed: the file is back, marked.
    let (x, facts) = assemble_one(&r, &lib, &id, m, EARLIER);
    assert_eq!(
        x.attachment_access[withheld],
        AttachmentAccess::Revealed(WithheldReason::Unsent)
    );
    assert!(x.item.message.attachments[withheld].blob.is_some());
    assert!(facts[withheld].facts.blob.is_some());
    assert_eq!(preview(EARLIER).attachments_with_file, 3);
    // Every part unsent: every file withheld.
    unsent(&c, body, 0);
    unsent(&c, body, 2);
    let (x, _) = assemble_one(&r, &lib, &id, m, Reveal::default());
    assert!(x.attachment_access.iter().all(|a| a.is_withheld()));
    assert!(x.item.message.attachments.iter().all(|a| a.blob.is_none()));
    assert_eq!(preview(Reveal::default()).attachments_with_file, 0);
    // A part whose index is not recorded cannot keep a file either.
    c.execute(
        "DELETE FROM message_unsent_part WHERE body_id = ?1 AND part <> 1",
        [body],
    )
    .expect("one unsent again");
    c.execute(
        "UPDATE message_body_part SET part_index = NULL WHERE body_id = ?1 AND ordinal = 0",
        [body],
    )
    .expect("no index");
    let (x, _) = assemble_one(&r, &lib, &id, m, Reveal::default());
    let att0 = order.iter().position(|a| *a == atts[0].0).expect("listed");
    assert!(x.attachment_access[att0].is_withheld());
}

#[test]
fn the_alternative_text_is_withheld_with_earlier_content_and_with_codes() {
    let (tmp, lib, id) = bulk(400);
    let c = raw(&db_path(&tmp, &id));
    let (m, body) = text_message(&c, 2);
    let r = lib.open_read(&id).expect("reader");
    let alt = |reveal: Reveal| assemble_one(&r, &lib, &id, m, reveal).0.alternative_text;
    // The bulk bodies agree with their text column.
    assert_eq!(alt(Reveal::default()), AlternativeText::None);
    c.execute(
        "UPDATE message_body SET display_text = 'shown text', display_source = 1, \
           sources_differ = 1, text_raw = CAST('SYNTH-ALT-PLAIN-COLUMN' AS BLOB) \
         WHERE id = ?1",
        [body],
    )
    .expect("differ");
    let text = |a: &AlternativeText| match a {
        AlternativeText::Text { text, source } => {
            assert_eq!(*source, AltTextSource::TextColumn);
            text.text.expose().clone()
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(text(&alt(Reveal::default())), "SYNTH-ALT-PLAIN-COLUMN");
    // A flagged code withholds it whole unless codes are revealed.
    part(&c, body, 0, Some(0), (0, 5), CODE_FLAG, None);
    assert_eq!(
        alt(Reveal::default()),
        AlternativeText::Withheld(AltTextWithheld::OneTimeCode)
    );
    assert_eq!(alt(EARLIER), alt(Reveal::default()));
    assert_eq!(text(&alt(CODES)), "SYNTH-ALT-PLAIN-COLUMN");
    // An unsent part withholds it unless earlier content is revealed.
    unsent(&c, body, 0);
    assert_eq!(
        alt(CODES),
        AlternativeText::Withheld(AltTextWithheld::EarlierContent(WithheldReason::Unsent))
    );
    assert_eq!(
        alt(EARLIER),
        AlternativeText::Withheld(AltTextWithheld::OneTimeCode)
    );
    assert_eq!(text(&alt(REVEAL_ALL)), "SYNTH-ALT-PLAIN-COLUMN");
    // Shown from the text column: the decoded attributedBody was empty.
    c.execute_batch(&format!(
        "DELETE FROM message_unsent_part WHERE body_id = {body};
         DELETE FROM message_body_part WHERE body_id = {body};
         UPDATE message_body SET display_source = 2 WHERE id = {body};"
    ))
    .expect("text column");
    match alt(Reveal::default()) {
        AlternativeText::Text { text, source } => {
            assert_eq!(source, AltTextSource::DecodedAttributedBody);
            assert!(text.text.expose().is_empty());
        }
        other => panic!("{other:?}"),
    }
    // Not valid UTF-8: decoded lossily and marked.
    c.execute(
        "UPDATE message_body SET display_source = 1, text_raw = X'41FF42' WHERE id = ?1",
        [body],
    )
    .expect("lossy");
    match alt(Reveal::default()) {
        AlternativeText::Text { text, .. } => {
            assert!(text.lossy);
            assert_eq!(text.text.expose(), "A\u{FFFD}B");
        }
        other => panic!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Search scopes
// ---------------------------------------------------------------------------

fn kept_ids(sc: &oma_archive::SearchScope) -> Vec<i64> {
    sc.groups
        .iter()
        .flat_map(|g| g.messages.iter().map(|m| m.get()))
        .collect()
}

#[test]
fn hits_only_in_hidden_content_are_excluded_unless_revealed() {
    let (tmp, lib, id) = bulk(600);
    let c = raw(&db_path(&tmp, &id));
    // A: the query word only inside a flagged code.
    let (a, a_body) = text_message(&c, 0);
    c.execute(
        "UPDATE message_body SET display_text = 'your code synthcodeword here', \
           display_source = 1 WHERE id = ?1",
        [a_body],
    )
    .expect("code text");
    part(&c, a_body, 0, Some(0), (10, 23), CODE_FLAG, None);
    // B: the word both in a code and in shown text: kept.
    let (b, b_body) = text_message(&c, 1);
    c.execute(
        "UPDATE message_body SET display_text = 'synthcodeword 99 synthcodeword', \
           display_source = 1 WHERE id = ?1",
        [b_body],
    )
    .expect("code text");
    part(&c, b_body, 0, Some(0), (0, 13), CODE_FLAG, None);
    // C: the word only in an earlier version of an unsent message.
    let (cm, c_body) = text_message(&c, 2);
    earlier_version(&c, cm, c_body, "before synthearlyword unsend");
    unsent(&c, c_body, 0);
    // D: the word in an earlier version of an edited message (not
    // withheld, but written only with history).
    let (d, d_body) = text_message(&c, 3);
    earlier_version(&c, d, d_body, "draft synthearlyword edited");
    let r = lib.open_read(&id).expect("reader");
    let v = lib.open_read(&id).expect("viewer");
    let scope_of = |q: &SearchRequest, opts: PreviewOptions| {
        let set = v
            .search(q, &CancelToken::new(), &mut |_| {})
            .expect("search");
        snapshot(&r, &lib, &id, |s| s.search_scope(q, &set, opts))
    };
    for mode in [SearchMode::Auto, SearchMode::Exact] {
        let mut q = SearchRequest::new("synthcodeword");
        q.mode = mode;
        // Matching covers code text: the viewer's search finds A.
        let set = v.search(&q, &CancelToken::new(), &mut |_| {}).expect("set");
        let found: BTreeSet<i64> = set.entries.iter().map(|e| e.message.get()).collect();
        assert_eq!(found, BTreeSet::from([a, b]), "{mode:?}");
        let sc = scope_of(&q, PreviewOptions::default());
        assert_eq!(kept_ids(&sc), vec![b], "{mode:?}");
        assert_eq!(sc.excluded.one_time_codes, 1, "{mode:?}");
        assert_eq!(sc.excluded.total(), 1);
        let sc = scope_of(&q, PreviewOptions::new(CODES, ExportDepth::Items));
        assert_eq!(
            kept_ids(&sc).into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([a, b])
        );
        assert_eq!(sc.excluded.total(), 0);
    }
    let mut q = SearchRequest::new("synthearlyword");
    q.include_earlier = true;
    let sc = scope_of(&q, PreviewOptions::default());
    assert!(kept_ids(&sc).is_empty());
    assert_eq!(sc.excluded.earlier_content, 1, "{sc:?}");
    assert_eq!(sc.excluded.not_written, 1, "D's version is not written");
    let sc = scope_of(&q, PreviewOptions::new(EARLIER, ExportDepth::Items));
    assert_eq!(kept_ids(&sc), vec![cm]);
    let sc = scope_of(
        &q,
        PreviewOptions::new(Reveal::default(), ExportDepth::WithHistory),
    );
    assert_eq!(kept_ids(&sc), vec![d], "history writes D's version");
    assert_eq!(sc.excluded.earlier_content, 1);
    let sc = scope_of(&q, PreviewOptions::new(EARLIER, ExportDepth::WithHistory));
    assert_eq!(
        kept_ids(&sc).into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from([cm, d])
    );
}

#[test]
fn all_matches_reruns_with_the_export_cap_and_groups_by_conversation() {
    let (tmp, lib, id) = bulk(2_000);
    let c = raw(&db_path(&tmp, &id));
    // A few hits in no conversation.
    let loose = ints(
        &c,
        "SELECT message_id FROM chat_message WHERE chat_id = 2 ORDER BY message_id LIMIT 5",
        &[],
    );
    for m in &loose {
        c.execute("DELETE FROM chat_message WHERE message_id = ?1", [m])
            .expect("detach");
    }
    let r = lib.open_read(&id).expect("reader");
    let v = lib.open_read(&id).expect("viewer");
    let word = {
        let text: String = c
            .query_row(
                "SELECT display_text FROM message_body WHERE display_text GLOB '*[a-z]*' \
                 ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .expect("a text");
        text.split_whitespace()
            .find(|w| w.len() >= 4 && w.chars().all(|ch| ch.is_ascii_lowercase()))
            .expect("a word")
            .to_owned()
    };
    for mode in [SearchMode::Auto, SearchMode::Exact] {
        let mut q = SearchRequest::new(&word);
        q.mode = mode;
        let live = v
            .search(&q, &CancelToken::new(), &mut |_| {})
            .expect("live");
        assert!(live.entries.len() > 20 && !live.capped, "{mode:?}");
        let (all, small, sc) = snapshot(&r, &lib, &id, |s| {
            let mut progress = 0;
            let all = s.search_ids(&q, EXPORT_SEARCH_CAP, &mut |_| progress += 1)?;
            if mode == SearchMode::Exact {
                assert!(progress > 0);
            }
            let small = s.search_ids(&q, 10, &mut |_| {})?;
            let sc = s.search_scope(&q, &all, PreviewOptions::default())?;
            Ok((all, small, sc))
        });
        assert_eq!(all.entries, live.entries, "{mode:?}: the same set");
        assert!(!all.capped);
        assert!(small.capped && small.entries.len() == 10, "{mode:?}");
        if mode == SearchMode::Exact {
            assert!(small.newest_kept);
            assert!(!small.coverage.expect("coverage").complete);
            assert!(sc.coverage_complete);
        }
        // Every hit kept (no codes, no withheld text), grouped by the
        // lowest conversation, no conversation last, in timeline order.
        assert_eq!(sc.kept as usize, all.entries.len());
        let mut last_chat: Option<Option<i64>> = None;
        for g in &sc.groups {
            let chat = g.chat.map(ChatId::get);
            if let Some(prev) = last_chat {
                assert!(prev.is_some(), "no conversation is last");
                assert!(chat.is_none() || chat > prev, "conversations in id order");
            }
            last_chat = Some(chat);
            let keys: Vec<(Vec<u8>, i64)> = g
                .messages
                .iter()
                .map(|m| {
                    let lowest = opt(
                        &c,
                        "SELECT min(chat_id) FROM chat_message WHERE message_id = ?1",
                        &[&m.get()],
                    );
                    assert_eq!(lowest, chat, "written under its lowest conversation");
                    let key: Vec<u8> = c
                        .query_row(
                            "SELECT sort_key FROM message WHERE id = ?1",
                            [m.get()],
                            |r| r.get(0),
                        )
                        .expect("key");
                    (key, m.get())
                })
                .collect();
            assert!(keys.windows(2).all(|w| w[0] < w[1]), "timeline order");
        }
        // The shell's search preview (slice 6h) is the same scope and the
        // same counts as the job's steps in a snapshot, with and without
        // the re-run.
        let opts = PreviewOptions::default();
        let (job_scope, job_counts) = snapshot(&r, &lib, &id, |s| {
            let sc = s.search_scope(&q, &small, opts)?;
            let ids = ExportScopeIds::Messages(sc.groups.clone());
            let counts = s.precount(&ids, opts, &mut |_| {})?;
            Ok((sc, counts))
        });
        let (scope, counts) = r
            .export_search_preview(&q, &small, None, opts, &CancelToken::new(), &mut |_| {})
            .expect("search preview");
        assert_eq!((scope, counts), (job_scope, job_counts), "{mode:?}");
        let (rerun, _) = r
            .export_search_preview(
                &q,
                &small,
                Some(EXPORT_SEARCH_CAP),
                opts,
                &CancelToken::new(),
                &mut |_| {},
            )
            .expect("search preview with the re-run");
        assert_eq!(rerun, sc, "{mode:?}: the re-run keeps every match");
    }
    // A cancelled job stops the re-run.
    let hold = lib.hold_for_export(&id).expect("hold");
    let token = CancelToken::new();
    let out = r.with_export_snapshot(&hold, &token, |s| {
        token.cancel();
        let mut q = SearchRequest::new(&word);
        q.mode = SearchMode::Exact;
        s.search_ids(&q, EXPORT_SEARCH_CAP, &mut |_| {})
    });
    assert!(matches!(out, Err(ArchiveError::Cancelled)), "{out:?}");
}

// ---------------------------------------------------------------------------
// The masking oracle, archive side
// ---------------------------------------------------------------------------

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn push_opt(o: &OptContent, out: &mut String) {
    if let OptContent::Text(t) = o {
        out.push_str(t.text.expose());
        out.push('\u{0}');
    }
}

fn push_pieces(p: &[TextPiece], out: &mut String) {
    for piece in p {
        if let TextPiece::Text(t) = piece {
            out.push_str(t.text.expose());
        }
    }
    out.push('\u{0}');
}

/// Everything an export reads for `ids` under `reveal`, as text, and every
/// blob id it can reach.
fn everything(s: Snap<'_, '_>, ids: &[MessageId], reveal: Reveal) -> (String, HashSet<BlobId>) {
    let mut out = String::new();
    let mut blobs = HashSet::new();
    let xs = s
        .assemble(ids, reveal, ExportDepth::WithHistory)
        .expect("assemble");
    for x in &xs {
        let m = &x.item.message;
        if let BodyView::Text(b) = &m.body {
            for seg in &b.segments {
                if let SegmentKind::Text(t) = &seg.kind {
                    out.push_str(t.text.expose());
                }
            }
            out.push('\u{0}');
        }
        if let EarlierText::Revealed { text, .. } = &m.edit.earlier {
            push_pieces(text, &mut out);
        }
        if let Some(e) = &m.event {
            if let Some(t) = &e.title {
                push_opt(t, &mut out);
            }
        }
        if let Some(a) = &m.association {
            if let Some(e) = &a.emoji {
                push_opt(e, &mut out);
            }
        }
        for a in &m.attachments {
            push_opt(&a.name, &mut out);
            push_opt(&a.declared_type, &mut out);
            blobs.extend(a.blob);
        }
        if let AlternativeText::Text { text, .. } = &x.alternative_text {
            out.push_str(text.text.expose());
            out.push('\u{0}');
        }
        push_opt(&x.detail.subject, &mut out);
        push_opt(&x.detail.account, &mut out);
        for v in &x.value_history {
            if let ValueHistoryEntry::Value(h) = v {
                push_opt(&h.value, &mut out);
            }
        }
        for row in &x.reaction_rows {
            if let Some(e) = &row.emoji {
                push_opt(e, &mut out);
            }
        }
        let mut h = s.history(x, reveal).expect("history");
        while let Some(v) = h.next_version().expect("version") {
            if let VersionText::Text(p) = &v.text {
                push_pieces(p, &mut out);
            }
            for e in &v.edits {
                if let EditEntryText::Text(p) = &e.text {
                    push_pieces(p, &mut out);
                }
            }
        }
        for a in s.attachment_facts(x).expect("facts") {
            push_opt(&a.facts.name, &mut out);
            push_opt(&a.facts.uti, &mut out);
            push_opt(&a.facts.emoji_description, &mut out);
            for h in &a.facts.history {
                push_opt(&h.value, &mut out);
            }
            blobs.extend(a.facts.blob.map(|b| b.id));
        }
    }
    for chat in s.chat_ids().expect("chats") {
        let cc = s.chat_columns(chat).expect("chat");
        push_opt(&cc.display_name, &mut out);
        push_opt(&cc.chat_identifier, &mut out);
        for h in &cc.name_history {
            push_opt(&h.value, &mut out);
        }
    }
    (out, blobs)
}

fn all_ids(i: &Imported) -> Vec<MessageId> {
    let c = common::import::read_only(&i.db());
    ints(&c, "SELECT id FROM message ORDER BY id", &[])
        .into_iter()
        .map(MessageId::new)
        .collect()
}

#[test]
fn no_masked_text_reaches_any_export_read_unless_revealed() {
    let mut checked = 0;
    for what in ["view-main-plain", "lineages"] {
        let (_, model) = model_docs()
            .into_iter()
            .find(|(w, _)| w == what)
            .expect("doc");
        let i: Imported = import_order(&model["orders"][0], what);
        let r = i.lib.open_read(&i.id).expect("reader");
        let ids = all_ids(&i);
        let codes = strings(&model["viewer"]["masked"]["oneTimeCodes"]);
        let earlier = strings(&model["viewer"]["masked"]["earlierText"]);
        assert!(!codes.is_empty() || !earlier.is_empty(), "{what}");
        let read =
            |reveal: Reveal| snapshot(&r, &i.lib, &i.id, |s| Ok(everything(s, &ids, reveal)));
        let (hidden, _) = read(Reveal::default());
        for m in codes.iter().chain(&earlier) {
            assert!(!hidden.contains(m.as_str()), "{what}: {m} leaked");
        }
        let (codes_only, _) = read(CODES);
        for m in &earlier {
            assert!(!codes_only.contains(m.as_str()), "{what}: {m} leaked");
        }
        let (earlier_only, _) = read(EARLIER);
        for m in &codes {
            assert!(!earlier_only.contains(m.as_str()), "{what}: {m} leaked");
        }
        let (shown, _) = read(REVEAL_ALL);
        for m in codes.iter().chain(&earlier) {
            assert!(shown.contains(m.as_str()), "{what}: {m} revealed");
            checked += 1;
        }
        // Each reveal alone shows its own sentinels where nothing else
        // hides them.
        for m in &codes {
            if earlier.iter().all(|e| !e.contains(m.as_str())) {
                assert!(codes_only.contains(m.as_str()), "{what}: {m} with codes");
            }
        }
    }
    assert!(checked >= 4);

    // The export catalogue's sentinels, by kind (slice 6c).
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    for (what, content, e) in export_docs() {
        let i = import_order_in(&export_root(), &content["orders"][0], &what);
        let r = i.lib.open_read(&i.id).expect("reader");
        let ids = all_ids(&i);
        let sent = |k: &str| strings(&e["sentinels"][k]);
        let codes = sent("oneTimeCodes");
        let earlier: Vec<String> = ["earlierText", "editText", "subjectHistory"]
            .iter()
            .flat_map(|k| sent(k))
            .collect();
        let alt = sent("alternativeText");
        let visible = strings(&e["visible"]);
        let c = common::import::read_only(&i.db());
        let unsent_blobs: HashSet<BlobId> = e["unsentAttachments"]
            .as_array()
            .expect("unsent")
            .iter()
            .map(|a| {
                BlobId::new(one(
                    &c,
                    "SELECT current_blob_id FROM attachment WHERE guid = ?1",
                    &[&a["attachment"].as_str().expect("guid").as_bytes()],
                ))
            })
            .collect();
        let read =
            |reveal: Reveal| snapshot(&r, &i.lib, &i.id, |s| Ok(everything(s, &ids, reveal)));
        let (hidden, blobs) = read(Reveal::default());
        for m in codes.iter().chain(&earlier).chain(&alt) {
            assert!(!hidden.contains(m.as_str()), "{what}: {m} leaked");
        }
        for m in &visible {
            assert!(hidden.contains(m.as_str()), "{what}: {m} not exported");
        }
        assert!(
            blobs.is_disjoint(&unsent_blobs),
            "{what}: an unsent part's file"
        );
        let (codes_only, blobs) = read(CODES);
        for m in &earlier {
            assert!(!codes_only.contains(m.as_str()), "{what}: {m} with codes");
        }
        for m in &codes {
            assert!(codes_only.contains(m.as_str()), "{what}: {m} with codes");
        }
        assert!(blobs.is_disjoint(&unsent_blobs), "{what}: with codes");
        let (earlier_only, blobs) = read(EARLIER);
        for m in &codes {
            assert!(
                !earlier_only.contains(m.as_str()),
                "{what}: {m} with earlier"
            );
        }
        for m in &earlier {
            assert!(
                earlier_only.contains(m.as_str()),
                "{what}: {m} with earlier"
            );
        }
        assert!(unsent_blobs.is_subset(&blobs), "{what}: files with earlier");
        let (shown, _) = read(REVEAL_ALL);
        for m in codes.iter().chain(&earlier).chain(&alt) {
            assert!(shown.contains(m.as_str()), "{what}: {m} revealed");
        }
        for (k, v) in e["sentinels"].as_object().expect("sentinels") {
            *by_kind.entry(k.clone()).or_default() += v.as_array().expect("list").len();
        }
        *by_kind.entry("unsentFiles".to_owned()).or_default() += unsent_blobs.len();
    }
    for k in [
        "oneTimeCodes",
        "earlierText",
        "editText",
        "subjectHistory",
        "alternativeText",
        "unsentFiles",
    ] {
        assert!(by_kind.get(k).copied().unwrap_or(0) > 0, "no {k} checked");
    }

    // Built cases on a bulk archive: alternative text, subject history and
    // the file of an unsent part, beside a code and earlier text.
    let (tmp, lib, id) = bulk(300);
    let c = raw(&db_path(&tmp, &id));
    let (m, body, atts) = three_attachments(&c);
    c.execute(
        "UPDATE message_body SET display_text = 'one SYNTHOTCCODE three', display_source = 1, \
           sources_differ = 1, text_raw = CAST('SYNTH-ALT-BEFORE-UNSEND' AS BLOB) \
         WHERE id = ?1",
        [body],
    )
    .expect("text");
    part(&c, body, 3, Some(0), (4, 16), CODE_FLAG, None);
    earlier_version(&c, m, body, "SYNTH-EARLIER-BEFORE-UNSEND");
    unsent(&c, body, 1);
    c.execute_batch(&format!(
        "INSERT INTO value_history (entity, row_id, col, value_hash, value, first_rank, \
           first_session_id) SELECT 3, id, 318, zeroblob(32), \
           CAST('SYNTH-SUBJECT-BEFORE-UNSEND' AS BLOB), first_rank, 1 FROM message \
           WHERE id = {m};"
    ))
    .expect("subject history");
    let r = lib.open_read(&id).expect("reader");
    let ids = [MessageId::new(m)];
    let read = |reveal: Reveal| snapshot(&r, &lib, &id, |s| Ok(everything(s, &ids, reveal)));
    let earlier = [
        "SYNTH-ALT-BEFORE-UNSEND",
        "SYNTH-EARLIER-BEFORE-UNSEND",
        "SYNTH-SUBJECT-BEFORE-UNSEND",
    ];
    let unsent_blob = BlobId::new(atts[1].1);
    let (hidden, blobs) = read(Reveal::default());
    for s in earlier.iter().chain(&["SYNTHOTCCODE"]) {
        assert!(!hidden.contains(s), "{s} leaked");
    }
    assert!(!blobs.contains(&unsent_blob), "the unsent part's file");
    assert_eq!(blobs.len(), 2);
    let (codes_only, blobs) = read(CODES);
    assert!(codes_only.contains("SYNTHOTCCODE"));
    for s in &earlier {
        assert!(!codes_only.contains(s), "{s} leaked with codes");
    }
    assert!(!blobs.contains(&unsent_blob));
    let (earlier_only, blobs) = read(EARLIER);
    assert!(!earlier_only.contains("SYNTHOTCCODE"));
    // The alternative text also needs codes (the body has one).
    assert!(earlier_only.contains("SYNTH-EARLIER-BEFORE-UNSEND"));
    assert!(earlier_only.contains("SYNTH-SUBJECT-BEFORE-UNSEND"));
    assert!(!earlier_only.contains("SYNTH-ALT-BEFORE-UNSEND"));
    assert!(blobs.contains(&unsent_blob));
    let (shown, _) = read(REVEAL_ALL);
    for s in earlier.iter().chain(&["SYNTHOTCCODE"]) {
        assert!(shown.contains(s), "{s} revealed");
    }
}
