//! Typedstream vectors and hostile inputs (M3 §19.2, items 1 and 2).
//!
//! - Every committed vector in `tests/fixtures/typedstream` decodes, through
//!   the public API, to the `public` entry of `expected.json`, with
//!   `TsStats` within the budget.
//! - Hostile inputs too large to commit are built here with the test-only
//!   writer: 16 MiB + 1, zero-width type elements, one type string
//!   referenced a million times, 300,000 references to one object, and
//!   500,000 runs sharing one 8 MiB mention dictionary.
//! - The depth limit is tested on a thread with the std default stack, as
//!   the job thread is.
mod common;

use std::fs;
use std::path::PathBuf;

use common::ts_writer::{self as tw, AttrVal, W};
use oma_messages::{
    decode_attributed_body, decode_attributed_body_with_stats, AttrKey, AttributedBody, RunAttrs,
    SharedValue, TsBudget, TsErrorKind, TsStats, ValueId,
};
use serde_json::{json, Value};

fn dir() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/typedstream"
    ))
}

fn assert_within(stats: &TsStats, budget: &TsBudget, len: usize, what: &str) {
    assert!(stats.nodes <= budget.max_nodes(), "{what}: nodes {stats:?}");
    assert!(
        stats.slots <= budget.max_objects(),
        "{what}: slots {stats:?}"
    );
    assert!(
        stats.shared <= budget.max_shared(),
        "{what}: shared {stats:?}"
    );
    assert!(stats.runs <= budget.max_runs(), "{what}: runs {stats:?}");
    assert!(
        stats.max_depth_seen <= budget.max_depth(),
        "{what}: depth {stats:?}"
    );
    assert!(
        stats.bytes_materialised <= budget.max_materialised_bytes(len),
        "{what}: materialised {stats:?}"
    );
}

fn value_text(body: &AttributedBody, id: Option<ValueId>) -> Value {
    match id.and_then(|id| body.values.get(id.index())) {
        Some(SharedValue::Text(t)) => json!(t.expose().as_str()),
        Some(_) => json!("<other>"),
        None => Value::Null,
    }
}

fn attrs_json(body: &AttributedBody, a: &RunAttrs) -> Value {
    json!({
        "part": a.part_index,
        "guid": value_text(body, a.attachment_guid),
        "mention": value_text(body, a.mention),
        "flags": a.flags.iter().map(|f| f.name()).collect::<Vec<_>>(),
        "keys": a.keys_present.iter().map(AttrKey::name).collect::<Vec<_>>(),
        "text_effect": a.text_effect,
    })
}

