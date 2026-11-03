<#
Installs or upgrades CellGlide on Windows 11.

  irm https://pkg.cellglide.com/install.ps1 | iex
  powershell -NoProfile -ExecutionPolicy Bypass -File install.ps1 [-ZipPath <zip>] [-Unattended]
  powershell -NoProfile -ExecutionPolicy Bypass -File install.ps1 -BaseUrl http://127.0.0.1:<port> -AllowLoopback [-Unattended]

Checks the system, Smart App Control and Apple's device service in the user's own process; the
download, the file swap, the service, the data directory ACLs, the Start menu shortcut and the
Installed-apps entry are done elevated (one UAC prompt). The elevated child is started with a
small -EncodedCommand bootstrap that reads this script's own text from %TEMP% once, checks its
SHA-256 against the hash the parent computed, and runs it from memory with -ElevatedPart; the
child's log is shown here afterwards. Then the app is started as the user and "cellglide doctor"
runs.

The packages come from https://pkg.cellglide.com. Only the -BaseUrl parameter changes that, and
only to a loopback test source (http://127.0.0.1:<port> or http://localhost:<port>) together
with -AllowLoopback; there is no environment variable, so "irm | iex" always uses production.
-ZipPath installs a local package (a mapped network drive is turned into its UNC path, which the
elevated child can see) and prints its SHA-256 to compare with the published SHA256SUMS.
-Unattended asks nothing and does not start the app. Run through iex, the script runs again in a
child scope; the caller's own variables that share a name with a parameter keep their values
(PowerShell itself drops a type constraint on such a variable), and nothing else is left in the
session.

Only CellGlide's own service, files, shortcut, uninstall entry and data directory are touched;
no other adapter, DNS setting or route is changed. Exit codes: 0 installed, 1 failed or
cancelled, 2 this PC does not meet the requirements.
#>
param(
    # Under iex the param block binds into the caller's own scope. This default runs first,
    # before any parameter is set, and keeps copies of the caller's variables with these names
    # so the iex guard below can put them back.
    [string]$BaseUrl = $(Set-Variable -Name __CellGlideCallerVars -Value @(
        Get-Variable -Name '__CellGlideCallerVars', 'BaseUrl', 'ZipPath', 'Unattended', 'ElevatedPart', 'Arch', 'LogPath', 'AllowLoopback', 'CellGlideInstallText' -Scope 0 -ErrorAction SilentlyContinue |
            ForEach-Object { New-Object Management.Automation.PSVariable -ArgumentList $_.Name, $_.Value, $_.Options, (New-Object 'Collections.ObjectModel.Collection[Attribute]' (, [Attribute[]]@($_.Attributes))) }); ''),
    [string]$ZipPath = '',
    [switch]$Unattended,
    [switch]$ElevatedPart,
    [string]$Arch = '',
    [string]$LogPath = '',
    [switch]$AllowLoopback
)

# This script's own full text, whether it runs with -File, through iex or from the elevated
# bootstrap: the elevated child runs exactly this text (decision 4), never a second download.
$CellGlideInstallText = { }.Ast.Extent.StartScriptPosition.GetFullScript()
if (-not ($MyInvocation.MyCommand.ScriptBlock -and $MyInvocation.MyCommand.ScriptBlock.ToString() -ceq $CellGlideInstallText)) {
    # Run through iex, in the caller's own scope: run the same text again in a child scope, then
    # (from a child scope, so nothing new is defined here) remove what the param block and the
    # line above created and give the caller's own variables of those names their values back.
    $null = & ([scriptblock]::Create($CellGlideInstallText))
    & {
        param([object[]]$Saved, [string[]]$Names)
        $Saved = @($Saved | Where-Object { $_ -is [Management.Automation.PSVariable] })
        $keep = @($Saved | ForEach-Object { $_.Name })
        foreach ($name in $Names) {
            if ($name -notin $keep) { Remove-Variable -Name $name -Scope 1 -Force -ErrorAction SilentlyContinue }
        }
        foreach ($s in $Saved) {
            $v = Get-Variable -Name $s.Name -Scope 1 -ErrorAction SilentlyContinue
            if (-not $v) { New-Variable -Name $s.Name -Scope 1; $v = Get-Variable -Name $s.Name -Scope 1 }
            if ($v.Options -band [Management.Automation.ScopedItemOptions]::ReadOnly) { $v.Options = $v.Options -bxor [Management.Automation.ScopedItemOptions]::ReadOnly }
            $v.Attributes.Clear()
            $v.Value = $s.Value
            foreach ($a in $s.Attributes) { $v.Attributes.Add($a) }
            if ($v.Options -ne $s.Options) { $v.Options = $s.Options }
        }
    } @($__CellGlideCallerVars) @('__CellGlideCallerVars', 'BaseUrl', 'ZipPath', 'Unattended', 'ElevatedPart', 'Arch', 'LogPath', 'AllowLoopback', 'CellGlideInstallText')
    return
}

$InstallDir = 'C:\Program Files\CellGlide'
$ServiceName = 'CellGlide'
$DataDir = "$env:ProgramData\CellGlide"
$UninstallKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\CellGlide'
$Shortcut = "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\CellGlide.lnk"
$AppleService = 'Apple Mobile Device Service'
$AppleStoreId = '9NP83LWLPZ9K'
$DefaultBase = 'https://pkg.cellglide.com'
$AdapterName = 'cellglide0'
# cg_core::tunnel::WINDOWS_ADAPTER_GUID; wintun uses it as the device instance id.
$AdapterGuid = '{E490EEF2-2CB0-4A80-B341-BF929C2E56B3}'
$AdapterInstanceId = "SWD\Wintun\$AdapterGuid"
# Well-known SIDs, so the script works on any display language.
$SidSystem = '*S-1-5-18'
$SidAdmins = '*S-1-5-32-544'
$SidUsers = '*S-1-5-32-545'
# The first line of packaging/licenses/windows-end-user-notice.txt, word for word.
$EndUserNotice = 'CellGlide includes Microsoft Windows App SDK and Windows SDK components that are licensed to you under the Microsoft Software License Terms reproduced in this file (the Windows App SDK terms and the Windows SDK terms); by installing or using CellGlide you agree to those terms for those components.'

# ---- Shared with uninstall.ps1 (decision 3: kept identical) ----
# tests\Shared.Tests.ps1 also holds Stop-CellGlideProcesses, Get-TranscriptBody,
# New-ChildBootstrap, Get-FileIdentity, Copy-LogToParent, New-PrivateWorkDir and
# Remove-StaleWorkDirs (below) to the same rule.

function Test-Elevated {
    $ErrorActionPreference = 'Stop'
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    return ([Security.Principal.WindowsPrincipal]$id).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Test-TokenElevated {
    # Whether this process runs with an elevated token (TokenElevation), whatever its groups. A
    # user who is not an administrator can still have a split token (Backup Operators, say), and
    # an elevated one is not an administrator to Test-Elevated: this tells such a process apart
    # from an ordinary one, so it is never taken for the user's own unelevated window.
    $ErrorActionPreference = 'Stop'
    if (-not ('CellGlideInstall.Token' -as [type])) {
        Add-Type -Namespace CellGlideInstall -Name Token -MemberDefinition @'
[DllImport("advapi32.dll", SetLastError = true)]
public static extern bool GetTokenInformation(System.IntPtr token, int infoClass, out int info, int length, out int returned);
'@
    }
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    try {
        $elevated = 0; $returned = 0
        # 20 = TokenElevation: a DWORD, non-zero when elevated.
        if (-not [CellGlideInstall.Token]::GetTokenInformation($id.Token, 20, [ref]$elevated, 4, [ref]$returned)) {
            throw 'cannot read whether this process is elevated'
        }
        return ($elevated -ne 0)
    } finally {
        $id.Dispose()
    }
}

function Invoke-Native {
    # Runs a native command; throws with its output when the exit code is not 0.
    param([string]$Exe, [string[]]$ArgList)
    # stderr lines must not become terminating errors under 'Stop'.
    $ErrorActionPreference = 'Continue'
    $out = & $Exe @ArgList 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) {
        throw "$Exe $($ArgList -join ' ') failed (exit $LASTEXITCODE): $($out.Trim())"
    }
    return $out
}

function Get-OurAdapter {
    $ErrorActionPreference = 'Stop'
    Get-NetAdapter -IncludeHidden -ErrorAction SilentlyContinue |
        Where-Object { $_.InterfaceGuid -eq $AdapterGuid -or $_.Name -eq $AdapterName }
}

function Wait-AdapterGone {
    # True when cellglide0 is gone within the timeout.
    param([int]$TimeoutSeconds = 10)
    $ErrorActionPreference = 'Stop'
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ($true) {
        if (-not (Get-OurAdapter)) { return $true }
        if ((Get-Date) -ge $deadline) { return $false }
        Start-Sleep -Milliseconds 200
    }
}

function Stop-OurService {
    # Stops CellGlide if it exists and waits for cellglide0 to go. Returns true if it did.
    $ErrorActionPreference = 'Stop'
    $svc = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
    if ($svc -and $svc.Status -ne 'Stopped') {
        Write-Host "Stopping service $ServiceName"
        Stop-Service -Name $ServiceName -Force
        (Get-Service -Name $ServiceName).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(30))
    }
    return (Wait-AdapterGone -TimeoutSeconds 10)
}

