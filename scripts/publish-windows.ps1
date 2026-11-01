# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\publish-windows.ps1 [-Dist <dir>] [-Upload]
#
# Publishes a Windows release to pkg.cellglide.com (Cloudflare R2 bucket cellglide-pkg). Without
# -Upload (the default) it is a dry run: everything is checked, the upload plan is printed, and
# nothing is uploaded or written outside -Dist.
#
# -Dist (default dist\windows\<workspace version>\) must hold the five release files:
#   CellGlide-<v>-windows-x64.zip  CellGlide-<v>-windows-arm64.zip  SHA256SUMS  latest.json
#   release-windows.json
# (scripts\release-windows-json.py writes the last three from the zips and SHA256SUMS.<arch>).
# <v> is Cargo.toml's [workspace.package] version. Checks, in order:
#   1. the five files are there;
#   2. SHA256SUMS lists exactly the two zips, and their actual SHA-256 matches;
#   3. latest.json and release-windows.json agree with SHA256SUMS and the files (version, url,
#      sha256, size, test_build) and use one base URL; data made with a loopback --base (a local
#      rehearsal) is reported, and -Upload refuses it;
#   4. install.ps1 and uninstall.ps1 are taken from packaging\windows\ (copied into -Dist) and are
#      ASCII only;
#   5. packaging\windows\published.sha256 (lines "<hex>  windows/<v>/<file>") either has no lines
#      for <v>, or records exactly these three versioned files with these hashes: bytes that were
#      published once are never changed.
# Upload order (Get-UploadPlan): the versioned files, then the latest.json pointer, then the two
# scripts, so at no moment does the one-line command point at a file that is not there yet.
#
# With -Upload (needs npx; wrangler must be logged in to the Cloudflare account): each file is put
# with "npx --yes wrangler@4.31.0 r2 object put cellglide-pkg/<key> ... --remote", stopping at the
# first failure; then every public URL must answer a HEAD with 200 and the file's Content-Length;
# then the version's three lines are appended to packaging\windows\published.sha256 (commit it),
# and the path of release-windows.json (for CellProxy Website/release-windows.json) is printed.
# Exit code 0 when everything passed, 1 otherwise.
param(
    [string]$Dist = '',
    [switch]$Upload
)

$Bucket = 'cellglide-pkg'
$PublicBase = 'https://pkg.cellglide.com'
$Wrangler = 'wrangler@4.31.0'
$Arches = @('x64', 'arm64')
$ImmutableCache = 'public, max-age=31536000, immutable'
$TextType = 'text/plain; charset=utf-8'

function Get-WorkspaceVersion([string]$CargoToml) {
    $ErrorActionPreference = 'Stop'
    $section = ''
    foreach ($line in [IO.File]::ReadAllLines($CargoToml)) {
        $trimmed = $line.Trim()
        if ($trimmed -match '^\[(.+)\]$') { $section = $Matches[1].Trim(); continue }
        if ($section -eq 'workspace.package' -and $trimmed -match '^version\s*=\s*"([^"]+)"') { return $Matches[1] }
    }
    throw "no version in the [workspace.package] section of $CargoToml"
}

function Get-UploadPlan([string]$Version) {
    # Key in the bucket, file name in -Dist, Content-Type, Cache-Control; in upload order.
    $ErrorActionPreference = 'Stop'
    $plan = @()
    foreach ($arch in $Arches) {
        $name = "CellGlide-$Version-windows-$arch.zip"
        $plan += [pscustomobject]@{ Key = "windows/$Version/$name"; File = $name; ContentType = 'application/zip'; CacheControl = $ImmutableCache }
    }
    $plan += [pscustomobject]@{ Key = "windows/$Version/SHA256SUMS"; File = 'SHA256SUMS'; ContentType = $TextType; CacheControl = $ImmutableCache }
    $plan += [pscustomobject]@{ Key = 'windows/latest.json'; File = 'latest.json'; ContentType = 'application/json'; CacheControl = 'no-cache' }
    $plan += [pscustomobject]@{ Key = 'uninstall.ps1'; File = 'uninstall.ps1'; ContentType = $TextType; CacheControl = 'no-cache' }
    $plan += [pscustomobject]@{ Key = 'install.ps1'; File = 'install.ps1'; ContentType = $TextType; CacheControl = 'no-cache' }
    return $plan
}

