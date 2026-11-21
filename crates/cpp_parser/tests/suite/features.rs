//! **`config.support(feature)`, one case per combination a real build has.**
//!
//! The point of these tests is the one the enum could not express: a project is a **combination** of a standard and the
//! vendor whose compiler built it, and a parser that models the standard alone refuses the project's own headers.

use cpp_parser::features::{Feature, LanguageConfig, STANDARDS, Standard, Vendor};

/// **`-std=gnu++23`: the standard and the extensions at once.** The case the single enum could not state.
#[test]
fn a_gnu_cpp23_build_has_the_standard_and_the_gnu_extensions() {
    let config = LanguageConfig::new()
        .with_standard(Standard::Cpp23)
        .with_vendor(Vendor::Gnu);

    // The standard's own features.
    assert!(config.support(Feature::DeducingThis));
    assert!(config.support(Feature::IfConsteval));
    assert!(config.support(Feature::MultidimensionalSubscript));
    assert!(config.support(Feature::StaticOperator));

    // …and the extensions, from the same config.
    assert!(config.support(Feature::StatementExpression));
    assert!(config.support(Feature::ElvisOperator));
    assert!(config.support(Feature::NestedFunction));
    assert!(config.support(Feature::Typeof));

    // Empty braces and the like are not GNU's.
    assert!(
        !config.support(Feature::StructuredExceptionHandling),
        "that is MSVC's"
    );
    assert!(!config.support(Feature::BlockLiteral), "that is Clang's");
}

/// **`-std=c++23`: the standard without any extension.**
#[test]
fn a_strict_cpp23_build_has_no_extensions() {
    let config = LanguageConfig::new().with_standard(Standard::Cpp23);
    assert!(config.support(Feature::DeducingThis));
    assert!(!config.support(Feature::StatementExpression));
    assert!(!config.support(Feature::ElvisOperator));
    assert!(!config.support(Feature::StructuredExceptionHandling));
}

/// **A later standard covers the earlier ones of its family** — `-std=c++20` accepts C++17's syntax.
#[test]
fn a_later_standard_covers_the_earlier_ones() {
    let config = LanguageConfig::new().with_standard(Standard::Cpp20);
    assert!(config.support(Feature::StructuredBindings), "C++17");
    assert!(config.support(Feature::GenericLambda), "C++14");
    assert!(config.support(Feature::Lambda), "C++11");
    assert!(
        !config.support(Feature::DeducingThis),
        "C++23, which is later"
    );
    assert!(
        !config.support(Feature::StatementExpression),
        "GNU, which is not the standard"
    );
}

/// **The two families do not cover each other**: C++23 says nothing about C's `_Generic`.
#[test]
fn the_families_are_separate() {
    let cpp = LanguageConfig::new().with_standard(Standard::Cpp23);
    assert!(!cpp.support(Feature::GenericSelection));
    assert!(!cpp.support(Feature::Atomic));
    assert!(!cpp.support(Feature::VariableLengthArrays));

    let c = LanguageConfig::new().with_standard(Standard::C11);
    assert!(c.support(Feature::GenericSelection));
    assert!(c.support(Feature::Atomic));
    assert!(
        c.support(Feature::VariableLengthArrays),
        "C99, covered by C11"
    );
    assert!(!c.support(Feature::Lambda));
}

/// **`-std=gnu89`** — the C89 report a system compiler still defaults to: statement expressions yes, VLAs no.
#[test]
fn gnu_c89_has_the_extensions_and_not_c99() {
    let config = LanguageConfig::new()
        .with_standard(Standard::C89)
        .with_vendor(Vendor::Gnu);
    assert!(config.support(Feature::StatementExpression));
    assert!(config.support(Feature::Typeof));
    assert!(
        config.support(Feature::VariableLengthArrays),
        "GNU accepts them in every mode"
    );
    assert!(!config.support(Feature::GenericSelection), "C11");
    assert!(!config.support(Feature::Atomic), "C11");
}

