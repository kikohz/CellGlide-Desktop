# Pester 5 tests for packaging\windows\uninstall.ps1 (run by scripts\test-windows-scripts.ps1).
# Only the script's functions and constants are loaded (Import-ScriptFunctions); the steps that
# change the system are mocked, or pointed at temporary folders and randomly named HKCU keys.
BeforeAll {
    . "$PSScriptRoot\..\..\..\scripts\tests\Import-ScriptFunctions.ps1"
    $script:File = "$PSScriptRoot\..\uninstall.ps1"; . Import-ScriptFunctions $script:File
    $script:Ast = [Management.Automation.Language.Parser]::ParseFile((Resolve-Path $script:File), [ref]$null, [ref]$null)
    $script:Exe = 'C:\Program Files\CellGlide\app\CellGlide.exe'
    $script:NewTempKey = { $k = 'HKCU:\Software\CellGlideTest-' + [guid]::NewGuid().ToString('N'); $null = New-Item -Path $k; $k }
    $script:FakeProcess = { param($code) [pscustomobject]@{ ExitCode = $code } | Add-Member -MemberType ScriptMethod -Name WaitForExit -Value { } -PassThru }
}
Describe 'uninstall.ps1' {
    It 'Only our own Run value is ours' {
        $exe = 'C:\Program Files\CellGlide\app\CellGlide.exe'
        Test-OurRunValue '"C:\Program Files\CellGlide\app\CellGlide.exe" --tray' $exe | Should -BeTrue
        Test-OurRunValue '"c:\program files\cellglide\APP\cellglide.exe" --tray' $exe | Should -BeTrue
        Test-OurRunValue '"D:\dev\CellGlide.exe" --tray' $exe | Should -BeFalse
        Test-OurRunValue '"C:\Program Files\CellGlide\app\CellGlide.exe.bak"' $exe | Should -BeFalse
        Test-OurRunValue '' $exe | Should -BeFalse
    }
    It 'reads unquoted and odd Run values without guessing' {
        Test-OurRunValue 'C:\Program Files\CellGlide\app\CellGlide.exe' $script:Exe | Should -BeTrue
        Test-OurRunValue 'C:\Program Files\CellGlide\app\CellGlide.exe --tray' $script:Exe | Should -BeTrue
        Test-OurRunValue '  "C:\Program Files\CellGlide\app\CellGlide.exe"  ' $script:Exe | Should -BeTrue
        Test-OurRunValue 'C:\Program Files\CellGlide\app\CellGlide.exe.bak --tray' $script:Exe | Should -BeFalse
        Test-OurRunValue '"C:\Program Files\CellGlide\app\CellGlide.exe' $script:Exe | Should -BeFalse          # no closing quote
        Test-OurRunValue '"C:\Program Files\CellGlideX\app\CellGlide.exe" --tray' $script:Exe | Should -BeFalse
        Test-OurRunValue $script:Exe '' | Should -BeFalse
    }
    It 'Purge refuses a junction' -Tag 'Elevated' {
        $victim = Join-Path $TestDrive 'victim'; New-Item -ItemType Directory $victim | Out-Null
        Set-Content (Join-Path $victim 'keep.txt') 'x'
        $dir = Join-Path $TestDrive 'profile\AppData\Local\CellGlide'; New-Item -ItemType Directory $dir | Out-Null
        New-Item -ItemType Junction -Path (Join-Path $dir 'old') -Target $victim | Out-Null
        { Remove-DataDir $dir } | Should -Throw '*junction or symbolic link*'
        Test-Path (Join-Path $victim 'keep.txt') | Should -BeTrue
        Test-Path $dir | Should -BeTrue
    }
    It 'a junction deep inside stops the purge before anything is deleted' {
        $victim = Join-Path $TestDrive 'victim4'; New-Item -ItemType Directory $victim | Out-Null
        Set-Content (Join-Path $victim 'keep.txt') 'x'
        # Folders are listed first, by name: "health" would be emptied before "logs\z\old" is reached.
        $dir = Join-Path $TestDrive 'deep\CellGlide'; New-Item -ItemType Directory (Join-Path $dir 'health'), (Join-Path $dir 'logs\z') | Out-Null
        Set-Content (Join-Path $dir 'health\a.jsonl') '{}'; Set-Content (Join-Path $dir 'settings.json') '{}'
        New-Item -ItemType Junction -Path (Join-Path $dir 'logs\z\old') -Target $victim | Out-Null
        try { { Remove-DataDir $dir } | Should -Throw '*logs\z\old is a junction or symbolic link*' }
        finally { [IO.Directory]::Delete((Join-Path $dir 'logs\z\old')) }
        Test-Path (Join-Path $dir 'health\a.jsonl') | Should -BeTrue
        Test-Path (Join-Path $dir 'settings.json') | Should -BeTrue
        Test-Path (Join-Path $victim 'keep.txt') | Should -BeTrue
    }
    It 'Purge refuses a data folder that is itself a junction' {
        $victim = Join-Path $TestDrive 'victim2'; New-Item -ItemType Directory $victim | Out-Null
        Set-Content (Join-Path $victim 'keep.txt') 'x'
        $dir = Join-Path $TestDrive 'linked-CellGlide'
        New-Item -ItemType Junction -Path $dir -Target $victim | Out-Null
        try { { Remove-DataDir $dir } | Should -Throw '*junction or symbolic link*' }
        finally { [IO.Directory]::Delete($dir) }
        Test-Path (Join-Path $victim 'keep.txt') | Should -BeTrue
    }
    It 'Purge removes a plain data folder, read-only files included, and a missing one is fine' {
        Mock Write-Host {}
        $dir = Join-Path $TestDrive 'plain\CellGlide'; New-Item -ItemType Directory (Join-Path $dir 'logs\deep') | Out-Null
        Set-Content (Join-Path $dir 'settings.json') '{}'; Set-Content (Join-Path $dir 'logs\deep\a.log') 'x'
        (Get-Item (Join-Path $dir 'settings.json')).IsReadOnly = $true
        Remove-DataDir $dir
        Test-Path $dir | Should -BeFalse
        Test-Path (Join-Path $TestDrive 'plain') | Should -BeTrue
        { Remove-DataDir (Join-Path $TestDrive 'no-such\CellGlide') } | Should -Not -Throw
    }
    It 'Only profile folders under the profiles root are purged' {
        Test-ProfileDataPath 'C:\Users\alice\AppData\Local\CellGlide' 'C:\Users' | Should -BeTrue
        Test-ProfileDataPath 'c:\users\BOB\AppData\Local\CellGlide' 'C:\Users' | Should -BeTrue
        Test-ProfileDataPath 'C:\Windows\System32\config\systemprofile\AppData\Local\CellGlide' 'C:\Users' | Should -BeFalse
        Test-ProfileDataPath 'C:\UsersX\eve\AppData\Local\CellGlide' 'C:\Users' | Should -BeFalse
        Test-ProfileDataPath 'C:\Users\..\Windows\AppData\Local\CellGlide' 'C:\Users' | Should -BeFalse
    }
    It 'a profiles root that is a drive root, relative or empty purges nothing' {
        Test-ProfileDataPath 'C:\Windows\AppData\Local\CellGlide' 'C:\' | Should -BeFalse
        Test-ProfileDataPath 'C:\Users\a\AppData\Local\CellGlide' '' | Should -BeFalse
        Test-ProfileDataPath 'C:\Users\a\AppData\Local\CellGlide' 'Users' | Should -BeFalse
        Test-ProfileDataPath 'Users\a\AppData\Local\CellGlide' 'C:\Users' | Should -BeFalse
        Test-ProfileDataPath 'C:\Users' 'C:\Users' | Should -BeFalse
        Test-ProfileDataPath 'C:\Users\' 'C:\Users\' | Should -BeFalse
    }
    It 'Remove-OwnUserData removes only a folder under the profiles root, and a missing one is fine' {
        Mock Write-Host {}
        $root = Join-Path $TestDrive 'Users'
        $dir = Join-Path $root 'me\AppData\Local\CellGlide'; New-Item -ItemType Directory (Join-Path $dir 'x') | Out-Null
        Set-Content (Join-Path $dir 'speedtests.json') '[]'
        Remove-OwnUserData $dir $root
        Test-Path $dir | Should -BeFalse
        Test-Path (Join-Path $root 'me\AppData\Local') | Should -BeTrue
        { Remove-OwnUserData (Join-Path $root 'gone\AppData\Local\CellGlide') $root } | Should -Not -Throw
    }
    It 'Remove-OwnUserData refuses a folder outside the profiles root and never follows a junction' {
        Mock Write-Host {}
        $root = Join-Path $TestDrive 'Users3'; $null = New-Item -ItemType Directory $root
        $outside = Join-Path $TestDrive 'Elsewhere\CellGlide'; New-Item -ItemType Directory $outside | Out-Null; Set-Content (Join-Path $outside 'k.txt') 'x'
        { Remove-OwnUserData $outside $root } | Should -Throw '*not under the profiles folder*'
        Test-Path (Join-Path $outside 'k.txt') | Should -BeTrue
        $victim = Join-Path $TestDrive 'victim5'; New-Item -ItemType Directory $victim | Out-Null; Set-Content (Join-Path $victim 'keep.txt') 'x'
        $dir = Join-Path $root 'me2\AppData\Local\CellGlide'; New-Item -ItemType Directory $dir | Out-Null
        New-Item -ItemType Junction -Path (Join-Path $dir 'j') -Target $victim | Out-Null
        try { { Remove-OwnUserData $dir $root } | Should -Throw '*junction or symbolic link*' }
        finally { [IO.Directory]::Delete((Join-Path $dir 'j')) }
        Test-Path (Join-Path $victim 'keep.txt') | Should -BeTrue
    }
    It 'Remove-TreeNoFollow refuses a junction swapped in mid-tree and deletes nothing beyond it' {
        # Reaches Remove-TreeNoFollow's own per-item check directly (not Remove-DataDir's up-front scan).
        $victim = Join-Path $TestDrive 'victim6'; New-Item -ItemType Directory $victim | Out-Null; Set-Content (Join-Path $victim 'keep.txt') 'x'
        $dir = Join-Path $TestDrive 'tree\CellGlide'; New-Item -ItemType Directory (Join-Path $dir 'logs') | Out-Null
        Set-Content (Join-Path $dir 'logs\a.log') 'x'
        New-Item -ItemType Junction -Path (Join-Path $dir 'logs\link') -Target $victim | Out-Null
        try { { Remove-TreeNoFollow $dir } | Should -Throw '*logs\link is a junction or symbolic link*' }
        finally { [IO.Directory]::Delete((Join-Path $dir 'logs\link')) }
        Test-Path (Join-Path $victim 'keep.txt') | Should -BeTrue
    }
    It 'Remove-TreeNoFollow refuses a file symbolic link in the tree (its per-item check)' -Tag 'Elevated' {
        # A file symlink has PSIsContainer false, so only Remove-TreeNoFollow's own per-entry
        # check (not the recursion's top check) can stop it before [IO.File]::Delete.
        $target = Join-Path $TestDrive 'target.txt'; Set-Content $target 'secret'
        $dir = Join-Path $TestDrive 'tree2\CellGlide'; New-Item -ItemType Directory $dir | Out-Null
        Set-Content (Join-Path $dir 'settings.json') '{}'
        New-Item -ItemType SymbolicLink -Path (Join-Path $dir 'slink') -Target $target | Out-Null
        try { { Remove-TreeNoFollow $dir } | Should -Throw '*slink is a junction or symbolic link*' }
        finally { Remove-Item -LiteralPath (Join-Path $dir 'slink') -Force -ErrorAction SilentlyContinue }
        (Get-Item $target).Length | Should -BeGreaterThan 0
    }
    Context 'the ProgramData purge checks owners and ACLs of the whole tree' -Tag 'Elevated' {
        BeforeAll {
            # Gives $Path a protected ACL: SYSTEM and Administrators full control, plus $Extra
            # (sid = rights), owned by $Owner. Setting Administrators or oneself as owner needs no
            # privilege beyond an elevated token.
            $script:SetSecurity = { param([string]$Path, [string]$Owner, [hashtable]$Extra = @{})
                $isDir = (Get-Item -LiteralPath $Path -Force).PSIsContainer
                $sec = if ($isDir) { New-Object Security.AccessControl.DirectorySecurity } else { New-Object Security.AccessControl.FileSecurity }
                $sec.SetAccessRuleProtection($true, $false)
                $inherit = if ($isDir) { [Security.AccessControl.InheritanceFlags]'ContainerInherit, ObjectInherit' } else { [Security.AccessControl.InheritanceFlags]::None }
                $grants = @{ 'S-1-5-18' = 'FullControl'; 'S-1-5-32-544' = 'FullControl' } + $Extra
                foreach ($sid in $grants.Keys) {
                    $sec.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule((New-Object Security.Principal.SecurityIdentifier($sid)), $grants[$sid], $inherit, 'None', 'Allow')))
                }
                $sec.SetOwner((New-Object Security.Principal.SecurityIdentifier($Owner)))
                Set-Acl -LiteralPath $Path -AclObject $sec }
            $script:Me = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
            $script:NewTree = { $d = Join-Path $TestDrive ('pd-' + [guid]::NewGuid().ToString('N') + '\CellGlide')
                $null = New-Item -ItemType Directory (Join-Path $d 'health'); Set-Content (Join-Path $d 'settings.json') '{}'; Set-Content (Join-Path $d 'health\a.jsonl') '{}'
                foreach ($p in $d, (Join-Path $d 'health'), (Join-Path $d 'settings.json'), (Join-Path $d 'health\a.jsonl')) { & $script:SetSecurity $p 'S-1-5-32-544' }
                $d }
        }
        It 'an admin-owned tree with admin-only write access may be purged; read-only Users is fine' {
            $d = & $script:NewTree
            Test-DataDirAdminOnly $d | Should -BeTrue
            & $script:SetSecurity (Join-Path $d 'health') 'S-1-5-32-544' @{ 'S-1-5-32-545' = 'ReadAndExecute' }
            Test-DataDirAdminOnly $d | Should -BeTrue
        }
        It 'Users with write access anywhere refuses it' {
            $d = & $script:NewTree
            & $script:SetSecurity $d 'S-1-5-32-544' @{ 'S-1-5-32-545' = 'Modify' }
            Test-DataDirAdminOnly $d | Should -BeFalse
            $d = & $script:NewTree
            & $script:SetSecurity (Join-Path $d 'health\a.jsonl') 'S-1-5-32-544' @{ 'S-1-5-32-545' = 'Write' }
            Test-DataDirAdminOnly $d | Should -BeFalse
        }
        It 'a root owned by a standard user refuses it, even with an admin-only ACL' {
            $d = & $script:NewTree
            & $script:SetSecurity $d $script:Me
            Test-DataDirAdminOnly $d | Should -BeFalse
        }
        It 'a descendant owned by a standard user refuses it' {
            $d = & $script:NewTree
            & $script:SetSecurity (Join-Path $d 'health\a.jsonl') $script:Me
            Test-DataDirAdminOnly $d | Should -BeFalse
        }
        It 'a link anywhere in the tree refuses it' {
            $d = & $script:NewTree
            $outside = Join-Path $TestDrive ('o-' + [guid]::NewGuid().ToString('N')); $null = New-Item -ItemType Directory $outside
            $null = New-Item -ItemType Junction -Path (Join-Path $d 'health\j') -Target $outside
            try { Test-DataDirAdminOnly $d | Should -BeFalse }
            finally { [IO.Directory]::Delete((Join-Path $d 'health\j')) }
        }
    }
    It 'never loads another user''s registry hive' {
        $calls = $script:Ast.FindAll({ param($n) $n -is [Management.Automation.Language.CommandAst] }, $true) | ForEach-Object { $_.Extent.Text }
        ($calls | Where-Object { $_ -match '^\s*reg(\.exe)?\s+load' }) | Should -BeNullOrEmpty
        ($calls | Where-Object { $_ -match 'RegLoadKey|Invoke-Native\s+.reg' }) | Should -BeNullOrEmpty
    }
    It 'the hives are HKCU and the signed-in users'' hives that are already loaded' {
        $hives = @(Get-LoadedUserHives)
        $hives[0] | Should -Be 'HKCU:'
        foreach ($h in $hives | Select-Object -Skip 1) { $h | Should -Match '\ARegistry::HKEY_USERS\\S-1-5-21-[0-9-]+\z' }
        $own = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
        $hives | Should -Not -Contain "Registry::HKEY_USERS\$own"         # that is HKCU already
        ($hives -join '|') | Should -Not -Match '_Classes'
    }
    It 'removes only a CellGlide Run value that points at our exe, and never StartupApproved' {
        Mock Write-Host {}
        $mine = & $script:NewTempKey; $other = & $script:NewTempKey
        try {
            $run = { param($hive) $k = "$hive\Software\Microsoft\Windows\CurrentVersion\Run"; $null = New-Item -Path $k -Force; $k }
            $r1 = & $run $mine; $r2 = & $run $other
            $approved = "$mine\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"; $null = New-Item -Path $approved -Force
            $null = New-ItemProperty -LiteralPath $approved -Name CellGlide -Value ([byte[]](3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)) -PropertyType Binary
            $null = New-ItemProperty -LiteralPath $r1 -Name CellGlide -Value "`"$script:Exe`" --tray"
            $null = New-ItemProperty -LiteralPath $r1 -Name Other -Value "`"$script:Exe`" --tray"
            $null = New-ItemProperty -LiteralPath $r2 -Name CellGlide -Value '"D:\dev\CellGlide.exe" --tray'
            Remove-RunValues $script:Exe -Hives @($mine, $other, 'HKCU:\Software\CellGlideTest-missing')
            (Get-Item -LiteralPath $r1).GetValueNames() | Should -Not -Contain 'CellGlide'
            (Get-Item -LiteralPath $r1).GetValue('Other') | Should -Be "`"$script:Exe`" --tray"
            (Get-Item -LiteralPath $r2).GetValue('CellGlide') | Should -Be '"D:\dev\CellGlide.exe" --tray'
            (Get-Item -LiteralPath $approved).GetValue('CellGlide')[0] | Should -Be 3
            Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'Users who are not signed in keep a CellGlide startup entry that points to the removed program; it does nothing.' }
        } finally { Remove-Item -LiteralPath $mine, $other -Recurse -Force }
    }
    It 'removes only our GUID''s interface keys, and none while cellglide0 is still there' {
        Mock Write-Host {}
        $base = & $script:NewTempKey
        try {
            foreach ($stack in 'Tcpip', 'Tcpip6') {
                foreach ($g in $AdapterGuid, '{11111111-2222-3333-4444-555555555555}') { $null = New-Item -Path "$base\$stack\Parameters\Interfaces\$g" -Force }
            }
            Mock Get-OurAdapter { [pscustomobject]@{ Name = 'cellglide0' } }
            { Remove-InterfaceKeys $base } | Should -Throw '*still present*'
            Test-Path "$base\Tcpip\Parameters\Interfaces\$AdapterGuid" | Should -BeTrue
            Mock Get-OurAdapter { }
            Remove-InterfaceKeys $base
            foreach ($stack in 'Tcpip', 'Tcpip6') {
                Test-Path "$base\$stack\Parameters\Interfaces\$AdapterGuid" | Should -BeFalse
                Test-Path "$base\$stack\Parameters\Interfaces\{11111111-2222-3333-4444-555555555555}" | Should -BeTrue
            }
        } finally { Remove-Item -LiteralPath $base -Recurse -Force }
    }
    It 'removes the program folder, a leftover .new and .old, and old work folders, never through a link' {
        Mock Write-Host {}
        $root = Join-Path $TestDrive ('pf-' + [guid]::NewGuid().ToString('N')); $null = New-Item -ItemType Directory $root
        $InstallDir = Join-Path $root 'CellGlide'
        foreach ($d in $InstallDir, "$InstallDir.new", "$InstallDir.work-aa", "$InstallDir.work-own", (Join-Path $root 'CellGlideX')) {
            $null = New-Item -ItemType Directory (Join-Path $d 'app'); Set-Content (Join-Path $d 'app\f.txt') 'x'
        }
        $outside = Join-Path $TestDrive ('outside-' + [guid]::NewGuid().ToString('N')); $null = New-Item -ItemType Directory $outside; Set-Content (Join-Path $outside 'keep.txt') 'x'
        $null = New-Item -ItemType Directory "$InstallDir.old"; $null = New-Item -ItemType Junction -Path "$InstallDir.old\j" -Target $outside
        try {
            { Remove-ProgramDirs "$InstallDir.work-own" } | Should -Throw '*CellGlide.old*junction or symbolic link*'
            foreach ($gone in $InstallDir, "$InstallDir.new", "$InstallDir.work-aa") { Test-Path $gone | Should -BeFalse -Because $gone }
            foreach ($kept in "$InstallDir.old", "$InstallDir.work-own", (Join-Path $root 'CellGlideX'), (Join-Path $outside 'keep.txt')) { Test-Path $kept | Should -BeTrue -Because $kept }
        } finally { [IO.Directory]::Delete("$InstallDir.old\j") }
        Remove-ProgramDirs "$InstallDir.work-own"
        Test-Path "$InstallDir.old" | Should -BeFalse
    }
    Context 'the uninstall steps' {
        BeforeEach {
            Mock Write-Host {}
            $script:calls = New-Object System.Collections.Generic.List[string]
            foreach ($n in 'Stop-CellGlideProcesses', 'Remove-ServiceEntry', 'Remove-Shortcut', 'Remove-UninstallEntry', 'Remove-InterfaceKeys', 'Remove-PhantomDevice') {
                Mock $n ([scriptblock]::Create("`$script:calls.Add('$n')"))
            }
            Mock Stop-OurService { $script:calls.Add('Stop-OurService'); $true }
            Mock Remove-RunValues { $script:calls.Add("Remove-RunValues $ExePath") }
            Mock Remove-ProgramDirs { $script:calls.Add("Remove-ProgramDirs $Keep") }
            Mock Remove-DataDir { $script:calls.Add("Remove-DataDir $Dir") }
            Mock Test-DataDirAdminOnly { $true }
            Mock Remove-OwnUserData { $script:calls.Add('Remove-OwnUserData') }   # never called from the elevated part
        }
        It 'the elevated part purges ProgramData only, after the interface keys and the device, and never the user data' {
            Invoke-UninstallPart -Purge -Keep 'C:\Program Files\CellGlide.work-x' | Should -Be 0
            $script:calls | Should -Be @('Stop-CellGlideProcesses', 'Stop-OurService', 'Remove-ServiceEntry',
                "Remove-RunValues $script:Exe", 'Remove-Shortcut', 'Remove-UninstallEntry', 'Remove-ProgramDirs C:\Program Files\CellGlide.work-x',
                'Remove-InterfaceKeys', 'Remove-PhantomDevice', "Remove-DataDir $env:ProgramData\CellGlide")
            $script:calls | Should -Not -Contain 'Remove-OwnUserData'
            Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'CellGlide uninstalled.' }
        }
        It 'the ProgramData purge is refused when its ACL lets a non-administrator write' {
            Mock Test-DataDirAdminOnly { $false }
            Mock Test-Path { $true } -ParameterFilter { $LiteralPath -eq "$env:ProgramData\CellGlide" }
            Invoke-UninstallPart -Purge | Should -Be 1
            ($script:calls | Where-Object { $_ -like 'Remove-DataDir*' }) | Should -BeNullOrEmpty
            Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'data directory failed:*non-administrator*' }
        }
        It 'without -Purge the ProgramData folder is kept and said so, and nothing is purged' {
            Mock Test-Path { $true } -ParameterFilter { $LiteralPath -eq "$env:ProgramData\CellGlide" }
            Invoke-UninstallPart | Should -Be 0
            ($script:calls | Where-Object { $_ -like 'Remove-DataDir*' }) | Should -BeNullOrEmpty
            Should -Invoke Write-Host -ParameterFilter { "$Object" -like "Kept $env:ProgramData\CellGlide*-Purge*" }
        }
        It 'a failed step is reported, the rest still runs, and the result is 1' {
            Mock Remove-ProgramDirs { $script:calls.Add('Remove-ProgramDirs'); throw 'locked' }
            Mock Stop-OurService { $script:calls.Add('Stop-OurService'); $false }
            Invoke-UninstallPart -Purge | Should -Be 1
            $script:calls | Should -Contain 'Remove-InterfaceKeys'
            $script:calls | Should -Contain "Remove-DataDir $env:ProgramData\CellGlide"
            Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'Uninstall finished with failures: stop service, program files' }
        }
    }
    Context 'where the uninstall runs' {
        BeforeEach {
            Mock Write-Host {}
            Mock Invoke-UninstallPart { $script:part = @{ Purge = [bool]$Purge; Keep = $Keep }; 0 }
            Mock Remove-OwnUserData { $script:ownData = $true }
            # The test shell's own token (elevated on CI) must not decide; the cases below say.
            Mock Test-TokenElevated { $false }
            $script:part = $null; $script:ownData = $false
        }
        It 'an elevated token that is no administrator''s, or one that cannot be read, never deletes the user''s data' {
            Mock Test-Elevated { $false }
            Mock Invoke-UninstallChild { 0 }
            foreach ($token in { $true }, { throw 'no token' }) {
                Mock Test-TokenElevated $token
                Invoke-CellGlideUninstall -Purge -ScriptPath 'C:\Temp\uninstall.ps1' | Should -Be 0
                $script:ownData | Should -BeFalse
            }
        }
        It 'an ordinary window hands the work to an elevated child, then removes the user''s own data with -Purge' {
            Mock Test-Elevated { $false }
            Mock Invoke-UninstallChild { $script:child = @{ Text = $ScriptText; Args = $Arguments }; 0 }
            $CellGlideUninstallText = '# own'
            Invoke-CellGlideUninstall -Purge -ScriptPath 'C:\Temp\uninstall.ps1' | Should -Be 0
            $script:child.Text | Should -Be '# own'
            $script:child.Args | Should -Be @('-Purge')
            $script:part | Should -BeNullOrEmpty                 # the elevated part runs in the child, not here
            $script:ownData | Should -BeTrue                     # the unelevated parent removes the user's own data
        }
        It 'a failed elevated part keeps the user''s data' {
            Mock Test-Elevated { $false }
            Mock Invoke-UninstallChild { 4 }
            Invoke-CellGlideUninstall -Purge -ScriptPath 'C:\Temp\uninstall.ps1' | Should -Be 4
            $script:ownData | Should -BeFalse
        }
        Context 'the real elevation path, with the UAC prompt replaced' {
            BeforeEach { Mock Test-Elevated { $false } }
            It 'a declined UAC prompt deletes nothing of the user''s' {
                Mock Start-Process { throw 'The operation was canceled by the user.' }
                Invoke-CellGlideUninstall -Purge -ScriptPath 'C:\Temp\uninstall.ps1' | Should -Be 1
                Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'Uninstall cancelled.' }
                $script:ownData | Should -BeFalse
            }
            It 'an elevated child that fails deletes nothing of the user''s' {
                Mock Start-Process { & $script:FakeProcess 2 }
                Invoke-CellGlideUninstall -Purge -ScriptPath 'C:\Temp\uninstall.ps1' | Should -Be 2
                $script:ownData | Should -BeFalse
            }
            It 'a changed script (exit 3) deletes nothing of the user''s' {
                Mock Start-Process { & $script:FakeProcess 3 }
                Invoke-CellGlideUninstall -Purge -ScriptPath 'C:\Temp\uninstall.ps1' | Should -Be 1
                Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'Uninstall failed: the copy of the uninstaller in %TEMP% changed*' }
                $script:ownData | Should -BeFalse
            }
            It 'a successful elevated child is followed by the user''s own data' {
                Mock Start-Process { & $script:FakeProcess 0 }
                Invoke-CellGlideUninstall -Purge -ScriptPath 'C:\Temp\uninstall.ps1' | Should -Be 0
                $script:ownData | Should -BeTrue
            }
        }
        It 'without -Purge the user data is kept, said so, and not removed' {
            Mock Test-Elevated { $false }
            Mock Invoke-UninstallChild { 0 }
            Invoke-CellGlideUninstall -ScriptPath 'C:\Temp\uninstall.ps1' | Should -Be 0
            $script:ownData | Should -BeFalse
            Should -Invoke Write-Host -ParameterFilter { "$Object" -like "Kept $env:LOCALAPPDATA\CellGlide*-Purge*" }
        }
        It 'a user-data failure after a clean elevated part makes the result 1' {
            Mock Test-Elevated { $false }
            Mock Invoke-UninstallChild { 0 }
            Mock Remove-OwnUserData { throw 'locked' }
            Invoke-CellGlideUninstall -Purge -ScriptPath 'C:\Temp\uninstall.ps1' | Should -Be 1
            Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'user data failed:*' }
        }
        It 'an administrator window uninstalls in the same process but never deletes the user''s data, and says so in both languages' {
            Mock Test-Elevated { $true }
            Mock Invoke-UninstallChild { throw 'not reached' }
            Mock Test-Path { $true } -ParameterFilter { $LiteralPath -eq (Join-Path $env:LOCALAPPDATA 'CellGlide') }
            Invoke-CellGlideUninstall -Purge -ScriptPath 'C:\Temp\uninstall.ps1' | Should -Be 0
            $script:part.Purge | Should -BeTrue                  # ProgramData is still purged by the elevated part
            $script:ownData | Should -BeFalse
            Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'Not removed: *CellGlide (an administrator window never deletes*normal (not administrator) window*-Purge*' }
            $zh = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($UserDataSkippedZh)) -f (Join-Path $env:LOCALAPPDATA 'CellGlide')
            $zh | Should -Match '-Purge'
            [int][char]$zh[0] | Should -Be 0x672A
            Should -Invoke Write-Host -ParameterFilter { "$Object" -ceq $zh }
        }
        It 'iex in an administrator window (no script path) never deletes the user''s data either' {
            Mock Test-Elevated { $true }
            Invoke-CellGlideUninstall -Purge | Should -Be 0
            $script:ownData | Should -BeFalse
        }
        It 'the copy in the install folder relocates even in an administrator window' {
            Mock Test-Elevated { $true }
            Mock Invoke-UninstallChild { $script:child = @{ Args = @($Arguments) }; 0 }
            Invoke-CellGlideUninstall -ScriptPath 'C:\Program Files\CellGlide\uninstall.ps1' | Should -Be 0
            $script:child.Args.Count | Should -Be 0
            $script:part | Should -BeNullOrEmpty
        }
        It 'the elevated child keeps its log and its own work folder private, and hands the log to the parent' {
            $work = Join-Path $TestDrive ('CellGlide.work-' + [guid]::NewGuid().ToString('N')); $null = New-Item -ItemType Directory $work
            Mock Test-Elevated { $true }
            Mock New-PrivateWorkDir { $work }
            Mock Start-Transcript { $script:transcript = $LiteralPath }
            Mock Stop-Transcript { }
            Mock Copy-LogToParent { $script:copied = @($Source, $Dest) }
            Invoke-CellGlideUninstall -Purge -ElevatedPart -LogPath 'C:\t\cellglide-uninstall-x.log' | Should -Be 0
            $script:part.Purge | Should -BeTrue
            $script:part.Keep | Should -Be $work
            $script:transcript | Should -Be (Join-Path $work 'uninstall.log')
            $script:copied | Should -Be @((Join-Path $work 'uninstall.log'), 'C:\t\cellglide-uninstall-x.log')
            Test-Path $work | Should -BeFalse
        }
        It 'an elevated part without administrator rights changes nothing' {
            Mock Test-Elevated { $false }
            Mock New-PrivateWorkDir { throw 'not reached' }
            Invoke-CellGlideUninstall -ElevatedPart | Should -Be 1
            $script:part | Should -BeNullOrEmpty
        }
    }
    It 'A script inside the install directory relocates before it runs' {
        Test-NeedsRelocation 'C:\Program Files\CellGlide\uninstall.ps1' 'C:\Program Files\CellGlide' $true | Should -BeTrue
        Test-NeedsRelocation 'C:\Temp\uninstall.ps1' 'C:\Program Files\CellGlide' $false | Should -BeTrue
        Test-NeedsRelocation 'C:\Temp\uninstall.ps1' 'C:\Program Files\CellGlide' $true | Should -BeFalse
        Test-NeedsRelocation '' 'C:\Program Files\CellGlide' $true | Should -BeFalse          # iex, already elevated
        Test-NeedsRelocation 'C:\Program Files\CellGlideX\uninstall.ps1' 'C:\Program Files\CellGlide' $true | Should -BeFalse
        Mock Write-Host {}
        # The child gets the bootstrap (decision 4 / Task 5): the script text in %TEMP%, checked by hash.
        $start = { param($EncodedCommand, $Info) $script:ranText = [IO.File]::ReadAllText($Info.ScriptPath); $script:ranPath = $Info.ScriptPath
            Set-Content $Info.LogPath 'removed'
            $script:bootstrap = [Text.Encoding]::Unicode.GetString([Convert]::FromBase64String($EncodedCommand))
            & $script:FakeProcess 0 }
        Invoke-UninstallChild "# own text`n" @('-Purge') $start | Should -Be 0
        $script:ranText | Should -Be "# own text`n"
        $script:ranPath | Should -BeLike "$([IO.Path]::GetTempPath())cellglide-uninstall-*.ps1"
        Test-Path $script:ranPath | Should -BeFalse
        Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'removed' }
        $blob = [regex]::Match($script:bootstrap, "FromBase64String\('([A-Za-z0-9+/=]+)'\)").Groups[1].Value
        $config = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($blob)) | ConvertFrom-Json
        $config.Parameters.Purge | Should -BeTrue
        $config.Parameters.ElevatedPart | Should -BeTrue
        $config.Parameters.LogPath | Should -Match '\\cellglide-uninstall-[0-9a-f]{32}\.log$'
    }
    It 'Get-CanonicalPath resolves a junction and keeps a missing path' {
        $real = Join-Path $TestDrive 'Canon Target'; New-Item -ItemType Directory $real | Out-Null
        $link = Join-Path $TestDrive 'canonlink'; New-Item -ItemType Junction -Path $link -Target $real | Out-Null
        try { Get-CanonicalPath $link | Should -Be (Get-CanonicalPath $real) }
        finally { [IO.Directory]::Delete($link) }
        Get-CanonicalPath (Join-Path $TestDrive 'does-not-exist-xyz') | Should -Be (Join-Path $TestDrive 'does-not-exist-xyz')
        Get-CanonicalPath '' | Should -Be ''
    }
    It 'Test-NeedsRelocation follows a link into the install folder, not just the text' {
        $real = Join-Path $TestDrive 'realinstall'; New-Item -ItemType Directory $real | Out-Null
        Set-Content (Join-Path $real 'uninstall.ps1') '# x'
        $link = Join-Path $TestDrive 'linkinstall'; New-Item -ItemType Junction -Path $link -Target $real | Out-Null
        try {
            Test-NeedsRelocation (Join-Path $link 'uninstall.ps1') $real $true | Should -BeTrue    # reached through the link
            Test-NeedsRelocation (Join-Path $real 'uninstall.ps1') $real $true | Should -BeTrue
        } finally { [IO.Directory]::Delete($link) }
    }
    It 'the parent''s %TEMP% cleanup deletes only a plain single-link file, never through a link' {
        $plain = Join-Path $TestDrive 'cellglide-uninstall-plain.log'; Set-Content $plain 'x'
        Remove-OwnTempFile $plain | Should -BeTrue
        Test-Path $plain | Should -BeFalse
        $victim = Join-Path $TestDrive 'victim-hard.txt'; Set-Content $victim 'keep'
        $hard = Join-Path $TestDrive 'cellglide-uninstall-hard.log'; $null = New-Item -ItemType HardLink -Path $hard -Target $victim
        Remove-OwnTempFile $hard | Should -BeFalse                      # two links: left alone
        Test-Path $hard | Should -BeTrue
        Remove-OwnTempFile (Join-Path $TestDrive 'missing.log') | Should -BeFalse
    }
    It 'the parent''s %TEMP% cleanup never deletes through a symbolic link' -Tag 'Elevated' {
        $victim = Join-Path $TestDrive 'victim-sym.txt'; Set-Content $victim 'keep'
        $link = Join-Path $TestDrive 'cellglide-uninstall-sym.log'; $null = New-Item -ItemType SymbolicLink -Path $link -Target $victim
        Remove-OwnTempFile $link | Should -BeFalse
        Get-Content $victim | Should -Be 'keep'
        Remove-Item -LiteralPath $link -Force
    }
    It 'a cancelled UAC prompt is a cancelled uninstall, and unknown arguments are refused' {
        Mock Write-Host {}
        Invoke-UninstallChild "# x`n" @() { param($EncodedCommand, $Info) throw 'The operation was canceled by the user.' } | Should -Be 1
        Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'Uninstall cancelled.' }
        { Invoke-UninstallChild "# x`n" @('-Force') { throw 'not reached' } } | Should -Throw '*-Force*'
        { Invoke-UninstallChild '' @() { throw 'not reached' } } | Should -Throw "*Cannot find the running script's text*"
    }
    Context 'the bootstrap run for real in a separate (unelevated) PowerShell' {
        BeforeAll {
            $script:runReal = { param($EncodedCommand, $Info)
                if ($script:tamper) { Add-Content -LiteralPath $Info.ScriptPath "`n[IO.File]::WriteAllText(`$LogPath, 'tampered')" }
                $p = Start-Process powershell.exe -PassThru -WindowStyle Hidden -ArgumentList "-NoProfile -ExecutionPolicy Bypass -EncodedCommand $EncodedCommand"
                $null = $p.Handle
                $p }
            # uninstall.ps1's param block, capture, iex guard and footer, with a stand-in main.
            $stmts = $script:Ast.EndBlock.Statements
            $guard = ($stmts | Where-Object { $_ -is [Management.Automation.Language.IfStatementAst] } | Select-Object -First 1).Extent.Text
            $capture = ($stmts | Where-Object { $_.Extent.Text -like '$CellGlideUninstallText =*' }).Extent.Text
            $footer = ($stmts | Select-Object -Last 2 | ForEach-Object { $_.Extent.Text }) -join "`n"
            $script:childText = $script:Ast.ParamBlock.Extent.Text + "`n" + $capture + "`n" + $guard + "`n" +
                'function Invoke-CellGlideUninstall { param([switch]$Purge, [switch]$ElevatedPart, $LogPath, $ScriptPath) [IO.File]::WriteAllText($LogPath, "main $Purge $ElevatedPart [$ScriptPath]"); 6 }' + "`n" + $footer
        }
        BeforeEach { Mock Write-Host {}; $script:tamper = $false }
        It 'uninstall.ps1''s own guard and footer run in the bootstrap and give it the exit code' {
            Invoke-UninstallChild $script:childText @('-Purge') $script:runReal | Should -Be 6
            Should -Invoke Write-Host -ParameterFilter { "$Object" -eq 'main True True []' }
        }
        It 'a script changed after the parent wrote it is not run' {
            $script:tamper = $true
            Invoke-UninstallChild $script:childText @() $script:runReal | Should -Be 1
            Should -Invoke Write-Host -ParameterFilter { "$Object" -like 'Uninstall failed: the copy of the uninstaller in %TEMP% changed*' }
            Should -Invoke Write-Host -Times 0 -Exactly -ParameterFilter { "$Object" -like 'main*' -or "$Object" -eq 'tampered' }
        }
    }
    It 'iex leaves no functions or variables behind, and the caller''s own variables keep their values' {
        $stmts = $script:Ast.EndBlock.Statements
        $guard = ($stmts | Where-Object { $_ -is [Management.Automation.Language.IfStatementAst] } | Select-Object -First 1).Extent.Text
        $capture = ($stmts | Where-Object { $_.Extent.Text -like '$CellGlideUninstallText =*' }).Extent.Text
        $text = $script:Ast.ParamBlock.Extent.Text + "`n" + $capture + "`n" + $guard + "`n" +
            "function CgLeak { }`n`$cgLeak = 1`n[Console]::Out.WriteLine('body ran')"
        $iex = "iex ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$([Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($text)))')))"
        $names = "'cgLeak', 'CellGlideUninstallText', 'Purge', 'ElevatedPart', 'LogPath', '__CellGlideCallerVars', 'keep', 'name', 's', 'v', 'a', 'Saved', 'Names'"
        $quiet = "`$ProgressPreference = 'SilentlyContinue'`n"       # no "Preparing modules for first use" CLIXML on stderr
        $report = "[Console]::Out.WriteLine('left: ' + (@('CgLeak') | Where-Object { Get-Command `$_ -ErrorAction SilentlyContinue }) + '|' + (@($names) | Where-Object { Get-Variable `$_ -ErrorAction SilentlyContinue }))"
        $out = @(& powershell.exe -NoProfile -EncodedCommand ([Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes("$quiet$iex`n$report"))))
        $out | Should -Contain 'body ran'
        $out | Should -Contain 'left: |'
        $pre = "`$Purge = 'mine'; `$LogPath = 42; `$CellGlideUninstallText = 'own text'; `$ElevatedPart = 'ep'"
        $check = "[Console]::Out.WriteLine('vals: ' + `$Purge + '|' + `$LogPath + '|' + `$LogPath.GetType().Name + '|' + `$CellGlideUninstallText + '|' + `$ElevatedPart + '|' + [bool](Get-Variable __CellGlideCallerVars -ErrorAction SilentlyContinue) + '|' + [bool](Get-Variable cgLeak -ErrorAction SilentlyContinue))"
        $out = @(& powershell.exe -NoProfile -EncodedCommand ([Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes("$quiet$pre`n$iex`n$check"))) 2>&1 | ForEach-Object { "$_" })
        $out | Should -Contain 'body ran'
        $out | Should -Contain 'vals: mine|42|Int32|own text|ep|False|False'
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
        $functions.Count | Should -BeGreaterThan 25
        foreach ($f in $functions) {
            $set = $f.Body.FindAll({ param($n) $n -is [Management.Automation.Language.AssignmentStatementAst] -and $n.Left.Extent.Text -eq '$ErrorActionPreference' }, $false)
            @($set).Count | Should -BeGreaterThan 0 -Because $f.Name
        }
    }
    It 'is ASCII only' { [IO.File]::ReadAllBytes((Resolve-Path $script:File)) | Where-Object { $_ -gt 0x7F } | Should -BeNullOrEmpty }
}
