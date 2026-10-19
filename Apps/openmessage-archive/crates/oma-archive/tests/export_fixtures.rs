//! The synthetic export catalogue, `tests/fixtures/export` (M6 spec §15,
//! slice 6c).
//!
//! - The inputs (every backup file and `hidden-vectors.json`) match
//!   `SHA256SUMS`, whose own SHA-256 is pinned here: they never change once
//!   committed. The expectations match `EXPECTED.SHA256SUMS`, pinned
//!   separately: they may be regenerated when a slice settles a rule (the
//!   commit names the rule). Every file is covered by one of the two.
//! - `hidden-vectors.json` is well formed: each vector's text is its
//!   pieces, the classes are closed, and the copy and marked sets follow
//!   from the classes (the Vitest `hidden-vectors.spec.ts` checks the
//!   viewer's copy against it).
//! - Every entry and the scenario import into an archive that matches the
//!   model's `content` manifest (the import is the producer).
//! - `export.json` describes the archive: every sentinel and visible
//!   string is in it, every hostile, formula and file case is stored where
//!   the generator planted it (exactly, or by digest), each conversation
//!   has the members it states, and the messages in no conversation have
//!   none.
//! - The `patches` (codes an import cannot write) apply under the schema's
//!   `CHECK`s, and an export snapshot still assembles every patched item;
//!   the corrupted store file fails its re-hash.
//! - `write_oracle_archives` (ignored; Linux CI's oracle step) writes an
//!   archive per entry and scenario of both catalogues below
//!   `OMA_ORACLE_OUT`, for `scripts/oracles/run_oracles.py`.
//!
//! The masking oracle over this catalogue is in `export_scope.rs`.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use common::import::{assert_consistent, read_only};
use common::viewer::{
    self, export_docs, export_root, import_order_in, import_steps, json_under, names, sha256_hex,
    under, viewer_root, Imported, ENTRIES, EXPORT_ENTRIES, EXPORT_SCENARIOS, SCENARIOS,
};
use common::*;
use oma_archive::{BlobId, CancelToken, ExportDepth, Library, MessageId, Reveal};
use serde_json::{json, Value};

/// The SHA-256 of `tests/fixtures/export/SHA256SUMS`. The inputs are
/// frozen: regenerating them invalidates every recorded run.
const INPUT_SUMS_SHA256: &str = "dee62b084b721c322e5194d7149c5804733afcdf61269391421e64d1c6e4c887";

/// The SHA-256 of `tests/fixtures/export/EXPECTED.SHA256SUMS` (slice 6c).
const EXPECTED_SUMS_SHA256: &str =
    "de9bb3259cc989512000002870ee02d5c663aa83986464829db717d031090e46";

fn at(rel: &str) -> PathBuf {
    under(&export_root(), rel)
}

