# Dependencies and licenses

OpenMessage Archive is Apache-2.0 and must remain redistributable under it.
Every dependency is pinned to an exact version. Adding or upgrading a
dependency is a deliberate change: record it in the upgrade log at the end
of this file.

Two automated audits enforce the license policy:

- **Rust:** [`deny.toml`](../deny.toml), via `cargo deny check`. It allows
  permissive licenses, plus MPL-2.0 for five named crates (see below). It
  rejects unknown registries and git sources, and fails on yanked crates
  and vulnerabilities. It runs in CI.
- **JavaScript:** [`scripts/check-licenses.mjs`](../scripts/check-licenses.mjs),
  via `npm run licenses`. It reads each package's license from
  `package-lock.json`. It fails if a runtime package is outside the
  allowlist, and reports dev-only tools for review. It runs in CI.

## Direct Rust dependencies

| Crate | Version | License | Why |
|---|---|---|---|
| tauri | 2.12.4 | Apache-2.0 OR MIT | Desktop runtime |
| tauri-build | 2.7.3 | Apache-2.0 OR MIT | Build script: context and ACL permissions |
| tauri-plugin-dialog | 2.8.3 | Apache-2.0 OR MIT | Native folder picker, driven from Rust only |
| tauri-plugin-opener | 2.7.2 | Apache-2.0 OR MIT | Show the archive folder in the file manager, driven from Rust only |
| serde / serde_json | 1.0.229 / 1.0.154 | MIT OR Apache-2.0 | IPC and settings serialization |
| thiserror | 2.0.21 | MIT OR Apache-2.0 | Error types |
| tracing / tracing-subscriber | 0.1.44 / 0.3.23 | MIT | Diagnostic logging |
| ts-rs | 12.0.1 | MIT | Generates TypeScript IPC types from Rust |
| dunce | 1.0.5 | CC0-1.0 OR MIT-0 OR Apache-2.0 | Canonical paths without Windows `\\?\` prefixes |
| getrandom | 0.3.4 | MIT OR Apache-2.0 | The shell's media tokens (M5 spec §6.3, slice 5f): 128 random bits per token from the OS (`getrandom::fill`). Default features only. Already in the lockfile through Tauri's tree; a direct dependency of `src-tauri` (as a workspace dependency) from slice 5f |
| tempfile (dev only) | 3.27.0 | MIT OR Apache-2.0 | Test directories |
| aes | 0.9.3 | MIT OR Apache-2.0 | AES-256 for backup decryption (`oma-backup`); `zeroize` feature |
| aes-kw | 0.3.1 | MIT OR Apache-2.0 | RFC 3394 key unwrap for class, manifest and file keys; default features off (drops `const-oid`), `zeroize` on |
| cbc | 0.2.1 | MIT OR Apache-2.0 | AES-CBC decryption with strict PKCS#7 (`block-padding`, `zeroize`) |
| hmac | 0.13.0 | MIT OR Apache-2.0 | HMAC for the in-crate, cancellable PBKDF2 |
| sha1 / sha2 | 0.11.0 / 0.11.0 | MIT OR Apache-2.0 | HMAC-SHA1 and HMAC-SHA256 for PBKDF2; SHA-1 for fileIDs; SHA-256 for ids and staging names. Default features off, `zeroize` on |
| zeroize | 1.9.1 | Apache-2.0 OR MIT | Wiping passwords, keys and plaintext buffers |
| plist | 1.10.1 | MIT | Property lists, read only through the budgeted event-stream reader. Default features (`serde`) off; `enable_unstable_features_that_may_break_with_minor_version_bumps` on, because that feature is what makes `plist::stream` public. The exact pin makes any change to that API a reviewed upgrade |
| rusqlite | 0.40.2 | MIT | Read-only access to staged copies of Manifest.db and (M3) sms.db, and (M4) **read-write** access to each archive's `archive.sqlite` (`oma-archive`: WAL, the import's batch transactions, the verifier's states), with its FTS5 index (FTS5 is compiled in by `bundled`). Features `bundled` (compiles SQLite 3.53.2, public-domain C source, through libsqlite3-sys 0.38.2, MIT), `limits` (`Connection::set_limit`) and `hooks`: the authorizer and progress handler that guard staged sms.db connections, the progress handler behind the archive's op budgets, cancellable reads and armed interrupt, and (tests only, through the `fault-injection` feature) the commit hook the ordering test observes. `limits` and `hooks` are empty features that add no crates. `cache` (statement cache) stays off, because it adds `hashlink` |
| pbkdf2 (dev only) | 0.13.0 | MIT OR Apache-2.0 | Differential tests against the in-crate PBKDF2; never linked into the app |
| fs4 | 1.1.0 | MIT OR Apache-2.0 | Free space for the archive's space checks (`oma_fs::available_space`): `statvfs` on Unix, `GetDiskFreeSpaceExW` (caller-scoped, so quotas count) on Windows. Default features off: `sync` (file locking) is not used. It depends on `rustix` (Unix) and `windows-sys` 0.61.2 (Windows), both already in the tree |
| rustix (Linux only) | 1.1.5 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | `oma-fs` on Linux: `statfs` for the volume kind, `renameat2(RENAME_NOREPLACE)`, `O_NOFOLLOW` opens and `geteuid`, all without `unsafe`. Features `fs` and `process` (plus the default `std`). Already in the tree through Tauri's Linux stack, with both features on |
| image | 0.25.10 | MIT OR Apache-2.0 | Decodes and re-encodes image renditions in `oma-archive`'s `media::render`, the pure function the decode helper process runs (M5 spec §5.5; a dependency from slice 5d, a dev-dependency in 5a), and encodes the viewer fixtures (`make_viewer_media`, `tests/viewer_fixtures.rs`). Only `media/render.rs` may name it (`tests/source_rules.rs`). Default features off; only `jpeg`, `png`, `gif` and `webp` on (no `avif`, an encoder through `ravif`/`rav1e` that M5 does not need, and no `avif-native`, the decoder, which pulls in MPL-2.0 `mp4parse` and C `dav1d`; no TIFF, EXR or rayon). MSRV 1.88, under the workspace's 1.90 |
| heic-rs (optional, never shipped) | 0.1.4 | MIT OR Apache-2.0 | Owner decision Q2 (M5): HEIF decoding behind `oma-archive`'s non-default `heic` feature, inside `media::render` only. **Official and CI release builds never enable it**; CI only runs clippy and `cargo deny` with it on Linux. Pure Rust, `#![forbid(unsafe_code)]`, no C; default features off and `std` on, so its optional `rayon` (the `parallel` feature) is not pulled in and it has **no dependencies**. Its NOTICE says it was written from the ISO/IEC and ITU-T texts without GPL, LGPL or AGPL code, and that it **grants no patent licence to HEVC**. HEVC is pooled-patent technology (Access Advance's HEVC Advance overview lists "All HEVC Software" with per-copy royalties; a reported waiver for application-layer software is UNVERIFIED), so a self-builder who enables the feature takes that legal risk (`privacy.md`). Young: first published 2026-09-12, 0.1.4 on 2026-10-11, edition 2024 and MSRV 1.85 (under the toolchain's 1.97). Its decode path is untested here (no synthetic HEIC image can be made without an encoder). The AGPL `heic` crate and LGPL libheif (and the MIT `libheif-rs` wrapper over it) are not used |

