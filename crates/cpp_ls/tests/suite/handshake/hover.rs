//! Hover cards: types, members, documentation, macros and format strings.

use super::fixtures::*;
use super::support::*;

///
/// Two reports, one card each:
///
/// * a class template's card used to show only the **declaration's** parameter names (`map<_Kty, _Ty, _Pr, _Alloc>`),
///   because it was rebuilt from the fact — while the reader is looking at `std::map<const char*, int, std::less<>>`
///   and asking what *that* is;
/// * `static const struct { … } priority[]` was answered `void[]`, because the anonymous class body contributed no
///   name and the specifier reader fell back to `void`. The honest answer is that this layer cannot spell the type,
///   and the card then shows the declaration's own text — which is the only place the reader can see it.
#[test]
fn a_hover_names_the_instantiation_and_never_invents_a_void() {
    let project = Project::new("hover-instantiation");
    project.write("map.h", INSTANTIATION_HEADER);
    project.write("main.cpp", INSTANTIATION_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": INSTANTIATION_CPP }
        }),
    );

    let instantiation = hover_over(
        &mut server,
        100,
        &main_uri,
        INSTANTIATION_CPP,
        "map<const char*, int, Less>",
    );
    let text = instantiation["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {instantiation}"));
    assert!(
        text.contains("std::map<const char*, int, Less>"),
        "the use's own spelling, not the declaration's parameter names: {text}"
    );
    assert!(
        !text.contains("map<_Kty, _Ty"),
        "and not the shape the header declares: {text}"
    );

    let anonymous = hover_over(&mut server, 200, &main_uri, INSTANTIATION_CPP, "priority");
    let text = anonymous["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {anonymous}"));
    assert!(
        !text.contains("void"),
        "an anonymous struct's variable is not `void`: {text}"
    );
    // **The card is generated, not the declaration's text.** It used to be the file's own spelling of the anonymous
    // struct — the only way to show `unsigned char` for a type with no name — and that is exactly the copy-paste the
    // card must not do: the type has no name to spell, so the card shows the name and says nothing it did not read.
    assert!(
        !text.contains("unsigned char"),
        "the card does not echo the struct's body: {text}"
    );
    assert!(
        text.contains("priority"),
        "and it is still about the name the cursor is on: {text}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **Hover shows what the file wrote about a declaration — and only what it wrote as documentation.**
///
/// Three positions in one fixture, because the interesting part is the *boundary*: the documented declaration
/// carries its comment (its `@param`s rendered as a list, not as markers in a paragraph — see
/// `render_documentation`), the one under a plain `//` comment
/// carries none — a `// TODO` dressed as an interface description is the failure the parser's own classification
/// exists to prevent — and `this`, which names no declaration at all, is answered by the expression question
/// instead: the class it points at.
#[test]
fn hover_shows_the_documentation_and_refuses_a_plain_comment() {
    let project = Project::new("hover");
    project.write("main.cpp", HOVER_CPP);

    let mut server = Server::start_within(project.root(), STD_LIBRARY_TIMEOUT);
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": HOVER_CPP }
        }),
    );

    let documented = hover_over(&mut server, 100, &main_uri, HOVER_CPP, "add(int a, int b);");
    let text = documented["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {documented}"));
    assert!(
        text.contains("Adds two counts."),
        "the comment above the declaration is the documentation: {text}"
    );
    assert!(
        text.contains("**`a`** — the first") && text.contains("**`b`** — the second"),
        "each parameter is a list item, named and described: {text}"
    );
    assert!(
        !text.contains("@param"),
        "and the markers are no longer in the text: {text}"
    );
    assert!(
        text.contains("add(int a, int b)"),
        "with the declaration itself: {text}"
    );

    let plain = hover_over(
        &mut server,
        200,
        &main_uri,
        HOVER_CPP,
        "undocumented(int c);",
    );
    let text = plain["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {plain}"));
    assert!(
        !text.contains("A note to the reader"),
        "a plain `//` comment is not documentation: {text}"
    );
    assert!(
        text.contains("undocumented(int c)"),
        "the declaration is still what the hover is about: {text}"
    );

    let this = hover_over(&mut server, 300, &main_uri, HOVER_CPP, "this->scaled(2)");
    let text = this["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected for `this`, got {this}"));
    assert!(
        text.contains("enclosing class `Widget`"),
        "`this` names no declaration, so the expression question answers: {text}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **Hover on a UE member shows the reflection annotation it was written under.**
///
/// `UPROPERTY(EditAnywhere, Category = "Game")` is an empty macro: the tree has nothing of it, and the card a reader got
/// for a reflected member said only `bool bIsReady` — while the annotation is the whole reason the engine's build tool
/// does anything with the declaration. The fact carries it (read from the file's text), and the card shows it as the file
/// wrote it.
#[test]
fn hover_shows_the_unreal_reflection_annotation() {
    let project = Project::new("unreal-hover");
    project.write(".cppls.toml", "[unreal]\nenable = true\n");
    // **The macros come from a header, the way an engine's do** — the same shape the simulated engine has, and the one
    // this feature was measured on: the annotation is left standing in the file's own text, which is what the summary
    // reads, while the reading a compiler parses has it expanded away.
    project.write(
        "engine.h",
        "#pragma once\n\n#define UCLASS(...)\n#define UPROPERTY(...)\n#define GENERATED_BODY()\n",
    );
    const ACTOR_H: &str = "\
#include \"engine.h\"

UCLASS(Blueprintable)
class AMyActor
{
    GENERATED_BODY()

    UPROPERTY(EditAnywhere, Category = \"Game\")
    bool bIsReady = false;
};
";
    project.write("actor.h", ACTOR_H);

    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("actor.h"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {},
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": ACTOR_H } }),
    );

    let hover = hover_at(&mut server, 100, &uri, ACTOR_H, "AMyActor", 1);
    let text = hover["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {hover}"));
    assert!(
        text.contains("UCLASS(Blueprintable)"),
        "the annotation, as the file wrote it: {text}"
    );
    assert!(
        text.contains("AMyActor"),
        "and the declaration is still what the card is about: {text}"
    );

    server.request(200, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A hover on a call shows only the declarations that call can reach.**
///
/// Reported from an editor: hovering `append` in `result_str.append(x)` drew **nine** `basic_string::append`
/// declarations, several of which cannot accept one argument at all. The arity rule asked only "no more arguments than
/// parameters" — a longer list *may* have defaults — so every two- and three-parameter overload survived it. The
/// defaulted count is in the declaration's own `parameter_list` spelling, which the hover holds, so it is counted now.
///
/// The fixture separates the two results the resolution can give: `f(1)` has exact types for the one- and two-parameter
/// overloads, and "fewer parameters used is better" then selects `f(int)`, so **one** line is printed; `g(unknown)`
/// makes the argument's type unknown, so the two one-parameter overloads tie and **both** are printed — and the
/// two-parameter `g` is not, which is what the arity filter is for.
#[test]
fn a_hover_on_a_call_shows_only_the_declarations_it_can_reach() {
    const SHAPES_H: &str = "\
struct S {
    void f(int a);
    void f(int a, int b = 0);
    void f(int a, int b, int c);

    void g(int a);
    void g(long a);
    void g(int a, int b);
};
";
    const SHAPES_CPP: &str = "\
#include \"shapes.h\"
void h(S s) {
    s.f(1);
    s.g(unknown_name);
}
";

    let project = Project::new("hover-viable-overloads");
    project.write("shapes.h", SHAPES_H);
    project.write("main.cpp", SHAPES_CPP);

    let mut server = Server::start_within(project.root(), STD_LIBRARY_TIMEOUT);
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": SHAPES_CPP }
        }),
    );

    let hovered = hover_over(&mut server, 100, &main_uri, SHAPES_CPP, "f(1)");
    let text = hovered["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {hovered}"));
    assert!(
        text.contains("void f(int a)") && text.contains("S::f"),
        "the call selects the one-parameter overload, and the card is that member's own: {text}"
    );
    assert!(
        !text.contains("int b"),
        "and the overloads that need more arguments are not shown: {text}"
    );

    let hovered = hover_over(&mut server, 200, &main_uri, SHAPES_CPP, "g(unknown_name)");
    let text = hovered["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {hovered}"));
    assert!(
        text.contains("void g(int a)") && text.contains("void g(long a)"),
        "both one-parameter overloads accept an unknown argument: {text}"
    );
    assert!(
        !text.contains("int a, int b"),
        "the two-parameter overload cannot take one argument: {text}"
    );
}

