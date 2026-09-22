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

    /// Class/struct/union body - the `{ ... }` part of a class definition, including access
    /// specifier sections and member declarations.
    /// e.g.: the `{ public: void f(); }` in `class A { public: void f(); };`
    ClassBody,

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

    /// A structured binding's name list (C++17).
    /// e.g.: the `[a, b]` of `auto [a, b] = pair;`
    ///
    /// A node of its own because the names it introduces are *not* declarators: each one binds a
    /// sub-object of whatever initializes the whole, and none of them has a type of its own. A
    /// consumer looking for "the things this declaration declares" has to find them here or it
    /// reports one variable named `[a, b]`.
    StructuredBinding,

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
    RestrictQual, // ========== Other Syntax Elements ==========
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
    /// A line comment - `//` style comment
    /// e.g.: // This is a comment
    LineComment,

    /// Block comment - `/* */` style comment
    /// e.g.: /* This is a block comment */
    BlockComment,

    /// A run of consecutive comments, parsed as documentation.
    ///
    /// The node a consumer of the tree actually walks. One node covers **one or more** adjacent
    /// comments, because that is the unit a doc comment is written in:
    ///
    /// ```text
    /// /// Computes the area.
    /// /// @param r  the radius.
    /// /// @returns  the area.
    /// ```
    ///
    /// Three comments, one document. Splitting them into three nodes would push the job of grouping
    /// them back onto every consumer.
    ///
    /// A `Comment` node also covers ordinary `//` and `/* */` comments — the ones with nothing
    /// documentation-shaped in them. They are kept in the tree for losslessness, and giving them the
    /// same node kind means a consumer walking comments sees all of them rather than having to look
    /// in two places. They simply contain no `DocCommand` children.
    ///
    /// Constructs inside it: [`DocCommand`](Self::DocCommand), [`DocInline`](Self::DocInline).
    DocComment,

    /// One Doxygen command and its arguments: `@brief x`, `@param[in] x desc`, `@returns y`.
    ///
    /// A single node kind driven by the command's *name* rather than one node kind per command, so
    /// the grammar is table-driven: adding `@since` is a row in a table, not a new parse function
    /// and a new kind. The name is the [`CppTokenKind::DocCommandName`] token directly inside.
    DocCommand,

    /// The body of a documentation comment: everything that is not part of a command.
    ///
    /// One of these wraps the whole comment's content, and a second kind of the same name carries a
    /// command's payload — see [`DocCommandBody`](Self::DocCommandBody). The wrapper exists so that a
    /// consumer can iterate a comment's commands without also seeing the comment's own delimiters and
    /// line markers.
    DocCommentBody,

    /// A command's payload: everything from the end of the command's arguments to the end of its
    /// line, or to the end of a block comment.
    ///
    /// Separate from [`DocCommand`](Self::DocCommand) so that a consumer asking "what does `@param x`
    /// say about `x`?" has one node to read, whether the payload is a phrase or three paragraphs.
    DocCommandBody,

    /// A named argument of a command, such as the `x` of `@param[in] x`.
    ///
    /// Its own node because it is the part a cross-reference needs: matching `@param x` against the
    /// parameter named `x` is a lookup by this node's text, and doing that from a token stream means
    /// re-implementing the argument grammar at every use.
    DocCommandArg,

    /// An inline reference inside a description: `@ref Foo`, `@p name`, `#member`, `::scope::name`.
    ///
    /// Kept as a node rather than plain text because it is a link: it names a declaration, and that
    /// is what "go to definition" from inside a comment needs.
    DocInline,

    /// A code block: the lines between `@code` and `@endcode`.
    ///
    /// Its contents are *not* parsed as documentation. That is the whole point — code inside a code
    /// block is full of `@`, `<` and `\`, and treating it as prose turns a snippet into nonsense.
    DocCodeBlock,

    // ========== Declarations (grammar detail) ==========
    /// A preprocessor directive, from `#` to the end of its logical line: `#include <vector>`,
    /// `#define MAX(a, b) ...`, `#if`, `#endif`.
    ///
    /// Directives are *not* removed from the tree. They have to stay for the CST to be lossless, and
    /// more importantly the branches of a conditional compilation block contain real declarations
    /// that an editor must still parse and index — the preprocessor layer, which knows which branch
    /// is selected, is built on top of this node rather than underneath it.
    PreprocessorDirective,

    // ========== Modules (C++20) ==========
    // `module` and `import` are *contextual* keywords: the standard calls them "identifier with
    // special meaning", so they stay usable as ordinary names and the lexer hands them over as
    // identifiers. The parser decides from the text, in the contexts where the meaning is special.
    /// A module declaration: `export? module name : partition? ;`.
    ///
    /// e.g.: `export module my.mod;`, `module my.mod:part;`
    ModuleDecl,

    /// An import declaration: `export? import name | :partition | <header> ;`.
    ///
    /// e.g.: `import std;`, `export import :part;`, `import <iostream>;`
    ImportDecl,

    /// An export block: `export { declaration-seq? }`.
    ExportBlock,

    /// The dotted name of a module.
    ///
    /// e.g.: the `my.mod` in `export module my.mod;`
    ModuleName,

    /// A module partition, including its leading `:`.
    ///
    /// e.g.: the `:part` in `module my.mod:part;`
    ModulePartition,

    /// A header unit name in an import: `<iostream>` or `"local.h"`.
    HeaderName,

    /// A global module fragment: `module ;` followed by preprocessing directives.
    ///
    /// This is where `#include` is still allowed inside a module unit, because the preprocessor runs
    /// before the module machinery and a header that uses configuration macros has to be included
    /// rather than imported.
    GlobalModuleFragment,

    /// A private module fragment: `module : private ;` followed by declarations that importers of
    /// the module cannot see.
    PrivateModuleFragment,
    // A declaration in C++ is `decl-specifier-seq init-declarator-list ;`, and every one of those
    // three pieces is optional depending on the declaration, so they each get their own node
    // rather than being flattened into the declaration. The AST layer needs the boundaries: "what
    // is the type of this declaration" and "what is its name" are the two most common questions
    // asked of a C++ tree, and neither can be answered from a flat token list.
    /// One complete declaration, whatever its kind.
    ///
    /// e.g.: `int x = 1;`, `void f();`, `MyClass obj;`, `static constexpr int n = 5;`
    Declaration,

    /// The `decl-specifier-seq`: type specifiers, cv-qualifiers, storage class and function
    /// specifiers, in any order.
    ///
    /// e.g.: `int`, `static const`, `virtual inline`, `std::vector<int>`, `auto`
    DeclSpecifierSeq,

    /// An `init-declarator`: a declarator plus an optional initializer or function body.
    ///
    /// e.g.: `x = 1`, `*p`, `f(int a)`, `arr[10]`
    InitDeclarator,

    /// A `declarator`: the part that names the entity, possibly wrapped in pointers, references,
    /// arrays, function parameter lists and template argument lists.
    ///
    /// e.g.: `x`, `*p`, `&r`, `arr[10]`, `f(int)`, `ns::C::operator+`
    Declarator,

    /// A `friend` declaration: `friend class X;`, `friend void f();`, `friend bool operator==(...)`.
    ///
    /// `friend` is not a specifier of the declaration that follows it; it *is* the declaration, which
    /// is why this is a separate node rather than a specifier inside the friend's own declaration.
    FriendDecl,

    /// A type on its own, as it appears after `sizeof`, in a cast, in a parameter or as a    /// `type-id`.
    ///
    /// e.g.: `int`, `const char*`, `std::vector<int>`
    TypeId,

    /// A trailing return type after `->`.
    ///
    /// This gets its own kind rather than reusing [`CppSyntaxKind::TypeId`] because it is also the
    /// declarator's "this is a function" marker: a declarator whose events contain one can only be a
    /// function, which is how a `{` after it is recognised as a body rather than an initializer.
    ///
    /// e.g.: the `-> T*` in `auto begin() -> T*`
    TrailingReturnType,

    // ========== Error Recovery ==========
    /// Error node - for error recovery in parsing
    /// Used when the parser encounters unrecognized syntax
    ErrorNode,

    /// Missing node - represents a missing syntax element
    /// Used for handling incomplete syntax structures
    MissingNode,
}
