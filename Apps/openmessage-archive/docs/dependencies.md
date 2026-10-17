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
| StackOverflow answer 13793043 (backup decryption) | CC BY-SA 4.0 |
| heic (imazen) | AGPL-3.0 |

## Upgrade log

| Date | Change | Reason |
|---|---|---|
| 2026-10-17 | Initial set (Milestone 1) | — |
| 2026-10-17 | MPL-2.0 exception for five Tauri transitive crates | Found by the first `cargo deny` run; see above |
| 2026-10-17 | Project moved to `Apps/openmessage-archive/` | Repository layout; no dependency change |
