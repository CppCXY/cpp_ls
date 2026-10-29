
---

## N. C 支持性测量：一个纯 C 语料工程（2026-10-30）

语料与测量工具的完整说明在 `docs/c-support.md`。这一节只记**还没修的**，以及为什么。

第一次测量（9 个文件、416 个声明、能编译）：

```text
parse    9 errors，集中在 3 个文件（aggregates.c 1、c23.c 4、gnu.c 4）
members  26/50
types    25/33
findings 89 条，其中 assign-type-check 13、undefined-name 12、argument-count 1、redefinition 1
```

### N1. C 工程的头文件搜索用了 C++ 的 include 路径 —— 影响最大，没修

```text
toolchain include paths: […/include/c++ , …/include/c++/x86_64-w64-mingw32 , …/include/c++/backward , …]
```

一个**纯 C** 工程（`compile_commands.json` 里只有 `-std=c17`/`gnu17`/`c23`，没有任何 C++ 文件），
`<stdlib.h>` 却解析到 C++ 的 wrapper `cstdlib`：那里的 `div`/`ldiv`/`lldiv` 只有 `(long,long)` 系列重载，
于是 `div(7, 2)` 报 "is not declared"；`<complex.h>` 的 `creal` 同理。后果不止 4 条 finding ——
`src/library.c` 整个模块（`struct tm`、`div_t`、`struct lconv`、`jmp_buf`）的成员测量都失去了意义，
因为那些声明根本没被读到。

修法方向：**工具链要按文件的语言问**（C 文件问 `gcc`，C++ 文件问 `g++`），或者至少在项目里没有 C++ 文件时选 C 编译器。
`session.toolchain()` 现在是项目级的一个答案，per-file 的 include 搜索已经在 `CompilerConfig::for_file` 里分叉了，
所以这两处要对齐。

### N2. 文件作用域的 `_Static_assert` 被读成调用

块作用域的那条在 c17 夹具里是对的（`tests/checks.rs` 的 `the_c_spellings_…`），文件作用域的三条仍然报
"`_Static_assert` is not declared"。所以坏的只是 `parse_declaration` 在**翻译单元顶层**遇到它时的那条路径。

### N3. 名字查询与索引不一致（第三次出现的"两个读者"）

`functions_selector` 和 `functions_aborts` 在索引里都有事实
（`corpus.h`, `Function`, parameters `(void)` / `(int code)`，用 `CPPLS_C_NAMES` 打印出来的），
`undefined-name` 仍然报"nothing this file can see names it"。§15.11 记录过同一个形状：一条检查用了另一个读者。

### N4. 剩余的解析错误

```text
aggregates.c:90   expected primary expression    复合字面量 + 下标/强转的组合附近
c23.c:32/34       constexpr 聚合、auto 推导
c23.c:85          enum 的固定基类型 `enum E : unsigned char`
gnu.c:36          __typeof__(struct point) origin = {1, 2};
gnu.c:130/136     语句表达式 `({ … })`、计算 goto
```

### N5. 成员/类型清单里剩下的 24 条与 8 条

按形状：`div_t`（typedef 的**匿名** struct）4 条、匿名 union 成员 1 条、`corpus.h` 的 `static inline` 里的
成员访问 3 条、c23 的 `_BitInt` 位域 2 条、`__typeof__` 变量 1 条，其余是 N1/N4 的连带。

### N6. `assign-type-check` 少报

`struct point wrong = 1;`（真正的类型不匹配）**不报** —— C 里没有构造函数，`a_class_can_be_built_from`
答不出 `Some(false)` 就沉默了。方向是对的（宁可少报），但这是能力缺口，不是修好的东西；`docs/c-support.md` §3.1
的测试只钉了"不误报"的那两条。

---
