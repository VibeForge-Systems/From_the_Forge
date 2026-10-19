#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""Build the SYNTHETIC export catalogue for OpenMessage Archive (M6).

Every value written here is invented: example.invalid addresses, +1555
numbers, and sentinel text (SYNTH-EXP-...). No real device, person,
message, backup or file is used, and nothing from any sample database is
copied. The attachment payloads are built here (a stored-deflate PNG, a PDF
header, plain text and unrecognised bytes).

Usage:

  make_synthetic_exports.py OUT_DIR [--seed N]

Writes, under OUT_DIR (which it empties first):

  <entry>/SYNTHETIC0-0000000000000000/  one backup (the import input)
  <entry>.expected/content.json         the import and viewer model after importing <entry> alone
  <entry>.expected/export.json          the export expectations for <entry>
  scenarios/<name>.content.json         the models after a sequence of imports
  scenarios/<name>.export.json          the export expectations for that sequence
  hidden-vectors.json                   the hidden-character vectors (M6 spec 4.7)
  SHA256SUMS                            the inputs: every backup file and hidden-vectors.json
  EXPECTED.SHA256SUMS                   every file under *.expected/ and scenarios/
  README.md                             generated; do not edit

content.json is the viewer catalogue's document (make_synthetic_viewer.py,
imported as a module): "content", the archive the M4 import must leave, and
"viewer", what the M5 viewer derives from it, including the masked strings.
export.json is this generator's own statement about the same inputs, made
from the inputs (never from Rust output): the masking sentinels by kind,
the hostile strings and formula cases with where each was planted, each
conversation's members, the messages in no conversation, and the archive
patches and store corruptions a test applies after the import (values an
import cannot produce, M6 spec 15). Agreement with the Rust code proves
self-consistency only.

SQLite file bytes depend on the SQLite version, so the script refuses to
run under any SQLite but the reference 3.45.1. Run with `python -I`.
"""
import argparse
import hashlib
import json
import os
import random
import shutil
import sqlite3
import struct
import sys
import unicodedata
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.dont_write_bytecode = True  # importing the sibling generators leaves no __pycache__

import make_synthetic_viewer as mv  # noqa: E402

mi, ms, ts = mv.mi, mv.ms, mv.ts

REFERENCE_SQLITE = "3.45.1"
NS = ms.NS
T0_NS = ms.T0_NS
T0_S = ms.T0_S
SECONDS_MAX = 3_124_137_600     # 2100-01-01 in Cocoa seconds (M3 time.rs)
LONG_TEXT = 4096                # as the viewer catalogue's manifests
UDID = "SYNTHETIC0-0000000000000000"

DEV_E = mi.Device("Synthetic Export Phone", "iPhone0,0", "18.4")


# --------------------------------------------------------------------------
# Payloads
# --------------------------------------------------------------------------

def _chunk(kind, data):
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)


def png(w, h, rgb):
    """An opaque RGB8 PNG of one colour. The image data is a stored (level 0)
    deflate stream, so the bytes do not depend on the zlib version."""
    row = b"\x00" + bytes(rgb) * w
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + _chunk(b"IHDR", ihdr)
            + _chunk(b"IDAT", zlib.compress(row * h, 0)) + _chunk(b"IEND", b""))


def pdf(tag):
    return (b"%PDF-1.4\n% " + tag.encode("ascii") + b"\n1 0 obj << /Type /Catalog >> endobj\n"
            b"trailer << /Root 1 0 R >>\n%%EOF\n")


def text_file(s):
    return s.encode("utf-8")


# --------------------------------------------------------------------------
# Rows
# --------------------------------------------------------------------------

def H(rowid, ident, service="iMessage"):
    return mv.H(rowid, ident, service)


def msg(n, prefix, body, **kw):
    return mv.msg(n, prefix, body, **kw)


def react(m, n, kind, target, **kw):
    return m(n, associated_message_type=kind, associated_message_guid=target, text=None,
             associated_message_range_location=0, associated_message_range_length=1, **kw)


def att_path(tag, n):
    """A usual lookup path; the hostile name is the transfer name only."""
    return f"~/Library/SMS/Attachments/{tag}/{n:02d}/SYNTH-EXP-{tag.upper()}-{n:04d}/file-{n:04d}.bin"


class Book:
    """What one content's export expectations collect while it is built."""

    def __init__(self):
        self.sentinels = {k: [] for k in SENTINEL_KINDS}
        self.visible = []
        self.hostile = []
        self.formula = []
        self.patches = []
        self.corrupt = []
        self.unsent_files = []

    def hostile_case(self, cid, where, text):
        self.hostile.append({"id": cid, "where": where, **described(text)})

    def formula_case(self, cid, where, text, leading, after=None):
        self.formula.append({"id": cid, "where": where, "leading": leading,
                             "after": after, **described(text)})


SENTINEL_KINDS = ("oneTimeCodes", "earlierText", "editText", "alternativeText",
                  "subjectHistory", "unsentAttachmentFiles")


