#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""Check the encrypted synthetic fixtures against an independent decrypter.

The generator and the Rust crate were written from the same reading of the
same documents, so a shared misreading of the keybag, MBFile or Manifest
layout would pass CI. This manual check runs `iphone_backup_decrypt` 0.11.2
(MIT, used as a black-box oracle and never distributed) against every
encrypted fixture that is within the oracle's capabilities.

Usage:
  verify_with_oracle.py FIXTURES_DIR

FIXTURES_DIR is tests/fixtures/backups. Each fixture is copied to a
temporary directory first, so the committed files are never touched.
Prints one PASS/FAIL line per check, the SHA-256 of SHA256SUMS, and exits
non-zero on the first failure.

Fixtures deliberately left out (see tests/fixtures/backups/README.md):
- enc-key-uid-data, enc-key-inline: EncryptionKey shapes the oracle does
  not handle ("Rust-only, shape UNVERIFIED");
- enc-wpky39-unused, enc-dup-clas, hostile-wrap-device-only,
  hostile-keybag-truncated: keybag divergences (D18). For these and the two
  shapes above, the oracle's outcome is printed as an OBSERVED line rather
  than asserted;
- the classification fixtures (no keybag, no ManifestKey, plaintext
  Manifest.db, IsEncrypted of the wrong type), which never reach decryption.
