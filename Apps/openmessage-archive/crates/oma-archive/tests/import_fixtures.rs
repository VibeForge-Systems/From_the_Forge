//! The synthetic import catalogue, `tests/fixtures/imports` (spec §19.1).
//!
//! - The inputs match `SHA256SUMS`, and the SHA-256 of `SHA256SUMS` itself is
//!   pinned here: the inputs never change once committed.
//! - The model's expectations match `EXPECTED.SHA256SUMS`. That file is
//!   pinned separately (with `ImportCounts`, in slice 4f), because the
//!   expectations may be regenerated while the import is built.
//! - Every entry opens, and the M3 check reads it as importable.
//! - The generator's model agrees with the Rust readers on what it can be
//!   checked against before the import exists: which rows are keyed (the R6
//!   key passes), each staged content digest (the rank's tie-break), the
//!   plist dates the rank is built from, and each attachment's SHA-256
//!   (`copy_file_to`).
//!
//! The content manifests themselves are asserted against archives by the
//! import tests (slices 4f to 4j). Opening a backup here writes only to a
//! temporary staging directory; the committed fixtures are never written.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use oma_backup::{
    inspect, CancelToken, Candidate, Inspection, JobCtx, Limits, OpenBackup, Password, Progress,
    StagingRoot, WalPolicy, Zeroizing,
};
use oma_messages::{
    connect, stage, CheckOptions, Col, Connect, Cursor, ExtractionStatus, HandleKeyRow, KeyRow,
    MessagesCtx, MessagesProgress, Probe, ProbeOptions, Qualifier, Redacted, SmsDb, SqlText,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The SHA-256 of `tests/fixtures/imports/SHA256SUMS`. The inputs are
/// frozen: regenerating them invalidates every recorded run.
const INPUT_SUMS_SHA256: &str = "74a82a95bc1580864d8dd09c72e7223196fe1e4586decf048be6cb564a1ed113";

const UDID: &str = "SYNTHETIC0-0000000000000000";
const PASSWORD: &str = "synthetic-test-password";
const MEDIA_DOMAIN: &str = "MediaDomain";

/// The entries the catalogue must hold (spec §19.1, with the WAL variant).
const ENTRIES: &[&str] = &[
    "imp-attach-plain",
    "imp-dev1-baddate-plain",
    "imp-dev1-undated-plain",
    "imp-dev1-v1-plain",
    "imp-dev1-v2-enc",
    "imp-dev1-v2-plain",
    "imp-dev2-plain",
    "imp-fts-plain",
    "imp-keys-hostile-plain",
    "imp-rd-plain",
    "imp-wal-missing-plain",
    "imp-wal-plain",
];

/// The rank's date bounds as the generator applies them (spec §8.7): from
/// 2007-01-01 to its model import start (2027-01-01) plus 48 hours.
const DATE_FLOOR_NS: i128 = 1_167_609_600 * 1_000_000_000;
const DATE_CEILING_NS: i128 = (1_798_761_600 + 48 * 3600) * 1_000_000_000;

fn root() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/imports"
    ))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn password(entry: &str) -> Option<&'static str> {
    entry.ends_with("-enc").then_some(PASSWORD)
}

/// Every file below `dir`, relative to `base`, with `/` separators.
fn walk(dir: &Path, base: &Path, out: &mut BTreeSet<String>) {
    for e in fs::read_dir(dir).expect("read_dir").flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, base, out);
        } else {
            let rel = p.strip_prefix(base).expect("under root");
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.insert(parts.join("/"));
        }
    }
}

/// A path below the catalogue from a `/`-separated relative name.
fn at(rel: &str) -> PathBuf {
    rel.split('/').fold(root(), |p, part| p.join(part))
}

fn is_expectation(rel: &str) -> bool {
    let first = rel.split('/').next().unwrap_or_default();
    first.ends_with(".expected") || first == "scenarios"
}

