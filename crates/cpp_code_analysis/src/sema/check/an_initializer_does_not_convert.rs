//! **An initialiser that does not convert to the type it is assigned to.**
//!
//! ```cpp
//! int count = "three";        // no conversion from `const char*` to `int`
//! ```
//!
//! # One relation, two checks
//!
//! The question is not "is a string a number" — that was the shape of the first version of this check, and it was
//! the wrong shape: a special case dressed as a rule, which would have needed a sibling for every pair of types.
//! What it asks is [`Type::convertible_to`], the crate's single answer to *can a value of this type initialise one
//! of that type* — and [`super::an_argument_does_not_convert`] asks the same relation at a call site. One question,
//! one implementation, which is the rule this crate keeps having to relearn.
//!
//! # What is reported, and what is refused
//!
//! Only [`Known::No`](crate::Known::No) — a conversion the standard does not provide, between two types whose shapes
//! are both known. Everything else is silence, and the relation's own documentation lists what silence covers:
//! anything named (a class may have a converting constructor, and `std::string s = "x";` is the commonest line in
//! modern C++), anything depending on a template parameter, and any pair of different pointer types.
//!
//! # Where the two types come from
//!
//! The declared one is the summary's fact; the initialiser's is [`type_of_expression`](crate::ProjectIndex), the
//! same engine `auto` deduction is built on. Neither costs a parse: the tree was parsed to produce the summary, and
//! the scopes came with it.

use cpp_parser::CppSyntaxKind;

use crate::Known;
use crate::sema::types::parse_type_spelling;

use super::{Checks, Finding};

/// The name this check reports under — see [`Finding::check`].
pub const CHECK: &str = "an_initializer_does_not_convert";

