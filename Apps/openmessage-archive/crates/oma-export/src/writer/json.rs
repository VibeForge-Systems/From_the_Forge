//! The JSON writer (M6 spec §5.1, D123).
//!
//! JSON is the records' own serde form: every value a record holds is
//! written, with codes as `{token, code}`, raw integers that can exceed
//! 2^53 − 1 as decimal strings, ordinals as numbers and absence tagged.
//! Every line and every array element goes through
//! [`jsonfmt::to_line`], which escapes U+2028, U+2029, U+0085, DEL and the
//! C1 controls on top of `serde_json`'s escapes, so no record can be split
//! by a tool that breaks lines on them (Python's `str.splitlines()`).
//! UTF-8 without a BOM, LF.
//!
//! **Bundle** (`json/`):
//!
//! - `conversations.json`: `{"type":"conversations","format",
//!   "schemaVersion","conversations":[…]}`, one conversation record per
//!   line, in conversation order, without items;
//! - `handles.json`: the same shape with `"handles"`, in the order the
//!   mapping first referenced them;
//! - `c<nnnn>.jsonl` and `no-conversation.jsonl`, JSON Lines: line 1 is
//!   `{"type":"header","format","schemaVersion","conversation":n}`
//!   (`null` for the messages in no conversation), then one
//!   `{"type":"item",…}` per timeline item in timeline order (dated, then
//!   undated; folded reaction rows after their batch's items, with
//!   `context.foldedInto`), each followed by its stored body versions as
//!   `{"type":"version",…}` lines when history is included (once per
//!   message, in its first conversation file);
//! - `schema/export-v1.schema.json` and `schema/manifest-v1.schema.json`,
//!   copies of the open schemas.
//!
//! **Single file** (one conversation): `{"format","schemaVersion",
//! "conversation":{…},"items":[…],"handles":[…],"about":{…}}`, where
//! `items` holds the same `item` and `version` objects as the JSON Lines
//! files. It streams: `handles` and `about` (the manifest's fields
//! without `files`, §7.3) come last.
//!
//! **The intrinsic part.** An item's fields are written in record order
//! with `context` last, so a message that is a timeline item of two
//! conversations is written byte for byte the same in both files up to
//! `"context"` (spec §4.9; `tests/json.rs` checks it).
//!
//! **Not I-JSON.** RFC 7493 §2.1 forbids noncharacters even escaped, and
//! message text may hold them; JSON claims RFC 8259 only. Text is not
//! Unicode-normalised and lossy text is already U+FFFD-replaced (and
//! flagged); `README.txt` says so. Nothing here logs.

use std::io::Write;

use serde::Serialize;

use crate::error::{ExportError, LimitKind};
use crate::jsonfmt;
use crate::manifest::SCHEMA_VERSION;
use crate::naming::{BundlePath, FileExt, FileStem, SchemaFile, TableStem};
use crate::plan::{Format, Packaging};
use crate::record::{ConversationRecord, HandleRecord, ItemRecord, VersionRecord};
use crate::stage::{StagedFile, Staging};

use super::{
    AboutDraft, ConversationFile, ConversationTotals, ExportContext, ExportWriter, WriterReport,
};

/// `format` of every JSON file of an export.
pub const FORMAT: &str = "openmessage-archive-export";

/// The open record schema (published; a copy ships in every JSON
/// bundle).
pub const EXPORT_SCHEMA: &str = include_str!("../../schema/export-v1.schema.json");
/// The strict record schema (CI only, never shipped).
pub const STRICT_SCHEMA: &str = include_str!("../../schema/export-v1.strict.schema.json");
/// The manifest schema (a copy ships in every JSON bundle).
pub const MANIFEST_SCHEMA: &str = include_str!("../../schema/manifest-v1.schema.json");

fn io(e: std::io::Error) -> ExportError {
    ExportError::io(&e)
}

fn order() -> ExportError {
    ExportError::Limit(LimitKind::RowOrder)
}

fn not_serializable() -> ExportError {
    ExportError::Limit(LimitKind::NotSerializable)
}

