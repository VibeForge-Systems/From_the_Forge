//! The writer trait and the fan-out (M6 spec §4.3).
//!
//! Every format is an [`ExportWriter`]: it receives the conversation
//! records and the canonical [`ItemRecord`]s, in order, and writes them
//! through [`Staging`] (a `StagedFile` per output, hashed while written).
//! [`FanOut`] hands every call to **every** writer of the plan, so all
//! formats render the same records, and enforces the order:
//!
//! ```text
//! begin  (begin_conversation  item*  end_conversation)*  finish
//! ```
//!
//! It checks the cancel token before every item, counts the items and
//! folded reaction rows of each conversation file, and refuses a writer
//! whose report disagrees with what it was given (`RowCountMismatch`, the
//! manifest's "counts equal the writer reports"). The writers themselves
//! arrive with slices 6e2 (TXT), 6f (JSON, CSV), 6g (HTML) and 6i/6j
//! (XLSX, PDF through the export-render helper).

use std::collections::BTreeMap;

use oma_archive::CancelToken;

use crate::error::{ExportError, LimitKind};
use crate::manifest::ManifestBody;
use crate::map::ExportZone;
use crate::naming::EntryId;
use crate::plan::{ExportPlan, Format};
use crate::record::{ConversationRecord, ItemRecord};
use crate::stage::Staging;

/// What every writer knows before its first record.
#[derive(Debug, Clone, Copy)]
pub struct ExportContext<'a> {
    pub plan: &'a ExportPlan,
    pub zone: &'a ExportZone,
    /// The export's id (128 random bits; the manifest's `exportId`).
    pub export_id: EntryId,
    /// When the export started, Unix ns (UTC).
    pub created_utc_ns: i64,
}

/// The conversation file a run of items belongs to.
#[derive(Debug, Clone, Copy)]
pub enum ConversationFile<'a> {
    Conversation(&'a ConversationRecord),
    /// The messages in no conversation (`no-conversation`).
    NoConversation,
}

/// What one conversation file held, counted by the fan-out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConversationTotals {
    /// Timeline items shown as items (folded reaction rows excluded).
    pub items: u64,
    /// Reaction rows folded into a target (chips in TXT, HTML and PDF;
    /// rows with `foldedInto` in the structured formats).
    pub folded_rows: u64,
}

/// What a single file's "About this export" says (spec §7.3): the
/// manifest's fields without `files`.
#[derive(Debug, Clone, Copy)]
pub struct AboutDraft<'a> {
    pub body: &'a ManifestBody,
}

/// What a writer reports when it finishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterReport {
    pub format: Format,
    pub conversation_files: u64,
    pub items: u64,
    pub folded_rows: u64,
    /// Format-specific counts for the manifest (`neutralisedCells`,
    /// `continuationRows`, …), by their manifest key.
    pub counts: BTreeMap<&'static str, u64>,
}

impl WriterReport {
    /// An empty report for `format`.
    pub fn new(format: Format) -> WriterReport {
        WriterReport {
            format,
            conversation_files: 0,
            items: 0,
            folded_rows: 0,
            counts: BTreeMap::new(),
        }
    }
}

/// One output format (spec §4.3).
pub trait ExportWriter: Send {
    fn format(&self) -> Format;
    fn begin(&mut self, ctx: &ExportContext<'_>, out: &mut Staging) -> Result<(), ExportError>;
    fn begin_conversation(
        &mut self,
        c: ConversationFile<'_>,
        out: &mut Staging,
    ) -> Result<(), ExportError>;
    fn item(&mut self, it: &ItemRecord, out: &mut Staging) -> Result<(), ExportError>;
    fn end_conversation(
        &mut self,
        totals: &ConversationTotals,
        out: &mut Staging,
    ) -> Result<(), ExportError>;
    /// Write the format's own "About" part (single-file mode) and return
    /// its counts. Every file the writer opened must be closed by now.
    fn finish(
        self: Box<Self>,
        about: &AboutDraft<'_>,
        out: &mut Staging,
    ) -> Result<WriterReport, ExportError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    New,
    Begun,
    InFile,
}

/// Every writer of one export, driven together.
pub struct FanOut {
    writers: Vec<Box<dyn ExportWriter>>,
    phase: Phase,
    file: ConversationTotals,
    totals: WriterTotals,
}

impl std::fmt::Debug for FanOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FanOut")
            .field("writers", &self.writers.len())
            .field("phase", &self.phase)
            .field("totals", &self.totals)
            .finish()
    }
}

