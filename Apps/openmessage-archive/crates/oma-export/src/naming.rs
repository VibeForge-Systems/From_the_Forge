//! Every name an export writes (M6 spec §6.2, §6.4, §8.1, §8.3, §8.5, I2).
//!
//! This is the only module that builds a path inside a bundle or beside it
//! (`tests/source_rules.rs` bans `.join(` elsewhere in the crate). Every
//! name is one of:
//!
//! - a fixed name of ours ([`README`], [`MANIFEST`], [`SUMS`], [`MARKER`]);
//! - an ordinal-built name from a closed grammar ([`BundlePath::format_file`]:
//!   `txt/c0001.txt`, `json/no-conversation.jsonl`, `csv/messages.csv`);
//! - an attachment name ([`attachment_name`]):
//!   `attachments/<shard>/<6-digit file ordinal>-<name>`, where `<name>` is
//!   the untrusted `transfer_name` without hidden characters, through
//!   `oma_fs::sanitize_filename_max` within the path's length budget, with
//!   `.blocked` appended unless the extension is on the passive allowlist
//!   and agrees with the sniffed kind (owner Q3b);
//! - a staging entry of ours beside the bundle ([`EntryName`]:
//!   `.oma-export-<32 hex>.partial`, `.partial-file` or `.probe`), whose
//!   name carries no content and matches the sweep's pattern;
//! - the name the export is published under ([`TargetName`]), which must be
//!   one `sanitize_filename` leaves unchanged and must not start with
//!   `.oma-export-`, so a sweep can never touch a real export.
//!
//! The ordinal prefix makes attachment names unique, including names that
//! differ only by case or by Unicode normalisation (which
//! `sanitize_filename` leaves to callers); staging still keeps a set of
//! lowercase names as a defensive check and creates every file with
//! `create_new`.
//!
//! `Debug` on these types never prints a name derived from the archive.

use std::path::{Path, PathBuf};

use oma_archive::SniffedKind;

use crate::error::{ExportError, LimitKind};
use crate::hidden;
use crate::ids::Ordinal;
use crate::plan::Format;

/// The bundle's README (written first of the provenance files).
pub const README: &str = "README.txt";
/// The bundle's manifest.
pub const MANIFEST: &str = "manifest.json";
/// The bundle's checksums (coreutils format), written last.
pub const SUMS: &str = "SHA256SUMS";
/// The marker inside a staging folder, held locked for the whole job and
/// removed only after the publish rename (D144).
pub const MARKER: &str = ".oma-export-partial";
/// The prefix of every staging entry. A name starting with it is never
/// accepted as an export's name.
pub const RESERVED_PREFIX: &str = ".oma-export-";
/// Appended to a copied attachment whose extension is not allowed (Q3b).
pub const BLOCKED: &str = ".blocked";
/// The NTFS stream a Mark of the Web is written to (Q3c). The one name
/// with a `:`, built only here.
pub const ZONE_IDENTIFIER: &str = ":Zone.Identifier";
/// The folder copied attachments go into.
pub const ATTACHMENTS_DIR: &str = "attachments";
/// The folder of a JSON bundle's copies of its schemas (spec §5.1).
pub const SCHEMA_DIR: &str = "schema";
/// The folder of an HTML bundle's larger previews, inside `html/`
/// (spec §5.5).
pub const PREVIEWS_DIR: &str = "previews";
/// The most bytes an attachment's `<name>` part may take (§6.2).
pub const MAX_ATTACHMENT_NAME_BYTES: usize = 120;
/// Below this budget an attachment's name is its ordinal only (§6.2).
pub const MIN_ATTACHMENT_NAME_BYTES: usize = 16;
/// The longest full path an export writes, in UTF-8 bytes: Windows'
/// classic `MAX_PATH` without its terminating NUL. UTF-8 bytes are never
/// fewer than UTF-16 units, so the bound holds on both systems, and an
/// export made on Linux can be copied to a Windows folder of the same depth.
pub const MAX_PATH_BYTES: usize = 259;
/// The highest ` (n)` suffix a bundle is published under.
pub const MAX_BUNDLE_SUFFIX: u32 = 99;

/// How many attachment files share one `<shard>` folder.
pub const FILES_PER_SHARD: u64 = 1_000;

/// 128 random bits naming one staging entry, an export or a destination.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntryId([u8; 16]);

impl EntryId {
    /// 128 bits from the operating system's generator.
    pub fn random() -> Result<EntryId, ExportError> {
        let mut b = [0u8; 16];
        getrandom::fill(&mut b).map_err(|_| ExportError::Write {
            kind: std::io::ErrorKind::Other,
        })?;
        Ok(EntryId(b))
    }

    /// A fixed id (tests and goldens).
    pub fn from_bytes(b: [u8; 16]) -> EntryId {
        EntryId(b)
    }

    /// 32 lowercase hex digits.
    pub fn hex(&self) -> String {
        hex(&self.0)
    }

