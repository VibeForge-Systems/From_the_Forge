//! Import helpers: opening catalogue entries and synthetic backups, running
//! the import core, and reading archives back as content-free snapshots
//! and as the catalogue's content-manifest form.
//!
//! Archives are read back through a plain read-only connection **after**
//! the `Archive` is dropped (no second descriptor while the crate holds
//! one).
#![allow(dead_code, clippy::disallowed_methods, clippy::disallowed_types)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use oma_archive::{
    Archive, ArchiveCtx, ArchiveError, CancelToken, CoreOutcome, FixedClock, ImportOptions,
    ImportSummary, LineageChoice, LineageId, NoProgress,
};
use oma_backup::{
    inspect, Candidate, JobCtx, Limits, OpenBackup, Password, Progress, StagingRoot, Zeroizing,
    SMS_DB_FILE_ID,
};
use rusqlite::types::ValueRef;
use serde_json::{json, Map, Value};

use super::hex;

pub const UDID: &str = "SYNTHETIC0-0000000000000000";
pub const PASSWORD: &str = "synthetic-test-password";

/// The model's import start, 2027-01-01T00:00:00Z: every catalogue date
/// meant to count lies before it (catalogue README).
pub const MODEL_NOW_NS: i64 = 1_798_761_600 * 1_000_000_000;

/// The import catalogue's entries.
pub const ENTRIES: &[&str] = &[
    "imp-attach-plain",
    "imp-dev1-baddate-plain",
    "imp-dev1-undated-plain",
    "imp-dev1-v1-plain",
    "imp-dev1-v2-enc",
    "imp-dev1-v2-plain",
    "imp-dev2-plain",
    "imp-fts-plain",
    "imp-keys-hostile-plain",
    "imp-rd-plain",
    "imp-wal-missing-plain",
    "imp-wal-plain",
];

pub fn imports_root() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/imports"
    ))
}

pub fn backups_root() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/backups"
    ))
}

/// A JSON file under the catalogue.
pub fn catalogue_json(rel: &str) -> Value {
    let p = rel.split('/').fold(imports_root(), |p, part| p.join(part));
    serde_json::from_slice(&fs::read(p).expect("json")).expect("parse")
}

/// An opened backup with its own staging library.
pub struct Source {
    pub ob: OpenBackup,
    _staging: tempfile::TempDir,
}

/// Open the backup in `dir` (the `<UDID>` folder), staging in a temporary
/// library. The backup itself is never written.
pub fn open_dir(dir: &Path, password: Option<&str>) -> Source {
    let staging = tempfile::tempdir().expect("tempdir");
    let limits = Limits::default();
    let mut n = 0;
    let candidate = Candidate::for_dir(dir).expect("candidate");
    let inspection = inspect(&candidate, &limits, &mut n).expect("inspect");
    let root = StagingRoot::new(staging.path(), &inspection.root).expect("staging root");
    let session = root.begin().expect("begin");
    let pw =
        password.map(|p| Password::new(Zeroizing::new(p.to_owned()), &limits).expect("password"));
    let token = CancelToken::new();
    let mut sink = |_: Progress| {};
    let mut ctx = JobCtx::new(&token, &mut sink);
    let ob = OpenBackup::open(inspection, session, pw, &limits, &mut ctx)
        .unwrap_or_else(|e| panic!("open failed: {:?}", e.error));
    Source {
        ob,
        _staging: staging,
    }
}

/// Open a catalogue entry in place.
pub fn open_entry(entry: &str) -> Source {
    let pw = entry.ends_with("-enc").then_some(PASSWORD);
    open_dir(&imports_root().join(entry).join(UDID), pw)
}

