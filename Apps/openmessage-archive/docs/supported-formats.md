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

Researched 2026-10-17 and 2026-10-18 for the extraction check. The rules
below are what `crates/oma-messages` implements. Facts were taken from the
sources listed here; **no code was copied** from any of them, and no GPL or
LGPL implementation was opened while the code was written
(`scripts/fixtures/PROVENANCE.md`). **UNVERIFIED** marks a claim not
confirmed by a primary source, an observed artifact or a recorded run.

### Sources

| ID | Source | Licence | Used for |
|---|---|---|---|
| S1 | imessage-exporter releases, https://github.com/ReagentX/imessage-exporter/releases | GPL-3.0 | Facts only |
| S2 | imessage-database source (`tables/messages/message.rs`, `columns.rs`, `tables/capabilities.rs`, `models/{group_action,service,filter_action}.rs`, `tables/{attachment,chat}.rs`, `message_types/{edited,variants,translation,polls}.rs`), read for facts only during research; not opened while M3 code was written | GPL-3.0 | Column names, codes and key names as facts |
| S3 | imessage-exporter `test_data/db/test.db`, opened read-only in a scratch directory; structural and aggregate queries only | GPL-3.0 | Observation ("snapshot C") and oracle run 4; nothing copied |
| S4 | Magnet Forensics, "iOS 16: What Digital Investigators Need to Know", https://www.magnetforensics.com/blog/ios-16-what-digital-investigators-need-to-know/ | Proprietary blog | Facts |
| S5 | Magnet Forensics, "The Meaning of Messages", https://www.magnetforensics.com/blog/the-meaning-of-messages/ | Proprietary blog | Facts |
| S6 | Magnet Forensics, "A look into iOS 18's changes", https://www.magnetforensics.com/blog/a-look-into-ios-18s-changes/ | Proprietary blog | Facts |
| S8 | ChatExportKnowledge, https://github.com/ChatExport/ChatExportKnowledge (README, `attachments.md`, `message-summary-info.md`, `wal.md`, `transports.md`, `what-cannot-be-recovered.md`) | **CC BY 4.0** | Facts, restated in our own words, with this attribution. It is written by a vendor and says it was checked against iOS 11–26 backups |
| S9 | d204n6, "iOS 14 Message Mentions and Threading", https://blog.d204n6.com/2020/09/ios-14-message-mentions-and-threading.html | Blog | Facts |
| S10 | iLEAPP `scripts/artifacts/sms.py`, https://github.com/abrignoni/iLEAPP | MIT | Facts; not ported |
| S12 | plaso `test_data/sqlite/imessage_chat.db`, https://github.com/log2timeline/plaso | Repository Apache-2.0; the file's own terms **UNVERIFIED** | Observation ("snapshot A") and oracle run 4; nothing copied |
| S13 | Gist "sms.sql" (withzombies), https://gist.github.com/withzombies/d50012d14f88a24d7a915b2bb0042fbd | No licence | Column names as facts |
| S15 | Apple, `CFAbsoluteTime` documentation, https://developer.apple.com/documentation/corefoundation/cfabsolutetime | Apple | The epoch |
| S16 | Apple Support 104972 and 120930 | Apple | RCS and satellite features (no code mapping is claimed) |
| S18 | SQLite, "Defense Against The Dark Arts", https://sqlite.org/security.html | Public domain | Hardening; run the integrity check first; triggers and views off |
| S19 | SQLite, "Database File Format", https://sqlite.org/fileformat2.html | Public domain | WAL header, salts and checksums |
| S20 | SQLite, "Quirks" (double-quoted strings) and "CREATE TABLE" (rowid; `INTEGER PRIMARY KEY DESC`) | Public domain | The read-only guard and the rowid-shape check |
| S21 | SQLite, `sqlite3_set_authorizer` and `sqlite3_progress_handler` | Public domain | The authorizer and operation budgets |
| T1 | C. Sardegna, "Inside imessage-exporter: Reverse Engineering Apple's typedstream Format" (2025-01-31), https://chrissardegna.com/blog/reverse-engineering-apples-typedstream-format/ | Not stated | Typedstream format facts; the published sample is used only as a hash anchor |
| T2 | libmagic `magic/Magdir/apple` | BSD-2-Clause | Typedstream header facts |
| T3 | Apple, Objective-C Runtime Programming Guide, "Type Encodings" | Apple | The type grammar |
| T4 | Apple, `NSString.length`, `NSArchiver` and `NSCoder` documentation | Apple | UTF-16 lengths and API semantics |
| T5 | pytypedstream 0.1.0 | LGPL-3.0-or-later | Black-box oracle only (oracle run 2) |
| T6 | imessage-database 4.3.0 `body.rs` and `edited.rs` documentation (docs.rs), and the imessage-exporter 4.3.0 binary | GPL-3.0-or-later | Attribute key names as facts; black-box oracle (runs 3 and 4) |

