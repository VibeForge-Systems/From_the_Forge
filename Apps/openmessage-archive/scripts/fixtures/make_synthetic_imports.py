#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""Build the SYNTHETIC import catalogue for OpenMessage Archive (M4).

Every value written here is invented: example.invalid addresses, +1555
numbers marked SYNTH, and sentinel text. No real device, person, message or
backup is used, and nothing from any sample database is copied.

Usage:

  make_synthetic_imports.py OUT_DIR [--seed N] [--allow-other-sqlite]

Writes, under OUT_DIR:

  <entry>/SYNTHETIC0-0000000000000000/  one backup (the import input)
  <entry>.expected/content.json         the content manifest after importing
                                        <entry> alone into an empty archive
  <entry>.expected/attachments/<fileID> attachment plaintexts (encrypted entries)
  scenarios/<name>.content.json         the content manifest after a sequence
                                        of imports (one or more orders)
  SHA256SUMS                            the inputs (every backup file)
  EXPECTED.SHA256SUMS                   every file under *.expected/ and scenarios/
  README.md                             generated; do not edit

The content manifests are the generator's own model of what the M4 import
must leave in the archive (spec sections 6.2, 6.5, 8.7, 8.8, 9 and D58). The
model is a second statement of those rules, applied to the rows the
generator wrote as SQLite stored them; it does not run the Rust code.
Agreement between the two proves self-consistency only.

It imports make_synthetic_messages (the M3 generator) as a module for the
sms.db writer, the value-reading rules, the typedstream and summary blobs
and the attachment path rules, and make_synthetic_backups (the M2
generator) for the backup format. Both are left untouched.

