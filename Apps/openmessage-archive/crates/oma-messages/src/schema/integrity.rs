//! Step 2 of the probe: `PRAGMA quick_check(1)` under an op budget (M3 §6.2,
//! §7.6, D37).
//!
//! It runs first after the scan, as sqlite.org/security.html recommends
//! (S18). Only `== "ok"` is evaluated. Any other text names attacker-chosen
//! tables, so it is compared and dropped, never kept or logged.

use oma_backup::{BackupError, Budgeted, StagedConnection};
use rusqlite::types::ValueRef;

use crate::error::{MessagesError, Stage};

/// The result of the integrity check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Integrity {
    /// `quick_check` returned exactly one row, `ok`.
    Ok,
    /// Anything else, including a corruption error from the check itself.
    Problems,
    /// The op budget ran out first.
    OverBudget,
}

/// `SQLITE_CORRUPT` and `SQLITE_NOTADB`: the check found damage rather than
/// failing for an unrelated reason.
const DAMAGE_CODES: [i32; 2] = [11, 26];

/// Run `PRAGMA quick_check(1)` under a budget of `ops` VM operations.
pub(crate) fn quick_check(
    conn: &StagedConnection<'_>,
    ops: u64,
) -> Result<Integrity, MessagesError> {
    let result = conn.with_op_budget(ops, |c| {
        let mut st = c.prepare_read("PRAGMA quick_check(1)")?;
        let mut rows = st.query([]).map_err(|e| c.sqlite_error(&e))?;
        let mut n: u32 = 0;
        let mut ok = false;
        while let Some(row) = rows.next().map_err(|e| c.sqlite_error(&e))? {
            n = n.saturating_add(1);
            ok = matches!(row.get_ref(0), Ok(ValueRef::Text(b"ok")));
        }
        Ok(n == 1 && ok)
    });
    match result {
        Ok(Budgeted::Done(true)) => Ok(Integrity::Ok),
        Ok(Budgeted::Done(false)) => Ok(Integrity::Problems),
        Ok(Budgeted::OverBudget) => Ok(Integrity::OverBudget),
        Err(BackupError::Sqlite { code: Some(code) }) if DAMAGE_CODES.contains(&code) => {
            Ok(Integrity::Problems)
        }
        Err(e) => Err(MessagesError::from_conn(e, Stage::Integrity)),
    }
}
