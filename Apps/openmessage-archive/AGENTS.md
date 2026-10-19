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
the manual checklist in `docs/compatibility.md`. **Milestone 4 (the archive
database and import) is implemented**: `crates/oma-archive` (schema v1,
the import with its gate, the content-addressed attachment store, FTS5,
the report-only verifier), six archive IPC commands with import and verify
jobs, and the Archive page. Archives are plaintext at rest (encryption is
M8). All slices pass CI on `ubuntu-22.04` and `windows-2022`. Its sign-off
is pending the manual checklist in
`docs/compatibility.md`, and the owner's answers on D69, D70, D71, D79 and
D82 (D51, D59, D60 and D61 are owner-confirmed). **Milestone 5 (the rich
viewer) is implemented:** PR A (slices 5a–5f: the viewer catalogue, the
viewer reads and search in `oma-archive`, media sniffing and checks, the
viewer commands, the `oma-media` protocol, the image decode helper and the
main window built in Rust) is merged into `main` (`eabb759`) after CI
passed on `ubuntu-22.04` and `windows-2022` ([run 39205163013](https://github.com/VibeForge-Systems/From_the_Forge/actions/runs/39205163013),
including the Windows-only tests). PR B (slices 5g–5k: the frontend
foundations, the in-house `VirtualFeed`, the conversation list and thread,
bubbles, attachments, the media viewer, details, search, the date dialog,
the privacy screen, copy actions, page guards, the accessibility pass and
the documents) is in review. The viewer opens from the Archive page and
the roadmap's `viewer`, `search` and `privacy-screen` are available. It
was tested in jsdom and once in a real window, a headless smoke test of the
release binary on WebKitGTK 2.52.6 in a Linux container
(`docs/compatibility.md`); its sign-off is pending PR B's CI and the M5
manual checklist there (no desktop, audio, screen-reader or WebView2 run
yet, so no engine floor is established).
The owner answered the M5 questions (`docs/architecture.md`, "Milestone 5
decisions").

Read these before designing anything:

| Doc | Read it for |
|---|---|
| [`docs/architecture.md`](docs/architecture.md) | The five invariants, the crates, the IPC boundary, the extraction check, the archive (layout, threat model, import data flow, one section per M4 and M5 slice), the viewer, and the decision records (D1–D122; D43 is how "do not fabricate" is read for the check) |
| [`docs/supported-formats.md`](docs/supported-formats.md) | Sourced research on the backup format, encryption, the sms.db schema (columns with sources, timestamps, codes, typedstream) and device tooling. Items marked **UNVERIFIED** are hypotheses, not facts. |
| [`docs/privacy.md`](docs/privacy.md) | What is copied, where and for how long, including the plaintext copies of sms.db, and what an archive holds (plaintext at rest) |
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
   - From M5, `tauri.linux.conf.json` and `tauri.windows.conf.json` each
     add exactly one scheme source (`oma-media:` on Linux,
     `http://oma-media.localhost` on Windows) to `img-src` and
     `media-src`, and nothing else. The `window` tests compare the merged
     maps exactly; change them only with the spec.
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
  IPC commands and state. `oma-fs`, `oma-backup` (M2), `oma-messages`
  (M3) and `oma-archive` (M4) are implemented (`oma-archive`'s skeleton:
  create, open, lock, schema v1, fingerprint, migrations; and the import
  core: sessions, the Recently Deleted and duplicate-key scans, the merge
  engine, the handle, chat and message passes with body versions and the
  FTS index, the link passes, the attachments with their files in the
  content-addressed store, the Recently Deleted markers, the space
  estimate and guards, the gate, finalize and the provenance files,
  `Archive::import`; and the report-only verifier `Archive::verify` with
  `open_blob`), driven by the shell's six archive commands and its
  import and verify jobs (slice 4l), and the Archive page (slice 4m).
  `oma-export` (M6) has its record core from slice 6d, its files core
  (names, staging, the manifest, the writer trait) from 6e1, and the
  attachment copy, the TXT writer and the pipeline from 6e2, the JSON
  and CSV writers from 6f, and the HTML writer from 6g; `oma-device` (M7)
  is planned. Each must build and test without
  Tauri. The shell's `src-tauri/src/backups/` (registry, job table, worker,
  DTOs, messages), `src-tauri/src/archives/` (listing, details, the
  gate, the import and verify workers, DTOs, sentences) and
  `src-tauri/src/exports/` (drafts, destination checks, the export job,
  DTOs, sentences; slice 6h1) have no Tauri types either, so they are
  tested without a window.
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
- **Frontend tests live in `tests/frontend/*.spec.ts`** (the DST tests in
  `tests/frontend/dst/`, run under four `TZ` values, one of which skipped a
  whole local day).
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
- **`v1.sql` is checked by an exact fingerprint, and freezes at the M4
  release tag.** Every open compares the database's schema objects with
  `v1.sql` (plus the migrations up to its version), so any edit to
  `v1.sql` makes every existing archive `Damaged(SchemaMismatch)`. Until
  the tag, regenerate the golden archive with such an edit; after it, a
  schema change is a forward-only migration run only by an explicit
  `Archive::migrate()` (D87), never on open. `archive.lock` is created once
  and never deleted, not even by the sweep.
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
- **Viewer reads run in phases (`ReadConn::viewing`, M5 slice 5b1).**
  Each phase has its own op budget, an optional cancel token and a
  deadline; overruns are `QueryTooBroad`, cancels `Cancelled`, never
  `Damaged`. Wrap a call's phases in `ReadConn::snapshot` (one read
  transaction per call), call `Phase::checkpoint` between short
  statements, and bound every "recent", "newest" or "from a date" read
  with `sort_key < X'01'` (`sortkey.rs` is the one encoder). Content
  leaves the crate only as `OptContent`/`ContentText` (`Redacted`), with
  one-time codes masked unless the caller reveals them. Every viewer
  statement is listed in `view::plan_statements` and pinned in
  `tests/query_plans.rs`. Only `conn.rs` may call `interrupt()`.
