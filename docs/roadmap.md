# 路线图：做什么，以及每一步的思路

**这一份是队列**——做什么、为什么这么做、怎么知道做对了、哪里会踩坑。它叫 `roadmap.md` 而不是
"next-steps"：它是**活的队列**，每做完一条就在这里改一条，而不是一次性的交接件。
四份规格文档不要重复它们的内容：

| 文档 | 管什么 |
|---|---|
| [`index-design.md`](index-design.md) | 索引与语义层的**设计**：事实层、缓存键、查询清单、三条不变量。"现在答不了什么"那张表在它的末尾 |
| [`grammar-gaps.md`](grammar-gaps.md) | parser 的**读法登记处**：每条缺口的四要素、已修/待办、以及三十六条**维护约定**（大半的坑在那里） |
| [`std-library.md`](std-library.md) | **标准库这条线**：为什么它是最好的探针、P0–P4 的计划、每一次普查的数字 |
| [`parser-assessment.md`](parser-assessment.md) | **parser 够不够好的判断**（一页纸）：三个语料的实测、"好在哪 / 不够好在哪"、以及对宏处理意味着什么 |

本文档只回答一件事：**下一步做什么，以及做的时候脑子里该有什么。**

---

## 0. 三分钟进入状态

**是什么**：`cpp_ls` 是一个 C++ 语言服务器的内核，两个 crate——
`cpp_parser`（无损 CST、容错、宏表、外部符号表接口）与 `cpp_code_analysis`（预处理、每文件事实、缓存、跨文件查询）。
**驱动层已经落地**（`session.rs`：开项目 → 发现工具链 → 惰性索引 → 接 `didOpen`/`didChange` → 查询），
**还没有语言服务器二进制**——也就是把 `Session` 接到 JSON-RPC 上的那一层，它是队列里的下一条。

**现在的数字**（最近一次普查，两份清单都是 `std_probe` 的**真总数**——那份直方图只打前 15 种消息，
"把看得见的加起来"比总数少，所以探针现在直接把总数打出来）：
`%TEMP%\stdprobe\files.txt` 的 128 个文件（`<vector>/<string>/<map>/<algorithm>` 的闭包）——
`干净 93 / 报错 35`，消息总数 **366**；每文件错误数 `干净 93 | 只有一个 11 | 两到五个 11 | 超过五个 13`；
`%TEMP%\cppls-indexed.txt` 的 455 个文件（分析闭包）——`干净 379 / 报错 76`，消息总数 **496**。
Rust 侧 `cargo test --workspace` = **1044 个测试 / 34 个套件全绿**。

**门禁三条 + 一条**（改完必须全绿，`index-design.md` §门禁有同样的表）：

```bash
cargo test --workspace                     # 1044 个测试，34 个套件
cargo clippy --workspace --all-targets     # 零警告
cargo doc --no-deps -p cpp_code_analysis   # 零警告（cpp_parser 有历史链接问题，不管）
cargo run -q -p cpp_parser --bin cpp_dump -- crates/cpp_parser/tests/real_world.cpp   # 必须 0 error
```

`rustfmt` **不是**门禁：这个仓库是手写格式（约 110 列），`cargo fmt` 会重排几千行。

**怎么跑普查**（所有数字的来源，`crates/cpp_code_analysis/examples/std_probe.rs`）：

```bash
# 一次性：拿一份闭包的文件清单（g++ 的 -M 输出就是）
g++ -M -std=c++20 t.cpp | tr '\\' '/' | tr ' ' '\n' | sort -u > files.txt
cargo run --release -p cpp_code_analysis --example std_probe -- files.txt
```

它打三段：**代价**（文件/行/字节/耗时）、**普查**（干净 vs 报错、**消息真总数** + 前 15 种消息的直方图、
每文件错误数直方图）、**每个失败文件的第一个错**（带文件名与源码行——这是唯一能看见**成因**的视角，
第一个错是上面什么都解释不了的那个）。本机的那份清单在 `%TEMP%\stdprobe\files.txt`（**128 个文件**；
换机器要重新生成）。

> **消息总数要读它打的那一行**（`--- messages: N in total over K kinds`），不是把直方图加起来：直方图只列
> 前 15 种，尾部的消息是真实的（第十七轮才发现这件事——相加得 920，真总数 941，差 21 条）。
> 本文档里第十七轮之前的那些"消息总数"是相加得来的，因此**偏小**；从第十七轮起都是真总数。

**看一个文件被读成了什么**（排查缺规则唯一有效的动作）：

```bash
cargo run -q -p cpp_parser --bin cpp_dump -- <file> --tree    # 有错也打树；不加 --tree 只在干净时打
```

**其它探针**：`examples/std_index.rs`（闭包的事实统计：`-- <清单> <缓存目录>`）、`examples/std_query.rs`
（**端到端**：真写一个 TU、发现工具链、索引闭包，然后按光标问成员——这一轮的 0/7 → 3/7 就是它）、
`examples/index_includes.rs`（冷/热索引代价）、`examples/open_project.rs`（**驱动层**：开项目 → 惰性索引 →
按光标问；冷/热两遍，以及"缓冲区就是文本"）、`examples/find_references.rs`（**找引用**：四级阶梯各自的代价、
词法 vs 解析、以及重命名会改几处）、`examples/macro_shape.rs`（**宏与读法**：失败文件的首错行上有没有宏、形态知不知道，以及"把闭包的宏表喂给 parser"会怎样）、`examples/condition_reach.rs`（**条件求值**：闭包里每个条件 include 的形状、
名字的来源，以及求值之后有多少个被判定）、`examples/measure.rs`（构建与命中）。

---

## 1. 方法：怎么判断一轮做完了

这五条是几轮下来真正管用的，其余都能推到它们身上。

1. **先量，再改，改完再量。** 每条结论都要能被同一条命令再现。本文档里每个数字都有出处。
2. **两个数字一起看**：`clean` 数得清"报错"，数不出"错树"。
   第 8 轮把 `__cdecl` 的 1464 处**静默错树**改对了，干净文件只多 1 个——因为那些文件本来就"解析成功"。
   所以：普查看进度，`gaps.rs` 的形状断言看正确性（维护约定第 29 条）。
3. **一轮做 3–6 条，按"第一个错"排。** 一个文件有 5 个以上错误时，后面全是级联；
   修"第一条错"才是把文件推到干净的唯一杠杆（维护约定第 11 条：先归类再动手）。
4. **每条都要一个最小复现。** 从 `<file>:<line>` 抄一行出来，缩到能一屏幕放下的程度，再问**它现在被读成了什么节点**。
   症状几乎从不指向成因：`using ::wint_t;` 报 `expected ; after expression`，而因在 137 行的一个 `extern "C++"`。
5. **改读法就要加形状断言**（`crates/cpp_parser/tests/gaps.rs`），改查询就要加"能力落地那天会失败"的测试
   （`index-design.md` §一条很好用的工作方式）。这一轮把 `f().size` 做出来时，正是两条旧测试如期失败，改动才是**有意**的。

**新缺口的三件套**：先写进 `grammar-gaps.md`（例子、现象、成因、性质），需要护栏时再加进 `gaps.rs`；
修好之后**保留成因与修复过程**，下一个人会需要它。普查的数字写回 `std-library.md`。

---

## 2. parser 队列（按"值多少文件"排，括号里是当前首错文件数）

**先读 [`parser-assessment.md`](parser-assessment.md)**：一页纸说清它现在处在什么水平、够不够好、以及
不够的地方挡着什么。这一节是**队列**——具体哪一条、怎么修、拿什么验；那一份是**判断**。

每条给：**形状 → 现在读成什么 → 思路**。"已确认"表示最小复现已验证，"待缩"表示只知道文件与行。

> 本文档里的 `<文件>:<行>` 是**编辑器行号**（从 1 起）。`std_probe` 打印的是从 0 起的行号，差一；
> 用 `cargo run -q -p cpp_parser --bin cpp_dump -- <文件> --tree` 看树时不必在意这个差。

### 2.0 先量了一件事：**不要把闭包的宏表喂给 parser**（`examples/macro_shape.rs`）

"剩下的失败都是宏"这句话被反着量了一遍。新探针做两件事：把每个失败文件的首错行按"行上有没有宏、那个宏的形态
索引知不知道"分类，然后**真的把闭包里所有宏的形态做成一张表喂给 parser 重解析**：

```text
128 个文件的闭包（编译器真正读的那份）   455 个文件的闭包（分析闭包，含 MinGW 头）
  首错 48 个                                首错 120 个
    行上没有任何宏          38 (79%)           行上没有任何宏          76 (63%)
    宏的形态已知             2 ( 4%)           宏的形态已知             1 ( 1%)
    宏的形态 Unknown         8 (17%)           宏的形态 Unknown        43 (36%)
    （三行窗口下 22/48 提到宏）                 （三行窗口下 67/120 提到宏）
  喂表之后：干净 80 → 43                     喂表之后：干净 335 → 290
           消息 941 → 1081                            消息 2551 → 4928
           **变干净 0 个，变脏 37 个**                **变干净 1 个（find.h），变脏 46 个**
  只喂"形态无冲突"的名字也一样（80 → 43）：问题不是表不自洽，是**答案没有位置**
```

