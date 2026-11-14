//! **What a "create the definition" edit would write**, and when there is nothing to write.
//!
//! The pieces come from two places on purpose: the class, the name and the return type from the summary, and the
//! parameter list and the trailing qualifiers from the declaration's own declarator — a definition must repeat
//! `(int w, int h)` as written, and `DeclFact` does not hold a parameter list at all.

use cpp_code_analysis::definition::MissingDefinition;
use cpp_code_analysis::file::coordinates::FileOffset;
use cpp_code_analysis::{
    CompilerConfig, FileView, MemoryFiles, OpenDocuments, Session, SessionFiles, WatchFilter,
};

fn session_of(files: &[(&str, &str)]) -> Session<MemoryFiles> {
    let mut memory = MemoryFiles::new();
    let mut paths = Vec::new();
    for (path, text) in files {
        memory = memory.with_file(*path, *text);
        paths.push(std::path::PathBuf::from(path));
    }
    let providers = SessionFiles::new(OpenDocuments::new(), memory);
    let mut session = Session::with_config(
        "/p",
        providers,
        WatchFilter::new("/p"),
        CompilerConfig::default(),
    );
    session.add_project_files(paths.clone());
    for path in &paths {
        session.load(path);
        session.want_cooked_reading(path);
    }
    session.index_everything();
    session
}

/// The definition the session would write for the cursor placed on `needle` in `path`.
fn written(session: &Session<MemoryFiles>, path: &str, needle: &str) -> Option<MissingDefinition> {
    let path = std::path::Path::new(path);
    let view: FileView = session.view(path)?;
    let at = view.written_text()?.find(needle)?;
    let reading = view.reading_offset_of(FileOffset::new(at))?;
    session.missing_definition_at(&view, reading)
}

const SHAPE: &str = "\
struct Shape {
    virtual double area() const;
    void noop();
    void draw(int w, int h) noexcept;
    Shape(int x);
};
";

/// **A declared member with no definition** gives the whole line: return type, qualified name, parameters, qualifiers.
#[test]
fn a_missing_definition_is_written_with_its_qualifiers() {
    let session = session_of(&[("/p/shape.h", SHAPE)]);
    let found = written(&session, "/p/shape.h", "area").expect("`area` has no definition");
    assert_eq!(found.class, "Shape");
    assert_eq!(found.qualified_name, "Shape::area");
    assert_eq!(found.returns.as_deref(), Some("double"));
    assert_eq!(found.parameters, "()");
    assert_eq!(found.qualifiers, " const");
    assert_eq!(found.text(), "double Shape::area() const\n{\n}");
}

/// **The parameter list is copied as written**, names and all.
#[test]
fn the_parameters_are_copied_as_written() {
    let session = session_of(&[("/p/shape.h", SHAPE)]);
    let found = written(&session, "/p/shape.h", "draw").expect("`draw` has no definition");
    assert_eq!(found.parameters, "(int w, int h)");
    assert_eq!(found.qualifiers, " noexcept");
    assert_eq!(
        found.text(),
        "void Shape::draw(int w, int h) noexcept\n{\n}"
    );
}

/// **A constructor has no return type**, and its qualified name repeats the class's.
#[test]
fn a_constructor_definition_has_no_return_type() {
    let session = session_of(&[("/p/shape.h", SHAPE)]);
    let found = written(&session, "/p/shape.h", "Shape(int").expect("the constructor has no definition");
    assert_eq!(found.returns, None);
    assert_eq!(found.qualified_name, "Shape::Shape");
    assert_eq!(found.text(), "Shape::Shape(int x)\n{\n}");
}

/// **A member defined in the same file** is not offered again — the edit would duplicate a definition.
#[test]
fn a_member_defined_in_the_class_is_not_offered() {
    let session = session_of(&[("/p/shape.h", "struct Shape {\n    void noop() { }\n};\n")]);
    assert!(
        written(&session, "/p/shape.h", "noop").is_none(),
        "`noop` already has a body"
    );
}

/// **A member defined in another file** is not offered either — the index knows it.
#[test]
fn a_member_defined_in_a_cpp_is_not_offered() {
    let session = session_of(&[
        ("/p/shape.h", "struct Shape {\n    void noop();\n};\n"),
        (
            "/p/shape.cpp",
            "#include \"shape.h\"\nvoid Shape::noop() { }\n",
        ),
    ]);
    assert!(
        written(&session, "/p/shape.h", "noop").is_none(),
        "the `.cpp` defines it"
    );
}

/// **A pure virtual is not offered**: its body belongs to a derived class, not to the class declaring it.
#[test]
fn a_pure_virtual_is_not_offered() {
    let session = session_of(&[(
        "/p/shape.h",
        "struct Shape {\n    virtual double area() const = 0;\n};\n",
    )]);
    assert!(
        written(&session, "/p/shape.h", "area").is_none(),
        "a pure virtual is `= 0`"
    );
}

/// **A free function** is not a class member: the edit is about the declaration/definition split a class makes.
#[test]
fn a_free_function_is_not_offered() {
    let session = session_of(&[("/p/a.cpp", "void run();\n")]);
    assert!(
        written(&session, "/p/a.cpp", "run").is_none(),
        "`run` is not a member"
    );
}
