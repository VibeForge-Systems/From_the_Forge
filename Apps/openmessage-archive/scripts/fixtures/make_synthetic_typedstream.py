#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""Build the SYNTHETIC typedstream test vectors for OpenMessage Archive.

Every value written here is invented. The vectors are test input for the
typedstream decoder in crates/oma-messages (M3 spec section 10).

Clean-room: written from the format facts recorded for M3 (C. Sardegna's
2025 write-up of the format, the libmagic "Apple/NeXT typedstream" entry,
Apple's Objective-C type-encoding table and NSString/NSArchiver
documentation). No code from any GPL or LGPL implementation was read or
copied. See scripts/fixtures/PROVENANCE.md.

Usage:

  make_synthetic_typedstream.py OUT_DIR

Writes OUT_DIR/*.bin, OUT_DIR/expected.json, OUT_DIR/SHA256SUMS and
OUT_DIR/README.md, after removing any older *.bin there. The output is
deterministic: it uses no randomness, no clock and no SQLite, so it is
byte-identical on every machine and Python 3 version.

Before writing, the script checks that its writer reproduces the sample
blob published in Sardegna's write-up (233 bytes, SHA-256 below). Those
bytes are never written: the blob's licence is not stated.

Standard library only. Run with `python -I`.
"""
import argparse
import glob
import hashlib
import json
import os
import struct
import sys

NEW, NIL, END = 0x84, 0x85, 0x86
REF_BIAS = 110  # reference index = (head read as a signed integer) + 110

PUBLISHED_SAMPLE_SHA256 = "33162f000e9abad2ba56ebfcec6ef4ef671eb68048de1b0fc3c6bf2178412d9f"
PUBLISHED_SAMPLE_LEN = 233

# Attribute keys (names as facts, M3 spec Appendix A, T6).
P = "__kIMMessagePartAttributeName"
GUID = "__kIMFileTransferGUIDAttributeName"
FILENAME = "__kIMFilenameAttributeName"
MENTION = "__kIMMentionConfirmedMention"
BOLD = "__kIMTextBoldAttributeName"
ITALIC = "__kIMTextItalicAttributeName"
EFFECT = "__kIMTextEffectAttributeName"
OTC = "__kIMOneTimeCodeAttributeName"
LINK = "__kIMLinkAttributeName"
EMOJI = "__kIMEmojiImageAttributeName"

FLAG_KEYS = {
    BOLD: "bold",
    ITALIC: "italic",
    "__kIMTextUnderlineAttributeName": "underline",
    "__kIMTextStrikethroughAttributeName": "strikethrough",
    OTC: "one_time_code",
    EMOJI: "inline_emoji",
    "IMAudioTranscription": "audio_transcription",
}
KNOWN_KEYS = [
    P, GUID, FILENAME,
    "__kIMInlineMediaWidthAttributeName", "__kIMInlineMediaHeightAttributeName",
    "IMAudioTranscription", MENTION, LINK, EMOJI, EFFECT,
    BOLD, ITALIC, "__kIMTextUnderlineAttributeName", "__kIMTextStrikethroughAttributeName",
    OTC, "__kIMCalendarEventAttributeName", "__kIMDataDetectedAttributeName",
    "__kIMMoneyAttributeName", "__kIMAddressAttributeName",
]
FLAG_ORDER = ["bold", "italic", "underline", "strikethrough", "one_time_code",
              "inline_emoji", "audio_transcription"]

SYNTH_HANDLE = "+15555550100"  # SYNTH: reserved fictional number
SYNTH_GUID_A = "at_0_00000000-0000-4000-8000-000000000001"
SYNTH_GUID_B = "at_2_00000000-0000-4000-8000-000000000002"


# --------------------------------------------------------------------------
# Writer
# --------------------------------------------------------------------------

class Unknown:
    """An attribute value of a class outside the decoder's closed set."""

    def __init__(self, name):
        self.name = name


class BadUtf8:
    """An NSString whose `+` bytes are not UTF-8."""

    def __init__(self, raw):
        self.raw = raw


class W:
    def __init__(self):
        self.b = bytearray()
        self.strings = {}   # shared-string table: bytes -> index
        self.objs = 0       # object table: objects, classes and char * pointers
        self.classes = {}   # class name -> object index
        self.cptr = {}      # char * text -> pointer slot, for pointer reuse
        self.b += bytes([4, 11]) + b"streamtyped"
        self.int(1000)

    def int(self, v):
        if -110 <= v <= 127:
            self.b += struct.pack("<b", v)
        elif -0x8000 <= v <= 0x7FFF:
            self.b += b"\x81" + struct.pack("<h", v)
        elif -0x80000000 <= v <= 0x7FFFFFFF:
            self.b += b"\x82" + struct.pack("<i", v)
        else:
            self.b += b"\x87" + struct.pack("<q", v)

    def ref(self, idx):
        self.int(idx - REF_BIAS)

    def shared(self, s):
        if s in self.strings:
            self.ref(self.strings[s])
        else:
            self.strings[s] = len(self.strings)
            self.b.append(NEW)
            self.int(len(s))
            self.b += s

    def cls(self, chain):
        """chain: [(name, version), ...] from the most derived class up."""
        if not chain:
            self.b.append(NIL)
            return
        name, ver = chain[0]
        if name in self.classes:
            self.ref(self.classes[name])
            return
        self.b.append(NEW)
        self.classes[name] = self.objs
        self.objs += 1
        self.shared(name.encode())
        self.int(ver)
        self.cls(chain[1:])

    def begin_obj(self, chain):
        self.b.append(NEW)
        idx = self.objs
        self.objs += 1
        self.cls(chain)
        return idx

    def end_obj(self):
        self.b.append(END)

    def types(self, t):
        self.shared(t)


NSOBJ = [("NSObject", 0)]
NSSTRING = [("NSString", 1)] + NSOBJ
NSMSTRING = [("NSMutableString", 1)] + NSSTRING
NSDICT = [("NSDictionary", 0)] + NSOBJ
NSNUMBER = [("NSNumber", 0), ("NSValue", 0)] + NSOBJ
NSDATA = [("NSData", 0)] + NSOBJ
NSAS = [("NSAttributedString", 0)] + NSOBJ
NSMAS = [("NSMutableAttributedString", 0)] + NSAS


def w_plus(w, raw):
    w.types(b"+")
    w.int(len(raw))
    w.b += raw


def w_nsstring(w, text, chain=NSSTRING):
    raw = text.raw if isinstance(text, BadUtf8) else text.encode("utf-8")
    w.types(b"@")
    w.begin_obj(chain)
    w_plus(w, raw)
    w.end_obj()


def w_nsnumber(w, value, code=b"i", reuse_ptr=False):
    w.types(b"@")
    w.begin_obj(NSNUMBER)
    w.types(b"*")
    if reuse_ptr and code in w.cptr:
        w.ref(w.cptr[code])
    else:
        w.b.append(NEW)
        w.cptr[code] = w.objs
        w.objs += 1
        w.shared(code)
    w.types(code)
    w.int(value)
    w.end_obj()


def w_unknown(w, name):
    w.types(b"@")
    w.begin_obj([(name, 0)] + NSOBJ)
    w.types(b"i")
    w.int(0)
    w.end_obj()


def w_value(w, v, reuse_ptr=False):
    if isinstance(v, (str, BadUtf8)):
        w_nsstring(w, v)
    elif isinstance(v, bool):
        raise TypeError(v)
    elif isinstance(v, int):
        w_nsnumber(w, v, reuse_ptr=reuse_ptr)
    elif isinstance(v, Unknown):
        w_unknown(w, v.name)
    else:
        raise TypeError(v)


def w_dict(w, items, reuse_ptr=False):
    w.types(b"@")
    w.begin_obj(NSDICT)
    w.types(b"i")
    w.int(len(items))
    for k, v in items:
        w_nsstring(w, k)
        w_value(w, v, reuse_ptr=reuse_ptr)
    w.end_obj()


def utf16_len(text):
    return len(text.encode("utf-16-le")) // 2


def build_attributed(text_raw, runs, dicts, root=NSMAS, string=NSMSTRING,
                     reuse_ptr=False, dict_hook=None):
    """runs: [(dict_no, utf16_len)]; a dictionary is written after a run
    whose number is one more than the dictionaries written so far, when one
    is left in `dicts` (a list of attribute lists, or None for a nil
    object). dict_hook(w, i) may write dictionary i itself."""
    w = W()
    w.types(b"@")
    w.begin_obj(root)
    w.types(b"@")
    w.begin_obj(string)
    w_plus(w, text_raw)
    w.end_obj()
    written = 0
    for idx, ln in runs:
        w.types(b"iI")
        w.int(idx)
        w.int(ln)
        if idx == written + 1 and written < len(dicts):
            d = dicts[written]
            if dict_hook is not None and dict_hook(w, written):
                pass
            elif d is None:
                w.types(b"@")
                w.b.append(NIL)
            else:
                w_dict(w, d, reuse_ptr=reuse_ptr)
            written += 1
    w.end_obj()
    return bytes(w.b)


def segments(segs):
    """segs: [(text, attrs)]. Equal attribute lists share one dictionary."""
    seen = []
    runs = []
    for text, attrs in segs:
        key = tuple(attrs)
        if key not in seen:
            seen.append(key)
        runs.append((seen.index(key) + 1, utf16_len(text)))
    text = "".join(t for t, _ in segs)
    return text, runs, [list(k) for k in seen]


# --------------------------------------------------------------------------
# The decoder's expected result (a second statement of M3 section 10.7)
# --------------------------------------------------------------------------

def attrs_model(items):
    a = {"part": None, "guid": None, "mention": None, "flags": [], "keys": [],
         "text_effect": None}
    counts = {"unknown_attribute_keys": 0, "unknown_attribute_classes": 0,
              "unreadable_attribute_values": 0}
    flags = set()
    keys = []
    for k, v in items:
        if isinstance(v, Unknown):
            counts["unknown_attribute_classes"] += 1
        if k not in KNOWN_KEYS:
            counts["unknown_attribute_keys"] += 1
            continue
        first = k not in keys
        if first:
            keys.append(k)
        if k in (P, EFFECT):
            field = "part" if k == P else "text_effect"
            if isinstance(v, int):
                if first:
                    a[field] = v
            elif first:
                counts["unreadable_attribute_values"] += 1
        elif k in (GUID, MENTION):
            field = "guid" if k == GUID else "mention"
            if isinstance(v, str):
                if first:
                    a[field] = v
            elif first:
                counts["unreadable_attribute_values"] += 1
        elif k in FLAG_KEYS:
            if isinstance(v, int) and v != 0:
                flags.add(FLAG_KEYS[k])
    a["flags"] = [f for f in FLAG_ORDER if f in flags]
    a["keys"] = [k for k in KNOWN_KEYS if k in keys]
    return a, counts


def model(text, runs, dicts):
    """Expected `ok` result for a well-formed attributed string with the
    given runs; follows the validation order of M3 section 10.7."""
    units = []  # (utf16 width, utf8 width) per character
    for ch in text:
        units.append((utf16_len(ch), len(ch.encode("utf-8"))))
    total16 = sum(u for u, _ in units)
    attrs = []      # one per interpreted dictionary
    seen = []       # dictionary number -> attrs index
    counts = {"unknown_attribute_keys": 0, "unknown_attribute_classes": 0,
              "unreadable_attribute_values": 0}
    out = []
    issue = None
    pos16 = pos8 = ci = 0
    written = 0
    for n, (idx, ln) in enumerate(runs):
        if idx == len(seen) + 1:
            d = dicts[written] if written < len(dicts) else None
            if written < len(dicts):
                written += 1
            if d is None:
                issue = {"kind": "BadRunIndex", "run": n}
                break
            a, c = attrs_model(d)
            for k in counts:
                counts[k] += c[k]
            attrs.append(a)
            seen.append(len(attrs) - 1)
        elif not 1 <= idx <= len(seen):
            issue = {"kind": "BadRunIndex", "run": n}
            break
        aid = seen[idx - 1]
        end16 = pos16 + ln
        if end16 > total16:
            issue = {"kind": "RunOverflow", "run": n}
            break
        s16, s8 = pos16, pos8
        while pos16 < end16:
            u16, u8 = units[ci]
            ci += 1
            pos16 += u16
            pos8 += u8
        if pos16 != end16:
            issue = {"kind": "RunSplitsSurrogate", "run": n}
            break
        if out and out[-1]["aid"] == aid and out[-1]["utf16"][1] == s16:
            out[-1]["utf16"][1] = end16
            out[-1]["utf8"][1] = pos8
        else:
            out.append({"aid": aid, "utf16": [s16, end16], "utf8": [s8, pos8]})
    else:
        if pos16 != total16:
            issue = {"kind": "RunLengthMismatch", "run": len(runs)}
    runs_out = [{"utf16": r["utf16"], "utf8": r["utf8"], "attrs": attrs[r["aid"]]}
                for r in out]
    parts = []
    for i, r in enumerate(runs_out):
        a = r["attrs"]
        if parts and parts[-1]["part"] == a["part"]:
            p = parts[-1]
            p["utf16"][1] = r["utf16"][1]
            p["utf8"][1] = r["utf8"][1]
            p["runs"][1] = i + 1
            for f in ("guid", "mention", "text_effect"):
                if p[f] is None:
                    p[f] = a[f]
            p["flags"] = [f for f in FLAG_ORDER if f in p["flags"] or f in a["flags"]]
            p["keys"] = [k for k in KNOWN_KEYS if k in p["keys"] or k in a["keys"]]
        else:
            parts.append({"part": a["part"], "utf16": list(r["utf16"]),
                          "utf8": list(r["utf8"]), "runs": [i, i + 1],
                          "guid": a["guid"], "mention": a["mention"],
                          "text_effect": a["text_effect"], "flags": list(a["flags"]),
                          "keys": list(a["keys"])})
    res = {"text": text, "runs": runs_out, "parts": parts, "run_issue": issue}
    res.update(counts)
    return res


def oracle_view_attributed(text, runs, dicts):
    """What the structural (oracle) decode shows: text, runs as written,
    dictionaries as written."""
    def val(v):
        if isinstance(v, Unknown):
            return {"class": v.name}
        if isinstance(v, int):
            return {"int": v}
        return {"str": v}
    return {"kind": "attributed", "text": text, "runs": [list(r) for r in runs],
            "dicts": [None if d is None else [[k, val(v)] for k, v in d] for d in dicts]}


# --------------------------------------------------------------------------
# Vector catalogue
# --------------------------------------------------------------------------

VECTORS = []


def add(name, data, public, generic=None, oracle=None, note=""):
    assert not any(v["name"] == name for v in VECTORS), name
    VECTORS.append({"name": name, "data": data, "public": public,
                    "generic": generic, "oracle": oracle or {"check": "exempt",
                                                             "reason": "not set"},
                    "note": note})


def compare(view):
    return {"check": "compare", "view": view}


def must_fail(reason):
    return {"check": "must_fail", "reason": reason}


def exempt(reason):
    return {"check": "exempt", "reason": reason}


def ok(res):
    return {"ok": res}


def err(kind):
    return {"error": kind}


def add_attributed(name, segs, note="", reuse_ptr=False, root=NSMAS, string=NSMSTRING):
    text, runs, dicts = segments(segs)
    data = build_attributed(text.encode("utf-8"), runs, dicts, root=root,
                            string=string, reuse_ptr=reuse_ptr)
    add(name, data, ok(model(text, runs, dicts)),
        oracle=compare(oracle_view_attributed(text, runs, dicts)), note=note)
    return data


def published_sample_check():
    text, runs, dicts = segments([("Noter test", [(P, 0)])])
    data = build_attributed(text.encode(), runs, dicts)
    digest = hashlib.sha256(data).hexdigest()
    if len(data) != PUBLISHED_SAMPLE_LEN or digest != PUBLISHED_SAMPLE_SHA256:
        sys.exit(f"writer does not reproduce the published sample: {len(data)} bytes, {digest}")
    return digest


HDR = bytes([4, 11]) + b"streamtyped" + b"\x81\xe8\x03"


def build_catalogue():
    # ---- attributed strings (v*) -------------------------------------------
    add_attributed("v1_hello", [("Hello", [(P, 0)])])
    add_attributed("v2_emoji", [("Hi \U0001F44B\U0001F3FD!", [(P, 0)])],
                   note="non-BMP characters and a skin-tone modifier: 12 UTF-8 bytes, 8 UTF-16 units")
    add_attributed("v3_attachment_then_text",
                   [("￼", [(P, 0), (GUID, SYNTH_GUID_A), (FILENAME, "SYNTH_0001.HEIC")]),
                    ("Look", [(P, 1)])])
    add_attributed("v4_mention_reuse",
                   [("Hey ", [(P, 0)]), ("Alex", [(P, 0), (MENTION, SYNTH_HANDLE)]),
                    (" été \U0001F600", [(P, 0)])],
                   note="the third run reuses dictionary 1; no dictionary follows it")
    add_attributed("v5_long", [("x" * 300, [(P, 0)])],
                   note="`+` and run lengths over 127 use the 0x81 form")
    add_attributed("v6_mention_after_emoji",
                   [("\U0001F600 ", [(P, 0)]), ("Alex", [(P, 0), (MENTION, SYNTH_HANDLE)]),
                    (" \U0001F44B\U0001F3FD ok", [(P, 0)])],
                   note="mention placement after a non-BMP character (UTF-16 run lengths)")
    add_attributed("v7_cstring_ptr_reuse", [("Hey ", [(P, 0)]), ("Alex", [(P, 1)])],
                   reuse_ptr=True,
                   note="the second NSNumber's objCType is a reference to the first char * pointer")
    add_attributed("v8_formatting",
                   [("SYNTH ", [(P, 0)]),
                    ("bold", [(P, 0), (BOLD, 1), (ITALIC, 0), (EFFECT, 5)]),
                    (" 123456", [(P, 0), (OTC, 1), ("__kSYNTHUnknownKey", "x")]),
                    (" link", [(P, 0), (LINK, Unknown("SYNTHUnknownValue"))])],
                   note="flags from NSNumber values; key presence; an unknown key; an unknown value class")
    add_attributed("v9_multipart",
                   [("￼", [(P, 0), (GUID, SYNTH_GUID_A)]), ("caption", [(P, 1)]),
                    ("￼", [(P, 2), (GUID, SYNTH_GUID_B)])])
    add_attributed("v10_empty", [], note="an empty string with no runs")
    add_attributed("v11_immutable_classes", [("plain", [(P, 0)])], root=NSAS, string=NSSTRING,
                   note="NSAttributedString holding an NSString")

    # ---- scalars (int_*) ---------------------------------------------------
    ints = [
        ("i", "00", 0), ("i", "7f", 127), ("i", "80", "UnexpectedTag"), ("i", "92", -110),
        ("i", "a0", -96), ("i", "ff", -1), ("i", "81 c8 00", 200), ("i", "81 88 ff", -120),
        ("i", "82 00 00 10 00", 1048576), ("i", "87 00 e6 8e e7 fd ff ff ff", -9000000000),
        ("I", "c8", 200), ("I", "ff", 255), ("I", "81 c8 00", 200), ("I", "81 ff ff", 65535),
        ("c", "ff", -1), ("C", "ff", 255), ("q", "87 00 e6 8e e7 fd ff ff ff", -9000000000),
        ("Q", "87 ff ff ff ff ff ff ff ff", 18446744073709551615),
        ("f", "83 00 00 60 40", 3.5), ("d", "83 00 00 00 00 00 20 59 40", 100.5),
        ("d", "05", 5.0), ("f", "81 e8 03", 1000.0), ("i", "83 00 00 60 40", "UnexpectedTag"),
        ("i", "84", "UnexpectedTag"), ("i", "85", "UnexpectedTag"), ("i", "86", "UnexpectedTag"),
        ("i", "88", "UnexpectedTag"), ("i", "91", "UnexpectedTag"), ("B", "01", True),
        ("s", "81 00 80", -32768), ("l", "82 ff ff ff 7f", 2147483647),
    ]
    assert len(ints) == 31
    for t, hx, want in ints:
        name = f"int_{t}_{hx.replace(' ', '')}"[:30]
        data = HDR + b"\x84\x01" + t.encode() + bytes.fromhex(hx)
        if isinstance(want, str):
            add(name, data, err(want), generic={"error": want},
                oracle=must_fail("not a valid integer head for this type"))
            continue
        if isinstance(want, bool):
            gv = {"int": int(want)}
        elif isinstance(want, float):
            gv = {"real": want}
        else:
            gv = {"int": want}
        view = {"kind": "scalar", "type": t, "value": want}
        orc = (exempt("pytypedstream 0.1.0 rejects 0x87; T1 observed it from Apple's encoder")
               if hx.startswith("87") else compare(view))
        add(name, data, err("NotAttributedString"), generic={"groups": [[gv]]}, oracle=orc)

    # ---- references (ref_*) ------------------------------------------------
    def refs(n, tail):
        w = W()
        for i in range(n):
            w_nsstring(w, f"s{i}")
        # objects: #0 the first string, #1 NSString class, #2 NSObject class,
        # then one object per later string (#k is s(k-2))
        w.types(b"@")
        w.b += tail
        return bytes(w.b)

    def ref_case(name, n, tail, target):
        data = refs(n, tail)
        strings = [f"s{i}" for i in range(n)]
        if target is None:
            add(name, data, err("TrailingBytes"), generic={"error": "BadReference"},
                oracle=must_fail("the reference points past the object table"))
            return
        groups = [[{"nsstring": s}] for s in strings + [f"s{target - 2}"]]
        add(name, data, err("TrailingBytes"), generic={"groups": groups},
            oracle=compare({"kind": "strings", "strings": strings + [f"s{target - 2}"]}))

    ref_case("ref_small_obj3", 5, bytes([0x92 + 3]), 3)
    ref_case("ref_obj120_i8", 150, struct.pack("<b", 120 - REF_BIAS), 120)
    ref_case("ref_obj120_81i16", 150, b"\x81" + struct.pack("<h", 120 - REF_BIAS), 120)
    ref_case("ref_obj120_81u16", 150, b"\x81" + struct.pack("<H", 120 + 0x92), None)
    ref_case("ref_obj250_81i16", 300, b"\x81" + struct.pack("<h", 250 - REF_BIAS), 250)
    ref_case("ref_obj250_81u16", 300, b"\x81" + struct.pack("<H", 250 + 0x92), None)

    # ---- other value shapes ------------------------------------------------
    payload = bytes([0x84, 0x85, 0x86, 0x00, 0xFF])
    w = W()
    w.types(b"@")
    w.begin_obj(NSDATA)
    w.types(b"i")
    w.int(len(payload))
    w.types(b"[5c]")
    w.b += payload
    w.end_obj()
    add("nsdata_5c", bytes(w.b), err("NotAttributedString"),
        generic={"groups": [[{"nsdata": payload.hex()}]]},
        oracle=compare({"kind": "data", "hex": payload.hex()}),
        note="NSData: an `i` length, then `[5c]` raw bytes that look like tags")
    add("array_5c", HDR + b"\x84\x04[5c]" + payload, err("NotAttributedString"),
        generic={"groups": [[{"bytes": payload.hex()}]]},
        oracle=compare({"kind": "bytes", "type": "[5c]", "hex": payload.hex()}))
    t = b"{_NSRange=QQ}"
    add("struct_nsrange", HDR + b"\x84" + bytes([len(t)]) + t + b"\x05\x81\x2c\x01",
        err("NotAttributedString"), generic={"groups": [[{"int": 5}, {"int": 300}]]},
        oracle=compare({"kind": "struct", "type": t.decode(), "values": [5, 300]}),
        note="struct members are written flat; the name contains a type letter (S)")

    # ---- hostile (h_*): the ten from the research run ----------------------
    good_text = "Hi \U0001F44B!"
    gt, gr, gd = segments([(good_text, [(P, 0)])])
    good = build_attributed(gt.encode(), gr, gd)

    def custom(text_raw, runs, dicts, **kw):
        return build_attributed(text_raw, runs, dicts, **kw)

    add("h_truncated", good[:-5], err("Truncated"), oracle=must_fail("truncated"))
    add("h_trailing", good + b"\x00", err("TrailingBytes"), oracle=must_fail("a byte after the root"))
    add("h_bad_version", bytes([5]) + good[1:], err("UnsupportedHeader"),
        oracle=must_fail("streamer version 5"))
    add("h_bigendian_sig", good[:2] + b"typedstream" + good[13:], err("UnsupportedHeader"),
        oracle=exempt("pytypedstream reads big-endian streams; the decoder refuses them by policy (M3 10.1)"))
    for name, raw, runs, dicts in [
        ("h_runlen_short", good_text.encode(), [(1, 5)], [[(P, 0)]]),
        ("h_runlen_utf8count", good_text.encode(), [(1, 8)], [[(P, 0)]]),
        ("h_split_surrogate", good_text.encode(), [(1, 4), (1, 2)], [[(P, 0)]]),
        ("h_bad_run_index", b"Hi", [(2, 2)], []),
    ]:
        text = raw.decode()
        add(name, custom(raw, runs, dicts), ok(model(text, runs, dicts)),
            oracle=compare(oracle_view_attributed(text, runs, dicts)),
            note="run fault: validated, the text is kept")
    add("h_invalid_utf8", custom(b"Hi\xff", [(1, 3)], [[(P, 0)]]), err("InvalidUtf8"),
        oracle=must_fail("the string bytes are not UTF-8"))
    add("h_dangling_ref", HDR + b"\x84\x01\x40\xa0", err("BadReference"),
        oracle=must_fail("object reference 14 with an empty table"))

    # ---- hostile: new in M3 ------------------------------------------------
    def nested_objects(depth_total):
        """An attributed string whose dictionary value nests unknown-class
        objects so that the innermost object is at `depth_total`."""
        def hook(w, i):
            w.types(b"@")
            w.begin_obj(NSDICT)            # depth 2
            w.types(b"i")
            w.int(1)
            w_nsstring(w, LINK)            # depth 3
            levels = depth_total - 2       # objects at depths 3..depth_total
            for _ in range(levels):
                w.types(b"@")
                w.begin_obj([("SYNTHNest", 0)] + NSOBJ)
            w.types(b"i")
            w.int(0)
            for _ in range(levels):
                w.end_obj()
            w.end_obj()
            return True
        return build_attributed(b"x", [(1, 1)], [[]], dict_hook=hook)

    def nested_struct(struct_depth):
        """A dictionary value (an unknown-class object at depth 3) holding a
        value whose type nests `struct_depth` structs."""
        def hook(w, i):
            w.types(b"@")
            w.begin_obj(NSDICT)
            w.types(b"i")
            w.int(1)
            w_nsstring(w, LINK)
            w.types(b"@")
            w.begin_obj([("SYNTHNest", 0)] + NSOBJ)
            t = b"{a=" * struct_depth + b"i" + b"}" * struct_depth
            w.types(t)
            w.int(7)
            w.end_obj()
            w.end_obj()
            return True
        return build_attributed(b"x", [(1, 1)], [[]], dict_hook=hook)

    deep_ok = {"text": "x", "runs": [{"utf16": [0, 1], "utf8": [0, 1], "attrs": {
        "part": None, "guid": None, "mention": None, "flags": [], "keys": [LINK],
        "text_effect": None}}], "parts": [{"part": None, "utf16": [0, 1], "utf8": [0, 1],
        "runs": [0, 1], "guid": None, "mention": None, "text_effect": None, "flags": [],
        "keys": [LINK]}], "run_issue": None, "unknown_attribute_keys": 0,
        "unknown_attribute_classes": 1, "unreadable_attribute_values": 0}
    policy = "a decoder budget (M3 10.6); pytypedstream has no such limit"
    add("h_depth64_objects", nested_objects(64), ok(deep_ok), oracle=exempt(policy),
        note="objects nested to depth 64 (the maximum)")
    add("h_depth65_objects", nested_objects(65), err("TooDeep"), oracle=exempt(policy))
    add("h_depth64_structs", nested_struct(61), ok(deep_ok), oracle=exempt(policy),
        note="61 nested structs inside an object at depth 3: depth 64")
    add("h_depth65_structs", nested_struct(62), err("TooDeep"), oracle=exempt(policy))

    # A string object whose only value is a reference to itself.
    w = W()
    w.types(b"@")
    w.begin_obj(NSMAS)          # object 0 (classes 1, 2, 3)
    w.types(b"@")
    assert w.begin_obj(NSMSTRING) == 4   # object 4 (classes 5, 6), under construction
    w.types(b"@")
    w.ref(4)
    w.end_obj()
    w.end_obj()
    add("h_self_reference", bytes(w.b), err("BadReference"),
        oracle=must_fail("a reference to an object under construction"))

    w = W()
    w.types(b"+")
    w.b += b"\x82" + struct.pack("<I", 1 << 31) + b"abc"
    add("h_plus_len_2pow31", bytes(w.b), err("Truncated"), oracle=must_fail("length 2^31"))
    add("h_array_huge", HDR + b"\x84\x0d[4294967295c]" + b"abc", err("Truncated"),
        oracle=must_fail("array length past the end"))
    add("h_array_4i", HDR + b"\x84\x04[4i]" + b"\x01\x02\x03\x04", err("UnsupportedType"),
        oracle=exempt("arrays of non-char elements are refused by policy (M3 10.4)"))
    for name, t in [("h_union", b"(u=ic)"), ("h_pointer", b"^i"), ("h_bitfield", b"b4")]:
        add(name, HDR + b"\x84" + bytes([len(t)]) + t + b"\x01\x02", err("UnsupportedType"),
            oracle=must_fail("unions, pointers and bitfields cannot be archived"))
    add("h_int_tag_80", custom(b"Hi", [(1, 2)], [[(P, 0)]]).replace(b"iI\x01\x02", b"iI\x01\x80", 1),
        err("UnexpectedTag"), oracle=must_fail("0x80 as an integer head"))
    add("h_not_attributed", HDR + b"\x84\x01@\x84\x84\x84\x08NSString\x01\x84\x84\x08NSObject\x00\x85"
        b"\x84\x01+\x05SYNTH\x86", err("NotAttributedString"),
        oracle=compare({"kind": "strings", "strings": ["SYNTH"]}))
    add("h_run_index_zero", custom(b"Hi", [(0, 2)], []),
        ok(model("Hi", [(0, 2)], [])),
        oracle=compare(oracle_view_attributed("Hi", [(0, 2)], [])))
    add("h_nil_dictionary", custom(b"Hi", [(1, 2)], [None]),
        ok(model("Hi", [(1, 2)], [None])),
        oracle=compare(oracle_view_attributed("Hi", [(1, 2)], [None])))
    add("h_shared_ref_oob", HDR + b"\x84\x01@\x84\x84\x84\x08NSString\x01\x84\x84\x08NSObject\x00\x85"
        b"\x95\x05", err("BadReference"),
        oracle=must_fail("a type reference to an undefined shared string"))
    add("h_class_self_super", HDR + b"\x84\x01@\x84\x84\x84\x08NSString\x01\x93\x86",
        err("BadReference"), oracle=must_fail("a superclass reference to the class being read"))
    add("h_empty_after_header", HDR, err("Truncated"),
        oracle=exempt("an empty stream is valid typedstream; an attributedBody needs a root object"))
    add("h_attr_invalid_utf8",
        custom(b"Hi", [(1, 2)], [[(P, 0), (MENTION, BadUtf8(b"+1\xff"))]]),
        ok(model("Hi", [(1, 2)], [[(P, 0), (MENTION, BadUtf8(b"+1\xff"))]])),
        oracle=must_fail("an attribute string that is not UTF-8"),
        note="an attribute value that is not UTF-8 is counted, the text is kept")


def to_json_vector(v):
    out = {"name": v["name"], "file": v["name"] + ".bin",
           "sha256": hashlib.sha256(v["data"]).hexdigest(), "public": v["public"],
           "generic": v["generic"], "oracle": v["oracle"]}
    if v["note"]:
        out["note"] = v["note"]
    return out


README = """\
# Synthetic typedstream vectors

Test input for the typedstream decoder in `crates/oma-messages` (M3 spec
section 10). **Every value in these files is invented.** None was taken
from a real device or message. The mention handle `{handle}` is a
fictional number (`SYNTH`).

Every file here is written by
[`scripts/fixtures/make_synthetic_typedstream.py`](../../../scripts/fixtures/make_synthetic_typedstream.py),
which is clean-room code (see
[`scripts/fixtures/PROVENANCE.md`](../../../scripts/fixtures/PROVENANCE.md)).
Do not edit them by hand.

## Layout

```text
<name>.bin      one typedstream blob
expected.json   for each vector: the decoder's expected public result
                ("public"), the generic decode checked by the crate's unit
                tests ("generic"), and the oracle check ("oracle")
SHA256SUMS      every file except this one and README.md
```

Vector families: `v*` attributed strings, `int_*` the 31 integer-head
probes, `ref_*` references (including indexes of 110 and above),
`nsdata_5c`, `array_5c`, `struct_nsrange`, and `h_*` hostile inputs. The
largest hostile inputs (16 MiB + 1, zero-width type elements, a type string
referenced a million times, 300,000 references to one object, half a million
runs sharing an 8 MiB dictionary) are generated inside
`crates/oma-messages/tests/typedstream_vectors.rs` instead of being
committed.

`.gitattributes` marks `tests/fixtures/**` as binary. The
`fixtures_integrity` test in `crates/oma-messages` checks every file
against `SHA256SUMS` and pins the SHA-256 of `SHA256SUMS` itself, so any
change shows up as a test failure that points at the oracle record.

## Trust anchors

1. **The published sample.** Before writing, the generator checks that its
   writer reproduces the 233-byte sample blob from C. Sardegna's write-up
   (SHA-256 `{sample}`). Those bytes are not
   committed: the blob's licence is not stated.
2. **Oracle 2 (black box).** `scripts/fixtures/verify_typedstream_with_oracle.py`
   runs `pytypedstream` 0.1.0 (LGPL-3.0-or-later, pinned with its wheel
   hash in `scripts/fixtures/requirements-oracle-typedstream.txt`) as a
   subprocess. It never imports it. Each vector's `oracle.check` says what
   is compared:
   - `compare`: the oracle decodes the vector, and its structure (text,
     runs and dictionaries as written; scalars; strings) equals
     `oracle.view`;
   - `must_fail`: the oracle rejects the vector;
   - `exempt`: not checked, with the reason in `oracle.reason`. The `0x87`
     vectors are exempt because pytypedstream 0.1.0 rejects `0x87`, which
     Sardegna observed from Apple's encoder. Budget and policy vectors are
     exempt because they test our own limits.

   The run is recorded in `docs/compatibility.md`, keyed to the SHA-256 of
   `SHA256SUMS`.

Both anchors show only that our writer and our decoder agree with one
published blob and one independent decoder. Confirmation from Apple's own
encoder needs a Mac (a hardware question in `docs/supported-formats.md`).

## Regenerating

Linux (Python 3, standard library only; no SQLite involved, so the output
is byte-identical everywhere):

```sh
python3 -I scripts/fixtures/make_synthetic_typedstream.py tests/fixtures/typedstream
python3 -m venv .venv-oracle-ts
.venv-oracle-ts/bin/pip install --require-hashes -r scripts/fixtures/requirements-oracle-typedstream.txt
.venv-oracle-ts/bin/python -I scripts/fixtures/verify_typedstream_with_oracle.py tests/fixtures/typedstream
sha256sum tests/fixtures/typedstream/SHA256SUMS      # record with the oracle run
```

Windows is not a supported generation environment; the tests only read the
committed files.

| | |
|---|---|
| Generated with | Python 3.13.16 |
| Clean-room review | Pending |
"""


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("out_dir")
    args = ap.parse_args()
    sample = published_sample_check()
    build_catalogue()
    os.makedirs(args.out_dir, exist_ok=True)
    for old in glob.glob(os.path.join(args.out_dir, "*.bin")):
        os.remove(old)
    sums = []
    for v in VECTORS:
        path = os.path.join(args.out_dir, v["name"] + ".bin")
        with open(path, "wb") as f:
            f.write(v["data"])
    doc = {"format": 1,
           "generator": "scripts/fixtures/make_synthetic_typedstream.py",
           "vectors": [to_json_vector(v) for v in VECTORS]}
    with open(os.path.join(args.out_dir, "expected.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(doc, f, indent=1, sort_keys=True, ensure_ascii=True)
        f.write("\n")
    names = sorted([v["name"] + ".bin" for v in VECTORS] + ["expected.json"])
    for n in names:
        with open(os.path.join(args.out_dir, n), "rb") as f:
            sums.append(f"{hashlib.sha256(f.read()).hexdigest()}  {n}\n")
    with open(os.path.join(args.out_dir, "SHA256SUMS"), "w", encoding="ascii", newline="\n") as f:
        f.writelines(sums)
    with open(os.path.join(args.out_dir, "README.md"), "w", encoding="utf-8", newline="\n") as f:
        f.write(README.format(handle=SYNTH_HANDLE, sample=sample))
    print(f"published sample reproduced: {sample}")
    print(f"wrote {len(VECTORS)} vectors to {args.out_dir}")


if __name__ == "__main__":
    main()
