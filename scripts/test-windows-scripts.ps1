# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\test-windows-scripts.ps1
#
# Runs the Pester 5 tests of the Windows scripts: scripts\tests\*.Tests.ps1 and
# packaging\windows\tests\*.Tests.ps1. Windows PowerShell ships Pester 3.4, so Pester 5 (5.5.0 or
# later, below 6) is installed for the current user only; without it this prints the command and
# exits 1. Tests tagged 'Elevated' run only from an administrator shell. Exits with the number of
# failed tests (0 = all passed).
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

$pester = Get-Module -ListAvailable Pester |
    Where-Object { $_.Version -ge [version]'5.5.0' -and $_.Version.Major -eq 5 } |
    Sort-Object Version -Descending | Select-Object -First 1
if (-not $pester) {
    Write-Host 'Pester 5.5.0 or later (5.x) is needed. Install it for the current user with:'
    Write-Host '  Install-Module Pester -MinimumVersion 5.5.0 -MaximumVersion 5.99.99 -Scope CurrentUser -Force -SkipPublisherCheck'
    exit 1
}
Get-Module Pester | Remove-Module -Force
Import-Module $pester.Path -Force
Write-Host "Pester $($pester.Version) from $($pester.ModuleBase)"

$paths = @()
foreach ($rel in 'scripts\tests', 'packaging\windows\tests') {
    $dir = Join-Path $root $rel
    if (@(Get-ChildItem -Path $dir -Filter '*.Tests.ps1' -File -ErrorAction SilentlyContinue).Count -gt 0) { $paths += $dir }
}
if ($paths.Count -eq 0) {
    Write-Host 'FAILED: no *.Tests.ps1 files found under scripts\tests or packaging\windows\tests.'
    exit 1
}

$config = New-PesterConfiguration
$config.Run.Path = [string[]]$paths
$config.Run.Exit = $true
$config.Output.Verbosity = 'Detailed'
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    $config.Filter.ExcludeTag = [string[]]@('Elevated')
    Write-Host "Not running as administrator: tests tagged 'Elevated' are skipped."
}
Invoke-Pester -Configuration $config
