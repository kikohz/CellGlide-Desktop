# Loads a script's functions and constants for Pester tests, without running the script.
#
# Usage (dot-source both, or the definitions vanish with the function's own scope):
#   BeforeAll { . "$PSScriptRoot\Import-ScriptFunctions.ps1"; . Import-ScriptFunctions <script> }
#
# The script is parsed, not run. Its top-level statements are executed in order, in the caller's
# scope, when they are
#   (a) a function definition, or
#   (b) an assignment whose right-hand side has no command call (a constant such as
#       $InstallDir = 'C:\...'; $code = Invoke-Something is skipped, as is anything that would
#       call a command to compute its value).
# The param block and every other statement are not executed, so the script's parameters (for
# example $BaseUrl) are not defined: a test that needs one assigns it itself. $Path (this
# function's parameter) is left set in the caller's scope.
function Import-ScriptFunctions([string]$Path) {
    $__isfTokens = $null
    $__isfErrors = $null
    $__isfAst = [System.Management.Automation.Language.Parser]::ParseFile(
        [IO.Path]::GetFullPath($Path), [ref]$__isfTokens, [ref]$__isfErrors)
    if ($__isfErrors -and $__isfErrors.Count -gt 0) {
        throw "Import-ScriptFunctions: $Path does not parse: $($__isfErrors[0].Message) (line $($__isfErrors[0].Extent.StartLineNumber))"
    }
    $__isfRun = New-Object System.Collections.Generic.List[string]
    foreach ($__isfBlock in @($__isfAst.BeginBlock, $__isfAst.ProcessBlock, $__isfAst.EndBlock)) {
        if (-not $__isfBlock) { continue }
        foreach ($__isfStatement in $__isfBlock.Statements) {
            if ($__isfStatement -is [System.Management.Automation.Language.FunctionDefinitionAst]) {
                $__isfRun.Add($__isfStatement.Extent.Text)
            } elseif ($__isfStatement -is [System.Management.Automation.Language.AssignmentStatementAst]) {
                $__isfCalls = $__isfStatement.Right.FindAll(
                    { param($node) $node -is [System.Management.Automation.Language.CommandAst] }, $true)
                if (@($__isfCalls).Count -eq 0) { $__isfRun.Add($__isfStatement.Extent.Text) }
            }
        }
    }
    Remove-Variable __isfTokens, __isfErrors, __isfAst, __isfBlock, __isfStatement, __isfCalls -ErrorAction SilentlyContinue
    foreach ($__isfText in $__isfRun) {
        . ([scriptblock]::Create($__isfText))
    }
    Remove-Variable __isfRun, __isfText -ErrorAction SilentlyContinue
}
