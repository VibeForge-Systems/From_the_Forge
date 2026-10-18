//! P3 (slice 4g): messages, body versions, the display-text policy, the
//! FTS index and the Recently Deleted policy (spec §8.4, §8.7, §8.8, §10,
//! §18.3, §18.6).
//!
//! - **Idempotency:** every catalogue entry imported again in a second
//!   session leaves the content snapshot unchanged and inserts nothing.
//! - **Versions:** an edited or unsent message gains a body version; the
//!   earlier text stays indexed and searchable (D61); an older backup
//!   imported later does not move the current body.
//! - **Re-derivation:** a stored body with an older decoder version is
//!   re-derived in place, and the index follows.
//! - **The tokenizer table** of §10 as expectations.
//! - **Recently Deleted:** excluded by default, re-observed only when
//!   already archived, imported with the opt-in.
//! - **D58** on synthetic bodies, with decoded `attributedBody` vectors from
//!   the M3 typedstream catalogue (read only).
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::import::*;
use common::*;
use oma_archive::{ImportSummary, LineageChoice};
use serde_json::Value;

/// Import `entry` into the archive `id` (finalize completes the session).
fn step(
    lib: &oma_archive::Library,
    id: &oma_archive::ArchiveId,
    db: &Path,
    entry: &str,
    lineage: LineageChoice,
    include_rd: bool,
) -> ImportSummary {
    let mut a = lib.open(id).expect("open");
    let src = open_entry(entry);
    let mut o = opts(1000);
    o.include_recently_deleted = include_rd;
    let s = import(&mut a, &src, &o, lineage);
    drop(a);
    assert!(src.ob.close().removed);
    assert_completed(db, s.session);
    s
}

/// The message rows `MATCH q` finds, as (guid, body is current).
fn search(db: &Path, q: &str) -> rusqlite::Result<Vec<(String, bool)>> {
    let c = read_only(db);
    let mut stmt = c.prepare(
        "SELECT m.guid, m.current_body_id = b.id FROM message_fts f \
         JOIN message_body b ON b.id = f.rowid JOIN message m ON m.id = b.message_id \
         WHERE message_fts MATCH ?1 ORDER BY m.guid, 2",
    )?;
    let rows = stmt.query_map([q], |r| {
        let g: Vec<u8> = r.get(0)?;
        Ok((String::from_utf8_lossy(&g).into_owned(), r.get(1)?))
    })?;
    rows.collect()
}

fn hits(db: &Path, q: &str) -> Vec<(String, bool)> {
    search(db, q).unwrap_or_else(|e| panic!("{q}: {e}"))
}

#[test]
fn every_entry_imported_again_in_a_second_session_changes_nothing() {
    for entry in ENTRIES {
        let (tmp, lib) = library();
        let id = lib.create(None).expect("create");
        let db = db_path(&tmp, &id);
        let first = step(&lib, &id, &db, entry, LineageChoice::New, false);
        let before = logical_snapshot(&db, Scope::Content);
        let second = step(
            &lib,
            &id,
            &db,
            entry,
            LineageChoice::Existing(first.lineage),
            false,
        );
        assert_ne!(first.session, second.session, "{entry}");
        let (m1, m2) = (&first.counts.messages, &second.counts.messages);
        assert_eq!(m2.seen, m1.seen, "{entry}");
        assert_eq!(m2.inserted, 0, "{entry}: {m2:?}");
        assert_eq!(m2.already_present, m1.seen, "{entry}");
        assert_eq!(m2.cells_replaced, 0, "{entry}: {m2:?}");
        assert_eq!(m2.history_added, 0, "{entry}");
        let (b1, b2) = (&first.counts.bodies, &second.counts.bodies);
        assert_eq!(b2.added, 0, "{entry}: {b2:?}");
        assert_eq!(b2.unchanged, b1.added, "{entry}");
        assert_eq!((b2.rederived, b2.current_moved), (0, 0), "{entry}");
        // Recently Deleted rows that were excluded are excluded again.
        assert_eq!(
            second.counts.recently_deleted, first.counts.recently_deleted,
            "{entry}"
        );
        assert_consistent(&db);
        let after = logical_snapshot(&db, Scope::Content);
        assert_eq!(before, after, "{entry}: the content changed");
    }
}

