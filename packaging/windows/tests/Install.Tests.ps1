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
        Resolve-BaseUrl '' '' | Should -Be 'https://pkg.cellglide.com'
        Resolve-BaseUrl '' 'http://127.0.0.1:8770/' | Should -Be 'http://127.0.0.1:8770'
        Resolve-BaseUrl 'http://localhost:9' 'http://127.0.0.1:1' | Should -Be 'http://localhost:9'
        foreach ($bad in 'https://evil.example', 'http://pkg.cellglide.com', 'http://10.0.0.1:80', 'https://127.0.0.1:1', 'file:///C:/x') {
            { Resolve-BaseUrl $bad '' } | Should -Throw '*Unsupported download location*'
        }
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
        { Get-CellGlidePackage 'http://127.0.0.1:1' 'x64' (Join-Path $TestDrive 'w4') $fake } | Should -Throw '*SHA-256 mismatch*'
    }
    It 'A hash mismatch stops before anything changes' {
        Mock Get-CellGlidePackage { throw 'SHA-256 mismatch for CellGlide-1.0.0-windows-x64.zip' }
        Mock Stop-CellGlideProcesses {}; Mock Stop-OurService { $true }; Mock Install-Files {}
        Mock Set-OurService {}; Mock Set-DataDirAcl {}; Mock Set-UninstallEntry {}; Mock New-StartMenuShortcut {}
        Mock Start-Service {}; Mock Write-Host {}
        Invoke-ElevatedPart 'https://pkg.cellglide.com' '' 'x64' | Should -Be 1
        foreach ($changer in 'Stop-CellGlideProcesses', 'Stop-OurService', 'Install-Files', 'Set-OurService', 'Set-DataDirAcl', 'Set-UninstallEntry', 'New-StartMenuShortcut') {
            Should -Invoke $changer -Times 0 -Exactly -Because $changer
        }
        Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'Installation failed:*SHA-256 mismatch*' }
    }
    Context 'the elevated part with every system change mocked' {
        BeforeEach {
            $script:calls = New-Object System.Collections.Generic.List[string]
            $script:work = Join-Path $TestDrive ('work-' + [guid]::NewGuid().ToString('N'))
            Mock New-PrivateWorkDir { $null = New-Item -ItemType Directory $script:work; $script:work }
            Mock Get-CellGlidePackage { [pscustomobject]@{ ZipPath = 'pkg.zip'; Version = '1.0.0'; TestBuild = $false } }
            Mock Expand-Package { $null =New-Item -ItemType Directory $Dest; Set-Content (Join-Path $Dest 'version.txt') $script:packageVersion }
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
            Mock Get-Service { [pscustomobject]@{ Status = 'Stopped' } | Add-Member -MemberType ScriptMethod -Name WaitForStatus -Value { } -PassThru }
            Mock Write-Host {}
            $script:packageVersion = '1.0.0'; $script:installedVersion = ''; $script:stopResult = $true
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
            $script:calls.Count | Should -Be 1                       # only the restart attempt
            $script:calls[0] | Should -Be 'start'
            Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'Installation failed:*1.0.1*latest.json*' }
        }
        It 'a package for the other architecture changes nothing' {
            Mock Get-PeMachine { 0xAA64 }
            Invoke-ElevatedPart 'http://127.0.0.1:8770' '' 'x64' | Should -Be 1
            Should -Invoke Install-Files -Times 0 -Exactly
            Should -Invoke Stop-CellGlideProcesses -Times 0 -Exactly
        }
        It 'an adapter that does not go away stops the install and restarts the service' {
            $script:stopResult = $false
            Invoke-ElevatedPart 'http://127.0.0.1:8770' '' 'x64' | Should -Be 1
            ($script:calls -join ',') | Should -Be 'processes,stop,start'
            Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'Installation failed:*cellglide0*' }
        }
        It 'a failed swap restarts the existing service' {
            Mock Install-Files { $script:calls.Add('files'); throw 'locked' }
            Invoke-ElevatedPart 'http://127.0.0.1:8770' '' 'x64' | Should -Be 1
            ($script:calls -join ',') | Should -Be 'processes,stop,files,start'
        }
        It 'a local zip is copied into the private folder and its hash printed' {
            $zip = Join-Path $TestDrive 'local.zip'; Set-Content $zip 'local' -NoNewline
            Invoke-ElevatedPart '' $zip 'x64' | Should -Be 0
            Should -Invoke Get-CellGlidePackage -Times 0 -Exactly
            Should -Invoke Expand-Package -ParameterFilter { $Zip -eq (Join-Path $script:work 'local.zip') }
            $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
            Should -Invoke Write-Host -ParameterFilter { "$Object" -eq "SHA-256: $hash" }
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
        It 'an unknown machine needs a supported PC' {
            Mock Get-NativeMachine { [uint16]0x014C }
            Invoke-CellGlideInstall -Unattended | Should -Be 2
            Should -Invoke Invoke-ElevatedChild -Times 0 -Exactly
        }
        It 'without administrator rights the child gets the base, the architecture and the full zip path' {
            $zip = Join-Path $TestDrive 'pkg.zip'; Set-Content $zip 'z'
            Push-Location $TestDrive
            try { Invoke-CellGlideInstall -BaseUrl 'http://127.0.0.1:8770/' -ZipPath 'pkg.zip' -Unattended | Should -Be 0 }
            finally { Pop-Location }
            Should -Invoke Invoke-ElevatedChild -Times 1 -Exactly -ParameterFilter {
                $ScriptText -eq $CellGlideInstallText -and
                ($Arguments -join '|') -eq "-BaseUrl|http://127.0.0.1:8770|-Arch|x64|-ZipPath|$zip"
            }
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
        $start = { param($Path, $ArgList)
            $log = $ArgList[[array]::IndexOf($ArgList, '-LogPath') + 1]
            Set-Content $log "line from child`nsecond line"
            $script:ranText = Get-Content $Path -Raw; $script:ranArgs = $ArgList
            [pscustomobject]@{ ExitCode = 3 } | Add-Member -MemberType ScriptMethod -Name WaitForExit -Value { } -PassThru }
        Invoke-ElevatedChild "# the running script`n" @('-Arch', 'x64') $start | Should -Be 3
        $script:ranText | Should -Be "# the running script`n"          # the child runs the parent's own text, not a download
        $script:ranArgs | Should -Contain '-ElevatedPart'
        Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'line from child' }
        Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'second line' }
    }
    It 'the child''s script and log are removed afterwards' {
        Mock Write-Host {}
        $start = { param($Path, $ArgList)
            $script:paths = @($Path, $ArgList[[array]::IndexOf($ArgList, '-LogPath') + 1])
            Set-Content $script:paths[1] 'x'
            [pscustomobject]@{ ExitCode = 0 } | Add-Member -MemberType ScriptMethod -Name WaitForExit -Value { } -PassThru }
        Invoke-ElevatedChild "# x`n" @() $start | Should -Be 0
        foreach ($p in $script:paths) { Test-Path $p | Should -BeFalse }
        $script:paths[0] | Should -BeLike '*\cellglide-install-*.ps1'
    }
    It 'a cancelled UAC prompt is a cancelled install' {
        Mock Write-Host {}
        Invoke-ElevatedChild "# x`n" @() { param($Path, $ArgList) throw 'The operation was canceled by the user.' } | Should -Be 1
        Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'Installation cancelled.' }
    }
    It 'refuses to start the child without the script text' {
        { Invoke-ElevatedChild '' @() { throw 'not reached' } } | Should -Throw "*Cannot find the running script's text*"
    }
    It 'transcript banners are not printed' {
        $lines = @('**********************', 'Windows PowerShell transcript start', 'Start time: 1', 'Username: x', '**********************',
            'Installing 1.0.0', 'Service CellGlide is running.',
            '**********************', 'Windows PowerShell transcript end', 'End time: 2', '**********************')
        Get-TranscriptBody $lines | Should -Be @('Installing 1.0.0', 'Service CellGlide is running.')
        Get-TranscriptBody @('only', 'lines') | Should -Be @('only', 'lines')
    }
    It 'arguments with spaces and quotes reach the child unchanged' {
        $list = @('-File', 'C:\Users\a b\cellglide-install-1.ps1', '-ZipPath', 'C:\x y\', 'say "hi"', '')
        $line = ConvertTo-CommandLine $list
        # Split the line back the way the child's runtime does (CommandLineToArgvW rules).
        Add-Type -Namespace CgTest -Name Argv -MemberDefinition '[DllImport("shell32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr CommandLineToArgvW(string cmd, out int n); [DllImport("kernel32.dll")] public static extern IntPtr LocalFree(IntPtr p);' -ErrorAction SilentlyContinue
        $n = 0; $ptr = [CgTest.Argv]::CommandLineToArgvW("x.exe $line", [ref]$n)
        $parsed = for ($i = 1; $i -lt $n; $i++) { [Runtime.InteropServices.Marshal]::PtrToStringUni([Runtime.InteropServices.Marshal]::ReadIntPtr($ptr, $i * [IntPtr]::Size)) }
        $null = [CgTest.Argv]::LocalFree($ptr)
        $parsed | Should -Be $list
    }
    It 'iex hands the script its own text' {
        # The same capture statement install.ps1 uses, run through iex in a fresh PowerShell.
        $capture = ($script:Ast.EndBlock.Statements | Where-Object { $_.Extent.Text -like '$CellGlideInstallText =*' }).Extent.Text
        $capture | Should -Not -BeNullOrEmpty
        $snippet = "$capture`n[Console]::Out.Write(`$CellGlideInstallText)"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes("iex ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($snippet)))')))"))
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
