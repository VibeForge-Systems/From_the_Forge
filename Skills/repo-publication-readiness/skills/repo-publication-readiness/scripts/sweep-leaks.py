#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""
sweep-leaks.py — find what a secret scanner is not built to find.

Secret scanners (gitleaks, TruffleHog) key on the shape of credentials. A
repository being made public leaks other things too: who committed it (author,
committer, tagger and trailer emails), where it was built (private IPs,
internal hostnames, UNC shares, home-directory paths that carry a username),
whose work it was (employer and client names, from a private deny list), and
whole files that should never have been added (.env, keys, keystores, HAR
captures, notebooks with outputs, oversized blobs).

This walks the same ground a history scanner does, plus the two places those
scanners do not look:
  * merge resolutions — `git log -p` prints no diff for a merge commit, so a
    line introduced only while resolving a conflict is invisible to it. This
    script adds a `--remerge-diff` pass (git >= 2.36; `--cc` fallback).
  * commit messages, tag messages and ref names.

Only ADDED lines are examined: a removed line was added somewhere earlier, and
that is where it is reported, with the commit that introduced it.

Usage:
  sweep-leaks.py [--repo PATH] [--all-refs | --range REVSPEC | --commits FILE]
                 [--denylist FILE]... [--allow FILE] [--identities FILE]
                 [--max-blob-mb N] [--json OUT] [--quiet]

Exit codes: 0 nothing needing review · 1 findings · 2 incomplete (a git command
failed, so the sweep did not cover what it claims) · 3 usage error.

The private deny list holds the names you must not publish — an employer, its
clients, internal product codenames. It is read from --denylist, then
$RPR_DENYLIST, then ~/.config/repo-publication-readiness/denylist. It must
never be committed to the repository it protects: committing it publishes
exactly the names it exists to keep out. This script refuses a deny list that
lives inside the repository being swept.