/// **`-std=c11`** — no GNU: VLAs are C99 and covered, `typeof` is C23 and not.
#[test]
fn strict_c11_has_c99_and_not_the_extensions() {
    let config = LanguageConfig::new().with_standard(Standard::C11);
    assert!(config.support(Feature::VariableLengthArrays));
    assert!(config.support(Feature::GenericSelection));
    assert!(!config.support(Feature::StatementExpression));
    assert!(!config.support(Feature::Typeof), "C23 or GNU");
}

/// **A feature with two ways to arrive** — designated initializers are C99 and C++20, and neither the other's.
#[test]
fn a_feature_with_two_requirements_accepts_either() {
    assert!(
        LanguageConfig::new()
            .with_standard(Standard::C99)
            .support(Feature::DesignatedInitializers)
    );
    assert!(
        LanguageConfig::new()
            .with_standard(Standard::Cpp20)
            .support(Feature::DesignatedInitializers)
    );
    // C11 **covers** C99, so designated initializers are there — that is the same rule that makes C++20 cover C++17.
    assert!(
        LanguageConfig::new()
            .with_standard(Standard::C11)
            .support(Feature::DesignatedInitializers)
    );
    assert!(
        !LanguageConfig::new()
            .with_standard(Standard::C89)
            .support(Feature::DesignatedInitializers),
        "C89 is not C99"
    );
    assert!(
        !LanguageConfig::new()
            .with_standard(Standard::Cpp17)
            .support(Feature::DesignatedInitializers),
        "C++17 is not C++20"
    );
}

/// **`typeof` is C23 *and* GNU**, which is why it was visible as a gap: a `gnu++` config has it and a strict `c++` one
/// does not.
#[test]
fn typeof_arrives_by_either_route() {
    let gnu = LanguageConfig::new()
        .with_standard(Standard::Cpp17)
        .with_vendor(Vendor::Gnu);
    assert!(gnu.support(Feature::Typeof));

    let c23 = LanguageConfig::new().with_standard(Standard::C23);
    assert!(c23.support(Feature::Typeof));

    let strict_cpp = LanguageConfig::new().with_standard(Standard::Cpp23);
    // `typeof` is a GNU/MSVC extension: C23 has it, and no C++ standard does.
    assert!(!strict_cpp.support(Feature::Typeof));
}

/// **An unconfigured parse accepts nothing**, which is the conservative direction: a caller that forgot to configure gets
/// a parse that refuses rather than one that assumes.
#[test]
fn nothing_is_supported_by_default() {
    let config = LanguageConfig::new();
    assert!(!config.support(Feature::Lambda));
    assert!(!config.support(Feature::StatementExpression));
    assert!(!config.support(Feature::GenericSelection));
}

/// **`support_all` and `support_any`**, the two call shapes a grammar branch needs.
#[test]
fn all_and_any() {
    let config = LanguageConfig::new()
        .with_standard(Standard::Cpp23)
        .with_vendor(Vendor::Gnu);
    assert!(config.support_all(&[Feature::Lambda, Feature::Concepts, Feature::DeducingThis]));
    assert!(config.support_all(&[Feature::DeducingThis, Feature::StatementExpression]));
    assert!(!config.support_all(&[Feature::DeducingThis, Feature::BlockLiteral]));
    assert!(config.support_any(&[Feature::BlockLiteral, Feature::StatementExpression]));
    assert!(!config.support_any(&[Feature::BlockLiteral, Feature::Interface]));
}

/// **The config describes itself the way a compiler's flag does** — which is how a log line says what a parse was given.
#[test]
fn the_description_reads_like_a_compiler_flag() {
    assert_eq!(
        LanguageConfig::new()
            .with_standard(Standard::Cpp23)
            .with_vendor(Vendor::Gnu)
            .describe(),
        "gnu++23"
    );
    assert_eq!(
        LanguageConfig::new()
            .with_standard(Standard::Cpp20)
            .describe(),
        "c++20"
    );
    assert_eq!(
        LanguageConfig::new()
            .with_standard(Standard::C89)
            .describe(),
        "c89"
    );
    assert_eq!(
        LanguageConfig::new()
            .with_standard(Standard::C17)
            .with_vendor(Vendor::Msvc)
            .describe(),
        "c17+msvc"
    );
    assert_eq!(LanguageConfig::new().describe(), "no-standard");
}

