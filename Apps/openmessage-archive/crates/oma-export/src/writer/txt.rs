//! The TXT writer (M6 spec §5.4, D138).
//!
//! One file per conversation file (`txt/c0001.txt`,
//! `txt/no-conversation.txt`), or the single file. UTF-8 without a BOM, LF
//! (CRLF as an option).
//!
//! **The line grammar** (`scripts/oracles/txt_grammar.py` parses it back):
//!
//! ```text
//! file      = header-block LF body footer
//! header    = first line, "Conversation N: <name>" or "No conversation: …",
//!             "Addresses recorded … : N", ("Other recorded name of this
//!             conversation: <name>")*, "Times: <zone> (…)"
//! body      = (LF day-line | LF "==== Undated ====" | item)*
//! item      = item-line (label-line | content-line)*
//! item-line = "[" (time | "Undated" | "No usable time") "] [item " N "]"
//!             [" " sender] (" [" tag "]")*                           (column 0)
//! label     = 4 spaces, then a non-space
//! content   = 6 spaces, then recorded text (possibly empty)
//! footer    = LF "==== End of conversation file ====" …
//! ```
//!
//! - **Header fields are content, so they are escaped**
//!   ([`MarkPolicy::text_header`]): the sender, the conversation name,
//!   attachment names, every other recorded value on a tag or label line.
//!   A line break in them (LF, CR, U+2028, U+2029, U+0085, VT, FF) becomes
//!   a visible `⟨U+XXXX⟩`; hidden characters are always marked; and so are
//!   `[` and `]` (`⟨U+005B⟩`, `⟨U+005D⟩`), so a recorded address can never
//!   forge a tag on an item line (an addition to §5.4: brackets are the
//!   grammar's delimiters there).
//! - **Recorded text** (bodies, earlier text, versions) is split at its
//!   line breaks (CRLF, LF, CR, U+2028, U+2029, U+0085), each line indented
//!   by six spaces: a message holding `\n[2024-… ] Alice` cannot fake an
//!   item boundary, and no raw CR is ever written from content. Controls
//!   are always escaped; hidden characters are marked when the option is
//!   on (the default); a literal `⟨` is `⟨U+27E8⟩`; a combining run longer
//!   than 16 marks keeps its first 16 and `⟨+N combining marks⟩` (§4.7).
//!   A bidi paragraph ends at each line break (UAX #9), so a control in a
//!   body cannot reorder an item line.
//! - **Segments** keep their kinds: placeholders are `⟨one-time code
//!   hidden⟩`, `⟨Attachment N⟩`, `⟨Inline emoji image (not shown)⟩` and
//!   `⟨Attachment placeholder not matched to a file⟩`, never
//!   hidden-character markers; a transcription is written under its
//!   label, "Transcription recorded on the phone". Styles are dropped.
//! - **Edits and unsends** are tags on the item line (`[Edited: part 1]`,
//!   `[Unsent by the sender: part 2 of 3]`). The record's segments carry no
//!   part index, so the body is written whole, not per part (a deviation
//!   from §5.4's per-part rendering, recorded in `architecture.md`).
//! - **Reaction rows folded into their target** are not items here: they
//!   are the target's derived reaction lines, and each file's footer
//!   counts them.
//! - **Day separators** `==== 2024-03-10 ====` are in the export's zone
//!   (UTC dates for instants outside the zone window), and the undated
//!   section comes last under `==== Undated ====`.
//! - **The footer** counts the file's items and folded rows, then the
//!   legend (`words::txt_legend`); a single file then carries "About this
//!   export" (§7.3), the manifest's fields without `files`.
//!
//! Every sentence comes from `words.rs`. Nothing here logs.

use std::collections::BTreeMap;
use std::io::Write;

use crate::codes::{AttachmentAccessCode, CardStateCode, ChipSenderCode, SegmentKindCode};
use crate::error::{ExportError, LimitKind};
use crate::hidden::{self, MarkCounts, MarkPolicy};
use crate::ids::Ordinal;
use crate::map::ExportZone;
use crate::naming::{BundlePath, FileExt, FileStem};
use crate::plan::{Clock, Format, LineEndings, Packaging};
use crate::record::*;
use crate::stage::{StagedFile, Staging};
use crate::time::{CivilDate, HumanStyle};
use crate::words::txt_legend;

