//! **`assign-type-check`**: a value that does not convert to the type it is assigned to.
//!
//! ```cpp
//! int  count = "three";      // a string where an int is declared
//! bool ready = 3.5;          // no conversion from a floating-point value to `bool` in this language
//!
//! int other;
//! other = "four";            // the same question at an assignment, which is the second place a value enters an
//!                            // object whose type is written down
//! ```
//!
//! # The two places, which are one question
//!
//! ```text
//!   a declaration    `T x = expr;` and `T x{expr};`   the type is the declaration's, and its initialiser is here
//!   an assignment    `x = expr;`                      the type is the object's, and the type of the left-hand side
//!                                                     is what the model answers for `x`
//! ```
//!
//! Both end in the same comparison against [`Type::convertible_to`], so this is one rule rather than two.
//!
//! # What is not reported
//!
//! ```text
//!   not reported  `auto x = expr;`       a deduction question, not a conversion one
//!   not reported  `T x;`                 nothing is being converted
//!   not reported  a parameter            the argument's conversion is `param-type-check`'s question
//!   not reported  `a[i] = …`, `w.f = …`  the left-hand side is an expression rather than a name, and its type is a
//!                                        question about that expression — the model answers it, but as a type
//!                                        rather than as a declaration this check can name in a message
//!   not reported  `Unknown`              a class with a conversion constructor, a template, a type from a header
//!                                        nobody read: a conversion this layer cannot see is not a conversion the
//!                                        program does not have
//! ```
//!
//! # The pairing, and the cost that was measured for the wrong one
//!
//! Each initialiser is paired with the declarator written **beside it**, in one walk over the tree: the walk
//! collects the initialisers, and the name written immediately before one is the name being initialised. The check
//! this replaces did it the other way round — for every declaration, search the tree for its initialiser — and its
//! documentation records what that cost: **6 430 ms over twenty MSVC headers, where the other checks together cost
//! 396 ms**, 94% of the layer, paid per keystroke. One walk is what makes this version linear in the file.

use std::collections::HashMap;

use cpp_parser::{CppAstNode, CppSyntaxKind};

use super::{Check, CheckContext, DiagnosticCode};

/// **`assign-type-check`** — see the module documentation.
pub struct AssignTypeCheck;

