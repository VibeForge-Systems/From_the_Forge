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
| Viewer preferences (`viewer`: time format and zone, motion, list previews, revealing earlier text by default, the playback choice, hidden characters, blur, idle hide, capture protection and zoom). Choices and numbers only. | The same `settings.json`. | M5 |
| Temporary copy of a backup's file index (Manifest.db, decrypted for an encrypted backup) | `<archive folder>/.oma-staging/`, only while a check runs; see below | M2 |
| Temporary copy of the Messages database (`sms.db` and its `-wal`, decrypted for an encrypted backup) | `<archive folder>/.oma-staging/`, only during an extraction check, and deleted as soon as its rows have been read; see "The extraction check (M3)" | M3 |
| The last extraction report (counts and a date range) | **Memory only**, until the app closes. It is never written to disk and never logged. | M3 |
| Temporary copy of the Messages database during an import (decrypted for an encrypted backup) | `<archive folder>/.oma-staging/`, from staging until after the import's last pass, including the confirmation step (up to two minutes) and the attachment copy; see "The archive (M4)" | M4 |
| Archives: messages and their raw source bytes, addresses (handles), chat names, attachment metadata and files, the search index, import records and verify results | Only inside the archive library folder the user chooses, one `a<32 hex>/` folder per archive, **stored unencrypted** (see "The archive (M4)") | M4 |
| Exports (M6): copies of messages, names, addresses and, if chosen, the original attachment files | Wherever you save them; **plaintext, decrypted, outside the app's control**; see "Exports (M6)". Nothing about an export is kept after the app closes. | M6 |
| Backup passwords | **Never stored, anywhere.** A password is asked for on each check, used once, and zeroized in Rust; see "Backup passwords" below. No OS credential store is used. Storing passwords there was dropped from M2 and no milestone is planned for it. | M2 |

There is no search history: the row that promised one stored inside the
archive was removed (owner-confirmed 2026-10-18, Q1c), because the viewer
is read-only and writes nothing to the archive.

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

## The archive (M4)

Milestone 4 imports a backup's messages and attachment files into an
archive inside your archive folder, from the Archive page (or **Import
messages…** on the Backups page). Four of its privacy decisions were
confirmed by the project owner on 2026-10-18: D51 (device identity), D59
(raw message data), D60 (Recently Deleted) and D61 (text captured before
an unsend). They are marked **owner-confirmed** below. One related choice,
D70 (the model and iOS version in the import records), follows the
design's lean and still awaits the owner's confirmation.

### Archives are stored unencrypted

**An archive is plaintext at rest.** That covers `archive.sqlite` and its
`-wal` and `-shm` files, the full-text search index inside it, every file
under `attachments/`, and the files under `imports/`. Any future
pre-migration copy (`archive.pre-v<n>.sqlite`, not written by M4, which
ships no real migration) will be **another full plaintext copy**.

- **Content from an encrypted backup is stored decrypted.** The backup
  stays encrypted on disk; the archive does not.
- Until archive encryption arrives (M8), rely on your OS account's
  security and on full-disk encryption (BitLocker, LUKS).
- **The confirmation step says this on every import**: "The archive is
  stored unencrypted…", with its own sentence for an encrypted backup.

### What an archive holds

- Message rows with every source column the app knows, the decoded text
  of each message, and the **raw `attributedBody` bytes**.
- The **raw `message_summary_info` bytes**, which **contain the text of
  earlier versions of edited messages** (`ec[*].t`). M4 does not decode
  them, but the bytes are in the archive.
- The **raw `payload_data` bytes** (polls, app and link payloads), whose
  contents this project has not characterised.
- Addresses (handles), chat names and identifiers, attachment metadata
  (original names, types, sizes, dates) and the attachment files
  themselves.
- `destination_caller_id`, which third-party sources describe as the
  account (number or email) a message was sent to or from, that is,
  probably **your own address**. That meaning is **UNVERIFIED** in this
  project.
- For each import: the device name, model, iOS version and build exactly
  as the backup records them, the import's start and end times, the
  backup's dates, and the counts. The archive's label is your own text.

It holds **no** UDID, serial number, IMEI, backup folder path or backup
password. (An earlier draft said the archive holds "no phone number of the
device"; that claim is withdrawn, because `destination_caller_id` may be
exactly that.)

**Raw message data (D59, owner-confirmed).** The raw `attributedBody`,
`message_summary_info` and `payload_data` bytes are always kept, within
fixed size caps, so a newer version of the app can decode them again
without the backup.

### Edited, unsent and deleted messages

- **Nothing is ever removed.** A message deleted on the phone stays in the
  archive. Later releases will label such messages "not in the latest
  import from this phone", and only against a complete import (one whose
  pending-changes file was read).
- **Text captured before an unsend (D61, owner-confirmed).** If the
  archive already holds a message's text and a later backup shows the
  sender unsent it, the archive **keeps the earlier text**, keeps it
  **searchable**, and marks the message as unsent by the sender.
- **Edits.** The text before and after an edit is kept as separate
  versions, and every version stays in the search index.
- **The search index (D62).** It holds the display text of **every**
  version of every imported message. Earlier versions stay searchable;
  nothing is removed from it.

### Recently Deleted (D60, owner-confirmed)

- **Excluded by default.** A message that is only in Recently Deleted when
  the archive first sees it is not imported, **nor are attachments that
  belong only to such messages**: the archive gets no record of them and
  no file. An attachment also used by an imported message is kept.
