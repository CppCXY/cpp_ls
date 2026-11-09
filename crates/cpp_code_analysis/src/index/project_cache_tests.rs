use std::path::Path;

use super::{ProjectIndex, Visibility, normalize};
use crate::file::paths::FileProvider;
use crate::{CompilerConfig, FileIndexer, MemoryFiles, SummaryKey};

fn summary(files: &MemoryFiles, path: &str) -> crate::FileSummary {
    let path = Path::new(path);
    let source = files.read(path).expect("fixture text");
    FileIndexer::new(files, &CompilerConfig::default()).index(path, &source, SummaryKey::new(0, 0))
}

#[test]
fn editing_a_known_file_keeps_unrelated_visibility_answers() {
    let files = MemoryFiles::new()
        .with_file("/p/x.h", "#define FLAG 1\n")
        .with_file("/p/a.cpp", "#include \"x.h\"\nint a;\n")
        .with_file("/p/b.cpp", "int b;\n");
    let mut index = ProjectIndex::new();
    for path in ["/p/x.h", "/p/a.cpp", "/p/b.cpp"] {
        index.insert(summary(&files, path));
    }
    assert!(
        index
            .dependents_of(Path::new("/p/x.h"))
            .iter()
            .any(|path| normalize(path) == normalize(Path::new("/p/a.cpp")))
    );
    {
        let mut answers = index.visibility_answers.lock().unwrap();
        for path in ["/p/x.h", "/p/a.cpp", "/p/b.cpp"] {
            answers.insert((normalize(Path::new(path)), 1), Visibility::Active);
        }
    }

    index.insert(summary(&files, "/p/x.h"));
    let answers = index.visibility_answers.lock().unwrap();
    assert!(!answers.contains_key(&(normalize(Path::new("/p/x.h")), 1)));
    assert!(!answers.contains_key(&(normalize(Path::new("/p/a.cpp")), 1)));
    assert!(answers.contains_key(&(normalize(Path::new("/p/b.cpp")), 1)));
}

#[test]
fn a_new_path_invalidates_all_visibility_answers() {
    let files = MemoryFiles::new()
        .with_file("/p/a.cpp", "int a;\n")
        .with_file("/p/new.h", "int added;\n");
    let mut index = ProjectIndex::new();
    index.insert(summary(&files, "/p/a.cpp"));
    index
        .visibility_answers
        .lock()
        .unwrap()
        .insert((normalize(Path::new("/p/a.cpp")), 1), Visibility::Active);

    index.insert(summary(&files, "/p/new.h"));
    assert!(index.visibility_answers.lock().unwrap().is_empty());
}
