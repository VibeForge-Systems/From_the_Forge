#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""Build the SYNTHETIC viewer catalogue for OpenMessage Archive (M5).

Every value written here is invented: example.invalid addresses, +1555
numbers, and sentinel text (SYNTH-VIEW-..., SYNTH-HX-..., SYNTH-MEDIA-...,
SYNTH-LIN-..., SYNTH-OTC-...). No real device, person, message, backup or
photo is used, and nothing from any sample database is copied.

Usage (the media files come first, from the Rust example):

  cargo run -p oma-archive --example make_viewer_media -- OUT_DIR/media
  make_synthetic_viewer.py OUT_DIR [--seed N]

Writes, under OUT_DIR (beside the existing media/):

  <entry>/SYNTHETIC0-0000000000000000/  one backup (the import input)
  <entry>.expected/content.json         the model after importing <entry> alone
  scenarios/<name>.content.json         the model after a sequence of imports
  SHA256SUMS                            the inputs: media/ and every backup file
  EXPECTED.SHA256SUMS                   every file under *.expected/ and scenarios/
  README.md                             generated; do not edit

Each content.json holds two statements by this generator's own model:
"content", the archive the M4 import must leave (computed by the M4
generator's model, make_synthetic_imports.py, imported as a module), and
"viewer", what the M5 viewer must derive from that archive (conversation
order, thread timelines with folded reactions, reaction chips, reply
resolution, direction and sender reasons, edits and unsends, withheld and
masked text, labels, and attachment sniffing, structure checks and cards;
M5 spec sections 3.3, 3.5, 3.6, 3.8, 5.1-5.3 and 5.9). The viewer model is
a second statement of the spec's rules applied to the model archive; it
never reads Rust output. Agreement proves self-consistency only.

SQLite file bytes depend on the SQLite version, so the script refuses to
run under any SQLite but the reference 3.45.1 (there is no override). Run
with `python -I`.
"""
import argparse
import datetime as dt
import hashlib
import json
import os
import random
import shutil
import sqlite3
import struct
import sys
import unicodedata

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.dont_write_bytecode = True  # importing the sibling generators leaves no __pycache__

import make_synthetic_imports as mi  # noqa: E402
import make_synthetic_messages as ms  # noqa: E402
import make_synthetic_typedstream as ts  # noqa: E402

REFERENCE_SQLITE = "3.45.1"
NS = ms.NS
T0_NS = ms.T0_NS
T0_S = ms.T0_S
VALUE, NULL, ABSENT, OVERSIZED, UNREADABLE = mi.VALUE, mi.NULL, mi.ABSENT, mi.OVERSIZED, mi.UNREADABLE

# 2007-01-01T00:00:00Z: dated values before it are marked (spec 3.6).
FLOOR_2007_NS = 1_167_609_600 * NS
# Caps (spec 3.5, 3.6).
REACTION_ROWS_CAP = 200
REPLY_COUNT_CAP = 1000
PREVIEW_SCAN_ROWS = 50
# Structural-check bounds (spec 5.3).
MAX_BOXES = 10_000
MAX_DEPTH = 16

MEDIA_DIR = "media"


# --------------------------------------------------------------------------
# Small helpers over the M4 model's observations
# --------------------------------------------------------------------------

def ival(o):
    """The integer of an observation, or None."""
    return o[1][1] if o[0] == VALUE and o[1][0] == "int" else None


def bval(o):
    """The bytes of an observation, or None."""
    return o[1][1] if o[0] == VALUE and o[1][0] == "bytes" else None


def tval(o):
    """(unit, unix ns) of a time observation, or None."""
    return (o[1][2], o[1][3]) if o[0] == VALUE and o[1][0] == "time" else None


def msv(o):
    """An M4 observation in the M3 model's (state, value) form."""
    if o[0] == VALUE:
        return ("value", o[1][1])
    return (o[0], None)


def text_of(b):
    """Display form of stored bytes: the text, or lossy text when not UTF-8."""
    return b.decode("utf-8", "replace")


def utf16_slice(text, start, end):
    """text[start:end] in UTF-16 code units."""
    enc = text.encode("utf-16-le")
    return enc[2 * start:2 * end].decode("utf-16-le")


def col(row, name):
    return row.cols.get(name, (ABSENT, None))


# --------------------------------------------------------------------------
# Rows
# --------------------------------------------------------------------------

def msg(n, prefix, text_prefix, base_ns=T0_NS, **kw):
    """Message n: rowid n, GUID prefix+n, date base + n minutes."""
    r = ms.msg(n, guid=f"{prefix}{n:04d}", date=base_ns + n * 60 * NS, text=f"{text_prefix}-{n}")
    r.update(kw)
    return r


def H(rowid, ident, service="iMessage"):
    return mi.H(rowid, ident, service)


def rows_of(handles, chats, msgs, chat_handles, chat_msgs, msg_atts=(), atts=(), rd=()):
    return {
        "handle": list(handles),
        "chat": list(chats),
        "message": list(msgs),
        "chat_handle_join": [{"chat_id": c, "handle_id": h} for c, h in chat_handles],
        "chat_message_join": [{"chat_id": c, "message_id": m} for c, m in chat_msgs],
        "message_attachment_join": [{"rowid": 100 + i, "message_id": m, "attachment_id": a}
                                    for i, (m, a) in enumerate(msg_atts)],
        "attachment": list(atts),
        "chat_recoverable_message_join": [
            {"chat_id": c, "message_id": m, "delete_date": T0_NS + 5000 * NS, "ck_sync_state": 0}
            for c, m in rd],
        "recoverable_message_part": [],
        "_SqliteDatabaseProperties": [{"key": "_ClientVersion", "value": "17001"}],
    }


def media_bytes(out, name):
    with open(os.path.join(out, MEDIA_DIR, name), "rb") as f:
        return f.read()


# attributedBody, summary and `ec` text blobs by their bytes (the model's
# expected decode results).
ANN = {}


def ec_text(text):
    """An `ec` entry's `t`: an attributed string (typedstream), decoded at
    read time (spec 3.7)."""
    ab = ms.ab_plain(text)
    ANN[ab.data] = ab
    return ab.data


# ---- view-main ---------------------------------------------------------------

VP = "SYNTH-VIEW-MSG-"
VB = "SYNTH-VIEW-BODY"


def vguid(n):
    return f"{VP}{n:04d}"


def content_main(out):
    sticker = media_bytes(out, "png-alpha-4x4.png")
    m = lambda n, **kw: msg(n, VP, VB, **kw)
    react = lambda n, kind, target, **kw: m(
        n, associated_message_type=kind, associated_message_guid=target, text=None,
        associated_message_range_location=0, associated_message_range_length=1, **kw)
    t = lambda n: T0_NS + n * 60 * NS
    handles = [
        H(1, "synthetic-viewer-1@example.invalid"),
        H(2, "+15550000101"),
        H(3, "+15550000102"),
        H(4, "+15550000103", "SMS"),
        H(5, "synthetic-viewer-5@example.invalid"),
    ]
    chats = [
        mi.C(1, "iMessage;-;synthetic-viewer-1@example.invalid", ident="synthetic-viewer-1@example.invalid"),
        mi.C(2, "iMessage;+;chatSYNTHVIEW2", style=43, ident="chatSYNTHVIEW2", room="chatSYNTHVIEW2",
             name="SYNTH-VIEW-CHAT-GROUP"),
        mi.C(3, "SMS;-;+15550000103", ident="+15550000103", service="SMS"),
        mi.C(4, "iMessage;-;synthetic-viewer-5@example.invalid", ident="synthetic-viewer-5@example.invalid"),
        mi.C(5, "iMessage;-;+15550000102", ident="+15550000102"),
        mi.C(6, "iMessage;-;chatSYNTHVIEW6", style=45, ident="chatSYNTHVIEW6"),
    ]
    otc_ab = ms.ab_text([
        ("Your code is ", [(ts.P, 0)]),
        ("SYNTH-OTC-482913", [(ts.P, 0), (ts.OTC, 1)]),
        (", ", [(ts.P, 0)]),
        ("SYNTH-BOLD", [(ts.P, 0), (ts.BOLD, 1)]),
        (" for ", [(ts.P, 0)]),
        ("SYNTH-MENTION", [(ts.P, 0), (ts.MENTION, "synthetic-viewer-1@example.invalid")]),
    ])
    otc_text = "Your code is SYNTH-OTC-482913, SYNTH-BOLD for SYNTH-MENTION"
    # Part 1 was unsent: the current body holds parts 0 and 2 only.
    three_parts = ms.ab_text([("SYNTH-VIEW-PART-0 ", [(ts.P, 0)]), ("SYNTH-VIEW-PART-2", [(ts.P, 2)])])
    inline_ab = ms.ab_text([
        ("SYNTH-VIEW-INLINE ", [(ts.P, 0)]),
        ("￼", [(ts.P, 1), (ts.EMOJI, 1)]),
        (" and ", [(ts.P, 2)]),
        ("￼", [(ts.P, 3), (ts.GUID, "SYNTH-VIEW-ATT-NOT-A-FILE")]),
    ])
    msgs = [
        # c1: targets, then every association kind.
        m(1, handle_id=1, attributedBody=ms.ab_plain(f"{VB}-1"), text=f"{VB}-1"),
        m(2, is_from_me=1, handle_id=1),
        react(3, 2000, f"p:0/{vguid(1)}", handle_id=1),                       # love by h1
        react(4, 2001, f"p:0/{vguid(1)}", is_from_me=1, handle_id=1),         # like by me ...
        react(5, 3001, f"p:0/{vguid(1)}", is_from_me=1, handle_id=1),         # ... removed later
        react(6, 2002, f"p:0/{vguid(2)}", handle_id=1, date=t(6)),            # dislike, tied with 7
        react(7, 2002, f"p:0/{vguid(2)}", handle_id=1, date=t(6)),
        react(8, 2003, f"p:0/{vguid(2)}", handle_id=1, date=0),               # laugh, undated
        react(9, 2004, f"p:0/{vguid(1)}", handle_id=0),                       # sender not recorded
        react(10, 2006, f"p:0/{vguid(1)}", handle_id=1, associated_message_emoji="\U0001F525"),
        react(11, 3006, f"p:0/{vguid(1)}", handle_id=1, associated_message_emoji="\U0001F525"),
        react(12, 3006, f"p:0/{vguid(2)}", handle_id=1),                      # emoji removal, no emoji
        react(13, 2006, f"p:0/{vguid(2)}", is_from_me=1, handle_id=1, associated_message_emoji="\U0001F44D"),
        react(14, 2007, f"p:0/{vguid(1)}", handle_id=1, cache_has_attachments=1),  # sticker tapback
        react(15, 2005, vguid(1), is_from_me=1, handle_id=1),                 # question, bare target
        react(16, 1000, f"p:0/{vguid(1)}", handle_id=1, cache_has_attachments=1),  # legacy sticker
        react(17, 4000, f"p:0/{vguid(1)}", handle_id=1),                      # poll vote
        react(18, 2500, f"p:0/{vguid(1)}", handle_id=1),                      # unknown type
        react(19, "SYNTH-UNREADABLE-TYPE", None, handle_id=1),                # assoc_present = 2
        react(20, None, f"p:0/{vguid(1)}", handle_id=1),                      # present, no kind
        react(21, 2000, f"p:0/{vguid(40)}", handle_id=1),                     # target in c2
        react(22, 2000, "p:0/SYNTH-VIEW-MISSING-GUID", handle_id=1),          # unresolved
        react(23, 2000, "p:x/SYNTH-VIEW-UNPARSED", handle_id=1),              # unparsed
        # replies
        m(24, handle_id=1, thread_originator_guid=vguid(1), thread_originator_part="0:0:16"),
        m(25, is_from_me=1, handle_id=1, thread_originator_guid="SYNTH-VIEW-MISSING-ORIGIN"),
        m(26, handle_id=1, thread_originator_guid=vguid(41)),
        m(27, handle_id=1, thread_originator_guid="SYNTH-VIEW-DUP-GUID"),
        m(28, handle_id=1, guid="SYNTH-VIEW-DUP-GUID", text=f"{VB}-28 (unkeyed)"),
        m(29, handle_id=1, guid="SYNTH-VIEW-DUP-GUID", text=f"{VB}-29 (unkeyed)"),
        # edits and unsends
        m(30, is_from_me=1, handle_id=1, text=f"{VB}-30 edited", date_edited=t(30) + 30 * NS,
          message_summary_info=ms.summary(otr=1, ec={0: [{"d": T0_S + 1800, "t": ec_text(f"{VB}-30 original")}]})),
        m(31, is_from_me=1, handle_id=1, text=None, attributedBody=three_parts, part_count=3,
          date_edited=t(31) + 30 * NS, message_summary_info=ms.summary(otr=3, rp=[1])),
        m(32, is_from_me=1, handle_id=1, text=None, date_edited=t(32) + 30 * NS,
          message_summary_info=ms.summary(otr=1, rp=[0])),
        m(33, handle_id=1, date_edited=t(33) + 30 * NS),                       # code 7
        m(34, handle_id=1, text="", date_edited=t(34) + 30 * NS),             # code 6
        m(35, handle_id=1, message_summary_info=ms.summary_raw(b"SYNTH-NOT-A-PLIST", "PlistInvalid")),
        m(36, handle_id=1, date_edited=-5),                                    # code 9
        # formatting and placement
        m(37, handle_id=1, text=otc_text, attributedBody=otc_ab),
        m(38, handle_id=1, text=f"{VB}-38", attributedBody=ms.ab_vector("h_truncated")),
        m(39, handle_id=1, text=None, attributedBody=inline_ab, part_count=4),
        m(42, is_from_me=1, handle_id=1, schedule_type=2, schedule_state=1),
        m(43, handle_id=1, date=100_000_000),                                  # 2004: before 2007
        m(44, handle_id=1, date=0),                                            # undated
        m(45, handle_id=1, date=-60),                                          # undated (negative)
        m(46, handle_id=1, text=f"{VB}-46 in two chats"),
        m(47, is_from_me=1, handle_id=1, service="SMS"),                       # service change
        m(48, handle_id=1, is_from_me=None),                                   # direction not recorded
        m(49, handle_id=0),                                                    # no sender
        m(50, handle_id=999),                                                  # sender not found
        m(51, handle_id="SYNTH-UNREADABLE-HANDLE"),                            # unreadable
        m(52, handle_id=1, balloon_bundle_id="com.example.synthetic.app", payload_data=b"\x00SYNTH-PAYLOAD"),
        m(53, is_from_me=1, handle_id=1, expressive_send_style_id="com.example.synthetic.effect"),
        m(54, handle_id=1, text=f"{VB}-54", is_audio_message=1,
          attributedBody=ms.ab_text([(f"{VB}-54", [(ts.P, 0), ("IMAudioTranscription", 1)])])),
        m(55, handle_id=1, text=f"{VB}-55 plain column",
          attributedBody=ms.ab_plain(f"{VB}-55 formatted column")),            # sources differ
        m(56, handle_id=1, text=None),                                         # no source
        m(57, handle_id=1, text=""),                                           # text empty
        m(58, handle_id=1, text=None, attributedBody=ms.ab_vector("h_truncated")),
        m(59, handle_id=1, text=ms.TextBytes(b"SYNTH-VIEW-BODY-59 \xc3\x28 lossy")),
        m(60, handle_id=1, text="see https://example.invalid/SYNTH-PATH?q=1 ok"),
        m(61, handle_id=1, text="SYNTH-VIEW-URL https://example.invalid/‮SYNTH-gpj.exe"),
        m(62, handle_id=1, text="Hi", attributedBody=ms.ab_vector("h_bad_run_index")),  # run issue
        # c2 (a group by address count)
        m(40, handle_id=2),
        m(41, handle_id=3),
        m(70, item_type=1, group_action_type=0, handle_id=1, other_handle=2, text=None),
        m(71, item_type=1, group_action_type=0, handle_id=2, other_handle=2, text=None),
        m(72, item_type=1, group_action_type=1, handle_id=1, other_handle=3, text=None),
        m(73, item_type=2, group_title="SYNTH-VIEW-GROUP-TITLE", handle_id=2, text=None),
        m(74, item_type=3, group_action_type=0, handle_id=3, text=None),
        m(75, item_type=3, group_action_type=1, handle_id=2, text=None),
        m(76, item_type=3, group_action_type=2, handle_id=2, text=None),
        m(77, item_type=3, group_action_type=4, handle_id=2, text=None),
        m(78, item_type=3, group_action_type=6, handle_id=2, text=None),
        m(79, item_type=4, handle_id=2, other_handle=1, text=None),
        m(80, item_type=5, handle_id=2, text=None),
        m(81, item_type=6, handle_id=2, text=None),
        m(82, item_type=9, group_action_type=7, handle_id=2, text="SYNTH-VIEW-UNKNOWN-ITEM"),
        m(83, item_type=3, group_action_type=99, handle_id=2, text=None),
        m(84, handle_id=2),
        m(85, is_from_me=1, handle_id=0),
        # c3: undated only
        m(86, handle_id=4, service="SMS", date=0),
        m(87, is_from_me=1, handle_id=4, service="SMS", date=0),
        # c4: one target with 201 reactions
        m(90, handle_id=5),
        # c6: style 45, two addresses
        m(95, handle_id=1),
        m(96, handle_id=2),
    ]
    for i in range(201):
        msgs.append(react(100 + i, 2000, f"p:0/{vguid(90)}", handle_id=5))
    c1 = [n for n in list(range(1, 40)) + list(range(42, 63)) if n not in (40, 41)]
    c2 = [40, 41, 46] + list(range(70, 86))
    chat_msgs = ([(1, n) for n in c1] + [(2, n) for n in c2] + [(3, 86), (3, 87)]
                 + [(4, n) for n in [90] + list(range(100, 301))] + [(6, 95), (6, 96)])
    p14 = "~/Library/SMS/StickerCache/v1/SYNTH-VIEW-S14/SYNTH-VIEW-STICKER-14.png"
    p16 = "~/Library/SMS/Attachments/v1/16/SYNTH-VIEW-S16/SYNTH-VIEW-STICKER-16.png"
    atts = [
        mi.A(1, "SYNTH-VIEW-ATT-0014", p14, len(sticker), is_sticker=1, uti="public.png",
             mime_type="image/png"),
        mi.A(2, "SYNTH-VIEW-ATT-0016", p16, len(sticker), is_sticker=1, uti="public.png",
             mime_type="image/png"),
    ]
    files = [mi.AttFile(mi.media(p14), sticker), mi.AttFile(mi.media(p16), sticker)]
    rows = rows_of(handles, chats, msgs,
                   [(1, 1), (2, 1), (2, 2), (2, 3), (3, 4), (4, 5), (5, 3), (6, 1), (6, 2)],
                   chat_msgs, [(14, 1), (16, 2)], atts)
    return mi.Content("view-main", mi.import_profile(), rows, files)


# ---- view-hostile-text --------------------------------------------------------

HXP = "SYNTH-HX-MSG-"
LONG_LOCAL = "synthetic-" + "a" * 300
LARGE_BODY_BYTES = 1 << 20


def large_body():
    head = "SYNTH-HX-LARGE "
    unit = "0123456789abcdef"
    body = head + unit * ((LARGE_BODY_BYTES - len(head)) // len(unit) + 1)
    return body[:LARGE_BODY_BYTES]


HOSTILE_TEXTS = [
    ("html script", "<script>alert('SYNTH-HX')</script>"),
    ("img onerror", "<img src=x onerror=alert('SYNTH-HX')>"),
    ("template", "{{7*7}} SYNTH-HX-TEMPLATE ${7*7}"),
    ("RLO", "SYNTH-HX-RLO ‮gnirts desrever"),
    ("LRI/RLI/PDI", "SYNTH-HX-ISOLATES ⁦a⁩ ⁧b⁩"),
    ("ZWJ/ZWSP/ZWNJ", "SYNTH-HX-ZERO‍WIDTH​‌JOINERS"),
    ("BOM", "﻿SYNTH-HX-BOM"),
    ("object replacement", "SYNTH-HX-OBJECT ￼ in text"),
    ("tag characters", "SYNTH-HX-TAGS \U000E0041\U000E0042\U000E007F"),
    ("NUL", "SYNTH-HX-NUL\x00AFTER-NUL"),
    ("zalgo", "SYNTH-HX-ZALGO Z" + "̶̷̸̐̑" * 40),
    ("noncharacters", "SYNTH-HX-NONCHAR ﷐ and ﷑"),
    ("URL with RLO", "SYNTH-HX-URL https://example.invalid/‮SYNTH-gpj.exe"),
    ("variation selectors", "SYNTH-HX-VS a️️️️️"),
    ("CJK", "SYNTH-HX-CJK 東京タワーに行きました"),
    ("Thai", "SYNTH-HX-THAI ภาษาไทยเป็นภาษา"),
    ("Korean", "SYNTH-HX-KOREAN 한국어 문장입니다"),
    ("emoji only", "\U0001F600\U0001F44D\U0001F3FD\U0001F468‍\U0001F469‍\U0001F467"),
    ("sharp s", "SYNTH-HX-SHARP Straße"),
    ("NFD", "SYNTH-HX-NFD Café"),
    ("100,000-character token", "SYNTH" + "x" * 99_995),
    ("1 MiB body", large_body()),
]


def content_hostile():
    m = lambda n, **kw: msg(n, HXP, "SYNTH-HX-BODY", **kw)
    handles = [
        H(1, ms.TextBytes(b"synthetic-\xff\xfe@example.invalid")),
        H(2, LONG_LOCAL + "-one@example.invalid"),
        H(3, LONG_LOCAL + "-two@example.invalid"),
        H(4, "synthetic-‮ltr@example.invalid"),
    ]
    chats = [
        mi.C(1, "iMessage;-;SYNTH-HX-CHAT-1", ident="SYNTH-HX-CHAT-1",
             name=ms.TextBytes(b"SYNTH-HX-CHAT-\xff-NAME")),
        mi.C(2, "iMessage;+;chatSYNTHHX2", style=43, ident="chatSYNTHHX2",
             name="SYNTH-HX-CHAT-‮EMAN"),
    ]
    msgs = []
    for i, (_, text) in enumerate(HOSTILE_TEXTS, 1):
        msgs.append(m(i, handle_id=1, text=text))
    n = len(HOSTILE_TEXTS)
    # 10,000 formatting runs in one body, alternating bold and plain. Each
    # run holds 16 characters so the decode stays inside the typedstream
    # decoder's materialisation budget (twice the input plus 64 KiB): with
    # two characters per run it refuses the body (MaterialisationBudget)
    # and the text column is shown instead.
    run_text = "abcdefghijklmnop"
    segs = [(run_text, [(ts.P, 0), (ts.BOLD, 1)] if k % 2 else [(ts.P, 0)]) for k in range(10_000)]
    runs_ab = ms.ab_text(segs)
    msgs.append(m(n + 1, handle_id=1, text=run_text * 10_000, attributedBody=runs_ab))
    msgs.append(m(n + 2, handle_id=1, text=ms.TextBytes(b"SYNTH-HX-BAD-\xc3\x28-UTF8")))
    msgs.append(m(n + 3, handle_id=1, cache_has_attachments=1))
    msgs.append(m(n + 4, handle_id=2))
    msgs.append(m(n + 5, handle_id=3))
    msgs.append(m(n + 6, handle_id=4))
    p_rlo = "~/Library/SMS/Attachments/hx/01/SYNTH-HX-A1/SYNTH-HX-FILE-1.exe"
    atts = [mi.A(1, "SYNTH-HX-ATT-0001", p_rlo, 42, transfer_name="SYNTH-FILE-‮gpj.exe",
                 mime_type=None, uti=None)]
    chat_msgs = [(1, k) for k in range(1, n + 4)] + [(2, n + 4), (2, n + 5), (2, n + 6)]
    rows = rows_of(handles, chats, msgs, [(1, 1), (2, 2), (2, 3), (2, 4)], chat_msgs,
                   [(n + 3, 1)], atts)
    return mi.Content("view-hostile-text", mi.import_profile(), rows)


# ---- view-oldcols: columns absent --------------------------------------------

def content_oldcols():
    m = lambda n, **kw: msg(n, "SYNTH-OLD-MSG-", "SYNTH-OLD-BODY", **kw)
    drop = [("message", "handle_id"), ("message", "date_edited"), ("message", "message_summary_info")]
    rows = rows_of([H(1, "synthetic-old-1@example.invalid")],
                   [mi.C(1, "iMessage;-;synthetic-old-1@example.invalid",
                         ident="synthetic-old-1@example.invalid")],
                   [m(1), m(2, is_from_me=1)], [(1, 1)], [(1, 1), (1, 2)])
    return mi.Content("view-oldcols", mi.import_profile(drop), rows)


# ---- view-media ----------------------------------------------------------------

# (file, declared mime, declared uti). The declared type is a label only.
MEDIA_ATTACHMENTS = [
    ("png-rgb-6x4.png", "image/png", "public.png"),
    ("png-alpha-4x4.png", "image/png", "public.png"),
    ("jpeg-exif6-gps-8x6.jpg", "image/jpeg", "public.jpeg"),
    ("jpeg-truncated.jpg", "image/jpeg", "public.jpeg"),
    ("gif-2frames-4x4.gif", "image/gif", "com.compuserve.gif"),
    ("webp-lossless-4x4.webp", "image/webp", "org.webmproject.webp"),
    ("png-bomb-12000.png", "image/png", "public.png"),
    ("png-rgba16-4097.png", "image/png", "public.png"),
    ("png-ihdr-60000.png", "image/png", "public.png"),
    ("mp4-minimal.mp4", "video/mp4", "public.mpeg-4"),
    ("mov-minimal.mov", "video/quicktime", "com.apple.quicktime-movie"),
    ("m4a-minimal.m4a", "audio/x-m4a", "com.apple.m4a-audio"),
    ("wav-minimal.wav", "audio/wav", "com.microsoft.waveform-audio"),
    ("caf-header.caf", "audio/x-caf", "com.apple.coreaudio-format"),
    ("heic-header.heic", "image/heic", "public.heic"),
    ("mov-refmovie-rmra.mov", "video/quicktime", "com.apple.quicktime-movie"),
    ("mov-dref-external.mov", "video/quicktime", "com.apple.quicktime-movie"),
    ("mp4-box-past-eof.mp4", "video/mp4", "public.mpeg-4"),
    ("mp4-box-64bit-overflow.mp4", "video/mp4", "public.mpeg-4"),
    ("wav-chunk-past-eof.wav", "audio/wav", "com.microsoft.waveform-audio"),
    ("svg-script.svg", "image/svg+xml", "public.svg-image"),
    ("html-named-jpg.jpg", "image/jpeg", "public.jpeg"),
    ("gif-html-polyglot.gif", "image/gif", "com.compuserve.gif"),
    ("pdf-minimal.pdf", "application/pdf", "com.adobe.pdf"),
    ("vcard-synthetic.vcf", "text/vcard", "public.vcard"),
    ("text-plain.txt", "text/plain", "public.plain-text"),
    ("unknown-bytes.bin", "application/octet-stream", "public.data"),
    ("one-byte.bin", None, None),
    ("empty.bin", None, None),
]

MP = "SYNTH-MEDIA-MSG-"


def media_path(n, name, sticker=False):
    if sticker:
        return f"~/Library/SMS/StickerCache/m{n:02d}/SYNTH-MEDIA-{n:04d}/{name}"
    return f"~/Library/SMS/Attachments/m{n % 100:02d}/{n:02d}/SYNTH-MEDIA-{n:04d}/{name}"


def content_media(out, variant="v1"):
    """`v1`: every media file, one per message, and the card states. `v2`
    (a later backup): one file gone, one GUID with new bytes."""
    m = lambda n, **kw: msg(n, MP, "SYNTH-MEDIA-BODY", cache_has_attachments=1, handle_id=1, **kw)
    msgs, atts, files, links = [], [], [], []
    for n, (name, mime, uti) in enumerate(MEDIA_ATTACHMENTS, 1):
        data = media_bytes(out, name)
        path = media_path(n, name)
        msgs.append(m(n))
        atts.append(mi.A(n, f"SYNTH-MEDIA-ATT-{n:04d}", path, len(data), mime_type=mime, uti=uti,
                         transfer_name=name))
        links.append((n, n))
        state = "present"
        if variant == "v2" and name == "png-rgb-6x4.png":
            state = "missing"            # the newer backup lost the file: the older blob stays
        payload = data
        if variant == "v2" and name == "png-alpha-4x4.png":
            payload = media_bytes(out, "webp-lossless-4x4.webp")  # new bytes, another length
        files.append(mi.AttFile(mi.media(path), payload, state))
    base = len(MEDIA_ATTACHMENTS)
    png = media_bytes(out, "png-rgb-6x4.png")
    gif = media_bytes(out, "gif-2frames-4x4.gif")
    # Card states (spec 5.9).
    extra = [
        # (name, filename, file state or None (not in backup), kwargs, declared size)
        ("blob-missing", media_path(base + 1, "missing.png"), "missing", {}, None),
        ("not-in-backup", media_path(base + 2, "absent.png"), None, {}, None),
        ("sticker-present", media_path(base + 3, "sticker.png", True), "present",
         {"is_sticker": 1}, None),
        ("sticker-not-found", media_path(base + 4, "sticker.png", True), None, {"is_sticker": 1}, None),
        ("null-filename", None, None, {}, None),
        ("class-not-looked-up", "~/Library/Messages/Attachments/SYNTH-MEDIA-CLASS.png", None, {}, None),
        ("size-mismatch", media_path(base + 7, "size.png"), "present", {}, len(png) + 9),
        ("hidden", media_path(base + 8, "hidden.png"), "present", {"hide_attachment": 1}, None),
        ("genmoji", media_path(base + 9, "genmoji.png", True), "present",
         {"is_sticker": 1, "emoji_image_short_description": "SYNTH-GENMOJI-PROMPT-MEDIA"}, None),
        ("same-bytes", media_path(base + 10, "same.png"), "present", {}, None),
        ("inline-a", media_path(base + 11, "inline-a.png"), "present", {}, None),
        ("inline-b", media_path(base + 11, "inline-b.gif"), "present", {}, None),
        ("unmatched", media_path(base + 12, "unmatched.png"), "present", {}, None),
        ("declared-differs", media_path(base + 13, "declared.png"), "present",
         {"total_bytes": 999_999}, None),
    ]
    for i, (what, path, state, kw, declared) in enumerate(extra):
        n = base + 1 + i
        data = gif if what == "inline-b" else png
        aguid = f"SYNTH-MEDIA-ATT-{n:04d}"
        kw = dict(kw)
        kw.setdefault("mime_type", "image/png")
        kw.setdefault("uti", "public.png")
        kw.setdefault("transfer_name", f"SYNTH-MEDIA-{what}.png")
        atts.append(mi.A(n, aguid, path, len(data), **kw))
        if path is not None and state is not None:
            files.append(mi.AttFile(mi.media(path), data, state, declared=declared))
    # Messages for the extra attachments: one each, except the inline pair.
    nmsg = base + 1
    att_n = base + 1
    for what, *_ in extra:
        if what == "inline-b":
            att_n += 1
            continue
        if what == "inline-a":
            ab = ms.ab_text([
                ("SYNTH-MEDIA-INLINE ", [(ts.P, 0)]),
                ("￼", [(ts.P, 1), (ts.GUID, f"SYNTH-MEDIA-ATT-{att_n + 1:04d}")]),
                (" then ", [(ts.P, 2)]),
                ("￼", [(ts.P, 3), (ts.GUID, f"SYNTH-MEDIA-ATT-{att_n:04d}")]),
                (" emoji ", [(ts.P, 4)]),
                ("￼", [(ts.P, 5), (ts.EMOJI, 1)]),
            ])
            msgs.append(m(nmsg, text=None, attributedBody=ab, part_count=6))
            links += [(nmsg, att_n), (nmsg, att_n + 1)]
        elif what == "unmatched":
            ab = ms.ab_text([("SYNTH-MEDIA-UNMATCHED ", [(ts.P, 0)]),
                             ("￼", [(ts.P, 1), (ts.GUID, "SYNTH-MEDIA-ATT-NOT-LINKED")])])
            msgs.append(m(nmsg, text=None, attributedBody=ab, part_count=2))
            links.append((nmsg, att_n))
        else:
            msgs.append(m(nmsg))
            links.append((nmsg, att_n))
        nmsg += 1
        att_n += 1
    handles = [H(1, "synthetic-media-1@example.invalid")]
    chats = [mi.C(1, "iMessage;-;synthetic-media-1@example.invalid", ident="synthetic-media-1@example.invalid")]
    rows = rows_of(handles, chats, msgs, [(1, 1)], [(1, r["ROWID"]) for r in msgs], links, atts)
    return mi.Content(f"view-media-{variant}", mi.import_profile(), rows, files)


# ---- lineages ------------------------------------------------------------------

LP = "SYNTH-LIN-MSG-"
LB = "SYNTH-LIN-BODY"


def content_lineage(version, out):
    """Device A versions `a1`, `a2`, `a3` (WAL listed, missing), `rd`; and
    device B, `b`."""
    m = lambda n, **kw: msg(n, LP, LB, handle_id=1, **kw)
    t = lambda n: T0_NS + n * 60 * NS
    three = lambda: ms.ab_text([("SYNTH-LIN-PART-0 ", [(ts.P, 0)]), ("SYNTH-LIN-PART-1 ", [(ts.P, 1)]),
                                ("SYNTH-LIN-PART-2", [(ts.P, 2)])])
    two = lambda: ms.ab_text([("SYNTH-LIN-PART-0 ", [(ts.P, 0)]), ("SYNTH-LIN-PART-2", [(ts.P, 2)])])
    msgs = []
    chat_name = {"a1": "SYNTH-LIN-NAME-1", "a2": "SYNTH-LIN-NAME-2", "a3": "SYNTH-LIN-NAME-2",
                 "rd": "SYNTH-LIN-NAME-1", "b": "SYNTH-LIN-NAME-B"}[version]
    rd = []
    if version == "b":
        msgs = [m(8, is_from_me=0), m(20, text="SYNTH-LIN-BODY-20 (device B only)")]
    elif version == "a1":
        msgs = [
            m(1), m(2),
            m(3, text="SYNTH-LIN-BODY-3 original"),
            m(4, text="SYNTH-LIN-BODY-4 before"),
            m(5, text=None, attributedBody=three(), part_count=3),
            m(6, text="SYNTH-LIN-BODY-6 unchanged"),
            m(7, subject="SYNTH-LIN-SUBJECT-1"),
            m(8, is_from_me=1),
            m(10, text="SYNTH-LIN-BODY-10 first"),
            m(11, text="SYNTH-LIN-BODY-11 (later in Recently Deleted)"),
        ]
    elif version in ("a2", "a3"):
        msgs = [
            m(1),
            m(3, text=None, date_edited=t(3) + 60 * NS, message_summary_info=ms.summary(otr=1, rp=[0])),
            m(4, text="", date_edited=t(4) + 60 * NS),
            m(5, text=None, attributedBody=two(), part_count=3, date_edited=t(5) + 60 * NS,
              message_summary_info=ms.summary(otr=3, rp=[1])),
            m(6, text="SYNTH-LIN-BODY-6 unchanged", message_summary_info=ms.summary(tmp=True)),
            m(7, subject="SYNTH-LIN-SUBJECT-2"),
            m(8, is_from_me=1),
            m(9, text="SYNTH-LIN-BODY-9 (new in a2)"),
            m(10, text="SYNTH-LIN-BODY-10 edited", date_edited=t(10) + 60 * NS,
              message_summary_info=ms.summary(otr=1, ec={0: [{"d": T0_S + 700,
                                                                "t": ec_text("SYNTH-LIN-EDIT-ORIGINAL-10")}]})),
        ]
    elif version == "rd":
        msgs = [m(1), m(11, text="SYNTH-LIN-BODY-11 (later in Recently Deleted)")]
        rd = [(1, 11)]
    handles = [H(1, "synthetic-lineage-1@example.invalid")]
    chats = [mi.C(1, "iMessage;-;synthetic-lineage-1@example.invalid",
                  ident="synthetic-lineage-1@example.invalid", name=chat_name)]
    live = [r["ROWID"] for r in msgs if (1, r["ROWID"]) not in rd]
    rows = rows_of(handles, chats, msgs, [(1, 1)], [(1, n) for n in live], rd=rd)
    wal_rows = []
    if version == "a3":
        # The newest rows live only in the -wal, which the backup lists but
        # does not hold: the import sees the main file, and it is not a
        # full observation.
        rows["message"] = [r for r in msgs if r["ROWID"] == 1]
        rows["chat_message_join"] = [{"chat_id": 1, "message_id": 1}]
        wal_rows = [("message", m(12, text="SYNTH-LIN-BODY-12 (WAL only)")),
                    ("chat_message_join", {"chat_id": 1, "message_id": 12})]
    return mi.Content(f"view-lin-{version}", mi.import_profile(), rows, [], wal_rows=wal_rows)


# --------------------------------------------------------------------------
# Entries and scenarios
# --------------------------------------------------------------------------

DEV_V = mi.Device("Synthetic Viewer Phone", "iPhone0,0", "18.2")
DEV_A = mi.Device("Synthetic Lineage Phone A", "iPhone0,0", "18.3")
DEV_B = mi.Device("Synthetic Lineage Phone B", "iPhone0,1", "18.3")


class Entry(mi.Entry):
    pass


def catalogue(out):
    d = mi.dt_utc
    lin = {v: content_lineage(v, out) for v in ("a1", "a2", "a3", "b", "rd")}
    return [
        Entry("view-main-plain", content_main(out), DEV_V, d(2026, 6, 1, 10), d(2026, 6, 1, 10),
              purpose="Every association kind (reactions folded or not, a target in another chat, unresolved "
                      "and unparsed targets, tied, undated and sender-undeterminable reactions, an emoji "
                      "removal without an emoji, 201 reactions on one target), replies (resolved, missing, "
                      "unkeyed, another chat), edit codes 2, 5, 6, 7, 9, partial and full unsends, every "
                      "group event and display reason, formatting with a one-time code, a mention and "
                      "inline placeholders, a run issue, sources that differ, URL runs, scheduled, "
                      "pre-2007, undated and zero-dated messages, a message in two chats, every sender "
                      "reason, a service change, app content and an effect, an empty chat, a chat with "
                      "undated messages only, and group-ness by address count"),
        Entry("view-hostile-text-plain", content_hostile(), DEV_V, d(2026, 6, 2, 10), d(2026, 6, 2, 10),
              purpose="Hostile text: markup, bidi controls, invisible and tag characters, NUL, zalgo, "
                      "noncharacters, a URL with U+202E, CJK, Thai, Korean, emoji, `ß`, NFD, a 100,000-"
                      "character token, a 1 MiB body, 10,000 formatting runs, invalid UTF-8 in a body, an "
                      "address and a chat name, two addresses equal in their first 256 units, and a "
                      "file name with U+202E"),
        Entry("view-oldcols-plain", content_oldcols(), DEV_V, d(2026, 6, 3, 10), d(2026, 6, 3, 10),
              purpose="`handle_id`, `date_edited` and `message_summary_info` absent: the sender column "
                      "is not in this backup, and edit code 1 (not recorded)"),
        Entry("view-media-plain", content_media(out, "v1"), DEV_V, d(2026, 6, 4, 10), d(2026, 6, 4, 10),
              purpose="One attachment per media file (encoded images, hostile PNGs, containers, hostile "
                      "files), declared types that disagree with the content, and the card states: a "
                      "missing blob, a file not in the backup, StickerCache found and not found, a NULL "
                      "filename, a path class not looked up, a Manifest size mismatch, hidden, a Genmoji, "
                      "the same bytes twice, inline placement, an unmatched placeholder and an inline "
                      "emoji, a declared size that differs"),
        Entry("view-media2-plain", content_media(out, "v2"), DEV_V, d(2026, 6, 5, 10), d(2026, 6, 5, 10),
              purpose="A later backup of the media chat: one file no longer present (the older blob is "
                      "shown), one GUID with new bytes"),
        Entry("view-lin-a1-plain", lin["a1"], DEV_A, d(2026, 2, 1, 10), d(2026, 2, 1, 10),
              purpose="Lineage A, first backup"),
        Entry("view-lin-rd-plain", lin["rd"], DEV_A, d(2026, 2, 15, 10), d(2026, 2, 15, 10),
              purpose="Lineage A: one message in Recently Deleted"),
        Entry("view-lin-b-plain", lin["b"], DEV_B, d(2026, 3, 1, 10), d(2026, 3, 1, 10),
              purpose="Lineage B: another phone, sharing one message GUID with `is_from_me` = 0"),
        Entry("view-lin-a2-plain", lin["a2"], DEV_A, d(2026, 4, 1, 10), d(2026, 4, 1, 10),
              purpose="Lineage A, later: a message gone, a full and a partial unsend with earlier text, "
                      "code 6 with earlier text, an edit with `ec` text, a new summary with unchanged "
                      "text, a changed subject and chat name"),
        Entry("view-lin-a3-incomplete-plain", lin["a3"], DEV_A, d(2026, 5, 1, 10), d(2026, 5, 1, 10),
              wal_missing=True,
              purpose="Lineage A, newest but incomplete (its `-wal` is listed and missing): not a full "
                      "observation, so it labels nothing \"not in the newest complete import\""),
    ]


# Steps: (entry, lineage label, options).
SCENARIOS = [
    ("media-copy-off", [[("view-media-plain", "L1", {"copyAttachments": False})]],
     "The media entry with attachment copy off"),
    ("media-older-blob", [[("view-media-plain", "L1", {}), ("view-media2-plain", "L1", {})]],
     "The media entry, then its later backup: a file shown from the older backup, a replaced GUID"),
    ("lineages", [[("view-lin-a1-plain", "L1", {}), ("view-lin-rd-plain", "L1", {}),
                   ("view-lin-b-plain", "L2", {}), ("view-lin-a2-plain", "L1", {"iosVersionUntested": True}),
                   ("view-lin-a3-incomplete-plain", "L1", {})]],
     "Two lineages, Recently Deleted, edits and unsends across backups, value history, an untested "
     "iOS version and an incomplete newest import"),
    ("rd-first", [[("view-lin-rd-plain", "L1", {}), ("view-lin-a1-plain", "L1", {})]],
     "The Recently Deleted backup imported first: its message is excluded, then imported from the "
     "older backup, and labelled \"not in the newest complete import\" (the false positive the "
     "caveat names)"),
]


# --------------------------------------------------------------------------
# The import, with what the viewer needs beyond the M4 manifest
# --------------------------------------------------------------------------

class ViewModel:
    """The M4 model archive plus per-session facts: options, attachment
    observations, body sightings and blob bytes."""

    def __init__(self):
        self.arch = mi.Archive()
        self.steps = []         # (entry, lineage, options, session)
        self.obs = {}           # attachment ref -> [(rank, step index, lookup)]
        self.body_seen = {}     # (message ref, body key) -> max rank
        self.body_first = {}    # (message ref, body key) -> (min rank, step)
        self.bytes = {}         # sha256 -> bytes
        self.models = None


def body_key(cols):
    return (mi.obs_key(cols["text"]), mi.obs_key(cols["attributedBody"]),
            mi.obs_key(cols["message_summary_info"]))


def lookup_kind(em, fname, copy):
    """The observation's outcome (spec 5.9's card states, by name)."""
    if not copy:
        return "copyOff"
    if fname[0] == NULL:
        return "notAttempted:null"
    if fname[0] == ABSENT:
        return "notAttempted:columnAbsent"
    if fname[0] == UNREADABLE:
        return "notAttempted:unreadable"
    if fname[0] == OVERSIZED:
        return "notAttempted:tooLong"
    b = fname[1][1]
    cls = ms.path_class(b)
    if cls == "unrecognized":
        return "notAttempted:unrecognized"
    if cls not in ("sms_attachments", "sms_sticker_cache"):
        return "notAttempted:class"
    rel = ms.candidate(b)
    if rel is None:
        return "notAttempted:unusualName"
    f = em.files.get(rel)
    if f is None:
        return "notListedAtSourcedKey" if cls == "sms_attachments" else "notFoundAtAssumedKey"
    if f.state != "present":
        return "blob:" + f.state
    return "present"


def run_view_import(vm, entry, lineage, options):
    em = vm.models[entry]
    copy = options.get("copyAttachments", True)
    s = mi.run_import(vm.arch, em, entry, lineage, include_rd=False, copy=copy)
    s.untested = bool(options.get("iosVersionUntested", False))
    step = len(vm.steps)
    vm.steps.append((entry, lineage, options, s))
    prof = em.prof
    obs = lambda t, c, row: mi.read_obs(t, c, row, prof)
    mkeys = mi.scan_keys(em, "message", lambda r: mi.usable_key_bytes(obs("message", "guid", r)))
    for rowid, r in em.table("message"):
        k = mkeys[rowid]
        if k is None:
            continue
        ref = mi.ref_bytes(k)
        if ref not in vm.arch.rows["message"]:
            continue  # excluded (Recently Deleted)
        cols = {c: obs("message", c, r) for c in ("text", "attributedBody", "message_summary_info")}
        key = body_key(cols)
        row = vm.arch.rows["message"][ref]
        if key in row.bodies:
            prev = vm.body_seen.get((ref, key))
            if prev is None or s.rank > prev:
                vm.body_seen[(ref, key)] = s.rank
            first = vm.body_first.get((ref, key))
            if first is None or (s.rank, step) < first:
                vm.body_first[(ref, key)] = (s.rank, step)
    akeys = mi.scan_keys(em, "attachment", lambda r: mi.usable_key_bytes(obs("attachment", "guid", r)))
    for rowid, r in em.table("attachment"):
        k = akeys[rowid]
        assert k is not None, "the viewer catalogue keys every attachment"
        ref = mi.ref_bytes(k)
        if ref not in vm.arch.rows["attachment"]:
            continue
        fname = obs("attachment", "filename", r)
        kind = lookup_kind(em, fname, copy)
        vm.obs.setdefault(ref, []).append((s.rank, step, kind))
        if kind == "present":
            payload = em.files[ms.candidate(fname[1][1])].payload
            vm.bytes[hashlib.sha256(payload).hexdigest()] = payload
    return s


# --------------------------------------------------------------------------
# Sniffing (spec 5.2): a clean-room statement of the WHATWG MIME Sniffing
# tables' facts (image 6.1, audio and video 6.2) plus ISO-BMFF brands
# --------------------------------------------------------------------------

SNIFF_WINDOW = 4096
IMAGE_KINDS = ("png", "jpeg", "gif", "webp")
STRUCTURED_AV = ("mp4", "m4v", "quicktime", "m4a", "wav")
MAGIC_ONLY_AV = ("mp3", "ogg", "webm")


def brands(b):
    """(major, [compatible]) of a leading ISO-BMFF `ftyp` box, or None."""
    if len(b) < 12 or b[4:8] != b"ftyp":
        return None
    size = struct.unpack(">I", b[:4])[0]
    if size < 16 or size % 4 or size > len(b):
        return None
    return b[8:12], [b[i:i + 4] for i in range(16, size, 4)]


def sniff(data):
    b = data[:SNIFF_WINDOW]
    if b[:6] in (b"GIF87a", b"GIF89a"):
        return "gif"
    if b[:4] == b"RIFF" and b[8:14] == b"WEBPVP":
        return "webp"
    if b[:8] == b"\x89PNG\r\n\x1a\n":
        return "png"
    if b[:3] == b"\xff\xd8\xff":
        return "jpeg"
    if b[:2] == b"BM":
        return "bmp"
    if b[:4] in (b"\x00\x00\x01\x00", b"\x00\x00\x02\x00"):
        return "ico"
    fb = brands(b)
    if fb is not None:
        major, compat = fb
        table = {b"qt  ": "quicktime", b"M4A ": "m4a", b"M4V ": "m4v",
                 b"heic": "heif", b"heix": "heif", b"mif1": "heif", b"msf1": "heif"}
        for brand in [major] + compat:
            if brand in table:
                return table[brand]
            if brand[:3] == b"mp4":
                return "mp4"
        return "isobmff-other"
    if b[:4] == b"\x1a\x45\xdf\xa3" and b"webm" in b[4:64]:
        return "webm"
    if b[:4] == b"RIFF" and b[8:12] == b"WAVE":
        return "wav"
    if b[:4] == b"FORM" and b[8:12] == b"AIFF":
        return "aiff"
    if b[:5] == b"OggS\x00":
        return "ogg"
    if b[:3] == b"ID3":
        return "mp3"
    if b[:4] == b"caff":
        return "caf"
    if b[:5] == b"%PDF-":
        return "pdf"
    stripped = b.lstrip(b"\t\n\x0c\r ")
    low = stripped.lower()
    if low.startswith(b"<?xml"):
        return "xml"
    for tag in (b"<!doctype html", b"<html", b"<head", b"<script", b"<iframe", b"<h1", b"<div",
                b"<font", b"<table", b"<a", b"<style", b"<title", b"<b", b"<body", b"<br", b"<p",
                b"<!--", b"<svg"):
        if low.startswith(tag) and len(low) > len(tag) and low[len(tag)] in b" >":
            return "html"
    if low.startswith(b"begin:vcard"):
        return "vcard"
    if b and all(c not in BINARY_BYTES for c in b):
        try:
            b.decode("utf-8")
            return "text"
        except UnicodeDecodeError:
            pass
    return "unknown"


BINARY_BYTES = set(range(0x00, 0x09)) | {0x0B} | set(range(0x0E, 0x1B)) | set(range(0x1C, 0x20))


# --------------------------------------------------------------------------
# The structural check (spec 5.3)
# --------------------------------------------------------------------------

CONTAINERS = {b"moov", b"trak", b"mdia", b"minf", b"dinf", b"stbl", b"edts", b"udta", b"mvex",
              b"rmra", b"rmda"}
REFERENCE_ATOMS = {b"rmra", b"rmda", b"rdrf", b"rmdr"}
DREF_ENTRIES = {b"url ", b"urn ", b"alis", b"rsrc"}


class Refused(Exception):
    pass


def check_bmff(data):
    count = [0]

    def walk(start, end, depth, top):
        if depth > MAX_DEPTH:
            raise Refused("tooDeep")
        pos = start
        seen = []
        while pos < end:
            if end - pos < 8:
                raise Refused("boxPastEnd")
            count[0] += 1
            if count[0] > MAX_BOXES:
                raise Refused("tooManyBoxes")
            size = struct.unpack(">I", data[pos:pos + 4])[0]
            typ = data[pos + 4:pos + 8]
            hdr = 8
            if size == 1:
                if end - pos < 16:
                    raise Refused("boxPastEnd")
                size = struct.unpack(">Q", data[pos + 8:pos + 16])[0]
                hdr = 16
                if pos + size > 0xFFFF_FFFF_FFFF_FFFF:
                    raise Refused("sizeOverflow")
            elif size == 0:
                if not top:
                    raise Refused("sizeZeroNested")
                size = end - pos
            if size < hdr:
                raise Refused("boxTooSmall")
            if pos + size > end:
                raise Refused("boxPastEnd")
            if typ in REFERENCE_ATOMS:
                raise Refused("referenceMovie")
            body = (pos + hdr, pos + size)
            if typ in CONTAINERS:
                walk(body[0], body[1], depth + 1, False)
            elif typ == b"dref":
                if body[1] - body[0] < 8:
                    raise Refused("boxTooSmall")
                entries_start = body[0] + 8
                for etyp, eflags in dref_entries(entries_start, body[1], depth + 1):
                    if etyp in DREF_ENTRIES and not eflags & 1:
                        raise Refused("externalDataReference")
            seen.append(typ)
            pos += size
        return seen

    def dref_entries(start, end, depth):
        if depth > MAX_DEPTH:
            raise Refused("tooDeep")
        pos = start
        out = []
        while pos < end:
            if end - pos < 12:
                raise Refused("boxPastEnd")
            count[0] += 1
            if count[0] > MAX_BOXES:
                raise Refused("tooManyBoxes")
            size = struct.unpack(">I", data[pos:pos + 4])[0]
            if size < 12:
                raise Refused("boxTooSmall")
            if pos + size > end:
                raise Refused("boxPastEnd")
            flags = int.from_bytes(data[pos + 9:pos + 12], "big")
            out.append((data[pos + 4:pos + 8], flags))
            pos += size
        return out

    top = walk(0, len(data), 1, True)
    if b"moov" not in top:
        raise Refused("noMoov")


def check_riff(data):
    if len(data) < 12:
        raise Refused("chunkPastEnd")
    riff = struct.unpack("<I", data[4:8])[0]
    end = 8 + riff
    if end > len(data):
        raise Refused("chunkPastEnd")
    pos = 12
    fmt = False
    n = 0
    while pos < end:
        if end - pos < 8:
            raise Refused("chunkPastEnd")
        n += 1
        if n > MAX_BOXES:
            raise Refused("tooManyBoxes")
        cid = data[pos:pos + 4]
        size = struct.unpack("<I", data[pos + 4:pos + 8])[0]
        if pos + 8 + size > end:
            raise Refused("chunkPastEnd")
        if cid == b"fmt ":
            fmt = True
        if cid == b"data" and not fmt:
            raise Refused("dataBeforeFmt")
        pos += 8 + size + (size & 1)
    if not fmt:
        raise Refused("noFmt")


def structure(kind, data):
    if kind in ("mp4", "m4v", "quicktime", "m4a"):
        try:
            check_bmff(data)
        except Refused as e:
            return {"state": "refused", "reason": str(e)}
        return {"state": "ok"}
    if kind == "wav":
        try:
            check_riff(data)
        except Refused as e:
            return {"state": "refused", "reason": str(e)}
        return {"state": "ok"}
    if kind in MAGIC_ONLY_AV:
        return {"state": "notChecked"}
    return None


# --------------------------------------------------------------------------
# The viewer model (spec 3.3, 3.5, 3.6, 3.8, 5.9)
# --------------------------------------------------------------------------

REACTION_NAMES = ["love", "like", "dislike", "laugh", "emphasize", "question", "emoji", "sticker"]

ITEM_NAMES = {
    "message": "message", "participant_added": "participantAdded",
    "own_number_changed": "ownNumberChanged", "participant_removed": "participantRemoved",
    "group_renamed": "groupRenamed", "participant_left": "participantLeft",
    "group_photo_changed": "groupPhotoChanged", "group_photo_removed": "groupPhotoRemoved",
    "background_changed": "backgroundChanged", "background_removed": "backgroundRemoved",
    "legacy_shared_location": "legacySharedLocation", "audio_kept": "audioKept",
    "share_play": "sharePlay", "unknown": "unknown",
}

EDIT_NAMES = {
    "not_recorded": "notRecorded", "summary_unreadable": "summaryUnreadable",
    "unsent_parts_and_edit_history": "unsentPartsAndEditHistory", "unsent_parts": "unsentParts",
    "edit_history": "editHistory", "date_edited_no_body": "dateEditedNoBody",
    "date_edited_only": "dateEditedOnly", "no_edit_recorded": "noEditRecorded",
    "undetermined": "undetermined",
}

BIDI = {0x061C, 0x200E, 0x200F} | set(range(0x202A, 0x202F)) | set(range(0x2066, 0x206A))
INVISIBLE = set(range(0x200B, 0x200E)) | {0x2060, 0xFEFF, 0xFFFC} | set(range(0xE0000, 0xE0080))


def text_facts(text):
    """bidi_controls and invisible (spec 3.8)."""
    cps = [ord(c) for c in text]
    bidi = any(c in BIDI for c in cps)
    inv = any(c in INVISIBLE for c in cps)
    run = 0
    for c in cps:
        if 0xFE00 <= c <= 0xFE0F or 0xE0100 <= c <= 0xE01EF:
            run += 1
            if run > 4:
                inv = True
        else:
            run = 0
    return bidi, inv


def url_runs(text):
    """Runs that begin http:// or https:// (ASCII, case-insensitive) and
    continue to the next whitespace; `hidden` when a bidi control or an
    invisible character is inside (spec 3.6)."""
    out = []
    i = 0
    low = text.lower()
    while i < len(text):
        j = min([p for p in (low.find("http://", i), low.find("https://", i)) if p >= 0], default=-1)
        if j < 0:
            break
        k = j
        while k < len(text) and not text[k].isspace():
            k += 1
        run = text[j:k]
        bidi, inv = text_facts(run)
        out.append({"text": run, "hidden": bidi or inv})
        i = k
    return out


def sort_pos(row, order):
    """The model's sort_key, plus the archive id (insertion order)."""
    d = tval(col(row, "date"))
    if d is not None and d[0] in ("seconds", "nanoseconds"):
        return (0, d[1], b"", 0, order[row.ref])
    return (1, 0, row.first_rank, row.first_src_rowid, order[row.ref])


def build_viewer(vm, name_of):
    arch = vm.arch
    msgs = arch.rows["message"]
    order = {ref: i for i, ref in enumerate(msgs)}
    chat_order = {ref: i for i, ref in enumerate(arch.rows["chat"])}
    by_guid = {}
    for ref, row in msgs.items():
        if row.unkeyed is None:
            by_guid[bval(col(row, "guid"))] = ref
    unkeyed_guids = {}
    for ref, row in msgs.items():
        if row.unkeyed is not None:
            g = bval(col(row, "guid"))
            if g:
                unkeyed_guids[g] = unkeyed_guids.get(g, 0) + 1
    chats_of = {}
    members = {}
    for c, mref in arch.chat_message:
        chats_of.setdefault(mref, set()).add(c)
        members.setdefault(c, set()).add(mref)
    handles_of = {}
    for c, h in arch.chat_handle:
        handles_of.setdefault(c, set()).add(h)
    rank_session = {}
    for entry, lineage, options, s in vm.steps:
        rank_session[s.rank] = (entry, lineage, s)
    entry_of_digest = {vm.models[e].info["contentDigest"]: e for e, _, _, _ in vm.steps}

    def dated(row):
        d = tval(col(row, "date"))
        return d is not None and d[0] in ("seconds", "nanoseconds")

    def date_ns(row):
        return tval(col(row, "date"))[1]

    def assoc(row):
        """(present code, kind name or None, shape, target guid, part, emoji)."""
        typ, g = col(row, "associated_message_type"), col(row, "associated_message_guid")
        typed = ival(typ) is not None and ival(typ) != 0
        gb = bval(g)
        targeted = (gb is not None and gb != b"") or g[0] in (OVERSIZED, UNREADABLE)
        if not (typed or targeted):
            present = 2 if typ[0] in (OVERSIZED, UNREADABLE) else 0
            return (present, None, None, None, None, None)
        kind = ms.assoc_kind(ival(typ)) if ival(typ) is not None else None
        shape, tg, part = None, None, None
        if gb is not None and gb != b"":
            shape, tg = ms.assoc_target(gb)
            if shape == "part":
                part = int(gb[2:].split(b"/", 1)[0])
        elif g[0] in (OVERSIZED, UNREADABLE):
            shape = "unparsed"
        if kind is None and shape is None:
            shape = "unparsed"
        emoji = bval(col(row, "associated_message_emoji"))
        return (1, kind, shape, tg, part, text_of(emoji) if emoji is not None else None)

    def is_reaction(a):
        return a[0] == 1 and a[1] is not None and a[1].split(":")[0] in ("added", "removed")

    def target_ref(a):
        return by_guid.get(a[3]) if a[3] is not None else None

    # Timelines and folding (3.5 step 3).
    folded = {}
    threads = {}
    for c in sorted(arch.rows["chat"], key=lambda r: chat_order[r]):
        rows = sorted(members.get(c, ()), key=lambda r: sort_pos(msgs[r], order))
        tl = {"dated": [], "undated": [], "folded": []}
        for r in rows:
            a = assoc(msgs[r])
            t = target_ref(a) if is_reaction(a) else None
            if t is not None and t in members.get(c, ()):
                tl["folded"].append({"reaction": name_of(r), "target": name_of(t)})
                folded.setdefault(c, set()).add(r)
                continue
            tl["dated" if dated(msgs[r]) else "undated"].append(name_of(r))
        threads[name_of(c)] = tl

    # Conversations (3.5): dated activity of any row, then chats with none.
    conv = []
    for c in arch.rows["chat"]:
        rows = sorted(members.get(c, ()), key=lambda r: sort_pos(msgs[r], order))
        dated_rows = [r for r in rows if dated(msgs[r])]
        last = date_ns(msgs[dated_rows[-1]]) if dated_rows else None
        preview = None
        for r in list(reversed(dated_rows))[:PREVIEW_SCAN_ROWS]:
            if r not in folded.get(c, set()):
                preview = name_of(r)
                break
        modern = [date_ns(msgs[r]) for r in dated_rows if date_ns(msgs[r]) >= FLOOR_2007_NS]
        conv.append(((last is None, -(last or 0), chat_order[c]), {
            "chat": name_of(c),
            "lastDatedUtcNs": last,
            "addresses": len(handles_of.get(c, ())),
            "group": len(handles_of.get(c, ())) > 1,
            "preview": preview,
            "itemsRecorded": len(rows),
            "undatedItems": len(rows) - len(dated_rows),
            "firstDate2007UtcNs": min(modern) if modern else None,
            "earlierDatedItems": len(dated_rows) - len(modern),
            "empty": not rows,
            "notInLatest": mi.not_in_latest(arch, "chat", c),
        }))
    conv.sort(key=lambda x: x[0])

    # Reactions per target (3.6).
    reactions_of = {}
    for ref, row in msgs.items():
        a = assoc(row)
        if is_reaction(a) and target_ref(a) is not None:
            reactions_of.setdefault(target_ref(a), []).append(ref)

    def sender_of(row):
        fm = ival(col(row, "is_from_me"))
        if fm == 1:
            return "me"
        if fm == 0 and row.handle_ref is not None:
            return row.handle_ref
        return None

    def chips(target):
        rows = reactions_of.get(target, [])
        if len(rows) > REACTION_ROWS_CAP:
            return {"rows": len(rows), "over200": True, "chips": [], "unpaired": []}
        groups, single, unpaired = {}, [], []
        for r in rows:
            row = msgs[r]
            kind = assoc(row)[1]
            verb, base = kind.split(":")
            emoji = assoc(row)[5]
            if base == "emoji":
                if verb == "removed" and emoji is None:
                    unpaired.append(name_of(r))
                    continue
                key_kind = "emoji:" + (emoji if emoji is not None else "(none)")
            else:
                key_kind = base
            snd = sender_of(row)
            if snd is None:
                single.append({"kind": key_kind, "sender": None, "rows": [name_of(r)],
                               "state": "added" if verb == "added" else "removed"})
                continue
            groups.setdefault((key_kind, snd), []).append(r)
        out = []
        for (k, snd), rs in sorted(groups.items(), key=lambda x: (x[0][0], name_of(x[0][1]) if x[0][1] != "me" else "")):
            if any(not dated(msgs[r]) for r in rs):
                state = "undeterminable"
            else:
                keys = [date_ns(msgs[r]) for r in rs]
                if len(set(keys)) != len(keys):
                    state = "undeterminable"
                else:
                    latest = max(rs, key=lambda r: date_ns(msgs[r]))
                    state = "shown" if assoc(msgs[latest])[1].startswith("added") else "hidden"
            out.append({"kind": k, "sender": "me" if snd == "me" else name_of(snd),
                        "rows": sorted(name_of(r) for r in rs), "state": state})
        return {"rows": len(rows), "over200": False, "chips": out + single,
                "unpaired": sorted(unpaired)}

    # Bodies: per message, the current body and the versions.
    def body_info(row):
        cur = row.current_body
        text_o = json.loads(cur[0])
        ab_o = json.loads(cur[1])
        summ_o = json.loads(cur[2])
        return cur, text_o, ab_o, summ_o

    def ab_public(ab_o):
        if ab_o[0] != VALUE:
            return None
        b = bytes.fromhex(ab_o[1][1])
        return ANN[b].public

    def summary_info(summ_o):
        if summ_o[0] != VALUE:
            return (summ_o[0], None)
        b = bytes.fromhex(summ_o[1][1])
        exp = ANN[b].expect
        if exp[0] == "ok":
            return ("value", exp[1])
        return ("unreadable", None)

    def summary_root(summ_o):
        if summ_o[0] != VALUE:
            return None
        import plistlib
        try:
            return plistlib.loads(bytes.fromhex(summ_o[1][1]))
        except Exception:
            return None

    masked_otc, masked_earlier = set(), set()
    out_msgs = {}
    reply_counts = {}
    for ref, row in msgs.items():
        g = bval(col(row, "thread_originator_guid"))
        a = assoc(row)
        item = ms.item_kind(msv(col(row, "item_type")), msv(col(row, "group_action_type")),
                            msv(col(row, "handle_id")), msv(col(row, "other_handle")))
        if g and a[0] == 0 and item == "message":
            reply_counts[g] = reply_counts.get(g, 0) + 1

    for ref, row in msgs.items():
        a = assoc(row)
        item = ms.item_kind(msv(col(row, "item_type")), msv(col(row, "group_action_type")),
                            msv(col(row, "handle_id")), msv(col(row, "other_handle")))
        f = {"item": ITEM_NAMES[item]}
        # Direction (3.6).
        fm = ival(col(row, "is_from_me"))
        f["direction"] = {1: "you", 0: "received"}.get(fm, "notRecorded")
        lins = sorted({lin for (e, r, lin) in arch.seen if e == "message" and r == ref})
        if len(lins) > 1:
            entry, lineage, _ = rank_session[row.last_rank]
            f["directionAsRecordedBy"] = {"lineage": lineage, "entry": entry}
        # Sender (3.6).
        if fm == 1:
            f["sender"] = {"reason": "fromMe"}
        else:
            h = col(row, "handle_id")
            if h[0] == ABSENT:
                f["sender"] = {"reason": "columnAbsent"}
            elif h[0] in (UNREADABLE, OVERSIZED):
                f["sender"] = {"reason": "unreadable"}
            elif h[0] == NULL or ival(h) == 0:
                f["sender"] = {"reason": "noSender"}
            elif row.handle_ref is None:
                f["sender"] = {"reason": "notFound"}
            else:
                f["sender"] = {"reason": "resolved", "handle": name_of(row.handle_ref)}
        f["service"] = ms.service_class(msv(col(row, "service")))
        # Association (3.5 step 3, 3.6).
        if a[0] != 0:
            t = target_ref(a)
            here = chats_of.get(ref, set())
            if a[0] == 2:
                vis = "visible:undeterminable"
            elif a[1] is None or not is_reaction(a):
                vis = "visible:" + ("noKind" if a[1] is None else a[1].replace("_", ""))
            elif a[2] == "unparsed":
                vis = "visible:targetUnreadable"
            elif t is None:
                vis = "visible:targetNotUnique"
            elif chats_of.get(t, set()) & here:
                vis = "folded"
            else:
                vis = "visible:targetInAnotherChat"
            f["association"] = {"present": a[0], "kind": a[1], "shape": a[2],
                                "target": name_of(t) if t else None, "part": a[4],
                                "emoji": a[5], "timeline": vis}
            if a[2] == "bare":
                f["association"]["note"] = "bareTarget"
        # Reactions on this message.
        if ref in reactions_of:
            f["reactions"] = chips(ref)
        # Replies (3.6).
        g = bval(col(row, "thread_originator_guid"))
        if g:
            o = by_guid.get(g)
            if o is None:
                f["reply"] = {"state": "notUniquelyIdentified",
                              "unkeyedShareIdentifier": unkeyed_guids.get(g, 0) > 0}
            else:
                same = bool(chats_of.get(o, set()) & chats_of.get(ref, set()))
                f["reply"] = {"state": "resolved" if same else "inAnotherConversation",
                              "originator": name_of(o)}
        if row.unkeyed is None:
            n = reply_counts.get(bval(col(row, "guid")), 0)
            if n:
                f["replyCount"] = min(n, REPLY_COUNT_CAP)
                f["replyCountCapped"] = n > REPLY_COUNT_CAP
        # Body (3.6, 3.7, 3.8).
        cur, text_o, ab_o, summ_o = body_info(row)
        disp_text, how = row.bodies[cur]
        if disp_text is not None:
            bidi, inv = text_facts(disp_text)
            f["body"] = {"source": how, "utf16": len(disp_text.encode("utf-16-le")) // 2,
                         "bidiControls": bidi, "invisible": inv}
            runs = url_runs(disp_text)
            if runs:
                f["body"]["urls"] = runs
        else:
            f["body"] = {"noneReason": how}
            if ab_o[0] == VALUE and "error" in ab_public(ab_o):
                f["body"]["attributedError"] = ab_public(ab_o)["error"]
        pub = ab_public(ab_o)
        parts = [0] if disp_text is not None else []
        if pub is not None and "ok" in pub:
            ok = pub["ok"]
            if text_o[0] == VALUE:
                f["body"]["sourcesDiffer"] = bytes.fromhex(text_o[1][1]) != ok["text"].encode("utf-8")
            if ok["runs"]:
                if how != "attributedBody":
                    f["formatting"] = "notApplied"
                elif ok["run_issue"]:
                    f["formatting"] = "unreadable"
                else:
                    f["formatting"] = "applied"
            elif ok["run_issue"]:
                f["formatting"] = "unreadable"
            ps = sorted({r["attrs"]["part"] for r in ok["runs"] if r["attrs"]["part"] is not None})
            parts = ps or parts
            if f.get("formatting") == "applied":
                otc, inline = [], []
                att_guids = {}
                for aref, _ in sorted(((k[1], v) for k, v in arch.message_attachment.items() if k[0] == ref)):
                    att_guids[bval(col(arch.rows["attachment"][aref], "guid"))] = aref
                for r in ok["runs"]:
                    seg = utf16_slice(ok["text"], r["utf16"][0], r["utf16"][1])
                    if "one_time_code" in r["attrs"]["flags"]:
                        otc.append(seg)
                    for ch in seg:
                        if ch != "￼":
                            continue
                        g2 = r["attrs"]["guid"]
                        if "inline_emoji" in r["attrs"]["flags"]:
                            inline.append({"kind": "inlineEmoji"})
                        elif g2 is not None and g2.encode() in att_guids:
                            inline.append({"kind": "attachment",
                                           "attachment": name_of(att_guids[g2.encode()])})
                        else:
                            inline.append({"kind": "unmatched"})
                if otc:
                    f["oneTimeCodes"] = otc
                    masked_otc.update(otc)
                if inline:
                    placed = {i["attachment"] for i in inline if i["kind"] == "attachment"}
                    f["inline"] = inline
                    f["listedBelow"] = [name_of(x) for x in sorted(att_guids.values(), key=name_of)
                                        if name_of(x) not in placed]
                if any("audio_transcription" in r["attrs"]["flags"] for r in ok["runs"]):
                    f["audioTranscription"] = True
                if any(r["attrs"]["mention"] is not None for r in ok["runs"]):
                    f["mentions"] = sum(1 for r in ok["runs"] if r["attrs"]["mention"] is not None)
        # Edits and unsends.
        summ = summary_info(summ_o)
        de = col(row, "date_edited")
        de_m = (ABSENT, None) if de[0] == ABSENT else (
            ("value", (de[1][2], de[1][3])) if de[0] == VALUE else (de[0], None))
        tx_m = (text_o[0], bytes.fromhex(text_o[1][1]) if text_o[0] == VALUE else None)
        ab_state = "null" if ab_o[0] == NULL else ("absent" if ab_o[0] == ABSENT else ab_o[0])
        if ab_o[0] == VALUE:
            ab_state = "failed" if "error" in pub else "decoded"
        label = ms.edit_label(de_m, summ, tx_m, ab_state)
        e = {"label": EDIT_NAMES[label]}
        info = summ[1] if summ[0] == "value" else None
        if info and info["ec"] and info["ec"][0] == "value":
            e["editedParts"] = sorted(p for p, _ in info["ec"][1])
            root = summary_root(summ_o)
            texts = []
            for p, entries in sorted(root.get("ec", {}).items(), key=lambda x: int(x[0])):
                for k, en in enumerate(entries):
                    tb = en.get("t")
                    texts.append({"part": int(p), "ordinal": k,
                                  "text": ANN[tb].public["ok"]["text"] if tb in ANN else None})
            e["ecTexts"] = texts
        unsent = []
        if info and info["rp"] and info["rp"][0] == "value":
            unsent = sorted(info["rp"][1])
            e["unsentParts"] = unsent
            e["unsentOf"] = len(set(parts) | set(unsent))
            e["unsentWhole"] = set(parts) <= set(unsent)
        versions = sorted(row.bodies.items(), key=lambda kv: vm_first_rank(vm, ref, kv[0]))
        e["storedVersions"] = len(versions)
        groups = []
        for k, (txt, _how) in versions:
            if groups and groups[-1]["text"] == txt:
                groups[-1]["versions"] += 1
            else:
                groups.append({"text": txt, "versions": 1})
        if len(versions) > 1:
            e["versionGroups"] = [{"hasText": g["text"] is not None, "versions": g["versions"]}
                                  for g in groups]
        earlier = [txt for k, (txt, _h) in versions if k != cur and txt is not None and txt != disp_text]
        if earlier:
            withheld = bool(unsent) or disp_text is None
            e["earlierText"] = "withheld" if withheld else "shown"
            if withheld:
                e["earlierTextReason"] = "unsent" if unsent else (
                    "earlierVersion" if label == "date_edited_no_body" else "noCurrentText")
                masked_earlier.update(earlier)
                if "ecTexts" in e:
                    masked_earlier.update(x["text"] for x in e["ecTexts"] if x["text"])
        f["edit"] = e
        # Labels.
        labels = []
        d = tval(col(row, "date"))
        if not dated(row):
            labels.append("undated")
        elif d[1] < FLOOR_2007_NS:
            labels.append("before2007")
        if row.unkeyed is not None:
            labels.append("unkeyed")
        st = ival(col(row, "schedule_type"))
        if st:
            labels.append(f"scheduleType:{st}")
        if bval(col(row, "balloon_bundle_id")):
            labels.append("appContent")
        if bval(col(row, "expressive_send_style_id")):
            labels.append("effect")
        for lin in mi.not_in_latest(arch, "message", ref):
            labels.append(f"notInLatest:{lin}")
        last_entry, _, last_s = rank_session[row.last_rank]
        body_rank = vm.body_seen.get((ref, cur))
        body_s = rank_session[body_rank][2] if body_rank is not None else None
        if getattr(last_s, "untested", False) or (body_s is not None and getattr(body_s, "untested", False)):
            labels.append("provisional")
        f["labels"] = sorted(labels)
        rds = []
        for digest, _j, mref, cref in sorted(arch.markers, key=lambda x: (x[0], x[1])):
            if mref != ref:
                continue
            entry = entry_of_digest[digest]
            s = next(s for e2, _, _, s in vm.steps if e2 == entry)
            later = [e2 for e2, lin2, _, s2 in vm.steps if lin2 == s.lineage and s2.rank > s.rank
                     and not any(m2 == ref and entry_of_digest[d2] == e2 for d2, _, m2, _ in arch.markers)]
            rds.append({"entry": entry, "chat": name_of(cref) if cref else None,
                        "notListedLater": sorted(set(later))})
        if rds:
            f["recentlyDeleted"] = rds
        if dated(row):
            f["utcNs"] = d[1]
        out_msgs[name_of(ref)] = f

    # Attachments (3.9, 5.9).
    out_atts = {}
    for aref, row in arch.rows["attachment"].items():
        obs = sorted(vm.obs.get(aref, []), key=lambda x: (x[0], x[1]))
        top = obs[-1][2] if obs else None
        a = {"declaredBytes": ival(col(row, "total_bytes")),
             "storedBytes": arch.blobs.get(row.current_blob) if row.current_blob else None,
             "isSticker": ival(col(row, "is_sticker")) == 1,
             "hideAttachment": ival(col(row, "hide_attachment")) == 1,
             "observations": [o[2] for o in obs]}
        name = bval(col(row, "transfer_name"))
        if name is not None:
            bidi, inv = text_facts(text_of(name))
            a["nameBidiControls"] = bidi
            a["nameInvisible"] = inv
        if row.current_blob is not None:
            data = vm.bytes[row.current_blob]
            kind = sniff(data)
            a["sniff"] = kind
            mime = bval(col(row, "mime_type"))
            a["declaredType"] = text_of(mime) if mime is not None else None
            if kind in IMAGE_KINDS:
                a["card"] = "image"
            elif kind in STRUCTURED_AV or kind in MAGIC_ONLY_AV:
                a["card"] = "play"
                a["structure"] = structure(kind, data)
            elif kind == "unknown":
                a["card"] = "unrecognised"
            else:
                a["card"] = "file:" + kind
            a["fromOlderBackup"] = top != "present"
        elif top is None:
            a["card"] = "noObservation"
        elif top == "copyOff":
            a["card"] = "notCopied"
        elif top == "notListedAtSourcedKey":
            a["card"] = "notInBackup"
        elif top == "notFoundAtAssumedKey":
            a["card"] = "notFoundAtAssumedKey"
        elif top == "notAttempted:class":
            a["card"] = "classNotLookedUp"
        elif top.startswith("notAttempted:"):
            a["card"] = "locationUnusable"
        elif top.startswith("blob:"):
            a["card"] = "couldNotBeRead"
        else:
            a["card"] = "unavailable:" + top
        out_atts[name_of(aref)] = a

    return {
        "conversations": [c for _, c in conv],
        "threads": threads,
        "messages": out_msgs,
        "attachments": out_atts,
        "masked": {"oneTimeCodes": sorted(masked_otc), "earlierText": sorted(masked_earlier)},
    }


def vm_first_rank(vm, ref, key):
    """A body version's order: the lowest rank that saw it (`first_rank`),
    then the import step. Unkeyed messages have one version here."""
    return vm.body_first.get((ref, key), (b"\xff", 0))


# Text values longer than this many UTF-8 bytes are replaced in the content
# manifest by "sha256:<hex>:<bytes>" (the 1 MiB body and the 100,000-character
# token would otherwise be copied into every manifest). The Rust comparison
# applies the same rule to what it reads back.
LONG_TEXT = 4096


def shorten(v):
    if isinstance(v, dict):
        return {k: (long_text(x) if k == "text" and isinstance(x, str) else shorten(x))
                for k, x in v.items()}
    if isinstance(v, list):
        return [shorten(x) for x in v]
    return v


def long_text(s):
    b = s.encode("utf-8")
    if len(b) <= LONG_TEXT:
        return s
    return f"sha256:{hashlib.sha256(b).hexdigest()}:{len(b)}"


def run_steps(steps, models):
    vm = ViewModel()
    vm.models = models
    for entry, lineage, options in steps:
        run_view_import(vm, entry, lineage, options)
    content = shorten(mi.manifest(vm.arch))
    name_of = mi.label_unkeyed(vm.arch)
    viewer = build_viewer(vm, name_of)
    return content, viewer


def step_json(entry, lineage, options):
    return {"entry": entry, "lineage": lineage,
            "includeRecentlyDeleted": False,
            "copyAttachments": options.get("copyAttachments", True),
            "iosVersionUntested": options.get("iosVersionUntested", False)}


def document(orders, models, content, viewer):
    entries = sorted({e for steps in orders for e, _, _ in steps})
    return {"format": "oma-viewer-content", "formatVersion": 1,
            "orders": [[step_json(*s) for s in steps] for steps in orders],
            "sessions": {n: models[n].info for n in entries},
            "content": content, "viewer": viewer}


def write_json(path, value):
    mi.write_json(path, value)


def is_expected(rel):
    first = rel.split("/", 1)[0]
    return first.endswith(".expected") or first == "scenarios"


README = """\
# Synthetic viewer catalogue (M5)

Test input for the viewer reads in `crates/oma-archive` (M5 spec §14.1).
**Every value in these files is invented**: addresses under
`example.invalid`, `+1555…` numbers, and sentinel text (`SYNTH-VIEW-…`,
`SYNTH-HX-…`, `SYNTH-MEDIA-…`, `SYNTH-LIN-…`, `SYNTH-OLD-…`, `SYNTH-OTC-…`).
No real device, person, message, backup or photo was used. Generated by
`crates/oma-archive/examples/make_viewer_media` (the `media/` files) and
`scripts/fixtures/make_synthetic_viewer.py` (everything else); do not edit by
hand.

## Layout

```text
media/<file>                          the media files (see the table below)
<entry>/SYNTHETIC0-0000000000000000/  one backup (the import input)
<entry>.expected/content.json         the model after importing <entry> alone
scenarios/<name>.content.json         the model after a sequence of imports
SHA256SUMS                            the inputs: media/ and every backup file
EXPECTED.SHA256SUMS                   every file under *.expected/ and scenarios/
```

There are two pins, as for the import catalogue. `SHA256SUMS` covers the
inputs and is frozen once committed; `EXPECTED.SHA256SUMS` covers the
model's expectations, which may be regenerated when a slice settles a rule
(the commit says which rule changed). The SHA-256 of each is pinned in
`crates/oma-archive/tests/viewer_fixtures.rs`, which also rebuilds every
`media/` file from `make_viewer_media`'s code and compares the bytes.

## The model

Each `content.json` holds two statements by the generator's own model.
**Agreement with the Rust code proves self-consistency only.**

- `content` is the archive the M4 import must leave, in the import
  catalogue's manifest form (see `tests/fixtures/imports/README.md`),
  computed by `make_synthetic_imports.py`'s model.
- `viewer` is what the M5 viewer must derive from that archive:
  - `conversations`, in the list's order: by the newest **dated** row of
    any kind (reactions and events included), newest first; chats with no
    dated row after every other, in archive id order (§3.5). Each has its
    address count (`group` when more than one; `chat.style` is never read),
    the preview (the newest dated item that is not folded, within 50 rows),
    the counts, and the first date of 2007 or later with the number of
    earlier-dated rows.
  - `threads`: per chat, the visible `dated` and `undated` items, oldest
    first, and the `folded` reactions with their targets. Only reactions
    (`assoc_kind` add or remove, `assoc_present = 1`) whose target resolves
    to a keyed message that is a timeline item of the same chat are folded.
  - `messages`: per message, the closed facts: item kind, direction (and,
    when more than one lineage saw it, the lineage and backup the value
    follows), the sender reason, the service class, the association and
    where it lands on the timeline, the reaction chips on it, the reply
    resolution and the reply count, the body's display source or reason,
    URL runs, formatting (applied only with `display_source = 1` and no run
    issue), one-time codes, inline placement, the edit label with edited
    and unsent parts, `ec` texts, stored versions grouped by text, whether
    earlier text is withheld, labels, and Recently Deleted markers.
  - `attachments`: the declared and stored sizes, the observations' lookup
    outcomes, the sniffed kind (from the bytes, never the stored type), the
    card, and for playable kinds the structural check's verdict.
  - `masked`: the one-time codes and the earlier text that must appear in
    no DTO unless the request carries `reveal` (D115).

### Readings the model makes (to be confirmed by the viewer slices)

- **Chip state** is per (target, kind or emoji, sender). The latest row by
  date wins; a removal hides the chip; any undated row, or two rows with the
  same date, make it `undeterminable`. A row whose sender cannot be
  determined is its own chip. An emoji removal without an emoji is listed
  as `unpaired`. Over 200 rows on a target derives no chip.
- **The sender** of a reaction is `me` for `is_from_me = 1`, else the
  resolved handle.
- **Parts** of a message are the `part` attributes of its decoded runs, or
  part 0 when there are none; `unsentWhole` means every part is listed.
- **Earlier text** is a non-current body version's display text that
  differs from the current one; it is withheld when the message has an
  unsent part or no current text.
- **The first date of 2007 or later** and the earlier-dated count are over
  every dated row of the chat, as `chat_stats` counts them.
- **Sniffing**: the image table first (GIF, WebP, PNG, JPEG, BMP, ICO), then
  an `ftyp` box whose major brand, then compatible brands in order, decide
  (`qt  ` QuickTime, `M4A `, `M4V `, `heic`/`heix`/`mif1`/`msf1` HEIF, any
  `mp4*` MP4), then WebM, WAV, AIFF, Ogg, `ID3` MP3, CAF, PDF, XML and HTML
  (leading whitespace skipped, case-insensitive, a tag followed by a space
  or `>`), vCard, and UTF-8 text without control bytes. Everything else,
  including empty files, is `unknown`.
- **The structural check**: ISO-BMFF boxes are walked through `moov`,
  `trak`, `mdia`, `minf`, `dinf`, `stbl`, `edts`, `udta`, `mvex`, `rmra` and
  `rmda`, and `dref` entries are read; reason names are the model's.
- **No attachment row without an observation** can be built by an import,
  and a verifier result (`verify_state`) needs a verify run, so those card
  states are left to the tests that construct them.

## Regenerating (reference environment only)

```sh
python3 -m venv .venv-fixtures
.venv-fixtures/bin/pip install -r scripts/fixtures/requirements.txt
.venv-fixtures/bin/python -I -c "import sqlite3; print(sqlite3.sqlite_version)"   # must print 3.45.1
rm -rf /tmp/viewer && cargo run -p oma-archive --example make_viewer_media -- /tmp/viewer/media
.venv-fixtures/bin/python -I scripts/fixtures/make_synthetic_viewer.py /tmp/viewer --seed 1
rm -rf tests/fixtures/viewer && cp -r /tmp/viewer tests/fixtures/viewer
sha256sum tests/fixtures/viewer/SHA256SUMS tests/fixtures/viewer/EXPECTED.SHA256SUMS
```

The script refuses to run under any other SQLite. Windows is not a
generation environment. CI runs no Python; `viewer_fixtures.rs` checks
every file against the two sums.

| | |
|---|---|
| Python | {python} |
| SQLite (Python `sqlite3`) | {sqlite} |
| cryptography | {cryptography} |
| Seed | {seed} |

## Media

| File | What it is |
|---|---|
{media}

## Entries

| Entry | What it covers |
|---|---|
{entries}

## Scenarios

| Scenario | Orders | What it covers |
|---|---|---|
{scenarios}
"""


def media_table(out):
    """The media rows from the example's own purpose strings (read from
    the Rust source, so the README and the code cannot disagree)."""
    src = os.path.join(HERE, "..", "..", "crates", "oma-archive", "examples", "make_viewer_media", "media.rs")
    with open(src, encoding="utf-8") as f:
        text = f.read()
    rows = []
    for chunk in text.split("MediaFile {")[1:]:
        try:
            name = chunk.split('name: "', 1)[1].split('"', 1)[0]
            purpose = chunk.split('purpose: "', 1)[1].split('",', 1)[0].replace('\\"', '"')
        except IndexError:
            continue
        rows.append((name, purpose))
    names = sorted(os.listdir(os.path.join(out, MEDIA_DIR)))
    assert sorted(n for n, _ in rows) == names, "media/ does not match media.rs"
    return "\n".join(f"| `{n}` | {p} |" for n, p in sorted(rows))


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("out")
    ap.add_argument("--seed", type=int, default=1)
    a = ap.parse_args()
    print(f"python {sys.version.split()[0]}, sqlite {sqlite3.sqlite_version}")
    if sqlite3.sqlite_version != REFERENCE_SQLITE:
        sys.exit(f"refusing to write: sqlite {sqlite3.sqlite_version} is not the reference {REFERENCE_SQLITE}")
    out = os.path.abspath(a.out)
    if not os.path.isdir(os.path.join(out, MEDIA_DIR)):
        sys.exit("run the make_viewer_media example into OUT_DIR/media first")
    for item in os.listdir(out):
        if item != MEDIA_DIR:
            p = os.path.join(out, item)
            shutil.rmtree(p) if os.path.isdir(p) else os.remove(p)
    entries = catalogue(out)
    models = {}
    for e in entries:
        rng = random.Random(f"{a.seed}:{e.name}")
        main_db, wal = e.content.built(a.seed)
        exp = os.path.join(out, e.name + ".expected")
        mi.write_backup(e, os.path.join(out, e.name), rng, main_db, wal, exp)
        models[e.name] = mi.EntryModel(e, a.seed)
        ANN.update(ms.annotations(e.content.spec))
    for e in entries:
        steps = [(e.name, "L1", {})]
        content, viewer = run_steps(steps, models)
        write_json(os.path.join(out, e.name + ".expected", "content.json"),
                   document([steps], models, content, viewer))
        size = sum(os.path.getsize(os.path.join(dp, n))
                   for d in (os.path.join(out, e.name), os.path.join(out, e.name + ".expected"))
                   for dp, _, ns in os.walk(d) for n in ns)
        assert size < 4_000_000, (e.name, size)
        print(f"wrote {e.name} ({size} bytes)")
    for name, orders, _ in SCENARIOS:
        results = [run_steps(steps, models) for steps in orders]
        for r in results[1:]:
            assert r == results[0], name
        write_json(os.path.join(out, "scenarios", f"{name}.content.json"),
                   document(orders, models, *results[0]))
        print(f"wrote scenario {name}")
    mi.write_sums(out, "SHA256SUMS", lambda rel: not is_expected(rel))
    mi.write_sums(out, "EXPECTED.SHA256SUMS", is_expected)
    import cryptography
    ent = "\n".join(f"| `{e.name}` | {e.purpose} |" for e in entries)
    scn = "\n".join(f"| `{n}` | {len(o)} | {why} |" for n, o, why in SCENARIOS)
    with open(os.path.join(out, "README.md"), "w", encoding="utf-8", newline="\n") as f:
        text = README
        for k, v in (("python", sys.version.split()[0]), ("sqlite", sqlite3.sqlite_version),
                     ("cryptography", cryptography.__version__), ("seed", str(a.seed)),
                     ("media", media_table(out)), ("entries", ent), ("scenarios", scn)):
            text = text.replace("{" + k + "}", v)
        f.write(text)
    for n in ("SHA256SUMS", "EXPECTED.SHA256SUMS"):
        with open(os.path.join(out, n), "rb") as f:
            print(f"{n} sha256 {hashlib.sha256(f.read()).hexdigest()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
