//! Reading a compiler configuration, and a compile database.
//!
//! The theme of these tests is that a wrong configuration is invisible: nothing in a source file says
//! which `#ifdef` branches are live or which `vector` was included, so a mistake here produces an analysis
//! that is confidently wrong rather than loudly incomplete. Each flag is therefore pinned separately,
//! including the two spellings of every option and the ones that are deliberately ignored.

use cpp_code_analysis::{
    CommandLineMacro, CompilerConfig, IncludePath, parse_compile_commands, split_command_line,
};
use std::path::{Path, PathBuf};

/// A configuration read from a command line, which is how every one of these arrives in practice.
fn from_arguments(arguments: &[&str]) -> CompilerConfig {
    let command = cpp_code_analysis::CompileCommand {
        file: PathBuf::from("src/main.cpp"),
        directory: Some(PathBuf::from("/project")),
        arguments: arguments.iter().map(|it| it.to_string()).collect(),
    };

    command.to_config()
}

// ============================================================================
// Command lines
// ============================================================================

#[test]
fn both_spellings_of_an_include_path_are_read() {
    let joined = from_arguments(&["-Iinc", "main.cpp"]);
    let separate = from_arguments(&["-I", "inc", "main.cpp"]);

    assert_eq!(joined.include_paths, vec![IncludePath::user("inc")]);
    assert_eq!(joined.include_paths, separate.include_paths);
}

#[test]
fn a_system_include_path_is_marked_as_one() {
    let config = from_arguments(&["-isystem", "/usr/include", "-Iinc"]);

    assert_eq!(
        config.include_paths,
        vec![
            IncludePath::system("/usr/include"),
            IncludePath::user("inc"),
        ]
    );
    assert_eq!(
        config.user_include_paths().collect::<Vec<_>>(),
        vec![Path::new("inc")]
    );
    assert_eq!(
        config.system_include_paths().collect::<Vec<_>>(),
        vec![Path::new("/usr/include")]
    );
}

/// Order is load-bearing: the first directory holding a file wins, which is how a project shadows a system
/// header. Reversing the lists would break that silently.
#[test]
fn include_path_order_is_preserved() {
    let config = from_arguments(&["-I", "first", "-I", "second", "-isystem", "third"]);

    assert_eq!(
        config.search_order().collect::<Vec<_>>(),
        vec![
            (Path::new("first"), false),
            (Path::new("second"), false),
            (Path::new("third"), true),
        ]
    );
}

/// **`-DFOO` defines `FOO` as `1`, not as nothing.** The standard says so, and `#if FOO` depends on it —
/// treating it as an empty body would make every `#if FOO` false.
#[test]
fn a_bare_define_is_one_and_a_valued_define_keeps_its_value() {
    let config = from_arguments(&["-DFOO", "-DBAR=2", "-DBAZ="]);

    assert_eq!(
        config.defines,
        vec![
            CommandLineMacro::defined("FOO"),
            CommandLineMacro::with_value("BAR", "2"),
            CommandLineMacro::with_value("BAZ", ""),
        ]
    );

    assert_eq!(
        config.defines[0].value, None,
        "`-DFOO` has no written value"
    );
    assert_eq!(
        config.defines[2].value.as_deref(),
        Some(""),
        "and `-DBAZ=` is an empty one, which is a different thing"
    );
}

#[test]
fn an_undefine_is_read() {
    let config = from_arguments(&["-UFOO", "-U", "BAR"]);

    assert_eq!(config.undefines, vec!["FOO".into(), "BAR".into()]);
}

#[test]
fn the_standard_and_the_target_are_read() {
    let config = from_arguments(&["-std=c++20", "--target=x86_64-linux-gnu", "-m32"]);

    assert_eq!(config.standard.as_deref(), Some("c++20"));
    assert_eq!(
        config.target.as_deref(),
        Some("-m32"),
        "the later flag wins"
    );
}

#[test]
fn a_target_given_as_a_separate_argument_is_read() {
    let config = from_arguments(&["--target", "aarch64-none-elf"]);

    assert_eq!(config.target.as_deref(), Some("aarch64-none-elf"));
}

