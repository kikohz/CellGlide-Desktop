# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\publish-windows.ps1 [-Dist <dir>] [-Commit <sha>] [-Upload]
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
#      ASCII only; their SHA-256 is printed (record it in the validation doc). They are tied to
#      the build (final review finding 2):
#      a. each zip's own top-level uninstall.ps1 (and install.ps1, should a zip ever carry one)
#         must be byte-identical to packaging\windows\'s, so Settings > Installed apps and
#         "irm .../uninstall.ps1 | iex" run the same script;
#      b. the checkout must be a git work tree in which packaging\windows\install.ps1 and
#         uninstall.ps1 have no uncommitted changes (published.sha256, which this script writes,
#         is not checked);
#      c. -Commit <sha> is commit C, the commit the CI release run built the zips from
#         ("gh run view <run> --json headSha -q .headSha"; the artifacts carry no commit of their
#         own). Both scripts must be the same at <sha> as in this checkout, so later commits
#         that only change docs are fine. -Upload refuses to run without -Commit; a dry run
#         checks it when given.
#   5. packaging\windows\published.sha256 (lines "<hex>  windows/<v>/<file>") either has no lines
#      for <v>, or records exactly these three versioned files with these hashes: bytes that were
#      published once are never changed.
# Upload order (Get-UploadPlan): the versioned files, then the latest.json pointer, then the two
# scripts, so at no moment does the one-line command point at a file that is not there yet.
#
# <v> must be X.Y.Z (it names bucket keys), and the -Dist path may not contain % ^ ! " < > | &
# (npx is npx.cmd, so cmd.exe parses the --file argument).
#
# With -Upload (needs npx; wrangler must be logged in to the Cloudflare account):
#   1. Before anything is put, each immutable key (windows/<v>/...) is read back from the bucket
#      through its public URL (a cache-busting query, so the CDN asks R2): 404 = absent; 200 = the
#      bytes must equal the local file, else the whole run is refused (published bytes never
#      change); any other answer or error stops the run (fail closed, never "absent").
#   2. Absent immutable keys are put; ones already there with the same bytes are skipped, never
#      put again. Then the mutable keys: windows/latest.json, uninstall.ps1, install.ps1.
#      Each put is "npx --yes wrangler@4.31.0 r2 object put cellglide-pkg/<key> ... --remote";
#      success is wrangler's exit code alone (its output is printed on failure), and the first
#      failure stops the run, so nothing after it goes live.
#   3. Every public URL must answer a HEAD with 200 and the file's Content-Length.
#   4. The version's three lines are appended to packaging\windows\published.sha256 (commit it),
#      and the path of release-windows.json (for CellProxy Website/release-windows.json) is printed.
# Exit code 0 when everything passed, 1 otherwise.
param(
    [string]$Dist = '',
    [string]$Commit = '',
    [switch]$Upload
)

$Bucket = 'cellglide-pkg'
$PublicBase = 'https://pkg.cellglide.com'
$Wrangler = 'wrangler@4.31.0'
$Arches = @('x64', 'arm64')
$ImmutableCache = 'public, max-age=31536000, immutable'
$TextType = 'text/plain; charset=utf-8'

function Test-ZipScripts([string]$Dist, [string]$Version, [string]$Root) {
    # Problems (strings) when a zip's top-level uninstall.ps1 or install.ps1 differs from
    # packaging\windows\'s; uninstall.ps1 must be in every zip.
    $ErrorActionPreference = 'Stop'
    Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
    $problems = @()
    foreach ($arch in $Arches) {
        $name = "CellGlide-$Version-windows-$arch.zip"
        try {
            $zip = [IO.Compression.ZipFile]::OpenRead((Join-Path $Dist $name))
        } catch {
            $problems += "$name cannot be opened as a zip: $($_.Exception.Message)"
            continue
        }
        try {
            foreach ($script in 'uninstall.ps1', 'install.ps1') {
                $entry = $zip.GetEntry($script)
                if (-not $entry) {
                    if ($script -eq 'uninstall.ps1') { $problems += "$name has no uninstall.ps1" }
                    continue
                }
                $stream = $entry.Open()
                try {
                    $buffer = New-Object IO.MemoryStream
                    $stream.CopyTo($buffer)
                    $inZip = $buffer.ToArray()
                } finally { $stream.Dispose() }
                $local = [IO.File]::ReadAllBytes((Join-Path $Root "packaging\windows\$script"))
                if (-not [Linq.Enumerable]::SequenceEqual([byte[]]$inZip, [byte[]]$local)) {
                    $problems += "$name's $script is not byte-identical to packaging\windows\$script (the zips were built from another commit)"
                }
            }
        } finally { $zip.Dispose() }
    }
    return $problems
}

function Invoke-Git([string]$Root, [string[]]$GitArgs) {
    # Runs git in $Root; returns its exit code and output lines.
    $ErrorActionPreference = 'Continue'
    $global:LASTEXITCODE = -1
    $out = @(& git -C $Root @GitArgs 2>&1 | ForEach-Object { "$_" })
    return [pscustomobject]@{ Code = $LASTEXITCODE; Out = $out }
}

