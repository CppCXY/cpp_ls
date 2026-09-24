# 路线图：做什么，以及每一步的思路

**这一份是队列**——做什么、为什么这么做、怎么知道做对了、哪里会踩坑。它叫 `roadmap.md` 而不是
"next-steps"：它是**活的队列**，每做完一条就在这里改一条，而不是一次性的交接件。
三份规格文档不要重复它们的内容：

| 文档 | 管什么 |
|---|---|
| [`index-design.md`](index-design.md) | 索引与语义层的**设计**：事实层、缓存键、查询清单、三条不变量。"现在答不了什么"那张表在它的末尾 |
| [`grammar-gaps.md`](grammar-gaps.md) | parser 的**读法登记处**：每条缺口的四要素、已修/待办、以及三十一条**维护约定**（大半的坑在那里） |
| [`std-library.md`](std-library.md) | **标准库这条线**：为什么它是最好的探针、P0–P4 的计划、每一次普查的数字 |

本文档只回答一件事：**下一步做什么，以及做的时候脑子里该有什么。**

---

## 0. 三分钟进入状态

**是什么**：`cpp_ls` 是一个 C++ 语言服务器的内核，两个 crate——
`cpp_parser`（无损 CST、容错、宏表、外部符号表接口）与 `cpp_code_analysis`（预处理、每文件事实、缓存、跨文件查询）。
**驱动层已经落地**（`session.rs`：开项目 → 发现工具链 → 惰性索引 → 接 `didOpen`/`didChange` → 查询），
**还没有语言服务器二进制**——也就是把 `Session` 接到 JSON-RPC 上的那一层，它是队列里的下一条。

**现在的数字**（最近一次普查，`%TEMP%\stdprobe\files.txt` 的 128 个文件——`<vector>/<string>/<map>/<algorithm>` 的闭包）：
`干净 80 / 报错 48`，错误消息总数 **932**；每文件错误数 `干净 80 | 只有一个 6 | 两到五个 13 | 超过五个 29`；
Rust 侧 `cargo test --workspace` = **979 个测试 / 34 个套件全绿**。

**门禁三条 + 一条**（改完必须全绿，`index-design.md` §门禁有同样的表）：

```bash
cargo test --workspace                     # 979 个测试，34 个套件
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

它打三段：**代价**（文件/行/字节/耗时）、**普查**（干净 vs 报错、消息直方图、每文件错误数直方图）、
**每个失败文件的第一个错**（带文件名与源码行——这是唯一能看见**成因**的视角，第一个错是上面什么都解释不了的那个）。
本机的那份清单在 `%TEMP%\stdprobe\files.txt`（**128 个文件**；换机器要重新生成）。

**看一个文件被读成了什么**（排查缺规则唯一有效的动作）：

```bash
cargo run -q -p cpp_parser --bin cpp_dump -- <file> --tree    # 有错也打树；不加 --tree 只在干净时打
```

**其它探针**：`examples/std_index.rs`（闭包的事实统计：`-- <清单> <缓存目录>`）、`examples/std_query.rs`
（**端到端**：真写一个 TU、发现工具链、索引闭包，然后按光标问成员——这一轮的 0/7 → 3/7 就是它）、
`examples/index_includes.rs`（冷/热索引代价）、`examples/open_project.rs`（**驱动层**：开项目 → 惰性索引 →
按光标问；冷/热两遍，以及"缓冲区就是文本"）、`examples/find_references.rs`（**找引用**：四级阶梯各自的代价、
词法 vs 解析、以及重命名会改几处）、`examples/measure.rs`（构建与命中）。

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

每条给：**形状 → 现在读成什么 → 思路**。"已确认"表示最小复现已验证，"待缩"表示只知道文件与行。

> 本文档里的 `<文件>:<行>` 是**编辑器行号**（从 1 起）。`std_probe` 打印的是从 0 起的行号，差一；
> 用 `cargo run -q -p cpp_parser --bin cpp_dump -- <文件> --tree` 看树时不必在意这个差。

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

形状断言六条（`gaps.rs`）：九段接缝、四段瓦砾、一个构造两个分支、模板实参是调用还是函数类型、一个声明符一个
初始化式、语句失败之后块还在。五条教训进了维护约定第 32–35 条，其中第 34、35 条最贵：**"就地放弃、token 留着"
的错误路径必须带上 `NodeEnd` 关节点**（这一条收了两轮学费，声明层与语句层各一次）；**`rollback` 只截断，
回不到"未来"**。

**还没做的**，按值排：

1. 各文件的**下一条**：`bits/move.h:233`（函数体里那个没有 `;` 的宏——现在只是**一条诊断**，块与类都不再丢）、
   `bits/alloc_traits.h:453`、`bits/iterator_concepts.h:908`、`bits/stl_pair.h:407`、`bits/basic_string.h:4531`、
   `bits/stl_vector.h:1865`——都等着归类与缩。
2. 队列里**还没碰**的：§2.2（GNU 类型拼写：`__typeof__` / `__int128`）、§2.5（模板参数表里的宏）。
3. 剩下 29 个"超过五个错"的文件——那些是级联，按第 11 条先归类再动手。

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
不是文件守卫），所以"这个名字在这里是不是宏"取决于分析没有的宏环境。但这件事**有一条可靠的定理**可用：

```text
#if 的每个分支都写了同一个名字、同一种 kind   ⇒  这个条件块之后，这个名字的宏状态与分支无关
特例（也是最常见的形状）：#ifndef NAME / #define NAME / #endif
        ——条件成立则这里定义，条件不成立则它已经被定义过了：两种情况之后它都是宏
