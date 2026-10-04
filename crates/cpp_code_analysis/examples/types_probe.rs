//! How much of a corpus's `auto` is now a type: for every declaration the file writes with the placeholder, does
//! the query layer answer something that is not `auto`?
//!
//! The other half of the round's evidence — the tests say the rule is right on the shapes it was written for, and
//! this says how often those shapes occur in real headers, and where the answer is still `auto` or nothing.
//!
//! ```text
//! cargo run --release --example types_probe -- <list> [--limit <n>]
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

/// **The kind of node an `auto` declaration's initializer is** — what the refusal was about.
///
/// Found by descending to the declaration holding `name_at`, then to its `Initializer`, then to that node's last
/// child that is a node rather than a token: the same three steps the analysis itself takes, so a shape reported
/// here is the shape the rules were handed. `"no initializer"` when the walk finds none, which is a different
/// answer from every expression shape and is counted as one.
fn initializer_shape(root: &cpp_parser::CppSyntaxNode, name_at: usize) -> String {
    // **The innermost node holding the name**, then **up**: the outermost node containing it is the root, and a
    // search from there finds the file's *first* initializer rather than this declaration's. Measured, that is what
    // the first version did: `auto d = q - raw();` was reported as a `CallExpr`, because the initializer it found
    // belonged to some earlier declaration and happened to end in a call.
    let Some(innermost) = root
        .descendants()
        .filter(|node| {
            let range = cpp_parser::source_range(node.text_range());
            range.start_offset <= name_at && name_at < range.end_offset()
        })
        .last()
    else {
        return "not found".to_string();
    };

    // Up until a node that **has** an initializer, which is the declaration: a declarator does not, its declaration
    // does, and the first ancestor that has one is the smallest such node rather than the file.
    for candidate in innermost.ancestors() {
        let initializer = candidate.descendants().find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::Initializer
        });
        let Some(initializer) = initializer else {
            continue;
        };
        return match initializer.children().last() {
            Some(expression) => format!("{:?}", cpp_parser::CppSyntaxKind::from(expression.kind())),
            None => "empty initializer".to_string(),
        };
    }

    "no initializer".to_string()
}

