//! The reader additions for the M4 archive import (M4 spec §3.3), on
//! synthetic databases built with SQLite:
//!
//! - R2: `raw_issues` keeps the raw value of each unreadable cell, in its
//!   storage class and under the column's cap, only when asked;
//! - R3: `payload_data` under its 1 MiB cap, only with `keep_raw_blobs`;
//! - R4: `assoc_raw` keeps the five association columns as read;
//! - R6: the key passes deliver exactly the keys of the full passes;
//! - R7: `SqlText::class()` reports the storage class SQLite returned.
//!
//! Typeless columns keep whatever storage class was inserted, so every case
//! can be built. Every value is synthetic.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use std::ops::ControlFlow;

use common::*;
use oma_messages::{
    ChatRecord, Col, Cursor, HandleKeyRow, IssueKind, KeyRow, KnownColumn, KnownTable,
    MessageReadOptions, MessageRecord, Probe, ProbeOptions, RawSql, SqlType, TextClass,
    ValueIssues, CAP_PAYLOAD_DATA, CAP_RAW_UNREADABLE,
};

fn schema() -> String {
    let mut s: String = [KnownTable::ChatMessageJoin, KnownTable::ChatHandleJoin]
        .iter()
        .map(|t| create_table(*t, true))
        .collect();
    let (p, p1, r1) = (
        CAP_PAYLOAD_DATA,
        CAP_PAYLOAD_DATA + 1,
        CAP_RAW_UNREADABLE + 1,
    );
    s.push_str(&format!(
        "CREATE TABLE message (ROWID INTEGER PRIMARY KEY, guid, date, is_from_me, text,
             payload_data, associated_message_type, associated_message_guid,
             associated_message_emoji, associated_message_range_location,
             associated_message_range_length, is_read, item_type);
         INSERT INTO message VALUES (1, 'SYNTH-MSG-GUID-1', 0, 0, 'SYNTH-BODY-1', X'0102',
             0, '', NULL, NULL, NULL, 'SYNTH-TEXT-bool', 2.5);
         INSERT INTO message VALUES (2, CAST('SYNTH-MSG-GUID-2' AS BLOB), 0, 0, NULL,
             zeroblob({p}), NULL, NULL, NULL, NULL, NULL, 7, printf('%.{r1}c', 'x'));
         INSERT INTO message VALUES (3, 'SYNTH-MSG-GUID-3', 'SYNTH-TEXT-date', 1, NULL,
             zeroblob({p1}), 2001, 'p:0/SYNTH-MSG-GUID-1', 'x', 0, 5, 1, 0);
         INSERT INTO message VALUES (4, 'SYNTH-MSG-GUID-4', 0, 0, NULL, 42, 0, NULL, NULL,
             NULL, NULL, 0, X'5359');
         INSERT INTO message VALUES (5, 'SYNTH-MSG-GUID-5', 0, 0, NULL, 'SYNTH-PAYLOAD', NULL,
             NULL, NULL, NULL, NULL, NULL, NULL);
         INSERT INTO message VALUES (6, NULL, 0, 0, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
             NULL, NULL);
         CREATE TABLE handle (ROWID INTEGER PRIMARY KEY, id, service, country);
         INSERT INTO handle VALUES (1, 'synthetic-handle-1@example.invalid', 'iMessage', 'x');
         INSERT INTO handle VALUES (2, printf('%.4097c', 'a'), 'SMS', 7);
         INSERT INTO handle VALUES (3, NULL, NULL, NULL);
         INSERT INTO handle VALUES (4, CAST('synthetic-handle-4' AS BLOB), 'SMS', NULL);
         CREATE TABLE chat (ROWID INTEGER PRIMARY KEY, guid, style, display_name, properties,
             is_archived);
         INSERT INTO chat VALUES (1, 'SYNTH-CHAT-GUID-1', 'SYNTH-CHAT-style', 7.5, 5, 3);
         INSERT INTO chat VALUES (2, 'SYNTH-CHAT-GUID-2', 43, 'SYNTH-CHAT-NAME', X'00', 1);
         CREATE TABLE attachment (ROWID INTEGER PRIMARY KEY, guid, total_bytes);
         INSERT INTO attachment VALUES (1, 'SYNTH-ATT-GUID-1', 'SYNTH-TEXT-size');
         INSERT INTO attachment VALUES (2, printf('%.257c', 'g'), 10);\n"
    ));
    s
}

fn col(t: KnownTable, name: &str) -> KnownColumn {
    t.known_columns()
        .find(|c| c.name() == name)
        .expect("known column")
}

type Text = Col<oma_messages::Redacted<oma_messages::SqlText>>;

