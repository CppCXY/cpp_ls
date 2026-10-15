//! **`argument-count`**: a call that passes more arguments than the callee takes, or fewer than it needs.
//!
//! ```cpp
//! int scale(int count, double factor);
//!
//! scale(1, 2.0);        // fine
//! scale(1);             // one argument for two parameters
//! scale(1, 2.0, 3);     // three for two
//! ```
//!
//! # Only a callee this file declares, and that is a fact about the analysis rather than a choice
//!
//! The parameter list of a function declared **here** is in this file's tree, and this check reads it there. For a
//! callee declared elsewhere the honest answer would need that file's tree, which a check is not given — see
//! [`CheckContext`]'s note on why it is handed a model and not a session — so a call into a header is **not
//! judged at all**, however wrong it looks.
//!
//! The field that looks like it would answer this — [`DeclFact::parameters`](crate::DeclFact::parameters) — must
//! not be used: it is the **template** parameter list (its own documentation says "a partial spelling of the
//! template parameters"), and the check this one replaces was measured reading it: on 200 MSVC headers it reported
//! **119 findings**, every one of them a function whose `parameters` was empty and whose real parameters were in
//! the tree all along.
//!
//! # `...`, defaults, and the two directions
//!
//! ```text
//!   too many     only when the callee is not variadic
//!   too few      only below the number of parameters that have no default
//! ```
//!
//! A default is recognised by the `=` the declaration writes, which is the only evidence in the tree — this crate
//! does not evaluate default arguments, so a parameter with one counts as satisfied whether or not the argument
//! could be formed. Erring that way is the direction to err in: a default that cannot be used is the compiler's
//! complaint, and this layer's job is to be quiet when it is not sure.

use cpp_parser::CppAstNode;

use super::{Check, CheckContext, DiagnosticCode};

/// **`argument-count`** — see the module documentation.
pub struct ArgumentCount;

/// What a callee declared in this file says about how it may be called.
struct Callee {
    /// The parameters that have no default — a call below this many is missing arguments.
    required: usize,
    /// How many parameters are written, defaults included — a call above this many passes too many.
    written: usize,
    /// The declaration ends in `...`, so any number of arguments above `required` is allowed.
    variadic: bool,
}

impl Check for ArgumentCount {
    const CODES: &'static [DiagnosticCode] = &[DiagnosticCode::ArgumentCount];

    fn check(context: &mut CheckContext<'_>) {
        let callees = callees_in_this_file(context);

        // **The typed layer's own call node**, with its own accessors: `get_callee` and `get_args`. Both were being
        // guessed at before, and both guesses were wrong for this grammar — measured with `cpp_dump`, `scale(1, 2, 3)`
        // is
        //
        // ```text
        //   CallExpr   IdentifierExpr "scale"   "("   LiteralExpr 1   ","   LiteralExpr 2   ","   LiteralExpr 3   ")"
        // ```
        //
        // — there is **no `ArgumentList` node** for a simple call, so `children().find(kind == ArgumentList)` never
        // matched and this check reported nothing at all, in any file, for as long as it has existed.
        for call in context.own::<cpp_parser::CppCallExpr>().collect::<Vec<_>>() {
            let Some(callee) = call.get_callee() else {
                continue;
            };
            // The **last name written** is what a declaration can be found by: `scale` in `scale(1)`, `scale` in
            // `w.scale(1)`, and the last of them in `ns::scale(1)`.
            let Some(name) = super::spelling_of(callee.syntax()) else {
                continue;
            };
            // **This file first, the index second.** A callee declared here is read from the tree, which is where its
            // defaults and its `...` are; one declared in another file is a **fact** the cook already produced —
            // `DeclFact::parameter_list`, the field whose own documentation says it is stored *"because a query holds
            // one file and the parameters are written in another"*.
            let declared = match callees.get(name.as_str()) {
                Some(declared) => Callee {
                    required: declared.required,
                    written: declared.written,
                    variadic: declared.variadic,
                },
                None => match callee_from_the_index(context, &name) {
                    Some(declared) => declared,
                    // Overloaded, or not a function this index can see: not judged, which is the direction to be
                    // wrong in. See the module documentation.
                    None => continue,
                },
            };

            // The arguments as the typed layer counts them — for `scale(1, 2, 3)`, three.
            let passed = call.get_args().len();

            if !declared.variadic && passed > declared.written {
                context.add(
                    DiagnosticCode::ArgumentCount,
                    cpp_parser::source_range(call.get_range()),
                    name.clone(),
                    at_most(&name, declared.written),
                );
            } else if passed < declared.required {
                context.add(
                    DiagnosticCode::ArgumentCount,
                    cpp_parser::source_range(call.get_range()),
                    name.clone(),
                    at_least(&name, declared.required),
                );
            }
        }
    }
}

