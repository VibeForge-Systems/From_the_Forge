//! Building the backup list and the details panel from `oma_backup`
//! inspections. Pure functions over the filesystem; no Tauri types.
//!
//! The listing budget is deterministic (§11.3, §24.1): at most
//! `list_backups_total` candidates are inspected and at most
//! `list_plist_bytes_total` plist bytes read per refresh, and each root
//! yields at most `backups_per_root` candidates. Candidates beyond the budget
//! are listed as `notInspected`, never dropped.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use oma_backup::{
    inspect, library_relation, scan_root, BackupError, BackupFile, BackupId, Candidate,
    DefaultRoot, DisplayText, Inspection, LibraryRelation, Limits, LocationId, NoteKind, ParseNote,
    RefusalReason, RootKind, RootScan, RootScanState, Sourced,
};

use super::dto::{
    BackupDetails, BackupFormat, BackupList, BackupLocation, BackupLocationKind,
    BackupLocationState, BackupState, BackupSummary, DeviceDetails, EncryptionDetails,
    LayoutDetails, SourceFile, SourcedText, StatusDetails,
};
use super::messages;
use super::{BackupEntry, BackupRegistry, Jobs, LocationEntry};
use crate::settings::{AddedFolderEntry, AddedFolderKind};

/// Everything a listing needs, cloned out of the app state so no lock is
/// held while the disk is read.
#[derive(Debug, Clone)]
pub struct ListInputs {
    pub default_roots: Vec<DefaultRoot>,
    pub added: Vec<AddedFolderEntry>,
    pub library: Option<PathBuf>,
    pub limits: Limits,
}

fn location_kind(root: RootKind) -> BackupLocationKind {
    match root {
        RootKind::AppleDevicesOrStoreITunes => BackupLocationKind::AppleDevices,
        RootKind::DesktopITunes => BackupLocationKind::ItunesDesktop,
        RootKind::IMazing => BackupLocationKind::IMazing,
        RootKind::Added => BackupLocationKind::AddedParent,
    }
}

/// The location id: a hash of the display-canonical path when it resolves,
/// otherwise of the path as configured (an absent default root, or an added
/// folder on an unplugged drive), so the id stays stable.
pub fn location_id_for(path: &Path) -> Option<LocationId> {
    match dunce::canonicalize(path) {
        Ok(c) => LocationId::for_path(&c),
        Err(_) => LocationId::for_path(path),
    }
}

fn scan_state(s: RootScanState) -> BackupLocationState {
    match s {
        RootScanState::Present => BackupLocationState::Present,
        RootScanState::Absent => BackupLocationState::Absent,
        RootScanState::Unreadable(_) => BackupLocationState::Unreadable,
        RootScanState::TooManyEntries => BackupLocationState::TooManyEntries,
    }
}

