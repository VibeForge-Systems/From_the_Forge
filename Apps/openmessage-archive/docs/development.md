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
| The check on a backup outside the repository (oracle run 4) | `OMA_EXTERNAL_BACKUP=<backup dir> cargo test -p oma-messages --test external_sample -- --ignored --nocapture` |

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

### Headless smoke test of the Backups page

The GTK folder picker cannot list directories without D-Bus, so pre-write
`settings.json` with an archive folder and an added parent folder that
holds copies of some fixtures, then drive the release binary with `xdotool`
(`ctrl+3` opens the Backups page). See the app's `CLAUDE.md` for the Xvfb
command. Copies of the messages catalogue entries work for the extraction
check: each entry is a folder holding one backup folder
(`SYNTHETIC0-0000000000000000`), so either add each entry as a parent
folder, or move each backup folder up into one parent. Encrypted entries
use the password `synthetic-test-password`.

## Repository conventions

This project lives in the `From_the_Forge` repository, which gates itself
with `.vibeforge/gate.sh`. Sign off every commit (`git commit -s`). The DCO
terms for this directory are in [`../DCO`](../DCO).
