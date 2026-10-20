//! Where an export goes (M6 spec §8.1, §8.2, D127, D143), without Tauri:
//! the native dialog is opened by `commands::exports`, which hands the
//! path it returned to [`inspect`]. The path never crosses IPC (I4); the
//! webview gets the name it will be published under, closed warning
//! codes and buckets.
//!
//! - **Canonical first.** The folder is canonicalised (`dunce`) and must
//!   be a real directory with a UTF-8 path; every check runs on that
//!   form, and the job writes below it (on Linux through handles that
//!   never follow links, `oma_export::stage`).
//! - **Refusals** (`exportDestinationRefused`): inside a backup,
//!   `MobileSync/Backup` or `iMazing/Backups` (I1); inside the archive
//!   folder (every archive is there); not a folder, not UTF-8; not
//!   writable (a `.oma-export-<32 hex>.probe` is created and removed);
//!   and, when files are copied, less free space than their bytes
//!   (`insufficientSpace`).
//! - **Warnings** ([`ExportWarningDto`]): a folder others can change
//!   (Linux: group- or other-writable without the sticky bit), a sync
//!   client's folder, a network or virtualised drive, FAT (files of
//!   4 GiB or more are left out), and "may not fit" (free space below the
//!   estimate × 1.2 + 64 MiB; an unreadable free space is "unknown",
//!   never zero). Every one but FAT must be confirmed at start.
//! - **Names** (§8.1): a bundle is named `Messages export <date> <time>`
//!   in the export's zone, then ` (2)` … ` (99)`; a single file keeps the
//!   name the user typed when it ends in the format's own extension
//!   (compared ignoring case) and gets it appended otherwise; a name
//!   `sanitize_filename` would change, or one starting with
//!   `.oma-export-`, is refused (`exportNameRefused`).
//! - **Leftovers** (§8.5): unfinished exports in the folder at least five
//!   minutes old and not locked are listed; the job removes exactly
//!   those, re-checked, when the user asks.
//!
//! [`recheck`] runs everything again at start from a fresh
//! canonicalisation: a changed folder is refused, and so is a warning
//! that needs confirming and was not confirmed.

use std::io;
use std::path::{Path, PathBuf};

use oma_export::time::{CivilTime, Localized};
use oma_export::{ExportPlan, ExportZone, Format, Leftover, Packaging, TargetName};

use super::dto::{
    DestinationToken, ExportDestinationDto, ExportWarningDto, FreeSpaceDto, PackagingDto,
    SpaceVerdictDto, SyncProviderDto,
};
use super::messages as em;
use crate::error::{ErrorCode, IpcError, IpcResult};
use crate::viewer::dto::UntrustedText;

/// 4 GiB: FAT12/16/32's file size limit is one byte less.
const FAT_LIMIT: u64 = 4 << 30;
/// The margin over the estimate (§8.2).
const SPACE_MARGIN: u64 = 64 << 20;

/// The facts of the file system the checks read; tests use a fake.
pub trait DestinationProbe: Send + Sync {
    fn volume(&self, dir: &Path) -> io::Result<oma_fs::VolumeKind>;
    fn available(&self, dir: &Path) -> io::Result<u64>;
    /// Linux: the folder is writable by group or others and has no sticky
    /// bit, on a file system with Unix permissions.
    fn shared(&self, dir: &Path) -> bool;
}

/// The real file system.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealProbe;

impl DestinationProbe for RealProbe {
    fn volume(&self, dir: &Path) -> io::Result<oma_fs::VolumeKind> {
        oma_fs::volume_kind(dir)
    }

    fn available(&self, dir: &Path) -> io::Result<u64> {
        oma_fs::available_space(dir)
    }

    #[cfg(unix)]
    fn shared(&self, dir: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        let Ok(meta) = std::fs::metadata(dir) else {
            return false;
        };
        let fat = matches!(oma_fs::volume_kind(dir), Ok(oma_fs::VolumeKind::Fat(_)));
        !fat && meta.mode() & 0o022 != 0 && meta.mode() & 0o1000 == 0
    }

