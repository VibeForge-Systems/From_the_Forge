//! The CSV writer (M6 spec §5.2, D130).
//!
//! CSV is always a bundle (D126): nine files under `csv/`, each **always
//! written**, with its header row even when an option leaves its rows out
//! (`reactions.csv` with reactions off, `edit_versions.csv` with history
//! off). Their columns are [`TABLES`], the contract of
//! `tabularColumnsVersion` 1.0.0 (pinned by `tests/csv.rs` against
//! `tests/csv_columns.pin`): consumers read columns by header name, and
//! columns are only ever appended, in a MINOR version.
//!
//! - `messages.csv`: one row per (conversation file, timeline item),
//!   folded reaction rows included with `folded_into`; the spec's columns
//!   in its order, then the v1 additions (the states, units and context
//!   facts the spec's list has no column for).
//! - `segments.csv`, `attachments.csv`, `attachment_observations.csv`,
//!   `reactions.csv` (the raw rows in scope), `edit_versions.csv` and the
//!   message rows of `value_history.csv` hold a message's intrinsic part
//!   and are written once per message, at its first placement, keyed by
//!   `item`.
//! - `conversations.csv`, `handles.csv` and the conversations' name
//!   history in `value_history.csv`.
//!
//! **RFC 4180.** CRLF record ends (`Terminator::CRLF`), a header row,
//! embedded `"` doubled. Every field is quoted (`QuoteStyle::Always`):
//! `csv` sets one quoting style per writer, so the spec's "necessary,
//! plus every content-derived column always" is met by quoting all of
//! them (a deviation recorded in `architecture.md`). A lone CR is inside
//! quotes like everything else. UTF-8 is an extension of RFC 4180's
//! printable-ASCII `TEXTDATA`, which `README.txt` states. The `csv`
//! writer is made only by `WriterBuilder::from_writer` over a
//! [`StagedFile`]; `from_path` is banned (`clippy.toml`).
//!
//! **Profiles** ([`CsvProfile`]):
//!
//! - *For spreadsheets* (the default): a UTF-8 BOM; C0 and C1 controls
//!   other than TAB, CR and LF, and DEL, written as `⟨U+XXXX⟩` and
//!   counted; U+27E8 escaped; formula neutralisation on.
//! - *For data tools*: no BOM, no neutralisation, controls raw; only
//!   U+27E8 is escaped, so the export's placeholders stay unforgeable.
//!
//! **Neutralisation is default-deny** (OWASP "CSV Injection"):
//!
//! - Only typed columns of ours are exempt, and only when the value
//!   matches the column's grammar ([`Kind`]: `^-?[0-9]+$`, a list of
//!   those, an ISO time, `[+-]HH:MM[:SS]`, a closed token). Every other
//!   cell is tested, content and typed values that fail their grammar
//!   alike.
//! - A cell triggers when its first character is TAB, CR or LF, or when
//!   its first character after skipping White_Space and
//!   Default_Ignorable_Code_Point characters (bidi controls, ZWSP, BOM,
//!   U+180E, …) is `=`, `+`, `-`, `@`, a full-width `＝ ＋ － ＠` or a
//!   small form `﹢ ﹣ ﹦ ﹫` (U+FE6B, the small commercial at, is added to
//!   the spec's three: the export catalogue plants it, and default-deny
//!   errs towards the prefix).
//! - The test runs on the final cell text, after placeholders, list
//!   joining and control escaping.
//! - The fix is a `'` prefix inside the quotes. The cell is counted
//!   (`neutralisedCells`) and its column named in the row's
//!   `neutralised_columns`, so a real leading apostrophe can be told
//!   from ours (every E.164 address starts with `+`).
//!
//! Lists are `;`-joined. Codes are tokens with a `_code` column where
//! the spec names one; a number this build does not know is written as
//! `unrecognisedCode:<n>`. Ids are export-local ordinals; the `source_*`
//! columns are empty unless the D141 opt-in is on. The raw columns no
//! viewer shows (`record.raw`) are in JSON only. Nothing here logs.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use crate::codes::{Code, SegmentKindCode};
use crate::error::{ExportError, LimitKind};
use crate::hidden::{self, in_table, MARKER_OPEN};
use crate::ids::Ordinal;
use crate::naming::{BundlePath, FileExt, FileStem, TableStem};
use crate::plan::{CsvProfile, Format};
use crate::record::*;
use crate::stage::{StagedFile, Staging};
use crate::unicode_tables::{DEFAULT_IGNORABLE, WHITE_SPACE};

use super::{
    is, phrase, placeholder, AboutDraft, ConversationFile, ConversationTotals, ExportContext,
    ExportWriter, Words, WriterReport,
};

/// The column a row lists its neutralised cells in.
pub const NEUTRALISED: &str = "neutralised_columns";

/// A column's grammar: the value types that are exempt from neutralisation
/// when they match it. Everything else is [`Kind::Content`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Tested, always.
    Content,
    /// `^-?[0-9]+$`.
    Int,
    /// `;`-joined [`Kind::Int`]s.
    IntList,
    /// A real number as stored: `^-?[0-9]+(\.[0-9]+)?(e-?[0-9]+)?$`, or
    /// `inf`, `-inf`, `NaN`.
    Real,
    /// An ISO 8601 time with `Z` or an offset (`time::format_iso`).
    Iso,
    /// `[+-]HH:MM` or `[+-]HH:MM:SS`.
    Offset,
    /// `;`-joined closed tokens: a letter, then letters, digits and
    /// `:`-separated parts (`unrecognisedCode:7`, `lineage:1:session:2`),
    /// and `true` / `false`.
    Token,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Content => "content",
            Kind::Int => "int",
            Kind::IntList => "intList",
            Kind::Real => "real",
            Kind::Iso => "iso",
            Kind::Offset => "offset",
            Kind::Token => "token",
        }
    }

    /// Whether `v` matches the grammar (and so is exempt).
    pub fn matches(self, v: &str) -> bool {
        match self {
            Kind::Content => false,
            Kind::Int => is_int(v),
            Kind::IntList => !v.is_empty() && v.split(';').all(is_int),
            Kind::Real => is_real(v),
            Kind::Iso => is_iso(v),
            Kind::Offset => is_offset(v),
            Kind::Token => !v.is_empty() && v.split(';').all(is_token),
        }
    }
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_int(s: &str) -> bool {
    digits(s.strip_prefix('-').unwrap_or(s))
}

