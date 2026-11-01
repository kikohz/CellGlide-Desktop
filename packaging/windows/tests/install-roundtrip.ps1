<#
Install / reinstall / uninstall round trip of a CellGlide package, with network snapshots (CI and
this machine; needs an elevated PowerShell).

  powershell -NoProfile -ExecutionPolicy Bypass -File packaging\windows\tests\install-roundtrip.ps1 -Zip <zip> [-BackupDir <new folder>]

Steps: snapshot A (scripts\network-snapshot.ps1); note Apple's device service; install.ps1
-ZipPath <zip> -Unattended; check the service (Running, quoted path), app\CellGlide.exe, the
Start menu shortcut and the Installed-apps entry (DisplayVersion = the zip's version.txt);
install again (Reinstalling, no CellGlide.new or .old left); snapshot B = A; the installed
uninstall.ps1 -Purge; check that the service, the program folder, the shortcut, the entry,
cellglide0's two interface keys, C:\ProgramData\CellGlide and the phantom wintun device with our
GUID are all gone; Apple's service as at the start; snapshot C = A.

-Purge deletes C:\ProgramData\CellGlide and %LOCALAPPDATA%\CellGlide, so when either exists the
script refuses to run without -BackupDir. With it, both are copied there first (robocopy /E
/COPYALL) and copied back after the uninstall; their file counts and the SHA-256 of
settings.json and speedtests.json (where present) must match what they were. Every failed check
is printed as FAIL and the exit code is then 1.
#>
param(
    [Parameter(Mandatory)][string]$Zip,
    [string]$BackupDir = ''
)
$ErrorActionPreference = 'Stop'
$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
$Installer = Join-Path $Repo 'packaging\windows\install.ps1'
$Snapshot = Join-Path $Repo 'scripts\network-snapshot.ps1'
$PowerShell = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"

$InstallDir = 'C:\Program Files\CellGlide'
$ServiceName = 'CellGlide'
$DataDir = "$env:ProgramData\CellGlide"
$UserDataDir = "$env:LOCALAPPDATA\CellGlide"
$UninstallKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\CellGlide'
$Shortcut = "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\CellGlide.lnk"
$AdapterGuid = '{E490EEF2-2CB0-4A80-B341-BF929C2E56B3}'
$AdapterInstanceId = "SWD\Wintun\$AdapterGuid"
$AppleService = 'Apple Mobile Device Service'
# Files whose hash must survive the backup and restore, where present.
$CheckedFiles = @('settings.json', 'speedtests.json')

$script:Failures = New-Object System.Collections.Generic.List[string]

function Test-Check([bool]$Ok, [string]$What) {
    if ($Ok) { Write-Host "ok   $What" }
    else { Write-Host "FAIL $What"; $script:Failures.Add($What) }
}

function Invoke-Script([string]$Path, [string[]]$Arguments) {
    # Runs a script in a new Windows PowerShell; prints and returns its output lines and exit code.
    $ErrorActionPreference = 'Continue'
    Write-Host "> $Path $($Arguments -join ' ')"
    $lines = @(& $PowerShell -NoProfile -ExecutionPolicy Bypass -File $Path @Arguments 2>&1 | ForEach-Object { "$_" })
    $code = $LASTEXITCODE
    foreach ($line in $lines) { Write-Host "  $line" }
    return [pscustomobject]@{ Code = $code; Lines = $lines }
}

function Get-ServiceState([string]$Name) {
    $svc = Get-Service -Name $Name -ErrorAction SilentlyContinue
    if ($svc) { return [string]$svc.Status }
    return 'absent'
}

function Get-ZipVersion([string]$Path) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($Path)
    try {
        $entry = $archive.GetEntry('version.txt')
        if (-not $entry) { throw "$Path has no version.txt" }
        $reader = New-Object IO.StreamReader($entry.Open())
        try { return $reader.ReadToEnd().Trim() } finally { $reader.Dispose() }
    } finally { $archive.Dispose() }
}

