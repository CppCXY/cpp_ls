//! **A class body that stays one body** — the member after a method whose body contains the macro-shaped
//! try/catch of `nlohmann`'s `set_parents()`.
//!
//! Reported from an editor on `CodeFormatServer/src/LSP/LSP.cpp`: `nlohmann::json::object()` had no hover. The index
//! holds `bool_widget`'s members up to `assert_invariant` and then holds **`parse`, `dump`, `size` with no scope at
//! all** — so the class body is closed early by recovery, and every later member is a declaration the class does not
//! own. This fixture is that shape, and it asserts the later member still belongs to the class.

use cpp_code_analysis::file::coordinates::{FileOffset, ReadingOffset};
use cpp_code_analysis::{
    CompilerConfig, Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

fn session_of(files: &[(&str, &str)]) -> Session<MemoryFiles> {
    let mut memory = MemoryFiles::new();
    let mut paths = Vec::new();
    for (path, text) in files {
        memory = memory.with_file(*path, *text);
        paths.push(std::path::PathBuf::from(path));
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files(paths.clone());
    for path in &paths {
        session.load(path);
        session.want_cooked_reading(path);
    }
    session.index_everything();
    session
}

/// **The member a name at `needle` declares**, from the index, as `scope::name` — or the refusal.
fn declared_at(session: &Session<MemoryFiles>, path: &str, needle: &str) -> String {
    let path = std::path::Path::new(path);
    let view = session.view(path).expect("the file has a view");
    let at = view
        .written_text()
        .expect("the file is its own text")
        .find(needle)
        .unwrap_or_else(|| panic!("`{needle}` is in the file"));
    let reading = view
        .reading_offset_of(FileOffset::new(at))
        .expect("the file offset is in the reading");
    let Some(name) = session.the_name_declared_at(&view, reading.get()) else {
        return "no name".to_string();
    };
    match session.definition(&view, ReadingOffset::new(reading.get())) {
        Known::Yes(found) => match &found.fact.scope {
            Some(scope) => format!("{scope}::{name}"),
            None => format!("(none)::{name}"),
        },
        Known::Unknown(reason) => format!("unknown: {reason:?}"),
        Known::No => "no".to_string(),
    }
}

const SHAPE: &str = "\
#define JSON_TRY try
#define JSON_CATCH(...) catch (__VA_ARGS__)
#define JSON_ASSERT(x) assert(x)

struct Widget {
    bool check = false;

    void assert_invariant(bool check_parents = true) const noexcept
    {
        JSON_ASSERT(check);
#if JSON_DIAGNOSTICS
        JSON_TRY
        {
            check_parents = false;
        }
        JSON_CATCH(...) {}
#endif
        static_cast<void>(check_parents);
    }

    void set_parents()
    {
        check = true;
    }

    bool after_this = true;
};
";

/// **`set_parents` and everything after it still belong to `Widget`.** A member whose name answers with no scope is a
/// member the index cannot find when a reader asks `Widget::x`.
#[test]
fn a_method_after_the_macro_shaped_try_catch_is_still_a_member() {
    let session = session_of(&[("/p/widget.h", SHAPE)]);
    assert_eq!(
        declared_at(&session, "/p/widget.h", "set_parents"),
        "Widget::set_parents",
        "the method written after the macro-shaped try/catch is Widget's"
    );
    assert_eq!(
        declared_at(&session, "/p/widget.h", "after_this"),
        "Widget::after_this",
        "and so is the member after that"
    );
}
