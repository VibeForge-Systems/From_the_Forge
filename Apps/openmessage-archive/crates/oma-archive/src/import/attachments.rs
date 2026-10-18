//! P6, attachments (spec §8.4, §9.1): the attachment **rows**, keyed on the
//! exact GUID bytes, merged by the column rules of §8.7, mapped for P7 and
//! recorded in `lineage_seen`; and their **files**, copied into the
//! content-addressed store in four phases.
//!
//! - **Phase A, while the pass reads, with no archive transaction open.**
//!   For each record: decide whether to copy (copying is on, the
//!   attachment is not Recently Deleted-only, the Manifest lookup found the
//!   file and its blob is present, and the archive does not already hold
//!   the current blob's file with that length); check the space for it;
//!   create `attachments/.incoming/<n>.partial`; stream the plaintext into
//!   it through `OpenBackup::copy_file_to` (hashed as it streams, decrypted
//!   for an encrypted backup); `sync_all`; check the length. A failed copy
//!   removes its partial file and is counted; a changed source, a cancel or
//!   too little space ends the batch at the record before.
//! - **Phase B, the intents.** One transaction records a `blob_intent` for
//!   each copied digest that has no `blob` row, and commits.
//! - **Phase C, placement** ([`crate::blobs::Placer`]): no-replace renames,
//!   deduplication, adoption of matching orphans, replacement of the others;
//!   then one flush per touched `<hh>` directory.
//! - **Phase D, the rows**, in the batch transaction: the attachment merges
//!   and maps, `blob` rows (`ON CONFLICT DO NOTHING`), one
//!   `attachment_observation` per row and session, `current_blob_id` by
//!   rank, the deletion of the batch's intents, and the cursor and counts.
//!
//! So the order for every file is: write, `sync_all`, intent commit,
//! rename, directory flush, row commit. A crash before the row commit
//! leaves placed files with intents, which the next write open removes
//! (`blobs::reconcile_intents`).
//!
//! **`current_blob_id` follows rank:** the blob of the highest-ranked
//! session that read bytes for the attachment. A session that read none
//! (the file was already archived, or missing from its backup) never moves
//! it, so the result does not depend on import order.
//!
//! **"Already archived"** compares presence and length only, never the
//! bytes (spec §9.2): the current blob's store file is a regular file of
//! `byte_len`, and the backup's file has that plaintext length. For an
//! encrypted backup the plaintext length is the Manifest `Size`, which the
//! probe checked against the padded blob; without a usable `Size` the file
//! is copied again. The lookup is by exact GUID: an unkeyed attachment is
//! always copied again (the store deduplicates it).
//!
//! An attachment linked only to Recently Deleted messages that the policy
//! left out (P5b) gets no row, no lookup and no file, and is counted
//! (§8.8). It is skipped before it is identified, so it takes no
//! occurrence number.
//!
//! Nothing here logs a name, a path, a digest or a byte of content.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufWriter, Write};
use std::ops::ControlFlow;
use std::sync::Arc;

use oma_backup::{
    AttachmentKey, BackupError, BlobProbe, CancelToken, CopiedFile, IoSide, JobCtx, LimitKind,
    OpenBackup, Progress,
};
use oma_messages::{
    classify_attachment_path, AttachmentLocator, AttachmentRecord, Col, Cursor, KnownTable,
    LookupOutcome, MessagesCtx, MessagesError, NotAttemptedReason, PassOutcome, PathClass, SmsDb,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use super::batch::{Brk, IssueDeltas, PassError, PassWriter, Run};
use super::bind::Cell;
use super::counts::{AttachmentFileCounts, ImportCounts};
use super::entity::{keep_raw, text_len, write_records, EntityKind, EntityRecord, KeyStyle};
use super::merge::TableDef;
use super::preflight::guid_digest;
use super::rd::RdSkip;
use super::StopReason;
use crate::blobs::{store_file_state, Partial, Placement, Placer, StorageName, StoreFileState};
use crate::fault::{point, FaultPoint};
use crate::layout::ArchiveDir;
use crate::progress::ImportPhase;
use crate::schema::codes::{CopyCode, Entity, LookupCode, Pass, PathClassCode};
use crate::store_fs::StoreFs;

/// A batch holds at most this many copied files (spec §8.5).
pub(crate) const BATCH_FILES: usize = 64;
/// A batch holds at most this many copied bytes.
pub(crate) const BATCH_FILE_BYTES: u64 = 256 << 20;
/// A file this large always ends its batch.
pub(crate) const LARGE_FILE: u64 = 64 << 20;
/// The copy buffer (spec §8.13).
const COPY_BUFFER: usize = 1 << 20;

/// What the store side of P6 needs.
pub(crate) struct StoreCtx<'a> {
    pub(crate) ob: &'a OpenBackup,
    pub(crate) dir: ArchiveDir,
    pub(crate) store: Arc<dyn StoreFs>,
    pub(crate) cancel: &'a CancelToken,
    /// `copy_attachments`.
    pub(crate) copy: bool,
}

