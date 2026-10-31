# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check-windows-core.ps1 [-Release] [-Publish]
#
# The C# side of the Windows app (windows/CellGlide.Core, CellGlide.Tests, CellGlide.App):
#   1. cargo build -p cg-ffi (cellglide_native.dll, and csbindgen rewrites NativeMethods.g.cs; build.rs
#      is touched first, so its build script always runs, even on a target that was built before);
#   2. the committed NativeMethods.g.cs must be what the build wrote;
#   3. dotnet build of windows/CellGlide.sln (warnings are errors), with locked package versions;
#   4. dotnet test of windows/CellGlide.Tests against that DLL;
#   5. scripts/tests/test-check-nuget-licenses.ps1 (the license script against fixtures), then
#      scripts/check-nuget-licenses.ps1 -Check (packaging/licenses/nuget.txt is current and every
#      license is allowed or reviewed).
# -Release takes the DLL from target\release instead of target\debug.
# -Publish then builds cg-ffi in release and publishes CellGlide.App (Native AOT, self-contained) for
# this machine's architecture, and checks that the publish folder (the project's PublishDir) has
# CellGlide.exe and cellglide_native.dll and no onnxruntime*.dll or DirectML*.dll.
# The dotnet steps run inside windows\ so that windows\global.json (SDK pin, test runner) applies.
param(
    [switch]$Release,
    [switch]$Publish
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
    # cargo reruns build.rs only when ffi.rs, codes.rs or build.rs changed. A committed
    # NativeMethods.g.cs that went stale without them changing would pass unnoticed, so give
    # build.rs a new modification time (its content is untouched) to make csbindgen run now.
    (Get-Item (Join-Path $root 'crates\cg-ffi\build.rs')).LastWriteTime = Get-Date

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

    Invoke-Step 'NuGet license script tests' {
        powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'tests\test-check-nuget-licenses.ps1')
    }

    Invoke-Step 'NuGet licenses' {
        powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'check-nuget-licenses.ps1') -Check
    }

    if ($Publish) {
        $rid = 'win-x64'
        if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq 'Arm64') { $rid = 'win-arm64' }
        Invoke-Step 'cargo build -p cg-ffi --release' { cargo build -p cg-ffi --release --locked }
        Push-Location (Join-Path $root 'windows')
        try {
            # The publish folder as the project defines it (its TFM is in the csproj, not here).
            $publishDir = (dotnet msbuild CellGlide.App -nologo -getProperty:PublishDir -p:Configuration=Release "-p:RuntimeIdentifier=$rid" | Out-String).Trim()
            if ($LASTEXITCODE -ne 0 -or -not $publishDir) {
                Write-Host "FAILED: could not read CellGlide.App's PublishDir for Release/$rid from the project (got '$publishDir')."
                exit 1
            }
            if (-not [IO.Path]::IsPathRooted($publishDir)) { $publishDir = Join-Path (Join-Path $root 'windows\CellGlide.App') $publishDir }
            $publishDir = [IO.Path]::GetFullPath($publishDir)
            # An earlier publish must not satisfy the checks below.
            if (Test-Path $publishDir) { Remove-Item -Recurse -Force $publishDir }
            Invoke-Step "dotnet publish CellGlide.App -r $rid" {
                dotnet publish CellGlide.App -c Release -r $rid '-p:CellGlideNativeDir=..\target\release\'
            }
        } finally {
            Pop-Location
        }
        foreach ($file in 'CellGlide.exe', 'cellglide_native.dll') {
            if (-not (Test-Path (Join-Path $publishDir $file))) {
                Write-Host "FAILED: $file is missing from $publishDir"
                exit 1
            }
        }
        # The Windows App SDK's AI and ML components are excluded in CellGlide.App.csproj (and
        # nuget.txt says they are not shipped); a package update must not bring their files back.
        $unwanted = @(Get-ChildItem -Path $publishDir -Recurse -File -Include 'onnxruntime*.dll', 'DirectML*.dll')
        if ($unwanted.Count -gt 0) {
            Write-Host "FAILED: the publish folder has excluded AI/ML files: $(($unwanted | ForEach-Object { $_.Name }) -join ', ')"
            exit 1
        }
        Write-Host "==> CellGlide.exe and cellglide_native.dll are in $publishDir; no onnxruntime or DirectML"
    }
} finally {
    Pop-Location
}

if ($Publish) {
    Write-Host 'OK: bindings match, build has no warnings, all tests pass, NuGet licenses listed, app published.'
} else {
    Write-Host 'OK: bindings match, build has no warnings, all tests pass, NuGet licenses listed.'
}
