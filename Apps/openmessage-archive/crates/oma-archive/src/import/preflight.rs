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
//! The Recently Deleted scans, the attachment estimate and the gate come
//! with the passes that need them (slices 4g–4j).

use std::ops::ControlFlow;

use oma_messages::{Cursor, HandleKeyRow, KeyRow, MessagesCtx, MessagesError, SmsDb};
use rusqlite::Connection;

use super::bind::Cell;
use super::keys::{guid_key_encoding, handle_natural_key, key_digest};
use crate::schema::codes::{Entity, SessionState};

/// The most rows of one keyed table the key scan reads (the D46 cap).
pub const KEY_SCAN_CAP: u64 = 4_194_304;

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

/// The digests that occur more than once in `all` (sorted in place).
fn duplicated(all: &mut [[u8; 16]]) -> Vec<[u8; 16]> {
    all.sort_unstable();
    let mut out: Vec<[u8; 16]> = Vec::new();
    for w in all.windows(2) {
        if let [a, b] = w {
            if a == b && out.last() != Some(a) {
                out.push(*a);
            }
        }
    }
    out
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

/// What the key scan found.
pub(crate) enum KeyScan {
    Done(DupSet),
    /// More than [`KEY_SCAN_CAP`] rows in this entity's table.
    OverLimit(Entity),
}

/// The key scan of §8.2 step 3.
pub(crate) fn scan_keys(
    db: &SmsDb<'_>,
    ctx: &mut MessagesCtx<'_>,
) -> Result<KeyScan, MessagesError> {
    let mut out = DupSet::default();
    for entity in KEYED {
        let mut digests: Vec<[u8; 16]> = Vec::new();
        let mut rows = 0u64;
        let mut over = false;
        {
            let mut push = |d: Option<[u8; 16]>| -> ControlFlow<()> {
                rows = rows.saturating_add(1);
                if rows > KEY_SCAN_CAP {
                    over = true;
                    return ControlFlow::Break(());
                }
                if let Some(d) = d {
                    digests.push(d);
                }
                ControlFlow::Continue(())
            };
            match entity {
                Entity::Handle => {
                    db.for_each_handle_key(Cursor::Start, ctx, |r: HandleKeyRow| {
                        let a = Cell::text(&r.id);
                        let s = Cell::text(&r.service);
                        push(handle_digest(&a, &s).map(|(d, _)| d))
                    })?;
                }
                Entity::Chat => {
                    db.for_each_chat_key(Cursor::Start, ctx, |r: KeyRow| {
                        push(guid_digest(&Cell::text(&r.guid)).map(|(d, _)| d))
                    })?;
                }
                Entity::Message => {
                    db.for_each_message_key(Cursor::Start, ctx, |r: KeyRow| {
                        push(guid_digest(&Cell::text(&r.guid)).map(|(d, _)| d))
                    })?;
                }
                _ => {
                    db.for_each_attachment_key(Cursor::Start, ctx, |r: KeyRow| {
                        push(guid_digest(&Cell::text(&r.guid)).map(|(d, _)| d))
                    })?;
                }
            }
        }
        if over {
            return Ok(KeyScan::OverLimit(entity));
        }
        let dups = duplicated(&mut digests);
        if let Some(s) = slot(entity).and_then(|i| out.sets.get_mut(i)) {
            *s = dups;
        }
    }
    Ok(KeyScan::Done(out))
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

/// The resume decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResumeDecision {
    /// The session to continue, with its stored counts JSON.
    pub(crate) resume: Option<(i64, Option<String>)>,
    /// Unfinished sessions to supersede.
    pub(crate) supersede: Vec<i64>,
}

/// Decide whether an unfinished session continues (§8.2 step 1). The
/// newest matching candidate is continued; every other candidate is
/// superseded. A failed session (3) is never a candidate.
pub(crate) fn decide_resume(conn: &Connection, want: &Binding) -> rusqlite::Result<ResumeDecision> {
    let mut stmt = conn.prepare(
        "SELECT id, lineage_id, content_digest, main_len, wal_outcome, wal_len, app_version, \
                schema_version, deriver_version, decoder_version, read_profile, \
                include_recently_deleted, copy_attachments, counts \
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
    };
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let lineage: i64 = r.get(1)?;
        let digest: Vec<u8> = r.get(2)?;
        let app: String = r.get(6)?;
        let profile: Vec<u8> = r.get(10)?;
        let matches = out.resume.is_none()
            && want.lineage == Some(lineage)
            && digest.as_slice() == want.digest.as_slice()
            && r.get::<_, i64>(3)? == want.main_len
            && r.get::<_, i64>(4)? == want.wal_outcome
            && r.get::<_, Option<i64>>(5)? == want.wal_len
            && app == want.app_version
            && r.get::<_, i64>(7)? == want.schema_version
            && r.get::<_, i64>(8)? == want.deriver_version
            && r.get::<_, i64>(9)? == want.decoder_version
            && profile.as_slice() == want.read_profile.as_slice()
            && r.get::<_, i64>(11)? == i64::from(want.include_recently_deleted)
            && r.get::<_, i64>(12)? == i64::from(want.copy_attachments);
        if matches {
            out.resume = Some((id, r.get(13)?));
        } else {
            out.supersede.push(id);
        }
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
        let mut all = vec![c, a, b, a, c, c];
        assert_eq!(duplicated(&mut all), vec![a, c]);
        let mut none = vec![a, b, c];
        assert!(duplicated(&mut none).is_empty());
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
