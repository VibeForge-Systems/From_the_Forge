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
