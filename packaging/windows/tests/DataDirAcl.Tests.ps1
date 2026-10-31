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
        Set-DataDirAcl
        foreach ($p in @($DataDir, (Join-Path $DataDir 'logs'), (Join-Path $DataDir 'logs\sub'),
                         (Join-Path $DataDir 'logs\sub\a.log'), (Join-Path $DataDir 'health'))) {
            Owner $p | Should -Be 'S-1-5-32-544' -Because $p
        }
        Sddl (Join-Path $DataDir 'health') | Should -Match ';;;BU\)' -Because 'health needs the Users entry'
        Sddl (Join-Path $DataDir 'logs') | Should -Not -Match ';;;BU\)' -Because 'logs must not have a Users entry'
        Sddl $DataDir | Should -Not -Match '\(A;[A-Z]*ID;' -Because 'the root must not inherit'
    }
}
