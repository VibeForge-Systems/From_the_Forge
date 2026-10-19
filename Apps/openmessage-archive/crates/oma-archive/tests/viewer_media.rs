//! Media in the crate (M5 spec §5.2–§5.5, §13.1, §13.5, slice 5d).
//!
//! - **The viewer catalogue:** for every entry and scenario with stored
//!   files, every attachment's sniffed kind and what the viewer offers for
//!   it (a rendition, the Play card, a file card or "Unrecognised content")
//!   match the Python model's `sniff` and `card`, from the bytes and never
//!   the stored type; for every playable file, `verify_blob` then
//!   `check_container` give the model's `structure` verdict and reason.
//!   Every image is read with `read_blob_verified` and rendered: valid
//!   images become renditions without metadata, the hostile PNGs are
//!   refused before decoding. Reading writes nothing.
//! - **Blob readers** on a copy: `open_range` refuses another blob's
//!   identity, a changed length, mtime and (Unix) inode, a link and a
//!   directory; `read_at` past the end is refused; a digest mismatch fails
//!   `verify_blob` and `read_blob_verified` (and nothing is returned) but
//!   not the sniff window; cancel, the size limit and unknown ids.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;

use common::import::read_only;
use common::viewer::{import_order, model_docs, names, viewer_root, Imported};
use common::*;
use oma_archive::media::render::{render, RenderLimits, Rendition, MAX_INPUT_BYTES};
use oma_archive::{
    ArchiveError, ArchiveReader, AttachmentId, BlobId, BlobProblem, CancelToken, ContainerRefusal,
    ContainerVerdict, MediaClass, RenderError, RenderedFormat, SniffedKind,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The model's name of a sniffed kind.
fn kind_name(k: SniffedKind) -> &'static str {
    use SniffedKind as K;
    match k {
        K::Gif => "gif",
        K::Webp => "webp",
        K::Png => "png",
        K::Jpeg => "jpeg",
        K::Bmp => "bmp",
        K::Ico => "ico",
        K::QuickTime => "quicktime",
        K::M4a => "m4a",
        K::M4v => "m4v",
        K::Heif => "heif",
        K::Mp4 => "mp4",
        K::IsoBmffOther => "isobmff-other",
        K::Webm => "webm",
        K::Wav => "wav",
        K::Aiff => "aiff",
        K::Ogg => "ogg",
        K::Mp3 => "mp3",
        K::Caf => "caf",
        K::Pdf => "pdf",
        K::Xml => "xml",
        K::Html => "html",
        K::Vcard => "vcard",
        K::Text => "text",
        K::Unknown => "unknown",
        _ => "?",
    }
}

/// The model's card of a present file.
fn card_name(k: SniffedKind) -> String {
    match k.class() {
        MediaClass::Image => "image".to_owned(),
        MediaClass::Playable { .. } => "play".to_owned(),
        MediaClass::Unrecognised => "unrecognised".to_owned(),
        _ => format!("file:{}", kind_name(k)),
    }
}

fn refusal_name(r: ContainerRefusal) -> &'static str {
    use ContainerRefusal as R;
    match r {
        R::TooDeep => "tooDeep",
        R::TooManyBoxes => "tooManyBoxes",
        R::BoxPastEnd => "boxPastEnd",
        R::SizeOverflow => "sizeOverflow",
        R::SizeZeroNested => "sizeZeroNested",
        R::BoxTooSmall => "boxTooSmall",
        R::ReferenceMovie => "referenceMovie",
        R::ExternalDataReference => "externalDataReference",
        R::NoMoov => "noMoov",
        R::ChunkPastEnd => "chunkPastEnd",
        R::DataBeforeFmt => "dataBeforeFmt",
        R::NoFmt => "noFmt",
        _ => "?",
    }
}

fn verdict_json(v: ContainerVerdict) -> Value {
    match v {
        ContainerVerdict::Ok => serde_json::json!({"state": "ok"}),
        ContainerVerdict::NotChecked => serde_json::json!({"state": "notChecked"}),
        ContainerVerdict::Refused(r) => {
            serde_json::json!({"state": "refused", "reason": refusal_name(r)})
        }
        _ => serde_json::json!({"state": "?"}),
    }
}

