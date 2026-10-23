template <typename T, int N=3>
struct Buffer {
    T items[N];
    constexpr int size() const noexcept { return N; }
    [[nodiscard]] constexpr const T& at(int i) const { return items[i]; }
};
template <typename T>
concept Sizeable = requires(T t) { t.size(); };
template <Sizeable T>
void print(const T& v) noexcept(noexcept(v.size())) {
    static_assert(sizeof(T) > 0, "no empty");
    for (int i = 0; i < v.size(); ++i) { (void)i; }
}
