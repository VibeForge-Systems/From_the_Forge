//! The hostile-history memory bound (M6 spec §3.3, §12, slice 6b2): one
//! message with many 16 MiB body versions is exported in bounded memory,
//! because its history streams one version at a time and its batch holds
//! it alone.
//!
//! The parent builds the archive, then runs this test binary again as a
//! child that opens it, assembles the message with its history and walks
//! every version; the child reports its peak resident set (`VmHWM` from
//! `/proc/self/status`) and the parent asserts the bound. Linux only: the
//! file compiles to nothing elsewhere (Windows has no `VmHWM`).
//!
//! - **On every push:** 21 versions of 16 MiB (336 MiB of text, more than
//!   the 300 MiB bound, so a history held whole would fail it).
//! - **By hand** (`#[ignore]`): the spec's 200 versions of 16 MiB
//!   (3.2 GiB of text, and as much disk):
//!
//! ```sh
//! cargo test -p oma-archive --test export_memory -- --ignored --exact \
//!   history_of_two_hundred_16_mib_versions_streams_under_300_mib --nocapture
//! ```
#![cfg(target_os = "linux")]
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use common::bulk::{generate, BulkSpec};
use common::*;
use oma_archive::{
    ArchiveId, BatchLimits, CancelToken, ExportDepth, HistoryState, Library, MessageId, Reveal,
    VersionText,
};

const ENV_ROOT: &str = "OMA_TEST_HISTORY_ROOT";
const ENV_ARCHIVE: &str = "OMA_TEST_HISTORY_ARCHIVE";
const ENV_MESSAGE: &str = "OMA_TEST_HISTORY_MESSAGE";
/// The bound the spec sets (§12): 300 MiB.
const BOUND_KIB: u64 = 300 * 1024;
const VERSION_BYTES: i64 = 16 << 20;

fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// The child: walk the message's export with its history and report the
/// peak resident set. Does nothing unless the parent set the variables.
#[test]
#[ignore = "helper process for the history memory tests"]
fn child_walks_a_hostile_history() {
    let (Ok(root), Ok(archive), Ok(message)) = (
        std::env::var(ENV_ROOT),
        std::env::var(ENV_ARCHIVE),
        std::env::var(ENV_MESSAGE),
    ) else {
        return;
    };
    let lib = Library::new(std::path::Path::new(&root)).expect("library");
    let id = ArchiveId::parse(&archive).expect("id");
    let m = MessageId::new(message.parse().expect("message id"));
    let r = lib.open_read(&id).expect("reader");
    let hold = lib.hold_for_export(&id).expect("hold");
    let (versions, text_bytes, batch) = r
        .with_export_snapshot(&hold, &CancelToken::new(), |s| {
            // The pre-measure puts it in a batch of its own.
            let mut cur = s
                .by_ids(&[m, MessageId::new(1)])
                .with_depth(ExportDepth::WithHistory)
                .with_limits(BatchLimits::DEFAULT);
            let first = cur.next_batch()?.expect("a batch");
            let x = s
                .assemble(&first.items, Reveal::default(), ExportDepth::WithHistory)?
                .remove(0);
            let mut h = s.history(&x, Reveal::default())?;
            let (mut n, mut bytes) = (0u64, 0u64);
            while let Some(v) = h.next_version()? {
                n += 1;
                if let VersionText::Text(p) = &v.text {
                    for piece in p {
                        if let oma_archive::TextPiece::Text(t) = piece {
                            bytes += t.text.expose().len() as u64;
                        }
                    }
                }
            }
            Ok::<_, oma_archive::ArchiveError>((n, bytes, first.items.len()))
        })
        .expect("snapshot");
    let peak = peak_rss_kib().expect("VmHWM");
    println!(
        "OMA-HISTORY versions={versions} text_bytes={text_bytes} batch={batch} vmhwm_kib={peak}"
    );
}

