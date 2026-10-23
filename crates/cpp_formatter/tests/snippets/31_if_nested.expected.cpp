#define FEATURE 1
#if defined(FEATURE) && FEATURE > 0
#if FEATURE == 1
int one = 1;
#elif FEATURE == 2
int two = 2;
#else
int other = 3;
#endif
#endif
#ifdef __cplusplus
extern "C" {
#endif
    int c_function();
#ifdef __cplusplus
}
#endif
