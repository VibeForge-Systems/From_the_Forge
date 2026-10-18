//! The preflight steps of the import core (spec §8.2): reads only.
//!
//! - **The key scan** (step 3, D83): for each of handle, chat, message and
//!   attachment, stream the keys (the R6 key passes), push the 128-bit
//!   truncated SHA-256 of each usable key's tagged encoding, sort, and keep
//!   the digests that occur more than once. Every row whose key digest is
//!   in that set is imported unkeyed. Only digests are kept, never a key.
//!   The cap is [`KEY_SCAN_CAP`] rows per entity, one entity in memory at a
//!   time (at most 64 MiB of digests).
//! - **The resume decision** (step 1): a session in state 0, 2, 4 or 6 is
//!   continued only when its digest framing, code versions, read profile,
//!   options and lineage all equal this import's; every other unfinished
//!   session is superseded.
//!
//! - **P0, the recoverable scan** (step 2, D60): the message rowids that
//!   `chat_recoverable_message_join` lists, sorted, capped at
//!   [`RECOVERABLE_SCAN_CAP`] join rows. It is re-run on every resume.
//! - **The Recently Deleted classification** (step 3, during the message
//!   key scan): with the default exclusion, a Recently Deleted row whose
//!   key is unique in this source and already archived is only re-observed
//!   ([`RdPlan::archived`]); every other Recently Deleted row, an unkeyed
//!   one included, is excluded ([`RdPlan::excluded`]). The archive lookup
//!   (one indexed read per message key on the read-only connection) runs
//!   one row at a time; no key outlives its row.
//!
//! The key scan also tallies, per keyed table, the keyable, unkeyed and
//! duplicated keys, and for messages the keys already archived: what the
//! gate shows (`gate.rs`).

use std::ops::ControlFlow;

use oma_messages::{
    Col, Cursor, HandleKeyRow, KeyRow, MessagesCtx, MessagesError, RecoverableLink, SmsDb,
};
use rusqlite::Connection;

use super::bind::Cell;
use super::keys::{guid_key_encoding, handle_natural_key, key_digest, Rank};
use crate::schema::codes::{Entity, SessionState};

/// The most rows of one keyed table the key scan reads (the D46 cap).
pub const KEY_SCAN_CAP: u64 = 4_194_304;

/// The most `chat_recoverable_message_join` rows P0 reads (the D46 cap).
/// Over it the backup is not importable: the exclusion could not be
/// applied (spec §23).
pub const RECOVERABLE_SCAN_CAP: u64 = 4_194_304;

/// What P0 found.
pub(crate) enum RecoverableScan {
    /// The distinct message rowids in Recently Deleted, sorted.
    Done(Vec<i64>),
    /// More than [`RECOVERABLE_SCAN_CAP`] join rows.
    OverLimit,
}

/// P0 (spec §8.2 step 2): the message rowids in Recently Deleted. A join
/// row whose `message_id` is not a readable integer names no message and
/// is not counted here (P8 counts it). `part_text` is never read: the
/// reader's authorizer denies it.
pub(crate) fn scan_recoverable(
    db: &SmsDb<'_>,
    ctx: &mut MessagesCtx<'_>,
) -> Result<RecoverableScan, MessagesError> {
    let mut rows: Vec<i64> = Vec::new();
    let mut seen = 0u64;
    let mut over = false;
    db.for_each_recoverable(Cursor::Start, ctx, |r: RecoverableLink| {
        seen = seen.saturating_add(1);
        if seen > RECOVERABLE_SCAN_CAP {
            over = true;
            return ControlFlow::Break(());
        }
        if let Col::Value(m) = r.message_id {
            rows.push(m);
        }
        ControlFlow::Continue(())
    })?;
    if over {
        return Ok(RecoverableScan::OverLimit);
    }
    rows.sort_unstable();
    rows.dedup();
    Ok(RecoverableScan::Done(rows))
}

/// How P3 treats the Recently Deleted rows of this source (spec §8.8).
/// Every list is sorted. Its `Debug` prints sizes only.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct RdPlan {
    /// P0: the message rowids in Recently Deleted.
    pub(crate) rd: Vec<i64>,
    /// Not imported (default exclusion): no row, no map, no join.
    pub(crate) excluded: Vec<i64>,
    /// Already archived (default exclusion): mapped and re-observed for
    /// the lineage only; no column, body or rank changes.
    pub(crate) archived: Vec<i64>,
}

impl std::fmt::Debug for RdPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RdPlan(rd {}, excluded {}, archived {})",
            self.rd.len(),
            self.excluded.len(),
            self.archived.len()
        )
    }
}

