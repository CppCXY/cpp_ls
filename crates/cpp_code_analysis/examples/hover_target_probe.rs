//! **What a cursor on a name resolves to** — the two shapes a reader reported as wrong.
//!
//! ```text
//! cargo run --release -p cpp_code_analysis --example hover_target_probe
//! ```
//!
//! 1. **The injected class name.** Inside `struct X { X(const X&); }` the word `X` names the *class*, not the
//!    constructor — and the constructor is also a binding called `X` in the class's own scope, which is what a
//!    name lookup finds first.
//! 2. **A scoped enumerator.** `enum class E { Yes };` used as `E::Yes` from another file: the name has to be
//!    found through the enum's scope, in the index.
//!
//! Both are printed as the analysis answers them, so a fix can be measured rather than argued about.

use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::{CompilerConfig, Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter};
use std::path::Path;

fn main() {
    injected_class_name();
    scoped_enumerator();
    variable_template();
}

fn session_with(files: &[(&str, &str)]) -> Session<MemoryFiles> {
    let mut memory = MemoryFiles::new();
    for (path, text) in files {
        memory = memory.with_file(*path, *text);
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files(files.iter().map(|(path, _)| std::path::PathBuf::from(*path)));
    for (path, _) in files {
        session.want_cooked_reading(Path::new(path));
    }
    session.index_everything();
    session
}

fn ask(session: &Session<MemoryFiles>, path: &str, what: &str, text: &str) {
    let Some(view) = session.view(path) else {
        println!("   {what}: no view");
        return;
    };
    let Some(at) = text.find(what) else {
        println!("   {what}: not in the fixture");
        return;
    };
    let Some(reading) = view.reading_offset_of(FileOffset::new(at)) else {
        println!("   {what}: not in the rendering");
        return;
    };

    match session.definition(&view, reading) {
        Known::Yes(found) => println!(
            "   `{what}` at {at}: {:?} `{}` in {}",
            found.fact.kind,
            found.fact.qualified_name(),
            found.file.display()
        ),
        Known::Unknown(reason) => println!("   `{what}` at {at}: Unknown({reason:?})"),
        Known::No => println!("   `{what}` at {at}: No"),
    }
}

fn injected_class_name() {
    println!("\n--- the injected class name ---");
    let source = "\
struct TrivialityTester {
    TrivialityTester() = default;
    TrivialityTester(const TrivialityTester& other) noexcept {}
    TrivialityTester& operator=(const TrivialityTester& other);
};
";
    let session = session_with(&[("/p/test.cpp", source)]);
    // The `TrivialityTester` **inside the parameter list** — that is the injected class name, and it must resolve to
    // the class rather than to the constructor declared three lines above.
    ask(&session, "/p/test.cpp", "TrivialityTester& other", source);
    let inside = source.find("const TrivialityTester& other").map(|at| at + "const ".len());
    if let Some(at) = inside {
        let view = session.view("/p/test.cpp").expect("held");
        if let Some(reading) = view.reading_offset_of(FileOffset::new(at)) {
            match session.definition(&view, reading) {
                Known::Yes(found) => println!(
                    "   parameter's `TrivialityTester` at {at}: {:?} `{}`",
                    found.fact.kind,
                    found.fact.qualified_name()
                ),
                Known::Unknown(reason) => println!("   parameter's `TrivialityTester` at {at}: Unknown({reason:?})"),
                Known::No => println!("   parameter's `TrivialityTester` at {at}: No"),
            }
        }
    }
}

fn scoped_enumerator() {
    println!("\n--- a scoped enumerator in another file ---");
    let header = "\
namespace std {
enum class IsTriviallyCopyConstructible { Yes, No };
}
using std::IsTriviallyCopyConstructible;
";
    let source = "\
#include \"traits.h\"
int main() {
    constexpr bool same = IsTriviallyCopyConstructible::Yes;
    return same ? 0 : 1;
}
";
    let session = session_with(&[("/p/traits.h", header), ("/p/test.cpp", source)]);

    let at = source.find("IsTriviallyCopyConstructible::Yes").expect("the use");
    let name = at + "IsTriviallyCopyConstructible::".len();
    let view = session.view("/p/test.cpp").expect("held");
    for (label, offset) in [("the enum", at), ("the enumerator", name)] {
        let Some(reading) = view.reading_offset_of(FileOffset::new(offset)) else {
            println!("   {label}: not in the rendering");
            continue;
        };
        match session.definition(&view, reading) {
            Known::Yes(found) => println!(
                "   {label}: {:?} `{}` in {}",
                found.fact.kind,
                found.fact.qualified_name(),
                found.file.display()
            ),
            Known::Unknown(reason) => println!("   {label}: Unknown({reason:?})"),
            Known::No => println!("   {label}: No"),
        }
    }

    if let Some(header) = session.view("/p/traits.h") {
        for (index, scope) in header.scopes.scopes().iter().enumerate() {
            println!(
                "   scope {index}: {:?} name {:?} bindings {:?} qualified {:?}",
                scope.kind,
                scope.name,
                scope.bindings.iter().map(|binding| binding.name.text()).collect::<Vec<_>>(),
                header.scopes.qualified_name_of(cpp_code_analysis::ScopeId(index))
            );
        }
    }

    for (file, fact) in session.index().every_fact_named("Yes") {
        println!(
            "   fact `Yes`: scope {:?} kind {:?} qualified `{}` in {}",
            fact.scope,
            fact.kind,
            fact.qualified_name(),
            file.display()
        );
    }

    // And what the colouring says, which is the other half of the report.
    let names = session.classified_names(&view);
    for name in &names {
        let Some(text) = source.get(name.range.start_offset..name.range.end_offset()) else {
            continue;
        };
        if matches!(text, "Yes" | "IsTriviallyCopyConstructible") {
            println!("   classified `{text}` as {:?} ({:?})", name.kind, name.provenance);
        }
    }
}

fn variable_template() {
    println!("\n--- a variable template ---");
    let header = "\
namespace std {
template <class T, class U>
inline constexpr bool is_trivially_assignable_v = false;
}
using std::is_trivially_assignable_v;
";
    let source = "\
#include \"traits.h\"
int main() {
    static_assert(is_trivially_assignable_v<int, int&>);
    return 0;
}
";
    let session = session_with(&[("/p/traits.h", header), ("/p/test.cpp", source)]);
    let view = session.view("/p/test.cpp").expect("held");
    let names = session.classified_names(&view);
    for name in &names {
        let Some(text) = source.get(name.range.start_offset..name.range.end_offset()) else {
            continue;
        };
        if text == "is_trivially_assignable_v" {
            println!("   classified `{text}` as {:?} ({:?})", name.kind, name.provenance);
        }
    }

    let at = source.find("is_trivially_assignable_v<int").expect("the use");
    if let Some(reading) = view.reading_offset_of(FileOffset::new(at)) {
        match session.definition(&view, reading) {
            Known::Yes(found) => println!(
                "   definition: {:?} `{}`",
                found.fact.kind,
                found.fact.qualified_name()
            ),
            Known::Unknown(reason) => println!("   definition: Unknown({reason:?})"),
            Known::No => println!("   definition: No"),
        }
    }
}
