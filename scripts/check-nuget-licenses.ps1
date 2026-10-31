# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check-nuget-licenses.ps1 [-Check]
#
# Writes two files from windows/CellGlide.App/packages.lock.json (every framework and RID section;
# Direct, Transitive and CentralTransitive) and the NuGet global cache ($env:NUGET_PACKAGES,
# otherwise ~\.nuget\packages; run a restore first):
#
# packaging/licenses/nuget.txt: the license of every package in the lock file, one line per package,
# "id version license projectUrl", sorted by id; then, as # comments, why each reviewed license is
# allowed, which packages are not shipped, and what is compiled in without being in the lock file.
#
# packaging/licenses/nuget-notices.txt: what the Windows About page and THIRD-PARTY-LICENSES.txt
# show for the .NET side. Line 1 is a header; then packaging/licenses/windows-end-user-notice.txt
# (the end-user terms notice of the Windows App SDK terms, section 3.b.ii); then one section per
# component that ships with the app, sorted by id: an 80 '=' line, "id version", the license name,
# the project URL, a blank line and the full license text, followed by the package's own
# third-party notices files if it has any ($NoticePatterns: NOTICE*, THIRD-PARTY-NOTICES* or
# ThirdPartyNotices*, any case). nuget.txt records which files each section's texts came from.
# The components are the lock file's packages that are not in $NotShipped, plus $Extra: what is
# compiled into CellGlide.exe without being in the lock file. The license text is the license file
# the .nuspec declares, otherwise the package root's LICENSE* / License* files, otherwise
# packaging/licenses/nuget-texts/<id>.txt (taken from the package's upstream repository at the
# commit its .nuspec names; line 1 says where from). If none exists the script fails. A text (or a
# notices file) that an earlier section already carries is not repeated: the section says
# "Same terms as <id> above." (the Windows App SDK packages share one license file). The same id at
# two versions gets two sections.
#
# A license expression in $Allowed passes as is. Anything else (another expression, a license
# file, or only a licenseUrl) must be listed in $Reviewed below, pinned to the reviewed version and
# to what that version's .nuspec declares: the expression, the licenseUrl, or the license file's
# name and SHA-256. A new version, or a different declared license, fails until someone reviews it
# again and updates the entry. A package that would ship with the app under a license that is not
# permissive is a decision for a person, not for this table's editor.
#
# -Check: generate both in memory and compare them with the committed files; exit 1 if either
# differs, a license is not allowed or reviewed, or a text is missing. Without -Check both files
# are rewritten (only when everything succeeded).
# -LockFile, -PackagesDir, -OutFile, -NoticesFile, -TextsDir, -AssetsFile, -NoExtra: for
# scripts/tests/test-check-nuget-licenses.ps1 (most fixtures have no project.assets.json, so they
# pass -NoExtra; the $Extra cases pass -AssetsFile).
param(
    [switch]$Check,
    [string]$LockFile = '',
    [string]$PackagesDir = '',
    [string]$OutFile = '',
    [string]$NoticesFile = '',
    [string]$TextsDir = '',
    [string]$AssetsFile = '',
    [switch]$NoExtra
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$licensesDir = Join-Path $root 'packaging\licenses'
$lockPath = if ($LockFile) { $LockFile } else { Join-Path $root 'windows\CellGlide.App\packages.lock.json' }
$assetsPath = if ($AssetsFile) { $AssetsFile } else { Join-Path $root 'windows\CellGlide.App\obj\project.assets.json' }
$outPath = if ($OutFile) { $OutFile } else { Join-Path $licensesDir 'nuget.txt' }
$noticesPath = if ($NoticesFile) { $NoticesFile } elseif ($OutFile) { Join-Path (Split-Path -Parent $OutFile) 'nuget-notices.txt' } else { Join-Path $licensesDir 'nuget-notices.txt' }
$textsPath = if ($TextsDir) { $TextsDir } else { Join-Path $licensesDir 'nuget-texts' }
$endUserPath = Join-Path $licensesDir 'windows-end-user-notice.txt'
$cache = $PackagesDir
if (-not $cache) { $cache = $env:NUGET_PACKAGES }
if (-not $cache) { $cache = Join-Path $env:USERPROFILE '.nuget\packages' }

$Allowed = @('MIT', 'Apache-2.0', 'BSD-2-Clause', 'BSD-3-Clause')

$WinAppSdk = 'Microsoft Software License Terms (Windows App SDK), redistributable per section 3.a.i'
$WinAppSdkShipped = 'the WinUI 3 runtime the app is built on (spec 9.1); its files are carried in the self-contained publish folder, which section 3.a.i of its terms allows'
$WinAppSdkExcluded = 'pulled in by the Microsoft.WindowsAppSDK metapackage; CellGlide.App references it with ExcludeAssets="all", so none of its files are built or published (check-windows-core.ps1 -Publish checks)'
$SdkBuildOnly = 'Microsoft Software License Terms (Windows SDK), build only, not shipped'

# What the reviewed versions declare. A license file is identified by its name and the first 16
# hex digits of its SHA-256.
$WasdkTerms = 'file license.txt sha256:5b11e6347756e40f'
$WasdkMlTerms = 'file license.txt sha256:656aab74c15aa9f9'

# id -> reviewed version, what that version's .nuspec declares, the license name shown in
# nuget.txt, and why it is acceptable.
$Reviewed = [ordered]@{
    'Microsoft.WindowsAppSDK'                        = @{ Version = '2.6.4'; Declared = $WasdkTerms; License = $WinAppSdk; Reason = 'metapackage (no files of its own); ' + $WinAppSdkShipped }
    'Microsoft.WindowsAppSDK.Base'                   = @{ Version = '2.6.4'; Declared = $WasdkTerms; License = $WinAppSdk; Reason = $WinAppSdkShipped }
    'Microsoft.WindowsAppSDK.DWrite'                 = @{ Version = '2.6.4'; Declared = $WasdkTerms; License = $WinAppSdk; Reason = $WinAppSdkShipped }
    'Microsoft.WindowsAppSDK.Foundation'             = @{ Version = '2.6.4'; Declared = $WasdkTerms; License = $WinAppSdk; Reason = $WinAppSdkShipped }
    'Microsoft.WindowsAppSDK.InteractiveExperiences' = @{ Version = '2.6.4'; Declared = $WasdkTerms; License = $WinAppSdk; Reason = $WinAppSdkShipped }
    'Microsoft.WindowsAppSDK.Runtime'                = @{ Version = '2.6.4'; Declared = $WasdkTerms; License = $WinAppSdk; Reason = $WinAppSdkShipped }
    'Microsoft.WindowsAppSDK.Widgets'                = @{ Version = '2.6.4'; Declared = $WasdkTerms; License = $WinAppSdk; Reason = $WinAppSdkShipped }
    'Microsoft.WindowsAppSDK.WinUI'                  = @{ Version = '2.6.4'; Declared = $WasdkTerms; License = $WinAppSdk; Reason = $WinAppSdkShipped }
    'Microsoft.WindowsAppSDK.AI'                     = @{ Version = '2.6.4'; Declared = $WasdkTerms; License = $WinAppSdk; Reason = $WinAppSdkExcluded }
    'Microsoft.WindowsAppSDK.ML'                     = @{ Version = '2.6.4'; Declared = $WasdkMlTerms; License = $WinAppSdk; Reason = '(its license file adds the Windows ML terms) ' + $WinAppSdkExcluded }
    'Microsoft.WindowsAppSDK.Search'                 = @{ Version = '2.6.4'; Declared = $WasdkTerms; License = $WinAppSdk; Reason = $WinAppSdkExcluded }
    'Microsoft.Windows.AI.MachineLearning'           = @{ Version = '2.4.89'; Declared = 'file license.txt sha256:66395f8cb219087f'; License = $WinAppSdk; Reason = '(its license file is the Windows ML Runtime terms) ' + $WinAppSdkExcluded }
    'Microsoft.Web.WebView2'                         = @{ Version = '1.0.3719.77'; Declared = 'file LICENSE.txt sha256:0af8f1b807512aae'; License = 'BSD-3-Clause'; Reason = 'its LICENSE.txt is the BSD 3-clause text; shipped (a dependency of the WinUI package)' }
    'Microsoft.Windows.SDK.BuildTools'               = @{ Version = '10.0.26100.4654'; Declared = 'licenseUrl https://aka.ms/WinSDKLicenseURL'; License = $SdkBuildOnly; Reason = 'build tools (Windows SDK metadata and tools), used only while building' }
    'Microsoft.Windows.SDK.BuildTools.MSIX'          = @{ Version = '1.7.251221100'; Declared = 'file sdk_license.txt sha256:a7a5c7e7ff998558'; License = $SdkBuildOnly; Reason = 'MSIX build tools that make resources.pri, used only while building' }
}

# Packages of the lock file that do not ship with the app: used while building only, or with none
# of their files built or published. They keep their line in nuget.txt but get no section in
# nuget-notices.txt. Every other package of the lock file counts as shipped. (Checked against the
# Release publish folder and the Native AOT compiler's inputs, obj\Release\...\native\CellGlide.ilc.rsp.)
$ExcludedAllAssets = 'referenced with ExcludeAssets="all" in CellGlide.App.csproj: none of its files are built or published'
$NotShipped = [ordered]@{
    'Microsoft.NET.ILLink.Tasks'            = 'the trimmer, run while publishing; nothing of it is in the publish folder'
    'Microsoft.DotNet.ILCompiler'           = 'the Native AOT compiler driver (MSBuild targets), run while publishing; what is compiled into CellGlide.exe is the Native AOT runtime pack, Microsoft.NETCore.App.Runtime.NativeAOT.<rid> (see below); runtime.win-*.Microsoft.DotNet.ILCompiler (the compiler itself) is listed as shipped only to be safe'
    'Microsoft.Windows.SDK.BuildTools'      = 'Windows SDK build tools, run while building'
    'Microsoft.Windows.SDK.BuildTools.MSIX' = 'MSIX build tools that make resources.pri, run while building'
    'Microsoft.WindowsAppSDK'               = 'metapackage: only MSBuild props and targets, no files of its own; its component packages have their own sections'
    'Microsoft.WindowsAppSDK.AI'            = $ExcludedAllAssets
    'Microsoft.WindowsAppSDK.ML'            = $ExcludedAllAssets
    'Microsoft.WindowsAppSDK.Search'        = $ExcludedAllAssets
    'Microsoft.Windows.AI.MachineLearning'  = $ExcludedAllAssets
}

# Compiled into CellGlide.exe but not in the lock file. Each is found through
# windows/CellGlide.App/obj/project.assets.json (downloadDependencies), so a restore must have run.
#   Source 'download': the package of that id in downloadDependencies, read like a lock-file
#     package (its .nuspec declaration must be allowed or match Declared).
#   Source 'winrt': C#/WinRT's runtime, WinRT.Runtime.dll, which the Windows SDK projection package
#     carries; its version is the DLL's file version and must be the reviewed Version (its license
#     text is taken from the CsWinRT commit that version names).
$Extra = [ordered]@{
    'Microsoft.NETCore.App.Runtime.NativeAOT.win-arm64' = @{ Source = 'download'; Note = 'the .NET runtime and libraries Native AOT compiles into CellGlide.exe (win-arm64)' }
    'Microsoft.NETCore.App.Runtime.NativeAOT.win-x64'   = @{ Source = 'download'; Note = 'the .NET runtime and libraries Native AOT compiles into CellGlide.exe (win-x64)' }
    'Microsoft.Windows.SDK.NET.Ref'                     = @{ Source = 'download'; Declared = 'licenseUrl https://aka.ms/WinSDKLicenseURL'; License = 'Microsoft Software License Terms (Windows SDK)'; Note = 'the Windows SDK projection (Microsoft.Windows.SDK.NET.dll) compiled into CellGlide.exe' }
    'Microsoft.Windows.CsWinRT'                         = @{ Source = 'winrt'; Version = '2.2.0.48161'; License = 'MIT'; ProjectUrl = 'https://github.com/microsoft/CsWinRT'; Note = 'C#/WinRT runtime (WinRT.Runtime.dll from Microsoft.Windows.SDK.NET.Ref) compiled into CellGlide.exe' }
}

$Rule = '=' * 80

# The package root files appended to a section as its third-party notices.
$NoticePatterns = @('NOTICE*', 'THIRD-PARTY-NOTICES*', 'ThirdPartyNotices*')

function Get-Metadata([xml]$doc, [string]$name) {
    $node = $doc.SelectSingleNode("/*[local-name()='package']/*[local-name()='metadata']/*[local-name()='$name']")
    if ($node) { return $node } else { return $null }
}

# What a .nuspec declares, as one string: "expression X", "file NAME sha256:HEX16",
# "licenseUrl URL", or "none".
function Get-Declared([xml]$doc, [string]$packageDir) {
    $license = Get-Metadata $doc 'license'
    if ($license) {
        $type = $license.GetAttribute('type')
        $value = $license.InnerText.Trim()
        if ($type -eq 'file') {
            $file = Join-Path $packageDir $value
            if (-not (Test-Path $file)) { return "file $value (missing)" }
            $hash = (Get-FileHash -Algorithm SHA256 $file).Hash.ToLowerInvariant().Substring(0, 16)
            return "file $value sha256:$hash"
        }
        return "$type $value"
    }
    $url = Get-Metadata $doc 'licenseUrl'
    if ($url) { return "licenseUrl $($url.InnerText.Trim())" }
    return 'none'
}

function Get-PackageDir([string]$id, [string]$version) {
    return Join-Path $cache (Join-Path $id.ToLowerInvariant() $version.ToLowerInvariant())
}

# A text file as LF-only text: a BOM is dropped, CRLF and lone CR become LF, blank lines at either
# end are trimmed, and it ends with exactly one LF. Bytes that are not UTF-8 make it fail.
function Read-Text([string]$path) {
    $bytes = [IO.File]::ReadAllBytes($path)
    $utf8 = New-Object System.Text.UTF8Encoding($false, $true)
    try {
        if ($bytes.Length -ge 2 -and (($bytes[0] -eq 0xFF -and $bytes[1] -eq 0xFE) -or ($bytes[0] -eq 0xFE -and $bytes[1] -eq 0xFF))) {
            $text = [IO.File]::ReadAllText($path)
        } else {
            $offset = 0
            if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) { $offset = 3 }
            $text = $utf8.GetString($bytes, $offset, $bytes.Length - $offset)
        }
    } catch {
        throw "$path is not UTF-8 text"
    }
    $text = $text.Replace("`r`n", "`n").Replace("`r", "`n")
    $text = $text.Trim("`n")
    return $text + "`n"
}

