<#
    ============================================================================
    ⚒️  VibeForge Systems — Windows Environment Bundle
    config-bundle/scripts/apply-shell.ps1

    Activates the shell components after install/config:
        • Confirm the ExplorerPatcher profile (it applies on Explorer restart)
        • Import Open-Shell settings explicitly (they are NOT auto-read)
        • Launch TaskbarX if it is present (optional component)

    Minimal. Deterministic. Each step is guarded so a missing optional
    component warns instead of aborting the run.

    Part of the VibeForge "From the Forge" collection.
    Licensed under the GNU Affero General Public License v3.0 (AGPL-3.0).
    ============================================================================
#>

[CmdletBinding(SupportsShouldProcess)]
param(
    [string]$BundleRoot   = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
    [string]$ManifestPath = (Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path 'bundle.psd1'),
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$script:ForgeDryRun = [bool]$DryRun

. (Join-Path $PSScriptRoot 'lib\common.ps1')

# --- ExplorerPatcher ---------------------------------------------------------
# EP's core module (dxgi.dll) is loaded into Explorer at startup and reads
# HKCU\Software\ExplorerPatcher then. So EP settings take effect on the Explorer
# restart setup.ps1 performs at the end of the run - there is no separate rundll
# reload to invoke (ep_setup.dll is the installer, not a reload entrypoint).
function Confirm-ExplorerPatcherProfile {
    if (Test-Path 'HKCU:\Software\ExplorerPatcher') {
        Write-Forge "ExplorerPatcher profile present; applies on the Explorer restart." -Level Ok
    }
    else {
        Write-Forge "ExplorerPatcher settings key not found; is EP installed?" -Level Warn
    }
}

# --- Open-Shell --------------------------------------------------------------
# Open-Shell reads its config from the registry; the shipped XML is imported
# explicitly through StartMenu.exe -xml (a silent, non-interactive full-replace
# import). This is the honest path — there is no ProgramData file Open-Shell
# picks up on its own.
function Import-OpenShellSettings {
    param([string]$XmlPath)

    # Open-Shell's binary is StartMenu.exe. ClassicStartMenu.exe was the old,
    # discontinued Classic Shell name and is not present in an Open-Shell install.
    $exe = Join-Path $env:ProgramFiles 'Open-Shell\StartMenu.exe'
    if (-not (Test-Path $exe)) {
        Write-Forge "Open-Shell (StartMenu.exe) not found at $exe; skipping settings import." -Level Warn
        return
    }
    if (-not (Test-Path $XmlPath)) {
        Write-Forge "Open-Shell settings XML not found at $XmlPath; using component defaults." -Level Warn
        return
    }
    # StartMenu.exe -xml tokenizes the path at the first space and does NOT strip
    # quotes, so a path containing a space would be silently truncated. Guard
    # instead of importing a corrupt path.
    if ($XmlPath -match '\s') {
        Write-Forge "Open-Shell XML path contains a space ($XmlPath); StartMenu.exe would truncate it. Skipping import — stage the XML at a space-free path." -Level Warn
        return
    }
    Write-Forge "Importing Open-Shell settings..." -Level Step
    if ($script:ForgeDryRun) { Write-Forge "[dry-run] would import $XmlPath via StartMenu.exe -xml" -Level Skip; return }
    # Path is space-free (guarded above), so pass it unquoted; -xml imports silently.
    Start-Process -FilePath $exe -ArgumentList "-xml $XmlPath" -ErrorAction SilentlyContinue
    Write-Forge "Open-Shell settings imported." -Level Ok
}

# --- TaskbarX (optional) -----------------------------------------------------
function Start-TaskbarX {
    $candidates = @(
        (Join-Path $env:LOCALAPPDATA 'TaskbarX\TaskbarX.exe'),
        (Join-Path $env:ProgramFiles 'TaskbarX\TaskbarX.exe')
    )
    $exe = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $exe) {
        Write-Forge "TaskbarX not found (optional); skipping." -Level Skip
        return
    }
    Write-Forge "Starting TaskbarX..." -Level Step
    if ($script:ForgeDryRun) { Write-Forge "[dry-run] would start $exe" -Level Skip; return }
    Start-Process -FilePath $exe -ErrorAction SilentlyContinue
    Write-Forge "TaskbarX started." -Level Ok
}

# --- Orchestrate -------------------------------------------------------------
$manifest = Import-PowerShellDataFile -Path $ManifestPath

# Resolve the Open-Shell XML destination from the manifest's ConfigMap.
$osEntry = $manifest.ConfigMap | Where-Object { $_.Name -eq 'Open-Shell settings' } | Select-Object -First 1
$osXml   = if ($osEntry -and $osEntry.ContainsKey('Destination')) { Expand-ForgePath $osEntry.Destination } else { '' }

Confirm-ExplorerPatcherProfile
Import-OpenShellSettings -XmlPath $osXml
Start-TaskbarX

Write-Forge "Shell configuration applied. ExplorerPatcher and registry changes take effect on the next Explorer restart — setup.ps1 does this automatically; if you ran this script standalone, restart Explorer." -Level Ok
