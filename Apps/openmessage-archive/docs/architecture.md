# Architecture

Status: Milestone 4 (the archive database and import) is implemented;
its sign-off, like that of milestones 2 and 3, is pending (see the README
roadmap). Sections marked *planned* describe the intended design for later
milestones. They will be revised against evidence as each milestone lands.
Nothing in them is implemented yet.

## Invariants

Every design decision below serves these five rules. A change that weakens
one of them needs an explicit decision record, not a quiet exception.

The app's [`AGENTS.md`](../AGENTS.md) lists seven invariants, numbered I1 to
I7, and the milestone 2 and 3 sections below cite that numbering. This document
numbers its five rules differently:

| This document | AGENTS.md | Rule |
|---|---|---|
| 1 | I1 | Source backups are read-only |
| 2 | I3 | No network |
| 3 | I2 | Paths from outside go through `oma-fs` (backups are hostile input) |
| 4 | I5 | Do not fabricate |
| 5 | I4 | The webview never supplies paths |
| — | I6 | Never log content or passwords |
| — | I7 | Render untrusted text as text |

1. **The source backup is immutable.** The application opens backups read-only
   and never writes inside one. Archive data cannot be stored inside a backup,
   inside the system's MobileSync store, or inside iMazing's default backup
   store. This is enforced in
   `crates/oma-fs`, not by convention.
2. **Message data never leaves the machine unless the user exports it.** The
   application has no telemetry, no update check, and no HTTP client. The
   webview's CSP allows connections to the IPC channel only.
3. **Backups are hostile input.** Manifest paths, filenames, plists, SQLite
   files and attachments may be malformed or malicious. Every untrusted path
   goes through `oma_fs::resolve_within` or `oma_fs::sanitize_filename`, which
   reject anything they cannot prove safe.
4. **Do not fabricate.** A value missing from the source stays missing.
   Unknown schema versions are reported as unsupported; they are not
   approximated. A message state the source does not record (delivered, read)
   is never shown.
5. **The frontend is a presentation layer.** It holds no business logic and
   has no filesystem access. Paths come from native dialogs that Rust opens,
   or from settings that Rust has already validated.

## Components

```mermaid
flowchart LR
  subgraph Webview["Webview (Vue 3 + TypeScript)"]
    Views --> Stores --> IPC["services/ipc.ts"]
  end
  IPC -- "typed commands (allow-listed)" --> Shell
  subgraph Rust
    Shell["src-tauri (commands, state, settings)"]
    Shell --> FS["oma-fs ✔"]
    Shell --> Backup["oma-backup ✔"]
    Shell --> Messages["oma-messages ✔"]
    Shell --> Archive["oma-archive ✔"]
    Shell -.-> Export["oma-export (M6)"]
    Shell -.-> Device["oma-device (M7)"]
    Backup --> FS
    Messages --> Backup
    Messages --> FS
    Archive --> FS
    Archive --> Backup
    Export --> Archive
  end
  Backup -. read-only .-> SRC[("iPhone backup")]
  Archive --> LIB[("Archive library")]
```

| Unit | Responsibility | State |
|---|---|---|
| `src/` | Vue 3 UI: views, Pinia stores, the typed IPC client, the shortcut registry, and the capability roadmap | M1, M2 (Backups page), M3 (extraction report), M4 slice 4m (Archive page) |
| `src-tauri/` | Tauri shell. Holds IPC commands, application state, settings persistence, archive-location validation, the backup registry, job table and DTOs, (M3) the report mapping and the panic hook, and (M4 slice 4l) the archive commands, the import gate and the import and verify jobs. Contains no domain logic. | M1, M2, M3, M4 (slice 4l) |
| `crates/oma-fs` | Filesystem safety: filename sanitization, contained path resolution, and guards for backups, MobileSync, iMazing's default store and cloud sync. From M4: volume facts (`volume`: free space, network/virtualised/FAT detection), the Unix private-permission check (`perm::check_private`), and durable placement (`place`: `fsync_dir`, `create_dir_durable`, `place_noreplace`, `open_nofollow`). From slice 4d: `check_owned_unshared` (the library folder), `rename_dir_noreplace`, `create_new_private` and `total_space` | M1, M4 (slices 4a, 4d) |
| `crates/oma-backup` | Discovers backups, parses Manifest.plist, Info.plist and Status.plist under a budget, classifies them, unwraps the keybag, stages and validates Manifest.db, checks sms.db in place, and extracts single files. From M3 it also stages sms.db with its `-wal` (blob stamps, content digest, WAL facts), opens staged databases under a read-only authorizer and operation budgets, and probes attachment blobs without extracting them. From M4: `copy_file_to` streams one listed file's plaintext into a caller's writer, hashing it, under `Limits::attachment_bytes` and per-file stamps | ✔ M2, M3, M4 (slice 4b) |
| `crates/oma-messages` | Scans and probes the sms.db schema by capability, streams typed records (handles, chats, the joins, messages, attachments, Recently Deleted links) as `Col<T>` values, decodes `attributedBody` with a clean-room typedstream decoder, and produces the aggregate `ExtractionReport`. Persists nothing. From M4: stable `KnownColumn` codes, raw values of unreadable cells (`raw_issues`), `payload_data` on request, the raw association columns, key-only passes and the storage class of text cells | ✔ M3, M4 (slice 4c) |
| `crates/oma-archive` | Owns the versioned archive schema and migrations, runs transactional and resumable imports, builds FTS5 indexes, verifies integrity. Slice 4d ships the skeleton: create, list, open (read-only first), the lock, the one hardened opener, the lock-free reader, schema v1 with its codes and fingerprint, the migration framework, write-open recovery and the creation sweep; slices 4e–4j the import; slice 4k the report-only verifier and `open_blob`. Used by the shell since slice 4l | ✔ M4 (slices 4d–4k) |
| `crates/oma-export` | Exports to TXT, CSV, JSON, HTML, PDF and XLSX | planned, M6 |
| `crates/oma-device` | Detects devices and creates backups behind a platform trait | planned, M7 |

Each library crate can be built and tested without Tauri. The shell only
converts between IPC types and library calls.

## The IPC boundary

- **Allow-listed commands.** `build.rs` declares every command, so
  `tauri-build` generates an `allow-<command>` permission for each one.
  `capabilities/default.json` grants exactly those permissions. The webview
  has no core window, filesystem, shell, HTTP, dialog or opener permissions.
  The dialog and opener plugins are driven from Rust.
- **Types defined once.** IPC types are Rust structs annotated with
  `#[derive(TS)]`. `cargo test` writes them to `src/types/generated/`, and
  CI fails if the committed copies are stale.
- **Inputs validated by deserialization.** Input types use
  `deny_unknown_fields`, and enums are closed. For example, `SettingsUpdate`
  has no `archiveDirectory` field, so the webview cannot set one.
- **Uniform errors.** Every failure crosses as `IpcError { code, message }`.
  `code` is a closed enum. `message` is a user-facing sentence. Errors never
  carry message content or secrets.
- **Progress and cancellation (M2).** `open_backup` returns a job id at once
  and runs the job on a dedicated `std::thread` (`oma-job-<n>`). Progress
  and exactly one terminal event (`finished`, `failed` or `cancelled`, each
  carrying `stagingRemoved`) travel over a `tauri::ipc::Channel<JobEvent>`
  passed as a command argument. `cancel_job` sets a `CancelToken` that the
  crate checks inside every loop, and a per-connection SQLite interrupt
  watcher covers long statements. One job runs at a time (D7); a second
  request gets `busy`. Progress is throttled to phase changes and at most one
  event per 100 ms.
- **No `core:*` permission for channels.** Small channel payloads (under
  8 KiB) are delivered with `webview.eval`. Larger ones are fetched through
  `plugin:__TAURI_CHANNEL__|fetch`, which Tauri 2.12.4 exempts from the ACL
  check (`src/webview/mod.rs`, under `// TODO: Remove this special check in
  v3`). **Every Tauri upgrade must re-verify, by hand, that a `finished`
  event over 8 KiB arrives without a `core:*` grant.** A channel cannot be
  reattached after a page reload, so the UI polls `get_backup_details`
  every 2 s while a job it cannot hear is active. A failed delivery never
  cancels the job.
- **The extraction check (M3).** `check_extraction` takes the `backupId`
  hash and an optional password (`deny_unknown_fields`), and runs one job
  on the same model: open, validate, check, close (D21). Its terminal event
  is a new variant, `extractionFinished` (D34), which carries **only a
  summary**: the outcome, `sourceChanged`, `stagingRemoved` and
  `plaintextRemovedAfterRowPass` (D47). The full report is fetched with
  `get_backup_details` as `lastExtractionReport`, so the check never
  depends on the channel's ACL-exempt fetch path. A guard test keeps the
  summary under 1 KiB. The report DTO has no free-text strings, and a
  key-based structural test walks every reachable DTO type (I7). Counters
  that are not recorded cross as `null`, never as 0 (I5), and integers
  outside ±(2⁵³ − 1) are clamped with a `saturated` flag.
- **Archives (M4 slice 4l).** Six commands: `list_archives`,
  `create_archive`, `get_archive_details`, `start_import`,
  `confirm_import` and `verify_archive` (§"The archive commands and jobs"
  below). They take archive ids (`a<32hex>`), backup ids, booleans, a
  lineage number and an optional label; never a path. Two new job kinds,
  `import` and `verify`, use the same channel and job table; the new
  events are `importPreflight` (non-terminal), `importFinished` and
  `verifyFinished`, each under 1 KiB, with reports and the preflight
  fetched through `get_archive_details`.

## Settings

Settings are stored in `settings.json` in the platform config directory
(`app_config_dir()`). The file holds preferences, the archive library path
and, from M2, `backupFolders` (the folders the user added on the Backups
page, as `{ path, kind }`), nothing else. `backupFolders` is read leniently:
a malformed value or element is dropped with a notice, never treated as a
corrupt file. The webview cannot set it. Loading never fails:

| Condition | Behaviour |
|---|---|
| File missing | Defaults are used. The file is written on the first change. |
| Unreadable (I/O) | Defaults are used and settings are read-only for the session. A notice is shown. |
| Malformed or wrongly typed | The file is renamed to `settings.json.corrupt-<unix-time>` with its bytes preserved. Defaults are used and a notice is shown. |
| `schemaVersion` newer than this build | Defaults are used. The file is never overwritten, and a notice is shown. |

Saves are atomic: write to a sibling `.tmp`, `fsync`, then rename. Memory
changes only after the save succeeds.

## Archive library location

The user chooses one *library* directory. Each archive gets a subdirectory
inside it (from M4). A location is checked every time it is chosen or
re-checked:

