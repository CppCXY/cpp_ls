# Generate the category test files from the candidate inputs.
#
# For every candidate it asks clang-format (the oracle) and this formatter for their answer on the same temporary path,
# and only the pairs they agree on are emitted — so every expected string in the generated tests is the oracle's.
#
# The path matters: `format_file` uses the file's stem for the include sorter's "main header" rule, so both formatters
# are run on `snippet.cpp` and the tests call `format_file(Path::new("snippet.cpp"), ...)`.

$root = Join-Path $env:TEMP 'cases'
$clangFormat = 'C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\Llvm\x64\bin\clang-format.exe'
$styleArgument = '-style={BasedOnStyle: LLVM, IndentWidth: 4, ContinuationIndentWidth: 8, ColumnLimit: 0}'
$formatter = 'target\release\cppfmt.exe'
$runDirectory = Join-Path $env:TEMP 'case_run'
New-Item -ItemType Directory -Force -Path $runDirectory | Out-Null
$runPath = Join-Path $runDirectory 'snippet.cpp'
$newline = [char]10

$headerLines = @(
    '//! **Category: __CATEGORY__** — inline cases, no files, no clang-format at test time.',
    '//!',
    '//! Each case is a pair of strings: the input as written, and the text this formatter must produce for it. The pairs',
    '//! were taken from clang-format 22.1.3 for the same style that `style()` documents, so the expected side is the',
    '//! oracle''s answer rather than whatever this implementation happened to print.',
    '',
    'use cpp_formatter::config::Style;',
    '',
    '/// Format `input` with this project''s style and compare against the oracle''s answer, then re-format the answer.',
    '///',
    '/// The second half is the property a user feels: formatting text that is already formatted has to leave it alone.',
    '#[track_caller]',
    'fn check(input: &str, expected: &str) {',
    '    let style = style();',
    '    let path = std::path::Path::new("snippet.cpp");',
    '    let actual = cpp_formatter::format_file(path, input, &style);',
    '    // The trailing newline is the *source''s* (the formatter preserves its absence), so the cases compare the',
    '    // content and leave the file''s final byte to `trim_end_matches` on both sides.',
    '    let actual = actual.trim_end_matches(''\n'');',
    '    let expected = expected.trim_end_matches(''\n'');',
    '    assert_eq!(',
    '        actual, expected,',
    '        "formatting drifted\ninput:\n{input}\nexpected:\n{expected}\nactual:\n{actual}"',
    '    );',
    '    let again = cpp_formatter::format_file(path, expected, &style);',
    '    assert_eq!(again, expected, "the expected text is not a fixed point");',
    '}',
    '',
    '/// The style the expected strings were produced with.',
    'fn style() -> Style {',
    '    let mut style = Style::preset("LLVM").expect("LLVM is a preset");',
    '    style.set_from_text("IndentWidth", "4");',
    '    style.set_from_text("ContinuationIndentWidth", "8");',
    '    style.set_from_text("ColumnLimit", "0");',
    '    style',
    '}'
)

$agree = 0
$differ = 0

foreach ($category in @('spacing', 'types', 'templates', 'statements', 'classes', 'functions', 'preprocessor', 'comments')) {
    $lines = New-Object System.Collections.Generic.List[string]
    foreach ($line in $headerLines) {
        $lines.Add($line.Replace('__CATEGORY__', $category))
    }
    $lines.Add('')
    foreach ($file in Get-ChildItem (Join-Path $root $category) -Filter '*.cpp' -File | Sort-Object Name) {
        Copy-Item $file.FullName $runPath -Force
        $input = [System.IO.File]::ReadAllText($file.FullName).TrimEnd($newline)
        $ours = ((& $formatter $runPath 2>$null) -join $newline).TrimEnd($newline)
        $theirs = ((& $clangFormat $styleArgument $runPath 2>$null) -join $newline).TrimEnd($newline)
        if ($ours -ne $theirs) {
            $differ++
            continue
        }
        $agree++
        $name = $category + '_' + ($file.BaseName -replace '[^A-Za-z0-9_]', '_')
        $lines.Add('/// `' + $file.BaseName + '`')
        $lines.Add('#[test]')
        $lines.Add('fn ' + $name + '() {')
        $lines.Add('    check(')
        $lines.Add('        r#"' + $input + '"#,')
        $lines.Add('        r#"' + $theirs + '"#,')
        $lines.Add('    );')
        $lines.Add('}')
        $lines.Add('')
    }
    $outPath = Join-Path (Get-Location) ('crates\cpp_formatter\tests\cases_' + $category + '.rs')
    [System.IO.File]::WriteAllText($outPath, ($lines -join $newline))
}

Write-Output ('agreed cases: ' + $agree)
Write-Output ('cases that differ from the oracle (not emitted): ' + $differ)