- **Including them is an opt-in for each import**, with a warning at the
  start of the import. When you opt in, their attachments are included
  too.
- **A message already in the archive** that later appears in Recently
  Deleted is never removed. It is **marked** as recently deleted (for each
  backup that showed it there: the conversation it was deleted from and
  the deletion time as the backup stores it), but its archived content is
  not updated from the deleted copy.
- The text of Recently Deleted message parts
  (`recoverable_message_part.part_text`) is **never read**, either way.

### During an import

- **The temporary copy of the Messages database (D74).** The decrypted
  copy of sms.db (and its `-wal`) in `.oma-staging/` exists from staging
  until after the last database pass. That includes the confirmation step,
  which waits **at most two minutes**, and the whole attachment copy. It is
  deleted before the archive is finalized; the import's result says
  whether that succeeded. Declining, cancelling, quitting the app or letting the
  confirmation time out writes nothing to the archive and deletes the
  copy with the rest of the staging folder.
- **The backup's unwrapped keys are held in memory for the whole import
  job** (they are needed to decrypt each attachment file) and zeroized when
  it ends.
- **A crash leaves the copy** on disk until the staging sweep removes it
  (at the next start or check, after the 5-minute rule above).
- **Attachment copies in flight.** Each file is first written to
  `attachments/.incoming/<n>.partial` (0600 on Unix); for an encrypted
  backup it holds the decrypted bytes. A failed copy deletes it, and so
  does an import that stops, fails or is cancelled before the copied file
  is placed in the store (including a stop for lack of space). A partial
  file that could not be deleted is counted in the import's counts and
  removed the next time the archive is opened for writing, as is one left
  by a crash.
- **The password** (`start_import`) travels exactly as for the backup
  checks ("Backup passwords" above): zeroized in Rust, used once, never
  stored. It is asked for again to continue an import.

### Attachment files

- Each file is stored **as plaintext** in `attachments/<first two>/<64
  hex>`, named only by the **SHA-256 of its content**, never by its
  original name (which stays in the database). Identical files are stored
  once.
- **A content-hash name is not secret:** someone who can list the folder
  can confirm that a file they already have is present. M8 plans keyed
  names.
- A file that is already in the archive with the same length is not read
  again, so its bytes are re-checked only by a full verify.
- Nothing about a file (name, path, hash or content) is logged; only
  counts are.

### Device identity (D51, owner-confirmed)

The archive is the de-duplication scope; it does not identify your phone.
It never stores or hashes the UDID (D14 is unchanged). At the confirmation
step the app shows, for each earlier import set ("lineage"), whether the
model and the device name **match** or **differ**, never the values, and
**you choose** whether this backup is from the same phone. The gate cannot
prove identity: choosing the wrong set mislabels "not in the latest import"
for that set, and nothing else.

### Import records and verify results

- **`imports/session-<n>.json`** (0600 on Unix) is written when an import
  completes, is cancelled, stops or fails, so the import history survives
  damage to the database. It holds the app and schema versions, the
  session's state, its own start and end times, whether the backup was
  dated and which file the date came from (not the date), the lineage
  number, the backup's **model and iOS version** (D70, a lean awaiting the
  owner) and whether it was encrypted, the WAL and option settings, the
  qualifiers, and the counts. It holds **no** device name, message,
  address, GUID, content digest, backup date, path or label.
- **Verify** reads the database, every attachment file (a full verify reads
  each one to the end) and every `imports/` file, and changes none of them:
  it repairs, deletes and moves nothing. It records only numbers, booleans,
  check states and the names of the archive's own tables (`verify_run`),
  and after a full verify a check state and time on each stored file's row.

### What crosses into the window

(For the viewer, see "The viewer (M5)" below.) The Archive page and the
confirmation step show counts, closed kinds,
booleans, dates and the archive's label only, never a message, address,
chat name, file name or device value. The IPC types have one free-text
field, the label, and a structural test fails on any other. To pre-select
the backup an unfinished import came from, the app compares, inside Rust,
the listed backups' device name, model and backup date with the stored
ones, and returns only backup ids.

### Permissions, sync clients and volumes

- **Linux:** archive folders are 0700 and files 0600, re-checked at every
  open. An archive whose owner or mode has changed is **refused**.
- **Windows:** the archive inherits the archive folder's permissions (no
  private ACL is set), so anyone who can read that folder can read the
  archive. The confirmation step says so on every import.
- **Sync and backup clients, antivirus and indexers** may read archive
  files. When the archive folder is inside a known sync client's folder,
  the confirmation step repeats the sync warning on **every** import,
  because unlike staging, the archive is permanent. Exclude the archive
  folder from sync and indexing if that matters to you.
- **Network and virtualised volumes are refused** for archives (network
  shares, UNC paths, FUSE, 9P and VirtualBox shared folders on Linux).
  On Windows a mapped network drive is refused only if the path reveals it
  (see compatibility.md).

### Deletion is not erasure

The archive database runs with SQLite's `secure_delete` on and the search
index with FTS5's `secure-delete` option. These zero freed database pages
only. Old page images in the `-wal`, file-system blocks released by
checkpoints or truncation, SSD remapping and snapshots are **not** erased.
M4 deletes nothing a user can see: there is no way to delete an archive or
a message from one yet. These settings are hygiene, not protection; see
"Deleting data" below.

## The viewer (M5)

