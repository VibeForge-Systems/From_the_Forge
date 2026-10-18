//! Session facts and bookkeeping (spec §8.2, §8.3, §8.7).
//!
//! [`SessionFacts`] is everything a session row records about its source:
//! the content digest and its framing, the read profile, the rank, the
//! qualifiers, the capabilities, and the `source_backup` facts. It is
//! computed before anything is written.
//!
//! [`begin_session`] is the one transaction after the archive's first
//! write: the `source_backup` upsert, the lineage, superseding unfinished
//! sessions that are not continued, and the new session row with its
//! duplicate keys (or the continued session set back to running), and the
//! rules high-water marks. [`end_session`] records a cancelled, stopped or
//! failed session; it is best effort.

use std::time::{SystemTime, UNIX_EPOCH};

use oma_backup::{OpenBackup, RawText, StagedDatabase, WalOutcome};
use oma_messages::{Capabilities, KnownTable, TextEncoding};
use rusqlite::{Connection, TransactionBehavior};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::counts::ImportCounts;
use super::keys::{encode, Field, Rank};
use super::preflight::{Binding, DupSet, Resume};
use super::LineageChoice;
use crate::schema::codes::{
    IssueCode, OrderClass, OrderKeySource, QualifierBit, SessionState, TextEncodingCode,
    UnreadableKind, WalOutcomeCode,
};
use crate::schema::{DECODER_VERSION, DERIVER_VERSION, SCHEMA_VERSION};
use crate::APP_VERSION;

/// The earliest backup date the rank accepts: 2007-01-01T00:00:00Z.
pub const ORDER_KEY_FLOOR_NS: i64 = 1_167_609_600_000_000_000;
/// How far past the import's start a backup date may lie (48 hours).
pub const ORDER_KEY_SLACK_NS: i64 = 48 * 3600 * 1_000_000_000;

/// Unix nanoseconds of a plist date; `None` outside the `i64` range.
fn unix_ns(t: SystemTime) -> Option<i64> {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).ok(),
        Err(e) => i64::try_from(e.duration().as_nanos())
            .ok()
            .and_then(i64::checked_neg),
    }
}

/// The bits of `source_backup.wrong_type_fields` and `oversized_fields`
/// (v1.sql). A field with no value and neither bit was absent.
pub(crate) mod source_field {
    pub(crate) const DEVICE_NAME: i64 = 1;
    pub(crate) const PRODUCT_TYPE: i64 = 2;
    pub(crate) const PRODUCT_VERSION: i64 = 4;
    pub(crate) const BUILD_VERSION: i64 = 8;
    pub(crate) const SNAPSHOT_STATE: i64 = 16;
    pub(crate) const INFO_LAST_BACKUP_DATE: i64 = 32;
    pub(crate) const STATUS_DATE: i64 = 64;
    pub(crate) const IS_FULL_BACKUP: i64 = 128;
}

/// The `source_backup` facts: no UDID, serial, phone number or path (D14).
///
/// The strings are the plist values **as recorded** ([`RawText`]): not
/// sanitised and not truncated. A string over the cap or of the wrong type
/// is not stored, and its bit in `oversized_fields` or `wrong_type_fields`
/// says so; display sanitising happens only when shown (I5).
#[derive(Clone, Default)]
pub(crate) struct SourceBackup {
    device_name: RawText,
    product_type: RawText,
    product_version: RawText,
    build_version: RawText,
    info_last_backup_ns: Option<i64>,
    info_last_backup_wrong_type: bool,
    status_date_ns: Option<i64>,
    status_date_wrong_type: bool,
    snapshot_state_raw: RawText,
    is_full_backup: Option<bool>,
    is_full_backup_wrong_type: bool,
    encrypted: bool,
}

impl SourceBackup {
    fn texts(&self) -> [(&RawText, i64); 5] {
        [
            (&self.device_name, source_field::DEVICE_NAME),
            (&self.product_type, source_field::PRODUCT_TYPE),
            (&self.product_version, source_field::PRODUCT_VERSION),
            (&self.build_version, source_field::BUILD_VERSION),
            (&self.snapshot_state_raw, source_field::SNAPSHOT_STATE),
        ]
    }

