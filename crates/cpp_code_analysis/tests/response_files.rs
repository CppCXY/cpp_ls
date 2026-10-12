//! **A build system's command line, read as the compiler reads it** — response files included.
//!
//! The case this exists for is CMake's, measured on a real project: **every** entry of its
//! `compile_commands.json` writes its include paths as
//!
//! ```text
//! g++.exe -DURI_STATIC_BUILD @CMakeFiles/LuaParser.dir/includes_CXX.rsp -O3 -std=gnu++20 … -c src/Parse/LuaParser.cpp
//! ```
//!
//! so a reader that does not follow the `@` indirection sees a project with **no include paths at all**: its own
//! headers stop resolving, the file list stops being the build's, and the flags that decide what the code *means*
//! (`-D`, `-std=`) are missing too.

use std::collections::HashMap;
use std::path::Path;

use cpp_code_analysis::include::config::expand_response_files;

/// The ordinary shape: one `@file`, whose contents are arguments, in the position the build wrote it.
#[test]
fn a_response_file_is_read_for_the_arguments_it_names() {
    let files: HashMap<&str, &str> = HashMap::from([(
        "includes_CXX.rsp",
        "-I/p/LuaParser/include -I/p/LuaParser/src\n-I/p/Util/include",
    )]);
    let arguments: Vec<String> = ["g++", "-DNDEBUG", "@CMakeFiles/LuaParser.dir/includes_CXX.rsp", "-O3"]
        .into_iter()
        .map(String::from)
        .collect();

    // Looked up by **file name**, because a path built with `Path::join` is spelled with the platform's
    // separator and this test runs on Windows as well as on Unix. What the lookup proves is the *resolution*:
    // the relative name was joined to the working directory the command names.
    let mut asked: Vec<String> = Vec::new();
    let expanded = expand_response_files(&arguments, Some(Path::new("/p/build")), &mut |path| {
        asked.push(path.to_string_lossy().to_string());
        files
            .get(path.file_name()?.to_string_lossy().as_ref())
            .map(|text| text.to_string())
    });

    assert_eq!(
        expanded,
        [
            "g++",
            "-DNDEBUG",
            "-I/p/LuaParser/include",
            "-I/p/LuaParser/src",
            "-I/p/Util/include",
            "-O3",
        ],
        "the file's own arguments, in its own order, where the `@` stood"
    );
    assert!(
        asked.iter().any(|path| path.ends_with("CMakeFiles/LuaParser.dir/includes_CXX.rsp")
            || path.ends_with(r"CMakeFiles\LuaParser.dir\includes_CXX.rsp")),
        "the relative name is resolved against the command's working directory: {asked:?}"
    );
}

/// A response file that cannot be read is **left as it stands**: an argument the build wrote is not one this layer
/// may delete, and a compiler given a missing response file says so itself.
#[test]
fn an_unreadable_response_file_is_left_alone() {
    let arguments: Vec<String> = ["g++", "@nowhere.rsp", "-O2"].into_iter().map(String::from).collect();

    let expanded = expand_response_files(&arguments, Some(Path::new("/p")), &mut |_| None);

    assert_eq!(expanded, arguments);
}

/// A file that names itself terminates rather than hanging the analysis, and what it wrote is kept once.
#[test]
fn a_response_file_that_names_itself_terminates() {
    let files: HashMap<&str, &str> = HashMap::from([("self.rsp", "-DA @self.rsp")]);
    let arguments: Vec<String> = ["g++", "@self.rsp"].into_iter().map(String::from).collect();

    let expanded = expand_response_files(&arguments, Some(Path::new("/p")), &mut |path| {
        files
            .get(path.file_name()?.to_string_lossy().as_ref())
            .map(|text| text.to_string())
    });

    assert!(expanded.iter().any(|argument| argument == "-DA"), "{expanded:?}");
    assert!(expanded.len() < 40, "bounded rather than endless: {expanded:?}");
}