fn main() {
    let list = std::env::args().nth(1).expect("a file list");
    let limit = std::env::args()
        .position(|argument| argument == "--limit")
        .and_then(|at| std::env::args().nth(at + 1))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(usize::MAX);

    let paths: Vec<PathBuf> = std::fs::read_to_string(&list)
        .expect("the list reads")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();

    // **A session that discovered the toolchain**, rather than one configured by hand.
    //
    // This probe reads **renderings**, and a rendering needs a file's macro environment, which needs the unit walk,
    // which needs the include graph to resolve. `CompilerConfig::default()` has no include paths, so every
    // `#include <…>` in a standard-library header resolves to nothing, the walk enters one file, and the
    // environment for the file asked about is not in it — measured, that is what turned this probe's answer into
    // "0 declarations" after it was pointed at the rendering. `Session::open` asks the machine what it compiles
    // with, which is the only thing that knows where `<vector>` is.
    let root = paths
        .first()
        .and_then(|path| path.parent())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let mut session = cpp_code_analysis::Session::open(
        root.clone(),
        cpp_code_analysis::SessionFiles::new(
            cpp_code_analysis::OpenDocuments::new(),
            cpp_code_analysis::DiskFiles,
        ),
        cpp_code_analysis::WatchFilter::new(&root),
    );

    let indexed = Instant::now();
    session.add_project_files(paths.iter().cloned());
    session.index_everything();
    println!(
        "indexed {} files in {:?}",
        session.project_files().len(),
        indexed.elapsed()
    );

    let mut written_with_auto = 0usize;
    let mut deduced = 0usize;
    let mut refused = 0usize;
    let mut still_auto = 0usize;
    let mut examples: Vec<(String, String, String)> = Vec::new();
    let mut refused_examples: Vec<(String, String, String)> = Vec::new();
    let mut refusal_shapes: HashMap<String, usize> = HashMap::new();
    let mut refusals: HashMap<String, usize> = HashMap::new();

    for path in paths.iter().take(limit) {
        // **The rendering, built here rather than waited for.** `Session::view` answers from the file's own tokens
        // when no rendering is cached yet and asks the work loop for one — which is right for an editor, where a
        // query must not block, and wrong for a probe: it takes one look per file, so it would measure the reading
        // the product only uses as a fallback. The first run of this probe did exactly that and reported `_STD
        // _Get_unwrapped` and `_RANGES next` as unresolved names — macro spellings that a **rendering** does not
        // contain at all, which is how the mistake was visible.
        let Some(view) = session.view_of_the_rendering(path) else {
            continue;
        };

        for scope in view.scopes.scopes() {
            for binding in &scope.bindings {
                let Some(written) = cpp_code_analysis::declared_type_of(&view.root, binding) else {
                    continue;
                };
                if !written
                    .split(|character: char| !character.is_alphanumeric() && character != '_')
                    .any(|word| word == "auto")
                {
                    continue;
                }

                written_with_auto += 1;
                let answer = session.type_at(&view, binding.name_range.start_offset);
                let shown = match &answer {
                    cpp_code_analysis::Known::Yes(found) => {
                        let type_of = found.type_of.clone();
                        if type_of == "auto" {
                            still_auto += 1;
                            *refusals.entry("the placeholder again".to_string()).or_default() += 1;
                        } else {
                            deduced += 1;
                            if examples.len() < 12 {
                                examples.push((
                                    binding.name.text(),
                                    type_of.clone(),
                                    path.file_name().unwrap_or_default().to_string_lossy().to_string(),
                                ));
                            }
                        }
                        type_of
                    }
                    cpp_code_analysis::Known::Unknown(reason) => {
                        refused += 1;
                        // **The reason in full, payload and all.** It carries the spelling that could not be
                        // answered — `UnknownType("_Mypair._Myval2")` — and the first version of this probe split on
                        // the parenthesis and threw that away, so every refusal read as the same four words. The
                        // payload is what says *which* expression stopped the walk.
                        let why = format!("{reason:?}");
                        *refusals.entry(why.clone()).or_default() += 1;
                        // **What the refusal was about, not only how many there were.** A count says the analysis
                        // does not know a type; the declaration says whether that is a shape the rule is missing or
                        // one no rule can answer — and without it the 501 refusals below are a number rather than a
                        // work list, which is what the first version of this probe printed.
                        if refused_examples.len() < 400 {
                            refused_examples.push((
                                binding.name.text(),
                                written.to_string(),
                                path.file_name().unwrap_or_default().to_string_lossy().to_string(),
                            ));
                        }
                        // **The shape of the initializer**, because that is what a work list is divided by: a
                        // member access and a cast reach their types by different routes through this analysis, and
                        // a total of 500 across both says nothing about which route to build.
                        *refusal_shapes
                            .entry(initializer_shape(&view.root, binding.name_range.start_offset))
                            .or_default() += 1;
                        String::new()
                    }
                    cpp_code_analysis::Known::No => {
                        refused += 1;
                        *refusals.entry("nothing to ask".to_string()).or_default() += 1;
                        String::new()
                    }
                };
                let _ = shown;
            }
        }
    }

    println!(
        "\n--- `auto` declarations: {written_with_auto} written | {deduced} deduced | {refused} refused | \
         {still_auto} still `auto` ---"
    );
    let mut shapes: Vec<(String, usize)> = refusal_shapes.into_iter().collect();
    shapes.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    println!("--- refusals by the shape of the initializer ---");
    for (shape, count) in shapes {
        println!("{count:8}  {shape}");
    }
    let mut ranked: Vec<(String, usize)> = refusals.into_iter().collect();
    ranked.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    for (why, count) in ranked {
        println!("{count:8}  {why}");
    }
    for (name, type_of, file) in examples {
        println!("   {name} = {type_of}   ({file})");
    }
    println!("
--- what was refused, as written ---");
    for (name, written, file) in refused_examples {
        println!("   {name}: {written}   ({file})");
    }
}
