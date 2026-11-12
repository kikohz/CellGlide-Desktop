# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check-windows-core.ps1 [-Release] [-Publish]
#
# The C# side of the Windows app (windows/CellGlide.Core, CellGlide.Tests, CellGlide.App):
#   1. cargo build -p cg-ffi (cellglide_native.dll, and csbindgen rewrites NativeMethods.g.cs; build.rs
#      is touched first, so its build script always runs, even on a target that was built before);
#   2. the committed NativeMethods.g.cs must be what the build wrote;
#   3. dotnet build of windows/CellGlide.sln (warnings are errors), with locked package versions;
#   4. dotnet test of windows/CellGlide.Tests against that DLL;
#   5. scripts/tests/test-check-nuget-licenses.ps1 (the license script against fixtures), then
#      scripts/check-nuget-licenses.ps1 -Check (packaging/licenses/nuget.txt and nuget-notices.txt
#      are current, every license is allowed or reviewed, every shipped component has its text);
#   6. scripts/gen-windows-icons.py --check (the committed .ico files are what the script makes).
#   7. scripts/update-licenses.sh --check in Git's bash (the Rust license lists are current, and
#      render-about.py still renders as cargo-about 0.9.2 does).
#   8. scripts/test-windows-scripts.ps1: the Pester 5 tests of the Windows scripts (scripts/tests and
#      packaging/windows/tests *.Tests.ps1; 'Elevated' ones only from an administrator shell).
# Step 4 also checks that a Release build defaults to target\release's DLL and refuses target\debug's,
# and that a Release build without the DLL says to run 'cargo build -p cg-ffi --release'.
# -Release takes the DLL from target\release instead of target\debug.
# -Publish then builds cg-ffi in release and publishes CellGlide.App (Native AOT, self-contained) for
# this machine's architecture with a plain `dotnet publish -c Release` (no CellGlideNativeDir), and
# checks that the publish folder (the project's PublishDir) has CellGlide.exe and target\release's
# cellglide_native.dll (same SHA-256) and no onnxruntime*.dll or DirectML*.dll, and that the
# shipped DLL has no debug-only diagnostics override (no CELLGLIDE_DIAG_TARGET string; not 'localhost',
# which the license texts the DLL embeds may contain).
# -Skip leaves out checks that come out the same on every machine, for CI jobs that run them
# elsewhere (once, on one runner): nuget-licenses (step 5), icons (6), rust-licenses (7) and
# scripts (8). Steps 1-4 always run: they exercise this machine's DLL and .NET runtime.
# The dotnet steps run inside windows\ so that windows\global.json (SDK pin, test runner) applies.
param(
    [switch]$Release,
    [switch]$Publish,
    # Names, or one comma-separated string (powershell -File passes 'a,b' as one argument).
    [string[]]$Skip = @()
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$Skip = @($Skip | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
foreach ($name in $Skip) {
    if ($name -notin 'nuget-licenses', 'icons', 'rust-licenses', 'scripts') {
        Write-Host "FAILED: -Skip $name is not one of nuget-licenses, icons, rust-licenses, scripts."
        exit 1
    }
}
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

        # A Release build or publish takes target\release's DLL unless told otherwise, and refuses a
        # debug one (windows/Directory.Build.props, CellGlide.Core.csproj CheckCellGlideNativeRelease).
        Write-Host '==> Release takes cellglide_native.dll from target\release and refuses target\debug'
        $wantRelease = [IO.Path]::GetFullPath((Join-Path $root 'target\release\cellglide_native.dll'))
        $releaseDll = (dotnet msbuild CellGlide.App -nologo -getProperty:CellGlideNativeDll -p:Configuration=Release | Out-String).Trim()
        if ($LASTEXITCODE -ne 0 -or $releaseDll -ne $wantRelease) {
            Write-Host "FAILED: a Release build of CellGlide.App takes '$releaseDll', not '$wantRelease'."
            exit 1
        }
        $refusal = (dotnet msbuild CellGlide.Core -nologo -t:CheckCellGlideNativeRelease -p:Configuration=Release '-p:CellGlideNativeDir=..\target\debug\' | Out-String)
        if ($LASTEXITCODE -eq 0 -or $refusal -notmatch 'must not ship the debug DLL') {
            Write-Host 'FAILED: a Release build with CellGlideNativeDir=..\target\debug\ was not refused:'
            Write-Host $refusal
            exit 1
        }
        # A Release build without the DLL says to build the release one (CheckCellGlideNative).
        $missing = (dotnet msbuild CellGlide.Core -nologo -t:CheckCellGlideNative -p:Configuration=Release '-p:CellGlideNativeDir=..\target\no-such-dir\' | Out-String)
        if ($LASTEXITCODE -eq 0 -or $missing -notmatch [regex]::Escape('cargo build -p cg-ffi --release')) {
            Write-Host 'FAILED: a Release build without cellglide_native.dll did not say to run cargo build -p cg-ffi --release:'
            Write-Host $missing
            exit 1
        }
        $debugDll = (dotnet msbuild CellGlide.Core -nologo -t:CheckCellGlideNativeRelease -p:Configuration=Debug | Out-String)
        if ($LASTEXITCODE -ne 0) {
            Write-Host 'FAILED: the Release-only DLL check refused a Debug build:'
            Write-Host $debugDll
            exit 1
        }
    } finally {
        Pop-Location
    }

    if ($Skip -contains 'nuget-licenses') {
        Write-Host '==> NuGet license script tests and NuGet licenses: skipped (-Skip nuget-licenses)'
    } else {
        Invoke-Step 'NuGet license script tests' {
            powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'tests\test-check-nuget-licenses.ps1')
        }

        Invoke-Step 'NuGet licenses' {
            powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'check-nuget-licenses.ps1') -Check
        }
    }

    # The committed .ico files are what scripts/gen-windows-icons.py makes (pure Python, the same on
    # every machine). Python 3: the py launcher, else python3 or python on PATH (not the Store stubs
    # in WindowsApps), else the newest per-user install. Looked for only when a Python step runs.
    $python = $null
    if (-not ($Skip -contains 'icons' -and $Skip -contains 'rust-licenses')) {
        $pyLauncher = Get-Command py -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($pyLauncher) {
            $python = @($pyLauncher.Source, '-3')
        } else {
            $found = Get-Command python3, python -CommandType Application -ErrorAction SilentlyContinue |
                Where-Object { $_.Source -notlike '*\WindowsApps\*' } | Select-Object -First 1
            if (-not $found) {
                $found = Get-ChildItem (Join-Path $env:LOCALAPPDATA 'Programs\Python\Python3*\python.exe') -ErrorAction SilentlyContinue |
                    Sort-Object FullName -Descending | Select-Object -First 1
            }
            if ($found) { $python = @($(if ($found.Source) { $found.Source } else { $found.FullName })) }
        }
        if (-not $python) {
            Write-Host 'FAILED: Python 3 is needed to check the icons and the Rust license lists.'
            exit 1
        }
    }
    if ($Skip -contains 'icons') {
        Write-Host '==> Windows icons: skipped (-Skip icons)'
    } else {
        Invoke-Step 'Windows icons match scripts/gen-windows-icons.py' {
            $exe = $python[0]
            $pre = @($python | Select-Object -Skip 1)
            & $exe @pre -I (Join-Path $PSScriptRoot 'gen-windows-icons.py') --check
        }
    }

    # The Rust license lists (the Windows one is embedded in cellglide_native.dll) are what
    # scripts/update-licenses.sh generates; its --check also runs render-about.py --self-test. It needs
    # Git's bash and cargo-about 0.9.2. CI runs it on Linux and passes -Skip rust-licenses here.
    if ($Skip -contains 'rust-licenses') {
        Write-Host '==> Rust license lists: skipped (-Skip rust-licenses)'
    } else {
        $gitCmd = Get-Command git -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        $bash = $null
        if ($gitCmd) {
            foreach ($rel in '..\bin\bash.exe', '..\..\bin\bash.exe') {
                $candidate = [IO.Path]::GetFullPath((Join-Path (Split-Path -Parent $gitCmd.Source) $rel))
                if (Test-Path $candidate) { $bash = $candidate; break }
            }
        }
        if (-not $bash) {
            Write-Host "FAILED: Git's bash.exe is needed for scripts/update-licenses.sh --check (not found next to git)."
            exit 1
        }
        if (-not (Get-Command cargo-about -CommandType Application -ErrorAction SilentlyContinue)) {
            Write-Host 'FAILED: cargo-about 0.9.2 is needed for scripts/update-licenses.sh --check (cargo install --locked --version 0.9.2 cargo-about --features cli).'
            exit 1
        }
        Invoke-Step 'Rust license lists match scripts/update-licenses.sh (--check, with render-about.py --self-test)' {
            # The Python found above, with forward slashes for bash; no spaces in the path is assumed.
            $env:PYTHON = (@($python) -join ' ').Replace('\', '/')
            try {
                & $bash (Join-Path $PSScriptRoot 'update-licenses.sh').Replace('\', '/') --check
            } finally {
                Remove-Item Env:PYTHON -ErrorAction SilentlyContinue
            }
        }
    }

    if ($Skip -contains 'scripts') {
        Write-Host '==> Pester tests of the Windows scripts: skipped (-Skip scripts)'
    } else {
        Invoke-Step 'Pester tests of the Windows scripts (scripts/test-windows-scripts.ps1)' {
            powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'test-windows-scripts.ps1')
        }
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
            # Plain: no CellGlideNativeDir, so the Release default (target\release) is what is checked.
            Invoke-Step "dotnet publish CellGlide.App -c Release -r $rid" {
                dotnet publish CellGlide.App -c Release -r $rid
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
        # The plain publish took the DLL cargo just built in release, byte for byte.
        $published = (Get-FileHash (Join-Path $publishDir 'cellglide_native.dll') -Algorithm SHA256).Hash
        $built = (Get-FileHash (Join-Path $root 'target\release\cellglide_native.dll') -Algorithm SHA256).Hash
        if ($published -ne $built) {
            Write-Host "FAILED: the published cellglide_native.dll ($published) is not target\release's ($built)."
            exit 1
        }
        Write-Host '==> the published cellglide_native.dll is target\release''s'
        # Diagnostics in a release build always go to production: the debug-only override
        # (crates/cg-ffi/src/runtime.rs, diag_target) must not be compiled in. Its variable name would
        # show as a string, ASCII or UTF-16. (Not its loopback host: the DLL embeds the third-party
        # license texts, where the word 'localhost' may appear.)
        $shipped = Join-Path $publishDir 'cellglide_native.dll'
        $bytes = [IO.File]::ReadAllBytes($shipped)
        # UTF-16 read from both byte alignments.
        $texts = @([Text.Encoding]::ASCII.GetString($bytes), [Text.Encoding]::Unicode.GetString($bytes), [Text.Encoding]::Unicode.GetString($bytes, 1, $bytes.Length - 1))
        foreach ($needle in @('CELLGLIDE_DIAG_TARGET')) {
            foreach ($text in $texts) {
                if ($text.IndexOf($needle, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
                    Write-Host "FAILED: the release cellglide_native.dll contains '$needle' (the debug-only diagnostics override)"
                    exit 1
                }
            }
        }
        Write-Host "==> the release cellglide_native.dll has no diagnostics override ('CELLGLIDE_DIAG_TARGET')"
    }
} finally {
    Pop-Location
}

if ($Publish) {
    Write-Host 'OK: bindings match, build has no warnings, all tests pass, NuGet licenses listed, app published.'
} else {
    Write-Host 'OK: bindings match, build has no warnings, all tests pass, NuGet licenses listed.'
}
