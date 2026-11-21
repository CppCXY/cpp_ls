//! A smart pointer's arrow reaches the pointee — `element_type`, the standard's pointer protocol.

use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::{
    CompilerConfig, Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

fn session_with(files: &[(&str, &str)]) -> Session<MemoryFiles> {
    let mut memory = MemoryFiles::new();
    for (path, source) in files {
        memory = memory.with_file(*path, *source);
    }

    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    let paths: Vec<std::path::PathBuf> = files
        .iter()
        .map(|(path, _)| std::path::PathBuf::from(path))
        .collect();
    session.add_project_files(paths);
    session.index_everything();
    session
}

/// **`auto owned = std::make_unique<Widget>(1); owned->size()` is an `int`.**
///
/// The member read asked `unique_ptr` for `size`, because a smart pointer is a class and `member_access_class` only
/// follows a **raw** pointer. The standard's answer is `element_type`, which the completion layer already followed —
/// this side did not, so the read had no type and the field check reported "`owned` has no member `size`".
#[test]
fn a_member_read_through_a_smart_pointers_arrow_is_the_pointers_member() {
    let header = "\
namespace std {
template <class T> struct pointers { typedef typename T::pointer pointer; };
template <class T> struct unique_ptr {
    typedef T element_type;
    typedef typename pointers<T>::pointer pointer;
    pointer operator->() const;
};
}
";
    let source = "\
#include \"smart.h\"

struct Widget { int size() const; };

int f() {
    std::unique_ptr<Widget> owned;
    auto value = owned->size();
    return value;
}
";
    let mut session = session_with(&[("/p/smart.h", header), ("/p/a.cpp", source)]);
    let path = std::path::PathBuf::from("/p/a.cpp");
    if session.view(&path).is_none() {
        session.cook(&path);
    }
    let view = session.view(&path).expect("the file is readable");
    let needle = "value = owned";
    let at = source.find(needle).expect("the needle is written") + needle.len();
    let reading = view
        .reading_offset_of(FileOffset::new(at))
        .expect("the cursor has a reading offset");
    match session.type_at(&view, reading) {
        Known::Yes(found) => assert_eq!(found.type_of.to_string(), "int"),
        other => panic!("the read is an `int`: {other:?}"),
    }
}