`tracing-subscriber` (above) is also a dev-dependency of `oma-backup` and
`oma-messages`, for their log-hygiene tests (and of `oma-archive`, for
its own). The shell (`src-tauri`) depends on the workspace crates
`oma-backup` (from M2), `oma-messages` (from M3) and `oma-archive` (from
M4, slice 4l); none adds a third-party crate beyond the list above.
`oma-archive` depends only on crates already listed (`rusqlite`, `serde`,
`serde_json`, `sha2`, `thiserror`, `dunce`, `tracing`, `zeroize`, and from
M5 `image` and the optional `heic-rs`; `tempfile` and `tracing-subscriber`
for its tests) and on the workspace crates
`oma-backup`, `oma-fs` and `oma-messages`. `oma-messages`
depends only on crates already listed (`rusqlite`, `sha2`, `thiserror`,
`tracing`, `zeroize`) and on the workspace crates `oma-backup` and
`oma-fs`; its typedstream decoder is our own code, not a dependency.
`oma-export` (M6, slice 6d) depends only on crates already listed
(`serde`, `serde_json`, `thiserror`; `tempfile`, `rusqlite` and `dunce`
for its tests, `rusqlite` there only to apply the export catalogue's
`patches` to a test's own archive) and on the workspace crates
`oma-archive` (and `oma-backup` for its tests); its Unicode property
tables are generated by our own script from Node's regular-expression
engine, not a dependency. No
JavaScript dependency was added in M2 or M3: `Channel` comes from
`@tauri-apps/api` 2.12.4, which was already a dependency.

`image` (M5) brings these crates, all new to the lockfile except where
noted, with their licenses as `Cargo.toml` states them: `zune-jpeg` 0.5.15
and `zune-core` 0.5.3 (MIT OR Apache-2.0 OR Zlib), `gif` 0.14.2, `image-webp`
0.2.4 and `weezl` 0.1.12 (MIT OR Apache-2.0), `moxcms` 0.8.1 and `pxfm`
0.1.30 (BSD-3-Clause OR Apache-2.0), `color_quant` 1.1.0 (MIT),
`byteorder-lite` 0.1.0 (Unlicense OR MIT) and `quick-error` 2.0.1 (MIT OR
Apache-2.0). `png` 0.18.1, `fdeflate`, `simd-adler32`, `flate2`,
`miniz_oxide`, `bytemuck` and `num-traits` were already in the lock through
Tauri. Every license is on `deny.toml`'s allowlist, which is unchanged.
Several of these crates contain `unsafe` SIMD code; the workspace's
`unsafe_code = "forbid"` covers only our own crates, which is one reason
images are decoded in a helper process (M5 spec §5.5).

The RustCrypto crates resolve to one `cipher` (0.5.2), one `digest` (0.11.3)
and one `hybrid-array` (0.4.15) major for the backup code. An older
`digest` 0.10 and `sha2` 0.10 remain in the tree through Tauri; nothing in
`oma-backup` uses them.

Transitive crates are listed by `cargo tree` and audited by `cargo deny`.

### MPL-2.0 crates (reviewed exception)

Five transitive crates are **MPL-2.0**, and all of them arrive through
Tauri:

- `cssparser`, `cssparser-macros`, `selectors` and `dtoa-short`, through
  `tauri-utils` → `dom_query`.
- `option-ext`, through `dirs` → `dirs-sys`.

MPL-2.0 is *file-level* copyleft. Section 3.3 lets the covered files be
combined into a larger work under another license, here Apache-2.0. The
obligation is that the MPL-covered files themselves stay under MPL-2.0 and
their source remains available. They are used unmodified, and their source
is published on crates.io. The NOTICE file says so.

They are allowed by name in `deny.toml`, not by adding MPL-2.0 to the
global allowlist. Any new MPL dependency still fails CI. Avoiding them would
mean leaving Tauri.
The release binary contains no HTTP client crate (`reqwest`, `hyper`,
`ureq`). Re-check with
`cargo tree -e normal | grep -iE 'reqwest|hyper|ureq'`.

## Direct JavaScript dependencies

| Package | Version | License | Shipped |
|---|---|---|---|
| vue | 3.5.44 | MIT | yes |
| vue-router | 4.6.4 | MIT | yes |
| pinia | 3.0.4 | MIT | yes |
| bootstrap (CSS only) | 5.3.8 | MIT | yes |
| bootstrap-icons | 1.13.1 | MIT | yes (font files bundled locally) |
| @tauri-apps/api | 2.12.4 | Apache-2.0 OR MIT | yes |
| @tauri-apps/cli | 2.12.3 | Apache-2.0 OR MIT | build tool |
| vite / @vitejs/plugin-vue | 7.3.7 / 6.0.10 | MIT | build tool |
| typescript / vue-tsc | 5.9.3 / 3.3.13 | Apache-2.0 / MIT | build tool |
| vitest / @vue/test-utils / jsdom | 4.1.12 / 2.4.6 / 27.4.0 | MIT | test tool |
| @types/node | 22.20.5 | MIT | types |

Versions were chosen one major release behind the newest where the newest
was under three months old: TypeScript 7, Vite 8, Vitest 5, vue-router 5
and Pinia 4 all had recent majors. The upgrade log tracks when to move.

`legacy-peer-deps=true` is set in `.npmrc`. npm 10's dependency resolver
crashes (`TypeError: Cannot read properties of null (reading 'edgesOut')`)
while resolving vitest 4's optional peer graph. Every peer this project
needs is a direct dependency, so nothing is lost. A side effect is that
Bootstrap's optional `@popperjs/core` peer is not installed, which is
correct because Bootstrap's JavaScript is not used.

### Dev-only Python tooling

Used only to generate and check the synthetic test fixtures in
`tests/fixtures/` and, from M6, the exports. Nothing here is shipped. The
generated files are committed and checked by `SHA256SUMS`.
`make_synthetic_messages.py`, `make_synthetic_imports.py`,
`make_synthetic_viewer.py` and `make_synthetic_exports.py` need only
`requirements.txt`, and `make_synthetic_typedstream.py` only the standard
library. Until M6 CI ran no Python; from slice 6c the Linux `checks` job
runs the export oracles (below), in a virtual environment, with
`python3 -I`.

| Package | Version | License | Use |
|---|---|---|---|
| cryptography | 50.0.2 | Apache-2.0 OR BSD-3-Clause | AES, key wrap and padding in `scripts/fixtures/make_synthetic_backups.py` (pinned in `scripts/fixtures/requirements.txt`) |
| iphone_backup_decrypt | 0.11.2 | MIT | Independent oracle, run by hand through `scripts/fixtures/verify_with_oracle.py`. Used as a black box; not distributed and not ported. Pinned in `scripts/fixtures/requirements-oracle.txt` |
| pycryptodome | 4.0.1 | BSD-2-Clause AND Unlicense | The oracle's own dependency (`Requires-Dist: pycryptodome>=3.23.0`); it performs the oracle's AES. Pinned in `scripts/fixtures/requirements-oracle.txt` at the version the recorded oracle run used |
| pytypedstream | 0.1.0 | **LGPL-3.0-or-later** | Black-box oracle for the typedstream vectors (M3 oracle run 2), run as a subprocess through `scripts/fixtures/verify_typedstream_with_oracle.py` in its own virtual environment. Never imported by our code, never shipped, not run in CI. Pinned with its wheel hash in `scripts/fixtures/requirements-oracle-typedstream.txt` |

Oracle runs 3 and 4 (M3) also use the imessage-exporter 4.3.0 release
binary (GPL-3.0) as a black box from a scratch directory. It is not a
dependency of anything here, and no wrapper for it is committed.

**Export oracles (M6 spec §14.4)**, pinned with their wheel hashes in
`scripts/oracles/requirements.txt` and installed only with
`pip install --require-hashes --only-binary :all: --no-deps` into a
virtual environment (CI: `$RUNNER_TEMP/venv-oracles`; never system-wide).
Every one is a pure-Python wheel except `rpds-py`, which is pinned by two
hashes: its CPython 3.10 manylinux wheel (the `ubuntu-22.04` runner's
Python) and its CPython 3.13 wheel (the reference fixture environment).
Versions were chosen on 2026-10-19 as the newest releases at least 14 days
old that still support Python 3.10. They are used as black boxes by
`scripts/oracles/run_oracles.py`; no code from them is copied, and none is
shipped. `openpyxl` and `pypdf` arrive with the XLSX and PDF slices (PR B).

| Package | Version | License | Use |
|---|---|---|---|
| jsonschema | 4.26.0 | MIT | Validates projections, `export.json` and `hidden-vectors.json` against `scripts/oracles/schemas/` (from slice 6f also every JSON export, strict and open schemas) |
| jsonschema-specifications | 2025.9.1 | MIT | jsonschema's bundled meta-schemas |
| referencing | 0.37.0 | MIT | jsonschema's `$ref` resolution |
| rpds-py | 0.30.0 | MIT | referencing's persistent maps (compiled; the last release with CPython 3.10 wheels, as 2026 releases need 3.11) |
| attrs | 26.1.0 | MIT | jsonschema and referencing |
| typing-extensions | 4.16.0 | PSF-2.0 | referencing on Python below 3.13 |
| html5lib | 1.1 | MIT | The HTML5 tree oracle of slice 6g; installed (and its version printed) from 6c |
| six | 1.17.0 | MIT | html5lib |
| webencodings | 0.5.1 | BSD-3-Clause | html5lib |

## System and platform components

These are not bundled. They come from the operating system or the user.

| Component | License | Needed for | Notes |
|---|---|---|---|
| WebKitGTK 4.1 (Linux) | LGPL-2.1 / BSD | UI | A shared system library, dynamically linked |
| GTK 3 (Linux) | LGPL-2.1 | Window and dialogs | Same |
| Microsoft Edge WebView2 Runtime (Windows) | **Proprietary (Microsoft)** | UI | Preinstalled on Windows 11. This is the one proprietary runtime the application cannot avoid on Windows, because Tauri renders through the OS webview. See [privacy.md](privacy.md). |
| usbmuxd (Linux) | GPL-2.0+/GPL-3.0 | Direct device backup (M7) only | A separate daemon reached over a socket. It is not linked or bundled. |
| Apple Mobile Device Service (Windows) | **Proprietary (Apple)** | Direct device backup (M7) only | Installed by the user with Apple Devices or iTunes. Importing existing backups never needs it. |

## Projects used only as references

These projects informed the design but **must not be copied from or linked
against**, because their licenses are incompatible with Apache-2.0
distribution. Reading their documentation, and comparing output against
them locally, is fine. See [supported-formats.md](supported-formats.md).

| Project | License |
|---|---|
| imessage-exporter / imessage-database | GPL-3.0 |
| crabstep (typedstream) | GPL-3.0 |
| crabapple (backup decryption) | GPL-3.0 |
| pymobiledevice3 | GPL-3.0 |
| pyiosbackup | GPL-3.0 |
| libimobiledevice (including `idevicebackup2`) | LGPL-2.1+ |
| StackOverflow answer 13793043 (backup decryption) | CC BY-SA (the version depends on the revision date; not checked) |
| pytypedstream (typedstream; black-box oracle only) | LGPL-3.0-or-later |
| ChatExportKnowledge (sms.db facts, restated with attribution) | CC BY 4.0 |
| heic (imazen) | AGPL-3.0 |
| libheif, libde265 (and the MIT `libheif-rs` wrapper, which links them) | LGPL (C libraries; version not re-checked) |

## Upgrade log

**Milestone 4 in summary:** one new third-party crate, `fs4` 1.1.0 (MIT OR
Apache-2.0), and a direct dependency on `rustix` 1.1.5 on Linux, which was
already in the tree; the new workspace crate `oma-archive`; no JavaScript
dependency change; `deny.toml` unchanged. Slice 4n changed no dependency.

**Milestone 5 in summary:** two new third-party crates, `image` 0.25.10
(with ten crates it brings in) and `heic-rs` 0.1.4 (optional, behind
`oma-archive`'s non-default `heic` feature, never in official or CI release
builds); `getrandom` 0.3.4 as a direct dependency (already locked through
Tauri); the npm package virtua 0.53.1 added for spike 5s(1) and removed
again (41 runtime npm packages, as before); `deny.toml` unchanged. Slice 5k
changed no dependency.

| Date | Change | Reason |
|---|---|---|
| 2026-10-17 | Initial set (Milestone 1) | — |
| 2026-10-17 | MPL-2.0 exception for five Tauri transitive crates | Found by the first `cargo deny` run; see above |
| 2026-10-17 | Project moved to `Apps/openmessage-archive/` | Repository layout; no dependency change |
| on merge (M2) | Added aes 0.9.3, aes-kw 0.3.1, cbc 0.2.1, hmac 0.13.0, sha1 0.11.0, sha2 0.11.0, zeroize 1.9.1, plist 1.10.1, rusqlite 0.40.2; pbkdf2 0.13.0 as a dev-dependency | Milestone 2: backup parsing and decryption in `crates/oma-backup`. `cargo deny check` passes with the unchanged `deny.toml` |
| on merge (M2) | rusqlite with `bundled` and `limits` | Hardened read-only open of staged Manifest.db copies (`set_limit`); bundled SQLite 3.53.2 |
| on merge (M2) | plist with `enable_unstable_features_that_may_break_with_minor_version_bumps`, default features off | `plist::stream::Reader` is public only under this feature; the budgeted reader never builds a `plist::Value` from backup data |
| on merge (M2) | sha2 0.11.1 deferred | 0.11.1 was under two weeks old and raises the MSRV to 1.88; upgrade later and log it |
| on merge (M2) | `TS_RS_LARGE_INT = "number"` in `.cargo/config.toml` | IPC `u64` fields become TypeScript `number`; no generated file changed when it was added |
| on merge (M2) | Dev-only Python tooling: cryptography 50.0.2; iphone_backup_decrypt 0.11.2 as an oracle | Synthetic fixture generation and the manual cross-check |
| on merge (M2) | Dev-only: pycryptodome 4.0.1 pinned for the oracle, with iphone_backup_decrypt, in `scripts/fixtures/requirements-oracle.txt` | It was installed unpinned with the oracle; the recorded oracle run used 4.0.1, and repeating the run must use the same crypto implementation |
| on merge (M2) | New setting `backupFolders` (settings format stays 1) | The Backups page remembers added folders. **Downgrade caveat:** an M1 build ignores the field and drops it on its next save |
| on merge (M2) | Tauri channels for job progress, with no `core:*` permission | Relies on Tauri 2.12.4 exempting `plugin:__TAURI_CHANNEL__\|fetch` from the ACL check, a special case marked "remove in v3". **Every Tauri upgrade must re-verify, by hand, that a `finished` event over 8 KiB arrives without a `core:*` grant** |
| on merge (M2) | The shell depends on the workspace crate `oma-backup` | Backup discovery, validation and the job worker; no new third-party crate |
| on merge (M3, PR 3a) | rusqlite feature `hooks` enabled | `sqlite3_set_authorizer` and `sqlite3_progress_handler` for the staged sms.db connection (read-only authorizer, op budgets). `hooks = []` in rusqlite 0.40.2's `Cargo.toml`: no new code from outside the rusqlite crate, and no new third-party crate; `Cargo.lock` is unchanged. `cargo deny check` passes with the unchanged `deny.toml` |
| on merge (M3, PR 3a) | Dev and test builds compile `libsqlite3-sys` (the bundled SQLite) at `opt-level = 3` | Cargo passes the profile's opt level to the `cc` build, so whole-database passes and integrity checks in tests run at release speed. Release builds are unaffected. The matching override for `oma-messages` is added with that crate (PR 3b), because Cargo warns about a profile entry for a package that does not exist yet |
| on merge (M3, PR 3b) | New workspace crate `oma-messages` (sms.db schema probe and record passes), at `opt-level = 3` in dev and test builds | Its dependencies are crates already in the workspace (`oma-backup`, `rusqlite` with the existing `bundled`, `limits` and `hooks` features, `thiserror`, `tracing`, `zeroize`; `tempfile` for tests). No new third-party crate: the only `Cargo.lock` change is the crate's own entry. `cargo deny check` passes with the unchanged `deny.toml` |
| on merge (M3, PR 3c) | `oma-messages` gains the dev-dependencies `serde_json` and `sha2`, both already workspace dependencies | Typedstream vector tests read `expected.json`; the fixture-integrity test hashes the vectors. No new third-party crate: `Cargo.lock` changes only in `oma-messages`'s own dependency list. `cargo deny check` passes with the unchanged `deny.toml` |
| on merge (M3, PR 3c) | Dev-only Python tooling: pytypedstream 0.1.0 (LGPL-3.0-or-later) as a black-box oracle, pinned with its wheel hash in `scripts/fixtures/requirements-oracle-typedstream.txt` | Oracle run 2 for the typedstream vectors. It is run as a subprocess, never imported, never shipped and not run in CI. `make_synthetic_typedstream.py` itself needs only the standard library |
| on merge (M3, PR 3d) | `sha2` moves from a dev-dependency to a dependency of `oma-messages` | `GuidSets` hashes message, reply-originator and association-target GUIDs with truncated SHA-256 (M3 §7.3, D46). `sha2` is already a workspace dependency (through `oma-backup`); `plist` is still reached only through `oma_backup::bplist`. No new third-party crate, and `Cargo.lock` is unchanged. `cargo deny check` passes with the unchanged `deny.toml` |
| on merge (M3, PR 3e) | `oma-messages` depends on the workspace crate `oma-fs`, and gains the dev-dependency `tracing-subscriber` (already a workspace dependency) | `oma_fs::resolve_within` gates attachment lookups (I2); the log-hygiene test captures every event with a global subscriber. No new third-party crate: `Cargo.lock` changes only in `oma-messages`'s own dependency list. `cargo deny check` passes with the unchanged `deny.toml` |
| on merge (M3, PR 3e) | Dev-only Python tooling: `make_synthetic_messages.py` needs only the M2 generator's `requirements.txt` (`cryptography`); oracle run 1 reuses `requirements-oracle.txt` | The messages catalogue and its oracle check. Oracle runs 3 and 4 use the imessage-exporter 4.3.0 binary (GPL-3.0) as a black box in a scratch directory; it is not a dependency, is never committed, and is not run in CI |
| on merge (M3, PR 3f) | The shell depends on the workspace crate `oma-messages` | The extraction-check job (`check_extraction`). No new third-party crate: the only `Cargo.lock` change is `oma-messages` added to the shell's own dependency list. No new npm package. `cargo deny check` passes with the unchanged `deny.toml` |
| on merge (M4, slice 4a) | Added fs4 1.1.0 (default features off) as a dependency of `oma-fs`; `oma-fs` depends directly on rustix 1.1.5 on Linux, with features `fs` and `process` | Volume facts, the private-permission check and durable no-replace placement (M4 spec §3.1). One new crate (`fs4`); `rustix` and `windows-sys` 0.61.2 were already locked, and the only other `Cargo.lock` change is `oma-fs`'s own dependency list. `cargo tree -e features` shows that `fs4` turns on `windows-sys` 0.61.2's `Win32_System_IO` feature, which nothing else in the tree enabled; on Linux, rustix's `process` feature was already on through Tauri's stack. `cargo deny check` passes with the unchanged `deny.toml` |
| on merge (M4, slice 4d) | New workspace crate `oma-archive` (archive creation, locking, the hardened opener, schema v1, the fingerprint and the migration framework), at `opt-level = 3` in dev and test builds. It has a non-default `fault-injection` feature, turned on only by its own tests through a self dev-dependency | Its dependencies are crates already in the workspace: `oma-fs`, `oma-backup` (for `CancelToken`), `rusqlite` with the existing `bundled`, `limits` and `hooks` features, `sha2`, `thiserror`, `tracing` and `dunce`; `tempfile` and `tracing-subscriber` for tests. No new third-party crate: the only `Cargo.lock` change is the crate's own entry. `cargo deny check` passes with the unchanged `deny.toml` |
| on merge (M4, slice 4e) | `oma-archive` gains the dev-dependencies `oma-messages` (a workspace crate) and `serde_json` (already a workspace dependency) | The import catalogue test reads the content manifests and runs the M3 check, the key passes and staging over each entry. No new third-party crate: `Cargo.lock` changes only in `oma-archive`'s own dependency list. `cargo deny check` passes with the unchanged `deny.toml` |
| on merge (M4, slice 4e) | Dev-only Python tooling: `make_synthetic_imports.py` needs only the existing `requirements.txt` (`cryptography`) | The import catalogue. It imports the M2 and M3 generators as modules; no new Python package |
| on merge (M4, slice 4f) | `oma-archive` moves `oma-messages` and `serde_json` from dev-dependencies to dependencies, and adds `serde` and `zeroize` (all already workspace dependencies) | The import core reads sms.db through the `oma-messages` passes, stores `ImportCounts` as JSON, and zeroizes the source values a batch holds. No new third-party crate: `Cargo.lock` changes only in `oma-archive`'s own dependency list (`serde`, `zeroize` added). `cargo deny check` passes with the unchanged `deny.toml` |
| on merge (M4, slice 4l) | The shell depends on the workspace crate `oma-archive` | The archive commands and the import and verify jobs. No new third-party crate: the only `Cargo.lock` change is `oma-archive` added to the shell's own dependency list. No new npm package. `cargo deny check` passes with the unchanged `deny.toml` |
| on merge (M5, slice 5a) | Added image 0.25.10 (`=0.25.10`, default features off, features `jpeg`, `png`, `gif`, `webp`) as a workspace dependency and a dev-dependency of `oma-archive` | The viewer fixture generator (`examples/make_viewer_media`) and `tests/viewer_fixtures.rs`. Eleven new crates in `Cargo.lock` (`image`, `zune-jpeg`, `zune-core`, `gif`, `image-webp`, `weezl`, `moxcms`, `pxfm`, `color_quant`, `byteorder-lite`, `quick-error`; licenses above); `png` 0.18.1 and its deflate crates were already locked. It enters the shipped binary only when slice 5d makes it a dependency of the decode helper. `cargo deny check` passes with the unchanged `deny.toml` |
| on merge (M5, slice 5a) | Dev-only Python tooling: `make_synthetic_viewer.py` needs only the existing `requirements.txt` (`cryptography`) | The viewer catalogue. It imports the M2, M3 and M4 generators as modules; no new Python package |
| on merge (M5, slice 5d) | `image` 0.25.10 becomes a regular dependency of `oma-archive` (it was a dev-dependency); added `heic-rs` 0.1.4 (`=0.1.4`, default features off, `std` on) as a workspace dependency and an **optional** dependency of `oma-archive` behind the non-default `heic` feature | `media::render` decodes images (in the decode helper only). `heic-rs` is owner decision Q2: an opt-in HEIC decoder for self-builders, never in official or CI release builds (HEVC patents; `privacy.md`). One new crate in `Cargo.lock` (`heic-rs`, MIT OR Apache-2.0, no dependencies with this feature set). `cargo deny check` and `cargo deny --features oma-archive/heic check` pass with the unchanged `deny.toml` (whose `all-features = false` would otherwise not see an optional crate; CI runs both) |
| on merge (M5, slice 5f) | Added getrandom 0.3.4 (`=0.3.4`, default features) as a workspace dependency and a direct dependency of `src-tauri` | Media tokens for the `oma-media` protocol. No new crate in `Cargo.lock` (0.3.4 was already locked through Tauri); the only lockfile change is `getrandom` added to the shell's own dependency list. No new npm package. `cargo deny check` passes with the unchanged `deny.toml` |
| on merge (M5, slice 5g) | Added the npm package virtua 0.53.1 (exact pin, a runtime dependency) | Spike 5s(1): virtualised lists with prepend support (`shift`) for the thread feed (spec §8.4, §15, D109). MIT, no dependencies; Vue (≥ 3.2) and the other frameworks are optional peers, and only `virtua/vue` would be imported. Chosen by the spec's rule, the newest release at least 14 days old when the spike ran on 2026-10-19: 0.53.1 (2026-10-05; 0.53.2 to 0.53.5 were younger). Pre-1.0 with frequent releases, so the `VirtualFeed` wrapper keeps it swappable. **Nothing under `src/` imports it yet**, so it is not in the bundle. The spike on WebKitGTK 2.52.6 (`compatibility.md`) passed except for prepends at scrollTop 0; slice 5h1 decides between a mitigation and the in-house feed (5h0), and removes this package if 5h0 replaces it. `npm run licenses` passes (42 runtime packages) |
| on merge (M5, slice 5h1) | Removed the npm package virtua 0.53.1 | It failed spike 5s(1) on WebKitGTK 2.52.6 (prepends at scrollTop 0), so slice 5h0 replaced it with the in-house `VirtualFeed` (`src/components/viewer/VirtualFeed.vue` over `src/services/feedModel.ts`, written without reference to any library's code). It was never imported, so the bundle is unchanged by the removal. `package.json` and `package-lock.json` lose its entry and nothing else. `npm run licenses` passes (41 runtime packages) |
| on merge (M5, slice 5k) | No dependency change. Two ignored test files in `oma-archive` (`perf_viewer.rs`, `smoke_library.rs`) use only its existing dev-dependencies | The viewer's performance numbers and the smoke-test library. `Cargo.lock`, `package.json` and `package-lock.json` are unchanged; `cargo deny check` and `npm run licenses` (41 runtime packages) pass |
| on merge (M6, slice 6c) | Dev-only Python tooling: the export oracles' packages above, pinned with hashes in the new `scripts/oracles/requirements.txt` (jsonschema 4.26.0, jsonschema-specifications 2025.9.1, referencing 0.37.0, rpds-py 0.30.0, attrs 26.1.0, typing-extensions 4.16.0, html5lib 1.1, six 1.17.0, webencodings 0.5.1); `make_synthetic_exports.py` needs only the existing `requirements.txt` (`cryptography`) | The independent oracles of M6 spec §14.4, run in CI's Linux `checks` job (a new step) and by hand. All are permissive (MIT, PSF-2.0, BSD-3-Clause). Installing the file into a fresh Python 3.13 virtual environment and resolving it for CPython 3.10 on `manylinux_2_17_x86_64` with `pip download` both succeeded with hashes enforced; the CI run itself is **UNVERIFIED** until the PR's CI runs. No Rust crate and no npm package changed: `Cargo.lock`, `package.json` and `package-lock.json` are unchanged, and `deny.toml` is unchanged |
| on merge (M6, slice 6d) | No third-party dependency change. The new workspace crate `oma-export` (`crates/oma-export`, Apache-2.0 like the workspace) is added to `[workspace.dependencies]` and depends only on existing pins: `oma-archive`, `serde`, `serde_json` and `thiserror`; for its tests `oma-backup`, `tempfile`, `rusqlite` and `dunce` | The export record core (M6 spec §4). The only `Cargo.lock` change is the new `oma-export` package with those existing dependencies; no new crate is resolved. `deny.toml`, `package.json` and `package-lock.json` are unchanged. `cargo deny check` passes. The csv crate (=1.4.0) is not added yet: no writer exists until slice 6f |
