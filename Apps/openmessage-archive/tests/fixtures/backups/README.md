# Synthetic backup fixtures

Test input for `crates/oma-backup`. **Every value in these files is
invented.** No real device, person, message or backup was used, and none
may ever be added here.

- Device name "Synthetic Test Phone", product type `iPhone0,0`.
- UDID `SYNTHETIC0-0000000000000000`, which is also each backup's folder
  name.
- Password `synthetic-test-password` (and, for `enc-unicode-pw`,
  `synthetic-pässwörd-ключ-🔑`).
- The `sms.db` stand-in has one invented table, `synthetic_marker`, holding
  the string "synthetic fixture, not real data". There is no message-like
  content.

## Layout

```text
<name>/SYNTHETIC0-0000000000000000/   one backup; <name>/ plays the part of MobileSync/Backup
<name>.expected/                      plaintexts: Manifest.db, sms.db, attachment.txt
SHA256SUMS                            every file except this one and README.md
```

`.gitattributes` marks `tests/fixtures/**` as binary, so Git for Windows
never converts line endings. The `fixtures_integrity` test checks every file
against `SHA256SUMS` and fails on any file missing from it.

## Regenerating

Only with the generator, and always together with `SHA256SUMS`, which it
rewrites. Changing any fixture invalidates the recorded oracle run below
until it is repeated.

Linux:

```sh
python3 -m venv .venv-fixtures
.venv-fixtures/bin/pip install -r scripts/fixtures/requirements.txt
.venv-fixtures/bin/python -I scripts/fixtures/make_synthetic_backups.py tests/fixtures/backups --seed 1
.venv-fixtures/bin/pip install -r scripts/fixtures/requirements-oracle.txt   # optional oracle
.venv-fixtures/bin/python -I scripts/fixtures/verify_with_oracle.py tests/fixtures/backups
sha256sum tests/fixtures/backups/SHA256SUMS                         # record with the oracle run
```

Windows (PowerShell): `py -3 -m venv .venv-fixtures`, then
`.venv-fixtures\Scripts\pip …` and `.venv-fixtures\Scripts\python -I …`
with the same arguments, and `Get-FileHash tests\fixtures\backups\SHA256SUMS`.

For a given `--seed` and the same Python `sqlite3` build the output is
byte-identical (WAL salts are normalised by the generator). Across machines
it is not claimed to be, because SQLite file bytes depend on the SQLite
version. The committed files were generated with:

| | |
|---|---|
| Python | 3.13.16 |
| SQLite (Python `sqlite3`) | 3.45.1 |
| cryptography | 50.0.2 |
| Seed | 1 |
| `SHA256SUMS` SHA-256 | `c8e3d0bfba700b10653521c49bfcb1fc30dcc1477a762541a17cab17b04a0ae3` |

## The fixtures

Unless noted, a fixture is encrypted with DPIC 1,000 and ITER 10 (small so
tests are fast) and holds six `Files` rows: three directories (one a domain
root with an empty `relativePath`), sms.db, one attachment and one symlink.

| Fixture | What is special | Exercised by |
|---|---|---|
| `plain` | Unencrypted | `unencrypted::validates`, `extract::sms_db` |
| `plain-xml` | XML Manifest.plist, Status.plist **and MBFile records** | `unencrypted::xml_plists` |
| `plain-wal` | Manifest.db in WAL mode; two rows live only in `Manifest.db-wal`; no `-shm` | `unencrypted::wal_manifest` |
| `plain-no-keybag` | Unencrypted without `BackupKeyBag` | `classify` |
| `enc` | Encrypted | `encrypted::validates`, `extract::sms_db` |
| `enc-xml` | XML Manifest.plist and Status.plist (binary MBFile records) | `encrypted::xml` |
| `enc-wal` | WAL-mode Manifest.db with an encrypted `-wal` (never applied) | `encrypted::wal_unapplied` |
| `enc-unicode-pw` | Non-ASCII password | `encrypted::unicode_password` |
| `enc-slow` | DPIC 2,000,000 | `encrypted::cancel_during_kdf` |
| `enc-type2` | Keybag TYPE 2 | `encrypted::keybag_divergences` |
| `enc-clas-unused-0x10` | An extra, unused class 0x10 | `encrypted::keybag_divergences` |
| `enc-wpky39-unused` | An extra, unused class 12 with a 39-byte WPKY | `encrypted::keybag_divergences` |
| `enc-dup-clas` | Two class records with CLAS 3 | `encrypted::duplicate_clas` |
| `enc-key-uid-data` | EncryptionKey as a UID to raw data | `encrypted::encryption_key_shapes` |
| `enc-key-inline` | EncryptionKey as inline data | `encrypted::encryption_key_shapes` |
| `incomplete` | SnapshotState `synthetic-uploading` (a sentinel) | `classify::incomplete` |
| `incomplete-no-status` | No Status.plist | `classify` |
| `snapshot-dir` | Unfinished, with a `Snapshot/` folder | `classify::snapshot_dir` |
| `legacy-mbdb` | Format 2.4 stub: `Manifest.mbdb` signature only | `classify::legacy_mbdb` |
| `enc-no-manifestkey` | Encrypted without ManifestKey | `classify` |
| `enc-no-keybag` | Encrypted without BackupKeyBag | `classify` |
| `wrong-type-isencrypted` | `IsEncrypted` is the string `"true"` | `classify` |
| `encrypted-plaintext-db` | `IsEncrypted` true, Manifest.db plaintext | `classify` |
| `bomb-info`, `bomb-status`, `bomb-manifest` | A binary-plist reference bomb (25 levels of `[x, x]`) in that plist | `hostile::plist_bombs` |
| `bomb-mbfile` | An extra row whose MBFile record is a reference bomb | `hostile::plist_bombs` |
| `hostile-paths` | relativePaths `../../escape`, `/abs`, `C:\x`, `a\..\..\b`, `What?.jpg`; fileIDs `../../xx` and an upper-case one | `hostile::lookup_keys` |
| `hostile-fileid` | A row whose fileID hashes a different path | `hostile::fileid_mismatch` |
| `hostile-dpic` | Keybag declares DPIC 0xFFFFFFFF | `hostile::huge_dpic_and_iter` |
| `hostile-iter` | Keybag declares ITER 1,000,001 | `hostile::huge_dpic_and_iter` |
| `hostile-keybag-truncated` | Keybag cut 5 bytes short (a record runs past the end) | `hostile::truncated_keybag` |
| `hostile-wrap-device-only` | Every class has WRAP 1 | `hostile::no_passphrase_class` |
| `hostile-padding-manifest` | Last byte of the encrypted Manifest.db changed | `hostile::bad_padding_manifest` |
| `hostile-padding-sms` | Last byte of the encrypted sms.db blob changed | `hostile::bad_padding_sms` |
| `hostile-size-max` | The attachment's `Size` is 2^64 − 1 | `hostile::size_max` |
| `hostile-mbfile-oversize` | The attachment's MBFile record is over 100 KiB | `hostile::oversize_mbfile` |
| `missing-sms-blob` | sms.db listed, its blob absent | `missing::sms_blob` |

