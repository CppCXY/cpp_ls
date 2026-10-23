//! **How much work P2 has: the logical lines that a column limit would have to break.**
//!
//! The formatter today keeps the source's line breaks and decides only the columns (`docs/cpp-format.md` §十, P0b).
//! With `ColumnLimit: 0` — what the corpus asks for — that is the right answer. With a real limit it is not: a logical
//! line longer than the limit must be broken, and breaking is P2's job (the penalty search clang-format calls
//! `UnwrappedLineParser` + `BreakableToken`).
//!
//! This example measures the gap without pretending to close it:
//!
//! ```text
//! $ column_audit <clang-format.exe> <dir>...
//! printed: 111 line(s) over 80 in our output, 0 in clang-format's
//! logical: 78 line(s) that a breaker would have to split (ours), 0 (clang-format's)
//! ```
//!
//! "logical" means a run of tokens between statement boundaries (`;`, `{`, `}`) — the unit the breaker works on, and
//! the one that says whether a file needs *breaking* at all rather than merely *different columns*.

use std::path::Path;
use std::process::Command;

fn main() {
    let mut arguments = std::env::args().skip(1);
    let Some(clang_format) = arguments.next() else {
        eprintln!("usage: column_audit <clang-format.exe> <file-or-directory>...");
        std::process::exit(2);
    };
    let roots: Vec<String> = arguments.collect();
    if roots.is_empty() {
        eprintln!("column_audit: no files or directories");
        std::process::exit(2);
    }

    let limit = 80usize;
    let style = cpp_formatter::config::parse(
        "BasedOnStyle: LLVM\nColumnLimit: 80\nIndentWidth: 4\n",
        None,
    )
    .expect("the measuring style parses");

    let mut files: Vec<String> = Vec::new();
    for root in &roots {
        let path = Path::new(root);
        if path.is_dir() {
            collect(path, &mut files);
        } else {
            files.push(root.clone());
        }
    }
    files.sort();

    let mut printed_ours = 0usize;
    let mut printed_theirs = 0usize;
    let mut logical_ours = 0usize;
    let mut breakable = 0usize;
    let mut still_over = 0usize;


    for file in &files {
        let Ok(source) = std::fs::read_to_string(file) else {
            continue;
        };
        // **Both sides under the same limit**: the corpus sets `ColumnLimit: 0`, which makes the measurement
        // meaningless (nothing is supposed to break), so the style is built here with an explicit limit.
        let ours = cpp_formatter::format(&source, &style);
        let Some(theirs) = run(&clang_format, file) else {
            continue;
        };
        printed_ours += ours.lines().filter(|line| width(line) > limit).count();
        printed_theirs += theirs.lines().filter(|line| width(line) > limit).count();
        logical_ours += too_wide_logical_lines(&source, &style);
        let (fits, overflow) = what_a_breaker_would_do(&source, &style);
        breakable += fits;
        still_over += overflow;

    }

    println!(
        "printed: {printed_ours} line(s) over {limit} in our output, {printed_theirs} in clang-format's"
    );
    println!(
        "logical: {logical_ours} logical line(s) exceed the limit — the units a breaker has to split"
    );
    println!(
        "breaker: {breakable} of them can be laid out under the limit by the search in `breaks::break_line`, \
         {still_over} would still overflow"
    );
}

/// The width of a line as a terminal shows it (tabs to the next stop, wide characters two columns).
fn width(line: &str) -> usize {
    let mut column = 0usize;
    for character in line.chars() {
        column += match character {
            '\t' => 8 - column % 8,
            _ => {
                let code = character as u32;
                if matches!(code, 0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0x4E00..=0x9FFF
                    | 0xFF00..=0xFF60)
                {
                    2
                } else {
                    1
                }
            }
        };
    }
    column
}

/// **The logical lines that would not fit**, measured with the real splitter (`cpp_formatter::breaks`): each one's
/// printed width is its tokens plus the gaps between them, and one that overflows the limit is one the breaker would
/// have to split.
fn too_wide_logical_lines(source: &str, style: &cpp_formatter::config::Style) -> usize {
    let stream = cpp_formatter::tokens::stream(source);
    let roles = cpp_formatter::roles::of(source);
    let mut over = 0usize;
    for line in cpp_formatter::breaks::logical_lines(&stream) {
        let mut width = 0usize;
        for index in line.clone() {
            if index > line.start {
                width += cpp_formatter::space::between(&stream, index, &roles, style);
            }
            width += stream.tokens[index].text.chars().count();
        }
        if width > 80 {
            over += 1;
        }
    }
    over
}
/// **What the break search would do with this file**: the over-long logical lines it can split so that every piece
/// fits, and the ones where even the search leaves a piece too long (a single token wider than the limit, say).
fn what_a_breaker_would_do(source: &str, style: &cpp_formatter::config::Style) -> (usize, usize) {
    use cpp_formatter::options::StyleOption;
    let limit = style.count(StyleOption::ColumnLimit);
    let stream = cpp_formatter::tokens::stream(source);
    let roles = cpp_formatter::roles::of(source);
    let mut fits = 0usize;
    let mut overflow = 0usize;
    for line in cpp_formatter::breaks::logical_lines(&stream) {
        let mut width = 0usize;
        for index in line.clone() {
            if index > line.start {
                width += cpp_formatter::space::between(&stream, index, &roles, style);
            }
            width += stream.tokens[index].text.chars().count();
        }
        if width <= limit {
            continue;
        }
        match cpp_formatter::breaks::break_line(&stream, &roles, style, &line, 0) {
            Some(breaks) => {
                let mut worst = 0usize;
                let mut start = line.start;
                for at in breaks.iter().copied().chain(std::iter::once(line.end)) {
                    let mut piece = 0usize;
                    for index in start..at {
                        if index > start {
                            piece += cpp_formatter::space::between(&stream, index, &roles, style);
                        }
                        piece += stream.tokens[index].text.chars().count();
                    }
                    worst = worst.max(piece);
                    start = at;
                }
                if worst > limit {
                    overflow += 1;
                } else {
                    fits += 1;
                }
            }
            None => overflow += 1,
        }
    }
    (fits, overflow)
}

fn style_for(path: &Path) -> cpp_formatter::config::Style {
    let mut directory = path.parent().map(Path::to_path_buf);
    while let Some(current) = directory {
        let candidate = current.join(".clang-format");
        if let Ok(text) = std::fs::read_to_string(&candidate)
            && let Ok(style) = cpp_formatter::config::parse(&text, None)
        {
            return style;
        }
        directory = current.parent().map(Path::to_path_buf);
    }
    cpp_formatter::config::Style::default_style()
}

fn run(clang_format: &str, file: &str) -> Option<String> {
    let output = Command::new(clang_format)
        .args(["-style={BasedOnStyle: LLVM, ColumnLimit: 80, IndentWidth: 4}", file])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn collect(root: &Path, files: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
        if name.starts_with(".cppls") || name == "3rd" || name.starts_with("build") {
            continue;
        }
        if path.is_dir() {
            collect(&path, files);
        } else if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("c" | "cc" | "cpp" | "cxx" | "h" | "hpp" | "hh")
        ) {
            files.push(path.to_string_lossy().into_owned());
        }
    }
}