//! The three vocabularies that are not declarations: **keywords**, **snippets** and **directives**.
//!
//! Everything else in completion is derived from the program — a scope's bindings, an index's facts, a class's
//! members. These three are the language's own, and they are here rather than in the client because the *client*
//! cannot know which of them fits: a keyword list offered after a `.` is noise, `while` is not a name, and a
//! snippet's placeholders are only worth inserting where a statement may be written.
//!
//! ```text
//! KEYWORDS    the language's spelling, with the shape it fits (a statement, a type, a specifier)
//! SNIPPETS    a construct and its body, so that `if` writes the braces and puts the cursor in them
//! DIRECTIVES  the preprocessor's own names, offered after a `#`
//! ```
//!
//! # Why the tables are keyed by `&'static str` and read linearly
//!
//! There are eighty-odd keywords, twenty snippets and fourteen directives. A completion is a keystroke, the prefix
//! is what filters them, and a linear scan of a hundred short strings is not a cost worth a data structure — while
//! a `HashMap` would be one more thing to keep in step. The tables are `const`, so they are in the binary and cost
//! nothing to build.
//!
//! # Where the line is drawn on snippets
//!
//! Only constructs whose body a reader would otherwise type by hand: the control-flow statements, a class, a
//! namespace, a function, a template, a `main`. Not "every declaration form" — a snippet for `int x = 0;` saves
//! three keystrokes and gets in the way of the far more common case of completing a name that happens to start
//! with `i`. A snippet earns its place by writing a **shape** (braces, a clause, a placeholder) rather than a
//! spelling.

/// Which of a keyword's several shapes a cursor is in.
///
/// A keyword's own grammar decides which positions it fits, and the three this can answer are the three a reader
/// notices: `while` is a statement and never a type, `void` is a type and never a statement, `static` is a
/// specifier and can start a declaration of either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeywordUse {
    /// A statement or a control-flow clause: `return`, `while`, `case`, `else`.
    Statement,
    /// A type specifier: `void`, `int`, `auto`, `decltype`, `bool`.
    Type,
    /// A declaration specifier that may begin a declaration of any kind: `const`, `static`, `namespace`, `using`.
    Specifier,
    /// A literal or an operator keyword that is an expression in its own right: `true`, `false`, `nullptr`,
    /// `this`, `new`, `delete`, `sizeof`, `throw`.
    Expression,
    /// A name that is only meaningful **inside** a class body or a class head: `public`, `virtual`, `friend`,
    /// `explicit`, `mutable`, `override` is not a keyword but `final` is not one either — only the keywords are
    /// listed.
    Member,
}

/// One keyword, and the shape it fits.
#[derive(Debug, Clone, Copy)]
pub struct Keyword {
    pub spelling: &'static str,
    pub use_kind: KeywordUse,
    pub detail: &'static str,
}

/// A snippet: a construct, its body, and what to call it.
#[derive(Debug, Clone, Copy)]
pub struct Snippet {
    /// What is typed to reach it, and the item's label.
    pub trigger: &'static str,
    /// A one-line description, shown beside the label.
    pub detail: &'static str,
    /// The body, in the client's snippet syntax: `$0` is where the cursor ends up, `${1:name}` is a placeholder.
    pub body: &'static str,
    /// Could this be written where **nothing** has been written yet — at the start of a statement?
    ///
    /// Everything in the table can, with one exception that is worth marking: a snippet whose body continues an
    /// enclosing construct (`else`, `case`, `catch`) is only useful after that construct, and a list that offered
    /// `else` at the top of a function would be offering something that cannot compile.
    pub starts_a_statement: bool,
}

/// One preprocessor directive.
///
/// Named [`DirectiveName`] rather than `Directive` because the *analysis* already has a `Directive`: the parsed
/// `#define`/`#include`/… that [`crate::preprocess::directive`] produces from a file's tokens. That one is a fact
/// about a line somebody wrote; this one is a word the preprocessor knows, which is what a completion offers.
#[derive(Debug, Clone, Copy)]
pub struct DirectiveName {
    /// The spelling after the `#`.
    pub name: &'static str,
    /// What it does, in one line.
    pub detail: &'static str,
}