M5 is the first milestone that shows message content, addresses, chat
names and attachment files in the window. *Status: implemented (PR A,
slices 5a–5f, merged; PR B, slices 5g–5k, in review). The screens were
tested in jsdom and once in a real window: a headless smoke test on
WebKitGTK 2.52.6 in a Linux container, with synthetic data and no audio
device (`compatibility.md`). Nothing here has run on a desktop or on
Windows. Items marked **UNVERIFIED** depend on the web engine and are not
yet tested; each is a row of the milestone 5 manual checklist in
`compatibility.md`, to be recorded per engine and engine version.
Spike 5s(2) (playback on a desktop with audio, and on Windows) is still
UNVERIFIED, so playback stays behind feature detection (`canPlayType`).*

### What crosses into the window

- Message text, addresses, chat names, file names and image previews now
  cross into the webview, which is a separate process. They cross only
  when the window asks for what it shows, page by page and under size
  limits.
- **One-time codes** and **text captured before the sender unsent it**
  cross only after you press "Show code" or "Show earlier text" for that
  message (owner-confirmed 2026-10-18, Q3b). A setting can show earlier
  text by default; it is off by default. Earlier versions of an edited
  message are decoded from the archive when you open its details; nothing
  is stored or indexed for that (Q3a).
- No GUID, content digest, rank, file path or storage name crosses, and
  **no read, delivery or sent value**, chat-level ones included. A
  structural test of the IPC types enforces this.
- Attachment files are named in the window only by a random token that is
  valid for the current viewer session, and only for files the session has
  shown. Tokens end when the viewer closes, the archive changes or the app
  exits. They keep row numbers out of URLs; they are not a protection
  against a compromised page.

### Images

Images are decoded by memory-safe Rust decoders in a separate helper
process (the app's own program, started for this) and re-encoded as JPEG
or PNG, without any metadata (location included), before the window sees
them. The original image bytes never reach the web engine's image
decoders. Before decoding, the whole file is re-read and checked against
the checksum recorded at import; that check protects against corruption
only. A helper that crashes ends one preview, not the app; after five
crashes in a session, previews are turned off for that session. **The
helper is not an operating-system sandbox:** it runs as you, so a
successful exploit of a decoder could read your files. SVG, HTML, PDF and
other formats are never rendered; they are shown as file cards.

### Audio and video

Audio and video are played by your system's media codecs (GStreamer on
Linux, Media Foundation on Windows) from the original file
(owner-confirmed 2026-10-18, Q1b and Q4). The original bytes reach the
web engine only when you press Play, and only after:

1. a structural check of the file (MP4, QuickTime, M4A and WAV; MP3, Ogg
   and WebM get a check of their first bytes only), which also refuses
   QuickTime reference movies and external data references, so a file
   cannot make the engine fetch another;
2. a full check against the checksum recorded at import, which proves the
   file is the one imported, **not that it is safe**;
3. by default (`confirm`), your confirmation for that file in this
   session.

A file crafted by its sender could exploit a flaw in those codecs; the app
cannot prevent that. On Linux the web engine's own sandbox may be off
(**UNVERIFIED**: not determined in the smoke test; a row of the manual
checklist in `compatibility.md`). Playback can be
turned off in the viewer settings; with `off`, the app refuses playback in
its own code and the engine never receives attachment bytes.

### Attachments are never opened, executed or saved

M5 never hands an attachment to another program and never saves one
(owner-confirmed 2026-10-18, Q1a). The window refuses downloads, new
windows and permission requests (camera, microphone, location and others)
in Rust, and may only navigate within the app. Copying attachments into
an export, and saving a copy of one attachment, are described in
"Exports (M6)"; saving one attachment is not built yet.

### Memory, cache and the web profile

- Image previews are kept in memory only (up to 96 MiB) and dropped when
  the viewer closes. **Nothing is cached on disk.**
- The window runs in a private (incognito) web profile, so the engine keeps
  no persistent HTTP cache or web storage, and every media response tells
  it not to store the response (`Cache-Control: no-store`). In the smoke
  test on WebKitGTK 2.52.6 (container, one session with image previews and
  one attempted playback), the app's data and cache folders afterwards held
  only WebKit's empty `salt` files, an `hsts-storage.sqlite` and
  GStreamer's plugin registry, and none of them contained message text.
  Whether either engine keeps anything elsewhere, or on a desktop, or on
  WebView2, is **UNVERIFIED** (manual checklist).
- Search runs in memory; no search history is kept anywhere. The query
  and filters stay in the window's memory until the viewer closes, never
  in a URL, a setting or a log. The search and date fields ask the engine
  not to spell-check, auto-complete or auto-capitalise
  (`spellcheck="false"`, `autocomplete="off"`, `autocapitalize="off"`);
  whether each engine and the system's input methods honour that is
  **UNVERIFIED** (the milestone 5 manual checklist).
- The window title is "Viewer — OpenMessage Archive" in the viewer, never
  a conversation name.
- Hiding the screen drops the image previews kept in memory as well, and
  a preview still being decoded at that moment is answered but not kept.
  A preview request the page sent just before hiding that reaches the app
  only after the hide is decoded and kept like any other, until the
  viewer closes or the screen is hidden again (a narrow race between the
  page and the app; not tested). Idle hide closes the viewer, which drops
  everything it held.

### The privacy screen

"Hide screen" in the viewer, or Ctrl+Shift+H, hides the conversations:
media stops and loses its source, fullscreen ends, the viewer's dialogs
close, everything the viewer showed is removed from the window, the image
previews kept in memory are dropped, and an opaque screen covers the
window until you press "Show". **It hides the screen; it is not a lock.
Anyone at this computer can show it again.** Archive passwords come with
encryption in M8.

