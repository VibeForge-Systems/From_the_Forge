//! Committed fixtures match their `SHA256SUMS`, and no file is missing from
//! it (M3 §19.3). This catches line-ending conversion on checkout (see
//! `.gitattributes`).
//!
//! The SHA-256 of the typedstream `SHA256SUMS` itself is pinned: oracle
//! run 2 in `docs/compatibility.md` is valid only for that checksum, so a
//! change to any vector fails here until the run is repeated and recorded.
//! The same holds for the messages catalogue, whose oracle runs 1 and 3 are
//! valid only for the pinned checksum of its `SHA256SUMS`.
//!
//! No two paths under `tests/fixtures/` may differ only by ASCII case:
//! Windows (and default macOS) filesystems would check them out as one
//! file, and a test would silently read the wrong vector.
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

/// Recorded with oracle run 2 (pytypedstream 0.1.0), repeated after the
/// rename-only regeneration that removed case collisions.
const TYPEDSTREAM_SUMS_SHA256: &str =
    "d3792a1f6c95255453f9b6336361470d4a90f90078463c37f290816121506f82";

/// Recorded with oracle runs 1 and 3 (`docs/compatibility.md`).
const MESSAGES_SUMS_SHA256: &str =
    "8ac4418ebce62f7407056e0041bde0195d10832e3a200d463f18911d4c171fd0";

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn typedstream_sha256sums() {
    let root = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/typedstream"
    ));
    let raw = fs::read(root.join("SHA256SUMS")).expect("SHA256SUMS");
    assert_eq!(
        hex(&raw),
        TYPEDSTREAM_SUMS_SHA256,
        "the vectors changed: regenerate them, repeat oracle run 2, record it in \
         docs/compatibility.md and update this constant"
    );
    let sums = String::from_utf8(raw).expect("ascii");
    assert!(!sums.contains('\r'), "SHA256SUMS has CRLF line endings");
    let mut listed = BTreeSet::new();
    for line in sums.lines() {
        let (want, name) = line.split_once("  ").expect("line format");
        let bytes = fs::read(root.join(name)).expect(name);
        assert_eq!(hex(&bytes), want, "{name} does not match SHA256SUMS");
        listed.insert(name.to_owned());
    }
    for e in fs::read_dir(&root).expect("dir").flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name == "SHA256SUMS" || name == "README.md" {
            continue;
        }
        assert!(listed.contains(&name), "{name} is not in SHA256SUMS");
    }
    assert!(listed.contains("expected.json"));
    assert!(listed.len() > 80, "{}", listed.len());
}

fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut BTreeSet<String>) {
    for e in fs::read_dir(dir).expect("dir").flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, root, out);
        } else {
            let rel = p
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel);
        }
    }
}

#[test]
fn messages_sha256sums() {
    let root = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/messages"
    ));
    let raw = fs::read(root.join("SHA256SUMS")).expect("SHA256SUMS");
    assert_eq!(
        hex(&raw),
        MESSAGES_SUMS_SHA256,
        "the messages catalogue changed: regenerate it, repeat oracle runs 1 and 3, \
         record them in docs/compatibility.md and update this constant"
    );
    let sums = String::from_utf8(raw).expect("ascii");
    assert!(!sums.contains('\r'), "SHA256SUMS has CRLF line endings");
    let mut listed = BTreeSet::new();
    for line in sums.lines() {
        let (want, name) = line.split_once("  ").expect("line format");
        let bytes = fs::read(root.join(name)).expect(name);
        assert_eq!(hex(&bytes), want, "{name} does not match SHA256SUMS");
        listed.insert(name.to_owned());
    }
    let mut present = BTreeSet::new();
    walk(&root, &root, &mut present);
    present.remove("SHA256SUMS");
    present.remove("README.md");
    assert_eq!(present, listed, "files and SHA256SUMS differ");
    assert!(listed.len() > 100, "{}", listed.len());
}

/// Collects every file and directory below `dir`, relative to `root`.
fn walk_all(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<String>) {
    for e in fs::read_dir(dir).expect("dir").flatten() {
        let p = e.path();
        let rel = p
            .strip_prefix(root)
            .expect("under root")
            .to_string_lossy()
            .replace('\\', "/");
        out.push(rel);
        if p.is_dir() {
            walk_all(&p, root, out);
        }
    }
}

/// A case-insensitive checkout (Windows) cannot hold two names that differ
/// only by case. This runs on case-sensitive filesystems, where both still
/// exist, and fails before such a pair reaches a Windows runner.
#[test]
fn fixture_paths_unique_ignoring_ascii_case() {
    let root = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures"));
    let mut paths = Vec::new();
    walk_all(&root, &root, &mut paths);
    assert!(paths.len() > 200, "{}", paths.len());
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let mut clashes = Vec::new();
    for p in paths {
        if let Some(prev) = seen.get(&p.to_ascii_lowercase()) {
            clashes.push(format!("{prev} / {p}"));
        } else {
            seen.insert(p.to_ascii_lowercase(), p);
        }
    }
    assert!(
        clashes.is_empty(),
        "paths under tests/fixtures differ only by case: {clashes:?}"
    );
}
