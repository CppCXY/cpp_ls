//! **What the editor's three requests actually get** on a real project: completion, hover and the signature.
//!
//! ```text
//! cargo run --release --example editor_probe -- <dir> [<file.cpp>]
//! ```
//!
//! The file is opened **as a buffer** with a few lines appended — no file on disk is touched — so the probe asks
//! about `#include <format>`, an `auto` variable and a `std::format(...)` call in the very project whose answers
//! were complained about:
//!
//! * `std::` — does the list contain `format`, `string`, `cout`? How many names does it offer at all?
//! * `std::string::` — a class template reached through an alias: a reader looking for `size()` types this every day.
//! * hover on an `auto` variable, and on a member whose type is written with the library's own macros.
//! * the signature of a call to a function declared in a header.
//!
//! Every answer is printed **with what the index knows beside it** (`declarations_in("std")`, is the header in the
//! closure, is `std::format` a declaration), because "completion is empty" and "the declaration is not there" are
//! different defects with the same symptom.

use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};
use std::path::PathBuf;

/// The lines appended to the file being probed: one question each.
const QUESTIONS: &str = r#"
void probe_auto() {
    std::string x;
    auto y = x.back();
    std::string::size_type n = 0;
    std::format("{}", n);
    std::string::iterator it;
}

void probe_member() {
    std::string s;
    s.append("x");
}
"#;