Requires Python 3.11+ (tomllib) and git 2.22+. No third-party modules.
"""
from __future__ import annotations

import argparse
import fnmatch
import json
import os
import re
import subprocess
import sys
import tempfile
from collections import defaultdict
from dataclasses import dataclass, field
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - environment check
    sys.stderr.write("sweep-leaks: Python 3.11+ is required (tomllib)\n")
    sys.exit(3)

# --------------------------------------------------------------------------
# Patterns. Each is deliberately narrow: a sweep that drowns the reader in
# false positives gets skimmed, and a skimmed sweep finds nothing.
# --------------------------------------------------------------------------
OCTET = r"(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)"
PRIVATE_IP = re.compile(
    r"(?<![\d.])(?:"
    rf"10\.{OCTET}\.{OCTET}\.{OCTET}"
    rf"|172\.(?:1[6-9]|2\d|3[01])\.{OCTET}\.{OCTET}"
    rf"|192\.168\.{OCTET}\.{OCTET}"
    rf"|100\.(?:6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.{OCTET}\.{OCTET}"  # RFC 6598 CGNAT (Tailscale et al.)
    r")(?![\d.]*\d)"
)
ULA_V6 = re.compile(r"(?<![0-9A-Fa-f:])f[cd][0-9a-f]{2}:[0-9a-f:]{2,}", re.I)
# A hostname ENDING in an internal suffix: not followed by more name, a call or
# an index, so `settings.local.py`, `.env.local`, `threading.local()` and
# `self.internal` are not mistaken for hosts.
INTERNAL_HOST = re.compile(
    r"(?<![.\w-])((?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+"
    r"(?:local|lan|internal|intranet|corp|localdomain|home\.arpa))"
    r"(?![\w(\[-])(?!\.\w)",
    re.I,
)
CODE_HEADS = {
    "self", "this", "cls", "obj", "os", "sys", "ctx", "config", "settings", "process",
    "window", "module", "exports", "props", "state", "args", "opts", "options", "req",
    "res", "app", "env", "threading", "thread", "super", "parent", "data", "s", "t", "x",
}
# Conventions every repo shares; naming them reveals nothing about anyone.
GENERIC_HOSTS = re.compile(
    r"^(?:localhost\.localdomain|host\.docker\.internal|gateway\.docker\.internal|"
    r"(?:[a-z0-9.-]+\.)?(?:svc\.)?cluster\.local|metadata\.google\.internal)$",
    re.I,
)
UNC_PATH = re.compile(r"\\\\[A-Za-z][A-Za-z0-9._-]{2,62}\\[A-Za-z0-9.$_-][A-Za-z0-9.$_ -]{1,79}")
# Windows profile paths match in any case; POSIX homes only as a path START
# (`Data/users/systems` in prose and `/api/users/x` in a URL are not homes).
HOME_PATH = re.compile(
    r"(?:(?i:[A-Za-z]:(?:\\{1,2}|/)Users(?:\\{1,2}|/))|(?<![\w.:-])/home/|(?<![\w.:-])/Users/)"
    r"([A-Za-z0-9._-]{2,})"
)
GENERIC_USERS = {
    "user", "username", "runner", "root", "ubuntu", "admin", "you", "me", "name",
    "example", "default", "public", "shared", "vscode", "node", "dev", "developer",
    "builder", "build", "ci", "jenkins", "github", "gitlab", "docker", "app",
    "<user>", "yourname", "your-name", "your_name", "someone", "foo", "bar",
    "all users", "defaultuser0", "linuxbrew",
}
# RFC 2606 / RFC 6761 reserved names, GitHub noreply, and role addresses.
EXAMPLE_EMAIL = re.compile(
    r"@(?:[a-z0-9.-]*\.)?(?:example\.(?:com|org|net)|example|test|invalid|localhost|local)$"
    r"|@(?:users\.)?noreply\.github\.com$|^(?:git|noreply|no-reply)@",
    re.I,
)
# `icon@2x.png` is a file name, not an address.
FILE_TLDS = {
    "png", "jpg", "jpeg", "gif", "svg", "webp", "ico", "bmp", "tif", "tiff", "avif", "js",
    "mjs", "cjs", "ts", "tsx", "jsx", "css", "scss", "json", "md", "txt", "rs", "py", "go",
    "html", "htm", "xml", "yaml", "yml", "toml", "lock", "map", "wasm", "zip", "gz",
}
# Files whose job is to carry third-party contact details.
EMAIL_EXEMPT_FILES = re.compile(
    r"(?:^|/)(?:NOTICE|AUTHORS|CONTRIBUTORS|MAINTAINERS|CODEOWNERS|LICEN[CS]E|COPYING|THIRD[_-]PARTY[^/]*|"
    r"package-lock\.json|npm-shrinkwrap\.json|yarn\.lock|pnpm-lock\.yaml|Cargo\.lock|poetry\.lock|"
    r"Pipfile\.lock|uv\.lock|composer\.lock|Gemfile\.lock|go\.sum|[^/]*\.dist-info/[^/]+)(?:\.[A-Za-z]+)?$",
    re.I,
)
GENERIC_UNC_HOSTS = {"server", "localhost", "host", "machine", "computer", "example", "wsl$", "wsl.localhost", "?", "."}
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
NOREPLY_DEFAULT = [
    re.compile(r"@users\.noreply\.github\.com$", re.I),
    re.compile(r"^noreply@github\.com$", re.I),
]
TRAILER_KEYS = re.compile(
    r"^(Signed-off-by|Co-authored-by|Reviewed-by|Acked-by|Tested-by|Reported-by|Helped-by|Suggested-by):\s*(.+)$",
    re.I,
)
# Files whose mere presence in history is the finding. Examples are exempt.
SENSITIVE_NAME = re.compile(
    r"(?:^|/)(?:"
    r"\.env(?:\.(?!example$|sample$|template$|dist$|defaults?$)[^/]+)?"
    r"|(?!example\.|sample\.|template\.)[^/]+\.env"
    r"|id_(?:rsa|dsa|ecdsa|ed25519)(?!\.pub$)"
    r"|[^/]+\.(?:pem|key|p12|pfx|jks|keystore|kdbx|ovpn|tfstate|tfstate\.backup|har|pcap|pcapng)"
    r"|credentials(?:[._-][^/]*)?\.json"
    r"|\.netrc|_netrc|\.pypirc|\.htpasswd|\.git-credentials"
    r"|service[-_]account[^/]*\.json"
    r")$",
    re.I,
)
SAFE_KEY_NAMES = re.compile(r"(?:^|/)(?:[^/]*public[^/]*|[^/]*\.pub)$", re.I)

DIFF_HEADER = re.compile(r"^diff --(?:git|cc|combined) (?:a/)?(.+?)(?: b/(.+))?$")
COMMIT_MARK = "\x1e\x1fRPR-COMMIT\x1f"


@dataclass
class Finding:
    category: str
    detail: str
    commit: str = ""
    path: str = ""
    line: str = ""

    def key(self) -> tuple:
        return (self.category, self.detail.lower(), self.path)


@dataclass
class Allow:
    category: str
    path: str = "*"
    match: re.Pattern | None = None
    reason: str = ""

    def covers(self, f: Finding) -> bool:
        if self.category not in ("*", f.category):
            return False
        if not fnmatch.fnmatch(f.path or "", self.path):
            return False
        if self.match is not None and not self.match.search(f.detail):
            return False
        return True


@dataclass
class Report:
    findings: list[Finding] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)
    identities: dict[str, dict] = field(default_factory=dict)
    commits_scanned: int = 0
    notes: list[str] = field(default_factory=list)


# --------------------------------------------------------------------------
def git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", "-C", str(repo), "-c", "core.quotePath=false", *args],
        capture_output=True,
        check=check,
    )


def stream_git(repo: Path, args: list[str], report: Report, label: str):
    """Yield decoded lines from a git command; record failure rather than hide it."""
    cmd = ["git", "-C", str(repo), "-c", "core.quotePath=false", *args]
    with tempfile.TemporaryFile() as errf:
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=errf)
        assert proc.stdout is not None
        for raw in proc.stdout:
            yield raw.decode("utf-8", errors="replace").rstrip("\n")
        rc = proc.wait()
        errf.seek(0)
        err = errf.read().decode("utf-8", errors="replace")
    if rc != 0:
        report.errors.append(f"{label}: git {' '.join(args[:3])} … exited {proc.returncode}: {err.strip()[:300]}")


def git_version(repo: Path) -> tuple[int, ...]:
    out = git(repo, "version").stdout.decode()
    m = re.search(r"(\d+)\.(\d+)(?:\.(\d+))?", out)
    return tuple(int(x or 0) for x in m.groups()) if m else (0, 0, 0)


def load_denylist(paths: list[str], repo: Path, report: Report) -> list[re.Pattern]:
    candidates = list(paths)
    if not candidates and os.environ.get("RPR_DENYLIST"):
        candidates = [os.environ["RPR_DENYLIST"]]
    if not candidates:
        default = Path.home() / ".config" / "repo-publication-readiness" / "denylist"
        if default.is_file():
            candidates = [str(default)]
    pats: list[re.Pattern] = []
    repo_real = repo.resolve()
    for p in candidates:
        path = Path(p).expanduser().resolve()
        if repo_real == path or repo_real in path.parents:
            sys.stderr.write(
                f"sweep-leaks: refusing deny list {path}: it is inside the repository being "
                "swept, so publishing the repo would publish the list. Move it outside.\n"
            )
            sys.exit(3)
        if not path.is_file():
            sys.stderr.write(f"sweep-leaks: deny list {path} does not exist\n")
            sys.exit(3)
        for n, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            try:
                pats.append(re.compile(line, re.I))
            except re.error as e:
                sys.stderr.write(f"sweep-leaks: {path}:{n}: bad regex ({e})\n")
                sys.exit(3)
    if not pats:
        report.notes.append(
            "NO PRIVATE DENY LIST — employer, client and codename terms were NOT checked. "
            "Create ~/.config/repo-publication-readiness/denylist (one regex per line)."
        )
    return pats


def load_allow(path: str | None) -> list[Allow]:
    if not path:
        return []
    data = tomllib.loads(Path(path).read_text(encoding="utf-8"))
    out = []
    for i, a in enumerate(data.get("allow", []), 1):
        if "category" not in a or not a.get("reason"):
            sys.stderr.write(f"sweep-leaks: allow entry #{i} needs 'category' and a 'reason'\n")
            sys.exit(3)
        out.append(
            Allow(
                category=a["category"],
                path=a.get("path", "*"),
                match=re.compile(a["match"], re.I) if a.get("match") else None,
                reason=a["reason"],
            )
        )
    return out


def load_identity_patterns(path: str | None) -> list[re.Pattern]:
    pats = list(NOREPLY_DEFAULT)
    if path:
        for line in Path(path).read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if line and not line.startswith("#"):
                pats.append(re.compile(line, re.I))
    return pats


# --------------------------------------------------------------------------
PRINTABLE_RUN = re.compile(rb"[\x20-\x7e]{6,}")
# Compressed or glyph data: printable runs inside them are noise, not text.
NOISE_BINARIES = re.compile(
    r"\.(?:ttf|otf|woff2?|eot|ttc|mp3|mp4|m4a|mov|avi|mkv|webm|ogg|wav|flac|zip|gz|tgz|bz2|xz|7z|zst|rar|"
    r"jar|war|whl|nupkg|apk|ipa|dmg|iso|png|gif|webp|ico|icns|wasm|pyc|class|o|a|so|dylib|dll|exe|lib)$",
    re.I,
)
OFFICE_EXT = re.compile(r"\.(?:docx|xlsx|pptx|odt|ods|odp|docm|xlsm)$", re.I)
OFFICE_PARTS = re.compile(r"^(?:docProps/.*\.xml|meta\.xml|word/document\.xml|word/comments\.xml|"
                          r"xl/sharedStrings\.xml|ppt/slides/slide\d+\.xml|content\.xml)$")


def binary_text(content: bytes, path: str) -> tuple[str, list[str]]:
    """Text worth sweeping inside a binary blob, plus any metadata notes."""
    notes: list[str] = []
    if content[:2] == b"PK" and OFFICE_EXT.search(path):
        import io
        import zipfile
        try:
            with zipfile.ZipFile(io.BytesIO(content)) as z:
                parts = [n for n in z.namelist() if OFFICE_PARTS.match(n)]
                xml = b" ".join(z.read(n)[:2_000_000] for n in parts)
            # authorship metadata is the usual leak in office files
            for tag in (b"dc:creator", b"cp:lastModifiedBy", b"Company", b"Manager"):
                for m in re.finditer(rb"<" + tag + rb">([^<]{1,200})</" + tag + rb">", xml):
                    notes.append(f"{tag.decode()}={m.group(1).decode('utf-8', 'replace')}")
            return re.sub(rb"<[^>]+>", b" ", xml).decode("utf-8", "replace"), notes
        except (zipfile.BadZipFile, KeyError, ValueError):
            pass
    if content[:2] == b"\xff\xd8" and b"Exif\x00\x00" in content[:65536]:
        app1 = content[:65536]
        if b"\x88\x25" in app1 or b"\x25\x88" in app1:
            notes.append("EXIF GPS IFD tag present (heuristic) — confirm with exiftool, strip with exiftool -all=")
    runs = PRINTABLE_RUN.findall(content)
    return "\n".join(r.decode("ascii") for r in runs), notes


# --------------------------------------------------------------------------
def scan_text(text: str, where: str, commit: str, path: str, deny: list[re.Pattern],
              out: list[Finding], ident_ok: list[re.Pattern] | None = None):
    for m in PRIVATE_IP.finditer(text):
        out.append(Finding("private-ip", m.group(0), commit, path, text.strip()[:200]))
    for m in ULA_V6.finditer(text):
        out.append(Finding("private-ip", m.group(0), commit, path, text.strip()[:200]))
    for m in INTERNAL_HOST.finditer(text):
        host = m.group(1)
        labels = host.lower().split(".")
        if GENERIC_HOSTS.match(host) or (len(labels) == 2 and labels[0] in CODE_HEADS):
            continue
        out.append(Finding("internal-host", host, commit, path, text.strip()[:200]))
    for m in UNC_PATH.finditer(text):
        host = m.group(0)[2:].split("\\", 1)[0].lower()
        if host not in GENERIC_UNC_HOSTS:
            out.append(Finding("internal-host", m.group(0), commit, path, text.strip()[:200]))
    if ident_ok is not None and "@" in text and not EMAIL_EXEMPT_FILES.search(path):
        for m in EMAIL.finditer(text):
            e = m.group(0)
            if e.rsplit(".", 1)[-1].lower() in FILE_TLDS:
                continue
            if where == "binary":
                # strings from a binary run fields together ("x@example.invalidiMessage");
                # judge every shorter candidate that still ends at a name boundary too
                at = e.index("@")
                cands = [e[:k] for k in range(len(e), at + 3, -1) if e[k - 1].isalpha()]
            else:
                cands = [e]
            if any(EXAMPLE_EMAIL.search(c) or c.rsplit(".", 1)[-1].lower() in FILE_TLDS
                   or any(p.search(c) for p in ident_ok) for c in cands):
                continue
            if True:
                out.append(Finding("email", e, commit, path, text.strip()[:200]))
    for m in HOME_PATH.finditer(text):
        if m.group(1).lower().strip("<>$%{}") not in GENERIC_USERS and not m.group(1).startswith(("$", "%", "<", "{")):
            out.append(Finding("local-path", m.group(0), commit, path, text.strip()[:200]))
    for pat in deny:
        for m in pat.finditer(text):
            out.append(Finding("denylist", m.group(0), commit, path, text.strip()[:200]))


def note_identity(report: Report, kind: str, ident: str, commit: str):
    m = re.search(r"<([^>]*)>", ident)
    email = (m.group(1) if m else ident).strip()
    entry = report.identities.setdefault(email.lower(), {"email": email, "names": set(), "roles": set(), "commits": 0, "first": commit})
    name = ident.split("<", 1)[0].strip()
    if name:
        entry["names"].add(name)
    entry["roles"].add(kind)
    entry["commits"] += 1


def walk_log(repo: Path, rev_args: list[str], deny, report: Report, merges_mode: str | None,
             ident_ok: list[re.Pattern]):
    """Stream one `git log -p` and scan headers, messages and added lines."""
    fmt = f"{COMMIT_MARK}%H%x1f%an <%ae>%x1f%cn <%ce>%n%B%x1e"
    args = ["log", "-p", "-U0", "--no-color", "--no-ext-diff", "--no-textconv", "--full-history", f"--format={fmt}"]
    if merges_mode:
        args += ["--merges", merges_mode]
    args += rev_args
    commit, path, in_msg, label = "", "", False, "merge-resolutions" if merges_mode else "history"
    for line in stream_git(repo, args, report, label):
        if line.startswith(COMMIT_MARK):
            sha, author, committer = line[len(COMMIT_MARK):].split("\x1f", 2)
            commit, path, in_msg = sha, "", True
            if not merges_mode:
                report.commits_scanned += 1
                note_identity(report, "author", author, sha)
                note_identity(report, "committer", committer, sha)
            continue
        if in_msg:
            if line.endswith("\x1e"):
                line, in_msg = line[:-1], False
            if not merges_mode:
                t = TRAILER_KEYS.match(line.strip())
                if t:
                    note_identity(report, t.group(1).lower(), t.group(2), commit)
                if not t:
                    scan_text(line, "message", commit, "(commit message)", deny, report.findings, ident_ok)
            continue
        h = DIFF_HEADER.match(line)
        if h:
            path = h.group(2) or h.group(1)
            continue
        if line.startswith("+++ ") or line.startswith("--- "):
            continue
        # added lines: '+' in a normal diff; '++', '+ ', ' +' in a combined diff
        added = None
        if line.startswith("+"):
            added = line[1:]
            if merges_mode and added[:1] in "+ ":
                added = added[1:]
        elif merges_mode and line.startswith(" +"):
            added = line[2:]
        if added is not None:
            scan_text(added, "diff", commit, path, deny, report.findings, ident_ok)


def blob_findings(repo: Path, rev_args: list[str], max_bytes: int, report: Report,
                  deny: list[re.Pattern], ident_ok: list[re.Pattern], binary_cap: int):
    """Paths that are findings by name, notebooks with outputs, and oversized blobs."""
    seen_blob: set[str] = set()
    raw_args = ["log", "--raw", "--no-abbrev", "--no-renames", "--diff-filter=AM", "--format=%x1e%H"]
    if git_version(repo) >= (2, 31):
        raw_args.append("--diff-merges=first-parent")  # blobs a merge resolution introduced
    raw_args += rev_args
    commit = ""
    blobs: list[tuple[str, str, str]] = []
    for line in stream_git(repo, raw_args, report, "blob-list"):
        if line.startswith("\x1e"):
            commit = line[1:]
            continue
        if not line.startswith(":"):
            continue
        meta, _, path = line.partition("\t")
        parts = meta.split()
        if len(parts) < 5:
            continue
        mode, blob = parts[1], parts[3]
        if mode == "160000":  # submodule gitlink, not a blob
            report.notes.append(f"submodule at {path} (commit {commit[:8]}) — audit that repository too")
            continue
        if SENSITIVE_NAME.search(path) and not SAFE_KEY_NAMES.search(path):
            report.findings.append(Finding("sensitive-file", path, commit, path, "file added to history"))
        if blob not in seen_blob:
            seen_blob.add(blob)
            blobs.append((blob, commit, path))
    if not blobs:
        return
    proc = subprocess.run(
        ["git", "-C", str(repo), "cat-file", "--batch-check=%(objectname) %(objectsize)"],
        input="\n".join(b for b, _, _ in blobs).encode(),
        capture_output=True,
    )
    if proc.returncode != 0:
        report.errors.append(f"blob sizes: cat-file exited {proc.returncode}")
        return
    sizes = {}
    for ln in proc.stdout.decode().splitlines():
        p = ln.split()
        if len(p) == 2 and p[1].isdigit():
            sizes[p[0]] = int(p[1])
    want: list[tuple[str, str, str]] = []
    for blob, commit, path in blobs:
        size = sizes.get(blob, 0)
        if size > max_bytes:
            report.findings.append(Finding("large-blob", f"{path} ({size / 1048576:.1f} MiB)", commit, path, f"blob {blob}"))
        if 0 < size <= binary_cap:
            want.append((blob, commit, path))
    if not want:
        return
    # One `cat-file --batch` stream for every candidate blob. Text blobs were
    # already swept line by line from the diffs; here only binaries (a NUL in the
    # first 8000 bytes, git's own heuristic) and notebooks are opened, because
    # no line-diff scanner reads a SQLite file, an office document or an image.
    proc = subprocess.Popen(["git", "-C", str(repo), "cat-file", "--batch"],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    assert proc.stdin is not None and proc.stdout is not None
    swept_binaries = 0
    try:
        for blob, commit, path in want:
            proc.stdin.write(blob.encode() + b"\n"); proc.stdin.flush()
            header = proc.stdout.readline().split()
            if len(header) < 3 or header[1] != b"blob":
                continue
            content = proc.stdout.read(int(header[2])); proc.stdout.read(1)
            if path.endswith(".ipynb"):
                try:
                    nb = json.loads(content)
                    with_out = sum(1 for c in nb.get("cells", []) if c.get("outputs"))
                except (ValueError, AttributeError):
                    with_out = 0
                if with_out:
                    report.findings.append(Finding("notebook-output", f"{path} ({with_out} cells with output)", commit, path, ""))
                continue
            if b"\x00" not in content[:8000] or NOISE_BINARIES.search(path):
                continue
            swept_binaries += 1
            text, meta = binary_text(content, path)
            for n in meta:
                report.findings.append(Finding("file-metadata", f"{path}: {n}", commit, path, ""))
            for chunk in text.splitlines():
                scan_text(chunk, "binary", commit, f"{path} (binary strings)", deny, report.findings, ident_ok)
    finally:
        proc.stdin.close(); proc.wait()
    report.notes.append(f"{swept_binaries} binary blob(s) swept as extracted strings (cap {binary_cap // 1048576} MiB each)")


def ref_and_tag_findings(repo: Path, deny, report: Report, refs_scope: list[str]):
    out = git(repo, "for-each-ref",
              "--format=%(refname)%1f%(objecttype)%1f%(taggername) %(taggeremail)%1f%(contents)%1e",
              *refs_scope, check=False)
    if out.returncode != 0:
        report.errors.append("for-each-ref failed")
        return
    for rec in out.stdout.decode("utf-8", "replace").split("\x1e"):
        rec = rec.lstrip("\n")
        if not rec:
            continue
        parts = rec.split("\x1f")
        if len(parts) < 4:
            continue
        ref, otype, tagger, contents = parts[0], parts[1], parts[2].strip(), parts[3]
        scan_text(ref, "ref", "", f"(ref name) {ref}", deny, report.findings)
        if otype == "tag":
            if tagger and "<" in tagger:
                note_identity(report, "tagger", tagger, ref)
            scan_text(contents, "tag", "", f"(tag message) {ref}", deny, report.findings)


# --------------------------------------------------------------------------
def main() -> int:
    ap = argparse.ArgumentParser(description="Sweep git history for non-secret publication leaks.")
    ap.add_argument("--repo", default=".", help="repository to sweep (a mirror clone is best)")
    scope = ap.add_mutually_exclusive_group()
    scope.add_argument("--all-refs", action="store_true", help="every ref, including refs/pull/* in a mirror (default)")
    scope.add_argument("--range", help="a revision range, e.g. origin/main..HEAD")
    scope.add_argument("--commits", help="file of commit SHAs, one per line (exactly these commits)")
    ap.add_argument("--denylist", action="append", default=[], help="private regex list; never inside the repo")
    ap.add_argument("--allow", help="TOML allow file: [[allow]] category, path, match, reason")
    ap.add_argument("--identities", help="file of regexes for identities that may be published")
    ap.add_argument("--max-blob-mb", type=float, default=5.0, help="report blobs larger than this")
    ap.add_argument("--binary-cap-mb", type=float, default=20.0, help="open binary blobs up to this size (0 disables)")
    ap.add_argument("--json", help="write the full report here (keep it OUTSIDE the repo)")
    ap.add_argument("--quiet", action="store_true", help="summary only")
    a = ap.parse_args()

    repo = Path(a.repo).resolve()
    if git(repo, "rev-parse", "--git-dir", check=False).returncode != 0:
        sys.stderr.write(f"sweep-leaks: {repo} is not a git repository\n")
        return 3
    if a.json:
        jp = Path(a.json).expanduser().resolve()
        top = git(repo, "rev-parse", "--show-toplevel", check=False).stdout.decode().strip()
        if top and (Path(top) == jp or Path(top) in jp.parents):
            sys.stderr.write("sweep-leaks: refusing to write the report inside the repository it describes\n")
            return 3

    report = Report()
    deny = load_denylist(a.denylist, repo, report)
    allows = load_allow(a.allow)
    ident_ok = load_identity_patterns(a.identities)

    if a.range:
        rev_args, refs_scope = [a.range], []
    elif a.commits:
        shas = [s.strip() for s in Path(a.commits).read_text().split() if s.strip()]
        if not shas:
            print("sweep-leaks: no commits to sweep")
            return 0
        rev_args, refs_scope = ["--no-walk=unsorted", *shas], []
    else:
        rev_args, refs_scope = ["--all"], []
        ref_and_tag_findings(repo, deny, report, refs_scope)

    walk_log(repo, rev_args, deny, report, merges_mode=None, ident_ok=ident_ok)
    if git_version(repo) >= (2, 36):
        walk_log(repo, rev_args, deny, report, merges_mode="--remerge-diff", ident_ok=ident_ok)
    else:
        report.notes.append("git < 2.36: merge resolutions swept with --cc (less precise than --remerge-diff)")
        walk_log(repo, rev_args, deny, report, merges_mode="--cc", ident_ok=ident_ok)
    blob_findings(repo, rev_args, int(a.max_blob_mb * 1048576), report, deny, ident_ok,
                  int(a.binary_cap_mb * 1048576))

    for email, ent in report.identities.items():
        if not any(p.search(email) for p in ident_ok):
            report.findings.append(
                Finding("identity", ent["email"], ent["first"], "(git metadata)",
                        f"{', '.join(sorted(ent['names'])) or '?'} as {'/'.join(sorted(ent['roles']))} on {ent['commits']} record(s)")
            )

    # de-duplicate on (category, detail, path); keep the first commit seen
    uniq: dict[tuple, Finding] = {}
    counts: dict[tuple, int] = defaultdict(int)
    allowed = 0
    for f in report.findings:
        if any(al.covers(f) for al in allows):
            allowed += 1
            continue
        counts[f.key()] += 1
        uniq.setdefault(f.key(), f)
    findings = list(uniq.values())

    by_cat: dict[str, list[Finding]] = defaultdict(list)
    for f in findings:
        by_cat[f.category].append(f)

    print(f"sweep-leaks: {report.commits_scanned} commits swept in {repo}")
    for n in report.notes:
        print(f"  NOTE  {n}")
    if allowed:
        print(f"  {allowed} occurrence(s) suppressed by the allow file")
    for cat in sorted(by_cat):
        items = sorted(by_cat[cat], key=lambda f: (f.path, f.detail))
        print(f"\n[{cat}] {len(items)} distinct")
        if a.quiet:
            continue
        for f in items[:200]:
            n = counts[f.key()]
            ref = f.commit[:10] if re.fullmatch(r"[0-9a-f]{40,64}", f.commit or "") else f.commit
            where = f"{f.path}" + (f" @ {ref}" if ref else "")
            print(f"  {f.detail}  —  {where}" + (f"  (x{n})" if n > 1 else ""))
        if len(items) > 200:
            print(f"  … {len(items) - 200} more in the JSON report")
    for e in report.errors:
        print(f"\nINCOMPLETE: {e}")

    if a.json:
        Path(a.json).expanduser().write_text(json.dumps({
            "repo": str(repo),
            "commits_scanned": report.commits_scanned,
            "notes": report.notes,
            "errors": report.errors,
            "findings": [f.__dict__ | {"occurrences": counts[f.key()]} for f in findings],
            "identities": [{**v, "names": sorted(v["names"]), "roles": sorted(v["roles"])} for v in report.identities.values()],
        }, indent=2))

    if report.errors:
        return 2
    if findings:
        print(f"\n{len(findings)} distinct finding(s) to review. Each is either removed from history, "
              "accepted in the allow file with a reason, or a blocker.")
        return 1
    print("\nno findings")
    return 0


if __name__ == "__main__":
    sys.exit(main())
