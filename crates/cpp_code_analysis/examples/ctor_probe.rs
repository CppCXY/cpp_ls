//! **What this layer reads out of a function's head** — one line per spelling.
//!
//! ```text
//! cargo run --release --example ctor_probe -p cpp_code_analysis
//! ```
//!
//! Written for a report that showed three false `missing-return` findings, and it is the file to start from before
//! changing anything about how a return type or a function's name is read: every one of the faults it found was
//! invisible in the spellings the tests used and obvious in the table.
//!
//! The distinctions that matter, and each of them broke a version of the reader:
//!
//! ```text
//!   constexpr ~S() {}                 a specifier that is not a type, and a name that is not an identifier
//!   explicit S(int a) {}              the same, with an ordinary identifier
//!   int S::f() {}                     a scope that belongs to the NAME
//!   const std::string& f() {}         a scope that belongs to the TYPE
//!   int a::b::c() {}                  two scopes, so stripping one is not enough
//!   operator+ / Iterator::operator*   a name that is not an identifier at all
//!   int* f()                          a declarator that belongs to the type while sitting outside it
//! ```
use cpp_code_analysis::sema::check::{name_declared_by, return_type_written_by};
use cpp_parser::{CppAstNode, CppDeclaration, CppParser, ParserConfig, CppSyntaxKind};

/// Every function definition in `source`, with what the readers answer for it.
fn head(label: &str, source: &str) {
    let tree = CppParser::parse(source, ParserConfig::default());
    let mut seen = false;
    for node in tree.get_red_root().descendants() {
        if CppDeclaration::cast(node.clone()).is_none() {
            continue;
        }
        if node
            .children()
            .all(|child| CppSyntaxKind::from(child.kind()) != CppSyntaxKind::CompoundStat)
        {
            continue;
        }
        seen = true;
        println!(
            "  {label:34} returns {:?}   name {:?}",
            return_type_written_by(&node),
            name_declared_by(&node)
        );
    }
    if !seen {
        println!("  {label:34} (no definition found)");
    }
}

fn main() {
    println!("--- what each spelling reads as ---");
    head("int f", "int f() {}\n");
    head("int* f", "int* f() {}\n");
    head("static int f", "static int f() {}\n");
    head("inline int f", "inline int f() {}\n");
    head("constexpr int f", "constexpr int f() {}\n");
    head("virtual int f", "struct S {\n    virtual int f() {}\n};\n");
    head("auto f", "auto f() { return 1; }\n");
    head("int S::f", "int S::f() {}\n");
    head("void S::g", "void S::g() {}\n");
    head("int a::b::c", "int a::b::c() {}\n");
    head("std::string h", "std::string h() {}\n");
    head("const std::string& h", "const std::string& h() {}\n");

    println!("--- the shapes that write no return type ---");
    head("~S", "struct S {\n    ~S() {}\n};\n");
    head("constexpr ~S", "struct S {\n    constexpr ~S() {}\n};\n");
    head("S (constructor)", "struct S {\n    S(int a) {}\n};\n");
    head("explicit S", "struct S {\n    explicit S(int a) {}\n};\n");
    head("S::~S", "S::~S() {}\n");
    head("S::S", "S::S(int a) {}\n");
    head("operator+", "struct S {\n    int operator+(int o) {}\n};\n");
    head("Iterator::operator*", "constexpr void Iterator::operator*() const {}\n");

    println!("\nEvery `returns` above is what a message would name, and `None` is a shape C++ gives no return type.");
}
