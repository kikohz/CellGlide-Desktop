<#
Uninstalls CellGlide from Windows 11.

  irm https://pkg.cellglide.com/uninstall.ps1 | iex
  powershell -NoProfile -ExecutionPolicy Bypass -File "C:\Program Files\CellGlide\uninstall.ps1" [-Purge]

Settings > Apps > Installed apps runs the second line. The work is done elevated (one UAC
prompt), and never by the copy inside C:\Program Files\CellGlide, which is deleted: in an
ordinary window, or when this copy lives in the install folder, the script writes its own text
to %TEMP% and starts an elevated child with a small -EncodedCommand bootstrap that checks the
text's SHA-256 and runs it from memory (as install.ps1 does); the child's log is shown here
afterwards. Run through iex, the script runs again in a child scope and leaves nothing in the
session.

In order, each step best-effort (a failed step is reported and the rest still run; the exit
code is then 1): the app and CLI running from the install folder are closed, the CellGlide
service is stopped (cellglide0 goes with it) and deleted, the CellGlide Run value of every
signed-in user that starts our app (HKCU and the user hives Windows already has loaded; no
other hive is loaded) is removed, then the Start menu shortcut, the Installed-apps entry,
C:\Program Files\CellGlide (and a leftover CellGlide.new, CellGlide.old or CellGlide.work-*),
cellglide0's Tcpip and Tcpip6 interface keys (only its fixed GUID, after the service is gone)
and a phantom cellglide0 device node.

With -Purge, C:\ProgramData\CellGlide and the invoking user's own %LOCALAPPDATA%\CellGlide are
deleted too, and never through a junction or symbolic link. The two are removed by different
processes on purpose: an elevated process must never delete inside a tree a standard user can
write to, or that user could swap a junction in and redirect the delete elsewhere.
C:\ProgramData\CellGlide has an admin/SYSTEM-only ACL (set by install.ps1), which is verified
first and the purge refuses it if a non-administrator can write there, so only the elevated
part touches it. %LOCALAPPDATA%\CellGlide is under the user's own profile, so it is removed by
the unelevated parent running as that user, after the elevated part finishes; a junction swap
gains nothing, because the user can already delete whatever their own token can delete.

Known limits: only the invoking user's %LOCALAPPDATA%\CellGlide is removed; other accounts keep
their own %LOCALAPPDATA%\CellGlide, and a signed-in Microsoft/Entra ID user (SID S-1-12-1-*)
keeps both their startup entry and their %LOCALAPPDATA%\CellGlide (only classic local accounts,
SID S-1-5-21-*, are handled). Nothing else is touched: no other adapter, DNS setting, route,
service or device. Exit codes: 0 uninstalled, 1 a step failed or the uninstall was cancelled.
#>
param(
    # Under iex the param block binds into the caller's own scope. This default runs before any
    # parameter is set (switches are bound before a later default runs, so this one comes
    # first) and keeps copies of the caller's variables with these names for the iex guard.
    [string]$LogPath = $(Set-Variable -Name __CellGlideCallerVars -Value @(
        Get-Variable -Name '__CellGlideCallerVars', 'LogPath', 'Purge', 'ElevatedPart', 'CellGlideUninstallText' -Scope 0 -ErrorAction SilentlyContinue |
            ForEach-Object { New-Object Management.Automation.PSVariable -ArgumentList $_.Name, $_.Value, $_.Options, (New-Object 'Collections.ObjectModel.Collection[Attribute]' (, [Attribute[]]@($_.Attributes))) }); ''),
    [switch]$Purge,
    [switch]$ElevatedPart
)

