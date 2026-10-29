# C 支持性测量：一个纯 C 工程

> 目的：把"这个服务器读 C 代码读得怎么样"变成**可重复的数字**——每个文件解析是否成功、每个成员访问能不能感知、
> 每个声明的类型对不对。语料是一份**能编译的** C 工程（MinGW gcc 15 + CMake），所以"分析说错了"是分析的缺陷，
> 不是文件的问题。

## 1. 语料

位置：`C:\Users\zc\Desktop\cpp`（`CPPLS_C_CORPUS` 可覆盖），由 `CMakeLists.txt` 用 **MinGW Makefiles** 构建：

```powershell
cmake -S . -B build -G "MinGW Makefiles" -DCMAKE_BUILD_TYPE=Release -DCMAKE_EXPORT_COMPILE_COMMANDS=ON
cmake --build build        # 0 error；只剩一条故意的 -Wdeprecated-declarations（[[deprecated]] 用例）
build\corpus.exe           # 1.4 total=16994915 names=corpus-1.4-enabled
```

| 文件 | 覆盖的语法 | 标准 |
|---|---|---|
| `include/corpus.h` | 共享声明：`struct`/`union`/`enum`、位域、柔性数组、不透明句柄、函数指针 typedef、`static inline`、X-macro | c17 |
| `src/types.c` | 全部算术类型、`_Bool`、`_Complex`、限定符（`const`/`volatile`/`restrict`）、存储类、位域全基类型、匿名 union、枚举（掩码/负值/字符）、`_Atomic`、`limits.h`/`float.h` | c17 |
| `src/aggregates.c` | 指定初始化、复合字面量、多维数组、VLA、柔性数组、嵌套/匿名聚合、自引用 struct、链表、`malloc` 后 `->` | c17 |
| `src/functions.c` | 原型/无原型、K&R 定义、可变参数（`va_list`/`vsnprintf`）、函数指针（含返回函数指针）、`static`/`inline`、递归、`qsort` 回调 | c17 |
| `src/control.c` | if/else-if/悬垂 else、`switch`（穿透/多标签/块/default 居中）、五种循环、`continue`/`break` 嵌套、`goto` 前向/后向、逗号表达式、短路 | c17 |
| `src/preprocessor.c` | 对象/函数/可变参数宏、`#`/`##`、`__VA_OPT__`、X-macro 双向展开、`_Generic`、`_Static_assert`（文件作用域 + 块作用域）、条件编译、预定义宏 | gnu17 |
| `src/gnu.c` | `__attribute__`（声明/参数/成员）、`__typeof__`/`typeof`、语句表达式 `({…})`、case range、`__builtin_*`、`__int128`、`__auto_type`、`__thread`、计算 goto | gnu17 |
| `src/c23.c` | `nullptr`、`constexpr`、`auto` 类型推导、`typeof`/`typeof_unqual`、`[[nodiscard]]`/`[[deprecated]]`/`[[fallthrough]]`、二进制字面量、数字分隔符、`_BitInt(N)`、`enum : unsigned char`、`#embed` | c23 |
| `src/library.c` | 标准库自己的类型：`struct tm`、`div_t`/`ldiv_t`/`lldiv_t`、`FILE*`、`struct lconv`、`jmp_buf`、`signal`、`<math.h>`、`<string.h>` | c17 |
| `src/main.c` | 全部模块的调用者（没有死代码） | c17 |

`build/compile_commands.json` 一并生成，因为这是服务器拿到 `-std=` 和 `-I` 的**最强来源**
（`docs/configuration.md` 的 discovery 顺序：`.cppls.toml` → `compile_commands.json` → `CMakeCache.txt` → 工具链）。

## 2. 测量工具

`crates/cpp_code_analysis/tests/c_corpus.rs`——**一个会话**（索引一次），然后三张表：

```powershell
$env:CPPLS_C_CORPUS = "C:\Users\zc\Desktop\cpp"     # 默认就是它
$env:CPPLS_C_NAMES  = "div,ldiv"                     # 可选：打印索引里这些名字的事实
cargo test --release -p cpp_code_analysis --test c_corpus -- --ignored --nocapture
```

```text
parse    ``session.cook`` 每个文件，``session.diagnostics`` 给出 syntax error 数与声明数（外加每个 code 的前几条 finding）
members  50 条手工清单：``x.member`` / ``x->member`` 的**补全列表**（光标在运算符后面），检查期望的成员在不在
types    33 条手工清单：光标放在声明名内部，``Session::type_at`` 的类型是否包含期望拼写
```

三个设计决定，都是为了**快**和**诚实**：

* **一个会话**。第一版每个文件开一个会话并 `want_cooked_reading` + `advance` 循环——`advance` 走的是**索引队列**
  而不是 cook 队列，于是它空转 20 分钟什么也没答；`diagnostics_rate` 那种"每文件一个会话"的形状在这里要 9 倍索引。
  现在一次 `index_everything`（≈5 s，150 个 MinGW 头）+ 每个文件一次 `Session::cook`（13–36 ms），总计 ≈7 s。
