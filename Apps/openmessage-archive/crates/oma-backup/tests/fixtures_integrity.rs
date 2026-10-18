//! Every fixture file matches SHA256SUMS, and no file is missing from it.
//! This catches line-ending conversion on checkout (see .gitattributes).
mod common;

use std::collections::BTreeSet;
use std::fs;

use common::*;
use sha2::{Digest, Sha256};

#[test]
fn sha256sums() {
    let root = fixtures_dir();
    let sums = fs::read_to_string(root.join("SHA256SUMS")).expect("SHA256SUMS");
    assert!(!sums.contains('\r'), "SHA256SUMS has CRLF line endings");
    let mut listed = BTreeSet::new();
    for line in sums.lines() {
        let (hex, rel) = line.split_once("  ").expect("line format");
        let bytes = fs::read(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        let digest: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(digest, hex, "{rel} does not match SHA256SUMS");
        listed.insert(rel.to_owned());
    }
    let mut files = Vec::new();
    walk(&root, &mut files);
    for f in files {
        let rel = f
            .strip_prefix(&root)
            .expect("prefix")
            .to_string_lossy()
            .replace('\\', "/");
        if rel == "SHA256SUMS" || rel == "README.md" {
            continue;
        }
        assert!(listed.contains(&rel), "{rel} is not in SHA256SUMS");
    }
    for name in ALL_FIXTURES {
        assert!(
            listed.iter().any(|p| p.starts_with(&format!("{name}/"))),
            "{name} has no files in SHA256SUMS"
        );
    }
}