**机制**（不是猜的，探针把第一个变脏的文件点出来了）：`bits/bit.h` 用了
`_GLIBCXX_BEGIN_NAMESPACE_VERSION`/`_GLIBCXX_END_NAMESPACE_VERSION`——这两个宏**就是** namespace 的开与关，
形态是 `Unknown`。表一旦说"这个名字是宏"，`at_a_macro_member` 对 `Described { .. }` 一律回答"可以独立成一个成员"
（`grammar/cpp/decls.rs:3196`），于是 namespace 嵌套在 `} // namespace std` 处崩塌（新错落在 497 行）。
一张无位置的表把"某个头文件里的宏"当成了"**这里**也是宏"的证据，而 `index-design.md` 早就把这条路记成
"A0 类问题：同一份源码会因索引状态不同而读成不同形状"——**现在它有了数字**。所以：**不做**无位置表。

同一次测量也回答了"接下来该做什么"：

```text
剩下的失败**大多不是宏**：真实闭包 79%、宏密集闭包 63% 的首错行上一个宏都没有
  （例：alloc_traits.h:454 `template<typename _Tp>`；amxtileintrin.h:56 `__asm__ volatile ("tilerelease" ::)`）
展开能买到的是 17% / 36%，而且集中在同一族：形态 Unknown 的函数式宏被当语句/声明片段用
  （_GLIBCXX11_DEPRECATED_SUGGEST("std::bind")、__glibcxx_function_requires(...)、_GLIBCXX_NOEXCEPT_PARM、
    __attribute__((__vector_size__(64)))）
顺带：闭包里 76 / 168 个名字有多种形态（_GLIBCXX20_CONSTEXPR 是 Specifier 又是 Unknown；ULONG_MAX 是
  Expression 又是 Unknown）——"一个位置无关的答案"连自洽都做不到
```

**结论**：下一步是下面这些**语法**条目（79% / 63% 的份额在那儿），展开是**有界**的第二件事、且只对这一族做，
并且必须是**按位置**的（parser 在具体调用点问"这次调用的展开是什么"），那就自然走向"带 origin 的展开片段"，
而不是表。

**按这个结论做的第十七轮**：六个构造，全部是语法、全部有形状断言，条目在 `grammar-gaps.md` B47–B52。
它们**分三批**落地，每一批都单独量了一遍——把某一批的改动反向应用回工作树再跑同一条命令，所以下面的
"逐批相减"是实测，不是记账：

```text
                                        128 个文件的闭包        455 个文件的分析闭包
                                        干净  报错  消息        干净  报错  消息
第十六轮基线                             80    48   941         335   120  2551
+ B47/B48/B49  条件声明 / 分配式括号组 / GNU 的 decltype 拼写
                                         86    42   874         340   115  2502
+ B50/B51      类头里名字之前的裸宏 / 类名与基类子句之间的指令
                                         86    42   813         341   114  2377
+ B52          空实参表的函数式转换
                                         87    41   802         342   113  2359
```

三批各买到什么（逐批相减），以及它们的**形状不同**——这一轮的收获之一就是这三种形状各有各的数法：

```text
B47/B48/B49   干净 +6 / +5，消息 −67 / −49     "读得出"型：一修就是几个文件整片变干净
B50/B51       干净 ±0 / +1，消息 −61 / −125    "把首错往后推"型：128 那份干净数一点没动
B52           干净 +1 / +1，消息 −11 / −18     "一条修两臂"型：`stl_iterator_base_types.h` 整个文件变干净
std_query（语义查询端到端）9/9 —— 三批都没动它
```

**B47/B48/B49**（第一批）：

```text
条件里声明一个变量     if (Foo p = get()) / if (const auto n = g())      B47（含静默的一半：Foo* p 读成乘法）
分配式的括号组         new T(a, *q) 里的 (a, *q) 是初始化式而非参数表      B48
GNU 的 decltype 拼写   typedef __typeof__(x) T; / __decltype(y) U;        B49
```

**B50/B51**（第二批）：类头"分几块写"的两种形状——名字**之前**的裸宏
（`class _GLIBCXX17_DEPRECATED unary_negate`）与名字与基类子句**之间**的指令
（`class move_iterator` / `#ifdef` / `: public …` / `#endif`）。两条都让 `expected ;` 落在**下一行**那个看着有问题的
`:` 上，而真正的成因在上一行。它们的收益形态值得单独记：128 个文件那份**干净数一点没动（86 → 86）而消息少了 61 条**，
455 个文件那份干净 +1、**消息少了 125 条**——正是"把首错往后推"那一类，所以两个数字要一起看，
而"消息变少"本身也是收益：少了 125 条级联意味着 125 条诊断不再误导读它的人。

**B52**（第三批）：函数式转换**没有实参**——`return int();`、
`return typename iterator_traits<_Iter>::iterator_category();`（`bits/stl_iterator_base_types.h:242`，也是这两条
让 `stl_iterator_base_types.h` 整个文件变干净）。两臂（关键字类型 / `typename`）都用
`parse_parenthesized_expression` 读实参表，而它读的是表达式、`)` 不是——**空实参表这种最常见的函数式转换**
因此读不出来。一条修两臂：共用一个把空 `()` 读成"没有子节点的 `ParenExpr`"的 payload 读取器。

**紧接着的第十八轮**（同一天，`grammar-gaps.md` B53/B54/B55/B56）：**从上一轮列出的"下一批"里接着做，前三条都是
`bits/alloc_traits.h` 那条链上的**，而且是**一个接一个露出来的**——每修好一条，那个文件的首错就往后跳一次，
下一条就现形了。这正是"用首错当队列"该有的样子，所以把四次跳跃和四条改动一起记下来：

```text
                                        128 个文件的闭包        455 个文件的分析闭包
                                        干净  报错  消息        干净  报错  消息
第十七轮结束（B47–B52）                  87    41   802         342   113  2359
+ B53  切开的说明符里，`#else` 那一支命名类型   87    41   796         342   113  2353
+ B54  模板实参的回落读了逗号运算符            87    41   721         342   113  2278
+ B55  条件决定的是限定符（`noexcept` 两个分支） 88    40   710         343   112  2267
+ B56  花括号函数式转换、以及作实参的 `T{…}`    88    40   702         343   112  2259
+ B57  requires 表达式**体内**的指令            89    39   701         344   111  2258
+ B58  **花括号债**（恢复：类体 / 语句块 / 链接块）92    36   375         364    91   688
+ B59  `typedef` 声明符后面的属性                 92    36   374         370    85   609
+ B60  条件决定的是**属性**（模板头与声明之间，与 B59 同一行）
+ B61  **方言**：`__int128` / `_Float16` / `__int64` 由目标编译器决定  92    36   372         371    84   607
+ B62  `T f(U) { … }`：**花括号体**说明括号组是形参表     93    35   366         375    80   596
+ B63  `(T)(U) x`：成串的括号组也要跨过去看后面         93    35   366         379    76   496
```

（B53/B54/B56 三步的**干净数一点没动**，B55 与 B57 各让两份清单 +1——前者是 `bits/basic_string.h`，
后者是 **`bits/alloc_traits.h`**，也就是被这五条一路推着走的那个文件。**B58 是另一类**：它不是读法，
是恢复——一个坏成员不再带走整个类/块/链接块，于是 455 那份一次 **+20 个干净文件、−1570 条消息**。
B59/B60 回到读法，走的是"GCC 自己的头文件"那一族：**+6 个干净文件、−79 条消息**。
B61 补的是**地基**：方言（哪个编译器）现在是一项配置，而它不只影响一个拼写——它是"编译器保留的名字是什么意思"
这类问题的答案所在。B62 又是"**偏好缺一条证据**"那一类：判据早就写好了（初始化式优先，为 `Max(a, b);` 而设），
缺的是"后面跟着函数体"这条证据，而它要用**真正的读取器**去问，不能另写扫描。
一路下来干净 87 → 93 / 342 → 375，消息 802 → 366 / 2359 → 596。）

```text
alloc_traits.h 的首错   454 → 536 → 689 → 941 → 1053（文件最后一行）→ **干净**
                        （B53 → B54 → B55 → B56 → B57：这正是"用首错当队列"的完整一轮）