/// The closed path class code of a reader class.
fn path_class_code(c: Option<PathClass>) -> PathClassCode {
    match c {
        None => PathClassCode::NoClass,
        Some(PathClass::SmsAttachments) => PathClassCode::SmsAttachments,
        Some(PathClass::SmsStickerCache) => PathClassCode::SmsStickerCache,
        Some(PathClass::MacMessages) => PathClassCode::MacMessages,
        Some(PathClass::OtherHomeRelative) => PathClassCode::OtherHomeRelative,
        Some(PathClass::AbsoluteVarMobile) => PathClassCode::AbsoluteVarMobile,
        Some(PathClass::AbsolutePrivateVarMobile) => PathClassCode::AbsolutePrivateVarMobile,
        Some(PathClass::Unrecognized) => PathClassCode::Unrecognized,
        Some(_) => PathClassCode::Other,
    }
}

/// The closed lookup code of a probe.
fn probe_code(p: Option<&BlobProbe>) -> LookupCode {
    match p {
        None => LookupCode::NotProbed,
        Some(BlobProbe::Present { .. }) => LookupCode::Present,
        Some(BlobProbe::Missing) => LookupCode::BlobMissing,
        Some(BlobProbe::NotRegular) => LookupCode::BlobNotRegular,
        Some(BlobProbe::Unreadable(_)) => LookupCode::BlobUnreadable,
        Some(BlobProbe::NotAFile) => LookupCode::NotAFile,
        Some(BlobProbe::InvalidEntry) => LookupCode::EntryInvalid,
        Some(BlobProbe::NoKey) => LookupCode::NoKey,
        Some(BlobProbe::NoProtectionClass) => LookupCode::NoProtectionClass,
        Some(BlobProbe::ClassKeyUnavailable) => LookupCode::ClassKeyUnavailable,
        Some(BlobProbe::KeyDoesNotUnwrap) => LookupCode::KeyDoesNotUnwrap,
        Some(BlobProbe::SizeMismatch { .. }) => LookupCode::SizeMismatch,
        Some(_) => LookupCode::ProbeOther,
    }
}

/// The closed lookup code of a lookup that found no file.
fn lookup_code(l: &LookupOutcome) -> LookupCode {
    match l {
        LookupOutcome::NotAttempted(r) => match r {
            NotAttemptedReason::Null => LookupCode::NotAttemptedNull,
            NotAttemptedReason::TooLong => LookupCode::NotAttemptedTooLong,
            NotAttemptedReason::UnusualName => LookupCode::NotAttemptedUnusualName,
            NotAttemptedReason::ClassNotLookedUp => LookupCode::NotAttemptedClass,
            NotAttemptedReason::Unrecognized => LookupCode::NotAttemptedUnrecognized,
            NotAttemptedReason::ColumnAbsent => LookupCode::NotAttemptedColumnAbsent,
            NotAttemptedReason::Unreadable => LookupCode::NotAttemptedUnreadable,
            _ => LookupCode::NotAttemptedOther,
        },
        LookupOutcome::Found { blob, .. } => probe_code(blob.as_ref()),
        LookupOutcome::NotListedAtSourcedKey => LookupCode::NotListedAtSourcedKey,
        LookupOutcome::NotFoundAtAssumedKey { .. } => LookupCode::NotFoundAtAssumedKey,
        LookupOutcome::Ambiguous => LookupCode::Ambiguous,
        LookupOutcome::InvalidEntry => LookupCode::InvalidEntry,
        _ => LookupCode::LookupOther,
    }
}

/// A file the lookup found and whose blob can be copied: its key, the
/// stored blob's length, and the plaintext length when it is known.
pub(crate) struct Copyable<'r> {
    pub(crate) key: &'r AttachmentKey,
    /// The blob's length on disk (for an encrypted backup, the ciphertext:
    /// an upper bound of the plaintext).
    pub(crate) stored_len: u64,
    /// The plaintext length: the blob's length for an unencrypted backup;
    /// the Manifest `Size` (checked against the padded blob by the probe)
    /// for an encrypted one; `None` when neither is usable.
    pub(crate) plain_len: Option<u64>,
}

/// Why a located record is not copyable: the codes to record.
pub(crate) type NotCopyable = (LookupCode, CopyCode);

