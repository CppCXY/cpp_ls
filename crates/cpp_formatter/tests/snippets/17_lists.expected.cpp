void lists() {
    std::vector<int> a = {1, 2, 3};
    std::vector<int> b{4, 5, 6};
    std::map<int, int> m = {{1, 2}, {3, 4}};
    std::vector<std::vector<int>> nested = {{1}, {2, 3}};
    auto c = std::vector<int>{7, 8};
    (void)a; (void)b; (void)m; (void)nested; (void)c;
    int arr[3] = {9, 10, 11};
    (void)arr;
}
