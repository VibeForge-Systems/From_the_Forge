//! Archives: the shell side of milestone 4 (spec §15).
//!
//! Nothing here depends on Tauri, so the listing, the details, the gate
//! and the import and verify workers are tested without a window. The
//! command layer (`commands::archives`) adds the blocking pool and the
//! `Channel` transport.
//!
//! Every archive operation goes through `oma_archive`: the shell never
//! opens an archive's files itself. It logs job ids, kinds and outcomes,
//! never an archive id, a label or a path (I6).

pub mod dto;
pub mod gate;
pub mod messages;
pub mod worker;
#[cfg(test)]
mod worker_tests;

use std::path::Path;
use std::sync::Arc;

use oma_archive::space::SpaceProbe;
use oma_archive::{ArchiveEntry, ArchiveError, ArchiveId, Label, Library, SourceFacts};
use oma_backup::Limits;

use crate::backups::dto::{BackupSummary, JobKind, SourceFile};
use crate::backups::Jobs;
use crate::error::{ErrorCode, IpcError, IpcResult};
use dto::{
    summary_dto, ArchiveDetailsDto, ArchiveEntryDto, ArchiveListingDto, ArchiveStatusDto,
    ArchiveSummaryDto, LastImportDto, LibraryStateDto, ResumeOptionsDto, SessionDto,
    VerifySummaryDto,
};

/// How the shell builds a [`Library`]. Production uses the real free-space
/// probe; tests inject one, so no test depends on the runner's disk.
#[derive(Clone, Default)]
pub struct ArchiveEnv {
    pub space: Option<Arc<dyn SpaceProbe>>,
}

impl std::fmt::Debug for ArchiveEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveEnv")
            .field("fake_space", &self.space.is_some())
            .finish()
    }
}

impl ArchiveEnv {
    pub fn library(&self, root: &Path) -> Result<Library, ArchiveError> {
        let lib = Library::new(root)?;
        Ok(match &self.space {
            Some(p) => lib.with_space_probe(Arc::clone(p)),
            None => lib,
        })
    }
}

fn ipc(e: &ArchiveError) -> IpcError {
    messages::to_ipc(e, &Limits::default())
}

/// Parse an archive id from the webview: `a` and 32 lowercase hex
/// characters, else `invalidInput`.
pub fn parse_id(s: &str) -> IpcResult<ArchiveId> {
    ArchiveId::parse(s)
        .map_err(|_| IpcError::new(ErrorCode::InvalidInput, messages::INVALID_ARCHIVE_ID))
}

/// Parse a label from the webview. An empty string means no label.
pub fn parse_label(s: Option<&str>) -> IpcResult<Option<Label>> {
    match s {
        None | Some("") => Ok(None),
        Some(s) => Label::parse(s)
            .map(Some)
            .map_err(|_| IpcError::new(ErrorCode::InvalidInput, messages::INVALID_LABEL)),
    }
}

/// `list_archives` (spec §15.2). An archive this app is importing into or
/// verifying is reported as `importing` or `verifying`, not as in use by
/// another program (§15.1).
pub fn listing(root: Option<&Path>, env: &ArchiveEnv, jobs: &Jobs) -> ArchiveListingDto {
    let unusable = |e: &ArchiveError| ArchiveListingDto {
        entries: Vec::new(),
        library_state: LibraryStateDto::Unusable,
        library_error: Some(ipc(e)),
    };
    let Some(root) = root else {
        return ArchiveListingDto {
            entries: Vec::new(),
            library_state: LibraryStateDto::NotConfigured,
            library_error: None,
        };
    };
    let lib = match env.library(root) {
        Ok(l) => l,
        Err(e) => return unusable(&e),
    };
    let found = match lib.list() {
        Ok(v) => v,
        Err(e) => return unusable(&e),
    };
    let entries = found.iter().filter_map(|e| entry_dto(e, jobs)).collect();
    ArchiveListingDto {
        entries,
        library_state: LibraryStateDto::Ready,
        library_error: None,
    }
}