"""
import argparse
import hashlib
import os
import shutil
import sqlite3
import sys
import tempfile

from iphone_backup_decrypt import EncryptedBackup, IncorrectPassphraseError
from iphone_backup_decrypt.exceptions import UnsafeBackupError

UDID = "SYNTHETIC0-0000000000000000"
DEFAULT_PASSWORD = "synthetic-test-password"
UNICODE_PASSWORD = "synthetic-pässwörd-ключ-\U0001F511"
SMS_REL = "Library/SMS/sms.db"
ATTACHMENT_REL = "Library/SMS/Attachments/00/00/SYNTH-0000/note.txt"

DECRYPTABLE = {
    "enc": DEFAULT_PASSWORD,
    "enc-xml": DEFAULT_PASSWORD,
    "enc-wal": DEFAULT_PASSWORD,
    "enc-unicode-pw": UNICODE_PASSWORD,
    "enc-slow": DEFAULT_PASSWORD,
    "enc-type2": DEFAULT_PASSWORD,
    "enc-clas-unused-0x10": DEFAULT_PASSWORD,
}

# Fixtures where the oracle and the Rust code are expected to disagree. The
# outcome is printed, not asserted, so a run records what the oracle did.
OBSERVE = (
    "enc-wpky39-unused",
    "enc-dup-clas",
    "hostile-wrap-device-only",
    "hostile-keybag-truncated",
    "enc-key-uid-data",
    "enc-key-inline",
)

failures = 0


def check(cond, msg):
    global failures
    print(("PASS " if cond else "FAIL ") + msg, flush=True)
    if not cond:
        failures += 1
        sys.exit(1)


def read(path):
    with open(path, "rb") as f:
        return f.read()


def staged_copy(fixtures, name, tmp):
    dst = os.path.join(tmp, name)
    shutil.copytree(os.path.join(fixtures, name, UDID), dst)
    return dst


def verify_decryptable(fixtures, name, password, tmp):
    backup = staged_copy(fixtures, name, tmp)
    out = os.path.join(tmp, name + ".out")
    os.makedirs(out)
    expected = os.path.join(fixtures, name + ".expected")

    b = EncryptedBackup(backup_directory=backup, passphrase=password)
    check(b.test_decryption() is True, f"{name}: test_decryption() with the right password")

    manifest = os.path.join(out, "Manifest.db")
    b.save_manifest_file(manifest)
    con = sqlite3.connect(f"file:{manifest}?mode=ro", uri=True)
    rows = con.execute("SELECT fileID, domain, relativePath FROM Files").fetchall()
    con.close()
    check(all(hashlib.sha1(f"{d}-{r}".encode()).hexdigest() == fid for fid, d, r in rows),
          f"{name}: every fileID == SHA1(domain-relativePath) ({len(rows)} rows)")
    if os.path.isdir(expected):
        check(read(manifest) == read(os.path.join(expected, "Manifest.db")),
              f"{name}: decrypted Manifest.db equals {name}.expected/Manifest.db")

    sms = os.path.join(out, "sms.db")
    b.extract_file(relative_path=SMS_REL, domain_like="HomeDomain", output_filename=sms)
    con = sqlite3.connect(f"file:{sms}?mode=ro", uri=True)
    note = con.execute("SELECT note FROM synthetic_marker").fetchone()[0]
    con.close()
    check(note.startswith("synthetic"), f"{name}: decrypted sms.db opens as SQLite")
    if os.path.isdir(expected):
        check(read(sms) == read(os.path.join(expected, "sms.db")),
              f"{name}: decrypted sms.db equals {name}.expected/sms.db")
        att = os.path.join(out, "attachment.txt")
        b.extract_file(relative_path=ATTACHMENT_REL, domain_like="MediaDomain", output_filename=att)
        check(read(att) == read(os.path.join(expected, "attachment.txt")),
              f"{name}: decrypted attachment equals {name}.expected/attachment.txt")
    del b


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("fixtures")
    a = ap.parse_args()
    fixtures = os.path.abspath(a.fixtures)

    with tempfile.TemporaryDirectory() as tmp:
        for name, password in DECRYPTABLE.items():
            verify_decryptable(fixtures, name, password, tmp)

        # A wrong password.
        backup = staged_copy(fixtures, "enc", os.path.join(tmp, "wrong"))
        try:
            EncryptedBackup(backup_directory=backup, passphrase="wrong-password").test_decryption()
            check(False, "enc: a wrong password is rejected")
        except IncorrectPassphraseError:
            check(True, "enc: a wrong password raises IncorrectPassphraseError")

        # Hostile iteration counts are refused before any derivation.
        for name in ("hostile-dpic", "hostile-iter"):
            backup = staged_copy(fixtures, name, tmp)
            try:
                EncryptedBackup(backup_directory=backup, passphrase=DEFAULT_PASSWORD).test_decryption()
                check(False, f"{name}: refused")
            except UnsafeBackupError:
                check(True, f"{name}: raises UnsafeBackupError")

        # Bad padding on Manifest.db: decryption must not succeed.
        backup = staged_copy(fixtures, "hostile-padding-manifest", tmp)
        try:
            ok = EncryptedBackup(backup_directory=backup, passphrase=DEFAULT_PASSWORD).test_decryption()
            check(ok is not True, "hostile-padding-manifest: test_decryption() does not succeed")
        except Exception as e:  # any failure is the expected outcome
            check(True, f"hostile-padding-manifest: fails ({type(e).__name__})")

        # Bad padding on sms.db: extraction must fail with a padding error.
        backup = staged_copy(fixtures, "hostile-padding-sms", tmp)
        b = EncryptedBackup(backup_directory=backup, passphrase=DEFAULT_PASSWORD)
        try:
            b.extract_file(relative_path=SMS_REL, domain_like="HomeDomain",
                           output_filename=os.path.join(tmp, "bad-sms.db"))
            check(False, "hostile-padding-sms: extraction fails")
        except ValueError as e:
            check("padding" in str(e).lower(), f"hostile-padding-sms: ValueError mentioning padding")
        del b

        for name in OBSERVE:
            backup = staged_copy(fixtures, name, tmp)
            try:
                b = EncryptedBackup(backup_directory=backup, passphrase=DEFAULT_PASSWORD)
                ok = b.test_decryption()
                outcome = f"test_decryption() = {ok}"
                try:
                    b.extract_file(relative_path=SMS_REL, domain_like="HomeDomain",
                                   output_filename=os.path.join(tmp, name + "-sms.db"))
                    outcome += "; sms.db extracted"
                except Exception as e:
                    outcome += f"; sms.db extraction raised {type(e).__name__}"
                del b
            except Exception as e:
                outcome = f"raised {type(e).__name__}"
            print(f"OBSERVED {name}: {outcome}", flush=True)

    sums = os.path.join(fixtures, "SHA256SUMS")
    print(f"SHA256SUMS sha256 {hashlib.sha256(read(sums)).hexdigest()}")
    print("ALL ORACLE CHECKS PASSED")


if __name__ == "__main__":
    main()
