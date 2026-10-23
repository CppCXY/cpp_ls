#define SQUARE(x) ((x) * (x))
#define LONG_MACRO(a, b) \
    do { \
        (void)(a); \
        (void)(b); \
    } while (0)
#define VARIADIC(fmt, ...) printf(fmt, ##__VA_ARGS__)
#define EMPTY
#define MULTI(a) \
    if (a) { \
        (void)0; \
    } else { \
        (void)1; \
    }
void macros() {
    VARIADIC("%d\n", SQUARE(2));
    LONG_MACRO(1, 2);
}
