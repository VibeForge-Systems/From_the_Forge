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
    let r = common::export_formats(
        &i,
        &dest,
        ScopePlan::Archive,
        &[Format::Txt, Format::Csv, Format::Json],
        &|o| o.attachments = AttachmentMode::None,
        "Perf export",
        None,
        &ExportZone::Utc,
    )
    .expect("export");
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
    let (tmp, lib) = archive_common::library();
    let id = lib.create(None).expect("create");
    let t = Instant::now();
    {
        let mut a = lib.open(&id).expect("open");
        generate(&mut a, &BulkSpec::new(messages));
    }
    println!(
        "RECORD perf_smoke generate messages={messages} secs={:.2}",
        t.elapsed().as_secs_f64()
    );
    let d = files::dest();
    let root = dunce::canonicalize(tmp.path()).expect("canonical");
    let out = std::process::Command::new(std::env::current_exe().expect("exe"))
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
