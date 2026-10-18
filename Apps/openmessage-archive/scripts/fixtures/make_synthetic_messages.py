#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""Build the SYNTHETIC messages catalogue for OpenMessage Archive (M3).

Every value written here is invented: example.invalid addresses, +1555
numbers marked SYNTH, and sentinel text. No real device, person, message or
backup is used, and nothing from any sample database (S3, S12) is copied,
DDL text included.

Usage:

  make_synthetic_messages.py OUT_DIR [--seed N] [--allow-other-sqlite]

Writes OUT_DIR/<name>/SYNTHETIC0-0000000000000000/ (one backup per entry),
OUT_DIR/<name>.expected/ (the plaintext sms.db, its -wal and attachment
files, and report.json), OUT_DIR/SHA256SUMS and OUT_DIR/README.md.

report.json is the generator's own model of what `oma_messages::check()`
must report, computed from the rows it wrote by a second statement of the
rules in M3 sections 5-13 (it does not run the Rust code). Agreement between
the two proves self-consistency only; external agreement comes from the
oracle runs recorded in docs/compatibility.md.

It imports make_synthetic_backups (the M2 generator) as a module for the
backup format (keybag, key wrap, AES-CBC, MBFile records, Manifest.db, WAL
salt normalisation) and leaves that module untouched, and
make_synthetic_typedstream for attributedBody blobs and their expected
decode. Hostile typedstream blobs are the committed single-fault vectors in
tests/fixtures/typedstream, with their kind taken from its expected.json.

SQLite file bytes depend on the SQLite version, so the script refuses to
write under any SQLite but the reference 3.45.1 unless --allow-other-sqlite
is given. Run with `python -I`.
"""
import argparse
import datetime as dt
import hashlib
import json
import math
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
import make_synthetic_typedstream as ts  # noqa: E402
import sms_profiles as sp  # noqa: E402

from cryptography.hazmat.primitives.keywrap import aes_key_wrap  # noqa: E402

REFERENCE_SQLITE = "3.45.1"
UDID = mb.SYNTHETIC_UDID
PASSWORD = mb.DEFAULT_PASSWORD
TS_DIR = os.path.normpath(os.path.join(HERE, "..", "..", "tests", "fixtures", "typedstream"))

COCOA = 978_307_200
NS = 1_000_000_000
NS_THRESHOLD = 1_000_000_000_000
SECONDS_MAX = 3_124_137_600
NS_MAX = 3_124_137_600_000_000_000
RANGE_FLOOR_NS = 1_167_609_600 * NS
I64_MIN = -(2 ** 63)
I64_MAX = 2 ** 63 - 1
GUID_SET_CAP = 4_194_304
HISTOGRAM_CAP = 16
PAGE_SIZE = 1024

# A date in 2026, in Cocoa nanoseconds and seconds.
T0_S = 790_000_000
T0_NS = T0_S * NS


# --------------------------------------------------------------------------
# Values
# --------------------------------------------------------------------------

class TextBytes:
    """Bytes stored with storage class TEXT (they may be invalid UTF-8)."""

    def __init__(self, raw):
        self.raw = raw


class AB:
    """An attributedBody blob and its expected public decode result:
    {"ok": {...}} or {"error": kind}, as in typedstream/expected.json."""

    def __init__(self, data, public):
        self.data = data
        self.public = public


class Summary:
    """A message_summary_info blob and its expected parse:
    ("ok", info) or ("unreadable", issue)."""

    def __init__(self, data, expect):
        self.data = data
        self.expect = expect


def ab_text(segs):
    """An attributed string written by the typedstream writer."""
    text, runs, dicts = ts.segments(segs)
    data = ts.build_attributed(text.encode("utf-8"), runs, dicts)
    return AB(data, {"ok": ts.model(text, runs, dicts)})


def ab_plain(text):
    return ab_text([(text, [(ts.P, 0)])])


_TS_EXPECTED = None


def ab_vector(name):
    """A committed typedstream vector, with its expected public result."""
    global _TS_EXPECTED
    if _TS_EXPECTED is None:
        with open(os.path.join(TS_DIR, "expected.json"), encoding="utf-8") as f:
            _TS_EXPECTED = {v["name"]: v for v in json.load(f)["vectors"]}
    with open(os.path.join(TS_DIR, name + ".bin"), "rb") as f:
        data = f.read()
    return AB(data, _TS_EXPECTED[name]["public"])


def summary(otr=None, ec=None, rp=None, tmp=False, unknown=None):
    """A well-formed summary. ec: {part: [{"d": value, "t": bytes}, ...]}."""
    root = {}
    if otr is not None:
        root["otr"] = {str(p): {} for p in range(otr)}
    if ec is not None:
        root["ec"] = {str(k): v for k, v in ec.items()}
    if rp is not None:
        root["rp"] = list(rp)
    if tmp:
        root["tmp"] = {"SYNTH-LANG": "SYNTH-TRANSLATION"}
    for k, v in (unknown or {}).items():
        root[k] = v
    info = {
        "otr": None if otr is None else ("value", otr),
        "ec": None if ec is None else ("value", [(int(k), [e.get("d") for e in v]) for k, v in ec.items()]),
        "rp": None if rp is None else ("value", list(rp)),
        "tmp": tmp,
        "unknown": len(unknown or {}),
    }
    return Summary(plistlib.dumps(root, fmt=plistlib.FMT_BINARY), ("ok", info))


def summary_raw(data, issue):
    return Summary(data, ("unreadable", issue))


def summary_shape(root, info):
    """A summary with a field of the wrong shape; `info` states the parse."""
    return Summary(plistlib.dumps(root, fmt=plistlib.FMT_BINARY), ("ok", info))


def nested(depth):
    x = [0]
    for _ in range(depth):
        x = [x]
    return x


# --------------------------------------------------------------------------
# The rules (a second statement of M3 sections 5, 8, 11 and 12)
# --------------------------------------------------------------------------

def time_interp(raw):
    """(kind, unix_ns) for a raw Cocoa value (section 8.1)."""
    if raw == 0:
        return ("zero", None)
    if raw < 0:
        return ("negative", None)
    if raw < SECONDS_MAX:
        return ("seconds", (raw + COCOA) * NS)
    if raw < NS_THRESHOLD:
        return ("ambiguous", None)
    if raw < NS_MAX:
        return ("nanoseconds", raw + COCOA * NS)
    return ("ambiguous", None)


def real_seconds_interp(r):
    if not math.isfinite(r):
        return ("ambiguous", None)
    if r == 0.0:
        return ("zero", None)
    if r < 0.0:
        return ("negative", None)
    if r >= SECONDS_MAX:
        return ("ambiguous", None)
    return ("seconds", None)


def summary_time_interp(d):
    if d is None:
        return None
    if isinstance(d, bool):
        raise TypeError(d)
    if isinstance(d, int):
        return time_interp(d)[0]
    if isinstance(d, float):
        return real_seconds_interp(d)[0]
    if isinstance(d, dt.datetime):
        cocoa = (d.replace(tzinfo=dt.timezone.utc) - dt.datetime(2001, 1, 1, tzinfo=dt.timezone.utc)).total_seconds()
        if cocoa == 0:
            return "zero"
        if cocoa < 0:
            return "negative"
        return "seconds" if cocoa < SECONDS_MAX else "ambiguous"
    raise TypeError(d)


class Issues:
    def __init__(self):
        self.c = {k: 0 for k in ("real_in_integer_column", "invalid_utf8_text", "blob_in_text_column",
                                 "text_in_blob_column", "type_mismatch", "bool_out_of_range",
                                 "plist_invalid", "plist_over_budget", "plist_not_dictionary",
                                 "value_oversized")}
        self.by_column = {}

    def oversized(self, key, n):
        self.c["value_oversized"] += 1
        self.by_column[key] = self.by_column.get(key, 0) + 1
        return ("oversized", n)

    def unreadable(self, kind):
        self.c[{"TypeMismatch": "type_mismatch", "BoolOutOfRange": "bool_out_of_range",
                "PlistInvalid": "plist_invalid", "PlistOverBudget": "plist_over_budget",
                "PlistNotDictionary": "plist_not_dictionary"}[kind]] += 1
        return ("unreadable", kind)


def raw_bytes(v):
    if isinstance(v, AB):
        return v.data
    if isinstance(v, Summary):
        return v.data
    return v


def read_integer(v, issues):
    if v is None:
        return ("null", None)
    if isinstance(v, bool):
        raise TypeError(v)
    if isinstance(v, int):
        return ("value", v)
    if isinstance(v, float):
        if math.isfinite(v) and v == int(v) and -(2.0 ** 63) <= v < 2.0 ** 63:
            issues.c["real_in_integer_column"] += 1
            return ("value", int(v))
        return issues.unreadable("TypeMismatch")
    return issues.unreadable("TypeMismatch")


def read_col(table, col, v, issues):
    """How the Rust readers see one present known column's value."""
    k = sp.known(table, col)
    kind, cap = k[1], k[2]
    key = f"{table}.{col}"
    if kind in ("rowid", "int"):
        return read_integer(v, issues)
    if kind == "bool":
        r = read_integer(v, issues)
        if r[0] != "value":
            return r
        if r[1] in (0, 1):
            return ("value", bool(r[1]))
        return issues.unreadable("BoolOutOfRange")
    if kind == "time":
        r = read_integer(v, issues)
        return ("value", time_interp(r[1])) if r[0] == "value" else r
    if kind == "typeonly":
        return ("null", None) if v is None else ("value", type(v).__name__)
    v = raw_bytes(v)
    if v is None:
        return ("null", None)
    if isinstance(v, (int, float)):
        return issues.unreadable("TypeMismatch")
    if kind == "text":
        if isinstance(v, str):
            b, is_text = v.encode("utf-8"), True
        elif isinstance(v, TextBytes):
            b, is_text = v.raw, True
        else:
            b, is_text = v, False
        if len(b) > cap:
            return issues.oversized(key, len(b))
        if is_text:
            try:
                b.decode("utf-8")
            except UnicodeDecodeError:
                issues.c["invalid_utf8_text"] += 1
        else:
            issues.c["blob_in_text_column"] += 1
        return ("value", b)
    # blob, blobsize
    if isinstance(v, str):
        b, is_text = v.encode("utf-8"), True
    elif isinstance(v, TextBytes):
        b, is_text = v.raw, True
    else:
        b, is_text = v, False
    if len(b) > cap:
        return issues.oversized(key, len(b))
    if is_text:
        issues.c["text_in_blob_column"] += 1
    return ("value", b if kind == "blob" else len(b))


def service_class(r):
    if r[0] != "value":
        return "not_text"
    b = r[1]
    if b == b"":
        return "empty"
    low = b.lower() if all(c < 0x80 for c in b) else None
    return {b"imessage": "imessage", b"sms": "sms", b"rcs": "rcs",
            b"imessagelite": "imessage_lite"}.get(low, "other")


def assoc_target(b):
    def guid(x):
        return x if x and b"\0" not in x else None
    if b.startswith(b"bp:"):
        return "bubble" if guid(b[3:]) else "unparsed", guid(b[3:])
    if b.startswith(b"p:"):
        rest = b[2:]
        if b"/" not in rest:
            return "unparsed", None
        digits, tail = rest.split(b"/", 1)
        ok = digits and all(48 <= c <= 57 for c in digits) and int(digits) <= 0xFFFFFFFF
        if ok and guid(tail):
            return "part", tail
        return "unparsed", None
    return ("bare", b) if guid(b) else ("unparsed", None)


def assoc_kind(raw):
    if raw == 0:
        return "no_association"
    if raw == 1000:
        return "sticker_legacy"
    if raw == 4000:
        return "poll_vote"
    names = ["love", "like", "dislike", "laugh", "emphasize", "question", "emoji", "sticker"]
    if 2000 <= raw <= 2007:
        return "added:" + names[raw - 2000]
    if 3000 <= raw <= 3007:
        return "removed:" + names[raw - 3000]
    return "unknown"


def item_kind(it, gat, h, o):
    iv = it[1] if it[0] == "value" else None
    gv = gat[1] if gat[0] == "value" else None
    if iv == 0:
        return "message"
    if iv == 1:
        if gv == 0:
            hv = h[1] if h[0] == "value" else None
            ov = o[1] if o[0] == "value" else None
            if hv is not None and ov is not None and hv != 0 and hv == ov:
                return "own_number_changed"
            return "participant_added"
        if gv == 1:
            return "participant_removed"
        return "unknown"
    if iv == 2:
        return "group_renamed"
    if iv == 3:
        return {0: "participant_left", 1: "group_photo_changed", 2: "group_photo_removed",
                4: "background_changed", 6: "background_removed"}.get(gv, "unknown")
    return {4: "legacy_shared_location", 5: "audio_kept", 6: "share_play"}.get(iv, "unknown")


