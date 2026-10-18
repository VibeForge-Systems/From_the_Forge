//! Deterministic cancellation per phase of this slice (M3 §7.2, §19.3
//! `cancel.rs`). The sink cancels at the first report of a phase; the
//! explicit checks at phase boundaries, between pages and every 128 rows
//! make the outcome independent of timing. Afterwards the backup closes with
//! an empty staging session and no `.partial` files.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use std::ops::ControlFlow;

use common::*;
use oma_backup::{CancelToken, OpenBackup, WalPolicy};
use oma_messages::{
    connect, stage, Connect, Cursor, MessagesCtx, MessagesError, MessagesPhase, MessagesProgress,
    Probe, ProbeOptions, SmsDb,
};

const PHASES: [MessagesPhase; 8] = [
    MessagesPhase::StagingMessagesDb,
    MessagesPhase::ScanningSchema,
    MessagesPhase::CheckingIntegrity,
    MessagesPhase::ReadingSchema,
    MessagesPhase::ReadingHandles,
    MessagesPhase::ReadingChats,
    MessagesPhase::ReadingChatLinks,
    MessagesPhase::CheckingAttachments,
];

fn sms() -> Vec<u8> {
    let mut sql = full_schema();
    sql.push_str(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 300)
         INSERT INTO handle (id) SELECT 'synthetic-handle-' || i || '@example.invalid' FROM n;
         INSERT INTO chat (guid) SELECT 'SYNTH-CHAT-GUID-' || ROWID FROM handle;
         INSERT INTO chat_handle_join SELECT ROWID, ROWID FROM handle;
         INSERT INTO chat_message_join SELECT ROWID, ROWID, 0 FROM handle;
         INSERT INTO message_attachment_join SELECT ROWID, ROWID FROM handle;",
    );
    build(&sql)
}

fn run_all(ob: &OpenBackup, ctx: &mut MessagesCtx<'_>) -> Result<(), MessagesError> {
    let staged = stage(ob, WalPolicy::BestEffort, ctx)?;
    let Connect::Ready(conn) = connect(&staged, ctx)? else {
        panic!("schema exceeds limits");
    };
    let opts = ProbeOptions::default().with_page_rows(16);
    let Probe::Supported(db) = SmsDb::open_with(&conn, &opts, ctx)? else {
        panic!("not supported");
    };
    db.for_each_handle(Cursor::Start, ctx, go::<_>)?;
    db.for_each_chat(Cursor::Start, ctx, go::<_>)?;
    db.for_each_chat_handle(Cursor::Start, ctx, go::<_>)?;
    db.for_each_chat_message(Cursor::Start, ctx, go::<_>)?;
    db.for_each_message_attachment(Cursor::Start, ctx, go::<_>)?;
    db.for_each_recoverable(Cursor::Start, ctx, go::<_>)?;
    drop(db);
    drop(conn);
    staged.remove().expect("remove");
    Ok(())
}

