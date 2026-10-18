//! `OpenBackup::copy_file_to`, the M4 attachment copy (M4 §3.2, §9.1):
//! plaintext hashes, the size cap, per-file stamps, cancellation and the
//! error side of a failing writer.
mod common;

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use common::*;
use oma_backup::{
    BackupError, CancelToken, CopiedFile, FileUnavailable, IoOp, IoSide, JobCtx, LimitKind, Limits,
    MalformedDetail, OpenBackup, Phase, Progress, SMS_DB_DOMAIN, SMS_DB_PATH,
};
use sha2::{Digest, Sha256};

/// Every fixture with a `.expected/` directory, and its password.
const EXPECTED: &[(&str, Option<&str>)] = &[
    ("plain", None),
    ("plain-xml", None),
    ("enc", Some(PASSWORD)),
    ("enc-xml", Some(PASSWORD)),
    ("enc-key-uid-data", Some(PASSWORD)),
    ("enc-key-inline", Some(PASSWORD)),
    ("enc-unicode-pw", Some(UNICODE_PASSWORD)),
];

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn expected(name: &str, file: &str) -> Vec<u8> {
    fs::read(fixtures_dir().join(format!("{name}.expected")).join(file)).expect("expected file")
}

/// The blob behind a listed file, built from components (never a `/`).
fn blob_of(fx: &Fixture, ob: &OpenBackup, domain: &str, path: &str) -> PathBuf {
    let entry = ob.find_file(domain, path).expect("lookup").expect("listed");
    fx.dir.join(&entry.file_id[..2]).join(&entry.file_id)
}

/// Copy a listed file into a `Vec`, collecting progress.
fn copy(
    ob: &OpenBackup,
    domain: &str,
    path: &str,
    token: &CancelToken,
) -> (Result<CopiedFile, BackupError>, Vec<u8>, Vec<Progress>) {
    let key = ob
        .find_attachment(domain, path)
        .expect("lookup")
        .expect("listed");
    let mut out = Vec::new();
    let mut seen = Vec::new();
    let mut sink = |p: Progress| seen.push(p);
    let mut ctx = JobCtx::new(token, &mut sink);
    let r = ob.copy_file_to(&key, &mut out, &mut ctx);
    (r, out, seen)
}

#[test]
fn copies_stream_the_expected_plaintext_and_its_hash() {
    for &(name, password) in EXPECTED {
        let fx = Fixture::read_only(name);
        let ob = fx.open(password).expect("open");
        let staging = ob.staging_path().expect("staging").to_path_buf();
        let staged_before = snapshot(&staging);

        // The attachment, through an `AttachmentKey`.
        let blob_len = fs::metadata(blob_of(&fx, &ob, ATTACHMENT_DOMAIN, ATTACHMENT_PATH))
            .expect("blob")
            .len();
        let (r, out, seen) = copy(&ob, ATTACHMENT_DOMAIN, ATTACHMENT_PATH, &CancelToken::new());
        let c = r.unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let plain = expected(name, "attachment.txt");
        assert_eq!(out, plain, "{name}");
        assert_eq!(c.sha256, sha256(&plain), "{name}");
        assert_eq!(c.len, 87, "{name}");
        assert_eq!(c.declared_size, Some(87), "{name}");
        assert_eq!(c.size_matches, Some(true), "{name}");
        assert!(!seen.is_empty(), "{name}");
        assert!(seen.iter().all(|p| p.phase == Phase::Copying), "{name}");
        assert!(seen.iter().all(|p| p.total == Some(blob_len)), "{name}");
        assert_eq!(seen.last().map(|p| p.done), Some(blob_len), "{name}");

        // sms.db, through a `ManifestEntry`: a larger file, many blocks.
        let entry = ob
            .find_file(SMS_DB_DOMAIN, SMS_DB_PATH)
            .expect("lookup")
            .expect("listed");
        let token = CancelToken::new();
        let mut sink = |_: Progress| {};
        let mut ctx = JobCtx::new(&token, &mut sink);
        let mut out = Vec::new();
        let c = ob
            .copy_file_to(&entry, &mut out, &mut ctx)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let plain = expected(name, "sms.db");
        assert_eq!(c.sha256, sha256(&plain), "{name}");
        assert_eq!(c.len, plain.len() as u64, "{name}");
        assert_eq!(out, plain, "{name}");

        // Nothing was written to staging, and the backup is unchanged.
        assert_eq!(snapshot(&staging), staged_before, "{name}");
        assert!(ob.unchanged_since_open().expect("stamps"), "{name}");
        assert!(ob.close().removed, "{name}");
    }
}

