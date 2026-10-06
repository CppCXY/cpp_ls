// Does the index know `memory` includes `atomic`, and does it hold atomic's declarations at all?
use cpp_code_analysis::{DiskFiles, OpenDocuments, Session, SessionFiles, WatchFilter};

fn main() {
    let inc = std::env::args().nth(1).expect("the include directory");
    let memory = std::path::PathBuf::from(&inc).join("memory");

    let mut session = Session::open(
        std::path::PathBuf::from(&inc),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(std::path::Path::new(".")),
    );
    session.index_everything();

    let index = session.index();
    println!("  the index holds {} file(s)", index.len());

    let atomic = std::path::PathBuf::from(&inc).join("atomic");
    println!("  atomic: summary={} declaration(s)", index.summary(&atomic).map(|s| s.declarations.len()).unwrap_or(0));

    match index.summary(&atomic) {
        Some(summary) => {
            let found: Vec<&cpp_code_analysis::DeclFact> = summary
                .declarations
                .iter()
                .filter(|fact| fact.name == "_Locked_pointer")
                .collect();
            println!("  `_Locked_pointer` facts in atomic: {}", found.len());
            for fact in found.iter().take(3) {
                println!("    scope={:?} qual={} kind={:?}", fact.scope, fact.qualified_name(), fact.kind);
            }
        }
        None => println!("  atomic has NO summary"),
    }

    let includers = index.includers_of(&atomic);
    println!("  includers_of(atomic): {}", includers.len());
    for path in includers.iter() {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mark = if name == "memory" { "  <-- memory IS an includer" } else { "" };
        println!("    {name}{mark}");
    }
    println!(
        "  memory itself is an includer of atomic: {}",
        includers.iter().any(|path| path.file_name().map(|n| n.to_string_lossy().to_string()).as_deref() == Some("memory"))
    );

    // **How many hops the reverse walk needs.** `memory` writes `#include <atomic>` directly, so if the edge were
    // there the pair would be one step apart; a `memory` that is missing from the list means the *edge* is missing
    // rather than the walk being short.
    println!(
        "  files_declaring(_Locked_pointer) from memory: {}",
        index.files_declaring("_Locked_pointer", &memory).len()
    );
    println!(
        "  files_declaring(std::_Locked_pointer) from memory: {}",
        index.files_declaring("std::_Locked_pointer", &memory).len()
    );
    println!(
        "  files_declaring(_Locked_pointer) from atomic: {}",
        index.files_declaring("_Locked_pointer", &atomic).len()
    );

    // **The forward closure, which is what visibility actually reads.** `includers_of` is the *reverse* edge and it
    // holds `memory`; the walk that answers a query goes the other way, from `memory` to what it sees. Both are
    // derived from one graph, so one direction working while the other does not is the thing to see.
    let visible: Vec<(String, cpp_code_analysis::IncludeVisibility)> = index.visible_files(&memory);
    println!("  visible_from(memory): {} file(s)", visible.len());
    let named = |entry: &(String, cpp_code_analysis::IncludeVisibility)| {
        std::path::Path::new(&entry.0)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    println!(
        "    atomic among them: {}",
        visible.iter().any(|entry| named(entry) == "atomic")
    );
    for entry in visible.iter() {
        let name = named(entry);
        if name.contains("atomic") || name.contains("Atomic") {
            println!("      {}  vis={:?}", entry.0, entry.1);
        }
    }
    println!("    (any entry whose path mentions atomic, above)");
    // The reverse edge again, with the exact spelling the graph holds.
    for path in index.includers_of(&atomic) {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if name == "memory" {
            println!("    the reverse edge spells it: {}", path.display());
        }
    }
}
