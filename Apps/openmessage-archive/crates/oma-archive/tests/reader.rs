//! `ArchiveReader` (spec §4.3, D84, §18.2).
#![allow(clippy::disallowed_methods)]

mod common;

use common::*;
use oma_archive::schema::codes::SessionState;
use oma_archive::{Recovery, SessionCounts};

#[test]
fn a_reader_sees_only_committed_state_during_a_write() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    // The import holds the lock and its writer is open.
    let mut archive = lib.open(&id).expect("open");
    archive.begin_write(Recovery::Run).expect("begin_write");
    let reader = lib.open_read(&id).expect("reader while locked");
    assert_eq!(
        reader.info().expect("info").sessions,
        SessionCounts::default()
    );

    // A write transaction in flight (standing in for an import batch).
    let c = raw(&db_path(&tmp, &id));
    c.execute_batch("BEGIN IMMEDIATE").expect("begin");
    insert_session(&c, 0);
    assert_eq!(reader.info().expect("mid-batch").sessions.total(), 0);
    assert!(reader.sessions().expect("sessions").is_empty());
    c.execute_batch("COMMIT").expect("commit");
    let info = reader.info().expect("after commit");
    assert_eq!(info.sessions.running, 1);
    let resumable = info.resumable.expect("newest session is running");
    assert_eq!(resumable.state, SessionState::Running);
    let sessions = reader.sessions().expect("sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].state, SessionState::Running);
    assert_eq!(sessions[0].ended_at_ns, None);
    assert_eq!(sessions[0].full_observation, None);
    drop(c);
    archive.close();
}

#[test]
fn stats_and_sessions_are_read_and_validated() {
    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let c = raw(&db_path(&tmp, &id));
        for state in [1, 1, 3, 5, 2] {
            insert_session(&c, state);
        }
        c.execute(
            "INSERT INTO archive_stats (id, handles, chats, messages, body_versions, \
             attachments, blobs, blob_bytes, computed_at_ns) VALUES (1, 1, 2, 3, 4, 5, 6, 7, 8)",
            [],
        )
        .expect("stats");
    }
    let reader = lib.open_read(&id).expect("reader");
    let info = reader.info().expect("info");
    let s = info.stats.expect("stats");
    assert_eq!(
        (
            s.handles,
            s.chats,
            s.messages,
            s.body_versions,
            s.attachments,
            s.blobs,
            s.blob_bytes
        ),
        (1, 2, 3, 4, 5, 6, 7)
    );
    assert_eq!(s.computed_at_ns, 8);
    assert_eq!(
        (
            info.sessions.completed,
            info.sessions.failed,
            info.sessions.superseded,
            info.sessions.cancelled
        ),
        (2, 1, 1, 1)
    );
    assert_eq!(info.sessions.total(), 5);
    // The newest session was cancelled: resumable as far as listing goes.
    assert_eq!(
        info.resumable.map(|r| (r.session, r.state)),
        Some((5, SessionState::Cancelled))
    );
    let ids: Vec<i64> = reader
        .sessions()
        .expect("sessions")
        .iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, vec![5, 4, 3, 2, 1], "newest first");
    drop(reader);

    // A negative count cannot be shown as a number.
    raw(&db_path(&tmp, &id))
        .execute("UPDATE archive_stats SET blobs = -1", [])
        .expect("tamper");
    assert!(matches!(
        lib.open_read(&id).expect("reader").info(),
        Err(oma_archive::ArchiveError::Damaged(
            oma_archive::DamageKind::Unreadable
        ))
    ));
}

/// `last_import` reads the newest session's state, counts and stop
/// reason; `resume_matches` pre-selects the backup an unfinished session
/// came from, by name, model and date, and nothing once it completed.
#[test]
fn last_import_and_resume_matches() {
    use common::import::{cancelled_at_chats, import, open_entry, opts, read_only};
    use oma_archive::{LineageChoice, SourceFacts, StopReason};

    let (tmp, lib) = library();
    let id = lib.create(None).expect("create");
    {
        let reader = lib.open_read(&id).expect("reader");
        assert_eq!(reader.last_import().expect("none"), None);
        let none = reader
            .resume_matches(&[SourceFacts {
                device_name: None,
                product_type: None,
                last_backup_secs: None,
            }])
            .expect("matches");
        assert_eq!(none, vec![false]);
    }
    let src = open_entry("imp-dev1-v1-plain");
    let mut archive = lib.open(&id).expect("open");
    cancelled_at_chats(&mut archive, &src, &opts(2), LineageChoice::New);
    archive.close();
    let db = db_path(&tmp, &id);
    let (name, model, date_ns): (String, String, i64) = read_only(&db)
        .query_row(
            "SELECT device_name, product_type, info_last_backup_ns FROM source_backup",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("source");
    let secs = date_ns.div_euclid(1_000_000_000);
    let reader = lib.open_read(&id).expect("reader");
    let last = reader.last_import().expect("read").expect("a session");
    assert_eq!(last.state, SessionState::Cancelled);
    assert!(last.counts.is_some_and(|c| c.handles.seen > 0));
    assert_eq!(last.stop_reason, None);
    fn probe<'a>(n: &'a str, m: &'a str, s: Option<i64>) -> SourceFacts<'a> {
        SourceFacts {
            device_name: Some(n),
            product_type: Some(m),
            last_backup_secs: s,
        }
    }
    let other = format!("{name} 2");
    let matches = reader
        .resume_matches(&[
            probe(&name, &model, Some(secs)),
            probe(&other, &model, Some(secs)),
            probe(&name, &model, Some(secs + 1)),
            probe(&name, &model, None),
        ])
        .expect("matches");
    assert_eq!(matches, vec![true, false, false, false]);
    drop(reader);

    // A stopped session reports its reason.
    {
        let c = raw(&db);
        c.execute("UPDATE import_session SET state = 6", [])
            .expect("stop");
        c.execute(
            "INSERT INTO import_issue (session_id, pass, kind, count) \
             SELECT id, 3, 3, 1 FROM import_session",
            [],
        )
        .expect("issue");
    }
    let reader = lib.open_read(&id).expect("reader");
    let last = reader.last_import().expect("read").expect("a session");
    assert_eq!(last.state, SessionState::Stopped);
    assert_eq!(last.stop_reason, Some(StopReason::SourceChanged));
    drop(reader);

    // Once completed, nothing is pre-selected.
    let mut archive = lib.open(&id).expect("open");
    import(&mut archive, &src, &opts(1000), LineageChoice::New);
    archive.close();
    let reader = lib.open_read(&id).expect("reader");
    let last = reader.last_import().expect("read").expect("a session");
    assert_eq!(last.state, SessionState::Completed);
    let matches = reader
        .resume_matches(&[probe(&name, &model, Some(secs))])
        .expect("matches");
    assert_eq!(matches, vec![false]);
}