    /// `source_backup.wrong_type_fields`.
    pub(crate) fn wrong_type_fields(&self) -> i64 {
        let mut bits = self
            .texts()
            .iter()
            .filter(|(t, _)| **t == RawText::WrongType)
            .fold(0, |b, (_, bit)| b | bit);
        for (wrong, bit) in [
            (
                self.info_last_backup_wrong_type,
                source_field::INFO_LAST_BACKUP_DATE,
            ),
            (self.status_date_wrong_type, source_field::STATUS_DATE),
            (self.is_full_backup_wrong_type, source_field::IS_FULL_BACKUP),
        ] {
            if wrong {
                bits |= bit;
            }
        }
        bits
    }

    /// `source_backup.oversized_fields`.
    pub(crate) fn oversized_fields(&self) -> i64 {
        self.texts()
            .iter()
            .filter(|(t, _)| matches!(t, RawText::Oversized { .. }))
            .fold(0, |b, (_, bit)| b | bit)
    }

    /// SHA-256 over the tagged encoding of every field, each in its state
    /// (absent, wrong type, oversized with its length, or the value).
    fn observed_key(&self) -> [u8; 32] {
        let unreadable =
            Field::Unreadable(u8::try_from(UnreadableKind::TypeMismatch.code()).unwrap_or(u8::MAX));
        let mut out = Vec::new();
        let text = |out: &mut Vec<u8>, v: &RawText| match v {
            RawText::Value(s) => encode(out, Field::Bytes(s.as_bytes())),
            RawText::Oversized { bytes } => encode(out, Field::Oversized(*bytes)),
            RawText::WrongType => encode(out, unreadable),
            _ => encode(out, Field::Absent),
        };
        let int = |out: &mut Vec<u8>, v: Option<i64>, wrong: bool| match (v, wrong) {
            (Some(i), _) => encode(out, Field::Int(i)),
            (None, true) => encode(out, unreadable),
            (None, false) => encode(out, Field::Absent),
        };
        text(&mut out, &self.device_name);
        text(&mut out, &self.product_type);
        text(&mut out, &self.product_version);
        text(&mut out, &self.build_version);
        int(
            &mut out,
            self.info_last_backup_ns,
            self.info_last_backup_wrong_type,
        );
        int(&mut out, self.status_date_ns, self.status_date_wrong_type);
        text(&mut out, &self.snapshot_state_raw);
        int(
            &mut out,
            self.is_full_backup.map(i64::from),
            self.is_full_backup_wrong_type,
        );
        int(&mut out, Some(i64::from(self.encrypted)), false);
        Sha256::digest(&out).into()
    }
}

/// Everything a session row records about its source.
#[derive(Clone)]
pub(crate) struct SessionFacts {
    pub(crate) binding: Binding,
    pub(crate) encoding: Option<TextEncodingCode>,
    pub(crate) order_key: Option<i64>,
    pub(crate) order_source: Option<OrderKeySource>,
    pub(crate) rank: Rank,
    pub(crate) qualifiers: i64,
    pub(crate) ios_version_untested: bool,
    pub(crate) capabilities: String,
    source: SourceBackup,
}

impl SessionFacts {
    /// The backup's device name, when one is recorded as a string (for the
    /// gate's comparison only; never logged).
    pub(crate) fn device_name(&self) -> Option<&str> {
        self.source.device_name.as_str()
    }

    /// The backup's product type, likewise.
    pub(crate) fn product_type(&self) -> Option<&str> {
        self.source.product_type.as_str()
    }
}

impl std::fmt::Debug for SessionFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionFacts")
            .field("rank", &self.rank)
            .field("qualifiers", &self.qualifiers)
            .finish_non_exhaustive()
    }
}

/// The read profile (§8.2): SHA-256 of every limit that decides what a
/// pass reads (the column caps and the row budget). The decoder budget is
/// covered by the decoder version.
pub(crate) fn read_profile() -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"oma-read-profile-v1");
    for c in [
        oma_messages::CAP_ATTRIBUTED_BODY,
        oma_messages::CAP_TEXT,
        oma_messages::CAP_SUMMARY,
        oma_messages::CAP_PROPERTIES,
        oma_messages::CAP_PAYLOAD_DATA,
        oma_messages::CAP_RAW_UNREADABLE,
        oma_messages::CAP_GUID,
        oma_messages::CAP_IDENTIFIER,
        oma_messages::CAP_OTHER_TEXT,
    ] {
        h.update(c.to_be_bytes());
    }
    h.update(oma_messages::ROW_BYTES_BUDGET.to_be_bytes());
    for t in KnownTable::ALL {
        for c in t.known_columns() {
            h.update(c.code().to_be_bytes());
            h.update(c.cap().unwrap_or(0).to_be_bytes());
        }
    }
    h.finalize().into()
}

