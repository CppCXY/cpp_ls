//! **Does the lookup-time guard patch still have a user?** — the measurement behind deleting it.
//!
//! `certain_declarations` removes candidates whose declaration is written in a `#if` region the compilation does not
//! take (`visibility_at`). That patch exists because a *summary* keeps both branches of every conditional; a
//! **rendering** has none left to remove. With ⑱ (a file with a rendering is read through it) and the background
//! project read (⑲, §7.13) almost every file now has one, so the filter should be nearly dead weight — and "nearly"
//! is what this probe measures rather than asserts.
//!
//! # How the comparison is made
//!
//! The same process asks the same questions twice — once with the patch (`CPPLS_NO_GUARD_FILTER` unset) and once
//! without it — and prints a digest per question. Two runs of the probe are diffed by the caller; the probe itself
//! only prints, because a probe that asserted would be a test with a machine-dependent corpus.
//!
//! A question is `definitions(name, from)` for every distinct name a file's own summary declares, asked from that
//! file: the population is the project's own declarations, which is what a reader's queries are about.

use cpp_code_analysis::{DiskFiles, Known, OpenDocuments, Session, SessionFiles, WatchFilter};

fn project() -> std::path::PathBuf {
    std::env::var_os("CPPLS_PROJECT_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Users\zc\Desktop\github\clice"))
}

fn sources_of(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    fn walk(directory: &std::path::Path, into: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path
                    .file_name()
                    .is_some_and(|name| name == ".git" || name == "build")
                {
                    continue;
                }
                walk(&path, into);
            } else if path.extension().is_some_and(|extension| {
                matches!(
                    extension.to_string_lossy().as_ref(),
                    "cpp" | "cc" | "cxx" | "h" | "hpp" | "hxx"
                )
            }) {
                into.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(root, &mut found);
    found.sort();
    found
}

#[test]
#[ignore = "a probe: answers every declaration question of a real project twice and prints the answers"]
fn what_the_guard_filter_still_removes() {
    let root = project();
    let all = sources_of(&root);
    let entry = all
        .iter()
        .find(|path| path.ends_with("src/AST/Selection.cpp"))
        .cloned()
        .unwrap_or_else(|| all[0].clone());

    let mut session = Session::open(
        root.clone(),
        SessionFiles::new(OpenDocuments::new(), DiskFiles),
        WatchFilter::new(&root),
    );
    session.add_project_files(all.iter().cloned());
    let text = std::fs::read_to_string(&entry).unwrap_or_default();
    session.did_open(&entry, &text);
    session.index_everything();
    while session.pending_work() > 0 {
        session.advance(64);
    }
    // **The renderings, read the way a reader's project becomes queryable**: the background schedule, driven here by
    // hand so that the probe's population is the one a settled session would have.
    std::thread::sleep(std::time::Duration::from_millis(10_500));
    while session.read_a_program_now().is_some() {}

    // **Both answers, in one process.** The population is read once and asked twice: with the patch and without it.
    // The memo that would otherwise serve the second pass from the first is dropped between them, and the switch is
    // the probe's rather than the environment's — see `probes::set_no_guard_filter` for why two processes would not do.
    let files: Vec<std::path::PathBuf> = all.to_vec();
    let (with, with_answers) = ask(&session, &files, false);
    session.index().forget_the_name_answers();
    let (without, _) = ask(&session, &files, true);

    println!("with the patch:    {with}");
    println!("without the patch: {without}");
    // **A digest that differs is the patch doing work; one that matches is a patch with nothing left to do.** The
    // count of differing characters is not a count of differing answers — the digest is folded — so the probe prints
    // the two digests and lets the equality be the statement.
    println!(
        "the two digests {} ({with_answers} answer(s))",
        if with == without {
            "are equal — the patch changed nothing"
        } else {
            "differ — the patch still changes answers"
        }
    );
    let cooked = session
        .index()
        .summaries()
        .filter(|summary| session.index().cooked_declarations(&summary.path).is_some())
        .count();
    println!(
        "{cooked} of {} file(s) answer from a rendering",
        session.index().len()
    );
}

/// One pass over every question: the digest of the answers, and how many answers there were.
fn ask(
    session: &Session<DiskFiles>,
    files: &[std::path::PathBuf],
    without_the_patch: bool,
) -> (String, usize) {
    cpp_code_analysis::probes::set_no_guard_filter(without_the_patch);
    let mut questions = 0usize;
    let mut answers = 0usize;
    let mut digest: u64 = 0xcbf29ce484222325;

    for file in files {
        let Some(summary) = session.index().summary(file) else {
            continue;
        };
        let mut names: Vec<String> = summary
            .declarations
            .iter()
            .filter(|fact| !fact.local && !fact.name.is_empty())
            .map(|fact| fact.name.clone())
            .collect();
        names.sort();
        names.dedup();
        for name in names {
            questions += 1;
            let answer = match session.index().definitions(&name, file) {
                Known::Yes(found) => {
                    let mut places: Vec<(u32, String, u32)> = found
                        .found
                        .iter()
                        .map(|one| {
                            (
                                one.fact.range.start_offset as u32,
                                one.file.to_string_lossy().to_string(),
                                one.fact.name_range.start_offset as u32,
                            )
                        })
                        .collect();
                    places.sort();
                    answers += places.len();
                    format!("{places:?}")
                }
                Known::Unknown(reason) => format!("unknown:{reason:?}"),
                Known::No => String::from("no"),
            };
            // A digest per question, folded into one number: the probe prints one line per run and the caller diffs
            // the two runs' `answer` lines.
            for byte in answer.as_bytes() {
                digest ^= u64::from(*byte);
                digest = digest.wrapping_mul(0x100000001b3);
            }
        }
    }

    (
        format!("{questions} question(s), {answers} answer(s), digest {digest:016x}"),
        answers,
    )
}
