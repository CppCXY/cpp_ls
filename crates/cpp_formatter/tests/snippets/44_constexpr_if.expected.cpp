constexpr int classify(int n) {
    if constexpr (sizeof(int) == 4) {
        return n;
    } else {
        return -n;
    }
}
template <typename T>
constexpr auto describe() {
    if constexpr (std::is_integral_v<T>) { return 1; } else { return 0; }
}
int use() { return classify(1) + describe<int>(); }