impl Check for AssignTypeCheck {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::AssignTypeCheck];

    fn check(context: &mut CheckContext<'_>) {
        // **The written type of every variable this file declares**, by name. Built first so that the walk below is
        // one pass over the tree and one lookup per place a value is assigned — see the module documentation for
        // what the opposite order cost.
        let mut declared: HashMap<&str, &str> = HashMap::new();
        for fact in &context.summary.declarations {
            if fact.kind != crate::DeclKind::Variable {
                continue;
            }
            let Some(written) = fact.type_of.as_deref() else {
                continue;
            };
            if is_a_deduced_auto(written) {
                continue;
            }
            declared.insert(fact.name.as_str(), written);
        }
        if declared.is_empty() {
            // Nothing to judge: no variable in this file has a written type. The ordinary answer for a file that
            // only declares functions, and the reason this check costs nothing on one.
            return;
        }

        // **The declarations**: `T x = expr;`. The value is the initialiser's own expression, and the name is the
        // declarator written beside it.
        // **This file's own initialisers** - see CheckContext::own_syntax_nodes.
        for initializer in context
            .own::<cpp_parser::CppInitializer>()
            .map(|initializer| initializer.syntax().clone())
            .collect::<Vec<_>>()
        {
            // The punctuation is not the expression: `= expr`, `{expr}` and `(expr)` each leave one child that is
            // the value — and which kind of expression it is, is the parser's answer rather than a comparison of the
            // child's text against a list of punctuation marks. See [`cpp_parser::CppExpr`], which is that answer.
            let Some(expression) = initializer
                .children()
                .find(|child| cpp_parser::CppExpr::cast(child.clone()).is_some())
            else {
                continue;
            };

            // **The name written beside it, from the tree**: the initialiser's parent is the declarator it belongs
            // to, and the name is the last identifier inside that. This was a text scan — "the last identifier
            // before the `=`" — which is a decision made by looking at characters, and the check layer's own rule is
            // that a decision comes from nodes and tokens while text is for messages. It is also what produced the
            // false positive in `assignment_in_condition`: reading a language by its punctuation is how a `<` in a
            // condition became a bracket.
            let Some(declarator) = initializer
                .parent()
                .and_then(|holder| {
                    holder.children().find(|child| {
                        CppSyntaxKind::from(child.kind()) == CppSyntaxKind::Declarator
                    })
                })
                .or_else(|| initializer.parent())
            else {
                continue;
            };
            let Some(name) = super::spelling_of(&declarator) else {
                continue;
            };
            let Some(written) = declared.get(name.as_str()).copied() else {
                continue;
            };

            // **A parameter's default value is not this check's question.** Measured on EmmyLuaCodeStyle:
            // `Range(Position _start = Position(0, 0), Position _end = Position(0, 0));` produced two findings — the
            // initialiser is a default argument, and what a default may be is the parameter's declaration rather than a
            // variable's own type.
            if declarator.ancestors().any(|node| {
                cpp_parser::CppSyntaxKind::from(node.kind())
                    == cpp_parser::CppSyntaxKind::ParameterList
            }) {
                continue;
            }

            // **A construction is not a conversion to check.** Measured on EmmyLuaCodeStyle:
            // `LineSpace line_space_after_if_statement = LineSpace(LineSpaceType::Keep);` produced ten findings. The
            // initialiser is a call to the type's **own name**, and what the type engine answers for that call is not
            // the type it constructs — so there is nothing here for this rule to compare.
            if constructs_the_written_type(&expression, written) {
                continue;
            }

            report_if_it_does_not_convert(context, &expression, written, &name, "initialised");
        }

        // **And the assignments**: `x = expr;`. Which object it is comes from the left-hand side being **a name and
        // nothing else** — `a[i] = …` and `w.field = …` are questions about those expressions rather than about a
        // declaration this check can name.
        //
        // **Typed, over this file's own text**, and read through the typed layer's own accessors: `get_lhs`, `get_rhs`
        // and `get_operator_text` are what `CppBinaryExpr` is for.
        //
        // The version before this one took the operands from `children()` and came out with **none**: measured, for
        // `count = "four"` the probe printed `equals=true sides=[]`, because a simple operand is a *token* in this
        // tree rather than a node. A hand-rolled shape reader was wrong again; the accessor was right all along.
        for assignment in context
            .own::<cpp_parser::CppBinaryExpr>()
            .collect::<Vec<_>>()
        {
            if assignment.get_operator_text().as_deref() != Some("=") {
                continue;
            }
            let Some(right) = assignment.get_rhs() else {
                continue;
            };

            // **The left side is a bare name, asked of the AST and not of the text.**
            //
            // This was `last_identifier_of(left_text)` plus `left_text != name` — a hand-written scanner for
            // identifiers, in a crate that has a lexer, and a comparison of characters to decide a question about
            // **shape**: *is the left operand a plain name rather than `a[i]` or `s.f`?* Text cannot answer that
            // reliably. `arr[i]` trims to `arr[i]`, which is not an identifier, so it happened to be excluded — but
            // `a.b` and `(a)` and `a ` are all questions the characters answer differently from the grammar, and the
            // scan itself is the thing `MacroFact::body_range`'s note names: *"a search is a second implementation of
            // the same rule, free to disagree with the one that assigned the name"*.
            //
            // The typed layer answers it directly: a plain name is a [`CppExpr::NameExpr`]. The name is then the
            // token the parser put there, not a substring this layer carved out.
            let Some(cpp_parser::CppExpr::NameExpr(named)) = assignment.get_lhs() else {
                continue;
            };
            let Some(name) = super::spelling_of(named.syntax()) else {
                continue;
            };
            let Some(written) = declared.get(name.as_str()).copied() else {
                continue;
            };

            report_if_it_does_not_convert(context, right.syntax(), written, &name, "assigned");
        }
    }
}

/// **A type the compiler deduces is not a conversion this check may judge** — `auto`, and every spelling of it.
///
/// The first version compared the written type against exactly `"auto"` and `"decltype(auto)"`, so `auto&`, `const
/// auto&` and `auto&&` were judged as *written* types: reported from an editor, `auto& config =
/// configs.emplace_back(…)` was underlined as "`config` is initialised with a value that does not convert to
/// `auto&`", which is a claim about a type that is not the type — `auto&` is a question for the initialiser.
/// The decorations are part of the deduction, not part of a type: `const`, `volatile`, `&`, `&&` and `*` are what
/// `auto` is being deduced *from*.
fn is_a_deduced_auto(written: &str) -> bool {
    let cleaned: String = written
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    let cleaned = cleaned
        .replace("const", "")
        .replace("volatile", "")
        .trim_end_matches(['&', '*'])
        .to_string();
    cleaned == "auto" || cleaned == "decltype(auto)"
}