```

所以正确的做法不是"猜"，而是**在事实层记下这个结论**（一个 `MacroFact` 的布尔字段：这条 `#define` 所在条件块
的每个分支都同意），再由**引用查询**（不是定义查询——"哪条 `#define` 在生效"仍然是有条件的）使用它。
那是一次 schema 改动（`CODEC_VERSION` 抬一格），也是让标准库上的重命名从"全是可能"变成"全是使用"的那一步。

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

**语义。** 驱动层（§3.6 前半）已经落地，`examples/open_project.rs` 上 `std_query` 那 9 条走"开项目 →
`did_open` → `advance` → 按光标问"也是 9/9——但那只是**入口**，没有加任何语义能力。这一轮加了一条：
**宏的找引用与重命名**（§3.5，四级阶梯 + 106 倍的那次修正）。**LSP 二进制暂缓**（客户端通知我们，不接
`notify`；二进制是协议活，不挡下面任何一条，理由写在 §3.6 末尾）。所以顺序是：

1. **宏这条线继续走到底**（用户的原始指示："真正的宏"）：
   * **条件块的分支结论**（§3.5 末尾）：`#ifndef NAME / #define NAME / #endif` 之后这个名字的宏状态与分支无关
     ——一条可靠的定理，记进 `MacroFact`（抬 `CODEC_VERSION`），引用查询据此把"可能"变成"使用"。
     这是让标准库上的重命名真正可用的那一步，**而且不需要宏环境**。
   * **`#if` 与宏**（§2.6 的 parser 那一格）与**宏展开进事实**（`BindingOrigin::MacroExpansion` 词汇已就位）——
     后者是 P4，最贵，且要先定 P3（宏环境进键）那个取舍；上面的定理能让 P3 变得不那么急。
2. **标准库与语义查询的边界**：类类型的下标（`v[0]` 要实例化）、模板实参的成员、`auto` 推导的返回、
   `using Base::f;` 与虚函数覆盖、运算符重载——每一条都是"能答的边界往外推一格"，`index-design.md` 末尾
   那张"现在答不了什么"的表就是队列。**普通名字（非宏）的找引用**也在这一格：它每个候选文件要**解析**
   （要作用域），实测约 3 ms/文件，同一个阶梯换个第 3 级就能做，先量再定。
3. **语义索引的规模**：名字/作用域 → 文件的倒排表（§3.3，一万文件时才疼）。**标识符位置表不用做了**——
   §3.5 量出来它比要存的事实还大（4.9 MB vs 5.4 MB），而查询只要 25–40 ms。
4. **parser 的长尾**（可以间隔着做）：各文件的"下一条"（§2.1 第 1 条）、§2.2 的 GNU 拼写、§2.6 的 `if` 与宏
   ——每一项 1–3 个文件，按首错归类再做（第 11 条）。它服务的仍然是第 2 条：`std::vector` 那三个类读得越全，
   能问的就越多。