def edit_label(date_edited, summ, text, ab_state):
    """Section 11.3; summ is ("absent"|"null"|"unreadable"|"oversized"|"value", info)."""
    if date_edited[0] == "absent" and summ[0] == "absent":
        return "not_recorded"
    if summ[0] in ("unreadable", "oversized"):
        return "summary_unreadable"
    info = summ[1] if summ[0] == "value" else None
    unsent = len(info["rp"][1]) if info and info["rp"] and info["rp"][0] == "value" else 0
    edited = len(info["ec"][1]) if info and info["ec"] and info["ec"][0] == "value" else 0
    if unsent and edited:
        return "unsent_parts_and_edit_history"
    if unsent:
        return "unsent_parts"
    if edited:
        return "edit_history"
    bad = info is not None and any(info[f] and info[f][0] == "bad" for f in ("ec", "rp"))
    is_set = date_edited[0] == "value" and date_edited[1][0] in ("seconds", "nanoseconds")
    text_empty = text[0] == "null" or (text[0] == "value" and text[1] == b"")
    if is_set and not bad and text_empty and ab_state == "null":
        return "date_edited_no_body"
    if is_set:
        return "date_edited_only"
    unset = date_edited[0] in ("absent", "null") or (date_edited[0] == "value" and date_edited[1][0] == "zero")
    if unset and not bad:
        return "no_edit_recorded"
    return "undetermined"


_SANITIZE_RESERVED = {"CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"} | {
    f"{p}{d}" for p in ("COM", "LPT") for d in "0123456789\u00b9\u00b2\u00b3"}


def unusual_component(c):
    """Whether oma_fs::sanitize_filename would alter a component (a second
    statement of its rules, for the names this catalogue uses)."""
    import unicodedata
    if len(c.encode("utf-8")) > 255 or c.endswith((".", " ")) or c.startswith(" "):
        return True
    for ch in c:
        if ch in '/\\<>:"|?*' or unicodedata.category(ch) == "Cc":
            return True
        if ch in "\u200e\u200f\u061c" or "\u202a" <= ch <= "\u202e" or "\u2066" <= ch <= "\u2069":
            return True
    return c.split(".")[0].rstrip(" ").upper() in _SANITIZE_RESERVED


def path_class(b):
    if b.startswith(b"~/Library/SMS/Attachments/"):
        return "sms_attachments"
    if b.startswith(b"~/Library/SMS/StickerCache/"):
        return "sms_sticker_cache"
    if b.startswith(b"~/Library/Messages/"):
        return "mac_messages"
    if b.startswith(b"~/"):
        return "other_home_relative"
    if b.startswith(b"/var/mobile/"):
        return "absolute_var_mobile"
    if b.startswith(b"/private/var/mobile/"):
        return "absolute_private_var_mobile"
    return "unrecognized"


def candidate(b):
    try:
        rel = b[2:].decode("utf-8")
    except UnicodeDecodeError:
        return None
    if "\0" in rel or "\\" in rel or rel.startswith("/") or (len(rel) > 1 and rel[1] == ":"):
        return None
    comps = rel.split("/")
    if any(c in ("", ".", "..") for c in comps) or any(unusual_component(c) for c in comps):
        return None
    return rel


# --------------------------------------------------------------------------
# One fixture
# --------------------------------------------------------------------------

class AttFile:
    """A file the backup lists for an attachment (MediaDomain)."""

    def __init__(self, rel, payload=b"", state="present", directory=False):
        self.rel = rel
        self.payload = payload
        self.state = state  # present | missing | no_key | class_unavailable | bad_key
        self.directory = directory


class Spec:
    def __init__(self, name, profile, rows, encrypted=False):
        self.name = name
        self.profile = profile
        self.rows = rows
        self.encrypted = encrypted
        self.scramble = False
        self.extra_sql = []
        self.wal = None            # None | "wal" | "missing" | "badframes"
        self.wal_rows = []         # [(table, row)] committed in the -wal (txn 1)
        self.wal_rows2 = []        # txn 2 (corrupted in "badframes")
        self.files = []            # [AttFile]
        self.corrupt = False
        self.rowid_desc = False
        self.expect_status = None  # overrides the model's status for non-readable fixtures
        self.decl = {}             # (table, column) -> declared type, overriding the profile


def column_order(spec, table, cols, rng):
    cols = list(cols)
    if spec.scramble:
        rng.shuffle(cols)
    return cols


def create_sql(spec, table, cols):
    if cols is None:
        return f'CREATE TABLE "{table}" ({sp.COUNT_ONLY_DDL[table]})'
    defs = []
    for c in cols:
        decl = spec.decl.get((table, c), sp.column_decl(table, c))
        if c == "ROWID" and spec.rowid_desc and table == "message":
            decl = "INTEGER PRIMARY KEY DESC"
        defs.append(f'"{c}" {decl}')
    return f'CREATE TABLE "{table}" ({", ".join(defs)})'


def insert(con, table, row):
    cols = list(row)
    marks = []
    params = []
    for c in cols:
        v = row[c]
        if isinstance(v, TextBytes):
            marks.append("CAST(? AS TEXT)")
            params.append(v.raw)
        else:
            marks.append("?")
            params.append(raw_bytes(v))
    sql = f'INSERT INTO "{table}" ({", ".join(chr(34) + c + chr(34) for c in cols)}) VALUES ({", ".join(marks)})'
    con.execute(sql, params)


def present_cols(spec, table):
    cols = spec.profile.get(table)
    return cols


def filtered(spec, table, row):
    cols = spec.profile.get(table)
    if cols is None:
        return dict(row)
    return {k: v for k, v in row.items() if k in cols or k == "rowid"}


def build_db(spec, work, rng):
    """Write sms.db (and its -wal). Returns (main bytes, wal bytes or None)."""
    path = os.path.join(work, "sms.db")
    con = sqlite3.connect(path, isolation_level=None)
    con.execute(f"PRAGMA page_size={PAGE_SIZE}")
    con.execute("BEGIN")
    for table in sp.TABLE_ORDER + [t for t in spec.profile if t not in sp.TABLE_ORDER]:
        if table not in spec.profile:
            continue
        cols = spec.profile[table]
        if cols is not None:
            cols = column_order(spec, table, cols, rng)
        con.execute(create_sql(spec, table, cols))
    for table, idx, col in sp.INDEXES:
        cols = spec.profile.get(table)
        if cols is not None and col in cols:
            con.execute(f'CREATE INDEX "{idx}" ON "{table}" ("{col}")')
    for sql in spec.extra_sql:
        con.execute(sql)
    for table, rows in spec.rows.items():
        for row in rows:
            insert(con, table, filtered(spec, table, row))
    con.execute("COMMIT")
    wal = None
    if spec.wal:
        con.execute("PRAGMA journal_mode=WAL")
        con.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        con.execute("PRAGMA wal_autocheckpoint=0")
        con.execute("BEGIN")
        for table, row in spec.wal_rows:
            insert(con, table, filtered(spec, table, row))
        con.execute("COMMIT")
        if spec.wal_rows2:
            con.execute("BEGIN")
            for table, row in spec.wal_rows2:
                insert(con, table, filtered(spec, table, row))
            con.execute("COMMIT")
        with open(path + "-wal", "rb") as f:
            wal = mb.normalize_wal(f.read(), 0x5359_4E54, 0x4D53_4753)  # "SYNT", "MSGS"
        with open(path, "rb") as f:
            main = f.read()
        con.close()
        with open(path, "wb") as f:
            f.write(main)
        for suffix in ("-wal", "-shm"):
            if os.path.exists(path + suffix):
                os.remove(path + suffix)
        if spec.wal == "badframes":
            wal = corrupt_second_transaction(wal)
        return main, wal
    con.close()
    with open(path, "rb") as f:
        main = f.read()
    if spec.corrupt:
        main = corrupt_page(main, spec)
    return main, None


def wal_frames(wal):
    """(page_size, [(pgno, commit, offset)]) for a normalized -wal."""
    page_size = struct.unpack(">I", wal[8:12])[0]
    frames = []
    pos = 32
    while pos + 24 + page_size <= len(wal):
        pgno, commit = struct.unpack(">II", wal[pos:pos + 8])
        frames.append((pgno, commit, pos))
        pos += 24 + page_size
    return page_size, frames


def corrupt_second_transaction(wal):
    """Flip one byte in the page data of the first frame after the first
    commit frame, so its checksum (and every later one) no longer chains."""
    page_size, frames = wal_frames(wal)
    first_commit = next(i for i, f in enumerate(frames) if f[1] != 0)
    target = frames[first_commit + 1][2] + 24 + 100
    b = bytearray(wal)
    b[target] ^= 0xFF
    return bytes(b)


def wal_facts(wal, db_page_size):
    """A second implementation of the -wal facts (sqlite.org/fileformat2.html)."""
    magic, version, page_size, _ckpt, salt1, salt2, c1, c2 = struct.unpack(">IIIIIIII", wal[:32])
    big = bool(magic & 1)
    header_valid = magic in (0x377F0682, 0x377F0683) and version == 3007000 \
        and page_size >= 512 and page_size & (page_size - 1) == 0 \
        and mb.wal_checksum(wal[:24], 0, 0, big) == (c1, c2)
    _, frames = wal_frames(wal)
    s0, s1 = c1, c2
    valid = 0
    committed = 0
    invalid_at = None
    for i, (pgno, commit, pos) in enumerate(frames):
        fs1, fs2, fc1, fc2 = struct.unpack(">IIII", wal[pos + 8:pos + 24])
        n0, n1 = mb.wal_checksum(wal[pos:pos + 8] + wal[pos + 24:pos + 24 + page_size], s0, s1, big)
        if header_valid and (fs1, fs2) != (salt1, salt2):
            # Other salts end the log: frames of an earlier generation left
            # after a checkpoint reset, not damage (not counted as invalid).
            break
        if not header_valid or pgno == 0 or (n0, n1) != (fc1, fc2):
            invalid_at = i
            break
        s0, s1 = n0, n1
        valid += 1
        if commit != 0:
            committed = valid
    after = len(frames) - invalid_at if invalid_at is not None else 0
    return {"valid_committed_frames": committed, "frames_after_first_invalid": after,
            "header_valid": header_valid, "page_size_matches": page_size == db_page_size}


def corrupt_page(main, spec):
    """Damage the b-tree page of the sentinel table holding many rows, so
    quick_check reports it. The scan, which reads page 1 only, still works."""
    path = tempfile.mkdtemp()
    p = os.path.join(path, "x.db")
    with open(p, "wb") as f:
        f.write(main)
    con = sqlite3.connect(p)
    root = con.execute("SELECT rootpage FROM sqlite_schema WHERE name = 'SYNTH_SENTINEL_CORRUPT_TABLE'").fetchone()[0]
    con.close()
    shutil.rmtree(path)
    b = bytearray(main)
    off = (root - 1) * PAGE_SIZE
    # Overwrite the cell-pointer array of the table's root page.
    for i in range(8, 40):
        b[off + i] = 0xEE
    return bytes(b)


# --------------------------------------------------------------------------
# The model: what check() must report
# --------------------------------------------------------------------------

def open_copy(main, wal, work):
    """A scratch copy of the database (with its -wal beside it) to read."""
    path = os.path.join(work, "readback.db")
    with open(path, "wb") as f:
        f.write(main)
    if wal is not None:
        with open(path + "-wal", "wb") as f:
            f.write(wal)
    con = sqlite3.connect(path)
    con.text_factory = bytes
    return con


def read_back(spec, main, wal):
    """The values as SQLite stored them (after affinity, defaults and the
    -wal), each tagged with its storage class. The rules are then applied to
    these values by the model below; SQLite's own storage is not one of the
    rules under test."""
    rows = {}
    counts = {}
    with tempfile.TemporaryDirectory() as work:
        con = open_copy(main, wal, work)
        names = {r[0].decode() for r in con.execute("SELECT name FROM sqlite_schema WHERE type = 'table'")}
        for t, cols in spec.profile.items():
            if t not in names:
                continue
            counts[t] = con.execute(f'SELECT count(*) FROM "{t}"').fetchone()[0]
            if cols is None or t not in sp.KNOWN:
                continue
            key = '"ROWID"' if t in sp.ROWID_TABLES else "rowid"
            sel = ", ".join(f'typeof("{c}"), "{c}"' for c in cols)
            out = []
            for r in con.execute(f'SELECT {key}, {sel} FROM "{t}" ORDER BY {key}'):
                row = {}
                for i, c in enumerate(cols):
                    typ, v = r[1 + 2 * i].decode(), r[2 + 2 * i]
                    row[c] = {"null": None, "integer": v, "real": v,
                              "text": TextBytes(v) if typ == "text" else None,
                              "blob": bytes(v) if typ == "blob" else None}[typ]
                out.append((r[0], row))
            rows[t] = out
        con.close()
    return rows, counts


def annotations(spec):
    """attributedBody and message_summary_info blobs, by their bytes."""
    ann = {}
    every = [r for rs in spec.rows.values() for r in rs] + [r for _, r in spec.wal_rows + spec.wal_rows2]
    for r in every:
        for v in r.values():
            if isinstance(v, (AB, Summary)):
                ann[v.data] = v
    return ann


def sql_ne_zero(v):
    """`v <> 0` is true (SQL semantics: NULL is not; text never equals 0)."""
    if v is None:
        return False
    if isinstance(v, (int, float)):
        return v != 0
    return True


def sql_not_in(v, targets):
    """`v NOT IN (integer keys)` is true."""
    if v is None:
        return False
    if isinstance(v, float):
        return not (v == int(v) and int(v) in targets) if math.isfinite(v) else True
    if isinstance(v, int):
        return v not in targets
    return True


