void ranges() {
    std::vector<int> v = {3, 1, 2};
    std::ranges::sort(v);
    auto evens = v | std::views::filter([](int x) { return x % 2 == 0; })
                   | std::views::transform([](int x) { return x * 2; });
    for (int x : evens) { (void)x; }
    auto it = std::ranges::find(v, 2);
    (void)it;
}
