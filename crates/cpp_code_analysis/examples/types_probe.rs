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

    let mut session = cpp_code_analysis::Session::with_config(
        std::env::temp_dir().join("types-probe-root"),
        cpp_code_analysis::SessionFiles::new(
            cpp_code_analysis::OpenDocuments::new(),
            cpp_code_analysis::DiskFiles,
        ),
        cpp_code_analysis::WatchFilter::new(std::env::temp_dir().join("types-probe-root")),
        cpp_code_analysis::CompilerConfig::default(),
    );
    std::fs::create_dir_all(std::env::temp_dir().join("types-probe-root")).ok();

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
    let mut refusals: HashMap<String, usize> = HashMap::new();

    for path in paths.iter().take(limit) {
        let Some(view) = session.view(path) else {
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
}
