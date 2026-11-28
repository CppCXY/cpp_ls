//! Fixtures shared by more than one test file.

use std::time::Duration;

/// How long a test waits for an answer before calling it a failure.
///
/// Thirty seconds is generous for a project of fixture files and it is **not** generous for one that pulls in the
/// standard library: `#include <string>` is a closure of thousands of MSVC headers, and the first request about a
/// name inside it waits for the index to read them. A test whose subject *is* that closure therefore waits longer —
/// see [`STD_LIBRARY_TIMEOUT`] — rather than the whole file being given a longer deadline, because a fixture that
/// hangs should still fail in half a minute.
///
/// **The deadline is a ceiling, not a wait** — a warm run answers in milliseconds — and it has to absorb a
/// *loaded* machine: `cargo test --workspace` runs several suites at once, and each handshake test spawns its own
/// server, so a debug-build cook that takes two seconds alone can take tens of seconds while the other binaries
/// are building or indexing. Measured: `handshake::features::the_folds_of_a_file_are_the_regions_a_reader_hides`
/// failed one full-workspace run in three at thirty seconds, and passed alone in 4.7 s every time.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(120);

/// The deadline for a test that makes the analysis read a standard-library header, which is real work.
pub(crate) const STD_LIBRARY_TIMEOUT: Duration = Duration::from_secs(180);

pub(crate) const WIDGET_H: &str = "struct Widget { int size; };\n";

/// A file with two things to find and one thing to report: a use of `Widget`, a member access, and a class whose
/// brace is never closed.
///
/// The unclosed `{` is deliberate and it is the *only* error in the file: the parser reports `expected ;` at the
/// end of the last line, which gives the diagnostic assertion a line and a column to check rather than a count.
/// (`int g() { return 1 }` — a missing semicolon before a closing brace — is **not** reported by this parser; that
/// is a real gap in its error reporting and it is recorded.)
pub(crate) const MAIN_CPP: &str = "#include \"widget.h\"\n\nvoid f() {\n    Widget w;\n    w.size = 1;\n}\nstruct Unclosed { int x;\n";

/// The macro-declared type: the declaration is inside `DECLARE_HANDLE`'s replacement list, and `HWND h;` is a use
/// of the name it makes.
pub(crate) const HANDLE_H: &str = "#define DECLARE_HANDLE(name) struct name##__ { int unused; }; \
                        typedef struct name##__ *name\n";

pub(crate) const API_H: &str = "#include \"handle.h\"\nDECLARE_HANDLE(HWND);\n";

pub(crate) const API_CPP: &str = "#include \"api.h\"\nHWND h;\n";

/// The reported file, verbatim: the includes it does not write are part of the report.
pub(crate) const REPORTED_CPP: &str = r#"#include <iostream>
#include <string>
#include <cstdio>
int main() {
    int a = 1, b = 2;
    int sum = a + b;
    std::cout << "The sum of " << a << " and " << b << " is: " << sum << std::endl;
    std::string full;
    std::cout << "Full name: " << full << std::endl;
    std::cout << "hello world " << std::endl;
    full.size();
    full.length();
    full.size();
    printf("Full name Length: %zu\n", full.size());
    // printf("Full name Length using length(): %zu\n", full.length());
    return 0;
}
"#;

/// The namespace is opened by a macro in a header the open file includes. **The raw reading handles this one** —
/// an indexer given the closure's macro bodies reads `BEGIN_NS` as `namespace one {`, which is what the shape rules
/// exist for — so this fixture is about the member question rather than about the second reading.
pub(crate) const NAMESPACE_H: &str = "#define BEGIN_NS namespace one {\n#define END_NS }\n";

pub(crate) const NAMESPACED_WIDGET_H: &str =
    "#include \"namespace.h\"\nBEGIN_NS struct Widget { int size; }; END_NS\n";

/// The fixture that **needs** the second reading: a macro that declares something. `api.h`'s own text has a call
/// where the struct and the typedef are, and no shape rule invents a declaration out of a call.
pub(crate) const HWND_CPP: &str = "#include \"api.h\"\nvoid f() {\n    HWND\n}\n";

/// The cursor is after a `.` — the question that needs a **type**, made interesting by the spelling: `w`'s type is
/// written `one::Widget`.
pub(crate) const MEMBER_CPP: &str =
    "#include \"widget.h\"\nvoid f() {\n    one::Widget w;\n    w.\n}\n";

