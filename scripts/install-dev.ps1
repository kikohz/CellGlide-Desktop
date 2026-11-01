<#
Development install of CellGlide on this Windows machine: the real installer with a local build
(decision 17), so a development install is laid out, registered and removed exactly as a user's.

  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install-dev.ps1
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install-dev.ps1 -Uninstall [-Purge]

Install: scripts\build-windows.ps1 -Arch <this PC's architecture> builds the package into
dist\windows\<version>\, then packaging\windows\install.ps1 -ZipPath <that zip> -Unattended
installs it (service, files, data directory ACLs, Start menu shortcut, Installed-apps entry).
Uninstall: packaging\windows\uninstall.ps1 [-Purge] (the repository's copy; -Purge also deletes
C:\ProgramData\CellGlide and every user's %LOCALAPPDATA%\CellGlide, so back them up first on a
machine whose data matters).

Both scripts ask for administrator rights themselves (one UAC prompt in an ordinary window).
Only CellGlide's own service, files, adapter (cellglide0) and registry entries are touched. Take
network snapshots (scripts\network-snapshot.ps1) before and after. The exit code is the
installer's or uninstaller's.
#>
param(
    [switch]$Uninstall,
    [switch]$Purge
)
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
$PowerShell = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"

function Get-WorkspaceVersion([string]$CargoToml) {
    # Cargo.toml [workspace.package] version, the only source of the version.
    $section = ''
    foreach ($line in [IO.File]::ReadAllLines($CargoToml)) {
        $trimmed = $line.Trim()
        if ($trimmed -match '^\[(.+)\]$') { $section = $Matches[1].Trim(); continue }
        if ($section -eq 'workspace.package' -and $trimmed -match '^version\s*=\s*"([^"]+)"') { return $Matches[1] }
    }
    throw "no [workspace.package] version in $CargoToml"
}

function Get-DevArch {
    # The package architecture of this PC (an x64 shell emulated on ARM64 still says ARM64 here).
    $machine = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    try {
        $os = [string][Runtime.InteropServices.RuntimeInformation]::OSArchitecture
        if ($os -eq 'Arm64') { $machine = 'ARM64' }
    } catch { }
    switch ($machine) {
        'AMD64' { return 'x64' }
        'ARM64' { return 'arm64' }
    }
    throw "CellGlide builds for x64 and ARM64 Windows only (this PC is $machine)"
}

if ($Purge -and -not $Uninstall) {
    Write-Host '-Purge is only valid with -Uninstall.'
    exit 1
}

if ($Uninstall) {
    $uninstallArgs = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $Root 'packaging\windows\uninstall.ps1'))
    if ($Purge) { $uninstallArgs += '-Purge' }
    & $PowerShell @uninstallArgs
    exit $LASTEXITCODE
}

$arch = Get-DevArch
$version = Get-WorkspaceVersion (Join-Path $Root 'Cargo.toml')
& $PowerShell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'build-windows.ps1') -Arch $arch
if ($LASTEXITCODE -ne 0) {
    Write-Host "build-windows.ps1 failed (exit $LASTEXITCODE); nothing was installed."
    exit 1
}
$zip = Join-Path $Root "dist\windows\$version\CellGlide-$version-windows-$arch.zip"
if (-not (Test-Path -LiteralPath $zip -PathType Leaf)) {
    Write-Host "build-windows.ps1 did not produce $zip; nothing was installed."
    exit 1
}
& $PowerShell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $Root 'packaging\windows\install.ps1') -ZipPath $zip -Unattended
exit $LASTEXITCODE
