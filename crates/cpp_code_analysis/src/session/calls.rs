//! **Calls and layout** — the declaration a call means, its deduced arguments, and the size of a class.
//!
//! Moved here from `session/mod.rs` without a change.

use super::*;

/// **A member's written type with the macros in it expanded** — `const l_signalT` is `const sig_atomic_t` where
/// `#define l_signalT sig_atomic_t` is in force at the member.
///
/// Every word of the spelling is asked, because a macro may be the whole type (`l_signalT`) or part of a templated
/// one; a word that is not a macro at that offset is kept as written. The expansion is asked **at the member's own
/// position**, so a header whose `#define` depends on a branch above the member is read the way the compiler read it.
impl<F: FileProvider + Clone> Session<F> {
    fn the_type_with_its_macros_expanded(
        &self,
        file: &Path,
        offset: usize,
        written: &str,
    ) -> Option<String> {
        let mut out = String::with_capacity(written.len());
        let mut changed = false;
        for (at, word) in written
            .split(|character: char| !(character.is_alphanumeric() || character == '_'))
            .enumerate()
        {
            if at > 0 {
                out.push(' ');
            }
            if word.is_empty() {
                continue;
            }
            match self.macro_body_at(file, offset, word) {
                Some(body) => {
                    out.push_str(body.trim());
                    changed = true;
                }
                None => out.push_str(word),
            }
        }
        changed.then_some(out)
    }
}
/// **A spelling without the qualifier words in front of its type** — `const volatile Widget` is `Widget`.
///
/// The words are stripped as *words* (each must be followed by a space), so a type called `constant` keeps its name,
/// and only the front of the spelling is looked at: `Widget* const` qualifies the pointer, not the type named here.
/// The layout's steps are lookups by name, and a qualified spelling is a different name to every one of them.
fn spelled_without_its_qualifiers(spelled: &str) -> &str {
    let mut word = spelled.trim();
    loop {
        let before = word;
        for qualifier in [
            "const",
            "volatile",
            "static",
            "extern",
            "mutable",
            "thread_local",
            "restrict",
            "__restrict",
        ] {
            if let Some(rest) = word.strip_prefix(qualifier)
                && rest.starts_with(char::is_whitespace)
            {
                word = rest.trim_start();
            }
        }
        if word == before {
            return word;
        }
    }
}

/// **Split a trailing `[dimension]` from a type spelling**, or `None` when the spelling does not end in one.
///
/// The declarator's name is already gone — `DeclFact::type_of` holds a type, not a declaration — so the last bracket
/// is the outermost array. A parenthesised declarator (`int (*table[3])(int)`) does not end in `]` and is left to the
/// steps that can read it; a flexible array's dimension is empty, which the caller reads as zero extent.
fn split_an_array_dimension(spelling: &str) -> Option<(&str, &str)> {
    let spelling = spelling.trim();
    let inside = spelling.strip_suffix(']')?;
    let open = inside.rfind('[')?;
    let element = inside[..open].trim();
    if element.is_empty() || element.contains("(*") || element.ends_with('(') {
        return None;
    }
    Some((element, inside[open + 1..].trim()))
}

/// **A constant integer expression, as a macro body or an enumerator's initialiser spells one** — `8`, `(53)`,
/// `('z' - 'a' + 1)`, `1 << 4` — or `None` for anything with a name or a call in it.
///
/// The array dimensions real headers use are not all literals: neovim's `NMARKS` is `('z' - 'a' + 1)` and its
/// `NSUBEXP` is an enumerator. Reading those as numbers is what keeps a struct with `fmark_T b_namedm[NMARKS]` and
/// `lpos_T startpos[NSUBEXP]` from being "not computed" for a reason that is not about its layout at all. This is a
/// reader of the operators C gives a constant expression, not an evaluator of the language: a name, a `sizeof`, or a
/// cast is `None` and the member stays unsized.
fn a_constant_expression(text: &str) -> Option<usize> {
    let mut parser = IntegerExpression {
        text: text.as_bytes(),
        at: 0,
    };
    let value = parser.a_sum()?;
    parser.skip_space();
    if parser.at != parser.text.len() {
        return None;
    }
    usize::try_from(value).ok()
}

struct IntegerExpression<'a> {
    text: &'a [u8],
    at: usize,
}

impl IntegerExpression<'_> {
    fn skip_space(&mut self) {
        while self
            .text
            .get(self.at)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        self.skip_space();
        if self.text.get(self.at) == Some(&byte) {
            self.at += 1;
            return true;
        }
        false
    }

    fn a_sum(&mut self) -> Option<i128> {
        let mut value = self.a_product()?;
        loop {
            if self.eat(b'+') {
                value = value.checked_add(self.a_product()?)?;
            } else if self.eat(b'-') {
                value = value.checked_sub(self.a_product()?)?;
            } else {
                return Some(value);
            }
        }
    }

    fn a_product(&mut self) -> Option<i128> {
        let mut value = self.a_shift()?;
        loop {
            if self.eat(b'*') {
                value = value.checked_mul(self.a_shift()?)?;
            } else if self.eat(b'/') {
                let divisor = self.a_shift()?;
                value = value.checked_div(divisor)?;
            } else if self.eat(b'%') {
                let divisor = self.a_shift()?;
                value = value.checked_rem(divisor)?;
            } else {
                return Some(value);
            }
        }
    }

    fn a_shift(&mut self) -> Option<i128> {
        let mut value = self.a_bitand()?;
        loop {
            self.skip_space();
            let rest = self.text.get(self.at..).unwrap_or_default();
            if rest.starts_with(b"<<") {
                self.at += 2;
                value = value.checked_shl(u32::try_from(self.a_bitand()?).ok()?)?;
            } else if rest.starts_with(b">>") {
                self.at += 2;
                value = value.checked_shr(u32::try_from(self.a_bitand()?).ok()?)?;
            } else {
                return Some(value);
            }
        }
    }

    fn a_bitand(&mut self) -> Option<i128> {
        let mut value = self.a_bitxor()?;
        loop {
            self.skip_space();
            // `&` and not `&&`: the latter is not a constant integer expression this reader claims.
            if self.text.get(self.at..self.at + 2) == Some(b"&&") {
                return None;
            }
            if self.eat(b'&') {
                value &= self.a_bitxor()?;
            } else {
                return Some(value);
            }
        }
    }

    fn a_bitxor(&mut self) -> Option<i128> {
        let mut value = self.a_bitor()?;
        while self.eat(b'^') {
            value ^= self.a_bitor()?;
        }
        Some(value)
    }

    fn a_bitor(&mut self) -> Option<i128> {
        let mut value = self.a_unary()?;
        loop {
            if self.eat(b'|') {
                value |= self.a_unary()?;
            } else {
                return Some(value);
            }
        }
    }

    fn a_unary(&mut self) -> Option<i128> {
        if self.eat(b'+') {
            return self.a_unary();
        }
        if self.eat(b'-') {
            return self.a_unary()?.checked_neg();
        }
        if self.eat(b'~') {
            return Some(!self.a_unary()?);
        }
        self.a_primary()
    }

    fn a_primary(&mut self) -> Option<i128> {
        self.skip_space();
        if self.eat(b'(') {
            let value = self.a_sum()?;
            return self.eat(b')').then_some(value);
        }
        let byte = *self.text.get(self.at)?;
        if byte == b'\'' {
            return self.a_character();
        }
        if !byte.is_ascii_digit() {
            return None;
        }
        let start = self.at;
        while self
            .text
            .get(self.at)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            self.at += 1;
        }
        let spelling = std::str::from_utf8(&self.text[start..self.at]).ok()?;
        let (radix, digits) = if let Some(rest) = spelling
            .strip_prefix("0x")
            .or_else(|| spelling.strip_prefix("0X"))
        {
            (16, rest)
        } else if let Some(rest) = spelling
            .strip_prefix("0b")
            .or_else(|| spelling.strip_prefix("0B"))
        {
            (2, rest)
        } else if spelling.len() > 1 && spelling.starts_with('0') {
            (8, &spelling[1..])
        } else {
            (10, spelling)
        };
        // A C suffix (`u`, `U`, `l`, `L`) is not part of the number; anything else is not this reader's.
        let digits = digits.trim_end_matches(['u', 'U', 'l', 'L']);
        i128::from_str_radix(digits, radix).ok()
    }

    fn a_character(&mut self) -> Option<i128> {
        self.at += 1; // the opening quote
        let byte = *self.text.get(self.at)?;
        self.at += 1;
        let value = match byte {
            b'\\' => {
                let escaped = *self.text.get(self.at)?;
                self.at += 1;
                match escaped {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'r' => b'\r',
                    b'0' => 0,
                    b'\\' => b'\\',
                    b'\'' => b'\'',
                    b'"' => b'"',
                    b'x' => {
                        let start = self.at;
                        while self
                            .text
                            .get(self.at)
                            .is_some_and(|byte| byte.is_ascii_hexdigit())
                        {
                            self.at += 1;
                        }
                        let digits = std::str::from_utf8(&self.text[start..self.at]).ok()?;
                        u8::from_str_radix(digits, 16).ok()?
                    }
                    other => other,
                }
            }
            other => other,
        };
        self.eat(b'\'').then_some(i128::from(value))
    }
}