/// **The inlay fixture whose callee is in a header** — the shape a reported file has, and the one that makes the
/// refusal visible.
///
/// The fixture below declares `scale` in the same file it is called from, which hides the bug the refusal caused:
/// an edit drops that file's summary, so the analysis itself answers nothing until the drain, and a test cannot tell
/// "refused" from "honestly empty". A callee in a header is untouched by an edit to the file that calls it, so the
/// answer is available the whole time — and anything that suppresses it is then the only thing being measured.
pub(crate) const INLAY_HEADER: &str = "\
#pragma once
double scale(int count, double factor);
";

pub(crate) const INLAY_ACROSS_FILES: &str = "\
#include \"widget.h\"

double f(int count) {
    return scale(count, 1);
}
";

/// The inlay fixture: a call whose argument already spells the parameter, one that does not, and a member call.
pub(crate) const INLAY_CPP: &str = "\
double scale(int count, double factor);

struct Widget {
    int size_of(int scale) const;
};

double f(Widget& w, int count) {
    return scale(count, 1) + w.size_of(2);
}
";

/// The `std::string` fixture: a standard-library type from a header, and a `.` where the member list belongs.
///
/// Written the way a reader does — `#include <string>`, a declaration, then the operator — so that the test covers
/// the chain and not the completion layer alone: the header has to be **found** on the search path, its declarations
/// have to be **indexed**, and `std::string` has to resolve before there is a member list to collapse.
pub(crate) const STD_STRING_CPP: &str = "\
#include <string>
int main() {
    std::string full;
    full.
}
";

/// The fixture for the last-line case: the operator is the **final line of the body**, with `}` after it.
///
/// Written to match the report it came from, including the parts that look incidental and are not: a member
/// function with a body (so the class has a non-trivial parse), a second variable of a user-defined type (so the
/// scope is not empty), and the standard library included (so the type's members come from a header).
pub(crate) const LAST_LINE_CPP: &str = "\
#include <iostream>
#include <string>
struct Name {
    std::string firstName;
    std::string lastName;
    std::string getFullName() const { return firstName + lastName; }
};
int main() {
    Name myName;
    std::string full2;
    full2.
}
";

/// The completion-resolve fixture: a documented member, offered through a member access.
pub(crate) const RESOLVE_CPP: &str = "\
struct Widget {
    /// The widget's size in cells.
    int size;
};
int f() {
    Widget w;
    w.
}
";

/// The fixture for [`a_completion_inside_a_macros_argument_is_about_that_argument`]: a macro whose second argument is
/// where the cursor goes, and two types with one member each so that a wrong subject is visible in the list.
pub(crate) const MACRO_ARGUMENT_CPP: &str = "\
struct alpha { int one; };
struct beta { int gamma; };

#define PICK(first, second) pick_impl((first), (second))

beta bet;

int f() {
    return PICK(1, bet.);
}
";

/// The fixture: a predefined macro in a directive, a macro defined and used, and a C11 keyword construct.
pub(crate) const MACROS_C: &str = "\
#ifdef __STDC_VERSION__
#define CORPUS_STANDARD __STDC_VERSION__
#else
#define CORPUS_STANDARD 0L
#endif

#define SUM(count, ...) (impl)(count __VA_OPT__(, ) __VA_ARGS__)

int impl(int count, ...);