function Test-ScriptsCommitted([string]$Root, [string]$Commit) {
    # Problems (strings) when install.ps1 / uninstall.ps1 are not committed, or (with $Commit)
    # differ from that commit's.
    $ErrorActionPreference = 'Stop'
    $paths = @('packaging/windows/install.ps1', 'packaging/windows/uninstall.ps1')
    $tree = Invoke-Git $Root @('rev-parse', '--is-inside-work-tree')
    if ($tree.Code -ne 0 -or ($tree.Out -join '') -ne 'true') {
        return @("$Root is not a git work tree, so the scripts cannot be tied to a commit")
    }
    $problems = @()
    $status = Invoke-Git $Root (@('status', '--porcelain', '--') + $paths)
    if ($status.Code -ne 0) { $problems += "git status failed: $($status.Out -join ' ')" }
    elseif (@($status.Out | Where-Object { $_ }).Count -gt 0) {
        $problems += "uncommitted changes to the scripts: $((@($status.Out | Where-Object { $_ })) -join '; ')"
    }
    if ($Commit) {
        if ($Commit -notmatch '\A[0-9a-fA-F]{7,40}\z') { return $problems + "-Commit '$Commit' is not a commit hash" }
        $exists = Invoke-Git $Root @('cat-file', '-e', "$Commit^{commit}")
        if ($exists.Code -ne 0) { return $problems + "commit $Commit is not in this repository (git fetch?)" }
        $diff = Invoke-Git $Root (@('diff', '--quiet', $Commit, '--') + $paths)
        if ($diff.Code -eq 1) { $problems += "install.ps1 or uninstall.ps1 here differs from commit $Commit, which the zips were built from" }
        elseif ($diff.Code -ne 0) { $problems += "git diff against $Commit failed: $($diff.Out -join ' ')" }
    }
    return $problems
}

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

function Invoke-WranglerPut($Item, [string]$Dist) {
    # Puts one plan item with wrangler. Returns exactly one [bool]: wrangler's exit code decides.
    # Its output is captured (never part of the return value) and printed when it fails.
    $ErrorActionPreference = 'Continue'   # native stderr through 2>&1 must not throw
    $npxArgs = Get-WranglerArgs $Item $Dist
    # When npx cannot start at all, PowerShell leaves $LASTEXITCODE as it was (possibly 0 from
    # an earlier put); -1 first, so that is never read as success.
    $global:LASTEXITCODE = -1
    $out = & npx @npxArgs 2>&1
    $code = $LASTEXITCODE
    if ($code -ne 0) {
        foreach ($line in @($out)) { Write-Host "  wrangler: $line" }
        Write-Host "  wrangler exit code: $code"
    }
    return [bool]($code -eq 0)
}

function Test-PutResult($Result) {
    # A put succeeded only when it returned exactly one [bool] that is $true; anything else (stray
    # output, nothing, a non-bool) counts as a failure.
    $r = @($Result)
    return [bool]($r.Count -eq 1 -and $r[0] -is [bool] -and $r[0])
}

function Test-ImmutableKey([string]$Key, [string]$Version) {
    return $Key.StartsWith("windows/$Version/", [StringComparison]::Ordinal)
}

