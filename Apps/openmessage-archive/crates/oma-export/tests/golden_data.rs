//! JSON and CSV golden outputs (M6 spec §14.3, slice 6f): every entry and
//! scenario of the viewer and export catalogues is imported (export
//! patches applied) and exported whole to one JSON + CSV bundle, names
//! only, history included, CSV "for spreadsheets", in UTC, with a fixed
//! export id and clock; every byte of the bundle must equal
//! `tests/fixtures/export/expected-data/<entry>/` (the JSON and CSV files,
//! the schema copies, `README.txt`, `manifest.json` and the bundle's own
//! `SHA256SUMS`). The comparison runs on Linux and on Windows; the CSV
//! files' CRLF record ends are kept byte for byte because
//! `tests/fixtures/**` is binary in `.gitattributes`.
//!
//! The goldens are pinned by `expected-data/SHA256SUMS`, whose hash is
//! pinned here. A file over [`INLINE_MAX`] bytes (the hostile catalogue's 1 MiB
//! body) is pinned by its line there only, not committed, so the
//! repository does not grow by its size; it is still compared byte for
//! byte through its digest. The schema copies are pinned by digest only
//! too (they are `crates/oma-export/schema/`'s files, pinned by
//! `tests/json.rs`). They are regenerated only deliberately, when a JSON
//! or CSV rule changes:
//!
//! ```sh
//! OMA_REGEN_GOLDENS=1 cargo test -p oma-export --test golden_data
//! ```
//!
//! then re-pin [`GOLDEN_SUMS_SHA256`] with the hash the run prints, and
//! name the rule that changed in the commit.
#![allow(clippy::disallowed_methods)]

mod common;
mod files;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::*;
use oma_export::plan::{AttachmentMode, Format, ScopePlan};

/// The SHA-256 of `tests/fixtures/export/expected/SHA256SUMS`.
const GOLDEN_SUMS_SHA256: &str = "0f4fc772e8b84b3ffd55669983e8bd8a67047852082718d31d382affede283c6";

const BUNDLE: &str = "Golden data export";

/// The largest golden committed as a file.
const INLINE_MAX: usize = 64 * 1024;

fn expected_root() -> PathBuf {
    fixtures("export").join("expected-data")
}

/// Every file of the bundle `doc` exports to, by its `/` path.
fn export_doc(doc: &Doc) -> BTreeMap<String, Vec<u8>> {
    let i = import(doc);
    if let Some(e) = &doc.export {
        apply_patches(&i.db(), e["patches"].as_array().expect("patches"));
    }
    let d = files::dest();
    let names_only = |o: &mut oma_export::plan::ExportOptions| {
        o.attachments = AttachmentMode::None;
        o.history = true;
    };
    export_formats(
        &i,
        &d.parent,
        ScopePlan::Archive,
        &[Format::Csv, Format::Json],
        &names_only,
        BUNDLE,
        None,
        &oma_export::ExportZone::Utc,
    )
    .expect("export");
    let bundle = d.parent.join(BUNDLE);
    files::tree(&bundle)
        .into_iter()
        .filter(|r| !r.ends_with('/'))
        .map(|r| {
            let bytes = std::fs::read(under(&bundle, &r)).expect("read");
            (r, bytes)
        })
        .collect()
}

fn sha256_hex(b: &[u8]) -> String {
    files::sha256_hex(b)
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, bytes).expect("write");
}

#[test]
fn json_and_csv_exports_equal_their_goldens_byte_for_byte() {
    let regen = std::env::var_os("OMA_REGEN_GOLDENS").is_some();
    let root = expected_root();
    let mut sums: BTreeMap<String, String> = BTreeMap::new();
    if regen && root.exists() {
        std::fs::remove_dir_all(&root).expect("clear goldens");
    }
    let pinned: BTreeMap<String, String> = if regen {
        BTreeMap::new()
    } else {
        std::fs::read_to_string(root.join("SHA256SUMS"))
            .expect("expected/SHA256SUMS")
            .lines()
            .map(|l| {
                let (h, r) = l.split_once("  ").expect("sums line");
                (r.to_owned(), h.to_owned())
            })
            .collect()
    };
    let mut inline = Vec::new();
    for doc in docs() {
        for (rel, bytes) in export_doc(&doc) {
            let golden = format!("{}/{rel}", doc.name);
            let path = under(&root, &golden);
            let hex = sha256_hex(&bytes);
            if bytes.len() > INLINE_MAX || rel.starts_with("schema/") {
                if !regen {
                    assert_eq!(
                        pinned.get(&golden),
                        Some(&hex),
                        "{golden} differs from its pinned digest"
                    );
                }
            } else if regen {
                inline.push(golden.clone());
                write(&path, &bytes);
            } else {
                inline.push(golden.clone());
                let want = std::fs::read(&path)
                    .unwrap_or_else(|e| panic!("{golden}: {e} (regenerate the goldens)"));
                if want != bytes {
                    let w = String::from_utf8_lossy(&want);
                    let g = String::from_utf8_lossy(&bytes);
                    let line = w
                        .lines()
                        .zip(g.lines())
                        .position(|(a, b)| a != b)
                        .map_or(0, |n| n + 1);
                    panic!("{golden} differs from its golden (first at line {line})");
                }
            }
            sums.insert(golden, hex);
        }
    }
    let mut text = String::new();
    for (rel, hex) in &sums {
        text.push_str(&format!("{hex}  {rel}\n"));
    }
    let pin = sha256_hex(text.as_bytes());
    if regen {
        write(&root.join("SHA256SUMS"), text.as_bytes());
        println!("RECORD golden_data files={} sums_sha256={pin}", sums.len());
        return;
    }
    // No golden is missing its export, and the pins hold.
    let on_disk = std::fs::read_to_string(root.join("SHA256SUMS")).expect("expected/SHA256SUMS");
    assert_eq!(on_disk, text, "expected/SHA256SUMS lists other files");
    assert_eq!(
        pin, GOLDEN_SUMS_SHA256,
        "expected/SHA256SUMS changed: regenerate deliberately and re-pin it here"
    );
    let mut listed: Vec<String> = files::tree(&root)
        .into_iter()
        .filter(|r| !r.ends_with('/') && r != "SHA256SUMS")
        .collect();
    listed.sort();
    inline.sort();
    assert_eq!(listed, inline);
    // JSON and the provenance files have LF line ends only; CSV records
    // end in CRLF.
    for rel in &inline {
        let b = std::fs::read(under(&root, rel)).expect("read");
        if rel.ends_with(".csv") {
            assert!(b.ends_with(b"\r\n"), "{rel}: CSV without CRLF");
        } else {
            assert!(!b.contains(&b'\r'), "{rel}: CR in a golden");
        }
    }
}