/// **Is this name declared somewhere the file is not allowed to see?**
///
/// The question the count above cannot answer on its own, and the difference between a gap and a refusal. A name
/// that a header the file never includes declares is one a lookup must not resolve, so counting it as work left
/// to do would aim the effort at a number that partly **should not** fall.
///
/// Asked through the graph rather than by matching spellings against the whole session, which is what two earlier
/// attempts did and why they disagreed: matching a bare name against every qualified name in the index answered
/// "held" for `value_type` because hundreds of unrelated classes have one, and the same check with the bare form
/// removed answered "not held" for names that are merely in another header. `visible_files` is the graph's own
/// answer and does not have either failure.
fn declared_out_of_sight<F: cpp_code_analysis::FileProvider + Clone>(
    session: &cpp_code_analysis::Session<F>,
    view: &cpp_code_analysis::FileView,
    name: &str,
    in_scope: Option<&str>,
) -> bool {
    let visible: std::collections::HashSet<String> = session
        .index()
        .visible_files(&view.path)
        .into_iter()
        .map(|(path, _)| path)
        .collect();

    // The spellings a lookup would have tried, in the order it tries them: the name under each enclosing scope,
    // outermost first, and then as written — the same walk `definition_where_written` makes.
    let mut spellings: Vec<String> = Vec::new();
    if let Some(scope) = in_scope {
        let segments: Vec<&str> = scope.split("::").collect();
        spellings.extend(
            (1..=segments.len())
                .rev()
                .map(|take| format!("{}::{name}", segments[..take].join("::"))),
        );
    }
    spellings.push(name.to_string());

    session.index().summaries().any(|summary| {
        let held = summary
            .declarations
            .iter()
            .any(|fact| spellings.iter().any(|wanted| *wanted == fact.qualified_name()));
        if !held {
            return false;
        }

        // Held **there**, and this file cannot reach the file it is held in: the name is real and out of sight.
        // The file that holds its own declarations is not "out of sight" of itself, so it is let through to the
        // check below rather than counted here.
        summary.path != view.path
            && !visible.contains(&cpp_code_analysis::normalize_path(
                std::path::Path::new(&summary.path),
                cfg!(windows),
            ))
    })
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("a directory"));
    let file = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("main.cpp"));

    let on_disk = std::fs::read_to_string(&file).expect("the file the probe was pointed at");
    // **The include the user's complaint is about**, added to the *buffer* rather than to their file. Prepended, so
    // the rest of the file reads exactly as it does on disk.
    let text = format!("#include <format>\n{on_disk}{QUESTIONS}");

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    // **As a buffer**, which is what the editor has: the probe must not edit the user's file to ask its questions.
    session.did_open(&file, &text);
    session.index_everything();

    let Some(view) = session.view(&file) else {
        println!("{} is not held", file.display());
        return;
    };

    // ---------------------------------------------------------------------------------------------
    // Is the header even in the program? `#include <format>` is the user's own line, and everything below is
    // moot if the closure never reached it — the two failures look identical from the completion list.
    // ---------------------------------------------------------------------------------------------
    let formats: Vec<PathBuf> = session
        .index()
        .summaries()
        .map(|summary| summary.path.clone())
        .filter(|path| path.to_string_lossy().contains("format"))
        .collect();
    println!("--- the program ---");
    // **What configuration the answers below are answers about**, before any of them: the language standard in
    // force, and whether the analysis chose it or the project stated it. Every `#if` in every header is a question
    // about this, and a project with no build configuration at all has nothing else to say it with.
    println!(
        "standard = {:?} | toolchain {:?} | note {:?}",
        session.config().standard.as_deref(),
        session.toolchain().and_then(|found| found.standard.as_deref()),
        session.toolchain().and_then(|found| found.note.as_deref()),
    );
    println!(
        "declarations_in(\"std\") = {} | files whose path mentions `format`: {}",
        session.index().declarations_in("std", &file).len(),
        formats.len()
    );

    // **The checks' answer, which on this fixture must be empty.** The file includes `<format>` and `<string>`
    // from a toolchain the session found, so every include resolves and no check has a claim to make. This is
    // the calibration `sema::check`'s module documentation describes: a check that fires on a header the
    // analysis read correctly is a check that will be wrong about the user's code too.
    match session.diagnostics(&file) {
        Some(diagnostics) => {
            println!(
                "diagnostics: reading {:?} | {} error(s), {} note(s), {} check(s), {} unplaced",
                diagnostics.reading,
                diagnostics.errors.len(),
                diagnostics.notes.len(),
                diagnostics.checks.len(),
                diagnostics.unplaced,
            );
            for finding in &diagnostics.checks {
                println!(
                    "   [{}] {}..{} {} — {}",
                    finding.check,
                    finding.range.start_offset,
                    finding.range.end_offset(),
                    finding.name,
                    finding.message
                );
            }
        }
        None => println!("diagnostics: none (the file is not indexed)"),
    }
    // **Why a name that is in a file in the closure is not in the index**: a header reached through `#include` has a
    // *raw* summary the moment it is parsed, and a *cooked* one only when something read it the way a compiler does —
    // and `std::format` is only `std::` after `_STD_BEGIN` (a macro in `yvals_core.h`) has been expanded.
    for path in &formats {
        let raw = session
            .index()
            .summary(path)
            .map(|summary| {
                summary
                    .declarations
                    .iter()
                    .filter(|fact| fact.name.contains("format"))
                    .map(|fact| format!("{}::{}", fact.scope.clone().unwrap_or_default(), fact.name))
                    .take(6)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let cooked = session
            .index()
            .cooked_declarations(path)
            .map(|facts| {
                facts
                    .iter()
                    .filter(|fact| fact.name.contains("format"))
                    .map(|fact| format!("{}::{}", fact.scope.clone().unwrap_or_default(), fact.name))
                    .take(6)
                    .collect::<Vec<_>>()
            });
        println!(
            "   {} | raw {} declarations, `format`-ish {:?} | cooked {:?}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            session.index().summary(path).map(|summary| summary.declarations.len()).unwrap_or(0),
            raw,
            cooked.as_ref().map(|facts| facts.len())
        );
        // **How many there are, not just the first six.** `take(6)` above is for the listing; a *count* is what
        // says whether a name is missing or merely late in the list, and reading the truncated list as the answer
        // is a mistake this probe made for several rounds (`std::format` was reported absent when the question
        // being answered was "is it in the first six").
        for (label, facts) in [
            ("raw", session.index().summary(path).map(|summary| summary.declarations.as_slice())),
            ("cooked", session.index().cooked_declarations(path)),
        ] {
            let Some(facts) = facts else { continue };
            let all: Vec<String> = facts
                .iter()
                .filter(|fact| fact.name.contains("format"))
                .map(cpp_code_analysis::DeclFact::qualified_name)
                .collect();
            println!(
                "      {label}: {} `format`-ish in all — `std::format` present: {} — {all:?}",
                all.len(),
                facts.iter().any(|fact| fact.qualified_name() == "std::format"),
            );
        }
    }
    // **The nested-name pair, and the difference between them is the whole question.** `size_type` is a member
    // of `basic_string`, so its qualified name is `std::basic_string::size_type`; `std::string` is an *alias*
    // for `basic_string<char, …>`, so `std::string::size_type` is a second question — resolve the alias, then
    // the member. Asking both says which of the two is missing, and one answer without the other is not enough
    // to tell them apart.
    for name in [
        "std::format",
        "std::vformat",
        "std::string",
        "std::cout",
        "std::vector",
        "std::basic_string::size_type",
        "std::string::size_type",
        "std::_Alloc_ptr_t",
        "std::_Allocation_guard::_Alloc_ptr_t",
        // **The three shapes a member can have, side by side.** `size_type` is declared in the class it is
        // asked of; `_Alty_traits` is a member alias of that class; `pointer` is **inherited** —
        // `struct allocator_traits : _Normal_allocator_traits<_Alloc>` — and asking for it is what the base
        // walk exists to answer. One of the three without the others cannot say which step is missing.
        "std::allocator_traits::pointer",
        "std::allocator_traits::difference_type",
        "std::basic_string::_Alty_traits",
        // …and the base the candidate walk should reach, named the way its own declaration is.
        "std::_Normal_allocator_traits",
        "std::_Normal_allocator_traits::pointer",
        "std::_Default_allocator_traits::pointer",
        // …and two of the names the count above says it cannot place, asked the way their own declaration
        // spells them: `_Choice_t` is a class at `std` scope (`concepts:264`), and `_Alvbase_traits` is a member
        // alias of `vector` declared twice (`vector:2828`, `2920`). If the plain spelling resolves and the one
        // a declaration wrote does not, what is missing is the step from one to the other.
        "std::_Choice_t",
        "std::ranges::_Begin::_Cpo::_Choice_t",
        "std::vector::_Alvbase_traits",
        "std::vector::_Alvbase_traits::size_type",
        // …and two the count still names, to say whether the alias fixes reached them: `_Mybase` is an alias of
        // a class the count lists as unresolved, and `_Mycont` is a member of what it points at.
        "std::_Vb_const_iterator::_Mybase",
        "std::_Vb_const_iterator::_Mybase::_Mycont",
        // …asked the way the count above asks: the spelling the type wrote, and the scope the declaration was
        // in. The two spellings asked in full above answer `Yes`; if these do not, the count is right and the
        // difference is the scope, not the alias.
        "std::_Vb_iterator::_Mybase",
        "std::_Vb_reference::_Mybase",
        "std::_Vb_val::_Alvbase_traits",
    ] {
        println!(
            "   definition({name:?}) = {:?}",
            session.definition_of_a_written_type(&view, name, None, 0)
        );
    }

    // ---------------------------------------------------------------------------------------------
    // **How many type names the reading cannot place** — the judgement on the whole type-resolution
    // effort, kept here rather than re-derived each time it is wanted.
    //
    // Every declaration of every cooked reading that spells a type, resolved by the name it wrote **in the
    // scope it wrote it in** (`definition_where_written`). The categories are separated because they call for
    // four different pieces of work, and a single total says none of them:
    //
    // * **an enclosing parameter** — `_Ty::value_type` inside `template <class _Ty>`, which no lookup can
    //   answer and none should: the answer is a property of the instantiation;
    // * **resolved** — the name is there, and this is the number that has to keep growing;
    // * **not name-shaped** — the type reader's fallback carried a spelling no identifier could be, such as
    //   `_CharT (*)(_CharT*, int&)`; `parse_type_spelling` says so in its own note, and a caller is expected
    //   to know what it is doing with the text it gets;
    // * **`auto`** — a placeholder, not a name, and the one category that stands for a missing **ability**
    //   (deduction) rather than a missing fact.
    //
    // Measured, in the order the work landed:
    //
    // ```text
    // 1868 unresolved   the first reading, which was the measurement being wrong: `Ambiguous` counted as a miss
    // 1386              with `Ambiguous` counted as the answer it is
    // 1235              with types that depend on a template parameter excluded
    //  171              with the name resolved where it was written, not where the reader stands
    //   75              with namespace-scope alias templates declaring facts
    // ```
    let (mut considered, mut parameters, mut resolved, mut fallback) = (0usize, 0usize, 0usize, 0usize);
    let mut placeholders = 0usize;
    let mut real: Vec<String> = Vec::new();
    let mut detail: Vec<String> = Vec::new();
    let mut out_of_sight: Vec<String> = Vec::new();
    for path in session.index().summaries().map(|summary| summary.path.clone()).collect::<Vec<_>>() {
        let Some(facts) = session.index().cooked_declarations(&path) else {
            continue;
        };
        let Some(view) = session.view(&path) else { continue };
        for fact in facts {
            let Some(spelled) = &fact.type_of else { continue };
            let ty = cpp_code_analysis::sema::types::parse_type_spelling(spelled);
            if ty.depends_on_a_parameter() {
                continue;
            }
            let Some(name) = ty.class_name() else { continue };
            considered += 1;

            let in_scope: Vec<String> = fact
                .scope
                .as_deref()
                .map(|scope| session.index().template_parameters_of(scope, &path))
                .unwrap_or_default();
            if name
                .split("::")
                .next()
                .is_some_and(|first| in_scope.iter().any(|parameter| parameter == first))
            {
                parameters += 1;
                continue;
            }

            // **Where the declaration is, as the fact itself says.** `scope` is the precise answer and is `None`
            // for a local, because a body contributes no segment to a qualified name; `in_namespace` is what a
            // local has instead — read with the closure's macro bodies in hand, so `_STD_BEGIN` opens `std` even
            // though the file spells no namespace at all. Asking the view's own scope tree instead answers for a
            // file that spells its namespaces out and not for one that opens them with a macro, which is what
            // every standard-library local is.
            let where_it_is = fact.scope.clone().or_else(|| fact.in_namespace.clone());

            if matches!(
                session.definition_of_a_written_type(
                    &view,
                    name,
                    where_it_is.as_deref(),
                    fact.range.start_offset
                ),
                cpp_code_analysis::Known::Yes(_)
                    | cpp_code_analysis::Known::Unknown(cpp_code_analysis::UnknownReason::Ambiguous(_))
            ) {
                resolved += 1;
                continue;
            }

            let name_shaped = name.split("::").all(|segment| {
                !segment.is_empty()
                    && !segment.starts_with(|c: char| c.is_ascii_digit())
                    && segment.chars().all(|c| c.is_alphanumeric() || c == '_')
            });
            if !name_shaped {
                fallback += 1;
            } else if name == "auto" {
                placeholders += 1;
            } else if declared_out_of_sight(&session, &view, name, where_it_is.as_deref()) {
                // **Declared, and the file is not allowed to see it.** This is the category that decides whether
                // the number below means anything: a name in a header the file does not include is a name a
                // lookup *must* refuse, so counting it as work left to do aims the effort at a number that
                // partly should not fall. Measured and it is not a small part of it.
                out_of_sight.push(name.to_string());
            } else {
                // **The name alone decides the count; the detail is for reading.** They are two lists rather
                // than one because `dedup` is what makes the number mean "names" instead of "occurrences" —
                // and appending `[scope=…]` to the string silently changed the metric from 71 to **125**, since
                // two occurrences of one name under two scopes stopped being equal. The metric stays the same
                // as every reading above it; only what is *printed* gained the detail.
                real.push(name.to_string());
                // **Is its first segment a template parameter?** `_Alloc::value_type` is not a name anybody
                // declares — `value_type` is a member of whatever `_Alloc` is instantiated with, and the answer
                // belongs to the instantiation rather than to this reading. A template parameter is *declared*,
                // in the `parameters` of the template that introduced it, so the question is whether any fact in
                // the session lists that name as one. Asked here rather than left to the reader of a list, and
                // printed beside the name so that the classification can be checked against the spelling.
                let first = name.split("::").next().unwrap_or(name);
                let dependent = name.contains("::")
                    && session.index().summaries().any(|summary| {
                        // **Both readings**, and the second one is the one that matters: a template's parameters
                        // are on the fact of the template, and a standard-library template is written with
                        // macros its own file's raw reading does not expand — so `_Alloc` is a parameter in the
                        // cooked facts and may be nothing at all in the raw ones. Scanning only the raw list
                        // answered "not a parameter" for eight names that are.
                        let cooked = session
                            .index()
                            .cooked_declarations(&summary.path)
                            .unwrap_or_default();
                        summary
                            .declarations
                            .iter()
                            .chain(cooked.iter())
                            .any(|fact| fact.parameters.iter().any(|held| held == first))
                    });

                // **Why `out_of_sight` said no**, asked separately for the three ways it can: nothing holds the
                // name under any spelling a lookup would try; something does and the file **can** see it, which
                // means the lookup itself is what failed; or the check above has a hole. The distinction decides
                // whether what is left is a gap in the reading or a gap in the question being asked.
                let mut spellings: Vec<String> = Vec::new();
                if let Some(scope) = where_it_is.as_deref() {
                    let segments: Vec<&str> = scope.split("::").collect();
                    spellings.extend(
                        (1..=segments.len())
                            .rev()
                            .map(|take| format!("{}::{name}", segments[..take].join("::"))),
                    );
                }
                spellings.push(name.to_string());

                let mut holders: Vec<std::path::PathBuf> = Vec::new();
                for summary in session.index().summaries() {
                    let cooked = session
                        .index()
                        .cooked_declarations(&summary.path)
                        .unwrap_or_default();
                    let held = summary
                        .declarations
                        .iter()
                        .chain(cooked.iter())
                        .any(|fact| spellings.iter().any(|wanted| *wanted == fact.qualified_name()));
                    if held {
                        holders.push(summary.path.clone());
                    }
                }

                // **What the index's own visibility says**, asked through its own public entry point rather
                // than through a second implementation of the same idea. `holders>0` says the name is held
                // somewhere; this says whether the index agrees the file can see it. The two disagreeing is a
                // bug **inside** the index, and that disagreement is what `_Choice_t` showed: held by
                // `concepts`, and the lookup still refuses it.
                let visible_here = session
                    .index()
                    .visible_declarations_where(
                        &view.path,
                        |fact: &cpp_code_analysis::DeclFact| {
                            spellings
                                .iter()
                                .any(|wanted| *wanted == fact.qualified_name())
                        },
                    )
                    .len();

                detail.push(format!(
                    "{name}{}  [scope={:?} in_namespace={:?} local={} holders={} visible={}]",
                    if dependent { "  ← 模板形参的成员" } else { "" },
                    fact.scope,
                    fact.in_namespace,
                    fact.local,
                    holders.len(),
                    visible_here
                ));
            }
        }
    }
    real.sort();
    real.dedup();
    println!(
        "\n--- type names the reading cannot place ---\n{considered} considered | {parameters} an enclosing \
         parameter | {resolved} resolved where written | {fallback} not name-shaped | {placeholders} `auto` | \
         {} declared out of sight | {} unplaced",
        // **Both counts are of names, not of occurrences**, and getting that wrong is how this category first
        // read `260` against an `unplaced` of `64`: one name written in twenty headers counted twenty times
        // while the other count deduped. A reading is only comparable to the ones above it if it is the same
        // measurement, so the category is deduped here rather than counted where it is found.
        {
            out_of_sight.sort();
            out_of_sight.dedup();
            out_of_sight.len()
        },
        real.len()
    );
    detail.sort();
    detail.dedup();
    for line in detail.iter() {
        println!("   {line}");
    }

    // TEMPORARY — whether the two halves compose: `members_of` resolves a **written type name** and walks the
    // base chain, so asking it for the alias that `_Alty_traits::pointer` starts from answers whether the
    // remaining gap is a missing ability or a missing wire between two abilities that both exist.
    //
    // Measured, and the answer is the wire: this was `NotDeclaredHere("std::basic_string::allocator_traits")`
    // before the four fixes it found — `is_declared` reading `Ambiguous` as "not declared", `direct_members`
    // reading an ambiguous *class* as a failure, `bases_of` asking the singular `definition` for a class
    // declared twice, and `lookup_names` taking a computed base (`conditional_t<…>`) at its word — and now
    // finds all 44 members through an alias, an enclosing scope and a base the class only names inside a
    // template argument. `definition` does not use this chain yet, which is why the count below has not moved.
    for written in [
        "std::allocator_traits",
        "std::basic_string::_Alty_traits",
        "std::string::_Alty_traits",
        // …and the alias the count names: `_Alvbase_traits` is declared **twice** in `vector` (`2828` as
        // `allocator_traits<_Alvbase>`, `2920` as `_Mybase::_Alvbase_traits`), and `size_type` is inherited from
        // whichever it resolves to. Asking both spellings says whether the alias step works and the member step
        // after it does not, or whether the alias itself is where it stops.
        "std::vector::_Alvbase_traits",
        "std::vector::_Alvbase_traits::size_type",
        // …and the one the count still names, where the two halves disagree: `std::_Vb_iterator::_Mybase`
        // resolves on its own (as `Ambiguous`) while `_Mybase::_Mycont` written in that same class does not, so
        // the step between them is what has to be looked at rather than the alias.
        "std::_Vb_iterator::_Mybase",
        // …and the classes either side of it, to say which one the alias should have landed on: `_Mybase` in
        // `_Vb_iterator` is that class's own base, and `_Mycont` is declared in it.
        "std::_Vb_iterator",
        "std::_Vb_const_iterator",
    ] {
        match session.members_of(&view, written) {
            cpp_code_analysis::Known::Yes(list) => {
                let names: Vec<&str> = list.members.iter().map(|member| member.fact.name.as_str()).collect();
                println!(
                    "   members_of({written:?}) = {} member(s) | pointer? {} difference_type? {}",
                    names.len(),
                    names.contains(&"pointer"),
                    names.contains(&"difference_type")
                );
            }
            other => println!("   members_of({written:?}) = {other:?}"),
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Completion after `std::`, and after `std::string::`.
    // ---------------------------------------------------------------------------------------------
    println!("\n--- completion ---");
    let after = |needle: &str, skip: usize| -> Option<usize> {
        text.find(needle).map(|at| at + skip)
    };

    for (what, needle, skip) in [
        ("std::", "std::format", "std::".len()),
        ("std::string::", "std::string::iterator", "std::string::".len()),
    ] {
        let Some(offset) = after(needle, skip) else {
            println!("{what:<14} (the probe's own line is not in the file)");
            continue;
        };
        let found = session.completions(&view, offset);
        let labels: Vec<&str> = found.items.iter().map(|item| item.label.as_str()).collect();
        println!(
            "{what:<14} scope {:?} | {} item(s), truncated {}",
            found.scope,
            found.items.len(),
            found.truncated
        );
        let wanted: Vec<&str> = match what {
            "std::" => vec!["format", "vformat", "string", "cout", "getline", "basic_string", "size_t"],
            _ => vec!["size", "length", "data", "begin", "push_back", "iterator"],
        };
        for name in wanted {
            match found.items.iter().find(|item| item.label == name) {
                Some(item) => println!("      {name:<14} offered | detail {:?}", item.detail),
                None => println!(
                    "      {name:<14} **NOT OFFERED** (and the index {} it)",
                    match session.index().definition(&format!("std::{name}"), &file) {
                        Known::Yes(_) => "has",
                        _ => "has not",
                    }
                ),
            }
        }
        let sample: Vec<&str> = labels.iter().take(8).copied().collect();
        println!("      first: {sample:?}");
    }

    // ---------------------------------------------------------------------------------------------
    // **The keystroke that was reported as "`std` is gone"**: a prefix typed inside a scope, which is what a completion
    // is actually asked about — a reader does not type `std::` and stop, they type `std::str` and look.
    //
    // The list is capped (`ITEM_BUDGET`), and the cap is applied *after* the names are ranked — so the question this
    // section answers is whether the ranking can push a match out of the answer. It is asked on a buffer of its own,
    // because the text has to contain the half-typed name for the query to see it.
    // ---------------------------------------------------------------------------------------------
    println!("\n--- completion with a prefix typed ---");
    let typed = format!("#include <format>\n{on_disk}\nvoid probe_typed() {{\n    std::str\n}}\n");
    let mut prefixed = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    prefixed.did_open(&file, &typed);
    prefixed.index_everything();

    if let Some(view) = prefixed.view(&file)
        && let Some(offset) = typed.find("std::str").map(|at| at + "std::str".len())
    {
        let found = prefixed.completions(&view, offset);
        let labels: Vec<&str> = found.items.iter().map(|item| item.label.as_str()).collect();
        println!(
            "std::str      scope {:?} prefix {:?} | {} item(s), truncated {}",
            found.scope,
            found.prefix,
            found.items.len(),
            found.truncated
        );
        for name in ["string", "string_view", "stoul", "strlen"] {
            println!(
                "      {name:<14} {}",
                if labels.contains(&name) {
                    "offered"
                } else {
                    "**NOT OFFERED**"
                }
            );
        }
        let sample: Vec<&str> = labels.iter().take(10).copied().collect();
        println!("      first: {sample:?}");
    }

    // ---------------------------------------------------------------------------------------------
    // Hover: the type at the cursor. `auto y = x.back()` is the case the user named, and `back` itself is the case
    // where the answer is a *spelling* — what the library wrote, macros and all.
    // ---------------------------------------------------------------------------------------------
    println!("\n--- hover (type_at) ---");
    for (what, needle, skip) in [
        ("the `y` of `auto y`", "auto y", "auto ".len()),
        ("the `x` of `x.back()`", "auto y = x.", "auto y = ".len()),
        ("`back` in the call", "auto y = x.back", "auto y = x.".len()),
        ("`size_type` in `std::string::size_type`", "std::string::size_type", "std::string::".len()),
    ] {
        let Some(offset) = after(needle, skip) else { continue };
        let answer = session.type_at(&view, offset);
        println!("{what:<40} {answer:?}");
    }

    // ---------------------------------------------------------------------------------------------
    // **The same two questions after the declaring header has been read the way a compiler reads it.**
    //
    // `auto y = x.back()` answers `_NODISCARD _CONSTEXPR20 reference` today, and that spelling can be wrong in two
    // different ways with two different fixes: the library's own macros standing where specifiers go (a *reading*
    // problem — the cooked rendering has them expanded), or a member alias (`reference` is `typedef _Ty& reference`)
    // that the substitution step did not follow (a *type* problem). Cooking the header separates them: if the answer
    // becomes `char&`, the fix is to have the cooked reading; if it stays `reference`, the alias step is what needs
    // the work.
    // ---------------------------------------------------------------------------------------------
    if let Some(back) = after("auto y = x.back", "auto y = x.".len())
        && let Known::Yes(declaring) = session.type_at(&view, back)
    {
        println!("\n--- after cooking {} ---", declaring.file.display());
        session.want_cooked_reading(&declaring.file);
        let mut rounds = 0usize;
        while (session.pending() > 0 || session.pending_cooking() > 0) && rounds < 10_000 {
            session.advance(64);
            rounds += 1;
        }

        if let Some(y) = after("auto y", "auto ".len()) {
            println!("the `y` of `auto y`   {:?}", session.type_at(&view, y));
        }
        println!("`back` in the call    {:?}", session.type_at(&view, back));

        // **What the two readings of that header say about `back`**, because "the cook did not change the answer"
        // has two causes: the cooked reading does not exist (the cook did not run), or it exists and spells the
        // return type the same way. Only the second one means the macros are not the problem.
        let raw = session
            .index()
            .summary(&declaring.file)
            .map(|summary| summary.declarations.iter().filter(|fact| fact.name == "back").count());
        let cooked = session
            .index()
            .cooked_declarations(&declaring.file)
            .map(|facts| facts.iter().filter(|fact| fact.name == "back").count());
        println!("declarations named `back`: raw {raw:?}, cooked {cooked:?}");
        if let Some(facts) = session.index().cooked_declarations(&declaring.file)
            && let Some(fact) = facts.iter().find(|fact| fact.name == "back")
        {
            println!("the cooked fact: returns {:?} type_of {:?}", fact.returns, fact.type_of);
        }
        if let Some(summary) = session.index().summary(&declaring.file)
            && let Some(fact) = summary.declarations.iter().find(|fact| fact.name == "back")
        {
            println!("the raw fact:    returns {:?} type_of {:?}", fact.returns, fact.type_of);
        }
    }

    // ---------------------------------------------------------------------------------------------
    // **What a hover on an overloaded name has to work with**: the name question answers `Ambiguous`, and the
    // plural question is what the popup is built from — one line per declaration, from the facts alone.
    // ---------------------------------------------------------------------------------------------
    println!("\n--- hover on an overloaded name ---");
    if let Some(offset) = after("std::format(\"{}\"", "std::".len()) {
        println!(
            "definition(std::format) = {:?}",
            session.definition(&view, offset).value().map(|found| found.fact.qualified_name())
        );
        match session.definitions(&view, offset) {
            Known::Yes(found) => {
                println!("definitions: {} declaration(s)", found.found.len());
                for declaration in &found.found {
                    println!(
                        "   {} {}{}",
                        declaration.fact.returns.clone().unwrap_or_default(),
                        declaration.fact.name,
                        declaration.fact.parameter_list.clone().unwrap_or_else(|| "(…)".to_string()),
                    );
                }
            }
            other => println!("definitions: {other:?}"),
        }
    }

    // ---------------------------------------------------------------------------------------------
    // The signature: what a reader gets while typing the arguments of a call to a header's function. **All** of
    // them, because an overload set is a list in the protocol and one signature per declaration here — a popup with
    // one of four overloads, or with none, is what a reader complained about.
    // ---------------------------------------------------------------------------------------------
    println!("\n--- signature ---");
    if let Some(offset) = after("std::format(\"{}\"", "std::format(".len() + 1) {
        let signatures = session.signatures_at(&view, offset);
        if signatures.is_empty() {
            println!("**no signature** for a call inside `std::format(...)`");
        }
        for signature in &signatures {
            println!("label: {}", signature.label);
            println!(
                "  parameters: {:?} | active {:?} | declared in {}",
                signature.parameters.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>(),
                signature.active_parameter,
                signature.declared_in.display()
            );
        }
    }

    // …and the same question about a **member** call: `s.append(` is where an overload set is the difference between
    // a popup that lists what the reader can write and one that shows whichever overload happened to be picked.
    if let Some(offset) = after("s.append(\"x\"", "s.append(".len() + 1) {
        let signatures = session.signatures_at(&view, offset);
        println!("s.append( → {} signature(s)", signatures.len());
        for signature in signatures.iter().take(12) {
            println!("   {}", signature.label);
        }
    }
}