def scan_counts(main):
    """The scan's counting rules, applied to the written schema."""
    path = tempfile.mkdtemp()
    p = os.path.join(path, "x.db")
    with open(p, "wb") as f:
        f.write(main)
    con = sqlite3.connect(f"file:{p}?mode=ro", uri=True)
    rows = con.execute("SELECT type, name, rootpage, sql FROM sqlite_schema").fetchall()
    enc = con.execute("PRAGMA encoding").fetchone()[0]
    con.close()
    shutil.rmtree(path)
    known = {t.lower() for t in sp.TABLE_ORDER}
    c = {"virtual": 0, "unknown_tables": 0, "views": 0, "triggers": 0, "indexes": 0,
         "stat_tables": 0, "internal_objects": 0, "entries": {}, "ordinary": {}}
    for kind, name, rootpage, sql in rows:
        lname = name.lower()
        is_known = lname in known
        if kind == "table":
            if lname.startswith("sqlite_stat"):
                c["stat_tables"] += 1
            elif lname.startswith("sqlite_"):
                c["internal_objects"] += 1
            elif rootpage == 0 or (sql or "").lstrip().upper().startswith("CREATE VIRTUAL TABLE"):
                c["virtual"] += 1
                if is_known:
                    c["entries"][lname] = c["entries"].get(lname, 0) + 1
            elif is_known:
                c["entries"][lname] = c["entries"].get(lname, 0) + 1
                c["ordinary"][lname] = c["ordinary"].get(lname, 0) + 1
            else:
                c["unknown_tables"] += 1
        elif kind == "view":
            c["views"] += 1
            if is_known:
                c["entries"][lname] = c["entries"].get(lname, 0) + 1
        elif kind == "index":
            if not lname.startswith("sqlite_autoindex_"):
                c["indexes"] += 1
            if is_known:
                c["entries"][lname] = c["entries"].get(lname, 0) + 1
        elif kind == "trigger":
            c["triggers"] += 1
        else:
            c["internal_objects"] += 1
    c["encoding"] = {"UTF-8": "utf8", "UTF-16le": "utf16le", "UTF-16be": "utf16be"}[enc]
    return c


def schema_model(spec, rows, counts, scan):
    tables = []
    reasons = []
    for t in sp.TABLE_ORDER:
        lt = t.lower()
        entries = scan["entries"].get(lt, 0)
        ordinary = entries == 1 and scan["ordinary"].get(lt, 0) == 1
        tr = {"table": t, "present": False, "rows": None, "missing": [], "unknown": 0,
              "generated": 0, "wrong_case": 0}
        if entries == 0:
            if t in sp.REQUIRED_TABLES:
                reasons.append(f"missing_table:{t}")
            tables.append(tr)
            continue
        if not ordinary:
            reasons.append(f"not_ordinary_table:{t}")
            tables.append(tr)
            continue
        tr["present"] = True
        cols = spec.profile.get(t)
        if t not in sp.COUNT_ONLY:
            names = [c[0] for c in sp.KNOWN[t]]
            for c in cols:
                if c in names:
                    continue
                if c.lower() in [n.lower() for n in names]:
                    tr["wrong_case"] += 1
                else:
                    tr["unknown"] += 1
            tr["missing"] = [n for n in names if n not in cols]
            for c in sp.KNOWN[t]:
                if c[3] and c[0] not in cols:
                    reasons.append(f"missing_required_column:{t}.{c[0]}")
            if t == "message" and spec.rowid_desc:
                reasons.append("rowid_shape:message")
        tr["rows"] = counts[t]
        tables.append(tr)
    have = lambda t, c: any(x["table"] == t and x["present"] and c not in x["missing"]
                            for x in tables) and c in [k[0] for k in sp.KNOWN[t]]
    present = lambda t: any(x["table"] == t and x["present"] for x in tables)
    feats = []
    for name, ok in [
        ("attributed_body", have("message", "attributedBody")),
        ("tapbacks", have("message", "associated_message_guid") and have("message", "associated_message_type")),
        ("emoji_tapbacks", have("message", "associated_message_emoji")),
        ("replies", have("message", "thread_originator_guid")),
        ("edits_and_unsends", have("message", "message_summary_info")
         and (have("message", "date_edited") or have("message", "date_retracted"))),
        ("recently_deleted", present("chat_recoverable_message_join")),
        ("scheduled", have("message", "schedule_type")),
        ("filtering", have("message", "filter_action") or have("chat", "is_filtered")),
        ("attachments", present("attachment") and present("message_attachment_join")),
        ("genmoji", have("attachment", "emoji_image_short_description")),
    ]:
        if ok:
            feats.append(name)
    cv = None
    if have("_SqliteDatabaseProperties", "key") and have("_SqliteDatabaseProperties", "value"):
        vals = [r.get("value") for _, r in rows.get("_SqliteDatabaseProperties", [])
                if isinstance(r.get("key"), TextBytes) and r["key"].raw == b"_ClientVersion"]
        if len(vals) == 1 and isinstance(vals[0], TextBytes):
            raw = vals[0].raw
            if 1 <= len(raw) <= 18 and all(48 <= c <= 57 for c in raw):
                cv = int(raw)
    schema = {"tables": tables, "unknown_tables": scan["unknown_tables"], "views": scan["views"],
              "triggers": scan["triggers"], "indexes": scan["indexes"],
              "stat_tables": scan["stat_tables"], "internal_objects": scan["internal_objects"],
              "encoding": scan["encoding"], "client_version": cv,
              "client_version_parsed": cv is not None, "features": feats}
    return schema, reasons


def zero_if(b):
    return 0 if b else None


def inc(d, k, n=1):
    if d is not None and d.get(k) is not None:
        d[k] += n


def svc_counts():
    return {k: 0 for k in ("imessage", "sms", "rcs", "imessage_lite", "empty", "other", "not_text")}


def hist_new():
    return {"entries": [], "other": 0, "null": 0, "unreadable": 0}


def hist_add(h, r):
    if h is None or r[0] == "absent":
        return
    if r[0] == "null":
        h["null"] += 1
    elif r[0] != "value":
        h["unreadable"] += 1
    else:
        for e in h["entries"]:
            if e[0] == r[1]:
                e[1] += 1
                return
        if len(h["entries"]) < HISTOGRAM_CAP:
            h["entries"].append([r[1], 1])
            h["entries"].sort(key=lambda e: e[0])
        else:
            h["other"] += 1


def units_new():
    return {k: 0 for k in ("zero", "negative", "seconds", "nanoseconds", "ambiguous", "null", "unreadable")}


def units_add(h, r):
    if h is None or r[0] == "absent":
        return
    if r[0] == "null":
        h["null"] += 1
    elif r[0] != "value":
        h["unreadable"] += 1
    else:
        h[r[1][0]] += 1