#[test]
fn the_overload_a_call_means_is_marked_in_the_hover_list() {
    const OVERLOADS_H: &str = "int parse(int value);\nint parse(const char* text);\n";
    const OVERLOADS_CPP: &str = "#include \"overloads.h\"\nint f() { return parse(1); }\n";

    let project = Project::new("hover-overloads");
    project.write("overloads.h", OVERLOADS_H);
    project.write("main.cpp", OVERLOADS_CPP);

    let mut server = Server::start_within(project.root(), STD_LIBRARY_TIMEOUT);
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": OVERLOADS_CPP }
        }),
    );

    let hovered = hover_over(&mut server, 100, &main_uri, OVERLOADS_CPP, "parse(1)");
    let text = hovered["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {hovered}"));

    assert!(
        text.contains("→ "),
        "the overload this call means is marked: {text}"
    );
    let marked = text
        .lines()
        .find(|line| line.trim_start().starts_with("→ "))
        .unwrap_or_default()
        .to_string();
    assert!(
        marked.contains("int value"),
        "and it is the `int` one, because the argument is an `int`: {marked:?} of\n{text}"
    );
}

/// **What the popup says, for every shape a declaration comes in** — printed, not asserted.
///
/// A hover is the one answer a reader judges by how it *looks*, and no assertion states that well. This exists so
/// the shapes can be read side by side while the rendering is being changed: a template, a member function with a
/// body, a documented class, a variable the file gave no type to, and a member of a class template from another
/// file. Ignored by default — run it with `--ignored --nocapture`.
#[test]
#[ignore = "a reading aid: it prints popups rather than checking them"]
fn what_the_popups_look_like() {
    let project = Project::new("hover-shapes");
    project.write(
        "box.h",
        "#pragma once\n\
         /// A box that holds one `T`.\n\
         ///\n\
         /// The documentation is the file's own words.\n\
         template <class T>\n\
         class Box {\n\
         public:\n\
             /// What is in the box.\n\
             T get() const { return v; }\n\
             T v;\n\
         };\n",
    );
    project.write(
        "main.cpp",
        "#include \"box.h\"\n\
         \n\
         int main() {\n\
             auto n = 1;\n\
             Box<int> b;\n\
             b.v = n;\n\
             return 0;\n\
         }\n",
    );

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));
    let main_text = "#include \"box.h\"\n\nint main() {\n    auto n = 1;\n    Box<int> b;\n    b.v = n;\n    \
                     return 0;\n}\n";

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "textDocument": { "hover": {} } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": main_text }
        }),
    );

    // (line, character, what is being hovered) — 0-based, as a client sends them.
    let asked: [(u64, u64, &str); 5] = [
        (
            3,
            9,
            "the local `n`, whose type is an `auto` the file wrote",
        ),
        (
            4,
            4,
            "`Box` in `Box<int> b;` — a class template from a header",
        ),
        (5, 6, "the member `v` of that class template"),
        (4, 13, "the variable `b`, whose type is that class template"),
        (1, 4, "`main` itself, a function this file defines"),
    ];

    for (line, character, what) in asked {
        let mut answer = server.ask_until_it(
            100,
            |id| {
                json!({
                    "id": id,
                    "method": "textDocument/hover",
                    "params": {
                        "textDocument": { "uri": main_uri },
                        "position": { "line": line, "character": character },
                    },
                })
            },
            |answer| !answer["result"].is_null(),
        );
        // Asked more than once so the server has had the turns it needs to read the file the way a client that
        // waited gets it — see the note in the coordinate test above.
        for _ in 0..4 {
            answer = server.ask_until_it(
                100,
                |id| {
                    json!({
                        "id": id,
                        "method": "textDocument/hover",
                        "params": {
                            "textDocument": { "uri": main_uri },
                            "position": { "line": line, "character": character },
                        },
                    })
                },
                |answer| !answer["result"].is_null(),
            );
        }
        let markdown = answer["result"]["contents"]["value"]
            .as_str()
            .unwrap_or("<no popup>")
            .to_string();
        println!("=== {line}:{character} — {what} ===\n{markdown}\n");
    }
}