    #[cfg(not(unix))]
    fn shared(&self, _dir: &Path) -> bool {
        false
    }
}

/// What the space checks need from the draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpaceNeeds {
    /// Bytes of the files the export copies (0 when it copies none).
    pub copied_bytes: u64,
    pub largest_copied: u64,
    pub estimate: u64,
}

/// A destination the user chose and the checks accepted, held in the
/// draft. `Debug` prints no path or name.
#[derive(Clone)]
pub struct Chosen {
    /// The token the webview echoes in `start_export` (§8.1).
    pub token: DestinationToken,
    /// The canonical folder the export is written into.
    pub parent: PathBuf,
    /// The name it is published under (a bundle's base name: the publish
    /// itself moves on to ` (n)` when it is taken).
    pub target: TargetName,
    pub packaging: Packaging,
    pub target_exists: bool,
    pub warnings: Vec<ExportWarningDto>,
    pub leftovers: Vec<Leftover>,
    /// "Save a copy": the name needs `keepRiskyExtension` (§6.6).
    pub risky_name: bool,
    pub dto: ExportDestinationDto,
}

impl std::fmt::Debug for Chosen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chosen")
            .field("packaging", &self.packaging)
            .field("target_exists", &self.target_exists)
            .field("warnings", &self.warnings)
            .field("leftovers", &self.leftovers.len())
            .finish_non_exhaustive()
    }
}

fn refused(sentence: &str) -> IpcError {
    IpcError::new(ErrorCode::ExportDestinationRefused, sentence)
}

/// The dialog's title (§8.1): it names every sensitive option of the
/// draft, so a compromised webview cannot hide them.
pub fn dialog_title(plan: &ExportPlan) -> String {
    let o = plan.options();
    let mut t = String::from("Save export");
    let mut extras = Vec::new();
    if o.reveal.one_time_codes {
        extras.push("includes one-time codes");
    }
    if o.reveal.earlier_content {
        extras.push("includes text captured before an unsend or edit");
    }
    if o.keep_extensions {
        extras.push("keeps risky file extensions");
    }
    if !extras.is_empty() {
        t.push_str(" — ");
        t.push_str(&extras.join("; "));
    }
    t
}

/// `Messages export YYYY-MM-DD HH-MM`, at `now_ns` in the export's zone
/// (UTC when the table does not place it).
pub fn default_name(zone: &ExportZone, now_ns: i64) -> String {
    let offset = match zone {
        ExportZone::Utc => 0,
        ExportZone::Table(t) => match t.localize(now_ns) {
            Ok(Localized::Local { offset_seconds }) => offset_seconds,
            _ => 0,
        },
    };
    let secs = now_ns.div_euclid(1_000_000_000) + i64::from(offset);
    let t = CivilTime::from_seconds(secs);
    format!(
        "Messages export {:04}-{:02}-{:02} {:02}-{:02}",
        t.date.year, t.date.month, t.date.day, t.hour, t.minute
    )
}

/// The single file's format, for the save dialog's filter.
pub fn single_format(plan: &ExportPlan) -> Option<Format> {
    match (plan.packaging(), plan.formats()) {
        (Packaging::SingleFile, [f]) => Some(*f),
        _ => None,
    }
}

