<#
    ============================================================================
    ⚒️  VibeForge Systems — Windows Environment Bundle
    config-bundle/scripts/apply-shell.ps1

    Activates the shell components after install/config:
        • Reload ExplorerPatcher so its settings take effect
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
# EP installs ep_setup.dll under %ProgramFiles%\ExplorerPatcher. Re-running its
# setup entrypoint re-applies the imported settings without a reboot.
function Invoke-ExplorerPatcherReload {
    $epDll = Join-Path $env:ProgramFiles 'ExplorerPatcher\ep_setup.dll'
    if (-not (Test-Path $epDll)) {
        Write-Forge "ExplorerPatcher not found at $epDll; skipping reload." -Level Warn
        return
    }
    Write-Forge "Reloading ExplorerPatcher..." -Level Step
    if ($script:ForgeDryRun) { Write-Forge "[dry-run] would run rundll32 ep_setup.dll" -Level Skip; return }
    Start-Process -FilePath 'rundll32.exe' -ArgumentList "`"$epDll`",ZZGUI" -ErrorAction SilentlyContinue
    Write-Forge "ExplorerPatcher reload requested." -Level Ok
}

# --- Open-Shell --------------------------------------------------------------
# Open-Shell reads its config from the registry; the shipped XML is imported
# explicitly through ClassicStartMenu.exe -xml. This is the honest path — there
# is no magic ProgramData file that Open-Shell picks up on its own.
function Import-OpenShellSettings {
    param([string]$XmlPath)

    $exe = Join-Path $env:ProgramFiles 'Open-Shell\ClassicStartMenu.exe'
    if (-not (Test-Path $exe)) {
        Write-Forge "Open-Shell not found at $exe; skipping settings import." -Level Warn
        return
    }
    if (-not (Test-Path $XmlPath)) {
        Write-Forge "Open-Shell settings XML not found at $XmlPath; using component defaults." -Level Warn
        return
    }
    Write-Forge "Importing Open-Shell settings..." -Level Step
    if ($script:ForgeDryRun) { Write-Forge "[dry-run] would import $XmlPath via ClassicStartMenu.exe -xml" -Level Skip; return }
    Start-Process -FilePath $exe -ArgumentList "-xml `"$XmlPath`"" -ErrorAction SilentlyContinue
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

Invoke-ExplorerPatcherReload
Import-OpenShellSettings -XmlPath $osXml
Start-TaskbarX

Write-Forge "Shell configuration applied." -Level Ok
