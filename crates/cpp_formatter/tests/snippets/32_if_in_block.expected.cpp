struct Config {
#if defined(_WIN32)
    int win;
#elif defined(__linux__)
    int linux;
#else
    int other;
#endif
    int common;
};
int compute(int a) {
#if DEBUG
    printf("%d\n", a);
#endif
    return a * 2;
}
