# Downloads the official signed Wintun 0.14.1 release, verifies its SHA256 and copies
# wintun.dll (for one architecture) plus its license next to the binaries that load it.
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\fetch-wintun.ps1 [-Arch x64|arm64] [-Dest <dir>...]
#
# The zip is kept in target\wintun\ and nothing is written to the repository.
# Default -Dest: target\debug, target\debug\deps, target\debug\examples.

param(
    [ValidateSet('x64', 'arm64')]
    [string]$Arch,
    [string[]]$Dest
)

$ErrorActionPreference = 'Stop'

$Version = '0.14.1'
$Url = "https://www.wintun.net/builds/wintun-$Version.zip"
# SHA2-256 published on https://www.wintun.net/ next to the download link.
$Sha256 = '07C256185D6EE3652E09FA55C0B673E2624B565E02C4B9091C79CA7D2F24EF51'

$Root = Split-Path -Parent $PSScriptRoot
$Work = Join-Path $Root 'target\wintun'

if (-not $Arch) {
    # PROCESSOR_ARCHITEW6432 is set when this shell runs emulated (x64 on ARM64).
    $native = $env:PROCESSOR_ARCHITEW6432
    if (-not $native) { $native = $env:PROCESSOR_ARCHITECTURE }
    if ($native -eq 'ARM64') { $Arch = 'arm64' } else { $Arch = 'x64' }
}
$ZipArch = @{ 'x64' = 'amd64'; 'arm64' = 'arm64' }[$Arch]

if (-not $Dest) {
    $Dest = @('target\debug', 'target\debug\deps', 'target\debug\examples')
}

New-Item -ItemType Directory -Force -Path $Work | Out-Null
$Zip = Join-Path $Work "wintun-$Version.zip"

function Test-ZipHash([string]$Path) {
    if (-not (Test-Path $Path)) { return $false }
    return (Get-FileHash -Algorithm SHA256 -Path $Path).Hash -eq $Sha256
}

if (-not (Test-ZipHash $Zip)) {
    Write-Host "Downloading $Url"
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $Zip
    if (-not (Test-ZipHash $Zip)) {
        $actual = (Get-FileHash -Algorithm SHA256 -Path $Zip).Hash
        Remove-Item -Force $Zip
        throw "SHA256 mismatch for wintun-$Version.zip: expected $Sha256, got $actual"
    }
}
Write-Host "SHA256 OK: $Sha256"

$Extract = Join-Path $Work "wintun-$Version"
if (Test-Path $Extract) { Remove-Item -Recurse -Force $Extract }
Expand-Archive -Path $Zip -DestinationPath $Extract

$Dll = Join-Path $Extract "wintun\bin\$ZipArch\wintun.dll"
$License = Join-Path $Extract 'wintun\LICENSE.txt'
foreach ($f in @($Dll, $License)) {
    if (-not (Test-Path $f)) { throw "Missing from the zip: $f" }
}

foreach ($d in $Dest) {
    if (-not [IO.Path]::IsPathRooted($d)) { $d = Join-Path $Root $d }
    New-Item -ItemType Directory -Force -Path $d | Out-Null
    Copy-Item -Force $Dll (Join-Path $d 'wintun.dll')
    Copy-Item -Force $License (Join-Path $d 'wintun-LICENSE.txt')
    Write-Host "Copied wintun.dll ($ZipArch) to $d"
}
