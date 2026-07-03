# ⚒️ Platform Profile — VibeForge Windows Workstation

> An internal platform spec, VibeForge style. This is the contract the bundle
> fulfills: what a "VibeForge workstation" *is*, what it guarantees, and how the
> profile is versioned and evolved. The runnable source of truth is
> [`bundle.psd1`](../bundle.psd1) — this doc explains it.

---

## 🪪 Profile identity

| Field         | Value                                                              |
| ------------- | ----------------------------------------------------------------- |
| **Profile**   | `vibeforge-windows-env-bundle`                                     |
| **Version**   | `1.0.0` (keep in sync with the `winenv-v*` release tag)           |
| **Target OS** | Windows 11, build ≥ 22000                                          |
| **Owner**     | VibeForge Systems — Platform                                       |
| **Intent**    | Reproducible baseline for regulated enterprise dev workstations   |
| **License**   | AGPL-3.0                                                           |

---

## ✅ What the profile guarantees

Applying the profile lands a fresh Windows 11 machine in a **deterministic
end-state**:

1. **A Windows 10-style shell** — classic taskbar, Start menu, ribbon Explorer,
   and the full right-click context menu (via ExplorerPatcher + Open-Shell).
2. **A minimum-viable dev toolset** — editor, browser, VCS, archiver, text editor.
3. **A reversible registry + settings baseline** — every change is documented
   with an inline revert path; nothing is a one-way door.
4. **An audit trail** — each real run is transcribed to
   `%ProgramData%\VibeForge\logs\`.

Re-applying the profile is **idempotent**: installed packages are skipped,
registry imports are safe to repeat.

---

## 📋 Profile contents

Sourced from [`bundle.psd1`](../bundle.psd1). Edit the manifest, not this table.

### Shell components (`ShellComponents`)

| Component       | Source            | Package id                   | Optional |
| --------------- | ----------------- | ---------------------------- | -------- |
| ExplorerPatcher | winget            | `valinet.ExplorerPatcher`    | no       |
| Open-Shell      | winget            | `Open-Shell.Open-Shell-Menu` | no       |
| TaskbarX        | msstore + zip fb. | `9PF3RHHHRN95`               | **yes**  |

### Baseline apps (`BaselineApps`)

| App                | Package id                  |
| ------------------ | --------------------------- |
| Visual Studio Code | `Microsoft.VisualStudioCode`|
| Google Chrome      | `Google.Chrome`             |
| Git                | `Git.Git`                   |
| 7-Zip              | `7zip.7zip`                 |
| Notepad++          | `Notepad++.Notepad++`       |

### Config artifacts (`ConfigMap`)

| Artifact                  | Method       | Applied by                          |
| ------------------------- | ------------ | ----------------------------------- |
| `explorerpatcher.reg`     | `reg-import` | `setup.ps1` (Step 2)                |
| `desktop.reg`             | `reg-import` | `setup.ps1` (Step 2)                |
| `explorer.reg`            | `reg-import` | `setup.ps1` (Step 2)                |
| `openshell.xml`           | `copy`       | `apply-shell.ps1` (`-xml` import)   |

---

## 🔢 Versioning conventions

The profile uses **SemVer** under the scoped `winenv-v*.*.*` tag namespace, so it
versions independently of everything else in `From_the_Forge`. Bump semantics are
**profile-centric**, not code-centric:

| Bump      | Meaning for the profile                                                            |
| --------- | --------------------------------------------------------------------------------- |
| **MAJOR** | A component is removed/replaced, or a registry change becomes non-reversible or behavior-breaking for existing workstations. |
| **MINOR** | A component or baseline app is **added**, or a new opt-in capability lands. Existing state stays valid. |
| **PATCH** | A pinned version, checksum, doc, or annotation changes. No behavioral shift.       |

**Golden rule:** the `Version` field in `bundle.psd1` and the `winenv-v*` git tag
must match. The tag is what ships; the manifest is what runs — they describe the
same release.

---

## 🔧 Change management — evolving the profile

The profile is a data change, not a code change. To ship a new revision:

1. **Edit [`bundle.psd1`](../bundle.psd1)** — add/remove a component or app, or
   adjust the `ConfigMap`. Scripts stay generic.
2. **Re-export shell settings from a reference box** — tune Open-Shell on a
   clean machine, `Backup → Save to XML`, and replace `openshell.xml`. Same for
   any ExplorerPatcher tweaks (re-export the `HKCU\Software\ExplorerPatcher` key).
3. **Pin binaries** — if enabling the TaskbarX zip fallback, set a real `Sha256`
   (the installer refuses to run without it).
4. **Bump the version** in the manifest per the table above.
5. **Tag & push** — `git tag winenv-vX.Y.Z && git push origin winenv-vX.Y.Z`.
   The pipeline packages and publishes the checksummed bundle.

Always validate with `.\setup.ps1 -DryRun` on a Windows 11 host before tagging.

---

## 🛡️ Compliance & safety posture

- **Third-party shell mods.** ExplorerPatcher and Open-Shell hook the Windows
  shell. On managed/regulated fleets, confirm they are permitted by endpoint and
  security baselines **before** rollout.
- **Reversible by design.** Every `.reg` file documents its revert path; both
  shell tools ship "restore defaults / reset all settings".
- **Verified supply chain.** Core components come from winget with exact ids; the
  optional TaskbarX zip path enforces SHA256 verification.
- **Auditable.** Every real run leaves a transcript under
  `%ProgramData%\VibeForge\logs\`.
- **Least surprise.** `-DryRun` previews the full plan; `-SkipApps` narrows scope
  to the shell profile only.

---

## 🧭 Forward hooks (documented, not built)

Natural next steps if this profile graduates into the broader platform:

- **OBTO agent manifest** — expose the profile as an agent-invokable capability
  (apply / dry-run / report) so workstation provisioning fits the OBTO
  multi-agent workflow.
- **Fleet rollout** — wrap `setup.ps1` in an Intune/SCCM package or a scheduled
  remediation, feeding the transcript into central logging.
- **Profile variants** — layer additional manifests (e.g. `bundle.security.psd1`)
  merged over the base for role-specific workstations.

---

*Part of the VibeForge [From the Forge](../../README.md) collection. See the
[Architecture](./ARCHITECTURE.md) doc for how the pieces execute.*