fn bytes(c: &Text) -> Option<&[u8]> {
    c.value().map(|v| v.expose().as_bytes())
}

/// The raw issues of a record as (column name, storage class).
fn issues(raw: &[oma_messages::RawIssue]) -> Vec<(&'static str, SqlType)> {
    raw.iter()
        .map(|i| (i.column.name(), i.raw.sql_type()))
        .collect()
}

struct Passes {
    plain: Vec<MessageRecord>,
    plain_issues: ValueIssues,
    raw: Vec<MessageRecord>,
    raw_issues: ValueIssues,
    keys: Vec<KeyRow>,
    chats_plain: Vec<ChatRecord>,
    chats_raw: Vec<ChatRecord>,
    handle_keys: Vec<HandleKeyRow>,
    handles: Vec<(i64, Text, Text)>,
    attachment_keys: Vec<KeyRow>,
    attachments: Vec<(i64, Option<Vec<u8>>, usize)>,
    chat_keys: Vec<KeyRow>,
}

fn collect<T>(v: &mut Vec<T>) -> impl FnMut(T) -> ControlFlow<()> + '_ {
    |r| {
        v.push(r);
        ControlFlow::Continue(())
    }
}

fn passes() -> Passes {
    let fx = Fixture::with_sms(&build(&schema()));
    let run = |keep_raw_values: bool| {
        let opts = ProbeOptions::default()
            .with_page_rows(16)
            .with_keep_raw_values(keep_raw_values);
        with_probe(&fx, None, &opts, |p, ctx| {
            let Probe::Supported(db) = p.expect("probe") else {
                panic!("not supported");
            };
            let mut chats = Vec::new();
            db.for_each_chat(Cursor::Start, ctx, collect(&mut chats))
                .expect("chats");
            if !keep_raw_values {
                return (chats, None);
            }
            let mut plain = Vec::new();
            let o = MessageReadOptions::default();
            let plain_out = db
                .for_each_message(Cursor::Start, &o, None, ctx, collect(&mut plain))
                .expect("messages");
            let mut raw = Vec::new();
            let o = o.with_keep_raw_blobs(true);
            let raw_out = db
                .for_each_message(Cursor::Start, &o, None, ctx, collect(&mut raw))
                .expect("messages");
            let mut keys = Vec::new();
            let key_out = db
                .for_each_message_key(Cursor::Start, ctx, collect(&mut keys))
                .expect("keys");
            assert_eq!(key_out.rows, raw_out.rows);
            let mut handle_keys = Vec::new();
            db.for_each_handle_key(Cursor::Start, ctx, collect(&mut handle_keys))
                .expect("handle keys");
            let mut handles = Vec::new();
            db.for_each_handle(Cursor::Start, ctx, |h| {
                handles.push((h.rowid, h.id, h.service));
                ControlFlow::<()>::Continue(())
            })
            .expect("handles");
            let mut attachment_keys = Vec::new();
            db.for_each_attachment_key(Cursor::Start, ctx, collect(&mut attachment_keys))
                .expect("attachment keys");
            let mut attachments = Vec::new();
            db.for_each_attachment(Cursor::Start, None, ctx, |a| {
                attachments.push((
                    a.rowid,
                    bytes(&a.guid).map(<[u8]>::to_vec),
                    a.raw_issues.len(),
                ));
                ControlFlow::<()>::Continue(())
            })
            .expect("attachments");
            let mut chat_keys = Vec::new();
            db.for_each_chat_key(Cursor::Start, ctx, collect(&mut chat_keys))
                .expect("chat keys");
            (
                chats,
                Some(Passes {
                    plain,
                    plain_issues: plain_out.value_issues,
                    raw,
                    raw_issues: raw_out.value_issues,
                    keys,
                    chats_plain: Vec::new(),
                    chats_raw: Vec::new(),
                    handle_keys,
                    handles,
                    attachment_keys,
                    attachments,
                    chat_keys,
                }),
            )
        })
        .expect("connected")
    };
    let (chats_plain, _) = run(false);
    let (chats_raw, passes) = run(true);
    let mut passes = passes.expect("passes");
    passes.chats_plain = chats_plain;
    passes.chats_raw = chats_raw;
    passes
}

