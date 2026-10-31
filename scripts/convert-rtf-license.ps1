# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\convert-rtf-license.ps1 -In <file.rtf> -Out <file.txt> -Source <text>
#
# Converts a license that is published only as RTF into the plain text kept in
# packaging/licenses/nuget-texts/ (used once for Microsoft.Windows.SDK.NET.Ref, whose .nuspec
# licenseUrl https://aka.ms/WinSDKLicenseURL is an .rtf). Steps, so the conversion can be redone:
#   1. a WinForms RichTextBox loads the RTF and its .Text is taken (list numbers such as "1." and
#      "a." are rendered by RichTextBox, as Word shows them);
#   2. CRLF and CR become LF, blank lines at either end are dropped, and the text ends with one LF;
#   3. hyperlinks: RichTextBox writes a HYPERLINK field as "<target>" when its visible text is the
#      target itself, and as "visible <target>" otherwise. Each is put back to its visible text alone
#      (the field's fldrslt), so the result reads as the document does. A field that cannot be
#      found in the text this way makes the script fail.
# The output's first line is "Source: <Source>", then a blank line, then the text.
param(
    [Parameter(Mandatory)][string]$In,
    [Parameter(Mandatory)][string]$Out,
    [Parameter(Mandatory)][string]$Source
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms

$rtf = [IO.File]::ReadAllText($In, [Text.Encoding]::GetEncoding(28591))
$box = New-Object System.Windows.Forms.RichTextBox
$box.Rtf = $rtf
$text = $box.Text.Replace("`r`n", "`n").Replace("`r", "`n").Trim("`n") + "`n"
$box.Dispose()

# The plain text of an RTF fragment: control words, \'xx escapes and braces removed.
function Get-RtfPlain([string]$s) {
    $s = [regex]::Replace($s, "\\'[0-9a-fA-F]{2}", '')
    $s = [regex]::Replace($s, '\\[a-zA-Z]+-?\d* ?', '')
    $s = $s.Replace('{', '').Replace('}', '').Replace("`r", '').Replace("`n", '')
    return $s
}

# The index of the brace that closes the group opening at $i.
function Get-GroupEnd([string]$s, [int]$i) {
    $depth = 0
    while ($true) {
        $c = $s[$i]
        if ($c -eq '\') { $i += 2; continue }
        if ($c -eq '{') { $depth++ }
        elseif ($c -eq '}') { $depth--; if ($depth -eq 0) { return $i } }
        $i++
    }
}

foreach ($m in [regex]::Matches($rtf, '\{\\field')) {
    $group = $rtf.Substring($m.Index, (Get-GroupEnd $rtf $m.Index) - $m.Index + 1)
    $at = $group.IndexOf('fldrslt')
    $inst = Get-RtfPlain $group.Substring(0, $at)
    if ($inst -notmatch 'HYPERLINK\s+"([^"]+)"') { continue }
    $target = $Matches[1]
    $visible = (Get-RtfPlain $group.Substring($at + 7)).Trim()
    if ($visible -ceq $target) { $from = "<$target>"; $to = $target }
    else { $from = "$visible <$target>"; $to = $visible }
    $where = $text.IndexOf($from, [StringComparison]::Ordinal)
    if ($where -lt 0) { throw "hyperlink '$target' (visible '$visible') not found in the converted text as '$from'" }
    $text = $text.Substring(0, $where) + $to + $text.Substring($where + $from.Length)
}

[IO.File]::WriteAllText($Out, "Source: $Source`n`n$text", (New-Object System.Text.UTF8Encoding($false)))
Write-Host "Wrote $Out"
