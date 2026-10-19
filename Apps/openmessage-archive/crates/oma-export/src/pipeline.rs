//! The export pipeline (M6 spec §4.3, `Exporter::run`).
//!
//! [`run`] does one export inside an export snapshot, from the pre-count
//! to the publish:
//!
//! 1. **Pre-count and consent** (§3.7, D147): the exact counts of the
//!    scope in this snapshot; a reveal count above the confirmed N stops
//!    the job before anything is written (`ConfirmationStale`).
//! 2. **Scope index and conversation records** (§4.9): message ordinals
//!    are ranks in the scope index; the exported conversations are those
//!    with a timeline row in scope.
//! 3. **Staging** inside the destination (a bundle folder or a
//!    `.partial-file`, §8.3).
//! 4. **For each conversation file**, its cursor's byte-sized batches are
//!    assembled under the plan's reveal and depth; each message's
//!    attachments are copied (deduplicated by blob, [`crate::attach`])
//!    while it is mapped; every record goes to **every** writer through
//!    [`FanOut`]; handles go to the writers before the first item that
//!    references them, and an item's stored versions follow it (once per
//!    message, under its first conversation file). Copied files are
//!    flushed once per batch. The cancel token is checked per item and per
//!    copied chunk.
//! 5. **Order ties** (§4.10) are computed over the whole file: the last
//!    item of a batch waits for the next batch's first, so a tie across a
//!    batch boundary is flagged on both sides. Folded reaction rows follow
//!    the batch's items.
//! 6. **At the end** the writers finish, the counts are checked against
//!    the pre-count (timeline rows, items and folded rows), then a bundle
//!    gets `README.txt`, `manifest.json` and `SHA256SUMS`, and the output
//!    is published with one no-replace rename (§8.3, §8.4).
//!
//! On any error the staging is removed before the error is returned (and
//! the sweep finds what a crash leaves). Nothing here logs.
//!
//! Not yet here: the "events" option off (refused with
//! `OptionDoesNotApply` until the scope index can leave events out without
//! a gap, slice 6h), `importSessions` (the session facts need a new
//! snapshot read, 6h), previews (6g, 6h) and the export-render helper's
//! formats (6i, 6j).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use oma_archive::{
    CancelToken, ChatId, ExportDepth, ExportMessage, ExportScopeIds, ExportSnapshot,
    PreviewOptions, TimeRange,
};

use crate::attach::{AttachCounts, AttachPolicy, Attacher};
use crate::error::{ExportError, LimitKind, PlanRefusal};
use crate::ids::Ordinals;
use crate::manifest::{
    write_provenance, DerivationFacts, ManifestBody, ManifestCounts, ManifestInput, SearchFacts,
};
use crate::map::{AttachmentFiles, ExportZone, FileOutcome, Mapper, Placement};
use crate::naming::{EntryId, TargetName};
use crate::plan::{ExportPlan, Packaging, ScopePlan};
use crate::record::{ConversationRecord, ItemRecord};
use crate::stage::{Published, Replace, Staging};
use crate::writer::{
    AboutDraft, ConversationFile, ExportContext, ExportWriter, FanOut, WriterReport,
};

/// Everything one run needs besides the snapshot and the writers.
#[derive(Debug, Clone)]
pub struct ExportRun<'a> {
    pub plan: &'a ExportPlan,
    pub zone: &'a ExportZone,
    /// The scope as ids: [`scope_ids`] for every scope but a search, whose
    /// set the shell holds.
    pub scope: &'a ExportScopeIds,
    pub export_id: EntryId,
    pub created_utc_ns: i64,
    pub app_version: &'static str,
    /// The canonical destination folder (checked by the caller, §8.2).
    pub parent: &'a Path,
    /// The bundle's or the single file's name.
    pub target: &'a TargetName,
    pub replace: Replace,
    /// `None`: the policy follows the plan ([`AttachPolicy::for_plan`]).
    pub attach: Option<AttachPolicy>,
    pub search: Option<SearchFacts>,
}

/// What one run did.
#[derive(Debug, Clone)]
pub struct ExportReport {
    pub published: Published,
    pub counts: ManifestCounts,
    pub writers: Vec<WriterReport>,
    pub attachments: AttachCounts,
}

/// Progress: items and folded rows handed to the writers so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExportProgress {
    pub rows: u64,
    pub files_written: u64,
}