function Remove-PathWithRetry {
    # The service's image can stay locked for a moment after it stops.
    param([string]$Path)
    $ErrorActionPreference = 'Stop'
    for ($i = 0; $i -lt 20; $i++) {
        if (-not (Test-Path -LiteralPath $Path)) { return }
        try { Remove-Item -LiteralPath $Path -Recurse -Force; return }
        catch { Start-Sleep -Milliseconds 500 }
    }
    Remove-Item -LiteralPath $Path -Recurse -Force
}

function Remove-PhantomDevice {
    # A crash can leave cellglide0's device node behind as non-present (CM_PROB_PHANTOM).
    # Only the node whose instance id is our fixed wintun GUID is removed, and only when
    # it is not present (a live adapter is never removed here).
    $ErrorActionPreference = 'Stop'
    $dev = Get-PnpDevice -Class Net -ErrorAction SilentlyContinue |
        Where-Object { $_.InstanceId -eq $AdapterInstanceId }
    if (-not $dev) { return }
    if ($dev.Present) {
        Write-Warning "$AdapterName device is present; not removing it"
        return
    }
    Write-Host "Removing phantom device $($dev.InstanceId) ($($dev.FriendlyName))"
    Invoke-Native 'pnputil.exe' @('/remove-device', $dev.InstanceId) | Out-Null
}

