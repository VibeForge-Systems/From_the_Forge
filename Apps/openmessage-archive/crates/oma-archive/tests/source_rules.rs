//! Rules on the crate's own sources (spec §5.5, §13.2, §18.9).
//!
//! - No `#[tracing::instrument]` without `skip_all`.
//! - No `File::open`, `OpenOptions` or `fs::copy` outside the modules that
//!   may touch files directly (`lock.rs`, `blobs.rs`, `provenance.rs`);
//!   clippy's `disallowed-methods` enforces the same for calls.
//! - No `Path::join` with an argument other than a string literal or a
//!   constant outside `layout.rs` and `blobs.rs`.
//! - No `INSERT OR REPLACE`.
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

#[test]
fn source_rules() {
    let mut files = Vec::new();
    sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(files.len() >= 15, "{}", files.len());
    for f in &files {
        let name = file_name(f);
        let text = fs::read_to_string(f).expect("read");
        let mut in_tests = false;
        for (n, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if code.starts_with("#[cfg(test)]") {
                in_tests = true;
            }
            let at = format!("{}:{}", f.display(), n + 1);
            if code.contains("instrument") {
                assert!(code.contains("skip_all"), "{at}: instrument needs skip_all");
            }
            assert!(
                !code.to_ascii_uppercase().contains("INSERT OR REPLACE"),
                "{at}: INSERT OR REPLACE"
            );
            assert!(!code.contains("interrupt()"), "{at}: interrupt()");
            if in_tests {
                continue;
            }
            if !["lock.rs", "blobs.rs", "provenance.rs"].contains(&name.as_str()) {
                for banned in ["File::open", "OpenOptions", "fs::copy"] {
                    assert!(!code.contains(banned), "{at}: {banned}");
                }
            }
            if !["layout.rs", "blobs.rs"].contains(&name.as_str()) {
                for arg in join_args(code) {
                    assert!(
                        is_literal_or_const(arg),
                        "{at}: Path::join with a computed argument ({arg})"
                    );
                }
            }
        }
    }
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
