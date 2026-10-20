//! What an export covers, as ids (M6 spec §3.2, §4.9), and the one walk
//! over it that the preview, the job's pre-count and the scope index all
//! share, so their numbers agree by construction.
//!
//! - [`ExportScopeIds::Conversation`]: one conversation's timeline in a
//!   [`TimeRange`] (the "conversation" scope, or a date range with a
//!   conversation);
//! - [`ExportScopeIds::Archive`]: every conversation in id order, then the
//!   messages in no conversation, in a range (the whole archive, or a date
//!   range);
//! - [`ExportScopeIds::Messages`]: given messages, grouped by the
//!   conversation each is written under (a search result set, from
//!   [`super::ExportSnapshot::search_scope`]); nothing folds.
//!
//! The walk visits every **timeline row** of the scope: each
//! conversation's `chat_message` rows in range (a timeline item, or a
//! reaction folded into its target) and each message in no conversation.
//!
//! **Events left out (slice 6k2).** When the snapshot leaves events out
//! ([`ExportSnapshot::leave_out_events`]), the walk skips every row
//! [`left_out`] names (a group or system event, and a reaction row folded
//! into one) and returns how many it skipped: the pre-count, the scope
//! index and the cursors all skip the same rows, so the numbers still
//! agree and ordinals have no gaps.

use rusqlite::OptionalExtension;

use super::cursor::TimeRange;
use super::measure::BatchLimits;
use super::ExportSnapshot;
use crate::error::ArchiveError;
use crate::schema::codes::ItemKindCode;
use crate::view::{ChatId, MessageId};

/// A message's derived item kind (one primary-key seek).
pub(crate) const SQL_ITEM_KIND: &str = "SELECT item_kind FROM message WHERE id = ?1";

/// Whether message `id` is a group or system event: an item kind that is
/// neither a message nor unknown (an unknown kind may be a message of a
/// type this version does not map, so it stays). A missing row is not
/// an event.
pub(crate) fn is_event(snap: &ExportSnapshot<'_>, id: i64) -> Result<bool, ArchiveError> {
    let kind: Option<i64> = snap.run(|| {
        snap.stmts
            .prepare(SQL_ITEM_KIND)?
            .query_row([id], |r| r.get(0))
            .optional()
    })?;
    Ok(
        kind.is_some_and(|k| {
            k != ItemKindCode::Message.code() && k != ItemKindCode::Unknown.code()
        }),
    )
}

/// Whether the timeline row `id` (folded into `folded_into`, if it folds)
/// is left out of this snapshot's export: never unless the snapshot
/// leaves events out; then an event, or a reaction row folded into one.
pub(crate) fn left_out(
    snap: &ExportSnapshot<'_>,
    id: i64,
    folded_into: Option<i64>,
) -> Result<bool, ArchiveError> {
    if !snap.leaves_out_events() {
        return Ok(false);
    }
    if is_event(snap, id)? {
        return Ok(true);
    }
    match folded_into {
        Some(target) => is_event(snap, target),
        None => Ok(false),
    }
}

/// The scope of an export, as ids.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExportScopeIds {
    /// One conversation's timeline in `range`.
    Conversation { chat: ChatId, range: TimeRange },
    /// Every conversation, in id order, then the messages in no
    /// conversation, in `range`.
    Archive { range: TimeRange },
    /// Given messages, by the conversation each is written under, each
    /// group in timeline order (`chat: None`, no conversation, last).
    Messages(Vec<ScopeGroup>),
}

impl std::fmt::Debug for ExportScopeIds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Ids and ranges are never printed (I6): the kind and sizes only.
        match self {
            ExportScopeIds::Conversation { range, .. } => f
                .debug_struct("Conversation")
                .field("dated", &matches!(range, TimeRange::Dated { .. }))
                .finish_non_exhaustive(),
            ExportScopeIds::Archive { range } => f
                .debug_struct("Archive")
                .field("dated", &matches!(range, TimeRange::Dated { .. }))
                .finish(),
            ExportScopeIds::Messages(g) => f
                .debug_struct("Messages")
                .field("groups", &g.len())
                .field(
                    "messages",
                    &g.iter().map(|g| g.messages.len()).sum::<usize>(),
                )
                .finish(),
        }
    }
}

/// The messages written under one conversation (or under none).
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ScopeGroup {
    /// `None`: the messages in no conversation.
    pub chat: Option<ChatId>,
    /// In timeline order, each once.
    pub messages: Vec<MessageId>,
}

impl ScopeGroup {
    pub fn new(chat: Option<ChatId>, messages: Vec<MessageId>) -> ScopeGroup {
        ScopeGroup { chat, messages }
    }
}

impl std::fmt::Debug for ScopeGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopeGroup")
            .field("in_conversation", &self.chat.is_some())
            .field("messages", &self.messages.len())
            .finish()
    }
}

