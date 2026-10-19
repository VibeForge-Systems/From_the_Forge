//! The writer trait and the fan-out (M6 spec §4.3).
//!
//! Every format is an [`ExportWriter`]: it receives the conversation
//! records and the canonical [`ItemRecord`]s, in order, and writes them
//! through [`Staging`] (a `StagedFile` per output, hashed while written).
//! [`FanOut`] hands every call to **every** writer of the plan, so all
//! formats render the same records, and enforces the order:
//!
//! ```text
//! begin  handle*  (begin_conversation  (handle* item version*)*  end_conversation)*  finish
//! ```
//!
//! A handle is handed over once, when the mapping first references it
//! (the conversation records' participants first, then senders and
//! reaction rows as items arrive), always before the first item that
//! names it, so a human-readable writer can print the address. An item's
//! stored body versions (history included) follow it.
//!
//! It checks the cancel token before every item, counts the items and
//! folded reaction rows of each conversation file, and refuses a writer
//! whose report disagrees with what it was given (`RowCountMismatch`, the
//! manifest's "counts equal the writer reports"). The writers themselves
//! are [`txt`] (slice 6e2), [`json`] and [`csv`] (6f), then HTML (6g),
//! and XLSX and PDF through the export-render helper (6i, 6j).
//!
//! The sentence lookup ([`Words`]), the phrase table and the placeholder
//! form are shared by the writers that print words (TXT, CSV).

pub mod csv;
pub mod json;
pub mod txt;

use std::collections::BTreeMap;

use oma_archive::CancelToken;

use crate::codes::{Code, ExportCode, UNRECOGNISED_TOKEN};
use crate::error::{ExportError, LimitKind};
use crate::hidden::{MARKER_CLOSE, MARKER_OPEN};
use crate::manifest::ManifestBody;
use crate::map::ExportZone;
use crate::naming::EntryId;
use crate::plan::{ExportPlan, Format};
use crate::record::{ConversationRecord, HandleRecord, ItemRecord, VersionRecord};
use crate::stage::Staging;
use crate::words::{self, WordsTable};

/// Sentences by (table, code), built once from `words.rs`.
pub(crate) struct Words {
    by: BTreeMap<(&'static str, i64), &'static str>,
}

impl Words {
    pub(crate) fn new() -> Words {
        let mut by = BTreeMap::new();
        for t in words::tables() {
            let WordsTable { name, entries, .. } = t;
            for e in entries {
                by.insert((name, e.code), e.sentence);
            }
        }
        Words { by }
    }

    /// The sentence of `c` in `table`; a number this build does not know
    /// says so with its number.
    pub(crate) fn say(&self, table: &'static str, c: Code) -> String {
        match self.by.get(&(table, c.code)) {
            Some(s) if !c.is_unrecognised() => (*s).to_owned(),
            _ => format!(
                "Recorded with a code this app does not know ({table} {})",
                c.code
            ),
        }
    }
}

/// A fixed phrase of `words.rs` by its key.
pub(crate) fn phrase(key: &str) -> &'static str {
    words::PHRASES
        .iter()
        .find(|p| p.key == key)
        .map_or("", |p| p.sentence)
}

/// Is `c` the code of `v`?
pub(crate) fn is<T: ExportCode>(c: Code, v: T) -> bool {
    c.code == v.export_code() && c.token != UNRECOGNISED_TOKEN && c.token == v.token()
}

/// A placeholder of the export, in the reserved marker form `⟨…⟩`.
pub(crate) fn placeholder(s: &str) -> String {
    format!("{MARKER_OPEN}{s}{MARKER_CLOSE}")
}

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
    /// A handle, before the first item that references it. Writers that
    /// collect handles (single-file JSON) or print addresses (TXT, HTML,
    /// PDF) keep it; the default ignores it.
    fn handle(&mut self, h: &HandleRecord, out: &mut Staging) -> Result<(), ExportError> {
        let _ = (h, out);
        Ok(())
    }
    /// One stored body version of the item just written (history
    /// included). The default ignores it.
    fn version(&mut self, v: &VersionRecord, out: &mut Staging) -> Result<(), ExportError> {
        let _ = (v, out);
        Ok(())
    }
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

    /// Hand `h` to every writer (after `begin`, before `finish`).
    pub fn handle(&mut self, h: &HandleRecord, out: &mut Staging) -> Result<(), ExportError> {
        if self.phase == Phase::New {
            return Err(order());
        }
        for w in &mut self.writers {
            w.handle(h, out)?;
        }
        Ok(())
    }

    /// Hand a body version of the last item to every writer.
    pub fn version(&mut self, v: &VersionRecord, out: &mut Staging) -> Result<(), ExportError> {
        if self.phase != Phase::InFile {
            return Err(order());
        }
        for w in &mut self.writers {
            w.version(v, out)?;
        }
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
