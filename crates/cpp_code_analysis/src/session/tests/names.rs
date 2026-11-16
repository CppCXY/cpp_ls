//! **Names** — names, includes, macros, and the memory budget

use crate::file::coordinates::ReadingOffset;
use crate::file::paths::MemoryFiles;
use crate::symbol::Known;
use std::path::{Path, PathBuf};

use super::*;

/// **A module's closure is what an importer sees** — the visibility rule `import std;` needed.
///
/// A module is not a header: what an importer compiles is the unit's **whole translation unit**, so a file the unit's
/// rendering entered is visible to the importer even when walking the interface's `#include` edges would drop it (each
/// guard on those edges is evaluated against a state that is not the unit's). Measured on a real project: `report.cppm`
/// importing `std` saw **151 files and no `std::string`**, while `main.cpp` saw 173 only because its own `#include`
/// pulled the same tree in textually.
///
/// The closure is recorded where the rendering is in hand (`ProjectIndex::remember_the_unit_closure`) and consulted by
/// the visibility walk for the asking file and for every file its imports reach.
#[test]
pub(crate) fn a_modules_closure_is_visible_to_a_file_that_imports_it() {
    let interface = "export module m;\nexport int given();\n";
    let implementation = "namespace hidden {\n    struct Widget { int weight; };\n}\n";
    let importer = "import m;\nvoid f() {}\n";
    let files = MemoryFiles::new()
        .with_file("/p/m.ixx", interface)
        .with_file("/p/hidden.cpp", implementation)
        .with_file("/p/user.cpp", importer);
    let fixture = Memory::new("a-modules-closure-is-visible", &files);
    let mut session = fixture.session();
    session.add_project_files([
        PathBuf::from("/p/m.ixx"),
        PathBuf::from("/p/hidden.cpp"),
        PathBuf::from("/p/user.cpp"),
    ]);
    session.index_everything();

    // **Nothing yet**: the walk knows only the interface, because no unit has been rendered.
    let before = session.index().visible_files(Path::new("/p/user.cpp"));
    assert!(
        !before.iter().any(|(path, _)| path.ends_with("hidden.cpp")),
        "the interface's own closure is empty until a unit is read: {before:?}"
    );

    // **The rendering's own answer**, recorded the way the session records it.
    session.index().remember_the_unit_closure(
        "m",
        vec!["/p/hidden.cpp".to_string(), "/p/m.ixx".to_string()],
    );
    let after = session.index().visible_files(Path::new("/p/user.cpp"));
    assert!(
        after.iter().any(|(path, _)| path.ends_with("hidden.cpp")),
        "a file the unit was rendered from is visible to its importer: {after:?}"
    );
    // **And its declarations answer**, which is the point of being visible.
    assert!(
        matches!(
            session
                .index()
                .definition("Widget", Path::new("/p/user.cpp")),
            Known::Yes(_)
        ),
        "the class the unit saw is nameable from the importer"
    );
}

/// **A type this file uses and does not include is reported, with the header it came from** — `missing-include`.
///
/// The reader's shape: `Widget` is declared in `widget.h`, and `widget.cpp` uses it without including it — the program
/// compiles only because another translation unit's include order pulled the header in first. The finding names the
/// header the code action will add.
///
/// **And the standard library is never named**, which is the other half of the rule and the reason it exists at all:
/// suggesting `<string>` would need the standard library's whole index. The exclusion is mechanical — a candidate must
/// be under the workspace root — so a header outside it is invisible to this rule without a single name being listed.
#[test]
pub(crate) fn a_type_used_without_its_include_is_reported_and_std_is_not() {
    let header = "struct Widget { int size; };\n";
    let source = "void grow(Widget* w) { w->size = 1; }\n";
    let files = MemoryFiles::new()
        .with_file("/p/widget.h", header)
        .with_file("/p/widget.cpp", source)
        // A header **outside** the workspace: the standard library's, or a vendored tree's.
        .with_file("/opt/sdk/strings.h", "struct String { int size; };\n")
        // A file that reaches the SDK header, so the index knows the type — and a **different** file that uses it
        // without including it.
        .with_file(
            "/p/other.cpp",
            "#include \"/opt/sdk/strings.h\"\nvoid g(String* s) { s->size = 2; }\n",
        )
        .with_file("/p/uses_string.cpp", "void f(String* s) { s->size = 1; }\n");
    let fixture = Memory::new("missing-include", &files);
    let mut session = fixture.session();
    session.add_project_files([
        PathBuf::from("/p/widget.h"),
        PathBuf::from("/p/widget.cpp"),
        PathBuf::from("/p/other.cpp"),
        PathBuf::from("/p/uses_string.cpp"),
    ]);
    session.want_cooked_reading(Path::new("/p/widget.cpp"));
    session.want_cooked_reading(Path::new("/p/uses_string.cpp"));
    session.index_everything();

    let found = session
        .diagnostics("/p/widget.cpp")
        .expect("the file reads")
        .checks;
    let missing: Vec<&crate::sema::check::Finding> = found
        .iter()
        .filter(|finding| finding.code == crate::sema::check::DiagnosticCode::MissingInclude)
        .collect();
    assert!(
        missing
            .iter()
            .any(|finding| finding.name == "Widget" && finding.message.contains("widget.h")),
        "the header that declares `Widget` is named: {found:?}"
    );

    // **Nothing about the toolchain's own headers** — a type declared outside the workspace is not this rule's to
    // suggest.
    let outside = session
        .diagnostics("/p/uses_string.cpp")
        .expect("the file reads")
        .checks;
    assert!(
        !outside
            .iter()
            .any(|finding| finding.code == crate::sema::check::DiagnosticCode::MissingInclude),
        "`String` is declared outside the workspace, so no include is suggested: {outside:?}"
    );
}

