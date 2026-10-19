#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""Run the independent export oracles that exist so far (M6 spec 14.4).

Usage (from Apps/openmessage-archive, in the oracle virtual environment):

  OMA_ORACLE_OUT=DIR cargo test -p oma-archive --test export_fixtures -- \\
      --ignored write_oracle_archives
  python -I scripts/oracles/run_oracles.py --archives DIR

Slice 6c checks the expectation chain's first link, archive -> projection:

- every archive the Rust import built from the viewer and export
  catalogues is projected by project_archive.py (stdlib sqlite3 on a
  read-only copy) and validated against schemas/projection.schema.json;
- each projection is compared with the generator's model (content.json):
  messages, conversation members, display text, dated or not and the
  instant, attachment lists, the sender reason (the viewer model's), the
  unsent parts, blob digests and every store file re-hashed;
- the completeness identities: each conversation's item count equals its
  members in the model and, for the export catalogue, export.json's
  membership; the sum equals chat_message's row count;
- the patched archive keeps the codes an import cannot write, raw;
- golden-v1 (read-only copy) projects to its logical snapshot's counts
  and blob digests;
- export.json and hidden-vectors.json validate against their schemas, and
  each vector's text is its pieces.

Writers (TXT, JSON, CSV, HTML and later XLSX and PDF) add their links of
the chain in their slices. Output is counts and check names only.
"""
import argparse
import hashlib
import json
import os
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.dont_write_bytecode = True

import project_archive  # noqa: E402

APP = HERE.parent.parent
FIXTURES = APP / "tests" / "fixtures"
SCHEMAS = HERE / "schemas"
LONG_TEXT = 4096


class Failures:
    def __init__(self):
        self.items = []
        self.checks = 0

    def check(self, ok, what):
        self.checks += 1
        if not ok:
            self.items.append(what)


def load(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def validator(name):
    import jsonschema
    schema = load(SCHEMAS / name)
    cls = jsonschema.validators.validator_for(schema)
    cls.check_schema(schema)
    return cls(schema, format_checker=cls.FORMAT_CHECKER)


def validate(v, doc, what, fails):
    errors = sorted(v.iter_errors(doc), key=lambda e: list(e.absolute_path))
    for e in errors[:5]:
        fails.check(False, f"{what}: schema: {'/'.join(map(str, e.absolute_path))}: {e.validator}")
    fails.check(not errors, f"{what}: schema")


def model_text(display):
    """The model's display text in the projection's form, or None."""
    if "text" not in display or display["text"] is None:
        return None
    t = display["text"]
    if t.startswith("sha256:") and t.count(":") == 2:
        _, sha, n = t.split(":")
        return {"sha256": sha, "utf8Bytes": int(n)}
    return {"text": t}


def projected_text(display):
    if "noneReason" in display:
        return None
    if "text" in display:
        raw = display["text"].encode("utf-8")
        if len(raw) > LONG_TEXT:
            return {"sha256": hashlib.sha256(raw).hexdigest(), "utf8Bytes": len(raw)}
        return {"text": display["text"]}
    return {"sha256": display["sha256"], "utf8Bytes": display["utf8Bytes"]}


def compare_with_model(p, doc, export, name, fails):
    content, viewer = doc["content"], doc["viewer"]
    model_msgs = {m["ref"]: m for m in content["messages"]}
    fails.check(set(p["messages"]) == set(model_msgs), f"{name}: message refs")
    for ref, m in model_msgs.items():
        got = p["messages"].get(ref)
        if got is None:
            continue
        fails.check(got["chats"] == sorted(m["chats"]), f"{name}: {ref}: conversations")
        fails.check(projected_text(got["display"]) == model_text(m["display"]), f"{name}: {ref}: display text")
        dated = got["date"]["unit"] in ("seconds", "nanoseconds")
        fails.check(dated == ("utcNs" in m["sort"]), f"{name}: {ref}: dated")
        if dated and "utcNs" in m["sort"]:
            fails.check(int(got["date"]["utcNs"]) == m["sort"]["utcNs"], f"{name}: {ref}: instant")
        fails.check(got["attachments"] == m["attachments"], f"{name}: {ref}: attachments")
        vm = viewer["messages"].get(ref, {})
        if "sender" in vm:
            fails.check(got["sender"] == vm["sender"], f"{name}: {ref}: sender reason")
        if "unsentParts" in vm.get("edit", {}):
            fails.check(got["unsentParts"] == vm["edit"]["unsentParts"], f"{name}: {ref}: unsent parts")
    # Conversations: members, and the completeness identity.
    members = {}
    for m in content["messages"]:
        for c in m["chats"]:
            members.setdefault(c, set()).add(m["ref"])
    total = 0
    for conv in p["conversations"]:
        refs = [i["message"] for i in conv["items"]]
        total += len(refs)
        fails.check(len(refs) == len(set(refs)), f"{name}: {conv['ref']}: an item twice")
        fails.check(set(refs) == members.get(conv["ref"], set()), f"{name}: {conv['ref']}: members")
        dated = [i["dated"] for i in conv["items"]]
        fails.check(dated == sorted(dated, reverse=True), f"{name}: {conv['ref']}: dated before undated")
        if export is not None:
            want = export["membership"]["chats"].get(conv["ref"], {}).get("items")
            fails.check(want == len(refs), f"{name}: {conv['ref']}: export.json item count")
    fails.check(total == p["counts"]["chat_message"], f"{name}: items equal chat_message rows")
    if export is not None:
        fails.check(sorted(p["noConversation"]) == sorted(export["membership"]["noConversation"]),
                    f"{name}: messages in no conversation")
    # Blobs: the model's digests, and every store file re-hashed.
    fails.check(sorted(b["sha256"] for b in p["blobs"]) == sorted(b["sha256"] for b in content["blobs"]),
                f"{name}: blob digests")
    for b in p["blobs"]:
        fails.check(b["storeFile"] == "ok", f"{name}: store file {b['sha256'][:12]}: {b['storeFile']}")
    for a in content["attachments"]:
        got = p["attachments"].get(a["ref"])
        if got is not None:
            fails.check(got["sha256"] == a["sha256"], f"{name}: {a['ref']}: current blob")


def check_patched(p, export, name, fails):
    by_guid = {f"guid:{x['guid']}": x["set"] for x in export["patches"]}
    seen = 0
    for ref, s in by_guid.items():
        m = p["messages"].get(ref)
        fails.check(m is not None, f"{name}: {ref} missing")
        if m is None:
            continue
        if "is_from_me_st" in s:
            fails.check(m["direction"] == {"raw": None, "state": f"unknown:{s['is_from_me_st']}"},
                        f"{name}: {ref}: unknown direction state kept")
            fails.check(m["sender"]["reason"] == "directionNotRecorded", f"{name}: {ref}: no sender inferred")
            seen += 1
        if "date_unit" in s:
            fails.check(m["date"]["unit"] == f"unknown:{s['date_unit']}" and m["date"]["utcNs"] is None,
                        f"{name}: {ref}: unknown unit kept")
            seen += 1
        if "item_kind" in s:
            fails.check(m["itemKind"] == s["item_kind"], f"{name}: {ref}: unknown item kind kept")
            seen += 1
    fails.check(seen == 3, f"{name}: every patch observed")


def check_golden(fails, proj_v):
    lib = FIXTURES / "archives" / "golden-v1" / "library"
    dirs = [d for d in lib.iterdir() if d.is_dir()]
    fails.check(len(dirs) == 1, "golden-v1: one archive")
    p = project_archive.project(dirs[0])
    validate(proj_v, p, "golden-v1", fails)
    snap = load(FIXTURES / "archives" / "golden-v1" / "logical_snapshot.json")
    for t in ("handle", "chat", "message", "chat_message", "message_body", "attachment",
              "message_attachment", "blob", "value_history"):
        fails.check(p["counts"][t] == len(snap[t]), f"golden-v1: {t} count")
    want = sorted(b["sha256"]["blob"] for b in snap["blob"])
    fails.check(sorted(b["sha256"] for b in p["blobs"]) == want, "golden-v1: blob digests")
    fails.check(all(b["storeFile"] == "ok" for b in p["blobs"]), "golden-v1: store files")
    total = sum(len(c["items"]) for c in p["conversations"])
    fails.check(total == p["counts"]["chat_message"], "golden-v1: items equal chat_message rows")
    return p


def check_vectors(fails):
    v = validator("hidden-vectors.schema.json")
    doc = load(FIXTURES / "export" / "hidden-vectors.json")
    validate(v, doc, "hidden-vectors", fails)
    for x in doc["vectors"]:
        fails.check(x["text"] == "".join(p["text"] for p in x["pieces"]), f"vector {x['name']}: text")
        copy = "".join(p["text"] for p in x["pieces"] if p["class"] in ("keep", "cr"))
        fails.check(x["withoutHiddenCharacters"] == copy, f"vector {x['name']}: copy")
    return len(doc["vectors"])


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--archives", required=True, help="the folder write_oracle_archives wrote")
    ap.add_argument("--keep", help="also write each projection into this folder")
    a = ap.parse_args()
    import html5lib
    import jsonschema
    from importlib.metadata import version
    print(f"python {sys.version.split()[0]}, sqlite {project_archive.sqlite3.sqlite_version}, "
          f"jsonschema {version('jsonschema')}, html5lib {version('html5lib')}")
    del html5lib, jsonschema
    fails = Failures()
    proj_v = validator("projection.schema.json")
    exp_v = validator("export-expectations.schema.json")
    root = pathlib.Path(a.archives)
    listing = load(root / "archives.json")
    fails.check(listing["format"] == "oma-oracle-archives", "archives.json format")
    ties = 0
    projected = 0
    for entry in listing["archives"]:
        name = entry["name"]
        cat = FIXTURES / entry["catalogue"]
        doc = load(cat / entry["content"])
        export = load(cat / entry["export"]) if entry["export"] else None
        if export is not None:
            validate(exp_v, export, f"{name}: export.json", fails)
        p = project_archive.project(root / entry["archive"])
        projected += 1
        validate(proj_v, p, name, fails)
        if a.keep:
            os.makedirs(a.keep, exist_ok=True)
            with open(os.path.join(a.keep, name + ".projection.json"), "w", encoding="ascii",
                      newline="\n") as f:
                json.dump(p, f, indent=1, sort_keys=True, ensure_ascii=True)
        if entry["patched"]:
            check_patched(p, export, name, fails)
            continue
        compare_with_model(p, doc, export, name, fails)
        if name == "export-records-plain":
            ties = sum(i["tie"] for c in p["conversations"] for i in c["items"])
            fails.check(ties >= 2, f"{name}: the tied pair is flagged")
    fails.check(projected >= 20, "every catalogue archive projected")
    check_golden(fails, proj_v)
    vectors = check_vectors(fails)
    print(f"projected {projected} archives and golden-v1; {vectors} hidden-character vectors; "
          f"{fails.checks} checks, {len(fails.items)} failed")
    for f in fails.items[:50]:
        print(f"FAIL {f}")
    return 1 if fails.items else 0


if __name__ == "__main__":
    sys.exit(main())
