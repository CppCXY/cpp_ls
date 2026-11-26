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

/// **Bitfields are placed, not withheld** — the `:` and its literal width are read out of the file, and the
/// storage-unit rules put each field where the compiler does.
#[test]
fn bitfields_are_placed_by_their_storage_units() {
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
    assert_eq!(layout.size, 8);
    assert_eq!(layout.alignment, 4);
    let placed: Vec<(&str, usize, Option<usize>, Option<usize>)> = layout
        .fields
        .iter()
        .map(|field| {
            (
                field.name.as_str(),
                field.offset,
                field.bit_offset,
                field.bit_width,
            )
        })
        .collect();
    assert_eq!(
        placed,
        [
            ("a", 0, Some(0), Some(3)),
            ("b", 0, Some(3), Some(5)),
            ("tail", 4, None, None),
        ]
    );
}

/// **The struct from the report**: `_BitInt(N)` members and bitfields, with the compiler's own layout.
///
/// Measured with MinGW's gcc under `-std=gnu2x`: `sizeof` 6, alignment 2; `three` bits 0–2, `signed_wide` bits 16–28
/// (a 2-byte unit at byte 2), `plain` at byte 4, `other` bits 40–41.
#[test]
fn a_struct_of_bitint_bitfields_has_a_layout() {
    let layout = layout(
        "\
struct bits {
    unsigned _BitInt(3) three : 3;
    _BitInt(13) signed_wide : 13;
    _BitInt(5) plain;
    unsigned _BitInt(4) other : 2;
};
",
        "bits",
    );
    assert_eq!(layout.size, 6);
    assert_eq!(layout.alignment, 2);
    let placed: Vec<(&str, usize, Option<usize>, Option<usize>)> = layout
        .fields
        .iter()
        .map(|field| {
            (
                field.name.as_str(),
                field.offset,
                field.bit_offset,
                field.bit_width,
            )
        })
        .collect();
    assert_eq!(
        placed,
        [
            ("three", 0, Some(0), Some(3)),
            ("signed_wide", 2, Some(16), Some(13)),
            ("plain", 4, None, None),
            ("other", 5, Some(40), Some(2)),
        ]
    );
}

/// **A bitfield whose width is not a literal withholds the layout**, because the fields after it would be guesses.
#[test]
fn a_bitfield_with_a_macro_width_is_still_unknown() {
    let layout = layout(
        "\
#define WIDTH 3
struct Flags {
    unsigned int a : WIDTH;
    int tail;
};
",
        "Flags",
    );
    assert_eq!(layout.unknown, ["a"]);
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

/// **`int64_t` and the other fixed-width typedefs are sized by name.** They are defined in the compiler's
/// `stdint.h`, which is not a project summary, so the typedef lookup has nothing to read — and every neovim struct
/// with an `OptInt` or `int64_t` member was "not computed" for that reason alone.
#[test]
fn a_fixed_width_standard_typedef_is_sized_by_its_name() {
    let layout = layout(
        "\
struct Fixed {
    int64_t wide;
    uint16_t half;
    size_t count;
};
",
        "Fixed",
    );
    assert_eq!(layout.size, 24);
    assert_eq!(layout.alignment, 8);
    let placed: Vec<(&str, usize, usize)> = layout
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field.offset, field.size))
        .collect();
    assert_eq!(placed, [("wide", 0, 8), ("half", 8, 2), ("count", 16, 8)]);
}

/// **An array of a struct, sized by a macro.** `#define COUNT 4` is the ordinary C spelling, and the array's element
/// is not a builtin — the two together used to make every member of this shape unsizable.
#[test]
fn an_array_of_a_struct_is_sized_from_its_macro() {
    let layout = layout(
        "\
#define COUNT 4
struct Point { int x; int y; };
struct Path {
    Point points[COUNT];
    char tag;
};
",
        "Path",
    );
    assert_eq!(layout.size, 36);
    assert_eq!(layout.alignment, 4);
    assert_eq!(layout.fields[0].offset, 0);
    assert_eq!(layout.fields[0].size, 32);
    assert_eq!(layout.fields[1].offset, 32);
}

