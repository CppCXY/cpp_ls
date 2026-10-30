//! **What a type is**, as a shape rather than as a spelling.
//!
//! Everything around this module used to speak about types in `String`s: a declaration recorded the type it was
//! written with, a member access asked a class by the spelling it read, and the inference layer built answers by
//! cutting and pasting those strings (`pointee_type_name`, `element_type_name`, `auto_substituted`). That works
//! while a type is one word and comes apart the moment it is not — and it comes apart **silently**, because the
//! failure mode is a `String` that looks like a type and is not one:
//!
//! ```text
//! std::map<std::string, int>   handed to a class query whole              → nothing declares `std::map<std::string, int>`
//! _Cont>                       a template argument whose cut went wrong   → nothing declares `_Cont>`
//! const [[nodiscard]] …        attributes and specifiers left in          → not a type at all
//! ```
//!
//! # The shape is modelled, the spelling is carried
//!
//! ```text
//! Type::Builtin { spelling }          `int`, `unsigned long long`, `bool`
//! Type::Named { name, arguments }     `Widget`, `std::vector<int>`, `T`
//! Type::TemplateParameter { name }    `T` in a context where nothing has said what it stands for
//! Type::Pointer { to }                `Widget*`
//! Type::Reference { to, rvalue }      `Widget&`, `Widget&&`
//! Type::Array { of, extent }          `int[4]`, `int[]`
//! Type::Function { returns, … }       `int(int)`, `int (*)(int)`
//! Type::Pack { of }                   `Args...`
//! Type::Qualified { of }              `const Widget`, `volatile int`
//! ```
//!
//! Three of those exist for reasons worth stating, because each replaced a bug rather than a gap:
//!
//! * **`Named` carries a name without its arguments** and the arguments beside it, which is the one distinction a
//!   member query needs: `std::vector<int>` is a *type*, `std::vector` is the *class* whose members it has, and a
//!   query handed the first answered "nothing declares it" — true, and useless.
//! * **`Builtin` is separate from `Named`** so that `int` is not a class called `int`. Reading it as a name made a
//!   member access on an `int` ask the index for a class by that name and report *"not declared here"*, which a
//!   reader takes as "this class is missing" rather than "this is not a class".
//! * **`Qualified` is a wrapper** rather than a flag, because a qualifier belongs to the type it is written on:
//!   `const Widget*` and `Widget* const` are the same three words in two places and two different types, and only
//!   the position tells them apart.
//!
//! The **spelling** is kept for two reasons, and "why not intern everything" is the obvious question:
//!
//! * a consumer shows the type to a user, and the answer must be *what the file wrote* — `std::size_t` and
//!   `unsigned long long` are one type and two different things to read, and this layer is not entitled to rewrite
//!   a file's own spelling;
//! * an **alias** is a name, not a definition: `using Int = int;` makes `Int` a type whose structure this layer does
//!   not have unless it resolves the alias, which is the index's job and a separate question.
//!
//! So the structure is what is *modelled* — enough to ask "what does this type name", "what does a `*` do to it",
//! "what are its template arguments" — and the spelling is what is *carried*. Equality is by spelling, which is the
//! honest rule for a layer that does not resolve aliases: two spellings that mean one type are two answers here.
//!
//! # The two ways in
//!
//! ```text
//! type_of_declaration(specifiers, declarator, name)   the syntax: what a declaration wrote
//! parse_type_spelling("std::vector<int>")             the text: what a `DeclFact` recorded on disk
//! ```
//!
//! Both are needed and neither can replace the other: a declaration in the file being edited has syntax and no
//! need of a spelling, while a declaration the *index* holds is a `String` whose file is not even open. The parser
//! guarantees exactly one thing — [`Type::class_name`] — because everything else it could get wrong is recoverable
//! and a wrong class name is not.
//!
//! # What this deliberately does not do
//!
//! No alias resolution, no template instantiation, no `typedef` chasing, no overload resolution. Each of those is a
//! query against the index and belongs where the index is; this module is the shape they are asked about. What it
//! *does* offer towards them is [`Type::substituted`], which pairs a class template's parameters with the arguments
//! a use wrote — the operation an instantiation would start from, and the one that makes
//! `std::vector<T>::size_type` answerable once `T` is known.

use std::fmt;
use std::sync::Arc;

use cpp_parser::{CppSyntaxKind, CppSyntaxNode};

/// A type inside a type, **shared rather than owned**.
///
/// Every compound type is a tree, and inference walks that tree constantly: a `substituted`, a `decay`, a
/// `pointee`, a `class_name` — each one asks about the inside of a type and each one produces a new type. With an
/// owned child (`TypeOf`) every one of those is a **deep clone of a tree**, so asking "what is `std::map<K,
/// V>::value_type`" clones the whole spelling of `K` and `V` to look at it, and a chain of member accesses clones
/// it once per link. The cost is invisible in a test with three identifiers and dominant in a header where the
/// same type is asked about a thousand times.
///
/// [`Arc`] makes the clone a refcount bump, and it buys three things that matter more than the atomic:
///
/// ```text
/// 1. a sub-type can be handed out        `fn inner(&self) -> TypeOf<Type>` returns a handle, not a copy
/// 2. one spelling is one allocation      `std::string` built a thousand times is one allocation, not a thousand
/// 3. nothing is mutated through it       `Arc` has no `get_mut` in safe code, so a type is a value
/// ```
///
/// The third is not a consolation prize: a type that could be mutated in place is a cache key that can change
/// under a map, and this layer hashes types. It is [`Arc`] rather than `Rc` because the index is shared between
/// the LSP's reader threads — a type crossing a thread boundary is the ordinary case here, not an exotic one.
pub type TypeOf = Arc<Type>;

/// **A type, as a shape.** See the module documentation for why the spelling is carried beside it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    /// A builtin: `int`, `void`, `unsigned long long`, `decltype(auto)`.
    Builtin {
        spelling: String,
    },
    /// A name, with the template arguments it was written with — `Widget`, `std::vector<int>`, `T`.
    ///
    /// Declared separately from [`Type::TemplateParameter`] because the difference is a *fact about the context*
    /// rather than about the text: `T` inside `template <class T> class vector` is a parameter, and `T` in a file
    /// that has a `struct T` is a class. This variant is the reading that does not know yet; a caller that has the
    /// template parameter list in hand says so, and [`Type::TemplateParameter`] is that answer.
    Named {
        name: String,
        arguments: Vec<Type>,
    },
    /// A template parameter written where nothing has said what it stands for: the `T` of a template body.
    TemplateParameter {
        name: String,
    },
    Pointer {
        to: TypeOf,
    },
    Reference {
        to: TypeOf,
        /// `&&` rather than `&`. Kept apart because it is the one difference deduction turns on — see
        /// `a_forwarding_reference_is_not_deduced`, which is the rule this field exists for.
        rvalue: bool,
    },
    Array {
        of: TypeOf,
        /// `Some(4)` for `int[4]`, `None` for `int[]`.
        extent: Option<usize>,
    },
    /// A function type: what it returns, and how many parameters it takes.
    Function {
        returns: TypeOf,
        parameters: Vec<Type>,
    },
    /// A `pack`: `Args...` — a template parameter that stands for several arguments.
    Pack {
        of: TypeOf,
    },
    /// **A qualified type**: `const Widget`, `volatile int`.
    ///
    /// A wrapper rather than a flag on every variant, because a qualifier belongs to *the type it is written on*
    /// and the position is the whole of its meaning: `const Widget*` is a pointer to a const Widget and
    /// `Widget* const` is a const pointer, the same words in a different place, and the operators between them are
    /// what tells the two apart. A wrapper is also the shape a peel can build at the moment it knows which type the
    /// qualifier was written on, which is exactly when it reads it.
    Qualified {
        of: TypeOf,
    },
}

impl Type {
    pub fn builtin(spelling: impl Into<String>) -> Type {
        Type::Builtin {
            spelling: spelling.into(),
        }
    }

    pub fn named(name: impl Into<String>) -> Type {
        Type::Named {
            name: name.into(),
            arguments: Vec::new(),
        }
    }

    /// **The class this type names**, for a caller about to ask a class question — a member list, a base chain.
    ///
    /// This is the method the whole module exists for. `std::vector<int>` is a *type* and `std::vector` is the
    /// *class* whose members it has; a query that was handed the first and wanted the second answered "nothing
    /// declares `std::vector<int>`" — which is true and useless.
    ///
    /// `None` for a type that names no class: a builtin, a pointer (the caller decays first — see
    /// [`Type::decay`]), an array, a function, a template parameter. A reference **is** transparent here: C++ says
    /// `r.size` on a `Widget&` is a member of `Widget`, and the reference is the binding's business, not the
    /// member's.
    pub fn class_name(&self) -> Option<&str> {
        match self {
            Type::Named { name, .. } if !name.is_empty() => Some(name),
            // A reference and a qualifier are both **transparent** to this question, and C++ is why: a member of a
            // `Widget&` is a member of `Widget`, and a member of a `const Widget` is a member of `Widget` — you
            // cannot assign to it, which is a fact about assignment and not about the class.
            Type::Reference { to, .. } | Type::Qualified { of: to } => to.class_name(),
            _ => None,
        }
    }

    /// The type's template arguments, when it has a name to hang them on.
    pub fn arguments(&self) -> &[Type] {
        match self {
            Type::Named { arguments, .. } => arguments,
            Type::Reference { to, .. } | Type::Qualified { of: to } => to.arguments(),
            _ => &[],
        }
    }

    /// **What an expression of this type is when it is *used*** — the two conversions C++ applies before almost
    /// every operator.
    ///
    /// * an **lvalue-to-rvalue** conversion, which drops the top-level reference: `Widget& w` used as a value is a
    ///   `Widget`, and a member access on it is a member of `Widget` either way;
    /// * an **array-to-pointer** decay: `int arr[4]` used as a value is an `int*`, which is why `arr[0]` and
    ///   `p[0]` are the same question.
    ///
    /// Functions decay to function pointers, which is spelled rather than modelled — see [`Type::Function`], whose
    /// callers are a call (`f()`) and an address-of (`&f`), and neither of them needs the pointer.
    ///
    /// This is deliberately **not** applied by the reader: a declaration's type is what the declaration wrote
    /// (`int arr[4]` is an array), and the decay belongs to the use. A layer that decayed at the declaration
    /// would report `arr` as `int*`, which is a different declaration than the one the file has.
    /// # The decay is **one level**, and that is the rule rather than a simplification
    ///
    /// `int[2][3]` decays to a pointer to the element of the *outer* array, which is itself an array: `int[2]*`. An
    /// implementation that decayed the element too would answer `int**`, a different type — and the difference
    /// shows in what a subscript on it gives. It also shares the element rather than copying it, which is what
    /// [`Arc`] is there for.
    pub fn decay(&self) -> Type {
        match self {
            Type::Reference { to, .. } => to.decay(),
            Type::Array { of, .. } => Type::Pointer { to: Arc::clone(of) },
            // **A qualifier survives the decay**, because the thing it qualifies does: `const int arr[4]` used as a
            // value is a pointer to a const `int`. Dropping it here would make `const` disappear from exactly the
            // types a `auto p = &arr;` deduces.
            Type::Qualified { of } => Type::Qualified {
                of: Arc::new(of.decay()),
            },
            other => other.clone(),
        }
    }

    /// **Can a value of this type initialise one of `target`?**
    ///
    /// The one relation the two type checks are made of — `int x = "three";` is an assignment and `f("three")` is a
    /// parameter, and both are the same question asked at a different place. Written once here so that they cannot
    /// disagree, which is the rule this crate keeps having to relearn: one question, one implementation.
    ///
    /// # Only what is certain
    ///
    /// `Known::Yes` and `Known::No` are answers; `Known::Unknown` is the absence of one, and a check reports only a
    /// definite `No` — see `sema::check`'s contract. So this relation is deliberately **partial**: it answers for the
    /// conversions the standard makes unconditionally and refuses to guess at everything that needs more of the
    /// program than two types.
    ///
    /// What it answers:
    ///
    /// ```text
    ///   identical after decay                  -> Yes     `int` to `int`, `const char*` to `const char*`
    ///   arithmetic to arithmetic               -> Yes     `double` to `int`, `char` to `long`
    ///   array or function to pointer           -> Yes     `int[4]` to `int*`
    ///   pointer to arithmetic, or the reverse  -> No      `const char*` to `int`
    ///   anything to `void`, or `void` to it    -> No      no object has type `void`
    ///   anything named, or a template parameter-> Unknown `S` may have a converting constructor
    ///   pointer to a different pointee         -> Unknown `Derived*` to `Base*` is a real conversion
    ///   a reference target                     -> the referent's question     `const int&` asked as `int`
    /// ```
    ///
    /// # Why a class is never an answer
    ///
    /// `std::string s = "x";` is a conversion *constructor* and `int n = widget;` may be a conversion *operator*, and
    /// neither is visible in the two types: the first needs the class's constructors and the second needs its
    /// members. Answering either from the spellings alone is the invented finding the check layer exists to avoid —
    /// and `std::string s = "x";` is the most ordinary line in modern C++.
    pub fn convertible_to(&self, target: &Type) -> crate::sema::symbol::Known<()> {
        use crate::sema::symbol::Known;

        let from = self.decay();
        // **The target is not decayed**, and that is the whole of a measured false positive: MSVC's
        // `__msvc_formatter.hpp` declares `_CharT _Fill[]`, whose type is an **array** of a template parameter, and
        // decaying the target turned it into `_CharT*` — so `int` met "a pointer" and the relation answered `No` for
        // a pair it cannot decide at all. Array-to-pointer is a conversion the *source* undergoes; the destination
        // of an initialisation keeps whatever shape it was written with.
        //
        // **A reference binds to its referent**, so the question is the one about the referent. `void
        // push_back(const _Ty&)` instantiated at `int` declares `const int&`, and a `const char*` argument against
        // it is the same mistake as against `int` — but this relation had no arm for a `Reference` on the right and
        // answered `Unknown`, which is why `anotherVec.push_back("123131")` on a `std::vector<int>` was never
        // reported while the same mistake against a by-value `int` parameter was. Nothing is invented by the peel:
        // binding a value to a reference of a type it does not convert to is exactly what the language rejects.
        let to = match target {
            Type::Reference { to, .. } => &**to,
            other => other,
        };

        // **Identical is always convertible**, before anything else is asked: a spellings-equal pair needs no rule,
        // and this is what makes `int` to `int` and a pointer to its own type answer `Yes` rather than falling into
        // the "different pointee" case below.
        if &from == to {
            return Known::Yes(());
        }

        // **`void` is not a value.** An object cannot have it, so nothing converts *to* it and it converts to
        // nothing — written before the arithmetic rule, which would otherwise call `void` an arithmetic type and
        // answer `Yes` for `void x = 1;`.
        if from.is_void() || to.is_void() {
            return Known::No;
        }

        match (&from, to) {
            // The standard conversions between arithmetic types, which are unconditional.
            (a, b) if a.is_arithmetic() && b.is_arithmetic() => Known::Yes(()),

            // Array-to-pointer and function-to-pointer. `decay` already did this, so a pair that arrives here with
            // one side an array did not come through `decay` — kept for a caller that built the types by hand.
            (Type::Array { .. } | Type::Function { .. }, Type::Pointer { .. }) => Known::Yes(()),

            // **A pointer and a number are not interconvertible here.** A literal `0` is, and that is a fact about
            // the *value* rather than about the type this relation is handed — so it is `Unknown`'s neighbour rather
            // than its case: a caller that knows the initialiser was the literal `0` does not ask.
            (Type::Pointer { .. }, b) if b.is_arithmetic() => Known::No,
            (a, Type::Pointer { .. }) if a.is_arithmetic() => Known::No,

            _ => Known::Unknown(crate::sema::symbol::UnknownReason::UnknownType(Box::from(
                "a conversion between these two types is not decided by their shapes",
            ))),
        }
    }

