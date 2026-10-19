//! Export reads (M6 spec §3, slice 6b1): the one way an export reads an
//! archive. `oma-export` never reaches SQLite; it calls these.
//!
//! - **The lock.** [`Library::hold_for_export`](crate::Library::hold_for_export)
//!   takes the archive's lock file for the job's lifetime ([`ExportHold`]),
//!   so no import or verify, in this process or another instance, writes
//!   the archive while the job reads it. Viewer readers stay lock-free.
//! - **One snapshot per job.** [`ArchiveReader::with_export_snapshot`]
//!   runs its closure inside **one** read transaction, in the cancellable
//!   mode (no op budget, no deadline, stopped by the job's cancel token
//!   only). The transaction ends when the closure returns. The mode is
//!   fixed: inside the closure every other read mode of the same reader
//!   refuses with `Internal`, so nothing can toggle the shared budget
//!   under a statement that is still stepping.
//! - **Cursors** ([`TimelineCursor`], [`IdCursor`], [`HistoryCursor`])
//!   materialise each batch and reset their statement before returning;
//!   batches are cut by a byte pre-measure ([`BatchLimits`]).
//! - **Assembly** ([`ExportSnapshot::assemble`]) runs the viewer's
//!   assembly layer under `Caps::EXPORT` (no cap) with the raw codes, then
//!   the check that runs in every build (a missed cap or missing extras is
//!   refused, never exported), and adds the uncapped rows and ledger
//!   columns ([`rows`]).
//! - **Statements** prepared by this module are kept for the snapshot's
//!   lifetime ([`stmt`]); each is pinned in `tests/query_plans.rs`.
//!
//! - **The preview** ([`ArchiveReader::export_preview`],
//!   [`ExportSnapshot::precount`]) counts a scope ([`ExportScopeIds`])
//!   exactly, with the dated span and the two reveal counts, and
//!   [`ExportSnapshot::scope_index`] numbers its messages ([`preview`],
//!   [`scope`]).
//! - **Search scopes** ([`ExportSnapshot::search_ids`],
//!   [`ExportSnapshot::search_scope`]) re-run a request with the export's
//!   cap and keep only hits whose match lies in what the export writes
//!   ([`search`]).
//! - **References and identifiers** ([`refs`], slice 6d): the
//!   conversations of any message, the ids of a message's replies, and
//!   the source identifiers that only the D141 opt-in reads.
//!
//! Masking is D115's, applied before anything leaves the crate: one-time
//! codes are masked and earlier text withheld unless the export's
//! [`Reveal`] asks (two per-export opt-ins, owner Q4; the viewer's
//! `revealEarlierText` setting never reaches this crate's export reads).
//! An export also withholds the attachments of unsent parts, the
//! plain-column alternative text and the subject history of such
//! messages ([`mask`], spec §3.8).
//!
//! Nothing here logs; errors carry kinds only.

pub mod cursor;
pub mod mask;
pub mod measure;
pub mod preview;
pub mod refs;
pub mod rows;
pub mod scope;
pub mod search;

use crate::blobs::BlobReader;
use crate::conn::{Phase, READ_OP_BUDGET};
use crate::error::{ArchiveError, Stage};
use crate::layout::ArchiveDir;
use crate::lock::ArchiveLock;
use crate::progress::CancelToken;
use crate::reader::{blob_row, open_blob_row, ArchiveReader, BlobId};
use crate::view::assemble::{Caps, Ctx, Reveal};
use crate::view::uncapped::{checked, export_item};
use crate::view::{ChatId, ExportItem, HandleId, MessageId};

