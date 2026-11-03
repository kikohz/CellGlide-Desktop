# Pester 5 tests for the data directory ACL step of packaging\windows\install.ps1 (Set-DataDirAcl),
# on TestDrive folders only. Tagged Elevated: setting owners needs an administrator shell.
# (Moved from scripts\tests\test-install-dev-datadir.ps1, same cases and assertions.)
Describe 'Set-DataDirAcl' -Tag 'Elevated' {
    BeforeAll {
        . "$PSScriptRoot\..\..\..\scripts\tests\Import-ScriptFunctions.ps1"; . Import-ScriptFunctions "$PSScriptRoot\..\install.ps1"
        $SidSystem = '*S-1-5-18'
        $SidAdmins = '*S-1-5-32-544'
        $SidUsers = '*S-1-5-32-545'
        # Any existing service gives a valid "NT SERVICE\<name>" account for the grant.
        $ServiceName = 'Dnscache'
        function Owner([string]$path) { return (Get-Acl -LiteralPath $path).GetOwner([Security.Principal.SecurityIdentifier]).Value }
        function Sddl([string]$path) { return (Get-Acl -LiteralPath $path).Sddl }
        # Sets the owner of $path and everything under it (no links in these trees).
        function Set-TreeOwner([string]$path, [string]$sid) {
            foreach ($p in @($path) + @(Get-ChildItem -LiteralPath $path -Recurse -Force | ForEach-Object FullName)) {
                Invoke-Native 'icacls.exe' @($p, '/setowner', "*$sid", '/Q') | Out-Null
            }
        }

        # A folder outside the data directory that a link points at; it must come out untouched.
        $script:victim = Join-Path $TestDrive 'victim'
        New-Item -ItemType Directory -Path $script:victim | Out-Null
        Set-Content -LiteralPath (Join-Path $script:victim 'file.txt') -Value 'x'
        $script:victimSddl = Sddl $script:victim
        $script:victimFileSddl = Sddl (Join-Path $script:victim 'file.txt')
    }
    AfterAll {
        # Junctions first, so removing the tree can't reach through them.
        Get-ChildItem -LiteralPath $TestDrive -Recurse -Force -Attributes ReparsePoint -ErrorAction SilentlyContinue |
            ForEach-Object { [System.IO.Directory]::Delete($_.FullName) }
    }
    It 'a junction inside a pre-existing data directory stops the step' {
        $DataDir = Join-Path $TestDrive 'data1'
        New-Item -ItemType Directory -Path (Join-Path $DataDir 'logs') | Out-Null
        New-Item -ItemType Junction -Path (Join-Path $DataDir 'logs\old') -Target $script:victim | Out-Null
        $err = $null
        try { Set-DataDirAcl } catch { $err = $_.Exception.Message }
        $err | Should -BeLike '*junction or symbolic link*'
        $err | Should -BeLike '*logs\old*'
        Sddl $script:victim | Should -Be $script:victimSddl -Because 'the junction target ACL must not change'
        Sddl (Join-Path $script:victim 'file.txt') | Should -Be $script:victimFileSddl -Because 'a file behind the junction must not change'
    }
    It 'the data directory itself a junction stops the step' {
        $DataDir = Join-Path $TestDrive 'data2'
        New-Item -ItemType Junction -Path $DataDir -Target $script:victim | Out-Null
        $err = $null
        try { Set-DataDirAcl } catch { $err = $_.Exception.Message }
        $err | Should -BeLike '*junction or symbolic link*'
        Sddl $script:victim | Should -Be $script:victimSddl -Because 'the junction target ACL must not change'
    }
    It 'a plain pre-existing tree is taken over' {
        # Every entry owned by Administrators, health\ readable by Users, logs\ not.
        $DataDir = Join-Path $TestDrive 'data3'
        New-Item -ItemType Directory -Path (Join-Path $DataDir 'logs\sub') | Out-Null
        Set-Content -LiteralPath (Join-Path $DataDir 'logs\sub\a.log') -Value 'x'
        # As an earlier install or the service leaves it: owned by Administrators, SYSTEM.
        Set-TreeOwner $DataDir 'S-1-5-32-544'
        Invoke-Native 'icacls.exe' @((Join-Path $DataDir 'logs\sub\a.log'), '/setowner', '*S-1-5-18', '/Q') | Out-Null
        Set-DataDirAcl
        foreach ($p in @($DataDir, (Join-Path $DataDir 'logs'), (Join-Path $DataDir 'logs\sub'),
                         (Join-Path $DataDir 'logs\sub\a.log'), (Join-Path $DataDir 'health'))) {
            Owner $p | Should -Be 'S-1-5-32-544' -Because $p
        }
        Sddl (Join-Path $DataDir 'health') | Should -Match ';;;BU\)' -Because 'health needs the Users entry'
        Sddl (Join-Path $DataDir 'logs') | Should -Not -Match ';;;BU\)' -Because 'logs must not have a Users entry'
        Sddl $DataDir | Should -Not -Match '\(A;[A-Z]*ID;' -Because 'the root must not inherit'
        # Taken over once, the folder is accepted again (a reinstall).
        Set-DataDirAcl
    }
    It 'a pre-existing folder owned by a standard account is refused before any icacls' {
        # As a standard user who pre-created ProgramData\CellGlide leaves it (Users stands in for them).
        $DataDir = Join-Path $TestDrive 'data4'
        New-Item -ItemType Directory -Path (Join-Path $DataDir 'logs') | Out-Null
        Set-TreeOwner $DataDir 'S-1-5-32-544'
        Invoke-Native 'icacls.exe' @($DataDir, '/setowner', '*S-1-5-32-545', '/Q') | Out-Null
        $before = Sddl $DataDir
        Mock Invoke-Native {}
        $err = $null
        try { Set-DataDirAcl } catch { $err = $_.Exception.Message }
        $err | Should -BeLike '*was not created by an administrator*Delete it*'
        Should -Invoke Invoke-Native -Times 0 -Exactly
        Sddl $DataDir | Should -Be $before
        Owner $DataDir | Should -Be 'S-1-5-32-545'
    }
    It 'something inside owned by a standard account is refused too' {
        $DataDir = Join-Path $TestDrive 'data5'
        New-Item -ItemType Directory -Path (Join-Path $DataDir 'logs\planted') | Out-Null
        Set-TreeOwner $DataDir 'S-1-5-32-544'
        Invoke-Native 'icacls.exe' @((Join-Path $DataDir 'logs\planted'), '/setowner', '*S-1-5-32-545', '/Q') | Out-Null
        Mock Invoke-Native {}
        $err = $null
        try { Set-DataDirAcl } catch { $err = $_.Exception.Message }
        $err | Should -BeLike '*was not created by an administrator*'
        Should -Invoke Invoke-Native -Times 0 -Exactly
    }
    It 'the owners-only check ignores the ACL, the full check does not' {
        # Made by an administrator but never locked: ProgramData's Users entries still apply.
        $d = Join-Path $TestDrive 'data6'
        New-Item -ItemType Directory -Path $d | Out-Null
        Set-TreeOwner $d 'S-1-5-32-544'
        Invoke-Native 'icacls.exe' @($d, '/grant', '*S-1-5-32-545:(OI)(CI)W', '/Q') | Out-Null
        Test-DataDirAdminOnly $d -OwnersOnly | Should -BeTrue
        Test-DataDirAdminOnly $d | Should -BeFalse
    }
}