B53   干净 ±0，消息 −6        "把首错往后推"型；它同时证明了上一轮那条 B53 诊断写错了（见下）
B54   干净 ±0，消息 −75       "一条改动、两份清单同额"型：库文件同时属于两份清单
B55   干净 +1 / +1，消息 −11  `bits/basic_string.h`（4532 行那条首错）整个文件变干净
B56   干净 ±0，消息 −8        五处对称的小改；它修掉的三种拼写里有**两种是完全静默的**（见下）
B57   干净 +1 / +1，消息 −1   一处接缝；`bits/alloc_traits.h` 整个文件变干净
B58   **干净 +3 / +20，消息 −326 / −1570**   恢复：级联的账全在这里（见下）
B59/60 干净 ±0 / +6，消息 −1 / −79            GCC 头文件那一族：`typedef` 属性 + 模板头后的属性与指令
std_query（语义查询端到端）9/9 —— 八条都没动它
```

**B53 的诊断被树推翻了，这一点值得留着。** 上一轮把它记成"函数自己的名字被当成类型的一个词"，而把树打出来
看到的是**反方向**：说明符序列把两支并成了一条，`void` 在**另一支**里却让"本序列已经命名过类型"为真，于是
`#else` 那一支的类型名被拒在类型位置之外、成了**声明符**，声明在 `#endif` 处没有 `;` 就结束了。修法因此不是
"收紧许可"，而是**在分支边界上重置知识**：`#else`/`#elif` 之后恢复"本支还没命名过类型"、"许可没花掉"、
判据回读事件流的窗口挪到指令之后；`#endif` 刻意不重置（它之后是两支共同的部分）。
`grammar-gaps.md` 的 B53 条目把**两版诊断都留着**，因为"猜的成因"和"树说的成因"差在哪，是下一轮最省时间的读物。

**B54 是一次"文档说修过了、代码没有"的实例**：`exprs.rs` 的 `Level` 表里明明列着
`types::parse_template_argument (expression fallback)` 读一个元素，而代码调的是含逗号运算符的 `parse_expr`——
所以 `S<3, 4>` 被读成**一个**实参 `(3, 4)`，**没有任何诊断**；`S<3, long>` 才报错。上一轮验证那条修复时用的例子
（`Grid<T, 3>::fill`）走的是**类型**读法，回落根本没跑，于是"文档对了、代码没动"这件事活了很久。教训与维护约定
第 5 条同源：那张表是"改共享入口前先列全调用点"的产物，**写下的每一条都得能被检查**。

**B56 的教训是"同一个判据的第五种用法"**：一个想法——*配对的括号属于包着它的东西*——在**五个地方**漏了。
三处是数角度的扫描（B30 那条"三处扫描要共用 `angle_depth_delta`"的教训在花括号上重演），一处是模板实参的
"类型读法停下来算不算读完"，一处是 `parse_primary_expr` 的函数式转换守卫只认 `(`。它修掉的拼写里
**有两种完全静默**：`X<int{}> m;` 读成比较式 `X < int{} > m`——树良构、无损、**零诊断**，
正是 `grammar-gaps.md` 开篇的 A0 类。所以 B56 的护栏一半是形状断言，因为 `assert_reads` 会认为旧读法是对的。

**B58 是这一轮里唯一一条"恢复"而不是"读法"，也是收益最大的一条**：一个失败的成员/语句如果已经吃掉了一个
`{`（requires 体、函数体、块、花括号初始化式都是这个形状），它就让外层容器少一个 `}`，而恢复是"跳到下一个
`}`"——那个 `}` 属于失败的那个构造，容器却把它当成了自己的结束。修法是**花括号债**：失败的构造为它消费掉却
没有配对的 `{` 记账，容器还清之前不许结束（用来还账的 `}` 读成 `ErrorNode`）。三个容器各一份（类体、语句块、
链接块），**两条失败路径都要记**（停在中间 / 整块回滚，后者的事件被截断、循环会一个 token 一个 token 重读）。

为什么它值这么多：一个坏成员带走整个类之后，**那个类里的每个成员都会各自再报一次错**——级联的账全在这里。
455 那份（含 MinGW 头，链接块最密）一次干净 +20、消息 −1570；128 那份 +3 / −326。
顺带一条方法论：`end_marks_to`（把失败处的 marker 带结束事件关掉）修的是**树**，`brace_balance_since`
（读事件流算括号差）修的是**归属**——两件事，缺一件就还是"诊断落在 900 行之外"。

**B59/B60 回到了读法，形状是"同一件事的第二条路径"**：`typedef` 有自己的声明符循环，于是漏了普通声明路径
早就有的"声明符后面的属性"（8 个 `*intrin.h` 的首错）；模板头后的属性有规则，但**指令**没有——而那个位置
的 `#` 既不是说明符序列的第一个说明符，也不是调用方会读的地方。两条都是一次调用/一段交替，各带形状断言。

**B61 补的是地基，而不是又一个拼写**：`__int128`（还有 `_Float16`、`__bf16`、`__int64`）算不算类型**取决于
目标编译器**，所以答案不该写在拼写表里——`cpp_parser` 因此有了 `Dialect`（`Gnu` / `Msvc`），语法层只有一处
读它，而**方言来自工具链自己 `-dM -E` 吐出的预定义宏**（`__GNUC__` / `_MSC_VER`），并且**进缓存键**：
同一段文字为两个目标读出的就是两份摘要。这一条修掉了一个**零诊断的静默错树**
（`unsigned __int128 x;` 曾经读成"名为 `__int128` 的变量 + 后缀宏 `x`"），而它一个数都不占——
所以护栏是形状断言，不是计数。它还带来一条**被量下来的取舍**：`_mingw.h:248` 那个 MSVC 分支的
`typedef int __int128 …` 在 GNU 方言下没有声明符名，让它报错**实测代价 9 个干净文件、246 条消息**，
所以不报，记在条目里（那一支在 GNU 目标下是死代码，按 MSVC 方言读它就完全正确）。

**B62 是"偏好缺一条证据"那一类，而且修法本身有一条纪律**：`T f(U)` 同时是"取无名形参的函数"和"用 `U` 初始化的
变量"，这个 parser 的偏好是初始化式优先（为没有类型的 `Max(a, b);` 而设）。缺的证据是"后面跟着 `{`"——
声明符只有一个初始化式，所以 `{` 是函数体。两条要记住：证据**用真正的读取器去问**（试读形参表 → 让后缀
读取器跑一轮 → 只有落在 `{` 上才保留），不另写一个扫描去抄后缀词汇表；以及"函数定义合法的地方"**不是**
`!is_inside_a_body()`——类体也算 body，`struct S { T f(U) { … } };` 因此第一版仍然读坏（它本来就是
零诊断 + 9 个 ErrorNode 的静默坏树）。正确判据是 `!is_inside_a_body() || is_at_class_member_level()`。

**B63 是 B62 的同族**：判据早就写好了，缺的又是证据——`(T)…` 只在"`)` 后面跟着操作数"时才读成转换
（`(f)(x)` 是调用），而 `(` 被刻意排除在那张表外，于是**成串的括号组**（`(T)(U) x`）连问题都问不到。
修法是"先跨过一串配平的括号组，再问同一个问题"——被跨过的恰是那个有歧义的 token，所以扫描是安全的
（`(f)(a)` 与 `(f)(a)(b)` 保持调用读法）。4 个 `avx10_2*` 文件因此变干净，`winbase.h` 首错后移 2400 行。

**下一批**：回到首错清单按文件数排（清单在 `%TEMP%\stdprobe\run_*.txt`，逐条都能重新生成）：
`::new (…) T(args)` 那种"分配式的类型是括号表达式"（已登记 `new (Widget)(1)`）、
`__asm__ volatile ("…" ::)` 那种**空的第二操作数段**（amxtileintrin.h、`_mingw.h`，方言已经在了）、
`0.0f16` 那种 `_Float16` 字面量后缀、`{ .__v = __A }` 那种**指定初始化式**、
`-> map<…>` 那种**跨行的尾随返回类型**、`_MM_REDUCE_OPERATOR_BASIC_EPI16 (+)` 那种**把运算符当宏实参**的调用，
以及 `_GLIBCXX11_DEPRECATED_SUGGEST(...)` / `__glibcxx_function_requires(...)` 这两个"独占一行的函数式宏"
（需要按位置的宏证据，是这张清单上最后一大族）。

### 2.1 指令落在构造的接缝上（**已做完**：九处接缝 + 恢复那一层 + 两个分支的构造，`std_query` **7/7**）

**这一条做完了三轮**：九处接缝（第十轮上半）、"读坏了之后怎么办"三条（第十轮下半）、"一个构造写在两个分支里"
与"一个 token 定读法"六条（第十一轮，最后一条是**同一条恢复判据在语句层的第二次**）。形状、位置、读法与
**四条被量下来的教训**都在 [`grammar-gaps.md`](grammar-gaps.md) 的"第十轮""第十一轮"两节里，这里只留
**结论与数字**：

```text
declarations_in("std::basic_string")   117 → 442 条（函数 103 → 391；size/find/substr/begin/… 全在里面）
std_query（examples/std_query.rs）      0/7 → 9/9（含解引用与下标两条，见 §3.2）
bits/stl_vector.h 的事实行数             37 → 469（`std::vector` 自己 0 → 113 条）
普查（128 个文件的闭包）                干净 76 → 80 / 报错 52 → 48；消息总数 1435 → 932
std_index                              声明 4821 → 12550；类型 2770 → 6223（别名 305 → 744）
bits/utility.h 与 include/c++/bit       0 报错
bits/basic_string.h 的首错              3838 → 4531 行；bits/stl_vector.h 541 → 1865 行
```

