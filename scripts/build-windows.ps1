# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-windows.ps1 -Arch x64|arm64 [-OutDir <dir>]
#
# Builds the Windows package for one architecture (an arm64 package is built on x64 but never run):
#   1. cargo build --release --locked --target <triple> -p cg-service -p cg-cli -p cg-ffi;
#   2. the committed NativeMethods.g.cs must be what that build wrote;
#   3. dotnet publish CellGlide.App (Native AOT, self-contained) for win-<arch> with that DLL and the
#      workspace version, from windows\ (so windows\global.json applies), locked package versions;
#   4. scripts\fetch-wintun.ps1 (verified Wintun 0.14.1) into the staging directory;
#   5. assembles the staging directory (spec 10.1):
#        cellglide-service.exe  cellglide.exe  wintun.dll  wintun-LICENSE.txt
#        THIRD-PARTY-LICENSES.txt  LICENSE.txt  uninstall.ps1  version.txt
#        app\   the publish folder without its .pdb files
#      and checks it with Test-PackageLayout;
#   6. writes, in -OutDir (default dist\windows\<version>\):
#        CellGlide-<version>-windows-<arch>.zip          (no .pdb inside)
#        SHA256SUMS.<arch>                              (one line: <lowercase hex>  <zip name>)
#        CellGlide-<version>-windows-<arch>-symbols.zip (the publish folder's .pdb files under app\,
#                                                        and the three Rust binaries' .pdb files)
# The version comes only from Cargo.toml's [workspace.package] version. Staging is under target\.
param(
    [Parameter(Mandatory)][ValidateSet('x64', 'arm64')][string]$Arch,
    [string]$OutDir = ''
)

$ErrorActionPreference = 'Stop'

# The Rust target of each package architecture.
$RustTargets = @{ 'x64' = 'x86_64-pc-windows-msvc'; 'arm64' = 'aarch64-pc-windows-msvc' }
# IMAGE_FILE_HEADER.Machine of each package architecture.
$PeMachines = @{ 'x64' = 0x8664; 'arm64' = 0xAA64 }
# Files every package has, relative to its root. The app's resource index is named after its
# assembly (CellGlide.pri), as the unpackaged publish writes it.
$RequiredFiles = @(
    'cellglide-service.exe', 'cellglide.exe', 'wintun.dll', 'wintun-LICENSE.txt',
    'THIRD-PARTY-LICENSES.txt', 'LICENSE.txt', 'uninstall.ps1', 'version.txt',
    'app\CellGlide.exe', 'app\cellglide_native.dll', 'app\CellGlide.pri'
)
# What may be at the top level (spec 10.1, plus LICENSE.txt and version.txt): the files above that
# are not under app\, and the app directory. app\ has the rules below, not a list. (A literal list:
# Import-ScriptFunctions loads only constants whose value calls no command.)
$TopLevel = @(
    'cellglide-service.exe', 'cellglide.exe', 'wintun.dll', 'wintun-LICENSE.txt',
    'THIRD-PARTY-LICENSES.txt', 'LICENSE.txt', 'uninstall.ps1', 'version.txt', 'app'
)
# The Windows App SDK's AI/ML files, excluded in CellGlide.App.csproj.
$UnwantedPatterns = @('onnxruntime*.dll', 'DirectML*.dll')
# The debug-only diagnostics override (crates/cg-ffi/src/runtime.rs) must not be in a release DLL.
# ('localhost' is not checked: the DLL embeds the third-party license texts, which may contain it.)
$DiagOverride = 'CELLGLIDE_DIAG_TARGET'
# The last section of THIRD-PARTY-LICENSES.txt (after the About page's text).
$WintunSection = ('=' * 80) + "`nWintun`nwintun.dll is distributed under its own license; see wintun-LICENSE.txt next to it.`n"

function Get-WorkspaceVersion([string]$CargoToml) {
    $section = ''
    foreach ($line in [IO.File]::ReadAllLines($CargoToml)) {
        $trimmed = $line.Trim()
        if ($trimmed -match '^\[(.+)\]$') { $section = $Matches[1].Trim(); continue }
        if ($section -eq 'workspace.package' -and $trimmed -match '^version\s*=\s*"([^"]+)"') {
            return $Matches[1]
        }
    }
    throw "no version in the [workspace.package] section of $CargoToml"
}