/// Checks one sums file and returns the names it lists.
fn check_sums(name: &str) -> BTreeSet<String> {
    let raw = fs::read(root().join(name)).expect(name);
    let text = String::from_utf8(raw).expect("ascii");
    assert!(!text.contains('\r'), "{name} has CRLF line endings");
    let mut listed = BTreeSet::new();
    for line in text.lines() {
        let (want, rel) = line.split_once("  ").expect("line format");
        let bytes = fs::read(at(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert_eq!(sha256_hex(&bytes), want, "{rel} does not match {name}");
        assert!(
            listed.insert(rel.to_owned()),
            "{rel} listed twice in {name}"
        );
    }
    listed
}

#[test]
fn inputs_and_expectations_match_their_sums() {
    let sums = fs::read(root().join("SHA256SUMS")).expect("SHA256SUMS");
    assert_eq!(
        sha256_hex(&sums),
        INPUT_SUMS_SHA256,
        "the import inputs changed: they are frozen once committed (spec §19.1)"
    );
    let inputs = check_sums("SHA256SUMS");
    let expected = check_sums("EXPECTED.SHA256SUMS");

    let mut present = BTreeSet::new();
    walk(&root(), &root(), &mut present);
    for meta in ["SHA256SUMS", "EXPECTED.SHA256SUMS", "README.md"] {
        assert!(present.remove(meta), "{meta} missing");
    }
    let (want_expected, want_inputs): (BTreeSet<String>, BTreeSet<String>) =
        present.into_iter().partition(|p| is_expectation(p));
    assert_eq!(inputs, want_inputs, "inputs and SHA256SUMS differ");
    assert_eq!(
        expected, want_expected,
        "expectations and EXPECTED.SHA256SUMS differ"
    );

    for entry in ENTRIES {
        assert!(
            inputs.contains(&format!("{entry}/{UDID}/Manifest.db")),
            "{entry} has no backup"
        );
        assert!(
            expected.contains(&format!("{entry}.expected/content.json")),
            "{entry} has no content manifest"
        );
    }
    let mut entries: Vec<String> = fs::read_dir(root())
        .expect("dir")
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.ends_with(".expected") && n != "scenarios")
        .collect();
    entries.sort();
    assert_eq!(entries, ENTRIES);
    assert!(
        expected
            .iter()
            .filter(|p| p.starts_with("scenarios/"))
            .count()
            >= 10
    );
}

// ---------------------------------------------------------------------------
// Opening an entry
// ---------------------------------------------------------------------------

/// An entry's backup, opened in place with a temporary staging library.
struct Opened {
    ob: OpenBackup,
    _library: tempfile::TempDir,
}

fn inspect_entry(entry: &str) -> Inspection {
    let limits = Limits::default();
    let mut n = 0;
    let candidate = Candidate::for_dir(&root().join(entry).join(UDID)).expect("candidate");
    inspect(&candidate, &limits, &mut n).expect("inspect")
}

fn open(entry: &str) -> Opened {
    let library = tempfile::tempdir().expect("tempdir");
    let limits = Limits::default();
    let inspection = inspect_entry(entry);
    let staging = StagingRoot::new(library.path(), &inspection.root).expect("staging root");
    let session = staging.begin().expect("begin");
    let pw = password(entry)
        .map(|p| Password::new(Zeroizing::new(p.to_owned()), &limits).expect("password"));
    let token = CancelToken::new();
    let mut sink = |_: Progress| {};
    let mut ctx = JobCtx::new(&token, &mut sink);
    let ob = OpenBackup::open(inspection, session, pw, &limits, &mut ctx)
        .unwrap_or_else(|e| panic!("{entry}: open failed: {:?}", e.error));
    Opened {
        ob,
        _library: library,
    }
}

fn content(rel: &str) -> Value {
    serde_json::from_slice(&fs::read(at(rel)).expect("manifest")).expect("json")
}

fn manifest(entry: &str) -> Value {
    content(&format!("{entry}.expected/content.json"))
}

fn list<'a>(v: &'a Value, path: &[&str]) -> &'a Vec<Value> {
    path.iter()
        .fold(v, |v, k| &v[*k])
        .as_array()
        .unwrap_or_else(|| panic!("{path:?} is not a list"))
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key]
        .as_str()
        .unwrap_or_else(|| panic!("{key} is not a string"))
}

