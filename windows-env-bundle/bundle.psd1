#
# ============================================================================
# ⚒️  VibeForge Systems — Windows Environment Bundle
# bundle.psd1 — the single source of truth for the bundle.
#
# This is a native PowerShell *data* file. It is read with
# Import-PowerShellDataFile, which only evaluates literals (no code runs), so
# it is safe to ship and easy to diff. Edit THIS file to change what the
# bundle installs or applies — the scripts stay generic.
#
# Part of the VibeForge "From the Forge" collection.
# Licensed under the GNU Affero General Public License v3.0 (AGPL-3.0).
# ============================================================================
#
@{
    # --- Bundle identity -----------------------------------------------------
    Name        = 'vibeforge-windows-env-bundle'
    Version     = '1.0.0'
    TargetOS    = 'Windows 11 (build >= 22000)'
    Description  = 'Reproducible Windows 10-style shell + baseline apps for regulated enterprise dev workstations.'

    # --- Shell components ----------------------------------------------------
    # Installed first; these define the "VibeForge shell profile".
    #   Source 'winget'  -> winget community repo (default source)
    #   Source 'msstore' -> Microsoft Store source
    #   DownloadUrl/Sha256 -> pinned zip fallback (Sha256 is MANDATORY to use)
    #   Optional = $true -> a failure warns instead of aborting the run
    ShellComponents = @(
        @{
            Name   = 'ExplorerPatcher'
            Id     = 'valinet.ExplorerPatcher'
            Source = 'winget'
            Notes  = 'Restores the Windows 10 taskbar, Start behavior, and ribbon Explorer on Windows 11.'
        },
        @{
            Name   = 'Open-Shell'
            # NOTE: the correct winget id is Open-Shell.Open-Shell-Menu.
            # The common "OpenShell.OpenShell" id does not exist.
            Id     = 'Open-Shell.Open-Shell-Menu'
            Source = 'winget'
            Notes  = 'Classic Start menu. Settings are imported explicitly (see ConfigMap), not auto-read.'
        },
        @{
            Name        = 'TaskbarX'
            # TaskbarX is NOT in the winget community repo. It ships through the
            # Microsoft Store, so we route to the msstore source. The pinned zip
            # below is a fallback; fill in a real release URL + SHA256 to enable.
            Id          = '9PF3RHHHRN95'      # Microsoft Store product id for TaskbarX
            Source      = 'msstore'
            Optional    = $true
            DownloadUrl = 'https://github.com/ChrisAnd1998/TaskbarX/releases/download/v1.7.8.0/TaskbarX.zip'
            Sha256      = 'REPLACE_WITH_SHA256'  # set this to enable the zip fallback
            ExtractTo   = '%LOCALAPPDATA%\TaskbarX'
            Notes       = 'Optional. Centers taskbar icons. Persists via Task Scheduler/CLI, not a JSON config.'
        }
    )

    # --- Baseline apps -------------------------------------------------------
    # The "minimum viable workstation". All available in the winget repo with
    # verified ids.
    BaselineApps = @(
        @{ Name = 'Visual Studio Code'; Id = 'Microsoft.VisualStudioCode'; Source = 'winget' },
        @{ Name = 'Google Chrome';      Id = 'Google.Chrome';             Source = 'winget' },
        @{ Name = 'Git';                Id = 'Git.Git';                   Source = 'winget' },
        @{ Name = '7-Zip';              Id = '7zip.7zip';                 Source = 'winget' },
        @{ Name = 'Notepad++';          Id = 'Notepad++.Notepad++';       Source = 'winget' }
    )

    # --- Config map ----------------------------------------------------------
    # How each config artifact is applied. Paths support %ENV% tokens.
    #   Method 'reg-import' -> imported via reg.exe (idempotent)
    #   Method 'copy'       -> staged to disk for a later explicit import
    ConfigMap = @(
        @{
            Name        = 'ExplorerPatcher settings'
            Source      = 'config-bundle\shell\explorerpatcher.reg'
            Method      = 'reg-import'
        },
        @{
            Name        = 'Desktop baselines'
            Source      = 'config-bundle\registry\desktop.reg'
            Method      = 'reg-import'
        },
        @{
            Name        = 'Explorer baselines'
            Source      = 'config-bundle\registry\explorer.reg'
            Method      = 'reg-import'
        },
        @{
            Name        = 'Open-Shell settings'
            Source      = 'config-bundle\shell\openshell.xml'
            Destination = '%PROGRAMDATA%\VibeForge\OpenShellSettings.xml'
            Method      = 'copy'
            AppliedBy   = 'apply-shell.ps1 (ClassicStartMenu.exe -xml)'
        }
    )
}
