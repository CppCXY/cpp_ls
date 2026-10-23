#define TWICE(x) ((x) + (x))
#define LONG_MACRO(a, b) \
    do { (void)(a); (void)(b); } while (0)
#if defined(FOO)
int foo = 1;
#else
int foo = 2;
#endif
enum class Color : unsigned char { Red, Green, Blue };
enum Plain { A, B };
int use() { return TWICE(foo) + static_cast<int>(Color::Red) + A + B; }
