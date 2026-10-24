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

## ③ 定义/引用仍然"按名字"查找（用户图 1）

`_diagnosticStyle` 在 `LuaCodeFormat::UpdateDiagnosticStyle` 里，Go to Definition 列出 **3 个**：
本类的、`FormatState.h` 里另一个类的、`luacodeformat.h` 里第三个类的。也就是查询**是**按名字扫可见集合的，
没有用"光标所在的类"。

要做的事：

```text
1. `definition` 的入口先看光标处是不是**成员**（在某个类的成员函数体里、名字是该类的成员）：
   是 → 只在该类的作用域里找（`definitions_in_scope` 已有这个规则的一半），不要返回别人的同名成员。
2. 引用（references）同一个规则：一个类的成员 `_diagnosticStyle` 与另一个类的同名成员是两个实体。
3. 现有 `definitions_in_scope` 的注释说"裸名字不进类"，但这里光标处是**类内**，规则相反：
   类内的裸名字**先**进本类。缺的正是这一步。
```

判据：`LuaParser`/`CodeFormatCLib` 里给 `_diagnosticStyle`、`_session`、`_configs` 这类成员做
Go to Definition，每条只给一个答案（本类声明处）；`references` 不再跨类。
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