//! The bulk generator (M5 spec §14.2, `common/bulk.rs`): a generated
//! archive is a valid v1 archive laid out like an imported one.
//!
//! - It passes a quick [`Archive::verify`] (schema, `quick_check`, foreign
//!   keys, the FTS rank-1 integrity check, the store).
//! - Finalize ran: `chat_stats` and `archive_stats` agree with the rows,
//!   and the FTS merge completed.
//! - Every `chat_message.sort_key` equals its message's, dated keys follow
//!   the dates, and undated rows sort after every dated one.
//! - Reactions target an earlier message of the same chat; replies name an
//!   existing message; full-text search finds generated tokens.
//! - With the blob option (M6 slice 6c), every attachment has its own
//!   store file: a full [`Archive::verify`] re-hashes them all, and
//!   `open_blob` reads back exactly what the generator wrote.
//!
//! `calibrate_bulk_against_import` (ignored) compares a bulk archive with an
//! import-built one of the same size; its numbers are recorded in
//! `docs/compatibility.md` as synthetic.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::time::Instant;

use common::bulk::{generate, BulkSpec};
use common::import::*;
use common::*;
use oma_archive::{
    ArchiveCtx, CancelToken, FixedClock, ImportOutcome, LineageChoice, NoProgress, ProceedWith,
    VerifyDepth, VerifyOutcome,
};

fn quick_verify(a: &mut oma_archive::Archive) -> VerifyOutcome {
    let cancel = CancelToken::new();
    let clock = FixedClock(MODEL_NOW_NS);
    let mut progress = NoProgress;
    let mut ctx = ArchiveCtx {
        cancel: &cancel,
        progress: &mut progress,
        clock: &clock,
    };
    a.verify(VerifyDepth::Quick, &mut ctx)
        .expect("verify")
        .outcome
}

fn verify(a: &mut oma_archive::Archive, depth: VerifyDepth) -> VerifyOutcome {
    let cancel = CancelToken::new();
    let clock = FixedClock(MODEL_NOW_NS);
    let mut progress = NoProgress;
    let mut ctx = ArchiveCtx {
        cancel: &cancel,
        progress: &mut progress,
        clock: &clock,
    };
    a.verify(depth, &mut ctx).expect("verify").outcome
}

fn count(c: &rusqlite::Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).expect(sql)
}

