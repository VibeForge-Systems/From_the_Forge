//! The generic (layer 1) half of the committed vectors: integer heads,
//! references, `NSData`, arrays and structs, checked against the
//! `generic` entries of `tests/fixtures/typedstream/expected.json`. The
//! public half is in `tests/typedstream_vectors.rs`.
#![allow(clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};

use super::foundation::{self, Kind};
use super::node::{walk_all, Doc, Val};
use super::stats::Meter;
use super::TsBudget;

fn dir() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/typedstream"
    ))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn render(doc: &Doc<'_>, v: Val) -> Value {
    match v {
        Val::Signed(n) => json!({ "int": n }),
        Val::Unsigned(n) => json!({ "int": n }),
        Val::Bool(n) => json!({ "int": n }),
        Val::Real(x) => json!({ "real": x }),
        Val::Bytes(s) => json!({ "bytes": hex(doc.span(s).expect("span")) }),
        Val::Object(None) => json!({ "nil": true }),
        Val::Object(Some(o)) => match foundation::kind(doc, o) {
            Kind::String => json!({
                "nsstring": std::str::from_utf8(foundation::string_bytes(doc, o).expect("string"))
                    .expect("utf-8")
            }),
            Kind::Data => json!({ "nsdata": hex(foundation::data_bytes(doc, o).expect("data")) }),
            k => json!({ "object": format!("{k:?}") }),
        },
        other => json!({ "other": format!("{other:?}") }),
    }
}

#[test]
fn generic_vectors_match_expected() {
    let doc: Value =
        serde_json::from_str(&fs::read_to_string(dir().join("expected.json")).expect("json"))
            .expect("parse");
    let mut checked = 0;
    for v in doc["vectors"].as_array().expect("vectors") {
        let generic = &v["generic"];
        if generic.is_null() {
            continue;
        }
        let name = v["name"].as_str().expect("name");
        let data = fs::read(dir().join(v["file"].as_str().expect("file"))).expect("read");
        let budget = TsBudget::default();
        let mut meter = Meter::new(&budget, data.len());
        let got = walk_all(&data, &mut meter);
        match (generic.get("error"), got) {
            (Some(kind), Err(e)) => assert_eq!(kind, e.kind().name(), "{name}"),
            (Some(kind), Ok(_)) => panic!("{name}: expected {kind}"),
            (None, Err(e)) => panic!("{name}: {e:?}"),
            (None, Ok((d, tops))) => {
                let groups: Vec<Value> = tops
                    .iter()
                    .map(|g| {
                        let g = d.group(*g).expect("group");
                        Value::Array(
                            d.values(g)
                                .expect("values")
                                .iter()
                                .map(|x| render(&d, *x))
                                .collect(),
                        )
                    })
                    .collect();
                assert_eq!(&Value::Array(groups), &generic["groups"], "{name}");
            }
        }
        checked += 1;
    }
    assert!(checked >= 31 + 6 + 3, "only {checked} generic vectors");
}
