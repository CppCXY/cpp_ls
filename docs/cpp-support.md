# C++ 支持性测量：`test_cpp`

> 目的：把"这个服务器读 C++ 代码读得怎么样"变成可重复的数字。语料是 `C:\Users\zc\Desktop\test_cpp`：16 个翻译
> 单元，每个覆盖一块语言特性，用 **MinGW g++ 15 + CMake 以 C++23 构建**（0 error），所以"分析报错"是分析的缺陷，
> 不是文件的问题。C 的同类测量在 [`c-support.md`](c-support.md)，语料在 `Desktop\c-project`。

## 1. 语料与探针

```powershell
cmake -S . -B build -G "MinGW Makefiles" -DCMAKE_BUILD_TYPE=Release -DCMAKE_EXPORT_COMPILE_COMMANDS=ON
cmake --build build                       # 0 error
cargo test --release -p cpp_code_analysis --test cpp_corpus -- --ignored --nocapture
```

`crates/cpp_code_analysis/tests/cpp_corpus.rs` 打开这个工程、索引一次、逐个文件 cook，然后三张表：

```text
parse    每个文件的语法错误数、finding 数、声明数
members  8 条手工清单：`x.member` / `x->member` 的补全列表里有没有期望的成员
types    4 条手工清单：声明名上的 `type_at` 是否给出期望的类型
```

文件与覆盖范围的表在语料自己的 `README.md`。

## 2. 第一次测量（2026-10-30，冷缓存）

```text
16 file(s), 640 declarations, 6 with a syntax error, 10 errors
members  6/8
types    3/4
```

### 2.1 语法错误（10 条，5 组）

| 位置 | 构造 | 现象 |
|---|---|---|
| `classes.cpp:64` | `[[likely]] case 2:` | `expected primary expression`。C++ 的 `labeled-statement` 允许属性在 `case` 前（`[stmt.label]`），g++ 接受，解析器不接受；第 67 行 `default:` 是连带错误 |
| `classes.cpp:162` | `typeid(*polymorphic).name()` | `expected ';' after expression`。`typeid` 带解引用表达式作载荷时读错 |
| `lambdas.cpp:46` | `[... copies = values]` 包捕获（C++20） | `expected primary expression`。lambda 的捕获列表不接受包展开 |
| `preprocessor.cpp:96` | `TEST_CPP_MAKE_NAME(test)`，宏体是 `TEST_CPP_CAT(prefix, _value)`，再展开成 `a##b` | `expected ), but get identifier`。`##` 的粘接仍坏（与 C 语料 §N9 同源） |
| `ranges.cpp:39,51,59` / `strings.cpp:87` | `for (int value : std::views::iota(1, 6))`、`for (… : map \| std::views::values)`、`for (… : rows \| std::views::join)`、`for (… : std::string_view{text} \| std::views::split(','))` | `expected primary expression`。range-for 的 range 表达式以 `views::` 的调用/管道开头时读不出 `:` 右侧 |

### 2.2 误报（findings，按根因分组）

| 规则 | 数量 | 例子 | 根因 |
|---|---|---|---|
| `undefined-name` | 22 | `factorial`、`sequences`、`exceptions`、`pointers`（都在**匿名 namespace** 里定义，在同一个文件的后面调用） | 匿名 namespace 的成员对索引里的名字查询不可见——同一文件里的调用被判"没有声明" |
| `else-after-return` | 5 | `if constexpr (…) { return …; } else { … }`、`if consteval { … } else { … }` | 编译期分支的 `else` 不是"嵌套了不必要的 else"；规则应当跳过 `if constexpr`/`if consteval` |
| `undefined-field` | 3 | 类模板 `Task` 的移动构造里 `std::exchange(other.handle_, …)` | 依赖类型的成员访问（`other` 的类型依赖 `T`）解析不出成员 |
| `assign-type-check` | 2 | `std::any any = 1;` | 声明类型被读成 `void`，于是"1 不能转换成 void" |
| `argument-count` | 1 | `sum_head(rest...)` | 包展开被当成"实参太少" |
| `use-override` | 1 | `ThreeD::sum`（**写了** `override`） | 误报，且 range 落在 `static_assert` 上——检查层报告的 range 与文件坐标不一致（§N7 记过同类问题） |
| `unused` | 2 | `Guard guard(counter);`、`std::shared_lock<std::shared_mutex> reader(...)` | RAII 对象本来就"不用"；`unused` 应豁免有非平凡析构的类型 |

### 2.3 成员 / 类型

```text
members  PASS derived.name()      PASS clone->describe()   FAIL unique->identifier()（0 offered）
         PASS vector.push_back    PASS deque.push_front     PASS accumulator.add(3)
         FAIL task.run()（0 offered）  PASS slot.get()
types    PASS derived → Derived   PASS slot → Box<int>
         FAIL task → unknown（`auto task = doubled();` 的类型推不出 `Task<int>`）
         PASS accumulator → Accumulator<int>
```

两条失败指向同一类问题：`std::unique_ptr<Resource>` 的成员查询，以及**协程函数的返回类型**（`Task<int> doubled()`）
没有进入 `auto` 的推导。

## 3. 下一轮的顺序

1. **range-for 的 range 表达式**（4 条错误，一行代码）——`for (x : expr)` 里 `expr` 以 `views::` 名称/管道/带花括号的
   临时量开头时 `:` 右侧读不出来，是语法层的一个入口判断。
2. **匿名 namespace 的可见性**（22 条 finding，影响最大）——同一文件、同一作用域的调用被判"未声明"，
   查法是 `CPPLS_C_NAMES=<name>` 看索引里的事实，再看名字查询的可见性判断是否把匿名 namespace 的作用域排除了。
3. `[[likely]] case`、`typeid(expr)`、包捕获、`##` 粘接（各一条，语法层各自独立）。
4. `if constexpr` / `if consteval` 的 `else-after-return`、RAII 的 `unused`、依赖类型的成员访问、`std::any` 的类型。
5. `auto` 对协程/模板函数返回类型的推导，以及 `unique_ptr` 的成员查询。
