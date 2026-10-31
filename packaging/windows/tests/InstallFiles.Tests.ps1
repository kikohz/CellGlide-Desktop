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