/// A copy of the M2 `plain` backup whose sms.db blob is `main`, in `dir`.
/// Returns the `<UDID>` folder.
pub fn backup_with_sms(dir: &Path, main: &[u8]) -> PathBuf {
    let src = backups_root().join("plain");
    copy_tree(&src, dir);
    let udid = dir.join(UDID);
    let blob = udid.join(&SMS_DB_FILE_ID[..2]).join(SMS_DB_FILE_ID);
    fs::write(blob, main).expect("sms blob");
    udid
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("mkdir");
    for e in fs::read_dir(src).expect("read_dir").flatten() {
        let to = dst.join(e.file_name());
        if e.file_type().expect("type").is_dir() {
            copy_tree(&e.path(), &to);
        } else {
            fs::copy(e.path(), &to).expect("copy");
        }
    }
}

/// Options with the time budget off and `rows` per batch, so batch
/// boundaries depend only on the data.
pub fn opts(rows: u32) -> ImportOptions {
    let mut o = ImportOptions::default();
    o.batch = oma_archive::BatchBudget::new(rows, 32 << 20, None);
    o
}

/// Run the import core with a fixed clock (the model's import start).
pub fn import_with(
    archive: &mut Archive,
    src: &Source,
    opts: &ImportOptions,
    lineage: LineageChoice,
    cancel: &CancelToken,
) -> Result<CoreOutcome, ArchiveError> {
    let clock = FixedClock(MODEL_NOW_NS);
    let mut progress = NoProgress;
    let mut ctx = ArchiveCtx {
        cancel,
        progress: &mut progress,
        clock: &clock,
    };
    archive.import_core(&src.ob, opts, lineage, &mut ctx)
}

/// Run the import core to the end of its passes and return the summary.
pub fn import(
    archive: &mut Archive,
    src: &Source,
    opts: &ImportOptions,
    lineage: LineageChoice,
) -> ImportSummary {
    match import_with(archive, src, opts, lineage, &CancelToken::new()).expect("import") {
        CoreOutcome::PassesDone(s) => s,
        other => panic!("import did not finish its passes: {other:?}"),
    }
}

/// The lineage choice for a label: a new lineage the first time, the same
/// one afterwards.
pub fn lineage_for(labels: &mut BTreeMap<String, LineageId>, label: &str) -> LineageChoice {
    labels
        .get(label)
        .map_or(LineageChoice::New, |id| LineageChoice::Existing(*id))
}

/// A read-only connection to a closed archive's database.
pub fn read_only(db: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open")
}

