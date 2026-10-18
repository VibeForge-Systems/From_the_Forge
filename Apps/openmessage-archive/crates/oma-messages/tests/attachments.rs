//! Attachment resolution (M3 §12, §19.3 `attachments.rs`): every lookup and
//! probe outcome, the key a lookup returns accepted by `extract_file`, and
//! no attachment plaintext written by the check itself.
#![allow(clippy::disallowed_types, clippy::disallowed_methods)]
mod common;

use std::ops::ControlFlow;

use common::*;
use oma_backup::{BlobProbe, CancelToken, JobCtx, Progress, WalPolicy};
use oma_messages::{
    connect, stage, AttachmentLocator, CheckOptions, Connect, Cursor, LookupOutcome, MessagesCtx,
    MessagesProgress, NotAttemptedReason, PathClass, Probe, SmsDb, ATTACHMENT_LOOKUP_DOMAIN,
};
use rusqlite::Connection;

#[test]
fn every_outcome_of_the_encrypted_fixture() {
    let fx = Fixture::messages("msgs-attachments-enc");
    let (report, cleanup) = run_check(&fx, Some(PASSWORD), &CheckOptions::default());
    let report = report.expect("check");
    assert!(cleanup.removed);
    let a = report.attachments.expect("attachments");
    assert_eq!(a.lookup_domain, ATTACHMENT_LOOKUP_DOMAIN);
    assert_eq!(a.rows, Some(18));
    let l = &a.lookup;
    assert_eq!(
        (
            l.found,
            l.not_listed_at_sourced_key,
            l.not_found_at_assumed_key
        ),
        (7, 1, 1)
    );
    for (reason, n) in [
        (NotAttemptedReason::Null, 1),
        (NotAttemptedReason::TooLong, 1),
        (NotAttemptedReason::UnusualName, 1),
        (NotAttemptedReason::ClassNotLookedUp, 4),
        (NotAttemptedReason::Unrecognized, 2),
    ] {
        assert_eq!(a.not_attempted(reason), n, "{reason:?}");
    }
    let p = &a.probes;
    assert_eq!(p.present, 2);
    assert_eq!(p.missing, 1);
    assert_eq!(p.not_a_file, 1);
    assert_eq!(p.no_key, 1);
    assert_eq!(p.class_key_unavailable, 1);
    assert_eq!(p.key_does_not_unwrap, 1);
    // Ciphertext lengths: two 26-byte files, padded to 32 bytes each.
    assert_eq!(a.present_blob_bytes, 64);
    assert_eq!(a.class(PathClass::SmsAttachments), 8);
    assert_eq!(a.class(PathClass::SmsStickerCache), 2);
    let f = report.failures.expect("failures");
    assert_eq!(f.attachment_key_problem, 3);
    assert_eq!(f.attachment_blob_missing, 1);
}

/// A key from a lookup is accepted by `extract_file`, which writes the
/// plaintext into `x/`; the check's own pass writes nothing there.
#[test]
fn a_located_key_extracts_and_the_pass_writes_no_attachment() {
    let fx = Fixture::messages("msgs-attachments-enc");
    let ob = fx.open(Some(PASSWORD));
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
    let mut locator = AttachmentLocator::new(&ob, true);
    let mut keys = Vec::new();
    db.for_each_attachment(Cursor::Start, Some(&mut locator), &mut ctx, |a| {
        if let Some(loc) = a.location {
            if let LookupOutcome::Found {
                key,
                blob: Some(BlobProbe::Present { .. }),
            } = loc.lookup
            {
                keys.push((a.rowid, key));
            }
        }
        ControlFlow::<()>::Continue(())
    })
    .expect("pass");
    drop(db);
    drop(conn);
    staged.remove().expect("remove");
    let x = ob.staging_path().expect("staging").join("x");
    assert!(!x.exists(), "the pass wrote into x/");

    assert_eq!(keys.len(), 2);
    let (rowid, key) = &keys[0];
    assert_eq!(*rowid, 1);
    assert!(!format!("{key:?}").contains("SYNTH"), "{key:?}");
    let mut psink = |_: Progress| {};
    let mut job = JobCtx::new(&token, &mut psink);
    let file = ob.extract_file(key, &mut job).expect("extract");
    let plain = std::fs::read(&file.path).expect("read");
    assert_eq!(plain, b"synthetic attachment 0001\n");
    file.remove().expect("remove");
    assert!(ob.close().removed);
}

/// `Ambiguous` and `InvalidEntry` are outcomes, not errors. A plain copy's
/// Manifest.db is edited: the `Files` table loses its primary key and gains
/// a duplicate of one attachment's row, and another row's `relativePath` no
/// longer matches its fileID.
#[test]
fn ambiguous_and_invalid_entries_are_counted() {
    let fx = Fixture::messages("msgs-full-plain");
    let c = Connection::open(fx.dir.join("Manifest.db")).expect("manifest");
    c.execute_batch(
        "ALTER TABLE Files RENAME TO Files_old;
         CREATE TABLE Files (fileID TEXT, domain TEXT, relativePath TEXT, flags INTEGER, file BLOB);
         INSERT INTO Files SELECT * FROM Files_old;
         DROP TABLE Files_old;
         INSERT INTO Files SELECT * FROM Files WHERE relativePath LIKE '%/SYNTH-0001/%';
         UPDATE Files SET relativePath = 'Library/SMS/StickerCache/elsewhere'
             WHERE relativePath LIKE '%StickerCache/00/SYNTH-0002/%';",
    )
    .expect("edit");
    drop(c);
    let (report, cleanup) = run_check(&fx, None, &CheckOptions::default());
    let report = report.expect("check");
    assert!(cleanup.removed);
    let a = report.attachments.expect("attachments");
    assert_eq!(a.lookup.ambiguous, 1);
    assert_eq!(a.lookup.invalid_entry, 1);
    assert_eq!(a.lookup.found, 0);
    let f = report.failures.expect("failures");
    assert_eq!((f.attachment_ambiguous, f.attachment_invalid_entry), (1, 1));
}

/// Without probing, a listed file is found but not probed.
#[test]
fn probing_can_be_turned_off() {
    let fx = Fixture::messages("msgs-full-plain");
    let opts = CheckOptions::default().with_probe_attachments(false);
    let (report, _) = run_check(&fx, None, &opts);
    let a = report.expect("check").attachments.expect("attachments");
    assert_eq!(a.lookup.found, 2);
    assert_eq!(a.probes.not_probed, 2);
    assert_eq!(a.probes.present, 0);
    assert_eq!(a.present_blob_bytes, 0);
}
