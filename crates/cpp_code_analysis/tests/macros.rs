//! **Macros: completion and hover**, the two features a reader expects on a `#define` and the report says are missing.
//!
//! A macro is not a declaration in the tree — the preprocessor removed it — so both layers have to read the *macro facts*
//! rather than the scope walk. These tests ask what the queries answer today, and fail with the list they got.

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    file::coordinates::FileOffset,
};

const SOURCE: &str = "\
#define WIDTH 10
#define MAX(a, b) ((a) > (b) ? (a) : (b))
void f() {
    int n = MAX;
    int w = WIDTH;
}
";

fn session() -> (Session<MemoryFiles>, std::path::PathBuf) {
    let memory = MemoryFiles::new().with_file("/p/a.cpp", SOURCE);
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
    (session, std::path::PathBuf::from("/p/a.cpp"))
}

/// **Completion offers the file's own macros**, object-like and function-like.
///
/// # Why this is `#[ignore]`d, and what it is not
///
/// Measured: `completions` at `int n = MAX;` answers **zero items of any kind**, not "the declarations but not the macros".
/// So the vocabulary is not the missing piece — `completion/mod.rs` already maps `DeclKind::MacroLike` to `ItemKind::Macro`,
/// and a macro is a declaration in its file's summary. What is missing is the *context* the query computes from the written
/// text at that offset: with no context, no list is built at all.
///
/// # The cause, measured
///
/// Four positions in the same file, asked with `completion_at_four_positions`:
///
/// ```text
/// inside `MA`        → 1 item: `main`     the prefix filter works, and no macro is in the vocabulary
/// after `MAX`        → 0 items            the prefix is `MAX`, and nothing in the vocabulary starts with it
/// after `WIDTH`      → 89 items           the vocabulary is there, and `WIDTH` is not in it
/// after `{`          → 87 items
/// ```
///
/// So the written-text context and the prefix are both fine, and the crossing between the two texts is not the cause (a
/// function-like macro that is **not called** survives the cook — `an_uncalled_function_like_macro_survives_the_cook` asserts
/// that). What is missing is the **vocabulary**: `name_completions_at` collects the visible declarations from the index and
/// never asks the file's own summary for its `macros`. The fix is one append — `summary.macros` as `OfferedName`s (the struct
/// is `file`, `fact: DeclFact`, `depth`, and a distance) — and it belongs in `name_completions_at`, where both `index` and
/// `path` are in hand.
#[test]
fn a_macro_is_offered_by_completion() {
    let (session, path) = session();
    let view = session.view(&path).expect("the file is cooked");
    // The cursor just after `MAX` in `int n = MAX;` — the reader is typing a macro's name.
    let after = SOURCE.find("MAX;").expect("the fixture uses it") + 3;
    let set = session.completions(&view, FileOffset::new(after));
    let offered: Vec<String> = set.items.iter().map(|item| item.label.clone()).collect();
    println!("offered {} item(s): {offered:?}", offered.len());
    assert!(
        offered.iter().any(|label| label == "MAX"),
        "`MAX` is defined in this file and should be offered: {offered:?}"
    );
    // …and the object-like one at its own cursor: the prefix filters, so `WIDTH` is asked for where `WID` is typed.
    let after_wid = SOURCE.find("WIDTH;").expect("the fixture uses it") + 3;
    let set = session.completions(&view, FileOffset::new(after_wid));
    let offered: Vec<String> = set.items.iter().map(|item| item.label.clone()).collect();
    assert!(
        offered.iter().any(|label| label == "WIDTH"),
        "…and so should `WIDTH`: {offered:?}"
    );
}

/// **Hover on a macro's use says what it is.**
#[test]
fn a_macros_use_has_a_hover() {
    let (session, path) = session();
    let view = session.view(&path).expect("the file is cooked");
    let at = SOURCE.find("MAX;").expect("the fixture uses it") + 1;
    let found = session.macro_at_a_file_offset(&view, FileOffset::new(at));
    println!("macro_at_a_file_offset at the use: {found:?}");
    assert!(
        matches!(found, cpp_code_analysis::Known::Yes(_)),
        "a macro's use has a definition to point at"
    );
}