/// `integrity_check`, `foreign_key_check` and the FTS rank-1 check.
pub fn assert_consistent(db: &Path) {
    let c = read_only(db);
    let ok: String = c
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .expect("integrity");
    assert_eq!(ok, "ok");
    let fk: Vec<String> = c
        .prepare("PRAGMA foreign_key_check")
        .expect("prepare")
        .query_map([], |r| r.get::<_, String>(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert!(fk.is_empty(), "foreign key problems in {fk:?}");
    drop(c);
    // The FTS check writes nothing but needs a writable connection.
    let w = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
        .expect("open rw");
    w.execute(
        "INSERT INTO message_fts(message_fts, rank) VALUES('integrity-check', 1)",
        [],
    )
    .expect("fts integrity");
}

fn value_json(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!({ "real": f.to_bits() }),
        ValueRef::Text(b) => json!({ "text": hex(b) }),
        ValueRef::Blob(b) => json!({ "blob": hex(b) }),
    }
}

/// Every row of `sql`, columns by name, with `rewrite` applied per column.
fn rows(
    c: &rusqlite::Connection,
    sql: &str,
    rewrite: &dyn Fn(&str, ValueRef<'_>) -> Option<Value>,
) -> Vec<Value> {
    let mut stmt = c.prepare(sql).expect(sql);
    let names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let mut out: Vec<Value> = stmt
        .query_map([], |r| {
            let mut m = Map::new();
            for (i, n) in names.iter().enumerate() {
                let v = r.get_ref(i)?;
                if let Some(j) = rewrite(n, v) {
                    m.insert(n.clone(), j);
                }
            }
            Ok(Value::Object(m))
        })
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    out.sort_by_key(|v| v.to_string());
    out
}

/// What [`logical_snapshot`] includes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The content tables only: what order independence compares.
    Content,
    /// Content and the session bookkeeping (cursors, maps, counters,
    /// duplicate keys, issues): what a resumed import must equal.
    WithSessions,
}

/// Every content table with ids replaced by natural keys (or `(fp, occ)`),
/// every session reference replaced by the session's content digest, and
/// times left out (spec §18.3).
pub fn logical_snapshot(db: &Path, scope: Scope) -> Value {
    let c = read_only(db);
    let mut sessions: BTreeMap<i64, String> = BTreeMap::new();
    {
        let mut s = c
            .prepare("SELECT id, content_digest FROM import_session")
            .expect("prepare");
        for r in s
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
            .expect("query")
        {
            let (id, d) = r.expect("row");
            sessions.insert(id, hex(&d));
        }
    }
    let mut keys: BTreeMap<(i64, i64), String> = BTreeMap::new();
    for (entity, table, key) in [(1i64, "handle", "natural_key"), (2, "chat", "guid")] {
        let sql = format!("SELECT id, key_kind, {key}, unkeyed_fp, unkeyed_occ FROM {table}");
        let mut s = c.prepare(&sql).expect("prepare");
        let mut q = s.query([]).expect("query");
        while let Some(r) = q.next().expect("row") {
            let id: i64 = r.get(0).expect("id");
            let kind: i64 = r.get(1).expect("kind");
            let k = if kind == 1 {
                format!("k:{}", hex(&r.get::<_, Vec<u8>>(2).expect("key")))
            } else {
                format!(
                    "u:{}:{}",
                    hex(&r.get::<_, Vec<u8>>(3).expect("fp")),
                    r.get::<_, i64>(4).expect("occ")
                )
            };
            keys.insert((entity, id), k);
        }
    }
    let session_ref = |v: ValueRef<'_>| match v {
        ValueRef::Integer(i) => json!(sessions.get(&i).cloned().unwrap_or_default()),
        other => value_json(other),
    };
    let entity_row = |entity: i64, v: ValueRef<'_>| match v {
        ValueRef::Integer(i) => json!(keys.get(&(entity, i)).cloned().unwrap_or_default()),
        other => value_json(other),
    };
    let plain = |n: &str, v: ValueRef<'_>| -> Option<Value> {
        if n == "id" {
            return None;
        }
        if n.ends_with("session_id") {
            return Some(session_ref(v));
        }
        Some(value_json(v))
    };
    let mut out = Map::new();
    out.insert(
        "handle".into(),
        json!(rows(&c, "SELECT * FROM handle", &plain)),
    );
    out.insert("chat".into(), json!(rows(&c, "SELECT * FROM chat", &plain)));
    // Rows that name an entity row by (entity, row_id).
    let by_entity = |sql: &str, row_col: &str| -> Vec<Value> {
        let mut stmt = c.prepare(sql).expect(sql);
        let names: Vec<String> = stmt
            .column_names()
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let at_entity = names.iter().position(|n| n == "entity").expect("entity");
        let mut out: Vec<Value> = stmt
            .query_map([], |r| {
                let entity: i64 = r.get(at_entity)?;
                let mut m = Map::new();
                for (i, n) in names.iter().enumerate() {
                    let v = r.get_ref(i)?;
                    let j = if n == row_col {
                        entity_row(entity, v)
                    } else if n.ends_with("session_id") {
                        session_ref(v)
                    } else {
                        value_json(v)
                    };
                    m.insert(n.clone(), j);
                }
                Ok(Value::Object(m))
            })
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        out.sort_by_key(|v| v.to_string());
        out
    };
    out.insert(
        "value_issue".into(),
        json!(by_entity("SELECT * FROM value_issue", "row_id")),
    );
    out.insert(
        "value_history".into(),
        json!(by_entity("SELECT * FROM value_history", "row_id")),
    );
    out.insert(
        "lineage_seen".into(),
        json!(by_entity("SELECT * FROM lineage_seen", "row_id")),
    );
    if scope == Scope::WithSessions {
        out.insert(
            "import_rowid_map".into(),
            json!(by_entity(
                "SELECT session_id, src AS entity, src_rowid, archive_id FROM import_rowid_map",
                "archive_id"
            )),
        );
        for t in [
            "import_cursor",
            "import_occurrence",
            "import_dup_key",
            "import_issue",
            "source_backup",
            "lineage",
        ] {
            let skip_time = |n: &str, v: ValueRef<'_>| -> Option<Value> {
                if n.ends_with("_at_ns") {
                    return None;
                }
                plain(n, v)
            };
            out.insert(
                t.into(),
                json!(rows(&c, &format!("SELECT * FROM {t}"), &skip_time)),
            );
        }
        out.insert(
            "import_session".into(),
            json!(rows(
                &c,
                "SELECT state, content_digest, rank, qualifiers, order_class, order_key, \
                        include_recently_deleted, copy_attachments, full_observation \
                 FROM import_session",
                &plain
            )),
        );
    }
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// The content-manifest form (catalogue README)
// ---------------------------------------------------------------------------

