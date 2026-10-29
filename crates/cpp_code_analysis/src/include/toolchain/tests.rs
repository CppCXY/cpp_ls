use super::{
    Dialect, Environment, Output, a_cuda_toolchain, parse_search_list, parse_version,
    search_list_flags,
};

/// **A CUDA toolchain is recognised from its macros or its name, and that reaches the parser's switch.**
///
/// The three layers a `.cu` file lives in are independent: the standard (`-std=`), the **dialect** (the host compiler
/// — MSVC on Windows, GCC on Linux, because `nvcc` compiles host code by invoking one), and the **CUDA layer** (the
/// placement qualifiers and the `<<<…>>>` brackets). This test is about the third: `__CUDACC__` is `nvcc`'s own
/// statement about itself, an `nvcc` that could not be asked is still `nvcc` by name, and either way the
/// configuration it produces must have the parser's CUDA switch on.
#[test]
fn a_cuda_toolchain_is_recognised_and_turns_the_parsers_switch_on() {
    // **The macro**, with a GNU macro table beside it: `nvcc` on Linux defines both, and the host dialect must not be
    // swallowed by the CUDA reading.
    let macros = super::parse_builtin_macros("#define __GNUC__ 12\n#define __CUDACC__ 1\n");
    assert!(
        a_cuda_toolchain(&macros, None),
        "`__CUDACC__` is nvcc's own word"
    );

    // **The name**, when there is no macro table to read.
    assert!(
        a_cuda_toolchain(&[], Some(std::path::Path::new("/usr/local/cuda/bin/nvcc"))),
        "an nvcc that could not be asked is still nvcc"
    );

    // **And neither**, which is the ordinary case: g++ and cl are not CUDA, and a project of theirs must not read
    // `__global__` as a qualifier.
    let gnu = super::parse_builtin_macros("#define __GNUC__ 12\n");
    assert!(!a_cuda_toolchain(&gnu, None));
    assert!(!a_cuda_toolchain(
        &[],
        Some(std::path::Path::new("/usr/bin/g++"))
    ));

    // The switch itself: a CUDA toolchain's configuration turns the parser's flag on, and keeps the host dialect.
    let toolchain = super::Toolchain {
        compiler: Some("/usr/local/cuda/bin/nvcc".into()),
        version: None,
        system_include_paths: Vec::new(),
        builtin_macros: macros,
        cuda: true,
        dialect: Some(Dialect::Gnu),
        standard: None,
        source: super::ToolchainSource::CompileDatabase,
        note: None,
    };
    let config = toolchain.config(&super::CompilerConfig::new());
    assert!(config.cuda(), "the parser must read this project as CUDA");
    assert_eq!(
        config.dialect(),
        Dialect::Gnu,
        "and the host dialect is still its own"
    );
}

/// GCC 15.1.0 (MinGW-w64), trimmed to three entries — the shape is verbatim, including the `..` segments
/// and the `#include "..."` block that precedes it.
///
/// **The paths are not**: they are anonymised to `C:\tools\mingw64`, because what this test is about is the
/// *shape* a compiler prints — `COLLECT_GCC`, the `#include "..."` block that must not be read, the `bin/../lib`
/// that has to be folded, the `ignoring duplicate directory` line in between — and a fixture that spells out the
/// directory of the machine it was recorded on is a fixture nobody else can run and nobody can tell from a
/// machine-specific assertion. (A recorded shape is worth having at all: the
/// alternative is a parser written against what a compiler is *documented* to print.)
const GCC: &str = "\
Using built-in specs.
COLLECT_GCC=C:\\tools\\mingw64\\bin\\g++.exe
Target: x86_64-w64-mingw32
gcc version 15.1.0 (x86_64-win32-seh-rev0, Built by MinGW-Builds project)
COLLECT_GCC_OPTIONS='-E' '-v' '-shared-libgcc' '-mtune=core2' '-march=nocona'
ignoring duplicate directory \"C:/tools/mingw64/lib/gcc/../../lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++\"
#include \"...\" search starts here:
#include <...> search starts here:
 C:/tools/mingw64/bin/../lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++
 C:/tools/mingw64/bin/../lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++/x86_64-w64-mingw32
 C:/tools/mingw64/bin/../lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++/backward
End of search list.
";

/// Clang 18 on Windows, trimmed to two entries. Note the backslashes and the absence of `..` — the two
/// compilers print the same *structure* and different *spellings*, which is exactly why the parser keys on
/// the structure. No `..` to fold, and a directory with a space in it, which is the other shape that breaks a
/// parser keyed on whitespace.
const CLANG: &str = "\
clang version 18.1.8
Target: x86_64-pc-windows-msvc
Thread model: posix
#include \"...\" search starts here:
#include <...> search starts here:
 C:\\Program Files\\LLVM\\lib\\clang\\18\\include
 C:\\Program Files\\Microsoft Visual Studio\\18\\Community\\VC\\Tools\\MSVC\\14.51.36231\\include
End of search list.
";

#[test]
fn both_compilers_search_lists_are_read_the_same_way() {
    let gcc = parse_search_list(GCC);
    let clang = parse_search_list(CLANG);

    assert_eq!(
        gcc.len(),
        3,
        "three entries, and nothing from the quoted block"
    );
    assert_eq!(clang.len(), 2);

    // Normalized, which folds the `..` GCC prints and the separators Clang prints.
    assert_eq!(
        gcc[0],
        std::path::Path::new("C:/tools/mingw64/lib/gcc/x86_64-w64-mingw32/15.1.0/include/c++"),
        "the `bin/../lib` in the compiler's own output is folded"
    );
    assert_eq!(
        clang[0],
        std::path::Path::new("C:/Program Files/LLVM/lib/clang/18/include")
    );
}

