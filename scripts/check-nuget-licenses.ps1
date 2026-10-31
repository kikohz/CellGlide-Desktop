# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check-nuget-licenses.ps1 [-Check]
#
# Lists the license of every NuGet package in windows/CellGlide.App/packages.lock.json (every
# framework and RID section; Direct, Transitive and CentralTransitive) into
# packaging/licenses/nuget.txt, one line per package: "id version license projectUrl", sorted by id.
# Each package's .nuspec is read from the NuGet global cache ($env:NUGET_PACKAGES, otherwise
# ~\.nuget\packages), so run a restore first.
#
# A license expression in $Allowed passes as is. Anything else (another expression, a license
# file, or only a licenseUrl) must be listed in $Reviewed below, pinned to the reviewed version and
# to what that version's .nuspec declares: the expression, the licenseUrl, or the license file's
# name and SHA-256. A new version, or a different declared license, fails until someone reviews it
# again and updates the entry. A package that would ship with the app under a license that is not
# permissive is a decision for a person, not for this table's editor.
#
# -Check: generate in memory and compare with the committed file; exit 1 if they differ or a
# license is not allowed or reviewed. Without -Check the file is rewritten.
# -LockFile, -PackagesDir, -OutFile: for scripts/tests/check-nuget-licenses.tests.ps1 (fixtures).
param(
    [switch]$Check,
    [string]$LockFile = '',
    [string]$PackagesDir = '',
    [string]$OutFile = ''
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$lockPath = if ($LockFile) { $LockFile } else { Join-Path $root 'windows\CellGlide.App\packages.lock.json' }
$outPath = if ($OutFile) { $OutFile } else { Join-Path $root 'packaging\licenses\nuget.txt' }
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

# Ordinal, case-insensitive order, so the file is the same whatever the machine's culture.
$keys = [string[]]@($packages.Keys)
[Array]::Sort($keys, [StringComparer]::OrdinalIgnoreCase)
foreach ($key in $keys) {
    $id = $packages[$key].Id
    $version = $packages[$key].Version
    $packageDir = Join-Path $cache (Join-Path $id.ToLowerInvariant() $version.ToLowerInvariant())
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
$out.Add('# Note for W4 (packaging): the Windows App SDK files ship under the Microsoft Software License')
$out.Add('# Terms, not a permissive license. The zip and the installer must carry the Windows App SDK license')
$out.Add('# text and an end-user terms notice (section 3.b.ii of those terms), and THIRD-PARTY-LICENSES must')
$out.Add('# list it.')
$text = ($out -join "`n") + "`n"

if ($problems.Count -gt 0) {
    foreach ($p in $problems) { Write-Host "FAILED: $p" }
    exit 1
}

if ($Check) {
    $committed = if (Test-Path $outPath) { [IO.File]::ReadAllText($outPath) } else { '' }
    if ($committed -ne $text) {
        Write-Host "FAILED: $outPath is not what scripts/check-nuget-licenses.ps1 generates; rerun it without -Check and commit the result."
        exit 1
    }
    Write-Host "OK: $outPath is up to date; every license is allowed or reviewed."
} else {
    [IO.File]::WriteAllText($outPath, $text, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "Wrote $outPath ($($lines.Count) packages)."
}
exit 0