    /// Is this spelling one of the arithmetic types?
    pub fn is_arithmetic(&self) -> bool {
        match self {
            Type::Builtin { spelling } => !matches!(spelling.as_str(), "void" | "decltype(auto)"),
            Type::Qualified { of } => of.is_arithmetic(),
            _ => false,
        }
    }

    /// Is this `void` — the incomplete type no object can have?
    pub fn is_void(&self) -> bool {
        match self {
            Type::Builtin { spelling } => spelling == "void",
            Type::Qualified { of } => of.is_void(),
            _ => false,
        }
    }

    /// The type a `*` on this one gives: the pointee, for a pointer or an array — **as a shared handle**, because
    /// the pointee is already an allocation inside this type and a caller asking for it is looking at it rather
    /// than taking it away.
    ///
    /// `None` for everything else, **including a reference**: a reference has no `operator*`, and the reason a
    /// caller wants a pointee is to apply one. `Type::Reference` is seen through by [`Type::class_name`] and
    /// [`Type::decay`], where C++ does see through it, and not here, where it does not. Nor for a `Named` type that
    /// has an `operator*` of its own — that needs the class's members and therefore the index. A caller that gets
    /// `None` has an ill-formed `*` **or** a class this layer cannot look inside, and those are the same answer at
    /// this level.
    pub fn pointee(&self) -> Option<TypeOf> {
        match self {
            Type::Pointer { to } | Type::Array { of: to, .. } => Some(Arc::clone(to)),
            // **A qualifier is transparent here too**, and C++ is again why: `*p` on a `const Widget*` gives a
            // `const Widget`, which is the pointee. Only the operators are peeled.
            Type::Qualified { of } => of.pointee(),
            _ => None,
        }
    }

    /// Is this a template parameter, or does it *contain* one anywhere?
    ///
    /// The predicate a caller uses to tell "the answer is not known yet" from "the answer is not here": a type
    /// holding a template parameter is one that only instantiation can finish, and this analysis does not
    /// instantiate. It is a fact about the type rather than about a position, which is what makes it cheap enough
    /// to ask before every index query.
    pub fn depends_on_a_parameter(&self) -> bool {
        match self {
            Type::TemplateParameter { .. } => true,
            Type::Named { arguments, .. } => arguments.iter().any(Type::depends_on_a_parameter),
            Type::Pointer { to }
            | Type::Reference { to, .. }
            | Type::Pack { of: to }
            | Type::Qualified { of: to } => to.depends_on_a_parameter(),
            Type::Array { of, .. } => of.depends_on_a_parameter(),
            Type::Function {
                returns,
                parameters,
            } => {
                returns.depends_on_a_parameter()
                    || parameters.iter().any(Type::depends_on_a_parameter)
            }
            Type::Builtin { .. } => false,
        }
    }

    /// **This type with the template parameters in `substitutions` replaced** — the one operation that makes a
    /// member of a class template answerable.
    ///
    /// `std::vector<T>::size_type` with `T = int` is `std::size_t`, and the substitution is by **name**: the
    /// parameters of a class template are a fixed list, the arguments are written at the use, and the pairing
    /// between them is positional. This is not instantiation — no body is re-read, no overload is chosen, no
    /// dependent name is resolved — and the limits are worth stating rather than discovering:
    ///
    /// * a parameter that appears **inside an expression** (`T::value_type`, `decltype(sizeof(T))`) is
    ///   substituted as a name and not evaluated: the first is answered because it is a named type, the second is
    ///   not a type at all and was refused when it was read;
    /// * a **dependent** name (`typename T::value_type`) has nothing to substitute *into* until `T` is known, and
    ///   this returns it unchanged — see [`Type::depends_on_a_parameter`], which is how a caller avoids asking a
    ///   question whose answer this cannot be;
    /// * a **partial** substitution is the ordinary case: `std::pair<T, int>` with `T` unknown substitutes the
    ///   `int`, which is what makes `second` answerable and `first` not.
    pub fn substituted(&self, substitutions: &TypeSubstitutions<'_>) -> Type {
        match self {
            // **A name that is in the parameter list *is* the parameter**, and this arm is why the substitution can
            // be done at all for a member read out of a header. A template parameter is not a kind of type that a
            // reader can recognise from the text: `template <class _Ty> struct vector { _Ty& front; };` writes
            // `_Ty` and spells it exactly like a class name, because at that point it *is* one — what makes it a
            // parameter is the list it was declared in, and that list belongs to the class rather than to the
            // member. So the pairing is asked, by name, and an empty list means the name is an ordinary class.
            //
            // A class genuinely called `_Ty`, used inside a class template whose parameters are named `_Ty`, would
            // be substituted wrongly. That is not a readable program (the parameter hides the class inside its own
            // body — C++ would resolve it to the parameter too), so this is the language's rule rather than a
            // guess, and getting it wrong in the other direction costs every member of every standard container.
            Type::Named { name, arguments } => {
                if arguments.is_empty()
                    && let Some(argument) = substitutions.get(name)
                {
                    return argument.clone();
                }

                Type::Named {
                    name: name.clone(),
                    arguments: arguments
                        .iter()
                        .map(|argument| argument.substituted(substitutions))
                        .collect(),
                }
            }
            Type::TemplateParameter { name } => substitutions
                .get(name)
                .cloned()
                .unwrap_or_else(|| self.clone()),
            Type::Pointer { to } => Type::Pointer {
                to: Arc::new(to.substituted(substitutions)),
            },
            Type::Reference { to, rvalue } => Type::Reference {
                to: Arc::new(to.substituted(substitutions)),
                rvalue: *rvalue,
            },
            Type::Array { of, extent } => Type::Array {
                of: Arc::new(of.substituted(substitutions)),
                extent: *extent,
            },
            Type::Function {
                returns,
                parameters,
            } => Type::Function {
                returns: Arc::new(returns.substituted(substitutions)),
                parameters: parameters
                    .iter()
                    .map(|parameter| parameter.substituted(substitutions))
                    .collect(),
            },
            Type::Pack { of } => Type::Pack {
                of: Arc::new(of.substituted(substitutions)),
            },
            Type::Qualified { of } => Type::Qualified {
                of: Arc::new(of.substituted(substitutions)),
            },
            Type::Builtin { .. } => self.clone(),
        }
    }
    /// **This type with one name in it replaced** — `value_type&` with `value_type` = `char` is `char&`.
    ///
    /// The companion of [`Type::substituted`], and the difference is what is being replaced: that one replaces a
    /// *template parameter* by the argument a use paired with it, this one replaces a name by whatever that name
    /// turned out to mean. Both walk the same structure for the same reason — spellings cannot be edited as text —
    /// and the walk is the whole point here: `value_type&` is not a bare name, so a reader that only ever looked at
    /// whole spellings stopped one step short of the answer.
    ///
    /// Measured, in MSVC's `<xstring>`: `back()` returns `reference`, which is `value_type&`, which is `_Ty&`,
    /// which is `char&` for a `std::string`. Every step is a member alias of the same class, and every step after
    /// the first is the **base** of a type rather than the whole of it.
    ///
    /// The operators around the name stay where they are, which is the language's rule: `value_type&` with
    /// `value_type` = `_Ty*` is `_Ty*&` — a reference to a pointer — and dropping either would answer a different
    /// type. A name carrying template arguments of its own (`value_type<int>`) is deliberately **not** replaced:
    /// the arguments would have to be substituted into the replacement, and a member alias of a class this walk is
    /// about is not a template in any case it exists for.
    pub fn replacing(&self, name: &str, with: &Type) -> Type {
        match self {
            Type::Named {
                name: found,
                arguments,
            } if arguments.is_empty() && found == name => with.clone(),
            Type::Named {
                name: found,
                arguments,
            } => Type::Named {
                name: found.clone(),
                arguments: arguments
                    .iter()
                    .map(|argument| argument.replacing(name, with))
                    .collect(),
            },
            Type::TemplateParameter { .. } | Type::Builtin { .. } => self.clone(),
            Type::Pointer { to } => Type::Pointer {
                to: Arc::new(to.replacing(name, with)),
            },
            Type::Reference { to, rvalue } => Type::Reference {
                to: Arc::new(to.replacing(name, with)),
                rvalue: *rvalue,
            },
            Type::Array { of, extent } => Type::Array {
                of: Arc::new(of.replacing(name, with)),
                extent: *extent,
            },
            Type::Function {
                returns,
                parameters,
            } => Type::Function {
                returns: Arc::new(returns.replacing(name, with)),
                parameters: parameters
                    .iter()
                    .map(|parameter| parameter.replacing(name, with))
                    .collect(),
            },
            Type::Pack { of } => Type::Pack {
                of: Arc::new(of.replacing(name, with)),
            },
            Type::Qualified { of } => Type::Qualified {
                of: Arc::new(of.replacing(name, with)),
            },
        }
    }
}

impl fmt::Display for Type {
    /// The type **as a file would write it**, which is what a consumer shows: the spelling each part was read
    /// with, put back together in the order the syntax wrote it.
    /// This is a writer for a human, not a parser's inverse: `int* const` and `const int*` differ by which side
    /// the `const` was on, and that difference is in the spelling of the parts rather than in the shape. Where
    /// this layer *does* know better than the source — a class named by a template's arguments — it still writes
    /// what the source wrote, because a consumer comparing the answer against the file is the ordinary case.
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Builtin { spelling } => write!(out, "{spelling}"),
            Type::TemplateParameter { name } => write!(out, "{name}"),
            Type::Named { name, arguments } => {
                write!(out, "{name}")?;
                if !arguments.is_empty() {
                    write!(out, "<")?;
                    for (at, argument) in arguments.iter().enumerate() {
                        if at > 0 {
                            write!(out, ", ")?;
                        }
                        write!(out, "{argument}")?;
                    }
                    write!(out, ">")?;
                }
                Ok(())
            }
            Type::Pointer { to } => write!(out, "{to}*"),
            Type::Reference { to, rvalue } => {
                if *rvalue {
                    write!(out, "{to}&&")
                } else {
                    write!(out, "{to}&")
                }
            }
            Type::Array { of, extent } => match extent {
                Some(size) => write!(out, "{of}[{size}]"),
                None => write!(out, "{of}[]"),
            },
            // **A pointer or an array in the return position needs its parentheses**, and they go around the
            // *declarator* rather than around the whole type: `void (*)(int)` is a pointer to a function, while
            // `(void*)(int)` and `void*(int)` read as a function returning a pointer — the one mistake this reader
            // is written not to make. The language's rule is that a declarator's operators bind in the order they are
            // parenthesised, so writing the shape back has to put the parentheses in the same place.
            Type::Function {
                returns,
                parameters,
            } => {
                match &**returns {
                    pointer @ Type::Pointer { .. } => write!(out, "{}", pointer.as_a_declarator())?,
                    array @ Type::Array { .. } => write!(out, "{}", array.as_a_declarator())?,
                    other => write!(out, "{other}")?,
                }
                write!(out, "(")?;
                for (at, parameter) in parameters.iter().enumerate() {
                    if at > 0 {
                        write!(out, ", ")?;
                    }
                    write!(out, "{parameter}")?;
                }
                write!(out, ")")
            }
            Type::Pack { of } => write!(out, "{of}..."),
            // `const Widget`, in the position the file wrote it — which is what makes `const Widget*` and
            // `Widget* const` two different answers here rather than one.
            Type::Qualified { of } if matches!(**of, Type::Pointer { .. } | Type::Array { .. }) => {
                write!(out, "{of} const")
            }
            Type::Qualified { of } => write!(out, "const {of}"),
        }
    }
}

impl Type {
    /// **This type written the way a declarator writes it** — the shape `void (*)(int)` needs, and nothing else
    /// does.
    ///
    /// A pointer or an array in the return position of a function type is the one place this layer has to write
    /// parentheses: `void*(int)` reads as a function *returning* a pointer, which is a different type. The
    /// parentheses go around the **declarator** rather than around the type — `void (*)(int)`, not `(void*)(int)` —
    /// because that is where a name would stand, and a type read out of a declaration is that declaration with the
    /// name taken out. `void (*)(int)` is `void (*F)(int)` minus the `F`.
    ///
    /// # Why the base goes on the left and the operators on the right
    ///
    /// Because that is the order a declaration writes them, and the shape holds them the other way round:
    /// `Pointer { to: Builtin("void") }` has the `void` innermost. So the operators are collected on the way down —
    /// `*` for a pointer, `[4]` for an array — and the base is written when the bottom is reached, in front of
    /// them. A writer that emitted each level as it met it produced `(*void)(int)`.
    fn as_a_declarator(&self) -> String {
        let mut suffix = String::new();
        let mut qualified = false;
        let mut current = self;

        loop {
            match current {
                Type::Pointer { to } => {
                    suffix.push('*');
                    current = to;
                }
                Type::Array { of, extent } => {
                    suffix.push('[');
                    if let Some(size) = extent {
                        suffix.push_str(&size.to_string());
                    }
                    suffix.push(']');
                    current = of;
                }
                // `void* const`: the qualifier is written after what it qualifies, which is why it is part of the
                // operator run rather than of the base.
                Type::Qualified { of }
                    if matches!(**of, Type::Pointer { .. } | Type::Array { .. }) =>
                {
                    qualified = true;
                    current = of;
                }
                _ => break,
            }
        }

        // **The parentheses go around the operators, not around the base**: `void (*)(int)` is `void (*F)(int)` with
        // the `F` left out, and `(void*)(int)` — what wrapping the whole thing gives — puts the `void` inside them.
        format!(
            "{current} ({suffix}{})",
            if qualified { " const" } else { "" }
        )
    }
}