/// **Every feature's requirement is reachable**: a config built from the requirement itself supports the feature. This is
/// the invariant that keeps a new feature's requirement from being written in a way nothing can satisfy.
#[test]
fn every_feature_is_supported_by_the_configuration_its_requirement_describes() {
    use cpp_parser::features::Requirement;
    let features = [
        Feature::Auto,
        Feature::Lambda,
        Feature::RvalueReferences,
        Feature::VariadicTemplates,
        Feature::AliasTemplates,
        Feature::GenericLambda,
        Feature::VariableTemplates,
        Feature::BinaryLiterals,
        Feature::DigitSeparators,
        Feature::StructuredBindings,
        Feature::IfConstexpr,
        Feature::FoldExpressions,
        Feature::InlineVariables,
        Feature::ClassTemplateArgumentDeduction,
        Feature::Concepts,
        Feature::Modules,
        Feature::Coroutines,
        Feature::DesignatedInitializers,
        Feature::ThreeWayComparison,
        Feature::UsingEnum,
        Feature::DeducingThis,
        Feature::IfConsteval,
        Feature::MultidimensionalSubscript,
        Feature::StaticOperator,
        Feature::VariableLengthArrays,
        Feature::GenericSelection,
        Feature::Atomic,
        Feature::StaticAssertC,
        Feature::AttributesC,
        Feature::Nullptr,
        Feature::Constexpr,
        Feature::StatementExpression,
        Feature::ElvisOperator,
        Feature::AsmBlock,
        Feature::NestedFunction,
        Feature::Typeof,
        Feature::StructuredExceptionHandling,
        Feature::Interface,
        Feature::BlockLiteral,
    ];

    fn from(requirement: Requirement, config: LanguageConfig) -> LanguageConfig {
        match requirement {
            Requirement::Standard(standard) => config.with_standard(standard),
            Requirement::Vendor(vendor) => config.with_vendor(vendor),
            Requirement::AnyOf(alternatives) => from(alternatives[0], config),
        }
    }

    for feature in features {
        let requirement = feature.requirement();
        let config = from(requirement, LanguageConfig::new());
        assert!(
            config.support(feature),
            "`{}` says it needs {requirement:?}, and a config of exactly that does not support it ({})",
            feature.name(),
            config.describe()
        );
    }
}

/// **The config is one `u64`** — the point of the bitmask, asserted rather than assumed: it is `Copy`, it is eight bytes,
/// and a session can store it in a field or a cache key without an allocation.
#[test]
fn the_config_is_one_word() {
    assert_eq!(std::mem::size_of::<LanguageConfig>(), 8);
    assert_eq!(std::mem::align_of::<LanguageConfig>(), 8);

    fn is_copy<T: Copy>(_: &T) {}
    let config = LanguageConfig::gnu_cpp17();
    is_copy(&config);
    let copied = config; // a copy, not a move: `config` is usable below
    assert_eq!(copied, config);
}

/// **It is `const`-constructible**, which is what makes it usable in a `static` default and in a `const` table of driver
/// presets without lazy initialisation.
#[test]
fn the_config_is_const_constructible() {
    const GNU_CPP23: LanguageConfig = LanguageConfig::new()
        .with_standard(Standard::Cpp23)
        .with_vendor(Vendor::Gnu);
    const C11: LanguageConfig = LanguageConfig::c11();

    assert!(GNU_CPP23.support(Feature::DeducingThis));
    assert!(GNU_CPP23.support(Feature::StatementExpression));
    assert!(C11.support(Feature::GenericSelection));
    assert!(!C11.support(Feature::StatementExpression));
}

/// **The mask round-trips** — `bits` out, `from_bits` in — which is what a session's stored configuration relies on.
#[test]
fn the_mask_round_trips() {
    let config = LanguageConfig::new()
        .with_standard(Standard::Cpp20)
        .with_vendor(Vendor::Clang);
    let restored = LanguageConfig::from_bits(config.bits());
    assert_eq!(restored, config);
    assert_eq!(restored.describe(), "c++20+clang");

    // One bit per standard, one per vendor: no collisions.
    let all = STANDARDS
        .iter()
        .fold(LanguageConfig::new(), |config, standard| {
            config.with_standard(*standard)
        });
    assert_eq!(
        all.standards().count(),
        STANDARDS.len(),
        "every standard has its own bit"
    );
}