/// **Every C++ keyword** the lexer knows, with the shape it fits.
///
/// The list is the standard's, and the `use_kind` of each is the grammar's: it is what
/// `is_type_keyword`/`is_declaration_keyword` in the parser decide at their own layer, stated once here for the
/// one consumer that has to *order* them rather than parse them.
pub const KEYWORDS: &[Keyword] = &[
    // Statements and control flow.
    Keyword { spelling: "break", use_kind: KeywordUse::Statement, detail: "leave the enclosing loop or switch" },
    Keyword { spelling: "case", use_kind: KeywordUse::Statement, detail: "a label in a switch" },
    Keyword { spelling: "catch", use_kind: KeywordUse::Statement, detail: "handle an exception" },
    Keyword { spelling: "continue", use_kind: KeywordUse::Statement, detail: "next iteration of the enclosing loop" },
    Keyword { spelling: "default", use_kind: KeywordUse::Statement, detail: "the fallback label in a switch" },
    Keyword { spelling: "do", use_kind: KeywordUse::Statement, detail: "a loop whose condition is checked last" },
    Keyword { spelling: "else", use_kind: KeywordUse::Statement, detail: "the branch taken when an `if` is false" },
    Keyword { spelling: "for", use_kind: KeywordUse::Statement, detail: "a counted or ranged loop" },
    Keyword { spelling: "goto", use_kind: KeywordUse::Statement, detail: "jump to a label" },
    Keyword { spelling: "if", use_kind: KeywordUse::Statement, detail: "a conditional statement" },
    Keyword { spelling: "return", use_kind: KeywordUse::Statement, detail: "leave the function, with a value" },
    Keyword { spelling: "switch", use_kind: KeywordUse::Statement, detail: "select a branch by value" },
    Keyword { spelling: "throw", use_kind: KeywordUse::Statement, detail: "raise an exception" },
    Keyword { spelling: "try", use_kind: KeywordUse::Statement, detail: "a block whose exceptions are handled" },
    Keyword { spelling: "while", use_kind: KeywordUse::Statement, detail: "a loop whose condition is checked first" },
    Keyword { spelling: "co_await", use_kind: KeywordUse::Statement, detail: "suspend a coroutine until a value is ready" },
    Keyword { spelling: "co_return", use_kind: KeywordUse::Statement, detail: "return from a coroutine" },
    Keyword { spelling: "co_yield", use_kind: KeywordUse::Statement, detail: "produce a value from a coroutine" },
    // Types.
    Keyword { spelling: "auto", use_kind: KeywordUse::Type, detail: "deduce the type from the initializer" },
    Keyword { spelling: "bool", use_kind: KeywordUse::Type, detail: "a boolean type" },
    Keyword { spelling: "char", use_kind: KeywordUse::Type, detail: "a character type" },
    Keyword { spelling: "decltype", use_kind: KeywordUse::Type, detail: "the type of an expression" },
    Keyword { spelling: "double", use_kind: KeywordUse::Type, detail: "a double-precision floating-point type" },
    Keyword { spelling: "float", use_kind: KeywordUse::Type, detail: "a single-precision floating-point type" },
    Keyword { spelling: "int", use_kind: KeywordUse::Type, detail: "an integer type" },
    Keyword { spelling: "long", use_kind: KeywordUse::Type, detail: "a wider integer type" },
    Keyword { spelling: "short", use_kind: KeywordUse::Type, detail: "a narrower integer type" },
    Keyword { spelling: "signed", use_kind: KeywordUse::Type, detail: "a signed integer type" },
    Keyword { spelling: "unsigned", use_kind: KeywordUse::Type, detail: "an unsigned integer type" },
    Keyword { spelling: "void", use_kind: KeywordUse::Type, detail: "no value" },
    Keyword { spelling: "wchar_t", use_kind: KeywordUse::Type, detail: "a wide character type (a keyword in C++, a built-in in this parser)" },
    Keyword { spelling: "typename", use_kind: KeywordUse::Type, detail: "a dependent name that is a type" },
    // Declaration specifiers.
    Keyword { spelling: "class", use_kind: KeywordUse::Specifier, detail: "declare a class" },
    Keyword { spelling: "const", use_kind: KeywordUse::Specifier, detail: "not modifiable" },
    Keyword { spelling: "consteval", use_kind: KeywordUse::Specifier, detail: "evaluated at compile time, always" },
    Keyword { spelling: "constexpr", use_kind: KeywordUse::Specifier, detail: "usable in a constant expression" },
    Keyword { spelling: "constinit", use_kind: KeywordUse::Specifier, detail: "initialized at compile time" },
    Keyword { spelling: "enum", use_kind: KeywordUse::Specifier, detail: "declare an enumeration" },
    Keyword { spelling: "extern", use_kind: KeywordUse::Specifier, detail: "a declaration with external linkage" },
    Keyword { spelling: "inline", use_kind: KeywordUse::Specifier, detail: "defined in every translation unit that uses it" },
    Keyword { spelling: "namespace", use_kind: KeywordUse::Specifier, detail: "declare a namespace" },
    Keyword { spelling: "noexcept", use_kind: KeywordUse::Specifier, detail: "does not throw" },
    Keyword { spelling: "register", use_kind: KeywordUse::Specifier, detail: "a storage hint (removed in C++17)" },
    Keyword { spelling: "static", use_kind: KeywordUse::Specifier, detail: "internal linkage, or one shared instance" },
    Keyword { spelling: "static_assert", use_kind: KeywordUse::Specifier, detail: "a compile-time assertion" },
    Keyword { spelling: "struct", use_kind: KeywordUse::Specifier, detail: "declare a struct" },
    Keyword { spelling: "template", use_kind: KeywordUse::Specifier, detail: "declare a template" },
    Keyword { spelling: "thread_local", use_kind: KeywordUse::Specifier, detail: "one instance per thread" },
    Keyword { spelling: "typedef", use_kind: KeywordUse::Specifier, detail: "name an existing type" },
    Keyword { spelling: "union", use_kind: KeywordUse::Specifier, detail: "declare a union" },
    Keyword { spelling: "using", use_kind: KeywordUse::Specifier, detail: "a using-declaration, -directive or alias" },
    Keyword { spelling: "volatile", use_kind: KeywordUse::Specifier, detail: "not to be optimized away" },
    Keyword { spelling: "alignas", use_kind: KeywordUse::Specifier, detail: "the alignment of an object" },
    Keyword { spelling: "export", use_kind: KeywordUse::Specifier, detail: "a module's exported declaration" },
    // Expressions.
    Keyword { spelling: "delete", use_kind: KeywordUse::Expression, detail: "release what `new` allocated" },
    Keyword { spelling: "false", use_kind: KeywordUse::Expression, detail: "the boolean false" },
    Keyword { spelling: "new", use_kind: KeywordUse::Expression, detail: "allocate and construct" },
    Keyword { spelling: "nullptr", use_kind: KeywordUse::Expression, detail: "a null pointer" },
    Keyword { spelling: "operator", use_kind: KeywordUse::Expression, detail: "declare an overloaded operator" },
    Keyword { spelling: "sizeof", use_kind: KeywordUse::Expression, detail: "the size of a type or object" },
    Keyword { spelling: "alignof", use_kind: KeywordUse::Expression, detail: "the alignment of a type" },
    Keyword { spelling: "this", use_kind: KeywordUse::Expression, detail: "the object a member function was called on" },
    Keyword { spelling: "true", use_kind: KeywordUse::Expression, detail: "the boolean true" },
    Keyword { spelling: "typeid", use_kind: KeywordUse::Expression, detail: "the type of an expression" },
    // Class bodies and class heads.
    Keyword { spelling: "explicit", use_kind: KeywordUse::Member, detail: "not usable as an implicit conversion" },
    Keyword { spelling: "friend", use_kind: KeywordUse::Member, detail: "give a function or class access" },
    Keyword { spelling: "mutable", use_kind: KeywordUse::Member, detail: "modifiable in a const member function" },
    Keyword { spelling: "private", use_kind: KeywordUse::Member, detail: "members only this class can reach" },
    Keyword { spelling: "protected", use_kind: KeywordUse::Member, detail: "members derived classes can reach" },
    Keyword { spelling: "public", use_kind: KeywordUse::Member, detail: "members anyone can reach" },
    Keyword { spelling: "virtual", use_kind: KeywordUse::Member, detail: "a member a derived class may replace" },
];