/// **An array dimension that is an enumerator is a number too** — neovim sizes `lpos_T startpos[NSUBEXP]` this way.
#[test]
fn an_array_dimension_that_is_an_enumerator_is_sized() {
    let layout = layout(
        "\
enum { COUNT = 3 };
struct Point { int x; int y; };
struct Path {
    Point points[COUNT];
};
",
        "Path",
    );
    assert_eq!(layout.size, 24);
    assert_eq!(layout.alignment, 4);
}

/// **A macro dimension may be a constant expression**, not only a literal: neovim's `NMARKS` is
/// `('z' - 'a' + 1)`.
#[test]
fn a_macro_dimension_that_is_a_character_expression_is_sized() {
    let layout = layout(
        "\
#define NMARKS ('z' - 'a' + 1)
struct Marks {
    int m[NMARKS];
};
",
        "Marks",
    );
    assert_eq!(layout.size, 104);
    assert_eq!(layout.alignment, 4);
}

/// **A nested struct over the old 64-member budget is still priced.** `wininfo_S` has seven members, one of them a
/// 68-member `winopt_T`; with the budget at 64 the inner class was refused and the outer card said nothing.
#[test]
fn a_sized_member_past_the_old_budget_is_priced() {
    let mut source = String::from("struct Inner {\n");
    for member in 0..68 {
        source.push_str(&format!("    int a{member};\n"));
    }
    source.push_str("};\nstruct Outer {\n    Inner inner;\n    char tail;\n};\n");
    let layout = layout(&source, "Outer");
    assert_eq!(layout.size, 276);
    assert_eq!(layout.alignment, 4);
    assert_eq!(layout.fields[0].size, 272);
    assert_eq!(layout.fields[1].offset, 272);
}

/// **A typedef of a struct is a struct even when a member is a union.** The name of a typedef of an anonymous class
/// comes after its body, and an older reader took the nearest `union` before that name — the member's own — as the
/// class's keyword. It laid `Tagged` out as a union of `data` and `type`, against the compiler. Measured on neovim's
/// `Callback` (16 bytes, not 40) and `wininfo_S` (1816, not 1848).
#[test]
fn a_typedef_of_a_struct_with_an_inner_union_is_still_a_struct() {
    let layout = layout(
        "\
typedef struct {
    union {
        int i;
        double d;
    } data;
    int type;
} Tagged;
",
        "Tagged",
    );
    assert_eq!(layout.size, 16);
    assert_eq!(layout.alignment, 8);
    assert!(!layout.union);
    let placed: Vec<(&str, usize, usize)> = layout
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field.offset, field.size))
        .collect();
    assert_eq!(placed, [("data", 0, 8), ("type", 8, 4)]);
}

/// **An anonymous union member is laid out as a union**, not as a struct: its members overlap.
#[test]
fn an_anonymous_union_member_is_laid_out_as_a_union() {
    let layout = layout(
        "\
struct Holder {
    union {
        int i;
        double d;
    } value;
    int tail;
};
",
        "Holder",
    );
    assert_eq!(layout.size, 16);
    assert_eq!(layout.alignment, 8);
    assert_eq!(layout.fields[0].size, 8);
    assert_eq!(layout.fields[1].offset, 8);
}

/// **A typedef of a builtin is the builtin's size**, not the empty class's one byte. `Integer` is
/// `typedef int64_t Integer` in neovim's API definitions, and its card said `size 1 byte(s)` because an alias has no
/// members and the empty-class rule answered.
#[test]
fn a_typedef_of_a_builtin_reports_the_builtins_size() {
    let layout = layout("typedef int64_t Integer;\n", "Integer");
    assert_eq!(layout.size, 8);
    assert_eq!(layout.alignment, 8);
    assert!(layout.fields.is_empty());
}

/// **A data member spelled with an alias is the aliased type's size.**
#[test]
fn a_member_spelled_with_an_alias_uses_the_aliased_size() {
    let layout = layout(
        "\
typedef int64_t Integer;
struct Holder {
    Integer value;
    char tag;
};
",
        "Holder",
    );
    assert_eq!(layout.size, 16);
    assert_eq!(layout.alignment, 8);
    assert_eq!(layout.fields[0].size, 8);
    assert_eq!(layout.fields[1].offset, 8);
}
