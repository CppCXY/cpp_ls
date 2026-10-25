//! Call signatures: which function is being called, and which parameter the cursor is in.
//!
//! The interesting half is the *active parameter*: the cursor is not on a parameter, it is between two of them, and
//! the states a half-typed call goes through are all different — nothing written, one argument written, a comma
//! just typed. Each is pinned here, and so is the refusal: a cursor on the callee is not a call being typed.

use cpp_code_analysis::signature::CallSignature;
use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
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
    session.add_project_files(
        files
            .iter()
            .map(|(path, _)| std::path::PathBuf::from(*path)),
    );

    // **The ask, and then the pump** - see `tests/completion.rs`'s `cooked_view`. A view is of the file's *cooked
    // reading*, and a rendering is built because something asked for it.
    for (path, _) in files {
        session.want_cooked_reading(std::path::Path::new(path));
    }
    session.index_everything();

    session
}

/// The signature at a cursor written in the fixture as `|`.
///
/// The marker is the whole point of the fixture: the states under test differ by *where* the cursor is, so a test
/// that passed an offset by hand would be a test of the arithmetic rather than of the analysis.
///
/// **One** signature, because that is what all but one of these fixtures declare — the overload case is its own test
/// below, and it is the plural that is the API: a name with four declarations has four signatures, and a test helper
/// that took the first would hide exactly the thing worth pinning.
fn signature_where_marked(source: &str) -> (String, Option<CallSignature>) {
    let offset = source.find('|').expect("the fixture marks the cursor");
    let without_marker = source.replace('|', "");

    let session = session_with(&[("/p/a.cpp", &without_marker)]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    let signature = {
        // **The cursor the fixture marks is the file's; `signatures_at` reads the rendering.** A comment is removed
        // before the parse, so the two offsets differ by everything the cook deleted — 46 bytes on the fixture below,
        // which put the cursor in the middle of the *declaration* and made every signature answer `None`. Measured:
        // this one helper is the reason all five of this suite's failures were about a call that resolves.
        let in_the_reading = view
            .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(
                offset,
            ))
            .expect("the cursor is in the rendering");
        session
            .signatures_at(&view, in_the_reading)
            .into_iter()
            .next()
    };

    (without_marker, signature)
}

/// **A constructor call is a call** — `Widget(|)` should offer `Widget(int size, int extra)`, and today offers
/// nothing.
///
/// **The cause is in the grammar, not here**, and finding that took two rounds of measurement:
///
/// ```text
///   SIGNATURES call "Widget()" callee "Widget" -> 1 candidate(s)   the call and the class are both found
///   CONSTRUCTORS `Widget` at 6: [23]                               and a member named `Widget` is found in it
///   SOURCE at 23: list None                                        but its parameter list cannot be read
/// ```
///
/// `cpp_dump` says why — a constructor declaration inside a class body is parsed as a **variable**:
///
/// ```text
///   Declaration@27..56
///     DeclSpecifierSeq@27..33 > NamedType > NameExpr "Widget"   <- the class name became the TYPE
///     InitDeclarator@33..54 > Declarator@33..54
///       ParameterList@33..54                                    <- the list hangs off a nameless declarator
/// ```
///
/// So there is no constructor *declaration* in the tree for a signature to be rendered from. The same gap is what
/// makes MSVC's `sprintf_s` arrive in the index as a `Variable` with `type=Some("_CSTD")` — measured while chasing
/// a false positive in `unused` — so it is one grammar defect with several symptoms, and it is why this test is
/// ignored rather than fixed here.
///
/// The fixture writes `Widget w = Widget(|);` rather than `Widget w(|);` on purpose: the second is the most
/// vexing parse, a **declaration** of a function `w` returning `Widget`, so there is no call at all. (The first
/// version of this test used it and measured nothing.)
#[test]
fn a_constructor_call_offers_its_parameters() {
    let source = "\
class Widget {
public:
    Widget(int size, int extra);
};