/// What a set of template parameter names stands for, by name — the map [Type::substituted] reads.
///
/// A newtype over slices rather than a `HashMap`, because of how it is built and used: a class template's
/// parameters are a short list in declaration order, the caller has just read them, and linear search over four
/// names is faster than hashing them. It also makes the *pairing* explicit — the names and the arguments are two
/// lists that must line up — which is the thing a bug here would get wrong.
#[derive(Debug, Clone, Copy)]
pub struct TypeSubstitutions<'a> {
    names: &'a [String],
    arguments: &'a [Type],
    /// **What a parameter with no argument stands for** — the declaration's own default, already parsed and already
    /// substituted with the arguments the use wrote, so `template <class T, class U = T>` and `twice<float>` give
    /// `U` the meaning `float`. Empty when the caller has no defaults, which is every caller that has no declaration
    /// in hand.
    defaults: &'a [Type],
}

impl<'a> TypeSubstitutions<'a> {
    pub fn new(names: &'a [String], arguments: &'a [Type]) -> TypeSubstitutions<'a> {
        TypeSubstitutions {
            names,
            arguments,
            defaults: &[],
        }
    }

    /// **[`TypeSubstitutions::new`] that falls back to the parameter's own default** for a use that wrote fewer
    /// arguments than the declaration has parameters — `std::vector<int>` is `vector<_Ty, _Alloc>` with `_Alloc`
    /// defaulted, and `twice<float>` writes one argument for two.
    pub fn with_defaults(
        names: &'a [String],
        arguments: &'a [Type],
        defaults: &'a [Type],
    ) -> TypeSubstitutions<'a> {
        TypeSubstitutions {
            names,
            arguments,
            defaults,
        }
    }

    /// What `name` stands for, if this map says.
    ///
    /// A parameter with **no** argument is mapped to nothing rather than to itself: `std::vector` written without
    /// arguments is not a `std::vector<T>` whose `T` happens to be called `T`, and answering the parameter name
    /// would be answering with a placeholder as if it were a type. A parameter with a **default** stands for the
    /// default, unless the default is the parameter itself (a spelling this layer cannot expand).
    pub fn get(&self, name: &str) -> Option<&Type> {
        let at = self
            .names
            .iter()
            .position(|parameter| !parameter.is_empty() && parameter == name)?;
        if let Some(argument) = self.arguments.get(at) {
            return Some(argument);
        }
        let default = self.defaults.get(at)?;
        let stands_for_itself = default
            .class_name()
            .is_some_and(|class| class == name && default.arguments().is_empty());
        (!stands_for_itself).then_some(default)
    }
}

/// **A substitution the caller owns** — the same pairing as [`TypeSubstitutions`], with the lists held rather than
/// borrowed.
///
/// The two exist because they are built in two different places, and the difference is lifetime rather than taste:
/// a caller that has the names and the arguments **in hand** (a query holding an argument list it just read) passes
/// slices and borrows nothing; a caller that had to *ask* for the names (a member lookup, whose parameter list is
/// the declaring file's) has a `Vec` of its own and needs a map that outlives the call. Making one type do both
/// would mean every caller allocating, and the ordinary case here is a class that is not a template at all.
#[derive(Debug, Clone, Default)]
pub struct TypeBindings {
    names: Vec<String>,
    arguments: Vec<Type>,
}

impl TypeBindings {
    /// The pairing of a class template's parameters with the arguments a use wrote.
    ///
    /// The pairing is **positional**, and a mismatch in length is not an error to report: `std::vector` written
    /// without arguments has no argument for `_Ty`, and a parameter with nothing to stand for stays itself. See
    /// [`TypeBindings::is_empty`] for the ordinary case.
    pub fn new(names: Vec<String>, arguments: Vec<Type>) -> TypeBindings {
        TypeBindings { names, arguments }
    }

    /// Nothing to substitute — an ordinary class, or a class template written without arguments.
    ///
    /// Asked before anything else by a caller about to walk a member's type, because it is the common case by a wide
    /// margin and because a substitution that changes nothing is work whose result a reader would have to compare to
    /// be sure of.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty() || self.arguments.is_empty()
    }

    /// **The arguments the use wrote**, in the order the class declares its parameters.
    ///
    /// A caller that has only a *spelling* of the class (`std::map`, from a member's `scope`) can rebuild it with them —
    /// `std::map<std::string, LuaTokenKind, …>` — which is what a qualifier inside the member's own return type needs in
    /// order to resolve: `_Tree<_Tmap_traits<_Kty, _Ty, …>>` names parameters that only the arguments can stand for.
    /// See `what_a_call_has`, which is the first caller.
    pub fn arguments(&self) -> &[Type] {
        &self.arguments
    }

    pub fn as_substitutions(&self) -> TypeSubstitutions<'_> {
        TypeSubstitutions::new(&self.names, &self.arguments)
    }

    /// `written` with every parameter in this map replaced — the whole reason the map exists.
    ///
    /// Takes and returns a `Type` rather than a spelling because the spellings are exactly what cannot be
    /// substituted textually: `_Ty&` and `_Ty*` and `std::vector<_Ty>` all contain the same four characters, and
    /// replacing them by text would also rewrite a class actually called `_Ty&` — or, in a real header, `_Ty` inside
    /// a longer identifier.
    pub fn applied_to(&self, written: &Type) -> Type {
        if self.is_empty() {
            return written.clone();
        }

        written.substituted(&self.as_substitutions())
    }
}

/// **Read a type out of the spelling a `DeclFact` records.**
///
/// # Why a spelling parser exists at all, when the syntax is the better source
///
/// Because two layers of this crate do not have the syntax, and cannot get it cheaply:
///
/// ```text
/// a declaration the index holds      `DeclFact::type_of` is a `String` on disk, and a query that needs the class
///                                     it names has no tree to walk — the declaring file is not even open
/// a type written as a template arg    `std::map<std::string, int>` arrives as text inside a name node
/// ```
///
/// So this is the **inverse of [`Type::Display`]**, and it is deliberately a *reading* rather than a validator:
/// it takes the spelling, peels the operators off the outside in, and answers with the shape. What it cannot
/// recognise becomes a [`Type::Named`] holding the spelling — which is what makes it usable on the shapes this
/// layer does not model, and is why [`Type::class_name`] is the question a caller should ask rather than
/// inspecting the variants.
///
/// # The one thing it must get right
///
/// [`Type::class_name`]. Everything else here is recoverable — a caller that gets `Type::Named("int")` instead of
/// `Type::Builtin` loses nothing it was going to ask about — while a **class name** that is wrong sends a member
/// query to a class that does not exist. So the peeling is written around that: the trailing declarator operators
/// come off first (they are written last), then the array extents, then the name and its arguments.
///
/// # What it does not do
///
/// Qualifiers (`const`, `volatile`) are **dropped**, from the front and from the back: they change what may be
/// assigned to a type and not which class it is, and this layer models no assignment. A spelling whose parts this
/// parser splits differently from how it was written still answers the same class — see the tests, which pin the
/// round trip for the shapes a fact actually records.
pub fn parse_type_spelling(written: &str) -> Type {
    let trimmed = written.trim();

    // **A compiler builtin is not a class with an odd name.**
    //
    // `enable_if_t<__is_constructible(_Ty, _Args...), _Ty>` reaches this reader as a spelling, and before this the
    // head of it — `__is_constructible` — was read as a class name, so the first argument of `enable_if_t` was a
    // class rather than a `bool` and everything after it followed from that. A trait is a `bool` whether or not its
    // value was decided; a type-producing builtin answers with a type; and the ones this layer recognises but will
    // not answer for keep the spelling they had, which is what they did before. See [`crate::sema::builtins`].
    //
    // Only the compiler's own names are consulted: in a type position `std::is_same<_Ty, _Uty>` is the **class** the
    // library defines, and reading it as a `bool` would be a wrong answer shaped exactly like a right one.
    if let Some((builtin, arguments)) = crate::sema::builtins::split_a_builtin_spelling(trimmed) {
        match crate::sema::builtins::answer(builtin, &arguments) {
            crate::sema::builtins::Answer::Boolean(_)
            | crate::sema::builtins::Answer::UndecidedBoolean => {
                return Type::builtin("bool");
            }
            crate::sema::builtins::Answer::Type(type_of) => return type_of,
            crate::sema::builtins::Answer::Unmodelled => {}
        }
    }
    if trimmed.is_empty() {
        return Type::builtin("void");
    }

    // **The qualifiers written last come off first.** `Widget* const` is a pointer with a `const` *after* the
    // operator, so peeling the `*` before the `const` would look for one at a position where it is not — the
    // spelling would end in `const` and the operator would end up inside the name. A qualifier on either side of a
    // type changes what may be assigned to it and not which class it names, and this layer models no assignment.
    let trimmed = strip_trailing_qualifiers(trimmed);

    // **Array extents, from the right.** `int[4]` is an array of four; `int[2][3]` is an array of two arrays of
    // three, and the *last* bracket pair belongs to the innermost array — so peeling from the right builds the
    // nesting in the order the type was written.
    if trimmed.ends_with(']')
        && let Some((element, extent)) = split_trailing_extent(trimmed)
    {
        return Type::Array {
            of: Arc::new(parse_type_spelling(element)),
            extent,
        };
    }

    // **The declarator operators, outside in.** A trailing `*` is a pointer *to* whatever is on its left; `&&`
    // before `&` before `*`, because that is the order they can be written in and the longer token must win.
    //
    // Each strip is tested by **matching on the stripped value**, not by `strip_suffix(..).map(..) && !empty`: that
    // form parses as `strip_suffix('*').map(str::trim_end && !rest.is_empty())` — the `&&` binds into the closure —
    // so the strip is never taken and the operator ends up inside the *name* (`Widget*` read as a class called
    // `Widget*`, which is a member query against a class that does not exist). Written this way there is nothing
    // for the precedence to get wrong.
    for (operator, rvalue) in [("&&", true), ("&", false)] {
        if let Some(rest) = trimmed.strip_suffix(operator).map(str::trim_end)
            && !rest.is_empty()
        {
            return Type::Reference {
                to: Arc::new(parse_type_spelling(rest)),
                rvalue,
            };
        }
    }

    if let Some(rest) = trimmed.strip_suffix('*').map(str::trim_end)
        && !rest.is_empty()
    {
        return Type::Pointer {
            to: Arc::new(parse_type_spelling(rest)),
        };
    }

    // **A function type**: `int(int, double)`. Recognised by a parameter list at the very end whose opening
    // parenthesis belongs to the base rather than to a declarator — which is the whole of the difference between
    // `int f(int)` and `int(int)`: the first has a name between the two, and a spelling a fact records has none.
    if trimmed.ends_with(')')
        && let Some((returns, parameters)) = split_trailing_parameters(trimmed)
        && !returns.is_empty()
        && !returns.contains('(')
    {
        return Type::Function {
            returns: Arc::new(parse_type_spelling(returns)),
            parameters: parameters
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(parse_type_spelling)
                .collect(),
        };
    }

    // **The base, and the qualifier written on it.** `const` is kept as a wrapper around whatever it qualifies
    // rather than dropped, because it is part of the type a reader is shown and part of what `auto` deduces:
    // `const auto& r = x` is a `const int&`, and a layer that answered `int&` would have lost the one word the
    // declaration was written to add.
    let (qualifier, base) = split_leading_qualifiers(trimmed);
    if base.is_empty() {
        return Type::builtin("void");
    }

    let qualified = match base.find('<') {
        Some(at) if base.ends_with('>') => {
            let name = base[..at].trim_end().to_string();
            let arguments = base[at + 1..base.len() - 1]
                .split_arguments()
                .into_iter()
                .map(|argument| parse_type_spelling(&argument))
                .collect();
            Type::Named { name, arguments }
        }
        // **A builtin is not a class**, and the difference is not cosmetic: `int` as a `Named` type answers
        // `Type::class_name` with `int`, so a member access on an `int` would send the index looking for a class
        // called `int` and report "not declared here" — which a reader takes as "this class is missing" rather
        // than "this is not a class". The words are the language's own list, and a builtin spelled in several
        // words (`unsigned long long`) is on it as one spelling.
        _ if is_a_builtin(base) => Type::builtin(base),
        // Anything else is a name this layer cannot place — a dependent name, a macro that expands to a type — and
        // carrying it as a name is what makes it usable by a caller that resolves names.
        _ => Type::Named {
            name: base.to_string(),
            arguments: Vec::new(),
        },
    };

    match qualifier {
        Some(_qualifier) => Type::Qualified {
            of: Arc::new(qualified),
        },
        None => qualified,
    }
}

/// `int[4]` → `("int", Some(4))`, and `int[]` → `("int", None)` — the **last** extent, from the right.
///
/// From the right because the last bracket pair is the one that applies to the whole type on its left:
/// `int[2][3]` is an array of two of `int[3]`, so the outer peel takes `[3]` and leaves `int[2]` to the recursion.
fn split_trailing_extent(written: &str) -> Option<(&str, Option<usize>)> {
    let mut depth = 0isize;

    for (index, character) in written.char_indices().rev() {
        match character {
            ']' => depth += 1,
            '[' => {
                depth -= 1;
                if depth == 0 {
                    let element = written[..index].trim_end();
                    if element.is_empty() {
                        return None;
                    }
                    let inside = written[index + 1..written.len() - 1].trim();
                    return Some((element, inside.parse().ok()));
                }
            }
            _ => {}
        }
    }

    None
}