- **Hide when the window loses focus** (off by default) hides it when you
  switch away. Window previews (Alt-Tab, taskbar thumbnails) may capture a
  frame before the screen is hidden.
- **Close the viewer when idle** (off by default; 1, 5, 15 or 30 minutes
  without a key press, click, pointer movement or scroll) hides the screen
  and closes the viewer, so nothing it showed stays in the window's
  memory. "Show" opens it again at the same conversation; nothing is lost.
- **Hide this window from screen capture** (Windows only, off by default)
  asks Windows to leave the window out of screenshots and screen capture.
  Microsoft says this is not a security feature; it also blanks the window
  in screen sharing and remote help, and its effect on Windows Recall is
  **UNVERIFIED**. Linux has no such setting.

### Copying text

Text is copied only when you choose a copy action in a message's menu
(right click, the context-menu key or Shift+F10): "Copy text", "Copy text
without invisible characters", "Copy with sender and time", or "Copy link"
for a link in the text. Nothing is copied automatically, and the app sets
no timer to clear the clipboard (it would not reach clipboard managers
anyway).

- A hidden one-time code is copied as "[one-time code hidden]", and text
  the sender withdrew is never copied unless you showed it.
- "Copy link" first shows the exact link, with hidden characters made
  visible, and refuses a link that contains hidden characters.
- Attachments and images are never copied.
- **Copied text leaves the app's control.** On Windows it can enter the
  clipboard history (Win+V) and, if you turned it on, cloud clipboard
  sync to your other devices. Windows offers clipboard formats that keep
  an entry out of both, but a web page cannot set them, and the
  Rust library that could (`arboard`) brings in a licence this project
  does not allow, so M5 does not use it (D107). Clipboard managers on
  Linux may keep a history too.

### Printing, saving and dragging out

The viewer stops Ctrl+P (print, which can save a PDF), Ctrl+S, F5 and
Ctrl+R, the engine's own context menu ("Save image as") and dragging
messages or images out of the window. This works only if the engine lets
the app see the key or the action first, which is **UNVERIFIED** on both
engines. (In the smoke test on WebKitGTK 2.52.6, a right click opened the
app's own menu, and Ctrl+P opened no print dialog; whether the app or the
engine was the reason was not determined.) If an engine prints anyway, the printout shows only "Printing is
not available in the conversation viewer."

### Not prevented by the app

These paths can keep or expose what the viewer showed, and M5 does not
prevent them:

- crash dumps (systemd-coredump, Windows Error Reporting) of the web
  process or the app, which may hold message text and, during an import,
  the backup's keys;
- swap and hibernation files;
- print-to-PDF, if an engine handles Ctrl+P before the app can intercept
  it and ignores the app's print style sheet (**UNVERIFIED** per engine);
- clipboard history and cloud clipboard, once you copy text;
- a frame captured by window previews before "Hide when the window loses
  focus" has covered the screen;
- window previews (Alt-Tab, taskbar thumbnails) and screen capture,
  including Windows Recall (capture protection is an opt-in setting on
  Windows only; its effect on Recall is **UNVERIFIED**);
- other programs running as you, which can read the archive and the
  screen.

## Exports (M6)