    fn parse(s: &str) -> Option<EntryId> {
        if s.len() != 32 || !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return None;
        }
        let mut b = [0u8; 16];
        for (i, out) in b.iter_mut().enumerate() {
            *out = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
        }
        Some(EntryId(b))
    }
}

impl std::fmt::Debug for EntryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Random and content-free, but not useful in a log either.
        f.write_str("EntryId(..)")
    }
}

/// Lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        for nibble in [b >> 4, b & 0x0f] {
            s.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0'));
        }
    }
    s
}

/// The kinds of staging entry an export leaves beside its target while it
/// runs, and a crash may leave behind (§8.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryKind {
    /// `.oma-export-<hex>.partial`: a bundle's staging folder.
    PartialDir,
    /// `.oma-export-<hex>.partial-file`: a single file being written.
    PartialFile,
    /// `.oma-export-<hex>.probe`: the writability probe (§8.2).
    Probe,
}

impl EntryKind {
    fn suffix(self) -> &'static str {
        match self {
            EntryKind::PartialDir => ".partial",
            EntryKind::PartialFile => ".partial-file",
            EntryKind::Probe => ".probe",
        }
    }

    pub const ALL: [EntryKind; 3] = [
        EntryKind::PartialDir,
        EntryKind::PartialFile,
        EntryKind::Probe,
    ];
}

/// A staging entry's name: `.oma-export-<32 hex>.<kind>`. Content-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntryName {
    id: EntryId,
    kind: EntryKind,
}

impl EntryName {
    pub fn new(id: EntryId, kind: EntryKind) -> EntryName {
        EntryName { id, kind }
    }

    /// A fresh random name of `kind`.
    pub fn random(kind: EntryKind) -> Result<EntryName, ExportError> {
        Ok(EntryName::new(EntryId::random()?, kind))
    }

    pub fn kind(&self) -> EntryKind {
        self.kind
    }

    /// The name, exactly as on disk.
    pub fn to_name(&self) -> String {
        format!("{RESERVED_PREFIX}{}{}", self.id.hex(), self.kind.suffix())
    }

    /// The entry a directory entry's name stands for, if it matches the
    /// sweep's pattern exactly (lowercase hex, a known suffix).
    pub fn parse(name: &str) -> Option<EntryName> {
        let rest = name.strip_prefix(RESERVED_PREFIX)?;
        let (hex, suffix) = rest.split_at_checked(32)?;
        let kind = EntryKind::ALL.into_iter().find(|k| k.suffix() == suffix)?;
        Some(EntryName::new(EntryId::parse(hex)?, kind))
    }

    /// The longest staging name: what a bundle's paths are budgeted for.
    pub const MAX_LEN: usize = RESERVED_PREFIX.len() + 32 + ".partial-file".len();
}

/// The name an export is published under: a bundle folder (`Messages
/// export 2026-10-19 14-32`, then ` (2)` …) or a single file.
#[derive(Clone, PartialEq, Eq)]
pub struct TargetName(String);

impl std::fmt::Debug for TargetName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A name the user chose; never printed (I6).
        f.write_str("TargetName(..)")
    }
}

impl TargetName {
    /// Accept `name` as it is (`exportNameRefused` otherwise, §8.1): one
    /// `sanitize_filename` leaves unchanged, not starting with
    /// `.oma-export-`, and short enough for a ` (99)` suffix.
    pub fn new(name: &str) -> Result<TargetName, ExportError> {
        let refused = Err(ExportError::NameRefused);
        if oma_fs::sanitize_filename(name) != name
            || name.starts_with(RESERVED_PREFIX)
            || name.len() + " (99)".len() > oma_fs::MAX_FILENAME_BYTES
        {
            return refused;
        }
        Ok(TargetName(name.to_owned()))
    }

    /// A single file's name for `format` (§8.1, C1.1): the final extension
    /// must be exactly the format's own, compared ignoring ASCII case;
    /// otherwise `.<ext>` is appended (`notes.html` → `notes.html.txt`), so
    /// text content is never saved under an extension that opens as HTML,
    /// SVG, HTA or MHT. Then [`TargetName::new`]'s rules apply.
    pub fn for_single_file(name: &str, format: Format) -> Result<TargetName, ExportError> {
        let ext = format.extension();
        let has_own = name
            .rsplit_once('.')
            .is_some_and(|(stem, e)| !stem.is_empty() && e.eq_ignore_ascii_case(ext));
        if has_own {
            TargetName::new(name)
        } else {
            TargetName::new(&format!("{name}.{ext}"))
        }
    }