/// `int(int, double)` → `("int", "int, double")`, when the closing parenthesis is the last thing written.
///
/// The **first** `(` at depth zero opens it, because a return type can hold parentheses of its own
/// (`int (*)(int)` is a pointer, and has been peeled above by the time this runs).
fn split_trailing_parameters(written: &str) -> Option<(&str, &str)> {
    let mut depth = 0isize;

    for (index, character) in written.char_indices().rev() {
        match character {
            ')' => depth += 1,
            '(' => {
                depth -= 1;
                if depth == 0 {
                    let returns = written[..index].trim_end();
                    let parameters = &written[index + 1..written.len() - 1];
                    return Some((returns, parameters));
                }
            }
            _ => {}
        }
    }

    None
}

/// Is this spelling one of the language's own type words?
///
/// The set is every word a builtin can be spelled with **and the combinations**: `unsigned long long` and
/// `long double` are builtins whose spelling is several words, so the test is on the whole spelling. Nothing here is
/// a keyword this layer handles earlier for another reason (`auto`, `decltype`) — those are read as the placeholders
/// they are before this function is reached.
fn is_a_builtin(spelling: &str) -> bool {
    matches!(
        spelling,
        "void"
            | "bool"
            | "char"
            | "char8_t"
            | "char16_t"
            | "char32_t"
            | "wchar_t"
            | "short"
            | "int"
            | "long"
            | "float"
            | "double"
            | "signed"
            | "unsigned"
            | "short int"
            | "long int"
            | "long long"
            | "long long int"
            | "long double"
            | "unsigned short"
            | "unsigned int"
            | "unsigned long"
            | "unsigned long long"
            | "unsigned short int"
            | "unsigned long int"
            | "unsigned long long int"
            | "signed short"
            | "signed int"
            | "signed long"
            | "signed long long"
            | "signed short int"
            | "signed long int"
            | "signed long long int"
            | "signed char"
            | "unsigned char"
    )
}

/// The spelling with its **trailing** qualifiers removed: `Widget* const` → `Widget*`.
///
/// Trailing only, because the leading ones are handled where the base is read ([`without_qualifiers`]) and doing
/// both in one place would have to know whether the qualifier it is looking at belongs to the type or to the
/// pointer — which is the question the *position* answers and nothing else does.
fn strip_trailing_qualifiers(written: &str) -> &str {
    let mut end = written.trim_end();

    loop {
        let Some(word) = end.split_whitespace().next_back() else {
            return end;
        };
        if !matches!(word, "const" | "volatile") || word.len() == end.len() {
            return end;
        }
        end = end[..end.len() - word.len()].trim_end();
    }
}

/// A spelling split into the **qualifier it leads with** and the type underneath — `const Widget` →
/// `(Some("const"), "Widget")`.
///
/// The elaborated keywords (`struct`, `class`, `enum`, `typename`) are stepped over and **not** reported: they are
/// how a type is disambiguated rather than anything about it, and `struct Widget` and `Widget` are one type. A
/// qualifier is reported because it is not.
fn split_leading_qualifiers(written: &str) -> (Option<&str>, &str) {
    let mut rest = written.trim();
    let mut qualifier: Option<&str> = None;

    while let Some(word) = rest.split_whitespace().next() {
        if word.len() == rest.len() {
            break;
        }
        match word {
            "const" if qualifier.is_none() => qualifier = Some("const"),
            "volatile" if qualifier.is_none() => qualifier = Some("volatile"),
            "struct" | "class" | "enum" | "typename" => {}
            _ => break,
        }
        rest = rest[word.len()..].trim_start();
    }

    (qualifier, rest.trim_end())
}

/// Split a template argument list on the commas that are **not** inside a nested list.
///
/// A trait rather than a function so it can be written where it reads best — `.split_arguments()` on the string —
/// and because it is the one splitting rule in this module: `std::map<std::string, int>` is two arguments and
/// `std::vector<std::pair<int, int>>` is one.
trait ArgumentSplit {
    fn split_arguments(&self) -> Vec<String>;
}

impl ArgumentSplit for str {
    fn split_arguments(&self) -> Vec<String> {
        let mut arguments = Vec::new();
        let mut depth = 0isize;
        let mut start = 0usize;

        for (index, character) in self.char_indices() {
            match character {
                '<' => depth += 1,
                '>' => depth -= 1,
                ',' if depth == 0 => {
                    arguments.push(self[start..index].trim().to_string());
                    start = index + 1;
                }
                _ => {}
            }
        }

        let last = self[start..].trim();
        if !last.is_empty() {
            arguments.push(last.to_string());
        }

        arguments
    }
}

/// **The names a template parameter list introduces**, in declaration order — `["_Ty", "_Alloc"]` for
/// `<class _Ty, class _Alloc = allocator<_Ty>>`.
///
/// # Why this is the half that was missing
///
/// [`Type::substituted`] pairs a parameter list with the arguments a use wrote, and it has existed since the model
/// did. What did not exist was anybody to hand it the names: a class template's parameters are written **once**, at
/// the declaration, and every use of the template says only what it passes. So a member's own recorded type keeps
/// saying `_Ty` — `std::vector<int>::reference` is declared `_Ty&` and *is* `int&` — and the layer answered about a
/// type called `_Ty`, which no class would be found for.
///
/// # What a parameter looks like, and the one cut that matters
///
/// ```text
/// <class _Ty>                     → `_Ty`
/// <class _Alloc = allocator<_Ty>> → `_Alloc`      the default is a *value* for the parameter, not its name
/// <int N>                         → `N`           a non-type parameter is a name too, and substituting it is the
///                                                 same textual operation — `std::array<int, N>` with `N = 4`
/// <typename... Args>              → `Args`        a pack, whose arguments are several instead of one
/// ```
///
/// So the name is everything before the first `=` (a default argument) — **not** the first word, because a
/// constrained parameter (`template <SomeConcept T>`) has words in front of the name that are not it. Taking the
/// **last** identifier before the `=` gets every shape above right and is what this does.
pub fn template_parameter_names(list: &CppSyntaxNode) -> Vec<String> {
    template_parameters_in(list)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// **Each template parameter's name and the constraint written in front of it** — `("T", "std::integral")` for
/// `template <std::integral T>`, and `("U", "")` for an unconstrained one.
///
/// One walk for both answers, because they are one question with two consumers: [`template_parameter_names`] wants
/// the names alone, and the concept check needs the constraint beside the name — "this argument does not satisfy what
/// the declaration asked of its parameter" cannot be answered by either half on its own.
///
/// The constraint is what stands **before** the name, with `class`/`typename`/`template` and any `...` taken off, so
/// `class... Ts` is unconstrained and `std::integral T` is `std::integral`.
pub fn template_parameters_in(list: &CppSyntaxNode) -> Vec<(String, String)> {
    template_parameters_with_defaults(list)
        .into_iter()
        .map(|(name, constraint, _)| (name, constraint))
        .collect()
}

/// **Each template parameter's default argument**, in the same order, empty string for a parameter that has none.
///
/// Separate from the names so that the fact can carry a parallel list — see [`crate::DeclFact::parameter_defaults`].
pub fn template_parameter_defaults(list: &CppSyntaxNode) -> Vec<String> {
    template_parameters_with_defaults(list)
        .into_iter()
        .map(|(_, _, default)| default.unwrap_or_default())
        .collect()
}

/// **Each template parameter's name, constraint and default** — `("_Alloc", "", Some("allocator<_Ty>"))`.
///
/// # Why the default is kept
///
/// A use writes fewer arguments than the declaration has parameters all the time: `std::vector<int>` is
/// `vector<_Ty, allocator<int>>`, and `std::map<Key, Value>` is four parameters. Without the default, a member whose
/// type names the second parameter answers a spelling that has no meaning at the use site — measured on
/// `auto lineStartOffset = _lineOffsetVec[line]`: the chain reached `_Alloc_traits::reference` →
/// `allocator_traits<_Alloc>::reference` and stopped at `_Alloc::value_type&`, because `_Alloc` had no argument and
/// its default (`std::allocator<std::size_t>`) was not in the fact. `twice<float>` answering `U` is the same gap.
///
/// The default is written in the **same parameter names** as the declaration (`_Alloc = allocator<_Ty>`), so a
/// substitution that uses it has to substitute *into* it as well — see
/// [`substituted_spelling_with_defaults`].
pub fn template_parameters_with_defaults(
    list: &CppSyntaxNode,
) -> Vec<(String, String, Option<String>)> {
    let mut found = Vec::new();

    for parameter in list.children() {
        if CppSyntaxKind::from(parameter.kind()) != CppSyntaxKind::TemplateParameter {
            continue;
        }

        let written = parameter.text().to_string();
        let before_a_default = written.split('=').next().unwrap_or(&written);

        // **The name is the last *identifier* before the default**, read from the **tokens** rather than from the
        // text. Splitting the text took "the last word", and for an **unnamed** parameter that word is the keyword
        // that introduces it: `template <typename, typename> class vector;` recorded `["typename", "typename"]` — two
        // "names" no body can mention, and a list that then *won* the choice of which declaration's parameters a
        // substitution used (see `is_a_parameter_name`, which had to be written to reject exactly that). A keyword is
        // not an identifier to the lexer, so the token stream answers what the text could not.
        let mut name: Option<String> = None;
        for token in parameter
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
        {
            let kind = cpp_parser::CppTokenKind::from(token.kind());
            // The default argument starts here, and everything after it is a *value* rather than the parameter's
            // name — `class _Alloc = allocator<_Ty>` must not answer `_Ty`.
            if kind == cpp_parser::CppTokenKind::Assign {
                break;
            }
            if kind == cpp_parser::CppTokenKind::Identifier {
                name = Some(token.text().to_string());
            }
        }

        let default = default_of(&written);

        // **An unnamed parameter keeps its slot.** Arguments are paired with names **by position**, so dropping the
        // entry would move every later name one argument to the left: `template <typename, class T>` would
        // substitute the *first* argument for `T`, and a `pair<int, long>` would answer `pair<int, int>`. The empty
        // string is a name no spelling can equal, which is exactly what a parameter with no name is.
        let Some(name) = name else {
            found.push((String::new(), String::new(), default));
            continue;
        };

        // Everything before the name, minus the spellings that are not a constraint.
        let before = before_a_default
            .rfind(&name)
            .map(|at| before_a_default[..at].trim())
            .unwrap_or("");
        let mut constraint = before.to_string();
        loop {
            let mut trimmed = constraint.trim().trim_end_matches("...").trim().to_string();
            for keyword in ["class", "typename", "template", "struct"] {
                if let Some(rest) = trimmed.strip_prefix(keyword) {
                    trimmed = rest.trim().to_string();
                }
            }
            trimmed = trimmed.trim().to_string();
            if trimmed == constraint || trimmed.is_empty() {
                constraint = trimmed;
                break;
            }
            constraint = trimmed;
        }

        // A pack whose name came first (`Ts...`) leaves nothing in front, and so does an unconstrained parameter.
        found.push((name, constraint, default));
    }

    found
}

/// **The default argument written for a template parameter**, if one is — `allocator<_Ty>` for
/// `class _Alloc = allocator<_Ty>`.
///
/// The first `=` at **angle depth zero**: a non-type default may be an expression (`= 4`, `= sizeof(T)`), and a type
/// default may contain its own `>` (`= std::greater<>`), which is what the depth counter is for. A constraint cannot
/// contain a `=` at that depth, so the first one is the default's boundary.
fn default_of(parameter: &str) -> Option<String> {
    let bytes = parameter.as_bytes();
    let mut depth = 0usize;
    for (at, byte) in bytes.iter().enumerate() {
        match byte {
            b'<' => depth += 1,
            b'>' => depth = depth.saturating_sub(1),
            b'=' if depth == 0 => {
                let default = parameter[at + 1..].trim();
                return (!default.is_empty()).then(|| default.to_string());
            }
            _ => {}
        }
    }
    None
}

/// **The declarator that declares the name at `range`** — the innermost one whose text holds it.
///
/// Innermost, because a declaration holds the declarators of what it declares *and* of the parameters it takes:
/// `void f(Widget* p)` has a `Declarator` for `f` whose text contains `p`, and the one that declares `p` is the
/// parameter's own. The shortest match is the innermost, and it is also the only one whose specifiers are the
/// *parameter's* specifiers.
///
/// # Why this descends instead of walking the tree
///
/// It used to be `root.descendants()`, filtered by "a declaration kind that contains this name and holds a
/// declarator somewhere in its subtree" — **two nested walks of the whole tree per question**, asked once per
/// declaration whose type the model reads. Every node whose range contains the name is on **one chain** — the
/// ancestors of the token the name is written in — so the innermost is the deepest, and descending into the child
/// that contains the name finds it in O(depth × children) visits. The subtree scan the old filter did is only needed
/// for a node that does not hold its declarator directly, which is the rare shape rather than the common one. The
/// answers are the same, including the one that is `None`: a node with a declarator *somewhere* but none it can name
/// directly is the node the old code chose, and it answered `None` rather than looking further outward.
///
/// **What it is worth, measured.** On the four-file audit of `EmmyLuaCodeStyle` this is **1 958 ms → 1 895 ms** —
/// small, and it was *not* what made the inference slow: the 5 923 ms a profile attributed to this area was the
/// engine re-asking the same declarations because its recursion had no memo (see
/// [`crate::index::project::type_of_expression_memoed`]). The descent is kept as the smaller walk with the same
/// answers rather than as a fix for a measured defect.
pub fn declarator_declaring(
    root: &cpp_parser::CppSyntaxNode,
    name: cpp_parser::SourceRange,
) -> Option<cpp_parser::CppSyntaxNode> {
    // **The outermost declarator that declares this name**, which is the one under the declaration or under its
    // `InitDeclarator` — not a search for the shortest one containing the name, and the difference is every
    // function pointer and every array: `typedef void (*Callback)(int);` nests `(* Callback)(int)`, `(* Callback)`
    // and `* Callback`, and the *innermost* of those is `* Callback` — whose reader answers `void*` with the
    // parameter list lost. The whole declarator is what holds the whole type; the name is found inside it by
    // [`type_of_declaration`], which descends to the node whose direct child is the name.
    //
    // This is the same shape the reader's own tests use, and it is not a coincidence: the declaration is where a
    // type is written down.

    // **One descent, and the chain it leaves behind.** Every node whose range holds the name is an ancestor of the
    // token the name is written in, so the innermost one is the deepest node the descent reaches.
    let mut chain: Vec<cpp_parser::CppSyntaxNode> = Vec::new();
    let mut current = root.clone();
    loop {
        chain.push(current.clone());
        let next = current.children().find(|child| {
            let own = child.text_range();
            usize::from(own.start()) <= name.start_offset
                && usize::from(own.end()) >= name.end_offset()
        });
        match next {
            Some(child) => current = child,
            None => break,
        }
    }

    // **Deepest first**, which is the old `min_by_key(span)`: the nodes that can hold the name are all on the chain,
    // and the one with the shortest span is the one furthest down it.
    for node in chain.iter().rev() {
        // **Three more kinds declare a name than the one called `Declaration`.** `typedef` and `using` are
        // their own nodes, and a **parameter** is a `Parameter` — so a search for `Declaration` alone found
        // none of them: a function-pointer typedef came back with no type at all, and `void f(Widget* p)`'s
        // `p` was read from the *function's* declaration, which is why a parameter answered `UnknownType`
        // while the same spelling declared at the top of a body worked.
        if !matches!(
            CppSyntaxKind::from(node.kind()),
            CppSyntaxKind::Declaration
                | CppSyntaxKind::TypedefDecl
                | CppSyntaxKind::UsingDecl
                | CppSyntaxKind::Parameter
        ) {
            continue;
        }
        let own = node.text_range();
        if usize::from(own.start()) > name.start_offset
            || usize::from(own.end()) < name.end_offset()
        {
            continue;
        }

        let init = node
            .children()
            .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::InitDeclarator);
        let declarator = init
            .as_ref()
            .and_then(|init| {
                init.children()
                    .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::Declarator)
            })
            .or_else(|| {
                node.children()
                    .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::Declarator)
            });
        if declarator.is_some() {
            return declarator;
        }

        // **A node that holds a declarator somewhere but none it can name directly is the node this function used
        // to choose — and it answered `None`.** Keeping that answer rather than looking further outward is what
        // makes this rewrite equivalent; the scan is here only for that shape, so the common case never pays it.
        if node
            .descendants()
            .any(|inner| CppSyntaxKind::from(inner.kind()) == CppSyntaxKind::Declarator)
        {
            return None;
        }
    }

    None
}
/// **Read a declared type from a declaration's syntax** — the specifier sequence plus the declarator around a name.
///
/// # Why both halves are needed
///
/// A C++ type is written in two places and neither is complete on its own:
///
/// ```text
/// const Widget* const p;      the specifiers say `const Widget`   the declarator says `* const  p`
/// int (*cb)(int);             the specifiers say `int`            the declarator says `(* cb)(int)`
/// ```
///
/// The specifier sequence holds the *base* type and its leading qualifiers; the declarator holds everything that
/// wraps it — pointers, references, arrays, a parameter list — nested inside one another in a way that has to be
/// walked from the name **outward**. So this takes the two nodes and the name's own span (which is how the
/// parameters of a function pointer are told from the parameters of the function being declared, the same
/// distinction [`crate::scopes::parameters_of`] makes).
///
/// # What it does with what it cannot read
///
/// A node it has no rule for contributes its **own spelling** as a [`Type::Named`], which is the honest reading of
/// "the file wrote this here and this layer does not know what shape it is" — and it is what makes the answer
/// usable in the cases this layer does not model (`decltype(x)`, `auto`, an attribute, a macro that expands to a
/// type). A caller that needs to know the difference asks [`Type::class_name`], which answers only for a name.
pub fn type_of_declaration(
    specifiers: &CppSyntaxNode,
    declarator: Option<&CppSyntaxNode>,
    name: cpp_parser::SourceRange,
) -> Type {
    type_of_declaration_named(specifiers, declarator, name, None)
}