**第十三轮补上的三个构造**（B44/B45/B46，都在"宏住的那批文件"里，见 §2）：

```text
winnt.h                                 417 条错 / 936 条丢失指令 → **15 条错 / 0 条丢失**
分支结论在它身上                         STDMETHODCALLTYPE 的定义 settles = true（此前整个文件不配平）
455 文件的闭包（含 MinGW 头）            干净 328 → 335 / 报错 127 → 120；消息总数 3383 → 2511
分支结论的覆盖面                         定得住的宏名 333 → 577；使用 2626 → 4602 条
128 个文件的 libstdc++ 闭包              干净 80 / 报错 48，消息 932 —— **没动**（这三个构造不在它里面）
```

形状断言六条（`gaps.rs`）：九段接缝、四段瓦砾、一个构造两个分支、模板实参是调用还是函数类型、一个声明符一个
初始化式、语句失败之后块还在。五条教训进了维护约定第 32–35 条，其中第 34、35 条最贵：**"就地放弃、token 留着"
的错误路径必须带上 `NodeEnd` 关节点**（这一条收了两轮学费，声明层与语句层各一次）；**`rollback` 只截断，
回不到"未来"**。

**还没做的**，按值排：

1. **条件求值（现在是第一条，而且它比 P3 便宜得多）**：第十四轮量清了形态（`examples/condition_reach.rs`）——
   486 个条件包含的每一种形状都可判定，输入是（闭包事实 + **编译器自己的宏表** `-dM -E` + 配置），
   **不需要宏环境进键**。做法三步：工具链的宏表（与 `discover` 同一处，一次进程）→ 区域的条件进摘要
   （文件自己的文字，抬 `CODEC_VERSION`）→ 查询时判定（`parse_condition`/`evaluate` 已存在）。
   预期收益：那 4 000 多条引用从"可能"变成"使用"，`ConditionalCompilation` 大面积变成真答案。
   判据是**输入齐了才判，缺一样就 `Unknown`**（闭世界判定会把 Unknown 换成可能错的答案）。
2. **新登记的 B42/B43**（修恢复时量出来的两个真缺口，各带最小复现）：
   `void f() try { } catch (...) { }`（函数定义里的 `try`）与 `if (int x = g())`（条件里的声明）。
3. 各文件的**下一条**：`bits/move.h:233`、`bits/alloc_traits.h:453`、`bits/iterator_concepts.h:908`、
   `bits/stl_pair.h:407`、`bits/basic_string.h:4531`、`bits/stl_vector.h:1865`——都等着归类与缩。
4. 队列里**还没碰**的：§2.2（GNU 类型拼写：`__typeof__` / `__int128`）、§2.5（模板参数表里的宏）。
5. 剩下 63 个"超过五个错"的文件——那些是级联，按第 11 条先归类再动手。
6. **宏展开（P4）**等某个问题需要它再做；**LSP 二进制**最后（协议活，不挡语义）。

下面这四处是同一个模式的**记录**（第十一轮之后 `bit` 已干净、`utility.h` 已干净、`alloc_traits.h` 的那一处已改，
留在这里是因为它们是"接缝"这个概念最好的例子，而不是待办）：

```cpp
// bits/move.h:221    说明符与返回类型之间                          ← 同族，库里还有
template<typename _Tp> _GLIBCXX20_CONSTEXPR inline
#if __cplusplus >= 201103L
  typename enable_if<...>::type
#endif
f();

// bits/utility.h:176  别名模板的名字与 `=` 之间                     ← 已修（一个构造两个分支）
template<typename _Tp, _Tp _Num> using make_integer_sequence
#if __has_builtin(__make_integer_seq)
  = __make_integer_seq<integer_sequence, _Tp, _Num>;
#endif

// bits/alloc_traits.h:48  类头与基类子句之间                       ← 该处已随之读通
template<typename _Alloc, typename = typename _Alloc::value_type> struct __alloc_traits
#if __cplusplus >= 201103L
  : std::allocator_traits<_Alloc>
#endif
{ };

// include/c++/bit:94   requires-clause 与函数体之间                 ← 已干净
```

### 2.2 GNU 的类型拼写（3 个文件，已确认）

```cpp
typedef __typeof__(x) y;                                  // stddef.h:466、stl_uninitialized.h:202
typedef int __int128 __attribute__ ((__mode__ (TI)));     // _mingw.h:248
__MINGW_EXTENSION typedef unsigned __int64 size_t;        // corecrt.h:35
```

**现在**：`__typeof__` 之后读不出类型（`expected ;` 在第 22 列）；`__int128` 与 `__int64` 之类的名字落在类型位置上，
而"名字 + `__attribute__`"这条组合让声明收不了尾。

**思路**：和 `__attribute__`/`__declspec`（第 24 条）以及 `__cdecl`（第 28 条）**同一条依据**：
名字由标准保留给实现、含义来自**编译器**而不是文件，所以按拼写认是**有证据的**。
- `__typeof__` / `__typeof` / `__decltype` 当作**类型说明符**，载荷是表达式（和 `decltype` 一样，归入已有的 `BuiltinType`/`Decltype` 分支）；
- `__int128` / `unsigned __int128` / `__int64`：进"类型关键字"的那张表（`is_type_specifier_keyword` 的调用点要先列全，维护约定第 5 条）；
- `__MINGW_EXTENSION` 这种"什么都不是"的宏：它已经在"编译器关键字"那条路上了（`an_implementation_keyword`），
  如果还不够，检查它是不是被类型表当成了**类型名**（那会让后面的 `unsigned` 变成声明符的名字）。

### 2.3 小写函数式宏独占一行（类体那一半**不需要了**，函数体那一半待做）

```cpp
__glibcxx_class_requires(_Tp, _SGIAssignableConcept)                  // stl_vector.h:464 ← 恢复那一层解决了
__glibcxx_function_requires(_Mutable_ForwardIteratorConcept<_Iter>)   // stl_algobase.h:161 ← 待做
_GLIBCXX17_CONSTEXPR reverse_iterator                                  // stl_iterator.h:302 ← 待做
```

**类体那一半不需要了，而且不是因为那条规则改对了，是因为"恢复"改对了**（§2.1 的后半段第一条）。
`__glibcxx_class_requires(_Tp, _SGIAssignableConcept)` 这条成员现在仍然读不出来——它变成若干 `ErrorNode`——
但**它后面的成员还在**：声明失败之后恢复只前进一个 token 再重试，所以这个类照常被读出来
（`std::vector` 113 条事实、`v.push_back` 能答）。这条经历值得留着，因为它说明了一件容易搞反的事：
**读不出来**和**读坏了**是两个问题，前者的代价可以只是一条声明。

**试过、量过、撤掉的那一版**（别再试第二次）：在类体里按"名字 + 括号组 + 没有 `;`"的形状把这种成员读成
`MacroCall`（判据在尝试之前问、检查点在失败之后用，见维护约定第 32 条）。隔离里完全正确，量下来是
**`declarations_in("std::basic_string")` 398 → 287**、而 `bits/stl_vector.h` 的首错一行没动：宏读法把名字**和**
括号组一起吃，而 `ErrorNode` 恢复只吃掉一个 token。函数保留在 `decls.rs` 里
（`at_a_call_shaped_macro_member`，`#[allow(dead_code)]`），注释里带着这段数字。

**待做的两半**：`__glibcxx_function_requires(…)` 在**函数体**里独占一行（它后面跟的是**另一个语句**，
所以类体这条思路用不上，而"后面能开始什么"要放宽到"语句"）；`_GLIBCXX17_CONSTEXPR reverse_iterator`
是宏站在**返回类型**位置。放宽之后**必须**给反例：`x = 1;`（赋值）、`FOO(x);`（most vexing parse）、
`TEST(A,B){ }`（定义）——函数体里 `COUNT` 后面跟 `return` 是漏了分号，那条拒绝是有意的
（见 `at_a_macro_that_stands_for_a_declaration` 的文档）。

### 2.4 `requires` 与它周围的构造（**构造函数那一格已做**，其余 2–3 个文件已确认）

```cpp
// bits/stl_pair.h:367   requires-clause 与构造函数的初始化列表之间  ← 已修（同一处读法）
template<typename _U1, typename _U2> constexpr pair(...)
  requires is_default_constructible_v<_T1> && is_default_constructible_v<_T2>
  : first(), second() { }
```

**构造函数那一格已经做了**：`finish_init_declarator` 的 requires 分支现在接着读**指令**再读 `:` 的成员初始化列表
（`bits/basic_string.h:585` 那一处，见 §2.1 的九处表）。`bits/stl_pair.h:372` 那个首错因此换人。

