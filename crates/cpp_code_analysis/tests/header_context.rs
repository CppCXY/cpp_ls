//! **A header read in the translation unit that includes it** — the declarations a macro definition unlocks.
//!
//! A header has no configuration of its own. `globals.h` in neovim declares the editor's globals as
//!
//! ```c
//! EXTERN VisualState Visual INIT( = { .mode = 'v' });        // globals.h:453
//! ```
//!
//! with `EXTERN` and `INIT` defined in `macros_defs.h`, a path that resolves only with the translation unit's `-I`
//! flags. Read **on its own**, the header cannot find that include, `EXTERN` and `INIT` are plain names, and sixteen
//! lines parse as expressions (`expected ';' after expression`); every declaration after the first of them is lost.
//!
//! The engine files a cooked reading **per program** (`ProjectIndex::insert_cooked_in_a_program`), exactly so that a
//! header read by two translation units can answer differently — so the declaration is in the index, filed under the
//! program that read it. What these tests pin is that a query made **from** that translation unit finds it: the
//! name lookup has to ask for the reading of the asking program, not for the header's own.

use cpp_code_analysis::{
    CompilerConfig, Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// A session whose project is `files`, with `args` as the project's compile flags.
fn session_of(files: &[(&str, &str)], args: &[&str]) -> Session<MemoryFiles> {
    let mut memory = MemoryFiles::new();
    let mut paths = Vec::new();
    for (path, text) in files {
        memory = memory.with_file(*path, *text);
        paths.push(std::path::PathBuf::from(path));
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    // **The include path, spelled the way the configuration loads it.** `extra_args` are folded into a file's
    // command by `CompilerConfig::for_file`, which needs a compilation database entry to fold them into — a test
    // project with no database states its search path directly.
    let config = CompilerConfig {
        root: Some(std::path::PathBuf::from("/p")),
        include_paths: args
            .iter()
            .filter_map(|argument| argument.strip_prefix("-I"))
            .map(cpp_code_analysis::include::config::IncludePath::user)
            .collect(),
        ..CompilerConfig::default()
    };
    let mut session = Session::with_config("/p", providers, WatchFilter::new("/p"), config);
    session.add_project_files(paths.clone());
    for path in &paths {
        session.load(path);
        session.want_cooked_reading(path);
    }
    session.index_everything();
    session
}

/// **The header's `-I` is the translation unit's**, and the declaration it unlocks is visible from that unit.
///
/// This is the shape that produced 135 `undefined-name` findings on neovim's own source: the global is declared, the
/// file that uses it includes the header, and the header parses only with the flags of the file that includes it.
#[test]
fn a_declaration_a_macro_unlocks_in_a_header_is_visible_from_the_unit_that_included_it() {
    let session = session_of(
        &[
            ("/p/include/macros.h", "#define EXTERN extern\n#define INIT(...)\n"),
            (
                "/p/include/globals.h",
                "#include \"include/macros.h\"\n\
                 typedef int colnr_T;\n\
                 EXTERN colnr_T dollar_vcol INIT( = -1);\n",
            ),
            (
                "/p/a.c",
                "#include \"include/globals.h\"\n\
                 void f(void) { dollar_vcol = 1; }\n",
            ),
        ],
        &["-I/p"],
    );

    let found = session.index().definition("dollar_vcol", std::path::Path::new("/p/a.c"));
    let Known::Yes(found) = &found else {
        panic!("`dollar_vcol` is declared in `globals.h`: {found:?}");
    };
    assert_eq!(found.fact.name.as_str(), "dollar_vcol");
    assert_eq!(
        found.fact.type_of.as_deref(),
        Some("colnr_T"),
        "the type the header writes"
    );
}

/// **The header's own reading stays its own answer.** A file that includes nothing must not see the declaration the
/// other translation unit's flags unlocked — that would be the answer of a program it is not part of.
#[test]
fn the_unlocked_declaration_is_not_visible_from_a_file_that_does_not_include_the_header() {
    let session = session_of(
        &[
            ("/p/include/macros.h", "#define EXTERN extern\n#define INIT(...)\n"),
            (
                "/p/include/globals.h",
                "#include \"include/macros.h\"\n\
                 typedef int colnr_T;\n\
                 EXTERN colnr_T dollar_vcol INIT( = -1);\n",
            ),
            (
                "/p/a.c",
                "#include \"include/globals.h\"\n\
                 void f(void) { dollar_vcol = 1; }\n",
            ),
            ("/p/b.c", "void g(void) { }\n"),
        ],
        &["-I/p"],
    );

    let found = session.index().definition("dollar_vcol", std::path::Path::new("/p/b.c"));
    assert!(
        !matches!(found, Known::Yes(_)),
        "`b.c` neither includes the header nor shares a program with the file that does: {found:?}"
    );
}