*Status: PR A of milestone 6, the export engine, is built: the archive's
export reads, the `oma-export` crate (TXT, CSV, JSON and HTML), the
export job, its five IPC commands and the decode-helper pool for HTML
previews. **There is no Export dialog yet** (PR B, slice 6k), so no
export can be made from the app's interface; PDF, XLSX, "Save a copy" of
one attachment and leaving events out are refused, never faked ("Save a
copy" and leaving events out are assigned to slice 6k). Every
statement below was tested on Linux with synthetic data only; nothing
was run on Windows, in a browser (pages were read back by parsers), or in
a spreadsheet program. Owner decisions are those of 2026-10-19 (Q3, Q4);
Q5 is a lean awaiting the owner.*

**An export is a plaintext copy outside the app's control.**

- It holds message text, addresses (your own account's where the phone
  recorded it), conversation names and, if you chose, the original
  attachment files.
- **Everything in it is decrypted**, even when the backup was encrypted.
- The app cannot encrypt, track or delete an export, and the archive
  encryption planned for M8 will not cover exports.
- Sync clients, backup tools and antivirus may copy or read it. The app
  warns when the folder is a known sync client's folder or a network or
  virtual drive; cancelling cannot recall files a sync client already
  uploaded.

**What it contains is what the dialog chose.**

- One-time codes the phone marked are hidden, and content captured
  before an unsend or edit is left out (text, and the files of
  attachments of unsent parts), unless you chose to include them for
  that export (owner-confirmed decision Q4). That choice is never
  remembered, is confirmed against an exact count, and the viewer's
  "show earlier text" setting does not carry over.
- Codes in plain text that the phone did not mark are not detected.
- Search results that matched only hidden content are left out unless
  you include that content.
- **Read, delivery and sent status is never exported.** Raw source data
  (`attributedBody`, `message_summary_info`, `payload_data`) is not
  exported. The stored plain `text` column is exported only as the
  "alternative text", when it differs from the displayed text, and it is
  withheld whole when the message has an unsent part or earlier text, or
  a one-time code the phone marked (so it can still hold a code the phone
  did not mark, like any plain text).
- By default, numbers in the export are counters local to that export.
  Archive identifiers, which reveal how many messages the archive holds
  and roughly when they were imported, and the phone's GUIDs are
  included only if you choose (question Q5: the lean is applied and
  awaits the owner's confirmation).

**Attachments are the original files, byte for byte** (owner-confirmed
decision Q3).

- They keep their metadata, which can include location.
- They may be harmful when opened with other programs; the app never
  opens them.
- Files whose type the app cannot confirm as a plain image, audio, video
  or PDF file get `.blocked` added, unless you chose "Keep original file
  extensions" and confirmed it.
- On Windows they are marked as coming from the internet
  (`Zone.Identifier`, `ZoneId=3`) where the drive supports it. What
  Windows does with the mark is **UNVERIFIED**.

**HTML exports** contain re-encoded previews without metadata, made by
the app's decode helper processes, never the original images, and no
scripts. Their pages block network loads with a Content-Security-Policy
(whether every browser honours it from a local file is **UNVERIFIED**).
PDF exports are not built yet.

**Where files are written.** Only inside the folder you chose.
Unfinished work goes into a folder or file named `.oma-export-….partial`
(or `.partial-file`) there, never into a temporary folder elsewhere.

- On Linux, on drives with Unix permissions, the files are private to
  your account (0600, folders 0700).
- On FAT, exFAT, NTFS-3g, SMB and similar drives, permissions follow the
  drive's mount options.
- On Windows, the files inherit the folder's permissions, and the
  `.oma-export-….partial` folder is a visible folder (a leading dot does
  not hide it there).

**Not prevented by the app:**

- Windows Search and GNOME's search indexer index the Documents folder,
  which is the default start folder of the export dialogs.
- Thumbnailers (`~/.cache/thumbnails`, the Windows thumbnail cache) decode
  exported original images and videos when the folder is viewed,
  including through "Show in folder".
- The system file dialogs keep lists of recently used folders (Windows
  `OpenSavePidlMRU`, GTK `recently-used.xbel`; both **UNVERIFIED**).

**After a crash,** an unfinished export may remain in that folder. The
app offers to remove it the next time you export there, and keeps no
list of past destinations.

**The manifest** records the scope, options, counts, versions, the time
zone with its offsets, and file checksums. The checksum of an attachment
equals the archive's internal name for that file. The manifest never
records the search query, device names, the archive's label or the
folder path.

**Logs** never record export content, names or paths (see "Logging").

**Deletion is not erasure** applies to exports and their leftovers (see
"Deleting data").

### What each part decides (by slice)

What is already decided and enforced in the archive's export reads
(`crates/oma-archive/src/export/`, tested there):

- **Masked and withheld by default (owner-confirmed 2026-10-19, Q4).**
  One-time codes the phone marked, and content captured before an unsend
  or an edit that the viewer also withholds (earlier text, decoded edit
  text, the subject history of a message with an unsent part, the
  plain-column alternative text of such a message, and the files of
  attachments that belonged to unsent parts), are masked or withheld
  before anything leaves the archive code. Two separate per-export
  opt-ins release them: one for one-time codes, one for earlier content.
  Both are off by default, reset each time the dialog opens, are never
  remembered, and are each confirmed against its own exact count of
  messages: the export starts only when each confirmed count equals the
  count the preview made (a number from the app's window is never
  trusted), and stops before writing anything if a recount under the
  archive's lock is higher. A message counts as having a one-time code
  when any stored version has a marked code, and, when history is
  included, also when a phone edit entry holds edit text (its codes are
  found only when it is decoded, so it is counted without decoding: the
  count may be higher than the codes written, never lower). A reaction
  folded into its target is written whole, so it is counted like any
  other message.
  The viewer's "Show earlier text by default" setting (`revealEarlierText`)
  does **not** carry over into exports. The dialog and the confirmation
  are not built yet.
- **Only codes the phone marked are masked.** A code in plain text without
  the phone's marking is not detected, in exports as in the viewer.
- **Search results do not reveal hidden content through membership.** A
  search hit whose match lies only in masked or withheld content is left
  out of an export of search results unless the matching opt-in is on. How
  many were left out is shown to the user and written to no output, and
  no output carries anything that would give that number away: a capped
  result set holds exactly its published cap (10,000, or 1,000,000 for
  "all matches"), so the manifest does not say "capped". It carries one
  `mayBeIncomplete` flag instead, true when the set was capped, when the
  exact-text search stopped at its bound, or when any hit was left out.
  **Accepted residual:** a `true` flag on an export with far fewer
  matches than any cap suggests that some hit was left out, though never
  how many.

What the export's record core decides (`crates/oma-export`, slice 6d,
tested there; no export file is written yet):

- **No archive ids by default (owner question Q5: the lean is applied and
  awaits the owner's confirmation).** Every id an export writes is an
  export-local number: messages are numbered 1, 2, 3, … in the order of
  their archive ids within the export, so the numbers have no gaps and say
  nothing about the archive outside the export. Archive ids, the source
  GUIDs (of messages, conversations and attachments, and those a message
  refers to), a conversation's `room_name` and `group_id`, and a handle's
  `uncanonicalized_id` and `person_centric_id` are written only under the
  opt-in "Include archive and source identifiers" (JSON, CSV and XLSX;
  off). **Archive ids reveal how many messages the archive holds and
  roughly when they were imported**; GUIDs identify the messages and
  conversations in the phone's own database. The opt-in and its Review
  sentence are not built yet.
- **Read, delivery and sent state is never exported** (D94): no record has
  a field for it, and a test checks every record of the synthetic
  catalogues for those keys.
- **References outside the export carry no id.** A reply to, or a reaction
  on, a message that is not in the export says "not in this export"
  without naming it; the other conversations a message is in are counted,
  not named, when they are not exported.

What the export's files core decides (`crates/oma-export`, slice 6e1,
tested on Linux with test writers; no export can be started from the app
yet, and nothing here was run on Windows):

- **Nothing is written outside the folder the export goes to.** A bundle
  is written into a folder named `.oma-export-<32 hex digits>.partial`
  inside the chosen folder, and a single file into a file named
  `.oma-export-<32 hex digits>.partial-file` beside it. Those names carry
  no content. No temporary folder of the system is used.
- **Private by default on Linux:** the export's folders are created 0700
  and its files 0600, readable only by the account that made them (on
  file systems that keep Unix permissions). On Windows the files take the
  chosen folder's permissions.
- **An unfinished export is plaintext on disk.** Cancelling or a failure
  removes what the export wrote; if that removal fails, the folder stays
  under its `.oma-export-….partial` name. After a crash, the unfinished
  folder or file stays where it was. The app finds it again only when the
  next export goes to the same folder (it keeps no list of past
  destinations); entries younger than five minutes, or held by a running
  export, are never removed (a running export renews its marker's time
  after every batch; on a network folder whose locks another computer
  cannot see, a single batch that takes over five minutes is the
  residual). If removing a cancelled export's own files fails, its marker
  stays, so the folder is offered again by a later export to that folder. On Windows a name starting with a dot is not
  hidden.
- **A published export never replaces anything** without the app's own
  confirmation, and then only a regular file of the same name; a bundle
  takes the next free name (` (2)` … ` (99)`).
- **`manifest.json`, `README.txt` and `SHA256SUMS` never contain** the
  words of a search, the number of search hits left out, device facts,
  the archive's name or id, or any path outside the export: the manifest's
  structure has no place for them. The manifest does list the export's
  own file paths, and an attachment's file name there is built from the
  name the phone recorded (hidden characters removed, cut to fit).
- **Attachment file names** keep their extension only when it is on a
  short list of passive types and matches what the file's bytes are;
  every other copied file gets `.blocked` appended (owner decision Q3b),
  unless "Keep original file extensions" is chosen and confirmed.

What the attachment copy and the TXT writer do (slice 6e2, tested on
Linux through the export pipeline on the synthetic catalogues; no export
can be started from the app yet; nothing was run on Windows):

- **Which files are copied** (owner-confirmed decision Q3a): by default,
  bundle exports with HTML or JSON, and whole-archive exports, copy the
  original attachment files; a single file carries names only; CSV, XLSX
  and TXT bundles copy files only when the box is ticked. A names-only
  export reads no attachment file.
- **A copy is the stored file, byte for byte, metadata included** (GPS
  coordinates in a photo too): nothing is converted or stripped. Its
  checksum in `manifest.json` and `SHA256SUMS` is the one the archive
  stored, checked while the file was copied. A stored file that no longer
  matches its checksum is not copied, and the record says so.
- **Attachments of unsent parts are not copied** unless the export
  includes content captured before an unsend (owner decision Q4, confirmed
  against an exact count); a test checks that their bytes appear nowhere
  in the export otherwise.
- **Mark of the Web** (owner-confirmed decision Q3c): on Windows, each
  copied file gets a `Zone.Identifier` stream marking it as from the
  Internet (`ZoneId=3`), so Windows may warn before opening it. What
  Windows actually does with the mark is **UNVERIFIED**, and the code has
  not been run on Windows yet. Nothing is written on Linux, and a volume
  without such streams (FAT, exFAT, some network shares) gets no mark
  (counted in `manifest.json` as `motwNotWritten`).
- **TXT files are plain text that a terminal or an editor shows as
  written.** Control characters, and in names and addresses also line
  breaks and square brackets, are written as codes such as `⟨U+001B⟩`;
  characters that do not show when printed are written as codes too
  unless the option is turned off (in names and addresses they always
  are). This keeps a message from faking another item or a sender line,
  but it is not a guarantee about every viewer.

What the JSON and CSV writers do (slice 6f, tested on Linux through the
export pipeline on the synthetic catalogues; no export can be started
from the app yet; nothing was run on Windows, in Excel or in
LibreOffice):

- **JSON keeps every character of the recorded text** (escaped where JSON
  needs it). Hidden characters are not marked; each text carries flags
  saying it holds direction or invisible characters. A JSON export also
  holds copies of its two schemas (no content).
- **CSV "for spreadsheets" (the default)** starts a cell that a
  spreadsheet could run as a formula (a leading `=`, `+`, `-`, `@`, their
  full-width and small forms, also after spaces and invisible
  characters, or a leading tab or line break) with an apostrophe, and
  names the column in the row's `neutralised_columns`; every E.164
  address starting with `+` is prefixed so. Control characters are
  written as codes. This follows OWASP's "CSV Injection" advice; OWASP
  also notes that a spreadsheet may turn the formula back on after the
  file is saved and opened again, which is **not tested** here, nor is
  how Excel or LibreOffice treat the invisible characters.
- **CSV "for data tools"** writes values raw, controls included, except
  the export's own marker character: opening those files in a
  spreadsheet can run formulas hidden in messages, and printing them in
  a terminal can run terminal escape codes. The dialog will say so
  (slice 6k).
- **With "Include archive and source identifiers" off** (the default),
  the CSV's `source_*` columns are present and empty, and JSON's
  `source` says the option left them out.

What the HTML writer does (slice 6g, tested on Linux through the export
pipeline on the synthetic catalogues and read back by two HTML parsers;
no export can be started from the app yet; no page was opened in a
browser in this slice, and nothing was run on Windows):

- **Pages are static and hold no script.** Each page starts with a
  Content-Security-Policy that forbids loading anything except the
  images embedded in it, and every message, name and address is written
  as escaped text. The policy is a second line of defence; whether any
  browser (WebKitGTK, WebView2, Firefox, Chrome) honours it for a page
  opened from a local file is **UNVERIFIED**; it is on the M6 manual
  checklist added in PR B (slice 6l).
- **Images in HTML are previews the app re-encoded, never the original
  files**: thumbnails embedded in the page and, with "Larger previews",
  larger copies beside it under `html/previews/`. They come only from
  the decode helper's renditions and are checked before they are
  written. Original files, when copied, are named by their path but not
  linked, so opening a page never hands sender bytes to the browser's
  image, audio or video decoders. Web addresses in messages are text,
  not links. The renditions come from the export's own decode helpers
  (slice 6h2, below).
- **Hidden characters are shown as framed codes** (`U+202E`), always in
  names, addresses and file names, and in message text unless the
  markers option is off; control characters are always shown so.
- **Titles are generic** ("Messages export — conversation 1"): a
  conversation's name appears only in the page body, not in a browser's
  tab, history or window title.

### The export job and its commands (slice 6h1)

What the shell does, tested on Linux without a window
(`src-tauri/src/exports/`); nothing here has run on Windows or in a
real window:

- **The webview never names a path.** The destination comes from a
  native save dialog or folder picker that Rust opens; Rust keeps the
  folder in memory, bound to the export's draft, and the webview gets
  only the name the export will have, closed warning codes and a free
  space bucket. The dialog's title names every sensitive option of the
  draft ("includes one-time codes", "includes text captured before an
  unsend or edit", "keeps risky file extensions"). No destination is
  remembered: `settings.json` gains nothing.
- **The checks run on the folder's real path.** A folder inside an iPhone
  backup, Apple's `MobileSync/Backup` or iMazing's `Backups`, or inside
  the archive folder, is refused, as is one the export cannot write to.
  A folder other accounts can change (Linux), a sync client's folder, a
  network or virtual drive, and too little free space are warnings the
  user must confirm; the checks run again when the export starts.
- **A draft holds the request in memory only**, for at most 30 minutes:
  the plan, the counts, the zone table, the destination, and for a
  search scope the search request (with its query) and its result set.
  The query is never sent back by the webview; the job drops it as soon
  as the search has been re-run and re-checked. Closing the viewer, the
  privacy screen and idle hide discard the draft and stop its count.
- **Consent is checked twice.** Starting refuses when the archive changed
  since the counts were taken, and the job counts again under the
  archive's lock and stops before writing anything when more messages
  would be revealed than the user confirmed.
- **The job holds the archive's lock**, so no import or verify, in this
  copy of the app or another, changes the archive while it is read; it
  keeps running if the viewer closes.
- **What the app keeps after an export:** the folder of each finished
  export, in memory only, so "Show in folder" can open it; the app
  forgets it when it closes. Unfinished exports are removed on cancel or
  failure; one a crash leaves is offered for removal the next time the
  same folder is chosen.
- **Not available yet, and refused rather than faked:** PDF and XLSX,
  saving one attachment, and leaving events out.

### Destinations and previews (slice 6h2)

Tested on Linux without a window (`src-tauri/src/exports/`,
`src-tauri/tests/`); nothing here has run on Windows or with a native
dialog:

- **A chosen folder is a token, not a path.** Each choice returns a
  random token bound to the draft; starting an export must carry the
  token of the latest choice, so warnings confirmed for one folder can
  never start an export into another. The path itself stays in Rust.
- **Previews are made by the export's own decode helpers**, separate
  from the viewer's: at most two helper processes (one on a two-core
  machine), each the app's own binary started for that purpose, given
  only the verified bytes of one image at a time, never a path or a
  name. When the files are copied, the bytes come from the copy itself;
  otherwise the file is read once, with its checksum checked. One decode
  gives both the thumbnail and, with "Larger previews", the larger copy.
  Images over 64 MiB, and any the helper refuses, become "Preview
  unavailable" cards; after three helper crashes in a row, previews are
  off for the rest of that export and the number skipped is recorded in
  the manifest. Cancelling kills the helpers.
- **Helper error output is never kept.** What a helper writes to its
  error stream is read and thrown away; only its length is logged, as a
  size bucket.

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
  CDN is used. From M5, images and media may also load from the app's own
  `oma-media` scheme (`oma-media:` on Linux, `http://oma-media.localhost`
  on Windows, where WebView2 serves custom schemes under that name); the
  app answers it itself and it reaches no network.
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
| Status | Unlocking and checking: M2. Checking that messages can be read: M3. Importing into an archive: M4. | Planned, M8. The likely approach is SQLCipher for the database plus encrypted attachment storage. Until then, **archives are stored unencrypted** and rely on OS account security and full-disk encryption (BitLocker, LUKS). |

Importing an encrypted backup produces a readable archive. The import
dialog and the confirmation step say so on every import (M4). Until M8,
full-disk encryption is the protection to rely on.

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
- An archive's own files are treated as untrusted too (M4), since anyone
  who can write to the archive folder can change them. An archive is opened
  read-only first; its schema must match the app's own (checked by
  fingerprint) before any table is read; stored file names are parsed as 64 hex digits
  before a path is built from them; store files and folders are never
  followed through links; and nothing is written until those checks pass.