fn is_token(s: &str) -> bool {
    let mut parts = s.split(':');
    let first = parts.next().unwrap_or("");
    first
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic())
        && first.bytes().all(|b| b.is_ascii_alphanumeric())
        && parts.all(|p| {
            let p = p.strip_prefix('-').unwrap_or(p);
            !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

fn is_real(s: &str) -> bool {
    if matches!(s, "inf" | "-inf" | "NaN") {
        return true;
    }
    let s = s.strip_prefix('-').unwrap_or(s);
    let (m, e) = match s.split_once('e') {
        Some((m, e)) => (m, Some(e)),
        None => (s, None),
    };
    let m_ok = match m.split_once('.') {
        Some((a, b)) => digits(a) && digits(b),
        None => digits(m),
    };
    m_ok && e.is_none_or(is_int)
}

fn two(s: &str) -> bool {
    s.len() == 2 && digits(s)
}

fn is_offset(s: &str) -> bool {
    let Some(rest) = s.strip_prefix('+').or_else(|| s.strip_prefix('-')) else {
        return false;
    };
    let parts: Vec<&str> = rest.split(':').collect();
    matches!(parts.as_slice(), [h, m] | [h, m, _] if two(h) && two(m))
        && parts.get(2).is_none_or(|x| two(x))
}

/// `YYYY-MM-DDTHH:MM:SS[.f…](Z|±HH:MM[:SS])`, with a year of four or more
/// digits, a leading `-`, or `+` and digits.
fn is_iso(s: &str) -> bool {
    let Some((date, time)) = s.split_once('T') else {
        return false;
    };
    let mut d: Vec<&str> = date.rsplitn(3, '-').collect();
    d.reverse();
    let date_ok = match d.as_slice() {
        [y, m, dd] => {
            let y = y
                .strip_prefix('-')
                .or_else(|| y.strip_prefix('+'))
                .unwrap_or(y);
            y.len() >= 4 && digits(y) && two(m) && two(dd)
        }
        _ => false,
    };
    let (clock, zone) = if let Some(c) = time.strip_suffix('Z') {
        (c, "Z")
    } else {
        match time.find(['+', '-']) {
            Some(i) => (time.get(..i).unwrap_or(""), time.get(i..).unwrap_or("")),
            None => return false,
        }
    };
    let (hms, frac) = match clock.split_once('.') {
        Some((a, f)) => (a, Some(f)),
        None => (clock, None),
    };
    let t: Vec<&str> = hms.split(':').collect();
    let clock_ok = matches!(t.as_slice(), [h, m, sec] if two(h) && two(m) && two(sec))
        && frac.is_none_or(digits);
    date_ok && clock_ok && (zone == "Z" || is_offset(zone))
}

fn is_formula_start(c: char) -> bool {
    matches!(
        c,
        '=' | '+'
            | '-'
            | '@'
            | '\u{FF1D}'
            | '\u{FF0B}'
            | '\u{FF0D}'
            | '\u{FF20}'
            | '\u{FE62}'
            | '\u{FE63}'
            | '\u{FE66}'
            | '\u{FE6B}'
    )
}

/// Whether a spreadsheet could read `cell` as a formula (the trigger
/// above).
pub fn triggers(cell: &str) -> bool {
    let mut chars = cell.chars();
    match chars.next() {
        None => false,
        Some('\t' | '\r' | '\n') => true,
        Some(first) => std::iter::once(first)
            .chain(chars)
            .find(|&c| !in_table(WHITE_SPACE, c) && !in_table(DEFAULT_IGNORABLE, c))
            .is_some_and(is_formula_start),
    }
}

/// What the profile escaping changed in a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Escaped {
    pub controls: u64,
    pub reserved: u64,
}

/// `cell` with the profile's escapes: U+27E8 always, and under
/// `controls` every C0 and C1 control other than TAB, CR and LF, and
/// DEL, as `⟨U+XXXX⟩`.
pub fn escape(cell: &str, controls: bool) -> (String, Escaped) {
    let control = |c: char| {
        let cp = u32::from(c);
        !matches!(c, '\t' | '\r' | '\n') && (cp < 0x20 || (0x7F..=0x9F).contains(&cp))
    };
    let mut n = Escaped::default();
    if !cell
        .chars()
        .any(|c| c == MARKER_OPEN || (controls && control(c)))
    {
        return (cell.to_owned(), n);
    }
    let mut out = String::with_capacity(cell.len() + 16);
    for c in cell.chars() {
        if c == MARKER_OPEN {
            out.push_str(&hidden::marker(c));
            n.reserved += 1;
        } else if controls && control(c) {
            out.push_str(&hidden::marker(c));
            n.controls += 1;
        } else {
            out.push(c);
        }
    }
    (out, n)
}

/// One table's columns, in order.
#[derive(Debug, Clone, Copy)]
pub struct TableDef {
    pub stem: TableStem,
    pub columns: &'static [(&'static str, Kind)],
}

use Kind::{Content as C, Int as I, IntList as L, Iso as T, Offset as O, Token as K};

const MESSAGES: &[(&str, Kind)] = &[
    ("conversation", I),
    ("item", I),
    ("also_in", L),
    ("also_in_elsewhere", I),
    ("item_kind", K),
    ("item_kind_code", I),
    ("date_local", T),
    ("utc_offset", O),
    ("date_utc", T),
    ("date_raw", I),
    ("date_unit", K),
    ("date_unit_code", I),
    ("date_state", K),
    ("before_2007", K),
    ("planned_time", K),
    ("order_tie", K),
    ("date_edited_raw", I),
    ("date_edited_state", K),
    ("date_retracted_raw", I),
    ("date_retracted_state", K),
    ("direction", K),
    ("direction_raw", I),
    ("direction_recorded_by", K),
    ("sender_reason", K),
    ("sender_reason_code", I),
    ("sender_handle", I),
    ("sender_address", C),
    ("service", K),
    ("service_code", I),
    ("subject", C),
    ("subject_state", K),
    ("account", C),
    ("account_state", K),
    ("text", C),
    ("text_state", K),
    ("text_reason", K),
    ("masked_pieces", I),
    ("text_parts", I),
    (NEUTRALISED, K),
    ("lossy", K),
    ("bidi_controls", K),
    ("invisible", K),
    ("formatting", K),
    ("sources_differ", K),
    ("alternative_text", C),
    ("alternative_text_state", K),
    ("display_source", K),
    ("attributed_outcome", K),
    ("run_issue", K),
    ("edit_state", K),
    ("edited_parts", L),
    ("unsent_parts", L),
    ("earlier_text_state", K),
    ("earlier_text", C),
    ("history_state", K),
    ("captured_versions", I),
    ("phone_entries", I),
    ("labels", K),
    ("recently_deleted_sessions", L),
    ("reply_to", I),
    ("reply_resolution", K),
    ("reply_in_export", K),
    ("reply_count_in_export", I),
    ("reply_count_elsewhere", I),
    ("assoc_kind", K),
    ("assoc_visibility", K),
    ("target", I),
    ("target_in_export", K),
    ("folded_into", I),
    ("reactions_derived", C),
    ("reactions_outside_scope", K),
    ("attachment_count", I),
    ("event_kind", K),
    ("event_handles", L),
    ("schedule_type", I),
    ("schedule_type_state", K),
    ("app_content", K),
    ("app_content_state", K),
    ("effect", K),
    ("effect_state", K),
    ("derived_by", I),
    ("decoder_version", I),
    ("source_archive_id", I),
    ("source_guid", C),
    // v1 additions after the spec's list.
    ("direction_state", K),
    ("unsent_whole", K),
    ("part_count", I),
    ("stored_versions", I),
    ("earlier_text_session", I),
    ("reply_count_state", K),
    ("reactions_state", K),
    ("reactions_not_included_rows", I),
    ("reaction_rows", I),
    ("unpaired_removals", L),
    ("assoc_determinable", K),
    ("assoc_shape", K),
    ("assoc_target_part", I),
    ("assoc_emoji", C),
    ("event_title", C),
    ("event_title_state", K),
    ("date_edited_unit", K),
    ("date_retracted_unit", K),
    ("outside_zone_window", K),
    ("reply_in_conversation", K),
    ("target_in_conversation", K),
    ("join_date_raw", I),
    ("join_date_unit", K),
    ("join_date_state", K),
    ("attributed_error", K),
    ("display_none_reason", K),
    ("text_raw_state", K),
    ("source_assoc_guid", C),
    ("source_thread_originator_guid", C),
    ("source_reply_to_guid", C),
];

const SEGMENTS: &[(&str, Kind)] = &[
    ("item", I),
    ("segment", I),
    ("kind", K),
    ("kind_code", I),
    ("text", C),
    ("withheld", K),
    ("attachment", I),
    ("transcription", K),
    ("mention", K),
    ("url", K),
    ("url_hidden", K),
    ("one_time_code", K),
    ("styles", K),
    ("utf16_start", I),
    ("utf16_len", I),
    (NEUTRALISED, K),
];

const ATTACHMENTS: &[(&str, Kind)] = &[
    ("item", I),
    ("position", I),
    ("attachment", I),
    ("name", C),
    ("name_state", K),
    ("mime_type", C),
    ("mime_type_state", K),
    ("uti", C),
    ("uti_state", K),
    ("total_bytes", I),
    ("total_bytes_state", K),
    ("stored_bytes", I),
    ("outcome", K),
    ("outcome_code", I),
    ("file", I),
    ("path", C),
    ("sniffed", K),
    ("access", K),
    ("access_reason", K),
    ("card_state", K),
    ("card_state_code", I),
    ("older_backup_held_by", I),
    ("older_backup_newest", I),
    ("location_lookup", K),
    ("unavailable_lookup", I),
    ("unavailable_copy", I),
    ("placed_inline", K),
    ("is_sticker", I),
    ("is_sticker_state", K),
    ("hide_attachment", I),
    ("hide_attachment_state", K),
    ("is_outgoing", I),
    ("is_outgoing_state", K),
    ("transfer_state", I),
    ("transfer_state_state", K),
    ("emoji_description", C),
    ("emoji_description_state", K),
    ("created_raw", I),
    ("created_unit", K),
    ("created_state", K),
    ("start_raw", I),
    ("start_unit", K),
    ("start_state", K),
    ("held_by", I),
    ("verify_state", K),
    ("verify_may_be_stale", K),
    ("source_archive_id", I),
    ("source_guid", C),
    ("source_original_guid", C),
    (NEUTRALISED, K),
];

