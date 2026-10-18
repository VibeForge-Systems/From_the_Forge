#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""Cross-check the synthetic typedstream vectors against an independent
decoder (M3 spec section 20.4, oracle 2).

The oracle is pytypedstream 0.1.0 (LGPL-3.0-or-later). It is run only as a
black box: this script starts its `pytypedstream decode` command as a
subprocess and reads the text it prints. It never imports the package, and
nothing from it is copied into this repository. Install it from the pinned
requirements file (wheel hash included):

  python3 -m venv .venv-oracle-ts
  .venv-oracle-ts/bin/pip install --require-hashes -r scripts/fixtures/requirements-oracle-typedstream.txt
  .venv-oracle-ts/bin/python -I scripts/fixtures/verify_typedstream_with_oracle.py tests/fixtures/typedstream

For each vector in expected.json, `oracle.check` decides what happens:

  compare    the oracle must decode the vector, and the structure it prints
             (normalised by this script) must equal `oracle.view`;
  must_fail  the oracle must reject the vector (non-zero exit);
  exempt     skipped; the reason is printed.

The oracle's error output can include a Python traceback. Only its last line
is printed, and only for a failed check.

Exit status 0 means every check passed ("ALL ORACLE CHECKS PASSED").
"""
import argparse
import ast
import hashlib
import json
import os
import subprocess
import sys

ORACLE_VERSION = "0.1.0"


def default_oracle():
    return os.path.join(os.path.dirname(sys.executable), "pytypedstream")


def lit(s):
    return ast.literal_eval(s)


def inner(s, prefix):
    """`Name('x')` -> the literal inside the parentheses."""
    assert s.startswith(prefix + "(") and s.endswith(")"), s
    return lit(s[len(prefix) + 1:-1])


def scalar(s):
    if s in ("True", "False"):
        return s == "True"
    v = lit(s)
    if not isinstance(v, (int, float)):
        raise ValueError(s)
    return v


def value_view(s):
    """A dictionary value as printed after `key: `."""
    for name in ("NSMutableString", "NSString"):
        if s.startswith(name + "("):
            return {"str": inner(s, name)}
    if s.startswith("NSNumber, type b'"):
        return {"int": scalar(s.split(": ", 1)[1])}
    if s.startswith("object of class "):
        return {"class": s[len("object of class "):].split(" ", 1)[0]}
    raise ValueError("unrecognised value: " + s[:60])


def depth(line):
    return len(line) - len(line.lstrip("\t"))


def view_attributed(lines):
    assert lines[0].startswith("type b'@': object of class "), lines[0][:60]
    text = None
    runs = []
    dicts = []
    i = 1
    while i < len(lines):
        line = lines[i]
        d = depth(line)
        s = line.strip()
        if d != 1:
            raise ValueError("unexpected indentation")
        if s.startswith("super object:"):
            i += 1
        elif s == "group:":
            a = lines[i + 1].strip()
            b = lines[i + 2].strip()
            assert a.startswith("type b'i': ") and b.startswith("type b'I': "), (a[:30], b[:30])
            runs.append([scalar(a.split(": ", 1)[1]), scalar(b.split(": ", 1)[1])])
            i += 3
        elif s.startswith("type b'@': NSDictionary"):
            i += 1
            entries = []
            while i < len(lines) and depth(lines[i]) >= 2:
                if depth(lines[i]) == 2:
                    k, v = lines[i].strip().split(": ", 1)
                    entries.append([inner(k, "NSString"), value_view(v)])
                i += 1
            dicts.append(entries)
        elif s == "type b'@': None":
            dicts.append(None)
            i += 1
        elif s.startswith("type b'@': ") and text is None:
            v = s[len("type b'@': "):]
            text = inner(v, v.split("(", 1)[0])
            i += 1
        else:
            raise ValueError("unrecognised line: " + s[:60])
    return {"kind": "attributed", "text": text, "runs": runs, "dicts": dicts}


def view(kind, out):
    lines = [l for l in out.splitlines() if l.strip()]
    if kind == "attributed":
        return view_attributed(lines)
    if kind == "scalar":
        assert len(lines) == 1
        t, v = lines[0].split(": ", 1)
        return {"kind": "scalar", "type": lit(t[len("type "):]).decode(), "value": scalar(v)}
    if kind == "strings":
        strings = []
        for l in lines:
            assert l.startswith("type b'@': NSString("), l[:40]
            strings.append(inner(l[len("type b'@': "):], "NSString"))
        return {"kind": "strings", "strings": strings}
    if kind == "data":
        assert len(lines) == 1
        return {"kind": "data", "hex": inner(lines[0][len("type b'@': "):], "NSData").hex()}
    if kind == "bytes":
        assert len(lines) == 1
        t, rest = lines[0].split(": ", 1)
        return {"kind": "bytes", "type": lit(t[len("type "):]).decode(),
                "hex": lit(rest.split(": ", 1)[1]).hex()}
    if kind == "struct":
        t = lit(lines[0].split(": ", 1)[0][len("type "):]).decode()
        return {"kind": "struct", "type": t, "values": [scalar(l.strip()) for l in lines[1:]]}
    raise ValueError(kind)


def main():
    ap = argparse.ArgumentParser(description="Cross-check typedstream vectors with pytypedstream.")
    ap.add_argument("fixtures")
    ap.add_argument("--oracle", default=default_oracle(), help="path of the pytypedstream command")
    args = ap.parse_args()

    ver = subprocess.run([args.oracle, "--version"], capture_output=True, text=True)
    version = (ver.stdout + ver.stderr).strip()
    if ORACLE_VERSION not in version:
        sys.exit(f"expected pytypedstream {ORACLE_VERSION}, got: {version[:80]}")
    with open(os.path.join(args.fixtures, "SHA256SUMS"), "rb") as f:
        sums = f.read()
    print(f"oracle: {version}")
    print(f"SHA-256 of SHA256SUMS: {hashlib.sha256(sums).hexdigest()}")
    with open(os.path.join(args.fixtures, "expected.json"), encoding="utf-8") as f:
        doc = json.load(f)

    counts = {"compare": 0, "must_fail": 0, "exempt": 0}
    failures = []
    for v in doc["vectors"]:
        path = os.path.join(args.fixtures, v["file"])
        with open(path, "rb") as f:
            if hashlib.sha256(f.read()).hexdigest() != v["sha256"]:
                failures.append(f"{v['name']}: file does not match expected.json")
                continue
        check = v["oracle"]["check"]
        counts[check] += 1
        if check == "exempt":
            print(f"  exempt     {v['name']}: {v['oracle']['reason']}")
            continue
        r = subprocess.run([args.oracle, "decode", path], capture_output=True)
        last = (r.stderr.decode("utf-8", "replace").strip().splitlines() or [""])[-1]
        if check == "must_fail":
            if r.returncode == 0:
                failures.append(f"{v['name']}: the oracle accepted it")
            continue
        if r.returncode != 0:
            failures.append(f"{v['name']}: the oracle failed: {last[:120]}")
            continue
        want = v["oracle"]["view"]
        try:
            got = view(want["kind"], r.stdout.decode("utf-8"))
        except (ValueError, AssertionError, IndexError, SyntaxError) as e:
            failures.append(f"{v['name']}: could not read the oracle's output ({type(e).__name__})")
            continue
        if json.loads(json.dumps(got)) != want:
            failures.append(f"{v['name']}: structure differs")
    print(f"compared {counts['compare']}, must fail {counts['must_fail']}, exempt {counts['exempt']}")
    if failures:
        for f in failures:
            print("FAIL", f)
        sys.exit(1)
    print("ALL ORACLE CHECKS PASSED")


if __name__ == "__main__":
    main()
