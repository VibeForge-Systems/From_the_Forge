//! Batches and the pass driver (spec §8.4, §8.5, §8.6, D63).
//!
//! A pass reads source records into a buffer **with no archive transaction
//! open**, until the batch budget is reached, the pass ends or the job is
//! cancelled. The buffer is then written in one batch: a rusqlite
//! `Transaction` with `TransactionBehavior::Immediate` and
//! `DropBehavior::Rollback`, holding
//!
//! - one `SAVEPOINT` per source record, released when the record is
//!   complete;
//! - the records' rows, map rows, occurrence counters, `value_issue` and
//!   `value_history` rows;
//! - the `import_cursor` upsert, the `import_session.counts` JSON and the
//!   `import_issue` deltas.
//!
//! The cursor is the importer's own: the rowid of the last **completed**
//! record, never the engine's `PassOutcome.last`. `rows_done` counts
//! completed records, summed across resumed segments, and must equal the
//! probe's `count(*)` when the pass ends.
//!
//! After any statement error the batch is rolled back (and `ROLLBACK` is
//! issued if the connection is somehow still in a transaction): nothing is
//! ever committed after a statement error. The caller classifies the error.
//!
//! Cancellation: the callback checks the token at the start of each record,
//! and the engine checks it every 128 rows. Either way the records already
//! read are complete, so the batch **commits** them with the cursor, and
//! the pass reports [`PassEnd::Cancelled`].

use std::ops::ControlFlow;
use std::time::Duration;

use oma_messages::{Cursor, KnownTable, MessagesCtx, MessagesError, PassOutcome, SmsDb};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use super::counts::ImportCounts;
use super::keys::Rank;
use super::preflight::DupSet;
use crate::fault::{point, FaultPoint};
use crate::progress::{ArchiveCtx, ArchiveEvent, ImportPhase};
use crate::schema::codes::{IssueCode, Pass};

/// How much one batch transaction holds (spec §8.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchBudget {
    rows: u32,
    bytes: u64,
    time: Option<Duration>,
}

impl BatchBudget {
    /// The default row budget.
    pub const ROWS_DEFAULT: u32 = 1000;
    /// The smallest row budget.
    pub const ROWS_MIN: u32 = 16;
    /// The largest row budget.
    pub const ROWS_MAX: u32 = 8192;
    /// The default byte budget (raw content bytes).
    pub const BYTES_DEFAULT: u64 = 32 << 20;
    /// The smallest byte budget.
    pub const BYTES_MIN: u64 = 1 << 20;
    /// The largest byte budget.
    pub const BYTES_MAX: u64 = 256 << 20;
    /// The default time budget, measured on the context's clock.
    pub const TIME_DEFAULT: Duration = Duration::from_secs(2);

    /// A budget clamped to the limits above. `time: None` disables the time
    /// budget, so batch boundaries depend only on the data (the crash tests
    /// use this with an injected clock).
    pub fn new(rows: u32, bytes: u64, time: Option<Duration>) -> BatchBudget {
        BatchBudget {
            rows: rows.clamp(Self::ROWS_MIN, Self::ROWS_MAX),
            bytes: bytes.clamp(Self::BYTES_MIN, Self::BYTES_MAX),
            time,
        }
    }

    pub fn rows(&self) -> u32 {
        self.rows
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn time(&self) -> Option<Duration> {
        self.time
    }
}

impl Default for BatchBudget {
    fn default() -> Self {
        BatchBudget::new(
            Self::ROWS_DEFAULT,
            Self::BYTES_DEFAULT,
            Some(Self::TIME_DEFAULT),
        )
    }
}

/// Why the pass callback stopped the engine.
pub(crate) enum Brk {
    /// The batch budget is reached.
    Full,
    /// The job was cancelled at the start of a record.
    Cancelled,
}

/// The import session a pass writes for.
pub(crate) struct Run {
    pub(crate) session: i64,
    pub(crate) lineage: i64,
    pub(crate) rank: Rank,
    pub(crate) dups: DupSet,
    pub(crate) budget: BatchBudget,
}

/// Issue counts a batch adds to `import_issue`, per kind.
#[derive(Default)]
pub(crate) struct IssueDeltas(std::collections::BTreeMap<i64, u64>);

impl IssueDeltas {
    pub(crate) fn add(&mut self, code: IssueCode, n: u64) {
        if n > 0 {
            let e = self.0.entry(code.code()).or_insert(0);
            *e = e.saturating_add(n);
        }
    }
}

/// One pass: how to read its records and how to write a batch of them.
pub(crate) trait PassWriter {
    type Record;
    const PASS: Pass;
    const PHASE: ImportPhase;
    /// The source table whose `count(*)` the pass must reach.
    const SOURCE: KnownTable;

