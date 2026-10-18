//! Shared helpers for the integration tests. Every test copies its fixture
//! into a temporary directory first; on Unix the copy is made read-only
//! unless the test changes it, so any write attempt fails loudly.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use oma_backup::{
    inspect, CancelToken, Candidate, CleanupReport, Inspection, JobCtx, Limits, OpenBackup,
    OpenError, Password, Progress, ProgressSink, StagingRoot, ValidationReport,
};
use zeroize::Zeroizing;

pub const UDID: &str = "SYNTHETIC0-0000000000000000";
pub const PASSWORD: &str = "synthetic-test-password";
pub const UNICODE_PASSWORD: &str = "synthetic-pässwörd-ключ-\u{1F511}";
pub const DEVICE_NAME: &str = "Synthetic Test Phone";
pub const SNAPSHOT_SENTINEL: &str = "synthetic-uploading";
pub const ATTACHMENT_DOMAIN: &str = "MediaDomain";
pub const ATTACHMENT_PATH: &str = "Library/SMS/Attachments/00/00/SYNTH-0000/note.txt";

/// Every fixture directory name.
pub const ALL_FIXTURES: &[&str] = &[
    "plain",
    "plain-xml",
    "plain-wal",
    "plain-no-keybag",
    "enc",
    "enc-xml",
    "enc-wal",
    "enc-unicode-pw",
    "enc-slow",
    "enc-type2",
    "enc-clas-unused-0x10",
    "enc-wpky39-unused",
    "enc-dup-clas",
    "enc-key-uid-data",
    "enc-key-inline",
    "incomplete",
    "incomplete-no-status",
    "snapshot-dir",
    "legacy-mbdb",
    "enc-no-manifestkey",
    "enc-no-keybag",
    "wrong-type-isencrypted",
    "encrypted-plaintext-db",
    "bomb-info",
    "bomb-status",
    "bomb-manifest",
    "bomb-mbfile",
    "hostile-paths",
    "hostile-fileid",
    "hostile-dpic",
    "hostile-iter",
    "hostile-keybag-truncated",
    "hostile-wrap-device-only",
    "hostile-padding-manifest",
    "hostile-padding-sms",
    "hostile-size-max",
    "hostile-mbfile-oversize",
    "missing-sms-blob",
];

/// `tests/fixtures/backups` at the app level.
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/backups"
    ))
}

/// The password a fixture was made with.
pub fn password_for(name: &str) -> &'static str {
    if name == "enc-unicode-pw" {
        UNICODE_PASSWORD
    } else {
        PASSWORD
    }
}

pub fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("mkdir");
    for entry in fs::read_dir(src).expect("read_dir") {
        let entry = entry.expect("entry");
        let to = dst.join(entry.file_name());
        let ty = entry.file_type().expect("type");
        if ty.is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), &to).expect("copy");
        }
    }
}

/// A fixture copied into a temporary directory, with an empty archive
/// library beside it.
pub struct Fixture {
    pub name: String,
    /// The folder holding the backup (like `MobileSync/Backup`).
    pub parent: PathBuf,
    /// The backup folder itself.
    pub dir: PathBuf,
    /// An empty archive library.
    pub library: PathBuf,
    read_only: bool,
    pub tmp: tempfile::TempDir,
}

