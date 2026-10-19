//! The shared hidden-character vectors (M6 spec §4.7, D121, D138):
//! `tests/fixtures/export/hidden-vectors.json` (slice 6c) against the
//! Rust classifier. The Vitest `hidden-vectors.spec.ts` checks the same
//! file against "Copy text without invisible characters", so the export's
//! markers and the copy stay each other's inverse.

use std::path::PathBuf;

use oma_export::hidden::{
    classify, mark, marked_code_points, marker, without_hidden, CharClass, MarkPolicy, MARKER_OPEN,
};
use serde_json::Value;

fn vectors() -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("export")
        .join("hidden-vectors.json");
    let doc: Value = serde_json::from_slice(&std::fs::read(path).expect("read")).expect("json");
    assert_eq!(doc["format"], "oma-hidden-vectors");
    doc["vectors"].as_array().expect("vectors").clone()
}

#[test]
fn the_classifier_matches_every_vector() {
    let vs = vectors();
    assert!(vs.len() >= 30);
    for v in &vs {
        let name = v["name"].as_str().expect("name");
        let text = v["text"].as_str().expect("text");
        let want: Vec<(String, String)> = v["pieces"]
            .as_array()
            .expect("pieces")
            .iter()
            .map(|p| {
                (
                    p["class"].as_str().expect("class").to_owned(),
                    p["text"].as_str().expect("text").to_owned(),
                )
            })
            .collect();
        let got: Vec<(String, String)> = classify(text)
            .into_iter()
            .map(|p| (p.class.name().to_owned(), text[p.start..p.end].to_owned()))
            .collect();
        assert_eq!(got, want, "{name}: pieces");
        assert_eq!(
            without_hidden(text),
            v["withoutHiddenCharacters"].as_str().expect("copy"),
            "{name}: copy"
        );
        let marked: Vec<String> = v["markedCodePoints"]
            .as_array()
            .expect("marked")
            .iter()
            .map(|c| c.as_str().expect("cp").to_owned())
            .collect();
        assert_eq!(marked_code_points(text), marked, "{name}: marked");
    }
}

#[test]
fn text_markers_replace_exactly_the_marked_characters() {
    for v in vectors() {
        let name = v["name"].as_str().expect("name");
        let text = v["text"].as_str().expect("text");
        let (out, n) = mark(text, MarkPolicy::text_body(true));
        // What is left once the markers are taken out is the copy, with
        // CR (a control in the text formats) gone too, and a literal ⟨
        // escaped.
        let mut rebuilt = String::new();
        for p in classify(text) {
            let s = &text[p.start..p.end];
            if p.class == CharClass::Keep {
                for c in s.chars() {
                    if c == MARKER_OPEN {
                        rebuilt.push_str(&marker(c));
                    } else {
                        rebuilt.push(c);
                    }
                }
            } else {
                for c in s.chars() {
                    rebuilt.push_str(&marker(c));
                }
            }
        }
        if !name.starts_with("combining") {
            assert_eq!(out, rebuilt, "{name}");
        }
        let hidden = classify(text)
            .iter()
            .filter(|p| p.class != CharClass::Keep)
            .map(|p| text[p.start..p.end].chars().count() as u64)
            .sum::<u64>();
        assert_eq!(n.hidden_marked + n.controls_escaped, hidden, "{name}");
        // A header field never breaks a line.
        let (h, _) = mark(text, MarkPolicy::text_header());
        assert!(
            !h.contains(['\n', '\r', '\u{2028}', '\u{2029}', '\u{85}']),
            "{name}: header"
        );
    }
}
