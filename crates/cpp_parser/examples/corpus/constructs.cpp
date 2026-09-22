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

// --- namespaces ------------------------------------------------------------
namespace outer::inner {
int nested = 1;
}  // namespace outer::inner

namespace alias = outer::inner;

}  // namespace probe
