//! **What a class costs in memory**, as the hover card shows it: the size, the alignment, and the offset of every data
//! member — the question rust-analyzer answers on a struct and C++ readers ask constantly.

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter, Known,
};

/// The layout of `name` in a one-file fixture.
fn layout(source: &str, name: &str) -> cpp_code_analysis::sema::layout::ClassLayout {
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
    let view = session.view("/p/a.cpp").expect("the file is read");
    match session.the_layout_of(&view, name) {
        Known::Yes(layout) => {
            eprintln!("[layout] {name}: {layout:?}");
            layout
        }
        other => panic!("`{name}` has no layout: {other:?}"),
    }
}

/// **The rule a struct is laid out by**: each member at the next multiple of its own alignment, and the class rounded
/// up to the largest one.
#[test]
fn a_struct_lays_its_members_out_by_alignment() {
    let layout = layout(
        "\
struct Widget {
    int x;
    double y;
    char c;
    int *p;
    short arr[3];
};
",
        "Widget",
    );
    assert_eq!(layout.size, 40, "38 rounded up to the alignment of 8");
    assert_eq!(layout.alignment, 8);
    let placed: Vec<(&str, usize, usize)> = layout
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field.offset, field.size))
        .collect();
    assert_eq!(
        placed,
        [
            ("x", 0, 4),
            ("y", 8, 8),
            ("c", 16, 1),
            ("p", 24, 8),
            ("arr", 32, 6),
        ]
    );
}

/// **A union** puts everything at 0 and is as large as its largest member.
#[test]
fn a_union_is_as_large_as_its_largest_member() {
    let layout = layout(
        "\
union Value {
    int i;
    double d;
    char raw[3];
};
",
        "Value",
    );
    assert_eq!(layout.size, 8);
    assert_eq!(layout.alignment, 8);
    assert!(layout.union);
    assert!(layout.fields.iter().all(|field| field.offset == 0));
}

/// **A nested class is sized by its own layout**, so a member of a member's type is placed correctly.
#[test]
fn a_member_whose_type_is_a_class_is_sized_by_that_class() {
    let layout = layout(
        "\
struct Point { int x; int y; };
struct Line { Point from; Point to; char tag; };
",
        "Line",
    );
    assert_eq!(
        layout
            .fields
            .iter()
            .map(|field| (field.name.as_str(), field.offset, field.size))
            .collect::<Vec<_>>(),
        [("from", 0, 8), ("to", 8, 8), ("tag", 16, 1)]
    );
    assert_eq!(layout.size, 20);
}

/// **A member nothing can size withholds the layout**: the unknown member is named and no size is invented.
#[test]
fn a_member_that_cannot_be_sized_is_named_rather_than_guessed() {
    let layout = layout(
        "\
struct Holder {
    int x;
    Unknown w;
};
",
        "Holder",
    );
    assert_eq!(layout.unknown, ["w"]);
    // The known member is still placed, which is what the card shows beside the note.
    assert_eq!(layout.fields.len(), 1);
}

/// **A bitfield withholds it too**, because a fact carries no width and every offset after one would be wrong.
#[test]
fn a_bitfield_member_is_named_rather_than_placed_wrongly() {
    let layout = layout(
        "\
struct Flags {
    unsigned int a : 3;
    unsigned int b : 5;
    int tail;
};
",
        "Flags",
    );
    assert_eq!(layout.unknown, ["a", "b"]);
}

/// **An empty class is one byte**, which is the language's rule rather than arithmetic.
#[test]
fn an_empty_class_is_one_byte() {
    let layout = layout("struct Empty {};\n", "Empty");
    assert_eq!(layout.size, 1);
    assert_eq!(layout.alignment, 1);
}
/// **A typedef is the type it names**, which is how most real C writes its members: `pos_T b_op_start` is a struct.
#[test]
fn a_member_whose_type_is_a_typedef_is_sized_through_it() {
    let layout = layout(
        "\
struct Inner { int x; int y; };
typedef Inner Alias;
struct Outer { Alias a; char c; };
",
        "Outer",
    );
    assert_eq!(
        layout
            .fields
            .iter()
            .map(|field| (field.name.as_str(), field.offset, field.size))
            .collect::<Vec<_>>(),
        [("a", 0, 8), ("c", 8, 1)]
    );
    assert_eq!(layout.size, 12);
}