impl RdPlan {
    pub(crate) fn is_rd(&self, rowid: i64) -> bool {
        self.rd.binary_search(&rowid).is_ok()
    }

    pub(crate) fn is_excluded(&self, rowid: i64) -> bool {
        self.excluded.binary_search(&rowid).is_ok()
    }

    pub(crate) fn is_archived(&self, rowid: i64) -> bool {
        self.archived.binary_search(&rowid).is_ok()
    }
}

/// The key digests that occur more than once in this source, per entity.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct DupSet {
    /// Sorted, per entity code 1–4.
    sets: [Vec<[u8; 16]>; 4],
}

impl std::fmt::Debug for DupSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n: Vec<usize> = self.sets.iter().map(Vec::len).collect();
        write!(f, "DupSet({n:?})")
    }
}

fn slot(entity: Entity) -> Option<usize> {
    match entity {
        Entity::Handle => Some(0),
        Entity::Chat => Some(1),
        Entity::Message => Some(2),
        Entity::Attachment => Some(3),
        _ => None,
    }
}

const KEYED: [Entity; 4] = [
    Entity::Handle,
    Entity::Chat,
    Entity::Message,
    Entity::Attachment,
];

impl DupSet {
    /// Whether `digest` is a duplicated key of `entity`.
    pub(crate) fn contains(&self, entity: Entity, digest: &[u8; 16]) -> bool {
        slot(entity)
            .and_then(|i| self.sets.get(i))
            .is_some_and(|s| s.binary_search(digest).is_ok())
    }

    /// Every (entity, digest), for `import_dup_key`.
    pub(crate) fn rows(&self) -> impl Iterator<Item = (Entity, &[u8; 16])> {
        KEYED
            .iter()
            .zip(self.sets.iter())
            .flat_map(|(e, s)| s.iter().map(move |d| (*e, d)))
    }

    /// Add one stored row (resume).
    pub(crate) fn insert(&mut self, entity: Entity, digest: [u8; 16]) {
        if let Some(s) = slot(entity).and_then(|i| self.sets.get_mut(i)) {
            if let Err(at) = s.binary_search(&digest) {
                s.insert(at, digest);
            }
        }
    }

    /// The number of duplicated keys of `entity`.
    #[cfg(test)]
    pub(crate) fn len(&self, entity: Entity) -> usize {
        slot(entity)
            .and_then(|i| self.sets.get(i))
            .map_or(0, Vec::len)
    }
}

/// One key-scan entry: the 16-byte key digest and a flag byte (1: the key
/// is already a keyed row of the archive; messages only).
type Entry = [u8; 17];

/// Sort `all` and return the digests that occur more than once, the rows
/// carrying them, and the rows whose key is unique here and flagged.
fn tally(all: &mut [Entry]) -> (Vec<[u8; 16]>, u64, u64) {
    all.sort_unstable();
    let mut dups: Vec<[u8; 16]> = Vec::new();
    let mut dup_rows = 0u64;
    let mut flagged = 0u64;
    for group in all.chunk_by(|a, b| a.first_chunk::<16>() == b.first_chunk::<16>()) {
        let Some(first) = group.first() else { continue };
        if group.len() > 1 {
            if let Some(d) = first.first_chunk::<16>() {
                dups.push(*d);
            }
            dup_rows = dup_rows.saturating_add(u64::try_from(group.len()).unwrap_or(u64::MAX));
        } else if first.last() == Some(&1) {
            flagged = flagged.saturating_add(1);
        }
    }
    (dups, dup_rows, flagged)
}

fn entry(digest: [u8; 16], flag: bool) -> Entry {
    let mut e = [0u8; 17];
    for (dst, src) in e.iter_mut().zip(digest.iter()) {
        *dst = *src;
    }
    if let Some(last) = e.last_mut() {
        *last = u8::from(flag);
    }
    e
}

/// The tagged-key digest of a GUID column, when it is a usable key: a
/// readable, non-empty value (an empty GUID is no identifier).
pub(crate) fn guid_digest(guid: &Cell) -> Option<([u8; 16], Vec<u8>)> {
    let b = guid.value.as_ref()?.as_bytes()?;
    if b.is_empty() {
        return None;
    }
    Some((key_digest(&guid_key_encoding(b)), b.to_vec()))
}

/// A handle's natural key and its digest, when the address is a readable,
/// non-empty value.
pub(crate) fn handle_digest(address: &Cell, service: &Cell) -> Option<([u8; 16], Vec<u8>)> {
    let b = address.value.as_ref()?.as_bytes()?;
    if b.is_empty() {
        return None;
    }
    let nk = handle_natural_key(b, service.field());
    Some((key_digest(&nk), nk))
}

