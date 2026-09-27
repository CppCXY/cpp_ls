//! Parameter names at the call sites — the inlay hint an editor draws inside the code.
//!
//! The hints are placed **by position**: the first argument gets the first parameter's name, and so on. That is
//! what most of these tests are about, because the interesting cases are the ones where the positions do *not*
//! line up — an unnamed parameter, a variadic tail, a call of a class rather than of a function, a callee nothing
//! can resolve. Each of those has to produce no hint rather than a plausible-looking wrong one: a hint is printed
//! in the middle of the user's code, where nothing tells them it was a guess.

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// A session over the given files, with all of them indexed so that a callee in another file resolves.
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

/// Every hint in one file, as `(offset, name)` pairs in the order they were produced.
fn hints_in(files: &[(&str, &str)], path: &str) -> Vec<(usize, String)> {
    let session = session_with(files);
    let view = session.view(path).expect("the file is held");
    let whole_file = cpp_parser::SourceRange::new(0, view.source.len());

    session
        .inlay_hints(&view, whole_file)
        .into_iter()
        .map(|hint| (hint.offset, hint.name))
        .collect()
}

/// The hints for the calls in a single file, with the named parameter each offset lands on.
fn hints_of(source: &str) -> Vec<(usize, String)> {
    hints_in(&[("/p/a.cpp", source)], "/p/a.cpp")
}

/// `scale(count: 3, factor: 0.5)` — the case the feature exists for.
#[test]
fn a_call_reports_the_parameters_of_its_callee() {
    let source = "\
double scale(int count, double factor);
double f() { return scale(3, 0.5); }
";
    let call = source.find("scale(3").expect("the fixture") + "scale(".len();

    assert_eq!(
        hints_of(source),
        vec![
            (call, "count".to_string()),
            (call + "3, ".len(), "factor".to_string()),
        ]
    );
}

/// A parameter list is not a call, and a hint goes at the **argument**: the declaration above writes
/// `int value`, and the one hint is the one at `other`, named after the parameter it lands in.
#[test]
fn only_the_arguments_of_a_call_are_hinted() {
    let source = "\
int twice(int value);
int f(int other) { return twice(other); }
";
    let argument = source.find("twice(other").expect("the fixture") + "twice(".len();

    assert_eq!(hints_of(source), vec![(argument, "value".to_string())]);
}

/// An argument that already spells the parameter needs no hint in front of it: the line says `count, factor`, and
/// `count: count, factor: factor` is the editor repeating the code back at the reader.
#[test]
fn an_argument_that_already_spells_the_parameter_is_not_hinted() {
    let source = "\
double scale(int count, double factor);
double f(int count, double factor) { return scale(count, factor); }
";
    assert!(hints_of(source).is_empty());
}

/// **A parameter with no name does not shift the ones after it.**
///
/// `void log(int, const char* message);` declares a second parameter named `message`, and the second *argument* is
/// where it belongs. A reading that dropped the unnamed parameter would put `message` against the first argument —
/// a wrong answer printed into the code, which is why the reading keeps the gap.
#[test]
fn an_unnamed_parameter_does_not_shift_the_others() {
    let source = "\
void log(int, const char* message);
void f() { log(1, \"hi\"); }
";
    let second = source.find("1, ").expect("the fixture") + "1, ".len();

    assert_eq!(
        hints_of(source),
        vec![(second, "message".to_string())],
        "the first parameter has no name, so only the second argument is hinted"
    );
}

/// A variadic tail is not a parameter, so the arguments past the named ones get nothing.
#[test]
fn a_variadic_tail_is_not_hinted() {
    let source = "\
void log(const char* format, ...);
void f() { log(\"%d %d\", 1, 2); }
";
    let format = source.find("\"%d %d\"").expect("the fixture");

    assert_eq!(
        hints_of(source),
        vec![(format, "format".to_string())],
        "the two integers have no parameter to be named after"
    );
}

/// A call through a member is the same question about the member's declaration.
#[test]
fn a_member_call_reports_the_members_parameters() {
    let source = "\
struct Widget {
    int size_of(int scale) const;
};
int f(Widget& w) { return w.size_of(3); }
";
    let argument = source.find("size_of(3").expect("the fixture") + "size_of(".len();

    assert_eq!(
        hints_of(source),
        vec![(argument, "scale".to_string())]
    );
}

/// **A call of a class is not a call of a function.**
///
/// `Widget(1, 2)` names a type, and a type's scope holds its *fields*, not the constructor's parameters — so a
/// hint here would label the arguments with whatever names the class happens to declare. The refusal is the scope
/// kind's: only a function scope has parameters.
#[test]
fn a_call_of_a_class_is_not_hinted() {
    let source = "\
struct Widget {
    int width;
    int height;
    Widget(int w, int h);
};
Widget make() { return Widget(3, 4); }
";
    assert!(hints_of(source).is_empty());
}

/// A callee this analysis cannot place is not guessed at: a function pointer's parameters are its type's, and this
/// layer does not read types that way.
#[test]
fn an_unresolvable_callee_is_not_hinted() {
    let source = "\
double f() { return unknown(1, 2); }
";
    assert!(hints_of(source).is_empty());
}

/// **A callee in another file.** The parameters are read from the header's own scope, which means the header is
/// parsed for the answer — and the index is what says the header declares the function at all.
#[test]
fn a_callee_in_another_file_reports_its_parameters() {
    let header = "double scale(int count, double factor);\n";
    let source = "#include \"b.h\"\ndouble f() { return scale(3, 0.5); }\n";
    let call = source.find("scale(3").expect("the fixture") + "scale(".len();

    assert_eq!(
        hints_in(&[("/p/a.cpp", source), ("/p/b.h", header)], "/p/a.cpp"),
        vec![
            (call, "count".to_string()),
            (call + "3, ".len(), "factor".to_string()),
        ]
    );
}

/// Two calls into the same header are one parse of the header and two sets of hints — a hint is about the call,
/// not about the declaration.
#[test]
fn every_call_is_hinted_separately() {
    let header = "double scale(int count, double factor);\n";
    let source = "\
#include \"b.h\"
double f() { return scale(3, 0.5); }
double g() { return scale(4, 1.5); }
";
    let hints = hints_in(&[("/p/a.cpp", source), ("/p/b.h", header)], "/p/a.cpp");

    assert_eq!(hints.len(), 4, "{hints:?}");
    assert_eq!(hints[0].1, "count");
    assert_eq!(hints[1].1, "factor");
    assert_eq!(hints[2].1, "count");
    assert_eq!(hints[3].1, "factor");
}

/// The range is the client's visible area: a hint outside it is work the client throws away, so an argument below
/// the range is not hinted even though its call starts inside it.
#[test]
fn only_arguments_inside_the_range_are_hinted() {
    let source = "\
double scale(int count, double factor);
double f() { return scale(3, 0.5); }
";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");
    let first = source.find("scale(3").expect("the fixture") + "scale(".len();

    // A range that ends inside the first argument.
    let partial = cpp_parser::SourceRange::new(0, first + 1);
    let hints = session.inlay_hints(&view, partial);

    assert_eq!(hints.len(), 1, "{hints:?}");
    assert_eq!(hints[0].name, "count");
}

/// **A call of a variable is not a call of a function**, even inside a function whose own parameters are right
/// there in the file. `x(1)` where `x` is an `int` is nonsense C++, and it is the shape that matters: the
/// enclosing function's parameter list is the nearest one up the tree, and reading *that* would label the
/// argument `y` — a name from a different declaration, printed into the code.
#[test]
fn a_call_of_a_variable_is_not_hinted() {
    let source = "\
int f(int y) {
    int x = 0;
    return x(1);
}
";
    assert!(hints_of(source).is_empty(), "{:?}", hints_of(source));
}

