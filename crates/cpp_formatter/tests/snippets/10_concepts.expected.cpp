template <typename T>
concept Numeric = std::is_arithmetic_v<T>;
template <Numeric T>
constexpr T twice(T v) { return v + v; }
template <typename T>
    requires Numeric<T> && (sizeof(T) > 1)
T four(T v) { return twice(twice(v)); }
template <typename T>
concept Container = requires(T c) {
    c.begin();
    c.end();
    { c.size() } -> std::convertible_to<size_t>;
};
struct S { ~S() requires true = default; };
