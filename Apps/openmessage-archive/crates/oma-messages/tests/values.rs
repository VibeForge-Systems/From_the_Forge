//! The value model on real rows (M3 §5.2, §5.4, §19.3 `values.rs`): the
//! `Col` matrix through SQLite, caps at the boundary and one byte over,
//! invalid UTF-8, and per-value time classification. Typeless columns keep
//! whatever storage class was inserted, so every cell of the matrix can be
//! built.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use std::ops::ControlFlow;

use common::*;
use oma_messages::{
    BlobSize, ChatRecord, Col, ColShape, Cursor, HandleRecord, IssueKind, KnownTable, Probe,
    ProbeOptions, TimeInterp, ValueIssues, CAP_GUID, CAP_IDENTIFIER, CAP_PROPERTIES,
};

fn schema() -> String {
    let mut s: String = [
        KnownTable::Message,
        KnownTable::ChatMessageJoin,
        KnownTable::ChatHandleJoin,
    ]
    .iter()
    .map(|t| create_table(*t, true))
    .collect();
    s.push_str(
        "CREATE TABLE handle (ROWID INTEGER PRIMARY KEY, id TEXT);
         CREATE TABLE chat (ROWID INTEGER PRIMARY KEY, guid TEXT, style, display_name TEXT,
                            properties, is_archived, last_read_message_timestamp);\n",
    );
    let (g, i, p) = (CAP_GUID, CAP_IDENTIFIER, CAP_PROPERTIES);
    s.push_str(&format!(
        "INSERT INTO handle VALUES (1, printf('%.{i}c', 'a'));
         INSERT INTO handle VALUES (2, printf('%.{i1}c', 'a'));
         INSERT INTO chat VALUES (1, 'SYNTH-CHAT-GUID-1', 9876543, NULL, zeroblob({p}), 1, 0);
         INSERT INTO chat VALUES (2, printf('%.{g}c', 'g'), 'x', 'SYNTH-CHAT-NAME', zeroblob({p1}), 2, 1.0);
         INSERT INTO chat VALUES (3, printf('%.{g1}c', 'g'), 1.5, X'53594e5448', 'text', -1, 'x');
         INSERT INTO chat VALUES (4, NULL, NULL, CAST(X'61ff' AS TEXT), 5, 1.0, 700000000000000000);\n",
        i1 = i + 1,
        p1 = p + 1,
        g1 = g + 1,
    ));
    s
}

fn passes() -> (Vec<HandleRecord>, ValueIssues, Vec<ChatRecord>, ValueIssues) {
    let fx = Fixture::with_sms(&build(&schema()));
    with_probe(&fx, None, &ProbeOptions::default(), |p, ctx| {
        let Probe::Supported(db) = p.expect("probe") else {
            panic!("not supported");
        };
        let mut handles = Vec::new();
        let h = db
            .for_each_handle::<()>(Cursor::Start, ctx, |r| {
                handles.push(r);
                ControlFlow::Continue(())
            })
            .expect("pass");
        let mut chats = Vec::new();
        let c = db
            .for_each_chat::<()>(Cursor::Start, ctx, |r| {
                chats.push(r);
                ControlFlow::Continue(())
            })
            .expect("pass");
        (handles, h.value_issues, chats, c.value_issues)
    })
    .expect("connected")
}

#[test]
fn caps_at_the_boundary_and_one_byte_over() {
    let (handles, issues, chats, _) = passes();
    let cap = u64::from(CAP_IDENTIFIER);
    assert_eq!(
        handles[0].id.value().map(|v| v.expose().len()),
        Some(usize::try_from(cap).expect("small"))
    );
    assert_eq!(handles[1].id, Col::Oversized { bytes: cap + 1 });
    assert_eq!(issues.value_oversized, 1);
    assert_eq!(
        issues
            .oversized_by_column
            .values()
            .copied()
            .collect::<Vec<_>>(),
        [1]
    );
    let p = u64::from(CAP_PROPERTIES);
    assert_eq!(chats[0].properties, Col::Value(BlobSize { bytes: p }));
    assert_eq!(chats[1].properties, Col::Oversized { bytes: p + 1 });
    assert_eq!(
        chats[1].guid.value().map(|v| v.expose().len()),
        Some(usize::try_from(CAP_GUID).expect("small"))
    );
    assert_eq!(
        chats[2].guid,
        Col::Oversized {
            bytes: u64::from(CAP_GUID) + 1
        }
    );
}