#[test]
fn a_hostile_declared_size_is_recorded_not_trusted() {
    let fx = Fixture::read_only("hostile-size-max");
    let ob = fx.open(Some(PASSWORD)).expect("open");
    let (r, out, _) = copy(&ob, ATTACHMENT_DOMAIN, ATTACHMENT_PATH, &CancelToken::new());
    let c = r.expect("copy");
    let plain = expected("plain", "attachment.txt");
    assert_eq!(out, plain);
    assert_eq!(c.sha256, sha256(&plain));
    assert_eq!(c.len, 87);
    assert_eq!(c.declared_size, Some(u64::MAX));
    assert_eq!(c.size_matches, Some(false));
    assert!(ob.close().removed);
}

#[test]
fn the_attachment_cap_refuses_before_any_byte_is_read() {
    for (name, password) in [("plain", None), ("enc", Some(PASSWORD))] {
        let fx = Fixture::read_only(name);
        let blob_len = {
            let ob = fx.open(password).expect("open");
            let len = fs::metadata(blob_of(&fx, &ob, ATTACHMENT_DOMAIN, ATTACHMENT_PATH))
                .expect("blob")
                .len();
            assert!(ob.close().removed);
            len
        };
        // The cap applies to the stored blob: for an encrypted backup that
        // is the padded ciphertext, longer than the 87-byte plaintext.
        assert_eq!(blob_len, if password.is_some() { 96 } else { 87 });

        for (cap, allowed) in [(blob_len - 1, false), (blob_len, true)] {
            let limits = Limits {
                attachment_bytes: cap,
                ..Limits::default()
            };
            let mut sink = |_: Progress| {};
            let ob = fx
                .open_with(password, &limits, &mut sink, &CancelToken::new())
                .expect("open");
            let (r, out, seen) = copy(&ob, ATTACHMENT_DOMAIN, ATTACHMENT_PATH, &CancelToken::new());
            if allowed {
                assert_eq!(r.expect("at the cap").len, 87, "{name}");
            } else {
                match r {
                    Err(BackupError::LimitExceeded {
                        what,
                        value,
                        cap: c,
                    }) => {
                        assert_eq!(what, LimitKind::AttachmentBytes);
                        assert_eq!(value, blob_len);
                        assert_eq!(c, cap);
                    }
                    other => panic!("{name}: {other:?}"),
                }
                assert!(out.is_empty(), "{name}: nothing written");
                assert!(seen.is_empty(), "{name}: nothing read");
            }
            assert!(ob.close().removed);
        }
    }
}

/// A writer that changes the source blob on its first write, standing in
/// for a backup tool rewriting the backup during the copy.
struct Tamper {
    blob: PathBuf,
    done: bool,
    out: Vec<u8>,
}

