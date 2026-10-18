//! Shared helpers for the oma-messages integration tests.
//!
//! Each test builds its sms.db stand-in with SQLite in a temporary directory
//! and swaps it into a temporary copy of the synthetic M2 `plain` backup.
//! The committed fixtures are never written. Every value is synthetic.
#![allow(dead_code, clippy::disallowed_types, clippy::disallowed_methods)]

pub mod bplist;
pub mod expected;
pub mod mutate;
pub mod ts_writer;

use std::fs;
use std::path::{Path, PathBuf};

use oma_backup::{
    inspect, CancelToken, Candidate, JobCtx, Limits, OpenBackup, Password, Progress, StagingRoot,
    WalPolicy, SMS_DB_FILE_ID,
};
use oma_messages::{
    connect, stage, ColumnKind, Connect, KnownTable, MessagesCtx, MessagesError, MessagesProgress,
    Probe, ProbeOptions,
};
use rusqlite::Connection;
use zeroize::Zeroizing;

pub const UDID: &str = "SYNTHETIC0-0000000000000000";
pub const PASSWORD: &str = "synthetic-test-password";

/// `tests/fixtures/backups` at the app level.
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/backups"
    ))
}

/// `tests/fixtures/messages` at the app level (the M3 catalogue).
pub fn messages_dir() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/messages"
    ))
}

/// The names of the messages catalogue's backups, sorted.
pub fn catalogue() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(messages_dir())
        .expect("messages dir")
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.ends_with(".expected"))
        .collect();
    names.sort();
    names
}

/// The password of an encrypted catalogue entry, `None` for a plain one.
pub fn catalogue_password(name: &str) -> Option<&'static str> {
    name.ends_with("-enc").then_some(PASSWORD)
}

pub fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("mkdir");
    for entry in fs::read_dir(src).expect("read_dir") {
        let entry = entry.expect("entry");
        let to = dst.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), &to).expect("copy");
        }
    }
}

/// Every regular file under `dir`.
pub fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// A copy of an M2 fixture in a temporary directory, with an empty archive
/// library beside it.
pub struct Fixture {
    pub dir: PathBuf,
    pub library: PathBuf,
    pub tmp: tempfile::TempDir,
}

impl Fixture {
    pub fn m2(name: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let parent = tmp.path().join("backups").join(name);
        copy_tree(&fixtures_dir().join(name), &parent);
        let library = tmp.path().join("library");
        fs::create_dir(&library).expect("mkdir");
        Fixture {
            dir: parent.join(UDID),
            library,
            tmp,
        }
    }

    /// A copy of a messages-catalogue backup.
    pub fn messages(name: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let parent = tmp.path().join("backups").join(name);
        copy_tree(&messages_dir().join(name), &parent);
        let library = tmp.path().join("library");
        fs::create_dir(&library).expect("mkdir");
        Fixture {
            dir: parent.join(UDID),
            library,
            tmp,
        }
    }

    /// A copy of `plain` whose sms.db blob is `main`.
    pub fn with_sms(main: &[u8]) -> Self {
        let fx = Fixture::m2("plain");
        let blob = fx.dir.join(&SMS_DB_FILE_ID[..2]).join(SMS_DB_FILE_ID);
        fs::write(blob, main).expect("sms blob");
        fx
    }

    pub fn open(&self, password: Option<&str>) -> OpenBackup {
        let limits = Limits::default();
        let mut n = 0;
        let candidate = Candidate::for_dir(&self.dir).expect("candidate");
        let inspection = inspect(&candidate, &limits, &mut n).expect("inspect");
        let root = StagingRoot::new(&self.library, &inspection.root).expect("staging root");
        let session = root.begin().expect("begin");
        let pw = password
            .map(|p| Password::new(Zeroizing::new(p.to_owned()), &limits).expect("password"));
        let token = CancelToken::new();
        let mut sink = |_: Progress| {};
        let mut ctx = JobCtx::new(&token, &mut sink);
        OpenBackup::open(inspection, session, pw, &limits, &mut ctx)
            .unwrap_or_else(|e| panic!("open failed: {:?}", e.error))
    }

    /// Every file under the library's staging directory.
    pub fn staged_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        walk(&self.library.join(".oma-staging"), &mut out);
        out
    }
}

/// Sentinel strings planted in the synthetic fixtures (M3 §19.3). None may
/// appear in a log line, an error's `Display` or `Debug`, or any public
/// type's `Debug`.
pub const SENTINELS: &[&str] = &[
    "SYNTH-BODY",
    "SYNTH-EDIT",
    "SYNTH-DELETED",
    "synthetic-handle",
    "+1555",
    "1555SYNTH",
    "SYNTH-CHAT",
    "chatSYNTH",
    "SYNTH-GROUP",
    "SYNTH-PERSON",
    "SYNTH-FILE",
    "SYNTH-GENMOJI",
    "SYNTH-MENTION",
    "SYNTH-SUBJECT",
    "SYNTH-MSG-GUID",
    "SYNTH-ATT-GUID",
    "SYNTH-MISSING",
    "SYNTH_SENTINEL",
    "SYNTH-TRANSLATION",
    "SYNTH-LANG",
    "SYNTH-PAYLOAD",
    "SYNTH-PROPERTY",
    "SYNTH-KEY",
    "SYNTH-VALUE",
    "SYNTH-CORRUPT",
    "SYNTH-LONG",
    "SYNTH-SERVICE",
    "SYNTH-TEXT",
    "SYNTH-NOT-A",
    "SYNTHLinkClass",
    "SYNTHUnknownValue",
    "__kSYNTHUnknownKey",
    "at_0_00000000",
    "\u{1F525}",
    "example.invalid",
    "com.example.synthetic",
];