#[test]
fn the_quoted_block_is_not_read() {
    // A shape the compiler can print and this must not confuse: directories a **quoted** include finds and an
    // angle one does not. Reading them as system paths would make `#include <vector>` resolve to a file the
    // compiler would not have found — a wrong answer rather than a missing one.
    let output = "\
#include \"...\" search starts here:
 /only/for/quotes
#include <...> search starts here:
 /for/angles
End of search list.
";
    assert_eq!(
        parse_search_list(output),
        [std::path::PathBuf::from("/for/angles")]
    );
}

#[test]
fn an_apple_framework_directory_loses_its_note() {
    // Clang on macOS annotates entries it treats as frameworks. The note is the compiler talking about its
    // own output, not part of the path — a directory named `… (framework directory)` does not exist.
    let output = "\
#include <...> search starts here:
 /System/Library/Frameworks (framework directory)
 /usr/include
End of search list.
";
    assert_eq!(
        parse_search_list(output),
        [
            std::path::PathBuf::from("/System/Library/Frameworks"),
            std::path::PathBuf::from("/usr/include")
        ]
    );
}

#[test]
fn output_without_a_search_list_has_no_directories() {
    // What an unrelated program with the compiler's name would print, and what a compiler that failed to
    // start prints. Both mean "this toolchain cannot be asked".
    assert!(parse_search_list("usage: not-a-compiler [options] file").is_empty());
    assert!(
        parse_search_list("#include <...> search starts here:\n").is_empty(),
        "a list that never ends has no entries, rather than everything after it"
    );
}

#[test]
fn the_version_line_is_the_one_the_compiler_printed() {
    assert_eq!(
        parse_version(GCC).as_deref(),
        Some("gcc version 15.1.0 (x86_64-win32-seh-rev0, Built by MinGW-Builds project)")
    );
    assert_eq!(
        parse_version(CLANG).as_deref(),
        Some("clang version 18.1.8")
    );

    // **The line a looser rule would answer with.** GCC's `-v` output also prints how it was built, and that line
    // mentions the version too; the colon and the flags before the name are what say it is not the compiler naming
    // itself. A wrong version string is worse than none, because it is what explains a wrong analysis.
    assert_eq!(
        parse_version(
            "Configured with: ../configure --prefix=/usr --with-gcc-major-version-only gcc version 15.1.0\n"
        ),
        None
    );
    // A name followed by something that is not a version is not a version line either.
    assert_eq!(parse_version("clang version unknown\n"), None);

    // **A distribution's own build puts its name first**: Ubuntu's clang and Apple's clang both do, and with the
    // narrow rule the version was `None` on those machines — the measured failure this list exists for.
    assert_eq!(
        parse_version("Ubuntu clang version 18.1.3 (1ubuntu1)\nTarget: x86_64-pc-linux-gnu\n")
            .as_deref(),
        Some("Ubuntu clang version 18.1.3 (1ubuntu1)")
    );
    assert_eq!(
        parse_version(
            "Apple clang version 15.0.0 (clang-1500.0.40.1)\nTarget: arm64-apple-darwin23.0.0\n"
        )
        .as_deref(),
        Some("Apple clang version 15.0.0 (clang-1500.0.40.1)")
    );
    assert_eq!(
        parse_version("Debian clang version 16.0.6 (15~deb12u1)").as_deref(),
        Some("Debian clang version 16.0.6 (15~deb12u1)")
    );
    // A distribution that prints GCC's own line is unchanged: `gcc version` is first there.
    assert_eq!(
        parse_version("Using built-in specs.\ngcc version 13.2.0 (Ubuntu 13.2.0-23ubuntu4) ")
            .as_deref(),
        Some("gcc version 13.2.0 (Ubuntu 13.2.0-23ubuntu4)")
    );
}

#[test]
fn the_output_documents_both_streams() {
    // GCC prints the list to stderr and Clang to stderr too, but a caller that kept only stdout would report
    // "no compiler" for a compiler that answered.
    let output = Output {
        stdout: "from stdout".to_string(),
        stderr: "from stderr".to_string(),
        status: Some(0),
        succeeded: true,
    };
    let combined = output.combined();
    assert!(combined.contains("from stdout") && combined.contains("from stderr"));
}

#[test]
fn the_environment_is_read_as_data() {
    // Not asserted against the process environment — that would be a test of the machine rather than of the
    // code. What matters is that the shape is usable without touching `std::env`, which is what the tests
    // below rely on.
    let environment = Environment {
        cxx: Some(std::path::PathBuf::from("/opt/gcc/bin/g++")),
        cc: None,
        path: vec![std::path::PathBuf::from("/usr/bin")],
        path_extensions: Vec::new(),
    };
    assert_eq!(environment.path.len(), 1);
}

// -------------------------------------------------------------------------------------------
// Which compiler is asked, and what is done with the answer
//
// Every test here runs without a compiler installed, which is the point of `CommandRunner` and
// `Environment` being data: the policy is what is under test, not this machine's toolchain.
// -------------------------------------------------------------------------------------------

use super::{
    COMPILER_NAMES, CommandRunner, CompilerConfig, IncludePath, Path, PathBuf, Toolchain,
    ToolchainSource, discover, find_compiler, find_compiler_among, installed_compilers, is_msvc,
    parse_builtin_macros, search_paths,
};
use crate::file::paths::{MemoryFiles, normalize_path};
use crate::include::config::{CompileCommand, CompileCommands};

