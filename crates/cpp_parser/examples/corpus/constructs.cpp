// A range of constructs, to find where the grammar gives up.
//
// This file is a probe, not a fixture: it is parsed by hand with
//   cargo run -p cpp_parser --example dump -- crates/cpp_parser/examples/corpus/constructs.cpp
// and the report is the list of constructs that still produce an `ErrorNode` or a syntax error.
//
// It is deliberately *not* wired into a test. What the parser is expected to read is pinned in
// `tests/gaps.rs`, one construct per line with the reason it is or is not supported; this file is
// for the opposite job — throwing a realistic mixture at the parser and seeing what comes back,
// which is how the list in that test was found in the first place.
#include <vector>
#include <string>
#include <memory>

namespace probe {

// --- declarations ---------------------------------------------------------
int a = 1, *b = nullptr, c[3] = {1, 2, 3};
const char* s = "x";
static thread_local int tls = 0;
extern "C" void c_linkage();
int (*fp)(int, double) = nullptr;
int (*fpa[4])(void);
void (*signal_handler(int sig))(int);
using Callback = void (*)(int);
typedef void (*OldCallback)(int);
constexpr int kMax = 16;
struct Forward;
struct Outer { struct Inner { int x; }; };
struct Point { int x; } origin;
enum Color { Red, Green = 2, Blue };
enum class Flags : unsigned int { None = 0, All = 0xffff };
union Value { int i; float f; ~Value() {} };

// --- direct initialisation -------------------------------------------------
struct Widget {
    Widget();
    Widget(int, int, int);
    Widget(std::initializer_list<int>);
};

Widget g_widget(1, 2, 3);
Widget g_copy(g_widget);
Widget g_one(1);
Widget g_brace{1, 2, 3};
auto g_auto = Widget(1, 2, 3);
std::string g_name("hi");

void use() {
    Widget w(1, 2, 3);
    Widget v = Widget(1, 2, 3);
    Widget u{w};
    int i(1);
    std::string str("hello");
    auto p = std::make_unique<Widget>(1, 2, 3);
    f(1, 2);
    obj.method(1);
}

// --- classes ---------------------------------------------------------------
class Base {
public:
    Base() = default;
    virtual ~Base() = default;
    virtual int value() const = 0;
};

class Derived final : public Base, private Widget {
public:
    Derived(int v) : Base(), Widget(), value_(v) {}
    int value() const override { return value_; }
    static int count;
    mutable int cache_ = 0;
    [[nodiscard]] int pure() const noexcept;
    explicit operator bool() const { return value_ != 0; }
    friend void swap(Derived&, Derived&);
    Derived& operator+=(const Derived& other);
    bool operator==(const Derived&) const = default;

private:
    int value_;
};

// --- templates -------------------------------------------------------------
template <typename T, int N = 3, typename... Rest>
class Grid {
public:
    T& at(int i) { return cells_[i]; }
    template <typename U>
    void fill(const U& value);

private:
    T cells_[N];
};

template <typename T>
void Grid<T, 3>::fill(const T& value) {}

template <>
struct Grid<int, 3> {
    int cells_[3];
};

using IntGrid = Grid<int, 3>;
using Vec = std::vector<std::vector<int>>;

// --- functions -------------------------------------------------------------
int add(int a, int b) { return a + b; }
auto trailing(int a) -> decltype(a) { return a; }
void variadic(const char* fmt, ...);
constexpr int ceval(int n) { return n > 0 ? n * ceval(n - 1) : 1; }
inline int inlined() { return 1; }
[[noreturn]] void never_returns();

// --- statements ------------------------------------------------------------
int control_flow(int n) {
    int total = 0;
    for (int i = 0; i < n; ++i) {
        if (i % 2 == 0) {
            continue;
        } else if (i == 7) {
            break;
        }
        for (auto& item : items) {
            total += item;
        }
        while (n > 0) {
            --n;
        }
        do {
            ++total;
        } while (total < 3);
    }

    switch (n) {
        case 1:
        case 2 ... 4:
            total = 1;
            break;
        default:
            total = -1;
            break;
    }

    try {
        throw 1;
    } catch (const std::exception& e) {
        total = 2;
    } catch (...) {
        total = 3;
    }

again:
    if (total > 100) {
        goto again;
    }

    auto lambda = [&total](int x) -> int { return total + x; };
    auto generic = [](auto&& x) { return x; };
    auto init_capture = [value = total + 1] { return value; };

    int arr[3] = {1, 2, 3};
    auto [first, second] = std::pair<int, int>{1, 2};
    for (auto&& [k, v] : map) {
        total += k;
    }

    total = total > 0 ? total : -total;
    total += static_cast<int>(3.5);
    auto np = new int[4]{1, 2, 3, 4};
    delete[] np;

    return total;
}

// --- lambdas and callables -------------------------------------------------
auto make_adder(int base) {
    return [base](int x) mutable constexpr noexcept { return base + x; };
}

// A C++20 generic lambda: a template parameter list between the capture list and the parameters.
auto make_identity() {
    return []<typename T>(T value) { return value; };
}

auto make_pack_forwarder() {
    // A parameter pack with a forwarding reference — `Ts&&... args` — in a definition.
    return []<typename... Ts>(Ts&&... args) { return first_of(args); };
}

// --- coroutines ------------------------------------------------------------
template <typename T>
struct Task {
    bool await_ready() const noexcept;
    void await_suspend(Handle handle) noexcept;
    T await_resume();
};

Task<int> counter(int limit) {
    int total = 0;
    for (int i = 0; i < limit; ++i) {
        total += co_await fetch(i);
        co_yield total;
    }
    co_return total;
}

// --- the `template` disambiguator ------------------------------------------
template <typename T>
struct Rebind {
    template <typename U>
    using rebind = Rebind<U>;
};

template <typename T>
void use_dependent_names(T value) {
    auto one = T::template rebind<int>(value);
    auto two = value.template rebind<int>(value);
    auto three = pointer->template rebind<int>(value);
    typename T::template rebind<int> named;
}

// --- conversion operators --------------------------------------------------
struct Ratio {
    Ratio(int numerator, int denominator);
    // A conversion operator's name is the type it converts to, and it begins the declaration — there is no
    // return type in front of it.
    operator double() const;
    operator bool() const noexcept;
    operator const char*() = delete;
    explicit operator std::string() const;
};

struct RefQualified {
    operator int() const &;
    operator bool() &&;
};

Ratio::operator double() const { return 1.0; }

// --- packs, expansions and folds -------------------------------------------
template <typename... Ts>
void forward_all(Ts&&... args) {
    // A pack expansion, in the three places one can be written.
    consume(count_of(args)...);
    auto counted = sizeof...(Ts);
    auto held = std::tuple<Ts...>();
}

template <typename... Ts>
using TupleOf = std::tuple<Ts...>;

template <typename... Ts>
auto sum_all(Ts... values) {
    // Fold expressions, in all four spellings.
    auto right = (values + ...);
    auto left = (... + values);
    auto all = (values && ...);
    auto any = (... || values);
    return right + left + (all ? 1 : 0) + (any ? 1 : 0);
}

// --- alias types with suffixes ---------------------------------------------
using Buffer = char[256];
using Handler = int(const char*);
using HandlerPtr = int (*)(const char*);
using Matrix = int (*)[4];

// --- attributes, in every position that reads them -------------------------
[[nodiscard]] int attributed();

struct Attributed {
    [[nodiscard]] int pure() const;
    void sink(int value [[maybe_unused]]);
    using Size [[deprecated]] = unsigned long;
    enum class Mode { Fast [[deprecated]] = 1, Slow };
};

int aligned_global [[gnu::aligned(16)]] = 0;
int carrying() [[carries_dependency]];
void never() [[noreturn]] { for (;;) {} }

template <typename T>
[[nodiscard]] T identity(T value) { return value; }

// --- alignment -------------------------------------------------------------
struct Aligned {
    alignas(16) int first;
    alignas(32) alignas(16) int second;
    alignas(double) char buffer[8];
};

alignas(16) int aligned_global = 0;
alignas(16) alignas(64) Aligned doubly_aligned;

// A named type after the alignment, which is the case that reads the *type* out of the specifiers rather than
// taking the name for a declarator.
alignas(16) Aligned aligned_object;

struct Empty { };
alignas(1) Empty after_a_declaration;

void take_aligned(alignas(16) int value);

// --- operators -------------------------------------------------------------
struct Ops {
    Ops operator+(const Ops&) const;
    Ops& operator++();
    Ops operator++(int);
    int operator[](int index) const;
    void* operator new(std::size_t size);
    void operator delete(void* ptr) noexcept;
    Ops& operator=(const Ops&) = delete;
    bool operator<(const Ops&) const;
};

// --- declarations that used to be gaps -------------------------------------
struct Packed {
    unsigned kind : 3;
    unsigned flags : 5 = 0;
    unsigned : 0;
    virtual ~Packed() = default;
    Packed(const Packed&) = delete;
    Packed& operator=(const Packed&) = delete;
    explicit operator bool() const noexcept;
    static constexpr int kWidth = 8;
    mutable int cache_ = 0;
    bool operator==(const Packed&) const = default;
};

union Variant {
    int i;
    float f;
    ~Variant() {}
};

struct Derived : Packed {
    using Packed::operator bool;
    using Alias = Packed;
    Derived() : Packed{}, cache_(0) {}
    ~Derived() {}
};

Packed::~Packed() {}

// --- namespaces ------------------------------------------------------------
namespace outer::inner {
int nested = 1;
}  // namespace outer::inner

namespace alias = outer::inner;

// --- concepts and constraints ----------------------------------------------
// One word, four constructs, each nested in the others: a constrained template parameter, a
// requires-clause after the parameter list and after a declarator, a concept whose constraint is a
// requires-expression, and a requires-expression used as an ordinary expression.
template <typename T>
concept Sized = requires(T t) {
    t.size();
    { t.empty() } noexcept -> bool;
    typename T::value_type;
    requires sizeof(T) > 1;
};

template <typename T>
concept Container = Sized<T> && requires(T t) { t.begin(); };

template <typename T>
concept Addable = requires(T a, T b) { a + b; } || requires(T a) { -a; };

template <Sized T>
void takes_a_sized(T& value) { }

template <Container<T> U>
struct Wrapper { };

template <typename T>
    requires Sized<T> && Container<T>
void constrained(T& value) { }

template <typename T>
void trailing(T& value) requires Sized<T> && Container<T>
{
    auto sized = requires { value.size(); };
    if constexpr (requires { value.begin(); }) {
        static_assert(requires(T t) { t.size(); });
    }
}

template <typename T>
    requires Sized<T>
struct Constrained {
    void member(T& value) requires Container<T> { }
};

template <typename T>
    requires requires(T t) { t.size(); }
void nested_clause(T& value) { }

// A parenthesised constraint, which the standard *requires* for anything that is not a conjunction of primary
// expressions: `requires N == 0` is ill-formed and `requires (N == 0)` is not.
template <typename T>
    requires (sizeof(T) > 1) && (sizeof(T) < 64)
void parenthesised(T& value) { }

template <int N>
    requires (N > 0)
struct Bounded { };

// A constraint after a trailing return type — the clause follows the `-> T`, never precedes it.
template <typename T>
auto trailing_after_return(T& value) -> decltype(value.size()) requires Sized<T>
{
    return value.size();
}

// --- braced initialisers where the grammar wants an initializer-clause ------
void braced_arguments(std::vector<int>& values) {
    int value = 0;
    value = {1};
    value += {2};
    values.push_back({1, 2});
    values = {};
}

// --- explicit instantiations -----------------------------------------------
extern template void forward_all<int>(int);
extern template struct Wrapper<Constrained<int>>;

}  // namespace probe