/// Classify one record's location (spec §9.1 phase A step 1, without the
/// archive's side). The Manifest `Size` mismatch is copyable: its bytes are
/// stored and the mismatch recorded (spec §9.2).
pub(crate) fn copyable(
    rec: &AttachmentRecord,
    encrypted: bool,
) -> Result<(LookupCode, Copyable<'_>), NotCopyable> {
    let Some(loc) = rec.location.as_ref() else {
        return Err((LookupCode::NotLookedUp, CopyCode::NotLookedUp));
    };
    let code = lookup_code(&loc.lookup);
    let LookupOutcome::Found { key, blob } = &loc.lookup else {
        let copy = match &loc.lookup {
            LookupOutcome::NotAttempted(_) => CopyCode::NotLookedUp,
            LookupOutcome::NotListedAtSourcedKey | LookupOutcome::NotFoundAtAssumedKey { .. } => {
                CopyCode::NotInBackup
            }
            _ => CopyCode::BlobUnavailable,
        };
        return Err((code, copy));
    };
    let (stored_len, plain_len) = match blob {
        Some(BlobProbe::Present { len }) if !encrypted => (*len, Some(*len)),
        Some(BlobProbe::Present { len }) => {
            let plain = match key.declared_size() {
                Some(d) => Some(d),
                None if *len == 0 => Some(0),
                None => None,
            };
            (*len, plain)
        }
        Some(BlobProbe::SizeMismatch { len }) => (*len, (!encrypted).then_some(*len)),
        _ => return Err((code, CopyCode::BlobUnavailable)),
    };
    Ok((
        code,
        Copyable {
            key,
            stored_len,
            plain_len,
        },
    ))
}

/// The exact GUID key of a record, when it identifies the row: a usable
/// value that is not duplicated in the source.
pub(crate) fn exact_key(
    rec: &AttachmentRecord,
    dups: &super::preflight::DupSet,
) -> Option<Vec<u8>> {
    guid_digest(&Cell::text(&rec.guid))
        .filter(|(d, _)| !dups.contains(Entity::Attachment, d))
        .map(|(_, k)| k)
}

/// The SQL that reads a keyed attachment's current blob.
pub(crate) const CURRENT_BLOB_SQL: &str = "SELECT b.sha256, b.byte_len FROM attachment a \
     JOIN blob b ON b.id = a.current_blob_id WHERE a.key_kind = 1 AND a.guid = ?1";

/// What the attachment estimate found (spec §8.2 step 5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AttachmentEstimate {
    pub(crate) bytes: u64,
    pub(crate) files: u64,
    pub(crate) largest: u64,
}

/// The attachment estimate: every attachment looked up and probed, and the
/// bytes still to copy added up. A file is left out when its attachment is
/// Recently Deleted-only (`rd_only`, sorted), or when the archive's current
/// blob for its exact key (`stored_len`) has its plaintext length. For an
/// encrypted file without a usable `Size`, the stored length is counted
/// (an upper bound). Reads only.
pub(crate) fn estimate(
    db: &SmsDb<'_>,
    ob: &OpenBackup,
    dups: &super::preflight::DupSet,
    rd_only: &[i64],
    ctx: &mut MessagesCtx<'_>,
    stored_len: &mut dyn FnMut(&[u8]) -> rusqlite::Result<Option<u64>>,
) -> Result<Result<AttachmentEstimate, rusqlite::Error>, MessagesError> {
    let encrypted = ob.is_encrypted();
    let mut locator = AttachmentLocator::new(ob, true);
    let mut out = AttachmentEstimate::default();
    let mut failed: Option<rusqlite::Error> = None;
    db.for_each_attachment(Cursor::Start, Some(&mut locator), ctx, |rec| {
        if rd_only.binary_search(&rec.rowid).is_ok() {
            return ControlFlow::Continue(());
        }
        let Ok((_, c)) = copyable(&rec, encrypted) else {
            return ControlFlow::Continue(());
        };
        if let Some(k) = exact_key(&rec, dups) {
            match stored_len(&k) {
                Ok(Some(len)) if c.plain_len == Some(len) => return ControlFlow::Continue(()),
                Ok(_) => {}
                Err(e) => {
                    failed = Some(e);
                    return ControlFlow::Break(());
                }
            }
        }
        let len = c.plain_len.unwrap_or(c.stored_len);
        out.bytes = out.bytes.saturating_add(len);
        out.files = out.files.saturating_add(1);
        out.largest = out.largest.max(len);
        ControlFlow::Continue(())
    })?;
    Ok(match failed {
        Some(e) => Err(e),
        None => Ok(out),
    })
}

/// A copy that reached its partial file.
struct Copied {
    sha: [u8; 32],
    len: u64,
    size_matches: Option<bool>,
    partial: Option<Partial>,
    /// The attachment's current blob before this copy, if its store file
    /// was missing or unusable (a copy with that digest places it again).
    replacing: Option<[u8; 32]>,
    /// The bytes are in the store under a `blob` row after phase C.
    stored: bool,
}

/// What P6 observed for one record.
struct Obs {
    path_class: PathClassCode,
    lookup: LookupCode,
    copy: CopyCode,
    file: Option<Copied>,
}

/// One P6 record: the source row and what phase A did with its file.
pub(crate) struct Item {
    rec: AttachmentRecord,
    obs: Obs,
}