/// Every media fixture by the SHA-256 of its bytes.
fn media_by_digest() -> HashMap<Vec<u8>, String> {
    fs::read_dir(viewer_root().join("media"))
        .expect("media")
        .flatten()
        .map(|e| {
            let bytes = fs::read(e.path()).expect("read");
            (
                Sha256::digest(&bytes).to_vec(),
                e.file_name().to_string_lossy().into_owned(),
            )
        })
        .collect()
}

/// What rendering each image fixture must give: `Ok(format, width,
/// height, animated)` or the refusal.
fn expected_render(file: &str) -> Option<Result<(RenderedFormat, u32, u32, bool), RenderError>> {
    use RenderedFormat::{Jpeg, Png};
    Some(match file {
        "png-rgb-6x4.png" => Ok((Jpeg, 6, 4, false)),
        "png-alpha-4x4.png" => Ok((Png, 4, 4, false)),
        // Orientation 6 rotates 8×6 to 6×8.
        "jpeg-exif6-gps-8x6.jpg" => Ok((Jpeg, 6, 8, false)),
        "gif-2frames-4x4.gif" => Ok((Png, 4, 4, true)),
        "gif-html-polyglot.gif" => Ok((Png, 2, 2, false)),
        "webp-lossless-4x4.webp" => Ok((Png, 4, 4, false)),
        "png-bomb-12000.png" | "png-rgba16-4097.png" => Err(RenderError::DecodedTooLarge),
        "png-ihdr-60000.png" => Err(RenderError::DimensionsTooLarge),
        // Decoding is not integrity: either answer is allowed, but never a
        // panic.
        "jpeg-truncated.jpg" => return None,
        // A HEIF header without an image (only with the `heic` feature).
        "heic-header.heic" => Err(RenderError::Malformed),
        other => panic!("{other}: not an image fixture"),
    })
}

fn reverse(map: &HashMap<i64, String>) -> HashMap<String, i64> {
    map.iter().map(|(k, v)| (v.clone(), *k)).collect()
}

#[derive(Default)]
struct Seen {
    kinds: BTreeSet<String>,
    verdicts: BTreeSet<String>,
    renders: BTreeMap<String, String>,
}

fn check_model(what: &str, model: &Value, seen: &mut Seen) {
    let order = &model["orders"][0];
    let i = import_order(order, what);
    let n = names(&i, order);
    let before = data_snapshot(&i.tmp, &i.id);
    let r = i.lib.open_read(&i.id).expect("reader");
    let atts = reverse(&n.attachments);
    let digests = media_by_digest();
    let cancel = CancelToken::new();
    for (name, w) in model["viewer"]["attachments"]
        .as_object()
        .expect("attachments")
    {
        let at = format!("{what}: {name}");
        let facts = r
            .attachment(AttachmentId::new(atts[name.as_str()]))
            .expect("facts");
        let Some(want_kind) = w["sniff"].as_str() else {
            assert!(
                facts.blob.is_none(),
                "{at}: a file the model does not sniff"
            );
            continue;
        };
        let blob = facts.blob.expect("a stored file").id;
        let s = r.inspect_blob(blob).expect("sniff");
        assert_eq!(kind_name(s.kind), want_kind, "{at}: sniffed kind");
        // The model is the default build's: HEIF is a file card unless the
        // `heic` feature is on (owner decision Q2).
        let want_card = match w["card"].as_str().expect("card") {
            "file:heif" if cfg!(feature = "heic") => "image",
            c => c,
        };
        assert_eq!(card_name(s.kind), want_card, "{at}: card");
        assert_eq!(
            Some(s.stored_bytes as i64),
            w["storedBytes"].as_i64(),
            "{at}"
        );
        assert_eq!(s.class, s.kind.class());
        seen.kinds.insert(want_kind.to_owned());
        match s.class {
            MediaClass::Playable { structure_checked } => {
                let ident = r.verify_blob(blob, &cancel).expect("verify");
                assert_eq!(ident.blob(), blob);
                assert_eq!(ident.len(), s.stored_bytes, "{at}");
                let v = r
                    .check_container(blob, &ident, s.kind, &cancel)
                    .expect("verdict");
                assert_eq!(verdict_json(v), w["structure"], "{at}: structure");
                assert_eq!(structure_checked, v != ContainerVerdict::NotChecked, "{at}");
                seen.verdicts.insert(verdict_json(v).to_string());
            }
            MediaClass::Image => {
                let bytes = r
                    .read_blob_verified(blob, MAX_INPUT_BYTES)
                    .expect("verified bytes");
                let file = digests
                    .get(Sha256::digest(bytes.expose()).as_slice())
                    .expect("a media fixture")
                    .clone();
                let got = render(bytes.expose(), Rendition::Thumb, &RenderLimits::default());
                if let Ok(out) = &got {
                    // Nothing of the original's metadata survives.
                    let b = out.bytes.expose();
                    for banned in [&b"Exif"[..], b"GPS", b"<html", b"SYNTH"] {
                        assert!(!b.windows(banned.len()).any(|x| x == banned), "{at}");
                    }
                    assert!(out.width <= 640 && out.height <= 640);
                }
                let shown = got
                    .as_ref()
                    .map(|o| (o.format, o.width, o.height, o.animated))
                    .map_err(|e| *e);
                if let Some(want) = expected_render(&file) {
                    assert_eq!(shown, want, "{at}: {file}");
                }
                seen.renders.insert(file, format!("{shown:?}"));
            }
            _ => {}
        }
    }
    drop(r);
    assert_eq!(
        data_snapshot(&i.tmp, &i.id),
        before,
        "{what}: reading wrote nothing"
    );
}