function Get-VersionedFiles([string]$Version) {
    # The files under windows/<v>/, which are immutable once published.
    return @("CellGlide-$Version-windows-x64.zip", "CellGlide-$Version-windows-arm64.zip", 'SHA256SUMS')
}

function Test-PublishedRecord([string]$RecordText, [string]$Version, [hashtable]$Hashes) {
    # Problems (none = may publish). $Hashes: file name -> SHA-256 of what is about to be uploaded.
    # When the record already has lines for $Version, every recorded file must be about to be
    # uploaded with the recorded hash, and no new versioned file may join that version.
    $ErrorActionPreference = 'Stop'
    $problems = @()
    $recorded = @{}
    $prefix = "windows/$Version/"
    foreach ($line in ($RecordText -split "`r?`n")) {
        if ($line.Trim() -eq '' -or $line.TrimStart().StartsWith('#')) { continue }
        if ($line -notmatch '\A([0-9a-fA-F]{64})  (windows/[^/\s]+/[^/\s]+)\z') {
            $problems += "published.sha256: bad line '$line'"
            continue
        }
        $path = $Matches[2]
        if ($path.StartsWith($prefix, [StringComparison]::Ordinal)) {
            $name = $path.Substring($prefix.Length)
            if ($recorded.ContainsKey($name)) { $problems += "published.sha256: $path is listed twice"; continue }
            $recorded[$name] = $Matches[1].ToLowerInvariant()
        }
    }
    if ($recorded.Count -eq 0) { return $problems }
    foreach ($name in ($recorded.Keys | Sort-Object)) {
        if (-not $Hashes.ContainsKey($name)) {
            $problems += "$prefix$name was published but is not in this release"
        } elseif ([string]$Hashes[$name] -ine $recorded[$name]) {
            $problems += "$prefix$name differs from the bytes published earlier ($($recorded[$name])); published bytes never change"
        }
    }
    foreach ($name in ($Hashes.Keys | Sort-Object)) {
        if (-not $recorded.ContainsKey($name)) { $problems += "$prefix$name is new, but $Version was already published; a published version cannot gain files" }
    }
    return $problems
}

function Test-AsciiOnly([byte[]]$Bytes) {
    foreach ($b in $Bytes) { if ($b -gt 0x7F) { return $false } }
    return $true
}

function Read-SumsFile([string]$Text) {
    # "<hex>  <name>" lines -> ordered name -> lower-case hash; a malformed or repeated line throws.
    $ErrorActionPreference = 'Stop'
    $sums = [ordered]@{}
    foreach ($line in ($Text -split "`n")) {
        if ($line -eq '') { continue }
        if ($line -notmatch '\A([0-9a-f]{64})  (\S+)\z') { throw "SHA256SUMS: bad line '$line'" }
        if ($sums.Contains($Matches[2])) { throw "SHA256SUMS: $($Matches[2]) is listed twice" }
        $sums[$Matches[2]] = $Matches[1]
    }
    return $sums
}

