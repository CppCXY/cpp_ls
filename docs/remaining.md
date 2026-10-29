
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

### N12. 匿名类型的成员还差「查询路径」这一半

```text
                 findings   parse errors   members   types
第四轮               0           0           45/50     29/33
第五轮               0           0           46/50     30/34
```

这一轮修的两条：

* **`typeof(type-id)`** —— `typeof(struct point) origin` 的事实类型现在是 `point`（`origin.x` 由 FAIL 变 PASS）。
* **无名类型的键** —— `struct { unsigned three : 3; } bits = {5};` 的事实现在是 `type bits`，成员 `three` 的 scope 是
  `Some("bits")`，作用域树里的类作用域也取了声明符的名字（`an_unnamed_class_takes_the_name_of_the_variable_it_declares`
  原来是断言"无名类是死路"，那条断言在"没人能访问到它"的年代是对的，现在改掉了）。

**还差的是查询路径**：`type_at(bits)` 仍然答**空类型**（`CPPLS_C_FACTS` 显示事实是 `bits`，但 `type_at` 从渲染树里
重新推导声明类型时没带声明符名字，于是走 `an_unnamed_body` 那条空拼写分支）。所以 `bits.three` /
`anonymous_union.integer` 还是 0 offered。下一轮第一件事：`sema::types::type_of_declaration_named` 的**调用方**在
查询路径上是哪个、为什么不传 `declared_name`（事实路径传了，见 `sema/declarations.rs:279`）。

剩下 4 行成员 + 4 行类型：

```text
types.c  anonymous_union.integer   同上（查询路径）
c23.c    bits.three / bits.signed_wide  同上
gnu.c    other_pointer->x          `typeof(other) *other_pointer` —— expression 载荷的 typeof（需要模型）
gnu.c    value                     `__auto_type` → 应推断成 initialiser 的类型
c23.c    copy                      `typeof(value)` —— 同上，expression 载荷
functions.c operation / compare    函数指针局部变量（`int (*operation)(int, int)`）→ `<no type>`
```

### N13. 类型推断查询路径修完之后的 3 项

```text
                 findings   parse errors   members   types
第五轮               0           0           46/50     30/34
第六轮               0           0           49/50     32/34
```

这一轮修的（都在**查询路径**，不是解析）：

* **`type_of_declaration_named` 的调用方少传了声明名** —— 事实里是 `type bits`，查询却答空。补上之后
  `bits.three` / `anonymous_union.integer` 从 0 offered 变 PASS。
* **`__auto_type` 是 GNU 的 `auto`** —— 两处门控（`deduce.rs` 的占位符判断、`writes_auto`/`auto_substituted` 的按词扫描）
  只认 `auto`，于是 `__auto_type value = 41;` 的事实是 `int`、查询却答 `__auto_type`。
* **`typeof(表达式)` 的载荷** —— `typeof(value) copy` 里只有语义层能回答，`declared_type` 现在把载荷交给
  `type_of_expression`；`copy : int` 与 `other_pointer->x`（`typeof(other) *` → `point*`）同时转绿。
* **`typeof(nullptr)` 不是 type-id** —— `starts_a_type_id` 把 `nullptr` 当类型会让 `typeof(nullptr) nothing = nullptr;`
  报错（加规则时新引入的唯一一条错误，已修）。

**一处取舍，写在这里免得下次重犯**：把"无名类型的键"放进 `read_specifiers_named`（事实也带名字）会让
`static const struct { … } priority[]` 的 hover 变成 `priority[] priority;`——一条 handshake 测试立刻变红。
所以键只加在**局部变量的查询路径**上：`bits`/`anonymous_union` 能查成员，文件作用域的 hover 保持"声明原文"。

剩下 3 项：

```text
c23.c  bits.signed_wide   struct bits 的第二个位域 `_BitInt(13) signed_wide : 13;` 没被记录
                           —— `_BitInt` 打头的声明还需要"可以是声明"的锚点，我试过，锚点会让
                           `_BitInt(37) wide = (_BitInt(37))1 << 30;` 的初始化器失败，所以撤回了
functions.c operation / compare  `binary_op operation = add;`（函数指针 typedef）→ `<no type>`；
                           fixture 里能过，只在"typedef 在头文件里"时不复现，下一轮第一步查这个差别
```

### N14. 第六轮结束时剩下的 3 项（都试过，都没成）

```text
                 findings   parse errors   members   types
第六轮(结束)        0           0           49/50     32/34
```

1. **`c23.c bits.signed_wide`** —— `struct bits` 的第二个位域 `_BitInt(13) signed_wide : 13;` 没有被记录。
   原因是 `_BitInt(` 打头的语句不被当成声明（`starts_declaration` 只看首 token，`_BitInt` 是个 identifier）。
   这一轮试了带前瞻的锚点（跨过宽度括号，看 `)` 后面是不是 identifier），**锚点生效但 `_BitInt(37) wide =
   (_BitInt(37))1 << 30;` 的初始化器随即解析失败**，于是撤回了——`git checkout` 恢复了 `decls.rs`。
   下一轮从那条 fixture 开始：`assert_all_parse` 一条 `void f(void) { _BitInt(37) wide = (_BitInt(37))1 << 30; }`，
   先看清错误落在哪，再决定锚点要不要带"初始化器里的 cast 也认 `_BitInt`"。

2. **`functions.c operation` / `compare`** —— 语料里答 `<no type>`。但**同样的形状放进两文件 fixture 是过的**：
   `binary_op operation = add;` 在 fixture 里答 `int (*)(int, int)`（引擎会把函数指针 typedef 展开，这比别名名字更好）。
   所以差别在 `functions.c` 自己的内容里，下一轮第一步：`CPPLS_C_FACTS=src/functions.c` 已经看到事实是
   `type binary_op` ✓，接着在 `type_at` 那条路上打断点（`declared_type` → `NamedDeclaration::type_of`），看它为什么
   连语法都读不出来。

新加的那条测试（`a_function_pointer_typedef_from_a_header`）钉住的是引擎的**展开**行为，刻意只断言形状
（`contains("int") && contains("(int, int)")`）——拼写的规范化（`const` 丢没丢、`*` 在不在）不是这个测试要管的事。
