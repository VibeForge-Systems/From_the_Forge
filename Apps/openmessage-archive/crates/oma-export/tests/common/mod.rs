//! Shared helpers for the oma-export integration tests: importing the
//! synthetic catalogues (`tests/fixtures/viewer`, `tests/fixtures/export`)
//! into a fresh archive, and walking a whole archive's records through an
//! export snapshot. The committed fixtures are never written: backups are
//! opened in place with a temporary staging directory.
#![allow(dead_code, clippy::disallowed_methods)]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use oma_archive::space::SpaceProbe;
use oma_archive::{
    Archive, ArchiveCtx, ArchiveId, CancelToken, ChatId, ExportAttachment, ExportScopeIds,
    ExportSnapshot, FixedClock, ImportOptions, ImportOutcome, Library, LineageChoice, LineageId,
    MessageId, NoProgress, ProceedWith, TimeRange,
};
use oma_backup::{inspect, Candidate, JobCtx, Limits, OpenBackup, Progress, StagingRoot};
use oma_export::map::{AttachmentFiles, ExportZone, FileDecision, FileOutcome, Mapper, Placement};
use oma_export::record::{ConversationRecord, HandleRecord, ItemRecord, VersionRecord};
use oma_export::{ExportError, IdentifierMode, Ordinals, RecordOptions};
use serde_json::Value;

pub const UDID: &str = "SYNTHETIC0-0000000000000000";
/// The catalogues' import start, 2027-01-01T00:00:00Z.
pub const MODEL_NOW_NS: i64 = 1_798_761_600 * 1_000_000_000;

pub const VIEWER_ENTRIES: &[&str] = &[
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
pub const VIEWER_SCENARIOS: &[&str] =
    &["lineages", "media-copy-off", "media-older-blob", "rd-first"];
pub const EXPORT_ENTRIES: &[&str] = &[
    "export-files-plain",
    "export-hist-1-plain",
    "export-hist-2-plain",
    "export-oldcols-plain",
    "export-records-plain",
    "export-text-plain",
];
pub const EXPORT_SCENARIOS: &[&str] = &["history"];

pub fn fixtures(catalogue: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join(catalogue)
}

/// A path below `root` from a `/`-separated relative name.
pub fn under(root: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(root.to_path_buf(), |p, part| p.join(part))
}

pub fn json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("read json")).expect("parse json")
}

/// One catalogue document: its name, `content.json` and, in the export
/// catalogue, `export.json`.
pub struct Doc {
    pub catalogue: &'static str,
    pub name: String,
    pub content: Value,
    pub export: Option<Value>,
}

/// Every entry and scenario of both catalogues.
pub fn docs() -> Vec<Doc> {
    let mut out = Vec::new();
    for (catalogue, entries, scenarios, has_export) in [
        ("viewer", VIEWER_ENTRIES, VIEWER_SCENARIOS, false),
        ("export", EXPORT_ENTRIES, EXPORT_SCENARIOS, true),
    ] {
        let root = fixtures(catalogue);
        for e in entries {
            out.push(Doc {
                catalogue,
                name: (*e).to_owned(),
                content: json(&under(&root, &format!("{e}.expected/content.json"))),
                export: has_export
                    .then(|| json(&under(&root, &format!("{e}.expected/export.json")))),
            });
        }
        for s in scenarios {
            out.push(Doc {
                catalogue,
                name: (*s).to_owned(),
                content: json(&under(&root, &format!("scenarios/{s}.content.json"))),
                export: has_export
                    .then(|| json(&under(&root, &format!("scenarios/{s}.export.json")))),
            });
        }
    }
    out
}

struct FakeSpace;

impl SpaceProbe for FakeSpace {
    fn available(&self, _path: &Path) -> io::Result<u64> {
        Ok(1 << 40)
    }
    fn total(&self, _path: &Path) -> io::Result<u64> {
        Ok(2 << 40)
    }
}

/// An archive the first order of a document was imported into.
pub struct Imported {
    pub tmp: tempfile::TempDir,
    pub lib: Library,
    pub id: ArchiveId,
}

impl Imported {
    pub fn db(&self) -> PathBuf {
        dunce_root(self.tmp.path())
            .join(self.id.as_str())
            .join("archive.sqlite")
    }
}

fn dunce_root(p: &Path) -> PathBuf {
    dunce::canonicalize(p).expect("canonical")
}