#[test]
fn sniffs_cards_structure_and_renders_match_the_model() {
    let mut seen = Seen::default();
    let mut docs = 0;
    for (what, model) in model_docs() {
        let sniffs = model["viewer"]["attachments"]
            .as_object()
            .is_some_and(|a| a.values().any(|w| w["sniff"].is_string()));
        if !sniffs {
            continue;
        }
        docs += 1;
        check_model(&what, &model, &mut seen);
    }
    assert!(docs >= 3, "{docs}");
    for k in [
        "png",
        "jpeg",
        "gif",
        "webp",
        "mp4",
        "quicktime",
        "m4a",
        "wav",
        "caf",
        "heif",
        "xml",
        "html",
        "pdf",
        "vcard",
        "text",
        "unknown",
    ] {
        assert!(seen.kinds.contains(k), "{k}: {:?}", seen.kinds);
    }
    for reason in [
        "referenceMovie",
        "externalDataReference",
        "boxPastEnd",
        "sizeOverflow",
        "chunkPastEnd",
    ] {
        assert!(
            seen.verdicts.iter().any(|v| v.contains(reason)),
            "{reason}: {:?}",
            seen.verdicts
        );
    }
    assert!(seen.verdicts.iter().any(|v| v.contains("\"ok\"")));
    for file in [
        "png-rgb-6x4.png",
        "png-alpha-4x4.png",
        "jpeg-exif6-gps-8x6.jpg",
        "jpeg-truncated.jpg",
        "gif-2frames-4x4.gif",
        "gif-html-polyglot.gif",
        "webp-lossless-4x4.webp",
        "png-bomb-12000.png",
        "png-rgba16-4097.png",
        "png-ihdr-60000.png",
    ] {
        assert!(
            seen.renders.contains_key(file),
            "{file}: {:?}",
            seen.renders
        );
    }
}

// ---------------------------------------------------------------------------
// Blob readers
// ---------------------------------------------------------------------------

struct Media {
    i: Imported,
    /// Blob id → the media fixture its bytes are.
    files: HashMap<String, (BlobId, PathBuf)>,
}

fn media_archive() -> Media {
    let (_, model) = model_docs()
        .into_iter()
        .find(|(w, _)| w == "view-media-plain")
        .expect("doc");
    let i = import_order(&model["orders"][0], "media");
    let digests = media_by_digest();
    let c = read_only(&i.db());
    let rows: Vec<(i64, Vec<u8>, String)> = c
        .prepare("SELECT id, sha256, storage_name FROM blob")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    let dir = archive_dir(&i.tmp, &i.id).join("attachments");
    let files = rows
        .into_iter()
        .filter_map(|(id, sha, name)| {
            let file = digests.get(&sha)?.clone();
            let path = dir.join(&name[..2]).join(&name);
            Some((file, (BlobId::new(id), path)))
        })
        .collect();
    Media { i, files }
}

