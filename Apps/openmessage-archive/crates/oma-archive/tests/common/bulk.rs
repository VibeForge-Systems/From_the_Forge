//! The bulk generator for scale (M5 spec §14.2): a synthetic schema-v1
//! archive of any size, written directly through
//! [`oma_archive::hooks::bulk_writer`] instead of through an import.
//!
//! - **Batches** are import-sized: [`BatchBudget::ROWS_DEFAULT`] messages
//!   per transaction, so FTS5's automerge sees the segment pattern an
//!   import leaves.
//! - **Insert order**: each `message` row is inserted with no current body,
//!   then its `message_body` (whose `message_id` must exist: that foreign
//!   key is immediate), then the message's `current_body_id` is set. A
//!   deferred `current_body_id` pointing at a body inserted later made
//!   bulk loading quadratic (observed in scratch, M5 spec §14.2).
//! - **Finalize**: the import's own `chat_stats`, `archive_stats` and FTS
//!   merge steps, then a `TRUNCATE` checkpoint
//!   ([`oma_archive::hooks::BulkWriter::finalize`]).
//! - **Keys**: `sort_key` comes from the import's encoder
//!   ([`oma_archive::hooks::sort_key`]).
//! - **Data**: seeded text in Latin words, CJK and emoji mixes; reactions
//!   (to an earlier message of the same chat) and replies at fixed ratios;
//!   undated rows; group chats; attachments without blobs (no store
//!   files) by default, or, with [`BulkSpec::blobs`] (M6 slice 6c), each
//!   with its own store file of [`BulkSpec::blob_len`] bytes, placed
//!   through [`oma_archive::hooks::BulkWriter::put_blob`] and recorded as
//!   copied. One session, completed and a full observation.
//!
//! Every value is synthetic. Every column a row leaves unset is recorded as
//! "NULL in the source" (`_st = 1`), which every `CHECK` of v1 accepts.
//! Test-only: the hook exists only with the `fault-injection` feature.
#![allow(dead_code, clippy::disallowed_methods, clippy::disallowed_types)]

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use oma_archive::hooks::{bulk_writer, sort_key};
use oma_archive::schema::codes::{
    AssocKindCode, AssocPresent, AssocTargetShape, AttributedOutcome, CopyCode, DisplaySource,
    EditLabelCode, ItemKindCode, KeyKind, LookupCode, OrderClass, OrderKeySource, PathClassCode,
    ServiceClassCode, SessionState, SummaryOutcome, TimeUnit, WalOutcomeCode,
};
use oma_archive::{Archive, BatchBudget, DECODER_VERSION, DERIVER_VERSION, SCHEMA_VERSION};
use rusqlite::types::Value;
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use super::import::{Rng, MODEL_NOW_NS};

/// What to generate. Ratios are per mille of messages.
#[derive(Debug, Clone)]
pub struct BulkSpec {
    pub seed: u64,
    pub messages: usize,
    pub chats: usize,
    pub handles: usize,
    /// Share of messages in chat 1 (one very long conversation).
    pub big_chat_permille: u64,
    pub reaction_permille: u64,
    pub reply_permille: u64,
    pub undated_permille: u64,
    pub cjk_permille: u64,
    pub emoji_permille: u64,
    pub attachment_permille: u64,
    /// Every n-th chat is a group of three addresses.
    pub group_every: usize,
    /// Messages per transaction.
    pub batch_rows: usize,
    /// Write the import-based perf generator's bodies (`SYNTH-BODY-<n>
    /// café`) instead of the mixed text, so the calibration compares like
    /// with like.
    pub sentinel_bodies: bool,
    /// Give every attachment a store file (a `blob` row, the observation
    /// `Present`/`Copied`), instead of none.
    pub blobs: bool,
    /// The size of each store file with [`BulkSpec::blobs`]: a JPEG start
    /// and end around a sentinel and filler, so the sniffer reads JPEG.
    pub blob_len: usize,
}

