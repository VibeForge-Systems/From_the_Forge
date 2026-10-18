//! The viewer catalogue, `tests/fixtures/viewer` (M5 spec §14.1): its
//! entries and scenarios, and importing one order of steps into a fresh
//! archive. The committed fixtures are never written: backups are opened
//! in place with a temporary staging directory.
#![allow(dead_code, clippy::disallowed_methods, clippy::disallowed_types)]

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use oma_archive::{ArchiveId, Library, LineageChoice, LineageId};
use serde_json::Value;

use super::import::{import, lineage_for, open_dir, opts, LineageLabels, UDID};
use super::{db_path, library};

/// The entries the catalogue must hold.
pub const ENTRIES: &[&str] = &[
    "view-hostile-text-plain",
    "view-lin-a1-plain",
    "view-lin-a2-plain",
    "view-lin-a3-incomplete-plain",
    "view-lin-b-plain",
    "view-lin-rd-plain",
    "view-main-plain",
    "view-media-plain",
    "view-media2-plain",
    "view-oldcols-plain",
];

/// The scenarios the catalogue must hold.
pub const SCENARIOS: &[&str] = &["lineages", "media-copy-off", "media-older-blob", "rd-first"];

pub fn viewer_root() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/viewer"
    ))
}

/// A path below the catalogue from a `/`-separated relative name.
pub fn at(rel: &str) -> PathBuf {
    rel.split('/').fold(viewer_root(), |p, part| p.join(part))
}

pub fn json_at(rel: &str) -> Value {
    serde_json::from_slice(&fs::read(at(rel)).expect("json")).expect("parse")
}

/// Every model document: each entry's `content.json`, then each scenario's.
pub fn model_docs() -> Vec<(String, Value)> {
    ENTRIES
        .iter()
        .map(|e| {
            (
                (*e).to_owned(),
                json_at(&format!("{e}.expected/content.json")),
            )
        })
        .chain(SCENARIOS.iter().map(|s| {
            (
                (*s).to_owned(),
                json_at(&format!("scenarios/{s}.content.json")),
            )
        }))
        .collect()
}

/// An archive the steps of one order were imported into.
pub struct Imported {
    pub tmp: tempfile::TempDir,
    pub lib: Library,
    pub id: ArchiveId,
    pub labels: LineageLabels,
}

impl Imported {
    pub fn db(&self) -> PathBuf {
        db_path(&self.tmp, &self.id)
    }
}

/// Import the steps of `order` (a model's `orders[i]`) into a fresh
/// archive, each with its options and lineage label.
pub fn import_order(order: &Value, what: &str) -> Imported {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut lineages: BTreeMap<String, LineageId> = BTreeMap::new();
    for step in order.as_array().expect("steps") {
        let entry = step["entry"].as_str().expect("entry");
        let label = step["lineage"].as_str().expect("lineage");
        let mut o = opts(1000);
        o.include_recently_deleted = step["includeRecentlyDeleted"] == true;
        o.copy_attachments = step["copyAttachments"] != false;
        o.ios_version_untested = step["iosVersionUntested"] == true;
        let mut a = lib.open(&id).expect("open");
        let src = open_dir(&viewer_root().join(entry).join(UDID), None);
        let choice: LineageChoice = lineage_for(&mut lineages, label);
        let s = import(&mut a, &src, &o, choice);
        lineages.insert(label.to_owned(), s.lineage);
        drop(a);
        assert!(src.ob.close().removed, "{what}: staging not removed");
    }
    let labels: LineageLabels = lineages.iter().map(|(k, v)| (v.get(), k.clone())).collect();
    Imported {
        tmp,
        lib,
        id,
        labels,
    }
}