/// The closed capability booleans: which known tables and columns the
/// probe found (static names only).
fn capabilities_json(caps: &Capabilities) -> String {
    let mut tables = Map::new();
    let mut columns = Map::new();
    for tc in &caps.tables {
        tables.insert(tc.table.name().to_owned(), Value::Bool(tc.present));
        for c in tc.table.known_columns() {
            let mut name = String::from(tc.table.name());
            name.push('.');
            name.push_str(c.name());
            columns.insert(
                name,
                Value::Bool(tc.present && tc.present_columns.contains(c)),
            );
        }
    }
    let mut root = Map::new();
    root.insert("tables".to_owned(), Value::Object(tables));
    root.insert("columns".to_owned(), Value::Object(columns));
    Value::Object(root).to_string()
}

/// Compute the session facts before anything is written.
pub(crate) fn facts(
    ob: &OpenBackup,
    staged: &StagedDatabase<'_>,
    caps: &Capabilities,
    opts: &super::ImportOptions,
    now_ns: i64,
    lineage: Option<i64>,
    cancel: &oma_backup::CancelToken,
) -> Result<SessionFacts, oma_backup::BackupError> {
    let ins = ob.inspection();
    let info = ins.info.as_ref();
    let status = ins.status.as_ref();
    let status_date = status.and_then(|s| s.date).map(SystemTime::from);
    let info_date = info.and_then(|i| i.last_backup_date).map(SystemTime::from);
    let info_raw = info.map(|i| i.raw.clone()).unwrap_or_default();
    let status_raw = status.map(|s| s.raw.clone()).unwrap_or_default();
    let source = SourceBackup {
        device_name: info_raw.device_name,
        product_type: info_raw.product_type,
        product_version: info_raw.product_version,
        build_version: info_raw.build_version,
        info_last_backup_ns: info_date.and_then(unix_ns),
        info_last_backup_wrong_type: info_raw.last_backup_date_wrong_type,
        status_date_ns: status_date.and_then(unix_ns),
        status_date_wrong_type: status_raw.date_wrong_type,
        snapshot_state_raw: status_raw.snapshot_state,
        is_full_backup: status.and_then(|s| s.is_full_backup),
        is_full_backup_wrong_type: status_raw.is_full_backup_wrong_type,
        encrypted: ob.is_encrypted(),
    };

    // The order key (§8.7): Status.plist Date, else Info.plist Last Backup
    // Date, each only inside 2007-01-01 .. start + 48 h.
    let ceiling = now_ns.saturating_add(ORDER_KEY_SLACK_NS);
    let mut qualifiers = 0i64;
    let mut order = None;
    for (date, src) in [
        (status_date, OrderKeySource::StatusDate),
        (info_date, OrderKeySource::InfoLastBackupDate),
    ] {
        let Some(date) = date else { continue };
        match unix_ns(date) {
            Some(ns) if (ORDER_KEY_FLOOR_NS..=ceiling).contains(&ns) => {
                order = Some((ns, src));
                break;
            }
            _ => qualifiers |= QualifierBit::BackupDateOutOfRange.mask(),
        }
    }

    // The M3 qualifiers this step can establish (spec §8.7: the WAL-related
    // ones decide whether a session is a full observation).
    let (wal_outcome, wal_len) = match staged.wal_outcome {
        WalOutcome::NotListed => (WalOutcomeCode::NotListed, None),
        WalOutcome::Staged { bytes } => (
            WalOutcomeCode::Staged,
            Some(i64::try_from(bytes).unwrap_or(i64::MAX)),
        ),
        _ => (WalOutcomeCode::Skipped, None),
    };
    if wal_outcome == WalOutcomeCode::Skipped {
        qualifiers |= QualifierBit::WalSkipped.mask();
    }
    if wal_outcome == WalOutcomeCode::NotListed && staged.header_facts()?.says_wal() {
        qualifiers |= QualifierBit::WalStateUnknown.mask();
    }
    if let Some(f) = staged.wal_facts(cancel)? {
        if f.frames_after_first_invalid > 0 || (!f.header_valid && wal_len.unwrap_or(0) > 0) {
            qualifiers |= QualifierBit::WalFramesInvalid.mask();
        }
    }
    // An encoding this build does not know is not recorded (NULL), and is
    // still not UTF-8.
    let encoding = caps.encoding.and_then(|e| match e {
        TextEncoding::Utf8 => Some(TextEncodingCode::Utf8),
        TextEncoding::Utf16le => Some(TextEncodingCode::Utf16le),
        TextEncoding::Utf16be => Some(TextEncodingCode::Utf16be),
        _ => None,
    });
    if caps.encoding.is_some_and(|e| e != TextEncoding::Utf8) {
        qualifiers |= QualifierBit::NonUtf8Encoding.mask();
    }
    if staged.size_matches == Some(false) || staged.wal_size_matches == Some(false) {
        qualifiers |= QualifierBit::SizeMismatch.mask();
    }
    if opts.ios_version_untested {
        qualifiers |= QualifierBit::IosVersionUntested.mask();
    }
    if caps.unknown_tables > 0 {
        qualifiers |= QualifierBit::UnknownTables.mask();
    }
    if caps.tables.iter().any(|t| t.unknown_columns > 0) {
        qualifiers |= QualifierBit::UnknownColumns.mask();
    }
    if caps.tables.iter().any(|t| t.wrong_case_columns > 0) {
        qualifiers |= QualifierBit::WrongCaseColumns.mask();
    }

    let digest = *staged.content_digest().as_bytes();
    Ok(SessionFacts {
        binding: Binding {
            digest,
            main_len: i64::try_from(staged.main_bytes).unwrap_or(i64::MAX),
            wal_outcome: wal_outcome.code(),
            wal_len,
            app_version: APP_VERSION,
            schema_version: i64::from(SCHEMA_VERSION),
            deriver_version: i64::from(DERIVER_VERSION),
            decoder_version: i64::from(DECODER_VERSION),
            read_profile: read_profile(),
            include_recently_deleted: opts.include_recently_deleted,
            copy_attachments: opts.copy_attachments,
            lineage,
        },
        encoding,
        order_key: order.map(|(k, _)| k),
        order_source: order.map(|(_, s)| s),
        rank: Rank::new(order.map(|(k, _)| k), &digest),
        qualifiers,
        ios_version_untested: opts.ios_version_untested,
        capabilities: capabilities_json(caps),
        source,
    })
}

