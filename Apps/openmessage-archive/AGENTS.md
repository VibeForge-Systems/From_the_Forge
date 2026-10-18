# AGENTS.md — OpenMessage Archive

Guidance for AI coding agents working in `Apps/openmessage-archive/`.
Repository-wide rules — licensing, sign-off, branches, the root gate — are in the
root [`AGENTS.md`](../../AGENTS.md). Paths below are relative to this directory.

## What this is and where it stands

A local-first desktop app that imports local iPhone backups. It extracts the
Messages database and attachments into a searchable SQLite archive.

- **Stack:** Tauri 2, Rust, Vue 3 with TypeScript, Bootstrap 5 (CSS only),
  and SQLite with FTS5.
- **Targets:** Windows 11 and Linux.
- **License:** Apache-2.0.

Work proceeds in milestones; the table and current state are in
[`README.md`](README.md#roadmap). **Milestone 1 (the shell, settings and
archive location) is done.** **Milestone 2 (backup discovery and
validation) is implemented**: `crates/oma-backup`, the Backups page, six IPC
commands and a channel-based job model. Its sign-off is pending the
recorded review of the fixture generator and the manual tests in
`docs/compatibility.md`. **Milestone 3 (the extraction check) is
implemented**: `crates/oma-messages` (schema probe, typed record passes, a
clean-room typedstream decoder, the aggregate `ExtractionReport`), the
`check_extraction` command and the report on the Backups page. It shows
counts only and persists nothing. CI passes on `ubuntu-22.04`
and `windows-2022`. Its sign-off is pending the clean-room review of the typedstream
decoder and the fixture generators (`scripts/fixtures/PROVENANCE.md`), and
the manual checklist in `docs/compatibility.md`. Milestone 4 (the archive
database and import) is next.

Read these before designing anything:

| Doc | Read it for |
|---|---|
| [`docs/architecture.md`](docs/architecture.md) | The five invariants, the crates, the IPC boundary, the extraction check, and the decision records (D1–D50; D43 is how "do not fabricate" is read for the check) |
| [`docs/supported-formats.md`](docs/supported-formats.md) | Sourced research on the backup format, encryption, the sms.db schema (columns with sources, timestamps, codes, typedstream) and device tooling. Items marked **UNVERIFIED** are hypotheses, not facts. |
| [`docs/privacy.md`](docs/privacy.md) | What is copied, where and for how long, including the extraction check's plaintext copy of sms.db |
| [`docs/compatibility.md`](docs/compatibility.md) | What has actually been tested |
| [`docs/development.md`](docs/development.md) | Setup and every command |

## Commands

Run all commands from this directory.

```sh
npm ci                                                  # JS dependencies
npm run tauri dev                                       # run with hot reload
npm test                                                # Vitest (frontend)
npm run typecheck                                       # vue-tsc
npm run licenses                                        # JS license audit
cargo test --workspace                                  # Rust tests; also regenerates IPC types
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo deny check                                        # Rust licenses and advisories
npx tauri build --no-bundle                             # release binary
```

Linux system packages are listed in `docs/development.md`, starting with
`pkg-config` and `libwebkit2gtk-4.1-dev`. If a build fails in `glib-sys`,
`gobject-sys`, `gio-sys` or `gdk-sys` with "pkg-config could not be found",
those packages are missing; this is not a code problem.

### Before opening a PR

CI (`.github/workflows/openmessage-archive.yml`) runs each of these, so run
them first:

1. `npm run licenses`
2. `npm run typecheck`
3. `npm test`
4. `cargo fmt --all --check`
5. `cargo clippy --workspace --all-targets -- -D warnings`
6. `cargo test --workspace`
7. Confirm `src/types/generated/` has no uncommitted changes.
8. `cargo deny check`

Then run the root gate. Note that the root gate does **not** build this app.

## Invariants: never violate these

Each one is enforced in code. Keep it that way.

1. **Source backups are read-only.**
   - Never write inside a backup, inside `MobileSync/Backup`, or inside
     iMazing's default store `iMazing/Backups`.
   - Open source databases read-only, from copies.
   - `crates/oma-fs` guards location choice.