function Get-Sha256([string]$text) {
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($text)))).Replace('-', '')
    } finally {
        $sha.Dispose()
    }
}

# Files directly in $dir whose name matches one of $patterns (wildcards, any case), in ordinal,
# case-insensitive order of their names.
function Get-RootFiles([string]$dir, [string[]]$patterns) {
    if (-not $dir -or -not (Test-Path -LiteralPath $dir)) { return @() }
    $files = @(Get-ChildItem -LiteralPath $dir -File | Where-Object {
            $name = $_.Name
            @($patterns | Where-Object { $name -like $_ }).Count -gt 0
        })
    $names = [string[]]@($files | ForEach-Object { $_.Name })
    [Array]::Sort($names, [StringComparer]::OrdinalIgnoreCase)
    return @($names | ForEach-Object { Join-Path $dir $_ })
}

$textsUsed = @{}

# Where a text came from, as nuget.txt records it: a package file relative to the NuGet cache
# ("<id>/<version>/<file>", lower-case id and version as the cache has them), or the repository's
# nuget-texts file.
function Get-SourceName([string]$path) {
    $full = [IO.Path]::GetFullPath($path)
    $base = [IO.Path]::GetFullPath($cache).TrimEnd('\') + '\'
    if ($full.StartsWith($base, [StringComparison]::OrdinalIgnoreCase)) { return $full.Substring($base.Length).Replace('\', '/') }
    return 'packaging/licenses/nuget-texts/' + (Split-Path -Leaf $full)
}

# The license text of a component and the files it came from ({ Text; Sources }), or $null: the
# declared license file, else the package root's LICENSE* / License* files (joined by a blank
# line), else nuget-texts/<id>.txt.
function Get-LicenseText([string]$id, [xml]$doc, [string]$packageDir) {
    if ($doc) {
        $license = Get-Metadata $doc 'license'
        if ($license -and $license.GetAttribute('type') -eq 'file') {
            $file = Join-Path $packageDir $license.InnerText.Trim()
            if (Test-Path -LiteralPath $file) { return @{ Text = (Read-Text $file); Sources = @(Get-SourceName $file) } }
        }
    }
    $files = @(Get-RootFiles $packageDir @('LICENSE*'))
    if ($files.Count -gt 0) {
        return @{ Text = (@($files | ForEach-Object { Read-Text $_ }) -join "`n"); Sources = @($files | ForEach-Object { Get-SourceName $_ }) }
    }
    $own = Join-Path $textsPath "$id.txt"
    if (Test-Path -LiteralPath $own) {
        $textsUsed["$id.txt"] = $true
        return @{ Text = (Read-Text $own); Sources = @(Get-SourceName $own) }
    }
    return $null
}

$lock = Get-Content -Raw $lockPath | ConvertFrom-Json
$packages = @{}
foreach ($section in $lock.dependencies.PSObject.Properties) {
    foreach ($dep in $section.Value.PSObject.Properties) {
        if ($dep.Value.type -eq 'Project') { continue }
        $key = "$($dep.Name) $($dep.Value.resolved)"
        $packages[$key] = @{ Id = $dep.Name; Version = $dep.Value.resolved }
    }
}

$lines = New-Object System.Collections.Generic.List[string]
$notes = New-Object System.Collections.Generic.List[string]
$problems = New-Object System.Collections.Generic.List[string]
# The components that get a section in nuget-notices.txt: Id, Version, License, ProjectUrl, Doc
# (the .nuspec, or $null), PackageDir.
$components = New-Object System.Collections.Generic.List[object]

# Ordinal, case-insensitive order, so the files are the same whatever the machine's culture.
$keys = [string[]]@($packages.Keys)
[Array]::Sort($keys, [StringComparer]::OrdinalIgnoreCase)
foreach ($key in $keys) {
    $id = $packages[$key].Id
    $version = $packages[$key].Version
    $packageDir = Get-PackageDir $id $version
    $nuspec = Join-Path $packageDir ($id.ToLowerInvariant() + '.nuspec')
    if (-not (Test-Path $nuspec)) {
        $problems.Add("$id ${version}: $nuspec not found (run dotnet restore first)")
        continue
    }
    [xml]$doc = Get-Content -Raw $nuspec
    $projectUrlNode = Get-Metadata $doc 'projectUrl'
    $projectUrl = if ($projectUrlNode) { $projectUrlNode.InnerText.Trim() } else { '-' }
    $declared = Get-Declared $doc $packageDir

    if ($declared.StartsWith('expression ') -and ($Allowed -contains $declared.Substring(11))) {
        $license = $declared.Substring(11)
    } elseif ($Reviewed.Contains($id)) {
        $entry = $Reviewed[$id]
        if ($entry.Version -ne $version) {
            $problems.Add("$id ${version}: reviewed for version $($entry.Version) only; review $version ($declared) and update `$Reviewed")
            continue
        }
        if ($entry.Declared -ne $declared) {
            $problems.Add("$id ${version}: the .nuspec now declares '$declared', but '$($entry.Declared)' was reviewed; review it and update `$Reviewed")
            continue
        }
        $license = $entry.License
        $notes.Add("#   $id ${version}: declares $declared; $($entry.Reason)")
    } else {
        $problems.Add("$id ${version}: $declared is not in the allowed list and not reviewed")
        continue
    }
    $lines.Add("$id $version $license $projectUrl")
    if (-not $NotShipped.Contains($id)) {
        $components.Add(@{ Id = $id; Version = $version; License = $license; ProjectUrl = $projectUrl; Doc = $doc; PackageDir = $packageDir })
    }
}

$extraNotes = New-Object System.Collections.Generic.List[string]
if (-not $NoExtra) {
    $download = @{}
    if (-not (Test-Path $assetsPath)) {
        $problems.Add("$assetsPath not found (run dotnet restore first); it names the components compiled in without being in the lock file")
    } else {
        $assets = Get-Content -Raw $assetsPath | ConvertFrom-Json
        foreach ($fw in $assets.project.frameworks.PSObject.Properties) {
            foreach ($dd in @($fw.Value.downloadDependencies)) {
                if (-not $dd) { continue }
                # "[10.0.26100.57, 10.0.26100.57]": an exact version.
                if ($dd.version -match '^\[([^,\]]+),\s*\1\]$') { $download[$dd.name] = $Matches[1] }
                else { $problems.Add("$($dd.name): downloadDependencies version '$($dd.version)' is not an exact version") }
            }
        }
    }
    $sdkRef = 'Microsoft.Windows.SDK.NET.Ref'
    foreach ($id in $Extra.Keys) {
        $entry = $Extra[$id]
        if ($entry.Source -eq 'download') {
            if (-not $download.ContainsKey($id)) {
                $problems.Add("${id}: not in the downloadDependencies of $assetsPath")
                continue
            }
            $version = $download[$id]
            $packageDir = Get-PackageDir $id $version
            $nuspec = Join-Path $packageDir ($id.ToLowerInvariant() + '.nuspec')
            if (-not (Test-Path $nuspec)) {
                $problems.Add("$id ${version}: $nuspec not found (run dotnet restore first)")
                continue
            }
            [xml]$doc = Get-Content -Raw $nuspec
            $declared = Get-Declared $doc $packageDir
            if ($entry.Declared) {
                if ($entry.Declared -ne $declared) {
                    $problems.Add("$id ${version}: the .nuspec now declares '$declared', but '$($entry.Declared)' was reviewed; review it and update `$Extra")
                    continue
                }
                $license = $entry.License
            } elseif ($declared.StartsWith('expression ') -and ($Allowed -contains $declared.Substring(11))) {
                $license = $declared.Substring(11)
            } else {
                $problems.Add("$id ${version}: $declared is not in the allowed list and not reviewed")
                continue
            }
            $projectUrlNode = Get-Metadata $doc 'projectUrl'
            $projectUrl = if ($projectUrlNode) { $projectUrlNode.InnerText.Trim() } else { '-' }
            $components.Add(@{ Id = $id; Version = $version; License = $license; ProjectUrl = $projectUrl; Doc = $doc; PackageDir = $packageDir })
            $extraNotes.Add("#   $id ${version}: $license; $($entry.Note)")
        } elseif ($entry.Source -eq 'winrt') {
            if (-not $download.ContainsKey($sdkRef)) {
                $problems.Add("${id}: $sdkRef, which carries WinRT.Runtime.dll, is not in the downloadDependencies of $assetsPath")
                continue
            }
            $dll = Join-Path (Get-PackageDir $sdkRef $download[$sdkRef]) 'lib\net8.0\WinRT.Runtime.dll'
            if (-not (Test-Path $dll)) {
                $problems.Add("${id}: $dll not found (run dotnet restore first)")
                continue
            }
            $version = [Diagnostics.FileVersionInfo]::GetVersionInfo($dll).FileVersion
            if ($version -ne $entry.Version) {
                $problems.Add("${id}: WinRT.Runtime.dll is version $version, but $($entry.Version) was reviewed; take its license from the CsWinRT commit in its product version, update nuget-texts/$id.txt and `$Extra")
                continue
            }
            $components.Add(@{ Id = $id; Version = $version; License = $entry.License; ProjectUrl = $entry.ProjectUrl; Doc = $null; PackageDir = '' })
            $extraNotes.Add("#   $id ${version}: $($entry.License); $($entry.Note)")
        }
    }
}

# The sections of nuget-notices.txt, sorted by id like nuget.txt. Keyed by "id version": the same id
# at two versions (two RIDs resolving differently) gets a section for each.
$byKey = @{}
foreach ($c in $components) {
    $k = "$($c.Id) $($c.Version)"
    if ($byKey.ContainsKey($k)) { $problems.Add("${k}: listed twice (lock file and `$Extra?)"); continue }
    $byKey[$k] = $c
}
$sectionKeys = [string[]]@($byKey.Keys)
[Array]::Sort($sectionKeys, [StringComparer]::OrdinalIgnoreCase)
$sections = New-Object System.Collections.Generic.List[string]
$sourceNotes = New-Object System.Collections.Generic.List[string]
$seenText = @{}
$seenNotices = @{}
foreach ($k in $sectionKeys) {
    $c = $byKey[$k]
    $found = Get-LicenseText $c.Id $c.Doc $c.PackageDir
    if (-not $found) {
        $problems.Add("$($c.Id) $($c.Version): no license text (no license file in the package and no packaging/licenses/nuget-texts/$($c.Id).txt; take it from the package's upstream repository at the commit its .nuspec names)")
        continue
    }
    $text = $found.Text
    $sources = New-Object System.Collections.Generic.List[string]
    foreach ($src in $found.Sources) { $sources.Add("license $src") }
    $s = New-Object System.Text.StringBuilder
    [void]$s.Append("$Rule`n$($c.Id) $($c.Version)`n$($c.License)`n$($c.ProjectUrl)`n`n")
    $hash = Get-Sha256 $text
    if ($seenText.ContainsKey($hash)) {
        [void]$s.Append("Same terms as $($seenText[$hash]) above.`n")
    } else {
        $seenText[$hash] = $c.Id
        [void]$s.Append($text)
    }
    if ($c.PackageDir) {
        foreach ($file in @(Get-RootFiles $c.PackageDir $NoticePatterns)) {
            $name = Split-Path -Leaf $file
            $sources.Add("notices $(Get-SourceName $file)")
            $notice = Read-Text $file
            $nhash = Get-Sha256 $notice
            [void]$s.Append("`nThird-party notices ($name in the package):`n`n")
            if ($seenNotices.ContainsKey($nhash)) {
                [void]$s.Append("The same as for $($seenNotices[$nhash]) above.`n")
            } else {
                $seenNotices[$nhash] = $c.Id
                [void]$s.Append($notice)
            }
        }
    }
    [void]$s.Append("`n")
    $sections.Add($s.ToString())
    $sourceNotes.Add("#   $($c.Id) $($c.Version): $($sources -join '; ')")
}

# A text in nuget-texts that no component needs (its package now has a license file, or is gone)
# is stale.
if (Test-Path -LiteralPath $textsPath) {
    foreach ($f in @(Get-ChildItem -LiteralPath $textsPath -File)) {
        if (-not $textsUsed.ContainsKey($f.Name)) {
            $problems.Add("$($f.FullName) is not needed by any shipped component; remove it")
        }
    }
}

if (-not (Test-Path $endUserPath)) {
    $problems.Add("$endUserPath not found")
}

if ($problems.Count -gt 0) {
    foreach ($p in $problems) { Write-Host "FAILED: $p" }
    exit 1
}

$out = New-Object System.Collections.Generic.List[string]
$out.Add('# Generated by scripts/check-nuget-licenses.ps1')
$out.Add('# NuGet packages of windows/CellGlide.App (both RIDs): id version license projectUrl')
foreach ($l in $lines) { $out.Add($l) }
$out.Add('#')
$out.Add('# Reviewed (not a plain MIT, Apache-2.0 or BSD license expression in the .nuspec), with what the')
$out.Add('# .nuspec declares (a license file by name and the first 16 hex digits of its SHA-256):')
foreach ($n in $notes) { $out.Add($n) }
$out.Add('#')
$out.Add('# Not shipped (used while building, or none of their files built or published), so not in')
$out.Add('# nuget-notices.txt:')
foreach ($key in $keys) {
    $id = $packages[$key].Id
    if ($NotShipped.Contains($id)) { $out.Add("#   ${id}: $($NotShipped[$id])") }
}
if (-not $NoExtra) {
    $out.Add('#')
    $out.Add('# Compiled into CellGlide.exe without being in the lock file (found through')
    $out.Add('# windows/CellGlide.App/obj/project.assets.json), so in nuget-notices.txt too:')
    foreach ($n in $extraNotes) { $out.Add($n) }
}
$out.Add('#')
$out.Add('# The full license texts and third-party notices of everything that ships, with the end-user terms')
$out.Add('# notice of the Windows App SDK terms (section 3.b.ii) first, are in nuget-notices.txt, which this')
$out.Add('# script also writes; the About page and THIRD-PARTY-LICENSES.txt show that file, not this one.')
$out.Add('# Where each section''s texts come from (package files relative to the NuGet cache):')
foreach ($n in $sourceNotes) { $out.Add($n) }
$text = ($out -join "`n") + "`n"

$notices = "# Generated by scripts/check-nuget-licenses.ps1`n" + (Read-Text $endUserPath) + "`n" + ($sections -join '')

if ($Check) {
    $stale = $false
    foreach ($pair in @(@($outPath, $text), @($noticesPath, $notices))) {
        $committed = if (Test-Path $pair[0]) { [IO.File]::ReadAllText($pair[0]) } else { '' }
        # Ordinal: a change of case in a license text is a change (PowerShell's -ne ignores case).
        if (-not [string]::Equals($committed, $pair[1], [StringComparison]::Ordinal)) {
            Write-Host "FAILED: $($pair[0]) is not what scripts/check-nuget-licenses.ps1 generates; rerun it without -Check and commit the result."
            $stale = $true
        }
    }
    if ($stale) { exit 1 }
    Write-Host "OK: $outPath and $noticesPath are up to date; every license is allowed or reviewed and every shipped component has its text."
} else {
    $enc = New-Object System.Text.UTF8Encoding($false)
    [IO.File]::WriteAllText($outPath, $text, $enc)
    [IO.File]::WriteAllText($noticesPath, $notices, $enc)
    Write-Host "Wrote $outPath ($($lines.Count) packages) and $noticesPath ($($sections.Count) components)."
}
exit 0