/// **C89's own feature** — the baseline whose rules are the ones a later standard relaxes.
#[test]
fn c89_is_the_baseline() {
    let c89 = LanguageConfig::new().with_standard(Standard::C89);
    assert!(c89.support(Feature::C89Declarations));
    assert!(!c89.support(Feature::LineComments), "`//` is C99");
    assert!(!c89.support(Feature::LongLong), "C99");
    assert!(!c89.support(Feature::VariableLengthArrays), "C99");
    assert!(!c89.support(Feature::CompoundLiterals), "C99");
}

/// **C99's additions**, and GNU's spelling of `restrict` reaching C++ too.
#[test]
fn c99_adds_what_c89_lacks() {
    let c99 = LanguageConfig::new().with_standard(Standard::C99);
    for feature in [
        Feature::LineComments,
        Feature::LongLong,
        Feature::InlineC,
        Feature::BoolC,
        Feature::FlexibleArrayMembers,
        Feature::VariadicMacros,
        Feature::CompoundLiterals,
        Feature::VariableLengthArrays,
        Feature::Restrict,
    ] {
        assert!(c99.support(feature), "{} is C99", feature.name());
    }

    // GNU's spelling of `restrict` is accepted in C++ mode too, which is why it is its own feature.
    let gnu_cpp = LanguageConfig::new()
        .with_standard(Standard::Cpp17)
        .with_vendor(Vendor::Gnu);
    assert!(gnu_cpp.support(Feature::RestrictGnu));
    // …and `restrict` itself arrives by either route — C99's own spelling, or GNU's — so a **gnu** C++ config has it and
    // a strict one does not.
    assert!(
        gnu_cpp.support(Feature::Restrict),
        "GNU's spelling is one of its two routes"
    );
    assert!(
        !LanguageConfig::new()
            .with_standard(Standard::Cpp17)
            .support(Feature::Restrict)
    );
}

/// **C11's additions**, and C17's honest emptiness.
#[test]
fn c11_and_c17() {
    let c11 = LanguageConfig::c11();
    for feature in [
        Feature::GenericSelection,
        Feature::Atomic,
        Feature::StaticAssertC,
        Feature::Alignas,
        Feature::Noreturn,
        Feature::AnonymousStructs,
        Feature::ThreadLocalC,
    ] {
        assert!(c11.support(feature), "{} is C11", feature.name());
    }

    let c17 = LanguageConfig::new().with_standard(Standard::C17);
    assert!(c17.support(Feature::C17NoNewSyntax));
    assert!(c17.support(Feature::GenericSelection), "C17 covers C11");
    assert!(!c17.support(Feature::Nullptr), "C23");
}

/// **C23's additions** — the release that made C look like C++ in the places a parser cares about.
#[test]
fn c23_adds_the_modern_spellings() {
    let c23 = LanguageConfig::new().with_standard(Standard::C23);
    for feature in [
        Feature::AttributesC,
        Feature::Nullptr,
        Feature::Constexpr,
        Feature::BitInt,
        Feature::AutoC,
        Feature::EnumeratorsWithAFixedType,
        Feature::EmptyInitializer,
        Feature::Elifdef,
        Feature::Typeof,
    ] {
        assert!(c23.support(feature), "{} is C23", feature.name());
    }

    // …and a C++23 config does not get C23's `typeof`, while a `gnu++23` one does.
    assert!(
        !LanguageConfig::new()
            .with_standard(Standard::Cpp23)
            .support(Feature::Typeof)
    );
    assert!(
        LanguageConfig::new()
            .with_standard(Standard::Cpp23)
            .with_vendor(Vendor::Gnu)
            .support(Feature::Typeof)
    );
}

