//! P4–P8 (slice 4h): the link passes, the attachment rows, the Recently
//! Deleted attachment exclusion and the markers (spec §8.4, §8.7, §8.8,
//! §6.5, §18.3).
//!
//! - **Recently Deleted:** by default an attachment linked only to excluded
//!   messages has no row and no link; one also linked to a live message is
//!   kept; an already archived message gets a marker and no link from its
//!   deleted state; the opt-in imports the messages and their attachments,
//!   and two `chat_recoverable_message_join` rows for one message are two
//!   markers.
//! - **Links merge by rank:** a link of an older backup stays when a newer
//!   one lacks it; `last_*` follow the highest rank, `first_session_id` the
//!   lowest, and `ord_rank`/`ord_rowid` the highest-ranked backup that saw
//!   the link, in both import orders.
//! - **Re-observing** the same content changes no link.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::path::Path;

use common::import::*;
use common::*;
use oma_archive::{ImportSummary, LineageChoice};

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
    let mut o = opts(16);
    o.include_recently_deleted = include_rd;
    let s = import(&mut a, &src, &o, lineage);
    drop(a);
    assert!(src.ob.close().removed);
    finish_sessions(db);
    s
}

fn count(db: &Path, sql: &str, guid: &str) -> i64 {
    read_only(db)
        .query_row(sql, [guid.as_bytes()], |r| r.get(0))
        .expect(sql)
}

fn attachments_of(db: &Path, guid: &str) -> i64 {
    count(db, "SELECT count(*) FROM attachment WHERE guid = ?1", guid)
}

fn links_of_attachment(db: &Path, guid: &str) -> i64 {
    count(
        db,
        "SELECT count(*) FROM message_attachment ma JOIN attachment a ON a.id = ma.attachment_id \
         WHERE a.guid = ?1",
        guid,
    )
}

/// The chats (by GUID) of a message's Recently Deleted markers.
fn markers_of(db: &Path, guid: &str) -> Vec<String> {
    let c = read_only(db);
    let mut v: Vec<String> = c
        .prepare(
            "SELECT c.guid FROM message_recently_deleted r JOIN message m ON m.id = r.message_id \
             LEFT JOIN chat c ON c.id = r.chat_ref WHERE m.guid = ?1",
        )
        .expect("prepare")
        .query_map([guid.as_bytes()], |r| r.get::<_, Option<Vec<u8>>>(0))
        .expect("query")
        .map(|g| String::from_utf8(g.expect("row").expect("resolved")).expect("utf-8"))
        .collect();
    v.sort();
    v
}

