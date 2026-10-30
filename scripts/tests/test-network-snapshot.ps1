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

    Write-Host 'all passed'
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