#[test]
fn a_bulk_archive_is_a_valid_v1_archive() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut a = lib.open(&id).expect("open");
    let mut spec = BulkSpec::new(5_000);
    // Small batches, so several of them and the FTS automerge run.
    spec.batch_rows = 700;
    let r = generate(&mut a, &spec);
    assert_eq!(r.messages, 5_000);
    assert!(
        r.reactions > 100 && r.replies > 20 && r.undated > 0 && r.attachments > 0,
        "{r:?}"
    );
    assert!(r.cjk > 20, "{r:?}");
    assert_eq!(r.batches, 1 + 5_000_u64.div_ceil(700));
    assert!(r.fts_merge_complete, "{r:?}");
    assert_eq!(quick_verify(&mut a), VerifyOutcome::Ok);
    drop(a);

    let db = db_path(&tmp, &id);
    let c = read_only(&db);
    assert_eq!(count(&c, "SELECT count(*) FROM message"), 5_000);
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM message WHERE current_body_id IS NULL"
        ),
        0
    );
    // Finalize's statistics.
    assert_eq!(
        count(&c, "SELECT count(*) FROM chat_stats"),
        count(&c, "SELECT count(*) FROM chat")
    );
    assert_eq!(
        count(&c, "SELECT sum(message_count) FROM chat_stats"),
        5_000
    );
    assert_eq!(
        count(&c, "SELECT sum(undated_count) FROM chat_stats"),
        i64::try_from(r.undated).expect("n")
    );
    assert_eq!(count(&c, "SELECT messages FROM archive_stats"), 5_000);
    // Sort keys: the import's encoder, copied to chat_message.
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM chat_message cm JOIN message m ON m.id = cm.message_id \
             WHERE cm.sort_key <> m.sort_key"
        ),
        0
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM message WHERE (date_utc_ns IS NULL) <> (sort_key >= X'01')"
        ),
        0
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM message a JOIN message b ON a.id < b.id \
             WHERE a.date_utc_ns IS NOT NULL AND b.date_utc_ns IS NOT NULL \
               AND b.id = a.id + 1 AND NOT (a.sort_key < b.sort_key)"
        ),
        0,
        "dated keys follow the (increasing) dates"
    );
    // Reactions and replies.
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM message r WHERE r.assoc_present = 1 AND NOT EXISTS ( \
               SELECT 1 FROM message t JOIN chat_message ct ON ct.message_id = t.id \
               JOIN chat_message cr ON cr.message_id = r.id AND cr.chat_id = ct.chat_id \
               WHERE t.key_kind = 1 AND t.guid = r.assoc_target_guid AND t.id < r.id)"
        ),
        0,
        "every reaction targets an earlier message of its chat"
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM message r WHERE r.thread_originator_guid IS NOT NULL \
             AND NOT EXISTS (SELECT 1 FROM message t WHERE t.key_kind = 1 \
                 AND t.guid = r.thread_originator_guid)"
        ),
        0
    );
    // Full-text search over generated text.
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM message_fts WHERE message_fts MATCH 'SYNTHBULK4321'"
        ),
        1
    );
    assert!(
        count(
            &c,
            "SELECT count(*) FROM message_fts WHERE message_fts MATCH 'zephyr'"
        ) > 0
    );
    // One very long chat.
    assert!(
        count(&c, "SELECT max(message_count) FROM chat_stats") > 1_500,
        "chat 1 holds about 40 %"
    );
    assert_store_consistent(&db, true);
}

fn peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Facts about one archive database for the calibration.
fn facts(db: &std::path::Path) -> String {
    let c = read_only(db);
    let pages = count(&c, "PRAGMA page_count");
    let fts = count(
        &c,
        "SELECT coalesce(sum(length(block)), 0) FROM message_fts_data",
    );
    let fts_rows = count(&c, "SELECT count(*) FROM message_fts_data");
    let size = std::fs::metadata(db).map(|m| m.len()).unwrap_or(0);
    format!("db_bytes={size} pages={pages} fts_data_bytes={fts} fts_data_rows={fts_rows}")
}