/// R3: `payload_data` is read only with `keep_raw_blobs`, under its cap,
/// and its storage class is reported either way. The pass's counts do not
/// depend on the option.
#[test]
fn payload_data_only_on_request_and_under_its_cap() {
    let p = passes();
    assert_eq!(p.plain.len(), 6);
    for m in &p.plain {
        assert_eq!(m.payload_data, None, "row {}", m.rowid);
    }
    let payload = |i: usize| p.raw[i].payload_data.clone().expect("requested");
    assert_eq!(
        payload(0).value().map(|b| b.expose().as_bytes().to_vec()),
        Some(vec![1, 2])
    );
    let cap = usize::try_from(CAP_PAYLOAD_DATA).expect("small");
    assert_eq!(payload(1).value().map(|b| b.expose().len()), Some(cap));
    assert_eq!(
        payload(2),
        Col::Oversized {
            bytes: u64::from(CAP_PAYLOAD_DATA) + 1
        }
    );
    assert_eq!(payload(3), Col::Unreadable(IssueKind::TypeMismatch));
    assert_eq!(
        payload(4).value().map(|b| b.expose().as_bytes().to_vec()),
        Some(b"SYNTH-PAYLOAD".to_vec())
    );
    assert_eq!(payload(5), Col::Null);
    // The storage class is the same in both modes.
    let classes = |v: &[MessageRecord]| -> Vec<Col<SqlType>> {
        v.iter().map(|m| m.payload_data_type.clone()).collect()
    };
    assert_eq!(classes(&p.plain), classes(&p.raw));
    assert_eq!(
        classes(&p.raw),
        [
            Col::Value(SqlType::Blob),
            Col::Value(SqlType::Blob),
            Col::Value(SqlType::Blob),
            Col::Value(SqlType::Integer),
            Col::Value(SqlType::Text),
            Col::Null
        ]
    );
    assert_eq!(p.plain_issues, p.raw_issues, "counts do not depend on R3");
}

/// R2: each unreadable cell's raw value, in its class, under the column's
/// cap; never without the option.
#[test]
fn raw_issues_keep_unreadable_cells() {
    let p = passes();
    for m in &p.plain {
        assert!(m.raw_issues.is_empty(), "row {}", m.rowid);
    }
    let m = &p.raw;
    // Row 1: text in a boolean column, a non-integral real in an integer one.
    assert_eq!(m[0].is_read, Col::Unreadable(IssueKind::TypeMismatch));
    assert_eq!(m[0].item_type, Col::Unreadable(IssueKind::TypeMismatch));
    assert_eq!(
        issues(&m[0].raw_issues),
        [("is_read", SqlType::Text), ("item_type", SqlType::Real)]
    );
    match &m[0].raw_issues[0].raw {
        RawSql::Text(t) => assert_eq!(t.expose().as_bytes(), b"SYNTH-TEXT-bool"),
        other => panic!("{other:?}"),
    }
    assert_eq!(m[0].raw_issues[1].raw, RawSql::Real(2.5));
    assert_eq!(
        m[0].raw_issues[1].column,
        col(KnownTable::Message, "item_type")
    );
    // Row 2: a boolean out of range is kept as its integer; text over
    // CAP_RAW_UNREADABLE in an integer column is unreadable but not kept.
    assert_eq!(m[1].is_read, Col::Unreadable(IssueKind::BoolOutOfRange));
    assert_eq!(m[1].item_type, Col::Unreadable(IssueKind::TypeMismatch));
    assert_eq!(issues(&m[1].raw_issues), [("is_read", SqlType::Integer)]);
    assert_eq!(m[1].raw_issues[0].raw, RawSql::Integer(7));
    // Row 3: text in a time column. The oversized payload is not an issue.
    assert_eq!(issues(&m[2].raw_issues), [("date", SqlType::Text)]);
    // Row 4: an integer payload_data, and a blob in an integer column.
    assert_eq!(
        issues(&m[3].raw_issues),
        [
            ("item_type", SqlType::Blob),
            ("payload_data", SqlType::Integer)
        ]
    );
    assert_eq!(m[3].raw_issues[1].raw, RawSql::Integer(42));
    assert!(m[4].raw_issues.is_empty() && m[5].raw_issues.is_empty());

    // Other records follow ProbeOptions::with_keep_raw_values.
    for c in &p.chats_plain {
        assert!(c.raw_issues.is_empty());
    }
    let c = &p.chats_raw[0];
    assert_eq!(c.style, Col::Unreadable(IssueKind::TypeMismatch));
    assert_eq!(c.display_name, Col::Unreadable(IssueKind::TypeMismatch));
    assert_eq!(c.properties, Col::Unreadable(IssueKind::TypeMismatch));
    assert_eq!(c.is_archived, Col::Unreadable(IssueKind::BoolOutOfRange));
    // `properties` is read for presence only, so it has no raw value.
    assert_eq!(
        issues(&c.raw_issues),
        [
            ("style", SqlType::Text),
            ("display_name", SqlType::Real),
            ("is_archived", SqlType::Integer)
        ]
    );
    assert_eq!(c.raw_issues[1].raw, RawSql::Real(7.5));
    assert!(p.chats_raw[1].raw_issues.is_empty());
    // The plain and raw chat records differ only in raw_issues.
    assert_eq!(p.chats_plain[1], p.chats_raw[1]);
    assert_eq!(p.attachments[0].2, 1, "text in total_bytes");
    // Debug never shows a raw value.
    let shown = format!("{:?} {:?}", m[0], p.chats_raw[0]);
    for s in ["SYNTH", "2.5", "7.5"] {
        assert!(!shown.contains(s), "{s} in {shown}");
    }
    assert!(shown.contains("RawIssue { column: message.is_read, raw: Text("));
}