#[test]
fn recently_deleted_attachments_and_markers_follow_the_policy() {
    // Alone, by default: messages 0006, 0011, 0030 and 0031 are excluded.
    // Attachment 0009 is linked only to 0030: no row, no link. Attachment
    // 0010 is linked to 0031 and a live message: kept, with the live link
    // only. Nothing is marked.
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let s = step(&lib, &id, &db, "imp-rd-plain", LineageChoice::New, false);
    let rd = s.counts.recently_deleted;
    assert_eq!(
        (
            rd.excluded,
            rd.attachments_excluded,
            rd.marked,
            rd.not_marked
        ),
        (4, 1, 0, 5)
    );
    assert_eq!(attachments_of(&db, "SYNTH-ATT-GUID-0009"), 0);
    assert_eq!(attachments_of(&db, "SYNTH-ATT-GUID-0010"), 1);
    assert_eq!(links_of_attachment(&db, "SYNTH-ATT-GUID-0010"), 1);
    let ma = s.counts.message_attachments;
    assert_eq!(ma.skipped_recently_deleted, 2, "{ma:?}");
    assert_eq!(ma.unresolved, 0, "{ma:?}");
    assert_eq!(s.counts.attachments.seen, 7);
    assert_consistent(&db);

    // After v1, by default: 0006 and 0011 are archived, re-observed and
    // marked in their chats; 0030 and 0031 are excluded as before.
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
    let links_before = {
        let snap = logical_snapshot(&db, Scope::Content);
        (
            snap["chat_message"].clone(),
            snap["message_attachment"].clone(),
        )
    };
    let s = step(
        &lib,
        &id,
        &db,
        "imp-rd-plain",
        LineageChoice::Existing(v1.lineage),
        false,
    );
    let rd = s.counts.recently_deleted;
    assert_eq!((rd.not_updated, rd.marked, rd.not_marked), (2, 2, 3));
    assert_eq!(markers_of(&db, "SYNTH-MSG-GUID-0006").len(), 1);
    assert_eq!(markers_of(&db, "SYNTH-MSG-GUID-0011").len(), 1);
    assert!(markers_of(&db, "SYNTH-MSG-GUID-0030").is_empty());
    assert_eq!(attachments_of(&db, "SYNTH-ATT-GUID-0009"), 0);
    let snap = logical_snapshot(&db, Scope::Content);
    // The newer backup's links are added; the v1 links all stay.
    for (table, before) in [
        ("chat_message", &links_before.0),
        ("message_attachment", &links_before.1),
    ] {
        let keys = |v: &serde_json::Value| -> Vec<(String, String)> {
            let (a, b) = if table == "chat_message" {
                ("chat_id", "message_id")
            } else {
                ("message_id", "attachment_id")
            };
            v.as_array()
                .expect("rows")
                .iter()
                .map(|r| (r[a].to_string(), r[b].to_string()))
                .collect()
        };
        let after = keys(&snap[table]);
        for k in keys(before) {
            assert!(after.contains(&k), "{table}: a link disappeared");
        }
    }
    assert_consistent(&db);

    // The opt-in: every message and attachment is imported and every join
    // row is a marker; 0030, in two chats, has two.
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let s = step(&lib, &id, &db, "imp-rd-plain", LineageChoice::New, true);
    let rd = s.counts.recently_deleted;
    assert_eq!(
        (rd.imported, rd.excluded, rd.attachments_excluded, rd.marked),
        (4, 0, 0, 5)
    );
    assert_eq!(attachments_of(&db, "SYNTH-ATT-GUID-0009"), 1);
    assert_eq!(links_of_attachment(&db, "SYNTH-ATT-GUID-0009"), 1);
    assert_eq!(links_of_attachment(&db, "SYNTH-ATT-GUID-0010"), 2);
    let m30 = markers_of(&db, "SYNTH-MSG-GUID-0030");
    assert_eq!(m30.len(), 2, "two chats, two markers");
    assert_ne!(m30[0], m30[1]);
    assert_eq!(s.counts.message_attachments.skipped_recently_deleted, 0);
    assert_consistent(&db);
}

/// `(first, last)` session digests and the order columns of every
/// `message_attachment` row, by (message GUID, attachment GUID).
type OrdRow = (String, String, String, String, Vec<u8>, i64);

fn ord_rows(db: &Path) -> Vec<OrdRow> {
    let c = read_only(db);
    let mut v: Vec<OrdRow> = c
        .prepare(
            "SELECT hex(m.guid), hex(a.guid), hex(f.content_digest), hex(l.content_digest), \
                    ma.ord_rank, ma.ord_rowid \
             FROM message_attachment ma JOIN message m ON m.id = ma.message_id \
             JOIN attachment a ON a.id = ma.attachment_id \
             JOIN import_session f ON f.id = ma.first_session_id \
             JOIN import_session l ON l.id = ma.last_session_id",
        )
        .expect("prepare")
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    v.sort();
    v
}