/// What phase A decided for a record.
enum PhaseA {
    Obs(Obs),
    Cancelled,
    Stop(StopReason),
}

/// The per-pass state the phases share.
#[derive(Default)]
struct State {
    next_partial: u64,
    /// Counted outside any record (partial files left behind, flushes);
    /// added to the next committed batch.
    pending: AttachmentFileCounts,
}

pub(crate) struct Attachments<'a> {
    kind: EntityKind,
    rd: &'a RdSkip,
    store: StoreCtx<'a>,
    state: RefCell<State>,
}

fn rename(source: &'static str) -> &'static str {
    match source {
        "created_date" => "created",
        "start_date" => "start",
        "emoji_image_short_description" => "emoji_description",
        other => other,
    }
}

/// `EFBIG` or `ERROR_FILE_TOO_LARGE`.
fn too_large(kind: io::ErrorKind, os: Option<i32>) -> bool {
    kind == io::ErrorKind::FileTooLarge
        || if cfg!(windows) {
            os == Some(223)
        } else {
            os == Some(27)
        }
}

/// `ENOSPC`, `ERROR_DISK_FULL` or `ERROR_HANDLE_DISK_FULL`.
fn no_space(kind: io::ErrorKind, os: Option<i32>) -> bool {
    kind == io::ErrorKind::StorageFull
        || if cfg!(windows) {
            matches!(os, Some(112 | 39))
        } else {
            os == Some(28)
        }
}

/// The partial file's writer: marks the first write as a fault point and,
/// under a test hook, fails it.
struct SinkWriter<W: Write> {
    inner: W,
    first: bool,
}

impl<W: Write> Write for SinkWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.first {
            self.first = false;
            point(FaultPoint::AttachmentCopying);
            if let Some(code) = crate::fault::copy_write_error() {
                return Err(io::Error::from_raw_os_error(code));
            }
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The attachment table's merge definition.
pub(crate) fn table_def() -> TableDef {
    TableDef::new(Entity::Attachment, "attachment", false, &[])
}

impl<'a> Attachments<'a> {
    pub(crate) fn new(rd: &'a RdSkip, store: StoreCtx<'a>) -> Attachments<'a> {
        Attachments {
            kind: EntityKind {
                def: table_def(),
                key: KeyStyle::Guid(0),
                lineage_seen: true,
                derive_insert: |_| Vec::new(),
                derive_update: |_, _| Vec::new(),
            },
            rd,
            store,
            state: RefCell::new(State::default()),
        }
    }

    fn cells(&self, r: &AttachmentRecord) -> Vec<Cell> {
        let mut cells = vec![
            Cell::text(&r.guid),
            Cell::text(&r.original_guid),
            Cell::text(&r.filename),
            Cell::text(&r.uti),
            Cell::text(&r.mime_type),
            Cell::text(&r.transfer_name),
            Cell::int(&r.total_bytes),
            Cell::boolean(&r.is_sticker),
            Cell::boolean(&r.hide_attachment),
            Cell::boolean(&r.is_outgoing),
            Cell::int(&r.transfer_state),
            Cell::time(&r.created_date),
            Cell::time(&r.start_date),
            Cell::text(&r.emoji_image_short_description),
        ];
        keep_raw(&self.kind.def, &mut cells, &r.raw_issues, rename);
        cells
    }

    fn left_partial(&self) {
        let mut st = self.state.borrow_mut();
        st.pending.partials_left = st.pending.partials_left.saturating_add(1);
    }

    /// Phase A for one record (spec §9.1).
    fn phase_a(&self, conn: &Connection, run: &Run, rec: &AttachmentRecord) -> PhaseA {
        let path_class = match rec.location.as_ref() {
            Some(loc) => path_class_code(loc.class),
            None => match &rec.filename {
                Col::Value(v) => {
                    path_class_code(Some(classify_attachment_path(v.expose().as_bytes())))
                }
                _ => PathClassCode::NoClass,
            },
        };
        let obs = |lookup, copy| {
            PhaseA::Obs(Obs {
                path_class,
                lookup,
                copy,
                file: None,
            })
        };
        if !self.store.copy || self.rd.skips_attachment(rec.rowid) {
            return obs(LookupCode::NotLookedUp, CopyCode::NotRequested);
        }
        let encrypted = self.store.ob.is_encrypted();
        let (lookup, c) = match copyable(rec, encrypted) {
            Ok(x) => x,
            Err((lookup, copy)) => return obs(lookup, copy),
        };
        // The archive's side: is the current blob's file already here?
        let current: Option<([u8; 32], u64)> = exact_key(rec, &run.dups).and_then(|k| {
            conn.query_row(CURRENT_BLOB_SQL, [k.as_slice()], |r| {
                let sha: Vec<u8> = r.get(0)?;
                let len: i64 = r.get(1)?;
                Ok((sha, len))
            })
            .optional()
            .ok()
            .flatten()
            .and_then(|(sha, len)| {
                Some((
                    <[u8; 32]>::try_from(sha.as_slice()).ok()?,
                    u64::try_from(len).ok()?,
                ))
            })
        });
        let mut replacing = None;
        if let Some((sha, byte_len)) = current {
            let file_ok = StorageName::of_sha256(&sha).is_some_and(|n| {
                store_file_state(&self.store.dir, &n) == StoreFileState::Regular(byte_len)
            });
            if file_ok && c.plain_len == Some(byte_len) {
                return obs(lookup, CopyCode::AlreadyArchived);
            }
            if !file_ok {
                replacing = Some(sha);
            }
        }
        // Space for this file above the floor (spec §9.1 step 2).
        if !run.guard.file_fits(c.stored_len) {
            return PhaseA::Stop(StopReason::InsufficientSpace);
        }
        let partial = {
            let mut st = self.state.borrow_mut();
            Partial::create(&self.store.dir, &mut st.next_partial)
        };
        let mut partial = match partial {
            Ok(p) => p,
            Err(e) if no_space(e.kind(), e.raw_os_error()) => {
                return PhaseA::Stop(StopReason::InsufficientSpace)
            }
            Err(_) => return obs(lookup, CopyCode::WriteFailed),
        };
        point(FaultPoint::AttachmentPartialCreated);
        match self.copy_into(&mut partial, c.key) {
            Ok(copied) => {
                point(FaultPoint::AttachmentSynced);
                PhaseA::Obs(Obs {
                    path_class,
                    lookup,
                    copy: CopyCode::Copied,
                    file: Some(Copied {
                        sha: copied.sha256,
                        len: copied.len,
                        size_matches: copied.size_matches,
                        partial: Some(partial),
                        replacing,
                        stored: false,
                    }),
                })
            }
            Err(end) => {
                if !partial.remove(self.store.store.as_ref()) {
                    self.left_partial();
                }
                match end {
                    CopyEnd::Code(copy) => obs(lookup, copy),
                    CopyEnd::Cancelled => PhaseA::Cancelled,
                    CopyEnd::Stop(r) => PhaseA::Stop(r),
                }
            }
        }
    }

    /// Steps 4 and 5 of phase A: the copy, the flush and the checks.
    fn copy_into(&self, partial: &mut Partial, key: &AttachmentKey) -> Result<CopiedFile, CopyEnd> {
        let io_end = |e: &io::Error| {
            if too_large(e.kind(), e.raw_os_error()) {
                CopyEnd::Code(CopyCode::TooLargeForVolume)
            } else if no_space(e.kind(), e.raw_os_error()) {
                CopyEnd::Stop(StopReason::InsufficientSpace)
            } else {
                CopyEnd::Code(CopyCode::WriteFailed)
            }
        };
        let copied = {
            let file = partial.file().map_err(|e| io_end(&e))?;
            let mut sink = SinkWriter {
                inner: BufWriter::with_capacity(COPY_BUFFER, file),
                first: true,
            };
            let mut progress = |_: Progress| {};
            let mut jctx = JobCtx::new(self.store.cancel, &mut progress);
            let copied = self
                .store
                .ob
                .copy_file_to(key, &mut sink, &mut jctx)
                .map_err(copy_end)?;
            // `into_inner` surfaces a failed final flush.
            sink.inner.into_inner().map_err(|e| io_end(e.error()))?;
            copied
        };
        partial.finish(copied.len).map_err(|e| io_end(&e))?;
        Ok(copied)
    }

    /// Phase D for one record, inside its savepoint: the blob row, the
    /// observation and `current_blob_id`.
    fn observe(
        st: &mut ObsStmts<'_>,
        run: &Run,
        id: i64,
        obs: &Obs,
        files: &mut AttachmentFileCounts,
    ) -> rusqlite::Result<()> {
        let mut blob_id: Option<i64> = None;
        let mut size_matched: Option<i64> = None;
        if let Some(f) = obs.file.as_ref().filter(|f| f.stored) {
            let Some(name) = StorageName::of_sha256(&f.sha) else {
                return Err(rusqlite::Error::InvalidQuery);
            };
            let added = st.blob_ins.execute(rusqlite::params![
                f.sha.as_slice(),
                i64::try_from(f.len).unwrap_or(i64::MAX),
                name.as_str(),
                run.session
            ])?;
            files.blobs_added = files
                .blobs_added
                .saturating_add(u64::try_from(added).unwrap_or(0));
            blob_id = Some(st.blob_id.query_row([f.sha.as_slice()], |r| r.get(0))?);
            size_matched = f.size_matches.map(i64::from);
        }
        st.obs_ins.execute(rusqlite::params![
            id,
            run.session,
            obs.path_class.code(),
            obs.lookup.code(),
            obs.copy.code(),
            blob_id,
            size_matched
        ])?;
        let Some(blob) = blob_id else {
            return Ok(());
        };
        // `current_blob_id` follows rank (spec §8.7): this session's blob
        // wins unless another session that read bytes ranks higher.
        let higher: Option<Vec<u8>> = st
            .higher
            .query_row(rusqlite::params![id, run.session], |r| r.get(0))?;
        if higher.is_some_and(|h| h.as_slice() > run.rank.as_bytes().as_slice()) {
            return Ok(());
        }
        let moved = st.current.execute(rusqlite::params![blob, id])?;
        files.current_moved = files
            .current_moved
            .saturating_add(u64::try_from(moved).unwrap_or(0));
        Ok(())
    }
}

/// Phase D's statements, prepared once per batch.
struct ObsStmts<'c> {
    blob_ins: rusqlite::Statement<'c>,
    blob_id: rusqlite::Statement<'c>,
    obs_ins: rusqlite::Statement<'c>,
    higher: rusqlite::Statement<'c>,
    current: rusqlite::Statement<'c>,
}

