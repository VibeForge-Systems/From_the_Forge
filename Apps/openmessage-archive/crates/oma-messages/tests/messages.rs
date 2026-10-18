//! The message pass on rows built in the test (M3 §9–§11): bodies,
//! tapbacks and other associations, replies, summaries and edit labels,
//! item kinds, GUID sets, raw blobs, and redaction. Every value is
//! synthetic.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use std::collections::BTreeMap;
use std::ops::ControlFlow;

use common::bplist::{Bp, Obj};
use common::ts_writer::{self as tw, AttrVal};
use common::*;
use oma_messages::{
    AssocTarget, AssociatedKind, AttributedOutcome, Col, Cursor, EditLabel, GuidSets, IssueKind,
    ItemKind, KnownTable, MessageReadOptions, MessageRecord, PassOutcome, Probe, ProbeOptions,
    Reaction, ServiceClass, SqlType, TimeInterp, TsErrorKind, CAP_GUID, CAP_SUMMARY,
};
use rusqlite::{params, Connection};

const NS_DATE: i64 = 700_000_000_000_000_000;
const SEC_DATE: i64 = 700_000_000;

fn ts_body(segs: &[(&str, Vec<(&'static str, AttrVal)>)]) -> Vec<u8> {
    let segs: Vec<(String, tw::Attrs)> = segs
        .iter()
        .map(|(t, a)| ((*t).to_owned(), a.clone()))
        .collect();
    tw::attributed(&segs).0
}

fn summary_ec(entries: usize) -> Vec<u8> {
    let mut b = Bp::new();
    let d = b.int(SEC_DATE + 10);
    let t = b.add(Obj::Data(b"SYNTH-EDIT-1".to_vec()));
    let e = b.dict(&[("d", d), ("t", t)]);
    let part = b.add(Obj::Array(vec![e; entries]));
    let ec = b.dict(&[("0", part)]);
    let root = b.dict(&[("ec", ec)]);
    b.finish(root)
}

fn summary_rp() -> Vec<u8> {
    let mut b = Bp::new();
    let zero = b.int(0);
    let rp = b.add(Obj::Array(vec![zero]));
    let empty = b.add(Obj::Dict(vec![]));
    let otr = b.dict(&[("0", empty)]);
    let root = b.dict(&[("rp", rp), ("otr", otr)]);
    b.finish(root)
}

/// Every known table, plus the message rows described inline.
fn fixture() -> Vec<u8> {
    build_with(|c: &Connection| {
        c.execute_batch(&full_schema()).expect("schema");
        let base = "INSERT INTO message (ROWID, guid, date, is_from_me, service, handle_id";
        let msg = |rowid: i64, extra_cols: &str, extra_vals: &str| {
            let sql = format!(
                "{base}{extra_cols}) VALUES ({rowid}, 'SYNTH-MSG-{rowid}', {NS_DATE}, 0, 'iMessage', 1{extra_vals})"
            );
            c.execute(&sql, []).expect("insert");
        };
        let body1 = ts_body(&[("SYNTH-BODY-1", vec![(tw::PART, AttrVal::Int(0))])]);
        c.execute(
            &format!("{base}, text, attributedBody, item_type) VALUES (1, 'SYNTH-MSG-1', ?1, 0, 'iMessage', 1, 'SYNTH-BODY-1', ?2, 0)"),
            params![NS_DATE, body1],
        )
        .expect("1");
        // Empty text beside an attributedBody with two parts and a mention.
        let body2 = ts_body(&[
            ("SYNTH-BODY-2 ", vec![(tw::PART, AttrVal::Int(0))]),
            (
                "SYNTH-MENTION",
                vec![
                    (tw::PART, AttrVal::Int(0)),
                    (
                        tw::MENTION,
                        AttrVal::Str("synthetic-handle-2@example.invalid".into()),
                    ),
                ],
            ),
            (
                "\u{FFFC}",
                vec![
                    (tw::PART, AttrVal::Int(1)),
                    (tw::GUID, AttrVal::Str("SYNTH-AT-1".into())),
                ],
            ),
        ]);
        c.execute(
            &format!(
                "{base}, text, attributedBody) VALUES (2, 'SYNTH-MSG-2', ?1, 1, 'SMS', 2, '', ?2)"
            ),
            params![NS_DATE, body2],
        )
        .expect("2");
        c.execute(
            &format!("{base}, attributedBody) VALUES (3, 'SYNTH-MSG-3', ?1, 0, 'rcs', 1, ?2)"),
            params![NS_DATE, b"SYNTH-BODY-not-typedstream".to_vec()],
        )
        .expect("3");
        // Tapbacks and other associations.
        msg(
            4,
            ", associated_message_type, associated_message_guid",
            ", 2000, 'p:0/SYNTH-MSG-1'",
        );
        msg(
            5,
            ", associated_message_type, associated_message_guid",
            ", 3001, 'bp:SYNTH-MSG-2'",
        );
        msg(
            6,
            ", associated_message_type, associated_message_guid, associated_message_emoji, associated_message_range_location, associated_message_range_length",
            ", 2006, 'p:0/SYNTH-MSG-GONE', 'SYNTH-EMOJI', 0, 12",
        );
        // Replies.
        msg(
            7,
            ", thread_originator_guid, thread_originator_part, reply_to_guid",
            ", 'SYNTH-MSG-1', '0:0:12', 'SYNTH-MSG-1'",
        );
        msg(8, ", thread_originator_guid", ", 'SYNTH-MSG-GONE'");
        msg(9, ", reply_to_guid", ", 'SYNTH-MSG-1'");
        // Edits and unsends.
        c.execute(
            &format!("{base}, text, date_edited, message_summary_info) VALUES (10, 'SYNTH-MSG-10', ?1, 1, 'iMessage', 1, 'SYNTH-BODY-10', ?1, ?2)"),
            params![NS_DATE, summary_ec(2)],
        )
        .expect("10");
        c.execute(
            &format!("{base}, date_edited, message_summary_info) VALUES (11, 'SYNTH-MSG-11', ?1, 1, 'iMessage', 1, ?1, ?2)"),
            params![NS_DATE, summary_rp()],
        )
        .expect("11");
        msg(12, ", date_edited", &format!(", {NS_DATE}"));
        c.execute(
            &format!("{base}, message_summary_info) VALUES (13, 'SYNTH-MSG-13', ?1, 0, 'iMessage', 1, ?2)"),
            params![NS_DATE, b"bplist00SYNTH-not-a-plist".to_vec()],
        )
        .expect("13");
        msg(
            14,
            ", message_summary_info",
            &format!(", zeroblob({})", CAP_SUMMARY + 1),
        );
        // Item kinds.
        msg(
            15,
            ", item_type, group_action_type, other_handle",
            ", 1, 0, 1",
        );
        msg(
            16,
            ", item_type, other_handle, share_direction",
            ", 4, 2, 0",
        );
        msg(
            17,
            ", payload_data, balloon_bundle_id, item_type, group_title",
            ", X'00ff', 'SYNTH-BALLOON', 2, 'SYNTH-GROUP-TITLE'",
        );
        // An oversized guid and association target: never read, never hashed.
        c.execute(
            &format!("{base}, associated_message_type, associated_message_guid) VALUES (18, printf('%.{g}c', 'g'), ?1, 0, 'iMessage', 1, 2001, 'p:0/' || printf('%.{g}c', 'g'))", g = CAP_GUID + 1),
            params![NS_DATE],
        )
        .expect("18");
        msg(
            19,
            ", associated_message_type, associated_message_guid",
            ", 2003, 'SYNTH-MSG-2'",
        );
        msg(
            20,
            ", associated_message_type, associated_message_guid",
            ", 2, 'p:x/SYNTH-MSG-1'",
        );
    })
}

struct Pass {
    rows: BTreeMap<i64, MessageRecord>,
    out: PassOutcome<()>,
    guids: GuidSets,
    count: Option<u64>,
}

fn run(db: &[u8], opts: &MessageReadOptions) -> Pass {
    let fx = Fixture::with_sms(db);
    with_probe(
        &fx,
        None,
        &ProbeOptions::default().with_page_rows(16),
        |p, ctx| {
            let Probe::Supported(db) = p.expect("probe") else {
                panic!("not supported");
            };
            let mut rows = BTreeMap::new();
            let mut guids = GuidSets::new();
            let out = db
                .for_each_message(Cursor::Start, opts, Some(&mut guids), ctx, |m| {
                    rows.insert(m.rowid, m);
                    ControlFlow::Continue(())
                })
                .expect("pass");
            let count = db
                .capabilities()
                .table(KnownTable::Message)
                .and_then(|t| t.rows);
            Pass {
                rows,
                out,
                guids,
                count,
            }
        },
    )
    .expect("connected")
}

fn text_of(c: &Col<oma_messages::Redacted<oma_messages::SqlText>>) -> Option<&[u8]> {
    c.value().map(|t| t.expose().as_bytes())
}

#[test]
fn bodies_associations_replies_and_edits() {
    let mut p = run(&fixture(), &MessageReadOptions::default());
    assert_eq!(p.out.rows, 20);
    assert_eq!(p.count, Some(p.out.rows), "rows streamed == count(*)");
    let m = |id: i64| p.rows.get(&id).expect("row");

    // Bodies: both sources kept, neither preferred.
    let r1 = m(1);
    assert_eq!(text_of(&r1.body.text_column), Some(&b"SYNTH-BODY-1"[..]));
    let AttributedOutcome::Decoded(b1) = &r1.body.attributed else {
        panic!("row 1 decodes: {:?}", r1.body.attributed);
    };
    assert_eq!(b1.text.expose().as_str(), "SYNTH-BODY-1");
    assert_eq!(r1.service_class, Some(ServiceClass::IMessage));
    assert_eq!(r1.item_kind, ItemKind::Message);
    assert_eq!(r1.association, None);
    assert!(!r1.reply.is_reply());
    // date_edited and the summary exist but say nothing.
    assert_eq!(r1.edit_label, EditLabel::NoEditRecorded);
    assert_eq!(r1.payload_data_type, Col::Null);
    assert!(matches!(
        r1.date.value().map(|d| d.interp),
        Some(TimeInterp::Nanoseconds { .. })
    ));

    let r2 = m(2);
    assert_eq!(text_of(&r2.body.text_column), Some(&b""[..]));
    let AttributedOutcome::Decoded(b2) = &r2.body.attributed else {
        panic!("row 2 decodes");
    };
    assert_eq!(b2.parts.len(), 2);
    assert!(b2.parts[0].mention.is_some() && b2.parts[1].attachment_guid.is_some());
    assert_eq!(r2.is_from_me, Col::Value(true));
    assert_eq!(r2.service_class, Some(ServiceClass::Sms));

    assert_eq!(
        m(3).body.attributed,
        AttributedOutcome::Failed(TsErrorKind::UnsupportedHeader)
    );
    assert_eq!(m(3).body.text_column, Col::Null);
    assert_eq!(m(3).service_class, Some(ServiceClass::Rcs));

    // Associations.
    let a4 = m(4).association.as_ref().expect("tapback");
    assert_eq!(
        a4.kind,
        Some(AssociatedKind::Tapback {
            reaction: Reaction::Love,
            removal: false
        })
    );
    assert!(matches!(
        a4.target,
        Col::Value(AssocTarget::Part { part: 0, .. })
    ));
    assert_eq!(a4.emoji, Col::Null);
    let a5 = m(5).association.as_ref().expect("removal");
    assert_eq!(
        a5.kind,
        Some(AssociatedKind::Tapback {
            reaction: Reaction::Like,
            removal: true
        })
    );
    assert!(matches!(a5.target, Col::Value(AssocTarget::Bubble { .. })));
    let a6 = m(6).association.as_ref().expect("emoji");
    assert_eq!(
        a6.kind,
        Some(AssociatedKind::Tapback {
            reaction: Reaction::Emoji,
            removal: false
        })
    );
    assert_eq!(text_of(&a6.emoji), Some(&b"SYNTH-EMOJI"[..]));
    assert_eq!(
        (a6.range_location.clone(), a6.range_length.clone()),
        (Col::Value(0), Col::Value(12))
    );
    let a18 = m(18).association.as_ref().expect("oversized target");
    assert_eq!(
        a18.target,
        Col::Oversized {
            bytes: u64::from(CAP_GUID) + 5
        }
    );
    assert_eq!(
        m(18).guid,
        Col::Oversized {
            bytes: u64::from(CAP_GUID) + 1
        }
    );
    let a19 = m(19).association.as_ref().expect("bare");
    assert!(matches!(a19.target, Col::Value(AssocTarget::Bare { .. })));
    let a20 = m(20).association.as_ref().expect("unknown");
    assert_eq!(a20.kind, Some(AssociatedKind::Unknown(2)));
    assert_eq!(a20.target, Col::Value(AssocTarget::Unparsed));

    // Replies; reply_to_guid alone is not a reply.
    assert!(m(7).reply.is_reply());
    assert_eq!(
        text_of(&m(7).reply.thread_originator_part),
        Some(&b"0:0:12"[..])
    );
    assert!(m(8).reply.is_reply());
    assert!(!m(9).reply.is_reply());
    assert!(m(9).reply.reply_to_guid.value().is_some());

    // Edit labels.
    assert_eq!(
        m(10).edit_label,
        EditLabel::EditHistory {
            parts: 1,
            max_entries: 2
        }
    );
    assert_eq!(m(11).edit_label, EditLabel::UnsentParts { parts: 1 });
    assert_eq!(
        m(11).summary.value().and_then(|s| s.otr.value().copied()),
        Some(1)
    );
    assert_eq!(m(12).edit_label, EditLabel::DateEditedNoBody);
    assert_eq!(m(13).summary, Col::Unreadable(IssueKind::PlistInvalid));
    assert_eq!(m(13).edit_label, EditLabel::SummaryUnreadable);
    assert_eq!(
        m(14).summary,
        Col::Oversized {
            bytes: u64::from(CAP_SUMMARY) + 1
        }
    );
    assert_eq!(m(14).edit_label, EditLabel::SummaryUnreadable);

    // Item kinds and the derived location direction.
    assert_eq!(m(15).item_kind, ItemKind::OwnNumberChanged);
    assert_eq!(m(16).item_kind, ItemKind::LegacySharedLocation);
    assert_eq!(m(16).from_me_derived_for_location, Some(true));
    assert_eq!(m(16).is_from_me, Col::Value(false), "the raw value is kept");
    assert_eq!(m(1).from_me_derived_for_location, None);
    assert_eq!(m(17).item_kind, ItemKind::GroupRenamed);
    assert_eq!(m(17).payload_data_type, Col::Value(SqlType::Blob));
    assert!(m(17).group_title.value().is_some());

    // Raw blobs are not kept by default.
    assert!(p
        .rows
        .values()
        .all(|r| r.raw_attributed_body.is_none() && r.raw_message_summary_info.is_none()));

    // Value issues: one invalid plist; three oversized values.
    assert_eq!(p.out.value_issues.plist_invalid, 1);
    assert_eq!(p.out.value_issues.value_oversized, 3);

    // GUID sets: 19 readable message GUIDs; one reply originator and one
    // association target that match none (the oversized one is not hashed).
    assert!(p.guids.complete());
    let c = p.guids.counts();
    assert_eq!(c.distinct_messages, Some(19));
    assert_eq!(c.duplicate_messages, Some(0));
    assert_eq!(c.reply_originator_not_found, Some(1));
    assert_eq!(c.association_target_not_found, Some(1));

    // Debug never shows content.
    for r in p.rows.values() {
        let shown = format!("{r:?} {r:#?}");
        for s in ["SYNTH", "synthetic-handle", "0:0:12", "\u{FFFC}"] {
            assert!(!shown.contains(s), "row {}: {s} in {shown}", r.rowid);
        }
    }
}

/// Decoding off and raw blobs kept.
#[test]
fn options_change_what_is_kept() {
    let opts = MessageReadOptions::default()
        .with_decode_attributed_body(false)
        .with_keep_raw_blobs(true);
    let p = run(&fixture(), &opts);
    let r1 = &p.rows[&1];
    assert_eq!(r1.body.attributed, AttributedOutcome::NotDecoded);
    assert!(r1
        .raw_attributed_body
        .as_ref()
        .is_some_and(|b| b.expose().len() > 12));
    assert!(p.rows[&10].raw_message_summary_info.is_some());
    // Over the cap: never read, so never kept.
    assert!(p.rows[&14].raw_message_summary_info.is_none());
    assert!(p.rows[&4].raw_attributed_body.is_none());
    // Edit labels do not depend on decoding.
    assert_eq!(p.rows[&12].edit_label, EditLabel::DateEditedNoBody);
}

/// A database with only the required columns: every optional field is
/// absent, nothing is invented.
#[test]
fn required_columns_only() {
    let db = build_with(|c: &Connection| {
        c.execute_batch(&minimal_schema()).expect("schema");
        c.execute(
            "INSERT INTO message (ROWID, guid, date, is_from_me) VALUES (1, 'SYNTH-MSG-1', 0, 1)",
            [],
        )
        .expect("insert");
    });
    let p = run(&db, &MessageReadOptions::default());
    let r = &p.rows[&1];
    assert_eq!(r.body.text_column, Col::Absent);
    assert_eq!(r.body.attributed, AttributedOutcome::Absent);
    assert_eq!(r.summary, Col::Absent);
    assert_eq!(r.edit_label, EditLabel::NotRecorded);
    assert_eq!(r.association, None);
    assert_eq!(r.handle_id, Col::Absent);
    assert_eq!(r.service_class, None);
    assert_eq!(
        r.item_kind,
        ItemKind::Unknown {
            item_type: Col::Absent,
            group_action_type: Col::Absent
        }
    );
    // date = 0 is reported as zero, never as "unset".
    assert_eq!(r.date.value().map(|d| d.interp), Some(TimeInterp::Zero));
    assert_eq!(r.is_from_me, Col::Value(true));
    let c = p.guids.clone().counts();
    assert_eq!(c.distinct_messages, Some(1));
}