| Check | Level | Why |
|---|---|---|
| `unicode-path` | error | The path is stored as UTF-8 and is never silently mangled |
| `absolute` | error | A relative path cannot be validated |
| `exists` | error | The directory may be on an unplugged drive |
| `not-root` | error | A drive root is never an archive |
| `outside-backup` | error | **Invariant 1.** No ancestor may contain two or more backup marker files, and the path must not be inside `MobileSync/Backup` or `iMazing/Backups` (iMazing's default store; both matched by component, case-insensitively). |
| `local-volume` | error or warning | From M4. The volume kind from `oma_fs::volume_kind`. **Network** (NFS, SMB/CIFS, AFS, Ceph, Coda on Linux; a UNC path, or one that `canonicalize` reveals as `\\?\UNC\…`, on Windows) is an error that stops the checks: SQLite's WAL "does not work over a network filesystem" (sqlite.org/wal.html), and staging needs file locks. **Virtualised** (FUSE, 9P, VirtualBox shared folders; Linux only) is a warning: backup checks keep working, but archives refuse such a folder (D54). **FAT/exFAT** (Linux only) is a warning: no hard links, and FAT's 4 GiB file limit. A recognised local file system is ok. An unknown kind adds no row: every drive letter on Windows, because detecting FAT or a mapped network drive there needs `unsafe` Win32 calls. **This changes behaviour:** a library already chosen on a network share is now refused. |
| `writable` | error | A probe file is created, written, `fsync`ed and removed. This catches ACLs, read-only mounts and quota problems. |
| `local-only` | warning | The folder is inside a known sync client's folder (OneDrive, Dropbox, iCloud Drive and others). **Invariant 2**: the user must know the data will leave the machine. This is a name heuristic, so it warns and never blocks. |

### Archive layout (M4)

```
<library>/
  .oma-staging/                   M2/M3 staging (unchanged)
  .oma-new-<32hex>/               an archive being created; swept when stale
  a<32hex>/                       one archive; 0700 on Unix
    archive.lock                  sidecar lock file; created once, never deleted
    archive.sqlite                schema v1, WAL, FTS5; 0600 on Unix
    archive.sqlite-wal, -shm      SQLite's own files; same mode as the database
    attachments/
      .incoming/                  <n>.partial while a copy is in flight
      <hh>/<64 hex>               content-addressed plaintext
    imports/
      session-<n>.json            content-free provenance (slice 4j)
```

### Threat model for archive files (M4, spec §4.5)

An `a<32hex>` directory may have been planted or altered by another program
or user. The goals: nothing is written outside the archive directory, no
code is loaded from it, listing and opening do bounded work, verification
is cancellable, and no import writes into an archive that failed a check.
The measures: id parsing; refusal of links and wrong types; owner and mode
on Unix; a hardened read-only first connection (`DEFENSIVE`,
`TRUSTED_SCHEMA` off, DQS off, no extensions, limits, an op budget for
`list` and `open`); the schema fingerprint before any table read; CHECK
constraints; `storage_name` parsed as `^[0-9a-f]{64}$`; no-follow opens;
bounded walks; size caps on `imports/*.json`; the label re-validated.
**Accepted:** SQLite's page parsing and the FTS5 integrity and merge code
run over archive pages whose content may be anything, because M5 must read
the index anyway, SQLite documents these settings for untrusted database
files, verification runs only on request and only reports, and `list`
never touches FTS. This is narrower than D44 for sms.db: the archive's FTS
table is our own, and the fingerprint has matched its schema text.

### Archive naming

The directory name is a random id (`a` and 32 lowercase hex characters);
the user's label lives only in the database (D52). There is no
`thumbnails/` directory in M4 (D72). Attachments are content-addressed by
SHA-256, and a stored name is parsed against `^[0-9a-f]{64}$` before any
path is built from it. The original filename stays in the database and is
used only on export, after `sanitize_filename`.

## Data flow

Milestone 2 (implemented):

```
backup dir (read-only, never opened by SQLite)
  → oma-backup inspect: marker stats, two header reads, three budgeted plist reads
  → OpenBackup::open: [password → PBKDF2 ×2 → class keys → ManifestKey]
      → Manifest.db copied (or decrypted) into <library>/.oma-staging/s<16hex>/m/
  → validate: hardened read-only open, quick_check, schema, row pass, blob guards
      → sms.db checked in place: header (and first/last block when encrypted); no copy
  → close(): connection closed, keys zeroized, staging removed (bounded retry)
```

Milestone 3, the extraction check (implemented; one job, `check_extraction`):

```
  open → validate (M2) → if not importable: skippedNotImportable
  → oma_messages::check(&open_backup):
      a. extract_database_with(HomeDomain, Library/SMS/sms.db, WAL best effort)
         → <library>/.oma-staging/s<16hex>/db/<n>/sms.db[-wal]
           (source blobs stamped by length and mtime; staged content digested;
            WAL header and frames validated; Windows temporary attributes)
      b. hardened open of the staged copy: read-only authorizer, DQS off,
         triggers and views off, SMS_DB_LIMITS, operation budgets
      c. scan sqlite_schema (no virtual table is connected)
         → any CREATE VIRTUAL TABLE: Unsupported(VirtualTablePresent)
      d. PRAGMA quick_check(1) under an op budget
         → not "ok": Damaged(IntegrityProblems); over budget: Incomplete
      e. probe each known table with pragma_table_xinfo, rowid shape, count(*)
         → missing minimum, view instead of table, bad rowid: Unsupported
      f. record passes: handles, chats, chat_handle_join, chat_message_join,
         messages (bodies, summaries, associations, GUID sets),
         message_attachment_join, attachments (classify, look up, probe the blob),
         Recently Deleted links; each pass's row count must equal count(*)
      g. aggregates: integer SQL under op budgets; GUID checks in Rust
      h. assert query_only, no authorizer denials, total_changes() == 0
      i. connection dropped; staged sms.db removed straight after the row pass
  → unchanged_since_open() (now covers the sms.db and -wal blobs)
  → close(): keys zeroized, staging removed
  → exactly one terminal event: extractionFinished | failed | cancelled
```

Milestone 4, the import (implemented; one job, `start_import`, with the
gate answered by `confirm_import`):

```
  open → validate (M2) → if not importable: importFinished/notImportable
  → Archive::import(&open_backup, options, gate, ctx)   (oma-archive)
      preflight (reads only; nothing written to the archive):
        a. stage sms.db[-wal] as M3 does; schema scan, quick_check, probe
        b. P0: Recently Deleted scan; key scan (sorted 128-bit digests,
           4,194,304 cap) marks every duplicated key unkeyed (D83)
        c. resume plan (digest, framing, versions, options, lineage; D64),
           lineages and boolean device evidence, order facts, space estimate
        d. the gate: ImportGate::confirm → the shell publishes the preflight,
           sends importPreflight and waits ≤ 2 min for confirm_import (D65)
           → Decline / cancel / exit / timeout: nothing written
      after Proceed: begin_write(Recovery::Run), the session row, then passes,
        each in IMMEDIATE batches (1,000 rows / 32 MiB / 2 s) with the pass
        cursor in the same transaction and a savepoint per record (D63):
        P1 handles → P2 chats → P3 messages, bodies (FTS by triggers only),
        parts, edits, unsent parts → P4/P5 chat links → P6 attachment rows
        and files (write → sync_all → intent commit → no-replace rename →
        directory flush → row commit) → P7 attachment links → P8 Recently
        Deleted markers; space guards before every transaction and file
      → staged sms.db removed (D74)
      P9 finalize: statistics, session-local cleanup, completed, FTS merge,
        wal_checkpoint(TRUNCATE); imports/session-<n>.json (best effort)
  → close(): keys zeroized, staging removed
  → exactly one terminal event: importFinished | failed | cancelled
```

A verify (`verify_archive`) opens the writer without recovery, runs the
database, index and file checks of slice 4k, and records one `verify_run`
row; its terminal event is `verifyFinished`.

Planned (M5):

```
  → UI: paged queries through ArchiveReader (keyset pagination on
        (chat_id, sort_key, message_id)), virtualized thread list,
        attachments loaded on demand through BlobReader and a scoped asset protocol
```

Decrypted intermediates are written only inside the archive library's
staging directory, never in the system temp folder. They are removed when
the job ends, and leftovers from a crash are removed by the sweep at the next
start with the same library. docs/privacy.md explains the limits of
deletion.

## Backups (milestone 2)

### Discovery

On Windows, `list_backups` scans `<home>\Apple\MobileSync\Backup`,
`<roaming>\Apple Computer\MobileSync\Backup` (both subfolders are
UNVERIFIED by Apple; see supported-formats.md) and iMazing's default
`<roaming>\iMazing\Backups` (observed in iMazing's settings on one Windows 11
machine; configurable in iMazing), plus every folder the user added. A root
scan never lists a child named `iMazing.Versions`, where iMazing keeps
archived snapshots in a layout that is UNVERIFIED. Linux has no standard location. Scanning happens only when the
Backups page opens or the user clicks Refresh, never at startup. A listing is
bounded by counts, not time: at most 512 candidates are inspected and 256 MiB
of plist bytes read per refresh, and at most 256 candidates come from one
root. The rest are listed as `notInspected`. `BackupId` and `LocationId` are
hashes of the canonical path, computed in Rust; the webview can only echo
them.

### Budgeted plist reader (D17)

Backup plists are never parsed with `plist::Value::from_reader`, because a
binary plist can reference one object many times and expand without bound.
`oma_backup::bplist::read_bounded` iterates `plist::stream::Reader` and caps
events (`4 × len + 1024`), cumulative string and data bytes (`2 × len`) and
depth while streaming. Size caps are 1 MiB for Status.plist, 32 MiB for
Manifest.plist and 64 MiB for Info.plist (Info.plist failures are never
fatal). The identifying root keys (`Unique Identifier`, serial number,
IMEI, ICCID, MEID, phone number and others) are skipped while streaming:
the stream reader still decodes their values, which are counted against the
budgets and discarded, and no output struct has a field for them (D14).
Skipping applies to root keys only; Manifest.plist's `Lockdown` dictionary
is built whole and four keys are kept from it.

### Staging (D5, D6)

```
<library>/.oma-staging/          real directory, 0700 on Unix
  s<16hex>/                      one per job; .lock held with File::try_lock
    m/Manifest.db[-wal]          the staged (decrypted) index
    x/<n>, db/<n>/<basename>     extraction outputs (M3 API)
```

`StagingRoot::new` refuses a staging directory that is a link, is not at
`<canonical library>/.oma-staging`, is owned by another user or readable by
others (Unix), or would sit inside the backup, inside any backup, inside
`MobileSync/Backup`, or inside `iMazing/Backups`. `OpenBackup::close()` deletes plaintext first, releases
the lock, then removes the session with up to five attempts (50–400 ms
between attempts, about 0.75 s in total). Every extraction writes through
`<dest>.partial`; on any error the partial file (and, for a database, a main
file whose `-wal` then failed) is removed at once, not left until close. The sweep runs at startup, before each job, and against the old
location after the archive location changes; it touches only `s<16hex>`
directories whose lock it can take, never follows links, and never creates a
lock. On Windows, staging inherits the library's permissions. On Linux, a
volume that does not keep POSIX modes (FAT, exFAT or NTFS without a private
umask, many network shares) reports group and other bits on the new 0700
directory, so `StagingRoot::new` refuses it (`LoosePermissions`), and
`inspect_location` marks such a folder unusable (the `private` check) so it
is not accepted as an archive location in the first place. The
`RunEvent::Exit` handler cancels running jobs and waits up to 5 s for their
cleanup; a kill or crash skips it, and the sweep covers that case. M4's archive enumeration must skip directories whose names start with
a dot.

### Hardened SQLite open

The staged Manifest.db is opened `SQLITE_OPEN_READ_ONLY | NO_MUTEX`. If
SQLite cannot open a staged WAL database read-only (`SQLITE_CANTOPEN`), it
reopens the **staged copy** (never the backup) `SQLITE_OPEN_READ_WRITE`, with
the same hardening including `query_only` (`OpenMode::ReadWriteQueryOnly`);
SQLite may then create a `-shm` inside staging. Either way the connection has
`SQLITE_DBCONFIG_DEFENSIVE` on, `TRUSTED_SCHEMA` off, `query_only`,
`cell_size_check`, `temp_store = MEMORY` (decrypted pages never spill to the
OS temp folder), `mmap_size = 0`, and tight `sqlite3_limit` values following
sqlite.org/security.html. Extensions are never enabled; queries use bound
parameters and name their columns. Databases extracted for M3 are opened
the same way through `StagedDatabase::open`, which returns a
`StagedConnection` that borrows the `OpenBackup` (so it is closed before
`close()` deletes the file) and uses wider, provisional limits for sms.db's
many columns and long values. The backup's own Manifest.db is never
opened by SQLite, because SQLite creates a `-shm` next to a WAL database it
opens. The backup's `-shm` is never copied. On Linux, a read-only connection
applies a staged `-wal` without a `-shm` (tested with the `plain-wal`
fixture).

## The extraction check (milestone 3)

The design is recorded in full in the Milestone 3 spec; this section states
what the code does and why.

### Staging sms.db (D22, superseding D11 for the check)

M2 checked sms.db in place and never copied it (D11). The extraction check
must read every row, so it stages a plaintext copy, decrypted for an
encrypted backup, through `extract_database_with` into
`<library>/.oma-staging/s<16hex>/db/<n>/`, with the `-wal` when one is
listed (`WalPolicy::BestEffort`, D23). The copy is removed straight after
the row pass (`StagedDatabase::remove`); `close()` is the backstop and the
sweep covers crashes. The staged connection borrows the `StagedDatabase`,
so it cannot outlive the file it reads. sms.db is refused above 32 GiB
(`Limits::sms_db_bytes`, D40). Before closing, `unchanged_since_open()`
compares the length and modification time of Status.plist, Manifest.db, and
now the sms.db and `-wal` blobs; a change is the qualifier `SourceChanged`
(D31). A `-wal` is reported as *staged*, never as *applied*.

### The read-only guard (D36)

`StagedConnection::conn()` is crate-private, so no caller outside
`oma-backup` holds a bare `rusqlite::Connection`. Every staged sms.db
connection has:

- **an authorizer** that allows only reads, a fixed list of functions and
  pragmas, and denies `recoverable_message_part.part_text` (deleted message
  content) outright;
- **double-quoted string literals off** (`DQS_DML`, `DQS_DDL`), so a
  misspelt identifier can never become a string;
- **triggers and views off** (S18);
- **operation budgets** through `sqlite3_progress_handler` on every
  statement whose cost grows with the data (`quick_check` and the
  aggregates), instead of wall-clock deadlines (D45);
- the M2 hardening (`query_only`, defensive mode, `trusted_schema` off,
  `temp_store = MEMORY`, `mmap_size = 0`) and wider but still bounded
  `SMS_DB_LIMITS`.

At the end of the row pass the check asserts `query_only`, zero authorizer
denials and `total_changes() == 0`. Those are checks, not proof; the
authorizer is the guard.

### Schema: scan, integrity, probe (D37, D44)

1. **Scan** `sqlite_schema` without touching any table. A virtual table is
   refused before `quick_check`, which would otherwise run FTS or R-tree
   integrity code on attacker data (D44). A `table` row is virtual when its
   rootpage is 0 or when its whole SQL, tokenised the way SQLite's schema
   loader reads it (comments and any whitespace skipped), starts `CREATE
   VIRTUAL`; a reserved `sqlite_` name does not exempt it. This replaces the
   design's 32-byte prefix test, which a commented or padded statement with
   a non-zero rootpage could bypass. `pragma_table_list` is not used: it
   connects every virtual table to count its columns. Names and SQL text are
   classified, never stored or logged.
2. **`PRAGMA quick_check(1)`**, under an op budget. Only `"ok"` is read;
   anything else is `Damaged(IntegrityProblems)` and no row is read.
3. **Probe** each known table by capability (`pragma_table_xinfo`, columns
   matched by exact name). A required column missing, a table that is a
   view, or an unsafe rowid shape (including `INTEGER PRIMARY KEY DESC`,
   which is not a rowid alias) is `Unsupported` with reasons.

### Values: `Col<T>` and redaction (D49)

Every source field is a `Col<T>`: the column is absent, the value is NULL,
the value is unreadable (wrong type, invalid UTF-8), the value was withheld
because it is over its byte cap, or the value. M4 can therefore tell "not
recorded" from "NULL" from "bad". Content and identifiers are
`Redacted<SqlText>` or `Redacted<RawBlob>`: their `Debug` prints only a
length, they have no `Display` or `Serialize`, and they are zeroized on
drop. Coded values keep the raw integer and are mapped only where a source
documents the meaning. Timestamps are classified per value (seconds,
nanoseconds, zero, negative, ambiguous), not per database (D27).

### Typedstream (D26)

`attributedBody` is decoded by a clean-room module in `oma-messages`
(`src/typedstream/`): a budgeted generic walk, then an interpretation of a
closed set of Foundation classes, with every length checked before use and
every reference bounds-checked. Run problems keep the text and mark the
runs; nothing is clamped silently. Neither body source is preferred: the
`text` column and the decoded body are both kept, and M4 decides.
Provenance is recorded file by file in `scripts/fixtures/PROVENANCE.md`.

### Attachments (D29, D30, D50)

`attachment.filename` is an untrusted lookup key, never a path. Only the
`~/Library/SMS/Attachments/` and `~/Library/SMS/StickerCache/` classes are
looked up, in `MediaDomain`, after an unusual-name gate built on
`oma_fs::resolve_within`. A hit is probed (metadata, and the file key
unwrapped in memory for an encrypted backup) without writing any plaintext.
A blob whose length differs from the Manifest's `Size` (padded to the next
16-byte block when encrypted, as validation compares it) is a size
mismatch, never "present"; this adds an outcome to the design's probe
set, so a truncated or empty blob is not reported as the file. Every other
class is counted only. Without an `attachment` table nothing is looked up,
and the shell sends no attachment section, so its counters read "not
recorded", never 0.

### The attachment copy for the archive (M4 slice 4b)

`OpenBackup::copy_file_to(entry, sink, ctx)` is how the archive import
(M4) will read attachment files. It shares one read-and-decrypt loop with
`extract_file` and `extract_database_with`; only the destination differs.

- **It never writes to the file system.** The plaintext goes to a
  `&mut dyn Write` the caller owns, so `oma-backup` keeps its rule of
  writing only inside staging, the archive's files never carry
  `FILE_ATTRIBUTE_TEMPORARY`, and buffering, `sync_all`, placement and
  cleanup stay with the caller. On any error the caller discards what the
  writer received.
- **SHA-256 of the plaintext is computed while it streams** and returned
  in `CopiedFile` with the plaintext length, the Manifest `Size` and
  whether they match (`None` when no `Size` is recorded). A mismatch is
  recorded, never corrected. `CopiedFile`'s `Debug` prints the digest as
  `<sha256>`, because the digest names the archived file.
