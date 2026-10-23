#define SHORT(x) \
    (x)
#define MISALIGNED(a, b)                 \
    do {                                 \
        (void)(a);                       \
        (void)(b);                       \
    } while (0)
#define NO_ALIGN \
    int x