/// A flag this does not know is ignored rather than guessed at. A wrong `-D` is worse than a missing one,
/// and the flags that matter are all read.
#[test]
fn unknown_flags_are_ignored() {
    let config = from_arguments(&[
        "-O2",
        "-Wall",
        "-fno-exceptions",
        "-c",
        "-o",
        "build/main.o",
        "-Iinc",
    ]);

    assert_eq!(config.include_paths, vec![IncludePath::user("inc")]);
    assert!(config.defines.is_empty());
}

/// A relative `-I` is relative to where the compiler ran. Nothing in the source says where that was, so
/// without the working directory a project's own include paths resolve against nothing in particular.
#[test]
fn paths_resolve_against_the_working_directory() {
    let config = from_arguments(&["-Iinc", "src/main.cpp"]);

    assert_eq!(config.working_directory, Some(PathBuf::from("/project")));
    assert_eq!(
        config.resolve_against_working_directory(Path::new("inc")),
        PathBuf::from("/project/inc")
    );
    assert_eq!(
        config.resolve_against_working_directory(Path::new("/absolute/inc")),
        PathBuf::from("/absolute/inc"),
        "an absolute path is already resolved"
    );
}

#[test]
fn a_configuration_with_nothing_supplied_is_empty() {
    assert!(CompilerConfig::new().is_empty());
    assert!(
        !CompilerConfig::new()
            .with_define(CommandLineMacro::defined("X"))
            .is_empty()
    );
    assert!(!CompilerConfig::new().with_include_path("inc").is_empty());
    assert!(!CompilerConfig::new().with_standard("c++20").is_empty());
    assert!(
        !CompilerConfig::new()
            .with_working_directory("/project")
            .is_empty()
    );
}

// ============================================================================
// Command-line splitting
// ============================================================================

#[test]
fn a_command_line_splits_on_whitespace() {
    assert_eq!(
        split_command_line("c++ -c src/main.cpp -o build/main.o"),
        vec!["c++", "-c", "src/main.cpp", "-o", "build/main.o"]
    );
}

#[test]
fn quoting_keeps_an_argument_together() {
    assert_eq!(
        split_command_line("c++ \"-DFOO=some value\" 'x y'"),
        vec!["c++", "-DFOO=some value", "x y"]
    );
}

#[test]
fn an_empty_quoted_argument_is_an_argument() {
    assert_eq!(split_command_line("a \"\" b"), vec!["a", "", "b"]);
}

#[test]
fn a_backslash_escapes_the_next_character() {
    assert_eq!(split_command_line("c++ -I a\\ b"), vec!["c++", "-I", "a b"]);
}

/// Runs of whitespace collapse, and leading and trailing ones produce nothing.
#[test]
fn extra_whitespace_produces_no_empty_arguments() {
    assert_eq!(split_command_line("  a   b  "), vec!["a", "b"]);
    assert!(split_command_line("   ").is_empty());
    assert!(split_command_line("").is_empty());
}

// ============================================================================
// Compile databases
// ============================================================================

const DATABASE: &str = r#"[
  {
    "directory": "/home/u/project",
    "command": "/usr/bin/c++ -Iinc -DFOO=1 -std=c++20 -c src/main.cpp",
    "file": "/home/u/project/src/main.cpp"
  },
  {
    "directory": "/home/u/project",
    "arguments": ["/usr/bin/c++", "-isystem", "/usr/include", "-c", "src/other.cpp"],
    "file": "/home/u/project/src/other.cpp"
  }
]"#;

#[test]
fn both_shapes_of_entry_are_read() {
    let database = parse_compile_commands(DATABASE);

    assert_eq!(database.len(), 2);
    assert_eq!(database.malformed, 0);

    let first = database.command_for(Path::new("/home/u/project/src/main.cpp"));
    assert!(first.is_some(), "the `command` shape");

    let second = database.command_for(Path::new("/home/u/project/src/other.cpp"));
    assert!(second.is_some(), "the `arguments` shape");
}

#[test]
fn a_command_string_becomes_a_configuration() {
    let database = parse_compile_commands(DATABASE);
    let config = database
        .command_for(Path::new("/home/u/project/src/main.cpp"))
        .expect("an entry")
        .to_config();

    assert_eq!(config.include_paths, vec![IncludePath::user("inc")]);
    assert_eq!(
        config.defines,
        vec![CommandLineMacro::with_value("FOO", "1")]
    );
    assert_eq!(config.standard.as_deref(), Some("c++20"));
    assert_eq!(
        config.working_directory,
        Some(PathBuf::from("/home/u/project"))
    );
}