fn bare(id: &ArchiveId, status: ArchiveStatusDto) -> ArchiveEntryDto {
    ArchiveEntryDto {
        archive_id: id.as_str().to_owned(),
        status,
        schema_version: None,
        damage: None,
        refusal: None,
        summary: None,
    }
}

/// One listed archive; `None` for an entry kind this build does not know.
pub fn entry_dto(e: &ArchiveEntry, jobs: &Jobs) -> Option<ArchiveEntryDto> {
    Some(match e {
        ArchiveEntry::Ready(info) => ArchiveEntryDto {
            summary: Some(summary_dto(info)),
            ..bare(&info.id, ArchiveStatusDto::Ready)
        },
        ArchiveEntry::InUse { id } => {
            let status = match jobs.active_for_archive(id.as_str()) {
                Some((job, _)) if job.kind == JobKind::Import => ArchiveStatusDto::Importing,
                Some((job, _)) if job.kind == JobKind::Verify => ArchiveStatusDto::Verifying,
                _ => ArchiveStatusDto::InUse,
            };
            bare(id, status)
        }
        ArchiveEntry::NeedsMigration { id, version } => ArchiveEntryDto {
            schema_version: Some(*version),
            ..bare(id, ArchiveStatusDto::NeedsUpgrade)
        },
        ArchiveEntry::NewerSchema { id, version } => ArchiveEntryDto {
            schema_version: Some(*version),
            ..bare(id, ArchiveStatusDto::NewerVersion)
        },
        ArchiveEntry::NotAnArchive { id } => bare(id, ArchiveStatusDto::NotAnArchive),
        ArchiveEntry::Damaged { id, kind } => ArchiveEntryDto {
            damage: Some((*kind).into()),
            ..bare(id, ArchiveStatusDto::Damaged)
        },
        ArchiveEntry::Refused { id, why } => ArchiveEntryDto {
            refusal: Some((*why).into()),
            ..bare(id, ArchiveStatusDto::Refused)
        },
        _ => return None,
    })
}

/// `create_archive`: a new archive, then its summary through the reader.
pub fn create(root: &Path, env: &ArchiveEnv, label: Option<Label>) -> IpcResult<ArchiveSummaryDto> {
    let lib = env.library(root).map_err(|e| ipc(&e))?;
    let id = lib.create(label).map_err(|e| ipc(&e))?;
    let reader = lib.open_read(&id).map_err(|e| ipc(&e))?;
    let info = reader.info().map_err(|e| ipc(&e))?;
    Ok(summary_dto(&info))
}

/// `YYYY-MM-DDTHH:MM:SSZ` (as `backups::dto::rfc3339` writes it) as Unix
/// seconds.
pub fn rfc3339_secs(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[19] != b'Z' {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = s.get(r)?;
        if !t.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        t.parse().ok()
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    // Days from the civil date (the inverse of `rfc3339`'s algorithm).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se)
}

/// The facts the resume pre-selection compares, from a listed backup:
/// only values read from Info.plist, as the import records them.
fn source_facts(b: &BackupSummary) -> SourceFacts<'_> {
    fn info(t: &Option<crate::backups::dto::SourcedText>) -> Option<&str> {
        t.as_ref()
            .filter(|t| t.source == SourceFile::InfoPlist)
            .map(|t| t.value.as_str())
    }
    SourceFacts {
        device_name: info(&b.device_name),
        product_type: info(&b.product_type),
        last_backup_secs: b.last_backup_date.as_deref().and_then(rfc3339_secs),
    }
}

