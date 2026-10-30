//! **What syntax a parse may accept, as a bitmask.**
//!
//! A project is never "C++23" *or* "GNU". It is `-std=gnu++23`, which is C++23 **and** GCC's extensions; a Windows build
//! is C++20 **and** MSVC's spellings; a C file may be `-std=gnu89`, where statement expressions are available and a
//! variable-length array is not. A single enum cannot say any of those, and a `Vec` of them is more than the question
//! costs: this is **one `u64`**, `Copy`, and every query is a shift and an AND.
//!
//! # The bits
//!
//! ```text
//!   bits 0..9    one per Standard: C89 C99 C11 C17 C23, C++11 C++14 C++17 C++20 C++23
//!   bits 10..12  one per Vendor:   GNU, MSVC, Clang
//!   bits 13..    free — a feature that needs its own flag (`-fno-exceptions`) has somewhere to go
//! ```
//!
//! Coverage — "a later standard accepts the earlier ones of its family" — is a **precomputed mask** per standard
//! ([`Standard::covers_mask`]), so `support` never walks a list.
//!
//! # The three layers
//!
//! ```text
//! Standard   one published standard per family; covers the earlier ones of that family (`-std=c++20` accepts C++17)
//! Vendor     an implementation's own spellings; independent of the standard and combinable with it and with each other
//! Feature    one syntax feature the grammar branches on, and the **requirement** that enables it:
//!            Standard(..) | Vendor(..) | AnyOf(&[..])   — designated initializers are C99 *and* C++20
//! ```
//!
//! # Why a feature carries its requirement
//!
//! Because the alternative — a standard plus a list of exceptions, or an `if config.standard >= Cpp20 || config.c99` at
//! every branch — spreads the same knowledge over every call site. A feature whose requirement changes is one line here.

use std::fmt;

/// The language family a standard belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Family {
    C,
    Cpp,
}

/// **A published standard**, one per family.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Standard {
    C89,
    C99,
    C11,
    C17,
    C23,
    Cpp11,
    Cpp14,
    Cpp17,
    Cpp20,
    Cpp23,
}

/// **Every standard, in the order of their bits** — the table `covers_mask` is built from.
pub const STANDARDS: [Standard; 10] = [
    Standard::C89,
    Standard::C99,
    Standard::C11,
    Standard::C17,
    Standard::C23,
    Standard::Cpp11,
    Standard::Cpp14,
    Standard::Cpp17,
    Standard::Cpp20,
    Standard::Cpp23,
];

impl Standard {
    pub fn family(self) -> Family {
        match self {
            Standard::C89 | Standard::C99 | Standard::C11 | Standard::C17 | Standard::C23 => {
                Family::C
            }
            Standard::Cpp11
            | Standard::Cpp14
            | Standard::Cpp17
            | Standard::Cpp20
            | Standard::Cpp23 => Family::Cpp,
        }
    }

    /// Where this standard sits in its family — C89 is 0, C23 is 4; C++11 is 0, C++23 is 4.
    pub fn rank(self) -> u8 {
        match self {
            Standard::C89 | Standard::Cpp11 => 0,
            Standard::C99 | Standard::Cpp14 => 1,
            Standard::C11 | Standard::Cpp17 => 2,
            Standard::C17 | Standard::Cpp20 => 3,
            Standard::C23 | Standard::Cpp23 => 4,
        }
    }

    /// This standard's own bit.
    pub const fn bit(self) -> u64 {
        1 << (self as u32)
    }

