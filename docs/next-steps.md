# 接手：做什么，以及每一步的思路

给下一个动这份代码的人（或 AI）。**这一份是队列**——做什么、为什么这么做、怎么知道做对了、哪里会踩坑。
三份已有的文档是**规格**，不要重复它们：

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
没有语言服务器二进制，没有 driver；**查询是产品，驱动层还没写**（这是队列里的一条）。

**现在的数字**（最近一次普查，185 个文件的标准库闭包）：
`干净 101 / 报错 81`，错误消息总数约 2252；Rust 侧 `cargo test --workspace` = **926 个测试 / 34 个套件全绿**。

**门禁三条 + 一条**（改完必须全绿，`index-design.md` §门禁有同样的表）：

```bash
cargo test --workspace                     # 926 个测试，34 个套件
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
本机的那份清单在 `%TEMP%\stdprobe\files.txt`（185 个文件；换机器要重新生成）。

**看一个文件被读成了什么**（排查缺规则唯一有效的动作）：

```bash
cargo run -q -p cpp_parser --bin cpp_dump -- <file> --tree    # 有错也打树；不加 --tree 只在干净时打
```

**其它探针**：`examples/std_index.rs`（闭包的事实统计）、`examples/index_includes.rs`（冷/热索引代价）、`examples/measure.rs`（构建与命中）。

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

### 2.1 指令落在构造的接缝上（4+ 个文件，**最值钱的一条**）

B23/B24/B40 已经接过几个接缝（初始化列表元素之间、字符串字面量串中间、`try`/`catch` 的每个关节），
**下面这四个还没接**，每个都是"parser 要一个特定 token，来的却是 `#`"：

```cpp
// bits/move.h:221    说明符与返回类型之间
template<typename _Tp> _GLIBCXX20_CONSTEXPR inline
#if __cplusplus >= 201103L
  typename enable_if<...>::type
#endif
f();

// bits/utility.h:176  别名模板的名字与 `=` 之间
template<typename _Tp, _Tp _Num> using make_integer_sequence
#if __has_builtin(__make_integer_seq)
  = __make_integer_seq<integer_sequence, _Tp, _Num>;
#endif

// bits/alloc_traits.h:48  类头与基类子句之间
template<typename _Alloc, typename = typename _Alloc::value_type> struct __alloc_traits
#if __cplusplus >= 201103L
  : std::allocator_traits<_Alloc>
#endif
{ };

// include/c++/bit:94   requires-clause 与函数体之间
template<typename _To, typename _From> constexpr _To bit_cast(const _From& __from)
  requires (sizeof(_To) == sizeof(_From)) && is_trivially_copyable_v<_To>
#endif
{ return __builtin_bit_cast(_To, __from); }
```

**思路**：这是**同一个模式**的第五、六、七、八处，而现成的机制就在手边：
`stats.rs::eat_preprocessor_directives`（`parse_try_statement` 用的那个），它在光标处把 `#` 开头的行读成**指令节点**再继续。
要点两条：**只在接缝上接**（"a `#` anywhere it cannot be a directive is still an error"），
以及**接完要再问一次**"现在这个 token 对了没有"——不要在解析中途盲目跳过指令。
还有一个同族的：`bits/concepts`（`concept` 的名字与 `=` 之间，`iterator_concepts.h:617` 的 `#if __SIZEOF_INT128__`）。

**思路的延伸**：与其一处处打补丁，不如把"接缝"这个概念写下来——
**一条规则在等一个特定 token 时，`#` 是它必须接受的前缀**。可以做成一个小助手
`expect_token_allowing_directives`，用它替换现在手写的 `eat_preprocessor_directives` + `expect_token` 组合。
但要**先列出所有调用点**（维护约定第 5 条：改公共入口前先列"谁在拼这串 token"）。

### 2.2 GNU 的类型拼写（4 个文件，已确认）

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

### 2.3 小写函数式宏独占一行（4–5 个文件，待缩）

```cpp
__glibcxx_function_requires(_Mutable_ForwardIteratorConcept<_Iter>)   // stl_algobase.h:161
_GLIBCXX17_CONSTEXPR reverse_iterator                                  // stl_iterator.h:302
```

**现在**：前者的形状规则要求"名字（可带括号组）之后**能开始一个声明**"，而它后面跟着的是**另一个语句**；
后者是宏站在**返回类型**的位置（`expected ; after expression`）。

**思路**：这两条是同一件事的两面——**宏在声明/语句的最前面**。
- 第 2.3 条的后一半（`_GLIBCXX17_CONSTEXPR reverse_iterator`）可以靠"说明符序列接受形状像宏的名字"解决，
  但注意与"`Widget w;`"的边界（名字 + 名字 = 声明，两个名字都是类型时才会误判）；
- 前一半要放宽"后面能开始什么"，**只在文件/命名空间作用域**（函数体里 `COUNT` 后面跟 `return` 是漏了分号，
  那条拒绝是有意的，见 `at_a_macro_that_stands_for_a_declaration` 的文档）。
放宽之后**必须**给反例：`x = 1;`（赋值）、`FOO(x);`（most vexing parse）、`TEST(A,B){ }`（定义）。

### 2.4 `requires` 与它周围的构造（3–4 个文件，已确认）

```cpp
// bits/stl_pair.h:367   requires-clause 与构造函数的初始化列表之间
template<typename _U1, typename _U2> constexpr pair(...)
  requires is_default_constructible_v<_T1> && is_default_constructible_v<_T2>
  : first(), second() { }
```

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

