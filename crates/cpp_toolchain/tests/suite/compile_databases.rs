//! **A CMake `compile_commands.json`** — forward slashes, a build directory as `directory`, an absolute source.
//!
//! Every project this analysis has been measured on emits one of two spellings: Ninja's own (backslashes on Windows,
//! `directory` the source root) and CMake's (forward slashes, `directory` the build tree). libuv's database is the
//! first and curl's is the second — and a `sweep` of curl reported **every** file as "not in the compilation
//! database", which is a statement about this reader, not about curl (`docs/worklog.md`).

use cpp_toolchain::include::config::parse_compile_commands;

/// The CMake spelling in one line: a build directory, a forward-slash absolute source, and the flags.
const CMAKE: &str = r#"[
  {
    "directory": "E:/p/build",
    "command": "C:/mingw/bin/gcc.exe -DBUILDING_LIBCURL -DHAVE_CONFIG_H -IE:/p/include -IE:/p/build/lib -IE:/p/lib -W -Wall -c E:/p/lib/url.c",
    "file": "E:/p/lib/url.c",
    "output": "E:/p/build/CMakeFiles/libcurl.dir/lib/url.c.obj"
  },
  {
    "directory": "E:/p/build",
    "command": "C:/mingw/bin/gcc.exe -DOTHER=1 -IE:/p/include -c E:/p/lib/sendf.c",
    "file": "E:/p/lib/sendf.c"
  }
]"#;

/// Both entries are read, and either spelling of the path finds its command.
#[test]
fn a_cmake_database_is_read_and_its_paths_match() {
    let database = parse_compile_commands(CMAKE);
    assert_eq!(database.commands.len(), 2, "both entries");
    assert_eq!(database.malformed, 0, "neither entry is malformed");

    let forward = database.command_for(std::path::Path::new("E:/p/lib/url.c"));
    assert!(forward.is_some(), "the forward-slash spelling matches");
    let backward = database.command_for(std::path::Path::new(r"E:\p\lib\url.c"));
    assert!(
        backward.is_some(),
        "the backslash spelling matches the same entry"
    );

    let command = database
        .command_for(std::path::Path::new("lib/sendf.c"))
        .expect("a suffix matches");
    assert!(
        command.arguments.iter().any(|a| a.contains("OTHER=1")),
        "the flags came with it"
    );
}

/// A path the database does not name is not matched — the negative half, or "every file is in the database" would be
/// the answer to any bug in this reader.
#[test]
fn a_file_the_database_does_not_name_has_no_command() {
    let database = parse_compile_commands(CMAKE);
    assert!(
        database
            .command_for(std::path::Path::new("E:/p/lib/aix.c"))
            .is_none()
    );
}

/// **The real thing**: curl's CMake database, when this machine has the corpus. A hand-written fixture proves the
/// reader's shape; only the real file proves it against what CMake writes — four hundred characters of flags per entry,
/// escaped quotes, and 255 entries.
#[test]
fn a_real_cmake_database_reads_whole() {
    let path = std::path::Path::new(r"E:\corpora\curl\compile_commands.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return; // the corpus is not on this machine; the fixtures above still cover the shape
    };
    let database = parse_compile_commands(&text);
    assert_eq!(
        database.malformed, 0,
        "every entry of a CMake database parses: {} malformed",
        database.malformed
    );
    assert!(
        database.commands.len() > 200,
        "a CMake database has one entry per translation unit: {}",
        database.commands.len()
    );
    assert!(
        database
            .command_for(std::path::Path::new("E:/corpora/curl/lib/url.c"))
            .is_some(),
        "and a file it names has its command"
    );
}
