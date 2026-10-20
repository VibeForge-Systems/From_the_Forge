#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""The PDF text-layer oracle (M6 spec 14.4, "JSON <-> PDF"; slice 6j1):
sample PDFs made by the export-render helper's PDF backend are read back
by two independent tools, pypdf and poppler's `pdftotext` (run as an
external program; nothing of it is copied or shipped), and checked
against what each sample must and must never hold.

Usage (from Apps/openmessage-archive, in the oracle virtual environment,
with poppler-utils installed):

  OMA_PDF_SAMPLES=DIR cargo test -p oma-export --test pdf_layout -- \\
      --ignored --exact write_samples
  python -I scripts/oracles/pdf_text.py --samples DIR

write_samples writes one PDF per sample (Latin, Greek and Cyrillic;
Arabic; Hebrew; Devanagari; Thai; Chinese, Japanese and Korean; colour
emoji; characters no bundled font has; the hidden-character markers on
and off; a hostile corpus of long tokens, a 10,000-mark combining flood
and a bidi-control bomb) and `expected.json`. This script checks, per
sample:

- pypdf: the file opens, has at least one page, its document title is the
  generic "Messages export — conversation 1" (D112), its outline is not
  empty, and every font is embedded (a /FontFile, /FontFile2 or
  /FontFile3, or a Type 3 font);
- pdftotext (UTF-8): every `must` string occurs and no `never` string
  does, after removing white space and the bidi embedding marks
  (U+202A, U+202B, U+202C) that pdftotext itself inserts around
  right-to-left runs; pdftotext applies the PDF's ActualText, so this is
  the text layer as a reader that honours it sees;
- pypdf's extract_text: the same, for the samples marked `"pypdf": true`.
  pypdf 6.19.0 does not apply ActualText, so Arabic, Devanagari, Thai,
  Hebrew (visual order), the undrawable characters and the markers-off
  spoof are held to pdftotext only.

The marker rule (spec 4.7) is the pair `markers-on` and `markers-off`:
with markers on, the text layer holds `⟨U+202E⟩` and never U+202E; with
markers off, it holds U+202E and never the marker.

A floor: at least 11 samples, and the marker pair, must be present.
"""

import argparse
import json
import pathlib
import subprocess
import sys

import pypdf

TITLE = "Messages export — conversation 1"
BIDI_EMBEDDING = {"‪", "‫", "‬"}
FLOOR = 11


def normalise(text):
    return "".join(c for c in text if not c.isspace() and c not in BIDI_EMBEDDING)


def pdftotext(path):
    out = subprocess.run(
        ["pdftotext", "-enc", "UTF-8", str(path), "-"],
        check=True,
        capture_output=True,
    )
    return out.stdout.decode("utf-8")


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


def check(path, spec, errors):
    name = path.stem
    reader = pypdf.PdfReader(str(path))
    if len(reader.pages) < 1:
        errors.append(f"{name}: no page")
    title = reader.metadata.title if reader.metadata else None
    if title != TITLE:
        errors.append(f"{name}: title {title!r}")
    if not reader.outline:
        errors.append(f"{name}: no outline")
    ok, fonts = embedded(reader)
    if not ok or fonts == 0:
        errors.append(f"{name}: a font is not embedded ({fonts} fonts)")
    texts = {"pdftotext": normalise(pdftotext(path))}
    if spec["pypdf"]:
        texts["pypdf"] = normalise("".join(p.extract_text() for p in reader.pages))
    for tool, text in texts.items():
        for s in spec["must"]:
            if normalise(s) not in text:
                errors.append(f"{name}: {tool} lacks {s!r}")
        for s in spec["never"]:
            if normalise(s) in text:
                errors.append(f"{name}: {tool} holds {s!r}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--samples", required=True, type=pathlib.Path)
    args = ap.parse_args()
    expected = json.loads((args.samples / "expected.json").read_text(encoding="utf-8"))
    errors = []
    if len(expected) < FLOOR:
        errors.append(f"only {len(expected)} samples (floor {FLOOR})")
    for needed in ("markers-on", "markers-off"):
        if needed not in expected:
            errors.append(f"missing the {needed} sample")
    for name, spec in sorted(expected.items()):
        path = args.samples / f"{name}.pdf"
        if not path.is_file():
            errors.append(f"{name}: no PDF")
            continue
        check(path, spec, errors)
    for e in errors:
        print(e, file=sys.stderr)
    print(f"pdf_text: {len(expected)} samples, {len(errors)} errors")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