**思路**：`requires` 是**子句**而不是表达式，它右边允许什么由"谁拥有这个子句"决定
（declarator 的后缀、模板头之后、类头之后）。这里缺的一格是"子句之后是**构造函数的初始化列表**（`:`）"。
`concepts.rs` 里那一族测试是这类改动的护栏——先看它们再动手。
同族还有两条**线索不一致**的地方，值得单独查：`concepts:170` 是 `(void) ::new _Tp;`（在 requires 体里），
而隔离测试给出的是**相反的**结果——`(void) ::new T;` 通、`(void) new T;`（没有 `::`）不通。
也就是说文件可能栽在**更早**的第二条 requirement（`_Tp{};`）上，或者两条线索各指一个成因：**先缩**。

### 2.5 模板参数表里的宏（1–2 个文件，已确认形状）

```cpp
template<typename _Res, typename... _ArgTypes _GLIBCXX_NOEXCEPT_PARM>   // bits/refwrap.h:142
```

**思路**：模板参数表的收尾方向是 C1 时期的老问题（`>` 是收尾还是比较），现在缺的是"参数表里允许一个宏"。
代价要先想清楚：参数表里的东西是**声明**，所以"宏站在这里"的判据要和 `parse_template_parameter` 的形状配合，
不能让它把后面的 `,` / `>` 吃掉。建议**只在"参数名之后、`>`/`,` 之前"这一格**接受一个标识符。

### 2.6 `if` 与它的条件/分支被宏和指令切开（3 个文件，已确认）

```cpp
if constexpr (requires { ... })                      // basic_string.h:491
if (auto __ne_ptr = dynamic_cast<T*>(__ptr))          // nested_exception.h:177（这个在隔离里是干净的）
if constexpr (std::__is_same(...))                    // new:234
```

**思路**：`if` 的条件位置现在接受"一个标识符 + `(`"（`if _GLIBCXX17_CONSTEXPR (x)`，上一轮做的），
但 `if constexpr (requires { … })` 要求条件里的**表达式**读得下 requires 表达式；
`new:234` 那一条是 `std::__is_same(...)` 在条件里——先在隔离里复现，别急着改规则的形状。

### 2.7 指令/宏切开函数体与类体（2–3 个文件，待缩）

```cpp
#if ...
  }
#endif
  { return pointer::pointer_to(__r); }   // bits/ptr_traits.h:113：函数体的括号被 #if 分开
private:                                  // stl_pair.h:372：这一行的错来自上面那条构造函数
```

**思路**：`ptr_traits.h` 是 2.1 那个模式的又一处（函数体的 `{` 与 `}` 之间夹着指令）；
`stl_pair.h` 是 2.4 的**后果**，不是新成因——先修 2.4 再看它。

### 2.8 队列尾巴（每条 1 个文件，先缩再看值不值得）

| 文件:行 | 形状 | 备注 |
|---|---|---|
| `bits/stl_iterator_base_types.h:132` | `typedef _Category iterator_category;`（`_Category` 是模板参数） | **在隔离里是干净的**（`template<typename T> struct S { typedef T t; };` 通过），所以文件里的失败是级联或静默错树——值得用 `cpp_dump --tree` 看 `struct iterator` 那一段 |
| `string_view:596` | `basic_string_view(_It, _End) -> basic_string_view<iter_value_t<_It>>;`（推导指引） | 最小推导指引 **通过**，所以成因在细节（模板实参里的 `iter_value_t<_It>`？） |
| `bits/ranges_util.h:267` | `template<kind K = X ? kind::sized : kind::unsized>`（**三目当默认模板实参**） | 已确认：`template<typename T, int N = (X ? 1 : 2)>` 通过，而**带作用域的枚举值**那个拼法不通过 |
| `bits/max_size_type.h:566` | `_M_rep \|= ~(__max_size_type(-1) >> __r._M_rep);` | `expected ), but get (`：函数式转换的对象是一个**类型名**，最小复现（`T(-1)`）通过，待缩 |
| `ostream.h:125` | `operator<<(__ios_type& (*__pf)(__ios_type&))` | 形参是**函数指针**，最小复现通过，待缩 |
| `include/c++/exception:87` | `typedef void (*_GLIBCXX11_DEPRECATED unexpected_handler) ();` | 宏在**函数指针声明符中间** |
| `typeinfo:194` | `[[__gnu__::__always_inline__]]` | 隔离里干净，待缩（可能是它出现的位置不允许属性） |

> **表里带"隔离里干净"或"最小复现通过"的那几条都属于"先别改"**：一个形状在隔离里能读、在文件里不能读，
> 说明**因在别处**（级联或静默错树）。这种时候正确的动作是 `cpp_dump --tree` 看**上下文**，
> 而不是改那条看起来失败的规则。

---

## 3. 语义队列（按价值排）

### 3.1 跟着 typedef / 别名走一步（**已完成**，附带挖出一个更大的洞）

**现象**：`s.substr(1).size` 报 `NotDeclaredHere("std::string::substr")`——`std::string` 是
`typedef basic_string<char> string;`，而成员查找是**按名字找类**，不跟别名走。同一条边界也挡住 `std::vector`、
`std::string_view`、所有 `*_type` 别名。

**做完了什么**（`sema::declarations::declared_alias_target` + `index::project::resolve_aliases`）：

- `typedef`/`using` 的事实把**目标拼写**记进 `type_of`（对变量是"它的类型"，对别名是"它指向的类型"；
  一条 `DeclKind::Type` 且 `type_of` 有值的事实就是别名——这就是判定规则，没有加新字段）。
  两种拼法都读：`using X = T;` 取 `TypeId`；`typedef T X;` 是**说明符 + 声明符**，
  把别名自己的名字从声明符里**剪掉**，于是 `typedef void (*F)(int);` 得到 `void (*)(int)`。
- 查询侧在"把一个拼写当成类"的那一个地方跟一步（`direct_member` / `direct_members` / `member_fact`），
  **目标在别名自己的作用域里解析**（`namespace std { typedef basic_string<char> string; }` 的目标是相对写的，
  所以要试 `std::basic_string` 再试裸的 `basic_string`），深度上限 8，成环返回最后那个拼写。
- **`CODEC_VERSION` 不用抬**（原文写的是"这是 CODEC_VERSION 变更"，那是指纹落地之前的说法）：
  `build.rs` 的 `READING_FINGERPRINT` 对 `cpp_parser/src` 与 `src` 取哈希，**改了生产者源码整库自动作废**。
  只有"源码里看不出来的字段格式变化"才抬 `CODEC_VERSION`。

**量到的**（`examples/std_index.rs`、`examples/std_query.rs`，闭包 = `<vector>/<string>/<map>/<algorithm>` 的 128 个文件）：
第十轮之后是 **7407 条声明**，其中 **486 条是别名**（3881 条 `DeclKind::Type` 里的一部分）、
**111 个类带基类**——别名这一步要跟的目标全在里面。

**顺带挖出来的那个洞比别名大**：加完别名这一步，`std_query` 依然 **0/7**——因为 `std::string` 那条事实
虽然索引里有，查询却报 `ConditionalCompilation`。成因是头文件把整个身体包在**自己的 include guard** 里，
于是"每个 `#include` 都落在 `#if` 里"。修法是 `index::deguard_the_files_own_guard`：**文件自身守卫里的事实记为
`Unconditional`**（理由与测量记在 `index-design.md` 的三道判据第 3 条）。修完之后 `std::string` 找到了，
并且带着 `type_of = Some("basic_string<char>")`、`scope = Some("std")`——正是别名这一步需要的输入。

**这一格现在的状态（第十轮之后：`std_query` 3/7）**：

1. **`std::basic_string` 已通**（`size`/`substr`/`empty` 三条查询都答出来了）。它仍然报 `Ambiguous`——
   在 `bits/stringfwd.h` 里前向声明、在 `bits/basic_string.h` 里定义——而成员查找**不走** `definition()`，
   改问 `declarations_in(class)` 并按声明顺序取第一条（见上面"重载那一格"），所以歧义不再挡路。
2. **`std::vector` / `std::map` 仍报 `NotDeclaredHere`，但成因不是条件性**：是 parser 在
   `bits/stl_vector.h` / `bits/stl_map.h` 里读不下去（`std::vector` 一条事实都没有）。
   第十轮把它定位到 `_Vector_impl` 体内的两个构造之上，见 §2.1 的第 1 条。
   ——所以"要么喂宏环境、要么把'候选全是条件'与'一条候选也没有'分清"这个二选一**先搁置**：
   等 parser 那边通了，再量 `definition()` 到底报哪一种。


### 3.2 解引用与下标：`(*p).size`、`arr[i].size`（**已完成**，`type_of_expression` 的第五、六格）

**做完了，而且是一次"边界移动"的完整例子**：两条读法和一条事实层的补充。

* **`*p`**：对象是指针/引用时，`*` 是**对拼写做算术**——声明已经写了指向什么（`Widget*` → `Widget`），
  没有查找。`&x` 是同一个规则的另一个方向（拼写后面接 `*`），一起做了：`(&r)->size` 也因此能答。
