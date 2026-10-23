int sum(const std::vector<int> &v) {
    int total = 0;
    for (auto x : v) total += x;
    if (auto it = v.begin(); it != v.end()) total += *it;
    switch (v.size()) {
    case 0: return 0;
    case 1: { return v[0]; }
    default: break;
    }
    auto f = [&](int a) -> int { return a + total; };
    auto g = [](auto a) { return a; };
    return f(1) + g(2);
}