    /// **Every standard that covers this one** — this one and the later ones of its family. The mask `support` ANDs
    /// with: enabling `C++23` enables the bit for `C++17`'s requirements because `C++23` is inside that mask.
    ///
    /// The direction is the whole care of this function. "The standards this one covers" — the earlier ones — is the
    /// inverse, and using it asked `C++23 & {C++11..C++17}` which is zero: every standard query failed, on every config,
    /// which is how it was caught (ten of twelve tests, all of them the ones mentioning a standard).
    pub const fn covered_by_mask(self) -> u64 {
        let (base, rank) = match self {
            Standard::C89 => (0u32, 0u32),
            Standard::C99 => (0, 1),
            Standard::C11 => (0, 2),
            Standard::C17 => (0, 3),
            Standard::C23 => (0, 4),
            Standard::Cpp11 => (5, 0),
            Standard::Cpp14 => (5, 1),
            Standard::Cpp17 => (5, 2),
            Standard::Cpp20 => (5, 3),
            Standard::Cpp23 => (5, 4),
        };
        // `5 - rank` bits, starting at this standard's own bit: `C++17` (rank 2, base 5) → bits 7..9, the standards at or
        // after it in its family.
        (((1u64 << (5 - rank)) - 1) << (base + rank)) & 0b11111_11111
    }

    /// The inverse of [`Standard::covered_by_mask`]: every standard this one accepts — its own and the earlier ones of
    /// its family. Kept because the question "what does enabling this standard give me?" is a different one, and a
    /// caller asking it should not have to invert the arithmetic by hand.
    pub const fn covers_mask(self) -> u64 {
        let (base, rank) = match self {
            Standard::C89 => (0u32, 0u32),
            Standard::C99 => (0, 1),
            Standard::C11 => (0, 2),
            Standard::C17 => (0, 3),
            Standard::C23 => (0, 4),
            Standard::Cpp11 => (5, 0),
            Standard::Cpp14 => (5, 1),
            Standard::Cpp17 => (5, 2),
            Standard::Cpp20 => (5, 3),
            Standard::Cpp23 => (5, 4),
        };
        ((1u64 << (rank + 1)) - 1) << base
    }

    /// **Does enabling this standard enable `needed`?** — same family, and at least as late.
    pub fn covers(self, needed: Standard) -> bool {
        self.family() == needed.family() && self.rank() >= needed.rank()
    }
}

impl fmt::Display for Standard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Standard::C89 => "c89",
            Standard::C99 => "c99",
            Standard::C11 => "c11",
            Standard::C17 => "c17",
            Standard::C23 => "c23",
            Standard::Cpp11 => "c++11",
            Standard::Cpp14 => "c++14",
            Standard::Cpp17 => "c++17",
            Standard::Cpp20 => "c++20",
            Standard::Cpp23 => "c++23",
        })
    }
}

/// **An implementation whose own spellings a parse may accept.** Independent of the standard, and combinable with it and
/// with each other: `clang -std=gnu++20` is C++20 plus GNU.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Vendor {
    Gnu,
    Msvc,
    Clang,
}

impl Vendor {
    pub const fn bit(self) -> u64 {
        1 << (10 + self as u32)
    }
}

impl fmt::Display for Vendor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Vendor::Gnu => "gnu",
            Vendor::Msvc => "msvc",
            Vendor::Clang => "clang",
        })
    }
}