SQLite file bytes depend on the SQLite version, so the script refuses to
write under any SQLite but the reference 3.45.1 unless --allow-other-sqlite
is given. Run with `python -I`.
"""
import argparse
import datetime as dt
import hashlib
import json
import os
import plistlib
import random
import shutil
import sqlite3
import struct
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.dont_write_bytecode = True  # importing the sibling generators leaves no __pycache__

import make_synthetic_backups as mb  # noqa: E402
import make_synthetic_messages as ms  # noqa: E402
import make_synthetic_typedstream as ts  # noqa: E402
import sms_profiles as sp  # noqa: E402

from cryptography.hazmat.primitives.keywrap import aes_key_wrap  # noqa: E402

REFERENCE_SQLITE = "3.45.1"
UDID = mb.SYNTHETIC_UDID
PASSWORD = mb.DEFAULT_PASSWORD
NS = ms.NS
T0_NS = ms.T0_NS
T0_S = ms.T0_S
MIB = 1024 * 1024

# The rank's date bounds (spec 8.7): 2007-01-01T00:00:00Z up to the import
# start plus 48 hours. The model imports "after" 2027-01-01; every fixture
# date that is meant to count lies in 2026.
DATE_FLOOR_NS = 1_167_609_600 * NS
IMPORT_START_NS = 1_798_761_600 * NS
DATE_CEILING_NS = IMPORT_START_NS + 48 * 3600 * NS

# R3: message.payload_data is read as a blob under 1 MiB when raw blobs are kept.
CAP_PAYLOAD_DATA = MIB

# The closed list of history columns (spec 8.7, D82), by source column name.
HISTORY = {
    "message": {"subject", "group_title", "payload_data", "destination_caller_id"},
    "chat": {"display_name", "room_name"},
    "attachment": {"filename", "transfer_name", "emoji_image_short_description"},
    "handle": {"uncanonicalized_id", "person_centric_id"},
}

# The columns each manifest shows (a sample, spec 18.3), by source name.
SAMPLED = {
    "handle": ["id", "service", "country", "uncanonicalized_id", "person_centric_id"],
    "chat": ["guid", "style", "chat_identifier", "service_name", "display_name", "room_name",
             "group_id", "is_archived"],
    "message": ["guid", "is_from_me", "service", "date", "is_read", "is_delivered", "subject",
                "item_type", "group_action_type", "group_title", "associated_message_type",
                "associated_message_guid", "payload_data", "date_edited", "date_retracted",
                "cache_has_attachments", "destination_caller_id"],
    "attachment": ["guid", "filename", "transfer_name", "mime_type", "total_bytes", "is_sticker",
                   "emoji_image_short_description"],
}

ENTITY = {"handle": "handle", "chat": "chat", "message": "message", "attachment": "attachment"}


def dt_utc(y, mo, d, h=0, mi=0, s=0):
    """A naive datetime, which plistlib writes as UTC."""
    return dt.datetime(y, mo, d, h, mi, s)


def unix_ns(d):
    return int((d.replace(tzinfo=dt.timezone.utc) - dt.datetime(1970, 1, 1, tzinfo=dt.timezone.utc))
               .total_seconds()) * NS


# --------------------------------------------------------------------------
# Rows
# --------------------------------------------------------------------------

def import_profile(drop=()):
    """Every known column (no unknown one), minus `drop` [(table, column)]."""
    p = sp.full()
    p["message"].remove("synth_unknown_column")
    for t, c in drop:
        p[t].remove(c)
    return p


def mguid(n, prefix="SYNTH-MSG-GUID-"):
    return f"{prefix}{n:04d}"


def M(n, rowid, prefix="SYNTH-MSG-GUID-", base_ns=T0_NS, **kw):
    """Message n (its GUID and date follow n, never the rowid)."""
    r = ms.msg(rowid, guid=mguid(n, prefix), date=base_ns + n * 60 * NS, text=f"SYNTH-BODY-{n}")
    r.update(kw)
    return r


def H(rowid, ident, service, country="us", unc=None, pci=None):
    return {"ROWID": rowid, "id": ident, "service": service, "country": country,
            "uncanonicalized_id": unc, "person_centric_id": pci}


def C(rowid, guid, style=45, ident=None, service="iMessage", room=None, name=None, group=None,
      **kw):
    r = {"ROWID": rowid, "guid": guid, "style": style, "chat_identifier": ident,
         "service_name": service, "room_name": room, "display_name": name, "group_id": group,
         "properties": None, "is_archived": 0, "is_filtered": 0, "is_blackholed": 0,
         "is_pending_review": 0, "is_recovered": 0, "last_read_message_timestamp": 0}
    r.update(kw)
    return r


def A(rowid, guid, filename, payload_len, **kw):
    name = None if filename is None else filename.rsplit("/", 1)[-1]
    r = {"ROWID": rowid, "guid": guid, "filename": filename, "uti": "public.heic",
         "mime_type": "image/heic", "transfer_name": name, "total_bytes": payload_len,
         "is_sticker": 0, "hide_attachment": 0, "is_outgoing": 0, "transfer_state": 5,
         "created_date": T0_S, "start_date": 0, "emoji_image_short_description": None,
         "original_guid": None}
    r.update(kw)
    return r


class AttFile(ms.AttFile):
    """A MediaDomain file; `declared` overrides the Manifest Size."""

    def __init__(self, rel, payload=b"", state="present", declared=None):
        super().__init__(rel, payload, state)
        self.declared = declared


def media(path):
    """The MediaDomain relative path of a `~/Library/...` filename."""
    assert path.startswith("~/")
    return path[2:]


class Content:
    """One sms.db (and its MediaDomain files), shared by entries that differ
    only in their plists or their encryption."""

    def __init__(self, name, profile, rows, files=(), decl=None, wal_rows=()):
        self.spec = ms.Spec(name, profile, rows)
        self.spec.decl = dict(decl or {})
        if wal_rows:
            self.spec.wal = "wal"
            self.spec.wal_rows = list(wal_rows)
        self.files = list(files)
        self._built = None

    def built(self, seed):
        """(main bytes, wal bytes or None), built once."""
        if self._built is None:
            rng = random.Random(f"{seed}:content:{self.spec.name}")
            with tempfile.TemporaryDirectory() as work:
                self._built = ms.build_db(self.spec, work, rng)
        return self._built


# ---- device 1 ---------------------------------------------------------------

A_PATHS = {
    1: "~/Library/SMS/Attachments/a1/01/SYNTH-0001/SYNTH-FILE-0001.heic",
    2: "~/Library/SMS/Attachments/a2/02/SYNTH-0002/SYNTH-FILE-0002.jpeg",
    3: "~/Library/SMS/Attachments/a3/03/SYNTH-0003/SYNTH-FILE-0003.heic",
    4: "~/Library/SMS/Attachments/a4/04/SYNTH-0004/SYNTH-FILE-0004.png",
    5: "~/Library/SMS/Attachments/a5/05/SYNTH-0005/SYNTH-FILE-0005.heic",
    6: "~/Library/SMS/Attachments/a6/06/SYNTH-0006/SYNTH-FILE-0006.heic",
    7: "~/Library/SMS/Attachments/a7/07/SYNTH-0007/SYNTH-FILE-0007.jpeg",
    8: "~/Library/SMS/StickerCache/a8/SYNTH-0008/SYNTH-FILE-0008.heic",
    9: "~/Library/SMS/Attachments/a9/09/SYNTH-0009/SYNTH-FILE-0009.heic",
    10: "~/Library/SMS/Attachments/aa/10/SYNTH-0010/SYNTH-FILE-0010.heic",
}
A_BYTES = {
    1: b"synthetic heic bytes 0001\n",
    2: b"synthetic jpeg bytes 0002\n",
    3: b"synthetic heic bytes 0003\n",
    4: b"synthetic png bytes 0004 v1\n",
    5: b"synthetic heic bytes 0005\n",
    6: b"synthetic heic bytes 0006\n",
    8: b"synthetic sticker bytes 0008\n",
    9: b"synthetic heic bytes 0009 (recently deleted only)\n",
    10: b"synthetic heic bytes 0010 (shared with a live message)\n",
}
A_BYTES[7] = A_BYTES[2]                                  # the same bytes under two GUIDs
A4_V2 = b"synthetic png bytes 0004, version two\n"       # new bytes, a different length


def att_guid(n):
    return f"SYNTH-ATT-GUID-{n:04d}"


def dev1_rows(version, ids):
    """Device 1, `version` "v1", "v2" or "rd". `ids` maps logical names
    ("h1", "c2", "m15", "a4", ...) to the source rowid of this version."""
    h = lambda n: ids[f"h{n}"]
    c = lambda n: ids[f"c{n}"]
    m = lambda n: ids[f"m{n}"]
    a = lambda n: ids[f"a{n}"]
    v2 = version == "v2"
    rd = version == "rd"
    handles = [
        H(h(1), "synthetic-handle-1@example.invalid", "iMessage"),
        H(h(2), "+15550000001", "SMS", unc="+1555SYNTH0001"),
        H(h(3), "+15550000002", "iMessage", pci="SYNTH-PERSON-2" if v2 else "SYNTH-PERSON-1"),
        H(h(4), "synthetic-handle-4@example.invalid", "iMessage", country=None),
    ]
    chats = [
        C(c(1), "iMessage;-;synthetic-handle-1@example.invalid",
          ident="synthetic-handle-1@example.invalid", name="", group="SYNTH-GROUP-1"),
        C(c(2), "iMessage;+;chatSYNTH0002", style=43, ident="chatSYNTH0002", room="chatSYNTH0002",
          name="SYNTH-CHAT-NAME", group="SYNTH-GROUP-2", is_archived=1),
        C(c(3), "SMS;-;+15550000001", ident="+15550000001", service="SMS", group="SYNTH-GROUP-3"),
    ]
    ab3 = ms.ab_text([("￼", [(ts.P, 0), (ts.GUID, att_guid(1))]),
                      ("￼", [(ts.P, 1), (ts.GUID, att_guid(8))])])
    ab10 = ms.ab_text([("￼", [(ts.P, 0), (ts.GUID, att_guid(2))])])
    msgs = {
        1: M(1, m(1), handle_id=h(1), attributedBody=ms.ab_plain("SYNTH-BODY-1")),
        2: M(2, m(2), is_from_me=1, handle_id=0, cache_has_attachments=1, is_sent=1),
        3: M(3, m(3), handle_id=h(1), text=None, attributedBody=ab3, cache_has_attachments=1,
             part_count=2),
        4: M(4, m(4), handle_id=h(1), associated_message_type=2000,
             associated_message_guid=f"p:0/{mguid(1)}", associated_message_range_location=0,
             associated_message_range_length=12, text="Loved “SYNTH-BODY-1”"),
        5: M(5, m(5), is_from_me=1, handle_id=0, thread_originator_guid=mguid(1),
             thread_originator_part="0:0:12"),
        6: M(6, m(6), handle_id=h(3), is_read=0 if rd else 1),
        7: M(7, m(7), item_type=1, group_action_type=0, handle_id=h(1), other_handle=h(4),
             text=None),
        8: M(8, m(8), item_type=3, group_action_type=0, handle_id=h(4), text=None),
        9: M(9, m(9), item_type=2, group_title="SYNTH-GROUP-TITLE", handle_id=h(3), text=None),
        10: M(10, m(10), handle_id=h(4), text=None, attributedBody=ab10, cache_has_attachments=1),
        11: M(11, m(11), handle_id=h(2), service="SMS", subject="SYNTH-SUBJECT-1"),
        12: M(12, m(12), is_from_me=1, handle_id=h(2), service="SMS", cache_has_attachments=1),
        13: M(13, m(13), handle_id=h(1), date=0),
        14: M(14, m(14), handle_id=h(3), date=-5),
        15: M(15, m(15), handle_id=h(1)),
        16: M(16, m(16), handle_id=h(1)),
        18: M(18, m(18), handle_id=h(2), service="SMS", is_read=1 if v2 else 0),
        19: M(19, m(19), handle_id=h(1),
              subject="SYNTH-SUBJECT-NEW" if v2 else "SYNTH-SUBJECT-OLD"),
        20: M(20, m(20), handle_id=h(1), payload_data=b"\x00SYNTH-PAYLOAD",
              balloon_bundle_id="com.example.synthetic"),
        21: M(21, m(21), handle_id=h(1), text=""),
    }
    if not v2:
        msgs[17] = M(17, m(17), handle_id=h(3), cache_has_attachments=1)
    if v2:
        ec = {0: [{"d": T0_S + 15 * 60, "t": b"SYNTH-EDIT-15-ORIGINAL"},
                  {"d": T0_S + 2000, "t": b"SYNTH-EDIT-15-EDITED"}]}
        msgs[15] = M(15, m(15), handle_id=h(1), text="SYNTH-BODY-15-EDITED",
                     date_edited=T0_NS + 2000 * NS, message_summary_info=ms.summary(otr=1, ec=ec))
        msgs[16] = M(16, m(16), handle_id=h(1), text=None, attributedBody=None,
                     date_edited=T0_NS + 2100 * NS, date_retracted=T0_NS + 2100 * NS,
                     message_summary_info=ms.summary(otr=1, rp=[0]))
        msgs[22] = M(22, m(22), handle_id=h(1), cache_has_attachments=1)
        msgs[23] = M(23, m(23), handle_id=h(3), cache_has_attachments=1)
        msgs[24] = M(24, m(24), handle_id=h(2), service="SMS")
    if rd:
        msgs[30] = M(30, m(30), handle_id=h(1), text="SYNTH-BODY-30-DELETED", cache_has_attachments=1)
        msgs[31] = M(31, m(31), handle_id=h(1), text="SYNTH-BODY-31-DELETED", cache_has_attachments=1)
        msgs[32] = M(32, m(32), handle_id=h(1), cache_has_attachments=1)

    chat_of = {1: 1, 2: 1, 3: 1, 4: 1, 5: 1, 13: 1, 15: 1, 16: 1, 20: 1, 21: 1, 22: 1, 32: 1,
               6: 2, 7: 2, 8: 2, 9: 2, 10: 2, 14: 2, 17: 2, 19: 2, 23: 2,
               11: 3, 12: 3, 18: 3, 24: 3}
    in_rd = {6: [2], 11: [3], 30: [1, 2], 31: [1]} if rd else {}
    cmj = []
    for n in sorted(msgs):
        if n in in_rd:
            continue
        cmj.append({"rowid": ids.get(f"cmj{n}", 100 + n), "chat_id": c(chat_of[n]),
                    "message_id": m(n), "message_date": msgs[n]["date"]})
    crmj = []
    for n, chs in sorted(in_rd.items()):
        for k, ch in enumerate(chs):
            crmj.append({"rowid": 300 + 10 * n + k, "chat_id": c(ch), "message_id": m(n),
                         "delete_date": T0_NS + 5000 * NS, "ck_sync_state": 0})
    chj = [(1, 1), (2, 1), (2, 3), (3, 2)] + ([] if v2 else [(2, 4)])
    links = {1: [3], 8: [3], 2: [10], 4: [2], 5: [12]}  # attachment -> messages
    if not v2:
        links[3] = [17]
    if v2:
        links.update({6: [22], 7: [23]})
    if rd:
        links.update({9: [30], 10: [31, 32]})
    maj = []
    for an, ms_ in sorted(links.items()):
        for mn in ms_:
            maj.append({"rowid": ids.get(f"maj{an}-{mn}", 200 + 10 * an + mn % 10),
                        "message_id": m(mn), "attachment_id": a(an)})
    atts = []
    files = []
    for an in sorted(links):
        payload = A4_V2 if (v2 and an == 4) else A_BYTES[an]
        kw = {}
        if an == 8:
            kw = dict(is_sticker=1, emoji_image_short_description="SYNTH-GENMOJI-PROMPT")
        if an == 2 or an == 7:
            kw = dict(uti="public.jpeg", mime_type="image/jpeg")
        if an == 4:
            kw = dict(uti="public.png", mime_type="image/png", is_outgoing=1)
        atts.append(A(a(an), att_guid(an), A_PATHS[an], len(payload), **kw))
        state = "missing" if (v2 and an == 5) else "present"
        files.append(AttFile(media(A_PATHS[an]), payload, state))
    rows = {
        "handle": handles,
        "chat": chats,
        "message": [msgs[n] for n in sorted(msgs)],
        "chat_handle_join": [{"rowid": 50 + i, "chat_id": c(x), "handle_id": h(y)}
                             for i, (x, y) in enumerate(chj)],
        "chat_message_join": cmj,
        "message_attachment_join": maj,
        "attachment": atts,
        "chat_recoverable_message_join": crmj,
        "recoverable_message_part": [{"chat_id": c(1), "message_id": m(30), "part_index": 0,
                                      "delete_date": T0_NS + 5000 * NS,
                                      "part_text": b"SYNTH-DELETED-30"}] if rd else [],
        "deleted_messages": [],
        "_SqliteDatabaseProperties": [{"key": "_ClientVersion", "value": "17001"}],
    }
    return rows, files


def dev1_ids(version):
    ids = {}
    for n in range(1, 5):
        ids[f"h{n}"] = n
    for n in range(1, 4):
        ids[f"c{n}"] = n
    for n in range(1, 33):
        ids[f"m{n}"] = n
    for n in range(1, 11):
        ids[f"a{n}"] = n
    if version == "v2":
        # Every rowid moves (a seeded shuffle into a new range), and the
        # attachment joins of message 3 swap their order.
        rng = random.Random("imports:dev1-v2-rowids")
        for prefix, count, base in (("h", 4, 20), ("c", 3, 40), ("m", 32, 500), ("a", 10, 70)):
            new = list(range(base, base + count))
            rng.shuffle(new)
            for n in range(1, count + 1):
                ids[f"{prefix}{n}"] = new[n - 1]
        ids["maj1-3"] = 299
        ids["maj8-3"] = 201
    return ids


def content_dev1(version):
    rows, files = dev1_rows(version, dev1_ids(version))
    drop = [("chat", "group_id")] if version == "v2" else []
    return Content(f"dev1-{version}", import_profile(drop), rows, files)


# ---- device 2 ---------------------------------------------------------------

def content_dev2():
    P = "SYNTH-MSG-GUID-D2-"
    base = T0_NS + 7 * NS
    handles = [H(1, "synthetic-handle-d2@example.invalid", "iMessage"),
               H(2, "synthetic-handle-1@example.invalid", "iMessage")]  # shared with device 1
    chats = [C(1, "iMessage;-;synthetic-handle-d2@example.invalid",
               ident="synthetic-handle-d2@example.invalid", name=""),
             C(2, "iMessage;+;chatSYNTHD201", style=43, ident="chatSYNTHD201",
               room="chatSYNTHD201", name="SYNTH-CHAT-NAME-D2")]
    msgs = [M(1, 1, P, base, handle_id=1), M(2, 2, P, base, is_from_me=1, handle_id=0),
            M(3, 3, P, base, handle_id=2), M(4, 4, P, base, handle_id=1, cache_has_attachments=1),
            M(5, 5, P, base, handle_id=0, date=0)]
    payload = b"synthetic heic bytes D2-0001\n"
    path = "~/Library/SMS/Attachments/d2/01/SYNTH-D2-0001/SYNTH-FILE-D2-0001.heic"
    rows = {
        "handle": handles, "chat": chats, "message": msgs,
        "chat_handle_join": [{"chat_id": 1, "handle_id": 1}, {"chat_id": 2, "handle_id": 1},
                             {"chat_id": 2, "handle_id": 2}],
        "chat_message_join": [{"chat_id": 1, "message_id": 1}, {"chat_id": 1, "message_id": 2},
                              {"chat_id": 2, "message_id": 3}, {"chat_id": 1, "message_id": 4},
                              {"chat_id": 2, "message_id": 5}],
        "message_attachment_join": [{"message_id": 4, "attachment_id": 1}],
        "attachment": [A(1, "SYNTH-ATT-GUID-D2-0001", path, len(payload))],
        "chat_recoverable_message_join": [], "recoverable_message_part": [],
        "_SqliteDatabaseProperties": [{"key": "_ClientVersion", "value": "17001"}],
    }
    return Content("dev2", import_profile(), rows, [AttFile(media(path), payload)])


# ---- keys and hostile values -------------------------------------------------

def content_keys_hostile():
    P = "SYNTH-MSG-GUID-K-"
    same = dict(handle_id=1, text="SYNTH-BODY-IDENTICAL", guid=None, date=T0_NS + 4000 * NS)
    handles = [
        H(1, "synthetic-handle-k1@example.invalid", "iMessage"),
        H(5, "synthetic-handle-k1@example.invalid", "iMessage", country="ca"),  # duplicated (id, service)
        H(2, "synthetic-handle-k2@example.invalid", "SMS"),
        H(3, "synthetic-handle-k2@example.invalid", None),           # same id, NULL service: unique
        H(4, None, "iMessage"),                                      # NULL id
        H(6, "", "iMessage"),                                        # '' id
        H(7, 42, "iMessage"),                                        # an integer id: unreadable
    ]
    chats = [
        C(1, "SYNTH-CHAT-GUID-K1", ident="synthetic-handle-k1@example.invalid"),
        C(9, "SYNTH-CHAT-GUID-DUP", ident="SYNTH-CHAT-DUP-A"),
        C(3, "SYNTH-CHAT-GUID-DUP", ident="SYNTH-CHAT-DUP-B"),
        C(4, None, ident="SYNTH-CHAT-NULL-GUID"),
        C(5, b"SYNTH-CHAT-GUID-BLOB", ident="SYNTH-CHAT-BLOB-GUID", style=7.5),
    ]
    msgs = [
        M(1, 50, P, handle_id=1),
        M(2, 40, P, handle_id=1, guid="SYNTH-MSG-GUID-DUP-1", text="SYNTH-BODY-DUP-A"),
        M(3, 7, P, handle_id=1, guid="SYNTH-MSG-GUID-DUP-1", text="SYNTH-BODY-DUP-B"),
        ms.msg(41, **same),
        ms.msg(42, **same),
        M(6, 43, P, handle_id=1, guid=""),
        M(7, 44, P, handle_id=1, guid="G" * 300),
        M(8, 45, P, handle_id=1, guid=12345),
        M(9, 46, P, handle_id=1, text=ms.TextBytes(b"SYNTH-BODY-\xff\xfe-invalid")),
        M(10, 47, P, handle_id=1, text=b"SYNTH-BODY-BLOB-CLASS"),
        M(11, 48, P, handle_id=1, is_from_me=2, is_read=5),
        M(12, 49, P, handle_id=1, date="SYNTH-NOT-A-DATE"),
        M(13, 51, P, handle_id=99, text=None, attributedBody=None),
    ]
    rows = {
        "handle": handles, "chat": chats, "message": msgs,
        "chat_handle_join": [{"chat_id": 1, "handle_id": 1}, {"chat_id": 1, "handle_id": 77},
                             {"chat_id": 9, "handle_id": 5}, {"chat_id": 4, "handle_id": 4}],
        "chat_message_join": [{"chat_id": 1, "message_id": n} for n in (50, 40, 7, 41, 42, 43, 44,
                                                                          45, 46, 47, 48, 49, 51)]
        + [{"chat_id": 9, "message_id": 50}, {"chat_id": 99, "message_id": 50},
           {"chat_id": 1, "message_id": 999}],
        "message_attachment_join": [{"message_id": 50, "attachment_id": 1},
                                    {"message_id": 40, "attachment_id": 2},
                                    {"message_id": 7, "attachment_id": 8},
                                    {"message_id": 50, "attachment_id": 88}],
        "attachment": [A(1, "SYNTH-ATT-GUID-K1", None, 0),
                       A(2, "SYNTH-ATT-GUID-DUP", None, 0, transfer_name="SYNTH-FILE-DUP-A"),
                       A(8, "SYNTH-ATT-GUID-DUP", None, 0, transfer_name="SYNTH-FILE-DUP-B",
                         is_sticker=3)],
        "chat_recoverable_message_join": [], "recoverable_message_part": [],
        "_SqliteDatabaseProperties": [{"key": "_ClientVersion", "value": "17001"}],
    }
    # No declared type (no affinity), so integers, reals and blobs stay as stored.
    decl = {("message", "guid"): "", ("message", "text"): "", ("handle", "id"): "",
            ("chat", "guid"): "", ("chat", "style"): ""}
    return Content("keys-hostile", import_profile(), rows, [], decl)


# ---- attachments -------------------------------------------------------------

def content_attach():
    pre = "~/Library/SMS/Attachments/b1"
    p = {
        1: f"{pre}/01/SYNTH-0101/SYNTH-FILE-0101.heic",
        2: f"{pre}/02/SYNTH-0102/SYNTH-FILE-0102.heic",
        3: f"{pre}/03/SYNTH-0103/SYNTH-FILE-0103.heic",
        4: f"{pre}/04/SYNTH-0104/SYNTH-FILE-TRAILING.",
        5: "~/Library/SMS/StickerCache/b5/SYNTH-0105/SYNTH-FILE-0105.heic",
        6: "~/Library/Messages/Attachments/SYNTH-FILE-0106.heic",
        7: f"{pre}/07/SYNTH-0107/SYNTH-FILE-0107.heic",
        8: f"{pre}/08/SYNTH-0108/SYNTH-FILE-0108.heic",
        9: None,
        10: f"{pre}/10/SYNTH-0110/SYNTH-FILE-0110.exe",
    }
    payload = {n: f"synthetic attachment bytes {100 + n:04d}\n".encode() for n in range(1, 11)}
    payload[7] = payload[1]                                   # the same bytes under two GUIDs
    files = [
        AttFile(media(p[1]), payload[1]),
        AttFile(media(p[2]), payload[2], "missing"),          # listed, blob missing
        AttFile(media(p[3]), payload[3], declared=len(payload[3]) + 7),  # Manifest Size differs
        AttFile(media(p[5]), payload[5]),
        AttFile(media(p[7]), payload[7]),
        AttFile(media(p[10]), payload[10]),
    ]
    atts = []
    for n in range(1, 11):
        kw = {}
        if n == 5:
            kw = dict(is_sticker=1, emoji_image_short_description="SYNTH-GENMOJI-PROMPT-B5")
        if n == 10:
            kw = dict(transfer_name="SYNTH-FILE-‮gpj.exe", mime_type=None, uti=None)
        atts.append(A(n, f"SYNTH-ATT-GUID-B{n:03d}", p[n],
                      len(payload[n]) if p[n] else 0, **kw))
    msgs = [M(n, n, "SYNTH-MSG-GUID-B-", handle_id=1, cache_has_attachments=1) for n in range(1, 10)]
    links = [(1, 1), (1, 7), (2, 2), (3, 3), (4, 4), (5, 5), (6, 6), (7, 8), (8, 9), (9, 10)]
    rows = {
        "handle": [H(1, "synthetic-handle-b1@example.invalid", "iMessage")],
        "chat": [C(1, "iMessage;-;synthetic-handle-b1@example.invalid",
                   ident="synthetic-handle-b1@example.invalid")],
        "message": msgs,
        "chat_handle_join": [{"chat_id": 1, "handle_id": 1}],
        "chat_message_join": [{"chat_id": 1, "message_id": n} for n in range(1, 10)],
        "message_attachment_join": [{"rowid": 10 + i, "message_id": mm, "attachment_id": aa}
                                    for i, (mm, aa) in enumerate(links)],
        "attachment": atts,
        "chat_recoverable_message_join": [], "recoverable_message_part": [],
        "_SqliteDatabaseProperties": [{"key": "_ClientVersion", "value": "17001"}],
    }
    return Content("attach", import_profile(), rows, files)


# ---- full-text search ----------------------------------------------------------

FTS_TEXTS = [
    "SYNTH-BODY-FTS-1 Café au lait",                 # NFC
    "SYNTH-BODY-FTS-2 Café noir",                   # NFD
    "SYNTH-BODY-FTS-3 naïve CAFE",
    "SYNTH-BODY-FTS-4 東京タワー",    # CJK, unspaced
    "SYNTH-BODY-FTS-5 ภาษาไทย",  # Thai, unspaced
    "SYNTH-BODY-FTS-6 한국어 문장",   # Korean
    "SYNTH-BODY-FTS-7 \U0001F44D",                        # a single emoji
    "SYNTH-BODY-FTS-8 \U0001F468‍\U0001F469‍\U0001F467 family",  # a ZWJ sequence
    "SYNTH-BODY-FTS-9 Straße",                        # sharp s
]


def content_fts():
    msgs = [M(n, n, "SYNTH-MSG-GUID-F-", handle_id=1, text=t) for n, t in enumerate(FTS_TEXTS, 1)]
    msgs[0]["attributedBody"] = ms.ab_plain(FTS_TEXTS[0])
    rows = {
        "handle": [H(1, "synthetic-handle-f1@example.invalid", "iMessage")],
        "chat": [C(1, "iMessage;-;synthetic-handle-f1@example.invalid",
                   ident="synthetic-handle-f1@example.invalid")],
        "message": msgs,
        "chat_handle_join": [{"chat_id": 1, "handle_id": 1}],
        "chat_message_join": [{"chat_id": 1, "message_id": m["ROWID"]} for m in msgs],
        "message_attachment_join": [], "attachment": [],
        "chat_recoverable_message_join": [], "recoverable_message_part": [],
        "_SqliteDatabaseProperties": [{"key": "_ClientVersion", "value": "17001"}],
    }
    return Content("fts", import_profile(), rows)


# ---- WAL ---------------------------------------------------------------------

def content_wal():
    P = "SYNTH-MSG-GUID-W-"
    rows = {
        "handle": [H(1, "synthetic-handle-w1@example.invalid", "iMessage")],
        "chat": [C(1, "iMessage;-;synthetic-handle-w1@example.invalid",
                   ident="synthetic-handle-w1@example.invalid")],
        "message": [M(n, n, P, handle_id=1) for n in (1, 2, 3)],
        "chat_handle_join": [{"chat_id": 1, "handle_id": 1}],
        "chat_message_join": [{"chat_id": 1, "message_id": n} for n in (1, 2, 3)],
        "message_attachment_join": [], "attachment": [],
        "chat_recoverable_message_join": [], "recoverable_message_part": [],
        "_SqliteDatabaseProperties": [{"key": "_ClientVersion", "value": "17001"}],
    }
    wal_rows = [("message", M(4, 4, P, handle_id=1, text="SYNTH-BODY-WAL-4")),
                ("message", M(5, 5, P, handle_id=1, text="SYNTH-BODY-WAL-5")),
                ("chat_message_join", {"chat_id": 1, "message_id": 4}),
                ("chat_message_join", {"chat_id": 1, "message_id": 5})]
    return Content("wal", import_profile(), rows, [], wal_rows=wal_rows)


# --------------------------------------------------------------------------
# Entries
# --------------------------------------------------------------------------

class Device:
    def __init__(self, name, product_type, product_version):
        self.name = name
        self.product_type = product_type
        self.product_version = product_version


DEV1 = Device(mb.DEVICE_NAME, mb.PRODUCT_TYPE, "18.0")
DEV2 = Device("Synthetic Second Phone", "iPhone0,1", "18.1")


class Entry:
    def __init__(self, name, content, device, status_date, info_date, encrypted=False,
                 wal_missing=False, purpose=""):
        self.name = name
        self.content = content
        self.device = device
        self.status_date = status_date  # None: no Date in Status.plist
        self.info_date = info_date      # None: no Last Backup Date in Info.plist
        self.encrypted = encrypted
        self.wal_missing = wal_missing
        self.purpose = purpose


def catalogue():
    v1, v2, rd = content_dev1("v1"), content_dev1("v2"), content_dev1("rd")
    wal = content_wal()
    d = dt_utc
    return [
        Entry("imp-dev1-v1-plain", v1, DEV1, d(2026, 2, 1, 10), d(2026, 2, 1, 10),
              purpose="Baseline device: three chats, a group with a join and a leave, a reply, a tapback, "
                      "attachments (two on one message), undated messages (date 0 and negative), "
                      "an empty `text`, `payload_data`"),
        Entry("imp-dev1-v2-plain", v2, DEV1, d(2026, 3, 1, 10), d(2026, 3, 1, 10),
              purpose="v2 of the same device: every rowid shuffled; one message edited; one unsent; one "
                      "message and its attachment gone; new messages; one attachment GUID with new bytes "
                      "of another length; one blob missing; the same bytes under two GUIDs; `is_read` "
                      "changed; one subject changed; `chat.group_id` absent; a later Status.plist Date"),
        Entry("imp-dev1-v2-enc", v2, DEV1, d(2026, 3, 1, 10), d(2026, 3, 1, 10), encrypted=True,
              purpose="`imp-dev1-v2-plain`, encrypted: the same sms.db bytes and dates (equal rank)"),
        Entry("imp-dev1-undated-plain", v1, DEV1, None, None,
              purpose="The v1 sms.db with no Status.plist Date and no Info.plist Last Backup Date: undated"),
        Entry("imp-dev1-baddate-plain", v1, DEV1, d(9999, 12, 31), d(2026, 1, 15, 10),
              purpose="The v1 sms.db with a Status.plist Date in year 9999 (out of range, treated as "
                      "missing) and an earlier Info.plist Last Backup Date (used instead)"),
        Entry("imp-dev2-plain", content_dev2(), DEV2, d(2026, 2, 20, 10), d(2026, 2, 20, 10),
              purpose="Another device name and model; no chat, message or attachment GUID in common "
                      "with device 1; one handle in common"),
        Entry("imp-keys-hostile-plain", content_keys_hostile(), DEV1, d(2026, 5, 1, 10),
              d(2026, 5, 1, 10),
              purpose="NULL, `''`, oversized and unreadable GUIDs; a duplicated GUID in each keyed table "
                      "at shuffled rowids; a duplicated (id, service) handle; two identical unkeyed rows; "
                      "invalid UTF-8; a BLOB-class text cell; out-of-range booleans; an unreadable date; "
                      "orphan joins"),
        Entry("imp-attach-plain", content_attach(), DEV1, d(2026, 5, 2, 10), d(2026, 5, 2, 10),
              purpose="A missing blob, a Manifest Size mismatch, an unusual name, `StickerCache`, a "
                      "path class not looked up, a file not in the backup, a NULL filename, the same "
                      "bytes under two GUIDs, a bidi control in `transfer_name`"),
        Entry("imp-rd-plain", rd, DEV1, d(2026, 2, 15, 10), d(2026, 2, 15, 10),
              purpose="Recently Deleted: two v1 messages now deleted (one with a changed `is_read`), a "
                      "new deleted message in two chats, an attachment linked only to a deleted message, "
                      "one shared with a live message"),
        Entry("imp-fts-plain", content_fts(), DEV1, d(2026, 5, 3, 10), d(2026, 5, 3, 10),
              purpose="NFC and NFD diacritics, CJK, Thai, Korean, a single emoji, a ZWJ sequence, `ß`"),
        Entry("imp-wal-plain", wal, DEV1, d(2026, 4, 1, 10), d(2026, 4, 1, 10),
              purpose="Two messages and their links only in `sms.db-wal` (staged)"),
        Entry("imp-wal-missing-plain", wal, DEV1, d(2026, 4, 2, 10), d(2026, 4, 2, 10),
              wal_missing=True,
              purpose="The same backup with the `-wal` listed but its blob missing: the main file's rows "
                      "only, and not a full observation"),
    ]


# Sequences of imports. Each step: (entry, lineage label). A tuple of
# several sequences is a set of orders the model must agree on.
SCENARIOS = [
    ("dev1-v1-v2", [[("imp-dev1-v1-plain", "L1"), ("imp-dev1-v2-plain", "L1")],
                    [("imp-dev1-v2-plain", "L1"), ("imp-dev1-v1-plain", "L1")]],
     "v1 and v2 of one phone, in both orders"),
    ("dev1-v2-plain-enc", [[("imp-dev1-v2-plain", "L1"), ("imp-dev1-v2-enc", "L1")],
                           [("imp-dev1-v2-enc", "L1"), ("imp-dev1-v2-plain", "L1")]],
     "Equal rank: the plain and encrypted copies of one backup"),
    ("dev1-v1-undated", [[("imp-dev1-v1-plain", "L1"), ("imp-dev1-undated-plain", "L1")],
                         [("imp-dev1-undated-plain", "L1"), ("imp-dev1-v1-plain", "L1")]],
     "Dated and undated copies of one sms.db"),
    ("dev1-v1-baddate", [[("imp-dev1-v1-plain", "L1"), ("imp-dev1-baddate-plain", "L1")],
                         [("imp-dev1-baddate-plain", "L1"), ("imp-dev1-v1-plain", "L1")]],
     "An out-of-range Status.plist Date"),
    ("lineages", [[("imp-dev1-v1-plain", "L1"), ("imp-dev2-plain", "L2"), ("imp-dev1-v2-plain", "L1")]],
     "Device 1 v1, device 2 (a new lineage), device 1 v2 (the first lineage)"),
    ("dev1-v1-rd", [[("imp-dev1-v1-plain", "L1"), ("imp-rd-plain", "L1")]],
     "Recently Deleted after v1 (default exclusion)"),
    ("dev1-rd-v1", [[("imp-rd-plain", "L1"), ("imp-dev1-v1-plain", "L1")]],
     "Recently Deleted before v1 (default exclusion)"),
    ("dev1-v1-rd-optin", [[("imp-dev1-v1-plain", "L1"), ("imp-rd-plain+rd", "L1")]],
     "Recently Deleted after v1, with the opt-in"),
    ("wal-missing", [[("imp-wal-plain", "L1"), ("imp-wal-missing-plain", "L1")],
                     [("imp-wal-missing-plain", "L1"), ("imp-wal-plain", "L1")]],
     "A WAL-skipped import is not a full observation"),
    ("keys-hostile-twice", [[("imp-keys-hostile-plain", "L1"), ("imp-keys-hostile-plain", "L1")]],
     "The same backup twice: unkeyed rows are idempotent"),
]

# Scenarios whose two orders are allowed to differ, and where (spec 8.7's
# stated exception for Recently Deleted under the default exclusion).
ASYMMETRIC = {("dev1-v1-rd", "dev1-rd-v1"): {"recentlyDeleted", "notInLatest"}}


# --------------------------------------------------------------------------
# The backup
# --------------------------------------------------------------------------

def write_backup(e, out_parent, rng, main, wal, expected_dir):
    """Write one backup (and, for an encrypted one, the expected plaintexts)."""
    rb = rng.randbytes
    o = argparse.Namespace(**mb.defaults())
    out = os.path.join(out_parent, UDID)
    if os.path.exists(out_parent):
        shutil.rmtree(out_parent)
    os.makedirs(out)
    enc = e.encrypted
    dev = e.device
    info = {"Build Version": "00A000", "Device Name": dev.name, "Display Name": dev.name,
            "Product Name": "Synthetic iPhone", "Product Type": dev.product_type,
            "Product Version": dev.product_version, "Unique Identifier": UDID}
    if e.info_date is not None:
        info["Last Backup Date"] = e.info_date
    mb.write_plist(os.path.join(out, "Info.plist"), info, plistlib.FMT_XML)
    status = {"BackupState": "new", "IsFullBackup": True, "SnapshotState": "finished",
              "UUID": "00000000-0000-0000-0000-000000000000", "Version": "3.3"}
    if e.status_date is not None:
        status["Date"] = e.status_date
    mb.write_plist(os.path.join(out, "Status.plist"), status, plistlib.FMT_BINARY)
    keybag, class_keys = mb.build_keybag(o, rb, PASSWORD)

    entries = [
        ("HomeDomain", "", None, 0o40755, 0, "dir", None),
        ("HomeDomain", "Library", None, 0o40755, 0, "dir", None),
        ("HomeDomain", "Library/SMS", None, 0o40755, 0, "dir", None),
        ("HomeDomain", "Library/SMS/sms.db", main, 0o100644, 3, "present", None),
        ("MediaDomain", "", None, 0o40755, 0, "dir", None),
    ]
    if wal is not None:
        entries.append(("HomeDomain", "Library/SMS/sms.db-wal", wal, 0o100644, 3,
                        "missing" if e.wal_missing else "present", None))
    for f in e.content.files:
        entries.append(("MediaDomain", f.rel, f.payload, 0o100644, 3, f.state, f.declared))
    rows = []
    for inode, (domain, rel, payload, mode, prot, state, declared) in enumerate(entries, start=3000):
        fid = mb.file_id(domain, rel)
        wrapped = None
        data = payload
        if state != "dir":
            if enc:
                fkey = rb(32)
                wrapped = aes_key_wrap(class_keys[prot], fkey)
                data = mb.cbc_encrypt(fkey, payload)
            if state != "missing":
                d = os.path.join(out, fid[:2])
                os.makedirs(d, exist_ok=True)
                with open(os.path.join(d, fid), "wb") as fh:
                    fh.write(data)
        record = mb.mbfile(o, size=len(payload) if payload else 0, mode=mode, prot=prot,
                           wrapped_key=wrapped, inode=inode, size_override=declared)
        rows.append((fid, domain, rel, 2 if state == "dir" else 1, record))

    with tempfile.TemporaryDirectory() as work:
        mpath = os.path.join(work, "Manifest.db")
        mb.build_manifest_db(o, mpath, rows, [])
        with open(mpath, "rb") as f:
            mdb_plain = f.read()
    mdb = mdb_plain
    manifest_key = None
    if enc:
        mkey = rb(32)
        manifest_key = struct.pack("<I", mb.MANIFEST_CLASS) + aes_key_wrap(class_keys[mb.MANIFEST_CLASS], mkey)
        mdb = mb.cbc_encrypt(mkey, mdb_plain)
    with open(os.path.join(out, "Manifest.db"), "wb") as f:
        f.write(mdb)
    lockdown = {"BuildVersion": "00A000", "DeviceName": dev.name, "ProductType": dev.product_type,
                "ProductVersion": dev.product_version}
    manifest = {"Version": "10.0", "Date": e.status_date or mb.FIXED_DATE, "SystemDomainsVersion": "24.0",
                "WasPasscodeSet": False, "IsEncrypted": enc, "Lockdown": lockdown,
                "Applications": {}, "BackupKeyBag": keybag}
    if enc:
        manifest["ManifestKey"] = manifest_key
    mb.write_plist(os.path.join(out, "Manifest.plist"), manifest, plistlib.FMT_BINARY)

    os.makedirs(expected_dir, exist_ok=True)
    if enc:
        for f in e.content.files:
            if f.state == "present":
                d = os.path.join(expected_dir, "attachments")
                os.makedirs(d, exist_ok=True)
                with open(os.path.join(d, mb.file_id("MediaDomain", f.rel)), "wb") as fh:
                    fh.write(f.payload)


# --------------------------------------------------------------------------
# The model: what an import must leave in the archive
# --------------------------------------------------------------------------

VALUE, NULL, ABSENT, OVERSIZED, UNREADABLE = "value", "null", "absent", "oversized", "unreadable"
WEAK = (ABSENT, OVERSIZED, UNREADABLE)


def kind_of(table, col):
    """(kind, cap) of a known column as the M4 import reads it."""
    if (table, col) == ("message", "payload_data"):
        return "blob", CAP_PAYLOAD_DATA
    k = sp.known(table, col)
    return k[1], k[2]


def read_obs(table, col, row, prof):
    """One observed source value: (state, payload). Value payloads are
    ("bytes", b, storage class), ("int", n) or ("time", raw, unit, unix ns)."""
    cols = prof.get(table)
    if cols is None or col not in cols:
        return (ABSENT, None)
    v = row.get(col)
    kind, cap = kind_of(table, col)
    iss = ms.Issues()
    if kind == "time":
        r = ms.read_integer(v, iss)
        if r[0] != VALUE:
            return r
        unit, ns = ms.time_interp(r[1])
        return (VALUE, ("time", r[1], unit, ns))
    if kind in ("int", "rowid"):
        r = ms.read_integer(v, iss)
        return (VALUE, ("int", r[1])) if r[0] == VALUE else r
    if kind == "bool":
        r = ms.read_col(table, col, v, iss)
        return (VALUE, ("int", int(r[1]))) if r[0] == VALUE else r
    if kind == "blobsize":
        r = ms.read_col(table, col, v, iss)
        return (VALUE, ("int", r[1])) if r[0] == VALUE else r
    # text and blob
    raw = ms.raw_bytes(v)
    if raw is None:
        return (NULL, None)
    if isinstance(raw, (int, float)):
        return (UNREADABLE, "TypeMismatch")
    if isinstance(raw, str):
        b, cls = raw.encode("utf-8"), "text"
    elif isinstance(raw, ms.TextBytes):
        b, cls = raw.raw, "text"
    else:
        b, cls = bytes(raw), "blob"
    if len(b) > cap:
        return (OVERSIZED, len(b))
    return (VALUE, ("bytes", b, cls))


def obs_key(o):
    """A hashable, order-free form of an observation."""
    return json.dumps(o, sort_keys=True, default=lambda x: x.hex() if isinstance(x, bytes) else str(x))


def strong(o):
    return o[0] in (VALUE, NULL)


def replaces(stored, observed, rank, last_rank):
    """The column rule of spec 8.7 for a raw value and its state."""
    if strong(observed):
        return rank >= last_rank or stored[0] in WEAK
    return stored[0] in WEAK and rank >= last_rank


def usable_key_bytes(o):
    """A key field: a readable, non-empty value (the model reads '' as no
    usable identifier; see README)."""
    if o[0] == VALUE and o[1][0] == "bytes" and o[1][1] != b"":
        return o[1][1]
    return None


def fingerprint(table, row, prof, body=None):
    """The model's stand-in for unkeyed_fp: every raw field but ROWID."""
    parts = []
    for c in [k[0] for k in sp.KNOWN[table] if k[1] != "rowid"]:
        parts.append([c, obs_key(read_obs(table, c, row, prof))])
    return hashlib.sha256(json.dumps(parts).encode()).digest()


