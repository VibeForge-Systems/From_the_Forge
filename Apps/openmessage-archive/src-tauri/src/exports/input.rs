//! `preview_export`'s request turned into an export plan (M6 spec §9.2).
//!
//! Every combination that does not apply is refused (`invalidInput`), never
//! ignored, through `oma_export::ExportPlan::new`. What this version does
//! not build is refused with `exportUnavailable` and its own sentence:
//! XLSX (deferred until its writer crate is old enough to pin). PDF is
//! built from slice 6j2. Image previews are built (slice 6h2: the export's
//! decode-helper pool) and apply to HTML and PDF. Leaving events out and
//! "Save a copy" of one attachment are built from slice 6k2.
//! Nothing is faked.
//!
//! **"Save a copy"** ([`parse_request`] → [`Parsed::Copy`]) has no format
//! and no records, so its request is fixed: no formats, and the options
//! [`copy_options_apply`] accepts (every option off, files copied, UTC).
//! Its one sensitive choice, content captured before an unsend, is
//! confirmed at `start_export` against the attachment's own facts.

use oma_archive::{ArchiveId, AttachmentId, ChatId};
use oma_export::plan::{Clock, LineEndings, PageSize, ZoneChoice};
use oma_export::{
    AttachmentMode, CivilDate, CsvProfile, ExportOptions, ExportPlan, Format, IdentifierMode,
    RevealPlan, ScopePlan, ZoneTableInput,
};

use super::dto::{
    AttachmentModeInput, ClockInput, CsvProfileInput, ExportFormatInput, ExportOptionsInput,
    ExportScopeInput, LineEndingsInput, PageSizeInput, PreviewExportRequest, ZoneChoiceInput,
    ZoneTableInputDto,
};
use super::messages as em;
use crate::archives::parse_id;
use crate::error::{ErrorCode, IpcError, IpcResult};

/// Where the scope's messages come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// An archive, by id (conversation, date range, whole archive).
    Archive(ArchiveId),
    /// The viewer's live search set (the archive is the session's).
    Search {
        viewer: u32,
        search_id: u32,
        all_matches: bool,
    },
}

/// "Save a copy" of one attachment the viewer surfaced (§6.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopySpec {
    /// The viewer session's generation.
    pub viewer: u32,
    pub attachment: AttachmentId,
}

/// A parsed `preview_export` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Export(DraftSpec),
    Copy(CopySpec),
}

/// A parsed, validated request: the plan's parts, without the reveal's
/// confirmed count (which only `start_export` supplies).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftSpec {
    pub source: Source,
    pub scope: ScopePlan,
    pub formats: Vec<Format>,
    pub options: ExportOptions,
}

impl DraftSpec {
    /// The plan with each reveal's confirmed count (`codes` for one-time
    /// codes, `earlier` for earlier content; `None` for a reveal that is
    /// off).
    pub fn plan(
        &self,
        codes: Option<u64>,
        earlier: Option<u64>,
    ) -> Result<ExportPlan, oma_export::ExportError> {
        let mut o = self.options;
        o.reveal.codes_confirmed_up_to = codes;
        o.reveal.earlier_confirmed_up_to = earlier;
        ExportPlan::new(self.scope.clone(), self.formats.clone(), o)
    }

    /// The plan as the preview counts it: a reveal is checked with a
    /// placeholder count (the counts are what the user will confirm).
    pub fn probe_plan(&self) -> IpcResult<ExportPlan> {
        let r = self.options.reveal;
        self.plan(
            r.one_time_codes.then_some(u64::MAX),
            r.earlier_content.then_some(u64::MAX),
        )
        .map_err(|_| IpcError::new(ErrorCode::InvalidInput, em::OPTIONS_DO_NOT_APPLY))
    }

    /// The local dates of a date range.
    pub fn date_range(&self) -> Option<(CivilDate, CivilDate)> {
        match &self.scope {
            ScopePlan::DateRange { from, until, .. } => Some((*from, *until)),
            _ => None,
        }
    }

