#define DECLARE(name) \
    struct name {     \
        int v;        \
    }
#define CALL(f, ...) f(__VA_ARGS__)
#define EMPTY
DECLARE(A);
DECLARE(B);
int add(int a, int b) { return a + b; }
int call() { return CALL(add, 1, 2) + EMPTY 3; }
