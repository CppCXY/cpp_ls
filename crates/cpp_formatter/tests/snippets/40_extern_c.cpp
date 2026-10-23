#ifdef __cplusplus
extern "C" {
#endif
int c_api(int a);
void c_api2(void);
#ifdef __cplusplus
}
#endif
int after_guard() { return 0; }