/// The scope of `plan` as ids, the local dates of a range turned into
/// instants in `zone` (`ZoneTable::day_range`). `None` for a search scope
/// (the shell holds its set).
pub fn scope_ids(
    plan: &ExportPlan,
    zone: &ExportZone,
) -> Result<Option<ExportScopeIds>, ExportError> {
    let range = |from, until| -> Result<TimeRange, ExportError> {
        let (a, b) = match zone {
            ExportZone::Table(t) => t.day_range(from, until)?,
            ExportZone::Utc => {
                let day = 86_400 * 1_000_000_000_i64;
                let (a, b) = (
                    crate::time::CivilDate::days_from_epoch(from).saturating_mul(day),
                    crate::time::CivilDate::days_from_epoch(until).saturating_mul(day),
                );
                if b <= a {
                    return Err(ExportError::DateRange(
                        crate::error::DateRangeRefusal::Empty,
                    ));
                }
                (a, b)
            }
        };
        Ok(TimeRange::Dated {
            from_ns: a,
            until_ns: b,
        })
    };
    Ok(match plan.scope() {
        ScopePlan::Conversation { chat } => Some(ExportScopeIds::Conversation {
            chat: *chat,
            range: TimeRange::All,
        }),
        ScopePlan::DateRange {
            from,
            until,
            chat: Some(chat),
        } => Some(ExportScopeIds::Conversation {
            chat: *chat,
            range: range(*from, *until)?,
        }),
        ScopePlan::DateRange {
            from,
            until,
            chat: None,
        } => Some(ExportScopeIds::Archive {
            range: range(*from, *until)?,
        }),
        ScopePlan::Archive => Some(ExportScopeIds::Archive {
            range: TimeRange::All,
        }),
        ScopePlan::SearchResults { .. } => None,
    })
}

/// The files of a names-only export: never asked (the mapper decides
/// `notRequested` itself).
struct NoCopies;

impl AttachmentFiles for NoCopies {
    fn decide(
        &mut self,
        _a: &oma_archive::ExportAttachment,
        _b: oma_archive::BlobId,
    ) -> Result<FileOutcome, ExportError> {
        Err(ExportError::Limit(LimitKind::RowOrder))
    }
}

/// One conversation file: its conversation (or none) and its cursor.
enum Group {
    Timeline {
        chat: Option<ChatId>,
        range: TimeRange,
    },
    Messages {
        chat: Option<ChatId>,
        ids: Vec<oma_archive::MessageId>,
    },
}

impl Group {
    fn chat(&self) -> Option<ChatId> {
        match self {
            Group::Timeline { chat, .. } | Group::Messages { chat, .. } => *chat,
        }
    }
}

enum Cursor<'a, 's> {
    Timeline(oma_archive::TimelineCursor<'a, 's>),
    Ids(oma_archive::IdCursor<'a, 's>),
}

impl Cursor<'_, '_> {
    fn next_batch(&mut self) -> Result<Option<oma_archive::CursorBatch>, ExportError> {
        Ok(match self {
            Cursor::Timeline(c) => c.next_batch()?,
            Cursor::Ids(c) => c.next_batch()?,
        })
    }
}

fn cursor<'a, 's>(snap: &'a ExportSnapshot<'s>, g: &Group, depth: ExportDepth) -> Cursor<'a, 's> {
    match g {
        Group::Timeline {
            chat: Some(c),
            range,
        } => Cursor::Timeline(snap.timeline(*c, *range).with_depth(depth)),
        Group::Timeline { chat: None, range } => {
            Cursor::Timeline(snap.unassigned(*range).with_depth(depth))
        }
        Group::Messages { ids, .. } => Cursor::Ids(snap.by_ids(ids).with_depth(depth)),
    }
}

