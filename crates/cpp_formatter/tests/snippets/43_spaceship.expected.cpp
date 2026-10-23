struct Version {
    int major;
    int minor;
    auto operator<=>(const Version &) const = default;
    bool operator==(const Version &) const = default;
};
int compare(const Version &a, const Version &b) {
    if (a < b) { return -1; }
    if (a > b) { return 1; }
    return 0;
}
