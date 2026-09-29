//! **Where a member access stops working**: `w.size`, `v[0].size`, `p->size`, `it->first` — the everyday shapes a
//! reader expects a C++ server to answer, broken down by *which step* refused.
//!
//! A member access needs three things in a row, and a measurement that reports only "it failed" cannot say which:
//!
//! ```text
//! 1. the member's name is written        `w.` with nothing after it is a completion, not a jump
//! 2. the object has a type               the inference layer: a declaration's `type_of`, `auto`, `*p`, `f()`
//! 3. that type's members can be listed   the class is in the index, its bases resolve, the name is not ambiguous
//! ```
//!
//! So this walks a corpus, finds every member access in every file, and asks the shipping queries about it — the
//! same `Session::type_at` and `Session::members_of` the LSP handlers call, not a model of them. What it prints is
//! the count at each step and the **shape** of the ones that stopped (`auto`, a template parameter, a call, a
//! subscript), which is the number a plan can be built on.
//!
//! ```text
//! cargo run --release --example member_probe -- <list> [--limit <n>] [--examples <n>]
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use cpp_code_analysis::sema::resolve::{member_access_at, member_access_of};
use cpp_code_analysis::sema::types::{type_of_declaration, Type};
use cpp_code_analysis::Known;

/// The declared type of every variable-like declaration in a file, read **from the syntax** rather than from the
/// spelling the summary recorded.
///
/// This is the shape a query has: a declaration's specifier sequence, the declarator under it, and the name the
/// declarator declares. Doing it here rather than through `declared_type_of` is the point of the census — the two
/// are supposed to agree, and where they do not, the spelling-based one is the one that was wrong.
fn declarations_of(root: &cpp_parser::CppSyntaxNode) -> Vec<(cpp_parser::SourceRange, Type)> {
    let mut found = Vec::new();

    for declaration in root.descendants() {
        if cpp_parser::CppSyntaxKind::from(declaration.kind())
            != cpp_parser::CppSyntaxKind::Declaration
        {
            continue;
        }

        let Some(specifiers) = declaration
            .children()
            .find(|child| {
                cpp_parser::CppSyntaxKind::from(child.kind())
                    == cpp_parser::CppSyntaxKind::DeclSpecifierSeq
            })
        else {
            continue;
        };

        // Every declarator **directly** under the declaration or under its `InitDeclarator`, which is where a
        // variable's own declarator is — a parameter's is under a `Parameter`, and belongs to the function's type.
        let declarators: Vec<cpp_parser::CppSyntaxNode> = declaration
            .children()
            .filter(|child| {
                matches!(
                    cpp_parser::CppSyntaxKind::from(child.kind()),
                    cpp_parser::CppSyntaxKind::InitDeclarator | cpp_parser::CppSyntaxKind::Declarator
                )
            })
            .flat_map(|child| {
                let own = matches!(
                    cpp_parser::CppSyntaxKind::from(child.kind()),
                    cpp_parser::CppSyntaxKind::Declarator
                );
                let mut list: Vec<cpp_parser::CppSyntaxNode> = if own {
                    vec![child.clone()]
                } else {
                    child
                        .children()
                        .filter(|inner| {
                            cpp_parser::CppSyntaxKind::from(inner.kind())
                                == cpp_parser::CppSyntaxKind::Declarator
                        })
                        .collect()
                };
                list.shrink_to_fit();
                list
            })
            .collect();

        for declarator in declarators {
            let Some(name) = declarator.descendants().find(|child| {
                cpp_parser::CppSyntaxKind::from(child.kind()) == cpp_parser::CppSyntaxKind::NameExpr
            }) else {
                continue;
            };

            found.push((
                cpp_parser::source_range(name.text_range()),
                type_of_declaration(
                    &specifiers,
                    Some(&declarator),
                    cpp_parser::source_range(name.text_range()),
                ),
            ));
        }
    }

    found
}

/// Is this type written with the `auto` placeholder? A word, not a substring: `automatic` is a name.
fn writes_auto(written: &str) -> bool {
    written
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .any(|word| word == "auto")
}

