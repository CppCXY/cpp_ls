//! Read one file the way the indexer reads it, and print the facts one name produced.
//!
//! ```text
//! facts_probe <file> <name> [<name> …]
//! ```
//!
//! # Why this exists
//!
//! A name that cannot be found can be missing for two reasons that call for completely different work: the
//! index holds the declaration and the lookup cannot reach it, or the declaration never became a fact at all.
//! Answering that from a whole session costs a cook, an index and a probe that knows how to ask — and the
//! answer is one line: **what does the summary hold under this name**.
//!
//! This reads the file with the same call the indexer makes ([`cpp_code_analysis::summarize`]), so what it
//! prints is what the index would hold, with the offsets and the base clauses that decide every question
//! downstream. It parses no includes and expands no macros: the input is the text to be read, and the point is
//! to be able to hand it the **rendering** of a real header when that is what the analysis actually reads.
//!
//! ```text
//! facts_probe target/scratch/xm1/tu.cpp allocator_traits
//! ```

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: facts_probe <file> <name> [<name> …]");
        std::process::exit(2);
    };
    let wanted: Vec<String> = args.collect();

    let source = std::fs::read_to_string(&path).expect("the file is readable");
    let summary = cpp_code_analysis::summarize(
        std::path::Path::new(&path),
        &source,
        cpp_code_analysis::SummaryKey::new(0, 0),
    );

    println!(
        "{}: {} bytes, {} declarations, {} macros, {} includes",
        path,
        source.len(),
        summary.declarations.len(),
        summary.macros.len(),
        summary.includes.len()
    );

    if wanted.is_empty() {
        // No name asked for: the shape of the reading, which is what says whether a name is missing from a
        // hole the reader left or from a lookup that cannot reach it.
        let highest = summary
            .declarations
            .iter()
            .map(|fact| fact.range.end_offset())
            .max()
            .unwrap_or(0);
        println!("   highest declaration offset: {highest} of {} bytes", source.len());

        // **What the `auto`s are written with.** Deduction is the next piece of work and its shape decides how
        // much of a type system it needs: a declaration whose initializer is a call needs the callee's return
        // type, one whose initializer is a literal needs nothing at all, and one whose initializer is another
        // `auto` needs that one resolved first. Counted by **what follows the name**, which is the question,
        // rather than by total, which is not.
        let mut by_shape: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for fact in &summary.declarations {
            if fact.type_of.as_deref() != Some("auto") {
                continue;
            }
            let written = source
                .get(fact.range.start_offset..fact.range.end_offset())
                .unwrap_or_default();
            let shape = if !written.contains('=') {
                "no initializer — `auto` as a return type, or declared and assigned later"
            } else if written.contains('{') {
                "a braced initializer"
            } else if written.contains("static_cast")
                || written.contains("reinterpret_cast")
                || written.contains("const_cast")
            {
                "a cast"
            } else if written.contains('(') {
                "a call"
            } else {
                "a plain expression"
            };
            *by_shape.entry(shape).or_default() += 1;
        }
        if !by_shape.is_empty() {
            println!("   `auto` declarations, by what follows the name:");
            for (shape, count) in &by_shape {
                println!("      {count:5}  {shape}");
            }
        }
        return;
    }

    for name in &wanted {
        let found: Vec<&cpp_code_analysis::DeclFact> = summary
            .declarations
            .iter()
            .filter(|fact| &fact.name == name || fact.qualified_name() == *name)
            .collect();

        println!("`{name}`: {} fact(s)", found.len());
        for fact in found {
            println!(
                "   @{:<7} {:<44} kind {:?} bases {:?} type_of {:?} returns {:?}",
                fact.range.start_offset,
                fact.qualified_name(),
                fact.kind,
                fact.bases,
                fact.type_of,
                // **`returns` is where a function's `auto` lives**, and it was missing from this dump — which is
                // how a test that looked for `type_of` in both places spent a round reporting "`a_return_type` is
                // declared `auto`" about a fact that was right there.
                fact.returns
            );
        }
    }
}
