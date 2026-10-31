# Test for scripts\check-nuget-licenses.ps1. Plain script: throws on failure, prints "all passed".
# Every case builds its own fixture (a lock file and a fake NuGet package cache with .nuspec files)
# in a temporary directory; the real cache and packaging/licenses/nuget.txt are never touched.
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$script = Join-Path (Split-Path -Parent $here) 'check-nuget-licenses.ps1'
$tmp = Join-Path $env:TEMP ('cg-nuget-lic-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null

function Assert($cond, $msg) { if (-not $cond) { throw "FAIL: $msg" } }

function Write-Text([string]$path, [string]$text) {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $path) | Out-Null
    [IO.File]::WriteAllText($path, $text, (New-Object System.Text.UTF8Encoding($false)))
}

# A package: Id, Version, License (inner xml of the license element, or '' for none),
# LicenseType ('expression' or 'file'), LicenseUrl, Files (name -> content).
function New-Fixture([string]$name, [object[]]$packages) {
    $dir = Join-Path $tmp $name
    $cache = Join-Path $dir 'packages'
    $deps = [ordered]@{}
    foreach ($p in $packages) {
        $pkgDir = Join-Path $cache (Join-Path $p.Id.ToLowerInvariant() $p.Version.ToLowerInvariant())
        $meta = "<id>$($p.Id)</id><version>$($p.Version)</version><projectUrl>https://example.invalid/$($p.Id)</projectUrl>"
        if ($p.License) { $meta += "<license type=`"$($p.LicenseType)`">$($p.License)</license>" }
        if ($p.LicenseUrl) { $meta += "<licenseUrl>$($p.LicenseUrl)</licenseUrl>" }
        $nuspec = "<?xml version=`"1.0`" encoding=`"utf-8`"?>`n<package xmlns=`"http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd`"><metadata>$meta</metadata></package>`n"
        Write-Text (Join-Path $pkgDir ($p.Id.ToLowerInvariant() + '.nuspec')) $nuspec
        if ($p.Files) {
            foreach ($f in $p.Files.Keys) { Write-Text (Join-Path $pkgDir $f) $p.Files[$f] }
        }
        $deps[$p.Id] = [ordered]@{ type = 'Transitive'; resolved = $p.Version; contentHash = 'x' }
    }
    $lock = [ordered]@{ version = 2; dependencies = [ordered]@{ 'net10.0' = $deps; 'net10.0/win-x64' = [ordered]@{} } }
    Write-Text (Join-Path $dir 'packages.lock.json') ($lock | ConvertTo-Json -Depth 6)
    return $dir
}

function Run-Script([string]$dir, [switch]$Check) {
    $a = @('-LockFile', (Join-Path $dir 'packages.lock.json'), '-PackagesDir', (Join-Path $dir 'packages'), '-OutFile', (Join-Path $dir 'nuget.txt'))
    if ($Check) { $a += '-Check' }
    $out = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $script @a 2>&1 | Out-String
    return [pscustomobject]@{ Code = $LASTEXITCODE; Output = $out }
}

$mit = @{ Id = 'Fixture.Mit'; Version = '1.2.3'; LicenseType = 'expression'; License = 'MIT' }
# A reviewed entry in the real table that is declared by licenseUrl, so it needs no license file.
$sdkUrl = 'https://aka.ms/WinSDKLicenseURL'
$sdk = @{ Id = 'Microsoft.Windows.SDK.BuildTools'; Version = '10.0.26100.4654'; LicenseUrl = $sdkUrl }

try {
    # Case 1: a permissive package and a reviewed package that matches its review: written, then -Check passes.
    $d = New-Fixture 'ok' @($mit, $sdk)
    $r = Run-Script $d
    Assert ($r.Code -eq 0) "case 1: expected exit 0, got $($r.Code): $($r.Output)"
    $text = [IO.File]::ReadAllText((Join-Path $d 'nuget.txt'))
    Assert ($text.Contains("Fixture.Mit 1.2.3 MIT https://example.invalid/Fixture.Mit`n")) "case 1: MIT line missing: $text"
    Assert ($text.Contains("Microsoft.Windows.SDK.BuildTools 10.0.26100.4654: declares licenseUrl $sdkUrl;")) "case 1: reviewed note missing: $text"
    $r = Run-Script $d -Check
    Assert ($r.Code -eq 0) "case 1: -Check expected exit 0, got $($r.Code): $($r.Output)"

    # Case 2: the committed file is stale (a version edited): -Check fails.
    Write-Text (Join-Path $d 'nuget.txt') ($text.Replace('Fixture.Mit 1.2.3', 'Fixture.Mit 1.2.4'))
    $r = Run-Script $d -Check
    Assert ($r.Code -eq 1) "case 2: expected exit 1, got $($r.Code): $($r.Output)"
    Assert ($r.Output -match 'is not what') "case 2: output: $($r.Output)"

    # Case 3: an unreviewed package with only a license file fails.
    $d = New-Fixture 'unreviewed' @($mit, @{ Id = 'Fixture.Unknown'; Version = '1.0.0'; LicenseType = 'file'; License = 'LICENSE.txt'; Files = @{ 'LICENSE.txt' = 'Some terms' } })
    $r = Run-Script $d
    Assert ($r.Code -eq 1) "case 3: expected exit 1, got $($r.Code): $($r.Output)"
    Assert ($r.Output -match 'Fixture\.Unknown 1\.0\.0: file LICENSE\.txt sha256:[0-9a-f]{16} is not in the allowed list and not reviewed') "case 3: output: $($r.Output)"
    Assert (-not (Test-Path (Join-Path $d 'nuget.txt'))) 'case 3: nuget.txt written despite the failure'

    # Case 4: a non-permissive license expression fails, as does a compound one.
    $d = New-Fixture 'gpl' @(@{ Id = 'Fixture.Gpl'; Version = '2.0.0'; LicenseType = 'expression'; License = 'GPL-3.0-only' }, @{ Id = 'Fixture.Dual'; Version = '1.0.0'; LicenseType = 'expression'; License = 'MIT OR GPL-2.0-only' })
    $r = Run-Script $d
    Assert ($r.Code -eq 1) "case 4: expected exit 1, got $($r.Code): $($r.Output)"
    Assert ($r.Output -match 'Fixture\.Gpl 2\.0\.0: expression GPL-3\.0-only is not in the allowed list') "case 4: output: $($r.Output)"
    Assert ($r.Output -match 'Fixture\.Dual 1\.0\.0: expression MIT OR GPL-2\.0-only is not in the allowed list') "case 4: output: $($r.Output)"

    # Case 5: a reviewed package whose license file changed (same version) fails.
    $d = New-Fixture 'changed-file' @(@{ Id = 'Microsoft.Web.WebView2'; Version = '1.0.3719.77'; LicenseType = 'file'; License = 'LICENSE.txt'; Files = @{ 'LICENSE.txt' = 'New, stricter terms' } })
    $r = Run-Script $d
    Assert ($r.Code -eq 1) "case 5: expected exit 1, got $($r.Code): $($r.Output)"
    Assert ($r.Output -match "Microsoft\.Web\.WebView2 1\.0\.3719\.77: the \.nuspec now declares 'file LICENSE\.txt sha256:[0-9a-f]{16}', but 'file LICENSE\.txt sha256:0af8f1b807512aae' was reviewed") "case 5: output: $($r.Output)"

    # Case 6: a reviewed package whose license changed to a non-permissive expression fails.
    $d = New-Fixture 'changed-expr' @(@{ Id = $sdk.Id; Version = $sdk.Version; LicenseType = 'expression'; License = 'GPL-3.0-only' })
    $r = Run-Script $d
    Assert ($r.Code -eq 1) "case 6: expected exit 1, got $($r.Code): $($r.Output)"
    Assert ($r.Output -match "now declares 'expression GPL-3\.0-only'") "case 6: output: $($r.Output)"

    # Case 7: a reviewed package at a new version fails even with the same declared license.
    $d = New-Fixture 'bumped' @(@{ Id = $sdk.Id; Version = '10.0.26100.9999'; LicenseUrl = $sdkUrl })
    $r = Run-Script $d
    Assert ($r.Code -eq 1) "case 7: expected exit 1, got $($r.Code): $($r.Output)"
    Assert ($r.Output -match 'Microsoft\.Windows\.SDK\.BuildTools 10\.0\.26100\.9999: reviewed for version 10\.0\.26100\.4654 only') "case 7: output: $($r.Output)"

    # Case 8: a missing .nuspec fails.
    $d = New-Fixture 'missing' @($mit)
    Remove-Item -Recurse -Force (Join-Path $d 'packages\fixture.mit')
    $r = Run-Script $d
    Assert ($r.Code -eq 1) "case 8: expected exit 1, got $($r.Code): $($r.Output)"
    Assert ($r.Output -match 'not found') "case 8: output: $($r.Output)"

    Write-Host 'check-nuget-licenses tests: all passed'
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
