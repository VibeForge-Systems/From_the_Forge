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
| Ubuntu 26.04.1 LTS x64 | Launches (built from source with `npm run tauri dev` and opened on a real desktop, a QEMU VM; reported 2026-10-17) | Planned | Planned | First run on a real desktop. The folder picker and persistence across restarts have not been reported yet. Ships libimobiledevice 1.4.0. |
| Debian 12, Fedora 40+ | Untested | Planned | Planned | Need WebKitGTK 4.1 |
| macOS | Unsupported for now | — | — | The architecture keeps platform code behind interfaces so it can be added later |

"Supported (built and unit-tested)" on Ubuntu 24.04 means the release build,
all unit tests, and the `.deb` bundle succeeded.

"Launches" on Ubuntu 26.04.1 means a development build opened on a real
desktop. That is the first manual run anywhere. It is not yet a full
interactive test, which still needs:

- choosing a folder through the native picker;
- confirming that a folder inside a backup is refused;
- confirming the choice survives a restart.

Those three steps are part of M1 sign-off. Windows 11 still has no manual
run at all.

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
