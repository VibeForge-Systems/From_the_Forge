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
  `Target`. Permissive parsers exist: `ns-keyed-archive` and
  `nskeyedunarchiver`, both MIT/Apache-2.0.
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
CC BY-SA 4.0. It is a reference only; do not copy its code.

1. The keybag in `BackupKeyBag` is a TLV stream: a 4-byte tag, a 4-byte
   big-endian length, then the value. Tags are TYPE, UUID, WRAP, SALT,
   ITER, DPSL and DPIC, then per class UUID, CLAS, WRAP, KTYP and WPKY.
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
| andrewdotn's decryption answer | CC BY-SA 4.0 | Reference only |
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