/// **A macro**的 hover 显示的是它自己的 `#define` 行，而不是一段无关的代码。
///
/// 报告来自编辑器:悬停 `#define STD std::` 里的 `STD` 时,弹窗里是**一大段 `main` 的函数体**。
/// 原因是坐标用错了地方:事实(`DeclFact`/`MacroFact` 的 range)按**文件**坐标记录,而当时的实现拿它去切
/// **渲染**(指令和注释都已被删掉,长度短得多),于是偏移落到了程序中间,再往后切到 body 的末尾。
///
/// 断言两件事,因为两件都是这个 bug 的两面:弹窗**包含** `#define STD std::`,且**不含**任何 `int main`。
#[test]
fn a_macro_hover_shows_its_own_define_line() {
    let project = Project::new("macro-hover");
    // **A `#   define` with whitespace after the `#`**, the shape neovim's CRT headers use. The directive scanner has
    // to index it or the use below has no macro fact to hover.
    let source = "#define FULL_NAME_FORMAT \"Full name: {}\"\n#   define STD std::\n\nint main() {\n    STD c;\n    return 0;\n}\n";
    project.write("main.cpp", source);

    let mut server = Server::start_within(project.root(), STD_LIBRARY_TIMEOUT);
    let main_uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "textDocument": { "hover": {} } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": source }
        }),
    );

    // The **use** of `STD` on line 4 (0-based), which is where a reader hovers to ask what it stands for.
    let hovered = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": main_uri },
                "position": { "line": 4, "character": 5 },
            },
        })
    });

    let value = hovered["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("the macro has a popup: {hovered}"));
    println!("hover: {value}");
    assert!(
        value.contains("define") && value.contains("STD") && value.contains("std::"),
        "the popup is the macro's own definition line: {value}"
    );
    assert!(
        !value.contains("int main"),
        "and not a slice of the program starting somewhere else: {value}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A hover inside a format string explains the bytes under the cursor.**
///
/// The other thing a reader points at that no declaration describes: `%zu` is a `size_t` in decimal, `{:.3f}` is
/// fixed-point with three digits, and the only place a reader is already looking is the line of code. The words come
/// from `ormat::describe` — the module that decides which bytes are a *piece* — so the explanation cannot
/// disagree with the colouring.
///
/// Three assertions, in the order the feature is built:
///
/// ```text
///   inside a printf conversion       the type it converts, and the flags/width/precision beside it
///   inside a std::format field       the specification's own parts
///   inside a plain string            nothing — a string is not a format string unless a call says it is
/// ```
#[test]
fn a_hover_inside_a_format_string_explains_the_part_it_is_on() {
    const FIXTURE: &str = "\
#include <cstdio>
struct width { int value; };
void f(double d, int n) {
    std::printf(\"%-8.2f|%zu\", d, n);
    const char *plain = \"not a format string %d\";
    (void)plain;
}
";
    let project = Project::new("hover-format");
    project.write("main.cpp", FIXTURE);

    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("main.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": FIXTURE }
        }),
    );

    // **A position on a byte of a spelling**, found in the fixture rather than counted.
    let at = |needle: &str, within: usize| -> (u32, u32) {
        let line = line_of(FIXTURE, needle);
        let offset = FIXTURE
            .lines()
            .nth(line as usize)
            .and_then(|text| text.find(needle))
            .unwrap_or_else(|| panic!("the fixture writes {needle:?}"))
            + within;
        (line as u32, offset as u32)
    };
    let ask = |server: &mut Server, id: i64, (line, character): (u32, u32)| -> String {
        let answer = server.ask_until(id, |id| {
            json!({
                "id": id,
                "method": "textDocument/hover",
                "params": {
                    "textDocument": { "uri": uri },
                    "position": { "line": line, "character": character },
                },
            })
        });
        answer["result"]["contents"]["value"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };

    // Inside the `f` of `%.2f` — the conversion, and the sentence a reader wants.
    let text = ask(&mut server, 100, at("%-8.2f", 5));
    assert!(
        text.contains("fixed-point decimal"),
        "the conversion's meaning: {text:?}"
    );

    // Inside the width: the grammar's own words for the numbers in front of the letter.
    let text = ask(&mut server, 101, at("%-8.2f", 2));
    assert!(
        text.contains("width") && text.contains('8'),
        "the width: {text:?}"
    );
    assert!(
        text.contains("left-aligned"),
        "and the flag before it: {text:?}"
    );

    // A string that no call passes anywhere is not a format string, so there is no popup for its `%d`.
    let text = ask(&mut server, 102, at("not a format string", 17));
    assert!(
        !text.contains("conversion"),
        "a string nothing formats is left to the reader: {text:?}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **A documentation comment's symbol reference works end to end** — Rust's ``[`Widget::grow`]``, over the wire.
///
/// The three layers are tested apart (`cpp_parser::doc_links`, the analysis's colouring, and the two handlers), and
/// this is the one that says the pair an editor actually exercises — a hover and a jump — answer for a position
/// **inside a comment**, which is where every other declaration query in the server answers about the token *after*
/// it. It is also the check that the two handlers agree: both resolve the target with `ProjectIndex::definition`, so
/// a card that has a declaration and a jump that has none would be a defect this catches.
#[test]
fn a_documentation_link_hovers_and_jumps_over_the_wire() {
    const SOURCE: &str = "\
struct Widget {
    /// See [`Widget::grow`] for the rule.
    void grow(int by);
};
";
    let project = Project::new("doc-link");
    project.write("a.cpp", SOURCE);

    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("a.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {},
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": SOURCE }
        }),
    );

    // `/// See [` is columns 0–9, so the target starts at character 10 on line 1.
    let hover = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/hover",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 1, "character": 14 } },
        })
    });
    let card = hover["result"]["contents"]["value"]
        .as_str()
        .or_else(|| hover["result"]["contents"].as_str())
        .unwrap_or_else(|| panic!("a card was expected for the link, got {hover}"));
    assert!(
        card.contains("void grow(int by)"),
        "the card is the declaration the link names: {card}"
    );

    let location = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/definition",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 1, "character": 14 } },
        })
    });
    assert!(
        location["result"]["uri"]
            .as_str()
            .is_some_and(|found| found.ends_with("a.cpp")),
        "the jump names the file that declares it: {location}"
    );
    assert_eq!(
        location["result"]["range"]["start"]["line"],
        json!(2),
        "and the line of the declaration, which is below the comment: {location}"
    );
    assert_eq!(
        location["result"]["range"]["start"]["character"],
        json!(9),
        "and the column of the name: {location}"
    );

    // --- and the colour ---------------------------------------------------------------------------
    //
    // A comment is trivia, so the token pass cannot find a link in the tree: the name is read from the file's own
    // text and resolved by spelling, and it arrives in the same delta-encoded stream as every other name. A client
    // that declares no `tokenTypes` is sent the server's whole legend, which is what this request relies on.
    let tokens = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/semanticTokens/full",
            "params": { "textDocument": { "uri": uri } },
        })
    });
    let data = tokens["result"]["data"]
        .as_array()
        .unwrap_or_else(|| panic!("a token stream was expected, got {tokens}"));

    // Decode the delta walk: five numbers per token — `deltaLine`, `deltaStart`, `length`, `type`, `modifiers`.
    let mut line = 0i64;
    let mut character = 0i64;
    let mut length_at_the_link = 0i64;
    let mut at_the_link = None;
    for token in data.chunks(5) {
        let delta_line = token[0].as_i64().unwrap_or(0);
        let delta_start = token[1].as_i64().unwrap_or(0);
        line += delta_line;
        character = if delta_line == 0 {
            character + delta_start
        } else {
            delta_start
        };
        if line == 1 && character == 14 {
            length_at_the_link = token[2].as_i64().unwrap_or(0);
            at_the_link = token[3].as_i64();
        }
    }
    assert_eq!(
        length_at_the_link,
        "Widget::grow".len() as i64,
        "the token covers exactly the name between the backticks: {data:?}"
    );
    assert!(
        at_the_link.is_some(),
        "and it starts where the target does (line 1, character 14): {data:?}"
    );
}

