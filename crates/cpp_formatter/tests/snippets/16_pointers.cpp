void pointers(int* p, const int* cp, int** pp, int*& rp) {
    int a = *p;
    int b = **pp;
    int* q = p;
    *q = a + b;
    q = nullptr;
    const int* const ccp = cp;
    (void)ccp; (void)rp;
    int* const* ppc = &q;
    (void)ppc;
    auto fp = [](int* x) -> int* { return x; };
    (void)fp;
}
