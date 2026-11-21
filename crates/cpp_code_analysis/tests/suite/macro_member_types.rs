//! **A member reached through a macro** — `MACRO(x)->member`, which is Lua 5.5.0's `ci_func(ci)->p`.
//!
//! The file's own text cannot type it: `ci_func` is a macro, so the object of the arrow is a call to a name nothing
//! declares, and the honest answer from that reading is "not known". The **rendering** has the expansion, and the chain
//! types there — which is the answer a reader wants, and the one the hover and `type` now give.
//!
//! Two things have to hold for it, and both are asserted here:
//!
//! * the expansion types (it did not until the depth bound was raised: the chain needs ten levels, because every
//!   parenthesis a macro adds is one — see `MAX_TYPE_DEPTH`), and
//! * a written expression is found in the rendering **by its span**, not by its first byte: the first byte of an
//!   expansion is a `(`, and what contains it is the whole `->p`, which is the *member*'s type rather than the
//!   object's.

use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::{
    CompilerConfig, Known, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

/// The shape of Lua 5.5.0's `ci_func`: a macro whose expansion is a pointer, and a member access on top of it.
const SOURCE: &str = "\
struct Proto { int nup; };
struct GCObject { int x; };
typedef union Value { struct GCObject *gc; void *p; } Value;
struct TValue { Value value_; unsigned char tt_; };
struct LClosure { struct Proto *p; struct GCObject *gclist; };
typedef union Closure { struct LClosure l; struct GCObject *gc; } Closure;
union GCUnion { struct GCObject *gc; Closure cl; };
struct StackValue { struct TValue val; };
typedef struct StackValue *StkId;
struct CallInfo { union { StkId p; long offset; } func; };
#define ci_func(ci) (&(cast(union GCUnion *, ((&((ci)->func.p)->val))->value_.gc))->cl.l)
#define cast(t, exp) ((t)(exp))
int f(struct CallInfo *ci) { return ci_func(ci)->p->nup; }
";

fn session_with(source: &str) -> Session<MemoryFiles> {
    let memory = MemoryFiles::new().with_file("/p/a.c", source);
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
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
    session
}

/// The type `type_at` answers for the innermost expression at the **last** byte of `needle` — the step's own result,
/// rather than the first thing at its first byte.
fn the_type_at_the_end_of(
    session: &Session<MemoryFiles>,
    needle: &str,
) -> Known<cpp_code_analysis::ExpressionType> {
    let view = session.view("/p/a.c").expect("the file has a rendering");
    let at = SOURCE
        .rfind(needle)
        .unwrap_or_else(|| panic!("{needle} is written"))
        + needle.len()
        - 1;
    let reading = view
        .reading_offset_of(FileOffset::new(at))
        .expect("the cursor is in the rendering");
    session.type_at(&view, reading)
}

#[test]
fn a_member_of_a_macro_call_types_through_the_rendering() {
    let session = session_with(SOURCE);

    // The member of the expansion, which is the whole point: `Proto *`, declared in `LClosure`.
    let member = the_type_at_the_end_of(&session, "->p->nup");
    match member {
        Known::Yes(found) => assert_eq!(found.type_of, "int", "{}", found.expression),
        other => panic!("`ci_func(ci)->p->nup` types: {other:?}"),
    }

    let pointer = the_type_at_the_end_of(&session, "->p");
    match pointer {
        Known::Yes(found) => assert_eq!(found.type_of, "Proto*", "{}", found.expression),
        other => panic!("`ci_func(ci)->p` types: {other:?}"),
    }
}
