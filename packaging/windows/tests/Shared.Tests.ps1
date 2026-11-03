# Pester 5 tests: the functions install.ps1 and uninstall.ps1 share are kept identical (decision 3).
BeforeAll {
    $script:Functions = { param($file) $ast = [Management.Automation.Language.Parser]::ParseFile($file, [ref]$null, [ref]$null)
        $map = @{}; foreach ($f in $ast.FindAll({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] }, $false)) { $map[$f.Name] = $f.Extent.Text }; $map }
    $script:Install = & $script:Functions (Resolve-Path "$PSScriptRoot\..\install.ps1").Path
    $script:Uninstall = & $script:Functions (Resolve-Path "$PSScriptRoot\..\uninstall.ps1").Path
}
Describe 'Shared functions are identical' {
    It 'in install.ps1 and uninstall.ps1' {
        # Decision 3's list, then the elevated-child and work-folder helpers both scripts use.
        $names = 'Test-Elevated', 'Test-TokenElevated', 'Invoke-Native', 'Get-OurAdapter', 'Wait-AdapterGone', 'Stop-OurService', 'Remove-PathWithRetry',
                 'Remove-PhantomDevice', 'Select-ProcessesToStop', 'Test-ReparsePoint', 'Get-ReparsePoints', 'Refuse-ReparsePoint',
                 'Stop-CellGlideProcesses', 'Get-TranscriptBody', 'New-ChildBootstrap', 'Get-FileIdentity', 'Copy-LogToParent',
                 'New-PrivateWorkDir', 'Remove-StaleWorkDirs', 'Test-AdminSid', 'Test-DataDirAdminOnly'
        foreach ($n in $names) {
            $script:Install.ContainsKey($n) | Should -BeTrue -Because "install.ps1 must define $n"
            $script:Uninstall.ContainsKey($n) | Should -BeTrue -Because "uninstall.ps1 must define $n"
            $script:Uninstall[$n] | Should -BeExactly $script:Install[$n] -Because $n
        }
    }
    It 'any function both scripts define is the same text' {
        $both = @($script:Install.Keys | Where-Object { $script:Uninstall.ContainsKey($_) })
        $both.Count | Should -BeGreaterOrEqual 18
        foreach ($n in $both) { $script:Uninstall[$n] | Should -BeExactly $script:Install[$n] -Because $n }
    }
}