fn blob_err<T: std::fmt::Debug>(r: Result<T, ArchiveError>) -> BlobProblem {
    match r {
        Err(ArchiveError::Blob(p)) => p,
        other => panic!("expected a blob problem, got {other:?}"),
    }
}

fn reader(m: &Media) -> ArchiveReader {
    m.i.lib.open_read(&m.i.id).expect("reader")
}

/// Write `bytes` over a store file, keeping its permissions.
fn overwrite(path: &PathBuf, bytes: &[u8]) {
    let mut perms = fs::metadata(path).expect("meta").permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(path, perms).expect("writable");
    fs::write(path, bytes).expect("write");
}

#[test]
fn open_range_reads_only_a_verified_unchanged_file() {
    let m = media_archive();
    let r = reader(&m);
    let cancel = CancelToken::new();
    let (mp4, mp4_path) = m.files["mp4-minimal.mp4"].clone();
    let (wav, _) = m.files["wav-minimal.wav"].clone();
    let ident = r.verify_blob(mp4, &cancel).expect("verify");
    let other = r.verify_blob(wav, &cancel).expect("verify");
    // Another blob's identity is refused, for streaming and the check.
    assert_eq!(
        blob_err(r.open_range(mp4, &other)),
        BlobProblem::NotVerified
    );
    assert_eq!(
        blob_err(r.check_container(mp4, &other, SniffedKind::Mp4, &cancel)),
        BlobProblem::NotVerified
    );
    // Ranges read the file's bytes; past the end is refused.
    let want = fs::read(&mp4_path).expect("read");
    let mut range = r.open_range(mp4, &ident).expect("range");
    assert_eq!(range.len(), want.len() as u64);
    let mut buf = [0u8; 16];
    assert_eq!(range.read_at(4, &mut buf).expect("read"), 16);
    assert_eq!(&buf[..], &want[4..20]);
    let last = want.len() as u64 - 3;
    assert_eq!(range.read_at(last, &mut buf).expect("tail"), 3);
    assert_eq!(&buf[..3], &want[want.len() - 3..]);
    for past in [want.len() as u64, want.len() as u64 + 1, u64::MAX] {
        let e = range.read_at(past, &mut buf).expect_err("past the end");
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
    }
    assert!(format!("{range:?} {ident:?}").len() < 80);
    drop(range);
    // A changed mtime.
    let f = fs::OpenOptions::new()
        .write(true)
        .open(&mp4_path)
        .or_else(|_| {
            overwrite(&mp4_path, &want);
            fs::OpenOptions::new().write(true).open(&mp4_path)
        })
        .expect("open");
    let t = fs::metadata(&mp4_path)
        .expect("meta")
        .modified()
        .expect("mtime");
    f.set_modified(t + std::time::Duration::from_secs(5))
        .expect("set mtime");
    drop(f);
    assert_eq!(blob_err(r.open_range(mp4, &ident)), BlobProblem::Changed);
    assert_eq!(
        blob_err(r.check_container(mp4, &ident, SniffedKind::Mp4, &cancel)),
        BlobProblem::Changed
    );
    // Verified again, it streams again.
    let ident = r.verify_blob(mp4, &cancel).expect("verify again");
    r.open_range(mp4, &ident).expect("range");
    // A changed length is refused before the identity is compared.
    let mut longer = want.clone();
    longer.push(0);
    overwrite(&mp4_path, &longer);
    assert_eq!(
        blob_err(r.open_range(mp4, &ident)),
        BlobProblem::LengthMismatch
    );
    overwrite(&mp4_path, &want);
    assert_eq!(blob_err(r.open_range(mp4, &ident)), BlobProblem::Changed);
}

