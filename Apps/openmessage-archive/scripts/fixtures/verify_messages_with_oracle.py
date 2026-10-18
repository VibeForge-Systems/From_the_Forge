#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""Oracle run 1 for the messages catalogue (M3 spec section 20.4).

Checks every encrypted fixture in tests/fixtures/messages against an
independent decrypter, `iphone_backup_decrypt` 0.11.2 (MIT, used as a black
box and never distributed): Manifest.db, sms.db, sms.db-wal and every
attachment blob the generator wrote as decryptable must decrypt to the
fixture's `.expected` plaintext.

Exempt, by name (the oracle's outcome is printed as OBSERVED, not asserted):
in msgs-attachments-enc, the attachments whose key is missing
(SYNTH-FILE-0006), whose protection class is unavailable (SYNTH-FILE-0007),
whose key does not unwrap (SYNTH-FILE-0008) and whose blob is missing
(SYNTH-FILE-0002).

Usage:
  verify_messages_with_oracle.py FIXTURES_DIR

FIXTURES_DIR is tests/fixtures/messages. Each fixture is copied to a
temporary directory first, so the committed files are never touched. Prints
one PASS/FAIL line per check, the SHA-256 of SHA256SUMS, and exits non-zero
on the first failure.
"""
import argparse
import hashlib
import os
import shutil
import sqlite3
import sys
import tempfile

from iphone_backup_decrypt import EncryptedBackup

UDID = "SYNTHETIC0-0000000000000000"
PASSWORD = "synthetic-test-password"
EXEMPT = {"msgs-attachments-enc": ("SYNTH-FILE-0002", "SYNTH-FILE-0006", "SYNTH-FILE-0007",
                                   "SYNTH-FILE-0008")}


def check(cond, msg):
    print(("PASS " if cond else "FAIL ") + msg, flush=True)
    if not cond:
        sys.exit(1)


def read(path):
    with open(path, "rb") as f:
        return f.read()


def verify(fixtures, name, tmp):
    backup = os.path.join(tmp, name)
    shutil.copytree(os.path.join(fixtures, name, UDID), backup)
    expected = os.path.join(fixtures, name + ".expected")
    out = os.path.join(tmp, name + ".out")
    os.makedirs(out)

    b = EncryptedBackup(backup_directory=backup, passphrase=PASSWORD)
    check(b.test_decryption() is True, f"{name}: test_decryption()")
    manifest = os.path.join(out, "Manifest.db")
    b.save_manifest_file(manifest)
    check(read(manifest) == read(os.path.join(expected, "Manifest.db")),
          f"{name}: Manifest.db equals .expected/Manifest.db")
    con = sqlite3.connect(f"file:{manifest}?mode=ro", uri=True)
    rows = con.execute("SELECT fileID, domain, relativePath, flags FROM Files").fetchall()
    con.close()
    check(all(hashlib.sha1(f"{d}-{r}".encode()).hexdigest() == fid for fid, d, r, _ in rows),
          f"{name}: every fileID == SHA1(domain-relativePath) ({len(rows)} rows)")

    for rel, exp in (("Library/SMS/sms.db", "sms.db"), ("Library/SMS/sms.db-wal", "sms.db-wal")):
        want = os.path.join(expected, exp)
        if not any(r == rel for _, _, r, _ in rows):
            continue
        got = os.path.join(out, exp)
        b.extract_file(relative_path=rel, domain_like="HomeDomain", output_filename=got)
        check(read(got) == read(want), f"{name}: {exp} equals .expected/{exp}")

    att_dir = os.path.join(expected, "attachments")
    for fid, domain, rel, flags in rows:
        if domain != "MediaDomain" or flags != 1:
            continue
        got = os.path.join(out, fid)
        want = os.path.join(att_dir, fid)
        if any(x in rel for x in EXEMPT.get(name, ())):
            try:
                b.extract_file(relative_path=rel, domain_like="MediaDomain", output_filename=got)
                outcome = "extracted" + (" (equal to a plaintext)" if os.path.exists(want) else "")
            except Exception as e:  # the oracle's own outcome, recorded
                outcome = f"raised {type(e).__name__}"
            print(f"OBSERVED {name}: {os.path.basename(rel)}: {outcome}", flush=True)
            continue
        b.extract_file(relative_path=rel, domain_like="MediaDomain", output_filename=got)
        check(os.path.exists(want) and read(got) == read(want),
              f"{name}: {os.path.basename(rel)} equals .expected/attachments/{fid[:8]}…")
    del b


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("fixtures")
    a = ap.parse_args()
    fixtures = os.path.abspath(a.fixtures)
    names = sorted(n for n in os.listdir(fixtures)
                   if n.endswith("-enc") and os.path.isdir(os.path.join(fixtures, n)))
    check(len(names) >= 3, f"encrypted fixtures found: {', '.join(names)}")
    with tempfile.TemporaryDirectory() as tmp:
        for name in names:
            verify(fixtures, name, tmp)
    sums = os.path.join(fixtures, "SHA256SUMS")
    print(f"SHA256SUMS sha256 {hashlib.sha256(read(sums)).hexdigest()}")
    print("ALL ORACLE CHECKS PASSED")


if __name__ == "__main__":
    main()
