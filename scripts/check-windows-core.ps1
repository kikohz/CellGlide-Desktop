# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check-windows-core.ps1 [-Release]
#
# The C# side of the Windows app (windows/CellGlide.Core, windows/CellGlide.Tests):
#   1. cargo build -p cg-ffi (cellglide_native.dll, and csbindgen rewrites NativeMethods.g.cs);
#   2. the committed NativeMethods.g.cs must be what the build wrote;
#   3. dotnet build of windows/CellGlide.sln (warnings are errors), with locked package versions;
#   4. dotnet test of windows/CellGlide.Tests against that DLL.
# -Release takes the DLL from target\release instead of target\debug.
# The dotnet steps run inside windows\ so that windows\global.json (SDK pin, test runner) applies.
param(
    [switch]$Release
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'dev-env.ps1')

$env:DOTNET_CLI_TELEMETRY_OPTOUT = '1'
$env:TESTINGPLATFORM_TELEMETRY_OPTOUT = '1'
$env:DOTNET_NOLOGO = '1'

function Invoke-Step([string]$name, [scriptblock]$command) {
    Write-Host "==> $name"
    & $command
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: $name (exit code $LASTEXITCODE)"
        exit 1
    }
}

Push-Location $root
try {
    if ($Release) {
        Invoke-Step 'cargo build -p cg-ffi --release' { cargo build -p cg-ffi --locked --release }
        $nativeDir = Join-Path $root 'target\release'
    } else {
        Invoke-Step 'cargo build -p cg-ffi' { cargo build -p cg-ffi --locked }
        $nativeDir = Join-Path $root 'target\debug'
    }

    Write-Host '==> NativeMethods.g.cs matches the committed copy'
    git diff --exit-code -- windows/CellGlide.Core/Native/NativeMethods.g.cs
    if ($LASTEXITCODE -ne 0) {
        Write-Host 'FAILED: building cg-ffi changed windows/CellGlide.Core/Native/NativeMethods.g.cs.'
        Write-Host 'Commit the regenerated file together with crates/cg-ffi/src/ffi.rs (and codes.rs).'
        exit 1
    }

    Push-Location (Join-Path $root 'windows')
    try {
        $native = "-p:CellGlideNativeDir=$nativeDir"
        Invoke-Step 'dotnet restore (locked)' { dotnet restore CellGlide.sln --locked-mode $native }
        Invoke-Step 'dotnet build' { dotnet build CellGlide.sln -c Debug --no-restore $native }
        Invoke-Step 'dotnet test' { dotnet test --project CellGlide.Tests --no-build -c Debug }
    } finally {
        Pop-Location
    }
} finally {
    Pop-Location
}

Write-Host 'OK: bindings match, build has no warnings, all tests pass.'