def text_repr(b):
    try:
        return b.decode("utf-8")
    except UnicodeDecodeError:
        return None


def ref_bytes(b):
    t = text_repr(b)
    return f"guid:{t}" if t is not None else f"guid-hex:{b.hex()}"


def handle_ref(addr, svc):
    a = text_repr(addr)
    a = a if a is not None else "hex:" + addr.hex()
    if svc[0] == VALUE:
        s = text_repr(svc[1][1])
        s = s if s is not None else "hex:" + svc[1][1].hex()
    else:
        s = f"({svc[0]})"
    return f"handle:{a}|{s}"


class Row:
    def __init__(self, entity, ref, cols, rank, src_rowid, unkeyed=None):
        self.entity = entity
        self.ref = ref
        self.unkeyed = unkeyed  # (fp, occ) or None
        self.cols = dict(cols)
        self.first_rank = rank
        self.last_rank = rank
        self.first_src_rowid = src_rowid
        self.seen = {c: {obs_key(o): o} for c, o in cols.items()
                     if c in HISTORY.get(entity, ()) and strong(o)}
        # message
        self.bodies = {}
        self.current_body = None
        self.handle_ref = None
        # attachment
        self.blobs = set()
        self.current_blob = None
        self.current_blob_rank = None

    def merge(self, cols, rank, src_rowid):
        """Apply one observation; returns the columns whose pair changed hands."""
        replaced = set()
        for c, o in cols.items():
            cur = self.cols.get(c, (ABSENT, None))
            if replaces(cur, o, rank, self.last_rank):
                self.cols[c] = o
                replaced.add(c)
            if c in HISTORY.get(self.entity, ()) and strong(o):
                self.seen.setdefault(c, {})[obs_key(o)] = o
        if rank > self.last_rank:
            self.last_rank = rank
        if rank < self.first_rank:
            self.first_rank = rank
            self.first_src_rowid = src_rowid
        return replaced

    def history(self, c):
        cur = obs_key(self.cols.get(c, (ABSENT, None)))
        return [o for k, o in sorted(self.seen.get(c, {}).items()) if k != cur]