/// The first line of a JSON Lines file.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Header {
    r#type: &'static str,
    format: &'static str,
    schema_version: &'static str,
    conversation: Option<crate::ids::Ordinal>,
}

/// `{"type":"<kind>",` followed by `v`'s own fields: the line form of an
/// item or a version, whose `type` comes first.
fn tagged<T: Serialize>(kind: &str, v: &T) -> Result<String, ExportError> {
    let s = jsonfmt::to_line(v)?;
    let rest = s.strip_prefix('{').ok_or_else(not_serializable)?;
    let mut out = String::with_capacity(s.len() + kind.len() + 12);
    out.push_str("{\"type\":\"");
    out.push_str(kind);
    out.push('"');
    if rest != "}" {
        out.push(',');
    }
    out.push_str(rest);
    Ok(out)
}

/// The line form of an item record.
pub fn item_line(it: &ItemRecord) -> Result<String, ExportError> {
    tagged("item", it)
}

/// The line form of a version record.
pub fn version_line(v: &VersionRecord) -> Result<String, ExportError> {
    tagged("version", v)
}

/// A streamed JSON array inside a file: `head`, then one element per
/// line, then `tail`.
struct Array {
    file: StagedFile,
    elements: u64,
}

impl Array {
    fn open(mut file: StagedFile, head: &str) -> Result<Array, ExportError> {
        file.write_all(head.as_bytes()).map_err(io)?;
        Ok(Array { file, elements: 0 })
    }

    fn push(&mut self, line: &str) -> Result<(), ExportError> {
        let sep: &[u8] = if self.elements == 0 { b"\n" } else { b",\n" };
        self.file.write_all(sep).map_err(io)?;
        self.file.write_all(line.as_bytes()).map_err(io)?;
        self.elements += 1;
        Ok(())
    }

    /// Close the array with `]` and write `tail` after it.
    fn end(mut self, tail: &str) -> Result<StagedFile, ExportError> {
        let close: &[u8] = if self.elements == 0 { b"]" } else { b"\n]" };
        self.file.write_all(close).map_err(io)?;
        self.file.write_all(tail.as_bytes()).map_err(io)?;
        Ok(self.file)
    }
}

/// The head of a bundle's table file: `{"type":…,"format":…,
/// "schemaVersion":…,"<key>":[`.
fn table_head(kind: &str) -> String {
    format!(
        "{{\"type\":\"{kind}\",\"format\":\"{FORMAT}\",\"schemaVersion\":\"{SCHEMA_VERSION}\",\"{kind}\":["
    )
}

// One per export: the size of the bundle variant (three hashing writers)
// does not matter.
#[allow(clippy::large_enum_variant)]
enum Mode {
    /// Not begun yet.
    New,
    Bundle {
        conversations: Array,
        handles: Array,
        /// The open JSON Lines file.
        lines: Option<StagedFile>,
    },
    Single {
        /// The single file, inside its `items` array once the conversation
        /// began.
        items: Option<Array>,
        /// Handles, as lines, written after the items.
        handles: Vec<String>,
    },
    Done,
}

/// The JSON writer.
pub struct JsonWriter {
    mode: Mode,
    report: WriterReport,
    /// Items of the open conversation file (folded rows included).
    file_items: u64,
    file_folded: u64,
    versions: u64,
    handles: u64,
    lines_escaped: u64,
}

impl std::fmt::Debug for JsonWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonWriter")
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

impl Default for JsonWriter {
    fn default() -> JsonWriter {
        JsonWriter::new()
    }
}

impl JsonWriter {
    pub fn new() -> JsonWriter {
        JsonWriter {
            mode: Mode::New,
            report: WriterReport::new(Format::Json),
            file_items: 0,
            file_folded: 0,
            versions: 0,
            handles: 0,
            lines_escaped: 0,
        }
    }

    fn count_escapes(&mut self, line: &str) {
        if line.contains("\\u2028")
            || line.contains("\\u2029")
            || line.contains("\\u0085")
            || line.contains("\\u007f")
        {
            self.lines_escaped += 1;
        }
    }