- **`Limits::attachment_bytes` (8 GiB, fixed like D40)** is checked
  against the stored blob's length before a byte is read, and again
  against the open handle's length. For an encrypted backup that is the
  ciphertext, up to 16 bytes longer than the plaintext. A larger blob is
  `LimitExceeded { what: AttachmentBytes }`.
- **Per-file stamps.** The blob's length and modification time are taken
  before the copy and compared after it; a difference is `SourceChanged`.
  They are not added to the stamps `unchanged_since_open()` compares, so
  that check stays O(1) however many attachments are copied. A change that
  keeps both the length and the modification time is not detected; the
  full verifier re-hashes stored files.
- **Errors keep their side.** A failed write or flush of the caller's
  writer is `Io { side: Sink, .. }` with its `kind` and OS code, so the
  import can tell a full volume (`StorageFull`) or a per-file size limit
  (`FileTooLarge`, `EFBIG`, `ERROR_FILE_TOO_LARGE`) from a source problem.
- **Progress** is `Phase::Copying` in bytes of the stored blob (the shell
  maps it to `copyingAttachments`). Cancellation is checked before every
  1 MiB read, so a cancel inside a large file stops within one chunk.
- **Not done here:** `AttachmentKey` stays opaque and cannot be persisted,
  so the import must copy inside its attachment pass (M4 §9.1).

### The reader additions for the archive import (M4 slice 4c)

`oma-messages` gains what the archive import needs to store every source
value without a second read and without fabricating one (M4 spec §3.3).
All of it is additive; the extraction check's report is unchanged, because
every new read is off unless asked for.

- **Stable column codes.** `KnownColumn::code()` is a `u16` written beside
  each column in the static list: the table's `code_base()` (`message`
  100, `handle` 200, `chat` 300, the joins 400–600 and 800, `attachment`
  700, `_SqliteDatabaseProperties` 900) plus a per-column number.
  `KnownColumn::from_code` reverses it. Reordering the list cannot change
  a code; a compile-time check keeps codes in their table's range and
  unique, and a unit test pins every code. A removed column retires its
  code. The archive stores these codes (`value_issue.col`), so a code is
  never reused.
- **Raw values of unreadable cells (R2).** Every record has
  `raw_issues: Vec<RawIssue>`, where `RawIssue { column, raw: RawSql }`
  holds what SQLite returned for an `Unreadable` cell, in its storage
  class (`Integer`, `Real`, `Text` or `Blob`). It is filled only when
  asked: `MessageReadOptions::keep_raw_blobs` for messages, and
  `ProbeOptions::with_keep_raw_values` for the other passes. A text or
  blob value is kept up to the column's cap, or `CAP_RAW_UNREADABLE`
  (4 KiB) for an integer, boolean or time column; a larger one is not
  kept, and the `Col` still says "unreadable". Columns read for presence
  only (`chat.properties`) never materialise a value, so they have no raw
  value. A property list that does not parse is not a raw issue: its bytes
  are `raw_message_summary_info`. Issues are listed in column-code order.
  `RawSql`'s `Debug` prints only the class and a length, never a value,
  not even an integer.
- **`payload_data` on request (R3).** With `keep_raw_blobs`, the message
  pass uses a third form of its query that also selects
  `CASE WHEN octet_length("payload_data") <= 1048576 THEN "payload_data"
  END` and its length, so `MessageRecord::payload_data` is
  `Some(Col<Redacted<RawBlob>>)` and a larger value is `Oversized`.
  Without it the field is `None` and the bytes are never read, as in M3.
  The value's counts go to a scratch counter, so a pass's `ValueIssues`
  does not depend on the option. Its contents (polls, app and link
  payloads) are not decoded. Whether the import keeps it is D59, which
  the owner confirmed on 2026-10-18: the import always keeps the raw
  `attributedBody`, `message_summary_info` and `payload_data` bytes within
  the caps, and privacy.md discloses it.
- **Raw association columns (R4).** `MessageRecord::assoc_raw` holds the
  five `associated_message_*` columns exactly as read, for every row.
  `Association` maps `''` to NULL and returns `None` for several distinct
  states; `assoc_raw` keeps them apart, and `AssocRaw::association()`
  derives the same `Association` the record carries.
- **Key-only passes (R6).** `for_each_message_key`, `for_each_chat_key`
  and `for_each_attachment_key` yield `KeyRow { rowid, guid }`, and
  `for_each_handle_key` yields `HandleKeyRow { rowid, id, service }`
  (sms.db's own uniqueness). They select the key and those columns only,
  under the same caps, pages, cursors and cancellation as the record
  passes, so the import's duplicate-key scan never reads a body. They
  report the phase of the table's record pass (`ReadingMessages`,
  `ReadingChats`, `CheckingAttachments`, `ReadingHandles`), so the shell's
  phase mapping is unchanged. An absent optional `attachment` table gives
  an empty pass.
- **Storage class of text (R7).** `SqlText::class()` is `TextClass::Text`
  or `TextClass::Blob`, as SQLite reported the cell. Equality compares the
  class as well as the bytes, and `Debug` adds `, blob` for a BLOB cell.
  The archive stores text as BLOBs, so it records the class separately.
- **Statement limits.** `record_queries()` now lists every form (records,
  the raw message form and the four key forms); each still fits the
  `sql_length` and `vdbe_op` limits with a 2x margin.
- **Not done here:** `chat.properties` raw bytes (R5) are deferred
  (M4 §21.2); the property list is still read for presence and size only.

### The archive skeleton (M4 slice 4d)

`crates/oma-archive` is a library with no Tauri dependency. Slice 4d ships
everything an import needs before it writes a row (spec §4, §5, §6, §7):

- **`Library::new`** canonicalises the folder and refuses a file, a
  file-system root, anything inside a backup, `MobileSync/Backup` or
  `iMazing/Backups` (I1), a network or virtualised volume (D54), and on
  Unix a folder owned by someone else or writable by group or others.
- **`Library::create`** builds the archive in `.oma-new-<32hex>` under its
  own lock: the database is created empty with mode 0600, switched to WAL,
  and filled in one transaction (the v1 DDL, `application_id` "OMA1",
  `user_version`, the meta row, FTS `secure-delete`), then checkpointed.
  The store directories are created and flushed, the directory is flushed,
  every handle is closed, and it is renamed to `a<32hex>` without replacing
  anything (`renameat2(RENAME_NOREPLACE)` on Linux; a checked rename with
  retries on Windows), and the library is flushed. Creation needs 64 MiB
  free; unknown free space does not block it.
- **`Library::open` writes nothing** (§5.3). In order: the id is parsed;
  the directory must be real; on Unix the directory, database, `-wal`,
  `-shm`, lock and store directories must be private (links and wrong
  types are refused everywhere); the volume is checked; the lock is taken
  (five retries at 200 ms, then `Locked`) and self-tested (a second handle
  must be refused, otherwise `LockUnreliable`); a ReadOnly connection reads
  `application_id` and `user_version`, then the schema fingerprint, then
  `archive_meta`. A newer schema is `NewerSchema`; a newer
  `max_deriver_version` or `max_decoder_version` sets
  `ImportBlock::NewerRules` (read and verify only); an older schema opens
  with `needs_migration()` set. The only exception to "writes nothing" is a
  missing lock file, which is recreated.
- **`ArchiveReader`** (`Library::open_read`) takes no lock, so details can
  be read while an import holds the archive; in WAL mode it sees only
  committed state. It refuses an older schema with `NeedsMigration`. It
  reads the label (re-validated; an invalid one is `Unreadable`), stats,
  session counts and the newest unfinished session.
- **`Library::list`** reports every real `a<32hex>` directory as `Ready`,
  `InUse` (lock held; never waits), `NeedsMigration`, `NewerSchema`,
  `NotAnArchive`, `Damaged` or `Refused`, and never writes archive data.
- **`Library::sweep`** removes creation directories at least five minutes
  old that are unlocked and hold only what creation makes; anything else is
  counted and left alone.
- **`conn.rs` is the one opener** (§5.5), so M8 can add a key pragma in one
  place. Both profiles set `DEFENSIVE`, `TRUSTED_SCHEMA` off, DQS off, views
  off, `foreign_keys` on, `temp_store` MEMORY, `mmap_size` 0,
  `SQLITE_LIMIT_ATTACHED` 0 and length limits, and use the `win32-longpath`
  VFS with a verbatim path on Windows. ReadOnly adds `query_only`, triggers
  off and an op budget (progress handler; an exhausted budget is
  `Damaged(WorkBudget)`). ReadWrite adds `synchronous` FULL,
  `secure_delete`, `journal_size_limit` 64 MiB, and checks that the file is
  in WAL mode. The database path is reachable only through a key type that
  module alone can make, and clippy forbids `File::open`, `OpenOptions`,
  `fs::copy` and other connection openers in the crate.
- **The fingerprint** (§7.3) builds the expected objects in memory from
  `v1.sql` plus the migrations up to the file's version, and compares
  `(type, name, tbl_name, sql)` exactly; FTS5 shadow tables by name and
  type only; `sqlite_stat1`/`sqlite_stat4` are tolerated and reported.
  At most a few rows more than expected are read.
- **Codes** (`schema::codes`) are closed numeric codes for every value
  `v1.sql` constrains, pinned by tests, and the column-rule table of §8.7:
  each of the 81 source-value columns gets a stable code and its rule
  (key, raw, history, time, body raw). A test checks the table against
  `v1.sql` in both directions; the history columns are D82's closed list.
- **Migrations** run only through `Archive::migrate()` (D87): the current
  fingerprint is checked, free space must be at least three times the
  database plus the reserve (unknown space does not block), unfinished
  sessions are superseded, then each step runs with `foreign_keys` off in
  one `BEGIN IMMEDIATE` with a foreign-key check, the fingerprint of the
  target version and the FTS rank-1 integrity check before `user_version`,
  `archive_meta.schema_version` and a `schema_migration` row are written.
  Any failure rolls back and drops the write connection. M4 ships no real
  migration; a step not marked test-only is refused until the
  pre-migration copy exists (it ships with the first real migration).
- **`Archive::begin_write(Recovery::Run)`** is the first write: it sweeps
  `attachments/.incoming/<n>.partial` and `imports/session-<n>.json.tmp`,
  reconciles `blob_intent` (a placed file without a `blob` row is removed),
  and marks running sessions interrupted. `Recovery::Skip` opens the writer
  only. `Archive::close` checkpoints with TRUNCATE if a writer was opened.
- **Test hooks** (a failed lock self-test, test-only v2 schema plans, a
  skewed expected shadow DDL, a smaller op budget) exist only with the
  non-default `fault-injection` feature (D85), which only the crate's own
  tests enable.

**Owner decisions.** `v1.sql` and the codes implement D51 (the archive is
the de-duplication scope), D59 (`attributedBody`, `message_summary_info`
and `payload_data` are kept raw within caps), D60 (Recently Deleted
excluded by default) and D61 (text captured before an unsend is kept),
all four **owner-confirmed on 2026-10-18** (the M4 decision table below).
D69, D70, D71, D79 and D82 (the closed history-column list in
`schema::codes`) follow the spec's leans, which still await the owner's
confirmation.

**Not in this slice:** the import (4f–4j), `open_blob` and the verifier
(4k), the `StoreFs` trait, the `max_page_count` guard during a migration
step, provenance files, and any use from the shell.

### The import catalogue and its content model (M4 slice 4e)

`tests/fixtures/imports/` holds the synthetic backups the import is tested
on (spec §19.1), written by `scripts/fixtures/make_synthetic_imports.py`.
That generator imports the M2 and M3 generators as modules and leaves them
untouched, so the frozen catalogues cannot change by accident.

- **Twelve entries.** Device 1 as v1, as v2 (rowids shuffled, an edit, an
  unsend, a deleted message, new bytes for one attachment GUID, a lost blob,
  a dropped column, a later date), as v2 encrypted with the same sms.db
  bytes (equal rank), and as undated and out-of-range-date copies of v1's
  sms.db. Then a second device, hostile keys and values, attachment
  outcomes, Recently Deleted, full-text search text, and a `-wal` that is
  staged or missing.
- **Two pins.** `SHA256SUMS` covers the inputs and its own hash is pinned
  in `crates/oma-archive/tests/import_fixtures.rs`: the inputs are frozen.
  `EXPECTED.SHA256SUMS` covers `*.expected/` and `scenarios/`. Its own hash
  is pinned too (since slice 4f, with `import.json` and the scenario
  counts).
- **The content manifest** (`content.json`) is the generator's model of
  what an import must leave in the archive. It covers each row's key or
  unkeyed occurrence, a sample of `Col` states, history values,
  `display_text` or its none-reason, every body version, joins, the
  attachment order, the current blob and every blob read, Recently Deleted
  markers, "not in the latest import" per lineage, and the rows the
  Recently Deleted policy excluded. It also states each session's rank. It
  restates §6.2, §6.5, §8.7, §8.8, §9 and D58; it does not run the Rust
  code. There is one manifest per entry (imported alone) and ten
  `scenarios/` (sequences). Where a scenario lists two orders, the
  generator asserts that the model gives the same content for both.
- **What is checked now.** `import_fixtures.rs` checks both sum files. It
  also checks that every entry opens and that the M3 check reads it as
  readable (`WalSkipped` only for the missing `-wal`). Against the Rust
  readers it checks the model's keyed rows and row counts per entity (the
  R6 key passes), every content digest (staging), the order key, its
  source, the out-of-range qualifier and the 41-byte rank (the plist dates
  read by `oma-backup`), and every attachment hash (`copy_file_to`). The
  manifests themselves are asserted against archives from slice 4f on
  (handles and chats so far).
- **Readings the model makes,** for the import slices to confirm or change
  (they re-pin `EXPECTED.SHA256SUMS`, never the inputs):
  - an empty GUID or handle address is not a usable key, so such a row is
    unkeyed;
  - `current_blob_id` is the blob of the highest-ranked session that read
    bytes. A session that read none never moves it, which keeps the result
    order-independent when a newer backup has lost a file;
  - a Recently Deleted message already in the archive is re-observed for
    its lineage and its marker only;
  - under the default exclusion, importing v1 then the Recently Deleted
    backup differs from the reverse order in the markers, which §8.7 states.
    **It also differs in "not in the latest import"**: in the reverse order
    the deleted messages were never seen by the newer session, so they are
    labelled. §8.7's list of exceptions does not name this case yet;
  - the display none-reasons are named (`attributedBodyUnusable`,
    `textEmpty`, `noSource`); slice 4g fixed their codes as 1, 2 and 3
    (`DisplayNoneReason`).