/// The source name of an archive column, as the manifest uses it.
fn source_name(table: &str, col: &str) -> String {
    match (table, col) {
        ("handle", "address") => "id".into(),
        (_, c) => c.into(),
    }
}

const TEXT_COLS: &[(&str, &str)] = &[
    ("handle", "address"),
    ("handle", "service"),
    ("handle", "country"),
    ("handle", "uncanonicalized_id"),
    ("handle", "person_centric_id"),
    ("chat", "guid"),
    ("chat", "chat_identifier"),
    ("chat", "service_name"),
    ("chat", "display_name"),
    ("chat", "room_name"),
    ("chat", "group_id"),
];

const SAMPLED: &[(&str, &[&str])] = &[
    (
        "handle",
        &[
            "address",
            "service",
            "country",
            "uncanonicalized_id",
            "person_centric_id",
        ],
    ),
    (
        "chat",
        &[
            "guid",
            "style",
            "chat_identifier",
            "service_name",
            "display_name",
            "room_name",
            "group_id",
            "is_archived",
        ],
    ),
];

const KINDS: &[(i64, &str)] = &[
    (16, "TypeMismatch"),
    (17, "BoolOutOfRange"),
    (18, "PlistInvalid"),
    (19, "PlistOverBudget"),
    (20, "PlistNotDictionary"),
];

fn text_or_hex(b: &[u8]) -> Value {
    match std::str::from_utf8(b) {
        Ok(t) => json!({ "text": t }),
        Err(_) => json!({ "hex": hex(b) }),
    }
}

/// One cell in the manifest's form.
fn cell_json(
    table: &str,
    col: &str,
    value: ValueRef<'_>,
    st: Option<i64>,
    issues: &[(i64, Option<i64>)],
) -> Value {
    let is_text = TEXT_COLS.contains(&(table, col));
    match st {
        None => {
            let mut v = match value {
                ValueRef::Blob(b) if is_text => text_or_hex(b),
                ValueRef::Blob(b) => json!({ "hex": hex(b) }),
                ValueRef::Integer(i) => json!({ "int": i }),
                other => panic!("unexpected stored value {other:?}"),
            };
            if issues.iter().any(|(k, _)| *k == 3) {
                v["storedAsBlob"] = json!(true);
            }
            v
        }
        Some(1) => json!({ "state": "null" }),
        Some(2) => json!({ "state": "absent" }),
        Some(3) => {
            let bytes = issues.iter().find(|(k, _)| *k == 1).and_then(|(_, b)| *b);
            json!({ "state": "oversized", "bytes": bytes })
        }
        Some(4) => {
            let kind = issues
                .iter()
                .find_map(|(k, _)| KINDS.iter().find(|(c, _)| c == k).map(|(_, n)| *n));
            json!({ "state": "unreadable", "kind": kind })
        }
        Some(other) => panic!("bad state {other}"),
    }
}