* **`arr[i]`**：数组的元素类型写在声明里（`Widget[4]` → `Widget`，`int[2][3]` → `int[2]`），取**最后一对**
  方括号。**类**的下标（`v[0]`）仍然答 `Unknown`，而且理由写在代码里：那是 `operator[]` 的返回类型，
  要**实例化**模板；从拼写里取第一个模板实参对 `vector` 对、对 `map` 错，而这一层分不出两者。
* **两层事实补上了缺的一半**（否则前两条无从谈起）：
  - `DeclFact::returns` 只读说明符序列，于是 `Widget* make()` 的返回类型是 `Widget`——**指针丢了**。
    现在把**声明符里名字之前的那段**（并且只有纯 `*`/`&`）接上去。
  - `DeclFact::type_of` 同样只读说明符序列，于是 `Widget* p` 的类型是 `Widget`、`Widget arr[4]` 是 `Widget`。
    现在接上声明符里**除名字与初始化式之外**的部分（`Widget w(1, 2)` 的直接初始化括号在声明符**里面**，
    所以这是"多个区间一起剪"）。

**量到的**（`examples/std_query.rs`，分母从 7 扩到 9 —— 新增的两条各自要跨三步：别名、解引用/下标、跨文件成员查找）：

```text
(*p).size       -> bits/basic_string.h  std::basic_string::size     （p 是 std::string* 形参）
arr[0].empty    -> bits/basic_string.h  std::basic_string::empty    （arr 是 std::string[4] 形参）
std_query 7/7 → 9/9
```

**落地的信号是两条测试先失败**（这个仓库记能力的办法）：`a_member_access_on_an_expression_that_is_not_a_call_is_an_unknown_type`
与 `a_completion_on_an_expression_with_no_type_offers_nothing_and_says_why` 两条断言"这里答 Unknown"的测试，
改成了断言新答案；剩下的边界换成**算术表达式**（`(a.size + a.size).` 仍然 Unknown——它的类型要语言自己做转换）。

**顺带修掉的两个"读错了一半"**（都不是新功能，是旧字段不完整）：形参的 `Binding.range` 覆盖的是**整个形参**
（`Widget* p` 从 `Widget` 开始），所以按它下潜会走进说明符序列、永远见不到声明符——改成按**名字**下潜，
并且取路径上**最内层**的那个声明符（形参的路径上最外层是函数的声明符）。

### 3.3 作用域/名字的倒排表（一万文件时才疼，但设计现在就该定）

`ProjectIndex::declarations_in` 仍然是"遍历所有摘要、逐条比较 `scope`"。上一轮拿掉了更大的常数
（可见性从"每个文件走一遍 include 图"改成"一次查询一遍图"，573 ms → 3.2 ms），
剩下的是 O(所有声明)。**做法**：`HashMap<String, Vec<PathBuf>>`（作用域 → 声明它的文件），在 `insert`/`remove` 里增量维护；
查名字那一条同理（名字 → 文件）。**先量再改**：现在 308 个文件的闭包是 3.2 ms，一万文件时才值这一刀。

### 3.4 `DeclFact::clean` 的第一个消费者（诚实这条线还差最后一步）

字段已经落盘、判据有测试钉住（每条声明回答"这条读得干不干净"），但**没有查询读它**。
差的是"谁降权"：成员列表/补全要不要隐藏 `clean == false` 的事实，还是照常给出并标注。
**倾向**：标注而不是隐藏（4% 的声明不可信，隐藏会丢真答案），但那需要客户端能显示，
所以先定**查询层的形状**（`MemberList` 加一个字段？还是让 `UnknownReason` 多一格？），并有测试钉住。

### 3.5 宏的"找引用/重命名"（**已做完**：`index/references.rs`；量出来"位置表"不需要）

**这一条原本的前提是错的，而且是被量翻的。** 原文写的是"摘要里没有标识符位置，所以要么逐文件解析、
要么加一份 token 位置表"——先量之后发现**第三种更便宜的做法**：**只词法、不解析**。

```text
1. 候选集    定义所在文件 + 它们的传递反向 include 闭包（看不见这个名字的文件不可能是用户）
2. 文本预筛  text.contains(名字) —— 精确，不是启发式：标识符的文本一定是正文的子串
3. 词法      CppLexer 一遍。注释和字符串**各是一个 token**，所以文本搜索最大的两类假阳性根本进不来
4. 精确判定  每个命中问"这个名字在这里是不是宏、是哪条 #define"
```

**量到的**（`examples/find_references.rs`，标准库闭包 454 个文件，release；问的是这个闭包里用得最多的四个宏——
由"数每个标识符"选出，不是挑的）：

```text
宏                       候选   读过   词法   引用数              整条查询   同样这些文件解析   倍数
__attribute__            169    169     26    770（1 定义+769 可能）   24.7 ms   472 ms          17×
__MSABI_LONG             170    170     18   1165（2 + 1163）          30.4 ms   448 ms          31×
STDMETHODCALLTYPE         42     42     12   4079（1 + 4078）          35.5 ms   932 ms          56×
WINAPI                    77     77     56   2637（5 + 2632）          39.2 ms   669 ms          45×
```

* **位置表不值得**：整个闭包有 630 278 个标识符，一张"名字 → 位置"的表约 **4.9 MB**，而现有摘要一共
  5.4 MB——查询却只要 25–40 ms。**要存的位置比要存的事实还多，而查询并不慢。**
* **写这份查询时先写错了一次，量出来才发现**：第一版每个命中单独调一次 `macro_definition`，于是每个命中都
  走一遍 include 图。`STDMETHODCALLTYPE` 的 4 079 个命中 = **3.77 s**。改成"每个文件算一次宏环境、
  再按偏移问它"（`MacroEnvironment`）之后是 **35 ms**，**106 倍**，而且两次的答案逐条相同。
  这条教训进 §5 的坑清单。
* **顺带修掉两个"读错了一半"**（都是旧字段不够用，不是新功能）：
  `#undef` 的事实带的是**整条指令**的范围（指令读取器不记名字在哪），于是重命名会把 `#undef API` 整行换掉——
  现在用词法给出的名字范围补上；以及 `name_at` 在**指令里**答 `None`（`#define FOO` 是 token，不是名字节点），
  于是"在 `#define` 上右键找引用"这条路本来是断的（新增 `name_at_including_directives`，有测试）。
* **验证方式**：`examples/find_references.rs` 里项目自己的宏给出 **3 处引用（1 定义 + 2 使用，0 个可能）**，
  重命名 3 处编辑；`rename()` 的测试是**把编辑应用到文本上，再和期望的文本逐字比较**——范围差一个字符就会失败。

**这一格剩下的、也是下一步**：上面四个宏的引用**全部是"可能"**，不是"确定"。原因量清楚了，不是 bug：
MinGW 的 `winnt.h:450` 把 `STDMETHODCALLTYPE` 定义在 **`#ifndef STDMETHODCALLTYPE`** 里（"define-once"惯例，
不是文件守卫），所以"这个名字在这里是不是宏"取决于分析没有的宏环境。

### 3.5b 条件块的分支结论（**已做完**：`MacroFact::settles_the_name`）

**定理**（可靠，不是猜）：`#if` 的每个分支都写了同一个名字、同一种 kind，而且要么有 `#else`、要么是单分支的
`#ifndef NAME / #define NAME` ⇒ 这个条件块之后这个名字的宏状态与分支无关。`#ifndef NAME` 的条件**就是**
"NAME 还没定义"，这正是它和别的 `#if` 的区别：条件成立则这里定义，不成立则它已经被别的头文件定义过。

**落地**：一个布尔字段记在**事实层**（由本文件自己的指令算出，别的文件怎么变都不会让它过期——过了"事实"
那条不变量的判据，`CODEC_VERSION` 抬到 10），**只有引用查询读它**；定义查询仍然答"哪一条 `#define` 在生效"
（那确实仍然有条件）。规则要读的是**嵌套**，所以还有一条**安全网**：指令不配平的文件（`#endif` 对不上任何
`#if`，或文件结束时还有没关的区域）**一条都不认**——偏短的父子链会让它多认一个"定得住"，而多认就是错答案。

**量到的**（同一个 454 文件的闭包）：

```text
闭包里"定得住"的宏名                     333
其中有引用的 100 个：使用 / 仍可能        2 626 / 417
最大的几个   _HRESULT_TYPEDEF_ 1394 uses(41) | __mingw_ovr 263(0) | WSABASEERR 178(0)
安全网挡掉的"本来会认"                    184 条（都在指令丢了 `#endif` 的文件里 = 错答案）
```

**而探针里那四个宏一个都没被救到，原因同样是量出来的**：定义它们的文件**解析不干净**（`winnt.h` 417 条错），
而错的代价是**指令丢了八条 `#endif`**（`scan_directives` 只认语法树里的 `PreprocessorDirective` 节点——这是
刻意的，哪些 `#` 是指令已经由 parser 决定）。所以它们仍然全是"可能"，而这一格的下一步因此变成了
**parser 的长尾**：`winnt.h` 那 417 条错（§2.1 第 1 条那一族），不是规则的问题。