/// **What enables one feature.**
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Requirement {
    /// One standard — satisfied by that standard or any later one of the same family.
    Standard(Standard),
    /// One implementation's extensions.
    Vendor(Vendor),
    /// Either of these — `designated initializers` are C99 and C++20; `typeof` is C23 and GNU.
    AnyOf(&'static [Requirement]),
}

/// **One syntax feature the grammar branches on.**
///
/// A feature belongs here when a **grammar decision** depends on it — not when a library arrives with a standard, which
/// is not a syntax question.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Feature {
    // ---- C++11 ----
    /// A raw string literal, `R"(…)"` — C++11, and `LexerConfig::supports_raw_strings` gates it.
    RawStrings,
    Auto,
    Lambda,
    RvalueReferences,
    VariadicTemplates,
    AliasTemplates,
    /// `[[attribute]]` in C++.
    Attributes,
    /// `static_assert` in C++.
    StaticAssert,
    /// `alignas` / `alignof` in C++ — C11 spells them `_Alignas` / `_Alignof`, which is [`Feature::Alignas`].
    AlignasCpp,
    /// `thread_local` in C++ — C11 spells it `_Thread_local`, which is [`Feature::ThreadLocalC`].
    ThreadLocal,
    // ---- C++14 ----
    GenericLambda,
    VariableTemplates,
    BinaryLiterals,
    DigitSeparators,
    // ---- C++17 ----
    StructuredBindings,
    IfConstexpr,
    FoldExpressions,
    InlineVariables,
    ClassTemplateArgumentDeduction,
    // ---- C++20 ----
    Concepts,
    Modules,
    Coroutines,
    DesignatedInitializers,
    ThreeWayComparison,
    UsingEnum,
    Char8T,
    Consteval,
    /// `constinit` — C++20.
    Constinit,
    // ---- C++23 ----
    DeducingThis,
    IfConsteval,
    MultidimensionalSubscript,
    StaticOperator,
    // ---- C89 ----
    /// The C89 baseline itself: a declaration must precede statements, no `//`, no `long long`, no `//`…
    C89Declarations,
    // ---- C99 ----
    VariableLengthArrays,
    /// `//` line comments — C99, and C++ from the start.
    LineComments,
    LongLong,
    Restrict,
    InlineC,
    BoolC,
    FlexibleArrayMembers,
    VariadicMacros,
    CompoundLiterals,
    // ---- C11 ----
    GenericSelection,
    Atomic,
    StaticAssertC,
    Alignas,
    Noreturn,
    AnonymousStructs,
    ThreadLocalC,
    // ---- C17 ----
    /// C17 added no syntax of its own: it is C11 with defect reports. The variant exists so a caller can ask, and so the
    /// table says so rather than leaving a reader to wonder.
    C17NoNewSyntax,
    // ---- C23 ----
    /// `[[attribute]]` syntax in C.
    AttributesC,
    Nullptr,
    Constexpr,
    BitInt,
    AutoC,
    EnumeratorsWithAFixedType,
    EmptyInitializer,
    Elifdef,
    // ---- vendor extensions ----
    StatementExpression,
    /// **The address of a label** — `&&here`, GNU's operand for a computed `goto`. It is what an interpreter's dispatch
    /// table is built from, and it is the one `&&` that is not a logical operator.
    LabelsAsValues,
    ElvisOperator,
    AsmBlock,
    NestedFunction,
    /// `typeof` — C23, and GNU in every mode (measured: `__typeof__` parsed and `typeof` did not).
    Typeof,
    /// `__try` / `__except` / `__finally` — MSVC's structured exception handling.
    StructuredExceptionHandling,
    /// `__interface` — MSVC.
    Interface,
    /// A block literal, `^(int n) { … }` — Clang.
    BlockLiteral,
    /// `__restrict` and `__restrict__` — GNU's spelling of C99's `restrict`, accepted in C++ mode too.
    RestrictGnu,
    /// `__int128`, a builtin type on GCC and Clang and not a type at all on MSVC.
    Int128,
}

