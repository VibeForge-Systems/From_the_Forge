# ⚒️ Windows Environment Bundle — VibeForge Workstation Profile

A reproducible Windows 10-style shell + baseline dev environment for **Windows 11**
workstations. One entrypoint, one manifest, no mystery meat.

> **Working principle:** configs → scripts → orchestrator → pipeline.
> Modular, annotated, deterministic, contamination-free. Forge for clarity.

---

## 🧭 What this is

A productized internal-tooling bundle that takes a fresh Windows 11 machine and
makes it **VibeForge-consistent** in about a minute:

- Restores the **Windows 10-style shell** on Windows 11 (classic taskbar, Start
  menu, ribbon Explorer, full right-click context menu) via **ExplorerPatcher**
  and **Open-Shell**, with **TaskbarX** as an optional centered-icons touch.
- Installs a **minimum-viable-workstation** app set (VS Code, Chrome, Git,
  7-Zip, Notepad++).
- Applies an annotated, **reversible** registry + settings baseline.

Everything is driven from a single manifest (`bundle.psd1`) and a single
entrypoint (`setup.ps1`).

---

## 📂 Repository layout

```
windows-env-bundle/
├── README.md                       # this file
├── setup.ps1                       # single entrypoint orchestrator
├── bundle.psd1                     # manifest — what gets installed & applied
└── config-bundle/
    ├── shell/
    │   ├── explorerpatcher.reg     # Win11 → Win10 shell, documented + reversible
    │   └── openshell.xml           # classic Start menu profile (imported explicitly)
    ├── registry/
    │   ├── desktop.reg             # desktop / taskbar UX baselines
    │   └── explorer.reg            # File Explorer + classic context menu
    └── scripts/
        ├── apply-shell.ps1         # reload EP, import Open-Shell, start TaskbarX
        ├── baseline-apps.ps1       # manifest-driven app install
        └── lib/
            └── common.ps1          # shared helpers (guards, logging, install/import)
```

The release workflow lives at the **repo root** (`.github/workflows/`), not
here — GitHub only runs workflows from the root.

---

## 🚀 Quickstart

Run from an **elevated** PowerShell (Run as administrator):

```powershell
cd windows-env-bundle
.\setup.ps1
```

Useful switches:

| Switch        | Effect                                                        |
| ------------- | ------------------------------------------------------------ |
| `-DryRun`     | Log every action, change nothing. **Run this first.**        |
| `-SkipApps`   | Apply the shell profile only; skip baseline apps.            |
| `-Force`      | Bypass the Windows 11 guard (advanced; testing on VM images).|

Every run writes a transcript to `%ProgramData%\VibeForge\logs\`.

---

## 🧩 Manifest-driven model

Editing the bundle means editing **`bundle.psd1`** — never the scripts. It is a
native PowerShell data file (read with `Import-PowerShellDataFile`, so no code
executes on load). It defines three lists:

- **`ShellComponents`** — the shell profile, installed first.
- **`BaselineApps`** — the minimum-viable workstation apps.
- **`ConfigMap`** — how each `.reg` / `.xml` artifact is applied.

Each install routes through one primitive (`Install-Package` in `common.ps1`)
that is idempotent, dry-run aware, and source-routed:

| Source                | Used for                                               |
| --------------------- | ------------------------------------------------------ |
| `winget`              | ExplorerPatcher, Open-Shell, all baseline apps         |
| `msstore`             | TaskbarX (it is **not** in the winget community repo)  |
| pinned zip + SHA256   | TaskbarX fallback — checksum verification is mandatory |

---

## 🛠️ Components & the Windows 11 rationale

| Component           | Source          | Why (on Windows 11)                                                            |
| ------------------- | --------------- | ----------------------------------------------------------------------------- |
| **ExplorerPatcher** | `winget` (`valinet.ExplorerPatcher`) | Restores the Win10 taskbar, Start behavior, Alt+Tab, and classic flyouts. |
| **Open-Shell**      | `winget` (`Open-Shell.Open-Shell-Menu`) | Brings back a real Start menu. Settings are imported **explicitly** via `StartMenu.exe -xml`. |
| **TaskbarX** *(optional)* | Microsoft Store (`msstore`) + zip fallback | Centers taskbar icons. Persists via Task Scheduler / CLI — **not** a JSON config. |

> **Honest note on TaskbarX:** earlier guides (and the draft this bundle grew
> from) assumed a `taskbarx.json` settings file. There isn't one. TaskbarX is
> optional here and configured through its own configurator + a logon task.

---

## 🛡️ Safety notes for regulated / managed workstations

These are **third-party shell modifications**. On managed or regulated fleets:

- **Check your org's policy first.** ExplorerPatcher and Open-Shell hook the
  Windows shell; some endpoint-management or security baselines disallow them.
- **It's reversible.** Every `.reg` file documents how to revert inline, and
  both shell tools ship "restore defaults / reset all settings" options.
- **Determinism by design.** Re-running `setup.ps1` is safe — already-installed
  packages are skipped, and registry imports are idempotent.
- **Pin before you trust.** The TaskbarX zip fallback refuses to run without a
  verified `SHA256` set in the manifest.
- **Audit trail.** Each run is transcribed to `%ProgramData%\VibeForge\logs\`.

---

## 📦 Release & versioning

The bundle versions **independently** of the rest of the `From_the_Forge`
collection using a scoped tag prefix:

```bash
git tag winenv-v1.0.0
git push origin winenv-v1.0.0
```

That triggers `.github/workflows/release-winenv-bundle.yml`, which packages
`windows-env-bundle/` into a checksummed `windows-env-bundle.zip` +
`SHA256SUMS.txt` and publishes a GitHub Release. You can also run it on demand
from the **Actions** tab (`workflow_dispatch`).

---

## ✅ Pre-flight checklist

- [ ] Running PowerShell **as administrator**
- [ ] Target is **Windows 11** (build ≥ 22000)
- [ ] `winget` present (`App Installer` from the Microsoft Store)
- [ ] Ran `.\setup.ps1 -DryRun` and reviewed the planned actions
- [ ] (Optional) Set a real `SHA256` for TaskbarX in `bundle.psd1`

---

## 📚 Docs

- [**Architecture**](./docs/ARCHITECTURE.md) — layered component view, runtime
  flow, the install-decision primitive, release pipeline, and the trust boundary
  (Mermaid diagrams, render natively on GitHub).
- [**Platform Profile**](./docs/PLATFORM-PROFILE.md) — the internal spec: what a
  VibeForge workstation guarantees, profile contents, versioning conventions, and
  change management.

---

## 📄 License

Part of the VibeForge **From the Forge** collection. Licensed under the
**GNU Affero General Public License v3.0 (AGPL-3.0)** — see the repository
[`LICENSE`](../LICENSE).