### 3.5c 条件求值（**已做完**：存问题、按翻译顺序喂闭包的宏；差"输入完整吗"这一格）

**做什么**：`#if` 到底成不成立。两半都落地了：

```text
第一半（第十五轮）  条件进摘要（分支链、正文、嵌套，CODEC_VERSION 11），查询时求值
第二半（第十六轮）  walk 按翻译顺序把闭包自己的宏喂进环境：
                    文件的事实与 include 合成一条按偏移排序的流；
                    每条事实只喂确定的（区间取到 → 名字+值；settles → 只喂"是宏"；否则按名字记"不确定"）；
                    文件自己的守卫不是条件（SummaryGuards::own_guard，链上跳过它）
一条新规则         Lookup::{Defined, DefinedWithoutAValue, Undefined, Unanswered}
                    "这张表说不出" ≠ "没定义"——标准的 0 是对完整输入说的
```

**为什么答案不能存进摘要**：条件成不成立取决于**怎么编译**（`-std=` 决定 `__cplusplus`），而摘要的键只由文字和
路径算出。存答案 = 把 `-D` 塞进键 = 取消"先查盘、后解析"。所以存问题，答案是查询期的。

**量到的**（459 个文件的闭包，`examples/condition_reach.rs`，默认配置）：

```text
条件 include                              486 个
  判定为取到（Active）                    127      例：vector:90  #if __cplusplus >= 201703L
  判定为不取（Inactive，include 不再跟进）  61      例：char_traits.h:53
  Unknown                                298      例：string:71  #if __cplusplus >= 201703L && _GLIBCXX_USE_CXX11_ABI
  **共 188 个被判定**（第十五轮结束时是 85）——而且一个否定结论都没用上
查守卫 vs 查偏移的分歧                     486 里 2 个（都在指令不配平的文件里；取守卫那一份）
```

**代价**（同一个探针的四个查询）：判定加了"边走边喂状态"之后仍在同一量级；真正的大头是每条候选文件走一遍图
（`index-design.md` 的四级阶梯），以及每次判定把条件文本重新词法化（`GuardBranch::as_branch`）——真要紧时缓存它
（每个区间一份重建好的 `Branch`，用 interior mutability）。**没量到痛点之前不做**。

**"输入完整吗"这一格：量清了，但还没打开。** 把环境声明成"这就是这次编译的全部定义"（`Session::open` 读到
`compile_commands.json` 时才敢这么说）之后，同一个闭包是 **440/486**（332 取到、108 不取、46 未知——剩下的几乎
全是 `__has_include`）。但代价是两条**路径被切断**：`__attribute__` 的 767 条、`STDMETHODCALLTYPE` 的 4 078 条
"可能"变成了"不是引用"。原因定位到一行：

```text
minwindef.h:2594  #include <winnt.h>   条件 Unknown   ← walk 先到这里
windef.h:366      #include <winnt.h>   条件 Active    ← 第二次，被 visited 挡掉（一个文件只走一次）
⇒ winnt.h 的宏全部"不确定"，定义不再是候选
```

这与"一次查询走一遍 include 图"那一节记过的 `visible_files` 旧 bug 是同一类：**先被条件路径找到的文件，不会再被
无条件路径改善**。修法也是同一个——"更好的答案赢"：`visited` 要记住"是怎么进来的"，确定路径到达时要能改写带疑的
那次访问（候选升级 + 状态里那条 `uncertain` 被确定的事实覆盖）。在那之前 `Session` 声明 `configured = false`
（理由写在那一行旁边），因为强声明会把诚实的疑问变成错答案。

**接着做的顺序**：

1. **让确定路径改写带疑的访问**（上一条），然后才谈打开"输入完整"。它同时会让 `STDMETHODCALLTYPE` 那四个宏
   从"可能"变成"使用"——那是第十四轮起就挂着的那一格。
2. **`__has_include(…)`（47 处）接进求值器**：这是**文件问题**，`IncludeResolver` 已经能答，缺的是
   `parse_condition` 认识这个运算符（它现在会 `EvalError`）。它也是"输入完整"打开后剩下的 46 个 Unknown 的全部。
3. **每个 target 一份环境**（现在一份：编译数据库第一条）。摘要格式不用动，因为环境不在摘要里。

**风险写在明处**：判定用的是**配置**。`Session::open` 从数据库取 `-std=`/`-D`，没有数据库时用编译器默认
（本机 g++ 15 = C++17）。用 `-std=c++20` 构建却没有数据库的项目会被按 C++17 读，`>= 202002L` 的块判成"不取"
而跳过——配置的错，以"少了声明"的形式出现。
### 3.6 "打开一个项目"的入口（**驱动层已做完**：`session.rs`；差一个 LSP 二进制）

**这一条原本写的是**："今天 `discover`/`index_includes_from`/`Worklist` 都能跑，但没有把它们串起来的那个函数"。
现在有了，而且是**一个类型**：`Session`（`crates/cpp_code_analysis/src/session.rs`，15 条测试）。它把四件事串起来：

```text
开项目      Session::open   发现工具链 + 读 compile_commands.json + 扫描源文件列表（三者都进队列）
说变了什么  did_open / did_change / did_save / did_close / changed(事件)
索引多少    advance(n)      一次 n 个文件；顺序是"开着的文件 → 它 include 的 → 项目其余"
回答问题    view(路径)      → definition / macro_definition / member_completions / name_completions / members_of
```

**几个设计上真正花了心思的地方**（都是被实测或借用检查器逼出来的）：

* **provider 归调用方所有**。`SummaryStore` 借 provider 过活，所以 `Session` 只能借那串
  `OverlayFiles<OpenDocuments, DiskFiles>`——而那个 **handle 就是编辑器的入口**：缓冲区放在 `Arc<RwLock<…>>`
  里，`did_change` 能在 store 还活着的时候改文本。自引用结构（store 借着自己所在结构的一个字段）Rust 没有
  安全写法，也不值得为它写 unsafe。
* **队列不是 `Worklist`**。理由有两条：它会**活过一批**（通知会重新播种），而 `Worklist` 可变借用 store，
  拿着它的 Session 在两步之间答不了任何查询。顺序是同一条（`outcome_of` 这条"这一步干了什么"的判据两边共用）。
* **两个半边 + 升级**。项目扫描在开项目时就把所有文件放进 rest 半边，用户随后打开其中一个——如果不去管它，
  那个文件就排在"项目其余"后面，**而所有"从不打开任何文件"的测试都会是绿的**。所以路径带 standing
  （`Queued(Open)`/`Queued(Rest)`/`Worked`），升级只加一条队列项、不重复算一个文件，
  旧的那条浮上来时被跳过（有测试钉住"它不会被读第二遍"）。
* **`again` 与 `requeue`**：前者是"用户正在等的那个文件变了"（进它那半边的**队首**），后者是"整库的键都作废了"
  （配置变了，进队尾）。两者都会清掉 standing——`add` 的职责恰恰是拒绝已经读过的路径，而变更必须越过它。
* **诚实那一条在驱动层是"两句话"**。惰性索引让 `NotDeclaredHere` 同时表示"这里没有"和"还没读到"，
  而这**不是**能靠改查询解决的：`ProjectIndex::definition` 的文档早就写了索引永远是全集的一个子集，从不声称"哪儿都没有"。
  所以分界线留给上层，规则写在这里：**`pending() > 0` 时不要报"名字不存在"，`pending() == 0` 时它才是关于项目的结论**。
* **缓冲区就是文本**。`OverlayFiles` 让没保存的 buffer 参与 include 解析（`#include "widget.h"` 找到的是
  buffer，不是磁盘），而 `store.forget` 让"改了但还没重读"这段时间的查询答 `Unknown` 而不是答旧文本——
  探针里 `widget.h` 磁盘写 `on_disk`、buffer 写 `in_buffer`，报出来的是 `in_buffer`；`did_close` 之后又变回 `on_disk`。

**量到的**（`examples/open_project.rs`，release，本机 mingw gcc 15.1.0；项目里 `main.cpp` include
`<string>/<vector>/<map>` 加一个本地头，工具链自己发现）：

```text
冷启动  开项目（发现工具链+扫描） 72 ms ；`s.size` 的成员在第 32 个文件后答出来，2.58 s
        整个闭包 454 个文件 9.4 s（全部要解析，0 个命中）
热启动  开项目 74 ms ；`s.size` 同样是第 32 个文件，但只用 26 ms（**快 100 倍**）
        其余 422 个文件：3 个解析、451 个从盘上读回，命中率 99%
查询    9/9（`s.size`/`s.substr`/`s.empty`/`v.push_back`/`v.size`/`m.find`/`m.begin`/`(*p).size`/`arr[0].empty`）
```

冷启动那两个数字连起来看才是产品结论：**"第一个答案"是 32 个文件 2.6 s，而"整个项目就绪"是 454 个文件 9.4 s**——
差的 420 个文件就是惰性索引省下来的东西，也正是"打开项目"和"打开项目并等它读完"的区别。
（3 个"每次都要重解析"的文件是那两个**开着的 buffer** 和 1 个"include 没解析到、故意不入缓存"的文件，
理由在 `index/store.rs` 的模块文档里。）同机重跑这些数字在 ±10% 内波动，**文件数是硬的、毫秒数只用来比大小**。

