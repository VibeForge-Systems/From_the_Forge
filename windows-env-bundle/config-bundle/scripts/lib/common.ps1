<#
    ============================================================================
    ⚒️  VibeForge Systems — Windows Environment Bundle
    config-bundle/scripts/lib/common.ps1

    Shared helper library. Dot-source this from every script in the bundle so
    logging, guards, and the install/import primitives stay in one place:

        . "$PSScriptRoot\lib\common.ps1"

    Design rules (the VibeForge way):
      • No brittle connectors. Every external action is wrapped in a guard.
      • Deterministic. Same inputs -> same result. Idempotent where it counts.
      • Honest. Optional things fail soft and say so; required things fail loud.

    Part of the VibeForge "From the Forge" collection.
    Licensed under the GNU Affero General Public License v3.0 (AGPL-3.0).
    ============================================================================
#>

Set-StrictMode -Version Latest

# ---------------------------------------------------------------------------
# Global toggles. setup.ps1 and the helper scripts set $script:ForgeDryRun from
# their -DryRun switch before invoking any primitive. If the library is
# dot-sourced standalone without setting it, default to live (non-dry-run)
# execution — a caller that wants a preview must pass -DryRun explicitly.
# ---------------------------------------------------------------------------
if (-not (Get-Variable -Name 'ForgeDryRun' -Scope Script -ErrorAction SilentlyContinue)) {
    $script:ForgeDryRun = $false
}

# ---------------------------------------------------------------------------
# Write-Forge — branded, leveled logging. Goes to the host and, when a
# transcript is running (started by setup.ps1), into the log file too.
# ---------------------------------------------------------------------------
function Write-Forge {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory, Position = 0)]
        [string]$Message,

        [ValidateSet('Info', 'Step', 'Warn', 'Error', 'Ok', 'Skip')]
        [string]$Level = 'Info'
    )

    $prefix = '>> VibeForge:'
    switch ($Level) {
        'Step'  { Write-Host "$prefix $Message"            -ForegroundColor Cyan }
        'Ok'    { Write-Host "$prefix [ok]   $Message"     -ForegroundColor Green }
        'Skip'  { Write-Host "$prefix [skip] $Message"     -ForegroundColor DarkGray }
        'Warn'  { Write-Warning "$prefix $Message" }
        'Error' { Write-Host "$prefix [FAIL] $Message"     -ForegroundColor Red }
        default { Write-Host "$prefix $Message" }
    }
}

# ---------------------------------------------------------------------------
# Assert-Administrator — winget and HKLM writes need an elevated context.
# Fail fast with a clear remediation instead of erroring deep in a step.
# ---------------------------------------------------------------------------
function Assert-Administrator {
    [CmdletBinding()]
    param()

    $identity  = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    $isAdmin   = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

    if (-not $isAdmin) {
        throw "This bundle must run elevated. Right-click PowerShell and 'Run as administrator', then re-run setup.ps1."
    }
    Write-Forge "Elevation confirmed (administrator)." -Level Ok
}

# ---------------------------------------------------------------------------
# Assert-Windows11 — this bundle restores a Windows 10-style shell on
# Windows 11 (build >= 22000). On anything older the components are a no-op
# at best and confusing at worst, so guard the entry point.
# ---------------------------------------------------------------------------
function Assert-Windows11 {
    [CmdletBinding()]
    param(
        # Allow power users to proceed anyway (e.g. testing on a VM image).
        [switch]$Force
    )

    $build = [Environment]::OSVersion.Version.Build
    Write-Forge "Detected Windows build $build." -Level Info

    if ($build -lt 22000) {
        $msg = "Windows 11 (build >= 22000) is required; this host reports build $build."
        if ($Force) {
            Write-Forge "$msg Continuing because -Force was supplied." -Level Warn
        }
        else {
            throw "$msg Re-run with -Force to override at your own risk."
        }
    }
    else {
        Write-Forge "Windows 11 target confirmed." -Level Ok
    }
}

# ---------------------------------------------------------------------------
# Test-WinGet — confirm the package manager is present before we lean on it.
# ---------------------------------------------------------------------------
function Test-WinGet {
    [CmdletBinding()]
    param()

    $winget = Get-Command winget -ErrorAction SilentlyContinue
    if (-not $winget) {
        throw "winget (Windows Package Manager) was not found. Install 'App Installer' from the Microsoft Store, then re-run."
    }
    Write-Forge "winget found: $($winget.Source)" -Level Ok
    return $true
}