**Both schema snapshots are macOS.** Snapshot A (S12) is an old macOS
`chat.db` (`_ClientVersion` 9005, dates in seconds). Snapshot C (S3) is a
recent macOS `chat.db` (attachment paths under `~/Library/Messages/`).
Whether iOS sms.db has the same columns as macOS `chat.db` of the same
generation is **UNVERIFIED** (question 19). Neither snapshot is in the
repository; the synthetic fixtures are hand-written from column-name facts.

### Location and WAL

- sms.db is `HomeDomain` / `Library/SMS/sms.db` (fileID
  `3d0d7e5fb2ce288813306e4d4636395e047a3d28`).
- `sms.db-wal` is "very often" present (S8). Its fileID is
  `cd47480f213dba9bc38ee792775d17e3f5a73a59` (computed as
  `SHA1("HomeDomain-Library/SMS/sms.db-wal")`; it matches S8). Ignoring it
  would silently lose the newest messages (S8), so the check stages it
  with the main file when the backup lists it. A `-shm` would be
  `36ce215df2239f14660f11541cb64a3f790c95cd` (computed); it is never
  copied, and whether it ever appears is **UNVERIFIED**.
- The staged `-wal` header and frames are validated (magic, page size,
  salts and checksums, S19). A `-wal` with bytes but an invalid header, or
  a frame that carries the header's salts but has a zero page number or a
  broken checksum chain, is the qualifier `WalFramesInvalid`; a
  zero-length `-wal` is a normal state. After a checkpoint SQLite rewrites
  the `-wal` from the start with new salts and does not truncate it, so
  frames of an earlier generation (already in the main file) can follow
  the current ones. Their salts differ from the header's, which is how
  SQLite finds the end of the log (S19), so the check counts them as
  stale (`stale_generation_frames`), not as invalid. This narrows the
  M3 design rule, which counted every frame from the first salt mismatch
  as invalid. A `-wal` that is not listed while the main file's header
  says WAL mode is the qualifier `WalStateUnknown`: the header shows WAL
  mode, not that changes were pending, because SQLite deletes the `-wal`
  when the last connection closes cleanly. The report says the `-wal` was
  *staged*, never *applied*.
- Whether Apple checkpoints the WAL before a backup, and whether a main file
  and `-wal` captured at different moments can be detected, are
  **UNVERIFIED** (question 25).

### Tables and columns, by source

Columns are matched by **exact** name with `pragma_table_xinfo`; positional
`SELECT *` is never used, because S1 says optional columns may be
"reordered and omitted". "A" and "C" mean the column was observed in that
snapshot (2026-10-18, by name only). S2 is the source of the column set as a
whole, and treats exactly `ROWID`, `guid`, `date` and `is_from_me` as the
required `message` columns.