/// **Is the class at `class_at` introduced by `union`?**
///
/// The name of a typedef of an anonymous class comes **after** its body (`typedef struct { … } Name;`), while the name
/// of `struct Foo { … }` comes before it. Reading the keyword from all text before the name therefore sees the last
/// member's `union` in the first form — neovim's `Callback` was laid out as a union because its `data` member is one.
/// So the body's opening brace is found on the side the name implies, and the nearest `union`/`struct`/`class` before
/// that brace is the declaration's own.
fn the_class_is_a_union(text: &str, class_at: usize) -> bool {
    let at = class_at.min(text.len());
    let before = text[..at].trim_end();
    let opener = if before.ends_with('}') {
        // A typedef of an anonymous class: walk back to the brace that opened this body.
        an_opening_brace_before(text, at)
    } else {
        text[at..].find('{').map(|offset| at + offset)
    };
    let Some(opener) = opener else {
        return false;
    };
    the_nearest_class_keyword(&text[..opener]) == Some("union")
}

/// **The brace that opened the body a member name follows**, or `None` when there is no `}` before `at`.
///
/// `at` is the end of a name: for a class name it follows the closing `}` of its body (`typedef struct { … } Name`),
/// and for a member name it follows the closing `}` of the anonymous class the member declares
/// (`union { … } data`). The last `}` before the name is therefore the one whose opener is wanted.
fn an_opening_brace_before(text: &str, at: usize) -> Option<usize> {
    let before = text[..at.min(text.len())].trim_end();
    let close = before.rfind('}')?;
    let before = &before[..=close];
    let mut depth = 0usize;
    for (index, character) in before.char_indices().rev() {
        match character {
            '}' => depth += 1,
            '{' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// The nearest `union`/`struct`/`class` keyword in `before`, as a word, or `None` when there is none.
fn the_nearest_class_keyword(before: &str) -> Option<&str> {
    let mut nearest: Option<(usize, &str)> = None;
    for keyword in ["union", "struct", "class"] {
        for (index, _) in before.match_indices(keyword) {
            let previous = before[..index].chars().next_back();
            let following = before[index + keyword.len()..].chars().next();
            let a_word = |character: Option<char>| {
                character.is_some_and(|character| character.is_alphanumeric() || character == '_')
            };
            if a_word(previous) || a_word(following) {
                continue;
            }
            if nearest.is_none_or(|(nearest_at, _)| index > nearest_at) {
                nearest = Some((index, keyword));
            }
        }
    }
    nearest.map(|(_, keyword)| keyword)
}

impl<F: FileProvider + Clone> Session<F> {
    /// **The declarations a call at this offset can reach** — indices into `parameter_lists`.
    /// The same resolution the argument checks and the signature popup use, asked over the declarations a caller already
    /// holds: a hover has the facts of the name under the cursor and needs to know which of them the call can mean.
    ///
    /// ```text
    ///   one index     the resolution decided (`Choice::One`)
    ///   several       every one of them can accept the arguments, and nothing decides between them by type
    ///   None          the offset is not on a call, or no declaration can accept it
    /// ```
    ///
    /// The **several** case is the one a hover needs: printing the chosen declaration beside five others that cannot take
    /// this argument list answers a question the reader did not ask.
    pub fn the_declarations_a_call_means(
        &self,
        view: &FileView,
        offset: ReadingOffset,
        parameter_lists: &[Option<String>],
        template_parameters: &[Vec<String>],
    ) -> Option<Vec<usize>> {
        let (types, candidates) =
            self.the_candidates_of_a_call(view, offset, parameter_lists, template_parameters)?;
        match crate::sema::overload::choose(&types, &candidates) {
            crate::sema::overload::Choice::One(chosen) => Some(vec![chosen]),
            // **Nothing decides between them**, so the viable set is the answer: every one of them can accept this call,
            // and the ones arity alone rules out are gone.
            crate::sema::overload::Choice::Ambiguous => Some(
                crate::sema::overload::viable_candidates(&types, &candidates),
            ),
            crate::sema::overload::Choice::None => None,
        }
    }

    /// **The one declaration a call at this offset means**, when the resolution decides — see
    /// [`Session::the_declarations_a_call_means`] for the answer that also covers the ambiguous case.
    pub fn the_declaration_a_call_means(
        &self,
        view: &FileView,
        offset: ReadingOffset,
        parameter_lists: &[Option<String>],
        template_parameters: &[Vec<String>],
    ) -> Option<usize> {
        let (types, candidates) =
            self.the_candidates_of_a_call(view, offset, parameter_lists, template_parameters)?;
        match crate::sema::overload::choose(&types, &candidates) {
            crate::sema::overload::Choice::One(chosen) => Some(chosen),
            _ => None,
        }
    }

    /// **The declarations and the argument types of the call at `offset`** — the one walk the two questions above share.
    pub(crate) fn the_candidates_of_a_call(
        &self,
        view: &FileView,
        offset: ReadingOffset,
        parameter_lists: &[Option<String>],
        template_parameters: &[Vec<String>],
    ) -> Option<(
        Vec<crate::Known<String>>,
        Vec<crate::sema::overload::Candidate>,
    )> {
        let token = cpp_parser::token_at(&view.root, offset.get())?;
        let call = token.parent_ancestors().find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::CallExpr
        })?;
        let arguments = crate::sema::call_syntax::arguments_of(&call)?;

        // The engine answers with a parsed `Type` and the file it came from; the resolver ranks spellings, which is the
        // same information the facts carry.
        let session: &Session<F> = self;
        let index: &crate::ProjectIndex = session.store.index();
        let resolver =
            Box::new(move |path: &Path| -> Option<FileView> { session.view_of_the_file(path) });
        let model = crate::sema::model::SemanticModel::new(
            index,
            &view.root,
            &view.scopes,
            &view.source,
            &view.path,
            resolver,
        );
        let types: Vec<crate::Known<String>> = arguments
            .iter()
            .map(|argument| match model.type_of(argument) {
                crate::Known::Yes((a_type, _)) => crate::Known::Yes(a_type.to_string()),
                crate::Known::No => crate::Known::No,
                crate::Known::Unknown(reason) => crate::Known::Unknown(reason),
            })
            .collect();

        let candidates: Vec<crate::sema::overload::Candidate> = parameter_lists
            .iter()
            .enumerate()
            .map(|(at, parameters)| {
                let (declared, variadic) = parameters
                    .as_deref()
                    .map(crate::sema::overload::parameters_in)
                    .unwrap_or_default();
                // **A template candidate is ranked as what the call instantiates**, when the caller
                // could read its header: `take(T*)` and `take(T)` say nothing about an `int*`, and
                // deduced they say `int*` and `int`.
                let names = template_parameters.get(at).cloned().unwrap_or_default();
                // Kept for the partial ordering, which compares what the *declarations* wrote, not what the call
                // substituted — and the substitution below consumes `declared`.
                let declared_for_ordering = declared.clone();
                let (types, is_a_template) =
                    match crate::sema::deduction::deduce(&names, &declared, &types) {
                        Some(arguments) => (
                            declared
                                .iter()
                                .map(|spelling| {
                                    crate::sema::types::substituted_spelling(
                                        spelling, &names, &arguments,
                                    )
                                })
                                .collect(),
                            true,
                        ),
                        None => (declared, !names.is_empty()),
                    };
                let candidate = crate::sema::overload::Candidate::new(
                    types,
                    variadic,
                    format!("declaration {at}"),
                );
                if is_a_template {
                    candidate.with_declaration(declared_for_ordering, names.clone())
                } else {
                    candidate
                }
            })
            .collect();

        Some((types, candidates))
    }

    /// **A file's facts, read from its own view** — the answer an index summary gives, without waiting for one.
    ///
    /// A request can beat the index: the file is open, its tree is parsed, and the summary that records a reflection
    /// annotation (`UCLASS(Blueprintable)`) is built by the pump some time later. A card that needs one field the
    /// binding does not carry cannot wait, so it asks the view — the same `build_facts` the summary is built from, over
    /// the same tree, which is why the two cannot disagree about what the file says.
    ///
    /// One parse and one fact-building pass, the price [`Session::outline`] already pays per request.
    pub fn facts_of_the_view(&self, view: &FileView) -> Vec<crate::DeclFact> {
        let preprocessing = crate::preprocess(&view.source, view.tree.get_tokens());
        let errors: Vec<cpp_parser::SourceRange> = view
            .tree
            .get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range))
            .collect();
        crate::build_facts(&view.scopes, &preprocessing, &view.root, &errors, true).0
    }

    /// **The declaration's own node**, for a fact that names one — the syntax a rendering needs.
    ///
    /// A [`DeclFact`] records the words of a declaration and not the structure of it: whether a class was written
    /// `class`, `struct` or `union` is the node's kind, and a template head's parameters are the node's children. A
    /// caller that has only the fact can render everything a fact carries and must guess the rest, so the card asks for
    /// the node where the fact says the declaration is.
    ///
    /// `None` when the file cannot be read (a header the project never loaded), when the offset is not inside a
    /// declaration, or when the declaration there is not the one asked about — all three are the caller's answer
    /// "render from the fact alone", never an error.
    ///
    /// # Why the answer is a syntax node and not a [`cpp_parser::CppDeclaration`]
    ///
    /// Because **a parameter is a declaration of a name this model does not accept as one**. The parser's declaration
    /// union is `Declaration | UsingDecl | TypedefDecl` (it is what a list of declarations iterates), and a
    /// `Parameter` node is none of them — so asking for the nearest `CppDeclaration` inside
    /// `[](auto const& value)` answered the **enclosing function**, whose specifiers say nothing about `value`. A card
    /// built from that printed `auto& value` for `auto const& value`, and `std::string& left` for
    /// `std::string const& left`. The node is what has the specifiers; a caller that needs the declaration API casts.
    pub fn the_declaration_node(
        &self,
        file: &Path,
        offset: usize,
    ) -> Option<cpp_parser::CppSyntaxNode> {
        let view = self.view_of_the_file(file)?;
        let token = cpp_parser::token_at(&view.root, offset)?;
        use cpp_parser::CppAstNode as _;
        token.parent_ancestors().find(|node| {
            let kind = cpp_parser::CppSyntaxKind::from(node.kind());
            // **`cast`, not `can_cast`.** `can_cast` is the narrow test a tree walk uses, and it deliberately
            // leaves `TypedefDecl` out; a *lookup* wants every spelling of a declaration, and `cast` is the accessor
            // that says so. Without this a typedef's name found no declaration at all, and its hover card had no
            // syntax to render from.
            kind == cpp_parser::CppSyntaxKind::Parameter
                || cpp_parser::CppDeclaration::cast(node.clone()).is_some()
        })
    }

    /// **The template arguments a call fixes**, for a declaration the caller already holds.
    ///
    /// Deduction needs the call's argument types, which is what a `SemanticModel` is for; the names come from the
    /// declaration's own text ([`crate::sema::deduction::template_parameters_before`]) and the parameters from its fact.
    /// `None` when the declaration is not a template, when the offset is not on a call, or when the call does not fix
    /// every parameter — the three answers for which nothing should be shown as an instantiation.
    pub fn the_deduced_arguments(
        &self,
        view: &FileView,
        offset: ReadingOffset,
        template_parameters: &[String],
        parameters: &[String],
    ) -> Option<Vec<String>> {
        if template_parameters.is_empty() {
            return None;
        }
        let token = cpp_parser::token_at(&view.root, offset.get())?;
        let call = token.parent_ancestors().find(|node| {
            cpp_parser::CppSyntaxKind::from(node.kind()) == cpp_parser::CppSyntaxKind::CallExpr
        })?;
        let arguments = crate::sema::call_syntax::arguments_of(&call)?;

        let session: &Session<F> = self;
        let index: &crate::ProjectIndex = session.store.index();
        let resolver =
            Box::new(move |path: &Path| -> Option<FileView> { session.view_of_the_file(path) });
        let model = crate::sema::model::SemanticModel::new(
            index,
            &view.root,
            &view.scopes,
            &view.source,
            &view.path,
            resolver,
        );
        let types: Vec<crate::Known<String>> = arguments
            .iter()
            .map(|argument| match model.type_of(argument) {
                crate::Known::Yes((a_type, _)) => crate::Known::Yes(a_type.to_string()),
                crate::Known::No => crate::Known::No,
                crate::Known::Unknown(reason) => crate::Known::Unknown(reason),
            })
            .collect();

        crate::sema::deduction::deduce(template_parameters, parameters, &types)
    }
    /// The members of a type, as a file's own scopes and the index together know them.
    ///
    /// `written_type` is a spelling — `Widget`, `ns::Widget`, `const Widget&` — and following it to a class is the
    /// same work [`Session::member_completions`] does for an object expression.
    pub fn members_of(&self, view: &FileView, written_type: &str) -> Known<MemberList> {
        let listed = members_of(
            self.store.index(),
            &view.scopes,
            &view.root,
            &view.path,
            written_type,
        );
        // **The class's arguments, written through its members** — see [`substituted_members`].
        let listed = substituted_members(self.store.index(), &view.path, written_type, listed);
        self.the_generated_members(written_type, &view.path, listed)
    }

    /// **What a class costs in memory**: its size, its alignment, and where each data member sits — see
    /// [`crate::sema::layout`].    ///
    /// `written_type` is a spelling (`Widget`, `ns::Widget`). `Known::No` when the name is not a class with a body,
    /// which is what "there is no layout to give" means here: a forward declaration has no size in this model, and
    /// `sizeof` of one is a compile error rather than a number.
    ///
    /// The layout is **withheld** when one of the members cannot be sized: [`ClassLayout::unknown`] names them, and a
    /// number beside a partial answer would be a guess. The one exception is a class with **no data members at all**,
    /// which is one byte by the language's own rule and is reported as such.
    pub fn the_layout_of(
        &self,
        view: &FileView,
        written_type: &str,
    ) -> Known<crate::sema::layout::ClassLayout> {
        self.the_layout_of_a_class(view, written_type, 0)
    }

    pub(crate) fn the_layout_of_a_class(
        &self,
        view: &FileView,
        written_type: &str,
        depth: usize,
    ) -> Known<crate::sema::layout::ClassLayout> {
        self.the_layout_of_a_class_with(view, written_type, &[], depth)
    }

    /// [`Session::the_layout_of_a_class`] for a class **template applied to arguments** — `std::vector<int>`.
    ///
    /// `arguments` are the ones the *use* wrote. An empty slice with a template-id spelling
    /// (`std::vector<int>` passed whole) splits it here; an empty slice with a plain name is the ordinary class.
    ///
    /// # Why the members are sized through a substitution
    ///
    /// A class template's data members are written in its parameters — `T value;` — and their sizes are the sizes of
    /// whatever the arguments make them. The member *facts* keep the declaration's spelling (`vector`'s `_Mypair` is
    /// `_Compressed_pair<_Alty, _Scary_val>`), so every member type is read with the parameters replaced by the
    /// arguments first, and a member whose type is a **member alias** of this class (`_Alty`) is followed to its
    /// target with the same substitution. The recursion is bounded by `depth`, and a member that is still written in
    /// a parameter after substitution is **unknown** rather than looked up by bare name — the mistake that made
    /// `std::vector<widget::Widget>`'s card say "size 9 byte(s), alignment 1" (see `docs/diagnostics-performance.md`
    /// and the note in `the_size_of_a_type_in`).
    pub(crate) fn the_layout_of_a_class_with(
        &self,
        view: &FileView,
        written_type: &str,
        arguments: &[String],
        depth: usize,
    ) -> Known<crate::sema::layout::ClassLayout> {
        if depth > 6 {
            return Known::No;
        }
        let (head, arguments) = match crate::index::project::split_template_arguments(written_type)
        {
            Some((head, written)) if arguments.is_empty() => (head, written),
            _ => (written_type.to_string(), arguments.to_vec()),
        };
        let full = if arguments.is_empty() {
            head.clone()
        } else {
            format!("{head}<{}>", arguments.join(", "))
        };
        let Known::Yes(list) = self.members_of(view, &full) else {
            return Known::No;
        };
        // **The class and its members as their file wrote them** — the text a union's keyword and a bitfield's `:` are
        // read out of, at file offsets. `None` when the index cannot place the class: the layout is still computed for
        // a class the asking file declares, and reported without the two spelling facts it cannot read.
        let as_written = self.the_class_as_its_file_wrote_it(&head, &view.path);
        // **A hover is a request, and a layout sizes every member.** A class with more than this many data members is
        // a registry — neovim's `file_buffer` has 253 — and the bound is a card the reader scrolls past versus one
        // that costs seconds. It was 64, which was too small in a way that was not visible on the large structs
        // alone: `wininfo_S` is seven members, but one of them is a `winopt_T` of 68, so the whole card was silent.
        // Measured on neovim: `file_buffer` at 253 members costs about 1.2 s of layout on top of its cold cook, and
        // the older no-bound 7.2 s was before the type and constant work this file does now. The budget is per class
        // and per request.
        let data_members = list
            .own()
            .filter(|member| {
                member.fact.kind == crate::DeclKind::Variable
                    && !member.fact.modifiers.contains(crate::DeclModifiers::STATIC)
            })
            .count();
        if crate::probes::layout() {
            eprintln!(
                "[layout] data-members {data_members} of {}, budget {LAYOUT_MEMBER_BUDGET}",
                list.members.len()
            );
        }
        if data_members > LAYOUT_MEMBER_BUDGET {
            return Known::No;
        }
        let union = as_written.as_ref().is_some_and(|written| {
            // **The keyword that introduces this class**, not the last one anywhere before its name. A typedef of
            // an anonymous struct may contain a member `union { … } data;`, and scanning all text before the name —
            // which sits after the closing brace — found that inner `union` and laid the whole struct out as one:
            // neovim's `Callback` came out 32 bytes and `a union`, against the compiler's 16. The body is before
            // the name for such a typedef and after it for `struct Foo {`, so the opener is found on the correct
            // side and the nearest keyword before it is the declaration's own.
            the_class_is_a_union(&written.text, written.class_at)
        }) || self.an_anonymous_member_is_a_union(&head, view);
        // **The environment a member's type is read in** — the class's parameters, the arguments the use wrote (or
        // the declaration's own defaults), and the class's own members, which is where a member alias lives.
        if crate::probes::layout() {
            eprintln!(
                "[layout] class {head:?} arguments {arguments:?} as-written {:?} members {}",
                as_written
                    .as_ref()
                    .map(|written| written.parameters.clone()),
                list.members.len()
            );
        }
        let no_names: Vec<String> = Vec::new();
        let instantiation = Instantiation {
            parameters: as_written
                .as_ref()
                .map_or(&no_names, |written| &written.parameters),
            defaults: as_written
                .as_ref()
                .map_or(&no_names, |written| &written.parameter_defaults),
            arguments: &arguments,
            members: &list.members,
        };
        let mut sized: Vec<(String, usize, usize, Option<usize>)> = Vec::new();
        let mut unknown: Vec<String> = Vec::new();
        // **Declaration order**, which is what decides the offsets: `members_of` answers in the index's own order
        // (names sorted, because a lookup by name is what it is for), and a layout computed in that order is a
        // different class. A member's own declaration offset is the only thing that says where it was written.
        let mut own: Vec<&crate::index::ProjectMember> = list.own().collect();
        own.sort_by(|left, right| {
            left.file.cmp(&right.file).then(
                left.fact
                    .range
                    .start_offset
                    .cmp(&right.fact.range.start_offset),
            )
        });
        for member in own {
            let fact = &member.fact;
            // **Data members only**: a member function, a nested type, a `static` — none of them take a byte in the
            // object, and `sizeof` does not count them.
            if fact.kind != crate::DeclKind::Variable
                || fact.modifiers.contains(crate::DeclModifiers::STATIC)
            {
                continue;
            }
            let Some(type_of) = fact.type_of.as_deref() else {
                unknown.push(fact.name.to_string());
                continue;
            };
            // **A bitfield's width is in the file's text, not in its fact.** It is read at the member's own file
            // offset, which is why the class was read out of its summary above: a member fact that came from a cooked
            // reading carries the rendering's offsets instead. A width that is not a literal (`: BITS`) stays
            // unknown — the layout after it would be a guess.
            let mut bit_width = None;
            if let Some(written) = &as_written
                && let Some(at) = written.members.get(fact.name.as_str())
                && Self::a_bitfield(&written.text, *at)
            {
                match Self::a_bitfield_width(&written.text, *at) {
                    Some(width) => bit_width = Some(width),
                    None => {
                        unknown.push(fact.name.to_string());
                        continue;
                    }
                }
            }
            // **MSVC's bitfield ABI is not the one the layout rules implement.** The card states the Itanium/GCC x64
            // rules, which MSVC agrees with for whole members and disagrees with for bitfields; until that ABI is
            // measured, a bitfield in an MSVC project is withheld rather than placed by the other compiler's rule.
            if bit_width.is_some() && self.config.dialect() == cpp_parser::Dialect::Msvc {
                unknown.push(fact.name.to_string());
                continue;
            }
            // **A type spelled through a macro.** `volatile l_signalT trap;` with `#define l_signalT sig_atomic_t`:
            // the fact records what was *written*, and only expanding it reaches a name the index can size. The
            // expansion is asked at the member's own offset in its own file, which is where the `#define` in force
            // is the one a compiler had. Measured on Lua 5.5.0's `lstate.h`: `trap` and `hookmask` are both spelled
            // `const l_signalT`, and with them unsized `CallInfo`, `lua_State::transferinfo`'s parents and finally
            // `lua_State` itself all withheld their layout.
            let measured = self
                .the_size_of_a_type_in(view, type_of, depth, Some(&instantiation))
                .or_else(|| {
                    let expanded = self.the_type_with_its_macros_expanded(
                        &member.file,
                        fact.range.start_offset,
                        type_of,
                    )?;
                    if expanded == type_of {
                        return None;
                    }
                    self.the_size_of_a_type_in(view, &expanded, depth, Some(&instantiation))
                });
            match measured {
                Some((size, alignment)) => {
                    // A bitfield wider than its declared type is not something a compiler accepts; a layout with it
                    // would be arithmetic on an invalid program.
                    if bit_width.is_some_and(|width| width > size * 8) {
                        unknown.push(fact.name.to_string());
                        continue;
                    }
                    sized.push((fact.name.to_string(), size, alignment, bit_width));
                }
                None => unknown.push(fact.name.to_string()),
            }
        }
        // **A base class takes bytes too, and this layer places none of them.** An *empty* base costs nothing — the
        // language's empty-base optimization is what every compiler does — while a non-empty one would have to be
        // laid at offset zero, with its own alignment, ahead of the members. That is a layout rule this layer does
        // not implement, so a class whose base is not empty is **withheld** rather than reported too small: the same
        // direction `sema::layout`'s note states, and the one a `size` that reads plausibly must not break.
        if let Some(written) = &as_written {
            for base in &written.bases {
                let empty = self
                    .the_layout_of_a_base(view, base, depth + 1, &instantiation)
                    .is_some_and(|layout| layout.fields.is_empty() && layout.size == 1);
                if !empty {
                    unknown.push(format!("base {base}"));
                }
            }
        }
        Known::Yes(crate::sema::layout::ClassLayout::of_members(
            &sized, union, unknown,
        ))
    }

    /// **The value of an array dimension** — a literal, a macro whose body is one, or an enumerator.
    ///
    /// The macros are the ordinary C way to size an array (`#define DB_COUNT 8`), and neovim's core structs use them
    /// on nearly every array. A macro's body is in force in the class's own translation unit, which is what
    /// [`Session::macro_body_at`] reads; an enumerator (`NSUBEXP = 10`) is a declaration the constant folder can
    /// evaluate. Anything else — `sizeof(x)`, an expression, a macro another macro builds — is `None`, and the
    /// member stays unsized rather than guessed.
    fn the_value_of_an_array_dimension(&self, view: &FileView, dimension: &str) -> Option<usize> {
        let dimension = dimension.trim();
        if dimension.is_empty() {
            // A flexible array member: C's `sizeof` leaves it out.
            return Some(0);
        }
        if let Ok(value) = dimension.parse::<usize>() {
            return Some(value);
        }
        // **The fact's spelling may already hold the expansion** — `int [('z'-'a'+1)]` rather than
        // `int [NMARKS]` — because the class's members came from a reading that expanded macros. So the dimension is
        // evaluated as written first, and only a bare name is looked up.
        if let Some(value) = a_constant_expression(dimension) {
            return Some(value);
        }
        if !dimension
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            return None;
        }
        let at = view.source.len();
        if let Some(body) = self.macro_body_at(&view.path, at, dimension) {
            if crate::probes::layout() {
                eprintln!(
                    "[layout] dimension {dimension:?} macro body {body:?} -> {:?}",
                    a_constant_expression(&body)
                );
            }
            if let Some(value) = a_constant_expression(&body) {
                return Some(value);
            }
        }
        if let Known::Yes(found) = self.store.index().definition(dimension, &view.path) {
            if let Some(value) = self.the_constant_a_declaration_writes(&found) {
                return Some(value);
            }
            if let Some(value) =
                self.constant_value_of_a_declaration(&found.file, found.fact.name_range, &view.path)
                && value >= 0
            {
                return Some(value as usize);
            }
        }
        if crate::probes::layout() {
            eprintln!("[layout] dimension {dimension:?} is not a number this layer can read");
        }
        None
    }

    /// **A declaration's initialiser, read from the text the declaration wrote** — `NSUBEXP = 10` in an enum, or
    /// `const int N = 8;`.
    ///
    /// The constant folder answers a variable's value, but an enumerator is a declaration rather than an expression
    /// node, and `NSUBEXP` is how neovim sizes `lpos_T startpos[NSUBEXP]`. The segment before the next `,`, `;` or `}`
    /// is the declaration's own; no `=` there means an enumerator that inherits its value, which this leaves to the
    /// folder rather than counting by hand.
    fn the_constant_a_declaration_writes(
        &self,
        found: &crate::index::project::ProjectDefinition,
    ) -> Option<usize> {
        let text = self.text(&found.file)?;
        let after = text.get(found.fact.name_range.end_offset()..)?;
        let boundary = after.find([',', ';', '}', '{'])?;
        let declaration = &after[..boundary];
        let equals = declaration.find('=')?;
        a_constant_expression(&declaration[equals + 1..])
    }

    /// **Is this a member of a class whose type is the anonymous class the member declares?**
    ///
    /// `Callback::data` is the type of `data` in `union { … } data;`, and the class fact has no written body of its
    /// own (`as_written` is `None`), so nothing said the four members are alternatives. Without this the union was
    /// laid out as a struct — 32 bytes where the compiler says 8 — and every containing class inherited the error.
    /// The parent's written text still holds the declaration, and the member's offset names it.
    fn an_anonymous_member_is_a_union(&self, head: &str, view: &FileView) -> bool {
        let Some((parent, member)) = head.rsplit_once("::") else {
            return false;
        };
        let Some(written) = self.the_class_as_its_file_wrote_it(parent, &view.path) else {
            return false;
        };
        let Some(at) = written.members.get(member).copied() else {
            return false;
        };
        let Some(opener) = an_opening_brace_before(&written.text, at) else {
            return false;
        };
        the_nearest_class_keyword(&written.text[..opener]) == Some("union")
    }

    /// The layout of one of a class's **bases**, read in the class's own instantiation.
    fn the_layout_of_a_base(
        &self,
        view: &FileView,
        base: &str,
        depth: usize,
        instantiation: &Instantiation<'_>,
    ) -> Option<crate::sema::layout::ClassLayout> {
        let spelled = crate::sema::types::substituted_spelling_with_defaults(
            base,
            instantiation.parameters,
            instantiation.defaults,
            instantiation.arguments,
        );
        if mentions_a_parameter(&spelled, instantiation.parameters) {
            return None;
        }
        match self.the_layout_of_a_class_with(view, &spelled, &[], depth) {
            Known::Yes(layout) => Some(layout),
            _ => None,
        }
    }

    /// **The class as its own file wrote it** — the text, the offset of the class's name, and where each data member's
    /// name sits.
    ///
    /// A fact taken from a **cooked reading** carries offsets in that reading, which is a different text: the union's
    /// keyword and a bitfield's `:` are found in the file's own bytes. The summary's facts are the file's, so this is
    /// where a fact about spelling has to come from. One summary lookup per class, not per member.
    pub(crate) fn the_class_as_its_file_wrote_it(
        &self,
        written_type: &str,
        asked_from: &std::path::Path,
    ) -> Option<TheClassAsWritten> {
        let found = self
            .store
            .index()
            .definition(written_type, asked_from)
            .value()?;
        let text = self.files.read(&found.file)?;
        let summary = self.store.index().summary(&found.file)?;
        let simple = written_type.rsplit("::").next().unwrap_or(written_type);
        let class = summary.declarations.iter().find(|fact| {
            fact.name == simple
                && fact.kind == crate::DeclKind::Type
                && fact.parameter_list().is_none()
        })?;
        // **A member's `scope` is the class's qualified name**, which is the scope the class was written in plus its
        // own name — `Widget` at file scope, `ns::Widget` in a namespace.
        let qualified = match &class.scope {
            Some(ctxt) => format!("{ctxt}::{simple}"),
            None => simple.to_string(),
        };
        let members = summary
            .declarations
            .iter()
            .filter(|fact| {
                fact.kind == crate::DeclKind::Variable && fact.scope.as_deref() == Some(&qualified)
            })
            .map(|fact| (fact.name.to_string(), fact.name_range.end_offset()))
            .collect();
        Some(TheClassAsWritten {
            text: text.to_string(),
            class_at: class.name_range.start_offset,
            members,
            parameters: class.parameters().to_vec(),
            parameter_defaults: class.parameter_defaults().to_vec(),
            bases: class.bases().to_vec(),
        })
    }

    /// **Is this data member a bitfield?** — the text right after its name says so: `unsigned x : 3` has a `:` where
    /// an initializer would have `=`, and a ternary's `:` lives after the one that starts the initializer.
    pub(crate) fn a_bitfield(text: &str, at: usize) -> bool {
        let after = text.get(at..).unwrap_or_default();
        let end = after.find(';').unwrap_or(after.len().min(80));
        after[..end].trim_start().starts_with(':')
    }

    /// **A bitfield's width**, when it is a literal — `: 3`, `: (13)`, `: 2,` — at the same file offset
    /// [`Session::a_bitfield`] is asked about.
    ///
    /// `None` for a width that is an expression or a macro (`: BITS`): the class's layout after that point cannot be
    /// computed without it, and a card that guessed would be wrong in a way a reader could not see.
    pub(crate) fn a_bitfield_width(text: &str, at: usize) -> Option<usize> {
        let after = text.get(at..)?;
        let end = after.find(';').unwrap_or(after.len().min(120));
        let declaration = after[..end].trim_start();
        let rest = declaration.strip_prefix(':')?.trim_start();
        let (digits, after) = if let Some(parenthesised) = rest.strip_prefix('(') {
            let close = parenthesised.find(')')?;
            (
                parenthesised[..close].trim(),
                parenthesised[close + 1..].trim_start(),
            )
        } else {
            let end = rest
                .find(|character: char| !character.is_ascii_digit())
                .unwrap_or(rest.len());
            (&rest[..end], rest[end..].trim_start())
        };
        if digits.is_empty() || !digits.chars().all(|character| character.is_ascii_digit()) {
            return None;
        }
        // **The literal must be the whole width**, not the first term of `: 3 + 2` or the start of a name.
        if !after.is_empty() && !matches!(after.chars().next(), Some(';' | ',' | '}')) {
            return None;
        }
        digits.parse().ok()
    }

    /// The size and alignment of a member's written type, **in the instantiation the member is written in**.
    ///
    /// `instantiation` is `None` for a class that is not a template — the ordinary member — and the class's own
    /// parameters/arguments/members otherwise. The steps, in the order that keeps each one from answering the next
    /// one's question:
    ///
    /// 1. **Substitute** the parameters, with the declaration's own defaults for the arguments a use did not write.
    ///    A spelling that still mentions a parameter afterwards is **unknown** — looking it up by bare name is what
    ///    found an unrelated `_Ty2` in another header and reported `std::vector<widget::Widget>` as 9 bytes.
    /// 2. **A builtin** — the scalars, a pointer, an array of one of them — is the answer.
    /// 3. **A member alias** of the class being laid out (`_Alty`, `_Scary_val`, `Ptr`) is followed to its target,
    ///    substituted in the same environment.
    /// 4. **A class template applied to arguments** is the instantiation: its own layout, computed from *its*
    ///    members with *its* parameters bound to these arguments.
    /// 5. **A typedef** is the type it names, and everything else is an ordinary class.
    pub(crate) fn the_size_of_a_type_in(
        &self,
        view: &FileView,
        spelling: &str,
        depth: usize,
        instantiation: Option<&Instantiation<'_>>,
    ) -> Option<(usize, usize)> {
        if depth > 8 {
            return None;
        }
        let spelled = match instantiation {
            Some(environment) => crate::sema::types::substituted_spelling_with_defaults(
                spelling,
                environment.parameters,
                environment.defaults,
                environment.arguments,
            ),
            None => spelling.to_string(),
        };
        let spelled = spelled.trim();
        // **A qualifier is not part of what the type *is*, and every step below is a lookup by name.** `const
        // l_signalT` — Lua's `#define l_signalT sig_atomic_t` used as `volatile l_signalT trap;` — is not a class the
        // index has, not a typedef and not a builtin, so the member was "cannot be sized" and every class containing
        // it withheld its layout. Measured on `lua_State`: `hookmask` and `CallInfo::u::l::trap` are both spelled
        // `const l_signalT`, and with them unsized the card said "not computed" naming `base_ci` and `hookmask`.
        // `a_builtin_in_memory` strips qualifiers itself; this is the same stripping for the steps that ask the index.
        let spelled = spelled_without_its_qualifiers(spelled);
        if crate::probes::layout() {
            eprintln!("[layout] depth {depth} size of {spelled:?}");
        }
        if let Some(environment) = instantiation
            && mentions_a_parameter(spelled, environment.parameters)
        {
            if crate::probes::layout() {
                eprintln!("[layout]   still a parameter");
            }
            return None;
        }
        // **An array of anything, before the builtin words are read as scalars.** `int m[NMARKS]` ends in `]`; a
        // builtin reader that stops at the first word would answer four bytes for the whole array, and neovim's
        // `border_chars[8][MAX_SCHAR_SIZE]` would be one `char`. The dimension is often a macro or an enumerator —
        // `DB_COUNT`, `SNAP_COUNT`, `JUMPLISTSIZE`, `NSUBEXP`, `kWinOptCount`, `NMARKS` all are — so it is evaluated
        // rather than required to be a literal. A flexible array (`[]`) has zero extent; C's `sizeof` leaves it out.
        if let Some((element, dimension)) = split_an_array_dimension(spelled) {
            let count = self.the_value_of_an_array_dimension(view, dimension)?;
            let (element_size, element_alignment) =
                self.the_size_of_a_type_in(view, element, depth + 1, instantiation)?;
            return element_size
                .checked_mul(count)
                .map(|size| (size, element_alignment));
        }
        if let Some(builtin) = crate::sema::layout::a_builtin_in_memory(spelled) {
            return Some(builtin);
        }
        if let Some(environment) = instantiation
            && let Some(alias) = environment.members.iter().find(|member| {
                member.fact.kind == crate::DeclKind::Type
                    && member.fact.name == spelled
                    && member.fact.parameter_list().is_none()
            })
            && let Some(target) = alias.fact.type_of.clone()
        {
            return self.the_size_of_a_type_in(view, &target, depth + 1, instantiation);
        }
        // **A dependent type** — `typename allocator_traits<_Alloc>::template rebind_alloc<_Ty>`. Its owner is a
        // class template, and the member is an alias whose own parameters are the use's arguments; resolving it is
        // what turns a standard container's `_Alty` into a type this layer can size (or, for an empty allocator crop,
        // into nothing at all). Asked before the template-id branch because the tail after the last `::` is a
        // template-id too, and taking the head as a class would look up `rebind_alloc` as if it were one.
        if let Some((owner, member, arguments)) = split_a_dependent_name(spelled) {
            return self.the_size_of_a_dependent_type(view, &owner, &member, &arguments, depth);
        }
        if let Some((head, arguments)) = crate::index::project::split_template_arguments(spelled) {
            // **The arguments are read in the environment of the class being laid out.** `_Mypair`'s type is
            // `_Compressed_pair<_Alty, _Scary_val>`, and both spellings are *member aliases of `std::vector`* — so
            // they are resolved here, where that class's members are at hand, and `_Compressed_pair` is instantiated
            // with what they name.
            let resolved: Vec<String> = arguments
                .iter()
                .map(|argument| {
                    self.the_spelling_through_the_member_aliases(argument, instantiation)
                })
                .collect();
            match self.the_layout_of_a_class_with(view, &head, &resolved, depth + 1) {
                Known::Yes(layout) => {
                    return layout
                        .unknown
                        .is_empty()
                        .then_some((layout.size, layout.alignment));
                }
                // **Not a class: an alias template**, whose fact holds the spelling it expands to
                // (`_Rebind_alloc_t` is `typename allocator_traits<_Alloc>::template rebind_alloc<_Value_type>`).
                Known::No | Known::Unknown(_) => {}
            }
            if let Some(target) = self.the_target_of_an_alias_template(&head, &resolved, &view.path)
            {
                if crate::probes::layout() {
                    eprintln!("[layout]   alias template {head} -> {target}");
                }
                return self.the_size_of_a_type_in(view, &target, depth + 1, None);
            }
            if crate::probes::layout() {
                eprintln!("[layout]   no class and no alias template for {head} with {resolved:?}");
            }
            return None;
        }
        // **A typedef is the type it names.** Real C decides its members through one — neovim's `file_buffer` has
        // `pos_T b_op_start`, `handle_T handle`, `memline_T b_ml` — so without this the layout of most real structs
        // is "not computed". The alias's own fact holds the spelling it expands to.
        if let Known::Yes(aliased) = self.store.index().definition(spelled, &view.path)
            && let Some(target) = aliased.fact.type_of.as_deref()
            && target != spelled
            && depth <= 8
        {
            return self.the_size_of_a_type_in(view, target, depth + 1, instantiation);
        }
        let Known::Yes(nested) = self.the_layout_of_a_class_with(view, spelled, &[], depth + 1)
        else {
            return None;
        };
        if nested.unknown.is_empty() {
            return Some((nested.size, nested.alignment));
        }
        None
    }

    /// **A member type named through a class template instance** — `typename A<args>::template name<args2>`.
    ///
    /// The owner is instantiated by [`Session::members_of`] (which substitutes its parameters), and the member alias
    /// it names carries its own parameters — `allocator_traits<…>::rebind_alloc<_Other>` is `allocator<_Other>` — so
    /// the use's arguments are substituted into the target before it is sized.
    fn the_size_of_a_dependent_type(
        &self,
        view: &FileView,
        owner: &str,
        member: &str,
        arguments: &[String],
        depth: usize,
    ) -> Option<(usize, usize)> {
        let Known::Yes(list) = self.members_of(view, owner) else {
            return None;
        };
        let alias = list.members.iter().find(|candidate| {
            candidate.fact.name == member
                && candidate.fact.kind == crate::DeclKind::Type
                && candidate.fact.type_of.is_some()
        })?;
        let written = alias.fact.type_of.as_deref()?;
        let target = crate::sema::types::substituted_spelling_with_defaults(
            written,
            alias.fact.parameters(),
            alias.fact.parameter_defaults(),
            arguments,
        );
        // **A member whose type *is itself* declares an anonymous class in place.** `struct CallInfo { union { struct
        // { … } l; … } u; };` files the union under the member's name (`CallInfo::u`, which `members_of` answers), and
        // its fact's `type_of` is that same spelling — so following the target walked in a circle until the depth
        // bound, and the member was "cannot be sized". The class is real and the member list is what prices it, which
        // is the step below. Measured on Lua 5.5.0's `lstate.h`: `CallInfo::u`, `lua_State::transferinfo` and the
        // `CallInfo` that `lua_State::base_ci` is decided the class's whole layout.
        let itself = format!("{owner}::{member}");
        if target == itself {
            let Known::Yes(nested) = self.the_layout_of_a_class_with(view, &itself, &[], depth + 1)
            else {
                return None;
            };
            return nested
                .unknown
                .is_empty()
                .then_some((nested.size, nested.alignment));
        }
        self.the_size_of_a_type_in(view, &target, depth + 1, None)
    }

    /// **A spelling with the enclosing class's member aliases substituted into it** — `_Scary_val` becomes what
    /// `std::vector` declares it to be, in the instantiation being laid out.
    ///
    /// One alias, and only when the **whole spelling** is its name. Textual replacement of a name *inside* a longer
    /// spelling was measured and is not sound: the member aliases of a standard container name each other
    /// (`_Val_types`, `_Alvbase_traits`, `size_type`), and substituting one after another expands the spelling without
    /// bound — the trace printed megabytes for a single `std::vector<widget::Widget>`, splicing fragments of other
    /// classes' aliases wherever their names happened to occur. A name is resolved where its scope is known, which is
    /// the recursive class walk below, not a global text substitution.
    fn the_spelling_through_the_member_aliases(
        &self,
        spelling: &str,
        instantiation: Option<&Instantiation<'_>>,
    ) -> String {
        let Some(environment) = instantiation else {
            return spelling.to_string();
        };
        let spelled = crate::sema::types::substituted_spelling_with_defaults(
            spelling,
            environment.parameters,
            environment.defaults,
            environment.arguments,
        );
        let Some(alias) = environment.members.iter().find(|member| {
            member.fact.kind == crate::DeclKind::Type
                && member.fact.name == spelled.trim()
                && member.fact.parameter_list().is_none()
        }) else {
            return spelled;
        };
        match alias.fact.type_of.as_deref() {
            Some(target) => crate::sema::types::substituted_spelling_with_defaults(
                target,
                environment.parameters,
                environment.defaults,
                environment.arguments,
            ),
            None => spelled,
        }
    }

    /// **The target of an alias template applied to arguments** — `_Rebind_alloc_t<A, T>` is what its fact says,
    /// substituted.
    fn the_target_of_an_alias_template(
        &self,
        head: &str,
        arguments: &[String],
        path: &std::path::Path,
    ) -> Option<String> {
        let known = self.store.index().definition(head, path).value()?;
        let written = known.fact.type_of.as_deref()?;
        if known.fact.parameters().is_empty() {
            return None;
        }
        let target = crate::sema::types::substituted_spelling_with_defaults(
            written,
            known.fact.parameters(),
            known.fact.parameter_defaults(),
            arguments,
        );
        (target != head).then_some(target)
    }

    /// **The members `GENERATED_BODY()` writes that a macro cannot** — `Super` and `ThisClass`.
    ///
    /// Unreal's build tool writes `typedef <Base> Super;` and `typedef <TheClass> ThisClass;` into the generated code,
    /// so a project whose generated headers are present has them as ordinary members and this adds nothing. A project
    /// that is read from a **simulation** — the macro expanded to the name-independent half, `StaticClass()` and
    /// `GetPrivateStaticClass()` — has the annotation and not these, which is the case this exists for: measured on
    /// `UE-like`, `Super::BeginPlay()` and `ThisClass` were names nothing declared.
    ///
    /// The signal is `StaticClass`, which only the generated code (or a simulation of it) declares, and the absence of
    /// `ThisClass`, which says the generated code is not here. Outside `[unreal]` nothing is added: an ordinary C++
    /// class with a `StaticClass` member must not grow a `Super` it never declared.
    pub(crate) fn the_generated_members(
        &self,
        written_type: &str,
        path: &std::path::Path,
        listed: Known<crate::index::MemberList>,
    ) -> Known<crate::index::MemberList> {
        let unreal = self
            .project_config()
            .config
            .unreal
            .as_ref()
            .is_some_and(|unreal| unreal.is_on());
        if !unreal {
            return listed;
        }
        let Known::Yes(mut list) = listed else {
            return listed;
        };
        let declares_static_class = list
            .members
            .iter()
            .any(|member| member.fact.name == "StaticClass");
        let already_generated = list
            .members
            .iter()
            .any(|member| member.fact.name == "ThisClass");
        if !declares_static_class || already_generated {
            return Known::Yes(list);
        }
        let Known::Yes(declared) = self.store.index().definition(written_type, path) else {
            return Known::Yes(list);
        };
        // The synthetic members point at the class's own declaration: that is where the macro that generates them is
        // written, and it is the honest place for a hover to open.
        let class_fact = declared.fact.clone();
        let class_name = class_fact.name.to_string();
        for (name, type_of) in
            crate::project::ue::generated_members(&class_name, class_fact.bases())
        {
            let mut fact = class_fact.clone();
            fact.name = name.into();
            fact.kind = crate::DeclKind::Type;
            fact.type_of = Some(type_of.into());
            // A generated member is not a member of a **namespace**: it belongs to the class it was generated for.
            fact.scope = Some(class_name.as_str().into());
            list.members.push(crate::index::ProjectMember {
                file: declared.file.clone(),
                fact,
                declared_in: class_name.to_string(),
                depth: 0,
                ambiguous: false,
            });
        }
        Known::Yes(list)
    }

    /// **A type name as a declaration wrote it, resolved where it was written — through a base if it has to.**
    ///
    /// [`ProjectIndex::definition_where_written`] answers for a name written in a scope: it follows an alias,
    /// and it looks outward through the enclosing scopes the way C++ does. What it cannot answer is a member the
    /// class **inherits**, because that is not a lookup in the index at all — it is a walk of the base chain,
    /// and the walk lives here, where the file's scope tree and tree are, rather than in the index.
    ///
    /// Measured, and it is what this wire exists for:
    ///
    /// ```text
    /// std::allocator_traits::pointer        NotDeclaredHere   the name is inherited, not declared
    /// members_of("std::allocator_traits")   44 members        …and `pointer` is one of them
    /// ```
    ///
    /// `pointer` is declared in `_Normal_allocator_traits`, which `allocator_traits` names only inside a
    /// `conditional_t<…>` — so finding it needs the alias step, the outward scope step **and** the base walk,
    /// and this is the one entry point that has all three.
    ///
    /// # A diamond is `Ambiguous`, and that answer comes from the walk rather than from here
    ///
    /// A member two bases declare at once is not uniquely resolved by the language, and the walk records it as
    /// [`ProjectMember::ambiguous`]. Picking one would be a jump to an entity the user cannot tell from the
    /// other, which is the answer this project refuses to give.
    pub fn definition_of_a_written_type(
        &self,
        view: &FileView,
        written: &str,
        in_scope: Option<&str>,
        at: usize,
    ) -> Known<ProjectDefinition> {
        let index = self.store.index();

        // **A declaration inside a body has no scope to be named by, and the file's own scopes can supply
        // one.** [`DeclFact::scope`] is `None` for a local — a function body contributes no segment to a
        // qualified name — so a name it wrote has nothing to be looked up from, and a local `Widget` in
        // `namespace app` is a name that cannot be found. The view's scope chain at the declaration's own
        // offset says which namespaces enclose it.
        //
        // **Literal namespaces only, and that is the honest limit here.** A view is built with no macro
        // evidence (see [`FileView::parse`]), so `namespace app {` is a scope in it and `_STD_BEGIN` — whose
        // replacement list is in a header nothing here has read — is not. A name written inside a namespace the
        // file spells out is therefore resolvable, and one inside a namespace a macro opened is not; the second
        // needs the fact to carry the namespace, which is a change to what a summary stores rather than to what
        // this query asks.
        let enclosing = match in_scope {
            Some(scope) => Some(scope.to_string()),
            None => enclosing_namespace_at(view, at),
        };

        let direct = index.definition_where_written(written, enclosing.as_deref(), &view.path);
        if !matches!(direct, Known::Unknown(UnknownReason::NotDeclaredHere(_))) {
            return direct;
        }
        let in_scope = enclosing.as_deref();

        // **The class half, then the member.** The class is handed to [`Session::members_of`] **as written**,
        // because that query is the one that already knows how to turn a spelling into a class: it follows an
        // alias, it looks outward through the enclosing scopes, and it walks the bases. Resolving it here first
        // would be a second implementation of that, and it was one — asking
        // `ProjectIndex::definition_where_written` for `std::allocator_traits` answers `Unknown(Ambiguous(…))`,
        // because the class is forward-declared **and** defined, and an `Ambiguous` read as "no class" is the
        // same mistake this file's own `is_declared` and `direct_members` each had to have fixed.
        let Some((class, member)) = written.rsplit_once("::") else {
            return direct;
        };

        // **Qualified by the scope first, then as written** — and the order is the whole of the care here. A
        // class named unqualified inside another one is written relative to it, so `_Mybase` in
        // `std::_Vb_const_iterator` means `std::_Vb_const_iterator::_Mybase` and not the bare name. Trying the
        // bare spelling first was the first attempt, and it stopped the walk on the wrong class: `members_of`
        // answered `Yes` for `_Mybase` — there is a class by that name somewhere — and the loop took that as the
        // class to look the member up in, so the scope-qualified spelling was never reached. Measured, that is
        // `_Mybase::_Mycont` at `std::_Vb_const_iterator`: unresolved with the bare spelling tried first, and
        // `Yes` when the same name is asked in full.
        let mut tried: Vec<String> = Vec::new();
        if let Some(scope) = in_scope.filter(|_| !class.starts_with("::") && !class.contains("::"))
        {
            tried.push(format!("{scope}::{class}"));
        }
        tried.push(class.to_string());

        let mut list = None;
        for spelling in &tried {
            if let Known::Yes(found) = self.members_of(view, spelling) {
                list = Some(found);
                break;
            }
        }

        let Some(list) = list else {
            return direct;
        };

        let mut found = list
            .members
            .iter()
            .filter(|found| found.fact.name == member);

        let Some(only) = found.next() else {
            // Nothing in the chain declares it, and the honest answer is the one the direct lookup gave: the
            // name is not here, rather than a name nobody wrote.
            return direct;
        };

        // **Two answers, or one answer the walk already called ambiguous.** A member that two bases declare at
        // once is not uniquely resolved by the language, and a jump to either is a jump to an entity the user
        // cannot tell from the other.
        if found.next().is_some() || only.ambiguous {
            return Known::Unknown(UnknownReason::Ambiguous(Box::from(written)));
        }

        Known::Yes(ProjectDefinition {
            file: only.file.clone(),
            fact: only.fact.clone(),
        })
    }

    /// **The file's declarations as a tree** — what an outline, a breadcrumb bar or a folding range is drawn from.
    ///
    /// # One source: the file's own text
    ///
    /// An outline is about the **file**, not about the program: a declaration in a branch nobody takes belongs in an
    /// outline, and a type a macro declares does not. So this reads the view it is given — the file's own parse
    /// ([`Session::view_of_the_file`]) — and never the index's summary.
    ///
    /// **That used to be worded "the index's summary when it has one, and the buffer's own parse
    /// when it does not", and the difference is no longer a fallback but a wrong answer.** While a
    /// summary was the file's own text reading, the two sources agreed about everything an outline
    /// shows. Measured in `cpp_ls`'s `the_outline_shows_the_file_and_not_the_compiler`, which is
    /// what says the two readings must not be mixed here.
    ///
    /// # What it costs, and why that is not a regression
    ///
    /// One parse of the file's own bytes per request (the buffer, which the VFS already holds), and no dependence on a
    /// rendering existing — so the "summary is gone between a keystroke and the next drain" case that the fallback
    /// existed for is now the only case, and the panel never waits for a cook.
    pub fn outline(&self, view: &FileView) -> Vec<OutlineSymbol> {
        let preprocessing = crate::preprocess(&view.source, view.tree.get_tokens());
        let errors: Vec<cpp_parser::SourceRange> = view
            .tree
            .get_errors()
            .iter()
            .map(|error| cpp_parser::source_range(error.range))
            .collect();
        let (facts, _guards) =
            crate::build_facts(&view.scopes, &preprocessing, &view.root, &errors, true);

        crate::outline_of(&facts)
    }
}