/// The fan-out's own counts over the whole export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WriterTotals {
    pub conversation_files: u64,
    pub items: u64,
    pub folded_rows: u64,
}

fn order() -> ExportError {
    ExportError::Limit(LimitKind::RowOrder)
}

impl FanOut {
    /// One writer per format of `plan`, exactly.
    pub fn new(
        writers: Vec<Box<dyn ExportWriter>>,
        plan: &ExportPlan,
    ) -> Result<FanOut, ExportError> {
        let mut formats: Vec<Format> = writers.iter().map(|w| w.format()).collect();
        formats.sort_unstable();
        if formats != plan.formats() {
            return Err(order());
        }
        Ok(FanOut {
            writers,
            phase: Phase::New,
            file: ConversationTotals::default(),
            totals: WriterTotals::default(),
        })
    }

    pub fn totals(&self) -> WriterTotals {
        self.totals
    }

    pub fn begin(&mut self, ctx: &ExportContext<'_>, out: &mut Staging) -> Result<(), ExportError> {
        if self.phase != Phase::New {
            return Err(order());
        }
        for w in &mut self.writers {
            w.begin(ctx, out)?;
        }
        self.phase = Phase::Begun;
        Ok(())
    }

    pub fn begin_conversation(
        &mut self,
        c: ConversationFile<'_>,
        out: &mut Staging,
    ) -> Result<(), ExportError> {
        if self.phase != Phase::Begun {
            return Err(order());
        }
        for w in &mut self.writers {
            w.begin_conversation(c, out)?;
        }
        self.phase = Phase::InFile;
        self.file = ConversationTotals::default();
        self.totals.conversation_files += 1;
        Ok(())
    }

    /// Hand `it` to every writer, after checking `cancel`.
    pub fn item(
        &mut self,
        it: &ItemRecord,
        out: &mut Staging,
        cancel: &CancelToken,
    ) -> Result<(), ExportError> {
        if cancel.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        if self.phase != Phase::InFile {
            return Err(order());
        }
        for w in &mut self.writers {
            w.item(it, out)?;
        }
        if it.context.folded_into.is_some() {
            self.file.folded_rows += 1;
            self.totals.folded_rows += 1;
        } else {
            self.file.items += 1;
            self.totals.items += 1;
        }
        Ok(())
    }

    pub fn end_conversation(
        &mut self,
        out: &mut Staging,
    ) -> Result<ConversationTotals, ExportError> {
        if self.phase != Phase::InFile {
            return Err(order());
        }
        let totals = self.file;
        for w in &mut self.writers {
            w.end_conversation(&totals, out)?;
        }
        self.phase = Phase::Begun;
        Ok(totals)
    }

    /// Finish every writer, in format order, and check each report against
    /// the fan-out's counts.
    pub fn finish(
        self,
        about: &AboutDraft<'_>,
        out: &mut Staging,
    ) -> Result<Vec<WriterReport>, ExportError> {
        if self.phase != Phase::Begun {
            return Err(order());
        }
        let t = self.totals;
        let mut writers = self.writers;
        writers.sort_by_key(|w| w.format());
        let mut reports = Vec::with_capacity(writers.len());
        for w in writers {
            let format = w.format();
            let r = w.finish(about, out)?;
            let agrees = r.format == format
                && r.conversation_files == t.conversation_files
                && r.items == t.items
                && r.folded_rows == t.folded_rows;
            if !agrees {
                return Err(ExportError::Limit(LimitKind::RowCountMismatch));
            }
            reports.push(r);
        }
        if out.open_files() != 0 {
            return Err(ExportError::Limit(LimitKind::FileStillOpen));
        }
        Ok(reports)
    }
}