function Test-ReleaseData {
    # Problems with SHA256SUMS, latest.json and release-windows.json in $Dist (none = consistent),
    # plus the base URL they were made for (Base; empty when it cannot be told).
    param([string]$Dist, [string]$Version)
    $ErrorActionPreference = 'Stop'
    $problems = @()
    $base = ''
    $sizes = @{}
    $hashes = @{}
    foreach ($name in (Get-VersionedFiles $Version)) {
        $path = Join-Path $Dist $name
        $sizes[$name] = (Get-Item -LiteralPath $path).Length
        $hashes[$name] = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
    }
    $zips = @($Arches | ForEach-Object { "CellGlide-$Version-windows-$_.zip" })

    try {
        $sums = Read-SumsFile ([IO.File]::ReadAllText((Join-Path $Dist 'SHA256SUMS')))
        $listed = @($sums.Keys)
        $expected = [string[]]@($zips)
        [Array]::Sort($expected, [StringComparer]::Ordinal)
        if (($listed -join '|') -cne ($expected -join '|')) {
            $problems += "SHA256SUMS must list exactly $($expected -join ' and ') (sorted by name), it lists $($listed -join ', ')"
        }
        foreach ($zip in $zips) {
            if ($sums.Contains($zip) -and $sums[$zip] -ne $hashes[$zip]) { $problems += "SHA256SUMS: $zip is $($hashes[$zip]), SHA256SUMS says $($sums[$zip])" }
        }
    } catch {
        $problems += $_.Exception.Message
    }

    try {
        $latest = [IO.File]::ReadAllText((Join-Path $Dist 'latest.json')) | ConvertFrom-Json
        $site = [IO.File]::ReadAllText((Join-Path $Dist 'release-windows.json')) | ConvertFrom-Json
    } catch {
        return [pscustomobject]@{ Problems = @($problems + "latest.json or release-windows.json does not parse: $($_.Exception.Message)"); Base = '' }
    }
    if ($latest.version -cne $Version) { $problems += "latest.json: version is '$($latest.version)', expected $Version" }
    if ($site.version -cne $Version) { $problems += "release-windows.json: version is '$($site.version)', expected $Version" }
    $bases = @()
    $files = @($site.files)
    if ($files.Count -ne $Arches.Count) { $problems += "release-windows.json: files must have $($Arches.Count) entries" }
    for ($i = 0; $i -lt $Arches.Count; $i++) {
        $arch = $Arches[$i]
        $zip = $zips[$i]
        $entry = $latest.$arch
        if ($null -eq $entry) { $problems += "latest.json: no entry for $arch"; continue }
        $suffix = "/windows/$Version/$zip"
        if (-not ($entry.url -is [string]) -or -not $entry.url.EndsWith($suffix, [StringComparison]::Ordinal)) {
            $problems += "latest.json: the $arch url '$($entry.url)' does not end with $suffix"
        } else {
            $bases += $entry.url.Substring(0, $entry.url.Length - $suffix.Length)
        }
        if ($entry.sha256 -cne $hashes[$zip]) { $problems += "latest.json: the $arch sha256 is not $zip's ($($hashes[$zip]))" }
        if (-not ($entry.size -is [int] -or $entry.size -is [long]) -or [long]$entry.size -ne $sizes[$zip]) { $problems += "latest.json: the $arch size is not $zip's ($($sizes[$zip]))" }
        if (-not ($entry.test_build -is [bool])) { $problems += "latest.json: bad test_build for $arch" }
        if ($arch -eq 'x64' -and $entry.test_build -eq $true) { $problems += 'latest.json: x64 is marked as a test build' }
        $file = if ($i -lt $files.Count) { $files[$i] } else { $null }
        if ($null -eq $file -or $file.kind -cne 'zip' -or $file.arch -cne $arch -or $file.url -cne $entry.url -or
            $file.sha256 -cne $entry.sha256 -or [string]$file.size -cne [string]$entry.size -or $file.test_build -ne $entry.test_build) {
            $problems += "release-windows.json: the $arch zip entry does not match latest.json"
        }
    }
    $bases = @($bases | Select-Object -Unique)
    if ($bases.Count -gt 1) { $problems += "latest.json: the urls use different bases ($($bases -join ', '))" }
    if ($bases.Count -eq 1) {
        $base = $bases[0]
        if ($site.install -cne "irm $base/install.ps1 | iex") { $problems += "release-windows.json: install is '$($site.install)'" }
        if ($site.uninstall -cne "irm $base/uninstall.ps1 | iex") { $problems += "release-windows.json: uninstall is '$($site.uninstall)'" }
        if ($base -cne $PublicBase -and $base -notmatch '\Ahttp://(127\.0\.0\.1|localhost):[0-9]{1,5}\z') { $problems += "latest.json: unexpected base $base" }
    }
    return [pscustomobject]@{ Problems = @($problems); Base = $base }
}