/// **A name declared in a header beside the file is offered with its `#include`** — the completion half of
/// `missing-include`.
///
/// The reader types `Wid` in `use.cpp`, `Widget` lives in `widget.h` next to it, and the file does not include it. The
/// item is offered (it is strongly related) **and carries the edit that makes it usable**: accepting it must produce
/// code that compiles, so the `#include` goes with the name in one accept.
#[test]
pub(crate) fn a_name_from_a_nearby_header_is_offered_with_its_include() {
    let header = "struct Widget { int size; };\n";
    let source = "void f() {\n    Wid\n}\n";
    let files = MemoryFiles::new()
        .with_file("/p/widget.h", header)
        .with_file("/p/use.cpp", source);
    let fixture = Memory::new("unincluded-completion", &files);
    let mut session = fixture.session();
    session.did_open("/p/use.cpp", source);
    session.add_project_files([PathBuf::from("/p/widget.h"), PathBuf::from("/p/use.cpp")]);
    session.index_everything();

    let path = PathBuf::from("/p/use.cpp");
    let view = session.view_of_the_file(&path).expect("a view");
    let at = source.find("Wid").expect("the fixture") + "Wid".len();
    let offered = session.completions(&view, crate::file::coordinates::FileOffset::new(at));
    let item = offered
        .items
        .iter()
        .find(|item| item.label == "Widget")
        .unwrap_or_else(|| {
            panic!(
                "`Widget` is offered from the header beside the file: {:?}",
                offered
                    .items
                    .iter()
                    .map(|item| &item.label)
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(
        item.inclusion,
        Some(crate::completion::Inclusion::Include(
            "widget.h".to_string()
        )),
        "the spelling is the one a compiler searches"
    );
    let (range, text) = item
        .additional_edits
        .first()
        .expect("the include is carried as an edit");
    assert_eq!(text, "#include \"widget.h\"\n");
    assert_eq!(
        range.start_offset, 0,
        "there is no include block, so the edit goes at the top"
    );
}

/// **A forward declaration does not shadow the definition.**
///
/// MSVC's standard library writes both — `struct _Ty;` near the top of a header, `struct _Ty { … };` further down, and
/// the same name declared in two headers — and the index had no way to tell them apart. A query that took the first
/// answer sent the reader to an empty class, and every member query built on it answered "that class has nothing in
/// it" about a class the index holds in full.
#[test]
pub(crate) fn a_forward_declaration_does_not_shadow_the_definition() {
    let header = "struct Widget;\n\nstruct Widget {\n    int size;\n};\n";
    let source = "#include \"widget.h\"\nvoid f(Widget* widget) { (void)widget; }\n";
    let files = MemoryFiles::new()
        .with_file("/p/widget.h", header)
        .with_file("/p/use.cpp", source);
    let fixture = Memory::new("forward-declaration", &files);
    let mut session = fixture.session();
    session.did_open("/p/use.cpp", source);
    session.add_project_files([PathBuf::from("/p/widget.h"), PathBuf::from("/p/use.cpp")]);
    session.index_everything();

    let definition = header.find("struct Widget {").expect("the fixture");
    match session
        .index()
        .definition("Widget", Path::new("/p/use.cpp"))
    {
        Known::Yes(found) => {
            assert_eq!(
                found.fact.range.start_offset, definition,
                "the answer is the definition, not the forward declaration written above it"
            );
            assert!(found.fact.definition, "and the fact says so");
        }
        other => panic!("`Widget` is defined in the header: {other:?}"),
    }

    // **Across files too** — a forward declaration in one header, the definition in another.
    let forward = "struct Gadget;\n";
    let body = "struct Gadget {\n    int size;\n};\n";
    let files = MemoryFiles::new()
        .with_file("/p/gadget_forward.h", forward)
        .with_file("/p/gadget.h", body)
        .with_file(
            "/p/use.cpp",
            "#include \"gadget_forward.h\"\n#include \"gadget.h\"\n",
        );
    let fixture = Memory::new("forward-declaration-across-files", &files);
    let mut session = fixture.session();
    session.add_project_files([
        PathBuf::from("/p/gadget_forward.h"),
        PathBuf::from("/p/gadget.h"),
        PathBuf::from("/p/use.cpp"),
    ]);
    session.index_everything();
    match session
        .index()
        .definition("Gadget", Path::new("/p/use.cpp"))
    {
        Known::Yes(found) => assert!(
            found.file.ends_with("gadget.h") && !found.file.ends_with("gadget_forward.h"),
            "the definition wins over the forward declaration in another file: {:?}",
            found.file
        ),
        other => panic!("`Gadget` is defined in a header this file includes: {other:?}"),
    }
}

/// **`std::vector` resolves to its primary definition, not to `Ambiguous`.**
///
/// The reader's report was that a hover on `std::vector` in a file that imports `std` answered nothing at all. The
/// index holds four candidates for that spelling — two forward declarations, the primary template's definition, and
/// `vector<bool, _Alloc>` — and every one of them is a fact with the same qualified name. Two rules now choose:
/// the *entity* the spelling names (no pattern, when the spelling writes no template arguments), and a **definition**
/// over a forward declaration inside that group.
#[test]
pub(crate) fn a_class_template_resolves_to_its_primary_definition() {
    let forward = "template <class _Ty, class _Alloc>\nclass vector;\n";
    let defining = "template <class _Ty, class _Alloc>\nclass vector {\npublic:\n    void push_back(const _Ty&);\n};\n\
                    \ntemplate <class _Alloc>\nclass vector<bool, _Alloc> {\npublic:\n    void flip();\n};\n";
    let source =
        "#include \"forward.h\"\n#include \"vector.h\"\nvoid f(std::vector<int>* v) { (void)v; }\n";
    let files = MemoryFiles::new()
        .with_file("/p/forward.h", forward)
        .with_file("/p/vector.h", defining)
        .with_file("/p/use.cpp", source);
    let fixture = Memory::new("primary-template", &files);
    let mut session = fixture.session();
    session.did_open("/p/use.cpp", source);
    session.add_project_files([
        PathBuf::from("/p/forward.h"),
        PathBuf::from("/p/vector.h"),
        PathBuf::from("/p/use.cpp"),
    ]);
    session.index_everything();

    let primary = defining.find("class vector {").expect("the fixture");
    match session
        .index()
        .definition("vector", Path::new("/p/use.cpp"))
    {
        Known::Yes(found) => {
            assert!(
                found.fact.definition,
                "a definition, not a forward declaration"
            );
            assert_eq!(
                found.fact.pattern, None,
                "the primary template, not `vector<bool>`"
            );
            assert_eq!(
                found.fact.range.start_offset, primary,
                "and its own declaration"
            );
        }
        other => panic!("`vector` has a primary definition in this closure: {other:?}"),
    }
}

/// **A class whose name is also its constructor's name is still a class** — the report from use, as a test.
///
/// `LSPHandle::RefreshDiagnostic()` in `LSPHandle.cpp`: the qualifier `LSPHandle` names the class *and* its
/// constructor, so a name lookup answers `Ambiguous` — and the member list then offered the constructor and nothing
/// else (or nothing at all), which an editor shows as "no completion after `LSPHandle::`". The same shape already broke
/// a nested class once (`project.rs`'s note on `generator<T>::iterator`: "a completion on such a class offered its
/// constructor and nothing else"); this is the case a reader meets most often, because a class and its constructor
/// always share a name.
///
/// **The declaration is in the header and the use is in the source**, which is the shape the report is about: the
/// answer comes through the index rather than from one file's own scopes.
#[test]
pub(crate) fn a_class_whose_name_is_also_its_constructor_still_lists_its_members() {
    const HEADER: &str = "#pragma once\n\
                          class LSPHandle {\n\
                          public:\n\
                          \x20   LSPHandle(int server);\n\
                          \x20   void RefreshDiagnostic();\n\
                          \x20   int connected_;\n\
                          };\n";
    // **The constructor is defined here too** — and that is the whole of the trigger: its name *is* `LSPHandle`, so
    // the file itself declares the class's name, and the local-win rule then dropped every header fact with that name.
    const SOURCE: &str = "#include \"LSPHandle.h\"\n\
                          LSPHandle::LSPHandle(int server) : connected_(server) {\n\
                          }\n\
                          void LSPHandle::RefreshDiagnostic() {\n\
                          \x20   LSPHandle::RefreshDiagnostic();\n\
                          }\n";
    let files = MemoryFiles::new()
        .with_file("/p/LSPHandle.h", HEADER)
        .with_file("/p/LSPHandle.cpp", SOURCE);
    let fixture = Memory::new("a-class-named-like-its-constructor", &files);
    let mut session = fixture.session();
    session.did_open("/p/LSPHandle.h", HEADER);
    session.did_open("/p/LSPHandle.cpp", SOURCE);
    session.index_everything();

    let view = session
        .view_of_the_file(std::path::Path::new("/p/LSPHandle.cpp"))
        .expect("a view of the source");
    let at = SOURCE
        .rfind("LSPHandle::RefreshDiagnostic")
        .expect("the qualified call")
        + "LSPHandle::".len();
    let set = session.completions(&view, crate::file::coordinates::FileOffset::new(at));
    let mut names: Vec<&str> = set.items.iter().map(|item| item.label.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    assert!(
        names.contains(&"RefreshDiagnostic"),
        "`LSPHandle::` from a fresh index must offer the class's members; got {} item(s): {:?}",
        names.len(),
        names.iter().take(12).collect::<Vec<_>>()
    );

    // **What the analysis thinks the name *is*** — the reader's own words are "LSPHandle was read as the constructor
    // itself rather than the class", so the type of the qualifier is the assertion that says which.
    let qualifier = SOURCE
        .rfind("LSPHandle::RefreshDiagnostic")
        .expect("the qualified call")
        + 3;
    let typed = session.type_at(
        &view,
        crate::file::coordinates::ReadingOffset::new(qualifier),
    );
    let shown = format!("{typed:?}");
    assert!(
        !shown.contains("LSPHandle(int") && !shown.contains("LSPHandle(int)"),
        "the qualifier was read as the constructor, not the class: {shown}"
    );

    // **And after one keystroke**, which is the only state an editor asks the question in.
    let edited = format!("{SOURCE}// typed\n");
    session.did_change("/p/LSPHandle.cpp", &edited);
    session.catch_up(std::path::Path::new("/p/LSPHandle.cpp"));
    let view = session
        .view_of_the_file(std::path::Path::new("/p/LSPHandle.cpp"))
        .expect("a view of the edited source");
    let set = session.completions(&view, crate::file::coordinates::FileOffset::new(at));
    let names: Vec<&str> = set.items.iter().map(|item| item.label.as_str()).collect();
    assert!(
        names.contains(&"RefreshDiagnostic"),
        "one keystroke later `LSPHandle::` must still offer the class's members; got {:?}",
        names.iter().take(12).collect::<Vec<_>>()
    );
}

/// **A macro whose region cannot be decided still has a value when every definition in force agrees.**
///
/// Reported from `EmmyLuaCodeStyle`: `const int FIRST_RESERVED = UCHAR_MAX + 1;` showed no value. `UCHAR_MAX` is
/// defined inside a conditional in the toolchain's `limits.h`, and the *strict* question ("which `#define` is in
/// force") answers `Unknown(ConditionalCompilation)` — right for a jump, useless for a number. All the candidates
/// fold to the same one here, so the number is the compilation's whatever the preprocessor took; when they disagree
/// the answer stays `None`, because a wrong number is worse than none.
#[test]
pub(crate) fn a_conditional_macros_value_is_folded_when_every_definition_agrees() {
    let agreeing = "#ifdef WIDE\n\
                    #define LIMIT 0x10\n\
                    #else\n\
                    #define LIMIT 0x10\n\
                    #endif\n\
                    const int VALUE = LIMIT * 2;\n";
    let files = MemoryFiles::new().with_file("/p/agree.cpp", agreeing);
    let fixture = Memory::new("a-conditional-macro-that-agrees", &files);
    let mut session = fixture.session();
    session.did_open("/p/agree.cpp", agreeing);
    session.index_everything();
    let view = session
        .view_of_the_file(std::path::Path::new("/p/agree.cpp"))
        .expect("a view");
    let at = agreeing.find("VALUE = ").expect("the fixture");
    assert_eq!(
        session.constant_value_at(&view, crate::file::coordinates::ReadingOffset::new(at)),
        Some(32),
        "both definitions in force say 0x10"
    );
}

/// **A header is read in one translation unit's context, and it is the same one every time.**
///
/// Nothing in the language says which translation unit a header is "in": the analysis reads it with the macros of
/// one of the units that contains it in force. It used to take *any* such unit out of a hash map, so the answer
/// depended on the map's layout — measured on neovim's `grid.h`, which is clean on its own and reads
/// `__declspec(dllexport) ScreenGrid …` when `main.c`'s unit answers for it, because `main.c:3` defines `EXTERN`
/// for its own translation unit. The census (517 files indexed) reported 2 errors there; the same file asked alone
/// reported 0.
///
/// The rule is now the **lowest root path** among the units held, which is a function of the files rather than of
/// their iteration order. `b.cpp` and `c.cpp` define the same name differently; the header's reading has to be
/// `b.cpp`'s whichever order the two were read in.
#[test]
pub(crate) fn a_header_is_read_in_the_same_translation_unit_every_time() {
    for order in [["/p/b.cpp", "/p/c.cpp"], ["/p/c.cpp", "/p/b.cpp"]] {
        let files = MemoryFiles::new()
            .with_file("/p/h.h", "KIND\n")
            .with_file("/p/b.cpp", "#define KIND int value_b;\n#include \"h.h\"\n")
            .with_file("/p/c.cpp", "#define KIND int value_c;\n#include \"h.h\"\n");
        let memory = Memory::new("header-context", &files);
        let mut session = memory.session();
        let roots: Vec<PathBuf> = order.iter().map(PathBuf::from).collect();
        session.add_project_files(roots.clone());
        session.index_everything();
        for root in &roots {
            session.cook(root).expect("the root is cooked");
        }
        session.cook("/p/h.h").expect("the header is cooked");

        let names: Vec<String> = session
            .index()
            .cooked_declarations(Path::new("/p/h.h"))
            .expect("the header has a reading")
            .iter()
            .map(|fact| fact.name.clone())
            .collect();
        assert!(
            names.iter().any(|name| name == "value_b"),
            "the lowest root path decides the context, whichever order the roots were read in ({order:?}): {names:?}"
        );
        assert!(
            !names.iter().any(|name| name == "value_c"),
            "and the other translation unit's context is not used ({order:?}): {names:?}"
        );
    }
}

/// **A name a macro writes is in the index only once the header that writes it has been cooked.**
///
/// A summary is the *shape-only* reading: `DECL(lib_open)` is a macro call and nothing else, so the name index has
/// no `lib_open` from it. The **cooked** reading is where the declaration appears, and the session keeps one per
/// file — so the answer depends on whether anything cooked the header:
///
/// ```text
/// census path    cook the .c, never its include   ->  NotDeclaredHere
/// editor path    the pump cooks the include too   ->  the declaration, in lib.h
/// ```
///
/// The editor path is the contract, not a convenience: `Session::want_the_closure_cooked` runs for every open
/// document and reaches the direct includes and one level under them. This test pins both, because the first is
/// what a project-wide run measures (the census asks for one file's reading) and the second is what a reader sees.
///
/// Measured on neovim: `uv.h`'s ~150 functions are written `UV_EXTERN int uv_run(…)`, its summary holds **4**
/// declarations and 61 macros, and the census's `uv_* names: 15 asked, 2 resolved` is the first path.
#[test]
pub(crate) fn a_name_a_macro_writes_is_in_the_index_once_its_header_was_cooked() {
    let files = MemoryFiles::new()
        .with_file(
            "/p/lib.h",
            "#define DECL(name) extern int name(void);\nDECL(lib_open);\n",
        )
        .with_file(
            "/p/a.c",
            "#include \"lib.h\"\nvoid f(void) { lib_open(); }\n",
        );
    let fixture = Memory::new("macro-declaration", &files);

    // **Read on its own.** The include is never cooked, so the declaration is nowhere in the index.
    let mut alone = fixture.session();
    alone.add_project_files([PathBuf::from("/p/a.c")]);
    alone.index_everything();
    alone.cook("/p/a.c").expect("the file is cooked");
    let view = alone.view("/p/a.c").expect("the file reads");
    let at = view
        .source
        .find("lib_open")
        .expect("the call is in the text");
    assert!(
        matches!(
            alone.definition(&view, ReadingOffset::new(at)),
            Known::Unknown(_)
        ),
        "nothing cooked the header, so the declaration is nowhere in the index"
    );

    // **Looked at.** The pump cooks the open file's closure, the header's cooked reading is where the macro's
    // declaration is, and the jump lands in `lib.h`.
    let mut editor = fixture.session();
    editor.did_open(
        "/p/a.c",
        "#include \"lib.h\"\nvoid f(void) { lib_open(); }\n",
    );
    editor.index_everything();
    editor.advance(64);
    let view = editor.view("/p/a.c").expect("the file reads");
    let at = view
        .source
        .find("lib_open")
        .expect("the call is in the text");
    match editor.definition(&view, ReadingOffset::new(at)) {
        Known::Yes(found) => {
            assert_eq!(found.file, Path::new("/p/lib.h"));
            assert_eq!(found.fact.name, "lib_open");
        }
        other => panic!("the macro's declaration is not in the index: {other:?}"),
    }
}

/// **A header's own include guard does not make its body inactive.**
///
/// The summary records `#ifndef H_H / #define H_H` at the top of a header as an **unconditional** definition —
/// `deguard_the_files_own_guard`, because the file is read once per translation unit — and the *composed*
/// environment (the one a header gets when no program has read it) took the file's own facts. So `H_H` was defined
/// **before the first line**, `#ifndef H_H` was false, and the whole file rendered empty: a per-file cook of every
/// guarded header produced no declarations at all.
///
/// Measured on neovim's `.deps/usr/include/uv.h`: 13 263 tokens in, **0 bytes** out, and 0 declarations filed; with
/// the file's own facts excluded, 37 112 bytes and **1 712** declarations — which is what `uv_run` and the rest of
/// libuv need to be answerable at all.
#[test]
pub(crate) fn a_headers_own_guard_does_not_empty_its_rendering() {
    let files = MemoryFiles::new()
        .with_file(
            "/p/guarded.h",
            "#ifndef GUARDED_H\n#define GUARDED_H\nstruct Widget { int size; };\n#endif\n",
        )
        .with_file("/p/a.c", "#include \"guarded.h\"\nvoid f(void) { }\n");
    let fixture = Memory::new("own-guard", &files);
    let mut session = fixture.session();
    session.add_project_files([PathBuf::from("/p/a.c")]);
    session.index_everything();
    // **The composed environment**: no program read this header, and it has an includer, so it is not a root and
    // gets the environment the index can describe rather than a timeline.
    session.cook("/p/guarded.h").expect("the header cooks");

    let facts = session
        .index()
        .cooked_declarations(Path::new("/p/guarded.h"))
        .expect("the header has a reading");
    assert_eq!(
        facts.iter().filter(|fact| fact.name == "Widget").count(),
        1,
        "the declaration inside the guard is in the rendered reading: {facts:?}"
    );
}

/// **One cheap view does not evict one expensive one.**
///
/// A rendering view is a whole unit — `test_cpp/src/containers.cpp` is seventeen standard headers, measured at
/// ~75 MB of parse tree — and a buffer view is the page being typed. Held in **one** cache of eight entries,
/// twelve buffer views (a hover, a pull and a semantic-token request each touch several) threw the rendering away,
/// so every later request parsed the whole unit again: private bytes 190 MB → 7 500 MB over twelve rounds while
/// the session's own tables stayed near 40 MB. Two populations, two caches.
#[test]
pub(crate) fn a_rendering_view_survives_twelve_buffer_views() {
    let main = "#include \"a.h\"\nint main() { return 0; }\n";
    let mut files = MemoryFiles::new().with_file("/p/main.cpp", main);
    for index in 0..12 {
        files = files.with_file(format!("/p/h{index}.h"), "#define A 1\n");
    }
    let fixture = Memory::new("a-rendering-view-survives", &files);
    let mut session = fixture.session();
    let project: Vec<std::path::PathBuf> = (0..12)
        .map(|index| std::path::PathBuf::from(format!("/p/h{index}.h")))
        .collect();
    session.add_project_files(project.iter().cloned());
    session.did_open("/p/main.cpp", main);
    for _ in 0..8 {
        session.index_everything();
        session.catch_up(std::path::Path::new("/p/main.cpp"));
        if session.pending_work() == 0 {
            break;
        }
    }

    let rendered = session.view(std::path::Path::new("/p/main.cpp"));
    assert!(rendered.is_some(), "the cook left a rendering to parse");
    assert_eq!(
        session.rendering_views.lock().expect("not poisoned").len(),
        1,
        "the rendering view is remembered in its own cache"
    );

    // The twelve cheap ones, each a distinct key and each far smaller than the rendering.
    for header in &project {
        let _ = session.view_of_the_file(header);
    }
    assert!(
        session.views.lock().expect("not poisoned").len() <= 8,
        "the buffer cache keeps its own cap"
    );
    assert_eq!(
        session.rendering_views.lock().expect("not poisoned").len(),
        1,
        "and twelve buffer views did not throw it away"
    );
    assert!(
        session.view(std::path::Path::new("/p/main.cpp")).is_some(),
        "the second ask is a cache hit, not a re-parse"
    );
}

/// **The document the reader is looking at gets its program, and a background tab does not.**
///
/// This replaces a test that measured a ceiling which no longer exists — a source-size limit and a session-wide byte
/// budget, both of which refused to read a program and left the file with the per-file reading. Their number was
/// "5 GB of private bytes", read off a counter that only ever **added**: measured with one that counts frees, a cold
/// index of `test_cpp` holds 147 MB, seventeen open files with the pump drained hold 250 MB, and seventeen
/// whole-program reads take it to 478 MB (peak 528) — about 13 MB each. See `tests/cold_start_memory_probe.rs` and
/// `docs/open-questions.md`.
///
/// The contract the schedule holds now is the one the measurement named: one program per *focused* document. Every
/// open document used to be a candidate, and twenty files open was measured as **19 whole-program reads in a
/// twenty-second idle window** — the same 116 headers parsed once per tab. A program read is the most expensive thing
/// this session does, and the answers a request needs are about the file it just named.
///
/// Every open document used to be a candidate, and twenty files open was measured as **19 whole-program reads in a
/// twenty-second idle window** on `test_cpp` — the same 116 headers parsed once per tab, ~90 MB of churn each, for
/// files nobody was looking at. A program read is the most expensive thing this session does, and the answers a
/// request needs are about the file it just named.
#[test]
pub(crate) fn the_focused_document_gets_its_program() {
    let main = "#include \"api.h\"\nApi a;\n";
    let second = "#include \"api.h\"\nApi b;\n";
    let files = MemoryFiles::new()
        .with_file("/p/api.h", "struct Api { int x; };\n")
        .with_file("/p/main.cpp", main)
        .with_file("/p/second.cpp", second);
    let fixture = Memory::new("the-focused-document-gets-its-program", &files);
    let mut session = fixture.session();
    session.did_open("/p/main.cpp", main);
    session.did_open("/p/second.cpp", second);
    session.index_everything();
    session.pretend_a_pause(std::time::Duration::from_millis(1_100));

    assert_eq!(
        session.the_program_to_read().as_deref(),
        Some(Path::new("/p/second.cpp")),
        "the document the reader opened last is the one whose program is next"
    );
    assert!(
        session.read_a_program_now().is_some(),
        "and it is read when the session gets the chance"
    );
    assert_eq!(
        session.the_program_to_read(),
        None,
        "the background tab is not a program to read"
    );

    // **Focusing the other tab makes it the next one** — and only then.
    session.did_change("/p/main.cpp", main);
    session.pretend_a_pause(std::time::Duration::from_millis(1_100));
    assert_eq!(
        session.the_program_to_read().as_deref(),
        Some(Path::new("/p/main.cpp")),
        "the file that was just typed into is the one whose program is next"
    );
}
