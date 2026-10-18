//! P9, finalize (spec §8.9). Runs on the archive only, after every pass is
//! done and the staged sms.db is removed. Each step is its own
//! transaction, the space guard runs before each, and a cancel is honoured
//! between steps:
//!
//! 1. recompute `chat_stats` (one statement, with the archive interrupt
//!    armed);
//! 2. recompute `archive_stats`;
//! 3. delete this session's `import_rowid_map`, `import_occurrence` and
//!    `import_dup_key` rows, in chunks of [`CLEANUP_CHUNK`];
//! 4. record the terminal state: completed, `ended_at_ns`,
//!    `full_observation` and the final counts. This is **best effort**: if
//!    it fails, the session stays running, becomes interrupted at the next
//!    write open, and is continued by the next import of the same content
//!    (its passes are already done);
//! 5. FTS merge steps, `merge = 500`, one statement per transaction with
//!    the interrupt armed, until `total_changes()` grows by less than 2
//!    (fts5.html);
//! 6. `PRAGMA wal_checkpoint(TRUNCATE)`.
//!
//! A cancel, a stop or an error before step 4 leaves the session to the
//! caller, which records it cancelled, stopped or failed exactly as after
//! a pass. After step 4 the session is completed whatever follows: a cancel
//! only skips the remaining merge steps, and a failed merge step or
//! checkpoint is reported, never an error.

use rusqlite::Connection;

use super::batch::{PassError, Run};
use super::counts::ImportCounts;
use super::StopReason;
use crate::archive::CheckpointOutcome;
use crate::conn::armed;
use crate::error::primary_code;
use crate::fault::{point, FaultPoint};
use crate::progress::{ArchiveCtx, ArchiveEvent, ImportPhase};
use crate::schema::codes::{QualifierBit, SessionState};

/// Rows deleted per cleanup transaction (spec §8.9 step 3).
pub(crate) const CLEANUP_CHUNK: i64 = 50_000;
/// `merge` pages per FTS merge step.
const MERGE_PAGES: i64 = 500;
/// The most FTS merge steps one finalize runs (a bound on a hostile or
/// damaged index; a normal index needs far fewer).
const MERGE_STEPS_MAX: u32 = 10_000;

/// The finalize `GROUP BY`: one `chat_stats` row per chat (spec §18.2
/// pins its plan). A message is dated when its instant was derived.
pub(crate) const CHAT_STATS_SQL: &str = "INSERT INTO chat_stats \
         (chat_id, message_count, undated_count, first_sort_ns, last_sort_ns) \
     SELECT c.id, count(m.id), coalesce(sum(m.id IS NOT NULL AND m.date_utc_ns IS NULL), 0), \
            min(m.date_utc_ns), max(m.date_utc_ns) \
     FROM chat c \
     LEFT JOIN chat_message cm ON cm.chat_id = c.id \
     LEFT JOIN message m ON m.id = cm.message_id \
     GROUP BY c.id";

/// `archive_stats`, the derived totals (one row).
const ARCHIVE_STATS_SQL: &str = "INSERT INTO archive_stats \
         (id, handles, chats, messages, body_versions, attachments, blobs, blob_bytes, \
          computed_at_ns) \
     VALUES (1, (SELECT count(*) FROM handle), (SELECT count(*) FROM chat), \
             (SELECT count(*) FROM message), (SELECT count(*) FROM message_body), \
             (SELECT count(*) FROM attachment), (SELECT count(*) FROM blob), \
             (SELECT coalesce(sum(byte_len), 0) FROM blob), ?1) \
     ON CONFLICT (id) DO UPDATE SET handles = excluded.handles, chats = excluded.chats, \
         messages = excluded.messages, body_versions = excluded.body_versions, \
         attachments = excluded.attachments, blobs = excluded.blobs, \
         blob_bytes = excluded.blob_bytes, computed_at_ns = excluded.computed_at_ns";

/// The session-local tables and the key columns of their chunked delete
/// (all three are `WITHOUT ROWID`).
const CLEANUP: [(&str, &str); 3] = [
    ("import_rowid_map", "src, src_rowid"),
    ("import_occurrence", "entity, fp"),
    ("import_dup_key", "entity, key_digest"),
];