/// Canonicalise `dir` and refuse what can never take an export.
fn canonical_folder(dir: &Path, library: Option<&Path>) -> IpcResult<PathBuf> {
    if dir.to_str().is_none() || !dir.is_absolute() {
        return Err(refused(em::DESTINATION_NOT_A_FOLDER));
    }
    let canonical = dunce::canonicalize(dir).map_err(|_| refused(em::DESTINATION_NOT_A_FOLDER))?;
    let meta =
        std::fs::symlink_metadata(&canonical).map_err(|_| refused(em::DESTINATION_NOT_A_FOLDER))?;
    if !meta.is_dir() || canonical.to_str().is_none() {
        return Err(refused(em::DESTINATION_NOT_A_FOLDER));
    }
    if oma_fs::enclosing_backup_dir(&canonical).is_some()
        || oma_fs::is_within_mobilesync(&canonical)
        || oma_fs::is_within_imazing_backups(&canonical)
    {
        return Err(refused(em::DESTINATION_INSIDE_BACKUP));
    }
    if let Some(lib) = library.and_then(|l| dunce::canonicalize(l).ok()) {
        if canonical.starts_with(&lib) {
            return Err(refused(em::DESTINATION_INSIDE_LIBRARY));
        }
    }
    Ok(canonical)
}

/// The checks on a canonical folder: writability, space and warnings.
fn facts(
    canonical: &Path,
    needs: SpaceNeeds,
    probe: &dyn DestinationProbe,
) -> IpcResult<(Vec<ExportWarningDto>, Option<SyncProviderDto>, Option<u64>)> {
    match oma_export::probe_writable(canonical) {
        Ok(true) => {}
        Ok(false) | Err(_) => return Err(refused(em::DESTINATION_NOT_WRITABLE)),
    }
    let mut warnings = Vec::new();
    if probe.shared(canonical) {
        warnings.push(ExportWarningDto::SharedFolder);
    }
    let provider = oma_fs::cloud_sync_provider(canonical).map(SyncProviderDto::of);
    if provider.is_some() {
        warnings.push(ExportWarningDto::SyncFolder);
    }
    match probe.volume(canonical) {
        Ok(oma_fs::VolumeKind::Network(_) | oma_fs::VolumeKind::Virtualised(_)) => {
            warnings.push(ExportWarningDto::NetworkOrVirtual);
        }
        Ok(oma_fs::VolumeKind::Fat(oma_fs::FatKind::Msdos))
            if needs.largest_copied >= FAT_LIMIT =>
        {
            warnings.push(ExportWarningDto::FatVolume);
        }
        _ => {}
    }
    let available = probe.available(canonical).ok();
    if let Some(a) = available {
        if a < needs.copied_bytes {
            return Err(IpcError::new(
                ErrorCode::InsufficientSpace,
                em::NO_SPACE_FOR_FILES,
            ));
        }
        let wanted = needs
            .estimate
            .saturating_add(needs.estimate / 5)
            .saturating_add(SPACE_MARGIN);
        if a < wanted {
            warnings.push(ExportWarningDto::MayNotFit);
        }
    }
    warnings.sort();
    Ok((warnings, provider, available))
}

/// What the dialog returned, by packaging: a folder for a bundle, a file
/// path for a single file, or a file path for "Save a copy" (whose kind
/// the name's extension is checked against).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picked {
    Folder,
    File(Format),
    Copy(Option<oma_archive::SniffedKind>),
}

/// The save dialog's title for "Save a copy" (§6.6): it names content
/// captured before an unsend, from the draft's facts.
pub fn copy_title(of_unsent_part: bool) -> String {
    if of_unsent_part {
        "Save a copy — includes content captured before an unsend".to_owned()
    } else {
        "Save a copy".to_owned()
    }
}