    /// The `n`-th candidate (from 1): the name itself, then `name (2)` to
    /// `name (99)`.
    pub fn candidate(&self, n: u32) -> TargetName {
        if n <= 1 {
            self.clone()
        } else {
            TargetName(format!("{} ({n})", self.0))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where a file's records come from: one conversation file, the "No
/// conversation" file, or a table of the whole export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileStem {
    /// `c0001`, by conversation ordinal (at least four digits).
    Conversation(Ordinal),
    /// `no-conversation`.
    NoConversation,
    /// A fixed table name.
    Table(TableStem),
}

/// The fixed table names of the JSON and CSV formats (§5.1, §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TableStem {
    Conversations,
    Handles,
    Messages,
    Segments,
    Attachments,
    AttachmentObservations,
    Reactions,
    EditVersions,
    ValueHistory,
}

impl TableStem {
    fn token(self) -> &'static str {
        match self {
            TableStem::Conversations => "conversations",
            TableStem::Handles => "handles",
            TableStem::Messages => "messages",
            TableStem::Segments => "segments",
            TableStem::Attachments => "attachments",
            TableStem::AttachmentObservations => "attachment_observations",
            TableStem::Reactions => "reactions",
            TableStem::EditVersions => "edit_versions",
            TableStem::ValueHistory => "value_history",
        }
    }
}

/// The schemas a JSON bundle carries a copy of (spec §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SchemaFile {
    /// `schema/export-v1.schema.json`, the open record schema.
    Export,
    /// `schema/manifest-v1.schema.json`.
    Manifest,
}

impl SchemaFile {
    fn file(self) -> &'static str {
        match self {
            SchemaFile::Export => "export-v1.schema.json",
            SchemaFile::Manifest => "manifest-v1.schema.json",
        }
    }
}

/// The extensions a format's files take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FileExt {
    Txt,
    Csv,
    Json,
    Jsonl,
    Html,
    Pdf,
    Xlsx,
}

impl FileExt {
    fn token(self) -> &'static str {
        match self {
            FileExt::Txt => "txt",
            FileExt::Csv => "csv",
            FileExt::Json => "json",
            FileExt::Jsonl => "jsonl",
            FileExt::Html => "html",
            FileExt::Pdf => "pdf",
            FileExt::Xlsx => "xlsx",
        }
    }
}

/// The file type of a decode-helper rendition (spec §6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ImageExt {
    Jpeg,
    Png,
}

impl ImageExt {
    pub fn token(self) -> &'static str {
        match self {
            ImageExt::Jpeg => "jpg",
            ImageExt::Png => "png",
        }
    }
}

/// A file's place inside a bundle, `/`-separated in the manifest and
/// `SHA256SUMS`. Built only by this module's constructors.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BundlePath {
    dirs: Vec<String>,
    file: String,
}

impl std::fmt::Debug for BundlePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // An attachment's name comes from the archive (I6).
        write!(f, "BundlePath({} folders, 1 file)", self.dirs.len())
    }
}

impl BundlePath {
    fn top(file: &str) -> BundlePath {
        BundlePath {
            dirs: Vec::new(),
            file: file.to_owned(),
        }
    }

    /// `README.txt`.
    pub fn readme() -> BundlePath {
        BundlePath::top(README)
    }

    /// `manifest.json`.
    pub fn manifest() -> BundlePath {
        BundlePath::top(MANIFEST)
    }

    /// `SHA256SUMS`.
    pub fn sums() -> BundlePath {
        BundlePath::top(SUMS)
    }

    /// `<format>/<stem>.<ext>`: `txt/c0001.txt`, `json/no-conversation.jsonl`,
    /// `csv/messages.csv`.
    pub fn format_file(format: Format, stem: FileStem, ext: FileExt) -> BundlePath {
        let stem = match stem {
            FileStem::Conversation(n) => format!("c{n:04}"),
            FileStem::NoConversation => "no-conversation".to_owned(),
            FileStem::Table(t) => t.token().to_owned(),
        };
        BundlePath {
            dirs: vec![format.token().to_owned()],
            file: format!("{stem}.{}", ext.token()),
        }
    }

    /// An HTML page of a conversation file (spec §5.5): `html/c0001.html`,
    /// then `html/c0001-2.html` … for the parts after a split
    /// (`no-conversation.html`, `no-conversation-2.html` …). `part` counts
    /// from 1.
    pub fn html_page(stem: FileStem, part: u32) -> BundlePath {
        let base = BundlePath::format_file(Format::Html, stem, FileExt::Html);
        if part <= 1 {
            return base;
        }
        let file = match base.file.strip_suffix(".html") {
            Some(s) => format!("{s}-{part}.html"),
            None => base.file.clone(),
        };
        BundlePath {
            dirs: base.dirs,
            file,
        }
    }

    /// `html/index.html`: the bundle's list of conversations.
    pub fn html_index() -> BundlePath {
        BundlePath {
            dirs: vec![Format::Html.token().to_owned()],
            file: "index.html".to_owned(),
        }
    }

