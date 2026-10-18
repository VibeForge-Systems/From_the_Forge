//! `perf_synthetic_large` (M3 §19.7): measured, not claimed.
//!
//! Ignored by default; run it by hand, in release mode, on each platform:
//!
//! ```sh
//! cargo test -p oma-messages --release --test perf -- --ignored perf_synthetic_large --nocapture
//! ```
//!
//! It builds a synthetic sms.db with 1,000,000 messages, 2,000 chats and
//! 50,000 attachments (none listed in the backup, so every lookup is a
//! miss), once without indexes and once with indexes of our own on the join
//! keys, runs `check()` on each, and prints the time of each phase and the
//! peak resident set (`VmHWM`, Linux only). The results are recorded in
//! `docs/compatibility.md` as **synthetic**; real-backup timing stays
//! UNVERIFIED.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use std::time::Instant;

use common::*;
use oma_backup::CancelToken;
use oma_messages::{check, CheckOptions, MessagesCtx, MessagesPhase, MessagesProgress};

fn database(indexes: bool) -> Vec<u8> {
    let mut sql = full_schema();
    sql.push_str(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000000)
         INSERT INTO message (ROWID, guid, date, is_from_me, handle_id, text, service)
             SELECT i, 'SYNTH-MSG-GUID-' || i, 700000000000000000 + i, i % 2, 1 + i % 500,
                    'SYNTH-BODY-' || i, 'iMessage' FROM n;
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000000)
         INSERT INTO chat_message_join (chat_id, message_id, message_date)
             SELECT 1 + i % 2000, i, 0 FROM n;
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000)
         INSERT INTO chat (ROWID, guid) SELECT i, 'SYNTH-CHAT-GUID-' || i FROM n;
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 500)
         INSERT INTO handle (ROWID, id) SELECT i, 'synthetic-handle-' || i || '@example.invalid' FROM n;
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000)
         INSERT INTO chat_handle_join (chat_id, handle_id) SELECT i, 1 + i % 500 FROM n;
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 50000)
         INSERT INTO attachment (ROWID, guid, filename)
             SELECT i, 'SYNTH-ATT-GUID-' || i,
                    '~/Library/SMS/Attachments/aa/' || i || '/SYNTH-FILE.heic' FROM n;
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 50000)
         INSERT INTO message_attachment_join (message_id, attachment_id) SELECT i * 20, i FROM n;",
    );
    if indexes {
        sql.push_str(
            "CREATE INDEX synth_cmj_message ON chat_message_join (message_id);
             CREATE INDEX synth_cmj_chat ON chat_message_join (chat_id);
             CREATE INDEX synth_chj_chat ON chat_handle_join (chat_id);
             CREATE INDEX synth_maj_attachment ON message_attachment_join (attachment_id);
             CREATE INDEX synth_message_handle ON message (handle_id);",
        );
    }
    build(&sql)
}

fn vm_hwm() -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM"))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "VmHWM: not available on this platform".to_owned())
}

#[test]
#[ignore = "manual: builds a 1,000,000-message database"]
fn perf_synthetic_large() {
    for indexes in [false, true] {
        let main = database(indexes);
        let fx = Fixture::with_sms(&main);
        let ob = fx.open(None);
        let token = CancelToken::new();
        let mut phases: Vec<(MessagesPhase, Instant)> = Vec::new();
        let mut sink = |p: MessagesProgress| {
            if phases.last().map(|x| x.0) != Some(p.phase) {
                phases.push((p.phase, Instant::now()));
            }
        };
        let start = Instant::now();
        let report = {
            let mut ctx = MessagesCtx::new(&token, &mut sink);
            check(&ob, &CheckOptions::default(), &mut ctx).expect("check")
        };
        let end = Instant::now();
        assert!(ob.close().removed);
        println!(
            "indexes={indexes} bytes={} status={} qualifiers={:?} messages={} total_ms={}",
            main.len(),
            report.status_kind(),
            report.qualifiers(),
            report.messages.as_ref().map_or(0, |m| m.rows),
            end.duration_since(start).as_millis()
        );
        for (i, (phase, at)) in phases.iter().enumerate() {
            let until = phases.get(i + 1).map_or(end, |x| x.1);
            println!("  {phase:?}: {} ms", until.duration_since(*at).as_millis());
        }
        println!("  {}", vm_hwm());
    }
}
