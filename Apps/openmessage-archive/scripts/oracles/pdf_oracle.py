#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""The PDF export oracle (M6 spec 14.4, "JSON <-> PDF"; slice 6j2): every
catalogue entry exported to JSON + PDF through the pipeline is read back
with two independent tools, pypdf and poppler's `pdftotext` (run as an
external program; nothing of it is copied or shipped), and checked
against the bundle's own JSON records.

Usage (from Apps/openmessage-archive, in the oracle virtual environment,
with poppler-utils installed):

  OMA_ORACLE_PDF_OUT=DIR cargo test -p oma-export --test pdf_writer -- \\
      --ignored --exact write_oracle_pdf
  python -I scripts/oracles/pdf_oracle.py --exports DIR

`pdf-oracle.json` lists, per entry, a `files` bundle (copies, previews and
the hidden-character markers on), a `names` bundle (names only, markers
off) and, for the export catalogue's entries, a `revealed` bundle (both
reveals: the positive control). Per bundle:

- every `SHA256SUMS` line verifies;
- each conversation file of the JSON (`json/<stem>.jsonl`) has its PDF
  volumes `pdf/<stem>.pdf`, `pdf/<stem>-2.pdf` ..., and no other PDF
  exists;
- each volume opens in pypdf, has a page, a non-empty outline, the
  generic document title (D112: "Messages export — conversation N" or
  "... messages in no conversation", with ", part K" after the first
  volume), a structure tree with `/MarkInfo /Marked true` (tagged; no
  PDF/UA claim), every font embedded, and no forbidden name (JavaScript,
  actions, launch, URI, embedded files, forms, rich media) as a key or a
  name value in any object;
- completeness and order: the text of a file's volumes (pdftotext, after
  removing white space and the bidi embedding marks pdftotext adds)
  holds `item N` for every item of the JSON file that is not a folded
  reaction row, in the JSON's order; for items whose recorded text is
  printable ASCII, that text too, in pdftotext and in pypdf's extraction;
- masking: no sentinel of the entry's export.json is in any volume's text
  (both tools; U+FFFC, an attachment's place, is drawn as
  `⟨Attachment N⟩`) or in the bytes of any other file of the bundle;
- the positive control: with both reveals every text sentinel's parts
  (split at U+FFFC) are in the text of the volumes;
- the manifest's `pdfUndrawableCharacters` and `pdfForcedWraps` exist, and
  a bundle whose manifest counts previews has an image in its PDFs.