    /// `html/previews/p<n>.<jpg|png>`: the larger preview of attachment
    /// record `n` (spec §6.5; bundles only).
    pub fn html_preview(n: Ordinal, ext: ImageExt) -> BundlePath {
        BundlePath {
            dirs: vec![Format::Html.token().to_owned(), PREVIEWS_DIR.to_owned()],
            file: format!("p{n}.{}", ext.token()),
        }
    }

    /// This path relative to the top-level folder `dir` (`html`),
    /// `/`-separated, for a relative link between files of that folder;
    /// `None` when the path is not below it.
    pub fn href_from(&self, dir: &str) -> Option<String> {
        let (first, rest) = self.dirs.split_first()?;
        if first != dir {
            return None;
        }
        let mut s = String::new();
        for d in rest {
            s.push_str(d);
            s.push('/');
        }
        s.push_str(&self.file);
        Some(s)
    }

    /// `schema/<file>`.
    pub fn schema(f: SchemaFile) -> BundlePath {
        BundlePath {
            dirs: vec![SCHEMA_DIR.to_owned()],
            file: f.file().to_owned(),
        }
    }

    /// `attachments/<shard>/<name>`.
    pub fn attachment(name: &AttachmentName) -> BundlePath {
        BundlePath {
            dirs: vec![ATTACHMENTS_DIR.to_owned(), name.shard.clone()],
            file: name.file.clone(),
        }
    }

    /// The folders, outermost first.
    pub fn dirs(&self) -> &[String] {
        &self.dirs
    }

    /// The file's own name.
    pub fn file(&self) -> &str {
        &self.file
    }

    /// The `/`-separated relative path (manifest, `SHA256SUMS`).
    pub fn to_slash(&self) -> String {
        let mut s = String::new();
        for d in &self.dirs {
            s.push_str(d);
            s.push('/');
        }
        s.push_str(&self.file);
        s
    }

    /// The path below `root` (the path-based staging used off Linux).
    pub fn under(&self, root: &Path) -> PathBuf {
        let mut p = root.to_path_buf();
        for d in &self.dirs {
            p.push(d);
        }
        p.push(&self.file);
        p
    }

    /// The folder `depth` levels down (1 = the first folder) below `root`.
    pub fn dir_under(&self, root: &Path, depth: usize) -> PathBuf {
        let mut p = root.to_path_buf();
        for d in self.dirs.iter().take(depth) {
            p.push(d);
        }
        p
    }

    /// The `/`-separated folder `depth` levels down, as a key.
    pub fn dir_key(&self, depth: usize) -> String {
        self.dirs
            .iter()
            .take(depth)
            .cloned()
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// `parent/<name>`: a staging entry, the published target or the marker
/// (the path-based staging used off Linux, and checks on every system).
pub fn child(parent: &Path, name: &str) -> PathBuf {
    parent.join(name)
}

/// `<file>:Zone.Identifier`: the NTFS stream a Mark of the Web goes into
/// (spec §6.4). The one path with a `:`, built only here.
pub fn zone_identifier(file: &Path) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(ZONE_IDENTIFIER);
    PathBuf::from(s)
}

// ---------------------------------------------------------------------------
// Attachment names (§6.2, §6.4)
// ---------------------------------------------------------------------------

/// The passive allowlist (owner Q3b; test-pinned): a copied file keeps its
/// extension only if it is listed here **for the kind its bytes were
/// sniffed as**. Everything else gets [`BLOCKED`] appended. No
/// completeness is claimed, which is why Mark of the Web follows (Q3c).
///
/// `aac` and `amr` are on the spec's audio row, but `sniff.rs` names no
/// such kind, so no file can be confirmed as either and both are blocked.
pub const PASSIVE: &[(&str, &[&str])] = &[
    ("jpeg", &["jpg", "jpeg"]),
    ("png", &["png"]),
    ("gif", &["gif"]),
    ("webp", &["webp"]),
    ("heif", &["heic", "heif"]),
    ("quickTime", &["mov", "mp4", "m4v"]),
    ("mp4", &["mov", "mp4", "m4v"]),
    ("m4v", &["mov", "mp4", "m4v"]),
    ("m4a", &["m4a"]),
    ("caf", &["caf"]),
    ("mp3", &["mp3"]),
    ("wav", &["wav"]),
    ("pdf", &["pdf"]),
];

/// The allowlist's token for a sniffed kind; `None` for kinds with no
/// allowed extension.
fn passive_token(kind: SniffedKind) -> Option<&'static str> {
    Some(match kind {
        SniffedKind::Jpeg => "jpeg",
        SniffedKind::Png => "png",
        SniffedKind::Gif => "gif",
        SniffedKind::Webp => "webp",
        SniffedKind::Heif => "heif",
        SniffedKind::QuickTime => "quickTime",
        SniffedKind::Mp4 => "mp4",
        SniffedKind::M4v => "m4v",
        SniffedKind::M4a => "m4a",
        SniffedKind::Caf => "caf",
        SniffedKind::Mp3 => "mp3",
        SniffedKind::Wav => "wav",
        SniffedKind::Pdf => "pdf",
        _ => return None,
    })
}