/// The session an import runs as.
#[derive(Debug, Clone)]
pub(crate) struct Started {
    pub(crate) session: i64,
    pub(crate) lineage: i64,
    pub(crate) resumed: bool,
    pub(crate) superseded: u32,
    pub(crate) counts: ImportCounts,
}

fn sql(e: rusqlite::Error) -> crate::ArchiveError {
    crate::ArchiveError::sqlite(crate::Stage::Session, &e)
}

/// The session-local tables: "for the running session only; emptied when
/// it ends" (v1.sql).
const SESSION_LOCAL_TABLES: [&str; 3] = ["import_rowid_map", "import_occurrence", "import_dup_key"];

/// Delete one session's rows from the session-local tables. Every path
/// that ends a session for good (superseded at import or by a migration,
/// failed) calls this in the transaction that records the state, so the
/// two paths cannot drift apart.
pub(crate) fn drop_session_local(conn: &Connection, session: i64) -> rusqlite::Result<()> {
    for table in SESSION_LOCAL_TABLES {
        conn.execute(
            &format!("DELETE FROM {table} WHERE session_id = ?1"),
            [session],
        )?;
    }
    Ok(())
}

/// Delete the session-local rows of every failed (3) or superseded (5)
/// session: a backstop for rows an earlier build left behind.
fn drop_finished_session_local(conn: &Connection) -> rusqlite::Result<()> {
    for table in SESSION_LOCAL_TABLES {
        conn.execute(
            &format!(
                "DELETE FROM {table} WHERE session_id IN \
                 (SELECT id FROM import_session WHERE state IN (?1, ?2))"
            ),
            rusqlite::params![SessionState::Failed.code(), SessionState::Superseded.code()],
        )?;
    }
    Ok(())
}