/// R4: the association columns as read, whatever `association` says.
#[test]
fn assoc_raw_keeps_every_state() {
    let p = passes();
    let m = &p.plain;
    // '' and NULL: no association either way, but distinct raw.
    assert!(m[0].association.is_none() && m[1].association.is_none());
    assert_eq!(bytes(&m[0].assoc_raw.raw_guid), Some(&b""[..]));
    assert_eq!(m[0].assoc_raw.raw_type, Col::Value(0));
    assert_eq!(m[1].assoc_raw.raw_guid, Col::Null);
    assert_eq!(m[1].assoc_raw.raw_type, Col::Null);
    // A tapback: the raw columns and the association agree.
    let a = m[2].association.as_ref().expect("tapback");
    assert_eq!(m[2].assoc_raw.raw_type, Col::Value(2001));
    assert_eq!(
        bytes(&m[2].assoc_raw.raw_guid),
        Some(&b"p:0/SYNTH-MSG-GUID-1"[..])
    );
    assert_eq!(a.raw_type, m[2].assoc_raw.raw_type);
    assert_eq!(m[2].assoc_raw.range_length, Col::Value(5));
    assert_eq!(m[2].assoc_raw.association().as_ref(), Some(a));
    // Columns the table lacks are Absent, not NULL.
    let fx = Fixture::with_sms(&build(&format!(
        "{}INSERT INTO message (guid, date, is_from_me) VALUES ('SYNTH-MSG-GUID-9', 0, 0);",
        minimal_schema()
    )));
    let raw = with_probe(&fx, None, &ProbeOptions::default(), |p, ctx| {
        let Probe::Supported(db) = p.expect("probe") else {
            panic!("not supported");
        };
        let mut v = Vec::new();
        let o = MessageReadOptions::default().with_keep_raw_blobs(true);
        db.for_each_message(Cursor::Start, &o, None, ctx, collect(&mut v))
            .expect("pass");
        v.pop().expect("one row")
    })
    .expect("connected");
    assert_eq!(raw.assoc_raw.raw_type, Col::Absent);
    assert_eq!(raw.assoc_raw.raw_guid, Col::Absent);
    assert_eq!(raw.assoc_raw.emoji, Col::Absent);
    assert_eq!(raw.payload_data, Some(Col::Absent));
    assert!(raw.association.is_none());
}

/// R7: the storage class of text cells, through SQLite.
#[test]
fn text_cells_keep_their_storage_class() {
    let p = passes();
    let class = |m: &MessageRecord| m.guid.value().map(|g| g.expose().class());
    assert_eq!(class(&p.plain[0]), Some(TextClass::Text));
    assert_eq!(class(&p.plain[1]), Some(TextClass::Blob));
    assert_eq!(
        bytes(&p.plain[1].guid),
        Some(&b"SYNTH-MSG-GUID-2"[..]),
        "same bytes"
    );
    assert_eq!(p.plain_issues.blob_in_text_column, 1);
    let key_class = p.keys[1].guid.value().map(|g| g.expose().class());
    assert_eq!(key_class, Some(TextClass::Blob));
    let id_class = p.handle_keys[3].id.value().map(|g| g.expose().class());
    assert_eq!(id_class, Some(TextClass::Blob));
}

