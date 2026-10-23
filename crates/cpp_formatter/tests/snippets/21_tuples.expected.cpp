void tuples() {
    auto t = std::make_tuple(1, 2.0, "three");
    auto [a, b, c] = t;
    (void)a; (void)b; (void)c;
    std::apply([](auto &&...args) { (void)sizeof...(args); }, t);
    auto truncated = std::tuple_cat(std::make_tuple(1), std::make_tuple(2));
    (void)truncated;
    std::pair<int, int> p{1, 2};
    auto &[x, y] = p;
    x = y;
}
