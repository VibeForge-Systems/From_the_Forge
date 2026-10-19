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
in Rust, and may only navigate within the app. Saving a copy of an
attachment is M6.

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
  2026-10-18, Q1a). Saving a copy of an attachment is M6.
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
  `v-html` with untrusted content, and HTML exports will escape everything.

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
The persistent diagnostic log file is deferred to M8 (D71, a lean
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