/// **A function-like macro's use is not expanded when it is not called**, so the rendering keeps its name — which is what
/// makes the empty completion list a puzzle rather than a consequence.
///
/// Measured: `#define MAX(a, b) …` followed by `int n = MAX;` leaves `MAX` in the rendering, and file offset 83 maps to
/// reading 20, inside `AX; int `. So the crossing is **not** lossy here, `name_at` finds `MAX` (which is how the old hover
/// answered `NotDeclaredHere("MAX")` rather than `UnparsableName`), and the hover's failure was the offset it then used.
/// The completion list is empty for a different reason, still to be found — this test records what has been ruled out.
#[test]
fn an_uncalled_function_like_macro_survives_the_cook() {
    let (session, path) = session();
    let view = session.view(&path).expect("the file is cooked");
    let at = SOURCE.find("MAX;").expect("the fixture uses it") + 1;
    let reading = view
        .reading_offset_of(FileOffset::new(at))
        .expect("the offset maps");
    println!("file offset {at} → reading {}", reading.get());
    assert!(
        view.source.contains("MAX;"),
        "`MAX` is not called, so the cook keeps its name in the rendering"
    );
}

/// **Where completion stops answering at all** — the offsets, side by side.
///
/// The ignored test asks for a macro; this one asks what the query does at four positions in the same file, which is how the
/// question "is it the macro or the position?" gets an answer instead of another hypothesis.
#[test]
fn completion_at_four_positions() {
    let (session, path) = session();
    let view = session.view(&path).expect("the file is cooked");
    let ask = |label: &str, at: usize| {
        let set = session.completions(&view, FileOffset::new(at));
        let labels: Vec<String> = set.items.iter().map(|item| item.label.clone()).collect();
        println!(
            "{label:<28} offset {at:>3} → {} item(s){}{}",
            labels.len(),
            if labels.is_empty() { "" } else { ": " },
            labels.iter().take(6).cloned().collect::<Vec<_>>().join(", ")
        );
    };
    ask("after `MAX` in `= MAX;`", SOURCE.find("MAX;").unwrap() + 3);
    ask("inside `MA`", SOURCE.find("MAX;").unwrap() + 2);
    ask("after `WIDTH` in `= WIDTH;`", SOURCE.find("WIDTH;").unwrap() + 5);
    ask("start of `int w`", SOURCE.find("    int w").unwrap() + 4);
    ask("inside the body, after `{`", SOURCE.find("void f() {").unwrap() + 10);
}

/// **A macro defined in an included header is offered too** — the common case: a project's `config.h` defines the names its
/// sources use, and the file that uses them has no `#define` of its own.
#[test]
fn a_headers_macro_is_offered() {
    use cpp_code_analysis::{
        CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
    };
    let header = "#define FEATURE_ENABLED 1\n";
    let source = "#include \"config.h\"\nvoid f() {\n    int n = FEATURE;\n}\n";
    let memory = MemoryFiles::new()
        .with_file("/p/config.h", header)
        .with_file("/p/a.cpp", source);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/a.cpp"),
        std::path::PathBuf::from("/p/config.h"),
    ]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();
    let view = session.view("/p/a.cpp").expect("the file is cooked");
    let at = source.find("FEATURE;").expect("the fixture uses it") + 3;
    let set = session.completions(&view, FileOffset::new(at));
    let labels: Vec<String> = set.items.iter().map(|item| item.label.clone()).collect();
    println!("offered at `FEATURE`: {labels:?}");
    assert!(
        labels.iter().any(|label| label == "FEATURE_ENABLED"),
        "a header's `#define` is a name this file can use: {labels:?}"
    );
}

/// **A function-like macro is offered with its parenthesis** — the only hint an item can carry about how it is called.
#[test]
fn a_function_like_macro_is_offered_with_its_parenthesis() {
    let (session, path) = session();
    let view = session.view(&path).expect("the file is cooked");
    let at = SOURCE.find("MAX;").expect("the fixture uses it") + 3;
    let set = session.completions(&view, FileOffset::new(at));
    let item = set
        .items
        .iter()
        .find(|item| item.label == "MAX")
        .expect("`MAX` is offered");
    println!("MAX: {item:?}");
    let written = format!("{item:?}");
    assert!(
        written.contains("insert_text") || written.contains("MAX("),
        "the item says the macro takes arguments: {written}"
    );
}