/// **C and C++ share the features they share**, and the requirement says so once: `//` comments are C99 and C++11.
#[test]
fn a_feature_shared_by_the_families() {
    for config in [
        LanguageConfig::new().with_standard(Standard::C99),
        LanguageConfig::new().with_standard(Standard::Cpp11),
    ] {
        assert!(
            config.support(Feature::LineComments),
            "{}",
            config.describe()
        );
    }
    assert!(
        !LanguageConfig::new()
            .with_standard(Standard::C89)
            .support(Feature::LineComments)
    );
}

/// **A `-std=` flag becomes a config**, which is what `compile_commands.json` hands over.
#[test]
fn a_standard_flag_becomes_a_configuration() {
    let config = LanguageConfig::from_flag("-std=gnu++23").expect("a flag a compiler accepts");
    assert_eq!(
        config.standards().collect::<Vec<_>>(),
        vec![Standard::Cpp23]
    );
    assert_eq!(config.vendors().collect::<Vec<_>>(), vec![Vendor::Gnu]);
    assert!(config.support(Feature::DeducingThis));
    assert!(config.support(Feature::StatementExpression));

    let strict = LanguageConfig::from_flag("c++20").expect("the spelling without the flag");
    assert_eq!(
        strict.standards().collect::<Vec<_>>(),
        vec![Standard::Cpp20]
    );
    assert!(strict.vendors().next().is_none());

    let c = LanguageConfig::from_flag("-std=gnu89").expect("the C89 report");
    assert_eq!(c.standards().collect::<Vec<_>>(), vec![Standard::C89]);
    assert!(c.support(Feature::StatementExpression));
    assert!(!c.support(Feature::LongLong));

    // The spellings compilers also print, and the ones this table does not carry.
    for (spelling, standard) in [
        ("-std=c++1z", Standard::Cpp17),
        ("-std=gnu++2a", Standard::Cpp20),
        ("-std=c++latest", Standard::Cpp23),
        ("-std=iso9899:1999", Standard::C99),
        ("--std=c11", Standard::C11),
    ] {
        let config = LanguageConfig::from_flag(spelling).unwrap_or_else(|| panic!("{spelling}"));
        assert_eq!(
            config.standards().collect::<Vec<_>>(),
            vec![standard],
            "{spelling}"
        );
    }
    assert!(
        LanguageConfig::from_flag("-std=c++03").is_none(),
        "not a spelling we model"
    );
    assert!(LanguageConfig::from_flag("-std=").is_none());
}

/// **The last `-std=` on a command line wins**, because that is what a compiler does with a repeated flag.
#[test]
fn the_last_standard_flag_wins() {
    let command = [
        "cl.exe",
        "/c",
        "-std=c++17",
        "-Iinclude",
        "-std=gnu++20",
        "main.cpp",
    ];
    let config = LanguageConfig::from_command_line(command).expect("a std flag is there");
    assert_eq!(
        config.standards().collect::<Vec<_>>(),
        vec![Standard::Cpp20]
    );
    assert_eq!(config.vendors().collect::<Vec<_>>(), vec![Vendor::Gnu]);

    assert!(LanguageConfig::from_command_line(["cl.exe", "-c", "main.cpp"]).is_none());
}

/// **The old enum converts both ways**, so the callers that still take one keep working during the migration.
#[test]
fn the_old_level_round_trips() {
    use cpp_parser::CppLanguageLevel;

    let gnu = LanguageConfig::from_language_level(CppLanguageLevel::GnuCpp);
    assert_eq!(gnu.standards().collect::<Vec<_>>(), vec![Standard::Cpp17]);
    assert_eq!(gnu.vendors().collect::<Vec<_>>(), vec![Vendor::Gnu]);

    let msvc = LanguageConfig::from_language_level(CppLanguageLevel::MsvcCpp);
    assert_eq!(msvc.vendors().collect::<Vec<_>>(), vec![Vendor::Msvc]);

    for level in [
        CppLanguageLevel::Cpp11,
        CppLanguageLevel::Cpp14,
        CppLanguageLevel::Cpp17,
        CppLanguageLevel::Cpp20,
        CppLanguageLevel::Cpp23,
    ] {
        let config = LanguageConfig::from_language_level(level);
        assert_eq!(
            std::mem::discriminant(&config.to_language_level()),
            std::mem::discriminant(&level),
            "{level:?} did not round-trip"
        );
    }
}