fn open_dir(dir: &Path) -> (OpenBackup, tempfile::TempDir) {
    let staging = tempfile::tempdir().expect("staging");
    let limits = Limits::default();
    let mut n = 0;
    let candidate = Candidate::for_dir(dir).expect("candidate");
    let inspection = inspect(&candidate, &limits, &mut n).expect("inspect");
    let root = StagingRoot::new(staging.path(), &inspection.root).expect("staging root");
    let session = root.begin().expect("begin");
    let token = CancelToken::new();
    let mut sink = |_: Progress| {};
    let mut ctx = JobCtx::new(&token, &mut sink);
    let ob = OpenBackup::open(inspection, session, None, &limits, &mut ctx)
        .unwrap_or_else(|e| panic!("open failed: {:?}", e.error));
    (ob, staging)
}

fn import_step(
    archive: &mut Archive,
    ob: &OpenBackup,
    step: &Value,
    lineage: LineageChoice,
) -> LineageId {
    let mut o = ImportOptions::default();
    o.batch = oma_archive::BatchBudget::new(1000, 32 << 20, None);
    o.include_recently_deleted = step["includeRecentlyDeleted"] == true;
    o.copy_attachments = step["copyAttachments"] != false;
    o.ios_version_untested = step["iosVersionUntested"] == true;
    let clock = FixedClock(MODEL_NOW_NS);
    let mut progress = NoProgress;
    let cancel = CancelToken::new();
    let mut ctx = ArchiveCtx {
        cancel: &cancel,
        progress: &mut progress,
        clock: &clock,
    };
    match archive
        .import(ob, &o, &mut ProceedWith(lineage), &mut ctx)
        .expect("import")
    {
        ImportOutcome::Completed(s) => s.lineage,
        other => panic!("import did not complete: {other:?}"),
    }
}

/// Import a document's first order into a fresh archive.
pub fn import(doc: &Doc) -> Imported {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (lib, id) = import_in(doc, tmp.path());
    Imported { tmp, lib, id }
}

/// Import a document's first order into a fresh archive of a library in
/// `dir` (which must exist).
pub fn import_in(doc: &Doc, dir: &Path) -> (Library, ArchiveId) {
    let lib = Library::new(dir)
        .expect("library")
        .with_space_probe(Arc::new(FakeSpace));
    let id = lib.create(None).expect("create");
    let root = fixtures(doc.catalogue);
    let mut lineages: BTreeMap<String, LineageId> = BTreeMap::new();
    for step in doc.content["orders"][0].as_array().expect("steps") {
        let entry = step["entry"].as_str().expect("entry");
        let label = step["lineage"].as_str().expect("lineage");
        let choice = lineages
            .get(label)
            .map_or(LineageChoice::New, |l| LineageChoice::Existing(*l));
        let mut a = lib.open(&id).expect("open");
        let (ob, _staging) = open_dir(&root.join(entry).join(UDID));
        let lineage = import_step(&mut a, &ob, step, choice);
        lineages.insert(label.to_owned(), lineage);
        drop(a);
        assert!(ob.close().removed, "staging not removed");
    }
    (lib, id)
}

/// Apply an export document's `patches` (codes an import cannot write) to
/// the test's own archive with a plain connection; the schema's `CHECK`s
/// still apply. Call it before any reader opens the archive.
pub fn apply_patches(db: &Path, patches: &[Value]) {
    let c = rusqlite::Connection::open(db).expect("raw open");
    for p in patches {
        assert_eq!(p["table"], "message", "only message patches");
        let guid = p["guid"].as_str().expect("guid");
        let set = p["set"].as_object().expect("set");
        let cols: Vec<String> = set.keys().map(|k| format!("{k} = ?")).collect();
        let mut values: Vec<rusqlite::types::Value> = set
            .values()
            .map(|v| match v {
                Value::Null => rusqlite::types::Value::Null,
                Value::Number(n) => rusqlite::types::Value::Integer(n.as_i64().expect("int")),
                other => panic!("unsupported patch value {other}"),
            })
            .collect();
        values.push(rusqlite::types::Value::Blob(guid.as_bytes().to_vec()));
        let n = c
            .execute(
                &format!("UPDATE message SET {} WHERE guid = ?", cols.join(", ")),
                rusqlite::params_from_iter(values),
            )
            .expect("patch");
        assert_eq!(n, 1, "patch {guid}");
    }
}

/// Copies every file without writing it (the real copy is `attach.rs`;
/// here every stored file counts as copied once and deduplicated after,
/// under the ordinal the mapper assigns at first reference).
#[derive(Default)]
pub struct CopyAll {
    seen: BTreeMap<oma_archive::BlobId, u64>,
}