#[test]
fn edited_and_unsent_messages_keep_every_version_searchable() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let v1 = step(
        &lib,
        &id,
        &db,
        "imp-dev1-v1-plain",
        LineageChoice::New,
        false,
    );
    assert_eq!(hits(&db, "edited").len(), 0);
    let v2 = step(
        &lib,
        &id,
        &db,
        "imp-dev1-v2-plain",
        LineageChoice::Existing(v1.lineage),
        false,
    );
    // Message 15 was edited and message 16 unsent: two new versions beside
    // those of v2's new messages, and both messages' current body moved.
    assert_eq!(
        v2.counts.bodies.added,
        v2.counts.messages.inserted + 2,
        "{:?}",
        v2.counts.bodies
    );
    assert_eq!(v2.counts.bodies.current_moved, 2);
    let g15 = "SYNTH-MSG-GUID-0015".to_owned();
    let g16 = "SYNTH-MSG-GUID-0016".to_owned();
    assert_eq!(hits(&db, "edited"), [(g15.clone(), true)]);
    // The original text is still indexed, and the current-body join
    // filters it out.
    assert_eq!(
        hits(&db, "\"synth body 15\" NOT edited"),
        [(g15.clone(), false)]
    );
    // D61: the unsent message's earlier text stays searchable; its current
    // version has no display text.
    assert_eq!(hits(&db, "\"synth body 16\""), [(g16.clone(), false)]);
    let c = read_only(&db);
    let current16: (Option<String>, i64, i64, Option<i64>) = c
        .query_row(
            "SELECT b.display_text, b.display_none_reason, b.edit_label, b.edit_label_a \
             FROM message m JOIN message_body b ON b.id = m.current_body_id WHERE m.guid = ?1",
            [g16.as_bytes()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("current 16");
    // No source; EditLabel::UnsentParts with one part.
    assert_eq!(current16, (None, 3, 4, Some(1)));
    let unsent: Vec<(i64, i64)> = c
        .prepare(
            "SELECT u.ordinal, u.part FROM message m JOIN message_unsent_part u \
             ON u.body_id = m.current_body_id WHERE m.guid = ?1",
        )
        .expect("prepare")
        .query_map([g16.as_bytes()], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(unsent, [(0, 0)]);
    // The edited version keeps its two edit entries (times in seconds) and
    // the raw summary bytes; EditLabel::EditHistory(1 part, 2 entries).
    let edits: Vec<EditRow> = c
        .prepare(
            "SELECT e.part, e.ordinal, e.at_kind, e.at_utc_ns, e.body_present \
             FROM message m JOIN message_edit_entry e ON e.body_id = m.current_body_id \
             WHERE m.guid = ?1 ORDER BY e.part, e.ordinal",
        )
        .expect("prepare")
        .query_map([g15.as_bytes()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(edits.len(), 2);
    for (i, e) in edits.iter().enumerate() {
        assert_eq!(
            (e.0, e.1, e.2, e.4),
            (0, i64::try_from(i).expect("i"), Some(1), 1)
        );
        assert!(e.3.is_some(), "an instant in seconds");
    }
    let (label, a, b, raw_len): (i64, Option<i64>, Option<i64>, i64) = c
        .query_row(
            "SELECT b.edit_label, b.edit_label_a, b.edit_label_b, length(b.summary_raw) \
             FROM message m JOIN message_body b ON b.id = m.current_body_id WHERE m.guid = ?1",
            [g15.as_bytes()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("label");
    assert_eq!((label, a, b), (5, Some(1), Some(2)));
    assert!(raw_len > 0);
    drop(c);
    assert_consistent(&db);
}

#[test]
fn an_older_backup_imported_later_keeps_the_newer_current_body() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let v2 = step(
        &lib,
        &id,
        &db,
        "imp-dev1-v2-plain",
        LineageChoice::New,
        false,
    );
    let v1 = step(
        &lib,
        &id,
        &db,
        "imp-dev1-v1-plain",
        LineageChoice::Existing(v2.lineage),
        false,
    );
    // The older versions are added (beside those of the message v2 lost),
    // and nothing moves.
    assert_eq!(
        v1.counts.bodies.added,
        v1.counts.messages.inserted + 2,
        "{:?}",
        v1.counts.bodies
    );
    assert_eq!(v1.counts.bodies.current_moved, 0);
    assert_eq!(
        hits(&db, "edited"),
        [("SYNTH-MSG-GUID-0015".to_owned(), true)]
    );
    assert_eq!(
        hits(&db, "\"synth body 15\" NOT edited"),
        [("SYNTH-MSG-GUID-0015".to_owned(), false)]
    );
    // The newer subject stays current; the older one is history.
    let c = read_only(&db);
    let (subject, history): (Vec<u8>, Vec<u8>) = c
        .query_row(
            "SELECT m.subject, CAST(h.value AS BLOB) FROM message m JOIN value_history h \
                 ON h.entity = 3 AND h.row_id = m.id AND h.col = 318 \
             WHERE m.guid = ?1",
            ["SYNTH-MSG-GUID-0019".as_bytes()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("subject");
    assert_eq!(history, b"SYNTH-SUBJECT-OLD");
    assert_eq!(subject, b"SYNTH-SUBJECT-NEW");
}

#[test]
fn a_newer_decoder_re_derives_a_stored_body_in_place() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let first = step(
        &lib,
        &id,
        &db,
        "imp-dev1-v1-plain",
        LineageChoice::New,
        false,
    );
    let want = logical_snapshot(&db, Scope::Content);
    // Stand in for bodies written by an older decoder: stale derived
    // columns and lost child rows. The FTS triggers keep the index in step
    // with the stale text.
    {
        let c = raw(&db);
        c.execute_batch(
            "UPDATE message_body SET decoder_version = 0, \
                 display_text = CASE WHEN display_text IS NULL THEN NULL ELSE 'SYNTH-STALE' END, \
                 edit_label = 0, edit_label_a = NULL, edit_label_b = NULL; \
             DELETE FROM message_body_part; DELETE FROM message_edit_entry; \
             DELETE FROM message_unsent_part;",
        )
        .expect("stale");
    }
    assert!(!hits(&db, "stale").is_empty());
    let second = step(
        &lib,
        &id,
        &db,
        "imp-dev1-v1-plain",
        LineageChoice::Existing(first.lineage),
        false,
    );
    assert_eq!(second.counts.bodies.rederived, first.counts.bodies.added);
    assert_eq!(second.counts.bodies.added, 0);
    assert!(hits(&db, "stale").is_empty());
    assert_consistent(&db);
    let got = logical_snapshot(&db, Scope::Content);
    for t in [
        "message",
        "message_body",
        "message_body_part",
        "message_edit_entry",
        "message_unsent_part",
        "message_fts_rows",
    ] {
        assert_eq!(got[t], want[t], "{t}");
    }
}

#[test]
fn the_fts_index_follows_the_tokenizer_table() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    step(&lib, &id, &db, "imp-fts-plain", LineageChoice::New, false);
    let g = |n: u32| (format!("SYNTH-MSG-GUID-F-{n:04}"), true);
    let guids = |q: &str| hits(&db, q);
    // NFC, NFD and upper case all match a folded query, both ways.
    assert_eq!(guids("cafe"), [g(1), g(2), g(3)]);
    assert_eq!(guids("CAFE"), [g(1), g(2), g(3)]);
    assert_eq!(guids("café"), [g(1), g(2), g(3)]);
    assert_eq!(guids("naive"), [g(3)]);
    // Unspaced CJK, Thai and Korean runs are one token: a part does not
    // match, the whole run does.
    assert!(guids("東京").is_empty());
    assert_eq!(guids("東京タワー"), [g(4)]);
    assert!(guids("ภาษา").is_empty());
    assert_eq!(guids("한국어"), [g(6)]);
    // A single emoji is not a token.
    assert!(search(&db, "\"\u{1F44D}\"").map_or(true, |r| r.is_empty()));
    assert_eq!(guids("family"), [g(8)]);
    // ß is not folded to ss.
    assert!(guids("strasse").is_empty());
    assert_eq!(guids("straße"), [g(9)]);
    // highlight() works directly on the table (D76).
    let c = read_only(&db);
    let h: String = c
        .query_row(
            "SELECT highlight(message_fts, 0, '[', ']') FROM message_fts \
             WHERE message_fts MATCH 'noir'",
            [],
            |r| r.get(0),
        )
        .expect("highlight");
    assert!(h.contains("[noir]"), "{h}");
    // The decoded attributedBody is the display source of message 1.
    let src: i64 = c
        .query_row(
            "SELECT b.display_source FROM message m JOIN message_body b \
             ON b.id = m.current_body_id WHERE m.guid = ?1",
            ["SYNTH-MSG-GUID-F-0001".as_bytes()],
            |r| r.get(0),
        )
        .expect("source");
    assert_eq!(src, 1);
}

#[test]
fn recently_deleted_messages_follow_the_policy() {
    // Alone, with the default: every Recently Deleted message is excluded.
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let alone = step(&lib, &id, &db, "imp-rd-plain", LineageChoice::New, false);
    let rd = alone.counts.recently_deleted;
    assert_eq!(
        (rd.messages, rd.excluded, rd.not_updated, rd.imported),
        (4, 4, 0, 0)
    );
    let excluded: i64 = read_only(&db)
        .query_row(
            "SELECT count(*) FROM message WHERE guid IN \
             (CAST('SYNTH-MSG-GUID-0006' AS BLOB), CAST('SYNTH-MSG-GUID-0011' AS BLOB), \
              CAST('SYNTH-MSG-GUID-0030' AS BLOB), CAST('SYNTH-MSG-GUID-0031' AS BLOB))",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(excluded, 0);

    // After v1: the two archived messages are re-observed only; the new
    // ones are excluded.
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let v1 = step(
        &lib,
        &id,
        &db,
        "imp-dev1-v1-plain",
        LineageChoice::New,
        false,
    );
    let before = logical_snapshot(&db, Scope::Content);
    let s = step(
        &lib,
        &id,
        &db,
        "imp-rd-plain",
        LineageChoice::Existing(v1.lineage),
        false,
    );
    let rd = s.counts.recently_deleted;
    assert_eq!((rd.excluded, rd.not_updated, rd.imported), (2, 2, 0));
    let after = logical_snapshot(&db, Scope::Content);
    // A message re-observed from Recently Deleted keeps its row and its
    // body versions exactly (its changed is_read is not applied), while
    // the other messages of the newer backup move to its rank.
    let of = |snap: &Value, table: &str, col: &str, key: &Value| -> Vec<Value> {
        snap[table]
            .as_array()
            .expect("rows")
            .iter()
            .filter(|r| &r[col] == key)
            .cloned()
            .collect()
    };
    for g in ["SYNTH-MSG-GUID-0006", "SYNTH-MSG-GUID-0011"] {
        let key = serde_json::json!(format!("k:{}", hex(g.as_bytes())));
        let blob = serde_json::json!({ "blob": hex(g.as_bytes()) });
        assert_eq!(of(&before, "message", "guid", &blob).len(), 1, "{g}");
        assert_eq!(
            of(&before, "message", "guid", &blob),
            of(&after, "message", "guid", &blob),
            "{g}"
        );
        assert_eq!(
            of(&before, "message_body", "message_id", &key),
            of(&after, "message_body", "message_id", &key),
            "{g}"
        );
    }
    assert_ne!(before["message"], after["message"]);

    // With the opt-in, every Recently Deleted message is imported.
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let v1 = step(
        &lib,
        &id,
        &db,
        "imp-dev1-v1-plain",
        LineageChoice::New,
        false,
    );
    let s = step(
        &lib,
        &id,
        &db,
        "imp-rd-plain",
        LineageChoice::Existing(v1.lineage),
        true,
    );
    let rd = s.counts.recently_deleted;
    assert_eq!((rd.excluded, rd.not_updated, rd.imported), (0, 0, 4));
    assert_consistent(&db);
}

// ---------------------------------------------------------------------------
// D58 on synthetic bodies
// ---------------------------------------------------------------------------

fn typedstream(name: &str) -> Vec<u8> {
    let p = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/typedstream"
    ))
    .join(name);
    fs::read(p).expect("vector")
}

fn typedstream_parts(name: &str) -> Value {
    let p = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/typedstream/expected.json"
    ));
    let all: Value = serde_json::from_slice(&fs::read(p).expect("expected")).expect("json");
    all["vectors"]
        .as_array()
        .expect("vectors")
        .iter()
        .find(|v| v["file"] == name)
        .map(|v| v["public"]["ok"]["parts"].clone())
        .expect("vector listed")
}

/// part, ordinal, at_kind, at_utc_ns, body_present.
type EditRow = (i64, i64, Option<i64>, Option<i64>, i64);
/// ordinal, part_index, (utf16, utf8 spans), attachment_guid, flags,
/// keys_present, text_effect.
type PartRow = (
    i64,
    Option<i64>,
    [i64; 4],
    Option<Vec<u8>>,
    i64,
    i64,
    Option<i64>,
);

/// One synthetic message: (guid, text, attributedBody, summary).
type Body<'a> = (&'a str, Option<&'a [u8]>, Option<Vec<u8>>, Option<&'a [u8]>);

fn sms_with_bodies(bodies: &[Body<'_>]) -> Vec<u8> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("sms.db");
    let c = rusqlite::Connection::open(&path).expect("create");
    c.execute_batch(&full_schema()).expect("schema");
    for (i, (guid, text, ab, summary)) in bodies.iter().enumerate() {
        c.execute(
            "INSERT INTO message (ROWID, guid, text, attributedBody, message_summary_info, date, \
                 is_from_me, item_type, handle_id) \
             VALUES (?1, ?2, CAST(?3 AS TEXT), ?4, ?5, 790000000000000000, 0, 0, 0)",
            rusqlite::params![i64::try_from(i).expect("i") + 1, guid, text, ab, summary],
        )
        .expect("message");
    }
    drop(c);
    fs::read(&path).expect("read")
}

/// (display_text, display_source, display_none_reason, attributed_outcome,
/// attributed_error, sources_differ, summary_outcome, edit_label).
type Derived = (
    Option<String>,
    Option<i64>,
    Option<i64>,
    i64,
    Option<i64>,
    Option<i64>,
    i64,
    i64,
);

#[test]
fn display_text_follows_d58_and_raw_bodies_are_kept_exactly() {
    let hello = typedstream("v1_hello.bin");
    let attach = typedstream("v3_attachment_then_text.bin");
    let formatted = typedstream("v8_formatting.bin");
    let garbage = vec![0x04u8, 0x0b, 0x01];
    let bodies: Vec<Body<'_>> = vec![
        ("SYNTH-D-01", Some(b"SYNTH-TEXT-ONLY"), None, None),
        ("SYNTH-D-02", Some(b"SYNTH-\xff-LOSSY"), None, None),
        ("SYNTH-D-03", Some(b""), None, None),
        ("SYNTH-D-04", None, None, None),
        ("SYNTH-D-05", Some(b""), Some(garbage.clone()), None),
        (
            "SYNTH-D-06",
            Some(b"SYNTH-BESIDE"),
            Some(garbage.clone()),
            None,
        ),
        ("SYNTH-D-07", Some(b"Hello"), Some(hello.clone()), None),
        (
            "SYNTH-D-08",
            Some(b"SYNTH-OTHER"),
            Some(hello.clone()),
            None,
        ),
        (
            "SYNTH-D-09",
            None,
            Some(attach.clone()),
            Some(b"not a plist"),
        ),
        ("SYNTH-D-10", None, Some(formatted.clone()), None),
    ];
    let sms = sms_with_bodies(&bodies);
    let btmp = tempfile::tempdir().expect("tempdir");
    let backup = backup_with_sms(&btmp.path().join("backup"), &sms);
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut a = lib.open(&id).expect("open");
    let src = open_dir(&backup, None);
    let s = import(&mut a, &src, &opts(16), LineageChoice::New);
    drop(a);
    src.ob.close();
    let db = db_path(&tmp, &id);
    assert_consistent(&db);
    assert_eq!(s.counts.bodies.added, 10);
    assert_eq!(s.counts.bodies.decode_failed, 2);
    assert_eq!(s.counts.bodies.display_text_lossy, 1);
    assert_eq!(s.counts.bodies.none_attributed_body_unusable, 1);
    assert_eq!(s.counts.bodies.sources_differ, 1);

    let c = read_only(&db);
    let mut by_guid: BTreeMap<String, Derived> = BTreeMap::new();
    let mut stmt = c
        .prepare(
            "SELECT m.guid, b.display_text, b.display_source, b.display_none_reason, \
                    b.attributed_outcome, b.attributed_error, b.sources_differ, \
                    b.summary_outcome, b.edit_label \
             FROM message m JOIN message_body b ON b.id = m.current_body_id",
        )
        .expect("prepare");
    let mut q = stmt.query([]).expect("query");
    while let Some(r) = q.next().expect("row") {
        let g: Vec<u8> = r.get(0).expect("guid");
        by_guid.insert(
            String::from_utf8(g).expect("utf8"),
            (
                r.get(1).expect("t"),
                r.get(2).expect("s"),
                r.get(3).expect("n"),
                r.get(4).expect("o"),
                r.get(5).expect("e"),
                r.get(6).expect("d"),
                r.get(7).expect("so"),
                r.get(8).expect("l"),
            ),
        );
    }
    drop(q);
    drop(stmt);
    let row = |g: &str| by_guid[g].clone();
    // 2 = text; summary absent (0) or NULL (1) per the column's state.
    assert_eq!(row("SYNTH-D-01").0.as_deref(), Some("SYNTH-TEXT-ONLY"));
    assert_eq!(row("SYNTH-D-01").1, Some(2));
    assert_eq!(row("SYNTH-D-02").0.as_deref(), Some("SYNTH-\u{FFFD}-LOSSY"));
    assert_eq!(row("SYNTH-D-02").1, Some(3));
    assert_eq!((row("SYNTH-D-03").0, row("SYNTH-D-03").2), (None, Some(2)));
    assert_eq!((row("SYNTH-D-04").0, row("SYNTH-D-04").2), (None, Some(3)));
    // Undecodable attributedBody beside an empty text: unusable, failed.
    let d5 = row("SYNTH-D-05");
    assert_eq!((d5.0.clone(), d5.2, d5.3), (None, Some(1), 6));
    assert!(d5.4.is_some());
    // Beside a text, the text is shown; the sources are not comparable.
    let d6 = row("SYNTH-D-06");
    assert_eq!(
        (d6.0.as_deref(), d6.1, d6.3, d6.5),
        (Some("SYNTH-BESIDE"), Some(2), 6, None)
    );
    // Decoded: the decoded text wins; sources compared.
    let d7 = row("SYNTH-D-07");
    assert_eq!(
        (d7.0.as_deref(), d7.1, d7.3, d7.5),
        (Some("Hello"), Some(1), 5, Some(0))
    );
    assert_eq!(row("SYNTH-D-08").5, Some(1));
    // A summary that is not a property list: unreadable, raw bytes kept.
    let d9 = row("SYNTH-D-09");
    assert_eq!((d9.6, d9.7), (3, 2));
    let (raw_ab, raw_summary): (Vec<u8>, Vec<u8>) = c
        .query_row(
            "SELECT b.attributed_raw, b.summary_raw FROM message m \
             JOIN message_body b ON b.id = m.current_body_id WHERE m.guid = ?1",
            [b"SYNTH-D-09".as_slice()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("raw");
    assert_eq!(raw_ab, attach, "attributedBody kept byte for byte (D59)");
    assert_eq!(raw_summary, b"not a plist");
    // The decoded parts, as the M3 catalogue states them.
    for (guid, vector) in [
        ("SYNTH-D-09", "v3_attachment_then_text.bin"),
        ("SYNTH-D-10", "v8_formatting.bin"),
    ] {
        let want = typedstream_parts(vector);
        let got: Vec<PartRow> = c
            .prepare(
                "SELECT p.ordinal, p.part_index, p.utf16_start, p.utf16_end, p.utf8_start, \
                        p.utf8_end, p.attachment_guid, p.flags, p.keys_present, p.text_effect \
                 FROM message m JOIN message_body_part p ON p.body_id = m.current_body_id \
                 WHERE m.guid = ?1 ORDER BY p.ordinal",
            )
            .expect("prepare")
            .query_map([guid.as_bytes()], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    [r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?],
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                ))
            })
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        let want = want.as_array().expect("parts");
        assert_eq!(got.len(), want.len(), "{guid}");
        for (g, w) in got.iter().zip(want) {
            assert_eq!(g.1, w["part"].as_i64(), "{guid}");
            let span = |k: &str| -> Vec<i64> {
                w[k].as_array()
                    .expect("span")
                    .iter()
                    .map(|x| x.as_i64().expect("n"))
                    .collect()
            };
            assert_eq!(g.2[..2], span("utf16")[..], "{guid}");
            assert_eq!(g.2[2..], span("utf8")[..], "{guid}");
            assert_eq!(
                g.3.as_deref()
                    .map(|b| String::from_utf8_lossy(b).into_owned()),
                w["guid"].as_str().map(str::to_owned),
                "{guid}"
            );
            let bits = |names: &Value, table: &[&str]| -> i64 {
                names
                    .as_array()
                    .expect("names")
                    .iter()
                    .map(|n| {
                        let at = table
                            .iter()
                            .position(|t| Some(*t) == n.as_str())
                            .expect("known name");
                        1i64 << at
                    })
                    .sum()
            };
            use oma_archive::schema::codes::{ATTR_FLAG_BITS, ATTR_KEY_BITS};
            assert_eq!(g.4, bits(&w["flags"], &ATTR_FLAG_BITS), "{guid} flags");
            assert_eq!(g.5, bits(&w["keys"], &ATTR_KEY_BITS), "{guid} keys");
            assert_eq!(g.6, w["text_effect"].as_i64(), "{guid}");
        }
        assert!(!got.is_empty());
    }
}
