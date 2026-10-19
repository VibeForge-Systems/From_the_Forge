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
    Archive, ArchiveCtx, ArchiveError, CancelToken, FixedClock, ImportOptions, ImportOutcome,
    ImportSummary, LineageChoice, LineageId, NoProgress, ProceedWith,
};
use oma_backup::{
    inspect, Candidate, JobCtx, Limits, OpenBackup, Password, Progress, StagingRoot, Zeroizing,
    SMS_DB_FILE_ID,
};
use rusqlite::types::ValueRef;
use rusqlite::OptionalExtension;
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

/// Where a test's staging libraries go when set (the crash harness points
/// its children inside a directory the parent removes, because a killed
/// child never drops its own).
pub const ENV_STAGING: &str = "OMA_TEST_STAGING";

/// Open the backup in `dir` (the `<UDID>` folder), staging in a temporary
/// library (under [`ENV_STAGING`] when that is set). The backup itself is
/// never written.
pub fn open_dir(dir: &Path, password: Option<&str>) -> Source {
    open_dir_with(dir, password, &Limits::default())
}

/// [`open_dir`] with other limits.
pub fn open_dir_with(dir: &Path, password: Option<&str>, limits: &Limits) -> Source {
    let staging = match std::env::var_os(ENV_STAGING) {
        Some(parent) => tempfile::tempdir_in(parent).expect("tempdir"),
        None => tempfile::tempdir().expect("tempdir"),
    };
    let limits = limits.clone();
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

/// A copy of a catalogue entry's backup in `dir` (for tests that change a
/// backup; the catalogue itself is never written). Returns the `<UDID>`
/// folder.
pub fn copy_entry(entry: &str, dir: &Path) -> PathBuf {
    copy_tree(&imports_root().join(entry), dir);
    dir.join(UDID)
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

/// Run the import with a fixed clock (the model's import start).
pub fn import_with(
    archive: &mut Archive,
    src: &Source,
    opts: &ImportOptions,
    lineage: LineageChoice,
    cancel: &CancelToken,
) -> Result<ImportOutcome, ArchiveError> {
    import_at(archive, src, opts, lineage, cancel, MODEL_NOW_NS)
}

/// Run the import with the clock fixed at `now_ns`.
pub fn import_at(
    archive: &mut Archive,
    src: &Source,
    opts: &ImportOptions,
    lineage: LineageChoice,
    cancel: &CancelToken,
    now_ns: i64,
) -> Result<ImportOutcome, ArchiveError> {
    let clock = FixedClock(now_ns);
    let mut progress = NoProgress;
    let mut ctx = ArchiveCtx {
        cancel,
        progress: &mut progress,
        clock: &clock,
    };
    archive.import(&src.ob, opts, &mut ProceedWith(lineage), &mut ctx)
}

/// Run the import to completion (a gate that proceeds with `lineage`) and
/// return the summary.
pub fn import(
    archive: &mut Archive,
    src: &Source,
    opts: &ImportOptions,
    lineage: LineageChoice,
) -> ImportSummary {
    match import_with(archive, src, opts, lineage, &CancelToken::new()).expect("import") {
        ImportOutcome::Completed(s) => s,
        other => panic!("import did not complete: {other:?}"),
    }
}

/// Cancels the import when the chat pass starts: P1's rows, map and
/// occurrence counters, and the session's duplicate keys, are committed.
pub struct CancelAtChats(pub CancelToken);

impl oma_archive::ArchiveSink for CancelAtChats {
    fn event(&mut self, event: oma_archive::ArchiveEvent) {
        if let oma_archive::ArchiveEvent::Import {
            phase: oma_archive::ImportPhase::ImportingChats,
            ..
        } = event
        {
            self.0.cancel();
        }
    }
}

/// Import until the chat pass starts, then cancel: an unfinished session.
pub fn cancelled_at_chats(
    a: &mut oma_archive::Archive,
    src: &Source,
    o: &oma_archive::ImportOptions,
    lineage: LineageChoice,
) -> oma_archive::ImportSummary {
    let token = CancelToken::new();
    let mut sink = CancelAtChats(token.clone());
    let clock = FixedClock(MODEL_NOW_NS);
    let out = a
        .import(
            &src.ob,
            o,
            &mut oma_archive::ProceedWith(lineage),
            &mut ArchiveCtx {
                cancel: &token,
                progress: &mut sink,
                clock: &clock,
            },
        )
        .expect("import");
    match out {
        ImportOutcome::Cancelled(Some(s)) => s,
        other => panic!("not cancelled: {other:?}"),
    }
}

/// The lineage choice for a label: a new lineage the first time, the same
/// one afterwards.
pub fn lineage_for(labels: &mut BTreeMap<String, LineageId>, label: &str) -> LineageChoice {
    labels
        .get(label)
        .map_or(LineageChoice::New, |id| LineageChoice::Existing(*id))
}

/// Session `session` is completed (finalize ran) and left no session-local
/// rows.
pub fn assert_completed(db: &Path, session: i64) {
    let c = read_only(db);
    let (state, local): (i64, i64) = c
        .query_row(
            "SELECT state, (SELECT count(*) FROM import_rowid_map WHERE session_id = ?1) \
                  + (SELECT count(*) FROM import_occurrence WHERE session_id = ?1) \
                  + (SELECT count(*) FROM import_dup_key WHERE session_id = ?1) \
             FROM import_session WHERE id = ?1",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("session");
    assert_eq!((state, local), (1, 0), "session {session}");
}

/// A read-only connection to a closed archive's database.
pub fn read_only(db: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open")
}

/// The archive database files a crash can leave: the main file, `-wal`
/// and `-shm`.
pub const DB_SIDES: [&str; 3] = ["", "-wal", "-shm"];

/// [`assert_consistent`] on a copy of the database, its `-wal` and its
/// `-shm`, so the checks change nothing the next open sees (the last
/// read-write connection to close checkpoints and deletes the `-wal`).
/// Only for a database no connection has open.
/// Also checks that no `blob` row references a missing or wrong file
/// (spec §18.4); files without rows are allowed here (the next write open
/// reconciles them).
pub fn assert_consistent_copy(db: &Path) {
    let scratch = tempfile::tempdir().expect("tempdir");
    let name = db.file_name().expect("name").to_string_lossy().into_owned();
    for side in DB_SIDES {
        let from = db.with_file_name(format!("{name}{side}"));
        if from.exists() {
            fs::copy(&from, scratch.path().join(format!("{name}{side}"))).expect("copy");
        }
    }
    let copy = scratch.path().join(&name);
    assert_store_consistent_at(&copy, db.parent().expect("archive dir"), false);
    assert_consistent(&copy);
}

/// The `-wal` next to `db`, if it exists: its bytes' hash.
pub fn wal_state(db: &Path) -> Option<String> {
    let name = db.file_name().expect("name").to_string_lossy().into_owned();
    super::file_hash(&db.with_file_name(format!("{name}-wal")))
}

/// `integrity_check`, `foreign_key_check` and the FTS rank-1 check. The
/// FTS check opens a read-write connection, which checkpoints and deletes
/// the `-wal` when it closes: use [`assert_consistent_copy`] where the next
/// step must see the files as they are.
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
    for (entity, table, key) in [
        (1i64, "handle", "natural_key"),
        (2, "chat", "guid"),
        (3, "message", "guid"),
        (4, "attachment", "guid"),
    ] {
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
    // Body versions by (message key, raw_hash), as entity 5.
    {
        let mut s = c
            .prepare("SELECT id, message_id, raw_hash FROM message_body")
            .expect("prepare");
        let mut q = s.query([]).expect("query");
        while let Some(r) = q.next().expect("row") {
            let id: i64 = r.get(0).expect("id");
            let m: i64 = r.get(1).expect("message");
            let h: Vec<u8> = r.get(2).expect("hash");
            let mk = keys.get(&(3, m)).cloned().unwrap_or_default();
            keys.insert((5, id), format!("b:{mk}:{}", hex(&h)));
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
    // Message, attachment and link tables, with every row reference
    // replaced by its key.
    let mut blobs: BTreeMap<i64, String> = BTreeMap::new();
    {
        let mut s = c.prepare("SELECT id, sha256 FROM blob").expect("prepare");
        for r in s
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))
            .expect("query")
        {
            let (id, sha) = r.expect("row");
            blobs.insert(id, hex(&sha));
        }
    }
    let refs = |n: &str, v: ValueRef<'_>| -> Option<Value> {
        if n == "current_blob_id" || n == "blob_id" {
            return Some(match v {
                ValueRef::Integer(i) => json!(blobs.get(&i).cloned().unwrap_or_default()),
                other => value_json(other),
            });
        }
        let entity = match n {
            "handle_ref" | "other_handle_ref" | "handle_id" => 1,
            "chat_id" | "chat_ref" => 2,
            "message_id" => 3,
            "attachment_id" => 4,
            "current_body_id" | "body_id" => 5,
            _ => return plain(n, v),
        };
        Some(entity_row(entity, v))
    };
    for t in [
        "message",
        "message_body",
        "message_body_part",
        "message_edit_entry",
        "message_unsent_part",
        "attachment",
        "chat_handle",
        "chat_message",
        "message_attachment",
        "message_recently_deleted",
    ] {
        out.insert(
            t.into(),
            json!(rows(&c, &format!("SELECT * FROM {t}"), &refs)),
        );
    }
    // The store: its rows (without the session that first wrote one,
    // which depends on the import order) and its files.
    out.insert(
        "blob".into(),
        json!(rows(
            &c,
            "SELECT sha256, byte_len, storage_name, verified_at_ns, verify_state FROM blob",
            &plain
        )),
    );
    out.insert("store".into(), json!(store_files(db)));
    let fts: i64 = c
        .query_row("SELECT count(*) FROM message_fts", [], |r| r.get(0))
        .expect("fts rows");
    out.insert("message_fts_rows".into(), json!(fts));
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
        out.insert(
            "attachment_observation".into(),
            json!(rows(&c, "SELECT * FROM attachment_observation", &refs)),
        );
        out.insert(
            "blob_sessions".into(),
            json!(rows(
                &c,
                "SELECT sha256, first_session_id FROM blob",
                &plain
            )),
        );
        for t in [
            "import_cursor",
            "import_occurrence",
            "import_dup_key",
            "import_issue",
            "source_backup",
            "lineage",
            "blob_intent",
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

/// The store files of the archive holding `db`: `<hh>/<name>` → SHA-256
/// of the content, without `.incoming`.
pub fn store_files(db: &Path) -> BTreeMap<String, String> {
    let att = db.parent().expect("archive dir").join("attachments");
    super::tree(&att)
        .into_iter()
        .filter(|(k, v)| !k.starts_with(".incoming") && v != "dir")
        .collect()
}

/// The archive's `blob` rows in the manifest's `blobs` form, sorted.
pub fn blob_list(db: &Path) -> Value {
    let c = read_only(db);
    let v: Vec<Value> = c
        .prepare("SELECT sha256, byte_len FROM blob ORDER BY lower(hex(sha256))")
        .expect("prepare")
        .query_map([], |r| {
            Ok(json!({ "byteLen": r.get::<_, i64>(1)?, "sha256": hex(&r.get::<_, Vec<u8>>(0)?) }))
        })
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    json!(v)
}

/// The partial files in `attachments/.incoming/`.
pub fn partial_files(db: &Path) -> usize {
    let inc = db
        .parent()
        .expect("archive dir")
        .join("attachments")
        .join(".incoming");
    fs::read_dir(inc).map_or(0, |d| d.count())
}

/// The store agrees with its rows (spec §18.4): every `blob` row's file is
/// a regular file of `byte_len` whose content hashes to `sha256`; with
/// `no_orphans`, every store file has a `blob` row, and no `blob_intent`
/// row is left.
pub fn assert_store_consistent(db: &Path, no_orphans: bool) {
    assert_store_consistent_at(db, db.parent().expect("archive dir"), no_orphans);
}

/// [`assert_store_consistent`] with the rows read from `db` (for example a
/// copy of the database files) and the store under `archive`.
pub fn assert_store_consistent_at(db: &Path, archive: &Path, no_orphans: bool) {
    let c = read_only(db);
    let rows: Vec<(Vec<u8>, i64, String)> = c
        .prepare("SELECT sha256, byte_len, storage_name FROM blob")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    let files = store_files(&archive.join("archive.sqlite"));
    let att = archive.join("attachments");
    for (sha, len, name) in &rows {
        assert_eq!(&hex(sha), name);
        let path = att.join(&name[..2]).join(name);
        let meta = fs::symlink_metadata(&path)
            .unwrap_or_else(|_| panic!("a blob row references a missing file"));
        assert!(meta.is_file(), "not a regular file");
        assert_eq!(i64::try_from(meta.len()).expect("len"), *len);
        assert_eq!(super::file_hash(&path).as_deref(), Some(name.as_str()));
    }
    if no_orphans {
        let named: std::collections::BTreeSet<String> = rows
            .iter()
            .map(|(_, _, n)| format!("{}|{n}", &n[..2]))
            .collect();
        for k in files.keys() {
            assert!(named.contains(k), "an unreferenced store file: {k}");
        }
        let intents: i64 = c
            .query_row("SELECT count(*) FROM blob_intent", [], |r| r.get(0))
            .expect("intents");
        assert_eq!(intents, 0, "blob intents left");
    }
    // Every current blob and every observed blob is a stored one.
    let dangling: i64 = c
        .query_row(
            "SELECT (SELECT count(*) FROM attachment WHERE current_blob_id IS NOT NULL \
                 AND current_blob_id NOT IN (SELECT id FROM blob)) + \
             (SELECT count(*) FROM attachment_observation WHERE blob_id IS NOT NULL \
                 AND blob_id NOT IN (SELECT id FROM blob))",
            [],
            |r| r.get(0),
        )
        .expect("dangling");
    assert_eq!(dangling, 0);
}

// ---------------------------------------------------------------------------
// The content-manifest form (catalogue README)
// ---------------------------------------------------------------------------

/// The source name of an archive column, as the manifest uses it.
fn source_name(table: &str, col: &str) -> String {
    match (table, col) {
        ("handle", "address") => "id".into(),
        ("message", "assoc_type") => "associated_message_type".into(),
        ("message", "assoc_guid_raw") => "associated_message_guid".into(),
        ("attachment", "emoji_description") => "emoji_image_short_description".into(),
        (_, c) => c.into(),
    }
}

/// The sampled message columns (archive names) and whether each is a
/// timestamp quad.
const MESSAGE_SAMPLED: &[(&str, bool)] = &[
    ("guid", false),
    ("is_from_me", false),
    ("service", false),
    ("date", true),
    ("is_read", false),
    ("is_delivered", false),
    ("subject", false),
    ("item_type", false),
    ("group_action_type", false),
    ("group_title", false),
    ("assoc_type", false),
    ("assoc_guid_raw", false),
    ("payload_data", false),
    ("date_edited", true),
    ("date_retracted", true),
    ("cache_has_attachments", false),
    ("destination_caller_id", false),
];

fn unit_name(u: i64) -> &'static str {
    match u {
        0 => "zero",
        1 => "negative",
        2 => "seconds",
        3 => "nanoseconds",
        _ => "ambiguous",
    }
}

const TEXT_COLS: &[(&str, &str)] = &[
    ("message", "guid"),
    ("message", "service"),
    ("message", "subject"),
    ("message", "group_title"),
    ("message", "assoc_guid_raw"),
    ("message", "destination_caller_id"),
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
    ("attachment", "guid"),
    ("attachment", "filename"),
    ("attachment", "mime_type"),
    ("attachment", "transfer_name"),
    ("attachment", "emoji_description"),
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
    (
        "attachment",
        &[
            "guid",
            "filename",
            "mime_type",
            "transfer_name",
            "total_bytes",
            "is_sticker",
            "emoji_description",
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

pub fn manifest_ref(table: &str, c: &rusqlite::Connection, id: i64) -> String {
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

/// Lineage ids by the labels a test gave them (`L1`, `L2`: the manifest's
/// names for the user's choices at the gate).
pub type LineageLabels = BTreeMap<i64, String>;

/// The labels of a single import into an empty archive: its one lineage is
/// `L1`.
pub fn single_lineage(db: &Path) -> LineageLabels {
    let c = read_only(db);
    let ids: Vec<i64> = c
        .prepare("SELECT id FROM lineage ORDER BY id")
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("ids");
    ids.into_iter()
        .enumerate()
        .map(|(i, id)| (id, format!("L{}", i + 1)))
        .collect()
}

/// The lineages that label a row "not in the latest import" (spec §8.7):
/// those whose newest **full observation** ranks above the highest rank at
/// which that lineage saw the row. Finalize sets `full_observation`.
fn not_in_latest(c: &rusqlite::Connection, entity: i64, id: i64, labels: &LineageLabels) -> Value {
    let mut out: Vec<String> = c
        .prepare(
            "SELECT s.lineage_id FROM lineage_seen s \
             WHERE s.entity = ?1 AND s.row_id = ?2 AND s.last_rank < \
               (SELECT max(rank) FROM import_session i \
                WHERE i.lineage_id = s.lineage_id AND i.full_observation = 1)",
        )
        .expect("prepare")
        .query_map(rusqlite::params![entity, id], |r| r.get::<_, i64>(0))
        .expect("query")
        .map(|l| {
            let l = l.expect("lineage");
            labels
                .get(&l)
                .cloned()
                .unwrap_or_else(|| format!("lineage {l}"))
        })
        .collect();
    out.sort();
    json!(out)
}

/// A list of manifest refs with every unkeyed ref collapsed to `"unkeyed"`
/// (the manifest numbers unkeyed rows locally), sorted; `dedup` for the
/// manifest's sets of names.
fn collapse(refs: impl Iterator<Item = String>, dedup: bool) -> Value {
    let mut v: Vec<String> = refs
        .map(|r| {
            if r.starts_with("unkeyed") {
                "unkeyed".to_owned()
            } else {
                r
            }
        })
        .collect();
    v.sort();
    if dedup {
        v.dedup();
    }
    json!(v)
}

/// The ids `sql` (one `?1` parameter) returns.
fn ids_of(c: &rusqlite::Connection, sql: &str, p: i64) -> Vec<i64> {
    c.prepare(sql)
        .expect("prepare")
        .query_map([p], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("ids")
}

/// The archive's handles, chats and attachments in the manifest's form:
/// per table, a list of `{ref, columns, history}`, with every unkeyed
/// row's ref `"unkeyed"` (the manifest numbers them locally; compare by
/// content). Chats also carry their `handles` (P4) and, like attachments,
/// `notInLatest`; attachments carry their current blob (`sha256`) and every
/// blob a session read for them (`blobs`).
pub fn manifest_view(db: &Path, labels: &LineageLabels) -> BTreeMap<&'static str, Vec<Value>> {
    let c = read_only(db);
    let mut out = BTreeMap::new();
    for (table, entity, plural) in [
        ("handle", 1i64, "handles"),
        ("chat", 2, "chats"),
        ("attachment", 4, "attachments"),
    ] {
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
                let issues = issues_of(&c, table, id, col);
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
            let mut row = json!({
                "ref": manifest_ref(table, &c, id),
                "columns": Value::Object(columns),
                "history": history,
            });
            if table == "chat" {
                let handles = ids_of(
                    &c,
                    "SELECT handle_id FROM chat_handle WHERE chat_id = ?1",
                    id,
                );
                row["handles"] = collapse(
                    handles.into_iter().map(|h| manifest_ref("handle", &c, h)),
                    false,
                );
            }
            if table != "handle" {
                row["notInLatest"] = not_in_latest(&c, entity, id, labels);
            }
            if table == "attachment" {
                let current: Option<Vec<u8>> = c
                    .query_row(
                        "SELECT b.sha256 FROM attachment a JOIN blob b ON b.id = a.current_blob_id \
                         WHERE a.id = ?1",
                        [id],
                        |r| r.get(0),
                    )
                    .optional()
                    .expect("current blob");
                row["sha256"] = current.map_or(Value::Null, |s| json!(hex(&s)));
                let mut read: Vec<String> = c
                    .prepare(
                        "SELECT DISTINCT b.sha256 FROM attachment_observation o \
                         JOIN blob b ON b.id = o.blob_id WHERE o.attachment_id = ?1",
                    )
                    .expect("prepare")
                    .query_map([id], |r| r.get::<_, Vec<u8>>(0))
                    .expect("query")
                    .map(|r| hex(&r.expect("row")))
                    .collect();
                read.sort();
                row["blobs"] = json!(read);
            }
            list.push(row);
        }
        list.sort_by_key(|v| v.to_string());
        out.insert(plural, list);
    }
    out
}

/// A manifest's handles, chats or attachments in [`manifest_view`]'s form,
/// with unkeyed refs replaced by `"unkeyed"` and history lists sorted as
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
            let mut row = json!({ "ref": rf, "columns": r["columns"], "history": history });
            if plural == "chats" {
                row["handles"] = collapse(str_list(&r["handles"]), false);
            }
            if plural != "handles" {
                row["notInLatest"] = r["notInLatest"].clone();
            }
            if plural == "attachments" {
                row["sha256"] = r["sha256"].clone();
                row["blobs"] = r["blobs"].clone();
            }
            row
        })
        .collect();
    out.sort_by_key(|v| v.to_string());
    out
}

/// The strings of a JSON list.
fn str_list(v: &Value) -> impl Iterator<Item = String> + '_ {
    v.as_array()
        .expect("list")
        .iter()
        .map(|x| x.as_str().expect("string").to_owned())
}

fn code_of(table: &str, col: &str) -> i64 {
    let entity = match table {
        "handle" => oma_archive::schema::codes::Entity::Handle,
        "message" => oma_archive::schema::codes::Entity::Message,
        "attachment" => oma_archive::schema::codes::Entity::Attachment,
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

fn issues_of(c: &rusqlite::Connection, table: &str, id: i64, col: &str) -> Vec<(i64, Option<i64>)> {
    let entity = match table {
        "handle" => 1,
        "chat" => 2,
        "attachment" => 4,
        _ => 3,
    };
    c.prepare("SELECT kind, bytes FROM value_issue WHERE entity = ?1 AND row_id = ?2 AND col = ?3")
        .expect("prepare")
        .query_map(rusqlite::params![entity, id, code_of(table, col)], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("issues")
}

/// A body version's display in the manifest's form.
fn display_of(source: Option<i64>, reason: Option<i64>, text: Option<String>) -> Value {
    match (source, text) {
        (Some(s), Some(t)) => json!({
            "text": t,
            "source": match s {
                1 => "attributedBody",
                2 => "text",
                _ => "textLossy",
            }
        }),
        _ => json!({
            "noneReason": match reason {
                Some(1) => "attributedBodyUnusable",
                Some(2) => "textEmpty",
                _ => "noSource",
            }
        }),
    }
}

/// id, handle_ref, current_body_id, sort_key, first_rank, first_src_rowid.
type MessageKeys = (i64, Option<i64>, Option<i64>, Vec<u8>, Vec<u8>, i64);
/// id, display_source, display_none_reason, display_text.
type BodyDisplay = (i64, Option<i64>, Option<i64>, Option<String>);

/// The archive's messages in the manifest's form, in archive order
/// (`sort_key`, then id): `{ref, sort, handle, display, bodies, columns,
/// history, chats, attachments, recentlyDeleted, notInLatest}`, with
/// unkeyed refs as `"unkeyed"`. `attachments` is in the order of spec
/// §6.5: `ord_rank` descending, `ord_rowid` ascending.
pub fn message_view(db: &Path, labels: &LineageLabels) -> Vec<Value> {
    let c = read_only(db);
    let ids: Vec<MessageKeys> = c
        .prepare(
            "SELECT id, handle_ref, current_body_id, sort_key, first_rank, first_src_rowid              FROM message ORDER BY sort_key, id",
        )
        .expect("prepare")
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("ids");
    let mut out = Vec::new();
    for (id, handle, current, sort_key, first_rank, first_src) in ids {
        let mut columns = Map::new();
        for (col, quad) in MESSAGE_SAMPLED {
            let issues = issues_of(&c, "message", id, col);
            let v = if *quad {
                c.query_row(
                    &format!(
                        "SELECT {col}_raw, {col}_st, {col}_unit, {col}_utc_ns FROM message WHERE id = ?1"
                    ),
                    [id],
                    |r| {
                        let st: Option<i64> = r.get(1)?;
                        Ok(match st {
                            None => json!({
                                "raw": r.get::<_, i64>(0)?,
                                "unit": unit_name(r.get(2)?),
                                "utcNs": r.get::<_, Option<i64>>(3)?,
                            }),
                            st => cell_json("message", col, ValueRef::Null, st, &issues),
                        })
                    },
                )
                .expect("quad")
            } else {
                c.query_row(
                    &format!("SELECT {col}, {col}_st FROM message WHERE id = ?1"),
                    [id],
                    |r| Ok(cell_json("message", col, r.get_ref(0)?, r.get(1)?, &issues)),
                )
                .expect("cell")
            };
            columns.insert(source_name("message", col), v);
        }
        let mut history: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        let mut stmt = c
            .prepare("SELECT col, value FROM value_history WHERE entity = 3 AND row_id = ?1")
            .expect("prepare");
        let mut q = stmt.query([id]).expect("query");
        while let Some(r) = q.next().expect("row") {
            let code: i64 = r.get(0).expect("col");
            let col = name_of(code);
            history
                .entry(source_name("message", col))
                .or_default()
                .push(history_json("message", col, r.get_ref(1).expect("value")));
        }
        for v in history.values_mut() {
            v.sort_by_key(|x| x.to_string());
        }
        let bodies: Vec<BodyDisplay> = c
            .prepare(
                "SELECT id, display_source, display_none_reason, display_text                  FROM message_body WHERE message_id = ?1",
            )
            .expect("prepare")
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("bodies");
        let mut display = Value::Null;
        let mut list: Vec<Value> = Vec::new();
        for (bid, source, reason, text) in bodies {
            let d = display_of(source, reason, text);
            if Some(bid) == current {
                display = d.clone();
            }
            let mut b = d;
            b["current"] = json!(Some(bid) == current);
            list.push(b);
        }
        list.sort_by_key(|b| (b["current"] != true, b.to_string()));
        let sort = if sort_key.first() == Some(&0) {
            let ns: i64 = c
                .query_row("SELECT date_utc_ns FROM message WHERE id = ?1", [id], |r| {
                    r.get(0)
                })
                .expect("utc");
            json!({ "utcNs": ns })
        } else {
            json!({ "undated": { "firstRank": hex(&first_rank), "firstSrcRowid": first_src } })
        };
        let handle = handle.map(|h| {
            let r = manifest_ref("handle", &c, h);
            Value::String(r)
        });
        let chats = ids_of(
            &c,
            "SELECT chat_id FROM chat_message WHERE message_id = ?1",
            id,
        );
        let attachments = ids_of(
            &c,
            "SELECT attachment_id FROM message_attachment WHERE message_id = ?1 \
             ORDER BY ord_rank DESC, ord_rowid ASC",
            id,
        );
        let markers: Vec<String> = c
            .prepare("SELECT chat_ref FROM message_recently_deleted WHERE message_id = ?1")
            .expect("prepare")
            .query_map([id], |r| r.get::<_, Option<i64>>(0))
            .expect("query")
            .map(|r| match r.expect("marker") {
                Some(chat) => manifest_ref("chat", &c, chat),
                None => "(unresolved)".to_owned(),
            })
            .collect();
        out.push(json!({
            "ref": manifest_ref("message", &c, id),
            "sort": sort,
            "handle": handle,
            "display": display,
            "bodies": list,
            "columns": Value::Object(columns),
            "history": history,
            "chats": collapse(chats.into_iter().map(|x| manifest_ref("chat", &c, x)), false),
            "attachments": attachments
                .into_iter()
                .map(|a| {
                    let r = manifest_ref("attachment", &c, a);
                    json!(if r.starts_with("unkeyed") { "unkeyed".to_owned() } else { r })
                })
                .collect::<Vec<_>>(),
            "recentlyDeleted": collapse(markers.into_iter(), true),
            "notInLatest": not_in_latest(&c, 3, id, labels),
        }));
    }
    out
}

/// A manifest's messages in [`message_view`]'s form, in the manifest's
/// order.
pub fn message_expected(m: &Value) -> Vec<Value> {
    let unkeyed = |v: &Value| match v.as_str() {
        Some(r) if r.starts_with("unkeyed:") => json!("unkeyed"),
        _ => v.clone(),
    };
    m["content"]["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .map(|r| {
            let mut history = r["history"].clone();
            if let Some(h) = history.as_object_mut() {
                for v in h.values_mut() {
                    if let Some(a) = v.as_array_mut() {
                        a.sort_by_key(|x| x.to_string());
                    }
                }
            }
            let mut bodies: Vec<Value> = r["bodies"].as_array().expect("bodies").clone();
            bodies.sort_by_key(|b| (b["current"] != true, b.to_string()));
            json!({
                "ref": unkeyed(&r["ref"]),
                "sort": r["sort"],
                "handle": unkeyed(&r["handle"]),
                "display": r["display"],
                "bodies": bodies,
                "columns": r["columns"],
                "history": history,
                "chats": collapse(str_list(&r["chats"]), false),
                "attachments": r["attachments"]
                    .as_array()
                    .expect("attachments")
                    .iter()
                    .map(unkeyed)
                    .collect::<Vec<_>>(),
                "recentlyDeleted": collapse(str_list(&r["recentlyDeleted"]), true),
                "notInLatest": r["notInLatest"],
            })
        })
        .collect()
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
pub fn full_schema() -> String {
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

/// Which of [`synthetic_sms`]'s message rows it lists in Recently Deleted:
/// positions among the rows with a rowid other than `i64::MIN` and
/// `i64::MAX`, in insertion order.
pub const SYNTHETIC_RD_POSITIONS: [usize; 3] = [5, 17, 29];

/// A synthetic sms.db: `handles` handle rows, `chats` chat rows,
/// `messages` message rows and `messages / 3` (at least 6) attachment rows
/// with shuffled rowids that include `i64::MIN` and `i64::MAX`, duplicated
/// keys (every occurrence must be unkeyed), identical keyless rows (same
/// fingerprint, counted occurrences), empty identifiers and NULLs. Messages
/// also carry undecodable `attributedBody` and `message_summary_info`
/// bytes, invalid UTF-8, tapbacks, out-of-range booleans and undated rows,
/// and three of them are in Recently Deleted. The three join tables link
/// them, with orphans, a NULL side, a repeated join, `chat_message_join`
/// rowids at the extremes, and an attachment linked only to a Recently
/// Deleted message. Every value is synthetic.
pub fn synthetic_sms(seed: u64, handles: usize, chats: usize, messages: usize) -> Vec<u8> {
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
    let mids = rowids(&mut rng, messages);
    for (i, rowid) in mids.iter().enumerate() {
        // Every 8th row has no GUID and identical content (same
        // fingerprint); every 9th repeats an earlier GUID; every 10th has an
        // empty one.
        let keyless = i % 8 == 7;
        let guid: Option<String> = if keyless {
            None
        } else if i % 9 == 8 {
            Some(format!("SYNTH-MSG-GUID-{:04}", i % 5))
        } else if i % 10 == 3 {
            Some(String::new())
        } else {
            Some(format!("SYNTH-MSG-GUID-{i:04}"))
        };
        let text: Option<Vec<u8>> = if keyless {
            Some(b"SYNTH-SAME".to_vec())
        } else if i % 11 == 4 {
            Some(b"SYNTH-\xff\xfe".to_vec())
        } else if i % 13 == 2 {
            None
        } else {
            Some(format!("SYNTH-BODY-{i} caf\u{e9}").into_bytes())
        };
        let handle = if keyless || hids.is_empty() {
            0
        } else {
            hids[usize::try_from(rng.below(hids.len() as u64)).expect("i")]
        };
        let date: i64 = if keyless || i % 12 == 1 {
            0
        } else if i % 17 == 3 {
            -5
        } else {
            790_000_000_000_000_000 + i64::try_from(i).expect("i") * 60_000_000_000
        };
        let attributed: Option<Vec<u8>> = (i % 6 == 2 && !keyless).then(|| vec![0x04, 0x0b, 0x01]);
        let summary: Option<Vec<u8>> = (i % 7 == 3 && !keyless).then(|| b"not a plist".to_vec());
        let (atype, aguid) = if i % 5 == 4 && !keyless {
            (2001i64, Some(format!("p:0/SYNTH-MSG-GUID-{:04}", i - 1)))
        } else {
            (0, None)
        };
        let is_read: i64 = if i % 14 == 9 {
            7
        } else {
            i64::try_from(i % 2).expect("b")
        };
        c.execute(
            "INSERT INTO message (ROWID, guid, text, attributedBody, message_summary_info,                  handle_id, date, is_from_me, is_read, service, associated_message_type,                  associated_message_guid, item_type, subject)              VALUES (?1, ?2, CAST(?3 AS TEXT), ?4, ?5, ?6, ?7, ?8, ?9, 'iMessage', ?10, ?11, 0, ?12)",
            rusqlite::params![
                rowid,
                guid,
                text,
                attributed,
                summary,
                handle,
                date,
                i64::from(!keyless && i % 3 == 0),
                if keyless { 0 } else { is_read },
                atype,
                aguid,
                (i % 15 == 0 && !keyless).then(|| format!("SYNTH-SUBJECT-{i}")),
            ],
        )
        .expect("message");
    }
    // Never an extreme rowid (the tests check those are imported).
    let plain: Vec<i64> = mids
        .iter()
        .copied()
        .filter(|r| *r != i64::MIN && *r != i64::MAX)
        .collect();
    for (n, at) in SYNTHETIC_RD_POSITIONS.iter().enumerate() {
        if let Some(m) = plain.get(*at) {
            c.execute(
                "INSERT INTO chat_recoverable_message_join (chat_id, message_id, delete_date)                  VALUES (?1, ?2, ?3)",
                rusqlite::params![cids.first().copied().unwrap_or(1), m, i64::try_from(n).expect("n")],
            )
            .expect("recoverable");
        }
    }
    // Attachments: every 7th has no GUID and identical content (same
    // fingerprint), every 9th repeats an earlier GUID.
    let aids = rowids(&mut rng, (messages / 3).max(6));
    for (i, rowid) in aids.iter().enumerate() {
        let keyless = i % 7 == 6;
        let guid: Option<String> = if keyless {
            None
        } else if i % 9 == 8 {
            Some(format!("SYNTH-ATT-GUID-{:04}", i % 3))
        } else {
            Some(format!("SYNTH-ATT-GUID-{i:04}"))
        };
        let filename = if keyless {
            "~/Library/SMS/Attachments/00/00/SYNTH-SAME/SYNTH-FILE.bin".to_owned()
        } else {
            format!(
                "~/Library/SMS/Attachments/{:02x}/{:02}/SYNTH-{i}/SYNTH-FILE-{i}.jpeg",
                i % 256,
                i % 100
            )
        };
        c.execute(
            "INSERT INTO attachment (ROWID, guid, filename, mime_type, transfer_name, total_bytes, \
                 is_sticker, created_date) VALUES (?1, ?2, ?3, 'image/jpeg', ?4, ?5, 0, ?6)",
            rusqlite::params![
                rowid,
                guid,
                filename,
                (!keyless).then(|| format!("SYNTH-FILE-{i}.jpeg")),
                if keyless { 26 } else { 26 + i64::try_from(i).expect("i") },
                if keyless { 0 } else { 790_000_000 + i64::try_from(i).expect("i") }
            ],
        )
        .expect("attachment");
    }
    // chat_handle_join: one or two handles per chat, an orphan on each side
    // and a NULL side.
    let mut chat_handles: Vec<(Option<i64>, Option<i64>)> = Vec::new();
    for (i, chat) in cids.iter().enumerate() {
        if !hids.is_empty() {
            chat_handles.push((Some(*chat), Some(hids[i % hids.len()])));
            if i % 3 == 0 {
                chat_handles.push((Some(*chat), Some(hids[(i * 7 + 1) % hids.len()])));
            }
        }
    }
    if let (Some(chat), Some(handle)) = (cids.first(), hids.first()) {
        chat_handles.extend([
            (Some(*chat), Some(999_999_999)),
            (Some(888_888_888), Some(*handle)),
            (None, Some(*handle)),
        ]);
    }
    for (chat, handle) in &chat_handles {
        c.execute(
            "INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (?1, ?2)",
            rusqlite::params![chat, handle],
        )
        .expect("chat_handle_join");
    }
    // chat_message_join, at shuffled rowids including the extremes: every
    // message in a chat (the Recently Deleted ones too), one in two chats,
    // one join repeated, and an orphan.
    if !cids.is_empty() {
        let mut joins: Vec<(i64, i64, i64)> = mids
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let chat = cids[usize::try_from(rng.below(cids.len() as u64)).expect("i")];
                (
                    chat,
                    *m,
                    790_000_000_000_000_000 + i64::try_from(i).expect("i"),
                )
            })
            .collect();
        if let Some(first) = joins.first().copied() {
            joins.push((cids[cids.len() - 1], first.1, first.2));
            joins.push(first);
        }
        joins.push((cids[0], 777_777_777, 0));
        let jids = rowids(&mut rng, joins.len());
        for ((chat, message, date), rowid) in joins.iter().zip(&jids) {
            c.execute(
                "INSERT INTO chat_message_join (ROWID, chat_id, message_id, message_date) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![rowid, chat, message, date],
            )
            .expect("chat_message_join");
        }
    }
    // message_attachment_join: each attachment on a message; the second
    // only on a Recently Deleted message (excluded with it), the third on
    // that message and a live one, and an orphan.
    if plain.len() > SYNTHETIC_RD_POSITIONS[0] + 2 {
        let rd_message = plain[SYNTHETIC_RD_POSITIONS[0]];
        let live = |j: usize| {
            let mut k = (j * 3 + 1) % plain.len();
            if SYNTHETIC_RD_POSITIONS.contains(&k) {
                k = (k + 1) % plain.len();
            }
            plain[k]
        };
        let mut links: Vec<(i64, i64)> = Vec::new();
        for (j, a) in aids.iter().enumerate() {
            match j {
                1 => links.push((rd_message, *a)),
                2 => links.extend([(rd_message, *a), (live(j), *a)]),
                _ => links.push((live(j), *a)),
            }
        }
        links.push((live(0), 666_666_666));
        for (message, attachment) in &links {
            c.execute(
                "INSERT INTO message_attachment_join (message_id, attachment_id) VALUES (?1, ?2)",
                rusqlite::params![message, attachment],
            )
            .expect("message_attachment_join");
        }
    }
    drop(c);
    fs::read(&path).expect("read")
}