| Table | Columns | Sources |
|---|---|---|
| `message` (required) | **Required:** `ROWID`, `guid`, `date`, `is_from_me` | S2; A, C |
| `message` | `text`, `attributedBody`, `service`, `handle_id`, `other_handle`, `subject`, `date_read`, `date_delivered`, `date_played`, `is_read`, `is_delivered`, `is_sent`, `is_audio_message`, `cache_has_attachments`, `item_type`, `group_action_type`, `group_title`, `share_status`, `share_direction`, `error`, `was_downgraded` | S2; A, C |
| `message` | `associated_message_guid`, `associated_message_type`, `associated_message_range_location`, `associated_message_range_length`, `balloon_bundle_id`, `expressive_send_style_id`, `payload_data` (only `typeof` is read), `message_summary_info`, `destination_caller_id`, `reply_to_guid` | S2, S13; C |
| `message` | `thread_originator_guid`, `thread_originator_part` (iOS 14, S8, S9) | S2, S8, S9, S13; C |
| `message` | `date_edited`, `date_retracted`, `part_count` (edits and unsends from iOS 16, S4, S8) | S2, S4, S13; C |
| `message` | `associated_message_emoji` (iOS 18, S8); `schedule_type`, `schedule_state` (iOS 18, S6) | S2, S6, S8; C |
| `message` | `filter_action`, `filter_sub_action` | S2 only (read by imessage-exporter 4.3.0, S1); in neither snapshot, so **UNVERIFIED** in practice |
| `handle` (required) | **Required:** `ROWID`, `id` | S2; A, C |
| `handle` | `service`, `country`, `uncanonicalized_id`; `person_centric_id` (C only). One address can appear once per service (S5) | S2, S5; A, C |
| `chat` (required) | **Required:** `ROWID`, `guid` | S2; A, C |
| `chat` | `style`, `chat_identifier`, `service_name`, `display_name`, `room_name`, `group_id`, `properties` (presence and size only), `is_archived`, `is_filtered` | S2; A, C |
| `chat` | `is_blackholed`, `is_recovered`, `last_read_message_timestamp` | S2; C |
| `chat` | `is_pending_review` | S2 only; in neither snapshot, **UNVERIFIED** in practice |
| `chat_message_join` (required) | **Required:** `chat_id`, `message_id`; optional `message_date` (C only) | S2, S10; A, C |
| `chat_handle_join` (required) | **Required:** `chat_id`, `handle_id` | S2; A, C |
| `message_attachment_join` (optional table) | `message_id`, `attachment_id` (required if the table exists) | S2, S5; A, C |
| `attachment` (optional table) | **Required if the table exists:** `ROWID`, `guid`. Optional: `filename`, `uti`, `mime_type`, `transfer_name`, `total_bytes`, `is_outgoing`, `transfer_state`, `created_date`, `start_date` | S2; A, C |
| `attachment` | `is_sticker`, `hide_attachment`, `original_guid`, `emoji_image_short_description` (Genmoji prompt) | S2; C |
| `chat_recoverable_message_join` (optional table; Recently Deleted, iOS 16, S4, S8) | `chat_id`, `message_id` (required if the table exists), `delete_date`, `ck_sync_state` | S2, S4, S8; C |
| `recoverable_message_part`, `deleted_messages`, `sync_deleted_messages`, `sync_deleted_chats`, `sync_deleted_attachments`, `unsynced_removed_recoverable_messages` (optional) | `count(*)` only. No column of `recoverable_message_part` is ever named, and the authorizer denies `part_text`, which holds deleted message content (S3 schema, S5) | S3, S5 |
| `_SqliteDatabaseProperties` (optional) | The value for key `_ClientVersion`, stored as TEXT (`'9005'` in A), parsed as strict ASCII decimal. Its mapping to iOS versions is **UNVERIFIED** | A |

Probe rules that follow from this list:

- **A missing required table or column, or a required table that is a view
  or virtual table, makes the database unsupported**, with the reason.
  Unknown extra tables and columns are counted (never named) and stated as
  qualifiers (D43 in architecture.md).
- **A required column whose name differs only in case makes the database
  unsupported** (`MissingRequiredColumn`), although SQLite's own lookup
  would match it. Known columns are matched by exact name, and a
  wrong-case known column is counted as `wrong_case_columns`. A known
  generated column is treated as absent.
- **Rowid shape.** `ROWID` must be a true rowid alias: declared `INTEGER`,
  the only primary-key column, and with no `pk` index, which rejects
  `INTEGER PRIMARY KEY DESC` (S20: that form is not an alias). Join tables
  must not be `WITHOUT ROWID` and must not shadow `rowid`. Anything else is
  `RowidShape`.
- **Virtual tables anywhere refuse the database** before `quick_check`
  runs (D44). Whether real iOS sms.db has FTS or R-tree tables is
  **UNVERIFIED** (question 19).