/// **A class as its own file wrote it** — the text, where its name sits, where each data member's name sits, and the
/// template parameters it declares.
///
/// The last field is what makes a layout honest: `_Ty2 _Mysecond;` inside `template <class _Ty1, class _Ty2> class
/// _Compressed_pair` has the size of whatever the instantiation puts there, and a lookup of `_Ty2` by bare name in
/// the asking file finds some *other* class spelled `_Ty2` — measured on `std::vector<widget::Widget>`, whose card
/// said "size 9 byte(s), alignment 1" from exactly that. A member written in one of the class's parameters is
/// therefore **unknown**, which withholds the layout rather than printing a number beside a guess (see
/// [`crate::sema::layout::ClassLayout::unknown`]).
pub(crate) struct TheClassAsWritten {
    pub text: String,
    pub class_at: usize,
    pub members: std::collections::HashMap<String, usize>,
    pub parameters: Vec<String>,
    pub parameter_defaults: Vec<String>,
    pub bases: Vec<String>,
}

/// **The instantiation a class's members are read in** — its parameters, the arguments a use wrote (or the
/// declaration's own defaults), and the class's members, which is where a *member alias* such as `_Alty` lives.
pub(crate) struct Instantiation<'a> {
    pub parameters: &'a [String],
    pub defaults: &'a [String],
    pub arguments: &'a [String],
    pub members: &'a [crate::index::ProjectMember],
}

