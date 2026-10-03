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
                        let why = format!("{reason:?}");
                        let why = why.split('(').next().unwrap_or(&why).to_string();
                        *refusals.entry(why).or_default() += 1;
                        // **What the refusal was about, not only how many there were.** A count says the analysis
                        // does not know a type; the declaration says whether that is a shape the rule is missing or
                        // one no rule can answer — and without it the 501 refusals below are a number rather than a
                        // work list, which is what the first version of this probe printed.
                        if refused_examples.len() < 24 {
                            refused_examples.push((
                                binding.name.text(),
                                written.to_string(),
                                path.file_name().unwrap_or_default().to_string_lossy().to_string(),
                            ));
                        }
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
