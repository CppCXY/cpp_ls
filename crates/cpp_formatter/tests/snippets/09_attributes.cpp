[[nodiscard]] int f();
[[maybe_unused]] static int g = 0;
[[deprecated("use h")]] void old();
struct [[nodiscard]] S { [[no_unique_address]] int x; };
void attrs() {
    [[maybe_unused]] int local = 1;
    switch (g) {
    case 1: [[fallthrough]];
    case 2: break;
    default: break;
    }
    if ([[maybe_unused]] auto v = f(); v > 0) { (void)v; }
}