/// **The same question, told which name is being declared.**
///
/// A definition written out of line puts the declarator's **qualified name** in the specifier sequence:
///
/// ```text
/// std::map<std::string, LuaTokenKind, std::less<>> LuaLexer::LuaReserved = { … };
///                                                   ^^^^^^^^^ the declarator's name
/// ```
///
/// so "the last name in the specifiers is the type" reads the *name* as the type — measured on that line, the fact
/// recorded `type_of: Some("LuaLexer::LuaReserved")`, and every member lookup on it answered "no member `find`". The
/// declared name is what tells the two apart, and a caller that has it (the fact builder does, from its binding)
/// hands it over.
pub fn type_of_declaration_named(
    specifiers: &CppSyntaxNode,
    declarator: Option<&CppSyntaxNode>,
    name: cpp_parser::SourceRange,
    declared_name: Option<&str>,
) -> Type {
    let base = read_specifiers_named(specifiers, declared_name);

    // **A base with no spelling has nothing for a declarator to be applied to.** `priority[]` of the unnamed class
    // type above is not `[]`, it is an array of a type this layer cannot spell — so wrapping it in an `Array`
    // would write `[]`, which is a different answer rather than a shorter one. The empty spelling goes back as it
    // came, and the fact records no type at all.
    if base.to_string().is_empty() {
        return base;
    }

    let Some(declarator) = declarator else {
        return base;
    };

    // **The name's own range, found in the declarator.** The range a caller passes is the one the *model* has — a
    // binding's `name_range`, which for a qualified declarator (`ns::Inner a`) or a pointer (`Widget* p`) may not
    // line up with a syntax node at all — so the declarator's own reading is the one the walk below uses.
    let name = declared_name_range(declarator).unwrap_or(name);

    // The node the wrapping starts from: the declarator whose direct child is the name. Everything outside it is
    // written *around* the name and is therefore part of the type — see [`wrap`].
    let inner = declarator_holding_the_name(declarator, name);

    wrap(base, declarator, &inner, name)
}

/// **The range of the identifier a declarator declares** — its first `NameExpr`, which is the name.
///
/// First and not last, and the order is the opposite of the one `parameters_of` uses for a good reason: a
/// declarator's own name is written **before** everything that belongs to what it declares — a parameter list
/// (`f(int x)`), an array extent (`arr[N]`), an initializer (`x = f()`). So the first name in a declarator is the
/// declared one, and every name after it belongs to something else.
fn declared_name_range(declarator: &CppSyntaxNode) -> Option<cpp_parser::SourceRange> {
    declarator
        .descendants()
        .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::NameExpr)
        .map(|child| cpp_parser::source_range(child.text_range()))
}

/// The specifier sequence's contribution: the base type, with its leading `const`/`volatile` kept.
///
/// **Attributes and declaration specifiers are dropped**, and that is the bug this function was written for: the
/// old spelling-based reader answered `const [[nodiscard]] constexpr size_type` for a return type, because it
/// stripped a list of known keywords from text rather than reading the syntax. Here the only nodes that contribute
/// are the ones that *are* the type's name — a builtin, a name, a `decltype`, an elaborated keyword — and
/// everything else (`const`, `volatile`, storage class, `constexpr`, attributes) is dropped because it is not part
/// of what the type is *called*.
///
/// # Why the builtin words are gathered rather than taken from the first node
///
/// `unsigned long long` parses as **three** `BuiltinType` nodes — one per word — so reading the first gives
/// `unsigned`, which is a different type from the one the file wrote. They are joined in the order they appear,
/// which is the order the words are written in, and that is the whole rule: a builtin type's name *is* its words.
///
/// The qualifiers are dropped rather than modelled, and the reason is the same one [`Type`] gives for keeping
/// spellings: this layer does not resolve aliases, so `const Int` and `Int` are already two names for one type, and
/// a `const` on the base would be a third axis no query here asks about.
/// Is this builtin specifier's text a **`typeof`/`decltype` spelling** — C23's, GNU's, or C++'s own?
fn is_a_typeof_spelling(word: &str) -> bool {
    let name = word.split('(').next().unwrap_or(word).trim();
    matches!(
        name,
        "typeof" | "typeof_unqual" | "__typeof__" | "__typeof" | "__decltype" | "decltype"
    )
}

/// The type a `typeof(…)`/`decltype(…)` specifier denotes, when its payload is a **type-id**.
///
/// `None` when the payload is an expression — `typeof(value)` — because that answer needs the semantic model and this
/// reader is given a tree and nothing else.
fn the_type_a_typeof_names(node: &CppSyntaxNode) -> Option<Type> {
    // The payload is the node inside the parentheses; a type-id is written as a TypeId holding a
    // DeclSpecifierSeq, which is the node this reader understands.
    let payload = node.descendants().find(|child| {
        matches!(
            CppSyntaxKind::from(child.kind()),
            CppSyntaxKind::TypeId | CppSyntaxKind::DeclSpecifierSeq
        )
    })?;
    let specifiers = if CppSyntaxKind::from(payload.kind()) == CppSyntaxKind::DeclSpecifierSeq {
        payload
    } else {
        payload
            .children()
            .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::DeclSpecifierSeq)?
    };
    let read = read_specifiers_named(&specifiers, None);
    // An elaborated name is the type's *name* on its own: struct point reaches this reader as a specifier sequence
    // holding BuiltinType "struct" and NamedType "point", and the answer is point — which is what a member list is
    // keyed under. A payload that resolved to nothing is not an answer.
    (!read.to_string().trim().is_empty()).then_some(read)
}
pub(crate) fn read_specifiers(specifiers: &CppSyntaxNode) -> Type {
    read_specifiers_named(specifiers, None)
}

/// [`read_specifiers`], with the name the declaration declares — see [`type_of_declaration_named`].
pub(crate) fn read_specifiers_named(
    specifiers: &CppSyntaxNode,
    declared_name: Option<&str>,
) -> Type {
    let mut builtin: Vec<String> = Vec::new();
    // The **last** name in the sequence, and the last unmodelled node — see the two notes below.
    let mut named: Option<Type> = None;
    let mut spelling = None;
    // Was a class-like **body** written here without a name in front of it? See the answer below.
    let mut an_unnamed_body = false;
    // Was an **elaborated keyword** (`struct`, `class`, `union`, `enum`) written here? See the `BuiltinType` arm.
    let mut elaborated = false;

    for child in specifiers.children_with_tokens() {
        let Some(node) = child.into_node() else {
            continue;
        };

        match CppSyntaxKind::from(node.kind()) {
            // The words of a builtin, in order — see the note above about `unsigned long long`.
            CppSyntaxKind::BuiltinType => {
                let word = node.text().to_string().trim().to_string();
                if word.is_empty() {
                    continue;
                }
                // **An elaborated keyword is not a type, and this is where it was being taken for one.** The
                // grammar reads `struct exarg` as **two** nodes —
                //
                // ```text
                // DeclSpecifierSeq "struct exarg"
                //   BuiltinType "struct"
                //   NamedType "exarg"
                // ```
                //
                // — so `builtin` was non-empty and won the answer below, while `named` held the only spelling
                // anything looks up. Measured: `typedef struct exarg exarg_T;` recorded `type_of: Some("struct")`,
                // the alias's target (which a cross-file lookup reads out of that field) was `struct`, and
                // `exarg_T` — the receiver of thousands of member questions in neovim — answered a member list of
                // zero. `unsigned long long` is unaffected: those words *are* the type.
                if matches!(word.as_str(), "struct" | "class" | "union" | "enum") {
                    elaborated = true;
                } else if is_a_typeof_spelling(&word) {
                    // **`typeof(struct point)` *is* the type it names.** The payload of a `typeof`/`decltype` specifier
                    // is a type-id when the language allows one (C23's `typeof`, GNU's `__typeof__`), and the spelling
                    // is not: without this the fact recorded `typeof(struct point)`, which no scope and no member list
                    // has ever heard of. Measured on a nine-file C corpus: `origin.x` and `other_pointer->x` answered
                    // nothing, and `copy` recorded `typeof(value)` as its own type.
                    //
                    // An **expression** payload (`typeof(value)`) still needs the semantic model, which the fact builder
                    // does not have — it keeps its spelling here and the query path resolves it.
                    match the_type_a_typeof_names(&node) {
                        Some(denoted) => named = Some(denoted),
                        None => builtin.push(word),
                    }
                } else {
                    builtin.push(word);
                }
            }
            // A name: `Widget`, `std::vector<int>`, `T`, and the dependent form `typename T::value_type`. A plain
            // name is a `NamedType` and a template-id a `TemplateType` — see [`CppSyntaxKind::NamedType`].
            CppSyntaxKind::NamedType
            | CppSyntaxKind::TemplateType
            | CppSyntaxKind::TypenameType
            | CppSyntaxKind::QualifiedType => {
                // **The declarator's own qualified name is not a type.** An out-of-line definition writes it in the
                // specifier sequence — `std::map<std::string, LuaTokenKind> LuaLexer::LuaReserved = { … }` — and taking
                // "the last name" as the type recorded `LuaLexer::LuaReserved`. It is recognisable without guessing: the
                // spelling ends with `::` and the name being declared, and a *type* never does.
                let written = node.text().to_string();
                let written = written.trim();
                let is_the_declared_name = declared_name.is_some_and(|name| {
                    written
                        .strip_suffix(name)
                        .is_some_and(|prefix| prefix.ends_with("::"))
                });
                if !is_the_declared_name && let Some(found) = read_named(&node) {
                    named = Some(found);
                }
            }
            // **`auto` and `decltype` are types this layer does not model**, and each is a spelling a caller
            // resolves elsewhere: `auto` is the placeholder the inference layer substitutes into, and
            // `decltype(x)` is an expression this layer would have to type to know. Keeping the spelling is what
            // lets the caller recognise them.
            CppSyntaxKind::AutoType | CppSyntaxKind::DecltypeType => {
                let written = node.text().to_string().trim().to_string();
                if !written.is_empty() && named.is_none() {
                    named = Some(Type::named(written));
                }
            }
            // Not part of what the type is called. Qualifiers, attributes, storage class, `constexpr`, `inline`,
            // `friend` — a caller that needs to know a declaration was `static` or `friend` asks the declaration.
            CppSyntaxKind::ConstQual
            | CppSyntaxKind::VolatileQual
            | CppSyntaxKind::RestrictQual
            | CppSyntaxKind::Attribute
            | CppSyntaxKind::AttributeList
            | CppSyntaxKind::FriendDecl
            | CppSyntaxKind::StaticSpec
            | CppSyntaxKind::ExternSpec
            | CppSyntaxKind::ThreadLocalSpec
            | CppSyntaxKind::MutableSpec
            | CppSyntaxKind::RegisterSpec
            | CppSyntaxKind::InlineSpec
            | CppSyntaxKind::VirtualSpec
            | CppSyntaxKind::ExplicitSpec
            | CppSyntaxKind::ConstexprSpec
            | CppSyntaxKind::NoexceptSpec
            | CppSyntaxKind::AlignasSpec => {}
            // **An elaborated specifier whose class body is here**: `struct Widget { … }` is a definition, and the
            // type it declares is named by the `NameExpr` inside it — which the arms above do **not** return,
            // because the name is a *child* of this node rather than a sibling of it:
            //
            // ```text
            // DeclSpecifierSeq "struct Plain { int d; }"
            //   StructDef "struct Plain { int d; }"
            //     NameExpr "Plain"          <- the tag, one level down
            //     ClassBody "{ int d; }"
            // ```
            //
            // Reading it here is what makes `struct Plain { int d; } plain;` type `plain` as `Plain`; before, the
            // whole sequence was an *anonymous* body to this function, so the fact carried no type at all and
            // `plain.d` could not be answered. And for a `typedef`, the tag is what the members are filed under
            // (`typedef struct Tag { int d; } Alias;` files `d` under `Tag`), so the alias's target has to be the
            // tag rather than the definition's text.
            //
            // **Unless there is no name**, and then this is the only thing in the sequence that says anything at
            // all: `static const struct { unsigned char left; unsigned char right; } priority[] = …` is an
            // **unnamed** class type, and this layer cannot spell it. The answer is the empty spelling below and
            // *not* `void` — see the note there.
            CppSyntaxKind::ClassDef
            | CppSyntaxKind::StructDef
            | CppSyntaxKind::UnionDef
            | CppSyntaxKind::EnumDef
            | CppSyntaxKind::EnumClassDef => {
                let tag = node
                    .children()
                    .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::NameExpr)
                    .map(|child| child.text().to_string().trim().to_string())
                    .find(|written| !written.is_empty());
                match tag {
                    // Assigned rather than only filled when empty: a tag is a name of the declaration's own, and
                    // the "last name wins" rule below is exactly the one that should apply to it.
                    Some(tag) => named = Some(Type::named(tag)),
                    None => an_unnamed_body = true,
                }
            }
            // **Anything else is not a type's name**, and the rule is the converse of the one above: what a file
            // writes in a specifier sequence that this layer has no rule for is a *keyword* far more often than it
            // is a type. Measured, the fallback that used to stand here produced `friend constexpr
            // iter_difference_t` for a friend declaration — a spelling that is not a type and cannot resolve. So a
            // node with no rule is kept only as a **last resort**, for the file that writes something this layer
            // does not model and would otherwise answer `void` for.
            _ => {
                let written = node.text().to_string().trim().to_string();
                if !written.is_empty() && spelling.is_none() {
                    spelling = Some(Type::named(written));
                }
            }
        }
    }

    if !builtin.is_empty() {
        return Type::builtin(builtin.join(" "));
    }

    // **The last name wins**, and the shape the rule exists for is a macro standing where a specifier goes: the
    // grammar reads `_EXPORT_STD extern "C++" __PURE_APPDOMAIN_GLOBAL _CRTDATA2_IMPORT istream cin;` as one
    // specifier sequence holding three names, and the type is the **last** of them. No C++ type is spelled as two
    // unqualified names in a row — `unsigned long` is two *keywords*, and a keyword is not a `NameExpr` — so the
    // count is the whole rule.
    //
    // Measured, and it is what made `std::cin` unanswerable: MSVC's `<iostream>` declares `cin` twice, and a reader
    // that took the first name recorded `__PURE_APPDOMAIN_GLOBAL` where the cooked reading of the same line recorded
    // `istream`, so the two declarations disagreed about the type and neither answered.
    if let Some(named) = named {
        return named;
    }

    // **A class body nothing named is a type, and it has no spelling here.** `void` is not "no answer": it is a
    // *different type*, and recording it made `static const struct { … } priority[];` answer `void[]` — measured on
    // the `LuaLexer.cpp` fixture this was written for, and the hover then printed the wrong signature instead of
    // the file's own text. The empty spelling is what [`declared_type_of`] reads as "no type to name", which is
    // the honest answer for a spelling a compiler would print as `struct { … }` and this layer does not model.
    //
    // **An elaborated keyword with no name after it is the same answer**, and it is the leftover half of the
    // `BuiltinType` note above: `struct` where an unexpanded macro should have named the type. Returning `void`
    // would claim a type the file never wrote.
    if (an_unnamed_body || elaborated) && spelling.is_none() {
        return Type::builtin("");
    }

    spelling.unwrap_or_else(|| Type::builtin("void"))
}

