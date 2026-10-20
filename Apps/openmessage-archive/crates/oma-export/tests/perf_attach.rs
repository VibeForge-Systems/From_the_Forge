//! Attachment copy throughput (M6 spec §12: "100,000 files / 50 GB on ext4
//! at disk speed (1 MiB chunks, one hash pass)", measured in slice 6e,
//! `#[ignore]`). A bulk archive (`oma-archive`'s generator with store
//! files, every message with one attachment) is exported whole to TXT with
//! the files copied, through `pipeline::run`; the same bytes are then read
//! and hashed once and copied once with `std::fs::copy` as baselines.
//! Prints `RECORD` lines; never asserts a timing.
//!
//! ```sh
//! OMA_PERF_ATTACH_FILES=2000 OMA_PERF_ATTACH_BYTES=262144 \
//!   cargo test -p oma-export --release --test perf_attach -- --ignored --nocapture
//! ```
//!
//! The full §12 size needs about 100 GB free (store plus copy) and is
//! **not run** here.
#![allow(clippy::disallowed_methods, clippy::disallowed_types, dead_code)]

#[path = "../../oma-archive/tests/common/mod.rs"]
mod archive_common;
mod common;
mod files;

use std::io::Read;
use std::time::Instant;

use archive_common::bulk::{generate, BulkSpec};
use oma_export::plan::ScopePlan;

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[test]
#[ignore = "throughput measurement; run by hand"]
fn attachment_copy_throughput() {
    let n = env("OMA_PERF_ATTACH_FILES", 2_000);
    let len = env("OMA_PERF_ATTACH_BYTES", 256 * 1024);
    let (tmp, lib) = archive_common::library();
    let id = lib.create(None).expect("create");
    let mut a = lib.open(&id).expect("open");
    let mut spec = BulkSpec::new(n);
    spec.attachment_permille = 1_000;
    spec.blobs = true;
    spec.blob_len = len;
    let t = Instant::now();
    let report = generate(&mut a, &spec);
    drop(a);
    println!(
        "RECORD perf_attach generate files={} bytes={} secs={:.2}",
        report.blobs,
        report.blob_bytes,
        t.elapsed().as_secs_f64()
    );
    let store = archive_common::archive_dir(&tmp, &id).join("attachments");
    let i = common::Imported { tmp, lib, id };
    let d = files::dest();
    let t = Instant::now();
    let r = common::export_txt(
        &i,
        &d.parent,
        ScopePlan::Archive,
        &|_| {},
        "Perf export",
        None,
    )
    .expect("export");
    let secs = t.elapsed().as_secs_f64();
    let bytes = r.attachments.bytes_written;
    println!(
        "RECORD perf_attach export files={} bytes={bytes} items={} secs={secs:.2} mib_s={:.1}",
        r.attachments.files_written,
        r.counts.items,
        bytes as f64 / (1 << 20) as f64 / secs
    );
    assert_eq!(r.attachments.files_written, report.blobs);
    // Baselines over the same store files: one read with one hash, and
    // one `fs::copy`.
    let mut paths = Vec::new();
    for shard in std::fs::read_dir(&store).expect("store").flatten() {
        if shard.path().is_dir() {
            for f in std::fs::read_dir(shard.path()).expect("shard").flatten() {
                paths.push(f.path());
            }
        }
    }
    let t = Instant::now();
    let mut buf = vec![0u8; 1 << 20];
    for p in &paths {
        use sha2::Digest;
        let mut h = sha2::Sha256::new();
        let mut f = std::fs::File::open(p).expect("open");
        loop {
            let k = f.read(&mut buf).expect("read");
            if k == 0 {
                break;
            }
            h.update(&buf[..k]);
        }
        let _ = h.finalize();
    }
    let read_secs = t.elapsed().as_secs_f64();
    let copy_dir = d.tmp.path().join("baseline");
    std::fs::create_dir(&copy_dir).expect("baseline dir");
    let t = Instant::now();
    for (k, p) in paths.iter().enumerate() {
        std::fs::copy(p, copy_dir.join(format!("{k}"))).expect("copy");
    }
    let copy_secs = t.elapsed().as_secs_f64();
    println!(
        "RECORD perf_attach baseline read_and_hash_secs={read_secs:.2} fs_copy_secs={copy_secs:.2}"
    );
}