/// The sentinels found in `s`.
pub fn sentinels_in(s: &str) -> Vec<&'static str> {
    SENTINELS
        .iter()
        .copied()
        .filter(|x| s.contains(x))
        .collect()
}

/// Run `check()` on a fixture, close the backup, and return the result and
/// the cleanup report.
pub fn run_check(
    fx: &Fixture,
    password: Option<&str>,
    opts: &oma_messages::CheckOptions,
) -> (
    Result<oma_messages::ExtractionReport, MessagesError>,
    oma_backup::CleanupReport,
) {
    let ob = fx.open(password);
    let token = CancelToken::new();
    let mut sink = |_: MessagesProgress| {};
    let mut ctx = MessagesCtx::new(&token, &mut sink);
    let result = oma_messages::check(&ob, opts, &mut ctx);
    (result, ob.close())
}

/// Build a database file with `build` and return its bytes.
pub fn build_with(build: impl FnOnce(&Connection)) -> Vec<u8> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("sms.db");
    let c = Connection::open(&path).expect("create");
    build(&c);
    drop(c);
    fs::read(&path).expect("read")
}

/// Build a database file from SQL.
pub fn build(sql: &str) -> Vec<u8> {
    build_with(|c| c.execute_batch(sql).expect("schema"))
}

/// The declared SQL type for a known column kind.
fn declared(kind: Option<ColumnKind>) -> &'static str {
    match kind {
        Some(ColumnKind::RowId) => "INTEGER PRIMARY KEY AUTOINCREMENT",
        Some(ColumnKind::Integer | ColumnKind::Bool | ColumnKind::Time) => "INTEGER",
        Some(ColumnKind::Text) => "TEXT",
        _ => "BLOB",
    }
}

/// `CREATE TABLE` for one known table with every known column, or with the
/// required ones only. Written from the crate's own list, so it is a
/// synthetic stand-in, not a copy of any real schema.
pub fn create_table(t: KnownTable, required_only: bool) -> String {
    let cols: Vec<String> = t
        .known_columns()
        .filter(|c| !required_only || c.required())
        .map(|c| format!("\"{}\" {}", c.name(), declared(c.kind())))
        .collect();
    let cols = if cols.is_empty() {
        match t {
            KnownTable::RecoverableMessagePart => {
                "chat_id INTEGER, message_id INTEGER, part_index INTEGER, part_text BLOB".to_owned()
            }
            _ => "id INTEGER, guid TEXT".to_owned(),
        }
    } else {
        cols.join(", ")
    };
    format!("CREATE TABLE \"{}\" ({cols});\n", t.name())
}

/// Every known table with every known column, plus a `_ClientVersion` row
/// and one Recently Deleted part (whose text must never be read).
pub fn full_schema() -> String {
    let mut s = String::new();
    for t in KnownTable::ALL {
        s.push_str(&create_table(t, false));
    }
    s.push_str(
        "INSERT INTO _SqliteDatabaseProperties (key, value) VALUES ('_ClientVersion', '17001');
         INSERT INTO recoverable_message_part VALUES (1, 1, 0, 'SYNTH-DELETED-0');\n",
    );
    s
}

/// The required tables with their required columns only.
pub fn minimal_schema() -> String {
    KnownTable::ALL
        .iter()
        .filter(|t| t.required())
        .map(|t| create_table(*t, true))
        .collect()
}

/// What a probe came to, without the borrow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Supported,
    Unsupported(Vec<oma_messages::UnsupportedReason>),
    Damaged(oma_messages::DamageReason),
    Incomplete(oma_messages::IncompleteReason),
    SchemaExceedsLimits,
}

/// Stage `fx`'s sms.db, connect, probe with `opts`, and hand the result and
/// a context to `f`. Staging is removed when the backup closes.
pub fn with_probe<R>(
    fx: &Fixture,
    password: Option<&str>,
    opts: &ProbeOptions,
    f: impl FnOnce(Result<Probe<'_>, MessagesError>, &mut MessagesCtx<'_>) -> R,
) -> Option<R> {
    let ob = fx.open(password);
    let token = CancelToken::new();
    let mut sink = |_: MessagesProgress| {};
    let mut ctx = MessagesCtx::new(&token, &mut sink);
    let staged = stage(&ob, WalPolicy::BestEffort, &mut ctx).expect("stage");
    let out = match connect(&staged, &ctx).expect("connect") {
        Connect::Ready(conn) => {
            let probe = oma_messages::SmsDb::open_with(&conn, opts, &mut ctx);
            Some(f(probe, &mut ctx))
        }
        Connect::SchemaExceedsLimits => None,
    };
    staged.remove().expect("remove");
    let cleanup = ob.close();
    assert!(cleanup.removed);
    out
}

/// The probe's kind and capabilities for a database.
pub fn probe_db(main: &[u8]) -> (Kind, Option<oma_messages::Capabilities>) {
    probe_db_with(main, &ProbeOptions::default())
}

pub fn probe_db_with(
    main: &[u8],
    opts: &ProbeOptions,
) -> (Kind, Option<oma_messages::Capabilities>) {
    let fx = Fixture::with_sms(main);
    with_probe(&fx, None, opts, |p, _| match p.expect("probe") {
        Probe::Supported(db) => (Kind::Supported, Some(db.capabilities().clone())),
        Probe::Unsupported(u) => (Kind::Unsupported(u.reasons), Some(u.capabilities)),
        Probe::Damaged(d) => (Kind::Damaged(d), None),
        Probe::Incomplete(i) => (Kind::Incomplete(i), None),
    })
    .unwrap_or((Kind::SchemaExceedsLimits, None))
}