fn scan_message(scan: &RootScan, limits: &Limits) -> Option<String> {
    let mut parts = Vec::new();
    match scan.state {
        RootScanState::Unreadable(kind) => {
            parts.push(format!("This folder could not be read ({kind})."))
        }
        RootScanState::TooManyEntries => parts.push(format!(
            "This folder has more than {} entries; only the first {} were read.",
            messages::thousands(u64::from(limits.root_entries)),
            messages::thousands(u64::from(limits.root_entries))
        )),
        RootScanState::Present | RootScanState::Absent => {}
    }
    if scan.over_cap > 0 {
        parts.push(format!(
            "{} more backup{} here {} not listed: at most {} are listed per folder.",
            messages::thousands(u64::from(scan.over_cap)),
            if scan.over_cap == 1 { "" } else { "s" },
            if scan.over_cap == 1 { "was" } else { "were" },
            messages::thousands(u64::from(limits.backups_per_root))
        ));
    }
    if scan.skipped_non_unicode > 0 {
        parts.push(format!(
            "{} folder{} with names that are not valid Unicode {} skipped.",
            messages::thousands(u64::from(scan.skipped_non_unicode)),
            if scan.skipped_non_unicode == 1 {
                ""
            } else {
                "s"
            },
            if scan.skipped_non_unicode == 1 {
                "was"
            } else {
                "were"
            }
        ));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// Scan every location and inspect candidates within the budget.
pub fn build_registry(inputs: &ListInputs) -> BackupRegistry {
    let limits = &inputs.limits;
    let mut reg = BackupRegistry::default();
    let mut seen_locations = HashSet::new();
    let mut seen_backups = HashSet::new();
    let mut inspected: u32 = 0;
    let mut bytes_read: u64 = 0;

    let mut pending: Vec<(String, Candidate)> = Vec::new();

    let mut roots: Vec<(BackupLocationKind, PathBuf, bool)> = inputs
        .default_roots
        .iter()
        .map(|r| (location_kind(r.kind), r.path.clone(), false))
        .collect();
    for entry in &inputs.added {
        let kind = match entry.kind {
            AddedFolderKind::Parent => BackupLocationKind::AddedParent,
            AddedFolderKind::Backup => BackupLocationKind::AddedBackup,
        };
        roots.push((kind, PathBuf::from(&entry.path), true));
    }

    for (kind, path, added) in roots {
        let Some(display) = path.to_str().map(str::to_owned) else {
            continue;
        };
        let Some(id) = location_id_for(&path) else {
            continue;
        };
        if !seen_locations.insert(id.clone()) {
            continue;
        }
        let mut dto = BackupLocation {
            id: id.as_str().to_owned(),
            kind,
            path: display,
            via_link: false,
            state: BackupLocationState::Present,
            backup_count: 0,
            message: None,
        };
        let candidates = if kind == BackupLocationKind::AddedBackup {
            match Candidate::for_dir(&path) {
                Ok(c) => {
                    dto.via_link = c.via_link;
                    vec![c]
                }
                Err(RefusalReason::NotFound) => {
                    dto.state = BackupLocationState::Absent;
                    dto.message = Some(
                        "This folder could not be found. If it is on a removable drive, reconnect it and refresh."
                            .into(),
                    );
                    Vec::new()
                }
                Err(r) => {
                    dto.state = BackupLocationState::Unreadable;
                    dto.message = Some(messages::refusal(r).to_owned());
                    Vec::new()
                }
            }
        } else {
            let scan = scan_root(&path, limits);
            dto.via_link = scan.via_link;
            dto.state = scan_state(scan.state);
            dto.message = scan_message(&scan, limits);
            if added && scan.state == RootScanState::Absent {
                dto.message = Some(
                    "This folder could not be found. If it is on a removable drive, reconnect it and refresh."
                        .into(),
                );
            }
            scan.candidates
        };
        for c in candidates {
            pending.push((dto.id.clone(), c));
        }
        reg.locations.push(LocationEntry { dto, added });
    }

    for (location_id, candidate) in pending {
        let Some(id) = BackupId::for_path(&candidate.display_canonical) else {
            continue;
        };
        if !seen_backups.insert(id.clone()) {
            continue;
        }
        let within_budget =
            inspected < limits.list_backups_total && bytes_read < limits.list_plist_bytes_total;
        let summary = if within_budget {
            inspected = inspected.saturating_add(1);
            match inspect(&candidate, limits, &mut bytes_read) {
                Ok(insp) => summarize(&insp, &location_id, inputs.library.as_deref(), limits),
                Err(e) => summary_for_error(&id, &location_id, &candidate, &e, limits),
            }
        } else {
            reg.budget_exhausted = true;
            not_inspected(&id, &location_id, &candidate)
        };
        reg.order.push(id.as_str().to_owned());
        reg.backups
            .insert(id.as_str().to_owned(), BackupEntry { candidate, summary });
    }

    for loc in &mut reg.locations {
        loc.dto.backup_count = reg
            .backups
            .values()
            .filter(|b| b.summary.location_id == loc.dto.id)
            .count() as u64;
    }
    if reg.budget_exhausted {
        tracing::info!(inspected, "backup listing budget exhausted");
    }
    tracing::info!(
        locations = reg.locations.len(),
        backups = reg.backups.len(),
        "backups listed"
    );
    reg
}

fn sourced(s: Option<Sourced<DisplayText>>) -> Option<SourcedText> {
    s.map(|s| SourcedText {
        value: s.value.as_str().to_owned(),
        source: match s.source {
            BackupFile::InfoPlist => SourceFile::InfoPlist,
            BackupFile::StatusPlist => SourceFile::StatusPlist,
            _ => SourceFile::ManifestPlistLockdown,
        },
    })
}

fn from_info(v: Option<&DisplayText>) -> Option<SourcedText> {
    v.map(|t| SourcedText {
        value: t.as_str().to_owned(),
        source: SourceFile::InfoPlist,
    })
}

fn date_string<D>(d: Option<D>) -> Option<String>
where
    SystemTime: From<D>,
{
    d.map(|d| super::dto::rfc3339(SystemTime::from(d)))
}

/// The summary for an inspected backup. The library position is re-checked
/// here on every listing (§3.3).
pub fn summarize(
    insp: &Inspection,
    location_id: &str,
    library: Option<&Path>,
    limits: &Limits,
) -> BackupSummary {
    let status = insp.status.as_ref();
    let (mut state, mut reason) = messages::state_of(
        &insp.classification,
        insp.layout.has_status_plist,
        status
            .and_then(|s| s.snapshot_state_raw.as_ref())
            .map(DisplayText::as_str),
        limits,
    );
    if let Some(lib) = library {
        if library_relation(&insp.root, lib) == LibraryRelation::LibraryInsideBackup {
            state = BackupState::Unreadable;
            reason = messages::LIBRARY_INSIDE_BACKUP.to_owned();
        }
    }
    let info = insp.info.as_ref();
    BackupSummary {
        id: insp.id.as_str().to_owned(),
        location_id: location_id.to_owned(),
        via_link: insp.root.via_link,
        device_name: sourced(insp.device_name()),
        product_type: sourced(insp.product_type()),
        product_name: from_info(info.and_then(|i| i.product_name.as_ref())),
        ios_version: sourced(insp.ios_version()),
        last_backup_date: date_string(info.and_then(|i| i.last_backup_date)),
        encrypted: insp.is_encrypted(),
        state,
        state_reason: reason,
        active_job_id: None,
        last_report_importable: None,
    }
}

fn blank_summary(id: &BackupId, location_id: &str, c: &Candidate) -> BackupSummary {
    BackupSummary {
        id: id.as_str().to_owned(),
        location_id: location_id.to_owned(),
        via_link: c.via_link,
        device_name: None,
        product_type: None,
        product_name: None,
        ios_version: None,
        last_backup_date: None,
        encrypted: None,
        state: BackupState::NotInspected,
        state_reason: messages::NOT_INSPECTED.to_owned(),
        active_job_id: None,
        last_report_importable: None,
    }
}

pub fn not_inspected(id: &BackupId, location_id: &str, c: &Candidate) -> BackupSummary {
    blank_summary(id, location_id, c)
}

/// `inspect` returns an error only when the folder cannot be read at all.
pub fn summary_for_error(
    id: &BackupId,
    location_id: &str,
    c: &Candidate,
    e: &BackupError,
    limits: &Limits,
) -> BackupSummary {
    let mut s = blank_summary(id, location_id, c);
    s.state = BackupState::Unreadable;
    s.state_reason = messages::to_ipc(e, limits).message;
    s
}

/// Notes that depend on the archive library, added to the crate's notes.
fn location_notes(insp: &Inspection, library: Option<&Path>) -> Vec<ParseNote> {
    let mut notes = insp.notes.clone();
    if let Some(lib) = library {
        let kind = match library_relation(&insp.root, lib) {
            LibraryRelation::BackupInsideLibrary => Some(NoteKind::InsideArchiveLibrary),
            LibraryRelation::LibraryInsideBackup => Some(NoteKind::ArchiveLibraryInsideBackup),
            LibraryRelation::Separate => None,
        };
        if let Some(kind) = kind {
            notes.push(ParseNote {
                file: BackupFile::ManifestPlist,
                key: "",
                kind,
            });
        }
    }
    notes
}

/// Everything about the archive location the details panel needs.
#[derive(Debug, Clone, Default)]
pub struct ArchiveFacts {
    pub library: Option<PathBuf>,
    pub configured: bool,
    pub sync_provider: Option<String>,
}

/// The details panel for an inspected backup.
pub fn details(
    insp: &Inspection,
    location_id: &str,
    archive: &ArchiveFacts,
    jobs: &Jobs,
    limits: &Limits,
) -> BackupDetails {
    let summary = jobs.decorate(summarize(
        insp,
        location_id,
        archive.library.as_deref(),
        limits,
    ));
    let info = insp.info.as_ref();
    let manifest = insp.manifest.as_ref();
    let status = insp.status.as_ref();
    let build = from_info(info.and_then(|i| i.build_version.as_ref())).or_else(|| {
        manifest
            .and_then(|m| m.lockdown_build_version.as_ref())
            .map(|t| SourcedText {
                value: t.as_str().to_owned(),
                source: SourceFile::ManifestPlistLockdown,
            })
    });
    let format = if insp.layout.has_manifest_db {
        BackupFormat::V3
    } else if insp.layout.has_manifest_mbdb {
        BackupFormat::LegacyMbdb
    } else {
        BackupFormat::Unknown
    };
    let last = jobs.last(insp.id.as_str());
    BackupDetails {
        path: insp.root.display_canonical.to_string_lossy().into_owned(),
        device: DeviceDetails {
            device_name: summary.device_name.clone(),
            display_name: from_info(info.and_then(|i| i.display_name.as_ref())),
            product_type: summary.product_type.clone(),
            product_name: summary.product_name.clone(),
            ios_version: summary.ios_version.clone(),
            build_version: build,
        },
        status: StatusDetails {
            snapshot_state: status
                .and_then(|s| s.snapshot_state_raw.as_ref())
                .map(|t| t.as_str().to_owned()),
            is_full_backup: status.and_then(|s| s.is_full_backup),
            format_version: status
                .and_then(|s| s.version.as_ref())
                .map(|t| t.as_str().to_owned()),
            date: date_string(status.and_then(|s| s.date)),
            backup_state: status
                .and_then(|s| s.backup_state.as_ref())
                .map(|t| t.as_str().to_owned()),
            snapshot_folder_present: insp.layout.has_snapshot_dir,
        },
        encryption: EncryptionDetails {
            is_encrypted: manifest.and_then(|m| m.is_encrypted),
            has_keybag: manifest.is_some_and(|m| m.has_backup_key_bag),
            has_manifest_key: manifest.is_some_and(|m| m.has_manifest_key),
        },
        layout: LayoutDetails {
            format,
            manifest_db_wal_present: insp.layout.has_manifest_db_wal,
            manifest_db_shm_present: insp.layout.has_manifest_db_shm,
            non_regular_markers: u64::from(insp.layout.non_regular_markers),
        },
        notes: location_notes(insp, archive.library.as_deref())
            .iter()
            .map(|n| messages::note(n, limits))
            .collect(),
        archive_sync_provider: archive.sync_provider.clone(),
        archive_configured: archive.configured,
        last_report: last.report,
        last_extraction_report: last.extraction,
        ios_version_untested: super::extraction::ios_version_untested(
            summary.ios_version.as_ref().map(|v| v.value.as_str()),
        ),
        last_cleanup: last.cleanup,
        active_job: jobs.active_for(insp.id.as_str()),
        summary,
    }
}

/// The list for a registry, decorated with job state.
pub fn list(reg: &BackupRegistry, jobs: &Jobs) -> BackupList {
    reg.list(jobs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tests/fixtures/backups"
        ))
    }

    fn copy_tree(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).expect("mkdir");
        for e in std::fs::read_dir(src).expect("read_dir") {
            let e = e.expect("entry");
            let to = dst.join(e.file_name());
            if e.file_type().expect("type").is_dir() {
                copy_tree(&e.path(), &to);
            } else {
                std::fs::copy(e.path(), &to).expect("copy");
            }
        }
    }

    /// A parent folder holding copies of some fixtures, each in its own
    /// subfolder (the fixture's UDID folder renamed so they can coexist).
    fn parent_with(names: &[&str]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        for (i, name) in names.iter().enumerate() {
            copy_tree(
                &fixtures().join(name).join("SYNTHETIC0-0000000000000000"),
                &tmp.path().join("parent").join(format!("backup-{i}")),
            );
        }
        tmp
    }

    fn inputs(parent: &Path, limits: Limits) -> ListInputs {
        ListInputs {
            default_roots: Vec::new(),
            added: vec![AddedFolderEntry {
                path: parent.to_string_lossy().into_owned(),
                kind: AddedFolderKind::Parent,
            }],
            library: None,
            limits,
        }
    }

    #[test]
    fn lists_states_for_a_parent_folder() {
        let tmp = parent_with(&["plain", "enc", "incomplete", "legacy-mbdb"]);
        let reg = build_registry(&inputs(&tmp.path().join("parent"), Limits::default()));
        let list = reg.list(&Jobs::default());
        assert_eq!(list.locations.len(), 1);
        assert_eq!(list.locations[0].backup_count, 4);
        assert_eq!(list.locations[0].kind, BackupLocationKind::AddedParent);
        let mut states: Vec<BackupState> = list.backups.iter().map(|b| b.state).collect();
        states.sort_by_key(|s| format!("{s:?}"));
        assert_eq!(
            states,
            vec![
                BackupState::Incomplete,
                BackupState::NeedsPassword,
                BackupState::Ready,
                BackupState::Unsupported
            ]
        );
        let ready = list
            .backups
            .iter()
            .find(|b| b.state == BackupState::Ready)
            .expect("ready");
        let name = ready.device_name.as_ref().expect("device name");
        assert_eq!(name.value, "Synthetic Test Phone");
        assert_eq!(name.source, SourceFile::InfoPlist);
        assert!(ready
            .last_backup_date
            .as_deref()
            .is_some_and(|d| d.ends_with('Z')));
        assert!(!list.budget_exhausted);
    }

    #[test]
    fn root_kinds_map_to_location_kinds() {
        assert_eq!(
            location_kind(RootKind::AppleDevicesOrStoreITunes),
            BackupLocationKind::AppleDevices
        );
        assert_eq!(
            location_kind(RootKind::DesktopITunes),
            BackupLocationKind::ItunesDesktop
        );
        assert_eq!(
            location_kind(RootKind::IMazing),
            BackupLocationKind::IMazing
        );
        assert_eq!(
            location_kind(RootKind::Added),
            BackupLocationKind::AddedParent
        );
    }

    #[test]
    fn the_imazing_default_root_is_listed_without_its_versions_folder() {
        let tmp = parent_with(&["plain"]);
        let store = tmp.path().join("parent");
        // A copy of a backup inside iMazing.Versions must not be listed.
        copy_tree(
            &fixtures().join("plain").join("SYNTHETIC0-0000000000000000"),
            &store.join(oma_backup::IMAZING_VERSIONS_DIR_NAME),
        );
        let reg = build_registry(&ListInputs {
            default_roots: vec![DefaultRoot {
                kind: RootKind::IMazing,
                path: store.clone(),
            }],
            added: Vec::new(),
            library: None,
            limits: Limits::default(),
        });
        let list = reg.list(&Jobs::default());
        assert_eq!(list.locations.len(), 1);
        assert_eq!(list.locations[0].kind, BackupLocationKind::IMazing);
        assert_eq!(list.locations[0].backup_count, 1);
        assert_eq!(list.backups.len(), 1);
        assert_eq!(list.backups[0].state, BackupState::Ready);
    }

    #[test]
    fn candidates_beyond_the_budget_are_not_inspected() {
        let tmp = parent_with(&["plain", "plain-xml", "enc"]);
        let limits = Limits {
            list_backups_total: 1,
            ..Limits::default()
        };
        let reg = build_registry(&inputs(&tmp.path().join("parent"), limits));
        let list = reg.list(&Jobs::default());
        assert!(list.budget_exhausted);
        assert_eq!(list.backups.len(), 3);
        let not = list
            .backups
            .iter()
            .filter(|b| b.state == BackupState::NotInspected)
            .count();
        assert_eq!(not, 2);
        for b in &list.backups {
            if b.state == BackupState::NotInspected {
                assert_eq!(b.state_reason, messages::NOT_INSPECTED);
                assert!(b.device_name.is_none());
            }
        }
    }

    #[test]
    fn an_absent_added_folder_stays_listed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let gone = tmp.path().join("unplugged");
        let reg = build_registry(&inputs(&gone, Limits::default()));
        let list = reg.list(&Jobs::default());
        assert_eq!(list.locations.len(), 1);
        assert_eq!(list.locations[0].state, BackupLocationState::Absent);
        assert!(list.backups.is_empty());
    }

    #[test]
    fn the_same_backup_through_two_locations_is_listed_once() {
        let tmp = parent_with(&["plain"]);
        let parent = tmp.path().join("parent");
        let mut i = inputs(&parent, Limits::default());
        i.added.push(AddedFolderEntry {
            path: parent.join("backup-0").to_string_lossy().into_owned(),
            kind: AddedFolderKind::Backup,
        });
        let list = build_registry(&i).list(&Jobs::default());
        assert_eq!(list.locations.len(), 2);
        assert_eq!(list.backups.len(), 1);
    }

    #[test]
    fn a_library_inside_a_backup_marks_it_unreadable() {
        let tmp = parent_with(&["plain"]);
        let parent = tmp.path().join("parent");
        let library = parent.join("backup-0").join("lib");
        std::fs::create_dir(&library).expect("mkdir");
        let mut i = inputs(&parent, Limits::default());
        i.library = Some(library.clone());
        let reg = build_registry(&i);
        let list = reg.list(&Jobs::default());
        assert_eq!(list.backups[0].state, BackupState::Unreadable);
        assert_eq!(
            list.backups[0].state_reason,
            messages::LIBRARY_INSIDE_BACKUP
        );

        let entry = reg.backups.values().next().expect("entry");
        let insp = inspect(&entry.candidate, &Limits::default(), &mut 0).expect("inspect");
        let d = details(
            &insp,
            &entry.summary.location_id,
            &ArchiveFacts {
                library: Some(library),
                configured: true,
                sync_provider: None,
            },
            &Jobs::default(),
            &Limits::default(),
        );
        assert!(d.notes.iter().any(|n| n.id == "library-inside-backup"));
    }

    #[test]
    fn details_carry_the_path_and_never_an_identifier_field() {
        let tmp = parent_with(&["plain"]);
        let reg = build_registry(&inputs(&tmp.path().join("parent"), Limits::default()));
        let entry = reg.backups.values().next().expect("entry");
        let insp = inspect(&entry.candidate, &Limits::default(), &mut 0).expect("inspect");
        let d = details(
            &insp,
            &entry.summary.location_id,
            &ArchiveFacts::default(),
            &Jobs::default(),
            &Limits::default(),
        );
        assert!(d.path.ends_with("backup-0"));
        assert_eq!(d.layout.format, BackupFormat::V3);
        assert_eq!(d.encryption.is_encrypted, Some(false));
        assert_eq!(d.status.snapshot_state.as_deref(), Some("finished"));
        assert!(!d.archive_configured);
        let json = serde_json::to_string(&d.summary).expect("json");
        assert!(!json.contains(&d.path));
    }
}
