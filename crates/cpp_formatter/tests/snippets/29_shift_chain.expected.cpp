int main() {
    auto x = 123;
    char y = 'A';
    printf("x = %d\n", x);
    auto vec = std::vector<int>();
    std::vector<int> anotherVec;
    anotherVec.push_back(1231);

    for (auto x : anotherVec) {
        std::cout << x << std::endl;
    }
    std::map<int, std::string> myMap;
    for (auto it : myMap) {
        std::cout
                << "Key: " << it.first << ", Value: " << it.second << std::endl;
    }
    anotherVec.push_back(123);
    auto x = anotherVec[1];
    auto size = vec.size();
    auto &dd = myMap[1];

    std::sort(vec.begin(), vec.end());
    std::format("x = {}", x);
    std::cout << "x = " << x << std::endl;
    std::cout << std::format(FULL_NAME_FORMAT, full2) << std::endl;
    Name myName("John", "F.", "Doe");
    auto c = std::format("Full name: {}", myName.getFullName(1));
    std::string full2;
    full2.append(myName.getFullName(2));
    std::string_view s = c;

    for (int i = 0; i < full2.size(); ++i) {
        std::cout << full2[i];
    }
    auto x = FULL_NAME_FORMAT;
    std::cout << std::endl;
    STD cout << "Using STD macro for cout" << STD endl;
    std::cout << "Hello, World!" << std::endl;
    std::cout
            << std::format("My name is {}", myName.getFullName(3)) << std::endl;
    int a = 5;
    int b = 3;
    std::cout << a << " + " << b << " = " << (a + b) << std::endl;
    int sum = a + b;
    std::cout
            << "The sum of " << a << " and " << b << " is: " << sum << std::endl;
    std::string full;
    std::cout << "Full name: " << full << std::endl;
    std::cout << "hello world " << std::endl;
    full.size();
    full.length();
    full.size();
    printf("Full name length: %zu\n", full.size());
    printh("123131", 123);

    // printh("1231313")
    // printf("")
    // printf("Full name length using length(): %zu\n", full.length());
    return 0;
}