impl<'c> ObsStmts<'c> {
    fn prepare(tx: &'c Connection) -> rusqlite::Result<ObsStmts<'c>> {
        Ok(ObsStmts {
            blob_ins: tx.prepare(
                "INSERT INTO blob (sha256, byte_len, storage_name, first_session_id) \
                 VALUES (?1, ?2, ?3, ?4) ON CONFLICT (sha256) DO NOTHING",
            )?,
            blob_id: tx.prepare("SELECT id FROM blob WHERE sha256 = ?1")?,
            obs_ins: tx.prepare(
                "INSERT INTO attachment_observation (attachment_id, session_id, path_class, \
                     lookup, copy, blob_id, size_matched_manifest) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT (attachment_id, session_id) DO NOTHING",
            )?,
            higher: tx.prepare(
                "SELECT max(s.rank) FROM attachment_observation o \
                 JOIN import_session s ON s.id = o.session_id \
                 WHERE o.attachment_id = ?1 AND o.blob_id IS NOT NULL AND o.session_id <> ?2",
            )?,
            current: tx.prepare(
                "UPDATE attachment SET current_blob_id = ?1 \
                 WHERE id = ?2 AND current_blob_id IS NOT ?1",
            )?,
        })
    }
}

/// How a copy ended without a file.
enum CopyEnd {
    Code(CopyCode),
    Cancelled,
    Stop(StopReason),
}

/// Classify a `copy_file_to` error (spec §9.1 step 5).
fn copy_end(e: BackupError) -> CopyEnd {
    match e {
        BackupError::Cancelled => CopyEnd::Cancelled,
        BackupError::SourceChanged => CopyEnd::Stop(StopReason::SourceChanged),
        BackupError::LimitExceeded {
            what: LimitKind::AttachmentBytes,
            ..
        } => CopyEnd::Code(CopyCode::OverLimit),
        BackupError::Io {
            side: IoSide::Sink,
            kind,
            os_code,
            ..
        } => {
            if too_large(kind, os_code) {
                CopyEnd::Code(CopyCode::TooLargeForVolume)
            } else if no_space(kind, os_code) {
                CopyEnd::Stop(StopReason::InsufficientSpace)
            } else {
                CopyEnd::Code(CopyCode::WriteFailed)
            }
        }
        BackupError::FileUnavailable(_) => CopyEnd::Code(CopyCode::BlobUnavailable),
        _ => CopyEnd::Code(CopyCode::ReadFailed),
    }
}

/// Count one row's outcome.
fn tally(files: &mut AttachmentFileCounts, obs: &Obs) {
    let field = match obs.copy {
        CopyCode::Copied => &mut files.copied,
        CopyCode::Replaced => &mut files.replaced,
        CopyCode::Deduplicated => &mut files.deduplicated,
        CopyCode::Adopted => &mut files.adopted,
        CopyCode::OrphanReplaced => &mut files.orphan_replaced,
        CopyCode::AlreadyArchived => &mut files.already_archived,
        CopyCode::StoreConflict => &mut files.store_conflicts,
        CopyCode::NotRequested => &mut files.not_requested,
        CopyCode::NotLookedUp => &mut files.not_looked_up,
        CopyCode::NotInBackup => &mut files.not_in_backup,
        CopyCode::BlobUnavailable => &mut files.blob_unavailable,
        CopyCode::OverLimit => &mut files.over_limit,
        CopyCode::TooLargeForVolume => &mut files.too_large_for_volume,
        CopyCode::ReadFailed => &mut files.read_failed,
        CopyCode::WriteFailed => &mut files.write_failed,
    };
    *field = field.saturating_add(1);
    if let Some(f) = &obs.file {
        files.bytes_copied = files.bytes_copied.saturating_add(f.len);
        // The file's own `sync_all`.
        files.fsyncs = files.fsyncs.saturating_add(1);
        if f.size_matches == Some(false) {
            files.size_mismatch = files.size_mismatch.saturating_add(1);
        }
    }
}

impl PassWriter for Attachments<'_> {
    type Record = Item;
    const PASS: Pass = Pass::Attachments;
    const PHASE: ImportPhase = ImportPhase::CopyingAttachments;
    const SOURCE: KnownTable = KnownTable::Attachment;

