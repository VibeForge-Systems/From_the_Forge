# Synthetic messages catalogue (M3)

Test input for `crates/oma-messages` (`tests/profiles.rs` and the suites
named below). **Every value in these files is invented**: addresses under
`example.invalid`, `+1555…` numbers, and sentinel text (`SYNTH-BODY-…`,
`SYNTH-EDIT-…`, `SYNTH-DELETED-…`, `SYNTH-CHAT-NAME`, `SYNTH-GROUP-TITLE`,
`SYNTH-FILE-….heic`, `SYNTH-GENMOJI-PROMPT`, `SYNTH-MENTION`,
`SYNTH-SUBJECT`, `SYNTH_SENTINEL_…` object names). No real device, person,
message or backup was used, and nothing from any sample database (S3, S12) is
copied, DDL text included. The column sets are hand-written from column-name
facts (`scripts/fixtures/sms_profiles.py`); no profile is an iOS version.

## Layout

```text
<name>/SYNTHETIC0-0000000000000000/   one backup (plain or encrypted)
<name>.expected/sms.db                the plaintext sms.db (main file)
<name>.expected/sms.db-wal            the plaintext -wal, for WAL fixtures
<name>.expected/Manifest.db           the plaintext Manifest.db (encrypted fixtures)
<name>.expected/attachments/<fileID>  attachment plaintexts (encrypted fixtures)
<name>.expected/report.json           the generator's model of check()'s report
SHA256SUMS                            every file except this one and README.md
```

`report.json` is computed by the generator from the rows it wrote, by a
second statement of the rules in M3 sections 5-13. `tests/profiles.rs`
compares `check()`'s report with it through a projection. **Agreement proves
self-consistency only**; external agreement comes from the oracle runs in
`docs/compatibility.md`.

Encrypted fixtures use the password `synthetic-test-password`, with DPIC 1,000 and ITER 10.
SQLite pages are 1,024 bytes, to keep every entry under 300 KB.

## Regenerating (reference environment only)

```sh
python3 -m venv .venv-fixtures
.venv-fixtures/bin/pip install -r scripts/fixtures/requirements.txt
.venv-fixtures/bin/python -I -c "import sqlite3; print(sqlite3.sqlite_version)"   # must print 3.45.1
.venv-fixtures/bin/python -I scripts/fixtures/make_synthetic_messages.py tests/fixtures/messages --seed 1
sha256sum tests/fixtures/messages/SHA256SUMS
```

The script refuses to write under any other SQLite unless
`--allow-other-sqlite` is passed: SQLite file bytes depend on the version.
Windows is not a supported generation environment. CI runs no Python; the
`fixtures_integrity` test checks every file against `SHA256SUMS`.

| | |
|---|---|
| Python | 3.13.16 |
| SQLite (Python `sqlite3`) | 3.45.1 |
| cryptography | 50.0.1 |
| Seed | 1 |

## Oracles (manual; nothing enters the repository or CI)

Numbered as in the M3 spec (run 2 is the typedstream vectors' oracle).

- **Oracle 1, `iphone_backup_decrypt` 0.11.2** (MIT), through
   `scripts/fixtures/verify_messages_with_oracle.py`: every encrypted
   fixture's sms.db, `-wal`, Manifest.db and present attachment blobs
   decrypt to `.expected`. **Exempt, by name:** in `msgs-attachments-enc`,
   the attachments whose key is missing (`SYNTH-FILE-0006`), whose class is
   unavailable (`SYNTH-FILE-0007`), whose key does not unwrap
   (`SYNTH-FILE-0008`) and whose blob is missing (`SYNTH-FILE-0002`).
- **Oracle 3, imessage-exporter 4.3.0** (GPL-3.0; release build in scratch, black
   box): for each readable plaintext `.expected/sms.db` (WAL fixtures with
   their `-wal` beside them), the message count, and for each row with only
   an `attributedBody`, the recovered text, equal the generator's model.
   Steps: copy `.expected/` to a scratch directory, run
   `imessage-exporter -a macOS -p <copy>/sms.db -d` (the counts) and
   `-f txt` / `-f html -o <out>` (the text and the mention markup), and
   compare. No GPL tooling or wrapper is committed.
- **Oracle 4, our check versus imessage-exporter on third-party samples** (S3, S12;
   macOS chat.db), wrapped in a scratch-only unencrypted backup and checked
   with `OMA_EXTERNAL_BACKUP=<dir> cargo test -p oma-messages --test
   external_sample -- --ignored --nocapture`: status, schema facts and
   message, chat and handle counts. Aggregate counts only.

Each run is recorded in `docs/compatibility.md`, keyed to the SHA-256 of
`SHA256SUMS`.

## The fixtures

| Fixture | What it covers |
|---|---|
| `msgs-full-plain` | Every known column (and one unknown): bodies (text, attributedBody, both, empty text), tapbacks and removals, emoji tapback, sticker, poll vote, unparsed and missing targets, replies (one to a missing GUID), `reply_to_guid` alone, group actions, shared location, edits, unsent parts, `tmp`, scheduled, mentions, link key, audio transcription, Recently Deleted, orphans on every join, a message in two chats, attachments (found, sticker cache, macOS path, NULL) |
| `msgs-full-enc` | `msgs-full-plain`, encrypted |
| `msgs-cols-a-seconds-plain` | Snapshot-A column set; dates in seconds; no `chat_message_join.message_date` |
| `msgs-cols-threads-edits-plain` | Adds threads, `date_edited`, `part_count`, `message_summary_info` and the Recently Deleted tables |
| `msgs-cols-emoji-schedule-plain` | Adds `associated_message_emoji`, `schedule_*` and the Genmoji column |
| `msgs-cols-filter-plain` | Adds `filter_action`, `filter_sub_action` and `chat.is_pending_review` |
| `msgs-mixed-units-plain` | `message.date` in both seconds and nanoseconds |
| `msgs-scrambled-plain` | Columns in a seeded random order, plus unknown columns |
| `msgs-minimal-plain` | Required tables and columns only: every optional counter is "not recorded" |
| `msgs-resume-plain` | Rowid gaps and rows keyed `i64::MIN` and `i64::MAX`, in rowid and join tables |
| `msgs-attachments-enc` | Every attachment outcome: found, blob missing, not listed at the sourced key, sticker-cache miss, NoKey, ClassKeyUnavailable, KeyDoesNotUnwrap, a directory, NULL, each class not looked up, unrecognized, unusual and too long |
| `msgs-wal-plain` | Two messages and a link only in `sms.db-wal` |
| `msgs-wal-enc` | `msgs-wal-plain`, encrypted |
| `msgs-wal-missing-plain` | `sms.db-wal` listed, blob missing: `WalSkipped`; the main file's rows only |
| `msgs-wal-badframes-plain` | A second `-wal` transaction with a corrupted frame: `WalFramesInvalid`; the first transaction's rows only |
| `msgs-hostile-plain` | Type mismatches, invalid UTF-8, oversized values, hostile filenames, plist errors and a depth bomb, single-fault typedstream blobs, views, a trigger, an index, and sentinel object names |
| `msgs-corrupt-plain` | A damaged b-tree page: `Damaged(IntegrityProblems)` |
| `msgs-unsupported-plain` | `message` is a view: `Unsupported([NotOrdinaryTable(Message)])` |
| `msgs-vtab-plain` | An FTS5 table: `Unsupported([VirtualTablePresent])` |
| `msgs-rowid-desc-plain` | `ROWID INTEGER PRIMARY KEY DESC`: `Unsupported([RowidShape(Message)])` |