    fn rowid(r: &Self::Record) -> i64;
    /// Content bytes, for the batch byte budget.
    fn size(r: &Self::Record) -> u64;
    /// Read records from `from` on. It takes `&self`, so a pass can hold
    /// what its reader needs at run time (P6's attachment locator, which
    /// borrows the opened backup).
    fn read(
        &self,
        db: &SmsDb<'_>,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: &mut dyn FnMut(Self::Record) -> ControlFlow<Brk>,
    ) -> Result<PassOutcome<Brk>, MessagesError>;
    /// Work a pass must make durable **before** its batch, in transactions
    /// of its own on `conn` (no batch is open). The default does nothing.
    /// P6 commits its blob intents and places the files here, so that the
    /// batch then commits the blob rows after the placement (spec §9.1). An
    /// error fails the pass like a batch error; nothing of the batch has
    /// been written.
    fn before_batch(
        &self,
        _conn: &mut Connection,
        _run: &Run,
        _records: &[Self::Record],
    ) -> rusqlite::Result<()> {
        Ok(())
    }
    /// Write the records, one savepoint each, inside the open batch.
    fn write(
        &self,
        tx: &Connection,
        run: &Run,
        records: &[Self::Record],
        counts: &mut ImportCounts,
        issues: &mut IssueDeltas,
    ) -> rusqlite::Result<()>;
}

/// Run `f` as one record: inside a savepoint that is released when it
/// completes. An error leaves the savepoint open; the caller rolls the
/// whole batch back.
pub(crate) fn record<T>(
    tx: &Connection,
    f: impl FnOnce() -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    tx.execute_batch("SAVEPOINT oma_record")?;
    let out = f()?;
    tx.execute_batch("RELEASE oma_record")?;
    Ok(out)
}

/// How a pass ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PassEnd {
    /// Every record is written and the cursor says `done`.
    Done,
    /// Cancelled; every record read was committed.
    Cancelled,
}

/// Why a pass failed.
pub(crate) enum PassError {
    /// A batch failed and was rolled back.
    Batch(rusqlite::Error),
    /// Reading the source failed (after the records read were committed).
    Read(MessagesError),
    /// `rows_done` differs from the probe's `count(*)`.
    CountMismatch,
}

/// A stored cursor.
struct StoredCursor {
    last: Option<i64>,
    rows_done: u64,
    done: bool,
}

fn load_cursor(
    conn: &Connection,
    session: i64,
    pass: Pass,
) -> rusqlite::Result<Option<StoredCursor>> {
    conn.query_row(
        "SELECT last_rowid, rows_done, done FROM import_cursor WHERE session_id = ?1 AND pass = ?2",
        rusqlite::params![session, pass.code()],
        |r| {
            let rows: i64 = r.get(1)?;
            Ok(StoredCursor {
                last: r.get(0)?,
                rows_done: u64::try_from(rows).unwrap_or(0),
                done: r.get::<_, i64>(2)? == 1,
            })
        },
    )
    .optional()
}

/// Write one batch and commit it. On any error the batch is rolled back.
#[allow(clippy::too_many_arguments)]
fn commit_batch<W: PassWriter>(
    w: &W,
    conn: &mut Connection,
    run: &Run,
    records: &[W::Record],
    last: Option<i64>,
    rows_done: u64,
    done: bool,
    counts: &mut ImportCounts,
) -> rusqlite::Result<()> {
    let mut working = counts.clone();
    let mut issues = IssueDeltas::default();
    let result = (|| -> rusqlite::Result<()> {
        let tx = rusqlite::Transaction::new(&mut *conn, TransactionBehavior::Immediate)?;
        w.write(&tx, run, records, &mut working, &mut issues)?;
        working.batches = working.batches.saturating_add(1);
        tx.execute(
            "INSERT INTO import_cursor (session_id, pass, last_rowid, rows_done, done) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT (session_id, pass) DO UPDATE SET last_rowid = excluded.last_rowid, \
                 rows_done = excluded.rows_done, done = excluded.done",
            rusqlite::params![
                run.session,
                W::PASS.code(),
                last,
                i64::try_from(rows_done).unwrap_or(i64::MAX),
                i64::from(done)
            ],
        )?;
        tx.execute(
            "UPDATE import_session SET counts = ?1 WHERE id = ?2",
            rusqlite::params![working.to_json(), run.session],
        )?;
        for (kind, n) in &issues.0 {
            tx.execute(
                "INSERT INTO import_issue (session_id, pass, kind, count) VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT (session_id, pass, kind) DO UPDATE SET count = count + excluded.count",
                rusqlite::params![
                    run.session,
                    W::PASS.code(),
                    kind,
                    i64::try_from(*n).unwrap_or(i64::MAX)
                ],
            )?;
        }
        point(FaultPoint::BatchBeforeCommit);
        if let Some(e) = crate::fault::batch_error() {
            return Err(e);
        }
        tx.commit()
    })();
    if result.is_err() && !conn.is_autocommit() {
        // A failed COMMIT (for example on a deferred foreign key) can leave
        // the transaction open; never commit after a statement error.
        let _ = conn.execute_batch("ROLLBACK");
    }
    result?;
    point(FaultPoint::BatchAfterCommit);
    *counts = working;
    Ok(())
}