/// The shapes an object expression can have, by node kind — the vocabulary the census is reported in.
fn shape_of(node: &cpp_parser::CppSyntaxNode) -> String {
    let text = node.text().to_string();
    let trimmed = text.trim();

    if cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::CallExpr {
        return "a call".to_string();
    }
    if member_access_of(node).is_some() {
        return "a member access".to_string();
    }
    // A single word is a name; anything else is a shape this layer would have to compute.
    if !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_' || character == ':')
    {
        return "a name".to_string();
    }

    format!("{:?}", cpp_parser::CppSyntaxKind::from(node.kind()))
}

fn main() {
    let list = std::env::args().nth(1).expect("a file list");
    let limit = std::env::args()
        .position(|argument| argument == "--limit")
        .and_then(|at| std::env::args().nth(at + 1))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    let shown = std::env::args()
        .position(|argument| argument == "--examples")
        .and_then(|at| std::env::args().nth(at + 1))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(12);

    let paths: Vec<PathBuf> = std::fs::read_to_string(&list)
        .expect("the list reads")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();

    let mut session = cpp_code_analysis::Session::with_config(
        std::env::temp_dir().join("member-probe-root"),
        cpp_code_analysis::SessionFiles::new(
            cpp_code_analysis::OpenDocuments::new(),
            cpp_code_analysis::DiskFiles,
        ),
        cpp_code_analysis::WatchFilter::new(std::env::temp_dir().join("member-probe-root")),
        cpp_code_analysis::CompilerConfig::default(),
    );
    std::fs::create_dir_all(std::env::temp_dir().join("member-probe-root")).ok();

    let indexed = Instant::now();
    session.add_project_files(paths.iter().cloned());
    session.index_everything();
    println!(
        "indexed {} files in {:?}",
        session.project_files().len(),
        indexed.elapsed()
    );

    let mut accesses = 0usize;
    let mut nameless = 0usize;
    let mut typed = 0usize;
    let mut listed = 0usize;
    let mut shapes: HashMap<String, usize> = HashMap::new();
    let mut refusals: HashMap<String, usize> = HashMap::new();
    let mut examples: Vec<String> = Vec::new();

    // **What the type reader produces**, counted separately from what the queries do with it: a spelling that holds
    // a stray `>` or a declaration specifier is a reading that went wrong, and a census that only counted answers
    // could not tell that from a type nothing declares.
    let mut declared = 0usize;
    let mut declared_with_auto = 0usize;
    let mut malformed: HashMap<String, usize> = HashMap::new();

    for path in paths.iter().take(limit) {
        let Some(view) = session.view(path) else {
            continue;
        };

        for (_, found) in declarations_of(&view.root) {
            let written = found.to_string();
            declared += 1;

            if writes_auto(&written) {
                declared_with_auto += 1;
            }

            // The three shapes of a reading that is not a type: an argument list that lost its name (`_Cont>`), a
            // specifier left in (`constexpr …`), or a stray angle bracket. Each was a real answer before the reader
            // was rewritten, and each is a class query that cannot succeed.
            if written.contains('>') && !written.contains('<') {
                *malformed.entry("a stray `>`".to_string()).or_default() += 1;
                if examples.len() < shown * 4 {
                    examples.push(format!("{}: `{written}`", path.display()));
                }
            } else if written.split_whitespace().any(|word| {
                matches!(
                    word,
                    "constexpr" | "static" | "inline" | "nodiscard" | "mutable"
                )
            }) {
                *malformed
                    .entry("a declaration specifier left in".to_string())
                    .or_default() += 1;
                if examples.len() < shown * 4 {
                    examples.push(format!("SPEC {}: `{written}`", path.display()));
                }
            }
        }

        // Every node that *is* a member access, found by the same reading the queries use — so this census counts
        // the shapes the product sees and not a second opinion about them.
        let found: Vec<cpp_parser::CppSyntaxNode> = view
            .root
            .descendants()
            .filter(|node| {
                matches!(
                    cpp_parser::CppSyntaxKind::from(node.kind()),
                    cpp_parser::CppSyntaxKind::MemberExpr
                        | cpp_parser::CppSyntaxKind::ArrowExpr
                        | cpp_parser::CppSyntaxKind::IndexExpr
                )
            })
            .filter(|node| member_access_of(node).is_some())
            .collect();

        for node in found {
            let Some(access) = member_access_at(&view.root, usize::from(node.text_range().start()))
                .or_else(|| member_access_of(&node))
            else {
                continue;
            };

            if access.member.is_empty() {
                nameless += 1;
                continue;
            }

            accesses += 1;
            let object = access.object.text().to_string().trim().to_string();
            let shape = shape_of(&access.object);

            // **The type of the *object*, not of the access.** Asking at the member's own offset would ask about
            // `w.size` — whose answer is the type of the member — and the question here is what `w` is. The last
            // byte inside the object's own range is the one offset no outer member access contains.
            let inside_the_object = usize::from(access.object.text_range().end()).saturating_sub(1);

            // **The shipping path**: the type query the hover and the member query both stand on.
            match session.type_at(&view, inside_the_object) {
                Known::Yes(found) => {
                    typed += 1;
                    let class = class_of(&found.type_of);
                    if class.is_empty() {
                        *refusals.entry("members: no class in the type".to_string()).or_default() += 1;
                        continue;
                    }

                    match session.members_of(&view, &class) {
                        Known::Yes(members) if !members.members.is_empty() => {
                            listed += 1;
                        }
                        Known::Yes(_) => {
                            *refusals.entry("listed nothing".to_string()).or_default() += 1;
                            if examples.len() < shown {
                                examples.push(format!(
                                    "{object}.{}: `{}` listed no member",
                                    access.member, found.type_of
                                ));
                            }
                        }
                        Known::Unknown(reason) => {
                            let why = format!("members: {:?}", reason)
                                .split('(')
                                .next()
                                .unwrap_or_default()
                                .to_string();
                            *refusals.entry(why).or_default() += 1;
                            if examples.len() < shown {
                                examples.push(format!(
                                    "{object}.{}: `{}` — {}",
                                    access.member,
                                    found.type_of,
                                    reason.describe()
                                ));
                            }
                        }
                        Known::No => {
                            *refusals.entry("members: no".to_string()).or_default() += 1;
                        }
                    }
                }
                Known::Unknown(reason) => {
                    *shapes.entry(shape.clone()).or_default() += 1;
                    let why = format!("type: {:?}", reason)
                        .split('(')
                        .next()
                        .unwrap_or_default()
                        .to_string();
                    *refusals.entry(why).or_default() += 1;
                    if examples.len() < shown {
                        examples.push(format!(
                            "{object}.{} ({shape}): {}",
                            access.member,
                            reason.describe()
                        ));
                    }
                }
                Known::No => {
                    *refusals.entry("type: no".to_string()).or_default() += 1;
                }
            }
        }
    }

    println!(
        "\n--- member accesses: {accesses} with a name | {nameless} still being typed ---\
         \n    {typed} had a type ({:.0}%) | listed: {listed} ({:.0}% of all, {:.0}% of typed) ---",
        ratio(typed, accesses),
        ratio(listed, accesses),
        ratio(listed, typed),
    );

    println!(
        "\n--- type readings: {declared} declarations | {declared_with_auto} written with `auto` | \
         {} malformed ---",
        malformed.values().sum::<usize>()
    );
    let mut ranked: Vec<(String, usize)> = malformed.into_iter().collect();
    ranked.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    for (why, count) in ranked {
        println!("{count:8}  {why}");
    }

    let mut ranked: Vec<(String, usize)> = refusals.into_iter().collect();
    ranked.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    println!("\n--- why it stopped ---");
    for (why, count) in ranked {
        println!("{count:8}  {why}");
    }

    let mut shapes: Vec<(String, usize)> = shapes.into_iter().collect();
    shapes.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    println!("\n--- the objects whose type was not known, by shape ---");
    for (shape, count) in shapes {
        println!("{count:8}  {shape}");
    }

    if !examples.is_empty() {
        println!("\n--- examples ---");
        for example in examples {
            println!("   {example}");
        }
    }
}

/// The class a written type names, without its template arguments or its operators — `std::vector<int>` is
/// `std::vector`, `const Widget&` is `Widget`.
///
/// What [`Session::members_of`] wants, and the reason this probe normalises rather than passing the spelling
/// through: the query is about a *class*, and `std::map<std::string, int>` is not a class name.
fn class_of(written: &str) -> String {
    let mut text = written.trim().trim_start_matches("const ").trim();
    text = text.trim_start_matches("struct ").trim_start_matches("class ").trim();

    let base = match text.find('<') {
        Some(at) => &text[..at],
        None => text,
    };

    base.trim().trim_end_matches('*').trim_end_matches('&').trim().to_string()
}

fn ratio(part: usize, whole: usize) -> f64 {
    100.0 * part as f64 / (whole.max(1) as f64)
}
