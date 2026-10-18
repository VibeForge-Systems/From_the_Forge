# Development

## Prerequisites

| Tool | Version | Notes |
|---|---|---|
| Rust | 1.97.0, pinned in `rust-toolchain.toml` | Install [rustup](https://rustup.rs). The pinned toolchain installs automatically on first build. |
| Node.js | 22.12 or later | npm 10 ships with it |
| Platform libraries | See below | |

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
   and the Windows SDK.
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
- **Fixtures** (from M2) go in `tests/fixtures/`. They must be synthetic.
  **Never commit a real backup, real messages, or real contact data**, even
  "anonymized" by hand. `.gitignore` blocks `*.db` and `*.sqlite` outside
  `tests/fixtures/`.

## Repository conventions

This project lives in the `From_the_Forge` repository, which gates itself
with `.vibeforge/gate.sh`. Sign off every commit (`git commit -s`). The DCO
terms for this directory are in [`../DCO`](../DCO).