use super::shared::{
    date_text, flatten, is_undated, item_tags, sender_tags, time_text, Comma, SenderTags,
};
use super::{
    is, phrase, placeholder, AboutDraft, ConversationFile, ConversationTotals, ExportContext,
    ExportWriter, Words, WriterReport,
};

/// The indent of a label line.
pub const LABEL: &str = "    ";
/// The indent of a line of recorded text.
pub const TEXT: &str = "      ";
/// The footer's first line.
pub const FOOTER: &str = "==== End of conversation file ====";
/// The undated section's heading.
pub const UNDATED: &str = "==== Undated ====";
/// The prefix of a header line naming another recorded name of the
/// conversation (its name history).
pub const OTHER_NAME: &str = "Other recorded name of this conversation: ";
/// The time field of an item in the undated section.
pub const UNDATED_TIME: &str = "Undated";
/// The time field of an item sorted among the dated ones whose own
/// recorded time is not usable (an unknown unit, for example).
pub const NO_TIME: &str = "No usable time";
/// The single file's "About" heading.
pub const ABOUT: &str = "==== About this export ====";

/// What one conversation file has written so far.
#[derive(Debug, Default)]
struct FileState {
    day: Option<CivilDate>,
    undated: bool,
    items: u64,
    folded: u64,
}

/// The TXT writer.
pub struct TxtWriter {
    nl: &'static str,
    markers: bool,
    style: HumanStyle,
    single: bool,
    zone_name: String,
    words: Words,
    /// Handle ordinal → its address, escaped as a header field.
    handles: BTreeMap<Ordinal, String>,
    file: Option<StagedFile>,
    state: FileState,
    counts: MarkCounts,
    brackets_escaped: u64,
    report: WriterReport,
    /// The last item written (folded rows are not items): the item a
    /// stored version belongs under.
    last_item: Option<Ordinal>,
}

impl std::fmt::Debug for TxtWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxtWriter")
            .field("single", &self.single)
            .field("handles", &self.handles.len())
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

impl Default for TxtWriter {
    fn default() -> TxtWriter {
        TxtWriter::new()
    }
}

fn io(e: std::io::Error) -> ExportError {
    ExportError::io(&e)
}

fn order() -> ExportError {
    ExportError::Limit(LimitKind::RowOrder)
}

/// `true` at a line break of recorded text.
fn is_break(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}' | '\u{85}')
}

/// `text` split at its line breaks (CRLF counts once). The PDF block
/// model splits recorded text into paragraphs with the same rule.
pub(crate) fn split_lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = text.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if is_break(c) {
            out.push(text.get(start..i).unwrap_or(""));
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = it.peek() {
                    it.next();
                    end = j + 1;
                }
            }
            start = end;
        }
    }
    out.push(text.get(start..).unwrap_or(""));
    out
}

/// A piece of a recorded text: text to mark, or a placeholder of ours.
enum Run<'a> {
    Text(&'a str),
    Placeholder(String),
}

impl TxtWriter {
    pub fn new() -> TxtWriter {
        TxtWriter {
            nl: "\n",
            markers: true,
            style: HumanStyle {
                h23: true,
                seconds: true,
            },
            single: false,
            zone_name: "UTC".to_owned(),
            words: Words::new(),
            handles: BTreeMap::new(),
            file: None,
            state: FileState::default(),
            counts: MarkCounts::default(),
            brackets_escaped: 0,
            report: WriterReport::new(Format::Txt),
            last_item: None,
        }
    }

    fn out(&mut self) -> Result<&mut StagedFile, ExportError> {
        self.file.as_mut().ok_or_else(order)
    }

    fn line(&mut self, s: &str) -> Result<(), ExportError> {
        let nl = self.nl;
        let f = self.out()?;
        f.write_all(s.as_bytes()).map_err(io)?;
        f.write_all(nl.as_bytes()).map_err(io)
    }