def model(spec, rows, counts, ann, files_by_rel, checked_at_ns):
    """The report projection for a readable database."""
    issues = Issues()
    prof = spec.profile

    def has(t, c):
        return prof.get(t) is not None and c in prof[t]

    def col(t, row, c):
        if not has(t, c):
            return ("absent", None)
        return read_col(t, c, row.get(c), issues)

    # ---- handles
    handles = {"rows": 0, "by_service": svc_counts() if has("handle", "service") else None}
    for _, r in rows.get("handle", []):
        handles["rows"] += 1
        s = col("handle", r, "service")
        read_all_cols_except(t="handle", row=r, cols=["service"], col=col, prof=prof)
        if handles["by_service"] is not None:
            handles["by_service"][service_class(s)] += 1

    # ---- chats
    chats = {"rows": 0, "by_service": svc_counts() if has("chat", "service_name") else None,
             "with_no_other_handle": None, "with_one_other_handle": None,
             "with_several_other_handles": None,
             "archived": zero_if(has("chat", "is_archived")),
             "is_filtered": hist_new() if has("chat", "is_filtered") else None,
             "style": hist_new() if has("chat", "style") else None,
             "with_properties": zero_if(has("chat", "properties"))}
    dates = {"message_date": units_new(),
             "chat_message_date": units_new() if has("chat_message_join", "message_date") else None,
             "chat_last_read": units_new() if has("chat", "last_read_message_timestamp") else None,
             "recoverable_delete_date": units_new() if has("chat_recoverable_message_join", "delete_date") else None,
             "edit_entry_date": units_new() if has("message", "message_summary_info") else None,
             "earliest_message": None, "latest_message": None,
             "excluded_scheduled": zero_if(has("message", "schedule_type")),
             "excluded_not_classified": 0, "excluded_before_floor": 0, "excluded_after_checked_at": 0}
    for _, r in rows.get("chat", []):
        chats["rows"] += 1
        vals = {c: col("chat", r, c) for c in prof["chat"] if sp.known("chat", c) and c != "ROWID"}
        if chats["by_service"] is not None:
            chats["by_service"][service_class(vals["service_name"])] += 1
        if vals.get("is_archived", ("absent",))[0] == "value" and vals["is_archived"][1] is True:
            chats["archived"] += 1
        hist_add(chats["is_filtered"], vals.get("is_filtered", ("absent", None)))
        hist_add(chats["style"], vals.get("style", ("absent", None)))
        if vals.get("properties", ("absent",))[0] in ("value", "oversized"):
            chats["with_properties"] += 1
        units_add(dates["chat_last_read"], vals.get("last_read_message_timestamp", ("absent", None)))

    # ---- links
    def ids(t, c):
        return [r.get(c) for _, r in rows.get(t, [])]

    chat_ids = {k for k, _ in rows.get("chat", [])}
    handle_ids = {k for k, _ in rows.get("handle", [])}
    message_ids = {k for k, _ in rows.get("message", [])}
    attachment_ids = {k for k, _ in rows.get("attachment", [])}
    for _, r in rows.get("chat_handle_join", []):
        read_all_cols_except("chat_handle_join", r, [], col, prof)
    for _, r in rows.get("chat_message_join", []):
        read_all_cols_except("chat_message_join", r, ["message_date"], col, prof)
        units_add(dates["chat_message_date"], col("chat_message_join", r, "message_date"))
    recoverable = "chat_recoverable_message_join" in prof
    deleted = set()
    for _, r in rows.get("chat_recoverable_message_join", []):
        read_all_cols_except("chat_recoverable_message_join", r, ["delete_date", "message_id"], col, prof)
        units_add(dates["recoverable_delete_date"], col("chat_recoverable_message_join", r, "delete_date"))
        m = col("chat_recoverable_message_join", r, "message_id")
        if m[0] == "value":
            deleted.add(m[1])
    links = {
        "chat_message_rows": counts["chat_message_join"],
        "chat_handle_rows": counts["chat_handle_join"],
        "message_attachment_rows": counts.get("message_attachment_join"),
        "recoverable_rows": counts.get("chat_recoverable_message_join"),
    }

    def orphans(t, c, targets, needs):
        if t not in prof or not needs:
            return None
        return sum(1 for v in ids(t, c) if sql_not_in(v, targets))

    links["orphan_chat_message_chat"] = orphans("chat_message_join", "chat_id", chat_ids, True)
    links["orphan_chat_message_message"] = orphans("chat_message_join", "message_id", message_ids, True)
    links["orphan_chat_handle_chat"] = orphans("chat_handle_join", "chat_id", chat_ids, True)
    links["orphan_chat_handle_handle"] = orphans("chat_handle_join", "handle_id", handle_ids, True)
    links["orphan_message_attachment_message"] = orphans("message_attachment_join", "message_id", message_ids, True)
    links["orphan_message_attachment_attachment"] = orphans("message_attachment_join", "attachment_id",
                                                            attachment_ids, "attachment" in prof)
    links["orphan_recoverable_chat"] = orphans("chat_recoverable_message_join", "chat_id", chat_ids, True)
    links["orphan_recoverable_message"] = orphans("chat_recoverable_message_join", "message_id", message_ids, True)
    in_chat = {v for v in ids("chat_message_join", "message_id") if v is not None}
    deleted_any = {v for v in ids("chat_recoverable_message_join", "message_id") if v is not None}
    links["messages_in_no_chat"] = sum(1 for m in message_ids if m not in in_chat)
    links["messages_in_no_chat_recently_deleted"] = (
        sum(1 for m in message_ids if m not in in_chat and m in deleted_any) if recoverable else None)
    per_msg = {}
    for _, r in rows.get("chat_message_join", []):
        if r.get("message_id") is not None and r.get("chat_id") is not None:
            per_msg.setdefault(r["message_id"], set()).add(r["chat_id"])
    links["messages_in_several_chats"] = sum(1 for s in per_msg.values() if len(s) > 1)
    per_chat = {c: set() for c in chat_ids}
    for _, r in rows.get("chat_handle_join", []):
        if r.get("chat_id") in per_chat and r.get("handle_id") is not None:
            per_chat[r["chat_id"]].add(r["handle_id"])
    chats["with_no_other_handle"] = sum(1 for s in per_chat.values() if not s)
    chats["with_one_other_handle"] = sum(1 for s in per_chat.values() if len(s) == 1)
    chats["with_several_other_handles"] = sum(1 for s in per_chat.values() if len(s) > 1)

    # ---- messages
    m = lambda c: has("message", c)
    assoc_cols = m("associated_message_type") or m("associated_message_guid")
    msgs = {
        "rows": 0,
        "with_association": zero_if(assoc_cols), "group_actions": zero_if(m("item_type")),
        "recently_deleted": zero_if(recoverable), "other_rows": 0,
        "from_me": 0, "not_from_me": 0, "direction_null": 0, "direction_unreadable": 0,
        "not_from_me_handle_zero": zero_if(m("handle_id")),
        "handle_unmatched": None, "other_handle_unmatched": None,
        "by_service": svc_counts() if m("service") else None,
        "item_kinds": {k: 0 for k in ("message", "participant_added", "own_number_changed",
                                      "participant_removed", "group_renamed", "participant_left",
                                      "group_photo_changed", "group_photo_removed", "background_changed",
                                      "background_removed", "legacy_shared_location", "audio_kept",
                                      "share_play", "unknown")} if m("item_type") else None,
        "associations": None,
        "replies": zero_if(m("thread_originator_guid")), "reply_originator_not_found": None,
        "reply_to_guid_set": zero_if(m("reply_to_guid")),
        "edit_labels": {k: 0 for k in ("not_recorded", "summary_unreadable", "unsent_parts_and_edit_history",
                                       "unsent_parts", "edit_history", "date_edited_no_body",
                                       "date_edited_only", "no_edit_recorded", "undetermined")},
        "ec_single_entry_parts": zero_if(m("message_summary_info")),
        "summaries": {k: 0 for k in ("parsed", "null", "unreadable", "oversized", "tmp_key_present")}
        if m("message_summary_info") else None,
        "scheduled": zero_if(m("schedule_type")),
        "filter_action": hist_new() if m("filter_action") else None,
        "payload_data_present": zero_if(m("payload_data")),
        "cache_has_attachments_set": zero_if(m("cache_has_attachments")),
        "date_retracted_set": zero_if(m("date_retracted")),
    }
    if assoc_cols:
        reactions = lambda: {k: 0 for k in ("love", "like", "dislike", "laugh", "emphasize", "question", "emoji", "sticker")}
        msgs["associations"] = {"sticker_legacy": 0, "tapback_added": reactions(), "tapback_removed": reactions(),
                                "poll_vote": 0, "no_association_type": 0, "unknown_type": 0,
                                "type_not_recorded": 0,
                                "targets": {k: 0 for k in ("part", "bubble", "bare", "unparsed", "null", "oversized", "unreadable")},
                                "target_not_found": None}
    bodies = {"text_by_attributed": {}, "decoded_ok": 0, "decoded_degraded": 0, "decode_failed": 0,
              "not_decoded": 0, "text_equals_decoded": 0, "text_differs_from_decoded": 0,
              "more_than_one_part": 0, "with_mention": 0, "with_link_key": 0, "with_audio_transcription": 0}
    failures = {"typedstream": {}, "summary_too_large": 0, "summary_plist_invalid": 0,
                "summary_plist_budget": 0, "summary_not_dictionary": 0, "summary_shape": 0,
                "summary_unknown_keys": 0, "date_ambiguous": 0, "date_negative": 0,
                "assoc_guid_unparsed": 0, "unknown_attribute_key": 0, "unknown_attribute_class": 0,
                "unreadable_attribute_value": 0, "orphan_join_row": 0, "attachment_ambiguous": 0,
                "attachment_invalid_entry": 0, "attachment_unusual_name": 0,
                "attachment_blob_missing": 0, "attachment_blob_unreadable": 0,
                "attachment_key_problem": 0, "aggregate_over_budget": 0}
    guids = set()
    reply_targets = []
    assoc_targets = []
    for key, r in rows.get("message", []):
        msgs["rows"] += 1
        v = {c: col("message", r, c) for c in [k[0] for k in sp.KNOWN["message"]] if c != "ROWID"}
        # partition
        typed = v["associated_message_type"][0] == "value" and v["associated_message_type"][1] != 0
        targeted = v["associated_message_guid"][0] not in ("absent", "null") and not (
            v["associated_message_guid"][0] == "value" and v["associated_message_guid"][1] == b"")
        has_assoc = typed or targeted
        if has_assoc and msgs["with_association"] is not None:
            msgs["with_association"] += 1
        elif msgs["group_actions"] is not None and v["item_type"][0] == "value" and v["item_type"][1] != 0:
            msgs["group_actions"] += 1
        elif recoverable and key in deleted:
            msgs["recently_deleted"] += 1
        else:
            msgs["other_rows"] += 1
        d = v["is_from_me"]
        if d[0] == "value" and d[1]:
            msgs["from_me"] += 1
        elif d[0] == "value":
            msgs["not_from_me"] += 1
            if v["handle_id"] == ("value", 0):
                inc(msgs, "not_from_me_handle_zero")
        elif d[0] == "null":
            msgs["direction_null"] += 1
        else:
            msgs["direction_unreadable"] += 1
        if msgs["by_service"] is not None:
            msgs["by_service"][service_class(v["service"])] += 1
        if msgs["item_kinds"] is not None:
            msgs["item_kinds"][item_kind(v["item_type"], v["group_action_type"], v["handle_id"], v["other_handle"])] += 1
        if has_assoc and msgs["associations"] is not None:
            a = msgs["associations"]
            t = v["associated_message_type"]
            if t[0] == "value":
                k = assoc_kind(t[1])
                if k.startswith("added:"):
                    a["tapback_added"][k[6:]] += 1
                elif k.startswith("removed:"):
                    a["tapback_removed"][k[8:]] += 1
                elif k == "unknown":
                    a["unknown_type"] += 1
                elif k == "no_association":
                    a["no_association_type"] += 1
                else:
                    a[k] += 1
            else:
                a["type_not_recorded"] += 1
            g = v["associated_message_guid"]
            if g[0] == "value" and g[1] != b"":
                form, tg = assoc_target(g[1])
                a["targets"][form] += 1
                if form == "unparsed":
                    failures["assoc_guid_unparsed"] += 1
                else:
                    assoc_targets.append(tg)
            elif g[0] in ("null", "value"):
                a["targets"]["null"] += 1
            elif g[0] == "oversized":
                a["targets"]["oversized"] += 1
            elif g[0] == "unreadable":
                a["targets"]["unreadable"] += 1
        tg = v["thread_originator_guid"]
        if tg[0] == "value" and tg[1] != b"":
            inc(msgs, "replies")
            reply_targets.append(tg[1])
        rt = v["reply_to_guid"]
        if rt[0] == "value" and rt[1] != b"":
            inc(msgs, "reply_to_guid_set")
        g = v["guid"]
        if g[0] == "value" and g[1] != b"":
            guids.add(g[1])
        # summary
        s = v["message_summary_info"]
        summ = (s[0], None)
        if s[0] == "value":
            exp = ann[r["message_summary_info"]].expect
            if exp[0] == "ok":
                info = exp[1]
                summ = ("value", info)
                msgs["summaries"]["parsed"] += 1
                if info["tmp"]:
                    msgs["summaries"]["tmp_key_present"] += 1
                if any(info[f] and info[f][0] == "bad" for f in ("ec", "rp", "otr")):
                    failures["summary_shape"] += 1
                failures["summary_unknown_keys"] += info["unknown"]
                if info["ec"] and info["ec"][0] == "value":
                    for _part, ds in info["ec"][1]:
                        if len(ds) == 1:
                            msgs["ec_single_entry_parts"] += 1
                        for dv in ds:
                            k = summary_time_interp(dv)
                            dates["edit_entry_date"]["null" if k is None else k] += 1
            else:
                issue = exp[1]
                issues.unreadable(issue)
                summ = ("unreadable", None)
                msgs["summaries"]["unreadable"] += 1
                failures[{"PlistInvalid": "summary_plist_invalid", "PlistOverBudget": "summary_plist_budget",
                          "PlistNotDictionary": "summary_not_dictionary"}[issue]] += 1
        elif s[0] == "null":
            msgs["summaries"]["null"] += 1
        elif s[0] == "oversized":
            msgs["summaries"]["oversized"] += 1
            failures["summary_too_large"] += 1
        elif s[0] == "unreadable":
            msgs["summaries"]["unreadable"] += 1
        sch = v["schedule_type"]
        if sch[0] == "value" and sch[1] != 0:
            inc(msgs, "scheduled")
        hist_add(msgs["filter_action"], v["filter_action"])
        if v["payload_data"][0] == "value":
            inc(msgs, "payload_data_present")
        if v["cache_has_attachments"] == ("value", True):
            inc(msgs, "cache_has_attachments_set")
        dr = v["date_retracted"]
        if dr[0] == "value" and dr[1][0] in ("seconds", "nanoseconds"):
            inc(msgs, "date_retracted_set")
        # body
        tx = v["text"]
        tstate = {"absent": "absent", "null": "null"}.get(tx[0], "withheld")
        if tx[0] == "value":
            tstate = "empty" if tx[1] == b"" else "non_empty"
        ab = v["attributedBody"]
        astate = {"absent": "absent", "null": "null", "value": "present"}.get(ab[0], "withheld")
        key2 = f"{tstate}/{astate}"
        bodies["text_by_attributed"][key2] = bodies["text_by_attributed"].get(key2, 0) + 1
        ab_outcome = "absent" if ab[0] == "absent" else ab[0]
        if ab[0] == "value":
            public = ann[r["attributedBody"]].public
            if "error" in public:
                bodies["decode_failed"] += 1
                failures["typedstream"][public["error"]] = failures["typedstream"].get(public["error"], 0) + 1
            else:
                o = public["ok"]
                if o["run_issue"]:
                    failures["typedstream"][o["run_issue"]["kind"]] = failures["typedstream"].get(o["run_issue"]["kind"], 0) + 1
                if o["run_issue"] or o["unreadable_attribute_values"]:
                    bodies["decoded_degraded"] += 1
                else:
                    bodies["decoded_ok"] += 1
                failures["unknown_attribute_key"] += o["unknown_attribute_keys"]
                failures["unknown_attribute_class"] += o["unknown_attribute_classes"]
                failures["unreadable_attribute_value"] += o["unreadable_attribute_values"]
                if tx[0] == "value":
                    if tx[1] == o["text"].encode("utf-8"):
                        bodies["text_equals_decoded"] += 1
                    else:
                        bodies["text_differs_from_decoded"] += 1
                if len(o["parts"]) > 1:
                    bodies["more_than_one_part"] += 1
                if any(p["mention"] is not None for p in o["parts"]):
                    bodies["with_mention"] += 1
                if any(ts.LINK in p["keys"] for p in o["parts"]):
                    bodies["with_link_key"] += 1
                if any("audio_transcription" in p["flags"] for p in o["parts"]):
                    bodies["with_audio_transcription"] += 1
        msgs["edit_labels"][edit_label(v["date_edited"], summ, tx,
                                       "null" if ab[0] == "null" else ab_outcome)] += 1
        # dates
        dd = v["date"]
        units_add(dates["message_date"], dd)
        if sch[0] == "value" and sch[1] != 0:
            inc(dates, "excluded_scheduled")
        else:
            ns = dd[1][1] if dd[0] == "value" else None
            if ns is None:
                dates["excluded_not_classified"] += 1
            elif ns < RANGE_FLOOR_NS:
                dates["excluded_before_floor"] += 1
            elif ns > checked_at_ns:
                dates["excluded_after_checked_at"] += 1
            else:
                dates["earliest_message"] = ns if dates["earliest_message"] is None else min(ns, dates["earliest_message"])
                dates["latest_message"] = ns if dates["latest_message"] is None else max(ns, dates["latest_message"])
    if msgs["replies"] is not None:
        msgs["reply_originator_not_found"] = sum(1 for t in reply_targets if t not in guids)
    if msgs["associations"] is not None:
        msgs["associations"]["target_not_found"] = sum(1 for t in assoc_targets if t not in guids)
    if m("handle_id") and "handle" in prof:
        msgs["handle_unmatched"] = sum(1 for _, r in rows.get("message", [])
                                       if sql_ne_zero(r.get("handle_id")) and sql_not_in(r.get("handle_id"), handle_ids))
    if m("other_handle"):
        msgs["other_handle_unmatched"] = sum(1 for _, r in rows.get("message", [])
                                             if sql_ne_zero(r.get("other_handle")) and sql_not_in(r.get("other_handle"), handle_ids))
    full_cross = {}
    for t in ("absent", "null", "empty", "non_empty", "withheld"):
        for a in ("absent", "null", "present", "withheld"):
            full_cross[f"{t}/{a}"] = bodies["text_by_attributed"].get(f"{t}/{a}", 0)
    bodies["text_by_attributed"] = full_cross
    msgs["bodies"] = bodies

    # ---- attachments
    for _, r in rows.get("message_attachment_join", []):
        read_all_cols_except("message_attachment_join", r, [], col, prof)
    att_present = "attachment" in prof
    a = lambda c: has("attachment", c)
    att = {"rows": 0 if att_present else None, "linked": None, "not_linked": None,
           "filename_null": zero_if(att_present and a("filename")),
           "filename_too_long": zero_if(att_present and a("filename")),
           "by_class": {k: 0 for k in ("sms_attachments", "sms_sticker_cache", "mac_messages",
                                       "other_home_relative", "absolute_var_mobile",
                                       "absolute_private_var_mobile", "unrecognized")},
           "lookup": {"found": 0, "not_listed_at_sourced_key": 0, "not_found_at_assumed_key": 0,
                      "ambiguous": 0, "invalid_entry": 0,
                      "not_attempted": {k: 0 for k in ("null", "too_long", "unusual_name", "class_not_looked_up",
                                                       "unrecognized", "column_absent", "unreadable")}},
           "probes": {k: 0 for k in ("present", "missing", "not_regular", "unreadable", "not_a_file",
                                     "invalid_entry", "no_key", "no_protection_class",
                                     "class_key_unavailable", "key_does_not_unwrap", "other", "not_probed")},
           "declared_total_bytes": zero_if(att_present and a("total_bytes")),
           "present_blob_bytes": 0,
           "stickers": zero_if(att_present and a("is_sticker")),
           "hidden": zero_if(att_present and a("hide_attachment")),
           "transfer_state": hist_new() if att_present and a("transfer_state") else None,
           "with_genmoji_description": zero_if(att_present and a("emoji_image_short_description"))}
    for _, r in rows.get("attachment", []):
        att["rows"] += 1
        vals = {c: col("attachment", r, c) for c in prof["attachment"] if sp.known("attachment", c) and c != "ROWID"}
        fn = vals.get("filename", ("absent", None))
        na = att["lookup"]["not_attempted"]
        if fn[0] == "absent":
            na["column_absent"] += 1
        elif fn[0] == "null":
            na["null"] += 1
            att["filename_null"] += 1
        elif fn[0] == "oversized":
            na["too_long"] += 1
            att["filename_too_long"] += 1
        elif fn[0] == "unreadable":
            na["unreadable"] += 1
        else:
            cls = path_class(fn[1])
            att["by_class"][cls] += 1
            if cls == "unrecognized":
                na["unrecognized"] += 1
            elif cls not in ("sms_attachments", "sms_sticker_cache"):
                na["class_not_looked_up"] += 1
            else:
                rel = candidate(fn[1])
                if rel is None:
                    na["unusual_name"] += 1
                    failures["attachment_unusual_name"] += 1
                elif rel in files_by_rel:
                    f = files_by_rel[rel]
                    att["lookup"]["found"] += 1
                    if f.directory:
                        att["probes"]["not_a_file"] += 1
                        failures["attachment_blob_unreadable"] += 1
                    elif f.state == "present":
                        att["probes"]["present"] += 1
                        att["present_blob_bytes"] += f.blob_len
                    elif f.state == "missing":
                        att["probes"]["missing"] += 1
                        failures["attachment_blob_missing"] += 1
                    else:
                        att["probes"][{"no_key": "no_key", "class_unavailable": "class_key_unavailable",
                                       "bad_key": "key_does_not_unwrap"}[f.state]] += 1
                        failures["attachment_key_problem"] += 1
                elif cls == "sms_attachments":
                    att["lookup"]["not_listed_at_sourced_key"] += 1
                else:
                    att["lookup"]["not_found_at_assumed_key"] += 1
        tb = vals.get("total_bytes", ("absent", None))
        if tb[0] == "value" and tb[1] >= 0:
            att["declared_total_bytes"] += tb[1]
        if vals.get("is_sticker") == ("value", True):
            att["stickers"] += 1
        if vals.get("hide_attachment") == ("value", True):
            att["hidden"] += 1
        hist_add(att["transfer_state"], vals.get("transfer_state", ("absent", None)))
        if vals.get("emoji_image_short_description", ("absent",))[0] == "value":
            att["with_genmoji_description"] += 1
    if att_present and "message_attachment_join" in prof:
        linked_ids = {x for x in ids("message_attachment_join", "attachment_id") if x is not None}
        att["linked"] = sum(1 for i in attachment_ids if i in linked_ids)
        att["not_linked"] = att["rows"] - att["linked"]

    rd_rows = lambda t: counts.get(t)
    recently = {"chat_recoverable_message_join": rd_rows("chat_recoverable_message_join"),
                "recoverable_message_part": rd_rows("recoverable_message_part"),
                "deleted_messages": rd_rows("deleted_messages"),
                "sync_deleted_messages": rd_rows("sync_deleted_messages"),
                "sync_deleted_chats": rd_rows("sync_deleted_chats"),
                "sync_deleted_attachments": rd_rows("sync_deleted_attachments"),
                "unsynced_removed_recoverable_messages": rd_rows("unsynced_removed_recoverable_messages")}
    failures["orphan_join_row"] = sum(x for x in (links[k] for k in links if k.startswith("orphan_")) if x)
    for h in (dates["message_date"], dates["chat_message_date"], dates["chat_last_read"],
              dates["recoverable_delete_date"], dates["edit_entry_date"]):
        if h:
            failures["date_ambiguous"] += h["ambiguous"]
            failures["date_negative"] += h["negative"]
    ts_all = {k: 0 for k in ("UnsupportedHeader", "Truncated", "UnexpectedTag", "BadReference",
                             "UnsupportedType", "TooDeep", "Budget", "MaterialisationBudget",
                             "TrailingBytes", "NotAttributedString", "InvalidUtf8", "RunLengthMismatch",
                             "RunOverflow", "RunSplitsSurrogate", "BadRunIndex")}
    ts_all.update(failures["typedstream"])
    failures["typedstream"] = ts_all
    failures["values"] = dict(issues.c, oversized_by_column=dict(sorted(issues.by_column.items())))
    return {"handles": handles, "chats": chats, "messages": msgs, "dates": dates,
            "attachments": att, "links": links, "recently_deleted": recently,
            "failures": failures}