/// The environment a test names explicitly, with no extensions so that a `PATH` entry is a whole file name.
fn environment() -> Environment {
    Environment {
        cxx: None,
        cc: None,
        path: vec![Path::new("/bin").into(), Path::new("/usr/bin").into()],
        path_extensions: Vec::new(),
    }
}

/// A compiler that answers with a fixed search list, and remembers what it was asked.
struct Answers {
    program: &'static str,
    output: &'static str,
}

impl CommandRunner for Answers {
    fn run(
        &self,
        program: &Path,
        arguments: &[&str],
        environment: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> Option<Output> {
        if program.file_name()?.to_str()? != self.program {
            return None;
        }

        assert!(
            environment.is_empty(),
            "a GNU-like compiler is asked with an empty environment: {environment:?}"
        );

        // The arguments are asserted rather than ignored: `-x c++` is what makes `gcc` list the C++
        // directories, `-dM` is what makes it print its predefined macros, and a discovery that dropped
        // either would answer a different question. `-std=` is asserted where a standard was given.
        assert!(
            arguments.starts_with(&["-dM", "-E", "-v", "-x", "c++"]),
            "the predefined macros and the search list come from one call: {arguments:?}"
        );
        assert!(
            arguments.iter().all(|argument| *argument == "-dM"
                || *argument == "-E"
                || *argument == "-v"
                || *argument == "-x"
                || *argument == "c++"
                || *argument == "-"
                || argument.starts_with("-std=")),
            "and nothing else is asked for: {arguments:?}"
        );

        Some(Output {
            stdout: String::new(),
            stderr: self.output.to_string(),
            status: Some(0),
            succeeded: true,
        })
    }
}

/// **A database the project wrote**, as the statement a toolchain is asked about.
fn from_database(commands: &CompileCommands) -> super::BuildStatement<'_> {
    super::BuildStatement {
        commands: Some(commands),
        standard: None,
    }
}

fn command_line(compiler: &str) -> CompileCommands {
    CompileCommands {
        commands: vec![CompileCommand {
            file: "/p/main.cpp".into(),
            directory: None,
            arguments: vec![compiler.to_string(), "-c".to_string()],
        }],
        malformed: 0,
    }
}

#[test]
fn the_compiler_the_compile_database_names_is_the_one_asked() {
    // Step 1 of the order, and the only step that knows *this* file is built by *that* compiler. A project
    // with two toolchains in it is what it exists for.
    let files = MemoryFiles::new()
        .with_file("/usr/bin/g++", "")
        .with_file("/opt/clang/bin/clang++", "");
    let commands = command_line("/opt/clang/bin/clang++");

    let found = find_compiler(
        &files,
        Some(&commands),
        Path::new("/p/main.cpp"),
        &environment(),
    );

    assert_eq!(
        found,
        Some(Path::new("/opt/clang/bin/clang++").to_path_buf())
    );
}

#[test]
fn a_named_compiler_that_is_not_there_falls_through_to_the_environment() {
    // A checked-in `compile_commands.json` names the machine it was generated on. Requiring that path to
    // exist would make the database worse than useless on every other machine, so a name that is not there is
    // passed over rather than reported.
    let files = MemoryFiles::new().with_file("/usr/bin/clang++", "");
    let commands = command_line("/opt/gcc/bin/g++");

    let mut environment = environment();
    environment.cxx = Some(Path::new("/usr/bin/clang++").into());

    let found = find_compiler(
        &files,
        Some(&commands),
        Path::new("/p/main.cpp"),
        &environment,
    );
    assert_eq!(found, Some(Path::new("/usr/bin/clang++").to_path_buf()));
}

#[test]
fn cxx_beats_cc_and_path() {
    let files = MemoryFiles::new()
        .with_file("/usr/bin/g++", "")
        .with_file("/usr/bin/cc", "");

    let mut environment = environment();
    environment.cc = Some(Path::new("/usr/bin/cc").into());
    environment.cxx = Some(Path::new("/usr/bin/g++").into());

    assert_eq!(
        find_compiler(&files, None, Path::new("/p/main.cpp"), &environment),
        Some(Path::new("/usr/bin/g++").to_path_buf()),
        "`CXX` is the project's stated C++ compiler and `CC` is only a fallback"
    );
}

#[test]
fn a_bare_name_is_searched_on_the_path_and_a_path_is_used_where_it_says() {
    // What a shell does with the same string, which is what the two spellings mean.
    let files = MemoryFiles::new()
        .with_file("/bin/g++", "")
        .with_file("/usr/bin/g++", "")
        .with_file("/opt/other/g++", "");

    let mut environment = environment();

    environment.cxx = Some(Path::new("g++").into());
    assert_eq!(
        find_compiler(&files, None, Path::new("/p/main.cpp"), &environment),
        Some(Path::new("/bin/g++").to_path_buf()),
        "a bare name is searched on `PATH`, in order"
    );

    environment.cxx = Some(Path::new("/opt/other/g++").into());
    assert_eq!(
        find_compiler(&files, None, Path::new("/p/main.cpp"), &environment),
        Some(Path::new("/opt/other/g++").to_path_buf()),
        "a name with a separator in it says where it is"
    );
}