2. **Paths from the outside go through `oma-fs`.** Any path built from
   backup data uses `oma_fs::resolve_within`. Any name written to disk uses
   `oma_fs::sanitize_filename`. Never `Path::join` an untrusted string
   directly.
3. **No network.**
   - Do not add HTTP client crates or CDN-hosted assets, and no telemetry.
   - The CSP in `src-tauri/tauri.conf.json` allows `connect-src` to IPC
     only, with no `unsafe-inline` in production. Do not loosen it.
4. **The webview never supplies filesystem paths.** Paths come from native
   dialogs that Rust opens, or from settings Rust has already validated.
   Input types use `#[serde(deny_unknown_fields)]`.
5. **Do not fabricate.**
   - A value missing from the source stays `None`.
   - A message status the source does not record is never shown.
   - An unknown schema or iOS version is reported as unsupported, never
     approximated.
6. **Never log message bodies, contact data, attachment contents, or
   passwords.** Log kinds, states and counts. Never log a backup password,
   and never pass one on a command line.
7. **Render untrusted text as text.** No `v-html` with message content.
   Exports must escape their output.

## Where code goes

- **Domain logic belongs in `crates/`.** `src-tauri/` stays a thin layer of
  IPC commands and state. `oma-fs`, `oma-backup` (M2) and `oma-messages`
  (M3) are implemented; `oma-archive` (M4) is in progress (its skeleton:
  create, open, lock, schema v1, fingerprint, migrations; and the import
  core: sessions, the Recently Deleted and duplicate-key scans, the merge
  engine, the handle, chat and message passes with body versions and the
  FTS index, the link passes, the attachments with their files in the
  content-addressed store, the Recently Deleted markers, the space
  estimate and guards, the gate, finalize and the provenance files,
  `Archive::import`; and the report-only verifier `Archive::verify` with
  `open_blob`), driven by the shell's six archive commands and its
  import and verify jobs (slice 4l), and the Archive page (slice 4m); the planned
  crates are `oma-export` (M6) and `oma-device` (M7). Each must build and test without
  Tauri. The shell's `src-tauri/src/backups/` (registry, job table, worker,
  DTOs, messages) and `src-tauri/src/archives/` (listing, details, the
  gate, the import and verify workers, DTOs, sentences) have no Tauri
  types either, so they are tested without a window.
- **Frontend:**
  - Views live in `src/views/`, components in `src/components/`, and Pinia
    stores in `src/stores/`.
  - The only path to the backend is `src/services/ipc.ts`.
  - Do not import Bootstrap's JavaScript. Use Vue components with Bootstrap
    classes, and native `<dialog>` for modals.

### Adding an IPC command

A command missing from any of these steps is denied at runtime:

1. Write the handler in `src-tauri/src/commands/`.
2. Register it in `generate_handler!` in `src-tauri/src/lib.rs`.
3. Add its name to `COMMANDS` in `src-tauri/build.rs`.
4. Grant `allow-<kebab-name>` in `src-tauri/capabilities/default.json`.
5. Add a typed wrapper in `src/services/ipc.ts`.

IPC types are Rust structs with `#[derive(TS)] #[ts(export)]`. Run
`cargo test` to regenerate `src/types/generated/`, then commit the result.
Never hand-edit generated files.

### UI rules

- **No control may look functional when it is not.** An unfinished capability
  renders disabled, with `<PlannedBadge capability="...">`, sourced from
  `src/services/roadmap.ts`. When a capability ships, flip its `status` there.
  `tests/frontend/roadmap.spec.ts` asserts which capabilities are available,
  so update that test in the same change.
- **Shortcuts are declared only in `src/services/shortcuts.ts`.** The help
  dialog renders from it.
- **Archive sentences live in `src/services/importText.ts`,** from exhaustive
  `Record<…>` tables over the DTOs' closed kinds, so a new kind fails the
  typecheck until it has words. The archive UI shows counts, states and the
  label only (rendered with interpolation), never message content.
- **The archive dialogs are mounted once, in `App.vue`**
  (`components/ArchiveDialogs.vue`), so the import gate shows on any page
  while an import waits. `stores/archives.ts` owns the job, the gate (live,
  or re-shown from `get_archive_details` after a reload) and the 2-second
  details polling; it never stores a password.

