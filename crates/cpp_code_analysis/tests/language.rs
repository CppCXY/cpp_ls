//! **The language a file is compiled as**, as the analysis can tell — `CompilerConfig::language_for`.
//!
//! The parser's feature set has to come from somewhere, and for a real project that somewhere is
//! `compile_commands.json`: each translation unit carries its own `-std=`, and one target may be C++17 while another is
//! C++20. This is the lookup that turns a file into a configuration, tested here because everything above it is
//! plumbing.

use std::path::Path;

use cpp_code_analysis::include::config::CompilerConfig;
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
        10,
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
    assert_eq!(language.standards().count(), 10);
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
