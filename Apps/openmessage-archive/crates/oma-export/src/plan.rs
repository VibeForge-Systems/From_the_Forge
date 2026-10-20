//! The export plan (M6 spec §9.2, §0 rule 1, D126): what one export
//! writes, validated before anything runs.
//!
//! The shell builds an [`ExportPlan`] from its parsed DTOs (slice 6h); the
//! webview never supplies a path or a message id. [`ExportPlan::new`]
//! rejects every combination that does not apply (spec §9.2:
//! "rejected, not ignored") and decides the packaging (D126).
//!
//! Defaults that are owner decisions live here too, so the dialog and the
//! job cannot disagree:
//!
//! - **Q3a** ([`default_attachments`]): bundle exports with HTML or JSON,
//!   and whole-archive exports, copy original files by default; a single
//!   file carries names only; CSV, XLSX and TXT bundles copy files only
//!   when the box is ticked.
//! - **Q4**: both reveal opt-ins start off and are never remembered
//!   ([`RevealPlan::default`]); each needs the count it was confirmed
//!   against.
//! - **Q5 (a lean awaiting the owner)**: identifiers are export-local
//!   ordinals unless the plan opts in ([`IdentifierMode`]).
//!
//! "Save a copy" of one attachment (Flow D) is not an `ExportPlan`: it has
//! no format and no records, and arrives with the attachment copy in
//! slice 6e.

use oma_archive::{ChatId, ExportDepth, Reveal};

use crate::error::{ExportError, PlanRefusal};
use crate::ids::IdentifierMode;
use crate::time::CivilDate;

/// An output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Format {
    Txt,
    Csv,
    Json,
    Html,
    Pdf,
    Xlsx,
}

impl Format {
    pub const ALL: [Format; 6] = [
        Format::Txt,
        Format::Csv,
        Format::Json,
        Format::Html,
        Format::Pdf,
        Format::Xlsx,
    ];

    /// The format's token (the manifest's `formats`).
    pub fn token(self) -> &'static str {
        match self {
            Format::Txt => "txt",
            Format::Csv => "csv",
            Format::Json => "json",
            Format::Html => "html",
            Format::Pdf => "pdf",
            Format::Xlsx => "xlsx",
        }
    }

    /// The extension a single file of this format always ends in.
    pub fn extension(self) -> &'static str {
        self.token()
    }

    /// Whether the D141 identifier opt-in can apply to this format.
    pub fn carries_identifiers(self) -> bool {
        matches!(self, Format::Json | Format::Csv | Format::Xlsx)
    }
}

/// What an export covers.
#[derive(Clone, PartialEq, Eq)]
pub enum ScopePlan {
    /// One conversation's whole timeline.
    Conversation { chat: ChatId },
    /// Local dates `[from, until)` in the export's zone, in one
    /// conversation or across the archive. Rust turns them into instants
    /// with the validated zone table (`ZoneTable::day_range`).
    DateRange {
        from: CivilDate,
        until: CivilDate,
        chat: Option<ChatId>,
    },
    /// A search result set the shell holds (never the query); with
    /// `all_matches`, the job re-runs it with the export's cap.
    SearchResults { all_matches: bool },
    /// Every conversation, then the messages in no conversation.
    Archive,
}

impl std::fmt::Debug for ScopePlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The kind only: never an id or a date (I6).
        f.write_str(self.kind())
    }
}

impl ScopePlan {
    /// The scope's kind token (logs and the manifest's `scope.kind`).
    pub fn kind(&self) -> &'static str {
        match self {
            ScopePlan::Conversation { .. } => "conversation",
            ScopePlan::DateRange { .. } => "dateRange",
            ScopePlan::SearchResults { .. } => "searchResults",
            ScopePlan::Archive => "archive",
        }
    }

    /// At most one conversation by construction.
    fn single_conversation(&self) -> bool {
        matches!(
            self,
            ScopePlan::Conversation { .. } | ScopePlan::DateRange { chat: Some(_), .. }
        )
    }
}

