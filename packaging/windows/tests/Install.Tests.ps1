# Pester 5 tests for packaging\windows\install.ps1 (run by scripts\test-windows-scripts.ps1).
# Only the script's functions and constants are loaded (Import-ScriptFunctions); the steps that
# change the system are mocked.
BeforeAll {
    . "$PSScriptRoot\..\..\..\scripts\tests\Import-ScriptFunctions.ps1"
    $script:File = "$PSScriptRoot\..\install.ps1"; . Import-ScriptFunctions $script:File     # functions and constants ($DefaultBase ...)
    $script:Ast = [Management.Automation.Language.Parser]::ParseFile((Resolve-Path $script:File), [ref]$null, [ref]$null)
}
Describe 'install.ps1' {
    It 'Base URL accepts only production and loopback' {
        Resolve-BaseUrl '' | Should -Be 'https://pkg.cellglide.com'
        Resolve-BaseUrl 'https://pkg.cellglide.com/' | Should -Be 'https://pkg.cellglide.com'
        Resolve-BaseUrl 'http://127.0.0.1:8770/' -AllowLoopback | Should -Be 'http://127.0.0.1:8770'
        Resolve-BaseUrl 'http://localhost:9' -AllowLoopback | Should -Be 'http://localhost:9'
        foreach ($bad in 'https://evil.example', 'http://pkg.cellglide.com', 'http://10.0.0.1:80', 'https://127.0.0.1:1', 'file:///C:/x',
                "http://127.0.0.1:80`n", 'http://127.0.0.1:1@evil.example:2', 'http://127.0.0.1:0', 'http://127.0.0.1:65536',
                'http://127.0.0.1:80/x', 'http://127.0.0.1:80?x', 'http://LOCALHOST:80', 'http://127.0.0.1', 'https://PKG.cellglide.com') {
            { Resolve-BaseUrl $bad -AllowLoopback } | Should -Throw '*Unsupported download location*' -Because $bad
        }
    }
    It 'a loopback source needs -AllowLoopback' {
        { Resolve-BaseUrl 'http://127.0.0.1:8770' } | Should -Throw '*Unsupported download location*needs -AllowLoopback*'
        { Resolve-BaseUrl 'http://localhost:8770' } | Should -Throw '*needs -AllowLoopback*'
    }
    It 'maps machines to packages' {
        ConvertTo-CellGlideArch 0x8664 | Should -Be 'x64'; ConvertTo-CellGlideArch 0xAA64 | Should -Be 'arm64'
        ConvertTo-CellGlideArch 0x014C | Should -BeNullOrEmpty
    }
    It 'checks the Windows build per architecture' {
        (Test-WindowsSupported 19045 'x64').Reason | Should -Be 'CellGlide needs Windows 11.'
        (Test-WindowsSupported 22000 'x64').Ok | Should -BeFalse
        (Test-WindowsSupported 22621 'x64').Ok | Should -BeTrue
        (Test-WindowsSupported 22631 'arm64').Reason | Should -Be 'CellGlide on ARM64 needs Windows 11 24H2 or newer.'
        (Test-WindowsSupported 26100 'arm64').Ok | Should -BeTrue
    }
    It 'stops only for enforced Smart App Control' {
        Test-SmartAppControlBlocks 1 | Should -BeTrue
        Test-SmartAppControlBlocks 0 | Should -BeFalse; Test-SmartAppControlBlocks 2 | Should -BeFalse
    }
    It 'Latest must name this version''s own file' {
        $ok = '{"version":"1.0.0","x64":{"url":"https://pkg.cellglide.com/windows/1.0.0/CellGlide-1.0.0-windows-x64.zip","sha256":"' + ('A' * 64) + '","size":1,"test_build":false},"arm64":{"url":"https://pkg.cellglide.com/windows/1.0.0/CellGlide-1.0.0-windows-arm64.zip","sha256":"' + ('b' * 64) + '","size":2,"test_build":true}}'
        $l = Read-Latest $ok 'x64' 'https://pkg.cellglide.com'
        $l.Sha256 | Should -Be ('a' * 64); $l.FileName | Should -Be 'CellGlide-1.0.0-windows-x64.zip'
        (Read-Latest $ok 'arm64' 'https://pkg.cellglide.com').TestBuild | Should -BeTrue
        { Read-Latest ($ok -replace 'windows/1.0.0/CellGlide-1.0.0-windows-x64', 'windows/0.9/CellGlide-1.0.0-windows-x64') 'x64' 'https://pkg.cellglide.com' } | Should -Throw
        { Read-Latest '{"version":"1.0.0"}' 'x64' 'https://pkg.cellglide.com' } | Should -Throw
    }
    It 'Latest refuses wrong types and another host' {
        $base = 'http://127.0.0.1:8770'
        $make = { param($sha, $size, $tb, $host_) '{"version":"1.0.0","x64":{"url":"' + $host_ + '/windows/1.0.0/CellGlide-1.0.0-windows-x64.zip","sha256":' + $sha + ',"size":' + $size + ',"test_build":' + $tb + '}}' }
        (Read-Latest (& $make ('"' + ('c' * 64) + '"') 5 'false' $base) 'x64' $base).Size | Should -Be 5
        { Read-Latest (& $make ('"' + ('c' * 63) + '"') 5 'false' $base) 'x64' $base } | Should -Throw '*sha256*'
        { Read-Latest (& $make ('"' + ('c' * 64) + '"') '"5"' 'false' $base) 'x64' $base } | Should -Throw '*size*'
        { Read-Latest (& $make ('"' + ('c' * 64) + '"') 0 'false' $base) 'x64' $base } | Should -Throw '*size*'
        { Read-Latest (& $make ('"' + ('c' * 64) + '"') 5 '"no"' $base) 'x64' $base } | Should -Throw '*test_build*'
        { Read-Latest (& $make ('"' + ('c' * 64) + '"') 5 'false' 'https://pkg.cellglide.com') 'x64' $base } | Should -Throw '*url*'
        { Read-Latest '{"version":"../1","x64":{}}' 'x64' $base } | Should -Throw '*version*'
    }
    It 'reads SHA256SUMS in both forms' {
        $s = Read-Sha256Sums ("{0}  a.zip`n{1} *b.zip`n`n" -f ('A' * 64), ('c' * 64))
        $s['a.zip'] | Should -Be ('a' * 64); $s['b.zip'] | Should -Be ('c' * 64)
        { Read-Sha256Sums 'not a line' } | Should -Throw
    }
    It 'SHA256SUMS refuses a duplicate, a malformed line and a missing entry' {
        $x = "{0}  CellGlide-1.0.0-windows-arm64.zip`n{1}  CellGlide-1.0.0-windows-x64.zip`n" -f ('a' * 64), ('b' * 64)
        (Read-Sha256Sums $x 'CellGlide-1.0.0-windows-x64.zip')['CellGlide-1.0.0-windows-x64.zip'] | Should -Be ('b' * 64)
        { Read-Sha256Sums ($x + ('c' * 64) + "  CellGlide-1.0.0-windows-x64.zip`n") } | Should -Throw '*duplicate entry for CellGlide-1.0.0-windows-x64.zip*'
        { Read-Sha256Sums ($x + ('c' * 63) + "  short.zip`n") } | Should -Throw '*bad line*'
        { Read-Sha256Sums (('c' * 64) + "   ") } | Should -Throw '*bad line*'
        { Read-Sha256Sums $x 'CellGlide-1.0.1-windows-x64.zip' } | Should -Throw '*no entry for CellGlide-1.0.1-windows-x64.zip*'
        { Read-Sha256Sums '' 'a.zip' } | Should -Throw '*no entry*'
    }
    It 'compares a download with its hash' {
        $f = Join-Path $TestDrive 'x.zip'; Set-Content $f 'x' -NoNewline
        Test-Download $f ('0' * 64) | Should -BeFalse
        Test-Download $f (Get-FileHash $f -Algorithm SHA256).Hash.ToLower() | Should -BeTrue
        Test-Download (Join-Path $TestDrive 'missing.zip') ('0' * 64) | Should -BeFalse
    }
    It 'a tampered zip is refused by Get-CellGlidePackage' {
        $src = Join-Path $TestDrive 'src'; $null = New-Item -ItemType Directory $src
        $zip = Join-Path $src 'CellGlide-1.0.0-windows-x64.zip'; Set-Content $zip 'zip' -NoNewline
        $good = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
        Set-Content (Join-Path $src 'latest.json') ('{"version":"1.0.0","x64":{"url":"http://127.0.0.1:1/windows/1.0.0/CellGlide-1.0.0-windows-x64.zip","sha256":"' + $good + '","size":3,"test_build":false},"arm64":{"url":"http://127.0.0.1:1/windows/1.0.0/CellGlide-1.0.0-windows-arm64.zip","sha256":"' + ('0' * 64) + '","size":1,"test_build":true}}')
        Set-Content (Join-Path $src 'SHA256SUMS') ("$good  CellGlide-1.0.0-windows-x64.zip")
        $fake = { param($Url, $OutFile) Copy-Item (Join-Path $src (Split-Path $Url -Leaf)) $OutFile }.GetNewClosure()
        (Get-CellGlidePackage 'http://127.0.0.1:1' 'x64' (Join-Path $TestDrive 'w1') $fake).Version | Should -Be '1.0.0'
        Set-Content $zip 'tampered' -NoNewline
        { Get-CellGlidePackage 'http://127.0.0.1:1' 'x64' (Join-Path $TestDrive 'w2') $fake } | Should -Throw '*SHA-256 mismatch*'
    }
    It 'SHA256SUMS that disagrees with latest.json is refused' {
        $src = Join-Path $TestDrive 'src2'; $null = New-Item -ItemType Directory $src
        $zip = Join-Path $src 'CellGlide-1.0.0-windows-x64.zip'; Set-Content $zip 'zip' -NoNewline
        $good = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
        Set-Content (Join-Path $src 'latest.json') ('{"version":"1.0.0","x64":{"url":"http://127.0.0.1:1/windows/1.0.0/CellGlide-1.0.0-windows-x64.zip","sha256":"' + $good + '","size":3,"test_build":false}}')
        $fake = { param($Url, $OutFile) Copy-Item (Join-Path $src (Split-Path $Url -Leaf)) $OutFile }.GetNewClosure()
        Set-Content (Join-Path $src 'SHA256SUMS') (('0' * 64) + '  CellGlide-1.0.0-windows-x64.zip')
        { Get-CellGlidePackage 'http://127.0.0.1:1' 'x64' (Join-Path $TestDrive 'w3') $fake } | Should -Throw '*SHA-256 mismatch*'
        Set-Content (Join-Path $src 'SHA256SUMS') ("$good  CellGlide-1.0.0-windows-arm64.zip")
        { Get-CellGlidePackage 'http://127.0.0.1:1' 'x64' (Join-Path $TestDrive 'w4') $fake } | Should -Throw '*no entry for CellGlide-1.0.0-windows-x64.zip*'
        Set-Content (Join-Path $src 'SHA256SUMS') ("$good  CellGlide-1.0.0-windows-x64.zip`n$good  CellGlide-1.0.0-windows-x64.zip")
        { Get-CellGlidePackage 'http://127.0.0.1:1' 'x64' (Join-Path $TestDrive 'w5') $fake } | Should -Throw '*duplicate entry*'
    }
    It 'downloads go into a new folder only, and TLS settings are put back' {
        $before = [Net.ServicePointManager]::SecurityProtocol
        $existing = Join-Path $TestDrive 'exists'; $null = New-Item -ItemType Directory $existing
        { Get-CellGlidePackage 'http://127.0.0.1:1' 'x64' $existing { throw 'not reached' } } | Should -Throw
        [Net.ServicePointManager]::SecurityProtocol | Should -Be $before
        { Get-CellGlidePackage 'http://127.0.0.1:1' 'x64' (Join-Path $TestDrive 'w6') { throw 'offline' } } | Should -Throw '*offline*'
        [Net.ServicePointManager]::SecurityProtocol | Should -Be $before
    }
    It 'A hash mismatch stops before anything changes' {
        Mock New-PrivateWorkDir { $d = Join-Path $TestDrive ('work-' + [guid]::NewGuid().ToString('N')); $null = New-Item -ItemType Directory $d; $d }
        Mock Get-CellGlidePackage { throw 'SHA-256 mismatch for CellGlide-1.0.0-windows-x64.zip' }
        Mock Stop-CellGlideProcesses {}; Mock Stop-OurService { $true }; Mock Install-Files {}
        Mock Set-OurService {}; Mock Set-DataDirAcl {}; Mock Set-UninstallEntry {}; Mock New-StartMenuShortcut {}
        Mock Start-Service {}; Mock Write-Host {}
        Invoke-ElevatedPart 'https://pkg.cellglide.com' '' 'x64' | Should -Be 1
        foreach ($changer in 'Stop-CellGlideProcesses', 'Stop-OurService', 'Install-Files', 'Set-OurService', 'Set-DataDirAcl', 'Set-UninstallEntry', 'New-StartMenuShortcut', 'Start-Service') {
            Should -Invoke $changer -Times 0 -Exactly -Because $changer
        }
        Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'Installation failed:*SHA-256 mismatch*' }
    }
    Context 'the elevated part with every system change mocked' {
        BeforeEach {
            $script:calls = New-Object System.Collections.Generic.List[string]
            $script:work = Join-Path $TestDrive ('work-' + [guid]::NewGuid().ToString('N'))
            Mock New-PrivateWorkDir { $null = New-Item -ItemType Directory $script:work; $script:work }
            Mock Remove-StaleWorkDirs {}
            Mock Get-CellGlidePackage { [pscustomobject]@{ ZipPath = 'pkg.zip'; Version = '1.0.0'; TestBuild = $false } }
            Mock Expand-Package { $null = New-Item -ItemType Directory $Dest; Set-Content (Join-Path $Dest 'version.txt') $script:packageVersion }
            Mock Get-PeMachine { 0x8664 }
            Mock Get-InstalledVersion { $script:installedVersion }
            Mock Test-Elevated { $true }
            Mock Stop-CellGlideProcesses { $script:calls.Add('processes') }
            Mock Stop-OurService { $script:calls.Add('stop'); $script:stopResult }
            Mock Install-Files { $script:calls.Add('files') }
            Mock Set-OurService { $script:calls.Add('service') }
            Mock Set-DataDirAcl { $script:calls.Add('acl') }
            Mock New-StartMenuShortcut { $script:calls.Add('shortcut') }
            Mock Set-UninstallEntry { $script:calls.Add("entry $Version") }
            Mock Start-Service { $script:calls.Add('start') }
            # The service as it was when the run began; $null = no service yet.
            Mock Get-Service {
                if ($null -eq $script:serviceStatus) { return $null }
                [pscustomobject]@{ Status = $script:serviceStatus } | Add-Member -MemberType ScriptMethod -Name WaitForStatus -Value { } -PassThru }
            Mock Write-Host {}
            $script:packageVersion = '1.0.0'; $script:installedVersion = ''; $script:stopResult = $true; $script:serviceStatus = 'Running'
        }
        It 'runs the steps in order after the package is verified' {
            Invoke-ElevatedPart 'http://127.0.0.1:8770' '' 'x64' | Should -Be 0
            ($script:calls -join ',') | Should -Be 'processes,stop,files,service,acl,shortcut,entry 1.0.0,start'
            Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'Installing 1.0.0' }
            Test-Path $script:work | Should -BeFalse
        }
        It 'names an upgrade' {
            $script:installedVersion = '0.9.0'
            Invoke-ElevatedPart 'http://127.0.0.1:8770' '' 'x64' | Should -Be 0
            Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'Upgrading 0.9.0 -> 1.0.0' }
        }
        It 'a package whose version.txt differs from latest.json changes nothing' {
            $script:packageVersion = '1.0.1'
            Invoke-ElevatedPart 'http://127.0.0.1:8770' '' 'x64' | Should -Be 1
            $script:calls.Count | Should -Be 0                        # not even a service start
            Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'Installation failed:*1.0.1*latest.json*' }
        }
        It 'a package for the other architecture changes nothing' {
            Mock Get-PeMachine { 0xAA64 }
            Invoke-ElevatedPart 'http://127.0.0.1:8770' '' 'x64' | Should -Be 1
            $script:calls.Count | Should -Be 0
        }
        It 'an architecture other than x64 or arm64 is refused before anything' {
            Invoke-ElevatedPart 'http://127.0.0.1:8770' '' 'x86' | Should -Be 1
            Should -Invoke Get-CellGlidePackage -Times 0 -Exactly
            Should -Invoke New-PrivateWorkDir -Times 0 -Exactly
            $script:calls.Count | Should -Be 0
            Should -Invoke Write-Host -ParameterFilter { "$Object" -eq "Installation failed: unsupported architecture 'x86'" }
        }
        It 'an adapter that does not go away stops the install and restarts the service it stopped' {
            $script:stopResult = $false
            Invoke-ElevatedPart 'http://127.0.0.1:8770' '' 'x64' | Should -Be 1
            ($script:calls -join ',') | Should -Be 'processes,stop,start'
            Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'Installation failed:*cellglide0*' }
        }
        It 'a failed swap restarts the service it stopped' {
            Mock Install-Files { $script:calls.Add('files'); throw 'locked' }
            Invoke-ElevatedPart 'http://127.0.0.1:8770' '' 'x64' | Should -Be 1
            ($script:calls -join ',') | Should -Be 'processes,stop,files,start'
        }
        It 'a service that was already stopped stays stopped after a failure' {
            $script:serviceStatus = 'Stopped'
            Mock Install-Files { $script:calls.Add('files'); throw 'locked' }
            Invoke-ElevatedPart 'http://127.0.0.1:8770' '' 'x64' | Should -Be 1
            ($script:calls -join ',') | Should -Be 'processes,stop,files'
        }
        It 'a service this run created is never started after a failure' {
            $script:serviceStatus = $null
            Mock Set-DataDirAcl { $script:calls.Add('acl'); throw 'C:\ProgramData\CellGlide\logs is a junction or symbolic link.' }
            Invoke-ElevatedPart 'http://127.0.0.1:8770' '' 'x64' | Should -Be 1
            ($script:calls -join ',') | Should -Be 'processes,stop,files,service,acl'
        }
        It 'a refused data directory ACL starts nothing, even a service that was running' {
            Mock Set-DataDirAcl { $script:calls.Add('acl'); throw 'C:\ProgramData\CellGlide\logs is a junction or symbolic link.' }
            Invoke-ElevatedPart 'http://127.0.0.1:8770' '' 'x64' | Should -Be 1
            ($script:calls -join ',') | Should -Be 'processes,stop,files,service,acl'
            Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'Installation failed:*junction*' }
        }
        It 'a later failure after the ACL step restarts the service it stopped' {
            Mock New-StartMenuShortcut { $script:calls.Add('shortcut'); throw 'COM failed' }
            Invoke-ElevatedPart 'http://127.0.0.1:8770' '' 'x64' | Should -Be 1
            ($script:calls -join ',') | Should -Be 'processes,stop,files,service,acl,shortcut,start'
        }
        It 'a local zip is copied into the private folder and its hash printed' {
            $zip = Join-Path $TestDrive 'local.zip'; Set-Content $zip 'local' -NoNewline
            Invoke-ElevatedPart '' $zip 'x64' | Should -Be 0
            Should -Invoke Get-CellGlidePackage -Times 0 -Exactly
            Should -Invoke Expand-Package -ParameterFilter { $Zip -eq (Join-Path $script:work 'local.zip') }
            $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
            Should -Invoke Write-Host -ParameterFilter { "$Object" -eq "SHA-256: $hash" }
        }
        It 'a local zip the elevated part cannot see is refused with a clear message' {
            Invoke-ElevatedPart '' 'Z:\nowhere\pkg.zip' 'x64' | Should -Be 1
            Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'Installation failed: cannot find Z:\nowhere\pkg.zip*local path or a UNC path' }
            $script:calls.Count | Should -Be 0
        }
        It 'a work folder handed in by the child is used and left for the child to remove' {
            $given = Join-Path $TestDrive 'given'; $null = New-Item -ItemType Directory $given
            Invoke-ElevatedPart 'http://127.0.0.1:8770' '' 'x64' $given | Should -Be 0
            Should -Invoke New-PrivateWorkDir -Times 0 -Exactly
            Should -Invoke Get-CellGlidePackage -ParameterFilter { $WorkDir -eq (Join-Path $given 'download') }
            Test-Path $given | Should -BeTrue
        }
    }
    Context 'the main flow' {
        BeforeEach {
            Mock Write-Host {}
            Mock Get-NativeMachine { [uint16]0x8664 }
            Mock Get-SmartAppControlState { 0 }
            Mock Test-AppleService { $true }
            Mock Request-AppleDevices {}
            Mock Test-Elevated { $false }
            Mock Invoke-ElevatedChild { 0 }
            Mock Invoke-ElevatedPart { 0 }
            Mock Get-InstalledVersion { '1.0.0' }
            Mock Start-AppAsUser {}
            Mock Invoke-Doctor {}
        }
        It 'an enforced Smart App Control stops before anything changes' {
            Mock Get-SmartAppControlState { 1 }
            Invoke-CellGlideInstall -Unattended | Should -Be 2
            Should -Invoke Invoke-ElevatedChild -Times 0 -Exactly
            Should -Invoke Invoke-ElevatedPart -Times 0 -Exactly
            Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'Smart App Control is on*Nothing was changed.' }
        }
        It 'an unsupported download location stops at once' {
            Invoke-CellGlideInstall -BaseUrl 'https://example.com' -Unattended | Should -Be 1
            Should -Invoke Get-NativeMachine -Times 0 -Exactly
            Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'Installation failed: Unsupported download location: https://example.com' }
        }
        It 'a loopback source without -AllowLoopback stops at once' {
            Invoke-CellGlideInstall -BaseUrl 'http://127.0.0.1:8770' -Unattended | Should -Be 1
            Should -Invoke Get-NativeMachine -Times 0 -Exactly
            Should -Invoke Invoke-ElevatedChild -Times 0 -Exactly
            Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'Installation failed: Unsupported download location: http://127.0.0.1:8770*-AllowLoopback*' }
        }
        It 'CELLGLIDE_PKG_BASE is ignored' {
            $env:CELLGLIDE_PKG_BASE = 'http://127.0.0.1:8770'
            try { Invoke-CellGlideInstall -Unattended | Should -Be 0 }
            finally { Remove-Item Env:\CELLGLIDE_PKG_BASE }
            Should -Invoke Invoke-ElevatedChild -Times 1 -Exactly -ParameterFilter { $Parameters.BaseUrl -eq 'https://pkg.cellglide.com' -and -not $Parameters.AllowLoopback }
        }
        It 'an unknown machine needs a supported PC' {
            Mock Get-NativeMachine { [uint16]0x014C }
            Invoke-CellGlideInstall -Unattended | Should -Be 2
            Should -Invoke Invoke-ElevatedChild -Times 0 -Exactly
        }
        It 'without administrator rights the child gets the base, the architecture, the loopback switch and the full zip path' {
            $zip = Join-Path $TestDrive 'pkg.zip'; Set-Content $zip 'z'
            Push-Location $TestDrive
            try { Invoke-CellGlideInstall -BaseUrl 'http://127.0.0.1:8770/' -AllowLoopback -ZipPath 'pkg.zip' -Unattended | Should -Be 0 }
            finally { Pop-Location }
            Should -Invoke Invoke-ElevatedChild -Times 1 -Exactly -ParameterFilter {
                $ScriptText -eq $CellGlideInstallText -and $Parameters.Count -eq 4 -and
                $Parameters.BaseUrl -eq 'http://127.0.0.1:8770' -and $Parameters.Arch -eq 'x64' -and
                $Parameters.AllowLoopback -eq $true -and $Parameters.ZipPath -eq $zip
            }
            Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'Downloading from a local test source: http://127.0.0.1:8770' }
            Should -Invoke Invoke-Doctor -Times 1 -Exactly
            Should -Invoke Start-AppAsUser -Times 0 -Exactly
            Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'CellGlide 1.0.0 is installed.*' }
        }
        It 'a failed child is the result, and the app is not started' {
            Mock Invoke-ElevatedChild { 1 }
            Invoke-CellGlideInstall | Should -Be 1
            Should -Invoke Invoke-Doctor -Times 0 -Exactly
            Should -Invoke Start-AppAsUser -Times 0 -Exactly
        }
        It 'an administrator window runs the elevated part in the same process and starts the app' {
            Mock Test-Elevated { $true }
            Invoke-CellGlideInstall | Should -Be 0
            Should -Invoke Invoke-ElevatedPart -Times 1 -Exactly -ParameterFilter { $BaseUrl -eq 'https://pkg.cellglide.com' -and $Arch -eq 'x64' }
            Should -Invoke Invoke-ElevatedChild -Times 0 -Exactly
            Should -Invoke Start-AppAsUser -Times 1 -Exactly
        }
        It 'Apple Devices is offered only when missing, and unattended means no' {
            Mock Test-AppleService { $false }
            Invoke-CellGlideInstall -Unattended | Should -Be 0
            Should -Invoke Request-AppleDevices -Times 1 -Exactly -ParameterFilter { $Unattended }
        }
        It 'shows the end-user notice before the install' {
            Invoke-CellGlideInstall -Unattended | Should -Be 0
            Should -Invoke Write-Host -ParameterFilter { "$Object" -eq $EndUserNotice }
        }
    }
    Context 'checks of the PC, with the registry and services mocked' {
        BeforeEach { Mock Write-Host {} }
        It 'Smart App Control is read from the CI policy key' {
            $script:sac = $null
            Mock Get-ItemProperty -ParameterFilter { $Path -eq 'HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy' -and $Name -eq 'VerifiedAndReputablePolicyState' } {
                if ($null -eq $script:sac) { return $null }
                [pscustomobject]@{ VerifiedAndReputablePolicyState = $script:sac } }
            Get-SmartAppControlState | Should -Be 0
            foreach ($v in 0, 1, 2) { $script:sac = $v; Get-SmartAppControlState | Should -Be $v }
            Should -Invoke Get-ItemProperty -Times 4 -Exactly -ParameterFilter { $Path -eq 'HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy' }
        }
        It 'Apple''s service is found as the classic service' {
            Mock Test-Path -ParameterFilter { $LiteralPath -eq 'HKLM:\SYSTEM\CurrentControlSet\Services\Apple Mobile Device Service' } { $true }
            Mock Get-Service -ParameterFilter { $Name -eq 'Apple Mobile Device Service' } { [pscustomobject]@{ Status = 'Running' } }
            Mock Get-ChildItem { throw 'not needed' }
            Test-AppleService | Should -BeTrue
            Should -Invoke Write-Host -ParameterFilter { "$Object" -eq "Apple's device service is installed (Running)." }
        }
        It 'Apple''s service is found as the Store app package' {
            Mock Test-Path -ParameterFilter { $LiteralPath -like '*Apple Mobile Device Service' } { $false }
            Mock Get-ChildItem -ParameterFilter { $LiteralPath -like '*AppModel\PackageRepository\Packages' } {
                [pscustomobject]@{ PSChildName = 'Microsoft.WindowsCalculator_11.0_x64__8wekyb3d8bbwe' },
                [pscustomobject]@{ PSChildName = 'AppleInc.AppleDevices_1.0.0.0_x64__nzyj5cx40ttqa' } }
            Test-AppleService | Should -BeTrue
        }
        It 'no Apple service, or a look-alike package from another publisher, is missing' {
            Mock Test-Path -ParameterFilter { $LiteralPath -like '*Apple Mobile Device Service' } { $false }
            Mock Get-ChildItem -ParameterFilter { $LiteralPath -like '*AppModel\PackageRepository\Packages' } {
                [pscustomobject]@{ PSChildName = 'AppleInc.AppleDevices_1.0.0.0_x64__0000000000000' } }
            Test-AppleService | Should -BeFalse
        }
        It 'winget installs Apple Devices without accepting anything for the user' {
            Mock Read-Host { 'y' }
            Mock Get-Command -ParameterFilter { $Name -eq 'winget.exe' } { [pscustomobject]@{ Name = 'winget.exe' } }
            $global:cgWingetArgs = $null
            function global:winget.exe { $global:cgWingetArgs = @($args); $global:LASTEXITCODE = 0 }
            try { Request-AppleDevices }
            finally { Remove-Item Function:\global:winget.exe }
            ($global:cgWingetArgs -join ' ') | Should -BeExactly 'install --id 9NP83LWLPZ9K --source msstore'
            @($global:cgWingetArgs | Where-Object { $_ -like '--accept*' }).Count | Should -Be 0
        }
        It 'an elevated process starts the app through explorer.exe' {
            Mock Test-Elevated { $true }
            Mock Start-Process {}
            Start-AppAsUser
            Should -Invoke Start-Process -Times 1 -Exactly -ParameterFilter {
                $FilePath -eq "$env:SystemRoot\explorer.exe" -and "$ArgumentList" -eq '"C:\Program Files\CellGlide\app\CellGlide.exe"' }
        }
        It 'an ordinary process starts the app directly' {
            Mock Test-Elevated { $false }
            Mock Start-Process {}
            Start-AppAsUser
            Should -Invoke Start-Process -Times 1 -Exactly -ParameterFilter { $FilePath -eq 'C:\Program Files\CellGlide\app\CellGlide.exe' -and -not $ArgumentList }
        }
        It 'the uninstall entry is written with string and DWord values' {
            $key = 'HKCU:\Software\CellGlideTest-' + [guid]::NewGuid().ToString('N')
            $dir = Join-Path $TestDrive 'installed'; $null = New-Item -ItemType Directory $dir; [IO.File]::WriteAllBytes((Join-Path $dir 'a.bin'), (New-Object byte[] 3000))
            $savedKey = $UninstallKey; $savedDir = $InstallDir
            $UninstallKey = $key; $InstallDir = $dir
            try {
                Set-UninstallEntry '1.0.0'
                $item = Get-Item -LiteralPath $key
                foreach ($n in 'NoModify', 'NoRepair', 'EstimatedSize') { $item.GetValueKind($n) | Should -Be 'DWord' -Because $n }
                foreach ($n in 'DisplayName', 'DisplayVersion', 'Publisher', 'DisplayIcon', 'InstallLocation', 'UninstallString', 'URLInfoAbout') {
                    $item.GetValueKind($n) | Should -Be 'String' -Because $n
                }
                $item.GetValue('EstimatedSize') | Should -Be 3
                $item.GetValue('DisplayVersion') | Should -Be '1.0.0'
                Set-UninstallEntry '1.0.1'
                (Get-Item -LiteralPath $key).GetValue('DisplayVersion') | Should -Be '1.0.1'
            } finally {
                Remove-Item -LiteralPath $key -Recurse -Force -ErrorAction SilentlyContinue
                $UninstallKey = $savedKey; $InstallDir = $savedDir
            }
        }
    }
    It 'unattended never asks about Apple Devices' {
        Mock Read-Host { throw 'asked' }; Mock Write-Host {}
        Request-AppleDevices -Unattended
        Should -Invoke Read-Host -Times 0 -Exactly
    }
    It 'names the install action' {
        Get-InstallAction '' '1.0.0' | Should -Be 'Install'
        Get-InstallAction '1.0.0' '1.0.0' | Should -Be 'Reinstall'
        Get-InstallAction '1.0.0' '1.0.1' | Should -Be 'Upgrade'
        Get-InstallAction '1.0.1' '1.0.0' | Should -Be 'Downgrade'
        Get-InstallAction 'junk' '1.0.0' | Should -Be 'Upgrade'
    }
    It 'Only processes under the install directory are stopped' {
        $p = @(
            [pscustomobject]@{ ProcessId = 1; ExecutablePath = 'C:\Program Files\CellGlide\app\CellGlide.exe' },
            [pscustomobject]@{ ProcessId = 2; ExecutablePath = 'C:\PROGRAM FILES\CELLGLIDE\cellglide.exe' },
            [pscustomobject]@{ ProcessId = 3; ExecutablePath = 'C:\Program Files\CellGlide\cellglide-service.exe' },
            [pscustomobject]@{ ProcessId = 4; ExecutablePath = 'C:\Users\iloss\CellGlide-Desktop\windows\CellGlide.App\bin\CellGlide.exe' },
            [pscustomobject]@{ ProcessId = 5; ExecutablePath = 'C:\Program Files\CellGlideX\CellGlide.exe' },
            [pscustomobject]@{ ProcessId = 6; ExecutablePath = $null })
        Select-ProcessesToStop $p 'C:\Program Files\CellGlide' | Should -Be @(1, 2)
    }
    It 'writes an uninstall entry that runs our script' {
        $e = Get-UninstallEntry 'C:\Program Files\CellGlide' '1.0.0' 81234
        $e.UninstallString | Should -Match '-File "C:\\Program Files\\CellGlide\\uninstall\.ps1"$'
        $e.DisplayVersion | Should -Be '1.0.0'; $e.EstimatedSize | Should -Be 81234; $e.NoModify | Should -Be 1
    }
    It 'No exit outside the guarded footer' {
        $exits = $script:Ast.FindAll({ param($n) $n -is [Management.Automation.Language.ExitStatementAst] }, $true)
        $exits.Count | Should -Be 1
        $exits[0].Parent.Parent.Extent.Text | Should -Match '^if \(\$PSCommandPath\)'
    }
    It 'No preference is changed at the top level' {
        $top = $script:Ast.EndBlock.Statements | Where-Object { $_ -is [Management.Automation.Language.AssignmentStatementAst] }
        ($top | ForEach-Object { $_.Left.Extent.Text }) | Should -Not -Contain '$ErrorActionPreference'
        ($top | ForEach-Object { $_.Left.Extent.Text }) | Should -Not -Contain '$ProgressPreference'
    }
    It 'every function sets its own error preference' {
        $functions = $script:Ast.FindAll({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] }, $true)
        $functions.Count | Should -BeGreaterThan 30
        foreach ($f in $functions) {
            $set = $f.Body.FindAll({ param($n) $n -is [Management.Automation.Language.AssignmentStatementAst] -and $n.Left.Extent.Text -eq '$ErrorActionPreference' }, $false)
            @($set).Count | Should -BeGreaterThan 0 -Because $f.Name
        }
    }
    It 'The elevated part''s log is printed by the parent' {
        Mock Write-Host {}
        $start = { param($EncodedCommand, $Info)
            $script:logExisted = (Test-Path $Info.LogPath) -and (Get-Item $Info.LogPath).Length -eq 0
            Set-Content $Info.LogPath "line from child`nsecond line"
            $script:ranText = [IO.File]::ReadAllText($Info.ScriptPath); $script:bootstrap = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String($EncodedCommand))
            [pscustomobject]@{ ExitCode = 2 } | Add-Member -MemberType ScriptMethod -Name WaitForExit -Value { } -PassThru }
        Invoke-ElevatedChild "# the running script`n" @{ Arch = 'x64' } $start | Should -Be 2
        $script:ranText | Should -Be "# the running script`n"          # the child runs the parent's own text, not a download
        $script:logExisted | Should -BeTrue                              # the parent made the empty log file first
        $blob = [regex]::Match($script:bootstrap, "FromBase64String\('([A-Za-z0-9+/=]+)'\)").Groups[1].Value
        $config = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($blob)) | ConvertFrom-Json
        $config.Parameters.ElevatedPart | Should -BeTrue
        $config.Parameters.Arch | Should -BeExactly 'x64'
        Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'line from child' }
        Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'second line' }
    }
    It 'the child''s script and log are removed afterwards' {
        Mock Write-Host {}
        $start = { param($EncodedCommand, $Info)
            $script:paths = @($Info.ScriptPath, $Info.LogPath)
            Set-Content $Info.LogPath 'x'
            [pscustomobject]@{ ExitCode = 0 } | Add-Member -MemberType ScriptMethod -Name WaitForExit -Value { } -PassThru }
        Invoke-ElevatedChild "# x`n" @{} $start | Should -Be 0
        foreach ($p in $script:paths) { Test-Path $p | Should -BeFalse }
        $script:paths[0] | Should -BeLike '*\cellglide-install-*.ps1'
        $script:paths[1] | Should -Match '\\cellglide-install-[0-9a-f]{32}\.log$'
    }
    It 'a cancelled UAC prompt is a cancelled install' {
        Mock Write-Host {}
        Invoke-ElevatedChild "# x`n" @{} { param($EncodedCommand, $Info) throw 'The operation was canceled by the user.' } | Should -Be 1
        Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'Installation cancelled.' }
    }
    It 'refuses to start the child without the script text' {
        { Invoke-ElevatedChild '' @{} { throw 'not reached' } } | Should -Throw "*Cannot find the running script's text*"
    }
    It 'no outside value appears in the bootstrap''s code' {
        $q = [string][char]0x2018 + [char]0x2019 + [char]0x201A + [char]0x201B
        $nasty = "C:\x$q`"``'`$(Write-Host pwned);`n\y"
        $code = New-ChildBootstrap "C:\t$q';`$(1)\s.ps1" ('AB' * 32) @{ ZipPath = $nasty; BaseUrl = "a'$q"; AllowLoopback = $true; Unattended = [switch]$true }
        $errors = $null; $tokens = $null
        $ast = [Management.Automation.Language.Parser]::ParseInput($code, [ref]$tokens, [ref]$errors)
        @($errors).Count | Should -Be 0
        # Every string literal in the generated code is ours: two fixed words and one base64 blob.
        $literals = @($ast.FindAll({ param($n) $n -is [Management.Automation.Language.StringConstantExpressionAst] -and $n.StringConstantType -ne 'BareWord' }, $true) | ForEach-Object { $_.Value })
        foreach ($l in $literals) { $l | Should -Match '\A(Stop|x2|[A-Za-z0-9+/=]{40,})\z' }
        @($ast.FindAll({ param($n) $n -is [Management.Automation.Language.ExpandableStringExpressionAst] }, $true)).Count | Should -Be 0
        foreach ($ch in $q.ToCharArray()) { $code.Contains([string]$ch) | Should -BeFalse }
        $code | Should -Not -BeLike '*pwned*'
        # The blob carries every value exactly; switches arrive as booleans.
        $blob = [regex]::Match($code, "FromBase64String\('([A-Za-z0-9+/=]+)'\)").Groups[1].Value
        $config = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($blob)) | ConvertFrom-Json
        $config.ScriptPath | Should -BeExactly "C:\t$q';`$(1)\s.ps1"
        $config.Sha256 | Should -BeExactly ('ab' * 32)
        $config.Parameters.ZipPath | Should -BeExactly $nasty
        $config.Parameters.AllowLoopback | Should -BeOfType [bool]
        $config.Parameters.Unattended | Should -BeOfType [bool]
    }
    Context 'the bootstrap run for real in a separate (unelevated) PowerShell' {
        BeforeAll {
            # A stand-in for install.ps1: the same parameters; it writes what it got to the log.
            $script:childText = @'
param([string]$BaseUrl = '', [string]$ZipPath = '', [switch]$Unattended, [switch]$ElevatedPart, [string]$Arch = '', [string]$LogPath = '', [switch]$AllowLoopback)
[IO.File]::WriteAllText($LogPath, "got $ElevatedPart|$BaseUrl|$Arch|$ZipPath|$AllowLoopback", [Text.Encoding]::UTF8)
$code = 7
'@
            $script:runReal = { param($EncodedCommand, $Info)
                if ($script:tamper) { Add-Content -LiteralPath $Info.ScriptPath "`n[IO.File]::WriteAllText(`$LogPath, 'tampered')" }
                $p = Start-Process powershell.exe -PassThru -WindowStyle Hidden -ArgumentList "-NoProfile -ExecutionPolicy Bypass -EncodedCommand $EncodedCommand"
                $null = $p.Handle
                $p }
        }
        BeforeEach { Mock Write-Host {}; $script:tamper = $false }
        It 'runs the checked text from memory with the parameters, and exits with its $code' {
            $zip = "C:\it's here\a b.zip"
            Invoke-ElevatedChild $script:childText @{ BaseUrl = 'http://127.0.0.1:8770'; Arch = 'x64'; ZipPath = $zip; AllowLoopback = $true } $script:runReal | Should -Be 7
            Should -Invoke Write-Host -ParameterFilter { "$Object" -eq "got True|http://127.0.0.1:8770|x64|$zip|True" }
        }
        It 'paths and values with any quote or PowerShell syntax run no injected code and arrive unchanged' {
            # %TEMP% (where the parent writes the script and the log) and -ZipPath carry each of
            # the four Unicode single quotes, an ASCII quote, a backtick, $( ) and ; - with the
            # old literal quoting, the first quote ended the string and "ni <marker>" ran.
            $q = [string][char]0x2018 + [char]0x2019 + [char]0x201A + [char]0x201B
            $work = Join-Path $TestDrive ('i' + [guid]::NewGuid().ToString('N').Substring(0, 6)); $null = New-Item -ItemType Directory $work
            $marker = 'pwned-' + [guid]::NewGuid().ToString('N').Substring(0, 8)          # short: the whole path stays under MAX_PATH
            $tmp = Join-Path $work ([string][char]0x2018 + ";ni $marker;" + [char]0x2019 + "'``x`$(ni $marker)" + [char]0x201A + [char]0x201B)
            $null = [IO.Directory]::CreateDirectory($tmp)
            $zip = "C:\z" + $q + "`"'``;`$(ni $marker);" + [char]0x2018 + ";ni $marker;" + [char]0x2019 + "\a b.zip"
            $savedTmp = $env:TMP; $savedTemp = $env:TEMP
            Push-Location $work
            try {
                $env:TMP = $tmp; $env:TEMP = $tmp
                [IO.Path]::GetTempPath().StartsWith($tmp) | Should -BeTrue
                Invoke-ElevatedChild $script:childText @{ BaseUrl = 'http://127.0.0.1:8770'; Arch = 'x64'; ZipPath = $zip; AllowLoopback = $true } $script:runReal | Should -Be 7
            } finally { $env:TMP = $savedTmp; $env:TEMP = $savedTemp; Pop-Location }
            @(Get-ChildItem -LiteralPath $work -Recurse -Force -Filter 'pwned-*').Count | Should -Be 0
            Should -Invoke Write-Host -ParameterFilter { "$Object" -ceq "got True|http://127.0.0.1:8770|x64|$zip|True" }
            @(Get-ChildItem -LiteralPath $tmp -Force).Count | Should -Be 0          # script and log removed
        }
        It 'a script changed after the parent wrote it is not run' {
            $script:tamper = $true
            Invoke-ElevatedChild $script:childText @{ Arch = 'x64' } $script:runReal | Should -Be 1
            Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'Installation failed: the copy of the installer in %TEMP% changed*' }
            Should -Invoke Write-Host -Times 0 -Exactly -ParameterFilter { "$Object" -like 'got*' -or "$Object" -eq 'tampered' }
        }
        It 'install.ps1''s own guard and footer run in the bootstrap and give it the exit code' {
            # install.ps1's param block, capture, iex guard and footer, with a stand-in main.
            $stmts = $script:Ast.EndBlock.Statements
            $guard = ($stmts | Where-Object { $_ -is [Management.Automation.Language.IfStatementAst] } | Select-Object -First 1).Extent.Text
            $capture = ($stmts | Where-Object { $_.Extent.Text -like '$CellGlideInstallText =*' }).Extent.Text
            $footer = ($stmts | Select-Object -Last 2 | ForEach-Object { $_.Extent.Text }) -join "`n"
            $text = $script:Ast.ParamBlock.Extent.Text + "`n" + $capture + "`n" + $guard + "`n" +
                'function Invoke-CellGlideInstall { param($BaseUrl, $ZipPath, [switch]$Unattended, [switch]$ElevatedPart, $Arch, $LogPath, [switch]$AllowLoopback) [IO.File]::WriteAllText($LogPath, "main $ElevatedPart $Arch"); 5 }' + "`n" + $footer
            Invoke-ElevatedChild $text @{ BaseUrl = 'https://pkg.cellglide.com'; Arch = 'arm64' } $script:runReal | Should -Be 5
            Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'main True arm64' }
        }
    }
    It 'iex leaves no functions or variables behind, and the caller''s own variables keep their values' {
        # install.ps1's param block, capture and guard, then a body that defines things.
        $stmts = $script:Ast.EndBlock.Statements
        $guard = ($stmts | Where-Object { $_ -is [Management.Automation.Language.IfStatementAst] } | Select-Object -First 1).Extent.Text
        $capture = ($stmts | Where-Object { $_.Extent.Text -like '$CellGlideInstallText =*' }).Extent.Text
        $text = $script:Ast.ParamBlock.Extent.Text + "`n" + $capture + "`n" + $guard + "`n" +
            "function CgLeak { }`n`$cgLeak = 1`n[Console]::Out.WriteLine('body ran')"
        $iex = "iex ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($text)))')))"
        $names = "'cgLeak', 'CellGlideInstallText', 'BaseUrl', 'ZipPath', 'Unattended', 'ElevatedPart', 'Arch', 'LogPath', 'AllowLoopback', '__CellGlideCallerVars', 'keep', 'name', 's', 'v', 'a', 'Saved', 'Names'"
        $report = "[Console]::Out.WriteLine('left: ' + (@('CgLeak', 'Invoke-CellGlideInstall') | Where-Object { Get-Command `$_ -ErrorAction SilentlyContinue }) + '|' + (@($names) | Where-Object { Get-Variable `$_ -ErrorAction SilentlyContinue }))"
        $quiet = "`$ProgressPreference = 'SilentlyContinue'`n"       # no "Preparing modules for first use" CLIXML on stderr
        # Nothing of the caller's: nothing is left.
        $out = @(& powershell.exe -NoProfile -EncodedCommand ([Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes("$quiet$iex`n$report"))))
        $out | Should -Contain 'body ran'
        $out | Should -Contain 'left: |'
        # The caller's own variables of the same names come back with their values and options.
        $pre = "`$BaseUrl = 'mine'; `$Arch = 42; `$CellGlideInstallText = 'own text'; `$__CellGlideCallerVars = 'h'; `$ZipPath = 'zp'"
        $check = "[Console]::Out.WriteLine('vals: ' + `$BaseUrl + '|' + `$Arch + '|' + `$Arch.GetType().Name + '|' + `$CellGlideInstallText + '|' + `$__CellGlideCallerVars + '|' + `$ZipPath + '|' + (Get-Variable ZipPath).Options + '|' + [bool](Get-Variable Unattended -ErrorAction SilentlyContinue) + '|' + [bool](Get-Variable cgLeak -ErrorAction SilentlyContinue))"
        $out = @(& powershell.exe -NoProfile -EncodedCommand ([Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes("$quiet$pre`n$iex`n$check"))) 2>&1 | ForEach-Object { "$_" })
        $out | Should -Contain 'body ran'
        $out | Should -Contain 'vals: mine|42|Int32|own text|h|zp|None|False|False'
    }
    It 'the log for the parent goes only into the empty file the parent made' {
        $src = Join-Path $TestDrive 'install.log'
        Set-Content $src @('**********************', 'Windows PowerShell transcript start', 'x', '**********************', 'Installing 1.0.0', '**********************', 'Windows PowerShell transcript end', 'y', '**********************')
        $name = { 'cellglide-install-' + [guid]::NewGuid().ToString('N') + '.log' }
        $dest = Join-Path $TestDrive (& $name); [IO.File]::WriteAllBytes($dest, @())
        Copy-LogToParent $src $dest
        @(Get-Content $dest) | Should -Be @('Installing 1.0.0')
        { Copy-LogToParent $src $dest } | Should -Throw '*not the empty file*'          # written once: never again
        { Copy-LogToParent $src (Join-Path $TestDrive (& $name)) } | Should -Throw    # missing: the child never creates it
        { Copy-LogToParent $src (Join-Path $TestDrive 'other.log') } | Should -Throw '*unexpected log path*'
        # A hard link to another file has two links.
        $victim = Join-Path $TestDrive 'victim.txt'; [IO.File]::WriteAllBytes($victim, @())
        $hard = Join-Path $TestDrive (& $name); $null = New-Item -ItemType HardLink -Path $hard -Target $victim
        { Copy-LogToParent $src $hard } | Should -Throw '*2 links*'
        (Get-Item $victim).Length | Should -Be 0
        # Through a junction, the final path is somewhere else.
        $real = Join-Path $TestDrive 'realdir'; $null = New-Item -ItemType Directory $real
        $leaf = & $name; [IO.File]::WriteAllBytes((Join-Path $real $leaf), @())
        $link = Join-Path $TestDrive 'linkdir'; $null = New-Item -ItemType Junction -Path $link -Target $real
        try { { Copy-LogToParent $src (Join-Path $link $leaf) } | Should -Throw '*leads to*' }
        finally { [IO.Directory]::Delete($link) }
        (Get-Item (Join-Path $real $leaf)).Length | Should -Be 0
    }
    It 'the log for the parent is never written through a symbolic link' -Tag 'Elevated' {
        $src = Join-Path $TestDrive 'install2.log'; Set-Content $src 'Installing 1.0.0'
        $victim = Join-Path $TestDrive 'victim2.txt'; [IO.File]::WriteAllBytes($victim, @())
        $dest = Join-Path $TestDrive ('cellglide-install-' + [guid]::NewGuid().ToString('N') + '.log')
        $null = New-Item -ItemType SymbolicLink -Path $dest -Target $victim
        { Copy-LogToParent $src $dest } | Should -Throw '*leads to*'
        (Get-Item $victim).Length | Should -Be 0
    }
    It 'only old work folders of killed installs are removed, never a running one''s' {
        $root = Join-Path $TestDrive ('stale-' + [guid]::NewGuid().ToString('N')); $null = New-Item -ItemType Directory $root
        $mk = { param($n, $minutes) $d = Join-Path $root $n; $null = New-Item -ItemType Directory $d; Set-Content (Join-Path $d 'f') 'x'; (Get-Item $d).CreationTimeUtc = (Get-Date).ToUniversalTime().AddMinutes(-$minutes); $d }
        $old = & $mk 'CellGlide.work-old' 120
        $recent = & $mk 'CellGlide.work-recent' 5
        $mine = & $mk 'CellGlide.work-mine' 300
        $other = & $mk 'Other.work-old' 300
        $outside = Join-Path $TestDrive ('outside-' + [guid]::NewGuid().ToString('N')); $null = New-Item -ItemType Directory $outside; Set-Content (Join-Path $outside 'keep.txt') 'x'
        $linked = & $mk 'CellGlide.work-linked' 300; $null = New-Item -ItemType Junction -Path (Join-Path $linked 'j') -Target $outside
        try {
            Remove-StaleWorkDirs $mine $root
            Test-Path $old | Should -BeFalse
            foreach ($d in $recent, $mine, $other, $linked) { Test-Path $d | Should -BeTrue -Because $d }
            Test-Path (Join-Path $outside 'keep.txt') | Should -BeTrue
        } finally { [IO.Directory]::Delete((Join-Path $linked 'j')) }
    }
    It 'a mapped network drive becomes its UNC path' {
        ConvertTo-UncPath 'Z:\pkgs\a.zip' 'Network' '\\server\share' | Should -Be '\\server\share\pkgs\a.zip'
        ConvertTo-UncPath 'Z:\a.zip' 'Network' '\\server\share\' | Should -Be '\\server\share\a.zip'
        ConvertTo-UncPath 'C:\pkgs\a.zip' 'Fixed' '' | Should -Be 'C:\pkgs\a.zip'
        ConvertTo-UncPath '\\server\share\a.zip' 'Network' '' | Should -Be '\\server\share\a.zip'
        { ConvertTo-UncPath 'Z:\a.zip' 'Network' '' } | Should -Throw '*copy the zip to a local folder*'
    }
    It 'transcript banners are not printed' {
        $lines = @('**********************', 'Windows PowerShell transcript start', 'Start time: 1', 'Username: x', '**********************',
            'Installing 1.0.0', 'Service CellGlide is running.',
            '**********************', 'Windows PowerShell transcript end', 'End time: 2', '**********************')
        Get-TranscriptBody $lines | Should -Be @('Installing 1.0.0', 'Service CellGlide is running.')
        Get-TranscriptBody @('only', 'lines') | Should -Be @('only', 'lines')
    }
    It 'iex hands the script its own text' {
        # The same capture statement install.ps1 uses, run through iex in a fresh PowerShell.
        $capture = ($script:Ast.EndBlock.Statements | Where-Object { $_.Extent.Text -like '$CellGlideInstallText =*' }).Extent.Text
        $capture | Should -Not -BeNullOrEmpty
        $snippet = "$capture`n[Console]::Out.Write(`$CellGlideInstallText)"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes("`$ProgressPreference = 'SilentlyContinue'`niex ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($snippet)))')))"))
        # A native command's output comes back line by line; join the lines with LF as the snippet has them.
        (@(& powershell.exe -NoProfile -EncodedCommand $encoded) -join "`n").Trim() | Should -Be $snippet.Trim()
    }
    It '-File hands the script its own text' {
        $capture = ($script:Ast.EndBlock.Statements | Where-Object { $_.Extent.Text -like '$CellGlideInstallText =*' }).Extent.Text
        $f = Join-Path $TestDrive 'self.ps1'; $out = Join-Path $TestDrive 'self.out'
        [IO.File]::WriteAllText($f, "# first`n$capture`n[IO.File]::WriteAllText(`$args[0], `$CellGlideInstallText)`n# last`n")
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $f $out
        [IO.File]::ReadAllText($out) | Should -BeExactly ([IO.File]::ReadAllText($f))
    }
    It 'is ASCII only' { [IO.File]::ReadAllBytes((Resolve-Path $script:File)) | Where-Object { $_ -gt 0x7F } | Should -BeNullOrEmpty }
    It 'shows the end-user notice word for word' {
        $first = (Get-Content "$PSScriptRoot\..\..\licenses\windows-end-user-notice.txt" -TotalCount 1)
        (Get-Content $script:File -Raw).Contains($first) | Should -BeTrue
        $EndUserNotice | Should -BeExactly $first
    }
}
