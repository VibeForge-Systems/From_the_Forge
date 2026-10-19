//! The JSON writer end to end (M6 spec §5.1, §14.2 "Per format: JSON"):
//! every catalogue entry is imported and exported to JSON through
//! `pipeline::run`, with a recording writer beside it that keeps the
//! serde value of every record it was handed.
//!
//! - every file is UTF-8 without a BOM; no raw U+2028, U+2029, U+0085,
//!   DEL or C1 control is written anywhere, so Python's
//!   `str.splitlines()` sees exactly the JSON Lines records;
//! - each `.jsonl` line parses and equals the record it was made from
//!   (the serialisation loses nothing); the header comes first;
//!   `conversations.json` and `handles.json` equal the records handed
//!   over; versions follow their item;
//! - a message in two conversation files is byte-identical up to
//!   `"context"` in both;
//! - raw integers are decimal strings, and the nanosecond ones exceed
//!   2^53 − 1;
//! - no read, delivery or sent key exists in any document (D94);
//! - the bundle carries the open and manifest schemas, byte for byte;
//! - a single file streams `handles` and `about` last;
//! - the strict schema's SHA-256 is pinned against `schemaVersion`, its
//!   code tables equal the registry, and the open schema is its
//!   derivation (`scripts/make-export-schemas.py`).
#![allow(clippy::disallowed_methods)]

mod common;
mod files;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::*;
use oma_export::manifest::SCHEMA_VERSION;
use oma_export::plan::{AttachmentMode, Format, RevealPlan, ScopePlan};
use oma_export::record::{HandleRecord, ItemRecord, VersionRecord};
use oma_export::stage::Staging;
use oma_export::writer::json::{JsonWriter, EXPORT_SCHEMA, MANIFEST_SCHEMA, STRICT_SCHEMA};
use oma_export::writer::{
    AboutDraft, ConversationFile, ConversationTotals, ExportContext, ExportWriter, WriterReport,
};
use oma_export::{ExportError, ExportZone};
use serde_json::Value;

/// The SHA-256 of `schema/export-v1.strict.schema.json` for
/// [`SCHEMA_VERSION`]. A schema change must bump the version (spec §5.1)
/// and re-pin here.
const STRICT_SCHEMA_SHA256: (&str, &str) = (
    "1.0.0",
    "84956f777e053e97d04a9631a29cea3c53d56fd0f00b09b41961a6df2567e807",
);

const BUNDLE: &str = "JSON export";

/// The keys no export document may carry (D94: read, delivery and sent
/// state), as camelCase JSON keys and as the archive's column names.
pub const D94_KEYS: &[&str] = &[
    "isRead",
    "isDelivered",
    "isSent",
    "isPlayed",
    "dateRead",
    "dateDelivered",
    "datePlayed",
    "error",
    "lastReadMessageTimestamp",
    "lastRead",
    "is_read",
    "is_delivered",
    "is_sent",
    "date_read",
    "date_delivered",
    "date_played",
    "readState",
    "deliveryState",
    "sentState",
];

/// What the recording writer saw, as serde values, per conversation file.
#[derive(Default)]
struct Seen {
    handles: Vec<Value>,
    conversations: Vec<Value>,
    /// Per conversation file: its item and version values in order, each
    /// with `type` added as the writer adds it.
    files: Vec<Vec<Value>>,
}

/// A JSON writer that also records what it is handed.
struct Tee {
    inner: JsonWriter,
    seen: Arc<Mutex<Seen>>,
}

fn with_type(kind: &str, v: Value) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("type".to_owned(), Value::String(kind.to_owned()));
    if let Value::Object(o) = v {
        m.extend(o);
    }
    Value::Object(m)
}