## Dependencies and licensing

- **Pin exact versions.** That means `=x.y.z` in `Cargo.toml` and exact
  versions in `package.json`, where `.npmrc` sets `save-exact`. Record every
  addition or upgrade in the `docs/dependencies.md` upgrade log.
- **Use permissive licenses only**, per `deny.toml`. Five MPL-2.0 crates that
  come in through Tauri are allowed **by name**. Never add MPL, or any other
  copyleft license, to the global allowlist.
- **Never copy code from the GPL, LGPL or CC BY-SA references:**
  imessage-exporter, imessage-database, crabstep, crabapple,
  pymobiledevice3, pyiosbackup (GPL-3.0), libimobiledevice including
  `idevicebackup2` (LGPL-2.1+), and StackOverflow answer 13793043. They are
  behavioural references only. MIT `iphone_backup_decrypt` may be ported
  with attribution.

## Testing

- **Rust tests live beside the code.** Filesystem tests use `tempfile`.
- **Frontend tests live in `tests/frontend/*.spec.ts`.**
  `tests/frontend/helpers.ts` provides snapshot and status builders, plus a
  `matchMedia` stub, because jsdom has none.
- **Fixtures in `tests/fixtures/` must be synthetic.** Never commit a real
  backup, real messages, or real contacts, even "anonymized" ones.

## Gotchas found the hard way

- **Fake Tauri `invoke` by hand, not with `vi.fn()`.** Vitest 4 reports a
  plain-object rejection returned from `vi.fn()` as unhandled, even when it is
  caught. See `tests/frontend/ipc.spec.ts`.
- **A release binary must be built through the Tauri CLI**, with
  `npx tauri build`. Plain `cargo build --release` omits the
  `custom-protocol` feature, and the resulting binary tries to load the dev
  server.
- **`legacy-peer-deps=true` in `.npmrc` is deliberate.** npm 10 crashes
  resolving vitest 4's peer graph. Do not remove it without re-testing
  `npm install`.
- **`npm ci` fails with `EPERM` or symlink errors on VM shared folders**
  (vboxsf, 9p). Build from a local disk.
- **Job progress uses a `tauri::ipc::Channel`, which needs no `core:*`
  permission.** That relies on a Tauri special case marked "remove in v3"
  (large payloads are fetched through an ACL-exempt internal command).
  Re-verify by hand on every Tauri upgrade that a `finished` event over
  8 KiB still arrives.
- **Crypto crates run at `opt-level = 3` in dev and test builds** (see the
  root `Cargo.toml`); otherwise the key-derivation tests are slow.
- **Fixtures are regenerated only with
  `scripts/fixtures/make_synthetic_backups.py`,** and
  `tests/fixtures/backups/SHA256SUMS` must be committed with them. Any change
  invalidates the oracle run recorded in `docs/compatibility.md`.
- **`tests/fixtures/**` is binary in `.gitattributes`,** so Git for Windows
  never converts line endings in the fixtures.
- **Never open the research scratch directory's GPL sources** (the
  downloaded reference implementations) while writing code. Facts already
  recorded in `docs/supported-formats.md` are fine to use.
- **The native `<dialog>` close event is asynchronous.** `ModalDialog` only
  reports it when the dialog was meant to be open, so a stale event cannot
  close a dialog that was reopened in the meantime (the password dialog
  reopens immediately on a wrong password).
- **`StagedConnection::conn()` is crate-private; the authorizer is the
  read-only guard.** Staged sms.db connections are opened with an
  authorizer that allows only reads, with DQS, triggers and views off, and
  with op budgets (`with_op_budget`) on any statement whose cost grows with
  the data. Do not add a way to reach the raw `rusqlite::Connection` from
  outside `oma-backup`, and do not run an unbounded statement without a
  budget. The end-of-check `total_changes() == 0` assertion is a check,
  not the guard.
- **`recoverable_message_part.part_text` is denied.** It holds deleted
  message content. The authorizer refuses any statement that reads it; M3
  only counts that table's rows. Never name its columns in SQL.
