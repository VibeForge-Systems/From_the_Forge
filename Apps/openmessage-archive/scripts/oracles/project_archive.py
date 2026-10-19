#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""Project an OpenMessage Archive database into the export oracle's
expectation form (M6 spec 14.4, "Archive -> expectation").

Usage:

  project_archive.py ARCHIVE_DIR [--out FILE]

ARCHIVE_DIR is one archive's folder (it holds archive.sqlite and
attachments/). The database (and its -wal, if any) is copied into a private
temporary folder and opened read-only from there with the standard
library's sqlite3, so the archive itself is never opened by this script.
Store files are read (never written) to re-hash them.

This is an independent statement of the archive's meaning, written from
the schema (crates/oma-archive/src/schema/v1.sql) and the spec, not from
the Rust export code: per conversation, its members in timeline order
(sort key, then message id) with ties flagged; per message, the raw date
with its unit and state, the raw direction and the sender reason, the item
kind and association codes as stored, the display text or the reason there
is none, the parts listed as edited and unsent, the attachment records with
their blob SHA-256, and the value-history counts per column. The output is
deterministic JSON (sorted keys, ASCII). It carries message content, so it
is written only where the caller says, never logged.

Run with `python3 -I`. Only the standard library is used.
"""
import argparse
import hashlib
import json
import os
import pathlib
import shutil
import sqlite3
import sys
import tempfile

FORMAT = "oma-archive-projection"
FORMAT_VERSION = 1
LONG_TEXT = 4096

# `x_st` (ValueState, spec 6.1): NULL means the value is stored.
STATES = {1: "null", 2: "absent", 3: "oversized", 4: "unreadable"}
# `*_unit` (TimeUnit, M3 time.rs).
UNITS = {0: "zero", 1: "negative", 2: "seconds", 3: "nanoseconds", 4: "ambiguous"}
# `blob.verify_state` is not read: the projection re-hashes on its own.


def state_name(st):
    if st is None:
        return "value"
    return STATES.get(st, f"unknown:{st}")


def unit_name(u):
    if u is None:
        return None
    return UNITS.get(u, f"unknown:{u}")


def text_of(b):
    """Stored bytes as text: UTF-8, or None when they are not."""
    if b is None:
        return None
    try:
        return b.decode("utf-8")
    except UnicodeDecodeError:
        return None


def described(b):
    """A stored string (bytes): itself when short and UTF-8, else its
    digest, length and (when UTF-8) its first 32 characters."""
    if b is None:
        return None
    t = text_of(b)
    if t is not None and len(b) <= LONG_TEXT:
        return {"text": t}
    out = {"sha256": hashlib.sha256(b).hexdigest(), "utf8Bytes": len(b)}
    if t is None:
        out["notUtf8"] = True
    else:
        out["prefix"] = t[:32]
    return out


def key_ref(guid, kind):
    if kind == 2:
        return "unkeyed"
    t = text_of(guid)
    return f"guid:{t}" if t is not None else f"guid-hex:{guid.hex()}"


def handle_ref(address, service, service_st, kind):
    if kind == 2:
        return "unkeyed"
    a = text_of(address)
    a = a if a is not None else f"hex:{address.hex()}"
    if service is not None:
        s = text_of(service)
        s = s if s is not None else f"hex:{service.hex()}"
    else:
        s = {1: "(null)", 2: "(absent)", 3: "(oversized)"}.get(service_st, "(unreadable)")
    return f"handle:{a}|{s}"


def open_copy(archive_dir, tmp):
    src = pathlib.Path(archive_dir) / "archive.sqlite"
    if not src.is_file():
        raise SystemExit(f"no archive.sqlite in the given folder")
    dst = pathlib.Path(tmp) / "archive.sqlite"
    shutil.copyfile(src, dst)
    wal = src.with_name("archive.sqlite-wal")
    if wal.is_file():
        shutil.copyfile(wal, dst.with_name("archive.sqlite-wal"))
    os.chmod(dst, 0o400)
    con = sqlite3.connect(dst.as_uri() + "?mode=ro", uri=True)
    con.text_factory = bytes
    return con


def rows(con, sql, args=()):
    return con.execute(sql, args).fetchall()


def project(archive_dir):
    with tempfile.TemporaryDirectory(prefix="oma-projection-") as tmp:
        os.chmod(tmp, 0o700)
        con = open_copy(archive_dir, tmp)
        try:
            return _project(con, pathlib.Path(archive_dir))
        finally:
            con.close()


def _project(con, archive_dir):
    (schema_version,) = con.execute("SELECT schema_version FROM archive_meta").fetchone()

    handles = {}
    for hid, kind, addr, svc, svc_st in rows(
            con, "SELECT id, key_kind, address, service, service_st FROM handle"):
        handles[hid] = handle_ref(addr, svc, svc_st, kind)

    chats = {}
    for cid, kind, guid, name, name_st in rows(
            con, "SELECT id, key_kind, guid, display_name, display_name_st FROM chat"):
        chats[cid] = {"ref": key_ref(guid, kind), "displayName": described(name),
                      "displayNameState": state_name(name_st)}

    # Messages: keyed by GUID; unkeyed ones by the 1-based position of their
    # fingerprint among the distinct fingerprints in byte order, and their
    # occurrence (the import catalogue's naming).
    mrows = rows(con, "SELECT id, key_kind, guid, unkeyed_fp, unkeyed_occ FROM message")
    fps = sorted({fp for _, k, _, fp, _ in mrows if k == 2})
    mref = {}
    for mid, kind, guid, fp, occ in mrows:
        mref[mid] = key_ref(guid, kind) if kind == 1 else f"unkeyed:message:{fps.index(fp) + 1}:{occ}"

    atts = {}
    for aid, kind, guid, name, name_st, blob_sha in rows(
            con, "SELECT a.id, a.key_kind, a.guid, a.transfer_name, a.transfer_name_st, b.sha256 "
                 "FROM attachment a LEFT JOIN blob b ON b.id = a.current_blob_id"):
        atts[aid] = {"ref": key_ref(guid, kind), "transferName": described(name),
                     "transferNameState": state_name(name_st),
                     "sha256": None if blob_sha is None else blob_sha.hex()}

    links = {}
    for mid, aid in rows(con, "SELECT message_id, attachment_id FROM message_attachment "
                              "ORDER BY message_id, ord_rank DESC, ord_rowid, attachment_id"):
        links.setdefault(mid, []).append(aid)

    unsent = {}
    for body, part in rows(con, "SELECT body_id, part FROM message_unsent_part ORDER BY body_id, ordinal"):
        unsent.setdefault(body, []).append(part)
    edited = {}
    for body, part in rows(con, "SELECT DISTINCT body_id, part FROM message_edit_entry ORDER BY body_id, part"):
        edited.setdefault(body, []).append(part)
    versions = dict(rows(con, "SELECT message_id, count(*) FROM message_body GROUP BY message_id"))

    history = {}
    for row_id, col, n in rows(con, "SELECT row_id, col, count(*) FROM value_history "
                                    "WHERE entity = 3 GROUP BY row_id, col"):
        history.setdefault(row_id, {})[str(col)] = n

    in_chats = {}
    for cid, mid in rows(con, "SELECT chat_id, message_id FROM chat_message"):
        in_chats.setdefault(mid, []).append(chats[cid]["ref"])

    messages = {}
    for r in rows(con, """
            SELECT m.id, m.date_raw, m.date_st, m.date_unit, m.date_utc_ns,
                   m.is_from_me, m.is_from_me_st, m.handle_rowid, m.handle_rowid_st, m.handle_ref,
                   m.item_kind, m.item_type, m.item_type_st, m.assoc_present, m.assoc_kind,
                   b.id, b.display_text, b.display_source, b.display_none_reason, b.edit_label
            FROM message m LEFT JOIN message_body b ON b.id = m.current_body_id"""):
        (mid, d_raw, d_st, d_unit, d_utc, fm, fm_st, h_raw, h_st, h_ref, item_kind, item_type,
         item_type_st, assoc_present, assoc_kind, body, text, source, none_reason, edit_label) = r
        # The sender reason: fromMe for is_from_me = 1; directionNotRecorded
        # for any other value or no value but 0; then from handle_id.
        if fm_st is None and fm == 1:
            sender = {"reason": "fromMe"}
        elif fm_st is not None or fm != 0:
            sender = {"reason": "directionNotRecorded"}
        elif h_st == 2:
            sender = {"reason": "columnAbsent"}
        elif h_st in (3, 4) or (h_st is not None and h_st not in STATES):
            sender = {"reason": "unreadable"}
        elif h_st == 1 or h_raw == 0:
            sender = {"reason": "noSender"}
        elif h_ref is None:
            sender = {"reason": "notFound"}
        else:
            sender = {"reason": "resolved", "handle": handles[h_ref]}
        messages[mref[mid]] = {
            "chats": sorted(in_chats.get(mid, [])),
            "date": {"raw": None if d_raw is None else str(d_raw), "state": state_name(d_st),
                     "unit": unit_name(d_unit), "utcNs": None if d_utc is None else str(d_utc)},
            "direction": {"raw": fm, "state": state_name(fm_st)},
            "sender": sender,
            "itemKind": item_kind,
            "itemType": {"raw": item_type, "state": state_name(item_type_st)},
            "association": {"present": assoc_present, "kind": assoc_kind},
            "display": ({"noneReason": none_reason} if text is None
                        else {"source": source, **described(text)}),
            "editLabel": edit_label,
            "editedParts": edited.get(body, []),
            "unsentParts": unsent.get(body, []),
            "bodyVersions": versions.get(mid, 0),
            "attachments": [atts[a]["ref"] for a in links.get(mid, [])],
            "valueHistory": history.get(mid, {}),
        }

    # Timelines: sort key, then message id; ties are equal dated instants.
    conversations = []
    for cid in sorted(chats, key=lambda c: chats[c]["ref"]):
        items = rows(con, """
            SELECT cm.message_id, m.date_utc_ns
            FROM chat_message cm JOIN message m ON m.id = cm.message_id
            WHERE cm.chat_id = ? ORDER BY cm.sort_key, cm.message_id""", (cid,))
        out = []
        for i, (mid, utc) in enumerate(items):
            tie = utc is not None and any(
                0 <= j < len(items) and j != i and items[j][1] == utc for j in (i - 1, i + 1))
            out.append({"message": mref[mid], "dated": utc is not None, "tie": tie})
        conversations.append({**chats[cid], "items": out})

    blobs = []
    store = archive_dir / "attachments"
    for sha, byte_len, name in rows(con, "SELECT sha256, byte_len, storage_name FROM blob ORDER BY sha256"):
        name = name.decode("ascii")
        state = "notChecked"
        if store.is_dir():
            f = store / name[:2] / name
            if not f.is_file():
                state = "missing"
            else:
                h = hashlib.sha256()
                with open(f, "rb") as fh:
                    for chunk in iter(lambda: fh.read(1 << 20), b""):
                        h.update(chunk)
                state = "ok" if h.digest() == sha else "mismatch"
        blobs.append({"sha256": sha.hex(), "byteLen": byte_len, "storeFile": state})

    count = lambda t: con.execute(f"SELECT count(*) FROM {t}").fetchone()[0]
    return {
        "format": FORMAT, "formatVersion": FORMAT_VERSION, "schemaVersion": schema_version,
        "counts": {t: count(t) for t in ("handle", "chat", "message", "chat_message", "message_body",
                                         "attachment", "message_attachment", "blob", "value_history")},
        "conversations": conversations,
        "messages": dict(sorted(messages.items())),
        "noConversation": sorted(mref[m] for m in mref if m not in in_chats),
        "attachments": dict(sorted((a["ref"], a) for a in atts.values() if a["ref"] != "unkeyed")),
        "blobs": blobs,
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("archive_dir")
    ap.add_argument("--out", help="write the projection here (default: standard output)")
    a = ap.parse_args()
    p = project(a.archive_dir)
    text = json.dumps(p, indent=1, sort_keys=True, ensure_ascii=True) + "\n"
    if a.out:
        with open(a.out, "w", encoding="ascii", newline="\n") as f:
            f.write(text)
    else:
        sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
