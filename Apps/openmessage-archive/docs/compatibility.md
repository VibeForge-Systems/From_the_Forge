# Compatibility matrix

This file records **what has been tested**, not what is expected to work.
Research findings and expectations are in
[supported-formats.md](supported-formats.md). A cell only changes to
"Supported" after a test proves it, whether automated, against a synthetic
fixture, or a recorded manual test on real hardware.

Legend: **Supported**: tested and working. **CI**: built and unit-tested in
CI, not exercised by hand. **Untested**: expected to work but not yet
verified. **Planned**: not implemented. **Unsupported**: deliberately out of
scope, and detected and reported when encountered.

Last updated: Milestone 1.

## Operating systems

| Platform | Shell and settings (M1) | Import backup (M2) | Direct device backup (M7) | Notes |
|---|---|---|---|---|
| Windows 11 x64 | CI | Planned | Planned | Needs WebView2, which ships with Windows 11. Direct backup will need Apple's Mobile Device Service (Apple Devices app or iTunes). |
| Windows 11 ARM64 | Untested | Planned | Planned | Not built in CI yet |
| Ubuntu 22.04 x64 | CI | Planned | Planned | CI build baseline, which sets the glibc floor. Needs `libwebkit2gtk-4.1-0`. |
| Ubuntu 24.04 x64 | Supported (built and unit-tested during M1 development) | Planned | Planned | Ships libimobiledevice 1.3.0, which is old. M7 will not depend on it. |
| Ubuntu 26.04.1 LTS x64 | **Supported** (built from source with `npm run tauri dev`, every M1 check done by hand on a real desktop, a QEMU VM; 2026-10-17/18) | Planned | Planned | First manual run anywhere. The native picker chose an ordinary folder. A folder laid out like a backup (`Manifest.db` + `Info.plist`) was refused with the reason shown, and the previous location was kept. The choice survived a restart. Ships libimobiledevice 1.4.0. |
| Debian 12, Fedora 40+ | Untested | Planned | Planned | Need WebKitGTK 4.1 |
| macOS | Unsupported for now | — | — | The architecture keeps platform code behind interfaces so it can be added later |

"Supported (built and unit-tested)" on Ubuntu 24.04 means the release build,
all unit tests, and the `.deb` bundle succeeded.

On Ubuntu 26.04.1, a development build ran on a real desktop and passed
every M1 manual check:

- **The native GTK picker works.** An ordinary folder was chosen through
  it. This is the step that could not be completed headlessly.
- **Backup folders are refused.** Picking a folder that looks like an iPhone
  backup was rejected with "This folder is inside an iPhone backup (…)",
  and the previously saved location was kept.
- **The setting persists.** After the app was closed and relaunched, the
  chosen folder was still set and still showed **Ready**.

That completes M1's manual sign-off on Linux. The release `.deb`, `.rpm`
and AppImage packages have been built by CI, but nobody has installed and
run them by hand yet. Windows 11 still has no manual run at all.

## iOS versions and backup types

All rows are **Planned** until M2 and M3 land. The plan, based on
[supported-formats.md](supported-formats.md):

| iOS | Backup format | Unencrypted import | Encrypted import | Messages extraction |
|---|---|---|---|---|
| ≤ 9 | 2.4 (`Manifest.mbdb`) | Unsupported (detected and reported) | Unsupported | — |
| 10 – 16 | 3.x (`Manifest.db`) | Planned (M2) | Planned (M2) | Planned (M3) |
| 17 – 18 | 3.x | Planned (M2) | Planned (M2) | Planned (M3); RCS service from iOS 18 |
| 26 – 27 | 3.x | Planned (M2) | Planned (M2) | Planned (M3); schema to be verified against real data |

## Backup sources

| Source | Status |
|---|---|
| Finder / iTunes / Apple Devices backup folder (local) | Planned (M2) |
| Backup created by `idevicebackup2` or pymobiledevice3 | Planned (M2), same format |
| iCloud backup | Unsupported: it is not stored locally, and fetching it would need a network service |
| Partial or in-progress backup (`SnapshotState` ≠ `finished`) | Planned: detected and reported, never imported as complete |
