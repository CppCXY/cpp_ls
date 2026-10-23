struct Error : std::runtime_error {
    using std::runtime_error::runtime_error;
};
void throwing() {
    try {
        throw Error{"bad"};
    } catch (const Error& e) {
        (void)e;
    } catch (...) {
        throw;
    }
    try { throwing(); } catch (const std::exception& e) { (void)e.what(); }
}