/// **`permissive` accepts everything** — the default for a parse whose caller has not migrated, and the shape that keeps
/// the migration from being a behaviour change.
#[test]
fn permissive_accepts_everything() {
    let config = LanguageConfig::permissive();
    for feature in [
        Feature::C89Declarations,
        Feature::DeducingThis,
        Feature::Typeof,
        Feature::StatementExpression,
        Feature::StructuredExceptionHandling,
        Feature::BlockLiteral,
        Feature::GenericSelection,
        Feature::BitInt,
    ] {
        assert!(
            config.support(feature),
            "`{}` should be accepted by a permissive parse",
            feature.name()
        );
    }
    // …and it is the union of the bits we model, not "all one bits forever".
    assert_eq!(config.bits() & !((1u64 << 13) - 1), 0);
}

/// **The lexer reads the feature set, not the old level** — a C configuration refuses a raw string literal whatever the
/// level says, which is the conflation the migration removes.
#[test]
fn the_lexer_gates_read_the_feature_set() {
    use cpp_parser::features::{Feature, LanguageConfig, Standard};
    use cpp_parser::{CppLanguageLevel, LexerConfig};

    // A C config: no raw strings, because they are C++11 and C has never had them.
    let c = LexerConfig::new(CppLanguageLevel::Cpp23)
        .with_language(LanguageConfig::new().with_standard(Standard::C11));
    assert!(!c.supports_raw_strings(), "`R\"(…)\"` is not C");
    assert!(!c.supports_digit_separators(), "`1'000'000` is C++14");

    // The same level with its derived config does have both, which is the old behaviour kept.
    let cpp = LexerConfig::new(CppLanguageLevel::Cpp23);
    assert!(cpp.supports_raw_strings());
    assert!(cpp.supports_digit_separators());

    // C++14 is the boundary for digit separators, and C++11 is not.
    let cpp11 = LexerConfig::new(CppLanguageLevel::Cpp11);
    assert!(cpp11.supports_raw_strings());
    assert!(!cpp11.supports_digit_separators());

    // …and a config from a flag behaves the same, which is what the migration is for.
    let from_flag = LexerConfig::new(CppLanguageLevel::Cpp23)
        .with_language(LanguageConfig::from_flag("-std=gnu++20").expect("a flag"));
    assert!(from_flag.supports_raw_strings());
    assert!(from_flag.supports_digit_separators());
    assert!(
        from_flag.language().support(Feature::StatementExpression),
        "gnu++20 has the GNU extensions"
    );
}

/// **`ParserConfig` carries the same language**, and a `Dialect` is a vendor written the old way.
#[test]
fn the_parser_carries_the_language_and_the_dialect_is_a_vendor() {
    use cpp_parser::features::{Feature, LanguageConfig, Standard, Vendor};
    use cpp_parser::{CppLanguageLevel, Dialect, ParserConfig};

    let config = ParserConfig::new(CppLanguageLevel::Cpp17, None)
        .with_language(LanguageConfig::from_flag("-std=gnu++23").expect("a flag"));
    assert_eq!(config.language().describe(), "gnu++23");
    assert!(config.language().support(Feature::DeducingThis));

    // The dialect adds its vendor on top of whatever the language already had — the two fields cannot disagree about
    // which compiler this is. The language is narrowed first, because a parse that has not been told which compiler built
    // the file accepts **every** vendor's spellings (that is what `LexerConfig::new` does, to keep the pre-migration
    // behaviour); narrowing is the caller's decision, and `with_dialect` is one way to make it.
    let strict = LanguageConfig::new().with_standard(Standard::Cpp17);

    let msvc = ParserConfig::new(CppLanguageLevel::Cpp17, None)
        .with_language(strict)
        .with_dialect(Dialect::Msvc);
    assert_eq!(
        msvc.language().vendors().collect::<Vec<_>>(),
        vec![Vendor::Msvc]
    );
    assert!(
        msvc.language()
            .support(Feature::StructuredExceptionHandling)
    );
    assert!(!msvc.language().support(Feature::StatementExpression));

    let gnu = ParserConfig::new(CppLanguageLevel::Cpp17, None)
        .with_language(strict)
        .with_dialect(Dialect::Gnu);
    assert!(gnu.language().support(Feature::StatementExpression));
    assert!(!gnu.language().support(Feature::StructuredExceptionHandling));

    // The standards survive the dialect: this is still C++17 plus a vendor, not "GNU means everything".
    assert_eq!(
        gnu.language().standards().collect::<Vec<_>>(),
        vec![Standard::Cpp17]
    );
}