/// The extensions a file sniffed as `kind` may keep.
pub fn allowed_extensions(kind: Option<SniffedKind>) -> &'static [&'static str] {
    kind.and_then(passive_token)
        .and_then(|t| PASSIVE.iter().find(|(k, _)| *k == t))
        .map_or(&[], |(_, exts)| exts)
}

/// The extension a nameless attachment gets from its sniffed kind (never
/// from the declared type): the allowlist's first, or a descriptive one
/// that the allowlist then blocks.
fn sniffed_extension(kind: Option<SniffedKind>) -> Option<&'static str> {
    if let Some(e) = allowed_extensions(kind).first() {
        return Some(e);
    }
    Some(match kind? {
        SniffedKind::Bmp => "bmp",
        SniffedKind::Ico => "ico",
        SniffedKind::Webm => "webm",
        SniffedKind::Aiff => "aiff",
        SniffedKind::Ogg => "ogg",
        SniffedKind::Xml => "xml",
        SniffedKind::Html => "html",
        SniffedKind::Vcard => "vcf",
        SniffedKind::Text => "txt",
        _ => return None,
    })
}

/// The extension of a sanitized name, lowercased (`None` without one).
fn extension_of(name: &str) -> Option<String> {
    name.rsplit_once('.')
        .filter(|(stem, e)| !stem.is_empty() && !e.is_empty())
        .map(|(_, e)| e.to_ascii_lowercase())
}

/// What the length budget leaves for attachment names below `parent`
/// (§6.2 step 4): the canonical parent, the bundle name with a possible
/// ` (99)` suffix or the longer staging name, `attachments/<shard>/`, the
/// six-digit prefix, `.blocked` and the Mark of the Web stream, all within
/// [`MAX_PATH_BYTES`], and never more than [`MAX_ATTACHMENT_NAME_BYTES`].
/// Both the staging path and the published path fit.
pub fn attachment_name_budget(parent: &Path, bundle: &TargetName) -> usize {
    let top = (bundle.as_str().len() + " (99)".len()).max(EntryName::MAX_LEN);
    let fixed = parent.as_os_str().as_encoded_bytes().len()
        + 1
        + top
        + 1
        + ATTACHMENTS_DIR.len()
        + 1
        + "000/".len()
        + "000000-".len()
        + BLOCKED.len()
        + ZONE_IDENTIFIER.len();
    MAX_PATH_BYTES
        .saturating_sub(fixed)
        .min(MAX_ATTACHMENT_NAME_BYTES)
}

/// An attachment's name inside `attachments/<shard>/`.
#[derive(Clone, PartialEq, Eq)]
pub struct AttachmentName {
    shard: String,
    file: String,
    /// The name is the ordinal only: the budget was below
    /// [`MIN_ATTACHMENT_NAME_BYTES`] (`namesShortened`).
    pub shortened: bool,
    /// `.blocked` was appended (`blocked`).
    pub blocked: bool,
}

impl std::fmt::Debug for AttachmentName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttachmentName")
            .field("shortened", &self.shortened)
            .field("blocked", &self.blocked)
            .finish_non_exhaustive()
    }
}

impl AttachmentName {
    /// The file's name (content: never log it).
    pub fn file(&self) -> &str {
        &self.file
    }
}

/// Build the name of the attachment file with ordinal `file` (from 1) from
/// its `transfer_name` (`None` when missing or unreadable), its sniffed
/// kind and `budget` (from [`attachment_name_budget`]) (§6.2):
///
/// 1. a missing or empty name becomes `attachment.<ext>`, the extension
///    from the **sniffed** kind;
/// 2. hidden characters are removed (§4.7; `sanitize_filename` removes
///    only bidi controls);
/// 3. `sanitize_filename_max` within the budget (less any prefix digits
///    beyond six); below [`MIN_ATTACHMENT_NAME_BYTES`] the name is the
///    ordinal only (`000045.heic`);
/// 4. `.blocked` is appended unless the extension is allowed for the
///    sniffed kind, or `keep_extensions` (the confirmed opt-out) is on.
pub fn attachment_name(
    file: Ordinal,
    transfer_name: Option<&str>,
    sniffed: Option<SniffedKind>,
    budget: usize,
    keep_extensions: bool,
) -> AttachmentName {
    let prefix = format!("{file:06}");
    let budget = budget.saturating_sub(prefix.len().saturating_sub(6));
    let nameless = || match sniffed_extension(sniffed) {
        Some(e) => format!("attachment.{e}"),
        None => "attachment".to_owned(),
    };
    let visible = match transfer_name {
        Some(n) if !n.is_empty() => hidden::without_hidden(n),
        _ => nameless(),
    };
    let visible = if visible.trim().is_empty() {
        nameless()
    } else {
        visible
    };
    let (mut name, shortened) = if budget < MIN_ATTACHMENT_NAME_BYTES {
        let ext = sniffed_extension(sniffed)
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        (format!("{prefix}{ext}"), true)
    } else {
        let n = oma_fs::sanitize_filename_max(&visible, budget);
        (format!("{prefix}-{n}"), false)
    };
    let allowed =
        extension_of(&name).is_some_and(|e| allowed_extensions(sniffed).iter().any(|a| *a == e));
    let blocked = !allowed && !keep_extensions;
    if blocked {
        name.push_str(BLOCKED);
    }
    AttachmentName {
        shard: format!("{:03}", file / FILES_PER_SHARD),
        file: name,
        shortened,
        blocked,
    }
}