* **成员问补全，类型问 hover**。这是编辑器真实发出的两个请求，所以答对就是用户看到对了。
* **清单而不是比率**。50 条成员 + 33 条类型，每一条都是一个具体构造（`tm_year`、匿名 union 的 `integer`、
  `_BitInt` 位域、`jmp_buf`…），失败时能直接读出"哪种 C 语法没读对"。

## 3. 第一次测量（2026-10-30，本轮开始时）

```text
indexed in 5.66 s
src/aggregates.c   1 error  20 finding  73 decl  26 ms cook  214 line(s)
src/c23.c          4 error   1 finding  15 decl  22 ms        182
src/control.c      0 error   1 finding  41 decl  14 ms        217
src/functions.c    0 error   4 finding  76 decl  18 ms        195
src/gnu.c          4 error  27 finding  21 decl  16 ms        181
src/library.c      0 error   5 finding  55 decl  35 ms        212
src/main.c         0 error   0 finding   7 decl   8 ms         91
src/preprocessor.c 0 error  20 finding  21 decl  14 ms        179
src/types.c        0 error  11 finding 107 decl  36 ms        277
totals: 9 files, 3 with a syntax error, 9 errors, 416 declarations
members: 26/50 answered
types:   25/33 correct
```

### 3.1 已修（本轮）

| 缺陷 | 症状（实测） | 修法 | 测试 |
|---|---|---|---|
| 花括号初始化被判成"不转换" | `struct point origin = {0, 0};` → `assign-type-check`，文件里 13 条 | `InitListExpr` 不是值：初始化列表按元素逐项判断，没有可比的类型 | `checks::a_braced_initialiser_and_a_const_copy_are_not_failed_conversions` |
| `(void)` 被当成一个参数 | `int f(void);` + `f()` → `argument-count` + `undefined-name` 两条 | 两个数参数的地方都加一条：只有一个无名 `void` = 零个参数 | `checks::the_c_spellings_void_restrict_and_static_assert_produce_no_finding` |
| `restrict` 被当成声明名 | `int *restrict left` → `\`restrict\` is already declared` | `*` 后面的限定符位置接受 `restrict`（按 `Feature::Restrict`，C++ 里它仍是普通名字） | 同上 |

### 3.2 未修，已定位到行（下一轮的工作清单）

```text
① C 工程的头文件搜索用的是 C++ 工具链的 include 路径
   toolchain include paths: […/include/c++ , …/include/c++/x86_64-w64-mingw32 , …/include/c++/backward , …]
   → <stdlib.h> 解析到 C++ 的 wrapper（cstdlib），于是 div/ldiv/lldiv 只剩 (long,long) 重载，
     `div(7, 2)` 报 "is not declared"；<complex.h> 的 creal 同理。
   影响 4 条 finding，并且让 library.c 的成员测量失去意义（库类型根本没读到）。

② 文件作用域的 _Static_assert 被读成调用
   preprocessor.c:78-80 三条 `_Static_assert(…);` → "`_Static_assert` is not declared"。
   块作用域的那条（line 85）在 c17 夹具里是对的——所以这是**文件作用域**的那条路径。

③ 名字查询与索引不一致（"两个读者"）
   functions_selector / functions_aborts 都在索引里（corpus.h，Function，(void)/(int code)），
   但 undefined-name 仍然报"not declared"。§15.11 记录过同类形状。

④ 解析错误 9 条，集中在三个文件：
   aggregates.c:90   expected primary expression          （复合字面量/数组字面量附近）
   c23.c:32/34/85    constexpr 聚合、auto、enum : 基类型
   gnu.c:36/130/136  __typeof__(struct point) origin = {1,2};、语句表达式、计算 goto

⑤ 成员 24 条 / 类型 8 条缺失，按形状分组：
   div_t（typedef 的匿名 struct）4 条、匿名 union 成员 1 条、corpus.h 的 static inline 里的成员访问 3 条、
   c23.c 的 _BitInt 位域 2 条、gnu.c 的 __typeof__ 变量 1 条，其余是上面 ①④ 的连带。
```

## 4. 第二次测量：findings 65 → 18

又修了三条，每条都先看着它失败：

| 缺陷 | 症状（实测） | 修法 |
|---|---|---|
| **解析失败的文件仍然产出语义诊断** | `__typeof__(struct point) origin = {1,2};` 一条读不懂，同一文件里**每个调用、每个成员**都报 "not declared"（gnu.c 20 条） | `check::run` 先跑读**文本**的规则（include 找不到、宏重定义、宏括号），文件有语法错误就到此为止：解析错误由它自己的通道发，派生出来的断言都是猜的 |
| `_Static_assert(…)` / `_Generic(…)` 被当成调用 | 11 条 `undefined-name`，全部来自**被内联进来的头文件**（那里解析时没有 C11 特性） | `undefined_name` 的调用侧：C 的关键字构造和 `__builtin_*` 永远不是"未声明的名字" |
| `macro_parentheses` 注册了两次 | preprocessor.c 的 5 条变成 10 条 | 它读宏事实而不是树，移到读文本的那一组，只跑一次 |