/// **Every function this file declares, by the name a call would write.** See [`Callee`].
///
/// Built once per check run: a file with a hundred calls and one declaration should read the declaration once, and
/// the map is what makes the cost the calls rather than the calls times the declarations.
fn callees_in_this_file(context: &CheckContext<'_>) -> std::collections::HashMap<String, Callee> {
    let mut callees = std::collections::HashMap::new();

    // **The shared predicate**, and the parameters through the typed layer: measured, a file-scope definition is a
    // `Declaration` whose parameter list is a **grandchild** (`InitDeclarator > Declarator > ParameterList`), so a
    // filter on `FunctionDef` found nothing and a `children()` lookup for the parameter list found nothing either.
    // This check had therefore never judged a function of the file it was checking — the same defect shape as the
    // three before it, and the reason `super::function_definitions` exists.
    for node in super::function_definitions(context) {
        let Some(parameters) = cpp_parser::CppDeclaration::cast(node.clone())
            .and_then(|declaration| declaration.descendants::<cpp_parser::CppParamList>().next())
            .map(|parameters| parameters.syntax().clone())
        else {
            continue;
        };
        let Some(name) = super::name_declared_by(&node) else {
            continue;
        };

        let mut written = 0usize;
        let mut required = 0usize;
        let mut variadic = false;
        for parameter in parameters.children() {
            // **Read from the tokens**, not from the parameter's text: a `...` is a token, an `=` is a different
            // token, and a default value that happens to hold an `=` — `int n = a == b` — is a string of tokens
            // rather than a substring to search. Counting text is how this layer produced its one false positive,
            // and the fix everywhere has been the same.
            let kinds: Vec<cpp_parser::CppTokenKind> = parameter
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .map(|token| token.kind().into())
                .collect();

            if kinds.contains(&cpp_parser::CppTokenKind::Ellipsis) {
                variadic = true;
                continue;
            }
            written += 1;
            // **A default is the `=` the declaration writes** — see the module documentation for why nothing more
            // is asked of it.
            if !kinds.contains(&cpp_parser::CppTokenKind::Assign) {
                required += 1;
            }
        }

        callees.insert(
            name,
            Callee {
                required,
                written,
                variadic,
            },
        );
    }

    callees
}

fn at_most(name: &str, written: usize) -> String {
    format!("`{name}` takes {written} argument(s) and this call passes more")
}