# ---------------------------------------------------------------------------
# Test-PackageInstalled — best-effort idempotency check so re-runs are cheap
# and quiet. winget's exit codes are noisy, so we parse `list` instead.
# ---------------------------------------------------------------------------
function Test-PackageInstalled {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Id,
        [ValidateSet('winget', 'msstore')][string]$Source = 'winget'
    )

    try {
        # 'winget list' does NOT accept --accept-source-agreements (it errors with
        # an unrecognized-argument message), which would make this check always
        # fail and re-trigger installs every run. Keep the list invocation minimal.
        $listArgs = @('list', '--id', $Id, '--exact')
        if ($Source -eq 'msstore') { $listArgs += @('--source', 'msstore') }
        $output = & winget @listArgs 2>$null
        # winget prints the id in the results table only when it is installed.
        return ($LASTEXITCODE -eq 0 -and ($output -join "`n") -match [regex]::Escape($Id))
    }
    catch {
        return $false
    }
}

# ---------------------------------------------------------------------------
# Install-Package — the one installer primitive. Idempotent, dry-run aware,
# and source-routed:
#   • Source 'winget'  -> winget community repo
#   • Source 'msstore' -> Microsoft Store (TaskbarX lives here)
#   • -DownloadUrl     -> pinned zip fallback with mandatory SHA256 verify
#   • -Optional        -> failures warn instead of throwing
# ---------------------------------------------------------------------------
function Install-Package {
    [CmdletBinding(SupportsShouldProcess)]
    param(
        [Parameter(Mandatory)][string]$Name,
        [string]$Id,
        [ValidateSet('winget', 'msstore')][string]$Source = 'winget',
        [string]$DownloadUrl,
        [string]$Sha256,
        [string]$ExtractTo,
        [switch]$Optional
    )

    Write-Forge "Installing $Name..." -Level Step

    if ($script:ForgeDryRun) {
        Write-Forge "[dry-run] would install $Name (id=$Id, source=$Source)." -Level Skip
        return
    }

    # Already present? Skip and move on — that's what "deterministic" buys us.
    if ($Id -and (Test-PackageInstalled -Id $Id -Source $Source)) {
        Write-Forge "$Name already installed." -Level Skip
        return
    }

    try {
        if ($Id) {
            $wingetArgs = @(
                'install', '--id', $Id, '--exact',
                '--accept-source-agreements', '--accept-package-agreements',
                '--disable-interactivity'
            )
            if ($Source -eq 'msstore') { $wingetArgs += @('--source', 'msstore') }

            if ($PSCmdlet.ShouldProcess($Name, "winget install ($Source)")) {
                & winget @wingetArgs
                if ($LASTEXITCODE -ne 0) {
                    throw "winget exited with code $LASTEXITCODE for '$Id' (source=$Source)."
                }
                Write-Forge "$Name installed via $Source." -Level Ok
                return
            }
            else {
                # ShouldProcess returned false (-WhatIf / -Confirm declined):
                # report a clear skipped outcome rather than falling through silently.
                Write-Forge "$Name install skipped (WhatIf/declined)." -Level Skip
                return
            }
        }
        elseif ($DownloadUrl) {
            Install-FromZip -Name $Name -DownloadUrl $DownloadUrl -Sha256 $Sha256 -ExtractTo $ExtractTo
            return
        }
        else {
            throw "No install method given for '$Name' (need -Id or -DownloadUrl)."
        }
    }
    catch {
        # Some packages (TaskbarX) may have a zip fallback even when the
        # primary source fails. Try it before deciding how loud to be.
        if ($DownloadUrl -and $Id) {
            Write-Forge "$Name primary install failed ($($_.Exception.Message)); trying pinned download." -Level Warn
            try {
                Install-FromZip -Name $Name -DownloadUrl $DownloadUrl -Sha256 $Sha256 -ExtractTo $ExtractTo
                return
            }
            catch {
                # fall through to the Optional/required decision below
            }
        }

        if ($Optional) {
            Write-Forge "$Name is optional and did not install: $($_.Exception.Message)" -Level Warn
            return
        }
        throw
    }
}