/// A lambda held in a variable has parameters of its own, and they are not read: `g`'s declaration has no
/// parameter list — the list belongs to the lambda *expression*, which is a different construct — so there is
/// nothing a hint could be named after.
#[test]
fn a_call_of_a_lambda_variable_is_not_hinted() {
    let source = "\
int f() {
    auto g = [](int a) { return a; };
    return g(1);
}
";
    assert!(hints_of(source).is_empty(), "{:?}", hints_of(source));
}

/// **A member declared in a class and called through `this`.** The declaration is in the class body, the call is
/// in a member function, and the parameter names come from the declaration's own declarator — which is also the
/// path a header's member is reached by.
#[test]
fn a_member_called_through_this_is_hinted() {
    let source = "\
struct S {
    int other(int c) const;
    int go() { return this->other(2); }
};
";
    let argument = source.find("other(2").expect("the fixture") + "other(".len();

    assert_eq!(hints_of(source), vec![(argument, "c".to_string())]);
}

/// **A boundary this feature inherits.** `void (*f(int a))(int b);` declares a function, and the scope model does
/// not bind `f`: the declarator is parenthesized, and `declared_name` walks the declarator-to-name path, which
/// this shape leaves. So the call is unresolved, and an unresolved call gets no hint rather than a guess.
///
/// Pinned as a claim about the **resolution** rather than as "no hint", because which layer refuses is the
/// interesting part: the day the binding exists, the reading already takes the declarator the name is a name of —
/// the one whose list says `a`, not the list of the function it returns.
#[test]
fn a_declaration_the_scope_model_does_not_bind_is_not_hinted() {
    let source = "void (*f(int a))(int b);\nvoid g() { f(1); }\n";
    let session = session_with(&[("/p/a.cpp", source)]);
    let view = session.view("/p/a.cpp").expect("the file is held");

    assert!(
        matches!(
            session.definition(&view, source.rfind("f(1)").expect("the call")),
            cpp_code_analysis::Known::Unknown(_)
        ),
        "this declaration shape is not bound, so nothing resolves the call"
    );
    assert!(hints_of(source).is_empty());
}

/// **A call through a function pointer is not hinted, and the parameters of its type are not read.**
///
/// `cb` is a parameter whose *type* is a function with a parameter `a`, and `cb(1)` passes `1` to that `a`. The
/// reading refuses anyway, deliberately: the parameter list belongs to the declarator the *name* is a name of, and
/// `cb`'s own declarator has none — the list is one level out, in the type. That is the boundary
/// `type_of_a_call` already has ("a call of a function pointer, of a lambda, of a template parameter" stays
/// `Unknown`), and reading through it here would make this feature the one place that trusts a type's shape.
///
/// It is also the case that tells this reading apart from "the nearest parameter list up the tree", which would
/// find `a` and hint it.
#[test]
fn a_call_through_a_function_pointer_is_not_hinted() {
    let source = "\
void h(int (*cb)(int a)) {
    cb(1);
}
";
    assert!(hints_of(source).is_empty(), "{:?}", hints_of(source));
}

/// A call whose argument list is empty has nothing to place a name at, which is not an error and not a hint.
#[test]
fn a_call_with_no_arguments_has_no_hints() {
    assert!(hints_of("int f();\nint g() { return f(); }\n").is_empty());
}