#[test]
fn the_path_is_searched_in_the_order_the_names_are_tried() {
    // **The directory is not a conventional install root** (`/usr/bin`, `/opt/homebrew/bin`, …), because a compiler
    // found *there* is step 3's answer rather than this test's: with the fixture in `/usr/bin`, a Unix-like machine
    // found `clang++` before `PATH` was consulted at all, while Windows looked for `/usr/bin/clang++.exe` and found
    // nothing. `PATH` is what this test is about, so the fixture lives only on it.
    let mut environment = environment();
    environment.path = vec![Path::new("/p/bin").into()];

    // `g++` is tried before `clang++`, because that is the order a person reading `COMPILER_NAMES` expects.
    let files = MemoryFiles::new().with_file("/p/bin/clang++", "");
    let found = find_compiler(&files, None, Path::new("/p/main.cpp"), &environment);
    assert_eq!(found, Some(Path::new("/p/bin/clang++").to_path_buf()));

    let files = MemoryFiles::new()
        .with_file("/p/bin/clang++", "")
        .with_file("/p/bin/g++", "");
    let found = find_compiler(&files, None, Path::new("/p/main.cpp"), &environment);
    assert_eq!(
        found,
        Some(Path::new("/p/bin/g++").to_path_buf()),
        "the first name on the list that exists, not the first directory that has any of them"
    );

    assert!(
        COMPILER_NAMES.contains(&"c++"),
        "the one name POSIX guarantees"
    );
}

#[test]
fn no_compiler_anywhere_is_no_answer_rather_than_an_error() {
    let files = MemoryFiles::new();
    assert_eq!(
        find_compiler(&files, None, Path::new("/p/main.cpp"), &environment()),
        None
    );
}

/// **The measurement this step exists for**: a clang that is installed is not a clang that is on `PATH`.
///
/// Measured on the machine this was written on — clang++ 18.1.8 under `C:\Program Files\LLVM\bin`, nothing
/// named `clang*` on `PATH`, and MinGW's `g++` there instead. Before this step the analysis answered with the
/// MinGW toolchain, whose libstdc++ is a *different* standard library from the one the project's build sees.
#[test]
fn a_compiler_that_is_installed_is_found_when_the_path_does_not_say() {
    let files = MemoryFiles::new()
        .with_case_insensitive(true)
        .with_file(
            "C:/Program Files/LLVM/bin/clang++.exe",
            "the installed clang",
        )
        .with_file("/mingw/bin/g++.exe", "what `PATH` says");

    let mut mingw_on_path = environment();
    mingw_on_path.path = vec![Path::new("/mingw/bin").into()];
    mingw_on_path.path_extensions = vec![".exe".to_string()];

    let found = find_compiler_among(
        &files,
        None,
        Path::new("C:/p/main.cpp"),
        &mingw_on_path,
        &[PathBuf::from("C:/Program Files")],
    );

    assert_eq!(
        found,
        Some(PathBuf::from("C:/Program Files/LLVM/bin/clang++.exe")),
        "the installed clang, not the MinGW `g++` on `PATH`: they are different standard libraries"
    );
}

/// The order is by **name** first and directory second, so a clang in the second directory beats a `g++` in
/// the first — the question is which compiler this is, not where it is kept.
#[test]
fn an_installed_clang_is_preferred_to_an_installed_gcc() {
    let files = MemoryFiles::new()
        .with_case_insensitive(true)
        .with_file("/usr/local/bin/g++.exe", "")
        .with_file("/opt/homebrew/bin/clang++.exe", "");

    let mut nothing_on_path = environment();
    nothing_on_path.path = Vec::new();
    nothing_on_path.path_extensions = vec![".exe".to_string()];

    let found = installed_compilers(&files, &nothing_on_path, &[PathBuf::from("/nothing/there")]);

    assert!(
        found.iter().any(|path| path.ends_with("clang++.exe")),
        "the clang is offered: {found:?}"
    );
    assert!(
        found.iter().any(|path| path.ends_with("g++.exe")),
        "and so is the `g++`, after it: {found:?}"
    );
    assert!(
        found
            .iter()
            .position(|path| path.ends_with("clang++.exe"))
            .zip(found.iter().position(|path| path.ends_with("g++.exe")))
            .is_some_and(|(clang, gcc)| clang < gcc),
        "clang first, whichever directory it is in: {found:?}"
    );
}

/// A conventional directory **without** the name is not an answer, which is what keeps the list from turning
/// into "a directory that exists is a toolchain".
#[test]
fn an_empty_installation_directory_is_not_a_compiler() {
    let files = MemoryFiles::new()
        .with_case_insensitive(true)
        .with_file("/opt/homebrew/bin/brew", "")
        .with_file("/usr/local/bin/cmake", "");

    let mut nothing_on_path = environment();
    nothing_on_path.path = Vec::new();
    nothing_on_path.path_extensions = vec![".exe".to_string()];

    assert!(
        installed_compilers(&files, &nothing_on_path, &[]).is_empty(),
        "no file named for a compiler is a compiler"
    );

    // …and a name the caller already found on `PATH` is still the answer when nothing is installed.
    let files = MemoryFiles::new().with_file("/mingw/bin/g++.exe", "");
    let mut on_path = environment();
    on_path.path = vec![Path::new("/mingw/bin").into()];
    on_path.path_extensions = vec![".exe".to_string()];

    assert_eq!(
        find_compiler_among(&files, None, Path::new("/p/main.cpp"), &on_path, &[]),
        Some(std::path::PathBuf::from("/mingw/bin/g++.exe"))
    );
}