/// **Where the argument list that ends this spelling starts**, if one does.
///
/// `std::vector<int>` → the `std::vector` cut; `A<B<int>>` → the `A` cut (the brackets nest, so the first `<` is the
/// one whose partner is the last character); `Base<T>::reference` → `None`, because the list is not at the end and
/// what follows it is part of the name; `Foo<int>::Bar<int>` → the `Bar` cut, which is the group that ends the
/// spelling.
///
/// A byte scan with a depth counter, which is enough: the text is a type spelling, and a `<` inside a *string* is not
/// a shape a type has.
fn trailing_template_id(written: &str) -> Option<usize> {
    let bytes = written.as_bytes();
    if bytes.last() != Some(&b'>') {
        return None;
    }

    // **The group that closes at the last byte**, not the first group written: the openings are stacked, so the one
    // popped when the depth falls to zero *and the bracket is the last character* is the answer.
    let mut depth = 0usize;
    let mut openings: Vec<usize> = Vec::new();
    for (at, byte) in bytes.iter().enumerate() {
        match byte {
            b'<' => {
                depth += 1;
                openings.push(at);
            }
            b'>' => {
                if depth == 0 {
                    // Unbalanced: not a template-id, and the name keeps its spelling.
                    return None;
                }
                depth -= 1;
                let opening = openings.pop()?;
                if depth == 0 && at + 1 == bytes.len() {
                    return Some(opening);
                }
            }
            _ => {}
        }
    }

    None
}

/// A name **without** its arguments, and the arguments beside it: `std::vector<int>` is the class `std::vector`
/// with one argument.
///
///
/// The parser folds a whole qualified name — qualifiers, the template argument list and all — into **one
/// `NameExpr`**, so its text is `std::vector<int>` and the argument list is a *child* of it. So the two readings a
/// caller needs come from one node, and the cut between them is the first `<`:
///
/// ```text
/// what a class question asks        `std::vector`     what has members
/// what a consumer shows             `std::vector<int>`  [`Type::Display`] writes it back
/// ```
///
/// Keeping the arguments **out** of the name rather than in it is what makes [`Type::class_name`] a field read
/// instead of a second parse, and what makes a type with arguments compare unequal to the same class without them —
/// `std::vector` and `std::vector<int>` are different types, and a `std::vector` written as a template argument is
/// not a vector of nothing.
///
/// The arguments are read from their own child rather than by parsing the text, because an argument's text can
/// contain commas and parentheses that a splitter would get wrong.
fn read_named(node: &CppSyntaxNode) -> Option<Type> {
    let written = node.text().to_string().trim().to_string();
    if written.is_empty() {
        return None;
    }

    // **The argument list has to be the *end* of the spelling.** Cutting at the first `<` took `Base<T>::reference`
    // as the class `Base<T>` — the `::reference` vanished, so a `typedef typename _Base::reference reference;`
    // recorded a type that names a class rather than the alias it is, and a hover on the variable deduced from it
    // printed the class. A template-id is a spelling that **ends** in its argument list; a qualified name with one in
    // the middle is not one, and this layer's model has no place to keep it, so the whole spelling stays the name.
    let trailing = trailing_template_id(&written);
    let (name, arguments) = match trailing {
        Some(at) => {
            let wanted = written[at..].trim().to_string();
            let list = node
                .descendants()
                .filter(|child| {
                    CppSyntaxKind::from(child.kind()) == CppSyntaxKind::TemplateArgumentList
                })
                // **The last list whose text is the group that ends the spelling** — `Foo<int>::Bar<int>` has two
                // identical-looking lists and only the second one is the type's own.
                .filter(|list| list.text().to_string().trim() == wanted)
                .last()
                .map(|list| read_template_arguments(&list))
                .unwrap_or_default();
            (written[..at].trim_end().to_string(), list)
        }
        None => (written, Vec::new()),
    };

    if name.is_empty() {
        return None;
    }

    Some(Type::Named { name, arguments })
}

/// The types written in a template argument list, in order.
///
/// Three shapes, and each is a different answer rather than a different syntax:
///
/// * a **type** (`std::vector<int>`) is read as one;
/// * a **non-type argument** (`std::array<int, 4>`) is a value, and this layer models no values — so it is read as
///   the name of what was written, which is what makes `std::array<int, 4>` ask about `std::array` and not about
///   `std::array<int, 4>`;
/// * a **pack expansion** (`std::tuple<Args...>`) is a parameter standing for several arguments, which is a shape
///   of its own because substituting into it is not substituting into a name.
pub(crate) fn read_template_arguments(list: &CppSyntaxNode) -> Vec<Type> {
    let mut arguments = Vec::new();

    for child in list.children() {
        let kind = CppSyntaxKind::from(child.kind());

        // A pack expansion is written as an argument ending in `...` — the parser gives it a node of its own, and
        // a spelling that ends in the ellipsis is the reading that does not depend on which node it chose.
        let text = child.text().to_string();
        let trimmed = text.trim();
        if trimmed.ends_with("...") {
            let inner = trimmed.trim_end_matches('.').trim();
            arguments.push(Type::Pack {
                of: Arc::new(Type::named(inner)),
            });
            continue;
        }

        match kind {
            CppSyntaxKind::TemplateArgument => {
                arguments.push(read_argument(&child));
            }
            _ => arguments.push(Type::named(trimmed)),
        }
    }

    arguments
}

/// One template argument: its type if it writes one, and its own spelling otherwise.
fn read_argument(node: &CppSyntaxNode) -> Type {
    // `typename T::type` is a dependent name: the parser wraps it, and the `typename` keyword is a claim that what
    // follows is a type. The name is what this layer can carry.
    for child in node.children_with_tokens() {
        let Some(inner) = child.into_node() else {
            continue;
        };

        match CppSyntaxKind::from(inner.kind()) {
            // Both wrappers: a written type name is what this arm is for, and a plain name is a `NamedType` while a
            // template-id is a `TemplateType`.
            CppSyntaxKind::TemplateType | CppSyntaxKind::NamedType => {
                if let Some(found) = read_named(&inner) {
                    return found;
                }
            }
            CppSyntaxKind::BuiltinType => {
                return Type::builtin(inner.text().to_string().trim());
            }
            _ => {}
        }
    }

    Type::named(node.text().to_string().trim())
}

/// Does this node's span **contain the start** of that range?
///
/// The weaker test, and the right one for walking *down* a declarator: a declarator's span ends at its own last
/// child, and for `* p` that is the `NameExpr`'s end — except in the shapes where a trailing space puts the end one
/// byte past the name and an extent or a parameter list puts it further. Asking whether the node's span contains
/// the name **whole** is therefore the wrong question on the way down (`* p`'s inner declarator ends where the name
/// does, so it does not "cover" it), while asking whether it contains the name's *start* is exactly the question
/// every level of the chain answers yes to.
fn reaches(node: &CppSyntaxNode, range: cpp_parser::SourceRange) -> bool {
    let own = node.text_range();
    usize::from(own.start()) <= range.start_offset && usize::from(own.end()) >= range.start_offset
}

/// The declarator **whose direct child is the name** — the bottom of the chain, where wrapping starts.
///
/// A declarator tree puts the name at the bottom: `* p` is a `Declarator` holding a `Declarator` holding the
/// `PointerType`, and a `NameExpr` holding `p`. This is the node whose *direct* child is that name, and finding it
/// is the whole of the walk below.
///
/// # Why the descent is by the name's *start*
///
/// Because a declarator's span is not required to contain the name whole. `* p`'s inner declarator spans `"* "` —
/// `(6, 8)` — while the name is `(8, 9)`: the end is one byte short, so a descent that asked "does this child
/// contain the name" would refuse the very node it is looking for and stop at the outer one, which is the shape
/// that read `Widget* p` as `Widget`.
fn declarator_holding_the_name(
    declarator: &CppSyntaxNode,
    name: cpp_parser::SourceRange,
) -> CppSyntaxNode {
    let mut node = declarator.clone();

    loop {
        let next = node
            .children()
            .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::Declarator)
            .find(|child| reaches(child, name));

        match next {
            Some(child) => node = child,
            None => return node,
        }
    }
}

