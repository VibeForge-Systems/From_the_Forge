//! "Save a copy" of one attachment (M6 spec §6.6, slice 6k2), on the
//! export catalogue's file cases (`export-files-plain`):
//!
//! - the copy is byte-identical to the store file, under the name given,
//!   with nothing else left in the folder;
//! - the offered name follows the bundle rule without the ordinal
//!   (`.blocked` unless the extension is on the passive allowlist for the
//!   sniffed kind), and a name the user types is risky unless allowed or
//!   `.blocked`;
//! - an existing file is never replaced without the app's confirmation,
//!   and is replaced with it;
//! - a corrupted store file refuses (`ChecksumMismatch`) and leaves
//!   nothing; so does a cancel; a kind other than the one the name was
//!   checked against refuses before anything is written;
//! - a volume limit refuses a larger file before it is read.
#![allow(clippy::disallowed_methods)]

mod common;
mod files;

use std::path::Path;

use common::*;
use oma_archive::{AttachmentId, CancelToken, CopyPreview, SniffedKind};
use oma_export::attach::VolumeRule;
use oma_export::copy::{save_copy, CopyRequest, CopyStep};
use oma_export::naming::{copy_name, risky_copy_name};
use oma_export::stage::Replace;
use oma_export::{ExportError, TargetName};

fn files_doc() -> Doc {
    docs()
        .into_iter()
        .find(|d| d.name == "export-files-plain")
        .expect("export-files-plain")
}

/// Every attachment with a stored file: (id, lowercase hex digest).
fn stored(i: &Imported) -> Vec<(i64, String)> {
    let c = rusqlite::Connection::open(i.db()).expect("raw open");
    let mut st = c
        .prepare(
            "SELECT a.id, lower(hex(b.sha256)) FROM attachment a \
             JOIN blob b ON b.id = a.current_blob_id ORDER BY a.id",
        )
        .expect("prepare");
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect()
}

fn preview(i: &Imported, id: i64) -> CopyPreview {
    i.lib
        .open_read(&i.id)
        .expect("reader")
        .attachment_copy_preview(AttachmentId::new(id), &CancelToken::new())
        .expect("copy preview")
}

fn copy(
    i: &Imported,
    p: &CopyPreview,
    parent: &Path,
    name: &str,
    replace: Replace,
    cancel: &CancelToken,
) -> Result<oma_export::copy::CopyReport, oma_export::pipeline::ExportFailure> {
    let (blob, len) = p.copy.blob.expect("a stored file");
    let target = TargetName::new(name).expect("name");
    let r = i.lib.open_read(&i.id).expect("reader");
    let hold = i.lib.hold_for_export(&i.id).expect("hold");
    r.with_export_snapshot(&hold, &CancelToken::new(), |snap| {
        let req = CopyRequest {
            blob,
            expected_bytes: len,
            sniffed: p.sniffed,
            parent,
            target: &target,
            replace,
            volume: VolumeRule::None,
        };
        let mut steps = Vec::new();
        let out = save_copy(snap, &req, cancel, &mut |s| steps.push(s.step));
        if out.is_ok() {
            assert_eq!(steps.first(), Some(&CopyStep::Copying));
            assert_eq!(steps.last(), Some(&CopyStep::Finishing));
        }
        Ok::<_, oma_export::pipeline::ExportFailure>(out)
    })
    .expect("snapshot")
}

