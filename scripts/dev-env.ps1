# 用法：. scripts\dev-env.ps1
# 把 cargo 和 VS BuildTools 自带的 clang（ring 编译 ARM64 需要）加到当前会话 PATH 前面。
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
$env:PATH = ($dirs -join ';') + ';' + $env:PATH