/// **A documentation comment's prose offers no code**, and its ``[`…`]`` offers names — asked over the wire.
///
/// Reported from an editor: typing in a `///` comment produced the file's classes and functions as completions.
/// The answer has to be "nothing" in prose — a comment is not a place a C++ name goes — while a symbol reference is
/// exactly the place one does, which is why the two positions are asked separately here.
#[test]
fn a_documentation_comment_offers_code_only_inside_a_link() {
    const SOURCE: &str = "\
struct Widget { int grow; };
/// Hello w
/// See [`Wid
void f() {}
";
    let project = Project::new("doc-complete");
    project.write("a.cpp", SOURCE);

    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("a.cpp"));

    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": { "textDocument": { "completion": { "completionItem": { "snippetSupport": false } } } },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "cpp", "version": 1, "text": SOURCE }
        }),
    );

    // --- prose: `/// Hello w` — the cursor is at the end of the line ------------------------------------------
    let prose = server.ask_until(100, |id| {
        json!({
            "id": id,
            "method": "textDocument/completion",
            "params": { "textDocument": { "uri": uri }, "position": { "line": 1, "character": 11 } },
        })
    });
    let empty: Vec<serde_json::Value> = Vec::new();
    let items = prose["result"]["items"].as_array().unwrap_or(&empty);
    assert!(
        items.is_empty(),
        "a comment's prose is not a place a code name goes, and this offered {} item(s): {:?}",
        items.len(),
        items
            .iter()
            .filter_map(|item| item["label"].as_str())
            .take(8)
            .collect::<Vec<_>>()
    );

    // --- inside the link: `/// See [`Wid` — a name belongs here --------------------------------
    let link = server.ask_until_it(
        100,
        |id| {
            json!({
                "id": id,
                "method": "textDocument/completion",
                "params": { "textDocument": { "uri": uri }, "position": { "line": 2, "character": 13 } },
            })
        },
        |response| {
            response["result"]["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item["label"] == json!("Widget")))
        },
    );
    let items = link["result"]["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        items.iter().any(|item| item["label"] == json!("Widget")),
        "a symbol reference completes the project's names: {link}"
    );
    // …and it completes **names**, not statements: a link takes a symbol, and a snippet is not one.
    for snippet in ["class", "dowhile", "forr", "function"] {
        assert!(
            !items.iter().any(|item| item["label"] == json!(snippet)),
            "`{snippet}` is a snippet, and a reference wants a name: {link}"
        );
    }
}

