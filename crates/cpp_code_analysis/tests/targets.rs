//! **A project with several compilation targets, described in `.cppls.toml`.**
//!
//! The case this exists for, in the words of the person who hit it: *"a C++ project may contain several compilation
//! targets, and a single `args` list searching includes with `-I` is simply not workable"*. It is not workable for a
//! reason worth stating as a test: with every target's directories in one list, a header reachable only from another
//! target's directory **resolves**, and an include of `"Config.h"` takes whichever `Config.h` the search order
//! happens to reach first — a plausible wrong answer rather than a missing one.
//!
//! So each target states its own directories, and a file gets **its target's**, in front.

use std::path::{Path, PathBuf};

use cpp_code_analysis::project::{TargetSection, parse_config, targets_for};
use cpp_code_analysis::{CompilerConfig, IncludePath};

/// The section a configuration file's `[compile]` holds, read the way the server reads it.
fn compile_section(text: &str) -> cpp_code_analysis::project::CompileSection {
    let report = parse_config(text, Path::new("/p/.cppls.toml"));
    assert!(
        report.problems.is_empty(),
        "the fixture's configuration must be understood: {:?}",
        report.problems
    );

    report
        .config
        .compile
}

/// The configuration one file is analysed with — what the session builds and the resolver asks for.
fn config_for(section: &cpp_code_analysis::project::CompileSection, file: &str) -> CompilerConfig {
    let mut config = CompilerConfig {
        root: Some(PathBuf::from("/p")),
        targets: targets_for(section),
        ..CompilerConfig::default()
    };

    // The flags a project would have discovered, so that "the target's directories go in front" is visible.
    config
        .include_paths
        .push(IncludePath::user("/p/project-wide"));

    config.for_file(Path::new(file))
}

/// The user's directories, as paths, in search order.
fn directories(config: &CompilerConfig) -> Vec<String> {
    config
        .include_paths
        .iter()
        .map(|path| path.directory.to_string_lossy().replace('\\', "/"))
        .collect()
}

fn defines(config: &CompilerConfig) -> Vec<String> {
    config
        .defines
        .iter()
        .map(|define| define.name.to_string())
        .collect()
}

const TWO_TARGETS: &str = r#"
[compile]
standard = "c++17"

[[compile.target]]
name     = "Server"
sources  = ["CodeFormatServer/**"]
includes = ["CodeFormatServer/include", "Util/include"]
defines  = ["SERVER_BUILD", "URI_STATIC_BUILD"]

[[compile.target]]
name     = "Core"
sources  = ["CodeFormatCore/**"]
includes = ["CodeFormatCore/include"]
defines  = ["CORE_BUILD"]
"#;

/// **Each file gets its own target's include directories**, and they come first.
#[test]
fn a_file_is_compiled_as_its_own_target() {
    let section = compile_section(TWO_TARGETS);


    let server = config_for(&section, "/p/CodeFormatServer/src/Server.cpp");
    assert_eq!(
        directories(&server),
        [
            "/p/CodeFormatServer/include",
            "/p/Util/include",
            "/p/project-wide",
        ],
        "the target's own directories first, then whatever the project gave"
    );
    assert_eq!(defines(&server), ["SERVER_BUILD", "URI_STATIC_BUILD"]);

    let core = config_for(&section, "/p/CodeFormatCore/src/Core.cpp");
    assert_eq!(
        directories(&core),
        ["/p/CodeFormatCore/include", "/p/project-wide"],
        "a header under Server/include is not reachable from Core — that is the point"
    );
    assert_eq!(defines(&core), ["CORE_BUILD"]);
}

/// **The most specific target wins**, whichever order the tables are written in.
#[test]
fn the_nearer_pattern_describes_the_file() {
    let section = compile_section(
        r#"
[[compile.target]]
name     = "everything"
sources  = ["**"]
includes = ["all/include"]

[[compile.target]]
name     = "legacy"
sources  = ["Util/src/legacy/**"]
includes = ["legacy/include"]
"#,
    );

    let legacy = config_for(&section, "/p/Util/src/legacy/Old.cpp");
    assert_eq!(
        directories(&legacy),
        ["/p/legacy/include", "/p/all/include", "/p/project-wide"],
        "two literal components beat none"
    );

    let other = config_for(&section, "/p/Util/src/New.cpp");
    assert_eq!(directories(&other), ["/p/all/include", "/p/project-wide"]);
}

/// **`[compile.include]` still means what it always documented** — and now it means it in effect.
///
/// It was parsed, validated and never applied before targets existed: a project that wrote it got nothing at all,
/// which is part of why whole projects end up with a single `args` list.
#[test]
fn the_older_per_directory_key_describes_a_target_too() {
    let section = compile_section(
        r#"
[compile.include]
"src/legacy" = ["legacy/include"]
"*" = ["include"]
"#,
    );

    let legacy = config_for(&section, "/p/src/legacy/Old.cpp");
    assert_eq!(
        directories(&legacy),
        ["/p/legacy/include", "/p/include", "/p/project-wide"]
    );

    let elsewhere = config_for(&section, "/p/src/New.cpp");
    assert_eq!(directories(&elsewhere), ["/p/include", "/p/project-wide"]);
}

/// A file no target describes is compiled as the project says, and a target that names no files describes none.
#[test]
fn a_file_no_target_describes_is_left_alone() {
    let section = compile_section(
        r#"
[[compile.target]]
name     = "nameless"
includes = ["nowhere/include"]
"#,
    );

    let config = config_for(&section, "/p/src/any.cpp");
    assert_eq!(
        directories(&config),
        ["/p/project-wide"],
        "a target with no `sources` is not a target for every file"
    );
}

/// The pattern language: `**` is any depth, `*` is within one component, and the rest is literal.
#[test]
fn patterns_mean_what_the_documentation_says() {
    let target = TargetSection {
        sources: vec!["*/src/*.cpp".to_string()],
        ..TargetSection::default()
    };
    let root = Path::new("/p");

    assert!(target.describes(root, Path::new("/p/Util/src/Only.cpp")).is_some());
    assert!(
        target.describes(root, Path::new("/p/Util/src/deeper/Only.cpp")).is_none(),
        "`*` does not cross a separator"
    );
    assert!(target.describes(root, Path::new("/p/NotUtil/Only.cpp")).is_none());
}