/// **One place a value enters an object whose type is written down**, reported when it does not convert.
///
/// The single comparison both halves end in — see the module documentation — so that the two cannot drift into
/// having different opinions about the same conversion.
fn report_if_it_does_not_convert(
    context: &mut CheckContext<'_>,
    expression: &cpp_parser::CppSyntaxNode,
    written: &str,
    name: &str,
    how: &str,
) {
    // **`0` is the null pointer constant.** It converts to any pointer, to `std::nullptr_t`, and to every smart pointer whose
    // constructor takes one — and the conversion relation cannot say so, because it lives in the type algebra and never sees a
    // declaration. The census found this as the only shape its `assign-type-check` examples shared: `0` into `scoped_ptr`,
    // `op_queue` and `object_pool` on a real project, every one of them valid C++.
    // **A literal node, not a trimmed string.** `0` is the null pointer constant, and the parser has a node for a literal —
    // its text is its token, so this cannot see a `0` inside a comment or a `"0"` string, which the text form could.
    if cpp_parser::CppLiteralExpr::cast(expression.clone())
        .is_some_and(|literal| literal.syntax().text().to_string().trim() == "0")
    {
        return;
    }
    let known_from = context.type_of(expression);
    let target = crate::sema::types::parse_type_spelling(written);
    let crate::Known::Yes((from, _)) = known_from else {
        return;
    };
    // **One facility, shared with `param-type-check`** — `sema::convert` normalises the spelling (a `const std::string&`
    // assignment target is `Reference { to: Named(..) }`, and `class_name()` answers `None` for a reference), asks the type
    // relation first and the index only for the class question, and answers `Unknown` — silence — when neither can speak.
    match crate::sema::convert::conversion(&from, written, |class| {
        super::a_class_can_be_built_from(context, class, &from)
    }) {
        crate::sema::convert::Conversion::Refused => {
            report(context, expression, name, how, written);
            return;
        }
        _ => {}
    }

    // **The class case, which the conversion relation cannot answer.** `convertible_to` lives in the type engine and
    // cannot see a declaration, so a class target always answers `Unknown` — the honest answer to "can this convert",
    // and the *wrong* answer to "does this class have a constructor that could take this".
    //
    // The question therefore goes to the **index**: `a_class_can_be_built_from` reads the class's member facts, whose
    // `parameter_list` is the parameter list as the declaring file spells it. That field exists for exactly this — a
    // query holds one file and the parameters are written in another — so the answer is a fact the cook already
    // produced rather than a walk over a rendered header. A class from `<string>` is as answerable as one written two
    // lines up, which is the point: **the finding is about this file, the knowledge is the project's.**
    if super::a_class_can_be_built_from(context, &target.class_name().unwrap_or_default(), &from)
        == Some(false)
    {
        report(context, expression, name, how, written);
    }
}

/// Put the finding where every other one goes.
fn report(
    context: &mut CheckContext<'_>,
    expression: &cpp_parser::CppSyntaxNode,
    name: &str,
    how: &str,
    written: &str,
) {
    context.add(
        DiagnosticCode::AssignTypeCheck,
        cpp_parser::source_range(expression.text_range()),
        name.to_string(),
        t!(
            "`%{name}` is %{how} with a value that does not convert to `%{written}`",
            name = name,
            how = how,
            written = written
        ),
    );
}

/// **Is this expression a call to the very type it is assigned to?** — `LineSpace(…)` for a `LineSpace`.
///
/// The last component of a qualified spelling is what a construction writes: `std::string(…)` for a `std::string`.
fn constructs_the_written_type(expression: &cpp_parser::CppSyntaxNode, written: &str) -> bool {
    if cpp_parser::CppSyntaxKind::from(expression.kind()) != cpp_parser::CppSyntaxKind::CallExpr {
        return false;
    }
    let Some(callee) = expression.children().next() else {
        return false;
    };
    let Some(spelled) = super::spelling_of(&callee) else {
        return false;
    };
    let target = written.rsplit("::").next().unwrap_or(written).trim();
    spelled.trim() == written.trim() || spelled.trim() == target
}