#[cfg(unix)]
#[test]
fn open_range_refuses_a_new_inode_a_link_and_a_directory() {
    let m = media_archive();
    let r = reader(&m);
    let cancel = CancelToken::new();
    let (wav, path) = m.files["wav-minimal.wav"].clone();
    let ident = r.verify_blob(wav, &cancel).expect("verify");
    // The same bytes, length and mtime in a new file renamed into place.
    let bytes = fs::read(&path).expect("read");
    let mtime = fs::metadata(&path)
        .expect("meta")
        .modified()
        .expect("mtime");
    let copy = path.with_extension("copy");
    fs::write(&copy, &bytes).expect("copy");
    fs::File::options()
        .write(true)
        .open(&copy)
        .expect("open")
        .set_modified(mtime)
        .expect("mtime");
    fs::rename(&copy, &path).expect("rename");
    assert_eq!(
        fs::metadata(&path).expect("m").modified().expect("t"),
        mtime
    );
    assert_eq!(blob_err(r.open_range(wav, &ident)), BlobProblem::Changed);
    // A link to the same bytes.
    let target = path.with_extension("target");
    fs::rename(&path, &target).expect("move");
    std::os::unix::fs::symlink(&target, &path).expect("symlink");
    assert_eq!(blob_err(r.open_range(wav, &ident)), BlobProblem::NotRegular);
    assert_eq!(blob_err(r.inspect_blob(wav)), BlobProblem::NotRegular);
    assert_eq!(
        blob_err(r.verify_blob(wav, &cancel)),
        BlobProblem::NotRegular
    );
    fs::remove_file(&path).expect("unlink");
    // A directory.
    fs::create_dir(&path).expect("dir");
    assert_eq!(blob_err(r.open_range(wav, &ident)), BlobProblem::NotRegular);
    assert_eq!(
        blob_err(r.read_blob_verified(wav, MAX_INPUT_BYTES)),
        BlobProblem::NotRegular
    );
}

#[test]
fn a_digest_mismatch_returns_nothing_but_the_sniff_window_still_reads() {
    let m = media_archive();
    let r = reader(&m);
    let cancel = CancelToken::new();
    let (png, path) = m.files["png-rgb-6x4.png"].clone();
    let mut bytes = fs::read(&path).expect("read");
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    overwrite(&path, &bytes);
    assert_eq!(r.inspect_blob(png).expect("sniff").kind, SniffedKind::Png);
    assert_eq!(
        blob_err(r.verify_blob(png, &cancel)),
        BlobProblem::DigestMismatch
    );
    assert_eq!(
        blob_err(r.read_blob_verified(png, MAX_INPUT_BYTES)),
        BlobProblem::DigestMismatch
    );
    // Cancel, the size limit and unknown ids.
    let (mov, _) = m.files["mov-minimal.mov"].clone();
    let stop = CancelToken::new();
    stop.cancel();
    assert!(matches!(
        r.verify_blob(mov, &stop),
        Err(ArchiveError::Cancelled)
    ));
    let ident = r.verify_blob(mov, &cancel).expect("verify");
    assert!(matches!(
        r.check_container(mov, &ident, SniffedKind::QuickTime, &stop),
        Err(ArchiveError::Cancelled)
    ));
    assert_eq!(
        r.check_container(mov, &ident, SniffedKind::QuickTime, &cancel)
            .expect("verdict"),
        ContainerVerdict::Ok
    );
    // A kind that is not playable is refused by the check.
    assert_eq!(
        r.check_container(mov, &ident, SniffedKind::Png, &cancel)
            .expect("verdict"),
        ContainerVerdict::Refused(ContainerRefusal::NotPlayable)
    );
    let len = ident.len();
    assert_eq!(
        blob_err(r.read_blob_verified(mov, len - 1)),
        BlobProblem::OverLimit
    );
    assert_eq!(
        r.read_blob_verified(mov, len)
            .expect("bytes")
            .expose()
            .len() as u64,
        len
    );
    let missing = BlobId::new(1_000_000);
    assert_eq!(blob_err(r.inspect_blob(missing)), BlobProblem::NotFound);
    assert_eq!(
        blob_err(r.verify_blob(missing, &cancel)),
        BlobProblem::NotFound
    );
    assert_eq!(
        blob_err(r.read_blob_verified(missing, 1)),
        BlobProblem::NotFound
    );
    // The empty and one-byte files sniff as unknown.
    for f in ["empty.bin", "one-byte.bin"] {
        if let Some((b, _)) = m.files.get(f) {
            assert_eq!(
                r.inspect_blob(*b).expect("sniff").kind,
                SniffedKind::Unknown
            );
        }
    }
}
