//! Round trip through the test-only writer (M3 §19.2 item 4).
//!
//! Random attributed strings (non-BMP characters, skin-tone sequences,
//! U+FFFC, run splits that respect surrogates, dictionary reuse, and 0–5
//! known attributes) are written by `tests/common/ts_writer.rs` and
//! decoded. The decoder must give back the same text, runs and attributes.
//! Writer and decoder come from the same reading of the format, so this
//! shows internal consistency only; the trust anchors are the published
//! sample and oracle 2 (see `tests/fixtures/typedstream/README.md`).
mod common;

use common::mutate::Rng;
use common::ts_writer::{self as tw, AttrVal, Attrs};
use oma_messages::{
    decode_attributed_body, AttrFlag, AttrKey, AttributedBody, SharedValue, TsBudget,
};

const CHARS: [&str; 9] = [
    "a",
    "Z",
    " ",
    "\u{e9}",
    "\u{20ac}",
    "\u{FFFC}",
    "\u{1F600}",
    "\u{1F44B}\u{1F3FD}",
    "\u{1F1FA}\u{1F1F8}",
];

fn random_attrs(rng: &mut Rng, n: usize) -> Attrs {
    let mut pool: Vec<(&'static str, AttrVal)> = vec![
        (tw::PART, AttrVal::Int(rng.below(3) as i64)),
        (
            tw::MENTION,
            AttrVal::Str(format!("+1555555010{}", rng.below(10))),
        ),
        (
            tw::GUID,
            AttrVal::Str(format!("at_{}_SYNTH", rng.below(100))),
        ),
        (tw::BOLD, AttrVal::Int(rng.below(2) as i64)),
        (tw::EFFECT, AttrVal::Int(rng.below(12) as i64)),
        (tw::LINK, AttrVal::Unknown),
    ];
    let mut out = Vec::new();
    for _ in 0..n.min(pool.len()) {
        let i = rng.below(pool.len());
        out.push(pool.swap_remove(i));
    }
    out
}

#[derive(Debug, PartialEq)]
struct Expect {
    utf16: (u32, u32),
    utf8: (u32, u32),
    part: Option<i64>,
    mention: Option<String>,
    guid: Option<String>,
    bold: bool,
    effect: Option<i64>,
    keys: Vec<&'static str>,
}

fn expect_attrs(a: &Attrs, utf16: (u32, u32), utf8: (u32, u32)) -> Expect {
    let get = |k: &str| a.iter().find(|(kk, _)| *kk == k).map(|(_, v)| v.clone());
    let int = |k: &str| match get(k) {
        Some(AttrVal::Int(n)) => Some(n),
        _ => None,
    };
    let text = |k: &str| match get(k) {
        Some(AttrVal::Str(s)) => Some(s),
        _ => None,
    };
    let mut keys: Vec<&'static str> = AttrKey::ALL
        .iter()
        .map(|k| k.name())
        .filter(|n| a.iter().any(|(k, _)| k == n))
        .collect();
    keys.sort_unstable();
    Expect {
        utf16,
        utf8,
        part: int(tw::PART),
        mention: text(tw::MENTION),
        guid: text(tw::GUID),
        bold: int(tw::BOLD).is_some_and(|n| n != 0),
        effect: int(tw::EFFECT),
        keys,
    }
}

fn text_of(body: &AttributedBody, id: Option<oma_messages::ValueId>) -> Option<String> {
    match body.values.get(id?.index())? {
        SharedValue::Text(t) => Some(t.expose().to_string()),
        _ => None,
    }
}

fn actual(body: &AttributedBody) -> Vec<Expect> {
    body.runs
        .iter()
        .map(|r| {
            let a = body.attrs[r.attrs as usize];
            let mut keys: Vec<&'static str> = a.keys_present.iter().map(AttrKey::name).collect();
            keys.sort_unstable();
            Expect {
                utf16: (r.utf16.start, r.utf16.end),
                utf8: (r.utf8.start, r.utf8.end),
                part: a.part_index,
                mention: text_of(body, a.mention),
                guid: text_of(body, a.attachment_guid),
                bold: a.flags.contains(AttrFlag::Bold),
                effect: a.text_effect,
                keys,
            }
        })
        .collect()
}

#[test]
fn random_attributed_strings_round_trip() {
    let mut rng = Rng::new(0x5EED_0000_0000_2222);
    let budget = TsBudget::default();
    for iter in 0..500 {
        let sets: Vec<Attrs> = (0..1 + rng.below(3))
            .map(|_| {
                let n = rng.below(6);
                random_attrs(&mut rng, n)
            })
            .collect();
        let mut segs: Vec<(String, Attrs)> = Vec::new();
        for _ in 0..rng.below(9) {
            let t: String = (0..rng.below(6))
                .map(|_| CHARS[rng.below(CHARS.len())])
                .collect();
            segs.push((t, sets[rng.below(sets.len())].clone()));
        }
        let (blob, numbers) = tw::attributed(&segs);
        // The model: consecutive segments of one dictionary merge.
        let mut want: Vec<(usize, Expect)> = Vec::new();
        let (mut p16, mut p8) = (0u32, 0u32);
        for ((t, attrs), n) in segs.iter().zip(&numbers) {
            let (e16, e8) = (p16 + tw::utf16_len(t) as u32, p8 + t.len() as u32);
            match want.last_mut() {
                Some((last, e)) if *last == *n => {
                    e.utf16.1 = e16;
                    e.utf8.1 = e8;
                }
                _ => want.push((*n, expect_attrs(attrs, (p16, e16), (p8, e8)))),
            }
            (p16, p8) = (e16, e8);
        }
        let body = decode_attributed_body(&blob, &budget)
            .unwrap_or_else(|e| panic!("iteration {iter}: {e:?}"));
        let text: String = segs.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(body.text.expose().as_str(), text, "iteration {iter}");
        assert_eq!(body.run_issue, None, "iteration {iter}");
        let want: Vec<Expect> = want.into_iter().map(|(_, e)| e).collect();
        assert_eq!(actual(&body), want, "iteration {iter}");
        // Parts: consecutive runs with the same part index.
        let mut parts: Vec<(Option<i64>, u32, u32)> = Vec::new();
        for e in &want {
            match parts.last_mut() {
                Some(p) if p.0 == e.part => p.2 = e.utf16.1,
                _ => parts.push((e.part, e.utf16.0, e.utf16.1)),
            }
        }
        let got: Vec<(Option<i64>, u32, u32)> = body
            .parts
            .iter()
            .map(|p| (p.part_index, p.utf16.start, p.utf16.end))
            .collect();
        assert_eq!(got, parts, "iteration {iter}");
        // Each dictionary is interpreted once: the unknown-class link value
        // is counted once per distinct dictionary that holds it.
        let mut dicts: Vec<usize> = numbers.clone();
        dicts.sort_unstable();
        dicts.dedup();
        let links = dicts
            .iter()
            .filter(|n| {
                let first = numbers.iter().position(|m| m == *n).expect("used");
                segs[first].1.iter().any(|(k, _)| *k == tw::LINK)
            })
            .count();
        assert_eq!(
            body.unknown_attribute_classes as usize, links,
            "iteration {iter}"
        );
        assert_eq!(body.unknown_attribute_keys, 0);
    }
}
