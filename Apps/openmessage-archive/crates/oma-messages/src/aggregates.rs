//! Aggregates: integer-keyed SQL under op budgets, and the GUID counts
//! (M3 §7.3, D45, D46).
//!
//! Each statement is static and runs only when the tables and columns it
//! names are present. Each runs under an op budget proportional to the
//! rows of the tables it touches, `AGG_OPS_PER_ROW × rows + AGG_OPS_BASE`.
//! A statement over its budget leaves its counter `None` and is counted in
//! [`Aggregates::over_budget`], which the report turns into the qualifier
//! `AggregatesIncomplete`. The constants are **UNVERIFIED** for real data.
//!
//! **Plan-safe forms.** The integer checks use `x NOT IN (SELECT …)` and
//! `GROUP BY … HAVING`, which SQLite plans as a list subquery or a temporary
//! B-tree, never as a correlated subquery that scans a table once per row
//! (`tests/query_plans.rs` checks every statement on an index-free
//! database). Every subquery over a column that can be NULL filters
//! `IS NOT NULL`, so a NULL cannot turn `NOT IN` into "unknown" for every
//! row.
//!
//! **GUID checks run in Rust**, from [`GuidSets`], so no automatic index is
//! ever built on attacker-sized `guid` columns.

use oma_backup::{BackupError, Budgeted, StagedConnection};
use rusqlite::types::ValueRef;

use crate::error::{MessagesError, Stage};
use crate::guidset::{GuidCounts, GuidSets};
use crate::progress::{MessagesCtx, MessagesPhase};
use crate::schema::known::message;
use crate::schema::{Capabilities, KnownColumn, KnownTable};

/// Op budget per row of the tables a statement touches. On the synthetic
/// catalogue each statement needs well under 100 operations per row; this
/// allows a wide margin. **UNVERIFIED** for real data.
pub const AGG_OPS_PER_ROW: u64 = 2_000;
/// Op budget added to every statement.
pub const AGG_OPS_BASE: u64 = 1_000_000;

/// One aggregate statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum AggregateId {
    /// Messages that no `chat_message_join` row names.
    MessagesInNoChat,
    /// Of those, the ones a Recently Deleted link names.
    MessagesInNoChatRecentlyDeleted,
    /// Messages linked to more than one distinct chat.
    MessagesInSeveralChats,
    /// `chat_message_join.chat_id` naming no chat.
    OrphanChatMessageChat,
    /// `chat_message_join.message_id` naming no message.
    OrphanChatMessageMessage,
    /// `chat_handle_join.chat_id` naming no chat.
    OrphanChatHandleChat,
    /// `chat_handle_join.handle_id` naming no handle.
    OrphanChatHandleHandle,
    /// `message_attachment_join.message_id` naming no message.
    OrphanMessageAttachmentMessage,
    /// `message_attachment_join.attachment_id` naming no attachment.
    OrphanMessageAttachmentAttachment,
    /// `chat_recoverable_message_join.chat_id` naming no chat.
    OrphanRecoverableChat,
    /// `chat_recoverable_message_join.message_id` naming no message.
    OrphanRecoverableMessage,
    /// Messages whose non-zero `handle_id` matches no handle.
    MessageHandleUnmatched,
    /// Messages whose non-zero `other_handle` matches no handle.
    MessageOtherHandleUnmatched,
    /// Attachments a `message_attachment_join` row names.
    AttachmentsLinked,
    /// Chats linked to no handle.
    ChatsWithNoHandle,
    /// Chats linked to exactly one distinct handle.
    ChatsWithOneHandle,
    /// Chats linked to two or more distinct handles.
    ChatsWithSeveralHandles,
}

/// What a statement needs: tables, and columns of those tables.
struct Needs {
    tables: &'static [KnownTable],
    columns: &'static [(KnownTable, u8)],
}

const fn needs(tables: &'static [KnownTable], columns: &'static [(KnownTable, u8)]) -> Needs {
    Needs { tables, columns }
}