function Get-DirRecord([string]$Dir) {
    # The number of files and folders under $Dir and the hash of each checked file.
    $hashes = @{}
    foreach ($name in $CheckedFiles) {
        $f = Join-Path $Dir $name
        if (Test-Path -LiteralPath $f -PathType Leaf) { $hashes[$name] = (Get-FileHash -LiteralPath $f -Algorithm SHA256).Hash }
    }
    return [pscustomobject]@{ Count = @(Get-ChildItem -LiteralPath $Dir -Recurse -Force).Count; Hashes = $hashes }
}

function Invoke-Robocopy([string]$From, [string]$To) {
    $ErrorActionPreference = 'Continue'
    $out = & robocopy.exe $From $To /E /COPYALL /R:2 /W:1 /NP /NFL /NDL 2>&1 | Out-String
    if ($LASTEXITCODE -ge 8) { throw "robocopy $From $To failed (exit $LASTEXITCODE): $($out.Trim())" }
}

function Save-Snapshot([string]$Name) {
    $file = Join-Path $script:Work "snapshot-$Name.json"
    & $PowerShell -NoProfile -ExecutionPolicy Bypass -File $Snapshot -Out $file | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "network-snapshot.ps1 -Out $file failed (exit $LASTEXITCODE)" }
    return $file
}

function Test-SameSnapshot([string]$Before, [string]$After, [string]$What) {
    $ErrorActionPreference = 'Continue'
    $out = @(& $PowerShell -NoProfile -ExecutionPolicy Bypass -File $Snapshot -Compare $Before $After 2>&1 | ForEach-Object { "$_" })
    $code = $LASTEXITCODE
    foreach ($line in $out) { Write-Host "  $line" }
    Test-Check ($code -eq 0 -and ($out -contains 'identical')) $What
}

function Test-Gone {
    $workDirs = @(Get-ChildItem -LiteralPath (Split-Path -Parent $InstallDir) -Directory -Force -Filter 'CellGlide.*' -ErrorAction SilentlyContinue)
    Test-Check ((Get-ServiceState $ServiceName) -eq 'absent') 'the service is gone'
    Test-Check (-not (Test-Path -LiteralPath $InstallDir)) "$InstallDir is gone"
    Test-Check ($workDirs.Count -eq 0) 'no CellGlide.new, .old or .work-* folder is left'
    Test-Check (-not (Test-Path -LiteralPath $Shortcut)) 'the Start menu shortcut is gone'
    Test-Check (-not (Test-Path -LiteralPath $UninstallKey)) 'the Installed-apps entry is gone'
    foreach ($stack in 'Tcpip', 'Tcpip6') {
        Test-Check (-not (Test-Path -LiteralPath "HKLM:\SYSTEM\CurrentControlSet\Services\$stack\Parameters\Interfaces\$AdapterGuid")) "cellglide0's $stack interface key is gone"
    }
    Test-Check (-not (Test-Path -LiteralPath $DataDir)) "$DataDir is gone"
    Test-Check (-not (Test-Path -LiteralPath $UserDataDir)) "$UserDataDir is gone"
    $device = @(Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { $_.InstanceId -eq $AdapterInstanceId })
    Test-Check ($device.Count -eq 0) "no device $AdapterInstanceId"
}

$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) { Write-Host 'install-roundtrip.ps1 must run elevated.'; exit 1 }
if (-not (Test-Path -LiteralPath $Zip -PathType Leaf)) { Write-Host "$Zip does not exist."; exit 1 }
$Zip = (Resolve-Path -LiteralPath $Zip).ProviderPath
$existing = @($DataDir, $UserDataDir | Where-Object { Test-Path -LiteralPath $_ })
if ($existing.Count -gt 0 -and -not $BackupDir) {
    Write-Host "$($existing -join ' and ') exist; uninstall.ps1 -Purge would delete them. Run again with -BackupDir <new folder> to have them copied there first and put back afterwards."
    exit 1
}
if ($BackupDir -and (Test-Path -LiteralPath $BackupDir) -and @(Get-ChildItem -LiteralPath $BackupDir -Force).Count -gt 0) {
    Write-Host "$BackupDir is not empty; give a new folder."
    exit 1
}

$script:Work = if ($BackupDir) { $BackupDir } else { Join-Path ([IO.Path]::GetTempPath()) ('cellglide-roundtrip-' + [guid]::NewGuid().ToString('N')) }
$null = New-Item -ItemType Directory -Force -Path $script:Work
$version = Get-ZipVersion $Zip
Write-Host "Round trip of CellGlide $version from $Zip; snapshots in $script:Work"