int main() {
    Widget w = Widget(|);
    return 0;
}
";
    let (_, signature) = signature_where_marked(source);
    let label = signature
        .map(|signature| signature.label)
        .unwrap_or_default();
    assert!(
        label.contains("size"),
        "the constructor's own parameters, or nothing: {label:?}"
    );
}
/// **Direct initialization is a call the grammar does not spell as one.**
///
/// `LuaParser p(file, std::move(tokens));` parses as a *declaration* whose declarator holds a parameter list, so the
/// query that looks for a call found none and the popup was empty — measured on a real project
/// (`CodeFormat.cpp:72` answered nothing while `LuaParser`'s constructors were facts in the index).
///
/// The answer is the class's constructors, and the class is the last name the declaration's specifiers write. A
/// function *definition* is the same declaration + declarator + list, and the body is what tells them apart.
#[test]
fn a_direct_initialization_offers_the_constructors() {
    let source = "\
class Widget {
public:
    Widget(int size, int extra);
};
void f() {
    Widget w(|);
}
";
    let (_, signature) = signature_where_marked(source);
    let label = signature
        .map(|signature| signature.label)
        .unwrap_or_default();
    println!("direct initialization: {label:?}");
    assert!(
        label.contains("size") && label.contains("extra"),
        "the constructor's own parameters: {label:?}"
    );
}