/// Every declaration whose initialiser's type cannot convert to the declared type.
pub fn an_initializer_does_not_convert(checks: &Checks<'_>) -> Vec<Finding> {
    // **Every initialiser in the file, found in one walk of the tree** — not one walk per declaration.
    //
    // The first version asked `root.descendants()` for each fact's own initialiser, which is the whole tree scanned
    // once *per declaration*: quadratic in the size of the file. Measured against MSVC's headers, that check alone
    // cost **6 430 ms over twenty files where the other checks together cost 396 ms** — 94% of the layer, paid per
    // keystroke by a diagnostics channel whose whole contract is that it answers from what it already has.
    //
    // The tree is the same either way, so walking it once costs what walking it once per fact cost once.
    let initializers: Vec<(cpp_parser::SourceRange, cpp_parser::CppSyntaxNode)> = checks
        .tree
        .descendants()
        .filter(|node| CppSyntaxKind::from(node.kind()) == CppSyntaxKind::Initializer)
        .map(|node| (cpp_parser::source_range(node.text_range()), node))
        .collect();

    // **And the search is a backward scan from a binary-searched bound, not a scan of the whole list.** Collecting
    // the list once removed the *tree* walk per declaration and left the *list* scan per declaration, which is the
    // same defect one layer down: MSVC's `<vector>` has 20 247 nodes, and `find` over every initialiser in the file
    // for every variable in the file is quadratic in the same way the tree walk was. Measured, and this is the whole
    // of a diagnostics call: **3516 ms of 3562 ms**, on every request, for one file.
    //
    // `partition_point` gives the first initialiser that starts *after* the fact, because `descendants()` is a
    // pre-order walk and the list is therefore in offset order. Everything that could contain the fact is before
    // that point, and the search walks back from it.
    //
    // **Backwards, because the answer must be the innermost.** A `find` from the front takes the *outermost*
    // initialiser that contains the declaration — an enclosing aggregate's, for a member inside it — and the
    // initialiser whose type is being converted is the one written on this declaration. That was a wrong answer
    // rather than a slow one, hidden behind the same line.
    let mut findings = Vec::new();

    if std::env::var_os("CPPLS_TRACE_DIAGNOSTICS").is_some() {
        eprintln!(
            "        {} initializer(s): {:?}",
            initializers.len(),
            initializers
                .iter()
                .map(|(r, _)| (r.start_offset, r.end_offset()))
                .collect::<Vec<_>>()
        );
    }

    for fact in &checks.summary.declarations {
        // **A variable with a written type and an initialiser.** A function's return type is a different question
        // (`return` statements), and a fact with no `type_of` has nothing to convert to.
        if fact.kind != crate::DeclKind::Variable {
            continue;
        }
        let Some(declared) = fact.type_of.as_deref() else {
            continue;
        };
        // A placeholder has no type to compare against — deduction is the question there, not conversion.
        if matches!(declared, "auto" | "decltype(auto)") {
            continue;
        }

        // **A binary search on the fact's own start**, then a walk back over the initialisers that begin at or
        // after it — a handful, because those are the ones written *inside* the range being examined and not every
        // initialiser in the file.
        //
        // `<=` and not `<`, and the inclusion of the boundary is the whole of the correctness: a fact's range is its
        // **name** (`count` at 17..23 in `int count = "three";`) while the initialiser written for it begins after
        // that range ends (25..32), so a strict partition drops exactly the initialiser the check is about. Measured:
        // on `<` the check finds nothing and `the_type_check_fires_on_the_line_it_is_about` goes from one finding to
        // zero.
        //
        // Measured the other way round as well, which is why the bound is the **start** and not the end: partitioning
        // on `fact.range.end_offset()` reaches back over most of the file for a short declarator — the same fixture —
        // and the check costs **3430 ms** instead of 2 ms.
        // **A binary search, then a walk forward that stops at the first candidate the fact does not contain.**
        // The list is in offset order — `descendants()` is a pre-order walk — so the partition point is where it
        // passes the fact's start, and everything that can lie inside the fact's range is at or after that point.
        //
        // `<=` and not `<`, and the boundary is the whole of the correctness: a fact's range covers its
        // **declarator and its initialiser** (`count = "three"` at 17..32 for `int count = "three";`), while the
        // `Initializer` node is only the value (25..32). A strict partition drops exactly the initialiser the check
        // is about, because it begins *after* the fact's start.
        //
        // **Forward, and bounded.** The walk stops at the first initialiser that starts inside the fact's range and
        // is not contained by it, which is the end of this declaration's own initialisers: how many are visited is a
        // property of the declaration (its array bounds, its nested braces), not of the file. Walking *backwards*
        // from the partition is unbounded — measured, `partition_point` on the fact's `end` gave a slice nearly the
        // whole file, and the check cost **3527 ms** again.
        let after = initializers.partition_point(|(range, _)| {
            range.start_offset <= fact.range.start_offset
        });

        let initializer = initializers[after..]
            .iter()
            .take_while(|(range, _)| range.start_offset < fact.range.end_offset())
            .find(|(range, _)| range.end_offset() <= fact.range.end_offset())
            .map(|(_, node)| node);

        // Bounded by how deeply initialisers nest, which is a handful: the walk back stops at the first ancestor,
        // and an initialiser that starts before the fact but does not contain it ends the search for this fact
        // rather than continuing to the front of the file.
        let Some(initializer) = initializer else {
            continue;
        };
        let Some(expression) = initializer.children().last() else {
            continue;
        };
        // **The number that matters, and it is not the search.** Every variable that gets this far pays a
        // `type_of_expression`, and that call is the check's cost — measured on MSVC's `<vector>`: with the search
        // returning nothing the check is **2 ms**, and with it finding initialisers the check is **3537 ms**. The
        // search is not what changed between those two runs; whether the inference runs at all is.
        let inferring = std::time::Instant::now();
        let typed = crate::index::project::type_of_expression(
            checks.index,
            &mut |_: &std::path::Path| None,
            checks.scopes,
            checks.tree,
            checks.path,
            &expression,
            0,
        );
        if std::env::var_os("CPPLS_TRACE_DIAGNOSTICS").is_some() {
            eprintln!(
                "        infer {:?} : {} -> {} ms",
                fact.name,
                declared,
                inferring.elapsed().as_millis()
            );
        }
        let Known::Yes((from, _)) = typed else {
            // The initialiser's type is not known — a name from a header nobody read, a call whose return type is
            // deduced, a template parameter. **Nothing is reported**: see the module documentation.
            continue;
        };

        let Known::No = from.convertible_to(&parse_type_spelling(declared)) else {
            continue;
        };

        findings.push(Finding {
            range: cpp_parser::source_range(expression.text_range()),
            name: fact.name.clone(),
            check: CHECK,
            message: format!(
                "`{}` has type `{declared}`, and `{}` does not convert to it",
                fact.name,
                from
            ),
        });
    }

    findings
}
