//! The synthetic viewer catalogue, `tests/fixtures/viewer` (M5 spec §14.1).
//!
//! - The inputs (`media/` and every backup file) match `SHA256SUMS`, and the
//!   SHA-256 of `SHA256SUMS` itself is pinned here: the inputs never change
//!   once committed.
//! - The model's expectations match `EXPECTED.SHA256SUMS`, whose own SHA-256
//!   is pinned separately: they may be regenerated when a viewer slice
//!   settles a rule (the commit names the rule).
//! - Every `media/` file is rebuilt by `make_viewer_media`'s own code and
//!   compared byte for byte, and each has the property it was made for
//!   (read back with the `image` crate's decoders, header only for the
//!   oversized ones).
//! - Every entry and every scenario imports into an archive that matches the
//!   model's `content` manifest (the M4 import is the producer; this is the
//!   viewer catalogue's import check).
//! - The model's `viewer` statement is self-consistent: every ref names a
//!   manifest row, every chat's rows are partitioned into dated, undated and
//!   folded items, and every masked string is in the archive.
//!
//! The `viewer` expectations themselves are asserted against the viewer
//! reads from slice 5b: the conversations in `viewer_conversations.rs`.
//! The committed fixtures are never written: backups are opened in place
//! with a temporary staging directory.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

#[path = "../examples/make_viewer_media/media.rs"]
mod media;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Cursor;
use std::path::Path;

use common::import::*;
use common::viewer::{self, at, json_at, viewer_root, ENTRIES, SCENARIOS};
use common::*;
use image::codecs::gif::GifDecoder;
use image::codecs::jpeg::JpegDecoder;
use image::{AnimationDecoder, ColorType, ImageDecoder, ImageReader};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The SHA-256 of `tests/fixtures/viewer/SHA256SUMS`. The inputs are
/// frozen: regenerating them invalidates every recorded run.
const INPUT_SUMS_SHA256: &str = "ee33f1fde09cf332384f61595c9705a5b256fb12cf86a0cc8fa0e5483e9080d8";

/// The SHA-256 of `tests/fixtures/viewer/EXPECTED.SHA256SUMS`. Re-pinned
/// after the M5 PR A review: a message whose direction is not recorded
/// has no sender (`directionNotRecorded`); the part set comes only from
/// the runs and the `otr` count; a reaction whose target would fold
/// itself stays visible. Only the main entry's expectations changed.
const EXPECTED_SUMS_SHA256: &str =
    "4d03c9747721e18dc4da5ab2e39226793bdaf16e0246d9f5a121c652cb4f9412";

/// Text values over this many UTF-8 bytes are written as
/// `sha256:<hex>:<bytes>` in the manifests (the generator's `LONG_TEXT`).
const LONG_TEXT: usize = 4096;

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Every file below `dir`, relative to `base`, with `/` separators.
fn walk(dir: &Path, base: &Path, out: &mut BTreeSet<String>) {
    for e in fs::read_dir(dir).expect("read_dir").flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, base, out);
        } else {
            let rel = p.strip_prefix(base).expect("under root");
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.insert(parts.join("/"));
        }
    }
}

fn is_expectation(rel: &str) -> bool {
    let first = rel.split('/').next().unwrap_or_default();
    first.ends_with(".expected") || first == "scenarios"
}

