using Alias = std::basic_string<char, std::char_traits<char>, std::allocator<char>>;
using Long = Very<Type, With, Many, Arguments, That, Goes, On>;
const char *text = "a // not a comment";
const char *raw = R"(raw " string)";
int calc(int a) { return a > 0 ? a * 2 : a < -10 ? a / 2 : -a; }
struct S { int a; int b; };
S make() { return S{1, 2}; }