use KnownTable as T;

const M: KnownTable = T::Message;
const CMJ: KnownTable = T::ChatMessageJoin;
const CHJ: KnownTable = T::ChatHandleJoin;
const MAJ: KnownTable = T::MessageAttachmentJoin;
const CRMJ: KnownTable = T::ChatRecoverableMessageJoin;

/// Every statement, its SQL and what it needs. The SQL names only our own
/// static identifiers, double-quoted.
fn spec(id: AggregateId) -> (&'static str, Needs) {
    use AggregateId as A;
    match id {
        A::MessagesInNoChat => (
            "SELECT count(*) FROM \"message\" WHERE \"ROWID\" NOT IN \
             (SELECT \"message_id\" FROM \"chat_message_join\" WHERE \"message_id\" IS NOT NULL)",
            needs(&[M, CMJ], &[]),
        ),
        A::MessagesInNoChatRecentlyDeleted => (
            "SELECT count(*) FROM \"message\" WHERE \"ROWID\" NOT IN \
             (SELECT \"message_id\" FROM \"chat_message_join\" WHERE \"message_id\" IS NOT NULL) \
             AND \"ROWID\" IN (SELECT \"message_id\" FROM \"chat_recoverable_message_join\" \
             WHERE \"message_id\" IS NOT NULL)",
            needs(&[M, CMJ, CRMJ], &[]),
        ),
        A::MessagesInSeveralChats => (
            "SELECT count(*) FROM (SELECT \"message_id\" FROM \"chat_message_join\" \
             WHERE \"message_id\" IS NOT NULL AND \"chat_id\" IS NOT NULL \
             GROUP BY \"message_id\" HAVING count(DISTINCT \"chat_id\") > 1)",
            needs(&[CMJ], &[]),
        ),
        A::OrphanChatMessageChat => (
            "SELECT count(*) FROM \"chat_message_join\" WHERE \"chat_id\" NOT IN \
             (SELECT \"ROWID\" FROM \"chat\")",
            needs(&[CMJ, T::Chat], &[]),
        ),
        A::OrphanChatMessageMessage => (
            "SELECT count(*) FROM \"chat_message_join\" WHERE \"message_id\" NOT IN \
             (SELECT \"ROWID\" FROM \"message\")",
            needs(&[CMJ, M], &[]),
        ),
        A::OrphanChatHandleChat => (
            "SELECT count(*) FROM \"chat_handle_join\" WHERE \"chat_id\" NOT IN \
             (SELECT \"ROWID\" FROM \"chat\")",
            needs(&[CHJ, T::Chat], &[]),
        ),
        A::OrphanChatHandleHandle => (
            "SELECT count(*) FROM \"chat_handle_join\" WHERE \"handle_id\" NOT IN \
             (SELECT \"ROWID\" FROM \"handle\")",
            needs(&[CHJ, T::Handle], &[]),
        ),
        A::OrphanMessageAttachmentMessage => (
            "SELECT count(*) FROM \"message_attachment_join\" WHERE \"message_id\" NOT IN \
             (SELECT \"ROWID\" FROM \"message\")",
            needs(&[MAJ, M], &[]),
        ),
        A::OrphanMessageAttachmentAttachment => (
            "SELECT count(*) FROM \"message_attachment_join\" WHERE \"attachment_id\" NOT IN \
             (SELECT \"ROWID\" FROM \"attachment\")",
            needs(&[MAJ, T::Attachment], &[]),
        ),
        A::OrphanRecoverableChat => (
            "SELECT count(*) FROM \"chat_recoverable_message_join\" WHERE \"chat_id\" NOT IN \
             (SELECT \"ROWID\" FROM \"chat\")",
            needs(&[CRMJ, T::Chat], &[]),
        ),
        A::OrphanRecoverableMessage => (
            "SELECT count(*) FROM \"chat_recoverable_message_join\" WHERE \"message_id\" NOT IN \
             (SELECT \"ROWID\" FROM \"message\")",
            needs(&[CRMJ, M], &[]),
        ),
        A::MessageHandleUnmatched => (
            "SELECT count(*) FROM \"message\" WHERE \"handle_id\" <> 0 AND \"handle_id\" NOT IN \
             (SELECT \"ROWID\" FROM \"handle\")",
            needs(&[M, T::Handle], &[(M, message::HANDLE_ID)]),
        ),
        A::MessageOtherHandleUnmatched => (
            "SELECT count(*) FROM \"message\" WHERE \"other_handle\" <> 0 AND \"other_handle\" \
             NOT IN (SELECT \"ROWID\" FROM \"handle\")",
            needs(&[M, T::Handle], &[(M, message::OTHER_HANDLE)]),
        ),
        A::AttachmentsLinked => (
            "SELECT count(*) FROM \"attachment\" WHERE \"ROWID\" IN \
             (SELECT \"attachment_id\" FROM \"message_attachment_join\" \
             WHERE \"attachment_id\" IS NOT NULL)",
            needs(&[T::Attachment, MAJ], &[]),
        ),
        A::ChatsWithNoHandle => (
            "SELECT count(*) FROM \"chat\" WHERE \"ROWID\" NOT IN \
             (SELECT \"chat_id\" FROM \"chat_handle_join\" \
             WHERE \"chat_id\" IS NOT NULL AND \"handle_id\" IS NOT NULL)",
            needs(&[T::Chat, CHJ], &[]),
        ),
        A::ChatsWithOneHandle => (
            "SELECT count(*) FROM (SELECT \"chat_id\" FROM \"chat_handle_join\" \
             WHERE \"handle_id\" IS NOT NULL AND \"chat_id\" IN (SELECT \"ROWID\" FROM \"chat\") \
             GROUP BY \"chat_id\" HAVING count(DISTINCT \"handle_id\") = 1)",
            needs(&[T::Chat, CHJ], &[]),
        ),
        A::ChatsWithSeveralHandles => (
            "SELECT count(*) FROM (SELECT \"chat_id\" FROM \"chat_handle_join\" \
             WHERE \"handle_id\" IS NOT NULL AND \"chat_id\" IN (SELECT \"ROWID\" FROM \"chat\") \
             GROUP BY \"chat_id\" HAVING count(DISTINCT \"handle_id\") > 1)",
            needs(&[T::Chat, CHJ], &[]),
        ),
    }
}

