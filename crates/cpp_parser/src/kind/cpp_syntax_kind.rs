/// C++ Syntax Kind Enumeration
///
/// This enum defines all possible syntax node types in the C++ syntax tree.
/// It is used to build the Abstract Syntax Tree (AST), and each enum value represents a kind of syntax structure.
///
/// Note: Only syntax structures are included here, not lexical tokens.
/// Tokens are defined in CppTokenKind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum CppSyntaxKind {
    /// Empty node, used for initialization
    None,

    // ========== Top-level Syntax Structure ==========
    /// Translation unit - the root node of a C++ source file
    /// Contains all top-level declarations and definitions
    TranslationUnit,
    // ========== Declarations ==========
    /// Function declaration - only the function signature, no implementation
    /// e.g.: int func(int x);
    FunctionDecl,

    /// Function definition - complete definition with implementation
    /// e.g.: int func(int x) { return x + 1; }
    FunctionDef,

    /// Class declaration - forward declaration
    /// e.g.: class MyClass;
    ClassDecl,

    /// Class definition - complete class definition
    /// e.g.: class MyClass { ... };
    ClassDef,

    /// Struct declaration - forward declaration
    /// e.g.: struct MyStruct;
    StructDecl,

    /// Struct definition - complete struct definition
    /// e.g.: struct MyStruct { ... };
    StructDef,

    /// Union declaration - forward declaration
    /// e.g.: union MyUnion;
    UnionDecl,

    /// Union definition - complete union definition
    /// e.g.: union MyUnion { ... };
    UnionDef,

    /// Enum declaration - forward declaration
    /// e.g.: enum Color : int;
    EnumDecl,

    /// Enum definition - complete enum definition
    /// e.g.: enum Color { Red, Green, Blue };
    EnumDef,

    /// Enum class declaration - forward declaration (C++11)
    /// e.g.: enum class MyEnum;
    EnumClassDecl,

    /// Enum class declaration - forward declaration (C++11)
    /// e.g.: enum class MyEnumClass;
    EnumClassDef,

    /// Typedef declaration - type alias
    /// e.g.: typedef int MyInt;
    TypedefDecl,

    /// Using declaration - introduce a name
    /// e.g.: using std::cout;
    UsingDecl,

    /// Using directive - introduce an entire namespace
    /// e.g.: using namespace std;
    UsingDirective,

    /// Namespace declaration
    /// e.g.: namespace MyNamespace { ... }
    NamespaceDecl,

    /// Variable declaration
    /// e.g.: int x, y = 5;
    VariableDecl,

    /// Field declaration - class/struct member variable
    /// e.g.: class A { int member; };
    FieldDecl,

    /// Template declaration
    /// e.g.: template<typename T> class MyClass;
    TemplateDecl,

    /// Template specialization - specialized version of a template
    /// e.g.: template<> class MyClass<int> { ... };
    TemplateSpecialization,

    /// Concept declaration (C++20)
    /// e.g.: template<typename T> concept Copyable = ...;
    ConceptDecl,
    // ========== Statements ==========
    /// Compound statement - block of statements in braces
    /// e.g.: { statement1; statement2; }
    CompoundStat,

    /// Expression statement - expression ending with a semicolon
    /// e.g.: x = 5;
    ExpressionStat,

    /// Declaration statement - declaration in statement position
    /// e.g.: int x = 5;
    DeclStat,

    /// if statement - conditional statement
    /// e.g.: if (condition) statement
    IfStat,

    /// else-if clause - else if part of if statement
    ElseIfStat,

    /// else clause - else part of if statement
    /// e.g.: else statement
    ElseStat,

    /// switch statement - multi-branch selection
    /// e.g.: switch (expr) { ... }
    SwitchStat,

    /// case label - case branch in switch
    /// e.g.: case 1:
    CaseStat,

    /// default label - default branch in switch
    /// e.g.: default:
    DefaultStat,

    /// while loop - pre-test loop
    /// e.g.: while (condition) statement
    WhileStat,

    /// do-while loop - post-test loop
    /// e.g.: do statement while (condition);
    DoWhileStat,

    /// for loop - traditional for loop
    /// e.g.: for (init; condition; increment) statement
    ForStat,

    /// Range-based for loop (C++11)
    /// e.g.: for (auto& item : container) statement
    RangeForStat,

    /// break statement - exit loop or switch
    BreakStat,

    /// continue statement - continue to next loop iteration
    ContinueStat,

    /// return statement - function return
    /// e.g.: return expression;
    ReturnStat,

    /// goto statement - unconditional jump
    /// e.g.: goto label;
    GotoStat,

    /// label statement - target for goto
    /// e.g.: label:
    LabelStat,

    /// try block - exception handling
    /// e.g.: try { ... }
    TryStat,

    /// catch block - exception catch
    /// e.g.: catch (Exception& e) { ... }
    CatchStat,

    /// throw statement - throw exception
    /// e.g.: throw exception;
    ThrowStat,

    /// Empty statement - single semicolon
    /// e.g.: ;
    EmptyStat,
    // ========== Expressions ==========
    /// Literal expression - numbers, strings, chars, etc.
    /// e.g.: 42, "hello", 'c', true, nullptr
    LiteralExpr,

    /// Identifier expression - variable/function names
    /// e.g.: variable, function
    IdentifierExpr,

    /// Parenthesized expression
    /// e.g.: (expression)
    ParenExpr,

    /// Unary expression - unary operator
    /// e.g.: -x, !flag, ++i, *ptr, &var
    UnaryExpr,

    /// Binary expression - binary operator
    /// e.g.: a + b, x == y, ptr->member
    BinaryExpr,

    /// Ternary expression - conditional operator
    /// e.g.: condition ? true_expr : false_expr
    TernaryExpr,

    /// Function call expression
    /// e.g.: func(args), obj.method(args)
    CallExpr,

    /// Member access expression - dot operator
    /// e.g.: obj.member
    MemberExpr,

    /// Arrow access expression - arrow operator
    /// e.g.: ptr->member
    ArrowExpr,

    /// Index expression - array/container access
    /// e.g.: arr[index], map[key]
    IndexExpr,

    /// Cast expression - type cast
    /// e.g.: (int)x, static_cast<int>(x)
    CastExpr,

    /// sizeof expression - get size of type or expression
    /// e.g.: sizeof(int), sizeof(expression)
    SizeofExpr,

    /// typeid expression - get type info
    /// e.g.: typeid(int), typeid(expression)
    TypeidExpr,

    /// new expression - dynamic memory allocation
    /// e.g.: new int, new int[10], new MyClass()
    NewExpr,

    /// delete expression - free dynamic memory
    /// e.g.: delete ptr, delete[] arr
    DeleteExpr,

    /// this expression - current object pointer
    /// e.g.: this, this->member
    ThisExpr,

    /// Lambda expression (C++11)
    /// e.g.: [capture](params) -> return_type { body }
    LambdaExpr,

    /// Initializer list expression (C++11)
    /// e.g.: {1, 2, 3}, {.x = 1, .y = 2}
    InitListExpr,

    /// Designated initializer expression (C++20)
    /// e.g.: {.member = value}
    DesignatedInitExpr,

    /// Compound literal expression
    /// e.g.: (struct Point){.x = 1, .y = 2}
    CompoundLiteralExpr,
    // ========== Types ==========
    /// Built-in type - C++ basic types
    /// e.g.: int, char, float, double, bool, void
    BuiltinType,

    /// Pointer type - pointer to another type
    /// e.g.: int*, char*, MyClass*
    PointerType,

    /// Reference type - lvalue reference
    /// e.g.: int&, const std::string&
    ReferenceType,

    /// Rvalue reference type (C++11)
    /// e.g.: int&&, std::string&&
    RValueReferenceType,

    /// Array type - fixed size array
    /// e.g.: int[10], char[256]
    ArrayType,

    /// Function type - function signature type
    /// e.g.: int(int, int), void()
    FunctionType,

    /// Qualified type - const/volatile qualified type
    /// e.g.: const int, volatile double
    QualifiedType,

    /// Template type - template instantiation
    /// e.g.: std::vector<int>, MyTemplate<T>
    TemplateType,

    /// auto type (C++11)
    /// e.g.: auto x = 5;
    AutoType,

    /// decltype type (C++11)
    /// e.g.: decltype(expr)
    DecltypeType,

    /// typename type - type name in template
    /// e.g.: typename T::value_type
    TypenameType,
    // ========== Template Related ==========
    /// Template parameter - parameter in template declaration
    /// e.g.: template<typename T> T
    TemplateParameter,

    /// Template argument - argument in template instantiation
    /// e.g.: std::vector<int> int
    TemplateArgument,

    /// Template argument list - argument list in template instantiation
    /// e.g.: <int, double>
    TemplateArgumentList,

    /// Template parameter list - parameter list in template declaration
    /// e.g.: <typename T, int N>
    TemplateParameterList,
    // ========== Preprocessor Directives ==========
    /// #include directive - include header file
    /// e.g.: #include <iostream>
    IncludeDirective,

    /// #define directive - macro definition
    /// e.g.: #define MAX_SIZE 100
    DefineDirective,

    /// #undef directive - undefine macro
    /// e.g.: #undef MAX_SIZE
    UndefDirective,

    /// #ifdef directive - conditional compilation (defined)
    /// e.g.: #ifdef DEBUG
    IfdefDirective,

    /// #ifndef directive - conditional compilation (not defined)
    /// e.g.: #ifndef HEADER_H
    IfndefDirective,

    /// #if directive - conditional compilation
    /// e.g.: #if VERSION > 2
    IfDirective,

    /// #else directive - else branch of conditional compilation
    /// e.g.: #else
    ElseDirective,

    /// #elif directive - else if branch of conditional compilation
    /// e.g.: #elif VERSION == 1
    ElifDirective,

    /// #endif directive - end of conditional compilation
    /// e.g.: #endif
    EndifDirective,

    /// #pragma directive - compiler-specific directive
    /// e.g.: #pragma once
    PragmaDirective,

    /// #error directive - compile-time error
    /// e.g.: #error "Unsupported platform"
    ErrorDirective,

    /// #warning directive - compile-time warning
    /// e.g.: #warning "Deprecated function"
    WarningDirective,

    /// #line directive - line number control
    /// e.g.: #line 100 "file.cpp"
    LineDirective,
    // ========== Specifiers and Qualifiers ==========
    /// Access specifier - class member access control
    /// e.g.: public:
    PublicAccess,
    /// e.g.: private:
    PrivateAccess,
    /// e.g.: protected:
    ProtectedAccess,

    /// Storage class specifier - variable/function storage
    /// e.g.: static int x;
    StaticSpec,
    /// e.g.: extern int x;
    ExternSpec,
    /// e.g.: thread_local int x;
    ThreadLocalSpec,
    /// e.g.: mutable int x;
    MutableSpec,

    /// Function specifier - special function attributes
    /// e.g.: inline void func();
    InlineSpec,
    /// e.g.: virtual void func();
    VirtualSpec,
    /// e.g.: explicit MyClass(int);
    ExplicitSpec,
    /// e.g.: constexpr int func();
    ConstexprSpec,
    /// e.g.: void func() noexcept;
    NoexceptSpec,

    /// CV qualifier - const/volatile qualifier for types
    /// e.g.: const int x;
    ConstQual,
    /// e.g.: volatile int x;
    VolatileQual,
    /// e.g.: restrict int* ptr; (C extension)
    RestrictQual,    // ========== Other Syntax Elements ==========
    /// Function parameter - single parameter in function definition/declaration
    /// e.g.: int func(int param) param
    Parameter,

    /// Parameter list - function parameter list
    /// e.g.: (int x, double y, char* z)
    ParameterList,

    /// Argument list - function call argument list
    /// e.g.: func(arg1, arg2, arg3)
    ArgumentList,

    // ========== Legacy Support (for migration compatibility) ==========
    /// Parameter list (legacy name for compatibility)
    ParamList,

    /// Call argument list (legacy name for compatibility)
    CallArgList,

    /// Parameter name (legacy for compatibility)
    ParamName,

    /// Local name (legacy for compatibility)
    LocalName,

    /// Name expression - identifier reference
    /// e.g.: variable_name
    NameExpr,
    
    /// Do statement - rarely used in C++ but exists
    /// e.g.: do { ... } while(condition);
    DoStat,

    /// Else-if clause in conditional statement
    /// e.g.: else if (condition) { ... }
    ElseIfClauseStat,

    /// Else clause in conditional statement
    /// e.g.: else { ... }
    ElseClauseStat,

    /// Function statement/definition
    /// e.g.: void func() { ... }
    FuncStat,

    /// Local function definition (for compatibility)
    LocalFuncStat,

    /// Local variable declaration/definition
    /// e.g.: int local_var = 5;
    LocalStat,

    /// Assignment statement
    /// e.g.: x = y + z;
    AssignStat,

    /// Call expression as statement
    /// e.g.: func(); (function call as standalone statement)
    CallExprStat,

    /// Global variable/function declaration
    GlobalStat,

    /// Repeat statement (for compatibility - maps to do-while)
    RepeatStat,

    /// Specialized call expressions (for migration compatibility)
    /// Assert function call
    AssertCallExpr,

    /// Error function call
    ErrorCallExpr,

    /// Require function call (for compatibility)
    RequireCallExpr,

    /// Type function call
    TypeCallExpr,

    /// Setmetatable function call (for compatibility)
    SetmetatableCallExpr,

    /// Closure expression - lambda expression
    /// e.g.: [capture](params) { body }
    ClosureExpr,

    // ========== Table-like structures (for compatibility with legacy code) ==========
    /// Empty table/initializer list expression
    /// e.g.: {} or std::initializer_list<T>{}
    TableEmptyExpr,

    /// Array-like table expression
    /// e.g.: {1, 2, 3} or std::array<int, 3>{1, 2, 3}
    TableArrayExpr,

    /// Object-like table expression
    /// e.g.: {.x = 1, .y = 2} (designated initializers)
    TableObjectExpr,

    /// Table field assignment
    /// e.g.: .field = value
    TableFieldAssign,

    /// Table field value
    /// e.g.: field in table initialization
    TableFieldValue,

    /// Attribute - C++11 attribute
    /// e.g.: [[nodiscard]], [[deprecated]]
    Attribute,

    /// Attribute list - collection of attributes
    /// e.g.: [[nodiscard, deprecated("use new_func instead")]]
    AttributeList,

    /// Initializer - variable initializer expression
    /// e.g.: int x = 5; = 5
    Initializer,

    /// Base specifier - inheritance specifier
    /// e.g.: class Derived : public Base public Base
    BaseSpecifier,

    /// Member initializer - constructor member initializer list
    /// e.g.: MyClass() : member(value) {}
    MemberInitializer,

    /// Catch handler - handler in catch block
    /// e.g.: catch (const std::exception& e)
    CatchHandler,

    /// Enumerator declaration - enum member
    /// e.g.: enum Color { Red, Green, Blue }; Red
    EnumeratorDecl,

    // ========== Comments and Documentation ==========
    /// Line comment - // style comment
    /// e.g.: // This is a comment
    LineComment,

    /// Block comment - /* */ style comment
    /// e.g.: /* This is a block comment */
    BlockComment,

    /// Documentation comment - for generating docs
    /// e.g.: /// or /** */ style comment
    DocComment,

    // ========== Error Recovery ==========
    /// Error node - for error recovery in parsing
    /// Used when the parser encounters unrecognized syntax
    ErrorNode,

    /// Missing node - represents a missing syntax element
    /// Used for handling incomplete syntax structures
    MissingNode,
}