- **Thread pages are keysets bounded to one range (M5 slice 5b2).** Page
  with row-value bounds on `chat_message_timeline`, never `OFFSET`; keep
  the dated (`< X'01'`) and undated (`>= X'01'`) ranges apart; anchor on
  message ids and hand sort keys out only as opaque `SortKeyHint`s.
  Everything a page or the reply thread shows is built by
  `view/assemble.rs` under `Caps`, masks one-time codes and earlier text
  unless the request's `Reveal` asks, and never reads read, delivery or
  sent state (D94).
- **Details decode edit text at read time and withhold it (M5 slice
  5b3).** `message_detail` decodes `ec[*].t` through
  `oma_messages::edit_texts` from the stored `summary_raw` (owner Q3a):
  never store, index or log it. Earlier versions, decoded edit text and
  `message_text` chunks of earlier bodies are withheld like the thread
  pages' earlier text, and one-time codes are masked, unless the
  request's `Reveal` asks (D115, Q3b). A new viewer statement goes into
  `view::plan_statements` and `tests/query_plans.rs`; SQL that excludes
  folded reactions with `NOT (…)` must `coalesce` a NULL `assoc_kind`.
- **Search never passes user text to `MATCH` (M5 slice 5c).** Build
  FTS5 expressions only with `search/query.rs` (quoted terms, ` AND `, a
  prefix `*` only on a last unquoted token of 3+ characters), bound as a
  parameter. Exact text matches in Rust over keyset batches that bound
  rows examined; keep texts streamed, never a batch in memory.
  `highlight()` output is used only after the marker and reconstruction
  checks, and one-time codes are masked in every snippet. A cancel must
  reach the statement through `SearchInterrupt` as well as the token:
  the op budget cannot see work inside one FTS5 call. A new search
  statement goes into `search::plan_statements` and `tests/query_plans.rs`.
- **Media in the crate (M5 slice 5d).** The stored type decides nothing:
  `media/sniff.rs` decides from the first 4 KiB, the only unverified read
  (`inspect_blob`). Streaming needs the `BlobIdentity` that `verify_blob`
  returns (`check_container`, `open_range`; a changed file is `Changed`).
  Images are decoded only by `media::render::render`, a pure function on
  the buffer `read_blob_verified` read to the end, and only inside the
  decode helper process (slice 5f): never call it in the main process.
  `media/` opens no file and only `media/render.rs` names `image` or
  `heic_rs` (`tests/source_rules.rs`). The `heic` feature (owner Q2) is
  off by default and must never be enabled by `src-tauri` or a release
  build; CI only lints and audits it. The structural check and the hash
  are not safety guarantees; never word them as such.