impl Feature {
    /// What a parse must enable for this feature to be accepted.
    pub fn requirement(self) -> Requirement {
        use Feature::*;
        use Requirement::{AnyOf, Standard as S, Vendor as V};
        match self {
            // C++ — each feature to the standard that introduced it.
            Auto | Lambda | RvalueReferences | VariadicTemplates | AliasTemplates | Attributes
            | StaticAssert | RawStrings | AlignasCpp | ThreadLocal => S(Standard::Cpp11),
            GenericLambda | VariableTemplates => S(Standard::Cpp14),
            // **Two routes each, because C23 took both from C++14.** `0b1011` and `1'000'000` were C++14's and became
            // C's in C23, so a `-std=c23` file that writes them is a file that compiles — and this table said it did
            // not, which was two of the syntax errors a nine-file C corpus produced (`c23.c`'s literals).
            BinaryLiterals | DigitSeparators => AnyOf(&[S(Standard::Cpp14), S(Standard::C23)]),
            StructuredBindings
            | IfConstexpr
            | FoldExpressions
            | InlineVariables
            | ClassTemplateArgumentDeduction => S(Standard::Cpp17),
            Concepts | Modules | Coroutines | ThreeWayComparison | UsingEnum | Char8T
            | Consteval | Constinit => S(Standard::Cpp20),
            // C99 **and** C++20 — the reason a feature carries a `Requirement` rather than a single standard.
            DesignatedInitializers => AnyOf(&[S(Standard::C99), S(Standard::Cpp20)]),
            DeducingThis | IfConsteval | MultidimensionalSubscript | StaticOperator => {
                S(Standard::Cpp23)
            }

            // C.
            C89Declarations => S(Standard::C89),
            VariableLengthArrays => AnyOf(&[S(Standard::C99), V(Vendor::Gnu)]),
            LineComments => AnyOf(&[S(Standard::C99), S(Standard::Cpp11)]),
            LongLong | InlineC | BoolC | FlexibleArrayMembers | VariadicMacros
            | CompoundLiterals => S(Standard::C99),
            // C99's `restrict`, and GNU accepts its own spelling in every mode — including C++.
            Restrict => AnyOf(&[S(Standard::C99), V(Vendor::Gnu)]),
            GenericSelection | Atomic | StaticAssertC | Alignas | Noreturn | AnonymousStructs
            | ThreadLocalC => S(Standard::C11),
            C17NoNewSyntax => S(Standard::C17),
            // **Two routes each**: C++11 and C23. `nullptr`, `constexpr` and `[[attribute]]` were C++'s first and became
            // C's in C23, which is what a disjunction in the table is for.
            AttributesC | Nullptr | Constexpr => AnyOf(&[S(Standard::Cpp11), S(Standard::C23)]),
            // `AttributesC`, `Nullptr` and `Constexpr` are **not** here: they have a second route (C++11) and are matched
            // by the arm above. A duplicate in this list silently won, because a `match` takes the last arm that names a
            // variant — which is what made `nullptr` C23-only for an afternoon.
            AutoC | EnumeratorsWithAFixedType | EmptyInitializer | Elifdef => S(Standard::C23),
            // **`_BitInt` is C23's, and every C compiler accepts it in every C mode.** Measured with gcc 15:
            // `struct bits { unsigned _BitInt(3) three : 3; _BitInt(13) signed_wide : 13; _BitInt(5) plain; };`
            // compiles with `-std=c99`, `-std=c11`, `-std=c17` and even `-std=gnu89` (exit 0, no diagnostic), and
            // clang 18 accepts it in `-std=c++23`. Gating the grammar on C23 alone therefore misreads a declaration a
            // compiler accepts whenever the standard is **not detected** — no compile database, a header opened on its
            // own — as a call: the struct's four members came out as none, and the reader saw a type nobody declares.
            // The bit is the only use of the answer — "is `_BitInt(` a type here" — and the detector around it already
            // requires a parenthesised width followed by an identifier, so an ordinary variable named `_BitInt` is
            // untouched.
            BitInt => AnyOf(&[S(Standard::C89), S(Standard::Cpp11)]),

            // Vendors.
            StatementExpression | ElvisOperator | AsmBlock | NestedFunction | LabelsAsValues => {
                V(Vendor::Gnu)
            }
            RestrictGnu | Int128 => V(Vendor::Gnu),
            Typeof => AnyOf(&[S(Standard::C23), V(Vendor::Gnu)]),
            StructuredExceptionHandling | Interface => V(Vendor::Msvc),
            BlockLiteral => V(Vendor::Clang),
        }
    }

