struct S {
    S& operator=(S&&) noexcept = default;
    S(S&&) noexcept = default;
    S(const S&) = delete;
    S& operator=(const S&) = delete;
    ~S() noexcept {}
    void swap(S&) noexcept;
};
struct T {
    T() : a(0) {}
    int a;
    std::string b;
    std::vector<int> c;
    S s;
};
void member_init() {
    T t;
    (void)t;
    S s1;
    S s2 = std::move(s1);
    (void)s2;
}