- **Content types are `Redacted`, source fields are `Col<T>`.** Every
  value read from sms.db is a `Col<T>` (absent, NULL, unreadable,
  oversized, or the value), so "not recorded" stays distinct from NULL and
  from 0. Content and identifiers are `Redacted<SqlText>` or
  `Redacted<RawBlob>`: `Debug` prints only a length, and there is no
  `Display` or `Serialize`. Read them with `expose()` and never log the
  result. A report counter that depends on an optional column is
  `Option<u64>`; never turn `None` into 0.
- **Fixture generators refuse a different SQLite version.**
  `make_synthetic_messages.py` writes only under the reference
  `sqlite3` 3.45.1 (Python 3.13.16), unless `--allow-other-sqlite` is
  passed, because SQLite file bytes depend on the version. Regenerate the
  M3 catalogues only in that environment, and never change a byte of
  `tests/fixtures/backups/` (its `SHA256SUMS` must stay
  `c8e3d0bf…a0ae3`). The messages catalogue's `README.md` is generated;
  do not edit it by hand.
- **The import catalogue has two pins.** `tests/fixtures/imports/` is
  written only by `scripts/fixtures/make_synthetic_imports.py`. Its inputs
  (`SHA256SUMS`, hash pinned in `crates/oma-archive/tests/import_fixtures.rs`)
  are frozen. Its model expectations (`content.json`, `import.json`,
  `scenarios/`, `EXPECTED.SHA256SUMS`, whose hash is pinned in the same
  file) may be regenerated when an import slice settles a rule or adds
  counts; say which rule changed and re-pin. The manifest format and the
  model's readings are in that catalogue's generated `README.md`.
- **No `#[tracing::instrument]` without `skip_all`** in `oma-messages` or
  `src-tauri`: it formats every argument with `Debug`.
  `crates/oma-messages/tests/source_rules.rs` enforces this for that crate.
- **`oma-archive`'s `conn.rs` is the only opener of `archive.sqlite`.**
  Nothing else may open, copy, hash or map the database or its `-wal` or
  `-shm` while a connection is open: on Unix, closing any descriptor to the
  file drops SQLite's POSIX locks. `crates/oma-archive/clippy.toml` bans
  `File::open`, `OpenOptions`, `fs::copy` and the other connection openers;
  `tests/source_rules.rs` also bans computed `Path::join` arguments outside
  `layout.rs` and `blobs.rs`. Opening an archive writes nothing; only
  `Archive::begin_write` and `Archive::migrate` write. Test hooks live behind
  the non-default `fault-injection` feature, which only the crate's own
  tests enable (a self dev-dependency).
