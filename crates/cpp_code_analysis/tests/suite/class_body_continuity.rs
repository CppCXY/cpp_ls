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
/// **The real file's tree**: are `assert_invariant` and `set_parents` inside the class body at all?
///
/// ```text
/// $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
/// cargo test -p cpp_code_analysis --test suite class_body_continuity -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a probe: parses one file of a real project"]
fn where_json_hpp_stops_being_a_class_body() {
    let Ok(root) = Result::<String, std::env::VarError>::Ok(r"E:\EmmyLuaCodeStyle".to_string()) else {
        eprintln!("set CPPLS_REAL_PROJECT to run this probe");
        return;
    };
    let path = std::path::PathBuf::from(root).join("3rd/nlohmann_json/include/nlohmann/json.hpp");
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("cannot read {}", path.display());
        return;
    };
    // **The file's own text, parsed as the index parses it**, so the tree here is the one the index used.
    {
        let tree = cpp_parser::CppParser::parse(&text, cpp_parser::ParserConfig::default());
        let root = tree.get_red_root();
        if let Some(at) = text.find("}));") {
            println!(
                "[brace] `}}}}));` at {at} (line {})",
                text[..at].lines().count()
            );
            let mut chain = Vec::new();
            if let Some(token) = cpp_parser::token_at(&root, at) {
                let mut node = token.parent();
                while let Some(next) = node {
                    chain.push(format!("{:?}", next.kind()));
                    node = next.parent();
                }
            }
            println!("[brace] `}}` chain: {}", chain.join(" < "));
        }
    }
    let memory = MemoryFiles::new().with_file(path.to_string_lossy().to_string(), text.clone());
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([path.clone()]);
    session.add_project_files([path.clone()]);
    session.did_open(&path, &text);
    session.index_everything();
    let view = session
        .view_of_the_file(&path)
        .expect("the file has a view");
    let rendering = view.source.to_string();
    // **The class body of `basic_json`, child by child**, and the first child that is not a plain declaration:
    // that is the construct which put the rest of the members inside a statement.
    if let Some(body) = view.root.descendants().find(|node| {
        cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::ClassBody
            && node
                .parent()
                .is_some_and(|parent| parent.text().to_string().contains("class basic_json"))
    }) {
        let children: Vec<_> = body.children().collect();
        println!(
            "[body] {} child(ren), body text {} byte(s)",
            children.len(),
            body.text().to_string().len()
        );
        for (index, child) in children.iter().enumerate() {
            if index + 1 < 66 {
                continue;
            }
            let text = child.text().to_string();
            let first_line = text.lines().next().unwrap_or("").trim();
            println!(
                "[body] child {}: {:?} `{}`",
                index + 1,
                child.kind(),
                first_line.chars().take(90).collect::<String>()
            );
            if index + 1 > 74 {
                break;
            }
        }
    }
    // **What the scope tree says** about the two names, which is where a fact's scope comes from.
    for needle in ["assert_invariant", "set_parents", "also_after"] {
        let Some(at) = view.source.find(needle) else {
            continue;
        };
        match view.scopes.binding_declared_at(at) {
            Some(binding) => println!(
                "[scope] {needle} at {at}: kind {:?}, scope {:?}, qualified {:?}",
                binding.kind,
                binding.scope,
                view.scopes.qualified_name_of(binding.scope)
            ),
            None => println!("[scope] {needle} at {at}: no binding"),
        }
    }
    // **What the parser reported**, which is the direct evidence for a body that runs away.
    let text = view.source.to_string();
    let errors = view.tree.get_errors();
    println!("[errors] {} error(s)", errors.len());
    for error in errors.iter().take(8) {
        let at = cpp_parser::source_range(error.range).start_offset;
        let line = text[..at.min(text.len())].lines().count();
        println!(
            "[errors] line {line} at {at}: {} — `{}`",
            error.message,
            text.get(at.saturating_sub(60)..(at + 40).min(text.len()))
                .unwrap_or("")
        );
    }
    for needle in [
        "assert_invariant",
        "set_parents",
        "static basic_json parse",
        "void dump",
    ] {
        let Some(at) = rendering.find(needle) else {
            println!("{needle}: not in the rendering");
            continue;
        };
        let mut chain = Vec::new();
        if let Some(token) = cpp_parser::token_at(&view.root, at) {
            let mut node = token.parent();
            while let Some(next) = node {
                chain.push(format!("{:?}", next.kind()));
                node = next.parent();
            }
        }
        println!("{needle} at {at}: {}", chain.join(" < "));
    }
}
/// **An access specifier written as a macro** — `nlohmann`'s `JSON_PRIVATE_UNLESS_TESTED:` — and the members after
/// it. The tree keeps every one of them inside the class body (measured on `json.hpp`: 151 children, `object` and
/// `dump` among them), so if a later member loses its class scope the loss is in the declaration walk and not in the
/// parse.
#[test]
fn a_member_after_a_macro_access_specifier_is_still_a_member() {
    let source = "\
#define PRIVATE_UNLESS_TESTED private

struct Widget {
    void before() {}

    PRIVATE_UNLESS_TESTED:
    void after() {}

    int also_after = 0;
};
";
    let session = session_of(&[("/p/widget.h", source)]);
    assert_eq!(
        declared_at(&session, "/p/widget.h", "after"),
        "Widget::after",
        "the member after the macro access specifier is Widget's"
    );
    assert_eq!(
        declared_at(&session, "/p/widget.h", "also_after"),
        "Widget::also_after",
        "and so is the one after that"
    );
}
/// **`json.hpp`'s `assert_invariant` body, verbatim in shape** — the lambda inside `JSON_ASSERT`, wrapped in
/// `JSON_TRY { … } JSON_CATCH(...) {}` under an `#if`, and the member written after the method.
#[test]
fn a_member_after_a_lambda_inside_a_macro_try_catch_is_still_a_member() {
    let source = "\
#define JSON_ASSERT(x) assert(x)
#define JSON_TRY try
#define JSON_CATCH(...) catch (__VA_ARGS__)

struct Widget {
    int m_type = 0;
    struct { int* object; } m_value;

    void assert_invariant(bool check_parents = true) const noexcept
    {
        JSON_ASSERT(m_type != 0 || m_value.object != nullptr);

#if JSON_DIAGNOSTICS
        JSON_TRY
        {
            /// cppcheck-suppress assertWithSideEffect
            JSON_ASSERT(!check_parents || !is_structured() || std::all_of(begin(), end(), [this](const Widget & j)
            {
                return j.m_type == this->m_type;
            }));
        }
        JSON_CATCH(...) {}
#endif
        static_cast<void>(check_parents);
    }

    void after_the_method() {}

    int also_after = 0;
};
";
    let session = session_of(&[("/p/widget.h", source)]);
    assert_eq!(
        declared_at(&session, "/p/widget.h", "after_the_method"),
        "Widget::after_the_method",
        "the member after the method is Widget's"
    );
    assert_eq!(
        declared_at(&session, "/p/widget.h", "also_after"),
        "Widget::also_after",
        "and so is the one after that"
    );
}
/// **The fragment the real parser reports an error on** — a lambda whose body closes into `}))`, inside a macro call.
///
/// `json.hpp:1267` reports `expected }, but get ;` at the `}));`, and the recovery that follows is what swallows the
/// rest of `basic_json`. This test asks the parser alone, with no class around it.
#[test]
fn a_lambda_inside_a_macro_argument_closes_without_an_error() {
    let source = "\
void probe(bool check_parents) {
    JSON_ASSERT(!check_parents || !is_structured() || std::all_of(begin(), end(), [this](const basic_json & j)
    {
        return j.m_parent == this;
    }));
}
";
    let tree = cpp_parser::CppParser::parse(source, cpp_parser::ParserConfig::default());
    let errors = tree.get_errors();
    println!("{} error(s)", errors.len());
    for error in errors {
        let at = cpp_parser::source_range(error.range).start_offset;
        println!(
            "at {at}: {} — `{}`",
            error.message,
            source
                .get(at.saturating_sub(50)..(at + 30).min(source.len()))
                .unwrap_or("")
        );
    }
    assert!(errors.is_empty(), "the lambda closes: {errors:?}");
}

/// **A class body survives a method it cannot read.** The recovery inside a broken body must end at the body's own
/// `}`, not at the class's — otherwise the members written after the mistake are declarations of nothing, which is
/// the failure `json.hpp` shows at 333 362 bytes.
#[test]
fn a_member_after_a_broken_method_body_is_still_a_member() {
    let source = "\
struct Widget {
    int before = 0;

    void broken() {
        FOO { @@@ }
    }

    void after_the_mistake() {}

    int also_after = 0;
};
";
    let session = session_of(&[("/p/widget.h", source)]);
    assert_eq!(
        declared_at(&session, "/p/widget.h", "after_the_mistake"),
        "Widget::after_the_mistake",
        "the member after a broken body is Widget's"
    );
    assert_eq!(
        declared_at(&session, "/p/widget.h", "also_after"),
        "Widget::also_after",
        "and so is the one after that"
    );
}