- **Owner decisions.** The model applies D60 (Recently Deleted excluded
  by default, its attachments included when opted in), D61 (text from
  before an unsend stays as an earlier body version) and D59 (raw values
  kept within caps), all **owner-confirmed on 2026-10-18**, and D82 (the
  history columns), a lean that still awaits the owner's confirmation.

### The import core (M4 slice 4f)

`Archive::import` (`crates/oma-archive/src/import/`) is the import as
far as slice 4f builds it: staging and the probe, the session's facts, the
resume decision, the duplicate-key scan, the session bookkeeping, and the
first two passes, **P1 handles and P2 chats**, in batches. The shell does
not call it yet (slice 4l).

- **Facts first, reads only** (`session.rs`, spec §8.2). The content digest
  and its framing (`main_len`, the WAL outcome and length), the read
  profile (SHA-256 of every column cap and the row budget), the code
  versions, the options and the lineage form the **binding** a resume must
  match. The rank is 41 bytes, compared with memcmp: the order class, the
  Status.plist `Date` (else Info.plist `Last Backup Date`) biased by 2^63,
  then the digest; a date outside 2007-01-01 .. import start + 48 h is
  ignored and sets `backupDateOutOfRange` (D81). The qualifiers bitmask
  holds the WAL ones (which decide "full observation" later), the encoding,
  the size mismatch, unknown tables and columns, and "iOS version
  untested".
- **Resume or supersede** (§8.2 step 1). A session in state 0, 2, 4 or 6
  whose binding equals this import's is continued (`resumes + 1`, counts
  reloaded); every other unfinished session is superseded (state 5) and
  its map, occurrence and duplicate-key rows are deleted (D64). **A
  continued session writes at its stored rank** and keeps its
  `source_backup` row: the order key is not part of the binding, so a
  resume after the host clock moved, or from a later backup with
  byte-identical sms.db but a newer Status.plist date, would otherwise
  merge one session at two ranks (§8.7). The session-local rows of a
  session that ends for good (superseded here or by `migrate()`, or
  failed) are deleted in the transaction that records its state, through
  one helper (`session::supersede_sessions`, `drop_session_local`); each
  import also deletes any such rows a failed or superseded session still
  owns.
- **The key scan runs before the resume decision**, on every import,
  so the gate can show its evidence before the lineage is chosen; a continued session keeps its stored duplicate set (the same
  set, as the digest is bound). Staging reports its bytes as the
  `StagingMessagesDb` phase.
- **`source_backup`** holds the plist strings exactly as recorded
  (`oma_backup::RawText`: not sanitised, not truncated), never the display
  copies. A value of the wrong type or over 4,096 bytes is NULL with its
  bit set in `wrong_type_fields` or `oversized_fields`; NULL with neither
  bit is absent. `observed_key` hashes each field in its state.
- **Keys** (`keys.rs`, `preflight.rs`, §6.2, D56, D83). The tagged encoding
  (0 Absent, 1 Null, 2 Unreadable(kind), 3 Oversized(u64), 4 Value) is
  pinned byte for byte. A handle is keyed by the tagged (address, service),
  every other entity by its exact GUID bytes, when the identifying field is
  a readable, **non-empty** value and its 128-bit key digest is not in the
  session's duplicate set. The key scan sorts the digests of all four keyed
  tables (cap 4,194,304 rows each) and persists the duplicated ones in
  `import_dup_key`; a resume reuses them. Every other row is unkeyed: a
  SHA-256 fingerprint over every raw field (with the storage class of text
  and the raw value of an unreadable cell) and a durable occurrence number
  from `import_occurrence`.
- **Batches** (`batch.rs`, §8.5, D63). A pass reads records into a buffer
  with no archive transaction open, until 1,000 rows (16–8,192), 32 MiB
  (1–256 MiB) or 2 s on the context's clock (tests turn the time budget
  off). The buffer is then written in one `IMMEDIATE` transaction with one
  savepoint per record, the importer's own cursor (the last completed
  rowid), the counts JSON and the issue deltas. After any statement error
  the batch rolls back, and `ROLLBACK` is issued if SQLite left the
  transaction open; nothing commits after an error. `SQLITE_FULL` stops the
  session resumably (state 6); a constraint error is a bug (state 3,
  `Internal`). `rows_done` must equal the probe's `count(*)`. A pass's
  `read` takes `&self` (so P6 can hold its attachment locator), and
  `before_batch` lets a pass commit work of its own before its batch (P6's
  intents and placement, §9.1); handles and chats use neither.
- **Merge** (`merge.rs`, §8.7, D57, D82). Read the stored row and its
  `value_issue` rows, decide per cell, then `UPDATE` only the changed
  columns. A Value or Null observation replaces the stored cell at a rank
  at least `last_rank`, or when the stored cell is Absent, Oversized or
  Unreadable; those three never replace a Value or Null and are counted as
  "not applied". A history column keeps every other observed Value or Null
  in `value_history` (set semantics, hashed with the storage class). A
  changed cell's `value_issue` rows are rewritten. `first_rank` and
  `last_rank` move by min and max; an equal rank keeps the earlier
  session.
- **Cancel** (§8.6). The callback checks the token at the start of each
  record; the engine checks it every 128 rows. Either way the completed
  records are committed and the session becomes cancelled (2), resumable.
- **Until finalize existed** (slices 4f to 4i), a session whose passes
  all ran stayed running (state 0) and the tests stood in for finalize
  between steps. Since slice 4j finalize completes it (below), so an
  import of the same content after a completed one is a fresh session
  that re-observes it: the model's per-step counts and order independence
  are asserted for every scenario, including the four that repeat a
  digest.
- **Crash points** (`fault.rs`, D85). `fault::point(FaultPoint::…)` marks
  the record start, before and after each batch commit, after the session
  transaction and the terminal state, each recovery step of `begin_write`,
  and both sides of the creation rename. Without the `fault-injection`
  feature the call is empty. `tests/crash.rs` kills a re-executed child at
  every point in `FaultPoint::ALL` (the terminal commit through a child
  that cancels during the chat pass) and checks that the archive is
  consistent and that continuing gives the uninterrupted result. The
  checks after a kill run on a **copy** of the database, `-wal` and
  `-shm`, and the test asserts that the `-wal` the kill left is unchanged
  when the resume starts, so the resume opens a hot WAL and the dead
  child's `-shm`. Each child stages under a directory the parent removes.
  `tests/resume.rs` does the same in process with panics at random batch
  boundaries and with a cancel at every record.
- **`StoreFs`** (`store_fs.rs`). Creation makes its directory fsyncs and
  its no-replace rename through a `StoreFs` (`RealFs` in production), so a
  recording implementation can check the order: subdirectories durably
  made, the creation directory flushed, the rename, then the library
  directory flushed. Before the rename the creation directory gets the
  permission checks every open runs, so a drive that ignores modes (vfat
  or exFAT with the usual umask) fails creation instead of leaving an
  archive no open accepts. A failed library flush **after** the rename is
  a logged durability warning, and the id is returned. The intent
  reconcile flushes each `attachments/<hh>/` it removed a file from,
  through the same trait, before the intents' deletion commits. The
  attachment store (slice 4i) uses the same trait, which gained
  `remove_file` (a default method) for partial files and replaced orphans.
- **Store paths** (`blobs.rs`, `store_dir`). Before any file under
  `attachments/<hh>/` is touched, `attachments/` and `<hh>` must each be
  a real directory (no link, junction or reparse point) and, on Unix,
  private (`check_private`). The reconcile, P6's "already archived" check
  and its placement go through it, and so do the verifier and
  `open_blob` (slice 4k). A
  planted link at `<hh>` leaves the intent in place and counts the file as
  left.
- **Counts** (`counts.rs`). `ImportCounts` is numbers only; the handle and
  chat groups hold seen, inserted, already present, unkeyed, cells
  replaced, not applied, history added and value-issue cells.
  `import.json` (per entry) and `scenarios/*.counts.json` hold the model's
  counts, and `EXPECTED.SHA256SUMS` is now pinned.

**Readings this slice takes.** These are implementation readings of the
spec, not owner decisions. `owner-decisions.md` existed when this slice
was written and confirms D51, D59, D60 and D61 (owner-confirmed
2026-10-18; see the M4 decision table); D69, D70, D71, D79 and D82 remain
leans awaiting the owner's confirmation.

- an empty GUID or address is not a key (the catalogue model's reading,
  now implemented);
- **`value_history.first_rank` and `first_session_id` mean "no earlier
  than"**, not "the observation that first saw the value". A value that
  leaves the current slot is recorded at the row's `last_rank` and
  `last_session_id` before the replacing observation, and those move with
  any higher-ranked observation of the row, including one where this
  column was absent, oversized or unreadable. Example: S1 (rank r1)
  observes `chat.display_name` 'A', S2 (r2 > r1) lacks the column, S3
  (r3 > r2) observes 'B'; 'A' goes to history with first rank r2 and
  session S2, which never observed 'A'. Imported in the order S1, S3, S2,
  the same row records r1 and S1. Exact with two sessions; with three or
  more, the rank is an upper bound on the first observation, the session
  may not have observed the value, and the provenance depends on order.
  This is an exception to §8.7's order independence beyond the spec's
  list (which names column values, not history provenance); the value set
  in `value_history` itself is order independent. Exact per-value
  provenance needs a per-cell rank and is left for the v1 freeze review;
- a key's tagged encoding ignores the text storage class (two sessions
  that disagree only in class are one key), while fingerprints and history
  include it;
- the resume binding is the spec's list only: a second backup with the
  same sms.db bytes but other plist dates continues the first one's
  session, and the continued session keeps its stored rank and
  `source_backup` row (tested with a clock change between segments in
  `tests/resume.rs`);
- the unkeyed fingerprints are pinned byte for byte against digests
  computed outside the crate (`bind.rs`), because they are the identity
  of every unkeyed row across sessions.

### Messages, bodies and the FTS index (M4 slice 4g)

Slice 4g adds the preflight's Recently Deleted steps and **P3 messages**
(`import/preflight.rs`, `import/messages.rs`) to `Archive::import`.
The passes now run P1, P2, P3; a session whose passes ran still stays
running until finalize (slice 4j, below).

- **P0** (spec §8.2 step 2). `for_each_recoverable` gives the sorted,
  distinct message rowids of `chat_recoverable_message_join`; a join row
  whose `message_id` is not a readable integer names no message. It is
  re-run on every resume. `recoverable_message_part.part_text` is never
  read (the reader's authorizer denies it). **Over 4,194,304 join rows the
  backup is `NotImportable(OverRecentlyDeletedCap)`**: the spec says no
  Recently Deleted content is imported over the cap, and the only way to
  guarantee that without the set is to import nothing (a reading of §8.2
  and §23; nothing is written).
- **The Recently Deleted classification** runs inside the message key
  scan, on the reader connection under an op budget that grows with the
  set. With the default exclusion (D60), a Recently Deleted row whose GUID
  is a usable key, unique in this source and already a keyed message of
  the archive is **re-observed only**; every other one (an unkeyed or
  duplicated one included) is **excluded**. Only digests outlive a row.
  With the opt-in, nothing is classified and every row merges.
- **P3** reads `for_each_message` with decoding on and raw blobs kept
  (D59). Per record, in its savepoint:
  - an excluded row is skipped: no row, no map, no join later
    (`recentlyDeleted.excluded`);
  - a re-observed row is mapped and recorded in `lineage_seen` only: no
    column, body or rank changes (`recentlyDeleted.notUpdated`);
  - every other row is keyed or unkeyed as in slice 4f. The unkeyed
    fingerprint covers the 41 message cells and the body's `raw_hash`.
    Its raw cells merge by the column rules. **Derived columns are
    recomputed from the stored raw cells after the merge** (`service_class`,
    `item_kind`, the `assoc_*` parse through `oma_messages::AssocRaw`,
    `sort_key`, `first_src_rowid`), and only those that changed are
    written, with `derived_by`. `handle_ref` and `other_handle_ref`
    resolve the raw rowid through this session's map, and are set only by
    the observation that takes the `handle_rowid` cell (a rowid means
    something inside its session only); 0, a non-value and an unmapped
    rowid give NULL.
- **`sort_key`** (§6.5): `0x00 ‖ be64(date_utc_ns XOR 2^63)` when `date`
  is in seconds or nanoseconds, else `0x01 ‖ first_rank ‖
  be64(first_src_rowid XOR 2^63)` (50 bytes). `first_src_rowid` moves with
  `first_rank`.
- **Body versions** (`message_body`). The raw cells are `text`,
  `attributedBody` and `message_summary_info` (exact bytes or their
  states, with their `value_issue` rows as entity 5); `raw_hash` is
  SHA-256 of `oma-body-raw-v1` and the three cells' fingerprint
  encodings. One row per (message, `raw_hash`), inserted with `ON
  CONFLICT DO NOTHING`; first and last rank move by min and max.
  `current_body_id` moves to this observation's version when the record
  is new or its rank is at least the message's `last_rank`. An edited
  message gains a version; an unsent one gains an empty version and **its
  earlier text stays indexed and searchable** (D61). A stored version
  whose `decoder_version` differs from the build's is **re-derived in
  place** (derived columns updated, child rows deleted and re-inserted);
  the FTS update trigger re-indexes only when `display_text` changed. A
  re-observation at a rank at least the version's `last_rank` refreshes
  its edit label, which also reads `date_edited`, a message column.
- **Child rows**: `message_body_part` from the decoded parts (UTF-16 and
  UTF-8 spans, the first transfer GUID and mention, flags and present
  keys as bits in the order pinned by `ATTR_FLAG_BITS` and
  `ATTR_KEY_BITS`), `message_edit_entry` from `ec` (stored order; `at_kind`
  1 integer, 2 real, 3 plist date with `at_raw` NULL), and
  `message_unsent_part` from `rp`. Edit-entry bodies (`t`) are not decoded
  (§0.2); their bytes are inside the raw summary.
- **`display_text`** (D58, §10): the decoded `attributedBody` when it
  decoded to non-empty text (`display_source` 1); else `text` when it is
  non-empty valid UTF-8 (2); else `text` decoded lossily (3); else NULL with
  `display_none_reason` 1 `attributedBodyUnusable` (the column holds a
  value, or one over its cap or unreadable, that gave no text), 2
  `textEmpty` (`text` is `''`) or 3 `noSource`. `sources_differ` compares
  the decoded text with `text` when both are values, and is NULL
  otherwise. These codes, `ItemKindCode`, `AssocKindCode`,
  `EditLabelCode`, `TsErrorCode` and `SummaryTimeKind` are new closed
  codes in `schema/codes.rs`, pinned by a test.
- **FTS** (D62): the `message_fts` triggers from schema v1 index every
  version's `display_text`; nothing writes the index from Rust. The rank-1
  `integrity-check` runs after every catalogue import in the tests, and
  the tokenizer table of §10 is asserted on `imp-fts-plain`.
- **Counts**: `messages` (the entity group, without Recently Deleted
  skips), `bodies` (added, unchanged, re-derived, current moved, display
  sources and none-reasons, sources differ, decode failures, run issues)
  and `recentlyDeleted` (P0's messages, excluded, not updated, imported).
  The generator's `import.json` does not count messages yet; P3's counts
  are asserted by targeted tests (`tests/messages.rs`, `tests/resume.rs`).
- **Tests**: every catalogue entry and every scenario in every order is
  compared with the content model for messages (columns, history, handle
  reference, display text, every body version, sort order, exclusions);
  a second session of each entry changes nothing; edits, unsends, an older
  backup imported later, re-derivation, the tokenizer table, the Recently
  Deleted policy and D58 on synthetic bodies (with decoded vectors from the
  M3 typedstream catalogue) have their own tests; the cancel-at-every-record,
  random-crash and kill-at-signal tests now run through P3 (the synthetic
  source has messages, undecodable bodies and Recently Deleted rows).

### Links, attachment rows and Recently Deleted markers (M4 slice 4h)

Slice 4h adds the passes after P3 (`import/links.rs`, `import/rd.rs`,
`import/attachments.rs`, `import/recently_deleted.rs`). The passes now run
P1, P2, P3, P4, P5, P5b, P6, P7, P8; a session whose passes ran stayed
running until finalize (slice 4j, below).

- **P4 chat ↔ handle, P5 chat ↔ message, P7 message ↔ attachment.** Each
  join row's two source rowids are resolved through this session's
  `import_rowid_map`, so a link always joins two committed archive rows. A
  row whose side does not resolve (unreadable, NULL, or no imported row) is
  **counted, never inserted** (`unresolved`, by side). Links are a union:
  never removed. `last_rank`/`last_session_id` follow the highest rank that
  saw the link; `first_session_id` the lowest (an equal rank keeps the
  stored one), so the result does not depend on import order.
  `chat_message.join_date` follows the raw rule of §8.7 against the
  link's `last_rank`, and its `sort_key` is copied from the message (the
  `message_sort_au` trigger keeps it in step). `message_attachment.ord_rank`
  and `ord_rowid` are replaced when the rank is at least `ord_rank`: the
  attachments of a message are ordered by the highest-ranked backup that
  saw each link (§6.5). A join cell's `value_issue` detail is not kept,
  because the link tables have no single row id; its `_st` is.
- **The Recently Deleted decisions after P3** (`RdSkip`) are re-derived
  from durable state: of the rows the preflight classified, those P3 did
  not map in this session were left out and those it mapped were only
  re-observed. P5 and P7 **skip the joins of both** and count them as
  `skippedRecentlyDeleted`, not as unresolved: a re-observed message is not
  updated from its deleted state, links included (D60).
- **P5b and the preflight's step 4.** Two streams of
  `message_attachment_join`: the attachment rowids linked to a left-out
  message, minus those linked to any other message. What remains gets **no
  attachment row, no link and no file**; a join row
  with an unreadable side is ignored. The preflight runs the same scan on
  the preflight's excluded set and refuses a source whose candidates exceed
  4,194,304 distinct rowids (`NotImportable(OverRecentlyDeletedCap)`,
  nothing written); P5b re-runs it after P3 on every resume.
- **P6's attachment rows.** Keyed on the exact GUID bytes (unkeyed by
  fingerprint otherwise), merged by the column rules, mapped for P7 and
  recorded in `lineage_seen`. In slice 4h no file was read; the files,
  `blob`, `blob_intent`, `attachment_observation` and `current_blob_id`
  arrived with the store in slice 4i (below).