/// Does a type spelling use one of these names as a **word**? _Ty2 matches _Ty2* and ector<_Ty2>; _Ty20 does
/// not match _Ty2.
/// **A type named through another type** — `typename A<args>::template name<args2>` is `(A<args>, name, [args2])`.
///
/// The `::` that matters is the last one at angle-bracket depth zero: `allocator_traits<allocator<int>>::rebind_alloc`
/// has three `:` pairs, and the last is the separator while the `std::`-shaped prefixes belong to the owner. Only a
/// name whose **owner is an instantiation** is a dependent type here; `ns::Widget` is a qualified name, which the
/// ordinary lookups already answer.
fn split_a_dependent_name(spelling: &str) -> Option<(String, String, Vec<String>)> {
    let trimmed = spelling.trim().trim_start_matches("typename ").trim();
    let (owner, tail) = split_at_the_last_qualifier(trimmed)?;
    if !owner.contains('<') {
        return None;
    }
    let tail = tail.trim().trim_start_matches("template ").trim();
    if tail.is_empty() {
        return None;
    }
    match crate::index::project::split_template_arguments(tail) {
        Some((member, arguments)) => Some((owner.to_string(), member, arguments)),
        None => Some((owner.to_string(), tail.to_string(), Vec::new())),
    }
}

