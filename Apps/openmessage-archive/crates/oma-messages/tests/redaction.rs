//! `Debug` of every public type prints presence, lengths and kinds only
//! (M3 §14 item 7, §19.3 `redaction.rs`): records from every pass over the
//! catalogue's richest fixtures, attachment locations and keys, the
//! capabilities and the report. The report's `Debug` also withholds the
//! date range (D31).
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use std::ops::ControlFlow;

use common::*;
use oma_backup::{CancelToken, WalPolicy};
use oma_messages::{
    connect, stage, AttachmentLocator, CheckOptions, Connect, Cursor, GuidSets, MessageReadOptions,
    MessagesCtx, MessagesProgress, Probe, SmsDb,
};

fn debug_of_every_record(name: &str, password: Option<&str>) -> Vec<String> {
    let fx = Fixture::messages(name);
    let ob = fx.open(password);
    let token = CancelToken::new();
    let mut sink = |_: MessagesProgress| {};
    let mut ctx = MessagesCtx::new(&token, &mut sink);
    let staged = stage(&ob, WalPolicy::BestEffort, &mut ctx).expect("stage");
    let mut out = vec![
        format!("{staged:?}"),
        format!("{:?}", staged.content_digest()),
    ];
    let Connect::Ready(conn) = connect(&staged, &ctx).expect("connect") else {
        panic!("limits");
    };
    let Probe::Supported(db) = SmsDb::open(&conn, &mut ctx).expect("probe") else {
        panic!("not supported");
    };
    out.push(format!("{db:?}"));
    let mut push = |s: String| {
        out.push(s);
        ControlFlow::<()>::Continue(())
    };
    db.for_each_handle(Cursor::Start, &mut ctx, |r| push(format!("{r:?}")))
        .expect("handles");
    db.for_each_chat(Cursor::Start, &mut ctx, |r| push(format!("{r:?}")))
        .expect("chats");
    db.for_each_chat_handle(Cursor::Start, &mut ctx, |r| push(format!("{r:?}")))
        .expect("links");
    db.for_each_chat_message(Cursor::Start, &mut ctx, |r| push(format!("{r:?}")))
        .expect("links");
    db.for_each_recoverable(Cursor::Start, &mut ctx, |r| push(format!("{r:?}")))
        .expect("deleted");
    let mut guids = GuidSets::new();
    let opts = MessageReadOptions::default().with_keep_raw_blobs(true);
    db.for_each_message(Cursor::Start, &opts, Some(&mut guids), &mut ctx, |r| {
        let extra = r
            .association
            .as_ref()
            .map(|a| format!("{a:?} {:?}", a.target))
            .unwrap_or_default();
        push(format!("{r:?} {:?} {:?} {extra}", r.body, r.summary))
    })
    .expect("messages");
    db.for_each_message_attachment(Cursor::Start, &mut ctx, |r| push(format!("{r:?}")))
        .expect("links");
    let mut locator = AttachmentLocator::new(&ob, true);
    db.for_each_attachment(Cursor::Start, Some(&mut locator), &mut ctx, |r| {
        push(format!("{r:?} {:?}", r.location))
    })
    .expect("attachments");
    out.push(format!("{locator:?} {guids:?}"));
    out.push(format!(
        "{:?}",
        db.aggregates(&mut guids, &mut ctx).expect("aggregates")
    ));
    drop(db);
    drop(conn);
    staged.remove().expect("remove");
    assert!(ob.close().removed);
    out
}

#[test]
fn no_debug_output_holds_a_sentinel() {
    let mut all = Vec::new();
    for (name, password) in [
        ("msgs-full-plain", None),
        ("msgs-full-enc", Some(PASSWORD)),
        ("msgs-hostile-plain", None),
        ("msgs-attachments-enc", Some(PASSWORD)),
    ] {
        all.extend(debug_of_every_record(name, password));
        let fx = Fixture::messages(name);
        let (report, _) = run_check(&fx, password, &CheckOptions::default());
        let report = report.expect("check");
        let shown = format!("{report:?}");
        let dates = report.dates.as_ref().expect("dates");
        for v in [dates.earliest_message, dates.latest_message]
            .into_iter()
            .flatten()
        {
            assert!(
                !shown.contains(&v.to_string()),
                "{name}: the date range is in Debug"
            );
        }
        all.push(shown);
    }
    assert!(all.len() > 100, "{}", all.len());
    // The detector works, and the fixtures do hold the sentinels.
    assert_eq!(sentinels_in("x SYNTH-BODY-1 y"), ["SYNTH-BODY"]);
    let raw = std::fs::read(messages_dir().join("msgs-full-plain.expected/sms.db")).expect("db");
    let raw = String::from_utf8_lossy(&raw);
    assert!(sentinels_in(&raw).len() >= 10, "{:?}", sentinels_in(&raw));
    for s in &all {
        let found = sentinels_in(s);
        assert!(found.is_empty(), "{found:?} in {s}");
    }
}