pub use cursor::{CursorBatch, HistoryCursor, IdCursor, TimeRange, TimelineCursor, VersionCodes};
pub use mask::{AltTextSource, AltTextWithheld, AlternativeText, AttachmentAccess};
pub use measure::{BatchLimits, ExportDepth};
pub use preview::{ArchiveGeneration, ExportPreview, PreviewOptions, PreviewProgress};
pub use refs::{MessageRawColumns, SourceIdentifiers, SourceOf};
pub use rows::{
    ChatColumns, ChatMembership, Derivation, DetailColumns, ExportAttachment, HandleColumns,
    HistoryState, ReactionRow, RecentlyDeletedRow, ValueHistoryEntry,
};
pub use scope::{ExportScopeIds, ScopeGroup, ScopeIndex};
pub use search::{ExcludedHits, SearchScope, EXPORT_SEARCH_CAP};

use crate::stmt::StmtCache;

/// An archive's lock file, held for an export job's lifetime (M6 spec
/// §3.5). Dropping it releases the lock. Made by
/// [`Library::hold_for_export`](crate::Library::hold_for_export).
pub struct ExportHold {
    _lock: ArchiveLock,
    dir: ArchiveDir,
}

impl std::fmt::Debug for ExportHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExportHold")
    }
}

impl ExportHold {
    pub(crate) fn new(lock: ArchiveLock, dir: ArchiveDir) -> ExportHold {
        ExportHold { _lock: lock, dir }
    }
}

/// One message as an export reads it (M6 spec §3.2): the assembly under
/// `Caps::EXPORT` with its raw codes, and what the viewer shows only in
/// the details, uncapped.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExportMessage {
    /// Assembled outside any conversation (its intrinsic part is the same
    /// in every conversation file), extras loaded, every cap lifted.
    pub item: ExportItem,
    /// Per attachment of `item.message`, in order: whether the export may
    /// copy its file. A withheld one has no `blob` in `item` (§3.8).
    pub attachment_access: Vec<AttachmentAccess>,
    /// The other stored text when the two differ, masked (§3.8).
    pub alternative_text: AlternativeText,
    pub detail: DetailColumns,
    /// The message columns no viewer read carries, raw (slice 6d).
    pub raw_columns: MessageRawColumns,
    /// Every add and remove row targeting it, sorted (D93 rule inputs).
    pub reaction_rows: Vec<ReactionRow>,
    pub history: HistoryState,
    /// The closed history column set (§3.8).
    pub value_history: Vec<ValueHistoryEntry>,
    /// Every conversation it is a timeline item of.
    pub chats: Vec<ChatMembership>,
    pub derivation: Derivation,
}

/// The reads of one export job, inside one read transaction. Made only by
/// [`ArchiveReader::with_export_snapshot`]; it cannot outlive the closure.
pub struct ExportSnapshot<'s> {
    reader: &'s ArchiveReader,
    phase: &'s Phase<'s>,
    stmts: StmtCache<'s>,
    ctx: Ctx,
}

impl std::fmt::Debug for ExportSnapshot<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExportSnapshot")
    }
}

pub(crate) const SQL_CHAT_IDS: &str = "SELECT id FROM chat ORDER BY id";

/// Every statement this module prepares, by name (pinned in
/// `tests/query_plans.rs`).
#[cfg_attr(not(feature = "fault-injection"), allow(dead_code))]
pub(crate) fn plan_statements() -> Vec<(&'static str, String)> {
    let mut v = vec![
        ("export chat ids", SQL_CHAT_IDS.to_owned()),
        ("export blob row", crate::reader::SQL_BLOB_ROW.to_owned()),
    ];
    v.extend(cursor::plan_statements());
    v.extend(mask::plan_statements());
    v.extend(measure::plan_statements());
    v.extend(preview::plan_statements());
    v.extend(refs::plan_statements());
    v.extend(rows::plan_statements());
    v.extend(search::plan_statements());
    v
}