/// The defensive collision key of a bundle path: its `/` form in ASCII
/// lowercase (names are already unique by their ordinal prefixes).
pub(crate) fn collision_key(p: &BundlePath) -> String {
    p.to_slash().to_ascii_lowercase()
}

/// A staging-internal invariant: a path whose collision key was already
/// used.
pub(crate) fn collision() -> ExportError {
    ExportError::Limit(LimitKind::NameCollision)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> EntryId {
        EntryId::from_bytes([n; 16])
    }

    #[test]
    fn html_pages_parts_previews_and_links() {
        let c = FileStem::Conversation(1);
        assert_eq!(BundlePath::html_page(c, 1).to_slash(), "html/c0001.html");
        assert_eq!(BundlePath::html_page(c, 2).to_slash(), "html/c0001-2.html");
        assert_eq!(
            BundlePath::html_page(FileStem::NoConversation, 3).to_slash(),
            "html/no-conversation-3.html"
        );
        assert_eq!(BundlePath::html_index().to_slash(), "html/index.html");
        let p = BundlePath::html_preview(45, ImageExt::Jpeg);
        assert_eq!(p.to_slash(), "html/previews/p45.jpg");
        assert_eq!(p.href_from("html").as_deref(), Some("previews/p45.jpg"));
        assert_eq!(
            BundlePath::html_page(c, 2).href_from("html").as_deref(),
            Some("c0001-2.html")
        );
        assert_eq!(BundlePath::readme().href_from("html"), None);
        assert_eq!(
            BundlePath::schema(SchemaFile::Export).href_from("html"),
            None
        );
    }

    #[test]
    fn entry_names_round_trip_and_nothing_else_parses() {
        for kind in EntryKind::ALL {
            let e = EntryName::new(id(0xab), kind);
            let s = e.to_name();
            assert!(s.starts_with(".oma-export-abab"));
            assert!(s.len() <= EntryName::MAX_LEN);
            assert_eq!(EntryName::parse(&s), Some(e));
        }
        for bad in [
            ".oma-export-partial",
            ".oma-export-ABABABABABABABABABABABABABABABAB.partial",
            ".oma-export-abababababababababababababababab.partial.x",
            ".oma-export-abababababababababababababababa.partial",
            ".oma-export-abababababababababababababababab",
            "oma-export-abababababababababababababababab.probe",
            ".oma-export-abababababababababababababababab.PROBE",
            "",
        ] {
            assert_eq!(EntryName::parse(bad), None, "{bad}");
        }
        assert_ne!(
            EntryId::random().expect("random"),
            EntryId::random().expect("random")
        );
    }

    #[test]
    fn target_names_are_refused_not_changed() {
        assert_eq!(
            TargetName::new("Messages export 2026-10-19 14-32")
                .expect("ok")
                .candidate(2)
                .as_str(),
            "Messages export 2026-10-19 14-32 (2)"
        );
        for bad in [
            "a/b",
            "con",
            "trailing.",
            "",
            ".oma-export-x",
            ".oma-export-partial",
            "x\u{202E}y",
        ] {
            assert!(
                matches!(TargetName::new(bad), Err(ExportError::NameRefused)),
                "{bad:?}"
            );
        }
        assert!(TargetName::new(&"x".repeat(250)).is_ok());
        assert!(TargetName::new(&"x".repeat(251)).is_err());
        assert_eq!(
            format!("{:?}", TargetName::new("secret").expect("ok")),
            "TargetName(..)"
        );
    }

    #[test]
    fn single_files_always_end_in_their_format_extension() {
        let risky = ["html", "htm", "xhtml", "svg", "hta", "mht", "mhtml", "xml"];
        for format in [Format::Txt, Format::Json, Format::Csv] {
            for ext in risky {
                let n = TargetName::for_single_file(&format!("notes.{ext}"), format).expect("ok");
                assert_eq!(n.as_str(), format!("notes.{ext}.{}", format.extension()));
            }
            let own = format!("Notes.{}", format.extension().to_uppercase());
            assert_eq!(
                TargetName::for_single_file(&own, format)
                    .expect("ok")
                    .as_str(),
                own
            );
        }
        assert_eq!(
            TargetName::for_single_file("x.svg", Format::Json)
                .expect("ok")
                .as_str(),
            "x.svg.json"
        );
        // A bare extension is not a name with that extension.
        assert_eq!(
            TargetName::for_single_file(".txt", Format::Txt)
                .expect("ok")
                .as_str(),
            ".txt.txt"
        );
        assert!(TargetName::for_single_file("a:b", Format::Txt).is_err());
    }

    #[test]
    fn format_files_follow_the_closed_grammar() {
        let p = BundlePath::format_file(Format::Txt, FileStem::Conversation(1), FileExt::Txt);
        assert_eq!(p.to_slash(), "txt/c0001.txt");
        let p =
            BundlePath::format_file(Format::Json, FileStem::Conversation(12_345), FileExt::Jsonl);
        assert_eq!(p.to_slash(), "json/c12345.jsonl");
        let p = BundlePath::format_file(Format::Json, FileStem::NoConversation, FileExt::Jsonl);
        assert_eq!(p.to_slash(), "json/no-conversation.jsonl");
        let p = BundlePath::format_file(
            Format::Csv,
            FileStem::Table(TableStem::AttachmentObservations),
            FileExt::Csv,
        );
        assert_eq!(p.to_slash(), "csv/attachment_observations.csv");
        assert_eq!(BundlePath::readme().to_slash(), "README.txt");
        assert_eq!(BundlePath::sums().to_slash(), "SHA256SUMS");
        assert_eq!(format!("{p:?}"), "BundlePath(1 folders, 1 file)");
        let root = Path::new("root");
        assert_eq!(
            p.under(root),
            root.join("csv").join("attachment_observations.csv")
        );
    }

    #[test]
    fn the_mark_of_the_web_stream_is_built_here_only() {
        let f = Path::new("x").join("000001-a.jpg");
        let z = zone_identifier(&f);
        assert_eq!(
            z.as_os_str().to_string_lossy(),
            format!("{}:Zone.Identifier", f.display())
        );
        assert_eq!(
            crate::stage::MARK_OF_THE_WEB,
            b"[ZoneTransfer]\r\nZoneId=3\r\n"
        );
    }

    #[test]
    fn the_allowlist_is_pinned() {
        let pinned: Vec<String> = PASSIVE
            .iter()
            .map(|(k, e)| format!("{k}:{}", e.join(",")))
            .collect();
        assert_eq!(
            pinned.join(" "),
            "jpeg:jpg,jpeg png:png gif:gif webp:webp heif:heic,heif \
             quickTime:mov,mp4,m4v mp4:mov,mp4,m4v m4v:mov,mp4,m4v m4a:m4a \
             caf:caf mp3:mp3 wav:wav pdf:pdf"
        );
        // Every sniffed kind with a token has a row, and only those.
        let with_rows = SniffedKind::ALL
            .iter()
            .filter(|k| !allowed_extensions(Some(**k)).is_empty())
            .count();
        assert_eq!(with_rows, PASSIVE.len());
        assert!(allowed_extensions(None).is_empty());
    }

    fn name(t: Option<&str>, k: Option<SniffedKind>) -> AttachmentName {
        attachment_name(45, t, k, MAX_ATTACHMENT_NAME_BYTES, false)
    }

    #[test]
    fn attachment_names_keep_allowed_extensions_and_block_the_rest() {
        let n = name(Some("IMG_0001.JPG"), Some(SniffedKind::Jpeg));
        assert_eq!((n.file(), n.blocked), ("000045-IMG_0001.JPG", false));
        assert_eq!(
            BundlePath::attachment(&n).to_slash(),
            "attachments/000/000045-IMG_0001.JPG"
        );
        // Declared and sniffed disagree.
        let n = name(Some("photo.jpg"), Some(SniffedKind::Png));
        assert_eq!(n.file(), "000045-photo.jpg.blocked");
        // Unknown kinds, risky and missing extensions.
        for (t, k) in [
            ("run.exe", Some(SniffedKind::Unknown)),
            ("page.html", Some(SniffedKind::Html)),
            ("script.py", Some(SniffedKind::Text)),
            ("x.appref-ms", None),
            ("voice.amr", None),
            ("noext", Some(SniffedKind::Jpeg)),
        ] {
            let n = name(Some(t), k);
            assert!(n.blocked && n.file().ends_with(".blocked"), "{t}");
        }
        // The bidi spoof is visible, and blocked.
        let n = name(Some("\u{202E}gpj.exe"), Some(SniffedKind::Jpeg));
        assert_eq!(n.file(), "000045-gpj.exe.blocked");
        // The opt-out keeps every extension.
        let n = attachment_name(45, Some("run.exe"), None, 120, true);
        assert_eq!((n.file(), n.blocked), ("000045-run.exe", false));
        // A nameless attachment takes the sniffed kind's extension.
        assert_eq!(
            name(None, Some(SniffedKind::Heif)).file(),
            "000045-attachment.heic"
        );
        assert_eq!(
            name(Some(""), Some(SniffedKind::M4a)).file(),
            "000045-attachment.m4a"
        );
        assert_eq!(
            name(Some("\u{200B}\u{2060}"), Some(SniffedKind::Png)).file(),
            "000045-attachment.png"
        );
        assert_eq!(
            name(None, Some(SniffedKind::Html)).file(),
            "000045-attachment.html.blocked"
        );
        assert_eq!(name(None, None).file(), "000045-attachment.blocked");
    }

    #[test]
    fn attachment_names_are_sanitized_unique_and_sharded() {
        // Hidden characters go, Windows rules apply, case and NFC/NFD
        // collisions differ by their ordinal prefix.
        let n = name(Some("a\u{200B}b\u{2060}.png"), Some(SniffedKind::Png));
        assert_eq!(n.file(), "000045-ab.png");
        assert_eq!(
            name(Some("CON.png"), Some(SniffedKind::Png)).file(),
            "000045-_CON.png"
        );
        assert_eq!(
            name(Some("trailing.png. . "), Some(SniffedKind::Png)).file(),
            "000045-trailing.png"
        );
        let a = attachment_name(1, Some("Photo.png"), Some(SniffedKind::Png), 120, false);
        let b = attachment_name(2, Some("photo.PNG"), Some(SniffedKind::Png), 120, false);
        let c = attachment_name(
            3,
            Some("Cafe\u{301}.png"),
            Some(SniffedKind::Png),
            120,
            false,
        );
        let d = attachment_name(4, Some("Caf\u{e9}.png"), Some(SniffedKind::Png), 120, false);
        let keys: std::collections::BTreeSet<String> = [a, b, c, d]
            .iter()
            .map(|n| collision_key(&BundlePath::attachment(n)))
            .collect();
        assert_eq!(keys.len(), 4);
        // Over 255 bytes: cut within the budget, extension kept.
        let long = format!("{}.jpeg", "é".repeat(200));
        let n = name(Some(&long), Some(SniffedKind::Jpeg));
        assert!(
            n.file().len() <= 7 + MAX_ATTACHMENT_NAME_BYTES,
            "{}",
            n.file().len()
        );
        assert!(n.file().ends_with(".jpeg") && !n.blocked);
        // Shards of 1,000; prefixes grow past six digits within the budget.
        let n = attachment_name(999, Some("a.png"), Some(SniffedKind::Png), 120, false);
        assert_eq!(BundlePath::attachment(&n).dirs()[1], "000");
        let n = attachment_name(1_000, Some("a.png"), Some(SniffedKind::Png), 120, false);
        assert_eq!(BundlePath::attachment(&n).dirs()[1], "001");
        let n = attachment_name(1_234_567, Some(&long), Some(SniffedKind::Jpeg), 120, false);
        assert_eq!(BundlePath::attachment(&n).dirs()[1], "1234");
        assert!(n.file().starts_with("1234567-"));
        assert!(n.file().len() <= 7 + MAX_ATTACHMENT_NAME_BYTES);
        assert_eq!(
            format!("{n:?}"),
            "AttachmentName { shortened: false, blocked: false, .. }"
        );
    }

    #[test]
    fn a_deep_parent_shortens_names_to_the_ordinal_and_every_path_fits() {
        let bundle = TargetName::new("Messages export 2026-10-19 14-32").expect("ok");
        let shallow = Path::new("/home/u/Documents");
        assert_eq!(
            attachment_name_budget(shallow, &bundle),
            MAX_ATTACHMENT_NAME_BYTES
        );
        let long = format!("{}.jpeg", "é".repeat(200));
        for depth in [10usize, 60, 100, 130, 150, 200, 300] {
            let parent = PathBuf::from(format!("/{}", "d".repeat(depth)));
            let budget = attachment_name_budget(&parent, &bundle);
            let n = attachment_name(999_999, Some(&long), Some(SniffedKind::Jpeg), budget, false);
            assert_eq!(n.shortened, budget < MIN_ATTACHMENT_NAME_BYTES, "{depth}");
            if n.shortened {
                assert_eq!(n.file(), "999999.jpg");
            }
            let rel = BundlePath::attachment(&n).to_slash();
            // The published path and the staging path, with `.blocked`
            // and the Mark of the Web stream reserved.
            for top in [
                bundle.candidate(99).as_str().to_owned(),
                EntryName::new(id(1), EntryKind::PartialFile).to_name(),
            ] {
                let full = format!("{}/{top}/{rel}", parent.display());
                if !n.shortened {
                    assert!(
                        full.len() + BLOCKED.len() + ZONE_IDENTIFIER.len() <= MAX_PATH_BYTES,
                        "{depth}: {}",
                        full.len()
                    );
                }
            }
        }
    }
}