- **The shell viewer (M5 slice 5e).** `src-tauri/src/viewer/` is
  Tauri-free; `commands/viewer.rs` only parses, picks the session by
  generation and runs on the blocking pool. Content crosses only as
  `UntrustedText`, raw integers as `DecimalString`, ids as `SafeId` (input
  ids too, `#[ts(as = "SafeId")]`); a new viewer DTO goes into
  `M5_VIEWER_TYPES` (the viewer guard fails otherwise) and must keep the
  archive guard's walk disjoint. Commands with free text take the raw
  `Request` and `parse_raw` it (never a serde error that could echo it),
  and their `ipc.ts` wrappers use `callRedacted`. A search cancels the
  previous one's token and interrupt **before** waiting for the search
  reader. `inspect_attachment` serves only surfaced attachments and
  refuses playback in Rust when `mediaPlayback` is `off`. A viewer session
  blocks location changes (D114). Never log query or filter text, ids or
  content (`src-tauri/tests/log_hygiene.rs`).
- **The media protocol and the window (M5 slice 5f).** `oma-media`
  is a custom scheme, and **custom schemes bypass the capability ACL**: the
  handler (`src-tauri/src/viewer/media/`, Tauri-free; `lib.rs` only adapts
  the request and responder) authorises every request itself and refuses
  anything not exactly `GET`/`HEAD /v1/<token>/<rendition>` from `main`
  with `404` and an empty body. It must never block (WebKitGTK calls it on
  the GTK main loop): only parse, authorise, answer cache hits and
  refusals, and enqueue; every `Reply` is answered once (a dropped one
  answers `503`). Tokens are per session and minted only for surfaced
  attachments; `stream` needs both the hash and the structural check in
  this session. Every response carries nosniff, `no-store` and the
  `sandbox` CSP; never add `Content-Disposition`. Log outcome kinds,
  statuses, size buckets and elapsed time only: never a token, URI, range
  or exact size. **Images are decoded only in the decode helper** (`main`
  runs `viewer::media::helper::run_helper` for `--oma-decode-helper`
  before anything else); never call `media::render` in the app process.
  Keep `protocol-asset` off and Tauri's `tracing` feature off. The main
  window is built in Rust (`window.rs`, `"create": false`, `incognito`):
  navigation to the app's origin only, new windows, downloads and
  permission requests denied; keep those hooks when touching `setup`.
- **The viewer frontend (M5 slice 5g).** `components/viewer/SafeText.vue`
  is the only renderer of archive content (`ContentText`, `OptContent`,
  text pieces), by interpolation inside a `<bdi>`; never bind content into
  an attribute (accessible names use `aria-labelledby`, images `alt=""`),
  and take a `src` only from `services/media.ts`'s `mediaUrl`.
  `tests/frontend/source-rules.spec.ts` enforces this over all of `src/`:
  a new `:class`, `:id`, `:title`, `:aria-*`… binding must be added to its
  per-file allowlist of non-content expressions, deliberately. It reads
  bindings from the template AST (`vue/compiler-sfc`), so every form Vue
  compiles into a binding counts (`.x`, modifiers, `:alt` shorthand). Viewer words
  live in `services/viewerText.ts` (exhaustive tables; never a read,
  delivery or sent word, D94). The console is reached only through
  `services/devlog.ts` (`devNote(kind, number?)`, dev builds only). Local
  day boundaries come from an injectable `DayBoundaryBuilder`
  (`services/time.ts`); its DST tests in `tests/frontend/dst/` run once
  per `TZ` as separate Vitest projects (`vite.config.ts`), and each run
  first checks that its zone took effect. `stores/viewer.ts` keeps at most
  1,500 items per chat and windows for three chats, and persists nothing.
