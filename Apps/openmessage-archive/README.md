# OpenMessage Archive

A local-first, open-source desktop application for archiving iPhone
Messages (iMessage, SMS and RCS) from local iPhone backups. You can browse,
search and export your history on your own computer. There are no cloud
services, accounts, subscriptions or telemetry.

Targets **Windows 11** and **Linux**. Built with Tauri 2, Rust, Vue 3,
TypeScript, Bootstrap 5 and SQLite. Licensed **Apache-2.0**.

> **Status: Milestone 3 (the extraction check) is implemented and awaiting
> sign-off.** The app finds iPhone backups, shows their details, unlocks
> encrypted ones with your password, checks that they can be read, and
> checks that their messages, chats, addresses and attachment references
> can be read, showing **counts only**. **It cannot import messages yet**
> (milestone 4). Milestones 2 and 3 have been tested only on synthetic
> backups and two third-party macOS samples, never on a backup made from a
> real iPhone. Every unfinished capability appears in the app disabled and
> marked "Planned · M*n*". See [the roadmap](#roadmap).

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
| [compatibility.md](docs/compatibility.md) | What has actually been tested, by OS, iOS version and backup type, including the extraction check's synthetic catalogue and oracle runs |
| [privacy.md](docs/privacy.md) | What is stored and where, the backup-password path, what validation and the extraction check copy and for how long, network posture, the difference between backup and archive encryption, deletion limits |
| [development.md](docs/development.md) | Setup, commands, adding IPC commands, testing |
| [dependencies.md](docs/dependencies.md) | Every dependency and its license, the audit policy, the upgrade log |

## Roadmap

| Milestone | Scope | State |
|---|---|---|
| 1 Foundation | Shell, settings, archive location, CI for Windows and Linux | **Done** |
| 2 Backup discovery and validation | Find backups, read their manifests, detect encryption, unlock, validate | Implemented; sign-off pending (fixture-generator review, manual tests) |
| 3 Extraction check | Locate sms.db, probe the schema, and check that messages, chats, handles and attachment references can be read (counts only) | Implemented; sign-off pending (Windows CI, the clean-room review of the typedstream decoder and the fixture generators, the manual tests) |
| 4 Archive database | Extract messages and copy attachments into the archive; versioned SQLite schema, transactional and resumable imports, FTS5 | Planned |
| 5 Rich viewer | Conversation list, threads, inline media, search, date navigation | Planned |
| 6 Exports | TXT, CSV, JSON, HTML, PDF, XLSX, and attachments | Planned |
| 7 Direct device backup | USB device detection and local backup creation | Planned |
| 8 Hardening and release | Archive verification, recovery tests, accessibility, signed packages | Planned |

## Known limitations

These apply to Milestone 3.

- **Nothing is imported, and no attachment is copied.** The extraction
  check reads the Messages database and shows counts; importing messages
  and attachments into an archive is milestone 4.
- **Tested only on synthetic data and on two third-party macOS samples.**
  No backup made from a real iPhone has been checked with this build yet.
  The test databases' column sets are hand-written from column-name facts
  taken from macOS `chat.db` snapshots; whether iOS sms.db matches them,
  including on iOS 27, is unverified. **Every real backup is reported with
  "This iOS version has not been tested".** See
  [compatibility.md](docs/compatibility.md).
- **The message-body decoder agrees with two independent decoders on
  synthetic test data.** It has not been checked against Apple's own
  encoder. Its independent clean-room review is still pending.
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
- **Not yet run by hand on Windows 11.** Every M1 check has been run by hand
  on Ubuntu 26.04.1: the folder picker, the backup refusal, and the setting
  surviving a restart. The M2 Backups page and the M3 extraction check have
  not been run by hand on any real desktop yet; both were exercised only in
  a headless smoke test in a Linux container. On Windows, CI built the app
  and ran the M2 tests, **but CI has not yet run the M3 tests on Windows**,
  and nobody has used the UI there. The installer packages, on either
  platform, have not been installed and run by hand. See
  [compatibility.md](docs/compatibility.md).
- **Archives are not encrypted yet.** Archive encryption is planned for M8.
  Until then, rely on full-disk encryption. See
  [privacy.md](docs/privacy.md).
- **Windows relies on Microsoft WebView2,** a proprietary OS component, to
  render the UI. Direct device backup on Windows (M7) will also need
  Apple's Mobile Device Service. Importing an existing backup never will.
- **Installers are unsigned.** Windows SmartScreen will warn until code
  signing is set up in M8.
- **Only one instance should run at a time.** This is not enforced yet.
  Settings writes are atomic, so a second instance is harmless now, but
  M4 will add per-archive locking.

### Deferred from milestone 3

These were cut to keep milestone 3 reviewable. They may follow before
milestone 4 or move into it; none is needed for the extraction check:

- decoding the text of earlier versions of edited messages;
- reading the keys of `chat.properties`;
- matching attachment parts in message bodies against attachment rows;
- date-unit histograms for columns whose unit is already sourced;
- counts of tapback history (whether only the latest reaction is stored);
- report sections in the UI for services, reactions, replies, edits and
  message bodies. Their counters are already computed; the app does not
  show them yet.

### Next: milestone 4 (archive database)

- A versioned archive schema.
- A transactional, resumable import driven by the same row passes the
  extraction check uses, bound to a digest of the copied database. **A
  backup that changes during an import aborts it.**
- Decisions on keeping raw message blobs, on importing Recently Deleted
  messages (the lean is to exclude them by default, with an opt-in and a
  warning), and on which body source to prefer.
- FTS5 search indexes and the persistent, redacted diagnostic log.

## Contributing

Sign off your commits (`git commit -s`). The contribution terms
(inbound = outbound, Apache-2.0) are in [DCO](DCO). Do not copy code from
GPL, LGPL, AGPL or CC BY-SA projects. Never commit real messages or
backups. Use synthetic fixtures instead.

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE). iPhone, iMessage
and iOS are trademarks of Apple Inc. This project is not affiliated with
Apple.