impl ArchiveReader {
    /// Run `f`, an export job's reads, inside ONE read transaction under
    /// the cancellable mode (M6 spec §3.2, §3.5, D124): no op budget, no
    /// deadline, stopped by `cancel` only (`Cancelled`). The transaction
    /// ends when `f` returns, so no read transaction outlives the call;
    /// the call is the job, and `hold` (this archive's lock) keeps every
    /// writer out meanwhile. Inside `f`, every other read of this reader
    /// refuses with `Internal`. A hold of another archive is refused with
    /// `Internal`.
    pub fn with_export_snapshot<T, E: From<ArchiveError>>(
        &self,
        hold: &ExportHold,
        cancel: &CancelToken,
        f: impl FnOnce(&ExportSnapshot<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        self.require_current()?;
        if hold.dir.root() != self.dir().root() {
            return Err(ArchiveError::Internal("export hold of another archive").into());
        }
        self.conn().exporting(cancel, |phase| {
            let ctx = phase.sql(Ctx::load)?;
            let snap = ExportSnapshot {
                reader: self,
                phase,
                stmts: StmtCache::new(phase.connection()),
                ctx,
            };
            f(&snap)
        })
    }

    /// The exact counts of `scope` under `opts` (M6 spec §3.7), in ONE read
    /// transaction with no op budget and no deadline, stopped only by
    /// `cancel` (`Cancelled`), pushing `progress` as it walks. It needs no
    /// hold: a draft's numbers are checked again by the job's
    /// [`ExportSnapshot::precount`], which runs the same code under the
    /// lock. Like a snapshot, the reader refuses other reads meanwhile.
    pub fn export_preview(
        &self,
        scope: &ExportScopeIds,
        opts: PreviewOptions,
        cancel: &CancelToken,
        progress: &mut dyn FnMut(PreviewProgress),
    ) -> Result<ExportPreview, ArchiveError> {
        self.require_current()?;
        self.conn().exporting(cancel, |phase| {
            let ctx = phase.sql(Ctx::load)?;
            let snap = ExportSnapshot {
                reader: self,
                phase,
                stmts: StmtCache::new(phase.connection()),
                ctx,
            };
            preview::precount(&snap, scope, opts, progress)
        })
    }

    /// The archive generation (M6 spec §3.7), in one read transaction
    /// under the reader's op budget.
    pub fn archive_generation(&self) -> Result<ArchiveGeneration, ArchiveError> {
        self.require_current()?;
        let conn = self.conn();
        conn.snapshot(|| conn.budgeted(READ_OP_BUDGET, Stage::Read, preview::generation))
    }
}

impl<'s> ExportSnapshot<'s> {
    /// Run `f` (statements from the cache) and map its SQLite error: an
    /// interrupt is `Cancelled`, never `Damaged`.
    fn run<T>(&self, f: impl FnOnce() -> rusqlite::Result<T>) -> Result<T, ArchiveError> {
        f().map_err(|e| self.phase.err(&e))
    }

    /// Stop with `Cancelled` when the job's token is set (between
    /// statements; inside one, the progress handler checks it).
    pub fn checkpoint(&self) -> Result<(), ArchiveError> {
        self.phase.checkpoint()
    }

    /// The exact counts of `scope` under `opts`, in this snapshot: the
    /// job's pre-count (M6 spec §3.7, D147). Equal to
    /// [`ArchiveReader::export_preview`] on an unchanged archive.
    pub fn precount(
        &self,
        scope: &ExportScopeIds,
        opts: PreviewOptions,
        progress: &mut dyn FnMut(PreviewProgress),
    ) -> Result<ExportPreview, ArchiveError> {
        preview::precount(self, scope, opts, progress)
    }

    /// Every message id that is a timeline row of `scope`, sorted and
    /// distinct (M6 spec §4.9).
    pub fn scope_index(&self, scope: &ExportScopeIds) -> Result<ScopeIndex, ArchiveError> {
        scope::index(self, scope)
    }

    /// The archive generation as this snapshot sees it.
    pub fn generation(&self) -> Result<ArchiveGeneration, ArchiveError> {
        self.run(|| preview::generation(&self.stmts))
    }

