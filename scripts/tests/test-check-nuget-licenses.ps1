# Test for scripts\check-nuget-licenses.ps1. Plain script: throws on failure, prints "all passed".
# Every case builds its own fixture (a lock file, a fake NuGet package cache with .nuspec files and an
# empty nuget-texts directory) in a temporary directory; the real cache, packaging/licenses/nuget.txt,
# nuget-notices.txt and nuget-texts are never touched. Fixtures have no project.assets.json, so the
# script runs with -NoExtra.
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
# LicenseType ('expression' or 'file'), LicenseUrl, Files (name -> content), Rid ($true: listed in
# the lock file's win-x64 section instead of the framework section), NotInLock ($true: only in the
# cache, as a $Extra component is).
function New-Package([string]$cache, $p) {
    $pkgDir = Join-Path $cache (Join-Path $p.Id.ToLowerInvariant() $p.Version.ToLowerInvariant())
    $meta = "<id>$($p.Id)</id><version>$($p.Version)</version><projectUrl>https://example.invalid/$($p.Id)</projectUrl>"
    if ($p.License) { $meta += "<license type=`"$($p.LicenseType)`">$($p.License)</license>" }
    if ($p.LicenseUrl) { $meta += "<licenseUrl>$($p.LicenseUrl)</licenseUrl>" }
    $nuspec = "<?xml version=`"1.0`" encoding=`"utf-8`"?>`n<package xmlns=`"http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd`"><metadata>$meta</metadata></package>`n"
    Write-Text (Join-Path $pkgDir ($p.Id.ToLowerInvariant() + '.nuspec')) $nuspec
    if ($p.Files) {
        foreach ($f in $p.Files.Keys) { Write-Text (Join-Path $pkgDir $f) $p.Files[$f] }
    }
    return $pkgDir
}

function New-Fixture([string]$name, [object[]]$packages) {
    $dir = Join-Path $tmp $name
    $cache = Join-Path $dir 'packages'
    $deps = [ordered]@{}
    $ridDeps = [ordered]@{}
    foreach ($p in $packages) {
        [void](New-Package $cache $p)
        if ($p.NotInLock) { continue }
        $entry = [ordered]@{ type = 'Transitive'; resolved = $p.Version; contentHash = 'x' }
        if ($p.Rid) { $ridDeps[$p.Id] = $entry } else { $deps[$p.Id] = $entry }
    }
    $lock = [ordered]@{ version = 2; dependencies = [ordered]@{ 'net10.0' = $deps; 'net10.0/win-x64' = $ridDeps } }
    Write-Text (Join-Path $dir 'packages.lock.json') ($lock | ConvertTo-Json -Depth 6)
    return $dir
}

# A project.assets.json naming $downloads (name -> version range string) as downloadDependencies.
function Write-Assets([string]$dir, $downloads) {
    $list = @($downloads.Keys | ForEach-Object { [ordered]@{ name = $_; version = $downloads[$_] } })
    $assets = [ordered]@{ version = 3; project = [ordered]@{ frameworks = [ordered]@{ 'net10.0-windows10.0.26100' = [ordered]@{ downloadDependencies = $list } } } }
    Write-Text (Join-Path $dir 'project.assets.json') ($assets | ConvertTo-Json -Depth 8)
}

# A real DLL whose file version is $version (C# compiled by Add-Type; the compiler writes the
# version resource from AssemblyFileVersion), at $path.
function New-VersionedDll([string]$path, [string]$version) {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $path) | Out-Null
    $src = "[assembly: System.Reflection.AssemblyFileVersion(`"$version`")] public class WinRtStub$([guid]::NewGuid().ToString('N')) { }"
    Add-Type -TypeDefinition $src -OutputAssembly $path -OutputType Library
}

function Run-Script([string]$dir, [switch]$Check, [switch]$Extra) {
    $a = @('-LockFile', (Join-Path $dir 'packages.lock.json'), '-PackagesDir', (Join-Path $dir 'packages'), '-OutFile', (Join-Path $dir 'nuget.txt'), '-TextsDir', (Join-Path $dir 'texts'))
    if ($Extra) { $a += @('-AssetsFile', (Join-Path $dir 'project.assets.json')) } else { $a += '-NoExtra' }
    if ($Check) { $a += '-Check' }
    $out = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $script @a 2>&1 | Out-String
    return [pscustomobject]@{ Code = $LASTEXITCODE; Output = $out }
}

$mit = @{ Id = 'Fixture.Mit'; Version = '1.2.3'; LicenseType = 'expression'; License = 'MIT'; Files = @{ 'LICENSE' = "MIT terms`n" } }
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

    # Case 9: a shipped MIT package with a LICENSE.txt: nuget-notices.txt has the end-user notice first,
    # then its section with the file's full text.
    $licText = "MIT License`r`n`r`nCopyright (c) Fixture Authors`r`n`r`nPermission is hereby granted, free of charge.`r`n"
    $withFile = @{ Id = 'Fixture.WithFile'; Version = '3.0.0'; LicenseType = 'expression'; License = 'MIT'; Files = @{ 'LICENSE.txt' = $licText; 'THIRD-PARTY-NOTICES.TXT' = "Notice for bundled code`n" } }
    $d = New-Fixture 'with-file' @($withFile)
    $r = Run-Script $d
    Assert ($r.Code -eq 0) "case 9: expected exit 0, got $($r.Code): $($r.Output)"
    $notices = [IO.File]::ReadAllText((Join-Path $d 'nuget-notices.txt'))
    Assert ($notices.StartsWith("# Generated by scripts/check-nuget-licenses.ps1`nCellGlide includes Microsoft Windows App SDK")) "case 9: header or end-user notice missing: $notices"
    $rule = '=' * 80
    $section = "$rule`nFixture.WithFile 3.0.0`nMIT`nhttps://example.invalid/Fixture.WithFile`n`nMIT License`n`nCopyright (c) Fixture Authors`n`nPermission is hereby granted, free of charge.`n`nThird-party notices (THIRD-PARTY-NOTICES.TXT in the package):`n`nNotice for bundled code`n`n"
    Assert ($notices.EndsWith($section)) "case 9: section missing or not LF-only: $notices"
    $r = Run-Script $d -Check
    Assert ($r.Code -eq 0) "case 9: -Check expected exit 0, got $($r.Code): $($r.Output)"

    # Case 10: a stale nuget-notices.txt fails -Check even when nuget.txt is current.
    [IO.File]::WriteAllText((Join-Path $d 'nuget-notices.txt'), $notices.Replace('Fixture Authors', 'Someone Else'))
    $r = Run-Script $d -Check
    Assert ($r.Code -eq 1) "case 10: expected exit 1, got $($r.Code): $($r.Output)"
    Assert ($r.Output -match 'nuget-notices\.txt is not what') "case 10: output: $($r.Output)"

    # Case 11: a shipped MIT package with no license file and nothing in nuget-texts fails, naming it;
    # with a text in nuget-texts it passes and that text is used.
    $bare = @{ Id = 'Fixture.Bare'; Version = '0.9.0'; LicenseType = 'expression'; License = 'MIT' }
    $d = New-Fixture 'bare' @($bare)
    $r = Run-Script $d
    Assert ($r.Code -eq 1) "case 11: expected exit 1, got $($r.Code): $($r.Output)"
    Assert ($r.Output -match 'Fixture\.Bare 0\.9\.0: no license text') "case 11: output: $($r.Output)"
    Assert (-not (Test-Path (Join-Path $d 'nuget.txt'))) 'case 11: nuget.txt written despite the failure'
    Assert (-not (Test-Path (Join-Path $d 'nuget-notices.txt'))) 'case 11: nuget-notices.txt written despite the failure'
    Write-Text (Join-Path $d 'texts\Fixture.Bare.txt') "Source: https://example.invalid/LICENSE`n`nBare terms`n"
    $r = Run-Script $d
    Assert ($r.Code -eq 0) "case 11: with nuget-texts expected exit 0, got $($r.Code): $($r.Output)"
    $notices = [IO.File]::ReadAllText((Join-Path $d 'nuget-notices.txt'))
    Assert ($notices.Contains("Fixture.Bare 0.9.0`nMIT`nhttps://example.invalid/Fixture.Bare`n`nSource: https://example.invalid/LICENSE`n`nBare terms`n")) "case 11: nuget-texts text missing: $notices"

    # Case 12: a package in $NotShipped has its line in nuget.txt but no section in nuget-notices.txt
    # (and needs no license text); a second package with the same license text says "Same terms".
    $illink = @{ Id = 'Microsoft.NET.ILLink.Tasks'; Version = '10.0.14'; LicenseType = 'expression'; License = 'MIT' }
    $twin = @{ Id = 'Fixture.Zed'; Version = '3.0.0'; LicenseType = 'expression'; License = 'MIT'; Files = @{ 'License.md' = $licText } }
    $d = New-Fixture 'not-shipped' @($illink, $withFile, $twin)
    $r = Run-Script $d
    Assert ($r.Code -eq 0) "case 12: expected exit 0, got $($r.Code): $($r.Output)"
    $text = [IO.File]::ReadAllText((Join-Path $d 'nuget.txt'))
    $notices = [IO.File]::ReadAllText((Join-Path $d 'nuget-notices.txt'))
    Assert ($text.Contains("Microsoft.NET.ILLink.Tasks 10.0.14 MIT https://example.invalid/Microsoft.NET.ILLink.Tasks`n")) "case 12: nuget.txt line missing: $text"
    Assert ($text.Contains('#   Microsoft.NET.ILLink.Tasks: ')) "case 12: not-shipped note missing: $text"
    Assert (-not $notices.Contains('Microsoft.NET.ILLink.Tasks')) "case 12: not-shipped package in nuget-notices.txt: $notices"
    Assert ($notices.Contains("Fixture.Zed 3.0.0`nMIT`nhttps://example.invalid/Fixture.Zed`n`nSame terms as Fixture.WithFile above.`n")) "case 12: shared text not referenced: $notices"

    # Case 13: a nuget-texts file that no shipped package needs fails.
    $d = New-Fixture 'stale-text' @($withFile)
    Write-Text (Join-Path $d 'texts\Fixture.Gone.txt') "Source: x`n`nterms`n"
    $r = Run-Script $d
    Assert ($r.Code -eq 1) "case 13: expected exit 1, got $($r.Code): $($r.Output)"
    Assert ($r.Output -match 'Fixture\.Gone\.txt is not needed') "case 13: output: $($r.Output)"

    # Case 14: the same id at two versions (one per lock section) gets a section for each.
    $mit2 = @{ Id = 'Fixture.Mit'; Version = '1.2.4'; LicenseType = 'expression'; License = 'MIT'; Files = @{ 'LICENSE' = "MIT terms`n" }; Rid = $true }
    $d = New-Fixture 'two-versions' @($mit, $mit2)
    $r = Run-Script $d
    Assert ($r.Code -eq 0) "case 14: expected exit 0, got $($r.Code): $($r.Output)"
    $notices = [IO.File]::ReadAllText((Join-Path $d 'nuget-notices.txt'))
    Assert ($notices.Contains("$rule`nFixture.Mit 1.2.3`n") -and $notices.Contains("$rule`nFixture.Mit 1.2.4`n")) "case 14: a version's section is missing: $notices"

    # Case 15: NOTICE* files are appended too, and nuget.txt records every source file.
    $withNotice = @{ Id = 'Fixture.Noticed'; Version = '1.0.0'; LicenseType = 'expression'; License = 'MIT'; Files = @{ 'LICENSE.txt' = "MIT terms`n"; 'NOTICE.txt' = "NOTICES AND INFORMATION`n" } }
    $d = New-Fixture 'notice' @($withNotice)
    $r = Run-Script $d
    Assert ($r.Code -eq 0) "case 15: expected exit 0, got $($r.Code): $($r.Output)"
    $notices = [IO.File]::ReadAllText((Join-Path $d 'nuget-notices.txt'))
    Assert ($notices.Contains("MIT terms`n`nThird-party notices (NOTICE.txt in the package):`n`nNOTICES AND INFORMATION`n")) "case 15: NOTICE.txt missing: $notices"
    $text = [IO.File]::ReadAllText((Join-Path $d 'nuget.txt'))
    Assert ($text.Contains("#   Fixture.Noticed 1.0.0: license fixture.noticed/1.0.0/LICENSE.txt; notices fixture.noticed/1.0.0/NOTICE.txt`n")) "case 15: sources not recorded: $text"
    # -Check compares case-sensitively: an upper-cased license text is a difference.
    [IO.File]::WriteAllText((Join-Path $d 'nuget-notices.txt'), $notices.Replace('MIT terms', 'MIT TERMS'))
    $r = Run-Script $d -Check
    Assert ($r.Code -eq 1) "case 15: -Check passed a change of case: $($r.Output)"

    # Case 16: $Extra, found through project.assets.json: the Native AOT runtime packs and the Windows
    # SDK projection by their exact downloadDependencies versions, C#/WinRT by WinRT.Runtime.dll's
    # file version (pinned).
    $aot = { param($rid) @{ Id = "Microsoft.NETCore.App.Runtime.NativeAOT.$rid"; Version = '10.0.14'; LicenseType = 'expression'; License = 'MIT'; NotInLock = $true; Files = @{ 'LICENSE.TXT' = "The MIT License (MIT)`n"; 'THIRD-PARTY-NOTICES.TXT' = "Runtime notices`n" } } }
    $sdkRef = @{ Id = 'Microsoft.Windows.SDK.NET.Ref'; Version = '10.0.26100.57'; LicenseUrl = 'https://aka.ms/WinSDKLicenseURL'; NotInLock = $true }
    $goodDownloads = [ordered]@{ 'Microsoft.NETCore.App.Runtime.NativeAOT.win-arm64' = '[10.0.14, 10.0.14]'; 'Microsoft.NETCore.App.Runtime.NativeAOT.win-x64' = '[10.0.14, 10.0.14]'; 'Microsoft.Windows.SDK.NET.Ref' = '[10.0.26100.57, 10.0.26100.57]' }
    function New-ExtraFixture([string]$name, [string]$winrtVersion) {
        $dir = New-Fixture $name @($mit, (& $aot 'win-arm64'), (& $aot 'win-x64'), $sdkRef)
        New-VersionedDll (Join-Path $dir 'packages\microsoft.windows.sdk.net.ref\10.0.26100.57\lib\net8.0\WinRT.Runtime.dll') $winrtVersion
        Write-Text (Join-Path $dir 'texts\Microsoft.Windows.SDK.NET.Ref.txt') "Source: sdk`n`nMICROSOFT SOFTWARE LICENSE TERMS`n"
        Write-Text (Join-Path $dir 'texts\Microsoft.Windows.CsWinRT.txt') "Source: cswinrt`n`nMIT License`n"
        Write-Assets $dir $goodDownloads
        return $dir
    }
    $d = New-ExtraFixture 'extra' '2.2.0.48161'
    $r = Run-Script $d -Extra
    Assert ($r.Code -eq 0) "case 16: expected exit 0, got $($r.Code): $($r.Output)"
    $notices = [IO.File]::ReadAllText((Join-Path $d 'nuget-notices.txt'))
    foreach ($want in @(
            "$rule`nMicrosoft.NETCore.App.Runtime.NativeAOT.win-arm64 10.0.14`nMIT`nhttps://example.invalid/Microsoft.NETCore.App.Runtime.NativeAOT.win-arm64`n`nThe MIT License (MIT)`n`nThird-party notices (THIRD-PARTY-NOTICES.TXT in the package):`n`nRuntime notices`n",
            "$rule`nMicrosoft.NETCore.App.Runtime.NativeAOT.win-x64 10.0.14`nMIT`n",
            "$rule`nMicrosoft.Windows.CsWinRT 2.2.0.48161`nMIT`nhttps://github.com/microsoft/CsWinRT`n`nSource: cswinrt`n`nMIT License`n",
            "$rule`nMicrosoft.Windows.SDK.NET.Ref 10.0.26100.57`nMicrosoft Software License Terms (Windows SDK)`nhttps://example.invalid/Microsoft.Windows.SDK.NET.Ref`n`nSource: sdk`n`nMICROSOFT SOFTWARE LICENSE TERMS`n")) {
        Assert ($notices.Contains($want)) "case 16: missing section start '$($want.Substring(81, 40))': $notices"
    }
    $text = [IO.File]::ReadAllText((Join-Path $d 'nuget.txt'))
    Assert ($text.Contains('#   Microsoft.Windows.CsWinRT 2.2.0.48161: MIT; ')) "case 16: extra note missing: $text"
    $r = Run-Script $d -Extra -Check
    Assert ($r.Code -eq 0) "case 16: -Check expected exit 0, got $($r.Code): $($r.Output)"
    # A range that is not one exact version fails.
    $loose = [ordered]@{}; foreach ($k in $goodDownloads.Keys) { $loose[$k] = $goodDownloads[$k] }
    $loose['Microsoft.NETCore.App.Runtime.NativeAOT.win-x64'] = '[10.0.14, )'
    Write-Assets $d $loose
    $r = Run-Script $d -Extra
    Assert ($r.Code -eq 1) "case 16: loose version expected exit 1, got $($r.Code): $($r.Output)"
    Assert ($r.Output -match "Microsoft\.NETCore\.App\.Runtime\.NativeAOT\.win-x64: downloadDependencies version '\[10\.0\.14, \)' is not an exact version") "case 16: output: $($r.Output)"
    # A runtime pack missing from downloadDependencies fails.
    $missing = [ordered]@{}; foreach ($k in $goodDownloads.Keys) { if ($k -notlike '*win-arm64') { $missing[$k] = $goodDownloads[$k] } }
    Write-Assets $d $missing
    $r = Run-Script $d -Extra
    Assert ($r.Code -eq 1) "case 16: missing pack expected exit 1, got $($r.Code): $($r.Output)"
    Assert ($r.Output -match 'Microsoft\.NETCore\.App\.Runtime\.NativeAOT\.win-arm64: not in the downloadDependencies') "case 16: output: $($r.Output)"

    # Case 17: a WinRT.Runtime.dll of another version than the reviewed one fails.
    $d = New-ExtraFixture 'winrt-bumped' '2.2.1.1'
    $r = Run-Script $d -Extra
    Assert ($r.Code -eq 1) "case 17: expected exit 1, got $($r.Code): $($r.Output)"
    Assert ($r.Output -match 'Microsoft\.Windows\.CsWinRT: WinRT\.Runtime\.dll is version 2\.2\.1\.1, but 2\.2\.0\.48161 was reviewed') "case 17: output: $($r.Output)"

    Write-Host 'check-nuget-licenses tests: all passed'
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