def display_of(text, ab, ann):
    """D58: (display text, source) or (None, none reason)."""
    if ab[0] == VALUE:
        public = ann[ab[1][1]].public
        if "ok" in public and public["ok"]["text"] != "":
            return public["ok"]["text"], "attributedBody"
    if text[0] == VALUE and text[1][1] != b"":
        t = text_repr(text[1][1])
        if t is not None:
            return t, "text"
        return text[1][1].decode("utf-8", "replace"), "textLossy"
    if ab[0] in (VALUE, OVERSIZED, UNREADABLE):
        return None, "attributedBodyUnusable"
    if text[0] == VALUE:
        return None, "textEmpty"
    return None, "noSource"


class Session:
    def __init__(self, entry, lineage, include_rd, copy, model):
        self.entry = entry
        self.lineage = lineage
        self.include_rd = include_rd
        self.copy = copy
        self.rank = model.rank
        self.info = model.info
        self.full = model.full


class EntryModel:
    """What one entry's backup offers an import: rows as SQLite stored them,
    the rank, and the attachment plaintexts."""

    def __init__(self, e, seed):
        main, wal = e.content.built(seed)
        staged_wal = wal if (wal is not None and not e.wal_missing) else None
        self.rows, _counts = ms.read_back(e.content.spec, main, staged_wal)
        self.prof = e.content.spec.profile
        self.ann = ms.annotations(e.content.spec)
        self.files = {f.rel: f for f in e.content.files}
        digest = hashlib.sha256(main + (staged_wal or b"")).digest()
        qualifiers = []
        key, source = None, None
        for d, src in ((e.status_date, "statusPlistDate"), (e.info_date, "infoPlistLastBackupDate")):
            if d is None:
                continue
            ns = unix_ns(d)
            if DATE_FLOOR_NS <= ns <= DATE_CEILING_NS:
                key, source = ns, src
                break
            if "backupDateOutOfRange" not in qualifiers:
                qualifiers.append("backupDateOutOfRange")
        if wal is not None and e.wal_missing:
            qualifiers.append("walSkipped")
        cls = 1 if key is not None else 0
        okey = (key ^ (1 << 63)) & ((1 << 64) - 1) if key is not None else 0
        self.rank = bytes([cls]) + okey.to_bytes(8, "big") + digest
        self.full = "walSkipped" not in qualifiers
        self.info = {"orderClass": "dated" if cls else "undated", "orderKeySource": source,
                     "orderKeyNs": key, "contentDigest": digest.hex(), "rank": self.rank.hex(),
                     "qualifiers": qualifiers, "fullObservation": self.full}

    def table(self, t):
        return self.rows.get(t, [])

    def lookup(self, filename):
        """("present", payload), or another lookup outcome (M3 rules)."""
        if filename[0] != VALUE:
            return ("not_looked_up", None)
        b = filename[1][1]
        cls = ms.path_class(b)
        if cls not in ("sms_attachments", "sms_sticker_cache"):
            return ("not_looked_up", None)
        rel = ms.candidate(b)
        if rel is None:
            return ("not_looked_up", None)
        f = self.files.get(rel)
        if f is None:
            return ("not_in_backup", None)
        if f.state != "present":
            return ("blob_" + f.state, None)
        return ("present", f.payload)