/// **Wrap `base` in everything the declarator wrote around the name** — from the outside in.
///
/// # Why the outside comes first
///
/// Because that is the order the type is built in, and the parentheses are what make it so. Without them a
/// declarator reads left to right and the operators apply in the order they are met: `Widget* p` is a pointer to
/// `Widget`, and iterating the levels from the name outwards gets that right by accident. With them the reading
/// flips: `int (*p)[4]` is a pointer to an array, and `typedef void (*Callback)(int)` is a pointer to a function —
/// while the same tokens without parentheses (`int *p[4]`, `void *Callback(int)`) are an array of pointers and a
/// function returning a pointer. A walk that started at the name produced `void*(int)` for the third of those:
/// **a function returning a pointer, which is the type the file did not write**.
///
/// # The rule at one level
///
/// ```text
/// a child Declarator            the way further in: recurse, then apply this level's operators to the result
/// no child Declarator           the name is here: apply this level's operators to the base
/// ```
///
/// and "this level's operators" are its children that are not that declarator — `PointerType`, `ReferenceType`,
/// `ArrayType`, `ParameterList`. The chain always ends at a level whose child *is* the name, because that is what
/// [`declarator_holding_the_name`] descended to; the `innermost` argument is only the stop condition.
///
/// A `ParameterList` is an operator like the others, and that is the whole of the difference between a function and
/// a pointer to one: `int f(int)` has the list at the level holding `f`, while `int (*f)(int)` has it at the level
/// *outside* the parentheses, so the pointer is applied first and the list wraps the pointer.
fn wrap(
    base: Type,
    outermost: &CppSyntaxNode,
    innermost: &CppSyntaxNode,
    name: cpp_parser::SourceRange,
) -> Type {
    /// One operator at one level, applied to the type read so far.
    ///
    /// `None` for a node that is not an operator, which is most of them: a `NameExpr`, a `DeclSpecifierSeq`, an
    /// initializer, an attribute. The caller loops rather than matching, because a level can hold more than one
    /// operator (`int (*p)[4]`), and the order among them is the order they are written in.
    fn apply(wrapped: &Type, node: &CppSyntaxNode) -> Option<Type> {
        Some(match CppSyntaxKind::from(node.kind()) {
            CppSyntaxKind::PointerType => Type::Pointer {
                to: Arc::new(wrapped.clone()),
            },
            CppSyntaxKind::ReferenceType | CppSyntaxKind::RValueReferenceType => Type::Reference {
                to: Arc::new(wrapped.clone()),
                rvalue: node.text().to_string().trim().starts_with("&&"),
            },
            CppSyntaxKind::ArrayType => Type::Array {
                of: Arc::new(wrapped.clone()),
                extent: array_extent(node),
            },
            CppSyntaxKind::ParameterList => Type::Function {
                returns: Arc::new(wrapped.clone()),
                parameters: read_parameters(node),
            },
            // **A nested declarator that is not the way down is an operator wrapper.**
            //
            // The tree keeps the derived-type part of a declaration in a declarator of its own: `Widget* w` has the
            // star inside an inner `Declarator` holding a `PointerType`, while the **name** sits at the outer level.
            // [`at_level`] walks only the declarators that *reach the name*, and it treats everything else as an
            // ordinary child — which is right for a `NameExpr` and wrong for this, so the star was never applied and
            // `Widget* w` read as `Widget`. Measured: four type tests and three semantic ones failed on it.
            //
            // Recursing here applies the wrapper's own operators, in the order they are written, to what has been
            // read so far — which is what [`at_level`] would have done with them had they been written one level up.
            CppSyntaxKind::Declarator => {
                let mut wrapped = wrapped.clone();
                for inner in node.children() {
                    if let Some(next) = apply(&wrapped, &inner) {
                        wrapped = next;
                    }
                }
                wrapped
            }
            _ => return None,
        })
    }

    /// The type of one declarator level, given the type the level **inside** it produced.
    ///
    /// `inside` is the specifiers' type when this is the level holding the name, and `None` when the level inside
    /// this one is still to be read — which is why the parameter is an `Option` and why the operators are applied
    /// whether or not it is `Some`: a level that wraps nothing yet still *has* operators to contribute, and
    /// skipping them because the inner answer had not arrived yet is how `Widget* p` came back as `Widget`.
    fn at_level(
        level: &CppSyntaxNode,
        name: cpp_parser::SourceRange,
        inside: Option<Type>,
        base: &Type,
    ) -> Type {
        // **The way down is not an operator**, and this is the rule rather than a special case: the operators of a
        // declaration are the nodes that are *not* on the path from the type to the name. Without the filter,
        // `(*p)`'s only child is `*p` and the pointer would be applied twice.
        let mut wrapped = inside.unwrap_or_else(|| base.clone());
        for child in level.children() {
            // **Only a `Declarator` can be the way down**, and the test is its kind rather than "does it contain the
            // name". The containment test is true of a `ParameterList` and an `ArrayType` as well — they are written
            // after the name and their spans reach past it — so using it skipped exactly the operators this function
            // exists to apply, which is how `void (*Callback)(int)` came back as `void*(int)`.
            if CppSyntaxKind::from(child.kind()) == CppSyntaxKind::Declarator
                && reaches(&child, name)
            {
                continue;
            }
            if let Some(next) = apply(&wrapped, &child) {
                wrapped = next;
            }
        }

        wrapped
    }

    // The levels from the outside in, **found by the descent that already exists** rather than by a climb:
    // [`declarator_holding_the_name`] walks from the outermost declarator to the one holding the name, one strict
    // narrowing step at a time, and the path it takes *is* the list of levels. Writing the walk a second time here
    // — upwards — is what produced an eight-gigabyte allocation, because `ancestors()` can answer with the node it
    // was asked about and a loop that walks it never advances.
    let mut levels: Vec<CppSyntaxNode> = vec![outermost.clone()];
    let mut current = outermost.clone();
    while current.text_range() != innermost.text_range() {
        let Some(down) = current
            .children()
            .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::Declarator)
            .find(|child| reaches(child, name))
        else {
            break;
        };
        levels.push(down.clone());
        current = down;
    }

    // Applied **outside in**, and that direction is the whole of the difference between a function and a pointer to
    // one. Read the levels from the name outwards and each one is something done *to* what is inside it — that is
    // what the parentheses are for, and it is why a member's own type is a function of the level nearest the name
    // rather than of the outermost one:
    //
    // ```text
    // int (*f(int))(int)   `f`    → call it with an int            → `int(*)(int)(int)`'s inner part
    //                      `*f`   → dereference that               → `int(*)(int)`
    //                      `(*f)(int)` → call *that* with an int   → `int`
    // ```
    //
    // so iterating **innermost first** builds the type the declaration wrote, and iterating outermost first builds
    // `void*(int)` for `void (*cb)(int)` — a function returning a pointer where the file wrote a pointer to a
    // function. Measured: five tests failed on that one inversion, and the two about arrays failed on it as well
    // (`int (*p)[4]` came back as a function).
    let mut wrapped: Option<Type> = None;
    for level in levels.iter().rev() {
        wrapped = Some(at_level(level, name, wrapped, &base));
    }

    wrapped.unwrap_or(base)
}

/// The `[4]` of an array declarator, when it is a number this layer can read.
///
/// `None` for `[]` **and** for `[N]` — a size that is a name is a value this layer does not have, and the extent
/// of an array is not a question any query here asks. It is recorded so that a consumer showing the type can write
/// it back.
fn array_extent(node: &CppSyntaxNode) -> Option<usize> {
    let text = node.text().to_string();
    let inside = text
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim();
    inside.parse().ok()
}

/// The parameters of a `ParameterList`, as types — for a function type's shape.
fn read_parameters(list: &CppSyntaxNode) -> Vec<Type> {
    let mut parameters = Vec::new();

    for parameter in list.children() {
        if CppSyntaxKind::from(parameter.kind()) != CppSyntaxKind::Parameter {
            continue;
        }

        let specifiers = parameter
            .children()
            .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::DeclSpecifierSeq);
        let declarator = parameter
            .children()
            .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::Declarator);

        let Some(specifiers) = specifiers else {
            continue;
        };

        // A parameter with no declarator (`void f(int)`) declares no name: the whole of it is the type.
        match declarator {
            Some(declarator) => {
                let name = declarator
                    .descendants()
                    .find(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::NameExpr)
                    .map(|child| cpp_parser::source_range(child.text_range()))
                    .unwrap_or_else(|| cpp_parser::source_range(declarator.text_range()));
                parameters.push(type_of_declaration(&specifiers, Some(&declarator), name));
            }
            None => parameters.push(read_specifiers(&specifiers)),
        }
    }

    parameters
}
/// **A type spelling with some names replaced by what they stand for** — whole words only.
///
/// This is the spelling-level twin of [`Type::substituted`], and it exists for the one step that has no `Type` to
/// work with: an **alias template**'s target. `template <class _Test, class _Ty> using enable_if_t = typename
/// enable_if<_Test, _Ty>::type;` is stored as the spelling after the `=`, and `enable_if_t<int, double>` has to become
/// `typename enable_if<int, double>::type` before anything can be looked up in it. The target is text at that point —
/// it is what the file wrote — so the substitution is text too.
///
/// Whole words, because `_Ty` is a prefix of `_Ty2` and a substring replacement would rewrite a name the alias never
/// mentioned. An identifier that is not one of `names` is left alone, which is the ordinary case: the target's own
/// `enable_if` and its `typename` are not parameters.
pub fn substituted_spelling(written: &str, names: &[String], arguments: &[String]) -> String {
    substituted_spelling_from(written, names, arguments)
}

/// **[`substituted_spelling`] that falls back to the parameter's own default** — the step a use that writes fewer
/// arguments than the declaration has parameters needs.
///
/// `_Alloc` has no argument in `std::vector<int>`, and its default is `allocator<int>`: a member whose type says
/// `_Alloc::value_type` cannot be resolved from the use alone, and answering `_Alloc::value_type&` is a spelling that
/// has no meaning where the reader is standing. Measured on `lineStartOffset`: with the default it is
/// `allocator<int>::value_type&` → `int&`; without it the chain stopped one hop short.
///
/// The default is written in **the declaration's own parameter names** (`_Alloc = allocator<_Ty>`), so it is itself
/// substituted with the same arguments before it is used. A default that names its own parameter is left alone —
/// that is a spelling this layer cannot expand, and expanding it would not terminate.
pub fn substituted_spelling_with_defaults(
    written: &str,
    names: &[String],
    defaults: &[String],
    arguments: &[String],
) -> String {
    if names.is_empty() {
        return written.to_string();
    }

    let mut replacements: Vec<String> = Vec::with_capacity(names.len());
    for (at, name) in names.iter().enumerate() {
        let written_argument = arguments.get(at).filter(|argument| !argument.is_empty());
        let replacement = match written_argument {
            Some(argument) => argument.clone(),
            None => defaults
                .get(at)
                .map(|default| default.trim())
                .filter(|default| !default.is_empty() && *default != name && !name.is_empty())
                // **Substituted as it is read**, because a default is written in the same parameter names:
                // `_Alloc = allocator<_Ty>` has to become `allocator<int>` before it can stand for `_Alloc`.
                .map(|default| substituted_spelling(default, names, arguments))
                .unwrap_or_default(),
        };
        replacements.push(replacement);
    }

    substituted_spelling_from(written, names, &replacements)
}

