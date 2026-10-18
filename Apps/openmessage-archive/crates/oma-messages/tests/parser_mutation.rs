//! The seeded mutation harness of `typedstream_mutation.rs`, over the other
//! parsers of the message pass (M3 §19.2 item 3, D42):
//!
//! - `message_summary_info`, through the budgeted plist reader;
//! - `associated_message_guid` targets;
//! - the `value.rs` readers, the decoder and the record builder together,
//!   through SQLite: a database whose `message` columns are typeless holds
//!   mutated values of every storage class, and the pass must read every
//!   row without an error or a panic.
//!
//! 20,000 iterations for each pure parser by default; `OMA_TS_MUTATIONS`
//! changes it. Every input is synthetic.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use std::ops::ControlFlow;

use common::bplist::{Bp, Obj};
use common::mutate::{iterations, mutate, Rng};
use common::ts_writer::{self as tw, AttrVal};
use common::*;
use oma_messages::{
    AssocTarget, Col, Cursor, EditLabel, GuidSets, IssueKind, KnownTable, MessageReadOptions,
    Probe, ProbeOptions, SummaryInfo,
};
use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection};

fn summary_corpus() -> Vec<Vec<u8>> {
    let mut b = Bp::new();
    let empty = b.add(Obj::Dict(vec![]));
    let otr = b.dict(&[("0", empty), ("1", empty)]);
    let d = b.int(700_000_000_000_000_000);
    let r = b.add(Obj::Real(700_000_000.5));
    let dt = b.add(Obj::Date(700_000_000.0));
    let t = b.add(Obj::Data(b"SYNTH-EDIT".to_vec()));
    let e0 = b.dict(&[("d", d), ("t", t)]);
    let e1 = b.dict(&[("d", r)]);
    let e2 = b.dict(&[("d", dt), ("t", t)]);
    let p0 = b.add(Obj::Array(vec![e0, e1, e2]));
    let ec = b.dict(&[("0", p0), ("3", p0)]);
    let one = b.int(1);
    let rp = b.add(Obj::Array(vec![one, one]));
    let tmp = b.str("SYNTH-TRANSLATION");
    let root = b.dict(&[("otr", otr), ("ec", ec), ("rp", rp), ("tmp", tmp)]);
    let binary = b.finish(root);
    let xml = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict>\
<key>otr</key><dict><key>0</key><dict/></dict>\
<key>ec</key><dict><key>0</key><array><dict><key>d</key><real>7.5e8</real><key>t</key><data>AA==</data></dict></array></dict>\
<key>rp</key><array><integer>0</integer></array></dict></plist>"
        .to_vec();
    vec![binary, xml]
}

fn assoc_corpus() -> Vec<Vec<u8>> {
    [
        &b"p:0/SYNTH-GUID-0"[..],
        b"p:12/SYNTH-GUID-1",
        b"bp:SYNTH-GUID-2",
        b"SYNTH-GUID-3",
        b"p:4294967295/G",
    ]
    .iter()
    .map(|s| s.to_vec())
    .collect()
}

#[test]
fn summary_mutations() {
    let corpus = summary_corpus();
    for c in &corpus {
        SummaryInfo::parse(c).expect("the corpus parses");
    }
    let mut rng = Rng::new(0x5u64 << 40 | 0x3d);
    let n = iterations(20_000);
    let mut ok = 0usize;
    for _ in 0..n {
        let pick = rng.below(corpus.len());
        let input = mutate(&mut rng, &corpus[pick]);
        match SummaryInfo::parse(&input) {
            Ok(s) => {
                ok += 1;
                // Counts agree with the fields they summarise.
                assert_eq!(s.edited_parts().is_some(), s.ec.value().is_some());
                assert_eq!(s.unsent_parts().is_some(), s.rp.value().is_some());
            }
            Err(k) => assert!(
                matches!(
                    k,
                    IssueKind::PlistInvalid
                        | IssueKind::PlistOverBudget
                        | IssueKind::PlistNotDictionary
                ),
                "{k:?}"
            ),
        }
    }
    assert!(ok > 0, "some mutants still parse");
}

#[test]
fn assoc_mutations() {
    let corpus = assoc_corpus();
    let mut rng = Rng::new(0x000a_550c);
    for _ in 0..iterations(20_000) {
        let pick = rng.below(corpus.len());
        let input = mutate(&mut rng, &corpus[pick]);
        let t = AssocTarget::parse(&input);
        if let Some(g) = t.guid() {
            let g = g.expose().as_bytes();
            assert!(!g.is_empty() && !g.contains(&0));
            assert!(input.ends_with(g), "the GUID is the input's tail");
        }
        match t {
            AssocTarget::Part { part, .. } => {
                assert!(input.starts_with(b"p:"));
                let digits = &input[2..input.iter().position(|b| *b == b'/').expect("slash")];
                assert_eq!(
                    std::str::from_utf8(digits)
                        .ok()
                        .and_then(|d| d.parse::<u32>().ok()),
                    Some(part)
                );
            }
            AssocTarget::Bubble { .. } => assert!(input.starts_with(b"bp:")),
            AssocTarget::Bare { .. } => {
                assert!(!input.starts_with(b"p:") && !input.starts_with(b"bp:"));
            }
            _ => {}
        }
    }
}