const OBSERVATIONS: &[(&str, Kind)] = &[
    ("item", I),
    ("attachment", I),
    ("observation", I),
    ("session", I),
    ("lookup", K),
    ("lookup_code", I),
    ("copy", K),
    ("copy_code", I),
    ("path_class", K),
    ("size_matched_manifest", K),
    ("holds_stored_file", K),
    (NEUTRALISED, K),
];

const REACTIONS: &[(&str, Kind)] = &[
    ("item", I),
    ("reaction", I),
    ("kind", K),
    ("kind_code", I),
    ("adds", K),
    ("emoji", C),
    ("emoji_state", K),
    ("direction", K),
    ("direction_state", K),
    ("direction_raw", I),
    ("handle", I),
    ("date_local", T),
    ("utc_offset", O),
    ("date_utc", T),
    ("date_raw", I),
    ("date_unit", K),
    ("date_state", K),
    ("target_part", I),
    (NEUTRALISED, K),
];

const EDIT_VERSIONS: &[(&str, Kind)] = &[
    ("item", I),
    ("version", I),
    ("entry", I),
    ("current", K),
    ("first_session", I),
    ("last_session", I),
    ("text_state", K),
    ("text", C),
    ("text_unchanged", K),
    ("edit_label", K),
    ("edit_texts", K),
    ("part", I),
    ("entry_ordinal", I),
    ("at_kind", K),
    ("at_raw_type", K),
    ("at_raw", Kind::Real),
    (NEUTRALISED, K),
    // 1.1.0: the stored `display_none_reason` of a version without text.
    ("display_none_reason", K),
];

const VALUE_HISTORY: &[(&str, Kind)] = &[
    ("entity", K),
    ("conversation", I),
    ("item", I),
    ("attachment", I),
    ("column", K),
    ("column_code", I),
    ("state", K),
    ("value", C),
    ("value_state", K),
    ("first_session", I),
    ("withheld_reason", K),
    (NEUTRALISED, K),
];

const CONVERSATIONS: &[(&str, Kind)] = &[
    ("conversation", I),
    ("name", C),
    ("name_state", K),
    ("chat_identifier", C),
    ("chat_identifier_state", K),
    ("service_name", C),
    ("service_name_state", K),
    ("service", K),
    ("service_code", I),
    ("style", I),
    ("style_state", K),
    ("is_filtered", I),
    ("is_filtered_state", K),
    ("is_archived", I),
    ("is_archived_state", K),
    ("is_blackholed", I),
    ("is_blackholed_state", K),
    ("is_pending_review", I),
    ("is_pending_review_state", K),
    ("is_recovered", I),
    ("is_recovered_state", K),
    ("properties_bytes", I),
    ("properties_bytes_state", K),
    ("participants", L),
    ("source_archive_id", I),
    ("source_guid", C),
    ("source_room_name", C),
    ("source_group_id", C),
    (NEUTRALISED, K),
];

const HANDLES: &[(&str, Kind)] = &[
    ("handle", I),
    ("address", C),
    ("address_state", K),
    ("service", C),
    ("service_state", K),
    ("service_class", K),
    ("service_class_code", I),
    ("country", C),
    ("country_state", K),
    ("source_archive_id", I),
    ("source_uncanonicalized_id", C),
    ("source_person_centric_id", C),
    (NEUTRALISED, K),
];

/// Every CSV file of a bundle, in write order.
pub const TABLES: &[TableDef] = &[
    TableDef {
        stem: TableStem::Conversations,
        columns: CONVERSATIONS,
    },
    TableDef {
        stem: TableStem::Handles,
        columns: HANDLES,
    },
    TableDef {
        stem: TableStem::Messages,
        columns: MESSAGES,
    },
    TableDef {
        stem: TableStem::Segments,
        columns: SEGMENTS,
    },
    TableDef {
        stem: TableStem::Attachments,
        columns: ATTACHMENTS,
    },
    TableDef {
        stem: TableStem::AttachmentObservations,
        columns: OBSERVATIONS,
    },
    TableDef {
        stem: TableStem::Reactions,
        columns: REACTIONS,
    },
    TableDef {
        stem: TableStem::EditVersions,
        columns: EDIT_VERSIONS,
    },
    TableDef {
        stem: TableStem::ValueHistory,
        columns: VALUE_HISTORY,
    },
];

/// The column contract as text, one `file<TAB>column<TAB>kind` line per
/// column: the form `tests/csv_columns.pin` holds.
pub fn columns_text() -> String {
    let mut out = String::new();
    for t in TABLES {
        let file = BundlePath::format_file(Format::Csv, FileStem::Table(t.stem), FileExt::Csv);
        for (name, kind) in t.columns {
            out.push_str(&format!("{}\t{name}\t{}\n", file.to_slash(), kind.name()));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Cell values
// ---------------------------------------------------------------------------

/// A code as a token (`unrecognisedCode:<n>` for a number this build does
/// not know).
fn tok(c: Code) -> String {
    if c.is_unrecognised() {
        format!("{}:{}", c.token, c.code)
    } else {
        c.token.to_owned()
    }
}

fn otok(c: Option<Code>) -> String {
    c.map(tok).unwrap_or_default()
}

fn b(v: bool) -> String {
    if v { "true" } else { "false" }.to_owned()
}

fn ob(v: Option<bool>) -> String {
    v.map(b).unwrap_or_default()
}

fn on(v: Option<u64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_default()
}

fn dec(d: &Option<Decimal>) -> String {
    d.as_ref()
        .map(|d| d.as_str().to_owned())
        .unwrap_or_default()
}

/// `;`-joined (no `.join(`, which the source rules keep for path
/// building in `naming.rs`).
fn list<I: IntoIterator<Item = String>>(items: I) -> String {
    let mut out = String::new();
    for (i, s) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(';');
        }
        out.push_str(&s);
    }
    out
}

fn ords(v: &[Ordinal]) -> String {
    list(v.iter().map(|n| n.to_string()))
}

fn decs(v: &[Decimal]) -> String {
    list(v.iter().map(|d| d.as_str().to_owned()))
}

fn text(v: &TextValue) -> String {
    v.text
        .as_ref()
        .map(|t| t.as_str().to_owned())
        .unwrap_or_default()
}

fn offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let a = seconds.unsigned_abs();
    let (h, m, s) = (a / 3600, a / 60 % 60, a % 60);
    if s == 0 {
        format!("{sign}{h:02}:{m:02}")
    } else {
        format!("{sign}{h:02}:{m:02}:{s:02}")
    }
}

fn msg_ref(r: &Option<MessageRefRecord>) -> (String, String) {
    match r {
        Some(MessageRefRecord::InExport { n }) => (n.to_string(), b(true)),
        Some(MessageRefRecord::NotInExport) => (String::new(), b(false)),
        None => (String::new(), String::new()),
    }
}

/// A row being built: cells by column name. Every value set with
/// [`Row::set`] gets the profile's escapes ([`escape`]); a text that
/// holds the export's own placeholders is built with [`Row::esc`] for its
/// recorded fragments and set with [`Row::set_rendered`], so a
/// placeholder is never escaped and a recorded `⟨` always is.
struct Row {
    def: &'static [(&'static str, Kind)],
    cells: Vec<String>,
    /// Where the last column was found: cells are mostly set in column
    /// order, so the search starts there.
    hint: usize,
    controls: bool,
    escaped: Escaped,
}

