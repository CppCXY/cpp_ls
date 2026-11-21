//! **A C declarator's parameter list**, read into the facts a hover, an inlay hint and a signature all use.
//!
//! Reported from an editor on Lua 5.5.0: hovering `lua_setfield` printed `lua_setfield(…)` and no inlay hint was
//! placed, because the header writes
//!
//! ```c
//! LUA_API void  (lua_setfield) (lua_State *L, int idx, const char *k);
//! ```
//!
//! — a **parenthesised declarator**, whose `ParameterList` is a *sibling* of the declarator in the tree. The fact read
//! as a function with no parameters, and its parameters were filed as file-scope variables.
//!
//! The function-pointer variable beside it has the same node shape and must not gain a list: `void (*bar)(int a);` is
//! a variable of type `void(int)*`, and `(int a)` belongs to the pointed-to type.

use cpp_code_analysis::{
    CompilerConfig, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// The facts of one source file, as the index files them.
fn facts_of(source: &str) -> Vec<cpp_code_analysis::DeclFact> {
    let files = MemoryFiles::new().with_file("/p/a.c", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.c")]);
    session.want_cooked_reading(std::path::Path::new("/p/a.c"));
    session.index_everything();
    session
        .index()
        .summary(std::path::Path::new("/p/a.c"))
        .map(|summary| summary.declarations.to_vec())
        .unwrap_or_default()
}

/// **A parenthesised declarator is a function with its parameters**, and a function pointer is a variable without.
#[test]
fn a_parenthesised_declarator_keeps_its_parameters() {
    let facts = facts_of(
        "\
void (foo) (int a, char *b);
void (*bar)(int a);
void  plain (int a, char *b);
",
    );

    let foo = facts
        .iter()
        .find(|fact| fact.name == "foo")
        .expect("foo is declared");
    assert_eq!(foo.kind, cpp_code_analysis::DeclKind::Function);
    assert_eq!(
        foo.parameter_list(),
        Some("(int a, char *b)"),
        "the list is beside the declarator, and it is the function's"
    );

    let bar = facts
        .iter()
        .find(|fact| fact.name == "bar")
        .expect("bar is declared");
    assert_eq!(
        bar.kind,
        cpp_code_analysis::DeclKind::Variable,
        "a function pointer variable declares a variable"
    );
    assert_eq!(
        bar.parameter_list(),
        None,
        "and its parameter list belongs to the pointed-to type"
    );

    let plain = facts
        .iter()
        .find(|fact| fact.name == "plain")
        .expect("plain is declared");
    assert_eq!(plain.parameter_list(), Some("(int a, char *b)"));
}

/// **The parameters of that spelling are the function's, not the file's** — a parameter is a local of the function's
/// scope, and a reading that never found the list filed them at file scope (where a name resolves to them from
/// anywhere, and a rename edits every use).
#[test]
fn a_parenthesised_declarators_parameters_are_locals() {
    use cpp_code_analysis::Known;
    use cpp_code_analysis::file::coordinates::ReadingOffset;

    let source = "void (foo) (int a, char *b);\n";
    let files = MemoryFiles::new().with_file("/p/a.c", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.c")]);
    session.index_everything();
    session.load(std::path::Path::new("/p/a.c"));
    let held = session
        .files()
        .held(std::path::Path::new("/p/a.c"))
        .expect("the file is readable")
        .clone();
    let view =
        cpp_code_analysis::FileView::parse_with(&held, &cpp_parser::MacroEnvironment::default());

    let at = source.find("int a").expect("the parameter") + "int ".len();
    match session.definitions(&view, ReadingOffset::new(at)) {
        Known::Yes(found) => {
            let parameter = &found.found[0].fact;
            assert_eq!(parameter.name, "a");
            assert!(
                parameter.local,
                "a parameter is a local of the function's scope: {parameter:?}"
            );
        }
        other => panic!("the parameter resolves: {other:?}"),
    }
}

/// **A `typedef` of a pointer is followed for a member access** — `typedef struct StackValue *StkId;`.
///
/// Lua 5.5.0 declares that alias and writes its whole stack API with one, so `s->val` asked the **alias** for its
/// members and answered `NotDeclaredHere("StkId::val")`; no `->` chain that started at a `StkId` could be followed,
/// including the one inside the expansion of `ci_func(ci)`. The alias walk in `resolve_aliases_or_unresolved` read the
/// target as a *type* and required a `class_name`, which a pointer type does not have (`StackValue *`) — so the walk
/// broke and returned the alias unchanged. The pointee's class name is the answer, which is the rule the rest of the
/// layer already applies to a written `T *`.
#[test]
fn a_typedef_of_a_pointer_is_followed_for_a_member() {
    use cpp_code_analysis::file::coordinates::ReadingOffset;

    let source = "\
struct StackValue { int val; };
typedef struct StackValue *StkId;
struct CallInfo { union { StkId p; long offset; } func; };
int f(struct CallInfo *ci) { return ci->func.p->val; }
int g(StkId s) { return s->val; }
";
    let files = MemoryFiles::new().with_file("/p/a.c", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.c")]);
    session.want_cooked_reading(std::path::Path::new("/p/a.c"));
    session.index_everything();
    session.load(std::path::Path::new("/p/a.c"));
    let held = session
        .files()
        .held(std::path::Path::new("/p/a.c"))
        .expect("the file is readable")
        .clone();
    let view =
        cpp_code_analysis::FileView::parse_with(&held, &cpp_parser::MacroEnvironment::default());

    // The `val` at the end of each chain, and what the chain says it is.
    for (needle, expected) in [("ci->func.p->val", "int"), ("return s->val", "int")] {
        let at = source.find(needle).expect("the chain") + needle.find("val").expect("the member");
        let found = session.type_at(&view, ReadingOffset::new(at));
        match found {
            cpp_code_analysis::Known::Yes(found) => assert_eq!(found.type_of, expected),
            other => panic!("{needle}: {other:?}"),
        }
    }
}

/// **An elaborated-type-specifier is not part of the class's name** — `(union GCUnion *)…->cl`.
///
/// A C cast spells its type that way, and every one of Lua 5.5.0's macros is written that way. `members_of("union
/// GCUnion")` answered its 8 members while the member *fact* query asked for `union GCUnion::cl` and came back
/// `NotDeclaredHere` — the member-list path normalised the keyword and the single-member path did not, so the chain
/// inside `ci_func(ci)`'s expansion stopped at `->cl` and `->p` after it never ran.
#[test]
fn a_member_of_an_elaborated_cast_is_found() {
    use cpp_code_analysis::file::coordinates::ReadingOffset;

    let source = "\
struct Proto { int nup; };
union Closure { struct LClosure { struct Proto *p; int n; } l; };
union GCUnion { void *gc; union Closure cl; };
int f(void *object) {
    return ((union GCUnion *)object)->cl.l.p->nup;
}
";
    let files = MemoryFiles::new().with_file("/p/a.c", source);
    let providers = SessionFiles::new(OpenDocuments::new(), files);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files([std::path::PathBuf::from("/p/a.c")]);
    session.want_cooked_reading(std::path::Path::new("/p/a.c"));
    session.index_everything();
    session.load(std::path::Path::new("/p/a.c"));
    let held = session
        .files()
        .held(std::path::Path::new("/p/a.c"))
        .expect("the file is readable")
        .clone();
    let view =
        cpp_code_analysis::FileView::parse_with(&held, &cpp_parser::MacroEnvironment::default());

    let at = source.find("nup;").expect("the member") + 1;
    match session.type_at(&view, ReadingOffset::new(at)) {
        cpp_code_analysis::Known::Yes(found) => assert_eq!(found.type_of, "int"),
        other => panic!("`->nup` through an elaborated cast: {other:?}"),
    }
}