/// Whether original files are copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttachmentMode {
    /// Names only (`notRequested` outcomes).
    None,
    CopyOriginals,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Clock {
    H23,
    H12,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineEndings {
    Lf,
    Crlf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CsvProfile {
    /// "For spreadsheets": default-deny formula neutralisation, controls
    /// escaped.
    Spreadsheet,
    /// "For data tools": raw except the reserved character.
    DataTools,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PageSize {
    A4,
    Letter,
}

/// Which zone the times are written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ZoneChoice {
    /// This computer's zone (the default).
    System,
    Utc,
    /// Another IANA zone the webview supports.
    Named,
}

/// The two per-export reveal opt-ins (owner Q4, D125): off by default,
/// reset each time the dialog opens, never remembered, each confirmed
/// against its own exact count. The viewer's `revealEarlierText` never
/// sets them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RevealPlan {
    pub one_time_codes: bool,
    pub earlier_content: bool,
    /// The N the user confirmed for one-time codes ("N messages"),
    /// exactly when that reveal is on.
    pub codes_confirmed_up_to: Option<u64>,
    /// The N the user confirmed for earlier content, exactly when that
    /// reveal is on.
    pub earlier_confirmed_up_to: Option<u64>,
}

impl RevealPlan {
    /// The reveals whose counts are given, each confirmed against its own
    /// N (`None`: that reveal off).
    pub fn confirmed(codes: Option<u64>, earlier: Option<u64>) -> RevealPlan {
        RevealPlan {
            one_time_codes: codes.is_some(),
            earlier_content: earlier.is_some(),
            codes_confirmed_up_to: codes,
            earlier_confirmed_up_to: earlier,
        }
    }

    /// The reveal the archive reads apply.
    pub fn reveal(self) -> Reveal {
        Reveal {
            earlier_text: self.earlier_content,
            one_time_code: self.one_time_codes,
        }
    }

    pub fn any(self) -> bool {
        self.one_time_codes || self.earlier_content
    }
}

/// The options of an export (spec §9.2 `ExportOptionsInput`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExportOptions {
    pub attachments: AttachmentMode,
    /// "Keep original file extensions" (Q3b; off, confirmed in Review).
    pub keep_extensions: bool,
    pub previews: bool,
    pub larger_previews: bool,
    pub history: bool,
    pub reactions: bool,
    pub events: bool,
    pub reveal: RevealPlan,
    pub hidden_character_markers: bool,
    pub seconds: bool,
    pub identifiers: IdentifierMode,
    pub clock: Clock,
    pub line_endings: LineEndings,
    pub csv_profile: Option<CsvProfile>,
    pub page_size: Option<PageSize>,
    pub zone: ZoneChoice,
}

impl ExportOptions {
    /// The defaults for `formats` over `scope` (the dialog's starting
    /// state): every opt-in off, Q3a's attachment default, markers on.
    pub fn defaults(scope: &ScopePlan, formats: &[Format]) -> ExportOptions {
        ExportOptions {
            attachments: default_attachments(scope, formats),
            keep_extensions: false,
            previews: false,
            larger_previews: false,
            history: false,
            reactions: true,
            events: true,
            reveal: RevealPlan::default(),
            hidden_character_markers: true,
            seconds: true,
            identifiers: IdentifierMode::Ordinals,
            clock: Clock::H23,
            line_endings: LineEndings::Lf,
            csv_profile: formats
                .contains(&Format::Csv)
                .then_some(CsvProfile::Spreadsheet),
            page_size: formats.contains(&Format::Pdf).then_some(PageSize::A4),
            zone: ZoneChoice::System,
        }
    }
}

/// How the output is packaged (D126).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Packaging {
    /// One file in the format's extension, with an "About this export"
    /// section.
    SingleFile,
    /// A folder with `README.txt`, `manifest.json` and `SHA256SUMS`.
    Bundle,
}