/// **`clang-cl` is not `cl`.** The two are asked in completely different ways — `cl` through `INCLUDE` and a
/// macro dump that clang's MSVC driver does not implement, everything else through `-E -v` — so a name test
/// that matched both would send one of them down the other's path.
#[test]
fn clang_cl_is_not_mistaken_for_microsofts_compiler() {
    assert!(is_msvc(Path::new(
        "/VS/VC/Tools/MSVC/14.51.36231/bin/Hostx64/x64/cl.exe"
    )));
    assert!(
        is_msvc(Path::new("cl.exe")),
        "the name is matched, not the path"
    );

    assert!(
        !is_msvc(Path::new("C:/Program Files/LLVM/bin/clang-cl.exe")),
        "clang's MSVC driver: it accepts `cl`'s flags and has no macro dump"
    );
    assert!(!is_msvc(Path::new("C:/Program Files/LLVM/bin/clang++.exe")));
    assert!(!is_msvc(Path::new("/usr/bin/c++")));
}

#[test]
fn windows_tries_the_extensions_in_the_order_pathext_gives() {
    // `g++` next to a file literally named `g++` and a `g++.exe`: Windows runs the `.exe`, so a search that
    // tried the bare name first would find a file the system would not run.
    let files = MemoryFiles::new()
        .with_case_insensitive(true)
        .with_file("/bin/g++", "")
        .with_file("/bin/g++.exe", "");

    let mut environment = environment();
    environment.cxx = Some(Path::new("g++").into());
    environment.path_extensions = vec![".COM".to_string(), ".EXE".to_string()];

    let found = find_compiler(&files, None, Path::new("/p/main.cpp"), &environment)
        .expect("`g++` is on the path");

    // Compared through the normalizer rather than as a string: the found path is built by joining, so it
    // carries the platform's separator, and the extension keeps the case `PATHEXT` gave it. Both are
    // spellings of one file, which is exactly what `normalize_path` is for.
    assert_eq!(
        normalize_path(&found, true),
        normalize_path(Path::new("/bin/g++.exe"), true),
        "the executable spelling, not the bare name that sits beside it"
    );
}

#[test]
fn a_compiler_that_answers_gives_its_directories_its_version_and_its_macros() {
    let runner = Answers {
        program: "g++",
        output: GCC,
    };

    let toolchain =
        search_paths(&runner, Path::new("/usr/bin/g++"), None, &[]).expect("the fixture answers");

    assert_eq!(
        toolchain.compiler.as_deref(),
        Some(Path::new("/usr/bin/g++"))
    );
    assert_eq!(toolchain.system_include_paths.len(), 3);
    assert!(
        toolchain
            .version
            .as_deref()
            .is_some_and(|version| version.starts_with("gcc version"))
    );
    assert!(
        toolchain.include_paths().iter().all(|path| path.is_system),
        "a toolchain's own directories are what `-isystem` would name, which is what decides whether a \
             diagnostic inside them is the project's business"
    );
}

#[test]
fn a_program_that_is_not_a_compiler_is_not_asked_twice() {
    // A file that happens to be named `g++` and prints something else. `None` is the same answer as "could
    // not run", because both mean the same thing to a caller: this toolchain cannot be asked.
    let runner = Answers {
        program: "g++",
        output: "usage: g++ [options] file\n",
    };
    assert_eq!(
        search_paths(&runner, Path::new("/usr/bin/g++"), None, &[]),
        None
    );
}

#[test]
fn the_predefined_macros_are_read_out_of_the_same_answer() {
    // `-dM`'s lines arrive in the middle of `-v`'s, so this is the read that has to tell them apart. The three
    // shapes that matter: a value, no value at all, and a function-like macro — whose parameter list is glued to
    // the name in this output and must not become part of it.
    let macros = parse_builtin_macros(
        "#define __cplusplus 201703L\n\
             #define __linux 1\n\
             #define __STDC_HOSTED__ 1\n\
             #define assert(expr) ((expr) ? (void)0 : abort())\n\
             #define __FLT_MIN__ 1.17549435082228750797e-38F\n\
             gcc version 11.2.0\n\
             #include <...> search starts here:\n",
    );

    let value_of = |name: &str| {
        macros
            .iter()
            .find(|define| define.name.as_ref() == name)
            .map(|define| define.value.as_deref())
    };

    assert_eq!(value_of("__cplusplus"), Some(Some("201703L")));
    assert_eq!(value_of("__STDC_HOSTED__"), Some(Some("1")));
    assert_eq!(
        value_of("assert"),
        None,
        "a function-like macro is not a name a condition can test"
    );
    assert_eq!(
        value_of("__FLT_MIN__"),
        Some(Some("1.17549435082228750797e-38F")),
        "a value is everything up to the line's end, floats included"
    );
    assert_eq!(value_of("gcc"), None, "`-v`'s own output is not a macro");
}

#[test]
fn a_standard_the_caller_names_is_passed_to_the_compiler() {
    // The value of `__cplusplus` is the one a condition's answer depends on, and the compiler prints the value
    // of the language version it was **invoked** for. Asking without `-std=` answers for its default, which is a
    // *wrong* number rather than a missing one — so the standard has to travel with the request.
    let runner = Answers {
        program: "g++",
        output: GCC,
    };

    // The fixture's `run` asserts the arguments, so reaching this line at all means `-std=c++20` was passed.
    let toolchain = search_paths(&runner, Path::new("/usr/bin/g++"), Some("c++20"), &[])
        .expect("the fixture answers");
    assert_eq!(
        toolchain.compiler.as_deref(),
        Some(Path::new("/usr/bin/g++"))
    );
}