/// Mark `ids` superseded (state 5) with `full_observation = 0`, and delete
/// their session-local rows: spec §8.3 step 3, shared by the import and by
/// [`crate::Archive::migrate`]. Only unfinished sessions (0, 2, 4, 6)
/// change. Returns how many sessions were marked.
pub(crate) fn supersede_sessions(
    conn: &Connection,
    ids: &[i64],
    now_ns: i64,
) -> rusqlite::Result<u32> {
    let mut superseded = 0u32;
    for id in ids {
        let n = conn.execute(
            "UPDATE import_session SET state = ?1, ended_at_ns = coalesce(ended_at_ns, ?2), \
                 full_observation = 0 \
             WHERE id = ?3 AND state IN (?4, ?5, ?6, ?7)",
            rusqlite::params![
                SessionState::Superseded.code(),
                now_ns,
                id,
                SessionState::Running.code(),
                SessionState::Cancelled.code(),
                SessionState::Interrupted.code(),
                SessionState::Stopped.code()
            ],
        )?;
        drop_session_local(conn, *id)?;
        superseded = superseded.saturating_add(u32::try_from(n).unwrap_or(0));
    }
    Ok(superseded)
}

/// The unfinished sessions (0, 2, 4, 6), for a migration to supersede.
pub(crate) fn unfinished_sessions(conn: &Connection) -> rusqlite::Result<Vec<i64>> {
    let mut stmt =
        conn.prepare("SELECT id FROM import_session WHERE state IN (?1, ?2, ?3, ?4) ORDER BY id")?;
    let rows = stmt.query_map(
        rusqlite::params![
            SessionState::Running.code(),
            SessionState::Cancelled.code(),
            SessionState::Interrupted.code(),
            SessionState::Stopped.code()
        ],
        |r| r.get(0),
    )?;
    rows.collect()
}

/// The source_backup row for these facts (upserted on `observed_key`).
fn source_backup_id(tx: &Connection, s: &SourceBackup) -> Result<i64, crate::ArchiveError> {
    let key = s.observed_key();
    tx.execute(
        "INSERT INTO source_backup (observed_key, device_name, product_type, product_version, \
             build_version, info_last_backup_ns, status_date_ns, snapshot_state_raw, \
             is_full_backup, encrypted, wrong_type_fields, oversized_fields) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
         ON CONFLICT (observed_key) DO NOTHING",
        rusqlite::params![
            key.as_slice(),
            s.device_name.as_str(),
            s.product_type.as_str(),
            s.product_version.as_str(),
            s.build_version.as_str(),
            s.info_last_backup_ns,
            s.status_date_ns,
            s.snapshot_state_raw.as_str(),
            s.is_full_backup.map(i64::from),
            i64::from(s.encrypted),
            s.wrong_type_fields(),
            s.oversized_fields()
        ],
    )
    .map_err(sql)?;
    tx.query_row(
        "SELECT id FROM source_backup WHERE observed_key = ?1",
        [key.as_slice()],
        |r| r.get(0),
    )
    .map_err(sql)
}

