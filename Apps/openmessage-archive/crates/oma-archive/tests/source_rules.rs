//! Rules on the crate's own sources (spec §5.5, §13.2, §18.9).
//!
//! - No `#[tracing::instrument]` without `skip_all`.
//! - No `File::open`, `OpenOptions` or `fs::copy` outside the modules that
//!   may touch files directly (`lock.rs`, `blobs.rs`, `provenance.rs`);
//!   clippy's `disallowed-methods` enforces the same for calls.
//! - No `Path::join` with an argument other than a string literal or a
//!   constant outside `layout.rs` and `blobs.rs`.
//! - No `INSERT OR REPLACE` (it deletes the row and changes its id), and no
//!   `RETURNING` (with `ON CONFLICT … DO UPDATE … WHERE` it returns no row
//!   when the WHERE is false; spec §8.5).
//! - No `interrupt()` (the armed interrupt guard arrives with the import).
//! - The fault-injection hooks are compiled only with their feature.
#![allow(clippy::disallowed_methods)]

use std::fs;
use std::path::{Path, PathBuf};

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).expect("read_dir").flatten() {
        let p = e.path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().expect("name").to_string_lossy().into_owned()
}

/// The arguments of every `.join(` on the line.
fn join_args(code: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = code;
    while let Some(at) = rest.find(".join(") {
        rest = &rest[at + ".join(".len()..];
        let end = rest.find(')').unwrap_or(rest.len());
        out.push(rest[..end].trim());
    }
    out
}

fn is_literal_or_const(arg: &str) -> bool {
    arg.starts_with('"')
        || (!arg.is_empty()
            && arg
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
}

/// Every rule violation in one source file. Only a `#[cfg(test)]` that is
/// followed by a `mod` item starts the exempt test section; an item-level
/// `#[cfg(test)]` (on a function or an impl) exempts nothing, so production
/// code after it is still checked.
fn violations(name: &str, text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut in_tests = false;
    for (n, line) in lines.iter().enumerate() {
        let code = line.trim_start();
        if code.starts_with("//") {
            continue;
        }
        if code.starts_with("#[cfg(test)]") {
            let next = lines
                .iter()
                .skip(n + 1)
                .map(|l| l.trim_start())
                .find(|l| !l.is_empty() && !l.starts_with("//") && !l.starts_with("#["));
            if next.is_some_and(|l| l.starts_with("mod ") || l.starts_with("pub(crate) mod ")) {
                in_tests = true;
            }
        }
        let at = format!("{name}:{}", n + 1);
        if code.contains("instrument") && !code.contains("skip_all") {
            out.push(format!("{at}: instrument needs skip_all"));
        }
        if code.to_ascii_uppercase().contains("INSERT OR REPLACE") {
            out.push(format!("{at}: INSERT OR REPLACE"));
        }
        if code.contains("RETURNING") {
            out.push(format!("{at}: RETURNING"));
        }
        if code.contains("interrupt()") {
            out.push(format!("{at}: interrupt()"));
        }
        if in_tests {
            continue;
        }
        if !["lock.rs", "blobs.rs", "provenance.rs"].contains(&name) {
            for banned in ["File::open", "OpenOptions", "fs::copy"] {
                if code.contains(banned) {
                    out.push(format!("{at}: {banned}"));
                }
            }
        }
        if !["layout.rs", "blobs.rs"].contains(&name) {
            for arg in join_args(code) {
                if !is_literal_or_const(arg) {
                    out.push(format!("{at}: Path::join with a computed argument ({arg})"));
                }
            }
        }
    }
    out
}

#[test]
fn source_rules() {
    let mut files = Vec::new();
    sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(files.len() >= 28, "{}", files.len());
    let mut found = Vec::new();
    for f in &files {
        let text = fs::read_to_string(f).expect("read");
        for v in violations(&file_name(f), &text) {
            found.push(format!("{} ({v})", f.display()));
        }
    }
    assert!(found.is_empty(), "{found:#?}");
}

/// The scanner itself: an item-level `#[cfg(test)]` exempts nothing after
/// it, and `#[cfg(test)] mod tests` exempts the rest of the file.
#[test]
fn the_scanner_checks_code_after_an_item_level_cfg_test() {
    let snippet = "\
fn a() {}
#[cfg(test)]
fn only_in_tests() {}
fn b(dir: &Path, name: &str) {
    let _ = dir.join(name);
}
#[cfg(test)]
mod tests {
    fn c(dir: &Path, name: &str) {
        let _ = dir.join(name);
        let _ = File::open(name);
    }
}
";
    let v = violations("preflight.rs", snippet);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].starts_with("preflight.rs:5: Path::join"), "{v:?}");
    // The exempt files keep their exemption.
    assert!(violations("layout.rs", snippet).is_empty());
}

#[test]
fn the_fault_hooks_are_feature_gated() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let fault = fs::read_to_string(src.join("fault.rs")).expect("fault.rs");
    let lines: Vec<&str> = fault.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with("pub mod hooks"))
        .expect("hooks module");
    assert_eq!(
        lines[at - 1].trim(),
        "#[cfg(feature = \"fault-injection\")]"
    );
    let lib = fs::read_to_string(src.join("lib.rs")).expect("lib.rs");
    let lines: Vec<&str> = lib.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with("pub use fault::hooks"))
        .expect("re-export");
    assert_eq!(
        lines[at - 1].trim(),
        "#[cfg(feature = \"fault-injection\")]"
    );
    let manifest =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).expect("toml");
    assert!(manifest.contains("fault-injection = []"));
    assert!(!manifest.contains("default = [\"fault-injection\"]"));
}