# This script's own full text, whether it runs with -File, through iex or from the elevated
# bootstrap: the elevated child runs exactly this text, never a download.
$CellGlideUninstallText = { }.Ast.Extent.StartScriptPosition.GetFullScript()
if (-not ($MyInvocation.MyCommand.ScriptBlock -and $MyInvocation.MyCommand.ScriptBlock.ToString() -ceq $CellGlideUninstallText)) {
    # Run through iex, in the caller's own scope: run the same text again in a child scope, then
    # (from a child scope, so nothing new is defined here) remove what the param block and the
    # line above created and give the caller's own variables of those names their values back.
    $null = & ([scriptblock]::Create($CellGlideUninstallText))
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
    } @($__CellGlideCallerVars) @('__CellGlideCallerVars', 'LogPath', 'Purge', 'ElevatedPart', 'CellGlideUninstallText')
    return
}

$InstallDir = 'C:\Program Files\CellGlide'
$ServiceName = 'CellGlide'
$DataDir = "$env:ProgramData\CellGlide"
$UninstallKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\CellGlide'
$Shortcut = "$env:ProgramData\Microsoft\Windows\Start Menu\Programs\CellGlide.lnk"
$AdapterName = 'cellglide0'
# cg_core::tunnel::WINDOWS_ADAPTER_GUID; wintun uses it as the device instance id.
$AdapterGuid = '{E490EEF2-2CB0-4A80-B341-BF929C2E56B3}'
$AdapterInstanceId = "SWD\Wintun\$AdapterGuid"
# The app's own start-at-login value (CellGlide.Core Autostart.ValueName) and where it lives.
$RunValueName = 'CellGlide'
$RunKeyPath = 'Software\Microsoft\Windows\CurrentVersion\Run'
$ProfileListKey = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList'
$ServicesKey = 'HKLM:\SYSTEM\CurrentControlSet\Services'

# ---- Shared with install.ps1 (decision 3: kept identical; tests\Shared.Tests.ps1) ----