- **Byte caps** are applied in SQL before a value is read: 16 MiB for
  `text` and `attributedBody`, 1 MiB for `message_summary_info`, 256 KiB for
  `chat.properties`, 256 bytes for GUID columns, 4 KiB for `filename`,
  `transfer_name` and identifier columns, and 64 KiB for other text. A
  larger value is withheld and counted.
- Snapshot C has Apple triggers that call Apple-only SQL functions
  (`delete_attachment_path` and others, observed). The fixtures omit them,
  and triggers are disabled on the connection in any case.

### Timestamps: classified per value

- The epoch is 2001-01-01T00:00:00Z (S15). One source's comment says "local
  time zone"; S15 is the primary source and wins.
- **The unit is decided per value, not per database or per iOS version.**
  Snapshot C stores `attachment.created_date` in seconds next to
  `message.date` in nanoseconds (observed). The switch from seconds to
  nanoseconds was "roughly iOS 11" (S8); the exact version is
  **UNVERIFIED**.
- Unit windows (`crates/oma-messages/src/time.rs`):

| Raw value | Interpretation |
|---|---|
| 0 | `Zero` |
| below 0 | `Negative` |
| 1 to below 3,124,137,600 (2100-01-01 in Cocoa seconds) | Seconds |
| 3,124,137,600 to below 10¹² | `Ambiguous`; the value is withheld and counted |
| 10¹² (2001-01-01T00:16:40Z as nanoseconds) to below 3,124,137,600 × 10⁹ | Nanoseconds |
| 3,124,137,600 × 10⁹ and above | `Ambiguous` |

  The 10¹² threshold is S8's (and earlier research's); iLEAPP uses 10¹⁵
  (S10). Both separate real data.
- **"Zero means unset" is recorded only for `date_delivered` and
  `date_edited`** (observed in C). For every other column a zero is
  reported as zero and claims nothing; in particular `message.date = 0` is
  not "unset".
- The units of `chat_message_join.message_date`,
  `chat.last_read_message_timestamp` and
  `chat_recoverable_message_join.delete_date` are **UNVERIFIED** (S4 says
  `delete_date` is nanoseconds; no snapshot row confirms it). Each keeps its
  own unit histogram in the report (question 24).
- **`message_summary_info` `ec[*].d`** may be an integer, a real (seconds
  with a fraction, S4) or a plist date (S8). A real is treated as seconds
  and never reclassified; an integer uses the per-value rule; a date is
  Cocoa seconds by definition. The whole rule is **UNVERIFIED**: no
  snapshot row has `ec`.
- Scheduled messages store the planned send time in `date` (S6), so the
  report's date range leaves out rows with a non-zero `schedule_type`, and
  dates before 2007-01-01 (a display floor, not a fact from any source) or
  after the check.

### Coded values: sourced mappings only

Every record keeps the raw integer next to any mapped value. A value with
no cited meaning stays raw and goes into a capped histogram.

**`associated_message_type`** (S2, ranges corroborated by S8):

| Value | Meaning |
|---|---|
| 0 | No association |
| 1000 | Sticker (older form) |
| 2000–2005 | Love, like, dislike, laugh, emphasize, question |
| 2006 | Emoji tapback; the emoji is in `associated_message_emoji` (iOS 18, S8) |
| 2007 | Sticker tapback |
| 3000–3007 | Removal of the matching 2000–2007 |
| 4000 | Poll vote |
| Anything else, **including 2 and 3** | Unknown, kept raw. S2 treats 2 and 3 as normal messages; their meaning is **UNVERIFIED** (question 21) |

- `associated_message_guid` targets: `p:N/<guid>` names body part N,
  `bp:<guid>` a bubble payload (S2). A bare GUID is counted; that shape is
  **UNVERIFIED**. "Only the latest tapback state is stored" (S2) is
  **UNVERIFIED**, since separate removal rows exist (question 21).
- **Replies:** a reply is a row with a non-empty `thread_originator_guid`
  (S9). `reply_to_guid` is also set on rows that are not replies (two rows
  in C), so it is counted separately. `thread_originator_part` is kept raw;
  its format is **UNVERIFIED** (question 22).

**`item_type` and `group_action_type`** (S2, `group_action.rs`; `item_type`
0 is a real message, S8):

