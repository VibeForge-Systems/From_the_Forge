# OpenMessage Archive

A local-first, open-source desktop application for archiving iPhone
Messages (iMessage, SMS and RCS) from local iPhone backups. You can browse,
search and export your history on your own computer. There are no cloud
services, accounts, subscriptions or telemetry.

Targets **Windows 11** and **Linux**. Built with Tauri 2, Rust, Vue 3,
TypeScript, Bootstrap 5 and SQLite. Licensed **Apache-2.0**.

> **Status: Milestone 2 (backup discovery and validation) is implemented
> and awaiting sign-off.** The app finds iPhone backups, shows their
> details, unlocks encrypted ones with your password, and checks that they
> can be read. **It cannot import messages yet** (milestone 3). Milestone 2
> has been tested only on synthetic backups. Every unfinished capability
> appears in the app disabled and marked "Planned · M*n*". See
> [the roadmap](#roadmap).

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
| [compatibility.md](docs/compatibility.md) | What has actually been tested, by OS, iOS version and backup type |
| [privacy.md](docs/privacy.md) | What is stored and where, the backup-password path, what a check copies and for how long, network posture, the difference between backup and archive encryption, deletion limits |
| [development.md](docs/development.md) | Setup, commands, adding IPC commands, testing |
| [dependencies.md](docs/dependencies.md) | Every dependency and its license, the audit policy, the upgrade log |

## Roadmap

| Milestone | Scope | State |
|---|---|---|
| 1 Foundation | Shell, settings, archive location, CI for Windows and Linux | **Done** |
| 2 Backup discovery and validation | Find backups, read their manifests, detect encryption, unlock, validate | Implemented; sign-off pending (Windows CI, fixture-generator review, manual tests) |
| 3 Extraction | Locate sms.db, probe the schema, extract messages, conversations, participants and attachments | Planned (next after M2 sign-off) |
| 4 Archive database | Versioned SQLite schema, transactional and resumable imports, FTS5 | Planned |
| 5 Rich viewer | Conversation list, threads, inline media, search, date navigation | Planned |
| 6 Exports | TXT, CSV, JSON, HTML, PDF, XLSX, and attachments | Planned |
| 7 Direct device backup | USB device detection and local backup creation | Planned |
| 8 Hardening and release | Archive verification, recovery tests, accessibility, signed packages | Planned |

## Known limitations

These apply to Milestone 2.

- **No messages are extracted yet.** Backups can be found and checked;
  importing messages arrives in milestone 3.
- **Tested only on synthetic backups.** No backup made from a real iPhone
  has been checked with this build yet. See
  [compatibility.md](docs/compatibility.md).
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
  surviving a restart. The M2 Backups page has not been run by hand on any
  real desktop yet. On Windows, CI builds the app and runs its tests,
  but nobody has used the UI there yet. The installer packages, on either
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

## Contributing

Sign off your commits (`git commit -s`). The contribution terms
(inbound = outbound, Apache-2.0) are in [DCO](DCO). Do not copy code from
GPL, LGPL, AGPL or CC BY-SA projects. Never commit real messages or
backups. Use synthetic fixtures instead.

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE). iPhone, iMessage
and iOS are trademarks of Apple Inc. This project is not affiliated with
Apple.
