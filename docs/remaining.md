
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

### N7. 第二轮修完之后的 18 条（逐条定位）

`docs/c-support.md` §4 有全表。要点：**8 条只有 N1 一个根因**（`div`/`ldiv`/`lldiv`/`labs`/`llabs`/`creal`/`cimag`，
以及 `aggregates.c` 那条解析错误——`NULL` 被 C++ 的 `stddef.h` 展开成 `__null`），其余是：两个读者不一致
（N3）、`macro_parentheses` 在五个人为构造上误报、语句表达式里同名局部变量被判 redefinition、匿名 union 成员、
`static` 函数参数上的 `restrict`。

另外两件这一轮顺带发现的：

* **c23.c 的解析错误**是 `0b1011'0110` / `1'000'000'000`（二进制字面量 + 数字分隔符）和 `[[fallthrough]]`：
  文件在 `compile_commands.json` 里是 `-std=c23`，但 cook 出来的渲染里这些特性没有打开。
* **检查层报告的 range 有时是文件的、有时是渲染的**（探针第一版按渲染切片，越界 panic 了）。这不是缺陷清单里的
  项目，但它是"诊断落在哪一行"这个问题至今没有单一答案的证据，值得单独查一次。

---

### N8. 第三轮之后剩下的 9 条（每条都定位到文本）

`18 → 9`，parse error 9 条不变，成员 26/50 → 30/50。剩下的每一条都能在文件里指出来：

```text
preprocessor.c  [argument-count] 5   strlen(CORPUS_TYPE_NAME(x)) —— 实参计数把宏展开里的逗号算成了外层调用的
preprocessor.c  [macro-parentheses] 1  CORPUS_DEFINE_POINT 的 body 以续行反斜杠开头，形状判断先 trim 掉它就好
preprocessor.c  [redefinition] 1     语句表达式里的局部变量（CORPUS_SQUARE 展开两次）被当成同一作用域重定义
types.c         [redefinition] 1     int *restrict right —— 带 include 的文件里第二个参数的 restrict 仍是名字
types.c         [undefined-field] 1  匿名 union 的成员 line.integer
aggregates.c    1 parse error        (const char[]){"compound literal"} 数组
c23.c           4 parse errors       _BitInt(37)、typeof(struct point)、[[fallthrough]] 前后
gnu.c           4 parse errors       &&label、goto *table[i]、__typeof__(struct point)、case -5 ... -1
```

其中 `restrict` 那一条**在最小 fixture 里不复现**（`c17` 下 `static int f(int *restrict a, int *restrict b)` 是干净的），
说明它依赖这个文件的其他内容——下一轮第一件要查的事，查法是用 `CPPLS_C_SHOW` 把渲染里那段 `restrict signature`
打印出来，看 cook 到底把它变成了什么。

### N9. X-macro 的 `##` 粘接是坏的（`#[ignore]` 的测试）

`an_x_macro_pastes_its_names`：`#define LIST(X) X(red, 0) X(green, 1)`、`#define ENTRY(n, v) COLOUR_##n = value,`
用在 `enum e { LIST(ENTRY) COUNT };` 里，渲染中 `COLOUR_##n` **原样留着 `##`**，于是解析报 `;` 缺失。

它和 `body_range` 的宽度无关（把宽度改回最宽也一样失败），所以不是这一轮的回归；根因在 `paste_at` 的位置记账：
`parse_body` 记的是 `body.tokens` 的下标，`substitute` 用的是 `significant_tokens` 的下标，中间隔着 `remap_positions`
——下一个 round 从那里开始，第一条命令是给 `substitute` 打印 `paste_at` 与 `body_tokens`。

---

### N10. 只剩 9 条 parse error（findings 已经清零）

`18 → 0`，成员 26/50 → 31/50。剩下的全是**解析器还不认识的构造**，每条都带文件与原文：

```text
aggregates.c  1   (const char []){"compound literal"}          复合字面量 + 数组类型
c23.c         4   _BitInt(37)、typeof(struct point)、[[fallthrough]] 位置、constexpr
gnu.c         4   &&state_zero、goto *TABLE[state]、__typeof__(struct point)、case -5 ... -1
```

顺序建议（每条都是独立的一小块语法）：

1. **`&&label` + `goto *expr`**（gnu.c 两条）——GNU 的计算跳转，表达式层一个 `CppTokenKind::AmpAmp` 后跟 label 的
   特例，加一条 `goto` 的 `*` 形式。
2. **`case -5 ... -1`**（gnu.c）——case 范围，`...` 在常量表达式之间。
3. **`typeof(struct point)`**（c23.c/gnu.c）——`typeof` 后面接 type-id 而不是表达式。
4. **`_BitInt(37)`**（c23.c）——带宽度参数的内建类型。
5. **`(const char []){…}`**（aggregates.c）——复合字面量的数组拼写。
6. **`[[fallthrough]]` 的位置**（c23.c）——属性在 `case` 前的语句位置。

---

### N11. 解析错误清零，剩下的是**类型推断**

```text
                 findings   parse errors   members   types
第三轮               0           6           45/50     29/33
第四轮               0           0           45/50     29/33
```

九条解析错误全部修掉（`508` 条声明，9 个文件零错误）：

| 构造 | 文件 | 修法 |
|---|---|---|
| `&&label`、`goto *table[i]` | gnu.c | `Feature::LabelsAsValues`：主表达式一条 `&&` + 名字；`goto` 一条 `*` 表达式 |
| `case -5 ... -1` | gnu.c | 随 `&&label` 一起通过（左端是一元负号常量） |
| `typeof(struct point)` / `__typeof__(const int)` | c23.c、gnu.c | specifier 的 `decltype` 分支在 `Feature::Typeof` 下接受 **type-id** 载荷 |
| `typeof_unqual(x)` | c23.c | lexer 的 `typeof` 拼写表 |
| `_BitInt(37)` | c23.c | specifier 里 `_BitInt` + `(` 宽度 `)`，按 `Feature::BitInt` 门控 |
| `(const char[]){"compound"}` | aggregates.c | `an_operand_follows_the_parentheses` 认 `{`（不能放 `starts_an_operand`） |
| `sizeof(union { … })` 的假 unused | aggregates.c | `unused` 看最内层作用域（类作用域=成员） |

**探针本身的一个大 bug**：`renderings` 缓存按权重有界，先 cook 的文件会被后面的挤掉，于是成员/类型检查读到的是"没渲染"而不是"引擎答不出"——
`aggregates.c`、`gnu.c` 的每一行都 `0 offered`，而最后 cook 的 `types.c` 全过。修法：每问一行前若 `view` 为空就重新 cook
（`member_labels` 调用点）。**45/50 与 29/33 是修了这个之后才看到的真实数字**（之前是 31/50、25/33）。

剩下的 5 个成员 / 4 个类型全是**类型推断**，不是解析：

```text
types.c   anonymous_union.integer   无名的 union 类型局部变量（`} anonymous_union = {0};`）的类型无从命名
gnu.c     other_pointer->x          `typeof(other) *other_pointer` —— typeof 的**推断**
c23.c     origin.x                  `typeof(struct point) origin` —— 同上
c23.c     bits.three / bits.signed_wide  `struct bits` 的位域（含 `_BitInt(37) wide;`）成员没列出
functions.c operation / compare     函数指针局部变量（`int (*operation)(int, int)`）→ `<no type>`
gnu.c     value                     `__auto_type` → 应推断成 `int`，现在是字面拼写
c23.c     copy                      `typeof(value)` → 应是 `int`
```
