//! **How much of a real project's member reasoning actually resolves** — the scoreboard for template reasoning.
//!
//! ```text
//! $env:CPPLS_REAL_PROJECT = "E:\EmmyLuaCodeStyle"
//! cargo test --release -p cpp_code_analysis --test resolution_rate -- --ignored --nocapture
//! ```
//!
//! # What it counts, and why this question
//!
//! Every `x.member` and `x->member` in the project's own files is a question the editor must answer, and the answer
//! has three states: a declaration (`Yes`), a refusal with a reason (`Unknown`), or nothing at all (`No`). The first
//! number is the product; the *reasons* in the second are the work list, and one of them — `UnknownType` — is the
//! template-reasoning wall: the receiver's type could not be built, so the member could not be looked up.
//!
//! The scoreboard exists because "the types got better" is not measurable and "these three names resolve now" is too
//! small to steer by. A rate over a whole project is, and it is the number every change below is judged by:
//! `docs/remaining.md` §A4's default-argument work, the alias chain, and whatever comes after them.
//!
//! It reads the **rendering** of each file (`Session::view`), because that is what a hover reads: a member question
//! about a file whose macros are unexpanded is a question about a program nobody compiles.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use cpp_code_analysis::file::coordinates::ReadingOffset;
use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> Option<PathBuf> {
    let named = std::env::var_os("CPPLS_REAL_PROJECT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"E:\EmmyLuaCodeStyle"));
    named.is_dir().then_some(named)
}

/// One `x.member` the project writes, and what came of it.
#[derive(Default)]
struct Scoreboard {
    /// Member questions asked, one per access written (nesting counts: `a.b.c` asks twice).
    asked: usize,
    /// `as`-through versus `->`-through, because they exercise different chains.
    through_a_dot: usize,
    through_an_arrow: usize,
    /// `Known::Yes` — a declaration was found.
    resolved: usize,
    /// `Known::Unknown` — a refusal, by the first word of its reason (`UnknownType`, `NotDeclaredHere`, …).
    refused: BTreeMap<String, usize>,
    /// `Known::No` — the name query declined to take the question.
    declined: usize,
    /// Of the refusals, how many had a **receiver type** at all. A refusal with a type is "the type is known and the
    /// member is not in it"; a refusal without one is the chain giving up one step earlier, and it is the number the
    /// template work moves.
    refused_with_a_type: usize,
    /// The receiver types seen among the refusals, most frequent first — the work list in a form a reader can act on.
    types_that_failed: BTreeMap<String, usize>,
    /// **What the receiver *is***, and how many of each resolved — the bucket that says where the work is. A call
    /// receiver needs a call's return type, an arrow receiver needs `operator->`'s chain, a plain name needs the
    /// declaration's own type. One rate for all of them hides which of those is failing.
    by_shape: BTreeMap<&'static str, (usize, usize)>,
    /// A handful of refusals written out, so that the next change starts from a name rather than a number.
    samples: Vec<String>,
    /// **Of the refusals that had a receiver type, what was there to choose from** — the split the last measurement
    /// was missing. `NotDeclaredHere` covers "the class is not visible at all" and "the class is visible several times
    /// and the chosen declaration has no such member", and they are different work: the first is the include graph,
    /// the second is the rule that picks among candidates.
    refused_choices: BTreeMap<&'static str, usize>,
}

/// **What kind of thing the receiver is** — the classification the scoreboard steers by.
fn shape_of(node: &cpp_parser::CppSyntaxNode) -> &'static str {
    use cpp_parser::CppSyntaxKind;
    let kind = CppSyntaxKind::from(node.kind());
    match kind {
        CppSyntaxKind::NameExpr => "a name",
        CppSyntaxKind::CallExpr => "a call",
        CppSyntaxKind::IndexExpr => {
            let has_brackets = node
                .children_with_tokens()
                .any(|element| element.as_token().is_some_and(|token| token.text() == "["));
            if has_brackets { "a subscript" } else { "a member" }
        }
        CppSyntaxKind::UnaryExpr => {
            let text = node.text().to_string();
            if text.trim_start().starts_with('*') {
                "a dereference"
            } else {
                "another shape"
            }
        }
        CppSyntaxKind::ThisExpr => "this",
        CppSyntaxKind::ParenExpr => "a parenthesis",
        CppSyntaxKind::CastExpr => "a cast",
        CppSyntaxKind::NewExpr => "a new",
        _ => "another shape",
    }
}

