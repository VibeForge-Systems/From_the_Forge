<#
    ============================================================================
    ⚒️  VibeForge Systems — Workstation Bootstrapper
    setup.ps1 — single entrypoint for the Windows Environment Bundle.

    Deterministic, reproducible, contamination-free. Reads bundle.psd1 for
    everything it installs and applies, then orchestrates the steps:

        components -> config -> baseline apps -> shell apply -> restart shell

    USAGE (run elevated):
        .\setup.ps1                 # full run
        .\setup.ps1 -DryRun         # log every action, change nothing
        .\setup.ps1 -SkipApps       # shell only, skip baseline apps
        .\setup.ps1 -Force          # bypass the Windows 11 guard (advanced)

    Part of the VibeForge "From the Forge" collection.
    Licensed under the GNU Affero General Public License v3.0 (AGPL-3.0).
    ============================================================================
#>

[CmdletBinding(SupportsShouldProcess)]
param(
    [string]$BundleRoot   = $PSScriptRoot,
    [string]$ManifestPath = (Join-Path $PSScriptRoot 'bundle.psd1'),
    [switch]$SkipApps,
    [switch]$DryRun,
    [switch]$Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Make the dry-run flag visible to the shared library before we load it.
$script:ForgeDryRun = [bool]$DryRun

# --- Load the shared library -------------------------------------------------
$libPath = Join-Path $BundleRoot 'config-bundle\scripts\lib\common.ps1'
if (-not (Test-Path $libPath)) {
    throw "Could not find common.ps1 at $libPath. Is -BundleRoot correct?"
}
. $libPath

# --- Start a transcript so a run is auditable --------------------------------
$logDir = Join-Path $env:PROGRAMDATA 'VibeForge\logs'
if (-not $DryRun) {
    if (-not (Test-Path $logDir)) { New-Item -ItemType Directory -Path $logDir -Force | Out-Null }
    $logFile = Join-Path $logDir ("setup-{0}.log" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))
    try { Start-Transcript -Path $logFile -Append | Out-Null } catch { }
}

Write-Forge "================================================================" -Level Info
Write-Forge "VibeForge Windows Environment Bundle — bootstrapping" -Level Step
if ($DryRun) { Write-Forge "DRY RUN: no changes will be made." -Level Warn }
Write-Forge "================================================================" -Level Info

try {
    # --- Preflight guards ----------------------------------------------------
    Assert-Administrator
    Assert-Windows11 -Force:$Force
    Test-WinGet | Out-Null

    # --- Load the manifest ---------------------------------------------------
    if (-not (Test-Path $ManifestPath)) { throw "Manifest not found: $ManifestPath" }
    $manifest = Import-PowerShellDataFile -Path $ManifestPath
    Write-Forge "Loaded manifest '$($manifest.Name)' v$($manifest.Version)." -Level Ok

    # --- 1. Shell components -------------------------------------------------
    Write-Forge "Step 1/5 — installing shell components" -Level Step
    foreach ($c in $manifest.ShellComponents) {
        $splat = @{
            Name     = $c.Name
            Source   = $c.Source
            Optional = [bool]($c.ContainsKey('Optional') -and $c.Optional)
        }
        if ($c.ContainsKey('Id'))          { $splat.Id          = $c.Id }
        if ($c.ContainsKey('DownloadUrl')) { $splat.DownloadUrl = $c.DownloadUrl }
        if ($c.ContainsKey('Sha256'))      { $splat.Sha256      = $c.Sha256 }
        if ($c.ContainsKey('ExtractTo'))   { $splat.ExtractTo   = Expand-ForgePath $c.ExtractTo }
        Install-Package @splat
    }

    # --- 2. Apply configuration bundle ---------------------------------------
    Write-Forge "Step 2/5 — applying configuration bundle" -Level Step
    foreach ($map in $manifest.ConfigMap) {
        $src = Join-Path $BundleRoot $map.Source
        switch ($map.Method) {
            'reg-import' { Import-RegFile -Path $src }
            'copy'       { Copy-BundleFile -Source $src -Destination (Expand-ForgePath $map.Destination) }
            default      { Write-Forge "Unknown ConfigMap method '$($map.Method)' for $($map.Name); skipping." -Level Warn }
        }
    }

    # --- 3. Baseline apps ----------------------------------------------------
    if ($SkipApps) {
        Write-Forge "Step 3/5 — baseline apps SKIPPED (-SkipApps)." -Level Skip
    }
    else {
        Write-Forge "Step 3/5 — installing baseline apps" -Level Step
        & (Join-Path $BundleRoot 'config-bundle\scripts\baseline-apps.ps1') `
            -ManifestPath $ManifestPath -BundleRoot $BundleRoot -DryRun:$DryRun
    }

    # --- 4. Apply shell configuration ----------------------------------------
    Write-Forge "Step 4/5 — applying shell configuration" -Level Step
    & (Join-Path $BundleRoot 'config-bundle\scripts\apply-shell.ps1') `
        -ManifestPath $ManifestPath -BundleRoot $BundleRoot -DryRun:$DryRun

    # --- 5. Restart Explorer (stop AND ensure start) -------------------------
    Write-Forge "Step 5/5 — restarting Explorer" -Level Step
    if ($DryRun) {
        Write-Forge "[dry-run] would restart explorer.exe" -Level Skip
    }
    else {
        Stop-Process -Name explorer -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 2
        if (-not (Get-Process -Name explorer -ErrorAction SilentlyContinue)) {
            Start-Process explorer.exe
        }
        Write-Forge "Explorer restarted." -Level Ok
    }

    Write-Forge "================================================================" -Level Info
    Write-Forge "Workstation ready — VibeForge profile applied." -Level Ok
    Write-Forge "================================================================" -Level Info
}
catch {
    Write-Forge "Bootstrap failed: $($_.Exception.Message)" -Level Error
    throw
}
finally {
    if (-not $DryRun) { try { Stop-Transcript | Out-Null } catch { } }
}