/// **The same source, two configurations** — which is the whole point of the migration, stated as one test.
///
/// `typeof(1) n = 1;` is GNU's spelling of `decltype` and C23's keyword. A `gnu++20` parse accepts it; a strict `c++20`
/// parse does not, because `typeof` there is an ordinary identifier and the declaration reads as a call. Before the
/// migration the lexer mapped it unconditionally, so both parses accepted it — the conflation, visible in one file.
#[test]
fn the_same_source_parses_differently_under_two_configurations() {
    use cpp_parser::{CppLanguageLevel, CppParser, ParserConfig};

    let source = "void f() { typeof(1) n = 1; }";

    let gnu = ParserConfig::new(CppLanguageLevel::Cpp23, None)
        .with_language(LanguageConfig::from_flag("-std=gnu++20").expect("a flag"));
    assert_eq!(
        CppParser::parse(source, gnu).get_errors(),
        [],
        "a GNU parse reads `typeof` as the keyword it is"
    );

    let strict = ParserConfig::new(CppLanguageLevel::Cpp23, None)
        .with_language(LanguageConfig::from_flag("-std=c++20").expect("a flag"));
    assert!(
        !CppParser::parse(source, strict).get_errors().is_empty(),
        "a strict C++20 parse reads `typeof` as a name, and the declaration does not parse"
    );

    // C23 has it as a keyword too, so a strict C23 parse accepts it — the feature's second route.
    let c23 = ParserConfig::new(CppLanguageLevel::Cpp23, None)
        .with_language(LanguageConfig::from_flag("-std=c23").expect("a flag"));
    assert_eq!(
        CppParser::parse(source, c23).get_errors(),
        [],
        "`typeof` is C23 as well as GNU"
    );
}

/// **A lexical gate now reads the feature set**: `0b1010` is C++14's, and a C configuration is a question the old level
/// could never answer — `CppLanguageLevel` has no C values at all.
#[test]
fn binary_literals_are_a_feature_not_a_level() {
    use cpp_parser::{CppLanguageLevel, CppParser, ParserConfig};

    let source = "int n = 0b1010;";
    let with = |standard| {
        ParserConfig::new(CppLanguageLevel::Cpp14, None)
            .with_language(LanguageConfig::new().with_standard(standard))
    };

    assert_eq!(
        CppParser::parse(source, with(Standard::Cpp14)).get_errors(),
        [],
        "C++14 has binary literals"
    );
    assert_eq!(
        CppParser::parse(source, with(Standard::Cpp23)).get_errors(),
        [],
        "…and a later standard covers it"
    );
    assert!(
        !CppParser::parse(source, with(Standard::Cpp11))
            .get_errors()
            .is_empty(),
        "C++11 does not"
    );
    assert!(
        !CppParser::parse(source, with(Standard::C11))
            .get_errors()
            .is_empty(),
        "and C11 has never had them — the level could not say this, and the config can"
    );
}

