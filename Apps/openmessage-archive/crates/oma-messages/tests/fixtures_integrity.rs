//! Committed fixtures match their `SHA256SUMS`, and no file is missing from
//! it (M3 §19.3). This catches line-ending conversion on checkout (see
//! `.gitattributes`).
//!
//! The SHA-256 of the typedstream `SHA256SUMS` itself is pinned: oracle
//! run 2 in `docs/compatibility.md` is valid only for that checksum, so a
//! change to any vector fails here until the run is repeated and recorded.
//! The same holds for the messages catalogue, whose oracle runs 1 and 3 are
//! valid only for the pinned checksum of its `SHA256SUMS`.
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

/// Recorded with oracle run 2 (pytypedstream 0.1.0).
const TYPEDSTREAM_SUMS_SHA256: &str =
    "43a9f7f612976ad9d51097a86552c7eaa4718095747981dde556deafd62de0d9";

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