    /// The feature's name as a reader writes it.
    pub fn name(self) -> &'static str {
        use Feature::*;
        match self {
            Auto => "auto",
            Lambda => "lambda",
            RvalueReferences => "rvalue references",
            VariadicTemplates => "variadic templates",
            AliasTemplates => "alias templates",
            Attributes => "[[attribute]]",
            StaticAssert => "static_assert",
            AlignasCpp => "alignas",
            ThreadLocal => "thread_local",
            GenericLambda => "generic lambda",
            VariableTemplates => "variable templates",
            BinaryLiterals => "binary literals",
            DigitSeparators => "digit separators",
            StructuredBindings => "structured bindings",
            IfConstexpr => "if constexpr",
            FoldExpressions => "fold expressions",
            InlineVariables => "inline variables",
            ClassTemplateArgumentDeduction => "class template argument deduction",
            Concepts => "concepts",
            Modules => "modules",
            Coroutines => "coroutines",
            DesignatedInitializers => "designated initializers",
            ThreeWayComparison => "three-way comparison",
            UsingEnum => "using enum",
            Char8T => "char8_t",
            Consteval => "consteval",
            Constinit => "constinit",
            DeducingThis => "deducing this",
            IfConsteval => "if consteval",
            MultidimensionalSubscript => "multidimensional subscript",
            StaticOperator => "static operator",
            C89Declarations => "C89 declarations",
            VariableLengthArrays => "variable-length arrays",
            LineComments => "// comments",
            LongLong => "long long",
            Restrict => "restrict",
            InlineC => "inline",
            BoolC => "_Bool",
            FlexibleArrayMembers => "flexible array members",
            VariadicMacros => "variadic macros",
            CompoundLiterals => "compound literals",
            GenericSelection => "_Generic",
            Atomic => "_Atomic",
            StaticAssertC => "_Static_assert",
            Alignas => "_Alignas",
            Noreturn => "_Noreturn",
            AnonymousStructs => "anonymous structs",
            ThreadLocalC => "_Thread_local",
            C17NoNewSyntax => "C17's syntax",
            AttributesC => "[[attribute]] in C",
            Nullptr => "nullptr",
            Constexpr => "constexpr",
            BitInt => "_BitInt",
            AutoC => "auto in C",
            EnumeratorsWithAFixedType => "enum with a fixed underlying type",
            EmptyInitializer => "empty initializer",
            Elifdef => "#elifdef",
            StatementExpression => "statement expression",
            LabelsAsValues => "the address of a label",
            ElvisOperator => "elvis operator",
            AsmBlock => "asm block",
            NestedFunction => "nested function",
            Typeof => "typeof",
            StructuredExceptionHandling => "__try/__except",
            Interface => "__interface",
            BlockLiteral => "block literal",
            RestrictGnu => "__restrict",
            RawStrings => "raw string literal",
            Int128 => "__int128",
        }
    }
}

/// **The syntax a parse accepts**, as one `u64`.
///
/// ```rust
/// use cpp_parser::features::{Feature, LanguageConfig, Standard, Vendor};
///
/// // `-std=gnu++23`: C++23 **and** the GNU extensions.
/// let config = LanguageConfig::new()
///     .with_standard(Standard::Cpp23)
///     .with_vendor(Vendor::Gnu);
///
/// assert!(config.support(Feature::DeducingThis));        // the standard
/// assert!(config.support(Feature::StatementExpression)); // the extension — both, from one config
///
/// // `-std=c++23`: the standard without the extensions.
/// let strict = LanguageConfig::new().with_standard(Standard::Cpp23);
/// assert!(strict.support(Feature::DeducingThis));
/// assert!(!strict.support(Feature::StatementExpression));
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct LanguageConfig {
    bits: u64,
}

impl LanguageConfig {
    /// **Nothing enabled.** A config that supports nothing is the honest starting point: a caller that forgot to
    /// configure gets a conservative parse rather than one that accepts everything.
    pub const fn new() -> Self {
        Self { bits: 0 }
    }

    /// `-std=c++17`, no vendor extensions — the most common C++ build a reader meets.
    pub const fn cpp17() -> Self {
        Self::new().with_standard(Standard::Cpp17)
    }

    /// `-std=gnu++17`, the GCC default mode for a long time.
    pub const fn gnu_cpp17() -> Self {
        Self::new()
            .with_standard(Standard::Cpp17)
            .with_vendor(Vendor::Gnu)
    }

    /// `-std=c11`, no vendor extensions.
    pub const fn c11() -> Self {
        Self::new().with_standard(Standard::C11)
    }

    /// `-std=gnu89`, which several system compilers still default to.
    /// **C, with nothing from C++** — the language a `.c` file is, and every C standard at once.
    ///
    /// Used where the *extension* is the only thing that says what the file is: no compile database entry, no
    /// `[compile] standard`, no `-std=`. Every compiler decides that way — `gcc x.c` is C and `g++ x.c` is C++, and
    /// the extension is what decides — so a reader who opens a `.c` file in a project that has stated nothing must
    /// get C's rules rather than a permissive blur of both.
    ///
    /// Every C standard and the GNU extensions, because a `.c` file written for C23 and one written for C89 are both
    /// `.c`; what this is asked is the *family*, and `family()` answers `C` for any of them.
    pub const fn c() -> Self {
        Self::new()
            .with_standard(Standard::C89)
            .with_standard(Standard::C99)
            .with_standard(Standard::C11)
            .with_standard(Standard::C17)
            .with_standard(Standard::C23)
            .with_vendor(Vendor::Gnu)
    }

