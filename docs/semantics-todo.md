# 语义正确性：已知问题与下一步（2026-10-24）

用户报的三个问题，按"诊断闪现 → 别名 → 定义查找"的顺序处理。这里记的是**还没解决的**，和为什么。

## ① 诊断闪现：已修（`c2c0357` 之后的 `sticky report` 规则）

**机制**：pull 的诊断在文件"已编辑但还没 cook"时返回 `None`，handler 把它变成**空报告**——
而空报告对客户端意味着"这个文件干净"，于是它把波浪线擦掉；pump 随后 cook 完，`workspace/diagnostic/refresh`
让客户端再问一次，波浪线又出现。**每次 keystroke 重复一次**，就是"闪现"。

修法：`AnalysisState::last_reports` 记住每个文件上一次真正给出的报告；**没有答案的 pull 重复上一次的报告**，
只有真正的答案（包括空答案）才替换它。规则抽成 `the_report()`，三个分支有单测
（`no_answer_repeats_the_last_report_instead_of_claiming_the_file_is_clean`）。

为什么这不是"错误地使用 push"：这个服务**只**有 pull（`grep publishDiagnostics` 只剩注释），
也没有乱发 refresh——问题在**答案本身在交替**，不在投递方式。

## ② `using` 别名与 `auto`：修了一半，量出来的结果

`CodeFormatCLib/src/CodeFormat.cpp`（libstdc++ 闭包）修前 **10 条 `undefined-field` 假报**，修后 **1 条**。

### 已修 a：`auto` 的每一种写法都不该参与"转换"判断

`auto& config = configs.emplace_back(...)` 被报 "does not convert to `auto&`"：检查只跳过了字面上的
`auto` / `decltype(auto)`，没跳过 `auto&`、`const auto&`、`auto&&`、`auto*`。
`assign_type_check::is_a_deduced_auto` 现在剥掉 `const`/`volatile`/`&`/`*` 再判断。
（**注意**：把用户那张图钉成 fixture 时发现，纯 fixture 里 `Widget& → auto&` 回答的是 `Unknown` 而不是 `No`，
所以这个假报的触发还需要真实类型；结论按代码读，按项目文件验证。）

### 已修 b：别名指向"没有人声明过的类"时，成员表是**缺口**，不是空表

`std::string` → `typedef basic_string<char> string` 在 libstdc++ 的 `inline namespace __cxx11` 里，
解析落到 `std::basic_string`（只有前向声明），`direct_members` 找不到任何成员 → 返回
**空但完整**的列表（`unlisted: []`）→ `undefined-field` 把 `result.size()`、`formattedText.begin()`
等全部报成"没有这个成员"。

现在两处：
- `resolve_aliases_or_unresolved` 第三个返回值 = "链的终点是没人声明的拼写"；
- 别名解析后的类**一个成员都没有**时，列表带上 `unlisted`（"答案到此为止"），`undefined-field` 因此闭嘴。

### 未修：`std::vector::erase` 不在事实里

剩下的那一条 `_configs has no member erase` 不是检查的问题，是**索引里没有这个声明**：

```text
[undefined-field] `std::vector` -> `erase` (303 member(s), 0 unlisted) — index: 121 posting(s) of `erase`,
                  278 in scope `std::vector`
[member] 第一次的成员表: _M_allocate, _M_erase, ... —— 没有裸 `erase`
```

`std::vector` 在 `bits/stl_vector.h` 里，`erase` 在 `#if __cplusplus >= 201103L` 的区域里。CDB 写的是
`-std=gnu++20`，`__cplusplus` 本应是 `202002L`（`cplusplus_value` 处理了 `gnu++` 前缀）。下一步：
用 `CPPLS_TRACE_MACRO=1` 打印 `stl_vector.h` 的宏环境，看 `__cplusplus` 有没有进去；如果没有，
追 `CompilerConfig::standard` 是否从 `-std=gnu++20` 解析出来了。**判据**：`members_of(std::vector)`
里出现裸 `erase`，且那一条假报消失。

## ③ 引用：**已按类过滤，头文件里的声明也在列表里**（用户图 2）

报告：`_diagnosticStyle` 的 Find References 给了 18 条、3 个文件，两个类的成员混在一起，而且**完全没有头文件**。

现状（`43f4162`、`8cb444f`）：

