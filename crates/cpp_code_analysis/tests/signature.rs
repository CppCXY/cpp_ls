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
    session.add_project_files(files.iter().map(|(path, _)| std::path::PathBuf::from(*path)));
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

    (without_marker, session.signatures_at(&view, offset).into_iter().next())
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
    assert_eq!(spans, vec!["int count".to_string(), "double factor".to_string()]);
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
        let source = format!("double scale(int count, double factor);\ndouble f() {{ return {call}; }}\n");
        let (_, _, active) = label_where_marked(&source);
        assert_eq!(active, Some(expected), "in `{call}`");
    }
}

/// **A cursor on the callee is not a call being typed.** It is a question about the name — which is hover's and
/// definition's business — and a signature popup there would appear while the reader is still choosing the
/// function.
#[test]
fn a_cursor_on_the_callee_has_no_signature() {
    let (_, signature) = signature_where_marked(
        "double scale(int count);\ndouble f() { return sca|le(1); }\n",
    );

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
    let signature = session
        .signatures_at(&view, offset)
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
    let signatures = session.signatures_at(&view, offset);

    let labels: Vec<&str> = signatures.iter().map(|found| found.label.as_str()).collect();
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
    let signatures = session.signatures_at(&view, offset);

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

/// **A call of a class is not a signature**: `Widget(|)` names a type, and which constructor it means is overload
/// resolution. The class's own declaration has no parameter list, so there is nothing to show — which is the
/// honest answer rather than the constructor's parameters picked by position.
#[test]
fn a_call_of_a_class_has_no_signature() {
    let (_, signature) = signature_where_marked(
        "struct Widget { Widget(int width, int height); };\nWidget f() { return Widget(|); }\n",
    );

    assert!(signature.is_none());
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