    pub const fn gnu_c89() -> Self {
        Self::new()
            .with_standard(Standard::C89)
            .with_vendor(Vendor::Gnu)
    }

    pub const fn with_standard(self, standard: Standard) -> Self {
        Self {
            bits: self.bits | standard.bit(),
        }
    }

    /// **Every vendor's spellings**, which is what a parse does when it has not been told which compiler built the file:
    /// `__typeof__`, `__restrict`, `__int128`, `__declspec` are all recognised, and no standard is implied. Narrowing
    /// this is what `with_language` is for.
    pub const fn with_all_vendors(self) -> Self {
        Self {
            bits: self.bits | Vendor::Gnu.bit() | Vendor::Msvc.bit() | Vendor::Clang.bit(),
        }
    }

    pub const fn with_vendor(self, vendor: Vendor) -> Self {
        Self {
            bits: self.bits | vendor.bit(),
        }
    }

    /// The raw mask, for a caller that wants to store it (a session's config, a cache key).
    pub const fn bits(self) -> u64 {
        self.bits
    }

    pub const fn from_bits(bits: u64) -> Self {
        Self { bits }
    }

    /// **Which family this configuration is for**, when it says one: `Some(C)` for a C-only config, `Some(Cpp)` for a
    /// C++-only one, and `None` when both are enabled — which is what `permissive` is, and what a parse must answer when
    /// nobody told it what it is reading. A rule that only makes sense for one family asks this, never "is C++23 enabled".
    pub fn family(self) -> Option<Family> {
        let c = self
            .standards()
            .any(|standard| standard.family() == Family::C);
        let cpp = self
            .standards()
            .any(|standard| standard.family() == Family::Cpp);
        match (c, cpp) {
            (true, false) => Some(Family::C),
            (false, true) => Some(Family::Cpp),
            _ => None,
        }
    }

    /// The standards this config enables — the set bits of the standards' block.
    pub fn standards(self) -> impl Iterator<Item = Standard> {
        STANDARDS
            .into_iter()
            .filter(move |standard| self.bits & standard.bit() != 0)
    }

    pub fn vendors(self) -> impl Iterator<Item = Vendor> {
        [Vendor::Gnu, Vendor::Msvc, Vendor::Clang]
            .into_iter()
            .filter(move |vendor| self.bits & vendor.bit() != 0)
    }

    /// **May a parse accept this feature here?** — the one question the grammar asks. One shift, one AND.
    pub fn support(self, feature: Feature) -> bool {
        self.supports(feature.requirement())
    }

    /// Every feature must be supported — a branch that needs two things.
    pub fn support_all(self, features: &[Feature]) -> bool {
        features.iter().all(|feature| self.support(*feature))
    }

    /// Any of these — a branch with alternatives.
    pub fn support_any(self, features: &[Feature]) -> bool {
        features.iter().any(|feature| self.support(*feature))
    }

    fn supports(self, requirement: Requirement) -> bool {
        match requirement {
            Requirement::Standard(needed) => self.bits & needed.covered_by_mask() != 0,
            Requirement::Vendor(vendor) => self.bits & vendor.bit() != 0,
            Requirement::AnyOf(alternatives) => alternatives
                .iter()
                .any(|alternative| self.supports(*alternative)),
        }
    }