impl ExportWriter for Tee {
    fn format(&self) -> Format {
        Format::Json
    }
    fn begin(&mut self, ctx: &ExportContext<'_>, out: &mut Staging) -> Result<(), ExportError> {
        self.inner.begin(ctx, out)
    }
    fn begin_conversation(
        &mut self,
        c: ConversationFile<'_>,
        out: &mut Staging,
    ) -> Result<(), ExportError> {
        let mut s = self.seen.lock().expect("lock");
        if let ConversationFile::Conversation(r) = c {
            s.conversations
                .push(serde_json::to_value(r).expect("value"));
        }
        s.files.push(Vec::new());
        drop(s);
        self.inner.begin_conversation(c, out)
    }
    fn item(&mut self, it: &ItemRecord, out: &mut Staging) -> Result<(), ExportError> {
        let v = with_type("item", serde_json::to_value(it).expect("value"));
        self.seen
            .lock()
            .expect("lock")
            .files
            .last_mut()
            .expect("file")
            .push(v);
        self.inner.item(it, out)
    }
    fn handle(&mut self, h: &HandleRecord, out: &mut Staging) -> Result<(), ExportError> {
        self.seen
            .lock()
            .expect("lock")
            .handles
            .push(serde_json::to_value(h).expect("value"));
        self.inner.handle(h, out)
    }
    fn version(&mut self, v: &VersionRecord, out: &mut Staging) -> Result<(), ExportError> {
        let x = with_type("version", serde_json::to_value(v).expect("value"));
        self.seen
            .lock()
            .expect("lock")
            .files
            .last_mut()
            .expect("file")
            .push(x);
        self.inner.version(v, out)
    }
    fn end_conversation(
        &mut self,
        totals: &ConversationTotals,
        out: &mut Staging,
    ) -> Result<(), ExportError> {
        self.inner.end_conversation(totals, out)
    }
    fn finish(
        self: Box<Self>,
        about: &AboutDraft<'_>,
        out: &mut Staging,
    ) -> Result<WriterReport, ExportError> {
        Box::new(self.inner).finish(about, out)
    }
}

fn imported(doc: &Doc) -> Imported {
    let i = import(doc);
    if let Some(e) = &doc.export {
        apply_patches(&i.db(), e["patches"].as_array().expect("patches"));
    }
    i
}

/// Export `i` to JSON, recording what the writer was handed.
fn export_json(
    i: &Imported,
    parent: &Path,
    scope: ScopePlan,
    tweak: &dyn Fn(&mut oma_export::plan::ExportOptions),
    target: &str,
) -> (oma_export::pipeline::ExportReport, Seen) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let tee = Tee {
        inner: JsonWriter::new(),
        seen: Arc::clone(&seen),
    };
    let r = export_with(
        i,
        parent,
        scope,
        &[Format::Json],
        tweak,
        target,
        None,
        &ExportZone::Utc,
        vec![Box::new(tee)],
    )
    .expect("export");
    let seen = Arc::try_unwrap(seen)
        .ok()
        .expect("one owner")
        .into_inner()
        .expect("lock");
    (r, seen)
}

/// Python's `str.splitlines()` boundaries.
fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let br = matches!(
            c,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if br {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = it.peek() {
                    it.next();
                    end = j + 1;
                }
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

fn read_text(p: &Path) -> String {
    let b = std::fs::read(p).expect("read");
    assert!(!b.starts_with(&[0xEF, 0xBB, 0xBF]), "BOM");
    let t = String::from_utf8(b).expect("UTF-8");
    for c in t.chars() {
        assert!(
            !matches!(c, '\u{7f}'..='\u{9f}' | '\u{2028}' | '\u{2029}'),
            "raw {c:?} in {}",
            p.display()
        );
        assert!(
            !(c < ' ' && c != '\n'),
            "raw control {c:?} in {}",
            p.display()
        );
    }
    t
}

/// Every key of `v`, recursively.
fn keys(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                out.push(k.clone());
                keys(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| keys(x, out)),
        _ => {}
    }
}

/// Every `raw` value of `v`, recursively.
fn raws(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if k == "raw" {
                    out.push(x.clone());
                }
                raws(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| raws(x, out)),
        _ => {}
    }
}

/// The parsed bundle: per `.jsonl` file (by name) its lines' values and
/// raw texts; the two tables.
struct Parsed {
    lines: BTreeMap<String, Vec<(String, Value)>>,
    conversations: Value,
    handles: Value,
}