fn listing(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .expect("list")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn a_copy_is_the_stored_file_under_the_chosen_name() {
    let i = import(&files_doc());
    let all = stored(&i);
    assert!(all.len() > 3);
    let (id, sha) = &all[0];
    let p = preview(&i, *id);
    assert!(p.sniffed.is_some(), "a stored file sniffs");
    assert!(!p.copy.of_unsent_part);
    let offered = copy_name(
        p.copy.name.text().map(|t| t.text.expose().as_str()),
        p.sniffed,
    );
    let d = files::dest();
    let rep = copy(
        &i,
        &p,
        &d.parent,
        &offered.name,
        Replace::Never,
        &CancelToken::new(),
    )
    .expect("copy");
    assert!(!rep.published.replaced);
    let saved = d.parent.join(&offered.name);
    assert_eq!(
        std::fs::read(&saved).expect("saved"),
        std::fs::read(store_file(&i, sha)).expect("store")
    );
    assert_eq!(rep.bytes, std::fs::metadata(&saved).expect("meta").len());
    assert_eq!(listing(&d.parent), vec![offered.name.clone()]);
    #[cfg(not(windows))]
    assert_eq!(rep.mark_of_the_web, None);
    #[cfg(windows)]
    assert!(rep.mark_of_the_web.is_some());
    // Saved again under the same name: refused without the app's
    // confirmation, replaced with it.
    let again = copy(
        &i,
        &p,
        &d.parent,
        &offered.name,
        Replace::Never,
        &CancelToken::new(),
    )
    .expect_err("exists");
    assert!(
        matches!(again.error, ExportError::TargetExists),
        "{:?}",
        again.error
    );
    assert_eq!(listing(&d.parent), vec![offered.name.clone()]);
    std::fs::write(&saved, b"old").expect("overwrite");
    let rep = copy(
        &i,
        &p,
        &d.parent,
        &offered.name,
        Replace::ConfirmedRegularFile,
        &CancelToken::new(),
    )
    .expect("replace");
    assert!(rep.published.replaced);
    assert_eq!(
        std::fs::read(&saved).expect("saved"),
        std::fs::read(store_file(&i, sha)).expect("store")
    );
    assert_eq!(listing(&d.parent), vec![offered.name]);
}

#[test]
fn a_mismatch_a_cancel_or_another_kind_leaves_nothing() {
    let i = import(&files_doc());
    let all = stored(&i);
    let (id, sha) = &all[1];
    let p = preview(&i, *id);
    let d = files::dest();
    // Cancelled before the first chunk.
    let cancel = CancelToken::new();
    cancel.cancel();
    let e = copy(&i, &p, &d.parent, "a copy", Replace::Never, &cancel).expect_err("cancel");
    assert!(matches!(e.error, ExportError::Cancelled), "{:?}", e.error);
    assert!(listing(&d.parent).is_empty());
    // The kind the name was checked against differs.
    let mut other = p.clone();
    other.sniffed = Some(if p.sniffed == Some(SniffedKind::Png) {
        SniffedKind::Jpeg
    } else {
        SniffedKind::Png
    });
    let e = copy(
        &i,
        &other,
        &d.parent,
        "a copy",
        Replace::Never,
        &CancelToken::new(),
    )
    .expect_err("stale");
    assert!(
        matches!(e.error, ExportError::ConfirmationStale),
        "{:?}",
        e.error
    );
    assert!(listing(&d.parent).is_empty());
    // A corrupted store file (same length, other digest).
    corrupt(&store_file(&i, sha));
    let e = copy(
        &i,
        &p,
        &d.parent,
        "a copy",
        Replace::Never,
        &CancelToken::new(),
    )
    .expect_err("mismatch");
    assert!(
        matches!(e.error, ExportError::ChecksumMismatch),
        "{:?}",
        e.error
    );
    // A small file fails in its first 4 KiB, before anything is created.
    assert_ne!(e.staging_removed, Some(false));
    assert!(listing(&d.parent).is_empty(), "{:?}", listing(&d.parent));
    // A file over 4 KiB fails at its last read, after the partial file
    // was written: it is removed. (The test grows a store file to 10,000
    // bytes and records that length, so its digest no longer matches.)
    let (big, big_sha) = all[2].clone();
    let path = store_file(&i, &big_sha);
    corrupt(&path);
    std::fs::write(&path, vec![0x5au8; 10_000]).expect("grow");
    rusqlite::Connection::open(i.db())
        .expect("raw open")
        .execute(
            "UPDATE blob SET byte_len = 10000 WHERE lower(hex(sha256)) = ?1",
            [&big_sha],
        )
        .expect("length");
    let p = preview(&i, big);
    let e = copy(
        &i,
        &p,
        &d.parent,
        "a copy",
        Replace::Never,
        &CancelToken::new(),
    )
    .expect_err("mismatch");
    assert!(
        matches!(e.error, ExportError::ChecksumMismatch),
        "{:?}",
        e.error
    );
    assert_eq!(e.staging_removed, Some(true));
    assert!(listing(&d.parent).is_empty(), "{:?}", listing(&d.parent));
}

#[test]
fn a_volume_limit_refuses_before_reading() {
    let i = import(&files_doc());
    let (id, _) = stored(&i)[0].clone();
    let p = preview(&i, id);
    let (blob, len) = p.copy.blob.expect("file");
    let d = files::dest();
    let target = TargetName::new("big").expect("name");
    let r = i.lib.open_read(&i.id).expect("reader");
    let hold = i.lib.hold_for_export(&i.id).expect("hold");
    let e = r
        .with_export_snapshot(&hold, &CancelToken::new(), |snap| {
            let req = CopyRequest {
                blob,
                expected_bytes: len,
                sniffed: p.sniffed,
                parent: &d.parent,
                target: &target,
                replace: Replace::Never,
                volume: VolumeRule::MaxFileBytes(len - 1),
            };
            Ok::<_, oma_export::pipeline::ExportFailure>(
                save_copy(snap, &req, &CancelToken::new(), &mut |_| {}).expect_err("too large"),
            )
        })
        .expect("snapshot");
    assert!(
        matches!(
            e.error,
            ExportError::Write {
                kind: std::io::ErrorKind::FileTooLarge
            }
        ),
        "{:?}",
        e.error
    );
    assert_eq!(e.staging_removed, None);
    assert!(listing(&d.parent).is_empty());
}

#[test]
fn offered_names_are_defused_and_typed_names_checked() {
    let png = Some(SniffedKind::Png);
    let n = copy_name(Some("photo.PNG"), png);
    assert_eq!((n.name.as_str(), n.blocked), ("photo.PNG", false));
    let n = copy_name(Some("invoice.pdf.exe"), png);
    assert_eq!(
        (n.name.as_str(), n.blocked),
        ("invoice.pdf.exe.blocked", true)
    );
    let n = copy_name(Some("photo.jpg"), png);
    assert!(
        n.blocked,
        "a declared extension that disagrees with the bytes"
    );
    let n = copy_name(None, png);
    assert_eq!((n.name.as_str(), n.blocked), ("attachment.png", false));
    let n = copy_name(Some("\u{202e}gpj.exe"), Some(SniffedKind::Jpeg));
    assert!(n.blocked && !n.name.contains('\u{202e}'), "{:?}", n.name);
    let long = format!("{}.png", "x".repeat(400));
    let n = copy_name(Some(&long), png);
    assert!(
        n.name.len() <= 120 && n.name.ends_with(".png"),
        "{}",
        n.name.len()
    );
    // What the user types.
    assert!(!risky_copy_name("mine.png", png));
    assert!(!risky_copy_name("mine.exe.BLOCKED", png));
    assert!(risky_copy_name("mine.exe", png));
    assert!(risky_copy_name("mine", png));
    assert!(risky_copy_name("mine.png", None));
}
