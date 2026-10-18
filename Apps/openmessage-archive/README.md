# OpenMessage Archive

A local-first, open-source desktop application for archiving iPhone
Messages (iMessage, SMS and RCS) from local iPhone backups. You can browse,
search and export your history on your own computer. There are no cloud
services, accounts, subscriptions or telemetry.

Targets **Windows 11** and **Linux**. Built with Tauri 2, Rust, Vue 3,
TypeScript, Bootstrap 5 and SQLite. Licensed **Apache-2.0**.

> **Status: Milestone 4 (the archive database and import) is implemented
> and awaiting sign-off.** The app finds iPhone backups, checks them, and
> **imports their messages and attachment files into a local archive**: a
> versioned SQLite database with a full-text index, and a folder of
> attachment files. Imports are transactional and can be cancelled and
> continued. The app shows **counts and status only**; reading your
> messages in the app (the conversation viewer and search) is milestone 5.
> **Archives are stored unencrypted** (archive encryption is milestone 8).
> Milestones 2 to 4 have been tested only on synthetic backups (and, for
> the extraction check, two third-party macOS samples), never on a backup
> made from a real iPhone. Every unfinished capability appears in the app
> disabled and marked "Planned · M*n*". See [the roadmap](#roadmap).

## What works today

- Three-panel desktop layout. Side panels collapse, and on narrow windows
  they become overlays.
- Light, dark and system themes, keyboard shortcuts (<kbd>F1</kbd> lists
  them), and an accessible native dialog.
- Choosing the archive location through the native folder picker, with
  validation checks:
  - The folder must exist and be writable (tested with a real probe file).
  - It cannot be a drive root.
  - It is **refused if it is inside an iPhone backup**, the MobileSync
    store, or iMazing's default backup store (`iMazing\Backups`).
  - You get a **warning if it is in a cloud-synced folder** such as OneDrive
    or Dropbox.
- Settings are saved atomically. A damaged settings file is set aside, not
  overwritten. A settings file from a newer version is never modified.
- **Backups page** (milestone 2):
  - Lists backups in the standard Windows locations (Apple Devices, desktop
    iTunes, and iMazing's default `iMazing\Backups`) and in folders you add
    with **Add backup folder…** (Linux has no standard location). Each
    backup shows its device name, model, iOS version, last backup date and
    state: ready, encrypted, incomplete, unsupported or unreadable, with the
    reason.
  - **Validate** (or **Unlock and validate…** for an encrypted backup) runs
    a cancellable check with progress. It copies the backup's file index
    (decrypted, if needed) into your archive folder, checks it and the
    files it lists, checks the Messages database where it is, and deletes
    the copy. The backup is never written to.
  - Backup passwords are used once and never saved.
- **Extraction check** (milestone 3): **Run extraction check** (or
  **Unlock and run extraction check…**) copies the Messages database out of
  the backup into your archive folder (decrypted, for an encrypted backup),
  reads every row, and deletes the copy straight after reading. It reports:
  - an outcome: readable, readable with stated limits (for example "This
    iOS version has not been tested"), damaged, or a layout this version
    does not support, with the reason;
  - counts named after the rows they count (messages, chats, addresses,
    attachments, Recently Deleted entries), the date range of your
    messages, what was found for each attachment reference, and the
    problems found, by kind.

  It shows no message text, address, name or chat title, saves nothing,
  and copies no attachment. A count the database does not record is shown
  as "not recorded", never as 0.
- **Archive page** (milestone 4; <kbd>Ctrl</kbd>+<kbd>1</kbd>):
  - **Create archive…** makes an archive in your archive folder, with an
    optional name. You can keep several archives.
  - **Import from a backup…** (or **Import messages…** on the Backups
    page) copies the backup's Messages database into staging (decrypted,
    for an encrypted backup), reads it, and stops at a **confirmation
    step** before anything is written. It shows the space needed, how many
    messages are new or already archived, the Recently Deleted counts, how
    the backup's date compares with earlier imports, the notes that apply
    (such as "This iOS version has not been tested"), and the privacy
    notes. You choose which earlier import set (phone) the backup belongs
    to, or a new one; the app says whether the model and the name match,
    never proving identity. Without an answer within two minutes the import
    is not started, and nothing is written.
  - The import then copies messages, chats, addresses, the links between
    them, and attachment files (stored once each, named by their SHA-256)
    into the archive, with progress. **Cancel** keeps what was committed;
    **Continue importing…** picks up from there with the same backup and
    the same options (the dialog starts from them; changing them starts
    over, and the confirmation step says so).
    Importing the same backup again adds nothing; an older backup imported
    later never overwrites newer values, and earlier versions of edited
    text are kept.
  - **Recently Deleted messages are left out by default**, with an opt-in
    for each import.
  - A summary of each import (counts by kind, problems by kind) and the
    archive's totals and import history.
  - **Quick verify** and **Full verify** check the database, the search
    index and every attachment file (a full verify re-hashes each one), and
    report problems by kind. They repair nothing.
- Strict security posture:
  - The webview can call only the app's own allow-listed commands.
  - The Content Security Policy allows no network connections.
  - The webview cannot choose filesystem paths.

## Quick start

Install the prerequisites listed in [docs/development.md](docs/development.md).

**Linux (bash)**

```sh
cd Apps/openmessage-archive
npm ci
npm run tauri dev          # run with hot reload
npm run tauri build        # .deb, .rpm and AppImage in target/release/bundle/
```

**Windows 11 (PowerShell)**

```powershell
cd Apps\openmessage-archive
npm ci
npm run tauri dev
npm run tauri build        # NSIS .exe and .msi in target\release\bundle\
```

**Tests and checks** (the same commands on both platforms)

```sh
npm test && npm run typecheck && npm run licenses
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Documentation

| Document | Contents |
|---|---|
| [architecture.md](docs/architecture.md) | Invariants, components, IPC boundary, key decisions |
| [supported-formats.md](docs/supported-formats.md) | Research on backup formats, encryption, the sms.db schema and device tooling, with sources |
| [compatibility.md](docs/compatibility.md) | What has actually been tested, by OS, iOS version and backup type, including the synthetic catalogues, oracle runs, synthetic performance, headless smoke tests and the manual checklists |
| [privacy.md](docs/privacy.md) | What is stored and where, the backup-password path, what validation, the extraction check and an import copy and for how long, what an archive holds (unencrypted), Recently Deleted and edited messages, network posture, the difference between backup and archive encryption, deletion limits |
| [development.md](docs/development.md) | Setup, commands, adding IPC commands, testing |
| [dependencies.md](docs/dependencies.md) | Every dependency and its license, the audit policy, the upgrade log |

## Roadmap

| Milestone | Scope | State |
|---|---|---|
| 1 Foundation | Shell, settings, archive location, CI for Windows and Linux | **Done** |
| 2 Backup discovery and validation | Find backups, read their manifests, detect encryption, unlock, validate | Implemented; sign-off pending (fixture-generator review, manual tests) |
| 3 Extraction check | Locate sms.db, probe the schema, and check that messages, chats, handles and attachment references can be read (counts only) | Implemented; sign-off pending (Windows CI, the clean-room review of the typedstream decoder and the fixture generators, the manual tests) |
| 4 Archive database | Extract messages and copy attachments into the archive; versioned SQLite schema, transactional and resumable imports, FTS5, integrity verification | Implemented; sign-off pending (CI of slices 4g–4n, two §18.8 Windows tests that are not written yet (a held partial file, a killed lock holder; see `docs/compatibility.md`), the manual tests, the owner's answers on D69, D70, D71, D79 and D82) |
| 5 Rich viewer | Conversation list, threads, inline media, search, date navigation | Planned |
| 6 Exports | TXT, CSV, JSON, HTML, PDF, XLSX, and attachments | Planned |
| 7 Direct device backup | USB device detection and local backup creation | Planned |
| 8 Hardening and release | Archive repair, recovery tests, diagnostic log, accessibility, signed packages | Planned |

## Known limitations

These apply to Milestone 4. The archive-specific ones come first; those
carried over from milestones 2 and 3 follow.

### The archive (milestone 4)

- **Archives are stored unencrypted,** including content from encrypted
  backups, the earlier text of edited messages inside the raw
  `message_summary_info` bytes, and any future pre-migration copy.
  Archive encryption is milestone 8; until then, rely on full-disk
  encryption. See [privacy.md](docs/privacy.md).
- **You cannot read your messages in the app yet.** The Archive page shows
  counts and status only; the viewer and search are milestone 5.
- **Tested only on synthetic data.** No backup made from a real iPhone has
  been imported. Whether message, chat and attachment GUIDs stay stable
  across iOS versions, restores and devices is unverified.
- **Crash consistency was tested by killing the process only.** Power-loss
  durability rests on drives honouring flushes, and on NTFS for directory
  entries, which is unverified.
- **De-duplication is per archive.** Two archives for the same phone do not
  merge. The confirmation step shows whether the model and name match, but
  cannot prove which phone a backup came from; picking the wrong import set
  mislabels "not in the latest import" (a label milestone 5 will show).
- **Rows without a usable or unique key** (for example a duplicated GUID)
  are imported unkeyed: importing the identical backup again adds nothing,
  but a changed backup may add them again. They are counted.
- **A backup with more than 4,194,304 rows in one keyed table cannot be
  imported.** With more than 4,194,304 Recently Deleted rows, no
  Recently-Deleted-only content is imported.
- **With three or more backups that disagree,** a column the newest backup
  did not record may keep a value that depends on import order; earlier
  values of the history columns are kept regardless.
- **Nothing is ever removed.** Messages deleted on the phone stay, and text
  captured before an unsend stays and stays searchable.
- **Search limits** (for milestone 5's search): parts of unspaced Chinese,
  Japanese, Thai and Korean text, a single emoji, and `ß`/`ss` do not
  match. Subjects, group titles and chat names are not indexed.
- **Attachments already in the archive are checked for presence and
  length, not re-read,** so bytes that changed at the same size go unseen
  until a full verify.
- **The disk-space estimate uses an unmeasured growth factor** (the code
  assumes 3.0; a synthetic database measured about 5.2) and only warns; the
  space guards stop an import safely and it can be continued. Free space
  on long Windows paths may read as unknown.
- **A file-size limit on the archive database itself** (for example FAT's
  4 GiB) fails the import instead of stopping it so it can be continued.
  On attachments it is counted per file.
- **Mapped network drives on Windows may go undetected,** depending on
  whether the path reveals them. FAT is not detected on Windows.
- **Archives on network shares and on virtualised shared folders**
  (VirtualBox, 9P, FUSE) are refused.
- **The decrypted copy of the Messages database and the backup's keys live
  for the whole import,** including the up-to-two-minute confirmation step
  and the attachment copy. A crash leaves the copy until the next sweep.
- **The password is asked again to continue an import.** Only one job runs
  at a time. A changed backup or an app update starts the import over.
- **Not decoded yet:** the text of earlier edits (kept raw),
  `chat.properties` (not kept), polls and link previews (kept raw).
- **No repair, no archive deletion, no thumbnails, and no pre-migration
  copy yet** (there is no real migration yet either). Verify only reports.
- **Copying attachments shows progress in attachment rows, not bytes.**
- **Real-backup performance, memory use and archive size are unknown.**
  Synthetic numbers are in [compatibility.md](docs/compatibility.md).
- **The persistent diagnostic log is deferred to milestone 8.**
- **Only one instance should run at a time.** Each archive is locked while
  it is written, so a second instance cannot write to it, but nothing else
  stops two instances from running.

### Carried over from milestones 2 and 3

- **The message-body decoder has been compared with two independent
  decoders, on synthetic data only and on part of it:** with pytypedstream
  on the structure of 46 synthetic vectors (10 more were exempted), and
  with imessage-exporter on the text of the four attributedBody-only rows
  of `msgs-full-plain` and `msgs-full-enc`. One hostile multipart row was
  not compared, because imessage-exporter's export does not show its text
  (see [compatibility.md](docs/compatibility.md), oracle runs 2 and 3). It
  has not been checked against Apple's own encoder. Its independent
  clean-room review is still pending.
- **The extraction check writes a plaintext copy of your Messages
  database into the archive folder**, decrypted for an encrypted backup,
  and deletes it straight after reading. **A crash leaves the copy on disk**
  until the app next starts or runs a check (after 5 minutes). Sync clients,
  backup tools and antivirus may see it while it exists. See
  [privacy.md](docs/privacy.md).
- **Pending changes (`-wal`):** if the Messages database's `-wal` file is
  missing, not listed, or invalid, the newest messages may be missing; the
  report says so. A `-wal` is reported as copied, never as applied. A main
  file and `-wal` captured at different moments may go undetected.
- **A damaged Messages database is not read at all** (its integrity check
  must pass first), and **a database with virtual tables is refused.**
- **Only attachments under `Library/SMS/Attachments/` and `StickerCache/`
  are looked up.** Other path forms are counted only. A Unicode
  normalisation or case difference between the database and the backup's
  file index reads as "not in this backup".
- **Counts of replies and reactions whose target was not found** use 64-bit
  hashes; a deliberately crafted collision could change a count, never a
  record.
- **Not decoded:** translations, polls, link previews, data detectors,
  chat backgrounds, Genmoji and the text of earlier edits. They are counted
  where they are recorded. **Not interpreted:** `chat.style`, the filter
  and transfer-state codes, the read and delivery flags, and associated
  types 2 and 3; they stay raw numbers.
- **The address on a message you sent is not its sender**, so no sender is
  attributed from it.
- **Real-backup performance, memory use and Messages database sizes are
  unknown.** A synthetic database with 1,000,000 messages was checked in
  6.6 s. The Messages database is refused above 32 GiB.
- **The report is kept in memory only** and is lost when the app closes.
- **Windows backup locations are not documented by Apple.** The
  `Apple\MobileSync\Backup` and `Apple Computer\MobileSync\Backup`
  subfolders come from third-party sources.
- **iMazing support is limited to its default location, and untested on
  real data.** `iMazing\Backups` was observed in iMazing's settings on one
  Windows 11 machine. If you moved iMazing's backup location, add that
  folder with **Add backup folder…**. iMazing's `iMazing.Versions` snapshots
  are never listed.
- **Linux has no default backup location.** Add the folder that holds your
  backups.
- **Encrypted backups whose Manifest.plist has no ManifestKey are
  unsupported.**
- **How Apple handles non-ASCII passwords is unverified.** The password is
  used exactly as typed.
- **The password is asked for on each check** and is never saved.
- **Checking needs an archive folder on a drive that supports file
  locking.** Some network shares do not.
- **On Linux, the archive folder must be on a drive that keeps Linux
  permissions.** FAT, exFAT or NTFS drives mounted with default options,
  and many network shares, are refused, because copies made while checking
  could be read by other users. Mounting with `dmask=077,fmask=077` makes
  such a drive usable. This follows from the code; it has not yet been
  tried on a real FAT, exFAT or NTFS mount.
- **A copy of the file index sits in the archive folder while a check
  runs** (decrypted, for an encrypted backup). It is deleted at the end. If
  the app was killed, it is deleted when the app next starts or checks a
  backup, once the copy is at least 5 minutes old. See
  [privacy.md](docs/privacy.md).
- **Progress cannot be reattached after a UI reload;** the page polls
  instead until the check ends.
- **One check runs at a time.**
- **Backup folder paths are shown,** and a backup's folder name is usually
  the device identifier (UDID): in the details panel, and in the Locations
  panel for a backup folder you added directly.
- **Not run by hand on Windows 11.** Every M1 check has been run by hand
  on Ubuntu 26.04.1: the folder picker, the backup refusal, and the setting
  surviving a restart. The M2 Backups page, the M3 extraction check and the
  M4 Archive page have not been run by hand on any real desktop yet; each
  was exercised only in a headless smoke test in a Linux container. On
  Windows, CI builds the app and runs the tests of M2, M3 and the first
  part of M4 (slices 4a–4f); the rest of M4 has not run on Windows at all,
  and nobody has used the UI there. The installer packages, on either
  platform, have not been installed and run by hand. See
  [compatibility.md](docs/compatibility.md).
- **Windows relies on Microsoft WebView2,** a proprietary OS component, to
  render the UI. Direct device backup on Windows (M7) will also need
  Apple's Mobile Device Service. Importing an existing backup never will.
- **Installers are unsigned.** Windows SmartScreen will warn until code
  signing is set up in M8.

### Deferred from milestone 4

These were cut to keep milestone 4 reviewable. They may follow as a
further slice or move into milestone 5; none is needed for an import:

- the pre-migration copy of the database, **required before the first real
  schema migration ships**;
- re-hashing attachments already in the archive during an import (a full
  verify does it);
- keeping `chat.properties` raw;
- the full verify's recomputation of derived columns from the raw values;
- a detailed verify report page (the app shows one sentence per kind of
  finding).

Still deferred from milestone 3: decoding the text of earlier edits,
reading `chat.properties` keys, matching attachment parts in message bodies
against attachment rows, date-unit histograms, tapback-history counts, and
the extraction report's sections for services, reactions, replies, edits
and message bodies.

### Next: milestone 5 (rich viewer)

- The conversation list and threads, paged through the archive's
  lock-free reader.
- Inline media read through the verified blob reader and a scoped asset
  protocol, with thumbnails.
- Search over the full-text index, with a bounded fallback for short
  Chinese, Japanese and Korean queries and emoji.
- Labels: "not in the latest import from this phone", "edited", "unsent;
  earlier text kept", "matched an earlier version", and "provisional
  (untested iOS version)".

## Contributing

Sign off your commits (`git commit -s`). The contribution terms
(inbound = outbound, Apache-2.0) are in [DCO](DCO). Do not copy code from
GPL, LGPL, AGPL or CC BY-SA projects. Never commit real messages or
backups. Use synthetic fixtures instead.

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE). iPhone, iMessage
and iOS are trademarks of Apple Inc. This project is not affiliated with
Apple.