def read_all_cols_except(t, row, cols, col, prof):
    """Read every present known column except `cols` (so issues are
    counted exactly once per value, as the Rust reader does)."""
    for c in prof.get(t) or []:
        k = sp.known(t, c)
        if k is None or k[1] == "rowid" or c in cols:
            continue
        col(t, row, c)


def qualifiers(schema, db, parts):
    q = []
    if schema["unknown_tables"]:
        q.append(f"unknown_tables:{schema['unknown_tables']}")
    uc = sum(t["unknown"] for t in schema["tables"])
    if uc:
        q.append(f"unknown_columns:{uc}")
    wc = sum(t["wrong_case"] for t in schema["tables"])
    if wc:
        q.append(f"wrong_case_columns:{wc}")
    if db["wal_outcome"].startswith("skipped"):
        q.append("wal_skipped")
    if db["wal_outcome"] == "not_listed" and db["header_says_wal"]:
        q.append("wal_state_unknown")
    if db["wal_frames_after_first_invalid"]:  # every fixture -wal has a valid header
        q.append("wal_frames_invalid")
    v = parts["failures"]["values"]
    if any(v[k] for k in ("value_oversized", "type_mismatch", "bool_out_of_range", "plist_invalid",
                          "plist_over_budget", "plist_not_dictionary")):
        q.append("values_withheld")
    if schema["encoding"] != "utf8":
        q.append("non_utf8_encoding")
    if db["size_matches"] is False or db["wal_size_matches"] is False:
        q.append("size_mismatch")
    return q


# --------------------------------------------------------------------------
# The backup
# --------------------------------------------------------------------------

def write_backup(spec, out_parent, rng, main, wal, expected_dir):
    """Write one backup (and the expected plaintexts). Returns facts the
    model needs: {rel: AttFile with blob_len}, staged sizes."""
    rb = rng.randbytes
    o = argparse.Namespace(**mb.defaults())
    out = os.path.join(out_parent, UDID)
    if os.path.exists(out_parent):
        shutil.rmtree(out_parent)
    os.makedirs(out)
    enc = spec.encrypted
    fixed = mb.FIXED_DATE
    info = {"Build Version": "00A000", "Device Name": mb.DEVICE_NAME, "Display Name": mb.DEVICE_NAME,
            "Last Backup Date": fixed, "Product Name": "Synthetic iPhone", "Product Type": mb.PRODUCT_TYPE,
            "Product Version": "18.0", "Unique Identifier": UDID}
    mb.write_plist(os.path.join(out, "Info.plist"), info, plistlib.FMT_XML)
    status = {"BackupState": "new", "Date": fixed, "IsFullBackup": True, "SnapshotState": "finished",
              "UUID": "00000000-0000-0000-0000-000000000000", "Version": "3.3"}
    mb.write_plist(os.path.join(out, "Status.plist"), status, plistlib.FMT_BINARY)
    keybag, class_keys = mb.build_keybag(o, rb, PASSWORD)

    entries = [
        ("HomeDomain", "", None, 0o40755, 0, "dir"),
        ("HomeDomain", "Library", None, 0o40755, 0, "dir"),
        ("HomeDomain", "Library/SMS", None, 0o40755, 0, "dir"),
        ("HomeDomain", "Library/SMS/sms.db", main, 0o100644, 3, "present"),
        ("MediaDomain", "", None, 0o40755, 0, "dir"),
    ]
    if spec.wal == "missing":
        entries.append(("HomeDomain", "Library/SMS/sms.db-wal", wal, 0o100644, 3, "missing"))
    elif spec.wal:
        entries.append(("HomeDomain", "Library/SMS/sms.db-wal", wal, 0o100644, 3, "present"))
    for f in spec.files:
        if f.directory:
            entries.append(("MediaDomain", f.rel, None, 0o40755, 0, "dir"))
        else:
            entries.append(("MediaDomain", f.rel, f.payload, 0o100644, 3, f.state))
    rows = []
    for inode, (domain, rel, payload, mode, prot, state) in enumerate(entries, start=3000):
        fid = mb.file_id(domain, rel)
        wrapped = None
        data = payload
        if state != "dir":
            if enc:
                fkey = rb(32)
                if state == "class_unavailable":
                    prot = 12
                    wrapped = aes_key_wrap(rb(32), fkey)
                elif state == "bad_key":
                    wrapped = aes_key_wrap(rb(32), fkey)
                elif state != "no_key":
                    wrapped = aes_key_wrap(class_keys[prot], fkey)
                data = mb.cbc_encrypt(fkey, payload)
            if state != "missing":
                d = os.path.join(out, fid[:2])
                os.makedirs(d, exist_ok=True)
                with open(os.path.join(d, fid), "wb") as fh:
                    fh.write(data)
            for f in spec.files:
                if f.rel == rel and domain == "MediaDomain":
                    f.blob_len = len(data)
        record = mb.mbfile(o, size=len(payload) if payload else 0, mode=mode, prot=prot,
                           wrapped_key=wrapped, inode=inode)
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
    lockdown = {"BuildVersion": "00A000", "DeviceName": mb.DEVICE_NAME, "ProductType": mb.PRODUCT_TYPE,
                "ProductVersion": "18.0"}
    manifest = {"Version": "10.0", "Date": fixed, "SystemDomainsVersion": "24.0", "WasPasscodeSet": False,
                "IsEncrypted": enc, "Lockdown": lockdown, "Applications": {}, "BackupKeyBag": keybag}
    if enc:
        manifest["ManifestKey"] = manifest_key
    mb.write_plist(os.path.join(out, "Manifest.plist"), manifest, plistlib.FMT_BINARY)

    os.makedirs(expected_dir, exist_ok=True)
    with open(os.path.join(expected_dir, "sms.db"), "wb") as f:
        f.write(main)
    if wal is not None:
        with open(os.path.join(expected_dir, "sms.db-wal"), "wb") as f:
            f.write(wal)
    if enc:
        with open(os.path.join(expected_dir, "Manifest.db"), "wb") as f:
            f.write(mdb_plain)
        for f in spec.files:
            if not f.directory and f.state == "present":
                d = os.path.join(expected_dir, "attachments")
                os.makedirs(d, exist_ok=True)
                with open(os.path.join(d, mb.file_id("MediaDomain", f.rel)), "wb") as fh:
                    fh.write(f.payload)