- **Attachments are never opened, executed or saved by M5** (owner-confirmed
  2026-10-18, Q1a). Copies written into an export are described in
  "Exports (M6)".
- **File names written by an export** (M6) are never the untrusted name
  alone: each starts with the file's own ordinal (`000045-`), has hidden
  characters removed, is passed through the app's file-name sanitiser and
  cut to a length budget computed from the destination path (to the
  ordinal alone if the folder is very deep), and gets `.blocked` appended
  unless its extension is on the passive allowlist and matches the
  file's bytes.
- **Previews and playback (owner-confirmed 2026-10-18, Q1b).** Images are
  decoded by memory-safe Rust decoders in a separate helper process and
  re-encoded before the window sees them. Audio and video are played by
  your system's media codecs (GStreamer on Linux, Media Foundation on
  Windows) from the original file. They play only after a structural
  check, and by default only after you confirm for that file. A file
  crafted by a sender could exploit a flaw in those codecs; the app cannot
  prevent that. Playback can be turned off. Before an image is decoded or
  a file is played, the app re-reads the whole file and checks it against
  the checksum recorded at import: **that check protects against
  corruption only**, not against a file that was harmful when it was
  imported. The type a message records for an attachment is never trusted;
  the app looks at the bytes, and treats anything it does not recognise as
  a file card. *Status: the content check, the structural check, the
  checksum readers and the image re-encoding exist from M5 slice 5d; the
  helper process, the media protocol and the hardened window from slice
  5f; the screens that use them come in M5 PR B. See "The viewer (M5)".*
