# **The formatter differential, as one runnable command.**
#
# Runs `crates/cpp_formatter/tests/against_clang_format.rs` over every corpus we compare against, for the naming
# styles and for the options the project tracks, and prints a markdown table: identical files, changed lines (a
# longest-common-subsequence diff), and the spacing-only column — the same text clang-format already laid out, fed
# back through `cppfmt`.
#
#     pwsh -File scripts/formatter-diff.ps1                       # every corpus, the three styles
#     pwsh -File scripts/formatter-diff.ps1 -Options              # plus the per-option matrix
#     pwsh -File scripts/formatter-diff.ps1 -Corpus EmmyLua       # one corpus
#
# The oracle must be set: `$env:CPPLS_CLANG_FORMAT` to a clang-format binary (23.1.5 is what the documented numbers
# were measured with). The harness prints the oracle's own version with its numbers, so a run says what it measured.

param(
    [string]$Clang = $env:CPPLS_CLANG_FORMAT,
    [string]$Corpus = '',
    [int]$Limit = 40,
    [switch]$Options
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$corpora = [ordered]@{
    'EmmyLua'        = 'E:\EmmyLuaCodeStyle\CodeFormatCore\src'
    'Lua 5.5'        = 'C:\Users\zc\Desktop\lua-5.5.0\src'
    'nlohmann json'  = 'E:\corpora\json\include'
    'cpp_ls fixtures' = 'E:\cpp_ls\crates\cpp_parser\tests'
}
$styles = @('LLVM', 'Google', 'Microsoft')
$option_cases = @(
    @{ Label = 'IndentWidth: 4'; Style = '{BasedOnStyle: LLVM, IndentWidth: 4}' },
    @{ Label = 'UseTab: ForIndentation'; Style = '{BasedOnStyle: LLVM, UseTab: ForIndentation}' },
    @{ Label = 'BreakBeforeBraces: Allman'; Style = '{BasedOnStyle: LLVM, BreakBeforeBraces: Allman}' },
    @{ Label = 'AllowShortFunctionsOnASingleLine: None'; Style = '{BasedOnStyle: LLVM, AllowShortFunctionsOnASingleLine: None}' },
    @{ Label = 'ColumnLimit: 0'; Style = '{BasedOnStyle: LLVM, ColumnLimit: 0}' }
)

if (-not $Clang) {
    throw 'set CPPLS_CLANG_FORMAT to the clang-format binary to compare against'
}
if (-not (Test-Path $Clang)) {
    throw "clang-format not found: $Clang"
}

function Measure-One([string]$CorpusPath, [string]$Style) {
    $env:CPPLS_CLANG_FORMAT = $Clang
    $env:CPPLS_FMT_CORPUS = $CorpusPath
    $env:CPPLS_FMT_STYLE = $Style
    $env:CPPLS_FMT_LIMIT = "$Limit"
    $output = & cargo test --release -p cpp_formatter --test against_clang_format -- --ignored --nocapture 2>&1
    $text = ($output | Out-String)
    $identical = [regex]::Match($text, '\[diff\] style (.+): (\d+) of (\d+) file\(s\) identical, \d+ could not be formatted, (\d+) of (\d+)')
    $spacing = [regex]::Match($text, '\[diff\] style (.+): (\d+) of (\d+) file\(s\) match on text clang-format already laid out \((\d+) differing')
    if (-not $identical.Success -or -not $spacing.Success) {
        return $null
    }
    # The style is capture 1 (it can contain `: `, so it is greedy); the numbers follow it.
    [pscustomobject]@{
        Identical = "$($identical.Groups[2].Value) / $($identical.Groups[3].Value)"
        Changed   = "$($identical.Groups[4].Value) / $($identical.Groups[5].Value)"
        Spacing   = "$($spacing.Groups[2].Value) / $($spacing.Groups[3].Value) files, $($spacing.Groups[4].Value) lines"
    }
}

$selected = $corpora.Keys
if ($Corpus) {
    $selected = $corpora.Keys | Where-Object { $_ -eq $Corpus }
    if (-not $selected) { throw "unknown corpus: $Corpus (one of $($corpora.Keys -join ', '))" }
}

Write-Output "Oracle: $(& $Clang --version | Select-Object -First 1)"
Write-Output ''
Write-Output '### The naming styles, over every corpus'
Write-Output ''
Write-Output '| corpus | style | identical files | changed lines | spacing-only |'
Write-Output '| --- | --- | --- | --- | --- |'
foreach ($name in $selected) {
    $path = $corpora[$name]
    if (-not (Test-Path $path)) {
        Write-Output "| $name | — | *not on this machine* | — | — |"
        continue
    }
    foreach ($style in $styles) {
        $result = Measure-One $path $style
        if ($null -eq $result) {
            Write-Output "| $name | ``$style`` | *the run printed no summary* | — | — |"
            continue
        }
        Write-Output "| $name | ``$style`` | $($result.Identical) | $($result.Changed) | $($result.Spacing) |"
    }
}

if ($Options) {
    Write-Output ''
    Write-Output '### One option at a time (EmmyLua corpus, LLVM base)'
    Write-Output ''
    Write-Output '| option | identical files | changed lines | spacing-only |'
    Write-Output '| --- | --- | --- | --- |'
    $base = Measure-One $corpora['EmmyLua'] 'LLVM'
    Write-Output "| ``LLVM`` (base) | $($base.Identical) | $($base.Changed) | $($base.Spacing) |"
    foreach ($case in $option_cases) {
        $result = Measure-One $corpora['EmmyLua'] $case.Style
        if ($null -eq $result) { continue }
        Write-Output "| ``$($case.Label)`` | $($result.Identical) | $($result.Changed) | $($result.Spacing) |"
    }
}

Write-Output ''
Write-Output 'A file is "identical" only when the two outputs have no line in common to keep; `changed lines` counts the'
Write-Output 'lines that change when one text is turned into the other (a longest-common-subsequence diff).'