/// What the key scan counted for one keyed table (spec §8.2 step 3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct KeyTally {
    /// Rows read.
    pub(crate) rows: u64,
    /// Rows with a usable key (a readable, non-empty identifier).
    pub(crate) keyable: u64,
    /// Distinct keys that occur more than once.
    pub(crate) duplicated_keys: u64,
    /// Rows imported unkeyed: no usable key, or a duplicated one.
    pub(crate) unkeyed: u64,
    /// Messages only: rows whose key is unique here and already a keyed
    /// message of the archive (the overlap the gate shows).
    pub(crate) already_archived: u64,
}

/// What the key scan found.
#[allow(clippy::large_enum_variant)] // made once per import
pub(crate) enum KeyScan {
    /// The duplicate set, the Recently Deleted plan, and the tallies of
    /// handle, chat, message and attachment keys.
    Done(DupSet, RdPlan, [KeyTally; 4]),
    /// More than [`KEY_SCAN_CAP`] rows in this entity's table.
    OverLimit(Entity),
}

/// Why the key scan stopped.
pub(crate) enum ScanError {
    /// Reading the source.
    Read(MessagesError),
    /// The archive lookup of a message key.
    Archive(rusqlite::Error),
}

impl From<MessagesError> for ScanError {
    fn from(e: MessagesError) -> Self {
        ScanError::Read(e)
    }
}

/// One Recently Deleted message row, as the key scan saw it.
struct RdRow {
    rowid: i64,
    /// The key digest, when the GUID is a usable key.
    digest: Option<[u8; 16]>,
    /// The key is already archived as a keyed message.
    archived: bool,
}

/// The key scan of §8.2 step 3. `rd` is P0's set. Every message row with a
/// usable key asks `is_archived` whether that key is already a keyed
/// message in the archive (one indexed lookup on the read-only connection;
/// the key is not kept past its row).
pub(crate) fn scan_keys(
    db: &SmsDb<'_>,
    ctx: &mut MessagesCtx<'_>,
    rd: Vec<i64>,
    exclude_rd: bool,
    is_archived: &mut dyn FnMut(&[u8]) -> rusqlite::Result<bool>,
) -> Result<KeyScan, ScanError> {
    let mut out = DupSet::default();
    let mut tallies = [KeyTally::default(); 4];
    let mut rd_rows: Vec<RdRow> = Vec::new();
    let mut lookup_err: Option<rusqlite::Error> = None;
    for entity in KEYED {
        let mut entries: Vec<Entry> = Vec::new();
        let mut rows = 0u64;
        let mut over = false;
        {
            let mut push = |d: Option<([u8; 16], bool)>| -> ControlFlow<()> {
                rows = rows.saturating_add(1);
                if rows > KEY_SCAN_CAP {
                    over = true;
                    return ControlFlow::Break(());
                }
                if let Some((d, flag)) = d {
                    entries.push(entry(d, flag));
                }
                ControlFlow::Continue(())
            };
            match entity {
                Entity::Handle => {
                    db.for_each_handle_key(Cursor::Start, ctx, |r: HandleKeyRow| {
                        let a = Cell::text(&r.id);
                        let s = Cell::text(&r.service);
                        push(handle_digest(&a, &s).map(|(d, _)| (d, false)))
                    })?;
                }
                Entity::Chat => {
                    db.for_each_chat_key(Cursor::Start, ctx, |r: KeyRow| {
                        push(guid_digest(&Cell::text(&r.guid)).map(|(d, _)| (d, false)))
                    })?;
                }
                Entity::Message => {
                    db.for_each_message_key(Cursor::Start, ctx, |r: KeyRow| {
                        let key = guid_digest(&Cell::text(&r.guid));
                        let archived = match &key {
                            Some((_, k)) => match is_archived(k) {
                                Ok(a) => a,
                                Err(e) => {
                                    lookup_err = Some(e);
                                    return ControlFlow::Break(());
                                }
                            },
                            None => false,
                        };
                        if exclude_rd && rd.binary_search(&r.rowid).is_ok() {
                            rd_rows.push(RdRow {
                                rowid: r.rowid,
                                digest: key.as_ref().map(|(d, _)| *d),
                                archived,
                            });
                        }
                        push(key.map(|(d, _)| (d, archived)))
                    })?;
                    if let Some(e) = lookup_err.take() {
                        return Err(ScanError::Archive(e));
                    }
                }
                _ => {
                    db.for_each_attachment_key(Cursor::Start, ctx, |r: KeyRow| {
                        push(guid_digest(&Cell::text(&r.guid)).map(|(d, _)| (d, false)))
                    })?;
                }
            }
        }
        if over {
            return Ok(KeyScan::OverLimit(entity));
        }
        let keyable = u64::try_from(entries.len()).unwrap_or(u64::MAX);
        let (dups, dup_rows, archived) = tally(&mut entries);
        drop(entries);
        if let Some(t) = slot(entity).and_then(|i| tallies.get_mut(i)) {
            *t = KeyTally {
                rows,
                keyable,
                duplicated_keys: u64::try_from(dups.len()).unwrap_or(u64::MAX),
                unkeyed: rows.saturating_sub(keyable).saturating_add(dup_rows),
                already_archived: archived,
            };
        }
        if let Some(s) = slot(entity).and_then(|i| out.sets.get_mut(i)) {
            *s = dups;
        }
    }
    // A Recently Deleted row is only re-observed when its key is unique in
    // this source and archived; every other one is excluded (spec §8.2
    // step 3: an unkeyed row counts as excluded).
    let mut plan = RdPlan {
        rd,
        ..RdPlan::default()
    };
    for r in rd_rows {
        let keyed = r.digest.is_some_and(|d| !out.contains(Entity::Message, &d));
        if keyed && r.archived {
            plan.archived.push(r.rowid);
        } else {
            plan.excluded.push(r.rowid);
        }
    }
    plan.archived.sort_unstable();
    plan.excluded.sort_unstable();
    Ok(KeyScan::Done(out, plan, tallies))
}