impl AttachmentFiles for CopyAll {
    fn decide(
        &mut self,
        _a: &ExportAttachment,
        blob: oma_archive::BlobId,
    ) -> Result<FileOutcome, ExportError> {
        let next = self.seen.len() as u64 + 1;
        let (decision, n) = match self.seen.get(&blob) {
            Some(n) => (FileDecision::Deduplicated, *n),
            None => {
                self.seen.insert(blob, next);
                (FileDecision::Copied, next)
            }
        };
        Ok(FileOutcome {
            decision,
            file: Some(n),
            path: Some(format!("attachments/000/{n:06}-file")),
            sniffed: None,
        })
    }
}

/// Everything a whole-archive export would record.
pub struct Walked {
    pub items: Vec<ItemRecord>,
    /// Reaction rows folded into a target, as records with `foldedInto`.
    pub folded: Vec<ItemRecord>,
    pub versions: Vec<VersionRecord>,
    pub conversations: Vec<ConversationRecord>,
    pub handles: Vec<HandleRecord>,
    pub counts: oma_export::RecordCounts,
    /// The scope index's size.
    pub scope_len: usize,
}

impl Walked {
    /// Every record, serialised.
    pub fn values(&self) -> Vec<Value> {
        let mut v: Vec<Value> = Vec::new();
        let ser = |x: &dyn erased::Ser| x.value();
        for i in &self.items {
            v.push(ser(i));
        }
        for i in &self.folded {
            v.push(ser(i));
        }
        for x in &self.versions {
            v.push(ser(x));
        }
        for x in &self.conversations {
            v.push(ser(x));
        }
        for x in &self.handles {
            v.push(ser(x));
        }
        v
    }

