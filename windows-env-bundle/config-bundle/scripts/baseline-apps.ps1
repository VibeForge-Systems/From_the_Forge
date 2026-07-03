<#
    ============================================================================
    ⚒️  VibeForge Systems — Windows Environment Bundle
    config-bundle/scripts/baseline-apps.ps1

    Installs the "minimum viable workstation" apps listed in bundle.psd1.
    Manifest-driven: this script never hardcodes a package id.

    Can be run standalone (resolves the lib + manifest relative to itself) or
    invoked by setup.ps1.

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

$manifest = Import-PowerShellDataFile -Path $ManifestPath
Write-Forge "Installing $($manifest.BaselineApps.Count) baseline app(s)." -Level Step

foreach ($app in $manifest.BaselineApps) {
    $splat = @{
        Name     = $app.Name
        Id       = $app.Id
        Source   = $(if ($app.ContainsKey('Source')) { $app.Source } else { 'winget' })
        Optional = $true   # one failed app should not abort the whole baseline
    }
    Install-Package @splat
}

Write-Forge "Baseline apps complete." -Level Ok
