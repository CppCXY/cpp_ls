//! **What a class costs in memory**, as the hover card shows it: the size, the alignment, and the offset of every data
//! member — the question rust-analyzer answers on a struct and C++ readers ask constantly.

use cpp_code_analysis::{
    CompilerConfig, Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
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

/// **A member written in one of the class's own template parameters is not sized**, and the layout says so instead of
/// printing a number beside a guess.
///
/// The lookup that sized it used to resolve the parameter's spelling by *bare name against the asking file*, where
/// some other class may well be called _Ty2 — measured on std::vector<widget::Widget>, whose card said
/// "size 9 byte(s), alignment 1" built from _Mypair's 9. See [ClassLayout::unknown].
#[test]
fn a_member_written_in_a_template_parameter_is_unknown() {
    let layout = layout(
        "\
template <class _Ty1, class _Ty2>
struct _Compressed_pair {
    _Ty2 _Mysecond;
};
template <class _Ty>
struct Box {
    _Compressed_pair<int, _Ty> pair;
    _Ty value;
    int tail;
};
",
        "Box",
    );
    assert_eq!(
        layout.unknown,
        ["pair", "value"],
        "the two dependent members are not sized"
    );
    assert_eq!(
        layout
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["tail"],
        "the members that *can* be sized are still placed"
    );
}

/// **A template-id whose member cannot be sized is unknown** — the instantiation is attempted, and a member written
/// in a parameter whose argument names nothing is where it stops. (`Holder<int>` itself is sized: see
/// `a_member_whose_type_is_an_instantiation_is_sized`.)
#[test]
fn a_member_whose_type_is_a_template_id_is_unknown() {
    let layout = layout(
        "\
template <class T> struct Holder { T value; };
struct Outer {
    Holder<NoSuchType> first;
    int second;
};
",
        "Outer",
    );
    assert_eq!(layout.unknown, ["first"]);
    assert_eq!(
        layout
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        ["second"]
    );
}

/// **A class template instance is sized from its members with the arguments substituted** — `Holder<double>` is
/// whatever `T value` is for `double`, plus its own `count`.
#[test]
fn a_member_whose_type_is_an_instantiation_is_sized() {
    let layout = layout(
        "\
template <class T>
struct Holder {
    T value;
    int count;
};
struct Outer {
    Holder<double> holder;
    int tail;
};
",
        "Outer",
    );
    assert_eq!(
        layout
            .fields
            .iter()
            .map(|field| (field.name.as_str(), field.offset, field.size))
            .collect::<Vec<_>>(),
        [("holder", 0, 16), ("tail", 16, 4)]
    );
    assert_eq!(layout.size, 24);
}

/// **A member alias of the instantiation is followed** — `using I = Inner<T>` inside `Outer2<T>` is `Inner<double>` at
/// the use, not the alias's own unsized spelling.
#[test]
fn a_member_whose_type_is_an_alias_of_an_instantiation_is_sized() {
    let layout = layout(
        "\
template <class T>
struct Inner {
    T a;
    T b;
};
template <class T>
struct Outer2 {
    using I = Inner<T>;
    I inner;
    int tail;
};
struct Use {
    Outer2<double> o;
};
",
        "Use",
    );
    assert_eq!(
        layout.size, 24,
        "inner is 16 bytes and the tail lands at 16"
    );
    assert_eq!(layout.alignment, 8);
}

/// **A pointer member of a template is eight bytes whatever the parameter is** — the substitution makes `T*` a
/// pointer, and a pointer's size does not depend on the pointee.
#[test]
fn a_pointer_member_of_a_template_is_a_word() {
    let layout = layout(
        "\
template <class T>
struct Vec {
    T* first;
    T* last;
};
struct Use2 {
    Vec<int> v;
};
",
        "Use2",
    );
    assert_eq!(layout.size, 16);
    assert_eq!(layout.alignment, 8);
}

/// **An empty base costs nothing** — the empty-base optimization, which is why `_Compressed_pair`'s `_Ty1` does not
/// make `std::vector` nine bytes.
#[test]
fn an_empty_base_of_a_template_costs_nothing() {
    let layout = layout(
        "\
struct Empty {};
template <class T>
struct WithBase : Empty {
    T value;
};
struct Use3 {
    WithBase<double> w;
};
",
        "Use3",
    );
    assert_eq!(layout.size, 8);
    assert_eq!(layout.alignment, 8);
}

/// **An alias template is what it names** — `template <class T> using Slot = Blob;` makes `Slot<int>` a `Blob`, and a
/// class template's *instance* is not the only spelling a member's type takes.
#[test]
fn a_member_whose_type_is_an_alias_template_is_sized() {
    let layout = layout(
        "\
struct Blob {
    int first;
    int second;
};
template <class T>
using Slot = Blob;
struct Use6 {
    Slot<double> slot;
    int tail;
};
",
        "Use6",
    );
    assert_eq!(layout.size, 12);
    assert_eq!(
        layout
            .fields
            .iter()
            .map(|field| (field.name.as_str(), field.offset))
            .collect::<Vec<_>>(),
        [("slot", 0), ("tail", 8)]
    );
}

/// **A base that is a template *parameter* costs nothing when the argument is empty** — the `_Compressed_pair<_Ty1,
/// _Ty2>` shape, which is what a standard container is built on.
#[test]
fn a_parameter_base_bound_to_an_empty_class_costs_nothing() {
    let layout = layout(
        "\
struct Empty {};
template <class B, class T>
struct Pair : private B {
    T second;
};
struct Use5 {
    Pair<Empty, double> p;
};
",
        "Use5",
    );
    assert_eq!(layout.size, 8);
    assert_eq!(layout.alignment, 8);
}

/// **A base with data is withheld, not guessed** — it would have to be laid at offset zero ahead of the members, and a
/// layout that ignored it would be too small.
#[test]
fn a_base_with_data_withholds_the_layout() {
    let layout = layout(
        "\
struct HasData {
    int x;
};
template <class T>
struct WithDataBase : HasData {
    T value;
};
",
        "WithDataBase<double>",
    );
    assert!(
        layout.unknown.iter().any(|name| name.contains("base")),
        "the base is named as unsized: {:?}",
        layout.unknown
    );
}
