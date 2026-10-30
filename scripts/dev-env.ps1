# ç”¨æ³•ï¼š. scripts\dev-env.ps1
# æŠŠ cargo å’Œ VS BuildTools è‡ªå¸¦çš„ clangï¼ˆring ç¼–è¯‘ ARM64 éœ€è¦ï¼‰åŠ åˆ°å½“å‰ä¼šè¯ PATH å‰é¢ã€‚
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$llvmBin = $null
if (Test-Path $vswhere) {
    $vsPath = & $vswhere -products * -latest -property installationPath
    if ($vsPath) { $llvmBin = Join-Path $vsPath 'VC\Tools\Llvm\x64\bin' }
}
$dirs = @($cargoBin)
if ($llvmBin -and (Test-Path (Join-Path $llvmBin 'clang.exe'))) { $dirs += $llvmBin }
else { Write-Warning 'clang.exe not found; install VS component Microsoft.VisualStudio.Component.VC.Llvm.Clang (ARM64 builds need it)' }
$have = $env:PATH -split ';'
$new = $dirs | Where-Object { $have -notcontains $_ }
if ($new) { $env:PATH = ($new -join ';') + ';' + $env:PATH }