- **P8 markers.** One `message_recently_deleted` row per
  `chat_recoverable_message_join` row whose message is mapped in this
  session (imported with the opt-in, or re-observed by default), with the
  chat resolved when it can be and the raw chat rowid, delete-date quad and
  `ck_sync_state` kept. Two join rows for one message are two markers.
  Join rows of left-out messages are counted as `notMarked`.
- **The staged copy was only read.** After the last pass the import
  checks, as M3's check does, that the staged connection is `query_only`,
  that its authorizer refused nothing (`part_text` included) and that it
  changed no row; otherwise the session fails. None of these is the guard;
  the authorizer is.
- **Counts**: `chatHandles`, `chatMessages` and `messageAttachments`
  (`LinkCounts`: seen, inserted, already present, unresolved and by side,
  skipped for Recently Deleted), `attachments` (the entity group), and in
  `recentlyDeleted` `attachmentsExcluded`, `marked` and `notMarked`.
- **"Not in the latest import"** (§8.7) needs no new column: a lineage's
  newest full observation is read from `import_session`, and a row it
  labels is one whose `lineage_seen.last_rank` for that lineage is lower.
  The tests compute it that way; the reader query is M5's.
- **Tests**: every entry and every scenario order now also match the
  model's chat handles, message chats, message attachments in the §6.5
  order, Recently Deleted markers, attachment rows and their columns and
  history, the excluded attachments, and "not in the latest import" per
  lineage (`tests/import_core.rs`); content snapshots, now with the
  attachment, link and marker tables, are equal in both orders.
  `tests/links.rs` covers the Recently Deleted attachments and markers,
  rank-following links in both orders, re-observation and hostile orphan
  joins; `tests/lineage.rs` the per-lineage labels and a cancelled or
  WAL-skipped latest session. The synthetic crash and resume source now
  has attachments and all three join tables (orphans, a NULL side, a
  repeated join, extreme join rowids, an attachment linked only to a
  Recently Deleted message), and the cancel test stops at every record of
  every pass.

### The attachment store and disk space (M4 slice 4i)

Slice 4i gives P6 its files (`import/attachments.rs`, `blobs.rs`) and the
import its space rules (`space.rs`).

- **Phase A, while the pass reads, with no archive transaction open.**
  Each record is classified: copying off (`notRequested`, and no lookup
  at all), Recently Deleted-only (no row, no file), no lookup attempted,
  not in the backup, blob or key unusable, or copyable. A copyable file is
  **already archived** when the attachment's current blob, found by exact
  GUID, has a store file that is a regular non-link file of `byte_len`
  and the backup's file has that plaintext length: it is not read
  (presence and length only, D66). Otherwise the space for it is checked,
  `attachments/.incoming/<n>.partial` is created new (0600), the plaintext
  streams into it through `OpenBackup::copy_file_to` and a 1 MiB
  `BufWriter` (`into_inner` surfaces the last flush), and it is synced and
  length-checked. A failed copy removes its partial file (five tries on
  Windows; one that stays is counted and swept at the next write open).
  `EFBIG`/`ERROR_FILE_TOO_LARGE` is counted per file and the pass goes on;
  a changed source, a cancel or no space ends the batch at the record
  before. A batch ends after 64 copied files, 256 MiB copied, or one file
  of 64 MiB or more.
- **Phase B** commits a `blob_intent` for each copied digest with no
  `blob` row. **Phase C** places each file (`Placer`): a no-replace
  rename when nothing is there; for a referenced name (or one placed
  earlier in the batch) the copy is dropped if the store file is regular
  with the right length (`deduplicated`), and otherwise left alone
  (`storeConflict`); an unreferenced file is re-hashed through
  `open_nofollow` and adopted if it matches its name, else removed (a
  link is removed itself, never its target; a directory is a conflict)
  and replaced (`orphanReplaced`). Each touched `<hh>` is flushed once
  after the renames. **Phase D**, in the batch transaction: the attachment
  merges, `blob` (`ON CONFLICT DO NOTHING`), one `attachment_observation`
  per row and session (path class, lookup and copy codes, the blob read,
  `size_matched_manifest`), `current_blob_id`, and the deletion of the
  batch's intents. The order per file is write → `sync_all` → intent
  commit → rename → directory flush → row commit; the crash tests kill at
  each step.
- **`current_blob_id`** is the blob of the highest-ranked session that
  read bytes for the attachment (rank compared as bytes, as everywhere); a
  session that read none never moves it. This is the catalogue model's
  reading, and it keeps the result order-independent when a newer backup
  lost a file.
- **An exception to §8.7's order independence: same GUID, same length,
  different bytes.** "Already archived" compares presence and length only
  (D66, §9.1, §9.2), so when two backups hold different bytes of the same
  length under one attachment GUID, whichever is imported first is
  stored and becomes `current_blob_id`; the second finds a file of that
  length, reads nothing, and its bytes are never stored. Imported in the
  other order, the other version is kept. This is the cost of not
  re-reading every archived attachment on every import; it is pinned by
  `store.rs` (`same_length_different_bytes_keep_the_first_imported_version`).
  Re-copying when a higher-ranked session meets a lower-ranked blob would
  close it, at the price of re-reading every attachment of every newer
  backup; that change is left for the v1 freeze review.
- **Partial files are never left to the next import.** A copied file
  whose record is not committed is removed at once and counted
  (`partialsLeft`) if it cannot be: when the intent transaction or the
  space guard before it fails (phase B), when the placement's `blob`
  lookup fails, and when a cancel lands after the copy but before its
  record was buffered. The counts kept outside the records (partial files
  left, directory flushes) stay pending until a batch commits, so a batch
  that rolls back into a resumable stop still records them. A `Partial`
  that is dropped unplaced (a panic) makes one uncounted removal attempt;
  the next write open's sweep remains the backstop for a process kill.
- **The preflight estimates** (§8.2 step 5, §12): every attachment is
  looked up and probed once more; the plaintext lengths of the files still
  to copy are summed (Recently Deleted-only and already-archived files left
  out), with the largest file, giving `SpaceEstimate`. Above the available
  space it only warns; below `reserve + wal_allowance` the import returns
  `ImportOutcome::InsufficientSpace` before the gate. Otherwise the
  estimate is shown at the gate (`Preflight::space`) and returned in
  `ImportSummary`.