/// `get_archive_details` (spec §15.2), read through the lock-free reader,
/// so it works during an import. `backups` are the listed backups, for
/// `matchingBackupIds`.
pub fn details(
    root: &Path,
    env: &ArchiveEnv,
    id: &ArchiveId,
    jobs: &Jobs,
    backups: &[BackupSummary],
) -> IpcResult<ArchiveDetailsDto> {
    let lib = env.library(root).map_err(|e| ipc(&e))?;
    let reader = lib.open_read(id).map_err(|e| ipc(&e))?;
    let info = reader.info().map_err(|e| ipc(&e))?;
    let sessions = reader.sessions().map_err(|e| ipc(&e))?;
    let last_import = reader.last_import().map_err(|e| ipc(&e))?;
    let last_verify = reader.last_verify().map_err(|e| ipc(&e))?;
    let resumable = info.resumable.is_some();
    let matching_backup_ids = if resumable {
        let facts: Vec<SourceFacts<'_>> = backups.iter().map(source_facts).collect();
        let hits = reader.resume_matches(&facts).map_err(|e| ipc(&e))?;
        backups
            .iter()
            .zip(hits)
            .filter(|(_, hit)| *hit)
            .map(|(b, _)| b.id.clone())
            .collect()
    } else {
        Vec::new()
    };
    let (active_job, preflight) = match jobs.active_for_archive(id.as_str()) {
        Some((job, preflight)) => (Some(job), preflight),
        None => (None, None),
    };
    Ok(ArchiveDetailsDto {
        summary: summary_dto(&info),
        sessions: sessions.iter().map(SessionDto::from).collect(),
        last_import: last_import.as_ref().map(LastImportDto::from),
        last_verify: last_verify.as_ref().map(VerifySummaryDto::from),
        active_job,
        preflight,
        resumable,
        matching_backup_ids,
        resume_options: info.resumable.map(|r| ResumeOptionsDto {
            include_recently_deleted: r.include_recently_deleted,
            copy_attachments: r.copy_attachments,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backups::dto::{rfc3339, BackupState, SourcedText};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn rfc3339_parses_what_rfc3339_writes() {
        for secs in [
            0i64,
            951_782_400,
            1_792_281_599,
            -86_400,
            -978_307_200,
            4_102_444_800,
        ] {
            let t = if secs >= 0 {
                UNIX_EPOCH + Duration::from_secs(secs.unsigned_abs())
            } else {
                UNIX_EPOCH - Duration::from_secs(secs.unsigned_abs())
            };
            assert_eq!(rfc3339_secs(&rfc3339(t)), Some(secs), "{secs}");
        }
        for bad in [
            "",
            "2026-10-18",
            "2026-13-01T00:00:00Z",
            "2026-10-18T24:00:00Z",
            "2026-10-18T00:00:00+",
            "２026-10-18T00:00:00Z",
            "2026-1a-18T00:00:00Z",
        ] {
            assert_eq!(rfc3339_secs(bad), None, "{bad}");
        }
    }

    #[test]
    fn ids_and_labels_are_validated() {
        assert!(parse_id(&format!("a{}", "0".repeat(32))).is_ok());
        for bad in ["", "a", "../a", &format!("A{}", "0".repeat(32))] {
            assert_eq!(
                parse_id(bad).expect_err("bad").code,
                ErrorCode::InvalidInput
            );
        }
        assert_eq!(parse_label(None).expect("none"), None);
        assert_eq!(parse_label(Some("")).expect("empty"), None);
        assert!(parse_label(Some("Family phone")).expect("ok").is_some());
        for bad in ["x\u{7}", "a\u{202e}b", &"x".repeat(201)] {
            assert_eq!(
                parse_label(Some(bad)).expect_err("bad").code,
                ErrorCode::InvalidInput
            );
        }
    }

    #[test]
    fn only_info_plist_facts_are_compared() {
        let b = BackupSummary {
            id: "b".into(),
            location_id: "l".into(),
            via_link: false,
            device_name: Some(SourcedText {
                value: "Synthetic".into(),
                source: SourceFile::ManifestPlistLockdown,
            }),
            product_type: Some(SourcedText {
                value: "iPhone1,1".into(),
                source: SourceFile::InfoPlist,
            }),
            product_name: None,
            ios_version: None,
            last_backup_date: Some("1970-01-01T00:01:00Z".into()),
            encrypted: Some(false),
            state: BackupState::Ready,
            state_reason: String::new(),
            active_job_id: None,
            last_report_importable: None,
        };
        let f = source_facts(&b);
        assert_eq!(f.device_name, None);
        assert_eq!(f.product_type, Some("iPhone1,1"));
        assert_eq!(f.last_backup_secs, Some(60));
    }
}