#[test]
fn every_entry_is_importable() {
    for entry in ENTRIES {
        let o = open(entry);
        let token = CancelToken::new();
        let mut sink = |_: MessagesProgress| {};
        let mut ctx = MessagesCtx::new(&token, &mut sink);
        let report = oma_messages::check(&o.ob, &CheckOptions::default(), &mut ctx)
            .unwrap_or_else(|e| panic!("{entry}: check failed: {e:?}"));
        let ExtractionStatus::Readable { qualifiers } = &report.status else {
            panic!("{entry}: not readable: {}", report.status_kind());
        };
        let wal_skipped = qualifiers.contains(&Qualifier::WalSkipped);
        assert_eq!(wal_skipped, *entry == "imp-wal-missing-plain", "{entry}");
        let want = manifest(entry);
        let session = &want["sessions"][*entry];
        let model_skipped = list(session, &["qualifiers"])
            .iter()
            .any(|q| q == "walSkipped");
        assert_eq!(wal_skipped, model_skipped, "{entry}");
        assert_eq!(session["fullObservation"], !wal_skipped, "{entry}");
        assert!(o.ob.close().removed, "{entry}: staging not removed");
    }
}

// ---------------------------------------------------------------------------
// Keys (spec §6.2): the model against the R6 key passes
// ---------------------------------------------------------------------------

type Text = Col<Redacted<SqlText>>;

fn bytes(c: &Text) -> Option<&[u8]> {
    c.value().map(|v| v.expose().as_bytes())
}

/// The manifest's ref for GUID bytes.
fn guid_ref(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(t) => format!("guid:{t}"),
        Err(_) => format!("guid-hex:{}", hex(b)),
    }
}

/// The manifest's ref for a handle's (address, service).
fn handle_ref(id: &[u8], service: &Text) -> String {
    let show = |b: &[u8]| match std::str::from_utf8(b) {
        Ok(t) => t.to_owned(),
        Err(_) => format!("hex:{}", hex(b)),
    };
    let s = match service {
        Col::Value(v) => show(v.expose().as_bytes()),
        Col::Null => "(null)".to_owned(),
        Col::Absent => "(absent)".to_owned(),
        Col::Oversized { .. } => "(oversized)".to_owned(),
        Col::Unreadable(_) => "(unreadable)".to_owned(),
    };
    format!("handle:{}|{s}", show(id))
}

/// Keys read by the passes: per entity, (rows, the refs that occur once).
/// A key is a readable, non-empty value (the model's reading of §6.2, in
/// the catalogue README).
#[derive(Default)]
struct SourceKeys {
    rows: BTreeMap<&'static str, usize>,
    unique: BTreeMap<&'static str, BTreeSet<String>>,
}

fn add_keys(out: &mut SourceKeys, entity: &'static str, refs: Vec<Option<String>>) {
    let mut count: BTreeMap<String, usize> = BTreeMap::new();
    for r in refs.iter().flatten() {
        *count.entry(r.clone()).or_default() += 1;
    }
    out.rows.insert(entity, refs.len());
    out.unique.insert(
        entity,
        count
            .into_iter()
            .filter(|(_, n)| *n == 1)
            .map(|(r, _)| r)
            .collect(),
    );
}