/// Run one pass from its stored cursor to the end, batch by batch.
/// `counts` holds the committed counts and is updated after each commit.
pub(crate) fn run_pass<W: PassWriter>(
    w: &W,
    db: &SmsDb<'_>,
    conn: &mut Connection,
    run: &Run,
    counts: &mut ImportCounts,
    ctx: &mut ArchiveCtx<'_>,
    mctx: &mut MessagesCtx<'_>,
) -> Result<PassEnd, PassError> {
    let stored = load_cursor(conn, run.session, W::PASS).map_err(PassError::Batch)?;
    let (mut cursor, mut rows_done) = match stored {
        Some(c) if c.done => return Ok(PassEnd::Done),
        Some(c) => (c.last, c.rows_done),
        None => (None, 0),
    };
    let total = db.capabilities().table(W::SOURCE).and_then(|t| t.rows);
    ctx.progress.event(ArchiveEvent::Import {
        phase: W::PHASE,
        done: rows_done,
        total,
    });
    let budget = run.budget;
    let rows_budget = usize::try_from(budget.rows()).unwrap_or(usize::MAX);
    let time_ns = budget
        .time()
        .map(|t| i64::try_from(t.as_nanos()).unwrap_or(i64::MAX));
    loop {
        let mut buf: Vec<W::Record> = Vec::new();
        let mut bytes = 0u64;
        let cancel = ctx.cancel;
        let clock = ctx.clock;
        let started = clock.now_ns();
        let from = cursor.map_or(Cursor::Start, Cursor::After);
        let read = w.read(db, from, mctx, &mut |rec| {
            point(FaultPoint::Record);
            if cancel.is_cancelled() {
                return ControlFlow::Break(Brk::Cancelled);
            }
            bytes = bytes.saturating_add(W::size(&rec));
            buf.push(rec);
            let over_time = time_ns.is_some_and(|t| clock.now_ns().saturating_sub(started) >= t);
            if buf.len() >= rows_budget || bytes >= budget.bytes() || over_time {
                ControlFlow::Break(Brk::Full)
            } else {
                ControlFlow::Continue(())
            }
        });
        enum End {
            Exhausted,
            Full,
            Cancelled,
            Failed(MessagesError),
        }
        let end = match read {
            Ok(o) => match o.broke {
                None => End::Exhausted,
                Some(Brk::Full) => End::Full,
                Some(Brk::Cancelled) => End::Cancelled,
            },
            Err(MessagesError::Cancelled) => End::Cancelled,
            Err(e) => End::Failed(e),
        };
        let done = matches!(end, End::Exhausted);
        if !buf.is_empty() || done {
            let last = buf.last().map(W::rowid).or(cursor);
            let n = u64::try_from(buf.len()).unwrap_or(u64::MAX);
            let new_rows = rows_done.saturating_add(n);
            if !buf.is_empty() {
                w.before_batch(conn, run, &buf).map_err(PassError::Batch)?;
            }
            commit_batch(w, conn, run, &buf, last, new_rows, done, counts)
                .map_err(PassError::Batch)?;
            cursor = last;
            rows_done = new_rows;
            ctx.progress.event(ArchiveEvent::Import {
                phase: W::PHASE,
                done: rows_done,
                total,
            });
        }
        match end {
            End::Exhausted => {
                if total.is_some_and(|t| t != rows_done) {
                    return Err(PassError::CountMismatch);
                }
                return Ok(PassEnd::Done);
            }
            End::Full => {}
            End::Cancelled => return Ok(PassEnd::Cancelled),
            End::Failed(e) => return Err(PassError::Read(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_are_clamped() {
        let d = BatchBudget::default();
        assert_eq!(
            (d.rows(), d.bytes(), d.time()),
            (1000, 32 << 20, Some(Duration::from_secs(2)))
        );
        let lo = BatchBudget::new(0, 0, None);
        assert_eq!((lo.rows(), lo.bytes(), lo.time()), (16, 1 << 20, None));
        let hi = BatchBudget::new(u32::MAX, u64::MAX, None);
        assert_eq!((hi.rows(), hi.bytes()), (8192, 256 << 20));
    }
}