/// The conversation files of `scope`, in order, empty ones left out.
fn groups(snap: &ExportSnapshot<'_>, scope: &ExportScopeIds) -> Result<Vec<Group>, ExportError> {
    // A batch of one id is enough to know a file is not empty.
    let one = oma_archive::BatchLimits::new(1, u64::MAX);
    let non_empty = |g: &Group| -> Result<bool, ExportError> {
        let first = match g {
            Group::Timeline {
                chat: Some(c),
                range,
            } => snap.timeline(*c, *range).with_limits(one).next_batch()?,
            Group::Timeline { chat: None, range } => {
                snap.unassigned(*range).with_limits(one).next_batch()?
            }
            Group::Messages { ids, .. } => snap.by_ids(ids).with_limits(one).next_batch()?,
        };
        Ok(first.is_some())
    };
    let mut out = Vec::new();
    match scope {
        ExportScopeIds::Conversation { chat, range } => {
            // One conversation is always written, even when empty.
            out.push(Group::Timeline {
                chat: Some(*chat),
                range: *range,
            });
        }
        ExportScopeIds::Archive { range } => {
            for c in snap.chat_ids()? {
                let g = Group::Timeline {
                    chat: Some(c),
                    range: *range,
                };
                if non_empty(&g)? {
                    out.push(g);
                }
            }
            let g = Group::Timeline {
                chat: None,
                range: *range,
            };
            if non_empty(&g)? {
                out.push(g);
            }
        }
        ExportScopeIds::Messages(groups) => {
            for g in groups {
                if !g.messages.is_empty() {
                    out.push(Group::Messages {
                        chat: g.chat,
                        ids: g.messages.clone(),
                    });
                }
            }
        }
        _ => return Err(ExportError::Limit(LimitKind::UnmappedVariant)),
    }
    Ok(out)
}

/// The run's mutable state.
struct Job<'r, 'a, 's> {
    run: &'r ExportRun<'a>,
    snap: &'r ExportSnapshot<'s>,
    cancel: &'r CancelToken,
    mapper: Mapper,
    attacher: Option<Attacher>,
    /// Taken by `finish`.
    fan: Option<FanOut>,
    staging: Staging,
    handles_sent: usize,
    streamed: BTreeSet<oma_archive::MessageId>,
    derivation: DerivationFacts,
    progress: ExportProgress,
}

impl Job<'_, '_, '_> {
    fn fan(&mut self) -> Result<&mut FanOut, ExportError> {
        self.fan
            .as_mut()
            .ok_or(ExportError::Limit(LimitKind::RowOrder))
    }

    fn flush_handles(&mut self) -> Result<(), ExportError> {
        let all: Vec<oma_archive::HandleId> = self.mapper.ordinals().handles.in_order().to_vec();
        for h in all.iter().skip(self.handles_sent) {
            let rec = self.mapper.handle(self.snap, *h)?;
            let fan = self
                .fan
                .as_mut()
                .ok_or(ExportError::Limit(LimitKind::RowOrder))?;
            fan.handle(&rec, &mut self.staging)?;
        }
        self.handles_sent = all.len();
        Ok(())
    }

    fn map(&mut self, m: &ExportMessage, at: Placement) -> Result<ItemRecord, ExportError> {
        let rec = match &mut self.attacher {
            Some(a) => {
                let mut files = a.session(self.snap, &mut self.staging, self.cancel);
                self.mapper.item(self.snap, m, at, &mut files)?
            }
            None => self.mapper.item(self.snap, m, at, &mut NoCopies)?,
        };
        if let Some(a) = &self.attacher {
            self.progress.files_written = a.counts().files_written;
        }
        Ok(rec)
    }

    fn emit(&mut self, m: &ExportMessage, at: Placement) -> Result<(), ExportError> {
        let rec = self.map(m, at)?;
        self.flush_handles()?;
        let fan = self
            .fan
            .as_mut()
            .ok_or(ExportError::Limit(LimitKind::RowOrder))?;
        fan.item(&rec, &mut self.staging, self.cancel)?;
        self.progress.rows += 1;
        let id = m.item.message.id;
        if self.streamed.insert(id) {
            let d = rec.derivation;
            *self
                .derivation
                .derived_by
                .entry(d.derived_by.to_string())
                .or_insert(0) += 1;
            if let Some(v) = d.decoder_version {
                *self
                    .derivation
                    .decoder_version
                    .entry(v.to_string())
                    .or_insert(0) += 1;
            }
            if self.run.plan.depth() == ExportDepth::WithHistory {
                let mut h = self.snap.history(m, self.run.plan.reveal())?;
                while let Some((v, codes)) = h.next_coded()? {
                    let rec = self.mapper.version(id, &v, codes)?;
                    let fan = self
                        .fan
                        .as_mut()
                        .ok_or(ExportError::Limit(LimitKind::RowOrder))?;
                    fan.version(&rec, &mut self.staging)?;
                }
            }
        }
        Ok(())
    }

    fn group(&mut self, g: &Group, conv: Option<&ConversationRecord>) -> Result<(), ExportError> {
        let file = match conv {
            Some(c) => ConversationFile::Conversation(c),
            None => ConversationFile::NoConversation,
        };
        let fan = self
            .fan
            .as_mut()
            .ok_or(ExportError::Limit(LimitKind::RowOrder))?;
        fan.begin_conversation(file, &mut self.staging)?;
        let (reveal, depth) = (self.run.plan.reveal(), self.run.plan.depth());
        let chat = g.chat();
        let snap = self.snap;
        let mut cur = cursor(snap, g, depth);
        // The last item of each batch waits for the next batch's first
        // (§4.10: a tie across a batch boundary).
        let mut pending: Option<ExportMessage> = None;
        let mut prev: Option<Option<i64>> = None;
        let time = |m: &ExportMessage| m.item.message.date.utc_ns;
        let tie = |t: Option<i64>, before: Option<Option<i64>>, after: Option<Option<i64>>| {
            t.is_some() && (before.flatten() == t || after.flatten() == t)
        };
        while let Some(batch) = cur.next_batch()? {
            let mut list: Vec<ExportMessage> = pending.take().into_iter().collect();
            list.extend(snap.assemble(&batch.items, reveal, depth)?);
            let times: Vec<Option<i64>> = list.iter().map(time).collect();
            let last = list.pop();
            for (i, m) in list.iter().enumerate() {
                let t = times.get(i).copied().flatten();
                let after = times.get(i + 1).copied();
                let at = Placement {
                    conversation: chat,
                    folded_into: None,
                    order_tie: tie(t, prev, after),
                };
                self.emit(m, at)?;
                prev = Some(t);
            }
            pending = last;
            for (row, target) in &batch.folded {
                for m in snap.assemble(&[*row], reveal, depth)? {
                    let at = Placement {
                        conversation: chat,
                        folded_into: Some(*target),
                        order_tie: false,
                    };
                    self.emit(&m, at)?;
                }
            }
            self.staging.sync_batch()?;
        }
        if let Some(m) = pending.take() {
            let at = Placement {
                conversation: chat,
                folded_into: None,
                order_tie: tie(time(&m), prev, None),
            };
            self.emit(&m, at)?;
        }
        self.staging.sync_batch()?;
        let fan = self
            .fan
            .as_mut()
            .ok_or(ExportError::Limit(LimitKind::RowOrder))?;
        fan.end_conversation(&mut self.staging)?;
        Ok(())
    }
}