    /// Every record, as one JSON text (for substring checks).
    pub fn text(&self) -> String {
        self.values()
            .iter()
            .map(|v| serde_json::to_string(v).expect("json"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub mod erased {
    pub trait Ser {
        fn value(&self) -> serde_json::Value;
    }
    impl<T: serde::Serialize> Ser for T {
        fn value(&self) -> serde_json::Value {
            serde_json::to_value(self).expect("serialise")
        }
    }
}

/// One conversation file's items and folded rows (`None`: no conversation).
type Group = (Option<ChatId>, Vec<MessageId>, Vec<(MessageId, MessageId)>);

/// Map a whole archive (every conversation in id order, then the messages
/// in no conversation) under `options`, with history when it is included.
pub fn walk_archive(
    snap: &ExportSnapshot<'_>,
    reveal: oma_archive::Reveal,
    options: RecordOptions,
) -> Result<Walked, ExportError> {
    let depth = if options.history {
        oma_archive::ExportDepth::WithHistory
    } else {
        oma_archive::ExportDepth::Items
    };
    let scope = snap.scope_index(&ExportScopeIds::Archive {
        range: TimeRange::All,
    })?;
    let scope_len = scope.len();
    let chats = snap.chat_ids()?;
    let mut groups: Vec<Group> = Vec::new();
    for chat in &chats {
        let mut cur = snap.timeline(*chat, TimeRange::All);
        let (mut items, mut folded) = (Vec::new(), Vec::new());
        while let Some(b) = cur.next_batch()? {
            items.extend(b.items);
            folded.extend(b.folded);
        }
        if !items.is_empty() || !folded.is_empty() {
            groups.push((Some(*chat), items, folded));
        }
    }
    let mut cur = snap.unassigned(TimeRange::All);
    let mut none = Vec::new();
    while let Some(b) = cur.next_batch()? {
        none.extend(b.items);
    }
    groups.push((None, none, Vec::new()));
    let exported: Vec<ChatId> = groups.iter().filter_map(|g| g.0).collect();
    let mut mapper = Mapper::new(
        Ordinals::new(scope, exported.clone(), options.identifiers),
        ExportZone::Utc,
        options,
    );
    let mut files = CopyAll::default();
    let mut out = Walked {
        items: Vec::new(),
        folded: Vec::new(),
        versions: Vec::new(),
        conversations: Vec::new(),
        handles: Vec::new(),
        counts: oma_export::RecordCounts::default(),
        scope_len,
    };
    for c in &exported {
        out.conversations.push(mapper.conversation(snap, *c)?);
    }
    // History is streamed once per message, under its first placement.
    let mut streamed = std::collections::BTreeSet::new();
    for (chat, items, folded) in &groups {
        let ms = snap.assemble(items, reveal, depth)?;
        let times: Vec<Option<i64>> = ms.iter().map(|m| m.item.message.date.utc_ns).collect();
        let ties = oma_export::order_ties(&times);
        for (m, tie) in ms.iter().zip(ties) {
            let at = Placement {
                conversation: *chat,
                folded_into: None,
                order_tie: tie,
            };
            out.items.push(mapper.item(snap, m, at, &mut files)?);
            if options.history && streamed.insert(m.item.message.id) {
                let mut h = snap.history(m, reveal)?;
                while let Some((v, codes)) = h.next_coded()? {
                    out.versions
                        .push(mapper.version(m.item.message.id, &v, codes)?);
                }
            }
        }
        for (row, target) in folded {
            let m = snap.assemble(&[*row], reveal, depth)?;
            let at = Placement {
                conversation: *chat,
                folded_into: Some(*target),
                order_tie: false,
            };
            for m in &m {
                out.folded.push(mapper.item(snap, m, at, &mut files)?);
                if options.history && streamed.insert(m.item.message.id) {
                    let mut h = snap.history(m, reveal)?;
                    while let Some((v, codes)) = h.next_coded()? {
                        out.versions
                            .push(mapper.version(m.item.message.id, &v, codes)?);
                    }
                }
            }
        }
    }
    let handles: Vec<oma_archive::HandleId> = mapper.ordinals().handles.in_order().to_vec();
    for h in handles {
        out.handles.push(mapper.handle(snap, h)?);
    }
    out.counts = mapper.counts().clone();
    Ok(out)
}

/// Record options: reactions on, names only off, ordinals.
pub fn options(history: bool, identifiers: IdentifierMode) -> RecordOptions {
    RecordOptions {
        history,
        reactions: true,
        identifiers,
        names_only: false,
    }
}

/// Run `f` inside an export snapshot of `i`.
pub fn with_snapshot<T>(
    i: &Imported,
    f: impl FnOnce(&ExportSnapshot<'_>) -> Result<T, ExportError>,
) -> T {
    let r = i.lib.open_read(&i.id).expect("reader");
    let hold = i.lib.hold_for_export(&i.id).expect("hold");
    r.with_export_snapshot(&hold, &CancelToken::new(), f)
        .expect("snapshot")
}

/// Export `i` to TXT through the whole pipeline under `scope` and
/// `tweak`ed defaults into `parent`, publishing as `target`, with the
/// attachment policy `attach` (or the plan's).
pub fn export_txt(
    i: &Imported,
    parent: &Path,
    scope: oma_export::plan::ScopePlan,
    tweak: &dyn Fn(&mut oma_export::plan::ExportOptions),
    target: &str,
    attach: Option<oma_export::attach::AttachPolicy>,
) -> Result<oma_export::pipeline::ExportReport, ExportError> {
    export_formats(
        i,
        parent,
        scope,
        &[oma_export::plan::Format::Txt],
        tweak,
        target,
        attach,
        &ExportZone::Utc,
    )
}

/// A writer for each in-process format.
pub fn writers(
    formats: &[oma_export::plan::Format],
) -> Vec<Box<dyn oma_export::writer::ExportWriter>> {
    use oma_export::plan::Format;
    formats
        .iter()
        .map(|f| -> Box<dyn oma_export::writer::ExportWriter> {
            match f {
                Format::Txt => Box::new(oma_export::writer::txt::TxtWriter::new()),
                Format::Json => Box::new(oma_export::writer::json::JsonWriter::new()),
                Format::Csv => Box::new(oma_export::writer::csv::CsvWriter::new()),
                other => panic!("no in-process writer for {other:?}"),
            }
        })
        .collect()
}

/// Export `i` to `formats` through the whole pipeline (see
/// [`export_txt`]), with times in `zone`.
#[allow(clippy::too_many_arguments)]
pub fn export_formats(
    i: &Imported,
    parent: &Path,
    scope: oma_export::plan::ScopePlan,
    formats: &[oma_export::plan::Format],
    tweak: &dyn Fn(&mut oma_export::plan::ExportOptions),
    target: &str,
    attach: Option<oma_export::attach::AttachPolicy>,
    zone: &ExportZone,
) -> Result<oma_export::pipeline::ExportReport, ExportError> {
    export_with(
        i,
        parent,
        scope,
        formats,
        tweak,
        target,
        attach,
        zone,
        writers(formats),
    )
}

/// [`export_formats`] with the caller's writers (one per format).
#[allow(clippy::too_many_arguments)]
pub fn export_with(
    i: &Imported,
    parent: &Path,
    scope: oma_export::plan::ScopePlan,
    formats: &[oma_export::plan::Format],
    tweak: &dyn Fn(&mut oma_export::plan::ExportOptions),
    target: &str,
    attach: Option<oma_export::attach::AttachPolicy>,
    zone: &ExportZone,
    writers: Vec<Box<dyn oma_export::writer::ExportWriter>>,
) -> Result<oma_export::pipeline::ExportReport, ExportError> {
    use oma_export::plan::{ExportOptions, ExportPlan};
    let mut o = ExportOptions::defaults(&scope, formats);
    tweak(&mut o);
    let plan = ExportPlan::new(scope, formats.to_vec(), o).expect("plan");
    let target = oma_export::TargetName::new(target).expect("name");
    let r = i.lib.open_read(&i.id).expect("reader");
    let hold = i.lib.hold_for_export(&i.id).expect("hold");
    r.with_export_snapshot(&hold, &CancelToken::new(), |snap| {
        let scope = oma_export::pipeline::scope_ids(&plan, zone)?.expect("not a search");
        let run = oma_export::pipeline::ExportRun {
            plan: &plan,
            zone,
            scope: &scope,
            export_id: oma_export::EntryId::from_bytes([9; 16]),
            created_utc_ns: MODEL_NOW_NS,
            app_version: "0.0.0-test",
            parent,
            target: &target,
            replace: oma_export::stage::Replace::Never,
            attach,
            search: None,
        };
        oma_export::pipeline::run(snap, &run, writers, &CancelToken::new(), &mut |_| {})
    })
}

/// The SHA-256 of `tests/fixtures/export/zones/SHA256SUMS` (slice 6f).
pub const ZONE_SUMS_SHA256: &str =
    "b5e68c4cb6316c0eacafaec16dcf0ec595d07d34adf258982da851a30a5a426c";

/// The fixed test zones (`tests/fixtures/export/zones/`, made by
/// `scripts/make-export-zone-tables.mjs` from Node's ICU), each checked
/// against its pin and validated by `ZoneTable::new` at the catalogues'
/// import start: `(name, zone)`.
pub fn zone_fixtures() -> Vec<(String, ExportZone)> {
    use sha2::Digest;
    let dir = fixtures("export").join("zones");
    let sums = fs::read(dir.join("SHA256SUMS")).expect("zones/SHA256SUMS");
    let hex = |b: &[u8]| -> String {
        sha2::Sha256::digest(b)
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect()
    };
    assert_eq!(hex(&sums), ZONE_SUMS_SHA256, "zones/SHA256SUMS changed");
    let mut out = Vec::new();
    for line in String::from_utf8(sums).expect("UTF-8").lines() {
        let (h, name) = line.split_once("  ").expect("sums line");
        let bytes = fs::read(dir.join(name)).expect("zone table");
        assert_eq!(hex(&bytes), h, "{name}");
        let v: Value = serde_json::from_slice(&bytes).expect("zone json");
        let w = &v["windowUtcMs"];
        let input = oma_export::ZoneTableInput {
            name: v["name"].as_str().expect("name").to_owned(),
            window_utc_ms: (w[0].as_i64().expect("from"), w[1].as_i64().expect("to")),
            transitions: v["transitions"]
                .as_array()
                .expect("transitions")
                .iter()
                .map(|t| {
                    (
                        t[0].as_i64().expect("ms"),
                        i32::try_from(t[1].as_i64().expect("offset")).expect("i32"),
                    )
                })
                .collect(),
        };
        let name = input.name.clone();
        let t = oma_export::ZoneTable::new(input, None, MODEL_NOW_NS / 1_000_000)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        out.push((name, ExportZone::Table(t)));
    }
    assert_eq!(out.len(), 4);
    out
}

/// The store file of the blob with digest `sha256` (hex) in `i`'s archive.
pub fn store_file(i: &Imported, sha256: &str) -> PathBuf {
    dunce_root(i.tmp.path())
        .join(i.id.as_str())
        .join("attachments")
        .join(&sha256[..2])
        .join(sha256)
}

/// Flip the last byte of a store file (same length, wrong digest).
pub fn corrupt(path: &Path) {
    let mut bytes = fs::read(path).expect("store file");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    #[cfg(windows)]
    {
        let mut p = fs::metadata(path).expect("meta").permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        p.set_readonly(false);
        fs::set_permissions(path, p).expect("writable");
    }
    fs::write(path, &bytes).expect("corrupt");
}
