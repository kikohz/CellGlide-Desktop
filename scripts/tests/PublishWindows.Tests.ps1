# Pester 5 tests for scripts\publish-windows.ps1 (run by scripts\test-windows-scripts.ps1).
# Only the script's functions and constants are loaded (Import-ScriptFunctions). The whole run is
# exercised against a temporary repository and a fake bucket (a folder): nothing is uploaded, npx
# and wrangler are never started, and pkg.cellglide.com is never contacted.
BeforeAll { . "$PSScriptRoot\Import-ScriptFunctions.ps1"; . Import-ScriptFunctions "$PSScriptRoot\..\publish-windows.ps1"
    $script:ScriptFile = "$PSScriptRoot\..\publish-windows.ps1"

    function Get-Sha([string]$Path) { (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant() }
    function Write-Lf([string]$Path, [string]$Text) { [IO.File]::WriteAllText($Path, $Text, (New-Object Text.UTF8Encoding $false)) }

    # A repository with Cargo.toml 1.0.0, the two scripts, the record header and a consistent
    # dist\windows\1.0.0\ made for $Base (as release-windows-json.py writes it).
    function New-FakeRepo([string]$Root, [string]$Base = 'https://pkg.cellglide.com') {
        $null = New-Item -ItemType Directory -Force (Join-Path $Root 'packaging\windows'), (Join-Path $Root 'dist\windows\1.0.0')
        Write-Lf (Join-Path $Root 'Cargo.toml') "[workspace]`n[workspace.package]`nversion = `"1.0.0`"`n"
        Write-Lf (Join-Path $Root 'packaging\windows\install.ps1') "Write-Host 'install'`n"
        Write-Lf (Join-Path $Root 'packaging\windows\uninstall.ps1') "Write-Host 'uninstall'`n"
        Write-Lf (Join-Path $Root 'packaging\windows\published.sha256') "# header`n"
        $dist = Join-Path $Root 'dist\windows\1.0.0'
        $latest = [ordered]@{ version = '1.0.0' }
        $files = @()
        foreach ($arch in 'x64', 'arm64') {
            $name = "CellGlide-1.0.0-windows-$arch.zip"
            [IO.File]::WriteAllBytes((Join-Path $dist $name), [Text.Encoding]::ASCII.GetBytes("zip bytes $arch"))
            $entry = [ordered]@{ url = "$Base/windows/1.0.0/$name"; sha256 = (Get-Sha (Join-Path $dist $name)); size = (Get-Item (Join-Path $dist $name)).Length; test_build = ($arch -eq 'arm64') }
            $latest[$arch] = $entry
            $files += [ordered]@{ kind = 'zip'; arch = $arch; url = $entry.url; size = $entry.size; sha256 = $entry.sha256; test_build = $entry.test_build }
        }
        Write-Lf (Join-Path $dist 'SHA256SUMS') ("{0}  CellGlide-1.0.0-windows-arm64.zip`n{1}  CellGlide-1.0.0-windows-x64.zip`n" -f $latest.arm64.sha256, $latest.x64.sha256)
        Write-Lf (Join-Path $dist 'latest.json') (($latest | ConvertTo-Json -Depth 5) + "`n")
        $site = [ordered]@{ version = '1.0.0'; date = '2026-11-05'; install = "irm $Base/install.ps1 | iex"; uninstall = "irm $Base/uninstall.ps1 | iex"; files = $files }
        Write-Lf (Join-Path $dist 'release-windows.json') (($site | ConvertTo-Json -Depth 5) + "`n")
        return $dist
    }

    # Runs Invoke-PublishWindows against the fake repository and a fake bucket; returns the exit
    # code, the printed text, the keys put (in order), the keys read back and the bucket folder.
    #   -FailKey        that put fails (returns $false)
    #   -NoisyFailKey   that put writes wrangler-like lines, then fails, like a real failed put
    #   -NoisyOkKey     that put writes lines, then returns $true (stray output: must count as failed)
    #   -FetchError     reading that key back throws (network, TLS, auth): -FetchStatus answers it
    #   -FetchStatus    reading that key back answers this status instead of 200/404
    function Invoke-Fake {
        param([string]$Root, [switch]$Upload, [string]$Dist = '', [string]$FailKey = '', [string]$NoisyFailKey = '', [string]$NoisyOkKey = '',
            [string]$FetchError = '', [hashtable]$FetchStatus = @{}, [hashtable]$WrongLength = @{}, [bool]$Npx = $true)
        $state = @{ Puts = New-Object Collections.Generic.List[string]; Fetches = New-Object Collections.Generic.List[string]; Bucket = (Join-Path $Root 'bucket')
            FailKey = $FailKey; NoisyFailKey = $NoisyFailKey; NoisyOkKey = $NoisyOkKey; FetchError = $FetchError; FetchStatus = $FetchStatus; WrongLength = $WrongLength }
        $null = New-Item -ItemType Directory -Force $state.Bucket
        $put = { param($Item, $Dist)
            if ($Item.Key -eq $state.FailKey) { return $false }
            if ($Item.Key -eq $state.NoisyFailKey) { 'wrangler 4.31.0'; 'Uploading object...'; 'X [ERROR] Authentication error [code: 10000]'; return $false }
            if ($Item.Key -eq $state.NoisyOkKey) { 'Upload complete.'; return $true }
            $state.Puts.Add($Item.Key)
            $dest = Join-Path $state.Bucket ($Item.Key -replace '/', '\')
            $null = New-Item -ItemType Directory -Force (Split-Path -Parent $dest)
            Copy-Item -LiteralPath (Join-Path $Dist $Item.File) -Destination $dest -Force
            return $true }.GetNewClosure()
        $head = { param($Url)
            $key = $Url.Substring('https://pkg.cellglide.com/'.Length)
            $path = Join-Path $state.Bucket ($key -replace '/', '\')
            if (-not (Test-Path -LiteralPath $path)) { return [pscustomobject]@{ Status = 404; Length = 0 } }
            $len = (Get-Item -LiteralPath $path).Length
            if ($state.WrongLength.ContainsKey($key)) { $len += 1 }
            return [pscustomobject]@{ Status = 200; Length = $len } }.GetNewClosure()
        $fetch = { param($Url, $OutFile)
            $key = $Url.Substring('https://pkg.cellglide.com/'.Length)
            $state.Fetches.Add($key)
            if ($key -eq $state.FetchError) { throw 'The remote name could not be resolved' }
            if ($state.FetchStatus.ContainsKey($key)) { return $state.FetchStatus[$key] }
            $path = Join-Path $state.Bucket ($key -replace '/', '\')
            if (-not (Test-Path -LiteralPath $path)) { return 404 }
            Copy-Item -LiteralPath $path -Destination $OutFile
            return 200 }.GetNewClosure()
        $npxCheck = if ($Npx) { { $true } } else { { $false } }
        $out = @(Invoke-PublishWindows -Dist $Dist -Upload:$Upload -Root $Root -HasNpx $npxCheck -Put $put -Fetch $fetch -Head $head 6>&1)
        return [pscustomobject]@{
            Code = $out[-1]
            Text = (@($out | Select-Object -SkipLast 1 | ForEach-Object { "$_" }) -join "`n")
            Puts = @($state.Puts)
            Fetches = @($state.Fetches)
            Bucket = $state.Bucket
        }
    }

    # Puts a file into the fake bucket under a key, as if published earlier.
    function Set-BucketObject([string]$Root, [string]$Key, [byte[]]$Bytes) {
        $dest = Join-Path (Join-Path $Root 'bucket') ($Key -replace '/', '\')
        $null = New-Item -ItemType Directory -Force (Split-Path -Parent $dest)
        [IO.File]::WriteAllBytes($dest, $Bytes)
    }
}

Describe 'publish-windows' {
    It 'uploads versioned files, then the pointer, then the scripts' {
        $plan = Get-UploadPlan '1.0.0'
        ($plan | ForEach-Object Key) | Should -Be @('windows/1.0.0/CellGlide-1.0.0-windows-x64.zip', 'windows/1.0.0/CellGlide-1.0.0-windows-arm64.zip',
            'windows/1.0.0/SHA256SUMS', 'windows/latest.json', 'uninstall.ps1', 'install.ps1')
        ($plan | Where-Object Key -eq 'install.ps1').CacheControl | Should -Be 'no-cache'
        ($plan | Where-Object Key -eq 'install.ps1').ContentType | Should -Be 'text/plain; charset=utf-8'
        $plan[0].CacheControl | Should -Be 'public, max-age=31536000, immutable'
    }
    It 'refuses to change published bytes' {
        $rec = "{0}  windows/1.0.0/CellGlide-1.0.0-windows-x64.zip`n" -f ('a' * 64)
        Test-PublishedRecord $rec '1.0.0' @{ 'CellGlide-1.0.0-windows-x64.zip' = ('b' * 64) } | Should -Not -BeNullOrEmpty
        Test-PublishedRecord $rec '1.0.0' @{ 'CellGlide-1.0.0-windows-x64.zip' = ('a' * 64) } | Should -BeNullOrEmpty
        Test-PublishedRecord '' '1.0.1' @{ 'CellGlide-1.0.1-windows-x64.zip' = ('b' * 64) } | Should -BeNullOrEmpty
    }
}

Describe 'publish-windows plan and record' {
    It 'gives every file its type and cache rule' {
        $plan = Get-UploadPlan '1.2.3'
        ($plan | ForEach-Object File) | Should -Be @('CellGlide-1.2.3-windows-x64.zip', 'CellGlide-1.2.3-windows-arm64.zip', 'SHA256SUMS', 'latest.json', 'uninstall.ps1', 'install.ps1')
        ($plan | ForEach-Object ContentType) | Should -Be @('application/zip', 'application/zip', 'text/plain; charset=utf-8', 'application/json', 'text/plain; charset=utf-8', 'text/plain; charset=utf-8')
        ($plan | ForEach-Object CacheControl) | Should -Be @('public, max-age=31536000, immutable', 'public, max-age=31536000, immutable', 'public, max-age=31536000, immutable', 'no-cache', 'no-cache', 'no-cache')
    }
    It 'builds the wrangler command of the Linux publisher' {
        $item = (Get-UploadPlan '1.0.0')[2]
        Get-WranglerArgs $item 'C:\d' | Should -Be @('--yes', 'wrangler@4.31.0', 'r2', 'object', 'put', 'cellglide-pkg/windows/1.0.0/SHA256SUMS',
            '--file', 'C:\d\SHA256SUMS', '--content-type', 'text/plain; charset=utf-8', '--cache-control', 'public, max-age=31536000, immutable', '--remote')
    }
    It 'ignores comments and other versions, and reads the whole record of this version' {
        $a = 'a' * 64; $b = 'b' * 64; $c = 'c' * 64
        $rec = "# header`n$c  windows/0.9.0/CellGlide-0.9.0-windows-x64.zip`n$a  windows/1.0.0/CellGlide-1.0.0-windows-x64.zip`n$b  windows/1.0.0/SHA256SUMS`n"
        Test-PublishedRecord $rec '1.0.0' @{ 'CellGlide-1.0.0-windows-x64.zip' = $a.ToUpper(); 'SHA256SUMS' = $b } | Should -BeNullOrEmpty
        Test-PublishedRecord $rec '1.0.0' @{ 'CellGlide-1.0.0-windows-x64.zip' = $a } | Should -Match 'not in this release'
        Test-PublishedRecord $rec '1.0.0' @{ 'CellGlide-1.0.0-windows-x64.zip' = $a; 'SHA256SUMS' = $b; 'CellGlide-1.0.0-windows-arm64.zip' = $c } | Should -Match 'cannot gain files'
        Test-PublishedRecord "not a line`n" '1.0.0' @{ 'SHA256SUMS' = $b } | Should -Match 'bad line'
        Test-PublishedRecord "$a  windows/1.0.0/SHA256SUMS`n$a  windows/1.0.0/SHA256SUMS`n" '1.0.0' @{ 'SHA256SUMS' = $a } | Should -Match 'twice'
    }
    It 'decides a wrangler put by its exit code alone and returns one bool' {
        function npx { 'wrangler 4.31.0'; 'X [ERROR] Authentication error'; cmd /c exit 7 }
        $item = (Get-UploadPlan '1.0.0')[0]
        $out = @(Invoke-WranglerPut $item 'C:\d' 6>&1)
        $result = @($out | Where-Object { $_ -is [bool] })
        $result.Count | Should -Be 1
        $result[0] | Should -BeFalse
        @($out | Where-Object { $_ -isnot [bool] }) -join "`n" | Should -Match 'wrangler: X \[ERROR\] Authentication error'
        function npx { 'Upload complete.'; cmd /c exit 0 }
        $out = @(Invoke-WranglerPut $item 'C:\d' 6>&1)
        $out.Count | Should -Be 1
        $out[0] | Should -BeTrue
        $out[0] | Should -BeOfType [bool]
    }
    It 'an npx that never ran is a failure, even after a put that succeeded' {
        # Like npx missing: nothing native runs, so nothing sets $LASTEXITCODE.
        function npx { Write-Error 'npx not found' }
        cmd /c exit 0
        $item = (Get-UploadPlan '1.0.0')[0]
        $out = @(Invoke-WranglerPut $item 'C:\d' 6>&1)
        @($out | Where-Object { $_ -is [bool] }) | Should -Be @($false)
    }
    It 'accepts only a single $true as a successful put' {
        Test-PutResult $true | Should -BeTrue
        Test-PutResult $false | Should -BeFalse
        Test-PutResult @('wrangler banner', $false) | Should -BeFalse
        Test-PutResult @('wrangler banner', $true) | Should -BeFalse
        Test-PutResult $null | Should -BeFalse
        Test-PutResult 'True' | Should -BeFalse
        Test-PutResult 1 | Should -BeFalse
    }
    It 'tells immutable keys from the pointer and the scripts' {
        @((Get-UploadPlan '1.0.0') | Where-Object { Test-ImmutableKey $_.Key '1.0.0' } | ForEach-Object Key) |
            Should -Be @('windows/1.0.0/CellGlide-1.0.0-windows-x64.zip', 'windows/1.0.0/CellGlide-1.0.0-windows-arm64.zip', 'windows/1.0.0/SHA256SUMS')
    }
    It 'is ASCII only, with LF line endings' {
        $bytes = [IO.File]::ReadAllBytes((Resolve-Path $script:ScriptFile))
        $bytes | Where-Object { $_ -gt 0x7F } | Should -BeNullOrEmpty
        $bytes | Where-Object { $_ -eq 0x0D } | Should -BeNullOrEmpty
    }
    It 'never uploads without -Upload, and has no other way to reach the bucket' {
        $text = [IO.File]::ReadAllText((Resolve-Path $script:ScriptFile))
        ([regex]::Matches($text, '& npx ')).Count | Should -Be 1
        $text | Should -Match "if \(-not \`$Upload\) \{"
    }
}

Describe 'publish-windows run against a fake bucket' {
    BeforeEach {
        $script:Root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        $script:DistDir = New-FakeRepo $script:Root
        $script:Record = Join-Path $script:Root 'packaging\windows\published.sha256'
    }

    It 'dry run: checks, prints the plan, copies the scripts and uploads nothing' {
        $r = Invoke-Fake $script:Root
        $r.Code | Should -Be 0
        $r.Puts | Should -BeNullOrEmpty
        $r.Text | Should -Match 'dry run: nothing uploaded'
        $keys = @([regex]::Matches($r.Text, '(?m)^  (\S+)  \[') | ForEach-Object { $_.Groups[1].Value })
        $keys | Should -Be @((Get-UploadPlan '1.0.0') | ForEach-Object Key)
        $r.Text | Should -Match ([regex]::Escape('windows/latest.json  [application/json; no-cache]'))
        foreach ($s in 'install.ps1', 'uninstall.ps1') {
            Get-Sha (Join-Path $script:DistDir $s) | Should -Be (Get-Sha (Join-Path $script:Root "packaging\windows\$s"))
        }
        [IO.File]::ReadAllText($script:Record) | Should -Be "# header`n"
        $r.Text | Should -Match ('  ' + (Get-Sha (Join-Path $script:DistDir 'SHA256SUMS')) + '  windows/1.0.0/SHA256SUMS')
    }
    It 'defaults to dist\windows\(workspace version)' {
        $r = Invoke-Fake $script:Root
        $r.Text | Should -Match ([regex]::Escape("from $script:DistDir"))
    }
    It 'stops when a release file is missing' {
        Remove-Item (Join-Path $script:DistDir 'release-windows.json')
        $r = Invoke-Fake $script:Root -Upload
        $r.Code | Should -Be 1
        $r.Text | Should -Match 'missing from .*release-windows\.json'
        $r.Puts | Should -BeNullOrEmpty
    }
    It 'stops when a zip does not match SHA256SUMS' {
        [IO.File]::WriteAllBytes((Join-Path $script:DistDir 'CellGlide-1.0.0-windows-arm64.zip'), [Text.Encoding]::ASCII.GetBytes('zip bytes ARM64'))
        $r = Invoke-Fake $script:Root -Upload
        $r.Code | Should -Be 1
        $r.Text | Should -Match 'SHA256SUMS: CellGlide-1.0.0-windows-arm64.zip is'
        $r.Puts | Should -BeNullOrEmpty
    }
    It 'stops when SHA256SUMS lists anything else' {
        Add-Content -LiteralPath (Join-Path $script:DistDir 'SHA256SUMS') -Value ('{0}  extra.zip' -f ('1' * 64)) -NoNewline
        $r = Invoke-Fake $script:Root
        $r.Code | Should -Be 1
        $r.Text | Should -Match 'must list exactly'
    }
    It 'stops when latest.json disagrees with SHA256SUMS' {
        $file = Join-Path $script:DistDir 'latest.json'
        $sha = Get-Sha (Join-Path $script:DistDir 'CellGlide-1.0.0-windows-x64.zip')
        Write-Lf $file ([IO.File]::ReadAllText($file).Replace($sha, ('0' * 64)))
        $r = Invoke-Fake $script:Root
        $r.Code | Should -Be 1
        $r.Text | Should -Match 'latest.json: the x64 sha256'
    }
    It 'stops when release-windows.json disagrees with latest.json' {
        $file = Join-Path $script:DistDir 'release-windows.json'
        Write-Lf $file ([IO.File]::ReadAllText($file).Replace('"test_build":  true', '"test_build":  false'))
        $r = Invoke-Fake $script:Root
        $r.Code | Should -Be 1
        $r.Text | Should -Match 'release-windows.json: the arm64 zip entry does not match'
    }
    It 'stops when a script is not ASCII' {
        [IO.File]::WriteAllBytes((Join-Path $script:Root 'packaging\windows\install.ps1'), [byte[]](0x57, 0xC3, 0xA9, 0x0A))
        $r = Invoke-Fake $script:Root -Upload
        $r.Code | Should -Be 1
        $r.Text | Should -Match 'non-ASCII'
        $r.Puts | Should -BeNullOrEmpty
    }
    It 'refuses, even in a dry run, to change bytes that were published' {
        $sha = Get-Sha (Join-Path $script:DistDir 'CellGlide-1.0.0-windows-x64.zip')
        Add-Content -LiteralPath $script:Record -Value ('{0}  windows/1.0.0/CellGlide-1.0.0-windows-x64.zip' -f ('f' * 64))
        $r = Invoke-Fake $script:Root
        $r.Code | Should -Be 1
        $r.Text | Should -Match 'differs from the bytes published earlier'
        $r.Text | Should -Not -Match 'dry run: nothing uploaded'
        $r = Invoke-Fake $script:Root -Upload
        $r.Code | Should -Be 1
        $r.Puts | Should -BeNullOrEmpty
    }
    It 'reports rehearsal data in a dry run and refuses to upload it' {
        $root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        $null = New-FakeRepo $root 'http://127.0.0.1:8770'
        $r = Invoke-Fake $root
        $r.Code | Should -Be 0
        $r.Text | Should -Match 'local rehearsal'
        $r = Invoke-Fake $root -Upload
        $r.Code | Should -Be 1
        $r.Text | Should -Match 'refusing to upload release data made for http://127.0.0.1:8770'
        $r.Puts | Should -BeNullOrEmpty
    }
    It 'refuses a base that is neither production nor loopback' {
        $root = Join-Path $TestDrive ([guid]::NewGuid().ToString('N'))
        $null = New-FakeRepo $root 'https://example.com'
        $r = Invoke-Fake $root
        $r.Code | Should -Be 1
        $r.Text | Should -Match 'unexpected base https://example.com'
    }
    It 'needs npx to upload' {
        $r = Invoke-Fake $script:Root -Upload -Npx $false
        $r.Code | Should -Be 1
        $r.Text | Should -Match 'npx is not available'
        $r.Puts | Should -BeNullOrEmpty
    }
    It 'uploads in plan order, checks every URL, then records the version once' {
        $r = Invoke-Fake $script:Root -Upload
        $r.Code | Should -Be 0
        $r.Puts | Should -Be @((Get-UploadPlan '1.0.0') | ForEach-Object Key)
        foreach ($item in (Get-UploadPlan '1.0.0')) {
            Get-Sha (Join-Path $r.Bucket ($item.Key -replace '/', '\')) | Should -Be (Get-Sha (Join-Path $script:DistDir $item.File))
            $r.Text | Should -Match ([regex]::Escape("ok: https://pkg.cellglide.com/$($item.Key)"))
        }
        $expected = "# header`n" + ((Get-VersionedFiles '1.0.0' | ForEach-Object { "$(Get-Sha (Join-Path $script:DistDir $_))  windows/1.0.0/$_" }) -join "`n") + "`n"
        [IO.File]::ReadAllText($script:Record) | Should -Be $expected
        $r.Text | Should -Match 'commit packaging/windows/published.sha256'
        $r.Text | Should -Match ([regex]::Escape((Join-Path $script:DistDir 'release-windows.json')))
        # The same bytes again: allowed, the immutable objects are not put again, and the record
        # is not appended twice.
        $again = Invoke-Fake $script:Root -Upload
        $again.Code | Should -Be 0
        $again.Text | Should -Match 'already in published.sha256'
        $again.Puts | Should -Be @('windows/latest.json', 'uninstall.ps1', 'install.ps1')
        [IO.File]::ReadAllText($script:Record) | Should -Be $expected
    }
    It 'reads every immutable key back before the first put' {
        $r = Invoke-Fake $script:Root -Upload
        $r.Code | Should -Be 0
        $r.Fetches | Should -Be @('windows/1.0.0/CellGlide-1.0.0-windows-x64.zip', 'windows/1.0.0/CellGlide-1.0.0-windows-arm64.zip', 'windows/1.0.0/SHA256SUMS')
        $first = $r.Text.IndexOf('put cellglide-pkg/')
        $first | Should -BeGreaterThan $r.Text.LastIndexOf('bucket: windows/1.0.0/SHA256SUMS')
    }
    It 'stops when a failed put prints output, and puts nothing after it' {
        $r = Invoke-Fake $script:Root -Upload -NoisyFailKey 'windows/1.0.0/CellGlide-1.0.0-windows-arm64.zip'
        $r.Code | Should -Be 1
        $r.Puts | Should -Be @('windows/1.0.0/CellGlide-1.0.0-windows-x64.zip')
        $r.Puts | Should -Not -Contain 'windows/latest.json'
        $r.Puts | Should -Not -Contain 'install.ps1'
        $r.Text | Should -Match 'uploading windows/1.0.0/CellGlide-1.0.0-windows-arm64.zip failed'
        Test-Path (Join-Path $r.Bucket 'windows\latest.json') | Should -BeFalse
        Test-Path (Join-Path $r.Bucket 'install.ps1') | Should -BeFalse
        [IO.File]::ReadAllText($script:Record) | Should -Be "# header`n"
    }
    It 'counts a put with stray output as failed, even when it ends with $true' {
        $r = Invoke-Fake $script:Root -Upload -NoisyOkKey 'windows/latest.json'
        $r.Code | Should -Be 1
        $r.Puts | Should -Not -Contain 'install.ps1'
        $r.Puts | Should -Not -Contain 'uninstall.ps1'
        [IO.File]::ReadAllText($script:Record) | Should -Be "# header`n"
    }
    It 'refuses when the bucket holds different bytes for a versioned key, and puts nothing' {
        Set-BucketObject $script:Root 'windows/1.0.0/SHA256SUMS' ([Text.Encoding]::ASCII.GetBytes("old sums`n"))
        $r = Invoke-Fake $script:Root -Upload
        $r.Code | Should -Be 1
        $r.Text | Should -Match 'windows/1.0.0/SHA256SUMS is already published with different bytes'
        $r.Puts | Should -BeNullOrEmpty
        [IO.File]::ReadAllText((Join-Path $r.Bucket 'windows\1.0.0\SHA256SUMS')) | Should -Be "old sums`n"
        [IO.File]::ReadAllText($script:Record) | Should -Be "# header`n"
    }
    It 'refuses even when the local record has no lines for the version (an unrecorded partial upload)' {
        Set-BucketObject $script:Root 'windows/1.0.0/CellGlide-1.0.0-windows-x64.zip' ([Text.Encoding]::ASCII.GetBytes('an earlier build'))
        $r = Invoke-Fake $script:Root
        $r.Code | Should -Be 0   # a dry run never reads the bucket
        $r.Fetches | Should -BeNullOrEmpty
        $r = Invoke-Fake $script:Root -Upload
        $r.Code | Should -Be 1
        $r.Puts | Should -BeNullOrEmpty
    }
    It 'skips versioned keys already there with the same bytes, uploads the rest, and records the version' {
        Set-BucketObject $script:Root 'windows/1.0.0/CellGlide-1.0.0-windows-x64.zip' ([IO.File]::ReadAllBytes((Join-Path $script:DistDir 'CellGlide-1.0.0-windows-x64.zip')))
        $r = Invoke-Fake $script:Root -Upload
        $r.Code | Should -Be 0
        $r.Text | Should -Match 'CellGlide-1.0.0-windows-x64.zip is already there with these bytes'
        $r.Puts | Should -Be @('windows/1.0.0/CellGlide-1.0.0-windows-arm64.zip', 'windows/1.0.0/SHA256SUMS', 'windows/latest.json', 'uninstall.ps1', 'install.ps1')
        ([IO.File]::ReadAllText($script:Record) -split "`n" | Where-Object { $_ -match 'windows/1.0.0/' }).Count | Should -Be 3
    }
    It 'fails closed when it cannot tell whether a versioned key exists' {
        $r = Invoke-Fake $script:Root -Upload -FetchError 'windows/1.0.0/CellGlide-1.0.0-windows-arm64.zip'
        $r.Code | Should -Be 1
        $r.Text | Should -Match 'cannot tell whether windows/1.0.0/CellGlide-1.0.0-windows-arm64.zip is already published'
        $r.Puts | Should -BeNullOrEmpty
        foreach ($status in 403, 500, 301) {
            $r = Invoke-Fake $script:Root -Upload -FetchStatus @{ 'windows/1.0.0/SHA256SUMS' = $status }
            $r.Code | Should -Be 1
            $r.Puts | Should -BeNullOrEmpty
        }
    }
    It 'refuses a version that is not X.Y.Z and a -Dist path cmd.exe would mangle' {
        Write-Lf (Join-Path $script:Root 'Cargo.toml') "[workspace.package]`nversion = `"1.0.0/../x`"`n"
        $r = Invoke-Fake $script:Root
        $r.Code | Should -Be 1
        $r.Text | Should -Match 'is not X.Y.Z'
        Write-Lf (Join-Path $script:Root 'Cargo.toml') "[workspace.package]`nversion = `"1.0.0`"`n"
        foreach ($bad in '100%', 'a^b', 'x!y', 'a&b') {
            $dir = Join-Path $TestDrive $bad
            $null = New-Item -ItemType Directory -Force $dir
            Copy-Item (Join-Path $script:DistDir '*') $dir
            $r = Invoke-Fake $script:Root -Upload -Dist $dir
            $r.Code | Should -Be 1
            $r.Text | Should -Match 'cmd.exe'
            $r.Puts | Should -BeNullOrEmpty
        }
    }
    It 'stops at the first failed upload and records nothing' {
        $r = Invoke-Fake $script:Root -Upload -FailKey 'windows/1.0.0/SHA256SUMS'
        $r.Code | Should -Be 1
        $r.Puts | Should -Be @('windows/1.0.0/CellGlide-1.0.0-windows-x64.zip', 'windows/1.0.0/CellGlide-1.0.0-windows-arm64.zip')
        $r.Text | Should -Match 'uploading windows/1.0.0/SHA256SUMS failed'
        [IO.File]::ReadAllText($script:Record) | Should -Be "# header`n"
    }
    It 'records nothing when a public URL has the wrong size' {
        $r = Invoke-Fake $script:Root -Upload -WrongLength @{ 'install.ps1' = $true }
        $r.Code | Should -Be 1
        $r.Text | Should -Match 'HEAD https://pkg.cellglide.com/install.ps1 answered 200'
        [IO.File]::ReadAllText($script:Record) | Should -Be "# header`n"
    }
}
