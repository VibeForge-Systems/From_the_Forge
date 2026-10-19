//! The code registry is pinned (M6 spec §5.1 "Codes", §14.2): every
//! table's `name, source, code, token` lines must equal `tests/codes.pin`.
//! A renumbered, reused or removed code fails here. A new code is added
//! by regenerating the pin deliberately:
//!
//! ```sh
//! OMA_REPIN_CODES=1 cargo test -p oma-export --test codes
//! ```
//!
//! and reviewing the diff: lines may only be added.

use std::path::PathBuf;

fn pin_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("codes.pin")
}

#[test]
fn the_registry_matches_its_pin() {
    let got = oma_export::codes::registry_text();
    if std::env::var_os("OMA_REPIN_CODES").is_some() {
        std::fs::write(pin_path(), &got).expect("write pin");
    }
    let want = std::fs::read_to_string(pin_path())
        .expect("tests/codes.pin")
        .replace("\r\n", "\n");
    let old: Vec<&str> = want.lines().collect();
    let new: Vec<&str> = got.lines().collect();
    let removed: Vec<&&str> = old.iter().filter(|l| !new.contains(l)).collect();
    assert!(removed.is_empty(), "codes changed or removed: {removed:?}");
    assert_eq!(got, want, "the registry has codes the pin does not list");
}

#[test]
fn a_number_is_never_reused_for_another_token() {
    for r in oma_export::REGISTRIES {
        for e in r.entries {
            let same: Vec<_> = r.entries.iter().filter(|x| x.code == e.code).collect();
            assert_eq!(same.len(), 1, "{}: {}", r.name, e.code);
        }
    }
}