    /// Every conversation id, ascending.
    pub fn chat_ids(&self) -> Result<Vec<ChatId>, ArchiveError> {
        self.run(|| {
            let mut st = self.stmts.prepare(SQL_CHAT_IDS)?;
            let rows = st.query_map([], |r| Ok(ChatId::new(r.get(0)?)))?;
            rows.collect()
        })
    }

    /// A cursor over conversation `chat`'s timeline in `range`.
    pub fn timeline<'a>(&'a self, chat: ChatId, range: TimeRange) -> TimelineCursor<'a, 's> {
        TimelineCursor::new(self, Some(chat.get()), range)
    }

    /// A cursor over the messages in no conversation, in `range`.
    pub fn unassigned<'a>(&'a self, range: TimeRange) -> TimelineCursor<'a, 's> {
        TimelineCursor::new(self, None, range)
    }

    /// A cursor over `ids`, in their order.
    pub fn by_ids<'a>(&'a self, ids: &[MessageId]) -> IdCursor<'a, 's> {
        IdCursor::new(self, ids)
    }

    /// The pre-measure of each of `ids` at `depth`, in bytes (M6 spec
    /// §3.3).
    pub fn measure(&self, ids: &[MessageId], depth: ExportDepth) -> Result<Vec<u64>, ArchiveError> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            self.checkpoint()?;
            out.push(self.run(|| measure::measure_one(&self.stmts, id.get(), depth))?);
        }
        Ok(out)
    }

    /// Each of `ids`, assembled under `Caps::EXPORT` with the uncapped
    /// rows (M6 spec §3.2), one-time codes and earlier text masked unless
    /// `reveal` asks (D115). A missed cap or missing extras refuses with
    /// `Internal` (never a cut value); an id that does not exist is
    /// `AnchorNotFound`.
    pub fn assemble(
        &self,
        ids: &[MessageId],
        reveal: Reveal,
        depth: ExportDepth,
    ) -> Result<Vec<ExportMessage>, ArchiveError> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            self.checkpoint()?;
            out.push(self.assemble_one(id.get(), reveal, depth)?);
        }
        Ok(out)
    }

    fn assemble_one(
        &self,
        id: i64,
        reveal: Reveal,
        depth: ExportDepth,
    ) -> Result<ExportMessage, ArchiveError> {
        let item = self
            .run(|| export_item(&self.stmts, &self.ctx, None, id, &Caps::EXPORT, reveal))?
            .ok_or(ArchiveError::AnchorNotFound)?;
        let mut item = checked(item)?;
        let attachment_access =
            self.run(|| mask::attachment_access(&self.stmts, &item.message, reveal))?;
        mask::withhold_files(&mut item, &attachment_access);
        let alternative_text =
            self.run(|| mask::alternative_text(&self.stmts, &item.message, reveal))?;
        self.checkpoint()?;
        let (detail, derivation) = self
            .run(|| rows::detail_columns(&self.stmts, id))?
            .ok_or(ArchiveError::AnchorNotFound)?;
        let raw_columns = self
            .run(|| refs::raw_columns(&self.stmts, id))?
            .ok_or(ArchiveError::AnchorNotFound)?;
        let chats = self.run(|| rows::chats_of(&self.stmts, id))?;
        let reaction_rows = self.run(|| rows::reaction_rows(&self.stmts, id))?;
        self.checkpoint()?;
        let value_history =
            self.run(|| rows::value_history(&self.stmts, &item.message, reveal.earlier_text))?;
        let history =
            self.run(|| rows::history_state(&self.stmts, id, depth == ExportDepth::WithHistory))?;
        Ok(ExportMessage {
            item,
            attachment_access,
            alternative_text,
            detail,
            raw_columns,
            reaction_rows,
            history,
            value_history,
            chats,
            derivation,
        })
    }

    /// Message `m`'s body versions, one at a time (M6 spec §3.3), masked
    /// per `reveal` as the details mask them. Pass the reveal `m` was
    /// assembled with.
    pub fn history<'a>(
        &'a self,
        m: &ExportMessage,
        reveal: Reveal,
    ) -> Result<HistoryCursor<'a, 's>, ArchiveError> {
        HistoryCursor::new(self, &m.item.message, reveal)
    }

    /// The facts of each of `m`'s attachments, in order (uncapped, with
    /// the columns M5 leaves out). A withheld attachment (§3.8) carries no
    /// stored file: its `facts.blob` is `None`, so nothing that follows
    /// the record can open it.
    pub fn attachment_facts(
        &self,
        m: &ExportMessage,
    ) -> Result<Vec<ExportAttachment>, ArchiveError> {
        let mut out = Vec::with_capacity(m.item.message.attachments.len());
        for (i, a) in m.item.message.attachments.iter().enumerate() {
            self.checkpoint()?;
            let access = m
                .attachment_access
                .get(i)
                .copied()
                .ok_or(ArchiveError::Internal("attachment access"))?;
            let mut x = self
                .run(|| rows::attachment(&self.stmts, a.id, access))?
                .ok_or(ArchiveError::AnchorNotFound)?;
            if access.is_withheld() {
                x.facts.blob = None;
            }
            out.push(x);
        }
        Ok(out)
    }

    /// Every conversation message `id` is a timeline item of, ascending
    /// (none for a message in no conversation or one that does not exist).
    pub fn chats_of(&self, id: MessageId) -> Result<Vec<ChatId>, ArchiveError> {
        self.checkpoint()?;
        let rows = self.run(|| rows::chats_of(&self.stmts, id.get()))?;
        Ok(rows.into_iter().map(|m| m.chat).collect())
    }

    /// The replies to message `id`, ascending: keyed plain messages whose
    /// `thread_originator_guid` names it (the viewer's reply-count rule,
    /// uncapped). None for an unkeyed message.
    pub fn reply_ids(&self, id: MessageId) -> Result<Vec<MessageId>, ArchiveError> {
        self.checkpoint()?;
        self.run(|| refs::reply_ids(&self.stmts, id.get()))
    }

    /// The source identifiers of `of` (D141, owner Q5's lean): read only
    /// when an export opts in to archive and source identifiers.
    /// `AnchorNotFound` when the row does not exist.
    pub fn source_identifiers(&self, of: SourceOf) -> Result<SourceIdentifiers, ArchiveError> {
        self.checkpoint()?;
        self.run(|| refs::source_identifiers(&self.stmts, of))?
            .ok_or(ArchiveError::AnchorNotFound)
    }

    /// Conversation `chat`'s columns; `AnchorNotFound` when it does not
    /// exist.
    pub fn chat_columns(&self, chat: ChatId) -> Result<ChatColumns, ArchiveError> {
        self.run(|| rows::chat_columns(&self.stmts, chat))?
            .ok_or(ArchiveError::AnchorNotFound)
    }

    /// Handle `id`'s columns; `AnchorNotFound` when it does not exist.
    pub fn handle_columns(&self, id: HandleId) -> Result<HandleColumns, ArchiveError> {
        self.run(|| rows::handle_columns(&self.stmts, id))?
            .ok_or(ArchiveError::AnchorNotFound)
    }

    /// Whether a statement on the connection has stepped and not been
    /// reset (the `statements_stepping` test hook).
    #[cfg(feature = "fault-injection")]
    pub(crate) fn statements_stepping(&self) -> bool {
        self.phase.conn().is_busy()
    }

    /// Open blob `id`'s store file, read in this snapshot's mode (never a
    /// nested budget). The [`BlobReader`] hashes what it yields and fails
    /// its last read when the content differs from the digest.
    pub fn open_blob(&self, id: BlobId) -> Result<BlobReader, ArchiveError> {
        let row = self.run(|| blob_row(&self.stmts, id))?;
        open_blob_row(self.reader.dir(), row)
    }
}
