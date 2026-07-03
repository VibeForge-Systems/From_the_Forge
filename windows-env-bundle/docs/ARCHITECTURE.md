# ⚒️ Architecture — VibeForge Windows Environment Bundle

> **Working principle:** configs → scripts → orchestrator → pipeline.
> One manifest is the source of truth; generic scripts execute it; guarded
> primitives touch the machine; a release pipeline packages the whole thing.

This document is the map. It shows how the pieces fit, how a run flows, and
where external (untrusted) bytes enter so the guardrails are visible.

---

## 🧱 Layered component view

Everything is grouped by **concern**, not by tool. Editing the bundle means
editing the manifest — the layers below it stay generic.

```mermaid
flowchart TD
    subgraph L0["Manifest layer — the source of truth"]
        M["bundle.psd1<br/>ShellComponents · BaselineApps · ConfigMap"]
    end

    subgraph L1["Orchestration layer"]
        S["setup.ps1<br/>single entrypoint"]
        BA["baseline-apps.ps1"]
        AS["apply-shell.ps1"]
    end

    subgraph L2["Shared library — guarded primitives"]
        C["config-bundle/scripts/lib/common.ps1"]
    end

    subgraph L3["Config artifacts — reversible, annotated"]
        R1["explorerpatcher.reg"]
        R2["desktop.reg / explorer.reg"]
        X1["openshell.xml"]
    end

    subgraph L4["External package sources"]
        WG["winget repo"]
        MS["Microsoft Store"]
        ZP["pinned zip + SHA256"]
    end

    M --> S
    S --> BA
    S --> AS
    S -. dot-source .-> C
    BA -. dot-source .-> C
    AS -. dot-source .-> C
    C --> R1 & R2 & X1
    C --> WG & MS & ZP
```

---

## 🔁 Runtime flow (`setup.ps1`)

The orchestrator runs five steps behind three preflight guards. `-DryRun`
short-circuits every mutation; a transcript is written for every real run.

```mermaid
flowchart TD
    Start(["setup.ps1 (elevated)"]) --> DR{"-DryRun?"}
    DR -- yes --> Log["log actions only<br/>change nothing"]
    DR -- no --> TR["Start-Transcript →<br/>%ProgramData%\VibeForge\logs"]

    TR --> G1["Assert-Administrator"]
    Log --> G1
    G1 --> G2["Assert-Windows11<br/>(build ≥ 22000)"]
    G2 --> G3["Test-WinGet"]
    G3 --> LM["Import-PowerShellDataFile<br/>bundle.psd1"]

    LM --> S1["Step 1 — install ShellComponents"]
    S1 --> S2["Step 2 — apply ConfigMap<br/>(reg-import / copy)"]
    S2 --> SK{"-SkipApps?"}
    SK -- no --> S3["Step 3 — baseline-apps.ps1"]
    SK -- yes --> S4
    S3 --> S4["Step 4 — apply-shell.ps1"]
    S4 --> S5["Step 5 — restart Explorer<br/>(stop AND ensure start)"]
    S5 --> Done(["Workstation ready"])
```

> Any step that throws is caught by the top-level `try/catch`, logged via
> `Write-Forge -Level Error`, and re-thrown so the run fails loud with the
> transcript intact.

---

## 📦 The install primitive (`Install-Package`)

Every package — shell component or baseline app — routes through one function
in `common.ps1`. It is idempotent, dry-run aware, source-routed, and refuses to
run an unverified binary.

```mermaid
flowchart TD
    IP(["Install-Package"]) --> D{"-DryRun?"}
    D -- yes --> Skip1["log intent, return"]
    D -- no --> Inst{"already installed?<br/>(Test-PackageInstalled)"}
    Inst -- yes --> Skip2["skip, return"]
    Inst -- no --> Src{"Source?"}

    Src -- winget --> WG["winget install --exact"]
    Src -- msstore --> MS["winget install --source msstore"]

    WG --> OK{"exit 0?"}
    MS --> OK
    OK -- yes --> Done(["installed"])
    OK -- no --> FB{"zip fallback<br/>configured?"}

    FB -- yes --> Z["Install-FromZip"]
    Z --> H{"SHA256 pinned<br/>& matches?"}
    H -- no --> Stop["throw — refuse unverified binary"]
    H -- yes --> Done

    FB -- no --> Opt{"-Optional?"}
    Opt -- yes --> Warn["warn, continue"]
    Opt -- no --> Throw["throw — fail loud"]
```

---

## 🚚 Release pipeline

Lives at the **repo root** (`.github/workflows/`) because GitHub only runs
workflows from there. Versioned independently of the rest of the collection via
a scoped tag prefix.

```mermaid
flowchart LR
    T["git tag winenv-v*.*.*<br/>(or workflow_dispatch)"] --> CO["actions/checkout@v4"]
    CO --> ZP["Compress-Archive<br/>windows-env-bundle/*"]
    ZP --> SH["Get-FileHash → SHA256SUMS.txt"]
    SH --> REL["action-gh-release@v2<br/>publish zip + checksum"]
```

---

## 🧩 `common.ps1` function reference

| Function                | Role                                                                 |
| ----------------------- | ------------------------------------------------------------------- |
| `Write-Forge`           | Branded, leveled logging (`>> VibeForge:`); feeds the transcript.    |
| `Assert-Administrator`  | Fail fast if not elevated (winget + HKLM writes need admin).         |
| `Assert-Windows11`      | Guard on build ≥ 22000; `-Force` overrides for VM/test images.       |
| `Test-WinGet`           | Verify winget exists; clear remediation if missing.                 |
| `Test-PackageInstalled` | Best-effort idempotency check via `winget list`.                    |
| `Install-Package`       | The one installer primitive (see decision flow above).              |
| `Install-FromZip`       | Pinned download + **mandatory** SHA256 verify + extract.            |
| `Import-RegFile`        | Guarded, logged, dry-run-aware `reg import`.                        |
| `Copy-BundleFile`       | Guarded copy that creates the destination tree.                    |
| `Expand-ForgePath`      | Expand `%ENV%` tokens used in manifest destinations.               |

---

## 🛡️ Trust boundary

External bytes enter at exactly three points — all in **L4**:

| Entry point       | What crosses the boundary        | Guardrail                                   |
| ----------------- | -------------------------------- | ------------------------------------------- |
| winget repo       | Signed package installs          | Exact-id match, `--exact`, exit-code check  |
| Microsoft Store   | TaskbarX (optional)              | `msstore` source, `-Optional` fail-soft     |
| pinned zip        | TaskbarX fallback binary         | **Mandatory SHA256**; refuses if unset/mismatch |

Cross-cutting guarantees: **idempotency** (safe re-runs), **`-DryRun`** (preview
without mutation), and a **transcript** (`%ProgramData%\VibeForge\logs\`) for
every real run — the audit trail.

---

*Part of the VibeForge [From the Forge](../../README.md) collection. Licensed
under the GNU Affero General Public License v3.0 — see [`LICENSE`](../../LICENSE).*