/// **A function declared in a header hovers as its declaration, not as the type of the call.**
///
/// The chain this pins is the one a reader lives in: a header declares
/// `consteval string_view identifier_of(info);` — a **typedef return type** and one argument that names a type — and
/// the `.cpp` that includes it calls it. Two defects met here and both were measured on a real project:
///
/// * the parser read `string_view f(info);` as a *variable* with a direct-initialiser (the most vexing parse is settled
///   by lookup, and it only asked whether the declaration's own type was a keyword), so the header's fact said
///   `variable`, and hover on the call fell through to the expression's type — "`std::meta::identifier_of` — has type
///   `string_view`", which is the card that was reported as "并非对应的函数";
/// * and the card for a declaration the summary had not yet reached printed `name_of(…)` with no return type and no
///   parameter list.
///
/// The test is a full handshake — initialize, didOpen, `textDocument/hover` — so it reads what an editor reads, on the
/// written text, with the header reached through the index rather than through a rendering.
#[test]
fn a_hover_on_a_header_declared_function_shows_the_declaration() {
    const TYPES_H: &str = "\
namespace lib {
using string_view = const char *;
struct info {};
consteval string_view identifier_of(info);
constexpr int count_of(info, int);
}
";
    const MAIN_CPP: &str = "\
#include \"types.h\"

