//! The viewer catalogue, `tests/fixtures/viewer` (M5 spec §14.1): its
//! entries and scenarios, and importing one order of steps into a fresh
//! archive. The committed fixtures are never written: backups are opened
//! in place with a temporary staging directory.
#![allow(dead_code, clippy::disallowed_methods, clippy::disallowed_types)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;

use oma_archive::{ArchiveId, Library, LineageChoice, LineageId};
use serde_json::Value;

use super::import::{
    import, lineage_for, manifest_ref, open_dir, opts, read_only, LineageLabels, UDID,
};
use super::{db_path, hex, library};

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

/// The model's name of every message: `guid:…` for keyed rows, and for
/// unkeyed rows `unkeyed:message:<n>:<occ>`, where `n` numbers the distinct
/// fingerprints in byte order (the model's `label_unkeyed`).
pub fn message_names(c: &rusqlite::Connection) -> HashMap<i64, String> {
    type KeyRow = (i64, i64, Option<Vec<u8>>, Option<Vec<u8>>, Option<i64>);
    let rows: Vec<KeyRow> = c
        .prepare("SELECT id, key_kind, guid, unkeyed_fp, unkeyed_occ FROM message")
        .expect("prepare")
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    let fps: Vec<Vec<u8>> = rows
        .iter()
        .filter_map(|r| r.3.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    rows.into_iter()
        .map(|(id, kind, guid, fp, occ)| {
            let name = if kind == 1 {
                let g = guid.expect("guid");
                match String::from_utf8(g.clone()) {
                    Ok(t) => format!("guid:{t}"),
                    Err(_) => format!("guid-hex:{}", hex(&g)),
                }
            } else {
                let n = fps.iter().position(|f| Some(f) == fp.as_ref()).expect("fp");
                format!("unkeyed:message:{}:{}", n + 1, occ.expect("occ"))
            };
            (id, name)
        })
        .collect()
}

/// What a model comparison needs to name rows.
pub struct Names {
    pub messages: HashMap<i64, String>,
    pub handles: HashMap<i64, String>,
    pub chats: HashMap<i64, String>,
    pub attachments: HashMap<i64, String>,
    pub lineages: BTreeMap<i64, String>,
    /// Import session id → the catalogue entry it imported.
    pub entries: HashMap<i64, String>,
}

pub fn names(i: &Imported, order: &Value) -> Names {
    let c = read_only(&i.db());
    let all = |table: &str| -> HashMap<i64, String> {
        let ids: Vec<i64> = c
            .prepare(&format!("SELECT id FROM {table}"))
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("ids");
        ids.into_iter()
            .map(|id| (id, manifest_ref(table, &c, id)))
            .collect()
    };
    let sessions: Vec<i64> = c
        .prepare("SELECT id FROM import_session ORDER BY id")
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("sessions");
    let steps = order.as_array().expect("steps");
    assert_eq!(sessions.len(), steps.len(), "one session per step");
    Names {
        messages: message_names(&c),
        handles: all("handle"),
        chats: all("chat"),
        attachments: all("attachment"),
        lineages: i.labels.clone(),
        entries: sessions
            .into_iter()
            .zip(steps)
            .map(|(s, st)| (s, st["entry"].as_str().expect("entry").to_owned()))
            .collect(),
    }
}