    /// The zone is UTC (no table needed).
    pub fn utc(&self) -> bool {
        self.options.zone == ZoneChoice::Utc
    }
}

fn format(f: ExportFormatInput) -> IpcResult<Format> {
    Ok(match f {
        ExportFormatInput::Txt => Format::Txt,
        ExportFormatInput::Csv => Format::Csv,
        ExportFormatInput::Json => Format::Json,
        ExportFormatInput::Html => Format::Html,
        // PDF from slice 6j2, through the export-render helper.
        ExportFormatInput::Pdf => Format::Pdf,
        // XLSX is deferred (its writer crate is not yet 14 days old).
        ExportFormatInput::Xlsx => return Err(em::unavailable(em::UNAVAILABLE_FORMAT)),
    })
}

fn chat(id: crate::viewer::input::InId) -> IpcResult<ChatId> {
    id.get().map(ChatId::new).map_err(|_| em::invalid())
}

fn date_range_refused() -> IpcError {
    IpcError::new(ErrorCode::InvalidInput, em::DATE_RANGE_REFUSED)
}

/// The scope and where it comes from.
fn scope(s: &ExportScopeInput) -> IpcResult<(Source, ScopePlan)> {
    Ok(match s {
        ExportScopeInput::Conversation {
            archive_id,
            chat_id,
        } => (
            Source::Archive(parse_id(archive_id)?),
            ScopePlan::Conversation {
                chat: chat(*chat_id)?,
            },
        ),
        ExportScopeInput::DateRange {
            archive_id,
            from_local_date,
            until_local_date,
            chat_id,
        } => {
            let from = from_local_date.civil().ok_or_else(date_range_refused)?;
            let until = until_local_date.civil().ok_or_else(date_range_refused)?;
            if until <= from {
                return Err(date_range_refused());
            }
            (
                Source::Archive(parse_id(archive_id)?),
                ScopePlan::DateRange {
                    from,
                    until,
                    chat: chat_id.map(chat).transpose()?,
                },
            )
        }
        ExportScopeInput::SearchResults {
            viewer,
            search_id,
            all_matches,
        } => (
            Source::Search {
                viewer: *viewer,
                search_id: *search_id,
                all_matches: *all_matches,
            },
            ScopePlan::SearchResults {
                all_matches: *all_matches,
            },
        ),
        ExportScopeInput::Archive { archive_id } => {
            (Source::Archive(parse_id(archive_id)?), ScopePlan::Archive)
        }
        // "Save a copy" is not an export plan (`parse_request`).
        ExportScopeInput::Attachment { .. } => return Err(em::invalid()),
    })
}

/// The one option set "Save a copy" accepts: the file is copied, every
/// other option is off (none applies to one file without records), and
/// the zone is UTC (nothing is placed in time). Earlier content is
/// confirmed at start, never asked for here.
pub fn copy_options_apply(o: &ExportOptionsInput) -> bool {
    let ExportOptionsInput {
        attachments,
        keep_extensions,
        previews,
        larger_previews,
        history,
        reactions,
        events,
        reveal_one_time_codes,
        reveal_earlier_content,
        hidden_character_markers,
        seconds,
        include_identifiers,
        clock,
        line_endings,
        csv_profile,
        page_size,
        zone,
    } = *o;
    attachments == AttachmentModeInput::CopyOriginals
        && ![
            keep_extensions,
            previews,
            larger_previews,
            history,
            reactions,
            events,
            reveal_one_time_codes,
            reveal_earlier_content,
            hidden_character_markers,
            seconds,
            include_identifiers,
        ]
        .contains(&true)
        && clock == ClockInput::H23
        && line_endings == LineEndingsInput::Lf
        && csv_profile.is_none()
        && page_size.is_none()
        && zone == ZoneChoiceInput::Utc
}