int main() {
    auto name = lib::identifier_of(lib::info{});
    auto count = lib::count_of(lib::info{}, 1);
    return 0;
}
";

    let project = Project::new("hover-header-declaration");
    project.write("types.h", TYPES_H);
    project.write("main.cpp", MAIN_CPP);

    let mut server = Server::start(project.root());
    let main_uri = uri_of(&project.root().join("main.cpp"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {
                "workspace": { "configuration": true, "didChangeWatchedFiles": { "dynamicRegistration": true } },
                "window": { "workDoneProgress": true },
            },
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": main_uri, "languageId": "cpp", "version": 1, "text": MAIN_CPP }
        }),
    );

    let answer = hover_over(&mut server, 100, &main_uri, MAIN_CPP, "identifier_of");
    let text = answer["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {answer}"));
    assert!(
        text.contains("consteval string_view identifier_of(info)"),
        "the declaration, with its return type and parameter list: {text}"
    );
    assert!(
        !text.contains("has type"),
        "a function's card is never the type of the call: {text}"
    );

    // **A parameter list with more than one element** — the shape a caller-argument count could confuse.
    let answer = hover_over(&mut server, 200, &main_uri, MAIN_CPP, "count_of");
    let text = answer["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("a markdown answer was expected, got {answer}"));
    assert!(
        text.contains("constexpr int count_of(info, int)"),
        "both parameters, spelled as the header writes them: {text}"
    );
}