/// R6: the key passes deliver exactly the full passes' keys.
#[test]
fn key_passes_match_the_full_passes() {
    let p = passes();
    assert_eq!(p.keys.len(), p.raw.len());
    for (k, m) in p.keys.iter().zip(&p.raw) {
        assert_eq!((k.rowid, &k.guid), (m.rowid, &m.guid));
    }
    assert_eq!(p.keys[5].guid, Col::Null);
    // Handles: (id, service), with caps and storage classes.
    assert_eq!(p.handle_keys.len(), 4);
    for (k, (rowid, id, service)) in p.handle_keys.iter().zip(&p.handles) {
        assert_eq!((k.rowid, &k.id, &k.service), (*rowid, id, service));
    }
    assert_eq!(
        bytes(&p.handle_keys[0].id),
        Some(&b"synthetic-handle-1@example.invalid"[..])
    );
    assert_eq!(bytes(&p.handle_keys[0].service), Some(&b"iMessage"[..]));
    assert_eq!(p.handle_keys[1].id, Col::Oversized { bytes: 4097 });
    assert_eq!(
        (&p.handle_keys[2].id, &p.handle_keys[2].service),
        (&Col::Null, &Col::Null)
    );
    // Attachments and chats.
    let att: Vec<(i64, Option<Vec<u8>>)> = p
        .attachment_keys
        .iter()
        .map(|k| (k.rowid, bytes(&k.guid).map(<[u8]>::to_vec)))
        .collect();
    let full: Vec<(i64, Option<Vec<u8>>)> = p
        .attachments
        .iter()
        .map(|(r, g, _)| (*r, g.clone()))
        .collect();
    assert_eq!(att, full);
    assert_eq!(p.attachment_keys[1].guid, Col::Oversized { bytes: 257 });
    let chat: Vec<(i64, Option<&[u8]>)> = p
        .chat_keys
        .iter()
        .map(|k| (k.rowid, bytes(&k.guid)))
        .collect();
    assert_eq!(
        chat,
        [
            (1, Some(&b"SYNTH-CHAT-GUID-1"[..])),
            (2, Some(&b"SYNTH-CHAT-GUID-2"[..]))
        ]
    );
    let shown = format!("{:?} {:?}", p.keys, p.handle_keys);
    assert!(sentinels_in(&shown).is_empty(), "{shown}");
    assert!(shown.contains("KeyRow { rowid: 1, guid: Value"), "{shown}");
}

/// R6 over the catalogue's richest fixtures, plain and encrypted: every
/// key pass delivers the full pass's (rowid, key) sequence.
#[test]
fn key_passes_match_on_the_catalogue() {
    for (name, password) in [("msgs-full-plain", None), ("msgs-full-enc", Some(PASSWORD))] {
        let fx = Fixture::messages(name);
        with_probe(&fx, password, &ProbeOptions::default(), |p, ctx| {
            let Probe::Supported(db) = p.expect("probe") else {
                panic!("not supported");
            };
            let mut full = Vec::new();
            let o = MessageReadOptions::default().with_decode_attributed_body(false);
            db.for_each_message(Cursor::Start, &o, None, ctx, |m| {
                full.push((m.rowid, m.guid));
                ControlFlow::<()>::Continue(())
            })
            .expect("messages");
            let mut keys = Vec::new();
            db.for_each_message_key(Cursor::Start, ctx, |k| {
                keys.push((k.rowid, k.guid));
                ControlFlow::<()>::Continue(())
            })
            .expect("keys");
            assert!(!full.is_empty());
            assert_eq!(keys, full, "{name}: messages");

            let mut full = Vec::new();
            db.for_each_handle(Cursor::Start, ctx, |h| {
                full.push((h.rowid, h.id, h.service));
                ControlFlow::<()>::Continue(())
            })
            .expect("handles");
            let mut keys = Vec::new();
            db.for_each_handle_key(Cursor::Start, ctx, |k| {
                keys.push((k.rowid, k.id, k.service));
                ControlFlow::<()>::Continue(())
            })
            .expect("keys");
            assert!(!full.is_empty());
            assert_eq!(keys, full, "{name}: handles");

            let mut full = Vec::new();
            db.for_each_chat(Cursor::Start, ctx, |c| {
                full.push((c.rowid, c.guid));
                ControlFlow::<()>::Continue(())
            })
            .expect("chats");
            let mut keys = Vec::new();
            db.for_each_chat_key(Cursor::Start, ctx, |k| {
                keys.push((k.rowid, k.guid));
                ControlFlow::<()>::Continue(())
            })
            .expect("keys");
            assert_eq!(keys, full, "{name}: chats");

            let mut full = Vec::new();
            db.for_each_attachment(Cursor::Start, None, ctx, |a| {
                full.push((a.rowid, a.guid));
                ControlFlow::<()>::Continue(())
            })
            .expect("attachments");
            let mut keys = Vec::new();
            db.for_each_attachment_key(Cursor::Start, ctx, |k| {
                keys.push((k.rowid, k.guid));
                ControlFlow::<()>::Continue(())
            })
            .expect("keys");
            assert_eq!(keys, full, "{name}: attachments");
        })
        .expect("connected");
    }
}