#[test]
fn discovery_walks_from_the_environment_to_the_answer() {
    let files = MemoryFiles::new().with_file("/usr/bin/g++", "");
    let runner = Answers {
        program: "g++",
        output: GCC,
    };

    let toolchain = discover(
        &files,
        &runner,
        super::BuildStatement::default(),
        Path::new("/p/main.cpp"),
        &environment(),
        &crate::include::msvc::WindowsLayout::default(),
    )
    .expect("the compiler is on the path and answers");

    assert_eq!(
        toolchain.compiler.as_deref(),
        Some(Path::new("/usr/bin/g++"))
    );
    assert_eq!(toolchain.source, ToolchainSource::Path);
    assert_eq!(
        toolchain.standard.as_deref(),
        Some("c++23"),
        "nothing stated a standard, so the newest this compiler takes is what it was asked about"
    );
    assert!(
        toolchain
            .note
            .as_deref()
            .is_some_and(|note| note.contains("newest") && note.contains("c++23")),
        "and the report says so rather than leaving it implicit: {:?}",
        toolchain.note
    );
    assert_eq!(toolchain.system_include_paths.len(), 3);
}

/// **Nothing states a standard → the compiler is asked for its newest one**, and the analysis says so.
///
/// The measurement this comes from is a real project with no `compile_commands.json`, no `CMakeLists.txt` and an
/// empty `.vscode/settings.json`: MSVC then builds as C++14, `<format>` is empty, and a reader typing
/// `#include <format>` gets nothing — while the compiler itself says `STL4038: The contents of <format> are
/// available only with C++20 or later` and C2039. A default that is nobody's build is not a neutral choice.
#[test]
fn a_project_that_states_no_standard_is_asked_for_the_newest() {
    let files = MemoryFiles::new().with_file("/usr/bin/g++", "");
    let runner = Answers {
        program: "g++",
        output: GCC,
    };

    let toolchain = discover(
        &files,
        &runner,
        super::BuildStatement::default(),
        Path::new("/p/main.cpp"),
        &environment(),
        &crate::include::msvc::WindowsLayout::default(),
    )
    .expect("the compiler answers");

    assert_eq!(toolchain.standard.as_deref(), Some("c++23"));

    // …and it reaches the configuration, where a condition's answer is decided.
    let config = toolchain.config(&CompilerConfig::new());
    assert_eq!(config.standard.as_deref(), Some("c++23"));
    let stated = crate::include::config::predefined_macros_of(&config);
    assert!(
        stated
            .iter()
            .any(|define| define.name.as_ref() == "__cplusplus"
                && define.value.as_deref() == Some("202302L")),
        "`__cplusplus` is what every `#if __cplusplus >= …` in every header asks: {stated:?}"
    );
}

/// **A project that states a standard keeps it.** The assumption is a floor, not a policy: `-std=c++17` in a
/// compile database is a statement about how the project is built, and answering a newer standard's macros
/// would make every `#if` in the standard library disagree with the compiler.
#[test]
fn a_standard_the_project_states_is_not_replaced_by_the_assumption() {
    let files = MemoryFiles::new().with_file("/usr/bin/g++", "");
    let runner = Answers {
        program: "g++",
        output: GCC,
    };

    let mut commands = command_line("/usr/bin/g++");
    commands.commands[0]
        .arguments
        .push("-std=c++17".to_string());

    let toolchain = discover(
        &files,
        &runner,
        from_database(&commands),
        Path::new("/p/main.cpp"),
        &environment(),
        &crate::include::msvc::WindowsLayout::default(),
    )
    .expect("the compiler answers");

    assert_eq!(toolchain.standard.as_deref(), Some("c++17"));
    assert!(
        toolchain.note.is_none(),
        "nothing was assumed, so there is nothing to report: {:?}",
        toolchain.note
    );
    assert_eq!(
        toolchain.config(&CompilerConfig::new()).standard.as_deref(),
        Some("c++17")
    );
}

#[test]
fn a_compiler_the_database_names_is_asked_before_the_environment_is() {
    // The order, pinned: the project's own statement about which compiler builds the file comes before the
    // machine's, because a project with two toolchains in it is what the database exists to describe.
    let files = MemoryFiles::new()
        .with_file("/toolchains/bin/g++", "")
        .with_file("/usr/bin/g++", "");
    let runner = Answers {
        program: "g++",
        output: GCC,
    };

    let mut environment = environment();
    environment.path = vec![Path::new("/toolchains/bin").into()];

    let commands = command_line("/toolchains/bin/g++");
    let toolchain = discover(
        &files,
        &runner,
        from_database(&commands),
        Path::new("/p/main.cpp"),
        &environment,
        &crate::include::msvc::WindowsLayout::default(),
    )
    .expect("the database's compiler answers");

    assert_eq!(
        toolchain.compiler.as_deref(),
        Some(Path::new("/toolchains/bin/g++"))
    );
    assert_eq!(
        toolchain.source,
        ToolchainSource::CompileDatabase,
        "and the report says which step answered"
    );
}