fn source_keys(entry: &str) -> (SourceKeys, String) {
    let o = open(entry);
    let token = CancelToken::new();
    let mut sink = |_: MessagesProgress| {};
    let mut ctx = MessagesCtx::new(&token, &mut sink);
    let staged = stage(&o.ob, WalPolicy::BestEffort, &mut ctx).expect("stage");
    let digest = hex(staged.content_digest().as_bytes());
    let mut out = SourceKeys::default();
    {
        let Connect::Ready(conn) = connect(&staged, &ctx).expect("connect") else {
            panic!("{entry}: schema exceeds limits");
        };
        let probe = SmsDb::open_with(&conn, &ProbeOptions::default(), &mut ctx).expect("probe");
        let Probe::Supported(db) = probe else {
            panic!("{entry}: not supported");
        };
        let guid_key = |r: &KeyRow| bytes(&r.guid).filter(|b| !b.is_empty()).map(guid_ref);
        for (entity, which) in [("message", 0), ("chat", 1), ("attachment", 2)] {
            let mut refs = Vec::new();
            let f = |r: KeyRow| {
                refs.push(guid_key(&r));
                ControlFlow::<()>::Continue(())
            };
            match which {
                0 => db.for_each_message_key(Cursor::Start, &mut ctx, f),
                1 => db.for_each_chat_key(Cursor::Start, &mut ctx, f),
                _ => db.for_each_attachment_key(Cursor::Start, &mut ctx, f),
            }
            .expect("key pass");
            add_keys(&mut out, entity, refs);
        }
        let mut refs = Vec::new();
        db.for_each_handle_key(Cursor::Start, &mut ctx, |r: HandleKeyRow| {
            refs.push(
                bytes(&r.id)
                    .filter(|b| !b.is_empty())
                    .map(|id| handle_ref(id, &r.service)),
            );
            ControlFlow::<()>::Continue(())
        })
        .expect("handle keys");
        add_keys(&mut out, "handle", refs);
    }
    staged.remove().expect("remove");
    assert!(o.ob.close().removed, "{entry}: staging not removed");
    (out, digest)
}

#[test]
fn the_model_keys_what_the_key_passes_read_and_digests_match() {
    let mut digests = BTreeMap::new();
    for entry in ENTRIES {
        let (keys, digest) = source_keys(entry);
        let m = manifest(entry);
        let c = &m["content"];
        assert_eq!(
            text(&m["sessions"][*entry], "contentDigest"),
            digest,
            "{entry}: content digest"
        );
        digests.insert(*entry, digest);
        let excluded = &c["excluded"];
        for (entity, plural, excl, excl_unkeyed) in [
            ("handle", "handles", None, None),
            ("chat", "chats", None, None),
            (
                "message",
                "messages",
                Some("messages"),
                Some("unkeyedMessageRows"),
            ),
            (
                "attachment",
                "attachments",
                Some("attachments"),
                Some("unkeyedAttachmentRows"),
            ),
        ] {
            let rows = list(c, &[plural]);
            let mut keyed: BTreeSet<String> = rows
                .iter()
                .map(|r| text(r, "ref").to_owned())
                .filter(|r| !r.starts_with("unkeyed:"))
                .collect();
            let mut total = rows.len();
            if let (Some(k), Some(u)) = (excl, excl_unkeyed) {
                let e = list(excluded, &[k]);
                keyed.extend(e.iter().map(|r| r.as_str().expect("ref").to_owned()));
                total +=
                    e.len() + usize::try_from(excluded[u].as_u64().expect("count")).expect("n");
            }
            assert_eq!(keyed, keys.unique[entity], "{entry}: keyed {entity} rows");
            assert_eq!(total, keys.rows[entity], "{entry}: {entity} rows");
        }
    }
    // Equal rank needs equal content: the plain and encrypted v2 (spec §19.1).
    assert_eq!(digests["imp-dev1-v2-plain"], digests["imp-dev1-v2-enc"]);
    // The dated, undated and out-of-range copies share v1's sms.db.
    assert_eq!(
        digests["imp-dev1-v1-plain"],
        digests["imp-dev1-undated-plain"]
    );
    assert_eq!(
        digests["imp-dev1-v1-plain"],
        digests["imp-dev1-baddate-plain"]
    );
    assert_ne!(digests["imp-wal-plain"], digests["imp-wal-missing-plain"]);
}

// ---------------------------------------------------------------------------
// The rank's dates (spec §8.7)
// ---------------------------------------------------------------------------

fn unix_ns(t: SystemTime) -> i128 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i128::try_from(d.as_nanos()).expect("ns"),
        Err(e) => -i128::try_from(e.duration().as_nanos()).expect("ns"),
    }
}

