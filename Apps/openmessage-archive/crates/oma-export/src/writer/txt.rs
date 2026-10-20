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

use super::lines::{self, Emit, Part, Run};
use super::shared::{
    date_text, flatten, is_undated, item_tags, sender_tags, time_text, SenderTags,
};
use super::{
    AboutDraft, ConversationFile, ConversationTotals, ExportContext, ExportWriter, Words,
    WriterReport,
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

    /// The lines of recorded text made of `runs`: text split at line
    /// breaks, placeholders kept as they are. The recorded text between
    /// two placeholders or line breaks is marked as one string, so a
    /// combining run split across formatting segments is cut like one in
    /// a single segment (review fix; TXT draws no styles).
    fn text_lines(&mut self, runs: &[Run<'_>]) -> Vec<String> {
        let policy = MarkPolicy::text_body(self.markers);
        let mut lines = vec![String::new()];
        let mut pending = String::new();
        let flush = |lines: &mut Vec<String>, pending: &mut String, counts: &mut MarkCounts| {
            if !pending.is_empty() {
                let (m, n) = hidden::mark(pending, policy);
                counts.add(n);
                if let Some(l) = lines.last_mut() {
                    l.push_str(&m);
                }
                pending.clear();
            }
        };
        for r in runs {
            match r {
                Run::Placeholder(p) => {
                    flush(&mut lines, &mut pending, &mut self.counts);
                    if let Some(l) = lines.last_mut() {
                        l.push_str(p);
                    }
                }
                Run::Text(t) => {
                    for (i, part) in split_lines(t).into_iter().enumerate() {
                        if i > 0 {
                            flush(&mut lines, &mut pending, &mut self.counts);
                            lines.push(String::new());
                        }
                        pending.push_str(part);
                    }
                }
            }
        }
        flush(&mut lines, &mut pending, &mut self.counts);
        lines
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

impl Emit for TxtWriter {
    type V = String;

    fn words(&self) -> &Words {
        &self.words
    }

    fn value(&mut self, raw: &str) -> String {
        self.header(raw)
    }

    fn handle_value(&self, h: Ordinal) -> Option<String> {
        self.handles.get(&h).cloned()
    }

    fn label(&mut self, parts: Vec<Part<String>>) -> Result<(), ExportError> {
        let s = concat(parts);
        self.line(&format!("{LABEL}{s}"))
    }

    fn content(&mut self, parts: Vec<Part<String>>) -> Result<(), ExportError> {
        let s = concat(parts);
        self.line(&format!("{TEXT}{s}"))
    }

    fn text(&mut self, label: Vec<Part<String>>, runs: &[Run<'_>]) -> Result<(), ExportError> {
        self.label(label)?;
        for l in self.text_lines(runs) {
            self.line(&format!("{TEXT}{l}"))?;
        }
        Ok(())
    }
}

/// A line's parts as one string (each value already marked).
fn concat(parts: Vec<Part<String>>) -> String {
    let mut s = String::new();
    for p in parts {
        match p {
            Part::Ours(x) | Part::Value(x) => s.push_str(&x),
        }
    }
    s
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
        let a = match lines::value_of(self, &h.address) {
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
                let name = match lines::value_of(self, &r.name) {
                    Some(n) if !n.is_empty() => n,
                    _ => "No name recorded".to_owned(),
                };
                self.line(&format!("Conversation {}: {name}", r.n))?;
                self.line(&format!(
                    "Addresses recorded in this conversation (includes people who left): {}",
                    r.participants.len()
                ))?;
                for h in &r.name_history {
                    let v = match lines::name_history_value(self, h) {
                        Part::Ours(s) | Part::Value(s) => s,
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
        lines::body(self, it)?;
        lines::details(self, it)?;
        lines::attachments(self, it)?;
        lines::reactions(self, it)?;
        lines::history(self, it)?;
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
        lines::version(self, v)
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
    use crate::writer::placeholder;

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

    /// Review finding: a combining flood split across formatting segments
    /// (one `Run::Text` per segment) is cut like one in a single segment.
    #[test]
    fn a_combining_flood_split_across_segments_is_cut_once() {
        let marks = "\u{301}".repeat(16);
        let mut runs = vec![Run::Text("e")];
        for _ in 0..600 {
            runs.push(Run::Text(&marks));
        }
        let mut w = TxtWriter::new();
        let lines = w.text_lines(&runs);
        assert_eq!(lines.len(), 1);
        let kept = lines[0].chars().filter(|c| *c == '\u{301}').count();
        assert_eq!(kept, crate::hidden::COMBINING_RUN_LIMIT);
        assert!(
            lines[0].ends_with("\u{27E8}+9584 combining marks\u{27E9}"),
            "{}",
            &lines[0][..40]
        );
        assert_eq!(w.counts.combining_runs_limited, 1);
        // A placeholder or a line break still ends a run.
        let mut w = TxtWriter::new();
        let p = placeholder("Attachment 1");
        let lines = w.text_lines(&[
            Run::Text("e"),
            Run::Text(&marks),
            Run::Placeholder(p),
            Run::Text(&marks),
        ]);
        assert_eq!(lines[0].chars().filter(|c| *c == '\u{301}').count(), 32);
        assert_eq!(w.counts.combining_runs_limited, 0);
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