class Archive:
    def __init__(self):
        self.rows = {"handle": {}, "chat": {}, "message": {}, "attachment": {}}
        self.blobs = {}
        self.chat_handle = set()
        self.chat_message = set()
        self.message_attachment = {}
        self.markers = set()
        self.seen = {}  # (entity, ref, lineage) -> max rank
        self.sessions = []
        self.excluded = {"message": set(), "attachment": set()}
        self.excluded_unkeyed = {"message": 0, "attachment": 0}

    def see(self, entity, ref, s):
        k = (entity, ref, s.lineage)
        if k not in self.seen or s.rank > self.seen[k]:
            self.seen[k] = s.rank

    def keyed_refs(self, entity):
        return {r for r, row in self.rows[entity].items() if row.unkeyed is None}


def scan_keys(em, table, keyf):
    """Each row's usable key, with duplicated keys set to None (D83)."""
    keys = {}
    count = {}
    for rowid, row in em.table(table):
        k = keyf(row)
        keys[rowid] = k
        if k is not None:
            count[k] = count.get(k, 0) + 1
    return {rowid: (k if k is not None and count[k] == 1 else None) for rowid, k in keys.items()}


def run_import(arch, em, entry_name, lineage, include_rd=False, copy=True):
    s = Session(entry_name, lineage, include_rd, copy, em)
    arch.sessions.append(s)
    prof = em.prof
    obs = lambda t, c, row: read_obs(t, c, row, prof)

    def cols_of(table, row, skip=()):
        return {c: obs(table, c, row) for c in [k[0] for k in sp.KNOWN[table]]
                if c != "ROWID" and c not in skip}

    def ident(entity, keyb, row, occ_counter):
        if keyb is not None:
            return (keyb, None)
        fp = fingerprint(entity, row, prof)
        n = occ_counter.get(fp, 0)
        occ_counter[fp] = n + 1
        return (None, (fp, n))

    def unkeyed_ref(entity, u):
        return f"unkeyed:{entity}:{u[0].hex()}:{u[1]}"

    def upsert(entity, ref, unk, cols, src_rowid):
        store = arch.rows[entity]
        row = store.get(ref)
        if row is None:
            row = Row(entity, ref, cols, s.rank, src_rowid, unk)
            store[ref] = row
            return row, set(cols), True, None
        prev_last = row.last_rank
        return row, row.merge(cols, s.rank, src_rowid), False, prev_last

    # Key scans (preflight).
    hkeys = scan_keys(em, "handle", lambda r: (lambda a: None if a is None else
                                               (a, obs_key(obs("handle", "service", r))))(
        usable_key_bytes(obs("handle", "id", r))))
    ckeys = scan_keys(em, "chat", lambda r: usable_key_bytes(obs("chat", "guid", r)))
    mkeys = scan_keys(em, "message", lambda r: usable_key_bytes(obs("message", "guid", r)))
    akeys = scan_keys(em, "attachment", lambda r: usable_key_bytes(obs("attachment", "guid", r)))

    # P0 and the Recently Deleted exclusion (8.2, 8.8).
    rd = set()
    for _, r in em.table("chat_recoverable_message_join"):
        o = obs("chat_recoverable_message_join", "message_id", r)
        if o[0] == VALUE:
            rd.add(o[1][1])
    archived = {row.ref for row in arch.rows["message"].values() if row.unkeyed is None}
    excluded, rd_archived = set(), set()
    for rowid, _ in em.table("message"):
        if rowid not in rd:
            continue
        k = mkeys[rowid]
        if k is not None and ref_bytes(k) in archived:
            if not include_rd:
                rd_archived.add(rowid)
        elif not include_rd:
            excluded.add(rowid)

    # P1 handles.
    hmap, occ = {}, {}
    for rowid, r in em.table("handle"):
        keyb, unk = ident("handle", hkeys[rowid], r, occ)
        cols = cols_of("handle", r)
        ref = handle_ref(keyb[0], cols["service"]) if keyb else unkeyed_ref("handle", unk)
        upsert("handle", ref, unk, cols, rowid)
        hmap[rowid] = ref

    # P2 chats.
    cmap, occ = {}, {}
    for rowid, r in em.table("chat"):
        keyb, unk = ident("chat", ckeys[rowid], r, occ)
        ref = ref_bytes(keyb) if keyb else unkeyed_ref("chat", unk)
        upsert("chat", ref, unk, cols_of("chat", r), rowid)
        cmap[rowid] = ref
        arch.see("chat", ref, s)

    # P3 messages.
    mmap, occ = {}, {}
    for rowid, r in em.table("message"):
        if rowid in excluded:
            if mkeys[rowid] is not None:
                arch.excluded["message"].add(ref_bytes(mkeys[rowid]))
            else:
                arch.excluded_unkeyed["message"] += 1
            continue
        if rowid in rd_archived:
            ref = ref_bytes(mkeys[rowid])
            mmap[rowid] = ref
            arch.see("message", ref, s)
            continue
        keyb, unk = ident("message", mkeys[rowid], r, occ)
        ref = ref_bytes(keyb) if keyb else unkeyed_ref("message", unk)
        cols = cols_of("message", r)
        row, replaced, new, prev_last = upsert("message", ref, unk, cols, rowid)
        body = (obs_key(cols["text"]), obs_key(cols["attributedBody"]),
                obs_key(cols["message_summary_info"]))
        if body not in row.bodies:
            row.bodies[body] = display_of(cols["text"], cols["attributedBody"], em.ann)
        if new or s.rank >= prev_last:
            row.current_body = body
        if "handle_id" in replaced:
            h = cols["handle_id"]
            row.handle_ref = hmap.get(h[1][1]) if h[0] == VALUE and h[1][1] != 0 else None
        mmap[rowid] = ref
        arch.see("message", ref, s)

    # P4 chat <-> handle; P5 chat <-> message (unresolved sides are skipped).
    def resolved(table, r, col, m):
        o = obs(table, col, r)
        return m.get(o[1][1]) if o[0] == VALUE else None

    for _, r in em.table("chat_handle_join"):
        c, h = resolved("chat_handle_join", r, "chat_id", cmap), resolved("chat_handle_join", r, "handle_id", hmap)
        if c and h:
            arch.chat_handle.add((c, h))
    for _, r in em.table("chat_message_join"):
        c, m = resolved("chat_message_join", r, "chat_id", cmap), resolved("chat_message_join", r, "message_id", mmap)
        if c and m:
            arch.chat_message.add((c, m))

    # P5b: attachments linked only to excluded messages.
    linked = {}
    for _, r in em.table("message_attachment_join"):
        mo, ao = obs("message_attachment_join", "message_id", r), obs("message_attachment_join", "attachment_id", r)
        if mo[0] == VALUE and ao[0] == VALUE:
            linked.setdefault(ao[1][1], set()).add(mo[1][1])
    rd_only = {a for a, msgs in linked.items() if msgs and msgs <= excluded}

    # P6 attachments.
    amap, occ = {}, {}
    for rowid, r in em.table("attachment"):
        if rowid in rd_only:
            if akeys[rowid] is not None:
                arch.excluded["attachment"].add(ref_bytes(akeys[rowid]))
            else:
                arch.excluded_unkeyed["attachment"] += 1
            continue
        keyb, unk = ident("attachment", akeys[rowid], r, occ)
        ref = ref_bytes(keyb) if keyb else unkeyed_ref("attachment", unk)
        cols = cols_of("attachment", r)
        row, _, _, _ = upsert("attachment", ref, unk, cols, rowid)
        found, payload = em.lookup(cols["filename"])
        if copy and found == "present":
            cur = row.current_blob
            if cur is None or arch.blobs[cur] != len(payload):
                sha = hashlib.sha256(payload).hexdigest()
                arch.blobs[sha] = len(payload)
                row.blobs.add(sha)
                if row.current_blob_rank is None or s.rank >= row.current_blob_rank:
                    row.current_blob = sha
                    row.current_blob_rank = s.rank
        amap[rowid] = ref
        arch.see("attachment", ref, s)

    # P7 message <-> attachment, with the order rule (6.5).
    for jrowid, r in em.table("message_attachment_join"):
        m, a = resolved("message_attachment_join", r, "message_id", mmap), resolved("message_attachment_join", r, "attachment_id", amap)
        if not (m and a):
            continue
        cur = arch.message_attachment.get((m, a))
        if cur is None or s.rank >= cur[0]:
            arch.message_attachment[(m, a)] = (s.rank, jrowid)

    # P8 Recently Deleted markers, for messages mapped in this session.
    for jrowid, r in em.table("chat_recoverable_message_join"):
        m = resolved("chat_recoverable_message_join", r, "message_id", mmap)
        if m:
            c = resolved("chat_recoverable_message_join", r, "chat_id", cmap)
            arch.markers.add((s.info["contentDigest"], jrowid, m, c))
    return s