function Get-RustTarget([string]$Arch) {
    if (-not $RustTargets.ContainsKey($Arch)) { throw "unknown architecture '$Arch' (x64 or arm64)" }
    return $RustTargets[$Arch]
}

function Get-PeMachine([string]$Path) {
    $stream = [IO.File]::OpenRead($Path)
    try {
        $reader = New-Object IO.BinaryReader($stream)
        if ($stream.Length -lt 0x40) { throw "$Path is not a PE file (too short)" }
        $stream.Position = 0x3C
        $offset = $reader.ReadUInt32()
        if ([int64]$offset + 6 -gt $stream.Length) { throw "$Path is not a PE file (e_lfanew 0x$('{0:X}' -f $offset) is past the end)" }
        $stream.Position = $offset
        $signature = $reader.ReadUInt32()
        if ($signature -ne 0x00004550) { throw "$Path is not a PE file (no PE\0\0 at 0x$('{0:X}' -f $offset))" }
        return [uint16]$reader.ReadUInt16()
    } finally {
        $stream.Dispose()
    }
}

# The text without its first line ('' when there is no newline), as about.rs without_first_line.
function Remove-FirstLine([string]$Text) {
    $newline = $Text.IndexOf("`n")
    if ($newline -lt 0) { return '' }
    return $Text.Substring($newline + 1)
}

# THIRD-PARTY-LICENSES.txt: the About page's text (crates/cg-ffi/src/about.rs licenses(): both lists
# without their first line, "\n" between them), then the Wintun section.
function Get-ThirdPartyText([string]$WindowsRust, [string]$NugetNotices) {
    return (Remove-FirstLine $WindowsRust) + "`n" + (Remove-FirstLine $NugetNotices) + $WintunSection
}

# Strict UTF-8 (a file that is not UTF-8 fails), without a BOM on writing.
function Read-Utf8Text([string]$Path) {
    $utf8 = New-Object Text.UTF8Encoding($false, $true)
    return $utf8.GetString([IO.File]::ReadAllBytes($Path))
}
function Write-Utf8Text([string]$Path, [string]$Text) {
    [IO.File]::WriteAllBytes($Path, (New-Object Text.UTF8Encoding($false, $true)).GetBytes($Text))
}

# Writes THIRD-PARTY-LICENSES.txt from the repository's committed lists, the same files cg-ffi embeds
# (packaging/licenses/windows-rust.txt and nuget-notices.txt).
function Write-ThirdPartyLicenses([string]$Root, [string]$Dest) {
    $rust = Read-Utf8Text (Join-Path $Root 'packaging\licenses\windows-rust.txt')
    $nuget = Read-Utf8Text (Join-Path $Root 'packaging\licenses\nuget-notices.txt')
    Write-Utf8Text $Dest (Get-ThirdPartyText $rust $nuget)
}