impl Row {
    fn new(def: &'static [(&'static str, Kind)], controls: bool) -> Row {
        Row {
            def,
            cells: vec![String::new(); def.len()],
            hint: 0,
            controls,
            escaped: Escaped::default(),
        }
    }

    /// A recorded fragment with the profile's escapes.
    fn esc(&mut self, t: &str) -> String {
        let (s, n) = escape(t, self.controls);
        self.escaped.controls += n.controls;
        self.escaped.reserved += n.reserved;
        s
    }

    /// Set `column`'s cell, escaped.
    fn set(&mut self, column: &str, v: String) -> Result<(), ExportError> {
        let v = self.esc(&v);
        self.set_rendered(column, v)
    }

    /// Set `column`'s cell as it is (its recorded parts already escaped);
    /// an unknown column is a bug (`RowOrder`).
    fn set_rendered(&mut self, column: &str, v: String) -> Result<(), ExportError> {
        let n = self.def.len();
        let i = (0..n)
            .map(|k| (self.hint + k) % n)
            .find(|&i| self.def.get(i).is_some_and(|(c, _)| *c == column))
            .ok_or(ExportError::Limit(LimitKind::RowOrder))?;
        self.hint = i + 1;
        if let Some(c) = self.cells.get_mut(i) {
            *c = v;
        }
        Ok(())
    }

    fn raw_int(&mut self, column: &str, state: &str, r: &RawIntRecord) -> Result<(), ExportError> {
        self.set(column, dec(&r.raw))?;
        self.set(state, otok(r.state))
    }

    fn text_value(&mut self, column: &str, state: &str, v: &TextValue) -> Result<(), ExportError> {
        self.set(column, text(v))?;
        self.set(state, otok(v.missing))
    }

    fn raw_time(
        &mut self,
        raw: &str,
        unit: &str,
        state: &str,
        t: &RawTimeRecord,
    ) -> Result<(), ExportError> {
        self.set(raw, dec(&t.raw))?;
        self.set(unit, otok(t.unit))?;
        self.set(state, otok(t.state))
    }

    fn time(&mut self, t: &TimeRecord) -> Result<(), ExportError> {
        self.set("date_local", t.local.clone().unwrap_or_default())?;
        let off = if t.local.is_some() {
            t.offset_seconds.map(offset).unwrap_or_default()
        } else {
            String::new()
        };
        self.set("utc_offset", off)?;
        self.set("date_utc", t.utc.clone().unwrap_or_default())?;
        self.set("date_raw", dec(&t.raw))?;
        self.set("date_unit", otok(t.unit))?;
        self.set("date_state", otok(t.state))
    }
}

/// One open table.
struct Table {
    w: csv::Writer<StagedFile>,
    def: &'static [(&'static str, Kind)],
    neutralised_at: Option<usize>,
    rows: u64,
}

fn csv_err(e: csv::Error) -> ExportError {
    match e.kind() {
        csv::ErrorKind::Io(e) => ExportError::io(e),
        _ => ExportError::Limit(LimitKind::NotSerializable),
    }
}

fn io(e: std::io::Error) -> ExportError {
    ExportError::io(&e)
}

fn order() -> ExportError {
    ExportError::Limit(LimitKind::RowOrder)
}

/// What the writer changed, for the manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Counts {
    neutralised: u64,
    controls: u64,
    reserved: u64,
}

/// The CSV writer.
pub struct CsvWriter {
    profile: CsvProfile,
    words: Words,
    /// Handle ordinal → its address (raw, for `sender_address`).
    addresses: BTreeMap<Ordinal, String>,
    tables: BTreeMap<TableStem, Table>,
    /// Messages whose intrinsic rows are written.
    seen: BTreeSet<Ordinal>,
    /// The message the next versions belong to, and their count so far.
    versions_of: Option<(Ordinal, u64)>,
    in_file: bool,
    file_items: u64,
    file_folded: u64,
    counts: Counts,
    report: WriterReport,
}

impl std::fmt::Debug for CsvWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CsvWriter")
            .field("profile", &self.profile)
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

impl Default for CsvWriter {
    fn default() -> CsvWriter {
        CsvWriter::new()
    }
}

impl CsvWriter {
    pub fn new() -> CsvWriter {
        CsvWriter {
            profile: CsvProfile::Spreadsheet,
            words: Words::new(),
            addresses: BTreeMap::new(),
            tables: BTreeMap::new(),
            seen: BTreeSet::new(),
            versions_of: None,
            in_file: false,
            file_items: 0,
            file_folded: 0,
            counts: Counts::default(),
            report: WriterReport::new(Format::Csv),
        }
    }