fn sum(reports: &[WriterReport], key: &str) -> u64 {
    reports
        .iter()
        .filter_map(|r| r.counts.get(key))
        .fold(0u64, |a, b| a.saturating_add(*b))
}

/// Run one export of `run.plan` inside `snap` with `writers` (one per
/// format of the plan), publishing into `run.parent`. See the module
/// documentation for the steps.
pub fn run(
    snap: &ExportSnapshot<'_>,
    run: &ExportRun<'_>,
    writers: Vec<Box<dyn ExportWriter>>,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(ExportProgress),
) -> Result<ExportReport, ExportError> {
    let plan = run.plan;
    if !plan.options().events {
        return Err(ExportError::Plan(PlanRefusal::OptionDoesNotApply));
    }
    // 1. Pre-count and consent, under the lock, in this snapshot.
    let pre = snap.precount(
        run.scope,
        PreviewOptions::new(plan.reveal(), plan.depth()),
        &mut |_| {},
    )?;
    plan.check_confirmation(
        pre.messages_with_flagged_code,
        pre.messages_with_earlier_content,
    )?;
    if cancel.is_cancelled() {
        return Err(ExportError::Cancelled);
    }
    // 2. Scope index, conversation files and records.
    let index = snap.scope_index(run.scope)?;
    let groups = groups(snap, run.scope)?;
    let chats: Vec<ChatId> = groups.iter().filter_map(Group::chat).collect();
    let mut mapper = Mapper::new(
        Ordinals::new(index, chats.clone(), plan.options().identifiers),
        run.zone.clone(),
        plan.record_options(),
    );
    let mut conversations: BTreeMap<ChatId, ConversationRecord> = BTreeMap::new();
    for c in &chats {
        conversations.insert(*c, mapper.conversation(snap, *c)?);
    }
    // 3. Staging.
    let staging = match plan.packaging() {
        Packaging::Bundle => Staging::bundle(run.parent)?,
        Packaging::SingleFile => Staging::single_file(run.parent)?,
    };
    let attach = match (plan.packaging(), plan.record_options().names_only) {
        (_, true) => None,
        (Packaging::SingleFile, false) => return Err(ExportError::Limit(LimitKind::RowOrder)),
        (Packaging::Bundle, false) => Some(
            run.attach
                .or_else(|| AttachPolicy::for_plan(plan, run.parent, run.target))
                .ok_or(ExportError::Limit(LimitKind::RowOrder))?,
        ),
    };
    let mut job = Job {
        run,
        snap,
        cancel,
        mapper,
        attacher: attach.map(Attacher::new),
        fan: Some(FanOut::new(writers, plan)?),
        staging,
        handles_sent: 0,
        streamed: BTreeSet::new(),
        derivation: DerivationFacts::default(),
        progress: ExportProgress::default(),
    };
    let result = body(&mut job, &groups, &conversations, &pre, progress);
    match result {
        Ok(report) => Ok(report),
        Err(e) => {
            let _ = job.staging.remove();
            Err(e)
        }
    }
}

