template <typename... Args>
void variadic(Args&&... args) { (void)sizeof...(args); }
template <typename... Args>
auto forward_all(Args&&... args) -> decltype(auto) {
    return std::make_tuple(std::forward<Args>(args)...);
}
template <typename T, template <typename> class Tmpl>
struct Rebind { using type = Tmpl<T>; };
template <typename T, typename = std::enable_if_t<std::is_integral_v<T>>>
T pick(T a, T b) { return a < b ? a : b; }
template <>
struct Rebind<int, std::allocator> { using type = int; };