/// The stored `import_dup_key` rows of a session being continued.
pub(crate) fn load_dup_keys(conn: &Connection, session: i64) -> rusqlite::Result<DupSet> {
    let mut out = DupSet::default();
    let mut stmt =
        conn.prepare("SELECT entity, key_digest FROM import_dup_key WHERE session_id = ?1")?;
    let mut rows = stmt.query([session])?;
    while let Some(r) = rows.next()? {
        let e: i64 = r.get(0)?;
        let d: Vec<u8> = r.get(1)?;
        if let (Some(e), Ok(d)) = (Entity::from_code(e), <[u8; 16]>::try_from(d.as_slice())) {
            out.insert(e, d);
        }
    }
    Ok(out)
}

/// What a resume must match (spec §8.2 step 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Binding {
    pub(crate) digest: [u8; 32],
    pub(crate) main_len: i64,
    pub(crate) wal_outcome: i64,
    pub(crate) wal_len: Option<i64>,
    pub(crate) app_version: &'static str,
    pub(crate) schema_version: i64,
    pub(crate) deriver_version: i64,
    pub(crate) decoder_version: i64,
    pub(crate) read_profile: [u8; 32],
    pub(crate) include_recently_deleted: bool,
    pub(crate) copy_attachments: bool,
    /// `None` for a new lineage, which no stored session can match.
    pub(crate) lineage: Option<i64>,
}

/// A session to continue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resume {
    pub(crate) id: i64,
    /// Its lineage.
    pub(crate) lineage: i64,
    /// Its stored counts JSON.
    pub(crate) counts: Option<String>,
    /// Its stored rank. A continued session writes every row at this rank,
    /// never at the one computed for this attempt: the order key is not
    /// part of the binding (spec §8.2 step 1), and a session merges at one
    /// rank (§8.7).
    pub(crate) rank: Rank,
}

/// The resume decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResumeDecision {
    /// The session to continue.
    pub(crate) resume: Option<Resume>,
    /// Unfinished sessions to supersede.
    pub(crate) supersede: Vec<i64>,
    /// Nothing continues, but an unfinished session matches in everything
    /// except the two import options (D64): the user changed them.
    pub(crate) options_differ: bool,
}

