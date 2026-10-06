//! **One question, asked of every macro environment the analysis has.**
//!
//! There is one condition evaluator (`Branch::holds` → `Region::visibility` → `Guard::visibility`) and there are
//! eight `MacroValues` implementations behind it. A guard is therefore decided by whichever environment its caller
//! handed over, and two callers asking the same question about the same position can disagree.
//!
//! This asks the one question that decides `_STL_LANG` — *is `__cplusplus` defined here* — of the two that matter:
//!
//! * `UnitMacros`, which the **cook** and the unit walk use, over the unit's own state;
//! * `Marked`, which `ProjectIndex::macros_at` builds, and which every **query** reads.
//!
//! ```text
//! usage: env_probe <include-dir> <file> <offset> <name>
//! ```
use cpp_code_analysis::MacroValues;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(dir) = args.first() else {
        eprintln!("usage: env_probe <include-dir> <file> <offset> <name>");
        std::process::exit(2);
    };
    let file = args.get(1).cloned().unwrap_or_else(|| "vcruntime.h".to_string());
    let offset: usize = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(0);
    let name = args.get(3).cloned().unwrap_or_else(|| "__cplusplus".to_string());

    let path = std::path::PathBuf::from(dir).join(&file);
    let mut session = cpp_code_analysis::Session::open(
        std::path::PathBuf::from(dir),
        cpp_code_analysis::SessionFiles::new(
            cpp_code_analysis::OpenDocuments::new(),
            cpp_code_analysis::DiskFiles,
        ),
        cpp_code_analysis::WatchFilter::new(std::path::Path::new(".")),
    );
    session.index_everything();

    // **The query path's answer**: what `macros_at` builds, which is what every guard in `visibility_of` is decided
    // against when a name is looked up.
    let marked = session.index().macros_at(&path, offset);
    match marked.lookup(&name) {
        cpp_code_analysis::Lookup::Defined(definition) => {
            let body: Vec<&str> = definition.body.significant().map(|t| t.text()).collect();
            println!("  Marked (the query path)      {name} -> Defined  body = {:?}", body.join(" "));
        }
        other => println!("  Marked (the query path)      {name} -> {other:?}"),
    }

    // **And the seed alone**, so that "the walk lost it" and "it was never there" are different answers.
    println!(
        "  index.macros (the seed)      {name} -> {:?}",
        session.index().macros().lookup(&name)
    );

    // How many names the walk's state holds at all, which says whether the walk ran for this file.
    println!("  the index holds {} file(s)", session.index().len());

    // **And the facts themselves, with their guards.** This is the half that decides `last_word_about`: it accepts
    // only `Unconditional` facts and takes the last one, so a `#define` written in a branch that is *not* compiled
    // wins the answer if its guard was lost on the way into the summary.
    if let Some(summary) = session.index().summary(&path) {
        let wanted = name.rsplit("::").next().unwrap_or(&name);
        let mut found = 0;
        for fact in &summary.macros {
            if &*fact.name == wanted {
                found += 1;
                println!(
                    "    fact at {:>6}  guard={:?}  kind={:?}  value={:?}  alias={:?}",
                    fact.range.start_offset, fact.guard, fact.kind, fact.value, fact.alias
                );
            }
        }
        println!("  {found} fact(s) named `{wanted}` in the indexed summary");
        println!("  **own_guard = {:?}**", summary.guards.own_guard);
        for region in [32u32, 33] {
            let Some(named) = summary.guards.region_at(region, 7422) else {
                println!("    region {region} -> None");
                continue;
            };
            println!(
                "    region {region} at 7422 -> active_branch={} (of {} branch(es))",
                named.active_branch,
                named.branches.len()
            );

            // **What each branch says there**, which is the whole of the verdict: `Region::visibility` asks the
            // branches *before* the active one, and the answer for an `#else` is the opposite of the first that
            // holds.
            let condition_at = summary
                .guards
                .span_of(region)
                .map(|span| span.start_offset)
                .unwrap_or(0);
            let macros = cpp_code_analysis::index::environment::MacrosHere::from_summary(
                session.index().macros_at(&path, condition_at),
                summary,
                condition_at,
            );
            for (index, branch) in named.branches.iter().enumerate() {
                println!(
                    "      branch {index} {:?} holds={:?}",
                    branch.kind,
                    branch.holds(&macros)
                );
            }
            println!("      => visibility {:?}", named.visibility(&macros));
        }
    }
}
