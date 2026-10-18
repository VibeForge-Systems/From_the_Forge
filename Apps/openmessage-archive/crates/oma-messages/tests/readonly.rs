//! I1 for the extraction check (M3 §7.5, §19.3 `readonly.rs`): the backup
//! is byte-for-byte unchanged after a check, and the staged connection ends
//! query-only, with no authorizer denial and no row change.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use common::*;
use oma_backup::{CancelToken, WalPolicy};
use oma_messages::{
    connect, stage, AttachmentLocator, CheckOptions, Connect, Cursor, GuidSets, MessageReadOptions,
    MessagesCtx, MessagesProgress, Probe, SmsDb,
};
use sha2::{Digest, Sha256};

type Snapshot = BTreeMap<PathBuf, (u64, Option<SystemTime>, Vec<u8>)>;

fn snapshot(dir: &Path) -> Snapshot {
    let mut files = Vec::new();
    walk(dir, &mut files);
    files
        .into_iter()
        .map(|p| {
            let meta = std::fs::metadata(&p).expect("metadata");
            let digest = Sha256::digest(std::fs::read(&p).expect("read")).to_vec();
            (p, (meta.len(), meta.modified().ok(), digest))
        })
        .collect()
}

#[test]
fn the_backup_is_unchanged_by_a_check() {
    for name in [
        "msgs-full-plain",
        "msgs-full-enc",
        "msgs-wal-plain",
        "msgs-wal-enc",
        "msgs-attachments-enc",
        "msgs-hostile-plain",
        "msgs-corrupt-plain",
    ] {
        let fx = Fixture::messages(name);
        let before = snapshot(&fx.dir);
        assert!(!before.is_empty());
        let (report, cleanup) = run_check(&fx, catalogue_password(name), &CheckOptions::default());
        report.unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert!(cleanup.removed, "{name}");
        assert_eq!(snapshot(&fx.dir), before, "{name}: the backup changed");
    }
}

fn go<T>(_: T) -> ControlFlow<()> {
    ControlFlow::Continue(())
}

/// After every pass and the aggregates: `query_only` is 1, the authorizer
/// denied nothing, and no row changed.
#[test]
fn the_connection_ends_unwritten() {
    let fx = Fixture::messages("msgs-full-plain");
    let ob = fx.open(None);
    let token = CancelToken::new();
    let mut sink = |_: MessagesProgress| {};
    let mut ctx = MessagesCtx::new(&token, &mut sink);
    let staged = stage(&ob, WalPolicy::BestEffort, &mut ctx).expect("stage");
    let Connect::Ready(conn) = connect(&staged, &ctx).expect("connect") else {
        panic!("limits");
    };
    let Probe::Supported(db) = SmsDb::open(&conn, &mut ctx).expect("probe") else {
        panic!("not supported");
    };
    db.for_each_handle(Cursor::Start, &mut ctx, go)
        .expect("handles");
    db.for_each_chat(Cursor::Start, &mut ctx, go)
        .expect("chats");
    db.for_each_chat_handle(Cursor::Start, &mut ctx, go)
        .expect("links");
    db.for_each_chat_message(Cursor::Start, &mut ctx, go)
        .expect("links");
    db.for_each_recoverable(Cursor::Start, &mut ctx, go)
        .expect("deleted");
    let mut guids = GuidSets::new();
    db.for_each_message(
        Cursor::Start,
        &MessageReadOptions::default(),
        Some(&mut guids),
        &mut ctx,
        go,
    )
    .expect("messages");
    db.for_each_message_attachment(Cursor::Start, &mut ctx, go)
        .expect("links");
    let mut locator = AttachmentLocator::new(&ob, true);
    db.for_each_attachment(Cursor::Start, Some(&mut locator), &mut ctx, go)
        .expect("attachments");
    let agg = db.aggregates(&mut guids, &mut ctx).expect("aggregates");
    assert_eq!(agg.over_budget, 0);
    assert!(agg.guids_complete);

    let c = db.connection();
    assert!(c.query_only().expect("query_only"));
    assert_eq!(c.authorizer_denials(), 0);
    assert_eq!(c.total_changes(), 0);
    drop(db);
    drop(conn);
    staged.remove().expect("remove");
    assert!(ob.close().removed);
}