impl Fixture {
    pub fn new(name: &str) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let parent = tmp.path().join("backups").join(name);
        copy_tree(&fixtures_dir().join(name), &parent);
        let library = tmp.path().join("library");
        fs::create_dir(&library).expect("mkdir");
        Fixture {
            name: name.to_owned(),
            dir: parent.join(UDID),
            parent,
            library,
            read_only: false,
            tmp,
        }
    }

    /// Copy the fixture and make the copy read-only (Unix).
    pub fn read_only(name: &str) -> Self {
        let mut f = Fixture::new(name);
        f.set_read_only(true);
        f
    }

    pub fn set_read_only(&mut self, on: bool) {
        #[cfg(unix)]
        {
            set_tree_writable(&self.parent, !on);
            self.read_only = on;
        }
        #[cfg(not(unix))]
        {
            let _ = on;
        }
    }

    pub fn candidate(&self) -> Candidate {
        Candidate::for_dir(&self.dir).expect("candidate")
    }

    pub fn inspect_with(&self, limits: &Limits) -> Inspection {
        let mut n = 0;
        inspect(&self.candidate(), limits, &mut n).expect("inspect")
    }

    pub fn inspect(&self) -> Inspection {
        self.inspect_with(&Limits::default())
    }

    pub fn open_with(
        &self,
        password: Option<&str>,
        limits: &Limits,
        sink: &mut dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<OpenBackup, OpenError> {
        let inspection = self.inspect_with(limits);
        let root = StagingRoot::new(&self.library, &inspection.root).expect("staging root");
        let session = root.begin().expect("begin");
        let pw = password
            .map(|p| Password::new(Zeroizing::new(p.to_owned()), limits).expect("password"));
        let mut ctx = JobCtx::new(cancel, sink);
        OpenBackup::open(inspection, session, pw, limits, &mut ctx)
    }

    pub fn open(&self, password: Option<&str>) -> Result<OpenBackup, OpenError> {
        let mut sink = |_: Progress| {};
        self.open_with(password, &Limits::default(), &mut sink, &CancelToken::new())
    }

    /// open + validate + close.
    pub fn validate(&self, password: Option<&str>) -> (ValidationReport, CleanupReport) {
        self.validate_with(password, &Limits::default())
    }

    pub fn validate_with(
        &self,
        password: Option<&str>,
        limits: &Limits,
    ) -> (ValidationReport, CleanupReport) {
        let mut sink = |_: Progress| {};
        let token = CancelToken::new();
        let ob = self
            .open_with(password, limits, &mut sink, &token)
            .unwrap_or_else(|e| panic!("{}: open failed: {:?}", self.name, e.error));
        let mut sink = |_: Progress| {};
        let mut ctx = JobCtx::new(&token, &mut sink);
        let report = ob.validate(&mut ctx);
        let cleanup = ob.close();
        let report = report.unwrap_or_else(|e| panic!("{}: validate failed: {e:?}", self.name));
        (report, cleanup)
    }

    /// Every file under the library's staging directory.
    pub fn staged_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        walk(&self.library.join(".oma-staging"), &mut out);
        out
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.read_only {
            self.set_read_only(false);
        }
    }
}

#[cfg(unix)]
fn set_tree_writable(path: &Path, writable: bool) {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::symlink_metadata(path).expect("meta");
    if meta.file_type().is_symlink() {
        return;
    }
    if meta.is_dir() {
        if writable {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        for e in fs::read_dir(path).expect("read_dir") {
            set_tree_writable(&e.expect("entry").path(), writable);
        }
        if !writable {
            fs::set_permissions(path, fs::Permissions::from_mode(0o555)).expect("chmod");
        }
    } else {
        let mode = if writable { 0o644 } else { 0o444 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
    }
}

/// Every regular file under `dir` (not following links).
pub fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let Ok(m) = fs::symlink_metadata(&p) else {
            continue;
        };
        if m.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// SHA-256 of every file, plus every entry name, under `dir`.
pub fn snapshot(dir: &Path) -> Vec<(String, String)> {
    use sha2::{Digest, Sha256};
    let mut entries = Vec::new();
    fn rec(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        let mut names: Vec<_> = fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| e.expect("entry").path())
            .collect();
        names.sort();
        for p in names {
            let rel = p
                .strip_prefix(base)
                .expect("prefix")
                .to_string_lossy()
                // `/` on every OS, so callers can filter with one spelling.
                .replace('\\', "/");
            let m = fs::symlink_metadata(&p).expect("meta");
            if m.is_dir() {
                out.push((rel, "dir".into()));
                rec(base, &p, out);
            } else {
                let bytes = match fs::read(&p) {
                    Ok(bytes) => bytes,
                    // A live session's `.lock` is byte-range locked, and on
                    // Windows that makes it unreadable (ERROR_LOCK_VIOLATION).
                    Err(e) if cfg!(windows) && e.raw_os_error() == Some(33) => {
                        out.push((rel, "<locked>".into()));
                        continue;
                    }
                    Err(e) => panic!("read: {e:?}"),
                };
                let digest = Sha256::digest(&bytes);
                let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
                out.push((rel, hex));
            }
        }
    }
    rec(dir, dir, &mut entries);
    entries
}