- **The list and the thread (M5 slices 5h0, 5h1).** `virtua` failed spike
  5s(1) (prepends at scrollTop 0 on WebKitGTK), so `VirtualFeed` is our own
  (`components/viewer/VirtualFeed.vue` over `services/feedModel.ts`). It
  keeps an anchor (a row and its offset) and rewrites `scrollTop` from it
  after every patch and every `ResizeObserver` height, so prepends,
  evictions and re-measured rows never move what is on screen. Keep
  `overflow-anchor: none` on `.oma-feed` (Chromium would compensate a
  second time), keep rows `display: flow-root` (margins inside the measured
  box), pass a **new** `items` array on every change (the watch is
  shallow), and put the focused article, the active option and any playing
  medium in `keepMounted`. `ConversationList` is a listbox with
  `aria-activedescendant` (option ids from the snapshot position, never
  content); `ThreadView` is the APG feed (`aria-setsize="-1"`, no
  `aria-posinset`) and blocks edge loads until its open is aligned and
  after a failed page (Retry). Leaving the `viewer`/`viewer-chat` routes
  closes the session (`closeViewerOnLeave`, D114); chat ids from the URL
  go through `services/ids.ts`, and message ids never enter a URL.
- **Bubbles and system rows (M5 slice 5h2).** `ThreadItem` picks a
  `MessageBubble` or a centred `SystemRow` by `services/messageView.ts`'s
  `itemShape`; that module holds the pure rules (edit and unsend sentences
  per part, chips, sender runs, service changes) and is where new reading
  rules go, with a test in `message-view.spec.ts`. "Edited" and "Unsent"
  mark only the parts the record lists, a whole-bubble "Unsent" needs
  `unsentWhole`, codes 6 and 7 never say unsent, and codes 1 and 8 show
  nothing on the bubble. Events show the addresses recorded with them but
  never the row's sender (no roles). Reveals never change a page: "Show
  code" and "Show earlier text" call `useViewer().revealMessage`, which
  re-reads that one message with `get_message_detail` and keeps an overlay
  (`shown(m)`, at most 100); "Hide" drops it. A reveal or full-text answer
  is kept only if no newer reveal, Hide or eviction of that message came
  while it ran (`revealEpoch`), and a full text is shown only under the
  code reveal it was read with (`fullTextFor`; never read `fullTexts`
  directly in a component). Put explicit `{{ ' ' }}`
  separators between inline elements whose text forms an accessible name,
  and wherever a space sits at the start or end of an element or a
  `<template>` (`<span> {{ x }}</span>`, `{{ x }} </span>`): Vue's
  whitespace condensing drops a whitespace-only text node there. Assert
  the space with `element.textContent` in a test.
