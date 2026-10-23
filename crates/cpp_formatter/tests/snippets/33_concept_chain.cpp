template <typename T>
concept Iterable = requires(T t) {
    t.begin();
    t.end();
};
template <typename T>
concept SizedIterable = Iterable<T> && requires(T t) {
    { t.size() } -> std::convertible_to<std::size_t>;
};
template <typename T>
    requires SizedIterable<T>
void walk(const T& t) {
    for (auto it = t.begin(); it != t.end(); ++it) { (void)it; }
}
template <SizedIterable T>
std::size_t count(const T& t) { return t.size(); }