/// Whether `scope` and `formats` need a bundle before attachments are
/// considered: more than one format, more than one conversation, or CSV
/// (always a bundle, D130).
fn bundle_without_files(scope: &ScopePlan, formats: &[Format]) -> bool {
    formats.len() != 1 || !scope.single_conversation() || formats.contains(&Format::Csv)
}

/// Owner decision Q3a: original files are copied by default for bundle
/// exports with HTML or JSON and for the whole archive; a single file
/// carries names only; CSV, XLSX and TXT bundles copy files only when the
/// box is ticked.
pub fn default_attachments(scope: &ScopePlan, formats: &[Format]) -> AttachmentMode {
    let rich = formats
        .iter()
        .any(|f| matches!(f, Format::Html | Format::Json));
    let archive = matches!(scope, ScopePlan::Archive);
    if archive || (rich && bundle_without_files(scope, formats)) {
        AttachmentMode::CopyOriginals
    } else {
        AttachmentMode::None
    }
}

/// A validated plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportPlan {
    scope: ScopePlan,
    formats: Vec<Format>,
    options: ExportOptions,
    packaging: Packaging,
}

impl ExportPlan {
    /// Validate a plan (spec §9.2: combinations that do not apply are
    /// rejected) and decide its packaging (D126): a single file for one
    /// conversation in one format other than CSV without copied files, a
    /// bundle otherwise.
    pub fn new(
        scope: ScopePlan,
        mut formats: Vec<Format>,
        options: ExportOptions,
    ) -> Result<ExportPlan, ExportError> {
        let refuse = |r| Err(ExportError::Plan(r));
        if formats.is_empty() {
            return refuse(PlanRefusal::NoFormat);
        }
        let n = formats.len();
        formats.sort_unstable();
        formats.dedup();
        if formats.len() != n {
            return refuse(PlanRefusal::DuplicateFormat);
        }
        let has = |f: Format| formats.contains(&f);
        let o = options;
        if o.identifiers == IdentifierMode::ArchiveAndSource
            && !formats.iter().any(|f| f.carries_identifiers())
        {
            return refuse(PlanRefusal::IdentifiersWithoutTabularOrJson);
        }
        for (on, n) in [
            (o.reveal.one_time_codes, o.reveal.codes_confirmed_up_to),
            (o.reveal.earlier_content, o.reveal.earlier_confirmed_up_to),
        ] {
            match (on, n) {
                (true, None) => return refuse(PlanRefusal::RevealNotConfirmed),
                (false, Some(_)) => return refuse(PlanRefusal::ConfirmationWithoutReveal),
                _ => {}
            }
        }
        if has(Format::Csv) != o.csv_profile.is_some() {
            return refuse(PlanRefusal::CsvProfileMismatch);
        }
        if has(Format::Pdf) != o.page_size.is_some() {
            return refuse(PlanRefusal::OptionDoesNotApply);
        }
        if o.keep_extensions && o.attachments != AttachmentMode::CopyOriginals {
            return refuse(PlanRefusal::OptionDoesNotApply);
        }
        if o.previews && !(has(Format::Html) || has(Format::Pdf)) {
            return refuse(PlanRefusal::OptionDoesNotApply);
        }
        if o.larger_previews && !o.previews {
            return refuse(PlanRefusal::OptionDoesNotApply);
        }
        let bundle = bundle_without_files(&scope, &formats)
            || o.attachments == AttachmentMode::CopyOriginals;
        Ok(ExportPlan {
            scope,
            formats,
            options,
            packaging: if bundle {
                Packaging::Bundle
            } else {
                Packaging::SingleFile
            },
        })
    }

    pub fn scope(&self) -> &ScopePlan {
        &self.scope
    }

    /// The formats, in a fixed order.
    pub fn formats(&self) -> &[Format] {
        &self.formats
    }

    pub fn options(&self) -> &ExportOptions {
        &self.options
    }