```text
                 findings   parse errors   members   types
第一次              89          9            26/50     25/33
修 braced-init      65          9            26/50     25/33
修 (void)/restrict  65          9            26/50     25/33
第二轮              18          9            26/50     25/33
```

探针现在打印**命中点前后 48 字节的渲染文本**（cook 把渲染压成一行，行号没有意义），所以剩下 18 条逐条定位得到：

```text
library.c       5  div/ldiv/lldiv/labs/llabs    ← ①：<stdlib.h> 解析到 C++ 的 cstdlib
types.c         2  creal/cimag                  ← ①：<complex.h> 同理
aggregates.c    1  parse error: NULL → __null   ← ①：libstdc++ 的 NULL 拼写
functions.c     3  functions_selector/functions_aborts（索引里其实有）+ sum 的 assign-type-check
preprocessor.c  6  5 条 macro-parentheses 误报 + 1 条语句表达式里同名局部变量的 redefinition
types.c         2  匿名 union 成员 line.integer / static 函数参数上的 restrict
```

也就是说：**18 条里有 8 条只有 ① 一个根因**（C 工程用了 C++ 的头文件搜索路径），而那一条同时是
`aggregates.c` 那条解析错误的根因。

## 5. 怎么再跑一次

```powershell
Remove-Item -Recurse -Force C:\Users\zc\Desktop\cpp\.cppls   # 冷缓存
cargo test --release -p cpp_code_analysis --test c_corpus -- --ignored --nocapture
$env:CPPLS_C_ALL = "1"      # 不截断 finding 列表
$env:CPPLS_C_NAMES = "div"  # 打印索引里这个名字的事实
```

改了语料之后先 `cmake --build build`——语料编译不过的时候，"分析报错"就不再是缺陷，而是预期。

## 6. 第三轮之后：findings 清零

```text
                 findings   parse errors   members   types
第二轮              18          9            30/50     25/33
第三轮               0          6            31/50     25/33
```

九条 finding 全部修掉，每条都有先看着它失败的测试：

| 缺陷 | 症状 | 修法 |
|---|---|---|
| `_Generic` 不是 `CppExpr` | `strlen(TYPE_NAME(x))` 的实参计数是 **0**，报"参数太少" | 加 `CppSyntaxKind::GenericSelectionExpr` 和它的 AST 类型，`CppCallExpr::get_args` 才看得见它 |
| `T * T;` 被当成声明 | `({ int s = (v); s * s; })` 用一次就报"`s` 重复声明" | `redefinition` 读声明符（`*s`）和它前面的名字，同一个拼写就是乘法 |
| C99 的 `restrict` 没有语法规则 | `int *restrict left` 里 `restrict` 变成参数名，两个参数撞车 | `eat_cv_qualifiers` 加一条 `Feature::Restrict` 的分支，产出 `RestrictQual` |
| 匿名 union 的成员无处可去 | `struct line` 只有 4 个成员而不是 6 个 | 没有名字、没有声明符、又在另一个类体里的类体，其成员绑到外层类的作用域 |
| 宏的替换表范围越界 | `macro-parentheses` 拿格式字符串里的 `%` 报错 | `body_range_of` 取最后一个*显著* token；没有替换表的宏（`#define GUARDED_H`）保持旧答案，因为宏表和 include 环境必须一致 |
| `&&label` / `goto *` | gnu.c 4 条解析错误里的 2 条 | 新的 `Feature::LabelsAsValues`，主表达式一条 `&&` + 名字的分支，`goto` 一条 `*` 表达式的分支 |
| 数组类型的复合字面量 | `(const char[]){"compound"}` 报 `expected primary expression` | `an_operand_follows_the_parentheses` 认 `{`；**不能**放进 `starts_an_operand`（那会让 `Vec<int[4]>{}` 的 `<` 变成比较） |
| `sizeof(union { int i; double d; })` | 类型里的成员被判"声明了没用过" | `unused` 看最内层作用域：类作用域里的声明是成员，不是局部变量 |

剩下的 6 条解析错误：`c23.c` 4 条（`_BitInt(37)`、`typeof(struct point)`、`[[fallthrough]]` 的位置、`constexpr`），
`gnu.c` 2 条（`__typeof__(struct point)`、case range 剩下的形状）。见 `docs/remaining.md` §N10。

**下一轮第一件事**：`aggregates.c` 的 11 行成员检查现在全是 `0 offered` —— 这个文件刚刚才解析成功，这是第一次真正看它；
先确认探针的 needle→offset 映射，再看 `completions` 对一个"头文件里的结构体"的局部变量回答什么。