function Get-RemoteState {
    # Whether an immutable key is already in the bucket: 'absent', 'same' (identical bytes) or
    # 'different'. Throws when it cannot tell, so callers fail closed.
    # $Fetch: param($Url, $OutFile) -> 200 (body written to $OutFile) or 404; throws otherwise.
    param($Item, [string]$Dist, [scriptblock]$Fetch)
    $ErrorActionPreference = 'Stop'
    $tmpDir = Join-Path ([IO.Path]::GetTempPath()) ('cellglide-publish-' + [guid]::NewGuid().ToString('N'))
    $null = New-Item -ItemType Directory -Path $tmpDir
    try {
        $tmp = Join-Path $tmpDir 'object'
        $answer = @(& $Fetch "$PublicBase/$($Item.Key)" $tmp)
        if ($answer.Count -ne 1) { throw "reading $($Item.Key) back gave no clear answer" }
        if ($answer[0] -is [int] -and $answer[0] -eq 404) { return 'absent' }
        if (-not ($answer[0] -is [int] -and $answer[0] -eq 200) -or -not (Test-Path -LiteralPath $tmp -PathType Leaf)) {
            throw "reading $($Item.Key) back answered '$($answer[0])'"
        }
        $remote = (Get-FileHash -LiteralPath $tmp -Algorithm SHA256).Hash
        $local = (Get-FileHash -LiteralPath (Join-Path $Dist $Item.File) -Algorithm SHA256).Hash
        if ($remote -eq $local) { return 'same' }
        return 'different'
    } finally {
        Remove-Item -LiteralPath $tmpDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-PublicFetch([string]$Url, [string]$OutFile) {
    # The default $Fetch: GET the public URL with a unique query, so the CDN does not answer from
    # its cache but asks R2. 200 -> body in $OutFile; 404 -> 404; anything else throws.
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    $protocols = [Net.ServicePointManager]::SecurityProtocol
    try {
        [Net.ServicePointManager]::SecurityProtocol = $protocols -bor [Net.SecurityProtocolType]::Tls12
        try {
            Invoke-WebRequest -UseBasicParsing -Uri ($Url + '?cellglide-verify=' + [guid]::NewGuid().ToString('N')) -OutFile $OutFile -Headers @{ 'Cache-Control' = 'no-cache' }
            return 200
        } catch [System.Net.WebException] {
            $response = $_.Exception.Response
            if ($response -and [int]$response.StatusCode -eq 404) { return 404 }
            throw
        }
    } finally {
        [Net.ServicePointManager]::SecurityProtocol = $protocols
    }
}

function Invoke-PublishWindows {
    # The whole run; returns the exit code. -Root, -HasNpx, -Put, -Fetch and -Head exist for the
    # tests (a temporary repository and a fake bucket); the defaults are the real repository, npx
    # and pkg.cellglide.com.
    param(
        [string]$Dist = '',
        [string]$Commit = '',
        [switch]$Upload,
        [string]$Root = (Split-Path -Parent $PSScriptRoot),
        [scriptblock]$HasNpx = { [bool](Get-Command npx -ErrorAction SilentlyContinue) },
        [scriptblock]$Put = { param($Item, $Dist) Invoke-WranglerPut $Item $Dist },
        [scriptblock]$Fetch = { param($Url, $OutFile) Invoke-PublicFetch $Url $OutFile },
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
        if ($Version -notmatch '\A\d+\.\d+\.\d+\z') {
            Write-Host "error: the workspace version '$Version' is not X.Y.Z; it would name bucket keys"
            return 1
        }
        if (-not $Dist) { $Dist = Join-Path $Root "dist\windows\$Version" }
        if (-not [IO.Path]::IsPathRooted($Dist)) { $Dist = Join-Path (Get-Location).Path $Dist }
        $Dist = [IO.Path]::GetFullPath($Dist)
        if ($Dist -match '[%^!"<>|&]') {
            Write-Host "error: the -Dist path '$Dist' contains one of % ^ ! `" < > | &, which cmd.exe (npx.cmd) would mangle; use another folder"
            return 1
        }
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
            Write-Host "$script SHA-256 $((Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash.ToLowerInvariant())"
        }
        # 4a-c. The scripts are the ones the zips were built with, and committed.
        $bound = @(Test-ZipScripts $Dist $Version $Root) + @(Test-ScriptsCommitted $Root $Commit)
        if ($Upload -and -not $Commit) { $bound += '-Upload needs -Commit <sha>: the commit the CI release run built the zips from' }
        if ($bound.Count -gt 0) {
            foreach ($p in $bound) { Write-Host "error: $p" }
            return 1
        }
        if ($Commit) { Write-Host "the scripts are committed and the same as at $Commit" }
        else { Write-Host 'the scripts are committed (no -Commit given: not compared with the build commit)' }

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
            Write-Host "$Version is already in published.sha256 with these exact bytes; an upload skips the windows/$Version/ objects the bucket already has."
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

        # Immutable keys: what does the bucket already hold? Fail closed; nothing is put until
        # every one of them is known to be absent or identical.
        $skip = @{}
        $different = 0
        foreach ($item in @($plan | Where-Object { Test-ImmutableKey $_.Key $Version })) {
            try {
                $state = Get-RemoteState -Item $item -Dist $Dist -Fetch $Fetch
            } catch {
                Write-Host "error: cannot tell whether $($item.Key) is already published ($($_.Exception.Message)); nothing was uploaded"
                return 1
            }
            switch ($state) {
                'absent' { Write-Host "bucket: $($item.Key) is not there yet" }
                'same' { Write-Host "bucket: $($item.Key) is already there with these bytes; it is not put again"; $skip[$item.Key] = $true }
                default { Write-Host "error: bucket: $($item.Key) is already published with different bytes; published bytes never change"; $different++ }
            }
        }
        if ($different -gt 0) { Write-Host 'error: refusing to upload; nothing was uploaded'; return 1 }

        # Puts, in plan order (immutable keys first, then latest.json and the scripts); the first
        # failure stops the run.
        foreach ($item in $plan) {
            if ($skip.ContainsKey($item.Key)) { continue }
            Write-Host "put $Bucket/$($item.Key)"
            $ok = $false
            try { $ok = Test-PutResult (& $Put $item $Dist) } catch { Write-Host "error: $($_.Exception.Message)" }
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

$code = Invoke-PublishWindows -Dist $Dist -Commit $Commit -Upload:$Upload
exit $code