/// A history value in the manifest's form.
fn history_json(table: &str, col: &str, v: ValueRef<'_>) -> Value {
    let is_text = TEXT_COLS.contains(&(table, col));
    match v {
        ValueRef::Null => json!({ "state": "null" }),
        ValueRef::Integer(i) => json!({ "int": i }),
        ValueRef::Text(b) if is_text => text_or_hex(b),
        ValueRef::Blob(b) if is_text => {
            let mut j = text_or_hex(b);
            j["storedAsBlob"] = json!(true);
            j
        }
        ValueRef::Text(b) | ValueRef::Blob(b) => json!({ "hex": hex(b) }),
        ValueRef::Real(_) => panic!("real in history"),
    }
}

/// `key_kind`, the GUID or address, and a handle's service and its state.
type KeyCells = (i64, Option<Vec<u8>>, Option<Vec<u8>>, Option<i64>);

fn manifest_ref(table: &str, c: &rusqlite::Connection, id: i64) -> String {
    let (kind, guid_or_addr, service, service_st): KeyCells = if table == "handle" {
        c.query_row(
            "SELECT key_kind, address, service, service_st FROM handle WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("handle")
    } else {
        c.query_row(
            &format!("SELECT key_kind, guid, NULL, NULL FROM {table} WHERE id = ?1"),
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("row")
    };
    if kind == 2 {
        return "unkeyed".into();
    }
    let b = guid_or_addr.expect("key bytes");
    let show = |b: &[u8]| match std::str::from_utf8(b) {
        Ok(t) => t.to_owned(),
        Err(_) => format!("hex:{}", hex(b)),
    };
    if table == "handle" {
        let s = match (service, service_st) {
            (Some(s), _) => show(&s),
            (None, Some(1)) => "(null)".into(),
            (None, Some(2)) => "(absent)".into(),
            (None, Some(3)) => "(oversized)".into(),
            _ => "(unreadable)".into(),
        };
        format!("handle:{}|{s}", show(&b))
    } else {
        match std::str::from_utf8(&b) {
            Ok(t) => format!("guid:{t}"),
            Err(_) => format!("guid-hex:{}", hex(&b)),
        }
    }
}

/// The archive's handles and chats in the manifest's form: per table, a
/// list of `{ref, columns, history}`, with every unkeyed row's ref
/// `"unkeyed"` (the manifest numbers them locally; compare by content).
pub fn manifest_view(db: &Path) -> BTreeMap<&'static str, Vec<Value>> {
    let c = read_only(db);
    let mut out = BTreeMap::new();
    for (table, entity, plural) in [("handle", 1i64, "handles"), ("chat", 2, "chats")] {
        let sampled = SAMPLED
            .iter()
            .find(|(t, _)| *t == table)
            .map(|(_, cols)| *cols)
            .expect("sampled");
        let ids: Vec<i64> = c
            .prepare(&format!("SELECT id FROM {table}"))
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("ids");
        let mut list = Vec::new();
        for id in ids {
            let mut columns = Map::new();
            for col in sampled {
                let issues: Vec<(i64, Option<i64>)> = c
                    .prepare(
                        "SELECT kind, bytes FROM value_issue \
                         WHERE entity = ?1 AND row_id = ?2 AND col = ?3",
                    )
                    .expect("prepare")
                    .query_map(rusqlite::params![entity, id, code_of(table, col)], |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })
                    .expect("query")
                    .collect::<Result<_, _>>()
                    .expect("issues");
                let v = c
                    .query_row(
                        &format!("SELECT {col}, {col}_st FROM {table} WHERE id = ?1"),
                        [id],
                        |r| Ok(cell_json(table, col, r.get_ref(0)?, r.get(1)?, &issues)),
                    )
                    .expect("cell");
                columns.insert(source_name(table, col), v);
            }
            let mut history: BTreeMap<String, Vec<Value>> = BTreeMap::new();
            let mut stmt = c
                .prepare("SELECT col, value FROM value_history WHERE entity = ?1 AND row_id = ?2")
                .expect("prepare");
            let mut q = stmt.query(rusqlite::params![entity, id]).expect("query");
            while let Some(r) = q.next().expect("row") {
                let code: i64 = r.get(0).expect("col");
                let col = name_of(code);
                history
                    .entry(source_name(table, col))
                    .or_default()
                    .push(history_json(table, col, r.get_ref(1).expect("value")));
            }
            for v in history.values_mut() {
                v.sort_by_key(|x| x.to_string());
            }
            list.push(json!({
                "ref": manifest_ref(table, &c, id),
                "columns": Value::Object(columns),
                "history": history,
            }));
        }
        list.sort_by_key(|v| v.to_string());
        out.insert(plural, list);
    }
    out
}

