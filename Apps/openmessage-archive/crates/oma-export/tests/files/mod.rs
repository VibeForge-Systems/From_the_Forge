//! Helpers for the staging, manifest and crash tests: a canonical
//! destination in a temporary folder, a minimal writer that drives the
//! fan-out, a manifest body, and a tree listing.
#![allow(dead_code, clippy::disallowed_methods)]

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use oma_export::manifest::{DerivationFacts, ManifestBody, ManifestCounts, ManifestInput};
use oma_export::map::ExportZone;
use oma_export::naming::{BundlePath, FileExt, FileStem};
use oma_export::plan::{ExportOptions, ExportPlan, Format, ScopePlan};
use oma_export::record::ItemRecord;
use oma_export::stage::{StagedFile, Staging};
use oma_export::writer::{
    AboutDraft, ConversationFile, ConversationTotals, ExportContext, ExportWriter, WriterReport,
};
use oma_export::{EntryId, ExportError};

/// A temporary folder and its canonical form (the destination parent).
pub struct Dest {
    pub tmp: tempfile::TempDir,
    pub parent: PathBuf,
}

/// A fresh destination `<tmp>/dest`, so tests can check that nothing is
/// written beside it.
pub fn dest() -> Dest {
    let tmp = tempfile::tempdir().expect("tempdir");
    let parent = tmp.path().join("dest");
    std::fs::create_dir(&parent).expect("mkdir");
    let parent = dunce::canonicalize(&parent).expect("canonical");
    Dest { tmp, parent }
}

/// Every path below `root`, relative and `/`-separated, sorted; folders
/// end in `/`. Links are listed, never followed.
pub fn tree(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel: Vec<String> = p
                .strip_prefix(root)
                .expect("below root")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            let rel = rel.join("/");
            let meta = std::fs::symlink_metadata(&p).expect("meta");
            if meta.is_dir() {
                out.push(format!("{rel}/"));
                walk(root, &p, out);
            } else {
                out.push(rel);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// A one-conversation TXT plan (single file) or a bundle (TXT + CSV
/// formats would need their writers; a TXT plan over the whole archive is
/// a bundle).
pub fn plan(single: bool) -> ExportPlan {
    let scope = if single {
        ScopePlan::Conversation {
            chat: oma_archive::ChatId::new(1),
        }
    } else {
        ScopePlan::Archive
    };
    let mut o = ExportOptions::defaults(&scope, &[Format::Txt]);
    o.attachments = oma_export::plan::AttachmentMode::None;
    ExportPlan::new(scope, vec![Format::Txt], o).expect("plan")
}

/// A writer for tests: one file per conversation file, one line per item
/// (`<ordinal>` or `<ordinal> folded`), and a line per `end_conversation`
/// with the totals it was given.
pub struct LinesWriter {
    pub format: Format,
    current: Option<StagedFile>,
    report: WriterReport,
    /// Report one item more than it was given (a lying writer).
    pub lie: bool,
    /// Leave the last file open at `finish`.
    pub leak: bool,
}

impl LinesWriter {
    pub fn new(format: Format) -> LinesWriter {
        LinesWriter {
            format,
            current: None,
            report: WriterReport::new(format),
            lie: false,
            leak: false,
        }
    }
}

fn io(e: std::io::Error) -> ExportError {
    ExportError::io(&e)
}

impl ExportWriter for LinesWriter {
    fn format(&self) -> Format {
        self.format
    }

    fn begin(&mut self, _ctx: &ExportContext<'_>, _out: &mut Staging) -> Result<(), ExportError> {
        Ok(())
    }

    fn begin_conversation(
        &mut self,
        c: ConversationFile<'_>,
        out: &mut Staging,
    ) -> Result<(), ExportError> {
        let stem = match c {
            ConversationFile::Conversation(r) => FileStem::Conversation(r.n),
            ConversationFile::NoConversation => FileStem::NoConversation,
        };
        let ext = match self.format {
            Format::Csv => FileExt::Csv,
            _ => FileExt::Txt,
        };
        let mut f = out.create(&BundlePath::format_file(self.format, stem, ext))?;
        f.write_all(b"conversation\n").map_err(io)?;
        self.current = Some(f);
        self.report.conversation_files += 1;
        Ok(())
    }

    fn item(&mut self, it: &ItemRecord, _out: &mut Staging) -> Result<(), ExportError> {
        let f = self
            .current
            .as_mut()
            .ok_or(ExportError::Limit(oma_export::LimitKind::RowOrder))?;
        if it.context.folded_into.is_some() {
            writeln!(f, "{} folded", it.n).map_err(io)?;
            self.report.folded_rows += 1;
        } else {
            writeln!(f, "{}", it.n).map_err(io)?;
            self.report.items += 1;
        }
        Ok(())
    }

    fn end_conversation(
        &mut self,
        totals: &ConversationTotals,
        out: &mut Staging,
    ) -> Result<(), ExportError> {
        let mut f = self
            .current
            .take()
            .ok_or(ExportError::Limit(oma_export::LimitKind::RowOrder))?;
        writeln!(f, "end {} {}", totals.items, totals.folded_rows).map_err(io)?;
        if self.leak {
            self.current = Some(f);
            return Ok(());
        }
        out.close(f)?;
        Ok(())
    }

    fn finish(
        mut self: Box<Self>,
        _about: &AboutDraft<'_>,
        _out: &mut Staging,
    ) -> Result<WriterReport, ExportError> {
        if self.lie {
            self.report.items += 1;
        }
        if self.leak {
            // Keep the file open past finish.
            std::mem::forget(self.current.take());
        }
        Ok(self.report.clone())
    }
}

/// A manifest body for `plan` with consistent zero counts.
pub fn body(plan: &ExportPlan, conversations: u64) -> ManifestBody {
    ManifestBody::new(ManifestInput {
        plan,
        zone: &ExportZone::Utc,
        export_id: EntryId::from_bytes([7; 16]),
        created_utc_ns: 1_798_761_600 * 1_000_000_000,
        app_version: "0.1.0",
        conversations,
        counts: ManifestCounts::default(),
        derivation: DerivationFacts::default(),
        search: None,
        import_sessions: Vec::new(),
    })
    .expect("body")
}

/// The export context the tests use.
pub fn ctx<'a>(plan: &'a ExportPlan, zone: &'a ExportZone) -> ExportContext<'a> {
    ExportContext {
        plan,
        zone,
        export_id: EntryId::from_bytes([7; 16]),
        created_utc_ns: 1_798_761_600 * 1_000_000_000,
    }
}

/// SHA-256 of `bytes`, hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    oma_export::naming::hex(&sha2::Sha256::digest(bytes))
}

/// `SHA256SUMS` of a published bundle, checked with Rust: every line's
/// file hashes to its value. Returns the listed paths.
pub fn verify_sums(bundle: &Path) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(bundle.join("SHA256SUMS")).expect("sums");
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let (hex, path) = line.split_once("  ").expect("two spaces");
        let full = path.split('/').fold(bundle.to_path_buf(), |p, c| p.join(c));
        let bytes = std::fs::read(&full).expect("listed file");
        assert_eq!(sha256_hex(&bytes), hex, "{path}");
        out.insert(path.to_owned(), hex.to_owned());
    }
    out
}