    fn rowid(r: &Item) -> i64 {
        r.rec.rowid
    }

    fn size(r: &Item) -> u64 {
        let r = &r.rec;
        [
            &r.guid,
            &r.original_guid,
            &r.filename,
            &r.uti,
            &r.mime_type,
            &r.transfer_name,
            &r.emoji_image_short_description,
        ]
        .iter()
        .map(|c| text_len(c))
        .fold(128u64, u64::saturating_add)
    }

    fn read(
        &self,
        db: &SmsDb<'_>,
        conn: &Connection,
        run: &Run,
        from: Cursor,
        ctx: &mut MessagesCtx<'_>,
        f: &mut dyn FnMut(Item) -> ControlFlow<Brk>,
    ) -> Result<PassOutcome<Brk>, MessagesError> {
        let mut locator = self
            .store
            .copy
            .then(|| AttachmentLocator::new(self.store.ob, true));
        let cancel = self.store.cancel;
        db.for_each_attachment(from, locator.as_mut(), ctx, |rec| {
            if cancel.is_cancelled() {
                return ControlFlow::Break(Brk::Cancelled);
            }
            match self.phase_a(conn, run, &rec) {
                PhaseA::Obs(obs) => f(Item { rec, obs }),
                PhaseA::Cancelled => ControlFlow::Break(Brk::Cancelled),
                PhaseA::Stop(r) => ControlFlow::Break(Brk::Stop(r)),
            }
        })
    }