fn parse_bundle(bundle: &Path) -> Parsed {
    let dir = bundle.join("json");
    let mut lines = BTreeMap::new();
    for e in std::fs::read_dir(&dir).expect("json folder").flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".jsonl") {
            continue;
        }
        let t = read_text(&e.path());
        assert!(t.ends_with('\n'), "{name}: final newline");
        let split = splitlines(&t);
        assert_eq!(split.len(), t.matches('\n').count(), "{name}: splitlines");
        let parsed: Vec<(String, Value)> = split
            .iter()
            .map(|l| {
                let v: Value = serde_json::from_str(l).expect("a JSON line");
                ((*l).to_owned(), v)
            })
            .collect();
        lines.insert(name, parsed);
    }
    let table = |n: &str| -> Value {
        let t = read_text(&dir.join(n));
        serde_json::from_str(&t).expect("table")
    };
    Parsed {
        lines,
        conversations: table("conversations.json"),
        handles: table("handles.json"),
    }
}

#[test]
fn every_catalogue_entry_exports_to_json_that_equals_its_records() {
    let mut exported = 0;
    let mut shared_checked = 0;
    let mut big_raw = 0;
    for doc in docs() {
        let what = doc.name.clone();
        let i = imported(&doc);
        let d = files::dest();
        let history = |o: &mut oma_export::plan::ExportOptions| o.history = true;
        let (report, seen) = export_json(&i, &d.parent, ScopePlan::Archive, &history, BUNDLE);
        let bundle = d.parent.join(BUNDLE);
        files::verify_sums(&bundle);
        assert_eq!(
            std::fs::read(bundle.join("schema").join("export-v1.schema.json")).expect("schema"),
            EXPORT_SCHEMA.as_bytes()
        );
        assert_eq!(
            std::fs::read(bundle.join("schema").join("manifest-v1.schema.json")).expect("schema"),
            MANIFEST_SCHEMA.as_bytes()
        );
        let p = parse_bundle(&bundle);
        assert_eq!(p.conversations["type"], "conversations", "{what}");
        assert_eq!(p.conversations["format"], "openmessage-archive-export");
        assert_eq!(p.conversations["schemaVersion"], SCHEMA_VERSION);
        assert_eq!(
            p.conversations["conversations"],
            Value::Array(seen.conversations.clone()),
            "{what}: conversations.json"
        );
        assert_eq!(
            p.handles["handles"],
            Value::Array(seen.handles.clone()),
            "{what}: handles.json"
        );
        // The files in order: c0001.jsonl … then no-conversation.jsonl.
        let mut names: Vec<&String> = p.lines.keys().collect();
        names.sort_by_key(|n| (n.as_str() == "no-conversation.jsonl", n.to_string()));
        assert_eq!(names.len(), seen.files.len(), "{what}: files");
        let mut intrinsic: BTreeMap<u64, String> = BTreeMap::new();
        let mut items = 0u64;
        let mut folded = 0u64;
        for (name, recorded) in names.into_iter().zip(&seen.files) {
            let lines = &p.lines[name];
            let (head, rest) = lines.split_first().expect("a header line");
            assert_eq!(head.1["type"], "header", "{what} {name}");
            assert_eq!(head.1["schemaVersion"], SCHEMA_VERSION);
            match head.1["conversation"].as_u64() {
                Some(n) => assert_eq!(name, &format!("c{n:04}.jsonl")),
                None => assert_eq!(name, "no-conversation.jsonl"),
            }
            let got: Vec<Value> = rest.iter().map(|(_, v)| v.clone()).collect();
            assert_eq!(&got, recorded, "{what} {name}: lines equal the records");
            for (raw, v) in rest {
                let mut k = Vec::new();
                keys(v, &mut k);
                for banned in D94_KEYS {
                    assert!(!k.iter().any(|x| x == banned), "{what}: {banned}");
                }
                let mut r = Vec::new();
                raws(v, &mut r);
                // `raw` is also the record's raw-columns object.
                for x in r.into_iter().filter(|x| !x.is_object()) {
                    assert!(x.is_string() || x.is_null(), "{what}: raw {x}");
                    if let Some(n) = x.as_str().and_then(|s| s.parse::<i64>().ok()) {
                        if n.unsigned_abs() > (1u64 << 53) {
                            big_raw += 1;
                        }
                    }
                }
                if v["type"] != "item" {
                    continue;
                }
                if v["context"]["foldedInto"].is_null() {
                    items += 1;
                } else {
                    folded += 1;
                }
                // The intrinsic part: everything before `,"context":`.
                let cut = raw.rfind(",\"context\":").expect("context last");
                let n = v["n"].as_u64().expect("n");
                match intrinsic.get(&n) {
                    Some(prev) => {
                        assert_eq!(prev, &raw[..cut], "{what}: item {n} differs across files");
                        shared_checked += 1;
                    }
                    None => {
                        intrinsic.insert(n, raw[..cut].to_owned());
                    }
                }
            }
        }
        let m = json(&bundle.join("manifest.json"));
        assert_eq!(m["counts"]["items"], items, "{what}");
        assert_eq!(m["counts"]["foldedReactionRows"], folded, "{what}");
        assert_eq!(report.counts.items, items);
        assert_eq!(m["formats"], serde_json::json!(["json"]));
        exported += 1;
    }
    assert!(exported >= 20, "{exported}");
    // The catalogues hold two messages that are items of two exported
    // conversations.
    assert!(shared_checked >= 2, "{shared_checked}");
    assert!(big_raw >= 10, "{big_raw}");
}