function Select-ProcessesToStop {
    # The ids of processes running a program under the install directory (the app and the
    # CLI, whose names differ only in case), except the service, which the SCM stops.
    param([object[]]$Processes, [string]$InstallDir)
    $ErrorActionPreference = 'Stop'
    $prefix = $InstallDir.TrimEnd('\') + '\'
    $ids = @()
    foreach ($p in @($Processes)) {
        $exe = [string]$p.ExecutablePath
        if (-not $exe) { continue }
        if (-not $exe.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { continue }
        if ([IO.Path]::GetFileName($exe) -ieq 'cellglide-service.exe') { continue }
        $ids += [int]$p.ProcessId
    }
    return $ids
}

function Test-ReparsePoint($item) {
    $ErrorActionPreference = 'Stop'
    return [bool]($item.Attributes -band [IO.FileAttributes]::ReparsePoint)
}

function Get-ReparsePoints([string]$Dir) {
    # Every junction or symbolic link under $Dir, without following any of them.
    $ErrorActionPreference = 'Stop'
    foreach ($child in @(Get-ChildItem -LiteralPath $Dir -Force)) {
        if (Test-ReparsePoint $child) { $child.FullName }
        elseif ($child.PSIsContainer) { Get-ReparsePoints $child.FullName }
    }
}

function Refuse-ReparsePoint([string]$Path) {
    $ErrorActionPreference = 'Stop'
    throw ("$Path is a junction or symbolic link. CellGlide's data directory must hold " +
        'plain files and folders only; remove it (it may point anywhere) and run again.')
}

# ---- Pure checks ----

function Resolve-BaseUrl([string]$Param, [switch]$AllowLoopback) {
    # Production, or with -AllowLoopback a loopback test source (http, 127.0.0.1 or localhost,
    # an explicit port, nothing else); there is no environment variable.
    $ErrorActionPreference = 'Stop'
    $value = if ($Param) { $Param } else { $DefaultBase }
    if ($value.EndsWith('/')) { $value = $value.Substring(0, $value.Length - 1) }
    if ($value -ceq $DefaultBase) { return $value }
    if ($value -cmatch '\A(http)://(127\.0\.0\.1|localhost):(\d{1,5})\z') {
        $port = [int]$Matches[3]
        $uri = $null
        if ($port -ge 1 -and $port -le 65535 -and [Uri]::TryCreate($value, [UriKind]::Absolute, [ref]$uri) -and
            $uri.Scheme -ceq 'http' -and $uri.IsLoopback -and $uri.Host -cin @('127.0.0.1', 'localhost') -and
            $uri.Port -eq $port -and $uri.UserInfo -eq '' -and $uri.PathAndQuery -eq '/' -and $uri.Fragment -eq '') {
            if ($AllowLoopback) { return $value }
            throw "Unsupported download location: $value (a loopback test source needs -AllowLoopback)"
        }
    }
    throw "Unsupported download location: $value"
}

function ConvertTo-CellGlideArch([uint16]$Machine) {
    $ErrorActionPreference = 'Stop'
    switch ($Machine) {
        0x8664 { return 'x64' }
        0xAA64 { return 'arm64' }
    }
    return $null
}

function Test-WindowsSupported([int]$Build, [string]$Arch) {
    $ErrorActionPreference = 'Stop'
    $reason = $null
    if ($Build -lt 22000) { $reason = 'CellGlide needs Windows 11.' }
    elseif ($Arch -eq 'x64' -and $Build -lt 22621) { $reason = 'CellGlide needs Windows 11 22H2 or newer.' }
    elseif ($Arch -eq 'arm64' -and $Build -lt 26100) { $reason = 'CellGlide on ARM64 needs Windows 11 24H2 or newer.' }
    return [pscustomobject]@{ Ok = ($null -eq $reason); Reason = $reason }
}

function Test-SmartAppControlBlocks([int]$State) {
    # 1 = enforced (decision 8); 2 = evaluation and 0 = off install as usual.
    $ErrorActionPreference = 'Stop'
    return ($State -eq 1)
}

function Read-Latest([string]$Json, [string]$Arch, [string]$BaseUrl) {
    # latest.json (decision 14), checked field by field; the URL must name this version's file.
    $ErrorActionPreference = 'Stop'
    $data = $Json | ConvertFrom-Json
    $version = $data.version
    if (-not ($version -is [string]) -or $version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$') {
        throw "latest.json: bad version '$version'"
    }
    $entry = $data.$Arch
    if ($null -eq $entry) { throw "latest.json: no entry for $Arch" }
    $fileName = "CellGlide-$version-windows-$Arch.zip"
    $expected = "$BaseUrl/windows/$version/$fileName"
    if (-not ($entry.url -is [string]) -or $entry.url -cne $expected) {
        throw "latest.json: the $Arch url is '$($entry.url)', expected '$expected'"
    }
    if (-not ($entry.sha256 -is [string]) -or $entry.sha256 -notmatch '^[0-9a-fA-F]{64}$') {
        throw "latest.json: bad sha256 for $Arch"
    }
    if (-not ($entry.size -is [int] -or $entry.size -is [long]) -or $entry.size -le 0) {
        throw "latest.json: bad size for $Arch"
    }
    if (-not ($entry.test_build -is [bool])) { throw "latest.json: bad test_build for $Arch" }
    return [pscustomobject]@{
        Version = $version; Url = $entry.url; Sha256 = $entry.sha256.ToLowerInvariant()
        Size = [long]$entry.size; TestBuild = $entry.test_build; FileName = $fileName
    }
}

function Read-Sha256Sums([string]$Text, [string]$Require = '') {
    # "<hex>  <name>" or "<hex> *<name>" per line; file name -> lower-case hash. A malformed
    # line, a name listed twice, or (with -Require) a missing name is refused.
    $ErrorActionPreference = 'Stop'
    $sums = @{}
    foreach ($line in ($Text -split "`r?`n")) {
        if ($line.Trim() -eq '') { continue }
        if ($line -notmatch '\A([0-9a-fA-F]{64}) [ *](\S[^\r\n]*)\z') { throw "SHA256SUMS: bad line '$line'" }
        $name = $Matches[2]
        if ($sums.ContainsKey($name)) { throw "SHA256SUMS: duplicate entry for $name" }
        $sums[$name] = $Matches[1].ToLowerInvariant()
    }
    if ($Require -and -not $sums.ContainsKey($Require)) { throw "SHA256SUMS: no entry for $Require" }
    return $sums
}

function Test-Download([string]$Path, [string]$Expected) {
    $ErrorActionPreference = 'Stop'
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $false }
    return ((Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash -ieq $Expected)
}

function Get-InstallAction([string]$Installed, [string]$New) {
    $ErrorActionPreference = 'Stop'
    if (-not $Installed) { return 'Install' }
    try {
        $old = [version]($Installed -replace '-.*$', '')
        $next = [version]($New -replace '-.*$', '')
    } catch { return 'Upgrade' }
    if ($next -eq $old) { return 'Reinstall' }
    if ($next -gt $old) { return 'Upgrade' }
    return 'Downgrade'
}

function Install-Files([string]$Source, [string]$Target) {
    # Decision 6: copy to <Target>.new, move <Target> to <Target>.old, move .new in, delete
    # .old. A failure removes .new and puts .old back, so the old version stays usable.
    $ErrorActionPreference = 'Stop'
    $new = "$Target.new"
    $old = "$Target.old"
    $targetLeaf = Split-Path -Leaf $Target
    # Nothing here follows a link: a junction or symbolic link at or under any of the four
    # directories stops the install before anything is moved or deleted.
    foreach ($dir in @($Source, $Target, $new, $old)) {
        if (-not (Test-Path -LiteralPath $dir)) { continue }
        $item = Get-Item -LiteralPath $dir -Force
        if ((Test-ReparsePoint $item) -or ($item.PSIsContainer -and @(Get-ReparsePoints $dir).Count -gt 0)) {
            throw "$dir is or holds a junction or symbolic link; nothing was replaced."
        }
    }
    # An earlier run that died between the two renames left only .old: that is the old version.
    if ((Test-Path -LiteralPath $old) -and -not (Test-Path -LiteralPath $Target)) {
        Rename-Item -Path $old -NewName $targetLeaf
    }
    foreach ($leftover in @($new, $old)) {
        if (Test-Path -LiteralPath $leftover) { Remove-PathWithRetry $leftover }
    }
    try {
        Copy-Item -LiteralPath $Source -Destination $new -Recurse
        if (Test-Path -LiteralPath $Target) { Rename-Item -Path $Target -NewName (Split-Path -Leaf $old) }
        Rename-Item -Path $new -NewName $targetLeaf
    } catch {
        $failure = $_
        if (Test-Path -LiteralPath $new) { Remove-PathWithRetry $new }
        if ((Test-Path -LiteralPath $old) -and -not (Test-Path -LiteralPath $Target)) {
            Rename-Item -Path $old -NewName $targetLeaf
        }
        throw $failure
    }
    if (Test-Path -LiteralPath $old) { Remove-PathWithRetry $old }
}

function Get-UninstallEntry([string]$InstallDir, [string]$Version, [long]$SizeKb) {
    $ErrorActionPreference = 'Stop'
    return @{
        DisplayName = 'CellGlide'
        DisplayVersion = $Version
        Publisher = 'CellGlide'
        DisplayIcon = "$InstallDir\app\CellGlide.exe"
        InstallLocation = $InstallDir
        UninstallString = "`"$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe`" -NoProfile -ExecutionPolicy Bypass -File `"$InstallDir\uninstall.ps1`""
        URLInfoAbout = 'https://cellglide.com'
        NoModify = 1
        NoRepair = 1
        EstimatedSize = $SizeKb
    }
}

function Get-TranscriptBody([string[]]$Lines) {
    # A transcript's lines without the start and end banners Start-Transcript adds.
    $ErrorActionPreference = 'Stop'
    $body = New-Object System.Collections.Generic.List[string]
    $i = 0
    while ($i -lt $Lines.Count) {
        $line = $Lines[$i]
        if ($line -match '^\*{10,}$' -and ($i + 1) -lt $Lines.Count -and
            $Lines[$i + 1] -match '^Windows PowerShell transcript (start|end)$') {
            $i += 2
            while ($i -lt $Lines.Count -and $Lines[$i] -notmatch '^\*{10,}$') { $i++ }
            $i++
            continue
        }
        $body.Add($line)
        $i++
    }
    return $body.ToArray()
}

function New-ChildBootstrap([string]$ScriptPath, [string]$Sha256, [hashtable]$Parameters) {
    # The elevated child's whole program: read the script once, check its hash, run the bytes
    # that were checked from memory (dot-sourced, so the script's $code is ours), exit with it.
    # Exit 3 means the file was not the text the parent wrote. No value from outside ever
    # appears in the code: the path, the hash and the parameters travel as one base64 blob of
    # JSON (base64 has no quote of any kind), decoded inside. The command line holds 32767
    # characters, so -EncodedCommand (UTF-16 in base64, about 2.7 characters per character of
    # code) fails to start only with a -ZipPath of roughly 8300 characters or more: refused, safe.
    $ErrorActionPreference = 'Stop'
    $values = @{}
    foreach ($key in @($Parameters.Keys)) {
        $value = $Parameters[$key]
        if ($value -is [switch]) { $value = [bool]$value }
        elseif ($value -isnot [bool]) { $value = [string]$value }
        $values[[string]$key] = $value
    }
    $config = New-Object PSObject -Property @{ ScriptPath = $ScriptPath; Sha256 = $Sha256.ToLowerInvariant(); Parameters = $values }
    $blob = [Convert]::ToBase64String((New-Object Text.UTF8Encoding($false)).GetBytes(($config | ConvertTo-Json -Compress -Depth 4)))
    return @(
        "`$ErrorActionPreference = 'Stop'"
        "`$config = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$blob')) | ConvertFrom-Json"
        "`$bytes = [IO.File]::ReadAllBytes([string]`$config.ScriptPath)"
        "`$sha = [Security.Cryptography.SHA256]::Create()"
        "`$hash = -join (`$sha.ComputeHash(`$bytes) | ForEach-Object { `$_.ToString('x2') })"
        "if (`$hash -cne [string]`$config.Sha256) { exit 3 }"
        "`$sb = [scriptblock]::Create([Text.Encoding]::UTF8.GetString(`$bytes))"
        "`$p = @{}"
        "foreach (`$property in `$config.Parameters.PSObject.Properties) { `$p[`$property.Name] = `$property.Value }"
        "`$code = 1"
        ". `$sb @p"
        "exit `$code"
    ) -join "`n"
}

function Get-FileIdentity([string]$Path) {
    # Opens an existing file for writing and returns the stream with what the handle really is:
    # its final path (links and junctions resolved), its attributes, size and number of links.
    $ErrorActionPreference = 'Stop'
    if (-not ('CellGlideInstall.FileHandle' -as [type])) {
        Add-Type -Namespace CellGlideInstall -Name FileHandle -MemberDefinition @'
[StructLayout(LayoutKind.Sequential)]
public struct Info {
    public uint FileAttributes;
    public System.Runtime.InteropServices.ComTypes.FILETIME CreationTime;
    public System.Runtime.InteropServices.ComTypes.FILETIME LastAccessTime;
    public System.Runtime.InteropServices.ComTypes.FILETIME LastWriteTime;
    public uint VolumeSerialNumber;
    public uint FileSizeHigh;
    public uint FileSizeLow;
    public uint NumberOfLinks;
    public uint FileIndexHigh;
    public uint FileIndexLow;
}
[DllImport("kernel32.dll", SetLastError = true)]
public static extern bool GetFileInformationByHandle(Microsoft.Win32.SafeHandles.SafeFileHandle file, out Info info);
[DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern uint GetFinalPathNameByHandle(Microsoft.Win32.SafeHandles.SafeFileHandle file, System.Text.StringBuilder path, uint size, uint flags);
[DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern uint GetLongPathName(string shortPath, System.Text.StringBuilder longPath, uint size);
'@
    }
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $info = New-Object CellGlideInstall.FileHandle+Info
        if (-not [CellGlideInstall.FileHandle]::GetFileInformationByHandle($stream.SafeFileHandle, [ref]$info)) { throw "cannot read what $Path is" }
        # A final path longer than 1024 characters makes the call fail (it returns the size it
        # needs), so such a file is refused: that fails safe, and our paths are far shorter.
        $buffer = New-Object Text.StringBuilder 1024
        if ([CellGlideInstall.FileHandle]::GetFinalPathNameByHandle($stream.SafeFileHandle, $buffer, 1024, 0) -eq 0) { throw "cannot read the final path of $Path" }
        $final = $buffer.ToString()
        if ($final.StartsWith('\\?\UNC\')) { $final = '\\' + $final.Substring(8) }
        elseif ($final.StartsWith('\\?\')) { $final = $final.Substring(4) }
        $long = New-Object Text.StringBuilder 1024
        $expected = [IO.Path]::GetFullPath($Path)
        if ([CellGlideInstall.FileHandle]::GetLongPathName($expected, $long, 1024) -gt 0) { $expected = $long.ToString() }
        return [pscustomobject]@{
            Stream = $stream; FinalPath = $final; ExpectedPath = $expected; Attributes = [IO.FileAttributes]$info.FileAttributes
            Size = ([long]$info.FileSizeHigh -shl 32) -bor $info.FileSizeLow; Links = $info.NumberOfLinks
        }
    } catch {
        $stream.Dispose()
        throw
    }
}

function Copy-LogToParent([string]$Source, [string]$Dest) {
    # The elevated child's log, without the transcript banners, written into the empty file the
    # parent created in its %TEMP%. Nothing else is written to: the file must already exist,
    # be empty, have one link, not be a link, and its handle's final path must be the very path
    # the parent named (so no junction or symbolic link anywhere on the way).
    $ErrorActionPreference = 'Stop'
    if ((Split-Path -Leaf $Dest) -notmatch '\Acellglide-(install|uninstall)-[0-9a-f]{32}\.log\z') { throw "unexpected log path $Dest" }
    $file = Get-FileIdentity $Dest
    try {
        if ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "$Dest is a link" }
        if ($file.FinalPath -ine $file.ExpectedPath) { throw "$Dest leads to $($file.FinalPath)" }
        if ($file.Links -ne 1) { throw "$Dest has $($file.Links) links" }
        if ($file.Size -ne 0) { throw "$Dest is not the empty file the parent created" }
        $text = (Get-TranscriptBody @(Get-Content -LiteralPath $Source)) -join "`r`n"
        $bytes = (New-Object Text.UTF8Encoding($false)).GetBytes($text + "`r`n")
        $file.Stream.Write($bytes, 0, $bytes.Length)
    } finally { $file.Stream.Dispose() }
}

function ConvertTo-UncPath([string]$Path, [string]$DriveType, [string]$ProviderName) {
    # A path on a mapped network drive, as its UNC path: the elevated child does not see the
    # user's drive mappings. Other paths are returned as they are.
    $ErrorActionPreference = 'Stop'
    if ($DriveType -ne 'Network' -or $Path -notmatch '\A[A-Za-z]:\\') { return $Path }
    if ($ProviderName -notmatch '\A\\\\[^\\]+\\[^\\]+') {
        throw "$Path is on a network drive whose UNC path is unknown; copy the zip to a local folder and run again."
    }
    return $ProviderName.TrimEnd('\') + $Path.Substring(2)
}

function Get-PeMachine([string]$Path) {
    # The Machine field of a PE file's COFF header.
    $ErrorActionPreference = 'Stop'
    $bytes = [IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 64 -or $bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A) { throw "$Path is not a program" }
    $pe = [BitConverter]::ToInt32($bytes, 0x3C)
    if ($pe -lt 0 -or $pe + 6 -gt $bytes.Length -or [BitConverter]::ToUInt32($bytes, $pe) -ne 0x4550) {
        throw "$Path is not a program"
    }
    return [BitConverter]::ToUInt16($bytes, $pe + 4)
}

# ---- Steps with side effects ----

function Get-NativeMachine {
    # IsWow64Process2: x64 PowerShell emulated on ARM64 still gets 0xAA64.
    $ErrorActionPreference = 'Stop'
    if (-not ('CellGlideInstall.Native' -as [type])) {
        Add-Type -Namespace CellGlideInstall -Name Native -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true)]
public static extern bool IsWow64Process2(IntPtr process, out ushort processMachine, out ushort nativeMachine);
[DllImport("kernel32.dll")]
public static extern IntPtr GetCurrentProcess();
'@
    }
    [uint16]$processMachine = 0
    [uint16]$nativeMachine = 0
    if (-not [CellGlideInstall.Native]::IsWow64Process2([CellGlideInstall.Native]::GetCurrentProcess(),
            [ref]$processMachine, [ref]$nativeMachine)) {
        throw 'IsWow64Process2 failed'
    }
    return $nativeMachine
}

function Get-SmartAppControlState {
    $ErrorActionPreference = 'Stop'
    $value = (Get-ItemProperty -Path 'HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy' -Name 'VerifiedAndReputablePolicyState' -ErrorAction SilentlyContinue).VerifiedAndReputablePolicyState
    if ($null -eq $value) { return 0 }
    return [int]$value
}

function Test-AppleService {
    # The same two sources as "cellglide doctor": the classic service that iTunes and Apple
    # Mobile Device Support install, or the Store's Apple Devices app package.
    $ErrorActionPreference = 'Stop'
    $classic = "HKLM:\SYSTEM\CurrentControlSet\Services\$AppleService"
    if (Test-Path -LiteralPath $classic) {
        $svc = Get-Service -Name $AppleService -ErrorAction SilentlyContinue
        $state = if ($svc) { [string]$svc.Status } else { 'not registered' }
        Write-Host "Apple's device service is installed ($state)."
        return $true
    }
    $packages = 'HKLM:\SOFTWARE\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\PackageRepository\Packages'
    $store = @(Get-ChildItem -LiteralPath $packages -ErrorAction SilentlyContinue |
        Where-Object { $_.PSChildName -like 'AppleInc.AppleDevices_*_nzyj5cx40ttqa' })
    if ($store.Count -gt 0) {
        Write-Host "Apple's device service is installed (Apple Devices app)."
        return $true
    }
    return $false
}

function Request-AppleDevices([switch]$Unattended) {
    # Offers the Store's Apple Devices app; the user accepts its terms in winget, so no --accept-*.
    $ErrorActionPreference = 'Stop'
    Write-Host "Apple's device service is not installed. CellGlide reaches the iPhone through it, and it comes with the Apple Devices app from the Microsoft Store."
    $answer = 'n'
    if (-not $Unattended) { $answer = Read-Host 'Install Apple Devices from the Microsoft Store now? [y/N]' }
    if ($answer -match '^(y|yes)$') {
        if (Get-Command winget.exe -ErrorAction SilentlyContinue) {
            & winget.exe install --id $AppleStoreId --source msstore
            if ($LASTEXITCODE -eq 0) { return }
            Write-Host "winget did not install Apple Devices (exit $LASTEXITCODE)."
        } else {
            Write-Host 'winget is not available on this PC.'
        }
    }
    Write-Host "CellGlide will be installed anyway; it works once Apple Devices is installed: winget install --id $AppleStoreId --source msstore"
}

function Get-CellGlidePackage {
    # Downloads latest.json, the zip and SHA256SUMS into $WorkDir (a new folder); latest.json,
    # SHA256SUMS and the file itself must all agree on the hash.
    param(
        [string]$BaseUrl,
        [string]$Arch,
        [string]$WorkDir,
        [scriptblock]$Download = { param($Url, $OutFile) Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $OutFile }
    )
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    $protocols = [Net.ServicePointManager]::SecurityProtocol
    try {
        [Net.ServicePointManager]::SecurityProtocol = $protocols -bor [Net.SecurityProtocolType]::Tls12
        $null = New-Item -ItemType Directory -Path $WorkDir
        $latestFile = Join-Path $WorkDir 'latest.json'
        Write-Host "Downloading $BaseUrl/windows/latest.json"
        & $Download "$BaseUrl/windows/latest.json" $latestFile
        $latest = Read-Latest ([IO.File]::ReadAllText($latestFile)) $Arch $BaseUrl
        $zip = Join-Path $WorkDir $latest.FileName
        Write-Host "Downloading $($latest.Url)"
        & $Download $latest.Url $zip
        $sumsFile = Join-Path $WorkDir 'SHA256SUMS'
        & $Download "$BaseUrl/windows/$($latest.Version)/SHA256SUMS" $sumsFile
        $sums = Read-Sha256Sums ([IO.File]::ReadAllText($sumsFile)) $latest.FileName
        if ($sums[$latest.FileName] -ne $latest.Sha256 -or -not (Test-Download $zip $latest.Sha256)) {
            throw "SHA-256 mismatch for $($latest.FileName)"
        }
        if ((Get-Item -LiteralPath $zip).Length -ne $latest.Size) { throw "Size mismatch for $($latest.FileName)" }
        Write-Host "SHA-256 verified: $($latest.Sha256)"
        return [pscustomobject]@{ ZipPath = $zip; Version = $latest.Version; TestBuild = $latest.TestBuild }
    } finally {
        [Net.ServicePointManager]::SecurityProtocol = $protocols
    }
}

function Get-LatestTestBuild([string]$BaseUrl, [string]$Arch) {
    # Whether latest.json marks this architecture as a test build; true when it cannot be read.
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    $protocols = [Net.ServicePointManager]::SecurityProtocol
    try {
        [Net.ServicePointManager]::SecurityProtocol = $protocols -bor [Net.SecurityProtocolType]::Tls12
        $json = (Invoke-WebRequest -UseBasicParsing -Uri "$BaseUrl/windows/latest.json").Content
        return [bool](Read-Latest $json $Arch $BaseUrl).TestBuild
    } catch {
        return $true
    } finally {
        [Net.ServicePointManager]::SecurityProtocol = $protocols
    }
}

function Get-InstalledVersion {
    $ErrorActionPreference = 'Stop'
    $file = Join-Path $InstallDir 'version.txt'
    if (Test-Path -LiteralPath $file -PathType Leaf) { return ([IO.File]::ReadAllText($file)).Trim() }
    return ''
}

function New-PrivateWorkDir([string]$Root = (Split-Path -Parent $InstallDir)) {
    # A new folder next to the install directory (Program Files: only administrators can create
    # anything there, so nothing can be planted in advance), created in one step with an ACL of
    # SYSTEM and Administrators (full control) and nothing inherited, owned by Administrators.
    # Where the policy makes the user, not Administrators, the owner of what an elevated process
    # creates, the files inside would be owned by the user, whose implicit owner rights would let
    # an unelevated process change their ACL; an inherited OWNER RIGHTS entry that allows only
    # reading the ACL removes those rights. Nothing unelevated can change the package, the
    # extracted files or the log between the hash check and the install.
    $ErrorActionPreference = 'Stop'
    $dir = Join-Path $Root ('CellGlide.work-' + [guid]::NewGuid().ToString('N'))
    if (Test-Path -LiteralPath $dir) { throw "$dir already exists" }
    $inherit = [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [Security.AccessControl.InheritanceFlags]::ObjectInherit
    $security = New-Object Security.AccessControl.DirectorySecurity
    $security.SetAccessRuleProtection($true, $false)
    $security.SetOwner((New-Object Security.Principal.SecurityIdentifier('S-1-5-32-544')))
    $grants = @(
        @('S-1-5-18', [Security.AccessControl.FileSystemRights]::FullControl),
        @('S-1-5-32-544', [Security.AccessControl.FileSystemRights]::FullControl),
        @('S-1-3-4', [Security.AccessControl.FileSystemRights]::ReadPermissions))
    foreach ($grant in $grants) {
        $security.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule(
            (New-Object Security.Principal.SecurityIdentifier($grant[0])), $grant[1], $inherit,
            [Security.AccessControl.PropagationFlags]::None, [Security.AccessControl.AccessControlType]::Allow)))
    }
    $null = [IO.Directory]::CreateDirectory($dir, $security)
    $item = Get-Item -LiteralPath $dir -Force
    if (Test-ReparsePoint $item) { throw "$dir is a junction or symbolic link" }
    if (@(Get-ChildItem -LiteralPath $dir -Force).Count -gt 0) { throw "$dir is not empty" }
    $acl = Get-Acl -LiteralPath $dir
    $owner = $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
    $others = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]) | Where-Object {
        $_.IsInherited -or $_.AccessControlType -ne 'Allow' -or -not (
            ($_.IdentityReference.Value -in 'S-1-5-18', 'S-1-5-32-544') -or
            ($_.IdentityReference.Value -eq 'S-1-3-4' -and ($_.FileSystemRights -band -bnot [Security.AccessControl.FileSystemRights]::Synchronize) -eq [Security.AccessControl.FileSystemRights]::ReadPermissions)) })
    if (-not $acl.AreAccessRulesProtected -or $others.Count -gt 0 -or $owner -notin 'S-1-5-32-544', 'S-1-5-18') {
        throw "$dir did not get its private ACL"
    }
    return $dir
}

function Remove-StaleWorkDirs([string]$Keep, [string]$Root = (Split-Path -Parent $InstallDir), [int]$MinAgeMinutes = 60) {
    # Work folders left by an install that was killed: only those created at least $MinAgeMinutes
    # ago, never the current one ($Keep), never through a link. install.ps1 passes the default 60
    # (an install takes seconds, so a concurrent install's folder is not touched); uninstall.ps1
    # passes 0 to clear every work folder, which can remove a concurrent install's (acceptable:
    # the two only collide when an administrator runs install and uninstall at the same time).
    $ErrorActionPreference = 'Stop'
    $cutoff = (Get-Date).ToUniversalTime().AddMinutes(-$MinAgeMinutes)
    foreach ($stale in @(Get-ChildItem -LiteralPath $Root -Directory -Force -Filter 'CellGlide.work-*' -ErrorAction SilentlyContinue)) {
        if ($stale.FullName -eq $Keep -or $stale.CreationTimeUtc -gt $cutoff) { continue }
        if ((Test-ReparsePoint $stale) -or @(Get-ReparsePoints $stale.FullName).Count -gt 0) { continue }
        Remove-PathWithRetry $stale.FullName
    }
}

function Expand-Package([string]$Zip, [string]$Dest) {
    # Unpacks the zip into a new folder and checks it holds a CellGlide package and no links.
    $ErrorActionPreference = 'Stop'
    $null = New-Item -ItemType Directory -Path $Dest
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::ExtractToDirectory($Zip, $Dest)
    if (@(Get-ReparsePoints $Dest).Count -gt 0) { throw 'The package holds a junction or symbolic link' }
    foreach ($f in 'version.txt', 'cellglide-service.exe', 'cellglide.exe', 'wintun.dll', 'uninstall.ps1', 'app\CellGlide.exe') {
        if (-not (Test-Path -LiteralPath (Join-Path $Dest $f) -PathType Leaf)) { throw "The package has no $f" }
    }
}

function Stop-CellGlideProcesses {
    $ErrorActionPreference = 'Stop'
    $ids = Select-ProcessesToStop @(Get-CimInstance -ClassName Win32_Process) $InstallDir
    foreach ($id in $ids) {
        Write-Host "Stopping process $id"
        Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
    }
}

function Set-OurService {
    $ErrorActionPreference = 'Stop'
    $binPath = '"' + (Join-Path $InstallDir 'cellglide-service.exe') + '"'
    if (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue) {
        # Not sc.exe config: Windows PowerShell 5.1 drops the embedded quotes when passing
        # binPath= to a native program, leaving an unquoted path with spaces (the
        # unquoted-service-path hole). Win32_Service.Change takes the string as is.
        $svc = Get-CimInstance -ClassName Win32_Service -Filter "Name='$ServiceName'"
        $r = Invoke-CimMethod -InputObject $svc -MethodName Change -Arguments @{
            PathName = $binPath; StartMode = 'Automatic'; DisplayName = $ServiceName;
            StartName = 'LocalSystem'; StartPassword = ''
        }
        if ($r.ReturnValue -ne 0) { throw "Win32_Service.Change failed (return $($r.ReturnValue))" }
        Write-Host "Updated service $ServiceName"
    } else {
        New-Service -Name $ServiceName -BinaryPathName $binPath -StartupType Automatic `
            -DisplayName $ServiceName | Out-Null
        Write-Host "Created service $ServiceName"
    }
    Invoke-Native 'sc.exe' @('failure', $ServiceName, 'reset=', '86400',
        'actions=', 'restart/2000/restart/5000/restart/30000') | Out-Null
    # NT SERVICE\CellGlide resolves only once the service has an unrestricted SID.
    Invoke-Native 'sc.exe' @('sidtype', $ServiceName, 'unrestricted') | Out-Null
    # Minimal privilege set, measured on a real iPhone in W1 Task 14 Step 3.
    Invoke-Native 'sc.exe' @('privs', $ServiceName,
        'SeChangeNotifyPrivilege/SeImpersonatePrivilege/SeLoadDriverPrivilege/SeCreateGlobalPrivilege/SeAuditPrivilege') | Out-Null
}

function Lock-DataItem($item) {
    # Takes ownership of one file or folder and resets its ACL to what it inherits, never
    # recursing (icacls /T would follow a junction). Checked before and after, so a link
    # swapped in between is caught: once locked, its owner can no longer change it.
    $ErrorActionPreference = 'Stop'
    if (Test-ReparsePoint $item) { Refuse-ReparsePoint $item.FullName }
    Invoke-Native 'icacls.exe' @($item.FullName, '/setowner', $SidAdmins, '/C', '/Q') | Out-Null
    Invoke-Native 'icacls.exe' @($item.FullName, '/reset', '/C', '/Q') | Out-Null
    if (Test-ReparsePoint (Get-Item -LiteralPath $item.FullName -Force)) {
        Refuse-ReparsePoint $item.FullName
    }
}

function Lock-DataTree([string]$Dir) {
    # Top-down: a folder is locked before its entries are listed, so nobody else can add a
    # link to it while the rest is done.
    $ErrorActionPreference = 'Stop'
    foreach ($child in @(Get-ChildItem -LiteralPath $Dir -Force)) {
        Lock-DataItem $child
        if ($child.PSIsContainer) { Lock-DataTree $child.FullName }
    }
}

function Set-DataDirAcl {
    # Root: no inheritance from ProgramData; SYSTEM and Administrators full control, the
    # service SID modify. health\ adds Users read (the GUI reads health files). logs\ keeps
    # only the inherited entries: users must not be able to plant files the service reads.
    # Anyone can create folders in ProgramData, so a pre-existing CellGlide folder may hold
    # anything, links included: the root is locked first, then each entry in turn, and a
    # junction or symbolic link anywhere stops the install before any ACL goes through it.
    $ErrorActionPreference = 'Stop'
    $health = Join-Path $DataDir 'health'
    $logs = Join-Path $DataDir 'logs'
    if (Test-Path -LiteralPath $DataDir) {
        $root = Get-Item -LiteralPath $DataDir -Force
        if (Test-ReparsePoint $root) { Refuse-ReparsePoint $DataDir }
    } else {
        New-Item -ItemType Directory -Path $DataDir | Out-Null
    }
    Invoke-Native 'icacls.exe' @($DataDir, '/setowner', $SidAdmins, '/C', '/Q') | Out-Null
    Invoke-Native 'icacls.exe' @($DataDir, '/inheritance:r', '/Q',
        '/grant:r', "${SidSystem}:(OI)(CI)F",
        '/grant:r', "${SidAdmins}:(OI)(CI)F",
        '/grant:r', "NT SERVICE\${ServiceName}:(OI)(CI)M") | Out-Null
    if (Test-ReparsePoint (Get-Item -LiteralPath $DataDir -Force)) { Refuse-ReparsePoint $DataDir }
    # Only administrators can add to the root now; created here, they are locked below too.
    New-Item -ItemType Directory -Force -Path $health, $logs | Out-Null
    Lock-DataTree $DataDir
    Invoke-Native 'icacls.exe' @($health, '/Q', '/grant:r', "${SidUsers}:(OI)(CI)RX") | Out-Null
}

function New-StartMenuShortcut {
    $ErrorActionPreference = 'Stop'
    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut($Shortcut)
    $link.TargetPath = Join-Path $InstallDir 'app\CellGlide.exe'
    $link.WorkingDirectory = Join-Path $InstallDir 'app'
    $link.IconLocation = (Join-Path $InstallDir 'app\CellGlide.exe') + ',0'
    $link.Description = 'CellGlide'
    $link.Save()
    $null = [Runtime.InteropServices.Marshal]::ReleaseComObject($shell)
    Write-Host "Created $Shortcut"
}

function Set-UninstallEntry([string]$Version) {
    $ErrorActionPreference = 'Stop'
    $bytes = (Get-ChildItem -LiteralPath $InstallDir -Recurse -Force -File | Measure-Object -Property Length -Sum).Sum
    $entry = Get-UninstallEntry $InstallDir $Version ([long][math]::Ceiling($bytes / 1KB))
    if (-not (Test-Path -LiteralPath $UninstallKey)) { $null = New-Item -Path $UninstallKey }
    foreach ($name in $entry.Keys) {
        $type = if ($name -in 'NoModify', 'NoRepair', 'EstimatedSize') { 'DWord' } else { 'String' }
        $null = New-ItemProperty -LiteralPath $UninstallKey -Name $name -Value $entry[$name] -PropertyType $type -Force
    }
    Write-Host 'Added CellGlide to Settings > Apps > Installed apps'
}

function Start-AppAsUser {
    # An elevated process starts the app through explorer.exe, so it runs as the signed-in
    # user without administrator rights. An elevated token that is not an administrator's counts
    # too (Test-TokenElevated); when that cannot be read, explorer.exe is used, which is safe.
    $ErrorActionPreference = 'Stop'
    $exe = Join-Path $InstallDir 'app\CellGlide.exe'
    $tokenElevated = $true
    try { $tokenElevated = Test-TokenElevated } catch { }
    if ((Test-Elevated) -or $tokenElevated) { Start-Process -FilePath "$env:SystemRoot\explorer.exe" -ArgumentList "`"$exe`"" }
    else { Start-Process -FilePath $exe }
}

function Invoke-Doctor {
    # Prints "cellglide doctor"; failed checks do not change the exit code. cellglide.exe writes
    # UTF-8 into a pipe, but PowerShell would decode a native program's output with the
    # console's encoding, which in an ordinary window is the OEM code page (437 shows a check
    # mark as three wrong characters). So the output is read here as UTF-8 directly, and the
    # console's own settings are never touched.
    # stdout and stderr come through two pipes, so stderr's lines (doctor writes none today) are
    # printed after stdout's rather than interleaved. Blank lines are kept. Doctor gets 60 s, then
    # it is stopped. Its exit code is ignored on purpose, and since it runs through
    # Process.Start, it no longer leaves $LASTEXITCODE behind in the user's session.
    $ErrorActionPreference = 'Continue'
    Write-Host 'cellglide doctor:'
    $p = $null
    try {
        $utf8 = New-Object Text.UTF8Encoding($false)
        $psi = New-Object Diagnostics.ProcessStartInfo((Join-Path $InstallDir 'cellglide.exe'), 'doctor')
        $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
        $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
        $psi.StandardOutputEncoding = $utf8; $psi.StandardErrorEncoding = $utf8
        $p = [Diagnostics.Process]::Start($psi)
        $err = $p.StandardError.ReadToEndAsync()
        $out = $p.StandardOutput.ReadToEndAsync()
        if (-not $p.WaitForExit(60000)) {
            try { $p.Kill() } catch { }
            Write-Host '  (cellglide doctor did not finish within 60 s and was stopped)'
            return
        }
        $text = ($out.Result + $err.Result) -replace "`r?`n\z", ''
        if ($text -ne '') { foreach ($line in ($text -split "`r?`n")) { Write-Host "  $line" } }
    } catch {
        Write-Host "  (cellglide doctor could not run: $($_.Exception.Message))"
    } finally {
        if ($p) { $p.Dispose() }
    }
}

# ---- Elevation ----

function Invoke-ElevatedChild {
    # Writes this script's own text to %TEMP% and starts the elevated child with a bootstrap
    # (-EncodedCommand) that runs that text only if its SHA-256 is the one computed here; then
    # prints the log the child left for us. $Start gets the encoded bootstrap and, for tests,
    # the paths and the hash.
    param(
        [string]$ScriptText,
        [hashtable]$Parameters,
        [scriptblock]$Start = { param($EncodedCommand, $Info)
            $p = Start-Process powershell.exe -Verb RunAs -PassThru -WindowStyle Hidden `
                -ArgumentList "-NoProfile -ExecutionPolicy Bypass -EncodedCommand $EncodedCommand"
            if ($p) { $null = $p.Handle }
            $p }
    )
    $ErrorActionPreference = 'Stop'
    if ([string]::IsNullOrEmpty($ScriptText)) { throw "Cannot find the running script's text" }
    $id = [guid]::NewGuid().ToString('N')
    $scriptFile = Join-Path ([IO.Path]::GetTempPath()) "cellglide-install-$id.ps1"
    $logFile = Join-Path ([IO.Path]::GetTempPath()) "cellglide-install-$id.log"
    $bytes = (New-Object Text.UTF8Encoding($false)).GetBytes($ScriptText)
    $sha = [Security.Cryptography.SHA256]::Create()
    $hash = -join ($sha.ComputeHash($bytes) | ForEach-Object { $_.ToString('x2') })
    [IO.File]::WriteAllBytes($scriptFile, $bytes)
    try {
        # The empty file the child writes its log into; it never creates a file of its own here.
        [IO.File]::Open($logFile, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None).Dispose()
        $childParams = @{}
        if ($Parameters) { foreach ($key in @($Parameters.Keys)) { $childParams[$key] = $Parameters[$key] } }
        $childParams['ElevatedPart'] = $true
        $childParams['LogPath'] = $logFile
        $bootstrap = New-ChildBootstrap $scriptFile $hash $childParams
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($bootstrap))
        $info = @{ ScriptPath = $scriptFile; LogPath = $logFile; Sha256 = $hash; Bootstrap = $bootstrap }
        $proc = $null
        try { $proc = & $Start $encoded $info } catch { $proc = $null }
        if (-not $proc) {
            Write-Host 'Installation cancelled.'
            return 1
        }
        $proc.WaitForExit()
        $code = [int]$proc.ExitCode
        $hadLog = (Test-Path -LiteralPath $logFile) -and (Get-Item -LiteralPath $logFile).Length -gt 0
        if ($hadLog) {
            foreach ($line in @(Get-Content -LiteralPath $logFile)) { Write-Host $line }
        }
        if ($code -eq 3) {
            Write-Host 'Installation failed: the copy of the installer in %TEMP% changed before the elevated part could run it.'
            return 1
        }
        if ($code -ne 0 -and -not $hadLog) {
            Write-Host "Installation failed: the elevated part ended with exit code $code and left no log."
        }
        return $code
    } finally {
        Remove-Item -LiteralPath $scriptFile, $logFile -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-ElevatedPart([string]$BaseUrl, [string]$ZipPath, [string]$Arch, [string]$WorkDir = '') {
    # Steps 4-8 and starting the service. Nothing changes before the package is verified. On a
    # failure only the service this run stopped is started again, and only if it was running;
    # a service this run created, or one already stopped, stays stopped, and nothing is started
    # when the data directory's ACL step refused.
    $ErrorActionPreference = 'Stop'
    $ownWork = -not $WorkDir
    $stoppedByUs = $false
    $step = 'prepare'
    $before = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
    $wasRunning = [bool]($before -and $before.Status -eq 'Running')
    try {
        if ($Arch -cnotin @('x64', 'arm64')) { throw "unsupported architecture '$Arch'" }
        if ($ownWork) { $WorkDir = New-PrivateWorkDir }
        if ($ZipPath) {
            if (-not (Test-Path -LiteralPath $ZipPath -PathType Leaf)) {
                throw "cannot find $ZipPath from the elevated part; use a local path or a UNC path"
            }
            $zip = Join-Path $WorkDir (Split-Path -Leaf $ZipPath)
            Copy-Item -LiteralPath $ZipPath -Destination $zip
            Write-Host "Package: $ZipPath"
            Write-Host "SHA-256: $((Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant())"
            Write-Host "Compare it with SHA256SUMS at $DefaultBase/windows/<version>/SHA256SUMS."
            $expected = ''
        } else {
            $package = Get-CellGlidePackage $BaseUrl $Arch (Join-Path $WorkDir 'download')
            $zip = $package.ZipPath
            $expected = $package.Version
        }
        $staged = Join-Path $WorkDir 'package'
        Expand-Package $zip $staged
        $version = ([IO.File]::ReadAllText((Join-Path $staged 'version.txt'))).Trim()
        if ($expected -and $version -ne $expected) {
            throw "the package is version $version, latest.json says $expected"
        }
        $wantMachine = if ($Arch -eq 'arm64') { 0xAA64 } else { 0x8664 }
        if ((Get-PeMachine (Join-Path $staged 'cellglide-service.exe')) -ne $wantMachine) {
            throw "the package is not the $Arch build"
        }
        $installed = Get-InstalledVersion
        switch (Get-InstallAction $installed $version) {
            'Install' { Write-Host "Installing $version" }
            'Upgrade' { Write-Host "Upgrading $installed -> $version" }
            'Reinstall' { Write-Host "Reinstalling $version" }
            'Downgrade' { Write-Host "Downgrading $installed -> $version" }
        }

        if (-not (Test-Elevated)) { throw 'the elevated part needs administrator rights' }
        $step = 'stop'
        Remove-StaleWorkDirs $WorkDir
        Stop-CellGlideProcesses
        $stoppedByUs = $wasRunning
        if (-not (Stop-OurService)) { throw "$AdapterName is still present 10 seconds after stopping the service" }
        $step = 'files'
        Install-Files $staged $InstallDir
        Write-Host "Installed files in $InstallDir"
        $step = 'service'
        Set-OurService
        $step = 'acl'
        Set-DataDirAcl
        Write-Host "Prepared $DataDir"
        $step = 'entries'
        New-StartMenuShortcut
        Set-UninstallEntry $version
        $step = 'start'
        Start-Service -Name $ServiceName
        (Get-Service -Name $ServiceName).WaitForStatus('Running', [TimeSpan]::FromSeconds(30))
        Write-Host "Service $ServiceName is running."
        return 0
    } catch {
        $reason = $_.Exception.Message
        if ($stoppedByUs -and $step -ne 'acl' -and $step -ne 'start') {
            try { Start-Service -Name $ServiceName; Write-Host "Started the $ServiceName service again." }
            catch { Write-Host "Could not start the $ServiceName service again: $($_.Exception.Message)" }
        }
        Write-Host "Installation failed: $reason"
        return 1
    } finally {
        if ($ownWork -and $WorkDir -and (Test-Path -LiteralPath $WorkDir)) { Remove-PathWithRetry $WorkDir }
    }
}

function Resolve-ZipPath([string]$Path) {
    # The full path of -ZipPath, with a mapped network drive turned into its UNC path.
    $ErrorActionPreference = 'Stop'
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "$Path does not exist" }
    $full = (Resolve-Path -LiteralPath $Path).ProviderPath
    if ($full -match '\A([A-Za-z]):\\') {
        $letter = $Matches[1]
        $drive = New-Object IO.DriveInfo($letter)
        $provider = ''
        if ([string]$drive.DriveType -eq 'Network') {
            $provider = [string](Get-CimInstance -ClassName Win32_LogicalDisk -Filter "DeviceID='${letter}:'").ProviderName
        }
        $full = ConvertTo-UncPath $full ([string]$drive.DriveType) $provider
    }
    return $full
}

# ---- Main ----

function Invoke-CellGlideInstall {
    param(
        [string]$BaseUrl = '',
        [string]$ZipPath = '',
        [switch]$Unattended,
        [switch]$ElevatedPart,
        [string]$Arch = '',
        [string]$LogPath = '',
        [switch]$AllowLoopback
    )
    $ErrorActionPreference = 'Stop'
    try {
        if ($ElevatedPart) {
            # The elevated child: everything, the log included, stays in the private folder; the
            # parent gets a copy of the log when the work is done.
            $work = New-PrivateWorkDir
            $log = Join-Path $work 'install.log'
            $null = Start-Transcript -LiteralPath $log -NoClobber
            try {
                return (Invoke-ElevatedPart (Resolve-BaseUrl $BaseUrl -AllowLoopback:$AllowLoopback) $ZipPath $Arch $work)
            } catch {
                Write-Host "Installation failed: $($_.Exception.Message)"
                return 1
            } finally {
                $null = Stop-Transcript
                if ($LogPath) { try { Copy-LogToParent $log $LogPath } catch { } }
                Remove-PathWithRetry $work
            }
        }

        # Steps 1-3 in the user's own process.
        $base = Resolve-BaseUrl $BaseUrl -AllowLoopback:$AllowLoopback
        if ($base -cne $DefaultBase) { Write-Host "Downloading from a local test source: $base" }
        if ($ZipPath) { $ZipPath = Resolve-ZipPath $ZipPath }
        $machine = Get-NativeMachine
        $arch = ConvertTo-CellGlideArch $machine
        if (-not $arch) {
            Write-Host ('CellGlide runs on x64 and ARM64 Windows only (this PC is 0x{0:X4}).' -f $machine)
            return 2
        }
        $check = Test-WindowsSupported ([Environment]::OSVersion.Version.Build) $arch
        if (-not $check.Ok) {
            Write-Host $check.Reason
            return 2
        }
        if (Test-SmartAppControlBlocks (Get-SmartAppControlState)) {
            Write-Host 'Smart App Control is on in enforcement mode. It blocks programs that are not signed, and this CellGlide build is not signed yet, so it could not run. Nothing was changed.'
            Write-Host 'A signed CellGlide will follow; see https://cellglide.com.'
            return 2
        }
        if (-not (Test-AppleService)) { Request-AppleDevices -Unattended:$Unattended }
        if ($arch -eq 'arm64' -and ($ZipPath -or (Get-LatestTestBuild $base $arch))) {
            Write-Host 'Note: the ARM64 build is a test build, not yet tested on real hardware.'
        }
        Write-Host $EndUserNotice
        Write-Host "(That file is THIRD-PARTY-LICENSES.txt in $InstallDir.)"

        # Steps 4-8, elevated.
        if (Test-Elevated) {
            $code = Invoke-ElevatedPart $base $ZipPath $arch
        } else {
            $childParams = @{ BaseUrl = $base; Arch = $arch; AllowLoopback = [bool]$AllowLoopback }
            if ($ZipPath) { $childParams['ZipPath'] = $ZipPath }
            $code = Invoke-ElevatedChild $CellGlideInstallText $childParams
        }
        if ($code -ne 0) { return $code }

        # Steps 9-10 back in the user's process.
        $version = Get-InstalledVersion
        if (-not $Unattended) { Start-AppAsUser }
        Invoke-Doctor
        Write-Host "CellGlide $version is installed. Uninstall it from Settings > Apps > Installed apps, or run: irm https://pkg.cellglide.com/uninstall.ps1 | iex"
        return 0
    } catch {
        Write-Host "Installation failed: $($_.Exception.Message)"
        return 1
    }
}

$code = Invoke-CellGlideInstall -BaseUrl $BaseUrl -ZipPath $ZipPath -Unattended:$Unattended -ElevatedPart:$ElevatedPart -Arch $Arch -LogPath $LogPath -AllowLoopback:$AllowLoopback
if ($PSCommandPath) { exit $code }
