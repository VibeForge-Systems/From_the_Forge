//! The export perf smoke (M6 spec §12, slice 6f): a bulk archive is
//! exported whole to every in-process format (TXT, CSV and JSON) through
//! `pipeline::run`, in a child process that reports its peak resident set
//! (`VmHWM` from `/proc/self/status`) and its wall time; the parent
//! asserts the memory bound. Linux only (Windows has no `VmHWM`); the file
//! compiles to nothing elsewhere.
//!
//! - **On every push:** 20,000 messages, peak RSS under 150 MiB.
//! - **By hand** (`#[ignore]`): the spec's 500,000 messages, RSS under
//!   300 MiB; the "under 5 minutes" target is printed, never asserted:
//!
//! ```sh
//! cargo test -p oma-export --release --test perf_smoke -- --ignored --exact \
//!   half_a_million_messages_to_txt_csv_and_json --nocapture
//! ```
//!
//! - **PDF, by hand** (`#[ignore]`, slice 6j2): 80,000 messages with 20 %
//!   CJK and 20 % emoji bodies and a preview for every attachment (a
//!   48 KiB JPEG, then a 640 × 480 PNG, then none), through the PDF writer with the backend in the
//!   child's own process (`render::pdf::InProcess`): the child's peak
//!   resident set is an upper bound on the helper's, which runs only the
//!   backend. The spec's "under 30 s, helper under 200 MiB" is printed;
//!   only the memory bound is asserted:
//!
//! ```sh
//! cargo test -p oma-export --release --test perf_smoke -- --ignored --exact \
//!   eighty_thousand_messages_to_pdf --nocapture
//! ```
//!
//! Timings are printed as `RECORD` lines and never asserted.
#![cfg(target_os = "linux")]
#![allow(clippy::disallowed_methods, clippy::disallowed_types, dead_code)]

#[path = "../../oma-archive/tests/common/mod.rs"]
mod archive_common;
mod common;
mod files;

use std::time::Instant;

use archive_common::bulk::{generate, BulkSpec};
use oma_archive::{ArchiveId, Library};
use oma_export::plan::{AttachmentMode, Format, ScopePlan};
use oma_export::ExportZone;

const ENV_ROOT: &str = "OMA_TEST_PERF_ROOT";
const ENV_ARCHIVE: &str = "OMA_TEST_PERF_ARCHIVE";
const ENV_DEST: &str = "OMA_TEST_PERF_DEST";
const ENV_PDF: &str = "OMA_TEST_PERF_PDF";

