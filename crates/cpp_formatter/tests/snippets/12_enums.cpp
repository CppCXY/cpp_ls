enum class Kind { A, B, C };
enum class Wide : unsigned long long { Big = 1ULL << 40, Small = 1 };
enum { PlainA, PlainB };
void enums(Kind k) {
    switch (k) {
    case Kind::A: break;
    case Kind::B: break;
    default: break;
    }
    Wide w = Wide::Big;
    (void)w;
}