function Test-Elevated {
    $ErrorActionPreference = 'Stop'
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    return ([Security.Principal.WindowsPrincipal]$id).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
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
    # JSON (base64 has no quote of any kind), decoded inside.
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

function Stop-CellGlideProcesses {
    $ErrorActionPreference = 'Stop'
    $ids = Select-ProcessesToStop @(Get-CimInstance -ClassName Win32_Process) $InstallDir
    foreach ($id in $ids) {
        Write-Host "Stopping process $id"
        Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
    }
}

# ---- Pure checks ----

function Test-OurRunValue([string]$Data, [string]$ExePath) {
    # True when the Run value's program (quotes and arguments removed) is $ExePath, in any case.
    $ErrorActionPreference = 'Stop'
    if ([string]::IsNullOrWhiteSpace($Data) -or [string]::IsNullOrWhiteSpace($ExePath)) { return $false }
    $text = $Data.Trim()
    if ($text.StartsWith('"')) {
        $end = $text.IndexOf('"', 1)
        if ($end -lt 0) { return $false }
        $program = $text.Substring(1, $end - 1)
    } elseif ($text.Equals($ExePath, [StringComparison]::OrdinalIgnoreCase) -or
              $text.StartsWith($ExePath + ' ', [StringComparison]::OrdinalIgnoreCase)) {
        # Unquoted, with spaces in the path: only our own exact path counts.
        $program = $ExePath
    } else {
        $program = ($text -split '\s+', 2)[0]
    }
    return $program.Equals($ExePath, [StringComparison]::OrdinalIgnoreCase)
}

function Test-ProfileDataPath([string]$Path, [string]$ProfilesRoot) {
    # True when $Path, made absolute, lies under the profiles folder (C:\Users) and has no "..".
    $ErrorActionPreference = 'Stop'
    if ($Path -notmatch '\A[A-Za-z]:\\' -or $ProfilesRoot -notmatch '\A[A-Za-z]:\\') { return $false }
    if (@($Path -split '[\\/]') -contains '..') { return $false }
    try {
        $full = [IO.Path]::GetFullPath($Path)
        $root = [IO.Path]::GetFullPath($ProfilesRoot).TrimEnd('\')
    } catch { return $false }
    if ($root.Length -le 2) { return $false }
    return $full.StartsWith($root + '\', [StringComparison]::OrdinalIgnoreCase) -and $full.Length -gt $root.Length + 1
}

function Test-DataDirAdminOnly([string]$Dir) {
    # True when $Dir grants write-class rights only to SYSTEM, the service accounts and
    # Administrators, so no standard user can plant a junction in it before the elevated purge.
    # Any Allow ACE that lets some other principal create, write, delete or re-permission fails.
    $ErrorActionPreference = 'Stop'
    $acl = Get-Acl -LiteralPath $Dir
    $readOnly = [Security.AccessControl.FileSystemRights]'ReadAndExecute, ReadPermissions, Synchronize'
    foreach ($ace in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
        if ($ace.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow) { continue }
        $sid = $ace.IdentityReference.Value
        # SYSTEM, LocalService, NetworkService, Administrators, and service SIDs (S-1-5-80-*): a
        # standard user cannot assume any of these, nor set an ACE for one on a folder they do
        # not control, so their write access is not a way in.
        if ($sid -in 'S-1-5-18', 'S-1-5-19', 'S-1-5-20', 'S-1-5-32-544' -or $sid -like 'S-1-5-80-*') { continue }
        if (([int]$ace.FileSystemRights -band -bnot [int]$readOnly) -ne 0) { return $false }
    }
    return $true
}

function Get-CanonicalPath([string]$Path) {
    # $Path with 8.3 short names expanded and links resolved, so a prefix comparison is not
    # fooled by C:\PROGRA~1 or a symbolic link pointing into the install folder. A path that
    # exists is resolved by its handle's final path; one that does not is expanded as far as
    # GetLongPathName can take it.
    $ErrorActionPreference = 'Stop'
    if (-not $Path) { return '' }
    if (-not ('CellGlideUninstall.PathApi' -as [type])) {
        Add-Type -Namespace CellGlideUninstall -Name PathApi -MemberDefinition @'
[DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern uint GetLongPathName(string path, System.Text.StringBuilder buffer, uint size);
[DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern System.IntPtr CreateFileW(string path, uint access, uint share, System.IntPtr sec, uint disposition, uint flags, System.IntPtr template);
[DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern uint GetFinalPathNameByHandleW(System.IntPtr handle, System.Text.StringBuilder buffer, uint size, uint flags);
[DllImport("kernel32.dll", SetLastError = true)]
public static extern bool CloseHandle(System.IntPtr handle);
'@
    }
    $full = [IO.Path]::GetFullPath($Path)
    if (Test-Path -LiteralPath $full) {
        $handle = [CellGlideUninstall.PathApi]::CreateFileW($full, 0, 7, [IntPtr]::Zero, 3, 0x02000000, [IntPtr]::Zero)
        if ($handle -ne [IntPtr](-1)) {
            try {
                $buffer = New-Object Text.StringBuilder 1024
                if ([CellGlideUninstall.PathApi]::GetFinalPathNameByHandleW($handle, $buffer, 1024, 0) -gt 0) {
                    $final = $buffer.ToString()
                    if ($final.StartsWith('\\?\UNC\')) { return '\\' + $final.Substring(8) }
                    if ($final.StartsWith('\\?\')) { return $final.Substring(4) }
                    return $final
                }
            } finally { [void][CellGlideUninstall.PathApi]::CloseHandle($handle) }
        }
    }
    $buffer = New-Object Text.StringBuilder 1024
    if ([CellGlideUninstall.PathApi]::GetLongPathName($full, $buffer, 1024) -gt 0) { return $buffer.ToString() }
    return $full
}

function Test-NeedsRelocation([string]$ScriptPath, [string]$InstallDir, [bool]$Elevated) {
    # The work must run elevated, and never from the copy in the folder it deletes. Both paths
    # are canonicalized first, so a short (8.3) path or a link into the install folder is caught.
    $ErrorActionPreference = 'Stop'
    if (-not $Elevated) { return $true }
    if (-not $ScriptPath) { return $false }
    $script = Get-CanonicalPath $ScriptPath
    $dir = (Get-CanonicalPath $InstallDir).TrimEnd('\') + '\'
    return $script.StartsWith($dir, [StringComparison]::OrdinalIgnoreCase)
}

# ---- Steps with side effects ----

function Get-LoadedUserHives {
    # HKCU, and the hive of every other signed-in user that Windows already has loaded. No hive
    # is ever loaded here: a user who is not signed in keeps a value that does nothing.
    $ErrorActionPreference = 'Stop'
    $own = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    $hives = @('HKCU:')
    foreach ($key in @(Get-ChildItem -LiteralPath 'Registry::HKEY_USERS' -ErrorAction SilentlyContinue)) {
        $sid = $key.PSChildName
        if ($sid -match '\AS-1-5-21-[0-9-]+\z' -and $sid -ne $own) { $hives += "Registry::HKEY_USERS\$sid" }
    }
    return $hives
}

function Remove-RunValues([string]$ExePath, [string[]]$Hives) {
    # Deletes the CellGlide Run value in each hive when it starts $ExePath. Other values, a
    # CellGlide value that starts something else, and StartupApproved (Windows' own) stay.
    $ErrorActionPreference = 'Stop'
    if (-not $PSBoundParameters.ContainsKey('Hives')) { $Hives = @(Get-LoadedUserHives) }
    $failed = @()
    foreach ($hive in $Hives) {
        $key = "$hive\$RunKeyPath"
        try {
            if (-not (Test-Path -LiteralPath $key)) { continue }
            $data = (Get-Item -LiteralPath $key).GetValue($RunValueName, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            if ($null -ne $data -and (Test-OurRunValue ([string]$data) $ExePath)) {
                Remove-ItemProperty -LiteralPath $key -Name $RunValueName
                Write-Host "Removed the CellGlide startup entry from $hive"
            }
        } catch { $failed += "$hive ($($_.Exception.Message))" }
    }
    Write-Host 'Users who are not signed in keep a CellGlide startup entry that points to the removed program; it does nothing.'
    if ($failed.Count -gt 0) { throw "could not remove the startup entry from $($failed -join ', ')" }
}

function Remove-TreeNoFollow([string]$Dir) {
    # Deletes $Dir bottom-up with calls that never recurse on their own (Windows PowerShell's
    # Remove-Item -Recurse goes through junctions). Each folder is checked right before it is
    # listed and each entry right before it is deleted; a junction or symbolic link found on the
    # way stops it, and is neither followed nor deleted. Locked files get a few retries.
    $ErrorActionPreference = 'Stop'
    $retry = { param([scriptblock]$Action)
        for ($i = 0; ; $i++) {
            try { & $Action; return }
            catch { if ($i -ge 19) { throw }; Start-Sleep -Milliseconds 500 }
        } }
    if (Test-ReparsePoint (Get-Item -LiteralPath $Dir -Force)) { Refuse-ReparsePoint $Dir }
    foreach ($child in @(Get-ChildItem -LiteralPath $Dir -Force)) {
        if (Test-ReparsePoint $child) { Refuse-ReparsePoint $child.FullName }
        if ($child.PSIsContainer) { Remove-TreeNoFollow $child.FullName; continue }
        if ($child.IsReadOnly) { $child.IsReadOnly = $false }
        & $retry { [IO.File]::Delete($child.FullName) }
    }
    if (Test-ReparsePoint (Get-Item -LiteralPath $Dir -Force)) { Refuse-ReparsePoint $Dir }
    & $retry { [IO.Directory]::Delete($Dir, $false) }
}

function Remove-DataDir([string]$Dir) {
    # A data folder, only when neither it nor anything in it is a junction or symbolic link.
    $ErrorActionPreference = 'Stop'
    if (-not (Test-Path -LiteralPath $Dir)) { return }
    if (Test-ReparsePoint (Get-Item -LiteralPath $Dir -Force)) { Refuse-ReparsePoint $Dir }
    foreach ($link in @(Get-ReparsePoints $Dir)) { Refuse-ReparsePoint $link }
    Write-Host "Removing $Dir"
    Remove-TreeNoFollow $Dir
}

function Remove-OwnUserData([string]$Dir = '', [string]$ProfilesRoot = '') {
    # The invoking user's own %LOCALAPPDATA%\CellGlide (-Purge only). This runs in the parent
    # process, as that user (unelevated when the user is), so it only ever deletes what the
    # user's own token can delete: a junction swap by them, or by anyone who can already write
    # their profile, gains no privilege. Other accounts' per-user data is left alone (the parent
    # is not them, and an elevated process must not reach into their writable profile).
    $ErrorActionPreference = 'Stop'
    if (-not $Dir) { $Dir = Join-Path $env:LOCALAPPDATA 'CellGlide' }
    if (-not (Test-Path -LiteralPath $Dir)) { return }
    if (-not $ProfilesRoot) {
        try { $ProfilesRoot = [Environment]::ExpandEnvironmentVariables([string](Get-ItemProperty -LiteralPath $ProfileListKey -Name ProfilesDirectory).ProfilesDirectory) } catch { $ProfilesRoot = '' }
    }
    if ($ProfilesRoot -and -not (Test-ProfileDataPath $Dir $ProfilesRoot)) {
        throw "$Dir is not under the profiles folder $ProfilesRoot"
    }
    Remove-DataDir $Dir
}

function Remove-ServiceEntry {
    # Deletes the stopped service and waits a moment for the SCM to let it go.
    $ErrorActionPreference = 'Stop'
    if (-not (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue)) { return }
    Write-Host "Deleting service $ServiceName"
    Invoke-Native 'sc.exe' @('delete', $ServiceName) | Out-Null
    for ($i = 0; $i -lt 50 -and (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue); $i++) { Start-Sleep -Milliseconds 200 }
    if (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue) {
        Write-Host "The $ServiceName service is marked for deletion; Windows removes it once nothing holds it open."
    }
}

function Remove-Shortcut {
    $ErrorActionPreference = 'Stop'
    if (Test-Path -LiteralPath $Shortcut) {
        Remove-Item -LiteralPath $Shortcut -Force
        Write-Host "Removed $Shortcut"
    }
}

function Remove-UninstallEntry {
    $ErrorActionPreference = 'Stop'
    if (Test-Path -LiteralPath $UninstallKey) {
        Remove-Item -LiteralPath $UninstallKey -Recurse -Force
        Write-Host 'Removed CellGlide from Settings > Apps > Installed apps'
    }
}

function Remove-ProgramDirs([string]$Keep = '') {
    # C:\Program Files\CellGlide, a CellGlide.new or CellGlide.old left by a failed upgrade, and
    # every CellGlide.work-* folder but this run's own; none that is or holds a link.
    $ErrorActionPreference = 'Stop'
    $root = Split-Path -Parent $InstallDir
    $refused = @()
    foreach ($dir in @($InstallDir, "$InstallDir.new", "$InstallDir.old")) {
        if (-not (Test-Path -LiteralPath $dir)) { continue }
        $item = Get-Item -LiteralPath $dir -Force
        if ((Test-ReparsePoint $item) -or ($item.PSIsContainer -and @(Get-ReparsePoints $dir).Count -gt 0)) { $refused += $dir; continue }
        Write-Host "Removing $dir"
        Remove-PathWithRetry $dir
    }
    Remove-StaleWorkDirs $Keep $root 0
    $refused += @(Get-ChildItem -LiteralPath $root -Directory -Force -Filter 'CellGlide.work-*' -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -ne $Keep } | ForEach-Object { $_.FullName })
    if ($refused.Count -gt 0) { throw "kept $($refused -join ', '): it is or holds a junction or symbolic link" }
}

function Remove-InterfaceKeys([string]$Base = $ServicesKey) {
    # cellglide0's Tcpip and Tcpip6 interface keys (our fixed GUID only), once the adapter is
    # gone; the service is already deleted, so its leftover cleanup cannot write them again.
    $ErrorActionPreference = 'Stop'
    if (Get-OurAdapter) { throw "$AdapterName is still present; its interface keys were kept" }
    foreach ($stack in @('Tcpip', 'Tcpip6')) {
        $key = "$Base\$stack\Parameters\Interfaces\$AdapterGuid"
        if (Test-Path -LiteralPath $key) {
            Remove-Item -LiteralPath $key -Recurse -Force
            Write-Host "Removed $key"
        }
    }
}

function Invoke-UninstallPart([switch]$Purge, [string]$Keep = '') {
    # Every step in order; a failed step is reported and the next one runs.
    $ErrorActionPreference = 'Stop'
    $failures = New-Object System.Collections.Generic.List[string]
    $steps = @(
        @('processes', { Stop-CellGlideProcesses }),
        @('stop service', { if (-not (Stop-OurService)) { throw "$AdapterName is still present 10 seconds after stopping the service" } }),
        @('delete service', { Remove-ServiceEntry }),
        @('startup entries', { Remove-RunValues "$InstallDir\app\CellGlide.exe" }),
        @('shortcut', { Remove-Shortcut }),
        @('uninstall entry', { Remove-UninstallEntry }),
        @('program files', { Remove-ProgramDirs $Keep }),
        @('interface keys', { Remove-InterfaceKeys }),
        @('phantom device', { Remove-PhantomDevice }))
    if ($Purge) {
        # ProgramData only: it has an admin/SYSTEM-only ACL, so no standard user can plant a
        # junction here. The invoking user's own %LOCALAPPDATA% is removed by the parent, not
        # here, so this elevated part never deletes inside a user-writable tree.
        $steps += , @('data directory', {
            if ((Test-Path -LiteralPath $DataDir) -and -not (Test-DataDirAdminOnly $DataDir)) {
                throw "$DataDir can be written by a non-administrator; it was not purged"
            }
            Remove-DataDir $DataDir })
    }
    foreach ($step in $steps) {
        try { & $step[1] }
        catch {
            Write-Host "$($step[0]) failed: $($_.Exception.Message)"
            $failures.Add($step[0])
        }
    }
    if (-not $Purge -and (Test-Path -LiteralPath $DataDir)) {
        Write-Host "Kept $DataDir (settings and logs); run with -Purge to delete it."
    }
    if ($failures.Count -gt 0) {
        Write-Host "Uninstall finished with failures: $($failures -join ', ')"
        return 1
    }
    Write-Host 'CellGlide uninstalled.'
    return 0
}

# ---- Elevation ----

function Invoke-UninstallChild {
    # As install.ps1's Invoke-ElevatedChild: writes this script's own text to %TEMP%, starts the
    # elevated child with a bootstrap (-EncodedCommand) that runs that text only if its SHA-256
    # is the one computed here, then prints the log the child wrote into the empty file made
    # here. $Arguments may hold only -Purge.
    param(
        [string]$ScriptText,
        [string[]]$Arguments,
        [scriptblock]$Start = { param($EncodedCommand, $Info)
            $p = Start-Process powershell.exe -Verb RunAs -PassThru -WindowStyle Hidden `
                -ArgumentList "-NoProfile -ExecutionPolicy Bypass -EncodedCommand $EncodedCommand"
            if ($p) { $null = $p.Handle }
            $p }
    )
    $ErrorActionPreference = 'Stop'
    if ([string]::IsNullOrEmpty($ScriptText)) { throw "Cannot find the running script's text" }
    $childParams = @{}
    foreach ($arg in @($Arguments)) {
        if ($arg -ceq '-Purge') { $childParams['Purge'] = $true }
        else { throw "unsupported argument $arg" }
    }
    $id = [guid]::NewGuid().ToString('N')
    $scriptFile = Join-Path ([IO.Path]::GetTempPath()) "cellglide-uninstall-$id.ps1"
    $logFile = Join-Path ([IO.Path]::GetTempPath()) "cellglide-uninstall-$id.log"
    $bytes = (New-Object Text.UTF8Encoding($false)).GetBytes($ScriptText)
    $sha = [Security.Cryptography.SHA256]::Create()
    $hash = -join ($sha.ComputeHash($bytes) | ForEach-Object { $_.ToString('x2') })
    [IO.File]::WriteAllBytes($scriptFile, $bytes)
    try {
        [IO.File]::Open($logFile, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None).Dispose()
        $childParams['ElevatedPart'] = $true
        $childParams['LogPath'] = $logFile
        $bootstrap = New-ChildBootstrap $scriptFile $hash $childParams
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($bootstrap))
        $info = @{ ScriptPath = $scriptFile; LogPath = $logFile; Sha256 = $hash; Bootstrap = $bootstrap }
        $proc = $null
        try { $proc = & $Start $encoded $info } catch { $proc = $null }
        if (-not $proc) {
            Write-Host 'Uninstall cancelled.'
            return 1
        }
        $proc.WaitForExit()
        $code = [int]$proc.ExitCode
        $hadLog = (Test-Path -LiteralPath $logFile) -and (Get-Item -LiteralPath $logFile).Length -gt 0
        if ($hadLog) {
            foreach ($line in @(Get-Content -LiteralPath $logFile)) { Write-Host $line }
        }
        if ($code -eq 3) {
            Write-Host 'Uninstall failed: the copy of the uninstaller in %TEMP% changed before the elevated part could run it.'
            return 1
        }
        if ($code -ne 0 -and -not $hadLog) {
            Write-Host "Uninstall failed: the elevated part ended with exit code $code and left no log."
        }
        return $code
    } finally {
        Remove-Item -LiteralPath $scriptFile, $logFile -Force -ErrorAction SilentlyContinue
    }
}

# ---- Main ----

function Invoke-CellGlideUninstall {
    param([switch]$Purge, [switch]$ElevatedPart, [string]$LogPath = '', [string]$ScriptPath = '')
    $ErrorActionPreference = 'Stop'
    try {
        if ($ElevatedPart) {
            # The elevated child: the log stays in a private folder until the work is done.
            if (-not (Test-Elevated)) {
                Write-Host 'Uninstall failed: the elevated part needs administrator rights.'
                return 1
            }
            $work = New-PrivateWorkDir
            $log = Join-Path $work 'uninstall.log'
            $null = Start-Transcript -LiteralPath $log -NoClobber
            try {
                return (Invoke-UninstallPart -Purge:$Purge -Keep $work)
            } catch {
                Write-Host "Uninstall failed: $($_.Exception.Message)"
                return 1
            } finally {
                $null = Stop-Transcript
                if ($LogPath) { try { Copy-LogToParent $log $LogPath } catch { } }
                Remove-PathWithRetry $work
            }
        }
        # The top-level process, running as the invoking user (elevated or not). The system
        # changes are done elevated (relocated to an elevated child, or here when already an
        # administrator outside the install folder). Then, with -Purge, this process removes the
        # user's own %LOCALAPPDATA%\CellGlide as that user: the elevated part never does.
        if (Test-NeedsRelocation $ScriptPath $InstallDir (Test-Elevated)) {
            if ($ScriptPath) {
                # Run with -File: this process's own working folder must not hold the install
                # folder open while the child deletes it.
                [Environment]::CurrentDirectory = [IO.Path]::GetTempPath()
            }
            $childArgs = @()
            if ($Purge) { $childArgs += '-Purge' }
            $code = Invoke-UninstallChild $CellGlideUninstallText $childArgs
        } else {
            $code = Invoke-UninstallPart -Purge:$Purge
        }
        if ($Purge) {
            try { Remove-OwnUserData }
            catch { Write-Host "user data failed: $($_.Exception.Message)"; if ($code -eq 0) { $code = 1 } }
        } else {
            Write-Host "Kept $env:LOCALAPPDATA\CellGlide (speed test history and preferences); run with -Purge to delete it."
        }
        return $code
    } catch {
        Write-Host "Uninstall failed: $($_.Exception.Message)"
        return 1
    }
}

$code = Invoke-CellGlideUninstall -Purge:$Purge -ElevatedPart:$ElevatedPart -LogPath $LogPath -ScriptPath $PSCommandPath
if ($PSCommandPath) { exit $code }