/// `{ref, columns, history}` of a manifest's handles or chats, with
/// unkeyed refs replaced by `"unkeyed"` and history lists sorted as
/// [`manifest_view`] sorts them.
pub fn manifest_expected(m: &Value, plural: &str) -> Vec<Value> {
    let mut out: Vec<Value> = m["content"][plural]
        .as_array()
        .expect("list")
        .iter()
        .map(|r| {
            let rf = r["ref"].as_str().expect("ref");
            let rf = if rf.starts_with("unkeyed:") {
                "unkeyed"
            } else {
                rf
            };
            let mut history = r["history"].clone();
            if let Some(h) = history.as_object_mut() {
                for v in h.values_mut() {
                    if let Some(a) = v.as_array_mut() {
                        a.sort_by_key(|x| x.to_string());
                    }
                }
            }
            json!({ "ref": rf, "columns": r["columns"], "history": history })
        })
        .collect();
    out.sort_by_key(|v| v.to_string());
    out
}

fn code_of(table: &str, col: &str) -> i64 {
    let entity = match table {
        "handle" => oma_archive::schema::codes::Entity::Handle,
        _ => oma_archive::schema::codes::Entity::Chat,
    };
    oma_archive::schema::codes::ARCHIVE_COLUMNS
        .iter()
        .find(|c| c.entity == entity && c.name == col)
        .map(|c| i64::from(c.code))
        .expect("column")
}

fn name_of(code: i64) -> &'static str {
    u16::try_from(code)
        .ok()
        .and_then(oma_archive::schema::codes::ArchiveColumn::from_code)
        .map(|c| c.name)
        .expect("column code")
}

/// The counts of one entity in an `import.json`-style object, as
/// `ImportCounts` serialises them.
pub fn counts_json(c: &oma_archive::EntityCounts) -> Value {
    serde_json::to_value(c).expect("counts")
}

// ---------------------------------------------------------------------------
// A synthetic sms.db with many handles and chats
// ---------------------------------------------------------------------------

/// A seeded xorshift (deterministic shuffles and choices).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.max(1))
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = usize::try_from(self.below(u64::try_from(i + 1).expect("n"))).expect("j");
            v.swap(i, j);
        }
    }
}

fn declared(kind: Option<oma_messages::ColumnKind>) -> &'static str {
    use oma_messages::ColumnKind;
    match kind {
        Some(ColumnKind::RowId) => "INTEGER PRIMARY KEY",
        Some(ColumnKind::Integer | ColumnKind::Bool | ColumnKind::Time) => "INTEGER",
        Some(ColumnKind::Text) => "TEXT",
        _ => "BLOB",
    }
}