    /// The configuration as a compiler's own name for it — `gnu++23`, `c++20`, `gnu89`, `c89`.
    pub fn describe(self) -> String {
        // The suffix a compiler's flag writes: `++23` for C++, `89` for C. GNU prefixes the whole thing (`gnu++23`,
        // `gnu89`), which is why the family has to be decided before the vendor is.
        let suffix = match self.standards().max_by_key(|standard| standard.rank()) {
            Some(Standard::C89) => "89",
            Some(Standard::C99) => "99",
            Some(Standard::C11) => "11",
            Some(Standard::C17) => "17",
            Some(Standard::C23) => "23",
            Some(Standard::Cpp11) => "++11",
            Some(Standard::Cpp14) => "++14",
            Some(Standard::Cpp17) => "++17",
            Some(Standard::Cpp20) => "++20",
            Some(Standard::Cpp23) => "++23",
            None => return "no-standard".to_string(),
        };
        let mut described = if self.bits & Vendor::Gnu.bit() != 0 {
            format!("gnu{suffix}")
        } else {
            format!("c{suffix}")
        };
        for vendor in [Vendor::Msvc, Vendor::Clang] {
            if self.bits & vendor.bit() != 0 {
                described.push('+');
                described.push_str(&vendor.to_string());
            }
        }
        described
    }
}

impl fmt::Debug for LanguageConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The flag rather than the number: a log line says `gnu++23`, and the mask is one `bits()` away when it is what
        // the reader needs.
        write!(
            f,
            "LanguageConfig({}, {:#014b})",
            self.describe(),
            self.bits
        )
    }
}

impl fmt::Display for LanguageConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}

impl LanguageConfig {
    /// **`-std=gnu++23`, or the spelling without the flag** — what `compile_commands.json` writes and what a caller
    /// pastes from a build log.
    ///
    /// ```rust
    /// use cpp_parser::features::{Feature, LanguageConfig, Standard, Vendor};
    ///
    /// let config = LanguageConfig::from_flag("-std=gnu++23").expect("a flag the compiler accepts");
    /// assert_eq!(config.standards().next(), Some(Standard::Cpp23));
    /// assert_eq!(config.vendors().next(), Some(Vendor::Gnu));
    /// assert!(config.support(Feature::DeducingThis));
    /// assert!(config.support(Feature::StatementExpression));
    ///
    /// assert!(LanguageConfig::from_flag("c11").is_some());
    /// // The alternate spellings compilers also print are carried; the ones this table does not model are `None`.
    /// assert!(LanguageConfig::from_flag("-std=c++1z").is_some());
    /// assert!(LanguageConfig::from_flag("-std=c++03").is_none(), "not a spelling this table carries");
    /// ```
    pub fn from_flag(flag: &str) -> Option<Self> {
        let spelling = flag
            .trim()
            .strip_prefix("--std=")
            .or_else(|| flag.trim().strip_prefix("-std="))
            .unwrap_or_else(|| flag.trim());
        Self::from_standard_spelling(spelling)
    }

    /// The spelling after `-std=`, without the flag. Accepts the canonical names and the ones compilers also print:
    /// `gnu++23`, `c++20`, `gnu89`, `c11`, `c17`, `c23`, `iso9899:1999`, `c++latest`.
    pub fn from_standard_spelling(spelling: &str) -> Option<Self> {
        let spelling = spelling.trim().to_ascii_lowercase();
        let (vendor, rest) = match spelling.strip_prefix("gnu") {
            Some(rest) => (Some(Vendor::Gnu), rest),
            None => match spelling.strip_prefix("clang") {
                Some(rest) => (Some(Vendor::Clang), rest),
                None => (None, spelling.as_str()),
            },
        };
        // `gnu++23` keeps its `++`; `gnu89` is GCC's spelling of **C89** and needs the `c` put back; a spelling with no
        // vendor prefix (`c11`, `c++20`) is already the canonical one.
        let rest = match rest.strip_prefix("++") {
            Some(version) => format!("c++{version}"),
            None if vendor.is_some() => format!("c{rest}"),
            None => rest.to_string(),
        };
        let standard = match rest.as_str() {
            "c++11" | "c++0x" => Standard::Cpp11,
            "c++14" | "c++1y" => Standard::Cpp14,
            "c++17" | "c++1z" => Standard::Cpp17,
            "c++20" | "c++2a" => Standard::Cpp20,
            "c++23" | "c++2b" | "c++latest" => Standard::Cpp23,
            "c89" | "c90" | "iso9899:1990" => Standard::C89,
            "c99" | "c9x" | "iso9899:1999" => Standard::C99,
            "c11" | "c1x" | "iso9899:2011" => Standard::C11,
            "c17" | "c18" | "iso9899:2017" => Standard::C17,
            "c23" | "c2x" | "iso9899:2024" => Standard::C23,
            _ => return None,
        };
        let mut config = Self::new().with_standard(standard);
        if let Some(vendor) = vendor {
            config = config.with_vendor(vendor);
        }
        Some(config)
    }