fn json_at(rel: &str) -> Value {
    json_under(&export_root(), rel)
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

fn is_expectation(rel: &str) -> bool {
    let first = rel.split('/').next().unwrap_or_default();
    first.ends_with(".expected") || first == "scenarios"
}

fn check_sums(name: &str, pinned: &str) -> BTreeSet<String> {
    let raw = fs::read(at(name)).expect("sums");
    assert_eq!(
        sha256_hex(&raw),
        pinned,
        "{name} changed: regenerate deliberately and re-pin it here"
    );
    let text = String::from_utf8(raw).expect("ascii");
    assert!(!text.contains('\r'), "{name} has CRLF line endings");
    let mut listed = BTreeSet::new();
    for line in text.lines() {
        let (want, rel) = line.split_once("  ").expect("line format");
        let bytes = fs::read(at(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert_eq!(sha256_hex(&bytes), want, "{rel} does not match {name}");
        listed.insert(rel.to_owned());
    }
    listed
}

#[test]
fn every_file_is_pinned_by_one_of_the_two_sums() {
    let inputs = check_sums("SHA256SUMS", INPUT_SUMS_SHA256);
    let expected = check_sums("EXPECTED.SHA256SUMS", EXPECTED_SUMS_SHA256);
    assert!(inputs.iter().all(|r| !is_expectation(r)));
    assert!(expected.iter().all(|r| is_expectation(r)));
    let mut all = BTreeSet::new();
    walk(&export_root(), &export_root(), &mut all);
    for extra in ["SHA256SUMS", "EXPECTED.SHA256SUMS", "README.md"] {
        assert!(all.remove(extra), "{extra} missing");
    }
    let listed: BTreeSet<String> = inputs.union(&expected).cloned().collect();
    assert_eq!(
        all, listed,
        "files not covered by the sums, or listed and missing"
    );
    assert!(inputs.contains("hidden-vectors.json"));
    for e in EXPORT_ENTRIES {
        assert!(
            inputs
                .iter()
                .any(|r| r.starts_with(&format!("{e}/{}/", common::import::UDID))),
            "{e}"
        );
        for doc in ["content.json", "export.json"] {
            assert!(expected.contains(&format!("{e}.expected/{doc}")), "{e}");
        }
    }
    for s in EXPORT_SCENARIOS {
        for doc in ["content", "export"] {
            assert!(
                expected.contains(&format!("scenarios/{s}.{doc}.json")),
                "{s}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Hidden-character vectors
// ---------------------------------------------------------------------------

const HIDDEN_CLASSES: [&str; 8] = [
    "keep",
    "bidi",
    "invisible",
    "joiner",
    "tag",
    "vsRun",
    "control",
    "cr",
];

#[test]
fn hidden_vectors_are_well_formed() {
    let v = json_at("hidden-vectors.json");
    assert_eq!(v["format"], "oma-hidden-vectors");
    let vectors = v["vectors"].as_array().expect("vectors");
    assert!(vectors.len() >= 30);
    let mut names = BTreeSet::new();
    let mut classes = BTreeSet::new();
    for x in vectors {
        let name = x["name"].as_str().expect("name");
        assert!(names.insert(name), "{name} twice");
        let mut text = String::new();
        let mut copy = String::new();
        let mut marked = BTreeSet::new();
        for p in x["pieces"].as_array().expect("pieces") {
            let t = p["text"].as_str().expect("text");
            let c = p["class"].as_str().expect("class");
            assert!(HIDDEN_CLASSES.contains(&c), "{name}: {c}");
            assert!(!t.is_empty(), "{name}");
            classes.insert(c);
            text.push_str(t);
            if c == "keep" || c == "cr" {
                copy.push_str(t);
            }
            if c != "keep" {
                for ch in t.chars() {
                    marked.insert(format!("U+{:04X}", u32::from(ch)));
                }
            }
        }
        assert_eq!(x["text"], text.as_str(), "{name}");
        assert_eq!(x["withoutHiddenCharacters"], copy.as_str(), "{name}");
        let listed: BTreeSet<String> = x["markedCodePoints"]
            .as_array()
            .expect("marked")
            .iter()
            .map(|s| s.as_str().expect("cp").to_owned())
            .collect();
        assert_eq!(listed, marked, "{name}");
    }
    assert_eq!(
        classes,
        HIDDEN_CLASSES.iter().copied().collect(),
        "every class has a vector"
    );
}

// ---------------------------------------------------------------------------
// The import check, and where each case was planted
// ---------------------------------------------------------------------------

fn texts(c: &rusqlite::Connection, sql: &str) -> Vec<Option<Vec<u8>>> {
    c.prepare(sql)
        .expect("prepare")
        .query_map([], |r| {
            Ok(match r.get_ref(0)? {
                rusqlite::types::ValueRef::Null => None,
                rusqlite::types::ValueRef::Text(t) | rusqlite::types::ValueRef::Blob(t) => {
                    Some(t.to_vec())
                }
                other => Some(format!("{other:?}").into_bytes()),
            })
        })
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// The column `where` names, as stored bytes.
fn column_of(c: &rusqlite::Connection, place: &str) -> Vec<Option<Vec<u8>>> {
    let sql = match place {
        "body" => "SELECT CAST(display_text AS BLOB) FROM message_body",
        "address" => "SELECT address FROM handle",
        "chatName" => "SELECT display_name FROM chat",
        "fileName" => "SELECT transfer_name FROM attachment",
        "associationEmoji" => "SELECT assoc_emoji FROM message",
        other => panic!("unknown place {other}"),
    };
    texts(c, sql)
}

/// `true` when `case` (a described string) equals one of `values`.
fn planted(case: &Value, values: &[Option<Vec<u8>>]) -> bool {
    if let Some(t) = case["text"].as_str() {
        values.iter().flatten().any(|v| v == t.as_bytes())
    } else {
        let sha = case["sha256"].as_str().expect("sha256");
        values.iter().flatten().any(|v| sha256_hex(v) == sha)
    }
}

/// Every place a string can be stored: bodies (all three sources),
/// subjects, value history, names, addresses, emoji, and the store's
/// files.
fn archive_contains(c: &rusqlite::Connection, archive: &Path, s: &str) -> bool {
    let n: i64 = c
        .query_row(
            "SELECT \
               (SELECT count(*) FROM message_body WHERE instr(CAST(display_text AS BLOB), ?1) \
                  OR instr(text_raw, ?1) OR instr(attributed_raw, ?1) OR instr(summary_raw, ?1)) + \
               (SELECT count(*) FROM message WHERE instr(subject, ?1)) + \
               (SELECT count(*) FROM value_history WHERE instr(CAST(value AS BLOB), ?1)) + \
               (SELECT count(*) FROM chat WHERE instr(display_name, ?1)) + \
               (SELECT count(*) FROM handle WHERE instr(address, ?1)) + \
               (SELECT count(*) FROM attachment WHERE instr(transfer_name, ?1))",
            [s.as_bytes()],
            |r| r.get(0),
        )
        .expect("count");
    if n > 0 {
        return true;
    }
    let names: Vec<String> = c
        .prepare("SELECT storage_name FROM blob")
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("names");
    names.iter().any(|n| {
        let bytes = fs::read(archive.join("attachments").join(&n[..2]).join(n)).expect("store");
        bytes.windows(s.len()).any(|w| w == s.as_bytes())
    })
}

fn check_plantings(i: &Imported, order: &Value, e: &Value, what: &str) {
    let db = i.db();
    let c = read_only(&db);
    let dir = archive_dir(&i.tmp, &i.id);
    for (kind, list) in e["sentinels"].as_object().expect("sentinels") {
        for s in list.as_array().expect("list") {
            let s = s.as_str().expect("sentinel");
            assert!(archive_contains(&c, &dir, s), "{what}: {kind} {s} absent");
        }
    }
    for s in e["visible"].as_array().expect("visible") {
        let s = s.as_str().expect("visible");
        assert!(archive_contains(&c, &dir, s), "{what}: visible {s} absent");
    }
    for list in ["hostile", "formulaCases"] {
        for case in e[list].as_array().expect(list) {
            let place = case["where"].as_str().expect("where");
            assert!(
                planted(case, &column_of(&c, place)),
                "{what}: {list} {} not stored in its {place}",
                case["id"]
            );
        }
    }
    let files = column_of(&c, "fileName");
    for case in e["fileCases"].as_array().expect("fileCases") {
        let guid = case["attachment"].as_str().expect("guid");
        let (name, sha): (Option<Vec<u8>>, Option<Vec<u8>>) = c
            .query_row(
                "SELECT a.transfer_name, b.sha256 FROM attachment a \
                 LEFT JOIN blob b ON b.id = a.current_blob_id WHERE a.guid = ?1",
                [guid.as_bytes()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("attachment");
        match &case["transferName"] {
            Value::Null => assert!(name.is_none(), "{what}: {guid}"),
            d => assert!(planted(d, &[name]), "{what}: {guid} name"),
        }
        assert_eq!(
            sha.map(|s| hex(&s)).as_deref(),
            case["sha256"].as_str(),
            "{what}: {guid} blob"
        );
        assert!(!files.is_empty());
    }
    // Membership, by the model's refs.
    let n = names(i, order);
    let mut members: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut in_some = BTreeSet::new();
    let rows: Vec<(i64, i64)> = c
        .prepare("SELECT chat_id, message_id FROM chat_message")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    for (chat, m) in rows {
        members
            .entry(n.chats[&chat].clone())
            .or_default()
            .push(n.messages[&m].clone());
        in_some.insert(m);
    }
    let want = e["membership"]["chats"].as_object().expect("chats");
    for (chat, w) in want {
        let mut got = members.remove(chat).unwrap_or_default();
        got.sort();
        let mut wm: Vec<String> = w["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .map(|x| x.as_str().expect("ref").to_owned())
            .collect();
        wm.sort();
        assert_eq!(
            got.len() as u64,
            w["items"].as_u64().expect("items"),
            "{what}: {chat}"
        );
        assert_eq!(got, wm, "{what}: {chat}");
    }
    assert!(members.is_empty(), "{what}: chats the model does not list");
    let nowhere: BTreeSet<String> = n
        .messages
        .iter()
        .filter(|(id, _)| !in_some.contains(*id))
        .map(|(_, r)| r.clone())
        .collect();
    let want_nowhere: BTreeSet<String> = e["membership"]["noConversation"]
        .as_array()
        .expect("noConversation")
        .iter()
        .map(|x| x.as_str().expect("ref").to_owned())
        .collect();
    assert_eq!(nowhere, want_nowhere, "{what}: messages in no conversation");
}

fn import_doc(content: &Value, what: &str) -> Imported {
    let orders = content["orders"].as_array().expect("orders");
    assert_eq!(orders.len(), 1, "{what}");
    import_order_in(&export_root(), &orders[0], what)
}

#[test]
fn every_entry_and_scenario_imports_as_the_model_says_and_holds_its_cases() {
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut places = BTreeSet::new();
    let mut leading = BTreeSet::new();
    for (what, content, e) in export_docs() {
        assert_eq!(content["format"], "oma-viewer-content", "{what}");
        assert_eq!(e["format"], "oma-export-expectations", "{what}");
        let i = import_doc(&content, &what);
        let db = i.db();
        assert_consistent(&db);
        viewer::assert_content_matches(&db, &content, &i.labels, &what);
        check_plantings(&i, &content["orders"][0], &e, &what);
        for (k, v) in e["sentinels"].as_object().expect("sentinels") {
            *kinds.entry(k.clone()).or_default() += v.as_array().expect("list").len();
        }
        for list in ["hostile", "formulaCases"] {
            for c in e[list].as_array().expect(list) {
                places.insert(c["where"].as_str().expect("where").to_owned());
                if let Some(l) = c["leading"].as_str() {
                    leading.insert(l.to_owned());
                }
            }
        }
    }
    // The catalogue covers what spec §15 asks for.
    for k in [
        "oneTimeCodes",
        "earlierText",
        "editText",
        "alternativeText",
        "subjectHistory",
        "unsentAttachmentFiles",
    ] {
        assert!(kinds.get(k).copied().unwrap_or(0) > 0, "no {k} sentinel");
    }
    for p in [
        "body",
        "address",
        "chatName",
        "fileName",
        "associationEmoji",
    ] {
        assert!(places.contains(p), "no case planted in a {p}");
    }
    for l in [
        "=", "+", "-", "@", "\t", "\r", "\u{ff1d}", "\u{ff0b}", "\u{ff0d}", "\u{ff20}", "\u{fe66}",
        "\u{fe62}", "\u{fe63}", "\u{fe6b}",
    ] {
        assert!(leading.contains(l), "no formula case led by {l:?}");
    }
}

// ---------------------------------------------------------------------------
// Patches and store corruption
// ---------------------------------------------------------------------------

/// Apply `patches` to the archive at `db` (a test's own copy) with a plain
/// connection; the schema's `CHECK`s still apply.
fn apply_patches(db: &Path, patches: &[Value]) {
    let c = raw(db);
    for p in patches {
        let table = p["table"].as_str().expect("table");
        assert_eq!(table, "message", "only message patches so far");
        let guid = p["guid"].as_str().expect("guid");
        let set = p["set"].as_object().expect("set");
        let cols: Vec<String> = set.keys().map(|k| format!("{k} = ?")).collect();
        let mut values: Vec<rusqlite::types::Value> = set
            .values()
            .map(|v| match v {
                Value::Null => rusqlite::types::Value::Null,
                Value::Number(n) => rusqlite::types::Value::Integer(n.as_i64().expect("int")),
                other => panic!("unsupported patch value {other}"),
            })
            .collect();
        values.push(rusqlite::types::Value::Blob(guid.as_bytes().to_vec()));
        let n = c
            .execute(
                &format!("UPDATE message SET {} WHERE guid = ?", cols.join(", ")),
                rusqlite::params_from_iter(values),
            )
            .unwrap_or_else(|e| panic!("patch {guid}: {e}"));
        assert_eq!(n, 1, "patch {guid}");
    }
}

#[test]
fn patches_apply_and_an_export_still_assembles_every_item() {
    let mut applied = 0;
    for (what, content, e) in export_docs() {
        let patches = e["patches"].as_array().expect("patches");
        if patches.is_empty() {
            continue;
        }
        let i = import_doc(&content, &what);
        apply_patches(&i.db(), patches);
        applied += patches.len();
        let c = read_only(&i.db());
        let ids: Vec<MessageId> = c
            .prepare("SELECT id FROM message ORDER BY id")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<Result<Vec<i64>, _>>()
            .expect("ids")
            .into_iter()
            .map(MessageId::new)
            .collect();
        let r = i.lib.open_read(&i.id).expect("reader");
        let hold = i.lib.hold_for_export(&i.id).expect("hold");
        let n = r
            .with_export_snapshot(&hold, &CancelToken::new(), |s| {
                s.assemble(&ids, Reveal::default(), ExportDepth::WithHistory)
                    .map(|v| v.len())
            })
            .expect("assemble");
        assert_eq!(n, ids.len(), "{what}");
    }
    assert!(applied >= 3);
}

#[test]
fn the_corrupted_store_file_fails_its_rehash() {
    let mut corrupted = 0;
    for (what, content, e) in export_docs() {
        for case in e["corruptStoreFiles"].as_array().expect("corrupt") {
            let i = import_doc(&content, &what);
            let c = read_only(&i.db());
            let guid = case["attachment"].as_str().expect("guid");
            let (blob, name): (i64, String) = c
                .query_row(
                    "SELECT b.id, b.storage_name FROM attachment a JOIN blob b \
                       ON b.id = a.current_blob_id WHERE a.guid = ?1",
                    [guid.as_bytes()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .expect("blob");
            assert_eq!(Some(name.as_str()), case["sha256"].as_str());
            let file = archive_dir(&i.tmp, &i.id)
                .join("attachments")
                .join(&name[..2])
                .join(&name);
            let mut bytes = fs::read(&file).expect("store file");
            let last = bytes.len() - 1;
            bytes[last] ^= 0xff;
            #[cfg(unix)]
            chmod(&file, 0o600);
            #[cfg(windows)]
            {
                let mut p = fs::metadata(&file).expect("meta").permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                p.set_readonly(false);
                fs::set_permissions(&file, p).expect("writable");
            }
            fs::write(&file, &bytes).expect("corrupt");
            let r = i.lib.open_read(&i.id).expect("reader");
            let mut reader = r.open_blob(BlobId::new(blob)).expect("open: same length");
            let mut sink = Vec::new();
            assert!(
                reader.read_to_end(&mut sink).is_err(),
                "{what}: a corrupted store file read back cleanly"
            );
            corrupted += 1;
        }
    }
    assert_eq!(corrupted, 1);
}

// ---------------------------------------------------------------------------
// Archives for the independent oracles (Linux CI)
// ---------------------------------------------------------------------------

/// Writes one library per entry and scenario of the viewer and export
/// catalogues below `OMA_ORACLE_OUT` (which must be empty or absent), plus
/// `export-records-plain+patched` with its patches applied, and
/// `archives.json` listing them, for `scripts/oracles/run_oracles.py`.
/// The committed fixtures are read in place and never written.
#[test]
#[ignore = "writes archives below OMA_ORACLE_OUT; run by CI's oracle step"]
fn write_oracle_archives() {
    let out = PathBuf::from(std::env::var("OMA_ORACLE_OUT").expect("set OMA_ORACLE_OUT"));
    fs::create_dir_all(&out).expect("create");
    assert!(
        fs::read_dir(&out).expect("read").next().is_none(),
        "OMA_ORACLE_OUT must be empty"
    );
    let mut listed = Vec::new();
    let mut jobs: Vec<(String, PathBuf, String, Option<String>)> = Vec::new();
    for e in ENTRIES {
        jobs.push((
            (*e).to_owned(),
            viewer_root(),
            format!("{e}.expected/content.json"),
            None,
        ));
    }
    for s in SCENARIOS {
        jobs.push((
            (*s).to_owned(),
            viewer_root(),
            format!("scenarios/{s}.content.json"),
            None,
        ));
    }
    for e in EXPORT_ENTRIES {
        jobs.push((
            (*e).to_owned(),
            export_root(),
            format!("{e}.expected/content.json"),
            Some(format!("{e}.expected/export.json")),
        ));
    }
    for s in EXPORT_SCENARIOS {
        jobs.push((
            (*s).to_owned(),
            export_root(),
            format!("scenarios/{s}.content.json"),
            Some(format!("scenarios/{s}.export.json")),
        ));
    }
    let catalogue_of = |root: &Path| {
        if root == export_root() {
            "export"
        } else {
            "viewer"
        }
    };
    for (name, root, content_rel, export_rel) in jobs {
        let content = json_under(&root, &content_rel);
        let variants: Vec<bool> = if name == "export-records-plain" {
            vec![false, true]
        } else {
            vec![false]
        };
        for patched in variants {
            let label = if patched {
                format!("{name}+patched")
            } else {
                name.clone()
            };
            let lib_dir = out.join(&label);
            fs::create_dir_all(&lib_dir).expect("library dir");
            #[cfg(unix)]
            chmod(&lib_dir, 0o700);
            let lib = Library::new(&lib_dir)
                .expect("library")
                .with_space_probe(fake_space(Some(TEST_AVAILABLE), Some(2 * TEST_AVAILABLE)));
            let id = lib.create(None).expect("create");
            import_steps(&root, &lib, &id, &content["orders"][0], &label);
            let db = dunce::canonicalize(&lib_dir)
                .expect("canonical")
                .join(id.as_str())
                .join("archive.sqlite");
            if patched {
                let e = json_under(&root, export_rel.as_deref().expect("export doc"));
                apply_patches(&db, e["patches"].as_array().expect("patches"));
            }
            drop(lib);
            listed.push(json!({
                "name": label,
                "catalogue": catalogue_of(&root),
                "archive": format!("{label}/{}", id.as_str()),
                "content": content_rel,
                "export": export_rel,
                "patched": patched,
            }));
        }
    }
    let doc = json!({"format": "oma-oracle-archives", "formatVersion": 1, "archives": listed});
    fs::write(
        out.join("archives.json"),
        serde_json::to_vec_pretty(&doc).expect("json"),
    )
    .expect("write");
}
