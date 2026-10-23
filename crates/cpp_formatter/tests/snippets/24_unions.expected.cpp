union Value {
    int i;
    float f;
    char bytes[4];
};
struct Tagged {
    enum { Int, Float } kind;
    union {
        int i;
        float f;
    };
};
void unions() {
    Value v;
    v.i = 1;
    Tagged t;
    t.kind = Tagged::Int;
    t.i = 2;
    (void)v; (void)t;
}