- **The space guard** (`SpaceGuard`) runs before every write transaction
  of the passes (P6's intent transactions included): below the floor the
  session stops (state 6, `insufficient_space`); otherwise
  `max_page_count` is set to the page count plus the room above the floor
  and read back, and any value other than the one requested stops the
  session too (SQLite never sets it below the current size). Unknown free
  space lifts the limit; the limit is lifted again when the passes end.
  `SQLITE_FULL` and `ENOSPC` stop the session the same way; it resumes
  once space is freed.
- **The source** must be unchanged: a file whose stamp changes during its
  copy, or `unchanged_since_open()` false (or unreadable) after P8, stops
  the session with `source_changed` (state 6). The records before it are
  committed with their files.
- **Codes and counts.** New closed codes, pinned by a test:
  `PathClassCode`, `LookupCode` (lookup and probe outcomes) and
  `CopyCode`. `ImportCounts.attachmentFiles` counts every P6 row once in
  an outcome (copied, replaced, deduplicated, adopted, orphan replaced,
  already archived, store conflict, not requested, not looked up, not in
  the backup, blob unavailable, over the limit, too large for the volume,
  read failed, write failed), plus size mismatches, partial files left,
  bytes copied, flushes, new blobs and current-blob moves.
- **Fault points**: after a partial file is created, at the first write of
  a copy, after `sync_all`, after the intent commit, after a rename, and
  after the directory flushes; hooks fail a copy's first write with an OS
  error and give the space guard a fixed page room.

**Readings and deviations this slice takes** (implementation readings, not
owner decisions):

- **A Manifest `Size` mismatch is copied.** §9.1 step 1 names only
  `Present`, but §9.2 says a mismatch "is recorded, and the bytes are
  stored anyway"; the generator's model copies it too.
- **The plaintext length of an encrypted file** is its Manifest `Size`
  (the probe already checked it against the padded blob), through a new
  `AttachmentKey::declared_size()` in `oma-backup`. Without a usable
  `Size` the file is copied again rather than declared archived. The
  estimate then counts the stored length, an upper bound.
- **"Already archived" is looked up by exact GUID.** An unkeyed attachment
  is always copied again; the store deduplicates it.
- **A conflict with a referenced blob** still records that blob in the
  observation (the bytes read hash to it), so `current_blob_id` may name
  a blob whose file the verifier will report. A conflict where no `blob`
  row exists records none.
- **An I/O error placing one file** is counted (`writeFailed`) and the
  pass goes on; a failed directory flush fails the session, because the
  rows must not commit before their files' entries are durable.
- **Progress for `CopyingAttachments` is in rows**, not bytes (§8.12).
- **`EFBIG` on the database** is not mapped to `Stopped { VolumeFileLimit }`
  yet: SQLite reports it as an I/O error, which fails the session.
- **Counts outside a record** (a partial file that could not be removed,
  flushes) are added to the next committed batch; a pass that stops
  before buffering any record of a batch loses them (the partial file is
  still swept).
- **`K`** stays at the provisional 3.0 in code; the synthetic measurement
  is in `compatibility.md`.

### The gate, finalize and provenance (M4 slice 4j)

Slice 4j completes the import: `Archive::import(ob, opts, gate, ctx)`
(renamed from `import_core`) asks an `ImportGate` before the first write,
then runs finalize and writes the provenance file. Its outcome is
`ImportOutcome`: `Completed`, `Cancelled`, `Stopped`, `Declined`,
`NotImportable` or `InsufficientSpace`.

- **The gate** (`import/gate.rs`, spec §8.2 step 7, D65). After every
  preflight read, the import builds a `Preflight` and calls
  `ImportGate::confirm`; the answer is `Proceed { lineage }` or `Decline`.
  **Decline writes nothing**: no write connection has been opened, and a
  test hashes the database, its `-wal`, `attachments/` and `imports/`
  before and after. A cancel while the gate waits is the same. The shell's
  blocking gate with its 2-minute timeout came in slice 4l
  (`src-tauri/src/archives/gate.rs`); `ProceedWith` and
  `ProceedWithSuggestion` are the fixed gates tests and tools use.
- **What the preflight shows.** The resume plan: `Continue { session,
  lineage, from }` (the first pass not done, or `Finalize`) for an
  unfinished session whose binding matches except for the lineage, or
  `Fresh { supersedes, reason }`. Whether a completed session already
  imported the same content. Every lineage (at most 1,000) with its
  session and full-observation counts, the backup date of its newest full
  observation, and **booleans only** for whether the backup's product type
  and device name equal that session's (D51); the values are compared
  inside the crate and never leave it. The suggested lineage: the one the
  continuable session belongs to, else the one lineage whose model and
  name both match, else a new one (D80). The key tallies per keyed table
  (rows, keyable, distinct duplicated keys, rows to be imported unkeyed)
  and the message keys already archived (the overlap; zero with a
  non-empty archive is shown, not refused). The Recently Deleted counts,
  the space estimate, the order facts (dated, the date's source, out of
  range, older than the archive's highest-ranked completed session, a
  different date source than that session), the encrypted source, the
  cloud-sync folder and Windows ACL warnings, and the qualifiers. The
  message-key scan now looks up every unique message key in the archive
  (one indexed read each on the read-only connection), not only the
  Recently Deleted ones.
- **The lineage decides the resume.** The decision is made again with the
  chosen lineage: an unfinished session continues only when that lineage
  is its own; otherwise it is superseded. An `Existing` lineage that does
  not exist is refused before anything is written.
- **The staged sms.db is removed after P8,** before finalize (§8.11);
  `ImportSummary::staging_removed` says whether it was.
- **Finalize** (`import/finalize.rs`, §8.9), each step its own
  transaction with the space guard before it and a cancel honoured between
  steps: `chat_stats` (one row per chat, with the archive interrupt armed
  around its single statement), `archive_stats`, the session-local rows in
  chunks of 50,000, then the terminal state (completed, `ended_at_ns`,
  `full_observation`, the final counts), the FTS merge steps (`merge =
  500`, armed, until `total_changes()` grows by less than 2, at most
  10,000 steps) and `wal_checkpoint(TRUNCATE)`. `full_observation` is 1
  unless a WAL-related qualifier is set (§8.7). A cancel or error before
  the terminal state records the session cancelled, stopped or failed as
  after a pass (`Pass::Finalize`, code 9, in `import_issue`); a resume
  finds every pass done and runs finalize again. **The terminal state is
  best effort:** if it fails, nothing else is recorded, the session stays
  running, the next write open marks it interrupted, and the outcome is
  `Stopped { InsufficientSpace }` for `SQLITE_FULL` (an error otherwise).
  After it the session is completed whatever follows: a cancel only skips
  the remaining merge steps, and a failed merge step or checkpoint is
  reported in `FinalizeReport`, never an error.
- **The armed interrupt** (`conn::armed`, §8.6) is a progress handler on
  the write connection that interrupts the one statement it wraps once the
  cancel token is set. It is removed before the caller's `COMMIT`; batch
  statements, the terminal update and every commit run disarmed (a test
  cancels at a batch's commit point and the batch still commits).
- **Provenance** (`provenance.rs`, §8.10, D70): `imports/session-<n>.json`
  is written at every terminal state the import records (completed,
  cancelled, stopped, failed) and rewritten when a continued session ends
  again: `.tmp` created new (0600), written, synced, renamed, `imports/`
  flushed. It holds the format and version, the session, its state,
  versions, the import's own start and end times, resumes, the order class
  and date source (not the date), `fullObservation`, the lineage number,
  the product type and version and whether the backup was encrypted, the
  qualifier names, the WAL outcome, the options and the counts
  (re-serialised from `ImportCounts`). It holds no device name, GUID,
  digest, rank, message date, path or label. A failure is logged by kind,
  sets `provenance_written = false` and never changes the outcome. The
  reader accepts only `^session-[0-9]+\.json$`, at most 64 KiB, opened
  without following links, with a matching format, version and session.
- **New fault points** (D85): while the gate waits; after the statistics;
  after each cleanup chunk; after each merge step; before the checkpoint;
  after the provenance `.tmp` is created and before its rename; inside a
  migration step and before its commit. `tests/crash.rs` kills at every
  one (a test fails if a point in `FaultPoint::ALL` has no kill): before
  completion the session continues to the uninterrupted result; after it
  the session is completed and consistent, the next write open sweeps the
  `.tmp`, and the archive equals the uninterrupted one; a killed migration
  step leaves version 1 and migrates afterwards. The observer hook
  (`hooks::observe`) receives every fault point and, through rusqlite's
  commit hook on write connections, every commit, for `tests/ordering.rs`.
- **`tests/ordering.rs`** records every `StoreFs` call with the observed
  points and commits in one sequence and asserts, for every placed file,
  write → `sync_all` → intent commit → rename → directory flush → row
  commit, and for creation, database commit and `.oma-new-*` flush before
  the rename and the library flush after it.
- **`tests/query_plans.rs`** pins `EXPLAIN QUERY PLAN` for every
  index-dependent import statement, taken from the crate as it prepares
  them (`hooks::plan_statements`: the merge reads by GUID, natural key and
  fingerprint, the value-issue read, the body-version read, the overlap
  lookup, the current-blob read, the map resolution, the occurrence
  counter and the finalize `GROUP BY`), and for the M5 shapes of §6.5 (the
  timeline keyset, FTS to the current body, a message's attachments in
  order). No import statement scans a table except the statistics' one
  pass over `chat`, and none sorts.
- **The golden archive** `tests/fixtures/archives/golden-v1/` (§7.3,
  §19.2) was made by `cargo run -p oma-archive --example make_golden` from
  `imp-dev1-v1-plain` then `imp-dev1-v2-plain` (one lineage, fixed clock),
  with its `SHA256SUMS` and `logical_snapshot.json`. `tests/golden.rs`
  checks the sums, opens a copy read-write (`begin_write` with recovery),
  runs the integrity, foreign-key, FTS and store checks, and compares the
  snapshot. It is regenerated only until schema v1 freezes at the M4
  release tag. Since slice 4k it also passes quick and full verify.

**Readings and deviations this slice takes** (implementation readings, not
owner decisions):

- **The preflight has no M3 extraction status.** The import computes no
  `ExtractionReport`; the qualifiers it records are shown instead. The
  shell already has the M3 check.
- **"Latest" for `sortsBeforeLatest`** is the archive's highest-ranked
  completed session in any lineage, because the lineage is not chosen yet
  when the gate is shown.
- **A cancel after the terminal state is `Completed`**, with
  `fts_merge_complete = false`: the session is complete and resumable work
  is not needed.
- **A failed terminal update leaves no provenance file** (the session is
  still running); the file appears when the continued session ends.
- **Superseded and interrupted sessions get no provenance file**: they are
  set by another import or by the next write open, not at their own end.
- **The `-wal` and `-shm` are left** after `Archive::close`, because the
  read-only connection closes last; the golden generator removes the empty
  `-wal` and the `-shm` from the fixture.

### The integrity verifier and the blob reader (M4 slice 4k)

`Archive::verify(depth, ctx)` (`verify.rs`, spec §11, D69) is
**report-only**: it repairs, sweeps and reconciles nothing, so it reports
what a crash left instead of cleaning it up. It runs under the archive's
lock. Its reads use the ReadOnly connection in a **cancellable** mode (the
progress handler stops the statement once the cancel token is set; there
is no op budget, because a verify runs only on the user's request). It
opens the writer with `begin_write(Recovery::Skip)` and writes only its
results: a `verify_run` row (inserted with no outcome when the verify
starts, so a crash leaves a visible unfinished run, and completed with the
outcome and the counts as JSON at the end), and in a full verify each
blob's `verify_state` and `verified_at_ns`. Recording is best effort: if
it fails, the report still comes back, with `run: None`.

| Check | Quick | Full | Reported as |
|---|---|---|---|
| Header, fingerprint and `archive_meta` (the open-time `inspect`, run again) | ✓ | ✓ | `SchemaCheck`; on anything but `Ok` nothing else runs and nothing is written |
| Sessions whose deriver or decoder version is above `archive_meta`'s high-water mark | ✓ | ✓ | count |
| `sqlite_stat1`/`sqlite_stat4`; newer rules | ✓ | ✓ | booleans, not problems |
| `message_fts_config` holds `secure-delete = 1` | ✓ | ✓ | check state |
| `PRAGMA quick_check(100)` / `PRAGMA integrity_check(100)` | quick | full | check state and the rows other than `ok` (a corruption error counts as one) |
| `PRAGMA foreign_key_check` | ✓ | ✓ | total, and by child table (`v1.sql` names only) |
| FTS rank-1 `integrity-check`, on the writer under the armed interrupt | ✓ | ✓ | check state (`Failed` is drift or corruption) |
| `value_issue` and `value_history` rows whose entity row is missing or whose entity code is unknown (entities 6 and 7 have no single row and are skipped) | ✓ | ✓ | counts |
| The label re-validated | ✓ | ✓ | boolean |
| Sessions in state 0, `blob_intent` rows | ✓ | ✓ | counts (crash leftovers) |
| Each `blob`: `storage_name` parses and spells `sha256`; a regular non-link file of `byte_len` | ✓ | ✓ | bad name, missing, not regular, length mismatch |
| Each `blob`: re-hashed to the end through `open_checked`, progress in bytes | — | ✓ | hash mismatch, unreadable; `verify_state` set for every row |
| `attachments/` walk: two levels, no link followed, at most `blobs + 10,000` entries | ✓ | ✓ | entries, unexpected names (and directories below `<hh>/`, never entered), orphans, links, partial files, truncated |
| `imports/`: every entry read through `provenance::read` (name, at most 64 KiB, no link, format, version and session), at most `sessions + 1,000` entries | ✓ | ✓ | readable files, unreadable entries, `.tmp` leftovers, truncated |
| Owner and mode of every archive file and directory (`check_private`; on Windows only links and types) | — | ✓ | count |

The outcome is `Cancelled` if a cancel stopped it, else `Problems` if any
problem was found, else `Incomplete` if a check could not run (an
unlistable directory, a read error, no write connection for the FTS
check), else `Ok`. `VerifyCounts::has_problems` names which fields are
problems; the totals, `stat_tables` and `newer_rules` are not. A cancel is
recorded as cancelled (outcome 2) and the checks that ran keep their
results. An archive that needs a migration is refused with
`NeedsMigration`; one written by newer rules verifies normally.
`ArchiveReader::last_verify` reads the newest run back as a
`VerifySummary`, dropping any table name in the stored counts that is not
one of `v1.sql`'s. Progress is `ArchiveEvent::Verify` with
`VerifyPhase::{CheckingDatabase, CheckingIndex, CheckingFiles}`. The full
verify's recompute of derived columns is deferred (§21.2).

**`open_blob`** (`ArchiveReader::open_blob` and `Archive::open_blob`,
`blobs::open_checked`, spec §4.3, §9.5) is the one way attachment bytes
are read back, by M5, M6 and the verifier: the row's `storage_name` must
parse and spell its `sha256` (`BlobProblem::BadName` otherwise);
`attachments/` and `<hh>/` must be private real directories, and the file
is opened with `open_nofollow` and must be a regular file of `byte_len`
bytes (`Missing`, `NotRegular`, `LengthMismatch`, `Io`). The returned
`BlobReader` yields at most `byte_len` bytes, hashes them as they stream,
and fails its final read with `InvalidData` (and every read after it) when
the length or the digest differs, so a caller that reads to the end gets
only the archived bytes. It is lock-free (a reader works while an import
holds the lock; placed files are never replaced). `BlobId` is an
archive-local row number.

**Readings and deviations this slice takes** (implementation readings, not
owner decisions):

- **FTS drift is caught only by the rank-1 check.** Observed on the
  bundled SQLite 3.53.2 (`tests/verify.rs`): `quick_check` and
  `integrity_check` pass on an external-content index whose content was
  changed with the triggers off. Dropping a trigger, the spec's example,
  changes the schema and is refused by the fingerprint at open, so the
  test drifts the content instead.
- **A failed schema check stops the verify** without writing anything
  (five checks are counted as not run), because nothing else may read
  tables whose schema is not ours (§4.5).
- **The `imports/` scan has its own bound** (`sessions + 1,000` entries);
  the spec bounds only the `attachments/` walk.
- **Leftovers count as problems** (running sessions, intents, partial and
  `.tmp` files): they are what a crash left, and only the next import's
  write open cleans them up.
- **A quick verify writes no blob state;** only the full one sets
  `verify_state` (also for missing, short and badly named files).
- **The verify writes are not under the space guard.** They are one row
  and one small update per 512 blobs; a failure leaves `run: None`.

### The archive commands and jobs (M4 slice 4l)

The shell side of M4 (spec §15) lives in `src-tauri/src/archives/`
(Tauri-free: listing, details, the gate, the workers, DTOs, sentences) and
`src-tauri/src/commands/archives.rs` (the six commands, each wired through
`generate_handler!`, `COMMANDS`, an `allow-<kebab>` grant and a wrapper in
`src/services/ipc.ts`).

| Command | Input (`deny_unknown_fields`) | Returns |
|---|---|---|
| `list_archives` | — | `ArchiveListingDto { entries, libraryState, libraryError }` |
| `create_archive` | `{ label? }` (≤ 200 UTF-8 bytes, no control or bidi characters; empty means none), else `invalidInput` | `ArchiveSummaryDto` |
| `get_archive_details` | `{ archiveId }` | `ArchiveDetailsDto`: summary, sessions, last import (state, stop reason, counts), last verify, active job, the preflight while the job waits, `resumable`, `matchingBackupIds`, `resumeOptions` (the unfinished session's two options) |
| `start_import` | `{ backupId, archiveId, password?, options: { includeRecentlyDeleted, copyAttachments } }` and the channel | `JobStarted` |
| `confirm_import` | `{ jobId, proceed, lineage?: { kind: "existing", lineageId } \| { kind: "new" } }` | `()`; `jobNotWaiting` when the job is not at the gate, `invalidInput` without a lineage or with one the preflight does not list |
| `verify_archive` | `{ archiveId, depth: "quick" \| "full" }` and the channel | `JobStarted` |

- **Jobs by subject (D73).** `ActiveJob` holds a `JobSubject` (a backup,
  an archive, or both for an import). `reserve` answers `busy` when an
  active job shares either, then applies the one slot (D7). An import's
  reservation carries its `Gate`. Changing the archive location is refused
  while any job runs (an archive is only open inside a job).
- **The gate (D65).** `archives::gate::Gate` is a `Mutex` + `Condvar`;
  its deadline comes from a `GateClock` (two minutes; tests inject a
  clock and advance it), and the wait re-reads the clock and the cancel
  token at least every 250 ms. The worker's `ImportGate` opens the gate,
  publishes the preflight in the job table, sends `importPreflight` and
  waits. `confirm_import`, `cancel_job` and the exit handler (which
  cancels and wakes every gate, then waits up to 5 s) end the wait; every
  ending but Proceed declines, and the crate writes nothing on Decline.
- **The import job** (`archives::worker::run_import`): `prepare_job`'s
  steps (re-inspect, the password, the location, the reservation,
  `confirm_library_unchanged`) after a read-only look at the archive;
  then the staging sweep, `Library::open` (read-only first, under the
  lock), `OpenBackup::open` and validation, `Archive::import` with the
  shell's gate, the backup's `close` (which removes the staging folder)
  and the archive's `close`. Exactly one terminal event:
  `importFinished` (`completed`, `stopped` with its reason, the
  `cancelledAtGate` endings `declined`, `cancelled` and `timedOut`, and
  `notImportable` with its reason and sentence), `cancelled` for a cancel
  before the gate or during the passes, or `failed`.
- **The verify job** (`run_verify`): `Library::open`, `Archive::verify`,
  `close`; `verifyFinished { outcome, recorded }`.
- **Phases.** `JobPhase` gains one phase per `ImportPhase` and three
  verify phases (`verifyingDatabase`, `verifyingIndex`,
  `verifyingFiles`, distinct from the validation phases). Both crate
  enums are non-exhaustive: a phase this build does not know is not
  forwarded, and a test pins every known one.
- **The list** merges `ArchiveEntry::InUse` with the job table: an archive
  this app is importing into or verifying shows `importing` or
  `verifying`.
- **DTOs** (`archives/dto.rs`) clamp every counter (with `saturated`),
  send dates as Unix milliseconds, device facts as `modelDiffers`,
  `nameDiffers` and `deviceMatches`, and have one free-text field,
  `ArchiveSummaryDto.label`. A structural guard over the generated
  declarations (shared with the Backups guard through
  `src-tauri/src/ts_guard.rs`) fails on any other `string` field and on
  keys such as `guid`, `digest`, `rank`, `rowid`, `cursor` or `path`.
- **Error codes.** Sixteen new `ErrorCode`s, each with one fixed sentence
  (`archives::messages`), mirrored in `services/ipc.ts`'s `ERROR_CODES`
  (enforced by `src/types/error-codes-exhaustive.ts`); a test maps every
  `ArchiveError` variant and every import outcome.
- **The library additions.** `ArchiveReader::last_import` (the newest
  session: state, lineage, counts, stop reason, provisional) and
  `ArchiveReader::resume_matches`, which compares listed backups' Info.plist
  device name, model and backup date (to the second) with the newest
  unfinished session's source inside the crate, for
  `matchingBackupIds`.

**Readings and deviations this slice takes** (implementation readings, not
owner decisions):

- **`importPreflight` carries only the job id.** The preflight (up to
  1,000 lineages) cannot be guaranteed under 1 KiB, so, as for the
  extraction report (D47), it is fetched with `get_archive_details`,
  which also re-shows it after a reload.
- **The archive jobs run in `archives::worker`**, started through
  `worker::spawn_with`, not inside `worker::run`; they keep its
  guarantees (slot freed before the event, exactly one terminal event).
- **`importFinished` carries more than §15.3 lists:** the gate ending,
  the session number, the not-importable reason, and a `notice`
  (`{ code, message }`) with the sentence for `stopped` and
  `notImportable`. `plaintextRemoved` is `null` when no session ran.
- **A refusal for space before the gate** (`ImportOutcome::InsufficientSpace`)
  ends with `failed` and `insufficientSpace`; a validation that is not
  importable ends with `importFinished` `notImportable`
  (`backupNotImportable`).
- **A verify cancelled before it starts** ends with `cancelled`; one
  cancelled while it runs ends with `verifyFinished` `cancelled` (its run
  is recorded).
- **The verify's per-table foreign-key counts stay in the archive;** only
  the total crosses, so no table name is a DTO string.
- **An import job stores no validation report** in the backup's last
  outcome (only its cleanup), so the Backups page keeps its own last
  report.
- **`matchingBackupIds` is a pre-selection** by device name, model and
  Info.plist date; whether a session continues is still decided by the
  crate's resume rules (content digest and framing).

### The Archive page (M4 slice 4m)

The UI of spec §16, over the six archive commands. Counts, states and the
label only; no message content, address, chat name or file name.

- **`src/stores/archives.ts`** mirrors `stores/backups.ts`: the listing,
  per-archive details and errors, one heard job (import or verify), the
  import results from `importFinished` and `verifyFinished`, and the
  dialogs' state. It polls `get_archive_details` every 2 s while a job runs
  on an archive (heard or not), which gives the phase of a job the page
  cannot hear and the gate after a reload. The import password passes
  through `submitPassword` into `startImport` and is stored nowhere.
- **The gate** (`ImportPreflightDialog.vue`) is shown from the store's
  `gate`: the heard job once `importPreflight` arrived and the details carry
  its preflight, else any archive whose details say an import waits there.
  Gates answered from this page are remembered, so a stale poll cannot
  re-show one. Each fact is its own sentence (`services/importText.ts`);
  the suggested lineage is pre-selected; Cancel, Escape and the backdrop
  decline. The dialogs are mounted once in `App.vue`
  (`ArchiveDialogs.vue`), so the gate appears on whatever page is open.
- **Pages.** The Archive page lists the archives with their status
  badges and shows the selected one: actions (Import from a backup…,
  Continue importing… when `resumable` (its import dialog starts from the
  unfinished session's `resumeOptions`, because a continue must use the
  same options, D64; a preflight with other options says
  `optionsDiffer`, not that the backup or the app changed), quick and
  full verify, Cancel
  through the progress card), the last import (`ImportSummary.vue`), the
  last verify (`VerifySummary.vue`, one sentence per finding kind), the
  archive's counts and its sessions. The Backups page's **Import
  messages…** opens the import dialog with that backup selected, enabled
  under the same rule as Validate; the dialog offers **Create archive
  first…** when there is none. `roadmap.ts` marks `archive-database`
  available and rewords `hardening` (D69).
- **Progress.** `JobProgress.vue` takes any `ProgressJob`
  (`src/types/progress.ts`). Counted phases are rows; the staging copy, and
  `verifyingFiles` in a full verify, count bytes. A job the page cannot
  hear shows its phase without a count.

**Readings and deviations this slice takes** (implementation readings, not
owner decisions):

- **`CopyingAttachments` counts attachment rows, not bytes** (spec §8.12
  says bytes): the crate reports rows (slice 4j), and the label says so.
- **The import dialog's button is "Prepare import"**, since the import
  first stops at the gate; the password dialog says "Unlock and prepare
  import" (its `submitLabel` prop).
- **Labels are checked in the webview too** (200 UTF-8 bytes, no C0/C1
  controls or bidirectional formatting characters), before IPC; the
  backend's `Label::parse` remains the rule.
- **"Older than the newest backup" and the zero-overlap sentence** follow
  the preflight's flags; zero overlap is stated only when the archive
  already has an import set.
- **The temporary copy line** ("was deleted: yes/no") is shown only for
  the session the page heard finish, from `stagingRemoved`; after a
  reload it is not known and not shown.
- **Backdrop clicks decline the gate**, like Escape. Declining writes
  nothing.

### Status and qualifiers (D43)

The report's status is `Readable { qualifiers }`, `Damaged`, `Unsupported`
or `Incomplete`. **D43 is the decision record for how invariant 4 (I5,
"an unknown schema or iOS version is reported as unsupported") is read in
M3:** an *unknown schema* is one that lacks the required minimum, has
virtual tables, has an unsafe rowid shape, or exceeds our schema limits,
and that is `Unsupported`. A database with the minimum plus unknown extra
tables or columns, an untested iOS version, or a WAL or source caveat is
*readable with qualifiers*, and each qualifier is stated in its own
sentence. The UI never says just "can be read" when a qualifier applies.
The shell adds `iosVersionUntested` unless the backup's iOS version is in
`TESTED_REAL_IOS_VERSIONS`, which is empty in M3 because no real backup has
been tested, so every readable result from a real backup carries it.
(Damaged, unsupported and incomplete results carry no qualifiers; their
iOS version is untested too.) Without this
reading, every real backup today would be "unsupported" and the check would
give no evidence; the qualifiers keep the claim honest.

### Constant-time reasoning

No step in M2 gives an attacker a timing oracle: the app is local and
single-user, and nothing is decrypted on anyone else's behalf. The code never
compares key material by hand; aes-kw compares the public integrity value
with `==`; PKCS#7 unpadding is not constant-time; fileIDs are not secret. A
future network feature must re-examine this.

## Key decisions

| Decision | Choice | Reasoning and trade-off |
|---|---|---|
| Who touches the filesystem | Rust only | One place to enforce the invariants. The cost is that every capability needs an IPC command. |
| How the archive directory is chosen | The backend opens the native picker | The webview cannot inject a path, even after an XSS. The cost: no drag-and-drop of folders until a validated path is designed for it. |
| Bootstrap's JavaScript | Not loaded; CSS only | Bootstrap's JavaScript mutates the DOM behind Vue. Dialogs use native `<dialog>`, and overlays and toasts are Vue components. |
| IPC type sharing | ts-rs, generated from Rust | No hand-maintained duplicate types, and drift fails CI. |
| Device communication (M7) | Tentatively the `idevice` crate (MIT, pure Rust, includes mobilebackup2) rather than shelling out to `idevicebackup2` | Passwords stay in the process, never on a command line, with no LGPL binaries to ship. The crate is pre-1.0 and may break between releases, so pin it exactly. This must be confirmed on real hardware in M7. See [supported-formats.md](supported-formats.md). |
| Reference implementations | GPL, LGPL and CC BY-SA projects (imessage-exporter, imessage-database, crabstep, crabapple, pymobiledevice3, pyiosbackup, libimobiledevice including `idevicebackup2`, the andrewdotn answer) are behavioural references only | The project must remain Apache-2.0. Clean-room code, or MIT/BSD sources with attribution. |
| sms.db parsing | Capability-based: probe columns by name and treat missing ones as absent | The schema changes with every major iOS release. Positional `SELECT *` breaks silently. |
| Node.js worker | None | Nothing so far needs one. Revisit only with a concrete requirement. |
| Single instance | Not enforced for the app itself; enforced per archive from M4 | Atomic settings writes keep two instances safe. Staging sessions are locked, and the sweep skips sessions younger than 5 minutes. From M4 each archive has a sidecar `archive.lock` held for every write open, and a lock self-test at open refuses a volume whose locks do not exclude a second handle (`LockUnreliable`); a second instance sees the archive as `inUse` and cannot write to it (D53). The lock-free `ArchiveReader` still reads committed state. |

### Milestone 2 decisions

| # | Topic | Choice | Main alternative |
|---|---|---|---|
| D1 | Where IPC types live | DTOs in the shell (`src-tauri/src/backups/dto.rs`); `oma-backup` has no ts-rs | `TS` in the library behind a feature |
| D2 | Progress transport | `tauri::ipc::Channel<JobEvent>`, no `core:*` grant; re-verified on every Tauri upgrade | `job://progress` events with `core:event:*` |
| D3 | Cancellation | `cancel_job` sets a `CancelToken`, checked in every loop, plus a SQLite interrupt watcher per connection | Discard results after uninterruptible work |
| D4 | PBKDF2 | Written in the crate on `hmac`, with cancellation and progress; differential tests against the `pbkdf2` crate | Call `pbkdf2::pbkdf2_hmac` (up to ~5 s uninterruptible) |
| D5 | Session lifetime | Scoped to the job; keys zeroized and staging removed at its end | Keep the unlocked session open (M3 needs that) |
| D6 | Staging location | `<library>/.oma-staging/s<16hex>/`; validation needs a usable archive folder | The app cache directory |
| D7 | Concurrency | One job globally; a second request gets `busy` | Parallel jobs |
| D8 | Remembering added folders | `backupFolders: [{ path, kind }]`, lenient; schema version stays 1 | Re-add every session |
| D9 | Guards on files opened from a backup | Regular, non-link files only; each `xx/` directory validated once | Canonicalise every blob, or `O_NOFOLLOW` |
| D10 | Extraction destination | Sequence numbers inside staging (`x/<n>`, `db/<n>/<basename>`) | Mirror `relativePath` |
| D11 | sms.db during validation | Checked in place; no plaintext copy on disk | Extract and open a full copy |
| D12 | Fixtures | Committed Python generator on `cryptography`, `SHA256SUMS`, and a recorded oracle run | Generate fixtures in Rust |
| D13 | Library in a synced folder | Allowed, with a warning before every validation | Block, or require an acknowledgement |
| D14 | Identifying fields | Never stored, UDID included: root keys are skipped while streaming (values decoded and discarded), and no struct has a field for them. Backup folder paths, usually named after the UDID, are shown in the details panel and, for an added backup folder, in the Locations panel; never logged | Parse and redact |
| D15 | Persistent diagnostic log file | Deferred to M4 | Ship in M2 |
| D16 | OS credential store for passwords | Dropped, with no milestone planned; the password is asked for on each validation | Opt-in storage |
| D17 | Plist parsing | Budgeted event-stream reader | `Value::from_reader` with size caps |
| D18 | Keybag strictness | Fail closed only on what decryption uses; the rest becomes a note | Refuse any unexpected shape |
| D19 | `relativePath` | A lookup key, not a path; unusual names are counted, never refused | Refuse rows that fail `resolve_within` |
| D20 | Scope | Domain classes, attachment counts and size measurement deferred to M3/M4 | Ship them in M2 |

D11 still describes M2's validation. For the extraction check it is
superseded by D22: sms.db is staged on disk.

### Milestone 3 decisions

| # | Topic | Choice | Main alternative | Trade-off |
|---|---|---|---|---|
| D21 | Session lifetime | One job: open → validate → check → close; the password is asked for on each run | Keep the session open between jobs | The password is asked twice, versus keys and plaintext lingering |
| D22 | sms.db plaintext | Staged on disk, removed after the row pass, Windows temporary attributes | `sqlite3_deserialize` into memory | Memory would scale with the database, and a `-wal` could not be applied. The disk copy is swept and documented in privacy.md |
| D23 | WAL | `BestEffort`, frames validated, the wording "staged" | Fail the whole check | One missing file would block the check, versus possibly missing the newest messages, which is always stated |
| D24 | Progress phases | Own `MessagesPhase` with an adapter | Add to `oma_backup::Phase` | `oma-backup` stays free of M3 knowledge |
| D25 | Reader API | Callback with `ControlFlow<B>`, a `Cursor`, resume tested | `Iterator` or `Vec` | Bounded memory and resumability for M4 |
| D26 | Typedstream | Clean-room module in `oma-messages` | Adopt `imessage-core` (MIT), or a separate crate | An unaudited young dependency versus our own code; it can be split out later |
| D27 | Timestamp units | Per value, unit windows, `Zero` as its own state | Per database or per iOS version | Some dates are withheld (and counted), versus mixing units |
| D28 | Schema problems | `Unsupported`, `Damaged`, `Incomplete` as values with reasons | Job errors | The UI can explain why, without a partial read |
| D29 | Attachment lookup | Cached single lookups | Batched `IN` | Simpler; Manifest.db's `variable_number = 10` stays |
| D30 | Attachment presence | Metadata probe plus key unwrap in memory | Presence only, or full decrypt | Catches a damaged `EncryptionKey` without writing plaintext |
| D31 | Report content | Aggregates and a date range (shown, never logged or persisted); a changed source is a qualifier | No date range; fail on change | Most useful sanity check; M4 will abort on a change |
| D32 | Raw blobs | Off by default; within caps when on | Always keep | Memory versus re-decoding later; M4 decides |
| D33 | Recently Deleted | Counted only | Ignore or import | Import policy belongs with M4's privacy design |
| D34 | Terminal event | New `extractionFinished` variant | A field on `finished` | M2's contract stays intact |
| D35 | Fixture generator | Separate `make_synthetic_messages.py` importing the M2 module, with a reference-environment check | A mode of the M2 script | The frozen M2 catalogue is untouched |
| D36 | Write protection | Authorizer, crate-private `conn()`, DQS off, triggers and views off, clippy list; end-of-check assertions are checks only | Convention plus `total_changes` | Enforced by SQLite rather than review |
| D37 | `quick_check` | Always, first after the scan; anything but `"ok"` is `Damaged` | Report and continue | A slightly damaged real database gives no counts; the reason is stated |
| D38 | Unknown names | Counted only | List them | Names are untrusted |
| D39 | Group chats | No group field; buckets of other linked handles; `style` raw | `style = 43` means group | That meaning is **UNVERIFIED** |
| D40 | sms.db size cap | 32 GiB, fixed | None, or a free-space check | Protects the volume; the error names the cap |
| D41 | `ErrorCode` | None new; stage-aware sentences | `MessagesDbUnreadable` | Fewer cross-cutting edits |
| D42 | Mutation testing | In-test seeded harness | `proptest` or `cargo-fuzz` | No shrinking, but a stable dependency tree |
| **D43** | **How invariant 4 (I5) is read** | See "Status and qualifiers" above: unknown schema means no required minimum, virtual tables, unsafe rowid shape or over our limits (**Unsupported**); unknown extras, an untested iOS version, WAL or source caveats are **stated qualifiers** | Treat any unknown column or untested version as unsupported | Every real backup would be unsupported, and the check would give no evidence |
| D44 | Virtual tables | Refused before `quick_check` | Let `quick_check` run their integrity code | No FTS or R-tree code runs on attacker data; revisit if real sms.db has them (**UNVERIFIED**) |
| D45 | Statement cost | Op budgets, plan-safe integer aggregates, GUID checks in Rust | Wall-clock deadlines, or `hard_heap_limit` | Deterministic bounds; `hard_heap_limit` is process-wide and was rejected |
| D46 | GUID sets | Sorted `Vec<u64>` of truncated SHA-256, 4,194,304-entry cap | A `RandomState` `HashSet` | Reproducible counts and half the memory; a crafted collision can change only a count |
| D47 | Event size | Summary-only event; the report is fetched by `get_backup_details` | Rely on the ACL-exempt channel fetch path | No dependency on a Tauri special case marked "remove in v3" |
| D48 | Scope cut | The deferred list in the README | Ship everything | Reviewable PRs |
| D49 | Source values | `Col<T>` | `Option<T>` | M4 can tell "not recorded" from "NULL" from "bad" |
| D50 | Lookup scope | Only `Library/SMS/Attachments/` and `StickerCache/` | Every `~/` and `/var/mobile` class | Never reaches non-Messages files; the other classes stay counts |

### Milestone 4 decisions

The full M4 list, D51–D88 (spec §22). **D51, D59, D60 and D61 are
owner-confirmed (2026-10-18).** Of the other questions put in the spec to
the owner, **D69, D70, D71, D79 and D82 are leans awaiting the owner's
confirmation**; the code implements the lean, and each is marked. The rest
are design decisions of the M4 spec.

| # | Topic | Choice | Main alternative | Trade-off |
|---|---|---|---|---|
| D51 | Device identity (**owner-confirmed 2026-10-18**) | The archive is the de-duplication scope; boolean device evidence at the gate and the user picks the lineage (D80); the UDID is never stored or hashed, D14 unchanged | Identify the phone by UDID | No device identifier in the archive, at the cost of the user choosing the lineage |
| D52 | Archive naming | `a<32hex>`; label in the database; created through `.oma-new-*` with a no-replace rename | Label as directory name | No untrusted names on disk |
| D53 | Locking | Sidecar `archive.lock` + `try_lock` + self-test; NORMAL locking with WAL; lock-free `ArchiveReader` | `locking_mode=EXCLUSIVE` | Details can be read during imports; external tools can still read the database |
| D54 | Volumes | Network and virtualised volumes refused for archives; FAT warned | Warn on virtualised | WAL and locks are unreliable there |
| D55 | Value model | `STRICT` tables; each source value stored with its `Col` state (`_st`); text stored raw as BLOB with its storage class in `value_issue`; time quads | A single nullable column | Wider tables, versus losing D49's distinctions between absent, NULL and unreadable |
| D56 | Keys | Exact GUID bytes, or the tagged (address, service); unkeyed rows by fingerprint and durable occurrence | Synthesise keys | No fabrication; unkeyed rows may repeat across changed sources |
| D57 | Merge | Never delete; rank decides; missing or unreadable values never replace values; only changed columns are updated | Last import wins | Older backups can be imported later without regression |
| D58 | Body source | Decoded `attributedBody`, else valid `text`, else lossy `text`, else NULL with a reason | Prefer `text` | M3 found `text = ''` next to a body |
| D59 | Raw blobs (**owner-confirmed 2026-10-18**) | Always keep the raw `attributedBody`, `message_summary_info` and `payload_data` bytes within the caps; disclosed in privacy.md | Keep only decoded text | Re-derivation with a newer decoder needs no backup; the archive holds more source bytes |
| D60 | Recently Deleted (**owner-confirmed 2026-10-18**) | Excluded by default; opt-in per import; attachments included when opted in; already-archived messages are marked but not updated from their deleted state | Import everything | The user decides per import; deleted-state rows never overwrite archived ones |
| D61 | Text captured before an unsend (**owner-confirmed 2026-10-18**) | Kept; the message is marked unsent by the sender; disclosed in privacy.md | Remove the earlier text | The archive keeps what it already captured, and says so |
| D62 | FTS | External content over `message_body.display_text`, unicode61 `remove_diacritics 2`, `detail=full`, guarded triggers, every version indexed, `secure-delete` | Writes from Rust; trigram now | Snippets, rebuild and drift checks; CJK limits stated |
| D63 | Transactions | `IMMEDIATE` batches (1,000 rows / 32 MiB / 2 s), a savepoint per record, the importer's own cursor, `synchronous=FULL` | Per-row transactions | Durable and resumable at batch granularity |
| D64 | Resume binding | Digest and framing, versions, read profile, options and lineage; otherwise supersede | Resume across any change | One rule set per session |
| D65 | Confirmation | A gate inside the import job (`ImportGate`), after every preflight read and before the first write; the shell waits for `confirm_import` at most two minutes; Decline, cancel, exit and the timeout write nothing | Two jobs | One staging and one password, versus a longer plaintext window (disclosed) |
| D66 | Attachment copy | `copy_file_to(Write)`, hashed while streaming; batched fsyncs; durable intents; no-replace placement; an orphan re-hashed before it is adopted; an archived file with the same length is not re-read | Re-read every file at import | Half the I/O; changed bytes at the same length are found only by a full verify |
| D67 | Storage names | `blob.storage_name` separate from `sha256`; the v1 CHECK ties them; M8's migration drops the CHECK for keyed names | Path = hash | M8 can switch to keyed names in one module |
| D68 | Disk space | Warn on the estimate; hard guards before every write transaction and file; `max_page_count` with read-back; the reserve clamped to 1–10 GiB | Refuse on the estimate | An unmeasured `K` never blocks an import that fits, and a wrong one never fills the disk |
| D69 | Verifier (roadmap: a lean **awaiting the owner's confirmation**) | The library verifier ships in M4, report-only, quick and full (slice 4k); the verify job is the shell's (4l) and its sentence per finding the UI's (4m); repair stays in M8 | All verification in M8 | The M4 brief asks for integrity checks; nothing is repaired before M8 |
| D70 | Provenance file (product fields: a lean **awaiting the owner's confirmation**) | `imports/session-<n>.json`, content-free, best effort, written at each terminal state; it includes the product type and version | A full copy of the session row | Survives database damage without personal data |
| D71 | Diagnostic log (a lean **awaiting the owner's confirmation**) | The persistent, redacted log file (D15) is deferred again, to M8 | Ship it in M4 | A new on-disk artifact needs its own review |
| D72 | Thumbnails | Deferred to M5; no `thumbnails/` directory in v1 | Reserve the directory | No speculative layout |
| D73 | Jobs | `reserve` by subject (backup, archive or both); `Import` and `Verify` kinds; one job at a time (D7) | Key jobs by backup only | An archive can never be double-booked |
| D74 | Staged sms.db lifetime | The whole import, the gate and P6 included; removed after P8, before finalize | Re-stage per pass | One copy, versus a longer plaintext window (disclosed) |
| D75 | `application_id` | `0x4F4D4131` ("OMA1"), unregistered | Register it | Registration is optional |
| D76 | Views | None in v1; views are disabled on every connection | Read views | `highlight()` fails in views with `TRUSTED_SCHEMA` off |
| D77 | Unicode normalisation | None in M4: raw bytes are stored and the tokenizer folds diacritics | `unicode-normalization` | No new crate; NFC and NFD forms match in search through `remove_diacritics 2` |
| D78 | Permissions | 0700/0600 on Unix, re-checked at every open; the library itself must be ours and not writable by others | Leave the umask | Plaintext archive; ACL inheritance on Windows is disclosed with the privacy write-up |
| D79 | Untested iOS versions (a lean **awaiting the owner's confirmation**; extends D43) | Imports are allowed; raw values are stored as read; derived columns are marked provisional through the session's `ios_version_untested` qualifier, which the gate states | Refuse imports from untested versions | Storing raw values approximates nothing, and derived values can be re-derived when the rules improve |
| D80 | Lineage | The user picks an existing lineage or a new one at the gate, with a suggestion and boolean device evidence; only the chosen lineage continues an unfinished session | Infer from device fields | No silent identity claim |
| D81 | Rank | (dated, bounded date, content digest), memcmp-compared; undated below dated | Session id as tie-break | Order independence; no host-clock wins |
| D82 | Value history (column list: a lean **awaiting the owner's confirmation**) | The closed list of 11 history columns keeps every other observed value | History for every column | Content kept where it matters |
| D83 | In-source duplicate keys | Every occurrence unkeyed; found by a sorted 128-bit digest scan; persisted per session | Key the first in rowid order | Rowid-order independence; a collision can only demote |
| D84 | Reader | `ArchiveReader`: lock-free, query-only, fingerprint first | Read through the locked `Archive` | Details during imports |
| D85 | Fault injection | Non-default `fault-injection` feature, per-thread hooks and fault points; crash by killing a re-executed child | Environment variables in release builds; `abort()` | No test hooks in shipped code; deterministic boundaries |
| D86 | Windows long paths | `win32-longpath` with a verbatim path; if WAL and `-shm` fail there in CI, refuse library paths whose deepest archive path exceeds 240 characters | `EXCLUSIVE` locking | Keeps D53's reader. On the `windows-2022` CI runner ([run 39137381988](https://github.com/VibeForge-Systems/From_the_Forge/actions/runs/39137381988)) WAL and `-shm` worked on `win32-longpath` with a `-shm` path over 260 characters, so the fallback is not applied. Not yet run on Windows 11 |
| D87 | Migrations | Explicit `migrate()` only; fingerprint first; space checked before anything changes; unfinished sessions superseded; a real migration refused until the pre-migration copy exists | Migrate on open | No surprise writes; reads never migrate |
| D88 | Stopped state | Session state 6, `stopped`, for resumable stops (space, a changed source, a volume limit) | Reuse cancelled or failed | "Failed" is kept for errors that cannot be continued. A file-size limit hit on the database itself (`EFBIG`) still fails the session; only per-file limits on attachments are handled |

## Logging

`tracing` writes to stderr, filtered by `OMA_LOG` (default `info`). Log
events record kinds, states and counts. They never record message bodies,
contact details, attachment contents or passwords. M1 does not log the
archive path at info level.

Milestone 2 logs backup ids (hashes), job ids, classification and phase
names, counts, durations, `io::ErrorKind`, raw OS error numbers, SQLite
primary result codes, and limit names and values. It never logs passwords
(not even their length), key bytes, paths, folder names, device names,
UDIDs, SnapshotState values, relativePaths, domains of individual rows,
fileIDs or SQLite message text. Two test binaries
(`crates/oma-backup/tests/log_hygiene.rs` and
`src-tauri/tests/log_hygiene.rs`) install a global capturing subscriber and
assert this over every fixture. Tauri's own `tracing` feature stays off.

Milestone 3 logs, at `info`, one line per phase change and one summary line
(totals, the status kind, elapsed time); at `debug`, failure-kind counts.
It also logs `OpenMode`, the WAL outcome kind, the integrity kind and
qualifier kinds. It **never** logs:

- any string from sms.db, including table, view, trigger, index and
  collation names, DDL text and `quick_check` output;
- any GUID, the content digest, the date range or any timestamp;
- raw code values;
- unknown column, table, class, attribute-key and plist-key names;
- `_ClientVersion`;
- attachment paths or fileIDs;
- typedstream offsets;
- panic payloads. The release build aborts on panic; the shell installs a
  panic hook that prints only the thread name and source location
  (`src-tauri/src/panic_hook.rs`).

`#[tracing::instrument]` without `skip_all` is banned in `oma-messages` and
`src-tauri`, because it formats every argument with `Debug`. A source test
enforces this in `oma-messages` (`crates/oma-messages/tests/source_rules.rs`);
in `src-tauri` it is a review rule, and no `instrument` is used there today. The log-hygiene tests in
`crates/oma-messages/tests/` and `src-tauri/tests/` run every catalogue
entry, including the failure paths, and a forced panic.

Milestone 4's `oma-archive` logs, at `info`, archive creation, a write
open's recovery counts and migration steps (versions only); at `debug`,
placement and directory-sync outcomes, checkpoint results and unknown free
space. It never logs a path, an archive id, the label or SQLite error
text, and its errors carry codes and kinds only.
`crates/oma-archive/tests/log_hygiene.rs` checks logs, `Debug` and
`Display` against the label, the id, the library path and any 32-hex run;
`tests/source_rules.rs` bans `instrument` without `skip_all`.

The shell (slice 4l) logs archive job ids, kinds, phases and outcomes,
never an archive id or label; `src-tauri/tests/log_hygiene.rs` imports and
fully verifies every import catalogue entry through the jobs and checks
the log for planted sentinels, the password, the UDID, the device name,
temporary paths, the archive id and the label, and every event, details
and listing JSON for planted sentinels, the password and the UDID. Together with spec §13.2, M4 never logs natural keys,
fingerprints, content digests, ranks, attachment hashes or storage names,
source rowids and cursors, device names or product types, display text,
values from `value_issue` or `value_history`, or SQLite error text (only
the primary code).

The persistent, redacted diagnostic log file was deferred to M4 (D15); M4
defers it again, to M8 (D71, a lean awaiting the owner).
