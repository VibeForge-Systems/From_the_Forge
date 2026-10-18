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
| Temporary copy of the Messages database (`sms.db` and its `-wal`, decrypted for an encrypted backup) | `<archive folder>/.oma-staging/`, only during an extraction check, and deleted as soon as its rows have been read; see "The extraction check (M3)" | M3 |
| The last extraction report (counts and a date range) | **Memory only**, until the app closes. It is never written to disk and never logged. | M3 |
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
  and domains of everything in the backup, attachment names included.
  Validation does **not** copy the Messages database (`sms.db`): it is
  checked where it is, by reading its first block (and, when encrypted, its
  last block) in memory. The extraction check (M3) **does** copy it; see
  the next section.
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

## The extraction check (M3)

**Check that messages can be read** runs validation first and then reads
every row of the Messages database. Everything in the previous section
(where copies go, the sweep, the 5-minute rule, synced and shared folders,
antivirus) applies to it as well. In addition:

- **A plaintext copy of the Messages database is written to disk.** The
  check copies `sms.db`, and its `-wal` when the backup lists one, into
  `<archive folder>/.oma-staging/s<16 hex digits>/db/<n>/`. For an
  encrypted backup the copy is **decrypted**: for as long as it exists,
  anyone who can read the archive folder can read your messages, including
  messages in Recently Deleted. This replaces M2's statement that sms.db is
  never copied, which still holds for validation.
- **How long.** The copy is deleted as soon as its rows have been read;
  the report says whether that succeeded.
  Closing the session deletes anything left. **If the app crashes or is
  killed during a check, the decrypted copy stays on disk** until the sweep
  removes it at the next start or check (after the same 5-minute rule as
  above). Release builds stop at once on an internal error (they abort on
  panic), so a crash skips every cleanup step. If the app is never started
  again, the copy remains until you delete `.oma-staging` yourself.
- **Windows.** The staged files are created with the *temporary* attribute,
  so Windows prefers to keep them in its cache. They are **not** excluded
  from Windows Search: `CreateFileW` ignores the *not content indexed*
  attribute (observed on the Windows CI runner), and setting it afterwards
  needs a Win32 call the app does not make yet. If the archive folder is in
  an indexed location such as Documents, the indexer could read a staged
  copy while it exists. Exclude the archive folder from indexing (Indexing
  Options) if that matters to you. The attribute does not stop an
  antivirus scanner, a backup tool, a sync client or another user who can
  read the archive folder from reading them. Whether Defender scans them
  is part of the manual checklist in compatibility.md.
- **Deleted is not securely erased**, as for any staged copy (see
  "Deleting data").
- **What is not copied.** Attachments are **not** copied, decrypted or
  hashed. For each attachment the check looks up its entry in the backup's
  file index and reads the file's metadata; for an encrypted backup it also
  unwraps the file's key in memory, which writes nothing.
- **What is never read.** The text of Recently Deleted messages
  (`recoverable_message_part.part_text`) is refused by an SQLite authorizer
  on the connection, so no query can read it. Poll and app payloads
  (`payload_data`) are read only as "present or not". (From milestone 4
  the reader can also return their bytes, up to 1 MiB, but only when the
  caller asks for raw values; the extraction check never does.) The check counts
  Recently Deleted entries and nothing more. How milestone 4 imports them
  is decided (D60, owner-confirmed on 2026-10-18; see "The archive (M4)"
  below).
- **What the report contains.** Counts, closed kinds (such as "damaged" or
  "unsupported" and their reasons), raw numeric codes in capped histograms,
  and the date range of your messages. It contains no message text, no
  address, no name, no chat title, no GUID and no file name. The date
  range is shown in the app, kept in memory only, and never written to disk
  or logged.
- **What stays in memory, and what is wiped.** While the check runs, the
  current record is in memory, plus bounded sets of truncated SHA-256
  hashes of message GUIDs (used to count replies and reactions whose
  target is missing). Text and blob values the app copies out of the
  database, and decoded message bodies, are zeroized when dropped. **Not
  wiped:** SQLite's page cache and internal buffers, rusqlite's
  intermediate copies, and the property-list values parsed from
  `message_summary_info`. They are freed normally when the check ends.
- **Logs.** The check logs phase names, counts, kinds and durations. It
  never logs any string from the database (table and column names
  included), any GUID, timestamp or date range, attachment paths, or the
  text of an internal error (panic). Automated tests check this over every
  synthetic fixture, including the failure paths and a forced panic.

## The archive (M4): decisions already taken

Milestone 4 is in progress: the import is not reachable from the app yet,
and this section lists only the decisions the owner has confirmed (on
2026-10-18). The full write-up of what an archive holds comes with the
milestone's documentation.

- **Recently Deleted (D60).** Messages that are only in Recently Deleted
  are **excluded by default**. Including them is an opt-in for each import,
  with a warning. When you opt in, their attachments are included too. A
  message that is already in the archive and later appears in Recently
  Deleted is **marked** as recently deleted, but its archived content is
  not updated from the deleted copy. The text of Recently Deleted message
  parts (`recoverable_message_part.part_text`) is never read either way.
  By default, an attachment that belongs **only** to excluded messages is
  left out as well: the archive gets no record of it (and, once
  attachment files are copied, no file). An attachment also used by a
  message that is imported is kept. The mark records, for each backup
  that showed the message in Recently Deleted, which conversation it was
  deleted from and the deletion time as the backup stores it.
- **Text captured before an unsend (D61).** If the archive already holds a
  message's text and a later backup shows the sender unsent it, the
  archive **keeps the earlier text** (it stays searchable) and marks the
  message as unsent by the sender.
- **The search index (D61, D62).** The archive's full-text index holds
  the display text of **every** version of a message it has seen: the
  text before and after an edit, and the text captured before an unsend.
  Earlier versions stay searchable; nothing is removed from the index.
  Messages excluded as Recently Deleted are not imported, so they are not
  indexed.
- **Raw message data (D59).** The archive keeps the raw `attributedBody`,
  `message_summary_info` and `payload_data` bytes of each message (poll,
  app and link payloads included), within fixed size caps, so a newer
  version of the app can decode them again without the backup.
- **Device identity (D51).** The archive does not identify your phone. It
  never stores or hashes the UDID; when a backup is imported, the app
  shows which device facts match earlier imports and you choose whether it
  is the same phone. The device name, model, iOS version and build are
  stored exactly as the backup records them.

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
| Status | Unlocking and checking: M2. Checking that messages can be read: M3. Importing: M4. | Planned, M8. The likely approach is SQLCipher for the database plus encrypted attachment storage. Until then, **archives are stored unencrypted** and rely on OS account security and full-disk encryption (BitLocker, LUKS). |

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
- The staged Messages database (M3) is opened under an SQLite authorizer
  that allows only reads, with double-quoted string literals, triggers and
  views turned off, and with operation budgets on every statement whose
  cost grows with the data. A database with virtual tables is refused
  before any integrity check runs, and a damaged one (failed
  `quick_check`) is not read at all. Every value is read under a byte cap,
  and the typedstream decoder for message bodies runs under fixed budgets
  and fails closed.
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
fixture. From M3 they also never record any string from the Messages
database, GUIDs, timestamps, the date range, attachment paths or panic
messages. Set `OMA_LOG=debug` for more detail. That
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
