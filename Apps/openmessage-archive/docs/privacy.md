# Privacy and security

The application exists so that people can keep control of their message
history. This document states what it does with data, what it does not
do, and the limits of both. Statements about planned behaviour are marked
by milestone and are commitments the design is built around.

## What is stored, and where

| Data | Location | Milestone |
|---|---|---|
| Preferences (theme, panel layout) and the archive library path | `settings.json` in the OS config directory. Linux: `~/.config/com.vibeforgesystems.openmessagearchive/`. Windows: `%APPDATA%\com.vibeforgesystems.openmessagearchive\` | M1 |
| Backup folders you added (`backupFolders`: each folder's path and whether it is one backup or a folder of backups) | The same `settings.json`. A folder's path often ends in the device's identifier (UDID). | M2 |
| Temporary copy of a backup's file index (Manifest.db, decrypted for an encrypted backup) | `<archive folder>/.oma-staging/`, only while a check runs; see below | M2 |
| Archives (messages, contacts metadata, attachments, indexes) | Only inside the archive library folder the user chooses | M4 |
| Backup passwords | **Never stored, anywhere.** A password is asked for on each check, used once, and zeroized in Rust; see "Backup passwords" below. No OS credential store is used. Storing passwords there was dropped from M2 and no milestone is planned for it. | M2 |
| Search history | Off by default. It can be cleared, and it is stored inside the archive. | M5 |

The settings file never contains message content, contact data or
credentials. You can delete it at any time; the application starts again
with defaults.

## Backup passwords (M2)

The password for an encrypted backup travels this way:

1. **The password dialog** keeps it in one component variable while the
   dialog is open, and clears it on submit, cancel, Escape and when the
   dialog is destroyed. It is never put in a store, a toast, an error or
   the URL. A rejection of the request that is not a structured error is
   replaced by a fixed sentence, so an error text can never echo it.
2. **IPC.** Tauri deserializes command arguments from a borrowed JSON value.
   The application copies the password from that value into a buffer that
   is zeroized when dropped (`Zeroizing<String>`). Its errors use fixed
   messages and never echo the value.
3. **Rust.** The password is consumed by the first stage of key derivation
   and dropped; the derived keys, class keys and file keys are zeroized when
   the check ends.

Copies that **cannot be wiped**:

- the string in the webview's JavaScript heap, until it is garbage
  collected;
- the invoke message's JSON value and the request body inside Tauri, until
  Tauri drops them;
- `serde_json`'s scratch buffers;
- any internal copies the `hmac` and `sha1`/`sha2` crates make. They are
  built with their `zeroize` features, but whether every internal copy is
  wiped is UNVERIFIED.

The password is never logged, not even its length, and never passed on a
command line. Whether WebView2 offers to save it despite
`autocomplete="off"` is UNVERIFIED (part of the Windows manual test).

## Checking a backup: what is copied, where, and for how long (M2)

- **What is staged.** Validation copies the backup's file index,
  `Manifest.db`, and its `-wal` if present, into your archive folder. For an
  encrypted backup the copy is decrypted. Manifest.db holds the file names
  and domains of everything in the backup, attachment names included. The
  Messages database (`sms.db`) is **not** copied in M2: it is checked where
  it is, by reading its first block (and, when encrypted, its last block)
  in memory.
- **Where.** `<archive folder>/.oma-staging/s<16 hex digits>/`, never the
  system temp folder. SQLite is told to keep its temporary data in memory.
  Staging is refused inside any backup, inside the MobileSync store and
  inside iMazing's default backup store (`iMazing\Backups`). A store
  moved to another folder is not recognised by name, but each backup in it
  is still refused.
- **How long.** Copies exist only while a check runs. If the check ends
  normally they are deleted at its end, and the page says whether that
  succeeded. If deletion fails, or the app is killed or crashes, a sweep
  removes them later. The sweep runs when the app starts and before each
  check, against the current archive folder, and removes only sessions
  whose lock nobody holds and that are at least 5 minutes old. So after a
  crash and a relaunch within 5 minutes, the copy stays until a later start
  or check. When the archive folder is changed or cleared, the old folder
  is swept once, with the same 5-minute rule: younger copies remain there
  until you delete `.oma-staging` yourself (or choose that folder again).
  If the app is never started again, copies remain until you delete them.
- **Deleted is not securely erased.** The SSD, journaling and snapshot
  caveats under "Deleting data" apply.
- **Synced archive folders.** If the archive folder is inside a folder a
  sync client manages, the client may upload the staged copy before it is
  deleted. A warning is shown before every validation, for encrypted and
  unencrypted backups alike.
- **Shared archive folders.** On Linux the staging folder is created with
  mode 0700 and its files with 0600, and the app refuses a staging folder
  owned by another user or readable by others. A drive that does not keep
  Linux permissions reports group and other bits whatever mode the app
  asks for: FAT, exFAT or NTFS mounted without a private umask, and many
  network shares mounted with default modes. Such an archive folder is
  refused: Settings marks it unusable, and a check will not start. Mounting
  the drive with `dmask=077,fmask=077` (or `umask=077`) makes it usable.
  This follows from the code and from documented mount defaults; it has
  **not** yet been tried on a real FAT, exFAT or NTFS mount. On Windows,
  staging cannot be made private: it inherits the archive folder's
  permissions, so other local users can read staged copies if they can
  read the archive folder.
- **Antivirus.** Defender and similar tools may scan staged files locally.
- **Backups inside sync folders.** Reading a backup stored in a OneDrive or
  Dropbox folder may make the sync client download files that were stored
  online only. The details panel says so.

## Device identifiers (M2)

The app never stores or keeps these fields from a backup: `Unique
Identifier` (the UDID), `Target Identifier`, `Serial Number`, `Phone
Number`, `IMEI`, `ICCID`, `MEID`, `GUID`, and the installed-application
lists. The plist parser still decodes every value while it streams through
a file, so these values pass through memory briefly and are discarded; no
data structure has a field for them. One exception to the streaming skip:
Manifest.plist's `Lockdown` dictionary is parsed whole, and only four keys
are kept from it (`ProductVersion`, `ProductType`, `BuildVersion`,
`DeviceName`). Third-party sources say `Lockdown` also holds the UDID and
serial number (**UNVERIFIED**); if so, those are parsed into memory and
dropped with the rest of the dictionary.

However, the folder of a backup made by Apple Devices, iTunes or
`idevicebackup2` is usually named after the device's UDID, and the app
shows backup folder paths in two places, so you can find, move or delete
the backup:

- **The details panel** shows the selected backup's folder path.
- **The Locations panel** on the Backups page shows the full path of every
  added folder. If you added a backup folder itself (rather than the folder
  that holds your backups), that path ends in the backup's folder name,
  usually the UDID, and it is visible whenever the Backups page is open,
  with or without a backup selected.

The backup list shows the device name and location, not the path. Paths are
never logged.

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
| Status | Unlocking and checking: M2. Importing: M3. | Planned, M8. The likely approach is SQLCipher for the database plus encrypted attachment storage. Until then, **archives are stored unencrypted** and rely on OS account security and full-disk encryption (BitLocker, LUKS). |

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
- Key-derivation iteration counts read from a backup are capped (at most
  20,000,000 first-stage and 1,000,000 second-stage rounds) before any
  derivation starts, so a crafted file cannot cause an unbounded
  computation. Implemented in M2.
- Property lists from a backup are read under event, byte and depth
  budgets, so a small crafted file cannot expand without bound (M2).
- Every file opened from a backup must be a regular file, not a link,
  FIFO or device (M2). The backup's own databases are never opened by
  SQLite; only staged copies are.
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
attachment contents or passwords. From M2 they also never record paths,
folder names, device names, UDIDs, backup file names or identifiers, or
SQLite message text; automated tests check this over every synthetic
fixture. Set `OMA_LOG=debug` for more detail. That
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
