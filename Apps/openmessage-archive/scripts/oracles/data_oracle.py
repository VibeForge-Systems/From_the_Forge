#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""The JSON, CSV and zone oracles (M6 spec 14.4, slice 6f): the expectation
chain's links archive -> JSON (through the projection) and JSON -> CSV,
the JSON schemas, and the zone conversion, checked independently of the
Rust export code.

Usage (from Apps/openmessage-archive, in the oracle virtual environment):

  OMA_ORACLE_DATA_OUT=DIR cargo test -p oma-export --test json -- \\
      --ignored --exact write_oracle_data
  python -I scripts/oracles/data_oracle.py --exports DIR

write_oracle_data imports every catalogue entry into its own library and
exports the whole archive three ways: `ids` (JSON + CSV "for
spreadsheets", history, files copied, the identifier opt-in), `ordinals`
(JSON + CSV "for data tools", history, names only, export-local ids only)
and `zone-<n>` (JSON in each fixed test zone). This script:

- schemas (jsonschema): every JSON Lines line, conversations.json and
  handles.json validate against the strict and the open schema; every
  manifest against the manifest schema; each bundle's schema copies equal
  the repository's;
- JSON against the projection (project_archive.py, stdlib sqlite3 on a
  read-only copy), in `ids`: message ordinals are ranks of all message ids,
  conversation ordinals ranks of the exported conversation ids; each file
  holds exactly its conversation's chat_message rows (items in timeline
  order, folded reaction rows beside them) and the no-conversation file
  the messages in no conversation; per message: the raw date, its unit and
  state, the UTC instant, the direction, the sender reason and the
  resolved sender's address, the item kind, the display text (rebuilt
  from the segments: placeholders as U+FFFC; skipped where a code is
  masked), the reason there is none, the edited and unsent parts, the
  stored version count and version lines, the attachments in order with
  each copied file re-hashed against its blob's SHA-256; and the GUIDs;
- `ordinals` is `ids` without the identifiers: the same items, in the same
  order, with the same ordinals;
- CSV against JSON, in both: messages.csv has one row per JSON item, in
  file order, and its columns equal the JSON values under T (our `'`
  removed where neutralised_columns names the column; every
  U+27E8-marker escape undone); a leading `'` appears exactly where
  neutralised_columns says; in the spreadsheet profile every cell that
  this script's own trigger rule (White_Space and default-ignorables
  skipped, the OWASP characters and their full-width and small forms)
  flags is neutralised unless its column's grammar exempts it, and no
  content control is raw; the data-tools profile has no BOM, no prefix
  and raw controls; every CSV file's rows parse with the stdlib reader;
- zones (zoneinfo): every exported `local` time equals the conversion of
  its `utc` instant in the zone, with the same offset;
- masking: no sentinel of the entry's export.json appears in any byte of
  any bundle (marker escapes undone);
- every bundle's SHA256SUMS verifies.