# A file's path relative to $Base (a full path without a trailing backslash), backslash separated.
function Get-RelativePath([string]$Base, [string]$FullName) {
    return $FullName.Substring($Base.Length).TrimStart('\')
}

function Test-PackageLayout([string]$Dir, [string]$Arch) {
    if (-not $PeMachines.ContainsKey($Arch)) { throw "unknown architecture '$Arch' (x64 or arm64)" }
    $problems = New-Object System.Collections.Generic.List[string]
    $base = (Get-Item -LiteralPath $Dir).FullName.TrimEnd('\')
    foreach ($rel in $RequiredFiles) {
        if (-not (Test-Path -LiteralPath (Join-Path $base $rel) -PathType Leaf)) { $problems.Add("missing: $rel") }
    }
    # One problem per file: a pdb or an excluded DLL is reported as such and not checked further.
    $reported = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
    $expected = [uint16]$PeMachines[$Arch]
    $files = @(Get-ChildItem -LiteralPath $base -Recurse -File -Force | Sort-Object FullName)
    foreach ($file in $files) {
        $rel = Get-RelativePath $base $file.FullName
        if ($file.Name -like '*.pdb') { $problems.Add("pdb: $rel"); $null = $reported.Add($rel); continue }
        $unwanted = $false
        foreach ($pattern in $UnwantedPatterns) {
            if ($file.Name -like $pattern) { $unwanted = $true; break }
        }
        if ($unwanted) { $problems.Add("unwanted: $rel"); $null = $reported.Add($rel); continue }
        # Every program in the package, ours and the runtimes', is for the package architecture.
        if ($file.Extension -eq '.exe' -or $file.Extension -eq '.dll') {
            try {
                $machine = Get-PeMachine $file.FullName
            } catch {
                $problems.Add("not a PE: $rel")
                continue
            }
            if ($machine -ne $expected) {
                $problems.Add(('machine: {0} is 0x{1:X4}, expected 0x{2:X4}' -f $rel, $machine, $expected))
            }
        }
    }
    # Nothing at the top level beyond $TopLevel ('app' a directory, the rest files).
    foreach ($entry in @(Get-ChildItem -LiteralPath $base -Force | Sort-Object Name)) {
        if ($TopLevel -contains $entry.Name) {
            if ([bool]$entry.PSIsContainer -ne ($entry.Name -eq 'app')) { $problems.Add("unexpected: $($entry.Name)") }
            continue
        }
        if (-not $reported.Contains($entry.Name)) { $problems.Add("unexpected: $($entry.Name)") }
    }
    # As check-windows-core.ps1 -Publish: ASCII, and UTF-16 read from both byte alignments.
    $native = Join-Path $base 'app\cellglide_native.dll'
    if (Test-Path -LiteralPath $native -PathType Leaf) {
        $bytes = [IO.File]::ReadAllBytes($native)
        $texts = @([Text.Encoding]::ASCII.GetString($bytes), [Text.Encoding]::Unicode.GetString($bytes))
        if ($bytes.Length -gt 1) { $texts += [Text.Encoding]::Unicode.GetString($bytes, 1, $bytes.Length - 1) }
        foreach ($text in $texts) {
            if ($text.IndexOf($DiagOverride, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
                $problems.Add('diag override: app\cellglide_native.dll')
                break
            }
        }
    }
    return [string[]]$problems
}

# Runs a native command; a non-zero exit code stops the build.
function Invoke-Native([string]$Name, [scriptblock]$Command) {
    Write-Host "==> $Name"
    & $Command
    if ($LASTEXITCODE -ne 0) { throw "$Name failed (exit code $LASTEXITCODE)" }
}

# A zip of $Source's files, in ordinal path order, entry names separated by '/'. (Windows
# PowerShell's ZipFile.CreateFromDirectory writes '\', which other unzip tools take as part of the
# file name, so the entries are added one by one.)
function New-Zip([string]$Source, [string]$Zip) {
    if (Test-Path -LiteralPath $Zip) { Remove-Item -LiteralPath $Zip -Force }
    $base = (Get-Item -LiteralPath $Source).FullName.TrimEnd('\')
    $files = [string[]]@(Get-ChildItem -LiteralPath $base -Recurse -File -Force | ForEach-Object { $_.FullName })
    [Array]::Sort($files, [StringComparer]::Ordinal)
    $archive = [IO.Compression.ZipFile]::Open($Zip, [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($file in $files) {
            $name = (Get-RelativePath $base $file).Replace('\', '/')
            $null = [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $file, $name, [IO.Compression.CompressionLevel]::Optimal)
        }
    } finally {
        $archive.Dispose()
    }
}

function Get-ZipEntryNames([string]$Zip) {
    $archive = [IO.Compression.ZipFile]::OpenRead($Zip)
    try { return [string[]]@($archive.Entries | ForEach-Object { $_.FullName }) } finally { $archive.Dispose() }
}

# The build itself; a function, so that Import-ScriptFunctions (tests) loads none of its variables.
function Invoke-WindowsBuild {
    $Root = Split-Path -Parent $PSScriptRoot
    . (Join-Path $PSScriptRoot 'dev-env.ps1')
    $env:DOTNET_CLI_TELEMETRY_OPTOUT = '1'
    $env:DOTNET_NOLOGO = '1'
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem

    $Version = Get-WorkspaceVersion (Join-Path $Root 'Cargo.toml')
    $Triple = Get-RustTarget $Arch
    $Rid = "win-$Arch"
    if (-not $OutDir) { $OutDir = Join-Path $Root "dist\windows\$Version" }
    if (-not [IO.Path]::IsPathRooted($OutDir)) { $OutDir = Join-Path (Get-Location).Path $OutDir }
    $OutDir = [IO.Path]::GetFullPath($OutDir)
    $ZipName = "CellGlide-$Version-windows-$Arch.zip"
    $SymbolsName = "CellGlide-$Version-windows-$Arch-symbols.zip"
    $SumsName = "SHA256SUMS.$Arch"
    $RustOut = Join-Path $Root "target\$Triple\release"
    $Staging = Join-Path $Root "target\windows-package\$Arch"
    $SymbolsStaging = Join-Path $Root "target\windows-package\$Arch-symbols"
    $Uninstall = Join-Path $Root 'packaging\windows\uninstall.ps1'
    $License = Join-Path $Root 'LICENSE'

    Write-Host "CellGlide $Version for $Arch ($Triple, $Rid) -> $OutDir"
    foreach ($needed in $Uninstall, $License) {
        if (-not (Test-Path -LiteralPath $needed -PathType Leaf)) { throw "missing: $needed (the package needs it)" }
    }

    # A failed build must not leave a new zip beside an old SHA256SUMS (or the reverse): the three
    # outputs go first; the zip is written under a temporary name and moved into place once checked.
    foreach ($name in $ZipName, $SumsName, $SymbolsName, "$ZipName.partial", "$SymbolsName.partial") {
        $old = Join-Path $OutDir $name
        if (Test-Path -LiteralPath $old) { Remove-Item -LiteralPath $old -Force }
    }

    Push-Location $Root
    try {
        # cargo reruns build.rs (which rewrites NativeMethods.g.cs) only when its inputs change; a new
        # modification time (content untouched) makes it run, so the check below sees what it writes.
        (Get-Item (Join-Path $Root 'crates\cg-ffi\build.rs')).LastWriteTime = Get-Date
        Invoke-Native "cargo build --release --target $Triple" {
            cargo build --release --locked --target $Triple -p cg-service -p cg-cli -p cg-ffi
        }
        Invoke-Native 'NativeMethods.g.cs matches the committed copy' {
            git diff --exit-code HEAD -- windows/CellGlide.Core/Native/NativeMethods.g.cs
        }

        Push-Location (Join-Path $Root 'windows')
        try {
            $nativeDir = "..\target\$Triple\release\"
            $publishDir = (dotnet msbuild CellGlide.App -nologo -getProperty:PublishDir -p:Configuration=Release "-p:RuntimeIdentifier=$Rid" | Out-String).Trim()
            if ($LASTEXITCODE -ne 0 -or -not $publishDir) { throw "could not read CellGlide.App's PublishDir for Release/$Rid (got '$publishDir')" }
            if (-not [IO.Path]::IsPathRooted($publishDir)) { $publishDir = Join-Path (Join-Path $Root 'windows\CellGlide.App') $publishDir }
            $publishDir = [IO.Path]::GetFullPath($publishDir)
            # An earlier publish must not leave files in this package.
            if (Test-Path -LiteralPath $publishDir) { Remove-Item -LiteralPath $publishDir -Recurse -Force }
            Invoke-Native "dotnet publish CellGlide.App -c Release -r $Rid" {
                dotnet publish CellGlide.App -c Release -r $Rid "-p:CellGlideNativeDir=$nativeDir" "-p:Version=$Version" -p:RestoreLockedMode=true
            }
        } finally {
            Pop-Location
        }
        $publishedDll = (Get-FileHash -LiteralPath (Join-Path $publishDir 'cellglide_native.dll') -Algorithm SHA256).Hash
        $builtDll = (Get-FileHash -LiteralPath (Join-Path $RustOut 'cellglide_native.dll') -Algorithm SHA256).Hash
        if ($publishedDll -ne $builtDll) { throw "the published cellglide_native.dll ($publishedDll) is not $RustOut's ($builtDll)" }

        Write-Host "==> staging $Staging"
        foreach ($dir in $Staging, $SymbolsStaging) {
            if (Test-Path -LiteralPath $dir) { Remove-Item -LiteralPath $dir -Recurse -Force }
            $null = New-Item -ItemType Directory -Force -Path $dir
        }
        Invoke-Native "fetch-wintun.ps1 -Arch $Arch" {
            powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'fetch-wintun.ps1') -Arch $Arch -Dest $Staging
        }
        Copy-Item -LiteralPath (Join-Path $RustOut 'cellglide-service.exe') -Destination $Staging
        Copy-Item -LiteralPath (Join-Path $RustOut 'cellglide.exe') -Destination $Staging
        Copy-Item -LiteralPath $publishDir.TrimEnd('\') -Destination (Join-Path $Staging 'app') -Recurse
        Get-ChildItem -LiteralPath (Join-Path $Staging 'app') -Recurse -File -Filter '*.pdb' | Remove-Item -Force
        Write-ThirdPartyLicenses $Root (Join-Path $Staging 'THIRD-PARTY-LICENSES.txt')
        Copy-Item -LiteralPath $License -Destination (Join-Path $Staging 'LICENSE.txt')
        Copy-Item -LiteralPath $Uninstall -Destination (Join-Path $Staging 'uninstall.ps1')
        Write-Utf8Text (Join-Path $Staging 'version.txt') "$Version`n"

        $problems = @(Test-PackageLayout $Staging $Arch)
        if ($problems.Count -gt 0) {
            throw "the package layout has problems:`n  $($problems -join "`n  ")"
        }
        Write-Host '==> package layout OK'

        $null = New-Item -ItemType Directory -Force -Path $OutDir
        $zip = Join-Path $OutDir $ZipName
        $partial = "$zip.partial"
        New-Zip $Staging $partial
        $entries = Get-ZipEntryNames $partial
        $bad = @($entries | Where-Object { $_ -like '*.pdb' -or $_.Contains('\') })
        if ($bad.Count -gt 0) { throw "$ZipName has entries it must not have: $($bad -join ', ')" }
        $hash = (Get-FileHash -LiteralPath $partial -Algorithm SHA256).Hash.ToLowerInvariant()
        Move-Item -LiteralPath $partial -Destination $zip
        Write-Utf8Text (Join-Path $OutDir $SumsName) "$hash  $ZipName`n"
        Write-Host "==> $zip ($((Get-Item -LiteralPath $zip).Length) bytes, $($entries.Count) entries)"

        # Symbols: the publish folder's .pdb files (under app\) and the three Rust binaries' .pdb files.
        foreach ($pdb in Get-ChildItem -LiteralPath $publishDir -Recurse -File -Filter '*.pdb') {
            $dest = Join-Path (Join-Path $SymbolsStaging 'app') (Get-RelativePath $publishDir.TrimEnd('\') $pdb.FullName)
            $null = New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dest)
            Copy-Item -LiteralPath $pdb.FullName -Destination $dest
        }
        foreach ($name in 'cellglide_service.pdb', 'cellglide.pdb', 'cellglide_native.pdb') {
            $pdb = Join-Path $RustOut $name
            if (-not (Test-Path -LiteralPath $pdb -PathType Leaf)) { throw "missing: $pdb" }
            Copy-Item -LiteralPath $pdb -Destination $SymbolsStaging
        }
        $symbols = Join-Path $OutDir $SymbolsName
        New-Zip $SymbolsStaging "$symbols.partial"
        Move-Item -LiteralPath "$symbols.partial" -Destination $symbols
        Write-Host "==> $symbols ($((Get-Item -LiteralPath $symbols).Length) bytes)"
        Write-Host "==> $(Join-Path $OutDir $SumsName): $hash  $ZipName"
    } finally {
        Pop-Location
    }
    Write-Host "OK: CellGlide $Version for $Arch packaged in $OutDir"
}

Invoke-WindowsBuild