int use(void) {
    int total = 0;
    int selected = 2;
    int n = SUM(selected, 1, 2, 3);
    _Static_assert(sizeof(int) >= 2, \"an int is at least two bytes\");
    return n + total;
}
";

/// The fixture: a header the file includes, a macro, a static function, and a call of each.
pub(crate) const C_COMPLETION_C: &str = "\
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <time.h>

#define LOCAL_MACRO(n) ((n) + 1)

static int local_helper(int n) { return n; }

int main(void) {
    int value = local_helper(1);
    fprintf(stdout, \"%d\\n\", value);
    return LOCAL_MACRO(value);
}
";

/// The file whose *text* has an error in a branch the preprocessor removes: `OFF` is 0, so the unclosed class never
/// reaches a compiler — while the bytes are right there in the buffer.
pub(crate) const BRANCH_CPP: &str =
    "#include \"cfg.h\"\n#if OFF\nstruct Unclosed { int x;\n#endif\nint ok;\n";

pub(crate) const CFG_H: &str = "#define OFF 0\n";

/// The clean file [`an_error_typed_into_an_open_file_is_answered`] opens, and the same file one `;` short.
pub(crate) const CLEAN_CPP: &str = "int g();\nstruct Name {\n    int middleName;\n    void print(int i) {\n        \
                         middleName = i;\n    }\n};\n";

pub(crate) const EDITED_CPP: &str = "int g();\nstruct Name {\n    int middleName;\n    void print(int i) {\n        \
                          middleName = i\n    }\n};\n";

/// The folding fixture: an include run, a comment block, a class, and a conditional — one of each kind the protocol
/// names, in an order where the lines are easy to count.
pub(crate) const FOLDS_CPP: &str = "#include <string>\n\
                         #include <vector>\n\
                         \n\
                         // a note\n\
                         // and another\n\
                         \n\
                         struct Widget {\n\
                             int size;\n\
                         };\n\
                         \n\
                         #if defined(_WIN32)\n\
                         int win;\n\
                         #endif\n";

/// The semantic-token fixture: a macro, a namespace, a class with a method, and a parameter.
pub(crate) const COLOURS_CPP: &str = "\
#define LIMIT 8
namespace shapes {
struct Widget {
    int size;
    int scaled(int factor) const;
};
}
";

/// The signature fixture: a documented declaration, and a call being typed into.
pub(crate) const SIGNATURE_CPP: &str = "\
/// Scales a count.
/// @param count the count
int scale(int count, double factor);

int f() {
    return scale(1, );
}
";

/// The fixture for [`a_signature_survives_being_inside_a_macro_argument`]: a real declaration, a call to it, and the
/// whole call written as a macro's argument so that the reading the server parses has been rewritten around it.
pub(crate) const SIGNATURE_IN_A_MACRO_CPP: &str = "\
#define PICK(first, second) pick_impl((first), (second))

/// Scales a count.
int scale(int count, double factor);

int f() {
    return PICK(1, scale(2, ));
}
";

/// The fixture for the two cards above: a class template declared in a **header** (the shape a real project has —
/// `map` is in `<map>`, not in the file being edited) used with arguments, and an anonymous class type.
pub(crate) const INSTANTIATION_HEADER: &str = "\
#pragma once
namespace std {
template <class _Kty, class _Ty, class _Less>
class map {
public:
    void insert(const _Kty& key, const _Ty& value);
};
}
";

pub(crate) const INSTANTIATION_CPP: &str = "\
#include \"map.h\"

std::map<const char*, int, Less> table;

static const struct
{
    unsigned char left;
    unsigned char right;
} priority[] = { { 1, 2 } };
";

/// The hover fixture: a documented function, one with a plain comment above it, and a `this` in a member function.
pub(crate) const HOVER_CPP: &str = "\
/// Adds two counts.
/// @param a the first
/// @param b the second
int add(int a, int b);

// A note to the reader, which is not documentation.
int undocumented(int c);

struct Widget {
    int size;
    int scaled(int factor) const { return size * factor; }
    int go() const { return this->scaled(2); }
};
";

/// A macro defined in one header and used in two files — the shape a rename has to get right, and the shape a
/// *reference list* has to walk: the definition, and the uses that can see it.
pub(crate) const LIMITS_H: &str = "#define MAX_ITEMS 64\n";

pub(crate) const ITEMS_H: &str = "#include \"limits.h\"\nint items[MAX_ITEMS];\n";

pub(crate) const ITEMS_CPP: &str = "#include \"api.h\"\nvoid f() {\n    int more[MAX_ITEMS];\n}\n";

/// The selection fixture: one expression nested in a statement, a body and a function.
pub(crate) const SELECTION_CPP: &str = "int f() {\n    return w.size;\n}\n";

/// The workspace-symbol fixture: a class in a namespace, in a header — and the client opens only the `.cpp`.
pub(crate) const SEARCH_H: &str =
    "namespace ns {\nstruct Widget { int size; };\n}\nstruct WidgetRegistry { int count; };\n";

pub(crate) const SEARCH_CPP: &str = "#include \"search.h\"\nvoid f() { ns::Widget w; }\n";

pub(crate) const OUTLINE_CPP: &str = "#include \"cfg.h\"\n\
                           #include \"handle.h\"\n\
                           #if OFF\n\
                           struct Dead { int x; };\n\
                           #endif\n\
                           struct Live { int y; };\n\
                           DECLARE_HANDLE(HWND);\n";

/// The type a `.` is asked about is declared in a **header**, and the test's change rewrites that header: the only
/// way the new member can be in the answer is if the header's *summary* was rebuilt, which is what
/// [`cpp_code_analysis::Session::catch_up`] does before the query.
pub(crate) const FRESH_TYPE_H: &str = "struct Widget { int size; };\n";

/// The uses of it, and a `.` at the end of the last line.
pub(crate) const FRESH_TYPE_CPP: &str = "\
#include \"widget.h\"
int main() {
    Widget w;
    w.
}
";