/// The bookkeeping transaction of §8.3.
///
/// On resume the continued session keeps its own `source_backup` row and
/// rank: nothing about the source is re-recorded, and its rows keep being
/// written at the stored rank ([`Resume::rank`]).
#[allow(clippy::too_many_arguments)]
pub(crate) fn begin_session(
    conn: &mut Connection,
    facts: &SessionFacts,
    lineage: LineageChoice,
    resume: Option<Resume>,
    supersede: &[i64],
    dups: &DupSet,
    now_ns: i64,
) -> Result<Started, crate::ArchiveError> {
    let tx = rusqlite::Transaction::new(conn, TransactionBehavior::Immediate).map_err(sql)?;

    // 1. source_backup (a new session only).
    let source_id = match resume {
        Some(_) => None,
        None => Some(source_backup_id(&tx, &facts.source)?),
    };

    // 2. The lineage.
    let lineage_id = match lineage {
        LineageChoice::New => {
            tx.execute("INSERT INTO lineage (created_at_ns) VALUES (?1)", [now_ns])
                .map_err(sql)?;
            tx.last_insert_rowid()
        }
        LineageChoice::Existing(id) => {
            let found: Option<i64> = tx
                .query_row("SELECT id FROM lineage WHERE id = ?1", [id.0], |r| r.get(0))
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    e => Err(e),
                })
                .map_err(sql)?;
            found.ok_or(crate::ArchiveError::UnknownLineage)?
        }
    };

    // 3. Supersede, and drop what failed or superseded sessions left.
    let superseded = supersede_sessions(&tx, supersede, now_ns).map_err(sql)?;
    drop_finished_session_local(&tx).map_err(sql)?;

    // 4. The new session, or the continued one.
    let b = &facts.binding;
    let (session, resumed, counts) = match (resume, source_id) {
        (Some(Resume { id, counts, .. }), _) => {
            let mut counts = counts
                .as_deref()
                .and_then(ImportCounts::from_json)
                .unwrap_or_default();
            counts.resumes = counts.resumes.saturating_add(1);
            tx.execute(
                "UPDATE import_session SET state = ?1, ended_at_ns = NULL, \
                     full_observation = NULL, resumes = resumes + 1, counts = ?2 \
                 WHERE id = ?3",
                rusqlite::params![SessionState::Running.code(), counts.to_json(), id],
            )
            .map_err(sql)?;
            (id, true, counts)
        }
        (None, source_id) => {
            let source_id = source_id.ok_or(crate::ArchiveError::Internal("no source_backup"))?;
            let counts = ImportCounts::default();
            tx.execute(
                "INSERT INTO import_session (source_backup_id, lineage_id, state, started_at_ns, \
                     app_version, schema_version, deriver_version, decoder_version, read_profile, \
                     content_digest, main_len, wal_outcome, wal_len, source_text_encoding, \
                     order_class, order_key, order_key_source, rank, qualifiers, \
                     ios_version_untested, include_recently_deleted, copy_attachments, \
                     capabilities, counts) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
                     ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
                rusqlite::params![
                    source_id,
                    lineage_id,
                    SessionState::Running.code(),
                    now_ns,
                    b.app_version,
                    b.schema_version,
                    b.deriver_version,
                    b.decoder_version,
                    b.read_profile.as_slice(),
                    b.digest.as_slice(),
                    b.main_len,
                    b.wal_outcome,
                    b.wal_len,
                    facts.encoding.map(TextEncodingCode::code),
                    if facts.order_key.is_some() {
                        OrderClass::Dated.code()
                    } else {
                        OrderClass::Undated.code()
                    },
                    facts.order_key,
                    facts.order_source.map(OrderKeySource::code),
                    facts.rank.as_bytes().as_slice(),
                    facts.qualifiers,
                    i64::from(facts.ios_version_untested),
                    i64::from(b.include_recently_deleted),
                    i64::from(b.copy_attachments),
                    facts.capabilities,
                    counts.to_json()
                ],
            )
            .map_err(sql)?;
            let id = tx.last_insert_rowid();
            {
                let mut ins = tx
                    .prepare(
                        "INSERT INTO import_dup_key (session_id, entity, key_digest) \
                         VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING",
                    )
                    .map_err(sql)?;
                for (e, d) in dups.rows() {
                    ins.execute(rusqlite::params![id, e.code(), d.as_slice()])
                        .map_err(sql)?;
                }
            }
            (id, false, counts)
        }
    };

    // 5. High-water marks.
    tx.execute(
        "UPDATE archive_meta SET max_deriver_version = max(max_deriver_version, ?1), \
             max_decoder_version = max(max_decoder_version, ?2) WHERE id = 1",
        rusqlite::params![b.deriver_version, b.decoder_version],
    )
    .map_err(sql)?;
    tx.commit().map_err(sql)?;
    crate::fault::point(crate::fault::FaultPoint::SessionCommitted);
    Ok(Started {
        session,
        lineage: lineage_id,
        resumed,
        superseded,
        counts,
    })
}

