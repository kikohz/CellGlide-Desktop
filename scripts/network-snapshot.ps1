<#
Network snapshot / compare for CellGlide Desktop (Windows PowerShell 5.1).

  network-snapshot.ps1 -Out <file.json>
  network-snapshot.ps1 -Compare <before.json> <after.json>

Compare exits 0 and prints "identical" when snapshots match, otherwise exits 1 and
prints every difference. The cellglide0 adapter (alias or GUID) is always excluded.
Volatile values (DHCP lease times, T1, T2, route lifetimes) are never compared.
An interface's InterfaceMetric is compared only when its AutomaticMetric is Disabled:
Windows recomputes automatic metrics from link speed (Wi-Fi changes them on its own).
The AutomaticMetric flag itself is always compared.
#>
[CmdletBinding(DefaultParameterSetName = 'Out')]
param(
    [Parameter(ParameterSetName = 'Out', Mandatory = $true)][string]$Out,
    [Parameter(ParameterSetName = 'Compare', Mandatory = $true)][string]$Compare,
    [Parameter(ParameterSetName = 'Compare', Mandatory = $true, Position = 0)][string]$After
)
$ErrorActionPreference = 'Stop'

$CgAlias = 'cellglide0'
$CgGuid = '{E490EEF2-2CB0-4A80-B341-BF929C2E56B3}'
# Registry values that change on their own (DHCP lease bookkeeping).
$VolatileRegex = '^(Lease.*|T1|T2|Dhcpv6.*(Time|T1|T2|Leases).*)$'

function Test-Excluded([string]$key) {
    return ($key.IndexOf($CgAlias, [StringComparison]::OrdinalIgnoreCase) -ge 0) -or
           ($key.IndexOf($CgGuid, [StringComparison]::OrdinalIgnoreCase) -ge 0)
}

function Add-Entry($map, [string]$key, [string]$value) {
    if (Test-Excluded $key) { return }
    $k = $key; $i = 1
    while ($map.Contains($k)) { $i++; $k = "$key#$i" }
    $map[$k] = $value
}

function Format-RegValue($v) {
    if ($null -eq $v) { return '' }
    if ($v -is [byte[]]) { return ($v | ForEach-Object { $_.ToString('x2') }) -join '' }
    if ($v -is [array]) { return ($v | ForEach-Object { [string]$_ }) -join ',' }
    return [string]$v
}

function Sort-Map($map) {
    $sorted = [ordered]@{}
    foreach ($k in ($map.Keys | Sort-Object { $_ }  -CaseSensitive)) { $sorted[$k] = $map[$k] }
    return $sorted
}

function New-Snapshot {
    $dns = @{}; $ipif = @{}; $routes = @{}; $reg = @{}; $nrpt = @{}; $hosts = @{}

    foreach ($d in Get-DnsClientServerAddress) {
        Add-Entry $dns "$($d.InterfaceAlias)|$($d.AddressFamily)" (($d.ServerAddresses | ForEach-Object { [string]$_ }) -join ',')
    }
    # NRPT: all rules, keyed by name; value is the full rule content.
    foreach ($r in @(Get-DnsClientNrptRule -ErrorAction SilentlyContinue)) {
        $val = ($r.PSObject.Properties | Where-Object { $_.Name -notmatch '^(Cim|PS)' } |
                Sort-Object Name | ForEach-Object { "$($_.Name)=$(Format-RegValue $_.Value)" }) -join ';'
        Add-Entry $nrpt "$($r.Name)" $val
    }
    foreach ($i in Get-NetIPInterface) {
        Add-Entry $ipif "$($i.InterfaceAlias)|$($i.AddressFamily)" "$($i.InterfaceMetric)|$($i.AutomaticMetric)|$($i.NlMtu)"
    }
    foreach ($r in Get-NetRoute -PolicyStore ActiveStore) {
        # ValidLifetime / PreferredLifetime are deliberately not recorded.
        Add-Entry $routes "$($r.InterfaceAlias)|$($r.DestinationPrefix)|$($r.NextHop)" "$($r.RouteMetric)"
    }
    foreach ($fam in 'Tcpip', 'Tcpip6') {
        $base = "HKLM:\SYSTEM\CurrentControlSet\Services\$fam\Parameters\Interfaces"
        if (-not (Test-Path $base)) { continue }
        foreach ($k in Get-ChildItem $base) {
            $props = Get-ItemProperty -LiteralPath $k.PSPath
            foreach ($p in $props.PSObject.Properties) {
                if ($p.Name -match '^PS(Path|ParentPath|ChildName|Drive|Provider)$') { continue }
                if ($p.Name -match $VolatileRegex) { continue }
                Add-Entry $reg "$fam\$($k.PSChildName)\$($p.Name)" (Format-RegValue $p.Value)
            }
        }
    }
    $hosts['hosts'] = (Get-FileHash -Algorithm SHA256 -LiteralPath 'C:\Windows\System32\drivers\etc\hosts').Hash

    return [ordered]@{
        version  = 1
        sections = [ordered]@{
            dns         = Sort-Map $dns
            hosts       = Sort-Map $hosts
            ipinterface = Sort-Map $ipif
            nrpt        = Sort-Map $nrpt
            registry    = Sort-Map $reg
            routes      = Sort-Map $routes
        }
    }
}

function Read-Flat([string]$path) {
    # Flattens a snapshot file to "section :: key" -> value, applying exclusion and
    # volatile filtering again so injected or older snapshot content is handled too.
    $flat = @{}
    $obj = Get-Content -Raw -LiteralPath $path | ConvertFrom-Json
    foreach ($s in $obj.sections.PSObject.Properties) {
        foreach ($e in $s.Value.PSObject.Properties) {
            if (Test-Excluded $e.Name) { continue }
            if ($s.Name -eq 'registry' -and (($e.Name -split '[\\]')[-1] -match $VolatileRegex)) { continue }
            $value = [string]$e.Value
            if ($s.Name -eq 'ipinterface') {
                # "InterfaceMetric|AutomaticMetric|NlMtu": an automatic metric follows link speed.
                $parts = $value -split '\|'
                if ($parts.Count -eq 3 -and $parts[1] -eq 'Enabled') { $value = "auto|Enabled|$($parts[2])" }
            }
            $flat["$($s.Name) :: $($e.Name)"] = $value
        }
    }
    return $flat
}

if ($PSCmdlet.ParameterSetName -eq 'Out') {
    $snap = New-Snapshot
    $json = $snap | ConvertTo-Json -Depth 10
    [System.IO.File]::WriteAllText($Out, $json, (New-Object System.Text.ASCIIEncoding))
    Write-Output "snapshot written: $Out"
    exit 0
}

$before = Read-Flat $Compare
$afterMap = Read-Flat $After
$diffs = @()
foreach ($k in (@($before.Keys) + @($afterMap.Keys) | Sort-Object -Unique)) {
    $inB = $before.ContainsKey($k); $inA = $afterMap.ContainsKey($k)
    if ($inB -and -not $inA) { $diffs += "REMOVED  $k (was: $($before[$k]))" }
    elseif ($inA -and -not $inB) { $diffs += "ADDED    $k (now: $($afterMap[$k]))" }
    elseif ($before[$k] -ne $afterMap[$k]) { $diffs += "CHANGED  $k : $($before[$k]) -> $($afterMap[$k])" }
}
if ($diffs.Count -eq 0) { Write-Output 'identical'; exit 0 }
$diffs | ForEach-Object { Write-Output $_ }
exit 1
