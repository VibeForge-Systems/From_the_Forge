//! Prepared statements kept for one read call or one export snapshot (M6
//! spec §3.1 item 8: statement reuse).
//!
//! The assembly layer prepares about fifteen statements per message
//! (`view/assemble.rs`, the details and the reply quote). It takes a
//! [`StmtCache`] instead of a bare connection: the cache derefs to the
//! connection for every other use, and its own [`StmtCache::prepare`]
//! (which takes the SQL as a `&'static str`, so only the code's constant
//! statements are cached) shadows `Connection::prepare`, so the layer's
//! `c.prepare(SQL_…)` calls reuse a statement prepared earlier in the same
//! call or snapshot. A viewer call keeps one cache per phase; an export
//! snapshot keeps one for the whole job.
//!
//! rusqlite's own `prepare_cached` lives behind its `cache` feature, which
//! pulls in a crate this workspace does not have (`hashlink`), and PR A
//! adds no dependency but `csv`. This is the same idea in a few lines: a
//! statement is taken out of the cache while it is used (so a nested use of
//! the same SQL prepares a second one rather than sharing a stepping
//! statement), and put back when the guard drops. By then every `Rows`
//! borrowing it has been dropped, and rusqlite resets a statement when its
//! `Rows` drop, so a cached statement never holds a read position. The
//! bindings are cleared too.
//!
//! The cache is per snapshot and dies with it; nothing outlives the
//! snapshot's read transaction.

use std::cell::RefCell;
use std::ops::{Deref, DerefMut};

use rusqlite::{Connection, Statement};

/// At most this many idle statements are kept (the M6 spec's
/// `set_prepared_statement_cache_capacity(64)`).
pub(crate) const STATEMENT_CACHE: usize = 64;

/// A connection with the idle statements of one call or snapshot, by their
/// SQL text.
pub(crate) struct StmtCache<'c> {
    conn: &'c Connection,
    idle: RefCell<Vec<(&'static str, Statement<'c>)>>,
}

impl<'c> StmtCache<'c> {
    pub(crate) fn new(conn: &'c Connection) -> StmtCache<'c> {
        StmtCache {
            conn,
            idle: RefCell::new(Vec::new()),
        }
    }

    /// `sql` prepared, from the cache when an idle copy is there.
    pub(crate) fn prepare(&self, sql: &'static str) -> rusqlite::Result<Cached<'_, 'c>> {
        let found = {
            let mut idle = self.idle.borrow_mut();
            idle.iter()
                .rposition(|(s, _)| *s == sql)
                .map(|i| idle.swap_remove(i).1)
        };
        let stmt = match found {
            Some(s) => s,
            None => self.conn.prepare(sql)?,
        };
        Ok(Cached {
            cache: self,
            sql,
            stmt: Some(stmt),
        })
    }

    /// How many statements are idle (tests).
    #[cfg(test)]
    pub(crate) fn idle(&self) -> usize {
        self.idle.borrow().len()
    }
}

impl Deref for StmtCache<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.conn
    }
}

/// A statement taken from a [`StmtCache`]; it goes back when dropped.
pub(crate) struct Cached<'s, 'c> {
    cache: &'s StmtCache<'c>,
    sql: &'static str,
    stmt: Option<Statement<'c>>,
}

impl<'c> Deref for Cached<'_, 'c> {
    type Target = Statement<'c>;

    #[allow(clippy::expect_used)] // `stmt` is `Some` until `drop`
    fn deref(&self) -> &Statement<'c> {
        self.stmt.as_ref().expect("a cached statement before drop")
    }
}

impl DerefMut for Cached<'_, '_> {
    #[allow(clippy::expect_used)] // `stmt` is `Some` until `drop`
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.stmt.as_mut().expect("a cached statement before drop")
    }
}

impl Drop for Cached<'_, '_> {
    fn drop(&mut self) {
        let Some(mut stmt) = self.stmt.take() else {
            return;
        };
        // No `Rows` borrows it any more, so it was reset when they
        // dropped; the bindings go too.
        stmt.clear_bindings();
        if let Ok(mut idle) = self.cache.idle.try_borrow_mut() {
            // Full: the statement idle longest is finalized.
            if idle.len() >= STATEMENT_CACHE {
                idle.remove(0);
            }
            idle.push((self.sql, stmt));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_are_reused_reset_and_never_shared_while_in_use() {
        let conn = crate::conn::open_memory().expect("memory");
        conn.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1), (2), (3);")
            .expect("table");
        let cache = StmtCache::new(&conn);
        const ALL: &str = "SELECT x FROM t ORDER BY x";
        {
            let mut a = cache.prepare(ALL).expect("prepare");
            let mut rows = a.query([]).expect("query");
            let first: i64 = rows.next().expect("step").expect("row").get(0).expect("x");
            assert_eq!(first, 1);
            // A nested use of the same SQL while `a` steps gets its own.
            let mut b = cache.prepare(ALL).expect("prepare");
            let n: i64 = b.query_row([], |r| r.get(0)).expect("row");
            assert_eq!(n, 1);
        }
        assert_eq!(cache.idle(), 2);
        // Taken again from the cache, it starts from the first row: the
        // `Rows` that stopped after one row reset it.
        let mut again = cache.prepare(ALL).expect("cached");
        let xs: Vec<i64> = again
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        assert_eq!(xs, vec![1, 2, 3]);
        drop(again);
        // No read transaction is left open by an idle statement.
        assert!(conn.is_autocommit());
    }
}
