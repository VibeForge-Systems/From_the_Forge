#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""The TXT oracle (M6 spec 14.4, "JSON <-> TXT", here archive <-> TXT until
the JSON writer exists): parse every TXT file of an export bundle back with
the line grammar of spec 5.4 and compare it with the archive.

Usage (from Apps/openmessage-archive, in the oracle virtual environment):

  OMA_ORACLE_TXT_OUT=DIR cargo test -p oma-export --test txt -- \\
      --ignored --exact write_oracle_txt
  python -I scripts/oracles/txt_grammar.py --exports DIR

write_oracle_txt imports every catalogue entry into its own library and
exports the whole archive to a TXT bundle (files copied, times in UTC,
markers on, LF). This script, independently of the Rust export code:

- byte scan: UTF-8 without a BOM; no C0 control but LF (TAB only in
  recorded text), no C1, DEL, CR, U+2028 or U+2029; every U+27E8 opens
  one of the export's markers;
- grammar: the header block, day lines and the undated section (last),
  item lines at column 0 (time, [item N], sender, tags; no bracket inside
  a sender), labels at four spaces, recorded text at six; no bidi or
  invisible character on an item or label line, nor in recorded text
  (markers are on); the footer's counts;
- against the archive (stdlib sqlite3 on a read-only copy): messages are
  numbered by their rank among all message ids; conversation n is the
  n-th conversation id with a timeline row; each file's items are its
  conversation's chat_message rows, in (sort_key, message_id) order, less
  the folded reaction rows the footer counts; the no-conversation file
  holds exactly the messages in no conversation; each item's time is its
  date_utc_ns in UTC (seconds), [Undated] exactly when its sort key is
  undated, and each day line is the UTC date of the items under it; each
  item's "Text:" block, with the markers undone and lines joined, equals
  the current body's display_text (line breaks normalised) when it holds
  no placeholder; a resolved sender's address, unmarked, equals the
  handle's address; and the bundle's SHA256SUMS verifies;
- masking: no sentinel of the entry's export.json appears in any byte of
  the bundle, marked or not.