- **HEIC images (owner-confirmed 2026-10-18, Q2).** Official builds, and
  every build CI produces, **do not decode HEIC or HEIF**: such an
  attachment is shown as a file card. Decoding HEIC means implementing
  HEVC, which is covered by patent pools that license "HEVC software" for
  royalties, and no open-source decoder licence grants those patent
  rights. People who build the app themselves can turn on the optional
  `heic` Cargo feature of `crates/oma-archive`, which uses the pure-Rust
  `heic-rs` decoder (MIT OR Apache-2.0, no C, no `unsafe`) inside the same
  helper process, under the same input, side and (halved) decoded-size
  limits as other images. `heic-rs` offers no allocation limit, so its
  internal buffers are bounded only by its own code and the helper
  process (a HEIF that makes it allocate too much ends the helper, which
  counts as a crash). The app's package does not forward the feature, so
  the crate's feature is named: `npx tauri build --features
  oma-archive/heic` (see `development.md`). **Doing so is at the
  self-builder's own legal risk**; the project gives no assurance about
  patents (see `dependencies.md`).
- Message text is rendered as text, never as HTML. The UI does not use
  `v-html` with untrusted content, and HTML exports escape everything
  (see "Exports (M6)" and "What the HTML writer does" above).

## Logging

Diagnostic logs record operation kinds, states and counts. They never
record message bodies, contact names, phone numbers, email addresses,
attachment contents or passwords. From M2 they also never record paths,
folder names, device names, UDIDs, backup file names or identifiers, or
SQLite message text; automated tests check this over every synthetic
fixture. From M3 they also never record any string from the Messages
database, GUIDs, timestamps, the date range, attachment paths or panic
messages. From M4 they also never record natural keys, fingerprints,
content digests, ranks, attachment hashes or storage names, source rowids
and cursors, an archive id or label, device names or product types, any
path (archive folder names included), display text, any value from the
archive's issue or history tables, or SQLite error text (only the primary
code). From M5 they also never record search or filter text, ids, media
tokens, media URLs, byte ranges or exact file sizes (sizes are bucketed).
From M6 the export job and commands log kinds, option flags, buckets,
outcomes and elapsed time only: never a destination, an export's file or
folder name, a zone name, a draft or destination token, a query, an
exact count or size, or content; a decode helper's error output is
counted, never logged. `src-tauri/tests/log_hygiene.rs` checks this over
the export commands' work and the TXT, CSV, JSON and HTML formats (with
previews from the real decode helper) on the synthetic hostile and media
archives, exported into a folder whose name is a sentinel. The persistent diagnostic log file is deferred to M8 (D71, a lean
awaiting the owner). Set `OMA_LOG=debug` for more detail. That
setting changes how much is recorded, not which kinds of data.

## Deleting data

M4 offers no way to remove an archive, or a message from one. When it
comes, removing an archive from the application will remove only the
application's reference to it, and deleting an archive's files will be a
separate, explicitly confirmed action. Until then you can delete an
archive's `a<32 hex>` folder yourself while the app is closed. Some limits
apply to deletion on modern storage:

- **SSDs** remap writes (wear levelling). Overwriting a file does not
  guarantee the old blocks are erased, and the application does not claim
  "secure deletion".
- **Journaled and copy-on-write filesystems** (NTFS, ext4 with data
  journaling, btrfs, ZFS) and **snapshots** (Windows shadow copies,
  Timeshift, btrfs snapshots) can retain old copies.
- **SQLite** can leave deleted content in free pages and in the WAL until
  it is vacuumed. Archives run with `secure_delete` (and the search index
  with FTS5 `secure-delete`), which zero freed pages, but not old page
  images in the `-wal`. A compaction step will come with deletion.

Full-disk encryption is the effective protection against recovery of
deleted data. Deleting an archive cannot replace it.