#[test]
fn revealed_and_masked_content_follow_the_plan_in_json() {
    let mut checked = 0;
    for doc in docs().into_iter().filter(|d| d.export.is_some()) {
        let e = doc.export.clone().expect("export");
        let sentinels: Vec<String> = [
            "oneTimeCodes",
            "earlierText",
            "editText",
            "subjectHistory",
            "alternativeText",
        ]
        .iter()
        .flat_map(|k| {
            e["sentinels"][*k]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
        })
        .collect();
        if sentinels.is_empty() {
            continue;
        }
        let i = imported(&doc);
        for reveal in [false, true] {
            let d = files::dest();
            let tweak = |o: &mut oma_export::plan::ExportOptions| {
                o.history = true;
                o.attachments = AttachmentMode::None;
                if reveal {
                    o.reveal = RevealPlan {
                        one_time_codes: true,
                        earlier_content: true,
                        confirmed_up_to: Some(u64::MAX),
                    };
                }
            };
            export_json(&i, &d.parent, ScopePlan::Archive, &tweak, BUNDLE);
            // The parsed values hold the text unescaped.
            let p = parse_bundle(&d.parent.join(BUNDLE));
            let all: String = p
                .lines
                .values()
                .flatten()
                .map(|(_, v)| v.to_string())
                .collect();
            let all = format!("{all}{}{}", p.conversations, p.handles);
            for s in &sentinels {
                let needle = serde_json::to_string(s).expect("json");
                let needle = &needle[1..needle.len() - 1];
                assert_eq!(all.contains(needle), reveal, "{}: {s}", doc.name);
                checked += 1;
            }
        }
    }
    assert!(checked >= 10, "{checked}");
}

#[test]
fn a_single_file_streams_handles_and_about_last() {
    let doc = docs()
        .into_iter()
        .find(|d| d.name == "view-main-plain")
        .expect("view-main");
    let i = imported(&doc);
    let d = files::dest();
    let chat = with_snapshot(&i, |s| Ok(s.chat_ids()?[0]));
    let scope = ScopePlan::Conversation { chat };
    let (_, seen) = export_json(&i, &d.parent, scope, &|_| {}, "One conversation.json");
    let path = d.parent.join("One conversation.json");
    assert!(path.is_file(), "a single file");
    let t = read_text(&path);
    let v: Value = serde_json::from_str(&t).expect("one JSON value");
    assert_eq!(v.as_object().expect("object").len(), 6);
    // The order is the text's (a parsed map is sorted).
    let pos = |k: &str| t.find(k).unwrap_or_else(|| panic!("{k}"));
    assert!(t.starts_with("{\"format\":\"openmessage-archive-export\",\"schemaVersion\":"));
    assert!(pos("\"schemaVersion\":") < pos("\"conversation\":{"));
    assert!(pos("\"conversation\":{") < pos("\"items\":["));
    assert!(pos("\"items\":[") < pos("],\"handles\":["));
    assert!(pos("],\"handles\":[") < pos("],\"about\":{"));
    assert!(t.ends_with("}}\n"));
    assert_eq!(v["conversation"], seen.conversations[0]);
    assert_eq!(v["items"], Value::Array(seen.files[0].clone()));
    assert_eq!(v["handles"], Value::Array(seen.handles.clone()));
    assert_eq!(v["about"]["formats"], serde_json::json!(["json"]));
    assert!(v["about"].get("files").is_none());
    assert_eq!(std::fs::read_dir(&d.parent).expect("dest").count(), 1);
}