```text
1. 词法只能看到拼写，所以每个同名 identifier 都被报出来 —— 现在由"光标所在的类"决定说的是哪个声明：
   裸名字出现在**另一个类**的成员函数里 → 丢掉（无限定查找先找本类的成员，它不可能是这一个）；
   带限定（x. / p-> / Type::）→ 保留（接收者是什么类型是类型问题，漏报比多报更糟）；
   光标不在类里 → 全部保留。声明也按同一规则过滤，所以别的类的声明不会进来。
2. 头文件里的声明以前被丢掉，因为 VFS 只持有**这一轮读过**的文件，而索引是从磁盘缓存读的 ——
   `held()` 是 None，push 里就 return 了。现在从磁盘读一次并建行索引（`position_of`），并有测试。
3. "在类里"有两种形状，只有一种有作用域：类体内定义的成员函数有 Class scope；类外定义
   （`void CodeFormat::UpdateCodeStyle() { … }`，.cpp 里的常态）只有声明符里的文字。两种都处理了 ——
   实测这种定义解析成 `Declaration`（不是 `FunctionDef`），而且 `InitDeclarator` 只装 `()`，
   所以类名从声明文字到第一个 `(` 之间取最后一个标识符。
```

判据（测试 `a_member_of_one_class::is_not_another_classs_member_and_the_header_declaration_counts`）：
两个类各有一份 `_value`、各在 `First::update`/`Second::update` 里用一次 → 修前 4 条（两个声明 + 两个用法），
修后 **2 条**（`First` 在头文件里的声明 + `First::update` 的用法）。

### 带限定的用法：接收者的类型决定（本次补上）

`other._value` / `p->_value` 现在按**接收者的类型**判断，和裸名字是两条规则：

```text
裸名字            外层类是哪个类，就是哪个类的成员（无限定查找先找本类）
带限定            用 member_access_at 拿到 object，Session::type_at 求它的类型，比对 owner
类型求不出来        保留（漏报一个可见的用法，比隐藏一个真实用法更糟）
```

判据（`a_receiver_of_another_class::is_the_receivers_and_a_bare_name_is_the_enclosing_classs`）：
`First::update` 里既有自己的裸 `_value`，也有 `first._value`（first 是 First），而 `Second::update` 里还有
一个裸 `_value` → 从 `First::_value` 出发只应得到 **第 2、5 行**；关掉接收者规则是 `[1, 4, 5]`（把 Second 的也算进来）。
## ④ operator 推断：**已落地**，而且成本是零（2026-10-24）

`DeclFact::name` 现在对 operator 是真实的拼写（`operator[]`、`operator+`、`~Widget`），operator 的返回类型也进了
事实——`v[0]`、`a(3)`、`-a`、`a + a`、`*a` 全部有答案：

```text
v[0] → int      a(3) → int      -a → Money      a + a → Money      *a → int
tests/types.rs 里两个原先 #[ignore] 的测试现在都在跑
```

关键的一步不在 operator 本身。第一次实现时量到 `LuaSyntaxTree.cpp` 的诊断 **207 → 510 ms**，看上去是
"答案变多 → 检查变慢"，于是回滚了。后来按"数量还是单价"的顺序查下去（见
[`index-lookup.md`](index-lookup.md)）：问题数**完全一样**（452/247/256），真正的原因是 `n.FirstChild` 里 `n` 的
类型是模板参数 `_Tp`，被当成类名去查成员——而索引把每个头文件里的 `_Tp` 都存成了文件作用域的 `Type` 声明，
**2288 条**，一次查询 187 ms。修掉那个（模板参数不再进索引）之后：

```text
                          补丁 + 未修      补丁 + 本修复        当前 main
ImplicitBoolConversion    199 ms          14 ms                14 ms
UndefinedField            234 ms          80–110 ms            84–117 ms
诊断                      551 ms          199 ms               208 ms（同量级波动）
索引事实数                 —              —                    176 816 → 152 576
CodeFormat.cpp findings    1              1                    1（60 条假报没有回来）
```

### 现在的实现（三处，都有注释写明为什么）

```text
1. `declared_returns_of_with` 允许 OperatorFunction 读返回类型；`an_operator_return_is_usable` 决定事实能不能带它：
   builtin（int）与限定名（std::string）永远可以；无限定名只有在**类体没有声明同名别名**时才可以
   （`std::vector::operator[]` 的 `reference` 就是被这一条挡住的，见那 60 条假报）。
2. 引擎新增一元 arm（`-`、`+`、`~`、`!`），类走自己的 `operator-`，内建类型保留自身类型，`!` 是 `bool`。
3. `operator_returns_of` 的快路径（`member_fact`，索引 postings）现在**也做模板实参替换**——
   以前只有成员表兜底那条路替换，于是同一个问题两条路给出 `T&` 和 `int&` 两个答案。
```

### 这条经验怎么复用

```text
语义修好但变慢 → 先分"数量/单价"（CPPLS_TRACE_TIME 的 questions/computed/reused），
数量不变就去 [question] 里找那一条，再看它前后的 [lookup]/[type_of]，
最后用 CPPLS_PRINT_LOOKUP 问"这个查询到底拿到了什么"。
多数时候答案是：索引里存了本来不该存的东西。
```
### 为什么 `std::vector::erase` 不在事实里 —— 下一刀的具体位置（测过）

`erase` 在 libstdc++ 的 `bits/stl_vector.h:1793` 是这样写的：**返回类型和声明符之间夹着条件编译**。

```cpp
      iterator
#if __cplusplus >= 201103L
      erase(const_iterator __position)
```

已经排除的和已经确认的：

```text
__cplusplus 在索引的宏表里         CPPLS_PRINT_LANGUAGE=1 → body 202002L、value Some(202002)
同样的形状在 fixture 里能读出来      `int #if HAS_ERASE erase(int); #else erase(long); #endif` →
                                  members_of(V) 有一条，v.erase(1) 没有假报
```

所以不是"条件夹在声明符里"这个形状本身，而是 `stl_vector.h` 里那一处的具体读法（模板类体内、
`_GLIBCXX20_CONSTEXPR` 在上一行、命名空间由 `_GLIBCXX_BEGIN_NAMESPACE_CONTAINER` 展开）。

**下一步**：把 `stl_vector.h` 的 cooked rendering 在 `erase` 附近 dump 出来（或临时打印
`index.declarations_in("std::vector")` 的名字集合），看 `erase` 是根本没进 facts，还是进了但 scope
不是 `std::vector`。判据：`members_of(std::vector)` 里出现裸 `erase`，CodeFormat.cpp 最后一条假报消失。
## ⑥ 补全：`->` 走 operator-> 链，`.` 打在指针上补成 `->`（用户图 3）

报告两条："智能指针的补全没有考虑到指向的内容"，以及"通过 `.` 补全的时候也应该替换为 `->`"。

现状（`33eb601`）：

```text
`->` 以前列出的是**对象自己的类**：`Widget* p` 上是空的（指针不命名类），`std::unique_ptr<Widget> sp`
上是 `operator->`、`operator*`、`reset`…… —— 全是箭头到不了的成员，而 `Widget` 的成员一个都没有。
现在顺着类型走：指针 → 指向的类型；类 → 它自己的 `operator->` 返回类型 → 再取指针指向的类型；
最多四跳（`shared_ptr<T>` 两跳、迭代器两跳，实测链长没有超过二的）。

`.` 打在指针上时列表已经是指向类的（读者就是这个意思），但操作符没改，选中会写出 `p.size` —— 不编译。
现在**替换范围向左扩一个字节**（那个 `.`）、插入文本带 `->`；label 不变（客户端按它过滤），
LSP 层对"不能插值 snippet"的客户端用 `->` + label，而不是把 `$0` 写进文件。
```

判据（`tests/completion.rs`）：
`an_arrow_reaches_the_class_through_the_pointer_and_through_operator_arrow` —— 修前两种接收者都是 `[]`；
`a_dot_on_a_pointer_lists_the_pointee`；`a_dot_on_a_pointer_is_completed_as_an_arrow` —— 替换范围覆盖的
那一个字符就是 `.`，插入文本是 `->size`。
## ⑦ `textDocument/implementation`：头文件里的声明 → `.cpp` 的实现（本次）

报告："把头文件上的 go to implement 也做了"。这一条以前**不可能**回答，原因不在这个 handler，而在索引：

```text
void CodeFormat::UpdateCodeStyle(…) { … }
  → 解析成 DeclSpecifierSeq [ void, NamedType C::f ] + InitDeclarator ()
  → declared_name 看到限定名就 return None（这条拒绝本身是对的：不能把 count 绑进本文件作用域）
  → 于是没有 binding、没有事实 —— 整个项目的 .cpp 定义都不在索引里。
    实测：definitions("UpdateCodeStyle") 只回答 1 条 —— 头文件那条，而读者正打开着定义它的文件。
```

修法（`sema/declarations.rs::qualified_declarations`）：**第二个 pass**，从拼写里取 scope 建事实，不建 binding（
拒绝的理由因此不受影响）。判据与两个需要实测的形状：

```text
void C::f() { }      名字在 DeclSpecifierSeq 里，declarator 只有 ()   → 取 specifier 里最后一个限定名
std::string f();     declarator 自己有名字                             → 跳过（否则会把返回类型当名字）
"自己有没有名字"      是最外层 Declarator 的 NameExpr，不是它下面任意 NameExpr —— 参数也是声明
ns::f(1); 在函数体里   会被解析成一个变量声明（most vexing parse）        → 只处理文件作用域，否则多出一条假事实
```

然后 `implementation` handler 增加了 `definitions_of`：从光标的声明取出 `(名字, 光标所在类)`，扫全部 summary 找
**同一个限定名、且文本以 `}` 结尾**的函数 —— 那就是定义。ctor 声明以 `;` 结尾，用来区分。不能复用
`definitions`：它回答的是"从这里能看到什么"，而 `.cpp` 对头文件不可见（是 .cpp include 头文件，不是反过来）。

判据（`cpp_ls` 的 `a_declaration_in_a_header::answers_the_body_in_the_source_file`）：`base.h` 的 `run()`
→ 唯一答案在 `base.cpp`。返回顺序是"定义先、override 后"，因为光标在声明上时读者要的是代码。

实测：真实项目事实 152 576 → 153 317（+741 个定义），诊断不变（214/187 ms，findings 不变）。
`tests/semantic.rs::an_unresolved_include_does_not_erase_the_files_own_facts` 第一次变绿 —— 它就是这条的哨兵。