| `item_type` | `group_action_type` | Kind |
|---|---|---|
| 0 | not read | Message |
| 1 | 0 | Participant added; "own number changed" is derived only when `handle_id` and `other_handle` are both present, non-zero and equal |
| 1 | 1 | Participant removed |
| 2 | not read | Group renamed |
| 3 | 0 / 1 / 2 / 4 / 6 | Participant left / group photo changed / group photo removed / background changed / background removed |
| 4 | not read | Legacy shared location |
| 5 | not read | Audio message kept |
| 6 | not read | SharePlay |
| anything else, or a combination not listed | | Unknown, kept raw |

**`service`** is matched literally, ASCII case-insensitively, against
`iMessage`, `SMS`, `RCS` (iOS 18, S16) and `iMessageLite` (satellite
messages per S2; the app labels it "iMessageLite", never "satellite"). An
empty value is its own class; anything else is "other", counted and never
echoed. MMS is stored as `SMS` (S8, inferred).

**Kept raw, never interpreted:** `chat.style` (the common claim that 43
means a group chat has no primary source; in A all seven chats were 45,
each with one participant), `chat.is_filtered`, `filter_action`,
`filter_sub_action`, `transfer_state`, `share_status`, `schedule_type`,
`schedule_state` and `error` (question 20). S2 gives meanings for
`filter_action` and `is_filtered`, but whether they hold across iOS
versions is **UNVERIFIED**, so no mapping is shipped.