fn body(
    job: &mut Job<'_, '_, '_>,
    groups: &[Group],
    conversations: &BTreeMap<ChatId, ConversationRecord>,
    pre: &oma_archive::ExportPreview,
    progress: &mut dyn FnMut(ExportProgress),
) -> Result<ExportReport, ExportError> {
    let run = job.run;
    let plan = run.plan;
    let ctx = ExportContext {
        plan,
        zone: run.zone,
        export_id: run.export_id,
        created_utc_ns: run.created_utc_ns,
    };
    job.fan
        .as_mut()
        .ok_or(ExportError::Limit(LimitKind::RowOrder))?
        .begin(&ctx, &mut job.staging)?;
    // The conversation records' participants are the first handles.
    job.flush_handles()?;
    for g in groups {
        let conv = match g.chat() {
            Some(c) => Some(
                conversations
                    .get(&c)
                    .ok_or(ExportError::Limit(LimitKind::ScopeMismatch))?,
            ),
            None => None,
        };
        job.group(g, conv)?;
        progress(job.progress);
    }
    // 6. Counts, checked against the pre-count.
    let totals = job.fan()?.totals();
    if totals.items + totals.folded_rows != pre.timeline_rows
        || totals.folded_rows != pre.folded_reaction_rows
    {
        return Err(ExportError::Limit(LimitKind::RowCountMismatch));
    }
    let attach = job
        .attacher
        .as_ref()
        .map(Attacher::counts)
        .unwrap_or_default();
    let mut counts = ManifestCounts::from_records(job.mapper.counts());
    // Conversations, not files: the no-conversation file is not one.
    counts.conversations = conversations.len() as u64;
    counts.timeline_rows = pre.timeline_rows;
    counts.distinct_messages = pre.distinct_messages;
    counts.cross_conversation_rows = pre.timeline_rows.saturating_sub(pre.distinct_messages);
    counts.folded_reaction_rows = totals.folded_rows;
    counts.items = totals.items;
    counts.undated_excluded = pre.undated_excluded;
    counts.attachments.names_shortened = attach.names_shortened;
    counts.attachments.motw_not_written = attach.motw_not_written;
    if counts.attachments.files_written != attach.files_written {
        return Err(ExportError::Limit(LimitKind::RowCountMismatch));
    }
    let input = |counts: ManifestCounts, job: &Job<'_, '_, '_>| ManifestInput {
        plan,
        zone: run.zone,
        export_id: run.export_id,
        created_utc_ns: run.created_utc_ns,
        app_version: run.app_version,
        conversations: conversations.len() as u64,
        counts,
        derivation: job.derivation.clone(),
        search: run.search.clone(),
        import_sessions: Vec::new(),
    };
    let about = ManifestBody::new(input(counts.clone(), job))?;
    let fan = job
        .fan
        .take()
        .ok_or(ExportError::Limit(LimitKind::RowOrder))?;
    let reports = fan.finish(&AboutDraft { body: &about }, &mut job.staging)?;
    counts.hidden_characters_marked = sum(&reports, "hiddenCharactersMarked");
    counts.controls_escaped = sum(&reports, "controlsEscaped");
    counts.combining_runs_limited = sum(&reports, "combiningRunsLimited");
    counts.check_identities()?;
    let body = ManifestBody::new(input(counts.clone(), job))?;
    if plan.packaging() == Packaging::Bundle {
        write_provenance(&mut job.staging, plan, &body)?;
    }
    if job.cancel.is_cancelled() {
        return Err(ExportError::Cancelled);
    }
    let published = job.staging.publish(run.target, run.replace)?;
    Ok(ExportReport {
        published,
        counts,
        writers: reports,
        attachments: attach,
    })
}