    fn row(&self, def: &'static [(&'static str, Kind)]) -> Row {
        Row::new(def, self.profile == CsvProfile::Spreadsheet)
    }

    /// Neutralise and write `row` to its table.
    fn write(&mut self, stem: TableStem, row: Row) -> Result<(), ExportError> {
        let spreadsheet = self.profile == CsvProfile::Spreadsheet;
        self.counts.controls += row.escaped.controls;
        self.counts.reserved += row.escaped.reserved;
        let t = self.tables.get_mut(&stem).ok_or_else(order)?;
        if t.def.as_ptr() != row.def.as_ptr() {
            return Err(order());
        }
        let mut cells = row.cells;
        let mut neutralised: Vec<String> = Vec::new();
        for (i, (cell, (name, kind))) in cells.iter_mut().zip(t.def).enumerate() {
            if Some(i) == t.neutralised_at {
                continue;
            }
            if spreadsheet && !kind.matches(cell) && triggers(cell) {
                cell.insert(0, '\'');
                self.counts.neutralised += 1;
                neutralised.push((*name).to_owned());
            }
        }
        if let Some(i) = t.neutralised_at {
            if let Some(c) = cells.get_mut(i) {
                *c = list(neutralised);
            }
        }
        t.w.write_record(&cells).map_err(csv_err)?;
        t.rows += 1;
        Ok(())
    }

    fn conversation_rows(&mut self, r: &ConversationRecord) -> Result<(), ExportError> {
        let mut row = self.row(CONVERSATIONS);
        row.set("conversation", r.n.to_string())?;
        row.text_value("name", "name_state", &r.name)?;
        row.text_value(
            "chat_identifier",
            "chat_identifier_state",
            &r.chat_identifier,
        )?;
        row.text_value("service_name", "service_name_state", &r.service_name)?;
        row.set("service", otok(r.service))?;
        row.set(
            "service_code",
            r.service.map(|c| c.code.to_string()).unwrap_or_default(),
        )?;
        row.raw_int("style", "style_state", &r.style)?;
        row.raw_int("is_filtered", "is_filtered_state", &r.is_filtered)?;
        row.raw_int("is_archived", "is_archived_state", &r.is_archived)?;
        row.raw_int("is_blackholed", "is_blackholed_state", &r.is_blackholed)?;
        row.raw_int(
            "is_pending_review",
            "is_pending_review_state",
            &r.is_pending_review,
        )?;
        row.raw_int("is_recovered", "is_recovered_state", &r.is_recovered)?;
        row.raw_int(
            "properties_bytes",
            "properties_bytes_state",
            &r.properties_bytes,
        )?;
        row.set("participants", ords(&r.participants))?;
        if let Included::Included { value: s } = &r.source {
            row.set("source_archive_id", s.archive_id.as_str().to_owned())?;
            row.set("source_guid", text(&s.guid))?;
            row.set("source_room_name", text(&s.room_name))?;
            row.set("source_group_id", text(&s.group_id))?;
        }
        self.write(TableStem::Conversations, row)?;
        for h in &r.name_history {
            let mut row = self.row(VALUE_HISTORY);
            row.set("entity", "conversation".to_owned())?;
            row.set("conversation", r.n.to_string())?;
            self.value_history(row, h)?;
        }
        Ok(())
    }

    fn value_history(&mut self, mut row: Row, h: &ValueHistoryRecord) -> Result<(), ExportError> {
        match h {
            ValueHistoryRecord::Value {
                column,
                value,
                first_session,
            } => {
                row.set("column", tok(*column))?;
                row.set("column_code", column.code.to_string())?;
                row.set("state", "value".to_owned())?;
                row.text_value("value", "value_state", value)?;
                row.set("first_session", first_session.to_string())?;
            }
            ValueHistoryRecord::Withheld { column, reason } => {
                row.set("column", tok(*column))?;
                row.set("column_code", column.code.to_string())?;
                row.set("state", "withheld".to_owned())?;
                row.set("withheld_reason", tok(*reason))?;
            }
        }
        self.write(TableStem::ValueHistory, row)
    }

    /// A body's text: the text segments, with our placeholders for the
    /// others; and the withheld segments' count.
    fn render_segments(&self, row: &mut Row, segments: &[SegmentRecord]) -> (String, u64) {
        let mut out = String::new();
        let mut masked = 0;
        for s in segments {
            let k = s.kind;
            if is(k, SegmentKindCode::Text) {
                if let Some(t) = &s.text {
                    out.push_str(&row.esc(t.as_str()));
                }
            } else if is(k, SegmentKindCode::Withheld) {
                masked += 1;
                out.push_str(&placeholder(phrase("oneTimeCodeMarker")));
            } else if is(k, SegmentKindCode::Attachment) && s.attachment.is_some() {
                out.push_str(&placeholder(&format!(
                    "Attachment {}",
                    s.attachment.unwrap_or_default()
                )));
            } else {
                out.push_str(&placeholder(&self.words.say("segmentKind", k)));
            }
        }
        (out, masked)
    }

    fn render_pieces(row: &mut Row, pieces: &[PieceRecord]) -> String {
        let mut out = String::new();
        for p in pieces {
            match &p.text {
                Some(t) if is(p.kind, SegmentKindCode::Text) => out.push_str(&row.esc(t.as_str())),
                _ => out.push_str(&placeholder(phrase("oneTimeCodeMarker"))),
            }
        }
        out
    }

    fn chip(row: &mut Row, c: &ChipRecord) -> String {
        let mut s = tok(c.kind);
        if let Some(e) = c.emoji.as_ref().and_then(|e| e.text.as_ref()) {
            s.push(' ');
            s.push_str(&row.esc(e.as_str()));
        }
        s.push_str(" from ");
        match c.sender.handle {
            Some(h) => s.push_str(&format!("handle {h}")),
            None => s.push_str(&tok(c.sender.kind)),
        }
        s.push_str(&format!(" ({})", tok(c.state)));
        s
    }

    fn message_row(&self, it: &ItemRecord) -> Result<Row, ExportError> {
        let mut row = self.row(MESSAGES);
        let cx = &it.context;
        row.set("conversation", on(cx.conversation))?;
        row.set("item", it.n.to_string())?;
        row.set("also_in", ords(&cx.also_in.in_export))?;
        row.set("also_in_elsewhere", cx.also_in.elsewhere.to_string())?;
        row.set("item_kind", tok(it.item))?;
        row.set("item_kind_code", it.item.code.to_string())?;
        row.time(&it.date)?;
        row.set(
            "date_unit_code",
            it.date.unit.map(|c| c.code.to_string()).unwrap_or_default(),
        )?;
        row.set("before_2007", b(it.date.before2007))?;
        row.set("planned_time", b(it.date.planned))?;
        row.set("outside_zone_window", b(it.date.outside_zone_window))?;
        row.set("order_tie", b(cx.order_tie))?;
        row.raw_time(
            "date_edited_raw",
            "date_edited_unit",
            "date_edited_state",
            &it.date_edited,
        )?;
        row.raw_time(
            "date_retracted_raw",
            "date_retracted_unit",
            "date_retracted_state",
            &it.date_retracted,
        )?;
        let d = &it.direction;
        row.set("direction", tok(d.token))?;
        row.set("direction_raw", dec(&d.raw))?;
        row.set("direction_state", otok(d.state))?;
        row.set(
            "direction_recorded_by",
            match d.recorded_by {
                RecordedByRecord::SingleLineage => "singleLineage".to_owned(),
                RecordedByRecord::Lineage { lineage, session } => {
                    format!("lineage:{lineage}:session:{session}")
                }
            },
        )?;
        row.set("sender_reason", tok(it.sender.reason))?;
        row.set("sender_reason_code", it.sender.reason.code.to_string())?;
        row.set("sender_handle", on(it.sender.handle))?;
        if let Some(h) = it.sender.handle {
            row.set(
                "sender_address",
                self.addresses.get(&h).cloned().unwrap_or_default(),
            )?;
        }
        row.set("service", otok(it.service))?;
        row.set(
            "service_code",
            it.service.map(|c| c.code.to_string()).unwrap_or_default(),
        )?;
        row.text_value("subject", "subject_state", &it.subject)?;
        row.text_value("account", "account_state", &it.account)?;
        match &it.body {
            BodyRecord::Text {
                segments,
                formatting,
                bidi_controls,
                invisible,
                lossy,
                ..
            } => {
                let (t, masked) = self.render_segments(&mut row, segments);
                row.set_rendered("text", t)?;
                row.set("text_state", "text".to_owned())?;
                row.set("masked_pieces", masked.to_string())?;
                row.set("text_parts", segments.len().to_string())?;
                row.set("lossy", b(*lossy))?;
                row.set("bidi_controls", b(*bidi_controls))?;
                row.set("invisible", b(*invisible))?;
                row.set("formatting", tok(*formatting))?;
            }
            BodyRecord::NoText { reason } => {
                row.set("text_state", "noText".to_owned())?;
                row.set("text_reason", tok(*reason))?;
                row.set("masked_pieces", "0".to_owned())?;
                row.set("text_parts", "0".to_owned())?;
            }
        }
        let f = &it.body_facts;
        row.set("sources_differ", ob(f.sources_differ))?;
        row.set("display_source", otok(f.display_source))?;
        row.set("display_none_reason", otok(f.display_none_reason))?;
        row.set("attributed_outcome", otok(f.attributed_outcome))?;
        row.set("attributed_error", otok(f.attributed_error))?;
        row.set("run_issue", otok(f.run_issue))?;
        row.set("text_raw_state", otok(f.text_raw_state))?;
        match &it.alternative_text {
            AltTextRecord::None => row.set("alternative_text_state", "none".to_owned())?,
            AltTextRecord::Text { source, text: t } => {
                row.set("alternative_text", text(t))?;
                let mut s = tok(*source);
                if let Some(m) = t.missing {
                    s.push(':');
                    s.push_str(&tok(m));
                }
                row.set("alternative_text_state", s)?;
            }
            AltTextRecord::Withheld { why, reason } => {
                let mut s = format!("withheld:{}", tok(*why));
                if let Some(r) = reason {
                    s.push(':');
                    s.push_str(&tok(*r));
                }
                row.set("alternative_text_state", s)?;
            }
        }
        let e = &it.edit;
        row.set("edit_state", otok(e.label))?;
        row.set("edited_parts", decs(&e.edited_parts))?;
        row.set("unsent_parts", decs(&e.unsent_parts))?;
        row.set("unsent_whole", b(e.unsent_whole))?;
        row.set(
            "part_count",
            e.part_count.map(|n| n.to_string()).unwrap_or_default(),
        )?;
        row.set("stored_versions", e.stored_versions.to_string())?;
        match &it.earlier_text {
            EarlierRecord::None => row.set("earlier_text_state", "none".to_owned())?,
            EarlierRecord::Available => row.set("earlier_text_state", "available".to_owned())?,
            EarlierRecord::Withheld { reason } => {
                row.set("earlier_text_state", format!("withheld:{}", tok(*reason)))?
            }
            EarlierRecord::Revealed {
                reason,
                pieces,
                session,
            } => {
                row.set("earlier_text_state", format!("revealed:{}", tok(*reason)))?;
                let t = Self::render_pieces(&mut row, pieces);
                row.set_rendered("earlier_text", t)?;
                row.set("earlier_text_session", session.to_string())?;
            }
        }
        let (state, h) = match &it.history {
            Included::Included { value } => ("included", value),
            Included::NotIncluded { counts, .. } => ("notIncluded", counts),
        };
        row.set("history_state", state.to_owned())?;
        row.set("captured_versions", h.captured_versions.to_string())?;
        row.set("phone_entries", h.phone_entries.to_string())?;
        row.set(
            "labels",
            list(it.labels.iter().map(|l| {
                let mut s = tok(l.kind);
                if let Some(v) = &l.value {
                    s.push(':');
                    s.push_str(v.as_str());
                }
                if let Some(n) = l.lineage {
                    s.push_str(&format!(":lineage:{n}"));
                }
                s
            })),
        )?;
        row.set(
            "recently_deleted_sessions",
            list(it.recently_deleted.iter().map(|r| r.session.to_string())),
        )?;
        if let Some(r) = &it.reply {
            row.set("reply_resolution", tok(r.resolution))?;
            let (to, in_export) = msg_ref(&r.originator);
            row.set("reply_to", to)?;
            row.set("reply_in_export", in_export)?;
        }
        row.set("reply_in_conversation", ob(cx.reply_in_conversation))?;
        match &it.reply_count {
            Determinable::Determined { value } => {
                row.set("reply_count_state", "determined".to_owned())?;
                row.set("reply_count_in_export", value.in_export.to_string())?;
                row.set("reply_count_elsewhere", value.elsewhere.to_string())?;
            }
            Determinable::NotDeterminable { reason } => {
                let r = match reason {
                    NotDeterminableReason::Unkeyed => "unkeyed",
                };
                row.set("reply_count_state", format!("notDeterminable:{r}"))?;
            }
        }
        if let Some(a) = &it.association {
            row.set("assoc_determinable", b(a.determinable))?;
            row.set("assoc_kind", otok(a.kind))?;
            row.set("assoc_shape", otok(a.shape))?;
            row.set("assoc_visibility", tok(a.visibility))?;
            let (target, in_export) = msg_ref(&a.target);
            row.set("target", target)?;
            row.set("target_in_export", in_export)?;
            row.set("assoc_target_part", dec(&a.target_part))?;
            if let Some(e) = &a.emoji {
                row.set("assoc_emoji", text(e))?;
            }
        }
        row.set("target_in_conversation", ob(cx.target_in_conversation))?;
        row.set("folded_into", on(cx.folded_into))?;
        match &it.reactions {
            Included::Included { value } => {
                row.set("reactions_state", "included".to_owned())?;
                let chips: Vec<String> = value
                    .chips
                    .iter()
                    .map(|c| Self::chip(&mut row, c))
                    .collect();
                let mut joined = String::new();
                for (i, c) in chips.iter().enumerate() {
                    if i > 0 {
                        joined.push_str("; ");
                    }
                    joined.push_str(c);
                }
                row.set_rendered("reactions_derived", joined)?;
                row.set(
                    "reactions_outside_scope",
                    b(value.chips.iter().any(|c| c.includes_rows_outside_scope)),
                )?;
                row.set("reaction_rows", value.rows.len().to_string())?;
                row.set(
                    "unpaired_removals",
                    list(value.unpaired.iter().filter_map(|r| match r {
                        MessageRefRecord::InExport { n } => Some(n.to_string()),
                        MessageRefRecord::NotInExport => None,
                    })),
                )?;
            }
            Included::NotIncluded { counts, .. } => {
                row.set("reactions_state", "notIncluded".to_owned())?;
                row.set("reactions_not_included_rows", counts.rows.to_string())?;
            }
        }
        row.set("attachment_count", it.attachments.len().to_string())?;
        if let Some(ev) = &it.event {
            row.set("event_kind", tok(ev.kind))?;
            row.set("event_handles", ords(&ev.handles))?;
            if let Some(t) = &ev.title {
                row.text_value("event_title", "event_title_state", t)?;
            }
        }
        row.raw_int("schedule_type", "schedule_type_state", &it.schedule_type)?;
        row.set("app_content", ob(it.app_content.value))?;
        row.set("app_content_state", otok(it.app_content.state))?;
        row.set("effect", ob(it.effect.value))?;
        row.set("effect_state", otok(it.effect.state))?;
        row.set("derived_by", it.derivation.derived_by.to_string())?;
        row.set(
            "decoder_version",
            it.derivation
                .decoder_version
                .map(|v| v.to_string())
                .unwrap_or_default(),
        )?;
        if let Some(j) = &cx.join_date {
            row.set("join_date_raw", dec(&j.raw))?;
            row.set("join_date_unit", otok(j.unit))?;
            row.set("join_date_state", otok(j.state))?;
        }
        if let Included::Included { value: s } = &it.source {
            row.set("source_archive_id", s.archive_id.as_str().to_owned())?;
            row.set("source_guid", text(&s.guid))?;
            row.set("source_assoc_guid", text(&s.assoc_guid))?;
            row.set(
                "source_thread_originator_guid",
                text(&s.thread_originator_guid),
            )?;
            row.set("source_reply_to_guid", text(&s.reply_to_guid))?;
        }
        Ok(row)
    }

    /// The rows of a message's intrinsic part (first placement only).
    fn intrinsic_rows(&mut self, it: &ItemRecord) -> Result<(), ExportError> {
        if let BodyRecord::Text { segments, .. } = &it.body {
            for (i, s) in segments.iter().enumerate() {
                let mut row = self.row(SEGMENTS);
                row.set("item", it.n.to_string())?;
                row.set("segment", (i + 1).to_string())?;
                row.set("kind", tok(s.kind))?;
                row.set("kind_code", s.kind.code.to_string())?;
                row.set(
                    "text",
                    s.text
                        .as_ref()
                        .map(|t| t.as_str().to_owned())
                        .unwrap_or_default(),
                )?;
                row.set("withheld", otok(s.withheld))?;
                row.set("attachment", on(s.attachment))?;
                row.set("transcription", b(s.transcription))?;
                row.set("mention", b(s.mention))?;
                row.set("url", b(s.url))?;
                row.set("url_hidden", b(s.url_hidden))?;
                row.set("one_time_code", b(s.one_time_code))?;
                row.set("styles", list(s.styles.iter().map(|c| tok(*c))))?;
                row.set("utf16_start", s.utf16_start.to_string())?;
                row.set("utf16_len", s.utf16_len.to_string())?;
                self.write(TableStem::Segments, row)?;
            }
        }
        for (pos, a) in it.attachments.iter().enumerate() {
            self.attachment_rows(it.n, pos + 1, a)?;
        }
        if let Included::Included { value } = &it.reactions {
            for r in &value.rows {
                let mut row = self.row(REACTIONS);
                row.set("item", it.n.to_string())?;
                row.set("reaction", r.n.to_string())?;
                row.set("kind", tok(r.kind))?;
                row.set("kind_code", r.kind.code.to_string())?;
                row.set("adds", b(r.adds))?;
                if let Some(e) = &r.emoji {
                    row.text_value("emoji", "emoji_state", e)?;
                }
                row.set("direction", tok(r.direction.token))?;
                row.set("direction_state", otok(r.direction.state))?;
                row.set("direction_raw", dec(&r.direction.raw))?;
                row.set("handle", on(r.handle))?;
                row.time(&r.date)?;
                row.set("target_part", dec(&r.target_part))?;
                self.write(TableStem::Reactions, row)?;
            }
        }
        for h in &it.value_history {
            let mut row = self.row(VALUE_HISTORY);
            row.set("entity", "message".to_owned())?;
            row.set("item", it.n.to_string())?;
            self.value_history(row, h)?;
        }
        Ok(())
    }

    fn attachment_rows(
        &mut self,
        item: Ordinal,
        pos: usize,
        a: &AttachmentRecord,
    ) -> Result<(), ExportError> {
        let mut row = self.row(ATTACHMENTS);
        row.set("item", item.to_string())?;
        row.set("position", pos.to_string())?;
        row.set("attachment", a.n.to_string())?;
        row.text_value("name", "name_state", &a.name)?;
        row.text_value("mime_type", "mime_type_state", &a.mime_type)?;
        row.text_value("uti", "uti_state", &a.uti)?;
        row.raw_int("total_bytes", "total_bytes_state", &a.total_bytes)?;
        row.set("stored_bytes", dec(&a.stored_bytes))?;
        row.set("outcome", tok(a.file.outcome))?;
        row.set("outcome_code", a.file.outcome.code.to_string())?;
        row.set("file", on(a.file.file))?;
        row.set(
            "path",
            a.file
                .path
                .as_ref()
                .map(|p| p.as_str().to_owned())
                .unwrap_or_default(),
        )?;
        row.set("sniffed", otok(a.file.sniffed))?;
        row.set("access", tok(a.access))?;
        row.set("access_reason", otok(a.access_reason))?;
        let c = &a.card;
        row.set("card_state", tok(c.state))?;
        row.set("card_state_code", c.state.code.to_string())?;
        row.set("older_backup_held_by", on(c.older_backup_held_by))?;
        row.set("older_backup_newest", on(c.older_backup_newest))?;
        row.set("location_lookup", otok(c.location_lookup))?;
        row.set("unavailable_lookup", dec(&c.unavailable_lookup))?;
        row.set("unavailable_copy", dec(&c.unavailable_copy))?;
        row.set("placed_inline", b(a.placed_inline))?;
        row.raw_int("is_sticker", "is_sticker_state", &a.is_sticker)?;
        row.raw_int(
            "hide_attachment",
            "hide_attachment_state",
            &a.hide_attachment,
        )?;
        row.raw_int("is_outgoing", "is_outgoing_state", &a.is_outgoing)?;
        row.raw_int("transfer_state", "transfer_state_state", &a.transfer_state)?;
        row.text_value(
            "emoji_description",
            "emoji_description_state",
            &a.emoji_description,
        )?;
        row.raw_time("created_raw", "created_unit", "created_state", &a.created)?;
        row.raw_time("start_raw", "start_unit", "start_state", &a.start)?;
        row.set("held_by", on(a.held_by))?;
        if let Some(v) = &a.verify {
            row.set("verify_state", tok(v.state))?;
            row.set("verify_may_be_stale", b(v.may_be_stale))?;
        }
        if let Included::Included { value: s } = &a.source {
            row.set("source_archive_id", s.archive_id.as_str().to_owned())?;
            row.set("source_guid", text(&s.guid))?;
            row.set("source_original_guid", text(&s.original_guid))?;
        }
        self.write(TableStem::Attachments, row)?;
        for (i, o) in a.observations.iter().enumerate() {
            let mut row = self.row(OBSERVATIONS);
            row.set("item", item.to_string())?;
            row.set("attachment", a.n.to_string())?;
            row.set("observation", (i + 1).to_string())?;
            row.set("session", o.session.to_string())?;
            row.set("lookup", tok(o.lookup))?;
            row.set("lookup_code", o.lookup.code.to_string())?;
            row.set("copy", tok(o.copy))?;
            row.set("copy_code", o.copy.code.to_string())?;
            row.set("path_class", otok(o.path_class))?;
            row.set("size_matched_manifest", ob(o.size_matched_manifest))?;
            row.set("holds_stored_file", b(o.holds_stored_file))?;
            self.write(TableStem::AttachmentObservations, row)?;
        }
        for h in &a.history {
            let mut row = self.row(VALUE_HISTORY);
            row.set("entity", "attachment".to_owned())?;
            row.set("item", item.to_string())?;
            row.set("attachment", a.n.to_string())?;
            self.value_history(row, h)?;
        }
        Ok(())
    }

    fn version_rows(&mut self, v: &VersionRecord) -> Result<(), ExportError> {
        let index = match &mut self.versions_of {
            Some((m, n)) if *m == v.message => {
                *n += 1;
                *n
            }
            _ => {
                self.versions_of = Some((v.message, 1));
                1
            }
        };
        let base = |row: &mut Row| -> Result<(), ExportError> {
            row.set("item", v.message.to_string())?;
            row.set("version", index.to_string())?;
            row.set("current", b(v.current))?;
            row.set("first_session", v.first_session.to_string())?;
            row.set("last_session", v.last_session.to_string())
        };
        let mut row = self.row(EDIT_VERSIONS);
        base(&mut row)?;
        match &v.text {
            VersionTextRecord::Text { pieces } => {
                row.set("text_state", "text".to_owned())?;
                let t = Self::render_pieces(&mut row, pieces);
                row.set_rendered("text", t)?;
            }
            VersionTextRecord::Withheld { reason } => {
                row.set("text_state", format!("withheld:{}", tok(*reason)))?;
            }
            VersionTextRecord::NoText {
                reason,
                display_none_reason,
            } => {
                row.set("text_state", format!("noText:{}", tok(*reason)))?;
                row.set("display_none_reason", otok(*display_none_reason))?;
            }
        }
        row.set("text_unchanged", b(v.text_unchanged))?;
        row.set("edit_label", otok(v.edit_label))?;
        row.set("edit_texts", tok(v.edit_texts))?;
        self.write(TableStem::EditVersions, row)?;
        for (i, e) in v.edits.iter().enumerate() {
            let mut row = self.row(EDIT_VERSIONS);
            base(&mut row)?;
            row.set("entry", (i + 1).to_string())?;
            row.set("part", e.part.as_str().to_owned())?;
            row.set("entry_ordinal", e.ordinal.as_str().to_owned())?;
            row.set("at_kind", otok(e.at.kind))?;
            row.set("at_raw_type", e.at.raw_type.unwrap_or_default().to_owned())?;
            row.set("at_raw", dec(&e.at.raw))?;
            let (state, t) = match &e.text {
                EditEntryTextRecord::Text { pieces } => {
                    ("text".to_owned(), Self::render_pieces(&mut row, pieces))
                }
                EditEntryTextRecord::Withheld { reason } => {
                    (format!("withheld:{}", tok(*reason)), String::new())
                }
                EditEntryTextRecord::NoBody => ("noBody".to_owned(), String::new()),
                EditEntryTextRecord::Failed { decode_error } => {
                    (format!("failed:{}", tok(*decode_error)), String::new())
                }
                EditEntryTextRecord::NotMatched => ("notMatched".to_owned(), String::new()),
            };
            row.set("text_state", state)?;
            row.set_rendered("text", t)?;
            self.write(TableStem::EditVersions, row)?;
        }
        Ok(())
    }
}

