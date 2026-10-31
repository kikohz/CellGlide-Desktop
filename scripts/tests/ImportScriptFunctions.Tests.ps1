# Pester 5 tests for scripts\tests\Import-ScriptFunctions.ps1.
BeforeAll {
    . "$PSScriptRoot\Import-ScriptFunctions.ps1"
}
Describe 'Import-ScriptFunctions' {
    BeforeAll {
        $script:sample = Join-Path $TestDrive 'sample.ps1'
        $marker = Join-Path $TestDrive 'ran.txt'
        Set-Content $script:sample @"
param([string]`$BaseUrl = 'https://example.invalid')
`$ErrorActionPreference = 'Stop'
`$InstallDir = 'C:\Program Files\Sample'
`$Codes = @{ x64 = 0x8664; arm64 = 0xAA64 }
`$Computed = Get-Date
`$Nested = @{ a = { Set-Content '$marker' 'nested' } }
function Get-Install { `$InstallDir }
function Get-Code([string]`$Arch) { `$Codes[`$Arch] }
Set-Content '$marker' 'main flow ran'
exit 3
"@
    }
    It 'defines functions and constants in the caller''s scope when dot-sourced' {
        . Import-ScriptFunctions $script:sample
        Get-Install | Should -Be 'C:\Program Files\Sample'
        Get-Code 'arm64' | Should -Be 0xAA64
        $InstallDir | Should -Be 'C:\Program Files\Sample'
    }
    It 'skips the param block, assignments that call commands and the main flow' {
        . Import-ScriptFunctions $script:sample
        Test-Path variable:BaseUrl | Should -BeFalse
        Test-Path variable:Computed | Should -BeFalse
        Test-Path variable:Nested | Should -BeFalse
        Test-Path (Join-Path $TestDrive 'ran.txt') | Should -BeFalse
    }
    It 'leaves nothing behind when called without dot-sourcing' {
        & {
            Import-ScriptFunctions $script:sample
            Test-Path function:Get-Install | Should -BeFalse
            Test-Path variable:InstallDir | Should -BeFalse
        }
    }
    It 'refuses a script that does not parse' {
        $broken = Join-Path $TestDrive 'broken.ps1'
        Set-Content $broken 'function Broken {'
        { . Import-ScriptFunctions $broken } | Should -Throw '*does not parse*'
    }
}