/// The decoded body in the shape of `expected.json`'s `ok` entries.
fn body_json(body: &AttributedBody) -> Value {
    let runs: Vec<Value> = body
        .runs
        .iter()
        .map(|r| {
            let a = body.attrs[r.attrs as usize];
            json!({
                "utf16": [r.utf16.start, r.utf16.end],
                "utf8": [r.utf8.start, r.utf8.end],
                "attrs": attrs_json(body, &a),
            })
        })
        .collect();
    let parts: Vec<Value> = body
        .parts
        .iter()
        .map(|p| {
            json!({
                "part": p.part_index,
                "utf16": [p.utf16.start, p.utf16.end],
                "utf8": [p.utf8.start, p.utf8.end],
                "runs": [p.runs.start, p.runs.end],
                "guid": value_text(body, p.attachment_guid),
                "mention": value_text(body, p.mention),
                "text_effect": p.text_effect,
                "flags": p.flags.iter().map(|f| f.name()).collect::<Vec<_>>(),
                "keys": p.keys_present.iter().map(AttrKey::name).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "text": body.text.expose().as_str(),
        "runs": runs,
        "parts": parts,
        "run_issue": body.run_issue.map(|i| json!({ "kind": i.kind.name(), "run": i.run })),
        "unknown_attribute_keys": body.unknown_attribute_keys,
        "unknown_attribute_classes": body.unknown_attribute_classes,
        "unreadable_attribute_values": body.unreadable_attribute_values,
    })
}

#[test]
fn committed_vectors_decode_as_expected() {
    let doc: Value =
        serde_json::from_str(&fs::read_to_string(dir().join("expected.json")).expect("json"))
            .expect("parse");
    let vectors = doc["vectors"].as_array().expect("vectors");
    let budget = TsBudget::default();
    let mut ok = 0;
    for v in vectors {
        let name = v["name"].as_str().expect("name");
        let data = fs::read(dir().join(v["file"].as_str().expect("file"))).expect("read");
        let (got, stats) = decode_attributed_body_with_stats(&data, &budget);
        assert_within(&stats, &budget, data.len(), name);
        let public = &v["public"];
        match (public.get("ok"), got) {
            (Some(want), Ok(body)) => {
                assert_eq!(&body_json(&body), want, "{name}");
                ok += 1;
            }
            (Some(_), Err(e)) => panic!("{name}: {e:?}"),
            (None, Ok(_)) => panic!("{name}: expected {}", public["error"]),
            (None, Err(e)) => {
                assert_eq!(public["error"], e.kind().name(), "{name}");
                assert!(e.kind().is_fatal(), "{name}");
            }
        }
    }
    assert!(vectors.len() >= 80, "{}", vectors.len());
    assert!(ok >= 20, "{ok}");
}

/// The UTF-8 byte offset of a UTF-16 offset in `text`, computed here from
/// the text alone.
fn utf8_offset(text: &str, utf16: u32) -> Option<u32> {
    let mut u16s = 0u32;
    for (byte, ch) in text.char_indices() {
        if u16s == utf16 {
            return u32::try_from(byte).ok();
        }
        u16s += ch.len_utf16() as u32;
    }
    (u16s == utf16).then(|| u32::try_from(text.len()).ok())?
}

/// Oracle run 2 checks only `oracle.view` (the text, the raw (index, length)
/// runs and the dictionaries in stream order) against pytypedstream. This
/// ties the decoder's result to that oracle-checked view directly, rather
/// than only to `public`, which comes from the same generator model:
///
/// - the decoded text is the view's text;
/// - the view's run lengths cover the text's UTF-16 length exactly, and
///   every decoded run starts and ends on one of the view's run boundaries;
/// - each view run's known keys (the dictionary its index names, in order
///   of first appearance, T1) are the keys of the decoded run covering it;
/// - each decoded run's UTF-8 range is recomputed here from the text alone.
#[test]
fn the_oracle_checked_view_constrains_the_decoded_result() {
    let doc: Value =
        serde_json::from_str(&fs::read_to_string(dir().join("expected.json")).expect("json"))
            .expect("parse");
    let budget = TsBudget::default();
    let mut checked = 0;
    for v in doc["vectors"].as_array().expect("vectors") {
        let name = v["name"].as_str().expect("name");
        let oracle = &v["oracle"];
        if oracle["check"] != "compare" || oracle["view"]["kind"] != "attributed" {
            continue;
        }
        let view = &oracle["view"];
        let data = fs::read(dir().join(v["file"].as_str().expect("file"))).expect("read");
        let Ok(body) = decode_attributed_body(&data, &budget) else {
            continue;
        };
        if body.run_issue.is_some() {
            continue;
        }
        let text = body.text.expose().as_str();
        assert_eq!(text, view["text"].as_str().expect("text"), "{name}");

        let dicts: Vec<Vec<&'static str>> = view["dicts"]
            .as_array()
            .expect("dicts")
            .iter()
            .map(|d| {
                let mut keys: Vec<&'static str> = d
                    .as_array()
                    .expect("dict")
                    .iter()
                    .filter_map(|e| AttrKey::from_name(e[0].as_str().expect("key").as_bytes()))
                    .map(AttrKey::name)
                    .collect();
                keys.sort_unstable();
                keys.dedup();
                keys
            })
            .collect();
        let mut bounds = vec![0u32];
        let mut run_keys = Vec::new();
        for r in view["runs"].as_array().expect("runs") {
            let idx = r[0].as_u64().expect("index") as usize;
            let len = r[1].as_u64().expect("length") as u32;
            bounds.push(bounds.last().copied().unwrap_or(0) + len);
            run_keys.push(
                dicts
                    .get(idx - 1)
                    .expect("index names a dictionary")
                    .clone(),
            );
        }
        let total = text.encode_utf16().count() as u32;
        assert_eq!(bounds.last().copied(), Some(total), "{name}");

        let mut at = 0u32;
        for run in &body.runs {
            assert_eq!(run.utf16.start, at, "{name}: runs are contiguous");
            assert!(bounds.contains(&run.utf16.start), "{name}");
            assert!(bounds.contains(&run.utf16.end), "{name}");
            assert_eq!(
                Some(run.utf8.start..run.utf8.end),
                utf8_offset(text, run.utf16.start)
                    .zip(utf8_offset(text, run.utf16.end))
                    .map(|(a, b)| a..b),
                "{name}"
            );
            at = run.utf16.end;
        }
        assert_eq!(at, total, "{name}");
        for (i, keys) in run_keys.iter().enumerate() {
            let start = bounds[i];
            if bounds[i + 1] == start {
                continue;
            }
            let run = body
                .runs
                .iter()
                .find(|r| r.utf16.start <= start && start < r.utf16.end)
                .expect("a decoded run covers each view run");
            let mut got: Vec<&'static str> = body.attrs[run.attrs as usize]
                .keys_present
                .iter()
                .map(AttrKey::name)
                .collect();
            got.sort_unstable();
            assert_eq!(&got, keys, "{name}: run {i}");
        }
        checked += 1;
    }
    assert!(checked >= 10, "{checked}");
}

#[test]
fn every_vector_file_is_listed() {
    let doc: Value =
        serde_json::from_str(&fs::read_to_string(dir().join("expected.json")).expect("json"))
            .expect("parse");
    let listed: Vec<String> = doc["vectors"]
        .as_array()
        .expect("vectors")
        .iter()
        .map(|v| v["file"].as_str().expect("file").to_owned())
        .collect();
    for e in fs::read_dir(dir()).expect("dir").flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.ends_with(".bin") {
            assert!(listed.contains(&n), "{n} is not in expected.json");
        }
    }
}

#[test]
fn error_debug_and_display_omit_the_offset() {
    let mut data = fs::read(dir().join("h_trailing.bin")).expect("read");
    data.extend_from_slice(b"SYNTH-SENTINEL");
    let e = decode_attributed_body(&data, &TsBudget::default()).expect_err("trailing");
    assert_eq!(e.kind(), TsErrorKind::TrailingBytes);
    let shown = format!("{e:?} {e}");
    assert!(!shown.contains(&e.offset().to_string()), "{shown}");
    assert!(!shown.contains("SYNTH"), "{shown}");
}

#[test]
fn body_debug_shows_no_content() {
    let data = fs::read(dir().join("v4_mention_reuse.bin")).expect("read");
    let body = decode_attributed_body(&data, &TsBudget::default()).expect("decode");
    let shown = format!("{body:?}");
    for s in ["Hey", "Alex", "555", "__kIM"] {
        assert!(!shown.contains(s), "{s} in {shown}");
    }
}

// ---- nesting depth on the default stack ---------------------------------

/// An attributed string whose one dictionary value nests objects so the
/// innermost is at `depth`.
fn nested_objects(depth: usize) -> Vec<u8> {
    let mut w = W::new();
    w.begin_attributed(b"x");
    w.run(1, 1);
    w.types(b"@");
    w.begin_obj(tw::NSDICT);
    w.types(b"i");
    w.int(1);
    w.nsstring(tw::LINK.as_bytes());
    for _ in 0..depth - 2 {
        w.types(b"@");
        w.begin_obj(tw::UNKNOWN);
    }
    w.types(b"i");
    w.int(0);
    for _ in 0..depth - 2 {
        w.end_obj();
    }
    w.end_obj();
    w.end_obj();
    w.b
}

fn nested_structs(n: usize) -> Vec<u8> {
    let mut w = W::new();
    w.types(b"@");
    w.begin_obj(tw::UNKNOWN);
    let t = format!("{}i{}", "{a=".repeat(n), "}".repeat(n));
    w.types(t.as_bytes());
    w.int(7);
    w.end_obj();
    w.b
}

#[test]
fn depth_limit_on_a_default_stack_thread() {
    let h = std::thread::spawn(|| {
        let b = TsBudget::default();
        let (ok, s) = decode_attributed_body_with_stats(&nested_objects(64), &b);
        assert!(ok.is_ok(), "{ok:?}");
        assert_eq!(s.max_depth_seen, 64);
        let deep = decode_attributed_body(&nested_objects(65), &b).expect_err("65");
        assert_eq!(deep.kind(), TsErrorKind::TooDeep);
        // An object at depth 1 holding 63 nested structs reaches 64.
        let (r, s) = decode_attributed_body_with_stats(&nested_structs(63), &b);
        assert_eq!(
            r.expect_err("not attributed").kind(),
            TsErrorKind::NotAttributedString
        );
        assert_eq!(s.max_depth_seen, 64);
        let r = decode_attributed_body(&nested_structs(64), &b);
        assert_eq!(r.expect_err("65").kind(), TsErrorKind::TooDeep);
        // A tighter budget is honoured.
        let r = decode_attributed_body(&nested_objects(8), &b.with_max_depth(7));
        assert_eq!(r.expect_err("tight").kind(), TsErrorKind::TooDeep);
    });
    h.join().expect("no stack overflow");
}

// ---- budgets -------------------------------------------------------------

#[test]
fn input_over_16_mib_is_refused_before_reading() {
    let data = vec![0u8; (16 << 20) + 1];
    let (r, s) = decode_attributed_body_with_stats(&data, &TsBudget::default());
    assert_eq!(r.expect_err("big").kind(), TsErrorKind::Budget);
    assert_eq!(s, TsStats::default());
}

/// A top-level group whose type is `n` copies of `elem`, referenced again
/// and again: each read charges `n` nodes although it reads no bytes.
fn zero_width(elem: &[u8], n: usize, reads: usize) -> Vec<u8> {
    let mut w = W::new();
    w.types(b"@");
    w.begin_obj(tw::BULK);
    let t = elem.repeat(n);
    for _ in 0..reads {
        w.types(&t);
    }
    w.end_obj();
    w.b
}

#[test]
fn zero_width_elements_are_charged() {
    let b = TsBudget::default();
    for elem in [&b"[0c]"[..], b"{}"] {
        let data = zero_width(elem, 100_000, 11);
        let (r, s) = decode_attributed_body_with_stats(&data, &b);
        assert_eq!(r.expect_err("budget").kind(), TsErrorKind::Budget);
        assert_within(&s, &b, data.len(), "zero width");
        assert_eq!(s.type_parses, 2, "{s:?}");
        // Ten reads fit.
        let data = zero_width(elem, 100_000, 10);
        let (r, s) = decode_attributed_body_with_stats(&data, &b);
        assert_eq!(
            r.expect_err("not attributed").kind(),
            TsErrorKind::NotAttributedString
        );
        assert_eq!(s.nodes, 1_000_001, "ten reads plus the root value");
    }
}

#[test]
fn one_type_string_referenced_a_million_times_is_parsed_once() {
    let mut w = W::new();
    w.begin_attributed(b"x");
    w.run(1, 1);
    w.types(b"@");
    w.begin_obj(tw::NSDICT);
    w.types(b"i");
    w.int(1);
    w.nsstring(tw::LINK.as_bytes());
    w.types(b"@");
    w.begin_obj(tw::BULK);
    for _ in 0..1_000_000 {
        w.types(b"i");
        w.int(0);
    }
    w.end_obj();
    w.end_obj();
    w.end_obj();
    let b = TsBudget::default();
    let (r, s) = decode_attributed_body_with_stats(&w.b, &b);
    let body = r.expect("decodes");
    assert_eq!(body.unknown_attribute_classes, 1);
    assert_within(&s, &b, w.b.len(), "1M refs");
    assert!(s.type_parses <= 8, "{s:?}");
    assert!(s.nodes > 1_000_000, "{s:?}");
}

#[test]
fn many_references_to_one_object_are_interpreted_once() {
    // One dictionary of 150,000 entries: every key is a reference to one
    // mention-key string and every value a reference to one 4 MiB string
    // (300,000 references in all). The first entry wins.
    let big = vec![b'm'; 4 << 20];
    let mut w = W::new();
    w.begin_attributed(b"Hi");
    w.run(1, 2);
    w.types(b"@");
    w.begin_obj(tw::NSDICT);
    w.types(b"i");
    w.int(150_000);
    let key = w.nsstring(tw::MENTION.as_bytes());
    let value = w.nsstring(&big);
    for _ in 1..150_000 {
        w.types(b"@");
        w.reference(key);
        w.types(b"@");
        w.reference(value);
    }
    w.end_obj();
    w.end_obj();
    let b = TsBudget::default();
    let (r, s) = decode_attributed_body_with_stats(&w.b, &b);
    let body = r.expect("decodes");
    assert_eq!(body.values.len(), 1);
    assert!(body.runs[0..1]
        .iter()
        .all(|r| body.attrs[r.attrs as usize].mention.is_some()));
    assert_within(&s, &b, w.b.len(), "300k refs");
    assert!(s.bytes_materialised < (4 << 20) + 4096, "{s:?}");
}

#[test]
fn many_runs_share_one_large_mention_dictionary() {
    // 1M runs (as M3 §19.2 lists) would need 2M nodes, over `max_nodes`
    // (each run is an `iI` group of two values), so 500,000 runs are used:
    // the most that fit the default budget.
    const RUNS: usize = 500_000;
    let mention = vec![b'+'; 8 << 20];
    let mut w = W::new();
    w.begin_attributed(&vec![b'x'; RUNS]);
    w.run(1, 1);
    w.dict(&[
        (tw::PART, AttrVal::Int(0)),
        (
            tw::MENTION,
            AttrVal::Str(String::from_utf8(mention).expect("ascii")),
        ),
    ]);
    for _ in 1..RUNS {
        w.run(1, 1);
    }
    w.end_obj();
    let b = TsBudget::default();
    let (r, s) = decode_attributed_body_with_stats(&w.b, &b);
    let body = r.expect("decodes");
    assert_eq!(s.runs as usize, RUNS);
    assert_eq!(
        body.runs.len(),
        1,
        "consecutive runs of one dictionary merge"
    );
    assert_eq!(body.runs[0].utf16, 0..RUNS as u32);
    assert_eq!(body.values.len(), 1);
    assert_eq!(body.run_issue, None);
    assert_within(&s, &b, w.b.len(), "500k runs");
    assert!(
        s.bytes_materialised < (8 << 20) + RUNS as u64 + 4096,
        "{s:?}"
    );
}

#[test]
fn run_and_materialisation_budgets_are_fatal() {
    // Alternating dictionaries: every run is kept, so records add up.
    let mut segs = Vec::new();
    for i in 0..200 {
        segs.push(("ab".to_owned(), vec![(tw::PART, AttrVal::Int(i % 2))]));
    }
    let (data, _) = tw::attributed(&segs);
    let b = TsBudget::default();
    assert!(decode_attributed_body(&data, &b).is_ok());
    let r = decode_attributed_body(&data, &b.with_max_runs(199));
    assert_eq!(r.expect_err("runs").kind(), TsErrorKind::Budget);
    let tight = b.with_materialised_factor(1).with_materialised_floor(1);
    let (r, s) = decode_attributed_body_with_stats(&data, &tight);
    assert_eq!(
        r.expect_err("materialised").kind(),
        TsErrorKind::MaterialisationBudget
    );
    assert_within(&s, &tight, data.len(), "tight");
    let r = decode_attributed_body(&data, &b.with_max_objects(5));
    assert_eq!(r.expect_err("slots").kind(), TsErrorKind::Budget);
    let r = decode_attributed_body(&data, &b.with_max_shared(5));
    assert_eq!(r.expect_err("shared").kind(), TsErrorKind::Budget);
    let r = decode_attributed_body(&data, &b.with_max_input(100));
    assert_eq!(r.expect_err("input").kind(), TsErrorKind::Budget);
}