#[test]
fn the_rank_dates_are_the_plist_dates_the_backup_holds() {
    for entry in ENTRIES {
        let ins = inspect_entry(entry);
        let status = ins
            .status
            .as_ref()
            .and_then(|s| s.date)
            .map(|d| unix_ns(d.into()));
        let info = ins
            .info
            .as_ref()
            .and_then(|i| i.last_backup_date)
            .map(|d| unix_ns(d.into()));
        let in_range = |ns: i128| (DATE_FLOOR_NS..=DATE_CEILING_NS).contains(&ns);
        let (key, source) = match (status, info) {
            (Some(s), _) if in_range(s) => (Some(s), Some("statusPlistDate")),
            (_, Some(i)) if in_range(i) => (Some(i), Some("infoPlistLastBackupDate")),
            _ => (None, None),
        };
        let out_of_range =
            status.is_some_and(|s| !in_range(s)) || info.is_some_and(|i| !in_range(i));
        let m = manifest(entry);
        let s = &m["sessions"][*entry];
        assert_eq!(
            s["orderKeyNs"].as_i64().map(i128::from),
            key,
            "{entry}: order key"
        );
        assert_eq!(s["orderKeySource"].as_str(), source, "{entry}: source");
        assert_eq!(
            s["orderClass"],
            if key.is_some() { "dated" } else { "undated" },
            "{entry}"
        );
        let flagged = list(s, &["qualifiers"])
            .iter()
            .any(|q| q == "backupDateOutOfRange");
        assert_eq!(flagged, out_of_range, "{entry}: out-of-range qualifier");
        // The rank is order class, the biased order key, then the digest.
        let mut rank = vec![u8::from(key.is_some())];
        let biased = key.map_or(0u64, |k| u64::try_from(k).expect("positive") ^ (1u64 << 63));
        rank.extend_from_slice(&biased.to_be_bytes());
        let rank = format!("{}{}", hex(&rank), text(s, "contentDigest"));
        assert_eq!(text(s, "rank"), rank, "{entry}: rank");
    }
    // The fixtures this test is about.
    let s = &manifest("imp-dev1-undated-plain")["sessions"]["imp-dev1-undated-plain"];
    assert_eq!(s["orderClass"], "undated");
    let s = &manifest("imp-dev1-baddate-plain")["sessions"]["imp-dev1-baddate-plain"];
    assert_eq!(s["orderKeySource"], "infoPlistLastBackupDate");
}

// ---------------------------------------------------------------------------
// Attachments (spec §9): the model's hashes against copy_file_to
// ---------------------------------------------------------------------------

#[test]
fn the_model_hashes_are_the_copied_plaintexts() {
    let mut copied = 0;
    let mut not_copied = 0;
    for entry in ENTRIES {
        let m = manifest(entry);
        let o = open(entry);
        let token = CancelToken::new();
        for a in list(&m["content"], &["attachments"]) {
            let Some(name) = a["columns"]["filename"]["text"].as_str() else {
                assert!(a["sha256"].is_null(), "{entry}: no filename but bytes");
                continue;
            };
            let Some(rel) = name.strip_prefix("~/") else {
                assert!(a["sha256"].is_null(), "{entry}");
                continue;
            };
            let key = o.ob.find_attachment(MEDIA_DOMAIN, rel);
            match a["sha256"].as_str() {
                Some(want) => {
                    let key = key.expect("lookup").expect("listed");
                    let mut out = Vec::new();
                    let mut sink = |_: Progress| {};
                    let mut ctx = JobCtx::new(&token, &mut sink);
                    let c =
                        o.ob.copy_file_to(&key, &mut out, &mut ctx)
                            .unwrap_or_else(|e| panic!("{entry}: copy failed: {e:?}"));
                    assert_eq!(hex(&c.sha256), want, "{entry}: attachment hash");
                    assert_eq!(sha256_hex(&out), want, "{entry}");
                    let blobs = list(a, &["blobs"]);
                    assert!(blobs.iter().any(|b| b == want), "{entry}");
                    let len = list(&m["content"], &["blobs"])
                        .iter()
                        .find(|b| b["sha256"] == want)
                        .and_then(|b| b["byteLen"].as_u64())
                        .expect("blob listed");
                    assert_eq!(c.len, len, "{entry}");
                    copied += 1;
                }
                None => {
                    // Not copied: not listed, not looked up, or a blob that
                    // cannot be read.
                    if let Ok(Some(key)) = key {
                        let mut sink = |_: Progress| {};
                        let mut ctx = JobCtx::new(&token, &mut sink);
                        let looked_up = name.starts_with("~/Library/SMS/");
                        let r = o.ob.copy_file_to(&key, &mut std::io::sink(), &mut ctx);
                        assert!(!looked_up || r.is_err(), "{entry}: copyable but not copied");
                    }
                    not_copied += 1;
                }
            }
        }
        assert!(o.ob.close().removed, "{entry}");
    }
    assert!(copied >= 20, "{copied}");
    assert!(not_copied >= 6, "{not_copied}");

    // The encrypted entry's plaintexts are committed beside it.
    let dir = root().join("imp-dev1-v2-enc.expected").join("attachments");
    let mut n = 0;
    for e in fs::read_dir(&dir).expect("attachments").flatten() {
        let want = sha256_hex(&fs::read(e.path()).expect("plaintext"));
        let m = manifest("imp-dev1-v2-enc");
        assert!(
            list(&m["content"], &["blobs"])
                .iter()
                .any(|b| b["sha256"] == want.as_str()),
            "a plaintext the model does not store"
        );
        n += 1;
    }
    assert_eq!(n, 6, "the six present v2 attachments");
}