/// Mutated values of every storage class in every `message` column, read
/// by the real pass.
#[test]
fn value_reader_mutations() {
    const ROWS: i64 = 1_500;
    let bodies: Vec<Vec<u8>> = vec![
        tw::attributed(&[("SYNTH-BODY".to_owned(), vec![(tw::PART, AttrVal::Int(0))])]).0,
        tw::attributed(&[
            ("SYNTH-".to_owned(), vec![(tw::PART, AttrVal::Int(0))]),
            (
                "\u{FFFC}".to_owned(),
                vec![
                    (tw::PART, AttrVal::Int(1)),
                    (tw::GUID, AttrVal::Str("SYNTH-AT".into())),
                ],
            ),
        ])
        .0,
    ];
    let summaries = summary_corpus();
    let assocs = assoc_corpus();
    let columns: Vec<&'static str> = KnownTable::Message
        .known_columns()
        .map(|c| c.name())
        .filter(|n| *n != "ROWID")
        .collect();

    let db = build_with(|c: &Connection| {
        let mut schema: String = [
            KnownTable::Handle,
            KnownTable::Chat,
            KnownTable::ChatMessageJoin,
            KnownTable::ChatHandleJoin,
        ]
        .iter()
        .map(|t| create_table(*t, true))
        .collect();
        let typeless: Vec<String> = columns.iter().map(|n| format!("\"{n}\"")).collect();
        schema.push_str(&format!(
            "CREATE TABLE message (\"ROWID\" INTEGER PRIMARY KEY, {});",
            typeless.join(", ")
        ));
        c.execute_batch(&schema).expect("schema");
        let marks: Vec<String> = (0..=columns.len()).map(|i| format!("?{}", i + 1)).collect();
        let sql = format!(
            "INSERT INTO message (\"ROWID\", {}) VALUES ({})",
            typeless.join(", "),
            marks.join(", ")
        );
        let mut st = c.prepare(&sql).expect("prepare");
        let mut rng = Rng::new(0x007a_15e5);
        for rowid in 1..=ROWS {
            let mut vals = vec![Value::Integer(rowid)];
            for name in &columns {
                let seed = match *name {
                    "attributedBody" => bodies[rng.below(bodies.len())].clone(),
                    "message_summary_info" => summaries[rng.below(summaries.len())].clone(),
                    "associated_message_guid" | "thread_originator_guid" => {
                        assocs[rng.below(assocs.len())].clone()
                    }
                    _ => format!("SYNTH-{name}-{rowid}").into_bytes(),
                };
                let v = match rng.below(7) {
                    0 => Value::Null,
                    1 => Value::Integer(match rng.below(4) {
                        0 => 0,
                        1 => 1,
                        2 => i64::MIN,
                        _ => rng.next() as i64,
                    }),
                    2 => Value::Real(match rng.below(4) {
                        0 => 1.0,
                        1 => f64::NAN,
                        2 => 1.5,
                        _ => f64::from_bits(rng.next()),
                    }),
                    3 => Value::Blob(mutate(&mut rng, &seed)),
                    4 => Value::Blob(seed),
                    // Text with arbitrary bytes (rusqlite binds a String, so
                    // invalid UTF-8 text is made with CAST below).
                    5 => Value::Text(String::from_utf8_lossy(&mutate(&mut rng, &seed)).into()),
                    _ => Value::Text(String::from_utf8_lossy(&seed).into()),
                };
                vals.push(v);
            }
            st.execute(params_from_iter(vals)).expect("insert");
        }
        // Some invalid UTF-8 stored as TEXT.
        c.execute(
            "UPDATE message SET guid = CAST(X'53ff59' AS TEXT), text = CAST(X'c0' AS TEXT) WHERE ROWID % 13 = 0",
            [],
        )
        .expect("update");
    });

    let fx = Fixture::with_sms(&db);
    with_probe(&fx, None, &ProbeOptions::default(), |p, ctx| {
        let Probe::Supported(db) = p.expect("probe") else {
            panic!("not supported");
        };
        for opts in [
            MessageReadOptions::default(),
            MessageReadOptions::default()
                .with_decode_attributed_body(false)
                .with_keep_raw_blobs(true),
        ] {
            let mut guids = GuidSets::new();
            let mut labels = 0u64;
            let out = db
                .for_each_message::<()>(Cursor::Start, &opts, Some(&mut guids), ctx, |m| {
                    // Every row gets exactly one label and keeps its raw codes.
                    let _: EditLabel = m.edit_label;
                    labels += 1;
                    if let Some(a) = &m.association {
                        assert!(!matches!(a.target, Col::Absent));
                    }
                    let _ = format!("{m:?}");
                    ControlFlow::Continue(())
                })
                .expect("the pass never fails on bad values");
            assert_eq!(out.rows, u64::try_from(ROWS).expect("small"));
            assert_eq!(labels, out.rows);
            assert!(out.value_issues.type_mismatch > 0);
            assert!(out.value_issues.invalid_utf8_text > 0);
            let _ = guids.counts();
        }
    })
    .expect("connected");
}