/// What finalize did after the session completed. Counts and kinds only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct FinalizeReport {
    /// Session-local rows deleted (step 3).
    pub session_rows_removed: u64,
    /// FTS merge steps committed (step 5).
    pub fts_merge_steps: u32,
    /// The merge ran until the index reported no more work; `false` when
    /// a cancel, an error or the step bound ended it first.
    pub fts_merge_complete: bool,
    /// The step-6 checkpoint.
    pub checkpoint: CheckpointOutcome,
}

/// How finalize ended before the session completed. The caller records
/// the session's state.
pub(crate) enum NotCompleted {
    /// Cancelled between steps (or a step's armed statement was
    /// interrupted and rolled back).
    Cancelled,
    /// A step failed: classified like a pass error.
    Failed(PassError),
    /// Step 4 itself failed. The session is still running (state 0) and
    /// must be left so: the next write open marks it interrupted and it
    /// stays resumable (spec §8.9 step 4).
    TerminalFailed(PassError),
}

const SQLITE_INTERRUPT: i32 = 9;

fn is_interrupt(e: &rusqlite::Error) -> bool {
    primary_code(e) == Some(SQLITE_INTERRUPT)
}

/// Run `f` in one `BEGIN IMMEDIATE` transaction; roll back on any error.
fn in_tx<T>(
    conn: &Connection,
    f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let out = f(conn).and_then(|v| conn.execute_batch("COMMIT").map(|()| v));
    if out.is_err() && !conn.is_autocommit() {
        let _ = conn.execute_batch("ROLLBACK");
    }
    out
}

/// `full_observation` (spec §8.7): a completed session whose M3 status
/// carries no WAL-related qualifier.
pub(crate) fn is_full_observation(qualifiers: i64) -> bool {
    QualifierBit::WAL_RELATED
        .iter()
        .all(|q| qualifiers & q.mask() == 0)
}

/// Steps 1–3: the statistics and the session-local cleanup.
fn prepare(conn: &Connection, run: &Run, ctx: &mut ArchiveCtx<'_>) -> Result<u64, NotCompleted> {
    let cancelled = |ctx: &ArchiveCtx<'_>| ctx.cancel.is_cancelled();
    let fail = |e: rusqlite::Error| {
        if is_interrupt(&e) {
            NotCompleted::Cancelled
        } else {
            NotCompleted::Failed(PassError::Batch(e))
        }
    };
    if cancelled(ctx) {
        return Err(NotCompleted::Cancelled);
    }
    // 1. chat_stats, with the interrupt armed around its one statement.
    run.guard(conn).map_err(NotCompleted::Failed)?;
    let cancel = ctx.cancel;
    in_tx(conn, |c| {
        c.execute("DELETE FROM chat_stats", [])?;
        armed(c, cancel, |c| c.execute(CHAT_STATS_SQL, []))?;
        Ok(())
    })
    .map_err(fail)?;
    if cancelled(ctx) {
        return Err(NotCompleted::Cancelled);
    }
    // 2. archive_stats.
    run.guard(conn).map_err(NotCompleted::Failed)?;
    let now = ctx.clock.now_ns();
    in_tx(conn, |c| c.execute(ARCHIVE_STATS_SQL, [now]).map(|_| ())).map_err(fail)?;
    point(FaultPoint::FinalizeStats);
    // 3. The session-local rows, in chunks.
    let mut removed = 0u64;
    for (table, key) in CLEANUP {
        loop {
            if cancelled(ctx) {
                return Err(NotCompleted::Cancelled);
            }
            run.guard(conn).map_err(NotCompleted::Failed)?;
            let sql = format!(
                "DELETE FROM {table} WHERE session_id = ?1 AND ({key}) IN \
                 (SELECT {key} FROM {table} WHERE session_id = ?1 LIMIT ?2)"
            );
            let n = in_tx(conn, |c| {
                c.execute(&sql, rusqlite::params![run.session, CLEANUP_CHUNK])
            })
            .map_err(fail)?;
            point(FaultPoint::FinalizeCleanup);
            removed = removed.saturating_add(u64::try_from(n).unwrap_or(0));
            if i64::try_from(n).unwrap_or(i64::MAX) < CLEANUP_CHUNK {
                break;
            }
        }
    }
    Ok(removed)
}