impl ExportWriter for CsvWriter {
    fn format(&self) -> Format {
        Format::Csv
    }

    fn begin(&mut self, ctx: &ExportContext<'_>, out: &mut Staging) -> Result<(), ExportError> {
        if !self.tables.is_empty() {
            return Err(order());
        }
        self.profile = ctx.plan.options().csv_profile.ok_or(ExportError::Plan(
            crate::error::PlanRefusal::CsvProfileMismatch,
        ))?;
        for t in TABLES {
            let path = BundlePath::format_file(Format::Csv, FileStem::Table(t.stem), FileExt::Csv);
            let mut f = out.create(&path)?;
            if self.profile == CsvProfile::Spreadsheet {
                f.write_all("\u{FEFF}".as_bytes()).map_err(io)?;
            }
            let mut w = csv::WriterBuilder::new()
                .terminator(csv::Terminator::CRLF)
                .quote_style(csv::QuoteStyle::Always)
                .from_writer(f);
            w.write_record(t.columns.iter().map(|(n, _)| *n))
                .map_err(csv_err)?;
            self.tables.insert(
                t.stem,
                Table {
                    w,
                    def: t.columns,
                    neutralised_at: t.columns.iter().position(|(n, _)| *n == NEUTRALISED),
                    rows: 0,
                },
            );
        }
        Ok(())
    }

