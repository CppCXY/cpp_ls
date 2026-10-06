//! **Does this name resolve, and if not, which layer said so?**
//!
//! Written for the family of `NotDeclaredHere` refusals that are not about types at all: measured on MSVC's
//! `<memory>`, ten `auto` declarations were refused because `_Locked_pointer::_Lock_and_load` (7),
//! `_Locked_pointer::_Unsafe_load_relaxed` (2) and `weak_ptr::_Rep` (1) could not be found — while the *class* was
//! found and the qualified name was spelled correctly, which means the member lookup is what failed.
//!
//! Every class in those headers is declared inside a namespace that a **macro** opens (`_STD_BEGIN` is
//! `namespace std {`), so the two candidate causes are:
//!
//! * **the cook** — the file that declares the member has not been cooked, so its facts carry no scope;
//! * **the lookup** — the facts are there and the qualified query does not match them.
//!
//! The difference is measurable rather than arguable: cook the file first, and ask again. `--cook` does that, and
//! without it the summary is the raw reading's.
//!
//! ```text
//! usage: lookup_probe <file> <qualified-name> [--cook]
//! ```
fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: lookup_probe <file> <qualified-name> [--cook]");
        std::process::exit(2);
    };
    let Some(name) = args.next() else {
        eprintln!("usage: lookup_probe <file> <qualified-name> [--cook]");
        std::process::exit(2);
    };
    let cook = args.any(|argument| argument == "--cook");

    if std::fs::read_to_string(&path).is_err() {
        eprintln!("cannot read the file");
        std::process::exit(2);
    }

    let path = std::path::PathBuf::from(&path);
    let root = path.parent().unwrap_or(std::path::Path::new(".")).to_path_buf();
    let mut session = cpp_code_analysis::Session::open(
        root,
        cpp_code_analysis::SessionFiles::new(
            cpp_code_analysis::OpenDocuments::new(),
            cpp_code_analysis::DiskFiles,
        ),
        cpp_code_analysis::WatchFilter::new(std::path::Path::new(".")),
    );

    session.index_everything();

    if cook {
        // **The unit cooked, which is what the pump does for an open file.** `cook` is where the macro-opened
        // namespaces are resolved: the cooked reading knows `_STD_BEGIN` is `namespace std {`, while the raw one
        // files every declaration inside it as if it were at file scope.
        session.cook(&path);
    }

    let _ = session.view(&path);
    if session.view(&path).is_none() {
        eprintln!("the session holds no view of {}", path.display());
        std::process::exit(2);
    }

    // **What the file's own facts say about the name**, which is what a lookup matches on. Printed before the
    // lookup so that "the fact is not there" and "the fact is there and was not matched" cannot be confused — the
    // distinction the whole probe exists for.
    let last = name.rsplit("::").next().unwrap_or(&name);
    let mut facts = 0;
    for fact in session.index().summary(&path).map(|summary| summary.declarations.as_slice()).unwrap_or(&[]) {
        if fact.name == last {
            facts += 1;
            println!(
                "  fact: name={:<28} scope={:<28} kind={:?} qual={}",
                fact.name,
                fact.scope.as_deref().unwrap_or("<none>"),
                fact.kind,
                fact.qualified_name()
            );
        }
    }
    println!("  {facts} fact(s) spelled `{last}` in this file");

    // **The half that decides whether the retry can happen**: does the CLASS resolve, and to what qualified name?
    if let Some((class, _)) = name.rsplit_once("::") {
        match session.index().definition(class, &path) {
            cpp_code_analysis::Known::Yes(found) => println!(
                "  class `{class}` -> qual=`{}` in {}",
                found.fact.qualified_name(),
                found.file.display()
            ),
            cpp_code_analysis::Known::No => println!("  class `{class}` -> NO"),
            cpp_code_analysis::Known::Unknown(reason) => {
                println!("  class `{class}` -> UNKNOWN {}", reason.describe());
            }
        }
        // **All the declarations the bare name finds**, in order — the step that refuses takes only the first, so
        // what the rest of the list holds is the question.
        let declaring = session.index().files_declaring("_Locked_pointer", &path);
        println!("  files_declaring(_Locked_pointer) -> {} candidate(s)", declaring.len());
        for found in declaring.iter().take(4) {
            println!(
                "    scope={:<28} qual={:<32} kind={:?} in {}",
                found.fact.scope.as_deref().unwrap_or("<none>"),
                found.fact.qualified_name(),
                found.fact.kind,
                found.file.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default()
            );
        }

        // **Which spelling of the class the query actually carries.** The object's type is `_Locked_pointer<_Ty>`,
        // not `_Locked_pointer`, and that difference decides whether a retry keyed on the class can even happen.
        for spelling in ["_Locked_pointer", "_Locked_pointer<_Ty>", "std::_Locked_pointer"] {
            let members = session.index().declarations_in(spelling, &path).len();
            let resolves = match session.index().definition(spelling, &path) {
                cpp_code_analysis::Known::Yes(found) => {
                    format!("qual={}", found.fact.qualified_name())
                }
                cpp_code_analysis::Known::No => "NO".to_string(),
                cpp_code_analysis::Known::Unknown(reason) => {
                    format!("UNKNOWN {}", reason.describe())
                }
            };
            println!("  `{spelling}`: declarations_in={members}  definition={resolves}");
        }
    }

    match session.index().kind_of(&name, &path) {
        cpp_code_analysis::Known::Yes(found) => {
            println!("  lookup `{name}` -> YES {:?}", found.kind);
        }
        cpp_code_analysis::Known::No => println!("  lookup `{name}` -> NO"),
        cpp_code_analysis::Known::Unknown(reason) => {
            println!("  lookup `{name}` -> UNKNOWN {}", reason.describe());
        }
    }
}