    /// A header field: every control and line break marked, hidden
    /// characters always marked, and `[` / `]` escaped.
    fn header(&mut self, s: &str) -> String {
        let (t, n) = hidden::mark(s, MarkPolicy::text_header());
        self.counts.add(n);
        let mut out = String::with_capacity(t.len());
        for c in t.chars() {
            if c == '[' || c == ']' {
                out.push_str(&hidden::marker(c));
                self.brackets_escaped += 1;
            } else {
                out.push(c);
            }
        }
        out
    }

    fn header_value(&mut self, v: &TextValue) -> Option<String> {
        v.text.as_ref().map(|t| self.header(t.as_str()))
    }

    /// The lines of recorded text made of `runs`: text split at line
    /// breaks and marked per line, placeholders kept as they are.
    fn text_lines(&mut self, runs: &[Run<'_>]) -> Vec<String> {
        let policy = MarkPolicy::text_body(self.markers);
        let mut lines = vec![String::new()];
        for r in runs {
            match r {
                Run::Placeholder(p) => {
                    if let Some(l) = lines.last_mut() {
                        l.push_str(p);
                    }
                }
                Run::Text(t) => {
                    for (i, part) in split_lines(t).into_iter().enumerate() {
                        if i > 0 {
                            lines.push(String::new());
                        }
                        let (m, n) = hidden::mark(part, policy);
                        self.counts.add(n);
                        if let Some(l) = lines.last_mut() {
                            l.push_str(&m);
                        }
                    }
                }
            }
        }
        lines
    }

    fn write_text(&mut self, label: &str, runs: &[Run<'_>]) -> Result<(), ExportError> {
        self.line(&format!("{LABEL}{label}"))?;
        for l in self.text_lines(runs) {
            self.line(&format!("{TEXT}{l}"))?;
        }
        Ok(())
    }

    fn label(&mut self, s: &str) -> Result<(), ExportError> {
        self.line(&format!("{LABEL}{s}"))
    }

    fn content(&mut self, s: &str) -> Result<(), ExportError> {
        self.line(&format!("{TEXT}{s}"))
    }

    fn write_pieces(&mut self, label: &str, pieces: &[PieceRecord]) -> Result<(), ExportError> {
        let owned: Vec<(bool, String)> = pieces
            .iter()
            .map(|p| match &p.text {
                Some(t) if is(p.kind, SegmentKindCode::Text) => (true, t.as_str().to_owned()),
                _ => (false, placeholder(phrase("oneTimeCodeMarker"))),
            })
            .collect();
        let runs: Vec<Run<'_>> = owned
            .iter()
            .map(|(text, s)| {
                if *text {
                    Run::Text(s)
                } else {
                    Run::Placeholder(s.clone())
                }
            })
            .collect();
        self.write_text(label, &runs)
    }

    // -----------------------------------------------------------------------
    // The item line
    // -----------------------------------------------------------------------

    fn item_line(&mut self, it: &ItemRecord) -> Result<(), ExportError> {
        // The undated section is the items the archive sorts as undated
        // (their `undated` label); an item in the dated range whose own
        // time is not usable stays in place as `[No usable time]`.
        let undated = is_undated(it);
        let time = match (time_text(&it.date, self.style), undated) {
            (_, true) => {
                if !self.state.undated {
                    self.line("")?;
                    self.line(UNDATED)?;
                    self.state.undated = true;
                }
                UNDATED_TIME.to_owned()
            }
            (Some((t, day)), false) => {
                if self.state.day != Some(day) || self.state.undated {
                    self.line("")?;
                    self.line(&format!("==== {} ====", date_text(day)))?;
                    self.state.day = Some(day);
                    self.state.undated = false;
                }
                t
            }
            (None, false) => NO_TIME.to_owned(),
        };
        let SenderTags { resolved, mut tags } = sender_tags(&self.words, it);
        let sender = resolved.then(|| {
            it.sender
                .handle
                .and_then(|h| self.handles.get(&h).cloned())
                .unwrap_or_else(|| self.words.say("senderReason", it.sender.reason))
        });
        item_tags(&self.words, it, &mut tags);
        let mut s = format!("[{time}] [item {}]", it.n);
        if let Some(a) = sender {
            s.push(' ');
            s.push_str(&a);
        }
        for t in tags {
            let t = self.header(&t);
            s.push_str(" [");
            s.push_str(&t);
            s.push(']');
        }
        self.line(&s)
    }

    // -----------------------------------------------------------------------
    // The item's lines
    // -----------------------------------------------------------------------

    fn body(&mut self, it: &ItemRecord) -> Result<(), ExportError> {
        match &it.body {
            BodyRecord::Text {
                segments, lossy, ..
            } => {
                let mut main: Vec<Run<'_>> = Vec::new();
                let mut spoken: Vec<Run<'_>> = Vec::new();
                for seg in segments {
                    let k = seg.kind;
                    let run = if is(k, SegmentKindCode::Text) {
                        match &seg.text {
                            Some(t) => Run::Text(t.as_str()),
                            None => continue,
                        }
                    } else if is(k, SegmentKindCode::Withheld) {
                        Run::Placeholder(placeholder(phrase("oneTimeCodeMarker")))
                    } else if is(k, SegmentKindCode::Attachment) {
                        match seg.attachment {
                            Some(n) => Run::Placeholder(placeholder(&format!("Attachment {n}"))),
                            None => {
                                Run::Placeholder(placeholder(&self.words.say("segmentKind", k)))
                            }
                        }
                    } else {
                        Run::Placeholder(placeholder(&self.words.say("segmentKind", k)))
                    };
                    if seg.transcription {
                        spoken.push(run);
                    } else {
                        main.push(run);
                    }
                }
                if !main.is_empty() {
                    self.write_text("Text:", &main)?;
                }
                if !spoken.is_empty() {
                    self.write_text(&format!("{}:", phrase("transcription")), &spoken)?;
                }
                if *lossy {
                    self.label("Some bytes of this text were not valid text and show as U+FFFD")?;
                }
            }
            BodyRecord::NoText { reason } => {
                let s = self.words.say("noTextReason", *reason);
                self.label(&s)?;
            }
        }
        if it.body_facts.sources_differ == Some(true) {
            self.label(phrase("sourcesDiffer"))?;
        }
        match &it.alternative_text {
            AltTextRecord::None => {}
            AltTextRecord::Text { source, text } => {
                let label = format!("{}:", self.words.say("altTextSource", *source));
                match &text.text {
                    Some(t) => {
                        let t = t.as_str().to_owned();
                        self.write_text(&label, &[Run::Text(&t)])?;
                    }
                    None => self.label(&label)?,
                }
            }
            AltTextRecord::Withheld { why, .. } => {
                let s = self.words.say("altTextWithheld", *why);
                self.label(&s)?;
            }
        }
        match &it.earlier_text {
            EarlierRecord::None | EarlierRecord::Available => {}
            EarlierRecord::Withheld { reason } => {
                let s = self.words.say("withheldReason", *reason);
                self.label(&s)?;
            }
            EarlierRecord::Revealed { reason, pieces, .. } => {
                let label = format!("{}:", self.words.say("revealedReason", *reason));
                self.write_pieces(&label, pieces)?;
            }
        }
        Ok(())
    }

    fn details(&mut self, it: &ItemRecord) -> Result<(), ExportError> {
        if let Some(s) = self.header_value(&it.subject) {
            self.label(&format!("Subject: {s}"))?;
        }
        if let Some(s) = self.header_value(&it.account) {
            self.label(&format!("{}: {s}", phrase("account")))?;
        }
        if let Some(ev) = &it.event {
            let what = self.words.say("itemKind", ev.kind);
            self.label(&format!("Event: {what}"))?;
            if !ev.handles.is_empty() {
                let list: Vec<String> = ev
                    .handles
                    .iter()
                    .map(|h| {
                        self.handles
                            .get(h)
                            .cloned()
                            .unwrap_or_else(|| h.to_string())
                    })
                    .collect();
                self.label(&format!(
                    "Addresses recorded with it: {}",
                    list.as_slice().comma()
                ))?;
            }
            if let Some(t) = ev.title.as_ref().and_then(|t| self.header_value(t)) {
                self.label(&format!("Name recorded with it: {t}"))?;
            }
        }
        if let Some(a) = &it.association {
            let mut s = String::from("Reaction");
            if let Some(k) = a.kind {
                s.push_str(&format!(": {}", self.words.say("assocKind", k)));
            }
            if let Some(e) = a.emoji.as_ref().and_then(|e| self.header_value(e)) {
                s.push_str(&format!(" {e}"));
            }
            match a.target {
                Some(MessageRefRecord::InExport { n }) => s.push_str(&format!(", on item {n}")),
                Some(MessageRefRecord::NotInExport) => {
                    s.push_str(&format!(", on a message {}", phrase("notInExport")))
                }
                None => {}
            }
            s.push_str(&format!(
                " ({})",
                self.words.say("assocVisibility", a.visibility)
            ));
            self.label(&s)?;
        }
        if let Some(r) = &it.reply {
            let s = match r.originator {
                Some(MessageRefRecord::InExport { n }) => {
                    let elsewhere = it.context.reply_in_conversation == Some(false);
                    if elsewhere {
                        format!("Reply to item {n}, in another conversation of this export")
                    } else {
                        format!("Reply to item {n}")
                    }
                }
                Some(MessageRefRecord::NotInExport) => {
                    format!("Reply to a message {}", phrase("notInExport"))
                }
                None => self.words.say("replyResolution", r.resolution),
            };
            self.label(&s)?;
        }
        if let Determinable::Determined { value } = &it.reply_count {
            if value.in_export > 0 || value.elsewhere > 0 {
                self.label(&format!(
                    "Replies: {} in this export, {} {}",
                    value.in_export,
                    value.elsewhere,
                    phrase("notInExport")
                ))?;
            }
        }
        Ok(())
    }

    fn attachments(&mut self, it: &ItemRecord) -> Result<(), ExportError> {
        for a in &it.attachments {
            let name = match self.header_value(&a.name) {
                Some(n) if n.is_empty() => "an empty name recorded".to_owned(),
                Some(n) => n,
                None => {
                    let why = a
                        .name
                        .missing
                        .map(|m| self.words.say("valueState", m))
                        .unwrap_or_default();
                    format!("no name recorded ({why})")
                }
            };
            self.label(&format!("Attachment {}: {name}", a.n))?;
            let mut outcome = self.words.say("attachmentOutcome", a.file.outcome);
            if let Some(p) = &a.file.path {
                let p = self.header(p.as_str());
                outcome.push_str(&format!(": {p}"));
            }
            self.content(&outcome)?;
            if let Some(k) = a.file.sniffed {
                let s = self.words.say("sniffedKind", k);
                self.content(&format!("Content: {s}"))?;
            }
            if let Some(t) = self.header_value(&a.mime_type) {
                self.content(&format!("Declared type: {t}"))?;
            }
            if !is(a.access, AttachmentAccessCode::Included) {
                let s = self.words.say("attachmentAccess", a.access);
                self.content(&s)?;
            }
            if !is(a.card.state, CardStateCode::File) {
                let mut s = self.words.say("cardState", a.card.state);
                if let (Some(l), Some(c)) = (&a.card.unavailable_lookup, &a.card.unavailable_copy) {
                    s = s
                        .replace("{lookup}", l.as_str())
                        .replace("{copy}", c.as_str());
                }
                self.content(&s)?;
            }
            if let Some(d) = self.header_value(&a.emoji_description) {
                self.content(&format!("Description recorded with it: {d}"))?;
            }
            for h in &a.history {
                self.value_history(h, true)?;
            }
        }
        Ok(())
    }

    fn reactions(&mut self, it: &ItemRecord) -> Result<(), ExportError> {
        match &it.reactions {
            Included::NotIncluded { counts, .. } => {
                if counts.rows > 0 {
                    self.label(&format!(
                        "Reactions not included in this export: {} recorded",
                        counts.rows
                    ))?;
                }
            }
            Included::Included { value } => {
                for c in &value.chips {
                    let mut s = if c.includes_rows_outside_scope {
                        format!("Reaction ({}): ", phrase("chipsOutsideScope"))
                    } else {
                        format!("Reaction ({}): ", phrase("derived"))
                    };
                    s.push_str(&self.words.say("reactionKind", c.kind));
                    if let Some(e) = c.emoji.as_ref().and_then(|e| self.header_value(e)) {
                        s.push_str(&format!(" {e}"));
                    }
                    let by = if is(c.sender.kind, ChipSenderCode::Handle) {
                        c.sender
                            .handle
                            .and_then(|h| self.handles.get(&h).cloned())
                            .unwrap_or_else(|| self.words.say("chipSender", c.sender.kind))
                    } else {
                        self.words.say("chipSender", c.sender.kind)
                    };
                    s.push_str(&format!(
                        " by {by}, {}",
                        self.words.say("chipState", c.state)
                    ));
                    self.label(&s)?;
                }
            }
        }
        Ok(())
    }

    fn value_history(&mut self, h: &ValueHistoryRecord, nested: bool) -> Result<(), ExportError> {
        let s = match h {
            ValueHistoryRecord::Value { column, value, .. } => {
                let col = self.words.say("historyColumn", *column);
                let v = match self.header_value(value) {
                    Some(v) => v,
                    None => value
                        .missing
                        .map(|m| self.words.say("valueState", m))
                        .unwrap_or_default(),
                };
                format!("Other recorded value of {col}: {v}")
            }
            ValueHistoryRecord::Withheld { column, reason } => {
                let col = self.words.say("historyColumn", *column);
                format!(
                    "Other recorded value of {col}: {}",
                    self.words.say("withheldReason", *reason)
                )
            }
        };
        if nested {
            self.content(&s)
        } else {
            self.label(&s)
        }
    }

    fn history(&mut self, it: &ItemRecord) -> Result<(), ExportError> {
        if let Included::NotIncluded { counts, .. } = &it.history {
            if counts.captured_versions > 1 || counts.phone_entries > 0 {
                self.label(&format!(
                    "Stored versions not included in this export: {} captured, {} phone edit entries",
                    counts.captured_versions, counts.phone_entries
                ))?;
            }
        }
        for h in &it.value_history {
            self.value_history(h, false)?;
        }
        Ok(())
    }

    fn about(&mut self, about: &AboutDraft<'_>) -> Result<(), ExportError> {
        self.line("")?;
        self.line(ABOUT)?;
        let v = serde_json::to_value(about.body)
            .map_err(|_| ExportError::Limit(LimitKind::NotSerializable))?;
        let mut lines = Vec::new();
        flatten("", &v, &mut lines);
        for l in lines {
            // Every value is a number, a boolean, a closed token or a
            // validated name, but escape anyway: nothing here may break a
            // line or carry a control.
            let l = self.header(&l);
            self.line(&l)?;
        }
        Ok(())
    }
}

impl ExportWriter for TxtWriter {
    fn format(&self) -> Format {
        Format::Txt
    }

    fn begin(&mut self, ctx: &ExportContext<'_>, _out: &mut Staging) -> Result<(), ExportError> {
        let o = ctx.plan.options();
        self.nl = match o.line_endings {
            LineEndings::Lf => "\n",
            LineEndings::Crlf => "\r\n",
        };
        self.markers = o.hidden_character_markers;
        self.style = HumanStyle {
            h23: o.clock == Clock::H23,
            seconds: o.seconds,
        };
        self.single = ctx.plan.packaging() == Packaging::SingleFile;
        self.zone_name = match ctx.zone {
            ExportZone::Utc => "UTC".to_owned(),
            ExportZone::Table(t) => t.name().to_owned(),
        };
        Ok(())
    }

    fn handle(&mut self, h: &HandleRecord, _out: &mut Staging) -> Result<(), ExportError> {
        let a = match self.header_value(&h.address) {
            Some(a) if !a.is_empty() => a,
            _ => format!("address {} (not recorded)", h.n),
        };
        self.handles.insert(h.n, a);
        Ok(())
    }

    fn begin_conversation(
        &mut self,
        c: ConversationFile<'_>,
        out: &mut Staging,
    ) -> Result<(), ExportError> {
        if self.file.is_some() {
            return Err(order());
        }
        let stem = match c {
            ConversationFile::Conversation(r) => FileStem::Conversation(r.n),
            ConversationFile::NoConversation => FileStem::NoConversation,
        };
        let path = BundlePath::format_file(Format::Txt, stem, FileExt::Txt);
        self.file = Some(out.create(&path)?);
        self.state = FileState::default();
        self.report.conversation_files += 1;
        self.line(txt_legend::FIRST_LINE)?;
        match c {
            ConversationFile::Conversation(r) => {
                let name = match self.header_value(&r.name) {
                    Some(n) if !n.is_empty() => n,
                    _ => "No name recorded".to_owned(),
                };
                self.line(&format!("Conversation {}: {name}", r.n))?;
                self.line(&format!(
                    "Addresses recorded in this conversation (includes people who left): {}",
                    r.participants.len()
                ))?;
                for h in &r.name_history {
                    let v = match h {
                        ValueHistoryRecord::Value { value, .. } => match self.header_value(value) {
                            Some(v) => v,
                            None => value
                                .missing
                                .map(|m| self.words.say("valueState", m))
                                .unwrap_or_default(),
                        },
                        ValueHistoryRecord::Withheld { reason, .. } => {
                            self.words.say("withheldReason", *reason)
                        }
                    };
                    self.line(&format!("{OTHER_NAME}{v}"))?;
                }
            }
            ConversationFile::NoConversation => {
                self.line("No conversation: messages recorded in no conversation")?;
            }
        }
        let zone = self.zone_name.clone();
        self.line(&format!("Times: {zone} (offsets shown on every line)"))
    }

    fn item(&mut self, it: &ItemRecord, _out: &mut Staging) -> Result<(), ExportError> {
        if self.file.is_none() {
            return Err(order());
        }
        if it.context.folded_into.is_some() {
            self.state.folded += 1;
            self.report.folded_rows += 1;
            self.last_item = None;
            return Ok(());
        }
        self.last_item = Some(it.n);
        self.item_line(it)?;
        self.body(it)?;
        self.details(it)?;
        self.attachments(it)?;
        self.reactions(it)?;
        self.history(it)?;
        self.state.items += 1;
        self.report.items += 1;
        Ok(())
    }

    fn version(&mut self, v: &VersionRecord, _out: &mut Staging) -> Result<(), ExportError> {
        if self.file.is_none() {
            return Err(order());
        }
        // A folded reaction row's versions (the row is a chip of its
        // target, not an item) are in the structured formats only; before
        // slice 6g they were written under the item before it.
        if self.last_item != Some(v.message) {
            return Ok(());
        }
        let label = format!(
            "Stored version{}, kept by imports {} to {}:",
            if v.current { " (current)" } else { "" },
            v.first_session,
            v.last_session
        );
        match &v.text {
            VersionTextRecord::Text { pieces } => self.write_pieces(&label, pieces)?,
            VersionTextRecord::Withheld { reason } => {
                let s = self.words.say("withheldReason", *reason);
                self.label(&format!("{label} {s}"))?;
            }
            VersionTextRecord::NoText { reason, .. } => {
                let s = self.words.say("noTextReason", *reason);
                self.label(&format!("{label} {s}"))?;
            }
        }
        for e in &v.edits {
            let at = e.at.raw.as_ref().map_or("not recorded", |r| r.as_str());
            let label = format!(
                "Phone edit entry, part {}, {} {at}:",
                e.part.as_str(),
                phrase("phoneEditTime")
            );
            match &e.text {
                EditEntryTextRecord::Text { pieces } => self.write_pieces(&label, pieces)?,
                EditEntryTextRecord::Withheld { reason } => {
                    let s = self.words.say("withheldReason", *reason);
                    self.label(&format!("{label} {s}"))?;
                }
                EditEntryTextRecord::NoBody => {
                    self.label(&format!("{label} No text recorded for this version"))?
                }
                EditEntryTextRecord::Failed { .. } => {
                    self.label(&format!("{label} Edit text could not be decoded"))?
                }
                EditEntryTextRecord::NotMatched => {
                    self.label(&format!("{label} Edit text could not be matched"))?
                }
            }
        }
        Ok(())
    }

    fn end_conversation(
        &mut self,
        totals: &ConversationTotals,
        out: &mut Staging,
    ) -> Result<(), ExportError> {
        if totals.items != self.state.items || totals.folded_rows != self.state.folded {
            return Err(ExportError::Limit(LimitKind::RowCountMismatch));
        }
        self.line("")?;
        self.line(FOOTER)?;
        self.line(&format!("Items: {}", self.state.items))?;
        self.line(&format!(
            "Reactions shown on the messages they react to: {}",
            self.state.folded
        ))?;
        self.line("")?;
        self.line("Legend:")?;
        for l in txt_legend::LINES {
            self.line(l)?;
        }
        if !self.single {
            let f = self.file.take().ok_or_else(order)?;
            out.close(f)?;
        }
        Ok(())
    }

    fn finish(
        mut self: Box<Self>,
        about: &AboutDraft<'_>,
        out: &mut Staging,
    ) -> Result<WriterReport, ExportError> {
        if self.single {
            if self.file.is_none() {
                return Err(order());
            }
            self.about(about)?;
            let f = self.file.take().ok_or_else(order)?;
            out.close(f)?;
        } else if self.file.is_some() {
            return Err(ExportError::Limit(LimitKind::FileStillOpen));
        }
        let mut r = self.report.clone();
        let c = self.counts;
        r.counts.insert("hiddenCharactersMarked", c.hidden_marked);
        r.counts.insert("controlsEscaped", c.controls_escaped);
        r.counts
            .insert("combiningRunsLimited", c.combining_runs_limited);
        r.counts.insert("reservedEscaped", c.reserved_escaped);
        r.counts.insert("bracketsEscaped", self.brackets_escaped);
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_text_splits_at_every_line_break_once() {
        assert_eq!(
            split_lines("a\r\nb\rc\nd\u{2028}e\u{2029}f\u{85}g"),
            ["a", "b", "c", "d", "e", "f", "g"]
        );
        assert_eq!(split_lines(""), [""]);
        assert_eq!(split_lines("x\n"), ["x", ""]);
        assert_eq!(split_lines("\r\r\n"), ["", "", ""]);
    }

    #[test]
    fn header_fields_escape_brackets_line_breaks_and_hidden_characters() {
        let mut w = TxtWriter::new();
        let h = w.header("Bob [Edited: part 1]\n[2024-01-01 00:00:00 +00:00] \u{202E}x");
        assert_eq!(
            h,
            "Bob \u{27E8}U+005B\u{27E9}Edited: part 1\u{27E8}U+005D\u{27E9}\u{27E8}U+000A\u{27E9}\
             \u{27E8}U+005B\u{27E9}2024-01-01 00:00:00 +00:00\u{27E8}U+005D\u{27E9} \u{27E8}U+202E\u{27E9}x"
        );
        assert!(!h.contains('[') && !h.contains(']') && !h.contains('\n'));
        assert_eq!(w.brackets_escaped, 4);
    }

    #[test]
    fn recorded_text_keeps_placeholders_and_marks_the_rest() {
        let mut w = TxtWriter::new();
        let lines = w.text_lines(&[
            Run::Text("one\r\ntwo \u{27E8}x\u{27E9}"),
            Run::Placeholder(placeholder("Attachment 3")),
            Run::Text("\u{1B}[31m"),
        ]);
        assert_eq!(
            lines,
            [
                "one".to_owned(),
                "two \u{27E8}U+27E8\u{27E9}x\u{27E9}\u{27E8}Attachment 3\u{27E9}\u{27E8}U+001B\u{27E9}[31m"
                    .to_owned()
            ]
        );
    }
}