    fn handle(&mut self, h: &HandleRecord, _out: &mut Staging) -> Result<(), ExportError> {
        if let Some(a) = &h.address.text {
            self.addresses.insert(h.n, a.as_str().to_owned());
        }
        let mut row = self.row(HANDLES);
        row.set("handle", h.n.to_string())?;
        row.text_value("address", "address_state", &h.address)?;
        row.text_value("service", "service_state", &h.service)?;
        row.set("service_class", otok(h.service_class))?;
        row.set(
            "service_class_code",
            h.service_class
                .map(|c| c.code.to_string())
                .unwrap_or_default(),
        )?;
        row.text_value("country", "country_state", &h.country)?;
        if let Included::Included { value: s } = &h.source {
            row.set("source_archive_id", s.archive_id.as_str().to_owned())?;
            row.set("source_uncanonicalized_id", text(&s.uncanonicalized_id))?;
            row.set("source_person_centric_id", text(&s.person_centric_id))?;
        }
        self.write(TableStem::Handles, row)
    }

    fn begin_conversation(
        &mut self,
        c: ConversationFile<'_>,
        _out: &mut Staging,
    ) -> Result<(), ExportError> {
        if self.in_file || self.tables.is_empty() {
            return Err(order());
        }
        if let ConversationFile::Conversation(r) = c {
            self.conversation_rows(r)?;
        }
        self.in_file = true;
        self.file_items = 0;
        self.file_folded = 0;
        self.report.conversation_files += 1;
        Ok(())
    }