/// An archive whose message has `versions` earlier versions of
/// [`VERSION_BYTES`] each, plus a current body of the same size.
fn hostile(versions: i64) -> (tempfile::TempDir, Library, ArchiveId, i64) {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let mut a = lib.open(&id).expect("open");
        generate(&mut a, &BulkSpec::new(20));
    }
    let c = raw(&db_path(&tmp, &id));
    // No FTS work: the export does not search, and the index's size is
    // not what this test measures.
    c.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER,
        false,
    )
    .expect("triggers off");
    let (m, body): (i64, i64) = c
        .query_row(
            "SELECT id, current_body_id FROM message WHERE assoc_present = 0 \
               AND current_body_id IS NOT NULL ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("a message");
    c.execute(
        "UPDATE message_body SET display_text = printf('%.*c', ?2, 'c'), display_source = 2 \
         WHERE id = ?1",
        [body, VERSION_BYTES],
    )
    .expect("a large current body");
    let cols: Vec<String> = c
        .prepare("PRAGMA table_info(message_body)")
        .expect("pragma")
        .query_map([], |r| r.get::<_, String>(1))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("columns");
    let select: Vec<&str> = cols
        .iter()
        .map(|col| match col.as_str() {
            "id" => "?1",
            "raw_hash" => "randomblob(32)",
            "text_raw" => "CAST(?2 AS BLOB)",
            // Each version differs in its first character.
            "display_text" => "char(65 + ?2 % 26) || printf('%.*c', ?3 - 1, 'v')",
            other => other,
        })
        .collect();
    let sql = format!(
        "INSERT INTO message_body ({}) SELECT {} FROM message_body WHERE id = ?4",
        cols.join(", "),
        select.join(", ")
    );
    let max = c
        .query_row("SELECT max(id) FROM message_body", [], |r| {
            r.get::<_, i64>(0)
        })
        .expect("max");
    for i in 1..=versions {
        c.execute(&sql, rusqlite::params![max + i, i, VERSION_BYTES, body])
            .expect("version");
        if i % 4 == 0 {
            // Keep the write-ahead log, and so the disk, small.
            c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
                .expect("checkpoint");
        }
    }
    c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .expect("checkpoint");
    drop(c);
    (tmp, lib, id, m)
}

fn run(versions: i64) {
    let (tmp, lib, id, m) = hostile(versions);
    // The counts, in this process (no text is read for them).
    {
        let r = lib.open_read(&id).expect("reader");
        let hold = lib.hold_for_export(&id).expect("hold");
        let x = r
            .with_export_snapshot(&hold, &CancelToken::new(), |s| {
                let sizes = s.measure(&[MessageId::new(m)], ExportDepth::WithHistory)?;
                assert!(sizes[0] > (versions as u64) * VERSION_BYTES as u64);
                Ok::<_, oma_archive::ArchiveError>(
                    s.assemble(&[MessageId::new(m)], Reveal::default(), ExportDepth::Items)?
                        .remove(0)
                        .history,
                )
            })
            .expect("snapshot");
        assert!(matches!(
            x,
            HistoryState::NotIncluded { captured_versions, .. }
                if captured_versions == versions as u64 + 1
        ));
    }
    let out = std::process::Command::new(std::env::current_exe().expect("exe"))
        .args([
            "--ignored",
            "--exact",
            "child_walks_a_hostile_history",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ENV_ROOT, root(&tmp))
        .env(ENV_ARCHIVE, id.as_str())
        .env(ENV_MESSAGE, m.to_string())
        .output()
        .expect("child");
    assert!(out.status.success(), "the child failed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The harness may print the test's name on the same line.
    let line = stdout
        .lines()
        .find_map(|l| l.find("OMA-HISTORY").and_then(|i| l.get(i..)))
        .expect("the child's report");
    let field = |k: &str| -> u64 {
        line.split_whitespace()
            .find_map(|w| w.strip_prefix(&format!("{k}=")))
            .and_then(|v| v.parse().ok())
            .expect(k)
    };
    println!("RECORD {line}");
    assert_eq!(field("versions"), versions as u64 + 1);
    assert_eq!(field("batch"), 1, "a large message is a batch of its own");
    assert!(field("text_bytes") >= (versions as u64 + 1) * VERSION_BYTES as u64);
    let peak = field("vmhwm_kib");
    assert!(
        peak < BOUND_KIB,
        "peak resident set {peak} KiB over {BOUND_KIB} KiB"
    );
}

#[test]
fn history_of_21_16_mib_versions_streams_under_300_mib() {
    run(20);
}

#[test]
#[ignore = "3.2 GiB of disk and several minutes; run by hand"]
fn history_of_two_hundred_16_mib_versions_streams_under_300_mib() {
    run(199);
}