def described(text):
    """A string in the expectations: itself when short, else its digest,
    length and first 32 characters (the long-text rule)."""
    raw = text.encode("utf-8", "surrogatepass")
    out = {"utf16Units": len(text.encode("utf-16-le", "surrogatepass")) // 2, "utf8Bytes": len(raw)}
    if len(raw) <= LONG_TEXT:
        out["text"] = text
    else:
        out["sha256"] = hashlib.sha256(raw).hexdigest()
        out["prefix"] = text[:32]
    return out


# ---- export-text: hostile text, quoting, formulas, scripts, lengths ----------

TP = "SYNTH-EXP-TEXT-MSG-"
TB = "SYNTH-EXP-TEXT-BODY"

# OWASP CSV-injection leading characters, after ignorables, and their
# full-width and small forms (M6 spec 5.2, 14.2).
FORMULA_BODIES = [
    ("eq", "=1+1 SYNTH-EXP-F-EQ", "=", None),
    ("plus", "+1+1 SYNTH-EXP-F-PLUS", "+", None),
    ("minus", "-1+1 SYNTH-EXP-F-MINUS", "-", None),
    ("at", "@SUM(1) SYNTH-EXP-F-AT", "@", None),
    ("tab", "\t=1 SYNTH-EXP-F-TAB", "\t", None),
    ("cr", "\r=1 SYNTH-EXP-F-CR", "\r", None),
    ("space-eq", "  =1 SYNTH-EXP-F-SPACE", "=", "whitespace"),
    ("bom-eq", "﻿=1 SYNTH-EXP-F-BOM", "=", "U+FEFF"),
    ("zwsp-eq", "​=1 SYNTH-EXP-F-ZWSP", "=", "U+200B"),
    ("shy-eq", "­=1 SYNTH-EXP-F-SHY", "=", "U+00AD"),
    ("wj-eq", "⁠=1 SYNTH-EXP-F-WJ", "=", "U+2060"),
    ("cgj-plus", "͏+1 SYNTH-EXP-F-CGJ", "+", "U+034F"),
    ("vs-at", "️@SUM(1) SYNTH-EXP-F-VS", "@", "U+FE0F"),
    ("fw-eq", "＝1+1 SYNTH-EXP-F-FW-EQ", "＝", None),
    ("fw-plus", "＋1 SYNTH-EXP-F-FW-PLUS", "＋", None),
    ("fw-minus", "－1 SYNTH-EXP-F-FW-MINUS", "－", None),
    ("fw-at", "＠SUM(1) SYNTH-EXP-F-FW-AT", "＠", None),
    ("small-eq", "﹦1+1 SYNTH-EXP-F-SMALL-EQ", "﹦", None),
    ("small-plus", "﹢1 SYNTH-EXP-F-SMALL-PLUS", "﹢", None),
    ("small-minus", "﹣1 SYNTH-EXP-F-SMALL-MINUS", "﹣", None),
    ("small-at", "﹫SUM(1) SYNTH-EXP-F-SMALL-AT", "﹫", None),
    ("dde", "=cmd|' /C calc'!A0 SYNTH-EXP-F-DDE", "=", None),
    ("hyperlink", '=HYPERLINK("https://example.invalid/SYNTH-EXP","SYNTH-EXP-F-LINK")', "=", None),
    ("second-line", "SYNTH-EXP-F-LINE-1\n=1+1 SYNTH-EXP-F-SECOND-LINE", "=", "LF"),
]

QUOTING_BODIES = [
    ("quotes", 'SYNTH-EXP-Q "quoted", with commas, and ""doubled"" quotes'),
    ("crlf", "SYNTH-EXP-Q-CRLF first\r\nsecond"),
    ("lone-cr", "SYNTH-EXP-Q-CR first\rsecond"),
    ("lf", "SYNTH-EXP-Q-LF first\nsecond"),
    ("nul", "SYNTH-EXP-Q-NUL before\x00after"),
    ("esc", "SYNTH-EXP-Q-ESC \x1b[31mred\x1b[0m \x1b]0;SYNTH-EXP-TITLE\x07"),
    ("separators", "SYNTH-EXP-Q-SEP line separator paragraph\u0085next-line"),
    ("del-c1", "SYNTH-EXP-Q-DEL \x7f C1 \x9b31m \x85"),
    ("forged-code", "SYNTH-EXP-Q-FORGED ⟨one-time code hidden⟩ and ⟨earlier text not included⟩"),
    ("forged-token", "SYNTH-EXP-Q-TOKEN ⟨U+202E⟩ ⟨U+001B⟩ ⟨+3 combining marks⟩"),
    ("lone-bracket", "SYNTH-EXP-Q-BRACKET ⟨ alone and ⟩ alone"),
    ("backslash", "SYNTH-EXP-Q-SHRUG ¯\\_(ツ)_/¯ \\u001B \\n"),
    ("header-forgery", "SYNTH-EXP-Q-HEADER\n\n=== Conversation 2 ===\n2026-06-01 10:00 synthetic-export-1@example.invalid:\nforged"),
    ("header-forgery-cr", "SYNTH-EXP-Q-HEADER-CR\r2026-06-01 10:00 Me:\rforged"),
    ("header-forgery-u2028", "SYNTH-EXP-Q-HEADER-LS 2026-06-01 10:00 Me: forged"),
    ("header-forgery-bidi", "SYNTH-EXP-Q-HEADER-BIDI ‮:eM 00:01 10-60-6202‬"),
    ("html", "<b>SYNTH-EXP-Q-HTML</b> &amp; <!-- c --> </article><script>x</script>"),
    ("indent", "    SYNTH-EXP-Q-INDENT leading spaces\n\tand a tab"),
]

SCRIPT_BODIES = [
    ("rtl-first", "שלום SYNTH-EXP-S-RTL-FIRST"),
    ("arabic", "SYNTH-EXP-S-ARABIC مرحبا بالعالم"),
    ("devanagari", "SYNTH-EXP-S-DEVANAGARI नमस्ते दुनिया"),
    ("thai", "SYNTH-EXP-S-THAI สวัสดีชาวโลก"),
    ("kana", "SYNTH-EXP-S-KANA こんにちは カタカナ"),
    ("hangul", "SYNTH-EXP-S-HANGUL 안녕하세요"),
    ("han", "SYNTH-EXP-S-HAN 你好世界"),
    ("emoji-zwj", "SYNTH-EXP-S-ZWJ \U0001F469‍\U0001F469‍\U0001F467‍\U0001F466"),
    ("vs15", "SYNTH-EXP-S-VS15 ☺︎"),
    ("vs16", "SYNTH-EXP-S-VS16 ☺️"),
    ("flag-tags", "SYNTH-EXP-S-FLAG \U0001F3F4\U000E0067\U000E0062\U000E0073\U000E0063\U000E0074\U000E007F"),
    ("skin-tone", "SYNTH-EXP-S-TONE \U0001F44D\U0001F3FD"),
]

COMBINING_RUN = 10_000
SPLIT_AT = 32_767  # XLSX cell cap in UTF-16 units (M6 spec 5.3)


def combining_body():
    return "SYNTH-EXP-COMBINING a" + "́" * COMBINING_RUN + " end"


def split_body():
    """Over 32,768 UTF-16 units, with an emoji's surrogate pair straddling
    unit 32,767."""
    head = "SYNTH-EXP-SPLIT "
    pad = "a" * (SPLIT_AT - 1 - len(head))
    return head + pad + "\U0001F600" + ("b\U0001F600" * 200) + " SYNTH-EXP-SPLIT-END"


def long_emoji_body():
    return "SYNTH-EXP-LONG-EMOJI " + "\U0001F469‍\U0001F469‍\U0001F467‍\U0001F466" * 1400


LONG_ADDRESS = "synthetic-export-" + "b" * 300 + "@example.invalid"
LONG_CHAT_NAME = "SYNTH-EXP-LONG-NAME-" + "c" * 300
LONG_ASSOC_EMOJI = "\U0001F469‍\U0001F469‍\U0001F467‍\U0001F466" * 4  # 100 bytes


def content_text(book):
    m = lambda n, **kw: msg(n, TP, TB, **kw)
    handles = [
        H(1, "synthetic-export-1@example.invalid"),
        H(2, "+15550000201"),                                       # leading '+'
        H(3, "-synthetic-export-3@example.invalid"),                 # leading '-'
        H(4, "@synthetic-export-4"),                                 # leading '@'
        H(5, "=synthetic-export-5@example.invalid"),                 # leading '='
        H(6, "synthetic-export-6 line@example.invalid"),       # U+2028 in a sender
        H(7, "synthetic-export-7\u0085next‮@example.invalid"), # U+0085 and RLO
        H(8, LONG_ADDRESS),                                          # over 256 units
        H(9, "＝synthetic-export-9@example.invalid"),            # full-width '='
        H(10, "​=synthetic-export-10@example.invalid"),         # '=' after ZWSP
    ]
    for h in handles[1:5] + handles[8:10]:
        ident = h["id"]
        lead = ident.lstrip("​")[0]
        book.formula_case(f"address-{h['ROWID']}", "address", ident, lead,
                          "U+200B" if ident.startswith("​") else None)
    for h in (handles[5], handles[6], handles[7]):
        book.hostile_case(f"address-{h['ROWID']}", "address", h["id"])
    group_name = '=HYPERLINK("https://example.invalid/SYNTH-EXP-CHAT","SYNTH-EXP-CHAT")'
    sep_name = "SYNTH-EXP-NAME first second\u0085third fourth ‮evil"
    chats = [
        mi.C(1, "iMessage;-;synthetic-export-1@example.invalid", ident="synthetic-export-1@example.invalid"),
        mi.C(2, "iMessage;+;chatSYNTHEXP2", style=43, ident="chatSYNTHEXP2", room="chatSYNTHEXP2",
             name=group_name),
        mi.C(3, "iMessage;+;chatSYNTHEXP3", style=43, ident="chatSYNTHEXP3", room="chatSYNTHEXP3",
             name=sep_name),
        mi.C(4, "iMessage;+;chatSYNTHEXP4", style=43, ident="chatSYNTHEXP4", room="chatSYNTHEXP4",
             name=LONG_CHAT_NAME),
    ]
    book.formula_case("chat-name-2", "chatName", group_name, "=")
    book.hostile_case("chat-name-3", "chatName", sep_name)
    book.hostile_case("chat-name-4", "chatName", LONG_CHAT_NAME)
    msgs, c1, c2, c3, c4 = [], [], [], [], []
    n = 0

    def add(chat_list, text, cid, kind, **kw):
        nonlocal n
        n += 1
        msgs.append(m(n, text=text, **kw))
        chat_list.append(n)
        if kind == "hostile":
            book.hostile_case(cid, "body", text)
        return n

    for cid, text, lead, after in FORMULA_BODIES:
        add(c1, text, None, None, handle_id=1)
        book.formula_case(f"body-{cid}", "body", text, lead, after)
    for cid, text in QUOTING_BODIES:
        add(c1, text, f"body-{cid}", "hostile", handle_id=1)
    for cid, text in SCRIPT_BODIES:
        add(c1, text, f"body-{cid}", "hostile", handle_id=1)
    for label, text in mv.HOSTILE_TEXTS:
        if label == "1 MiB body":
            continue  # the viewer catalogue holds it; the export reads it from there
        cid = "viewer-" + "".join(ch if ch.isalnum() else "-" for ch in label.lower())
        add(c1, text, cid, "hostile", handle_id=1)
    add(c1, combining_body(), "body-combining-10000", "hostile", handle_id=1)
    add(c1, split_body(), "body-split-32767", "hostile", handle_id=1)
    add(c1, long_emoji_body(), "body-long-emoji", "hostile", handle_id=1)
    target = n
    n += 1
    msgs.append(react(m, n, 2006, f"p:0/{TP}{target:04d}", handle_id=1,
                      associated_message_emoji=LONG_ASSOC_EMOJI))
    c1.append(n)
    book.hostile_case("assoc-emoji-100-bytes", "associationEmoji", LONG_ASSOC_EMOJI)
    # Senders with hostile addresses, in the formula-named group.
    for h in range(2, 11):
        add(c2, f"SYNTH-EXP-TEXT-FROM-{h}", None, None, handle_id=h)
    add(c2, "SYNTH-EXP-TEXT-FROM-ME", None, None, handle_id=0, is_from_me=1)
    add(c3, "SYNTH-EXP-TEXT-IN-SEPARATOR-CHAT", None, None, handle_id=6)
    add(c3, "SYNTH-EXP-TEXT-FROM-7", None, None, handle_id=7)
    add(c4, "SYNTH-EXP-TEXT-IN-LONG-CHAT", None, None, handle_id=8)
    chat_msgs = [(1, k) for k in c1] + [(2, k) for k in c2] + [(3, k) for k in c3] + [(4, k) for k in c4]
    chat_handles = [(1, 1)] + [(2, h) for h in range(2, 11)] + [(3, 6), (3, 7), (4, 8), (4, 1)]
    rows = mv.rows_of(handles, chats, msgs, chat_handles, chat_msgs)
    return mi.Content("export-text", mi.import_profile(), rows)


# ---- export-records: segments, codes, plain-column text, dates, membership ----

RP = "SYNTH-EXP-REC-MSG-"
RB = "SYNTH-EXP-REC-BODY"
OTC_REC = "SYNTH-EXP-OTC-KWQZ"
OTC_ALT = "SYNTH-EXP-OTC-ALTCODE"
EDIT_REC = "SYNTH-EXP-EDIT-BEFORE-UNSEND-REC"
ALT_PLAIN = "SYNTH-EXP-ALT-PLAIN-COLUMN"
ALT_OTC = "SYNTH-EXP-ALT-OTC-PLAIN-COLUMN"
ALT_UNSENT = "SYNTH-EXP-ALT-UNSENT-PLAIN-COLUMN"

# Raw Cocoa dates (M3 time.rs windows): name, raw value, expected unit.
DATES = [
    ("seconds-2020", 600_000_000, "seconds"),
    ("seconds-2004-before-2007", 100_000_000, "seconds"),
    ("ns-2099", (SECONDS_MAX - 86_400) * NS, "nanoseconds"),
    ("zero", 0, "zero"),
    ("negative", -60, "negative"),
    ("pre-1900", -3_200_000_000, "negative"),
    ("ambiguous", SECONDS_MAX + 1, "ambiguous"),
    ("year-9999-seconds", 252_423_993_600, "ambiguous"),
    ("null", None, None),
]


def content_records(book):
    m = lambda n, **kw: msg(n, RP, RB, **kw)
    handles = [H(1, "synthetic-export-rec-1@example.invalid"), H(2, "+15550000301")]
    chats = [
        mi.C(1, "iMessage;-;synthetic-export-rec-1@example.invalid", ident="synthetic-export-rec-1@example.invalid"),
        mi.C(2, "iMessage;+;chatSYNTHEXPREC2", style=43, ident="chatSYNTHEXPREC2", room="chatSYNTHEXPREC2",
             name="SYNTH-EXP-REC-GROUP"),
        mi.C(3, "iMessage;-;+15550000301", ident="+15550000301"),
    ]
    otc_ab = ms.ab_text([
        ("Your code is ", [(ts.P, 0)]),
        (OTC_REC, [(ts.P, 0), (ts.OTC, 1)]),
        (" for ", [(ts.P, 0)]),
        ("SYNTH-EXP-MENTION", [(ts.P, 0), (ts.MENTION, "synthetic-export-rec-1@example.invalid")]),
    ])
    otc_text = f"Your code is {OTC_REC} for SYNTH-EXP-MENTION"
    book.sentinels["oneTimeCodes"].append(OTC_REC)
    segments_ab = ms.ab_text([
        ("SYNTH-EXP-SEG-INLINE ", [(ts.P, 0)]),
        ("￼", [(ts.P, 1), (ts.EMOJI, 1)]),
        (" unmatched ", [(ts.P, 2)]),
        ("￼", [(ts.P, 3), (ts.GUID, "SYNTH-EXP-REC-ATT-NOT-LINKED")]),
    ])
    alt_otc_ab = ms.ab_text([("SYNTH-EXP-ALT-OTC-FORMATTED ", [(ts.P, 0)]),
                             (OTC_ALT, [(ts.P, 0), (ts.OTC, 1)])])
    book.sentinels["oneTimeCodes"].append(OTC_ALT)
    book.sentinels["alternativeText"] += [ALT_OTC, ALT_UNSENT]
    book.visible.append(ALT_PLAIN)
    unsent_ab = ms.ab_text([("SYNTH-EXP-ALT-UNSENT-PART-0 ", [(ts.P, 0)]),
                            ("SYNTH-EXP-ALT-UNSENT-PART-2", [(ts.P, 2)])])
    edit_ab = ms.ab_text([("SYNTH-EXP-REC-EDITED-PART-0 ", [(ts.P, 0)]),
                          ("SYNTH-EXP-REC-EDITED-PART-2", [(ts.P, 2)])])
    book.sentinels["editText"].append(EDIT_REC)
    msgs = [
        # Body segments.
        m(1, handle_id=1, text=otc_text, attributedBody=otc_ab),
        m(2, handle_id=1, text=None, attributedBody=segments_ab, part_count=4),
        m(3, handle_id=1, text=f"{RB}-3 transcription", is_audio_message=1,
          attributedBody=ms.ab_text([(f"{RB}-3 transcription", [(ts.P, 0), ("IMAudioTranscription", 1)])])),
        # Unknown values.
        m(4, handle_id=1, is_from_me=2),
        m(5, handle_id=1, item_type=77, text="SYNTH-EXP-REC-ITEM-77"),
        m(6, handle_id=1, item_type=3, group_action_type=99, text=None),
        m(7, handle_id=1, balloon_bundle_id=None),
        m(8, handle_id=1, balloon_bundle_id="com.example.synthetic.export"),
        # Plain-column text.
        m(9, handle_id=1, text=ALT_PLAIN, attributedBody=ms.ab_plain("SYNTH-EXP-ALT-FORMATTED")),
        m(10, handle_id=1, text=ALT_OTC, attributedBody=alt_otc_ab),
        m(11, is_from_me=1, handle_id=1, text=ALT_UNSENT, attributedBody=unsent_ab, part_count=3,
          date_edited=T0_NS + 11 * 60 * NS + 30 * NS, message_summary_info=ms.summary(otr=3, rp=[1])),
        # Decoded edit text on a message with an unsent part (withheld).
        m(12, is_from_me=1, handle_id=1, text=None, attributedBody=edit_ab, part_count=3,
          date_edited=T0_NS + 12 * 60 * NS + 30 * NS,
          message_summary_info=ms.summary(otr=3, rp=[1], ec={0: [{"d": T0_S + 720, "t": mv.ec_text(EDIT_REC)}]})),
        # Scheduled.
        m(13, is_from_me=1, handle_id=1, schedule_type=2, schedule_state=1),
        # Ties: the same date.
        m(14, handle_id=1, date=T0_NS + 14 * 60 * NS),
        m(15, handle_id=1, date=T0_NS + 14 * 60 * NS),
        # Membership: two conversations, and none.
        m(16, handle_id=1, text=f"{RB}-16 in two conversations"),
        m(17, handle_id=1, text=f"{RB}-17 in no conversation"),
        m(18, handle_id=2, text=f"{RB}-18 in the group"),
    ]
    first_date = 30
    for i, (name, raw, _unit) in enumerate(DATES):
        msgs.append(m(first_date + i, handle_id=2, date=raw, text=f"SYNTH-EXP-DATE-{name}"))
    c1 = [k for k in range(1, 17)]
    chat_msgs = [(1, k) for k in c1] + [(2, 16), (2, 18)] + [(3, first_date + i) for i in range(len(DATES))]
    # Values the import cannot produce: written into the archive after the
    # import (M6 spec 3.9, 15).
    book.patches += [
        {"table": "message", "guid": f"{RP}{4:04d}", "set": {"is_from_me": None, "is_from_me_st": 7},
         "why": "an is_from_me state this build does not know"},
        {"table": "message", "guid": f"{RP}{14:04d}", "set": {"date_unit": 9, "date_utc_ns": None},
         "why": "a time unit this build does not know (the sort key keeps the dated place)"},
        {"table": "message", "guid": f"{RP}{5:04d}", "set": {"item_kind": 99},
         "why": "an item kind this build does not know"},
    ]
    rows = mv.rows_of(handles, chats, msgs, [(1, 1), (2, 1), (2, 2), (3, 2)], chat_msgs)
    return mi.Content("export-records", mi.import_profile(), rows)


def content_oldcols(book):
    m = lambda n, **kw: msg(n, "SYNTH-EXP-OLD-MSG-", "SYNTH-EXP-OLD-BODY", **kw)
    rows = mv.rows_of([H(1, "synthetic-export-old-1@example.invalid")],
                      [mi.C(1, "iMessage;-;synthetic-export-old-1@example.invalid",
                            ident="synthetic-export-old-1@example.invalid")],
                      [m(1), m(2, is_from_me=1)], [(1, 1)], [(1, 1), (1, 2)])
    return mi.Content("export-oldcols", mi.import_profile([("message", "balloon_bundle_id")]), rows)


# ---- export-files: attachment names, types, shared and corrupted blobs --------

FP = "SYNTH-EXP-FILE-MSG-"
UNSENT_FILE_FILES = "SYNTH-EXP-UNSENT-FILE-SENTINEL-FILES"

# (id, transfer name, payload kind, mime, uti, note)
FILE_CASES = [
    ("allow-png", "SYNTH-EXP-picture.png", "png", "image/png", "public.png", "allowlisted and agrees"),
    ("allow-txt", "SYNTH-EXP-notes.txt", "txt", "text/plain", "public.plain-text", "allowlisted and agrees"),
    ("case-upper", "SYNTH-EXP-Report.PDF", "pdf", "application/pdf", "com.adobe.pdf", "differs only by case"),
    ("case-lower", "SYNTH-EXP-report.pdf", "pdf2", "application/pdf", "com.adobe.pdf", "differs only by case"),
    ("nfc", unicodedata.normalize("NFC", "SYNTH-EXP-café.txt"), "txt", "text/plain", "public.plain-text",
     "NFC"),
    ("nfd", unicodedata.normalize("NFD", "SYNTH-EXP-café.txt"), "txt2", "text/plain", "public.plain-text",
     "NFD of the previous name"),
    ("reserved-con", "CON.txt", "txt", "text/plain", "public.plain-text", "Windows reserved name"),
    ("reserved-aux", "aux", "unknown", None, None, "Windows reserved name, no extension"),
    ("reserved-com1", "COM1.png", "png", "image/png", "public.png", "Windows reserved name"),
    ("reserved-nul", "nul.jpg", "png", "image/jpeg", "public.jpeg", "Windows reserved name; declared JPEG, PNG bytes"),
    ("long-256", "SYNTH-EXP-" + "é" * 150 + ".png", "png", "image/png", "public.png", "over 255 UTF-8 bytes"),
    ("trailing-dots", "SYNTH-EXP-trailing.dots...", "txt", "text/plain", "public.plain-text", "trailing dots"),
    ("trailing-space", "SYNTH-EXP-trailing space.png ", "png", "image/png", "public.png", "trailing space"),
    ("invisible", "SYNTH-EXP-in​visi⁠ble.png", "png", "image/png", "public.png", "invisible characters"),
    ("rlo-exe", "SYNTH-EXP-‮gpj.exe", "unknown", "image/jpeg", "public.jpeg", "U+202E; executable extension"),
    ("formula-eq", "=SYNTH-EXP-cmd.png", "png", "image/png", "public.png", "formula prefix"),
    ("formula-plus", "+SYNTH-EXP-1.png", "png", "image/png", "public.png", "formula prefix"),
    ("formula-at", "@SYNTH-EXP-x.png", "png", "image/png", "public.png", "formula prefix"),
    ("formula-minus", "-SYNTH-EXP-x.png", "png", "image/png", "public.png", "formula prefix"),
    ("unknown-type", "SYNTH-EXP-data.xyz", "unknown", "application/octet-stream", "public.data",
     "unknown type"),
    ("mismatch-pdf", "SYNTH-EXP-photo.pdf", "png", "application/pdf", "com.adobe.pdf",
     "declared PDF, sniffed PNG"),
    ("mismatch-png", "SYNTH-EXP-doc.png", "txt", "image/png", "public.png", "declared PNG, sniffed text"),
    ("html-ext", "SYNTH-EXP-page.html", "txt", "text/html", "public.html", "active extension"),
    ("svg-ext", "SYNTH-EXP-image.svg", "txt", "image/svg+xml", "public.svg-image", "active extension"),
    ("empty-name", "", "png", "image/png", "public.png", "empty name (the sniffed extension)"),
    ("traversal-back", "..\\..\\SYNTH-EXP-evil.png", "png", "image/png", "public.png", "backslash traversal"),
    ("traversal-fwd", "../../SYNTH-EXP-evil.png", "png", "image/png", "public.png", "slash traversal"),
    ("separator", "SYNTH-EXP-a/b.png", "png", "image/png", "public.png", "a path separator"),
    ("controls", "SYNTH-EXP-tab\tnew\nline.txt", "txt", "text/plain", "public.plain-text", "controls"),
    ("shared-one", "SYNTH-EXP-shared-one.png", "shared", "image/png", "public.png", "one blob, two names"),
    ("shared-two", "SYNTH-EXP-shared-two.png", "shared", "image/png", "public.png", "one blob, two names"),
    ("corrupt", "SYNTH-EXP-corrupt-me.png", "corrupt", "image/png", "public.png",
     "the test corrupts its store file after the import"),
    ("null-name", None, "png", "image/png", "public.png", "transfer name NULL"),
]


def payload(kind, n):
    if kind == "png":
        return png(3, 2, (n * 7 % 256, 40, 200))
    if kind == "shared":
        return png(5, 5, (1, 2, 3))
    if kind == "corrupt":
        return png(4, 4, (9, 9, 9))
    if kind == "pdf":
        return pdf(f"SYNTH-EXP-PDF-{n}")
    if kind == "pdf2":
        return pdf(f"SYNTH-EXP-PDF-OTHER-{n}")
    if kind == "txt":
        return text_file(f"SYNTH-EXP-TEXT-FILE-{n}\n")
    if kind == "txt2":
        return text_file(f"SYNTH-EXP-TEXT-FILE-NFD-{n}\n")
    if kind == "unknown":
        return b"MZ\x90\x00SYNTH-EXP-UNKNOWN-" + bytes([n]) + b"\x00\x01\x02\xff"
    raise ValueError(kind)


def content_files(book):
    m = lambda n, **kw: msg(n, FP, "SYNTH-EXP-FILE-BODY", cache_has_attachments=1, handle_id=1, **kw)
    msgs, atts, files, links = [], [], [], []
    cases = []
    for n, (cid, name, kind, mime, uti, note) in enumerate(FILE_CASES, 1):
        data = payload(kind, n)
        path = att_path("f", n)
        guid = f"SYNTH-EXP-FILE-ATT-{n:04d}"
        atts.append(mi.A(n, guid, path, len(data), mime_type=mime, uti=uti, transfer_name=name))
        files.append(mi.AttFile(mi.media(path), data))
        msgs.append(m(n))
        links.append((n, n))
        cases.append({"id": cid, "attachment": guid, "note": note,
                      "transferName": None if name is None else described(name),
                      "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)})
        if kind == "corrupt":
            book.corrupt.append({"attachment": guid, "sha256": hashlib.sha256(data).hexdigest(),
                                 "why": "a store file whose bytes no longer hash to its name"})
        if name and name[0] in "=+-@":
            book.formula_case(f"file-{cid}", "fileName", name, name[0])
        elif name is not None:
            book.hostile_case(f"file-{cid}", "fileName", name)
    # Attachments of an unsent part (one import): part 0 names the kept
    # file, part 1 (unsent) named the withheld one, part 2 is text.
    k = len(FILE_CASES) + 1
    kept, gone = k, k + 1
    kept_data = png(2, 2, (10, 20, 30))
    gone_data = text_file(f"{UNSENT_FILE_FILES}\n")
    for n, data, name in ((kept, kept_data, "SYNTH-EXP-unsent-kept.png"),
                          (gone, gone_data, "SYNTH-EXP-unsent-gone.txt")):
        path = att_path("f", n)
        atts.append(mi.A(n, f"SYNTH-EXP-FILE-ATT-{n:04d}", path, len(data),
                         mime_type="image/png" if n == kept else "text/plain",
                         uti="public.png" if n == kept else "public.plain-text", transfer_name=name))
        files.append(mi.AttFile(mi.media(path), data))
        links.append((k, n))
    ab = ms.ab_text([("￼", [(ts.P, 0), (ts.GUID, f"SYNTH-EXP-FILE-ATT-{kept:04d}")]),
                     ("SYNTH-EXP-FILE-UNSENT-PART-2", [(ts.P, 2)])])
    msgs.append(m(k, is_from_me=1, text=None, attributedBody=ab, part_count=3,
                  date_edited=T0_NS + k * 60 * NS + 30 * NS, message_summary_info=ms.summary(otr=3, rp=[1])))
    book.sentinels["unsentAttachmentFiles"].append(UNSENT_FILE_FILES)
    book.unsent_files.append({"attachment": f"SYNTH-EXP-FILE-ATT-{gone:04d}",
                              "sha256": hashlib.sha256(gone_data).hexdigest()})
    handles = [H(1, "synthetic-export-files-1@example.invalid")]
    chats = [mi.C(1, "iMessage;-;synthetic-export-files-1@example.invalid",
                  ident="synthetic-export-files-1@example.invalid")]
    rows = mv.rows_of(handles, chats, msgs, [(1, 1)], [(1, r["ROWID"]) for r in msgs], links, atts)
    book.file_cases = cases
    return mi.Content("export-files", mi.import_profile(), rows, files)


# ---- export-hist-1 / export-hist-2: content captured before an unsend ---------

HP = "SYNTH-EXP-HIST-MSG-"
HB = "SYNTH-EXP-HIST-BODY"
EARLIER_HIST = "SYNTH-EXP-EARLIER-BEFORE-UNSEND"
SUBJECT_HIST = "SYNTH-EXP-SUBJECT-BEFORE-UNSEND"
EDIT_HIST = "SYNTH-EXP-EDIT-BEFORE-UNSEND-HIST"
UNSENT_FILE_HIST = "SYNTH-EXP-UNSENT-FILE-SENTINEL-HIST"


def content_hist(version, book):
    """Two backups of one phone. Version 1 holds every part; version 2 has
    part 1 of messages 1, 4 and 5 unsent, an edit, and new subjects and a
    new chat name."""
    m = lambda n, **kw: msg(n, HP, HB, handle_id=1, **kw)
    t = lambda n: T0_NS + n * 60 * NS
    three = lambda tag, middle: ms.ab_text([(f"SYNTH-EXP-{tag}-PART-0 ", [(ts.P, 0)]),
                                            (middle, [(ts.P, 1)]),
                                            (f" SYNTH-EXP-{tag}-PART-2", [(ts.P, 2)])])
    two = lambda tag: ms.ab_text([(f"SYNTH-EXP-{tag}-PART-0 ", [(ts.P, 0)]),
                                  (f" SYNTH-EXP-{tag}-PART-2", [(ts.P, 2)])])
    att_guid = "SYNTH-EXP-HIST-ATT-0001"
    att_data = text_file(f"{UNSENT_FILE_HIST}\n")
    path = att_path("h", 1)
    atts, files, links = [], [], []
    if version == 1:
        msgs = [
            m(1, text=None, attributedBody=three("H1", EARLIER_HIST), part_count=3, subject=SUBJECT_HIST),
            m(2, subject="SYNTH-EXP-SUBJECT-OLD-VISIBLE"),
            m(3, text="SYNTH-EXP-HIST-3 original"),
            m(4, text=None, attributedBody=three("H4", "SYNTH-EXP-H4-MIDDLE"), part_count=3),
            m(5, text=None, part_count=3, cache_has_attachments=1,
              attributedBody=ms.ab_text([("SYNTH-EXP-H5-PART-0 ", [(ts.P, 0)]),
                                         ("￼", [(ts.P, 1), (ts.GUID, att_guid)]),
                                         (" SYNTH-EXP-H5-PART-2", [(ts.P, 2)])])),
        ]
        atts = [mi.A(1, att_guid, path, len(att_data), mime_type="text/plain", uti="public.plain-text",
                     transfer_name="SYNTH-EXP-hist-unsent.txt")]
        files = [mi.AttFile(mi.media(path), att_data)]
        links = [(5, 1)]
        name = "SYNTH-EXP-HIST-NAME-1"
    else:
        msgs = [
            m(1, text=None, attributedBody=two("H1"), part_count=3, subject="SYNTH-EXP-SUBJECT-AFTER-UNSEND",
              date_edited=t(1) + 30 * NS, message_summary_info=ms.summary(otr=3, rp=[1])),
            m(2, subject="SYNTH-EXP-SUBJECT-NEW-VISIBLE"),
            m(3, text="SYNTH-EXP-HIST-3 edited", date_edited=t(3) + 30 * NS,
              message_summary_info=ms.summary(otr=1, ec={0: [{"d": T0_S + 200,
                                                              "t": mv.ec_text("SYNTH-EXP-HIST-3 original")}]})),
            m(4, text=None, attributedBody=two("H4"), part_count=3, date_edited=t(4) + 30 * NS,
              message_summary_info=ms.summary(otr=3, rp=[1], ec={0: [{"d": T0_S + 260,
                                                                      "t": mv.ec_text(EDIT_HIST)}]})),
            m(5, text=None, attributedBody=two("H5"), part_count=3, date_edited=t(5) + 30 * NS,
              message_summary_info=ms.summary(otr=3, rp=[1])),
        ]
        name = "SYNTH-EXP-HIST-NAME-2"
    handles = [H(1, "synthetic-export-hist-1@example.invalid")]
    chats = [mi.C(1, "iMessage;-;synthetic-export-hist-1@example.invalid",
                  ident="synthetic-export-hist-1@example.invalid", name=name)]
    rows = mv.rows_of(handles, chats, msgs, [(1, 1)], [(1, r["ROWID"]) for r in msgs], links, atts)
    return mi.Content(f"export-hist-{version}", mi.import_profile(), rows, files)


def hist_book(book):
    """The history scenario's sentinels (both backups imported)."""
    book.sentinels["earlierText"].append(EARLIER_HIST)
    book.sentinels["subjectHistory"].append(SUBJECT_HIST)
    book.sentinels["editText"].append(EDIT_HIST)
    book.sentinels["unsentAttachmentFiles"].append(UNSENT_FILE_HIST)
    book.unsent_files.append({"attachment": "SYNTH-EXP-HIST-ATT-0001",
                              "sha256": hashlib.sha256(text_file(f"{UNSENT_FILE_HIST}\n")).hexdigest()})
    book.visible += ["SYNTH-EXP-SUBJECT-OLD-VISIBLE", "SYNTH-EXP-SUBJECT-NEW-VISIBLE",
                     "SYNTH-EXP-HIST-NAME-1", "SYNTH-EXP-HIST-NAME-2", "SYNTH-EXP-HIST-3 edited"]


# --------------------------------------------------------------------------
# Hidden-character vectors (M6 spec 4.7; D121's rule, inverted)
# --------------------------------------------------------------------------

# Classes: "keep" (kept by "Copy text without invisible characters" and not
# marked), "bidi", "invisible", "joiner", "tag", "vsRun" (marked as hidden
# characters, dropped by the copy), "control" (C0 and C1 other than TAB,
# LF and CR, and DEL: marked as controls, dropped by the copy) and "cr"
# (kept by the copy, which keeps layout; marked as a control by the
# export's text formats).
HIDDEN_CLASSES = ("keep", "bidi", "invisible", "joiner", "tag", "vsRun", "control", "cr")

FAMILY = "\U0001F469‍\U0001F469‍\U0001F467"

HIDDEN_VECTORS = [
    ("plain", [("SYNTH plain text", "keep")]),
    ("rlo", [("pay", "keep"), ("‮", "bidi"), ("gnp.exe", "keep"), ("‬", "bidi")]),
    ("isolates", [("a", "keep"), ("⁦", "bidi"), ("b", "keep"), ("⁩", "bidi"), ("c", "keep"),
                  ("⁧", "bidi"), ("d", "keep"), ("⁨", "bidi"), ("e", "keep")]),
    ("marks", [("x", "keep"), ("‎", "bidi"), ("y", "keep"), ("‏", "bidi"), ("z", "keep"),
               ("؜", "bidi")]),
    ("embeddings", [("‪", "bidi"), ("‫", "bidi"), ("‭", "bidi"), ("q", "keep")]),
    ("zwsp-wj-bom", [("pay", "keep"), ("​", "invisible"), ("pal", "keep"), ("⁠", "invisible"),
                     ("!", "keep"), ("﻿", "invisible")]),
    ("object-replacement-unresolved", [("see ", "keep"), ("￼", "invisible"), (" here", "keep")]),
    ("zwj-in-emoji", [(FAMILY, "keep")]),
    ("zwj-in-script", [("क्‍ष", "keep")]),
    ("zwnj-in-script", [("می‌خواهم", "keep")]),
    ("zwj-between-ascii", [("ab", "keep"), ("‍", "joiner"), ("cd", "keep")]),
    ("zwnj-at-start", [("‌", "joiner"), ("من", "keep")]),
    ("zwj-at-end", [("من", "keep"), ("‍", "joiner")]),
    ("zwj-before-ascii", [("é", "keep"), ("‍", "joiner"), ("x", "keep")]),
    ("flag-tags", [("\U0001F3F4\U000E0067\U000E0062\U000E0073\U000E0063\U000E0074\U000E007F", "keep")]),
    ("tags-alone", [("SYNTH", "keep"), ("\U000E0041", "tag"), ("\U000E0042", "tag"), ("\U000E007F", "tag")]),
    ("tags-after-letter", [("a", "keep"), ("\U000E0067", "tag"), ("b", "keep")]),
    ("vs16-single", [("☺️", "keep")]),
    ("vs15-single", [("☺︎", "keep")]),
    ("vs-run-4", [("a️️️️", "keep")]),
    ("vs-run-5", [("a", "keep"), ("️️️️️", "vsRun"), ("b", "keep")]),
    ("vs-supplement-run", [("c", "keep"), ("\U000E0100" * 6, "vsRun")]),
    ("tab-lf-kept", [("a\tb\nc", "keep")]),
    ("cr", [("a", "keep"), ("\r", "cr"), ("b", "keep")]),
    ("c0", [("a", "keep"), ("\x00", "control"), ("b", "keep"), ("\x07", "control"), ("\x1b", "control"),
            ("[31m", "keep")]),
    ("del-c1", [("d", "keep"), ("\x7f", "control"), ("\x85", "control"), ("\x9b", "control"), ("e", "keep")]),
    ("line-separators", [("a b c", "keep")]),
    ("marker-literal", [("⟨U+202E⟩ ⟨one-time code hidden⟩", "keep")]),
    ("combining-flood", [("a" + "́" * 40, "keep")]),
    ("skin-tone", [("\U0001F44D\U0001F3FD", "keep")]),
    ("mixed", [("‮", "bidi"), ("SYNTH", "keep"), ("​", "invisible"), ("\x1b", "control"),
               ("‍", "joiner"), ("end", "keep")]),
]


def hidden_vectors():
    out = []
    names = set()
    for name, pieces in HIDDEN_VECTORS:
        assert name not in names, name
        names.add(name)
        for text, cls in pieces:
            assert cls in HIDDEN_CLASSES, (name, cls)
            assert text, name
        text = "".join(p for p, _ in pieces)
        copy = "".join(p for p, c in pieces if c in ("keep", "cr"))
        out.append({"name": name, "pieces": [{"text": p, "class": c} for p, c in pieces],
                    "text": text, "withoutHiddenCharacters": copy,
                    "markedCodePoints": sorted({f"U+{ord(ch):04X}" for p, c in pieces if c != "keep"
                                                for ch in p})})
    return {
        "format": "oma-hidden-vectors", "formatVersion": 1,
        "rule": ("Classes: keep (not marked, kept by the copy), bidi, invisible, joiner, tag and vsRun "
                 "(hidden characters: marked by the export, dropped by \"Copy text without invisible "
                 "characters\"), control (C0 and C1 other than TAB, LF and CR, and DEL: marked as a "
                 "control, dropped by the copy) and cr (kept by the copy, marked as a control by the "
                 "export's text formats). A ZWJ or ZWNJ is kept only between two non-ASCII letters, marks "
                 "or emoji; tag characters only inside a flag's tag sequence; variation-selector runs of "
                 "up to 4. U+FFFC is marked here because no segment resolved it."),
        "vectors": out,
    }


# --------------------------------------------------------------------------
# Entries, scenarios and expectations
# --------------------------------------------------------------------------

class Entry(mi.Entry):
    pass


def catalogue():
    d = mi.dt_utc
    books = {}

    def entry(name, build, day, purpose):
        book = Book()
        books[name] = book
        return Entry(name, build(book), DEV_E, d(2026, 7, day, 10), d(2026, 7, day, 10), purpose=purpose)

    entries = [
        entry("export-text-plain", content_text, 1,
              "Hostile text for every format: OWASP formula prefixes (after whitespace, the BOM and "
              "default-ignorables; full-width and small forms; on a second line) in bodies, addresses and "
              "a conversation name; quotes, commas, CRLF, lone CR, NUL, ESC, DEL and C1; U+2028, U+2029 and "
              "U+0085 in bodies, a sender and a conversation name; literal markers; header-forgery attempts; "
              "a 10,000-mark combining run; a body over 32,768 UTF-16 units with an emoji across unit "
              "32,767; an address and a conversation name over 256 units; a 100-byte reaction emoji; "
              "RTL-first, Arabic, Devanagari, Thai, Kana, Hangul, Han and emoji (ZWJ, VS15, VS16, a tag "
              "flag); and the viewer catalogue's hostile texts except its 1 MiB body"),
        entry("export-records-plain", content_records, 2,
              "Record cases: a one-time code and a mention, an inline emoji and an unmatched placeholder, a "
              "transcription; is_from_me = 2, an unknown item type and group action, NULL and recorded "
              "balloon_bundle_id; plain-column text that differs (plain, with a code, with an unsent "
              "part); decoded edit text beside an unsent part; scheduled; tied dates; every Cocoa date "
              "window (seconds, nanoseconds, zero, negative, pre-1900, ambiguous, year 9999, NULL); a "
              "message in two conversations and one in none; and patches for codes an import cannot write"),
        entry("export-oldcols-plain", content_oldcols, 3,
              "`balloon_bundle_id` absent from the backup (absent, not NULL)"),
        entry("export-files-plain", content_files, 4,
              "Attachment names and types: allowlisted and mismatched types, names differing by case and "
              "by NFC/NFD, Windows reserved names, over 255 bytes, trailing dots and spaces, invisible "
              "characters, U+202E, formula prefixes, traversal and separators, controls, empty and NULL "
              "names, two names sharing one blob, a store file the test corrupts, and the attachment of "
              "an unsent part"),
        entry("export-hist-1-plain", lambda b: content_hist(1, b), 5,
              "History, first backup: every part present, subjects and a conversation name"),
        entry("export-hist-2-plain", lambda b: content_hist(2, b), 6,
              "History, later backup: part 1 unsent on three messages (earlier text, the subject and the "
              "attachment of the unsent part withheld), decoded edit text beside an unsent part, a plain "
              "edit, a changed visible subject and conversation name"),
    ]
    return entries, books


SCENARIOS = [
    ("history", [[("export-hist-1-plain", "L1", {}), ("export-hist-2-plain", "L1", {})]],
     "Both history backups: earlier text, subject history, decoded edit text and the attachment of an "
     "unsent part, each captured before the unsend; value history of a subject and a conversation name"),
]


def membership(content):
    chats = {c["ref"]: [] for c in content["chats"]}
    nowhere = []
    for msg_ in content["messages"]:
        for c in msg_["chats"]:
            chats[c].append(msg_["ref"])
        if not msg_["chats"]:
            nowhere.append(msg_["ref"])
    return {"chats": {c: {"items": len(v), "messages": v} for c, v in sorted(chats.items())},
            "noConversation": nowhere}


def export_doc(book, content, viewer, extra_books=()):
    sentinels = {k: list(v) for k, v in book.sentinels.items()}
    visible = list(book.visible)
    unsent = list(book.unsent_files)
    for b in extra_books:
        for k, v in b.sentinels.items():
            sentinels[k] += v
        visible += b.visible
        unsent += b.unsent_files
    # The viewer model's masked strings are export sentinels too (D115).
    for s in viewer["masked"]["oneTimeCodes"]:
        if s not in sentinels["oneTimeCodes"]:
            sentinels["oneTimeCodes"].append(s)
    for s in viewer["masked"]["earlierText"]:
        if s not in sentinels["earlierText"]:
            sentinels["earlierText"].append(s)
    for k, v in sentinels.items():
        for s in v:
            assert len(s) >= 12 and not s.isdigit() and s.startswith("SYNTH"), (k, s)
    for s in visible:
        for v in sentinels.values():
            assert all(s not in x and x not in s for x in v), s
    return {
        "format": "oma-export-expectations", "formatVersion": 1,
        "sentinels": {k: sorted(set(v)) for k, v in sentinels.items()},
        "visible": sorted(set(visible)),
        "unsentAttachments": sorted(unsent, key=lambda x: x["attachment"]),
        "hostile": book.hostile,
        "formulaCases": book.formula,
        "fileCases": getattr(book, "file_cases", []),
        "membership": membership(content),
        "patches": book.patches,
        "corruptStoreFiles": book.corrupt,
    }


def is_expected(rel):
    first = rel.split("/", 1)[0]
    return first.endswith(".expected") or first == "scenarios"


README = """\
# Synthetic export catalogue (M6)

Test input for the exports (M6 spec §14, §15). **Every value in these
files is invented**: addresses under `example.invalid`, `+1555…` numbers,
and sentinel text (`SYNTH-EXP-…`). No real device, person, message, backup
or file was used. Generated by `scripts/fixtures/make_synthetic_exports.py`
(which imports the viewer and import catalogues' generators as modules); do
not edit by hand.

## Layout

```text
<entry>/SYNTHETIC0-0000000000000000/  one backup (the import input)
<entry>.expected/content.json         the import and viewer model after importing <entry> alone
<entry>.expected/export.json          the export expectations for <entry>
scenarios/<name>.content.json         the models after a sequence of imports
scenarios/<name>.export.json          the export expectations for that sequence
hidden-vectors.json                   the hidden-character vectors (M6 spec §4.7)
SHA256SUMS                            the inputs: every backup file and hidden-vectors.json
EXPECTED.SHA256SUMS                   every file under *.expected/ and scenarios/
```

There are two pins, as for the import and viewer catalogues. `SHA256SUMS`
covers the inputs and is frozen once committed; `EXPECTED.SHA256SUMS`
covers the model's expectations, which may be regenerated when a slice
settles a rule (the commit says which rule changed). The SHA-256 of each is
pinned in `crates/oma-archive/tests/export_fixtures.rs`. Expected export
outputs (`expected/<scenario>/…`, M6 spec §14.3) arrive with the writers.

## The statements

- `content.json` is the viewer catalogue's document format
  (`tests/fixtures/viewer/README.md`): `content`, the archive the M4 import
  must leave, and `viewer`, what the M5 viewer derives from it, with the
  one-time codes and earlier text it masks.
- `export.json` is this generator's own statement about the same inputs:
  - `sentinels`: distinctive strings (never short digit runs) that must
    appear in no export unless the matching opt-in is on, by kind:
    `oneTimeCodes`, `earlierText`, `editText` (decoded edit text of a
    message with an unsent part), `alternativeText` (the plain column when
    it differs and the body has a code or an unsent part),
    `subjectHistory` (of an unsent message) and `unsentAttachmentFiles`
    (bytes inside the file of an unsent part's attachment). The viewer
    model's masked strings are included.
  - `visible`: strings that an export with no opt-in must contain.
  - `unsentAttachments`: the attachment GUIDs (and file digests) whose
    files are withheld by default.
  - `hostile`, `formulaCases`, `fileCases`: each planted string, where it
    was planted (`body`, `address`, `chatName`, `fileName`,
    `associationEmoji`), and for formulas the leading character and what
    precedes it. Strings over 4096 UTF-8 bytes are given by SHA-256,
    length and their first 32 characters.
  - `membership`: each conversation's messages (refs as in `content`) and
    the messages in no conversation.
  - `patches` and `corruptStoreFiles`: values an import cannot produce
    (unknown codes and states, a store file that no longer matches its
    name), applied by a test to a copy of the imported archive.
- `hidden-vectors.json`: strings split into classed pieces. The Vitest
  `hidden-vectors.spec.ts` checks the viewer's "Copy text without
  invisible characters" against it; the export's classifier (`hidden.rs`)
  is checked against the same file.

## Coverage notes

- **Volume** cases (over 10,000 reactions on one target, over 50 versions,
  over 200 edit entries, 200 × 16 MiB versions) are generated at test time
  (`tests/export_reads.rs`, `tests/export_memory.rs`), not committed.
- **Unknown codes and states** (`_st`, units, item kinds) cannot come from
  a backup, so they are the `patches` above.
- The viewer catalogue (`tests/fixtures/viewer/`) and `golden-v1` stay
  inputs as they are; the export catalogue adds to them.

## Regenerating (reference environment only)

```sh
python3 -m venv .venv-fixtures
.venv-fixtures/bin/pip install -r scripts/fixtures/requirements.txt
.venv-fixtures/bin/python -I -c "import sqlite3; print(sqlite3.sqlite_version)"   # must print 3.45.1
.venv-fixtures/bin/python -I scripts/fixtures/make_synthetic_exports.py /tmp/export --seed 1
rm -rf tests/fixtures/export && cp -r /tmp/export tests/fixtures/export
sha256sum tests/fixtures/export/SHA256SUMS tests/fixtures/export/EXPECTED.SHA256SUMS
```

The script refuses to run under any other SQLite. Windows is not a
generation environment.

| | |
|---|---|
| Python | {python} |
| SQLite | {sqlite} |
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
    a = ap.parse_args()
    print(f"python {sys.version.split()[0]}, sqlite {sqlite3.sqlite_version}")
    if sqlite3.sqlite_version != REFERENCE_SQLITE:
        sys.exit(f"refusing to write: sqlite {sqlite3.sqlite_version} is not the reference {REFERENCE_SQLITE}")
    out = os.path.abspath(a.out)
    os.makedirs(out, exist_ok=True)
    for item in os.listdir(out):
        p = os.path.join(out, item)
        shutil.rmtree(p) if os.path.isdir(p) else os.remove(p)
    entries, books = catalogue()
    models = {}
    for e in entries:
        rng = random.Random(f"{a.seed}:{e.name}")
        main_db, wal = e.content.built(a.seed)
        exp = os.path.join(out, e.name + ".expected")
        mi.write_backup(e, os.path.join(out, e.name), rng, main_db, wal, exp)
        models[e.name] = mi.EntryModel(e, a.seed)
        mv.ANN.update(ms.annotations(e.content.spec))
    for e in entries:
        steps = [(e.name, "L1", {})]
        content, viewer = mv.run_steps(steps, models)
        exp = os.path.join(out, e.name + ".expected")
        mi.write_json(os.path.join(exp, "content.json"), mv.document([steps], models, content, viewer))
        book = books[e.name]
        if e.name.startswith("export-hist-"):
            book = Book()  # alone, a history backup carries no capture-before-unsend
        mi.write_json(os.path.join(exp, "export.json"), export_doc(book, content, viewer))
        size = sum(os.path.getsize(os.path.join(dp, n))
                   for d in (os.path.join(out, e.name), exp)
                   for dp, _, ns in os.walk(d) for n in ns)
        assert size < 4_000_000, (e.name, size)
        print(f"wrote {e.name} ({size} bytes)")
    for name, orders, _ in SCENARIOS:
        results = [mv.run_steps(steps, models) for steps in orders]
        for r in results[1:]:
            assert r == results[0], name
        content, viewer = results[0]
        mi.write_json(os.path.join(out, "scenarios", f"{name}.content.json"),
                      mv.document(orders, models, content, viewer))
        book = Book()
        hist_book(book)
        mi.write_json(os.path.join(out, "scenarios", f"{name}.export.json"),
                      export_doc(book, content, viewer))
        print(f"wrote scenario {name}")
    mi.write_json(os.path.join(out, "hidden-vectors.json"), hidden_vectors())
    mi.write_sums(out, "SHA256SUMS", lambda rel: not is_expected(rel))
    mi.write_sums(out, "EXPECTED.SHA256SUMS", is_expected)
    import cryptography
    ent = "\n".join(f"| `{e.name}` | {e.purpose} |" for e in entries)
    scn = "\n".join(f"| `{n}` | {len(o)} | {why} |" for n, o, why in SCENARIOS)
    with open(os.path.join(out, "README.md"), "w", encoding="utf-8", newline="\n") as f:
        text = README
        for k, v in (("python", sys.version.split()[0]), ("sqlite", sqlite3.sqlite_version),
                     ("cryptography", cryptography.__version__), ("seed", str(a.seed)),
                     ("entries", ent), ("scenarios", scn)):
            text = text.replace("{" + k + "}", v)
        f.write(text)
    for n in ("SHA256SUMS", "EXPECTED.SHA256SUMS"):
        with open(os.path.join(out, n), "rb") as f:
            print(f"{n} sha256 {hashlib.sha256(f.read()).hexdigest()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