impl Write for Tamper {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.done {
            self.done = true;
            let mut f = fs::OpenOptions::new().append(true).open(&self.blob)?;
            f.write_all(&[0u8; 16])?;
        }
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_blob_changed_during_the_copy_is_source_changed() {
    for (name, password) in [("plain", None), ("enc", Some(PASSWORD))] {
        let fx = Fixture::new(name);
        let ob = fx.open(password).expect("open");
        let blob = blob_of(&fx, &ob, ATTACHMENT_DOMAIN, ATTACHMENT_PATH);
        let key = ob
            .find_attachment(ATTACHMENT_DOMAIN, ATTACHMENT_PATH)
            .expect("lookup")
            .expect("listed");
        let mut tamper = Tamper {
            blob: blob.clone(),
            done: false,
            out: Vec::new(),
        };
        let token = CancelToken::new();
        let mut sink = |_: Progress| {};
        let mut ctx = JobCtx::new(&token, &mut sink);
        assert!(
            matches!(
                ob.copy_file_to(&key, &mut tamper, &mut ctx),
                Err(BackupError::SourceChanged)
            ),
            "{name}"
        );
        assert!(tamper.done, "{name}");
        // Per-file stamps are not added to the open's stamp list, so the
        // backup-wide check stays O(1) and does not see this blob.
        assert!(ob.unchanged_since_open().expect("stamps"), "{name}");

        // Another blob, untouched, still copies.
        let (r, _, _) = copy(&ob, SMS_DB_DOMAIN, SMS_DB_PATH, &CancelToken::new());
        assert!(r.is_ok(), "{name}");
        assert!(ob.close().removed, "{name}");
    }
}

#[test]
fn a_cancelled_copy_writes_nothing() {
    for (name, password) in [("plain", None), ("enc", Some(PASSWORD))] {
        let fx = Fixture::read_only(name);
        let ob = fx.open(password).expect("open");
        let token = CancelToken::new();
        token.cancel();
        let (r, out, _) = copy(&ob, ATTACHMENT_DOMAIN, ATTACHMENT_PATH, &token);
        assert!(matches!(r, Err(BackupError::Cancelled)), "{name}");
        assert!(out.is_empty(), "{name}");
        assert!(ob.close().removed);
    }
}

/// A writer that fails every write, or only its flush.
struct Failing {
    on_flush: bool,
    error: fn() -> io::Error,
}

impl Write for Failing {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.on_flush {
            Ok(buf.len())
        } else {
            Err((self.error)())
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Err((self.error)())
    }
}

#[test]
fn writer_failures_are_reported_on_the_sink_side() {
    fn full() -> io::Error {
        io::Error::from(io::ErrorKind::StorageFull)
    }
    fn too_large() -> io::Error {
        io::Error::from(io::ErrorKind::FileTooLarge)
    }
    for (name, password) in [("plain", None), ("enc", Some(PASSWORD))] {
        let fx = Fixture::read_only(name);
        let ob = fx.open(password).expect("open");
        let key = ob
            .find_attachment(ATTACHMENT_DOMAIN, ATTACHMENT_PATH)
            .expect("lookup")
            .expect("listed");
        for (on_flush, error, kind) in [
            (false, full as fn() -> io::Error, io::ErrorKind::StorageFull),
            (false, too_large, io::ErrorKind::FileTooLarge),
            (true, full, io::ErrorKind::StorageFull),
        ] {
            let token = CancelToken::new();
            let mut sink = |_: Progress| {};
            let mut ctx = JobCtx::new(&token, &mut sink);
            let mut w = Failing { on_flush, error };
            match ob.copy_file_to(&key, &mut w, &mut ctx) {
                Err(BackupError::Io {
                    op, side, kind: k, ..
                }) => {
                    assert_eq!(op, IoOp::Write, "{name}");
                    assert_eq!(side, IoSide::Sink, "{name}");
                    assert_eq!(k, kind, "{name}");
                }
                other => panic!("{name}: {other:?}"),
            }
        }
        assert!(ob.close().removed);
    }
}

#[test]
fn unavailable_and_malformed_sources() {
    // A listed blob that is missing: nothing is written.
    let fx = Fixture::read_only("missing-sms-blob");
    let ob = fx.open(None).expect("open");
    let (r, out, _) = copy(&ob, SMS_DB_DOMAIN, SMS_DB_PATH, &CancelToken::new());
    assert!(matches!(
        r,
        Err(BackupError::FileUnavailable(FileUnavailable::BlobMissing))
    ));
    assert!(out.is_empty());
    // A directory row is not a file.
    let dir = ob
        .find_file("HomeDomain", "Library/SMS")
        .expect("lookup")
        .expect("listed");
    let token = CancelToken::new();
    let mut sink = |_: Progress| {};
    let mut ctx = JobCtx::new(&token, &mut sink);
    assert!(matches!(
        ob.copy_file_to(&dir, &mut Vec::new(), &mut ctx),
        Err(BackupError::FileUnavailable(FileUnavailable::NotAFile))
    ));
    assert!(ob.close().removed);

    // Bad PKCS#7 padding in the last block.
    let fx = Fixture::read_only("hostile-padding-sms");
    let ob = fx.open(Some(PASSWORD)).expect("open");
    let (r, _, _) = copy(&ob, SMS_DB_DOMAIN, SMS_DB_PATH, &CancelToken::new());
    assert!(matches!(
        r,
        Err(BackupError::Malformed {
            detail: MalformedDetail::Padding,
            ..
        })
    ));
    assert!(ob.close().removed);
}

#[cfg(unix)]
#[test]
fn a_blob_replaced_by_a_link_is_not_followed() {
    {
        let fx = Fixture::new("plain");
        let ob = fx.open(None).expect("open");
        let blob = blob_of(&fx, &ob, ATTACHMENT_DOMAIN, ATTACHMENT_PATH);
        let elsewhere = fx.tmp.path().join("elsewhere");
        fs::write(&elsewhere, b"outside the backup").expect("write");
        fs::remove_file(&blob).expect("remove");
        std::os::unix::fs::symlink(&elsewhere, &blob).expect("symlink");
        let (r, out, _) = copy(&ob, ATTACHMENT_DOMAIN, ATTACHMENT_PATH, &CancelToken::new());
        assert!(matches!(
            r,
            Err(BackupError::FileUnavailable(
                FileUnavailable::BlobNotRegular
            ))
        ));
        assert!(out.is_empty());
        assert!(ob.close().removed);
    }
}

#[test]
fn debug_never_shows_the_digest() {
    let fx = Fixture::read_only("plain");
    let ob = fx.open(None).expect("open");
    let (r, _, _) = copy(&ob, ATTACHMENT_DOMAIN, ATTACHMENT_PATH, &CancelToken::new());
    let c = r.expect("copy");
    let hex: String = c.sha256.iter().map(|b| format!("{b:02x}")).collect();
    let shown = format!("{c:?}");
    assert!(!shown.contains(&hex), "{shown}");
    assert!(!shown.contains(&hex[..16]), "{shown}");
    assert!(
        shown.contains("<sha256>") && shown.contains("len: 87"),
        "{shown}"
    );
    assert!(ob.close().removed);
}