    /// **The last `-std=` in a command line**, which is the one a compiler uses when a file is built with several.
    ///
    /// `compile_commands.json` holds a real command line, and a project that passes `-std=c++17` before its own flags and
    /// something else after it means the later one. Reading the first would be wrong in exactly the case this exists for.
    pub fn from_command_line<S: AsRef<str>>(
        arguments: impl IntoIterator<Item = S>,
    ) -> Option<Self> {
        let mut found: Option<Self> = None;
        let mut iterator = arguments.into_iter();
        while let Some(argument) = iterator.next() {
            let argument = argument.as_ref();
            if let Some(config) = Self::from_flag(argument) {
                found = Some(config);
            } else if argument == "-std" || argument == "--std" {
                // A separated form: `-std gnu++23`, which is not how a compiler wants it but is how some build systems
                // write it.
                if let Some(config) = iterator
                    .next()
                    .and_then(|next| Self::from_flag(next.as_ref()))
                {
                    found = Some(config);
                }
            }
        }
        found
    }

    /// **The configuration an old-style [`CppLanguageLevel`](crate::kind::CppLanguageLevel) describes**, for the callers
    /// that still configure a parse with one.
    ///
    /// `GnuCpp` and `MsvcCpp` carry no version — that was the conflation this module exists to end — so they map to a
    /// C++17 baseline with the vendor, which is the mode those two spellings were used for.
    pub fn from_language_level(level: crate::kind::CppLanguageLevel) -> Self {
        use crate::kind::CppLanguageLevel;
        let standard = match level {
            CppLanguageLevel::Cpp11 => Standard::Cpp11,
            CppLanguageLevel::Cpp14 => Standard::Cpp14,
            CppLanguageLevel::Cpp17 | CppLanguageLevel::GnuCpp | CppLanguageLevel::MsvcCpp => {
                Standard::Cpp17
            }
            CppLanguageLevel::Cpp20 => Standard::Cpp20,
            CppLanguageLevel::Cpp23 => Standard::Cpp23,
        };
        let vendor = match level {
            CppLanguageLevel::GnuCpp => Some(Vendor::Gnu),
            CppLanguageLevel::MsvcCpp => Some(Vendor::Msvc),
            _ => None,
        };
        let config = Self::new().with_standard(standard);
        match vendor {
            Some(vendor) => config.with_vendor(vendor),
            None => config,
        }
    }

    /// **The old-style level this config is closest to**, for the two call sites that still take one.
    pub fn to_language_level(self) -> crate::kind::CppLanguageLevel {
        use crate::kind::CppLanguageLevel;
        match self.standards().max_by_key(|standard| standard.rank()) {
            Some(Standard::Cpp11) => CppLanguageLevel::Cpp11,
            Some(Standard::Cpp14) => CppLanguageLevel::Cpp14,
            Some(Standard::Cpp20) => CppLanguageLevel::Cpp20,
            Some(Standard::Cpp23) => CppLanguageLevel::Cpp23,
            Some(Standard::Cpp17) | None => CppLanguageLevel::Cpp17,
            // A C config has no level; C++17 is the conservative reading, and the vendors are what a C file needs.
            Some(_) => CppLanguageLevel::Cpp17,
        }
    }

    /// **Everything**: every standard, every vendor. What a parse that has not been told what it is reading accepts, and
    /// the honest default for a parser whose callers have not migrated yet — a strict config is a narrowing, not a
    /// requirement.
    pub const fn permissive() -> Self {
        Self {
            bits: (1u64 << 13) - 1,
        }
    }
}
