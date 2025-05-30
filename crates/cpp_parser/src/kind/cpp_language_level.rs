use core::fmt;

/// C++ language standard level
/// Defines the supported C++ standard versions, used to control the behavior of the parser
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CppLanguageLevel {
    /// C++11 Standard (ISO/IEC 14882:2011)
    /// Introduced auto, lambda, rvalue references, smart pointers, and other modern features
    Cpp11,
    
    /// C++14 Standard (ISO/IEC 14882:2014)
    /// Refinement of C++11, added generic lambdas, variable templates, etc.
    Cpp14,
    
    /// C++17 Standard (ISO/IEC 14882:2017)
    /// Introduced structured bindings, if constexpr, class template argument deduction, etc.
    Cpp17,
    
    /// C++20 Standard (ISO/IEC 14882:2020)
    /// Introduced concepts, modules, coroutines, ranges, and other major features
    Cpp20,
    
    /// C++23 Standard (ISO/IEC 14882:2023)
    /// The latest C++ standard, introducing more modern features
    Cpp23,
    
    /// GNU C++ Extensions
    /// Supports GCC-specific C++ extension syntax
    GnuCpp,
    
    /// Microsoft Visual C++ Extensions
    /// Supports MSVC-specific C++ extension syntax
    MsvcCpp,
}

impl fmt::Display for CppLanguageLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CppLanguageLevel::Cpp11 => write!(f, "C++11"),
            CppLanguageLevel::Cpp14 => write!(f, "C++14"),
            CppLanguageLevel::Cpp17 => write!(f, "C++17"),
            CppLanguageLevel::Cpp20 => write!(f, "C++20"),
            CppLanguageLevel::Cpp23 => write!(f, "C++23"),
            CppLanguageLevel::GnuCpp => write!(f, "GNU C++"),
            CppLanguageLevel::MsvcCpp => write!(f, "MSVC C++"),
        }
    }
}

impl CppLanguageLevel {
    /// Check if the current language level supports the specified feature
    pub fn supports_feature(&self, feature: CppFeature) -> bool {
        use CppFeature::*;
        match feature {
            // C++98/03 features
            BasicOOP | Templates | Exceptions | Namespaces => true,
            
            // C++11 features
            Auto | Lambda | RValueReferences | SmartPointers | 
            VariadicTemplates | ThreadSupport | RegexSupport |
            RandomNumbers | TimeUtilities => *self >= CppLanguageLevel::Cpp11,
            
            // C++14 features
            GenericLambda | VariableTemplates | BinaryLiterals |
            DigitSeparators => *self >= CppLanguageLevel::Cpp14,
            
            // C++17 features
            StructuredBindings | IfConstexpr | ClassTemplateArgumentDeduction |
            FoldExpressions | InlineVariables => *self >= CppLanguageLevel::Cpp17,
            
            // C++20 features
            Concepts | Modules | Coroutines | Ranges | 
            ThreeWayComparison | DesignatedInitializers => *self >= CppLanguageLevel::Cpp20,
            
            // C++23 features
            DeducingThis | IfConsteval | MultidimensionalSubscript => *self >= CppLanguageLevel::Cpp23,
            
            // Compiler-specific extensions
            GnuExtensions => matches!(self, CppLanguageLevel::GnuCpp),
            MsvcExtensions => matches!(self, CppLanguageLevel::MsvcCpp),
        }
    }
    
    /// Get the default language level
    pub fn default() -> Self {
        CppLanguageLevel::Cpp17
    }
    
    /// Parse language level from string
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "c++11" | "11" => Some(CppLanguageLevel::Cpp11),
            "c++14" | "14" => Some(CppLanguageLevel::Cpp14),
            "c++17" | "17" => Some(CppLanguageLevel::Cpp17),
            "c++20" | "20" => Some(CppLanguageLevel::Cpp20),
            "c++23" | "23" => Some(CppLanguageLevel::Cpp23),
            "gnu" | "gnu++" => Some(CppLanguageLevel::GnuCpp),
            "msvc" | "visual c++" => Some(CppLanguageLevel::MsvcCpp),
            _ => None,
        }
    }
}

/// C++ feature enumeration
/// Used to check whether a specific language level supports a certain feature
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CppFeature {
    // C++98/03 basic features
    BasicOOP,                    // Basic object-oriented programming
    Templates,                   // Templates
    Exceptions,                  // Exception handling
    Namespaces,                  // Namespaces
    
    // C++11 features
    Auto,                        // auto keyword
    Lambda,                      // lambda expressions
    RValueReferences,            // rvalue references
    SmartPointers,               // smart pointers
    VariadicTemplates,           // variadic templates
    ThreadSupport,               // thread support
    RegexSupport,                // regular expressions
    RandomNumbers,               // random number generation
    TimeUtilities,               // time utilities
    
    // C++14 features
    GenericLambda,               // generic lambda
    VariableTemplates,           // variable templates
    BinaryLiterals,              // binary literals
    DigitSeparators,             // digit separators
    
    // C++17 features
    StructuredBindings,          // structured bindings
    IfConstexpr,                 // if constexpr
    ClassTemplateArgumentDeduction, // class template argument deduction
    FoldExpressions,             // fold expressions
    InlineVariables,             // inline variables
    
    // C++20 features
    Concepts,                    // concepts
    Modules,                     // modules
    Coroutines,                  // coroutines
    Ranges,                      // ranges
    ThreeWayComparison,          // three-way comparison
    DesignatedInitializers,      // designated initializers
    
    // C++23 features
    DeducingThis,                // deducing this
    IfConsteval,                 // if consteval
    MultidimensionalSubscript,   // multidimensional subscript operator
    
    // Compiler extensions
    GnuExtensions,               // GNU extensions
    MsvcExtensions,              // MSVC extensions
}
