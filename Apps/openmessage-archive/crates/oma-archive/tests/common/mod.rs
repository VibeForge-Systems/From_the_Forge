//! Shared helpers for the oma-archive integration tests.
//!
//! Tests build hostile archives with plain `rusqlite` connections and plain
//! file operations, which the crate itself may not use; each test file
//! allows `disallowed_methods` for that.
#![allow(dead_code, clippy::disallowed_methods)]

pub mod bulk;
pub mod import;
pub mod viewer;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use oma_archive::space::SpaceProbe;
use oma_archive::{ArchiveId, Library};
use sha2::{Digest, Sha256};

/// Free space the default test probe reports: 1 TiB of a 2 TiB volume,
/// so no test depends on the runner's free disk (spec §12).
pub const TEST_AVAILABLE: u64 = 1 << 40;

/// A fresh library in a private temporary directory, with a fake space
/// probe that reports [`TEST_AVAILABLE`].
pub fn library() -> (tempfile::TempDir, Library) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let lib = Library::new(tmp.path())
        .expect("library")
        .with_space_probe(fake_space(Some(TEST_AVAILABLE), Some(2 * TEST_AVAILABLE)));
    (tmp, lib)
}

/// The canonical library root of `tmp`.
pub fn root(tmp: &tempfile::TempDir) -> PathBuf {
    dunce::canonicalize(tmp.path()).expect("canonical")
}

/// `<library>/a<32hex>`.
pub fn archive_dir(tmp: &tempfile::TempDir, id: &ArchiveId) -> PathBuf {
    root(tmp).join(id.as_str())
}

pub fn db_path(tmp: &tempfile::TempDir, id: &ArchiveId) -> PathBuf {
    archive_dir(tmp, id).join("archive.sqlite")
}

/// A plain read-write connection for building hostile archives. Not
/// hardened, so it can do what the crate refuses to.
pub fn raw(path: &Path) -> rusqlite::Connection {
    let c = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .expect("raw open");
    c.pragma_update(None, "foreign_keys", false)
        .expect("fk off");
    c
}

/// SHA-256 of a file, or `None` if it is absent.
pub fn file_hash(path: &Path) -> Option<String> {
    match fs::read(path) {
        Ok(bytes) => Some(hex(&Sha256::digest(&bytes))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => panic!("read: {e}"),
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Every file under `dir` (relative path → hash), and every directory
/// (relative path → "dir").
pub fn tree(dir: &Path) -> BTreeMap<String, String> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let rel = p
                .strip_prefix(base)
                .expect("prefix")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("|");
            let meta = fs::symlink_metadata(&p).expect("meta");
            if meta.is_dir() {
                out.insert(rel, "dir".into());
                walk(base, &p, out);
            } else {
                out.insert(rel, file_hash(&p).unwrap_or_default());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// The snapshot "open writes nothing" compares: the database and its
/// `-wal` (the `-shm` is SQLite's index, which a reader may create), and
/// the `attachments/` and `imports/` trees.
pub fn data_snapshot(tmp: &tempfile::TempDir, id: &ArchiveId) -> Vec<(String, Option<String>)> {
    let dir = archive_dir(tmp, id);
    let mut v = vec![
        ("db".to_owned(), file_hash(&dir.join("archive.sqlite"))),
        ("wal".to_owned(), wal_hash(&dir)),
    ];
    for (k, h) in tree(&dir.join("attachments")) {
        v.push((format!("attachments|{k}"), Some(h)));
    }
    for (k, h) in tree(&dir.join("imports")) {
        v.push((format!("imports|{k}"), Some(h)));
    }
    v
}

/// A `-wal` that is absent and one that is empty both mean "no WAL frames".
fn wal_hash(dir: &Path) -> Option<String> {
    let p = dir.join("archive.sqlite-wal");
    match fs::metadata(&p) {
        Ok(m) if m.len() == 0 => None,
        _ => file_hash(&p),
    }
}

/// A free-space probe with fixed answers.
pub struct FakeSpace {
    pub available: Option<u64>,
    pub total: Option<u64>,
}

impl SpaceProbe for FakeSpace {
    fn available(&self, _path: &Path) -> io::Result<u64> {
        self.available.ok_or_else(|| io::Error::other("unknown"))
    }
    fn total(&self, _path: &Path) -> io::Result<u64> {
        self.total.ok_or_else(|| io::Error::other("unknown"))
    }
}

pub fn fake_space(available: Option<u64>, total: Option<u64>) -> Arc<dyn SpaceProbe> {
    Arc::new(FakeSpace { available, total })
}

/// Insert one import session (and the rows it references) through a raw
/// connection. Returns the session id.
pub fn insert_session(c: &rusqlite::Connection, state: i64) -> i64 {
    c.execute(
        "INSERT OR IGNORE INTO source_backup (id, observed_key, encrypted) VALUES (1, zeroblob(32), 0)",
        [],
    )
    .expect("source_backup");
    c.execute(
        "INSERT OR IGNORE INTO lineage (id, created_at_ns) VALUES (1, 0)",
        [],
    )
    .expect("lineage");
    let ended: Option<i64> = if state == 0 { None } else { Some(2) };
    c.execute(
        "INSERT INTO import_session (source_backup_id, lineage_id, state, started_at_ns, \
            ended_at_ns, app_version, schema_version, deriver_version, decoder_version, \
            read_profile, content_digest, main_len, wal_outcome, order_class, rank, \
            qualifiers, ios_version_untested, include_recently_deleted, copy_attachments, \
            capabilities) \
         VALUES (1, 1, ?1, 1, ?2, '0.1.0', 1, 1, 1, zeroblob(32), zeroblob(32), 0, 0, 0, \
            zeroblob(41), 0, 0, 0, 1, '{}')",
        rusqlite::params![state, ended],
    )
    .expect("import_session");
    c.last_insert_rowid()
}

/// `attachments/<hh>/` of an archive directory, created as the store
/// creates it (0700 on Unix).
pub fn store_hh(archive: &Path, hh: &str) -> PathBuf {
    let p = archive.join("attachments").join(hh);
    oma_fs::create_dir_durable(&p).expect("store dir");
    p
}

#[cfg(unix)]
pub fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}

#[cfg(unix)]
pub fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::symlink_metadata(path)
        .expect("meta")
        .permissions()
        .mode()
        & 0o777
}