### 3.1 跟着 typedef / 别名走一步（**最大的一块**，设计已想清）

**现象**：`s.substr(1).size` 报 `NotDeclaredHere("std::string::substr")`——`std::string` 是
`typedef basic_string<char> string;`，而成员查找是**按名字找类**，不跟别名走。同一条边界也挡住 `std::vector`、
`std::string_view`、所有 `*_type` 别名。

**思路**：别名**本身就是一条声明**，所以它该记录自己指向什么拼写：
- `DeclKind::Type` 的事实目前 `type_of`/`returns` 都是 `None`；让 `typedef`/`using` 的那条事实把**目标拼写**记进 `type_of`
  （`using string = basic_string<char>` → `"basic_string<char>"`）。这是 `CODEC_VERSION` 变更，
  而 `type_of` 的文档要改一句话：对变量是"它的类型"，对别名是"它指向的类型"。
  **注意函数指针那类别名**：`typedef void (*F)(int);` 的类型是"说明符 + 声明符"，
  而 `declared_type_of` 只取说明符序列（会给 `void`）——别名这条要把它自己的声明符也算进去。
- 查询侧：`base_type_name` 之后加**一步**解析——名字在作用域/索引里是一条别名事实，就再查一次；
  **要有深度上限**（`using A = A;`、互相引用的两条别名），并把这个事实写进文档。
- 好处立刻可见：`std::string` 的整个表面（`substr`、`size`、`find`……）都在 `basic_string` 的摘要里，已经索引好了。

**要量**：闭包里有多少 `DeclKind::Type` 的**别名**事实（`typedef`/`using`），以及跟一步能救回多少查询。

### 3.2 解引用与下标：`(*p).size`、`arr[i].size`（`type_of_expression` 的第五、六格）

**思路**：这两条和刚做完的"调用"那一格同形——**先算出对象的类型，再去成员查找**，而算法是对**拼写**做算术：
- `*p`：对象的类型是 `Widget*` / `Widget&` → 去掉尾部的 `*`/`&`（`base_type_name` 已经会做的反向操作）；
- `arr[i]`：类型是 `Widget[4]` / `std::vector<Widget>` → 前者去掉 `[N]`，后者要看模板实参（**实例化**，
  属于更后面那一格，先答 `Unknown` 更诚实）。
- `a.b.c` 的递归已经在了，所以这两格插进 `type_of_expression` 的形状分派即可。
- 反面断言要跟上：`int*` 解引用得 `int`（不是"去掉星号后剩下的名字"），`Widget*` 才是 `Widget`。

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

### 3.5 宏的"找引用/重命名"（贵，且要先解决一个结构问题）

摘要里**没有标识符位置**，所以"哪些位置的名字解析到这条事实"要么逐文件解析（先用文本子串筛一遍），
要么在事实里加一份 token 位置表。**先量**：一个真实项目里"找引用"要扫多少文件、筛完还剩多少要解析。

### 3.6 "打开一个项目"的入口（产品上最短的一块）

今天 `discover`/`index_includes_from`/`Worklist` 都能跑，但**没有把它们串起来的那个函数**，
也没有语言服务器二进制。这一条不需要新语义，只需要一个 driver：
打开项目 → 发现工具链 → 惰性索引 → 接 `didOpen`/`didChange` → 把查询接到 LSP 的响应上。
**它是"能不能被用上"的分界线**，而且是纯工程活。

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

**另外三条不成文的**：

- **`Binding.range` 是 declarator，不是整条声明**（`Widget w;` 里只有 `w`）。要类型就从根走到声明处取 `DeclSpecifierSeq`。
- **`FileIndexer` 只喂 `ParserConfig::default()`**：`-D`/`-std` 不参与解析。哪天要喂进去，
  **键里必须同时把宏环境加回来**并抬 `FORMAT_VERSION`（`cache.rs` 与"第三个被测试抓出来的键错误"都记着）。
- **缓存键里有 `reading_fingerprint`**（`build.rs` 算的源码哈希）：改了 parser 或语义层的源码，整库自动作废，
  **不要**手动抬 `FORMAT_VERSION`（它现在是给"源码里看不出来的变化"用的）。改字段格式才抬 `CODEC_VERSION`。

---

## 6. 本机环境（换机器要重做的那几件事）

- **工具链**：`C:\Users\xx\Desktop\mingw\mingw64\bin\g++.EXE`（gcc 15.1.0）。`discover` 会自己找到它，
  但 `crates/cpp_code_analysis/tests/toolchain.rs` 里那几个测试在没有编译器的机器上会走另一条分支（有测试钉住）。
- **普查的清单**：`%TEMP%\stdprobe\files.txt`（185 个文件，路径指向上面的 mingw）。
  换机器或换标准库版本，数字会变——**变化本身不重要，同一台机器上的趋势才重要**。
- **临时目录**：探针自己会在 `%TEMP%` 下建项目（`cppls-index-includes`、`cppls-returns` 之类），随时可删。
- **`%TEMP%` 下的缓存**：`SummaryStore` 的缓存写在项目里的 `.cppls/`；探针建的是临时项目，删掉即清。

---

## 7. 一句话的优先级

**parser**：先做 2.1（四个接缝，一个模式）与 2.2（GNU 类型拼写），这两个合起来值 8 个文件左右；
再按队列往下。
**语义**：先做 3.1（跟着 typedef 走一步）——它是"标准库能不能被真正用上"的那一步；
然后 3.2（解引用/下标），再考虑 3.6（driver，产品上最短的一块）。
