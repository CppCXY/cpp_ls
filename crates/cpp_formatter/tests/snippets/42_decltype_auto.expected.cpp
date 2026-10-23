auto make(int a) -> decltype(a + 1) { return a + 1; }
decltype(auto) forward(int &a) { return (a); }
auto trailing(int a) -> int { return a; }
template <typename T>
auto deduce(T v) -> decltype(v * 2) { return v * 2; }
void use() {
    decltype(auto) x = forward(*static_cast<int *>(nullptr));
    (void)x;
}