impl Scoreboard {
    fn add(&mut self, other: Scoreboard) {
        self.asked += other.asked;
        self.through_a_dot += other.through_a_dot;
        self.through_an_arrow += other.through_an_arrow;
        self.resolved += other.resolved;
        self.declined += other.declined;
        self.refused_with_a_type += other.refused_with_a_type;
        for (reason, count) in other.refused {
            *self.refused.entry(reason).or_default() += count;
        }
        for (type_of, count) in other.types_that_failed {
            *self.types_that_failed.entry(type_of).or_default() += count;
        }
        for (shape, (asked, resolved)) in other.by_shape {
            let entry = self.by_shape.entry(shape).or_default();
            entry.0 += asked;
            entry.1 += resolved;
        }
        self.samples.extend(other.samples);
        for (choice, count) in other.refused_choices {
            *self.refused_choices.entry(choice).or_default() += count;
        }
    }

    fn report(&mut self, files: usize, uncooked: usize) {
        let total = self.asked.max(1);
        println!("\n--- the project's member questions ---");
        println!(
            "{files} file(s) ({uncooked} with no rendering), {} member question(s): {} through `.`, {} through `->`",
            self.asked, self.through_a_dot, self.through_an_arrow
        );
        println!(
            "resolved {:>6} ({:>5.1}%)   refused {:>6}   declined {:>6}",
            self.resolved,
            100.0 * self.resolved as f64 / total as f64,
            self.refused.values().sum::<usize>(),
            self.declined
        );
        println!(
            "of the refusals, {} had a receiver type and {} did not",
            self.refused_with_a_type,
            self.refused.values().sum::<usize>() - self.refused_with_a_type
        );

        println!("\nby receiver shape:");
        let mut shapes: Vec<(&str, (usize, usize))> =
            self.by_shape.iter().map(|(shape, counts)| (*shape, *counts)).collect();
        shapes.sort_by_key(|(_, (asked, _))| std::cmp::Reverse(*asked));
        for (shape, (asked, resolved)) in shapes {
            let rate = 100.0 * resolved as f64 / asked.max(1) as f64;
            println!("  {asked:>6} asked  {resolved:>6} resolved  {rate:>5.1}%  {shape}");
        }

        println!("\nby reason:");
        let mut reasons: Vec<(&String, &usize)> = self.refused.iter().collect();
        reasons.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        for (reason, count) in reasons {
            println!("  {count:>6}  {reason}");
        }

        println!("\nthe receiver types that failed most often:");
        let mut types: Vec<(&String, &usize)> = self.types_that_failed.iter().collect();
        types.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        for (type_of, count) in types.iter().take(25) {
            println!("  {count:>6}  {type_of}");
        }

        println!("\nof the refusals that had a receiver type, what there was to choose from:");
        let mut choices: Vec<(&str, usize)> = self
            .refused_choices
            .iter()
            .map(|(choice, count)| (*choice, *count))
            .collect();
        choices.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        for (choice, count) in choices {
            println!("  {count:>6}  {choice}");
        }

        println!("\nthe first refusals, so the next change starts from a name:");
        self.samples.sort();
        self.samples.dedup();
        for sample in self.samples.iter().take(30) {
            println!("  {sample}");
        }
    }
}

