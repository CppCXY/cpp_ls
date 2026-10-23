void lambdas() {
    auto a = []{ return 1; };
    auto b = [x](int y) mutable { return x + y; };
    auto c = [&, x](auto&& v) -> decltype(auto) { return v; };
    auto d = [](int a,int b) { return a*b; };
    auto e = [this]() { return 1; };
    auto f = []() noexcept -> int { return 0; };
    int arr[] = {1, 2, 3};
    for (auto& v : arr) { v += 1; }
}