    /// Write one line (an item or a version) where items go.
    fn emit(&mut self, line: String) -> Result<(), ExportError> {
        self.count_escapes(&line);
        match &mut self.mode {
            Mode::Bundle { lines: Some(f), .. } => {
                f.write_all(line.as_bytes()).map_err(io)?;
                f.write_all(b"\n").map_err(io)
            }
            Mode::Single { items: Some(a), .. } => a.push(&line),
            _ => Err(order()),
        }
    }
}

fn write_whole(out: &mut Staging, path: &BundlePath, text: &str) -> Result<(), ExportError> {
    let mut f = out.create(path)?;
    f.write_all(text.as_bytes()).map_err(io)?;
    out.close(f)?;
    Ok(())
}

impl ExportWriter for JsonWriter {
    fn format(&self) -> Format {
        Format::Json
    }

    fn begin(&mut self, ctx: &ExportContext<'_>, out: &mut Staging) -> Result<(), ExportError> {
        if !matches!(self.mode, Mode::New) {
            return Err(order());
        }
        self.mode = match ctx.plan.packaging() {
            Packaging::Bundle => {
                write_whole(out, &BundlePath::schema(SchemaFile::Export), EXPORT_SCHEMA)?;
                write_whole(
                    out,
                    &BundlePath::schema(SchemaFile::Manifest),
                    MANIFEST_SCHEMA,
                )?;
                let table = |t: TableStem| {
                    BundlePath::format_file(Format::Json, FileStem::Table(t), FileExt::Json)
                };
                let conversations = Array::open(
                    out.create(&table(TableStem::Conversations))?,
                    &table_head("conversations"),
                )?;
                let handles = Array::open(
                    out.create(&table(TableStem::Handles))?,
                    &table_head("handles"),
                )?;
                Mode::Bundle {
                    conversations,
                    handles,
                    lines: None,
                }
            }
            Packaging::SingleFile => Mode::Single {
                items: None,
                handles: Vec::new(),
            },
        };
        Ok(())
    }

    fn handle(&mut self, h: &HandleRecord, _out: &mut Staging) -> Result<(), ExportError> {
        let line = jsonfmt::to_line(h)?;
        self.count_escapes(&line);
        self.handles += 1;
        match &mut self.mode {
            Mode::Bundle { handles, .. } => handles.push(&line),
            Mode::Single { handles, .. } => {
                handles.push(line);
                Ok(())
            }
            _ => Err(order()),
        }
    }

    fn begin_conversation(
        &mut self,
        c: ConversationFile<'_>,
        out: &mut Staging,
    ) -> Result<(), ExportError> {
        let conv: Option<&ConversationRecord> = match c {
            ConversationFile::Conversation(r) => Some(r),
            ConversationFile::NoConversation => None,
        };
        self.file_items = 0;
        self.file_folded = 0;
        self.report.conversation_files += 1;
        match &mut self.mode {
            Mode::Bundle {
                conversations,
                lines,
                ..
            } => {
                if lines.is_some() {
                    return Err(order());
                }
                let stem = match conv {
                    Some(r) => {
                        let line = jsonfmt::to_line(r)?;
                        conversations.push(&line)?;
                        FileStem::Conversation(r.n)
                    }
                    None => FileStem::NoConversation,
                };
                let mut f =
                    out.create(&BundlePath::format_file(Format::Json, stem, FileExt::Jsonl))?;
                let header = jsonfmt::to_line(&Header {
                    r#type: "header",
                    format: FORMAT,
                    schema_version: SCHEMA_VERSION,
                    conversation: conv.map(|r| r.n),
                })?;
                f.write_all(header.as_bytes()).map_err(io)?;
                f.write_all(b"\n").map_err(io)?;
                *lines = Some(f);
                Ok(())
            }
            Mode::Single { items, .. } => {
                if items.is_some() {
                    return Err(order());
                }
                // The plan's packaging is a single file only for one
                // conversation, so this is the first and only file.
                let f = out.create(&BundlePath::format_file(
                    Format::Json,
                    match conv {
                        Some(r) => FileStem::Conversation(r.n),
                        None => FileStem::NoConversation,
                    },
                    FileExt::Json,
                ))?;
                let conversation = match conv {
                    Some(r) => jsonfmt::to_line(r)?,
                    None => "null".to_owned(),
                };
                let head = format!(
                    "{{\"format\":\"{FORMAT}\",\"schemaVersion\":\"{SCHEMA_VERSION}\",\"conversation\":{conversation},\"items\":["
                );
                *items = Some(Array::open(f, &head)?);
                Ok(())
            }
            _ => Err(order()),
        }
    }