#[test]
#[ignore = "opens a real project; run with --ignored"]
fn the_member_questions_of_a_real_project_and_how_many_resolve() {
    let Some(root) = project() else {
        eprintln!("skipped: no project (set CPPLS_REAL_PROJECT)");
        return;
    };

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    let files: Vec<PathBuf> = session.project_files().to_vec();
    session.add_project_files(files.clone());
    session.index_everything();
    while session.pending_work() > 0 {
        session.advance(64);
    }
    println!("the project has {} file(s)", files.len());

    let mut total = Scoreboard::default();
    let mut counted_files = 0usize;
    let mut uncooked = 0usize;

    for path in &files {
        // **The file's own text, not its rendering** — deliberately, because the number has to be comparable across
        // changes. Cooking every project file means rendering its whole standard-library closure (measured: minutes
        // on this project), and a scoreboard that takes an hour is not run after every change. The cost is that a
        // macro-written member access cannot be typed here, so the rate is *pessimistic* by a fixed amount — the same
        // pessimism before and after, which is what a rate is for. The rendering's own rate is measured where it
        // matters, on the two files an editor opens (`cpp_ls`'s `real_project.rs`).
        let Some(view) = session.view_of_the_file(path) else {
            uncooked += 1;
            continue;
        };
        // **Which file the walk was in when it died.** A probe that dies without a name has to be bisected by hand;
        // one line per file costs nothing and names the case (`E:\clice` overflows the stack without it).
        if std::env::var_os("CPPLS_TRACE_PROGRESS").is_some() {
            eprintln!("[file] {}", path.display());
        }
        counted_files += 1;

        let mut scoreboard = Scoreboard::default();
        // Each access is asked **once**: `a.b.c` nests two nodes, and the inner one is the same question asked again.
        let mut asked = BTreeSet::new();
        for node in view.root.descendants() {
            let Some(access) = cpp_code_analysis::sema::resolve::member_access_of(&node) else {
                continue;
            };
            if access.member.is_empty() {
                continue;
            }
            if !asked.insert(access.member_range.start_offset) {
                continue;
            }

            scoreboard.asked += 1;
            if access.arrow {
                scoreboard.through_an_arrow += 1;
            } else {
                scoreboard.through_a_dot += 1;
            }
            let shape = shape_of(&access.object);
            let entry = scoreboard.by_shape.entry(shape).or_default();
            entry.0 += 1;

            // **Inside the member's own name**, not at its first byte: the completion context the member query reads
            // is text-based ("is the caret right after a `.`?"), and at the first byte it reports a member access
            // with *nothing written* — a different question, which no name can answer.
            let at = ReadingOffset::new(access.member_range.start_offset + 1);
            match session.definitions(&view, at) {
                Known::Yes(_) => {
                    scoreboard.resolved += 1;
                    entry.1 += 1;
                }
                Known::No => scoreboard.declined += 1,
                Known::Unknown(reason) => {
                    let spelled = format!("{reason:?}");
                    let kind = spelled
                        .split(['(', ' '])
                        .next()
                        .unwrap_or("Unknown")
                        .to_string();
                    *scoreboard.refused.entry(kind).or_default() += 1;

                    // **Where the chain stopped**: a receiver type is what the member lookup needs, and a refusal
                    // that has one failed *in* the type; one without failed *before* it.
                    let object = cpp_parser::source_range(access.object.text_range());
                    let receiver = match session.type_at(&view, ReadingOffset::new(object.start_offset)) {
                        Known::Yes(found) => {
                            scoreboard.refused_with_a_type += 1;
                            *scoreboard
                                .types_that_failed
                                .entry(found.type_of.clone())
                                .or_default() += 1;
                            found.type_of
                        }
                        _ => "?".to_string(),
                    };
                    if scoreboard.samples.len() < 400 {
                        let line = view
                            .source
                            .get(..object.start_offset)
                            .map(|before| before.matches('\n').count() + 1)
                            .unwrap_or(0);
                        let object_text = access.object.text().to_string();
                        let object_text = object_text.trim().replace('\n', " ");
                        // **Is the member in the class's own list?** The two failures are different work: a list that
                        // does not hold the member is the member walk's bug, and a list that holds it but a query that
                        // refuses is the query's.
                        let listed = if receiver == "?" {
                            "no receiver".to_string()
                        } else {
                            // The class the member query would use — a pointer's pointee, since `p->m` asks about the
                            // class the pointer names. `members_of` is given that spelling, not the pointer.
                            let class = receiver
                                .trim_end_matches(|character: char| matches!(character, '*' | '&'))
                                .trim()
                                .to_string();
                            // **How many declarations of that class this file can see**, which is the number the last
                            // measurement was missing: a refusal with *no* visible declaration is visibility, one
                            // with several is the choice between them, and `NotDeclaredHere` reports both the same
                            // way. `members_of` then says what the chosen one answered.
                            let declared = session
                                .index()
                                .declarations_in(&class, &view.path)
                                .len();
                            let choice = match session.members_of(&view, &class) {
                                Known::Yes(list) => {
                                    let has = list
                                        .members
                                        .iter()
                                        .any(|member| member.fact.name == access.member);
                                    if declared == 0 {
                                        "a list from no visible declaration"
                                    } else if list.members.is_empty() {
                                        "a visible class with an empty list"
                                    } else if has {
                                        "the member is in the list and the query still refused"
                                    } else {
                                        "a list that does not hold the member"
                                    }
                                }
                                Known::Unknown(reason) => {
                                    if declared == 0 {
                                        "no visible declaration of the class"
                                    } else {
                                        let _ = reason;
                                        "visible declarations and no list"
                                    }
                                }
                                Known::No => "no list",
                            };
                            *scoreboard.refused_choices.entry(choice).or_default() += 1;
                            match session.members_of(&view, &class) {
                                Known::Yes(list) => {
                                    let has = list
                                        .members
                                        .iter()
                                        .any(|member| member.fact.name == access.member);
                                    format!(
                                        "{declared} visible declaration(s), {} member(s), has it: {has}",
                                        list.members.len()
                                    )
                                }
                                Known::Unknown(reason) => {
                                    format!("{declared} visible declaration(s), no list: {reason:?}")
                                }
                                Known::No => format!("{declared} visible declaration(s), no list"),
                            }
                        };
                        let operator = if access.arrow { "->" } else { "." };
                        scoreboard.samples.push(format!(
                            "{}:{line}  {object_text}{operator}{}  (shape {shape}, kind {:?}, receiver {receiver}, {listed})",
                            path.file_name().unwrap_or_default().to_string_lossy(),
                            access.member,
                            cpp_parser::CppSyntaxKind::from(access.object.kind()),
                        ));
                    }
                }
            }
        }

        if scoreboard.asked > 0 {
            total.add(scoreboard);
        }
    }

    total.report(counted_files, uncooked);

    // **Is one file in the index twice?** The finding that the 77 refusals with a receiver type are *all* visibility
    // led to a hypothesis: the same header arrives under two spellings, so it has two summaries, two sequence numbers
    // and two sets of postings — which would explain why making the dropped spellings visible *cost* 28 answers
    // instead of gaining any. This counts it, because the fix depends entirely on the answer.
    {
        let mut by_identity: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (path, _) in session.index().all_summaries() {
            let spelling = path.to_string_lossy().to_lowercase().replace('\\', "/");
            by_identity
                .entry(spelling)
                .or_default()
                .push(path.to_string_lossy().to_string());
        }
        let duplicated: Vec<(&String, &Vec<String>)> = by_identity
            .iter()
            .filter(|(_, spellings)| spellings.len() > 1)
            .collect();
        println!(
            "\n{} summar(ies), {} distinct file(s) by identity, {} in the index twice",
            by_identity.values().map(Vec::len).sum::<usize>(),
            by_identity.len(),
            duplicated.len()
        );
        for (identity, spellings) in duplicated.iter().take(5) {
            println!("  {identity}: {spellings:?}");
        }
    }

    // **How many of the project's own `#include`s reached a file at all.** Every refusal this scoreboard reports has
    // the same upstream question: a name is only visible if the header that declares it was *entered*, and a header is
    // only entered if the directive resolved. Measured on `clice` (Bazel, no compile database): **12.2%** of 68 210
    // member questions resolved, and 11 866 of the refusals had "no visible declaration of the class" — so the number
    // below is the one that explains the project rather than the code. `cppls` cannot see a header the *build* found
    // through a path it does not know, and this is how much that costs.
    {
        let mut resolved = 0usize;
        let mut total = 0usize;
        for path in &files {
            let Some(summary) = session.index().summary(path) else {
                continue;
            };
            for include in &summary.includes {
                total += 1;
                if include.resolved.is_some() {
                    resolved += 1;
                }
            }
        }
        println!(
            "\n{resolved} of {total} include(s) in the project's own files reached a file ({:.1}%)",
            100.0 * resolved as f64 / total.max(1) as f64
        );
    }

    // **One class, on request** — `CPPLS_CLASS=LanguageServer` prints the member list the query builds for it, which
    // is the first thing to look at when the scoreboard names a class: an empty list and a missing class are two
    // different bugs, and the sample line cannot tell them apart from the outside.
    if let Some(asked) = std::env::var_os("CPPLS_CLASS") {
        let asked = asked.to_string_lossy().to_string();
        let somewhere = files
            .iter()
            .find(|path| session.view_of_the_file(path).is_some())
            .cloned();
        if let Some(somewhere) = somewhere {
            let view = session.view_of_the_file(&somewhere).expect("a view");
            // **The two answers that separate the two bugs**: facts *filed* under the scope, and facts the query can
            // *see* from this file. A postings count of zero is the member walk never having filed them; a positive
            // count with an empty list is the visibility filter dropping them.
            println!(
                "\n{asked}: {} fact(s) filed under that scope, {} visible from {}",
                session.index().scope_postings(&asked),
                session
                    .index()
                    .declarations_in(&asked, &somewhere)
                    .len(),
                somewhere.display()
            );
            for (file, fact) in session.index().every_fact_named("SendRequest").iter().take(6) {
                println!(
                    "  SendRequest in {}: scope {:?} qualified {:?}",
                    file.file_name().unwrap_or_default().to_string_lossy(),
                    fact.scope,
                    fact.qualified_name()
                );
            }
            // **And from where**: the same scope asked from a few files. A file that includes the header has to see
            // the facts, and a file that does not has to see none — the two answers are what makes the visibility
            // filter right, and a count of zero where it should be 32 is the bug.
            for path in files.iter().take(400) {
                let visible = session.index().declarations_in(&asked, path).len();
                if visible > 0 {
                    println!(
                        "  visible from {}: {visible} fact(s)",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    );
                }
            }

            // **One file, in detail** — `CPPLS_FROM=CommandService.cpp` prints what that file's own summary says its
            // `#include`s resolved to and how many files the walk thinks it sees. The end-to-end symptom ("this class
            // is invisible here") has exactly two causes: the directive did not resolve, or it did and the walk did
            // not cross the edge.
            if let Some(named) = std::env::var_os("CPPLS_FROM") {
                let named = named.to_string_lossy().to_string();
                if let Some(path) = files
                    .iter()
                    .find(|path| path.to_string_lossy().contains(&named))
                {
                    println!("\nfrom {}:", path.display());
                    let mut visible = session.index().visible_files(path);
                    println!("  {} file(s) visible", visible.len());
                    // **Is the header the class is in, in the walk at all?**
                    let header = visible
                        .iter()
                        .find(|(seen, _)| seen.ends_with("languageserver.h"))
                        .map(|(seen, _)| seen.clone());
                    println!("  the header in the walk: {header:?}");
                    // **And does the index have a summary under the path the walk handed back?** A walk that names a
                    // file the index cannot map to a summary is a walk whose edges are invisible to every query —
                    // the answer here separates "the edge was never crossed" from "the edge crossed under a name the
                    // index does not know".
                    for (seen, _) in visible.iter().take(5) {
                        println!(
                            "    {seen}: summary {}",
                            if session.index().summary(std::path::Path::new(seen)).is_some() {
                                "found"
                            } else {
                                "MISSING"
                            }
                        );
                    }
                    visible.truncate(12);
                    visible.truncate(12);
                    for (seen, _) in &visible {
                        println!("    {seen}");
                    }
                    if let Some(summary) = session.index().summary(path) {
                        println!("  {} include(s) recorded:", summary.includes.len());
                        for include in summary.includes.iter().take(20) {
                            println!(
                                "    {:?} {:?} -> {:?}",
                                include.form, include.spelling, include.resolved
                            );
                        }
                    }
                }
            }

            match session.members_of(&view, &asked) {
                Known::Yes(list) => {
                    println!("\n{asked}: {} member(s)", list.members.len());
                    for member in list.members.iter().take(40) {
                        println!(
                            "  {:?} {:?} scope {:?} type {:?}",
                            member.fact.kind, member.fact.name, member.fact.scope, member.fact.type_of
                        );
                    }
                }
                Known::Unknown(reason) => println!("\n{asked}: no list, {reason:?}"),
                Known::No => println!("\n{asked}: no list"),
            }
        }
    }
}