// ---------------------------------------------------------------------------
// The manifests' own shape
// ---------------------------------------------------------------------------

#[test]
fn every_manifest_names_known_entries_and_resolves_its_refs() {
    let mut files = BTreeSet::new();
    walk(&root(), &root(), &mut files);
    let manifests: Vec<String> = files
        .into_iter()
        .filter(|f| f.ends_with("content.json"))
        .collect();
    assert_eq!(manifests.len(), ENTRIES.len() + 10, "{manifests:?}");
    for rel in &manifests {
        let m = content(rel);
        assert_eq!(m["format"], "oma-import-content", "{rel}");
        assert_eq!(m["formatVersion"], 1, "{rel}");
        for order in list(&m, &["orders"]) {
            for step in order.as_array().expect("steps") {
                let e = text(step, "entry");
                assert!(ENTRIES.contains(&e), "{rel}: {e}");
                assert!(m["sessions"][e].is_object(), "{rel}: {e}");
            }
        }
        let c = &m["content"];
        let refs = |plural: &str| -> BTreeSet<String> {
            list(c, &[plural])
                .iter()
                .map(|r| text(r, "ref").to_owned())
                .collect()
        };
        let (handles, chats, attachments) = (refs("handles"), refs("chats"), refs("attachments"));
        let blobs: BTreeSet<&str> = list(c, &["blobs"])
            .iter()
            .map(|b| text(b, "sha256"))
            .collect();
        for chat in list(c, &["chats"]) {
            for h in list(chat, &["handles"]) {
                assert!(handles.contains(h.as_str().expect("ref")), "{rel}");
            }
        }
        let mut last: Option<(u8, i64)> = None;
        for msg in list(c, &["messages"]) {
            for ch in list(msg, &["chats"]) {
                assert!(chats.contains(ch.as_str().expect("ref")), "{rel}");
            }
            for a in list(msg, &["attachments"]) {
                assert!(attachments.contains(a.as_str().expect("ref")), "{rel}");
            }
            if let Some(h) = msg["handle"].as_str() {
                assert!(handles.contains(h), "{rel}");
            }
            let d = &msg["display"];
            assert!(
                d["text"].is_string() != d["noneReason"].is_string(),
                "{rel}"
            );
            assert_eq!(
                list(msg, &["bodies"])
                    .iter()
                    .filter(|b| b["current"] == true)
                    .count(),
                1,
                "{rel}"
            );
            // Dated messages come first, in date order.
            let key = match msg["sort"]["utcNs"].as_i64() {
                Some(ns) => (0, ns),
                None => (1, 0),
            };
            if let Some(prev) = last {
                assert!(prev <= key, "{rel}: messages out of order");
            }
            last = Some(key);
        }
        for a in list(c, &["attachments"]) {
            if let Some(sha) = a["sha256"].as_str() {
                assert!(blobs.contains(sha), "{rel}");
            }
            for b in list(a, &["blobs"]) {
                assert!(blobs.contains(b.as_str().expect("sha")), "{rel}");
            }
        }
    }
}