impl AggregateId {
    /// Every statement, in the order they run.
    pub const ALL: [AggregateId; 17] = [
        AggregateId::MessagesInNoChat,
        AggregateId::MessagesInNoChatRecentlyDeleted,
        AggregateId::MessagesInSeveralChats,
        AggregateId::OrphanChatMessageChat,
        AggregateId::OrphanChatMessageMessage,
        AggregateId::OrphanChatHandleChat,
        AggregateId::OrphanChatHandleHandle,
        AggregateId::OrphanMessageAttachmentMessage,
        AggregateId::OrphanMessageAttachmentAttachment,
        AggregateId::OrphanRecoverableChat,
        AggregateId::OrphanRecoverableMessage,
        AggregateId::MessageHandleUnmatched,
        AggregateId::MessageOtherHandleUnmatched,
        AggregateId::AttachmentsLinked,
        AggregateId::ChatsWithNoHandle,
        AggregateId::ChatsWithOneHandle,
        AggregateId::ChatsWithSeveralHandles,
    ];

    /// The statement's SQL (our own, static). Exposed for the query-plan
    /// and statement-size tests.
    pub fn sql(self) -> &'static str {
        spec(self).0
    }
}

/// The aggregate counts. `None` means "not recorded by this database" (a
/// table or column is missing) or "over budget" (counted in
/// `over_budget`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Aggregates {
    /// One entry per [`AggregateId`], in [`AggregateId::ALL`] order.
    pub values: Vec<(AggregateId, Option<u64>)>,
    /// Statements that ran out of their op budget.
    pub over_budget: u32,
    /// The GUID-set counts (§7.3).
    pub guids: GuidCounts,
    /// Whether every GUID offered was kept (no cap was reached).
    pub guids_complete: bool,
}

