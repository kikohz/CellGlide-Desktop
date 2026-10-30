# Test for the data directory ACL step of scripts\install-dev.ps1 (Set-DataDirAcl), on
# temporary folders only. Needs an elevated PowerShell (it sets owners). Plain script:
# throws on failure, prints "all passed".
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$install = Join-Path (Split-Path -Parent $here) 'install-dev.ps1'

# Load the install script's functions without running it.
$ast = [System.Management.Automation.Language.Parser]::ParseFile($install, [ref]$null, [ref]$null)
foreach ($fn in $ast.FindAll({ param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst] }, $false)) {
    . ([scriptblock]::Create($fn.Extent.Text))
}
$SidSystem = '*S-1-5-18'
$SidAdmins = '*S-1-5-32-544'
$SidUsers = '*S-1-5-32-545'
# Any existing service gives a valid "NT SERVICE\<name>" account for the grant.
$ServiceName = 'Dnscache'

function Assert($cond, $msg) { if (-not $cond) { throw "FAIL: $msg" } }
function Owner([string]$path) { return (Get-Acl -LiteralPath $path).GetOwner([Security.Principal.SecurityIdentifier]).Value }
function Sddl([string]$path) { return (Get-Acl -LiteralPath $path).Sddl }

$tmp = Join-Path $env:TEMP ('cg-datadir-test-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    # A folder outside the data directory that a link points at; it must come out untouched.
    $victim = Join-Path $tmp 'victim'
    New-Item -ItemType Directory -Path $victim | Out-Null
    Set-Content -LiteralPath (Join-Path $victim 'file.txt') -Value 'x'
    $victimSddl = Sddl $victim
    $victimFileSddl = Sddl (Join-Path $victim 'file.txt')

    # Case 1: a junction inside a pre-existing data directory stops the step.
    $DataDir = Join-Path $tmp 'data1'
    New-Item -ItemType Directory -Path (Join-Path $DataDir 'logs') | Out-Null
    New-Item -ItemType Junction -Path (Join-Path $DataDir 'logs\old') -Target $victim | Out-Null
    $err = $null
    try { Set-DataDirAcl } catch { $err = $_.Exception.Message }
    Assert ($err -and $err.Contains('junction or symbolic link')) "case 1: expected a refusal, got: $err"
    Assert ($err.Contains('logs\old')) "case 1: the refusal doesn't name the link: $err"
    Assert ((Sddl $victim) -eq $victimSddl) 'case 1: the junction target ACL changed'
    Assert ((Sddl (Join-Path $victim 'file.txt')) -eq $victimFileSddl) 'case 1: a file behind the junction changed'

    # Case 2: the data directory itself a junction.
    $DataDir = Join-Path $tmp 'data2'
    New-Item -ItemType Junction -Path $DataDir -Target $victim | Out-Null
    $err = $null
    try { Set-DataDirAcl } catch { $err = $_.Exception.Message }
    Assert ($err -and $err.Contains('junction or symbolic link')) "case 2: expected a refusal, got: $err"
    Assert ((Sddl $victim) -eq $victimSddl) 'case 2: the junction target ACL changed'

    # Case 3: a plain pre-existing tree is taken over: every entry owned by Administrators,
    # health\ readable by Users, logs\ not.
    $DataDir = Join-Path $tmp 'data3'
    New-Item -ItemType Directory -Path (Join-Path $DataDir 'logs\sub') | Out-Null
    Set-Content -LiteralPath (Join-Path $DataDir 'logs\sub\a.log') -Value 'x'
    Set-DataDirAcl
    foreach ($p in @($DataDir, (Join-Path $DataDir 'logs'), (Join-Path $DataDir 'logs\sub'),
                     (Join-Path $DataDir 'logs\sub\a.log'), (Join-Path $DataDir 'health'))) {
        Assert ((Owner $p) -eq 'S-1-5-32-544') "case 3: $p is owned by $(Owner $p)"
    }
    Assert ((Sddl (Join-Path $DataDir 'health')) -match ';;;BU\)') 'case 3: health lacks the Users entry'
    Assert (-not ((Sddl (Join-Path $DataDir 'logs')) -match ';;;BU\)')) 'case 3: logs has a Users entry'
    Assert (-not ((Sddl $DataDir) -match '\(A;[A-Z]*ID;')) 'case 3: the root still inherits'

    Write-Host 'all passed'
}
finally {
    # Junctions first, so removing the tree can't reach through them.
    Get-ChildItem -LiteralPath $tmp -Recurse -Force -Attributes ReparsePoint -ErrorAction SilentlyContinue |
        ForEach-Object { [System.IO.Directory]::Delete($_.FullName) }
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