/// **The snippets**, in the order a reader is most likely to want them.
///
/// Every body leaves the cursor where the reader has to type next (`$0`) and names the parts they would otherwise
/// have to remember the order of. `${1:condition}` is a **placeholder** and not a text to insert: a client that
/// supports snippets selects it, so typing replaces it.
pub const SNIPPETS: &[Snippet] = &[
    Snippet { trigger: "if", detail: "if statement", body: "if (${1:condition}) {\n\t$0\n}", starts_a_statement: true },
    Snippet { trigger: "ifelse", detail: "if / else", body: "if (${1:condition}) {\n\t${2}\n} else {\n\t$0\n}", starts_a_statement: true },
    Snippet { trigger: "else", detail: "else branch", body: "else {\n\t$0\n}", starts_a_statement: false },
    Snippet { trigger: "while", detail: "while loop", body: "while (${1:condition}) {\n\t$0\n}", starts_a_statement: true },
    Snippet { trigger: "dowhile", detail: "do / while loop", body: "do {\n\t${1}\n} while (${2:condition});\n$0", starts_a_statement: true },
    Snippet { trigger: "for", detail: "for loop", body: "for (int ${1:i} = 0; ${1:i} < ${2:count}; ++${1:i}) {\n\t$0\n}", starts_a_statement: true },
    Snippet { trigger: "forr", detail: "range-based for loop", body: "for (const auto& ${1:item} : ${2:container}) {\n\t$0\n}", starts_a_statement: true },
    Snippet { trigger: "switch", detail: "switch statement", body: "switch (${1:value}) {\ncase ${2:value}:\n\t$0\n\tbreak;\ndefault:\n\tbreak;\n}", starts_a_statement: true },
    Snippet { trigger: "case", detail: "case label", body: "case ${1:value}:\n\t$0\n\tbreak;", starts_a_statement: false },
    Snippet { trigger: "try", detail: "try / catch", body: "try {\n\t${1}\n} catch (const ${2:std::exception}& ${3:e}) {\n\t$0\n}", starts_a_statement: true },
    Snippet { trigger: "catch", detail: "catch clause", body: "catch (const ${1:std::exception}& ${2:e}) {\n\t$0\n}", starts_a_statement: false },
    Snippet { trigger: "return", detail: "return a value", body: "return ${1:value};$0", starts_a_statement: true },
    Snippet { trigger: "class", detail: "class definition", body: "class ${1:Name} {\npublic:\n\t${2:${1:Name}}();\n\t~${2:${1:Name}}();\n\nprivate:\n\t$0\n};", starts_a_statement: true },
    Snippet { trigger: "struct", detail: "struct definition", body: "struct ${1:Name} {\n\t$0\n};", starts_a_statement: true },
    Snippet { trigger: "enum", detail: "enumeration", body: "enum class ${1:Name} {\n\t$0\n};", starts_a_statement: true },
    Snippet { trigger: "namespace", detail: "namespace", body: "namespace ${1:name} {\n\t$0\n}", starts_a_statement: true },
    Snippet { trigger: "function", detail: "function definition", body: "${1:void} ${2:name}(${3}) {\n\t$0\n}", starts_a_statement: true },
    Snippet { trigger: "template", detail: "template declaration", body: "template <typename ${1:T}>\n${2:void} ${3:name}(${4}) {\n\t$0\n}", starts_a_statement: true },
    Snippet { trigger: "main", detail: "the program's entry point", body: "int main(int argc, char** argv) {\n\t$0\n\treturn 0;\n}", starts_a_statement: true },
    Snippet { trigger: "include", detail: "a header guard", body: "#ifndef ${1:HEADER}_H\n#define ${1:HEADER}_H\n\n$0\n\n#endif  // ${1:HEADER}_H", starts_a_statement: true },
];