#[test]
fn the_col_matrix_through_sqlite() {
    let (_, _, chats, issues) = passes();
    let [c1, c2, c3, c4] = &chats[..] else {
        panic!("four chats");
    };
    // Integer column: integer, text, real, NULL.
    assert_eq!(c1.style, Col::Value(9_876_543));
    assert_eq!(c2.style, Col::Unreadable(IssueKind::TypeMismatch));
    assert_eq!(c3.style, Col::Unreadable(IssueKind::TypeMismatch));
    assert_eq!(c4.style, Col::Null);
    // Bool column: 1, 2, -1, 1.0.
    assert_eq!(c1.is_archived, Col::Value(true));
    assert_eq!(c2.is_archived, Col::Unreadable(IssueKind::BoolOutOfRange));
    assert_eq!(c3.is_archived, Col::Unreadable(IssueKind::BoolOutOfRange));
    assert_eq!(c4.is_archived, Col::Value(true));
    // Text column: NULL, text, blob, invalid UTF-8.
    assert_eq!(c1.display_name, Col::Null);
    assert_eq!(
        c2.display_name.value().and_then(|v| v.expose().to_str()),
        Some("SYNTH-CHAT-NAME")
    );
    assert_eq!(
        c3.display_name.value().map(|v| v.expose().as_bytes()),
        Some(&b"SYNTH"[..])
    );
    assert_eq!(
        c4.display_name.value().map(|v| v.expose().as_bytes()),
        Some(&[0x61, 0xff][..])
    );
    // Blob-size column: text, integer.
    assert_eq!(c3.properties, Col::Value(BlobSize { bytes: 4 }));
    assert_eq!(c4.properties, Col::Unreadable(IssueKind::TypeMismatch));
    // Absent columns, distinct from NULL.
    assert_eq!(c1.chat_identifier, Col::Absent);
    assert!(!c1.room_name.is_recorded());
    assert!(c1.display_name.is_recorded());
    assert_eq!(c1.service_class, None);
    // Time, classified per value.
    assert_eq!(
        c1.last_read_message_timestamp.value().map(|t| t.interp),
        Some(TimeInterp::Zero)
    );
    assert!(matches!(
        c2.last_read_message_timestamp.value().map(|t| t.interp),
        Some(TimeInterp::Seconds { .. })
    ));
    assert_eq!(
        c3.last_read_message_timestamp,
        Col::Unreadable(IssueKind::TypeMismatch)
    );
    assert!(matches!(
        c4.last_read_message_timestamp.value().map(|t| t.interp),
        Some(TimeInterp::Nanoseconds { .. })
    ));
    assert_eq!(c4.guid.shape(), ColShape::Null);

    assert_eq!(issues.type_mismatch, 4);
    assert_eq!(issues.bool_out_of_range, 2);
    assert_eq!(issues.real_in_integer_column, 2);
    assert_eq!(issues.value_oversized, 2);
    assert_eq!(issues.blob_in_text_column, 1);
    assert_eq!(issues.text_in_blob_column, 1);
    assert_eq!(issues.invalid_utf8_text, 1);
}

/// Record `Debug` prints shapes and kinds, never values.
#[test]
fn record_debug_prints_no_value() {
    let (handles, _, chats, _) = passes();
    let shown = format!("{handles:?} {chats:?} {:#?}", chats[1]);
    for sentinel in ["SYNTH", "aaaa", "gggg", "9876543"] {
        assert!(!shown.contains(sentinel), "{sentinel} in {shown}");
    }
    assert!(shown.contains("Oversized"));
}