- **The import merges in Rust, one savepoint per record, in batches.**
  Every insert keyed on source data is an upsert or `DO NOTHING`; never
  `INSERT OR REPLACE`, never `RETURNING` (`tests/source_rules.rs`). A
  record's `UPDATE` sets only the columns that changed, so the guarded
  triggers fire only on real changes. A batch never commits after a
  statement error. Derived columns (`item_kind`, `assoc_*`, `sort_key`,
  a body's `display_text`) are computed in Rust from the stored raw cells;
  the FTS index is written **only** by the `message_body` triggers. Links
  resolve both sides through this session's `import_rowid_map` and are
  never inserted with an unresolved side; the Recently Deleted decisions
  after P3 (`import/rd.rs`) are re-derived from that map, never carried
  over from the preflight. The gate (`import/gate.rs`) runs before the
  first write: nothing may be written before `ImportGate::confirm`
  returns Proceed (Decline is tested to leave the files byte-identical),
  and the preflight it shows carries counts, closed kinds and booleans
  only (device facts are compared inside the crate). Finalize
  (`import/finalize.rs`) completes the session; its terminal update is
  best effort and must never run under the armed interrupt.
- **Only `conn::armed` arms the archive interrupt,** around a single long
  statement in its own transaction (the `chat_stats` recompute, an FTS
  merge step); it is removed before any `COMMIT`. Never wrap a batch, a
  terminal update or a commit in it.
- **`imports/session-<n>.json` is content-free** (`provenance.rs`): no
  device name, GUID, digest, rank, message date, path or label; counts are
  re-serialised from `ImportCounts`. Writing it is best effort and never
  changes an outcome.
- **The golden archive** (`tests/fixtures/archives/golden-v1/`) is made
  only by `cargo run -p oma-archive --example make_golden` and pinned by
  its `SHA256SUMS` and `logical_snapshot.json`; tests work on a copy.
  Regenerate it only until schema v1 freezes at the M4 release tag.
- **`tests/query_plans.rs` pins the plans** of the import's
  index-dependent statements, taken from the crate through
  `hooks::plan_statements`. A new merge read or lookup goes into that list;
  a changed plan is re-pinned deliberately.
- **P6 copies files in four phases** (`import/attachments.rs`, `blobs.rs`;
  spec §9.1): the copy into `.incoming/<n>.partial` while the pass reads,
  then the `blob_intent` commit, the no-replace placement and one flush
  per `<hh>`, then the rows in the batch. Keep that order (write →
  `sync_all` → intent commit → rename → directory flush → row commit);
  the crash tests kill at each step. Every store path goes through
  `blobs::store_dir`, every store file is opened with `open_nofollow`, and
  nothing replaces an existing file. The space guard (`space.rs`) runs
  before every write transaction; tests get a fake probe with 1 TiB free
  from `common::library()`, so no test depends on the runner's disk.
- **The verifier (`verify.rs`) is report-only.** It opens the writer with
  `begin_write(Recovery::Skip)` and writes only `verify_run` and, in a
  full verify, `blob.verify_state`/`verified_at_ns`; never make it sweep,
  reconcile or repair (that is M8). Its reads go through
  `ReadConn::cancellable` (no op budget, stopped by the cancel token);
  the FTS rank-1 check runs on the writer under `conn::armed`. A failed
  schema check stops it before any table read and writes nothing. Its
  counts are numbers, booleans, closed states and `v1.sql` table names
  only. Attachment bytes are read back only through `blobs::open_checked`
  (`open_blob`, the verifier): add no other reader of store files.
- **Crash points are `fault::point(FaultPoint::…)` calls.** They compile
  to nothing without the feature. `tests/crash.rs` re-runs its own binary
  with `OMA_TEST_FAULT=<point>:<n>` and kills the child there; add a point
  to `FaultPoint::ALL` and a kill at it in that test together
  (`every_fault_point_has_a_kill` fails otherwise). Its checks
  after a kill run on a copy of the database files: a read-write
  connection closing on the original would checkpoint and delete the
  `-wal` the resume must see.
- **The shell's import gate** (`src-tauri/src/archives/gate.rs`) is a
  `Mutex` + `Condvar` with a two-minute timeout from an injectable
  `GateClock`. `ShellGate::confirm` opens the gate *before* it publishes
  the preflight and sends `importPreflight`, so an answer that arrives
  first is kept; `Jobs::cancel` and the exit handler wake it, and every
  ending but Proceed declines (nothing written). The `importPreflight`
  event carries only the job id: the preflight is fetched with
  `get_archive_details` (D47). Jobs are reserved by subject (backup,
  archive or both, D73); keep `MAX_CONCURRENT_JOBS` at 1.
- **Archive DTOs** (`src-tauri/src/archives/dto.rs`) have one free-text
  field, `ArchiveSummaryDto.label`; the structural guard there (sharing
  `src-tauri/src/ts_guard.rs` with the Backups guard) fails on any other
  `string` field and on keys such as `guid`, `rank` or `path`. Device
  facts cross as booleans, dates as Unix milliseconds. The shell never
  logs an archive id or a label; `tests/log_hygiene.rs` checks it.
- **Headless smoke testing works**, using `xvfb-run` with
  `WEBKIT_DISABLE_DMABUF_RENDERER=1` and `XDG_CONFIG_HOME` pointed at a
  scratch directory. However, the GTK folder picker cannot list directories
  without D-Bus, so a full folder pick still needs a real desktop.

## Finishing a milestone

The project's working agreement requires all of the following:

1. Explain the design decisions and platform limits.
2. Ship complete files, not snippets or pseudocode.
3. Build and tests pass on CI for Linux and Windows.
4. Give exact commands for Windows 11 and Linux.
5. Update `docs/compatibility.md`, recording **only tested claims**, and the
   README roadmap and known limitations.
6. List the known limitations and name the next milestone.
