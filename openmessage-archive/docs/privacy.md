# Privacy and security

The application exists so that people can keep control of their message
history. This document states what it does with data, what it does not
do, and the limits of both. Statements about planned behaviour are marked
by milestone and are commitments the design is built around.

## What is stored, and where

| Data | Location | Milestone |
|---|---|---|
| Preferences (theme, panel layout) and the archive library path | `settings.json` in the OS config directory. Linux: `~/.config/com.vibeforgesystems.openmessagearchive/`. Windows: `%APPDATA%\com.vibeforgesystems.openmessagearchive\` | M1 |
| Archives (messages, contacts metadata, attachments, indexes) | Only inside the archive library folder the user chooses | M4 |
| Backup passwords | **Never written to disk in plain text.** They are held in memory for the import only and zeroized afterward. Saving them in the OS credential store (Windows Credential Manager; Secret Service on Linux) will be opt-in. | M2 |
| Search history | Off by default. It can be cleared, and it is stored inside the archive. | M5 |

The settings file never contains message content, contact data or
credentials. You can delete it at any time; the application starts again
with defaults.

## Network

- The application makes **no network requests**. There is no telemetry,
  analytics, crash reporting, update check, account or licence check.
- The Rust side does not depend on an HTTP client crate. CI's `cargo deny`
  run makes the dependency tree auditable.
- The webview runs under a Content Security Policy whose `connect-src`
  allows only Tauri's IPC channel. Fonts, icons and styles are bundled; no
  CDN is used.
- **Caveat: the webview runtime itself.** On Windows, the UI runs in
  Microsoft Edge WebView2, a Microsoft component that updates itself and
  can contact Microsoft services (for example SmartScreen or update checks)
  on its own schedule. This application does not send content through it.
  The runtime's own behaviour is governed by Microsoft's policies and the
  user's Windows settings. On Linux, WebKitGTK is a system library with no
  such service. This is the main place the application relies on software
  it does not control.
- **Direct device backup (M7)** talks to the phone over USB, through
  usbmuxd on Linux or Apple Mobile Device Service on Windows. Both are
  local channels.

## The original backup and the archive are different things

| | iPhone backup encryption | Archive encryption |
|---|---|---|
| What it protects | The backup folder written by Finder, iTunes or Apple Devices | The archive this application creates |
| Who sets the password | The user, on the iPhone or in Apple's software | The user, in this application (planned) |
| Effect of importing | The application decrypts what it needs, read-only. The backup stays encrypted on disk. | — |
| Status | Planned, M2 | Planned, M8. The likely approach is SQLCipher for the database plus encrypted attachment storage. Until then, **archives are stored unencrypted** and rely on OS account security and full-disk encryption (BitLocker, LUKS). |

Importing an encrypted backup produces a readable archive. The UI will say
so at the point of import, and recommend full-disk encryption or (from M8)
an encrypted archive.

## Handling of untrusted input

Backups and their contents are treated as hostile:

- Paths from manifests and attachment records go through
  `oma_fs::resolve_within`. Absolute paths, drive prefixes, `..`, Windows
  device names, NTFS stream syntax, control characters and bidi overrides
  are all rejected.
- Filenames written on export go through `oma_fs::sanitize_filename`.
- Key-derivation iteration counts read from a backup are capped, so a
  crafted file cannot cause an unbounded computation (M2).
- SQL uses bound parameters only. Source databases are opened read-only,
  from copies.
- Attachments are never executed. Previews will be generated only for
  formats with memory-safe decoders. Anything else is shown as a file to
  open with the user's own application.
- Message text is rendered as text, never as HTML. The UI does not use
  `v-html` with untrusted content, and HTML exports will escape everything.

## Logging

Diagnostic logs record operation kinds, states and counts. They never
record message bodies, contact names, phone numbers, email addresses,
attachment contents or passwords. Set `OMA_LOG=debug` for more detail. That
setting changes how much is recorded, not which kinds of data.

## Deleting data

Removing an archive from the application (M4) will remove only the
application's reference to it. Deleting an archive's files will be a
separate, explicitly confirmed action. Some limits apply to deletion on
modern storage:

- **SSDs** remap writes (wear levelling). Overwriting a file does not
  guarantee the old blocks are erased, and the application does not claim
  "secure deletion".
- **Journaled and copy-on-write filesystems** (NTFS, ext4 with data
  journaling, btrfs, ZFS) and **snapshots** (Windows shadow copies,
  Timeshift, btrfs snapshots) can retain old copies.
- **SQLite** can leave deleted content in free pages and in the WAL until
  it is vacuumed. The archive will offer a compaction step after
  deletions.

Full-disk encryption is the effective protection against recovery of
deleted data. Deleting an archive cannot replace it.