    fn ends_batch(&self, buffered: &[Item]) -> bool {
        let mut files = 0usize;
        let mut bytes = 0u64;
        for it in buffered {
            if let Some(f) = &it.obs.file {
                files = files.saturating_add(1);
                bytes = bytes.saturating_add(f.len);
            }
        }
        let last_large = buffered
            .last()
            .and_then(|it| it.obs.file.as_ref())
            .is_some_and(|f| f.len >= LARGE_FILE);
        files >= BATCH_FILES || bytes >= BATCH_FILE_BYTES || last_large
    }

    /// Phases B and C.
    fn before_batch(
        &self,
        conn: &mut Connection,
        run: &Run,
        records: &mut [Item],
    ) -> Result<(), PassError> {
        let digests: BTreeSet<[u8; 32]> = records
            .iter()
            .filter_map(|it| it.obs.file.as_ref().map(|f| f.sha))
            .collect();
        if digests.is_empty() {
            return Ok(());
        }
        // Phase B: the intents, committed before any rename.
        run.guard(conn)?;
        let tx = rusqlite::Transaction::new(&mut *conn, TransactionBehavior::Immediate)
            .map_err(PassError::Batch)?;
        for sha in &digests {
            tx.execute(
                "INSERT INTO blob_intent (sha256, session_id) \
                 SELECT ?1, ?2 WHERE NOT EXISTS (SELECT 1 FROM blob WHERE sha256 = ?1) \
                 ON CONFLICT (sha256, session_id) DO NOTHING",
                rusqlite::params![sha.as_slice(), run.session],
            )
            .map_err(PassError::Batch)?;
        }
        tx.commit().map_err(PassError::Batch)?;
        point(FaultPoint::AttachmentIntentCommitted);

        // Phase C: placement.
        let mut stored_len: BTreeMap<[u8; 32], Option<u64>> = BTreeMap::new();
        {
            let mut stmt = conn
                .prepare("SELECT byte_len FROM blob WHERE sha256 = ?1")
                .map_err(PassError::Batch)?;
            for sha in &digests {
                let len: Option<i64> = stmt
                    .query_row([sha.as_slice()], |r| r.get(0))
                    .optional()
                    .map_err(PassError::Batch)?;
                stored_len.insert(*sha, len.and_then(|l| u64::try_from(l).ok()));
            }
        }
        let store = Arc::clone(&self.store.store);
        let mut placer = Placer::new(&self.store.dir, store.as_ref());
        let mut placed: BTreeMap<[u8; 32], u64> = BTreeMap::new();
        for it in records.iter_mut() {
            let Some(f) = it.obs.file.as_mut() else {
                continue;
            };
            let Some(partial) = f.partial.take() else {
                continue;
            };
            let Some(name) = StorageName::of_sha256(&f.sha) else {
                continue;
            };
            let referenced = stored_len.get(&f.sha).copied().flatten();
            let known = placed.get(&f.sha).copied().or(referenced);
            let (copy, stored) = match placer.place(partial, &name, known) {
                Placement::Placed => {
                    placed.insert(f.sha, f.len);
                    let again = f.replacing == Some(f.sha);
                    (
                        if again {
                            CopyCode::Replaced
                        } else {
                            CopyCode::Copied
                        },
                        true,
                    )
                }
                Placement::Deduplicated => (CopyCode::Deduplicated, true),
                Placement::Adopted => {
                    placed.insert(f.sha, f.len);
                    (CopyCode::Adopted, true)
                }
                Placement::OrphanReplaced => {
                    placed.insert(f.sha, f.len);
                    (CopyCode::OrphanReplaced, true)
                }
                // The bytes read hash to a stored blob whose file is
                // unusable: the observation still names that blob.
                Placement::Conflict => (CopyCode::StoreConflict, known.is_some()),
                Placement::Failed(_) => (CopyCode::WriteFailed, false),
            };
            it.obs.copy = copy;
            f.stored = stored;
        }
        let flushes = placer.sync().map_err(|e| PassError::Store(e.kind()))?;
        {
            let mut st = self.state.borrow_mut();
            st.pending.fsyncs = st.pending.fsyncs.saturating_add(flushes);
            st.pending.partials_left = st
                .pending
                .partials_left
                .saturating_add(placer.partials_left);
        }
        point(FaultPoint::AttachmentDirSynced);
        Ok(())
    }

