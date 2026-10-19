#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""The HTML oracle (M6 spec 14.4, "JSON <-> HTML", slice 6g): every page of
an export's HTML is read back by two independent parsers, the standard
library's html.parser and html5lib (an HTML5 tree builder), and checked
against the template's rules and against the same export's JSON,
independently of the Rust export code.

Usage (from Apps/openmessage-archive, in the oracle virtual environment):

  OMA_ORACLE_HTML_OUT=DIR cargo test -p oma-export --test html -- \\
      --ignored --exact write_oracle_html
  python -I scripts/oracles/html_oracle.py --exports DIR

write_oracle_html imports every catalogue entry into its own library and
exports the whole archive to one HTML + JSON bundle twice: `files`
(history, files copied, previews on with the test's 1 x 1 PNG renditions,
larger previews, markers on) and `names` (history, names only, markers
off). This script:

- byte scan: UTF-8 without a BOM; no CR, no C0 control but LF and TAB,
  no C1 and no DEL anywhere;
- html.parser (convert_charrefs off, so every reference is seen): the
  doctype; the head in its fixed order, the CSP meta first after the
  charset and equal to the policy html-oracle.json states; the one
  <style>, whose SHA-256 (base64) is the CSP's style-src hash; no element
  outside the template's list (so no script, base, form, frame, object,
  embed, link, audio, video, svg or math), no event, style or unknown
  attribute; only the five references &amp; &lt; &gt; &quot; &#x27;;
  links of the template's grammar (fragments, index.html, the pages, the
  larger previews) and no link to an original file; image sources only
  data:image/png|jpeg;base64 whose bytes decode to the format's signature;
  one h1; main, then nav#days, then footer#about; ids unique; every item
  link (#m<n>) points to an article already read on the page;
- html5lib, strict (any parse error fails): the same document builds a
  tree with no script element and no attribute outside the template;
  per article (m<n>): its body paragraph's text, read from the tree with
  each marker element (U+XXXX) turned back into its character and each
  placeholder as U+FFFC, equals the JSON item's segments under T (CR and
  CRLF as LF; placeholders as U+FFFC; transcriptions apart); its
  transcription likewise; the sender equals the JSON handle's address;
- completeness: each conversation file's articles, over its parts in
  order, are the JSON file's items without folded rows, in order, with
  the same ordinals; index.html links every page; the manifest's
  previewsEmbedded equals the images found;
- masking: no sentinel of the entry's export.json appears in any byte or
  in any decoded text of the bundle;
- every bundle's SHA256SUMS verifies.

