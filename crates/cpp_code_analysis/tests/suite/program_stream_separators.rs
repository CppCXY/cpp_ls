//! **Two characters the preprocessor wrote together stay together in the program stream.**
//!
//! Reported from an editor: opening Lua 5.5.0's `ldebug.c` showed 15 `expected primary expression` errors. The file's
//! own cook was clean, every individual construct parsed on its own, and the errors were all in the **program** read:
//! the stitched stream spelled a header's macro expansion as
//!
//! ```text
//! ( ( s ) -> shrlen > = 0 )
//! ```
//!
//! from `lobject.h:420`'s `#define strisshr(ts) ((ts)->shrlen >= 0)`. The lexer leaves the `>`-family in one-byte
//! tokens on purpose (`CppLexer::push`) and the grammar puts them back together **by byte adjacency**, so `> =` is two
//! tokens the parser will not join — same failure the per-file renderer had, one layer down.
//!
//! The cause was in [`RenderedUnit`]'s writer: it decided whether two tokens touched by comparing their **written**
//! ranges, and every token of one expansion carries the **invocation** as its range — so no pair of an expansion was
//! ever adjacent, and each got a space. The expander already keeps the answer per token
//! (`ExpandedToken::space_before`), and the writer now takes it for the tokens the cook produced.

use std::path::PathBuf;

use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn a_project(name: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf) {
    let root: PathBuf = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a project directory");
    for (name, text) in files {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a directory");
        }
        std::fs::write(&path, text).expect("the fixture writes");
    }
    (root.clone(), root.join("main.c"))
}

#[test]
fn an_operators_two_characters_survive_the_program_stream() {
    let (root, file) = a_project(
        "cppls-program-stream-separators",
        &[
            (
                "object.h",
                "#pragma once\n\
                 #define strisshr(ts) ((ts)->shrlen >= 0)\n\
                 #define at_most(a, b) ((a) <= (b))\n\
                 #define halved(a) ((a) >> 2)\n\
                 struct TString { int shrlen; char *contents; };\n",
            ),
            (
                "main.c",
                "#include \"object.h\"\n\
                 \n\
                 static const char *f(const struct TString *s, int n) {\n\
                 \x20   if (!strisshr(s) || !at_most(n, 4))\n\
                 \x20       return \"?\";\n\
                 \x20   return halved(n) == 0 ? s->contents : s->contents;\n\
                 }\n",
            ),
        ],
    );

    let mut session = Session::open(
        &root,
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files([file.clone()]);
    session.catch_up(&file);
    let _ = session.index_everything();

    let unit = session.render_the_unit(&file).expect("a program");
    // **The three shapes**, none of which may be split: the operators the lexer keeps in pieces.
    for spelled in [">=", "<=", ">>"] {
        assert!(
            unit.text.contains(spelled),
            "`{spelled}` must survive the program stream: {}",
            unit.text
        );
    }
    // **And no split one.** `> =` is what the parser cannot join.
    for split in ["> =", "< =", "> >"] {
        assert!(
            !unit.text.contains(split),
            "`{split}` is the split form of an operator and must not appear: {}",
            unit.text
        );
    }

    // **The stream parses**, which is the measurement the report is about: 15 errors before, 0 after.
    let tree = cpp_parser::CppParser::parse(
        &unit.text,
        cpp_parser::ParserConfig::default().with_file_boundaries(unit.file_boundaries()),
    );
    assert_eq!(
        tree.get_errors().len(),
        0,
        "the stitched program must parse: {:?}",
        tree.get_errors()
            .iter()
            .map(|error| (error.message.clone(), {
                let range = cpp_parser::source_range(error.range);
                unit.text
                    .get(range.start_offset.saturating_sub(40)..range.start_offset + 80)
                    .unwrap_or("")
                    .to_string()
            }))
            .collect::<Vec<_>>()
    );

    let _ = std::fs::remove_dir_all(&root);
}
