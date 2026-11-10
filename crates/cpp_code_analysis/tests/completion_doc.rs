//! **Completion inside a documentation comment** — `@see`, `@param`, the ``[`…`]`` link — and the **edit** it sends.
//!
//! The edit is the whole of the contract here: a doc comment is prose around a word, and the client must replace
//! exactly the word the reader is typing. A range that starts too early eats the text before it — reported from an
//! editor as "按 Tab 会把前面的补全吃掉", i.e. accepting a name after `@see` removed the text already written.

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// A session over one file, index and rendering both built — the same pump a server does between keystrokes.
fn session_with(source: &str) -> Session<MemoryFiles> {
    let memory = MemoryFiles::new().with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.cpp")]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();
    session
}

/// The completion at the `|` marker, through the **plain view** the handler uses.
fn completion_where_marked(source: &str) -> (cpp_code_analysis::CompletionSet, usize) {
    let marker = source.find('|').expect("the fixture marks the cursor");
    let text = source.replace('|', "");
    let mut session = session_with(&text);
    let path = std::path::Path::new("/p/a.cpp");
    // The ask, the pump, the ask — the deferral contract every view has.
    let _ = session.view_of_the_file(path);
    session.index_everything();
    let view = session.view_of_the_file(path).expect("the file is held");
    let set = session.completions(
        &view,
        cpp_code_analysis::file::coordinates::FileOffset::new(marker),
    );
    (set, marker)
}

/// **The edit after `@see` replaces the typed target and nothing else.**
///
/// `@see Wid|` in a `/** … */` block: the item's edit has to cover the three bytes of `Wid`, so that accepting
/// `Widget` leaves `@see Widget` — and never reaches back over the `@see ` before it.
#[test]
fn the_edit_after_see_covers_the_typed_target_only() {
    let source = "\
/** A ship. @see Wid| */
struct Widget { int size; };
";
    let (set, marker) = completion_where_marked(source);
    assert!(
        set.items.iter().any(|item| item.label == "Widget"),
        "the declaration is what `@see` is for: {:?}",
        set.items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
    );
    // The word typed is `Wid`, starting three bytes into the text before the cursor.
    let expected = marker - "Wid".len();
    assert_eq!(
        set.replace.start_offset, expected,
        "the edit starts where the typed word starts, not at `@see`"
    );
    assert_eq!(set.replace.length, "Wid".len(), "and covers exactly it");
}

/// **A link's target is the same edit**: ``[`Wid|`]`` replaces `Wid`, not the backtick before it.
#[test]
fn the_edit_inside_a_link_covers_the_typed_target_only() {
    let source = "\
/** See [`Wid|`] for the size. */
struct Widget { int size; };
";
    let (set, marker) = completion_where_marked(source);
    assert!(
        set.items.iter().any(|item| item.label == "Widget"),
        "a link's target is a program name: {:?}",
        set.items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>()
    );
    let expected = marker - "Wid".len();
    assert_eq!(set.replace.start_offset, expected);
    assert_eq!(set.replace.length, "Wid".len());
}