/// Checks one sums file and returns the names it lists.
fn check_sums(name: &str, pinned: &str) -> BTreeSet<String> {
    let raw = fs::read(at(name)).expect("sums");
    assert_eq!(
        sha256_hex(&raw),
        pinned,
        "{name} changed: regenerate deliberately and re-pin it here"
    );
    let text = String::from_utf8(raw).expect("ascii");
    assert!(!text.contains('\r'), "{name} has CRLF line endings");
    let mut listed = BTreeSet::new();
    for line in text.lines() {
        let (want, rel) = line.split_once("  ").expect("line format");
        let bytes = fs::read(at(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert_eq!(sha256_hex(&bytes), want, "{rel} does not match {name}");
        listed.insert(rel.to_owned());
    }
    listed
}

#[test]
fn every_file_is_pinned_by_one_of_the_two_sums() {
    let inputs = check_sums("SHA256SUMS", INPUT_SUMS_SHA256);
    let expected = check_sums("EXPECTED.SHA256SUMS", EXPECTED_SUMS_SHA256);
    assert!(inputs.iter().all(|r| !is_expectation(r)));
    assert!(expected.iter().all(|r| is_expectation(r)));
    let mut all = BTreeSet::new();
    walk(&viewer_root(), &viewer_root(), &mut all);
    for extra in ["SHA256SUMS", "EXPECTED.SHA256SUMS", "README.md"] {
        assert!(all.remove(extra), "{extra} missing");
    }
    let listed: BTreeSet<String> = inputs.union(&expected).cloned().collect();
    assert_eq!(
        all, listed,
        "files not covered by the sums, or listed and missing"
    );
    // The catalogue's shape.
    for e in ENTRIES {
        assert!(
            inputs
                .iter()
                .any(|r| r.starts_with(&format!("{e}/{UDID}/"))),
            "{e}"
        );
        assert!(
            expected.contains(&format!("{e}.expected/content.json")),
            "{e}"
        );
    }
    for s in SCENARIOS {
        assert!(
            expected.contains(&format!("scenarios/{s}.content.json")),
            "{s}"
        );
    }
}

// ---------------------------------------------------------------------------
// Media
// ---------------------------------------------------------------------------

fn media_file(name: &str) -> Vec<u8> {
    fs::read(viewer_root().join("media").join(name)).expect("media file")
}

#[test]
fn media_files_are_what_make_viewer_media_writes() {
    let built = media::all();
    let names: BTreeSet<&str> = built.iter().map(|f| f.name).collect();
    assert_eq!(names.len(), built.len(), "duplicate names");
    let on_disk: BTreeSet<String> = fs::read_dir(viewer_root().join("media"))
        .expect("media dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        on_disk,
        names
            .iter()
            .map(|n| (*n).to_owned())
            .collect::<BTreeSet<_>>()
    );
    for f in &built {
        assert_eq!(
            f.name.to_ascii_lowercase(),
            f.name,
            "media names are lowercase"
        );
        assert!(
            media_file(f.name) == f.bytes,
            "{} differs from what make_viewer_media builds (rerun it, or the image crate's \
             encoders changed)",
            f.name
        );
    }
}

fn decoder_of(bytes: &[u8]) -> Box<dyn ImageDecoder + '_> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .expect("format");
    Box::new(reader.into_decoder().expect("decoder"))
}

#[test]
fn encoded_images_have_their_designed_properties() {
    let img = image::load_from_memory(&media_file("png-rgb-6x4.png")).expect("png");
    assert_eq!(
        (img.width(), img.height(), img.color()),
        (6, 4, ColorType::Rgb8)
    );
    let img = image::load_from_memory(&media_file("png-alpha-4x4.png")).expect("png");
    assert_eq!(img.color(), ColorType::Rgba8);
    assert!(img.to_rgba8().pixels().any(|p| p.0[3] < 255), "has alpha");
    let img = image::load_from_memory(&media_file("webp-lossless-4x4.webp")).expect("webp");
    assert_eq!((img.width(), img.height()), (4, 4));

    // EXIF orientation 6 and a GPS IFD pointer, read by the decoder that
    // the render step will use.
    let jpeg = media_file("jpeg-exif6-gps-8x6.jpg");
    let mut dec = JpegDecoder::new(Cursor::new(&jpeg)).expect("jpeg");
    assert_eq!(dec.dimensions(), (8, 6));
    assert_eq!(
        dec.orientation().expect("orientation"),
        image::metadata::Orientation::Rotate90
    );
    let exif = dec.exif_metadata().expect("exif").expect("an EXIF block");
    assert!(
        exif.windows(2).any(|w| w == [0x25, 0x88]),
        "GPSInfo tag 0x8825"
    );
    image::load_from_memory(&jpeg).expect("decodes");

    // The truncated JPEG: a JPEG start, no end-of-image marker.
    let cut = media_file("jpeg-truncated.jpg");
    assert_eq!(&cut[..3], &[0xff, 0xd8, 0xff]);
    assert_ne!(&cut[cut.len() - 2..], &[0xff, 0xd9]);

    // Two frames.
    let gif = media_file("gif-2frames-4x4.gif");
    let frames = GifDecoder::new(Cursor::new(&gif))
        .expect("gif")
        .into_frames()
        .collect_frames()
        .expect("frames");
    assert_eq!(frames.len(), 2);
    // The polyglot decodes as a GIF and carries HTML after it.
    let poly = media_file("gif-html-polyglot.gif");
    image::load_from_memory(&poly).expect("polyglot decodes as a GIF");
    assert!(poly.windows(7).any(|w| w == b"<script"));
}

#[test]
fn hostile_pngs_exceed_the_caps_from_their_headers() {
    let cap = media::DECODED_CAP;
    let max_alloc: u64 = 160 << 20;
    // The bomb: inside 16384 px, over the decoded cap, under max_alloc, so
    // only the cap refuses it.
    let bomb = media_file("png-bomb-12000.png");
    let d = decoder_of(&bomb);
    assert_eq!(d.dimensions(), (media::BOMB_SIDE, media::BOMB_SIDE));
    assert_eq!(d.color_type(), ColorType::L8);
    assert!(d.total_bytes() > cap && d.total_bytes() < max_alloc);
    assert!(bomb.len() < 200_000, "a small file");
    // 16-bit RGBA: over the cap only when the colour type is counted.
    let rgba16 = media_file("png-rgba16-4097.png");
    let d = decoder_of(&rgba16);
    assert_eq!(d.color_type(), ColorType::Rgba16);
    assert!(d.total_bytes() > cap);
    let side = u64::from(media::RGBA16_SIDE);
    assert!(side * side * 4 < cap, "4 bytes per pixel would pass");
    // The oversized IHDR.
    let huge = media_file("png-ihdr-60000.png");
    assert_eq!(
        decoder_of(&huge).dimensions(),
        (media::HUGE_SIDE, media::HUGE_SIDE)
    );
}

/// The hand-written deflate stream the hostile PNGs use is valid: small
/// images built the same way decode to all-zero pixels, across the
/// remainder cases of its 258-byte matches.
#[test]
fn the_zero_deflate_writer_round_trips() {
    for (w, h) in [(1, 1), (257, 1), (300, 2), (5, 43), (1000, 3)] {
        let png = media::png_zero_image(w, h, 0, 8, 1);
        let img = image::load_from_memory(&png).expect("decodes");
        assert_eq!((img.width(), img.height()), (w, h));
        assert!(img.to_luma8().pixels().all(|p| p.0[0] == 0));
    }
    assert_eq!(media::crc32(b"IEND"), 0xae42_6082);
}

#[test]
fn containers_and_hostile_files_have_their_designed_shape() {
    let mp4 = media_file("mp4-minimal.mp4");
    assert_eq!(&mp4[4..12], b"ftypisom");
    assert!(mp4.windows(4).any(|w| w == b"moov"));
    assert_eq!(&media_file("mov-minimal.mov")[8..12], b"qt  ");
    assert_eq!(&media_file("m4a-minimal.m4a")[8..12], b"M4A ");
    assert_eq!(&media_file("heic-header.heic")[8..12], b"heic");
    assert_eq!(&media_file("caf-header.caf")[..4], b"caff");
    let wav = media_file("wav-minimal.wav");
    assert_eq!((&wav[..4], &wav[8..12]), (&b"RIFF"[..], &b"WAVE"[..]));
    let refm = media_file("mov-refmovie-rmra.mov");
    for atom in [b"rmra", b"rmda", b"rdrf"] {
        assert!(refm.windows(4).any(|w| w == atom));
    }
    assert!(refm.windows(18).any(|w| w == b"http://127.0.0.1:9"));
    let dref = media_file("mov-dref-external.mov");
    assert!(dref.windows(18).any(|w| w == b"http://127.0.0.1:9"));
    let past = media_file("mp4-box-past-eof.mp4");
    let moov = past.windows(4).position(|w| w == b"moov").expect("moov") - 4;
    let size = u32::from_be_bytes(past[moov..moov + 4].try_into().expect("4"));
    assert!(
        moov + size as usize > past.len(),
        "moov claims more than the file"
    );
    let big = media_file("mp4-box-64bit-overflow.mp4");
    assert!(big
        .windows(16)
        .any(|w| w[..8] == [0, 0, 0, 1, b'm', b'd', b'a', b't'] && w[8..] == [0xff; 8]));
    let wav_bad = media_file("wav-chunk-past-eof.wav");
    let data = wav_bad.windows(4).position(|w| w == b"data").expect("data");
    let claimed = u32::from_le_bytes(wav_bad[data + 4..data + 8].try_into().expect("4"));
    assert!(data + 8 + claimed as usize > wav_bad.len());
    assert!(String::from_utf8(media_file("svg-script.svg"))
        .expect("utf-8")
        .contains("<script>"));
    assert!(media_file("html-named-jpg.jpg").starts_with(b"<!DOCTYPE html>"));
    assert!(media_file("empty.bin").is_empty());
    assert_eq!(media_file("one-byte.bin").len(), 1);
}

// ---------------------------------------------------------------------------
// The import check against the model's content manifest
// ---------------------------------------------------------------------------

/// The generator's long-text rule, applied to what is read back.
fn shorten(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for (k, x) in map.iter_mut() {
                match x {
                    Value::String(s) if k == "text" && s.len() > LONG_TEXT => {
                        *x = Value::String(format!(
                            "sha256:{}:{}",
                            sha256_hex(s.as_bytes()),
                            s.len()
                        ));
                    }
                    _ => shorten(x),
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(shorten),
        _ => {}
    }
}

fn sorted_strings(v: &[Value]) -> Vec<String> {
    let mut s: Vec<String> = v.iter().map(Value::to_string).collect();
    s.sort();
    s
}

fn assert_same(got: &[Value], want: &[Value], what: &str) {
    let (g, w) = (sorted_strings(got), sorted_strings(want));
    if g != w {
        let gs: BTreeSet<&String> = g.iter().collect();
        let ws: BTreeSet<&String> = w.iter().collect();
        panic!(
            "{what}: differ\n  only in the archive: {:#?}\n  only in the model: {:#?}",
            gs.difference(&ws).collect::<Vec<_>>(),
            ws.difference(&gs).collect::<Vec<_>>()
        );
    }
}

/// The archive at `db` holds what the manifest `m` says: handles, chats,
/// attachments, messages (with their order), blobs and the masked strings.
fn assert_matches_model(db: &Path, m: &Value, labels: &LineageLabels, what: &str) {
    let mut view = manifest_view(db, labels);
    for plural in ["handles", "chats", "attachments"] {
        let got = view.get_mut(plural).expect("plural");
        got.iter_mut().for_each(shorten);
        assert_same(
            got,
            &manifest_expected(m, plural),
            &format!("{what}: {plural}"),
        );
    }
    let mut got = message_view(db, labels);
    got.iter_mut().for_each(shorten);
    let want = message_expected(m);
    assert_same(&got, &want, &format!("{what}: messages"));
    let sorts = |v: &[Value]| v.iter().map(|x| x["sort"].clone()).collect::<Vec<_>>();
    assert_eq!(sorts(&got), sorts(&want), "{what}: message order");
    assert_eq!(blob_list(db), m["content"]["blobs"], "{what}: blobs");
    assert_store_consistent(db, true);
    // Every masked string the viewer must withhold is in the archive.
    let c = read_only(db);
    for kind in ["oneTimeCodes", "earlierText"] {
        for s in m["viewer"]["masked"][kind].as_array().expect("masked") {
            let s = s.as_str().expect("text");
            let n: i64 = c
                .query_row(
                    "SELECT count(*) FROM message_body WHERE instr(display_text, ?1) > 0",
                    [s],
                    |r| r.get(0),
                )
                .expect("count");
            assert!(
                n > 0,
                "{what}: a masked {kind} string is not in the archive"
            );
        }
    }
}

/// Import the steps of one order into a fresh archive and compare.
fn import_order(order: &Value, m: &Value, what: &str) {
    let i = viewer::import_order(order, what);
    let db = i.db();
    assert_consistent(&db);
    assert_matches_model(&db, m, &i.labels, what);
}

#[test]
fn every_entry_imports_as_the_model_says() {
    for entry in ENTRIES {
        let m = json_at(&format!("{entry}.expected/content.json"));
        assert_eq!(m["format"], "oma-viewer-content");
        let orders = m["orders"].as_array().expect("orders");
        assert_eq!(orders.len(), 1);
        import_order(&orders[0], &m, entry);
    }
}

#[test]
fn every_scenario_imports_as_the_model_says() {
    for name in SCENARIOS {
        let m = json_at(&format!("scenarios/{name}.content.json"));
        for (i, order) in m["orders"].as_array().expect("orders").iter().enumerate() {
            import_order(order, &m, &format!("{name} order {i}"));
        }
    }
}

/// The media entry's store holds exactly the media files' bytes (each
/// attachment's current blob is the file the generator placed).
#[test]
fn media_entry_blobs_are_the_media_files() {
    let m = json_at("view-media-plain.expected/content.json");
    let media: BTreeSet<String> = media::all().iter().map(|f| sha256_hex(&f.bytes)).collect();
    let blobs: BTreeSet<String> = m["content"]["blobs"]
        .as_array()
        .expect("blobs")
        .iter()
        .map(|b| b["sha256"].as_str().expect("sha").to_owned())
        .collect();
    assert_eq!(blobs, media, "one blob per distinct media file");
}

// ---------------------------------------------------------------------------
// The viewer statement's self-consistency
// ---------------------------------------------------------------------------

fn refs(m: &Value, plural: &str) -> BTreeSet<String> {
    m["content"][plural]
        .as_array()
        .expect("list")
        .iter()
        .map(|r| r["ref"].as_str().expect("ref").to_owned())
        .collect()
}

fn check_viewer(m: &Value, what: &str) {
    let v = &m["viewer"];
    let chats = refs(m, "chats");
    let messages = refs(m, "messages");
    let attachments = refs(m, "attachments");
    let handles = refs(m, "handles");
    // Conversations: every chat once.
    let conv: Vec<&str> = v["conversations"]
        .as_array()
        .expect("conversations")
        .iter()
        .map(|c| c["chat"].as_str().expect("chat"))
        .collect();
    assert_eq!(
        conv.iter()
            .map(|s| (*s).to_owned())
            .collect::<BTreeSet<_>>(),
        chats,
        "{what}"
    );
    assert_eq!(conv.len(), chats.len(), "{what}: a chat listed twice");
    // Dated chats come first, newest first.
    let last: Vec<Option<i64>> = v["conversations"]
        .as_array()
        .expect("conversations")
        .iter()
        .map(|c| c["lastDatedUtcNs"].as_i64())
        .collect();
    let dated: Vec<i64> = last.iter().map_while(|x| *x).collect();
    assert!(dated.windows(2).all(|w| w[0] >= w[1]), "{what}: order");
    assert!(last[dated.len()..].iter().all(Option::is_none), "{what}");
    // Threads partition each chat's rows.
    let mut chat_rows: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for msg in m["content"]["messages"].as_array().expect("messages") {
        for c in msg["chats"].as_array().expect("chats") {
            chat_rows
                .entry(c.as_str().expect("chat"))
                .or_default()
                .insert(msg["ref"].as_str().expect("ref").to_owned());
        }
    }
    for (chat, t) in v["threads"].as_object().expect("threads") {
        assert!(chats.contains(chat), "{what}");
        let mut seen = BTreeSet::new();
        for k in ["dated", "undated"] {
            for r in t[k].as_array().expect("items") {
                assert!(seen.insert(r.as_str().expect("ref").to_owned()), "{what}");
            }
        }
        for f in t["folded"].as_array().expect("folded") {
            let target = f["target"].as_str().expect("target").to_owned();
            assert!(
                chat_rows
                    .get(chat.as_str())
                    .is_some_and(|s| s.contains(&target)),
                "{what}: a folded reaction's target is not in its chat"
            );
            assert!(
                seen.insert(f["reaction"].as_str().expect("ref").to_owned()),
                "{what}"
            );
        }
        assert_eq!(
            Some(&seen),
            chat_rows.get(chat.as_str()).or(Some(&BTreeSet::new())),
            "{what}: {chat}'s rows are not partitioned"
        );
    }
    // Every message and attachment is described, and refs resolve.
    let vm: BTreeSet<String> = v["messages"]
        .as_object()
        .expect("messages")
        .keys()
        .cloned()
        .collect();
    assert_eq!(vm, messages, "{what}");
    let va: BTreeSet<String> = v["attachments"]
        .as_object()
        .expect("attachments")
        .keys()
        .cloned()
        .collect();
    assert_eq!(va, attachments, "{what}");
    for (r, f) in v["messages"].as_object().expect("messages") {
        if let Some(h) = f["sender"]["handle"].as_str() {
            assert!(handles.contains(h), "{what}: {r}");
        }
        for key in ["originator"] {
            if let Some(o) = f["reply"][key].as_str() {
                assert!(messages.contains(o), "{what}: {r}");
            }
        }
        if let Some(t) = f["association"]["target"].as_str() {
            assert!(messages.contains(t), "{what}: {r}");
        }
        if let Some(rx) = f["reactions"]["chips"].as_array() {
            for chip in rx {
                for row in chip["rows"].as_array().expect("rows") {
                    assert!(messages.contains(row.as_str().expect("ref")), "{what}");
                }
            }
        }
    }
}

#[test]
fn the_viewer_statement_is_self_consistent() {
    let mut cards = BTreeSet::new();
    let mut labels = BTreeSet::new();
    let mut edit_labels = BTreeSet::new();
    let mut senders = BTreeSet::new();
    let mut items = BTreeSet::new();
    let mut chip_states = BTreeSet::new();
    let mut structure = BTreeSet::new();
    let docs = viewer::model_docs();
    for (what, m) in &docs {
        check_viewer(m, what);
        let v = &m["viewer"];
        for a in v["attachments"].as_object().expect("a").values() {
            cards.insert(a["card"].as_str().expect("card").to_owned());
            if let Some(s) = a["structure"].as_object() {
                structure.insert(format!(
                    "{}:{}",
                    s["state"].as_str().expect("state"),
                    s.get("reason").and_then(Value::as_str).unwrap_or("")
                ));
            }
        }
        for f in v["messages"].as_object().expect("m").values() {
            for l in f["labels"].as_array().expect("labels") {
                let l = l.as_str().expect("label");
                labels.insert(l.split(':').next().expect("name").to_owned());
            }
            edit_labels.insert(f["edit"]["label"].as_str().expect("edit").to_owned());
            senders.insert(f["sender"]["reason"].as_str().expect("sender").to_owned());
            items.insert(f["item"].as_str().expect("item").to_owned());
            for chip in f["reactions"]["chips"].as_array().into_iter().flatten() {
                chip_states.insert(chip["state"].as_str().expect("state").to_owned());
            }
        }
    }
    // The catalogue covers what spec §14.1 asks for.
    for c in [
        "image",
        "play",
        "unrecognised",
        "notCopied",
        "notInBackup",
        "notFoundAtAssumedKey",
        "classNotLookedUp",
        "locationUnusable",
        "couldNotBeRead",
        "file:heif",
        "file:html",
        "file:xml",
        "file:pdf",
        "file:vcard",
        "file:text",
        "file:caf",
    ] {
        assert!(cards.contains(c), "no {c} card in the catalogue");
    }
    for s in [
        "ok:",
        "refused:referenceMovie",
        "refused:externalDataReference",
        "refused:boxPastEnd",
        "refused:sizeOverflow",
        "refused:chunkPastEnd",
    ] {
        assert!(structure.contains(s), "no {s} structure verdict");
    }
    for l in [
        "undated",
        "before2007",
        "unkeyed",
        "scheduleType",
        "appContent",
        "effect",
        "notInLatest",
        "provisional",
    ] {
        assert!(labels.contains(l), "no {l} label");
    }
    for e in [
        "notRecorded",
        "summaryUnreadable",
        "unsentParts",
        "editHistory",
        "dateEditedNoBody",
        "dateEditedOnly",
        "noEditRecorded",
        "undetermined",
    ] {
        assert!(edit_labels.contains(e), "no {e} edit label");
    }
    for s in [
        "fromMe",
        "resolved",
        "noSender",
        "columnAbsent",
        "unreadable",
        "notFound",
        "directionNotRecorded",
    ] {
        assert!(senders.contains(s), "no {s} sender reason");
    }
    assert_eq!(items.len(), 14, "every item kind: {items:?}");
    for s in ["shown", "hidden", "undeterminable", "added"] {
        assert!(chip_states.contains(s), "no {s} chip");
    }
}
