# Pester 5 tests for Install-Files in packaging\windows\install.ps1 (decision 6), on TestDrive only.
BeforeAll {
    . "$PSScriptRoot\..\..\..\scripts\tests\Import-ScriptFunctions.ps1"; . Import-ScriptFunctions "$PSScriptRoot\..\install.ps1"
    function New-Tree([string]$Dir, [string[]]$Files) { $null = New-Item -ItemType Directory -Force $Dir; foreach ($f in $Files) { Set-Content (Join-Path $Dir $f) $f } }
    function Names([string]$Dir) { @(Get-ChildItem -LiteralPath $Dir -Recurse -File | ForEach-Object Name | Sort-Object) }
}
Describe 'Install-Files' {
    BeforeEach {
        $script:target = Join-Path $TestDrive ('CellGlide-' + [guid]::NewGuid().ToString('N')) | Join-Path -ChildPath 'CellGlide'
        $script:source = Join-Path $TestDrive ('src-' + [guid]::NewGuid().ToString('N'))
        New-Tree $script:target @('old.txt'); New-Tree $script:source @('new.txt')
    }
    It 'swaps a new directory in' {
        Install-Files $script:source $script:target
        Names $script:target | Should -Be @('new.txt')
        Test-Path "$($script:target).new" | Should -BeFalse; Test-Path "$($script:target).old" | Should -BeFalse
    }
    It 'A failed swap puts the old directory back' {
        Mock Rename-Item -ParameterFilter { "$Path" -like '*.new' } { throw 'locked' }
        { Install-Files $script:source $script:target } | Should -Throw '*locked*'
        Names $script:target | Should -Be @('old.txt')
        Test-Path "$($script:target).new" | Should -BeFalse; Test-Path "$($script:target).old" | Should -BeFalse
    }
    It 'A leftover .new or .old from an earlier failure is cleared first' {
        New-Tree "$($script:target).new" @('junk-new.txt'); New-Tree "$($script:target).old" @('junk-old.txt')
        Install-Files $script:source $script:target
        Names $script:target | Should -Be @('new.txt')
        Test-Path "$($script:target).new" | Should -BeFalse; Test-Path "$($script:target).old" | Should -BeFalse
    }
    It 'a failed copy leaves the old directory untouched' {
        Mock Copy-Item { $null = New-Item -ItemType Directory $Destination; Set-Content (Join-Path $Destination 'half.txt') 'x'; throw 'disk full' }
        { Install-Files $script:source $script:target } | Should -Throw '*disk full*'
        Names $script:target | Should -Be @('old.txt')
        Test-Path "$($script:target).new" | Should -BeFalse; Test-Path "$($script:target).old" | Should -BeFalse
    }
    It 'a target that cannot be moved aside stays as it was' {
        Mock Rename-Item -ParameterFilter { "$Path" -notlike '*.new' -and "$Path" -notlike '*.old' } { throw 'in use' }
        { Install-Files $script:source $script:target } | Should -Throw '*in use*'
        Names $script:target | Should -Be @('old.txt')
        Test-Path "$($script:target).new" | Should -BeFalse; Test-Path "$($script:target).old" | Should -BeFalse
    }
    It 'an old directory left alone by a run that died between the renames comes back first' {
        Rename-Item $script:target 'CellGlide.old'
        Mock Copy-Item { throw 'disk full' }
        { Install-Files $script:source $script:target } | Should -Throw '*disk full*'
        Names $script:target | Should -Be @('old.txt')
        Test-Path "$($script:target).old" | Should -BeFalse
    }
    It 'installs into a target that does not exist yet' {
        Remove-Item -Recurse -Force $script:target
        Install-Files $script:source $script:target
        Names $script:target | Should -Be @('new.txt')
    }
    It 'nothing from the old version survives in app\' {
        New-Tree (Join-Path $script:target 'app') @('Old.Runtime.dll', 'CellGlide.exe'); New-Tree (Join-Path $script:source 'app') @('CellGlide.exe')
        Install-Files $script:source $script:target
        Names (Join-Path $script:target 'app') | Should -Be @('CellGlide.exe')
    }
}
Describe 'Install-Files and links' {
    BeforeEach {
        $script:root = Join-Path $TestDrive ('r-' + [guid]::NewGuid().ToString('N'))
        $script:target = Join-Path $script:root 'CellGlide'
        $script:source = Join-Path $script:root 'src'
        $script:outside = Join-Path $script:root 'outside'
        New-Tree $script:target @('old.txt'); New-Tree $script:source @('new.txt'); New-Tree $script:outside @('keep.txt')
    }
    AfterEach {
        Get-ChildItem -LiteralPath $script:root -Recurse -Force -Attributes ReparsePoint -ErrorAction SilentlyContinue |
            ForEach-Object { [IO.Directory]::Delete($_.FullName) }
    }
    It 'a junction inside the installed directory stops the swap before anything moves' {
        $null = New-Item -ItemType Junction -Path (Join-Path $script:target 'app') -Target $script:outside
        { Install-Files $script:source $script:target } | Should -Throw '*junction or symbolic link*'
        Test-Path (Join-Path $script:target 'old.txt') | Should -BeTrue
        Names $script:outside | Should -Be @('keep.txt')
        Test-Path "$($script:target).new" | Should -BeFalse
    }
    It 'a leftover .old that is a junction is refused, not deleted through' {
        $null = New-Item -ItemType Junction -Path "$($script:target).old" -Target $script:outside
        { Install-Files $script:source $script:target } | Should -Throw '*junction or symbolic link*'
        Names $script:outside | Should -Be @('keep.txt')
        Names $script:target | Should -Be @('old.txt')
    }
    It 'a junction in the new files is refused' {
        $null = New-Item -ItemType Junction -Path (Join-Path $script:source 'app') -Target $script:outside
        { Install-Files $script:source $script:target } | Should -Throw '*junction or symbolic link*'
        Names $script:target | Should -Be @('old.txt')
    }
}
Describe 'New-PrivateWorkDir' -Tag 'Elevated' {
    It 'creates a new folder that only SYSTEM and Administrators can use, inheriting nothing' {
        $dir = New-PrivateWorkDir $TestDrive
        try {
            Split-Path -Leaf $dir | Should -Match '^CellGlide\.work-[0-9a-f]{32}$'
            $acl = Get-Acl -LiteralPath $dir
            $acl.AreAccessRulesProtected | Should -BeTrue
            $rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
            @($rules | ForEach-Object { $_.IdentityReference.Value } | Sort-Object -Unique) | Should -Be @('S-1-3-4', 'S-1-5-18', 'S-1-5-32-544')
            @($rules | Where-Object { $_.IsInherited }).Count | Should -Be 0
            @($rules | Where-Object { $_.IdentityReference.Value -ne 'S-1-3-4' -and $_.FileSystemRights -ne 'FullControl' }).Count | Should -Be 0
            # OWNER RIGHTS may only read the ACL: whoever owns a file inside cannot change it.
            @($rules | Where-Object { $_.IdentityReference.Value -eq 'S-1-3-4' -and ($_.FileSystemRights -band -bnot [Security.AccessControl.FileSystemRights]::Synchronize) -ne [Security.AccessControl.FileSystemRights]::ReadPermissions }).Count | Should -Be 0
            $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value | Should -Be 'S-1-5-32-544'
            # What is created inside inherits the same three entries only.
            $child = Join-Path $dir 'download'; $null = New-Item -ItemType Directory $child
            $file = Join-Path $child 'x.zip'; Set-Content $file 'x'
            foreach ($p in $child, $file) {
                $r = @((Get-Acl -LiteralPath $p).GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
                @($r | ForEach-Object { $_.IdentityReference.Value } | Sort-Object -Unique) | Should -Be @('S-1-3-4', 'S-1-5-18', 'S-1-5-32-544') -Because $p
                @($r | Where-Object { $_.IdentityReference.Value -eq 'S-1-3-4' -and ($_.FileSystemRights -band -bnot [Security.AccessControl.FileSystemRights]::Synchronize) -ne [Security.AccessControl.FileSystemRights]::ReadPermissions }).Count | Should -Be 0 -Because $p
            }
        } finally { Remove-Item -LiteralPath $dir -Recurse -Force }
    }
    It 'defaults to the folder that holds the install directory' {
        $InstallDir = Join-Path $TestDrive 'Programs\CellGlide'; $null = New-Item -ItemType Directory (Split-Path $InstallDir)
        $dir = New-PrivateWorkDir
        try { Split-Path -Parent $dir | Should -Be (Split-Path -Parent $InstallDir) }
        finally { Remove-Item -LiteralPath $dir -Recurse -Force }
    }
}