/// **The built-in type names that are not keywords.**
///
/// `char8_t`, `char16_t` and `char32_t` really are keywords, and this parser reads them as *names* — its lexer
/// keeps only the words the grammar has a rule for, and these three are read by the same rule that reads any
/// typedef'd name. `size_t` and its neighbours are typedefs the standard requires of `<cstddef>`, so they are
/// names in the strict reading and constants of the language in practice: a reader writing `for (size_` wants the
/// type, and a list that only had what the file includes would not have it in a file that includes nothing.
///
/// Offering them is therefore a small lie about *where* the name comes from, and the honest version of it is that
/// a program which uses these has them. `wchar_t` is not here: it is a keyword and is in [`KEYWORDS`].
pub const BUILTIN_TYPES: &[(&str, &str)] = &[
    ("char8_t", "a UTF-8 code unit"),
    ("char16_t", "a UTF-16 code unit"),
    ("char32_t", "a UTF-32 code unit"),
    ("size_t", "the unsigned type of a size or an index"),
    ("ssize_t", "the signed type of a size or an index"),
    ("ptrdiff_t", "the signed type of a pointer difference"),
    ("intptr_t", "a signed integer wide enough for a pointer"),
    ("uintptr_t", "an unsigned integer wide enough for a pointer"),
    ("int8_t", "an 8-bit signed integer"),
    ("int16_t", "a 16-bit signed integer"),
    ("int32_t", "a 32-bit signed integer"),
    ("int64_t", "a 64-bit signed integer"),
    ("uint8_t", "an 8-bit unsigned integer"),
    ("uint16_t", "a 16-bit unsigned integer"),
    ("uint32_t", "a 32-bit unsigned integer"),
    ("uint64_t", "a 64-bit unsigned integer"),
];