/// Check the location the dialog returned (`picked`) for a draft's
/// `plan`, and name the output (§8.1, §8.2). `library` is the archive
/// folder; `bundle_name` the bundle's base name.
pub fn inspect(
    picked_path: &Path,
    picked: Picked,
    bundle_name: &str,
    library: Option<&Path>,
    needs: SpaceNeeds,
    probe: &dyn DestinationProbe,
) -> IpcResult<Chosen> {
    let name_refused = |_| IpcError::new(ErrorCode::ExportNameRefused, em::NAME_REFUSED);
    let split = || match (
        picked_path.parent(),
        picked_path.file_name().and_then(|n| n.to_str()),
    ) {
        (Some(dir), Some(name)) => Ok((dir, name)),
        _ => Err(refused(em::DESTINATION_NOT_A_FOLDER)),
    };
    let mut risky_name = false;
    let (dir, target, packaging) = match picked {
        Picked::Folder => {
            let target = TargetName::new(bundle_name).map_err(name_refused)?;
            (picked_path.to_path_buf(), target, Packaging::Bundle)
        }
        Picked::File(format) => {
            let (dir, name) = split()?;
            let target = TargetName::for_single_file(name, format).map_err(name_refused)?;
            (dir.to_path_buf(), target, Packaging::SingleFile)
        }
        Picked::Copy(sniffed) => {
            // The name is kept as typed (never changed); one that could
            // run programs needs the app's own confirmation (§6.6).
            let (dir, name) = split()?;
            let target = TargetName::new(name).map_err(name_refused)?;
            risky_name = oma_export::naming::risky_copy_name(target.as_str(), sniffed);
            (dir.to_path_buf(), target, Packaging::SingleFile)
        }
    };
    let parent = canonical_folder(&dir, library)?;
    let (warnings, provider, available) = facts(&parent, needs, probe)?;
    let exists = |t: &TargetName| std::fs::symlink_metadata(parent.join(t.as_str())).is_ok();
    let (shown, target_exists) = match packaging {
        Packaging::Bundle => {
            let free = (1..=99)
                .map(|n| target.candidate(n))
                .find(|c| !exists(c))
                .ok_or_else(|| IpcError::new(ErrorCode::ExportTargetExists, em::TARGET_EXISTS))?;
            (free, false)
        }
        Packaging::SingleFile => {
            let e = exists(&target);
            if e && !std::fs::symlink_metadata(parent.join(target.as_str()))
                .is_ok_and(|m| m.is_file())
            {
                // Only a regular file may ever be replaced (§8.4).
                return Err(IpcError::new(
                    ErrorCode::ExportTargetExists,
                    em::TARGET_EXISTS,
                ));
            }
            (target.clone(), e)
        }
    };
    let leftovers = oma_export::find_leftovers(&parent, oma_export::SWEEP_MIN_AGE)
        .map(|s| s.found)
        .unwrap_or_default();
    let enough = match available {
        None => SpaceVerdictDto::Unknown,
        Some(_) if warnings.contains(&ExportWarningDto::MayNotFit) => SpaceVerdictDto::No,
        Some(_) => SpaceVerdictDto::Yes,
    };
    let token = DestinationToken::random().ok_or_else(|| IpcError::internal("token"))?;
    let dto = ExportDestinationDto {
        destination_token: token.clone(),
        packaging: PackagingDto::from(packaging),
        display_name: UntrustedText::of_name(shown.as_str()),
        target_exists,
        warnings: warnings.clone(),
        sync_provider: provider,
        free_space: available.map(FreeSpaceDto::of),
        enough,
        leftovers: u32::try_from(leftovers.len()).unwrap_or(u32::MAX),
        risky_name,
    };
    Ok(Chosen {
        token,
        parent,
        target,
        packaging,
        target_exists,
        warnings,
        leftovers,
        risky_name,
        dto,
    })
}

/// The checks again at start (§8.2): from a fresh canonicalisation, which
/// must give the same folder, and with every warning that needs a
/// confirmation among `confirmed`.
pub fn recheck(
    chosen: &Chosen,
    library: Option<&Path>,
    needs: SpaceNeeds,
    confirmed: &[ExportWarningDto],
    probe: &dyn DestinationProbe,
) -> IpcResult<()> {
    let again = canonical_folder(&chosen.parent, library)?;
    if again != chosen.parent {
        return Err(refused(em::DESTINATION_CHANGED));
    }
    let (warnings, _, _) = facts(&again, needs, probe)?;
    let missing = warnings
        .iter()
        .any(|w| w.needs_confirmation() && !confirmed.contains(w));
    if missing {
        return Err(IpcError::new(
            ErrorCode::ExportConfirmationStale,
            em::NEW_WARNING,
        ));
    }
    Ok(())
}