function Get-WranglerArgs($Item, [string]$Dist) {
    # The npx arguments that put one plan item into the bucket.
    return @('--yes', $Wrangler, 'r2', 'object', 'put', "$Bucket/$($Item.Key)", '--file', (Join-Path $Dist $Item.File),
        '--content-type', $Item.ContentType, '--cache-control', $Item.CacheControl, '--remote')
}

function Invoke-PublishWindows {
    # The whole run; returns the exit code. -Root, -HasNpx, -Put and -Head exist for the tests
    # (a temporary repository and a fake bucket); the defaults are the real repository, npx and
    # pkg.cellglide.com.
    param(
        [string]$Dist = '',
        [switch]$Upload,
        [string]$Root = (Split-Path -Parent $PSScriptRoot),
        [scriptblock]$HasNpx = { [bool](Get-Command npx -ErrorAction SilentlyContinue) },
        [scriptblock]$Put = { param($Item, $Dist)
            $npxArgs = Get-WranglerArgs $Item $Dist
            & npx @npxArgs
            return ($LASTEXITCODE -eq 0) },
        [scriptblock]$Head = { param($Url)
            $protocols = [Net.ServicePointManager]::SecurityProtocol
            try {
                [Net.ServicePointManager]::SecurityProtocol = $protocols -bor [Net.SecurityProtocolType]::Tls12
                $r = Invoke-WebRequest -Method Head -UseBasicParsing -Uri $Url
                return [pscustomobject]@{ Status = [int]$r.StatusCode; Length = [long]$r.Headers['Content-Length'] }
            } finally { [Net.ServicePointManager]::SecurityProtocol = $protocols } }
    )
    $ErrorActionPreference = 'Stop'
    try {
        $Version = Get-WorkspaceVersion (Join-Path $Root 'Cargo.toml')
        if (-not $Dist) { $Dist = Join-Path $Root "dist\windows\$Version" }
        if (-not [IO.Path]::IsPathRooted($Dist)) { $Dist = Join-Path (Get-Location).Path $Dist }
        $Dist = [IO.Path]::GetFullPath($Dist)
        $RecordPath = Join-Path $Root 'packaging\windows\published.sha256'
        Write-Host "CellGlide $Version for Windows, from $Dist"

        # 1. The five release files.
        $missing = @()
        foreach ($name in @((Get-VersionedFiles $Version) + 'latest.json' + 'release-windows.json')) {
            if (-not (Test-Path -LiteralPath (Join-Path $Dist $name) -PathType Leaf)) { $missing += $name }
        }
        if ($missing.Count -gt 0) {
            Write-Host "error: missing from ${Dist}: $($missing -join ', ')"
            return 1
        }

        # 2-3. SHA256SUMS, latest.json and release-windows.json agree with the files.
        $data = Test-ReleaseData -Dist $Dist -Version $Version
        if ($data.Problems.Count -gt 0) {
            foreach ($p in $data.Problems) { Write-Host "error: $p" }
            return 1
        }
        $production = $data.Base -ceq $PublicBase
        if (-not $production) {
            Write-Host "note: this release data points at $($data.Base) (a local rehearsal); it cannot be uploaded."
            Write-Host '      Regenerate it with scripts\release-windows-json.py without --base before publishing.'
        }

        # 4. The two scripts, from packaging\windows\, ASCII only.
        foreach ($script in 'install.ps1', 'uninstall.ps1') {
            $source = Join-Path $Root "packaging\windows\$script"
            if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { Write-Host "error: $source is missing"; return 1 }
            $bytes = [IO.File]::ReadAllBytes($source)
            if (-not (Test-AsciiOnly $bytes)) { Write-Host "error: $source has non-ASCII bytes (Windows PowerShell 5.1 would misread it)"; return 1 }
            [IO.File]::WriteAllBytes((Join-Path $Dist $script), $bytes)
        }

        # 5. Published bytes never change.
        $hashes = @{}
        foreach ($name in (Get-VersionedFiles $Version)) {
            $hashes[$name] = (Get-FileHash -LiteralPath (Join-Path $Dist $name) -Algorithm SHA256).Hash.ToLowerInvariant()
        }
        $recordText = if (Test-Path -LiteralPath $RecordPath -PathType Leaf) { [IO.File]::ReadAllText($RecordPath) } else { '' }
        $recordProblems = @(Test-PublishedRecord $recordText $Version $hashes)
        if ($recordProblems.Count -gt 0) {
            foreach ($p in $recordProblems) { Write-Host "error: $p" }
            return 1
        }
        $alreadyPublished = $recordText -match ('(?m)^[0-9a-fA-F]{64}  windows/' + [regex]::Escape($Version) + '/')
        $newLines = @((Get-VersionedFiles $Version) | ForEach-Object { "$($hashes[$_])  windows/$Version/$_" })

        # The plan.
        $plan = @(Get-UploadPlan $Version)
        Write-Host "Upload plan ($Bucket, in this order):"
        foreach ($item in $plan) {
            $size = (Get-Item -LiteralPath (Join-Path $Dist $item.File)).Length
            Write-Host ("  {0}  [{1}; {2}]  {3} bytes" -f $item.Key, $item.ContentType, $item.CacheControl, $size)
        }
        if ($alreadyPublished) {
            Write-Host "$Version is already in published.sha256 with these exact bytes; uploading it again changes nothing under windows/$Version/."
        }
        if (-not $Upload) {
            if (-not $alreadyPublished) {
                Write-Host 'A completed upload would add to packaging\windows\published.sha256:'
                foreach ($l in $newLines) { Write-Host "  $l" }
            }
            Write-Host 'dry run: nothing uploaded'
            return 0
        }

        # The upload.
        if (-not $production) { Write-Host "error: refusing to upload release data made for $($data.Base)"; return 1 }
        if (-not (& $HasNpx)) { Write-Host 'error: npx is not available (Node.js runs wrangler)'; return 1 }
        foreach ($item in $plan) {
            Write-Host "put $Bucket/$($item.Key)"
            $ok = $false
            try { $ok = [bool](& $Put $item $Dist) } catch { Write-Host "error: $($_.Exception.Message)" }
            if (-not $ok) {
                Write-Host "error: uploading $($item.Key) failed; stopped here (later files were not uploaded)"
                return 1
            }
        }
        $bad = 0
        foreach ($item in $plan) {
            $url = "$PublicBase/$($item.Key)"
            $size = (Get-Item -LiteralPath (Join-Path $Dist $item.File)).Length
            try {
                $r = & $Head $url
                if ($r.Status -ne 200 -or [long]$r.Length -ne $size) {
                    Write-Host "error: HEAD $url answered $($r.Status) with Content-Length $($r.Length), expected 200 and $size"
                    $bad++
                } else {
                    Write-Host "ok: $url ($size bytes)"
                }
            } catch {
                Write-Host "error: HEAD ${url}: $($_.Exception.Message)"
                $bad++
            }
        }
        if ($bad -gt 0) { return 1 }

        if (-not $alreadyPublished) {
            $text = $recordText
            if ($text -ne '' -and -not $text.EndsWith("`n")) { $text += "`n" }
            $text += (($newLines -join "`n") + "`n")
            [IO.File]::WriteAllText($RecordPath, $text, (New-Object Text.UTF8Encoding $false))
            Write-Host "added $Version to packaging\windows\published.sha256"
        }
        Write-Host 'commit packaging/windows/published.sha256'
        Write-Host "Website data: $(Join-Path $Dist 'release-windows.json') (copy to CellProxy Website/release-windows.json)"
        return 0
    } catch {
        Write-Host "error: $($_.Exception.Message)"
        return 1
    }
}

$code = Invoke-PublishWindows -Dist $Dist -Upload:$Upload
exit $code