/// **The keyword table reads the feature set** — a word that became a keyword is an identifier before the release that
/// made it one, which is the class of change the old level enum could not express.
///
/// Asserted at the **lexer**, because that is the layer this change is in: `name_to_kind` decides a token's kind from the
/// configuration, and the grammar above it has its own text-level checks that are a separate question. A token that says
/// `Identifier` is a token the grammar cannot treat as a keyword.
#[test]
fn the_keyword_table_reads_the_feature_set() {
    use cpp_parser::CppLexer;
    use cpp_parser::features::{Feature, LanguageConfig, Standard};
    use cpp_parser::{CppLanguageLevel, CppTokenKind, LexerConfig};

    let kind_of = |word: &str, standard: Standard| {
        let config = LexerConfig::new(CppLanguageLevel::Cpp23)
            .with_language(LanguageConfig::new().with_standard(standard));

        CppLexer::new(word, config, &mut Vec::new())
            .tokenize()
            .first()
            .map(|token| token.kind)
            .expect("a word is a token")
    };

    // `consteval` is C++20's.
    assert_eq!(
        kind_of("consteval", Standard::Cpp17),
        CppTokenKind::Identifier
    );
    assert_eq!(
        kind_of("consteval", Standard::Cpp20),
        CppTokenKind::ConstevalKeyword
    );
    assert_eq!(
        kind_of("consteval", Standard::Cpp23),
        CppTokenKind::ConstevalKeyword
    );

    // `nullptr` has two routes: C++11 and C23.
    assert_eq!(kind_of("nullptr", Standard::C11), CppTokenKind::Identifier);
    assert_eq!(
        kind_of("nullptr", Standard::C23),
        CppTokenKind::NullptrKeyword
    );
    assert_eq!(
        kind_of("nullptr", Standard::Cpp11),
        CppTokenKind::NullptrKeyword
    );

    // `static_assert` is C++11's *and* C11's, which is what a disjunction looks like in the table.
    assert_eq!(
        kind_of("static_assert", Standard::C89),
        CppTokenKind::Identifier
    );
    assert_eq!(
        kind_of("static_assert", Standard::C11),
        CppTokenKind::StaticAssertKeyword
    );
    assert_eq!(
        kind_of("static_assert", Standard::Cpp11),
        CppTokenKind::StaticAssertKeyword
    );

    // `alignas` is C++11's spelling; C11 writes `_Alignas`.
    assert_eq!(kind_of("alignas", Standard::C11), CppTokenKind::Identifier);
    assert_eq!(
        kind_of("alignas", Standard::Cpp11),
        CppTokenKind::AlignasKeyword
    );

    // `thread_local`, and the coroutine words from C++20.
    assert_eq!(
        kind_of("thread_local", Standard::C89),
        CppTokenKind::Identifier
    );
    assert_eq!(
        kind_of("thread_local", Standard::Cpp11),
        CppTokenKind::ThreadLocalKeyword
    );
    for word in ["co_await", "co_return", "co_yield"] {
        assert_eq!(
            kind_of(word, Standard::Cpp17),
            CppTokenKind::Identifier,
            "{word}"
        );
        assert_eq!(
            kind_of(word, Standard::Cpp20),
            match word {
                "co_await" => CppTokenKind::CoAwaitKeyword,
                "co_return" => CppTokenKind::CoReturnKeyword,
                _ => CppTokenKind::CoYieldKeyword,
            },
            "{word}"
        );
    }

    // `constexpr` is C++11's and C23's — the third feature with two routes.
    assert_eq!(
        kind_of("constexpr", Standard::C89),
        CppTokenKind::Identifier
    );
    assert_eq!(
        kind_of("constexpr", Standard::C11),
        CppTokenKind::Identifier
    );
    assert_eq!(
        kind_of("constexpr", Standard::Cpp11),
        CppTokenKind::ConstexprKeyword
    );
    assert_eq!(
        kind_of("constexpr", Standard::C23),
        CppTokenKind::ConstexprKeyword
    );

    // `constinit` is C++20's.
    assert_eq!(
        kind_of("constinit", Standard::Cpp17),
        CppTokenKind::Identifier
    );
    assert_eq!(
        kind_of("constinit", Standard::Cpp20),
        CppTokenKind::ConstinitKeyword
    );

    // …and a `gnu++17` config is still C++17 for these, because GNU is a vendor and `consteval` is a standard's word.
    let gnu17 = LexerConfig::new(CppLanguageLevel::Cpp23)
        .with_language(LanguageConfig::from_flag("-std=gnu++17").expect("a flag"));
    assert_eq!(
        CppLexer::new("consteval", gnu17, &mut Vec::new())
            .tokenize()
            .first()
            .map(|token| token.kind),
        Some(CppTokenKind::Identifier),
        "a vendor does not back-port a standard's keyword"
    );
    let _ = Feature::Consteval;
}
