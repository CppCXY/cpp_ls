constexpr int square(int x) { return x * x; }
consteval int cubed(int x) { return x * x * x; }
constinit int global = 4;
constexpr int kValue = square(3) + cubed(2);
static_assert(kValue > 0);
struct S {
    int a;
    constexpr S(int v) : a(v) {}
    constexpr bool operator==(const S& o) const { return a == o.a; }
};
constexpr S s1{1};
static_assert(s1 == S{1});
