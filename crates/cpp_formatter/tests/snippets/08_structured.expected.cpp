struct Point { int x; int y; };
void structured() {
    Point p{1, 2};
    auto [a, b] = p;
    auto &[ra, rb] = p;
    const auto [ca, cb] = p;
    (void)a; (void)b; (void)ra; (void)rb; (void)ca; (void)cb;
    if (auto [ok, value] = std::make_pair(true, 3); ok) { (void)value; }
    int matrix[2][2] = {{1, 2}, {3, 4}};
    auto [first, second] = matrix[0];
    (void)first; (void)second;
}