**Direction and sender.** `is_from_me` is kept raw. S2's shared-location
exception (`item_type = 4`, `other_handle ≠ 0`, `share_direction` false) is
exposed only as a separate derived flag. **`handle_id` is not the sender on
rows from the owner:** in C a row with `is_from_me = 1` has `handle_id` set
to the other party, and 0 is a placeholder. `is_read`, `is_delivered` and
`is_sent` are counted, never shown as states; their semantics by direction
and service are **UNVERIFIED** (question 27). `cache_has_attachments` is set
by a trigger and never cleared (C's schema), so it is a cache flag, not a
fact.

### `message_summary_info`

A binary plist (S4), read through the same budgeted reader as the backup
plists (depth 16, arrays capped at 4,096 entries). A bad field spoils only
itself.

| Key | Content | Source |
|---|---|---|
| `otr` | Original parts; its entry count is used. Read as a dictionary keyed by part index; its exact shape is **UNVERIFIED** | S2, S8 |
| `ec` | Edit history: `{ "<part index>": [ { d: time, t: typedstream, … }, … ] }`. Entries are kept in stored order; "oldest first, entry 0 is the original" is **UNVERIFIED**. The `t` bodies are not decoded in M3 | S2, S4, S8 |
| `rp` | Unsent part indexes (integers) | S2, S8 |
| `tmp` | Translation (iOS 26). Only its presence is recorded | S2 |
| `ust`, `ams`, `amc`, `euh` and any other key | Counted as unknown keys; no source found (question 22) | — |

- **Edits and unsends** are labelled by an ordered list of neutral rules
  (`EditLabel`): unsent parts and edit history come from `rp` and `ec`
  only. A row with `date_edited` set, an empty or NULL `text`, a NULL
  `attributedBody` and no `ec` or `rp` is a heuristic match for an unsent
  message (S4), labelled as such and **never** called "unsent" in the UI.
  The text of an unsent message is not stored anywhere (S8).
- **`date_retracted` exists in the macOS snapshot C (S3).** On iOS it is
  still **UNVERIFIED**. S4 says Apple "doesn't seem to be using it", so it
  is read and counted but drives no label.
- **Polls** (iOS 26) live in `payload_data` (S2); M3 reads only its storage
  class. From M4 the reader can keep its raw bytes, up to 1 MiB, without
  decoding them. Poll votes use associated type 4000.

### Message bodies and typedstream

- A body may be in `text`, in `attributedBody`, or both. **Neither is
  preferred:** in C, two of three rows have `text = ''` (empty, not NULL)
  next to an `attributedBody` blob, so a rule like "the text column, else
  the decoded body" would drop the decoded body. The report counts every
  combination, and whether the two always agree (U+FFFC included) and
  which is current after an edit are **UNVERIFIED** (question 27).
- `text` is NULL for edited messages (S4) and often from iOS 16 on (S8);
  iOS 17 audio transcripts are also in `attributedBody`.
- **Typedstream summary** (T1–T5; the decoder is clean room):
  - Header: `04`, then a length-prefixed `streamtyped`, then system version
    1000 (`81 e8 03`) (T1, T2). Anything else is `UnsupportedHeader`.
    Whether every Messages blob uses this header is **UNVERIFIED**.
  - Integers: a literal byte, or `0x81` + 2 bytes, `0x82` + 4 bytes, `0x87`
    + 8 bytes (little-endian); `0x83` introduces a float (T1). `0x84` marks
    a new entry, `0x85` is nil or the end of a class chain, and `0x86` ends
    an object (T1).
  - References are the head decoded as a signed integer plus 110, into
    tables of shared strings and objects numbered in order of first
    appearance (T1). Every reference is bounds-checked.
  - Types follow Apple's encodings (T3): `c i s l q C I S L Q f d B`, `+`,
    `*`, `:`, `%`, `#`, `@`, `[Nc]` and structs. Messages' observed
    vocabulary is `@ + * i I d [Nc]` (T1).
  - The root is an `NSMutableAttributedString`; its string's `+` bytes are
    the text, decoded as strict UTF-8; then come runs of (dictionary
    number, length in UTF-16 units, T4) with an attribute dictionary per
    new run (T1). Consecutive runs with the same
    `__kIMMessagePartAttributeName` form a part. Attachment runs carry
    `__kIMFileTransferGUIDAttributeName`, and mentions
    `__kIMMentionConfirmedMention` (T6 names). Whether the file-transfer
    GUID always equals `attachment.guid` is **UNVERIFIED** (question 23).
  - Run problems (a bad index, a run past the end, a boundary inside a
    surrogate pair, a total that differs from the text length) keep the
    text and mark the runs; nothing is clamped silently. Index 0 and a nil
    dictionary are treated as bad indexes; whether they occur is
    **UNVERIFIED**.
  - Budgets: input 16 MiB, depth 64, 262,144 objects, 65,536 shared
    strings, 1,048,576 nodes and runs, and materialised bytes at most
    twice the input plus 64 KiB.
  - pytypedstream 0.1.0 (T5) agrees with our decoder on every compared
    synthetic vector (oracle run 2). It rejects the `0x87` form, which T1
    observed from Apple's encoder; those vectors are exempt. Nothing has
    been checked against Apple's own encoder (question 26).

### Attachments

- `attachment.filename` is an untrusted lookup key, never a path. The
  common form is `~/Library/SMS/Attachments/xx/yy/<guid>/<name>` (S8).
- **The file is in `MediaDomain`, not `HomeDomain`.** Looking in
  `HomeDomain` silently finds nothing (S8). The fileID is
  `SHA1("MediaDomain-" + path without "~/")` (S2, S8, S10).

| Prefix | Looked up? | Status |
|---|---|---|
| `~/Library/SMS/Attachments/` | Yes, in `MediaDomain` | Sourced (S2, S8). A miss is reported as "not in this backup" |
| `~/Library/SMS/StickerCache/` | Yes, in `MediaDomain` | Domain **UNVERIFIED**; a miss claims nothing |
| `~/Library/Messages/` | No, counted | The macOS form (3 of 4 attachments in C); on iOS **UNVERIFIED** |
| any other `~/…` | No, counted | **UNVERIFIED** |
| `/var/mobile/…`, `/private/var/mobile/…` | No, counted | Whether these occur is **UNVERIFIED** |
| NULL, empty, anything else | No, counted | |

- A candidate path is looked up only if it passes `oma_fs::resolve_within`
  and has no NUL, backslash or `..` component; otherwise it is counted as
  an unusual name.
- An NFC/NFD or case difference between `filename` and Manifest.db's
  `relativePath` would read as a miss; how often that happens is
  **UNVERIFIED** (question 23).
- A missing file is a normal outcome (S8): never downloaded, purged, or
  kept only in iCloud. A row stays after its file is gone (S5, S8).
- When `mime_type` is NULL, the UTI `com.apple.coreaudio-format` marks an
  audio message (S2).

### Recently Deleted

Deleted messages stay in sms.db for about 30 days (S4, S8):
`chat_recoverable_message_join` links them, and `recoverable_message_part`
holds their content. M3 counts both and never reads `part_text`.

### Other iOS 26 and later features

iOS 26 added translations (`message_summary_info.tmp`), polls
(`payload_data`) and chat backgrounds (`chat.properties`) (S1, S2). iOS 27
was released 2026-09-14
([MacRumors](https://macrumors.com/2026/09/09/apple-announces-ios-27-release-date/));
imessage-exporter 4.3.0, its "iOS 27 Support" release, moved to
capability-based column handling (S1). No published iOS 27 schema diff was
found (question 6). M3 records which optional columns are present and
derives features from them, labelled "derived" and never mapped to an iOS
version.

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
| pytypedstream | LGPL-3.0-or-later | Black-box oracle for the typedstream vectors (run as a subprocess); never imported or shipped |
| ChatExportKnowledge (S8) | CC BY 4.0 | Facts, restated in our own words, with attribution; no text copied |
| iLEAPP (S10) | MIT | Facts; could be ported with attribution, but nothing was |
| plaso `imessage_chat.db` (S12) | Repository Apache-2.0; file terms UNVERIFIED | Observed in scratch and used as an oracle sample; never committed |
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

Open after milestone 3 (each **UNVERIFIED** in the sms.db section above;
questions 4, 6 and 16 also stay open):

19. Whether iOS sms.db has the same columns as macOS `chat.db` in each
    generation (both schema snapshots are macOS), and whether real iOS
    sms.db contains virtual tables (FTS or R-tree), which the check
    refuses (D44).
20. The values and meanings of `chat.style`, `chat.is_filtered`,
    `filter_action`, `filter_sub_action` and `transfer_state`; the first
    iOS version with `filter_action`; how `_ClientVersion` maps to iOS
    versions.
21. The meaning of associated types 2 and 3, and whether only the latest
    tapback state is stored.
22. The format of `thread_originator_part`; the `message_summary_info` keys
    `ust`, `ams`, `amc` and `euh`; the shape of `otr`; the order of `ec`
    entries and the unit of an integer `ec.d`.
23. Attachments: whether absolute `/var/mobile` paths occur; the domain of
    `StickerCache`; normalisation or case differences between `filename`
    and `relativePath`; whether the file-transfer GUID in `attributedBody`
    always equals `attachment.guid`.
24. The units of `chat_message_join.message_date`,
    `chat.last_read_message_timestamp` and
    `chat_recoverable_message_join.delete_date`, and whether zero means
    "unset" in columns other than `date_delivered` and `date_edited`.
25. Whether sms.db `-wal` and `-shm` appear in real backups; whether Apple
    checkpoints before a backup; whether a main file and `-wal` can be
    captured at different moments; whether a staged sms.db `-wal` is read
    on Windows (the Windows CI run of `msgs-wal-plain` will show this for
    synthetic data).
26. Confirmation from Apple's own encoder (`NSArchiver` on a Mac) of the
    typedstream facts the decoder relies on: references for indexes of 110
    and above; the unsigned 128–255 form; when `0x87` is used; `+` holding
    UTF-8; run index 0 or a nil dictionary; the link and data-detector
    attribute layouts; a constant header across iOS versions.
27. Whether `text` always equals the decoded `attributedBody` (U+FFFC
    included) and which is current after an edit; the delivery and read
    semantics of `is_delivered`, `is_read` and `is_sent` by direction and
    service.
28. The text encoding of real sms.db (UTF-8 is assumed; a UTF-16 database
    is a qualifier) and SQLite's handling of invalid UTF-16; real sms.db
    sizes, check durations, peak memory, and whether the operation-budget
    constants fit real data; the licence terms of the Digital Corpora iOS
    images (a possible source of iOS DDL) and of the S12 file.

Two questions from the M3 research were settled by tests rather than
hardware: a schema with double-quoted string literals still loads with
`DQS_DDL` off (`crates/oma-backup/tests/staged_db.rs`), and table-valued
pragma functions are authorized as reads and pragmas
(`crates/oma-backup/src/staged_conn.rs`).