    pub fn packaging(&self) -> Packaging {
        self.packaging
    }

    /// The reveal the archive reads apply (never the viewer's setting).
    pub fn reveal(&self) -> Reveal {
        self.options.reveal.reveal()
    }

    /// The assembly depth: history streams only when included.
    pub fn depth(&self) -> ExportDepth {
        if self.options.history {
            ExportDepth::WithHistory
        } else {
            ExportDepth::Items
        }
    }

    /// The options the record mapping applies.
    pub fn record_options(&self) -> RecordOptions {
        RecordOptions {
            history: self.options.history,
            reactions: self.options.reactions,
            identifiers: self.options.identifiers,
            names_only: self.options.attachments == AttachmentMode::None,
        }
    }

    /// Check the job's pre-count (under the lock) against the confirmed
    /// counts (D147): a reveal's count above its own N stops the job
    /// before anything is written.
    pub fn check_confirmation(
        &self,
        flagged_codes: u64,
        earlier_content: u64,
    ) -> Result<(), ExportError> {
        let r = self.options.reveal;
        let over = |on: bool, n: Option<u64>, count: u64| on && n.is_none_or(|n| count > n);
        let over = over(r.one_time_codes, r.codes_confirmed_up_to, flagged_codes)
            || over(
                r.earlier_content,
                r.earlier_confirmed_up_to,
                earlier_content,
            );
        if over {
            Err(ExportError::ConfirmationStale)
        } else {
            Ok(())
        }
    }
}