/// **The overload the call's arguments select comes first.**
///
/// A client whose `activeSignature` defaults to the first entry shows the language's answer; before the resolver was
/// used here it showed whichever overload the index happened to hold first — a popup that is *right* about the set and
/// wrong about the call. The list is still sent whole, so a reader can still cycle to the others.
#[test]
fn the_overload_the_arguments_select_is_the_first_signature() {
    let integer = "\
void print(int value);
void print(const char* text);
void g() {
    print(|1);
}
";
    let (_, signature) = signature_where_marked(integer);
    let label = signature
        .map(|signature| signature.label)
        .unwrap_or_default();
    assert!(
        label.starts_with("print(int"),
        "an `int` argument selects the `int` overload: {label:?}"
    );

    let text = "\
void print(int value);
void print(const char* text);
void g() {
    print(|\"x\");
}
";
    let (_, signature) = signature_where_marked(text);
    let label = signature
        .map(|signature| signature.label)
        .unwrap_or_default();
    assert!(
        label.starts_with("print(const char*"),
        "a string literal selects the `const char*` overload: {label:?}"
    );
}
/// **A template candidate is ranked as the function the call instantiates.**
///
/// The distinguishing fixture: two templates whose declared spellings (`T*` and `T`) say nothing about an `int*`
/// argument — without deduction both are `Match::Unranked` and the call is ambiguous, so nothing is chosen. With it,
/// the first is `int*` (exact) and the second is `int` against an `int*` (not viable), so the `T*` one is chosen and
/// comes first. If deduction silently stopped working, this test would still pass its first half and the signature
/// would be the `T` one — so the assertion is on **which** of the two.
/// **The fixture's argument is a pointer variable, not `&n`** — measured: `take(&n)` produces **no call node at all**
/// (`call_around` answers `None`), so the address-of expression as a call argument is a parser defect of its own,
/// recorded in `docs/overloads.md` §12. With `int* p = &n; take(p);` the callees are found (two candidates, both with
/// their parameter lists) and the deduction is what decides between them.
#[test]
fn a_template_candidate_is_ranked_as_the_function_the_call_instantiates() {
    let source = "\
template <typename T> void take(T* value);
template <typename T> void take(T value);
int g() { int n = 0; int* p = &n; take(|p); return 0; }
";
    let (_, signature) = signature_where_marked(source);
    let label = signature
        .map(|signature| signature.label)
        .unwrap_or_default();
    assert!(
        label.contains("T*"),
        "an `int*` argument selects the `T*` overload: {label:?}"
    );
}
/// **An ordinary function is what a tie means.**
///
/// The template is written **first**, so a resolution without the rule would keep the declaration order and show
/// `pick(T value)` — which is a list that is right about the set and wrong about the call. With it, `f(int)` is chosen
/// for `f(5)` even though the template deduces to exactly the same type.
#[test]
fn an_ordinary_function_beats_a_template_that_is_equally_good() {
    let source = "\
template <typename T> void pick(T value);
void pick(int value);
int g() { pick(|5); return 0; }
";
    let (_, signature) = signature_where_marked(source);
    let label = signature
        .map(|signature| signature.label)
        .unwrap_or_default();
    assert!(
        label.starts_with("pick(int"),
        "the ordinary function comes first, not the template: {label:?}"
    );
}
/// The label at the cursor, and the text each declared span points at.
fn label_where_marked(source: &str) -> (String, Vec<String>, Option<usize>) {
    let (_, signature) = signature_where_marked(source);
    let signature = signature.unwrap_or_else(|| panic!("no signature at the cursor in {source:?}"));

    let spans: Vec<String> = signature
        .parameters
        .iter()
        .map(|(range, _)| signature.label[range.clone()].to_string())
        .collect();

    (signature.label, spans, signature.active_parameter)
}

const SCALE: &str = "double scale(int count, double factor);\ndouble f() { return scale(|); }\n";

/// The signature is the declaration's own text, and the spans point at the parameters inside it.
#[test]
fn a_call_reports_the_declarations_own_signature() {
    let (label, spans, active) = label_where_marked(SCALE);

    assert_eq!(label, "scale(int count, double factor)");
    assert_eq!(
        spans,
        vec!["int count".to_string(), "double factor".to_string()]
    );
    assert_eq!(active, Some(0), "the cursor is in the first parameter");
}

/// **Every state a half-typed argument list goes through**, with the parameter each one is asking about.
#[test]
fn the_active_parameter_follows_the_arguments() {
    let cases = [
        ("scale(|)", 0),
        ("scale(1|)", 0),
        ("scale(1,|)", 1),
        ("scale(1, |)", 1),
        ("scale(1, 2|)", 1),
        // Past the declared parameters: the index is left as it is rather than clamped, because a variadic call
        // really is past the list.
        ("scale(1, 2, |)", 2),
    ];

    for (call, expected) in cases {
        let source =
            format!("double scale(int count, double factor);\ndouble f() {{ return {call}; }}\n");
        let (_, _, active) = label_where_marked(&source);
        assert_eq!(active, Some(expected), "in `{call}`");
    }
}

/// **A declaration in a file whose rendering nobody has built still gets a popup.**
///
/// The popup used to be answered *only* from the declaring file's tree, and that tree is a **rendering** — which a
/// query does not build (65–190 ms for a header), so `Session::view` queues one and answers `None` and the candidate
/// was skipped. A client asks once per `(`, so "the next request will have it" never happens: measured on a real
/// C++23 module project, `math::add(` answered **nothing** on its first `(` and one signature on the second, which
/// from the outside is "the signature popup almost never appears".
///
/// The fallback is the declaration's own **fact**, which every summary already carries a parameter list in. What it
/// gives up is the parameter spans; what it keeps is the label, the count and the active parameter.
#[test]
fn a_declaration_in_an_uncooked_file_still_answers() {
    // **Only the caller is cooked.** `session_with` asks for every file's rendering, which would answer this test
    // through the tree path and prove nothing; the state a fresh `(` arrives in is one where the *declaring* file
    // has no rendering yet.
    let files = [
        (
            "/p/math.cppm",
            "export module math;\nexport namespace math {\nint add(int lhs, int rhs);\n}\n",
        ),
        (
            "/p/a.cpp",
            "import math;\nint f() { return math::add(|1, 2); }\n",
        ),
    ];
    let mut memory = MemoryFiles::new();
    for (path, source) in files {
        memory = memory.with_file(path, source);
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([
        std::path::PathBuf::from("/p/math.cppm"),
        std::path::PathBuf::from("/p/a.cpp"),
    ]);
    session.want_cooked_reading(std::path::Path::new("/p/a.cpp"));
    session.index_everything();

    let view = session.view("/p/a.cpp").expect("the caller is held");
    assert!(
        session.view("/p/math.cppm").is_none(),
        "the fixture is only meaningful while the declaration's file has no rendering"
    );
    let cursor = view
        .source
        .find("math::add(")
        .expect("the fixture writes the call")
        + "math::add(".len();

    let signatures = session.signatures_at(
        &view,
        cpp_code_analysis::file::coordinates::ReadingOffset::new(cursor),
    );
    assert_eq!(
        signatures.len(),
        1,
        "the fact is enough to write the label: {signatures:?}"
    );
    assert_eq!(signatures[0].label, "math::add(int lhs, int rhs)");
    assert_eq!(
        signatures[0].active_parameter,
        Some(0),
        "the count and the active parameter do not need a tree"
    );
    assert!(
        signatures[0].parameters.is_empty(),
        "the spans do need one, and are honestly absent rather than wrong"
    );
}

/// **A cursor on the callee is not a call being typed.** It is a question about the name — which is hover's and
/// definition's business — and a signature popup there would appear while the reader is still choosing the
/// function.
#[test]
fn a_cursor_on_the_callee_has_no_signature() {
    let (_, signature) =
        signature_where_marked("double scale(int count);\ndouble f() { return sca|le(1); }\n");

    assert!(signature.is_none());
}

/// A nested call answers about the **innermost** one: `outer(inner(|))` is a question about `inner`.
#[test]
fn a_nested_call_reports_the_innermost_signature() {
    let (label, _, _) = label_where_marked(
        "int inner(int depth);\nint outer(int width);\nint f() { return outer(inner(|)); }\n",
    );

    assert_eq!(label, "inner(int depth)");
}

/// A member call is the same question about the member's declaration, and the label is the *member's* spelling.
#[test]
fn a_member_call_reports_the_members_signature() {
    let (label, spans, _) = label_where_marked(
        "struct Widget { int scaled(int factor) const; };\nint f(Widget& w) { return w.scaled(|); }\n",
    );

    assert_eq!(label, "scaled(int factor)");
    assert_eq!(spans, vec!["int factor".to_string()]);
}

/// A callee in another file: the parameters are read from the header's own declaration, and the label says what
/// that declaration wrote.
#[test]
fn a_callee_in_another_file_reports_its_signature() {
    let header = "double scale(int count, double factor);\n";
    let source = "#include \"b.h\"\ndouble f() { return scale(|); }\n";
    let offset = source.find('|').expect("the fixture marks the cursor");
    let without_marker = source.replace('|', "");

    let session = session_with(&[("/p/a.cpp", &without_marker), ("/p/b.h", header)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let in_the_reading = view
        .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(
            offset,
        ))
        .expect("the cursor is in the rendering");
    let signature = session
        .signatures_at(&view, in_the_reading)
        .into_iter()
        .next()
        .expect("the header declares the callee");

    assert_eq!(signature.label, "scale(int count, double factor)");
    assert_eq!(signature.declared_in, std::path::Path::new("/p/b.h"));
}

/// **A call to an overloaded function answers every overload**, and that is the answer to "which one is this":
/// the reader picks, and the client cycles. The alternative this replaced was an empty popup — `std::format` is
/// four declarations, the name answered `Ambiguous`, and the signature was refused.
///
/// The order is the declarations' own, and each signature carries its **own** active parameter: the overloads of a
/// real function do not take the same number of parameters, so one count for the whole popup would highlight the
/// wrong one as soon as the reader switched.
#[test]
fn a_call_to_an_overloaded_function_answers_every_overload() {
    let source = "\
int scale(int count);
double scale(double factor, int places);
int f() { return scale(1, |); }
";
    let offset = source.find('|').expect("the fixture marks the cursor");
    let without_marker = source.replace('|', "");

    let session = session_with(&[("/p/a.cpp", &without_marker)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    // **The fixture's cursor is the file's; `signatures_at` reads the rendering** — see `signature_where_marked`:
    // a comment is removed before the parse, so the two offsets differ by everything the cook deleted.
    let in_the_reading = view
        .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(
            offset,
        ))
        .expect("the cursor is in the rendering");
    let signatures = session.signatures_at(&view, in_the_reading);

    let labels: Vec<&str> = signatures
        .iter()
        .map(|found| found.label.as_str())
        .collect();
    assert_eq!(
        labels,
        ["scale(int count)", "scale(double factor, int places)"],
        "both declarations, in the order they are written"
    );
    assert_eq!(
        signatures[0].active_parameter,
        Some(1),
        "the cursor is past one comma, which is the *second* parameter — and the first overload has no second one"
    );
    assert_eq!(
        signatures[1].active_parameter,
        Some(1),
        "the count is the call's, and each signature reports it against its own list"
    );
}

/// **A member call to an overloaded member answers every overload too** — `s.push_back(` is two declarations in
/// `basic_string`, and the member path had been left singular while the name path became plural.
///
/// The base's overloads count as well, which is why this goes through the member *list* rather than through the
/// one member a direct lookup picks: what a reader can call on a derived object includes what its bases declare.
#[test]
fn a_member_call_answers_every_overload_of_the_member() {
    let source = "\
struct Widget {
    void scaled(int factor);
    void scaled(double factor, int places);
    void grow();
};
int f(Widget& w) { return w.scaled(|); }
";
    let offset = source.find('|').expect("the fixture marks the cursor");
    let without_marker = source.replace('|', "");

    let session = session_with(&[("/p/a.cpp", &without_marker)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    // **The fixture's cursor is the file's; `signatures_at` reads the rendering** — see `signature_where_marked`:
    // a comment is removed before the parse, so the two offsets differ by everything the cook deleted.
    let in_the_reading = view
        .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(
            offset,
        ))
        .expect("the cursor is in the rendering");
    let signatures = session.signatures_at(&view, in_the_reading);

    let labels: Vec<&str> = signatures
        .iter()
        .map(|found| found.label.as_str())
        .collect();
    assert_eq!(
        labels,
        ["scaled(int factor)", "scaled(double factor, int places)"],
        "both member overloads, and not the one that is not called"
    );
}

/// **A member inherited from a base is a candidate too**, and its parameters come from the base's declaration.
#[test]
fn a_member_call_answers_an_inherited_overload() {
    let source = "\
struct Base { void scaled(int factor); };
struct Widget : Base { };
int f(Widget& w) { return w.scaled(|); }
";
    let offset = source.find('|').expect("the fixture marks the cursor");
    let without_marker = source.replace('|', "");

    let session = session_with(&[("/p/a.cpp", &without_marker)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    // **The fixture's cursor is the file's; `signatures_at` reads the rendering** — see `signature_where_marked`:
    // a comment is removed before the parse, so the two offsets differ by everything the cook deleted.
    let in_the_reading = view
        .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(
            offset,
        ))
        .expect("the cursor is in the rendering");
    let signatures = session.signatures_at(&view, in_the_reading);

    assert_eq!(
        signatures
            .iter()
            .map(|found| found.label.as_str())
            .collect::<Vec<_>>(),
        ["scaled(int factor)"],
        "the base declares it, and the derived class inherits it"
    );
}

/// **A declaration laid out over several lines is one line in the popup**, and the spans still point at the
/// parameters. Measured on MSVC's `<xstring>`: three of `basic_string::append`'s nine overloads are wrapped, so this
/// is the ordinary shape in the headers this analysis reads, not a corner. The tidying is the same rule the fact's
/// own `parameter_list` gets — one implementation — and the byte map is why it can be applied to a label at all.
#[test]
fn a_wrapped_parameter_list_is_one_line_and_the_spans_follow_it() {
    let (label, spans, active) = label_where_marked(
        "int scale(int count,\n          double factor);\nint f() { return scale(|); }\n",
    );

    assert_eq!(
        label, "scale(int count, double factor)",
        "the declaration's newline and indentation are the file's layout, not the signature"
    );
    assert_eq!(
        spans,
        vec!["int count".to_string(), "double factor".to_string()]
    );
    assert_eq!(active, Some(0));
}

/// A callee in another file with several overloads: each is read from the same header, and the header is parsed
/// once for the answer rather than once per overload.
#[test]
fn overloads_in_one_header_are_read_from_it_once() {
    let header = "int scale(int count);\ndouble scale(double factor, int places);\n";
    let source = "#include \"b.h\"\nint f() { return scale(|); }\n";
    let offset = source.find('|').expect("the fixture marks the cursor");
    let without_marker = source.replace('|', "");

    let session = session_with(&[("/p/a.cpp", &without_marker), ("/p/b.h", header)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    // **The fixture's cursor is the file's; `signatures_at` reads the rendering** — see `signature_where_marked`:
    // a comment is removed before the parse, so the two offsets differ by everything the cook deleted.
    let in_the_reading = view
        .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(
            offset,
        ))
        .expect("the cursor is in the rendering");
    let signatures = session.signatures_at(&view, in_the_reading);

    assert_eq!(signatures.len(), 2);
    assert!(
        signatures
            .iter()
            .all(|found| found.declared_in == std::path::Path::new("/p/b.h")),
        "both are the header's declarations"
    );
}

/// A callee nothing declares has no signature — the same refusal the parameter hints make, for the same reason.
#[test]
fn an_unresolvable_callee_has_no_signature() {
    let (_, signature) = signature_where_marked("int f() { return mystery(|); }\n");
    assert!(signature.is_none());
}

/// **A call of a class offers its constructors** — `Widget(|)` is a constructor call, and *which* overload it means
/// is overload resolution, which signature help does not do and must not pretend to do. It answers the way it answers
/// a name with several declarations: **every** constructor, in the order they are written, and the reader picks.
///
/// This test used to assert the opposite — `a_call_of_a_class_has_no_signature` — and the reasoning it recorded was
/// sound as far as it went: a class has no parameter list of its own, so there was nothing to show, and picking one
/// constructor by position would be inventing an answer. What it could not know was *why* there was nothing to show.
/// A constructor declaration inside a class body was parsed as a **variable** — measured, `Widget(int size);` came
/// out as `DeclSpecifierSeq > NamedType "Widget"` beside a nameless `Declarator` — so no constructor existed in the
/// tree at all, for this or for anything else. That is fixed in the parser (`parse_constructor_declaration`), and the
/// honest answer is now the one this test pins.
#[test]
fn a_call_of_a_class_offers_its_constructors() {
    let source = "\
struct Widget {
  Widget();
  Widget(int width, int height);
};
Widget f() { return Widget(|); }
";
    let offset = source.find('|').expect("the fixture marks the cursor");
    let without_marker = source.replace('|', "");

    let session = session_with(&[("/p/a.cpp", &without_marker)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let in_the_reading = view
        .reading_offset_of(cpp_code_analysis::file::coordinates::FileOffset::new(
            offset,
        ))
        .expect("the cursor is in the rendering");
    let signatures = session.signatures_at(&view, in_the_reading);

    let labels: Vec<&str> = signatures
        .iter()
        .map(|found| found.label.as_str())
        .collect();
    assert_eq!(
        labels,
        ["Widget()", "Widget(int width, int height)"],
        "every constructor the class declares, in the order they are written"
    );
}

/// The documentation above the declaration comes with the signature — the place the reader needs it, because the
/// parameter they are typing is the one the comment describes.
#[test]
fn the_declarations_documentation_travels_with_the_signature() {
    let (_, signature) = signature_where_marked(
        "/// Scales a count.\n/// @param count the count\nint scale(int count);\nint f() { return scale(|); }\n",
    );
    let signature = signature.expect("the callee resolves");
    let documentation = signature.documentation.expect("the file documents it");

    assert!(documentation.get_comment_text().contains("Scales a count."));
    assert!(documentation.get_comment_text().contains("@param count"));
}

/// A call with no arguments at all, and no position inside it, is answered rather than refused: `make()` with the
/// cursor between the parentheses is the first parameter.
#[test]
fn an_empty_argument_list_is_answered() {
    let (label, _, active) =
        label_where_marked("int make(int count);\nint f() { return make(|); }\n");

    assert_eq!(label, "make(int count)");
    assert_eq!(active, Some(0));
}
/// **A call written inside a macro's argument is still that call.**
///
/// `scale(2, |)` is an argument of `PICK(...)`, so the rendering the analysis parses is not the text the file writes:
/// the macro is expanded, and the call appears inside `pick_impl((1), (scale(2, )))` — present, but under a different
/// parent and at a different offset. The cursor has to cross that seam, and the question this holds is *which token
/// the file wrote at that position*: the mapping from a file offset into the rendering asked each span where it
/// **reports** (which for anything an expansion touches is the outermost call site) instead of where it was
/// **written**, so the cursor landed before `scale` rather than inside it, the call found was the enclosing
/// `pick_impl`, and the answer was no signature at all.
#[test]
fn a_call_inside_a_macros_argument_answers_its_own_signature() {
    let source = "\
#define PICK(first, second) pick_impl((first), (second))

/// Scales a count.
int scale(int count, double factor);
int pick_impl(int first, int second);

int f() {
    return PICK(1, scale(2, |));
}
";
    let (label, spans, active) = label_where_marked(source);

    assert!(
        label.contains("scale") && label.contains("double factor"),
        "the call inside the macro argument is the one asked about: {label:?}"
    );
    assert_eq!(spans.len(), 2, "two parameters: {spans:?}");
    assert_eq!(active, Some(1), "and the cursor is on the second");
}