/// The last `::` outside angle brackets, split into what is on either side.
fn split_at_the_last_qualifier(text: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    let mut at = None;
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'<' => depth += 1,
            b'>' => depth = depth.saturating_sub(1),
            b':' if depth == 0 && bytes.get(index + 1) == Some(&b':') => at = Some(index),
            _ => {}
        }
    }
    let at = at?;
    Some((&text[..at], &text[at + 2..]))
}

fn mentions_a_parameter(spelling: &str, parameters: &[String]) -> bool {
    if parameters.is_empty() {
        return false;
    }

    spelling
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .any(|word| parameters.iter().any(|parameter| parameter == word))
}

#[cfg(test)]
mod tests {
    use super::a_constant_expression;

    #[test]
    fn a_constant_expression_reads_what_a_dimension_writes() {
        for (text, expected) in [
            ("8", 8usize),
            ("(53)", 53),
            ("10u", 10),
            ("0x10", 16),
            ("1 << 4", 16),
            ("('z' - 'a' + 1)", 26),
        ] {
            assert_eq!(a_constant_expression(text), Some(expected), "{text}");
        }
        for text in ["sizeof(x)", "COUNT", "8 /* n */", "1 +"] {
            assert_eq!(a_constant_expression(text), None, "{text}");
        }
    }
}