# ---- the manifest --------------------------------------------------------------

def enc_obs(table, col, o):
    st, v = o
    if st == VALUE:
        if v[0] == "bytes":
            kind, _ = kind_of(table, col)
            t = text_repr(v[1]) if kind == "text" else None
            out = {"text": t} if t is not None else {"hex": v[1].hex()}
            if kind == "text" and v[2] == "blob":
                out["storedAsBlob"] = True
            return out
        if v[0] == "int":
            return {"int": v[1]}
        return {"raw": v[1], "unit": v[2], "utcNs": v[3]}
    if st == OVERSIZED:
        return {"state": st, "bytes": v}
    if st == UNREADABLE:
        return {"state": st, "kind": v}
    return {"state": st}


def label_unkeyed(arch):
    """Unkeyed refs carry the model's fingerprint; the manifest numbers them
    instead (labels local to one manifest)."""
    names = {}
    for entity, store in arch.rows.items():
        fps = sorted({row.unkeyed[0] for row in store.values() if row.unkeyed})
        for row in store.values():
            if row.unkeyed:
                names[row.ref] = f"unkeyed:{entity}:{fps.index(row.unkeyed[0]) + 1}:{row.unkeyed[1]}"
    return lambda ref: names.get(ref, ref) if ref is not None else None