# ---------------------------------------------------------------------------
# Install-FromZip — pinned download with mandatory checksum verification.
# We refuse to run an unverified binary; a missing/blank SHA256 is a hard stop.
# ---------------------------------------------------------------------------
function Install-FromZip {
    [CmdletBinding(SupportsShouldProcess)]
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][string]$DownloadUrl,
        [string]$Sha256,
        [string]$ExtractTo
    )

    if ([string]::IsNullOrWhiteSpace($Sha256) -or $Sha256 -eq 'REPLACE_WITH_SHA256') {
        throw "Refusing to install '$Name' from $DownloadUrl without a pinned SHA256. Set the hash in bundle.psd1 first."
    }
    if ([string]::IsNullOrWhiteSpace($ExtractTo)) {
        $ExtractTo = Join-Path $env:LOCALAPPDATA $Name
    }

    $tempZip = Join-Path $env:TEMP ("{0}.zip" -f $Name)
    Write-Forge "Downloading $Name from $DownloadUrl" -Level Info

    if (-not $PSCmdlet.ShouldProcess($Name, "download + verify + extract")) { return }

    # TLS 1.2 for older hosts; harmless on current ones.
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    # -UseBasicParsing is intentional and cross-version safe: on Windows PowerShell
    # 5.1 it avoids the IE DOM engine (which throws when IE first-run isn't set up);
    # on PowerShell 6/7+ it is a deprecated no-op that is retained, NOT removed, so
    # it does not error. Keep it.
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $tempZip -UseBasicParsing

    $actual = (Get-FileHash -Path $tempZip -Algorithm SHA256).Hash
    if ($actual -ne $Sha256.ToUpperInvariant()) {
        Remove-Item $tempZip -Force -ErrorAction SilentlyContinue
        throw "Checksum mismatch for '$Name'. Expected $Sha256 but got $actual. Aborting."
    }
    Write-Forge "Checksum verified for $Name." -Level Ok

    if (-not (Test-Path $ExtractTo)) {
        New-Item -ItemType Directory -Path $ExtractTo -Force | Out-Null
    }
    Expand-Archive -Path $tempZip -DestinationPath $ExtractTo -Force
    Remove-Item $tempZip -Force -ErrorAction SilentlyContinue
    Write-Forge "$Name extracted to $ExtractTo." -Level Ok
}

# ---------------------------------------------------------------------------
# Import-RegFile — guarded, logged `reg import`. Honors dry-run and verifies
# the file exists before shelling out.
# ---------------------------------------------------------------------------
function Import-RegFile {
    [CmdletBinding(SupportsShouldProcess)]
    param(
        [Parameter(Mandatory)][string]$Path
    )

    if (-not (Test-Path $Path)) {
        Write-Forge "Registry file not found, skipping: $Path" -Level Warn
        return
    }

    Write-Forge "Importing registry: $([IO.Path]::GetFileName($Path))" -Level Step
    if ($script:ForgeDryRun) {
        Write-Forge "[dry-run] would reg import $Path" -Level Skip
        return
    }

    if ($PSCmdlet.ShouldProcess($Path, 'reg import')) {
        & reg.exe import $Path 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0) {
            throw "reg import failed (exit $LASTEXITCODE) for $Path"
        }
        Write-Forge "Imported $([IO.Path]::GetFileName($Path))." -Level Ok
    }
}

# ---------------------------------------------------------------------------
# Copy-BundleFile — guarded copy that creates the destination tree as needed.
# ---------------------------------------------------------------------------
function Copy-BundleFile {
    [CmdletBinding(SupportsShouldProcess)]
    param(
        [Parameter(Mandatory)][string]$Source,
        [Parameter(Mandatory)][string]$Destination
    )

    if (-not (Test-Path $Source)) {
        Write-Forge "Source file missing, skipping copy: $Source" -Level Warn
        return
    }

    if ($script:ForgeDryRun) {
        Write-Forge "[dry-run] would copy $Source -> $Destination" -Level Skip
        return
    }

    $destDir = Split-Path -Parent $Destination
    if ($destDir -and -not (Test-Path $destDir)) {
        New-Item -ItemType Directory -Path $destDir -Force | Out-Null
    }

    if ($PSCmdlet.ShouldProcess($Destination, "copy from $Source")) {
        Copy-Item -Path $Source -Destination $Destination -Force
        Write-Forge "Copied $([IO.Path]::GetFileName($Source)) -> $Destination" -Level Ok
    }
}

# Expand any %ENV% tokens used in the manifest's ConfigMap destinations.
function Expand-ForgePath {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$Path)
    return [Environment]::ExpandEnvironmentVariables($Path)
}