    fn item(&mut self, it: &ItemRecord, _out: &mut Staging) -> Result<(), ExportError> {
        if !self.in_file {
            return Err(order());
        }
        let row = self.message_row(it)?;
        self.write(TableStem::Messages, row)?;
        if self.seen.insert(it.n) {
            self.intrinsic_rows(it)?;
        }
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
        if !self.in_file {
            return Err(order());
        }
        self.version_rows(v)
    }

    fn end_conversation(
        &mut self,
        totals: &ConversationTotals,
        _out: &mut Staging,
    ) -> Result<(), ExportError> {
        if !self.in_file {
            return Err(order());
        }
        if totals.items != self.file_items || totals.folded_rows != self.file_folded {
            return Err(ExportError::Limit(LimitKind::RowCountMismatch));
        }
        self.in_file = false;
        Ok(())
    }

    fn finish(
        mut self: Box<Self>,
        _about: &AboutDraft<'_>,
        out: &mut Staging,
    ) -> Result<WriterReport, ExportError> {
        if self.in_file {
            return Err(ExportError::Limit(LimitKind::FileStillOpen));
        }
        let mut r = self.report.clone();
        for (stem, t) in std::mem::take(&mut self.tables) {
            let f = t.w.into_inner().map_err(|e| ExportError::io(e.error()))?;
            out.close(f)?;
            if stem == TableStem::Messages {
                r.counts.insert("csvMessageRows", t.rows);
            }
        }
        r.counts.insert("neutralisedCells", self.counts.neutralised);
        r.counts.insert("controlsEscaped", self.counts.controls);
        r.counts.insert("reservedEscaped", self.counts.reserved);
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_owasp_leading_character_triggers_after_whitespace_and_ignorables() {
        let leads = [
            "=", "+", "-", "@", "\t", "\r", "\n", "\u{FF1D}", "\u{FF0B}", "\u{FF0D}", "\u{FF20}",
            "\u{FE62}", "\u{FE63}", "\u{FE66}", "\u{FE6B}",
        ];
        for l in leads {
            assert!(triggers(&format!("{l}1+1")), "{l:?}");
        }
        // Skipped before the test: White_Space and Default_Ignorable code
        // points (the BOM, ZWSP, bidi controls, U+180E, soft hyphen, …).
        let skipped = [
            " ",
            "\u{A0}",
            "\u{3000}",
            "\u{2028}",
            "\u{FEFF}",
            "\u{200B}",
            "\u{200E}",
            "\u{202E}",
            "\u{2066}",
            "\u{180E}",
            "\u{AD}",
            "\u{2060}",
            "\u{FE0F}",
            "\u{E0001}",
            "  \u{200B} ",
        ];
        for s in skipped {
            for l in ["=", "+", "-", "@", "\u{FF1D}", "\u{FE62}"] {
                assert!(triggers(&format!("{s}{l}cmd")), "{s:?}{l:?}");
            }
        }
        for safe in [
            "",
            "a=1",
            "1-2",
            "'=x",
            "\u{27E8}U+001B\u{27E9}=",
            "x\t=",
            "(=)",
            " a",
        ] {
            assert!(!triggers(safe), "{safe:?}");
        }
        // Whitespace alone does not trigger, nor does a TAB after a space.
        assert!(!triggers("   "));
        assert!(!triggers(" \u{200B}x"));
    }

    #[test]
    fn typed_columns_are_exempt_only_when_they_match_their_grammar() {
        assert!(Kind::Int.matches("-5") && Kind::Int.matches("0") && !Kind::Int.matches("-"));
        assert!(!Kind::Int.matches("+5") && !Kind::Int.matches("1e3") && !Kind::Int.matches(""));
        assert!(Kind::IntList.matches("1;-2;3") && !Kind::IntList.matches("1;;2"));
        assert!(Kind::Offset.matches("+05:30") && Kind::Offset.matches("-00:19:32"));
        assert!(!Kind::Offset.matches("+5:30") && !Kind::Offset.matches("=05:30"));
        for t in [
            "2024-03-10T12:00:00Z",
            "2024-03-10T12:00:00.123456789Z",
            "-0001-01-01T00:00:00+00:00",
            "+10000-01-01T00:00:00Z",
            "1900-01-01T00:00:00-00:19:32",
        ] {
            assert!(Kind::Iso.matches(t), "{t}");
        }
        for t in [
            "2024-03-10 12:00:00Z",
            "2024-03-10T12:00Z",
            "=2024-03-10T12:00:00Z",
        ] {
            assert!(!Kind::Iso.matches(t), "{t}");
        }
        assert!(Kind::Token.matches("unrecognisedCode:-7;lineage:1:session:2;true"));
        assert!(
            !Kind::Token.matches("-x") && !Kind::Token.matches("a b") && !Kind::Token.matches("@a")
        );
        assert!(!Kind::Content.matches("1"));
        for r in ["1.5", "-2", "1e100", "1e-7", "-0.0", "inf", "-inf", "NaN"] {
            assert!(Kind::Real.matches(r), "{r}");
        }
        assert!(
            !Kind::Real.matches("+1") && !Kind::Real.matches("1.") && !Kind::Real.matches("e5")
        );
    }

    #[test]
    fn controls_are_escaped_only_for_spreadsheets_and_the_reserved_character_always() {
        let cell = "a\u{1B}[31m\t\r\n\u{7F}\u{85}\u{0}\u{27E8}x\u{27E9}";
        let (s, n) = escape(cell, true);
        assert_eq!(
            s,
            "a\u{27E8}U+001B\u{27E9}[31m\t\r\n\u{27E8}U+007F\u{27E9}\u{27E8}U+0085\u{27E9}\
             \u{27E8}U+0000\u{27E9}\u{27E8}U+27E8\u{27E9}x\u{27E9}"
        );
        assert_eq!(
            n,
            Escaped {
                controls: 4,
                reserved: 1
            }
        );
        let (s, n) = escape(cell, false);
        assert_eq!(
            s,
            "a\u{1B}[31m\t\r\n\u{7F}\u{85}\u{0}\u{27E8}U+27E8\u{27E9}x\u{27E9}"
        );
        assert_eq!(
            n,
            Escaped {
                controls: 0,
                reserved: 1
            }
        );
    }

    #[test]
    fn every_table_lists_its_neutralised_columns_and_names_are_unique() {
        for t in TABLES {
            let names: BTreeSet<&str> = t.columns.iter().map(|(n, _)| *n).collect();
            assert_eq!(names.len(), t.columns.len(), "{:?}", t.stem);
            assert!(names.contains(NEUTRALISED), "{:?}", t.stem);
            for (n, _) in t.columns {
                assert!(n
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
            }
        }
        assert_eq!(TABLES.len(), 9);
    }

    #[test]
    fn offsets_keep_their_seconds() {
        assert_eq!(offset(0), "+00:00");
        assert_eq!(offset(19_800), "+05:30");
        assert_eq!(offset(-1_172), "-00:19:32");
    }

    #[test]
    fn the_skipped_classes_are_sorted_and_disjoint() {
        for t in [WHITE_SPACE, DEFAULT_IGNORABLE] {
            assert!(t.windows(2).all(|w| w[0].1 < w[1].0));
            assert!(t.iter().all(|(a, b)| a <= b));
        }
    }
}
