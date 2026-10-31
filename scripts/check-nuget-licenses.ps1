# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check-nuget-licenses.ps1 [-Check]
#
# Lists the license of every NuGet package in windows/CellGlide.App/packages.lock.json (every
# framework and RID section; Direct, Transitive and CentralTransitive) into
# packaging/licenses/nuget.txt, one line per package: "id version license projectUrl", sorted by id.
# Each package's .nuspec is read from the NuGet global cache ($env:NUGET_PACKAGES, otherwise
# ~\.nuget\packages), so run a restore first.
#
# A license expression in $Allowed passes as is. Anything else (another expression, a license
# file, or only a licenseUrl) must be listed in $Reviewed below with the license's name and the
# reason it is acceptable; otherwise the script fails. A package that would ship with the app under
# a license that is not permissive is a decision for a person, not for this table's editor.
#
# -Check: generate in memory and compare with the committed file; exit 1 if they differ or a
# license is not allowed or reviewed. Without -Check the file is rewritten.
param(
    [switch]$Check
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$lockPath = Join-Path $root 'windows\CellGlide.App\packages.lock.json'
$outPath = Join-Path $root 'packaging\licenses\nuget.txt'

$Allowed = @('MIT', 'Apache-2.0', 'BSD-2-Clause', 'BSD-3-Clause')

$WinAppSdk = 'Microsoft Software License Terms (Windows App SDK), redistributable per section 3.a.i'
$WinAppSdkShipped = 'the WinUI 3 runtime the app is built on (spec 9.1); its files are carried in the self-contained publish folder, which section 3.a.i of its terms allows'
$WinAppSdkExcluded = 'pulled in by the Microsoft.WindowsAppSDK metapackage; CellGlide.App references it with ExcludeAssets="all", so none of its files are built or published'
$SdkBuildOnly = 'Microsoft Software License Terms (Windows SDK), build only, not shipped'

# id -> license name and reason. Keyed by id: a new version keeps its review, but the license
# metadata is shown in nuget.txt, so a change there shows up in the -Check diff.
$Reviewed = [ordered]@{
    'Microsoft.WindowsAppSDK'                        = @{ License = $WinAppSdk; Reason = 'metapackage (no files of its own); ' + $WinAppSdkShipped }
    'Microsoft.WindowsAppSDK.Base'                   = @{ License = $WinAppSdk; Reason = $WinAppSdkShipped }
    'Microsoft.WindowsAppSDK.DWrite'                 = @{ License = $WinAppSdk; Reason = $WinAppSdkShipped }
    'Microsoft.WindowsAppSDK.Foundation'             = @{ License = $WinAppSdk; Reason = $WinAppSdkShipped }
    'Microsoft.WindowsAppSDK.InteractiveExperiences' = @{ License = $WinAppSdk; Reason = $WinAppSdkShipped }
    'Microsoft.WindowsAppSDK.Runtime'                = @{ License = $WinAppSdk; Reason = $WinAppSdkShipped }
    'Microsoft.WindowsAppSDK.Widgets'                = @{ License = $WinAppSdk; Reason = $WinAppSdkShipped }
    'Microsoft.WindowsAppSDK.WinUI'                  = @{ License = $WinAppSdk; Reason = $WinAppSdkShipped }
    'Microsoft.WindowsAppSDK.AI'                     = @{ License = $WinAppSdk; Reason = $WinAppSdkExcluded }
    'Microsoft.WindowsAppSDK.ML'                     = @{ License = $WinAppSdk; Reason = $WinAppSdkExcluded }
    'Microsoft.WindowsAppSDK.Search'                 = @{ License = $WinAppSdk; Reason = $WinAppSdkExcluded }
    'Microsoft.Windows.AI.MachineLearning'           = @{ License = $WinAppSdk; Reason = '(its license file is the Windows ML Runtime terms) ' + $WinAppSdkExcluded }
    'Microsoft.Web.WebView2'                         = @{ License = 'BSD-3-Clause'; Reason = 'its LICENSE.txt is the BSD 3-clause text; shipped (a dependency of the WinUI package)' }
    'Microsoft.Windows.SDK.BuildTools'               = @{ License = $SdkBuildOnly; Reason = 'build tools (Windows SDK metadata and tools), used only while building' }
    'Microsoft.Windows.SDK.BuildTools.MSIX'          = @{ License = $SdkBuildOnly; Reason = 'MSIX build tools that make resources.pri, used only while building' }
}

$cache = $env:NUGET_PACKAGES
if (-not $cache) { $cache = Join-Path $env:USERPROFILE '.nuget\packages' }

function Get-MetadataText([xml]$doc, [string]$name) {
    $node = $doc.SelectSingleNode("/*[local-name()='package']/*[local-name()='metadata']/*[local-name()='$name']")
    if ($node) { return $node } else { return $null }
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
$problems = New-Object System.Collections.Generic.List[string]
$reviewedUsed = New-Object System.Collections.Generic.List[string]

# Ordinal, case-insensitive order, so the file is the same whatever the machine's culture.
$keys = [string[]]@($packages.Keys)
[Array]::Sort($keys, [StringComparer]::OrdinalIgnoreCase)
foreach ($key in $keys) {
    $id = $packages[$key].Id
    $version = $packages[$key].Version
    $nuspec = Join-Path $cache (Join-Path $id.ToLowerInvariant() (Join-Path $version.ToLowerInvariant() ($id.ToLowerInvariant() + '.nuspec')))
    if (-not (Test-Path $nuspec)) {
        $problems.Add("$id ${version}: $nuspec not found (run dotnet restore first)")
        continue
    }
    [xml]$doc = Get-Content -Raw $nuspec
    $licenseNode = Get-MetadataText $doc 'license'
    $licenseUrlNode = Get-MetadataText $doc 'licenseUrl'
    $projectUrlNode = Get-MetadataText $doc 'projectUrl'
    $projectUrl = if ($projectUrlNode) { $projectUrlNode.InnerText.Trim() } else { '-' }

    $expression = $null
    if ($licenseNode -and $licenseNode.GetAttribute('type') -eq 'expression') {
        $expression = $licenseNode.InnerText.Trim()
    }

    if ($expression -and ($Allowed -contains $expression)) {
        $license = $expression
    } elseif ($Reviewed.Contains($id)) {
        $license = $Reviewed[$id].License
        $reviewedUsed.Add($id)
    } else {
        $declared = if ($expression) { "expression $expression" }
            elseif ($licenseNode) { "$($licenseNode.GetAttribute('type')) $($licenseNode.InnerText.Trim())" }
            elseif ($licenseUrlNode) { "licenseUrl $($licenseUrlNode.InnerText.Trim())" }
            else { 'no license metadata' }
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
$out.Add('# Reviewed (not a plain MIT, Apache-2.0 or BSD license expression in the .nuspec):')
$reviewedIds = [string[]]@($reviewedUsed | Select-Object -Unique)
[Array]::Sort($reviewedIds, [StringComparer]::OrdinalIgnoreCase)
foreach ($id in $reviewedIds) {
    $out.Add("#   ${id}: $($Reviewed[$id].Reason)")
}
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