/// The schema text is pinned to its version (spec §5.1).
#[test]
fn the_strict_schema_is_pinned_to_the_schema_version() {
    let hex = files::sha256_hex(STRICT_SCHEMA.as_bytes());
    let (version, pinned) = STRICT_SCHEMA_SHA256;
    assert_eq!(
        (version, hex.as_str()),
        (SCHEMA_VERSION, pinned),
        "the strict schema changed: bump schemaVersion (spec §5.1 rules) and re-pin"
    );
}

/// The strict schema's code tables are this build's registry, and the open
/// schema is the strict one's mechanical derivation.
#[test]
fn the_schemas_agree_with_the_registry_and_each_other() {
    let strict: Value = serde_json::from_str(STRICT_SCHEMA).expect("strict");
    let open: Value = serde_json::from_str(EXPORT_SCHEMA).expect("open");
    let defs = strict["$defs"].as_object().expect("defs");
    for r in oma_export::REGISTRIES {
        let d = &defs[&format!("code-{}", r.name)];
        let pairs: Vec<(String, i64)> = d["oneOf"]
            .as_array()
            .expect("oneOf")
            .iter()
            .filter_map(|x| {
                let p = &x["properties"];
                Some((
                    p["token"]["const"].as_str()?.to_owned(),
                    p["code"]["const"].as_i64()?,
                ))
            })
            .collect();
        let want: Vec<(String, i64)> = r
            .entries
            .iter()
            .map(|e| (e.token.to_owned(), e.code))
            .collect();
        assert_eq!(pairs, want, "{}", r.name);
    }
    assert_eq!(
        defs.keys().filter(|k| k.starts_with("code-")).count(),
        oma_export::REGISTRIES.len()
    );
    fn opened(v: &Value) -> Value {
        match v {
            Value::Object(m) => Value::Object(
                m.iter()
                    .filter(|(k, x)| {
                        !(k.as_str() == "additionalProperties" && **x == Value::Bool(false))
                    })
                    .map(|(k, x)| (k.clone(), opened(x)))
                    .collect(),
            ),
            Value::Array(a) => Value::Array(a.iter().map(opened).collect()),
            other => other.clone(),
        }
    }
    let mut derived = opened(&strict);
    for k in ["$id", "title", "description"] {
        derived[k] = open[k].clone();
    }
    let dd = derived["$defs"].as_object_mut().expect("defs");
    for (k, v) in dd.iter_mut() {
        if k.starts_with("code-") {
            *v = open["$defs"][k].clone();
            assert_eq!(v["$ref"], "#/$defs/code", "{k}");
        }
    }
    assert_eq!(
        derived, open,
        "regenerate with scripts/make-export-schemas.py"
    );
    assert_eq!(open["$id"], "urn:openmessage-archive:export:1");
}

