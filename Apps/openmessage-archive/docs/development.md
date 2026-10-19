# Development

## Prerequisites

| Tool | Version | Notes |
|---|---|---|
| Rust | 1.97.0, pinned in `rust-toolchain.toml` | Install [rustup](https://rustup.rs). The pinned toolchain installs automatically on first build. |
| Node.js | 22.12 or later | npm 10 ships with it |
| Platform libraries | See below | |
| Python 3 (fixtures only) | 3.13 recommended | Needed only to regenerate or cross-check the synthetic fixtures; building and testing the app need no Python. See [Synthetic backup fixtures](#synthetic-backup-fixtures). |

### Linux

Ubuntu 22.04+ / Debian 12+:

```sh
sudo apt-get update
sudo apt-get install -y build-essential curl file pkg-config \
  libwebkit2gtk-4.1-dev libxdo-dev libssl-dev \
  libayatana-appindicator3-dev librsvg2-dev
```

Check that the install worked before building. The command should print two
version numbers:

```sh
pkg-config --modversion gtk+-3.0 webkit2gtk-4.1
```

If a Rust build fails in `glib-sys`, `gobject-sys`, `gio-sys` or `gdk-sys` with
"The pkg-config command could not be found", these packages are not installed.

Fedora 40+:

```sh
sudo dnf install -y pkgconf-pkg-config webkit2gtk4.1-devel openssl-devel libxdo-devel \
  libappindicator-gtk3-devel librsvg2-devel
sudo dnf group install -y "c-development"
```

Arch: `sudo pacman -S --needed pkgconf webkit2gtk-4.1 base-devel openssl xdotool libappindicator-gtk3 librsvg`

### Windows 11

1. Install the [Visual Studio 2022 Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
   with the **Desktop development with C++** workload. It provides MSVC
   and the Windows SDK. From milestone 2 it also compiles the bundled
   SQLite (rusqlite's `bundled` feature); nothing else is needed for that.
2. WebView2 is preinstalled on Windows 11.
3. Install Rust with rustup (`rustup-init.exe`) and accept the default MSVC
   toolchain.
4. Install Node.js 22 LTS.

The commands below work the same in PowerShell, `cmd`, and bash.

## Everyday commands

Run them from `Apps/openmessage-archive/`.

| Task | Command |
|---|---|
| Install JS dependencies | `npm ci` |
| Run the app with hot reload | `npm run tauri dev` |
| Release build and installers | `npm run tauri build` |
| Release binary only (no installer) | `npm run tauri build -- --no-bundle` |
| Frontend unit tests | `npm test` |
| Typecheck | `npm run typecheck` |
| Rust tests (also regenerates IPC types) | `cargo test --workspace` |
| Backup library tests only | `cargo test -p oma-backup` |
| Messages library tests only (M3) | `cargo test -p oma-messages` |
| Shell tests only (job worker, DTOs, settings) | `cargo test -p openmessage-archive` |
| Rust lint | `cargo clippy --workspace --all-targets -- -D warnings` |
| Rust format | `cargo fmt --all` |
| JS license audit | `npm run licenses` |
| Rust license and advisory audit | `cargo deny check` (`cargo install cargo-deny@0.20.2 --locked` first) |
| Regenerate app icons from `scripts/icon.svg` | `npm run icons` |

Installers are written to `target/release/bundle/`. On Linux these are
`deb/`, `rpm/` and `appimage/`. On Windows they are `nsis/` and `msi/`.
Installers are unsigned until Milestone 8.

**On a VM shared folder** (vboxsf, 9p, virtiofs), keep the build output on
local disk. Cargo writes several gigabytes, and some shared-folder drivers
mishandle its file locking. Set the target directory before building:

```sh
export CARGO_TARGET_DIR="$HOME/.cache/oma-target"
```

Installers then land in `$CARGO_TARGET_DIR/release/bundle/` instead.

**Build through the Tauri CLI, not plain `cargo build --release`.** The CLI
enables the `custom-protocol` feature that embeds the frontend. A binary
built with plain cargo tries to load the dev server and shows "Could not
connect to localhost".

Diagnostic logging goes to stderr: `OMA_LOG=debug npm run tauri dev`
(PowerShell: `$env:OMA_LOG="debug"; npm run tauri dev`).

**The decode helper (M5).** The viewer decodes images in helper processes:
the app's own binary started with the single argument
`--oma-decode-helper`, which reads length-prefixed requests on stdin and
writes renditions on stdout (`src-tauri/src/viewer/media/helper.rs`). It is
not meant to be run by hand; it exits on any malformed input. Its tests
start the binary Cargo builds:

```sh
cargo test -p openmessage-archive --test decode_helper
cargo test -p openmessage-archive --lib viewer::media   # the protocol, with a fake decoder
cargo test -p openmessage-archive --lib window          # merged CSP files, window policies
```

## IPC types

IPC request and response types are defined once, in Rust, with
`#[derive(TS)] #[ts(export)]`. `cargo test` writes them to
`src/types/generated/`. Commit the regenerated files with the Rust change;
CI fails if they are stale. `src/types/index.ts` re-exports them.

### Adding a command

1. Write the handler in `src-tauri/src/commands/`.
2. Register it in `generate_handler!` in `src-tauri/src/lib.rs`.
3. Add its name to `COMMANDS` in `src-tauri/build.rs`.
4. Grant `allow-<command-name>` in `src-tauri/capabilities/default.json`.
5. Add a typed wrapper to `src/services/ipc.ts`.

If you skip step 3 or 4, the command is denied at runtime. The
frontend then reports an `internal` error naming the command.

## Tests

- **Rust:** unit tests sit beside the code (`#[cfg(test)]`). Filesystem
  tests use `tempfile` and never touch real user data.
- **Frontend:** `tests/frontend/*.spec.ts` (Vitest + jsdom). IPC is faked.
  For the Tauri `invoke` function, use a hand-written fake, not `vi.fn()`:
  Vitest 4 reports a plain-object rejection returned from a `vi.fn()` as
  unhandled even when the caller handles it.
- **Vitest projects (M5 slice 5g).** `vite.config.ts` defines a `frontend`
  project (every spec except `tests/frontend/dst/`) and one project per
  time zone for the DST tests of the day-boundary builder:
  `dst Europe/London`, `dst America/Sao_Paulo`, `dst Australia/Lord_Howe`
  and `dst Pacific/Apia` (which skipped the whole local day 2011-12-30),
  each with `TZ` set in its workers' environment. `npm test` runs all five.
  Run one with `npx vitest run --project 'dst Europe/London'`. Each DST run
  first asserts that its zone took effect (the zone's UTC offsets differ
  between two dates), so a runner that ignores `TZ` fails rather than
  passing on UTC; whether Node honours `TZ` on the Windows runner is
  checked by that assertion in CI.
- **The source rule** (`tests/frontend/source-rules.spec.ts`) reads every
  file under `src/` and fails on HTML sinks, string evaluation, dynamic
  bindings and components, render functions, DOM writes outside
  `SafeText.vue`, navigation, object URLs, casts to the content brands,
  `console.` outside `services/devlog.ts`, and any `:href`, `:src`,
  `:class`, `:id`, `:title`, `:alt`, `:aria-*`, `:data-*` (and similar)
  binding that is not on its per-file allowlist (a `:src` must be exactly
  one `mediaUrl(…)` call). Bindings are read from each template's AST
  (`vue/compiler-sfc`, the app's own Vue), so `:x`, `v-bind:x`, the `.x`
  prop shorthand, modifiers, either quote style, any case and the
  same-name shorthand (`:alt`) are all seen. Casts to the brands include
  the angle-bracket form (`<SafeId>raw`), and render functions include
  `createVNode` and its relatives. Comments may name the banned forms;
  whole-line `//`, block and HTML comments are ignored.
- **Fixtures** go in `tests/fixtures/`. They must be synthetic.
  **Never commit a real backup, real messages, or real contact data**, even
  "anonymized" by hand. `.gitignore` blocks `*.db` and `*.sqlite` outside
  `tests/fixtures/`. `.gitattributes` marks `tests/fixtures/**` as binary,
  so Git for Windows never converts line endings in them.
- **The Windows-only tests** (`crates/oma-backup/tests/windows.rs`: junctions
  made with `cmd /C mklink /J`, a library path of about 200 characters) run
  in `cargo test` on Windows with no extra setup.

### Synthetic backup fixtures

`tests/fixtures/backups/` is generated by
`scripts/fixtures/make_synthetic_backups.py` and checked by
`tests/fixtures/backups/SHA256SUMS` (the `fixtures_integrity` test). Change
fixtures **only** by regenerating them with the script, and commit the new
`SHA256SUMS` with them. Any fixture change invalidates the oracle run
recorded in `docs/compatibility.md` until it is repeated. CI runs no Python.

This needs Python 3 with the `venv` module. The committed fixtures were
generated with Python 3.13.16 and its SQLite 3.45.1 (see
[`tests/fixtures/backups/README.md`](../tests/fixtures/backups/README.md)).
On Debian and Ubuntu, `venv` is a separate package; without it the first
command fails with "ensurepip is not available":

```sh
sudo apt-get install -y python3 python3-venv
```

On Windows, install Python from python.org or with
`winget install --id Python.Python.3.13`; either installs the `py` launcher
used below.

The oracle's own dependencies are pinned in
`scripts/fixtures/requirements-oracle.txt` (iphone_backup_decrypt and
pycryptodome, which performs its AES), so a repeated run uses the recorded
versions.

Linux:

```sh
python3 -m venv .venv-fixtures
.venv-fixtures/bin/pip install -r scripts/fixtures/requirements.txt
.venv-fixtures/bin/python -I scripts/fixtures/make_synthetic_backups.py tests/fixtures/backups --seed 1
.venv-fixtures/bin/pip install -r scripts/fixtures/requirements-oracle.txt   # optional oracle
.venv-fixtures/bin/python -I scripts/fixtures/verify_with_oracle.py tests/fixtures/backups
sha256sum tests/fixtures/backups/SHA256SUMS                         # record with the oracle run
```

Windows (PowerShell):

```powershell
py -3 -m venv .venv-fixtures
.venv-fixtures\Scripts\pip install -r scripts\fixtures\requirements.txt
.venv-fixtures\Scripts\python -I scripts\fixtures\make_synthetic_backups.py tests\fixtures\backups --seed 1
.venv-fixtures\Scripts\pip install -r scripts\fixtures\requirements-oracle.txt
.venv-fixtures\Scripts\python -I scripts\fixtures\verify_with_oracle.py tests\fixtures\backups
Get-FileHash tests\fixtures\backups\SHA256SUMS
```

The output is reproducible for a given `--seed` with the same Python
`sqlite3` build; it is not claimed to be byte-identical across machines.

### Synthetic messages and typedstream fixtures (M3)

Two more catalogues test `crates/oma-messages`. Each is produced only by its
committed generator and checked by its own `SHA256SUMS`, which the crate's
`fixtures_integrity` test pins:

| Catalogue | Generator | Contents |
|---|---|---|
| `tests/fixtures/messages/` | `scripts/fixtures/make_synthetic_messages.py`, with the column profiles in `scripts/fixtures/sms_profiles.py` | 20 backups (plain and encrypted) holding synthetic sms.db files, each with `<name>.expected/` (the plaintext sms.db and the generator's model of the report, `report.json`) |
| `tests/fixtures/typedstream/` | `scripts/fixtures/make_synthetic_typedstream.py` (standard library only; no SQLite is involved, so its output is byte-identical everywhere) | Typedstream vectors, `expected.json` and `README.md` |

`make_synthetic_messages.py` imports `make_synthetic_backups.py` as a module
and leaves its code untouched, so the frozen M2 catalogue in
`tests/fixtures/backups/` (its `SHA256SUMS` hashes to
`c8e3d0bfba700b10653521c49bfcb1fc30dcc1477a762541a17cab17b04a0ae3`) cannot
change by accident.

**Reference environment:** Python 3.13.16 with its `sqlite3` 3.45.1. SQLite
file bytes depend on the SQLite version, so `make_synthetic_messages.py`
prints the version and **refuses to write** under any other SQLite unless
`--allow-other-sqlite` is passed. Windows is not a supported generation
environment. The messages catalogue's `README.md` is written by the
generator and records the environment it was built in; do not edit it by
hand. (It records `cryptography` 50.0.1, the version actually installed when
the catalogue was built; `requirements.txt` pins 50.0.2. AES output does not
depend on the version.)

**Manual gate.** CI runs no Python, so nothing checks automatically that a
generator still reproduces the committed bytes. If you touch
`make_synthetic_backups.py`, regenerate the frozen catalogue into a scratch
directory and confirm its `SHA256SUMS` still hashes to `c8e3d0bf…a0ae3`
before committing:

```sh
python3 -m venv .venv-fixtures
.venv-fixtures/bin/pip install -r scripts/fixtures/requirements.txt
.venv-fixtures/bin/python -I -c "import sqlite3; print(sqlite3.sqlite_version)"   # must print 3.45.1
.venv-fixtures/bin/python -I scripts/fixtures/make_synthetic_backups.py "$(mktemp -d)/b" --seed 1
#   then: sha256sum <that dir>/b/SHA256SUMS   (must be c8e3d0bf…a0ae3)
.venv-fixtures/bin/python -I scripts/fixtures/make_synthetic_messages.py tests/fixtures/messages --seed 1
.venv-fixtures/bin/python -I scripts/fixtures/make_synthetic_typedstream.py tests/fixtures/typedstream
sha256sum tests/fixtures/backups/SHA256SUMS tests/fixtures/messages/SHA256SUMS tests/fixtures/typedstream/SHA256SUMS
```

A change to either M3 catalogue invalidates the oracle runs recorded for it
in `docs/compatibility.md` until they are repeated with the new checksum.

### Synthetic import fixtures (M4)

`tests/fixtures/imports/` is written by
`scripts/fixtures/make_synthetic_imports.py`, in the same reference
environment, and refuses any other SQLite in the same way. It imports the
M2 and M3 generators as modules and changes neither. It writes twelve backups,
one content manifest and one counts file per entry
(`<entry>.expected/content.json` and `import.json`, the model's expectation
for that entry imported alone), ten `scenarios/<name>.content.json`
(sequences of imports) with their per-step `scenarios/<name>.counts.json`,
and two sums files:

| File | Covers | Pinned |
|---|---|---|
| `SHA256SUMS` | Every file of every backup (the inputs) | Its SHA-256 is pinned in `crates/oma-archive/tests/import_fixtures.rs`; the inputs are frozen |
| `EXPECTED.SHA256SUMS` | Every file under `*.expected/` and `scenarios/` | Checked file by file; its own hash is pinned in the same test (since slice 4f) and is re-pinned when an import slice regenerates the expectations |

```sh
.venv-fixtures/bin/python -I scripts/fixtures/make_synthetic_imports.py tests/fixtures/imports --seed 1
sha256sum tests/fixtures/imports/SHA256SUMS tests/fixtures/imports/EXPECTED.SHA256SUMS
cargo test -p oma-archive --test import_fixtures
```

A model change that leaves the inputs alone changes only
`EXPECTED.SHA256SUMS` (re-pin it in `import_fixtures.rs` and say which rule
changed); if `SHA256SUMS` changes, the generator changed an input, and the
pin in `import_fixtures.rs` fails on purpose.

### Synthetic viewer fixtures (M5)

`tests/fixtures/viewer/` (M5 spec §14.1) is written in two steps. The media
files come from a Rust example that uses the `image` crate's encoders and
writes the hostile PNGs and the containers by hand; the backups and the
model's expectations come from `scripts/fixtures/make_synthetic_viewer.py`,
in the reference environment (it refuses any SQLite but 3.45.1 and has no
override). The script imports the M2, M3 and M4 generators as modules and
changes none of them.

```sh
rm -rf /tmp/viewer
cargo run -p oma-archive --example make_viewer_media -- /tmp/viewer/media
.venv-fixtures/bin/python -I scripts/fixtures/make_synthetic_viewer.py /tmp/viewer --seed 1
rm -rf tests/fixtures/viewer && cp -r /tmp/viewer tests/fixtures/viewer
sha256sum tests/fixtures/viewer/SHA256SUMS tests/fixtures/viewer/EXPECTED.SHA256SUMS
cargo test -p oma-archive --test viewer_fixtures
```

| File | Covers | Pinned |
|---|---|---|
| `SHA256SUMS` | `media/` and every file of every backup (the inputs) | Its SHA-256 is pinned in `crates/oma-archive/tests/viewer_fixtures.rs`; the inputs are frozen |
| `EXPECTED.SHA256SUMS` | Every file under `*.expected/` and `scenarios/`: the `content` the import must leave and the `viewer` facts the viewer must derive | Its own hash is pinned in the same test and is re-pinned when a viewer slice settles a rule (say which) |

`viewer_fixtures.rs` also rebuilds every `media/` file with the example's
code (`examples/make_viewer_media/media.rs`) and compares the bytes, so an
`image` upgrade that changes an encoder's output fails there first. On
Windows, run the example the same way from PowerShell (it writes plain
files); the Python step is not run on Windows.

### Synthetic export fixtures and the independent oracles (M6)

`tests/fixtures/export/` (M6 spec §15) is written only by
`scripts/fixtures/make_synthetic_exports.py`, in the reference environment
(it refuses any SQLite but 3.45.1). It imports the viewer generator (and,
through it, the M2–M4 generators) as modules and changes none of them. Each
entry is a plain backup with the viewer catalogue's `content.json`, plus an
`export.json` of the generator's own: the masking sentinels by kind, the
strings that must stay visible, every hostile, formula and file-name case
with where it was planted, each conversation's members, and the archive
patches and store corruption a test applies after the import. The
catalogue's generated `README.md` describes each file.

```sh
.venv-fixtures/bin/python -I scripts/fixtures/make_synthetic_exports.py /tmp/export --seed 1
rm -rf tests/fixtures/export && cp -r /tmp/export tests/fixtures/export
sha256sum tests/fixtures/export/SHA256SUMS tests/fixtures/export/EXPECTED.SHA256SUMS
cargo test -p oma-archive --test export_fixtures --test export_scope
npx vitest run tests/frontend/hidden-vectors.spec.ts
```

| File | Covers | Pinned |
|---|---|---|
| `SHA256SUMS` | Every file of every backup, and `hidden-vectors.json` (the inputs) | Its SHA-256 is pinned in `crates/oma-archive/tests/export_fixtures.rs`; the inputs are frozen |
| `EXPECTED.SHA256SUMS` | Every file under `*.expected/` and `scenarios/` (`content.json`, `export.json`) | Its own hash is pinned in the same test and is re-pinned when a slice settles a rule (say which) |

`export_fixtures.rs` imports every entry and the `history` scenario and
compares the archive with the model, checks that every case is stored where
`export.json` says, applies the patches (codes an import cannot write) and
assembles every patched item, and checks that the corrupted store file
fails its re-hash. `export_scope.rs`'s masking oracle reads every item of
the catalogue under each reveal. The volume cases (over 10,000 reactions
on one target, over 50 versions, 200 × 16 MiB versions) are generated at
test time, not committed.

**The independent oracles** (spec §14.4; `scripts/oracles/`) run in CI's
Linux `checks` job and by hand. The Rust side writes one archive per entry
and scenario of the viewer and export catalogues (an ignored test); the
Python side projects each with the standard library's `sqlite3` on a
read-only copy (`project_archive.py`, written from `v1.sql` and the spec,
not from the Rust export code), validates it with `jsonschema` against
`scripts/oracles/schemas/`, and compares it with the generators' models:
members, display text, dates, attachments, the sender reason, unsent
parts, blob digests (every store file re-hashed) and the completeness
identities. It also projects a copy of `golden-v1`. Install the packages
only into a virtual environment, never system-wide:

```sh
python3 -m venv .venv-oracles
.venv-oracles/bin/pip install --require-hashes --only-binary :all: --no-deps \
  -r scripts/oracles/requirements.txt
OMA_ORACLE_OUT=/tmp/oma-oracle-archives \
  cargo test -p oma-archive --test export_fixtures -- --ignored --exact write_oracle_archives
.venv-oracles/bin/python -I scripts/oracles/run_oracles.py --archives /tmp/oma-oracle-archives
```

`OMA_ORACLE_OUT` must be empty or absent. `--keep DIR` also writes each
projection. The output is counts and check names only. Run here (Python
3.13.16, SQLite 3.45.1): 22 archives and `golden-v1`, 4,817 checks, none
failed. Whether `ubuntu-22.04`'s Python 3.10 and its system SQLite open
schema v1 (it uses `STRICT` tables, SQLite 3.37 or later) is
**UNVERIFIED** until CI runs the step. The writers' oracles (TXT grammar,
JSON schemas, CSV, HTML, and later XLSX and PDF) join `run_oracles.py` in
their slices.

### The export record core (M6 slice 6d)

`crates/oma-export` has three generated or pinned files; regenerate them
only deliberately, and commit the result:

```sh
cargo test -p oma-export                                 # also rewrites src/types/generated/export-words.json
OMA_REPIN_CODES=1 cargo test -p oma-export --test codes  # a new code: lines may only be added to tests/codes.pin
node scripts/make-export-unicode-tables.mjs              # crates/oma-export/src/unicode_tables.rs
```

- `export-words.json` holds the sentence of every code; the Vitest
  `tests/frontend/export-words.spec.ts` compares each entry that names a
  viewer key with `src/services/viewerText.ts`, so run `npm test` after
  changing either side.
- `unicode_tables.rs` is computed from the regular-expression properties
  of the Node that runs the script (Unicode 16.0 with Node 22.22.0 here;
  the version is written into the file). The Rust classifier must keep
  passing `tests/hidden_vectors.rs`, and the copy rule
  `hidden-vectors.spec.ts`, after a regeneration.
- `tests/record_catalogue.rs` imports every entry and scenario of the
  viewer and export catalogues and maps them whole; it is the place for a
  new record rule's test.

### Export staging and crash tests (M6 slice 6e1)

```sh
cargo test -p oma-export --test stage    # staging, publish, removal, the sweep; the swap tests are Linux only
cargo test -p oma-export --test crash    # kills a child export at every crash point
cargo test -p oma-export --test fanout   # the writer trait and fan-out over the view catalogue's records
```

- `tests/crash.rs` re-runs its own binary with `OMA_EXPORT_TEST_FAULT`
  and `OMA_EXPORT_TEST_DEST` set (`crash_child` does nothing without
  them). A new `fault::point` goes into `FaultPoint::ALL` and into one of
  the test's point lists in the same change: `every_fault_point_has_a_kill`
  fails otherwise.
- The tests write only below their own temporary folder and check that
  nothing appears beside the destination. The swap tests rename the
  staging folder (and the parent) mid-export and plant links; they need a
  Linux file system with symbolic links.
- Windows: the staging code was only compiled and linted for
  `x86_64-pc-windows-msvc` with a scratch harness (the crate as a whole
  needs MSVC for the bundled SQLite). Its behaviour there (sharing
  violations, retries, `remove_dir_all` of a leftover) is **UNVERIFIED**
  until the Windows CI job runs these tests.

### Attachments, TXT and the pipeline (M6 slice 6e2)

```sh
cargo test -p oma-export --test txt          # every catalogue entry to TXT through the pipeline, parsed back
cargo test -p oma-export --test attach       # copies, names, exclusions, the reveal, cancel; the swap test is Linux only
cargo test -p oma-export --test golden_txt   # names-only TXT bundles against tests/fixtures/export/expected/
OMA_REGEN_GOLDENS=1 cargo test -p oma-export --test golden_txt   # regenerate them (then re-pin the hash it prints)

# The independent TXT oracle (Linux; in the oracle virtual environment):
OMA_ORACLE_TXT_OUT=/tmp/oma-txt cargo test -p oma-export --test txt -- --ignored --exact write_oracle_txt
python3 -I scripts/oracles/txt_grammar.py --exports /tmp/oma-txt

# Attachment copy throughput (ignored; prints RECORD lines, asserts no timing):
OMA_PERF_ATTACH_FILES=1000 OMA_PERF_ATTACH_BYTES=262144 \
  cargo test -p oma-export --test perf_attach -- --ignored --nocapture
```

- `tests/txtgrammar/mod.rs` is a strict parser of the TXT grammar for the
  Rust tests; `scripts/oracles/txt_grammar.py` is an independent one that
  also compares each file with its archive through stdlib `sqlite3`
  (membership, order, times, text, senders) and checks `SHA256SUMS`.
  Both must change with the grammar. Run here (Python 3.13.16): 21
  exports, 9,777 checks, 0 failed; with the header escaping turned off by
  hand, both it and four Rust tests failed.
- The goldens are regenerated only when a TXT, manifest or README rule
  changes on purpose; name the rule in the commit. Files over 64 KiB are
  pinned by digest in `expected/SHA256SUMS` and not committed.
- `perf_attach.rs` builds a bulk archive with store files through
  `oma-archive`'s generator (its test module is included by path, which
  needs `oma-archive`'s `fault-injection` feature and `oma-messages` as
  `oma-export` dev-dependencies). Run once here, debug build, on the
  container's disk (not a tmpfs): 953 files of 256 KiB (238 MiB) exported
  in 1.76 s (135 MiB/s, TXT included, with one `sync_all` per file),
  against 0.24 s to read and hash the same files once and 0.57 s to
  `fs::copy` them. The spec's 100,000 files / 50 GB target needs about
  100 GB free and was **not run**; the FAT32 manual row (§21) is not run
  either.
- Windows: `write_mark_of_the_web` and the Windows half of the FAT
  boundary rule were only compiled and linted for
  `x86_64-pc-windows-msvc` with the scratch harness; the Mark of the Web
  assertions in `attach.rs` run only on the Windows CI job.

### The bulk generator (M5)

`crates/oma-archive/tests/common/bulk.rs` writes a schema-valid v1 archive
of any size directly (M5 spec §14.2), through the test-only hook
`oma_archive::hooks::bulk_writer` (the `fault-injection` feature, enabled
only by the crate's own tests). It commits import-sized batches, runs the
import's finalize statistics and FTS merge steps, and the result passes a
quick verify (`tests/bulk.rs`). Attachments have no store files by
default; `BulkSpec::blobs` (M6 slice 6c) gives each one its own file of
`BulkSpec::blob_len` bytes, placed through
`oma_archive::hooks::BulkWriter::put_blob` as the import places copies, and
such an archive passes a full verify. It replaces the import-based generator for
performance work; the calibration against an import-built archive is an
ignored test:

```sh
OMA_PERF_MESSAGES=100000 cargo test -p oma-archive --release --test bulk -- --ignored --nocapture
```

### Import core tests (M4)

The import tests run in `cargo test --workspace`; these run them alone:

```sh
cargo test -p oma-archive --test import_core   # catalogue entries and scenarios against the model
cargo test -p oma-archive --test resume        # cancel at every record; random in-process crashes
cargo test -p oma-archive --test crash         # crash by kill at every fault point, then resume
cargo test -p oma-archive --test finalize      # the gate, finalize and the provenance files
cargo test -p oma-archive --test ordering      # the store's write/sync/intent/rename/flush/commit order
cargo test -p oma-archive --test query_plans   # pinned EXPLAIN QUERY PLAN of the import's statements
cargo test -p oma-archive --test golden        # the golden v1 archive opens and reads as written
```

**The golden archive** (`tests/fixtures/archives/golden-v1/`, spec
§19.2) is generated by the build itself, never by hand:

```sh
rm -rf tests/fixtures/archives/golden-v1
cargo run -p oma-archive --example make_golden
cargo test -p oma-archive --test golden
```

It imports `imp-dev1-v1-plain` then `imp-dev1-v2-plain` with a fixed clock
and writes the archive under `library/`, its `SHA256SUMS` and
`logical_snapshot.json`. The archive id and `archive_uuid` are random, so
two runs differ byte for byte; the committed one is the reference. It is
regenerated only until schema v1 freezes at the M4 release tag (after that
it is frozen, and a build that cannot read it is wrong).

`tests/crash.rs` re-runs its own test binary as a child with
`OMA_TEST_FAULT=<point>:<n>` (and the library, archive and backup in
`OMA_TEST_*` variables). The child test `crash_child` does nothing when the
variable is unset. At the n-th hit of the point the child prints
`FAULT <point> <n>` and blocks; the parent kills it, checks the archive and
continues the import. On Windows the plan is a subset (spec §18.12): the
batch commit points (`batch-before-commit` and `batch-after-commit`, at
their first and last hit), the P6 intent-commit and placement points
(`attachment-intent-committed`, `attachment-placed`, first and last hit),
the terminal commit of a cancelled session, the three kills in a row
(including one during recovery) and both creation points. The record,
session and recovery points of the import plan, the other store points,
and the gate, finalize, after-completion and migration kill tests are
skipped on Windows (those four tests return early there);
`every_fault_point_has_a_kill` checks the full union off Windows and only
records the points left out on Windows. A child that neither reaches its
point nor finishes within 120 s fails the test.

### Test time (spec §0.1)

The M4 budget is at most 4 minutes of added Rust test time per Linux job
and 8 minutes on Windows. Measured on 2026-10-18 in a Linux container
(4 CPUs, debug build, after compiling; synthetic fixtures), slice 4f:

| What | Time |
|---|---|
| `cargo test --workspace`, wall clock | 32 s |
| Sum of all test binaries' run times | 30.1 s |
| `oma-archive`, all binaries | 13.3 s |
| of which `crash.rs` (29 child processes) | 4.2 s |
| of which `resume.rs` | 2.5 s |
| of which `import_core.rs` | 1.4 s |

Slice 4g (same container, same method):

| What | Time |
|---|---|
| `cargo test --workspace`, wall clock (including compilation of the changed crates) | 3 min 46 s |
| `oma-archive`, all binaries | 25 s |
| of which `crash.rs` (30 child processes) | 10.4 s |
| of which `resume.rs` | 5.2 s |
| of which `import_core.rs` | 2.5 s |
| of which `messages.rs` | 1.6 s |

Slice 4h (same container; after compiling, as for slice 4f):

| What | Time |
|---|---|
| `cargo test --workspace`, wall clock | 50 s |
| Sum of all test binaries' run times | 47.9 s |
| `oma-archive`, all binaries | 34.8 s |
| of which `crash.rs` (the main kill test runs 56 child processes) | 17.5 s |
| of which `resume.rs` | 5.7 s |
| of which `import_core.rs` | 2.5 s |
| of which `messages.rs` | 1.8 s |
| of which `links.rs` and `lineage.rs` | 1.9 s |

Slice 4i (same container and method):

| What | Time |
|---|---|
| `cargo test --workspace`, wall clock | 57 s |
| Sum of all test binaries' run times | 54.3 s |
| `oma-archive`, all binaries | 38.3 s |
| of which `crash.rs` (the main kill test runs 56 child processes, the store kill test 10) | 18.0 s |
| of which `resume.rs` | 5.6 s |
| of which `import_core.rs` | 4.0 s |
| of which `store.rs` and `space.rs` | 1.3 s |

Slice 4j (same container and method):

| What | Time |
|---|---|
| `cargo test --workspace`, wall clock | 55 s |
| Sum of all test binaries' run times | 53.1 s |
| `oma-archive`, all binaries | 36.7 s |
| of which `crash.rs` (78 child processes in all) | 17.0 s |
| of which `resume.rs` | 5.4 s |
| of which `import_core.rs` | 2.9 s |
| of which `finalize.rs`, `ordering.rs`, `query_plans.rs` and `golden.rs` | 0.8 s |

Slice 4k (same container and method):

| What | Time |
|---|---|
| `cargo test --workspace`, wall clock | 63 s |
| Sum of all test binaries' run times | 59.7 s |
| `oma-archive`, all binaries | 41.1 s |
| of which `crash.rs` (78 child processes in all; unchanged) | 18.5 s |
| of which `resume.rs` | 6.0 s |
| of which `verify.rs` (11 tests, one writes 10,001 files) | 2.1 s |

Slice 4l (same container and method):

| What | Time |
|---|---|
| `cargo test --workspace`, wall clock | 68 s |
| Sum of all test binaries' run times | 65.1 s |
| The shell's unit tests (240, the archive jobs over the import catalogue included) | 0.6 s |
| The shell's `log_hygiene.rs` (now also an import and a full verify of every import catalogue entry) | 2.4 s |

`tests/perf.rs` is ignored and not counted.

Milestone 4 as a whole, slice 4n (same container and method; 879 Rust
tests passed, 7 ignored):

| What | Time |
|---|---|
| `cargo test --workspace`, wall clock | 60 s |
| Sum of all test binaries' run times | 57.2 s |
| `oma-archive`, all binaries (all of it M4 test time) | 38.7 s |
| of which `crash.rs` | 16.9 s |
| of which `resume.rs` | 5.9 s |
| of which `import_core.rs` | 3.1 s |
| `npm test` (206 frontend tests), wall clock | 8 s |

**On CI** (slices 4a–4f only, [run 39137381988](https://github.com/VibeForge-Systems/From_the_Forge/actions/runs/39137381988);
sums of the test binaries' own times, after compiling):

| Job | All Rust tests | `oma-archive` |
|---|---|---|
| `checks` (ubuntu-22.04) | 26.8 s | 12.1 s |
| `build` (ubuntu-22.04) | 28.7 s | 14.3 s |
| `build` (windows-2022) | 232.1 s | 56.3 s |

On Windows the largest binaries were `oma-messages`' `resume.rs` (75.3 s),
`parser_mutation.rs` (42.1 s) and `oma-archive`'s `crash.rs` (20.0 s), so
most of the Windows test time predates M4. Slices 4g–4n have not run on CI;
locally they took `oma-archive` from 13.3 s (slice 4f) to 38.7 s, so the
M4 total stays inside the budget of 4 minutes per Linux job and 8 minutes
on Windows if CI scales as it did for slices 4a–4f (about 1× on Linux and
4× on Windows). That is an extrapolation until a CI run of the whole
milestone is recorded. The Windows crash plan is smaller on purpose
(§18.12). The
catalogue's `README.md` is generated and documents the manifest format and
the readings the model makes.

**Milestone 5 (spec §0.1: at most 3 more minutes of Rust test time per
Linux job, 6 on Windows), measured in slice 5f** after the decode, protocol
and shell tests exist, in the same container (4 CPUs, debug build, after
compiling; 1,220 Rust tests passed, 10 ignored):

| What | Time |
|---|---|
| `cargo test --workspace`, wall clock | 2 min 2 s |
| Sum of all test binaries' run times | 118.6 s |
| Added since M4 (slice 4n: 57.2 s) | about 61 s |
| of which the shell's unit tests (the viewer session, commands and protocol tests import viewer catalogue entries; 0.6 s in M4) | 36.1 s |
| of which `oma-archive`'s viewer binaries (`viewer_conversations`, `viewer_threads`, `viewer_details`, `viewer_search`, `viewer_fixtures`, `viewer_media`, `media_corpus`) | 16.1 s |
| of which the protocol tests alone (`viewer::media`, `window`; 35 tests) | 1.1 s |
| of which `decode_helper.rs` (three helper processes) | 0.3 s |
| the shell's `log_hygiene.rs` (now also the viewer and the protocol through the real helper) | 2.6 s |

The ignored performance tests (`perf_search.rs`, the bulk calibration) are
not counted. So M5 so far adds about 1 minute per Linux job here, inside
its 3-minute budget. **Windows has not been measured:** PR A has not run
on CI yet. With the 4× factor seen for slices 4a–4f, the M5 tests would add
about 4 minutes on `windows-2022`, inside the 6-minute budget; that is an
extrapolation until a CI run is recorded. Slice 5k measures again.

**Milestone 5, measured again in slice 5k** (spec §0.1), in the same
container and the same way (after compiling; 1,240 Rust tests passed, 12
ignored, the two new ignored ones being `perf_viewer.rs` and
`smoke_library.rs`):

| What | Time |
|---|---|
| `cargo test --workspace`, wall clock | 2 min 2 s |
| Sum of all test binaries' run times | 119.0 s |
| Added since M4 (slice 4n: 57.2 s) | about 62 s |
| of which the shell's unit tests | 37.4 s |
| of which `oma-archive`'s viewer binaries (the seven of slice 5f) | 14.7 s |
| `npm test` (570 frontend tests in 46 files, the three DST projects included), wall clock | 19 s |

PR B added two quick Rust tests (slice 5j) and one test case (slice 5i),
so the Rust figure is essentially slice 5f's. **On CI**, PR A's run
([39205163013](https://github.com/VibeForge-Systems/From_the_Forge/actions/runs/39205163013))
against milestone 4's last run
([39163096062](https://github.com/VibeForge-Systems/From_the_Forge/actions/runs/39163096062)),
as the sums of the test binaries' own times read from the job logs:

| Job | M4 (slices 4g–4n) | M5 PR A | Added |
|---|---|---|---|
| `checks` (ubuntu-22.04) | 54.2 s | 106.3 s | 52.1 s |
| `build` (ubuntu-22.04) | 54.2 s | 100.9 s | 46.7 s |
| `build` (windows-2022) | 325.5 s | 560.4 s | 234.9 s (3.9 min) |

So milestone 5 adds under 1 minute of Rust test time per Linux job and
about 4 minutes on Windows, inside its budgets of 3 and 6 minutes. On
Windows `oma-archive` went from 159.3 s to 310.8 s; its largest binaries in
PR A's run were `crash.rs` (78.2 s), `resume.rs` (52.0 s), `import_core.rs`
(23.7 s), `viewer_search.rs` (19.6 s) and `viewer_details.rs` (16.2 s), and
the shell's unit tests took 43.7 s. Run-to-run noise on the Windows runner
is large: `oma-messages`' `resume.rs` alone took 94.0 s in PR A's run.

**Milestone 6 (spec §0.1: at most 3 more minutes of Rust test time per
Linux job, 6 on Windows), measured from slice 6a.** The baseline is `main`
at `9883217` (milestones 1–5), run in a Linux container (4 CPUs, debug
build, `CARGO_INCREMENTAL=0`, after compiling), then again with slice 6a:

| What | Baseline (`9883217`) | Slice 6a | Slice 6b1 | Slice 6b2 | Slice 6c | Slice 6d | Slice 6e1 | Slice 6e2 |
|---|---|---|---|---|---|---|---|---|
| `cargo test --workspace`, wall clock | 2 min 8 s | 2 min 6 s | 2 min 26 s | 2 min 38 s | 2 min 42 s | 2 min 42 s | 2 min 41 s | 2 min 46 s |
| Sum of all test binaries' run times | 124.2 s | 123.1 s | 142.9 s | 153.9 s | 158.5 s | 159.0 s | 150.0 s | 161.7 s |
| Rust tests passed | 1,241 (12 ignored) | 1,251 (12 ignored) | 1,270 (12 ignored) | 1,284 (15 ignored) | 1,290 (16 ignored) | 1,332 (16 ignored) | 1,371 (16 ignored) | 1,392 (18 ignored) |
| `oma-archive`'s `export_caps.rs` | — | 2.4 s (4 tests) | 3.0 s | 2.7 s | 3.3 s | 3.3 s | 2.6 s | 2.6 s |
| `oma-archive`'s `export_reads.rs` | — | — | 5.4 s (12 tests) | 5.5 s | 5.3 s | 5.7 s | 5.6 s | 5.2 s |
| `oma-archive`'s `export_scope.rs` | — | — | — | 2.0 s (7 tests) | 2.6 s (the oracle also reads the export catalogue) | 2.4 s | 2.6 s | 2.2 s |
| `oma-archive`'s `export_memory.rs` | — | — | — | 10.6 s (1 test; Linux only) | 9.8 s | 10.1 s | 8.9 s | 9.4 s |
| `oma-archive`'s `export_fixtures.rs` | — | — | — | — | 0.7 s (5 tests, 1 ignored) | 0.7 s | 0.7 s | 0.6 s |
| `oma-archive`'s `bulk.rs` | — | — | — | — | 0.9 s (2 tests; the blob option's test is new) | 0.8 s | 0.8 s | 1.0 s |
| `oma-export`'s `record_catalogue.rs` | — | — | — | — | — | 8.1 s (7 tests) | 7.7 s | 8.2 s |
| `oma-export`'s `stage.rs`, `crash.rs`, `fanout.rs` | — | — | — | — | — | — | 0.1 s, 0.1 s, 0.6 s (15, 4 and 3 tests) | 0.1 s, 0.3 s (5 tests), 0.5 s |
| `oma-export`'s `txt.rs`, `attach.rs`, `golden_txt.rs` | — | — | — | — | — | — | — | 3.4 s (7 tests, 1 ignored), 0.8 s (6 tests), 3.1 s (1 test) |

The difference between the two runs is inside the run-to-run noise: a
first, cold run of the baseline in the same container took 3 min 58 s
(148.4 s summed). Slice 6a's own cost is `export_caps.rs` plus six quick
unit tests. Slice 6b1 adds `export_reads.rs` (5.4 s), one plan test and
six unit tests; the rest of its run's increase is inside the noise above
(the same binaries' times move by several seconds between runs). Slice
6b2 adds `export_scope.rs` (2.0 s), `export_memory.rs` (10.6 s: it writes
and reads back 336 MiB of version text), one plan test's new pins, six
unit tests and three ignored tests (the memory test's child helper, its
200-version variant and `perf_export.rs`). Slice 6c adds
`export_fixtures.rs` (0.7 s), the export catalogue in `export_scope.rs`'s
oracle (about 0.6 s) and the bulk blob test, plus the Linux CI oracle step
(about 2.5 s here: writing 22 archives, then the Python run, outside
`cargo test`); the Vitest `hidden-vectors.spec.ts` adds 32 frontend tests. Slice 6d
adds the crate `oma-export`: 42 tests, of which `record_catalogue.rs`
(8.1 s: it imports all 21 entries and scenarios of the two catalogues for
each of its 7 tests, in parallel) is the only one that takes measurable
time; the rest of its run is inside the noise. The Vitest
`export-words.spec.ts` adds 4 frontend tests. Slice 6e1 adds 39 tests (2
in `oma-fs`, 14 unit tests and 23 integration tests in `oma-export`); the
crash test's 14 child processes take about 0.1 s together. The lower
summed time of that run is within the run-to-run noise described above.
Slice 6e2 adds 21 tests (6 unit tests and 15 integration tests in
`oma-export`) and two ignored ones (`write_oracle_txt`, `perf_attach`):
`txt.rs` and `golden_txt.rs` each import every catalogue entry and export
it through the pipeline (about 3 s each), and the crash test's new kill
imports one entry and runs two child exports. The Linux CI oracle step
gains the TXT oracle (about 2.5 s for the export, under 1 s for the
Python run here).
Windows has not been measured for M6. Slices 6h and 6l record the figures again, with CI's.

**Export read measurements (slice 6b2; synthetic, one host, no timing
asserted).** In the same container, debug test build with `oma-archive`
and SQLite at `opt-level = 3`, the archive and temporary files on a tmpfs:

```sh
TMPDIR=/dev/shm cargo test -p oma-archive --test perf_export -- --ignored --nocapture
TMPDIR=/dev/shm cargo test -p oma-archive --test export_memory -- --ignored --exact \
  history_of_two_hundred_16_mib_versions_streams_under_300_mib --nocapture
```

| Read (bulk archive: 500,000 messages, 2,000 chats, 412 MiB) | Time |
|---|---|
| `export_preview`, whole archive (500,000 rows, 474,991 distinct, 25,009 folded) | 7.0 s |
| `export_preview`, the largest conversation (200,375 rows) | 1.5 s |
| `scope_index`, whole archive | 1.9 s |
| Cursors, pre-measure, byte batches and `assemble` at `Items`, every timeline item (474,991 items, 3,302 batches) | 77.8 s (6,107 items/s) |
| `search_ids`, cap 1,000,000, words ("zephyr": 88,731 hits) / exact text (88,777 hits) | 0.26 s / 0.91 s |
| `search_scope` over those sets (no code or withheld text in the bulk data, so every hit takes the fast path) | 0.33 s each |
| Peak resident set of the whole test process (`VmHWM`) | 49 MiB |

The hostile history (one message, a 16 MiB current body and 199 earlier
versions of 16 MiB, 3.2 GiB of text) streamed in 64 s with the child's
`VmHWM` at 74,248 KiB (72.5 MiB); the 21-version test that runs on every
push peaked at 74,256 KiB. The 500,000-message targets of spec §12 (the
formats, 5 minutes, 300 MiB) are slice 6f's; these figures are the read
side only. The classification's cost for hits that need the slow path
(masked or withheld content) and the grouping of 1,000,000 hits were not
measured.

**Optional oracles** (manual; nothing enters the repository or CI; each
run is recorded in `docs/compatibility.md` with the checksum it tested):

```sh
# Oracle 1: iphone_backup_decrypt decrypts every encrypted messages fixture to .expected
.venv-fixtures/bin/pip install -r scripts/fixtures/requirements-oracle.txt
.venv-fixtures/bin/python -I scripts/fixtures/verify_messages_with_oracle.py tests/fixtures/messages
# Oracle 2: pytypedstream (LGPL) as a black-box subprocess, pinned by wheel hash
python3 -m venv .venv-oracle-ts
.venv-oracle-ts/bin/pip install --require-hashes -r scripts/fixtures/requirements-oracle-typedstream.txt
.venv-oracle-ts/bin/python -I scripts/fixtures/verify_typedstream_with_oracle.py tests/fixtures/typedstream
```

Oracles 3 and 4 run the imessage-exporter 4.3.0 binary (GPL-3.0) as a black
box from a scratch directory; their steps are in
[`tests/fixtures/messages/README.md`](../tests/fixtures/messages/README.md).
No GPL or LGPL tool or wrapper is committed.

### Heavier M3 tests (run by hand)

| Test | Command (Linux; on Windows set the variable with `$env:NAME=value;` first) |
|---|---|
| Typedstream mutations, one million iterations (20,000 by default) | `OMA_TS_MUTATIONS=1000000 cargo test -p oma-messages --release --test typedstream_mutation` |
| Synthetic performance: 1,000,000 messages, 2,000 chats, 50,000 attachments | `cargo test -p oma-messages --release -- --ignored perf_synthetic_large --nocapture` |
| The archive's growth factor `K`, the time of each import phase, the FTS size and the time of a quick and a full verify, on a synthetic import (`OMA_PERF_MESSAGES` messages, default 20,000; generating 100,000 takes about 4 minutes) | `OMA_PERF_MESSAGES=100000 cargo test -p oma-archive --release --test perf -- --ignored --nocapture` |
| The check on a backup outside the repository (oracle run 4) | `OMA_EXTERNAL_BACKUP=<backup dir> cargo test -p oma-messages --test external_sample -- --ignored --nocapture` |
| One large photo made into both image renditions (M5 slice 5d: a 4032 × 3024 noise JPEG through `media::render`) | `cargo test -p oma-archive --release --test media_corpus -- --ignored --nocapture` |
| Search on a bulk archive (M5 slice 5c; `OMA_PERF_MESSAGES` messages, default 500,000) | `cargo test -p oma-archive --test perf_search -- --ignored --nocapture` |
| The viewer reads of spec §11 on bulk archives: the conversation list (2,000 and 20,000 chats), thread pages, `seek_date`, `day_counts` and a chat of 90 % reactions (M5 slice 5k; `OMA_PERF_MESSAGES`, `OMA_PERF_LIST_MESSAGES` and `OMA_PERF_REACTION_MESSAGES` set the sizes; about 1 minute and 550 MiB of temporary space at the defaults) | `cargo test -p oma-archive --test perf_viewer -- --ignored --nocapture` |
| A library for the viewer's smoke test: one archive with three viewer catalogue entries (M5 slice 5k) | `OMA_SMOKE_LIB=/dev/shm/oma-smoke/lib cargo test -p oma-archive --test smoke_library -- --ignored --nocapture` (the folder must be empty or absent) |
| The media code with the optional, never-shipped HEIC decoder (owner decision Q2; CI runs only clippy and `cargo deny` with it) | `cargo clippy -p oma-archive --all-targets --features heic -- -D warnings`, `cargo test -p oma-archive --features heic --test viewer_media --test media_corpus` and `cargo deny --features oma-archive/heic check` |
| A self-built app with HEIC previews (owner decision Q2: **at your own legal risk**, HEVC is pooled-patent technology; never for official or CI release builds). The app's package does not forward the feature, so name the crate's feature | `npx tauri build --features oma-archive/heic` (the release binary, through the Tauri CLI as always), or `cargo build -p openmessage-archive --features oma-archive/heic` for a debug build; `--features heic` alone fails ("the package 'openmessage-archive' does not contain this feature"). Only the feature resolution was checked (`cargo tree -p openmessage-archive --features oma-archive/heic -i heic-rs`); no HEIC-enabled app build has been run |

Record performance results in `docs/compatibility.md` as **synthetic**,
with the profile, OS and CPU count.

`external_sample` is for **third-party sample databases wrapped in a
synthetic backup** (oracle run 4), not for a backup of a real phone: it
copies the whole backup into the system temp folder and stages the
plaintext sms.db there, outside the archive folder that `docs/privacy.md`
describes. Check a real backup in the app instead (the manual checklist in
`docs/compatibility.md`), and never commit anything from one. If a sample
needs `OMA_EXTERNAL_PASSWORD`, never type the password on the command line:
read it without echo (`read -rs OMA_EXTERNAL_PASSWORD && export
OMA_EXTERNAL_PASSWORD` in bash; `$env:OMA_EXTERNAL_PASSWORD = Read-Host
-MaskInput` in PowerShell 7) and unset it afterwards.

**Wall-time targets on CI.** With the bundled SQLite and `oma-messages` at
`opt-level = 3` in dev and test builds, each test file in
`crates/oma-messages/tests/` should finish within 60 s on a CI runner, and
`cancel.rs` and `typedstream_mutation.rs` within 120 s. These targets are
not enforced by a timer; review them when a run exceeds them.

### Headless smoke test of the Backups and Archive pages and the viewer

The GTK folder picker cannot list directories without D-Bus, so pre-write
`settings.json` with an archive folder and an added parent folder that
holds copies of some fixtures, then drive the release binary with `xdotool`
(`ctrl+3` opens the Backups page). See the app's `CLAUDE.md` for the Xvfb
command. Copies of the messages catalogue entries work for the extraction
check: each entry is a folder holding one backup folder
(`SYNTHETIC0-0000000000000000`), so either add each entry as a parent
folder, or move each backup folder up into one parent. Encrypted entries
use the password `synthetic-test-password`.

For the Archive page (`ctrl+1`), add entries of the import catalogue
(`tests/fixtures/imports/`) the same way: each entry folder holds one
backup folder, so add each entry as a parent folder. Put the archive
folder on a volume with more free space than the import's reserve (5 % of
the volume, clamped to 1–10 GiB); a 16 GB tmpfs such as `/dev/shm` works.
Start `Xvfb :99` once and run the binary with `DISPLAY=:99` in the
background, so the window can be driven step by step with `xdotool` and
`import -window root`; the confirmation step waits two minutes for an
answer. The milestone 4 run is recorded in `docs/compatibility.md`.

For the viewer (M5), make a library with `smoke_library.rs` (the table
above) instead of importing through the window, and point
`archiveDirectory` at it:

```sh
OMA_SMOKE_LIB=/dev/shm/oma-smoke/lib \
  cargo test -p oma-archive --test smoke_library -- --ignored --nocapture
mkdir -p "$S/config/com.vibeforgesystems.openmessagearchive"
printf '%s' '{"schemaVersion": 1, "archiveDirectory": "/dev/shm/oma-smoke/lib", "theme": "light", "backupFolders": []}' \
  > "$S/config/com.vibeforgesystems.openmessagearchive/settings.json"
```

Run the release binary with `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and
`XDG_CACHE_HOME` all under `$S`, so the web profile's files can be checked
afterwards. On the Archive page, scroll to **Browse conversations**. The
first open after a launch can take a few seconds to fill the list.
`xdotool type` needs `LC_ALL=C.UTF-8` for non-ASCII text (a CJK search).
Without an audio device, playback ends with "This file could not be played
here."; record that as not tested. The slice 5k run is recorded in
`docs/compatibility.md`.

## Repository conventions

This project lives in the `From_the_Forge` repository, which gates itself
with `.vibeforge/gate.sh`. Sign off every commit (`git commit -s`). The DCO
terms for this directory are in [`../DCO`](../DCO).