/// The bulk generator against an import-built archive of the same size
/// (spec §14.2): database size, page count, FTS size, and the time of a
/// quick verify. Prints numbers only. The bulk archive uses the perf
/// generator's sentinel bodies unless `OMA_BULK_MIXED_TEXT` is set.
///
/// ```sh
/// OMA_PERF_MESSAGES=100000 cargo test -p oma-archive --release --test bulk -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a measurement; run with --ignored --nocapture"]
fn calibrate_bulk_against_import() {
    let messages: usize = std::env::var("OMA_PERF_MESSAGES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);
    // The import-built archive, from the perf generator's sms.db.
    let handles = (messages / 50).max(16);
    let chats = (messages / 100).max(16);
    let sms = synthetic_sms(5, handles, chats, messages);
    let btmp = tempfile::tempdir().expect("tempdir");
    let backup = backup_with_sms(&btmp.path().join("backup"), &sms);
    let (itmp, ilib) = library();
    let iid = ilib.create(None).expect("create");
    let mut a = ilib.open(&iid).expect("open");
    let src = open_dir(&backup, None);
    let t = Instant::now();
    let outcome = {
        let cancel = CancelToken::new();
        let clock = FixedClock(MODEL_NOW_NS);
        let mut progress = NoProgress;
        let mut ctx = ArchiveCtx {
            cancel: &cancel,
            progress: &mut progress,
            clock: &clock,
        };
        a.import(
            &src.ob,
            &opts(1000),
            &mut ProceedWith(LineageChoice::New),
            &mut ctx,
        )
        .expect("import")
    };
    let import_s = t.elapsed().as_secs_f64();
    assert!(matches!(outcome, ImportOutcome::Completed(_)));
    let t = Instant::now();
    assert_eq!(quick_verify(&mut a), VerifyOutcome::Ok);
    let iverify = t.elapsed().as_secs_f64();
    let _ = a.close();
    src.ob.close();
    let idb = db_path(&itmp, &iid);
    let imsgs = count(&read_only(&idb), "SELECT count(*) FROM message");

    // The bulk archive with the same number of messages, chats and handles.
    let (btmp2, blib) = library();
    let bid = blib.create(None).expect("create");
    let mut b = blib.open(&bid).expect("open");
    let mut spec = BulkSpec::new(usize::try_from(imsgs).expect("n"));
    spec.chats = chats;
    spec.handles = handles;
    spec.sentinel_bodies = std::env::var_os("OMA_BULK_MIXED_TEXT").is_none();
    let r = generate(&mut b, &spec);
    let t = Instant::now();
    assert_eq!(quick_verify(&mut b), VerifyOutcome::Ok);
    let bverify = t.elapsed().as_secs_f64();
    let _ = b.close();
    let bdb = db_path(&btmp2, &bid);
    println!(
        "CALIBRATE import messages={imsgs} import_s={import_s:.2} quick_verify_s={iverify:.2} {}",
        facts(&idb)
    );
    println!(
        "CALIBRATE bulk messages={} write_s={:.2} finalize_s={:.2} fts_merge_steps={} \
         quick_verify_s={bverify:.2} {} peak_rss_kib={:?}",
        r.messages,
        r.write_time.as_secs_f64(),
        r.finalize_time.as_secs_f64(),
        r.fts_merge_steps,
        facts(&bdb),
        peak_rss_kib()
    );
}

#[test]
fn the_blob_option_gives_every_attachment_a_verified_store_file() {
    use std::io::Read;

    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    let mut a = lib.open(&id).expect("open");
    let mut spec = BulkSpec::new(3_000);
    spec.batch_rows = 1_000;
    spec.attachment_permille = 40;
    spec.blobs = true;
    spec.blob_len = 4_096;
    let r = generate(&mut a, &spec);
    assert!(r.attachments > 50, "{r:?}");
    assert_eq!(r.blobs, r.attachments);
    assert_eq!(r.blob_bytes, r.blobs * 4_096);
    assert_eq!(verify(&mut a, VerifyDepth::Full), VerifyOutcome::Ok);
    drop(a);

    let c = read_only(&db_path(&tmp, &id));
    assert_eq!(
        count(&c, "SELECT count(*) FROM blob") as u64,
        r.blobs,
        "one blob per attachment"
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM attachment WHERE current_blob_id IS NULL"
        ),
        0
    );
    assert_eq!(
        count(
            &c,
            "SELECT count(*) FROM attachment_observation WHERE blob_id IS NULL"
        ),
        0
    );
    let rows: Vec<(i64, i64)> = c
        .prepare("SELECT id, current_blob_id FROM attachment ORDER BY id")
        .expect("prepare")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    let reader = lib.open_read(&id).expect("reader");
    for (att, blob) in rows {
        let mut got = Vec::new();
        reader
            .open_blob(oma_archive::BlobId::new(blob))
            .expect("open")
            .read_to_end(&mut got)
            .expect("read");
        assert_eq!(
            got,
            common::bulk::blob_bytes(att, 4_096),
            "attachment {att}"
        );
    }
    // The default leaves the store empty, as before.
    let (tmp2, lib2) = library();
    let id2 = lib2.create(None).expect("create");
    let mut a2 = lib2.open(&id2).expect("open");
    let r2 = generate(&mut a2, &BulkSpec::new(1_000));
    assert_eq!(r2.blobs, 0);
    drop(a2);
    let c2 = read_only(&db_path(&tmp2, &id2));
    assert_eq!(count(&c2, "SELECT count(*) FROM blob"), 0);
}