/// Writes, for the independent JSON, CSV and zone oracles
/// (`scripts/oracles/json_csv_oracle.py`, Linux CI), one library per entry
/// and scenario of both catalogues below `OMA_ORACLE_DATA_OUT` (empty or
/// absent), each with its archive and three bundles of the whole archive:
/// JSON + CSV with files copied, history and the identifier opt-in, in
/// UTC (`ids`); the same with ordinals, names only and the "data tools"
/// CSV profile (`ordinals`); and JSON in each fixed test zone
/// (`zone-<n>`, from `tests/fixtures/export/zones/`). Plus
/// `data-oracle.json` listing them.
#[test]
#[ignore = "writes the JSON and CSV oracles' inputs; run by CI's oracle step"]
fn write_oracle_data() {
    use oma_export::ids::IdentifierMode;
    use oma_export::plan::CsvProfile;
    let out = std::path::PathBuf::from(std::env::var("OMA_ORACLE_DATA_OUT").expect("set it"));
    std::fs::create_dir_all(&out).expect("create");
    assert!(
        std::fs::read_dir(&out).expect("read").next().is_none(),
        "OMA_ORACLE_DATA_OUT must be empty"
    );
    let out = dunce::canonicalize(&out).expect("canonical");
    let zones = zone_fixtures();
    let mut listed = Vec::new();
    for doc in docs() {
        let lib_dir = out.join(&doc.name).join("library");
        std::fs::create_dir_all(&lib_dir).expect("library dir");
        let (lib, id) = import_in(&doc, &lib_dir);
        let db = dunce::canonicalize(&lib_dir)
            .expect("canonical")
            .join(id.as_str())
            .join("archive.sqlite");
        if let Some(e) = &doc.export {
            apply_patches(&db, e["patches"].as_array().expect("patches"));
        }
        let i = Imported {
            tmp: tempfile::tempdir().expect("unused"),
            lib,
            id,
        };
        let mut bundles = Vec::new();
        let ids = |o: &mut oma_export::plan::ExportOptions| {
            o.history = true;
            o.identifiers = IdentifierMode::ArchiveAndSource;
            o.attachments = AttachmentMode::CopyOriginals;
        };
        let ordinals = |o: &mut oma_export::plan::ExportOptions| {
            o.history = true;
            o.attachments = AttachmentMode::None;
            o.csv_profile = Some(CsvProfile::DataTools);
        };
        for (name, tweak) in [
            ("ids", &ids as &dyn Fn(&mut oma_export::plan::ExportOptions)),
            ("ordinals", &ordinals),
        ] {
            let dest = out.join(&doc.name).join(name);
            std::fs::create_dir_all(&dest).expect("dest");
            export_formats(
                &i,
                &dest,
                ScopePlan::Archive,
                &[Format::Csv, Format::Json],
                tweak,
                BUNDLE,
                None,
                &ExportZone::Utc,
            )
            .unwrap_or_else(|e| panic!("{}: {e:?}", doc.name));
            bundles.push(serde_json::json!({
                "kind": name,
                "bundle": format!("{}/{name}/{BUNDLE}", doc.name),
                "zone": "UTC",
            }));
        }
        for (k, (zone_name, zone)) in zones.iter().enumerate() {
            let name = format!("zone-{}", k + 1);
            let dest = out.join(&doc.name).join(&name);
            std::fs::create_dir_all(&dest).expect("dest");
            let names_only = |o: &mut oma_export::plan::ExportOptions| {
                o.attachments = AttachmentMode::None;
            };
            export_formats(
                &i,
                &dest,
                ScopePlan::Archive,
                &[Format::Json],
                &names_only,
                BUNDLE,
                None,
                zone,
            )
            .unwrap_or_else(|e| panic!("{} in {zone_name}: {e:?}", doc.name));
            bundles.push(serde_json::json!({
                "kind": name,
                "bundle": format!("{}/{name}/{BUNDLE}", doc.name),
                "zone": zone_name,
            }));
        }
        let export = doc.export.as_ref().map(|_| match doc.name.as_str() {
            "history" => "scenarios/history.export.json".to_owned(),
            n => format!("{n}.expected/export.json"),
        });
        listed.push(serde_json::json!({
            "name": doc.name,
            "catalogue": doc.catalogue,
            "archive": format!("{}/library/{}", doc.name, i.id.as_str()),
            "bundles": bundles,
            "export": export,
        }));
    }
    let doc =
        serde_json::json!({"format": "oma-oracle-data", "formatVersion": 1, "exports": listed});
    std::fs::write(
        out.join("data-oracle.json"),
        serde_json::to_vec_pretty(&doc).expect("json"),
    )
    .expect("write");
}
