# Test for scripts\network-snapshot.ps1. Plain script: throws on failure, prints "all passed".
# Never changes real network settings; tampering is done on copies of snapshot JSON files.
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$script = Join-Path (Split-Path -Parent $here) 'network-snapshot.ps1'
$tmp = Join-Path $env:TEMP ('cg-snap-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null

function Run-Script {
    # Runs the snapshot script in a child powershell.exe; returns exit code and output.
    param([string[]]$ScriptArgs)
    $out = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $script @ScriptArgs 2>&1 | Out-String
    return [pscustomobject]@{ Code = $LASTEXITCODE; Output = $out }
}
function Assert($cond, $msg) { if (-not $cond) { throw "FAIL: $msg" } }

try {
    # Case 1: two real snapshots compare identical.
    $a = Join-Path $tmp 'a.json'; $b = Join-Path $tmp 'b.json'
    $r = Run-Script @('-Out', $a); Assert ($r.Code -eq 0) "snapshot a failed: $($r.Output)"
    $r = Run-Script @('-Out', $b); Assert ($r.Code -eq 0) "snapshot b failed: $($r.Output)"
    $r = Run-Script @('-Compare', $a, $b)
    Assert ($r.Code -eq 0) "case 1: expected exit 0, got $($r.Code): $($r.Output)"
    Assert ($r.Output -match 'identical') "case 1: output lacks 'identical'"

    # Case 2: modify one adapter's DNS servers in a copy; compare must exit 1 and name the adapter.
    $snap = Get-Content -Raw $a | ConvertFrom-Json
    $props = @($snap.sections.dns.PSObject.Properties)
    Assert ($props.Count -gt 0) 'no DNS entries in snapshot to tamper with'
    $victim = $props[0]
    $victimAlias = ($victim.Name -split '\|')[0]
    $snap.sections.dns.($victim.Name) = '203.0.113.99'
    $c = Join-Path $tmp 'c.json'
    $snap | ConvertTo-Json -Depth 10 | Set-Content -Encoding ASCII $c
    $r = Run-Script @('-Compare', $a, $c)
    Assert ($r.Code -eq 1) "case 2: expected exit 1, got $($r.Code): $($r.Output)"
    Assert ($r.Output.Contains($victimAlias)) "case 2: output lacks alias '$victimAlias': $($r.Output)"

    # Case 3: injected cellglide0 records (and its GUID) are filtered out.
    $snap = Get-Content -Raw $a | ConvertFrom-Json
    $snap.sections.dns | Add-Member -NotePropertyName 'cellglide0|IPv4' -NotePropertyValue '198.18.0.1' -Force
    $snap.sections.ipinterface | Add-Member -NotePropertyName 'cellglide0|IPv4' -NotePropertyValue '5|False|1500' -Force
    $snap.sections.routes | Add-Member -NotePropertyName 'cellglide0|0.0.0.0/1|0.0.0.0' -NotePropertyValue '5' -Force
    $snap.sections.registry | Add-Member -NotePropertyName 'Tcpip\{E490EEF2-2CB0-4A80-B341-BF929C2E56B3}\NameServer' -NotePropertyValue '198.18.0.1' -Force
    $d = Join-Path $tmp 'd.json'
    $snap | ConvertTo-Json -Depth 10 | Set-Content -Encoding ASCII $d
    $r = Run-Script @('-Compare', $a, $d)
    Assert ($r.Code -eq 0) "case 3: expected exit 0, got $($r.Code): $($r.Output)"

    # Cases 4-6: InterfaceMetric counts only when AutomaticMetric is Disabled; the flag always counts.
    function Write-WithIf([string]$name, [string]$value) {
        $s = Get-Content -Raw $a | ConvertFrom-Json
        $s.sections.ipinterface | Add-Member -NotePropertyName 'TestIf|IPv4' -NotePropertyValue $value -Force
        $p = Join-Path $tmp $name
        $s | ConvertTo-Json -Depth 10 | Set-Content -Encoding ASCII $p
        return $p
    }
    $e1 = Write-WithIf 'e1.json' '30|Enabled|1500'
    $e2 = Write-WithIf 'e2.json' '55|Enabled|1500'
    $r = Run-Script @('-Compare', $e1, $e2)
    Assert ($r.Code -eq 0) "case 4: automatic metric change should be ignored, got $($r.Code): $($r.Output)"
    $f1 = Write-WithIf 'f1.json' '5|Disabled|1500'
    $f2 = Write-WithIf 'f2.json' '6|Disabled|1500'
    $r = Run-Script @('-Compare', $f1, $f2)
    Assert ($r.Code -eq 1) "case 5: manual metric change should be reported, got $($r.Code): $($r.Output)"
    Assert ($r.Output.Contains('TestIf')) "case 5: output lacks 'TestIf': $($r.Output)"
    $r = Run-Script @('-Compare', $e1, $f1)
    Assert ($r.Code -eq 1) "case 6: AutomaticMetric flag change should be reported, got $($r.Code): $($r.Output)"


    # Case 7: snapshots are UTF-8 without a BOM, and adapter names outside ASCII survive a
    # round trip: equal stays identical, a change is reported. (This file stays ASCII: the
    # name is built from code points, "Ethernet" in Chinese.)
    $bytes = [System.IO.File]::ReadAllBytes($a)
    Assert (-not ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF)) "case 7: snapshot starts with a BOM"
    $utf8 = New-Object System.Text.UTF8Encoding($false, $true)
    $null = $utf8.GetString($bytes)  # throws on invalid UTF-8
    $wide = (-join ([char]0x4EE5, [char]0x592A, [char]0x7F51)) + ' 2|IPv4'
    function Write-WithDns([string]$name, [string]$value) {
        $s = [System.IO.File]::ReadAllText($a, $utf8) | ConvertFrom-Json
        $s.sections.dns | Add-Member -NotePropertyName $wide -NotePropertyValue $value -Force
        $p = Join-Path $tmp $name
        [System.IO.File]::WriteAllText($p, ($s | ConvertTo-Json -Depth 10), (New-Object System.Text.UTF8Encoding($false)))
        return $p
    }
    $g1 = Write-WithDns 'g1.json' '192.0.2.1'
    $g2 = Write-WithDns 'g2.json' '192.0.2.1'
    $g3 = Write-WithDns 'g3.json' '192.0.2.2'
    $r = Run-Script @('-Compare', $g1, $g2)
    Assert ($r.Code -eq 0) "case 7: same non-ASCII entry should be identical, got $($r.Code): $($r.Output)"
    $r = Run-Script @('-Compare', $g1, $g3)
    Assert ($r.Code -eq 1) "case 7: changed non-ASCII entry should be reported, got $($r.Code): $($r.Output)"
    Assert ($r.Output.Contains('192.0.2.2')) "case 7: output lacks the new value: $($r.Output)"

    # Case 8: Dhcpv6InterfaceOptions is DHCPv6 bookkeeping and never compared.
    function Write-WithReg([string]$name, [string]$value) {
        $s = [System.IO.File]::ReadAllText($a, $utf8) | ConvertFrom-Json
        $s.sections.registry | Add-Member -NotePropertyName 'Tcpip6\{00000000-0000-0000-0000-000000000001}\Dhcpv6InterfaceOptions' -NotePropertyValue $value -Force
        $p = Join-Path $tmp $name
        [System.IO.File]::WriteAllText($p, ($s | ConvertTo-Json -Depth 10), (New-Object System.Text.UTF8Encoding($false)))
        return $p
    }
    $h1 = Write-WithReg 'h1.json' '0102'
    $h2 = Write-WithReg 'h2.json' '0103'
    $r = Run-Script @('-Compare', $h1, $h2)
    Assert ($r.Code -eq 0) "case 8: Dhcpv6InterfaceOptions should be ignored, got $($r.Code): $($r.Output)"
    $r = Run-Script @('-Compare', $a, $h1)
    Assert ($r.Code -eq 0) "case 8: a new Dhcpv6InterfaceOptions should be ignored, got $($r.Code): $($r.Output)"

    # Case 9: failing to read the NRPT is an error, not an empty section. A function of the
    # same name, defined globally after the module loads, replaces the real one for the run.
    $n = Join-Path $tmp 'n.json'
    $cmd = "Import-Module DnsClient; function global:Get-DnsClientNrptRule { throw 'NRPT unavailable' }; & '$script' -Out '$n'"
    # The child writes the error to stderr; under Stop that would end this script here.
    $ErrorActionPreference = 'Continue'
    $out = & powershell.exe -NoProfile -ExecutionPolicy Bypass -Command $cmd 2>&1 | Out-String
    $ErrorActionPreference = 'Stop'
    $code = $LASTEXITCODE
    Assert ($code -ne 0) "case 9: a failed NRPT read should fail the snapshot: $out"
    Assert ($out.Contains("can't read the NRPT rules")) "case 9: output lacks the NRPT error: $out"
    Assert (-not (Test-Path $n)) "case 9: a snapshot was written anyway"

    # Case 10: route entries carry their metric in the key.
    $snap = [System.IO.File]::ReadAllText($a, $utf8) | ConvertFrom-Json
    $routeKeys = @($snap.sections.routes.PSObject.Properties | ForEach-Object { $_.Name })
    Assert ($routeKeys.Count -gt 0) 'case 10: no routes in snapshot'
    foreach ($k in $routeKeys) {
        Assert (($k -replace '#\d+$', '').Split('|').Count -eq 4) "case 10: route key without a metric: $k"
    }
    Write-Host 'all passed'
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