/// **Every standard directive**, offered after a `#`.
///
/// The list is the standard's, plus the two the two big implementations add and every real file uses: `#pragma`
/// and `#error` are standard, `#warning` is not but is universally spelled that way, and `#include_next` is GCC's.
/// A directive that is not here is not offered, which is the right direction: a list of every directive any
/// preprocessor accepts would include spellings that only one compiler takes.
pub const DIRECTIVES: &[DirectiveName] = &[
    DirectiveName { name: "include", detail: "paste a header into this file" },
    DirectiveName { name: "define", detail: "define a macro" },
    DirectiveName { name: "undef", detail: "remove a macro definition" },
    DirectiveName { name: "if", detail: "compile the block when a condition holds" },
    DirectiveName { name: "ifdef", detail: "compile the block when a macro is defined" },
    DirectiveName { name: "ifndef", detail: "compile the block when a macro is not defined" },
    DirectiveName { name: "elif", detail: "another condition for the same `#if`" },
    DirectiveName { name: "else", detail: "the block taken when no condition held" },
    DirectiveName { name: "endif", detail: "end a conditional block" },
    DirectiveName { name: "pragma", detail: "an implementation-defined instruction" },
    DirectiveName { name: "error", detail: "stop the compilation with a message" },
    DirectiveName { name: "warning", detail: "report a message and continue" },
    DirectiveName { name: "line", detail: "set the line number the compiler reports" },
    DirectiveName { name: "include_next", detail: "paste the next header of this name on the search path" },
];

/// **The keywords a cursor in this shape may be writing**, in table order.
///
/// `statement` is whether a *statement* may be written here, and `member` whether the cursor is inside a class
/// body — the two positions that change which of the four vocabularies applies. Everything else is a specifier or
/// a type, which can begin any declaration and therefore any statement.
pub fn keywords_for(statement: bool, member: bool) -> impl Iterator<Item = &'static Keyword> {
    KEYWORDS.iter().filter(move |keyword| match keyword.use_kind {
        KeywordUse::Statement | KeywordUse::Expression => statement,
        KeywordUse::Type | KeywordUse::Specifier => true,
        KeywordUse::Member => member,
    })
}

/// **The snippets a cursor may be writing.** See [`Snippet::starts_a_statement`].
pub fn snippets_for(statement: bool) -> impl Iterator<Item = &'static Snippet> {
    SNIPPETS
        .iter()
        .filter(move |snippet| statement || !snippet.starts_a_statement)
}