    /// Phase D (with the cursor and counts written by the batch).
    fn write(
        &self,
        tx: &Connection,
        run: &Run,
        records: &[Item],
        counts: &mut ImportCounts,
        issues: &mut IssueDeltas,
    ) -> rusqlite::Result<()> {
        let mut excluded = 0u64;
        let by_rowid: BTreeMap<i64, &Obs> =
            records.iter().map(|it| (it.rec.rowid, &it.obs)).collect();
        let recs: Vec<EntityRecord> = records
            .iter()
            .filter(|it| {
                let skip = self.rd.skips_attachment(it.rec.rowid);
                if skip {
                    excluded = excluded.saturating_add(1);
                }
                !skip
            })
            .map(|it| EntityRecord {
                rowid: it.rec.rowid,
                cells: self.cells(&it.rec),
            })
            .collect();
        let mut files = AttachmentFileCounts::default();
        let mut stmts = ObsStmts::prepare(tx)?;
        let mut after = |rowid: i64, id: i64| -> rusqlite::Result<()> {
            let Some(obs) = by_rowid.get(&rowid) else {
                return Ok(());
            };
            Self::observe(&mut stmts, run, id, obs, &mut files)?;
            tally(&mut files, obs);
            Ok(())
        };
        write_records(
            tx,
            &self.kind,
            run,
            recs.into_iter(),
            &mut counts.attachments,
            issues,
            &mut after,
        )?;
        // This batch's intents: its files are placed and their rows are in
        // this transaction.
        let mut del =
            tx.prepare("DELETE FROM blob_intent WHERE session_id = ?1 AND sha256 = ?2")?;
        let digests: BTreeSet<[u8; 32]> = records
            .iter()
            .filter_map(|it| it.obs.file.as_ref().map(|f| f.sha))
            .collect();
        for sha in &digests {
            del.execute(rusqlite::params![run.session, sha.as_slice()])?;
        }
        // Counted outside the records since the last batch. A batch that
        // rolls back loses them; its session fails anyway.
        files.add(&std::mem::take(&mut self.state.borrow_mut().pending));
        counts.attachment_files.add(&files);
        let rd = &mut counts.recently_deleted;
        rd.attachments_excluded = rd.attachments_excluded.saturating_add(excluded);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cells_follow_the_column_list() {
        let def = TableDef::new(Entity::Attachment, "attachment", false, &[]);
        let names: Vec<&str> = def.cols.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            [
                "guid",
                "original_guid",
                "filename",
                "uti",
                "mime_type",
                "transfer_name",
                "total_bytes",
                "is_sticker",
                "hide_attachment",
                "is_outgoing",
                "transfer_state",
                "created",
                "start",
                "emoji_description"
            ]
        );
        for (source, archive) in [
            ("created_date", "created"),
            ("start_date", "start"),
            ("emoji_image_short_description", "emoji_description"),
            ("filename", "filename"),
        ] {
            assert_eq!(rename(source), archive);
        }
    }

    #[test]
    fn os_errors_classify() {
        let big = if cfg!(windows) { 223 } else { 27 };
        let full = if cfg!(windows) { 112 } else { 28 };
        assert!(too_large(io::ErrorKind::Other, Some(big)));
        assert!(!too_large(io::ErrorKind::Other, Some(full)));
        assert!(no_space(io::ErrorKind::Other, Some(full)));
        assert!(no_space(io::ErrorKind::StorageFull, None));
        assert!(!no_space(io::ErrorKind::Other, None));
        assert!(matches!(
            copy_end(BackupError::SourceChanged),
            CopyEnd::Stop(StopReason::SourceChanged)
        ));
        assert!(matches!(
            copy_end(BackupError::Cancelled),
            CopyEnd::Cancelled
        ));
    }
}