**这一格还没做的两件事**（都在这一节里留着）：

1. **LSP 二进制：暂缓，等明确要求再做。** 语言服务器那层的分工已经定了——**客户端通知我们，不接 `notify`、
   不做去抖时钟**——而二进制本身（JSON-RPC 帧、`textDocument/*` 到 `Session` 的映射、位置换算）是协议活，
   不是语义活。已经就位的东西够接：`Session` 收 `didOpen`/`didChange`/`didClose`/事件、`view` + 五个查询、
   `cpp_parser::LineIndex::get_offset/get_line_col` 就是位置映射（LSP 的列是 UTF-16 码元，非 ASCII 行上要转，
   那是协议层的事）、`pending() > 0` 时不报未解析的名字。**所以它不挡任何语义工作**——这是暂缓它的真正理由，
   而不是"以后再说"。
2. **每个文件一份配置**：`Session` 现在是"整项目一份配置"（编译数据库的第一条），真实项目不同 target 的 `-D` 不同。
   修法在 `SummaryStore`（它对所有文件持有一个 `config`），而键里已经记了完整编译上下文，所以是可表示的，
   只是没实现。

---

## 4. 两条线怎么交替

**一轮 parser、一轮语义**（这个节奏是显式选的，见 `std-library.md` 的"判断点"）。理由：
parser 的边际收益是"每轮 1–3 个文件"，连续磨十几轮会失去设计视角；语义那边每一步都要 parser 先把东西读下来。
判据是**两个数字**都动：parser 轮看 `clean`，语义轮看查询的实测（能不能答、多少毫秒）。

**一轮的配方**（照抄即可）：

1. 跑普查，取**第一个错**那一列，按"同一成因"归类（维护约定第 11 条）；
2. 挑 3–6 条同类或高值的，每条**缩小到最小复现**，用 `cpp_dump --tree` 看它被读成了什么；
3. 改读法（parser）或加一格（语义），**同时**写形状断言/能力测试；
4. 跑普查，记下数字（干净文件数 + 消息总数 + 任何"答了但答得对不对"的抽查）；
5. 三份文档各写一处：`grammar-gaps.md` 的条目与约定、`std-library.md` 的数字与队列、
   `index-design.md` 的查询清单与"答不了什么"表；
6. 门禁四条全绿再收工。

---

## 5. 坑清单（维护约定里对新读者最要紧的那些）

完整的三十二条在 [`grammar-gaps.md`](grammar-gaps.md) §维护约定（编号到 31）。以下是**最常撞的**：
- **第 13 条：解析成功不是证据。** 要问"它停在哪个 token、后面允许是什么"。
  一个容错 parser 在垃圾输入上"成功"是常态。
- **第 6 条：改读法必须加形状断言。** 报错、无损、良构三条判据拦不住**错树**——
  一棵错的树同样可以无损、良构、零诊断（A0 那一类）。
- **第 16 条：约定要换成证据。** 能建表就建表（`TypeNames`/`MacroNames`）；
  只有在"两种读法都是错、挑损失小的那个"时才用拼写兜底，而且要写清为什么。
- **第 29 条：一轮的收益要数两个数字。**（见 §1）
- **第 30 条：一个计数器只回答一个问题。**（上一轮：链接块被当成 body，一次坏掉 4 个文件，症状在别的行上）
- **第 31 条：向回走的判据要知道什么"包着"这个构造。**（上一轮：回走跨过了模板头，收上来的是 `<`）
- **第 18 条：跨层按形状写的判据要留痕。**（`w.size` 是 `IndexExpr`，不是 `MemberExpr`——
  谁改了读法必须同时改 `sema/resolve.rs`，否则成员访问**静默**全失效）
- **第 9 条："旁边有个同名判据"不等于能复用。** 先问它从哪里开始看。
- **改"公共入口"之前先列调用点**（第 5 条）；**第三次出现同一个判据就抽出来**（第 14 条）。

**另外几条不成文的**：

- **`Binding.range` 是 declarator，不是整条声明**（`Widget w;` 里只有 `w`）。要类型就从根走到声明处取 `DeclSpecifierSeq`。
- **一条"每个命中都查一次"的判据，要问它是不是每次都在算同一件事**。宏的找引用第一版每个命中调一次
  `macro_definition`，于是 4 079 个命中走了 4 079 遍同一张 include 图：**3.77 s**。改成"每个文件算一次宏环境、
  按偏移问它"是 **35 ms**，答案逐条相同（`index-design.md` 记着这次测量）。
- **两条队列的路径要能"升级"**。项目扫描先把所有文件排进 rest 半边，用户随后打开其中一个——不升级的话，
  用户正看着的文件排在"项目其余"后面，**而所有"从不打开任何文件"的测试都会是绿的**。
  找这条 bug 的测试必须是"先扫描、后打开"那个顺序（`session.rs` 有）。
- **`Session::pending() > 0` 时不要报"名字不存在"**。惰性索引下 `NotDeclaredHere` 同时意味着"这里没有"和
  "还没读到"，而查询层分不出这两件事（它也不该分——索引永远是全集的一个子集）。分界线在上层。
- **`FileIndexer` 只喂 `ParserConfig::default()`**：`-D`/`-std` 不参与解析。哪天要喂进去，
  **键里必须同时把宏环境加回来**并抬 `FORMAT_VERSION`（`cache.rs` 与"第三个被测试抓出来的键错误"都记着）。
- **缓存键里有 `reading_fingerprint`**（`build.rs` 算的源码哈希）：改了 parser 或语义层的源码，整库自动作废，
  **不要**手动抬 `FORMAT_VERSION`（它现在是给"源码里看不出来的变化"用的）。改字段格式才抬 `CODEC_VERSION`。

---

## 6. 本机环境（换机器要重做的那几件事）

- **工具链**：`C:\Users\xx\Desktop\mingw\mingw64\bin\g++.EXE`（gcc 15.1.0）。`discover` 会自己找到它，
  但 `crates/cpp_code_analysis/tests/toolchain.rs` 里那几个测试在没有编译器的机器上会走另一条分支（有测试钉住）。
- **普查的清单**：`%TEMP%\stdprobe\files.txt`（**128 个文件**，路径指向上面的 mingw）。
  换机器或换标准库版本，数字会变——**变化本身不重要，同一台机器上的趋势才重要**。
- **临时目录**：探针自己会在 `%TEMP%` 下建项目（`cppls-index-includes`、`cppls-returns` 之类），随时可删。
- **`%TEMP%` 下的缓存**：`SummaryStore` 的缓存写在项目里的 `.cppls/`；探针建的是临时项目，删掉即清。

---

## 7. 一句话的优先级

**条件求值两半都做完了**（第十五、十六轮，见 §3.5c）：摘要存**问题**（分支链、正文、嵌套、文件自己的守卫），
查询时用（编译器 `-dM` 预定义 + 编译数据库的 `-D`/`-U` + walk 按翻译顺序喂进来的闭包定义，含单整数字面量的值）
求值成 `Active`/`Inactive`/`Unknown`，而"我不知道"（`Lookup::Unanswered`）与"没定义"从此是**两个答案**——`#if defined(X)`
和 `#if X` 不再把前者读成 `0`。量到的：459 文件的闭包里 **486 个条件 include 里 188 个被判定**（127 取到、61 跳过、
298 未知，而且**一个否定结论都没用上**）；`#if 0` 后面的 include 不再跟进，`_GLIBCXX_USE_CXX11_ABI` 这类 feature 宏
也能算了。方向鉴定仍在 [`parser-assessment.md`](parser-assessment.md) §7。

**接着做的顺序**（§2.0 那次测量把顺序改了）：

1. **parser 语法长尾**（§2）：两个语料上 **79% / 63% 的失败文件首错行上一个宏都没有**——剩下的份额大多在语法，
   不在宏。队列仍是"按首错归类"：`template<typename _Tp>`、`__asm__ volatile ("tilerelease" ::)` 这些。
2. **让确定路径改写带疑的 `visited`**（§3.5c）：它挡住"输入完整吗"这一格，也挡着 `winnt.h` 家族的四个宏。
3. **有界的展开**：只对"形态 Unknown 的函数式宏当语句/声明片段"这一族（17% / 36% 的失败文件），而且**按位置**——
   parser 在具体调用点问"这次调用的展开是什么"，不是一张无位置的表（§2.0 已经把那条路量死了：+0 个文件变干净、
   −37 个变脏）。
4. **其余语义边界**：类类型下标、模板实参成员、`auto` 返回、`using Base::f;`、运算符重载——`index-design.md`
   末尾那张表就是队列；**`__has_include`**（47 处）接进求值器也在这一档。
5. **LSP 二进制**最后（客户端会通知我们，见 §3.6）。