`hostile-dpic` and `hostile-iter` only *declare* their counts: their keys
are wrapped with the default counts, because the fixtures exist to be
refused before any derivation.

### Rust-only, shape UNVERIFIED

These shapes come from the format description, not from a real backup, and
the oracle cannot read them. Passing them shows only that the Rust code
reads what the generator writes.

- `enc-key-uid-data` and `enc-key-inline`: the two EncryptionKey shapes the
  oracle does not handle.
- `plain-xml`'s XML MBFile records (`CF$UID` dictionaries). Whether MBFile
  records are ever XML is UNVERIFIED.

## Oracle run

`scripts/fixtures/verify_with_oracle.py` runs `iphone_backup_decrypt`
0.11.2 (MIT) as a black box against every encrypted fixture within its
capabilities.

| | |
|---|---|
| Date | 2026-10-18 |
| Versions | iphone_backup_decrypt 0.11.2 with pycryptodome 4.0.1 (its AES; `cryptography` 50.0.2 is used only by the generator), Python 3.13.16 |
| `SHA256SUMS` SHA-256 checked | `c8e3d0bfba700b10653521c49bfcb1fc30dcc1477a762541a17cab17b04a0ae3` |
| Result | ALL ORACLE CHECKS PASSED |

Asserted and passed: `test_decryption()` with the right password for `enc`,
`enc-xml`, `enc-wal`, `enc-unicode-pw`, `enc-slow`, `enc-type2` and
`enc-clas-unused-0x10`; every decrypted fileID equals
SHA1(domain-relativePath); the decrypted Manifest.db, sms.db and attachment
equal the `.expected` copies for `enc`, `enc-xml` and `enc-unicode-pw`; a
wrong password raises `IncorrectPassphraseError`; `hostile-dpic` and
`hostile-iter` raise `UnsafeBackupError`; `hostile-padding-manifest` fails;
`hostile-padding-sms` fails with a padding `ValueError`.

Observed, not asserted (where the oracle and the Rust code differ):

| Fixture | Oracle 0.11.2 | Rust |
|---|---|---|
| `enc-wpky39-unused` | `IncorrectPassphraseError` | Unlocks, with a keybag note (D18) |
| `enc-dup-clas` | `test_decryption()` true; sms.db extraction raises `ValueError` | `Malformed { Keybag, DuplicateClass(3) }` |
| `hostile-wrap-device-only` | raises `ValueError` | `Unsupported(NoPassphraseClasses)` |
| `hostile-keybag-truncated` | `IncorrectPassphraseError` | `Malformed { Keybag, Truncated }` |
| `enc-key-uid-data` | `test_decryption()` true; sms.db extraction raises `TypeError` | Decrypts |
| `enc-key-inline` | `test_decryption()` true; sms.db extraction raises `AttributeError` | Decrypts |

## Generator review (clean-room record)

The generator was ported from the research script that accompanied the
milestone 2 design, which states that it was written from the format
description and copies no GPL or CC BY-SA code.

**Review: PENDING.** The recorded check compares
`scripts/fixtures/make_synthetic_backups.py` against the downloaded
reference sources kept in the research scratch directory and confirms that
no code was copied. The rules forbid the implementer of the Rust code from
opening those sources, so the review must be done by someone else. Record
the reviewer and the date here when it is done.