/// CRC-32 (ISO 3309), for the test PNG's chunks.
fn crc32(bytes: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for b in bytes {
        c ^= u32::from(*b);
        for _ in 0..8 {
            c = if c & 1 == 1 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// A 640 × 480 RGB PNG of a gradient with noise (a thumbnail-sized
/// rendition that does not compress to nothing).
fn thumbnail_png() -> Vec<u8> {
    let (w, h) = (640u32, 480u32);
    let mut raw = Vec::with_capacity((w * 3 + 1) as usize * h as usize);
    let mut x = 0x1234_5678u32;
    for y in 0..h {
        raw.push(0);
        for i in 0..w {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            raw.extend_from_slice(&[(i % 256) as u8, (y % 256) as u8, (x & 0x3F) as u8]);
        }
    }
    let idat = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], data: &[u8]| {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(data);
        png.extend_from_slice(&c);
        png.extend_from_slice(&crc32(&c).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &idat);
    chunk(b"IEND", &[]);
    png
}

/// A JPEG thumbnail's bytes: the viewer catalogue's synthetic 8 × 6 JPEG
/// with a 48 KiB comment segment after its SOI, about what a 640 px
/// quality-85 rendition weighs. krilla embeds a JPEG as it is (no
/// decode), so its cost is its bytes.
fn thumbnail_jpeg() -> Vec<u8> {
    let small = std::fs::read(
        common::fixtures("viewer")
            .join("media")
            .join("jpeg-exif6-gps-8x6.jpg"),
    )
    .expect("the synthetic JPEG");
    let mut out = small[..2].to_vec();
    let pad = 48 * 1024;
    out.extend_from_slice(&[0xFF, 0xFE]);
    out.extend_from_slice(&((pad + 2) as u16).to_be_bytes());
    out.extend((0..pad).map(|i| (i % 251) as u8));
    out.extend_from_slice(&small[2..]);
    out
}

/// The same thumbnail for every attachment the archive holds a file for.
struct Thumbs(Vec<u8>);

impl oma_export::preview::PreviewSource for Thumbs {
    fn renditions(
        &mut self,
        a: &oma_export::record::AttachmentRecord,
        _display: bool,
    ) -> Result<oma_export::preview::Preview, oma_export::ExportError> {
        Ok(match a.file.sniffed {
            Some(_) => oma_export::preview::Preview::Ready {
                thumb: oma_export::preview::RenditionBytes(self.0.clone()),
                display: None,
            },
            None => oma_export::preview::Preview::None,
        })
    }
}

fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// The child: export the archive and report. Does nothing unless the
/// parent set the variables.
#[test]
#[ignore = "helper process for the perf smoke tests"]
fn child_exports_the_bulk_archive() {
    let (Ok(root), Ok(archive), Ok(dest)) = (
        std::env::var(ENV_ROOT),
        std::env::var(ENV_ARCHIVE),
        std::env::var(ENV_DEST),
    ) else {
        return;
    };
    let lib = Library::new(std::path::Path::new(&root)).expect("library");
    let id = ArchiveId::parse(&archive).expect("id");
    let i = common::Imported {
        tmp: tempfile::tempdir().expect("unused"),
        lib,
        id,
    };
    let dest = std::path::PathBuf::from(dest);
    let t = Instant::now();
    let r = if let Some(mode) = std::env::var_os(ENV_PDF) {
        let previews: Option<Box<dyn oma_export::preview::PreviewSource>> = if mode == "jpeg" {
            Some(Box::new(Thumbs(thumbnail_jpeg())))
        } else if mode == "png" {
            Some(Box::new(Thumbs(thumbnail_png())))
        } else {
            None
        };
        let with_previews = previews.is_some();
        let pdf = common::pdf_writer(previews);
        common::export_with(
            &i,
            &dest,
            ScopePlan::Archive,
            &[Format::Pdf],
            &|o| {
                o.attachments = AttachmentMode::CopyOriginals;
                o.previews = with_previews;
            },
            "Perf export",
            None,
            &ExportZone::Utc,
            vec![Box::new(pdf)],
        )
    } else {
        common::export_formats(
            &i,
            &dest,
            ScopePlan::Archive,
            &[Format::Txt, Format::Csv, Format::Json],
            &|o| o.attachments = AttachmentMode::None,
            "Perf export",
            None,
            &ExportZone::Utc,
        )
    }
    .expect("export");
    if std::env::var_os(ENV_PDF).is_some() {
        let w = &r.writers[0].counts;
        println!(
            "OMA-PDF pages={} volumes={} previews={} notdef={} wraps={}",
            w["pdfPages"],
            w["pdfVolumes"],
            w["previewsEmbedded"],
            w["pdfUndrawableCharacters"],
            w["pdfForcedWraps"]
        );
    }
    let secs = t.elapsed().as_secs_f64();
    let bytes: u64 = files::tree(&dest.join("Perf export"))
        .iter()
        .filter(|p| !p.ends_with('/'))
        .map(|p| {
            std::fs::metadata(common::under(&dest.join("Perf export"), p))
                .map(|m| m.len())
                .unwrap_or(0)
        })
        .sum();
    let peak = peak_rss_kib().expect("VmHWM");
    println!(
        "OMA-PERF items={} folded={} bytes={bytes} secs={secs:.2} vmhwm_kib={peak}",
        r.counts.items, r.counts.folded_reaction_rows
    );
}

fn run(messages: usize, bound_kib: u64) {
    run_spec(BulkSpec::new(messages), bound_kib, None);
}

/// `pdf`: `Some("jpeg")`, `Some("png")` or `Some("text")` exports to PDF
/// instead (with JPEG or PNG previews, or none).
fn run_spec(spec: BulkSpec, bound_kib: u64, pdf: Option<&str>) {
    let messages = spec.messages;
    let (tmp, lib) = archive_common::library();
    let id = lib.create(None).expect("create");
    let t = Instant::now();
    {
        let mut a = lib.open(&id).expect("open");
        generate(&mut a, &spec);
    }
    println!(
        "RECORD perf_smoke generate messages={messages} secs={:.2}",
        t.elapsed().as_secs_f64()
    );
    let d = files::dest();
    let root = dunce::canonicalize(tmp.path()).expect("canonical");
    let mut child = std::process::Command::new(std::env::current_exe().expect("exe"));
    if let Some(mode) = pdf {
        child.env(ENV_PDF, mode);
    }
    let out = child
        .args([
            "--ignored",
            "--exact",
            "child_exports_the_bulk_archive",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ENV_ROOT, &root)
        .env(ENV_ARCHIVE, id.as_str())
        .env(ENV_DEST, &d.parent)
        .output()
        .expect("child");
    assert!(out.status.success(), "the child failed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find_map(|l| l.find("OMA-PERF").and_then(|i| l.get(i..)))
        .expect("the child's report");
    let field = |k: &str| -> f64 {
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(&format!("{k}=")))
            .and_then(|v| v.parse().ok())
            .expect(k)
    };
    println!("RECORD perf_smoke messages={messages} {line}");
    if let Some(l) = stdout
        .lines()
        .find_map(|l| l.find("OMA-PDF").and_then(|i| l.get(i..)))
    {
        println!("RECORD perf_pdf {l}");
    }
    let rows = field("items") + field("folded");
    assert!(rows >= messages as f64 * 0.9, "{rows} rows");
    let peak = field("vmhwm_kib") as u64;
    assert!(
        peak < bound_kib,
        "peak resident set {peak} KiB over {bound_kib} KiB"
    );
}

#[test]
fn twenty_thousand_messages_to_txt_csv_and_json_under_150_mib() {
    run(20_000, 150 * 1024);
}

#[test]
#[ignore = "about 500,000 messages; run by hand"]
fn half_a_million_messages_to_txt_csv_and_json() {
    run(500_000, 300 * 1024);
}

#[test]
#[ignore = "80,000 messages to PDF with CJK, emoji and previews; run by hand"]
fn eighty_thousand_messages_to_pdf() {
    let mut spec = BulkSpec::new(80_000);
    spec.cjk_permille = 200;
    spec.emoji_permille = 200;
    spec.attachment_permille = 20;
    spec.blobs = true;
    // JPEG previews (photos: embedded as they are), PNG previews (images
    // with alpha: decoded and deflated again by krilla, the worst case),
    // and none (the text's share of the time).
    run_spec(spec.clone(), 200 * 1024, Some("jpeg"));
    run_spec(spec.clone(), 200 * 1024, Some("png"));
    run_spec(spec, 200 * 1024, Some("text"));
}