impl BulkSpec {
    /// The defaults of spec §3.5's scratch archive: 5 % reactions, 2 % CJK
    /// bodies, one chat with 40 % of the messages.
    pub fn new(messages: usize) -> BulkSpec {
        BulkSpec {
            seed: 5,
            messages,
            chats: (messages / 250).max(4),
            handles: (messages / 200).max(8),
            big_chat_permille: 400,
            reaction_permille: 50,
            reply_permille: 20,
            undated_permille: 2,
            cjk_permille: 20,
            emoji_permille: 10,
            attachment_permille: 10,
            group_every: 10,
            batch_rows: BatchBudget::ROWS_DEFAULT as usize,
            sentinel_bodies: false,
            blobs: false,
            blob_len: 1024,
        }
    }
}

/// What [`generate`] wrote. Counts and times only.
#[derive(Debug, Clone, Default)]
pub struct BulkReport {
    pub messages: u64,
    pub reactions: u64,
    pub replies: u64,
    pub undated: u64,
    pub cjk: u64,
    pub attachments: u64,
    /// Store files written ([`BulkSpec::blobs`]) and their bytes.
    pub blobs: u64,
    pub blob_bytes: u64,
    pub batches: u64,
    pub fts_merge_steps: u32,
    pub fts_merge_complete: bool,
    pub write_time: Duration,
    pub finalize_time: Duration,
}

/// The session's rank: dated (class 1), the backup date, a digest.
fn rank(order_key: i64, digest: &[u8; 32]) -> [u8; 41] {
    let mut r = [0u8; 41];
    r[0] = 1;
    let biased = u64::from_be_bytes(order_key.to_be_bytes()) ^ (1u64 << 63);
    r[1..9].copy_from_slice(&biased.to_be_bytes());
    r[9..].copy_from_slice(digest);
    r
}