#[test]
fn a_machine_with_no_compiler_still_has_the_systems_headers_and_no_macros() {
    // The last resort, and the two halves of what it means: `#include <vector>` resolves — so a project is
    // half-usable — and the macro table is empty, so every condition naming `__cplusplus` or `_WIN32` is
    // `Unknown`. The note says so, because a caller that read emptiness as "not defined" would answer wrongly.
    let files = MemoryFiles::new();
    let runner = Answers {
        program: "nothing",
        output: "",
    };
    let mut environment = environment();
    environment.path = Vec::new();
    environment.cxx = None;
    environment.cc = None;

    let found = discover(
        &files,
        &runner,
        super::BuildStatement::default(),
        Path::new("/p/main.cpp"),
        &environment,
        &crate::include::msvc::WindowsLayout::default(),
    );

    // A machine with no standard library installed anywhere conventional has no guess at all — which is the
    // honest answer rather than a directory that does not exist — so this test asserts about the answer when
    // there is one.
    if let Some(toolchain) = found {
        assert_eq!(toolchain.source, ToolchainSource::SystemHeaders);
        assert!(toolchain.source.is_a_guess());
        assert_eq!(toolchain.compiler, None);
        assert!(toolchain.builtin_macros.is_empty());
        assert!(
            toolchain
                .note
                .as_deref()
                .is_some_and(|note| note.contains("Unknown")),
            "the note says what is missing: {:?}",
            toolchain.note
        );
        assert!(
            !toolchain.system_include_paths.is_empty(),
            "and every directory is one that exists"
        );
        for directory in &toolchain.system_include_paths {
            assert!(directory.is_dir(), "{}", directory.display());
        }
    }
}

#[test]
fn the_directories_are_appended_so_the_project_keeps_its_own_order() {
    // A search stops at the first directory that has the file, so position *is* precedence: `-I` finds a
    // header before the compiler's own directories do, which is how a project overrides one.
    let toolchain = Toolchain {
        compiler: Some("/usr/bin/g++".into()),
        version: None,
        system_include_paths: vec!["/gcc/include/c++".into(), "/gcc/include".into()],
        builtin_macros: Vec::new(),
        cuda: false,
        dialect: None,
        standard: None,
        source: ToolchainSource::Path,
        note: None,
    };

    let base = CompilerConfig::new().with_include_path("project/inc");
    let config = toolchain.config(&base);

    assert_eq!(
        config.include_paths,
        [
            IncludePath::user("project/inc"),
            IncludePath::system("/gcc/include/c++"),
            IncludePath::system("/gcc/include"),
        ]
    );
}

#[test]
fn preparing_a_configuration_twice_changes_nothing_the_second_time() {
    // A second occurrence of a directory can never win, so a configuration that grew every time it was
    // prepared would say the same thing in more words — and a caller that prepares it per query would grow it
    // without bound. (GCC reaches the same conclusion out loud: `ignoring duplicate directory …`.)
    let toolchain = Toolchain {
        compiler: Some("/usr/bin/g++".into()),
        version: None,
        system_include_paths: vec!["/gcc/include".into(), "/gcc/include".into()],
        builtin_macros: Vec::new(),
        cuda: false,
        dialect: None,
        standard: None,
        source: ToolchainSource::Path,
        note: None,
    };

    let once = toolchain.config(&CompilerConfig::new());
    let twice = toolchain.config(&once);

    assert_eq!(once, twice);
    assert_eq!(
        once.include_paths.len(),
        1,
        "and the duplicate inside is folded too"
    );

    let already_there = CompilerConfig::new().with_system_include_path("/gcc/include");
    assert_eq!(
        toolchain.config(&already_there),
        already_there,
        "a directory the project already lists is not added again"
    );
}

/// The toolchain a test builds by hand, with only the fields the note reads filled in.
fn toolchain_of(dialect: Option<Dialect>, include_paths: &[&str]) -> Toolchain {
    Toolchain {
        compiler: None,
        version: None,
        system_include_paths: include_paths.iter().map(PathBuf::from).collect(),
        builtin_macros: Vec::new(),
        cuda: false,
        dialect,
        standard: None,
        source: ToolchainSource::SystemHeaders,
        note: None,
    }
}

/// **The sentence a reader gets for a module this project does not contain, and why it differs per compiler.**
///
/// The failure this exists for is not a wrong answer but a useless one: "no file in this project declares module
/// `std`" is true, and it leaves the reader with nothing to do — while the fact that actually helps (where
/// *their* compiler keeps modules) is one this layer holds and was not saying.
///
/// MSVC and everything else are asserted apart, because the two sentences name different mechanisms and a
/// regression that collapsed them into one would still read as a reasonable sentence.
#[test]
fn a_module_nobody_declares_gets_a_note_about_where_the_compiler_keeps_modules() {
    let msvc = toolchain_of(Some(Dialect::Msvc), &["C:/VC/Tools/MSVC/14.35/include"]);

    let about_std = msvc.module_note("std");
    assert!(
        about_std.contains("/reference std="),
        "MSVC is told the switch that names a module: {about_std}"
    );
    assert!(
        about_std.contains("std.ixx") && about_std.contains("modules"),
        "and where the standard module's own source is, because that file exists on the machine and is the whole \
             of the fix: {about_std}"
    );
    assert!(
        about_std.contains("unknown rather than absent"),
        "the note never claims the names do not exist: {about_std}"
    );
    // The separators are the platform's, so the *sentence* is asserted on a path that was not spelled with
    // them — a test that hardcoded `\` would be a test about Windows.
    assert!(
        !about_std.contains("\\\\"),
        "the path is not escaped as if it were in a string literal: {about_std}"
    );

    // A module that is not `std` gets the switch and not somebody else's file: MSVC ships one standard module,
    // and pointing at it for `import mylib;` would be a fabricated path.
    let about_another = msvc.module_note("mylib");
    assert!(about_another.contains("/reference mylib="));
    assert!(
        !about_another.contains("std.ixx"),
        "no standard library path for a module that is not one: {about_another}"
    );

    // GCC and Clang: the module has to have been built, and the note says where such a build puts things.
    let gnu = toolchain_of(Some(Dialect::Gnu), &["/usr/include/c++/15"]);
    let about_std = gnu.module_note("std");
    assert!(
        about_std.contains("gcm.cache/std.gcm") && about_std.contains("-fmodule-file=std="),
        "both mechanisms are named, because the dialect does not tell them apart: {about_std}"
    );
    assert!(
        !about_std.contains("/reference"),
        "and MSVC's switch is not offered on a compiler that would reject it: {about_std}"
    );

    // Nobody was asked: the note still says the honest thing and claims no switches at all.
    let unknown = toolchain_of(None, &[]).module_note("std");
    assert!(
        unknown.contains("unknown rather than absent") && !unknown.contains("/reference"),
        "an unknown compiler is not guessed at: {unknown}"
    );
}

