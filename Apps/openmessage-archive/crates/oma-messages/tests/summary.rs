//! `message_summary_info` shapes and bombs (M3 §11.3, §19.1 `summary`).
//!
//! Binary property lists are built with the test-only writer in
//! `common/bplist.rs`, which can share one object among many references;
//! XML ones are written by hand. Every value is synthetic.
mod common;

use common::bplist::{Bp, Obj};
use oma_messages::{
    FieldOutcome, IssueKind, ShapeError, SummaryInfo, SummaryTimeRaw, TimeInterp,
    SUMMARY_ARRAY_CAP, SUMMARY_MAX_DEPTH,
};

/// A summary with `otr`, `ec` (two parts), `rp` and `tmp`, binary.
fn full_binary() -> Vec<u8> {
    let mut b = Bp::new();
    let empty = b.add(Obj::Dict(vec![]));
    let otr = b.dict(&[("0", empty), ("1", empty)]);
    let d_int = b.int(700_000_000_000_000_000);
    let d_real = b.add(Obj::Real(700_000_100.25));
    let d_date = b.add(Obj::Date(700_000_200.0));
    let body = b.add(Obj::Data(b"SYNTH-EDIT-0".to_vec()));
    let e0 = b.dict(&[("d", d_int), ("t", body)]);
    let e1 = b.dict(&[("d", d_real), ("t", body)]);
    let e2 = b.dict(&[("d", d_date)]);
    let p0 = b.add(Obj::Array(vec![e0, e1]));
    let p1 = b.add(Obj::Array(vec![e2]));
    let ec = b.dict(&[("0", p0), ("1", p1)]);
    let one = b.int(1);
    let rp = b.add(Obj::Array(vec![one]));
    let tmp_text = b.str("SYNTH-TRANSLATION");
    let tmp = b.dict(&[("0", tmp_text)]);
    let yes = b.add(Obj::Bool(true));
    let root = b.dict(&[
        ("otr", otr),
        ("ec", ec),
        ("rp", rp),
        ("tmp", tmp),
        ("euh", yes),
    ]);
    b.finish(root)
}

const FULL_XML: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<plist version=\"1.0\"><dict>\
<key>otr</key><dict><key>0</key><dict/><key>1</key><dict/></dict>\
<key>ec</key><dict>\
<key>0</key><array>\
<dict><key>d</key><integer>700000000000000000</integer><key>t</key><data>U1lOVEgtRURJVC0w</data></dict>\
<dict><key>d</key><real>700000100.25</real><key>t</key><data>U1lOVEgtRURJVC0w</data></dict>\
</array>\
<key>1</key><array><dict><key>d</key><date>2023-03-08T20:30:00Z</date></dict></array>\
</dict>\
<key>rp</key><array><integer>1</integer></array>\
<key>tmp</key><dict><key>0</key><string>SYNTH-TRANSLATION</string></dict>\
<key>euh</key><true/>\
</dict></plist>";

/// XML and binary forms of the same summary read the same.
#[test]
fn xml_and_binary_agree() {
    let bin = SummaryInfo::parse(&full_binary()).expect("binary");
    let xml = SummaryInfo::parse(FULL_XML.as_bytes()).expect("xml");
    assert_eq!(bin, xml);
    assert_eq!(bin.otr, FieldOutcome::Value(2));
    assert_eq!(bin.rp, FieldOutcome::Value(vec![1]));
    assert!(bin.has_tmp);
    assert_eq!(bin.unknown_keys, 1);
    let ec = bin.ec.value().expect("ec");
    assert_eq!(ec.len(), 2);
    assert_eq!((ec[0].part, ec[0].entries.len()), (0, 2));
    assert_eq!((ec[1].part, ec[1].entries.len()), (1, 1));
    let d = ec[0].entries[0].at.expect("d");
    assert_eq!(d.raw, SummaryTimeRaw::Integer(700_000_000_000_000_000));
    assert!(matches!(d.interp, TimeInterp::Nanoseconds { .. }));
    let d = ec[0].entries[1].at.expect("d");
    assert!(matches!(d.interp, TimeInterp::Seconds { .. }));
    let d = ec[1].entries[0].at.expect("d");
    assert_eq!(d.raw, SummaryTimeRaw::Date);
    // 700,000,200 s after 2001-01-01 is 2023-03-08T20:30:00Z.
    assert_eq!(
        d.unix_ns(),
        Some((978_307_200 + 700_000_200) * 1_000_000_000)
    );
    assert!(ec[0].entries[0].body_present && !ec[1].entries[0].body_present);
    let shown = format!("{bin:?}");
    assert!(!shown.contains("SYNTH"), "{shown}");
}