Output is counts and check names only (never content). Run with
`python3 -I`; the standard library plus jsonschema.
"""
import argparse
import csv
import datetime
import hashlib
import io
import json
import pathlib
import re
import sys
import tempfile
import unicodedata
import zoneinfo

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.dont_write_bytecode = True

import project_archive  # noqa: E402

APP = HERE.parent.parent
FIXTURES = APP / "tests" / "fixtures"
SCHEMAS = APP / "crates" / "oma-export" / "schema"
COLUMNS_PIN = APP / "crates" / "oma-export" / "tests" / "csv_columns.pin"
MARKER = re.compile("⟨U\\+([0-9A-F]{4,6})⟩")
FFFC = "￼"
LONG_TEXT = 4096
UNITS = {0: "zero", 1: "negative", 2: "seconds", 3: "nanoseconds", 4: "ambiguous"}
STATES = {1: "null", 2: "absent", 3: "oversized", 4: "unreadable"}


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


def validators():
    import jsonschema
    out = {}
    for key, name in (("strict", "export-v1.strict.schema.json"), ("open", "export-v1.schema.json"),
                      ("manifest", "manifest-v1.schema.json")):
        s = load(SCHEMAS / name)
        cls = jsonschema.validators.validator_for(s)
        cls.check_schema(s)
        out[key] = cls(s)
    return out


def valid(v, doc, what, fails):
    errors = list(v.iter_errors(doc))
    for e in errors[:3]:
        fails.check(False, f"{what}: schema: {'/'.join(map(str, e.absolute_path))}: {e.validator}")
    fails.check(not errors, f"{what}: schema")


def unmark(s):
    return MARKER.sub(lambda m: chr(int(m.group(1), 16)), s)


# ---------------------------------------------------------------------------
# The trigger rule, written here from the spec (5.2), not from the Rust code
# ---------------------------------------------------------------------------

# Default_Ignorable_Code_Point (UAX #44): Cf and variation selectors and the
# other default ignorables, less White_Space, the prepended concatenation
# marks and the interlinear annotation and Egyptian format controls.
_NOT_DI = set(range(0x0600, 0x0606)) | {0x06DD, 0x070F, 0x0890, 0x0891, 0x08E2, 0x110BD, 0x110CD} \
    | set(range(0xFFF9, 0xFFFC)) | set(range(0x13430, 0x13441))
_EXTRA_DI = {0x034F, 0x115F, 0x1160, 0x17B4, 0x17B5, 0x3164, 0xFFA0} | set(range(0x180B, 0x1810)) \
    | set(range(0xFE00, 0xFE10)) | set(range(0xFFF0, 0xFFF9)) | set(range(0xE0000, 0xE1000)) \
    | set(range(0x1D173, 0x1D17B)) | set(range(0x1BCA0, 0x1BCA4)) | set(range(0x2060, 0x2070))
WHITE_SPACE = {0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x20, 0x85, 0xA0, 0x1680, 0x2028, 0x2029, 0x202F,
               0x205F, 0x3000} | set(range(0x2000, 0x200B))
FORMULA = set("=+-@") | {"＝", "＋", "－", "＠", "﹢", "﹣", "﹦", "﹫"}


def skipped(c):
    cp = ord(c)
    if cp in WHITE_SPACE:
        return True
    if cp in _NOT_DI:
        return False
    return unicodedata.category(c) == "Cf" or cp in _EXTRA_DI


def triggers(cell):
    if not cell:
        return False
    if cell[0] in "\t\r\n":
        return True
    for c in cell:
        if not skipped(c):
            return c in FORMULA
    return False


INT = re.compile(r"-?[0-9]+")
GRAMMARS = {
    "int": INT,
    "intList": re.compile(r"-?[0-9]+(;-?[0-9]+)*"),
    "real": re.compile(r"-?[0-9]+(\.[0-9]+)?(e-?[0-9]+)?|-?inf|NaN"),
    "iso": re.compile(r"(-|\+)?[0-9]{4,}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]+)?"
                      r"(Z|[+-][0-9]{2}:[0-9]{2}(:[0-9]{2})?)"),
    "offset": re.compile(r"[+-][0-9]{2}:[0-9]{2}(:[0-9]{2})?"),
    "token": re.compile(r"[A-Za-z][A-Za-z0-9]*(:-?[A-Za-z0-9]+)*(;[A-Za-z][A-Za-z0-9]*(:-?[A-Za-z0-9]+)*)*"),
}


def exempt(kind, v):
    g = GRAMMARS.get(kind)
    return g is not None and g.fullmatch(v) is not None


def column_kinds():
    kinds = {}
    for line in COLUMNS_PIN.read_text(encoding="utf-8").splitlines():
        f, col, kind = line.split("\t")
        kinds.setdefault(f, {})[col] = kind
    return kinds


def is_control(c):
    cp = ord(c)
    return c not in "\t\r\n" and (cp < 0x20 or 0x7F <= cp <= 0x9F)


# ---------------------------------------------------------------------------
# Reading a bundle
# ---------------------------------------------------------------------------

def jsonl_files(bundle):
    d = bundle / "json"
    names = sorted((p.name for p in d.glob("*.jsonl")), key=lambda n: (n == "no-conversation.jsonl", n))
    out = []
    for n in names:
        raw = (d / n).read_bytes()
        text = raw.decode("utf-8")
        out.append((n, [json.loads(line) for line in text.split("\n") if line], text))
    return out


def read_csv(path, bom):
    # The catalogue holds a 100,000-character token and a 33,790-byte body.
    csv.field_size_limit(1 << 30)
    raw = path.read_bytes()
    has = raw.startswith(b"\xef\xbb\xbf")
    text = raw[3:].decode("utf-8") if has else raw.decode("utf-8")
    rows = list(csv.reader(io.StringIO(text, newline="")))
    return has == bom, rows[0], rows[1:]


def crlf_records(raw):
    """Every record ends in CRLF and every field is quoted: outside quotes
    only `,` and CRLF appear."""
    if raw.startswith(b"\xef\xbb\xbf"):
        raw = raw[3:]
    if not raw.endswith(b"\r\n"):
        return False
    i, quoted, start = 0, False, True
    while i < len(raw):
        c = raw[i:i + 1]
        if quoted:
            if c == b'"':
                if raw[i + 1:i + 2] == b'"':
                    i += 1
                else:
                    quoted = False
        elif c == b'"' and start:
            quoted = True
        elif c == b",":
            start = True
            i += 1
            continue
        elif raw[i:i + 2] == b"\r\n":
            start = True
            i += 2
            continue
        else:
            return False
        start = False
        i += 1
    return not quoted


def verify_sums(bundle, what, fails):
    for line in (bundle / "SHA256SUMS").read_text(encoding="utf-8").splitlines():
        h, rel = line.split("  ", 1)
        got = hashlib.sha256((bundle / rel).read_bytes()).hexdigest()
        fails.check(got == h, f"{what}: SHA256SUMS {rel}")


def iso(ns, nanos):
    s, f = divmod(ns, 1_000_000_000)
    t = datetime.datetime(1970, 1, 1) + datetime.timedelta(seconds=s)
    out = t.strftime("%Y-%m-%dT%H:%M:%S")
    if t.year < 1000:
        out = f"{t.year:04d}" + out[out.index("-"):]
    return out + (f".{f:09d}Z" if nanos else "Z")


def offset_text(sec):
    sign = "-" if sec < 0 else "+"
    a = abs(sec)
    h, m, s = a // 3600, a // 60 % 60, a % 60
    return f"{sign}{h:02d}:{m:02d}" + (f":{s:02d}" if s else "")


def segments_text(body):
    """The display text the segments stand for (placeholders as U+FFFC), or
    None when a masked code hides part of it."""
    if body["kind"] != "text":
        return None
    out = []
    for s in body["segments"]:
        k = s["kind"]["token"]
        if k == "text":
            out.append(s["text"] or "")
        elif k == "withheld":
            return None
        else:
            out.append(FFFC)
    return "".join(out)


def projected_text(display):
    if "noneReason" in display:
        return None
    if "text" in display:
        return ("text", display["text"])
    return ("sha256", display["sha256"])


def as_projected(text):
    raw = text.encode("utf-8")
    if len(raw) > LONG_TEXT:
        return ("sha256", hashlib.sha256(raw).hexdigest())
    return ("text", text)


# ---------------------------------------------------------------------------
# JSON against the projection
# ---------------------------------------------------------------------------

def json_against_projection(name, bundle, archive, fails, v, counts):
    p = project_archive.project(archive)
    with tempfile.TemporaryDirectory(prefix="oma-data-oracle-") as tmp:
        con = project_archive.open_copy(archive, tmp)
        try:
            ids = [r[0] for r in project_archive.rows(con, "SELECT id FROM message ORDER BY id")]
            with_rows = [r[0] for r in project_archive.rows(
                con, "SELECT DISTINCT chat_id FROM chat_message ORDER BY chat_id")]
        finally:
            con.close()
    rank = {mid: i + 1 for i, mid in enumerate(ids)}
    convs = load(bundle / "json" / "conversations.json")
    handles = load(bundle / "json" / "handles.json")
    for doc, what in ((convs, "conversations.json"), (handles, "handles.json")):
        valid(v["strict"], doc, f"{name}: {what} strict", fails)
        valid(v["open"], doc, f"{name}: {what} open", fails)
    by_conv = {}
    for c in convs["conversations"]:
        aid = int(c["source"]["value"]["archiveId"])
        fails.check(c["n"] == with_rows.index(aid) + 1 if aid in with_rows else False,
                    f"{name}: conversation {c['n']} ordinal")
        g = c["source"]["value"]["guid"]["text"]
        by_conv[c["n"]] = f"guid:{g}" if g is not None else None
    addr = {h["n"]: h for h in handles["handles"]}
    proj_convs = {c["ref"]: c for c in p["conversations"]}
    seen_versions = {}
    for fname, lines, _ in jsonl_files(bundle):
        head, body = lines[0], lines[1:]
        for line in lines:
            valid(v["strict"], line, f"{name}: {fname} strict", fails)
            valid(v["open"], line, f"{name}: {fname} open", fails)
            counts["lines"] += 1
        conv = head["conversation"]
        items = [x for x in body if x["type"] == "item"]
        refs = []
        for x in items:
            src = x["source"]["value"]
            aid = int(src["archiveId"])
            fails.check(x["n"] == rank.get(aid), f"{name}: item {x['n']} is the rank of its id")
            g = src["guid"]["text"]
            # An unkeyed message is named by its fingerprint in the
            # projection, which the export does not carry.
            unkeyed = any(lab["kind"]["token"] == "unkeyed" for lab in x["labels"])
            refs.append((f"guid:{g}" if g is not None and not unkeyed else None, x))
        if conv is None:
            want = set(p["noConversation"])
            got = {r for r, _ in refs if r is not None}
            fails.check(got <= want, f"{name}: no-conversation file holds other messages")
            fails.check(len(refs) == len(want), f"{name}: no-conversation count")
        else:
            pc = proj_convs.get(by_conv.get(conv))
            fails.check(pc is not None, f"{name}: conversation {conv} projected")
            if pc is not None:
                prows = [i["message"] for i in pc["items"]]
                fails.check(len(prows) == len(refs), f"{name}: {fname}: rows equal chat_message rows")
                folded = {r for r, x in refs if x["context"]["foldedInto"] is not None}
                order = [r for r, x in refs if x["context"]["foldedInto"] is None and r is not None]
                want = [r for r in prows if r not in folded and not r.startswith("unkeyed")]
                fails.check(order == want, f"{name}: {fname}: timeline order")
                fails.check(sorted(r for r, _ in refs if r) ==
                            sorted(r for r in prows if not r.startswith("unkeyed")),
                            f"{name}: {fname}: membership")
        for ref, x in refs:
            # Version lines are checked against the record's own count; the
            # count against the projection's below.
            seen_versions.setdefault(x["n"], x["edit"]["storedVersions"])
            if ref is None or ref not in p["messages"]:
                counts["unkeyed"] += 1
                continue
            m = p["messages"][ref]
            what = f"{name}: item {x['n']}"
            d = x["date"]
            fails.check(d["raw"] == m["date"]["raw"], f"{what}: date raw")
            st = m["date"]["state"]
            fails.check((d["state"] is None) == (st == "value"), f"{what}: date state")
            if d["state"] is not None:
                tok = d["state"]["token"]
                fails.check(tok == st or (st.startswith("unknown:") and tok == "unrecognisedCode"
                                          and str(d["state"]["code"]) == st[8:]), f"{what}: date state token")
            unit = m["date"]["unit"]
            if unit is None:
                fails.check(d["unit"] is None, f"{what}: no unit")
            elif unit.startswith("unknown:"):
                fails.check(d["unit"]["token"] == "unrecognisedCode" and str(d["unit"]["code"]) == unit[8:],
                            f"{what}: unknown unit kept")
            else:
                fails.check(d["unit"]["token"] == unit, f"{what}: unit")
            utc = m["date"]["utcNs"]
            if utc is None or unit not in ("seconds", "nanoseconds"):
                fails.check(d["utc"] is None, f"{what}: no instant")
            else:
                fails.check(d["utc"] == iso(int(utc), unit == "nanoseconds"), f"{what}: utc")
            dr = m["direction"]
            tok = x["direction"]["token"]["token"]
            if dr["state"] != "value":
                fails.check(tok == "notRecorded", f"{what}: direction not recorded")
            elif dr["raw"] == 1:
                fails.check(tok == "fromMe", f"{what}: from me")
            elif dr["raw"] == 0:
                fails.check(tok == "received", f"{what}: received")
            else:
                fails.check(tok == "unknownValue" and x["direction"]["raw"] == str(dr["raw"]),
                            f"{what}: unknown direction kept")
            fails.check(x["sender"]["reason"]["token"] == m["sender"]["reason"], f"{what}: sender reason")
            if m["sender"]["reason"] == "resolved":
                fails.check(handle_ref(addr.get(x["sender"]["handle"])) == m["sender"]["handle"],
                            f"{what}: sender address")
            fails.check(x["item"]["code"] == m["itemKind"], f"{what}: item kind")
            pt = projected_text(m["display"])
            st = segments_text(x["body"])
            if pt is None:
                fails.check(x["body"]["kind"] == "noText", f"{what}: no text")
            elif st is not None:
                fails.check(as_projected(st) == pt, f"{what}: display text")
                counts["texts"] += 1
            else:
                counts["masked"] += 1
            fails.check([int(z) for z in x["edit"]["editedParts"]] == m["editedParts"], f"{what}: edited parts")
            fails.check([int(z) for z in x["edit"]["unsentParts"]] == m["unsentParts"], f"{what}: unsent parts")
            fails.check(x["edit"]["storedVersions"] == m["bodyVersions"], f"{what}: stored versions")
            arefs = []
            for a in x["attachments"]:
                g = a["source"]["value"]["guid"]["text"]
                arefs.append(f"guid:{g}" if g is not None else "unkeyed")
                pa = p["attachments"].get(arefs[-1])
                f = a["file"]
                if f["path"] is not None and pa is not None:
                    data = (bundle / pathlib.PurePosixPath(f["path"])).read_bytes()
                    fails.check(hashlib.sha256(data).hexdigest() == pa["sha256"], f"{what}: copied file digest")
                    counts["files"] += 1
            fails.check([r for r in arefs if r != "unkeyed"] ==
                        [r for r in m["attachments"] if r != "unkeyed"], f"{what}: attachments")
            fails.check(x["source"]["value"]["guid"]["text"] == ref[5:], f"{what}: guid")
            counts["items"] += 1
        # Versions follow their item, once per message.
        last = None
        for x in body:
            if x["type"] == "item":
                last = x["n"]
            else:
                fails.check(x["message"] == last, f"{name}: version follows its item")
                counts["versions"] += 1
                seen_versions[x["message"]] = seen_versions.get(x["message"], 0) - 1
    for n, left in seen_versions.items():
        fails.check(left == 0, f"{name}: item {n}: version lines equal stored versions")
    m = load(bundle / "manifest.json")
    valid(v["manifest"], m, f"{name}: manifest", fails)
    for f in ("export-v1.schema.json", "manifest-v1.schema.json"):
        fails.check((bundle / "schema" / f).read_bytes() == (SCHEMAS / f).read_bytes(), f"{name}: {f} copy")


def lossy_equal(text, ref_part):
    """A projection handle part (`hex:<bytes>` for bytes that are not
    UTF-8) against an exported text, which holds U+FFFD for them."""
    if ref_part.startswith("hex:"):
        return bytes.fromhex(ref_part[4:]).decode("utf-8", "replace") == text
    return ref_part == text


def handle_ref(h):
    """The projection's `handle:<address>|<service>` test for handle `h`,
    as a comparable value."""
    if h is None:
        return None
    return HandleRef(h)


class HandleRef:
    def __init__(self, h):
        self.h = h

    def __eq__(self, ref):
        if not isinstance(ref, str) or not ref.startswith("handle:"):
            return False
        a, _, s = ref[len("handle:"):].rpartition("|")
        addr, svc = self.h["address"], self.h["service"]
        svc_ok = (lossy_equal(svc["text"], s) if svc["text"] is not None
                  else s == f"({svc['missing']['token']})")
        return addr["text"] is not None and lossy_equal(addr["text"], a) and svc_ok


def same_items(name, ids_bundle, ord_bundle, fails):
    a = [[x for x in lines[1:]] for _, lines, _ in jsonl_files(ids_bundle)]
    b = [[x for x in lines[1:]] for _, lines, _ in jsonl_files(ord_bundle)]
    fails.check(len(a) == len(b), f"{name}: same files with and without identifiers")
    for xa, xb in zip(a, b):
        fails.check([(x["type"], x.get("n", x.get("message"))) for x in xa]
                    == [(x["type"], x.get("n", x.get("message"))) for x in xb], f"{name}: same ordinals")
        for x in xb:
            if x["type"] == "item":
                fails.check(x["source"]["state"] == "notIncluded", f"{name}: no identifiers by default")


# ---------------------------------------------------------------------------
# CSV against JSON
# ---------------------------------------------------------------------------

def rendered_text(body):
    """messages.csv's `text` under T: segments with the export's
    placeholders."""
    if body["kind"] != "text":
        return ""
    out = []
    for s in body["segments"]:
        k = s["kind"]["token"]
        if k == "text":
            out.append(s["text"] or "")
        elif k == "withheld":
            out.append("⟨one-time code hidden⟩")
        elif k == "attachment" and s["attachment"] is not None:
            out.append(f"⟨Attachment {s['attachment']}⟩")
        else:
            out.append(None)
    return out


def text_matches(cell, parts):
    if isinstance(parts, str):
        return cell == parts
    if None not in parts:
        return cell == "".join(parts)
    # Other placeholders (inline emoji, unmatched): compare the recorded
    # fragments in order around any marker.
    pattern = "".join(re.escape(p) if p is not None else "⟨[^⟩]*⟩" for p in parts)
    return re.fullmatch(pattern, cell, flags=re.S) is not None


def tok(c):
    if c is None:
        return ""
    if c["token"] == "unrecognisedCode":
        return f"unrecognisedCode:{c['code']}"
    return c["token"]


def b(v):
    return "" if v is None else ("true" if v else "false")


def csv_against_json(name, bundle, spreadsheet, fails, kinds, counts):
    files = sorted(p.name for p in (bundle / "csv").glob("*.csv"))
    fails.check(len(files) == 9, f"{name}: nine CSV files")
    tables = {}
    for f in files:
        bom_ok, header, rows = read_csv(bundle / "csv" / f, spreadsheet)
        fails.check(bom_ok, f"{name}: {f}: BOM only for spreadsheets")
        fails.check(crlf_records((bundle / "csv" / f).read_bytes()), f"{name}: {f}: CRLF, every field quoted")
        want = list(kinds[f"csv/{f}"])
        fails.check(header == want, f"{name}: {f}: header is the column contract")
        tables[f] = (header, rows)
        at = header.index("neutralised_columns")
        for r in rows:
            fails.check(len(r) == len(header), f"{name}: {f}: row width")
            named = [x for x in r[at].split(";") if x]
            if not spreadsheet:
                fails.check(not named, f"{name}: {f}: nothing neutralised for data tools")
            for col, cell in zip(header, r):
                if col == "neutralised_columns":
                    continue
                kind = kinds[f"csv/{f}"][col]
                if col in named:
                    fails.check(cell.startswith("'") and triggers(cell[1:]) and not exempt(kind, cell[1:]),
                                f"{name}: {f}: {col} prefixed without a trigger")
                    counts["neutralised"] += 1
                elif spreadsheet:
                    fails.check(not triggers(cell) or exempt(kind, cell),
                                f"{name}: {f}: {col} would run as a formula")
                    fails.check(not any(is_control(c) for c in cell), f"{name}: {f}: {col}: raw control")
                counts["cells"] += 1
    header, rows = tables["messages.csv"]
    col = {h: i for i, h in enumerate(header)}
    items = [x for _, lines, _ in jsonl_files(bundle) for x in lines[1:] if x["type"] == "item"]
    handles = {h["n"]: h for h in load(bundle / "json" / "handles.json")["handles"]}
    fails.check(len(rows) == len(items), f"{name}: one messages.csv row per JSON item")
    at = col["neutralised_columns"]
    for r, x in zip(rows, items):
        named = set(z for z in r[at].split(";") if z)

        def T(c):
            v = r[col[c]]
            if c in named:
                v = v[1:]
            return unmark(v)
        what = f"{name}: item {x['n']}"
        cx = x["context"]
        d = x["date"]
        expect = {
            "conversation": "" if cx["conversation"] is None else str(cx["conversation"]),
            "item": str(x["n"]),
            "also_in": ";".join(str(z) for z in cx["alsoIn"]["inExport"]),
            "also_in_elsewhere": str(cx["alsoIn"]["elsewhere"]),
            "item_kind": tok(x["item"]), "item_kind_code": str(x["item"]["code"]),
            "date_local": d["local"] or "", "date_utc": d["utc"] or "", "date_raw": d["raw"] or "",
            "date_unit": tok(d["unit"]), "date_state": tok(d["state"]),
            "before_2007": b(d["before2007"]), "planned_time": b(d["planned"]),
            "order_tie": b(cx["orderTie"]),
            "direction": tok(x["direction"]["token"]), "direction_raw": x["direction"]["raw"] or "",
            "sender_reason": tok(x["sender"]["reason"]),
            "sender_handle": "" if x["sender"]["handle"] is None else str(x["sender"]["handle"]),
            "service": tok(x["service"]),
            "subject": x["subject"]["text"] or "", "subject_state": tok(x["subject"]["missing"]),
            "account": x["account"]["text"] or "", "account_state": tok(x["account"]["missing"]),
            "text_state": x["body"]["kind"],
            "edited_parts": ";".join(x["edit"]["editedParts"]),
            "unsent_parts": ";".join(x["edit"]["unsentParts"]),
            "folded_into": "" if cx["foldedInto"] is None else str(cx["foldedInto"]),
            "attachment_count": str(len(x["attachments"])),
            "derived_by": str(x["derivation"]["derivedBy"]),
            "source_archive_id": x["source"]["value"]["archiveId"] if x["source"]["state"] == "included" else "",
            "source_guid": (x["source"]["value"]["guid"]["text"] or "") if x["source"]["state"] == "included" else "",
            "history_state": x["history"]["state"],
            "reactions_state": x["reactions"]["state"],
        }
        if d["local"] is not None and d["offsetSeconds"] is not None:
            expect["utc_offset"] = offset_text(d["offsetSeconds"])
        if x["sender"]["handle"] is not None:
            expect["sender_address"] = handles[x["sender"]["handle"]]["address"]["text"] or ""
        for c, want in expect.items():
            fails.check(T(c) == want, f"{what}: messages.csv {c}")
        fails.check(text_matches(T("text"), rendered_text(x["body"])), f"{what}: messages.csv text")
        masked = sum(1 for s in x["body"].get("segments", []) if s["kind"]["token"] == "withheld")
        fails.check(T("masked_pieces") == str(masked), f"{what}: masked_pieces")
        counts["rows"] += 1
    # segments.csv: one row per segment of each message's first placement.
    header, rows = tables["segments.csv"]
    first = {}
    for x in items:
        first.setdefault(x["n"], x)
    want = sum(len(x["body"].get("segments", [])) for x in first.values())
    fails.check(len(rows) == want, f"{name}: segments.csv rows")
    header, rows = tables["attachments.csv"]
    want = sum(len(x["attachments"]) for x in first.values())
    fails.check(len(rows) == want, f"{name}: attachments.csv rows")


# ---------------------------------------------------------------------------
# Zones
# ---------------------------------------------------------------------------

def times(v, out):
    if isinstance(v, dict):
        if "unitRule" in v and "local" in v:
            out.append(v)
        for x in v.values():
            times(x, out)
    elif isinstance(v, list):
        for x in v:
            times(x, out)


def zone_check(name, bundle, zone, fails, counts):
    z = zoneinfo.ZoneInfo(zone)
    for fname, lines, _ in jsonl_files(bundle):
        found = []
        times(lines, found)
        for t in found:
            if t["utc"] is None:
                fails.check(t["local"] is None, f"{name}: {zone}: local without utc")
                continue
            if t["outsideZoneWindow"]:
                fails.check(t["local"] is None, f"{name}: {zone}: outside window but local")
                continue
            u = datetime.datetime.strptime(t["utc"][:19], "%Y-%m-%dT%H:%M:%S").replace(tzinfo=datetime.timezone.utc)
            loc = u.astimezone(z)
            off = int(loc.utcoffset().total_seconds())
            want = loc.strftime("%Y-%m-%dT%H:%M:%S") + offset_text(off)
            fails.check(t["local"] == want and t["offsetSeconds"] == off, f"{name}: {zone}: local time")
            counts["zoned"] += 1


def masking(name, bundle, export, fails):
    if export is None:
        return
    blob = b"".join(p.read_bytes() for p in sorted(bundle.rglob("*")) if p.is_file())
    text = unmark(blob.decode("utf-8", "replace"))
    for k in ("oneTimeCodes", "earlierText", "editText", "subjectHistory", "alternativeText",
              "unsentAttachmentFiles"):
        for s in export["sentinels"][k]:
            fails.check(s.encode("utf-8") not in blob and s not in text, f"{name}: a {k} sentinel leaked")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--exports", required=True, help="the folder write_oracle_data wrote")
    a = ap.parse_args()
    import jsonschema
    from importlib.metadata import version
    print(f"python {sys.version.split()[0]}, sqlite {project_archive.sqlite3.sqlite_version}, "
          f"jsonschema {version('jsonschema')}, unicodedata {unicodedata.unidata_version}")
    del jsonschema
    fails = Failures()
    v = validators()
    kinds = column_kinds()
    root = pathlib.Path(a.exports)
    listing = load(root / "data-oracle.json")
    fails.check(listing["format"] == "oma-oracle-data", "data-oracle.json format")
    counts = {k: 0 for k in ("lines", "items", "texts", "masked", "unkeyed", "files", "versions", "rows",
                             "cells", "neutralised", "zoned")}
    for e in listing["exports"]:
        name = e["name"]
        export = load(FIXTURES / e["catalogue"] / e["export"]) if e["export"] else None
        bundles = {x["kind"]: (root / x["bundle"], x["zone"]) for x in e["bundles"]}
        for kind, (bundle, zone) in bundles.items():
            verify_sums(bundle, f"{name} {kind}", fails)
            masking(f"{name} {kind}", bundle, export, fails)
        ids, _ = bundles["ids"]
        ords, _ = bundles["ordinals"]
        json_against_projection(name, ids, root / e["archive"], fails, v, counts)
        same_items(name, ids, ords, fails)
        csv_against_json(f"{name} ids", ids, True, fails, kinds, counts)
        csv_against_json(f"{name} ordinals", ords, False, fails, kinds, counts)
        for kind, (bundle, zone) in bundles.items():
            if kind.startswith("zone-"):
                zone_check(name, bundle, zone, fails, counts)
                for _, lines, _ in jsonl_files(bundle):
                    for line in lines:
                        valid(v["strict"], line, f"{name}: {kind} strict", fails)
    print(f"{len(listing['exports'])} exports; " + ", ".join(f"{k} {n}" for k, n in counts.items())
          + f"; {fails.checks} checks, {len(fails.items)} failed")
    for f in fails.items[:50]:
        print(f"FAIL {f}")
    return 1 if fails.items else 0


if __name__ == "__main__":
    sys.exit(main())