/// Decide whether an unfinished session continues (§8.2 step 1). The
/// newest matching candidate is continued; every other candidate is
/// superseded. A failed session (3) is never a candidate. A candidate
/// whose stored rank cannot be read is superseded.
///
/// With `any_lineage`, the lineage is not compared: the preflight asks
/// which session *could* continue before the user picks the lineage at the
/// gate; the decision that counts is made again with the chosen one.
pub(crate) fn decide_resume(
    conn: &Connection,
    want: &Binding,
    any_lineage: bool,
) -> rusqlite::Result<ResumeDecision> {
    let mut stmt = conn.prepare(
        "SELECT id, lineage_id, content_digest, main_len, wal_outcome, wal_len, app_version, \
                schema_version, deriver_version, decoder_version, read_profile, \
                include_recently_deleted, copy_attachments, counts, rank \
         FROM import_session WHERE state IN (?1, ?2, ?3, ?4) ORDER BY id DESC",
    )?;
    let mut rows = stmt.query(rusqlite::params![
        SessionState::Running.code(),
        SessionState::Cancelled.code(),
        SessionState::Interrupted.code(),
        SessionState::Stopped.code()
    ])?;
    let mut out = ResumeDecision {
        resume: None,
        supersede: Vec::new(),
        options_differ: false,
    };
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let lineage: i64 = r.get(1)?;
        let digest: Vec<u8> = r.get(2)?;
        let app: String = r.get(6)?;
        let profile: Vec<u8> = r.get(10)?;
        let rank: Option<Rank> = Rank::from_slice(&r.get::<_, Vec<u8>>(14)?);
        let same_source = out.resume.is_none()
            && rank.is_some()
            && (any_lineage || want.lineage == Some(lineage))
            && digest.as_slice() == want.digest.as_slice()
            && r.get::<_, i64>(3)? == want.main_len
            && r.get::<_, i64>(4)? == want.wal_outcome
            && r.get::<_, Option<i64>>(5)? == want.wal_len
            && app == want.app_version
            && r.get::<_, i64>(7)? == want.schema_version
            && r.get::<_, i64>(8)? == want.deriver_version
            && r.get::<_, i64>(9)? == want.decoder_version
            && profile.as_slice() == want.read_profile.as_slice();
        let same_options = r.get::<_, i64>(11)? == i64::from(want.include_recently_deleted)
            && r.get::<_, i64>(12)? == i64::from(want.copy_attachments);
        let matches = same_source && same_options;
        if same_source && !same_options {
            out.options_differ = true;
        }
        match (matches, rank) {
            (true, Some(rank)) => {
                out.resume = Some(Resume {
                    id,
                    lineage,
                    counts: r.get(13)?,
                    rank,
                });
            }
            _ => out.supersede.push(id),
        }
    }
    if out.resume.is_some() {
        out.options_differ = false;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicates_are_found_once_each() {
        let a = [1u8; 16];
        let b = [2u8; 16];
        let c = [3u8; 16];
        let d = [4u8; 16];
        // The flag never splits a group: `a` is duplicated whether or not
        // one occurrence is archived.
        let mut all = vec![
            entry(c, false),
            entry(a, true),
            entry(b, false),
            entry(a, false),
            entry(c, false),
            entry(c, false),
            entry(d, true),
        ];
        assert_eq!(tally(&mut all), (vec![a, c], 5, 1));
        let mut none = vec![entry(a, false), entry(b, false), entry(c, false)];
        assert_eq!(tally(&mut none), (Vec::new(), 0, 0));
        let mut set = DupSet::default();
        set.insert(Entity::Chat, b);
        set.insert(Entity::Chat, a);
        set.insert(Entity::Chat, a);
        assert!(set.contains(Entity::Chat, &a));
        assert!(!set.contains(Entity::Handle, &a));
        assert!(!set.contains(Entity::MessageBody, &a));
        assert_eq!(set.len(Entity::Chat), 2);
        assert_eq!(set.rows().count(), 2);
        assert_eq!(format!("{set:?}"), "DupSet([0, 2, 0, 0])");
    }

    #[test]
    fn empty_and_unreadable_identifiers_are_no_key() {
        use oma_messages::{Col, IssueKind, Redacted, SqlText};
        let t = |s: &[u8]| Cell::text(&Col::Value(Redacted::new(SqlText::from_bytes(s))));
        assert!(guid_digest(&t(b"")).is_none());
        assert!(guid_digest(&Cell::text(&Col::Null)).is_none());
        assert!(guid_digest(&Cell::text(&Col::Unreadable(IssueKind::TypeMismatch))).is_none());
        assert!(guid_digest(&Cell::text(&Col::Oversized { bytes: 999 })).is_none());
        let (d, k) = guid_digest(&t(b"G")).expect("key");
        assert_eq!(k, b"G");
        assert_eq!(d, key_digest(&guid_key_encoding(b"G")));
        // A handle's service state separates keys.
        let a = t(b"+15550000001");
        let null = handle_digest(&a, &Cell::text(&Col::Null)).expect("key");
        let sms = handle_digest(&a, &t(b"SMS")).expect("key");
        assert_ne!(null.0, sms.0);
        assert!(handle_digest(&t(b""), &t(b"SMS")).is_none());
    }
}