/// Every known table with every known column, built from the reader's own
/// static list (a synthetic stand-in, not any real schema).
fn full_schema() -> String {
    use oma_messages::KnownTable;
    let mut s = String::new();
    for t in KnownTable::ALL {
        let cols: Vec<String> = t
            .known_columns()
            .map(|c| format!("\"{}\" {}", c.name(), declared(c.kind())))
            .collect();
        let cols = if cols.is_empty() {
            match t {
                KnownTable::RecoverableMessagePart => {
                    "chat_id INTEGER, message_id INTEGER, part_index INTEGER, part_text BLOB"
                        .to_owned()
                }
                _ => "id INTEGER, guid TEXT".to_owned(),
            }
        } else {
            cols.join(", ")
        };
        s.push_str(&format!("CREATE TABLE \"{}\" ({cols});\n", t.name()));
    }
    s.push_str(
        "INSERT INTO _SqliteDatabaseProperties (key, value) VALUES ('_ClientVersion', '17001');\n",
    );
    s
}

/// A synthetic sms.db: `handles` handle rows and `chats` chat rows with
/// shuffled rowids that include `i64::MIN` and `i64::MAX`, duplicated keys
/// (every occurrence must be unkeyed), identical keyless rows (same
/// fingerprint, counted occurrences), empty identifiers and NULLs. Every
/// value is synthetic.
pub fn synthetic_sms(seed: u64, handles: usize, chats: usize) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("sms.db");
    let c = rusqlite::Connection::open(&path).expect("create");
    c.execute_batch(&full_schema()).expect("schema");
    let rowids = |rng: &mut Rng, n: usize| -> Vec<i64> {
        let mut ids: Vec<i64> = vec![i64::MIN, i64::MAX, -7, 0];
        let mut next = 1i64;
        while ids.len() < n {
            next += 1 + i64::try_from(rng.below(3)).expect("small");
            ids.push(next);
        }
        ids.truncate(n);
        rng.shuffle(&mut ids);
        ids
    };
    let hids = rowids(&mut rng, handles);
    for (i, rowid) in hids.iter().enumerate() {
        // Every 9th row repeats an earlier address and service; every 11th
        // has no address (identical rows); every 13th an empty one.
        let (id, service): (Option<String>, &str) = if i % 11 == 5 {
            (None, "SMS")
        } else if i % 13 == 6 {
            (Some(String::new()), "iMessage")
        } else if i % 9 == 8 {
            (Some(format!("+1555SYNTH{:04}", i % 4)), "SMS")
        } else {
            (
                Some(format!("+1555SYNTH{i:04}")),
                if i % 2 == 0 { "SMS" } else { "iMessage" },
            )
        };
        c.execute(
            "INSERT INTO handle (ROWID, id, service, country, uncanonicalized_id, person_centric_id) \
             VALUES (?1, ?2, ?3, 'us', ?4, NULL)",
            rusqlite::params![rowid, id, service, (i % 3 == 0).then(|| format!("SYNTH-PERSON-{i}"))],
        )
        .expect("handle");
    }
    let cids = rowids(&mut rng, chats);
    for (i, rowid) in cids.iter().enumerate() {
        let guid: Option<String> = if i % 10 == 4 {
            None
        } else if i % 7 == 6 {
            Some(format!("SYNTH-CHAT-GUID-{}", i % 3))
        } else {
            Some(format!("SYNTH-CHAT-GUID-{i}"))
        };
        c.execute(
            "INSERT INTO chat (ROWID, guid, style, chat_identifier, service_name, display_name, \
                 room_name, group_id, is_archived) \
             VALUES (?1, ?2, 45, ?3, 'iMessage', ?4, NULL, ?5, ?6)",
            rusqlite::params![
                rowid,
                guid,
                format!("chatSYNTH{i}"),
                (i % 4 == 0).then(|| format!("SYNTH-CHAT-NAME-{i}")),
                format!("SYNTH-GROUP-{i}"),
                i64::try_from(i % 2).expect("bool")
            ],
        )
        .expect("chat");
    }
    drop(c);
    fs::read(&path).expect("read")
}
