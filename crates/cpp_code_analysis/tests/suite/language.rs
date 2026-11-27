//! **The language a file is compiled as**, as the analysis can tell — `CompilerConfig::language_for`.
//!
//! The parser's feature set has to come from somewhere, and for a real project that somewhere is
//! `compile_commands.json`: each translation unit carries its own `-std=`, and one target may be C++17 while another is
//! C++20. This is the lookup that turns a file into a configuration, tested here because everything above it is
//! plumbing.

use std::path::Path;

use cpp_code_analysis::include::config::CompilerConfig;
use cpp_code_analysis::parse_compile_commands;
use cpp_parser::features::{Feature, Standard, Vendor};

/// **A command line's `-std=` is the answer**, and the last one wins, because that is what a compiler does.
#[test]
fn a_projects_own_flags_decide_the_language() {
    let config = CompilerConfig {
        extra_args: vec!["-Iinclude".to_string(), "-std=gnu++20".to_string()],
        ..CompilerConfig::default()
    };
    let language = config.language_for(Path::new("/p/a.cpp"));
    assert_eq!(language.describe(), "gnu++20");
    assert_eq!(
        language.standards().collect::<Vec<_>>(),
        vec![Standard::Cpp20]
    );
    assert_eq!(language.vendors().collect::<Vec<_>>(), vec![Vendor::Gnu]);
    assert!(language.support(Feature::Concepts));
    assert!(language.support(Feature::StatementExpression));
    assert!(
        !language.support(Feature::DeducingThis),
        "C++23, which is later"
    );
}

/// **Nothing said means nothing refused**: with no database and no flags, the answer is `permissive`, because a parse that
/// has not been told what it is reading must not refuse the headers it is reading.
#[test]
fn an_unconfigured_project_is_permissive() {
    let language = CompilerConfig::default().language_for(Path::new("/p/a.cpp"));
    assert!(language.support(Feature::DeducingThis));
    assert!(language.support(Feature::StatementExpression));
    assert!(language.support(Feature::StructuredExceptionHandling));
    assert!(language.support(Feature::C89Declarations));
    assert_eq!(
        language.standards().count(),
        11,
        "every standard, every vendor"
    );
}

/// **A flag that is not a standard is not a language**: `-Ifoo` and `-O2` say nothing about syntax, so the answer falls
/// through rather than inventing a configuration from the first argument.
#[test]
fn a_command_without_a_standard_flag_falls_through() {
    let config = CompilerConfig {
        extra_args: vec![
            "-Iinclude".to_string(),
            "-O2".to_string(),
            "-DFOO=1".to_string(),
        ],
        ..CompilerConfig::default()
    };
    let language = config.language_for(Path::new("/p/a.cpp"));
    assert!(
        language.support(Feature::DeducingThis),
        "the permissive fallback"
    );
    assert_eq!(language.standards().count(), 11);
}

/// **A separated `-std c++17` is read too**, which some build systems write even though a compiler wants it joined.
#[test]
fn a_separated_standard_flag_is_read() {
    let config = CompilerConfig {
        extra_args: vec!["-std".to_string(), "c++17".to_string()],
        ..CompilerConfig::default()
    };
    assert_eq!(
        config.language_for(Path::new("/p/a.cpp")).describe(),
        "c++17"
    );
}

/// **A header's language is the language of the source that compiles it.**
///
/// A build lists its *sources*; a header appears nowhere in the database, so the lookup fell to the project-wide
/// `[compile] standard` — the editor's default `c++23` — and a C target's headers were read as C++. That is the
/// mixed-project case: the same directory holds `a.c` and `b.cpp`, and `a.h` is compiled by `a.c`.
#[test]
fn a_header_inherits_the_language_of_the_source_beside_it() {
    let database = parse_compile_commands(
        r#"[
            {"directory": "/p", "file": "/p/a.c", "arguments": ["cc", "-std=c17", "-c", "a.c"]},
            {"directory": "/p", "file": "/p/b.cpp", "arguments": ["c++", "-std=c++20", "-c", "b.cpp"]}
        ]"#,
    );
    let config = CompilerConfig {
        database: Some(std::sync::Arc::new(database)),
        ..CompilerConfig::default()
    };

    // The neighbour is the alphabetically first source in the directory — `a.c` — so `/p/a.h` is C. A header whose
    // implementation is `b.cpp` would be C++: the point of the test is that the *database* decides, not the
    // project-wide default, which is what it used to do.
    let header = config.language_for(Path::new("/p/a.h"));
    assert_eq!(
        header.family(),
        Some(cpp_parser::features::Family::C),
        "the source beside it is C: {header:?}"
    );
    assert!(header.support(Feature::CompoundLiterals));

    // **A `.hpp` states its own family**: it is a C++ header even beside a `.c` file.
    let cpp_header = config.language_for(Path::new("/p/types.hpp"));
    assert_ne!(
        cpp_header.family(),
        Some(cpp_parser::features::Family::C),
        "a `.hpp` is C++ by its extension: {cpp_header:?}"
    );
}

/// **With nothing in the database, the project's own configuration is the assumption** — the fallback the
/// user asked for: "or assume one from the configuration".
#[test]
fn a_header_with_no_source_beside_it_takes_the_project_standard() {
    let config = CompilerConfig::default().with_standard("c17");
    assert_eq!(
        config.language_for(Path::new("/p/lonely.h")).family(),
        Some(cpp_parser::features::Family::C)
    );

    // …and with no configuration at all a header stays permissive rather than being guessed.
    let unstated = CompilerConfig::default().language_for(Path::new("/p/lonely.h"));
    assert_eq!(unstated.family(), None, "not told is not C");
}
