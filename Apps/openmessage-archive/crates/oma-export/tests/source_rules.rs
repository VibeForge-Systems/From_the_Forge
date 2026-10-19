//! Source rules of `oma-export` (M6 spec §4.1, D123):
//!
//! - the crate is Tauri-free and never names `rusqlite`: every archive
//!   read goes through `oma-archive`'s export APIs;
//! - record and mapping code iterates only ordered collections (no
//!   `HashMap` or `HashSet`), so outputs are deterministic;
//! - nothing logs (no `tracing`, `log`, `println!` or `eprintln!`): the
//!   crate holds content (I6);
//! - no record field is a bare `String` (other than the formatted `utc`
//!   and `local` times): content is a `record::Content` and raw numbers
//!   and ids a `record::Decimal`, whose `Debug` print a length only.

use std::path::{Path, PathBuf};

fn sources() -> Vec<(PathBuf, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    for e in std::fs::read_dir(&dir).expect("src") {
        let p = e.expect("entry").path();
        if p.extension().is_some_and(|x| x == "rs") {
            let text = std::fs::read_to_string(&p).expect("read");
            out.push((p, text));
        }
    }
    assert!(out.len() >= 10);
    out
}

/// The code of a file without its `//` comments.
fn code(text: &str) -> String {
    text.lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn no_sqlite_no_tauri_no_logging() {
    for (p, text) in sources() {
        let c = code(&text);
        for banned in [
            "rusqlite",
            "tauri",
            "tracing::",
            "log::",
            "println!",
            "eprintln!",
            "dbg!",
        ] {
            assert!(!c.contains(banned), "{}: {banned}", p.display());
        }
    }
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("Cargo.toml");
    let deps = manifest
        .split("[dependencies]")
        .nth(1)
        .and_then(|s| s.split("[dev-dependencies]").next())
        .expect("dependencies");
    assert!(!deps.contains("rusqlite") && !deps.contains("tauri"));
}

#[test]
fn records_and_mapping_iterate_ordered_collections_only() {
    for (p, text) in sources() {
        let c = code(&text);
        for banned in ["HashMap", "HashSet"] {
            assert!(!c.contains(banned), "{}: {banned}", p.display());
        }
    }
}

#[test]
fn content_is_never_a_bare_string_in_a_record() {
    let (_, record) = sources()
        .into_iter()
        .find(|(p, _)| p.ends_with("record.rs"))
        .expect("record.rs");
    for (i, line) in record.lines().enumerate() {
        let l = line.trim();
        let formatted_time = l.starts_with("pub utc:") || l.starts_with("pub local:");
        if l.starts_with("pub ") && l.contains(": ") && !l.starts_with("pub fn") && !formatted_time
        {
            assert!(
                !l.contains(": String") && !l.contains("<String>"),
                "record.rs:{}: content must be a `Content`, ids and raw numbers a \
                 `Decimal`",
                i + 1
            );
        }
    }
}