Output is counts and check names only (never content, not even an
element or attribute name that is not the template's). Run with
`python3 -I`; the standard library plus html5lib.
"""
import argparse
import base64
import hashlib
import html.parser
import json
import pathlib
import re
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.dont_write_bytecode = True

APP = HERE.parent.parent
FIXTURES = APP / "tests" / "fixtures"
FFFC = "￼"

ELEMENTS = {
    "html", "head", "meta", "title", "style", "body", "a", "header", "p", "h1", "h2", "h3",
    "main", "section", "article", "span", "bdi", "time", "figure", "img", "figcaption",
    "details", "summary", "ol", "ul", "li", "div", "nav", "footer",
}
ATTRIBUTES = {
    "html": {"lang"}, "meta": {"charset", "http-equiv", "content", "name"},
    "a": {"href", "class"}, "main": {"id"}, "nav": {"id", "aria-label"}, "footer": {"id"},
    "section": {"aria-labelledby"}, "h2": {"id"}, "h3": {"id"},
    "article": {"id", "aria-labelledby", "class"}, "span": {"class"}, "bdi": {"dir", "class"},
    "time": {"datetime"}, "img": {"alt", "width", "height", "src"}, "p": {"class"},
    "ul": {"class"}, "ol": {"class"}, "div": {"class"},
}
HREF = re.compile(r"#[A-Za-z0-9+-]+|index\.html|c[0-9]{4,}(-[0-9]+)?\.html"
                  r"|no-conversation(-[0-9]+)?\.html|previews/p[0-9]+\.(jpg|png)")
REFS = ("&amp;", "&lt;", "&gt;", "&quot;", "&#x27;")
SIGNATURES = {"image/png": b"\x89PNG\r\n\x1a\n", "image/jpeg": b"\xff\xd8\xff"}
DATETIME = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,3})?Z")


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


def byte_scan(raw, what, fails):
    fails.check(not raw.startswith(b"\xef\xbb\xbf"), f"{what}: BOM")
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        fails.check(False, f"{what}: not UTF-8")
        return None
    bad = [c for c in text if (ord(c) < 0x20 and c not in "\n\t") or 0x7F <= ord(c) <= 0x9F]
    fails.check(not bad, f"{what}: a raw control ({len(bad)})")
    return text


class Tokens(html.parser.HTMLParser):
    """The stdlib tokenizer's view of a page, with every reference kept."""

    def __init__(self):
        super().__init__(convert_charrefs=False)
        self.events = []
        self.decl = []
        self.refs = []
        self.style = []
        self._in_style = False

    def handle_decl(self, d):
        self.decl.append(d)

    def handle_starttag(self, tag, attrs):
        self.events.append(("start", tag, attrs))
        self._in_style = tag == "style"

    def handle_startendtag(self, tag, attrs):
        self.events.append(("start", tag, attrs))

    def handle_endtag(self, tag):
        self.events.append(("end", tag, []))
        self._in_style = False

    def handle_data(self, data):
        if self._in_style:
            self.style.append(data)
        self.events.append(("data", data, []))

    def handle_entityref(self, name):
        self.refs.append(f"&{name};")

    def handle_charref(self, name):
        self.refs.append(f"&#{name};")

    def handle_comment(self, data):
        self.events.append(("comment", data, []))

    def handle_pi(self, data):
        self.events.append(("pi", data, []))


def stdlib_checks(text, name, csp, fails, index):
    t = Tokens()
    t.feed(text)
    t.close()
    what = name
    fails.check(t.decl == ["doctype html"], f"{what}: doctype")
    fails.check(all(r in ("&amp;", "&lt;", "&gt;", "&quot;", "&#x27;") for r in t.refs), f"{what}: a reference")
    # Raw ampersands: every '&' starts one of the five references.
    amps = [m.start() for m in re.finditer("&", text)]
    fails.check(all(text.startswith(REFS, i) for i in amps), f"{what}: a raw &")
    starts = [(tag, dict(a)) for k, tag, a in t.events if k == "start"]
    fails.check(not any(k in ("comment", "pi") for k, _, _ in t.events), f"{what}: a comment")
    tags = [s[0] for s in starts]
    fails.check(tags[:9] == ["html", "head", "meta", "meta", "meta", "meta", "meta", "title", "style"],
                f"{what}: head order")
    metas = [a for tag, a in starts[:9] if tag == "meta"]
    if len(metas) == 5:
        fails.check(metas[0] == {"charset": "utf-8"}, f"{what}: charset first")
        fails.check(metas[1].get("http-equiv") == "Content-Security-Policy" and metas[1].get("content") == csp,
                    f"{what}: the CSP meta second")
        fails.check(metas[2] == {"name": "referrer", "content": "no-referrer"}, f"{what}: referrer")
    style = "".join(t.style)
    h = base64.b64encode(hashlib.sha256(style.encode("utf-8")).digest()).decode("ascii")
    fails.check(f"style-src 'sha256-{h}'" in csp, f"{what}: the style hash")
    fails.check(tags.count("style") == 1, f"{what}: one style")
    fails.check(tags.count("h1") == 1, f"{what}: one h1")
    ids = []
    seen = set()
    item_links = 0
    images = 0
    for tag, a in starts:
        fails.check(tag in ELEMENTS, f"{what}: an element outside the template")
        allowed = ATTRIBUTES.get(tag, set())
        for k in a:
            fails.check(k in allowed and not k.startswith("on") and k != "style",
                        f"{what}: an attribute outside the template")
        if "id" in a:
            ids.append(a["id"])
        if tag == "a":
            href = a.get("href") or ""
            fails.check(HREF.fullmatch(href) is not None, f"{what}: href outside the grammar")
            fails.check("attachments" not in href, f"{what}: a link to an original")
            m = re.fullmatch(r"#(m[0-9]+)", href)
            if m:
                fails.check(m.group(1) in seen, f"{what}: a forward item link")
                item_links += 1
        if tag == "time" and "datetime" in a:
            fails.check(DATETIME.fullmatch(a["datetime"]) is not None, f"{what}: datetime")
        if tag == "img":
            src = a.get("src") or ""
            m = re.fullmatch(r"data:(image/png|image/jpeg);base64,([A-Za-z0-9+/]*={0,2})", src)
            fails.check(m is not None and a.get("alt") == "", f"{what}: img src or alt")
            if m:
                try:
                    raw = base64.b64decode(m.group(2), validate=True)
                    fails.check(raw.startswith(SIGNATURES[m.group(1)]), f"{what}: img signature")
                except ValueError:
                    fails.check(False, f"{what}: img base64")
            images += 1
        if "id" in a:
            seen.add(a["id"])
    fails.check(len(ids) == len(set(ids)), f"{what}: ids unique")
    # Landmarks: main, then nav#days, then footer#about.
    order = [tag + ("#" + a["id"] if "id" in a else "") for tag, a in starts
             if tag in ("main", "nav", "footer")]
    want = ["main#main", "footer#about"] if index else ["main#main", "nav#days", "footer#about"]
    fails.check(order == want, f"{what}: landmarks")
    hrefs = [a.get("href") for tag, a in starts if tag == "a"]
    return {"images": images, "item_links": item_links, "hrefs": hrefs}


def html5_tree(text, what, fails):
    import html5lib
    p = html5lib.HTMLParser(strict=True, namespaceHTMLElements=False)
    try:
        doc = p.parse(text)
    except Exception as e:  # html5lib raises ParseError on any error in strict mode
        fails.check(False, f"{what}: html5lib strict: {type(e).__name__}")
        return None
    fails.check(not p.errors, f"{what}: html5lib errors")
    for el in doc.iter():
        tag = el.tag if isinstance(el.tag, str) else ""
        fails.check(tag in ELEMENTS, f"{what}: html5lib: an element outside the template")
        for k in el.attrib:
            fails.check(k in ATTRIBUTES.get(tag, set()), f"{what}: html5lib: an attribute outside the template")
    return doc


def classes(el):
    return (el.get("class") or "").split()


def text_of(el):
    """The text below el: marker elements back to their character,
    placeholders as U+FFFC."""
    out = [el.text or ""]
    for c in el:
        cl = classes(c)
        if c.tag == "span" and "mark" in cl:
            v = "".join(c.itertext())
            m = re.fullmatch(r"U\+([0-9A-F]{4,6})", v)
            out.append(chr(int(m.group(1), 16)) if m else "�")
        elif c.tag == "span" and "ph" in cl:
            out.append(FFFC)
        elif c.tag == "span" and "label" in cl:
            pass
        else:
            out.append(text_of(c))
        out.append(c.tail or "")
    return "".join(out)


def t_normalise(s):
    return s.replace("\r\n", "\n").replace("\r", "\n")


def json_parts(item):
    """(body text, transcription text) of a JSON item under T, or None."""
    body = item["body"]
    if body["kind"] != "text":
        return None, None
    main, spoken = [], []
    for s in body["segments"]:
        piece = t_normalise(s["text"] or "") if s["kind"]["token"] == "text" else FFFC
        if s["kind"]["token"] == "text" and s["text"] is None:
            continue
        (spoken if s["transcription"] else main).append(piece)
    return ("".join(main) if main else None), ("".join(spoken) if spoken else None)


def articles(doc):
    out = []
    for a in doc.iter("article"):
        n = int(a.get("id")[1:])
        body = trans = who = None
        for p in a.iter("p"):
            cl = classes(p)
            if "body" in cl and body is None:
                body = text_of(p)
            elif "transcription" in cl and trans is None:
                t = text_of(p)
                trans = t[1:] if t.startswith(" ") else t
        for s in a.iter("span"):
            if "who" in classes(s):
                who = text_of(s)
                break
        out.append((n, body, trans, who))
    return out


def page_key(name):
    stem = name[:-5]
    m = re.fullmatch(r"(.*)-([0-9]+)", stem)
    if m and m.group(1) != "no":
        return m.group(1), int(m.group(2))
    return stem, 1


def jsonl(path):
    return [json.loads(line) for line in path.read_text(encoding="utf-8").split("\n") if line]


def verify_sums(bundle, what, fails):
    for line in (bundle / "SHA256SUMS").read_text(encoding="utf-8").splitlines():
        h, rel = line.split("  ", 1)
        got = hashlib.sha256((bundle / rel).read_bytes()).hexdigest()
        fails.check(got == h, f"{what}: a SHA256SUMS line")


def check_bundle(name, b, csp, export, fails, counts):
    bundle = b["root"]
    what = f"{name}/{b['kind']}"
    verify_sums(bundle, what, fails)
    handles = {h["n"]: h["address"]["text"] for h in load(bundle / "json" / "handles.json")["handles"]}
    pages = sorted(p.name for p in (bundle / "html").glob("*.html"))
    index_hrefs = []
    by_file = {}
    images = 0
    decoded = []
    for page in pages:
        raw = (bundle / "html" / page).read_bytes()
        text = byte_scan(raw, f"{what}/{page}", fails)
        if text is None:
            continue
        info = stdlib_checks(text, f"{what}/{page}", csp, fails, page == "index.html")
        doc = html5_tree(text, f"{what}/{page}", fails)
        counts["pages"] += 1
        if doc is not None:
            decoded.append(text_of(doc))
        if page == "index.html":
            index_hrefs = info["hrefs"]
            continue
        images += info["images"]
        counts["item_links"] += info["item_links"]
        if doc is not None:
            stem, part = page_key(page)
            by_file.setdefault(stem, []).append((part, articles(doc)))
    for page in pages:
        if page != "index.html":
            fails.check(page in index_hrefs, f"{what}: {page} not in the index")
    for stem, parts in by_file.items():
        arts = [a for _, p in sorted(parts) for a in p]
        path = bundle / "json" / f"{stem}.jsonl"
        fails.check(path.exists(), f"{what}: {stem} has no JSON file")
        if not path.exists():
            continue
        items = [x for x in jsonl(path) if x["type"] == "item" and x["context"]["foldedInto"] is None]
        fails.check([a[0] for a in arts] == [x["n"] for x in items], f"{what}: {stem} items and order")
        for (n, body, trans, who), x in zip(arts, items):
            want_body, want_trans = json_parts(x)
            fails.check(body == want_body, f"{what}: {stem} m{n}: body text")
            fails.check(trans == want_trans, f"{what}: {stem} m{n}: transcription")
            if x["sender"]["reason"]["token"] == "resolved" and x["sender"]["handle"] in handles:
                addr = handles[x["sender"]["handle"]]
                if addr:
                    fails.check(who == t_normalise(addr), f"{what}: {stem} m{n}: sender")
            counts["items"] += 1
    m = load(bundle / "manifest.json")
    fails.check(m["counts"]["attachments"]["previewsEmbedded"] == images, f"{what}: previewsEmbedded")
    counts["images"] += images
    if export is not None:
        blob = b"\n".join((p.read_bytes() for p in bundle.rglob("*") if p.is_file()))
        text = "\n".join(decoded)
        for k in ("oneTimeCodes", "earlierText", "editText", "subjectHistory", "alternativeText",
                  "unsentAttachmentFiles"):
            for s in export["sentinels"][k]:
                fails.check(s.encode("utf-8") not in blob and s not in text, f"{what}: a {k} sentinel leaked")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--exports", required=True, help="the folder write_oracle_html wrote")
    a = ap.parse_args()
    import html5lib
    from importlib.metadata import version
    print(f"python {sys.version.split()[0]}, html5lib {version('html5lib')}")
    del html5lib
    root = pathlib.Path(a.exports)
    listing = load(root / "html-oracle.json")
    fails = Failures()
    fails.check(listing["format"] == "oma-oracle-html", "html-oracle.json format")
    csp = listing["csp"]
    fails.check(csp == "default-src 'none'; img-src data:; style-src 'sha256-"
                + csp.split("'sha256-")[1].split("'")[0]
                + "'; base-uri 'none'; form-action 'none'", "the policy's shape")
    counts = {"pages": 0, "items": 0, "images": 0, "item_links": 0}
    bundles = 0
    for e in listing["exports"]:
        export = load(FIXTURES / e["catalogue"] / e["export"]) if e["export"] else None
        for b in e["bundles"]:
            b = dict(b, root=root / b["bundle"])
            check_bundle(e["name"], b, csp, export, fails, counts)
            bundles += 1
    fails.check(bundles >= 40, "every catalogue entry exported twice")
    fails.check(counts["images"] >= 2 and counts["item_links"] >= 1, "previews and reply links exercised")
    print(f"{bundles} bundles, {counts['pages']} pages, {counts['items']} items, {counts['images']} images, "
          f"{counts['item_links']} item links; {fails.checks} checks, {len(fails.items)} failed")
    for f in fails.items[:50]:
        print(f"FAIL {f}")
    return 1 if fails.items else 0


if __name__ == "__main__":
    sys.exit(main())