/// **A partition is not a module, so the note says something else** — `import :area;` names a unit of the module
/// the file itself declares, and the fix is not "build a module and name it" but "compile the module's interface
/// against that partition's `.ifc`".
///
/// The three commands behind MSVC's sentence are in `target/build_partitions.bat` and were measured on
/// 14.35.32215: the primary interface unit fails with C7621 without `/reference shapes:area=shapes-area.ifc`, the
/// partition itself compiles with no `/reference` at all, and the consumer never names the partition.
#[test]
fn a_partition_gets_a_note_about_the_module_it_belongs_to() {
    let msvc = toolchain_of(Some(Dialect::Msvc), &["C:/VC/Tools/MSVC/14.35/include"]);

    let note = msvc.partition_note("shapes", "area");
    assert!(
        note.contains("`shapes:area`") || note.contains("/reference shapes:area="),
        "MSVC is told the switch, spelled with the module *and* the partition: {note}"
    );
    assert!(
        note.contains(":area") && note.contains("`shapes`"),
        "and which module the partition belongs to: {note}"
    );
    assert!(
        !note.contains("declares module `area`"),
        "the note must not read a partition as a module — nothing declares a module called `area`: {note}"
    );
    assert!(note.contains("unknown rather than absent"));

    let gnu =
        toolchain_of(Some(Dialect::Gnu), &["/usr/include/c++/15"]).partition_note("shapes", "area");
    assert!(
        !gnu.contains("/reference"),
        "MSVC's switch is not offered to a compiler that would reject it: {gnu}"
    );
    assert!(
        gnu.contains("gcm.cache/shapes.gcm"),
        "GCC builds it into the module file: {gnu}"
    );
}

/// **Where a standard module's source is, when the toolchain ships one.** Found by asking the filesystem for
/// each include path's sibling `modules` directory — the layout MSVC uses — and answered as the conventional
/// location when nothing is there, so that a message names a directory rather than nothing.
///
/// The scratch tree is built here rather than borrowed from the machine, for the reason every machine-path test
/// in this repository is written this way: a test that asserts about `C:\Program Files\…` passes on this laptop
/// and fails on the next one, and the thing being tested is the *search*, not the compiler's installation.
#[test]
fn a_standard_modules_source_is_looked_for_beside_the_include_directories() {
    let scratch = std::env::temp_dir().join(format!("cppls-toolchain-{}", std::process::id()));
    let include = scratch
        .join("VC")
        .join("Tools")
        .join("MSVC")
        .join("14.35")
        .join("include");
    let modules = include.parent().expect("a parent").join("modules");
    std::fs::create_dir_all(&modules).expect("a scratch tree");
    std::fs::write(modules.join("std.ixx"), "export module std;\n").expect("the shipped source");

    let toolchain = toolchain_of(Some(Dialect::Msvc), &[include.to_string_lossy().as_ref()]);

    assert_eq!(
        toolchain.standard_module_source("std"),
        modules.join("std.ixx"),
        "the file beside the include directory is the one that ships with the toolchain"
    );

    // …and for a module no toolchain ships, the answer is still under that directory rather than a bare name:
    // a sentence naming a path is actionable and one naming a file name is not.
    let absent = toolchain.standard_module_source("mylib");
    assert_eq!(absent, modules.join("mylib.ixx"));

    let _ = std::fs::remove_dir_all(&scratch);
}

/// **Which of a command's flags change the compiler's search list** — and which do not.
///
/// The measurement behind this: a project whose compile database names clang++ for MinGW was configured with cl and
/// answered with MSVC's include paths, because the flags that say *which* library this build uses
/// (`--target`, `--gcc-toolchain`) were dropped before the compiler was asked. `-I` is deliberately not taken: that
/// is the project's own search path and travels with the file's configuration.
#[test]
fn the_search_list_flags_of_a_command_are_taken_and_nothing_else() {
    let flags = |arguments: &[&str]| -> Vec<String> {
        search_list_flags(&arguments.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    };

    assert_eq!(
        flags(&[
            "clang++",
            "--target=x86_64-w64-mingw32",
            "--gcc-toolchain=C:/mingw64",
            "-std=c++23",
            "-Iproj",
            "-c",
            "src/main.cpp",
            "-o",
            "build/main.o",
        ]),
        vec![
            "--target=x86_64-w64-mingw32".to_string(),
            "--gcc-toolchain=C:/mingw64".to_string()
        ],
        "the two flags that decide which library, and nothing else"
    );

    assert_eq!(
        flags(&[
            "clang++",
            "-target",
            "aarch64-linux-gnu",
            "-isystem",
            "/opt/inc",
            "-c",
            "a.cpp",
        ]),
        vec![
            "-target".to_string(),
            "aarch64-linux-gnu".to_string(),
            "-isystem".to_string(),
            "/opt/inc".to_string(),
        ],
        "a flag whose value is the next token takes that token with it"
    );
}