#[test]
fn links_follow_rank_in_both_orders() {
    let digest = |entry: &str| -> String {
        let m = catalogue_json(&format!("{entry}.expected/content.json"));
        m["sessions"][entry]["contentDigest"]
            .as_str()
            .expect("digest")
            .to_uppercase()
    };
    let rank = |entry: &str| -> Vec<u8> {
        let m = catalogue_json(&format!("{entry}.expected/content.json"));
        let h = m["sessions"][entry]["rank"].as_str().expect("rank");
        (0..h.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&h[i..i + 2], 16).expect("hex"))
            .collect()
    };
    let (v1, v2) = (digest("imp-dev1-v1-plain"), digest("imp-dev1-v2-plain"));
    let v2_rank = rank("imp-dev1-v2-plain");
    let mut results = Vec::new();
    for order in [
        ["imp-dev1-v1-plain", "imp-dev1-v2-plain"],
        ["imp-dev1-v2-plain", "imp-dev1-v1-plain"],
    ] {
        let (tmp, lib) = library();
        let id = lib.create(None).expect("create");
        let db = db_path(&tmp, &id);
        let first = step(&lib, &id, &db, order[0], LineageChoice::New, false);
        let second = step(
            &lib,
            &id,
            &db,
            order[1],
            LineageChoice::Existing(first.lineage),
            false,
        );
        assert!(second.counts.chat_messages.already_present > 0);
        assert!(second.counts.message_attachments.already_present > 0);
        assert_consistent(&db);
        let rows = ord_rows(&db);
        let mut both = 0;
        for (_, _, f, l, ord_rank, _) in &rows {
            if f == &v1 && l == &v2 {
                // Seen by both: first is the lower rank, last and the order
                // the higher, whichever came first.
                both += 1;
                assert_eq!(ord_rank, &v2_rank);
            } else {
                // Seen by one backup only: both ends are that session.
                assert_eq!(f, l);
            }
        }
        assert!(both > 0, "{rows:?}");
        // A v1 link whose attachment v2 lost stays.
        assert!(rows.iter().any(|(_, _, f, l, _, _)| f == &v1 && l == &v1));
        results.push((rows, logical_snapshot(&db, Scope::Content)));
    }
    assert_eq!(results[0].0, results[1].0);
    for t in ["chat_handle", "chat_message", "message_attachment"] {
        assert_eq!(results[0].1[t], results[1].1[t], "{t}");
    }
}

#[test]
fn re_observing_the_same_content_changes_no_link() {
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
        let after = logical_snapshot(&db, Scope::Content);
        for (got, was) in [
            (second.counts.chat_handles, first.counts.chat_handles),
            (second.counts.chat_messages, first.counts.chat_messages),
            (
                second.counts.message_attachments,
                first.counts.message_attachments,
            ),
        ] {
            assert_eq!(got.seen, was.seen, "{entry}");
            assert_eq!(got.inserted, 0, "{entry}: {got:?}");
            assert_eq!(
                got.already_present,
                was.inserted + was.already_present,
                "{entry}"
            );
        }
        assert_eq!(second.counts.attachments.inserted, 0, "{entry}");
        for t in [
            "attachment",
            "chat_handle",
            "chat_message",
            "message_attachment",
        ] {
            assert_eq!(before[t], after[t], "{entry}: {t}");
        }
        // Markers are per session: a second session adds its own.
        let markers =
            |s: &serde_json::Value| s["message_recently_deleted"].as_array().map_or(0, Vec::len);
        assert_eq!(markers(&after), 2 * markers(&before), "{entry}: markers");
    }
}

#[test]
fn hostile_links_are_counted_never_inserted() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let db = db_path(&tmp, &id);
    let s = step(
        &lib,
        &id,
        &db,
        "imp-keys-hostile-plain",
        LineageChoice::New,
        false,
    );
    // The catalogue's orphan joins name rows that do not exist.
    let unresolved = s.counts.chat_handles.unresolved
        + s.counts.chat_messages.unresolved
        + s.counts.message_attachments.unresolved;
    assert!(unresolved > 0, "{:?}", s.counts);
    // Every stored link names two archive rows (foreign keys hold).
    assert_consistent(&db);
    let c = read_only(&db);
    let links: i64 = c
        .query_row(
            "SELECT (SELECT count(*) FROM chat_handle) + (SELECT count(*) FROM chat_message) \
                  + (SELECT count(*) FROM message_attachment)",
            [],
            |r| r.get(0),
        )
        .expect("count");
    let inserted = s.counts.chat_handles.inserted
        + s.counts.chat_messages.inserted
        + s.counts.message_attachments.inserted;
    assert_eq!(u64::try_from(links).expect("n"), inserted);
}