/// What the record mapping needs from the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecordOptions {
    pub history: bool,
    pub reactions: bool,
    pub identifiers: IdentifierMode,
    /// No file is copied: every attachment with a stored file is
    /// `notRequested`.
    pub names_only: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv() -> ScopePlan {
        ScopePlan::Conversation {
            chat: ChatId::new(1),
        }
    }

    fn plan(scope: ScopePlan, formats: &[Format]) -> Result<ExportPlan, ExportError> {
        ExportPlan::new(
            scope.clone(),
            formats.to_vec(),
            ExportOptions::defaults(&scope, formats),
        )
    }

    fn refusal(r: Result<ExportPlan, ExportError>) -> PlanRefusal {
        match r {
            Err(ExportError::Plan(p)) => p,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn packaging_follows_d126_and_q3a() {
        // One conversation, one format: a single file with names only.
        for f in [
            Format::Txt,
            Format::Json,
            Format::Html,
            Format::Pdf,
            Format::Xlsx,
        ] {
            let p = plan(conv(), &[f]).expect("plan");
            assert_eq!(p.packaging(), Packaging::SingleFile, "{f:?}");
            assert_eq!(p.options().attachments, AttachmentMode::None);
        }
        // CSV is always a bundle, and copies only when ticked.
        let p = plan(conv(), &[Format::Csv]).expect("plan");
        assert_eq!(
            (p.packaging(), p.options().attachments),
            (Packaging::Bundle, AttachmentMode::None)
        );
        // An HTML or JSON bundle copies by default; TXT and XLSX do not.
        let p = plan(conv(), &[Format::Html, Format::Txt]).expect("plan");
        assert_eq!(p.options().attachments, AttachmentMode::CopyOriginals);
        let p = plan(conv(), &[Format::Txt, Format::Xlsx]).expect("plan");
        assert_eq!(
            (p.packaging(), p.options().attachments),
            (Packaging::Bundle, AttachmentMode::None)
        );
        // The whole archive copies by default, whatever the format.
        let p = plan(ScopePlan::Archive, &[Format::Txt]).expect("plan");
        assert_eq!(p.options().attachments, AttachmentMode::CopyOriginals);
        // Copying files makes a bundle.
        let mut o = ExportOptions::defaults(&conv(), &[Format::Txt]);
        o.attachments = AttachmentMode::CopyOriginals;
        let p = ExportPlan::new(conv(), vec![Format::Txt], o).expect("plan");
        assert_eq!(p.packaging(), Packaging::Bundle);
    }

    #[test]
    fn combinations_that_do_not_apply_are_rejected() {
        assert_eq!(refusal(plan(conv(), &[])), PlanRefusal::NoFormat);
        assert_eq!(
            refusal(plan(conv(), &[Format::Txt, Format::Txt])),
            PlanRefusal::DuplicateFormat
        );
        let with = |f: &dyn Fn(&mut ExportOptions), formats: &[Format]| {
            let mut o = ExportOptions::defaults(&conv(), formats);
            f(&mut o);
            ExportPlan::new(conv(), formats.to_vec(), o)
        };
        assert_eq!(
            refusal(with(
                &|o| o.identifiers = IdentifierMode::ArchiveAndSource,
                &[Format::Txt, Format::Html]
            )),
            PlanRefusal::IdentifiersWithoutTabularOrJson
        );
        assert!(with(
            &|o| o.identifiers = IdentifierMode::ArchiveAndSource,
            &[Format::Json]
        )
        .is_ok());
        assert_eq!(
            refusal(with(&|o| o.reveal.one_time_codes = true, &[Format::Txt])),
            PlanRefusal::RevealNotConfirmed
        );
        assert_eq!(
            refusal(with(
                &|o| o.reveal.codes_confirmed_up_to = Some(3),
                &[Format::Txt]
            )),
            PlanRefusal::ConfirmationWithoutReveal
        );
        assert_eq!(
            refusal(with(&|o| o.csv_profile = None, &[Format::Csv])),
            PlanRefusal::CsvProfileMismatch
        );
        assert_eq!(
            refusal(with(&|o| o.keep_extensions = true, &[Format::Txt])),
            PlanRefusal::OptionDoesNotApply
        );
        assert_eq!(
            refusal(with(&|o| o.previews = true, &[Format::Txt])),
            PlanRefusal::OptionDoesNotApply
        );
        assert_eq!(
            refusal(with(&|o| o.page_size = Some(PageSize::A4), &[Format::Txt])),
            PlanRefusal::OptionDoesNotApply
        );
    }

    #[test]
    fn reveals_are_off_by_default_and_checked_against_the_confirmed_count() {
        let p = plan(conv(), &[Format::Txt]).expect("plan");
        assert_eq!(p.reveal(), Reveal::default());
        assert!(p.check_confirmation(100, 100).is_ok());
        let mut o = ExportOptions::defaults(&conv(), &[Format::Txt]);
        o.reveal = RevealPlan::confirmed(Some(3), None);
        let p = ExportPlan::new(conv(), vec![Format::Txt], o).expect("plan");
        assert!(p.reveal().one_time_code && !p.reveal().earlier_text);
        assert!(p.check_confirmation(3, 99).is_ok());
        assert!(matches!(
            p.check_confirmation(4, 0),
            Err(ExportError::ConfirmationStale)
        ));
        // Both reveals: each count against its own N, never the other's.
        o.reveal = RevealPlan::confirmed(Some(2), Some(40));
        let p = ExportPlan::new(conv(), vec![Format::Txt], o).expect("plan");
        assert!(p.check_confirmation(2, 40).is_ok());
        assert!(matches!(
            p.check_confirmation(3, 40),
            Err(ExportError::ConfirmationStale)
        ));
        assert!(matches!(
            p.check_confirmation(2, 41),
            Err(ExportError::ConfirmationStale)
        ));
        // One reveal on with only the other's count is refused.
        o.reveal = RevealPlan {
            one_time_codes: true,
            earlier_content: true,
            codes_confirmed_up_to: Some(2),
            earlier_confirmed_up_to: None,
        };
        assert!(matches!(
            ExportPlan::new(conv(), vec![Format::Txt], o),
            Err(ExportError::Plan(PlanRefusal::RevealNotConfirmed))
        ));
        assert_eq!(format!("{:?}", p.scope()), "conversation");
    }
}
