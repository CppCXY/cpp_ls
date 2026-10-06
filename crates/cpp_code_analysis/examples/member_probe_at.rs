//! **What class does the member path think `obj.member` is?**
//!
//! The lookup that refuses `_Locked_pointer::_Lock_and_load` builds that name from the type of the object, and the
//! type is whatever the reader that ran before it produced. Three spellings have been tried by hand
//! (`_Locked_pointer`, `_Locked_pointer<_Ty>`, `std::_Locked_pointer`) and the answer was unchanged, so the guess
//! about which one arrives is the thing to stop guessing about.
//!
//! This asks the analysis the same question the refusal is about — `member_across_files`, at the offset of the
//! member itself — and prints the layers under it: the object's type, the class derived from it, and whether the
//! class resolves.
//!
//! ```text
//! usage: member_probe_at <file> <line> <column-of-the-member-name> [--cook]
//! ```
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: member_probe_at <file> <line> <column> [--cook]");
        std::process::exit(2);
    };
    let offset: usize = args.get(1).and_then(|value| value.parse().ok()).unwrap_or(0);
    let cook = args.iter().any(|argument| argument == "--cook");

    let source = std::fs::read_to_string(path).expect("the file is readable");
    let path = std::path::PathBuf::from(path);

    // **A byte offset, handed in.** The first version took a line and a column and counted lines itself, and
    // counted them as `\n` — which is one byte short per line in a file that ends its lines with `\r\n`, so it
    // landed four thousand bytes early and reported "not a member access at that offset". An offset has no such
    // arithmetic in it.
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
        session.cook(&path);
    }

    let Some(view) = session.view(&path) else {
        eprintln!("no view");
        std::process::exit(2);
    };

    println!("  offset {offset}: {:?}", &source[offset.saturating_sub(20)..(offset + 20).min(source.len())]);

    // **The object's type first**, because that is what the class spelling is derived from — and the class spelling
    // is the one thing three hand-guesses could not settle.
    if let Some(access) = cpp_code_analysis::sema::resolve::member_access_at(&view.root, offset) {
        println!("  object: {:?}", access.object.text().to_string().trim());
        match session.type_at(&view, usize::from(access.object.text_range().start())) {
            cpp_code_analysis::Known::Yes(found) => println!("  object type -> {}", found.type_of),
            cpp_code_analysis::Known::No => println!("  object type -> NO"),
            cpp_code_analysis::Known::Unknown(reason) => {
                println!("  object type -> UNKNOWN {}", reason.describe());
            }
        }
    } else {
        println!("  not a member access at that offset");
    }

    match cpp_code_analysis::member_across_files(
        session.index(),
        &mut |path| session.view(path),
        &view.scopes,
        &view.root,
        &view.path,
        offset,
    ) {
        cpp_code_analysis::Known::Yes(found) => {
            println!("  member -> {} in {}", found.fact.qualified_name(), found.file.display());
        }
        cpp_code_analysis::Known::No => println!("  member -> NO"),
        cpp_code_analysis::Known::Unknown(reason) => {
            println!("  member -> UNKNOWN {}", reason.describe());
        }
    }
}
