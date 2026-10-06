// **What the session's own macro environment says** about the three names that decide `#if _HAS_CXX20`:
//   `_MSVC_LANG` (a compiler built-in) -> `_STL_LANG` -> `_HAS_CXX20` -> the guarded `#include <atomic>` in
//   `memory`, which is the edge the visibility walk drops and the cook keeps.
//
// It asks the same way `Session::open` builds a server's session, so the answer is the one a user gets.
use cpp_code_analysis::{DiskFiles, MacroValues, OpenDocuments, Session, SessionFiles, WatchFilter};

fn main() {
    let inc = std::env::args().nth(1).expect("include dir");
    let memory = std::path::PathBuf::from(&inc).join("memory");

    let mut session = Session::open(
        std::path::PathBuf::from(&inc),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(std::path::Path::new(".")),
    );
    session.index_everything();

    let index = session.index();

    // **At the offset of the guarded include itself**, which is where the walk asks: `_HAS_CXX20` is defined by
    // `vcruntime.h`, so only a state that has already walked the includes can answer it. Offset 0 cannot, and
    // asking there is how the first version of this probe reported the name as undefined.
    let source = std::fs::read_to_string(&memory).expect("memory is readable");
    // **And the same question in `vcruntime.h`**, at the `#ifdef __cplusplus` that decides `_STL_LANG`. If the two
    // files disagree about `__cplusplus`, the disagreement is the bug and its location is the file boundary.
    let runtime = std::path::PathBuf::from(&inc).join("vcruntime.h");
    let runtime_source = std::fs::read_to_string(&runtime).expect("vcruntime.h is readable");
    let guard = runtime_source
        .find("_STL_LANG > 201402L")
        .or_else(|| runtime_source.find("_STL_LANG > 201703L"))
        .expect("vcruntime.h compares _STL_LANG");
    println!("  vcruntime.h: the `_STL_LANG > …` comparison is at offset {guard}");
    for offset in [0usize, guard] {
    println!("  --- at offset {offset} ---");
    for name in ["__cplusplus", "_MSVC_LANG", "_STL_LANG", "_HAS_CXX17"] {
        let macros = index.macros_at(&runtime, offset);
        let said = match macros.lookup(name) {
            cpp_code_analysis::Lookup::Defined(definition) => {
                let body: Vec<&str> = definition.body.significant().map(|token| token.text()).collect();
                format!("= {}", body.join(" "))
            }
            cpp_code_analysis::Lookup::DefinedWithoutAValue => "defined, no body".to_string(),
            cpp_code_analysis::Lookup::Undefined => "**UNDEFINED**".to_string(),
            cpp_code_analysis::Lookup::Unanswered => "**UNANSWERED**".to_string(),
        };
        println!("    {name:<14} {said}");
    }
    }
    let at = source.find("#include <atomic>").expect("memory includes atomic");
    println!(
        "  the guarded include is at offset {at}, line {}",
        source[..at].matches('\n').count() + 1
    );

    // `macros_at` is what the visibility walk evaluates a guard against, so its answer is the one that decides.
    for name in ["_MSVC_LANG", "__cplusplus", "_STL_LANG", "_HAS_CXX17", "_HAS_CXX20"] {
        let macros = index.macros_at(&memory, at);
        let said = match macros.lookup(name) {
            cpp_code_analysis::Lookup::Defined(definition) => {
                let body: Vec<&str> = definition.body.significant().map(|token| token.text()).collect();
                format!("= {}", body.join(" "))
            }
            cpp_code_analysis::Lookup::DefinedWithoutAValue => "defined, no body".to_string(),
            cpp_code_analysis::Lookup::Undefined => "**UNDEFINED**".to_string(),
            cpp_code_analysis::Lookup::Unanswered => "**UNANSWERED**".to_string(),
        };
        println!("  index.macros_at(memory): {name:<16} {said}");
    }
}