/// **A macro defined under `#if !defined(NAME)` still gets its `#define` card.**
///
/// This is the system-header idiom, and neovim's `ENABLE_VIRTUAL_TERMINAL_PROCESSING` uses it in the file that
/// consumes it. The strict macro query refuses the name because which `#define` is in force depends on an earlier
/// header; a reader pointing at the use still wants the replacement list, so the card shows it and names the doubt.
#[test]
fn a_macro_defined_under_if_not_defined_gets_its_define_card() {
    const SOURCE: &str = "\
#if !defined(ENABLE_VIRTUAL_TERMINAL_PROCESSING)
# define ENABLE_VIRTUAL_TERMINAL_PROCESSING 0x0004
#endif
int mode = ENABLE_VIRTUAL_TERMINAL_PROCESSING;
";
    let project = Project::new("conditional-macro-hover");
    project.write("a.c", SOURCE);
    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("a.c"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {},
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "c", "version": 1, "text": SOURCE }
        }),
    );

    let answer = hover_at(
        &mut server,
        100,
        &uri,
        SOURCE,
        "ENABLE_VIRTUAL_TERMINAL_PROCESSING",
        1,
    );
    let value = answer["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("the macro has a popup: {answer}"));
    assert!(
        value.contains("0x0004"),
        "the replacement list is in the card: {value}"
    );
    assert!(
        !value.contains("a macro whose `#define` is inside a conditional block"),
        "the generic refusal is not the answer when a definition is available: {value}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}

/// **The same card for a macro guarded in a header**, which is the shape of `LR_LOADFROMFILE`.
///
/// The definition is not in the buffer, so `macro_definition_text` cannot find it; the index's candidate carries the
/// defining file and body range, and the card reads the replacement list from that file.
#[test]
fn a_macro_defined_in_a_guarded_header_gets_its_define_card() {
    const HEADER: &str = "\
#ifndef LR_LOADFROMFILE
# define LR_LOADFROMFILE 0x00000010
#endif
";
    const SOURCE: &str = "\
#include \"winuser.h\"
int flags = LR_LOADFROMFILE;
";
    let project = Project::new("conditional-header-macro-hover");
    project.write("winuser.h", HEADER);
    project.write("main.c", SOURCE);
    let mut server = Server::start(project.root());
    let uri = uri_of(&project.root().join("main.c"));
    server.request(
        1,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": uri_of(project.root()),
            "capabilities": {},
        }),
    );
    server.notify("initialized", json!({}));
    server.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": { "uri": uri, "languageId": "c", "version": 1, "text": SOURCE }
        }),
    );

    let answer = hover_at(&mut server, 100, &uri, SOURCE, "LR_LOADFROMFILE", 1);
    let value = answer["result"]["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("the macro has a popup: {answer}"));
    assert!(
        value.contains("0x00000010"),
        "the header's replacement list is in the card: {value}"
    );

    server.request(999, "shutdown", Value::Null);
    server.notify("exit", Value::Null);
}