def not_in_latest(arch, entity, ref):
    out = []
    for lin in sorted({s.lineage for s in arch.sessions}):
        full = [s.rank for s in arch.sessions if s.lineage == lin and s.full]
        seen = arch.seen.get((entity, ref, lin))
        if full and seen is not None and seen < max(full):
            out.append(lin)
    return out


def columns(table, row):
    return {c: enc_obs(table, c, row.cols.get(c, (ABSENT, None))) for c in SAMPLED[table]}


def histories(table, row):
    out = {}
    for c in sorted(HISTORY.get(table, ())):
        h = row.history(c)
        if h:
            out[c] = sorted((enc_obs(table, c, o) for o in h), key=lambda x: json.dumps(x, sort_keys=True))
    return out


def display_json(d):
    text, how = d
    return {"text": text, "source": how} if text is not None else {"noneReason": how}


def manifest(arch):
    name = label_unkeyed(arch)
    out = {}
    out["handles"] = sorted(({"ref": name(r.ref), "columns": columns("handle", r),
                              "history": histories("handle", r)}
                             for r in arch.rows["handle"].values()), key=lambda x: x["ref"])
    chats = []
    for r in arch.rows["chat"].values():
        chats.append({"ref": name(r.ref), "columns": columns("chat", r), "history": histories("chat", r),
                      "handles": sorted(name(h) for c, h in arch.chat_handle if c == r.ref),
                      "notInLatest": not_in_latest(arch, "chat", r.ref)})
    out["chats"] = sorted(chats, key=lambda x: x["ref"])
    msgs = []
    for r in arch.rows["message"].values():
        d = r.cols["date"]
        if d[0] == VALUE and d[1][2] in ("seconds", "nanoseconds"):
            sort = {"utcNs": d[1][3]}
            skey = (0, d[1][3], b"", 0)
        else:
            sort = {"undated": {"firstRank": r.first_rank.hex(), "firstSrcRowid": r.first_src_rowid}}
            skey = (1, 0, r.first_rank, r.first_src_rowid)
        atts = [(k[1], v) for k, v in arch.message_attachment.items() if k[0] == r.ref]
        # (ord_rank DESC, ord_rowid ASC, natural key)
        atts.sort(key=lambda x: (bytes(255 - b for b in x[1][0]), x[1][1], x[0]))
        bodies = sorted(({"current": k == r.current_body, **display_json(v)} for k, v in r.bodies.items()),
                        key=lambda x: (not x["current"], json.dumps(x, sort_keys=True)))
        markers = sorted({name(c) or "(unresolved)" for _, _, m, c in arch.markers if m == r.ref})
        msgs.append((skey, {
            "ref": name(r.ref), "sort": sort,
            "chats": sorted(name(c) for c, m in arch.chat_message if m == r.ref),
            "handle": name(r.handle_ref),
            "display": display_json(r.bodies[r.current_body]),
            "bodies": bodies,
            "columns": columns("message", r), "history": histories("message", r),
            "attachments": [name(a) for a, _ in atts],
            "recentlyDeleted": markers,
            "notInLatest": not_in_latest(arch, "message", r.ref)}))
    msgs.sort(key=lambda x: (x[0], x[1]["ref"]))
    out["messages"] = [m for _, m in msgs]
    atts = []
    for r in arch.rows["attachment"].values():
        atts.append({"ref": name(r.ref), "sha256": r.current_blob, "blobs": sorted(r.blobs),
                     "columns": columns("attachment", r), "history": histories("attachment", r),
                     "notInLatest": not_in_latest(arch, "attachment", r.ref)})
    out["attachments"] = sorted(atts, key=lambda x: x["ref"])
    out["blobs"] = [{"sha256": k, "byteLen": v} for k, v in sorted(arch.blobs.items())]
    present = {e: set(arch.rows[e]) for e in ("message", "attachment")}
    out["excluded"] = {
        "messages": sorted(arch.excluded["message"] - present["message"]),
        "attachments": sorted(arch.excluded["attachment"] - present["attachment"]),
        "unkeyedMessageRows": arch.excluded_unkeyed["message"],
        "unkeyedAttachmentRows": arch.excluded_unkeyed["attachment"],
    }
    return out


def run_steps(steps, models):
    arch = Archive()
    for entry, lineage in steps:
        include_rd = entry.endswith("+rd")
        base = entry.split("+")[0]
        run_import(arch, models[base], base, lineage, include_rd=include_rd)
    return manifest(arch)


def diff_paths(a, b, path=""):
    """The JSON paths where a and b differ."""
    if type(a) is not type(b):
        return [path]
    if isinstance(a, dict):
        out = []
        for k in sorted(set(a) | set(b)):
            out += diff_paths(a.get(k), b.get(k), f"{path}/{k}")
        return out
    if isinstance(a, list):
        if len(a) != len(b):
            return [path]
        out = []
        for i, (x, y) in enumerate(zip(a, b)):
            out += diff_paths(x, y, f"{path}/{i}")
        return out
    return [] if a == b else [path]


def step_json(entry, lineage):
    include_rd = entry.endswith("+rd")
    base = entry.split("+")[0]
    return {"entry": base, "lineage": lineage, "includeRecentlyDeleted": include_rd,
            "copyAttachments": True}


def write_json(path, value):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        json.dump(value, f, indent=1, sort_keys=True, ensure_ascii=True)
        f.write("\n")


def document(orders, models, content):
    entries = sorted({step_json(e, l)["entry"] for steps in orders for e, l in steps})
    return {"format": "oma-import-content", "formatVersion": 1,
            "orders": [[step_json(e, l) for e, l in steps] for steps in orders],
            "sessions": {n: models[n].info for n in entries},
            "content": content}


# --------------------------------------------------------------------------
# Output
# --------------------------------------------------------------------------

