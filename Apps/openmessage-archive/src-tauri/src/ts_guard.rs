//! The structural guard over the generated IPC declarations (M3 §16.4,
//! M4 §15.4), shared by the Backups and Archive DTO tests: a walk of every
//! declared type reachable from some roots, and a parser for the field
//! keys of a declaration. Test-only.

use std::any::TypeId;
use std::collections::{BTreeMap, HashSet};

use ts_rs::{Config, TypeVisitor, TS};

/// Every declared IPC type reachable from the roots, by name.
pub struct Walk {
    cfg: Config,
    seen: HashSet<TypeId>,
    decls: BTreeMap<String, String>,
}

impl TypeVisitor for Walk {
    fn visit<T: TS + 'static + ?Sized>(&mut self) {
        if T::output_path().is_none() || !self.seen.insert(TypeId::of::<T>()) {
            return;
        }
        self.decls.insert(T::ident(&self.cfg), T::decl(&self.cfg));
        T::visit_dependencies(self);
    }
}

impl Walk {
    pub fn new() -> Walk {
        Walk {
            cfg: Config::from_env(),
            seen: HashSet::new(),
            decls: BTreeMap::new(),
        }
    }

    pub fn root<T: TS + 'static + ?Sized>(&mut self) {
        self.visit::<T>();
    }

    pub fn decls(self) -> BTreeMap<String, String> {
        self.decls
    }
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start..].find("*/") {
            Some(end) => rest = &rest[start + end + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Whether a field's type text names `string`.
pub fn is_string(field_ty: &str) -> bool {
    field_ty
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == "string")
}

/// The field keys of a declaration and the text of each field's type.
/// String literal *values* (such as `"readingHandles"`) are not keys.
pub fn fields(decl: &str) -> Vec<(String, String)> {
    let body = strip_comments(decl);
    let body = body.split_once('=').map_or(body.as_str(), |(_, b)| b);
    let chars: Vec<char> = body.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '{' && chars[i] != ',' {
            i += 1;
            continue;
        }
        // A key: an identifier or a quoted string, then `?:` or `:`.
        let mut j = i + 1;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        let mut key = String::new();
        if j < chars.len() && chars[j] == '"' {
            j += 1;
            while j < chars.len() && chars[j] != '"' {
                key.push(chars[j]);
                j += 1;
            }
            j += 1;
        } else {
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                key.push(chars[j]);
                j += 1;
            }
        }
        if j < chars.len() && chars[j] == '?' {
            j += 1;
        }
        if key.is_empty() || j >= chars.len() || chars[j] != ':' {
            i += 1;
            continue;
        }
        // Its type, up to the next `,` or `}` at the same depth.
        j += 1;
        let mut depth = 0i32;
        let mut ty = String::new();
        while j < chars.len() {
            let c = chars[j];
            match c {
                '{' | '<' | '(' | '[' => depth += 1,
                '}' | '>' | ')' | ']' if depth > 0 => depth -= 1,
                ',' | '}' if depth == 0 => break,
                _ => {}
            }
            ty.push(c);
            j += 1;
        }
        out.push((key, ty.trim().to_owned()));
        i = j;
    }
    out
}

/// The types the Backups guard reviews (milestones 2 and 3).
pub const M2_M3_TYPES: &[&str] = &[
    "AttachmentOutcomesDto",
    "BackupDetails",
    "BackupState",
    "BackupSummary",
    "CheckLevel",
    "CleanupDto",
    "CountsDto",
    "DamageReasonDto",
    "DatesDto",
    "DbFactsDto",
    "DeviceDetails",
    "EncryptionDetails",
    "ErrorCode",
    "ExtractionOutcomeDto",
    "ExtractionReportDto",
    "FailureKindDto",
    "FeaturesDto",
    "IncompleteReasonDto",
    "IntegrityDto",
    "IpcError",
    "JobEvent",
    "JobKind",
    "JobPhase",
    "JobSummary",
    "KnownColumnDto",
    "KnownTableDto",
    "LayoutDetails",
    "BackupFormat",
    "ManifestStatsDto",
    "OpenModeDto",
    "ProblemDto",
    "QualifierDto",
    "SchemaReportDto",
    "SmsDbStatusDto",
    "SourceFile",
    "SourcedText",
    "StatusDetails",
    "TableReportDto",
    "TextEncodingDto",
    "UnsupportedReasonDto",
    "ValidationCheck",
    "ValidationReportDto",
    "WalFramesDto",
    "WalOutcomeDto",
];

/// The archive types `JobEvent` reaches since milestone 4 (the Archive
/// guard reviews them).
pub const M4_EVENT_TYPES: &[&str] = &[
    "GateEndingDto",
    "ImportOutcomeDto",
    "NotImportableDto",
    "StopReasonDto",
    "VerifyOutcomeDto",
];

/// The archive types `JobEvent` reaches through `exportFinished` (M6
/// §9.3): the archive guard reviews them, as it does the M4 ones.
pub const M6_EVENT_TYPES: &[&str] = &[
    "AttachmentExclusionsDto",
    "ExportOutcomeDto",
    "ExportSizeBucketDto",
    "ExportSummaryDto",
];

/// The export walk's own types (the guard compares this list with the
/// walk).
pub const M6_EXPORT_TYPES: &[&str] = &[
    "AttachmentModeInput",
    "ChooseExportDestinationRequest",
    "ClockInput",
    "CopyPreviewDto",
    "CsvProfileInput",
    "DestinationToken",
    "DraftId",
    "DraftRef",
    "ExcludedHitsDto",
    "ExportConfirmationsInput",
    "ExportDestinationDto",
    "ExportDraftDto",
    "ExportFormatInput",
    "ExportOptionsInput",
    "ExportPreviewDto",
    "ExportScopeInput",
    "ExportWarningDto",
    "FreeSpaceDto",
    "LineEndingsInput",
    "LocalDate",
    "PackagingDto",
    "PageSizeInput",
    "PreviewEvent",
    "PreviewExportRequest",
    "RevealConfirmationInput",
    "SearchPreviewDto",
    "ShowExportInFolderRequest",
    "SpaceVerdictDto",
    "StartExportRequest",
    "SyncProviderDto",
    "ZoneChoiceInput",
    "ZoneName",
    "ZoneTableInputDto",
];

/// Types the export walk shares with another walk: the viewer's brands
/// (`SafeId` for input ids, `UntrustedText` for `displayName` only) and
/// the error code a failed preview carries.
pub const M6_SHARED_TYPES: &[&str] = &["ErrorCode", "SafeId", "UntrustedText"];