impl Aggregates {
    /// One statement's count.
    pub fn get(&self, id: AggregateId) -> Option<u64> {
        self.values
            .iter()
            .find(|(i, _)| *i == id)
            .and_then(|(_, v)| *v)
    }
}

/// Budget constants, which options may only tighten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AggBudget {
    pub(crate) per_row: u64,
    pub(crate) base: u64,
}

impl Default for AggBudget {
    fn default() -> Self {
        AggBudget {
            per_row: AGG_OPS_PER_ROW,
            base: AGG_OPS_BASE,
        }
    }
}

fn applies(caps: &Capabilities, n: &Needs) -> bool {
    n.tables
        .iter()
        .all(|t| caps.table(*t).is_some_and(|c| c.present))
        && n.columns
            .iter()
            .all(|(t, i)| caps.has_column(KnownColumn::new(*t, *i)))
}

fn rows_touched(caps: &Capabilities, n: &Needs) -> u64 {
    n.tables
        .iter()
        .filter_map(|t| caps.table(*t).and_then(|c| c.rows))
        .fold(0u64, u64::saturating_add)
}

/// Run every applicable statement, then count the GUID sets.
pub(crate) fn run(
    conn: &StagedConnection<'_>,
    caps: &Capabilities,
    budget: AggBudget,
    guids: &mut GuidSets,
    ctx: &mut MessagesCtx<'_>,
) -> Result<Aggregates, MessagesError> {
    let err = |e: BackupError| MessagesError::from_conn(e, Stage::Aggregates);
    let total = u64::try_from(AggregateId::ALL.len()).ok();
    let mut out = Aggregates::default();
    ctx.check()?;
    ctx.report(MessagesPhase::Summarising, 0, total);
    for (done, id) in AggregateId::ALL.into_iter().enumerate() {
        ctx.check()?;
        let (sql, n) = spec(id);
        let value = if applies(caps, &n) {
            let ops = budget
                .per_row
                .saturating_mul(rows_touched(caps, &n))
                .saturating_add(budget.base);
            let result = conn
                .with_op_budget(ops, |c| {
                    let mut st = c.prepare_read(sql)?;
                    st.query_row([], |r| {
                        Ok(match r.get_ref(0)? {
                            ValueRef::Integer(n) => u64::try_from(n).ok(),
                            _ => None,
                        })
                    })
                    .map_err(|e| c.sqlite_error(&e))
                })
                .map_err(err)?;
            match result {
                Budgeted::Done(Some(v)) => Some(v),
                Budgeted::Done(None) => {
                    return Err(MessagesError::Internal("count is not an integer"))
                }
                Budgeted::OverBudget => {
                    out.over_budget = out.over_budget.saturating_add(1);
                    tracing::debug!(aggregate = ?id, "aggregate over budget");
                    None
                }
            }
        } else {
            None
        };
        out.values.push((id, value));
        ctx.report(
            MessagesPhase::Summarising,
            u64::try_from(done).unwrap_or(u64::MAX).saturating_add(1),
            total,
        );
    }
    ctx.check()?;
    out.guids_complete = guids.complete();
    out.guids = guids.counts();
    Ok(out)
}

/// Every aggregate statement's SQL, for the plan and size tests.
pub fn aggregate_queries() -> impl Iterator<Item = &'static str> {
    AggregateId::ALL.into_iter().map(AggregateId::sql)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_are_distinct_and_quoted() {
        let mut all: Vec<&str> = aggregate_queries().collect();
        let n = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), n);
        for sql in all {
            assert!(sql.starts_with("SELECT count(*) FROM "), "{sql}");
            assert!(!sql.contains('\''), "no string literals: {sql}");
            assert!(!sql.contains('?'), "no parameters: {sql}");
        }
    }
}