fn options(o: &ExportOptionsInput) -> IpcResult<ExportOptions> {
    Ok(ExportOptions {
        attachments: match o.attachments {
            AttachmentModeInput::None => AttachmentMode::None,
            AttachmentModeInput::CopyOriginals => AttachmentMode::CopyOriginals,
        },
        keep_extensions: o.keep_extensions,
        previews: o.previews,
        larger_previews: o.larger_previews,
        history: o.history,
        reactions: o.reactions,
        events: o.events,
        reveal: RevealPlan {
            one_time_codes: o.reveal_one_time_codes,
            earlier_content: o.reveal_earlier_content,
            codes_confirmed_up_to: None,
            earlier_confirmed_up_to: None,
        },
        hidden_character_markers: o.hidden_character_markers,
        seconds: o.seconds,
        identifiers: if o.include_identifiers {
            IdentifierMode::ArchiveAndSource
        } else {
            IdentifierMode::Ordinals
        },
        clock: match o.clock {
            ClockInput::H23 => Clock::H23,
            ClockInput::H12 => Clock::H12,
        },
        line_endings: match o.line_endings {
            LineEndingsInput::Lf => LineEndings::Lf,
            LineEndingsInput::Crlf => LineEndings::Crlf,
        },
        csv_profile: o.csv_profile.map(|p| match p {
            CsvProfileInput::Spreadsheet => CsvProfile::Spreadsheet,
            CsvProfileInput::DataTools => CsvProfile::DataTools,
        }),
        page_size: o.page_size.map(|p| match p {
            PageSizeInput::A4 => PageSize::A4,
            PageSizeInput::Letter => PageSize::Letter,
        }),
        zone: match o.zone {
            ZoneChoiceInput::System => ZoneChoice::System,
            ZoneChoiceInput::Utc => ZoneChoice::Utc,
            ZoneChoiceInput::Named => ZoneChoice::Named,
        },
    })
}

/// Parse and validate `preview_export`'s request: an export, or "Save a
/// copy" of one attachment (no formats, [`copy_options_apply`], no zone
/// table).
pub fn parse_request(req: &PreviewExportRequest) -> IpcResult<Parsed> {
    if let ExportScopeInput::Attachment {
        viewer,
        attachment_id,
    } = &req.scope
    {
        if !req.formats.is_empty() || req.zone.is_some() || !copy_options_apply(&req.options) {
            return Err(IpcError::new(
                ErrorCode::InvalidInput,
                em::OPTIONS_DO_NOT_APPLY,
            ));
        }
        let attachment = AttachmentId::new(attachment_id.get().map_err(|_| em::invalid())?);
        return Ok(Parsed::Copy(CopySpec {
            viewer: *viewer,
            attachment,
        }));
    }
    parse(req).map(Parsed::Export)
}

/// Parse and validate an export's request (the zone table aside). "Save a
/// copy" is [`parse_request`]'s.
pub fn parse(req: &PreviewExportRequest) -> IpcResult<DraftSpec> {
    if req.formats.is_empty() || req.formats.len() > Format::ALL.len() {
        return Err(IpcError::new(
            ErrorCode::InvalidInput,
            em::OPTIONS_DO_NOT_APPLY,
        ));
    }
    let formats = req
        .formats
        .iter()
        .map(|f| format(*f))
        .collect::<IpcResult<Vec<_>>>()?;
    let (source, scope) = scope(&req.scope)?;
    let options = options(&req.options)?;
    let spec = DraftSpec {
        source,
        scope,
        formats,
        options,
    };
    spec.probe_plan()?;
    Ok(spec)
}

/// The webview's zone table as the crate takes it. An offset beyond
/// `i32` saturates to ±(2^31 − 1), which the crate's ±18 h check refuses.
pub fn zone_input(z: &ZoneTableInputDto) -> ZoneTableInput {
    ZoneTableInput {
        name: z.name.0.clone(),
        window_utc_ms: (z.window_utc_ms[0], z.window_utc_ms[1]),
        transitions: z
            .transitions
            .iter()
            .map(|[t, o]| {
                let max = i64::from(i32::MAX);
                (*t, i32::try_from((*o).clamp(-max, max)).unwrap_or(i32::MAX))
            })
            .collect(),
    }
}