/// Record a terminal state other than completed: cancelled (2), failed (3)
/// or stopped (6), with the counts committed so far and, for a stop or
/// failure, its reason in `import_issue`. A failed session's session-local
/// rows are deleted in the same transaction. Best effort: if this fails, the
/// session stays running and the next write open marks it interrupted,
/// which is just as resumable.
pub(crate) fn end_session(
    conn: &mut Connection,
    session: i64,
    state: SessionState,
    reason: Option<(i64, IssueCode)>,
    counts: &ImportCounts,
    now_ns: i64,
) -> bool {
    let mut run = || -> rusqlite::Result<()> {
        if !conn.is_autocommit() {
            conn.execute_batch("ROLLBACK")?;
        }
        let tx = rusqlite::Transaction::new(conn, TransactionBehavior::Immediate)?;
        let n = tx.execute(
            "UPDATE import_session SET state = ?1, ended_at_ns = ?2, full_observation = 0, \
                 counts = ?3 WHERE id = ?4 AND state = ?5",
            rusqlite::params![
                state.code(),
                now_ns,
                counts.to_json(),
                session,
                SessionState::Running.code()
            ],
        )?;
        if let Some((pass, code)) = reason {
            tx.execute(
                "INSERT INTO import_issue (session_id, pass, kind, count) VALUES (?1, ?2, ?3, 1) \
                 ON CONFLICT (session_id, pass, kind) DO UPDATE SET count = count + 1",
                rusqlite::params![session, pass, code.code()],
            )?;
        }
        // A failed session is never resumed (§8.2 step 1): its session-local
        // rows go with the state that ends it.
        if state == SessionState::Failed && n == 1 {
            drop_session_local(&tx, session)?;
        }
        tx.commit()
    };
    match run() {
        Ok(()) => {
            crate::fault::point(crate::fault::FaultPoint::TerminalCommitted);
            true
        }
        Err(e) => {
            tracing::warn!(
                code = ?crate::error::primary_code(&e),
                "could not record the session's end; it stays resumable"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_dates_convert_to_unix_ns() {
        assert_eq!(unix_ns(UNIX_EPOCH), Some(0));
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_769_940_000);
        assert_eq!(unix_ns(t), Some(1_769_940_000_000_000_000));
        let before = UNIX_EPOCH - std::time::Duration::from_secs(1);
        assert_eq!(unix_ns(before), Some(-1_000_000_000));
        // Year 9999 does not fit in i64 nanoseconds.
        let far = UNIX_EPOCH + std::time::Duration::from_secs(253_402_300_799);
        assert_eq!(unix_ns(far), None);
    }

    #[test]
    fn the_read_profile_is_stable_and_observed_keys_differ() {
        assert_eq!(read_profile(), read_profile());
        let a = SourceBackup::default();
        let b = SourceBackup {
            encrypted: true,
            ..SourceBackup::default()
        };
        let c = SourceBackup {
            device_name: RawText::Value(String::new()),
            ..SourceBackup::default()
        };
        assert_ne!(a.observed_key(), b.observed_key());
        assert_ne!(a.observed_key(), c.observed_key());
        // Absent, wrong type, oversized and a value never share a key, and
        // names that differ only after 127 characters or by a control or
        // bidi mark stay distinct (nothing is sanitised or truncated).
        let name = |t: RawText| SourceBackup {
            device_name: t,
            ..SourceBackup::default()
        };
        let long = "n".repeat(200);
        let mut longer = long.clone();
        longer.push('m');
        let keys = [
            name(RawText::Absent).observed_key(),
            name(RawText::WrongType).observed_key(),
            name(RawText::Oversized { bytes: 5000 }).observed_key(),
            name(RawText::Value(long)).observed_key(),
            name(RawText::Value(longer)).observed_key(),
            name(RawText::Value("Phone".into())).observed_key(),
            name(RawText::Value("Pho\u{202E}ne".into())).observed_key(),
        ];
        for (i, k) in keys.iter().enumerate() {
            for l in keys.iter().skip(i + 1) {
                assert_ne!(k, l);
            }
        }
        let wrong_date = SourceBackup {
            status_date_wrong_type: true,
            ..SourceBackup::default()
        };
        assert_ne!(a.observed_key(), wrong_date.observed_key());
        // The marker columns.
        let marked = SourceBackup {
            device_name: RawText::WrongType,
            build_version: RawText::Oversized { bytes: 5000 },
            snapshot_state_raw: RawText::Oversized { bytes: 5000 },
            is_full_backup_wrong_type: true,
            ..SourceBackup::default()
        };
        assert_eq!(
            marked.wrong_type_fields(),
            source_field::DEVICE_NAME | source_field::IS_FULL_BACKUP
        );
        assert_eq!(
            marked.oversized_fields(),
            source_field::BUILD_VERSION | source_field::SNAPSHOT_STATE
        );
        assert_eq!((a.wrong_type_fields(), a.oversized_fields()), (0, 0));
    }
}
