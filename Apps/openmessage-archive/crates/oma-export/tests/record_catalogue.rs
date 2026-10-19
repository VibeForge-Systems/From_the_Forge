//! The canonical record over both synthetic catalogues (M6 spec §4.2,
//! §4.8, §4.9, §14.2 "Records", slice 6d).
//!
//! Every entry and scenario of `tests/fixtures/viewer` and
//! `tests/fixtures/export` is imported (the export catalogue with its
//! `patches`, codes an import cannot write), then mapped whole, history
//! included:
//!
//! - every in-scope message is exported, ordinals are dense, and a message
//!   in two conversations has one ordinal and the same intrinsic record;
//! - no D94 key exists anywhere, and tagged absence is never `null`;
//! - unknown codes keep their numbers;
//! - nothing masked or withheld reaches a record unless revealed, and each
//!   sentinel appears with the reveal (the record half of the masking
//!   oracle; the writers' half is slice 6f's);
//! - every column the ledger exports appears at its path, and the
//!   opt-in identifiers appear only under the D141 opt-in;
//! - segment offsets are taken after masking;
//! - parity with the viewer: every text the details show for a message is
//!   in its record, with the documented exceptions.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::*;
use oma_archive::{
    BodyView, EditEntryText, HistoryColumn, MessageId, OptContent, Reveal, SegmentKind, TextPiece,
    VersionText,
};
use oma_export::ledger::{Disposition, LEDGER};
use oma_export::IdentifierMode;
use serde_json::Value;

const REVEAL_ALL: Reveal = Reveal {
    earlier_text: true,
    one_time_code: true,
};

/// The M5 key list of read, delivery and sent state (D94).
const D94_KEYS: &[&str] = &[
    "isRead",
    "isDelivered",
    "isSent",
    "dateRead",
    "dateDelivered",
    "datePlayed",
    "error",
    "lastRead",
    "is_read",
    "is_delivered",
    "is_sent",
    "date_read",
    "date_delivered",
    "date_played",
    "last_read",
    "read",
    "delivered",
];

/// Keys whose value is tagged absence: never `null`.
const TAGGED: &[&str] = &[
    "source",
    "history",
    "reactions",
    "replyCount",
    "earlierText",
];

fn imported(doc: &Doc) -> Imported {
    let i = import(doc);
    if let Some(e) = &doc.export {
        apply_patches(&i.db(), e["patches"].as_array().expect("patches"));
    }
    i
}

fn walk(i: &Imported, reveal: Reveal, ids: IdentifierMode) -> Walked {
    with_snapshot(i, |s| walk_archive(s, reveal, options(true, ids)))
}

fn walk_keys(v: &Value, path: &str, out: &mut BTreeSet<String>, banned: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if D94_KEYS.contains(&k.as_str()) {
                    banned.push(format!("{path}.{k}"));
                }
                if TAGGED.contains(&k.as_str()) && x.is_null() {
                    banned.push(format!("{path}.{k} is null"));
                }
                let p = format!("{path}.{k}");
                out.insert(p.clone());
                walk_keys(x, &p, out, banned);
            }
        }
        Value::Array(a) => {
            let p = format!("{path}[]");
            for x in a {
                walk_keys(x, &p, out, banned);
            }
        }
        _ => {}
    }
}