Floors: 40 bundles, 300 items, 14 sentinels scanned for, 10 found
revealed. Output is counts and check names only (never content). Run with
`python3 -I`; the standard library plus pypdf.
"""
import argparse
import hashlib
import json
import pathlib
import re
import subprocess
import sys

import pypdf
from pypdf.generic import ArrayObject, DictionaryObject, IndirectObject, NameObject

HERE = pathlib.Path(__file__).resolve().parent
sys.dont_write_bytecode = True
APP = HERE.parent.parent
FIXTURES = APP / "tests" / "fixtures"
SENTINEL_KINDS = ("oneTimeCodes", "earlierText", "editText", "subjectHistory", "alternativeText",
                  "unsentAttachmentFiles")
BIDI_EMBEDDING = {"\u202a", "\u202b", "\u202c"}
FORBIDDEN = {"/JavaScript", "/JS", "/OpenAction", "/AA", "/Launch", "/URI", "/SubmitForm",
             "/ImportData", "/GoToR", "/GoToE", "/EmbeddedFile", "/EmbeddedFiles", "/AcroForm",
             "/XFA", "/RichMedia"}
TITLE = "Messages export \u2014 "
MIN_BUNDLES = 40
MIN_ITEMS = 300
MIN_SENTINELS = 14
MIN_REVEALED = 10


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


def normalise(text):
    return "".join(c for c in text if not c.isspace() and c not in BIDI_EMBEDDING)


def pdftotext(path):
    out = subprocess.run(["pdftotext", "-enc", "UTF-8", str(path), "-"],
                         check=True, capture_output=True)
    return out.stdout.decode("utf-8")


def jsonl(path):
    return [json.loads(line) for line in path.read_text(encoding="utf-8").split("\n") if line]


def verify_sums(bundle, what, fails):
    for line in (bundle / "SHA256SUMS").read_text(encoding="utf-8").splitlines():
        h, rel = line.split("  ", 1)
        got = hashlib.sha256((bundle / rel).read_bytes()).hexdigest()
        fails.check(got == h, f"{what}: a SHA256SUMS line")


def embedded(reader):
    fonts = 0
    for page in reader.pages:
        res = page.get("/Resources") or {}
        for _, ref in (res.get("/Font") or {}).items():
            font = ref.get_object()
            fonts += 1
            if font.get("/Subtype") == "/Type3":
                continue
            desc = font.get("/FontDescriptor")
            if desc is None:
                for d in font.get("/DescendantFonts") or []:
                    desc = d.get_object().get("/FontDescriptor")
            desc = desc.get_object() if desc is not None else {}
            if not any(k in desc for k in ("/FontFile", "/FontFile2", "/FontFile3")):
                return False, fonts
    return True, fonts


def names_in(obj, seen, out):
    """Every name used as a key or a value in `obj`, following references."""
    if isinstance(obj, IndirectObject):
        key = (obj.idnum, obj.generation)
        if key in seen:
            return
        seen.add(key)
        obj = obj.get_object()
    if isinstance(obj, DictionaryObject):
        for k, v in obj.items():
            out.add(str(k))
            names_in(v, seen, out)
    elif isinstance(obj, ArrayObject):
        for v in obj:
            names_in(v, seen, out)
    elif isinstance(obj, NameObject):
        out.add(str(obj))


def images_in(reader):
    n = 0
    for page in reader.pages:
        res = page.get("/Resources") or {}
        for _, ref in (res.get("/XObject") or {}).items():
            if ref.get_object().get("/Subtype") == "/Image":
                n += 1
    return n


def volume_key(name):
    m = re.fullmatch(r"(.+?)(?:-([0-9]+))?\.pdf", name)
    return m.group(1), int(m.group(2) or 1)


def plain_ascii(item):
    body = item["body"]
    if body["kind"] != "text":
        return None
    parts = []
    for s in body["segments"]:
        if s["kind"]["token"] != "text" or s["text"] is None or s["transcription"]:
            return None
        parts.append(s["text"])
    text = "".join(parts)
    if not text or not all(0x20 <= ord(c) < 0x7F for c in text):
        return None
    return text


def sentinel_parts(s):
    return [p.strip() for p in s.split("\ufffc") if p.strip()]


def sentinel_pattern(s):
    """A sentinel as drawn: U+FFFC (an attachment's place) is drawn as
    the placeholder `\u27e8Attachment N\u27e9`."""
    return re.compile("\u27e8Attachment[0-9]+\u27e9".join(
        re.escape(normalise(p)) for p in s.split("\ufffc")))


def check_bundle(name, b, export, fails, counts):
    bundle = b["root"]
    what = f"{name}/{b['kind']}"
    verify_sums(bundle, what, fails)
    stems = sorted(p.name[:-6] for p in (bundle / "json").glob("*.jsonl"))
    volumes = {}
    for p in sorted((bundle / "pdf").glob("*.pdf")):
        stem, part = volume_key(p.name)
        volumes.setdefault(stem, []).append((part, p))
    fails.check(sorted(volumes) == stems, f"{what}: one PDF file per conversation file")
    all_text = []
    images = 0
    for stem in stems:
        vols = sorted(volumes.get(stem, []))
        fails.check([v[0] for v in vols] == list(range(1, len(vols) + 1)), f"{what}: {stem} volumes")
        n = stem[1:].lstrip("0") if stem.startswith("c") else None
        texts, pyp = [], []
        for part, path in vols:
            reader = pypdf.PdfReader(str(path))
            counts["volumes"] += 1
            fails.check(len(reader.pages) >= 1, f"{what}: {stem}-{part} has a page")
            title = reader.metadata.title if reader.metadata else None
            want = TITLE + (f"conversation {n}" if n else "messages in no conversation")
            if part > 1:
                want += f", part {part}"
            fails.check(title == want, f"{what}: {stem}-{part} document title")
            fails.check(bool(reader.outline), f"{what}: {stem}-{part} outline")
            root = reader.trailer["/Root"].get_object()
            fails.check("/StructTreeRoot" in root, f"{what}: {stem}-{part} structure tree")
            mark = root.get("/MarkInfo")
            fails.check(mark is not None and bool(mark.get_object().get("/Marked")),
                        f"{what}: {stem}-{part} marked")
            ok, fonts = embedded(reader)
            fails.check(ok and fonts > 0, f"{what}: {stem}-{part} fonts embedded")
            found = set()
            names_in(reader.trailer, set(), found)
            fails.check(not (found & FORBIDDEN), f"{what}: {stem}-{part} forbidden names")
            images += images_in(reader)
            texts.append(normalise(pdftotext(path)))
            pyp.append(normalise("".join(p.extract_text() for p in reader.pages)))
        text = "".join(texts)
        pytext = "".join(pyp)
        all_text.extend([text, pytext])
        items = [x for x in jsonl(bundle / "json" / f"{stem}.jsonl")
                 if x["type"] == "item" and x["context"]["foldedInto"] is None]
        at = 0
        for x in items:
            m = re.compile(rf"item{x['n']}(?![0-9])").search(text, at)
            if fails.check(m is not None, f"{what}: {stem} item {x['n']} in order"):
                at = m.end()
            body = plain_ascii(x)
            if body is not None:
                nb = normalise(body)
                fails.check(nb in text, f"{what}: {stem} item {x['n']} text (pdftotext)")
                fails.check(nb in pytext, f"{what}: {stem} item {x['n']} text (pypdf)")
                counts["texts"] += 1
            counts["items"] += 1
    m = load(bundle / "manifest.json")
    for k in ("pdfUndrawableCharacters", "pdfForcedWraps"):
        fails.check(k in m["counts"], f"{what}: manifest {k}")
    if m["counts"]["attachments"]["previewsEmbedded"] > 0:
        fails.check(images > 0, f"{what}: previews drawn")
        counts["images"] += images
    if export is not None and b["kind"] != "revealed":
        blob = b"\n".join(p.read_bytes() for p in bundle.rglob("*")
                          if p.is_file() and p.suffix != ".pdf")
        text = "\n".join(all_text)
        for k in SENTINEL_KINDS:
            for s in export["sentinels"][k]:
                leaked = s.encode("utf-8") in blob or sentinel_pattern(s).search(text) is not None
                fails.check(not leaked, f"{what}: a {k} sentinel leaked")
                counts["masked"].add((k, s))
    if b["kind"] == "revealed":
        text = "\n".join(all_text)
        for k in SENTINEL_KINDS:
            if k == "unsentAttachmentFiles":
                continue  # in the copied file's bytes, not in the text
            for s in export["sentinels"][k]:
                found = all(normalise(p) in text for p in sentinel_parts(s))
                if fails.check(found, f"{what}: a {k} sentinel not drawn with both reveals"):
                    counts["revealed"].add((k, s))


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--exports", required=True, help="the folder write_oracle_pdf wrote")
    a = ap.parse_args()
    version = subprocess.run(["pdftotext", "-v"], capture_output=True, check=True)
    tool = (version.stderr or version.stdout).decode("utf-8", "replace").splitlines()[0]
    print(f"python {sys.version.split()[0]}, pypdf {pypdf.__version__}, {tool}")
    root = pathlib.Path(a.exports)
    listing = load(root / "pdf-oracle.json")
    fails = Failures()
    fails.check(listing["format"] == "oma-oracle-pdf", "pdf-oracle.json format")
    counts = {"volumes": 0, "items": 0, "texts": 0, "images": 0, "masked": set(),
              "revealed": set()}
    bundles = 0
    for e in listing["exports"]:
        export = load(FIXTURES / e["catalogue"] / e["export"]) if e["export"] else None
        if export is not None:
            fails.check(any(b["kind"] == "revealed" for b in e["bundles"]),
                        f"{e['name']}: the revealed bundle (positive control)")
        for b in e["bundles"]:
            b = dict(b, root=root / b["bundle"])
            check_bundle(e["name"], b, export, fails, counts)
            bundles += 1
    fails.check(bundles >= MIN_BUNDLES, f"bundles: {bundles}")
    fails.check(counts["items"] >= MIN_ITEMS, f"items: {counts['items']}")
    fails.check(len(counts["masked"]) >= MIN_SENTINELS, f"sentinels scanned for: {len(counts['masked'])}")
    fails.check(len(counts["revealed"]) >= MIN_REVEALED,
                f"sentinels found with both reveals: {len(counts['revealed'])}")
    fails.check(counts["images"] >= 1, "previews exercised")
    print(f"{bundles} bundles, {counts['volumes']} volumes, {counts['items']} items "
          f"({counts['texts']} texts compared), {counts['images']} images, "
          f"{len(counts['masked'])} sentinels scanned for, {len(counts['revealed'])} found revealed; "
          f"{fails.checks} checks, {len(fails.items)} failed")
    for f in fails.items[:50]:
        print(f"FAIL {f}")
    return 1 if fails.items else 0


if __name__ == "__main__":
    sys.exit(main())