    fn item(&mut self, it: &ItemRecord, _out: &mut Staging) -> Result<(), ExportError> {
        let line = item_line(it)?;
        self.emit(line)?;
        if it.context.folded_into.is_some() {
            self.file_folded += 1;
            self.report.folded_rows += 1;
        } else {
            self.file_items += 1;
            self.report.items += 1;
        }
        Ok(())
    }

    fn version(&mut self, v: &VersionRecord, _out: &mut Staging) -> Result<(), ExportError> {
        let line = version_line(v)?;
        self.emit(line)?;
        self.versions += 1;
        Ok(())
    }

    fn end_conversation(
        &mut self,
        totals: &ConversationTotals,
        out: &mut Staging,
    ) -> Result<(), ExportError> {
        if totals.items != self.file_items || totals.folded_rows != self.file_folded {
            return Err(ExportError::Limit(LimitKind::RowCountMismatch));
        }
        match &mut self.mode {
            Mode::Bundle { lines, .. } => {
                let f = lines.take().ok_or_else(order)?;
                out.close(f)?;
                Ok(())
            }
            // The single file stays open for `handles` and `about`.
            Mode::Single { items: Some(_), .. } => Ok(()),
            _ => Err(order()),
        }
    }

    fn finish(
        mut self: Box<Self>,
        about: &AboutDraft<'_>,
        out: &mut Staging,
    ) -> Result<WriterReport, ExportError> {
        match std::mem::replace(&mut self.mode, Mode::Done) {
            Mode::Bundle {
                conversations,
                handles,
                lines,
            } => {
                if lines.is_some() {
                    return Err(ExportError::Limit(LimitKind::FileStillOpen));
                }
                out.close(conversations.end("}\n")?)?;
                out.close(handles.end("}\n")?)?;
            }
            Mode::Single {
                items: Some(items),
                handles,
            } => {
                let f = items.end(",\"handles\":[")?;
                let mut a = Array {
                    file: f,
                    elements: 0,
                };
                for h in &handles {
                    a.push(h)?;
                }
                let about = jsonfmt::to_line(about.body)?;
                let f = a.end(&format!(",\"about\":{about}}}\n"))?;
                out.close(f)?;
            }
            _ => return Err(order()),
        }
        let mut r = self.report.clone();
        r.counts.insert("jsonVersions", self.versions);
        r.counts.insert("jsonHandles", self.handles);
        r.counts
            .insert("jsonLinesWithSeparatorEscapes", self.lines_escaped);
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tagged_line_puts_its_type_first_and_keeps_the_fields() {
        #[derive(Serialize)]
        struct T {
            a: u8,
            b: &'static str,
        }
        let s = tagged(
            "item",
            &T {
                a: 1,
                b: "x\u{2028}",
            },
        )
        .expect("line");
        assert_eq!(s, "{\"type\":\"item\",\"a\":1,\"b\":\"x\\u2028\"}");
        #[derive(Serialize)]
        struct E {}
        assert_eq!(
            tagged("version", &E {}).expect("line"),
            "{\"type\":\"version\"}"
        );
    }

    #[test]
    fn the_shipped_schemas_are_json_with_their_ids() {
        for (text, id) in [
            (EXPORT_SCHEMA, "urn:openmessage-archive:export:1"),
            (STRICT_SCHEMA, "urn:openmessage-archive:export:1.1.0:strict"),
            (MANIFEST_SCHEMA, "urn:openmessage-archive:export-manifest:1"),
        ] {
            let v: serde_json::Value = serde_json::from_str(text).expect("schema");
            assert_eq!(v["$id"], id);
            assert_eq!(v["$schema"], "https://json-schema.org/draft/2020-12/schema");
        }
    }
}