/// Step 4: the terminal state.
fn complete(
    conn: &Connection,
    run: &Run,
    counts: &ImportCounts,
    qualifiers: i64,
    now_ns: i64,
) -> Result<(), PassError> {
    run.guard(conn)?;
    let full = is_full_observation(qualifiers);
    in_tx(conn, |c| {
        let n = c.execute(
            "UPDATE import_session SET state = ?1, ended_at_ns = ?2, full_observation = ?3, \
                 counts = ?4 WHERE id = ?5 AND state = ?6",
            rusqlite::params![
                SessionState::Completed.code(),
                now_ns,
                i64::from(full),
                counts.to_json(),
                run.session,
                SessionState::Running.code()
            ],
        )?;
        if let Some(e) = crate::fault::terminal_error() {
            return Err(e);
        }
        if n != 1 {
            // Someone else ended the session: never complete it twice.
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    })
    .map_err(PassError::Batch)?;
    point(FaultPoint::TerminalCommitted);
    Ok(())
}

/// Steps 5 and 6, after the session completed: never an error.
fn optimize(
    conn: &Connection,
    run: &Run,
    ctx: &mut ArchiveCtx<'_>,
) -> (u32, bool, CheckpointOutcome) {
    let mut steps = 0u32;
    let mut complete = false;
    ctx.progress.event(ArchiveEvent::Import {
        phase: ImportPhase::OptimizingIndex,
        done: 0,
        total: None,
    });
    while steps < MERGE_STEPS_MAX {
        if ctx.cancel.is_cancelled() || run.guard(conn).is_err() {
            break;
        }
        let before = conn.total_changes();
        let cancel = ctx.cancel;
        let step = in_tx(conn, |c| {
            armed(c, cancel, |c| {
                c.execute(
                    "INSERT INTO message_fts(message_fts, rank) VALUES('merge', ?1)",
                    [MERGE_PAGES],
                )
            })
        });
        match step {
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(
                    code = ?primary_code(&e),
                    "an FTS merge step did not complete; the index stays valid"
                );
                break;
            }
        }
        steps = steps.saturating_add(1);
        point(FaultPoint::FtsMergeStep);
        ctx.progress.event(ArchiveEvent::Import {
            phase: ImportPhase::OptimizingIndex,
            done: u64::from(steps),
            total: None,
        });
        if conn.total_changes().saturating_sub(before) < 2 {
            complete = true;
            break;
        }
    }
    ctx.progress.event(ArchiveEvent::Import {
        phase: ImportPhase::Checkpointing,
        done: 0,
        total: None,
    });
    point(FaultPoint::Checkpoint);
    let checkpoint = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_or_else(
            |e| CheckpointOutcome::Failed(primary_code(&e)),
            |busy| {
                if busy == 0 {
                    CheckpointOutcome::Done
                } else {
                    CheckpointOutcome::Busy
                }
            },
        );
    tracing::debug!(?checkpoint, steps, complete, "finalize optimized the index");
    (steps, complete, checkpoint)
}

/// Run finalize for `run`'s session, whose passes are all done.
pub(crate) fn finalize(
    conn: &Connection,
    run: &Run,
    counts: &ImportCounts,
    qualifiers: i64,
    ctx: &mut ArchiveCtx<'_>,
) -> Result<FinalizeReport, NotCompleted> {
    ctx.progress.event(ArchiveEvent::Import {
        phase: ImportPhase::Finalizing,
        done: 0,
        total: None,
    });
    let removed = prepare(conn, run, ctx)?;
    if ctx.cancel.is_cancelled() {
        return Err(NotCompleted::Cancelled);
    }
    let now = ctx.clock.now_ns();
    complete(conn, run, counts, qualifiers, now).map_err(NotCompleted::TerminalFailed)?;
    let (fts_merge_steps, fts_merge_complete, checkpoint) = optimize(conn, run, ctx);
    Ok(FinalizeReport {
        session_rows_removed: removed,
        fts_merge_steps,
        fts_merge_complete,
        checkpoint,
    })
}

/// The stop a failed terminal bookkeeping means, if any: `SQLITE_FULL` and
/// a space shortfall are a resumable stop; anything else is an error.
pub(crate) fn terminal_stop(e: &PassError) -> Option<StopReason> {
    match e {
        PassError::Stopped(r) => Some(*r),
        PassError::Batch(e) if primary_code(e) == Some(13) => Some(StopReason::InsufficientSpace),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_observation_needs_no_wal_qualifier() {
        assert!(is_full_observation(0));
        assert!(is_full_observation(
            QualifierBit::IosVersionUntested.mask() | QualifierBit::BackupDateOutOfRange.mask()
        ));
        for q in QualifierBit::WAL_RELATED {
            assert!(!is_full_observation(q.mask()), "{q:?}");
        }
    }
}