- **Attachments, details and media (M5 slice 5h3).** Card rules live in
  `services/attachmentView.ts` (tests in `attachment-view.spec.ts`): Rust's
  card state first, then the sniffed content, never the declared type.
  Cards inspect when they come into view through `useViewer().inspect`,
  which runs at most four at once; take a `src` only from `mediaUrl` with
  the inspection's token, and only while the card is in view. Play goes
  through `requestPlay` (Rust's check, then `PlayConfirmDialog` under
  `confirm`, D118) and plays only in `MediaViewer`; never create an
  `<audio>`/`<video>` elsewhere, never autoplay, and keep `controlslist`,
  `disablepictureinpicture` and `disableremoteplayback`. Under `off` no
  element exists. `MediaViewer` and `PlayConfirmDialog` are mounted once in
  `ViewerView`; the right panel (`viewer.panel`) shows `MessageDetails` or
  `ReplyThread`, and closes when another conversation opens (its
  addresses come from the open chat's window; search stays open). A
  `ThreadItem` outside the feed needs its own `idPrefix`, so ids stay
  unique. Details repeat only the reveal the user asked for.
- **Search and dates (M5 slice 5i).** The search form lives in
  `stores/viewer.ts` (`searchForm`) until the viewer closes; never persist
  it or put the query, a filter or a message id in a URL or a sentence.
  Every text field in the search panel and the date dialog keeps
  `spellcheck="false"`, `autocomplete="off"` and `autocapitalize="off"`.
  Local dates become UTC instants only through `services/calendar.ts` and
  the injected `DayBoundaryBuilder` (D104); add a case to
  `tests/frontend/dst/` when that arithmetic changes. Snippets render only
  through `SafeText`'s `hits` mode (one isolate, `<mark>` per match).
  Cancel names a running scan by the `searchId` its progress carries
  (D119). Shortcuts that act on a focused message are declared with
  `scope: 'message'` and matched only by `ThreadView` on the article;
  the global handler reaches the thread through the store
  (`dateJumpOpen`, `threadCommand`), never through the DOM.
- **Privacy and the page guards (M5 slice 5j).** The privacy screen
  (`stores/privacy.ts`, `components/PrivacyCurtain.vue`) **unmounts** the
  viewer (`v-if` in `ViewerView`), never covers it with a blur: it stops
  every medium, leaves fullscreen, closes the viewer's dialogs through
  `useViewer().closeDialogs()` (a new viewer dialog must be closed there
  too), calls `hide_viewer` (D120) and opens the curtain last. Never word
  it as a lock. Idle hide closes the session; "Show" reopens at the place
  the thread reports through `setPlaceProvider`. Copy only on an explicit
  menu choice, through `services/copyText.ts` (masked codes stay masked,
  withheld text is never copied, "Copy link" refuses hidden characters
  and shows the exact string first) and `copyText`; toasts never carry
  what was copied. `services/guards.ts` stops print, save, reload, the
  engine's context menu and drag-out while the viewer is mounted; keep
  media and bubbles `draggable="false"`.
- **Every cap is a `Caps` field (M6 slice 6a).** The assembly layer and
  the details take their caps from `view::assemble::Caps`; never add a
  hard-coded cap there. `Caps::PAGE` is pinned byte for byte by the digests
  in `crates/oma-archive/tests/export_caps.rs` (re-pin only for a
  deliberate viewer change), and `Caps::EXPORT` lifts every cap.
  `AssembledMessage::cap_miss` must run in release builds (never a
  `debug_assert`). Raw codes and states for exports come from
  `base_coded()` as `RawCodes` (`view/raw.rs`); the view types keep
  collapsing them for the viewer.
- **Export reads (M6 slice 6b1).** An export reads only through
  `crates/oma-archive/src/export/`: `Library::hold_for_export` (the
  archive lock for the job) and `ArchiveReader::with_export_snapshot` (one
  read transaction in `ReadConn::exporting`, cancel only). Inside it every
  other mode of that reader refuses with `Internal`; never add a
  `budgeted`, `viewing` or `cancellable` call to code an export snapshot
  runs (`open_blob` there reads the row through the snapshot). Cursors
  read a batch to the end and return their statement before anything else
  runs (`hooks::statements_stepping` checks it), keep sort keys inside, and
  bound dated reads with `sort_key < X'01'`. The assembly layer and the
  details take a `&StmtCache` (`src/stmt.rs`): its `prepare` takes only a
  `&'static str` and shadows `Connection::prepare`, so a constant
  statement is reused; prepare dynamic SQL on the connection. A new export
  statement goes into `export::plan_statements` and `tests/query_plans.rs`.
- **Export previews, search scopes and masking (M6 slice 6b2).** The
  preview, the job's pre-count and the scope index share one walk
  (`export/scope.rs`), so their numbers agree by construction; count a new
  scope fact there, never in a second loop. The two reveal counts may
  overstate, never understate (D147). An export's search re-run uses the
  viewer's search work through `words::collect_in` and `scan::run_in` in
  the snapshot's phase (never a `viewing` phase), and `search_scope` keeps
  a hit only when the request matches the **masked** text the export
  writes (matching does cover code text). Masking for exports
  (`export/mask.rs`, owner Q4) is applied in `ExportSnapshot::assemble`
  and released only by the export's own `Reveal`: never read the viewer's
  `revealEarlierText` for an export. A withheld attachment loses its
  `blob` in the item and in `attachment_facts`, which takes the
  `ExportMessage`; do not add a path that hands a writer a blob id
  without its access. `tests/export_memory.rs` measures the child's
  `VmHWM` (Linux only); keep its versions above the bound's size.
- **Export fixtures and oracles (M6 slice 6c).** `tests/fixtures/export/`
  is written only by `scripts/fixtures/make_synthetic_exports.py` (SQLite
  3.45.1 only), except `expected/`, `expected-data/` and `expected-html/`
  (the writers' goldens, slices 6e2, 6f and 6g) and `zones/` (the zone
  oracle's tables), and
  has two pins in `tests/export_fixtures.rs`: the inputs
  (backups and `hidden-vectors.json`) are frozen; `*.expected/` and
  `scenarios/` (`content.json`, `export.json`) may be re-pinned when a
  slice settles a rule. A new export case goes into the generator with an
  `export.json` statement of where it was planted, never into a hand-built
  archive alone; codes an import cannot write are `patches`. The Python
  oracles (`scripts/oracles/`) use the standard library plus the
  hash-pinned `requirements.txt`, always in a virtual environment and with
  `python3 -I`; `project_archive.py` must stay independent of the Rust
  export code (written from `v1.sql` and the spec). A writer slice adds its
  oracle to `run_oracles.py`, which CI runs on Linux only. The bulk
  generator's `BulkSpec::blobs` gives attachments real store files.
- **The export record core (M6 slice 6d, `crates/oma-export`).** The crate
  never names `rusqlite` or Tauri and never logs (`tests/source_rules.rs`);
  a read it needs is a new public `ExportSnapshot` method in
  `oma-archive`, pinned in `tests/query_plans.rs`. Records are dedicated
  structs in `record.rs` (camelCase serde = the JSON format): content is a
  `Content`, raw numbers and ids a `Decimal` (never a bare `String`), and
  absence is tagged (`Included`, `Determinable`), never `null`. Every enum
  a record carries is a `Code` from `codes.rs`; a new code is added with a
  new number and `OMA_REPIN_CODES=1 cargo test -p oma-export --test codes`
  (lines may only be added to `tests/codes.pin`), and with its words in
  `words.rs` (`export-words.json` is regenerated by `cargo test`; an entry
  that equals a viewer string names its `viewerText.ts` key). Match an
  `oma-archive` `#[non_exhaustive]` enum with a `_ => Limit(UnmappedVariant)`
  arm, never a fallback value. A new v1 column needs a `ledger.rs` entry
  whose path the catalogue test finds. Ids are export-local ordinals unless
  the plan opts in (owner Q5's lean, awaiting confirmation): never write an
  archive id or a GUID outside `IdentifierMode::ArchiveAndSource`. Times
  are placed only through `time.rs` (`ZoneTable`, `format_iso`,
  `format_human`); hidden characters are classified only by `hidden.rs`,
  whose tables `scripts/make-export-unicode-tables.mjs` regenerates.
- **Export files (M6 slice 6e1, `oma-export`).** Only `naming.rs` builds
  a path inside or beside a bundle (`.join(` nowhere else) and only
  `stage.rs` creates, renames or removes files (`clippy.toml` and
  `tests/source_rules.rs` ban `OpenOptions`, `File::create`, `fs::write`,
  `fs::copy` and temporary directories). On Linux every staged write is
  relative to the `O_DIRECTORY | O_NOFOLLOW` handles; keep it so, and
  keep the `(dev, ino)` and canonical-parent check before the publish
  rename. The marker stays locked until **after** the publish rename;
  never remove it earlier. Names are refused, never changed
  (`TargetName`); an attachment's name keeps its extension only through
  `naming::PASSIVE` (test-pinned). Provenance is written only by
  `manifest::write_provenance` (README, manifest, sums), and
  `ManifestBody` must never gain a field for a query, a device fact, an
  archive id or label, a GUID or a path. A new crash point goes into
  `FaultPoint::ALL` and `tests/crash.rs` together.
- **Attachments, TXT and the pipeline (M6 slice 6e2, `oma-export`).**
  Attachment bytes come only from `ExportSnapshot::open_blob`
  (`attach.rs`): the first 4 KiB of that stream are sniffed, then the file
  is created under its final name with `Staging::create_verified` and
  closed with `close_verified` and the reader's `verified_digest`; never
  hash a copy again, never `verify_blob` then open, never add another
  reader of store files. A failed copy is `discard`ed (an undeletable one
  fails the export). Mark of the Web is `Staging::write_mark_of_the_web`
  (Windows only; a failure is counted, never fatal; its effect is
  **UNVERIFIED**). TXT (`writer/txt.rs`): item lines at column 0, labels
  at four spaces, recorded text at six, split at every line break; header
  fields through `TxtWriter::header` (marks, and escapes `[` `]`);
  sentences only from `words.rs`; no `.join(` (the source rule), use the
  `Comma` helper. A grammar change goes into `tests/txtgrammar` and
  `scripts/oracles/txt_grammar.py` together, and regenerates the goldens
  (`OMA_REGEN_GOLDENS=1 cargo test -p oma-export --test golden_txt`, then
  re-pin `GOLDEN_SUMS_SHA256`; files over 64 KiB are pinned by digest
  only). `pipeline::run` is the one driver: pre-count and consent first,
  handles before the item that names them, versions after their item,
  ties across batch boundaries, the counts checked against the pre-count
  before provenance; any error removes the staging. The record's file
  `path` is content.
- **JSON and CSV (M6 slice 6f, `oma-export`).** JSON lines and array
  elements go only through `jsonfmt::to_line` (separators, DEL and C1
  escaped); an item's `context` stays its last field, so a shared
  message's intrinsic part is byte-identical across files. The schemas in
  `crates/oma-export/schema/` are generated by
  `scripts/make-export-schemas.py` from the record shapes and
  `tests/codes.pin`; never edit them by hand, and a change needs a
  `schemaVersion` bump and a new `STRICT_SCHEMA_SHA256` in
  `tests/json.rs`. CSV uses `csv` only through
  `WriterBuilder::from_writer` over a `StagedFile` (`from_path` is
  banned). Every value goes through `Row::set` (escaped); a text holding
  our placeholders is built with `Row::esc` on its recorded fragments and
  set with `Row::set_rendered`, so placeholders are never escaped and a
  recorded `⟨` always is. Neutralisation is default-deny over the final
  cell (`csv::triggers`); only a column whose `Kind` grammar the value
  matches is exempt. Columns are only appended: re-pin
  `tests/csv_columns.pin` (`OMA_REPIN_CSV_COLUMNS=1`) and bump
  `TABULAR_COLUMNS_VERSION`'s MINOR. A JSON or CSV rule change
  regenerates `tests/fixtures/export/expected-data/`
  (`OMA_REGEN_GOLDENS=1 … --test golden_data`, then re-pin) and goes into
  `scripts/oracles/data_oracle.py` too. The zone oracle's tables
  (`tests/fixtures/export/zones/`) come only from
  `scripts/make-export-zone-tables.mjs`. The mapper repeats a shared
  message's attachment outcome at its later placements; keep it so.
- **HTML (M6 slice 6g, `oma-export`, `writer/html.rs`).** One fixed
  template: `<meta charset>`, then the CSP meta (`default-src 'none';
  img-src data:; style-src 'sha256-…'`), and the one stylesheet `STYLE`,
  whose hash `STYLE_SHA256` a unit test pins (change the style, then
  re-pin it; the contrast test reads its colour properties). Content goes
  **only** into text nodes through `H::content` (escapes `& < > " '`,
  wraps each run in `<bdi dir="auto">`, CR/CRLF as LF) and marks hidden
  characters, controls and noncharacters as `<span class="mark">U+XXXX
  </span>` elements; placeholders and labels are elements too, so a
  literal `⟨` stays literal. Never put content in an attribute, never add
  `<script>`, `style=`, events, forms, frames, `<link>`, `<audio>` or
  `<video>`, and never link or embed an original file; URLs stay text.
  Item links (`#m<n>`) point **backward** on the same page only. Pages
  split at `PageLimits::SPEC` (5,000 items or 20 MiB with `data:` bytes;
  16 MiB of base64 per page); tests lower the limits with
  `HtmlWriter::with_limits`. Images come only from a `preview::PreviewSource`
  (the decode-helper pool in 6h) and pass `preview::check_rendition`
  before they are embedded or written as `html/previews/p<n>.*`; a
  failed check fails the export (`HelperOutputRejected`), and previews
  without a source are refused. Words come from `words::html_text`. A
  grammar change goes into `tests/htmlgrammar`, `scripts/oracles/html_oracle.py`
  and the goldens together (`OMA_REGEN_GOLDENS=1 cargo test -p oma-export
  --test golden_html`, then re-pin `GOLDEN_SUMS_SHA256`). The tag and
  time helpers shared with TXT are in `writer/shared.rs`; a change there
  must leave the TXT goldens unchanged unless a TXT rule changes too.
- **The export job and commands (M6 slice 6h1, `src-tauri/src/exports/`,
  `commands/exports.rs`).** Five commands, each through the five steps
  (`exports::tests` checks the wiring) and each parsing its raw request
  with `parse_raw` (a rejection never echoes input; `ipc.ts` wrappers use
  `callRedacted`). `preview_export` makes the one draft (a new one
  replaces it and cancels its count) and counts on the blocking pool;
  `choose_export_destination` opens the native dialog in Rust, whose title
  names every sensitive option, and checks the folder in its canonical
  form (`exports::destination`; tests use a fake `DestinationProbe`);
  `start_export` consumes the draft, checks the confirmations
  (`worker::prepare`), the generation and the folder again
  (`worker::recheck`), reserves `JobKind::Export` on the archive's
  subject, takes `Library::hold_for_export` and runs
  `worker::run_export`, which re-checks the generation inside the
  snapshot, drops the search query after the re-run, calls
  `pipeline::run_detailed`, releases the lock **before** freeing the slot
  and sends one terminal event (`exportFinished` with counts only,
  `cancelled` or `failed`, each with `stagingRemoved`). `close_viewer` and
  `hide_viewer` go through `AppState::{close_viewer, hide_viewer}`, which
  discard the draft first; keep it so. What PR A does not build (PDF,
  XLSX, previews, the attachment scope, events off) is refused at the
  draft with `exportUnavailable`, never faked. Export DTOs live in
  `exports/dto.rs`: a new one goes into `ts_guard::M6_EXPORT_TYPES` (or
  `M6_EVENT_TYPES` if `JobEvent` reaches it, which the archive guard then
  reviews); the walks stay disjoint but for `M6_SHARED_TYPES`, and the
  only `string`s are `archiveId`, `jobId` and the brands (`DraftId`,
  `LocalDate`, `ZoneName`, `UntrustedText` for `displayName` only). The
  path never crosses IPC and is never logged.
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
  only. Attachment bytes are read back only through `blobs.rs`:
  `open_checked` (`open_blob`, the verifier, `verify_blob`,
  `read_blob_verified`) and `open_range` (plus the sniff window), both
  there: add no other reader of store files (`tests/source_rules.rs`).
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
- **Windows lessons from M2–M4 CI.** Directory handles need
  `FILE_FLAG_BACKUP_SEMANTICS` (`custom_flags(0x0200_0000)`); SQLite files
  under long paths are opened through the `win32-longpath` VFS with a
  verbatim path (`oma-backup`'s `manifest_db.rs` `open_connection`, and
  `oma-archive`'s `conn.rs`); tests must not assume `/` separators;
  `CreateFileW` ignores `FILE_ATTRIBUTE_NOT_CONTENT_INDEXED`; fixture file
  names must be unique ignoring ASCII case; `unsafe_code` is forbidden
  workspace-wide, so anything that needs a raw Win32 call (FAT detection,
  ACLs) is not done. The `RECORD` lines of the `windows.rs` tests are
  captured by the harness and do not reach the CI log; rerun with
  `--nocapture` to read them.
- **The space reserve can refuse a smoke test.** Imports keep 5 % of the
  volume free, clamped to 1–10 GiB, and refuse before the gate below that.
  On a nearly full development disk, put the smoke test's library on a
  tmpfs such as `/dev/shm` (classified as local).
- **Smoke-test the viewer on a prepared library.** Make one with
  `OMA_SMOKE_LIB=<empty dir> cargo test -p oma-archive --test
  smoke_library -- --ignored` (three viewer catalogue entries, one
  archive) and point `archiveDirectory` at it; `docs/development.md` has
  the steps. The slice 5k run found layout and wording defects that jsdom
  could not show (dropped spaces, a page that scrolled as a whole, cut-off
  selects), so look at screenshots after any change to the viewer's
  layout. `perf_viewer.rs` (ignored) measures the §11 reads on bulk
  archives; never assert a timing.
- **The three-panel grid has one `minmax(0, 1fr)` row.** A panel's view
  must be a flex column with `min-height: 0` whose `.oma-panel-body`
  scrolls (`.oma-details`, `.oma-replies`, `.oma-search`); otherwise a tall
  panel grows the row and the whole page scrolls under the navigation bar.
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