/// The walk itself: replace whole words that are one of `names` with the replacement at the same position, and leave
/// everything else alone. An **empty** replacement means "keep the word" — the parameter had neither an argument nor
/// a default, which is the state the caller must not turn into an erasure.
fn substituted_spelling_from(written: &str, names: &[String], replacements: &[String]) -> String {
    // **A use may write fewer arguments than the class declares parameters, and that is not a refusal.**
    //
    // `std::vector<MarkEvent>` writes one argument for a class declared with two parameters (`_Ty`, `_Alloc`), because
    // the second has a default. Requiring equal lengths made this return the spelling unchanged, and the chain it
    // broke was measured end to end on a real project: `events[Pos].U` answered `Unknown(NotDeclaredHere("_Ty"))`
    // because `std::vector<MarkEvent>::operator[]`'s `_Ty&` was never substituted.
    //
    // The pairing is the language's own — the arguments fill the parameters *in order* — so a parameter with a written
    // argument is substituted and the rest keep the spelling the declaration gave them (or their default, when the
    // caller has one — see [`substituted_spelling_with_defaults`]).
    if names.is_empty() || replacements.is_empty() {
        return written.to_string();
    }

    let mut out = String::with_capacity(written.len());
    let mut word = String::new();

    let flush = |word: &mut String, out: &mut String| {
        if word.is_empty() {
            return;
        }

        // **A parameter with no replacement keeps the declaration's own spelling.** The pairing is by position, and a
        // use may write fewer arguments than the declaration has parameters (`std::vector<_Ty, _Alloc>` used as
        // `std::vector<int>`), so the lookup must not assume the two lists are the same length — indexing with the
        // parameter's position was an out-of-bounds read waiting for the first use that omitted an argument.
        match names
            .iter()
            .position(|name| !name.is_empty() && name == word)
            .and_then(|at| replacements.get(at))
            .filter(|replacement| !replacement.is_empty())
        {
            Some(replacement) => out.push_str(replacement),
            None => out.push_str(word),
        }
        word.clear();
    };

    for character in written.chars() {
        if character.is_alphanumeric() || character == '_' {
            word.push(character);
            continue;
        }

        flush(&mut word, &mut out);
        out.push(character);
    }
    flush(&mut word, &mut out);

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpp_parser::{CppParser, ParserConfig};

    use crate::Known;

    /// The type of `name`, read the way a query reads it — see [`type_of_declarator`].
    ///
    /// The **last** declaration of that name, because every fixture declares before it uses and the question these
    /// tests ask is about the declaration's own type.
    fn type_of(source: &str, name: &str) -> Type {
        type_of_declarator(source, name)
    }

    fn written(source: &str, name: &str) -> String {
        type_of(source, name).to_string()
    }

    /// The type of `name` read the way **a query reads it**: the specifier sequence, the declarator, and the range
    /// of the name the declarator declares.
    ///
    /// The **shortest** declarator containing the name is the one that declares it: a declaration holds the
    /// declarators of what it declares *and* of the parameters it takes, and both contain the name — so a search
    /// that took the first would answer about `g` when the question was about `value`.
    fn type_of_declarator(source: &str, name: &str) -> Type {
        let tree = CppParser::parse(source, ParserConfig::default());
        let root = tree.get_red_root();

        let declared = root
            .descendants()
            .filter(|child| CppSyntaxKind::from(child.kind()) == CppSyntaxKind::Declarator)
            .filter(|child| {
                child.descendants().any(|inner| {
                    CppSyntaxKind::from(inner.kind()) == CppSyntaxKind::NameExpr
                        && inner.text().to_string().trim() == name
                })
            })
            .min_by_key(|child| {
                usize::from(child.text_range().end()) - usize::from(child.text_range().start())
            })
            .unwrap_or_else(|| panic!("{name} is declared in {source:?}"));

        let specifiers = declared
            .ancestors()
            .find_map(|node| {
                node.children().find(|child| {
                    CppSyntaxKind::from(child.kind()) == CppSyntaxKind::DeclSpecifierSeq
                })
            })
            .unwrap_or_else(|| panic!("{name} has a specifier sequence in {source:?}"));

        type_of_declaration(
            &specifiers,
            Some(&declared),
            cpp_parser::source_range(declared.text_range()),
        )
    }

    #[test]
    fn a_builtin_is_its_words_in_order() {
        assert_eq!(written("int x;", "x"), "int");
        assert_eq!(
            written("unsigned long long big;", "big"),
            "unsigned long long"
        );
        assert_eq!(written("char c;", "c"), "char");
    }

    /// **The pointer lives in the declarator and the base in the specifiers**, and the two have to be joined in
    /// the right order — outermost operator nearest the type.
    #[test]
    fn a_pointer_and_a_reference_are_read_out_of_the_declarator() {
        assert_eq!(written("Widget* p;", "p"), "Widget*");
        assert_eq!(written("Widget** pp;", "pp"), "Widget**");
        assert_eq!(written("Widget& r;", "r"), "Widget&");
        assert_eq!(written("Widget&& rr;", "rr"), "Widget&&");
        assert_eq!(written("const Widget* q;", "q"), "Widget*");
    }

    /// **A name is read without its template arguments and with them.** The class question wants the first, the
    /// type a consumer shows is the second, and both come from one node.
    #[test]
    fn a_name_keeps_its_arguments_and_offers_its_class() {
        let found = type_of("std::vector<int> v;", "v");
        assert_eq!(found.to_string(), "std::vector<int>");
        assert_eq!(found.class_name(), Some("std::vector"));
        assert_eq!(found.arguments().len(), 1);
        assert_eq!(found.arguments()[0].to_string(), "int");

        let nested = type_of("std::map<std::string, int> m;", "m");
        assert_eq!(nested.class_name(), Some("std::map"), "{nested}");
        assert_eq!(nested.arguments().len(), 2, "{nested}");

        // **The bug this was written for**: the whole spelling handed to a class query. `_Cont>` and
        // `std::vector<int>` are not class names, and a query given one answers "nothing declares it".
        let no_arguments = type_of("std::vector<int> v;", "v");
        assert_ne!(no_arguments.class_name(), Some("std::vector<int>"));
    }

    /// An array's extent is kept, and an array **decays** to a pointer when it is used — which is the difference
    /// between what a declaration says and what an expression of it is.
    #[test]
    fn an_array_keeps_its_extent_and_decays_when_used() {
        let array = type_of("int arr[4];", "arr");
        assert_eq!(array.to_string(), "int[4]");
        assert_eq!(array.decay().to_string(), "int*");
        assert_eq!(array.class_name(), None);

        let unknown = type_of("int arr[];", "arr");
        assert_eq!(unknown.to_string(), "int[]");
    }

    /// A reference **decays to what it refers to**, which is what makes `Widget& w; w.size` a member of `Widget`.
    #[test]
    fn a_reference_decays_to_what_it_refers_to() {
        let reference = type_of("Widget& r;", "r");
        assert_eq!(reference.decay().to_string(), "Widget");
        assert_eq!(
            reference.class_name(),
            Some("Widget"),
            "a class question sees through it"
        );
    }

    /// A `*` on a pointer gives the pointee, and on anything else gives nothing — a class with an `operator*` needs
    /// the index, and an ill-formed `*` needs a diagnostic this layer does not write.
    #[test]
    fn a_pointee_is_read_out_of_a_pointer() {
        assert_eq!(
            type_of("Widget* p;", "p")
                .pointee()
                .map(|found| found.to_string()),
            Some("Widget".to_string())
        );
        assert_eq!(type_of("int x;", "x").pointee(), None);
    }

    /// **A function's type is what it returns and what it takes**, and the parameters of a function *pointer*
    /// belong to the pointer's pointee rather than to the declaration.
    #[test]
    fn a_function_type_keeps_its_parameters() {
        let found = type_of("int f(int a, double b);", "f");
        assert_eq!(found.to_string(), "int(int, double)");
    }

    /// **A template parameter is a name nothing has defined yet**, which is a different answer from a class of the
    /// same spelling — and a caller tells them apart by what it knows, not by the text.
    #[test]
    fn a_parameter_in_a_template_body_is_read_as_a_name() {
        let found = type_of("template <class T> void f(T value) { }", "value");
        assert_eq!(found.to_string(), "T");
        assert_eq!(
            found.class_name(),
            Some("T"),
            "the text is a name either way"
        );

        let parameter = Type::TemplateParameter {
            name: "T".to_string(),
        };
        assert!(parameter.depends_on_a_parameter());
        assert!(!Type::named("Widget").depends_on_a_parameter());
    }

    /// **Substitution is by name and by position** — the operation that makes a member of `std::vector<T>`
    /// answerable once the use says what `T` is.
    #[test]
    fn a_parameter_is_substituted_by_name() {
        let names = vec!["T".to_string(), "Alloc".to_string()];
        let arguments = vec![Type::builtin("int"), Type::named("std::allocator<int>")];
        let substitutions = TypeSubstitutions::new(&names, &arguments);

        let reference = Type::Reference {
            to: Arc::new(Type::TemplateParameter {
                name: "T".to_string(),
            }),
            rvalue: false,
        };
        assert_eq!(reference.substituted(&substitutions).to_string(), "int&");

        let pair = Type::Named {
            name: "std::pair".to_string(),
            arguments: vec![
                Type::TemplateParameter {
                    name: "T".to_string(),
                },
                Type::TemplateParameter {
                    name: "Alloc".to_string(),
                },
            ],
        };
        assert_eq!(
            pair.substituted(&substitutions).to_string(),
            "std::pair<int, std::allocator<int>>"
        );

        // A parameter with no argument stays itself rather than becoming nothing: a `std::vector` written without
        // arguments is not a vector of nothing.
        let none = TypeSubstitutions::new(&names, &[]);
        assert_eq!(
            Type::TemplateParameter {
                name: "T".to_string()
            }
            .substituted(&none)
            .to_string(),
            "T"
        );
    }

    /// A type that holds a parameter anywhere is one only instantiation can finish — the predicate a caller uses
    /// before spending an index query on it.
    #[test]
    fn a_type_that_holds_a_parameter_says_so() {
        assert!(
            Type::Pointer {
                to: Arc::new(Type::TemplateParameter {
                    name: "T".to_string()
                })
            }
            .depends_on_a_parameter()
        );

        assert!(
            Type::Named {
                name: "std::vector".to_string(),
                arguments: vec![Type::TemplateParameter {
                    name: "T".to_string()
                }],
            }
            .depends_on_a_parameter()
        );

        assert!(!Type::named("std::vector<int>").depends_on_a_parameter());
    }

    /// **Attributes and declaration specifiers are not part of a type.** The old reader answered
    /// `const [[nodiscard]] constexpr size_type` for a return type, because it stripped a list of keywords from
    /// text; the syntax says which nodes are the type, and everything else is dropped.
    #[test]
    fn attributes_and_specifiers_are_not_part_of_the_type() {
        assert_eq!(written("static const int count = 1;", "count"), "int");
        assert_eq!(written("constexpr int size = 2;", "size"), "int");
        assert_eq!(written("[[nodiscard]] int f();", "f"), "int()");
    }

    /// **A macro standing where a specifier goes is not the type** — the **last** name in the sequence is.
    ///
    /// The shape is MSVC's, and it is not a corner: `<iostream>` declares `cin` as
    /// `_EXPORT_STD extern "C++" __PURE_APPDOMAIN_GLOBAL _CRTDATA2_IMPORT istream cin;`, which the grammar reads as
    /// a specifier sequence holding three names. A reader that took the first recorded
    /// `__PURE_APPDOMAIN_GLOBAL` where the cooked reading of the same line recorded `istream` — so the two
    /// declarations of `cin` disagreed about the type and **neither answered**: `std::cin`, `std::cin.read` and a
    /// completion after `std::cin.` all had nothing to say.
    #[test]
    fn the_last_name_in_a_specifier_sequence_is_the_type() {
        assert_eq!(
            written(
                "extern \"C++\" __PURE_APPDOMAIN_GLOBAL _CRTDATA2_IMPORT istream cin;",
                "cin"
            ),
            "istream"
        );
        // And the ordinary single-name case is unaffected.
        assert_eq!(written("Widget w;", "w"), "Widget");
    }

    /// **A spelling read back into a shape**, which is what the index path needs: a `DeclFact` records the type as
    /// text, and a member query needs the *class* in it.
    ///
    /// The one thing pinned here is [`Type::class_name`], because everything else a wrong reading loses is
    /// recoverable and a wrong class name is not: it sends a member query to a class that does not exist.
    #[test]
    fn a_spelling_is_read_back_into_the_class_it_names() {
        let class_of = |written: &str| {
            parse_type_spelling(written)
                .class_name()
                .map(str::to_string)
        };
        assert_eq!(class_of("Widget"), Some("Widget".to_string()));
        assert_eq!(
            class_of("std::vector<int>"),
            Some("std::vector".to_string())
        );
        assert_eq!(
            class_of("std::map<std::string, int>"),
            Some("std::map".to_string()),
            "the commas inside the argument list are not the type's own"
        );
        assert_eq!(
            class_of("Widget*"),
            None,
            "a pointer names no class — decay first"
        );
        assert_eq!(class_of("const Widget&"), Some("Widget".to_string()));
        assert_eq!(class_of("Widget&&"), Some("Widget".to_string()));
        assert_eq!(
            class_of("const Widget* const"),
            None,
            "the `const` on either side does not make it a class"
        );
        assert_eq!(
            class_of("std::vector<std::pair<int, int>>&"),
            Some("std::vector".to_string())
        );
        // **A builtin is not a class**, which is why `parse_type_spelling` carries the language's own list of words:
        // reading `int` as a name would make a member access on an `int` ask the index for a class called `int` and
        // report "nothing declares it" — which a reader takes as "this class is missing" rather than "this is not a
        // class at all".
        assert_eq!(class_of("int"), None);
    }

    /// **The operators come off in the order they were written**, which is the order that makes the last one the
    /// outermost.
    #[test]
    fn a_spelling_peels_its_operators_outside_in() {
        let pointed = parse_type_spelling("Widget*");
        assert!(
            matches!(pointed, Type::Pointer { .. }),
            "a trailing `*` is a pointer, whatever it is written after: {pointed:?}"
        );
        assert_eq!(pointed.to_string(), "Widget*");
        assert_eq!(parse_type_spelling("Widget**").to_string(), "Widget**");
        assert_eq!(parse_type_spelling("Widget&").to_string(), "Widget&");
        assert_eq!(parse_type_spelling("Widget&&").to_string(), "Widget&&");

        // A reference to a pointer and a pointer to a reference are the same spelling read from the two ends, and
        // the difference is which end was peeled first: the last operator written is the outermost one.
        let reference_to_pointer = parse_type_spelling("Widget*&");
        assert!(
            matches!(reference_to_pointer, Type::Reference { .. }),
            "the outermost operator is the one written last: {reference_to_pointer:?}"
        );
        // **A reference is not a pointer**, and the two questions are deliberately different: `pointee` answers
        // "what does a `*` give", which a reference has no operator for, while `decay` and `class_name` see through
        // it because C++ does.
        assert!(
            reference_to_pointer.pointee().is_none(),
            "`*` on a reference is not the pointer it refers to"
        );
        // A reference to a *class*, which is the shape a member access is written on, does see through.
        assert_eq!(
            parse_type_spelling("Widget&").class_name(),
            Some("Widget"),
            "a member access on a reference is a member of what it refers to"
        );

        // Arrays nest from the right: `int[2][3]` is two arrays of three, so peeling `[3]` leaves `int[2]` for the
        // recursion and the *outer* type is the `[2]`.
        let nested = parse_type_spelling("int[2][3]");
        assert_eq!(nested.to_string(), "int[2][3]");
        assert_eq!(
            nested.decay().to_string(),
            "int[2]*",
            "an array decays to a pointer to its element, and the element here is itself an array"
        );
    }

    /// **A function type keeps what it takes.** `int(int, double)` is what a `DeclFact::returns` records for a
    /// function, and reading it back must not lose the parameters — a call query asks how many there are.
    #[test]
    fn a_function_spelling_keeps_its_parameters() {
        let found = parse_type_spelling("int(int, double)");
        assert_eq!(found.to_string(), "int(int, double)");

        let Type::Function {
            returns,
            parameters,
        } = &found
        else {
            panic!("a parameter list at the end is a function type: {found:?}");
        };
        assert_eq!(returns.to_string(), "int");
        assert_eq!(parameters.len(), 2);
    }

    /// **A sub-type is shared, not copied.** This is the property the [`Arc`] is there for, and it is worth a test
    /// because the failure is invisible: an owned child still *works*, it just clones a whole tree every time a
    /// query looks inside a type.
    ///
    /// `Arc::ptr_eq` is the check that cannot pass by accident: two equal types built separately are two
    /// allocations, so a `pointee` that answered with a fresh one would fail here.
    #[test]
    fn a_sub_type_is_handed_out_shared_rather_than_copied() {
        let pointee: TypeOf = Arc::new(Type::named("Widget"));
        let pointer = Type::Pointer {
            to: Arc::clone(&pointee),
        };

        let handed_out = pointer.pointee().expect("a pointer has a pointee");
        assert!(
            Arc::ptr_eq(&handed_out, &pointee),
            "the pointee is the same allocation, not a copy of it"
        );

        // A clone of a compound type shares its children rather than copying them down.
        let cloned = pointer.clone();
        let Type::Pointer { to } = &cloned else {
            panic!("a clone of a pointer is a pointer");
        };
        assert!(Arc::ptr_eq(to, &pointee));
    }

    /// **A reference target is judged by its referent**, which is where the standard library's signatures live.
    ///
    /// `void push_back(const _Ty&)` instantiated at `int` declares `const int&`, and this relation had no arm for a
    /// `Reference` on the right: a `const char*` against it fell through to `Unknown`, and a check that reports only
    /// a definite `No` said nothing — measured on a `std::vector<int>`, `push_back("123131")` was silent while the
    /// same mistake against a by-value `int` parameter was reported.
    ///
    /// The two answers below are the whole rule: binding to a reference is the conversion to the referent, so a
    /// string where an `int` is expected is `No`, and an `int` where an `int` is expected is `Yes`.
    #[test]
    fn a_reference_target_is_judged_by_its_referent() {
        let a_string = parse_type_spelling("const char[7]");
        let an_int_reference = parse_type_spelling("const int&");
        assert!(
            matches!(a_string.convertible_to(&an_int_reference), Known::No),
            "a string literal does not bind to a `const int&`: {:?}",
            a_string.convertible_to(&an_int_reference)
        );

        let an_int = parse_type_spelling("int");
        assert!(
            matches!(an_int.convertible_to(&an_int_reference), Known::Yes(())),
            "an `int` binds to a `const int&`: {:?}",
            an_int.convertible_to(&an_int_reference)
        );

        // And the rule is about the **referent's** class, not about references in general: a `std::string`
        // parameter may have a converting constructor, so the answer stays `Unknown` rather than becoming `No`.
        let a_string_target = parse_type_spelling("const std::string&");
        assert!(
            matches!(a_string.convertible_to(&a_string_target), Known::Unknown(_)),
            "a class with a conversion constructor is not this layer's answer: {:?}",
            a_string.convertible_to(&a_string_target)
        );
    }
}