def write_sums(out_dir, name, include):
    lines = []
    for dirpath, dirnames, filenames in os.walk(out_dir):
        dirnames.sort()
        for fn in sorted(filenames):
            full = os.path.join(dirpath, fn)
            rel = os.path.relpath(full, out_dir).replace(os.sep, "/")
            if rel in ("SHA256SUMS", "EXPECTED.SHA256SUMS", "README.md") or not include(rel):
                continue
            with open(full, "rb") as f:
                lines.append(f"{hashlib.sha256(f.read()).hexdigest()}  {rel}")
    lines.sort(key=lambda l: l.split("  ", 1)[1])
    with open(os.path.join(out_dir, name), "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(lines) + "\n")


def is_expected(rel):
    first = rel.split("/", 1)[0]
    return first.endswith(".expected") or first == "scenarios"


README = """\
# Synthetic import catalogue (M4)

Test input for the archive import in `crates/oma-archive`. **Every value in
these files is invented**: addresses under `example.invalid`, `+1555…`
numbers, and sentinel text (`SYNTH-BODY-…`, `SYNTH-EDIT-…`,
`SYNTH-DELETED-…`, `SYNTH-MSG-GUID-…`, `SYNTH-ATT-GUID-…`, `SYNTH-CHAT-…`,
`SYNTH-GROUP-…`, `SYNTH-SUBJECT-…`, `SYNTH-FILE-…`, `SYNTH-GENMOJI-…`,
`SYNTH-PAYLOAD`, `SYNTH-PERSON-…`). No real device, person, message or backup
was used. Generated by `scripts/fixtures/make_synthetic_imports.py`; do not
edit by hand.

## Layout

```text
<entry>/SYNTHETIC0-0000000000000000/  one backup (the import input)
<entry>.expected/content.json         the content manifest after importing <entry> alone
<entry>.expected/attachments/<fileID> attachment plaintexts (encrypted entries)
scenarios/<name>.content.json         the content manifest after a sequence of imports
SHA256SUMS                            the inputs: every file of every backup
EXPECTED.SHA256SUMS                   every file under *.expected/ and scenarios/
```

There are two pins (M4 spec §19.1). `SHA256SUMS` covers the inputs, and the
SHA-256 of `SHA256SUMS` itself is pinned in
`crates/oma-archive/tests/import_fixtures.rs`; the inputs never change after
that. `EXPECTED.SHA256SUMS` covers the model's expectations, which may be
regenerated (and re-pinned) while the import is built in slices 4f to 4j.
The per-entry counts file `import.json` is added with `ImportCounts` in
slice 4f.

## The content manifest

Each `content.json` is the generator's own model of what the M4 import must
leave in the archive. The model is a second statement of the spec's rules
(§6.2 keys, §6.5 order, §8.7 merges, §8.8 Recently Deleted, §9 the store and
D58 display text), applied to the rows the generator wrote as SQLite stored
them. **Agreement with the Rust import proves self-consistency only.**

- `orders`: the import sequences the content holds for. Each step names an
  entry, a lineage label (`L1`, `L2`: the user's choice at the gate) and the
  options. When a scenario lists two orders, the model produced the same
  content for both.
- `sessions`: per entry, the rank the import must compute (order class, order
  key and its source, the content digest, the 41-byte rank in hex), the
  qualifiers and whether the session is a full observation. The model's
  import start is 2027-01-01, so every 2026 date is in range.
- `content.handles`, `chats`, `messages`, `attachments`: one object per
  archive row, identified by `ref`:
  - `guid:<text>` (or `guid-hex:<hex>`) for a keyed chat, message or
    attachment; `handle:<address>|<service>` for a keyed handle, with
    `(null)` or `(absent)` for a service that has no value;
  - `unkeyed:<entity>:<n>:<occ>` for an unkeyed row: `n` numbers the
    distinct rows of the source within this manifest, and `occ` is the
    occurrence. Match unkeyed rows by their content, not by `n`.
- `columns`: a sample of source columns (by **source** name: `handle.id` is
  stored as `address`, `message.date` as the `date_*` quad,
  `associated_message_type` and `associated_message_guid` as `assoc_type`
  and `assoc_guid_raw`, `emoji_image_short_description` as
  `emoji_description`). A value is `{"text": …}`, `{"hex": …}` (blob columns,
  or text that is not UTF-8), `{"int": …}` or `{"raw", "unit", "utcNs"}`;
  `"storedAsBlob": true` marks a text column cell SQLite returned as a BLOB
  (a `value_issue` of kind 3). Otherwise `{"state": "null" | "absent" |
  "oversized" | "unreadable"}`, with `bytes` or `kind`.
- `history`: the other observed values of the history columns (D82).
- `messages` are listed in `sort_key` order (§6.5); `sort` is `{"utcNs"}` or
  `{"undated": {"firstRank", "firstSrcRowid"}}`. Messages with equal sort keys
  are listed by `ref`; their relative order in the archive is unspecified.
- `display` is the current body's `display_text` and `display_source`
  (`attributedBody`, `text`, `textLossy`), or its `noneReason`
  (`attributedBodyUnusable`, `textEmpty`, `noSource`). `bodies` lists every
  body version, current first.
- `chats`, `handle` and `attachments` (in the §6.5 order) give the joins by
  `ref`; `recentlyDeleted` the chats of the message's Recently Deleted
  markers; `notInLatest` the lineages that label the row "not in the latest
  import".
- `attachments[].sha256` is the `current_blob_id` target and `blobs` every blob
  read for it; `blobs` lists the store.
- `excluded`: keyed rows the Recently Deleted policy left out (they must not
  be in the archive), and the number of unkeyed ones.

### Readings the model makes (to be confirmed by the import slices)

- **An empty GUID or handle address is not a key.** Spec §6.2 keys "a Value";
  the model reads `''` as no usable identifier, so such a row is unkeyed and
  two sources' empty GUIDs never merge.
- **`current_blob_id` is the blob of the highest-ranked session that read
  bytes.** A session that read none (blob missing, already archived) never
  moves it. This keeps the result order-independent when a newer backup has
  lost a file.
- **The display reasons** are named here; their codes are fixed in slice 4g.
- **A Recently Deleted message already in the archive** is re-observed for
  the lineage and its marker only: no column, body, rank or join changes.
- **Recently Deleted under the default exclusion** makes the two orders
  differ in the markers (spec §8.7) **and** in "not in the latest import":
  `dev1-v1-rd` and `dev1-rd-v1` are kept as separate scenarios, and the
  generator asserts that they differ only there.
- **The unkeyed fingerprint** here covers every known column but `ROWID`. The
  catalogue's unkeyed rows are either identical in every column or differ in
  a content column, so any fingerprint over every raw field groups them the
  same way.

## Regenerating (reference environment only)

```sh
python3 -m venv .venv-fixtures
.venv-fixtures/bin/pip install -r scripts/fixtures/requirements.txt
.venv-fixtures/bin/python -I -c "import sqlite3; print(sqlite3.sqlite_version)"   # must print 3.45.1
.venv-fixtures/bin/python -I scripts/fixtures/make_synthetic_imports.py tests/fixtures/imports --seed 1
sha256sum tests/fixtures/imports/SHA256SUMS tests/fixtures/imports/EXPECTED.SHA256SUMS
```

The script refuses to write under any other SQLite unless
`--allow-other-sqlite` is passed. Windows is not a generation environment. CI
runs no Python; `import_fixtures.rs` checks every file against the two sums.
Encrypted entries use the password `{password}`.

| | |
|---|---|
| Python | {python} |
| SQLite (Python `sqlite3`) | {sqlite} |
| cryptography | {cryptography} |
| Seed | {seed} |

## Entries

| Entry | What it covers |
|---|---|
{entries}

## Scenarios

| Scenario | Orders | What it covers |
|---|---|---|
{scenarios}
"""


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("out")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--allow-other-sqlite", action="store_true")
    a = ap.parse_args()
    print(f"python {sys.version.split()[0]}, sqlite {sqlite3.sqlite_version}")
    if sqlite3.sqlite_version != REFERENCE_SQLITE and not a.allow_other_sqlite:
        sys.exit(f"refusing to write: sqlite {sqlite3.sqlite_version} is not the reference "
                 f"{REFERENCE_SQLITE} (pass --allow-other-sqlite for an experiment)")
    out = os.path.abspath(a.out)
    os.makedirs(out, exist_ok=True)
    entries = catalogue()
    names = {e.name for e in entries}
    for item in os.listdir(out):
        base = item[:-len(".expected")] if item.endswith(".expected") else item
        if (base.startswith("imp-") and base not in names) or item == "scenarios":
            shutil.rmtree(os.path.join(out, item))
    models = {}
    for e in entries:
        rng = random.Random(f"{a.seed}:{e.name}")
        main_db, wal = e.content.built(a.seed)
        exp = os.path.join(out, e.name + ".expected")
        if os.path.exists(exp):
            shutil.rmtree(exp)
        write_backup(e, os.path.join(out, e.name), rng, main_db, wal, exp)
        models[e.name] = EntryModel(e, a.seed)
    for e in entries:
        content = run_steps([(e.name, "L1")], models)
        write_json(os.path.join(out, e.name + ".expected", "content.json"),
                   document([[(e.name, "L1")]], models, content))
        size = sum(os.path.getsize(os.path.join(dp, n))
                   for d in (os.path.join(out, e.name), os.path.join(out, e.name + ".expected"))
                   for dp, _, ns in os.walk(d) for n in ns)
        assert size < 300_000, (e.name, size)
        print(f"wrote {e.name} ({size} bytes)")

    # Equal content for equal rank (the plain and encrypted v2).
    one = lambda n: run_steps([(n, "L1")], models)
    assert one("imp-dev1-v2-plain") == one("imp-dev1-v2-enc")
    assert models["imp-dev1-v2-plain"].rank == models["imp-dev1-v2-enc"].rank

    results = {}
    for name, orders, _ in SCENARIOS:
        contents = [run_steps(steps, models) for steps in orders]
        for c in contents[1:]:
            d = diff_paths(contents[0], c)
            assert not d, (name, d[:10])
        results[name] = contents[0]
        write_json(os.path.join(out, "scenarios", f"{name}.content.json"),
                   document(orders, models, contents[0]))
        print(f"wrote scenario {name}")
    for (x, y), allowed in ASYMMETRIC.items():
        d = diff_paths(results[x], results[y])
        assert d, (x, y)
        bad = [p for p in d if not any(f"/{k}" in p for k in allowed)]
        assert not bad, (x, y, bad[:10])
    # Re-importing the same backup changes nothing.
    assert results["keys-hostile-twice"] == one("imp-keys-hostile-plain")

    write_sums(out, "SHA256SUMS", lambda rel: not is_expected(rel))
    write_sums(out, "EXPECTED.SHA256SUMS", is_expected)
    import cryptography
    ent = "\n".join(f"| `{e.name}` | {e.purpose} |" for e in entries)
    scn = "\n".join(f"| `{n}` | {len(o)} | {why} |" for n, o, why in SCENARIOS)
    with open(os.path.join(out, "README.md"), "w", encoding="utf-8", newline="\n") as f:
        text = README
        for k, v in (("password", PASSWORD), ("python", sys.version.split()[0]),
                     ("sqlite", sqlite3.sqlite_version), ("cryptography", cryptography.__version__),
                     ("seed", str(a.seed)), ("entries", ent), ("scenarios", scn)):
            text = text.replace("{" + k + "}", v)
        f.write(text)
    for n in ("SHA256SUMS", "EXPECTED.SHA256SUMS"):
        with open(os.path.join(out, n), "rb") as f:
            print(f"{n} sha256 {hashlib.sha256(f.read()).hexdigest()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