fn assert_clean(fx: &Fixture, ob: OpenBackup) {
    let cleanup = ob.close();
    assert!(cleanup.removed);
    let left = fx.staged_files();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn every_phase_is_reported_without_cancellation() {
    let fx = Fixture::with_sms(&sms());
    let ob = fx.open(None);
    let token = CancelToken::new();
    let mut seen = Vec::new();
    {
        let mut sink = |p: MessagesProgress| {
            if !seen.contains(&p.phase) {
                seen.push(p.phase);
            }
        };
        let mut ctx = MessagesCtx::new(&token, &mut sink);
        run_all(&ob, &mut ctx).expect("check");
    }
    assert_eq!(seen, PHASES);
    assert_clean(&fx, ob);
}

#[test]
fn cancelling_at_each_phase_stops_the_check() {
    let main = sms();
    for phase in PHASES {
        let fx = Fixture::with_sms(&main);
        let ob = fx.open(None);
        let token = CancelToken::new();
        let trigger = token.clone();
        let mut sink = move |p: MessagesProgress| {
            if p.phase == phase {
                trigger.cancel();
            }
        };
        let mut ctx = MessagesCtx::new(&token, &mut sink);
        let result = run_all(&ob, &mut ctx);
        assert!(
            matches!(result, Err(MessagesError::Cancelled)),
            "{phase:?}: {result:?}"
        );
        assert_clean(&fx, ob);
    }
}

fn go<T>(_: T) -> ControlFlow<()> {
    ControlFlow::Continue(())
}

/// Every phase of `check()` itself (§2 step 5), on the catalogue's
/// `msgs-full-plain`: the sink cancels at the first report of the phase,
/// the check returns `Cancelled` within the latency budget (§7.2: 2 s), and
/// the backup closes with an empty staging session.
#[test]
fn check_cancels_at_every_phase_within_the_latency_budget() {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    const ALL: [MessagesPhase; 10] = [
        MessagesPhase::StagingMessagesDb,
        MessagesPhase::ScanningSchema,
        MessagesPhase::CheckingIntegrity,
        MessagesPhase::ReadingSchema,
        MessagesPhase::ReadingHandles,
        MessagesPhase::ReadingChats,
        MessagesPhase::ReadingChatLinks,
        MessagesPhase::ReadingMessages,
        MessagesPhase::CheckingAttachments,
        MessagesPhase::Summarising,
    ];
    let opts = oma_messages::CheckOptions::default().with_page_rows(16);
    for phase in ALL {
        let fx = Fixture::messages("msgs-full-plain");
        let ob = fx.open(None);
        let token = CancelToken::new();
        let trigger = token.clone();
        let at: Arc<Mutex<Option<Instant>>> = Arc::default();
        let at_sink = Arc::clone(&at);
        let mut sink = move |p: MessagesProgress| {
            if p.phase == phase && !trigger.is_cancelled() {
                *at_sink.lock().expect("lock") = Some(Instant::now());
                trigger.cancel();
            }
        };
        let mut ctx = MessagesCtx::new(&token, &mut sink);
        let result = oma_messages::check(&ob, &opts, &mut ctx);
        let latency = at
            .lock()
            .expect("lock")
            .expect("the phase was reported")
            .elapsed();
        assert!(
            matches!(result, Err(MessagesError::Cancelled)),
            "{phase:?}: {result:?}"
        );
        assert!(latency < Duration::from_secs(2), "{phase:?}: {latency:?}");
        assert_clean(&fx, ob);
    }
}

/// The interrupt path (§7.2): a long aggregate on an index-free 200,000-row
/// database is stopped by a cancel from another thread, inside the
/// statement (the progress handler and the interrupt watcher), within the
/// latency budget.
#[test]
fn a_long_aggregate_is_interrupted_from_another_thread() {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let mut sql = full_schema();
    sql.push_str(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200000)
         INSERT INTO message (ROWID, guid, date, is_from_me, handle_id, other_handle)
             SELECT i, 'SYNTH-MSG-GUID-' || i, 0, 0, i, i FROM n;
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200000)
         INSERT INTO chat_message_join (chat_id, message_id) SELECT i % 7, i + 3 FROM n;
         INSERT INTO chat (guid) VALUES ('SYNTH-CHAT-GUID-1');",
    );
    let fx = Fixture::with_sms(&build(&sql));
    let ob = fx.open(None);
    let token = CancelToken::new();
    let trigger = token.clone();
    let at: Arc<Mutex<Option<Instant>>> = Arc::default();
    let at_thread = Arc::clone(&at);
    let mut started = false;
    let mut sink = move |p: MessagesProgress| {
        if p.phase == MessagesPhase::Summarising && !started {
            started = true;
            let trigger = trigger.clone();
            let at = Arc::clone(&at_thread);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(5));
                *at.lock().expect("lock") = Some(Instant::now());
                trigger.cancel();
            });
        }
    };
    let mut ctx = MessagesCtx::new(&token, &mut sink);
    let result = oma_messages::check(&ob, &oma_messages::CheckOptions::default(), &mut ctx);
    let latency = at.lock().expect("lock").expect("cancelled").elapsed();
    assert!(
        matches!(result, Err(MessagesError::Cancelled)),
        "{result:?}"
    );
    assert!(latency < Duration::from_secs(2), "{latency:?}");
    assert_clean(&fx, ob);
}