$backups = @()
if ($BackupDir) {
    foreach ($pair in @(@($DataDir, 'ProgramData'), @($UserDataDir, 'LocalAppData'))) {
        if (-not (Test-Path -LiteralPath $pair[0])) { continue }
        $copy = Join-Path $BackupDir $pair[1]
        Invoke-Robocopy $pair[0] $copy
        $record = Get-DirRecord $pair[0]
        $backups += [pscustomobject]@{ Dir = $pair[0]; Copy = $copy; Record = $record }
        Write-Host "Backed up $($pair[0]) ($($record.Count) entries) to $copy"
    }
}

try {
    $a = Save-Snapshot 'A'
    $appleBefore = Get-ServiceState $AppleService
    Write-Host "$AppleService`: $appleBefore"

    $r = Invoke-Script $Installer @('-ZipPath', $Zip, '-Unattended')
    Test-Check ($r.Code -eq 0) 'install.ps1 exits 0'
    $svc = Get-CimInstance -ClassName Win32_Service -Filter "Name='$ServiceName'"
    Test-Check ((Get-ServiceState $ServiceName) -eq 'Running') 'the service is Running'
    Test-Check ($svc -and $svc.PathName -ceq "`"$InstallDir\cellglide-service.exe`"") 'the service path is quoted'
    Test-Check (Test-Path -LiteralPath "$InstallDir\app\CellGlide.exe" -PathType Leaf) 'app\CellGlide.exe is installed'
    Test-Check (Test-Path -LiteralPath $Shortcut -PathType Leaf) 'the Start menu shortcut exists'
    $entry = Get-ItemProperty -LiteralPath $UninstallKey -ErrorAction SilentlyContinue
    Test-Check ($entry -and $entry.DisplayVersion -ceq $version) "the Installed-apps entry says $version"

    $r = Invoke-Script $Installer @('-ZipPath', $Zip, '-Unattended')
    Test-Check ($r.Code -eq 0) 'the second install.ps1 exits 0'
    Test-Check (@($r.Lines | Where-Object { $_ -ceq "Reinstalling $version" }).Count -eq 1) "the second run says Reinstalling $version"
    Test-Check (-not (Test-Path -LiteralPath "$InstallDir.new") -and -not (Test-Path -LiteralPath "$InstallDir.old")) 'no CellGlide.new or .old after the reinstall'
    Test-Check ((Get-ServiceState $ServiceName) -eq 'Running') 'the service is Running after the reinstall'
    $b = Save-Snapshot 'B'
    Test-SameSnapshot $a $b 'snapshot B (installed) is identical to A'

    $r = Invoke-Script "$InstallDir\uninstall.ps1" @('-Purge')
    Test-Check ($r.Code -eq 0) 'uninstall.ps1 -Purge exits 0'
    Test-Gone
    $appleAfter = Get-ServiceState $AppleService
    Test-Check ($appleAfter -eq $appleBefore) "$AppleService is $appleAfter, as at the start"
    $c = Save-Snapshot 'C'
    Test-SameSnapshot $a $c 'snapshot C (uninstalled) is identical to A'
} catch {
    Test-Check $false "the round trip ran to the end ($($_.Exception.Message))"
} finally {
    foreach ($backup in $backups) {
        try {
            Invoke-Robocopy $backup.Copy $backup.Dir
            $now = Get-DirRecord $backup.Dir
            Test-Check ($now.Count -eq $backup.Record.Count) "$($backup.Dir) is back with $($backup.Record.Count) entries (now $($now.Count))"
            foreach ($name in $backup.Record.Hashes.Keys) {
                Test-Check ($now.Hashes[$name] -ceq $backup.Record.Hashes[$name]) "$($backup.Dir)\$name has its SHA-256 back"
            }
        } catch {
            Test-Check $false "$($backup.Dir) was put back from $($backup.Copy) ($($_.Exception.Message))"
        }
    }
}

if ($script:Failures.Count -gt 0) {
    Write-Host "Round trip FAILED: $($script:Failures.Count) check(s): $($script:Failures -join '; ')"
    exit 1
}
Write-Host 'Round trip OK.'
exit 0