fn sha(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// `INSERT INTO table (every column) VALUES (...)` with a parameter for
/// each `provided` column, `1` (NULL in the source) for each `_st` whose
/// value column is not provided, and NULL elsewhere. Parameters follow
/// `provided`'s order.
fn insert_sql(conn: &Connection, table: &str, provided: &[&str]) -> rusqlite::Result<String> {
    let cols: Vec<String> = conn
        .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let set: BTreeSet<&str> = provided.iter().copied().collect();
    for p in provided {
        assert!(cols.iter().any(|c| c == p), "{table} has no column {p}");
    }
    let value_col = |st: &str| -> String {
        let base = &st[..st.len() - 3];
        if cols.iter().any(|c| c == base) {
            base.to_owned()
        } else {
            format!("{base}_raw")
        }
    };
    let mut values = Vec::new();
    for c in &cols {
        let v = if let Some(i) = provided.iter().position(|p| p == c) {
            format!("?{}", i + 1)
        } else if c.ends_with("_st") && !set.contains(value_col(c).as_str()) {
            "1".to_owned()
        } else {
            "NULL".to_owned()
        };
        values.push(v);
    }
    Ok(format!(
        "INSERT INTO {table} ({}) VALUES ({})",
        cols.join(", "),
        values.join(", ")
    ))
}

fn exec(conn: &Connection, sql: &str, values: Vec<Value>) -> rusqlite::Result<()> {
    conn.prepare(sql)?
        .execute(rusqlite::params_from_iter(values))
        .map(|_| ())
}

/// Execute a prepared insert (statements are prepared once per batch: the
/// crate's connection has no statement cache).
fn run(stmt: &mut rusqlite::Statement<'_>, values: Vec<Value>) -> rusqlite::Result<()> {
    stmt.execute(rusqlite::params_from_iter(values)).map(|_| ())
}

fn int(v: i64) -> Value {
    Value::Integer(v)
}

fn blob(v: impl AsRef<[u8]>) -> Value {
    Value::Blob(v.as_ref().to_vec())
}

fn text(v: impl Into<String>) -> Value {
    Value::Text(v.into())
}

fn opt_int(v: Option<i64>) -> Value {
    v.map_or(Value::Null, Value::Integer)
}

fn st(present: bool) -> Value {
    if present {
        Value::Null
    } else {
        Value::Integer(1)
    }
}

/// Synthetic words (invented, not any natural-language corpus).
const WORDS: &[&str] = &[
    "lorem", "ipsum", "dolor", "sit", "amet", "vela", "noro", "tesa", "milo", "parun", "kesto",
    "dumo", "lira", "fenwa", "orto", "brisk", "calm", "delta", "ember", "frost", "grove", "harbor",
    "island", "jasper", "kettle", "lantern", "meadow", "nectar", "orbit", "pepper", "quartz",
    "river", "saddle", "timber", "umber", "valley", "willow", "xenon", "yonder", "zephyr",
];

fn body_text(rng: &mut Rng, i: usize, kind: u64) -> String {
    let n = 3 + rng.below(12) as usize;
    let mut out = format!("SYNTHBULK{i}");
    for _ in 0..n {
        out.push(' ');
        match kind {
            1 => {
                for _ in 0..(2 + rng.below(4)) {
                    let cp = 0x4E00 + u32::try_from(rng.below(2000)).expect("small");
                    out.push(char::from_u32(cp).expect("CJK"));
                }
            }
            2 => {
                let cp = 0x1F600 + u32::try_from(rng.below(0x50)).expect("small");
                out.push(char::from_u32(cp).expect("emoji"));
                out.push_str(WORDS[rng.below(WORDS.len() as u64) as usize]);
            }
            _ => out.push_str(WORDS[rng.below(WORDS.len() as u64) as usize]),
        }
    }
    out
}

/// One generated message, before it is written.
struct Msg {
    id: i64,
    guid: String,
    chat: i64,
    utc_ns: Option<i64>,
    from_me: bool,
    handle: Option<i64>,
    text: String,
    reaction: Option<(i64, String)>, // (tapback offset 0..=5, target guid)
    reply_to: Option<String>,
    /// The attachment's id, when the message has one.
    attachment: Option<i64>,
}

/// The bytes of attachment `id`'s store file: a JPEG start of image, a
/// sentinel, filler and an end of image, `len` bytes in all (at least the
/// markers and the sentinel).
pub fn blob_bytes(id: i64, len: usize) -> Vec<u8> {
    let mut b = vec![0xff, 0xd8, 0xff, 0xe0];
    b.extend_from_slice(format!("SYNTH-BULK-BLOB-{id}").as_bytes());
    let fill = len.saturating_sub(b.len() + 2);
    b.extend(std::iter::repeat_n(b'.', fill));
    b.extend_from_slice(&[0xff, 0xd9]);
    b
}

/// Write a bulk archive into `archive` (empty, freshly created) and run the
/// finalize steps.
pub fn generate(archive: &mut Archive, spec: &BulkSpec) -> BulkReport {
    let mut rng = Rng::new(spec.seed);
    let mut report = BulkReport::default();
    let t0 = Instant::now();
    let mut w = bulk_writer(archive).expect("bulk writer");
    let order_key = MODEL_NOW_NS - 3600 * 1_000_000_000;
    let digest = sha(&[b"oma-bulk-content", &spec.seed.to_be_bytes()]);
    let rk = rank(order_key, &digest);
    let session = 1i64;

    // The session, its backup and lineage, handles and chats.
    let chats = i64::try_from(spec.chats).expect("chats");
    let handles = i64::try_from(spec.handles).expect("handles");
    w.batch(|c| {
        exec(
            c,
            "INSERT INTO source_backup (id, observed_key, device_name, product_type, \
                 product_version, status_date_ns, encrypted) VALUES (1, ?1, ?2, 'iPhone0,0', '18.0', ?3, 0)",
            vec![
                blob(sha(&[b"oma-bulk-source"])),
                text("Synthetic Bulk Phone"),
                int(order_key),
            ],
        )?;
        exec(
            c,
            "INSERT INTO lineage (id, created_at_ns) VALUES (1, ?1)",
            vec![int(MODEL_NOW_NS)],
        )?;
        exec(
            c,
            "INSERT INTO import_session (id, source_backup_id, lineage_id, state, started_at_ns, \
                 ended_at_ns, app_version, schema_version, deriver_version, decoder_version, \
                 read_profile, content_digest, main_len, wal_outcome, order_class, order_key, \
                 order_key_source, rank, qualifiers, ios_version_untested, \
                 include_recently_deleted, copy_attachments, capabilities, full_observation) \
             VALUES (?1, 1, 1, ?2, ?3, ?3, 'bulk', ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10, ?11, ?12, ?13, \
                 0, 0, 0, 1, '{}', 1)",
            vec![
                int(session),
                int(SessionState::Completed.code()),
                int(MODEL_NOW_NS),
                int(i64::from(SCHEMA_VERSION)),
                int(i64::from(DERIVER_VERSION)),
                int(i64::from(DECODER_VERSION)),
                blob(sha(&[b"oma-bulk-read-profile"])),
                blob(digest),
                int(WalOutcomeCode::NotListed.code()),
                int(OrderClass::Dated.code()),
                int(order_key),
                int(OrderKeySource::StatusDate.code()),
                blob(rk),
            ],
        )?;
        // The high-water marks an import's session start raises.
        exec(
            c,
            "UPDATE archive_meta SET max_deriver_version = max(max_deriver_version, ?1), \
                 max_decoder_version = max(max_decoder_version, ?2) WHERE id = 1",
            vec![int(i64::from(DERIVER_VERSION)), int(i64::from(DECODER_VERSION))],
        )?;
        let hsql = insert_sql(
            c,
            "handle",
            &[
                "id", "key_kind", "natural_key", "address", "service", "service_class",
                "first_session_id", "first_rank", "last_session_id", "last_rank",
            ],
        )?;
        let mut h_st = c.prepare(&hsql)?;
        for h in 1..=handles {
            let addr = if h % 2 == 0 {
                format!("+1555{h:07}")
            } else {
                format!("synthetic-bulk-{h}@example.invalid")
            };
            run(&mut h_st,
                vec![
                    int(h),
                    int(KeyKind::Exact.code()),
                    blob(sha(&[b"handle", addr.as_bytes()])),
                    blob(addr.as_bytes()),
                    blob(b"iMessage"),
                    int(ServiceClassCode::IMessage.code()),
                    int(session),
                    blob(rk),
                    int(session),
                    blob(rk),
                ],
            )?;
        }
        let csql = insert_sql(
            c,
            "chat",
            &[
                "id", "key_kind", "guid", "style", "chat_identifier", "service_name",
                "service_class", "display_name", "display_name_st", "first_session_id",
                "first_rank", "last_session_id", "last_rank",
            ],
        )?;
        let chsql = insert_sql(
            c,
            "chat_handle",
            &["chat_id", "handle_id", "first_session_id", "last_session_id", "last_rank"],
        )?;
        let mut c_st = c.prepare(&csql)?;
        let mut ch_st = c.prepare(&chsql)?;
        for ch in 1..=chats {
            let group = spec.group_every > 0 && ch % i64::try_from(spec.group_every).expect("n") == 0;
            run(&mut c_st,
                vec![
                    int(ch),
                    int(KeyKind::Exact.code()),
                    blob(format!("SYNTH-BULK-CHAT-{ch}")),
                    int(if group { 43 } else { 45 }),
                    blob(format!("chatSYNTHBULK{ch}")),
                    blob(b"iMessage"),
                    int(ServiceClassCode::IMessage.code()),
                    if group { blob(format!("SYNTH-BULK-GROUP-{ch}")) } else { Value::Null },
                    st(group),
                    int(session),
                    blob(rk),
                    int(session),
                    blob(rk),
                ],
            )?;
            let members = if group { 3 } else { 1 };
            for k in 0..members {
                let h = (ch * 7 + k * 13) % handles + 1;
                run(&mut ch_st, vec![int(ch), int(h), int(session), int(session), blob(rk)])?;
            }
        }
        Ok(())
    })
    .expect("session, handles and chats");

    // Messages, in import-sized batches.
    let mut per_chat: Vec<Vec<(i64, String)>> = vec![Vec::new(); spec.chats + 1];
    let mut t = MODEL_NOW_NS - 400 * 86_400 * 1_000_000_000;
    let mut id = 0i64;
    let mut att_id = 0i64;
    let total = spec.messages;
    let mut done = 0usize;
    while done < total {
        let n = spec.batch_rows.min(total - done);
        let mut batch = Vec::with_capacity(n);
        for _ in 0..n {
            id += 1;
            let i = usize::try_from(id).expect("id");
            let chat = if rng.below(1000) < spec.big_chat_permille {
                1
            } else {
                1 + i64::try_from(rng.below(spec.chats as u64)).expect("chat")
            };
            t += 1_000_000_000 * i64::try_from(1 + rng.below(90)).expect("step");
            let undated = rng.below(1000) < spec.undated_permille;
            let from_me = rng.below(3) == 0;
            let handle = (!from_me).then(|| (chat * 7) % handles + 1);
            let earlier = &per_chat[usize::try_from(chat).expect("chat")];
            let roll = rng.below(1000);
            let reaction = (!earlier.is_empty() && roll < spec.reaction_permille).then(|| {
                let (_, g) = &earlier[usize::try_from(rng.below(earlier.len() as u64)).expect("i")];
                (i64::try_from(rng.below(6)).expect("k"), g.clone())
            });
            let reply_to = (reaction.is_none()
                && !earlier.is_empty()
                && rng.below(1000) < spec.reply_permille)
                .then(|| {
                    earlier[usize::try_from(rng.below(earlier.len() as u64)).expect("i")]
                        .1
                        .clone()
                });
            let kind = {
                let r = rng.below(1000);
                if r < spec.cjk_permille {
                    1
                } else if r < spec.cjk_permille + spec.emoji_permille {
                    2
                } else {
                    0
                }
            };
            let guid = format!("SYNTH-BULK-MSG-{id}");
            let text = match &reaction {
                Some((k, _)) => format!(
                    "{} \u{201c}SYNTHBULK{i}\u{201d}",
                    [
                        "Loved",
                        "Liked",
                        "Disliked",
                        "Laughed at",
                        "Emphasized",
                        "Questioned"
                    ][usize::try_from(*k).expect("k")]
                ),
                None if spec.sentinel_bodies => format!("SYNTH-BODY-{i} caf\u{e9}"),
                None => body_text(&mut rng, i, kind),
            };
            report.cjk += u64::from(reaction.is_none() && kind == 1);
            let attachment = (reaction.is_none() && rng.below(1000) < spec.attachment_permille)
                .then(|| {
                    att_id += 1;
                    att_id
                });
            if reaction.is_none() {
                per_chat[usize::try_from(chat).expect("chat")].push((id, guid.clone()));
            }
            batch.push(Msg {
                id,
                guid,
                chat,
                utc_ns: (!undated).then_some(t),
                from_me,
                handle,
                text,
                reaction,
                reply_to,
                attachment,
            });
        }
        // Store files first (the rows that name them follow in the batch).
        let mut placed: Vec<(i64, [u8; 32], i64)> = Vec::new();
        if spec.blobs {
            for a in batch.iter().filter_map(|m| m.attachment) {
                let bytes = blob_bytes(a, spec.blob_len);
                let sha = w.put_blob(&bytes).expect("store file");
                let len = i64::try_from(bytes.len()).expect("len");
                report.blobs += 1;
                report.blob_bytes += bytes.len() as u64;
                placed.push((a, sha, len));
            }
        }
        let placed_of: std::collections::BTreeMap<i64, ([u8; 32], i64)> =
            placed.iter().map(|(a, s, l)| (*a, (*s, *l))).collect();
        w.batch(|c| {
            let bsql_blob = "INSERT INTO blob (id, sha256, byte_len, storage_name, \
                             first_session_id) VALUES (?1, ?2, ?3, ?4, ?5)";
            let mut blob_st = c.prepare(bsql_blob)?;
            for (a, sha, len) in &placed {
                run(
                    &mut blob_st,
                    vec![
                        int(*a),
                        blob(sha),
                        int(*len),
                        text(super::hex(sha)),
                        int(session),
                    ],
                )?;
            }
            let msql = insert_sql(
                c,
                "message",
                &[
                    "id",
                    "key_kind",
                    "guid",
                    "is_from_me",
                    "handle_rowid",
                    "handle_ref",
                    "service",
                    "service_class",
                    "date_raw",
                    "date_unit",
                    "date_utc_ns",
                    "item_type",
                    "group_action_type",
                    "item_kind",
                    "assoc_type",
                    "assoc_guid_raw",
                    "assoc_guid_raw_st",
                    "assoc_present",
                    "assoc_kind",
                    "assoc_target_shape",
                    "assoc_target_guid",
                    "assoc_target_part",
                    "thread_originator_guid",
                    "thread_originator_guid_st",
                    "sort_key",
                    "first_src_rowid",
                    "derived_by",
                    "first_session_id",
                    "first_rank",
                    "last_session_id",
                    "last_rank",
                ],
            )?;
            let bsql = insert_sql(
                c,
                "message_body",
                &[
                    "id",
                    "message_id",
                    "raw_hash",
                    "text_raw",
                    "attributed_outcome",
                    "display_text",
                    "display_source",
                    "summary_outcome",
                    "edit_label",
                    "decoder_version",
                    "first_session_id",
                    "first_rank",
                    "last_session_id",
                    "last_rank",
                ],
            )?;
            let cmsql = insert_sql(
                c,
                "chat_message",
                &[
                    "chat_id",
                    "message_id",
                    "sort_key",
                    "first_session_id",
                    "last_session_id",
                    "last_rank",
                ],
            )?;
            let asql = insert_sql(
                c,
                "attachment",
                &[
                    "id",
                    "key_kind",
                    "guid",
                    "filename",
                    "uti",
                    "mime_type",
                    "transfer_name",
                    "total_bytes",
                    "first_session_id",
                    "first_rank",
                    "last_session_id",
                    "last_rank",
                ],
            )?;
            let masql = insert_sql(
                c,
                "message_attachment",
                &[
                    "message_id",
                    "attachment_id",
                    "ord_rank",
                    "ord_rowid",
                    "first_session_id",
                    "last_session_id",
                    "last_rank",
                ],
            )?;
            let osql = insert_sql(
                c,
                "attachment_observation",
                &[
                    "attachment_id",
                    "session_id",
                    "path_class",
                    "lookup",
                    "copy",
                    "blob_id",
                ],
            )?;
            let mut cur_st =
                c.prepare("UPDATE attachment SET current_blob_id = ?1 WHERE id = ?1")?;
            let mut m_st = c.prepare(&msql)?;
            let mut b_st = c.prepare(&bsql)?;
            let mut cm_st = c.prepare(&cmsql)?;
            let mut a_st = c.prepare(&asql)?;
            let mut ma_st = c.prepare(&masql)?;
            let mut o_st = c.prepare(&osql)?;
            let mut body_st = c.prepare("UPDATE message SET current_body_id = ?1 WHERE id = ?1")?;
            for m in &batch {
                let key = sort_key(m.utc_ns, &rk, m.id);
                let (raw, unit) = match m.utc_ns {
                    // Cocoa nanoseconds (the source's unit for these dates).
                    Some(ns) => (
                        ns - 978_307_200 * 1_000_000_000,
                        TimeUnit::Nanoseconds.code(),
                    ),
                    None => (0, TimeUnit::Zero.code()),
                };
                let (atype, aguid, akind, ashape, atarget, apart) = match &m.reaction {
                    Some((k, g)) => (
                        int(2000 + k),
                        blob(format!("p:0/{g}")),
                        int(AssocKindCode::TapbackLove.code() + k),
                        int(AssocTargetShape::Part.code()),
                        blob(g.as_bytes()),
                        int(0),
                    ),
                    None => (
                        int(0),
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::Null,
                    ),
                };
                run(
                    &mut m_st,
                    vec![
                        int(m.id),
                        int(KeyKind::Exact.code()),
                        blob(m.guid.as_bytes()),
                        int(i64::from(m.from_me)),
                        int(m.handle.unwrap_or(0)),
                        opt_int(m.handle),
                        blob(b"iMessage"),
                        int(ServiceClassCode::IMessage.code()),
                        int(raw),
                        int(unit),
                        opt_int(m.utc_ns),
                        int(0),
                        int(0),
                        int(ItemKindCode::Message.code()),
                        atype,
                        aguid.clone(),
                        st(aguid != Value::Null),
                        int(if m.reaction.is_some() {
                            AssocPresent::Yes.code()
                        } else {
                            AssocPresent::No.code()
                        }),
                        akind,
                        ashape,
                        atarget,
                        apart,
                        m.reply_to
                            .as_ref()
                            .map_or(Value::Null, |g| blob(g.as_bytes())),
                        st(m.reply_to.is_some()),
                        blob(&key),
                        int(m.id),
                        int(i64::from(DERIVER_VERSION)),
                        int(session),
                        blob(rk),
                        int(session),
                        blob(rk),
                    ],
                )?;
                run(
                    &mut b_st,
                    vec![
                        int(m.id),
                        int(m.id),
                        blob(sha(&[
                            b"oma-bulk-body",
                            m.guid.as_bytes(),
                            m.text.as_bytes(),
                        ])),
                        blob(m.text.as_bytes()),
                        int(AttributedOutcome::Null.code()),
                        text(m.text.clone()),
                        int(DisplaySource::Text.code()),
                        int(SummaryOutcome::Null.code()),
                        int(EditLabelCode::NoEditRecorded.code()),
                        int(i64::from(DECODER_VERSION)),
                        int(session),
                        blob(rk),
                        int(session),
                        blob(rk),
                    ],
                )?;
                body_st.execute([m.id])?;
                run(
                    &mut cm_st,
                    vec![
                        int(m.chat),
                        int(m.id),
                        blob(&key),
                        int(session),
                        int(session),
                        blob(rk),
                    ],
                )?;
                if let Some(att_id) = m.attachment {
                    let stored = placed_of.get(&att_id);
                    let name = format!("SYNTH-BULK-FILE-{att_id}.jpeg");
                    run(
                        &mut a_st,
                        vec![
                            int(att_id),
                            int(KeyKind::Exact.code()),
                            blob(format!("SYNTH-BULK-ATT-{att_id}")),
                            blob(format!(
                                "~/Library/SMS/Attachments/bk/{att_id:02}/SYNTH-BULK/{name}"
                            )),
                            blob(b"public.jpeg"),
                            blob(b"image/jpeg"),
                            blob(name.as_bytes()),
                            int(stored.map_or(1024 + att_id, |s| s.1)),
                            int(session),
                            blob(rk),
                            int(session),
                            blob(rk),
                        ],
                    )?;
                    run(
                        &mut ma_st,
                        vec![
                            int(m.id),
                            int(att_id),
                            blob(rk),
                            int(att_id),
                            int(session),
                            int(session),
                            blob(rk),
                        ],
                    )?;
                    let (lookup, copy, blob_id) = match stored {
                        Some(_) => (LookupCode::Present, CopyCode::Copied, int(att_id)),
                        None => (
                            LookupCode::NotListedAtSourcedKey,
                            CopyCode::NotInBackup,
                            Value::Null,
                        ),
                    };
                    run(
                        &mut o_st,
                        vec![
                            int(att_id),
                            int(session),
                            int(PathClassCode::SmsAttachments.code()),
                            int(lookup.code()),
                            int(copy.code()),
                            blob_id,
                        ],
                    )?;
                    if stored.is_some() {
                        cur_st.execute([att_id])?;
                    }
                }
            }
            Ok(())
        })
        .expect("message batch");
        for m in &batch {
            report.messages += 1;
            report.reactions += u64::from(m.reaction.is_some());
            report.replies += u64::from(m.reply_to.is_some());
            report.undated += u64::from(m.utc_ns.is_none());
            report.attachments += u64::from(m.attachment.is_some());
        }
        done += n;
    }
    report.write_time = t0.elapsed();
    let t1 = Instant::now();
    let f = w.finalize(MODEL_NOW_NS).expect("finalize");
    report.finalize_time = t1.elapsed();
    report.fts_merge_steps = f.fts_merge_steps;
    report.fts_merge_complete = f.fts_merge_complete;
    report.batches = w.batches();
    report
}