Output is counts and check names only (never content). Run with
`python3 -I`; only the standard library is used.
"""
import argparse
import hashlib
import json
import pathlib
import re
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.dont_write_bytecode = True

import project_archive  # noqa: E402  (open_copy and rows only)

FIXTURES = HERE.parent.parent / "tests" / "fixtures"
# Floors of the masking scan (see `main`): the export catalogue's entries
# with sentinels, and their distinct (kind, sentinel) pairs.
MIN_EXPORT_ENTRIES = 7
MIN_SENTINELS = 14

FIRST = "OpenMessage Archive export — plaintext copy. See the end of this file for details."
OPEN, CLOSE = "⟨", "⟩"
PLACEHOLDERS = {"one-time code hidden", "earlier text not included",
                "Inline emoji image (not shown)",
                "Attachment placeholder not matched to a file"}
MARKER = re.compile(r"U\+[0-9A-F]{4,6}|\+\d+ combining marks|Attachment \d+")
TIME = re.compile(r"(-?\d{4,}|\+\d+)-\d{2}-\d{2} \d{2}:\d{2}(:\d{2})?( AM| PM)? (Z|[+-]\d{2}:\d{2}(:\d{2})?)")
DAY = re.compile(r"==== ((?:-?\d{4,}|\+\d+)-\d{2}-\d{2}) ====")
# What "Copy text without invisible characters" drops, as far as a raw
# byte scan can see it: bidi controls, zero-width and invisible
# characters and tags. (Joiners inside emoji are allowed; they are never
# on a header line in the catalogues, and the Rust test applies the full
# rule.)
BIDI = set("؜‎‏‪‫‬‭‮⁦⁧⁨⁩")
INVISIBLE = set("​⁠﻿￼")


class Failures:
    def __init__(self):
        self.items = []
        self.checks = 0

    def check(self, ok, what):
        self.checks += 1
        if not ok:
            self.items.append(what)
        return ok


def load(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def markers_ok(s):
    rest = s
    while OPEN in rest:
        i = rest.index(OPEN)
        after = rest[i + 1:]
        if CLOSE not in after:
            return False
        inner = after[:after.index(CLOSE)]
        if not (MARKER.fullmatch(inner) or inner in PLACEHOLDERS):
            return False
        rest = after[after.index(CLOSE) + 1:]
    return True


def unmark(s):
    def one(m):
        inner = m.group(1)
        if re.fullmatch(r"U\+[0-9A-F]{4,6}", inner):
            return chr(int(inner[2:], 16))
        return m.group(0)
    return re.sub(OPEN + "([^" + CLOSE + "]*)" + CLOSE, one, s)


def raw_bad(s, tab_ok):
    for c in s:
        o = ord(c)
        if (o < 0x20 and not (c == "\t" and tab_ok)) or 0x7F <= o <= 0x9F or c in "  ":
            return True
    return False


def hidden(s):
    in_flag = False
    for c in s:
        o = ord(c)
        if 0xE0020 <= o <= 0xE007F and in_flag:
            continue  # inside a flag's tag sequence, as the copy rule keeps it
        in_flag = c == "\U0001F3F4"
        if c in BIDI or c in INVISIBLE or 0xE0000 <= o <= 0xE007F:
            return True
    return False


def parse_item_line(line):
    m = re.match(r"\[([^\]]*)\] \[item (\d+)\]", line)
    if not m:
        return None
    time, n = m.group(1), int(m.group(2))
    if time not in ("Undated", "No usable time") and not TIME.fullmatch(time):
        return None
    rest = line[m.end():]
    sender = None
    if rest.startswith(" ") and not rest.startswith(" ["):
        end = rest.find(" [")
        end = len(rest) if end < 0 else end
        sender = rest[1:end]
        if not sender or "[" in sender or "]" in sender:
            return None
        rest = rest[end:]
    tags = []
    while rest:
        m = re.match(r" \[([^\[\]]+)\]", rest)
        if not m:
            return None
        tags.append(m.group(1))
        rest = rest[m.end():]
    return {"time": time, "n": n, "sender": sender, "tags": tags, "labels": []}


def parse(text, name, fails):
    """One TXT file: its conversation number and items, or None."""
    ok = fails.check
    if not ok(not text.startswith("﻿"), f"{name}: no BOM"):
        return None
    if not ok(text.endswith("\n") and "\r" not in text, f"{name}: LF line ends"):
        return None
    lines = text[:-1].split("\n")
    if not ok(lines[0] == FIRST, f"{name}: first line"):
        return None
    i = 1
    m = re.fullmatch(r"Conversation (\d+): (.*)", lines[i])
    conversation = int(m.group(1)) if m else None
    if not m and not ok(lines[i] == "No conversation: messages recorded in no conversation",
                        f"{name}: name line"):
        return None
    header = [lines[i]]
    i += 1
    if conversation is not None:
        if not ok(re.fullmatch(r"Addresses recorded in this conversation \(includes people who left\): \d+",
                               lines[i]) is not None, f"{name}: participants line"):
            return None
        i += 1
        while lines[i].startswith("Other recorded name of this conversation: "):
            header.append(lines[i])
            i += 1
    if not ok(re.fullmatch(r"Times: [A-Za-z0-9_+\-/]+ \(offsets shown on every line\)", lines[i]) is not None,
              f"{name}: zone line"):
        return None
    for h in header:
        ok(not raw_bad(h, False) and markers_ok(h) and not hidden(h), f"{name}: header line escaped")
    i += 1
    items, days, undated, day = [], [], False, None
    while i < len(lines):
        line = lines[i]
        i += 1
        if line == "":
            continue
        if line == "==== End of conversation file ====":
            break
        if line == "==== Undated ====":
            undated = True
            continue
        dm = DAY.fullmatch(line)
        if dm:
            ok(not undated, f"{name}: a day after the undated section")
            day = dm.group(1)
            days.append(day)
            continue
        if line.startswith("      "):
            c = line[6:]
            ok(not raw_bad(c, True) and markers_ok(c) and not hidden(c), f"{name}: recorded text escaped")
            if ok(bool(items) and bool(items[-1]["labels"]), f"{name}: text under a label"):
                items[-1]["labels"][-1][1].append(c)
            continue
        if line.startswith("    "):
            c = line[4:]
            ok(not c.startswith(" "), f"{name}: indentation is 4 or 6")
            ok(not raw_bad(c, False) and markers_ok(c) and not hidden(c), f"{name}: label escaped")
            if ok(bool(items), f"{name}: label under an item"):
                items[-1]["labels"].append([c, []])
            continue
        ok(not raw_bad(line, False) and markers_ok(line) and not hidden(line), f"{name}: item line escaped")
        it = parse_item_line(line)
        if not ok(it is not None, f"{name}: item line grammar"):
            continue
        it["day"] = day
        it["section"] = "undated" if undated else "dated"
        ok((it["time"] == "Undated") == undated, f"{name}: [Undated] only in the undated section")
        items.append(it)
    footer = lines[i:i + 2]
    fm = [re.fullmatch(r"Items: (\d+)", footer[0]) if footer else None,
          re.fullmatch(r"Reactions shown on the messages they react to: (\d+)", footer[1])
          if len(footer) > 1 else None]
    if not ok(all(fm), f"{name}: footer counts"):
        return None
    ok(int(fm[0].group(1)) == len(items), f"{name}: footer item count")
    for rest in lines[i + 2:]:
        ok(not raw_bad(rest, False), f"{name}: footer escaped")
    return {"conversation": conversation, "items": items, "folded": int(fm[1].group(1))}


def utc_text(ns):
    import datetime
    s = ns // 1_000_000_000
    t = datetime.datetime(1970, 1, 1) + datetime.timedelta(seconds=s)
    return t.strftime("%Y-%m-%d %H:%M:%S") + " +00:00", t.strftime("%Y-%m-%d")


def normalise(t):
    return t.replace("\r\n", "\n").replace("\r", "\n").replace(" ", "\n") \
        .replace(" ", "\n").replace("\u0085", "\n")


def check_export(root, entry, fails, seen):
    name = entry["name"]
    ok = fails.check
    bundle = root / entry["bundle"]
    # SHA256SUMS, with hashlib.
    for line in (bundle / "SHA256SUMS").read_text(encoding="utf-8").splitlines():
        hexd, rel = line.split("  ", 1)
        ok(hashlib.sha256((bundle / rel).read_bytes()).hexdigest() == hexd, f"{name}: SHA256SUMS")
    with tempfile.TemporaryDirectory(prefix="oma-txt-oracle-") as tmp:
        con = project_archive.open_copy(root / entry["archive"], tmp)
        try:
            rows = lambda sql, a=(): project_archive.rows(con, sql, a)
            ids = sorted(r[0] for r in rows("SELECT id FROM message"))
            rank = {m: k + 1 for k, m in enumerate(ids)}
            by_n = {k + 1: m for k, m in enumerate(ids)}
            chats = [r[0] for r in rows(
                "SELECT DISTINCT chat_id FROM chat_message ORDER BY chat_id")]
            msg = {}
            for mid, sk, utc, fm, fm_st, href in rows(
                    "SELECT id, sort_key, date_utc_ns, is_from_me, is_from_me_st, handle_ref FROM message"):
                msg[mid] = {"dated": sk is not None and sk[:1] < b"\x01", "utc": utc,
                            "fromMe": fm_st is None and fm == 1, "handle": href}
            text = dict(rows("SELECT m.id, b.display_text FROM message m "
                             "JOIN message_body b ON b.id = m.current_body_id"))
            address = dict(rows("SELECT id, address FROM handle"))
            in_chat = {r[0] for r in rows("SELECT DISTINCT message_id FROM chat_message")}
            files = sorted((bundle / "txt").iterdir())
            seen_files = 0
            texts_compared = 0
            for f in files:
                parsed = parse(f.read_text(encoding="utf-8"), f"{name}/{f.name}", fails)
                if parsed is None:
                    continue
                seen_files += 1
                c = parsed["conversation"]
                if c is None:
                    ok(f.name == "no-conversation.txt", f"{name}: no-conversation file name")
                    want = [r[0] for r in rows(
                        "SELECT id FROM message m WHERE NOT EXISTS "
                        "(SELECT 1 FROM chat_message WHERE message_id = m.id) ORDER BY sort_key, id")]
                    got = [by_n.get(it["n"]) for it in parsed["items"]]
                    ok(got == want, f"{name}: the no-conversation file holds exactly those messages")
                    ok(parsed["folded"] == 0, f"{name}: nothing folds outside a conversation")
                else:
                    ok(f.name == f"c{c:04d}.txt", f"{name}: conversation file name")
                    if not ok(1 <= c <= len(chats), f"{name}: conversation ordinal"):
                        continue
                    timeline = [r[0] for r in rows(
                        "SELECT message_id FROM chat_message WHERE chat_id = ? "
                        "ORDER BY sort_key, message_id", (chats[c - 1],))]
                    got = [by_n.get(it["n"]) for it in parsed["items"]]
                    ok(len(got) + parsed["folded"] == len(timeline),
                       f"{name}: c{c}: items and folded rows equal chat_message rows")
                    pos = [timeline.index(m) if m in timeline else -1 for m in got]
                    ok(-1 not in pos and pos == sorted(pos), f"{name}: c{c}: items in timeline order")
                for it in parsed["items"]:
                    m = by_n.get(it["n"])
                    if not ok(m is not None, f"{name}: item number in range"):
                        continue
                    info = msg[m]
                    ok((it["time"] == "Undated") == (not info["dated"]), f"{name}: [Undated] iff sort key undated")
                    if it["time"] not in ("Undated", "No usable time"):
                        want_t, want_d = utc_text(info["utc"])
                        ok(it["time"] == want_t, f"{name}: item time is date_utc_ns")
                        ok(it["day"] == want_d, f"{name}: day line is the item's date")
                    elif it["time"] == "No usable time":
                        ok(info["dated"] and info["utc"] is None, f"{name}: no usable time")
                    if info["fromMe"]:
                        ok("Sent from the backed-up phone's account" in it["tags"], f"{name}: from-me tag")
                    elif it["sender"] is not None and info["handle"] is not None:
                        a = address.get(info["handle"])
                        ok(a is not None and unmark(it["sender"]) == a.decode("utf-8", "replace"),
                           f"{name}: sender is the handle's address")
                    block = next((c for l, c in it["labels"] if l == "Text:"), None)
                    shown = text.get(m)
                    if block is None or shown is None:
                        continue
                    joined = "\n".join(block)
                    if any(OPEN + p + CLOSE in joined for p in PLACEHOLDERS) or \
                            re.search(OPEN + r"(Attachment \d+|\+\d+ combining marks)" + CLOSE, joined) or \
                            any(l.startswith("Transcription recorded on the phone") for l, _ in it["labels"]):
                        continue
                    want = normalise(shown.decode("utf-8", "replace"))
                    ok(unmark(joined) == want, f"{name}: item text equals display_text")
                    texts_compared += 1
            ok(seen_files == len(chats) + (1 if len(in_chat) < len(ids) else 0),
               f"{name}: one file per conversation with rows, and one for the rest")
        finally:
            con.close()
    # Masking: no sentinel anywhere, raw or unmarked; and the positive
    # control, every sentinel in the bundle exported with both reveals.
    if entry.get("export"):
        doc = load(FIXTURES / entry["catalogue"] / entry["export"])
        blob = b"".join(p.read_bytes() for p in sorted(bundle.rglob("*")) if p.is_file())
        unmarked = unmark(blob.decode("utf-8", "replace")).encode("utf-8")
        for k, v in doc["sentinels"].items():
            for s in v:
                bs = s.encode("utf-8")
                ok(bs not in blob and bs not in unmarked, f"{name}: sentinel {k} not exported")
                seen["masked"].add((k, s))
        ok(entry.get("revealed") is not None, f"{name}: the revealed bundle (positive control)")
        if entry.get("revealed"):
            rb = root / entry["revealed"]
            rblob = b"".join(p.read_bytes() for p in sorted(rb.rglob("*")) if p.is_file())
            runmarked = unmark(rblob.decode("utf-8", "replace")).encode("utf-8")
            for k, v in doc["sentinels"].items():
                for s in v:
                    bs = s.encode("utf-8")
                    found = bs in rblob or bs in runmarked
                    ok(found, f"{name}: sentinel {k} not found with both reveals")
                    if found:
                        seen["revealed"].add((k, s))
        seen["entries"] += 1
    return texts_compared


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--exports", required=True, help="the folder write_oracle_txt wrote")
    a = ap.parse_args()
    root = pathlib.Path(a.exports)
    listing = load(root / "txt-oracle.json")
    fails = Failures()
    fails.check(listing["format"] == "oma-oracle-txt", "txt-oracle.json format")
    compared = 0
    seen = {"masked": set(), "revealed": set(), "entries": 0}
    for entry in listing["exports"]:
        compared += check_export(root, entry, fails, seen)
    fails.check(len(listing["exports"]) >= 20, "every catalogue entry exported")
    # Floors: a listing without its `export` statements cannot pass with no
    # masking check (the export catalogue's entries and their sentinels).
    fails.check(seen["entries"] >= MIN_EXPORT_ENTRIES, f"entries with masking sentinels: {seen['entries']}")
    fails.check(len(seen["masked"]) >= MIN_SENTINELS, f"sentinels scanned for: {len(seen['masked'])}")
    fails.check(len(seen["revealed"]) >= MIN_SENTINELS,
                f"sentinels found with both reveals: {len(seen['revealed'])}")
    fails.check(compared >= 100, "item texts compared")
    print(f"txt: {len(listing['exports'])} exports, {compared} item texts compared; "
          f"{fails.checks} checks, {len(fails.items)} failed")
    for f in fails.items[:50]:
        print(f"FAIL {f}")
    return 1 if fails.items else 0


if __name__ == "__main__":
    sys.exit(main())
