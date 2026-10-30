<#
Development install of CellGlide on this Windows machine (needs an elevated PowerShell).

  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install-dev.ps1 [-Release]
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install-dev.ps1 -Uninstall [-Purge]

Install: builds cg-service and cg-cli, copies cellglide-service.exe, cellglide.exe,
wintun.dll and wintun-LICENSE.txt to C:\Program Files\CellGlide\, registers and starts the
CellGlide service (LocalSystem, automatic, restart on failure, unrestricted service SID)
and creates C:\ProgramData\CellGlide\{health,logs} with locked-down ACLs.

Uninstall: stops and deletes the service, removes the program directory, cellglide0's
Tcpip/Tcpip6 interface keys and a phantom (non-present) cellglide0 device node left by a
crash. C:\ProgramData\CellGlide\ is kept unless -Purge is given. Uninstall is best-effort
and can be run again on a clean machine.

Only CellGlide's own service, files, adapter (cellglide0) and its registry keys are
touched. Take network snapshots (scripts\network-snapshot.ps1) before and after.
#>
[CmdletBinding()]
param(
    [switch]$Release,
    [switch]$Uninstall,
    [switch]$Purge
)
$ErrorActionPreference = 'Stop'

$ServiceName = 'CellGlide'
$InstallDir = 'C:\Program Files\CellGlide'
$DataDir = Join-Path $env:ProgramData 'CellGlide'
$AdapterName = 'cellglide0'
# cg_core::tunnel::WINDOWS_ADAPTER_GUID; wintun uses it as the device instance id.
$AdapterGuid = '{E490EEF2-2CB0-4A80-B341-BF929C2E56B3}'
$AdapterInstanceId = "SWD\Wintun\$AdapterGuid"
$Root = Split-Path -Parent $PSScriptRoot

# Well-known SIDs, so the script works on any display language.
$SidSystem = '*S-1-5-18'
$SidAdmins = '*S-1-5-32-544'
$SidUsers = '*S-1-5-32-545'

function Test-Elevated {
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
    Get-NetAdapter -IncludeHidden -ErrorAction SilentlyContinue |
        Where-Object { $_.InterfaceGuid -eq $AdapterGuid -or $_.Name -eq $AdapterName }
}

function Wait-AdapterGone {
    # True when cellglide0 is gone within the timeout.
    param([int]$TimeoutSeconds = 10)
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while ($true) {
        if (-not (Get-OurAdapter)) { return $true }
        if ((Get-Date) -ge $deadline) { return $false }
        Start-Sleep -Milliseconds 200
    }
}

