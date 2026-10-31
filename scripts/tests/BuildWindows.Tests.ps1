# Pester 5 tests for scripts\build-windows.ps1 (run by scripts\test-windows-scripts.ps1).
# Only the script's functions and constants are loaded (Import-ScriptFunctions); nothing is built.
BeforeAll {
    . "$PSScriptRoot\Import-ScriptFunctions.ps1"; . Import-ScriptFunctions "$PSScriptRoot\..\build-windows.ps1"
    function New-FakePe([string]$Path, [uint16]$Machine) {
        $null = New-Item -ItemType Directory -Force (Split-Path $Path)
        $b = New-Object byte[] 512
        $b[0x3C] = 0x80; $b[0x80] = 0x50; $b[0x81] = 0x45          # e_lfanew -> 'PE\0\0'
        $b[0x84] = [byte]($Machine -band 0xFF); $b[0x85] = [byte]($Machine -shr 8)
        [IO.File]::WriteAllBytes($Path, $b)
    }
    function New-FakeLayout([string]$Dir, [uint16]$Machine) {
        foreach ($pe in 'cellglide-service.exe', 'cellglide.exe', 'wintun.dll', 'app\CellGlide.exe', 'app\cellglide_native.dll') { New-FakePe (Join-Path $Dir $pe) $Machine }
        foreach ($f in 'wintun-LICENSE.txt', 'THIRD-PARTY-LICENSES.txt', 'LICENSE.txt', 'uninstall.ps1', 'version.txt', 'app\CellGlide.pri') { Set-Content (Join-Path $Dir $f) 'x' }
    }
}
Describe 'build-windows' {
    It 'reads the workspace version' {
        $toml = Join-Path $TestDrive 'Cargo.toml'
        Set-Content $toml "[package]`nversion = `"9.9.9`"`n[workspace.package]`nversion = `"1.0.0`"`n"
        Get-WorkspaceVersion $toml | Should -Be '1.0.0'
    }
    It 'reads the repository version' {
        Get-WorkspaceVersion "$PSScriptRoot\..\..\Cargo.toml" | Should -Match '^\d+\.\d+\.\d+$'
    }
    It 'maps the architecture to the Rust target' {
        Get-RustTarget 'arm64' | Should -Be 'aarch64-pc-windows-msvc'
        Get-RustTarget 'x64' | Should -Be 'x86_64-pc-windows-msvc'
        { Get-RustTarget 'x86' } | Should -Throw
    }
    It 'reads the PE machine' {
        $f = Join-Path $TestDrive 'fake.dll'; New-FakePe $f 0xAA64
        Get-PeMachine $f | Should -Be 0xAA64
        $txt = Join-Path $TestDrive 'not-pe.dll'; Set-Content $txt 'hello'
        { Get-PeMachine $txt } | Should -Throw
    }
    It 'refuses a file whose e_lfanew does not lead to PE\0\0' {
        $f = Join-Path $TestDrive 'mz-only.dll'
        $b = New-Object byte[] 512; $b[0x3C] = 0x80; $b[0x80] = 0x4E; $b[0x81] = 0x45   # 'NE'
        [IO.File]::WriteAllBytes($f, $b)
        { Get-PeMachine $f } | Should -Throw
        $b[0x3C] = 0xFF; $b[0x3D] = 0xFF; $b[0x80] = 0x50                               # past the end
        [IO.File]::WriteAllBytes($f, $b)
        { Get-PeMachine $f } | Should -Throw
    }
    It 'builds the license file from both lists' {
        $t = Get-ThirdPartyText "# h1`nRust body`n" "# h2`nNuGet body`n"
        $t | Should -Match '^Rust body\n\nNuGet body\n'
        $t | Should -Match 'wintun-LICENSE\.txt'
        $t | Should -Not -Match '# h'
    }
    It 'ends the license file with the Wintun section' {
        $t = Get-ThirdPartyText "# h1`nR`n" "# h2`nN`n`n"
        $t | Should -BeExactly ("R`n`nN`n`n" + ('=' * 80) + "`nWintun`nwintun.dll is distributed under its own license; see wintun-LICENSE.txt next to it.`n")
    }
    It 'writes THIRD-PARTY-LICENSES.txt from the committed lists, as the About page assembles them' {
        $root = (Resolve-Path "$PSScriptRoot\..\..").Path
        $out = Join-Path $TestDrive 'THIRD-PARTY-LICENSES.txt'
        Write-ThirdPartyLicenses $root $out
        $utf8 = New-Object Text.UTF8Encoding($false, $true)
        $rust = $utf8.GetString([IO.File]::ReadAllBytes((Join-Path $root 'packaging\licenses\windows-rust.txt')))
        $nuget = $utf8.GetString([IO.File]::ReadAllBytes((Join-Path $root 'packaging\licenses\nuget-notices.txt')))
        $bytes = [IO.File]::ReadAllBytes($out)
        ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) | Should -BeFalse
        $text = $utf8.GetString($bytes)
        $text.Contains("`r") | Should -BeFalse
        # crates/cg-ffi/src/about.rs licenses(): both lists without their first line, "\n" between.
        $about = $rust.Substring($rust.IndexOf("`n") + 1) + "`n" + $nuget.Substring($nuget.IndexOf("`n") + 1)
        $text.StartsWith($about, [StringComparison]::Ordinal) | Should -BeTrue
        $text.Substring($about.Length) | Should -BeExactly $WintunSection
        $text | Should -Match 'MICROSOFT SOFTWARE LICENSE TERMS'
        $text | Should -Not -Match 'hev-socks5'
    }
    It 'accepts a complete layout' {
        $layout = Join-Path $TestDrive 'good'; New-FakeLayout $layout 0x8664
        Test-PackageLayout $layout 'x64' | Should -BeNullOrEmpty
    }
    It 'accepts a complete arm64 layout and refuses it as x64' {
        $layout = Join-Path $TestDrive 'arm'; New-FakeLayout $layout 0xAA64
        Test-PackageLayout $layout 'arm64' | Should -BeNullOrEmpty
        $problems = Test-PackageLayout $layout 'x64'
        $problems | Should -Contain 'machine: app\cellglide_native.dll is 0xAA64, expected 0x8664'
        @($problems).Count | Should -Be 5
    }
    It 'reports a missing file, a pdb, an excluded DLL, a wrong machine and the diag override' {
        $layout = Join-Path $TestDrive 'bad'; New-FakeLayout $layout 0x8664
        Remove-Item (Join-Path $layout 'version.txt')
        Set-Content (Join-Path $layout 'app\CellGlide.pdb') 'x'
        Set-Content (Join-Path $layout 'app\onnxruntime.dll') 'x'
        New-FakePe (Join-Path $layout 'wintun.dll') 0xAA64
        [IO.File]::WriteAllBytes((Join-Path $layout 'app\cellglide_native.dll'),
            [byte[]](Get-Content (Join-Path $layout 'cellglide.exe') -Encoding Byte) + [Text.Encoding]::ASCII.GetBytes('CELLGLIDE_DIAG_TARGET'))
        $problems = Test-PackageLayout $layout 'x64'
        $problems | Should -Contain 'missing: version.txt'
        $problems | Should -Contain 'pdb: app\CellGlide.pdb'
        $problems | Should -Contain 'unwanted: app\onnxruntime.dll'
        $problems | Should -Contain 'machine: wintun.dll is 0xAA64, expected 0x8664'
        $problems | Should -Contain 'diag override: app\cellglide_native.dll'
        @($problems).Count | Should -Be 5
    }
    It 'finds the diag override in UTF-16 at either alignment, and a file that is not a PE' {
        foreach ($pad in 0, 1) {
            $layout = Join-Path $TestDrive "utf16-$pad"; New-FakeLayout $layout 0x8664
            $dll = Join-Path $layout 'app\cellglide_native.dll'
            $bytes = [byte[]][IO.File]::ReadAllBytes($dll) + (New-Object byte[] $pad) + [Text.Encoding]::Unicode.GetBytes('cellglide_diag_target')
            [IO.File]::WriteAllBytes($dll, [byte[]]$bytes)
            Test-PackageLayout $layout 'x64' | Should -Be @('diag override: app\cellglide_native.dll')
        }
        $layout = Join-Path $TestDrive 'notpe'; New-FakeLayout $layout 0x8664
        Set-Content (Join-Path $layout 'cellglide.exe') 'not a program'
        Test-PackageLayout $layout 'x64' | Should -Be @('not a PE: cellglide.exe')
    }
    It 'zips with / between path parts, in ordinal order' {
        Add-Type -AssemblyName System.IO.Compression
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $src = Join-Path $TestDrive 'zipsrc'
        $null = New-Item -ItemType Directory -Force (Join-Path $src 'app\en-US')
        Set-Content (Join-Path $src 'app\en-US\a.mui') 'a'
        Set-Content (Join-Path $src 'b.txt') 'b'
        Set-Content (Join-Path $src 'B.txt2') 'B'
        $zip = Join-Path $TestDrive 'out.zip'
        Set-Content $zip 'an older file'
        New-Zip $src $zip
        Get-ZipEntryNames $zip | Should -Be @('B.txt2', 'app/en-US/a.mui', 'b.txt')
    }
    It 'finds a pdb or an excluded DLL at any depth' {
        $layout = Join-Path $TestDrive 'deep'; New-FakeLayout $layout 0x8664
        $null = New-Item -ItemType Directory -Force (Join-Path $layout 'app\Microsoft.UI.Xaml')
        Set-Content (Join-Path $layout 'app\Microsoft.UI.Xaml\DirectML.dll') 'x'
        Set-Content (Join-Path $layout 'cellglide.pdb') 'x'
        $problems = Test-PackageLayout $layout 'x64'
        $problems | Should -Contain 'unwanted: app\Microsoft.UI.Xaml\DirectML.dll'
        $problems | Should -Contain 'pdb: cellglide.pdb'
        @($problems).Count | Should -Be 2
    }
}
