# Backup formats and device tooling: research brief

Researched on 2026-10-17, before any backup code was written. The aim is
to base compatibility decisions on evidence. Every claim links to its
source. **UNVERIFIED** marks claims that could not be confirmed from a
primary source. Treat them as hypotheses for hardware testing, not facts.

This document records what was found. The [compatibility matrix](compatibility.md)
records what has been tested and works.

## Device communication and backup creation (Milestone 7)

### libimobiledevice

- 1.4.0 was released on 2025-10-10 and is the first release since 1.3.0
  (2020). Its notes include "Fixes for idevicebackup2" and iOS 17+
  developer disk image support.
  ([release](https://github.com/libimobiledevice/libimobiledevice/releases/tag/1.4.0),
  [NEWS](https://github.com/libimobiledevice/libimobiledevice/blob/master/NEWS))
- The library and its tools are **LGPL-2.1-or-later** according to the
  per-file headers, `tools/idevicebackup2.c` included. The repository's
  `COPYING` file holds the GPLv2 text, and `COPYING.LESSER` holds the LGPL.
  ([README](https://github.com/libimobiledevice/libimobiledevice/blob/master/README.md),
  [idevicebackup2.c](https://github.com/libimobiledevice/libimobiledevice/blob/master/tools/idevicebackup2.c))
- Linux distributions do not all ship the same version. Ubuntu 24.04 and
  Debian 12 ship 1.3.0. Debian 13 and Ubuntu 25.10 ship a 2025 snapshot.
  Arch, Fedora rawhide, openSUSE Tumbleweed and Ubuntu 26.04 ship 1.4.0.
  ([Repology](https://repology.org/project/libimobiledevice/versions))
- `mobilebackup2` is still a classic lockdown service. The iOS 17
  RemoteXPC/RSD tunnels are required for developer services, not for backup.
  pymobiledevice3 starts `com.apple.mobilebackup2` over lockdown with an
  escrow bag.
  ([pymobiledevice3 mobilebackup2.py](https://github.com/doronz88/pymobiledevice3/blob/master/pymobiledevice3/services/mobilebackup2.py),
  [tunnel guide](https://doronz88.github.io/pymobiledevice3/guides/ios17-tunnels/))
- Whether `idevicebackup2` works with iOS 17, 18, 26 and 27 is
  **UNVERIFIED**. Open issues report service-start failures on iOS 17.3,
  receive errors (-4) and timeouts.
  ([issues](https://github.com/libimobiledevice/libimobiledevice/issues?q=is%3Aissue+idevicebackup2))
- Since iOS 16, the device asks the user to confirm with the passcode when
  a backup starts. The UI must tell the user to unlock the phone and
  confirm. ([Apple](https://support.apple.com/en-us/109054))

### Linux: usbmuxd

- usbmuxd 1.1.1 (2020) is the latest tag; distributions ship git snapshots
  of it. It is GPL-2.0-or-later/GPL-3.0 and runs as a separate system daemon
  that applications reach over a socket, so nothing is linked into this
  application. libusbmuxd is LGPL-2.1.
  ([usbmuxd](https://github.com/libimobiledevice/usbmuxd),
  [libusbmuxd](https://github.com/libimobiledevice/libusbmuxd))
- Pair records are stored in `/var/lib/lockdown`.
  ([userpref.c](https://github.com/libimobiledevice/libimobiledevice/blob/master/common/userpref.c))

### Windows: Apple Mobile Device Service

- On Windows, clients reach the device through Apple's
  `AppleMobileDeviceService.exe`, which listens on `127.0.0.1:27015`. The
  `USBMUXD_SOCKET_ADDRESS` environment variable can override the address.
  ([usbmuxd-proto.h](https://github.com/libimobiledevice/libusbmuxd/blob/master/include/usbmuxd-proto.h),
  [netmuxd](https://github.com/jkcoxson/netmuxd))
- The service and its USB driver come with iTunes or the Microsoft Store
  "Apple Devices" app. **This is a proprietary dependency.** It is needed
  only for direct device backup, never for importing an existing backup.
  ([Apple](https://support.apple.com/guide/devices-windows/install-the-apple-devices-app-mchl5ded2763/windows))
- Community reports say the service can be missing after installing Apple
  Devices (**UNVERIFIED** by Apple).
  ([discussion](https://discussions.apple.com/thread/255746335))
- Pair records are stored in `%ProgramData%\Apple\Lockdown`.
- Driver alternatives exist but are not recommended. netmuxd (LGPL-2.1)
  can bind libusb0 through libwdi, but this needs administrator rights,
  conflicts with iTunes, and needs test signing on ARM64.

### Rust and other clients

| Project | License | Backup support | Assessment |
|---|---|---|---|
| [`idevice`](https://github.com/jkcoxson/idevice) 0.1.68 | MIT | Yes: a `mobilebackup2` module with backup, restore, list and password change, plus usbmuxd (Unix socket, or TCP 27015 on Windows) and pairing | **Leading candidate for M7.** Pure Rust, and passwords stay in the process. Pre-1.0 with breaking changes on each point release, so pin it exactly. It does not write `Info.plist`; the caller must. |
| `rusty_libimobiledevice` 0.2.3 | LGPL-2.1+ | Via libimobiledevice | Its author recommends `idevice` instead |
| `idevicebackup2` 1.4.0 | LGPL-2.1+ | Yes | Possible as an optional, user-installed external tool. Windows builds are only community or CI artifacts, and MSYS2 is still on 1.3.0. |
| [pymobiledevice3](https://github.com/doronz88/pymobiledevice3) 11.33 | **GPL-3.0** | Yes, including encrypted backups | Behavioural reference only. It cannot be bundled or copied. |

**Passwords.** `idevicebackup2` takes the backup password as a command-line
argument, through `BACKUP_PASSWORD`, or interactively with `-i`. Command
lines can be read from the process list, and the environment can be read
by the same user. In-process handling with `idevice` avoids both. Creating
a backup needs no password, because the device encrypts with its stored
password. A password is needed to read an encrypted backup, and to enable
or change encryption.

## Backup layout (Milestone 2)

The format used since iOS 10 is called "3.x". The older "2.4" format, with
`Manifest.mbdb`, belongs to iOS 9 and earlier.
([Rich Infante](https://www.richinfante.com/2017/3/16/reverse-engineering-the-ios-backup))

| File | Content | Use |
|---|---|---|
| `Info.plist` | Written by the host from lockdown values: Product Version, Build Version, Device Name, Unique Identifier, Serial Number, Last Backup Date, Phone Number | Display, device identity |
| `Status.plist` | `SnapshotState`, `IsFullBackup`, `Version`, `Date`, `UUID` | **Anything other than `SnapshotState = finished` is an incomplete backup** |
| `Manifest.plist` | `IsEncrypted`, `BackupKeyBag`, `ManifestKey`, and `Lockdown` (with ProductVersion and others) | Encryption detection, key material, iOS version |
| `Manifest.db` | SQLite `Files(fileID, domain, relativePath, flags, file)` plus `Properties`. It is encrypted when the backup is. | The file index |
| `xx/<fileID>` | File contents, stored at `<first two hex characters>/<fileID>` | — |

- `fileID = SHA1(domain + "-" + relativePath)`. Verified locally:
  `SHA1("HomeDomain-Library/SMS/sms.db") = 3d0d7e5fb2ce288813306e4d4636395e047a3d28`.
- `flags`: 1 = file, 2 = directory, 4 = symlink.
  ([MVT manifest.py](https://github.com/mvt-project/mvt/blob/main/src/mvt/ios/modules/backup/manifest.py))
- `file` is an NSKeyedArchiver `MBFile` containing `Size`,
  `ProtectionClass`, `EncryptionKey`, `Birth`, `LastModified`, `Mode` and
  `Target`. **Correction (M2):** the permissive parsers named here
  earlier, `ns-keyed-archive` and `nskeyedunarchiver`, were judged not
  suitable: both depend on `plist` and bring dependencies the crate does not
  need, and neither reads under the budgets milestone 2 requires.
  `oma-backup` uses a hand-written resolver of about 150 lines over its own
  budgeted plist value (`mbfile.rs`).
- Whether WAL files for sms.db appear in backups is **UNVERIFIED**. The
  importer will look for `-wal` and `-shm` fileIDs and apply them to a copy
  if they are present.

**Decision:** M2 supports the 3.x format (iOS 10 and later) only. An iOS
9-era backup is detected by `Manifest.mbdb` and reported as unsupported. It
is never partially read.

### Encrypted backups

The steps below follow [iphone_backup_decrypt](https://github.com/jsharkey13/iphone_backup_decrypt)
(MIT, 0.11.2), which may be ported with attribution. The
[andrewdotn StackOverflow answer](https://stackoverflow.com/a/13793043) is
CC BY-SA (the version depends on the revision date; not checked). It is a
reference only; do not copy its code. Milestone 2's Rust code was written
from this description, RFC 3394 and RFC 8018, not translated from any
Python; iphone_backup_decrypt was used only as a black-box oracle.

1. The keybag in `BackupKeyBag` is a TLV stream: a 4-byte tag, a 4-byte
   big-endian length, then the value. Tags are TYPE, UUID, WRAP, SALT,
   ITER, DPSL and DPIC, then per class UUID, CLAS, WRAP, KTYP and WPKY.
   That the header tags always come before the first class record is
   **UNVERIFIED**: no primary source states the order. The oracle accepts
   header tags anywhere, and so does the Rust parser (see D18 below).
2. Derive the passphrase key with PBKDF2-HMAC-SHA256(password, DPSL, DPIC,
   32), then run PBKDF2-HMAC-SHA1(result, SALT, ITER, 32) on that.
   **Iteration counts come from the file and must be capped.** The reference
   caps are DPIC ≤ 20,000,000 and ITER ≤ 1,000,000, so a hostile backup
   cannot hang the importer. It is **UNVERIFIED** that the two-stage scheme
   arrived in iOS 10.2; the only source is secondary
   ([Practical Mobile Forensics](https://www.oreilly.com/library/view/practical-mobile-forensics/9781838647520/ecf9d6d1-707b-457c-b884-39f9d1717eca.xhtml)).
3. Unwrap the class keys with RFC 3394 AES key unwrap and check the IV
   `A6A6A6A6A6A6A6A6`. A wrong password shows up as an integrity failure.
4. `ManifestKey` is a little-endian u32 protection class followed by a
   wrapped key. Decrypt `Manifest.db` with AES-256-CBC and an all-zero IV,
   into a temporary copy.
5. Each file's key is `MBFile.EncryptionKey` minus its 4-byte prefix,
   unwrapped with that file's class key. Decrypt with AES-256-CBC and a
   zero IV, streaming in chunks, and validate the PKCS#7 padding. The
   recorded `Size` can differ from the decrypted size. Log the difference
   and never truncate silently.

All the primitives come from RustCrypto under MIT OR Apache-2.0: `aes`,
`cbc`, `aes-kw`, `pbkdf2`, `sha1`, `sha2` and `zeroize`. Before adopting
them, check that they all resolve to the same `cipher` and `digest` majors.

## Milestone 2 findings and decisions

Everything in this section is implemented in `crates/oma-backup` and tested
**only against synthetic fixtures** (see [compatibility.md](compatibility.md)).

### Where backups are on Windows

| Root | Source | Status |
|---|---|---|
| `<home>\Apple\MobileSync\Backup` (Apple Devices app, iTunes from the Microsoft Store) | Apple's support page ([support.apple.com/en-us/108809](https://support.apple.com/en-us/108809), dated 2026-01-14) names `%USERPROFILE%` only. The subfolder comes from the iphone_backup_decrypt docstring and third-party guides. | Subfolder **UNVERIFIED by Apple** |
| `<roaming>\Apple Computer\MobileSync\Backup` (iTunes desktop installer) | Apple names `%AppData%` only; the subfolder comes from the same third-party sources | Subfolder **UNVERIFIED by Apple** |
| `<roaming>\iMazing\Backups` (iMazing's own location) | Observed on a Windows 11 machine running iMazing 3.x, 2026-10-18: iMazing's Backup Location screen offers "iTunes Backup Location" (`C:\Users\<user>\AppData\Roaming\Apple Computer\MobileSync\Backup`) and "iMazing Backup Location" (`C:\Users\<user>\AppData\Roaming\iMazing\Backups`), and the iMazing location was the default there. Note the plural `Backups`: an older iMazing guide spells it `Backup`; the observed screen is taken as authoritative. | Observed on one machine. The location is configurable in iMazing, so other paths exist; those are found only when the user adds the folder. |

- All three roots are always scanned when their base folder is known,
  because a machine can have several of these apps.
  The shell passes Tauri's `home_dir()` and `data_dir()`; that `data_dir()`
  is the roaming AppData folder on Windows is **UNVERIFIED** until checked on
  Windows 11.
- MSIX-virtualised locations (`%LOCALAPPDATA%\Packages\AppleInc.iTunes_*`)
  are not scanned; whether Store iTunes ever writes there is **UNVERIFIED**.
- Users relocate backups with junctions; links are followed at the root
  level only. That std's `FileType::is_symlink()` on Windows covers
  junctions but not cloud-file placeholders is **UNVERIFIED** until the
  Windows tests run.
- **Linux has no standard location.** `idevicebackup2` and pymobiledevice3
  both take a required output directory and write `<dir>/<UDID>/`. Users add
  that directory with "Add backup folder…".
- A folder is recognised only by its marker files (at least two of
  Info.plist, Manifest.plist, Status.plist, Manifest.db), never by its name.
- **`iMazing.Versions` is never listed.** iMazing keeps archived snapshots
  of its backups in an `iMazing.Versions` folder at the backup location
  ([iMazing guide](https://imazing.com/guides/understanding-the-imazingversions-folder-in-imazing-backup-locations)).
  Its internal layout is **UNVERIFIED**, so a root scan skips a direct child
  with exactly that name, even if it holds backup marker files. This is the
  one exception to "never by its name".
- **The archive folder may not be inside iMazing's default store.** Like
  `MobileSync/Backup`, a path with consecutive components `iMazing` and
  `Backups` (case-insensitive) is refused as an archive location and as a
  staging location (`oma_fs::is_within_imazing_backups`). A store the user
  moved elsewhere is not recognised by name; the backups inside it are still
  refused by their marker files.

### Classification rules

`inspect` applies these in order; the first match wins:

1. `Manifest.mbdb` present and `Manifest.db` absent: unsupported (format 2.4).
2. `Manifest.plist` missing, not a regular file, unreadable, over its cap or
   over its budget: unreadable.
3. `Status.plist` missing: incomplete.
4. `Status.plist` not regular, unreadable or over budget: unreadable.
5. `SnapshotState` other than exactly `finished`: incomplete. This follows
   `idevicebackup2`'s `mb2_status_check_snapshot_state`. The other values
   are **UNVERIFIED** (`uploading` was observed once, below), so the UI
   shows the raw (sanitised) string.
6. `IsEncrypted` absent or not a boolean: unsupported.
7. `Manifest.db` absent or not regular: unreadable.
8. Encrypted and no `BackupKeyBag`: unsupported. Applies to encrypted
   backups only; whether unencrypted backups always carry a keybag is
   **UNVERIFIED** (an unencrypted backup without one gets a note).
9. Encrypted and no `ManifestKey`: unsupported.
10. Not encrypted, but Manifest.db lacks the SQLite header: unreadable.
11. Encrypted, but Manifest.db starts with the SQLite header: unreadable.
12. Encrypted: needs a password. 13. Otherwise: ready.

A `Snapshot/` folder only adds a note; its meaning is **UNVERIFIED**.
`Status.plist` `Version` is shown but not used to classify.

**Observed: an interrupted iMazing backup.** On the same Windows 11 machine
(iMazing 3.x, an iPhone 13 Pro on iOS 26.6.2, 2026-10-18), a backup that did
not complete left a **binary** `Status.plist` with exactly these keys, and a
byte-identical `Status.plist.backup` beside it (identifiers and the date are
not recorded here):

| Key | Type | Value |
|---|---|---|
| `IsFullBackup` | boolean | `true` |
| `Version` | string | `"3.3"` |
| `UUID` | string | a UUID |
| `Date` | date | the backup date |
| `BackupState` | string | `"empty"` |
| `SnapshotState` | string | `"uploading"` |

Rule 5 classifies it as incomplete, and the UI shows `uploading`.
`Status.plist.backup` is not read and causes no error. `BackupState` is
carried as raw text only; what `"empty"` means is **UNVERIFIED**, and
nothing classifies on it. A synthetic copy of this shape is tested in
`crates/oma-backup/src/inspect.rs`
(`an_interrupted_imazing_backup_is_incomplete`).

### Property lists

| File | Keys read | Cap | Over the cap or budget |
|---|---|---|---|
| `Info.plist` | `Device Name`, `Display Name`, `Product Type`, `Product Name`, `Product Version`, `Build Version`, `Last Backup Date` | 64 MiB | Not fatal: device details are omitted, with a note |
| `Status.plist` | `SnapshotState`, `IsFullBackup` (meaning **UNVERIFIED**; shown raw), `Version`, `Date`, `BackupState` | 1 MiB | Unreadable |
| `Manifest.plist` | `IsEncrypted`, `BackupKeyBag`, `ManifestKey`, and `Lockdown.ProductVersion`, plus `Lockdown.ProductType`, `BuildVersion` and `DeviceName` (those three key names **UNVERIFIED**) | 32 MiB | Unreadable |

The caps are provisional; real file sizes for large backups are
**UNVERIFIED**. **Never stored:** `Unique Identifier`, `Target Identifier`,
`Serial Number`, `Phone Number`, `IMEI`, `ICCID`, `MEID`, `GUID`,
`Installed Applications`, `Applications`, `iTunes Files`, `iTunes
Settings`. These root keys are skipped while streaming: the stream reader
still decodes their values, which are counted against the budgets and
discarded, and no struct has a field for them. Skipping applies to root keys
only, so Manifest.plist's `Lockdown` dictionary is built whole and only the
four keys above are kept (whether `Lockdown` holds identifiers is item 14,
**UNVERIFIED**). A key of the wrong type is ignored with a note, never
coerced.

**Reference bombs.** plist 1.10.1's binary reader rejects only references
already on its stack, not a DAG in which one object is referenced many
times, and building a `plist::Value` deep-copies every reference. So backup
plists are read with a budgeted event-stream reader: at most `4 × len +
1024` events, `2 × len` cumulative string and data bytes, and depth 32.

### Keybag (D18: fail closed only on what decryption uses)

- Records are `tag[4] ‖ len[4, big-endian] ‖ value`. The first `UUID` is
  the header's; every later `UUID` starts a class record. A header-only tag
  (`VERS`, `TYPE`, `HMCK`, `SALT`, `ITER`, `DPWT`, `DPIC`, `DPSL`) that
  appears after a class record has started still goes to the header, with
  a note, as the oracle does; otherwise a keybag with late `DPIC`/`DPSL`
  would be wrongly called single-stage. Whether real keybags ever do this
  is **UNVERIFIED**.
- `TYPE` is masked with `0x3FFF_FFFF`; a value other than 1 is a note (that
  local backups are TYPE 1 is **UNVERIFIED**). `CLAS` above 0xF is a note.
  Unknown tags, duplicate tags (last wins) and trailing bytes shorter than a
  record header are notes. A record that runs past the end is fatal
  (`Truncated`); two class records with the same `CLAS` are fatal.
- `DPSL` and `DPIC` absent: unsupported (single-stage KDF). No usable
  class: unsupported, refused before any derivation. A usable class has a
  valid `CLAS`, `WRAP & 2`, `KTYP` 0 (or absent) and a 40-byte `WPKY`;
  without one no key would be unwrapped, so any password would be accepted
  and the password would go unchecked. An absent `KTYP` is treated
  as 0 (AES), as the oracle does (**UNVERIFIED**); a class with `KTYP` 1
  (Curve25519) is unusable, and only files that need it fail.
- Iteration caps, checked before any derivation: `1 ≤ DPIC ≤ 20,000,000`
  and `1 ≤ ITER ≤ 1,000,000` (the oracle's caps; advisory
  GHSA-63p2-3674-32xw). Apple's Platform Security guide (topic
  sec6483d5760) states 10 million iterations for PBKDF2; `ITER` 10,000
  comes only from the oracle's README. Real values are **UNVERIFIED**.
- The password is the UTF-8 encoding exactly as entered (no trimming or
  normalisation), as the oracle does. Apple's handling of non-ASCII
  passwords is **UNVERIFIED**.

Where the Rust code deliberately differs from the oracle
(iphone_backup_decrypt 0.11.2), observed with the committed fixtures:

| Behaviour | Oracle | oma-backup |
|---|---|---|
| `TYPE` other than 1 | Accepts 0–3; raises above 3 | Masked; a note |
| `WPKY` of the wrong length on an unused class (`enc-wpky39-unused`) | Fails to unlock (`IncorrectPassphraseError`) — this contradicts the design's expectation that the oracle checks length only when used | Unlocks, with a note |
| Trailing bytes under 8 | Stops quietly | A note |
| A record running past the end (`hostile-keybag-truncated`) | `IncorrectPassphraseError` | `Truncated` (fatal) |
| Duplicate `CLAS` (`enc-dup-clas`) | Unlocks; sms.db extraction fails | Malformed (fatal) |
| No passphrase-wrapped class (`hostile-wrap-device-only`) | Raises `ValueError` — the design expected a false success | Unsupported |
| EncryptionKey shapes | Only UID → `NS.data` | Also UID → raw Data and inline Data (both Rust-only, shape **UNVERIFIED**) |
| `Size`, `Mode`, `ProtectionClass` missing | Required | Optional; counted |

### ManifestKey and file records

- `ManifestKey` must be 44 bytes: a 4-byte little-endian class, then a
  40-byte wrapped key. A wrong first class key means a wrong password (or a
  damaged keybag); a later class failing means a damaged keybag.
- `MBFile.EncryptionKey` must be 44 bytes; the first 4 are ignored, as the
  oracle does. That they are the little-endian protection class is
  **UNVERIFIED**. `$top.root` is followed, never `$objects[1]`; UIDs are
  bounds-checked and followed at most 4 levels deep; both binary `Uid` and
  XML `{CF$UID}` forms are accepted (whether MBFile records are ever XML is
  **UNVERIFIED**).
- **Output size comes from the padding, never from `Size`.** Decrypted
  output is never truncated (MVT discussion #316). The oracle's own code
  notes that `Size` "routinely" differs from the decrypted size
  (`iphone_backup.py`, `_decrypt_file_to_disk`), so size mismatches are
  informational.
- An incomplete backup can list files it failed to copy (MVT
  `manifest.py`), so a missing blob is counted, not fatal, except for
  sms.db. A blob or `xx/` folder whose metadata cannot be read (for example
  permission denied on a backup copied from another account) is counted
  the same way, as unreadable; only for sms.db is it an error.

### WAL handling

- Unencrypted: Manifest.db and its `-wal` are copied into staging; the
  backup's `-shm` is never copied. **On Linux, a read-only SQLite connection
  applies a staged `-wal` without a `-shm`** (the `plain-wal` fixture:
  opened read-only, all 8 rows counted). Not yet checked on Windows.
- Encrypted: how `-wal` behaves is **UNVERIFIED**; a `-wal` present in an
  encrypted backup is not applied, and the check reports a warning when the
  decrypted header says WAL mode.
- Whether sms.db `-wal`/`-shm` appear in backups is still **UNVERIFIED**;
  validation records whether they are listed.

### Measured key-derivation speed

PBKDF2-HMAC-SHA256, 1,000,000 rounds, release build, Linux development
container (2026-10-18): the crate's own implementation took 0.191 s and the
`pbkdf2` crate 0.188 s, a ratio of 1.018, inside the 15 % acceptance bar.
(The crate's first run the same day measured 0.190 s for both, ratio
0.996.) Real-device derivation times are **UNVERIFIED**.

## Messages database: sms.db (Milestone 3)

- sms.db is at `HomeDomain` / `Library/SMS/sms.db`.
- Attachments: `attachment.filename` looks like
  `~/Library/SMS/Attachments/xx/yy/<guid>/IMG_0001.HEIC`. Strip `~/` and
  hash `MediaDomain-Library/SMS/Attachments/...`. The relative path is
  untrusted and goes through `oma_fs::resolve_within`.
- Core tables are `message`, `chat`, `handle`, `attachment`,
  `chat_message_join`, `chat_handle_join` and `message_attachment_join`.
  Recently Deleted is held in `chat_recoverable_message_join`.
- **Timestamps** count from the Cocoa epoch, 2001-01-01 UTC. A value
  ≥ 1,000,000,000,000 is nanoseconds; anything smaller is seconds. Detect
  the unit by magnitude rather than by iOS version: the iOS version where
  the unit switched is **UNVERIFIED**.
  ([imessage-database dates.rs](https://github.com/ReagentX/imessage-exporter/blob/develop/imessage-database/src/util/dates.rs))
  The canonical archive representation (M4) will be UTC milliseconds or
  nanoseconds since the Unix epoch. The original raw value is kept
  alongside it.
- **Body text.** When `text` is NULL, the body is in `attributedBody`, a
  typedstream (NSArchiver) blob. This always applies to edited or unsent
  messages. A clean-room decoder is required, because the established Rust
  decoder (`crabstep`) is GPL-3.0.
  ([format writeup](https://chrissardegna.com/blog/reverse-engineering-apples-typedstream-format/))
- **Edits and unsends** use `date_edited` plus the `message_summary_info`
  plist: `ec` holds the edit history, `rp` the unsent parts, `otr` the
  originals. Whether a `date_retracted` column exists is **UNVERIFIED**.
- **Tapbacks** use `associated_message_guid` and `associated_message_type`.
  Type codes:
  - 2000–2005: love, like, dislike, laugh, emphasize, question.
  - 2006: emoji tapback, with the emoji in `associated_message_emoji`.
  - 2007: sticker.
  - 3000–3007: removals, in the same order.
  - 1000: sticker. 4000: poll vote.
- **Replies** use `thread_originator_guid` and `thread_originator_part`.
- **Service** may be `iMessage`, `SMS` or `RCS`. RCS was added in iOS 18,
  and the case varies, so compare case-insensitively. Unknown values are
  stored as-is and never mapped to a guess.
- **iOS 26** added translations (`message_summary_info.tmp`), polls, and
  chat backgrounds (`chat.properties`). iOS 27 was released 2026-09-14
  ([MacRumors](https://macrumors.com/2026/09/09/apple-announces-ios-27-release-date/)).
  The schema change notes come from imessage-exporter 4.3.0, which moved to
  capability-based column handling because optional columns may be missing
  or reordered. ([releases](https://github.com/ReagentX/imessage-exporter/releases))
  **Decision:** probe with `PRAGMA table_info`, read columns by name, and
  record which optional columns were present in the import provenance.

## Licensing constraints found

| Project | License | Allowed use |
|---|---|---|
| imessage-exporter / `imessage-database` | GPL-3.0 | Behavioural reference and test oracle only |
| `crabstep` (typedstream) | GPL-3.0 | Reference only |
| `crabapple` (backup decryption) | GPL-3.0 | Reference only |
| pymobiledevice3 | GPL-3.0 | Reference only |
| andrewdotn's decryption answer | CC BY-SA (version depends on the revision date; not checked) | Reference only |
| pyiosbackup | GPL-3.0 | Reference only (file names and keys only) |
| libimobiledevice (`idevicebackup2`) | LGPL-2.1+ | Reference only (behaviour, such as the SnapshotState check) |
| `imessage-core` (typedstream) | MIT | Candidate. It is young (0.1.0, 2026) with unstated provenance, so audit it before use. |
| iphone_backup_decrypt | MIT | May be ported with attribution |
| libheif / libde265 | LGPL-3.0 | Avoid static linking |
| `heic` (imazen) | AGPL-3.0 | Not allowed |
| `heic-rs` | MIT OR Apache-2.0 | Candidate for HEIC thumbnails (M5), behind a feature flag. It is very new and grants no HEVC patent rights. |

## Questions that require real hardware

1. Does `idevice` mobilebackup2 work on iOS 17, 18, 26 and 27, for full and
   incremental backups, both encrypted and unencrypted?
2. How do the iOS 16+ passcode confirmation prompts behave during backup?
3. Does the Apple Devices app's AMDS accept a third-party client on
   Windows 11 (x64 and ARM64)?
4. Are sms.db WAL files present in backups?
5. What are the actual DPIC and ITER values, and how long does key
   derivation take on typical hardware? This sets the progress UI.
6. What does the sms.db schema look like on iOS 27?

Open after milestone 2 (each **UNVERIFIED** above):

7. The Windows subfolders; whether Store iTunes writes under MSIX-virtualised
   AppData; whether Apple Devices works with junctions.
8. The full set of `SnapshotState` values, and what a leftover `Snapshot/`
   folder means.
9. Whether `-wal` and `-shm` exist for Manifest.db in encrypted backups, and
   whether an encrypted Manifest.db `-wal` is encrypted with the manifest key.
10. The meaning of the 4-byte EncryptionKey prefix.
11. Keybag shapes in local backups: TYPE flag bits or values other than 1,
    CLAS above 0xF, KTYP 1, duplicate tags, whether KTYP is always present.
12. Apple's normalisation of non-ASCII passwords, and its maximum password
    length.
13. How "Archive" backups are named.
14. The `Lockdown` key names other than `ProductVersion`; whether
    unencrypted backups always carry `BackupKeyBag`.
15. Whether MBFile records are always binary, and which EncryptionKey shapes
    occur.
16. Real sizes of sms.db, Info.plist, Manifest.plist and Manifest.db.
17. Platform behaviour: WebView2 password autosave; Tauri channel delivery
    above 8 KiB (re-check on every Tauri upgrade); bundled SQLite's default
    Windows VFS does **not** open staged paths beyond MAX_PATH (found by the
    Windows CI long-path test, 2026-10-18), so staged databases are opened
    through the `win32-longpath` VFS with a verbatim path; whether `File::try_lock`
    works on the SMB and NFS shares users choose for archive folders.
18. The layout of iMazing's `iMazing.Versions` folder; whether iMazing ever
    uses the singular `iMazing\Backup`; the meaning of `BackupState`
    (`"empty"` observed in one interrupted backup); and whether a finished
    iMazing backup reads exactly like one from Apple Devices or iTunes.