def build_entry(spec, out_dir, seed):
    rng = random.Random(f"{seed}:{spec.name}")
    with tempfile.TemporaryDirectory() as work:
        main, wal = build_db(spec, work, rng)
    exp = os.path.join(out_dir, spec.name + ".expected")
    if os.path.exists(exp):
        shutil.rmtree(exp)
    write_backup(spec, os.path.join(out_dir, spec.name), rng, main, wal, exp)

    header_wal = main[18] == 2 and main[19] == 2
    wf = wal_facts(wal, PAGE_SIZE) if wal is not None and spec.wal != "missing" else None
    staged_wal = wal is not None and spec.wal != "missing"
    db = {"wal_listed": spec.wal is not None,
          "wal_outcome": "not_listed" if spec.wal is None else ("skipped:blob_missing" if spec.wal == "missing" else "staged"),
          "wal_bytes": len(wal) if staged_wal else None,
          "header_says_wal": header_wal,
          "wal_valid_committed_frames": wf["valid_committed_frames"] if wf else None,
          "wal_frames_after_first_invalid": wf["frames_after_first_invalid"] if wf else None,
          "integrity": "ok",
          "staged_bytes": len(main) + (len(wal) if staged_wal else 0),
          "size_matches": True,
          "wal_size_matches": True if staged_wal else None}
    scan = scan_counts(main)
    empty_schema = {"tables": [], "unknown_tables": 0, "views": 0, "triggers": 0, "indexes": 0,
                    "stat_tables": 0, "internal_objects": 0, "encoding": None, "client_version": None,
                    "client_version_parsed": False, "features": []}
    groups = {k: None for k in ("handles", "chats", "messages", "dates", "attachments", "links",
                                "recently_deleted", "failures")}
    if spec.expect_status == "damaged":
        report = dict(status={"kind": "damaged", "reason": "integrity_problems"}, schema=empty_schema,
                      database=dict(db, integrity="problems"), **groups)
    elif scan["virtual"]:
        schema = dict(empty_schema, unknown_tables=scan["unknown_tables"], views=scan["views"],
                      triggers=scan["triggers"], indexes=scan["indexes"], stat_tables=scan["stat_tables"],
                      internal_objects=scan["internal_objects"], encoding=scan["encoding"])
        report = dict(status={"kind": "unsupported", "reasons": ["virtual_table_present"]}, schema=schema,
                      database=dict(db, integrity=None), **groups)
    else:
        rows, counts = read_back(spec, main, wal if staged_wal else None)
        if spec.wal == "badframes":
            # SQLite must stop at the first frame that fails validation: the
            # second transaction's message is not visible.
            want = len(spec.rows["message"]) + sum(1 for t, _ in spec.wal_rows if t == "message")
            assert counts["message"] == want, (counts["message"], want)
        schema, reasons = schema_model(spec, rows, counts, scan)
        if reasons:
            report = dict(status={"kind": "unsupported", "reasons": reasons}, schema=schema,
                          database=db, **groups)
        else:
            files_by_rel = {f.rel: f for f in spec.files}
            # Every fixture date is before 2027; the check runs later.
            parts = model(spec, rows, counts, annotations(spec), files_by_rel, CHECKED_AT_NS)
            report = dict(status={"kind": "readable", "qualifiers": qualifiers(schema, db, parts)},
                          schema=schema, database=db, **parts)
    with open(os.path.join(exp, "report.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(report, f, indent=1, sort_keys=True, ensure_ascii=True)
        f.write("\n")
    size = sum(os.path.getsize(os.path.join(dp, n)) for d in (os.path.join(out_dir, spec.name), exp)
               for dp, _, ns in os.walk(d) for n in ns)
    assert size < 300_000, (spec.name, size)
    return size


# The model compares dates with the moment check() runs; fixture dates are
# all before 2027-01-01, so any run after that date sees the same result.
CHECKED_AT_NS = 1_798_761_600 * NS  # 2027-01-01T00:00:00Z


# --------------------------------------------------------------------------
# The catalogue
# --------------------------------------------------------------------------

def handle_rows():
    return [
        {"ROWID": 1, "id": "synthetic-handle-1@example.invalid", "service": "iMessage", "country": "us",
         "uncanonicalized_id": None, "person_centric_id": None},
        {"ROWID": 2, "id": "+15550000001", "service": "SMS", "country": "us",
         "uncanonicalized_id": "+1555SYNTH0001", "person_centric_id": None},
        {"ROWID": 3, "id": "+15550000002", "service": "RCS", "country": "us",
         "uncanonicalized_id": None, "person_centric_id": "SYNTH-PERSON-1"},
        {"ROWID": 4, "id": "synthetic-handle-4@example.invalid", "service": "iMessageLite",
         "country": None, "uncanonicalized_id": None, "person_centric_id": None},
        {"ROWID": 5, "id": "synthetic-handle-5@example.invalid", "service": "SYNTH-SERVICE",
         "country": None, "uncanonicalized_id": None, "person_centric_id": None},
    ]


def chat_rows():
    props = plistlib.dumps({"SYNTH-KEY": "SYNTH-VALUE"}, fmt=plistlib.FMT_BINARY)
    return [
        {"ROWID": 1, "guid": "iMessage;-;synthetic-handle-1@example.invalid", "style": 45,
         "chat_identifier": "synthetic-handle-1@example.invalid", "service_name": "iMessage",
         "room_name": None, "display_name": "", "group_id": "SYNTH-GROUP-1", "properties": props,
         "is_archived": 0, "is_filtered": 0, "is_blackholed": 0, "is_pending_review": 0, "is_recovered": 0,
         "last_read_message_timestamp": T0_NS + 500 * NS},
        {"ROWID": 2, "guid": "iMessage;+;chatSYNTH0002", "style": 43, "chat_identifier": "chatSYNTH0002",
         "service_name": "iMessage", "room_name": "chatSYNTH0002", "display_name": "SYNTH-CHAT-NAME",
         "group_id": "SYNTH-GROUP-2", "properties": None, "is_archived": 1, "is_filtered": 1,
         "is_blackholed": 0, "is_pending_review": 0, "is_recovered": 0, "last_read_message_timestamp": 0},
        {"ROWID": 3, "guid": "SMS;-;+15550000001", "style": 45, "chat_identifier": "+15550000001",
         "service_name": "SMS", "room_name": None, "display_name": None, "group_id": "SYNTH-GROUP-3",
         "properties": None, "is_archived": 0, "is_filtered": 2, "is_blackholed": 0,
         "is_pending_review": 1, "is_recovered": 1, "last_read_message_timestamp": T0_S},
    ]


def msg(rowid, **kw):
    r = {"ROWID": rowid, "guid": f"SYNTH-MSG-GUID-{rowid:04d}", "date": T0_NS + rowid * 60 * NS,
         "is_from_me": 0, "handle_id": 1, "service": "iMessage", "item_type": 0, "group_action_type": 0,
         "other_handle": 0, "share_direction": 0, "share_status": 0, "is_read": 1, "is_delivered": 1,
         "is_sent": 0, "is_audio_message": 0, "cache_has_attachments": 0, "date_read": 0,
         "date_delivered": 0, "date_played": 0, "error": 0, "was_downgraded": 0, "date_edited": 0,
         "date_retracted": 0, "part_count": 1, "schedule_type": 0, "schedule_state": 0,
         "filter_action": 0, "filter_sub_action": 0, "associated_message_type": 0,
         "associated_message_guid": None, "text": f"SYNTH-BODY-{rowid}", "attributedBody": None,
         "message_summary_info": None, "thread_originator_guid": None, "reply_to_guid": None}
    r.update(kw)
    return r


def guid(n):
    return f"SYNTH-MSG-GUID-{n:04d}"


def full_messages():
    edit_ec = {0: [{"d": T0_S + 10, "t": b"SYNTH-EDIT-0"}, {"d": float(T0_S) + 20.5, "t": b"SYNTH-EDIT-1"}]}
    return [
        msg(1, attributedBody=ab_plain("SYNTH-BODY-1")),
        msg(2, text="", attributedBody=ab_plain("SYNTH-BODY-2")),
        msg(3, text=None, cache_has_attachments=1, part_count=2,
            attributedBody=ab_text([("\ufffc", [(ts.P, 0), (ts.GUID, "SYNTH-ATT-GUID-1")]),
                                    ("SYNTH-BODY-3", [(ts.P, 1)])])),
        msg(4, is_from_me=1, handle_id=1, text="SYNTH-BODY-4"),
        msg(5, associated_message_type=2000, associated_message_guid=f"p:0/{guid(1)}",
            associated_message_range_location=0, associated_message_range_length=12,
            text="Loved \u201cSYNTH-BODY-1\u201d"),
        msg(6, associated_message_type=3001, associated_message_guid=f"bp:{guid(1)}", text=None),
        msg(7, associated_message_type=2006, associated_message_guid=f"p:0/{guid(2)}",
            associated_message_emoji="\U0001F525", text=None),
        msg(8, associated_message_type=1000, associated_message_guid="SYNTH-MISSING-GUID-1", text=None),
        msg(9, associated_message_type=4000, associated_message_guid="p:x/SYNTH", text=None),
        msg(10, associated_message_type=2, associated_message_guid=None),
        msg(11, thread_originator_guid=guid(1), thread_originator_part="0:0:12", reply_to_guid=guid(1)),
        msg(12, thread_originator_guid="SYNTH-MISSING-GUID-2", thread_originator_part="0:0:1"),
        msg(13, reply_to_guid=guid(4)),
        msg(14, item_type=1, group_action_type=0, handle_id=2, other_handle=3, text=None),
        msg(15, item_type=1, group_action_type=0, handle_id=2, other_handle=2, text=None),
        msg(16, item_type=2, group_title="SYNTH-GROUP-TITLE", text=None),
        msg(17, item_type=3, group_action_type=1, text=None),
        msg(18, item_type=4, other_handle=1, share_direction=0, share_status=1, text=None),
        msg(19, date_edited=T0_NS + 900 * NS, message_summary_info=summary(otr=1, ec=edit_ec)),
        msg(20, date_edited=T0_NS + 901 * NS, text=None, message_summary_info=summary(otr=1, rp=[0])),
        msg(21, date_edited=T0_NS + 902 * NS, text="", attributedBody=None),
        msg(22, message_summary_info=summary(tmp=True, unknown={"ust": True})),
        msg(23, schedule_type=2, schedule_state=1, date=T0_NS + 10 ** 9 * 86400 * 30),
        msg(24, text="Hey SYNTH-MENTION see link",
            attributedBody=ab_text([("Hey ", [(ts.P, 0)]),
                                    ("SYNTH-MENTION", [(ts.P, 0), (ts.MENTION, "synthetic-handle-1@example.invalid")]),
                                    (" see ", [(ts.P, 0)]),
                                    ("link", [(ts.P, 0), (ts.LINK, ts.Unknown("SYNTHLinkClass"))])])),
        msg(25, date=100_000_000, text="SYNTH-BODY-OLD"),
        msg(26, date=0),
        msg(27, text="SYNTH-BODY-RECOVERABLE"),
        msg(28, text="SYNTH-BODY-28", is_audio_message=1,
            attributedBody=ab_text([("SYNTH-BODY-28", [(ts.P, 0), ("IMAudioTranscription", 1)])])),
        msg(29, payload_data=b"\x00SYNTH-PAYLOAD", balloon_bundle_id="com.example.synthetic",
            expressive_send_style_id="com.example.synthetic.effect", filter_action=1, filter_sub_action=2),
        msg(30, is_from_me=0, handle_id=0, service="SMS", subject="SYNTH-SUBJECT"),
        msg(31, handle_id=99, other_handle=98, service="RCS", is_from_me=1, date_retracted=T0_NS),
        msg(32, service="", text=None, attributedBody=None, date=T0_S + 5),
        # attributedBody only, with a mention after non-BMP characters (for
        # oracle 3's placement check).
        msg(33, text=None, attributedBody=ab_text([
            ("\U0001F600 ", [(ts.P, 0)]),
            ("SYNTH-MENTION", [(ts.P, 0), (ts.MENTION, "synthetic-handle-1@example.invalid")]),
            (" \U0001F44B\U0001F3FD SYNTH-BODY-33", [(ts.P, 0)])])),
    ]


def full_rows():
    msgs = full_messages()
    cmj = []
    for m in msgs:
        if m["ROWID"] in (27,):
            continue  # in no chat, linked from Recently Deleted
        if m["ROWID"] == 26:
            continue  # in no chat, not deleted
        chat = 3 if m.get("service") == "SMS" else (2 if 14 <= m["ROWID"] <= 18 else 1)
        cmj.append({"chat_id": chat, "message_id": m["ROWID"], "message_date": m["date"]})
    cmj.append({"chat_id": 2, "message_id": 1, "message_date": msgs[0]["date"]})  # one message in two chats
    cmj.append({"chat_id": 77, "message_id": 4, "message_date": 0})               # orphan chat side
    cmj.append({"chat_id": 1, "message_id": 555, "message_date": 0})              # orphan message side
    return {
        "handle": handle_rows(),
        "chat": chat_rows(),
        "message": msgs,
        "chat_handle_join": [{"chat_id": 1, "handle_id": 1}, {"chat_id": 2, "handle_id": 1},
                             {"chat_id": 2, "handle_id": 2}, {"chat_id": 2, "handle_id": 3},
                             {"chat_id": 3, "handle_id": 2}, {"chat_id": 3, "handle_id": 2},
                             {"chat_id": 9, "handle_id": 1}, {"chat_id": 1, "handle_id": 44}],
        "chat_message_join": cmj,
        "message_attachment_join": [{"message_id": 3, "attachment_id": 1},
                                    {"message_id": 3, "attachment_id": 2},
                                    {"message_id": 28, "attachment_id": 3},
                                    {"message_id": 4, "attachment_id": 66}],
        "attachment": [
            {"ROWID": 1, "guid": "SYNTH-ATT-GUID-1",
             "filename": "~/Library/SMS/Attachments/aa/01/SYNTH-0001/SYNTH-FILE-0001.heic",
             "uti": "public.heic", "mime_type": "image/heic", "transfer_name": "SYNTH-FILE-0001.heic",
             "total_bytes": 40, "is_sticker": 0, "hide_attachment": 0, "is_outgoing": 0,
             "transfer_state": 5, "created_date": T0_S, "start_date": 0,
             "emoji_image_short_description": None, "original_guid": "SYNTH-ATT-GUID-1"},
            {"ROWID": 2, "guid": "SYNTH-ATT-GUID-2",
             "filename": "~/Library/SMS/StickerCache/00/SYNTH-0002/SYNTH-FILE-0002.heic",
             "uti": "public.heic", "mime_type": "image/heic", "transfer_name": "SYNTH-FILE-0002.heic",
             "total_bytes": 30, "is_sticker": 1, "hide_attachment": 0, "is_outgoing": 0,
             "transfer_state": 5, "created_date": T0_S, "start_date": 0,
             "emoji_image_short_description": "SYNTH-GENMOJI-PROMPT", "original_guid": "SYNTH-ATT-GUID-2"},
            {"ROWID": 3, "guid": "SYNTH-ATT-GUID-3",
             "filename": "~/Library/SMS/Attachments/aa/03/SYNTH-0003/SYNTH-FILE-0003.caf",
             "uti": "com.apple.coreaudio-format", "mime_type": None, "transfer_name": "SYNTH-FILE-0003.caf",
             "total_bytes": 20, "is_sticker": 0, "hide_attachment": 1, "is_outgoing": 1,
             "transfer_state": 0, "created_date": T0_S, "start_date": 0,
             "emoji_image_short_description": None, "original_guid": None},
            {"ROWID": 4, "guid": "SYNTH-ATT-GUID-4", "filename": None, "uti": None, "mime_type": None,
             "transfer_name": None, "total_bytes": 0, "is_sticker": 0, "hide_attachment": 0,
             "is_outgoing": 0, "transfer_state": -1, "created_date": 0, "start_date": 0,
             "emoji_image_short_description": None, "original_guid": None},
            {"ROWID": 5, "guid": "SYNTH-ATT-GUID-5",
             "filename": "~/Library/Messages/Attachments/SYNTH-FILE-0005.heic", "uti": None,
             "mime_type": None, "transfer_name": None, "total_bytes": -5, "is_sticker": 0,
             "hide_attachment": 0, "is_outgoing": 0, "transfer_state": 5, "created_date": T0_S,
             "start_date": 0, "emoji_image_short_description": None, "original_guid": None},
        ],
        "chat_recoverable_message_join": [{"chat_id": 1, "message_id": 27, "delete_date": T0_NS + 7000 * NS,
                                           "ck_sync_state": 0}],
        "recoverable_message_part": [{"chat_id": 1, "message_id": 27, "part_index": 0,
                                      "delete_date": T0_NS + 7000 * NS, "part_text": b"SYNTH-DELETED-0"}],
        "deleted_messages": [{"guid": "SYNTH-DELETED-GUID-1"}],
        "_SqliteDatabaseProperties": [{"key": "_ClientVersion", "value": "17001"},
                                      {"key": "SYNTH-PROPERTY", "value": "SYNTH"}],
    }


def full_files():
    return [AttFile("Library/SMS/Attachments/aa/01/SYNTH-0001/SYNTH-FILE-0001.heic", b"synthetic heic bytes 0001\n"),
            AttFile("Library/SMS/StickerCache/00/SYNTH-0002/SYNTH-FILE-0002.heic", b"synthetic sticker bytes\n")]


def spec_full(name, encrypted):
    s = Spec(name, sp.full(), full_rows(), encrypted)
    s.files = full_files()
    return s


def small_rows(date=lambda i: T0_NS + i * 60 * NS, extra_messages=()):
    msgs = [msg(i, date=date(i)) for i in range(1, 6)] + list(extra_messages)
    return {
        "handle": handle_rows()[:2],
        "chat": chat_rows()[:2],
        "message": msgs,
        "chat_handle_join": [{"chat_id": 1, "handle_id": 1}, {"chat_id": 2, "handle_id": 2}],
        "chat_message_join": [{"chat_id": 1, "message_id": m["ROWID"], "message_date": m["date"]} for m in msgs],
        "message_attachment_join": [],
        "attachment": [],
        "_SqliteDatabaseProperties": [{"key": "_ClientVersion", "value": "9005"}],
    }


def spec_profile(name, profile, date=lambda i: T0_NS + i * 60 * NS, extra=()):
    rows = small_rows(date, extra)
    rows["message"][1]["date_edited"] = T0_NS + 99 * NS
    rows["message"][2]["thread_originator_guid"] = guid(1)
    rows["message"][3]["associated_message_type"] = 2001
    rows["message"][3]["associated_message_guid"] = f"p:0/{guid(1)}"
    rows["message"][4]["filter_action"] = 3
    rows["message"][4]["schedule_type"] = 0
    rows["message"][4]["message_summary_info"] = summary(otr=1, ec={0: [{"d": T0_S}]})
    if "chat_recoverable_message_join" in profile:
        rows["chat_recoverable_message_join"] = [{"chat_id": 1, "message_id": 5, "delete_date": T0_NS,
                                                  "ck_sync_state": 1}]
        rows["recoverable_message_part"] = [{"chat_id": 1, "message_id": 5, "part_index": 0,
                                             "part_text": b"SYNTH-DELETED-1"}]
    return Spec(name, profile, rows)


def spec_minimal():
    rows = {
        "handle": [{"ROWID": 1, "id": "synthetic-handle-1@example.invalid"}],
        "chat": [{"ROWID": 1, "guid": "SYNTH-CHAT-GUID-1"}],
        "message": [{"ROWID": i, "guid": guid(i), "date": T0_NS + i * NS, "is_from_me": i % 2}
                    for i in range(1, 4)],
        "chat_handle_join": [{"chat_id": 1, "handle_id": 1}],
        "chat_message_join": [{"chat_id": 1, "message_id": i} for i in range(1, 4)],
    }
    return Spec("msgs-minimal-plain", sp.minimal(), rows)


def spec_scrambled():
    p = sp.cols_threads_edits()
    p["handle"] = p["handle"] + ["synth_extra_a"]
    p["chat"] = p["chat"] + ["synth_extra_b"]
    s = spec_profile("msgs-scrambled-plain", p)
    s.scramble = True
    return s


def spec_resume():
    msgs = [msg(i) for i in (1, 2, 3, 10, 11, 50, 51, 52, 1000)]
    msgs.append(msg(I64_MIN, guid="SYNTH-MSG-GUID-MIN", date=T0_NS))
    msgs.append(msg(I64_MAX, guid="SYNTH-MSG-GUID-MAX", date=T0_NS))
    cmj = [{"rowid": I64_MIN, "chat_id": 1, "message_id": I64_MIN, "message_date": 0}]
    cmj += [{"rowid": 3 * i + 5, "chat_id": 1, "message_id": m["ROWID"], "message_date": 0}
            for i, m in enumerate(msgs[:-1])]
    rows = {
        "handle": [dict(h, ROWID=r) for h, r in zip(handle_rows(), (1, 7, 8, 300, I64_MIN))],
        "chat": chat_rows()[:1],
        "message": msgs,
        "chat_handle_join": [{"rowid": I64_MIN, "chat_id": 1, "handle_id": 1},
                             {"rowid": 2, "chat_id": 1, "handle_id": 7},
                             {"rowid": I64_MAX, "chat_id": 1, "handle_id": 8}],
        "chat_message_join": cmj,
        "message_attachment_join": [],
        "attachment": [],
    }
    s = Spec("msgs-resume-plain", sp.cols_threads_edits(), rows)
    # Rows deleted after insertion leave gaps in the keys.
    s.extra_sql = []
    return s


def spec_attachments_enc():
    rows = small_rows()
    names = [
        ("~/Library/SMS/Attachments/aa/01/SYNTH-0001/SYNTH-FILE-0001.heic", "present"),
        ("~/Library/SMS/Attachments/aa/02/SYNTH-0002/SYNTH-FILE-0002.heic", "missing"),
        ("~/Library/SMS/Attachments/aa/03/SYNTH-0003/SYNTH-FILE-0003.heic", None),
        ("~/Library/SMS/StickerCache/00/SYNTH-0004/SYNTH-FILE-0004.heic", None),
        ("~/Library/SMS/StickerCache/00/SYNTH-0005/SYNTH-FILE-0005.heic", "present"),
        ("~/Library/SMS/Attachments/aa/06/SYNTH-0006/SYNTH-FILE-0006.heic", "no_key"),
        ("~/Library/SMS/Attachments/aa/07/SYNTH-0007/SYNTH-FILE-0007.heic", "class_unavailable"),
        ("~/Library/SMS/Attachments/aa/08/SYNTH-0008/SYNTH-FILE-0008.heic", "bad_key"),
        (None, None),
        ("~/Library/Messages/Attachments/SYNTH-FILE-0010.heic", None),
        ("/var/mobile/Library/SMS/Attachments/SYNTH-FILE-0011.heic", None),
        ("/private/var/mobile/Library/SMS/Attachments/SYNTH-FILE-0012.heic", None),
        ("~/Documents/SYNTH-FILE-0013.heic", None),
        ("SYNTH-FILE-0014.heic", None),
        ("", None),
        ("~/Library/SMS/Attachments/../../SYNTH-FILE-0016.heic", None),
        ("~/Library/SMS/Attachments/aa/17/SYNTH-0017/SYNTH-FILE-0017", "dir"),
        ("~/Library/SMS/Attachments/aa/18/" + "SYNTH-LONG/" * 380 + "x.heic", None),
    ]
    atts = []
    files = []
    for i, (fn, state) in enumerate(names, start=1):
        atts.append({"ROWID": i, "guid": f"SYNTH-ATT-GUID-{i}", "filename": fn, "uti": "public.heic",
                     "mime_type": "image/heic", "transfer_name": f"SYNTH-FILE-{i:04d}.heic",
                     "total_bytes": 26, "is_sticker": int("StickerCache" in (fn or "")),
                     "hide_attachment": 0, "is_outgoing": i % 2, "transfer_state": 5,
                     "created_date": T0_S, "start_date": 0, "original_guid": None})
        if state == "dir":
            files.append(AttFile(fn[2:], directory=True))
        elif state is not None:
            files.append(AttFile(fn[2:], f"synthetic attachment {i:04d}\n".encode(), state))
    rows["attachment"] = atts
    rows["message_attachment_join"] = [{"message_id": 1 + i % 5, "attachment_id": i}
                                       for i in range(1, len(atts) + 1)]
    s = Spec("msgs-attachments-enc", sp.cols_a_seconds(), rows, encrypted=True)
    s.files = files
    return s


def spec_wal(name, mode, encrypted=False):
    rows = small_rows()
    s = Spec(name, sp.cols_threads_edits(), rows, encrypted)
    s.wal = mode
    s.wal_rows = [("message", msg(100, text="SYNTH-BODY-WAL-100")),
                  ("message", msg(101, text="SYNTH-BODY-WAL-101")),
                  ("chat_message_join", {"chat_id": 1, "message_id": 100, "message_date": 0})]
    if mode == "badframes":
        s.wal_rows2 = [("message", msg(200, text="SYNTH-BODY-WAL-200", attributedBody=ab_plain("x" * 900))),
                       ("chat_message_join", {"chat_id": 1, "message_id": 200, "message_date": 0})]
    return s


def spec_hostile():
    p = sp.full()
    p["message"].append("SYNTH_SENTINEL_COLUMN")
    rows = full_rows()
    rows["message"] = [
        msg(1, attributedBody=ab_plain("SYNTH-BODY-1")),
        msg(2, guid=12345, date="SYNTH-NOT-A-DATE", is_from_me=2, handle_id="SYNTH-TEXT-IN-INT"),
        msg(3, text=TextBytes(b"SYNTH-BODY-\xff\xfe-invalid"), service=b"iMessage", is_read=-1),
        msg(4, guid="G" * 300, associated_message_guid="p:0/" + "G" * 300, associated_message_type=2000),
        msg(5, date=3.0e9, is_from_me=1.0, error=2.5),
        msg(28, error=4.0, is_from_me=1),
        # A binary property list cut short (its trailer is gone). A bare
        # word would not do: the reader also accepts the ASCII format, where
        # it is a string.
        msg(6, message_summary_info=summary_raw(
            plistlib.dumps({"rp": [1, 2, 3]}, fmt=plistlib.FMT_BINARY)[:-12], "PlistInvalid")),
        msg(7, message_summary_info=summary_raw(plistlib.dumps({"ec": nested(40)}, fmt=plistlib.FMT_BINARY),
                                                "PlistOverBudget")),
        msg(8, message_summary_info=summary_raw(plistlib.dumps([1, 2], fmt=plistlib.FMT_BINARY),
                                                "PlistNotDictionary")),
        msg(9, message_summary_info=summary_shape(
            {"ec": {"x": []}, "rp": [1], "otr": {"0": {}}},
            {"otr": ("value", 1), "ec": ("bad", None), "rp": ("value", [1]), "tmp": False, "unknown": 0})),
        msg(10, attributedBody=ab_vector("h_truncated")),
        msg(11, attributedBody=ab_vector("h_trailing")),
        msg(12, attributedBody=ab_vector("h_bad_version")),
        msg(13, attributedBody=ab_vector("h_invalid_utf8")),
        msg(14, attributedBody=ab_vector("h_dangling_ref")),
        msg(15, attributedBody=ab_vector("h_depth65_objects")),
        msg(16, attributedBody=ab_vector("h_array_4i")),
        msg(17, attributedBody=ab_vector("h_runlen_short"), text="SYNTH-BODY-17"),
        msg(18, attributedBody=ab_vector("h_bad_run_index")),
        msg(19, attributedBody=ab_vector("h_attr_invalid_utf8")),
        msg(20, attributedBody=ab_vector("v8_formatting")),
        msg(21, attributedBody=ab_vector("v4_mention_reuse")),
        msg(22, attributedBody=ab_vector("v9_multipart"), text=None),
        msg(23, attributedBody=12, text=7),
        msg(24, associated_message_guid="", associated_message_type=0, subject="S" * (64 * 1024 + 1)),
        msg(25, date=-5, date_edited=-1, date_delivered=SECONDS_MAX),
        msg(26, date=NS_MAX, item_type=9, group_action_type=None, payload_data="SYNTH-TEXT-PAYLOAD"),
        msg(27, date=T0_NS, item_type=1, group_action_type=None, associated_message_guid="bp:",
            associated_message_type=None),
    ]
    rows["chat"][0]["properties"] = "SYNTH-TEXT-PROPERTIES"
    rows["chat"][1]["properties"] = 5
    rows["chat"][2]["style"] = 3.0
    rows["attachment"] = [
        {"ROWID": i, "guid": f"SYNTH-ATT-GUID-{i}", "filename": fn}
        for i, fn in enumerate([
            "~/Library/SMS/Attachments/aa/01/../../../SYNTH-FILE-ESCAPE",
            "~/Library/SMS/Attachments/aa\\01\\SYNTH-FILE-BACKSLASH",
            "~/Library/SMS/Attachments/C:\\SYNTH-FILE-DRIVE",
            TextBytes(b"~/Library/SMS/Attachments/aa/01/SYNTH\x00NUL"),
            "~/Library/SMS/Attachments/aa/01/SYNTH-FILE-\u202egpj.exe",
            "~/Library/SMS/Attachments/aa/01/SYNTH-FILE-TRAILING.",
            "~/Library/SMS/Attachments/aa/01/CON",
            "~/Library/SMS/Attachments/",
            "~/Library/SMS/Attachments/aa/01/" + "x" * 4097,
            b"~/Library/SMS/Attachments/aa/01/SYNTH-BLOB-NAME",
            42,
        ], start=1)
    ]
    rows["message_attachment_join"] = [{"message_id": 1, "attachment_id": i} for i in range(1, 12)]
    s = Spec("msgs-hostile-plain", p, rows)
    # No declared type (no affinity), so integers and reals stay as stored.
    s.decl = {("message", "guid"): "", ("message", "text"): "", ("message", "error"): ""}
    s.files = []
    s.extra_sql = [
        'CREATE TABLE "SYNTH_SENTINEL_TABLE" ("SYNTH_SENTINEL_TABLE_COLUMN" TEXT)',
        'CREATE VIEW "SYNTH_SENTINEL_VIEW" AS SELECT "ROWID" FROM "message"',
        'CREATE VIEW "SYNTH_SENTINEL_VIEW_2" AS SELECT "id" FROM "handle"',
        'CREATE TRIGGER "SYNTH_SENTINEL_TRIGGER" AFTER DELETE ON "SYNTH_SENTINEL_TABLE" BEGIN '
        'SELECT 1; END',
        'CREATE INDEX "SYNTH_SENTINEL_INDEX" ON "SYNTH_SENTINEL_TABLE" ("SYNTH_SENTINEL_TABLE_COLUMN" '
        'COLLATE NOCASE)',
    ]
    return s


def spec_corrupt():
    s = Spec("msgs-corrupt-plain", sp.minimal(), spec_minimal().rows)
    s.profile = dict(s.profile, SYNTH_SENTINEL_CORRUPT_TABLE=["SYNTH_SENTINEL_CORRUPT_COLUMN"])
    s.rows = dict(s.rows, SYNTH_SENTINEL_CORRUPT_TABLE=[
        {"SYNTH_SENTINEL_CORRUPT_COLUMN": f"SYNTH-CORRUPT-{i:04d}-" + "y" * 40} for i in range(200)])
    s.corrupt = True
    s.expect_status = "damaged"
    return s


def spec_unsupported():
    p = sp.minimal()
    del p["message"]
    p["message_store"] = ["ROWID", "guid", "date", "is_from_me"]
    rows = spec_minimal().rows
    rows["message_store"] = rows.pop("message")
    s = Spec("msgs-unsupported-plain", p, rows)
    s.extra_sql = ['CREATE VIEW "message" AS SELECT "ROWID", "guid", "date", "is_from_me" FROM "message_store"']
    return s


def spec_vtab():
    s = Spec("msgs-vtab-plain", sp.minimal(), spec_minimal().rows)
    s.extra_sql = ['CREATE VIRTUAL TABLE "SYNTH_SENTINEL_FTS" USING fts5(body)']
    return s


def spec_rowid_desc():
    s = Spec("msgs-rowid-desc-plain", sp.minimal(), spec_minimal().rows)
    s.rowid_desc = True
    return s


def catalogue():
    mixed = lambda i: T0_NS + i * NS if i % 2 else T0_S + i
    return [
        spec_full("msgs-full-plain", False),
        spec_full("msgs-full-enc", True),
        spec_profile("msgs-cols-a-seconds-plain", sp.cols_a_seconds(), date=lambda i: T0_S + i * 60),
        spec_profile("msgs-cols-threads-edits-plain", sp.cols_threads_edits()),
        spec_profile("msgs-cols-emoji-schedule-plain", sp.cols_emoji_schedule()),
        spec_profile("msgs-cols-filter-plain", sp.cols_filter()),
        spec_profile("msgs-mixed-units-plain", sp.cols_threads_edits(), date=mixed),
        spec_scrambled(),
        spec_minimal(),
        spec_resume(),
        spec_attachments_enc(),
        spec_wal("msgs-wal-plain", "wal"),
        spec_wal("msgs-wal-enc", "wal", encrypted=True),
        spec_wal("msgs-wal-missing-plain", "missing"),
        spec_wal("msgs-wal-badframes-plain", "badframes"),
        spec_hostile(),
        spec_corrupt(),
        spec_unsupported(),
        spec_vtab(),
        spec_rowid_desc(),
    ]


README = """\
# Synthetic messages catalogue (M3)

Test input for `crates/oma-messages` (`tests/profiles.rs` and the suites
named below). **Every value in these files is invented**: addresses under
`example.invalid`, `+1555…` numbers, and sentinel text (`SYNTH-BODY-…`,
`SYNTH-EDIT-…`, `SYNTH-DELETED-…`, `SYNTH-CHAT-NAME`, `SYNTH-GROUP-TITLE`,
`SYNTH-FILE-….heic`, `SYNTH-GENMOJI-PROMPT`, `SYNTH-MENTION`,
`SYNTH-SUBJECT`, `SYNTH_SENTINEL_…` object names). No real device, person,
message or backup was used, and nothing from any sample database (S3, S12) is
copied, DDL text included. The column sets are hand-written from column-name
facts (`scripts/fixtures/sms_profiles.py`); no profile is an iOS version.

## Layout

```text
<name>/SYNTHETIC0-0000000000000000/   one backup (plain or encrypted)
<name>.expected/sms.db                the plaintext sms.db (main file)
<name>.expected/sms.db-wal            the plaintext -wal, for WAL fixtures
<name>.expected/Manifest.db           the plaintext Manifest.db (encrypted fixtures)
<name>.expected/attachments/<fileID>  attachment plaintexts (encrypted fixtures)
<name>.expected/report.json           the generator's model of check()'s report
SHA256SUMS                            every file except this one and README.md
```

`report.json` is computed by the generator from the rows it wrote, by a
second statement of the rules in M3 sections 5-13. `tests/profiles.rs`
compares `check()`'s report with it through a projection. **Agreement proves
self-consistency only**; external agreement comes from the oracle runs in
`docs/compatibility.md`.

Encrypted fixtures use the password `{password}`, with DPIC 1,000 and ITER 10.
SQLite pages are 1,024 bytes, to keep every entry under 300 KB.

## Regenerating (reference environment only)

```sh
python3 -m venv .venv-fixtures
.venv-fixtures/bin/pip install -r scripts/fixtures/requirements.txt
.venv-fixtures/bin/python -I -c "import sqlite3; print(sqlite3.sqlite_version)"   # must print 3.45.1
.venv-fixtures/bin/python -I scripts/fixtures/make_synthetic_messages.py tests/fixtures/messages --seed 1
sha256sum tests/fixtures/messages/SHA256SUMS
```

The script refuses to write under any other SQLite unless
`--allow-other-sqlite` is passed: SQLite file bytes depend on the version.
Windows is not a supported generation environment. CI runs no Python; the
`fixtures_integrity` test checks every file against `SHA256SUMS`.

| | |
|---|---|
| Python | {python} |
| SQLite (Python `sqlite3`) | {sqlite} |
| cryptography | {cryptography} |
| Seed | {seed} |

## Oracles (manual; nothing enters the repository or CI)

Numbered as in the M3 spec (run 2 is the typedstream vectors' oracle).

- **Oracle 1, `iphone_backup_decrypt` 0.11.2** (MIT), through
   `scripts/fixtures/verify_messages_with_oracle.py`: every encrypted
   fixture's sms.db, `-wal`, Manifest.db and present attachment blobs
   decrypt to `.expected`. **Exempt, by name:** in `msgs-attachments-enc`,
   the attachments whose key is missing (`SYNTH-FILE-0006`), whose class is
   unavailable (`SYNTH-FILE-0007`), whose key does not unwrap
   (`SYNTH-FILE-0008`) and whose blob is missing (`SYNTH-FILE-0002`).
- **Oracle 3, imessage-exporter 4.3.0** (GPL-3.0; release build in scratch, black
   box): for each readable plaintext `.expected/sms.db` (WAL fixtures with
   their `-wal` beside them), the message count, and for each row with only
   an `attributedBody`, the recovered text, equal the generator's model.
   Steps: copy `.expected/` to a scratch directory, run
   `imessage-exporter -a macOS -p <copy>/sms.db -d` (the counts) and
   `-f txt` / `-f html -o <out>` (the text and the mention markup), and
   compare. No GPL tooling or wrapper is committed.
- **Oracle 4, our check versus imessage-exporter on third-party samples** (S3, S12;
   macOS chat.db), wrapped in a scratch-only unencrypted backup and checked
   with `OMA_EXTERNAL_BACKUP=<dir> cargo test -p oma-messages --test
   external_sample -- --ignored --nocapture`: status, schema facts and
   message, chat and handle counts. Aggregate counts only.

Each run is recorded in `docs/compatibility.md`, keyed to the SHA-256 of
`SHA256SUMS`.

## The fixtures

| Fixture | What it covers |
|---|---|
{table}
"""

PURPOSE = {
    "msgs-full-plain": "Every known column (and one unknown): bodies (text, attributedBody, both, empty text), tapbacks and removals, emoji tapback, sticker, poll vote, unparsed and missing targets, replies (one to a missing GUID), `reply_to_guid` alone, group actions, shared location, edits, unsent parts, `tmp`, scheduled, mentions, link key, audio transcription, Recently Deleted, orphans on every join, a message in two chats, attachments (found, sticker cache, macOS path, NULL)",
    "msgs-full-enc": "`msgs-full-plain`, encrypted",
    "msgs-cols-a-seconds-plain": "Snapshot-A column set; dates in seconds; no `chat_message_join.message_date`",
    "msgs-cols-threads-edits-plain": "Adds threads, `date_edited`, `part_count`, `message_summary_info` and the Recently Deleted tables",
    "msgs-cols-emoji-schedule-plain": "Adds `associated_message_emoji`, `schedule_*` and the Genmoji column",
    "msgs-cols-filter-plain": "Adds `filter_action`, `filter_sub_action` and `chat.is_pending_review`",
    "msgs-mixed-units-plain": "`message.date` in both seconds and nanoseconds",
    "msgs-scrambled-plain": "Columns in a seeded random order, plus unknown columns",
    "msgs-minimal-plain": "Required tables and columns only: every optional counter is \"not recorded\"",
    "msgs-resume-plain": "Rowid gaps and rows keyed `i64::MIN` and `i64::MAX`, in rowid and join tables",
    "msgs-attachments-enc": "Every attachment outcome: found, blob missing, not listed at the sourced key, sticker-cache miss, NoKey, ClassKeyUnavailable, KeyDoesNotUnwrap, a directory, NULL, each class not looked up, unrecognized, unusual and too long",
    "msgs-wal-plain": "Two messages and a link only in `sms.db-wal`",
    "msgs-wal-enc": "`msgs-wal-plain`, encrypted",
    "msgs-wal-missing-plain": "`sms.db-wal` listed, blob missing: `WalSkipped`; the main file's rows only",
    "msgs-wal-badframes-plain": "A second `-wal` transaction with a corrupted frame: `WalFramesInvalid`; the first transaction's rows only",
    "msgs-hostile-plain": "Type mismatches, invalid UTF-8, oversized values, hostile filenames, plist errors and a depth bomb, single-fault typedstream blobs, views, a trigger, an index, and sentinel object names",
    "msgs-corrupt-plain": "A damaged b-tree page: `Damaged(IntegrityProblems)`",
    "msgs-unsupported-plain": "`message` is a view: `Unsupported([NotOrdinaryTable(Message)])`",
    "msgs-vtab-plain": "An FTS5 table: `Unsupported([VirtualTablePresent])`",
    "msgs-rowid-desc-plain": "`ROWID INTEGER PRIMARY KEY DESC`: `Unsupported([RowidShape(Message)])`",
}


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
    specs = catalogue()
    names = {s.name for s in specs}
    for entry in os.listdir(out):
        base = entry[:-len(".expected")] if entry.endswith(".expected") else entry
        if base.startswith("msgs-") and base not in names:
            shutil.rmtree(os.path.join(out, entry))
    for s in specs:
        size = build_entry(s, out, a.seed)
        print(f"wrote {s.name} ({size} bytes)")
    mb.write_sha256sums(out)
    import cryptography
    table = "\n".join(f"| `{s.name}` | {PURPOSE[s.name]} |" for s in specs)
    with open(os.path.join(out, "README.md"), "w", encoding="utf-8", newline="\n") as f:
        f.write(README.format(password=PASSWORD, python=sys.version.split()[0],
                              sqlite=sqlite3.sqlite_version, cryptography=cryptography.__version__,
                              seed=a.seed, table=table))
    with open(os.path.join(out, "SHA256SUMS"), "rb") as f:
        print(f"SHA256SUMS sha256 {hashlib.sha256(f.read()).hexdigest()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