#[test]
fn an_arguments_entry_becomes_a_configuration() {
    let database = parse_compile_commands(DATABASE);
    let config = database
        .command_for(Path::new("/home/u/project/src/other.cpp"))
        .expect("an entry")
        .to_config();

    assert_eq!(
        config.include_paths,
        vec![IncludePath::system("/usr/include")]
    );
}

/// A database written on one machine and opened on another spells the same file with a different prefix.
/// Matching the tail is what makes a checked-in database usable at all.
#[test]
fn a_file_is_found_whatever_the_path_prefix_is() {
    let database = parse_compile_commands(DATABASE);

    assert!(
        database
            .command_for(Path::new("/somewhere/else/src/main.cpp"))
            .is_some()
    );
    assert!(
        database.command_for(Path::new("src/main.cpp")).is_some(),
        "and with no prefix at all"
    );
    assert!(database.command_for(Path::new("src/nothing.cpp")).is_none());
}

/// Case and separator differences are the same file: a database written on Windows and read on Linux — or
/// the reverse — must still match.
#[test]
fn a_file_matches_across_separators_and_case() {
    let database = parse_compile_commands(DATABASE);

    assert!(
        database
            .command_for(Path::new("/home/u/project/src/Main.cpp"))
            .is_some()
    );
    assert!(
        database
            .command_for(Path::new("\\home\\u\\project\\src\\main.cpp"))
            .is_some()
    );
}

/// **A malformed entry must not be a hard failure.** Projects emit databases with trailing commas, with
/// comments, and with entries for languages this has nothing to say about; the useful behaviour is to take
/// what parses and report how much did not.
///
/// The entry with no `command` and no `arguments` is counted as malformed, while `{ this is not json }` is
/// *not*: it is a well-formed object that happens to lack the fields, and the shallow reader cannot tell
/// the two apart — nor does it need to, because both lead to the same decision.
#[test]
fn a_malformed_database_is_reported_not_fatal() {
    let database = parse_compile_commands(
        r#"[
  { "directory": "/p", "command": "c++ -c a.cpp", "file": "/p/a.cpp" },
  { "directory": "/p", "file": "/p/no-command.cpp" },
  { this is not json at all },
  { "directory": "/p", "arguments": ["c++", "-c", "b.cpp"], "file": "/p/b.cpp" }
]"#,
    );

    assert_eq!(database.len(), 2, "the two readable entries");
    assert_eq!(database.malformed, 2, "and the two that could not be used");
    assert!(
        database.command_for(Path::new("a.cpp")).is_some(),
        "a malformed entry does not lose the good ones"
    );
    assert!(
        database.command_for(Path::new("b.cpp")).is_some(),
        "and the good entry after it is still read"
    );
}

#[test]
fn an_empty_or_absent_database_is_empty() {
    assert!(parse_compile_commands("[]").is_empty());
    assert!(parse_compile_commands("").is_empty());
    assert!(parse_compile_commands("not json").is_empty());
    assert!(
        parse_compile_commands("{}").is_empty(),
        "an object, not an array"
    );
}

/// A brace inside a string does not open an object, which is what a naive splitter would get wrong on a
/// command containing one.
#[test]
fn braces_inside_strings_do_not_confuse_the_reader() {
    let database = parse_compile_commands(
        r#"[{ "directory": "/p", "command": "c++ -DX={a} -c a.cpp", "file": "/p/a.cpp" }]"#,
    );

    assert_eq!(database.len(), 1);
    assert_eq!(database.malformed, 0);

    let config = database
        .command_for(Path::new("a.cpp"))
        .expect("an entry")
        .to_config();
    assert_eq!(
        config.defines,
        vec![CommandLineMacro::with_value("X", "{a}")]
    );
}

/// A Windows path in a database is spelled with doubled separators in JSON, which the reader has to undo
/// or every include in the project fails to resolve.
#[test]
fn escaped_separators_in_a_path_are_unescaped() {
    let database = parse_compile_commands(
        r#"[{ "directory": "C:\\proj", "command": "cl /c a.cpp", "file": "C:\\proj\\a.cpp" }]"#,
    );

    assert_eq!(database.len(), 1);
    let config = database
        .command_for(Path::new("a.cpp"))
        .expect("an entry")
        .to_config();
    assert_eq!(config.working_directory, Some(PathBuf::from("C:/proj")));
}