/// Wrong types at the root and at every level of each field, in binary.
#[test]
fn wrong_types_at_every_level() {
    let mut b = Bp::new();
    let s = b.str("x");
    let i = b.int(1);
    let neg = b.int(-1);
    let arr_s = b.add(Obj::Array(vec![s]));
    let arr_i = b.add(Obj::Array(vec![i]));
    let empty_d = b.add(Obj::Dict(vec![]));
    let bad_entry = b.dict(&[("d", s)]);
    let arr_bad_entry = b.add(Obj::Array(vec![bad_entry]));
    let arr_neg = b.add(Obj::Array(vec![neg]));
    let cases: Vec<(&str, usize, &str)> = vec![
        ("otr", s, "otr"),
        ("otr", arr_i, "otr"),
        ("ec", i, "ec"),
        ("ec", arr_s, "ec"),
        ("rp", empty_d, "rp"),
        ("rp", arr_s, "rp"),
        ("rp", arr_neg, "rp"),
    ];
    for (key, value, bad) in cases {
        let root = b.dict(&[(key, value)]);
        let info = SummaryInfo::parse(&b.finish(root)).expect("parses");
        for (name, is_bad) in [
            ("otr", info.otr.is_bad()),
            ("ec", info.ec.is_bad()),
            ("rp", info.rp.is_bad()),
        ] {
            assert_eq!(is_bad, name == bad, "{key}: {name}");
        }
    }
    // An entry with a wrong-typed `d` spoils `ec` only.
    let p = b.dict(&[("0", arr_bad_entry)]);
    let ok_rp = b.add(Obj::Array(vec![i]));
    let root = b.dict(&[("ec", p), ("rp", ok_rp)]);
    let info = SummaryInfo::parse(&b.finish(root)).expect("parses");
    assert_eq!(info.ec, FieldOutcome::Bad(ShapeError::WrongType));
    assert_eq!(info.rp, FieldOutcome::Value(vec![1]));

    for root_kind in [Obj::Array(vec![]), Obj::Int(3), Obj::Ascii("x".into())] {
        let mut b = Bp::new();
        let r = b.add(root_kind);
        assert_eq!(
            SummaryInfo::parse(&b.finish(r)),
            Err(IssueKind::PlistNotDictionary)
        );
    }
}

/// Depth: 16 levels read, 17 do not.
#[test]
fn depth_limit() {
    let nest = |levels: u32| {
        let mut b = Bp::new();
        // The root dictionary is level 1; arrays fill the rest.
        let mut inner = b.add(Obj::Array(vec![]));
        for _ in 2..levels {
            inner = b.add(Obj::Array(vec![inner]));
        }
        let root = b.dict(&[("SYNTH-UNKNOWN", inner)]);
        SummaryInfo::parse(&b.finish(root))
    };
    assert!(nest(SUMMARY_MAX_DEPTH).is_ok());
    assert_eq!(nest(SUMMARY_MAX_DEPTH + 1), Err(IssueKind::PlistOverBudget));
}

/// The array cap: 4096 elements read, 4097 do not.
#[test]
fn array_cap() {
    let wide = |n: u32| {
        let mut b = Bp::new();
        let one = b.int(1);
        let arr = b.add(Obj::Array(vec![one; n as usize]));
        let root = b.dict(&[("rp", arr)]);
        SummaryInfo::parse(&b.finish(root))
    };
    let ok = wide(SUMMARY_ARRAY_CAP).expect("at the cap");
    assert_eq!(ok.unsent_parts(), Some(SUMMARY_ARRAY_CAP));
    assert_eq!(wide(SUMMARY_ARRAY_CAP + 1), Err(IssueKind::PlistOverBudget));
}

/// A shared-reference bomb: each level is an array of 64 references to the
/// level below, so a few kilobytes describe 64^4 leaves. The event budget
/// stops it.
#[test]
fn event_bomb() {
    let mut b = Bp::new();
    let mut level = b.add(Obj::Data(b"SYNTH-EDIT-BOMB".to_vec()));
    for _ in 0..4 {
        level = b.add(Obj::Array(vec![level; 64]));
    }
    let root = b.dict(&[("SYNTH-UNKNOWN", level)]);
    let bytes = b.finish(root);
    assert!(bytes.len() < 4096, "{}", bytes.len());
    assert_eq!(SummaryInfo::parse(&bytes), Err(IssueKind::PlistOverBudget));

    // The same bomb under `ec`, built from entries.
    let mut b = Bp::new();
    let body = b.add(Obj::Data(vec![0x5a; 1024]));
    let e = b.dict(&[("t", body)]);
    let part = b.add(Obj::Array(vec![e; 2048]));
    let mut parts = Vec::new();
    for k in 0..64 {
        parts.push((k.to_string(), part));
    }
    let pairs: Vec<(&str, usize)> = parts.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let ec = b.dict(&pairs);
    let root = b.dict(&[("ec", ec)]);
    assert_eq!(
        SummaryInfo::parse(&b.finish(root)),
        Err(IssueKind::PlistOverBudget)
    );
}

#[test]
fn not_a_plist() {
    for bad in [
        &b""[..],
        b"bplist00",
        b"bplist00\0\0\0",
        b"<plist>",
        b"\xff\xfe",
    ] {
        assert_eq!(
            SummaryInfo::parse(bad),
            Err(IssueKind::PlistInvalid),
            "{bad:?}"
        );
    }
}