/// Every key path of `w`'s records, with roots `item`, `attachment`,
/// `conversation`, `handle` and `version`, and every banned key found.
fn paths(w: &Walked) -> (BTreeSet<String>, Vec<String>) {
    let mut out = BTreeSet::new();
    let mut banned = Vec::new();
    let ser = |x: &dyn erased::Ser| x.value();
    for i in w.items.iter().chain(&w.folded) {
        let v = ser(i);
        walk_keys(&v, "item", &mut out, &mut banned);
        for a in v["attachments"].as_array().expect("attachments") {
            walk_keys(a, "attachment", &mut out, &mut banned);
        }
    }
    for x in &w.versions {
        walk_keys(&ser(x), "version", &mut out, &mut banned);
    }
    for x in &w.conversations {
        walk_keys(&ser(x), "conversation", &mut out, &mut banned);
    }
    for x in &w.handles {
        walk_keys(&ser(x), "handle", &mut out, &mut banned);
    }
    (out, banned)
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn every_message_maps_once_with_dense_ordinals_and_one_intrinsic_record() {
    let mut checked = 0;
    for doc in docs() {
        let i = imported(&doc);
        let w = walk(&i, Reveal::default(), IdentifierMode::Ordinals);
        let what = &doc.name;
        let ns: BTreeSet<u64> = w.items.iter().chain(&w.folded).map(|r| r.n).collect();
        let want: BTreeSet<u64> = (1..=u64::try_from(w.scope_len).expect("len")).collect();
        assert_eq!(ns, want, "{what}: ordinals");
        // One ordinal per message: the intrinsic part is the same in every
        // conversation file; only the context differs.
        let mut by_n: BTreeMap<u64, Value> = BTreeMap::new();
        for r in w.items.iter().chain(&w.folded) {
            let mut v = serde_json::to_value(r).expect("json");
            v.as_object_mut().expect("object").remove("context");
            if let Some(prev) = by_n.insert(r.n, v.clone()) {
                assert_eq!(prev, v, "{what}: item {} differs between files", r.n);
            }
        }
        let (_, banned) = paths(&w);
        assert!(banned.is_empty(), "{what}: {banned:?}");
        assert_eq!(
            w.counts.items,
            u64::try_from(w.items.len() + w.folded.len()).expect("len")
        );
        // Conversation ordinals are ranks among the exported conversations.
        let cn: Vec<u64> = w.conversations.iter().map(|c| c.n).collect();
        assert_eq!(cn, (1..=cn.len() as u64).collect::<Vec<_>>(), "{what}");
        checked += w.items.len();
    }
    assert!(checked > 100, "{checked}");
}

#[test]
fn unknown_codes_keep_their_numbers() {
    let doc = docs()
        .into_iter()
        .find(|d| d.name == "export-records-plain")
        .expect("doc");
    let i = imported(&doc);
    let w = walk(&i, Reveal::default(), IdentifierMode::ArchiveAndSource);
    let by_guid = |g: &str| -> Value {
        w.items
            .iter()
            .map(|r| serde_json::to_value(r).expect("json"))
            .find(|v| v["source"]["value"]["guid"]["text"] == g)
            .unwrap_or_else(|| panic!("{g}"))
    };
    let kind = by_guid("SYNTH-EXP-REC-MSG-0005");
    assert_eq!(
        kind["item"],
        serde_json::json!({"token": "unrecognisedCode", "code": 99})
    );
    let dir = by_guid("SYNTH-EXP-REC-MSG-0004");
    assert_eq!(dir["direction"]["token"]["token"], "notRecorded");
    assert_eq!(dir["direction"]["state"]["code"], 7);
    let unit = by_guid("SYNTH-EXP-REC-MSG-0014");
    assert_eq!(unit["date"]["unit"]["code"], 9);
    assert_eq!(unit["date"]["unit"]["token"], "unrecognisedCode");
    assert!(unit["date"]["utc"].is_null() && unit["date"]["raw"].is_string());
}

#[test]
fn nothing_masked_reaches_a_record_unless_revealed() {
    let mut checked = 0;
    for doc in docs().into_iter().filter(|d| d.export.is_some()) {
        let e = doc.export.as_ref().expect("export");
        let what = &doc.name;
        let i = imported(&doc);
        let sentinels: Vec<String> = [
            "oneTimeCodes",
            "earlierText",
            "editText",
            "subjectHistory",
            "alternativeText",
        ]
        .iter()
        .flat_map(|k| strings(&e["sentinels"][k]))
        .collect();
        let hidden = walk(&i, Reveal::default(), IdentifierMode::Ordinals).text();
        for s in &sentinels {
            assert!(!hidden.contains(s.as_str()), "{what}: {s} leaked");
        }
        for s in strings(&e["visible"]) {
            assert!(hidden.contains(s.as_str()), "{what}: {s} not exported");
        }
        let shown = walk(&i, REVEAL_ALL, IdentifierMode::Ordinals).text();
        for s in &sentinels {
            assert!(shown.contains(s.as_str()), "{what}: {s} not revealed");
            checked += 1;
        }
    }
    assert!(checked >= 5, "{checked}");
}

#[test]
fn every_exported_column_appears_at_its_ledger_path() {
    let mut plain = BTreeSet::new();
    let mut opted = BTreeSet::new();
    for doc in docs() {
        let i = imported(&doc);
        plain.extend(paths(&walk(&i, Reveal::default(), IdentifierMode::Ordinals)).0);
        opted.extend(
            paths(&walk(
                &i,
                Reveal::default(),
                IdentifierMode::ArchiveAndSource,
            ))
            .0,
        );
    }
    for e in LEDGER {
        match e.disposition {
            Disposition::Raw { path } | Disposition::Derived { path, .. } => {
                assert!(plain.contains(path), "{}: {path} missing", e.code);
            }
            Disposition::Excluded {
                opt_in_path: Some(path),
                ..
            } => {
                assert!(
                    opted.contains(path),
                    "{}: {path} missing with the opt-in",
                    e.code
                );
                assert!(
                    !plain.contains(path),
                    "{}: {path} without the opt-in",
                    e.code
                );
            }
            Disposition::Excluded { .. } => {}
        }
    }
    // No archive id or source identifier by default (Q5's lean).
    assert!(!plain
        .iter()
        .any(|p| p.contains("archiveId") || p.contains("guid")));
    assert!(opted.contains("item.source.value.archiveId"));
}

#[test]
fn segment_offsets_are_taken_after_masking() {
    let mut withheld = 0;
    for doc in docs().into_iter().filter(|d| d.export.is_some()) {
        let i = imported(&doc);
        for w in [
            walk(&i, Reveal::default(), IdentifierMode::Ordinals),
            walk(&i, REVEAL_ALL, IdentifierMode::Ordinals),
        ] {
            for r in &w.items {
                let v = serde_json::to_value(r).expect("json");
                let body = &v["body"];
                if body["kind"] != "text" {
                    continue;
                }
                let mut at = 0u64;
                for s in body["segments"].as_array().expect("segments") {
                    assert_eq!(s["utf16Start"].as_u64(), Some(at));
                    let len = s["utf16Len"].as_u64().expect("len");
                    match s["text"].as_str() {
                        Some(t) => assert_eq!(len, t.encode_utf16().count() as u64),
                        None => assert_eq!(len, 0, "a non-text segment has no length"),
                    }
                    if s["kind"]["token"] == "withheld" {
                        withheld += 1;
                        assert!(s["text"].is_null() && s["withheld"]["token"] == "oneTimeCode");
                    }
                    at += len;
                }
                assert_eq!(body["utf16Len"].as_u64(), Some(at));
            }
        }
    }
    assert!(withheld > 0, "no withheld segment checked");
}

// ---------------------------------------------------------------------------
// Viewer parity
// ---------------------------------------------------------------------------

fn push(o: &OptContent, out: &mut Vec<String>) {
    if let OptContent::Text(t) = o {
        out.push(t.text.expose().clone());
    }
}

fn push_pieces(p: &[TextPiece], out: &mut Vec<String>) {
    for x in p {
        if let TextPiece::Text(t) = x {
            out.push(t.text.expose().clone());
        }
    }
}

/// Every text the viewer's details show for message `id` with no reveal,
/// except the subject history the export withholds (§3.8, the documented
/// exception) when `subject_withheld`.
fn viewer_texts(d: &oma_archive::MessageDetail, subject_withheld: bool) -> Vec<String> {
    let mut out = Vec::new();
    if let BodyView::Text(b) = &d.message.body {
        for s in &b.segments {
            if let SegmentKind::Text(t) = &s.kind {
                out.push(t.text.expose().clone());
            }
        }
    }
    push(&d.subject, &mut out);
    push(&d.account, &mut out);
    for h in &d.history {
        if !(subject_withheld && h.column == HistoryColumn::Subject) {
            push(&h.value, &mut out);
        }
    }
    for v in &d.versions {
        if let VersionText::Text(p) = &v.text {
            push_pieces(p, &mut out);
        }
        for e in &v.edits {
            if let EditEntryText::Text(p) = &e.text {
                push_pieces(p, &mut out);
            }
        }
    }
    for a in &d.attachments {
        for o in [&a.name, &a.declared_type, &a.uti, &a.emoji_description] {
            push(o, &mut out);
        }
        for h in &a.history {
            push(&h.value, &mut out);
        }
    }
    if let Some(e) = d.message.event.as_ref().and_then(|e| e.title.as_ref()) {
        push(e, &mut out);
    }
    if let Some(e) = d
        .message
        .association
        .as_ref()
        .and_then(|a| a.emoji.as_ref())
    {
        push(e, &mut out);
    }
    out.retain(|s| !s.is_empty());
    out
}

/// A string as it appears inside JSON text (escaped, without quotes).
fn escaped(s: &str) -> String {
    let q = serde_json::to_string(s).expect("json");
    q[1..q.len() - 1].to_owned()
}

#[test]
fn the_record_never_shows_less_than_the_viewer() {
    let mut compared = 0;
    for doc in docs() {
        let what = &doc.name;
        let i = imported(&doc);
        let w = walk(&i, Reveal::default(), IdentifierMode::ArchiveAndSource);
        // The record text of each message: its item(s) and its versions.
        let mut by_id: BTreeMap<i64, String> = BTreeMap::new();
        let mut n_to_id: BTreeMap<u64, i64> = BTreeMap::new();
        let mut subject_withheld: BTreeSet<i64> = BTreeSet::new();
        let mut stored: BTreeMap<i64, (u64, usize)> = BTreeMap::new();
        for r in w.items.iter().chain(&w.folded) {
            let v = serde_json::to_value(r).expect("json");
            let id: i64 = v["source"]["value"]["archiveId"]
                .as_str()
                .expect("archive id")
                .parse()
                .expect("decimal");
            n_to_id.insert(r.n, id);
            if v["valueHistory"]
                .as_array()
                .expect("history")
                .iter()
                .any(|h| h["state"] == "withheld" && h["column"]["token"] == "subject")
            {
                subject_withheld.insert(id);
            }
            stored.insert(
                id,
                (
                    v["edit"]["storedVersions"].as_u64().expect("versions"),
                    v["attachments"].as_array().expect("attachments").len(),
                ),
            );
            by_id
                .entry(id)
                .or_default()
                .push_str(&serde_json::to_string(&v).expect("json"));
        }
        let mut versions: BTreeMap<i64, usize> = BTreeMap::new();
        for x in &w.versions {
            let id = n_to_id[&x.message];
            *versions.entry(id).or_default() += 1;
            by_id
                .entry(id)
                .or_default()
                .push_str(&serde_json::to_string(x).expect("json"));
        }
        let r = i.lib.open_read(&i.id).expect("reader");
        for (id, text) in &by_id {
            let d = r
                .message_detail(MessageId::new(*id), Reveal::default())
                .expect("detail");
            for s in viewer_texts(&d, subject_withheld.contains(id)) {
                assert!(
                    text.contains(&escaped(&s)),
                    "{what}: message {id} lost a value the viewer shows"
                );
                compared += 1;
            }
            let (sv, atts) = stored[id];
            assert_eq!(u64::from(d.message.edit.stored_versions), sv, "{what}");
            assert_eq!(d.attachments.len(), atts, "{what}: attachments");
            assert_eq!(
                d.versions.len() + usize::try_from(d.more_versions).expect("n"),
                versions.get(id).copied().unwrap_or(0),
                "{what}: versions"
            );
        }
    }
    assert!(compared > 200, "{compared}");
}

// ---------------------------------------------------------------------------
// References outside the scope
// ---------------------------------------------------------------------------

/// Every message reference in `v`: `(scope, n)`.
fn refs(v: &Value, out: &mut Vec<(String, Option<u64>)>) {
    match v {
        Value::Object(m) => {
            if let Some(Value::String(s)) = m.get("scope") {
                out.push((s.clone(), m.get("n").and_then(Value::as_u64)));
            }
            for x in m.values() {
                refs(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| refs(x, out)),
        _ => {}
    }
}

#[test]
fn references_outside_a_conversation_export_carry_no_id() {
    use oma_archive::{ExportScopeIds, TimeRange};
    use oma_export::map::{ExportZone, Mapper, Placement};
    use oma_export::Ordinals;
    let (mut outside, mut elsewhere) = (0, 0);
    for doc in docs() {
        let what = &doc.name;
        let i = imported(&doc);
        with_snapshot(&i, |s| {
            for chat in s.chat_ids()? {
                let scope = s.scope_index(&ExportScopeIds::Conversation {
                    chat,
                    range: TimeRange::All,
                })?;
                let len = scope.len() as u64;
                let opts = options(false, IdentifierMode::Ordinals);
                let mut m = Mapper::new(
                    Ordinals::new(scope, vec![chat], opts.identifiers),
                    ExportZone::Utc,
                    opts,
                );
                let mut cur = s.timeline(chat, TimeRange::All);
                let mut files = CopyAll::default();
                while let Some(b) = cur.next_batch()? {
                    for x in
                        s.assemble(&b.items, Reveal::default(), oma_archive::ExportDepth::Items)?
                    {
                        let at = Placement {
                            conversation: Some(chat),
                            folded_into: None,
                            order_tie: false,
                        };
                        let r = m.item(s, &x, at, &mut files)?;
                        let v = serde_json::to_value(&r).expect("json");
                        let mut found = Vec::new();
                        refs(&v, &mut found);
                        for (scope, n) in found {
                            match scope.as_str() {
                                "inExport" => {
                                    assert!(n.is_some_and(|n| (1..=len).contains(&n)), "{what}")
                                }
                                "notInExport" => {
                                    assert_eq!(n, None, "{what}: an id outside the scope");
                                    outside += 1;
                                }
                                "notRecorded" => assert_eq!(n, None, "{what}"),
                                other => panic!("{what}: scope {other}"),
                            }
                        }
                        let also = &v["context"]["alsoIn"];
                        assert!(also["inExport"].as_array().expect("in").is_empty());
                        elsewhere += also["elsewhere"].as_u64().expect("elsewhere");
                        assert!(!v.to_string().contains("archiveId"));
                    }
                }
            }
            Ok(())
        });
    }
    assert!(outside > 0, "no reference outside a scope was checked");
    assert!(
        elsewhere > 0,
        "no message in another conversation was checked"
    );
}
