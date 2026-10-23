void loops(int n) {
    for (int i = 0; i < n; ++i) {
        while (n > 0) {
            --n;
        }
    }
    do {
        --n;
    } while (n > 0);
    for (;;) {
        break;
    }
    for (const auto &x : {1, 2, 3}) {
        (void)x;
    }
}
