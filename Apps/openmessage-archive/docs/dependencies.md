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
| tempfile (dev only) | 3.27.0 | MIT OR Apache-2.0 | Test directories |
| aes | 0.9.3 | MIT OR Apache-2.0 | AES-256 for backup decryption (`oma-backup`); `zeroize` feature |
| aes-kw | 0.3.1 | MIT OR Apache-2.0 | RFC 3394 key unwrap for class, manifest and file keys; default features off (drops `const-oid`), `zeroize` on |
| cbc | 0.2.1 | MIT OR Apache-2.0 | AES-CBC decryption with strict PKCS#7 (`block-padding`, `zeroize`) |
| hmac | 0.13.0 | MIT OR Apache-2.0 | HMAC for the in-crate, cancellable PBKDF2 |
| sha1 / sha2 | 0.11.0 / 0.11.0 | MIT OR Apache-2.0 | HMAC-SHA1 and HMAC-SHA256 for PBKDF2; SHA-1 for fileIDs; SHA-256 for ids and staging names. Default features off, `zeroize` on |
| zeroize | 1.9.1 | Apache-2.0 OR MIT | Wiping passwords, keys and plaintext buffers |
| plist | 1.10.1 | MIT | Property lists, read only through the budgeted event-stream reader. Default features (`serde`) off; `enable_unstable_features_that_may_break_with_minor_version_bumps` on, because that feature is what makes `plist::stream` public. The exact pin makes any change to that API a reviewed upgrade |
| rusqlite | 0.40.2 | MIT | Read-only access to staged copies of Manifest.db and (M3) sms.db. Features `bundled` (compiles SQLite 3.53.2, public-domain C source, through libsqlite3-sys 0.38.2, MIT), `limits` (`Connection::set_limit`) and `hooks` (the authorizer and progress handler that guard staged sms.db connections). `limits` and `hooks` are empty features that add no crates. `cache` (statement cache) stays off, because it adds `hashlink` |
| pbkdf2 (dev only) | 0.13.0 | MIT OR Apache-2.0 | Differential tests against the in-crate PBKDF2; never linked into the app |

`tracing-subscriber` (above) is also a dev-dependency of `oma-backup`, for
its log-hygiene test. The shell (`src-tauri`) depends on the workspace crate
`oma-backup` from M2; that adds no third-party crate beyond the list above.
No JavaScript dependency was added in M2: `Channel` comes from
`@tauri-apps/api` 2.12.4, which was already a dependency.

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
`tests/fixtures/backups/`. Nothing here is shipped, and CI runs no Python:
the generated files are committed and checked by `SHA256SUMS`.

| Package | Version | License | Use |
|---|---|---|---|
| cryptography | 50.0.2 | Apache-2.0 OR BSD-3-Clause | AES, key wrap and padding in `scripts/fixtures/make_synthetic_backups.py` (pinned in `scripts/fixtures/requirements.txt`) |
| iphone_backup_decrypt | 0.11.2 | MIT | Independent oracle, run by hand through `scripts/fixtures/verify_with_oracle.py`. Used as a black box; not distributed and not ported. Pinned in `scripts/fixtures/requirements-oracle.txt` |
| pycryptodome | 4.0.1 | BSD-2-Clause AND Unlicense | The oracle's own dependency (`Requires-Dist: pycryptodome>=3.23.0`); it performs the oracle's AES. Pinned in `scripts/fixtures/requirements-oracle.txt` at the version the recorded oracle run used |

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
| heic (imazen) | AGPL-3.0 |

## Upgrade log

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