/// **A callee the index knows** — its declaration is in another file, and its parameter list is a **fact**.
///
/// `DeclFact::parameter_list` is `(_Ty* _First, size_type _Count)`: parentheses, defaults, `...` and all, and it is
/// stored rather than read when wanted *"because a query holds one file and the parameters are written in
/// another"* — the sentence this function is an implementation of.
///
/// **One declaration or nothing.** Two declarations of one name are an overload set, and which of them a call
/// resolves to is overload resolution: picking one would report an arity against the wrong signature, so this
/// answers `None` for the whole set — the same rule [`super::a_class_can_be_built_from`] follows for constructors.
///
/// `None` as well for a declaration that is not a function, for one whose parameter list the reading could not
/// reach, and for a name the index does not hold at all.
fn callee_from_the_index(context: &CheckContext<'_>, name: &str) -> Option<Callee> {
    let crate::Known::Yes(definitions) = context.index().definitions(name, context.path) else {
        return None;
    };
    // A class's constructor and a free function may share a spelling; only the functions are this rule's question.
    let functions: Vec<&crate::DeclFact> = definitions
        .found
        .iter()
        .map(|definition| &definition.fact)
        .filter(|fact| fact.kind == crate::DeclKind::Function)
        .collect();
    let [only] = functions.as_slice() else {
        return None;
    };

    let parameters = only.parameter_list.as_deref()?;
    // **The parameters, split where the language splits them**: a comma at depth zero. `std::pair<int, int>` is one
    // parameter, and splitting on every comma would count it as two.
    let inner = parameters
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')');
    let mut depth = 0i32;
    let mut written = 0usize;
    let mut required = 0usize;
    let mut variadic = false;
    let mut current = String::new();
    let take = |parameter: &str, written: &mut usize, required: &mut usize, variadic: &mut bool| {
        let parameter = parameter.trim();
        if parameter.is_empty() {
            return;
        }
        // **A pack is a `...` that is not the whole parameter, and this test used to miss every real spelling of it.**
        //
        // The file-scope half above asks the **tokens** of each parameter for an `Ellipsis`, so `Args&&... args` marks
        // the callee variadic there. Here the parameter list is text — it is a `DeclFact` produced by the cook, read
        // when the declaration lives in another file — and the test was `parameter.ends_with("...")`, which is true of
        // the C-style `...` and of `Args...` and of **nothing else people write**. Measured shape of the defect:
        // `template <class... Args> void f(Args&&... args)` recorded one written, one required parameter, so a call
        // `f(1, 2, 3)` was reported as *"takes 1 argument(s) and this call passes more"*.
        //
        // The rule is now the file half's rule — an ellipsis anywhere in the parameter means the parameter (or the
        // callee) is a pack — with one guard the token half gets for free: an ellipsis inside a literal is text, not a
        // pack, so `const char* dots = "..."` does not make a callee variadic.
        if has_an_ellipsis(parameter) {
            *variadic = true;
            return;
        }
        *written += 1;
        // A default is the `=` the declaration writes.
        if !parameter.contains('=') {
            *required += 1;
        }
    };
    for character in inner.chars() {
        match character {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                take(&current, &mut written, &mut required, &mut variadic);
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(character);
    }
    take(&current, &mut written, &mut required, &mut variadic);

    Some(Callee {
        required,
        written,
        variadic,
    })
}

fn at_least(name: &str, required: usize) -> String {
    format!("`{name}` needs {required} argument(s) and this call passes fewer")
}
/// **Is there a `...` in this parameter that the language would call one?**
///
/// The parameter list of a declaration in another file arrives as text, so this is a scan rather than a token walk —
/// and the one thing a scan gets wrong is an ellipsis written inside a literal. `void log(const char* dots = "...")`
/// is not variadic, and `standardese`'s own `Args... args` is. Quotes are tracked with a backslash escape and no
/// attempt at raw strings: a raw string holding an odd number of quotes is the one case this misreads, and it errs
/// towards calling the callee variadic, which is the quiet direction.
fn has_an_ellipsis(parameter: &str) -> bool {
    let bytes = parameter.as_bytes();
    let mut at = 0usize;
    let mut quoted: Option<u8> = None;

    while at < bytes.len() {
        let byte = bytes[at];
        match quoted {
            Some(quote) => {
                if byte == b'\\' {
                    at += 2;
                    continue;
                }
                if byte == quote {
                    quoted = None;
                }
            }
            None => {
                if byte == b'"' || byte == b'\'' {
                    quoted = Some(byte);
                } else if parameter[at..].starts_with("...") {
                    return true;
                }
            }
        }

        at += 1;
    }

    false
}


