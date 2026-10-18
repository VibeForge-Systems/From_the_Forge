//! Seeded mutation testing of the typedstream decoder (M3 §19.2 item 3).
//!
//! Every committed vector, plus a few writer outputs, is mutated 1–8 times
//! per iteration (bit flips, byte sets, truncation, insertion, duplicated
//! spans, inflated lengths) and decoded. 20,000 iterations by default;
//! `OMA_TS_MUTATIONS=<n>` changes it. For every input:
//!
//! - nothing panics;
//! - a failure is a fatal kind of the closed set;
//! - `TsStats` stays within the budget;
//! - a success is internally consistent: runs are contiguous from 0, on
//!   character boundaries and within the text; parts cover the runs; every
//!   index resolves; no run issue means the runs cover the whole text.
mod common;

use std::fs;
use std::path::PathBuf;

use common::mutate::{iterations, mutate, Rng};
use common::ts_writer::{self as tw, AttrVal};
use oma_messages::{decode_attributed_body_with_stats, AttributedBody, TsBudget};

fn corpus() -> Vec<Vec<u8>> {
    let dir = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/typedstream"
    ));
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut names: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "bin"))
        .collect();
    names.sort();
    for p in names {
        out.push(fs::read(p).expect("read"));
    }
    assert!(out.len() >= 80);
    let segs = vec![
        ("\u{1F600} ".to_owned(), vec![(tw::PART, AttrVal::Int(0))]),
        (
            "Alex".to_owned(),
            vec![
                (tw::PART, AttrVal::Int(0)),
                (tw::MENTION, AttrVal::Str("+15555550100".into())),
                (tw::BOLD, AttrVal::Int(1)),
            ],
        ),
        (
            "\u{FFFC}".to_owned(),
            vec![
                (tw::PART, AttrVal::Int(1)),
                (tw::GUID, AttrVal::Str("at_1".into())),
            ],
        ),
        (" ok".to_owned(), vec![(tw::PART, AttrVal::Int(0))]),
    ];
    out.push(tw::attributed(&segs).0);
    out
}

fn check_body(body: &AttributedBody) {
    let text = body.text.expose().as_str();
    let total16 = text.encode_utf16().count() as u32;
    let mut pos16 = 0u32;
    let mut pos8 = 0u32;
    for r in &body.runs {
        assert_eq!(r.utf16.start, pos16);
        assert_eq!(r.utf8.start, pos8);
        assert!(r.utf16.end >= r.utf16.start && r.utf16.end <= total16);
        assert!(text.is_char_boundary(r.utf8.end as usize));
        assert!((r.utf8.end as usize) <= text.len());
        assert!((r.attrs as usize) < body.attrs.len());
        pos16 = r.utf16.end;
        pos8 = r.utf8.end;
    }
    match body.run_issue {
        None => assert_eq!(pos16, total16, "no issue, so the runs cover the text"),
        Some(issue) => assert!(!issue.kind.is_fatal()),
    }
    let mut next = 0u32;
    for p in &body.parts {
        assert_eq!(p.runs.start, next);
        assert!(p.runs.end > p.runs.start && p.runs.end as usize <= body.runs.len());
        next = p.runs.end;
    }
    assert_eq!(next as usize, body.runs.len());
    for a in &body.attrs {
        for id in [a.mention, a.attachment_guid].into_iter().flatten() {
            assert!(id.index() < body.values.len());
        }
    }
}

#[test]
fn mutated_inputs_fail_closed_within_budget() {
    let corpus = corpus();
    let budget = TsBudget::default();
    let n = iterations(20_000);
    let mut rng = Rng::new(0x5EED_0000_7357_0001);
    let (mut ok, mut failed) = (0usize, 0usize);
    for i in 0..n {
        let seed = &corpus[rng.below(corpus.len())];
        let input = mutate(&mut rng, seed);
        let (r, s) = decode_attributed_body_with_stats(&input, &budget);
        let what = format!("iteration {i}");
        assert!(s.nodes <= budget.max_nodes(), "{what}");
        assert!(s.slots <= budget.max_objects(), "{what}");
        assert!(s.shared <= budget.max_shared(), "{what}");
        assert!(s.runs <= budget.max_runs(), "{what}");
        assert!(s.max_depth_seen <= budget.max_depth(), "{what}");
        assert!(
            s.bytes_materialised <= budget.max_materialised_bytes(input.len()),
            "{what}"
        );
        match r {
            Ok(body) => {
                check_body(&body);
                ok += 1;
            }
            Err(e) => {
                assert!(e.kind().is_fatal(), "{what}: {e:?}");
                assert!(
                    e.offset() <= input.len().max(budget.max_input() as usize),
                    "{what}"
                );
                failed += 1;
            }
        }
    }
    eprintln!("mutation: {n} iterations, {ok} decoded, {failed} failed closed");
    assert_eq!(ok + failed, n);
    if n >= 1_000 {
        assert!(ok > 0 && failed > 0, "ok {ok}, failed {failed}");
    }
}