/// Every message id that is a timeline row of a scope (items and folded
/// reaction rows), sorted and distinct (§4.9): an export numbers messages
/// by their rank here, so ordinals have no gaps and say nothing about the
/// archive outside the scope.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct ScopeIndex {
    ids: Vec<i64>,
}

impl std::fmt::Debug for ScopeIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopeIndex")
            .field("len", &self.ids.len())
            .finish()
    }
}

impl ScopeIndex {
    pub(crate) fn from_ids(mut ids: Vec<i64>) -> ScopeIndex {
        ids.sort_unstable();
        ids.dedup();
        ScopeIndex { ids }
    }

    /// How many messages the scope holds.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Whether `id` is in the scope.
    pub fn contains(&self, id: MessageId) -> bool {
        self.ids.binary_search(&id.get()).is_ok()
    }

    /// `id`'s export-local ordinal: its rank among the scope's ids, from
    /// 1; `None` when it is not in the scope.
    pub fn ordinal(&self, id: MessageId) -> Option<u64> {
        let i = self.ids.binary_search(&id.get()).ok()?;
        u64::try_from(i).ok().map(|i| i + 1)
    }
}

/// One timeline row of a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScopeRow {
    /// The conversation it is written under; `None`: no conversation.
    pub(crate) chat: Option<i64>,
    pub(crate) id: i64,
    /// The target a reaction row folds into, in that conversation.
    pub(crate) folded_into: Option<i64>,
}

/// Rows read per walk batch (the walk measures nothing).
const WALK_BATCH: usize = 4_096;

/// Visit every timeline row of `scope`, in export order, materialised
/// batch by batch (no cursor statement steps while `f` runs). Rows the
/// snapshot leaves out ([`left_out`]) are skipped; returns how many.
pub(crate) fn walk(
    snap: &ExportSnapshot<'_>,
    scope: &ExportScopeIds,
    f: &mut dyn FnMut(ScopeRow) -> Result<(), ArchiveError>,
) -> Result<u64, ArchiveError> {
    let mut skipped = 0u64;
    let mut f = |row: ScopeRow| -> Result<(), ArchiveError> {
        if left_out(snap, row.id, row.folded_into)? {
            skipped += 1;
            return Ok(());
        }
        f(row)
    };
    let limits = BatchLimits::new(WALK_BATCH, u64::MAX);
    let mut timeline = |chat: Option<ChatId>, range: TimeRange| -> Result<(), ArchiveError> {
        let mut cur = match chat {
            Some(c) => snap.timeline(c, range),
            None => snap.unassigned(range),
        }
        .with_limits(limits);
        let chat = chat.map(ChatId::get);
        while let Some(b) = cur.next_batch_rows()? {
            for (id, folded_into) in b {
                f(ScopeRow {
                    chat,
                    id,
                    folded_into,
                })?;
            }
        }
        Ok(())
    };
    match scope {
        ExportScopeIds::Conversation { chat, range } => timeline(Some(*chat), *range)?,
        ExportScopeIds::Archive { range } => {
            for chat in snap.chat_ids()? {
                timeline(Some(chat), *range)?;
            }
            timeline(None, *range)?;
        }
        ExportScopeIds::Messages(groups) => {
            for g in groups {
                for (i, m) in g.messages.iter().enumerate() {
                    if i % WALK_BATCH == 0 {
                        snap.checkpoint()?;
                    }
                    f(ScopeRow {
                        chat: g.chat.map(ChatId::get),
                        id: m.get(),
                        folded_into: None,
                    })?;
                }
            }
        }
    }
    Ok(skipped)
}

/// The scope index of `scope` (§4.9).
pub(crate) fn index(
    snap: &ExportSnapshot<'_>,
    scope: &ExportScopeIds,
) -> Result<ScopeIndex, ArchiveError> {
    let mut ids = Vec::new();
    walk(snap, scope, &mut |row| {
        ids.push(row.id);
        Ok(())
    })?;
    Ok(ScopeIndex::from_ids(ids))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinals_are_ranks_without_gaps_and_debug_prints_no_id() {
        let ix = ScopeIndex::from_ids(vec![900, 7, 42, 7, 1_000_000]);
        assert_eq!(ix.len(), 4);
        assert_eq!(ix.ordinal(MessageId::new(7)), Some(1));
        assert_eq!(ix.ordinal(MessageId::new(42)), Some(2));
        assert_eq!(ix.ordinal(MessageId::new(1_000_000)), Some(4));
        assert_eq!(ix.ordinal(MessageId::new(8)), None);
        assert!(ix.contains(MessageId::new(900)));
        assert!(!format!("{ix:?}").contains("900"));
        let scope = ExportScopeIds::Messages(vec![ScopeGroup::new(
            Some(ChatId::new(31337)),
            vec![MessageId::new(4242)],
        )]);
        let dbg = format!("{scope:?}");
        assert!(!dbg.contains("31337") && !dbg.contains("4242"), "{dbg}");
    }
}