function Stop-OurService {
    # Stops CellGlide if it exists and waits for cellglide0 to go. Returns true if it did.
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

function Invoke-Uninstall {
    function Step([string]$name, [scriptblock]$body) {
        try { & $body }
        catch {
            Write-Warning "$name failed: $($_.Exception.Message)"
            $script:failures += $name
        }
    }
    $script:failures = @()

    Step 'stop service' {
        if (-not (Stop-OurService)) { throw "$AdapterName still present after 10 s" }
    }
    Step 'delete service' {
        if (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue) {
            Write-Host "Deleting service $ServiceName"
            Invoke-Native 'sc.exe' @('delete', $ServiceName) | Out-Null
        }
    }
    Step 'remove program directory' {
        if (Test-Path -LiteralPath $InstallDir) {
            Write-Host "Removing $InstallDir"
            Remove-PathWithRetry $InstallDir
        }
    }
    Step 'remove interface keys' {
        foreach ($stack in @('Tcpip', 'Tcpip6')) {
            $key = "HKLM:\SYSTEM\CurrentControlSet\Services\$stack\Parameters\Interfaces\$AdapterGuid"
            if (Test-Path -LiteralPath $key) {
                Write-Host "Removing $key"
                Remove-Item -LiteralPath $key -Recurse -Force
            }
        }
    }
    Step 'remove phantom device' { Remove-PhantomDevice }
    if ($Purge) {
        Step 'purge data directory' {
            if (Test-Path -LiteralPath $DataDir) {
                Write-Host "Removing $DataDir"
                Remove-PathWithRetry $DataDir
            }
        }
    } elseif (Test-Path -LiteralPath $DataDir) {
        Write-Host "Kept $DataDir (use -Purge to delete it)"
    }

    if ($script:failures.Count -gt 0) {
        Write-Host "Uninstall finished with failures: $($script:failures -join ', ')"
        exit 1
    }
    Write-Host 'CellGlide uninstalled.'
}

function Set-DataDirAcl {
    # Root: no inheritance from ProgramData; SYSTEM and Administrators full control, the
    # service SID modify. health\ adds Users read (the GUI reads health files). logs\ keeps
    # only the inherited entries: users must not be able to plant files the service reads.
    $health = Join-Path $DataDir 'health'
    $logs = Join-Path $DataDir 'logs'
    New-Item -ItemType Directory -Force -Path $DataDir, $health, $logs | Out-Null
    # Anyone can create folders in ProgramData; take ownership and drop whatever ACEs a
    # pre-existing directory had before granting ours.
    Invoke-Native 'icacls.exe' @($DataDir, '/setowner', $SidAdmins, '/T', '/C', '/Q') | Out-Null
    Invoke-Native 'icacls.exe' @($DataDir, '/reset', '/T', '/C', '/Q') | Out-Null
    Invoke-Native 'icacls.exe' @($DataDir, '/inheritance:r', '/Q',
        '/grant:r', "${SidSystem}:(OI)(CI)F",
        '/grant:r', "${SidAdmins}:(OI)(CI)F",
        '/grant:r', "NT SERVICE\${ServiceName}:(OI)(CI)M") | Out-Null
    Invoke-Native 'icacls.exe' @($health, '/Q', '/grant:r', "${SidUsers}:(OI)(CI)RX") | Out-Null
}

function Invoke-Install {
    . (Join-Path $PSScriptRoot 'dev-env.ps1')
    $profileDir = if ($Release) { 'release' } else { 'debug' }
    $cargoArgs = @('build', '-p', 'cg-service', '-p', 'cg-cli')
    if ($Release) { $cargoArgs += '--release' }
    Write-Host "cargo $($cargoArgs -join ' ')"
    Push-Location $Root
    try {
        & cargo @cargoArgs
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
    } finally { Pop-Location }
    $bin = Join-Path $Root "target\$profileDir"
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'fetch-wintun.ps1') -Dest $bin
    if ($LASTEXITCODE -ne 0) { throw "fetch-wintun.ps1 failed (exit $LASTEXITCODE)" }

    if (-not (Stop-OurService)) { throw "$AdapterName still present 10 s after stopping $ServiceName" }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    foreach ($f in @('cellglide-service.exe', 'cellglide.exe', 'wintun.dll', 'wintun-LICENSE.txt')) {
        $src = Join-Path $bin $f
        if (-not (Test-Path -LiteralPath $src)) { throw "missing $src" }
        Copy-Item -LiteralPath $src -Destination $InstallDir -Force
    }
    Write-Host "Copied binaries to $InstallDir"

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
    # Privileges (sc.exe privs) are left at the default until the minimal set is measured.

    Set-DataDirAcl
    Write-Host "Prepared $DataDir"

    Start-Service -Name $ServiceName
    (Get-Service -Name $ServiceName).WaitForStatus('Running', [TimeSpan]::FromSeconds(30))
    Write-Host "Service $ServiceName is running."
}

if (-not (Test-Elevated)) {
    Write-Host 'install-dev.ps1 must run elevated: open PowerShell with "Run as administrator" and try again.'
    exit 1
}
if ($Purge -and -not $Uninstall) {
    Write-Host '-Purge is only valid with -Uninstall.'
    exit 1
}
if ($Uninstall) { Invoke-Uninstall } else { Invoke-Install }
