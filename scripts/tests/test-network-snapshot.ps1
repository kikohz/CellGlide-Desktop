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

    Write-Host 'all passed'
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