/// Is a snippet already covering this spelling, so that the bare keyword would be a second identical item?
pub fn a_snippet_writes_this(spelling: &str) -> bool {
    SNIPPETS.iter().any(|snippet| snippet.trigger == spelling)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table is a **vocabulary**, so the two mistakes that matter are a duplicate and a spelling the lexer
    /// does not know. The first would offer one name twice; the second would offer something that is not a word.
    #[test]
    fn every_keyword_is_a_real_spelling_and_appears_once() {
        let mut seen = std::collections::HashSet::new();
        for keyword in KEYWORDS {
            assert!(
                seen.insert(keyword.spelling),
                "`{}` is in the table twice",
                keyword.spelling
            );
            assert!(
                keyword
                    .spelling
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_'),
                "`{}` is not spelled like a keyword",
                keyword.spelling
            );
            assert!(
                !keyword.detail.is_empty(),
                "`{}` has nothing to show beside it",
                keyword.spelling
            );
        }
    }

    /// The `char8_t`/`char16_t`/`char32_t` family is **deliberately split** across the two tables, so a reader
    /// who wonders why three of the four are one thing and the fourth another finds the answer here: `wchar_t` is
    /// a keyword in this parser's lexer and the other three are read as names.
    #[test]
    fn the_builtin_type_names_are_the_ones_the_lexer_reads_as_names() {
        let mut seen = std::collections::HashSet::new();
        for (name, detail) in BUILTIN_TYPES {
            assert!(seen.insert(*name), "`{name}` is in the table twice");
            assert!(!detail.is_empty(), "`{name}` has nothing to show beside it");
            assert!(
                !KEYWORDS.iter().any(|keyword| keyword.spelling == *name),
                "`{name}` is a keyword and a built-in at once"
            );
        }
    }

    /// A statement keyword is offered **only** where a statement may be written, and the type keywords are
    /// offered everywhere — which is what makes `void` reachable inside an expression and `while` unreachable
    /// after a `.`.
    #[test]
    fn the_shape_decides_which_keywords_are_offered() {
        let in_a_statement: Vec<&str> = keywords_for(true, false).map(|it| it.spelling).collect();
        let in_a_type: Vec<&str> = keywords_for(false, false).map(|it| it.spelling).collect();

        assert!(in_a_statement.contains(&"while"));
        assert!(!in_a_type.contains(&"while"), "a loop is not a type");
        assert!(in_a_type.contains(&"void"));
        assert!(!in_a_type.contains(&"public"), "not in a class body");
        assert!(
            !in_a_statement.contains(&"public"),
            "and not outside one either"
        );

        let in_a_class: Vec<&str> = keywords_for(true, true).map(|it| it.spelling).collect();
        assert!(in_a_class.contains(&"public"));
    }

    /// Every snippet's body is a **shape**: it has a hole for the cursor, and it is more than the word that
    /// triggered it.
    ///
    /// The second half is the rule that decides what earns a place in the table at all — a snippet that inserted
    /// its own trigger would be a row that does nothing but take up space in the list — and `return` is the one
    /// that comes closest to breaking it (it writes `return value;`), which is exactly why the assertion is about
    /// the body being *longer* than the trigger rather than about it holding a newline.
    #[test]
    fn every_snippet_leaves_the_cursor_somewhere() {
        let mut seen = std::collections::HashSet::new();
        for snippet in SNIPPETS {
            assert!(
                seen.insert(snippet.trigger),
                "`{}` is in the table twice",
                snippet.trigger
            );
            assert!(
                snippet.body.contains("$0"),
                "`{}` leaves the cursor nowhere",
                snippet.trigger
            );
            assert!(
                snippet.body.len() > snippet.trigger.len(),
                "`{}` writes nothing its own spelling does not",
                snippet.trigger
            );
        }
    }

    /// A keyword a snippet already writes is **not** offered as a bare word beside it: two items with one label
    /// and different effects is exactly the choice a completion should not make the user make.
    #[test]
    fn a_snippet_shadows_the_keyword_it_writes() {
        assert!(a_snippet_writes_this("if"));
        assert!(a_snippet_writes_this("while"));
        assert!(!a_snippet_writes_this("const"));
    }

    #[test]
    fn every_directive_is_a_real_spelling_and_appears_once() {
        let mut seen = std::collections::HashSet::new();
        for directive in DIRECTIVES {
            assert!(
                seen.insert(directive.name),
                "`{}` is in the table twice",
                directive.name
            );
        }
    }
}
